//! The rule editor window: list, edit, reorder and preview highlight rules,
//! then apply them to the tab or save them as a user profile.

use egui::{Color32, ComboBox, FontId, RichText, Ui, vec2};
use oxtail_highlight::{ColorRef, ColumnOp, Rule, RuleMatcher, Scope, SemanticColor};

use crate::colors::Colors;
use crate::highlight::{HighlightState, compile_lenient};
use crate::text::{JobOptions, build_job, compose};

/// What the user asked for when closing an interaction with the editor.
#[derive(Debug, Clone, PartialEq)]
pub enum EditorAction {
    /// Nothing.
    None,
    /// Use these rules in the current tab.
    Apply(Vec<Rule>),
    /// Save as a user profile named `name` (based on `base`) and use it.
    Save {
        /// Profile name.
        name: String,
        /// The rules.
        rules: Vec<Rule>,
        /// The profile whose other settings (file patterns, columns) to keep.
        base: Option<String>,
    },
}

/// Editor state.
#[derive(Default)]
pub struct RuleEditor {
    /// The window is visible.
    pub open: bool,
    /// The rules being edited.
    pub rules: Vec<Rule>,
    /// The selected rule.
    pub selected: Option<usize>,
    /// Name to save under.
    pub profile_name: String,
    /// The profile the rules came from.
    pub base_profile: Option<String>,
    /// A message under the buttons.
    pub status: Option<String>,
}

impl RuleEditor {
    /// Opens the editor on a copy of `rules`.
    pub fn open_with(&mut self, rules: &[Rule], profile: Option<&str>) {
        self.open = true;
        self.rules = rules.to_vec();
        self.selected = if rules.is_empty() { None } else { Some(0) };
        self.base_profile = profile.map(str::to_string);
        self.profile_name = match profile {
            Some(p) => format!("{p} (custom)"),
            None => "My rules".to_string(),
        };
        self.status = None;
    }

