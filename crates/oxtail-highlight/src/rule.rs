//! Highlight rule definitions (serialisable; stored in TOML profiles).

use serde::{Deserialize, Serialize};

use crate::color::ColorRef;
use crate::style::Style;

/// How a column value is compared in a [`RuleMatcher::Column`] rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnOp {
    /// Equal (numerically if both sides are numbers, else text, ASCII
    /// case-insensitive).
    Eq,
    /// Not equal (the negation of `Eq`).
    Ne,
    /// The column text contains the value (ASCII case-insensitive).
    Contains,
    /// The column text matches the value as a regex.
    Regex,
    /// Numeric greater than.
    Gt,
    /// Numeric greater than or equal.
    Ge,
    /// Numeric less than.
    Lt,
    /// Numeric less than or equal.
    Le,
}

/// What a rule tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuleMatcher {
    /// A plain-text substring.
    Literal {
        /// The text to find (must not be empty).
        text: String,
        /// Match case exactly (default: ignore case).
        #[serde(default)]
        case_sensitive: bool,
    },
    /// A regular expression (Rust `regex` syntax) searched in the line.
    Regex {
        /// The pattern.
        pattern: String,
        /// Match case exactly (default: ignore case).
        #[serde(default)]
        case_sensitive: bool,
    },
    /// A condition on a parsed column. Needs the `columns` argument of
    /// [`CompiledRules::highlight`](crate::CompiledRules::highlight); without
    /// it the rule never matches.
    Column {
        /// Column name (compared ASCII case-insensitively).
        column: String,
        /// The comparison.
        op: ColumnOp,
        /// The value to compare with.
        value: String,
    },
}

/// Which part of the line a matching rule styles.
///
/// For [`RuleMatcher::Column`] rules, `Match` and `Group(_)` both mean "the
/// tested column".
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// The whole line (as a background layer under the spans).
    Line,
    /// Each match.
    #[default]
    Match,
    /// One capture group of each match (0 is the whole match).
    Group(usize),
    /// A whole named column; needs column info.
    Column(String),
}

/// Side effects of a matching rule beyond styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleActions {
    /// Raise an alert (notification, sound) for new matching lines.
    pub alert: bool,
    /// Hide (fold) matching lines.
    pub hide: bool,
    /// Bookmark matching lines automatically.
    pub bookmark: bool,
}

fn yes() -> bool {
    true
}

/// One highlight rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Disabled rules are ignored.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// What to test.
    pub matcher: RuleMatcher,
    /// What to style.
    #[serde(default)]
    pub scope: Scope,
    /// The style to apply.
    #[serde(default)]
    pub style: Style,
    /// Higher priority wins conflicts (ties: the later rule wins).
    #[serde(default)]
    pub priority: i32,
    /// Colour of this rule's tick in the minimap, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimap: Option<ColorRef>,
    /// Show a marker in the gutter for matching lines.
    #[serde(default)]
    pub gutter_marker: bool,
    /// Side effects.
    #[serde(default)]
    pub actions: RuleActions,
}

impl Rule {
    /// A rule with default settings around `matcher`.
    pub fn new(name: impl Into<String>, matcher: RuleMatcher) -> Rule {
        Rule {
            name: name.into(),
            enabled: true,
            matcher,
            scope: Scope::default(),
            style: Style::default(),
            priority: 0,
            minimap: None,
            gutter_marker: false,
            actions: RuleActions::default(),
        }
    }

    /// A case-insensitive literal rule.
    pub fn literal(name: impl Into<String>, text: impl Into<String>) -> Rule {
        Rule::new(
            name,
            RuleMatcher::Literal {
                text: text.into(),
                case_sensitive: false,
            },
        )
    }

    /// A case-sensitive regex rule.
    pub fn regex(name: impl Into<String>, pattern: impl Into<String>) -> Rule {
        Rule::new(
            name,
            RuleMatcher::Regex {
                pattern: pattern.into(),
                case_sensitive: true,
            },
        )
    }

    /// Sets the style.
    #[must_use]
    pub fn styled(mut self, style: Style) -> Rule {
        self.style = style;
        self
    }

    /// Sets the scope.
    #[must_use]
    pub fn scoped(mut self, scope: Scope) -> Rule {
        self.scope = scope;
        self
    }

    /// Sets the priority.
    #[must_use]
    pub fn with_priority(mut self, priority: i32) -> Rule {
        self.priority = priority;
        self
    }

    /// Sets the minimap colour.
    #[must_use]
    pub fn with_minimap(mut self, color: ColorRef) -> Rule {
        self.minimap = Some(color);
        self
    }

    /// Turns on the gutter marker.
    #[must_use]
    pub fn with_gutter(mut self) -> Rule {
        self.gutter_marker = true;
        self
    }

    /// Sets the actions.
    #[must_use]
    pub fn with_actions(mut self, actions: RuleActions) -> Rule {
        self.actions = actions;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::SemanticColor;

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct File {
        rules: Vec<Rule>,
    }

    #[test]
    fn toml_round_trip() {
        let rules = vec![
            Rule::literal("err", "ERROR")
                .styled(Style::fg(ColorRef::solid(SemanticColor::Error)).bold())
                .with_priority(5)
                .with_minimap(ColorRef::solid(SemanticColor::Error))
                .with_gutter()
                .with_actions(RuleActions {
                    alert: true,
                    ..Default::default()
                }),
            Rule::regex("num", r"\d+(\.\d+)?")
                .scoped(Scope::Group(1))
                .styled(Style::fg(ColorRef::rgb(1, 2, 3))),
            Rule::new(
                "slow",
                RuleMatcher::Column {
                    column: "duration".into(),
                    op: ColumnOp::Gt,
                    value: "1000".into(),
                },
            )
            .scoped(Scope::Column("duration".into())),
            Rule::regex("line", "x").scoped(Scope::Line),
        ];
        let file = File { rules };
        let text = toml::to_string(&file).unwrap();
        let back: File = toml::from_str(&text).unwrap();
        assert_eq!(back, file, "{text}");
    }

    #[test]
    fn minimal_toml_uses_defaults() {
        let f: File = toml::from_str(
            r##"
            [[rules]]
            matcher = { type = "literal", text = "warn" }
            style = { fg = "warn", bg = "#102030" }
            "##,
        )
        .unwrap();
        let r = &f.rules[0];
        assert!(r.enabled);
        assert_eq!(r.scope, Scope::Match);
        assert_eq!(r.priority, 0);
        assert_eq!(r.style.bg, Some(ColorRef::rgb(0x10, 0x20, 0x30)));
        assert!(matches!(
            &r.matcher,
            RuleMatcher::Literal {
                case_sensitive: false,
                ..
            }
        ));
    }

    #[test]
    fn bad_colour_is_a_deserialisation_error() {
        let e = toml::from_str::<File>(
            r#"
            [[rules]]
            matcher = { type = "literal", text = "x" }
            style = { fg = "puce" }
            "#,
        );
        assert!(e.is_err());
    }
}
