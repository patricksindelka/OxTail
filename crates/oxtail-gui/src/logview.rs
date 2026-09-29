//! Painting and mouse handling of the log view: virtualized rows, gutter,
//! selection, scrollbar, minimap and the "new lines" pill.
//!
//! Only the rows that are visible are laid out; their galleys are cached. The
//! view never uses `ScrollArea::show_rows` over the whole line count (a
//! 100M-row list overflows `f32` pixel positions): position is a line offset
//! plus a pixel offset, see [`crate::scroll`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use egui::epaint::{CornerRadius, StrokeKind};
use egui::{
    Align2, Color32, FontId, Galley, Id, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2,
};
use oxtail_core::Line;
use oxtail_highlight::{ColorRef, Style, StyledSpan};
use oxtail_search::{MatchSet, Matcher};
use oxtail_time::jiff::Timestamp;

use crate::colors::{Colors, mix32};
use crate::docview::DocView;
use crate::highlight::{HighlightState, Prepared};
use crate::minimap::{bin_matches, bin_of, bin_points, click_fraction, intensity};
use crate::scroll::{Row, Visible};
use crate::table::{CellKey, HEADER_EXTRA, HeaderGeometry, draw_header};
use crate::tablepaint::{TableRow, paint_table_row};
use crate::text::{JobOptions, build_job, clean_ranges, compose};
use crate::timeview::{REL_CHARS, RelMode, format_gap, gap_between, relative_label};
use crate::viewport::{TrackClick, classify_click, digits, position_at, thumb_px};

/// Width of the vertical scrollbar.
pub const SCROLLBAR_W: f32 = 12.0;
/// Width of the minimap strip.
pub const MINIMAP_W: f32 = 12.0;
/// Height of the horizontal scrollbar.
pub const HBAR_H: f32 = 10.0;
/// Horizontal padding inside the gutter and before the text.
const PAD: f32 = 6.0;
/// Space for the bookmark / rule marker at the left edge of the gutter.
const MARKER_W: f32 = 14.0;

/// Everything about appearance and settings the view needs.
pub struct ViewEnv<'a> {
    /// Theme colours.
    pub colors: &'a Colors,
    /// Font size in points.
    pub font_size: f32,
    /// Line height as a multiple of the font size.
    pub line_height: f32,
    /// Show the line number gutter.
    pub line_numbers: bool,
    /// Show the minimap strip.
    pub minimap: bool,
    /// Changes whenever theme or font changed (invalidates laid-out lines).
    pub style_epoch: u64,
    /// Show a separator between rows further apart than this many seconds
    /// (`0` disables).
    pub gap_secs: f64,
}

// ------------------------------------------------------------------ galleys

struct GalleyEntry {
    key: GalleyKey,
    prepared: Arc<Prepared>,
    galley: Arc<Galley>,
    used: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct GalleyKey {
    epoch: u64,
    wrap_bits: u32,
    current: bool,
    dimmed: bool,
}

/// Cache of laid-out lines keyed by line start offset.
#[derive(Default)]
pub struct GalleyCache {
    map: HashMap<u64, GalleyEntry>,
    frame: u64,
}

impl GalleyCache {
    /// Forgets everything.
    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Starts a new frame and drops entries not used recently when the cache
    /// is large.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        if self.map.len() > 6000 {
            let keep_after = self.frame.saturating_sub(2);
            self.map.retain(|_, e| e.used >= keep_after);
        }
    }

    /// Number of cached lines.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Inputs to laying out one line.
pub struct LayoutOpts<'a> {
    /// Theme.
    pub colors: &'a Colors,
    /// Monospace font.
    pub font: FontId,
    /// Row height.
    pub row_h: f32,
    /// Wrap width, if wrapping.
    pub wrap_w: Option<f32>,
    /// Combined epoch of everything that affects appearance.
    pub epoch: u64,
    /// The search matcher, for painting matches.
    pub matcher: Option<&'a Arc<Matcher>>,
    /// The line holding the current match.
    pub current: Option<u64>,
    /// Lines of a filter view (for dimming context lines).
    pub set: Option<&'a MatchSet>,
}

