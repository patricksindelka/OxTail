//! Per-tab highlighting: the active rules, their compiled form, and a cache of
//! prepared lines (ANSI-stripped text plus rule highlights).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use oxtail_columns::{Parser, Record};
use oxtail_config::{Profile, ProfileSet};
use oxtail_core::Line;
use oxtail_highlight::{
    ColorRef, CompiledRules, HighlightError, LineHighlight, Rule, StyledSpan, ansi, presets,
};
use oxtail_search::LinePredicate;

use crate::filter::hide_predicate;
use crate::rules::rules_from_profile;

/// Most prepared lines kept.
const CACHE_LIMIT: usize = 20_000;
/// Most minimap ticks kept.
const TICK_LIMIT: usize = 200_000;

/// A line ready to be laid out.
#[derive(Debug, Clone)]
pub struct Prepared {
    /// The text to draw: the line with ANSI escapes removed (when ANSI
    /// rendering is on).
    pub text: String,
    /// Colours from ANSI escapes, over `text`.
    pub ansi: Vec<StyledSpan>,
    /// What the rules found in `text`.
    pub hl: LineHighlight,
    /// The columns of `text`, when the tab has a parser and the line is a
    /// record (`None` for continuation lines and plain text).
    pub record: Option<Record<'static>>,
}

/// Compiles `rules`, dropping (with a warning) any rule that does not
/// compile. Returns the compiled set and the rules that made it.
pub fn compile_lenient(rules: &[Rule]) -> (CompiledRules, Vec<Rule>, Vec<String>) {
    let mut usable: Vec<Rule> = rules.to_vec();
    let mut warnings = Vec::new();
    // Every failure removes at least one rule, and an empty set always
    // compiles, so this loop ends after at most `rules.len() + 1` rounds.
    loop {
        match CompiledRules::compile(&usable) {
            Ok(c) => return (c, usable, warnings),
            Err(e) => {
                let idx = match &e {
                    HighlightError::EmptyLiteral { rule, .. }
                    | HighlightError::InvalidRegex { rule, .. }
                    | HighlightError::BadGroup { rule, .. }
                    | HighlightError::BadNumber { rule, .. } => Some(*rule),
                    HighlightError::Build(_) => None,
                };
                warnings.push(e.to_string());
                match idx {
                    Some(i) if i < usable.len() => {
                        usable.remove(i);
                    }
                    _ => usable.clear(),
                }
            }
        }
    }
}

/// The rules of a tab and the caches that depend on them.
pub struct HighlightState {
    /// Name of the profile the rules came from, if any.
    pub profile: Option<String>,
    /// The usable rules.
    pub rules: Arc<Vec<Rule>>,
    compiled: Arc<CompiledRules>,
    /// Problems met while compiling.
    pub warnings: Vec<String>,
    /// Bumped whenever the rules change (invalidates painted lines).
    pub epoch: u64,
    /// Render ANSI escapes as colours (otherwise show them raw).
    pub show_ansi: bool,
    /// The predicate for lines that hide rules fold away.
    pub hide: Option<Arc<dyn LinePredicate>>,
    /// The tab's column parser: column rules see the parsed byte ranges.
    parser: Option<Arc<Parser>>,
    cache: HashMap<u64, Arc<Prepared>>,
    /// Minimap ticks of lines seen so far, by line start offset.
    pub ticks: BTreeMap<u64, ColorRef>,
}

impl HighlightState {
    /// A state with the built-in default rules.
    pub fn with_defaults() -> Self {
        let mut s = Self::empty();
        s.set_rules(presets::default_rules(), None);
        s
    }

    fn empty() -> Self {
        let (compiled, _, _) = compile_lenient(&[]);
        Self {
            profile: None,
            rules: Arc::new(Vec::new()),
            compiled: Arc::new(compiled),
            warnings: Vec::new(),
            epoch: 0,
            show_ansi: true,
            hide: None,
            parser: None,
            cache: HashMap::new(),
            ticks: BTreeMap::new(),
        }
    }

    /// Uses `parser` to give column rules their byte ranges (and to attach
    /// the parsed record to prepared lines). `None` for plain text.
    pub fn set_parser(&mut self, parser: Option<Arc<Parser>>) {
        self.parser = parser;
        self.clear_cache();
        self.epoch += 1;
    }

    /// The tab's column parser.
    pub fn parser(&self) -> Option<&Arc<Parser>> {
        self.parser.as_ref()
    }

    /// Replaces the rules.
    pub fn set_rules(&mut self, rules: Vec<Rule>, profile: Option<String>) {
        let (compiled, usable, warnings) = compile_lenient(&rules);
        self.hide = hide_predicate(&usable);
        self.compiled = Arc::new(compiled);
        self.rules = Arc::new(usable);
        self.warnings = warnings;
        self.profile = profile;
        self.clear_cache();
        self.epoch += 1;
    }

