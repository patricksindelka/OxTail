//! The settings window: every user-facing setting, grouped into sections,
//! plus the key bindings editor and the system integration and update
//! controls.
//!
//! The window edits a copy of [`oxtail_config::Settings`]; when the copy
//! differs afterwards, [`OxTailApp::settings_window`] stores it, applies the
//! side effects (theme, wrap, key map) and schedules the save through the
//! normal persist path (`settings.toml`, hot reload keeps working).
//! Nothing here touches the file system or the network: system integration
//! and the update check run on worker threads ([`crate::sysint`]).

use egui::{Context, Event, RichText, Ui, vec2};
use oxtail_config::integration::IntegrationState;
use oxtail_config::{Keymap, Renderer, Settings, ThemeChoice, TimezoneSetting};

use crate::app::OxTailApp;
use crate::keymap::{Action, Chord, KeyMap, is_bindable};
use crate::panels::ENCODINGS;
use crate::sysint::{IntegrationView, SysState, UpdateStatus, is_web_url};

/// The groups of settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Section {
    /// Theme, font, layout of the text.
    #[default]
    Appearance,
    /// Key bindings.
    Keyboard,
    /// Opening, caching and following files, notifications, renderer.
    Files,
    /// Time zone, gaps, sorting.
    Columns,
    /// Update check.
    Updates,
    /// System integration and the data folder.
    System,
}

impl Section {
    /// All sections in display order.
    pub const ALL: [Section; 6] = [
        Section::Appearance,
        Section::Keyboard,
        Section::Files,
        Section::Columns,
        Section::Updates,
        Section::System,
    ];

    /// The name in the section list.
    pub fn label(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Keyboard => "Keyboard",
            Section::Files => "Files & following",
            Section::Columns => "Columns & time",
            Section::Updates => "Updates",
            Section::System => "System",
        }
    }
}

/// State of the settings window itself (not the settings).
#[derive(Debug, Default)]
pub struct SettingsUi {
    /// The section shown.
    pub section: Section,
    /// The action whose new shortcut is being recorded.
    pub recording: Option<Action>,
    /// Filter text of the key bindings list.
    pub key_filter: String,
}

/// What the key recorder saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    /// Nothing usable yet.
    Nothing,
    /// The user pressed Esc: stop recording.
    Cancel,
    /// A chord was pressed.
    Chord(Chord),
}

/// Reads the key recorder's answer from this frame's events: Esc cancels,
/// the first bindable key with its modifiers is the chord.
pub fn record_chord(events: &[Event], mods: egui::Modifiers, mac: bool) -> Recorded {
    for e in events {
        // Cmd/Ctrl+C/X/V (with any Shift or Alt) arrive as their own events.
        let clipboard = match e {
            Event::Copy => Some(egui::Key::C),
            Event::Cut => Some(egui::Key::X),
            Event::Paste(_) => Some(egui::Key::V),
            _ => None,
        };
        if let Some(key) = clipboard {
            if !(mods.command || mods.ctrl) {
                continue;
            }
            return Recorded::Chord(Chord::from_event(key, mods, mac));
        }
        if let Event::Key {
            key,
            pressed: true,
            modifiers,
            repeat: false,
            ..
        } = e
        {
            let none = !(modifiers.command || modifiers.ctrl || modifiers.alt || modifiers.shift);
            if *key == egui::Key::Escape && none {
                return Recorded::Cancel;
            }
            if is_bindable(*key) {
                return Recorded::Chord(Chord::from_event(*key, *modifiers, mac));
            }
        }
    }
    Recorded::Nothing
}

/// Sets (or clears) the override of `action` in `custom` so that it is bound
/// to `chord`; an override equal to the preset's default is removed.
pub fn set_binding(
    custom: &mut std::collections::BTreeMap<String, String>,
    preset: Keymap,
    action: Action,
    chord: Chord,
    mac: bool,
) {
    let defaults = KeyMap::default_chords(preset, action, mac);
    if defaults.len() == 1 && defaults[0] == chord.normalized(mac) {
        custom.remove(action.id());
    } else {
        custom.insert(action.id().to_string(), chord.to_config());
    }
}

