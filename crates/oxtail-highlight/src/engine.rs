//! The rule engine: compiles rules once, highlights lines cheaply.
//!
//! # Cost model
//!
//! Compilation puts every literal rule into one `aho-corasick` automaton and
//! every regex rule into one `RegexSet`. Highlighting a line is then one
//! automaton pass and one `RegexSet` pass; only the rules that hit are
//! verified further (regex spans / capture groups, column tests). Rules that
//! do not appear in the line cost nothing beyond the shared passes.
//!
//! # Layering
//!
//! Each hit produces a *paint*: a byte range plus the rule's style. Paints
//! are sorted by `(priority, rule order)` and composed: at every position the
//! styles of all covering paints are layered, lowest priority first, so a
//! higher-priority rule overrides only the fields it sets and everything else
//! shows through. The result is a list of sorted, non-overlapping spans.
//! `Scope::Line` rules do not produce spans; they compose into
//! [`LineHighlight::line_style`], which the renderer draws underneath.
//!
//! # Bounds
//!
//! Only the first 64 KiB of a line are examined, and at most 256 matches per
//! rule and 512 paints per line are used, so hostile lines stay cheap.

use std::ops::Range;

use aho_corasick::{AhoCorasick, MatchKind};
use regex::{Regex, RegexBuilder, RegexSet};

use crate::color::{ColorRef, SemanticColor};
use crate::rule::{ColumnOp, Rule, RuleActions, RuleMatcher, Scope};
use crate::style::{Style, StyledSpan};

/// Longest prefix of a line that is examined.
pub const MAX_LINE_BYTES: usize = 64 * 1024;
const MAX_MATCHES_PER_RULE: usize = 256;
const MAX_PAINTS: usize = 512;

/// Why a rule set could not be compiled. `rule` is the index in the slice
/// passed to [`CompiledRules::compile`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HighlightError {
    /// A literal rule has empty text.
    #[error("rule {rule} ({name}): literal text is empty")]
    EmptyLiteral {
        /// Rule index.
        rule: usize,
        /// Rule name.
        name: String,
    },
    /// A regex does not compile.
    #[error("rule {rule} ({name}): invalid regex: {message}")]
    InvalidRegex {
        /// Rule index.
        rule: usize,
        /// Rule name.
        name: String,
        /// The regex engine's explanation.
        message: String,
    },
    /// `Scope::Group(n)` refers to a group the pattern does not have.
    #[error("rule {rule} ({name}): capture group {group} does not exist (pattern has {available})")]
    BadGroup {
        /// Rule index.
        rule: usize,
        /// Rule name.
        name: String,
        /// The requested group.
        group: usize,
        /// Number of groups available, not counting group 0.
        available: usize,
    },
    /// A numeric column comparison has a value that is not a number.
    #[error("rule {rule} ({name}): {value:?} is not a number")]
    BadNumber {
        /// Rule index.
        rule: usize,
        /// Rule name.
        name: String,
        /// The offending value.
        value: String,
    },
    /// The multi-pattern automaton could not be built (too many patterns).
    #[error("could not build the matcher: {0}")]
    Build(String),
}

/// The result of highlighting one line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LineHighlight {
    /// Composite style of all matching `Scope::Line` rules (background layer).
    pub line_style: Option<Style>,
    /// Sorted, non-overlapping, char-aligned styled ranges of the line.
    pub spans: Vec<StyledSpan>,
    /// A matching rule asks for the line to be hidden.
    pub hidden: bool,
    /// A matching rule asks for an alert.
    pub alert: bool,
    /// A matching rule asks for an automatic bookmark.
    pub bookmark: bool,
    /// Minimap tick colour (from the highest-priority matching rule that has one).
    pub minimap: Option<ColorRef>,
    /// Gutter marker colour (from the highest-priority matching rule that
    /// wants one; the rule's minimap colour, else its background, else its
    /// foreground, else `accent1`).
    pub gutter: Option<ColorRef>,
    /// Indices (into the slice given to `compile`) of the rules that matched,
    /// ascending.
    pub matched_rules: Vec<usize>,
}