    /// Draws the window. `preview` are sample lines (the tab's visible lines).
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        colors: &Colors,
        preview: &[String],
    ) -> EditorAction {
        let mut action = EditorAction::None;
        let mut open = self.open;
        egui::Window::new("Highlight rules")
            .open(&mut open)
            .default_size([820.0, 560.0])
            .resizable(true)
            .show(ctx, |ui| {
                action = self.contents(ui, colors, preview);
            });
        self.open = open && self.open;
        action
    }

    fn contents(&mut self, ui: &mut Ui, colors: &Colors, preview: &[String]) -> EditorAction {
        let mut action = EditorAction::None;
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(230.0);
                self.list(ui);
            });
            ui.separator();
            ui.vertical(|ui| match self.selected.filter(|&i| i < self.rules.len()) {
                Some(i) => rule_form(ui, &mut self.rules[i], colors),
                None => {
                    ui.label("Select a rule, or add one.");
                }
            });
        });
        ui.separator();
        self.preview_ui(ui, colors, preview);
        ui.separator();
        let (_, _, warnings) = compile_lenient(&self.rules);
        for w in &warnings {
            ui.label(RichText::new(w).color(Color32::LIGHT_RED));
        }
        ui.horizontal(|ui| {
            if ui.button("Apply to this tab").clicked() {
                action = EditorAction::Apply(self.rules.clone());
                self.status = Some("Applied.".into());
            }
            ui.separator();
            ui.label("Save as profile");
            ui.text_edit_singleline(&mut self.profile_name);
            if ui
                .add_enabled(
                    !self.profile_name.trim().is_empty(),
                    egui::Button::new("Save"),
                )
                .on_hover_text("Writes a user profile into the data folder")
                .clicked()
            {
                action = EditorAction::Save {
                    name: self.profile_name.trim().to_string(),
                    rules: self.rules.clone(),
                    base: self.base_profile.clone(),
                };
                self.status = Some("Saving\u{2026}".into());
            }
            if let Some(s) = &self.status {
                ui.label(s);
            }
        });
        action
    }

    fn list(&mut self, ui: &mut Ui) {
        ui.strong("Rules");
        egui::ScrollArea::vertical()
            .max_height(300.0)
            .id_salt("rule-list")
            .show(ui, |ui| {
                for (i, r) in self.rules.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut r.enabled, "");
                        let name = if r.name.is_empty() {
                            format!("Rule {}", i + 1)
                        } else {
                            r.name.clone()
                        };
                        let sel = self.selected == Some(i);
                        if ui.selectable_label(sel, name).clicked() {
                            self.selected = Some(i);
                        }
                    });
                }
            });
        ui.horizontal(|ui| {
            if ui.button("Add").clicked() {
                self.rules.push(Rule::literal("New rule", "text"));
                self.selected = Some(self.rules.len() - 1);
            }
            let sel = self.selected.filter(|&i| i < self.rules.len());
            if ui
                .add_enabled(sel.is_some(), egui::Button::new("Copy"))
                .clicked()
                && let Some(i) = sel
            {
                let mut c = self.rules[i].clone();
                c.name = format!("{} copy", c.name);
                self.rules.insert(i + 1, c);
                self.selected = Some(i + 1);
            }
            if ui
                .add_enabled(sel.is_some(), egui::Button::new("Delete"))
                .clicked()
                && let Some(i) = sel
            {
                self.rules.remove(i);
                self.selected = if self.rules.is_empty() {
                    None
                } else {
                    Some(i.min(self.rules.len() - 1))
                };
            }
        });
        ui.horizontal(|ui| {
            let sel = self.selected.filter(|&i| i < self.rules.len());
            if ui
                .add_enabled(sel.is_some_and(|i| i > 0), egui::Button::new("Up"))
                .clicked()
                && let Some(i) = sel
            {
                self.rules.swap(i, i - 1);
                self.selected = Some(i - 1);
            }
            if ui
                .add_enabled(
                    sel.is_some_and(|i| i + 1 < self.rules.len()),
                    egui::Button::new("Down"),
                )
                .clicked()
                && let Some(i) = sel
            {
                self.rules.swap(i, i + 1);
                self.selected = Some(i + 1);
            }
        });
    }

    fn preview_ui(&self, ui: &mut Ui, colors: &Colors, preview: &[String]) {
        ui.strong("Live preview (the lines currently on screen)");
        if preview.is_empty() {
            ui.label(RichText::new("Nothing on screen to preview.").weak());
            return;
        }
        let (compiled, usable, _) = compile_lenient(&self.rules);
        let _ = usable;
        let font = FontId::monospace(13.0);
        egui::Frame::new()
            .fill(colors.background)
            .inner_margin(4.0)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                for text in preview.iter().take(10) {
                    let hl = compiled.highlight(text, None);
                    let layer = hl
                        .line_style
                        .map(|s| {
                            vec![oxtail_highlight::StyledSpan {
                                range: 0..text.len(),
                                style: oxtail_highlight::Style { bg: None, ..s },
                            }]
                        })
                        .unwrap_or_default();
                    let segs = compose(text.len(), &[&layer, &hl.spans], &[], false);
                    let job = build_job(
                        text,
                        &segs,
                        false,
                        &JobOptions {
                            colors,
                            font: font.clone(),
                            row_height: 16.0,
                            wrap_width: None,
                            dimmed: false,
                        },
                    );
                    let bg = hl
                        .line_style
                        .as_ref()
                        .and_then(|s| colors.style_bg(s))
                        .unwrap_or(Color32::TRANSPARENT);
                    egui::Frame::new().fill(bg).show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.add(egui::Label::new(job).truncate());
                    });
                }
            });
    }
}

fn kind_name(m: &RuleMatcher) -> &'static str {
    match m {
        RuleMatcher::Literal { .. } => "Literal text",
        RuleMatcher::Regex { .. } => "Regular expression",
        RuleMatcher::Column { .. } => "Column condition",
    }
}

const OPS: [(ColumnOp, &str); 8] = [
    (ColumnOp::Eq, "equals"),
    (ColumnOp::Ne, "not equal"),
    (ColumnOp::Contains, "contains"),
    (ColumnOp::Regex, "matches regex"),
    (ColumnOp::Gt, ">"),
    (ColumnOp::Ge, ">="),
    (ColumnOp::Lt, "<"),
    (ColumnOp::Le, "<="),
];

