//! `settings.toml`: user preferences with schema versioning.

use std::{collections::BTreeMap, fs, path::Path};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{ConfigError, DataMode, write_atomic};

/// Current settings schema. Bump when a key is renamed or changes meaning and
/// add a step to [`migrate`].
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Colour theme selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeChoice {
    /// Follow the operating system's dark/light setting.
    #[default]
    System,
    /// Built-in dark theme.
    Dark,
    /// Built-in light theme.
    Light,
    /// Built-in high-contrast theme.
    HighContrast,
    /// The theme named by [`Settings::custom_theme`].
    Custom,
}

/// Key binding preset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Keymap {
    /// Standard desktop shortcuts.
    #[default]
    Standard,
    /// `less`-style keys.
    Less,
}

/// Renderer selection (`--renderer`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Renderer {
    /// Fallback chain: wgpu, then OpenGL, then software.
    #[default]
    Auto,
    /// Force wgpu.
    Wgpu,
    /// Force OpenGL (glow).
    Glow,
}

/// Time zone used for zone-less timestamps and for display.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TimezoneSetting {
    /// The system's local zone.
    #[default]
    Local,
    /// UTC.
    Utc,
    /// An IANA name such as `Europe/Amsterdam` (validated by the consumer).
    Named(String),
}

impl Serialize for TimezoneSetting {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Local => s.serialize_str("local"),
            Self::Utc => s.serialize_str("UTC"),
            Self::Named(n) => s.serialize_str(n),
        }
    }
}

impl<'de> Deserialize<'de> for TimezoneSetting {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let t = s.trim();
        Ok(if t.is_empty() || t.eq_ignore_ascii_case("local") {
            Self::Local
        } else if t.eq_ignore_ascii_case("utc") || t == "Z" {
            Self::Utc
        } else {
            Self::Named(t.to_string())
        })
    }
}

/// All user settings. Every field has a default, so partial and old files
/// load fine; unknown keys are ignored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Schema version of the file (see [`migrate`]).
    pub schema_version: u32,
    /// Theme choice.
    pub theme: ThemeChoice,
    /// Name of the custom theme when `theme` is [`ThemeChoice::Custom`].
    pub custom_theme: String,
    /// Font size in points.
    pub font_size: f32,
    /// Line height as a multiple of the font size.
    pub line_height: f32,
    /// Font family; empty means the built-in monospace font.
    pub font_family: String,
    /// Wrap long lines.
    pub wrap: bool,
    /// Show line numbers.
    pub line_numbers: bool,
    /// Lines longer than this many characters are cut for display.
    pub max_display_line_length: usize,
    /// Poll interval for followed files, in milliseconds (fallback when file
    /// system notifications are unavailable, e.g. on network shares).
    pub follow_poll_interval_ms: u64,
    /// Block cache size in megabytes.
    pub cache_size_mb: u32,
    /// Key binding preset.
    pub keymap: Keymap,
    /// Custom key bindings: action name to key chord, overriding the preset.
    pub custom_keybindings: BTreeMap<String, String>,
    /// Default text encoding label (`auto` detects).
    pub default_encoding: String,
    /// Time zone for zone-less timestamps.
    pub timezone: TimezoneSetting,
    /// Show a separator between lines further apart than this many seconds
    /// (`0` disables).
    pub time_gap_threshold_secs: f64,
    /// Desktop notifications for alert rules.
    pub notifications_enabled: bool,
    /// Check for updates. Off by default in portable mode.
    pub update_check: bool,
    /// Renderer.
    pub renderer: Renderer,
    /// How many recent files to remember.
    pub recent_files_limit: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            theme: ThemeChoice::System,
            custom_theme: String::new(),
            font_size: 13.0,
            line_height: 1.25,
            font_family: String::new(),
            wrap: false,
            line_numbers: true,
            max_display_line_length: 10_000,
            follow_poll_interval_ms: 250,
            cache_size_mb: 256,
            keymap: Keymap::Standard,
            custom_keybindings: BTreeMap::new(),
            default_encoding: "auto".to_string(),
            timezone: TimezoneSetting::Local,
            time_gap_threshold_secs: 0.0,
            notifications_enabled: true,
            update_check: false,
            renderer: Renderer::Auto,
            recent_files_limit: 20,
        }
    }
}

