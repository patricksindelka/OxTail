//! Key bindings end to end: the `less` preset, user overrides, warnings for
//! bad entries, text fields keeping their plain keys, and the menus and the
//! shortcuts window showing the active chords.

mod common;
use common::*;
use egui::{Event, Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use oxtail_config::{DataDir, Keymap, Settings};
use oxtail_gui::{AppInit, ExternalOpen, OpenRequest, OxTailApp, load_startup};

const SHIFT: Modifiers = Modifiers::SHIFT;

fn app_with(f: impl FnOnce(&mut Settings)) -> OxTailApp {
    let mut startup = load_startup(DataDir::in_memory("test"));
    f(&mut startup.settings);
    OxTailApp::from_init(AppInit {
        startup,
        request: OpenRequest::default(),
        external: ExternalOpen::disconnected(),
    })
}

fn view_of(app: OxTailApp) -> Harness<'static, OxTailApp> {
    let mut app = app;
    app.open_document("app.log", doc_from(logfmt_sample(300)));
    let mut h = harness(app);
    step_until(&mut h, "tail rows", |a| {
        visible_text(a)
            .last()
            .is_some_and(|l| l.contains("request 299 done"))
    });
    h
}

fn less() -> Harness<'static, OxTailApp> {
    view_of(app_with(|s| s.keymap = Keymap::Less))
}

fn first_row(app: &OxTailApp) -> String {
    visible_text(app).first().cloned().unwrap_or_default()
}

fn request_number(line: &str) -> Option<u32> {
    let rest = line.split("request ").nth(1)?;
    rest.split(' ').next()?.parse().ok()
}

#[test]
fn less_g_and_shift_g_jump_to_the_top_and_the_bottom() {
    let mut h = less();
    h.key_press(Key::G);
    step_until(&mut h, "top", |a| first_row(a).contains("request 0 done"));
    assert!(!h.state().active_view().unwrap().follow);
    h.key_press_modifiers(SHIFT, Key::G);
    step_until(&mut h, "bottom", |a| {
        visible_text(a)
            .last()
            .is_some_and(|l| l.contains("request 299 done"))
    });
    assert!(h.state().active_view().unwrap().follow);
}

#[test]
fn less_space_and_b_page_and_j_k_scroll() {
    let mut h = less();
    h.key_press(Key::G);
    step_until(&mut h, "top", |a| first_row(a).contains("request 0 done"));
    h.key_press(Key::Space);
    step_until(&mut h, "paged down", |a| {
        request_number(&first_row(a)).is_some_and(|n| n > 5)
    });
    let paged = request_number(&first_row(h.state())).unwrap();
    h.key_press(Key::B);
    step_until(&mut h, "paged up", |a| {
        request_number(&first_row(a)).is_some_and(|n| n < paged)
    });
    // j scrolls down a line, k back up.
    h.key_press(Key::G);
    step_until(&mut h, "top again", |a| {
        first_row(a).contains("request 0 done")
    });
    for _ in 0..5 {
        h.key_press(Key::J);
        h.step();
    }
    step_until(&mut h, "j scrolled", |a| {
        request_number(&first_row(a)).is_some_and(|n| n >= 1)
    });
    let at = request_number(&first_row(h.state())).unwrap();
    h.key_press(Key::K);
    step_until(&mut h, "k scrolled", |a| {
        request_number(&first_row(a)).is_some_and(|n| n < at)
    });
}

#[test]
fn less_slash_opens_find_and_n_steps_through_matches() {
    let mut h = less();
    h.key_press(Key::Slash);
    step_until(&mut h, "find open", |a| {
        a.active_view().is_some_and(|v| v.find.open)
    });
    h.event(Event::Text("request 1".into()));
    step_until(&mut h, "search settled", |a| {
        a.active_view()
            .is_some_and(|v| v.find.text == "request 1" && v.find.status.done)
    });
    // While the find field has the focus n is just a letter.
    h.event(Event::Text("n".into()));
    h.step();
    assert!(h.state().active_view().unwrap().find.text.ends_with('n'));
}