fn rule_form(ui: &mut Ui, r: &mut Rule, colors: &Colors) {
    egui::Grid::new("rule-form")
        .num_columns(2)
        .spacing([10.0, 6.0])
        .show(ui, |ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut r.name);
            ui.end_row();

            ui.label("Matches");
            ui.vertical(|ui| {
                let mut kind = match &r.matcher {
                    RuleMatcher::Literal { .. } => 0,
                    RuleMatcher::Regex { .. } => 1,
                    RuleMatcher::Column { .. } => 2,
                };
                let before = kind;
                ComboBox::from_id_salt("matcher-kind")
                    .selected_text(kind_name(&r.matcher))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut kind, 0, "Literal text");
                        ui.selectable_value(&mut kind, 1, "Regular expression");
                        ui.selectable_value(&mut kind, 2, "Column condition");
                    });
                if kind != before {
                    let text = match &r.matcher {
                        RuleMatcher::Literal { text, .. } => text.clone(),
                        RuleMatcher::Regex { pattern, .. } => pattern.clone(),
                        RuleMatcher::Column { value, .. } => value.clone(),
                    };
                    r.matcher = match kind {
                        0 => RuleMatcher::Literal {
                            text,
                            case_sensitive: false,
                        },
                        1 => RuleMatcher::Regex {
                            pattern: text,
                            case_sensitive: true,
                        },
                        _ => RuleMatcher::Column {
                            column: "level".into(),
                            op: ColumnOp::Eq,
                            value: text,
                        },
                    };
                }
                match &mut r.matcher {
                    RuleMatcher::Literal {
                        text,
                        case_sensitive,
                    } => {
                        ui.text_edit_singleline(text);
                        ui.checkbox(case_sensitive, "Case sensitive");
                    }
                    RuleMatcher::Regex {
                        pattern,
                        case_sensitive,
                    } => {
                        ui.text_edit_singleline(pattern);
                        ui.checkbox(case_sensitive, "Case sensitive");
                        if !pattern.is_empty()
                            && let Err(e) = oxtail_search::Matcher::compile(
                                &oxtail_search::Query::regex(pattern.as_str()),
                            )
                        {
                            ui.label(RichText::new(e.to_string()).color(Color32::LIGHT_RED));
                        }
                    }
                    RuleMatcher::Column { column, op, value } => {
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(column).desired_width(90.0));
                            let label = OPS
                                .iter()
                                .find(|(o, _)| o == op)
                                .map_or("equals", |(_, l)| *l);
                            ComboBox::from_id_salt("column-op")
                                .selected_text(label)
                                .show_ui(ui, |ui| {
                                    for (o, l) in OPS {
                                        ui.selectable_value(op, o, l);
                                    }
                                });
                            ui.add(egui::TextEdit::singleline(value).desired_width(90.0));
                        });
                        ui.label(
                            RichText::new("Needs a column definition (columns view: later).")
                                .weak(),
                        );
                    }
                }
            });
            ui.end_row();

            ui.label("Styles");
            ui.vertical(|ui| {
                let mut s = match &r.scope {
                    Scope::Line => 0,
                    Scope::Match => 1,
                    Scope::Group(_) => 2,
                    Scope::Column(_) => 3,
                };
                let before = s;
                ComboBox::from_id_salt("scope")
                    .selected_text(match s {
                        0 => "the whole line",
                        1 => "each match",
                        2 => "a capture group",
                        _ => "a column",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut s, 0, "the whole line");
                        ui.selectable_value(&mut s, 1, "each match");
                        ui.selectable_value(&mut s, 2, "a capture group");
                        ui.selectable_value(&mut s, 3, "a column");
                    });
                if s != before {
                    r.scope = match s {
                        0 => Scope::Line,
                        1 => Scope::Match,
                        2 => Scope::Group(1),
                        _ => Scope::Column("level".into()),
                    };
                }
                match &mut r.scope {
                    Scope::Group(n) => {
                        ui.add(egui::DragValue::new(n).range(0..=99).prefix("group "));
                    }
                    Scope::Column(c) => {
                        ui.add(egui::TextEdit::singleline(c).desired_width(120.0));
                    }
                    _ => {}
                }
            });
            ui.end_row();

            ui.label("Foreground");
            color_edit(ui, "fg", &mut r.style.fg, colors);
            ui.end_row();
            ui.label("Background");
            color_edit(ui, "bg", &mut r.style.bg, colors);
            ui.end_row();
            ui.label("Font");
            ui.horizontal(|ui| {
                ui.checkbox(&mut r.style.bold, "Bold");
                ui.checkbox(&mut r.style.italic, "Italic");
                ui.checkbox(&mut r.style.underline, "Underline");
                ui.checkbox(&mut r.style.dim, "Dim");
            });
            ui.end_row();

            ui.label("Priority");
            ui.add(egui::DragValue::new(&mut r.priority).range(-1000..=1000));
            ui.end_row();

            ui.label("Actions");
            ui.vertical(|ui| {
                ui.checkbox(&mut r.actions.alert, "Alert (notification) for new lines");
                ui.checkbox(&mut r.actions.hide, "Hide matching lines");
                ui.checkbox(&mut r.actions.bookmark, "Bookmark matching lines");
                ui.checkbox(&mut r.gutter_marker, "Gutter marker");
            });
            ui.end_row();

            ui.label("Minimap tick");
            color_edit(ui, "minimap", &mut r.minimap, colors);
            ui.end_row();
        });
}

