//! Drawing the application: menu bar, tab bar, notices, the active tab and
//! the dialog windows.

use std::time::Instant;

use egui::{Align2, Context, CornerRadius, Id, RichText, Ui, ViewportCommand};

use crate::app::{OxTailApp, Windows};
use crate::colui::{self, SuggestionAction};
use crate::keymap::Action;
use crate::logview::{self, ViewEnv};
use crate::panels::{self, StatusAction};
use crate::request::OpenRequest;
use crate::tab::{Tab, TabContent};

impl OxTailApp {
    /// Draws one frame into `ui`.
    pub fn show(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.ensure_started(&ctx);
        self.pre_frame(&ctx);
        self.update_title(&ctx);

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui, &ctx));
        if self.pane_count() <= 1 {
            let pane = self.panes.panes().first().copied().unwrap_or(0);
            egui::Panel::top("tabs").show(ui, |ui| self.tabs_strip(ui, pane));
        }
        self.notices_ui(ui);
        self.body(ui);
        self.drop_overlay_for_tabs(&ctx);
        self.windows_ui(&ctx);
        drop_overlay(&ctx);
        self.update_pacing(&ctx);
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

    /// A menu entry that runs `action`: label, shortcut from the key map,
    /// greyed out when it does not apply.
    fn menu_item(&mut self, ui: &mut Ui, ctx: &Context, action: Action) {
        let mut button = egui::Button::new(action.menu_label());
        if let Some(s) = self.keymap.shortcut_text(action) {
            button = button.shortcut_text(s);
        }
        if ui
            .add_enabled(self.action_enabled(action), button)
            .clicked()
        {
            self.perform(action, ctx);
            ui.close();
        }
    }

    /// A menu check box for a toggle action.
    fn menu_check(&mut self, ui: &mut Ui, ctx: &Context, action: Action) {
        let mut on = self.action_checked(action).unwrap_or(false);
        let resp = ui.add_enabled(
            self.action_enabled(action),
            egui::Checkbox::new(&mut on, action.menu_label()),
        );
        let resp = match self.keymap.shortcut_text(action) {
            Some(s) => resp.on_hover_text(s),
            None => resp,
        };
        if resp.changed() {
            self.perform(action, ctx);
        }
    }

    /// A menu radio entry for a choice action.
    fn menu_radio(&mut self, ui: &mut Ui, ctx: &Context, action: Action) {
        let on = self.action_checked(action).unwrap_or(false);
        if ui
            .add_enabled(
                self.action_enabled(action),
                egui::RadioButton::new(on, action.menu_label()),
            )
            .clicked()
        {
            self.perform(action, ctx);
            ui.close();
        }
    }

    fn menu_bar(&mut self, ui: &mut Ui, ctx: &Context) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                self.menu_item(ui, ctx, Action::Open);
                self.menu_item(ui, ctx, Action::OpenFolder);
                let recent = self.session.recent_files.clone();
                ui.add_enabled_ui(!recent.is_empty(), |ui| {
                    ui.menu_button("Open recent", |ui| {
                        for p in recent.iter().take(self.settings.recent_files_limit.max(1)) {
                            let name = p.display().to_string();
                            let exists = self.recent_exists.get(p).copied().unwrap_or(true);
                            if ui
                                .add_enabled(
                                    exists,
                                    egui::Button::new(crate::util::shorten(&name, 70)),
                                )
                                .clicked()
                            {
                                self.open_request(OpenRequest::file(p.clone()));
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button("Clear list").clicked() {
                            self.perform(Action::ClearRecent, ctx);
                            ui.close();
                        }
                    });
                });
                if ui
                    .add(
                        egui::Button::new("Open clipboard text").shortcut_text(
                            self.keymap
                                .shortcut_text(Action::PasteAsTab)
                                .unwrap_or_default(),
                        ),
                    )
                    .on_hover_text("Open the text on the clipboard in a new tab")
                    .clicked()
                {
                    self.perform(Action::PasteAsTab, ctx);
                    ui.close();
                }
                ui.separator();
                self.menu_item(ui, ctx, Action::CloseTab);
                self.menu_item(ui, ctx, Action::CloseOtherTabs);
                ui.separator();
                self.menu_item(ui, ctx, Action::Settings);
                self.menu_item(ui, ctx, Action::Quit);
            });
            ui.menu_button("Edit", |ui| {
                self.menu_item(ui, ctx, Action::Copy);
                self.menu_item(ui, ctx, Action::CopyWithNumbers);
                self.menu_item(ui, ctx, Action::SelectAll);
                ui.separator();
                self.menu_item(ui, ctx, Action::Find);
                self.menu_item(ui, ctx, Action::Filter);
                self.menu_item(ui, ctx, Action::GotoLine);
                self.menu_item(ui, ctx, Action::GotoTime);
                self.menu_item(ui, ctx, Action::FindInTabs);
                ui.separator();
                ui.menu_button("Bookmarks", |ui| {
                    self.menu_item(ui, ctx, Action::ToggleBookmark);
                    self.menu_item(ui, ctx, Action::NextBookmark);
                    self.menu_item(ui, ctx, Action::PrevBookmark);
                });
                self.menu_item(ui, ctx, Action::Mark);
            });
            ui.menu_button("View", |ui| {
                self.menu_check(ui, ctx, Action::ToggleFollow);
                self.menu_check(ui, ctx, Action::ToggleWrap);
                self.menu_check(ui, ctx, Action::ToggleAnsi);
                self.menu_check(ui, ctx, Action::ToggleHidden);
                self.menu_check(ui, ctx, Action::ToggleLineNumbers);
                self.menu_check(ui, ctx, Action::ToggleMinimap);
                self.columns_menu(ui, ctx);
                self.menu_check(ui, ctx, Action::ToggleTimeGaps);
                ui.separator();
                self.layout_menu(ui);
                ui.separator();
                self.menu_item(ui, ctx, Action::ZoomIn);
                self.menu_item(ui, ctx, Action::ZoomOut);
                self.menu_item(ui, ctx, Action::ZoomReset);
                ui.menu_button("Theme", |ui| {
                    self.menu_radio(ui, ctx, Action::ThemeSystem);
                    self.menu_radio(ui, ctx, Action::ThemeDark);
                    self.menu_radio(ui, ctx, Action::ThemeLight);
                    self.menu_radio(ui, ctx, Action::ThemeHighContrast);
                });
                ui.separator();
                self.menu_item(ui, ctx, Action::EditRules);
            });
            ui.menu_button("Help", |ui| {
                self.menu_item(ui, ctx, Action::Palette);
                self.menu_item(ui, ctx, Action::Shortcuts);
                ui.separator();
                self.menu_item(ui, ctx, Action::CheckUpdates);
                self.menu_item(ui, ctx, Action::About);
            });
        });
    }

    /// The "Columns" submenu of the View menu.
    fn columns_menu(&mut self, ui: &mut Ui, ctx: &Context) {
        if self.active_view().is_none() {
            return;
        }
        ui.menu_button("Columns", |ui| {
            self.menu_check(ui, ctx, Action::ToggleColumns);
            self.menu_item(ui, ctx, Action::ChooseParser);
            self.menu_check(ui, ctx, Action::ToggleDetail);
            ui.separator();
            ui.menu_button("Relative time", |ui| {
                self.menu_radio(ui, ctx, Action::RelTimeOff);
                self.menu_radio(ui, ctx, Action::RelTimePrevious);
                self.menu_radio(ui, ctx, Action::RelTimeSelected);
            });
            self.menu_item(ui, ctx, Action::Stats);
            self.menu_item(ui, ctx, Action::Export);
            if let Some(v) = self.active_view()
                && v.st.parser.is_some()
            {
                ui.separator();
                ui.label(RichText::new(format!("Format: {}", v.st.name)).weak());
            }
        });
    }

    // -------------------------------------------------------------- tab bar

    fn update_notice_ui(&mut self, ui: &mut Ui) {
        let Some(release) = self.sys.notice.clone() else {
            return;
        };
        let mut dismiss = false;
        let mut open_page = false;
        egui::Panel::top("update-notice").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("\u{2b06}").strong());
                ui.label(format!(
                    "OxTail {} is available (you have {}).",
                    release.version,
                    env!("CARGO_PKG_VERSION")
                ));
                if crate::sysint::is_web_url(&release.url) && ui.link("Release page").clicked() {
                    open_page = true;
                }
                if ui.small_button("Dismiss").clicked() {
                    dismiss = true;
                }
            });
        });
        if open_page {
            ui.ctx().open_url(egui::OpenUrl::new_tab(release.url));
        }
        if dismiss {
            self.sys.notice = None;
        }
    }

    fn notices_ui(&mut self, ui: &mut Ui) {
        self.update_notice_ui(ui);
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
        if self.tabs.is_empty() {
            self.pane_rects.clear();
            self.welcome(ui);
            return;
        }
        let panes = self.panes.panes();
        if panes.len() <= 1 {
            let rect = ui.available_rect_before_wrap();
            self.pane_rects = vec![(panes.first().copied().unwrap_or(0), rect)];
            self.tab_body(ui, self.active);
        } else {
            self.body_panes(ui);
        }
    }

    /// The content of tab `index`: opening message, error, log view or merged
    /// view.
    pub(crate) fn tab_body(&mut self, ui: &mut Ui, index: usize) {
        enum State {
            None,
            Opening,
            Failed(String),
            Ready,
            Merged,
        }
        let state = match self.tabs.get(index).map(|t| &t.content) {
            None => State::None,
            Some(TabContent::Opening(_)) => State::Opening,
            Some(TabContent::Failed(m)) => State::Failed(m.clone()),
            Some(TabContent::Ready(_)) => State::Ready,
            Some(TabContent::Merged(_)) => State::Merged,
        };
        match state {
            State::None => {}
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
                    self.close_tab(index);
                }
            }
            State::Ready => self.tab_ui(ui, index),
            State::Merged => self.merged_ui(ui, index),
        }
    }

    fn tab_ui(&mut self, ui: &mut Ui, index: usize) {
        let now = Instant::now();
        let mut status_action = StatusAction::None;
        let mut history_changed = false;
        let mut suggestion = SuggestionAction::None;
        let mut copy: Option<String> = None;
        let wrap_key = self.keymap.shortcut_text(Action::ToggleWrap);
        {
            let Self {
                tabs,
                colors,
                settings,
                profiles,
                history,
                style_epoch,
                pacer,
                ..
            } = self;
            let Some(tab) = tabs.get_mut(index) else {
                return;
            };
            let tab_id = tab.id;
            let TabContent::Ready(view) = &mut tab.content else {
                return;
            };
            if view.find.open {
                egui::Panel::top(Id::new(("find-bar", tab_id))).show(ui, |ui| {
                    history_changed = panels::find_bar(ui, tab_id, view, history, colors, now);
                });
            }
            if view.filter.open {
                egui::Panel::top(Id::new(("filter-panel", tab_id))).show(ui, |ui| {
                    panels::filter_panel(ui, tab_id, view, colors, now);
                });
            }
            if view.st.suggestion.is_some() {
                egui::Panel::top(Id::new(("suggest", tab_id))).show(ui, |ui| {
                    suggestion = colui::suggestion_bar(ui, view, colors);
                });
            }
            if view.banner.is_some() {
                egui::Panel::top(Id::new(("banner", tab_id))).show(ui, |ui| {
                    panels::banner(ui, view, colors);
                });
            }
            egui::Panel::bottom(Id::new(("status", tab_id))).show(ui, |ui| {
                status_action = panels::status_bar(ui, view, profiles, wrap_key.as_deref(), colors);
            });
            if view.detail_open && view.st.parser.is_some() {
                egui::Panel::bottom(Id::new(("detail", tab_id)))
                    .resizable(true)
                    .default_size(170.0)
                    .show(ui, |ui| {
                        copy = colui::detail_pane(ui, view, colors);
                    });
            }
            let env = ViewEnv {
                colors,
                font_size: settings.font_size,
                line_height: settings.line_height,
                line_numbers: settings.line_numbers,
                minimap: settings.show_minimap,
                style_epoch: *style_epoch,
                gap_secs: settings.time_gap_threshold_secs,
                pacer: Some(pacer),
            };
            egui::CentralPanel::no_frame().show(ui, |ui| {
                logview::show(ui, Id::new(("log", tab_id)), view, &env);
            });
        }
        if history_changed {
            self.mark_history_dirty();
        }
        if let Some(text) = copy {
            ui.ctx().copy_text(text);
        }
        if let Some(view) = self.tabs.get_mut(index).and_then(Tab::view_mut) {
            match suggestion {
                SuggestionAction::None => {}
                SuggestionAction::Accept => view.accept_suggestion(),
                SuggestionAction::Choose => view.chooser.open = true,
                SuggestionAction::Dismiss => view.dismiss_suggestion(),
            }
        }
        match status_action {
            StatusAction::None => {}
            StatusAction::SetProfile(p) => self.set_profile(p),
            StatusAction::EditRules => self.open_rule_editor(),
            StatusAction::SetWrap(wrap) => self.set_wrap_in_tab(index, wrap),
        }
    }

    // -------------------------------------------------------------- windows

    fn windows_ui(&mut self, ctx: &Context) {
        self.goto_window(ctx);
        self.goto_time_window(ctx);
        self.merge_window(ctx);
        self.cross_window(ctx);
        self.bookmark_window(ctx);
        self.settings_window(ctx);
        self.shortcuts_window(ctx);
        self.about_window(ctx);
        let colors = self.colors.clone();
        if let Some(view) = self.active_view_mut() {
            colui::chooser_window(ctx, view);
            colui::stats_window(ctx, view, &colors);
            colui::export_window(ctx, view, &colors);
        }
        if self.rule_editor.open {
            let preview: Vec<String> = self
                .active_view()
                .map(|v| v.last_rows.iter().map(|l| l.text.clone()).collect())
                .unwrap_or_default();
            let colors = self.colors.clone();
            let action = self.rule_editor.show(ctx, &colors, &preview);
            self.handle_editor_action(action);
        }
        self.palette_ui(ctx);
    }

    fn goto_time_window(&mut self, ctx: &Context) {
        let Some(mut dlg) = self.windows.goto_time.take() else {
            return;
        };
        let mut close = false;
        let mut apply = false;
        let mut cancel_search = false;
        egui::Window::new("Go to time")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_TOP, [0.0, 90.0])
            .show(ctx, |ui| {
                ui.label("2026-09-29 10:00, 10:15, -15m (from the end) or +1h (from the start)");
                let searching = dlg.job.is_some();
                let resp = ui.add_enabled(
                    !searching,
                    egui::TextEdit::singleline(&mut dlg.text)
                        .hint_text("e.g. 2026-09-29 10:00")
                        .desired_width(300.0),
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
                    if searching {
                        ui.spinner();
                        ui.label("Searching\u{2026}");
                        if ui.button("Cancel").clicked() {
                            cancel_search = true;
                        }
                    } else {
                        if ui.button("Go").clicked() {
                            apply = true;
                        }
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    }
                });
            });
        if cancel_search {
            // Dropping the job cancels the worker.
            dlg.job = None;
        }
        self.windows.goto_time = Some(dlg);
        if apply {
            let text = self
                .windows
                .goto_time
                .as_ref()
                .map(|d| d.text.clone())
                .unwrap_or_default();
            if let Err(e) = self.apply_goto_time(&text)
                && let Some(d) = self.windows.goto_time.as_mut()
            {
                d.error = Some(e);
            }
        }
        if close {
            self.windows.goto_time = None;
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

    fn shortcuts_window(&mut self, ctx: &Context) {
        if !self.windows.shortcuts {
            return;
        }
        let rows = crate::keymap::shortcut_rows(&self.keymap);
        let mut open = true;
        let mut settings = false;
        let height = (ctx.content_rect().height() - 120.0).clamp(200.0, 620.0);
        egui::Window::new("Keyboard shortcuts")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(match self.settings.keymap {
                        oxtail_config::Keymap::Standard => "Standard key bindings.",
                        oxtail_config::Keymap::Less => "less-style key bindings.",
                    });
                    if ui.link("Change\u{2026}").clicked() {
                        settings = true;
                    }
                });
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .max_height(height)
                    .show(ui, |ui| {
                        egui::Grid::new("shortcuts")
                            .num_columns(2)
                            .spacing([18.0, 4.0])
                            .striped(true)
                            .show(ui, |ui| {
                                for (k, d) in &rows {
                                    ui.label(RichText::new(k).monospace());
                                    ui.label(d);
                                    ui.end_row();
                                }
                            });
                    });
            });
        self.windows.shortcuts = open && !settings;
        if settings {
            self.open_settings(crate::settingsui::Section::Keyboard);
        }
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
    w.goto.is_some() || w.goto_time.is_some() || w.settings || w.shortcuts || w.about
}
