//! Painting a merged tab: rows with a coloured source badge, the line number
//! within its source, the highlighted text, a scrollbar and the find bar.

use std::sync::Arc;

use egui::epaint::{CornerRadius, StrokeKind};
use egui::{Align2, Color32, FontId, Id, Rect, Sense, Stroke, Ui, pos2, vec2};
use oxtail_highlight::{Style, StyledSpan};

use crate::colors::{Colors, mix32};
use crate::logview::{SCROLLBAR_W, ViewEnv, round_to_pixel};
use crate::mergeview::{MergedView, badge_letter, clamp_top};
use crate::text::{JobOptions, build_job, compose};
use crate::viewport::{
    ScrollSpace, TrackClick, classify_click, digits, lines_target, position_at, thumb_px,
};

/// Horizontal padding.
const PAD: f32 = 6.0;

/// Source badge colours (mid tones that read on light and dark themes).
pub const PALETTE: [(u8, u8, u8); 8] = [
    (0x3d, 0x8b, 0xe0),
    (0xd9, 0x82, 0x2b),
    (0x4c, 0xa8, 0x5f),
    (0xb0, 0x5c, 0xc8),
    (0xcf, 0x4f, 0x5e),
    (0x2f, 0xa5, 0xa0),
    (0xa8, 0x98, 0x2a),
    (0x7a, 0x80, 0xe0),
];

/// The badge colour of source `i`.
pub fn badge_color(i: usize) -> Color32 {
    let (r, g, b) = PALETTE[i % PALETTE.len()];
    Color32::from_rgb(r, g, b)
}

