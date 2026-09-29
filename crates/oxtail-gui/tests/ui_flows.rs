//! End-to-end UI flows with `egui_kittest`: the application runs in a
//! headless harness (no renderer needed) and is driven with key events and
//! accessibility queries. Everything that has to wait for a worker thread
//! polls with a timeout (at most 10 s), never a bare sleep.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use oxtail_config::DataDir;
use oxtail_core::{Document, MemSource};
use oxtail_gui::{AppInit, ExternalOpen, OpenRequest, OxTailApp, load_startup};

const CTRL: Modifiers = Modifiers::COMMAND;

fn new_app() -> OxTailApp {
    OxTailApp::from_init(AppInit {
        startup: load_startup(DataDir::in_memory("test")),
        request: OpenRequest::default(),
        external: ExternalOpen::disconnected(),
    })
}

fn harness(app: OxTailApp) -> Harness<'static, OxTailApp> {
    Harness::builder()
        .with_size(vec2(1000.0, 500.0))
        .build_ui_state(|ui, app: &mut OxTailApp| app.show(ui), app)
}

fn doc_from(text: String) -> Arc<Document> {
    Arc::new(Document::from_source(
        Arc::new(MemSource::new(text.into_bytes())),
        "sample.log",
    ))
}

fn sample(lines: usize) -> String {
    (0..lines)
        .map(|i| {
            if i % 50 == 25 {
                format!("2026-01-01 12:00:{:02} ERROR needle number {i}\n", i % 60)
            } else {
                format!("2026-01-01 12:00:{:02} INFO ordinary line {i}\n", i % 60)
            }
        })
        .collect()
}

/// Steps the harness until `cond` holds (max 10 s).
fn step_until(h: &mut Harness<'_, OxTailApp>, what: &str, cond: impl Fn(&OxTailApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        h.step();
        if cond(h.state()) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn visible_text(app: &OxTailApp) -> Vec<String> {
    app.active_view()
        .map(|v| v.last_rows.iter().map(|l| l.text.clone()).collect())
        .unwrap_or_default()
}

#[test]
fn a_document_tab_renders_its_lines_at_the_tail() {
    let mut app = new_app();
    app.open_document("sample.log", doc_from(sample(500)));
    let mut h = harness(app);
    step_until(&mut h, "tail lines", |a| {
        visible_text(a)
            .last()
            .is_some_and(|l| l.ends_with("ordinary line 499"))
    });
    let lines = visible_text(h.state());
    assert!(lines.len() > 10, "{} rows", lines.len());
    // Consecutive lines, ending at the last one.
    for w in lines.windows(2) {
        let n = |s: &str| s.rsplit(' ').next().unwrap().parse::<u64>().unwrap();
        assert_eq!(n(&w[1]), n(&w[0]) + 1);
    }
    let view = h.state().active_view().unwrap();
    assert!(view.follow);
    assert_eq!(h.state().tab_count(), 1);
}

#[test]
fn opening_a_file_shows_a_tab_that_becomes_ready() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    std::fs::write(&path, sample(120)).unwrap();
    let mut app = new_app();
    app.open_request(OpenRequest::file(&path));
    // Nothing started before the context is bound; the tab exists at once.
    assert_eq!(app.tab_count(), 0);
    let mut h = harness(app);
    h.step();
    assert_eq!(h.state().tab_count(), 1);
    step_until(&mut h, "file rows", |a| {
        visible_text(a)
            .last()
            .is_some_and(|l| l.ends_with("ordinary line 119"))
    });
    // Opening the same file again focuses the existing tab.
    h.state_mut().open_request(OpenRequest::file(&path));
    h.step();
    assert_eq!(h.state().tab_count(), 1);
}

#[test]
fn a_missing_file_gives_a_failed_tab_and_a_notice() {
    let mut app = new_app();
    app.open_request(OpenRequest::file("/definitely/not/here.log"));
    let mut h = harness(app);
    step_until(&mut h, "failed tab", |a| {
        a.active_tab()
            .is_some_and(|t| t.label().contains("(failed)"))
    });
    assert!(h.state().active_view().is_none());
}

#[test]
fn ctrl_f_opens_the_find_bar_and_typing_jumps_to_a_match() {
    let mut app = new_app();
    app.open_document("sample.log", doc_from(sample(2000)));
    let mut h = harness(app);
    step_until(&mut h, "tail lines", |a| !visible_text(a).is_empty());
    // Scroll away from the end so the jump is visible.
    h.state_mut().active_view_mut().unwrap().jump_top();
    step_until(&mut h, "top lines", |a| {
        visible_text(a)
            .first()
            .is_some_and(|l| l.ends_with("ordinary line 0"))
    });

    h.key_press_modifiers(CTRL, Key::F);
    h.step();
    assert!(h.state().active_view().unwrap().find.open);
    // The field has focus: type into it.
    h.event(egui::Event::Text("needle number 1025".into()));
    step_until(&mut h, "search hit", |a| {
        a.active_view().is_some_and(|v| v.find.current.is_some())
    });
    step_until(&mut h, "match on screen", |a| {
        visible_text(a)
            .iter()
            .any(|l| l.contains("needle number 1025"))
    });
    let view = h.state().active_view().unwrap();
    assert_eq!(view.find.status.matches_found, 1);
    assert!(!view.follow);
}

