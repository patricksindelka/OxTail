//! Adapter between the rule tables stored in profiles (`oxtail-config`) and
//! [`oxtail_highlight::Rule`].
//!
//! The config crate keeps `[[rules]]` as opaque TOML in the documented shape
//!
//! ```toml
//! [[rules]]
//! name  = "errors"
//! match = { regex = '\bERROR\b' }            # or { literal = "x" } or { column = "level", op = "eq", value = "error" }
//! scope = "line"                             # line | match | group:1 | column:level
//! style = { fg = "error", bg = "error.subtle", bold = true }
//! priority = 0
//! alert = false                              # also: hide, bookmark
//! minimap = "error"                          # optional
//! gutter = true                              # optional
//! ```
//!
//! while `oxtail-highlight` has its own serde shape (`matcher = { type = ... }`,
//! externally tagged scopes). This module converts both ways, so profiles
//! written by the rule editor use the documented shape. It also accepts the
//! highlight crate's native shape when a table has a `matcher` key.
//!
//! Case sensitivity, which the documented shape leaves open: a `regex` is
//! case-sensitive unless `case_sensitive = false` / `ignore_case = true` (the
//! built-in profiles use `(?i)` when they want to ignore case); a `literal`
//! ignores case unless `case_sensitive = true`.
//!
//! The highlight crate has no `fatal` colour but the built-in profiles and
//! themes use one; it is mapped to `error` here.

use oxtail_config::Profile;
use oxtail_highlight::{ColorRef, ColumnOp, Rule, RuleActions, RuleMatcher, Scope, Style};

/// The rules of a profile plus what went wrong while converting them.
#[derive(Debug, Clone, Default)]
pub struct Adapted {
    /// Successfully converted rules (in profile order).
    pub rules: Vec<Rule>,
    /// One message per rule that could not be converted.
    pub warnings: Vec<String>,
}

/// Converts all rules of `profile`. Bad rules are skipped with a warning.
pub fn rules_from_profile(profile: &Profile) -> Adapted {
    let mut out = Adapted::default();
    for (i, t) in profile.rules.iter().enumerate() {
        match rule_from_table(t) {
            Ok(r) => out.rules.push(r),
            Err(e) => out
                .warnings
                .push(format!("profile '{}', rule {}: {e}", profile.name, i + 1)),
        }
    }
    out
}

/// Maps colour names the highlight crate does not know to ones it does.
fn alias_color(s: &str) -> String {
    let t = s.trim();
    match t.to_ascii_lowercase().as_str() {
        "fatal" => "error".to_string(),
        "fatal.subtle" => "error.subtle".to_string(),
        _ => t.to_string(),
    }
}

fn parse_color(v: &toml::Value) -> Result<ColorRef, String> {
    let s = v.as_str().ok_or("a colour must be a string")?;
    alias_color(s)
        .parse::<ColorRef>()
        .map_err(|e| e.to_string())
}

fn parse_style(v: &toml::Value) -> Result<Style, String> {
    let t = v.as_table().ok_or("style must be a table")?;
    let mut style = Style::default();
    for (k, val) in t {
        match k.as_str() {
            "fg" => style.fg = Some(parse_color(val)?),
            "bg" => style.bg = Some(parse_color(val)?),
            "bold" => style.bold = val.as_bool().ok_or("bold must be true or false")?,
            "italic" => style.italic = val.as_bool().ok_or("italic must be true or false")?,
            "underline" => {
                style.underline = val.as_bool().ok_or("underline must be true or false")?;
            }
            "dim" => style.dim = val.as_bool().ok_or("dim must be true or false")?,
            _ => {}
        }
    }
    Ok(style)
}

fn parse_scope(s: &str) -> Result<Scope, String> {
    let t = s.trim();
    if t.eq_ignore_ascii_case("line") {
        Ok(Scope::Line)
    } else if t.eq_ignore_ascii_case("match") {
        Ok(Scope::Match)
    } else if let Some(n) = t.strip_prefix("group:") {
        n.trim()
            .parse::<usize>()
            .map(Scope::Group)
            .map_err(|_| format!("bad capture group in scope '{t}'"))
    } else if let Some(c) = t.strip_prefix("column:") {
        Ok(Scope::Column(c.trim().to_string()))
    } else {
        Err(format!(
            "unknown scope '{t}' (use line, match, group:N or column:NAME)"
        ))
    }
}

