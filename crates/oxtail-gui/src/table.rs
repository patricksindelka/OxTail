//! The column table: cell layout helpers, the galley cache and the header
//! (resize, reorder, hide/show, pin, auto-fit). The rows themselves are
//! painted by [`crate::logview`] next to the plain-text rows, from the same
//! virtualized row list.
//!
//! Only visible rows are parsed and laid out; cells are cached per
//! `(line offset, column)` and dropped when they were not used recently.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use egui::epaint::CornerRadius;
use egui::{Align2, FontId, Galley, Id, Rect, Sense, Stroke, Ui, pos2, vec2};
use oxtail_columns::{ColumnKind, Schema};
use oxtail_highlight::{Style, StyledSpan};

use crate::collayout::{ColumnLayout, Placement};
use crate::colors::{Colors, mix32};
use crate::text::{JobOptions, build_job, compose};

/// Horizontal padding inside a cell.
pub const CELL_PAD: f32 = 5.0;
/// Width of the grab zone at a header cell's right edge.
pub const RESIZE_GRAB: f32 = 6.0;
/// Extra height of the header over a text row.
pub const HEADER_EXTRA: f32 = 6.0;

/// Whether cells of this kind are right-aligned.
pub fn right_aligned(kind: ColumnKind) -> bool {
    matches!(
        kind,
        ColumnKind::Number | ColumnKind::Duration | ColumnKind::Bytes
    )
}

/// The style spans of a line, translated into the coordinates of a cell that
/// is the verbatim slice `cell` of the line.
pub fn spans_in_cell(spans: &[StyledSpan], cell: &Range<usize>) -> Vec<StyledSpan> {
    spans
        .iter()
        .filter_map(|s| {
            let a = s.range.start.max(cell.start);
            let b = s.range.end.min(cell.end);
            (a < b).then(|| StyledSpan {
                range: a - cell.start..b - cell.start,
                style: s.style,
            })
        })
        .collect()
}

/// The style of the first span that covers all of `cell` (used for cells
/// whose displayed text differs from the raw slice, e.g. formatted sizes).
pub fn covering_style(spans: &[StyledSpan], cell: &Range<usize>) -> Option<Style> {
    spans
        .iter()
        .find(|s| s.range.start <= cell.start && s.range.end >= cell.end)
        .map(|s| s.style)
}

/// Everything a cell's appearance depends on besides its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellKey {
    /// Combined epoch (theme, rules, structure, search).
    pub epoch: u64,
    /// The line holds the current search match.
    pub current: bool,
    /// Context line of a filtered view.
    pub dimmed: bool,
}

struct Entry {
    key: CellKey,
    galley: Arc<Galley>,
    used: u64,
}

/// Cache of laid-out cells keyed by `(line offset, column)`. Column
/// `usize::MAX` is the spanning text of a continuation line.
#[derive(Default)]
pub struct TableCache {
    map: HashMap<(u64, usize), Entry>,
    frame: u64,
}

/// The cache key of a continuation line's text.
pub const CONTINUATION: usize = usize::MAX;

impl TableCache {
    /// Forgets everything.
    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Starts a frame; drops cells not used recently when the cache is big.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        if self.map.len() > 30_000 {
            let keep = self.frame.saturating_sub(2);
            self.map.retain(|_, e| e.used >= keep);
        }
    }

    /// Number of cached cells.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The cached galley, if it is current.
    pub fn get(&mut self, offset: u64, col: usize, key: CellKey) -> Option<Arc<Galley>> {
        let frame = self.frame;
        let e = self.map.get_mut(&(offset, col))?;
        (e.key == key).then(|| {
            e.used = frame;
            Arc::clone(&e.galley)
        })
    }

    /// Stores a galley.
    pub fn put(&mut self, offset: u64, col: usize, key: CellKey, galley: Arc<Galley>) {
        self.map.insert(
            (offset, col),
            Entry {
                key,
                galley,
                used: self.frame,
            },
        );
    }
}

/// Lays out one piece of text with layered styles.
pub struct CellText<'a> {
    /// The displayed text.
    pub text: &'a str,
    /// Style layers, bottom to top, in the coordinates of `text`.
    pub layers: &'a [&'a [StyledSpan]],
    /// Search matches in the coordinates of `text`.
    pub search: &'a [Range<usize>],
    /// The current search hit.
    pub current: bool,
    /// Draw dimmed.
    pub dimmed: bool,
}

