//! Text styles and how they layer.

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::color::ColorRef;

/// A text style. Unset fields (`None` / `false`) inherit from lower layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    /// Foreground colour.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg: Option<ColorRef>,
    /// Background colour.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<ColorRef>,
    /// Bold.
    #[serde(skip_serializing_if = "is_false")]
    pub bold: bool,
    /// Italic.
    #[serde(skip_serializing_if = "is_false")]
    pub italic: bool,
    /// Underline.
    #[serde(skip_serializing_if = "is_false")]
    pub underline: bool,
    /// Dimmed (reduced emphasis).
    #[serde(skip_serializing_if = "is_false")]
    pub dim: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Style {
    /// Whether nothing is set.
    pub fn is_empty(&self) -> bool {
        *self == Style::default()
    }

    /// Puts `over` on top of `self`: every field `over` sets wins, the rest
    /// falls through from `self`. Flags are additive.
    #[must_use]
    pub fn layer(&self, over: &Style) -> Style {
        Style {
            fg: over.fg.or(self.fg),
            bg: over.bg.or(self.bg),
            bold: over.bold || self.bold,
            italic: over.italic || self.italic,
            underline: over.underline || self.underline,
            dim: over.dim || self.dim,
        }
    }

    /// A style with only a foreground.
    pub fn fg(color: ColorRef) -> Style {
        Style {
            fg: Some(color),
            ..Style::default()
        }
    }

    /// Sets the background and returns the style.
    #[must_use]
    pub fn on(mut self, bg: ColorRef) -> Style {
        self.bg = Some(bg);
        self
    }

    /// Makes the style bold.
    #[must_use]
    pub fn bold(mut self) -> Style {
        self.bold = true;
        self
    }

    /// Makes the style italic.
    #[must_use]
    pub fn italic(mut self) -> Style {
        self.italic = true;
        self
    }

    /// Makes the style underlined.
    #[must_use]
    pub fn underline(mut self) -> Style {
        self.underline = true;
        self
    }

    /// Makes the style dim.
    #[must_use]
    pub fn dim(mut self) -> Style {
        self.dim = true;
        self
    }
}

/// A style applied to a byte range of a line's text.
///
/// Spans returned by this crate are sorted, non-overlapping, in bounds and
/// aligned to `char` boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledSpan {
    /// Byte range in the text the span refers to.
    pub range: Range<usize>,
    /// The style.
    pub style: Style,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::SemanticColor;

    #[test]
    fn layering_prefers_the_top_and_inherits_the_rest() {
        let under = Style::fg(ColorRef::solid(SemanticColor::Info))
            .on(ColorRef::subtle(SemanticColor::Warn))
            .bold();
        let over = Style::fg(ColorRef::solid(SemanticColor::Error)).italic();
        let l = under.layer(&over);
        assert_eq!(l.fg, Some(ColorRef::solid(SemanticColor::Error)));
        assert_eq!(l.bg, Some(ColorRef::subtle(SemanticColor::Warn)));
        assert!(l.bold && l.italic && !l.underline);
        assert_eq!(under.layer(&Style::default()), under);
        assert!(Style::default().is_empty());
    }

    #[test]
    fn serde_skips_defaults() {
        let s = Style::fg(ColorRef::solid(SemanticColor::Error)).bold();
        let t = toml::to_string(&s).unwrap();
        assert_eq!(t.trim(), "fg = \"error\"\nbold = true");
        let back: Style = toml::from_str(&t).unwrap();
        assert_eq!(back, s);
        let empty: Style = toml::from_str("").unwrap();
        assert!(empty.is_empty());
    }
}
