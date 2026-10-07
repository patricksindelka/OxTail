//! Paces the repaints that documents request while their tabs follow a
//! growing file.
//!
//! A log written in many small pieces makes its document report new data
//! dozens of times per second, and every report would redraw the whole
//! window. While the user watches the end of the file there is nothing to
//! gain from that: [`WakePacer`] lets a short burst of wake-ups through at
//! once (so a single new line still shows without delay) and defers the rest
//! to the end of a [`WINDOW`], which caps a busy log at a few redraws per
//! window, like BareTail's refresh. When the user is not following (scrolling,
//! jumping, searching), every wake-up goes through at once.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Length of the pacing window.
pub const WINDOW: Duration = Duration::from_millis(250);
/// Wake-ups let through at once per window. A new line needs two: the new
/// length, then the lines read for it.
pub const BURST: usize = 3;
/// The shortest delay asked of egui: "as soon as possible", one frame.
pub const ONE_FRAME: Duration = Duration::from_millis(1);
/// Added to a deferral: egui fires delayed repaints a predicted frame time
/// early, and the deferred frame must land after the window has passed.
const SLACK: Duration = Duration::from_millis(20);

/// The pacing decision, without egui or a clock (testable).
#[derive(Debug, Default)]
pub struct Pacing {
    recent: VecDeque<Instant>,
}

impl Pacing {
    /// A wake-up at `now`: `None` to repaint at once, or how long to wait.
    pub fn decide(&mut self, now: Instant) -> Option<Duration> {
        while self
            .recent
            .front()
            .is_some_and(|&t| now.saturating_duration_since(t) >= WINDOW)
        {
            self.recent.pop_front();
        }
        if self.recent.len() < BURST {
            self.recent.push_back(now);
            return None;
        }
        // The oldest immediate wake-up leaves the window first.
        let oldest = *self.recent.front()?;
        Some((oldest + WINDOW + SLACK).saturating_duration_since(now))
    }
}

/// Shared by the wakers of all documents of the app.
#[derive(Debug, Default)]
pub struct WakePacer {
    paced: AtomicBool,
    pacing: Mutex<Pacing>,
}

impl WakePacer {
    /// A new pacer (not pacing until [`WakePacer::set_paced`]).
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Pace wake-ups (every visible view follows its end) or not.
    /// Returns `true` when pacing just stopped: wake-ups deferred until then
    /// should now be served by a frame soon.
    pub fn set_paced(&self, paced: bool) -> bool {
        let was = self.paced.swap(paced, Ordering::Relaxed);
        was && !paced
    }

    /// Whether wake-ups are paced now.
    pub fn is_paced(&self) -> bool {
        self.paced.load(Ordering::Relaxed)
    }

    /// Requests a repaint of `ctx`, now or a little later.
    pub fn wake(&self, ctx: &egui::Context) {
        self.wake_after(ctx, Duration::ZERO);
    }

    /// Requests a repaint of `ctx` after `delay`, or later when paced.
    pub fn wake_after(&self, ctx: &egui::Context, delay: Duration) {
        let wait = if self.paced.load(Ordering::Relaxed) {
            // A poisoned lock only loses the pacing: repaint at once.
            self.pacing
                .lock()
                .ok()
                .and_then(|mut p| p.decide(Instant::now()))
        } else {
            None
        };
        // `request_repaint` always runs two frames; the data a document
        // reports needs one. A non-zero delay gets exactly one.
        ctx.request_repaint_after(wait.unwrap_or_default().max(delay).max(ONE_FRAME));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_goes_through_and_the_rest_waits_for_the_window() {
        let t0 = Instant::now();
        let mut p = Pacing::default();
        for _ in 0..BURST {
            assert_eq!(p.decide(t0), None);
        }
        let ms = Duration::from_millis;
        assert_eq!(p.decide(t0 + ms(10)), Some(WINDOW + SLACK - ms(10)));
        assert_eq!(p.decide(t0 + ms(100)), Some(WINDOW + SLACK - ms(100)));
        // Once the window has passed, wake-ups go through again.
        assert_eq!(p.decide(t0 + WINDOW), None);
    }

    #[test]
    fn a_lone_wake_up_is_never_delayed() {
        let t0 = Instant::now();
        let mut p = Pacing::default();
        for i in 0..20u32 {
            assert_eq!(p.decide(t0 + WINDOW * i), None, "wake-up {i}");
        }
    }

    #[test]
    fn a_steady_stream_is_capped_per_window() {
        let t0 = Instant::now();
        let mut p = Pacing::default();
        let mut at_once = 0;
        // 200 wake-ups per second for two seconds.
        for i in 0..400u32 {
            if p.decide(t0 + Duration::from_millis(5) * i).is_none() {
                at_once += 1;
            }
        }
        assert_eq!(at_once, BURST * 8);
    }
}
