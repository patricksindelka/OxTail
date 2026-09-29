//! Themes: `themes/*.toml` and the built-in dark, light and high-contrast
//! themes.
//!
//! A theme maps *semantic* colour names (`error`, `warn`, `error.subtle`,
//! `accent1`, ...) used by highlight rules to concrete `#rrggbb` colours, and
//! carries the colours of the UI chrome.

use std::{collections::BTreeMap, fmt, fs, path::Path};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{ConfigError, Loaded, write_atomic};

/// An sRGB colour, written `#rrggbb` in files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Parses `#rrggbb` or `rrggbb` (also the short `#rgb`).
    pub fn parse(s: &str) -> Option<Rgb> {
        let h = s.trim().strip_prefix('#').unwrap_or(s.trim());
        if !h.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
        match h.len() {
            6 => Some(Rgb(byte(0)?, byte(2)?, byte(4)?)),
            3 => {
                let n = |i: usize| u8::from_str_radix(&h[i..=i], 16).ok().map(|v| v * 17);
                Some(Rgb(n(0)?, n(1)?, n(2)?))
            }
            _ => None,
        }
    }

    /// Formats as `#rrggbb`.
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Rgb::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("invalid colour '{s}'")))
    }
}

/// Colours of the application chrome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiColors {
    /// Text area background.
    pub background: Rgb,
    /// Default text colour.
    pub foreground: Rgb,
    /// Line-number gutter background.
    pub gutter_background: Rgb,
    /// Line-number gutter text.
    pub gutter_foreground: Rgb,
    /// Selection background.
    pub selection_background: Rgb,
    /// Background of the current/hovered line.
    pub current_line_background: Rgb,
    /// Background of search matches.
    pub search_match_background: Rgb,
    /// Borders and separators.
    pub border: Rgb,
    /// Accent (focus, links, active tab).
    pub accent: Rgb,
    /// Status bar background.
    pub status_bar_background: Rgb,
    /// Status bar text.
    pub status_bar_foreground: Rgb,
}

impl Default for UiColors {
    /// The dark theme's colours.
    fn default() -> Self {
        let c = |h: &str| Rgb::parse(h).unwrap_or(Rgb(0, 0, 0));
        Self {
            background: c("#1e1f22"),
            foreground: c("#d4d4d4"),
            gutter_background: c("#25262a"),
            gutter_foreground: c("#6b6f76"),
            selection_background: c("#264f78"),
            current_line_background: c("#2a2d33"),
            search_match_background: c("#5c4a00"),
            border: c("#3a3d42"),
            accent: c("#4d9de0"),
            status_bar_background: c("#2b2d31"),
            status_bar_foreground: c("#b8bcc4"),
        }
    }
}

/// A colour theme.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    /// Theme name (`dark`, `light`, `high-contrast` or custom).
    pub name: String,
    /// Whether this is a dark theme (selects the base egui visuals).
    pub dark: bool,
    /// UI chrome colours.
    pub ui: UiColors,
    /// Semantic colour names used by highlight rules.
    pub semantic: BTreeMap<String, Rgb>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: String::new(),
            dark: true,
            ui: UiColors::default(),
            semantic: BTreeMap::new(),
        }
    }
}

impl Theme {
    /// Parses a theme file.
    pub fn from_toml_str(text: &str) -> Result<Theme, toml::de::Error> {
        toml::from_str(text)
    }

    /// Resolves a colour reference from a rule: a semantic name in this theme,
    /// or a literal `#rrggbb`.
    pub fn resolve(&self, name: &str) -> Option<Rgb> {
        self.semantic
            .get(name)
            .copied()
            .or_else(|| name.starts_with('#').then(|| Rgb::parse(name)).flatten())
    }

    /// Saves atomically as `<dir>/<name>.toml`.
    pub fn save_to_dir(&self, dir: &Path) -> Result<std::path::PathBuf, ConfigError> {
        let stem: String = self
            .name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = dir.join(format!(
            "{}.toml",
            if stem.is_empty() { "theme" } else { &stem }
        ));
        let text = toml::to_string_pretty(self)?;
        write_atomic(&path, text.as_bytes()).map_err(|e| ConfigError::io(&path, e))?;
        Ok(path)
    }
}

const BUILTIN: &[&str] = &[
    include_str!("../themes/dark.toml"),
    include_str!("../themes/light.toml"),
    include_str!("../themes/high-contrast.toml"),
];

/// The available themes: built-ins plus the ones in `themes/`.
pub struct ThemeSet {
    themes: Vec<Theme>,
}

