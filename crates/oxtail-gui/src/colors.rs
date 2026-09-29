//! Themes: maps `oxtail-config` themes and `oxtail-highlight` palettes to
//! egui colours and `Visuals`.

use egui::{Color32, Stroke, Visuals};
use oxtail_config::{Settings, Theme, ThemeChoice, ThemeSet};
use oxtail_highlight::{ColorRef, Palette, Rgb, Style};

/// Converts a highlight colour.
pub fn rgb32(c: Rgb) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}

/// Converts a config colour.
pub fn cfg32(c: oxtail_config::Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

/// Blends `a` towards `b` by `t` (0 = `a`, 1 = `b`) in gamma space, which is
/// plenty for dimming and emphasis.
pub fn mix32(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

/// Everything the log view needs to paint: chrome colours, and the ability to
/// resolve a rule's colour reference for the active theme.
#[derive(Debug, Clone)]
pub struct Colors {
    /// Dark theme (selects egui's base visuals).
    pub dark: bool,
    /// Theme name.
    pub name: String,
    /// Text area background.
    pub background: Color32,
    /// Default text.
    pub text: Color32,
    /// Gutter background.
    pub gutter_bg: Color32,
    /// Gutter text.
    pub gutter_text: Color32,
    /// Selected lines (the solid theme colour; rows are tinted with
    /// [`Colors::selection_overlay`] so their own colours stay readable).
    pub selection_bg: Color32,
    /// Hovered / current line.
    pub current_line_bg: Color32,
    /// Search matches.
    pub search_match_bg: Color32,
    /// The current search match.
    pub search_current_bg: Color32,
    /// Borders and separators.
    pub border: Color32,
    /// Accent.
    pub accent: Color32,
    /// Status bar background.
    pub status_bg: Color32,
    /// Status bar text.
    pub status_text: Color32,
    theme: Theme,
    palette: Palette,
}

impl Colors {
    /// Colours for `theme`.
    pub fn from_theme(theme: &Theme) -> Colors {
        let palette = if theme.name.eq_ignore_ascii_case("high-contrast") {
            Palette::high_contrast()
        } else if theme.dark {
            Palette::dark()
        } else {
            Palette::light()
        };
        let ui = &theme.ui;
        let search = cfg32(ui.search_match_background);
        Colors {
            dark: theme.dark,
            name: theme.name.clone(),
            background: cfg32(ui.background),
            text: cfg32(ui.foreground),
            gutter_bg: cfg32(ui.gutter_background),
            gutter_text: cfg32(ui.gutter_foreground),
            selection_bg: cfg32(ui.selection_background),
            current_line_bg: cfg32(ui.current_line_background),
            search_match_bg: search,
            // Emphasis: a stronger, warmer version of the match colour.
            search_current_bg: mix32(
                search,
                if theme.dark {
                    Color32::from_rgb(255, 150, 40)
                } else {
                    Color32::from_rgb(255, 120, 0)
                },
                0.55,
            ),
            border: cfg32(ui.border),
            accent: cfg32(ui.accent),
            status_bg: cfg32(ui.status_bar_background),
            status_text: cfg32(ui.status_bar_foreground),
            theme: theme.clone(),
            palette,
        }
    }

    /// Resolves a rule colour: the theme's semantic colour when it defines
    /// one, otherwise the built-in palette.
    pub fn resolve(&self, c: &ColorRef) -> Color32 {
        if let ColorRef::Semantic { .. } = c
            && let Some(rgb) = self.theme.resolve(&c.to_string())
        {
            return cfg32(rgb);
        }
        rgb32(self.palette.resolve(c))
    }

    /// The translucent tint painted over selected rows, after their own
    /// background: severity colours (an error row's red) show through it.
    pub fn selection_overlay(&self) -> Color32 {
        let c = self.selection_bg;
        let alpha = if self.dark { 96 } else { 88 };
        Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
    }

    /// What to paint over a selected row: the solid selection colour for a
    /// plain row (a translucent tint over the plain background is nearly
    /// invisible in the light and high-contrast themes), the translucent
    /// overlay for a row with a background of its own, so severity shows through.
    pub fn selection_fill(&self, row_has_bg: bool) -> Color32 {
        if row_has_bg {
            self.selection_overlay()
        } else {
            self.selection_bg
        }
    }

    /// Foreground for a style (or the default text colour), with `dim`
    /// applied.
    pub fn style_fg(&self, s: &Style) -> Color32 {
        let base = s.fg.map_or(self.text, |c| self.resolve(&c));
        if s.dim {
            mix32(base, self.background, 0.45)
        } else {
            base
        }
    }

    /// Background of a style, if it sets one.
    pub fn style_bg(&self, s: &Style) -> Option<Color32> {
        s.bg.map(|c| self.resolve(&c))
    }

    /// A tick colour for the minimap.
    pub fn tick(&self, c: &ColorRef) -> Color32 {
        self.resolve(c)
    }

    /// The egui visuals for this theme.
    pub fn visuals(&self) -> Visuals {
        let mut v = if self.dark {
            Visuals::dark()
        } else {
            Visuals::light()
        };
        v.panel_fill = self.status_bg;
        v.window_fill = self.gutter_bg;
        v.extreme_bg_color = self.background;
        v.faint_bg_color = self.current_line_bg;
        v.hyperlink_color = self.accent;
        v.selection.bg_fill = self.selection_bg;
        v.selection.stroke = Stroke::new(1.0, self.text);
        v.window_stroke = Stroke::new(1.0, self.border);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, self.border);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, self.status_text);
        v.override_text_color = None;
        v
    }
}

