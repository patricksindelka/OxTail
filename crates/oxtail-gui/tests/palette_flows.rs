//! The command palette end to end: open with Ctrl+Shift+P, type, move with
//! the arrow keys, run with Enter, close with Esc, all from the keyboard.

mod common;
use common::*;
use egui::{Event, Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use oxtail_gui::OxTailApp;

const CTRL_SHIFT: Modifiers = Modifiers {
    alt: false,
    ctrl: false,
    shift: true,
    mac_cmd: false,
    command: true,
};

fn app_with_view() -> Harness<'static, OxTailApp> {
    let mut app = new_app();
    app.open_document("app.log", doc_from(logfmt_sample(300)));
    let mut h = harness(app);
    step_until(&mut h, "rows", |a| {
        a.active_view().is_some_and(|v| v.last_rows.len() > 5)
    });
    h
}

fn open_palette(h: &mut Harness<'_, OxTailApp>) {
    h.key_press_modifiers(CTRL_SHIFT, Key::P);
    step_until(h, "palette open", |a| a.palette_open());
}

fn type_query(h: &mut Harness<'_, OxTailApp>, text: &str) {
    h.event(Event::Text(text.to_string()));
    let want = text.to_string();
    step_until(h, "query typed", |a| a.palette_query() == want);
}

#[test]
fn ctrl_shift_p_opens_the_palette_and_enter_runs_the_typed_command() {
    let mut h = app_with_view();
    assert!(!h.state().active_view().unwrap().wrap);
    open_palette(&mut h);
    type_query(&mut h, "line wrap");
    // The best match is the wrapping toggle.
    assert_eq!(h.state().palette_result_ids()[0], "view.wrap");
    h.get_by_label("Toggle line wrapping");
    h.key_press(Key::Enter);
    step_until(&mut h, "wrap on and palette closed", |a| {
        !a.palette_open() && a.active_view().is_some_and(|v| v.wrap)
    });
    assert!(h.state().settings().wrap);
    assert_eq!(h.state().palette_recent(), ["view.wrap"]);
    // Recently used commands come first for an empty query.
    open_palette(&mut h);
    assert_eq!(h.state().palette_result_ids()[0], "view.wrap");
    h.key_press(Key::Enter);
    step_until(&mut h, "wrap off", |a| {
        !a.palette_open() && a.active_view().is_some_and(|v| !v.wrap)
    });
}

#[test]
fn escape_closes_the_palette_without_running_anything() {
    let mut h = app_with_view();
    open_palette(&mut h);
    type_query(&mut h, "wrap");
    h.key_press(Key::Escape);
    step_until(&mut h, "closed", |a| !a.palette_open());
    assert!(!h.state().active_view().unwrap().wrap);
    assert!(h.state().palette_recent().is_empty());
    // Ctrl+Shift+P toggles as well.
    open_palette(&mut h);
    h.key_press_modifiers(CTRL_SHIFT, Key::P);
    step_until(&mut h, "closed again", |a| !a.palette_open());
}

#[test]
fn arrow_keys_move_the_selection_and_enter_runs_the_selected_command() {
    let mut h = app_with_view();
    open_palette(&mut h);
    type_query(&mut h, "theme");
    let ids = h.state().palette_result_ids();
    assert!(ids.len() >= 4, "{ids:?}");
    assert_eq!(h.state().palette_selected(), 0);
    h.key_press(Key::ArrowDown);
    step_until(&mut h, "first down", |a| a.palette_selected() == 1);
    h.key_press(Key::ArrowDown);
    step_until(&mut h, "second down", |a| a.palette_selected() == 2);
    // Up and down again.
    h.key_press(Key::ArrowUp);
    step_until(&mut h, "up", |a| a.palette_selected() == 1);
    h.key_press(Key::ArrowDown);
    step_until(&mut h, "down", |a| a.palette_selected() == 2);
    // Up from the first row wraps to the last selectable one.
    h.key_press(Key::ArrowUp);
    h.key_press(Key::ArrowUp);
    step_until(&mut h, "on the first", |a| a.palette_selected() == 0);
    h.key_press(Key::ArrowUp);
    step_until(&mut h, "wrapped", |a| a.palette_selected() > 2);
    h.key_press(Key::ArrowDown);
    step_until(&mut h, "wrapped forward", |a| a.palette_selected() == 0);
    h.key_press(Key::ArrowDown);
    h.key_press(Key::ArrowDown);
    step_until(&mut h, "third row", |a| a.palette_selected() == 2);
    let chosen = ids[2].clone();
    h.key_press(Key::Enter);
    step_until(&mut h, "ran", |a| !a.palette_open());
    let want = match chosen.as_str() {
        "theme.system" => oxtail_config::ThemeChoice::System,
        "theme.dark" => oxtail_config::ThemeChoice::Dark,
        "theme.light" => oxtail_config::ThemeChoice::Light,
        "theme.high_contrast" => oxtail_config::ThemeChoice::HighContrast,
        other => panic!("unexpected third result {other}"),
    };
    assert_eq!(h.state().settings().theme, want);
}

