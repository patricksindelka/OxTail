//! The start screen shown when no tab is open: open buttons, recent files
//! (missing ones dimmed), the drop hint and a few key tips taken from the
//! active key map.
//!
//! Whether a recent file still exists is answered by a worker thread
//! (`OxTailApp::refresh_recent_status`); drawing only reads the answer.

use std::path::Path;

use egui::{Align, Color32, CornerRadius, Layout, Margin, RichText, Sense, Stroke, Ui, vec2};

use crate::app::OxTailApp;
use crate::keymap::Action;
use crate::request::OpenRequest;

/// Most recent files listed on the start screen.
pub const MAX_RECENT_SHOWN: usize = 8;
/// Width of the centered column.
const COLUMN_WIDTH: f32 = 560.0;

/// The tips: the action and what to say about it.
const TIPS: &[(Action, &str)] = &[
    (Action::Palette, "Search every command"),
    (Action::Find, "Find text or a regular expression"),
    (Action::Filter, "Show only matching lines"),
    (Action::GotoLine, "Jump to a line number"),
    (Action::ToggleFollow, "Follow the end of a growing file"),
    (Action::Settings, "Settings and key bindings"),
];

/// Splits a path into (file name, folder) for display.
pub fn split_display(path: &Path) -> (String, String) {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let dir = path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    (name, dir)
}

fn keycap(ui: &mut Ui, text: &str) {
    let v = ui.visuals();
    egui::Frame::new()
        .fill(v.faint_bg_color)
        .stroke(Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).monospace().small());
        });
}

fn dashed_rect(ui: &Ui, rect: egui::Rect, stroke: Stroke) {
    let r = rect.shrink(0.5);
    let pts = [
        r.left_top(),
        r.right_top(),
        r.right_bottom(),
        r.left_bottom(),
        r.left_top(),
    ];
    ui.painter()
        .extend(egui::Shape::dashed_line(&pts, stroke, 6.0, 4.0));
}

