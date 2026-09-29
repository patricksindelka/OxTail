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
        assert!(h.rules.iter().any(|r| r.name == "ERROR (text)"));
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

    // ---- severity colouring across the built-in profiles

    use oxtail_columns::ParserSpec;
    use oxtail_highlight::SemanticColor as Sem;

    /// The text and semantic foreground of every span with a foreground.
    fn fg_spans(p: &Prepared) -> Vec<(String, Sem)> {
        p.hl.spans
            .iter()
            .filter_map(|s| match s.style.fg? {
                ColorRef::Semantic { color, .. } => {
                    Some((p.text[s.range.clone()].to_string(), color))
                }
                ColorRef::Rgb(_) => None,
            })
            .collect()
    }

    fn state_for(profile: &str, parser: Option<ParserSpec>) -> HighlightState {
        let set = ProfileSet::builtin();
        let mut h = HighlightState::with_defaults();
        h.set_profile(Some(set.by_name(profile).expect("profile")));
        assert!(h.warnings.is_empty(), "{:?}", h.warnings);
        if let Some(spec) = parser {
            h.set_parser(Some(Arc::new(spec.compile().expect("parser"))));
        }
        h
    }

    fn log4j_parser() -> ParserSpec {
        let set = ProfileSet::builtin();
        let cols = set.by_name("Java / log4j").unwrap().columns.as_ref();
        let pattern = cols
            .and_then(|c| c.get("pattern"))
            .and_then(toml::Value::as_str)
            .expect("pattern")
            .to_string();
        ParserSpec::Regex {
            pattern,
            kinds: Default::default(),
        }
    }

    const LEVELS: [(&str, Sem); 6] = [
        ("TRACE", Sem::Trace),
        ("DEBUG", Sem::Debug),
        ("INFO", Sem::Info),
        ("WARN", Sem::Warn),
        ("ERROR", Sem::Error),
        ("FATAL", Sem::Error),
    ];

    #[test]
    fn generic_profile_colours_levels_and_mutes_timestamps() {
        let mut h = state_for("Generic", None);
        for (i, (lvl, want)) in LEVELS.iter().enumerate() {
            let text = format!("2026-09-22 14:42:31.962 [main] {lvl:<5} app.Db - reading");
            let p = h.prepare(&line(i as u64 * 100, &text));
            let spans = fg_spans(&p);
            assert!(
                spans.contains(&((*lvl).to_string(), *want)),
                "{lvl}: {spans:?}"
            );
            assert!(
                spans.contains(&("2026-09-22 14:42:31.962".to_string(), Sem::Muted)),
                "{lvl}: {spans:?}"
            );
            let row = p.hl.line_style.is_some_and(|s| s.bg.is_some());
            assert_eq!(row, matches!(*lvl, "ERROR" | "FATAL"), "{lvl}");
        }
    }

    #[test]
    fn log4j_colours_levels_in_text_with_and_without_columns() {
        for parser in [None, Some(log4j_parser())] {
            let mut h = state_for("Java / log4j", parser);
            for (i, (lvl, want)) in LEVELS.iter().enumerate() {
                let text = format!(
                    "2026-09-22 14:42:31.962 [main] {lvl:<5} app.Db - {lvl} in the message"
                );
                let p = h.prepare(&line(i as u64 * 100, &text));
                let spans = fg_spans(&p);
                assert!(
                    spans.contains(&((*lvl).to_string(), *want)),
                    "{lvl}: {spans:?}"
                );
                // Only the level itself is coloured, not the same word later.
                assert_eq!(
                    spans.iter().filter(|(t, c)| t == lvl && c == want).count(),
                    1,
                    "{lvl}: {spans:?}"
                );
                let ts = "2026-09-22 14:42:31.962".to_string();
                assert!(spans.contains(&(ts, Sem::Muted)), "{lvl}: {spans:?}");
                let row = p.hl.line_style.is_some_and(|s| s.bg.is_some());
                assert_eq!(row, matches!(*lvl, "ERROR" | "FATAL"), "{lvl}");
            }
        }
    }

    #[test]
    fn log4j_colours_spring_tomcat_and_python_layouts() {
        let mut h = state_for("Java / log4j", None);
        let cases = [
            (
                "2026-09-29T10:01:02.123+02:00 ERROR 1 --- [main] app.Db : failed",
                "ERROR",
                Sem::Error,
                true,
            ),
            (
                "29-Sep-2026 10:01:02.123 WARNING [main] org.apache.Catalina slow",
                "WARNING",
                Sem::Warn,
                false,
            ),
            (
                "2026-09-29 10:01:02,123 - app.db - ERROR - connection lost",
                "ERROR",
                Sem::Error,
                true,
            ),
            (
                "29-Sep-2026 10:01:02.123 SEVERE [main] org.apache.Catalina boom",
                "SEVERE",
                Sem::Error,
                true,
            ),
        ];
        for (i, (text, word, want, row)) in cases.iter().enumerate() {
            let p = h.prepare(&line(i as u64 * 100, text));
            let spans = fg_spans(&p);
            assert!(
                spans.contains(&((*word).to_string(), *want)),
                "{text}: {spans:?}"
            );
            let has_row = p.hl.line_style.is_some_and(|s| s.bg.is_some());
            assert_eq!(has_row, *row, "{text}");
        }
        // An INFO line that mentions ERROR later is not reddened.
        let p = h.prepare(&line(
            900,
            "2026-09-29 10:01:02.123 [main] INFO  a.B - ERROR later",
        ));
        assert!(p.hl.line_style.is_none_or(|s| s.bg.is_none()));
        assert!(!fg_spans(&p).contains(&("ERROR".to_string(), Sem::Error)));
    }

    #[test]
    fn logfmt_and_json_colour_the_level_value_and_mute_keys() {
        let logfmt = ParserSpec::Logfmt {
            columns: vec!["ts".into(), "level".into(), "msg".into()],
            kinds: Default::default(),
        };
        let json = ParserSpec::JsonLines {
            columns: vec!["ts".into(), "level".into(), "msg".into()],
            kinds: Default::default(),
        };
        for (profile, spec, mk) in [("logfmt", logfmt, 0), ("JSON lines", json, 1)] {
            let mut h = state_for(profile, Some(spec));
            for (i, (lvl, want)) in LEVELS.iter().enumerate() {
                let l = lvl.to_lowercase();
                let text = if mk == 0 {
                    format!("ts=2026-09-29T10:00:00Z level={l} took=3ms msg=\"hello\"")
                } else {
                    format!(
                        "{{\"ts\":\"2026-09-29T10:00:00Z\",\"level\":\"{l}\",\"msg\":\"hello\"}}"
                    )
                };
                let p = h.prepare(&line(i as u64 * 200, &text));
                let spans = fg_spans(&p);
                assert!(
                    spans.contains(&(l.clone(), *want)),
                    "{profile} {l}: {spans:?}"
                );
                assert!(
                    spans
                        .iter()
                        .any(|(t, c)| t.contains("ts") && *c == Sem::Muted),
                    "{profile}: keys or timestamps are muted: {spans:?}"
                );
                let row = p.hl.line_style.is_some_and(|s| s.bg.is_some());
                assert_eq!(
                    row,
                    matches!(*lvl, "ERROR" | "FATAL" | "WARN"),
                    "{profile} {l}"
                );
            }
        }
    }

    #[test]
    fn a_user_rule_wins_over_the_built_in_severity_colours() {
        use oxtail_highlight::Style;
        let set = ProfileSet::builtin();
        for prof in set.profiles() {
            // Built-in rules stay below the priority a new rule gets (0), so
            // that a rule a user adds wins where they overlap.
            let adapted = rules_from_profile(prof);
            assert!(
                adapted.rules.iter().all(|r| r.priority < 0),
                "{}: {:?}",
                prof.name,
                adapted
                    .rules
                    .iter()
                    .filter(|r| r.priority >= 0)
                    .map(|r| &r.name)
                    .collect::<Vec<_>>()
            );
        }
        let generic = rules_from_profile(set.by_name("Generic").unwrap());
        let mut rules = generic.rules;
        rules.push(
            Rule::regex("mine", r"\bERROR\b").styled(Style::fg(ColorRef::solid(Sem::Accent2))),
        );
        let mut h = HighlightState::with_defaults();
        h.set_rules(rules, None);
        let p = h.prepare(&line(0, "2026-09-22 14:42:31 ERROR boom"));
        let spans = fg_spans(&p);
        assert!(
            spans.contains(&("ERROR".to_string(), Sem::Accent2)),
            "{spans:?}"
        );
        assert!(
            !spans.contains(&("ERROR".to_string(), Sem::Error)),
            "{spans:?}"
        );
    }

    #[test]
    fn error_rows_and_severity_ticks_show_in_the_minimap() {
        let mut h = state_for("Generic", None);
        h.prepare(&line(0, "12:00:00 INFO fine"));
        h.prepare(&line(40, "12:00:01 ERROR bad"));
        h.prepare(&line(80, "12:00:02 WARN meh"));
        assert!(h.ticks.contains_key(&40) && h.ticks.contains_key(&80));
        assert!(!h.ticks.contains_key(&0));
    }
}