#[test]
fn typing_edits_the_query_and_resets_the_selection() {
    let mut h = app_with_view();
    open_palette(&mut h);
    type_query(&mut h, "the");
    h.key_press(Key::ArrowDown);
    step_until(&mut h, "moved", |a| a.palette_selected() == 1);
    h.event(Event::Text("m".into()));
    step_until(&mut h, "query grew", |a| a.palette_query() == "them");
    assert_eq!(h.state().palette_selected(), 0);
    h.key_press(Key::Backspace);
    step_until(&mut h, "query shrank", |a| a.palette_query() == "the");
}

#[test]
fn commands_that_do_not_apply_are_not_run() {
    // No file is open: "wrap" is unavailable, Enter does nothing and the
    // palette stays open.
    let mut h = harness(new_app());
    open_palette(&mut h);
    type_query(&mut h, "wrap");
    h.key_press(Key::Enter);
    h.step();
    h.step();
    assert!(h.state().palette_open());
    assert!(h.state().palette_recent().is_empty());
    assert!(!h.state().settings().wrap);
    // Global commands still run.
    h.key_press(Key::Backspace);
    h.key_press(Key::Backspace);
    h.key_press(Key::Backspace);
    h.key_press(Key::Backspace);
    step_until(&mut h, "cleared", |a| a.palette_query().is_empty());
    type_query(&mut h, "settings");
    assert_eq!(h.state().palette_result_ids()[0], "app.settings");
    h.key_press(Key::Enter);
    step_until(&mut h, "closed", |a| !a.palette_open());
    h.step();
    assert!(h.get_all_by_label("Appearance").count() >= 1);
}

#[test]
fn the_other_keys_are_left_alone_while_the_palette_is_open() {
    let mut h = app_with_view();
    open_palette(&mut h);
    // Ctrl+F would open the find bar; the palette owns the keyboard.
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.step();
    h.step();
    assert!(!h.state().active_view().unwrap().find.open);
    h.key_press(Key::Escape);
    step_until(&mut h, "closed", |a| !a.palette_open());
}

#[test]
fn a_click_on_a_result_runs_it() {
    let mut h = app_with_view();
    open_palette(&mut h);
    type_query(&mut h, "zoom out");
    let before = h.state().settings().font_size;
    h.get_by_label("Zoom out").click();
    step_until(&mut h, "zoomed out", |a| {
        !a.palette_open() && a.settings().font_size < before
    });
}

#[test]
fn recent_files_are_palette_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hello.log");
    std::fs::write(&path, "hello\n").unwrap();
    let mut app = new_app();
    app.open_request(oxtail_gui::OpenRequest::file(path));
    let mut h = harness(app);
    step_until(&mut h, "opened", |a| a.active_view().is_some());
    h.state_mut().close_tab(0);
    h.step();
    assert_eq!(h.state().tab_count(), 0);
    open_palette(&mut h);
    type_query(&mut h, "hello");
    let ids = h.state().palette_result_ids();
    assert!(ids[0].starts_with("recent:"), "{ids:?}");
    h.key_press(Key::Enter);
    step_until(&mut h, "reopened", |a| a.active_view().is_some());
}

#[test]
fn the_wrap_chip_the_shortcut_and_the_palette_are_one_setting() {
    let mut h = app_with_view();
    assert!(!h.state().settings().wrap);
    // The status bar chip changes the saved default too.
    h.get_by_label("Wrap long lines").click();
    step_until(&mut h, "chip on", |a| {
        a.settings().wrap && a.active_view().is_some_and(|v| v.wrap)
    });
    // Alt+Z switches it off again ...
    h.key_press_modifiers(Modifiers::ALT, Key::Z);
    step_until(&mut h, "shortcut off", |a| {
        !a.settings().wrap && a.active_view().is_some_and(|v| !v.wrap)
    });
    // ... and so does the palette entry, from the other direction.
    open_palette(&mut h);
    type_query(&mut h, "line wrap");
    h.key_press(Key::Enter);
    step_until(&mut h, "palette on", |a| {
        a.settings().wrap && a.active_view().is_some_and(|v| v.wrap)
    });
    h.get_by_label("Wrap long lines").click();
    step_until(&mut h, "chip off", |a| {
        !a.settings().wrap && a.active_view().is_some_and(|v| !v.wrap)
    });
}
