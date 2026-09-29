//! Turning a line of text plus highlight spans and search matches into an
//! egui `LayoutJob`.
//!
//! Layering, bottom to top: base text style, ANSI colours, rule highlight
//! spans, search matches. [`compose`] is pure (and unit-tested); [`build_job`]
//! maps the result to egui text formats.

use std::borrow::Cow;
use std::ops::Range;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Stroke};
use oxtail_highlight::{Style, StyledSpan};

use crate::colors::{Colors, mix32};

/// Whether a stretch of text is (part of) a search match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MatchKind {
    /// Not a match.
    #[default]
    None,
    /// A search match.
    Match,
    /// A match on the current search result.
    Current,
}

/// A run of text with one composed appearance.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    /// Byte range in the text.
    pub range: Range<usize>,
    /// Composed rule/ANSI style.
    pub style: Style,
    /// Search match state.
    pub search: MatchKind,
}

/// The span of `layer` that covers byte `at`, if any. Spans are sorted and
/// non-overlapping.
fn span_at(layer: &[StyledSpan], at: usize) -> Option<&StyledSpan> {
    let i = layer.partition_point(|s| s.range.end <= at);
    layer.get(i).filter(|s| s.range.start <= at)
}

/// Splits `0..len` into runs of constant appearance. `layers` are style spans
/// from bottom to top; `search` are the (sorted, non-overlapping) match ranges.
/// Boundaries beyond `len` are clamped; empty ranges are ignored.
pub fn compose(
    len: usize,
    layers: &[&[StyledSpan]],
    search: &[Range<usize>],
    current: bool,
) -> Vec<Segment> {
    if len == 0 {
        return Vec::new();
    }
    let mut cuts: Vec<usize> = vec![0, len];
    for layer in layers {
        for s in *layer {
            cuts.push(s.range.start.min(len));
            cuts.push(s.range.end.min(len));
        }
    }
    for r in search {
        cuts.push(r.start.min(len));
        cuts.push(r.end.min(len));
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut out: Vec<Segment> = Vec::with_capacity(cuts.len());
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a >= b {
            continue;
        }
        let mut style = Style::default();
        for layer in layers {
            if let Some(s) = span_at(layer, a) {
                style = style.layer(&s.style);
            }
        }
        let is_match = search.iter().any(|r| r.start <= a && a < r.end);
        let kind = match (is_match, current) {
            (false, _) => MatchKind::None,
            (true, false) => MatchKind::Match,
            (true, true) => MatchKind::Current,
        };
        // Merge with the previous run when nothing changes.
        match out.last_mut() {
            Some(prev) if prev.style == style && prev.search == kind && prev.range.end == a => {
                prev.range.end = b;
            }
            _ => out.push(Segment {
                range: a..b,
                style,
                search: kind,
            }),
        }
    }
    out
}