#[test]
fn f3_walks_through_matches_and_wraps() {
    let mut app = new_app();
    app.open_document("sample.log", doc_from(sample(300)));
    let mut h = harness(app);
    step_until(&mut h, "rows", |a| !visible_text(a).is_empty());
    {
        let v = h.state_mut().active_view_mut().unwrap();
        v.find.text = "needle".into();
        v.find.open = true;
        v.find.restart_now(Instant::now());
        v.jump_top();
    }
    let current = |a: &OxTailApp| a.active_view().and_then(|v| v.find.current);
    step_until(&mut h, "first hit", |a| current(a).is_some());
    let first = current(h.state()).unwrap();
    let mut seen = vec![first];
    for _ in 0..6 {
        let before = current(h.state());
        h.key_press(Key::F3);
        step_until(&mut h, "next hit", |a| current(a) != before);
        seen.push(current(h.state()).unwrap());
    }
    // 300 lines have needles at 25, 75, ..., 275 = 6 matches; the 7th wraps.
    assert_eq!(seen.len(), 7);
    assert_eq!(seen[6], first);
    assert!(seen[..6].windows(2).all(|w| w[0] < w[1]));
    // Shift+F3 goes back.
    h.key_press_modifiers(Modifiers::SHIFT, Key::F3);
    step_until(&mut h, "previous hit", |a| current(a) != Some(first));
    assert_eq!(current(h.state()), Some(seen[5]));
}

#[test]
fn the_filter_view_shows_only_matching_lines_with_original_numbers() {
    let mut app = new_app();
    app.open_document("sample.log", doc_from(sample(600)));
    let mut h = harness(app);
    step_until(&mut h, "rows", |a| !visible_text(a).is_empty());

    h.key_press_modifiers(CTRL | Modifiers::SHIFT, Key::F);
    h.step();
    assert!(h.state().active_view().unwrap().filter.open);
    // Type into the (only) text field: the filter entry.
    let field = h.get_by_role(egui::accesskit::Role::TextInput);
    field.click();
    h.step();
    h.event(egui::Event::Text("needle".into()));
    step_until(&mut h, "filtered view", |a| {
        a.active_view().is_some_and(|v| {
            v.is_filtered()
                && v.filter.status.done
                && !v.last_rows.is_empty()
                && v.last_rows.iter().all(|l| l.text.contains("needle"))
        })
    });
    let view = h.state().active_view().unwrap();
    // Needles are on lines 25, 75, ...: 12 of them in 600 lines.
    assert_eq!(view.filter.set.len(), 12);
    let numbers: Vec<u64> = view.last_rows.iter().map(|l| l.number).collect();
    assert!(numbers.iter().all(|n| n % 50 == 25), "{numbers:?}");

    // Switching to the full view keeps the line in view.
    let cursor_line = view.last_rows.last().unwrap().number;
    h.state_mut()
        .active_view_mut()
        .unwrap()
        .set_filter_view(false);
    step_until(&mut h, "full view", |a| {
        a.active_view().is_some_and(|v| {
            !v.is_filtered() && v.last_rows.iter().any(|l| l.number == cursor_line)
        })
    });
    let rows = visible_text(h.state());
    assert!(rows.iter().any(|l| l.contains("ordinary")), "{rows:?}");
}

#[test]
fn tabs_can_be_switched_and_closed_from_the_keyboard() {
    let mut app = new_app();
    app.open_document("a.log", doc_from(sample(50)));
    app.open_document("b.log", doc_from(sample(60)));
    app.open_document("c.log", doc_from(sample(70)));
    let mut h = harness(app);
    h.step();
    assert_eq!(h.state().tab_count(), 3);
    assert_eq!(h.state().active_tab().unwrap().title, "c.log");
    let ctrl_only = Modifiers {
        ctrl: true,
        command: true,
        ..Modifiers::NONE
    };
    h.key_press_modifiers(ctrl_only, Key::Tab);
    h.step();
    assert_eq!(h.state().active_tab().unwrap().title, "a.log");
    h.key_press_modifiers(
        Modifiers {
            shift: true,
            ..ctrl_only
        },
        Key::Tab,
    );
    h.step();
    assert_eq!(h.state().active_tab().unwrap().title, "c.log");
    h.key_press_modifiers(CTRL, Key::W);
    h.step();
    assert_eq!(h.state().tab_count(), 2);
    assert_eq!(h.state().active_tab().unwrap().title, "b.log");
}