/// What the window asks the application to do besides editing settings.
#[derive(Debug, Default)]
struct Effects {
    check_now: bool,
    integrate: bool,
    remove: bool,
    open_url: Option<String>,
}

fn note(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).small().weak()).wrap());
}

/// A two-column form: label on the left, control (and note) on the right.
fn form(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([18.0, 10.0])
        .min_col_width(if ui.available_width() < 480.0 {
            100.0
        } else {
            150.0
        })
        .show(ui, add);
}

fn row(ui: &mut Ui, label: &str, control: impl FnOnce(&mut Ui)) {
    ui.label(label);
    ui.vertical(|ui| {
        ui.set_max_width(360.0_f32.min(ui.available_width()));
        control(ui);
    });
    ui.end_row();
}

fn heading(ui: &mut Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(RichText::new(text).strong().size(15.0));
    ui.add_space(4.0);
}

// ----------------------------------------------------------------- sections

fn appearance(ui: &mut Ui, s: &mut Settings, themes: &[String]) {
    heading(ui, "Appearance");
    form(ui, "settings-appearance", |ui| {
        row(ui, "Theme", |ui| {
            let name = match s.theme {
                ThemeChoice::System => "Follow system",
                ThemeChoice::Dark => "Dark",
                ThemeChoice::Light => "Light",
                ThemeChoice::HighContrast => "High contrast",
                ThemeChoice::Custom => "Custom",
            };
            egui::ComboBox::from_id_salt("theme")
                .selected_text(name)
                .width(200.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.theme, ThemeChoice::System, "Follow system");
                    ui.selectable_value(&mut s.theme, ThemeChoice::Dark, "Dark");
                    ui.selectable_value(&mut s.theme, ThemeChoice::Light, "Light");
                    ui.selectable_value(&mut s.theme, ThemeChoice::HighContrast, "High contrast");
                    ui.selectable_value(&mut s.theme, ThemeChoice::Custom, "Custom");
                });
            if s.theme == ThemeChoice::Custom {
                egui::ComboBox::from_id_salt("custom-theme")
                    .selected_text(if s.custom_theme.is_empty() {
                        "Choose a theme"
                    } else {
                        s.custom_theme.as_str()
                    })
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        for n in themes {
                            ui.selectable_value(&mut s.custom_theme, n.clone(), n);
                        }
                    });
            }
        });
        row(ui, "Font", |ui| {
            egui::ComboBox::from_id_salt("font-family")
                .selected_text("Built-in monospace")
                .width(200.0)
                .show_ui(ui, |ui| {
                    // The only font OxTail can load is the one it carries.
                    let _ = ui.selectable_label(true, "Built-in monospace");
                });
            let chosen = s.font_family.trim();
            if chosen.is_empty() {
                note(
                    ui,
                    "OxTail carries its own font so it looks the same everywhere. \
                     Other fonts are not supported yet.",
                );
            } else {
                note(
                    ui,
                    &format!(
                        "settings.toml asks for \u{201c}{chosen}\u{201d}, which OxTail cannot \
                         load yet; the built-in font is used."
                    ),
                );
            }
        });
        row(ui, "Font size", |ui| {
            ui.add(
                egui::Slider::new(&mut s.font_size, 6.0..=72.0)
                    .clamping(egui::SliderClamping::Edits)
                    .suffix(" pt"),
            );
            note(ui, "Ctrl+wheel or Ctrl+= / Ctrl+- change it too.");
        });
        row(ui, "Line height", |ui| {
            ui.add(
                egui::Slider::new(&mut s.line_height, 1.0..=3.0)
                    .clamping(egui::SliderClamping::Edits)
                    .fixed_decimals(2),
            );
        });
        row(ui, "Wrap long lines", |ui| {
            ui.checkbox(&mut s.wrap, "Wrap instead of scrolling sideways");
        });
        row(ui, "Line numbers", |ui| {
            ui.checkbox(&mut s.line_numbers, "Show the line number column");
        });
        row(ui, "Minimap", |ui| {
            ui.checkbox(
                &mut s.show_minimap,
                "Show match and rule hits next to the scrollbar",
            );
        });
        row(ui, "Longest displayed line", |ui| {
            ui.add(
                egui::DragValue::new(&mut s.max_display_line_length)
                    .range(80..=10_000_000)
                    .speed(100.0)
                    .suffix(" characters"),
            );
            note(
                ui,
                "Longer lines are cut for display. Applies to files opened from now on.",
            );
        });
    });
}

