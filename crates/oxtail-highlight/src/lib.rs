//! Highlight rule engine and ANSI parsing for OxTail (PLAN.md section 7).
//!
//! * [`Rule`] / [`RuleMatcher`] / [`Scope`]: serialisable rule definitions.
//! * [`CompiledRules`]: compiles all rules into one `aho-corasick` automaton
//!   plus one `RegexSet` and turns a line into a [`LineHighlight`]: sorted,
//!   non-overlapping [`StyledSpan`]s layered by priority, plus line style,
//!   actions, minimap and gutter info.
//! * [`ColorRef`] / [`Palette`]: semantic colours resolved per theme, with a
//!   WCAG [`contrast_ratio`] helper used to validate the palettes.
//! * [`ansi::parse`]: strips SGR escapes and returns spans.
//! * [`presets`]: ready-made rule sets.
//!
//! All offsets are byte offsets into the line text, aligned to `char`
//! boundaries. Nothing here reads files or touches the UI.

#![forbid(unsafe_code)]

pub mod ansi;
mod color;
mod engine;
pub mod presets;
mod rule;
mod style;

pub use color::{ColorParseError, ColorRef, Palette, Rgb, SemanticColor, contrast_ratio};
pub use engine::{
    ColumnRanges, CompiledRules, HighlightError, LineHighlight, MAX_LINE_BYTES, parse_number,
};
pub use rule::{ColumnOp, Rule, RuleActions, RuleMatcher, Scope};
pub use style::{Style, StyledSpan};
