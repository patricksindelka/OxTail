//! Headless UI flows for long lines: they must be readable, either by
//! scrolling sideways (horizontal bar, Shift+wheel) or by wrapping (Alt+Z),
//! in the plain text view and in the column table.
//!
//! Every wait checks the condition that is asserted afterwards.

mod common;

use common::*;
use egui::{Event, Modifiers, MouseWheelUnit, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use oxtail_gui::OxTailApp;

/// 40 plain lines of ~600 characters ending in `END`.
fn plain_text() -> String {
    (0..40)
        .map(|i| format!("line {i:03} {}END\n", "x".repeat(600)))
        .collect()
}

/// 40 logfmt lines whose `msg` is 600 characters long and ends in `END`.
fn logfmt_text() -> String {
    (0..40)
        .map(|i| {
            format!(
                "time=2026-09-29T10:00:{:02}Z level=info msg=\"{}END\"\n",
                i % 60,
                "y".repeat(600)
            )
        })
        .collect()
}

/// Hovers `at` (a frame later the view knows the pointer is over it), then
/// turns the wheel.
fn wheel(h: &mut Harness<'_, OxTailApp>, at: egui::Pos2, delta: egui::Vec2, modifiers: Modifiers) {
    h.event(Event::PointerMoved(at));
    h.step();
    h.event(Event::MouseWheel {
        unit: MouseWheelUnit::Point,
        delta,
        modifiers,
        phase: egui::TouchPhase::Move,
    });
}

fn open_plain() -> Harness<'static, OxTailApp> {
    let mut app = new_app();
    app.open_document("long.log", doc_from(plain_text()));
    let mut h = harness(app);
    step_until(&mut h, "rows and a horizontal range", |a| {
        // The structure decision arrives on a worker; it redraws everything.
        a.active_view()
            .is_some_and(|v| v.st.decided && v.last_rows.len() > 5 && v.hbar_rect.is_some())
    });
    h
}

fn open_table() -> Harness<'static, OxTailApp> {
    let mut app = new_app();
    app.open_document("long.log", doc_from(logfmt_text()));
    let mut h = harness(app);
    step_until(&mut h, "a suggestion", |a| {
        a.active_view().is_some_and(|v| v.st.suggestion.is_some())
    });
    h.get_by_label("Accept").click();
    // The table is as wide as the longest message seen, and has a bar.
    step_until(&mut h, "a table wider than the view", |a| {
        a.active_view()
            .is_some_and(|v| v.table_active() && v.fill_chars >= 600.0 && v.hbar_rect.is_some())
    });
    h
}

fn view<'a>(h: &'a Harness<'_, OxTailApp>) -> &'a oxtail_gui::docview::DocView {
    h.state().active_view().unwrap()
}

#[test]
fn a_plain_view_scrolls_sideways_with_shift_wheel() {
    let mut h = open_plain();
    let v = view(&h);
    assert!(v.max_text_w > 4000.0, "range {}", v.max_text_w);
    assert_eq!(v.h_scroll, 0.0);
    wheel(
        &mut h,
        pos2(400.0, 200.0),
        vec2(0.0, -200.0),
        Modifiers::SHIFT,
    );
    step_until(&mut h, "the view moved right", |a| {
        a.active_view().is_some_and(|v| v.h_scroll >= 199.0)
    });
    // Never past the end of the longest line.
    wheel(
        &mut h,
        pos2(400.0, 200.0),
        vec2(0.0, -100_000.0),
        Modifiers::SHIFT,
    );
    let v = h.state().active_view().unwrap().max_text_w;
    step_until(&mut h, "clamped at the end", |a| {
        a.active_view()
            .is_some_and(|x| x.h_scroll > 1000.0 && x.h_scroll < v)
    });
    let max = view(&h).h_scroll;
    h.step();
    assert_eq!(view(&h).h_scroll, max, "stays clamped");
}

#[test]
fn the_horizontal_bar_drags_pages_and_takes_the_plain_wheel() {
    let mut h = open_plain();
    let bar = view(&h).hbar_rect.unwrap();
    assert!(bar.height() >= 12.0, "the bar is easy to hit: {bar:?}");

    // Wheel over the bar scrolls sideways.
    wheel(&mut h, bar.center(), vec2(0.0, -100.0), Modifiers::NONE);
    step_until(&mut h, "the wheel over the bar", |a| {
        a.active_view().is_some_and(|v| v.h_scroll >= 99.0)
    });
    let after_wheel = view(&h).h_scroll;

    // Dragging the thumb moves the view.
    let y = bar.center().y;
    let start = pos2(bar.left() + 10.0, y);
    h.hover_at(start);
    h.step();
    h.drag_at(start);
    h.step();
    for dx in [40.0, 120.0, 220.0] {
        h.hover_at(pos2(start.x + dx, y));
        h.step();
    }
    h.drop_at(pos2(start.x + 220.0, y));
    step_until(&mut h, "the thumb drag", |a| {
        a.active_view()
            .is_some_and(|v| v.h_scroll > after_wheel + 300.0)
    });
    let dragged = view(&h).h_scroll;

    // A click on the track beyond the thumb pages towards it.
    let far = pos2(bar.right() - 5.0, y);
    h.hover_at(far);
    h.step();
    h.drag_at(far);
    h.step();
    h.drop_at(far);
    step_until(&mut h, "a page to the right", |a| {
        a.active_view()
            .is_some_and(|v| v.h_scroll > dragged + 300.0)
    });
}

