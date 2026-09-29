//! Scrollbar mapping: between the view position and a thumb on a track.
//!
//! With an exact line count the thumb maps to the *line fraction*. While the
//! index is incomplete it maps to the *byte fraction* (like `less`): the
//! thumb position is the top line's byte offset relative to the file size,
//! and dragging asks the document for the line at that byte
//! (`LineRequest::ByteFraction`).

/// What the scrollbar measures.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScrollSpace {
    /// Rows are counted (exact line count, or the rows of a filter view).
    Lines {
        /// Total rows.
        total: u64,
        /// Row index of the top row.
        top: u64,
        /// Rows that fit in the viewport.
        visible: u64,
    },
    /// Position is measured in bytes (index still running).
    Bytes {
        /// Start offset of the top row.
        top_offset: u64,
        /// Bytes covered by the rows in the viewport.
        visible_bytes: u64,
        /// Length of the UTF-8 view.
        utf8_len: u64,
    },
}

/// Thumb position and size, both as fractions of the track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThumbFractions {
    /// Where the scroll position is, `0.0` (top) to `1.0` (bottom).
    pub position: f64,
    /// Share of the track the thumb covers, `0.0..=1.0`.
    pub size: f64,
}

impl ScrollSpace {
    /// The thumb for this position.
    pub fn thumb(&self) -> ThumbFractions {
        match *self {
            ScrollSpace::Lines {
                total,
                top,
                visible,
            } => {
                if total == 0 || visible >= total {
                    return ThumbFractions {
                        position: 0.0,
                        size: 1.0,
                    };
                }
                ThumbFractions {
                    position: (top as f64 / (total - visible) as f64).clamp(0.0, 1.0),
                    size: (visible as f64 / total as f64).clamp(0.0, 1.0),
                }
            }
            ScrollSpace::Bytes {
                top_offset,
                visible_bytes,
                utf8_len,
            } => {
                if utf8_len == 0 || visible_bytes >= utf8_len {
                    return ThumbFractions {
                        position: 0.0,
                        size: 1.0,
                    };
                }
                ThumbFractions {
                    position: (top_offset as f64 / (utf8_len - visible_bytes) as f64)
                        .clamp(0.0, 1.0),
                    size: (visible_bytes as f64 / utf8_len as f64).clamp(0.0, 1.0),
                }
            }
        }
    }
}

/// The row index that a thumb position selects (line-fraction mode).
pub fn lines_target(position: f64, total: u64, visible: u64) -> u64 {
    if total <= visible {
        return 0;
    }
    let p = if position.is_finite() {
        position.clamp(0.0, 1.0)
    } else {
        0.0
    };
    ((p * (total - visible) as f64).round() as u64).min(total - visible)
}

/// The byte fraction (of the whole file) that a thumb position selects, for a
/// `ByteFraction` request (byte-fraction mode).
pub fn bytes_target(position: f64, visible_bytes: u64, utf8_len: u64) -> f64 {
    if utf8_len == 0 {
        return 0.0;
    }
    let p = if position.is_finite() {
        position.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let span = utf8_len.saturating_sub(visible_bytes) as f64;
    (p * span / utf8_len as f64).clamp(0.0, 1.0)
}

/// Thumb placement on a track, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThumbPx {
    /// Distance from the start of the track to the thumb.
    pub start: f32,
    /// Thumb length.
    pub len: f32,
}

/// Places the thumb on a track of `track_len` pixels (never shorter than
/// `min_len`).
pub fn thumb_px(track_len: f32, t: ThumbFractions, min_len: f32) -> ThumbPx {
    let track_len = track_len.max(0.0);
    let len = ((t.size as f32) * track_len)
        .max(min_len)
        .min(track_len.max(0.0));
    let start = (t.position as f32).clamp(0.0, 1.0) * (track_len - len).max(0.0);
    ThumbPx { start, len }
}

/// The thumb position (`0.0..=1.0`) for a pointer at `pointer` pixels from the
/// start of the track when the thumb was grabbed `grab` pixels from its start.
pub fn position_at(track_len: f32, thumb_len: f32, pointer: f32, grab: f32) -> f64 {
    let room = track_len - thumb_len;
    if room <= 0.0 {
        return 0.0;
    }
    (((pointer - grab) / room) as f64).clamp(0.0, 1.0)
}

/// Where a click on the track landed relative to the thumb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackClick {
    /// Above the thumb.
    Before,
    /// On the thumb.
    OnThumb,
    /// Below the thumb.
    After,
}

/// Classifies a click at `pointer` pixels from the track start.
pub fn classify_click(thumb: ThumbPx, pointer: f32) -> TrackClick {
    if pointer < thumb.start {
        TrackClick::Before
    } else if pointer > thumb.start + thumb.len {
        TrackClick::After
    } else {
        TrackClick::OnThumb
    }
}

/// Splits fractional wheel movement into whole pixels, carrying the rest.
#[derive(Debug, Default, Clone, Copy)]
pub struct WheelAccumulator {
    carry: f32,
}

impl WheelAccumulator {
    /// Adds `delta` pixels and returns the whole number of pixels to apply.
    pub fn push(&mut self, delta: f32) -> f32 {
        if !delta.is_finite() {
            return 0.0;
        }
        self.carry += delta;
        let whole = self.carry.trunc();
        self.carry -= whole;
        whole
    }
}