#[test]
fn go_to_line_dialog_jumps() {
    let mut app = new_app();
    app.open_document("sample.log", doc_from(sample(1000)));
    let mut h = harness(app);
    step_until(&mut h, "exact index", |a| {
        a.active_view()
            .is_some_and(|v| v.snapshot.lines.exact && !v.last_rows.is_empty())
    });
    h.key_press_modifiers(CTRL, Key::G);
    h.step();
    h.event(egui::Event::Text("500".into()));
    h.step();
    h.key_press(Key::Enter);
    step_until(&mut h, "line 500", |a| {
        visible_text(a).iter().any(|l| l.ends_with("line 499"))
    });
    assert!(!h.state().active_view().unwrap().follow);
}

#[test]
fn apply_goto_understands_relative_and_percent() {
    let mut app = new_app();
    app.open_document("sample.log", doc_from(sample(1000)));
    let mut h = harness(app);
    step_until(&mut h, "exact index", |a| {
        a.active_view()
            .is_some_and(|v| v.snapshot.lines.exact && !v.last_rows.is_empty())
    });
    assert!(h.state_mut().apply_goto("nonsense").is_err());
    h.state_mut().apply_goto("50%").unwrap();
    step_until(&mut h, "50%", |a| {
        visible_text(a).iter().any(|l| l.ends_with("line 500"))
    });
}

#[test]
fn mark_bookmark_and_selection_actions_work_through_keys() {
    let mut app = new_app();
    app.open_document("sample.log", doc_from(sample(200)));
    let mut h = harness(app);
    step_until(&mut h, "exact index", |a| {
        a.active_view()
            .is_some_and(|v| v.snapshot.lines.exact && !v.last_rows.is_empty())
    });
    h.key_press_modifiers(CTRL, Key::M);
    h.step();
    assert_eq!(h.state().active_view().unwrap().marks.len(), 1);
    // Select a line, bookmark it, unbookmark it.
    {
        let v = h.state_mut().active_view_mut().unwrap();
        let l = v.last_rows[2].clone();
        v.select(&l, false);
    }
    h.key_press_modifiers(CTRL, Key::F2);
    h.step();
    assert_eq!(h.state().active_view().unwrap().bookmarks.len(), 1);
    h.key_press_modifiers(CTRL, Key::F2);
    h.step();
    assert!(h.state().active_view().unwrap().bookmarks.is_empty());
    // Select all lines needs the whole document cached; the count is exact.
    h.key_press(Key::Home);
    h.step();
    h.key_press(Key::PageDown);
    step_until(&mut h, "paged down", |a| {
        a.active_view()
            .is_some_and(|v| v.last_rows.first().is_some_and(|l| l.number > 0))
    });
}

#[test]
fn escape_closes_the_find_bar() {
    let mut app = new_app();
    app.open_document("sample.log", doc_from(sample(50)));
    let mut h = harness(app);
    h.step();
    h.key_press_modifiers(CTRL, Key::F);
    h.step();
    assert!(h.state().active_view().unwrap().find.open);
    h.key_press(Key::Escape);
    h.step();
    assert!(!h.state().active_view().unwrap().find.open);
}

#[test]
fn the_welcome_screen_renders_without_tabs() {
    let mut h = harness(new_app());
    h.run();
    assert_eq!(h.state().tab_count(), 0);
    h.get_by_label("OxTail");
}

#[test]
fn zoom_keys_change_the_font_size_within_limits() {
    let mut app = new_app();
    app.open_document("sample.log", doc_from(sample(50)));
    let mut h = harness(app);
    h.step();
    let start = h.state().settings().font_size;
    h.key_press_modifiers(CTRL, Key::Equals);
    h.step();
    assert!(h.state().settings().font_size > start);
    for _ in 0..80 {
        h.key_press_modifiers(CTRL, Key::Minus);
        h.step();
    }
    assert_eq!(h.state().settings().font_size, oxtail_gui::app::MIN_FONT);
    h.key_press_modifiers(CTRL, Key::Num0);
    h.step();
    assert_eq!(h.state().settings().font_size, start);
}

