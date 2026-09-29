//! Doing things: keyboard handling, [`OxTailApp::perform`] for every
//! [`Action`], and the questions the menus and the command palette ask about
//! an action (is it available now, is it switched on).
//!
//! Menus, shortcuts and the palette all end up in [`OxTailApp::perform`], so
//! there is one code path per command.

use std::time::Instant;

use egui::{Context, Event, Key, ViewportCommand};
use oxtail_config::{Settings, SplitDirection, ThemeChoice};

use crate::app::{Notice, OxTailApp};
use crate::find::{Dir, push_history};
use crate::gototime::GotoTimeDialog;
use crate::keymap::{Action, Chord, KeyMap};
use crate::sysint::{IntegrationView, Topic};
use crate::tab::Tab;
use crate::timeview::{DEFAULT_GAP_SECS, RelMode};

/// Largest clipboard text opened as a tab.
pub const MAX_CLIPBOARD_BYTES: usize = 64 * 1024 * 1024;

impl OxTailApp {
    /// The active key bindings.
    pub fn keymap(&self) -> &KeyMap {
        &self.keymap
    }

    /// Rebuilds the key map from the settings and shows what is wrong with
    /// the user's key bindings (once per distinct problem).
    pub(crate) fn rebuild_keymap(&mut self) {
        self.keymap = KeyMap::build(self.settings.keymap, &self.settings.custom_keybindings);
        // Old key binding warnings are stale now.
        self.notices
            .retain(|n| !n.text.starts_with("Key bindings:"));
        let warnings: Vec<String> = self.keymap.warnings().to_vec();
        for w in warnings {
            let n = Notice {
                text: w,
                error: true,
            };
            if !self.notices.contains(&n) {
                self.notices.push(n);
            }
        }
    }

    /// Whether the command palette is showing.
    pub fn palette_open(&self) -> bool {
        self.palette.open
    }

    /// What is typed in the command palette.
    pub fn palette_query(&self) -> &str {
        &self.palette.query
    }

    /// The highlighted row of the command palette.
    pub fn palette_selected(&self) -> usize {
        self.palette.selected
    }

    /// Ids of the palette entries matching the current query, best first.
    pub fn palette_result_ids(&self) -> Vec<String> {
        let items = self.palette_items();
        crate::palette::rank(&items, &self.palette.query, &self.palette.recent)
            .iter()
            .map(|h| items[h.index].id.clone())
            .collect()
    }

    /// Ids of the commands run from the palette last, most recent first.
    pub fn palette_recent(&self) -> &[String] {
        &self.palette.recent
    }

    /// Messages shown under the menu bar.
    pub fn notices(&self) -> &[Notice] {
        &self.notices
    }

    /// Whether an update check is running.
    pub fn update_check_running(&self) -> bool {
        self.sys.is_checking()
    }

    /// The newer release the user was told about, if any.
    pub fn update_notice(&self) -> Option<&oxtail_config::update::Release> {
        self.sys.notice.as_ref()
    }

    /// Uses another backend for system integration and the update check
    /// (tests answer without touching the system or the network).
    pub fn set_system_backend(
        &mut self,
        backend: std::sync::Arc<dyn crate::sysint::SystemBackend>,
    ) {
        self.sys.set_backend(backend);
    }

    /// Whether a recent file was found (`None` until the worker checked).
    pub fn recent_file_exists(&self, path: &std::path::Path) -> Option<bool> {
        self.recent_exists.get(path).copied()
    }

    /// The recent files, most recent first.
    pub fn recent_files(&self) -> &[std::path::PathBuf] {
        &self.session.recent_files
    }

    // ------------------------------------------------------------- keyboard

