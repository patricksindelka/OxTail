//! OxTail patch (not in upstream eframe): a frame identical to the last one
//! painted is not tessellated, painted or presented again.
//!
//! egui runs a full frame for every mouse move (and two more after each input
//! event, to let layouts settle). For a log viewer most of those frames draw
//! exactly the same picture, and painting it again is what costs: with a
//! software rasterizer (Remote Desktop, virtual machines, WARP, llvmpipe) it
//! can take a whole CPU core while the user merely moves the mouse.
//!
//! Only redraws eframe asked for itself may be skipped. A redraw the
//! operating system asks for (an uncovered window without a compositor) is
//! always painted: the window contents may be gone. As a safety net for an
//! operating-system redraw merged into one of eframe's own (X11 does that),
//! `run.rs` paints once more [`FORCED_PAINT_AFTER`] after a skipped frame.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Shape, ViewportId, epaint::ClippedShape};

/// Skipped frames are not paced by presenting (vsync): at most one per this.
const MIN_SKIPPED_FRAME_GAP: Duration = Duration::from_millis(16);

/// After a skipped frame, a frame is painted anyway this much later.
pub(crate) const FORCED_PAINT_AFTER: Duration = Duration::from_secs(1);

thread_local! {
    static MUST_PAINT: Cell<bool> = const { Cell::new(true) };
    static SKIPPED: Cell<bool> = const { Cell::new(false) };
    static SURFACE_FAILED: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f` (a `run_ui_and_paint`) knowing whether its frame must be painted
/// even if nothing changed, and returns whether painting was skipped. Calls
/// not wrapped in this always paint.
pub(crate) fn with_must_paint<R>(must_paint: bool, f: impl FnOnce() -> R) -> (R, bool) {
    let old = MUST_PAINT.with(|m| m.replace(must_paint));
    SKIPPED.with(|s| s.set(false));
    let r = f();
    MUST_PAINT.with(|m| m.set(old));
    (r, SKIPPED.with(Cell::take))
}

/// Called by the wgpu painter's surface-status hook: acquiring the surface
/// failed, so the frame being painted never reaches the screen.
pub(crate) fn surface_failed() {
    SURFACE_FAILED.with(|f| f.set(true));
}

/// Whether [`surface_failed`] was called since the last call of this.
pub(crate) fn take_surface_failed() -> bool {
    SURFACE_FAILED.with(Cell::take)
}

struct Painted {
    shapes: Vec<ClippedShape>,
    pixels_per_point: f32,
    size_in_pixels: [u32; 2],
}

/// The last frame painted, per viewport.
#[derive(Default)]
pub(crate) struct LastPainted {
    frames: HashMap<ViewportId, Painted>,
    last_frame: Option<Instant>,
}

impl LastPainted {
    /// Whether painting this frame can be skipped because it equals the last
    /// one painted. Otherwise remembers it as the last one painted (call
    /// [`LastPainted::forget`] if painting then fails). `extra_work` (texture
    /// updates, screenshots, clipboard actions) forces painting.
    pub(crate) fn skip(
        &mut self,
        viewport_id: ViewportId,
        shapes: &[ClippedShape],
        pixels_per_point: f32,
        size_in_pixels: [u32; 2],
        extra_work: bool,
    ) -> bool {
        let must = MUST_PAINT.with(Cell::get) || extra_work;
        let skip = self.decide(must, viewport_id, shapes, pixels_per_point, size_in_pixels);
        if skip {
            // Stand in for the vsync wait of a presented frame, so frames
            // nobody sees cannot run faster than the display.
            if let Some(t) = self.last_frame {
                let since = t.elapsed();
                if since < MIN_SKIPPED_FRAME_GAP {
                    std::thread::sleep(MIN_SKIPPED_FRAME_GAP - since);
                }
            }
            SKIPPED.with(|s| s.set(true));
        }
        self.last_frame = Some(Instant::now());
        skip
    }

    /// The decision of [`LastPainted::skip`], without side effects beyond
    /// remembering a painted frame.
    fn decide(
        &mut self,
        must: bool,
        viewport_id: ViewportId,
        shapes: &[ClippedShape],
        pixels_per_point: f32,
        size_in_pixels: [u32; 2],
    ) -> bool {
        if !must
            && let Some(last) = self.frames.get(&viewport_id)
            && last.pixels_per_point == pixels_per_point
            && last.size_in_pixels == size_in_pixels
            && same_shapes(&last.shapes, shapes)
        {
            return true;
        }
        self.frames.insert(
            viewport_id,
            Painted {
                shapes: shapes.to_vec(),
                pixels_per_point,
                size_in_pixels,
            },
        );
        false
    }

    /// Forgets the last frame of a viewport: its next frame paints. For a
    /// viewport that is not visible, or whose paint failed.
    pub(crate) fn forget(&mut self, viewport_id: ViewportId) {
        self.frames.remove(&viewport_id);
    }

    /// Forgets viewports that are gone.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&ViewportId) -> bool) {
        self.frames.retain(|id, _| keep(id));
    }
}

/// Equality of two frames' shapes, cheap for text: the same cached galley
/// (`Arc`) counts as the same text. A galley laid out anew with equal content
/// compares unequal, which only costs a paint.
fn same_shapes(a: &[ClippedShape], b: &[ClippedShape]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(a, b)| a.clip_rect == b.clip_rect && same_shape(&a.shape, &b.shape))
}

fn same_shape(a: &Shape, b: &Shape) -> bool {
    match (a, b) {
        (Shape::Vec(a), Shape::Vec(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_shape(a, b))
        }
        (Shape::Text(a), Shape::Text(b)) => {
            Arc::ptr_eq(&a.galley, &b.galley)
                && a.pos == b.pos
                && a.underline == b.underline
                && a.fallback_color == b.fallback_color
                && a.override_text_color == b.override_text_color
                && a.opacity_factor == b.opacity_factor
                && a.angle == b.angle
        }
        (Shape::Mesh(a), Shape::Mesh(b)) => Arc::ptr_eq(a, b) || a == b,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Color32, FontId, Rect, pos2};

    fn rect(color: Color32) -> ClippedShape {
        ClippedShape {
            clip_rect: Rect::EVERYTHING,
            shape: Shape::rect_filled(
                Rect::from_min_max(pos2(0.0, 0.0), pos2(9.0, 9.0)),
                0.0,
                color,
            ),
        }
    }

    fn text(galley: Arc<egui::Galley>) -> ClippedShape {
        ClippedShape {
            clip_rect: Rect::EVERYTHING,
            shape: Shape::galley(pos2(1.0, 1.0), galley, Color32::WHITE),
        }
    }

    fn galley(ctx: &egui::Context, s: &str) -> Arc<egui::Galley> {
        ctx.fonts_mut(|f| f.layout_no_wrap(s.into(), FontId::monospace(12.0), Color32::WHITE))
    }

    const VP: ViewportId = ViewportId::ROOT;

    #[test]
    fn an_unchanged_frame_is_skipped_unless_it_must_paint() {
        let mut p = LastPainted::default();
        let frame = [rect(Color32::RED)];
        assert!(!p.decide(false, VP, &frame, 1.0, [100, 100]), "first frame");
        assert!(p.decide(false, VP, &frame, 1.0, [100, 100]), "same again");
        assert!(!p.decide(true, VP, &frame, 1.0, [100, 100]), "must paint");
        assert!(p.decide(false, VP, &frame, 1.0, [100, 100]));
    }

    #[test]
    fn any_difference_paints() {
        let mut p = LastPainted::default();
        let frame = [rect(Color32::RED)];
        p.decide(false, VP, &frame, 1.0, [100, 100]);
        assert!(!p.decide(false, VP, &[rect(Color32::BLUE)], 1.0, [100, 100]));
        assert!(!p.decide(false, VP, &[rect(Color32::BLUE)], 2.0, [100, 100]));
        assert!(!p.decide(false, VP, &[rect(Color32::BLUE)], 2.0, [100, 90]));
        assert!(!p.decide(false, VP, &[], 2.0, [100, 90]));
        assert!(p.decide(false, VP, &[], 2.0, [100, 90]));
    }

    #[test]
    fn text_counts_as_the_same_only_for_the_same_galley() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
        let g = galley(&ctx, "line 1");
        let mut p = LastPainted::default();
        p.decide(false, VP, &[text(g.clone())], 1.0, [100, 100]);
        assert!(p.decide(false, VP, &[text(g)], 1.0, [100, 100]));
        let other = galley(&ctx, "line 2");
        assert!(!p.decide(false, VP, &[text(other)], 1.0, [100, 100]));
    }

    #[test]
    fn forgetting_makes_the_next_frame_paint() {
        let mut p = LastPainted::default();
        let frame = [rect(Color32::RED)];
        p.decide(false, VP, &frame, 1.0, [100, 100]);
        p.forget(VP);
        assert!(!p.decide(false, VP, &frame, 1.0, [100, 100]));
        p.retain(|_| false);
        assert!(!p.decide(false, VP, &frame, 1.0, [100, 100]));
    }

    #[test]
    fn the_wrapper_reports_skipping_and_restores_must_paint() {
        let mut p = LastPainted::default();
        let frame = [rect(Color32::RED)];
        let (_, skipped) = with_must_paint(false, || p.skip(VP, &frame, 1.0, [1, 1], false));
        assert!(!skipped, "first frame paints");
        let (_, skipped) = with_must_paint(false, || p.skip(VP, &frame, 1.0, [1, 1], false));
        assert!(skipped);
        // Outside the wrapper every frame paints.
        assert!(!p.skip(VP, &frame, 1.0, [1, 1], false));
        let (_, skipped) = with_must_paint(false, || p.skip(VP, &frame, 1.0, [1, 1], true));
        assert!(!skipped, "extra work paints");
    }
}
