//! Renders the app shell (command palette, settings, start screen) to PNG
//! files, to look at it without a display. Ignored by default (needs a Vulkan
//! driver; in a container, lavapipe). Run:
//!
//! ```sh
//! SHOT_DIR=/tmp/shots-shell cargo test -p oxtail-gui --test ui_screenshots_shell -- --ignored
//! ```
mod common;
use common::*;
use egui::{Event, Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use oxtail_config::{DataDir, Keymap, Session, ThemeChoice};
use oxtail_gui::{AppInit, ExternalOpen, OpenRequest, OxTailApp, load_startup};

fn shot(h: &mut Harness<'_, OxTailApp>, name: &str) {
    for _ in 0..6 {
        h.step();
    }
    let img = h.render().expect("render");
    let dir = std::env::var("SHOT_DIR").unwrap_or_else(|_| "target/screenshots".into());
    std::fs::create_dir_all(&dir).expect("screenshot dir");
    img.save(format!("{dir}/{name}.png")).expect("save");
}

fn app_with(f: impl FnOnce(&mut oxtail_gui::Startup)) -> OxTailApp {
    let mut startup = load_startup(DataDir::in_memory("shots"));
    f(&mut startup);
    OxTailApp::from_init(AppInit {
        startup,
        request: OpenRequest::default(),
        external: ExternalOpen::disconnected(),
    })
}

fn sized(app: OxTailApp, w: f32, h: f32) -> Harness<'static, OxTailApp> {
    Harness::builder()
        .with_size(egui::vec2(w, h))
        .build_ui_state(|ui, app: &mut OxTailApp| app.show(ui), app)
}

fn with_doc(mut app: OxTailApp, w: f32, h: f32) -> Harness<'static, OxTailApp> {
    app.open_document("app.log", doc_from(logfmt_sample(400)));
    let mut hh = sized(app, w, h);
    step_until(&mut hh, "rows", |a| {
        a.active_view().is_some_and(|v| v.last_rows.len() > 10)
    });
    hh
}

fn recents(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for (i, name) in [
        "nginx/access.log",
        "app/service.log",
        "syslog",
        "worker-3.log",
        "deploy-2026-09-28.log",
    ]
    .iter()
    .enumerate()
    {
        let p = dir.join(name);
        if i != 3 {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "x\n").unwrap();
        }
        out.push(p);
    }
    out
}

fn theme_shots(name: &str, make: impl Fn() -> Harness<'static, OxTailApp>) {
    for (suffix, theme) in [
        ("dark", ThemeChoice::Dark),
        ("light", ThemeChoice::Light),
        ("hc", ThemeChoice::HighContrast),
    ] {
        let mut h = make();
        h.state_mut().set_theme(theme);
        shot(&mut h, &format!("{name}_{suffix}"));
    }
}

#[test]
#[ignore = "renders with wgpu; run with --ignored and SHOT_DIR"]
fn start_screen() {
    let dir = tempfile::tempdir().unwrap();
    let files = recents(dir.path());
    // Without recent files.
    let mut h = sized(app_with(|_| {}), 1100.0, 760.0);
    h.state_mut().set_theme(ThemeChoice::Dark);
    shot(&mut h, "10_start_empty_dark");
    // With recent files (one missing).
    let f2 = files.clone();
    theme_shots("11_start_recent", move || {
        let f = f2.clone();
        let app = app_with(move |s| {
            let mut session = Session::new();
            session.recent_files = f;
            s.session = session;
        });
        let mut h = sized(app, 1100.0, 760.0);
        step_until(&mut h, "checked", |a| {
            a.recent_file_exists(a.recent_files().first().unwrap())
                .is_some()
        });
        h
    });
    // Narrow window and the less keymap.
    let f3 = files;
    let app = app_with(move |s| {
        let mut session = Session::new();
        session.recent_files = f3;
        s.session = session;
        s.settings.keymap = Keymap::Less;
    });
    let mut h = sized(app, 520.0, 700.0);
    shot(&mut h, "12_start_narrow_less");
}

#[test]
#[ignore = "renders with wgpu; run with --ignored and SHOT_DIR"]
fn palette() {
    let mut h = with_doc(app_with(|_| {}), 1100.0, 700.0);
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::P);
    step_until(&mut h, "open", |a| a.palette_open());
    shot(&mut h, "20_palette_empty");
    h.event(Event::Text("wrap".into()));
    shot(&mut h, "21_palette_query");
    let mut h = with_doc(app_with(|_| {}), 1100.0, 700.0);
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::P);
    h.event(Event::Text("view".into()));
    h.key_press(Key::ArrowDown);
    h.key_press(Key::ArrowDown);
    shot(&mut h, "22_palette_view_selected");
    h.state_mut().set_theme(ThemeChoice::Light);
    shot(&mut h, "23_palette_light");
    let mut h = sized(app_with(|_| {}), 600.0, 500.0);
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::P);
    h.event(Event::Text("s".into()));
    shot(&mut h, "24_palette_no_doc_narrow");
}

fn settings_harness(section: &str, theme: ThemeChoice) -> Harness<'static, OxTailApp> {
    let mut h = with_doc(app_with(|_| {}), 1100.0, 720.0);
    h.state_mut().set_theme(theme);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
    h.step();
    h.step();
    if section != "Appearance" {
        h.get_by_label(section).click();
    }
    h.step();
    h
}

#[test]
#[ignore = "renders with wgpu; run with --ignored and SHOT_DIR"]
fn settings() {
    for (i, section) in [
        "Appearance",
        "Keyboard",
        "Files & following",
        "Columns & time",
        "Updates",
        "System",
    ]
    .iter()
    .enumerate()
    {
        let mut h = settings_harness(section, ThemeChoice::Dark);
        shot(
            &mut h,
            &format!(
                "3{i}_settings_{}",
                section.split(' ').next().unwrap().to_lowercase()
            ),
        );
    }
    let mut h = settings_harness("Appearance", ThemeChoice::Light);
    shot(&mut h, "36_settings_light");
    let mut h = settings_harness("Keyboard", ThemeChoice::HighContrast);
    shot(&mut h, "37_settings_hc_keyboard");
    // Recording a shortcut, and a conflict.
    let mut h = with_doc(
        app_with(|s| {
            s.settings.keymap = Keymap::Less;
            s.settings
                .custom_keybindings
                .insert("edit.mark".into(), "Ctrl+K".into());
            s.settings
                .custom_keybindings
                .insert("search.filter".into(), "Ctrl+K".into());
            s.settings
                .custom_keybindings
                .insert("go.line".into(), "Ctrl+Banana".into());
        }),
        1100.0,
        720.0,
    );
    h.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
    h.step();
    h.get_by_label("Keyboard").click();
    h.step();
    shot(&mut h, "38_settings_keyboard_conflict");
}

#[test]
#[ignore = "renders with wgpu; run with --ignored and SHOT_DIR"]
fn small_window() {
    let mut h = with_doc(app_with(|_| {}), 560.0, 420.0);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
    h.step();
    h.step();
    shot(&mut h, "40_settings_small");
}