    /// Uses the rules of `profile` (falling back to the default rules when it
    /// has none).
    pub fn set_profile(&mut self, profile: Option<&Profile>) {
        match profile {
            Some(p) => {
                let adapted = rules_from_profile(p);
                let rules = if adapted.rules.is_empty() {
                    presets::default_rules()
                } else {
                    adapted.rules
                };
                self.set_rules(rules, Some(p.name.clone()));
                self.warnings.extend(adapted.warnings);
            }
            None => self.set_rules(presets::default_rules(), None),
        }
    }

    /// Toggles ANSI rendering.
    pub fn set_show_ansi(&mut self, on: bool) {
        if self.show_ansi != on {
            self.show_ansi = on;
            self.clear_cache();
            self.epoch += 1;
        }
    }

    /// Forgets prepared lines and minimap ticks.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.ticks.clear();
    }

    /// The compiled rules (shared with the alert worker).
    pub fn compiled(&self) -> Arc<CompiledRules> {
        Arc::clone(&self.compiled)
    }

    /// Whether any rule asks for alerts.
    pub fn has_alert_rules(&self) -> bool {
        self.rules.iter().any(|r| r.enabled && r.actions.alert)
    }

    /// Whether any rule asks for automatic bookmarks.
    pub fn has_bookmark_rules(&self) -> bool {
        self.rules.iter().any(|r| r.enabled && r.actions.bookmark)
    }

    /// Prepares `line` for drawing (cached by start offset).
    pub fn prepare(&mut self, line: &Line) -> Arc<Prepared> {
        if let Some(p) = self.cache.get(&line.offset) {
            return Arc::clone(p);
        }
        let prepared = Arc::new(self.compute(line));
        if let Some(m) = prepared.hl.minimap
            && self.ticks.len() < TICK_LIMIT
        {
            self.ticks.insert(line.offset, m);
        }
        if self.cache.len() >= CACHE_LIMIT {
            self.cache.clear();
        }
        self.cache.insert(line.offset, Arc::clone(&prepared));
        prepared
    }

    fn compute(&self, line: &Line) -> Prepared {
        let (text, ansi) = if self.show_ansi && line.text.contains('\u{1b}') {
            ansi::parse(&line.text)
        } else {
            (line.text.clone(), Vec::new())
        };
        let record = self
            .parser
            .as_ref()
            .and_then(|p| crate::structure::parse_owned(p, &text));
        let hl = match (&self.parser, &record) {
            (Some(p), Some(rec)) => {
                let cols: Vec<(&str, std::ops::Range<usize>)> = p
                    .schema()
                    .columns
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| rec.span(i).map(|r| (c.name.as_str(), r)))
                    .collect();
                self.compiled.highlight(&text, Some(&cols))
            }
            _ => self.compiled.highlight(&text, None),
        };
        Prepared {
            text,
            ansi,
            hl,
            record,
        }
    }

    /// Highlights arbitrary text without caching (used by the rule editor's
    /// preview with its own rules).
    pub fn highlight_with(rules: &[Rule], text: &str) -> LineHighlight {
        let (compiled, _, _) = compile_lenient(rules);
        compiled.highlight(text, None)
    }
}