fn keyboard(ui: &mut Ui, s: &mut Settings, state: &mut SettingsUi, keymap: &KeyMap) {
    let mac = keymap.is_mac();
    heading(ui, "Keyboard");
    form(ui, "settings-keyboard", |ui| {
        row(ui, "Key bindings", |ui| {
            ui.radio_value(&mut s.keymap, Keymap::Standard, "Standard");
            ui.radio_value(
                &mut s.keymap,
                Keymap::Less,
                "less-style (g, G, /, n, N, F, j, k, Space, b)",
            );
            note(
                ui,
                "The less keys only work when no text field has the focus. \
                 The standard shortcuts stay available.",
            );
        });
    });
    let warn = ui.visuals().warn_fg_color;
    if !keymap.warnings().is_empty() {
        ui.add_space(6.0);
        for w in keymap.warnings() {
            ui.add(egui::Label::new(RichText::new(w).color(warn).small()).wrap());
        }
    }
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Shortcuts").strong().size(15.0));
        ui.add_space(8.0);
        ui.add(
            egui::TextEdit::singleline(&mut state.key_filter)
                .hint_text("Filter commands")
                .desired_width(180.0),
        );
        if !s.custom_keybindings.is_empty() && ui.button("Reset all").clicked() {
            s.custom_keybindings.clear();
            state.recording = None;
        }
    });
    note(
        ui,
        "Click Change, then press the new shortcut (Esc cancels). Shortcuts you changed are marked.",
    );
    ui.add_space(4.0);
    let recorded = if state.recording.is_some() {
        ui.ctx()
            .input(|i| record_chord(&i.events, i.modifiers, mac))
    } else {
        Recorded::Nothing
    };
    match (recorded, state.recording) {
        (Recorded::Cancel, _) => state.recording = None,
        (Recorded::Chord(c), Some(a)) => {
            set_binding(&mut s.custom_keybindings, s.keymap, a, c, mac);
            state.recording = None;
        }
        _ => {}
    }
    if state.recording.is_some() {
        // The recorder owns the keyboard: keep the keys from editing widgets.
        ui.ctx().input_mut(|i| {
            i.events.retain(|e| {
                !matches!(
                    e,
                    Event::Key { .. } | Event::Text(_) | Event::Copy | Event::Cut | Event::Paste(_)
                )
            });
        });
    }
    let error = ui.visuals().error_fg_color;
    let mut last_category = "";
    egui::Grid::new("key-bindings")
        .num_columns(3)
        .spacing([14.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            for &a in Action::ALL {
                if !state.key_filter.trim().is_empty()
                    && crate::palette::fuzzy(&state.key_filter, a.label()).is_none()
                    && crate::palette::fuzzy(&state.key_filter, a.category()).is_none()
                {
                    continue;
                }
                if a.category() != last_category {
                    last_category = a.category();
                    ui.label(RichText::new(a.category()).strong());
                    ui.label("");
                    ui.label("");
                    ui.end_row();
                }
                let changed = s.custom_keybindings.contains_key(a.id());
                let conflict = keymap.in_conflict(a);
                let name = RichText::new(a.label());
                let name = if conflict { name.color(error) } else { name };
                ui.label(name)
                    .on_hover_text(format!("custom_keybindings id: {}", a.id()));
                let recording = state.recording == Some(a);
                let text = if recording {
                    "Press a shortcut\u{2026}".to_string()
                } else {
                    let t = keymap.shortcut_text_all(a);
                    if t.is_empty() {
                        "unassigned".to_string()
                    } else {
                        t
                    }
                };
                let mut chords = RichText::new(text).monospace();
                if recording {
                    chords = chords.strong();
                } else if keymap.chords_for(a).is_empty() {
                    chords = chords.weak();
                } else if conflict {
                    chords = chords.color(error);
                }
                ui.horizontal(|ui| {
                    ui.set_min_width(190.0);
                    ui.label(chords);
                    if changed {
                        ui.label(RichText::new("\u{25cf}").small().color(warn))
                            .on_hover_text("Changed from the preset");
                    }
                });
                ui.horizontal(|ui| {
                    if !a.remappable() {
                        ui.label(RichText::new("fixed").small().weak());
                        return;
                    }
                    let change = ui
                        .add_sized(
                            [64.0, 20.0],
                            egui::Button::new(if recording { "Cancel" } else { "Change" }),
                        )
                        .on_hover_text(format!(
                            "Record a new shortcut for \u{201c}{}\u{201d}",
                            a.label()
                        ));
                    if change.clicked() {
                        state.recording = if recording { None } else { Some(a) };
                    }
                    if ui
                        .add_enabled(!keymap.chords_for(a).is_empty(), egui::Button::new("Clear"))
                        .on_hover_text("Remove the shortcut")
                        .clicked()
                    {
                        s.custom_keybindings
                            .insert(a.id().to_string(), String::new());
                    }
                    if ui
                        .add_enabled(changed, egui::Button::new("Reset"))
                        .on_hover_text("Back to the preset's shortcut")
                        .clicked()
                    {
                        s.custom_keybindings.remove(a.id());
                    }
                });
                if conflict {
                    // The conflict is spelled out in the warnings above.
                }
                ui.end_row();
            }
        });
}