/// Builds the galley of a cell.
pub fn layout_cell(
    ctx: &egui::Context,
    colors: &Colors,
    font: &FontId,
    row_h: f32,
    cell: &CellText<'_>,
) -> Arc<Galley> {
    let segments = compose(cell.text.len(), cell.layers, cell.search, cell.current);
    let job = build_job(
        cell.text,
        &segments,
        false,
        &JobOptions {
            colors,
            font: font.clone(),
            row_height: row_h,
            wrap_width: None,
            dimmed: cell.dimmed,
        },
    );
    ctx.fonts_mut(|f| f.layout_job(job))
}

/// What the user did to the header.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HeaderEvent {
    /// Set the width (characters) of the column at this display position.
    Resize(usize, f32),
    /// Move the column at `from` to insertion index `to`.
    Reorder(usize, usize),
    /// Hide the column at this position.
    Hide(usize),
    /// Show the column at this position.
    Show(usize),
    /// Pin or unpin the column at this position.
    TogglePin(usize),
    /// Fit the column at this position to its content.
    Autofit(usize),
    /// Show all columns.
    ShowAll,
}

/// Geometry of the header for one frame.
pub struct HeaderGeometry<'a> {
    /// The header strip (over the gutter and the text area).
    pub rect: Rect,
    /// Left edge of the columns.
    pub text_left: f32,
    /// Right edge of the columns.
    pub text_right: f32,
    /// Horizontal scroll of the unpinned columns.
    pub h_scroll: f32,
    /// Width of the pinned block in pixels.
    pub pinned_w: f32,
    /// Pixels per character.
    pub char_w: f32,
    /// Column placements.
    pub placements: &'a [Placement],
}

