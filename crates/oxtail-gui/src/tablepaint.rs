//! Painting the rows of the column table (the header lives in
//! [`crate::table`]).

use std::sync::Arc;

use egui::{FontId, Rect, pos2};
use oxtail_columns::ColumnKind;
use oxtail_core::Line;
use oxtail_highlight::{Style, StyledSpan};
use oxtail_search::Matcher;

use crate::collayout::Placement;
use crate::colors::Colors;
use crate::docview::DocView;
use crate::highlight::Prepared;
use crate::table::{
    CELL_PAD, CONTINUATION, CellKey, CellText, TableCache, covering_style, layout_cell,
    right_aligned, spans_in_cell,
};
use crate::text::clean_ranges;

/// Everything needed to paint one table row.
pub struct TableRow<'a> {
    /// The egui context (for laying out text).
    pub ctx: &'a egui::Context,
    /// The view (structure and horizontal scroll are read from it).
    pub view: &'a DocView,
    /// Theme.
    pub colors: &'a Colors,
    /// Monospace font.
    pub font: &'a FontId,
    /// Row height.
    pub row_h: f32,
    /// Width of one character.
    pub char_w: f32,
    /// Top of the row.
    pub y: f32,
    /// The column area.
    pub text_rect: Rect,
    /// Column placements.
    pub placements: &'a [Placement],
    /// Width of the pinned block.
    pub pinned_w: f32,
    /// The prepared line (text, highlights, record).
    pub prepared: &'a Prepared,
    /// The line.
    pub line: &'a Line,
    /// What the cell appearance depends on.
    pub key: CellKey,
    /// Search matcher, painted into cells.
    pub matcher: Option<&'a Arc<Matcher>>,
}

/// Paints the cells of one record row, or the spanning text of a
/// continuation line. Cells are laid out lazily and cached.
pub fn paint_table_row(r: &TableRow<'_>, cache: &mut TableCache, painter: &egui::Painter) {
    let st = &r.view.st;
    let Some(parser) = &st.parser else { return };
    let text_left = r.text_rect.left();
    let h_scroll = r.view.h_scroll;
    let row_rect = Rect::from_min_max(
        pos2(r.text_rect.left(), r.y),
        pos2(r.text_rect.right(), r.y + r.row_h),
    );
    let prepared = r.prepared;
    let line_style: Option<Style> = prepared.hl.line_style.map(|s| Style { bg: None, ..s });
    let Some(rec) = &prepared.record else {
        // A continuation line spans all columns: indented and dimmed.
        let galley = cache
            .get(r.line.offset, CONTINUATION, r.key)
            .unwrap_or_else(|| {
                let text = prepared.text.as_str();
                let layer = whole(line_style, text.len());
                let g = layout_cell(
                    r.ctx,
                    r.colors,
                    r.font,
                    r.row_h,
                    &CellText {
                        text,
                        layers: &[&layer, &prepared.ansi, &prepared.hl.spans],
                        search: &[],
                        current: r.key.current,
                        dimmed: true,
                    },
                );
                cache.put(r.line.offset, CONTINUATION, r.key, Arc::clone(&g));
                g
            });
        painter.with_clip_rect(row_rect).galley(
            pos2(text_left + CELL_PAD + 2.0 * r.char_w - h_scroll, r.y),
            galley,
            r.colors.text,
        );
        return;
    };
    let split = (text_left + r.pinned_w).min(r.text_rect.right());
    let scroll_clip = Rect::from_min_max(pos2(split, row_rect.top()), row_rect.max);
    let pinned_clip = Rect::from_min_max(row_rect.min, pos2(split, row_rect.bottom()));
    for p in r.placements {
        let x0 = text_left + p.x - if p.pinned { 0.0 } else { h_scroll };
        let cell = Rect::from_min_max(pos2(x0, r.y), pos2(x0 + p.w, r.y + r.row_h));
        let clip = if p.pinned { pinned_clip } else { scroll_clip };
        let visible = cell.intersect(clip);
        if visible.width() <= 1.0 {
            continue;
        }
        let galley = match cache.get(r.line.offset, p.col, r.key) {
            Some(g) => g,
            None => {
                let display = st.cell_text(p.col, rec);
                if display.is_empty() {
                    continue;
                }
                let span = rec.span(p.col);
                let verbatim = span
                    .clone()
                    .filter(|s| prepared.text.get(s.clone()) == Some(display.as_str()));
                let (hl_sub, ansi_sub) = match &verbatim {
                    Some(s) => (
                        spans_in_cell(&prepared.hl.spans, s),
                        spans_in_cell(&prepared.ansi, s),
                    ),
                    None => (
                        span.as_ref()
                            .and_then(|s| covering_style(&prepared.hl.spans, s))
                            .map(|style| whole(Some(style), display.len()))
                            .unwrap_or_default(),
                        Vec::new(),
                    ),
                };
                let layer = whole(line_style, display.len());
                let search = match r.matcher {
                    Some(m) => clean_ranges(&display, m.find_iter(display.as_bytes())),
                    None => Vec::new(),
                };
                let g = layout_cell(
                    r.ctx,
                    r.colors,
                    r.font,
                    r.row_h,
                    &CellText {
                        text: &display,
                        layers: &[&layer, &ansi_sub, &hl_sub],
                        search: &search,
                        current: r.key.current,
                        dimmed: r.key.dimmed,
                    },
                );
                cache.put(r.line.offset, p.col, r.key, Arc::clone(&g));
                g
            }
        };
        let kind = parser
            .schema()
            .columns
            .get(p.col)
            .map_or(ColumnKind::Text, |c| c.kind);
        let gw = galley.size().x;
        let x = if right_aligned(kind) && gw + 2.0 * CELL_PAD <= p.w {
            x0 + p.w - CELL_PAD - gw
        } else {
            x0 + CELL_PAD
        };
        painter
            .with_clip_rect(visible)
            .galley(pos2(x, r.y), galley, r.colors.text);
    }
}

/// One span covering `0..len` with `style` (empty when there is no style).
fn whole(style: Option<Style>, len: usize) -> Vec<StyledSpan> {
    style
        .map(|style| {
            vec![StyledSpan {
                range: 0..len,
                style,
            }]
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxtail_highlight::{ColorRef, SemanticColor};

    #[test]
    fn a_whole_span_covers_the_text() {
        assert!(whole(None, 5).is_empty());
        let s = Style::fg(ColorRef::solid(SemanticColor::Warn));
        let v = whole(Some(s), 5);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].range, 0..5);
    }
}