fn scope_to_string(s: &Scope) -> String {
    match s {
        Scope::Line => "line".into(),
        Scope::Match => "match".into(),
        Scope::Group(n) => format!("group:{n}"),
        Scope::Column(c) => format!("column:{c}"),
    }
}

fn parse_matcher(v: &toml::Value) -> Result<RuleMatcher, String> {
    let t = v.as_table().ok_or("match must be a table")?;
    let case = t
        .get("case_sensitive")
        .and_then(toml::Value::as_bool)
        .or_else(|| {
            t.get("ignore_case")
                .and_then(toml::Value::as_bool)
                .map(|b| !b)
        });
    if let Some(p) = t.get("regex") {
        let pattern = p.as_str().ok_or("regex must be a string")?.to_string();
        return Ok(RuleMatcher::Regex {
            pattern,
            case_sensitive: case.unwrap_or(true),
        });
    }
    if let Some(p) = t.get("literal") {
        let text = p.as_str().ok_or("literal must be a string")?.to_string();
        return Ok(RuleMatcher::Literal {
            text,
            case_sensitive: case.unwrap_or(false),
        });
    }
    if let Some(c) = t.get("column") {
        let column = c.as_str().ok_or("column must be a string")?.to_string();
        let op = match t.get("op").and_then(toml::Value::as_str) {
            Some(op) => toml::Value::String(op.to_string())
                .try_into::<ColumnOp>()
                .map_err(|_| format!("unknown operator '{op}'"))?,
            None => ColumnOp::Eq,
        };
        let value = match t.get("value") {
            Some(toml::Value::String(s)) => s.clone(),
            Some(toml::Value::Integer(i)) => i.to_string(),
            Some(toml::Value::Float(f)) => f.to_string(),
            Some(toml::Value::Boolean(b)) => b.to_string(),
            _ => return Err("a column condition needs a value".into()),
        };
        return Ok(RuleMatcher::Column { column, op, value });
    }
    Err("match needs regex, literal or column".into())
}

/// Converts one `[[rules]]` table.
pub fn rule_from_table(t: &toml::Table) -> Result<Rule, String> {
    if t.contains_key("matcher") {
        // The highlight crate's own shape.
        let mut copy = t.clone();
        if let Some(toml::Value::Table(style)) = copy.get_mut("style") {
            for key in ["fg", "bg"] {
                if let Some(toml::Value::String(s)) = style.get_mut(key) {
                    *s = alias_color(s);
                }
            }
        }
        return toml::Value::Table(copy)
            .try_into::<Rule>()
            .map_err(|e| e.to_string());
    }
    let matcher = parse_matcher(t.get("match").ok_or("missing 'match'")?)?;
    let name = t
        .get("name")
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut rule = Rule::new(name, matcher);
    if let Some(v) = t.get("enabled") {
        rule.enabled = v.as_bool().ok_or("enabled must be true or false")?;
    }
    if let Some(v) = t.get("scope") {
        rule.scope = parse_scope(v.as_str().ok_or("scope must be a string")?)?;
    }
    if let Some(v) = t.get("style") {
        rule.style = parse_style(v)?;
    }
    if let Some(v) = t.get("priority") {
        let p = v.as_integer().ok_or("priority must be an integer")?;
        rule.priority = i32::try_from(p).map_err(|_| "priority out of range".to_string())?;
    }
    let flag = |key: &str| t.get(key).and_then(toml::Value::as_bool).unwrap_or(false);
    let nested = t.get("actions").and_then(toml::Value::as_table);
    let nflag = |key: &str| {
        nested
            .and_then(|a| a.get(key))
            .and_then(toml::Value::as_bool)
            .unwrap_or(false)
    };
    rule.actions = RuleActions {
        alert: flag("alert") || nflag("alert"),
        hide: flag("hide") || nflag("hide"),
        bookmark: flag("bookmark") || nflag("bookmark"),
    };
    if let Some(v) = t.get("minimap") {
        rule.minimap = Some(parse_color(v)?);
    }
    rule.gutter_marker = flag("gutter") || flag("gutter_marker");
    Ok(rule)
}

fn color_value(c: &ColorRef) -> toml::Value {
    toml::Value::String(c.to_string())
}

