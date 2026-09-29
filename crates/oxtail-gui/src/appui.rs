//! Drawing the application: menu bar, tab bar, notices, the active tab and
//! the dialog windows.

use std::time::Instant;

use egui::{Align2, Context, CornerRadius, Id, RichText, Sense, Ui, ViewportCommand, vec2};
use oxtail_config::ThemeChoice;

use crate::app::{OxTailApp, Windows};
use crate::keymap::Action;
use crate::logview::{self, ViewEnv};
use crate::panels::{self, StatusAction};
use crate::request::OpenRequest;
use crate::tab::{TabContent, drop_index, move_item};

impl OxTailApp {
    /// Draws one frame into `ui`.
    pub fn show(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.ensure_started(&ctx);
        self.pre_frame(&ctx);
        self.update_title(&ctx);

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui, &ctx));
        egui::Panel::top("tabs").show(ui, |ui| self.tabs_bar(ui));
        self.notices_ui(ui);
        self.body(ui);
        self.windows_ui(&ctx);
        drop_overlay(&ctx);
    }

    fn update_title(&mut self, ctx: &Context) {
        let title = match self.active_tab() {
            Some(t) => format!("{} \u{2013} OxTail", t.label()),
            None => "OxTail".to_string(),
        };
        if title != self.last_title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }

    // ------------------------------------------------------------- menu bar

    fn menu_bar(&mut self, ui: &mut Ui, ctx: &Context) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui
                    .add(egui::Button::new("Open\u{2026}").shortcut_text("Ctrl+O"))
                    .clicked()
                {
                    self.perform(Action::Open, ctx);
                    ui.close();
                }
                let recent = self.session.recent_files.clone();
                ui.add_enabled_ui(!recent.is_empty(), |ui| {
                    ui.menu_button("Open recent", |ui| {
                        for p in recent.iter().take(self.settings.recent_files_limit.max(1)) {
                            let name = p.display().to_string();
                            if ui.button(crate::util::shorten(&name, 70)).clicked() {
                                self.open_request(OpenRequest::file(p.clone()));
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button("Clear list").clicked() {
                            self.session.recent_files.clear();
                            ui.close();
                        }
                    });
                });
                ui.separator();
                if ui
                    .add_enabled(
                        !self.tabs.is_empty(),
                        egui::Button::new("Close tab").shortcut_text("Ctrl+W"),
                    )
                    .clicked()
                {
                    self.perform(Action::CloseTab, ctx);
                    ui.close();
                }
                ui.separator();
                if ui.button("Settings\u{2026}").clicked() {
                    self.windows.settings = true;
                    ui.close();
                }
                if ui.button("Quit").clicked() {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                    ui.close();
                }
            });
            let has_view = self.active_view().is_some();
            ui.menu_button("Edit", |ui| {
                ui.add_enabled_ui(has_view, |ui| {
                    if ui
                        .add(egui::Button::new("Copy").shortcut_text("Ctrl+C"))
                        .clicked()
                    {
                        self.perform(Action::Copy, ctx);
                        ui.close();
                    }
                    if ui
                        .add(
                            egui::Button::new("Copy with line numbers")
                                .shortcut_text("Ctrl+Shift+C"),
                        )
                        .clicked()
                    {
                        self.perform(Action::CopyWithNumbers, ctx);
                        ui.close();
                    }
                    if ui
                        .add(egui::Button::new("Select all").shortcut_text("Ctrl+A"))
                        .clicked()
                    {
                        self.perform(Action::SelectAll, ctx);
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .add(egui::Button::new("Find\u{2026}").shortcut_text("Ctrl+F"))
                        .clicked()
                    {
                        self.perform(Action::Find, ctx);
                        ui.close();
                    }
                    if ui
                        .add(egui::Button::new("Filter view\u{2026}").shortcut_text("Ctrl+Shift+F"))
                        .clicked()
                    {
                        self.perform(Action::Filter, ctx);
                        ui.close();
                    }
                    if ui
                        .add(egui::Button::new("Go to line\u{2026}").shortcut_text("Ctrl+G"))
                        .clicked()
                    {
                        self.perform(Action::GotoLine, ctx);
                        ui.close();
                    }
                    ui.separator();
                    ui.menu_button("Bookmarks", |ui| {
                        if ui
                            .add(egui::Button::new("Toggle bookmark").shortcut_text("Ctrl+F2"))
                            .clicked()
                        {
                            self.perform(Action::ToggleBookmark, ctx);
                            ui.close();
                        }
                        if ui
                            .add(egui::Button::new("Next bookmark").shortcut_text("F2"))
                            .clicked()
                        {
                            self.perform(Action::NextBookmark, ctx);
                            ui.close();
                        }
                        if ui
                            .add(egui::Button::new("Previous bookmark").shortcut_text("Shift+F2"))
                            .clicked()
                        {
                            self.perform(Action::PrevBookmark, ctx);
                            ui.close();
                        }
                    });
                    if ui
                        .add(egui::Button::new("Add mark").shortcut_text("Ctrl+M"))
                        .clicked()
                    {
                        self.perform(Action::Mark, ctx);
                        ui.close();
                    }
                });
            });
            ui.menu_button("View", |ui| {
                ui.add_enabled_ui(has_view, |ui| {
                    let (follow, wrap, hidden_toggle, ansi) = match self.active_view() {
                        Some(v) => (
                            v.follow,
                            v.wrap,
                            v.hl.borrow().hide.is_some(),
                            v.hl.borrow().show_ansi,
                        ),
                        None => (false, false, false, true),
                    };
                    let mut f = follow;
                    if ui
                        .add(egui::Checkbox::new(&mut f, "Follow").indeterminate(false))
                        .on_hover_text("F")
                        .changed()
                    {
                        self.perform(Action::ToggleFollow, ctx);
                    }
                    let mut w = wrap;
                    if ui
                        .checkbox(&mut w, "Wrap lines")
                        .on_hover_text("Alt+Z")
                        .changed()
                    {
                        self.set_wrap(w);
                    }
                    let mut a = ansi;
                    if ui.checkbox(&mut a, "Render ANSI colours").changed()
                        && let Some(v) = self.active_view_mut()
                    {
                        v.hl.borrow_mut().set_show_ansi(a);
                        v.galleys.borrow_mut().clear();
                    }
                    if hidden_toggle {
                        let mut show = self.active_view().is_some_and(|v| v.filter.show_hidden);
                        if ui.checkbox(&mut show, "Show hidden lines").changed()
                            && let Some(v) = self.active_view_mut()
                        {
                            v.filter.show_hidden = show;
                            v.filter.changed();
                        }
                    }
                });
                let mut ln = self.settings.line_numbers;
                if ui.checkbox(&mut ln, "Line numbers").changed() {
                    self.settings.line_numbers = ln;
                    self.mark_settings_dirty();
                }
                ui.separator();
                if ui
                    .add(egui::Button::new("Zoom in").shortcut_text("Ctrl+="))
                    .clicked()
                {
                    self.perform(Action::ZoomIn, ctx);
                }
                if ui
                    .add(egui::Button::new("Zoom out").shortcut_text("Ctrl+-"))
                    .clicked()
                {
                    self.perform(Action::ZoomOut, ctx);
                }
                if ui
                    .add(egui::Button::new("Reset zoom").shortcut_text("Ctrl+0"))
                    .clicked()
                {
                    self.perform(Action::ZoomReset, ctx);
                }
                ui.separator();
                if ui
                    .add_enabled(has_view, egui::Button::new("Highlight rules\u{2026}"))
                    .clicked()
                {
                    self.open_rule_editor();
                    ui.close();
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Keyboard shortcuts").clicked() {
                    self.windows.shortcuts = true;
                    ui.close();
                }
                if ui.button("About OxTail").clicked() {
                    self.windows.about = true;
                    ui.close();
                }
            });
        });
    }

    // -------------------------------------------------------------- tab bar

    fn tabs_bar(&mut self, ui: &mut Ui) {
        let colors = self.colors.clone();
        let mut select: Option<usize> = None;
        let mut close: Option<usize> = None;
        let mut reorder: Option<(usize, f32)> = None;
        let mut centers: Vec<f32> = Vec::new();
        egui::ScrollArea::horizontal()
            .id_salt("tab-scroll")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for (i, tab) in self.tabs.iter().enumerate() {
                        let selected = i == self.active;
                        let label = tab.label();
                        let font = egui::FontId::proportional(13.0);
                        let galley = ui.painter().layout_no_wrap(
                            label,
                            font,
                            if selected {
                                colors.text
                            } else {
                                colors.status_text
                            },
                        );
                        let close_w = 18.0;
                        let size = vec2(galley.size().x + 22.0 + close_w, 26.0);
                        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
                        centers.push(rect.center().x);
                        let bg = if selected {
                            colors.background
                        } else if resp.hovered() {
                            colors.current_line_bg
                        } else {
                            colors.gutter_bg
                        };
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius {
                                nw: 4,
                                ne: 4,
                                sw: 0,
                                se: 0,
                            },
                            bg,
                        );
                        if selected {
                            ui.painter().hline(
                                rect.x_range(),
                                rect.top() + 1.0,
                                egui::Stroke::new(2.0, colors.accent),
                            );
                        }
                        let text_pos =
                            egui::pos2(rect.left() + 10.0, rect.center().y - galley.size().y * 0.5);
                        ui.painter().galley(text_pos, galley, colors.text);
                        // Close button: an x drawn with two lines.
                        let close_rect = egui::Rect::from_center_size(
                            egui::pos2(rect.right() - close_w * 0.5 - 4.0, rect.center().y),
                            vec2(16.0, 16.0),
                        );
                        let close_resp =
                            ui.interact(close_rect, Id::new(("tab-close", tab.id)), Sense::click());
                        let c = if close_resp.hovered() {
                            colors.text
                        } else {
                            colors.gutter_text
                        };
                        if close_resp.hovered() {
                            ui.painter().rect_filled(
                                close_rect,
                                CornerRadius::same(3),
                                colors.selection_bg,
                            );
                        }
                        let m = 4.5;
                        let ctr = close_rect.center();
                        let stroke = egui::Stroke::new(1.3, c);
                        ui.painter()
                            .line_segment([ctr + vec2(-m, -m), ctr + vec2(m, m)], stroke);
                        ui.painter()
                            .line_segment([ctr + vec2(-m, m), ctr + vec2(m, -m)], stroke);
                        if close_resp.clicked() || resp.middle_clicked() {
                            close = Some(i);
                        } else if resp.clicked() {
                            select = Some(i);
                        }
                        if resp.drag_started() {
                            select = Some(i);
                        }
                        if resp.drag_stopped()
                            && let Some(p) = ui.input(|i| i.pointer.interact_pos())
                        {
                            reorder = Some((i, p.x));
                        }
                        let resp = match &tab.path {
                            Some(p) => resp.on_hover_text(p.display().to_string()),
                            None => resp,
                        };
                        let _ = resp;
                    }
                    if ui
                        .add(egui::Button::new("+").frame(false))
                        .on_hover_text("Open a file (Ctrl+O)")
                        .clicked()
                    {
                        self.file_dialog_requested = true;
                    }
                });
            });
        if let Some(i) = select {
            self.select_tab(i);
        }
        if let Some((from, x)) = reorder {
            let to = drop_index(&centers, x);
            let new_index = move_item(&mut self.tabs, from, to);
            if self.active == from {
                self.active = new_index;
            } else if from < self.active && new_index >= self.active {
                self.active -= 1;
            } else if from > self.active && new_index <= self.active {
                self.active += 1;
            }
        }
        if let Some(i) = close {
            self.close_tab(i);
        }
        if std::mem::take(&mut self.file_dialog_requested) {
            self.pick_files();
        }
    }

    fn notices_ui(&mut self, ui: &mut Ui) {
        if self.notices.is_empty() {
            return;
        }
        let colors = self.colors.clone();
        let mut dismiss: Option<usize> = None;
        egui::Panel::top("notices").show(ui, |ui| {
            for (i, n) in self.notices.iter().enumerate() {
                ui.horizontal(|ui| {
                    let text = RichText::new(&n.text);
                    let text = if n.error {
                        text.color(colors.resolve(&oxtail_highlight::ColorRef::solid(
                            oxtail_highlight::SemanticColor::Warn,
                        )))
                    } else {
                        text
                    };
                    ui.label(text);
                    if ui.small_button("Dismiss").clicked() {
                        dismiss = Some(i);
                    }
                });
            }
        });
        if let Some(i) = dismiss {
            self.notices.remove(i);
        }
    }

    // ----------------------------------------------------------------- body

    fn body(&mut self, ui: &mut Ui) {
        enum State {
            None,
            Opening,
            Failed(String),
            Ready,
        }
        let state = match self.tabs.get(self.active).map(|t| &t.content) {
            None => State::None,
            Some(TabContent::Opening(_)) => State::Opening,
            Some(TabContent::Failed(m)) => State::Failed(m.clone()),
            Some(TabContent::Ready(_)) => State::Ready,
        };
        match state {
            State::None => self.welcome(ui),
            State::Opening => {
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label(RichText::new("Opening\u{2026}").size(18.0));
                    });
                });
            }
            State::Failed(msg) => {
                let mut close = false;
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(60.0);
                        ui.heading("Could not open the file");
                        ui.label(&msg);
                        ui.add_space(8.0);
                        if ui.button("Close tab").clicked() {
                            close = true;
                        }
                    });
                });
                if close {
                    self.close_tab(self.active);
                }
            }
            State::Ready => self.tab_ui(ui),
        }
    }

    fn welcome(&mut self, ui: &mut Ui) {
        let mut open: Option<std::path::PathBuf> = None;
        let mut pick = false;
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(80.0);
                ui.heading("OxTail");
                ui.label("Open a log file with Ctrl+O, or drop one onto this window.");
                ui.add_space(8.0);
                if ui.button("Open file\u{2026}").clicked() {
                    pick = true;
                }
                if !self.session.recent_files.is_empty() {
                    ui.add_space(16.0);
                    ui.label(RichText::new("Recent files").strong());
                    for p in self.session.recent_files.iter().take(10) {
                        if ui
                            .link(crate::util::shorten(&p.display().to_string(), 90))
                            .clicked()
                        {
                            open = Some(p.clone());
                        }
                    }
                }
            });
        });
        if pick {
            self.pick_files();
        }
        if let Some(p) = open {
            self.open_request(OpenRequest::file(p));
        }
    }

    fn tab_ui(&mut self, ui: &mut Ui) {
        let now = Instant::now();
        let mut status_action = StatusAction::None;
        let mut history_changed = false;
        {
            let Self {
                tabs,
                active,
                colors,
                settings,
                profiles,
                history,
                style_epoch,
                ..
            } = self;
            let Some(tab) = tabs.get_mut(*active) else {
                return;
            };
            let tab_id = tab.id;
            let TabContent::Ready(view) = &mut tab.content else {
                return;
            };
            if view.find.open {
                egui::Panel::top("find-bar").show(ui, |ui| {
                    history_changed = panels::find_bar(ui, tab_id, view, history, colors, now);
                });
            }
            if view.filter.open {
                egui::Panel::top("filter-panel").show(ui, |ui| {
                    panels::filter_panel(ui, tab_id, view, colors, now);
                });
            }
            if view.banner.is_some() {
                egui::Panel::top("banner").show(ui, |ui| {
                    panels::banner(ui, view, colors);
                });
            }
            egui::Panel::bottom("status").show(ui, |ui| {
                status_action = panels::status_bar(ui, view, profiles);
            });
            let env = ViewEnv {
                colors,
                font_size: settings.font_size,
                line_height: settings.line_height,
                line_numbers: settings.line_numbers,
                minimap: true,
                style_epoch: *style_epoch,
            };
            egui::CentralPanel::no_frame().show(ui, |ui| {
                logview::show(ui, Id::new(("log", tab_id)), view, &env);
            });
        }
        if history_changed {
            self.mark_history_dirty();
        }
        match status_action {
            StatusAction::None => {}
            StatusAction::SetProfile(p) => self.set_profile(p),
            StatusAction::EditRules => self.open_rule_editor(),
        }
    }

    // -------------------------------------------------------------- windows

    fn windows_ui(&mut self, ctx: &Context) {
        self.goto_window(ctx);
        self.bookmark_window(ctx);
        self.settings_window(ctx);
        self.shortcuts_window(ctx);
        self.about_window(ctx);
        if self.rule_editor.open {
            let preview: Vec<String> = self
                .active_view()
                .map(|v| v.last_rows.iter().map(|l| l.text.clone()).collect())
                .unwrap_or_default();
            let colors = self.colors.clone();
            let action = self.rule_editor.show(ctx, &colors, &preview);
            self.handle_editor_action(action);
        }
    }

    fn goto_window(&mut self, ctx: &Context) {
        let Some(mut dlg) = self.windows.goto.take() else {
            return;
        };
        let mut close = false;
        let mut apply = false;
        egui::Window::new("Go to line")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_TOP, [0.0, 90.0])
            .show(ctx, |ui| {
                ui.label("Line number, +N / -N relative to the current line, or N%");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut dlg.text)
                        .hint_text("e.g. 1234, +100, 50%")
                        .desired_width(240.0),
                );
                if dlg.focus {
                    resp.request_focus();
                    dlg.focus = false;
                }
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    apply = true;
                }
                if let Some(e) = &dlg.error {
                    ui.label(RichText::new(e).color(egui::Color32::LIGHT_RED));
                }
                ui.horizontal(|ui| {
                    if ui.button("Go").clicked() {
                        apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        if apply {
            match self.apply_goto(&dlg.text) {
                Ok(()) => close = true,
                Err(e) => dlg.error = Some(e),
            }
        }
        if !close {
            self.windows.goto = Some(dlg);
        }
    }

    fn bookmark_window(&mut self, ctx: &Context) {
        let Some(view) = self.active_view_mut() else {
            return;
        };
        let Some(n) = view.editing_bookmark else {
            return;
        };
        let mut label = view
            .bookmarks
            .get(&n)
            .map(|b| b.label.clone())
            .unwrap_or_default();
        let mut done = false;
        let mut remove = false;
        egui::Window::new(format!("Bookmark on line {}", n + 1))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_TOP, [0.0, 90.0])
            .show(ctx, |ui| {
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut label)
                        .hint_text("label (optional)")
                        .desired_width(260.0),
                );
                resp.request_focus();
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    done = true;
                }
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        done = true;
                    }
                    if ui.button("Remove bookmark").clicked() {
                        remove = true;
                    }
                });
            });
        if let Some(b) = view.bookmarks.get_mut(&n) {
            b.label = label;
        }
        if remove {
            view.bookmarks.remove(&n);
            view.editing_bookmark = None;
        } else if done {
            view.editing_bookmark = None;
        }
    }

    fn settings_window(&mut self, ctx: &Context) {
        if !self.windows.settings {
            return;
        }
        let mut open = true;
        let mut changed_theme = false;
        let mut font = self.settings.font_size;
        let mut line_height = self.settings.line_height;
        let mut theme = self.settings.theme;
        let mut wrap = self.settings.wrap;
        let mut numbers = self.settings.line_numbers;
        let mut notifications = self.settings.notifications_enabled;
        let theme_names: Vec<String> = self
            .themes
            .themes()
            .iter()
            .map(|t| t.name.clone())
            .collect();
        let mut custom = self.settings.custom_theme.clone();
        egui::Window::new("Settings")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("settings-grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Theme");
                        egui::ComboBox::from_id_salt("theme")
                            .selected_text(match theme {
                                ThemeChoice::System => "Follow system",
                                ThemeChoice::Dark => "Dark",
                                ThemeChoice::Light => "Light",
                                ThemeChoice::HighContrast => "High contrast",
                                ThemeChoice::Custom => "Custom",
                            })
                            .show_ui(ui, |ui| {
                                changed_theme |= ui
                                    .selectable_value(
                                        &mut theme,
                                        ThemeChoice::System,
                                        "Follow system",
                                    )
                                    .changed();
                                changed_theme |= ui
                                    .selectable_value(&mut theme, ThemeChoice::Dark, "Dark")
                                    .changed();
                                changed_theme |= ui
                                    .selectable_value(&mut theme, ThemeChoice::Light, "Light")
                                    .changed();
                                changed_theme |= ui
                                    .selectable_value(
                                        &mut theme,
                                        ThemeChoice::HighContrast,
                                        "High contrast",
                                    )
                                    .changed();
                                changed_theme |= ui
                                    .selectable_value(&mut theme, ThemeChoice::Custom, "Custom")
                                    .changed();
                            });
                        ui.end_row();
                        if theme == ThemeChoice::Custom {
                            ui.label("Custom theme");
                            egui::ComboBox::from_id_salt("custom-theme")
                                .selected_text(&custom)
                                .show_ui(ui, |ui| {
                                    for n in &theme_names {
                                        changed_theme |= ui
                                            .selectable_value(&mut custom, n.clone(), n)
                                            .changed();
                                    }
                                });
                            ui.end_row();
                        }
                        ui.label("Font size");
                        ui.add(egui::Slider::new(&mut font, 6.0..=48.0).suffix(" pt"));
                        ui.end_row();
                        ui.label("Line height");
                        ui.add(egui::Slider::new(&mut line_height, 1.0..=2.5));
                        ui.end_row();
                        ui.label("Wrap long lines");
                        ui.checkbox(&mut wrap, "");
                        ui.end_row();
                        ui.label("Line numbers");
                        ui.checkbox(&mut numbers, "");
                        ui.end_row();
                        ui.label("Desktop notifications for alert rules");
                        ui.checkbox(&mut notifications, "");
                        ui.end_row();
                    });
                ui.separator();
                ui.label(
                    RichText::new(match &self.data_dir.root {
                        Some(r) => format!("Data folder: {}", r.display()),
                        None => "Data folder: none (settings are not saved)".to_string(),
                    })
                    .weak(),
                );
            });
        let mut dirty = false;
        if (font - self.settings.font_size).abs() > f32::EPSILON {
            self.set_font_size(font);
        }
        if (line_height - self.settings.line_height).abs() > f32::EPSILON {
            self.settings.line_height = line_height;
            dirty = true;
        }
        if wrap != self.settings.wrap {
            self.set_wrap(wrap);
        }
        if numbers != self.settings.line_numbers {
            self.settings.line_numbers = numbers;
            dirty = true;
        }
        if notifications != self.settings.notifications_enabled {
            self.settings.notifications_enabled = notifications;
            dirty = true;
        }
        if changed_theme {
            self.settings.theme = theme;
            self.settings.custom_theme = custom;
            self.apply_theme(ctx);
            dirty = true;
        }
        if dirty {
            self.bump_style();
            self.mark_settings_dirty();
        }
        self.windows.settings = open;
    }

    fn shortcuts_window(&mut self, ctx: &Context) {
        if !self.windows.shortcuts {
            return;
        }
        let mut open = true;
        egui::Window::new("Keyboard shortcuts")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("shortcuts").num_columns(2).show(ui, |ui| {
                    for (k, d) in SHORTCUTS {
                        ui.label(RichText::new(*k).monospace());
                        ui.label(*d);
                        ui.end_row();
                    }
                });
            });
        self.windows.shortcuts = open;
    }

    fn about_window(&mut self, ctx: &Context) {
        if !self.windows.about {
            return;
        }
        let mut open = true;
        egui::Window::new("About OxTail")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("OxTail");
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.label("A portable, real-time log viewer.");
            });
        self.windows.about = open;
    }

    pub(crate) fn mark_history_dirty(&mut self) {
        self.history_dirty = true;
    }
}

