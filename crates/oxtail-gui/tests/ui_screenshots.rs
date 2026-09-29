//! Renders real frames of the app to PNG files, to look at the UI without a
//! display. Ignored by default (it needs a Vulkan driver; in a container,
//! lavapipe: `apt-get install mesa-vulkan-drivers`). Run:
//!
//! ```sh
//! SHOT_DIR=/tmp/shots cargo test -p oxtail-gui --test ui_screenshots -- --ignored
//! ```
//!
//! Add a scene here when you change how something looks, and look at it.
mod common;
use common::*;
use egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;

fn shot(h: &mut egui_kittest::Harness<'_, oxtail_gui::OxTailApp>, name: &str) {
    for _ in 0..6 {
        h.step();
    }
    let img = h.render().expect("render");
    let dir = std::env::var("SHOT_DIR").unwrap_or_else(|_| "target/screenshots".into());
    std::fs::create_dir_all(&dir).expect("screenshot dir");
    img.save(format!("{dir}/{name}.png")).expect("save");
}

fn app_with(
    text: String,
    w: f32,
    hgt: f32,
) -> egui_kittest::Harness<'static, oxtail_gui::OxTailApp> {
    let mut app = new_app();
    app.open_document("app.log", doc_from(text));
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::vec2(w, hgt))
        .build_ui_state(|ui, app: &mut oxtail_gui::OxTailApp| app.show(ui), app);
    step_until(&mut h, "rows", |a| {
        a.active_view().is_some_and(|v| v.last_rows.len() > 10)
    });
    h
}

#[test]
#[ignore = "renders with wgpu; run with --ignored and SHOT_DIR"]
fn screenshots() {
    // Table view + detail pane.
    let mut h = app_with(logfmt_sample(400), 1280.0, 800.0);
    step_until(&mut h, "suggestion", |a| {
        a.active_view().is_some_and(|v| v.st.suggestion.is_some())
    });
    h.get_by_label("Accept").click();
    step_until(&mut h, "table", |a| {
        a.active_view()
            .is_some_and(|v| v.table_active() && !v.tcache.is_empty())
    });
    shot(&mut h, "02_table");
    h.state_mut().active_view_mut().unwrap().detail_open = true;
    h.state_mut().active_view_mut().unwrap().select_all();
    shot(&mut h, "03_table_detail");
    // Find bar.
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.step();
    h.event(egui::Event::Text("error".into()));
    shot(&mut h, "04_find");
    h.key_press(Key::Escape);
    // Filter panel.
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::F);
    shot(&mut h, "05_filter");
    // Light theme.
    h.state_mut().set_theme(oxtail_config::ThemeChoice::Light);
    shot(&mut h, "06_light");
}

#[test]
#[ignore = "renders with wgpu; run with --ignored and SHOT_DIR"]
fn screenshots_plain_generic() {
    let text: String = (0..300).map(|i| {
        let lvl = ["INFO", "DEBUG", "WARN", "ERROR", "INFO"][i % 5];
        format!("2026-09-22 14:42:{:02}.{:03} [main] {lvl:<5} liquibase.changelog - Reading resource db/changelog/{i}.sql\n", i % 60, i)
    }).collect();
    let mut h = app_with(text, 1280.0, 800.0);
    shot(&mut h, "07_java_plain");
    h.state_mut()
        .active_view_mut()
        .unwrap()
        .set_filter_view(false);
    // Merged view of two tabs.
    let mut app = new_app();
    app.open_document(
        "a.log",
        doc_from(
            (0..200)
                .map(|i| {
                    format!(
                        "2026-09-29 10:{:02}:{:02} INFO service-a request {i}\n",
                        (2 * i) / 60,
                        (2 * i) % 60
                    )
                })
                .collect(),
        ),
    );
    app.open_document(
        "b.log",
        doc_from(
            (0..200)
                .map(|i| {
                    format!(
                        "2026-09-29 10:{:02}:{:02} WARN service-b retry {i}\n",
                        (2 * i + 1) / 60,
                        (2 * i + 1) % 60
                    )
                })
                .collect(),
        ),
    );
    let ids: Vec<u64> = app.merge_candidates().iter().map(|c| c.0).collect();
    app.merge_tabs(&ids);
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .build_ui_state(|ui, app: &mut oxtail_gui::OxTailApp| app.show(ui), app);
    for _ in 0..60 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    shot(&mut h, "08_merged");
}

#[test]
#[ignore = "renders with wgpu; run with --ignored and SHOT_DIR"]
fn screenshots_more() {
    // Empty start.
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .build_ui_state(
            |ui, app: &mut oxtail_gui::OxTailApp| app.show(ui),
            new_app(),
        );
    shot(&mut h, "09_empty");
    // Find with a settled search, plain text view.
    let mut h = app_with(logfmt_sample(400), 1280.0, 800.0);
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.step();
    h.event(egui::Event::Text("error".into()));
    for _ in 0..200 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    shot(&mut h, "10_find_settled");
}
