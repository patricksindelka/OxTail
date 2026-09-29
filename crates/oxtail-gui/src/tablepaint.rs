//! Painting the rows of the column table (the header lives in
//! [`crate::table`]).

use std::cell::RefCell;
use std::sync::Arc;

use egui::{FontId, Galley, Rect, pos2};
use oxtail_columns::ColumnKind;
use oxtail_core::Line;
use oxtail_highlight::{Style, StyledSpan};
use oxtail_search::Matcher;

use crate::collayout::Placement;
use crate::colors::Colors;
use crate::detail::{tone_for, tone_style};
use crate::docview::DocView;
use crate::highlight::Prepared;
use crate::table::{
    CELL_PAD, CONTINUATION, CellKey, CellText, TableCache, covering_style, layout_cell,
    right_aligned, spans_in_cell,
};
use crate::text::clean_ranges;

/// How the table wraps long text (Alt+Z): the stretching ("fill") column's
/// cell wraps at its width and the row grows; a continuation line wraps at
/// the full width. Every other cell stays on one line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WrapSpec {
    /// The schema column that wraps.
    pub fill: usize,
    /// Whether that column holds plain text (other kinds are short and are
    /// formatted for display, so they are never wrapped).
    pub fill_is_text: bool,
    /// Text width of the fill cell in pixels (its column minus padding).
    pub cell_w: f32,
    /// Text width of a continuation line in pixels.
    pub cont_w: f32,
}

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
    /// Height of one text line.
    pub row_h: f32,
    /// Height of this row (taller than `row_h` when its text wraps).
    pub height: f32,
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
    /// What the cell appearance depends on (with the wrap width when the row
    /// wraps).
    pub key: CellKey,
    /// Search matcher, painted into cells.
    pub matcher: Option<&'a Arc<Matcher>>,
    /// The wrapping, when it is on.
    pub wrap: Option<WrapSpec>,
    /// Cache of wrapped galleys (shared with the row-height measurement).
    pub wrap_cache: &'a RefCell<TableCache>,
}

/// The laid-out (dimmed) text of a continuation line.
fn continuation_galley(
    ctx: &egui::Context,
    colors: &Colors,
    font: &FontId,
    row_h: f32,
    prepared: &Prepared,
    key: CellKey,
    wrap: Option<f32>,
) -> Arc<Galley> {
    let text = prepared.text.as_str();
    let line_style: Option<Style> = prepared.hl.line_style.map(|s| Style { bg: None, ..s });
    let layer = whole(line_style, text.len());
    layout_cell(
        ctx,
        colors,
        font,
        row_h,
        &CellText {
            text,
            layers: &[&layer, &prepared.ansi, &prepared.hl.spans],
            search: &[],
            current: key.current,
            dimmed: true,
            wrap,
        },
    )
}

/// The laid-out text of column `col` of a record: `display` is what the cell
/// shows (formatted by the structure), `rec` supplies its highlights.
#[allow(clippy::too_many_arguments)]
fn cell_galley(
    ctx: &egui::Context,
    colors: &Colors,
    font: &FontId,
    row_h: f32,
    prepared: &Prepared,
    rec: &oxtail_columns::Record<'_>,
    col: usize,
    kind: ColumnKind,
    display: &str,
    key: CellKey,
    matcher: Option<&Arc<Matcher>>,
    wrap: Option<f32>,
) -> Arc<Galley> {
    let line_style: Option<Style> = prepared.hl.line_style.map(|s| Style { bg: None, ..s });
    let span = rec.span(col);
    let verbatim = span
        .clone()
        .filter(|s| prepared.text.get(s.clone()) == Some(display));
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
    // Level and timestamp cells take their severity/muted colour from the
    // column kind, below the rules' own styles (rules cannot always reach a
    // cell: JSON values have no place in the raw line).
    let tone = whole(tone_style(tone_for(kind, display)), display.len());
    let search = match matcher {
        Some(m) => clean_ranges(display, m.find_iter(display.as_bytes())),
        None => Vec::new(),
    };
    layout_cell(
        ctx,
        colors,
        font,
        row_h,
        &CellText {
            text: display,
            layers: &[&layer, &tone, &ansi_sub, &hl_sub],
            search: &search,
            current: key.current,
            dimmed: key.dimmed,
            wrap,
        },
    )
}