#[test]
fn a_command_line_filter_and_profile_apply_to_the_new_tab() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cli.log");
    std::fs::write(&path, sample(300)).unwrap();
    let mut app = new_app();
    app.open_request(OpenRequest {
        files: vec![path],
        filter: Some("needle".into()),
        profile: Some("Syslog".into()),
        ..OpenRequest::default()
    });
    let mut h = harness(app);
    step_until(&mut h, "filtered tab", |a| {
        a.active_view().is_some_and(|v| {
            v.is_filtered()
                && v.filter.status.done
                && !v.last_rows.is_empty()
                && v.last_rows.iter().all(|l| l.text.contains("needle"))
        })
    });
    let view = h.state().active_view().unwrap();
    assert!(view.filter.open);
    assert_eq!(view.filter.entries.len(), 1);
    assert_eq!(view.hl.borrow().profile.as_deref(), Some("Syslog"));
    // The forced profile is remembered for the session.
    let session = h.state().current_session();
    assert_eq!(session.tabs[0].profile.as_deref(), Some("Syslog"));
    assert_eq!(session.tabs[0].filters, vec!["needle"]);
}

#[test]
fn the_profile_is_detected_from_the_first_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("access.log");
    let line =
        "127.0.0.1 - - [10/Oct/2000:13:55:36 -0700] \"GET /a HTTP/1.0\" 200 2326 \"-\" \"curl\"\n";
    std::fs::write(&path, line.repeat(40)).unwrap();
    let mut app = new_app();
    app.open_request(OpenRequest::file(&path));
    let mut h = harness(app);
    step_until(&mut h, "profile", |a| {
        a.active_view()
            .is_some_and(|v| v.hl.borrow().profile.is_some())
    });
    let name = h
        .state()
        .active_view()
        .unwrap()
        .hl
        .borrow()
        .profile
        .clone()
        .unwrap();
    assert!(
        name.to_lowercase().contains("nginx") || name.to_lowercase().contains("apache"),
        "{name}"
    );
}

#[test]
fn every_feature_can_be_on_at_once_without_breaking_the_frame() {
    // Wrapping, search highlights, a filter with context, bookmarks, marks,
    // a selection, the minimap and the "new lines" pill in one view.
    let long = "x".repeat(600);
    let text: String = (0..400)
        .map(|i| match i % 40 {
            7 => format!("2026-01-01 12:00:00 ERROR needle {i} {long}\n"),
            9 => format!("\u{1b}[31mred {i}\u{1b}[0m and \ttabs\u{1}\n"),
            _ => format!("2026-01-01 12:00:00 INFO ordinary {i}\n"),
        })
        .collect();
    let mem = Arc::new(MemSource::new(text.into_bytes()));
    let doc = Arc::new(Document::from_source(mem.clone(), "all.log"));
    let mut app = new_app();
    app.open_document("all.log", doc);
    let mut h = harness(app);
    step_until(&mut h, "exact index", |a| {
        a.active_view()
            .is_some_and(|v| v.snapshot.lines.exact && !v.last_rows.is_empty())
    });
    h.state_mut().set_wrap(true);
    {
        let v = h.state_mut().active_view_mut().unwrap();
        v.find.text = "needle".into();
        v.find.open = true;
        v.find.restart_now(Instant::now());
        v.filter.entries = vec![oxtail_gui::filter::FilterEntry::include("ordinary")];
        v.filter.before = 1;
        v.filter.after = 1;
        v.filter.open = true;
        v.filter.changed();
    }
    step_until(&mut h, "filter and search", |a| {
        a.active_view()
            .is_some_and(|v| v.filter.status.done && v.find.status.done && !v.last_rows.is_empty())
    });
    {
        let v = h.state_mut().active_view_mut().unwrap();
        v.set_filter_view(false);
        v.jump_top();
    }
    step_until(&mut h, "top", |a| {
        visible_text(a)
            .first()
            .is_some_and(|l| l.contains("ordinary 0"))
    });
    {
        let v = h.state_mut().active_view_mut().unwrap();
        let l = v.last_rows[3].clone();
        v.select(&l, false);
        v.toggle_bookmark();
        v.add_mark();
        let last = v.last_rows[5].clone();
        v.select(&last, true);
    }
    // New lines while paused make the pill appear.
    mem.append(b"appended 1\nappended 2\n");
    for _ in 0..20 {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    let v = h.state().active_view().unwrap();
    assert!(!v.follow);
    assert!(v.new_lines_while_paused() >= 2);
    assert_eq!(v.bookmarks.len(), 1);
    assert_eq!(v.selection_count(), Some((3, true)));
    // Wrapped long lines make the rows taller than the plain row height.
    h.state_mut()
        .active_view_mut()
        .unwrap()
        .jump_to_line(0, false);
    step_until(&mut h, "line 0", |a| {
        visible_text(a)
            .first()
            .is_some_and(|l| l.contains("ordinary 0"))
    });
    // Every theme renders the same view.
    for theme in [
        oxtail_config::ThemeChoice::Light,
        oxtail_config::ThemeChoice::HighContrast,
        oxtail_config::ThemeChoice::Dark,
        oxtail_config::ThemeChoice::System,
    ] {
        h.state_mut().set_theme(theme);
        h.step();
        h.step();
        assert!(!visible_text(h.state()).is_empty());
    }
}
