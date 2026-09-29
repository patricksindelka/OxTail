//! Helpers shared by the headless UI flow tests.
#![allow(dead_code)] // each test file uses a subset

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Modifiers, vec2};
use egui_kittest::Harness;
use oxtail_config::DataDir;
use oxtail_core::{Document, MemSource};
use oxtail_gui::{AppInit, ExternalOpen, OpenRequest, OxTailApp, load_startup};

pub const CTRL: Modifiers = Modifiers::COMMAND;

/// An app with in-memory configuration (nothing is written).
pub fn new_app() -> OxTailApp {
    OxTailApp::from_init(AppInit {
        startup: load_startup(DataDir::in_memory("test")),
        request: OpenRequest::default(),
        external: ExternalOpen::disconnected(),
    })
}

/// A headless harness around `app`.
pub fn harness(app: OxTailApp) -> Harness<'static, OxTailApp> {
    Harness::builder()
        .with_size(vec2(1000.0, 500.0))
        .build_ui_state(|ui, app: &mut OxTailApp| app.show(ui), app)
}

/// A document over `text`.
pub fn doc_from(text: String) -> Arc<Document> {
    Arc::new(Document::from_source(
        Arc::new(MemSource::new(text.into_bytes())),
        "sample.log",
    ))
}

/// Steps the harness until `cond` holds (max 10 s).
pub fn step_until(h: &mut Harness<'_, OxTailApp>, what: &str, cond: impl Fn(&OxTailApp) -> bool) {
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

/// logfmt lines: every fifth is an error, with timestamps one second apart.
pub fn logfmt_sample(lines: usize) -> String {
    (0..lines)
        .map(|i| {
            let level = if i % 5 == 0 { "error" } else { "info" };
            format!(
                "ts=2026-09-29T10:{:02}:{:02}Z level={level} took={}ms msg=\"request {i} done\"\n",
                (i / 60) % 60,
                i % 60,
                10 + i
            )
        })
        .collect()
}

/// The text of the rows drawn in the last frame.
pub fn visible_text(app: &OxTailApp) -> Vec<String> {
    app.active_view()
        .map(|v| v.last_rows.iter().map(|l| l.text.clone()).collect())
        .unwrap_or_default()
}