impl ThemeSet {
    /// Only the built-in themes.
    pub fn builtin() -> ThemeSet {
        let themes = BUILTIN
            .iter()
            .filter_map(|t| match Theme::from_toml_str(t) {
                Ok(t) => Some(t),
                Err(e) => {
                    // A broken built-in is a bug, caught by the unit tests.
                    tracing::error!("built-in theme is invalid: {e}");
                    None
                }
            })
            .collect();
        ThemeSet { themes }
    }

    /// Built-ins plus `dir/*.toml`. A user theme replaces a built-in of the
    /// same name. Invalid files are skipped with a warning.
    pub fn load_dir(dir: &Path) -> Loaded<ThemeSet> {
        let mut set = Self::builtin();
        let mut warnings = Vec::new();
        let mut files: Vec<_> = fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.extension()
                            .is_some_and(|x| x.eq_ignore_ascii_case("toml"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        for f in files {
            let text = match fs::read(&f) {
                Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                Err(e) => {
                    warnings.push(format!("{}: {e}", f.display()));
                    continue;
                }
            };
            match Theme::from_toml_str(&text) {
                Ok(t) if !t.name.trim().is_empty() => {
                    set.themes.retain(|x| x.name != t.name);
                    set.themes.push(t);
                }
                Ok(_) => warnings.push(format!("{}: theme has no name", f.display())),
                Err(e) => warnings.push(format!("{}: {e}", f.display())),
            }
        }
        Loaded {
            value: set,
            warnings,
        }
    }

    /// All themes.
    pub fn themes(&self) -> &[Theme] {
        &self.themes
    }

    /// A theme by name (ASCII case-insensitive).
    pub fn get(&self, name: &str) -> Option<&Theme> {
        self.themes
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_parse_and_format() {
        assert_eq!(Rgb::parse("#1e1f22"), Some(Rgb(0x1e, 0x1f, 0x22)));
        assert_eq!(Rgb::parse("FFFFFF"), Some(Rgb(255, 255, 255)));
        assert_eq!(Rgb::parse("#abc"), Some(Rgb(0xaa, 0xbb, 0xcc)));
        for bad in ["", "#", "#12345", "#gggggg", "#1234567", "é12345", "#ééé"] {
            assert_eq!(Rgb::parse(bad), None, "{bad}");
        }
        assert_eq!(Rgb(1, 2, 255).to_hex(), "#0102ff");
    }

    #[test]
    fn builtins_are_complete() {
        let set = ThemeSet::builtin();
        assert_eq!(set.themes().len(), 3);
        for name in ["dark", "light", "high-contrast"] {
            let t = set.get(name).unwrap_or_else(|| panic!("{name}"));
            for key in [
                "fatal",
                "error",
                "error.subtle",
                "warn",
                "warn.subtle",
                "info",
                "success",
                "debug",
                "muted",
                "accent1",
                "accent2",
                "accent3",
                "accent4",
            ] {
                assert!(t.resolve(key).is_some(), "{name} lacks {key}");
            }
        }
        assert!(!set.get("light").expect("light").dark);
        assert_eq!(set.get("DARK").expect("dark").name, "dark");
    }

    #[test]
    fn every_profile_colour_resolves_in_every_theme() {
        let profiles = crate::ProfileSet::builtin();
        let themes = ThemeSet::builtin();
        for prof in profiles.profiles() {
            for rule in &prof.rules {
                let Some(style) = rule.get("style").and_then(toml::Value::as_table) else {
                    continue;
                };
                for key in ["fg", "bg", "minimap"] {
                    if let Some(c) = style.get(key).and_then(toml::Value::as_str) {
                        for t in themes.themes() {
                            assert!(t.resolve(c).is_some(), "{} / {c} in {}", prof.name, t.name);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn resolve_literal_and_round_trip() {
        let t = ThemeSet::builtin().get("dark").expect("dark").clone();
        assert_eq!(t.resolve("#ff0000"), Some(Rgb(255, 0, 0)));
        assert_eq!(t.resolve("nope"), None);
        let back = Theme::from_toml_str(&toml::to_string_pretty(&t).expect("ser")).expect("de");
        assert_eq!(back, t);
    }

    #[test]
    fn load_dir_overrides_and_skips_bad() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("mine.toml"),
            "name = \"dark\"\n[semantic]\nerror = \"#010203\"\n",
        )
        .expect("w");
        fs::write(
            dir.path().join("bad.toml"),
            "name = \"x\"\n[semantic]\nerror = \"red\"\n",
        )
        .expect("w");
        fs::write(dir.path().join("junk.toml"), [0xff, 0x00]).expect("w");
        let l = ThemeSet::load_dir(dir.path());
        assert_eq!(l.warnings.len(), 2, "{:?}", l.warnings);
        assert_eq!(l.value.themes().len(), 3);
        assert_eq!(
            l.value.get("dark").expect("dark").resolve("error"),
            Some(Rgb(1, 2, 3))
        );
    }
}
