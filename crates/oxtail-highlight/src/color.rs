//! Colours: semantic names, RGB, palettes and WCAG contrast.
//!
//! Rules refer to *semantic* colours (`error`, `warn`, `accent3`, ...) so the
//! same rule set looks right on every theme. A [`Palette`] resolves them to
//! RGB. Each semantic colour also has a `*.subtle` variant meant for
//! backgrounds: the colour mixed into the palette background so ordinary text
//! stays readable on top of it.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// An sRGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
}

impl Rgb {
    /// Creates a colour from its components.
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parses `#rrggbb` (the leading `#` is required).
    pub fn from_hex(s: &str) -> Option<Rgb> {
        let h = s.strip_prefix('#')?;
        if h.len() != 6 || !h.is_ascii() {
            return None;
        }
        let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
        Some(Rgb::new(p(0)?, p(2)?, p(4)?))
    }

    /// Formats as `#rrggbb`.
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// WCAG relative luminance in `0.0..=1.0`.
    pub fn relative_luminance(self) -> f64 {
        fn lin(c: u8) -> f64 {
            let c = f64::from(c) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * lin(self.r) + 0.7152 * lin(self.g) + 0.0722 * lin(self.b)
    }

    /// Linear-in-sRGB blend: `t = 0` is `self`, `t = 1` is `other`.
    pub fn mix(self, other: Rgb, t: f64) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let m = |a: u8, b: u8| (f64::from(a) + (f64::from(b) - f64::from(a)) * t).round() as u8;
        Rgb::new(m(self.r, other.r), m(self.g, other.g), m(self.b, other.b))
    }
}

/// WCAG 2.x contrast ratio between two colours, from 1.0 to 21.0.
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (a.relative_luminance(), b.relative_luminance());
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// The named colours rules can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SemanticColor {
    /// Errors and failures.
    Error,
    /// Warnings.
    Warn,
    /// Informational messages.
    Info,
    /// Debug messages.
    Debug,
    /// Trace messages.
    Trace,
    /// Success.
    Success,
    /// General purpose accent 1.
    Accent1,
    /// General purpose accent 2.
    Accent2,
    /// General purpose accent 3.
    Accent3,
    /// General purpose accent 4.
    Accent4,
    /// General purpose accent 5.
    Accent5,
    /// General purpose accent 6.
    Accent6,
    /// General purpose accent 7.
    Accent7,
    /// General purpose accent 8.
    Accent8,
    /// De-emphasised text.
    Muted,
}

impl SemanticColor {
    /// Every semantic colour, in palette order.
    pub const ALL: [SemanticColor; 15] = [
        SemanticColor::Error,
        SemanticColor::Warn,
        SemanticColor::Info,
        SemanticColor::Debug,
        SemanticColor::Trace,
        SemanticColor::Success,
        SemanticColor::Accent1,
        SemanticColor::Accent2,
        SemanticColor::Accent3,
        SemanticColor::Accent4,
        SemanticColor::Accent5,
        SemanticColor::Accent6,
        SemanticColor::Accent7,
        SemanticColor::Accent8,
        SemanticColor::Muted,
    ];

    /// The name used in config files.
    pub fn name(self) -> &'static str {
        match self {
            SemanticColor::Error => "error",
            SemanticColor::Warn => "warn",
            SemanticColor::Info => "info",
            SemanticColor::Debug => "debug",
            SemanticColor::Trace => "trace",
            SemanticColor::Success => "success",
            SemanticColor::Accent1 => "accent1",
            SemanticColor::Accent2 => "accent2",
            SemanticColor::Accent3 => "accent3",
            SemanticColor::Accent4 => "accent4",
            SemanticColor::Accent5 => "accent5",
            SemanticColor::Accent6 => "accent6",
            SemanticColor::Accent7 => "accent7",
            SemanticColor::Accent8 => "accent8",
            SemanticColor::Muted => "muted",
        }
    }

    fn from_name(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.name() == s)
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|&c| c == self).unwrap_or(0)
    }
}