/// Digits needed to print `n`.
pub fn digits(mut n: u64) -> usize {
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_thumb_maps_top_to_position() {
        let s = ScrollSpace::Lines {
            total: 1000,
            top: 0,
            visible: 100,
        };
        assert_eq!(s.thumb().position, 0.0);
        assert!((s.thumb().size - 0.1).abs() < 1e-9);
        let s = ScrollSpace::Lines {
            total: 1000,
            top: 900,
            visible: 100,
        };
        assert_eq!(s.thumb().position, 1.0);
        let s = ScrollSpace::Lines {
            total: 1000,
            top: 450,
            visible: 100,
        };
        assert!((s.thumb().position - 0.5).abs() < 1e-9);
    }

    #[test]
    fn everything_visible_fills_the_track() {
        let s = ScrollSpace::Lines {
            total: 10,
            top: 0,
            visible: 50,
        };
        assert_eq!(
            s.thumb(),
            ThumbFractions {
                position: 0.0,
                size: 1.0
            }
        );
        let e = ScrollSpace::Lines {
            total: 0,
            top: 0,
            visible: 5,
        };
        assert_eq!(e.thumb().size, 1.0);
    }

    #[test]
    fn line_target_is_the_inverse_of_the_thumb() {
        for top in [0u64, 1, 17, 450, 899, 900] {
            let s = ScrollSpace::Lines {
                total: 1000,
                top,
                visible: 100,
            };
            assert_eq!(lines_target(s.thumb().position, 1000, 100), top);
        }
        assert_eq!(lines_target(2.0, 1000, 100), 900);
        assert_eq!(lines_target(-1.0, 1000, 100), 0);
        assert_eq!(lines_target(f64::NAN, 1000, 100), 0);
        assert_eq!(lines_target(0.5, 50, 100), 0);
    }

    #[test]
    fn byte_thumb_reaches_the_bottom_at_the_tail() {
        let len = 10_000_000_000u64;
        let vis = 4_000;
        let s = ScrollSpace::Bytes {
            top_offset: len - vis,
            visible_bytes: vis,
            utf8_len: len,
        };
        assert_eq!(s.thumb().position, 1.0);
        let s = ScrollSpace::Bytes {
            top_offset: 0,
            visible_bytes: vis,
            utf8_len: len,
        };
        assert_eq!(s.thumb().position, 0.0);
        assert!(s.thumb().size < 1e-5);
    }

    #[test]
    fn byte_target_maps_to_a_file_fraction() {
        let len = 1_000_000u64;
        assert_eq!(bytes_target(0.0, 1000, len), 0.0);
        let end = bytes_target(1.0, 1000, len);
        assert!((end - 0.999).abs() < 1e-9);
        let mid = bytes_target(0.5, 0, len);
        assert!((mid - 0.5).abs() < 1e-9);
        assert_eq!(bytes_target(0.5, 0, 0), 0.0);
        // Position -> target -> position round trip.
        let vis = 20_000;
        let top = 250_000u64;
        let pos = ScrollSpace::Bytes {
            top_offset: top,
            visible_bytes: vis,
            utf8_len: len,
        }
        .thumb()
        .position;
        let f = bytes_target(pos, vis, len);
        assert!((f * len as f64 - top as f64).abs() < 1.0);
    }

    #[test]
    fn thumb_pixels_respect_the_minimum() {
        let t = thumb_px(
            500.0,
            ThumbFractions {
                position: 0.5,
                size: 0.0001,
            },
            24.0,
        );
        assert_eq!(t.len, 24.0);
        assert!((t.start - 238.0).abs() < 1e-3);
        let full = thumb_px(
            500.0,
            ThumbFractions {
                position: 0.0,
                size: 1.0,
            },
            24.0,
        );
        assert_eq!(full.len, 500.0);
        assert_eq!(full.start, 0.0);
        let tiny = thumb_px(
            10.0,
            ThumbFractions {
                position: 1.0,
                size: 0.5,
            },
            24.0,
        );
        assert!(tiny.len <= 10.0 && tiny.start >= 0.0);
    }

    #[test]
    fn dragging_keeps_the_grab_point() {
        // Track 500, thumb 50, grabbed 10px into the thumb.
        assert_eq!(position_at(500.0, 50.0, 10.0, 10.0), 0.0);
        assert!((position_at(500.0, 50.0, 235.0, 10.0) - 0.5).abs() < 1e-9);
        assert_eq!(position_at(500.0, 50.0, 900.0, 10.0), 1.0);
        assert_eq!(position_at(50.0, 50.0, 10.0, 0.0), 0.0);
    }

    #[test]
    fn track_clicks_are_classified() {
        let t = ThumbPx {
            start: 100.0,
            len: 40.0,
        };
        assert_eq!(classify_click(t, 50.0), TrackClick::Before);
        assert_eq!(classify_click(t, 120.0), TrackClick::OnThumb);
        assert_eq!(classify_click(t, 200.0), TrackClick::After);
    }

    #[test]
    fn wheel_accumulator_carries_fractions() {
        let mut w = WheelAccumulator::default();
        assert_eq!(w.push(0.4), 0.0);
        assert_eq!(w.push(0.4), 0.0);
        assert_eq!(w.push(0.4), 1.0);
        assert_eq!(w.push(-2.5), -2.0);
        assert_eq!(w.push(f32::NAN), 0.0);
    }

    #[test]
    fn digit_counts() {
        assert_eq!(digits(0), 1);
        assert_eq!(digits(9), 1);
        assert_eq!(digits(10), 2);
        assert_eq!(digits(999_999), 6);
        assert_eq!(digits(u64::MAX), 20);
    }
}