/// Converts a rule to the documented profile shape.
pub fn rule_to_table(r: &Rule) -> toml::Table {
    let mut t = toml::Table::new();
    if !r.name.is_empty() {
        t.insert("name".into(), toml::Value::String(r.name.clone()));
    }
    if !r.enabled {
        t.insert("enabled".into(), toml::Value::Boolean(false));
    }
    let mut m = toml::Table::new();
    match &r.matcher {
        RuleMatcher::Literal {
            text,
            case_sensitive,
        } => {
            m.insert("literal".into(), toml::Value::String(text.clone()));
            if *case_sensitive {
                m.insert("case_sensitive".into(), toml::Value::Boolean(true));
            }
        }
        RuleMatcher::Regex {
            pattern,
            case_sensitive,
        } => {
            m.insert("regex".into(), toml::Value::String(pattern.clone()));
            if !*case_sensitive {
                m.insert("case_sensitive".into(), toml::Value::Boolean(false));
            }
        }
        RuleMatcher::Column { column, op, value } => {
            m.insert("column".into(), toml::Value::String(column.clone()));
            if let Ok(toml::Value::String(op)) = toml::Value::try_from(op) {
                m.insert("op".into(), toml::Value::String(op));
            }
            m.insert("value".into(), toml::Value::String(value.clone()));
        }
    }
    t.insert("match".into(), toml::Value::Table(m));
    t.insert(
        "scope".into(),
        toml::Value::String(scope_to_string(&r.scope)),
    );
    let mut s = toml::Table::new();
    if let Some(c) = &r.style.fg {
        s.insert("fg".into(), color_value(c));
    }
    if let Some(c) = &r.style.bg {
        s.insert("bg".into(), color_value(c));
    }
    for (k, on) in [
        ("bold", r.style.bold),
        ("italic", r.style.italic),
        ("underline", r.style.underline),
        ("dim", r.style.dim),
    ] {
        if on {
            s.insert(k.into(), toml::Value::Boolean(true));
        }
    }
    if !s.is_empty() {
        t.insert("style".into(), toml::Value::Table(s));
    }
    if r.priority != 0 {
        t.insert(
            "priority".into(),
            toml::Value::Integer(i64::from(r.priority)),
        );
    }
    if r.actions.alert {
        t.insert("alert".into(), toml::Value::Boolean(true));
    }
    if r.actions.hide {
        t.insert("hide".into(), toml::Value::Boolean(true));
    }
    if r.actions.bookmark {
        t.insert("bookmark".into(), toml::Value::Boolean(true));
    }
    if let Some(c) = &r.minimap {
        t.insert("minimap".into(), color_value(c));
    }
    if r.gutter_marker {
        t.insert("gutter".into(), toml::Value::Boolean(true));
    }
    t
}