/// Draws the header and returns what the user did. `id` must be unique per
/// tab and pane.
pub fn draw_header(
    ui: &mut Ui,
    id: Id,
    g: &HeaderGeometry<'_>,
    schema: &Schema,
    layout: &ColumnLayout,
    colors: &Colors,
    font: &FontId,
) -> Vec<HeaderEvent> {
    let mut events = Vec::new();
    let painter = ui.painter_at(g.rect);
    painter.rect_filled(g.rect, CornerRadius::ZERO, colors.gutter_bg);
    painter.hline(
        g.rect.x_range(),
        g.rect.bottom() - 0.5,
        Stroke::new(1.0, colors.border),
    );
    let scroll_clip = Rect::from_min_max(
        pos2((g.text_left + g.pinned_w).min(g.text_right), g.rect.top()),
        pos2(g.text_right, g.rect.bottom()),
    );
    let pinned_clip = Rect::from_min_max(
        pos2(g.text_left, g.rect.top()),
        pos2(
            (g.text_left + g.pinned_w).min(g.text_right),
            g.rect.bottom(),
        ),
    );
    let mut drag_target: Option<f32> = None;
    let strip = ui.interact(g.rect, id.with("strip"), Sense::click());

    // Pass 1: cells in display order; unpinned first so pinned ones paint over.
    let order: Vec<&Placement> = g
        .placements
        .iter()
        .filter(|p| !p.pinned)
        .chain(g.placements.iter().filter(|p| p.pinned))
        .collect();
    for p in order {
        let x0 = g.text_left + p.x - if p.pinned { 0.0 } else { g.h_scroll };
        let cell = Rect::from_min_max(pos2(x0, g.rect.top()), pos2(x0 + p.w, g.rect.bottom()));
        let clip = if p.pinned { pinned_clip } else { scroll_clip };
        let visible = cell.intersect(clip);
        if visible.width() <= 1.0 {
            continue;
        }
        let Some(pos) = layout.position(p.col) else {
            continue;
        };
        let info = &schema.columns[p.col];
        let cp = painter.with_clip_rect(clip);
        if p.pinned {
            cp.rect_filled(visible, CornerRadius::ZERO, colors.gutter_bg);
        }
        let resp = ui.interact(
            // Leave the resize grab zones (either side of the divider) free.
            visible.shrink2(vec2(RESIZE_GRAB * 0.5, 0.0)),
            id.with(("cell", p.col)),
            Sense::click_and_drag(),
        );
        if resp.hovered() || resp.dragged() {
            cp.rect_filled(
                visible,
                CornerRadius::ZERO,
                mix32(colors.gutter_bg, colors.text, 0.08),
            );
        }
        let label = if p.pinned {
            format!("\u{25cf} {}", info.name)
        } else {
            info.name.clone()
        };
        cp.text(
            pos2(x0 + CELL_PAD, cell.center().y),
            Align2::LEFT_CENTER,
            label,
            font.clone(),
            colors.text,
        );
        cp.vline(
            cell.right() - 0.5,
            g.rect.y_range(),
            Stroke::new(1.0, colors.border),
        );
        let resp = resp.on_hover_text(format!(
            "{} ({})\nDrag to reorder, right-click for more",
            info.name,
            kind_label(info.kind)
        ));
        if resp.dragged()
            && let Some(pp) = resp.interact_pointer_pos()
        {
            drag_target = Some(pp.x);
        }
        if resp.drag_stopped()
            && let Some(pp) = ui.input(|i| i.pointer.interact_pos())
            && g.rect.expand(40.0).contains(pp)
        {
            let x_rel = pp.x - g.text_left;
            let to = layout.drop_index(g.placements, g.h_scroll, x_rel);
            if to != pos && to != pos + 1 {
                events.push(HeaderEvent::Reorder(pos, to));
            }
        }
        resp.context_menu(|ui| {
            if ui.button("Hide column").clicked() {
                events.push(HeaderEvent::Hide(pos));
                ui.close();
            }
            let pin_label = if p.pinned {
                "Unpin column"
            } else {
                "Pin column"
            };
            if ui.button(pin_label).clicked() {
                events.push(HeaderEvent::TogglePin(pos));
                ui.close();
            }
            if ui.button("Auto-fit width").clicked() {
                events.push(HeaderEvent::Autofit(pos));
                ui.close();
            }
            ui.separator();
            columns_menu(ui, schema, layout, &mut events);
        });
        // Resize handle at the right edge.
        let grab = Rect::from_min_max(
            pos2(cell.right() - RESIZE_GRAB * 0.5, g.rect.top()),
            pos2(cell.right() + RESIZE_GRAB * 0.5, g.rect.bottom()),
        )
        .intersect(clip.expand2(vec2(RESIZE_GRAB, 0.0)));
        if grab.width() > 0.0 {
            let rr = ui.interact(grab, id.with(("resize", p.col)), Sense::click_and_drag());
            if rr.hovered() || rr.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                painter.vline(
                    cell.right(),
                    g.rect.y_range(),
                    Stroke::new(2.0, colors.accent),
                );
            }
            if rr.double_clicked() {
                events.push(HeaderEvent::Autofit(pos));
            } else if rr.dragged() {
                let w_px = p.w + rr.drag_delta().x;
                events.push(HeaderEvent::Resize(pos, w_px / g.char_w.max(1.0)));
            }
        }
    }
    // The empty strip right of the last column also has the menu (registered
    // before the cells, which are drawn over it).
    strip.context_menu(|ui| columns_menu(ui, schema, layout, &mut events));
    // Drop indicator while dragging a header cell.
    if let Some(x) = drag_target {
        let x_rel = x - g.text_left;
        let to = layout.drop_index(g.placements, g.h_scroll, x_rel);
        // The line sits at the left edge of the column that would follow.
        let seen: Vec<usize> = layout
            .cols
            .iter()
            .enumerate()
            .filter(|(_, c)| c.visible)
            .map(|(i, _)| i)
            .collect();
        let at = seen.iter().position(|&i| i >= to).unwrap_or(seen.len());
        let lx = match g.placements.get(at) {
            Some(p) => g.text_left + p.x - if p.pinned { 0.0 } else { g.h_scroll },
            None => g.placements.last().map_or(g.text_left, |p| {
                g.text_left + p.x + p.w - if p.pinned { 0.0 } else { g.h_scroll }
            }),
        };
        painter.vline(lx, g.rect.y_range(), Stroke::new(2.0, colors.accent));
    }
    events
}

fn kind_label(kind: ColumnKind) -> &'static str {
    match kind {
        ColumnKind::Text => "text",
        ColumnKind::Number => "number",
        ColumnKind::Timestamp => "timestamp",
        ColumnKind::Duration => "duration",
        ColumnKind::Bytes => "size",
        ColumnKind::Json => "JSON",
        ColumnKind::Level => "level",
    }
}