/// The height of a table row that wraps: the taller of the wrapped fill cell
/// and one text line (a continuation line wraps as a whole). The galleys go
/// into `cache`, where painting finds them again.
#[allow(clippy::too_many_arguments)]
pub fn wrapped_row_height(
    ctx: &egui::Context,
    colors: &Colors,
    font: &FontId,
    row_h: f32,
    cache: &mut TableCache,
    line: &Line,
    prepared: &Prepared,
    spec: &WrapSpec,
    key: CellKey,
    matcher: Option<&Arc<Matcher>>,
) -> f32 {
    let g = match &prepared.record {
        None => match cache.get(line.offset, CONTINUATION, key) {
            Some(g) => g,
            None => {
                let g =
                    continuation_galley(ctx, colors, font, row_h, prepared, key, Some(spec.cont_w));
                cache.put(line.offset, CONTINUATION, key, Arc::clone(&g));
                g
            }
        },
        Some(rec) => {
            if !spec.fill_is_text {
                return row_h;
            }
            match cache.get(line.offset, spec.fill, key) {
                Some(g) => g,
                None => {
                    let Some(display) = rec.get(spec.fill).filter(|d| !d.is_empty()) else {
                        return row_h;
                    };
                    let g = cell_galley(
                        ctx,
                        colors,
                        font,
                        row_h,
                        prepared,
                        rec,
                        spec.fill,
                        ColumnKind::Text,
                        display,
                        key,
                        matcher,
                        Some(spec.cell_w),
                    );
                    cache.put(line.offset, spec.fill, key, Arc::clone(&g));
                    g
                }
            }
        }
    };
    g.size().y.max(row_h)
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
        pos2(r.text_rect.right(), r.y + r.height),
    );
    let prepared = r.prepared;
    let Some(rec) = &prepared.record else {
        // A continuation line spans all columns: indented and dimmed.
        let galley = match r.wrap {
            Some(spec) => {
                let mut c = r.wrap_cache.borrow_mut();
                c.get(r.line.offset, CONTINUATION, r.key)
                    .unwrap_or_else(|| {
                        let g = continuation_galley(
                            r.ctx,
                            r.colors,
                            r.font,
                            r.row_h,
                            prepared,
                            r.key,
                            Some(spec.cont_w),
                        );
                        c.put(r.line.offset, CONTINUATION, r.key, Arc::clone(&g));
                        g
                    })
            }
            None => cache
                .get(r.line.offset, CONTINUATION, r.key)
                .unwrap_or_else(|| {
                    let g = continuation_galley(
                        r.ctx, r.colors, r.font, r.row_h, prepared, r.key, None,
                    );
                    cache.put(r.line.offset, CONTINUATION, r.key, Arc::clone(&g));
                    g
                }),
        };
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
        let cell = Rect::from_min_max(pos2(x0, r.y), pos2(x0 + p.w, r.y + r.height));
        let clip = if p.pinned { pinned_clip } else { scroll_clip };
        let visible = cell.intersect(clip);
        if visible.width() <= 1.0 {
            continue;
        }
        let kind = parser
            .schema()
            .columns
            .get(p.col)
            .map_or(ColumnKind::Text, |c| c.kind);
        let wrapped = r.wrap.filter(|w| w.fill == p.col && w.fill_is_text);
        let display_of = || st.cell_text(p.col, rec);
        let galley = match wrapped {
            Some(spec) => {
                let mut c = r.wrap_cache.borrow_mut();
                match c.get(r.line.offset, p.col, r.key) {
                    Some(g) => g,
                    None => {
                        let display = display_of();
                        if display.is_empty() {
                            continue;
                        }
                        let g = cell_galley(
                            r.ctx,
                            r.colors,
                            r.font,
                            r.row_h,
                            prepared,
                            rec,
                            p.col,
                            kind,
                            &display,
                            r.key,
                            r.matcher,
                            Some(spec.cell_w),
                        );
                        c.put(r.line.offset, p.col, r.key, Arc::clone(&g));
                        g
                    }
                }
            }
            None => match cache.get(r.line.offset, p.col, r.key) {
                Some(g) => g,
                None => {
                    let display = display_of();
                    if display.is_empty() {
                        continue;
                    }
                    let g = cell_galley(
                        r.ctx, r.colors, r.font, r.row_h, prepared, rec, p.col, kind, &display,
                        r.key, r.matcher, None,
                    );
                    cache.put(r.line.offset, p.col, r.key, Arc::clone(&g));
                    g
                }
            },
        };
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

    /// The colour the first section of the laid-out galley has.
    fn galley_color(
        colors: &Colors,
        kind: ColumnKind,
        text: &str,
        rule_style: Option<Style>,
    ) -> egui::Color32 {
        use crate::highlight::HighlightState;
        use oxtail_columns::ParserSpec;
        // A rule's style can only reach a cell that has a place in the raw
        // line (logfmt); JSON values have none, which is what the kind is for.
        let (spec, line) = if rule_style.is_some() {
            (
                ParserSpec::Logfmt {
                    columns: vec!["level".into()],
                    kinds: Default::default(),
                },
                format!("level={text}"),
            )
        } else {
            (
                ParserSpec::JsonLines {
                    columns: vec!["level".into()],
                    kinds: Default::default(),
                },
                format!("{{\"level\":\"{text}\"}}"),
            )
        };
        let parser = spec.compile().unwrap();
        let mut hl = HighlightState::with_defaults();
        hl.set_rules(Vec::new(), None);
        hl.set_parser(Some(Arc::new(parser)));
        let l = Line {
            number: 0,
            number_exact: true,
            offset: 0,
            len: line.len() as u64 + 1,
            text: line,
            truncated: false,
        };
        let mut prepared = (*hl.prepare(&l)).clone();
        let rec = prepared.record.clone().expect("a record");
        if let Some(style) = rule_style {
            prepared.hl.spans = vec![StyledSpan {
                range: rec.span(0).expect("a cell in the raw line"),
                style,
            }];
        }
        let ctx = egui::Context::default();
        let mut out = egui::Color32::PLACEHOLDER;
        let key = CellKey {
            epoch: 0,
            current: false,
            dimmed: false,
            wrap_bits: 0,
        };
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let ctx = ui.ctx().clone();
            let g = cell_galley(
                &ctx,
                colors,
                &FontId::monospace(13.0),
                16.0,
                &prepared,
                &rec,
                0,
                kind,
                text,
                key,
                None,
                None,
            );
            out = g.job.sections[0].format.color;
        });
        out
    }

    #[test]
    fn level_cells_take_the_severity_colour_from_their_kind() {
        use oxtail_highlight::{ColorRef, SemanticColor};
        let themes = oxtail_config::ThemeSet::builtin();
        for theme in themes.themes() {
            let colors = Colors::from_theme(theme);
            // JSON values have no place in the raw line, so no rule reaches
            // them: the column kind colours them, exactly as a rule with the
            // same style would.
            for (word, sem, bold) in [
                ("ERROR", SemanticColor::Error, true),
                ("warn", SemanticColor::Warn, true),
                ("Info", SemanticColor::Info, false),
                ("debug", SemanticColor::Debug, false),
                ("trace", SemanticColor::Trace, false),
            ] {
                let style = Style::fg(ColorRef::solid(sem));
                let style = if bold { style.bold() } else { style };
                let expected = galley_color(&colors, ColumnKind::Text, word, Some(style));
                assert_ne!(expected, colors.text);
                assert_eq!(
                    galley_color(&colors, ColumnKind::Level, word, None),
                    expected,
                    "{} {word}",
                    colors.name
                );
            }
            // Other kinds and unknown words stay plain.
            assert_eq!(
                galley_color(&colors, ColumnKind::Text, "ERROR", None),
                colors.text
            );
            assert_eq!(
                galley_color(&colors, ColumnKind::Level, "chatty", None),
                colors.text
            );
            // A rule's own colour wins over the kind's.
            let mine = Style::fg(ColorRef::solid(SemanticColor::Accent2));
            assert_eq!(
                galley_color(&colors, ColumnKind::Level, "ERROR", Some(mine)),
                // (the kind's bold stays; the colour is the rule's)
                galley_color(&colors, ColumnKind::Text, "ERROR", Some(mine.bold()))
            );
            assert_ne!(
                galley_color(&colors, ColumnKind::Level, "ERROR", Some(mine)),
                galley_color(&colors, ColumnKind::Level, "ERROR", None)
            );
        }
    }

    #[test]
    fn a_whole_span_covers_the_text() {
        assert!(whole(None, 5).is_empty());
        let s = Style::fg(ColorRef::solid(SemanticColor::Warn));
        let v = whole(Some(s), 5);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].range, 0..5);
    }
}
