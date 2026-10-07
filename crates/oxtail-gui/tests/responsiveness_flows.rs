//! The app reacts in the frame the input arrives: wheel notches are not eased
//! over several frames, and menus and windows do not fade in. While it only
//! follows a growing file, it redraws at a paced rate.

mod common;

use common::*;
use egui::{Event, Modifiers, MouseWheelUnit, TouchPhase, pos2, vec2};

fn top_number(h: &egui_kittest::Harness<'_, oxtail_gui::OxTailApp>) -> u64 {
    h.state().active_view().unwrap().last_rows[0].number
}

#[test]
fn a_wheel_notch_scrolls_three_rows_in_one_frame() {
    let mut app = new_app();
    app.open_document("sample.log", doc_from(logfmt_sample(500)));
    let mut h = harness(app);
    step_until(&mut h, "rows", |a| {
        a.active_view().is_some_and(|v| v.last_rows.len() > 5)
    });
    h.state_mut()
        .active_view_mut()
        .unwrap()
        .jump_to_line(100, false);
    step_until(&mut h, "line 100 at the top", |a| {
        a.active_view()
            .is_some_and(|v| v.last_rows.first().is_some_and(|l| l.number == 100))
    });
    h.event(Event::PointerMoved(pos2(400.0, 200.0)));
    h.step();
    for (notch, expect) in [(-1.0, 103), (1.0, 100), (-2.0, 106)] {
        h.event(Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: vec2(0.0, notch),
            modifiers: Modifiers::NONE,
            phase: TouchPhase::Move,
        });
        h.step();
        assert_eq!(top_number(&h), expect, "after a notch of {notch}");
        // Nothing trails behind.
        h.step();
        assert_eq!(top_number(&h), expect, "a frame later");
    }
}

#[test]
fn nothing_animates() {
    let mut h = harness(new_app());
    h.step();
    let style = h.ctx.global_style();
    assert_eq!(style.animation_time, 0.0);
    assert_eq!(style.scroll_animation.duration.max, 0.0);
}

#[test]
fn repaints_are_paced_only_while_following() {
    let mut app = new_app();
    app.open_document("sample.log", doc_from(logfmt_sample(500)));
    let mut h = harness(app);
    step_until(&mut h, "rows", |a| {
        a.active_view().is_some_and(|v| v.last_rows.len() > 5)
    });
    assert!(h.state().active_view().unwrap().follow);
    assert!(h.state().repaints_paced(), "following: paced");
    // A wheel turn up stops following while the view is drawn; pacing ends
    // in that same frame, so the replies it asks for show at once.
    h.event(Event::PointerMoved(pos2(400.0, 200.0)));
    h.step();
    assert!(h.state().repaints_paced(), "hovering changes nothing");
    h.event(Event::MouseWheel {
        unit: MouseWheelUnit::Line,
        delta: vec2(0.0, 2.0),
        modifiers: Modifiers::NONE,
        phase: TouchPhase::Move,
    });
    h.step();
    assert!(!h.state().active_view().unwrap().follow);
    assert!(!h.state().repaints_paced(), "browsing: not paced");
}