/// A colour as written in a rule: a semantic name or a fixed RGB value.
///
/// In TOML this is a string: `"error"`, `"warn.subtle"` or `"#ff8800"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum ColorRef {
    /// A palette colour; `subtle` selects the background-friendly variant.
    Semantic {
        /// Which colour.
        color: SemanticColor,
        /// The `*.subtle` variant.
        subtle: bool,
    },
    /// A fixed colour.
    Rgb(Rgb),
}

impl ColorRef {
    /// The solid semantic colour.
    pub const fn solid(color: SemanticColor) -> Self {
        ColorRef::Semantic {
            color,
            subtle: false,
        }
    }

    /// The `*.subtle` background variant of a semantic colour.
    pub const fn subtle(color: SemanticColor) -> Self {
        ColorRef::Semantic {
            color,
            subtle: true,
        }
    }

    /// A fixed RGB colour.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        ColorRef::Rgb(Rgb::new(r, g, b))
    }
}

/// Why a colour string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown colour {0:?}: use a name like \"error\" or \"warn.subtle\", or \"#rrggbb\"")]
pub struct ColorParseError(pub String);

impl FromStr for ColorRef {
    type Err = ColorParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        if t.starts_with('#') {
            return Rgb::from_hex(t)
                .map(ColorRef::Rgb)
                .ok_or_else(|| ColorParseError(s.to_owned()));
        }
        let lower = t.to_ascii_lowercase();
        let (name, subtle) = match lower.strip_suffix(".subtle") {
            Some(n) => (n, true),
            None => (lower.as_str(), false),
        };
        SemanticColor::from_name(name)
            .map(|color| ColorRef::Semantic { color, subtle })
            .ok_or_else(|| ColorParseError(s.to_owned()))
    }
}

impl fmt::Display for ColorRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ColorRef::Semantic {
                color,
                subtle: false,
            } => f.write_str(color.name()),
            ColorRef::Semantic {
                color,
                subtle: true,
            } => write!(f, "{}.subtle", color.name()),
            ColorRef::Rgb(c) => f.write_str(&c.to_hex()),
        }
    }
}

impl TryFrom<String> for ColorRef {
    type Error = ColorParseError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<ColorRef> for String {
    fn from(c: ColorRef) -> String {
        c.to_string()
    }
}

/// A theme's colours: resolves [`ColorRef`] to RGB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    /// Human-readable name.
    pub name: &'static str,
    /// The view background.
    pub background: Rgb,
    /// The default text colour.
    pub text: Rgb,
    solid: [Rgb; 15],
    subtle: [Rgb; 15],
}

/// How much of the hue is mixed into the background for `*.subtle`.
const SUBTLE_MIX: f64 = 0.16;

fn hex(s: &str) -> Rgb {
    // Only called with literals in this file (checked by tests).
    Rgb::from_hex(s).unwrap_or(Rgb::new(255, 0, 255))
}

impl Palette {
    fn build(
        name: &'static str,
        background: &str,
        text: &str,
        solid: [&str; 15],
        mix: f64,
    ) -> Palette {
        let background = hex(background);
        let solid = solid.map(hex);
        Palette {
            name,
            background,
            text: hex(text),
            solid,
            subtle: solid.map(|c| background.mix(c, mix)),
        }
    }

    /// The dark theme palette.
    pub fn dark() -> Palette {
        Self::build(
            "dark",
            "#1e1e1e",
            "#d4d4d4",
            [
                "#ff6b6b", "#e5b84b", "#5cb3ff", "#8fc27a", "#9aa0a6", "#57d38c", "#c792ea",
                "#4dd0e1", "#ffab70", "#f78fb3", "#82aaff", "#c3e88d", "#ffd166", "#b0bec5",
                "#979fa8",
            ],
            SUBTLE_MIX,
        )
    }

    /// The light theme palette.
    pub fn light() -> Palette {
        Self::build(
            "light",
            "#ffffff",
            "#1f2328",
            [
                "#b71c1c", "#8a5a00", "#0b5cb5", "#2e6b1f", "#57606a", "#116329", "#7b2fbf",
                "#0a6c7a", "#a3400a", "#b0206a", "#3949ab", "#4d6b00", "#7a5c00", "#455a64",
                "#5b636b",
            ],
            SUBTLE_MIX,
        )
    }

