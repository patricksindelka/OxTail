//! Drawing the application: menu bar, tab bar, notices, the active tab and
//! the dialog windows.

use std::time::Instant;

use egui::{Align2, Context, CornerRadius, Id, RichText, Ui, ViewportCommand};
use oxtail_config::{ThemeChoice, TimezoneSetting};

use crate::app::{OxTailApp, Windows};
use crate::colui::{self, SuggestionAction};
use crate::keymap::Action;
use crate::logview::{self, ViewEnv};
use crate::panels::{self, StatusAction};
use crate::request::OpenRequest;
use crate::tab::{Tab, TabContent};
use crate::timeview::{DEFAULT_GAP_SECS, RelMode};

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
                    if ui
                        .add(egui::Button::new("Go to time\u{2026}").shortcut_text("Ctrl+Shift+G"))
                        .clicked()
                    {
                        self.perform(Action::GotoTime, ctx);
                        ui.close();
                    }
                    if ui
                        .add(
                            egui::Button::new("Search in all tabs\u{2026}")
                                .shortcut_text("Ctrl+Alt+F"),
                        )
                        .clicked()
                    {
                        self.perform(Action::FindInTabs, ctx);
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
                self.columns_menu(ui);
                self.time_menu(ui);
                ui.separator();
                self.layout_menu(ui);
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

    /// The "Columns" submenu of the View menu.
    fn columns_menu(&mut self, ui: &mut Ui) {
        let Some(view) = self.active_view_mut() else {
            return;
        };
        ui.menu_button("Columns", |ui| {
            let has_parser = view.st.parser.is_some();
            let mut table = view.table_active();
            if ui
                .add_enabled(
                    has_parser,
                    egui::Checkbox::new(&mut table, "Show as columns"),
                )
                .changed()
            {
                view.set_table(table);
            }
            if ui.button("Choose parser\u{2026}").clicked() {
                view.chooser.open = true;
                ui.close();
            }
            let mut detail = view.detail_open;
            if ui
                .add_enabled(has_parser, egui::Checkbox::new(&mut detail, "Detail pane"))
                .changed()
            {
                view.detail_open = detail;
            }
            ui.separator();
            ui.menu_button("Relative time", |ui| {
                for mode in [RelMode::Off, RelMode::Previous, RelMode::Selected] {
                    if ui
                        .radio_value(&mut view.rel_mode, mode, mode.label())
                        .clicked()
                    {
                        ui.close();
                    }
                }
            });
            if ui
                .add_enabled(has_parser, egui::Button::new("Statistics\u{2026}"))
                .clicked()
            {
                view.stats.open = true;
                ui.close();
            }
            if ui
                .add_enabled(has_parser, egui::Button::new("Export\u{2026}"))
                .clicked()
            {
                view.export.open = true;
                ui.close();
            }
            if has_parser {
                ui.separator();
                ui.label(egui::RichText::new(format!("Format: {}", view.st.name)).weak());
            }
        });
    }

    /// The "Time" additions of the View menu: gap separators.
    fn time_menu(&mut self, ui: &mut Ui) {
        let mut on = self.settings.time_gap_threshold_secs > 0.0;
        if ui
            .checkbox(&mut on, "Show time gaps")
            .on_hover_text("A separator between lines that are far apart in time")
            .changed()
        {
            self.settings.time_gap_threshold_secs = if on { DEFAULT_GAP_SECS } else { 0.0 };
            self.mark_settings_dirty();
        }
    }

    // -------------------------------------------------------------- tab bar

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

    fn tab_ui(&mut self, ui: &mut Ui, index: usize) {
        let now = Instant::now();
        let mut status_action = StatusAction::None;
        let mut history_changed = false;
        let mut suggestion = SuggestionAction::None;
        let mut copy: Option<String> = None;
        {
            let Self {
                tabs,
                colors,
                settings,
                profiles,
                history,
                style_epoch,
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
                status_action = panels::status_bar(ui, view, profiles);
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
                minimap: true,
                style_epoch: *style_epoch,
                gap_secs: settings.time_gap_threshold_secs,
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
        let mut zone = self.settings.timezone.clone();
        let mut gap_secs = self.settings.time_gap_threshold_secs;
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
                        ui.label("Time zone");
                        ui.horizontal(|ui| {
                            let label = match &zone {
                                TimezoneSetting::Local => "Local".to_string(),
                                TimezoneSetting::Utc => "UTC".to_string(),
                                TimezoneSetting::Named(_) => "Named".to_string(),
                            };
                            egui::ComboBox::from_id_salt("time-zone")
                                .selected_text(label)
                                .width(80.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut zone, TimezoneSetting::Local, "Local");
                                    ui.selectable_value(&mut zone, TimezoneSetting::Utc, "UTC");
                                    if ui
                                        .selectable_label(
                                            matches!(zone, TimezoneSetting::Named(_)),
                                            "Named",
                                        )
                                        .clicked()
                                        && !matches!(zone, TimezoneSetting::Named(_))
                                    {
                                        zone = TimezoneSetting::Named("Europe/Amsterdam".into());
                                    }
                                });
                            if let TimezoneSetting::Named(n) = &mut zone {
                                ui.add(
                                    egui::TextEdit::singleline(n)
                                        .desired_width(150.0)
                                        .hint_text("IANA name, e.g. Europe/Amsterdam"),
                                );
                            }
                        });
                        ui.end_row();
                        ui.label("Time gap separator (seconds, 0 = off)");
                        ui.add(
                            egui::DragValue::new(&mut gap_secs)
                                .range(0.0..=86_400.0)
                                .speed(0.5),
                        );
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
        if zone != self.settings.timezone {
            self.settings.timezone = zone;
            dirty = true;
        }
        if (gap_secs - self.settings.time_gap_threshold_secs).abs() > f64::EPSILON {
            self.settings.time_gap_threshold_secs = gap_secs.max(0.0);
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
    ("Ctrl+Alt+F", "Search in all open tabs"),
    ("Ctrl+G", "Go to line (123, +100, -50, 50%)"),
    (
        "Ctrl+Shift+G",
        "Go to time (2026-09-29 10:00, 10:15, -15m from the end, +1h from the start)",
    ),
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
    w.goto.is_some() || w.goto_time.is_some() || w.settings || w.shortcuts || w.about
}