#[test]
fn plain_letters_do_not_fire_while_a_text_field_has_the_focus() {
    let mut h = less();
    h.key_press(Key::Slash);
    step_until(&mut h, "find open", |a| {
        a.active_view().is_some_and(|v| v.find.open)
    });
    h.step();
    h.step();
    // g would jump to the top; typed into the field it must not.
    h.key_press(Key::G);
    h.event(Event::Text("g".into()));
    for _ in 0..4 {
        h.step();
    }
    assert!(
        visible_text(h.state())
            .last()
            .is_some_and(|l| l.contains("request 299 done")),
        "the view jumped: {:?}",
        visible_text(h.state()).first()
    );
    assert_eq!(h.state().active_view().unwrap().find.text, "g");
    // Ctrl chords and Esc still work from the field.
    h.key_press_modifiers(Modifiers::COMMAND | SHIFT, Key::P);
    step_until(&mut h, "palette from the field", |a| a.palette_open());
    h.key_press(Key::Escape);
    step_until(&mut h, "palette closed", |a| !a.palette_open());
    h.key_press(Key::Escape);
    step_until(&mut h, "find closed", |a| {
        a.active_view().is_some_and(|v| !v.find.open)
    });
    // With the field gone the plain key works again.
    h.key_press(Key::G);
    step_until(&mut h, "top", |a| first_row(a).contains("request 0 done"));
}

#[test]
fn the_standard_preset_leaves_plain_letters_alone() {
    let mut h = view_of(app_with(|_| {}));
    h.key_press(Key::G);
    h.key_press(Key::J);
    h.key_press(Key::Space);
    h.key_press(Key::Slash);
    for _ in 0..4 {
        h.step();
    }
    assert!(
        visible_text(h.state())
            .last()
            .is_some_and(|l| l.contains("request 299 done"))
    );
    assert!(!h.state().active_view().unwrap().find.open);
    // Ctrl+F is the standard find, and stays available under less.
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    step_until(&mut h, "find", |a| {
        a.active_view().is_some_and(|v| v.find.open)
    });
    let mut h = less();
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    step_until(&mut h, "find under less", |a| {
        a.active_view().is_some_and(|v| v.find.open)
    });
}

#[test]
fn less_q_does_nothing() {
    let mut h = less();
    h.key_press(Key::Q);
    h.key_press_modifiers(SHIFT, Key::Q);
    for _ in 0..4 {
        h.step();
    }
    assert_eq!(h.state().tab_count(), 1);
    assert!(h.state().keymap().lookup(Key::Q, Modifiers::NONE).is_none());
}

#[test]
fn a_user_binding_replaces_the_preset_chord() {
    let mut h = view_of(app_with(|s| {
        s.custom_keybindings
            .insert("search.find".into(), "Ctrl+K".into());
    }));
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.step();
    h.step();
    assert!(!h.state().active_view().unwrap().find.open);
    h.key_press_modifiers(Modifiers::COMMAND, Key::K);
    step_until(&mut h, "find via Ctrl+K", |a| {
        a.active_view().is_some_and(|v| v.find.open)
    });
    assert!(
        !h.state()
            .notices()
            .iter()
            .any(|n| n.text.starts_with("Key bindings:"))
    );
}

#[test]
fn a_user_can_unbind_a_command() {
    let mut h = view_of(app_with(|s| {
        s.custom_keybindings.insert("search.find".into(), "".into());
    }));
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.step();
    h.step();
    assert!(!h.state().active_view().unwrap().find.open);
}

#[test]
fn bad_entries_are_shown_and_do_not_break_the_defaults() {
    let mut h = view_of(app_with(|s| {
        s.custom_keybindings
            .insert("search.find".into(), "Ctrl+Banana".into());
        s.custom_keybindings
            .insert("no.such_action".into(), "F9".into());
        s.custom_keybindings
            .insert("edit.copy".into(), "Ctrl+Y".into());
    }));
    let texts: Vec<String> = h.state().notices().iter().map(|n| n.text.clone()).collect();
    assert_eq!(
        texts
            .iter()
            .filter(|t| t.starts_with("Key bindings:"))
            .count(),
        3,
        "{texts:?}"
    );
    assert!(texts.iter().any(|t| t.contains("Banana")));
    assert!(texts.iter().any(|t| t.contains("no.such_action")));
    // Visible in the window, too.
    h.step();
    assert!(h.query_all_by_label_contains("Ctrl+Banana").count() >= 1);
    // The default Ctrl+F still works.
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    step_until(&mut h, "find", |a| {
        a.active_view().is_some_and(|v| v.find.open)
    });
}