    pub(crate) fn handle_keys(&mut self, ctx: &Context) {
        // The key recorder in the settings window takes every key press.
        if self.settings_ui.recording.is_some() {
            return;
        }
        let text_focus = ctx.egui_wants_keyboard_input();
        let palette_open = self.palette.open;
        let mac = self.keymap.is_mac();
        let mut actions: Vec<Action> = Vec::new();
        let mut pasted: Option<String> = None;
        ctx.input(|i| {
            // Text typed this frame: an Alt chord that produced it (Option or
            // AltGr typing a character) is text, not a command.
            let typed = i.events.iter().any(|e| matches!(e, Event::Text(_)));
            for ev in &i.events {
                match ev {
                    Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        repeat,
                        ..
                    } => {
                        let Some(a) = self.keymap.lookup(*key, *modifiers) else {
                            continue;
                        };
                        // Copy and paste arrive as their own events.
                        if a.event_driven() {
                            continue;
                        }
                        // The open palette owns the keyboard.
                        if palette_open && a != Action::Palette {
                            continue;
                        }
                        let chord = Chord::from_event(*key, *modifiers, mac);
                        if (*repeat && !a.repeats())
                            || (text_focus
                                && (!chord.works_in_text_fields_for(mac) || (typed && chord.alt)))
                        {
                            continue;
                        }
                        actions.push(a);
                    }
                    Event::Copy | Event::Cut | Event::Paste(_) if !text_focus && !palette_open => {
                        // egui-winit turns Cmd/Ctrl+C/X/V (with any Shift or
                        // Alt) into these events without a key event: rebuild
                        // the chord to find what the user bound to it.
                        let key = match ev {
                            Event::Copy => Key::C,
                            Event::Cut => Key::X,
                            _ => Key::V,
                        };
                        // A key event for the same press was handled above.
                        if i.events.iter().any(
                            |e| matches!(e, Event::Key { key: k, pressed: true, .. } if *k == key),
                        ) {
                            continue;
                        }
                        match self.keymap.lookup(key, i.modifiers) {
                            Some(a) if !a.event_driven() => actions.push(a),
                            _ => match ev {
                                Event::Copy => actions.push(Action::Copy),
                                Event::Paste(text) => pasted = Some(text.clone()),
                                _ => {}
                            },
                        }
                    }
                    _ => {}
                }
            }
        });
        for a in actions {
            self.perform(a, ctx);
        }
        if let Some(text) = pasted {
            self.open_clipboard_text(text);
        }
    }

    // ------------------------------------------------------------- state

    fn has_merged(&self) -> bool {
        self.tabs
            .get(self.active)
            .is_some_and(|t| t.merged().is_some())
    }

    /// Whether `action` can do something right now (menus grey it out, the
    /// palette lists it last).
    pub fn action_enabled(&self, action: Action) -> bool {
        let view = self.active_view();
        let merged = self.has_merged();
        let any_view = view.is_some() || merged;
        let parser = view.is_some_and(|v| v.st.parser.is_some());
        match action {
            Action::Open
            | Action::OpenFolder
            | Action::PasteAsTab
            | Action::Settings
            | Action::Palette
            | Action::Quit
            | Action::Escape
            | Action::FindInTabs
            | Action::ToggleLineNumbers
            | Action::ToggleMinimap
            | Action::ToggleTimeGaps
            | Action::ZoomIn
            | Action::ZoomOut
            | Action::ZoomReset
            | Action::ThemeSystem
            | Action::ThemeDark
            | Action::ThemeLight
            | Action::ThemeHighContrast
            | Action::Shortcuts
            | Action::About
            | Action::CheckUpdates => true,
            Action::CloseTab | Action::SplitRight | Action::SplitDown => !self.tabs.is_empty(),
            Action::CloseOtherTabs => self.tabs_in_pane(self.focused_pane).len() > 1,
            Action::NextTab | Action::PrevTab => self.tabs_in_pane(self.focused_pane).len() > 1,
            Action::ClearRecent => !self.session.recent_files.is_empty(),
            Action::Copy
            | Action::CopyWithNumbers
            | Action::SelectAll
            | Action::Find
            | Action::FindNext
            | Action::FindPrev
            | Action::Filter
            | Action::ScrollTop
            | Action::ScrollBottom
            | Action::PageUp
            | Action::PageDown
            | Action::LineUp
            | Action::LineDown
            | Action::ScrollLeft
            | Action::ScrollRight
            | Action::ToggleFollow => any_view,
            Action::Mark
            | Action::GotoLine
            | Action::GotoTime
            | Action::ToggleBookmark
            | Action::NextBookmark
            | Action::PrevBookmark
            | Action::ToggleWrap
            | Action::ToggleAnsi
            | Action::ChooseParser
            | Action::RelTimeOff
            | Action::RelTimePrevious
            | Action::RelTimeSelected
            | Action::EditRules => view.is_some(),
            Action::ToggleHidden => view.is_some_and(|v| v.hl.borrow().hide.is_some()),
            Action::ToggleColumns | Action::ToggleDetail | Action::Stats | Action::Export => parser,
            Action::SyncCursors => self.pane_count() > 1,
            Action::MergeTabs => self.merge_candidates().len() >= 2,
            Action::IntegrateSystem => {
                !self.sys.integration_busy
                    && !matches!(
                        self.sys.integration,
                        IntegrationView::Known(
                            oxtail_config::integration::IntegrationState::Integrated { .. }
                                | oxtail_config::integration::IntegrationState::Unsupported(_)
                        )
                    )
            }
            Action::RemoveIntegration => {
                !self.sys.integration_busy
                    && matches!(
                        self.sys.integration,
                        IntegrationView::Known(
                            oxtail_config::integration::IntegrationState::Integrated { .. }
                        )
                    )
            }
        }
    }

    /// For toggles and radio choices: whether the option is on now.
    pub fn action_checked(&self, action: Action) -> Option<bool> {
        let view = self.active_view();
        Some(match action {
            Action::ToggleFollow => match self.tabs.get(self.active) {
                Some(t) => match (t.view(), t.merged()) {
                    (Some(v), _) => v.follow,
                    (None, Some(m)) => m.follow,
                    _ => false,
                },
                None => false,
            },
            Action::ToggleWrap => view.is_some_and(|v| v.wrap),
            Action::ToggleLineNumbers => self.settings.line_numbers,
            Action::ToggleAnsi => view.is_none_or(|v| v.hl.borrow().show_ansi),
            Action::ToggleHidden => view.is_some_and(|v| v.filter.show_hidden),
            Action::ToggleMinimap => self.settings.show_minimap,
            Action::ToggleTimeGaps => self.settings.time_gap_threshold_secs > 0.0,
            Action::ToggleColumns => view.is_some_and(|v| v.table_active()),
            Action::ToggleDetail => view.is_some_and(|v| v.detail_open),
            Action::RelTimeOff => view.is_some_and(|v| v.rel_mode == RelMode::Off),
            Action::RelTimePrevious => view.is_some_and(|v| v.rel_mode == RelMode::Previous),
            Action::RelTimeSelected => view.is_some_and(|v| v.rel_mode == RelMode::Selected),
            Action::SyncCursors => self.sync_cursor(),
            Action::ThemeSystem => self.settings.theme == ThemeChoice::System,
            Action::ThemeDark => self.settings.theme == ThemeChoice::Dark,
            Action::ThemeLight => self.settings.theme == ThemeChoice::Light,
            Action::ThemeHighContrast => self.settings.theme == ThemeChoice::HighContrast,
            _ => return None,
        })
    }

    // ------------------------------------------------------------- perform

    /// Carries out an action (keyboard, menu or palette). Actions that do
    /// not apply right now do nothing.
    pub fn perform(&mut self, action: Action, ctx: &Context) {
        if !self.action_enabled(action) {
            return;
        }
        match action {
            Action::Open => self.pick_files(),
            Action::OpenFolder => self.pick_folder(),
            Action::PasteAsTab => ctx.send_viewport_cmd(ViewportCommand::RequestPaste),
            Action::CloseTab => self.close_tab(self.active),
            Action::CloseOtherTabs => {
                let keep = self.tabs.get(self.active).map(|t| t.id);
                let others: Vec<u64> = self
                    .tabs_in_pane(self.focused_pane)
                    .into_iter()
                    .filter_map(|i| self.tabs.get(i).map(|t| t.id))
                    .filter(|id| Some(*id) != keep)
                    .collect();
                for id in others {
                    if let Some(j) = self.tabs.iter().position(|t| t.id == id) {
                        self.close_tab(j);
                    }
                }
            }
            Action::ClearRecent => self.session.recent_files.clear(),
            Action::Settings => self.windows.settings = true,
            Action::Palette => self.toggle_palette(),
            Action::Quit => ctx.send_viewport_cmd(ViewportCommand::Close),
            Action::NextTab => self.cycle_tab(1),
            Action::PrevTab => self.cycle_tab(-1),
            Action::ZoomIn => self.set_font_size(self.settings.font_size + 1.0),
            Action::ZoomOut => self.set_font_size(self.settings.font_size - 1.0),
            Action::ZoomReset => self.set_font_size(Settings::default().font_size),
            Action::GotoLine => {
                self.windows.goto = Some(crate::app::GotoDialog {
                    focus: true,
                    ..crate::app::GotoDialog::default()
                });
            }
            Action::GotoTime => {
                self.windows.goto_time = Some(GotoTimeDialog {
                    focus: true,
                    ..GotoTimeDialog::default()
                });
            }
            Action::FindInTabs => {
                self.windows.cross.open = true;
                self.windows.cross.focus = true;
            }
            Action::Escape => self.escape(),
            Action::ToggleWrap => {
                let wrap = !self.active_view().is_some_and(|v| v.wrap);
                self.set_wrap(wrap);
            }
            Action::ToggleLineNumbers => {
                self.settings.line_numbers = !self.settings.line_numbers;
                self.bump_style();
                self.mark_settings_dirty();
            }
            Action::ToggleMinimap => {
                self.settings.show_minimap = !self.settings.show_minimap;
                self.mark_settings_dirty();
            }
            Action::ToggleTimeGaps => {
                let on = self.settings.time_gap_threshold_secs > 0.0;
                self.set_gap_threshold(if on { 0.0 } else { DEFAULT_GAP_SECS });
            }
            Action::ToggleAnsi => {
                if let Some(v) = self.active_view_mut() {
                    let on = !v.hl.borrow().show_ansi;
                    v.hl.borrow_mut().set_show_ansi(on);
                    v.galleys.borrow_mut().clear();
                }
            }
            Action::ToggleHidden => {
                if let Some(v) = self.active_view_mut() {
                    v.filter.show_hidden = !v.filter.show_hidden;
                    v.filter.changed();
                }
            }
            Action::ToggleColumns => {
                if let Some(v) = self.active_view_mut() {
                    let on = !v.table_active();
                    v.set_table(on);
                }
            }
            Action::ChooseParser => {
                if let Some(v) = self.active_view_mut() {
                    v.chooser.open = true;
                }
            }
            Action::ToggleDetail => {
                if let Some(v) = self.active_view_mut() {
                    v.detail_open = !v.detail_open;
                }
            }
            Action::RelTimeOff => self.set_rel_mode(RelMode::Off),
            Action::RelTimePrevious => self.set_rel_mode(RelMode::Previous),
            Action::RelTimeSelected => self.set_rel_mode(RelMode::Selected),
            Action::Stats => {
                if let Some(v) = self.active_view_mut() {
                    v.stats.open = true;
                }
            }
            Action::Export => {
                if let Some(v) = self.active_view_mut() {
                    v.export.open = true;
                }
            }
            Action::ThemeSystem => self.set_theme(ThemeChoice::System),
            Action::ThemeDark => self.set_theme(ThemeChoice::Dark),
            Action::ThemeLight => self.set_theme(ThemeChoice::Light),
            Action::ThemeHighContrast => self.set_theme(ThemeChoice::HighContrast),
            Action::SplitRight => self.split_active(SplitDirection::Horizontal),
            Action::SplitDown => self.split_active(SplitDirection::Vertical),
            Action::SyncCursors => {
                let on = !self.sync_cursor();
                self.set_sync_cursor(on);
            }
            Action::MergeTabs => {
                self.windows.merge = Some(crate::appmerge::MergeDialog::default());
            }
            Action::EditRules => self.open_rule_editor(),
            Action::Shortcuts => self.windows.shortcuts = true,
            Action::About => self.windows.about = true,
            Action::CheckUpdates => {
                self.windows.settings = true;
                self.settings_ui.section = crate::settingsui::Section::Updates;
                self.check_updates_now();
            }
            Action::IntegrateSystem => self.change_integration(false),
            Action::RemoveIntegration => self.change_integration(true),
            other => self.perform_on_view(other, ctx),
        }
        ctx.request_repaint();
    }

    fn set_rel_mode(&mut self, mode: RelMode) {
        if let Some(v) = self.active_view_mut() {
            v.rel_mode = mode;
        }
    }

    /// Opens clipboard text in a new tab (on a worker thread). Text beyond
    /// [`MAX_CLIPBOARD_BYTES`] is cut.
    pub fn open_clipboard_text(&mut self, mut text: String) {
        if text.trim().is_empty() {
            return;
        }
        if text.len() > MAX_CLIPBOARD_BYTES {
            let mut cut = MAX_CLIPBOARD_BYTES;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            self.notices.push(Notice {
                text: "The clipboard text is very large; only the first 64 MB were opened".into(),
                error: false,
            });
        }
        self.open_text_tab("Clipboard", text);
    }

    pub(crate) fn escape(&mut self) {
        if self.windows.goto.take().is_some() {
            return;
        }
        // Dropping the dialog cancels a running search.
        if self.windows.goto_time.take().is_some() {
            return;
        }
        if self.rule_editor.open {
            self.rule_editor.open = false;
            return;
        }
        if let Some(m) = self.tabs.get_mut(self.active).and_then(Tab::merged_mut)
            && m.find.open
        {
            m.find.open = false;
            return;
        }
        if let Some(v) = self.active_view_mut() {
            if v.editing_bookmark.take().is_some() {
                return;
            }
            if v.find.open {
                v.find.open = false;
            } else if v.selection.is_some() {
                v.clear_selection();
            }
        }
    }

    fn perform_on_view(&mut self, action: Action, ctx: &Context) {
        if self.has_merged() {
            self.perform_on_merged(action, ctx);
            return;
        }
        let now = Instant::now();
        let Some(view) = self.tabs.get_mut(self.active).and_then(Tab::view_mut) else {
            return;
        };
        match action {
            Action::Find => {
                view.find.open = true;
                view.find.focus = true;
            }
            Action::FindNext | Action::FindPrev => {
                if view.find.text.is_empty() {
                    view.find.open = true;
                    view.find.focus = true;
                } else {
                    // Matches are only painted while the bar is open.
                    view.find.open = true;
                    let dir = if action == Action::FindNext {
                        Dir::Next
                    } else {
                        Dir::Prev
                    };
                    if !view.find.is_running() {
                        view.find.restart_now(now);
                    }
                    view.find.step(dir, view.pos.top);
                    let text = view.find.text.clone();
                    push_history(&mut self.history, &text);
                    self.history_dirty = true;
                }
            }
            Action::Filter => {
                view.filter.open = !view.filter.open;
                if view.filter.open && view.filter.entries.is_empty() {
                    view.filter
                        .entries
                        .push(crate::filter::FilterEntry::default());
                }
                if view.filter.open {
                    view.filter.focus_last = true;
                }
            }
            Action::ToggleBookmark => view.toggle_bookmark(),
            Action::NextBookmark => view.goto_bookmark(Dir::Next),
            Action::PrevBookmark => view.goto_bookmark(Dir::Prev),
            Action::Mark => view.add_mark(),
            Action::Copy => view.copy_selection(false),
            Action::CopyWithNumbers => view.copy_selection(true),
            Action::SelectAll => view.select_all(),
            Action::ToggleFollow => view.toggle_follow(),
            Action::ScrollTop => view.jump_top(),
            Action::ScrollBottom => view.resume_follow(),
            Action::PageUp => view.page(false),
            Action::PageDown => view.page(true),
            Action::LineUp => {
                let h = view.metrics.row_h;
                view.scroll_px(-h);
            }
            Action::LineDown => {
                let h = view.metrics.row_h;
                view.scroll_px(h);
            }
            Action::ScrollLeft => view.h_scroll = (view.h_scroll - 40.0).max(0.0),
            Action::ScrollRight => view.h_scroll += 40.0,
            _ => {}
        }
        ctx.request_repaint();
    }

    // -------------------------------------------------- update / integration

    /// Starts the update check for the "Check now" button and the palette.
    pub fn check_updates_now(&mut self) {
        let wake = self.waker();
        self.sys.check_now(&self.data_dir, wake);
    }

    /// Registers or unregisters OxTail with the system (on a worker).
    pub fn change_integration(&mut self, remove: bool) {
        let wake = self.waker();
        self.sys.change_integration(&self.data_dir, remove, wake);
    }

    /// Applies the results of the update and integration workers.
    pub(crate) fn poll_system(&mut self, ctx: &Context) {
        let wake = self.waker();
        if self.sys.poll(&self.data_dir, &wake) {
            ctx.request_repaint();
        }
        let announcements = std::mem::take(&mut self.sys.announcements);
        // The settings window shows the results of the section it displays.
        let shown = self.windows.settings.then_some(self.settings_ui.section);
        for (text, error, topic) in announcements {
            let in_window = match topic {
                Topic::Integration => shown == Some(crate::settingsui::Section::System),
                Topic::Update => shown == Some(crate::settingsui::Section::Updates),
            };
            if !in_window {
                self.notices.push(Notice { text, error });
            }
        }
    }
}