fn files(ui: &mut Ui, s: &mut Settings) {
    heading(ui, "Files & following");
    form(ui, "settings-files", |ui| {
        row(ui, "Cache size", |ui| {
            ui.add(
                egui::DragValue::new(&mut s.cache_size_mb)
                    .range(4..=65_536)
                    .speed(1.0)
                    .suffix(" MB"),
            );
            note(
                ui,
                "Memory for recently read parts of each file. Applies to files opened from now on.",
            );
        });
        row(ui, "Follow poll interval", |ui| {
            ui.add(
                egui::DragValue::new(&mut s.follow_poll_interval_ms)
                    .range(20..=60_000)
                    .speed(5.0)
                    .suffix(" ms"),
            );
            note(
                ui,
                "Used when the system cannot report file changes (network shares).",
            );
        });
        row(ui, "Recent files", |ui| {
            ui.add(
                egui::DragValue::new(&mut s.recent_files_limit)
                    .range(0..=500)
                    .speed(0.2),
            );
            note(
                ui,
                "How many files the start screen and File menu remember.",
            );
        });
        row(ui, "Default encoding", |ui| {
            let current = if s.default_encoding.eq_ignore_ascii_case("auto")
                || s.default_encoding.is_empty()
            {
                "Auto-detect".to_string()
            } else {
                ENCODINGS
                    .iter()
                    .find(|(_, n)| n.eq_ignore_ascii_case(&s.default_encoding))
                    .map_or_else(|| s.default_encoding.clone(), |(l, _)| (*l).to_string())
            };
            egui::ComboBox::from_id_salt("default-encoding")
                .selected_text(current)
                .width(260.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.default_encoding, "auto".to_string(), "Auto-detect");
                    for (label, name) in ENCODINGS {
                        ui.selectable_value(&mut s.default_encoding, (*name).to_string(), *label);
                    }
                });
            note(
                ui,
                "For files opened from now on. Each tab can override it in the status bar.",
            );
        });
        row(ui, "Notifications", |ui| {
            ui.checkbox(
                &mut s.notifications_enabled,
                "Desktop notifications for alert rules",
            );
        });
        row(ui, "Renderer", |ui| {
            let name = match s.renderer {
                Renderer::Auto => "Automatic",
                Renderer::Wgpu => "GPU (wgpu)",
                Renderer::Glow => "OpenGL (glow)",
            };
            egui::ComboBox::from_id_salt("renderer")
                .selected_text(name)
                .width(200.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.renderer, Renderer::Auto, "Automatic");
                    ui.selectable_value(&mut s.renderer, Renderer::Wgpu, "GPU (wgpu)");
                    ui.selectable_value(&mut s.renderer, Renderer::Glow, "OpenGL (glow)");
                });
            note(
                ui,
                "Restart required. Automatic tries the GPU first, then OpenGL.",
            );
        });
    });
}

