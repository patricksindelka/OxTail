//! Mouse wheel scrolling without easing.
//!
//! egui's `smooth_scroll_delta` spreads each wheel notch over a few frames
//! (90 % after 0.1 s, the rest trailing behind), which makes a log view feel
//! sluggish next to a native one. The log views read the raw wheel events
//! instead and move by whole rows per notch, the way BareTail and terminals do.

use egui::{Event, InputState, MouseWheelUnit, Vec2, vec2};

/// Rows moved per wheel notch (the Windows default).
pub const ROWS_PER_NOTCH: f32 = 3.0;

/// The scroll distance in points of this frame's wheel events: `x` sideways,
/// `y` down-is-negative like egui's deltas. Line units (wheel notches) move
/// [`ROWS_PER_NOTCH`] rows of `row_h`, page units `page_h`, and point units
/// (touchpads) pass through. Shift scrolls sideways; wheel events with the
/// zoom modifier (Ctrl/Cmd) are left to zooming.
pub fn wheel_delta(events: &[Event], row_h: f32, page_h: f32) -> Vec2 {
    let mut total = Vec2::ZERO;
    for e in events {
        let Event::MouseWheel {
            unit,
            delta,
            modifiers,
            ..
        } = e
        else {
            continue;
        };
        if modifiers.command || modifiers.mac_cmd {
            continue;
        }
        let mut d = match unit {
            MouseWheelUnit::Point => *delta,
            MouseWheelUnit::Line => *delta * (ROWS_PER_NOTCH * row_h),
            MouseWheelUnit::Page => *delta * page_h,
        };
        if modifiers.shift {
            // Some platforms already send Shift+wheel as horizontal.
            d = vec2(d.x + d.y, 0.0);
        }
        total += d;
    }
    if total.x.is_finite() && total.y.is_finite() {
        total
    } else {
        Vec2::ZERO
    }
}

/// [`wheel_delta`] of the current input.
pub fn input_wheel_delta(i: &InputState, row_h: f32, page_h: f32) -> Vec2 {
    wheel_delta(&i.events, row_h, page_h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Modifiers, TouchPhase};

    fn wheel(unit: MouseWheelUnit, delta: Vec2, modifiers: Modifiers) -> Event {
        Event::MouseWheel {
            unit,
            delta,
            phase: TouchPhase::Move,
            modifiers,
        }
    }

    #[test]
    fn a_notch_moves_three_rows_at_once() {
        let ev = [wheel(
            MouseWheelUnit::Line,
            vec2(0.0, -1.0),
            Modifiers::NONE,
        )];
        assert_eq!(wheel_delta(&ev, 20.0, 500.0), vec2(0.0, -60.0));
    }

    #[test]
    fn units_and_events_add_up() {
        let ev = [
            wheel(MouseWheelUnit::Point, vec2(1.5, 7.0), Modifiers::NONE),
            wheel(MouseWheelUnit::Page, vec2(0.0, 1.0), Modifiers::NONE),
            Event::PointerGone,
        ];
        assert_eq!(wheel_delta(&ev, 20.0, 500.0), vec2(1.5, 507.0));
    }

    #[test]
    fn shift_scrolls_sideways_and_zoom_is_ignored() {
        let ev = [
            wheel(MouseWheelUnit::Line, vec2(0.0, 1.0), Modifiers::SHIFT),
            wheel(MouseWheelUnit::Line, vec2(0.0, 1.0), Modifiers::COMMAND),
        ];
        assert_eq!(wheel_delta(&ev, 10.0, 500.0), vec2(30.0, 0.0));
    }

    #[test]
    fn nonsense_is_dropped() {
        let ev = [wheel(
            MouseWheelUnit::Point,
            vec2(0.0, f32::NAN),
            Modifiers::NONE,
        )];
        assert_eq!(wheel_delta(&ev, 10.0, 500.0), Vec2::ZERO);
        assert_eq!(wheel_delta(&[], 10.0, 500.0), Vec2::ZERO);
    }
}
