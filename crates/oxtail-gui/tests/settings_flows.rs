//! The settings window, the key bindings editor, the update notice, system
//! integration and the start screen, driven through the UI. System calls are
//! answered by a fake backend, so nothing touches the machine or the network.

mod common;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use egui::{Event, Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use oxtail_config::integration::IntegrationState;
use oxtail_config::update::Release;
use oxtail_config::{ConfigError, DataDir, DataMode, Keymap, Session, Settings};
use oxtail_gui::sysint::SystemBackend;
use oxtail_gui::{AppInit, ExternalOpen, OpenRequest, OxTailApp, load_startup};

#[derive(Default)]
struct Fake {
    integrated: Mutex<bool>,
    release: Mutex<Option<Release>>,
    due: Mutex<bool>,
    checks: Mutex<u32>,
    due_asks: Mutex<u32>,
}

impl SystemBackend for Fake {
    fn integration_state(&self, _: &DataDir) -> IntegrationState {
        if *self.integrated.lock().unwrap() {
            IntegrationState::Integrated {
                items: vec!["Start menu entry".into()],
            }
        } else {
            IntegrationState::NotIntegrated
        }
    }
    fn integrate(&self, _: &DataDir, _: &Path) -> Result<Vec<String>, ConfigError> {
        *self.integrated.lock().unwrap() = true;
        Ok(vec!["Created a Start menu entry".into()])
    }
    fn remove(&self, _: &DataDir) -> Result<Vec<String>, ConfigError> {
        *self.integrated.lock().unwrap() = false;
        Ok(vec!["Removed the Start menu entry".into()])
    }
    fn check_latest(&self, _: &str) -> Result<Option<Release>, ConfigError> {
        *self.checks.lock().unwrap() += 1;
        Ok(self.release.lock().unwrap().clone())
    }
    fn check_due(&self, _: &DataDir) -> bool {
        *self.due_asks.lock().unwrap() += 1;
        *self.due.lock().unwrap()
    }
    fn record_check(&self, _: &DataDir) -> Result<(), ConfigError> {
        Ok(())
    }
}

fn release() -> Release {
    Release {
        version: "9.9.9".into(),
        url: "https://example.org/oxtail/9.9.9".into(),
        notes: None,
    }
}

fn persistent(dir: &Path) -> DataDir {
    for sub in ["profiles", "themes"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    DataDir {
        root: Some(dir.to_path_buf()),
        mode: DataMode::Cli,
    }
}

fn app_from(dd: DataDir, f: impl FnOnce(&mut oxtail_gui::Startup)) -> OxTailApp {
    let mut startup = load_startup(dd);
    f(&mut startup);
    OxTailApp::from_init(AppInit {
        startup,
        request: OpenRequest::default(),
        external: ExternalOpen::disconnected(),
    })
}

fn with_settings(f: impl FnOnce(&mut Settings)) -> OxTailApp {
    app_from(DataDir::in_memory("test"), |s| f(&mut s.settings))
}

fn with_view(mut app: OxTailApp) -> Harness<'static, OxTailApp> {
    app.open_document("app.log", doc_from(logfmt_sample(100)));
    let mut h = harness(app);
    step_until(&mut h, "rows", |a| {
        a.active_view().is_some_and(|v| v.last_rows.len() > 5)
    });
    h
}

fn open_settings(h: &mut Harness<'_, OxTailApp>) {
    h.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
    h.step();
    h.step();
    assert!(h.get_all_by_label("Appearance").count() >= 1);
}

/// Lets the layout settle (grids need a frame or two to find their size, and
/// a click uses the position the previous frame reported).
fn settle(h: &mut Harness<'_, OxTailApp>) {
    for _ in 0..4 {
        h.step();
    }
}

/// Steps until a node with this label exists (max 10 s).
fn wait_for_label(h: &mut Harness<'_, OxTailApp>, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        h.step();
        if h.query_by_label(label).is_some() {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_for_label_contains(h: &mut Harness<'_, OxTailApp>, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        h.step();
        if h.query_all_by_label_contains(text).next().is_some() {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {text:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn ctrl_comma_opens_the_settings_and_a_change_is_saved_to_settings_toml() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = with_view(app_from(persistent(dir.path()), |_| {}));
    open_settings(&mut h);
    h.get_by_label("Wrap instead of scrolling sideways").click();
    step_until(&mut h, "wrap on", |a| a.settings().wrap);
    assert!(h.state().active_view().unwrap().wrap);
    h.get_by_label("Show the line number column").click();
    step_until(&mut h, "numbers off", |a| !a.settings().line_numbers);
    // The save goes through the persist thread after a short delay.
    let path = dir.path().join("settings.toml");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        h.step();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if text.contains("wrap = true") && text.contains("line_numbers = false") {
            break;
        }
        assert!(Instant::now() < deadline, "settings.toml was not written");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn every_section_can_be_shown() {
    let mut h = with_view(with_settings(|_| {}));
    open_settings(&mut h);
    for (section, probe) in [
        ("Keyboard", "Key bindings"),
        ("Files & following", "Cache size"),
        ("Columns & time", "Time zone"),
        ("Updates", "Check now"),
        ("System", "Data folder"),
        ("Appearance", "Font size"),
    ] {
        h.get_by_label(section).click();
        h.step();
        h.step();
        assert!(
            h.query_by_label(probe).is_some(),
            "section {section} does not show {probe}"
        );
    }
}

#[test]
fn the_keymap_preset_can_be_switched_in_the_window() {
    let mut h = with_view(with_settings(|_| {}));
    open_settings(&mut h);
    h.get_by_label("Keyboard").click();
    h.step();
    h.get_by_label_contains("less-style").click();
    step_until(&mut h, "less", |a| a.settings().keymap == Keymap::Less);
    assert!(h.state().keymap().lookup(Key::J, Modifiers::NONE).is_some());
}

#[test]
fn a_shortcut_can_be_recorded_and_reset() {
    let mut h = with_view(with_settings(|_| {}));
    open_settings(&mut h);
    h.get_by_label("Keyboard").click();
    settle(&mut h);
    // Narrow the list to one command.
    h.get_by_role(egui::accesskit::Role::TextInput).click();
    settle(&mut h);
    h.event(Event::Text("mark after".into()));
    settle(&mut h);
    // Only the one command is left in the list.
    assert!(
        h.query_by_label("Insert a mark after the last line")
            .is_some()
    );
    assert_eq!(h.get_all_by_label("Change").count(), 1);
    h.get_by_label("Change").click();
    settle(&mut h);
    assert!(h.query_by_label("Cancel").is_some());
    // The recorder takes the next key press, whatever it is.
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::ALT, Key::K);
    step_until(&mut h, "recorded", |a| {
        a.settings().custom_keybindings.contains_key("edit.mark")
    });
    assert_eq!(
        h.state().settings().custom_keybindings["edit.mark"],
        "Mod+Alt+K"
    );
    h.step();
    assert_eq!(
        h.state()
            .keymap()
            .shortcut_text(oxtail_gui::keymap::Action::Mark)
            .as_deref(),
        Some(shown("Alt+K").as_str())
    );
    // The new chord works, the old one is gone.
    assert!(
        h.state()
            .keymap()
            .lookup(Key::M, Modifiers::COMMAND)
            .is_none()
    );
    // Reset brings the default back.
    h.get_by_label("Reset").click();
    step_until(&mut h, "reset", |a| {
        a.settings().custom_keybindings.is_empty()
    });
    h.step();
    assert!(
        h.state()
            .keymap()
            .lookup(Key::M, Modifiers::COMMAND)
            .is_some()
    );
}

#[test]
fn escape_cancels_recording() {
    let mut h = with_view(with_settings(|_| {}));
    open_settings(&mut h);
    h.get_by_label("Keyboard").click();
    settle(&mut h);
    h.get_by_role(egui::accesskit::Role::TextInput).click();
    settle(&mut h);
    h.event(Event::Text("mark after".into()));
    settle(&mut h);
    h.get_by_label("Change").click();
    settle(&mut h);
    assert!(h.query_by_label("Cancel").is_some());
    h.key_press(Key::Escape);
    h.step();
    h.step();
    assert!(h.state().settings().custom_keybindings.is_empty());
    assert!(h.query_by_label("Change").is_some());
    // The window is still open (Esc went to the recorder only).
    assert!(h.get_all_by_label("Keyboard").count() >= 1);
}

#[test]
fn a_conflicting_shortcut_is_flagged() {
    let mut h = with_view(with_settings(|s| {
        s.custom_keybindings
            .insert("edit.mark".into(), "Ctrl+K".into());
        s.custom_keybindings
            .insert("search.filter".into(), "Ctrl+K".into());
    }));
    open_settings(&mut h);
    h.get_by_label("Keyboard").click();
    h.step();
    h.step();
    assert!(
        h.query_all_by_label_contains("Ctrl+K is used by")
            .next()
            .is_some()
    );
}

#[test]
fn a_newer_release_is_announced_at_startup_and_can_be_dismissed() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    *fake.due.lock().unwrap() = true;
    *fake.release.lock().unwrap() = Some(release());
    let mut app = app_from(persistent(dir.path()), |s| s.settings.update_check = true);
    app.set_system_backend(fake.clone());
    let mut h = with_view(app);
    step_until(&mut h, "notice", |a| a.update_notice().is_some());
    wait_for_label_contains(&mut h, "9.9.9 is available");
    h.get_by_label("Release page");
    // The update notice is the first bar under the menu.
    h.get_all_by_label("Dismiss").next().unwrap().click();
    step_until(&mut h, "dismissed", |a| a.update_notice().is_none());
    assert_eq!(*fake.checks.lock().unwrap(), 1);
}

#[test]
fn no_check_happens_when_updates_are_off_or_not_due() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    *fake.release.lock().unwrap() = Some(release());
    // Off (the default in portable and CLI mode): never asks.
    let mut app = app_from(persistent(dir.path()), |s| s.settings.update_check = false);
    app.set_system_backend(fake.clone());
    let mut h = with_view(app);
    h.step();
    assert_eq!(*fake.checks.lock().unwrap(), 0);
    // On but not due: asks the throttle only.
    let mut app = app_from(persistent(dir.path()), |s| s.settings.update_check = true);
    app.set_system_backend(fake.clone());
    let mut h2 = with_view(app);
    // The worker asks the throttle; wait until it is done (max 10 s).
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        h2.step();
        if *fake.due_asks.lock().unwrap() > 0 && !h2.state().update_check_running() {
            break;
        }
        assert!(Instant::now() < deadline, "the throttled check never ended");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(*fake.checks.lock().unwrap(), 0);
    assert!(h2.state().update_notice().is_none());
    drop(h);
}

#[test]
fn check_now_reports_the_result_in_the_updates_section() {
    let mut h = {
        let mut app = with_settings(|_| {});
        let fake = Arc::new(Fake::default());
        app.set_system_backend(fake);
        with_view(app)
    };
    open_settings(&mut h);
    h.get_by_label("Updates").click();
    h.step();
    h.get_by_label("Check now").click();
    wait_for_label(&mut h, "You have the latest version.");
}

#[test]
fn integration_can_be_added_and_removed_from_the_system_section() {
    let mut h = {
        let mut app = with_settings(|_| {});
        app.set_system_backend(Arc::new(Fake::default()));
        with_view(app)
    };
    open_settings(&mut h);
    h.get_by_label("System").click();
    wait_for_label(&mut h, "Not integrated");
    h.get_by_label("Integrate with system").click();
    wait_for_label(&mut h, "Integrated with the system");
    assert!(
        h.query_all_by_label_contains("Created a Start menu entry")
            .next()
            .is_some()
    );
    h.get_by_label("Remove integration").click();
    wait_for_label(&mut h, "Not integrated");
}

#[test]
fn integration_offers_nothing_where_it_is_unsupported() {
    struct Unsupported;
    impl SystemBackend for Unsupported {
        fn integration_state(&self, _: &DataDir) -> IntegrationState {
            IntegrationState::Unsupported("Not on this system".into())
        }
        fn integrate(&self, _: &DataDir, _: &Path) -> Result<Vec<String>, ConfigError> {
            panic!("must not be called")
        }
        fn remove(&self, _: &DataDir) -> Result<Vec<String>, ConfigError> {
            panic!("must not be called")
        }
        fn check_latest(&self, _: &str) -> Result<Option<Release>, ConfigError> {
            Ok(None)
        }
        fn check_due(&self, _: &DataDir) -> bool {
            false
        }
        fn record_check(&self, _: &DataDir) -> Result<(), ConfigError> {
            Ok(())
        }
    }
    let mut app = with_settings(|_| {});
    app.set_system_backend(Arc::new(Unsupported));
    let mut h = with_view(app);
    open_settings(&mut h);
    h.get_by_label("System").click();
    wait_for_label(&mut h, "Not available on this system");
    // Clicking the disabled buttons does nothing (and must not call the backend).
    h.get_by_label("Integrate with system").click();
    h.get_by_label("Remove integration").click();
    h.step();
    h.step();
    assert!(h.query_by_label("Not available on this system").is_some());
}

// ------------------------------------------------------------ start screen

#[test]
fn the_start_screen_offers_the_ways_to_open_and_lists_the_keys() {
    let mut h = harness(with_settings(|_| {}));
    h.step();
    h.step();
    h.get_by_label("OxTail");
    h.get_by_label("Open file\u{2026}");
    h.get_by_label("Open folder\u{2026}");
    h.get_by_label("Paste from clipboard");
    h.get_by_label("Files you open will appear here.");
    h.get_by_label(&shown("Shift+P"));
    h.get_by_label("Search every command");
}

#[test]
fn the_start_screen_tips_follow_the_active_keymap() {
    let mut h = harness(with_settings(|s| {
        s.keymap = Keymap::Less;
        s.custom_keybindings
            .insert("app.palette".into(), "Ctrl+Alt+K".into());
    }));
    h.step();
    h.step();
    h.get_by_label("Ctrl+Alt+K");
    assert!(h.query_by_label("Ctrl+Shift+P").is_none());
    h.get_by_label("/");
}

#[test]
fn recent_files_open_on_click_and_missing_ones_are_dimmed_and_inert() {
    let dir = tempfile::tempdir().unwrap();
    let there = dir.path().join("there.log");
    let gone = dir.path().join("gone.log");
    std::fs::write(&there, "a line\nanother line\n").unwrap();
    let mut app = app_from(DataDir::in_memory("test"), |s| {
        let mut session = Session::new();
        session.recent_files = vec![gone.clone(), there.clone()];
        s.session = session;
    });
    app.open_request(OpenRequest::default());
    let mut h = harness(app);
    // A worker finds out which files exist.
    step_until(&mut h, "checked", |a| {
        a.recent_file_exists(&gone) == Some(false) && a.recent_file_exists(&there) == Some(true)
    });
    h.get_by_label("Open recent file there.log");
    // A missing file cannot be clicked open.
    h.get_by_label("Open recent file gone.log").click();
    h.step();
    h.step();
    assert_eq!(h.state().tab_count(), 0);
    h.get_by_label("Open recent file there.log").click();
    step_until(&mut h, "opened", |a| a.active_view().is_some());
    assert_eq!(h.state().tab_count(), 1);
}

#[test]
fn pasted_text_opens_in_a_new_tab_from_the_start_screen() {
    let mut h = harness(with_settings(|_| {}));
    h.step();
    h.event(Event::Paste(
        "first pasted line\nsecond pasted line\n".into(),
    ));
    step_until(&mut h, "clipboard tab", |a| {
        a.active_view().is_some_and(|v| {
            v.last_rows
                .iter()
                .any(|l| l.text.contains("second pasted line"))
        })
    });
    assert_eq!(h.state().tab_count(), 1);
    assert_eq!(h.state().active_tab().unwrap().title, "Clipboard");
}

#[test]
fn paste_is_ignored_while_a_text_field_has_the_focus() {
    let mut h = with_view(with_settings(|_| {}));
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.step();
    h.step();
    h.step();
    h.event(Event::Paste("pasted into the field".into()));
    for _ in 0..4 {
        h.step();
    }
    assert_eq!(h.state().tab_count(), 1);
}

#[test]
fn the_about_and_menu_entries_exist() {
    let mut h = harness(with_settings(|_| {}));
    h.step();
    h.get_by_label("Help").click();
    h.step();
    h.step();
    assert!(
        h.query_all_by_label_contains("Command palette")
            .next()
            .is_some()
    );
    assert!(
        h.query_all_by_label_contains("Check for updates")
            .next()
            .is_some()
    );
}

#[test]
fn closing_the_settings_while_recording_keeps_the_shortcuts_alive() {
    let mut h = with_view(with_settings(|_| {}));
    open_settings(&mut h);
    h.get_by_label("Keyboard").click();
    settle(&mut h);
    h.get_by_role(egui::accesskit::Role::TextInput).click();
    settle(&mut h);
    h.event(Event::Text("mark after".into()));
    settle(&mut h);
    h.get_by_label("Change").click();
    settle(&mut h);
    assert!(h.query_by_label("Cancel").is_some());
    // Close the window with its X button while the recorder is waiting.
    h.get_by_label("Close window").click();
    settle(&mut h);
    assert!(h.query_by_label("Cancel").is_none());
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    step_until(&mut h, "find open", |a| {
        a.active_view().is_some_and(|v| v.find.open)
    });
}

#[test]
fn opening_the_settings_never_rewrites_values_outside_the_widget_ranges() {
    let edge = |s: &mut Settings| {
        s.font_size = 72.0;
        s.line_height = 3.0;
        s.time_gap_threshold_secs = 250_000.0;
        s.max_display_line_length = 10_000_000;
        s.cache_size_mb = 65_536;
        s.follow_poll_interval_ms = 60_000;
        s.recent_files_limit = 500;
        s.sort_max_rows = 10_000_000;
    };
    let mut expected = Settings::default();
    edge(&mut expected);
    // A large font leaves too few rows for `with_view` to wait for.
    let mut app = with_settings(edge);
    app.open_document("app.log", doc_from(logfmt_sample(100)));
    let mut h = harness(app);
    settle(&mut h);
    open_settings(&mut h);
    for section in [
        "Keyboard",
        "Files & following",
        "Columns & time",
        "Updates",
        "System",
        "Appearance",
    ] {
        h.get_by_label(section).click();
        settle(&mut h);
        assert_eq!(*h.state().settings(), expected, "after opening {section}");
    }
}