/// Picks the profile for a file: `ProfileSet::select` on the path and the
/// first lines, or the profile named `forced`, or the generic fallback.
pub fn choose_profile<'a>(
    set: &'a ProfileSet,
    forced: Option<&str>,
    path: &std::path::Path,
    first_lines: &[&str],
) -> Option<&'a Profile> {
    if let Some(name) = forced
        && let Some(p) = set.by_name(name)
    {
        return Some(p);
    }
    set.select(path, first_lines)
        .or_else(|| set.by_name("Generic"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(offset: u64, text: &str) -> Line {
        Line {
            number: 0,
            number_exact: true,
            offset,
            len: text.len() as u64 + 1,
            text: text.into(),
            truncated: false,
        }
    }

    #[test]
    fn bad_rules_are_dropped_with_a_warning() {
        let rules = vec![
            Rule::literal("ok", "error"),
            Rule::regex("bad", "(unclosed"),
            Rule::literal("empty", ""),
            Rule::literal("ok2", "warn"),
        ];
        let (compiled, usable, warnings) = compile_lenient(&rules);
        assert_eq!(usable.len(), 2);
        assert_eq!(warnings.len(), 2);
        assert!(!compiled.is_empty());
    }

    #[test]
    fn defaults_highlight_log_levels() {
        let mut h = HighlightState::with_defaults();
        let p = h.prepare(&line(0, "2026-01-01 ERROR something failed"));
        assert!(!p.hl.spans.is_empty());
        assert_eq!(p.text, "2026-01-01 ERROR something failed");
    }

    #[test]
    fn column_rules_only_match_once_the_tab_has_a_parser() {
        use oxtail_columns::ParserSpec;
        use oxtail_highlight::{ColumnOp, RuleMatcher, Scope, SemanticColor, Style};
        let rules = vec![
            Rule::new(
                "server error",
                RuleMatcher::Column {
                    column: "status".into(),
                    op: ColumnOp::Ge,
                    value: "500".into(),
                },
            )
            .scoped(Scope::Column("status".into()))
            .styled(Style::fg(ColorRef::solid(SemanticColor::Error))),
        ];
        let mut h = HighlightState::with_defaults();
        h.set_rules(rules, None);
        let text =
            "127.0.0.1 - - [10/Oct/2000:13:55:36 -0700] \"GET /a HTTP/1.0\" 503 12 \"-\" \"curl\"";
        let plain = h.prepare(&line(0, text));
        assert!(plain.hl.spans.is_empty() && plain.record.is_none());
        let epoch = h.epoch;
        h.set_parser(Some(Arc::new(
            ParserSpec::AccessCombined.compile().unwrap(),
        )));
        assert!(h.epoch > epoch, "the painted lines must be invalidated");
        let p = h.prepare(&line(0, text));
        assert_eq!(p.hl.spans.len(), 1);
        let r = &p.hl.spans[0].range;
        assert_eq!(&p.text[r.clone()], "503");
        assert!(p.record.is_some());
        // A continuation line has no record and no column highlight.
        let cont = h.prepare(&line(200, "    at com.example.Foo.bar(Foo.java:42)"));
        assert!(cont.record.is_none() && cont.hl.spans.is_empty());
        h.set_parser(None);
        assert!(h.prepare(&line(0, text)).record.is_none());
    }

    #[test]
    fn ansi_is_stripped_or_kept() {
        let mut h = HighlightState::with_defaults();
        let raw = "\u{1b}[31mred\u{1b}[0m text";
        let p = h.prepare(&line(0, raw));
        assert_eq!(p.text, "red text");
        assert!(!p.ansi.is_empty());
        h.set_show_ansi(false);
        let p = h.prepare(&line(0, raw));
        assert_eq!(p.text, raw);
        assert!(p.ansi.is_empty());
    }

    #[test]
    fn cache_is_per_offset_and_cleared_on_rule_change() {
        let mut h = HighlightState::with_defaults();
        let a = h.prepare(&line(10, "ERROR x"));
        let b = h.prepare(&line(10, "different text same offset"));
        assert!(Arc::ptr_eq(&a, &b));
        let epoch = h.epoch;
        h.set_rules(vec![Rule::literal("x", "x")], Some("mine".into()));
        assert!(h.epoch > epoch);
        let c = h.prepare(&line(10, "ERROR x"));
        assert!(!Arc::ptr_eq(&a, &c));
        assert_eq!(h.profile.as_deref(), Some("mine"));
    }

    #[test]
    fn minimap_ticks_are_collected_for_rendered_lines() {
        let mut h = HighlightState::with_defaults();
        h.prepare(&line(0, "all quiet"));
        h.prepare(&line(50, "panicked at src/main.rs:3"));
        assert_eq!(h.ticks.len(), 1);
        assert!(h.ticks.contains_key(&50));
        h.clear_cache();
        assert!(h.ticks.is_empty());
    }

    #[test]
    fn alert_and_hide_rules_are_detected() {
        let mut r = Rule::literal("h", "health");
        r.actions.hide = true;
        let mut a = Rule::literal("a", "boom");
        a.actions.alert = true;
        a.actions.bookmark = true;
        let mut h = HighlightState::with_defaults();
        assert!(!h.has_alert_rules() && h.hide.is_none());
        h.set_rules(vec![r, a], None);
        assert!(h.has_alert_rules() && h.has_bookmark_rules());
        assert!(h.hide.is_some());
    }

    #[test]
    fn profile_selection_prefers_forced_then_detection_then_generic() {
        let set = ProfileSet::builtin();
        let path = std::path::Path::new("/var/log/nginx/access.log");
        let forced = choose_profile(&set, Some("Syslog"), path, &[]).unwrap();
        assert_eq!(forced.name, "Syslog");
        let nginx = "127.0.0.1 - - [10/Oct/2000:13:55:36 -0700] \"GET /a HTTP/1.0\" 200 2326 \"-\" \"curl\"";
        let detected = choose_profile(&set, None, path, &[nginx]).unwrap();
        assert!(
            detected.name.to_lowercase().contains("nginx"),
            "{}",
            detected.name
        );
        let other = choose_profile(&set, None, std::path::Path::new("/tmp/x.txt"), &["hello"]);
        assert_eq!(other.unwrap().name, "Generic");
        // An unknown forced name falls back to detection.
        let fallback = choose_profile(&set, Some("nope"), std::path::Path::new("/tmp/x.txt"), &[]);
        assert_eq!(fallback.unwrap().name, "Generic");
    }

    #[test]
    fn set_profile_uses_the_adapter_and_falls_back() {
        let set = ProfileSet::builtin();
        let mut h = HighlightState::with_defaults();
        h.set_profile(set.by_name("Java / log4j"));
        assert_eq!(h.profile.as_deref(), Some("Java / log4j"));
        assert!(h.warnings.is_empty(), "{:?}", h.warnings);
        assert!(h.rules.iter().any(|r| r.name == "ERROR"));
        h.set_profile(None);
        assert!(h.profile.is_none());
        assert!(h.rules.len() > 5);
        // A profile without rules gets the default rules.
        let empty = Profile {
            name: "empty".into(),
            ..Profile::default()
        };
        h.set_profile(Some(&empty));
        assert!(h.rules.len() > 5);
    }
}
