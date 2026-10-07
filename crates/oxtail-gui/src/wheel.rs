//! Mouse wheel scrolling without easing.
//!
//! egui's `smooth_scroll_delta` spreads each wheel notch over a few frames
//! (90 % after 0.1 s, the rest trailing behind), which makes a log view feel
//! sluggish next to a native one. The log views read the raw wheel events
//! instead and move by whole rows per notch, the way BareTail and terminals do.
//!
//! Reading them is not enough: egui still eases the events it sees, and asks
//! for a frame after frame until the easing has run out. So while the pointer
//! is over a log view, [`take_raw_wheel`] (the app's raw input hook) takes the
//! wheel events out of egui's input before egui sees them, and the views read
//! them back with [`frame_wheel_delta`]. Elsewhere (settings, side panels)
//! egui scrolls as usual.

use std::sync::Arc;

use egui::{Context, Event, Id, InputState, MouseWheelUnit, RawInput, Ui, Vec2, vec2};

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

fn hovered_id() -> Id {
    Id::new("oxtail-wheel-over-log")
}

fn taken_id() -> Id {
    Id::new("oxtail-wheel-taken")
}

/// A log view calls this in every frame the pointer is over it: the next
/// frame's wheel events are then kept from egui ([`take_raw_wheel`]).
pub fn mark_hovered(ctx: &Context) {
    ctx.data_mut(|d| d.insert_temp(hovered_id(), true));
}

/// The raw input hook: when a log view was hovered last frame, moves the
/// wheel events (except zooming ones) out of `raw` for [`frame_wheel_delta`].
pub fn take_raw_wheel(ctx: &Context, raw: &mut RawInput) {
    let over = ctx
        .data_mut(|d| d.remove_temp::<bool>(hovered_id()))
        .unwrap_or(false);
    let mut taken = Vec::new();
    if over {
        raw.events.retain(|e| match e {
            Event::MouseWheel { modifiers, .. } if !(modifiers.command || modifiers.mac_cmd) => {
                taken.push(e.clone());
                false
            }
            _ => true,
        });
    }
    ctx.data_mut(|d| d.insert_temp(taken_id(), Arc::new(taken)));
}

/// This frame's wheel scrolling for the hovered log view: the events taken
/// from egui plus any egui still has (see [`wheel_delta`]). The taken events
/// are consumed, like egui's own input: when egui runs a second pass of the
/// frame (to settle a layout), they do not scroll again.
pub fn frame_wheel_delta(ui: &Ui, row_h: f32, page_h: f32) -> Vec2 {
    let taken: Option<Arc<Vec<Event>>> = ui.ctx().data_mut(|d| d.remove_temp(taken_id()));
    let own = taken.map_or(Vec2::ZERO, |t| wheel_delta(&t, row_h, page_h));
    own + ui.input(|i| input_wheel_delta(i, row_h, page_h))
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
    fn wheel_events_over_a_log_view_are_kept_from_egui() {
        let ctx = Context::default();
        let events = vec![
            wheel(MouseWheelUnit::Line, vec2(0.0, -1.0), Modifiers::NONE),
            wheel(MouseWheelUnit::Line, vec2(0.0, 1.0), Modifiers::COMMAND),
            Event::PointerMoved(egui::pos2(1.0, 2.0)),
        ];
        // Not over a log view: egui keeps everything.
        let mut raw = RawInput {
            events: events.clone(),
            ..RawInput::default()
        };
        take_raw_wheel(&ctx, &mut raw);
        assert_eq!(raw.events, events);
        // Over a log view: the plain wheel event is taken, zooming stays.
        mark_hovered(&ctx);
        take_raw_wheel(&ctx, &mut raw);
        assert_eq!(raw.events, events[1..].to_vec());
        let taken: Option<Arc<Vec<Event>>> = ctx.data(|d| d.get_temp(taken_id()));
        assert_eq!(taken.unwrap().as_slice(), &events[..1]);
        // Reading them consumes them: a second pass does not scroll again.
        let mut deltas = Vec::new();
        let _ = ctx.run_ui(RawInput::default(), |ui| {
            deltas.push(frame_wheel_delta(ui, 10.0, 100.0));
            deltas.push(frame_wheel_delta(ui, 10.0, 100.0));
        });
        assert_eq!(deltas, vec![vec2(0.0, -30.0), Vec2::ZERO]);
        // The mark lasts one frame.
        let mut again = RawInput {
            events: events.clone(),
            ..RawInput::default()
        };
        take_raw_wheel(&ctx, &mut again);
        assert_eq!(again.events, events);
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