/// Prepares and lays out `line`, using the caches.
pub fn layout_line(
    ctx: &egui::Context,
    hl: &Rc<RefCell<HighlightState>>,
    galleys: &Rc<RefCell<GalleyCache>>,
    o: &LayoutOpts<'_>,
    line: &Line,
) -> (Arc<Prepared>, Arc<Galley>) {
    let dimmed = o
        .set
        .is_some_and(|s| s.is_context(s.rank(line.offset)) && s.contains(line.offset));
    let current = o.current == Some(line.offset);
    let key = GalleyKey {
        epoch: o.epoch,
        wrap_bits: o.wrap_w.map_or(0, f32::to_bits),
        current,
        dimmed,
    };
    let frame = galleys.borrow().frame;
    if let Some(e) = galleys.borrow_mut().map.get_mut(&line.offset)
        && e.key == key
    {
        e.used = frame;
        return (Arc::clone(&e.prepared), Arc::clone(&e.galley));
    }
    let prepared = hl.borrow_mut().prepare(line);
    let text = prepared.text.as_str();
    let line_layer = prepared
        .hl
        .line_style
        .map(|s| {
            vec![StyledSpan {
                range: 0..text.len(),
                style: Style { bg: None, ..s },
            }]
        })
        .unwrap_or_default();
    let ranges = match o.matcher {
        Some(m) => clean_ranges(text, m.find_iter(text.as_bytes())),
        None => Vec::new(),
    };
    let segments = compose(
        text.len(),
        &[&line_layer, &prepared.ansi, &prepared.hl.spans],
        &ranges,
        current,
    );
    let job = build_job(
        text,
        &segments,
        line.truncated,
        &JobOptions {
            colors: o.colors,
            font: o.font.clone(),
            row_height: o.row_h,
            wrap_width: o.wrap_w,
            dimmed,
        },
    );
    let galley = ctx.fonts_mut(|f| f.layout_job(job));
    galleys.borrow_mut().map.insert(
        line.offset,
        GalleyEntry {
            key,
            prepared: Arc::clone(&prepared),
            galley: Arc::clone(&galley),
            used: frame,
        },
    );
    (prepared, galley)
}

// ------------------------------------------------------------------ minimap

/// Binned minimap data, recomputed only when its inputs change.
#[derive(Default)]
pub struct MinimapCache {
    key: (usize, usize, usize, u64, usize, usize),
    search: Vec<u32>,
    filter: Vec<u32>,
    ticks: Vec<Option<(u32, ColorRef)>>,
    bookmarks: Vec<bool>,
}

impl MinimapCache {
    fn refresh(&mut self, view: &DocView, bins: usize) {
        let hl = view.hl.borrow();
        let utf8_len = view.snapshot.utf8_len;
        let key = (
            view.find.matches.len(),
            view.filter.set.len(),
            hl.ticks.len(),
            utf8_len,
            bins,
            view.bookmarks.len(),
        );
        if key == self.key && self.search.len() == bins {
            return;
        }
        self.key = key;
        self.search = bin_matches(&view.find.matches, utf8_len, bins);
        self.filter = if view.filter.has_job() {
            bin_matches(&view.filter.set, utf8_len, bins)
        } else {
            Vec::new()
        };
        self.ticks = bin_points(hl.ticks.iter().map(|(o, c)| (*o, *c)), utf8_len, bins);
        self.bookmarks = vec![false; bins];
        for b in view.bookmarks.values() {
            if let Some(o) = b.offset {
                self.bookmarks[bin_of(o, utf8_len, bins)] = true;
            }
        }
    }
}

// ------------------------------------------------------------------- helpers

/// The row containing vertical position `y` (relative to the top of the
/// viewport).
pub fn row_at(rows: &[Row], y: f32) -> Option<&Row> {
    rows.iter().find(|r| y >= r.y && y < r.y + r.height)
}

/// Rounds a length to whole physical pixels.
pub fn round_to_pixel(v: f32, pixels_per_point: f32) -> f32 {
    (v * pixels_per_point).round().max(1.0) / pixels_per_point
}

fn rect_between(a: Pos2, b: Pos2) -> Rect {
    Rect::from_min_max(a, b)
}

fn triangle_down(painter: &egui::Painter, center: Pos2, half: f32, color: Color32) {
    painter.add(Shape::convex_polygon(
        vec![
            pos2(center.x - half, center.y - half * 0.6),
            pos2(center.x + half, center.y - half * 0.6),
            pos2(center.x, center.y + half * 0.8),
        ],
        color,
        Stroke::NONE,
    ));
}

fn bookmark_icon(painter: &egui::Painter, center: Pos2, size: f32, color: Color32) {
    let h = size * 0.5;
    painter.add(Shape::convex_polygon(
        vec![
            pos2(center.x - h * 0.7, center.y - h),
            pos2(center.x + h * 0.7, center.y - h),
            pos2(center.x + h * 0.7, center.y + h),
            pos2(center.x, center.y + h * 0.35),
            pos2(center.x - h * 0.7, center.y + h),
        ],
        color,
        Stroke::NONE,
    ));
}

