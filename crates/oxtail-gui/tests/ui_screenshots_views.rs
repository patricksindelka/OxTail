//! Screenshot scenes for the view polish (severity colours, find/filter bars,
//! status bar, minimap, merged view, detail pane). Ignored by default: they
//! need a Vulkan driver (lavapipe in a container). Run:
//!
//! ```sh
//! SHOT_DIR=/tmp/shots-views-after cargo test -p oxtail-gui --test ui_screenshots_views -- --ignored
//! ```
mod common;
use common::*;
use egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
use oxtail_config::ThemeChoice;

type Harness = egui_kittest::Harness<'static, oxtail_gui::OxTailApp>;

const THEMES: [(ThemeChoice, &str); 3] = [
    (ThemeChoice::Dark, "dark"),
    (ThemeChoice::Light, "light"),
    (ThemeChoice::HighContrast, "hc"),
];

fn shot(h: &mut Harness, name: &str) {
    for _ in 0..6 {
        h.step();
    }
    let img = h.render().expect("render");
    let dir = std::env::var("SHOT_DIR").unwrap_or_else(|_| "target/screenshots".into());
    std::fs::create_dir_all(&dir).expect("screenshot dir");
    img.save(format!("{dir}/{name}.png")).expect("save");
}

fn harness_for(app: oxtail_gui::OxTailApp, w: f32, hgt: f32) -> Harness {
    egui_kittest::Harness::builder()
        .with_size(egui::vec2(w, hgt))
        .build_ui_state(|ui, app: &mut oxtail_gui::OxTailApp| app.show(ui), app)
}

fn app_with(text: String, w: f32, hgt: f32) -> Harness {
    let mut app = new_app();
    app.open_document("app.log", doc_from(text));
    let mut h = harness_for(app, w, hgt);
    step_until(&mut h, "rows", |a| {
        a.active_view().is_some_and(|v| v.last_rows.len() > 10)
    });
    h
}

fn java_sample(n: usize) -> String {
    (0..n)
        .map(|i| {
            let lvl = ["INFO", "DEBUG", "WARN", "ERROR", "INFO", "TRACE", "INFO", "FATAL"][i % 8];
            format!(
                "2026-09-22 14:42:{:02}.{:03} [main] {lvl:<5} app.service.Db - Reading resource db/changelog/{i}.sql\n",
                i % 60,
                i
            )
        })
        .collect()
}

fn jsonl_sample(n: usize) -> String {
    (0..n)
        .map(|i| {
            let lvl = ["info", "debug", "warn", "error", "info", "trace"][i % 6];
            format!(
                "{{\"ts\":\"2026-09-29T10:{:02}:{:02}Z\",\"level\":\"{lvl}\",\"msg\":\"request {i} done\",\"took\":{}}}\n",
                (i / 60) % 60,
                i % 60,
                10 + i
            )
        })
        .collect()
}

fn dismiss_suggestion(h: &mut Harness) {
    step_until(h, "suggestion", |a| {
        a.active_view().is_some_and(|v| v.st.suggestion.is_some())
    });
    h.state_mut()
        .active_view_mut()
        .unwrap()
        .dismiss_suggestion();
    h.step();
}

fn accept_columns(h: &mut Harness) {
    step_until(h, "suggestion", |a| {
        a.active_view().is_some_and(|v| v.st.suggestion.is_some())
    });
    h.get_by_label("Accept").click();
    step_until(h, "table", |a| {
        a.active_view()
            .is_some_and(|v| v.table_active() && !v.tcache.is_empty())
    });
}

#[test]
#[ignore = "renders with wgpu; run with --ignored and SHOT_DIR"]
fn severity_in_plain_and_table_views() {
    for (theme, tag) in THEMES {
        // logfmt: raw text with the suggestion bar, then the table.
        let mut h = app_with(logfmt_sample(400), 1280.0, 700.0);
        h.state_mut().set_theme(theme);
        step_until(&mut h, "suggestion", |a| {
            a.active_view().is_some_and(|v| v.st.suggestion.is_some())
        });
        shot(&mut h, &format!("11_logfmt_text_{tag}"));
        accept_columns(&mut h);
        shot(&mut h, &format!("12_logfmt_table_{tag}"));
        // A selection over severity rows.
        h.state_mut().active_view_mut().unwrap().select_all();
        shot(&mut h, &format!("13_logfmt_table_selected_{tag}"));

        // log4j text and table.
        let mut h = app_with(java_sample(300), 1280.0, 700.0);
        h.state_mut().set_theme(theme);
        step_until(&mut h, "suggestion", |a| {
            a.active_view().is_some_and(|v| v.st.suggestion.is_some())
        });
        shot(&mut h, &format!("14_log4j_text_{tag}"));
        accept_columns(&mut h);
        shot(&mut h, &format!("15_log4j_table_{tag}"));

        // JSON lines table.
        let mut h = app_with(jsonl_sample(300), 1280.0, 700.0);
        h.state_mut().set_theme(theme);
        accept_columns(&mut h);
        shot(&mut h, &format!("16_jsonl_table_{tag}"));
    }
}