    /// The high-contrast palette (black background).
    pub fn high_contrast() -> Palette {
        Self::build(
            "high-contrast",
            "#000000",
            "#ffffff",
            [
                "#ff5c5c", "#ffd60a", "#4db8ff", "#7dff7d", "#c0c0c0", "#39ff88", "#e0a3ff",
                "#3ff2ff", "#ffb060", "#ff8cc6", "#9fb8ff", "#d4ff70", "#fff36b", "#d0d8e0",
                "#b8b8b8",
            ],
            0.22,
        )
    }

    /// The solid colour for a semantic name.
    pub fn solid(&self, c: SemanticColor) -> Rgb {
        self.solid[c.index()]
    }

    /// The `*.subtle` background colour for a semantic name.
    pub fn subtle(&self, c: SemanticColor) -> Rgb {
        self.subtle[c.index()]
    }

    /// Resolves any colour reference to RGB.
    pub fn resolve(&self, c: &ColorRef) -> Rgb {
        match *c {
            ColorRef::Rgb(rgb) => rgb,
            ColorRef::Semantic {
                color,
                subtle: false,
            } => self.solid(color),
            ColorRef::Semantic {
                color,
                subtle: true,
            } => self.subtle(color),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palettes() -> [Palette; 3] {
        [Palette::dark(), Palette::light(), Palette::high_contrast()]
    }

    #[test]
    fn palette_hex_literals_are_valid() {
        for p in palettes() {
            for c in SemanticColor::ALL {
                assert_ne!(p.solid(c), Rgb::new(255, 0, 255), "{} {c:?}", p.name);
            }
        }
    }

    #[test]
    fn contrast_extremes() {
        let r = contrast_ratio(Rgb::new(0, 0, 0), Rgb::new(255, 255, 255));
        assert!((r - 21.0).abs() < 1e-9);
        assert!((contrast_ratio(Rgb::new(9, 9, 9), Rgb::new(9, 9, 9)) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn every_semantic_foreground_is_readable_on_its_background() {
        for p in palettes() {
            assert!(contrast_ratio(p.text, p.background) >= 7.0, "{}", p.name);
            for c in SemanticColor::ALL {
                let ratio = contrast_ratio(p.solid(c), p.background);
                assert!(ratio >= 4.5, "{} {c:?}: {ratio:.2}", p.name);
            }
        }
    }

    #[test]
    fn text_and_semantic_colours_stay_readable_on_subtle_backgrounds() {
        for p in palettes() {
            for c in SemanticColor::ALL {
                let sub = p.subtle(c);
                let plain = contrast_ratio(p.text, sub);
                assert!(plain >= 4.5, "{} text on {c:?}.subtle: {plain:.2}", p.name);
                let own = contrast_ratio(p.solid(c), sub);
                assert!(own >= 4.5, "{} {c:?} on its subtle: {own:.2}", p.name);
            }
        }
    }

    #[test]
    fn colour_strings_round_trip() {
        for s in ["error", "warn.subtle", "accent8", "muted", "#a1b2c3"] {
            let c: ColorRef = s.parse().unwrap();
            assert_eq!(c.to_string(), s);
        }
        assert_eq!(
            "ERROR".parse::<ColorRef>().unwrap(),
            ColorRef::solid(SemanticColor::Error)
        );
        assert!("nope".parse::<ColorRef>().is_err());
        assert!("#12345".parse::<ColorRef>().is_err());
        assert!("#gggggg".parse::<ColorRef>().is_err());
        assert!("accent9".parse::<ColorRef>().is_err());
    }

    #[test]
    fn resolve_uses_palette() {
        let p = Palette::dark();
        assert_eq!(p.resolve(&ColorRef::rgb(1, 2, 3)), Rgb::new(1, 2, 3));
        assert_eq!(
            p.resolve(&ColorRef::solid(SemanticColor::Error)),
            p.solid(SemanticColor::Error)
        );
        assert_eq!(
            p.resolve(&ColorRef::subtle(SemanticColor::Warn)),
            p.subtle(SemanticColor::Warn)
        );
    }
}