/// Draws the view for `view` into the available space of `ui`.
pub fn show(ui: &mut Ui, id: Id, view: &mut DocView, env: &ViewEnv<'_>) {
    let ctx = ui.ctx().clone();
    let colors = env.colors;
    let ppp = ctx.pixels_per_point();
    let font = FontId::monospace(env.font_size);
    let row_h = round_to_pixel(env.font_size * env.line_height, ppp);
    let char_w = ctx.fonts_mut(|f| f.glyph_width(&font, '0')).max(1.0);

    // In table mode the header sits above the body; `full` is the body.
    let table = view.table_active();
    let header_h = if table { row_h + HEADER_EXTRA } else { 0.0 };
    let outer = ui.available_rect_before_wrap();
    ui.allocate_rect(outer, Sense::hover());
    let painter = ui.painter_at(outer);
    painter.rect_filled(outer, CornerRadius::ZERO, colors.background);
    let full = Rect::from_min_max(pos2(outer.left(), outer.top() + header_h), outer.max);

    // ---- geometry
    let snapshot = Arc::clone(&view.snapshot);
    let mut max_number = snapshot.lines.estimated_total.max(1);
    if let Some(l) = view.last_rows.last() {
        max_number = max_number.max(l.number + 1);
    }
    let number_cols = digits(max_number) + usize::from(!snapshot.lines.exact);
    let rel_w = if view.rel_mode == RelMode::Off {
        0.0
    } else {
        (REL_CHARS as f32 + 1.0) * char_w
    };
    let gutter_w = MARKER_W
        + rel_w
        + if env.line_numbers {
            number_cols as f32 * char_w + PAD * 2.0
        } else {
            PAD
        };
    let right_w = SCROLLBAR_W + if env.minimap { MINIMAP_W } else { 0.0 };
    let text_left = full.left() + gutter_w;
    let text_right = (full.right() - right_w).max(text_left + 20.0);
    let text_w = text_right - text_left;
    if table {
        view.max_text_w = view.st.layout.total_width(char_w);
    }
    let need_hbar = (!view.wrap || table) && view.max_text_w + PAD * 2.0 > text_w;
    let bottom = full.bottom() - if need_hbar { HBAR_H } else { 0.0 };
    let text_rect = rect_between(pos2(text_left, full.top()), pos2(text_right, bottom));
    let gutter_rect = rect_between(pos2(full.left(), full.top()), pos2(text_left, bottom));
    let track_rect = rect_between(
        pos2(text_right, full.top()),
        pos2(text_right + SCROLLBAR_W, bottom),
    );
    let mm_rect = rect_between(
        pos2(text_right + SCROLLBAR_W, full.top()),
        pos2(full.right(), bottom),
    );
    let metrics = crate::docview::Metrics {
        view_h: text_rect.height(),
        row_h,
    };

    // ---- interaction with the text area (before layout: scrolling)
    let text_resp = ui.interact(
        rect_between(pos2(full.left(), full.top()), pos2(text_right, bottom)),
        id.with("text"),
        Sense::click_and_drag(),
    );
    // Over the view (also its scrollbars), and not under a popup or window.
    let hovered = ui.rect_contains_pointer(full);
    if hovered {
        let delta = ui.input(|i| i.smooth_scroll_delta);
        if delta.y != 0.0 {
            view.scroll_px(-delta.y);
        }
        // egui already turns Shift+wheel into a horizontal delta.
        if delta.x != 0.0 && !view.wrap {
            view.h_scroll -= delta.x;
        }
    }

    // ---- rows
    let wrap_w = (view.wrap && !table).then(|| (text_w - PAD * 2.0).max(char_w * 8.0));
    let hl = Rc::clone(&view.hl);
    let galleys = Rc::clone(&view.galleys);
    galleys.borrow_mut().begin_frame();
    // Matches are painted only while the find bar is open.
    let matcher = if view.find.open {
        view.find.matcher.clone()
    } else {
        None
    };
    let epoch = env
        .style_epoch
        .wrapping_mul(1_000_003)
        .wrapping_add(hl.borrow().epoch)
        .wrapping_mul(1_000_003)
        .wrapping_add(view.st.epoch)
        .wrapping_mul(1_000_003)
        .wrapping_add(view.find.epoch)
        .wrapping_mul(2)
        .wrapping_add(u64::from(view.find.open));
    let set = view.active_set();
    let current = view.find.open.then_some(view.find.current).flatten();
    let lopts = LayoutOpts {
        colors,
        font: font.clone(),
        row_h,
        wrap_w,
        epoch,
        matcher: matcher.as_ref(),
        current,
        set: set.as_deref(),
    };
    let height_of = |line: &Line| -> f32 {
        if wrap_w.is_some() {
            let (_, g) = layout_line(&ctx, &hl, &galleys, &lopts, line);
            g.size().y.max(row_h)
        } else {
            row_h
        }
    };
    view.repaint_after = None;
    let vis: Visible = view.update_rows(metrics, &height_of);
    if let Some(d) = view.repaint_after.take() {
        ctx.request_repaint_after(d);
    }

    // Horizontal scroll range.
    let mut widest = view.max_text_w;
    let mut laid: Vec<(Arc<Prepared>, Option<Arc<Galley>>)> = Vec::with_capacity(vis.rows.len());
    for r in &vis.rows {
        if table {
            laid.push((hl.borrow_mut().prepare(&r.line), None));
        } else {
            let (p, g) = layout_line(&ctx, &hl, &galleys, &lopts, &r.line);
            widest = widest.max(g.size().x);
            laid.push((p, Some(g)));
        }
    }
    if !table {
        view.max_text_w = widest;
    }
    let max_h = (view.max_text_w + PAD * 2.0 - text_w).max(0.0);
    view.h_scroll = if view.wrap && !table {
        0.0
    } else {
        view.h_scroll.clamp(0.0, max_h)
    };
    // Table geometry.
    let placements = if table {
        view.st.layout.placements(text_w, char_w)
    } else {
        Vec::new()
    };
    let pinned_w: f32 = placements.iter().filter(|p| p.pinned).map(|p| p.w).sum();
    let mut tcache = std::mem::take(&mut view.tcache);
    tcache.begin_frame();

    // ---- paint rows
    let text_painter = painter.with_clip_rect(text_rect);
    let gutter_painter = painter.with_clip_rect(gutter_rect);
    painter.rect_filled(gutter_rect, CornerRadius::ZERO, colors.gutter_bg);
    let mut prev_offset: Option<u64> = None;
    let time_on = view.rel_mode != RelMode::Off || env.gap_secs > 0.0;
    let mut prev_ts: Option<Timestamp> = if time_on {
        vis.rows.first().and_then(|r| view.time_before(&r.line))
    } else {
        None
    };
    let selected_ts = if view.rel_mode == RelMode::Selected {
        view.selected_time()
    } else {
        None
    };
    let selection = view.selection;
    let cursor = view.cursor;
    let mut bookmark_updates: Vec<(u64, u64)> = Vec::new();
    for (row, (prepared, galley)) in vis.rows.iter().zip(&laid) {
        let y = text_rect.top() + row.y;
        let row_rect = rect_between(
            pos2(text_rect.left(), y),
            pos2(text_rect.right(), y + row.height),
        );
        let gutter_row = rect_between(
            pos2(gutter_rect.left(), y),
            pos2(gutter_rect.right(), y + row.height),
        );
        // Marks: a separator between the rows around a marked position.
        if let Some(prev) = prev_offset
            && view
                .marks
                .range(prev + 1..=row.line.offset)
                .next()
                .is_some()
        {
            let ly = y.round();
            painter.hline(
                full.left()..=text_rect.right(),
                ly,
                Stroke::new(2.0, colors.accent),
            );
        }
        prev_offset = Some(row.line.offset);
        let ts = if time_on {
            view.row_time(&row.line)
        } else {
            None
        };
        let gap = gap_between(prev_ts, ts, env.gap_secs);
        let rel = relative_label(view.rel_mode, ts, prev_ts, selected_ts);
        if ts.is_some() {
            prev_ts = ts;
        }

        let selected = selection.is_some_and(|s| s.contains(row.line.offset));
        if selected {
            painter.rect_filled(row_rect, CornerRadius::ZERO, colors.selection_bg);
            painter.rect_filled(gutter_row, CornerRadius::ZERO, colors.selection_bg);
        } else if let Some(bg) = prepared
            .hl
            .line_style
            .as_ref()
            .and_then(|s| colors.style_bg(s))
        {
            painter.rect_filled(row_rect, CornerRadius::ZERO, bg);
        } else if cursor == Some(row.line.offset) {
            painter.rect_filled(row_rect, CornerRadius::ZERO, colors.current_line_bg);
        }
        if current == Some(row.line.offset) {
            painter.rect_stroke(
                row_rect.shrink(0.5),
                CornerRadius::ZERO,
                Stroke::new(1.0, colors.search_current_bg),
                StrokeKind::Inside,
            );
        }
        // Text.
        if let Some(galley) = galley {
            text_painter.galley(
                pos2(text_rect.left() + PAD - view.h_scroll, y),
                Arc::clone(galley),
                colors.text,
            );
        } else {
            let dimmed = set.as_deref().is_some_and(|s| {
                s.is_context(s.rank(row.line.offset)) && s.contains(row.line.offset)
            });
            let key = CellKey {
                epoch,
                current: current == Some(row.line.offset),
                dimmed,
            };
            paint_table_row(
                &TableRow {
                    ctx: &ctx,
                    view: &*view,
                    colors,
                    font: &font,
                    row_h,
                    char_w,
                    y,
                    text_rect,
                    placements: &placements,
                    pinned_w,
                    prepared,
                    line: &row.line,
                    key,
                    matcher: matcher.as_ref(),
                },
                &mut tcache,
                &painter,
            );
        }
        // Gutter: bookmark, rule marker, number.
        let marker_c = pos2(gutter_row.left() + MARKER_W * 0.5, y + row_h * 0.5);
        if row.line.number_exact && view.bookmarks.contains_key(&row.line.number) {
            bookmark_icon(&gutter_painter, marker_c, row_h * 0.7, colors.accent);
            bookmark_updates.push((row.line.number, row.line.offset));
        }
        if let Some(g) = prepared.hl.gutter {
            gutter_painter.circle_filled(
                pos2(gutter_row.left() + 3.0, y + row_h * 0.5),
                2.5,
                colors.resolve(&g),
            );
        }
        if env.line_numbers {
            let label = if row.line.number_exact {
                (row.line.number + 1).to_string()
            } else {
                format!("\u{2248}{}", row.line.number + 1)
            };
            gutter_painter.text(
                pos2(gutter_rect.right() - PAD - rel_w, y),
                Align2::RIGHT_TOP,
                label,
                font.clone(),
                colors.gutter_text,
            );
        }
        if let Some(rel) = rel {
            gutter_painter.text(
                pos2(gutter_rect.right() - PAD, y),
                Align2::RIGHT_TOP,
                rel,
                font.clone(),
                mix32(colors.gutter_text, colors.accent, 0.35),
            );
        }
        if let Some(g) = gap {
            // Like the mark separator, with the size of the gap.
            let warn = colors.resolve(&ColorRef::solid(oxtail_highlight::SemanticColor::Warn));
            let ly = y.round();
            painter.hline(full.left()..=text_rect.right(), ly, Stroke::new(1.5, warn));
            let label = format!("gap {}", format_gap(g));
            let galley = ctx.fonts_mut(|f| {
                f.layout_no_wrap(label, FontId::proportional(env.font_size * 0.85), warn)
            });
            let pill = Rect::from_min_size(
                pos2(text_rect.right() - galley.size().x - PAD * 2.0 - 2.0, ly),
                galley.size() + vec2(PAD * 2.0, 2.0),
            );
            painter.rect_filled(pill, CornerRadius::same(3), colors.background);
            painter.galley(pos2(pill.left() + PAD, pill.top() + 1.0), galley, warn);
        }
    }
    if let Some(prev) = prev_offset
        && vis.reaches_end
        && view.marks.range(prev + 1..).next().is_some()
        && let Some(last) = vis.rows.last()
    {
        let ly = (text_rect.top() + last.y + last.height).round();
        painter.hline(
            full.left()..=text_rect.right(),
            ly,
            Stroke::new(2.0, colors.accent),
        );
    }
    for (n, o) in bookmark_updates {
        view.learn_bookmark_offset(n, o);
    }
    view.tcache = tcache;
    if table {
        // Column separators.
        let sep = mix32(colors.background, colors.border, 0.6);
        let body = painter.with_clip_rect(text_rect);
        for p in &placements {
            let x = text_left + p.x + p.w - if p.pinned { 0.0 } else { view.h_scroll };
            let min_x = if p.pinned {
                text_left
            } else {
                text_left + pinned_w
            };
            if x >= min_x && x <= text_right {
                body.vline(x - 0.5, text_rect.y_range(), Stroke::new(1.0, sep));
            }
        }
        // Header.
        if let Some(parser) = view.st.parser.clone() {
            let geo = HeaderGeometry {
                rect: Rect::from_min_max(
                    pos2(outer.left(), outer.top()),
                    pos2(text_right, full.top()),
                ),
                text_left,
                text_right,
                h_scroll: view.h_scroll,
                pinned_w,
                char_w,
                placements: &placements,
            };
            let events = draw_header(
                ui,
                id.with("header"),
                &geo,
                parser.schema(),
                &view.st.layout,
                colors,
                &font,
            );
            if !events.is_empty() {
                let mut fits = HashMap::new();
                for ev in &events {
                    if let crate::table::HeaderEvent::Autofit(pos) = ev {
                        fits.insert(*pos, view.widest_cell(*pos));
                    }
                }
                let widest = |pos: usize| fits.get(&pos).copied().unwrap_or(0);
                if crate::table::apply_events(
                    &mut view.st.layout,
                    parser.schema(),
                    &events,
                    &widest,
                ) {
                    view.st.dirty = true;
                }
            }
            // The corner above the scrollbar and minimap.
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(text_right, outer.top()),
                    pos2(outer.right(), full.top()),
                ),
                CornerRadius::ZERO,
                colors.gutter_bg,
            );
        }
    }
    // Gutter / text separator.
    painter.vline(
        text_left,
        full.top()..=bottom,
        Stroke::new(1.0, colors.border),
    );

    // ---- empty states
    if vis.rows.is_empty() {
        let msg = empty_message(view);
        if let Some(msg) = msg {
            painter.text(
                text_rect.center(),
                Align2::CENTER_CENTER,
                msg,
                FontId::proportional(env.font_size + 1.0),
                colors.gutter_text,
            );
        }
    }

    // ---- mouse: selection, bookmarks, context menu
    handle_pointer(ui, view, &text_resp, &vis, text_rect, gutter_rect, row_h);

    // ---- vertical scrollbar
    draw_scrollbar(ui, id, view, &vis, track_rect, colors, &painter);

    // ---- minimap
    if env.minimap {
        draw_minimap(ui, id, view, &vis, mm_rect, colors, &painter);
    }

    // ---- horizontal scrollbar
    if need_hbar {
        let hb = rect_between(pos2(text_left, bottom), pos2(text_right, full.bottom()));
        painter.rect_filled(hb, CornerRadius::ZERO, colors.gutter_bg);
        let total = view.max_text_w + PAD * 2.0;
        let frac = (text_w / total).clamp(0.05, 1.0);
        let len = hb.width() * frac;
        let start = if max_h > 0.0 {
            (view.h_scroll / max_h) * (hb.width() - len)
        } else {
            0.0
        };
        let thumb = rect_between(
            pos2(hb.left() + start, hb.top() + 2.0),
            pos2(hb.left() + start + len, hb.bottom() - 2.0),
        );
        painter.rect_filled(
            thumb,
            CornerRadius::same(3),
            mix32(colors.border, colors.text, 0.2),
        );
        let resp = ui.interact(hb, id.with("hbar"), Sense::click_and_drag());
        if (resp.dragged() || resp.clicked())
            && let Some(p) = resp.interact_pointer_pos()
            && hb.width() > len
        {
            let f = ((p.x - hb.left() - len * 0.5) / (hb.width() - len)).clamp(0.0, 1.0);
            view.h_scroll = f * max_h;
        }
    }

    // ---- "new lines" pill
    let new_lines = view.new_lines_while_paused();
    if !view.follow && new_lines > 0 {
        let label = if new_lines == 1 {
            "1 new line".to_string()
        } else {
            format!("{new_lines} new lines")
        };
        let galley = ctx.fonts_mut(|f| {
            f.layout_no_wrap(
                label,
                FontId::proportional(env.font_size),
                colors.background,
            )
        });
        let size = galley.size() + vec2(34.0, 10.0);
        let rect = Rect::from_center_size(
            pos2(
                text_rect.center().x,
                text_rect.bottom() - size.y * 0.5 - 12.0,
            ),
            size,
        );
        let resp = ui.interact(rect, id.with("pill"), Sense::click());
        let fill = if resp.hovered() {
            mix32(colors.accent, Color32::WHITE, 0.2)
        } else {
            colors.accent
        };
        painter.rect_filled(rect, CornerRadius::same(255), fill);
        triangle_down(
            &painter,
            pos2(rect.left() + 15.0, rect.center().y),
            5.0,
            colors.background,
        );
        painter.galley(
            pos2(rect.left() + 26.0, rect.center().y - galley.size().y * 0.5),
            galley,
            colors.background,
        );
        if resp.clicked() {
            view.resume_follow();
        }
        resp.on_hover_text("Resume following (F)");
    }
}