impl Settings {
    /// Defaults appropriate for `mode`: the update check is on only for an
    /// installed copy (never phone home from a portable one).
    pub fn defaults_for(mode: &DataMode) -> Self {
        Self {
            update_check: matches!(mode, DataMode::Installed),
            ..Self::default()
        }
    }

    /// Clamps values into sane ranges so a hand-edited file cannot cause
    /// absurd behaviour (zero poll interval, gigantic caches, NaN sizes).
    pub fn sanitize(&mut self) {
        fn clamp_f32(v: &mut f32, lo: f32, hi: f32, default: f32) {
            *v = if v.is_finite() {
                v.clamp(lo, hi)
            } else {
                default
            };
        }
        clamp_f32(&mut self.font_size, 6.0, 72.0, 13.0);
        clamp_f32(&mut self.line_height, 1.0, 3.0, 1.25);
        self.max_display_line_length = self.max_display_line_length.clamp(80, 10_000_000);
        self.follow_poll_interval_ms = self.follow_poll_interval_ms.clamp(20, 60_000);
        self.cache_size_mb = self.cache_size_mb.clamp(4, 65_536);
        self.recent_files_limit = self.recent_files_limit.min(500);
        if !self.time_gap_threshold_secs.is_finite() || self.time_gap_threshold_secs < 0.0 {
            self.time_gap_threshold_secs = 0.0;
        }
    }

    /// Parses settings from TOML text: migrates old schemas, applies the
    /// mode-dependent defaults for absent keys and sanitises. Returns the
    /// settings plus a warning if the text was unusable (then defaults are
    /// returned).
    pub fn from_toml_str(text: &str, mode: &DataMode) -> (Self, Option<String>) {
        let mut table: toml::Table = match text.parse() {
            Ok(t) => t,
            Err(e) => {
                return (
                    Self::defaults_for(mode),
                    Some(format!("settings file is not valid TOML: {e}")),
                );
            }
        };
        let mut warning = None;
        let from = migrate(&mut table);
        if from > CURRENT_SCHEMA_VERSION {
            warning = Some(format!(
                "settings were written by a newer OxTail (schema {from}); unknown keys are ignored"
            ));
        }
        if !table.contains_key("update_check") {
            table.insert(
                "update_check".into(),
                toml::Value::Boolean(Self::defaults_for(mode).update_check),
            );
        }
        match table.try_into::<Settings>() {
            Ok(mut s) => {
                s.sanitize();
                (s, warning)
            }
            Err(e) => (
                Self::defaults_for(mode),
                Some(format!("settings file has invalid values: {e}")),
            ),
        }
    }

    /// Loads `path`. A missing file gives the mode's defaults with no warning;
    /// an unreadable or invalid file gives defaults plus a warning. Never
    /// fails, so startup cannot be blocked by a bad settings file.
    pub fn load(path: &Path, mode: &DataMode) -> (Self, Option<String>) {
        match fs::read(path) {
            Ok(bytes) => Self::from_toml_str(&String::from_utf8_lossy(&bytes), mode),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::defaults_for(mode), None),
            Err(e) => (
                Self::defaults_for(mode),
                Some(format!("cannot read {}: {e}", path.display())),
            ),
        }
    }

    /// Serialises to TOML.
    pub fn to_toml_string(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Saves atomically to `path`.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let text = self.to_toml_string()?;
        write_atomic(path, text.as_bytes()).map_err(|e| ConfigError::io(path, e))
    }
}