/// The "show / hide columns" checklist shared by the header menus.
pub fn columns_menu(
    ui: &mut Ui,
    schema: &Schema,
    layout: &ColumnLayout,
    events: &mut Vec<HeaderEvent>,
) {
    ui.label("Columns");
    for (pos, c) in layout.cols.iter().enumerate() {
        let mut on = c.visible;
        let name = schema.columns.get(c.col).map_or("?", |i| i.name.as_str());
        if ui.checkbox(&mut on, name).changed() {
            events.push(if on {
                HeaderEvent::Show(pos)
            } else {
                HeaderEvent::Hide(pos)
            });
        }
    }
    if ui.button("Show all").clicked() {
        events.push(HeaderEvent::ShowAll);
        ui.close();
    }
}

/// Applies header events to the layout. `content_chars(pos)` gives the widest
/// visible cell of the column at a display position, for auto-fit. Returns
/// whether anything changed.
pub fn apply_events(
    layout: &mut ColumnLayout,
    schema: &Schema,
    events: &[HeaderEvent],
    content_chars: &dyn Fn(usize) -> usize,
) -> bool {
    let mut changed = false;
    for ev in events {
        match *ev {
            HeaderEvent::Resize(pos, w) => layout.resize(pos, w),
            HeaderEvent::Reorder(from, to) => {
                layout.reorder(from, to);
            }
            HeaderEvent::Hide(pos) => layout.set_visible(pos, false),
            HeaderEvent::Show(pos) => layout.set_visible(pos, true),
            HeaderEvent::TogglePin(pos) => {
                let pinned = layout.cols.get(pos).is_some_and(|c| c.pinned);
                layout.set_pinned(pos, !pinned);
            }
            HeaderEvent::Autofit(pos) => {
                let header = layout
                    .cols
                    .get(pos)
                    .and_then(|c| schema.columns.get(c.col))
                    .map_or(0, |i| i.name.chars().count() + 2);
                layout.autofit(pos, header, content_chars(pos));
            }
            HeaderEvent::ShowAll => {
                for c in &mut layout.cols {
                    c.visible = true;
                }
            }
        }
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxtail_highlight::{ColorRef, SemanticColor};
    use std::collections::BTreeMap;

    fn red() -> Style {
        Style::fg(ColorRef::solid(SemanticColor::Error))
    }

    fn span(r: Range<usize>) -> StyledSpan {
        StyledSpan {
            range: r,
            style: red(),
        }
    }

    #[test]
    fn spans_are_translated_and_clipped_to_the_cell() {
        let spans = vec![span(0..3), span(8..14), span(20..30)];
        let cell = 5..12;
        let got = spans_in_cell(&spans, &cell);
        let r: Vec<_> = got.iter().map(|s| s.range.clone()).collect();
        assert_eq!(r, vec![3..7]);
        assert!(spans_in_cell(&spans, &(40..50)).is_empty());
    }

    #[test]
    fn a_covering_span_styles_a_whole_reformatted_cell() {
        let spans = vec![span(5..12)];
        assert_eq!(covering_style(&spans, &(5..12)), Some(red()));
        assert_eq!(covering_style(&spans, &(6..10)), Some(red()));
        assert_eq!(covering_style(&spans, &(4..12)), None);
    }

    #[test]
    fn numbers_align_right() {
        assert!(right_aligned(ColumnKind::Number));
        assert!(right_aligned(ColumnKind::Bytes));
        assert!(!right_aligned(ColumnKind::Text));
        assert!(!right_aligned(ColumnKind::Timestamp));
    }

    #[test]
    fn header_events_edit_the_layout() {
        let schema = Schema::from_names(&["a", "bb", "c"], &BTreeMap::new());
        let mut l = ColumnLayout::new(&schema, &[0, 1, 2]);
        let fit = |pos: usize| [30usize, 4, 2][pos];
        assert!(apply_events(
            &mut l,
            &schema,
            &[
                HeaderEvent::Resize(0, 33.0),
                HeaderEvent::Hide(2),
                HeaderEvent::TogglePin(1),
                HeaderEvent::Autofit(0),
            ],
            &fit
        ));
        // `bb` was pinned and moved to the front, so position 0 is now `bb`.
        assert_eq!(l.cols[0].col, 1);
        assert!(l.cols[0].pinned);
        assert_eq!(l.cols[0].width, 30.0 + crate::collayout::FIT_PADDING);
        assert!(!l.cols[2].visible);
        apply_events(&mut l, &schema, &[HeaderEvent::ShowAll], &fit);
        assert_eq!(l.visible_count(), 3);
        apply_events(&mut l, &schema, &[HeaderEvent::Reorder(2, 0)], &fit);
        assert_eq!(l.cols[0].col, 2);
        assert!(!apply_events(&mut l, &schema, &[], &fit));
    }
}