#[test]
fn two_commands_on_one_chord_are_reported_as_a_conflict() {
    let h = view_of(app_with(|s| {
        s.custom_keybindings
            .insert("edit.mark".into(), "Ctrl+K".into());
        s.custom_keybindings
            .insert("search.filter".into(), "Ctrl+K".into());
    }));
    assert_eq!(h.state().keymap().conflicts().len(), 1);
    assert!(
        h.state()
            .notices()
            .iter()
            .any(|n| n.text.contains("Ctrl+K") && n.text.contains("first one wins"))
    );
}

#[test]
fn the_shortcuts_window_lists_the_active_chords() {
    let mut h = view_of(app_with(|s| {
        s.keymap = Keymap::Less;
        s.custom_keybindings
            .insert("edit.mark".into(), "Ctrl+Alt+K".into());
    }));
    h.key_press(Key::F1);
    step_until(&mut h, "shortcuts window", |_| true);
    h.step();
    h.get_by_label("Ctrl+Alt+K");
    h.get_by_label("Insert a mark after the last line");
    h.get_by_label("less-style key bindings.");
    // The default Ctrl+M is gone from the list.
    assert!(h.query_by_label("Ctrl+M").is_none());
}

#[test]
fn menus_show_the_active_chords() {
    let mut h = view_of(app_with(|s| {
        s.custom_keybindings
            .insert("file.open".into(), "Ctrl+Alt+K".into());
    }));
    h.get_by_label("File").click();
    h.step();
    h.step();
    assert!(
        h.query_all_by_label_contains("Ctrl+Alt+K").count() >= 1,
        "the File menu does not show the custom chord"
    );
    assert!(h.query_all_by_label_contains("Ctrl+O").count() == 0);
}

#[test]
fn ctrl_shift_clipboard_events_run_the_bound_command() {
    // egui-winit reports Cmd/Ctrl+C with any Shift/Alt as Event::Copy alone.
    let mut h = view_of(app_with(|s| {
        s.custom_keybindings
            .insert("view.wrap".into(), "Mod+Alt+C".into());
    }));
    assert!(!h.state().settings().wrap);
    h.event_modifiers(Event::Copy, Modifiers::COMMAND | Modifiers::ALT);
    step_until(&mut h, "wrap on", |a| a.settings().wrap);
    // Plain Ctrl+C is still Copy: it does not toggle anything.
    h.event_modifiers(Event::Copy, Modifiers::COMMAND);
    h.step();
    h.step();
    assert!(h.state().settings().wrap);
}

#[test]
fn alt_chords_that_type_a_character_do_not_fire_in_a_text_field() {
    let mut h = view_of(app_with(|_| {}));
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    step_until(&mut h, "find open", |a| {
        a.active_view().is_some_and(|v| v.find.open)
    });
    h.step();
    let wrap = h.state().settings().wrap;
    // Option+Z types a character: the key press comes with a text event.
    // Both arrive in one frame, as they do from the windowing system.
    let input = h.input_mut();
    input.modifiers = Modifiers::ALT;
    input.events.push(Event::Text("\u{3a9}".into()));
    input.events.push(Event::Key {
        key: Key::Z,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::ALT,
    });
    h.step();
    let input = h.input_mut();
    input.events.push(Event::Key {
        key: Key::Z,
        physical_key: None,
        pressed: false,
        repeat: false,
        modifiers: Modifiers::ALT,
    });
    input.modifiers = Modifiers::NONE;
    h.step();
    assert_eq!(h.state().settings().wrap, wrap);
    // Without the text event Alt+Z is the command (proves the setup).
    h.key_press_modifiers(Modifiers::ALT, Key::Z);
    step_until(&mut h, "wrap toggled", |a| a.settings().wrap != wrap);
}