/// Draws the merged view into the available space of `ui`.
pub fn show(ui: &mut Ui, id: Id, mv: &mut MergedView, env: &ViewEnv<'_>) {
    let ctx = ui.ctx().clone();
    let colors = env.colors;
    let ppp = ctx.pixels_per_point();
    let font = FontId::monospace(env.font_size);
    let row_h = round_to_pixel(env.font_size * env.line_height, ppp);
    let char_w = ctx.fonts_mut(|f| f.glyph_width(&font, '0')).max(1.0);
    let full = ui.available_rect_before_wrap();
    ui.allocate_rect(full, Sense::hover());
    let painter = ui.painter_at(full);
    painter.rect_filled(full, CornerRadius::ZERO, colors.background);

    // ---- geometry
    let max_number = mv
        .last_rows
        .iter()
        .filter_map(|(_, l)| l.as_ref().map(|l| l.line.number + 1))
        .max()
        .unwrap_or(1);
    let number_cols = digits(max_number).max(4);
    let badge_w = char_w * 1.6 + 8.0;
    let gutter_w = PAD + badge_w + 4.0 + number_cols as f32 * char_w + PAD;
    let text_left = full.left() + gutter_w;
    let text_right = (full.right() - SCROLLBAR_W).max(text_left + 20.0);
    let text_w = text_right - text_left;
    let track = Rect::from_min_max(pos2(text_right, full.top()), full.max);
    mv.visible_rows = (full.height() / row_h).ceil().max(1.0) as usize;
    mv.row_h = row_h;

    // ---- input
    let resp = ui.interact(
        Rect::from_min_max(full.min, pos2(text_right, full.bottom())),
        id.with("rows"),
        Sense::click_and_drag(),
    );
    if ui.rect_contains_pointer(full) {
        let d = ui.input(|i| i.smooth_scroll_delta);
        if d.y != 0.0 {
            mv.scroll_px(-d.y);
        }
        if d.x != 0.0 {
            mv.h_scroll = (mv.h_scroll - d.x).max(0.0);
        }
    }
    let (top, count) = mv.update_window();
    // (merged index, entry) of the rows shown: filtered rows in the filter view.
    let entries = mv.entries_in(top..top + count);
    let max_h = (mv.max_text_w + PAD * 2.0 - text_w).max(0.0);
    mv.h_scroll = mv.h_scroll.clamp(0.0, max_h);

    // ---- rows
    let text_painter = painter.with_clip_rect(Rect::from_min_max(
        pos2(text_left, full.top()),
        pos2(text_right, full.bottom()),
    ));
    let gutter = Rect::from_min_max(full.min, pos2(text_left, full.bottom()));
    painter.rect_filled(gutter, CornerRadius::ZERO, colors.gutter_bg);
    let mut rows: Vec<(usize, Option<Arc<crate::mergeview::MergedLine>>)> =
        Vec::with_capacity(entries.len());
    let mut widest = mv.max_text_w;
    let selection = mv.selected_range();
    let current = mv.find.open.then_some(mv.find.current).flatten();
    for (k, (index, e)) in entries.iter().enumerate() {
        let index = *index;
        let y = full.top() + k as f32 * row_h;
        let row_rect = Rect::from_min_max(pos2(text_left, y), pos2(text_right, y + row_h));
        let line = mv.cache.get(&e.raw()).cloned();
        if selection.is_some_and(|(a, b)| a <= index && index <= b) {
            painter.rect_filled(
                Rect::from_min_max(pos2(full.left(), y), pos2(text_right, y + row_h)),
                CornerRadius::ZERO,
                colors.selection_bg,
            );
        } else if mv.is_match(index) {
            painter.rect_filled(
                row_rect,
                CornerRadius::ZERO,
                mix32(colors.background, colors.search_match_bg, 0.35),
            );
        }
        if current == Some(index) {
            painter.rect_stroke(
                row_rect.shrink(0.5),
                CornerRadius::ZERO,
                Stroke::new(1.0, colors.search_current_bg),
                StrokeKind::Inside,
            );
        }
        // Badge.
        let badge =
            Rect::from_min_size(pos2(full.left() + PAD, y + 1.0), vec2(badge_w, row_h - 2.0));
        painter.rect_filled(badge, CornerRadius::same(3), badge_color(e.source()));
        painter.text(
            badge.center(),
            Align2::CENTER_CENTER,
            badge_letter(e.source()),
            font.clone(),
            Color32::WHITE,
        );
        if let Some(src) = mv.sources.get(e.source()) {
            ui.interact(badge, id.with(("badge", k)), Sense::hover())
                .on_hover_text(match &src.path {
                    Some(p) => p.display().to_string(),
                    None => src.name.clone(),
                });
        }
        match &line {
            Some(l) => {
                painter.text(
                    pos2(gutter.right() - PAD, y),
                    Align2::RIGHT_TOP,
                    (l.line.number + 1).to_string(),
                    font.clone(),
                    colors.gutter_text,
                );
                let galley = row_galley(&ctx, mv, colors, &font, row_h, env.style_epoch, l);
                widest = widest.max(galley.size().x);
                text_painter.galley(pos2(text_left + PAD - mv.h_scroll, y), galley, colors.text);
            }
            None => {
                text_painter.text(
                    pos2(text_left + PAD, y),
                    Align2::LEFT_TOP,
                    "\u{2026}",
                    font.clone(),
                    colors.gutter_text,
                );
            }
        }
        rows.push((index, line));
    }
    mv.max_text_w = widest;
    mv.last_rows = rows;
    painter.vline(text_left, full.y_range(), Stroke::new(1.0, colors.border));

    // ---- empty state
    if entries.is_empty() {
        let msg = if mv.filter_view_active() {
            if mv.filter_progress().is_some_and(|(_, done)| done) {
                "No lines match the filter"
            } else {
                "Filtering\u{2026}"
            }
        } else if mv.loading() {
            "Merging\u{2026}"
        } else {
            "No lines yet (waiting for data)"
        };
        painter.text(
            full.center(),
            Align2::CENTER_CENTER,
            msg,
            FontId::proportional(env.font_size + 1.0),
            colors.gutter_text,
        );
    }

    // ---- clicks
    let row_at = |p: egui::Pos2| -> Option<usize> {
        let k = ((p.y - full.top()) / row_h).floor();
        (k >= 0.0)
            .then(|| entries.get(k as usize).map(|(i, _)| *i))
            .flatten()
    };
    if let Some(p) = resp.interact_pointer_pos()
        && (resp.clicked() || resp.drag_started() || resp.dragged())
        && let Some(i) = row_at(p)
    {
        let extend = ui.input(|i| i.modifiers.shift) || resp.dragged();
        mv.select(i, extend);
        resp.request_focus();
    }
    resp.context_menu(|ui| {
        let has = mv.selection.is_some();
        if ui.add_enabled(has, egui::Button::new("Copy")).clicked() {
            if let Some(t) = mv.selection_text() {
                ui.ctx().copy_text(t);
            }
            ui.close();
        }
        if ui.button("Select all visible").clicked() {
            if let (Some(a), Some(b)) = (
                mv.last_rows.first().map(|r| r.0),
                mv.last_rows.last().map(|r| r.0),
            ) {
                mv.selection = Some((a, b));
            }
            ui.close();
        }
    });

    // ---- horizontal position indicator via shift+wheel only; scrollbar:
    draw_vbar(ui, id, mv, track, colors, &painter);

    // ---- horizontal scrollbar
    if max_h > 0.0 {
        let hb = Rect::from_min_max(
            pos2(text_left, full.bottom() - 8.0),
            pos2(text_right, full.bottom()),
        );
        let frac = (text_w / (mv.max_text_w + PAD * 2.0)).clamp(0.05, 1.0);
        let len = hb.width() * frac;
        let start = (mv.h_scroll / max_h) * (hb.width() - len);
        painter.rect_filled(
            Rect::from_min_size(pos2(hb.left() + start, hb.top() + 2.0), vec2(len, 4.0)),
            CornerRadius::same(2),
            mix32(colors.border, colors.text, 0.3),
        );
        let r = ui.interact(hb, id.with("hbar"), Sense::click_and_drag());
        if (r.dragged() || r.clicked())
            && let Some(p) = r.interact_pointer_pos()
            && hb.width() > len
        {
            let f = ((p.x - hb.left() - len * 0.5) / (hb.width() - len)).clamp(0.0, 1.0);
            mv.h_scroll = f * max_h;
        }
    }
}