/// Picks the theme named by the settings. `system_dark` is what the operating
/// system reports (if it does).
pub fn select_theme(settings: &Settings, themes: &ThemeSet, system_dark: Option<bool>) -> Theme {
    let name = match settings.theme {
        ThemeChoice::System => {
            if system_dark.unwrap_or(true) {
                "dark"
            } else {
                "light"
            }
        }
        ThemeChoice::Dark => "dark",
        ThemeChoice::Light => "light",
        ThemeChoice::HighContrast => "high-contrast",
        ThemeChoice::Custom => settings.custom_theme.as_str(),
    };
    themes
        .get(name)
        .or_else(|| themes.get("dark"))
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // tests build states field by field for readability
mod tests {
    use super::*;
    use oxtail_highlight::SemanticColor;

    #[test]
    fn theme_selection_follows_the_setting_and_system() {
        let themes = ThemeSet::builtin();
        let mut s = Settings::default();
        s.theme = ThemeChoice::System;
        assert_eq!(select_theme(&s, &themes, Some(false)).name, "light");
        assert_eq!(select_theme(&s, &themes, Some(true)).name, "dark");
        assert_eq!(select_theme(&s, &themes, None).name, "dark");
        s.theme = ThemeChoice::HighContrast;
        assert_eq!(select_theme(&s, &themes, Some(false)).name, "high-contrast");
        s.theme = ThemeChoice::Custom;
        s.custom_theme = "does-not-exist".into();
        assert_eq!(select_theme(&s, &themes, None).name, "dark");
    }

    #[test]
    fn semantic_colours_come_from_the_theme() {
        let themes = ThemeSet::builtin();
        let dark = Colors::from_theme(themes.get("dark").unwrap());
        let light = Colors::from_theme(themes.get("light").unwrap());
        let err = ColorRef::solid(SemanticColor::Error);
        assert_ne!(dark.resolve(&err), light.resolve(&err));
        assert_eq!(
            dark.resolve(&ColorRef::rgb(1, 2, 3)),
            Color32::from_rgb(1, 2, 3)
        );
        // Subtle backgrounds resolve too.
        let sub = ColorRef::subtle(SemanticColor::Warn);
        assert_ne!(dark.resolve(&sub), dark.background);
    }

    #[test]
    fn every_semantic_colour_resolves_in_every_theme() {
        for theme in ThemeSet::builtin().themes() {
            let c = Colors::from_theme(theme);
            for s in SemanticColor::ALL {
                for subtle in [false, true] {
                    let r = ColorRef::Semantic { color: s, subtle };
                    let _ = c.resolve(&r);
                }
            }
        }
    }

    fn to_rgb(c: Color32) -> Rgb {
        Rgb::new(c.r(), c.g(), c.b())
    }

    fn ratio(a: Color32, b: Color32) -> f64 {
        oxtail_highlight::contrast_ratio(to_rgb(a), to_rgb(b))
    }

    #[test]
    fn severity_colours_are_readable_in_every_theme() {
        use oxtail_highlight::SemanticColor as S;
        for theme in ThemeSet::builtin().themes() {
            let c = Colors::from_theme(theme);
            for s in [
                S::Error,
                S::Warn,
                S::Info,
                S::Success,
                S::Debug,
                S::Trace,
                S::Muted,
            ] {
                let fg = c.resolve(&ColorRef::solid(s));
                let r = ratio(fg, c.background);
                assert!(r >= 4.5, "{} {s:?} on the background: {r:.2}", c.name);
            }
            // Error text on the subtle error row background.
            let row = c.resolve(&ColorRef::subtle(S::Error));
            let r = ratio(c.resolve(&ColorRef::solid(S::Error)), row);
            assert!(r >= 4.5, "{} error on its row: {r:.2}", c.name);
            // Default text on every subtle row background.
            for s in [S::Error, S::Warn, S::Info] {
                let r = ratio(c.text, c.resolve(&ColorRef::subtle(s)));
                assert!(r >= 7.0, "{} text on {s:?} row: {r:.2}", c.name);
            }
            // Line numbers.
            let r = ratio(c.gutter_text, c.gutter_bg);
            assert!(r >= 4.5, "{} gutter: {r:.2}", c.name);
        }
    }

    #[test]
    fn selection_keeps_severity_rows_readable() {
        for theme in ThemeSet::builtin().themes() {
            let c = Colors::from_theme(theme);
            let row = c.resolve(&ColorRef::subtle(oxtail_highlight::SemanticColor::Error));
            let overlay = c.selection_overlay();
            assert!(overlay.a() < 255 && overlay.a() > 0);
            let tint = c.selection_bg;
            let t = f32::from(overlay.a()) / 255.0;
            let blended = mix32(row, tint, t);
            // The row is still distinguishable from a selected plain row...
            let plain = mix32(c.background, tint, t);
            assert_ne!(blended, plain, "{}", c.name);
            // ...and its text stays readable.
            let err = c.resolve(&ColorRef::solid(oxtail_highlight::SemanticColor::Error));
            assert!(
                ratio(err, blended) >= 3.0,
                "{}: {:.2}",
                c.name,
                ratio(err, blended)
            );
            assert!(ratio(c.text, blended) >= 4.5, "{}", c.name);
        }
    }

    #[test]
    fn a_selected_plain_row_stands_out_in_every_theme() {
        for theme in ThemeSet::builtin().themes() {
            let c = Colors::from_theme(theme);
            let fill = c.selection_fill(false);
            assert_eq!(fill.a(), 255, "{}", c.name);
            let r = ratio(fill, c.background);
            assert!(r >= 1.3, "{}: selected vs plain row {r:.2}", c.name);
            assert_eq!(c.selection_fill(true), c.selection_overlay());
        }
    }

    #[test]
    fn visuals_follow_the_theme() {
        let themes = ThemeSet::builtin();
        let d = Colors::from_theme(themes.get("dark").unwrap()).visuals();
        assert!(d.dark_mode);
        let l = Colors::from_theme(themes.get("light").unwrap()).visuals();
        assert!(!l.dark_mode);
        assert_eq!(
            d.extreme_bg_color,
            cfg32(themes.get("dark").unwrap().ui.background)
        );
    }

    #[test]
    fn dim_moves_towards_the_background() {
        let themes = ThemeSet::builtin();
        let c = Colors::from_theme(themes.get("dark").unwrap());
        let normal = c.style_fg(&Style::default());
        let dim = c.style_fg(&Style::default().dim());
        assert_eq!(normal, c.text);
        assert_ne!(dim, normal);
    }

    #[test]
    fn mixing_is_bounded() {
        let a = Color32::from_rgb(0, 0, 0);
        let b = Color32::from_rgb(200, 100, 50);
        assert_eq!(mix32(a, b, 0.0), a);
        assert_eq!(mix32(a, b, 1.0), b);
        assert_eq!(mix32(a, b, 5.0), b);
        assert_eq!(mix32(a, b, -1.0), a);
    }
}