fn columns(ui: &mut Ui, s: &mut Settings) {
    heading(ui, "Columns & time");
    form(ui, "settings-columns", |ui| {
        row(ui, "Time zone", |ui| {
            ui.horizontal(|ui| {
                let label = match &s.timezone {
                    TimezoneSetting::Local => "Local",
                    TimezoneSetting::Utc => "UTC",
                    TimezoneSetting::Named(_) => "Named",
                };
                egui::ComboBox::from_id_salt("time-zone")
                    .selected_text(label)
                    .width(90.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut s.timezone, TimezoneSetting::Local, "Local");
                        ui.selectable_value(&mut s.timezone, TimezoneSetting::Utc, "UTC");
                        if ui
                            .selectable_label(
                                matches!(s.timezone, TimezoneSetting::Named(_)),
                                "Named",
                            )
                            .clicked()
                            && !matches!(s.timezone, TimezoneSetting::Named(_))
                        {
                            s.timezone = TimezoneSetting::Named("Europe/Amsterdam".into());
                        }
                    });
                if let TimezoneSetting::Named(n) = &mut s.timezone {
                    ui.add(
                        egui::TextEdit::singleline(n)
                            .desired_width(170.0)
                            .hint_text("IANA name, e.g. Europe/Amsterdam"),
                    );
                }
            });
            note(
                ui,
                "Used to read timestamps without a zone and to show times.",
            );
        });
        row(ui, "Time gap separator", |ui| {
            ui.add(
                egui::DragValue::new(&mut s.time_gap_threshold_secs)
                    .range(0.0..=f64::MAX)
                    .clamp_existing_to_range(false)
                    .speed(0.5)
                    .suffix(" s"),
            );
            note(
                ui,
                "Draw a line between entries further apart than this. 0 switches it off.",
            );
        });
        row(ui, "Largest sortable view", |ui| {
            ui.add(
                egui::DragValue::new(&mut s.sort_max_rows)
                    .range(0..=10_000_000)
                    .speed(1000.0)
                    .suffix(" rows"),
            );
            note(
                ui,
                "Sorting a column reads every row of the view, so it is only offered up to this size.",
            );
        });
    });
}

fn updates(ui: &mut Ui, s: &mut Settings, sys: &SysState, fx: &mut Effects) {
    heading(ui, "Updates");
    form(ui, "settings-updates", |ui| {
        row(ui, "Version", |ui| {
            ui.label(format!("OxTail {}", env!("CARGO_PKG_VERSION")));
        });
        row(ui, "Automatic check", |ui| {
            ui.checkbox(&mut s.update_check, "Check for a newer version at startup");
            note(
                ui,
                "At most once a day. It asks github.com for the latest release and sends \
                 nothing about you or your files. OxTail never downloads or installs updates.",
            );
        });
        row(ui, "Latest version", |ui| {
            ui.horizontal(|ui| {
                let busy = sys.update == UpdateStatus::Checking;
                if ui
                    .add_enabled(!busy, egui::Button::new("Check now"))
                    .clicked()
                {
                    fx.check_now = true;
                }
                match &sys.update {
                    UpdateStatus::Checking => {
                        ui.spinner();
                        ui.label("Checking\u{2026}");
                    }
                    UpdateStatus::UpToDate => {
                        ui.label("You have the latest version.");
                    }
                    UpdateStatus::Failed(e) => {
                        ui.label(
                            RichText::new(format!("Check failed: {e}"))
                                .color(ui.visuals().error_fg_color),
                        );
                    }
                    UpdateStatus::Available(r) => {
                        ui.label(format!("Version {} is available.", r.version));
                    }
                    UpdateStatus::Idle => {}
                }
            });
            if let UpdateStatus::Available(r) = &sys.update
                && is_web_url(&r.url)
                && ui.link("Open the release page").clicked()
            {
                fx.open_url = Some(r.url.clone());
            }
        });
    });
}