fn empty_message(view: &DocView) -> Option<String> {
    if view.is_opening() {
        return Some("Opening\u{2026}".into());
    }
    if view.is_filtered() {
        return Some(if view.filter.status.done {
            "No lines match the filters".into()
        } else {
            "Searching\u{2026}".into()
        });
    }
    if view.snapshot.utf8_len == 0 {
        return Some(if view.snapshot.file_missing {
            "The file does not exist (waiting for it to appear)".into()
        } else {
            "The file is empty (waiting for data)".into()
        });
    }
    None
}

fn handle_pointer(
    ui: &mut Ui,
    view: &mut DocView,
    resp: &egui::Response,
    vis: &Visible,
    text_rect: Rect,
    gutter_rect: Rect,
    row_h: f32,
) {
    let (shift, press_origin, pos) = ui.input(|i| {
        (
            i.modifiers.shift,
            i.pointer.press_origin(),
            i.pointer.interact_pos(),
        )
    });
    let row_for = |p: Pos2| row_at(&vis.rows, p.y - text_rect.top()).map(|r| Arc::clone(&r.line));
    if resp.clicked_by(egui::PointerButton::Primary)
        && let Some(p) = pos
        && let Some(line) = row_for(p)
    {
        view.select(&line, shift);
        resp.request_focus();
    }
    if resp.double_clicked_by(egui::PointerButton::Primary)
        && let Some(p) = pos
        && gutter_rect.contains(p)
        && let Some(line) = row_for(p)
        && line.number_exact
    {
        view.bookmarks
            .entry(line.number)
            .or_insert_with(|| crate::docview::BookmarkInfo {
                label: String::new(),
                offset: Some(line.offset),
            });
        view.editing_bookmark = Some(line.number);
    }
    if resp.drag_started_by(egui::PointerButton::Primary)
        && let Some(o) = press_origin
        && let Some(line) = row_for(o)
    {
        view.select(&line, shift);
        resp.request_focus();
    }
    if resp.dragged_by(egui::PointerButton::Primary)
        && let Some(p) = pos
    {
        // Auto-scroll when dragging past the edges.
        if p.y < text_rect.top() {
            view.scroll_px(-(text_rect.top() - p.y).min(row_h * 4.0) * 0.5 - 2.0);
            ui.ctx().request_repaint();
        } else if p.y > text_rect.bottom() {
            view.scroll_px((p.y - text_rect.bottom()).min(row_h * 4.0) * 0.5 + 2.0);
            ui.ctx().request_repaint();
        }
        let clamped = Pos2::new(p.x, p.y.clamp(text_rect.top(), text_rect.bottom() - 1.0));
        if let Some(line) = row_for(clamped) {
            view.select(&line, true);
        }
    }
    if resp.secondary_clicked()
        && let Some(p) = pos
        && let Some(line) = row_for(p)
        && !view.selection.is_some_and(|s| s.contains(line.offset))
    {
        view.select(&line, false);
    }
    let selected = view.selection.is_some();
    resp.context_menu(|ui| {
        if ui
            .add_enabled(selected, egui::Button::new("Copy"))
            .clicked()
        {
            view.copy_selection(false);
            ui.close();
        }
        if ui
            .add_enabled(selected, egui::Button::new("Copy with line numbers"))
            .clicked()
        {
            view.copy_selection(true);
            ui.close();
        }
        if ui.button("Select all").clicked() {
            view.select_all();
            ui.close();
        }
        if let Some(parser) = view.st.parser.clone() {
            ui.separator();
            let rec = view.selected_record().and_then(|(_, p)| p.record.clone());
            if ui
                .add_enabled(rec.is_some(), egui::Button::new("Copy record as JSON"))
                .clicked()
                && let Some(r) = &rec
            {
                ui.ctx()
                    .copy_text(crate::detail::record_json(parser.schema(), r));
                ui.close();
            }
            if ui
                .add_enabled(rec.is_some(), egui::Button::new("Copy record as CSV"))
                .clicked()
                && let Some(r) = &rec
            {
                ui.ctx()
                    .copy_text(crate::detail::record_csv(parser.schema(), r));
                ui.close();
            }
            if ui
                .checkbox(&mut view.detail_open, "Detail pane")
                .on_hover_text("Show the selected record as key/value pairs")
                .clicked()
            {
                ui.close();
            }
        }
        ui.separator();
        if ui
            .add_enabled(selected, egui::Button::new("Toggle bookmark"))
            .clicked()
        {
            view.toggle_bookmark();
            ui.close();
        }
        if ui.button("Add mark after last line").clicked() {
            view.add_mark();
            ui.close();
        }
    });
}

