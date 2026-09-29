//! Everything on screen at once: the table with its panels and windows, in
//! two panes, next to a merged tab. The point is that a frame with all of it
//! open runs without panicking and the state stays consistent; what is drawn
//! cannot be checked headlessly.

mod common;

use common::*;
use egui::{Key, Modifiers};
use oxtail_config::{SplitDirection, TimezoneSetting};
use oxtail_gui::filter::FilterEntry;
use oxtail_gui::timeview::RelMode;
use oxtail_gui::{OpenRequest, OxTailApp};

fn frames(h: &mut egui_kittest::Harness<'static, OxTailApp>, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

#[test]
fn every_panel_and_window_open_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("app.log");
    std::fs::write(&p, logfmt_sample(400)).unwrap();
    let q = dir.path().join("other.log");
    std::fs::write(&q, logfmt_sample(300)).unwrap();

    let mut app = new_app();
    app.set_timezone(TimezoneSetting::Named("Europe/Amsterdam".into()));
    app.set_gap_threshold(0.5);
    app.open_request(OpenRequest::file(&p));
    app.open_request(OpenRequest::file(&q));
    let mut h = harness(app);
    step_until(&mut h, "both tabs ready with a suggestion", |a| {
        a.tab_list().len() == 2
            && a.tab_list()
                .iter()
                .all(|t| t.view().is_some_and(|v| v.st.suggestion.is_some()))
    });
    // Table on in both tabs, with everything the view offers.
    for i in 0..2 {
        h.state_mut().select_tab(i);
        let v = h.state_mut().active_view_mut().unwrap();
        v.accept_suggestion();
        v.detail_open = true;
        v.rel_mode = RelMode::Selected;
        v.stats.open = true;
        v.export.open = true;
        v.chooser.open = true;
        v.filter.open = true;
        v.filter.entries = vec![
            FilterEntry::column_query("level:error", true),
            FilterEntry::column_query("nosuch:1", true),
            FilterEntry::include("request"),
        ];
        v.find.open = true;
        v.find.text = "request".into();
        v.find.restart_now(std::time::Instant::now());
    }
    step_until(&mut h, "tables and filters", |a| {
        a.tab_list()
            .iter()
            .all(|t| t.view().is_some_and(|v| v.table_active()))
            && a.active_view().is_some_and(|v| !v.tcache.is_empty())
    });
    // A split layout, a merged tab, and the dialogs of the application.
    h.state_mut().select_tab(0);
    h.state_mut().split_active(SplitDirection::Horizontal);
    let ids: Vec<u64> = h.state().merge_candidates().iter().map(|c| c.0).collect();
    h.state_mut().merge_tabs(&ids[..2]);
    h.key_press_modifiers(CTRL | Modifiers::SHIFT, Key::G);
    h.state_mut().start_cross_search("request");
    frames(&mut h, 8);
    h.key_press_modifiers(CTRL | Modifiers::SHIFT, Key::G);
    frames(&mut h, 3);
    // Move around: every pane, every tab.
    for i in 0..h.state().tab_count() {
        h.state_mut().select_tab(i);
        frames(&mut h, 2);
        if let Some(v) = h.state_mut().active_view_mut() {
            v.jump_top();
        }
        frames(&mut h, 2);
        h.key_press(Key::End);
        frames(&mut h, 2);
    }
    assert!(h.state().pane_count() >= 2);
    // Nothing got stuck: the app still answers to input.
    h.key_press_modifiers(CTRL, Key::F);
    frames(&mut h, 2);
}