impl OxTailApp {
    /// Draws the start screen.
    pub(crate) fn welcome(&mut self, ui: &mut Ui) {
        let mut open: Option<std::path::PathBuf> = None;
        let mut act: Option<Action> = None;
        let recent: Vec<std::path::PathBuf> = self
            .session
            .recent_files
            .iter()
            .take(MAX_RECENT_SHOWN.min(self.settings.recent_files_limit.max(1)))
            .cloned()
            .collect();
        let tips: Vec<(String, &str)> = TIPS
            .iter()
            .filter_map(|(a, text)| {
                let chords = self.keymap.chords_for(*a);
                (!chords.is_empty()).then(|| {
                    let mac = self.keymap.is_mac();
                    let joined = chords
                        .iter()
                        .take(2)
                        .map(|c| c.format_for(mac))
                        .collect::<Vec<_>>()
                        .join("  or  ");
                    (joined, *text)
                })
            })
            .collect();
        let open_hint = self.keymap.shortcut_text(Action::Open);
        let paste_hint = self.keymap.shortcut_text(Action::PasteAsTab);
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let avail = ui.available_width();
                    let width = avail.min(COLUMN_WIDTH) - 8.0;
                    let left = ((avail - width) / 2.0).max(0.0);
                    ui.add_space((ui.available_height() * 0.10).clamp(16.0, 72.0));
                    ui.horizontal_top(|ui| {
                        ui.add_space(left);
                        ui.vertical(|ui| {
                            ui.set_width(width);
                            ui.label(RichText::new("OxTail").size(38.0).strong());
                            ui.label(
                                RichText::new("A fast, portable log viewer.")
                                    .size(15.0)
                                    .weak(),
                            );
                            ui.add_space(18.0);
                            // Ways to open.
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 10.0;
                                let size = vec2(150.0, 34.0);
                                let primary =
                                    egui::Button::new(RichText::new("Open file\u{2026}").strong())
                                        .min_size(size)
                                        .fill(ui.visuals().selection.bg_fill);
                                let mut r = ui.add(primary);
                                if let Some(h) = &open_hint {
                                    r = r.on_hover_text(h.clone());
                                }
                                if r.clicked() {
                                    act = Some(Action::Open);
                                }
                                if ui
                                    .add(egui::Button::new("Open folder\u{2026}").min_size(size))
                                    .on_hover_text("Open the files in a folder as tabs")
                                    .clicked()
                                {
                                    act = Some(Action::OpenFolder);
                                }
                                let mut r = ui
                                    .add(egui::Button::new("Paste from clipboard").min_size(size));
                                r = r.on_hover_text(match &paste_hint {
                                    Some(h) => format!("Open the clipboard text in a tab ({h})"),
                                    None => "Open the clipboard text in a tab".to_string(),
                                });
                                if r.clicked() {
                                    act = Some(Action::PasteAsTab);
                                }
                            });
                            ui.add_space(14.0);
                            // Drop target.
                            let (rect, _) = ui.allocate_exact_size(
                                vec2(ui.available_width(), 54.0),
                                Sense::hover(),
                            );
                            let weak = ui.visuals().weak_text_color();
                            dashed_rect(ui, rect, Stroke::new(1.0, weak.gamma_multiply(0.6)));
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "Drop files or folders anywhere in this window",
                                egui::FontId::proportional(14.0),
                                weak,
                            );
                            ui.add_space(22.0);
                            // Recent files.
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Recent files").strong().size(15.0));
                                if !recent.is_empty() {
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if ui.link("Clear").clicked() {
                                            act = Some(Action::ClearRecent);
                                        }
                                    });
                                }
                            });
                            ui.add_space(4.0);
                            if recent.is_empty() {
                                ui.label(RichText::new("Files you open will appear here.").weak());
                            }
                            for p in &recent {
                                let exists = self.recent_exists.get(p).copied().unwrap_or(true);
                                let (name, dir) = split_display(p);
                                let (rect, resp) = ui.allocate_exact_size(
                                    vec2(ui.available_width(), 42.0),
                                    if exists {
                                        Sense::click()
                                    } else {
                                        Sense::hover()
                                    },
                                );
                                resp.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::Button,
                                        exists,
                                        format!("Open recent file {name}"),
                                    )
                                });
                                let v = ui.visuals();
                                if resp.hovered() && exists {
                                    ui.painter().rect_filled(
                                        rect,
                                        CornerRadius::same(6),
                                        v.widgets.hovered.weak_bg_fill,
                                    );
                                }
                                let (fg, sub) = if exists {
                                    (v.text_color(), v.weak_text_color())
                                } else {
                                    (
                                        v.weak_text_color().gamma_multiply(0.75),
                                        v.weak_text_color().gamma_multiply(0.6),
                                    )
                                };
                                let painter = ui.painter();
                                let x = rect.left() + 10.0;
                                let g = painter.layout_no_wrap(
                                    name.clone(),
                                    egui::FontId::proportional(15.0),
                                    fg,
                                );
                                let name_w = g.size().x;
                                painter.galley(
                                    egui::pos2(x, rect.top() + 5.0),
                                    g,
                                    Color32::PLACEHOLDER,
                                );
                                if !exists {
                                    painter.text(
                                        egui::pos2(x + name_w + 8.0, rect.top() + 7.0),
                                        egui::Align2::LEFT_TOP,
                                        "not found",
                                        egui::FontId::proportional(12.0),
                                        sub,
                                    );
                                }
                                let dir_text = crate::util::shorten(&dir, 80);
                                painter.text(
                                    egui::pos2(x, rect.top() + 24.0),
                                    egui::Align2::LEFT_TOP,
                                    dir_text,
                                    egui::FontId::proportional(12.0),
                                    sub,
                                );
                                if resp.clicked() && exists {
                                    open = Some(p.clone());
                                }
                                if !exists {
                                    resp.on_hover_text("This file no longer exists");
                                }
                            }
                            ui.add_space(22.0);
                            // Key tips.
                            ui.label(RichText::new("Keyboard").strong().size(15.0));
                            ui.add_space(4.0);
                            egui::Grid::new("welcome-tips")
                                .num_columns(2)
                                .spacing([14.0, 8.0])
                                .show(ui, |ui| {
                                    for (chords, text) in &tips {
                                        ui.horizontal(|ui| {
                                            ui.set_min_width(190.0);
                                            for (i, c) in chords.split("  or  ").enumerate() {
                                                if i > 0 {
                                                    ui.label(RichText::new("or").weak().small());
                                                }
                                                keycap(ui, c);
                                            }
                                        });
                                        ui.label(*text);
                                        ui.end_row();
                                    }
                                });
                            ui.add_space(24.0);
                        });
                    });
                });
        });
        if let Some(a) = act {
            let ctx = ui.ctx().clone();
            self.perform(a, &ctx);
        }
        if let Some(p) = open {
            self.open_request(OpenRequest::file(p));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_split_for_display() {
        let (n, d) = split_display(Path::new("/var/log/nginx/access.log"));
        assert_eq!(n, "access.log");
        assert_eq!(d, "/var/log/nginx");
        let (n, d) = split_display(Path::new("access.log"));
        assert_eq!(n, "access.log");
        assert_eq!(d, "");
        let (n, _) = split_display(Path::new("/"));
        assert_eq!(n, "/");
    }
}