/// The laid-out text of a merged line (cached per `(source, line)`).
fn row_galley(
    ctx: &egui::Context,
    mv: &mut MergedView,
    colors: &Colors,
    font: &FontId,
    row_h: f32,
    style_epoch: u64,
    l: &crate::mergeview::MergedLine,
) -> Arc<egui::Galley> {
    let epoch = style_epoch
        .wrapping_mul(1_000_003)
        .wrapping_add(mv.hls.get(l.source).map_or(0, |h| h.epoch));
    let key = (l.source, l.line.number);
    if let Some((e, g)) = mv.galleys.get(&key)
        && *e == epoch
    {
        return Arc::clone(g);
    }
    let prepared = match mv.hls.get_mut(l.source) {
        Some(h) => h.prepare(&l.line),
        None => {
            return ctx
                .fonts_mut(|f| f.layout_no_wrap(l.line.text.clone(), font.clone(), colors.text));
        }
    };
    let text = prepared.text.as_str();
    let line_layer: Vec<StyledSpan> = prepared
        .hl
        .line_style
        .map(|s| {
            vec![StyledSpan {
                range: 0..text.len(),
                style: Style { bg: None, ..s },
            }]
        })
        .unwrap_or_default();
    let segments = compose(
        text.len(),
        &[&line_layer, &prepared.ansi, &prepared.hl.spans],
        &[],
        false,
    );
    let job = build_job(
        text,
        &segments,
        l.line.truncated,
        &JobOptions {
            colors,
            font: font.clone(),
            row_height: row_h,
            wrap_width: None,
            dimmed: false,
        },
    );
    let galley = ctx.fonts_mut(|f| f.layout_job(job));
    if mv.galleys.len() > 8000 {
        mv.galleys.clear();
    }
    mv.galleys.insert(key, (epoch, Arc::clone(&galley)));
    galley
}

fn draw_vbar(
    ui: &mut Ui,
    id: Id,
    mv: &mut MergedView,
    track: Rect,
    colors: &Colors,
    painter: &egui::Painter,
) {
    painter.rect_filled(track, CornerRadius::ZERO, colors.gutter_bg);
    let len = mv.row_count();
    let space = ScrollSpace::Lines {
        total: len as u64,
        top: mv.top as u64,
        visible: mv.visible_rows as u64,
    };
    let thumb = thumb_px(track.height(), space.thumb(), 24.0);
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
        if pos >= 0.999_999 {
            mv.jump_bottom();
        } else {
            mv.follow = false;
            mv.pending_px = 0.0;
            let idx = lines_target(pos, len as u64, mv.visible_rows as u64) as usize;
            mv.top = clamp_top(idx, len, mv.visible_rows);
        }
    } else if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        match classify_click(thumb, p.y - track.top()) {
            TrackClick::Before => mv.page(false),
            TrackClick::After => mv.page(true),
            TrackClick::OnThumb => {}
        }
    }
    let rect = Rect::from_min_max(
        pos2(track.left() + 2.0, track.top() + thumb.start),
        pos2(track.right() - 2.0, track.top() + thumb.start + thumb.len),
    );
    let color = if resp.hovered() || resp.dragged() {
        mix32(colors.border, colors.text, 0.45)
    } else {
        mix32(colors.border, colors.text, 0.25)
    };
    painter.rect_filled(rect, CornerRadius::same(3), color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_colours_cycle_and_differ() {
        assert_eq!(badge_color(0), badge_color(8));
        let all: std::collections::HashSet<[u8; 4]> =
            (0..8).map(|i| badge_color(i).to_array()).collect();
        assert_eq!(all.len(), 8);
    }
}