#[test]
#[ignore = "renders with wgpu; run with --ignored and SHOT_DIR"]
fn find_filter_status_and_pill() {
    for (theme, tag) in THEMES {
        let mut h = app_with(logfmt_sample(400), 1280.0, 700.0);
        h.state_mut().set_theme(theme);
        dismiss_suggestion(&mut h);
        // Find bar: the moment after typing (searching), then settled.
        h.key_press_modifiers(Modifiers::COMMAND, Key::F);
        h.step();
        h.event(egui::Event::Text("error".into()));
        h.step();
        shot(&mut h, &format!("20_find_searching_{tag}"));
        for _ in 0..200 {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        shot(&mut h, &format!("21_find_settled_{tag}"));
        // Filter panel with two entries and a query.
        h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::F);
        h.step();
        h.get_by_label("Add").click();
        h.step();
        {
            let v = h.state_mut().active_view_mut().unwrap();
            v.filter.entries[0].text = "level:error".into();
            v.filter.entries[0].query = true;
            v.filter.entries[1].text = "took".into();
            v.filter.entries[1].include = false;
            v.filter.entries[1].regex = true;
            v.filter.changed();
        }
        shot(&mut h, &format!("22_filter_panel_{tag}"));
    }
}

#[test]
#[ignore = "renders with wgpu; run with --ignored and SHOT_DIR"]
fn status_bar_states_and_wrap() {
    for (theme, tag) in THEMES {
        let mut h = app_with(logfmt_sample(400), 1280.0, 500.0);
        h.state_mut().set_theme(theme);
        shot(&mut h, &format!("30_status_following_{tag}"));
        h.state_mut().active_view_mut().unwrap().toggle_follow();
        shot(&mut h, &format!("31_status_paused_{tag}"));
        h.get_by_label("Wrap long lines").click();
        shot(&mut h, &format!("32_status_wrap_{tag}"));
    }
}

#[test]
#[ignore = "renders with wgpu; run with --ignored and SHOT_DIR"]
fn detail_pane() {
    for (theme, tag) in THEMES {
        let mut h = app_with(logfmt_sample(400), 1280.0, 700.0);
        h.state_mut().set_theme(theme);
        accept_columns(&mut h);
        let v = h.state_mut().active_view_mut().unwrap();
        v.detail_open = true;
        v.select_all();
        shot(&mut h, &format!("40_detail_{tag}"));
    }
}

fn merged_app() -> oxtail_gui::OxTailApp {
    let mut app = new_app();
    let long = "x".repeat(400);
    app.open_document(
        "a.log",
        doc_from(
            (0..200)
                .map(|i| {
                    format!(
                        "2026-09-29 10:{:02}:{:02} INFO service-a request {i} {long}\n",
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
                    let lvl = if i % 7 == 0 { "ERROR" } else { "WARN" };
                    format!(
                        "2026-09-29 10:{:02}:{:02} {lvl} service-b retry {i}\n",
                        (2 * i + 1) / 60,
                        (2 * i + 1) % 60
                    )
                })
                .collect(),
        ),
    );
    let ids: Vec<u64> = app.merge_candidates().iter().map(|c| c.0).collect();
    app.merge_tabs(&ids);
    app
}

#[test]
#[ignore = "renders with wgpu; run with --ignored and SHOT_DIR"]
fn merged_view() {
    for (theme, tag) in THEMES {
        let mut h = harness_for(merged_app(), 1280.0, 600.0);
        h.state_mut().set_theme(theme);
        for _ in 0..60 {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        shot(&mut h, &format!("50_merged_{tag}"));
    }
}

#[test]
#[ignore = "renders with wgpu; run with --ignored and SHOT_DIR"]
fn static_file_find_shows_no_new_lines_pill() {
    let mut h = app_with(logfmt_sample(400), 1280.0, 700.0);
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.step();
    h.event(egui::Event::Text("error".into()));
    for _ in 0..200 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    shot(&mut h, "60_static_find_no_pill");
}

#[test]
#[ignore = "renders with wgpu; run with --ignored and SHOT_DIR"]
fn plain_java_in_all_themes() {
    for (theme, tag) in THEMES {
        let mut h = app_with(java_sample(300), 1280.0, 500.0);
        h.state_mut().set_theme(theme);
        dismiss_suggestion(&mut h);
        shot(&mut h, &format!("70_java_plain_{tag}"));
    }
}