/// Column names and their byte ranges within the line, as produced by a
/// column parser.
pub type ColumnRanges<'a> = &'a [(&'a str, Range<usize>)];

#[derive(Clone)]
struct ColumnTest {
    column: String,
    op: ColumnOp,
    value: String,
    number: Option<f64>,
    regex: Option<Regex>,
}

#[derive(Clone)]
enum Body {
    /// Found by the automaton; `exact` is set for case-sensitive rules.
    Literal {
        exact: Option<Vec<u8>>,
    },
    /// Found through the `RegexSet`.
    Regex(Regex),
    Column(ColumnTest),
}

#[derive(Clone)]
struct CompiledRule {
    /// Index in the caller's slice.
    index: usize,
    priority: i32,
    scope: Scope,
    style: Style,
    minimap: Option<ColorRef>,
    gutter: bool,
    actions: RuleActions,
    body: Body,
}

/// A compiled rule set. Cheap to clone, `Send + Sync`.
#[derive(Clone)]
pub struct CompiledRules {
    rules: Vec<CompiledRule>,
    literals: Option<AhoCorasick>,
    /// Automaton pattern id -> index in `rules`.
    literal_rules: Vec<usize>,
    set: Option<RegexSet>,
    /// Regex set index -> index in `rules`.
    set_rules: Vec<usize>,
    /// Indices in `rules` of column-condition rules.
    column_rules: Vec<usize>,
}

impl std::fmt::Debug for CompiledRules {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledRules")
            .field("rules", &self.rules.len())
            .finish()
    }
}

struct Paint {
    priority: i32,
    order: usize,
    range: Range<usize>,
    style: Style,
}

#[derive(Default)]
struct Acc {
    paints: Vec<Paint>,
    line_styles: Vec<(i32, usize, Style)>,
    matched: Vec<usize>,
}

impl CompiledRules {
    /// Compiles `rules`. Disabled rules are skipped (but keep their index).
    pub fn compile(rules: &[Rule]) -> Result<CompiledRules, HighlightError> {
        let mut out = CompiledRules {
            rules: Vec::new(),
            literals: None,
            literal_rules: Vec::new(),
            set: None,
            set_rules: Vec::new(),
            column_rules: Vec::new(),
        };
        let mut literal_patterns: Vec<String> = Vec::new();
        let mut set_patterns: Vec<String> = Vec::new();

        for (index, rule) in rules.iter().enumerate().filter(|(_, r)| r.enabled) {
            let err_name = || rule.name.clone();
            let slot = out.rules.len();
            let body = match &rule.matcher {
                RuleMatcher::Literal {
                    text,
                    case_sensitive,
                } => {
                    if text.is_empty() {
                        return Err(HighlightError::EmptyLiteral {
                            rule: index,
                            name: err_name(),
                        });
                    }
                    if *case_sensitive || text.is_ascii() {
                        check_group(&rule.scope, 1, index, &rule.name)?;
                        literal_patterns.push(text.clone());
                        out.literal_rules.push(slot);
                        Body::Literal {
                            exact: case_sensitive.then(|| text.as_bytes().to_vec()),
                        }
                    } else {
                        // Non-ASCII, case-insensitive: needs Unicode folding.
                        let pat = regex::escape(text);
                        regex_body(&pat, false, rule, index, &mut out, &mut set_patterns, slot)?
                    }
                }
                RuleMatcher::Regex {
                    pattern,
                    case_sensitive,
                } => regex_body(
                    pattern,
                    *case_sensitive,
                    rule,
                    index,
                    &mut out,
                    &mut set_patterns,
                    slot,
                )?,
                RuleMatcher::Column { column, op, value } => {
                    let number = parse_number(value, true);
                    if matches!(
                        op,
                        ColumnOp::Gt | ColumnOp::Ge | ColumnOp::Lt | ColumnOp::Le
                    ) && number.is_none()
                    {
                        return Err(HighlightError::BadNumber {
                            rule: index,
                            name: err_name(),
                            value: value.clone(),
                        });
                    }
                    let regex = if *op == ColumnOp::Regex {
                        Some(build_regex(value, true, index, &rule.name)?)
                    } else {
                        None
                    };
                    out.column_rules.push(slot);
                    Body::Column(ColumnTest {
                        column: column.clone(),
                        op: *op,
                        value: value.clone(),
                        number,
                        regex,
                    })
                }
            };
            out.rules.push(CompiledRule {
                index,
                priority: rule.priority,
                scope: rule.scope.clone(),
                style: rule.style,
                minimap: rule.minimap,
                gutter: rule.gutter_marker,
                actions: rule.actions,
                body,
            });
        }

        if !literal_patterns.is_empty() {
            out.literals = Some(
                AhoCorasick::builder()
                    .match_kind(MatchKind::Standard)
                    .ascii_case_insensitive(true)
                    .build(&literal_patterns)
                    .map_err(|e| HighlightError::Build(e.to_string()))?,
            );
        }
        if !set_patterns.is_empty() {
            out.set = Some(
                RegexSet::new(&set_patterns).map_err(|e| HighlightError::Build(e.to_string()))?,
            );
        }
        Ok(out)
    }

    /// Whether the set has no active rules.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Highlights one line (no terminator). `columns` gives the parsed
    /// columns' byte ranges within `line`; ranges that are out of bounds or
    /// not on char boundaries are ignored.
    pub fn highlight(&self, line: &str, columns: Option<ColumnRanges<'_>>) -> LineHighlight {
        if self.rules.is_empty() {
            return LineHighlight::default();
        }
        let line = truncate(line);
        let cols: Vec<(&str, Range<usize>)> = columns
            .unwrap_or(&[])
            .iter()
            .filter(|(_, r)| {
                r.start <= r.end
                    && r.end <= line.len()
                    && line.is_char_boundary(r.start)
                    && line.is_char_boundary(r.end)
            })
            .map(|(n, r)| (*n, r.clone()))
            .collect();
        let mut acc = Acc::default();

        // One automaton pass for all literal rules.
        if let Some(ac) = &self.literals {
            let bytes = line.as_bytes();
            let mut hits: Vec<(usize, Range<usize>)> = Vec::new();
            for m in ac.find_overlapping_iter(bytes) {
                let ri = self.literal_rules[m.pattern().as_usize()];
                let range = m.range();
                if let Body::Literal { exact: Some(x) } = &self.rules[ri].body
                    && &bytes[range.clone()] != x.as_slice()
                {
                    continue;
                }
                hits.push((ri, range));
                if hits.len() >= MAX_PAINTS {
                    break;
                }
            }
            hits.sort_by_key(|(ri, r)| (*ri, r.start));
            let mut i = 0;
            while i < hits.len() {
                let ri = hits[i].0;
                let mut ranges = Vec::new();
                while i < hits.len() && hits[i].0 == ri {
                    ranges.push(hits[i].1.clone());
                    i += 1;
                }
                self.apply(&mut acc, ri, &ranges, &cols);
            }
        }

        // One RegexSet pass for all regex rules, then spans for the hits.
        if let Some(set) = &self.set {
            for si in set.matches(line).iter() {
                let ri = self.set_rules[si];
                let rule = &self.rules[ri];
                let Body::Regex(re) = &rule.body else {
                    continue;
                };
                let ranges = match &rule.scope {
                    Scope::Line => Vec::new(),
                    Scope::Group(n) => re
                        .captures_iter(line)
                        .take(MAX_MATCHES_PER_RULE)
                        .filter_map(|c| c.get(*n).map(|m| m.range()))
                        .collect(),
                    Scope::Match | Scope::Column(_) => re
                        .find_iter(line)
                        .take(MAX_MATCHES_PER_RULE)
                        .map(|m| m.range())
                        .collect(),
                };
                self.apply(&mut acc, ri, &ranges, &cols);
            }
        }

        // Column conditions.
        for &ri in &self.column_rules {
            let rule = &self.rules[ri];
            let Body::Column(test) = &rule.body else {
                continue;
            };
            let Some((_, range)) = find_column(&cols, &test.column) else {
                continue;
            };
            if eval_column(test, &line[range.clone()]) {
                self.apply(&mut acc, ri, std::slice::from_ref(range), &cols);
            }
        }

        self.finish(acc)
    }

    /// Records a matching rule and its paints.
    fn apply(
        &self,
        acc: &mut Acc,
        ri: usize,
        ranges: &[Range<usize>],
        cols: &[(&str, Range<usize>)],
    ) {
        let rule = &self.rules[ri];
        let is_column_rule = matches!(rule.body, Body::Column(_));
        let mut paint = |range: Range<usize>| {
            if !range.is_empty() && !rule.style.is_empty() && acc.paints.len() < MAX_PAINTS {
                acc.paints.push(Paint {
                    priority: rule.priority,
                    order: ri,
                    range,
                    style: rule.style,
                });
            }
        };
        match &rule.scope {
            Scope::Line => {}
            Scope::Match | Scope::Group(_) => {
                for r in ranges {
                    paint(r.clone());
                }
            }
            Scope::Column(name) => match find_column(cols, name) {
                // A column condition that held still counts as a match even
                // if the styled column is missing; a text rule cannot say
                // whether its match lies in a column it cannot see.
                None if !is_column_rule => return,
                None => {}
                Some((_, col)) => {
                    let applies = is_column_rule
                        || ranges
                            .iter()
                            .any(|r| r.start >= col.start && r.end <= col.end);
                    if !applies {
                        return;
                    }
                    paint(col.clone());
                }
            },
        }
        acc.matched.push(ri);
        if matches!(rule.scope, Scope::Line) && !rule.style.is_empty() {
            acc.line_styles.push((rule.priority, ri, rule.style));
        }
    }

    fn finish(&self, mut acc: Acc) -> LineHighlight {
        let mut out = LineHighlight::default();
        acc.matched.sort_unstable();
        acc.matched.dedup();
        let mut best_minimap: Option<((i32, usize), ColorRef)> = None;
        let mut best_gutter: Option<((i32, usize), ColorRef)> = None;
        for &ri in &acc.matched {
            let r = &self.rules[ri];
            out.matched_rules.push(r.index);
            out.hidden |= r.actions.hide;
            out.alert |= r.actions.alert;
            out.bookmark |= r.actions.bookmark;
            let key = (r.priority, ri);
            if let Some(c) = r.minimap
                && best_minimap.is_none_or(|(k, _)| key >= k)
            {
                best_minimap = Some((key, c));
            }
            if r.gutter && best_gutter.is_none_or(|(k, _)| key >= k) {
                let c = r
                    .minimap
                    .or(r.style.bg)
                    .or(r.style.fg)
                    .unwrap_or(ColorRef::solid(SemanticColor::Accent1));
                best_gutter = Some((key, c));
            }
        }
        out.minimap = best_minimap.map(|(_, c)| c);
        out.gutter = best_gutter.map(|(_, c)| c);

        acc.line_styles.sort_by_key(|(p, ri, _)| (*p, *ri));
        let line_style = acc
            .line_styles
            .iter()
            .fold(Style::default(), |s, (_, _, over)| s.layer(over));
        out.line_style = (!line_style.is_empty()).then_some(line_style);
        out.spans = compose(acc.paints);
        out
    }
}

/// Builds the regex body for rule `slot`, registering it in the set.
fn regex_body(
    pattern: &str,
    case_sensitive: bool,
    rule: &Rule,
    index: usize,
    out: &mut CompiledRules,
    set_patterns: &mut Vec<String>,
    slot: usize,
) -> Result<Body, HighlightError> {
    let re = build_regex(pattern, case_sensitive, index, &rule.name)?;
    if let Scope::Group(n) = rule.scope {
        let available = re.captures_len() - 1;
        if n > available {
            return Err(HighlightError::BadGroup {
                rule: index,
                name: rule.name.clone(),
                group: n,
                available,
            });
        }
    }
    set_patterns.push(flagged(pattern, case_sensitive));
    out.set_rules.push(slot);
    Ok(Body::Regex(re))
}

fn flagged(pattern: &str, case_sensitive: bool) -> String {
    if case_sensitive {
        pattern.to_owned()
    } else {
        format!("(?i){pattern}")
    }
}

fn build_regex(
    pattern: &str,
    case_sensitive: bool,
    rule: usize,
    name: &str,
) -> Result<Regex, HighlightError> {
    RegexBuilder::new(&flagged(pattern, case_sensitive))
        .build()
        .map_err(|e| HighlightError::InvalidRegex {
            rule,
            name: name.to_owned(),
            message: e.to_string(),
        })
}

/// Literal rules have no capture groups beyond 0.
fn check_group(
    scope: &Scope,
    _available: usize,
    rule: usize,
    name: &str,
) -> Result<(), HighlightError> {
    match scope {
        Scope::Group(n) if *n > 0 => Err(HighlightError::BadGroup {
            rule,
            name: name.to_owned(),
            group: *n,
            available: 0,
        }),
        _ => Ok(()),
    }
}

fn truncate(line: &str) -> &str {
    if line.len() <= MAX_LINE_BYTES {
        return line;
    }
    let mut cut = MAX_LINE_BYTES;
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    &line[..cut]
}

fn find_column<'a>(
    cols: &'a [(&str, Range<usize>)],
    name: &str,
) -> Option<&'a (&'a str, Range<usize>)> {
    cols.iter().find(|(n, _)| n.eq_ignore_ascii_case(name))
}