/// A colour picker for an optional [`ColorRef`]: none, a semantic colour
/// (solid or subtle) or a custom RGB value.
fn color_edit(ui: &mut Ui, salt: &str, value: &mut Option<ColorRef>, colors: &Colors) {
    ui.horizontal(|ui| {
        let label = match value {
            None => "none".to_string(),
            Some(c) => c.to_string(),
        };
        if let Some(c) = value {
            swatch(ui, colors.resolve(c));
        }
        ComboBox::from_id_salt(("color", salt))
            .selected_text(label)
            .show_ui(ui, |ui| {
                if ui.selectable_label(value.is_none(), "none").clicked() {
                    *value = None;
                }
                for sc in SemanticColor::ALL {
                    for subtle in [false, true] {
                        let c = ColorRef::Semantic { color: sc, subtle };
                        ui.horizontal(|ui| {
                            swatch(ui, colors.resolve(&c));
                            if ui
                                .selectable_label(*value == Some(c), c.to_string())
                                .clicked()
                            {
                                *value = Some(c);
                            }
                        });
                    }
                }
                if ui.button("Custom colour\u{2026}").clicked() {
                    *value = Some(ColorRef::rgb(0x80, 0x80, 0x80));
                }
            });
        if let Some(ColorRef::Rgb(rgb)) = value {
            let mut arr = [rgb.r, rgb.g, rgb.b];
            if ui.color_edit_button_srgb(&mut arr).changed() {
                *value = Some(ColorRef::rgb(arr[0], arr[1], arr[2]));
            }
        }
    });
}

fn swatch(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, color);
}

/// Highlights `text` with `rules` (for tests and the preview).
pub fn preview_spans(rules: &[Rule], text: &str) -> usize {
    HighlightState::highlight_with(rules, text).spans.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_copies_the_rules_and_names_the_profile() {
        let mut e = RuleEditor::default();
        e.open_with(&[Rule::literal("a", "x")], Some("nginx"));
        assert!(e.open);
        assert_eq!(e.rules.len(), 1);
        assert_eq!(e.selected, Some(0));
        assert_eq!(e.profile_name, "nginx (custom)");
        e.open_with(&[], None);
        assert_eq!(e.selected, None);
        assert_eq!(e.profile_name, "My rules");
    }

    #[test]
    fn preview_uses_the_edited_rules() {
        let rules = vec![
            Rule::literal("w", "warn").styled(oxtail_highlight::Style::fg(ColorRef::solid(
                SemanticColor::Warn,
            ))),
        ];
        assert_eq!(preview_spans(&rules, "a warn b"), 1);
        assert_eq!(preview_spans(&[], "a warn b"), 0);
        assert_eq!(preview_spans(&[Rule::regex("bad", "(x")], "a warn b"), 0);
    }

    #[test]
    fn the_form_and_window_render_without_panicking() {
        let themes = oxtail_config::ThemeSet::builtin();
        let colors = Colors::from_theme(themes.get("dark").unwrap());
        let mut e = RuleEditor::default();
        e.open_with(&oxtail_highlight::presets::log_levels(), Some("Generic"));
        let mut rules_with_column = e.rules.clone();
        rules_with_column.push(Rule::new(
            "col",
            RuleMatcher::Column {
                column: "level".into(),
                op: ColumnOp::Ge,
                value: "3".into(),
            },
        ));
        e.open_with(&rules_with_column, Some("Generic"));
        let preview = vec!["2026 ERROR boom".to_string(), "plain".to_string()];
        let mut h = egui_kittest::Harness::new_ui_state(
            |ui, e: &mut RuleEditor| {
                let _ = e.contents(ui, &colors, &preview);
            },
            e,
        );
        h.run();
        // Every rule can be selected and rendered.
        for i in 0..h.state().rules.len() {
            h.state_mut().selected = Some(i);
            h.run();
        }
    }
}
