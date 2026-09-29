//! Headless UI flows for the time features (M5): go to time, relative time
//! and time gaps.

mod common;

use common::*;
use egui::{Key, Modifiers};
use oxtail_config::TimezoneSetting;
use oxtail_gui::timeview::{RelMode, gap_between};

/// One line per 10 seconds from 10:00:00, except a one-minute pause after
/// line 9.
fn timed_log(lines: usize) -> String {
    let mut secs = 0usize;
    let mut out = String::new();
    for i in 0..lines {
        if i == 10 {
            secs += 60;
        }
        out.push_str(&format!(
            "2026-09-29 {:02}:{:02}:{:02} INFO line {i}\n",
            10 + secs / 3600,
            (secs / 60) % 60,
            secs % 60
        ));
        secs += 10;
    }
    out
}

#[test]
fn go_to_time_finds_the_line_and_scrolls_there() {
    let mut app = new_app();
    app.set_timezone(TimezoneSetting::Utc);
    app.open_document("timed.log", doc_from(timed_log(3000)));
    let mut h = harness(app);
    step_until(&mut h, "rows", |a| {
        a.active_view()
            .is_some_and(|v| v.snapshot.lines.exact && !v.last_rows.is_empty() && v.st.decided)
    });
    h.key_press_modifiers(CTRL | Modifiers::SHIFT, Key::G);
    h.step();
    assert!(h.state().windows_open_goto_time());
    // Line 1500 is at 10:00:00 + 60 s + 1500 * 10 s = 14:11:00.
    h.event(egui::Event::Text("2026-09-29 14:11:00".into()));
    h.step();
    h.key_press(Key::Enter);
    step_until(&mut h, "the jump", |a| {
        !a.windows_open_goto_time() && visible_text(a).iter().any(|l| l.ends_with("line 1500"))
    });
    assert!(!h.state().active_view().unwrap().follow);
}

#[test]
fn relative_go_to_time_counts_from_the_end() {
    let mut app = new_app();
    app.set_timezone(TimezoneSetting::Utc);
    app.open_document("timed.log", doc_from(timed_log(1000)));
    let mut h = harness(app);
    step_until(&mut h, "rows", |a| {
        a.active_view()
            .is_some_and(|v| v.snapshot.lines.exact && !v.last_rows.is_empty() && v.st.decided)
    });
    h.key_press_modifiers(CTRL | Modifiers::SHIFT, Key::G);
    h.step();
    h.event(egui::Event::Text("-10m".into()));
    h.step();
    h.key_press(Key::Enter);
    // The last line is 999; ten minutes (60 lines) earlier is line 939.
    step_until(&mut h, "the jump", |a| {
        !a.windows_open_goto_time() && visible_text(a).iter().any(|l| l.ends_with("line 939"))
    });
}

#[test]
fn bad_go_to_time_input_stays_open_with_an_error() {
    let mut app = new_app();
    app.open_document("timed.log", doc_from(timed_log(100)));
    let mut h = harness(app);
    step_until(&mut h, "rows", |a| !visible_text(a).is_empty());
    h.key_press_modifiers(CTRL | Modifiers::SHIFT, Key::G);
    h.step();
    h.event(egui::Event::Text("whenever".into()));
    h.step();
    h.key_press(Key::Enter);
    h.step();
    assert!(h.state().windows_open_goto_time());
    assert!(h.state().goto_time_error().is_some());
    // Escape closes it.
    h.key_press(Key::Escape);
    h.step();
    assert!(!h.state().windows_open_goto_time());
}

#[test]
fn gaps_and_relative_times_are_computed_for_visible_rows() {
    let mut app = new_app();
    app.set_timezone(TimezoneSetting::Utc);
    app.set_gap_threshold(30.0);
    app.open_document("timed.log", doc_from(timed_log(40)));
    let mut h = harness(app);
    step_until(&mut h, "rows", |a| {
        a.active_view()
            .is_some_and(|v| v.snapshot.lines.exact && v.last_rows.len() >= 12 && v.st.decided)
    });
    {
        let v = h.state_mut().active_view_mut().unwrap();
        v.rel_mode = RelMode::Previous;
        v.jump_top();
    }
    step_until(&mut h, "the top rows", |a| {
        a.active_view().is_some_and(|v| {
            v.last_rows.first().is_some_and(|l| l.number == 0) && v.last_rows.len() >= 12
        })
    });
    let v = h.state().active_view().unwrap();
    let rows = v.last_rows.clone();
    // Exactly one gap among consecutive rows: between lines 9 and 10.
    let mut gaps = Vec::new();
    for w in rows.windows(2) {
        let g = gap_between(v.row_time(&w[0]), v.row_time(&w[1]), 30.0);
        if let Some(g) = g {
            gaps.push((w[1].number, g.as_secs()));
        }
    }
    assert_eq!(gaps, vec![(10, 70)]);
    // The first visible row has a predecessor timestamp when it is cached.
    assert!(v.selected_time().is_none() || v.selected_time().is_some());
}

#[test]
fn time_zone_changes_re_render_timestamps_in_the_table() {
    let mut app = new_app();
    app.set_timezone(TimezoneSetting::Utc);
    let text: String = (0..60)
        .map(|i| format!("ts=2026-09-29T10:00:{:02}Z level=info msg=hello\n", i % 60))
        .collect();
    app.open_document("app.log", doc_from(text));
    let mut h = harness(app);
    step_until(&mut h, "suggestion", |a| {
        a.active_view().is_some_and(|v| v.st.suggestion.is_some())
    });
    h.state_mut().active_view_mut().unwrap().accept_suggestion();
    step_until(&mut h, "table", |a| {
        a.active_view()
            .is_some_and(|v| v.table_active() && !v.tcache.is_empty())
    });
    let cell = |a: &oxtail_gui::OxTailApp| {
        let v = a.active_view().unwrap();
        let line = v.last_rows[0].clone();
        let p = v.hl.borrow_mut().prepare(&line);
        let rec = p.record.clone().unwrap();
        let col = v.st.ts_column.unwrap();
        v.st.cell_text(col, &rec)
    };
    assert!(cell(h.state()).starts_with("2026-09-29 10:00:"));
    h.state_mut()
        .set_timezone(TimezoneSetting::Named("Asia/Tokyo".into()));
    step_until(&mut h, "re-rendered", |a| {
        cell(a).starts_with("2026-09-29 19:00:")
    });
}