/// Layers overlapping paints into sorted, non-overlapping spans.
fn compose(mut paints: Vec<Paint>) -> Vec<StyledSpan> {
    if paints.is_empty() {
        return Vec::new();
    }
    paints.sort_by_key(|p| (p.priority, p.order));
    let mut bounds: Vec<usize> = paints
        .iter()
        .flat_map(|p| [p.range.start, p.range.end])
        .collect();
    bounds.sort_unstable();
    bounds.dedup();
    let mut spans: Vec<StyledSpan> = Vec::new();
    for w in bounds.windows(2) {
        let (a, b) = (w[0], w[1]);
        let style = paints
            .iter()
            .filter(|p| p.range.start <= a && p.range.end >= b)
            .fold(Style::default(), |s, p| s.layer(&p.style));
        if style.is_empty() {
            continue;
        }
        match spans.last_mut() {
            Some(last) if last.range.end == a && last.style == style => last.range.end = b,
            _ => spans.push(StyledSpan { range: a..b, style }),
        }
    }
    spans
}

fn eval_column(t: &ColumnTest, text: &str) -> bool {
    match t.op {
        ColumnOp::Eq | ColumnOp::Ne => {
            let equal = match (t.number, parse_number(text, false)) {
                (Some(a), Some(b)) => a == b,
                _ => text.trim().eq_ignore_ascii_case(t.value.trim()),
            };
            equal == (t.op == ColumnOp::Eq)
        }
        ColumnOp::Contains => text
            .to_ascii_lowercase()
            .contains(&t.value.to_ascii_lowercase()),
        ColumnOp::Regex => t.regex.as_ref().is_some_and(|r| r.is_match(text)),
        ColumnOp::Gt | ColumnOp::Ge | ColumnOp::Lt | ColumnOp::Le => {
            let (Some(a), Some(b)) = (parse_number(text, true), t.number) else {
                return false;
            };
            match t.op {
                ColumnOp::Gt => a > b,
                ColumnOp::Ge => a >= b,
                ColumnOp::Lt => a < b,
                _ => a <= b,
            }
        }
    }
}