/// Shortcuts listed in the help window.
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl+O", "Open a file"),
    ("Ctrl+W", "Close the tab"),
    ("Ctrl+Tab / Ctrl+Shift+Tab", "Next / previous tab"),
    ("Ctrl+F", "Find"),
    ("F3 / Shift+F3", "Next / previous match"),
    (
        "Enter / Shift+Enter",
        "Next / previous match (in the find field)",
    ),
    ("Ctrl+Shift+F", "Filter view"),
    ("Ctrl+G", "Go to line (123, +100, -50, 50%)"),
    (
        "Ctrl+F2 / F2 / Shift+F2",
        "Toggle / next / previous bookmark",
    ),
    ("Ctrl+M", "Insert a mark after the last line"),
    (
        "Ctrl+C / Ctrl+Shift+C",
        "Copy selected lines / with line numbers",
    ),
    ("Ctrl+A", "Select all lines"),
    ("F", "Toggle following"),
    ("Home / End", "First line / last line (and follow)"),
    ("PageUp / PageDown", "One page up / down"),
    ("Up / Down / Left / Right", "Scroll"),
    ("Alt+Z", "Wrap lines"),
    (
        "Ctrl+= / Ctrl+- / Ctrl+0",
        "Zoom in / out / reset (also Ctrl+wheel)",
    ),
    ("Esc", "Close find bar or dialog, clear selection"),
];

fn drop_overlay(ctx: &Context) {
    let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
    if !hovering {
        return;
    }
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        Id::new("drop-overlay"),
    ));
    let rect = ctx.content_rect();
    painter.rect_filled(
        rect,
        CornerRadius::ZERO,
        egui::Color32::from_black_alpha(160),
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        "Drop files to open them",
        egui::FontId::proportional(24.0),
        egui::Color32::WHITE,
    );
}

/// Windows that exist, for tests.
pub fn any_window_open(w: &Windows) -> bool {
    w.goto.is_some() || w.settings || w.shortcuts || w.about
}