fn system(ui: &mut Ui, sys: &SysState, dd: &oxtail_config::DataDir, fx: &mut Effects) {
    heading(ui, "System");
    form(ui, "settings-system", |ui| {
        row(ui, "Integration", |ui| {
            let integrated = matches!(
                sys.integration,
                IntegrationView::Known(IntegrationState::Integrated { .. })
            );
            let supported = !matches!(
                sys.integration,
                IntegrationView::Known(IntegrationState::Unsupported(_))
            );
            match &sys.integration {
                IntegrationView::Unknown | IntegrationView::Checking => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Checking\u{2026}");
                    });
                }
                IntegrationView::Known(IntegrationState::Integrated { items }) => {
                    ui.label(RichText::new("Integrated with the system").strong());
                    for i in items {
                        ui.label(RichText::new(format!("\u{2022} {i}")).small().weak());
                    }
                }
                IntegrationView::Known(IntegrationState::NotIntegrated) => {
                    ui.label("Not integrated");
                }
                IntegrationView::Known(IntegrationState::Unsupported(why)) => {
                    ui.label("Not available on this system");
                    note(ui, why);
                }
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let idle = !sys.integration_busy;
                if ui
                    .add_enabled(
                        idle && supported && !integrated,
                        egui::Button::new("Integrate with system"),
                    )
                    .on_hover_text("Adds \u{201c}Open with OxTail\u{201d} and a menu entry. Everything it does is listed here and can be undone.")
                    .clicked()
                {
                    fx.integrate = true;
                }
                if ui
                    .add_enabled(idle && integrated, egui::Button::new("Remove integration"))
                    .on_hover_text("Undoes everything the integration added")
                    .clicked()
                {
                    fx.remove = true;
                }
                if sys.integration_busy {
                    ui.spinner();
                }
            });
            match &sys.integration_report {
                Some(Ok(items)) => {
                    for i in items {
                        ui.label(RichText::new(format!("\u{2713} {i}")).small());
                    }
                }
                Some(Err(e)) => {
                    ui.add(
                        egui::Label::new(RichText::new(e).color(ui.visuals().error_fg_color))
                            .wrap(),
                    );
                }
                None => {}
            }
            note(
                ui,
                "OxTail is portable: it changes nothing outside its own folder unless you ask here.",
            );
        });
        row(ui, "Data folder", |ui| match &dd.root {
            Some(r) => {
                ui.add(
                    egui::Label::new(RichText::new(r.display().to_string()).monospace().small())
                        .wrap(),
                );
            }
            None => {
                ui.label("None: settings are not saved");
            }
        });
    });
}

/// The scrollable content of the selected section.
#[allow(clippy::too_many_arguments)]
fn section_body(
    ui: &mut Ui,
    state: &mut SettingsUi,
    s: &mut Settings,
    themes: &[String],
    keymap: &KeyMap,
    sys: &SysState,
    dd: &oxtail_config::DataDir,
    fx: &mut Effects,
    body_h: f32,
) {
    egui::ScrollArea::both()
        .id_salt("settings-scroll")
        .max_height(body_h)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().slider_width = (ui.available_width() - 200.0).clamp(80.0, 220.0);
            // The scroll area inherits the layout of its parent: stack the
            // section top-down.
            ui.vertical(|ui| match state.section {
                Section::Appearance => appearance(ui, s, themes),
                Section::Keyboard => keyboard(ui, s, state, keymap),
                Section::Files => files(ui, s),
                Section::Columns => columns(ui, s),
                Section::Updates => updates(ui, s, sys, fx),
                Section::System => system(ui, sys, dd, fx),
            });
        });
}

