//! Session, settings, profiles and IPC-style requests against a real data
//! folder in a temporary directory.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::unbounded;
use egui::{Key, Modifiers, vec2};
use egui_kittest::Harness;
use oxtail_config::{DataDir, PathMapper, ResolveInput, Session, TabState};
use oxtail_gui::ruleeditor::EditorAction;
use oxtail_gui::{AppInit, ExternalOpen, OpenRequest, OxTailApp, load_startup};
use oxtail_highlight::{ColorRef, Rule, SemanticColor, Style};

fn data_dir(root: &Path) -> DataDir {
    DataDir::resolve(ResolveInput {
        cli_override: Some(root.to_path_buf()),
        exe_path: PathBuf::new(),
        installed_dir: None,
    })
}

fn harness_with(init: AppInit) -> Harness<'static, OxTailApp> {
    Harness::builder()
        .with_size(vec2(900.0, 500.0))
        .build_ui_state(
            |ui, app: &mut OxTailApp| app.show(ui),
            OxTailApp::from_init(init),
        )
}

fn init_for(root: &Path) -> AppInit {
    AppInit {
        startup: load_startup(data_dir(root)),
        request: OpenRequest::default(),
        external: ExternalOpen::disconnected(),
    }
}

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

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn write_log(dir: &Path, name: &str, lines: usize) -> PathBuf {
    let path = dir.join(name);
    let text: String = (0..lines).map(|i| format!("{name} line {i}\n")).collect();
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn the_session_is_restored_and_saved_on_exit() {
    let tmp = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let a = write_log(logs.path(), "a.log", 400);
    let b = write_log(logs.path(), "b.log", 50);

    // A session from an earlier run: two tabs, the first scrolled to line 200.
    let mut session = Session::new();
    session.tabs = vec![
        TabState {
            path: a.clone(),
            follow: false,
            scroll_anchor_line: 200,
            ..TabState::default()
        },
        TabState {
            path: b.clone(),
            follow: true,
            profile: Some("Generic".into()),
            ..TabState::default()
        },
    ];
    session.active_tab = 0;
    session
        .save(
            &tmp.path().join("session.json"),
            &PathMapper::absolute_only(),
        )
        .unwrap();

    let mut h = harness_with(init_for(tmp.path()));
    h.step();
    assert_eq!(h.state().tab_count(), 2);
    assert_eq!(h.state().active_tab().unwrap().title, "a.log");
    step_until(&mut h, "restored position", |app| {
        app.active_view().is_some_and(|v| {
            !v.follow
                && v.snapshot.lines.exact
                && v.last_rows.iter().all(|l| l.number_exact)
                && v.last_rows.first().is_some_and(|l| l.number == 200)
        })
    });

    // Save on exit: both tabs come back next time, with the scroll position.
    h.state_mut().shutdown();
    let saved = Session::load(
        &tmp.path().join("session.json"),
        &PathMapper::absolute_only(),
    );
    assert_eq!(saved.tabs.len(), 2);
    assert_eq!(saved.tabs[0].path, a);
    assert!(!saved.tabs[0].follow);
    // The top line is what is saved, so restoring is stable across restarts.
    assert_eq!(saved.tabs[0].scroll_anchor_line, 200);
    assert_eq!(saved.tabs[1].path, b);
    assert_eq!(saved.tabs[1].profile.as_deref(), Some("Generic"));
    assert!(saved.recent_files.contains(&a));
}

#[test]
fn settings_changes_are_written_after_a_short_delay() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = harness_with(init_for(tmp.path()));
    h.step();
    let before = h.state().settings().font_size;
    h.key_press_modifiers(Modifiers::COMMAND, Key::Equals);
    h.step();
    let path = tmp.path().join("settings.toml");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        h.step();
        assert!(Instant::now() < deadline, "settings.toml was not written");
        std::thread::sleep(Duration::from_millis(20));
    }
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains(&format!("font_size = {}", before + 1.0)),
        "{text}"
    );
}

#[test]
fn a_profile_saved_from_the_rule_editor_is_written_and_hot_reloaded() {
    let tmp = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let log = write_log(logs.path(), "x.log", 30);
    let mut app = OxTailApp::from_init(init_for(tmp.path()));
    app.open_request(OpenRequest::file(&log));
    let mut h = Harness::builder()
        .with_size(vec2(900.0, 500.0))
        .build_ui_state(|ui, app: &mut OxTailApp| app.show(ui), app);
    step_until(&mut h, "tab", |a| a.active_view().is_some());

    let rules = vec![
        Rule::literal("hello", "line")
            .styled(Style::fg(ColorRef::solid(SemanticColor::Success)).bold()),
    ];
    h.state_mut().handle_editor_action(EditorAction::Save {
        name: "My Team".into(),
        rules,
        base: None,
    });
    wait_for_file(&tmp.path().join("profiles").join("My_Team.toml"));
    // The watcher notices the new file and the profile list follows.
    step_until(&mut h, "hot reload", |a| {
        a.profile_names().iter().any(|n| n == "My Team")
    });
    // The tab uses the saved profile.
    let profile = h.state().active_view().unwrap().hl.borrow().profile.clone();
    assert_eq!(profile.as_deref(), Some("My Team"));

    // A hand-edited change to the file is picked up too.
    let path = tmp.path().join("profiles").join("My_Team.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{text}\n# edited by hand\n")).unwrap();
    std::fs::write(
        tmp.path().join("profiles").join("Extra.toml"),
        "name = \"Extra\"\n[[rules]]\nmatch = { literal = \"z\" }\n",
    )
    .unwrap();
    step_until(&mut h, "second profile", |a| {
        a.profile_names().iter().any(|n| n == "Extra")
    });
}

#[test]
fn requests_from_another_instance_open_tabs() {
    let tmp = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let a = write_log(logs.path(), "first.log", 20);
    let b = write_log(logs.path(), "second.log", 20);
    let (tx, rx) = unbounded();
    let external = ExternalOpen {
        rx,
        ctx_slot: std::sync::Arc::new(std::sync::Mutex::new(None)),
    };
    let mut init = init_for(tmp.path());
    init.external = external;
    init.request = OpenRequest::file(&a);
    let mut h = harness_with(init);
    step_until(&mut h, "command line file", |app| {
        app.tab_count() == 1 && app.active_view().is_some()
    });
    tx.send(OpenRequest {
        files: vec![a.clone(), b.clone()],
        tail_lines: Some(5),
        ..OpenRequest::default()
    })
    .unwrap();
    step_until(&mut h, "second tab", |app| app.tab_count() == 2);
    // The already open file was only focused, the new one is active.
    assert_eq!(h.state().active_tab().unwrap().title, "second.log");
    step_until(&mut h, "tail lines", |app| {
        app.active_view()
            .is_some_and(|v| v.last_rows.first().is_some_and(|l| l.number == 15))
    });
}

#[test]
fn the_search_history_is_kept_in_the_data_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let a = write_log(logs.path(), "h.log", 40);
    let mut init = init_for(tmp.path());
    init.request = OpenRequest::file(&a);
    let mut h = harness_with(init);
    step_until(&mut h, "tab", |app| app.active_view().is_some());
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.step();
    h.event(egui::Event::Text("line 7".into()));
    h.step();
    h.key_press(Key::Enter);
    let path = tmp.path().join("search-history.json");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        h.step();
        assert!(Instant::now() < deadline, "history was not saved");
        std::thread::sleep(Duration::from_millis(5));
    }
    let history: Vec<String> = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(history, vec!["line 7"]);
    // And it is loaded again at the next start.
    assert_eq!(load_startup(data_dir(tmp.path())).history, history);
}