/// Builds a user profile from `rules`, keeping everything else (file
/// patterns, columns, timestamp settings, ...) from `base` when given.
pub fn profile_with_rules(name: &str, base: Option<&Profile>, rules: &[Rule]) -> Profile {
    let mut p = base.cloned().unwrap_or_default();
    p.name = name.to_string();
    p.rules = rules.iter().map(rule_to_table).collect();
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxtail_config::ProfileSet;
    use oxtail_highlight::{CompiledRules, SemanticColor};

    fn table(s: &str) -> toml::Table {
        s.parse().unwrap()
    }

    #[test]
    fn documented_regex_rule() {
        let r = rule_from_table(&table(
            r#"
            name = "slow"
            match = { regex = '\bduration=(\d{4,})ms' }
            scope = "group:1"
            style = { fg = "warn", bold = true }
            priority = 5
            alert = true
            "#,
        ))
        .unwrap();
        assert_eq!(r.name, "slow");
        assert_eq!(r.scope, Scope::Group(1));
        assert_eq!(r.priority, 5);
        assert!(r.style.bold);
        assert_eq!(r.style.fg, Some(ColorRef::solid(SemanticColor::Warn)));
        assert!(r.actions.alert && !r.actions.hide);
        assert!(matches!(
            r.matcher,
            RuleMatcher::Regex {
                case_sensitive: true,
                ..
            }
        ));
    }

    #[test]
    fn column_rule_and_literal_defaults() {
        let r = rule_from_table(&table(
            r#"match = { column = "level", op = "eq", value = "error" }
               scope = "line"
               style = { bg = "error.subtle" }
               minimap = "error"
               gutter = true"#,
        ))
        .unwrap();
        assert!(matches!(
            r.matcher,
            RuleMatcher::Column {
                op: ColumnOp::Eq,
                ..
            }
        ));
        assert_eq!(r.scope, Scope::Line);
        assert_eq!(r.minimap, Some(ColorRef::solid(SemanticColor::Error)));
        assert!(r.gutter_marker);
        let l = rule_from_table(&table(r#"match = { literal = "x" }"#)).unwrap();
        assert!(matches!(
            l.matcher,
            RuleMatcher::Literal {
                case_sensitive: false,
                ..
            }
        ));
        let n = rule_from_table(&table(
            r#"match = { column = "sc", op = "ge", value = 500 }"#,
        ))
        .unwrap();
        assert!(matches!(
            n.matcher,
            RuleMatcher::Column { ref value, op: ColumnOp::Ge, .. } if value == "500"
        ));
    }

    #[test]
    fn fatal_is_mapped_to_error() {
        let r = rule_from_table(&table(
            r#"match = { regex = "F" }
               style = { fg = "fatal", bg = "fatal.subtle" }"#,
        ))
        .unwrap();
        assert_eq!(r.style.fg, Some(ColorRef::solid(SemanticColor::Error)));
        assert_eq!(r.style.bg, Some(ColorRef::subtle(SemanticColor::Error)));
    }

    #[test]
    fn errors_are_reported_not_panics() {
        for bad in [
            r#"scope = "line""#,
            r#"match = { regex = 3 }"#,
            r#"match = { regex = "x" }
               scope = "banana""#,
            r#"match = { regex = "x" }
               style = { fg = "puce" }"#,
            r#"match = { column = "a", op = "wat", value = "1" }"#,
            r#"match = { column = "a" }"#,
            r#"match = { regex = "x" }
               priority = "high""#,
            r#"match = {}"#,
        ] {
            assert!(rule_from_table(&table(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn native_shape_is_accepted() {
        let r = rule_from_table(&table(
            r#"name = "n"
               matcher = { type = "literal", text = "warn" }
               style = { fg = "fatal" }"#,
        ))
        .unwrap();
        assert_eq!(r.name, "n");
        assert_eq!(r.style.fg, Some(ColorRef::solid(SemanticColor::Error)));
    }

    #[test]
    fn round_trip_through_the_documented_shape() {
        let rules = vec![
            Rule::regex("err", r"\bERROR\b")
                .scoped(Scope::Line)
                .styled(
                    Style::fg(ColorRef::solid(SemanticColor::Error))
                        .on(ColorRef::subtle(SemanticColor::Error))
                        .bold(),
                )
                .with_priority(40)
                .with_minimap(ColorRef::solid(SemanticColor::Error))
                .with_gutter()
                .with_actions(RuleActions {
                    alert: true,
                    hide: false,
                    bookmark: true,
                }),
            Rule::literal("lit", "hello").scoped(Scope::Group(0)),
            Rule::new(
                "col",
                RuleMatcher::Column {
                    column: "status".into(),
                    op: ColumnOp::Ge,
                    value: "500".into(),
                },
            )
            .scoped(Scope::Column("status".into())),
            {
                let mut r = Rule::regex("ci", "abc");
                r.matcher = RuleMatcher::Regex {
                    pattern: "abc".into(),
                    case_sensitive: false,
                };
                r.enabled = false;
                r
            },
            Rule::new(
                "lit-cs",
                RuleMatcher::Literal {
                    text: "X".into(),
                    case_sensitive: true,
                },
            ),
        ];
        for r in rules {
            let t = rule_to_table(&r);
            let back = rule_from_table(&t).unwrap_or_else(|e| panic!("{e}: {t:?}"));
            assert_eq!(back, r, "{t:?}");
        }
    }

    #[test]
    fn every_builtin_profile_converts_and_compiles() {
        let set = ProfileSet::builtin();
        let mut total = 0;
        for p in set.profiles() {
            let a = rules_from_profile(p);
            assert!(a.warnings.is_empty(), "{}: {:?}", p.name, a.warnings);
            assert_eq!(a.rules.len(), p.rules.len());
            total += a.rules.len();
            // Column rules compile too (they simply never match without columns).
            CompiledRules::compile(&a.rules)
                .unwrap_or_else(|e| panic!("profile {} does not compile: {e}", p.name));
        }
        assert!(total > 10);
    }

    #[test]
    fn profile_with_rules_keeps_the_base() {
        let set = ProfileSet::builtin();
        let base = set.by_name("logfmt").unwrap();
        let rules = vec![Rule::literal("x", "y")];
        let p = profile_with_rules("mine", Some(base), &rules);
        assert_eq!(p.name, "mine");
        assert_eq!(p.columns, base.columns);
        assert_eq!(p.rules.len(), 1);
        let text = p.to_toml_string().unwrap();
        let back = Profile::from_toml_str(&text).unwrap();
        assert_eq!(rules_from_profile(&back).rules, rules);
    }
}