fn draw_scrollbar(
    ui: &mut Ui,
    id: Id,
    view: &mut DocView,
    vis: &Visible,
    track: Rect,
    colors: &Colors,
    painter: &egui::Painter,
) {
    painter.rect_filled(track, CornerRadius::ZERO, colors.gutter_bg);
    let space = view.scroll_space(vis);
    let mut fractions = space.thumb();
    if let Some(d) = view.thumb_drag {
        fractions.position = d;
    }
    let thumb = thumb_px(track.height(), fractions, 24.0);
    let resp = ui.interact(track, id.with("vbar"), Sense::click_and_drag());
    let grab_id = id.with("vbar-grab");
    if resp.drag_started()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let y = p.y - track.top();
        let grab = if classify_click(thumb, y) == TrackClick::OnThumb {
            y - thumb.start
        } else {
            thumb.len * 0.5
        };
        ui.data_mut(|d| d.insert_temp(grab_id, grab));
    }
    if resp.dragged()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let grab = ui
            .data(|d| d.get_temp::<f32>(grab_id))
            .unwrap_or(thumb.len * 0.5);
        let pos = position_at(track.height(), thumb.len, p.y - track.top(), grab);
        view.scroll_to_thumb(pos, space);
        ui.ctx().request_repaint();
    } else if view.thumb_drag.is_some() && !resp.dragged() {
        view.thumb_drag = None;
    }
    if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        match classify_click(thumb, p.y - track.top()) {
            TrackClick::Before => view.page(false),
            TrackClick::After => view.page(true),
            TrackClick::OnThumb => {}
        }
    }
    let thumb_rect = rect_between(
        pos2(track.left() + 2.0, track.top() + thumb.start),
        pos2(track.right() - 2.0, track.top() + thumb.start + thumb.len),
    );
    let thumb_color = if resp.hovered() || resp.dragged() {
        mix32(colors.border, colors.text, 0.45)
    } else {
        mix32(colors.border, colors.text, 0.25)
    };
    painter.rect_filled(thumb_rect, CornerRadius::same(3), thumb_color);
}