/// Moves `i` down to a char boundary of `s`.
fn floor_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Moves `i` up to a char boundary of `s`.
fn ceil_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Makes `ranges` valid for `s`: clamped, on char boundaries, sorted and
/// non-overlapping.
pub fn clean_ranges(s: &str, ranges: impl IntoIterator<Item = Range<usize>>) -> Vec<Range<usize>> {
    let mut v: Vec<Range<usize>> = ranges
        .into_iter()
        .map(|r| floor_boundary(s, r.start)..ceil_boundary(s, r.end))
        .filter(|r| r.start < r.end)
        .collect();
    v.sort_by_key(|r| (r.start, r.end));
    let mut out: Vec<Range<usize>> = Vec::with_capacity(v.len());
    for r in v {
        match out.last_mut() {
            Some(last) if r.start < last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

/// Tab width used when drawing.
pub const TAB_WIDTH: usize = 4;

/// Replaces characters egui cannot draw sensibly: tabs become spaces and
/// other control characters a middle dot.
pub fn sanitize(piece: &str) -> Cow<'_, str> {
    if !piece.chars().any(|c| c.is_control()) {
        return Cow::Borrowed(piece);
    }
    let mut out = String::with_capacity(piece.len() + 8);
    for c in piece.chars() {
        match c {
            '\t' => out.push_str(&" ".repeat(TAB_WIDTH)),
            c if c.is_control() => out.push('\u{b7}'),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// Options for [`build_job`].
pub struct JobOptions<'a> {
    /// Theme colours.
    pub colors: &'a Colors,
    /// The monospace font.
    pub font: FontId,
    /// Exact row height.
    pub row_height: f32,
    /// Wrap at this width; `None` for a single row.
    pub wrap_width: Option<f32>,
    /// Draw dimmed (context lines of a filter view).
    pub dimmed: bool,
}

fn format_for(seg: &Segment, o: &JobOptions<'_>) -> TextFormat {
    let c = o.colors;
    let mut fg = c.style_fg(&seg.style);
    if seg.style.bold {
        let towards = if c.dark {
            Color32::WHITE
        } else {
            Color32::BLACK
        };
        fg = mix32(fg, towards, 0.25);
    }
    if o.dimmed {
        fg = mix32(fg, c.background, 0.5);
    }
    let mut bg = c.style_bg(&seg.style).unwrap_or(Color32::TRANSPARENT);
    match seg.search {
        MatchKind::None => {}
        MatchKind::Match => bg = c.search_match_bg,
        MatchKind::Current => bg = c.search_current_bg,
    }
    if seg.search != MatchKind::None {
        // Keep matches readable on their strong background.
        fg = if c.dark {
            Color32::WHITE
        } else {
            Color32::BLACK
        };
    }
    TextFormat {
        font_id: o.font.clone(),
        line_height: Some(o.row_height),
        color: fg,
        background: bg,
        expand_bg: 0.0,
        italics: seg.style.italic,
        underline: if seg.style.underline {
            Stroke::new(1.0, fg)
        } else {
            Stroke::NONE
        },
        ..TextFormat::default()
    }
}

/// Builds the layout job for `text`. `segments` must come from [`compose`]
/// over the same text; `truncated` appends a marker.
pub fn build_job(
    text: &str,
    segments: &[Segment],
    truncated: bool,
    o: &JobOptions<'_>,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = o.wrap_width.unwrap_or(f32::INFINITY);
    job.break_on_newline = false;
    job.round_output_to_gui = true;
    if segments.is_empty() {
        // An empty line still needs its row height.
        job.append(
            "",
            0.0,
            TextFormat {
                font_id: o.font.clone(),
                line_height: Some(o.row_height),
                color: o.colors.text,
                ..TextFormat::default()
            },
        );
    }
    for seg in segments {
        let piece = text.get(seg.range.clone()).unwrap_or("");
        if piece.is_empty() {
            continue;
        }
        job.append(&sanitize(piece), 0.0, format_for(seg, o));
    }
    if truncated {
        let seg = Segment {
            range: 0..0,
            style: Style::default().dim().italic(),
            search: MatchKind::None,
        };
        job.append(" \u{2026} [line truncated]", 0.0, format_for(&seg, o));
    }
    job
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)] // a slice holding one match range is what the API takes
mod tests {
    use super::*;
    use oxtail_highlight::{ColorRef, SemanticColor};

    fn span(range: Range<usize>, style: Style) -> StyledSpan {
        StyledSpan { range, style }
    }

    fn red() -> Style {
        Style::fg(ColorRef::solid(SemanticColor::Error))
    }

    #[test]
    fn plain_text_is_one_segment() {
        let segs = compose(10, &[], &[], false);
        assert_eq!(
            segs,
            vec![Segment {
                range: 0..10,
                style: Style::default(),
                search: MatchKind::None
            }]
        );
        assert!(compose(0, &[], &[], false).is_empty());
    }

    #[test]
    fn spans_split_the_text() {
        let layer = vec![span(2..5, red())];
        let segs = compose(10, &[&layer], &[], false);
        let ranges: Vec<_> = segs.iter().map(|s| s.range.clone()).collect();
        assert_eq!(ranges, vec![0..2, 2..5, 5..10]);
        assert_eq!(segs[1].style, red());
        assert!(segs[0].style.is_empty() && segs[2].style.is_empty());
    }

    #[test]
    fn upper_layers_win_and_lower_ones_show_through() {
        let ansi = vec![span(
            0..6,
            Style::fg(ColorRef::solid(SemanticColor::Info)).bold(),
        )];
        let rules = vec![span(2..4, red())];
        let segs = compose(8, &[&ansi, &rules], &[], false);
        // 2..4: red fg from the rule, bold from ANSI.
        let mid = segs.iter().find(|s| s.range == (2..4)).unwrap();
        assert_eq!(mid.style.fg, Some(ColorRef::solid(SemanticColor::Error)));
        assert!(mid.style.bold);
        let left = &segs[0];
        assert_eq!(left.style.fg, Some(ColorRef::solid(SemanticColor::Info)));
    }

    #[test]
    fn search_matches_are_marked_on_top() {
        let rules = vec![span(0..10, red())];
        let segs = compose(10, &[&rules], &[3..5, 7..8], false);
        let kinds: Vec<_> = segs.iter().map(|s| (s.range.clone(), s.search)).collect();
        assert_eq!(
            kinds,
            vec![
                (0..3, MatchKind::None),
                (3..5, MatchKind::Match),
                (5..7, MatchKind::None),
                (7..8, MatchKind::Match),
                (8..10, MatchKind::None)
            ]
        );
        let cur = compose(10, &[], &[3..5], true);
        assert_eq!(cur[1].search, MatchKind::Current);
    }

    #[test]
    fn out_of_range_boundaries_are_clamped() {
        let layer = vec![span(5..100, red())];
        let segs = compose(10, &[&layer], &[8..50], false);
        assert_eq!(segs.last().unwrap().range.end, 10);
        assert!(segs.iter().all(|s| s.range.start < s.range.end));
    }

    #[test]
    fn adjacent_equal_runs_are_merged() {
        let layer = vec![span(0..3, red()), span(3..6, red())];
        let segs = compose(6, &[&layer], &[], false);
        assert_eq!(segs.len(), 1);
    }

    #[test]
    fn clean_ranges_fixes_boundaries_and_overlaps() {
        let s = "aé b"; // é is two bytes: 1..3
        let r = clean_ranges(s, vec![2..4, 0..2, 10..12, 3..3]);
        // 0..2 floors/ceils to 0..3, 2..4 to 1..4 -> merged 0..4.
        assert_eq!(r, vec![0..4]);
    }

    #[test]
    fn sanitize_replaces_controls() {
        assert_eq!(sanitize("plain"), Cow::Borrowed("plain"));
        assert_eq!(sanitize("a\tb"), "a    b");
        assert_eq!(sanitize("a\u{1}b\r"), "a\u{b7}b\u{b7}");
    }

    #[test]
    fn job_covers_the_text() {
        let themes = oxtail_config::ThemeSet::builtin();
        let colors = Colors::from_theme(themes.get("dark").unwrap());
        let o = JobOptions {
            colors: &colors,
            font: FontId::monospace(13.0),
            row_height: 16.0,
            wrap_width: None,
            dimmed: false,
        };
        let text = "GET /x 200\tok";
        let layer = vec![span(0..3, red())];
        let segs = compose(text.len(), &[&layer], &[4..6], true);
        let job = build_job(text, &segs, true, &o);
        job.debug_sanity_check();
        assert!(job.text.starts_with("GET /x 200"));
        assert!(job.text.contains("[line truncated]"));
        assert!(!job.text.contains('\t'));
        // An empty line still gets a section for its height.
        let empty = build_job("", &[], false, &o);
        empty.debug_sanity_check();
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn spans(len: usize) -> impl Strategy<Value = Vec<StyledSpan>> {
            // Sorted, non-overlapping spans made from random cut points.
            proptest::collection::vec(0..=len, 0..8).prop_map(move |mut cuts| {
                cuts.sort_unstable();
                cuts.chunks(2)
                    .filter(|c| c.len() == 2 && c[0] < c[1])
                    .enumerate()
                    .map(|(i, c)| StyledSpan {
                        range: c[0]..c[1],
                        style: if i % 2 == 0 {
                            red()
                        } else {
                            Style::default().bold()
                        },
                    })
                    .collect()
            })
        }

        proptest! {
            /// The segments always tile `0..len` exactly, and each one has the
            /// naive layered style of its first byte.
            #[test]
            fn segments_partition_the_text(
                len in 0usize..80,
                a in spans(80),
                b in spans(80),
                m in spans(80),
                current in any::<bool>(),
            ) {
                let search: Vec<Range<usize>> = m.iter().map(|s| s.range.clone()).collect();
                let segs = compose(len, &[&a, &b], &search, current);
                let mut at = 0;
                for s in &segs {
                    prop_assert_eq!(s.range.start, at);
                    prop_assert!(s.range.start < s.range.end);
                    at = s.range.end;
                    // Naive: layer every span covering the first byte.
                    let mut want = Style::default();
                    for layer in [&a, &b] {
                        if let Some(sp) = layer.iter().find(|sp| sp.range.contains(&s.range.start)) {
                            want = want.layer(&sp.style);
                        }
                    }
                    prop_assert_eq!(s.style, want);
                    let is_match = search.iter().any(|r| r.contains(&s.range.start));
                    prop_assert_eq!(s.search != MatchKind::None, is_match);
                }
                prop_assert_eq!(at, len);
            }
        }
    }
}