/// Upgrades a raw settings table to [`CURRENT_SCHEMA_VERSION`] in place and
/// returns the schema version it had (0 if the file had no `schema_version`).
///
/// * v0 to v1: keys were renamed (`fontsize` to `font_size`, `word_wrap` to
///   `wrap`, `show_line_numbers` to `line_numbers`, `poll_ms` to
///   `follow_poll_interval_ms`, `time_zone` to `timezone`,
///   `notifications` to `notifications_enabled`).
pub fn migrate(table: &mut toml::Table) -> u32 {
    let from = table
        .get("schema_version")
        .and_then(toml::Value::as_integer)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);
    if from == 0 {
        for (old, new) in [
            ("fontsize", "font_size"),
            ("word_wrap", "wrap"),
            ("show_line_numbers", "line_numbers"),
            ("poll_ms", "follow_poll_interval_ms"),
            ("time_zone", "timezone"),
            ("notifications", "notifications_enabled"),
        ] {
            if let Some(v) = table.remove(old) {
                table.entry(new).or_insert(v);
            }
        }
    }
    if from < CURRENT_SCHEMA_VERSION {
        table.insert(
            "schema_version".into(),
            toml::Value::Integer(i64::from(CURRENT_SCHEMA_VERSION)),
        );
    }
    from
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut s = Settings {
            theme: ThemeChoice::Custom,
            custom_theme: "solar".into(),
            font_size: 15.5,
            wrap: true,
            keymap: Keymap::Less,
            timezone: TimezoneSetting::Named("Europe/Amsterdam".into()),
            renderer: Renderer::Glow,
            ..Settings::default()
        };
        s.custom_keybindings
            .insert("search".into(), "Ctrl+F".into());
        let text = s.to_toml_string().expect("ser");
        let (back, warn) = Settings::from_toml_str(&text, &DataMode::Portable);
        assert_eq!(warn, None);
        assert_eq!(back, s);
    }

    #[test]
    fn migrate_from_v0() {
        let v0 = "fontsize = 18.0\nword_wrap = true\nshow_line_numbers = false\n\
                  poll_ms = 500\ntime_zone = \"utc\"\nnotifications = false\ntheme = \"dark\"\n";
        let (s, warn) = Settings::from_toml_str(v0, &DataMode::Portable);
        assert_eq!(warn, None);
        assert_eq!(s.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(s.font_size, 18.0);
        assert!(s.wrap && !s.line_numbers && !s.notifications_enabled);
        assert_eq!(s.follow_poll_interval_ms, 500);
        assert_eq!(s.timezone, TimezoneSetting::Utc);
        assert_eq!(s.theme, ThemeChoice::Dark);
    }

    #[test]
    fn migrate_reports_old_version_and_is_idempotent() {
        let mut t: toml::Table = "fontsize = 9".parse().expect("toml");
        assert_eq!(migrate(&mut t), 0);
        assert_eq!(migrate(&mut t), CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn update_check_default_depends_on_mode() {
        let (p, _) = Settings::from_toml_str("", &DataMode::Portable);
        let (i, _) = Settings::from_toml_str("", &DataMode::Installed);
        assert!(!p.update_check && i.update_check);
        let (p, _) = Settings::from_toml_str("update_check = true", &DataMode::Portable);
        assert!(p.update_check);
    }

    #[test]
    fn garbage_gives_defaults_and_warning() {
        for text in [
            "",
            "= = =",
            "font_size = \"big\"",
            "\u{0}\u{1}[[[",
            "theme = 5",
        ] {
            let (s, _w) = Settings::from_toml_str(text, &DataMode::Portable);
            assert_eq!(s.schema_version, CURRENT_SCHEMA_VERSION);
        }
        let (_, w) = Settings::from_toml_str("= = =", &DataMode::Portable);
        assert!(w.is_some());
    }

    #[test]
    fn newer_schema_warns_but_loads() {
        let (s, w) = Settings::from_toml_str(
            "schema_version = 99\nfont_size = 20.0\nfuture_key = 1\n",
            &DataMode::Portable,
        );
        assert!(w.is_some());
        assert_eq!(s.font_size, 20.0);
    }

    #[test]
    fn sanitize_clamps() {
        let (s, _) = Settings::from_toml_str(
            "font_size = 1000.0\nfollow_poll_interval_ms = 0\ncache_size_mb = 0\nline_height = nan\n",
            &DataMode::Portable,
        );
        assert_eq!(s.font_size, 72.0);
        assert_eq!(s.follow_poll_interval_ms, 20);
        assert_eq!(s.cache_size_mb, 4);
        assert_eq!(s.line_height, 1.25);
    }

    #[test]
    fn load_and_save_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("settings.toml");
        let (s, w) = Settings::load(&p, &DataMode::Portable);
        assert_eq!((s.font_size, w), (13.0, None));
        Settings { wrap: true, ..s }.save(&p).expect("save");
        assert!(Settings::load(&p, &DataMode::Portable).0.wrap);
        fs::write(&p, [0xff, 0xfe, 0x00, b'=']).expect("write");
        let (s, w) = Settings::load(&p, &DataMode::Portable);
        assert!(w.is_some() && !s.wrap);
    }
}