fn draw_minimap(
    ui: &mut Ui,
    id: Id,
    view: &mut DocView,
    vis: &Visible,
    rect: Rect,
    colors: &Colors,
    painter: &egui::Painter,
) {
    painter.rect_filled(rect, CornerRadius::ZERO, colors.gutter_bg);
    let bins = rect.height().floor().max(1.0) as usize;
    let mut cache = std::mem::take(&mut view.minimap);
    cache.refresh(view, bins);
    let max_search = cache.search.iter().copied().max().unwrap_or(0);
    let max_filter = cache.filter.iter().copied().max().unwrap_or(0);
    let x0 = rect.left();
    let w = rect.width();
    for i in 0..bins {
        let y = rect.top() + i as f32;
        if let Some(Some((n, c))) = cache.ticks.get(i) {
            let _ = n;
            let col = colors.tick(c);
            painter.rect_filled(
                rect_between(pos2(x0 + 1.0, y), pos2(x0 + w * 0.5, y + 1.0)),
                CornerRadius::ZERO,
                col,
            );
        }
        if let Some(&n) = cache.filter.get(i)
            && n > 0
        {
            let a = intensity(n, max_filter);
            painter.rect_filled(
                rect_between(pos2(x0 + w * 0.5, y), pos2(x0 + w - 1.0, y + 1.0)),
                CornerRadius::ZERO,
                mix32(colors.gutter_bg, colors.accent, a),
            );
        }
        if let Some(&n) = cache.search.get(i)
            && n > 0
        {
            let a = intensity(n, max_search);
            painter.rect_filled(
                rect_between(pos2(x0 + 1.0, y), pos2(x0 + w - 1.0, y + 1.0)),
                CornerRadius::ZERO,
                mix32(colors.gutter_bg, colors.search_current_bg, a),
            );
        }
        if cache.bookmarks.get(i).copied().unwrap_or(false) {
            painter.rect_filled(
                rect_between(pos2(x0, y - 1.0), pos2(x0 + w, y + 1.0)),
                CornerRadius::ZERO,
                colors.accent,
            );
        }
    }
    view.minimap = cache;
    // The visible region.
    let utf8_len = view.snapshot.utf8_len;
    if utf8_len > 0
        && let (Some(first), Some(last)) = (vis.rows.first(), vis.rows.last())
    {
        let a = bin_of(first.line.offset, utf8_len, bins);
        let b = bin_of(last.line.offset + last.line.len, utf8_len, bins).max(a);
        let r = rect_between(
            pos2(rect.left(), rect.top() + a as f32),
            pos2(rect.right(), rect.top() + (b + 1) as f32),
        );
        painter.rect_stroke(
            r,
            CornerRadius::ZERO,
            Stroke::new(1.0, mix32(colors.border, colors.text, 0.5)),
            StrokeKind::Inside,
        );
    }
    let resp = ui.interact(rect, id.with("minimap"), Sense::click_and_drag());
    if (resp.clicked() || resp.dragged())
        && let Some(p) = resp.interact_pointer_pos()
    {
        view.jump_fraction(click_fraction(p.y, rect.top(), rect.height()));
    }
    resp.on_hover_text("Search matches and rule hits. Click to jump.");
}