#[test]
fn wrapping_makes_plain_rows_taller_and_leaves_no_sideways_scroll() {
    let mut h = open_plain();
    let single = view(&h).last_heights[0];
    let rows_before = view(&h).last_rows.len();
    wheel(
        &mut h,
        pos2(400.0, 200.0),
        vec2(0.0, -300.0),
        Modifiers::SHIFT,
    );
    step_until(&mut h, "scrolled sideways", |a| {
        a.active_view().is_some_and(|v| v.h_scroll > 100.0)
    });
    h.state_mut().set_wrap(true);
    step_until(&mut h, "wrapped rows", |a| {
        a.active_view().is_some_and(|v| {
            v.wrap
                && v.h_scroll == 0.0
                && v.hbar_rect.is_none()
                && !v.last_heights.is_empty()
                && v.last_heights.iter().all(|h| *h > single * 3.0)
        })
    });
    assert!(view(&h).last_rows.len() < rows_before);
}

#[test]
fn a_table_with_long_messages_can_be_scrolled_to_their_end() {
    let mut h = open_table();
    let v = view(&h);
    assert!(v.max_text_w > 4000.0, "range {}", v.max_text_w);
    assert_eq!(v.h_scroll, 0.0);
    wheel(
        &mut h,
        pos2(400.0, 200.0),
        vec2(0.0, -100_000.0),
        Modifiers::SHIFT,
    );
    step_until(&mut h, "the end of the message", |a| {
        a.active_view().is_some_and(|v| {
            // Scrolled as far as the table goes: the fill column's right edge
            // is at the view's edge.
            let (_, r) = v
                .header_cells
                .iter()
                .max_by(|a, b| a.1.right().total_cmp(&b.1.right()))
                .unwrap();
            v.h_scroll > 4000.0 && r.right() <= 1000.0 && r.right() > 800.0
        })
    });
}

#[test]
fn pinned_columns_stay_while_the_rest_scrolls() {
    let mut h = open_table();
    let (level, msg) = {
        let s = h.state().active_view().unwrap().st.parser.clone().unwrap();
        (
            s.schema().find("level").unwrap(),
            s.schema().find("msg").unwrap(),
        )
    };
    {
        let v = h.state_mut().active_view_mut().unwrap();
        let pos = v.st.layout.position(level).unwrap();
        v.st.layout.set_pinned(pos, true);
    }
    let x_of = |a: &OxTailApp, col: usize| {
        a.active_view()?
            .header_cells
            .iter()
            .find(|(c, _)| *c == col)
            .map(|(_, r)| r.left())
    };
    step_until(&mut h, "level pinned first", |a| {
        a.active_view().is_some_and(|v| {
            v.st.layout
                .cols
                .first()
                .is_some_and(|c| c.col == level && c.pinned)
                && v.header_cells.first().is_some_and(|(c, _)| *c == level)
        })
    });
    let (level_x, msg_x) = (
        x_of(h.state(), level).unwrap(),
        x_of(h.state(), msg).unwrap(),
    );
    wheel(
        &mut h,
        pos2(400.0, 200.0),
        vec2(0.0, -300.0),
        Modifiers::SHIFT,
    );
    step_until(&mut h, "the unpinned columns moved", |a| {
        a.active_view().is_some_and(|v| v.h_scroll > 100.0)
            && x_of(a, msg).is_some_and(|x| x < msg_x - 100.0)
    });
    assert_eq!(
        x_of(h.state(), level),
        Some(level_x),
        "the pinned column stays"
    );
}

#[test]
fn a_wrapped_table_grows_its_rows_and_shows_the_end_of_the_message() {
    let mut h = open_table();
    let single = view(&h).last_heights[0];
    h.state_mut().set_wrap(true);
    step_until(&mut h, "wrapped table rows", |a| {
        a.active_view().is_some_and(|v| {
            v.wrap
                && v.h_scroll == 0.0
                && !v.last_heights.is_empty()
                && v.last_heights.iter().all(|h| *h > single * 3.0)
        })
    });
    // The wrapped cell's text ends with the marker on its last line.
    let v = view(&h);
    let fill = v.st.layout.fill_col().unwrap();
    let cells = v.wrap_cells.borrow();
    let mut checked = 0;
    for (_, col, g) in cells.galleys() {
        if col != fill {
            continue;
        }
        assert!(g.rows.len() > 1, "the message is wrapped");
        assert!(g.text().ends_with("END"), "{}", g.text());
        // The whole message is laid out, not cut at the edge of the view.
        assert!(g.size().y > single * 3.0);
        checked += 1;
    }
    assert!(checked > 0, "wrapped fill cells were laid out");
    drop(cells);
    // Other cells stay on one line: the rows are as tall as the message only.
    let v = view(&h);
    let hs = &v.last_heights;
    assert!(hs.windows(2).all(|w| (w[0] - w[1]).abs() < 0.5), "{hs:?}");
    // Turning wrap off brings the sideways range back.
    h.state_mut().set_wrap(false);
    step_until(&mut h, "single-line rows again", |a| {
        a.active_view().is_some_and(|v| {
            !v.wrap && v.hbar_rect.is_some() && v.last_heights.iter().all(|x| *x < single * 1.5)
        })
    });
}