/// Parses a number out of column text.
///
/// Lenient mode reads the leading number and ignores what follows (`"250ms"`,
/// `"12%"`), accepts surrounding quotes, and thousands separators as in
/// `"1,234.5"`. Strict mode requires the whole trimmed text to be a number.
pub fn parse_number(text: &str, lenient: bool) -> Option<f64> {
    let t = text.trim().trim_matches(['"', '\'']).trim();
    if !lenient {
        return t.parse::<f64>().ok().filter(|v| v.is_finite());
    }
    let b = t.as_bytes();
    let mut s = String::new();
    let mut i = 0;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        if b[i] == b'-' {
            s.push('-');
        }
        i += 1;
    }
    let mut digits = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() {
            s.push(b[i] as char);
            digits += 1;
            i += 1;
        } else if b[i] == b','
            && digits > 0
            && b.get(i + 1..i + 4)
                .is_some_and(|d| d.iter().all(u8::is_ascii_digit))
            && !b.get(i + 4).is_some_and(u8::is_ascii_digit)
        {
            i += 1; // thousands separator
        } else {
            break;
        }
    }
    if i < b.len() && b[i] == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
        s.push('.');
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            s.push(b[i] as char);
            digits += 1;
            i += 1;
        }
    }
    if digits == 0 {
        return None;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        let mut exp = String::from("e");
        if j < b.len() && (b[j] == b'-' || b[j] == b'+') {
            exp.push(b[j] as char);
            j += 1;
        }
        let start = j;
        while j < b.len() && b[j].is_ascii_digit() {
            exp.push(b[j] as char);
            j += 1;
        }
        if j > start {
            s.push_str(&exp);
        }
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::SemanticColor as S;

    fn fg(c: S) -> Style {
        Style::fg(ColorRef::solid(c))
    }

    fn compile(rules: Vec<Rule>) -> CompiledRules {
        CompiledRules::compile(&rules).unwrap()
    }

    fn ranges(h: &LineHighlight) -> Vec<Range<usize>> {
        h.spans.iter().map(|s| s.range.clone()).collect()
    }

    #[test]
    fn literal_case_handling() {
        let rules = compile(vec![
            Rule::literal("ci", "warn").styled(fg(S::Warn)),
            Rule::new(
                "cs",
                RuleMatcher::Literal {
                    text: "ERROR".into(),
                    case_sensitive: true,
                },
            )
            .styled(fg(S::Error)),
        ]);
        let h = rules.highlight("WARN error ERROR Warn", None);
        assert_eq!(ranges(&h), vec![0..4, 11..16, 17..21]);
        assert_eq!(h.matched_rules, vec![0, 1]);
        let h = rules.highlight("error only", None);
        assert!(h.spans.is_empty() && h.matched_rules.is_empty());
    }

    #[test]
    fn unicode_case_insensitive_literal() {
        let rules = compile(vec![Rule::literal("u", "\u{c9}COLE").styled(fg(S::Info))]);
        let h = rules.highlight("la \u{e9}cole", None);
        assert_eq!(ranges(&h), vec![3..9]);
    }

    #[test]
    fn regex_scopes() {
        let rules = compile(vec![
            Rule::regex("g", r"user=(\w+)")
                .scoped(Scope::Group(1))
                .styled(fg(S::Accent1)),
            Rule::regex("m", r"\d+").styled(fg(S::Accent2)),
        ]);
        let h = rules.highlight("user=bob id 42", None);
        assert_eq!(ranges(&h), vec![5..8, 12..14]);
    }

    #[test]
    fn priority_layers_styles_and_higher_wins() {
        let rules = compile(vec![
            Rule::regex("wide", r"error \w+")
                .styled(fg(S::Info).italic())
                .with_priority(1),
            Rule::regex("narrow", r"error")
                .styled(fg(S::Error).bold())
                .with_priority(2),
        ]);
        let h = rules.highlight("an error occurred", None);
        assert_eq!(ranges(&h), vec![3..8, 8..17]);
        assert_eq!(h.spans[0].style, fg(S::Error).bold().italic());
        assert_eq!(h.spans[1].style, fg(S::Info).italic());
    }

    #[test]
    fn equal_priority_later_rule_wins() {
        let rules = compile(vec![
            Rule::regex("a", "x").styled(fg(S::Info)),
            Rule::regex("b", "x").styled(fg(S::Error)),
        ]);
        assert_eq!(rules.highlight("x", None).spans[0].style, fg(S::Error));
    }

    #[test]
    fn line_scope_actions_minimap_and_gutter() {
        let rules = compile(vec![
            Rule::regex("bg", "FATAL")
                .scoped(Scope::Line)
                .styled(Style::default().on(ColorRef::subtle(S::Error)))
                .with_minimap(ColorRef::solid(S::Error))
                .with_gutter()
                .with_actions(RuleActions {
                    alert: true,
                    hide: false,
                    bookmark: true,
                }),
            Rule::regex("hide", "healthcheck")
                .scoped(Scope::Line)
                .with_actions(RuleActions {
                    hide: true,
                    ..Default::default()
                }),
        ]);
        let h = rules.highlight("FATAL boom", None);
        assert!(h.spans.is_empty());
        assert_eq!(
            h.line_style,
            Some(Style::default().on(ColorRef::subtle(S::Error)))
        );
        assert!(h.alert && h.bookmark && !h.hidden);
        assert_eq!(h.minimap, Some(ColorRef::solid(S::Error)));
        assert_eq!(h.gutter, Some(ColorRef::solid(S::Error)));
        let h = rules.highlight("GET /healthcheck", None);
        assert!(h.hidden && h.line_style.is_none());
        assert_eq!(h.matched_rules, vec![1]);
    }

    #[test]
    fn column_rules_and_scopes() {
        let rules = compile(vec![
            Rule::new(
                "slow",
                RuleMatcher::Column {
                    column: "ms".into(),
                    op: ColumnOp::Gt,
                    value: "1000".into(),
                },
            )
            .scoped(Scope::Column("ms".into()))
            .styled(fg(S::Warn)),
            Rule::new(
                "lvl",
                RuleMatcher::Column {
                    column: "Level".into(),
                    op: ColumnOp::Eq,
                    value: "warn".into(),
                },
            )
            .scoped(Scope::Line)
            .styled(Style::default().on(ColorRef::subtle(S::Warn))),
            Rule::regex("txt", "42")
                .scoped(Scope::Column("msg".into()))
                .styled(fg(S::Info)),
        ]);
        let line = "WARN 1,500ms hello 42";
        let cols = [("level", 0..4), ("ms", 5..12), ("msg", 13..21)];
        let h = rules.highlight(line, Some(&cols));
        assert_eq!(ranges(&h), vec![5..12, 13..21]);
        assert!(h.line_style.is_some());
        assert_eq!(h.matched_rules, vec![0, 1, 2]);
        let h = rules.highlight(line, None);
        assert!(h.spans.is_empty() && h.matched_rules.is_empty());
        let cols = [("level", 0..4), ("ms", 5..12), ("msg", 13..999)];
        let h = rules.highlight(line, Some(&cols));
        assert_eq!(ranges(&h), vec![5..12]);
    }

    #[test]
    fn column_operators() {
        let t = |op, value: &str, text: &str| {
            let rules = compile(vec![
                Rule::new(
                    "c",
                    RuleMatcher::Column {
                        column: "c".into(),
                        op,
                        value: value.into(),
                    },
                )
                .scoped(Scope::Line)
                .styled(fg(S::Info)),
            ]);
            let cols = [("c", 0..text.len())];
            !rules.highlight(text, Some(&cols)).matched_rules.is_empty()
        };
        assert!(t(ColumnOp::Eq, "200", "200.0"));
        assert!(t(ColumnOp::Ne, "200", "404"));
        assert!(!t(ColumnOp::Ne, "OK", "ok"));
        assert!(t(ColumnOp::Contains, "TIME", "timeout"));
        assert!(t(ColumnOp::Regex, r"^5\d\d$", "503"));
        assert!(t(ColumnOp::Ge, "5", "5"));
        assert!(t(ColumnOp::Lt, "5", "-3"));
        assert!(!t(ColumnOp::Le, "5", "abc"));
        assert!(t(ColumnOp::Gt, "100", "250ms"));
    }

    #[test]
    fn lenient_number_parsing() {
        assert_eq!(parse_number(" 1,234.5 ms", true), Some(1234.5));
        assert_eq!(parse_number("\"42\"", true), Some(42.0));
        assert_eq!(parse_number("-7%", true), Some(-7.0));
        assert_eq!(parse_number("1e3x", true), Some(1000.0));
        assert_eq!(parse_number("1,5", true), Some(1.0));
        assert_eq!(parse_number(".5", true), Some(0.5));
        assert_eq!(parse_number("abc", true), None);
        assert_eq!(parse_number("12ms", false), None);
        assert_eq!(parse_number("inf", false), None);
    }

    #[test]
    fn compile_errors() {
        let e = CompiledRules::compile(&[Rule::regex("bad", "(")]).unwrap_err();
        assert!(matches!(e, HighlightError::InvalidRegex { rule: 0, .. }));
        let e = CompiledRules::compile(&[Rule::literal("e", "")]).unwrap_err();
        assert!(matches!(e, HighlightError::EmptyLiteral { .. }));
        let e = CompiledRules::compile(&[Rule::regex("g", "a(b)").scoped(Scope::Group(2))])
            .unwrap_err();
        assert!(matches!(
            e,
            HighlightError::BadGroup {
                group: 2,
                available: 1,
                ..
            }
        ));
        let e =
            CompiledRules::compile(&[Rule::literal("g", "a").scoped(Scope::Group(1))]).unwrap_err();
        assert!(matches!(e, HighlightError::BadGroup { .. }));
        let e = CompiledRules::compile(&[Rule::new(
            "n",
            RuleMatcher::Column {
                column: "a".into(),
                op: ColumnOp::Gt,
                value: "x".into(),
            },
        )])
        .unwrap_err();
        assert!(matches!(e, HighlightError::BadNumber { .. }));
    }

    #[test]
    fn disabled_rules_are_skipped_but_indices_are_kept() {
        let mut off = Rule::literal("off", "a").styled(fg(S::Error));
        off.enabled = false;
        let rules = compile(vec![off, Rule::literal("on", "a").styled(fg(S::Info))]);
        let h = rules.highlight("a", None);
        assert_eq!(h.matched_rules, vec![1]);
        assert!(compile(vec![]).highlight("a", None).spans.is_empty());
    }

    #[test]
    fn overlapping_literal_hits_and_empty_matches_do_not_break_spans() {
        let rules = compile(vec![
            Rule::literal("a", "aa").styled(fg(S::Info)),
            Rule::regex("e", "x*").styled(fg(S::Error)),
        ]);
        let h = rules.highlight("aaa", None);
        assert_eq!(ranges(&h), vec![0..3]);
    }

    #[test]
    fn very_long_lines_are_truncated_on_a_char_boundary() {
        let rules = compile(vec![Rule::literal("a", "\u{e9}").styled(fg(S::Info))]);
        let line = "\u{e9}".repeat(MAX_LINE_BYTES);
        let h = rules.highlight(&line, None);
        assert!(h.spans.iter().all(|s| s.range.end <= MAX_LINE_BYTES));
    }
}