/// A vector helper for tests and layout code.
pub fn size_of_galley(g: &Galley) -> Vec2 {
    g.size()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(y: f32, h: f32) -> Row {
        Row {
            line: Arc::new(Line {
                number: 0,
                number_exact: true,
                offset: y as u64,
                len: 1,
                text: String::new(),
                truncated: false,
            }),
            y,
            height: h,
        }
    }

    #[test]
    fn rows_are_hit_by_position() {
        let rows = vec![row(-4.0, 16.0), row(12.0, 16.0), row(28.0, 32.0)];
        assert_eq!(row_at(&rows, 0.0).unwrap().y, -4.0);
        assert_eq!(row_at(&rows, 12.0).unwrap().y, 12.0);
        assert_eq!(row_at(&rows, 59.9).unwrap().y, 28.0);
        assert!(row_at(&rows, 60.0).is_none());
        assert!(row_at(&rows, -5.0).is_none());
        assert!(row_at(&[], 1.0).is_none());
    }

    #[test]
    fn heights_snap_to_pixels() {
        assert_eq!(round_to_pixel(16.4, 1.0), 16.0);
        assert_eq!(round_to_pixel(16.4, 2.0), 16.5);
        assert!(round_to_pixel(0.0, 1.0) >= 1.0);
    }

    #[test]
    fn galley_cache_is_bounded() {
        let mut c = GalleyCache::default();
        assert!(c.is_empty());
        for _ in 0..3 {
            c.begin_frame();
        }
        assert_eq!(c.len(), 0);
    }
}