// ------------------------------------------------------------------- window

impl OxTailApp {
    /// Opens the settings window on `section`.
    pub fn open_settings(&mut self, section: Section) {
        self.windows.settings = true;
        self.settings_ui.section = section;
    }

    pub(crate) fn settings_window(&mut self, ctx: &Context) {
        if !self.windows.settings {
            self.settings_ui.recording = None;
            return;
        }
        if self.settings_ui.section == Section::System {
            let wake = self.waker();
            self.sys.query_integration(&self.data_dir, wake, false);
        }
        let mut open = true;
        let mut s = self.settings.clone();
        let mut fx = Effects::default();
        let themes: Vec<String> = self
            .themes
            .themes()
            .iter()
            .map(|t| t.name.clone())
            .collect();
        let screen = ctx.content_rect();
        let body_h = (screen.height() - 150.0).clamp(200.0, 560.0);
        let narrow = screen.width() < 720.0;
        let mut state = std::mem::take(&mut self.settings_ui);
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size(vec2(
                (screen.width() - 40.0).clamp(320.0, 780.0),
                body_h + 20.0,
            ))
            .max_size(screen.size() - vec2(24.0, 24.0))
            .show(ctx, |ui| {
                let nav = |ui: &mut Ui, state: &mut SettingsUi| {
                    for sec in Section::ALL {
                        if ui
                            .selectable_label(state.section == sec, sec.label())
                            .clicked()
                        {
                            state.section = sec;
                            state.recording = None;
                        }
                    }
                };
                if narrow {
                    // Sections as a row of tabs above the content.
                    ui.horizontal_wrapped(|ui| nav(ui, &mut state));
                    ui.separator();
                    section_body(
                        ui,
                        &mut state,
                        &mut s,
                        &themes,
                        &self.keymap,
                        &self.sys,
                        &self.data_dir,
                        &mut fx,
                        body_h,
                    );
                } else {
                    ui.horizontal_top(|ui| {
                        ui.vertical(|ui| {
                            ui.set_width(130.0);
                            ui.set_min_height(body_h);
                            nav(ui, &mut state);
                        });
                        ui.separator();
                        section_body(
                            ui,
                            &mut state,
                            &mut s,
                            &themes,
                            &self.keymap,
                            &self.sys,
                            &self.data_dir,
                            &mut fx,
                            body_h,
                        );
                    });
                }
            });
        self.settings_ui = state;
        self.windows.settings = open;
        self.apply_edited_settings(s, ctx);
        if fx.check_now {
            self.check_updates_now();
        }
        if fx.integrate {
            self.change_integration(false);
        }
        if fx.remove {
            self.change_integration(true);
        }
        if let Some(url) = fx.open_url
            && is_web_url(&url)
        {
            ctx.open_url(egui::OpenUrl::new_tab(url));
        }
        if !self.windows.settings {
            // The recorder only runs while the window is open.
            self.settings_ui.recording = None;
        }
    }

    /// Stores settings edited in the window: applies what needs applying and
    /// schedules the save.
    pub(crate) fn apply_edited_settings(&mut self, mut new: Settings, ctx: &Context) {
        new.sanitize();
        if new == self.settings {
            return;
        }
        let old = std::mem::replace(&mut self.settings, new);
        if self.settings.wrap != old.wrap {
            self.set_wrap(self.settings.wrap);
        }
        if self.settings.theme != old.theme || self.settings.custom_theme != old.custom_theme {
            self.apply_theme(ctx);
        }
        if self.settings.keymap != old.keymap
            || self.settings.custom_keybindings != old.custom_keybindings
        {
            self.rebuild_keymap();
        }
        if self.settings.time_gap_threshold_secs != old.time_gap_threshold_secs
            || self.settings.line_height != old.line_height
            || self.settings.font_size != old.font_size
        {
            self.bump_style();
        }
        self.mark_settings_dirty();
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use egui::{Key, Modifiers};

    use super::*;

    fn key(k: Key, m: Modifiers) -> Event {
        Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: m,
        }
    }

    #[test]
    fn the_recorder_takes_the_first_bindable_chord() {
        let cs = Modifiers {
            command: true,
            ctrl: true,
            shift: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            record_chord(&[key(Key::K, cs)], Modifiers::NONE, false),
            Recorded::Chord(Chord::parse_for("Ctrl+Shift+K", false).unwrap())
        );
        assert_eq!(
            record_chord(
                &[Event::Text("x".into()), key(Key::F5, Modifiers::NONE)],
                Modifiers::NONE,
                false
            ),
            Recorded::Chord(Chord::plain(Key::F5))
        );
        assert_eq!(record_chord(&[], Modifiers::NONE, false), Recorded::Nothing);
        assert_eq!(
            record_chord(&[key(Key::Escape, Modifiers::NONE)], Modifiers::NONE, false),
            Recorded::Cancel
        );
        // Shift+Esc is a chord, not a cancel.
        assert!(matches!(
            record_chord(
                &[key(Key::Escape, Modifiers::SHIFT)],
                Modifiers::SHIFT,
                false
            ),
            Recorded::Chord(_)
        ));
        // Keys that cannot be bound are ignored.
        assert_eq!(
            record_chord(&[key(Key::Copy, Modifiers::NONE)], Modifiers::NONE, false),
            Recorded::Nothing
        );
    }

    #[test]
    fn the_recorder_takes_clipboard_events_with_the_held_modifiers() {
        let ctrl_shift = Modifiers {
            command: true,
            ctrl: true,
            shift: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            record_chord(&[Event::Copy], ctrl_shift, false),
            Recorded::Chord(Chord::parse_for("Ctrl+Shift+C", false).unwrap())
        );
        assert_eq!(
            record_chord(&[Event::Paste("x".into())], Modifiers::COMMAND, false),
            Recorded::Chord(Chord::parse_for("Ctrl+V", false).unwrap())
        );
        assert_eq!(
            record_chord(&[Event::Cut], Modifiers::COMMAND, false),
            Recorded::Chord(Chord::parse_for("Ctrl+X", false).unwrap())
        );
        // Without Ctrl/Cmd it is not a shortcut.
        assert_eq!(
            record_chord(&[Event::Paste("x".into())], Modifiers::NONE, false),
            Recorded::Nothing
        );
    }

    #[test]
    fn a_binding_equal_to_the_default_removes_the_override() {
        let mut custom = BTreeMap::new();
        let ctrl_k = Chord::parse_for("Ctrl+K", false).unwrap();
        set_binding(&mut custom, Keymap::Standard, Action::Find, ctrl_k, false);
        assert_eq!(custom.get("search.find").map(String::as_str), Some("Mod+K"));
        let ctrl_f = Chord::parse_for("Ctrl+F", false).unwrap();
        set_binding(&mut custom, Keymap::Standard, Action::Find, ctrl_f, false);
        assert!(custom.is_empty());
        // Actions with two default chords keep an explicit override.
        let home = Chord::plain(Key::Home);
        set_binding(
            &mut custom,
            Keymap::Standard,
            Action::ScrollTop,
            home,
            false,
        );
        assert_eq!(custom.len(), 1);
    }

    #[test]
    fn recorded_bindings_build_a_working_key_map() {
        let mut custom = BTreeMap::new();
        set_binding(
            &mut custom,
            Keymap::Standard,
            Action::Mark,
            Chord::parse_for("Ctrl+Alt+K", false).unwrap(),
            false,
        );
        let k = KeyMap::build_for(Keymap::Standard, &custom, false);
        assert_eq!(k.shortcut_text(Action::Mark).as_deref(), Some("Ctrl+Alt+K"));
        assert!(k.warnings().is_empty());
    }

    #[test]
    fn every_section_has_a_distinct_label() {
        let mut seen = std::collections::HashSet::new();
        for s in Section::ALL {
            assert!(seen.insert(s.label()));
        }
    }
}
