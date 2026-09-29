//! Column layout maths for the table view: order, widths, visibility,
//! pinning, auto-fit and the resulting x positions. Pure data and functions,
//! independent of egui, so every rule is unit-tested.
//!
//! Widths are stored in *characters* (multiples of the monospace glyph
//! width), so they survive font-size changes and can be saved in a session.
//! The pixel maths takes the glyph width as a parameter.
//!
//! Invariants kept by every mutating method:
//!
//! * every schema column appears exactly once in [`ColumnLayout::cols`];
//! * pinned columns come before unpinned ones;
//! * widths stay within [`MIN_WIDTH`]..=[`MAX_WIDTH`].

use std::collections::BTreeMap;

use oxtail_columns::{ColumnKind, Schema};
use serde::{Deserialize, Serialize};

/// Narrowest a column can be, in characters.
pub const MIN_WIDTH: f32 = 3.0;
/// Widest a column can be, in characters.
pub const MAX_WIDTH: f32 = 400.0;
/// Extra characters added by auto-fit (cell padding).
pub const FIT_PADDING: f32 = 2.0;

/// One column of the table, in display order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColumnState {
    /// Index into the parser's schema.
    pub col: usize,
    /// Width in characters.
    pub width: f32,
    /// Whether the column is shown.
    pub visible: bool,
    /// Frozen at the left edge while scrolling horizontally.
    pub pinned: bool,
}

/// Where a visible column sits, in pixels relative to the table's left edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Index into the parser's schema.
    pub col: usize,
    /// Left edge (before horizontal scrolling).
    pub x: f32,
    /// Width.
    pub w: f32,
    /// Frozen columns do not move when scrolling horizontally.
    pub pinned: bool,
}

/// The default width of a column of `kind`, in characters.
pub fn default_width(name: &str, kind: ColumnKind) -> f32 {
    match kind {
        ColumnKind::Timestamp => 24.0,
        ColumnKind::Level => 7.0,
        ColumnKind::Number => 9.0,
        ColumnKind::Duration | ColumnKind::Bytes => 10.0,
        ColumnKind::Json => 40.0,
        ColumnKind::Text => {
            let n = name.to_ascii_lowercase();
            if matches!(n.as_str(), "msg" | "message" | "text" | "line") {
                60.0
            } else {
                18.0
            }
        }
    }
}

/// The layout of a table: display order, widths, visibility and pinning.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ColumnLayout {
    /// Columns in display order.
    pub cols: Vec<ColumnState>,
    /// The schema column that takes the remaining width (`None`: the last
    /// visible column does not stretch).
    pub message: Option<usize>,
}

impl ColumnLayout {
    /// A layout for `schema` in `order` (schema indices, see
    /// `colspec::apply_order`), with default widths. The column named
    /// `msg`/`message` (or the last column) is the stretching one.
    pub fn new(schema: &Schema, order: &[usize]) -> Self {
        let mut seen = vec![false; schema.len()];
        let mut cols = Vec::with_capacity(schema.len());
        for &i in order {
            if i < schema.len() && !seen[i] {
                seen[i] = true;
                cols.push(state_for(schema, i));
            }
        }
        for (i, s) in seen.iter().enumerate() {
            if !s {
                cols.push(state_for(schema, i));
            }
        }
        let message = schema
            .find("message")
            .or_else(|| schema.len().checked_sub(1));
        Self { cols, message }
    }

    /// The position of schema column `col` in display order.
    pub fn position(&self, col: usize) -> Option<usize> {
        self.cols.iter().position(|c| c.col == col)
    }

    /// Sets the width (characters) of the column at display position `pos`.
    pub fn resize(&mut self, pos: usize, width: f32) {
        if let Some(c) = self.cols.get_mut(pos) {
            c.width = clamp_width(width);
        }
    }

    /// Shows or hides the column at display position `pos`. The last visible
    /// column cannot be hidden.
    pub fn set_visible(&mut self, pos: usize, visible: bool) {
        if !visible && self.visible_count() <= 1 && self.cols.get(pos).is_some_and(|c| c.visible) {
            return;
        }
        if let Some(c) = self.cols.get_mut(pos) {
            c.visible = visible;
        }
    }

    /// Number of visible columns.
    pub fn visible_count(&self) -> usize {
        self.cols.iter().filter(|c| c.visible).count()
    }

    /// Moves the column at `from` so it ends up at insertion index `to` (an
    /// index into the current order, as a drop position). Dropping a column
    /// across the pinned boundary pins or unpins it. Returns its new
    /// position.
    pub fn reorder(&mut self, from: usize, to: usize) -> usize {
        if from >= self.cols.len() {
            return from;
        }
        let mut c = self.cols.remove(from);
        let target = if to > from { to - 1 } else { to }.min(self.cols.len());
        // Dropped inside the pinned block: pinned; beyond it: unpinned; right
        // at its edge the column keeps its state.
        let boundary = self.cols.iter().filter(|c| c.pinned).count();
        c.pinned = match target.cmp(&boundary) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => c.pinned,
        };
        self.cols.insert(target, c);
        target
    }

    /// Pins or unpins the column at `pos`: pinned columns move to the end of
    /// the pinned group, unpinned ones to its start. Returns the new position.
    pub fn set_pinned(&mut self, pos: usize, pinned: bool) -> usize {
        if pos >= self.cols.len() || self.cols[pos].pinned == pinned {
            return pos;
        }
        let mut c = self.cols.remove(pos);
        c.pinned = pinned;
        let boundary = self.cols.iter().filter(|c| c.pinned).count();
        self.cols.insert(boundary, c);
        boundary
    }

    /// Sets the width of the column at `pos` to fit `content_chars` (the
    /// widest cell in characters) and its header.
    pub fn autofit(&mut self, pos: usize, header_chars: usize, content_chars: usize) {
        let want = header_chars.max(content_chars) as f32 + FIT_PADDING;
        self.resize(pos, want);
    }

    /// Pixel placement of the visible columns for a table `available` pixels
    /// wide, with `char_w` pixels per character. When all columns are
    /// narrower than the table, the stretching column takes the remainder.
    pub fn placements(&self, available: f32, char_w: f32) -> Vec<Placement> {
        let char_w = char_w.max(1.0);
        let mut out: Vec<Placement> = Vec::with_capacity(self.cols.len());
        let mut x = 0.0;
        for c in self.cols.iter().filter(|c| c.visible) {
            let w = c.width * char_w;
            out.push(Placement {
                col: c.col,
                x,
                w,
                pinned: c.pinned,
            });
            x += w;
        }
        let spare = available - x;
        if spare > 0.0 {
            // The stretching column, else the last visible one.
            let idx = self
                .message
                .and_then(|m| out.iter().position(|p| p.col == m))
                .unwrap_or_else(|| out.len().saturating_sub(1));
            if let Some(p) = out.get_mut(idx) {
                p.w += spare;
                for later in out.iter_mut().skip(idx + 1) {
                    later.x += spare;
                }
            }
        }
        out
    }

    /// Total width in pixels of the visible columns (without stretching).
    pub fn total_width(&self, char_w: f32) -> f32 {
        self.cols
            .iter()
            .filter(|c| c.visible)
            .map(|c| c.width * char_w.max(1.0))
            .sum()
    }

    /// Width in pixels of the pinned block.
    pub fn pinned_width(&self, char_w: f32) -> f32 {
        self.cols
            .iter()
            .filter(|c| c.visible && c.pinned)
            .map(|c| c.width * char_w.max(1.0))
            .sum()
    }

    /// Where the drop position `to` (a display index for [`reorder`](Self::reorder))
    /// lies for a pointer at `x`: the number of visible columns whose centre
    /// is left of the pointer, mapped back to display indices.
    pub fn drop_index(&self, placements: &[Placement], scroll_x: f32, x: f32) -> usize {
        let mut count = 0;
        for p in placements {
            let px = if p.pinned { p.x } else { p.x - scroll_x };
            if px + p.w * 0.5 < x {
                count += 1;
            }
        }
        // Map "count visible columns before the pointer" to a display index.
        let mut seen = 0;
        for (i, c) in self.cols.iter().enumerate() {
            if c.visible {
                if seen == count {
                    return i;
                }
                seen += 1;
            }
        }
        self.cols.len()
    }

    /// The saveable form.
    pub fn snapshot(&self, schema: &Schema) -> LayoutSnapshot {
        let name = |c: &ColumnState| schema.columns.get(c.col).map(|i| i.name.clone());
        LayoutSnapshot {
            order: self.cols.iter().filter_map(name).collect(),
            hidden: self
                .cols
                .iter()
                .filter(|c| !c.visible)
                .filter_map(name)
                .collect(),
            pinned: self
                .cols
                .iter()
                .filter(|c| c.pinned)
                .filter_map(name)
                .collect(),
            widths: self
                .cols
                .iter()
                .filter_map(|c| name(c).map(|n| (n, c.width)))
                .collect(),
        }
    }

    /// Applies a saved layout to a freshly built one: names that no longer
    /// exist are ignored, new columns keep their place at the end.
    pub fn apply_snapshot(&mut self, schema: &Schema, snap: &LayoutSnapshot) {
        let index_of = |n: &String| schema.columns.iter().position(|c| &c.name == n);
        let mut order: Vec<usize> = snap.order.iter().filter_map(index_of).collect();
        order.dedup();
        let mut seen: Vec<usize> = Vec::new();
        let mut cols: Vec<ColumnState> = Vec::with_capacity(self.cols.len());
        for i in order {
            if seen.contains(&i) {
                continue;
            }
            if let Some(mut c) = self.cols.iter().copied().find(|c| c.col == i) {
                seen.push(i);
                if let Some(w) = snap.widths.get(&schema.columns[i].name) {
                    c.width = clamp_width(*w);
                }
                c.visible = !snap.hidden.contains(&schema.columns[i].name);
                c.pinned = snap.pinned.contains(&schema.columns[i].name);
                cols.push(c);
            }
        }
        for c in &self.cols {
            if !seen.contains(&c.col) {
                cols.push(*c);
            }
        }
        // Pinned first (stable), and never hide everything.
        let (mut pinned, rest): (Vec<_>, Vec<_>) = cols.into_iter().partition(|c| c.pinned);
        pinned.extend(rest);
        self.cols = pinned;
        if self.visible_count() == 0
            && let Some(c) = self.cols.first_mut()
        {
            c.visible = true;
        }
    }
}

fn state_for(schema: &Schema, i: usize) -> ColumnState {
    let info = &schema.columns[i];
    ColumnState {
        col: i,
        width: clamp_width(default_width(&info.name, info.kind)),
        visible: true,
        pinned: false,
    }
}

/// Clamps a width to the allowed range (NaN becomes the minimum).
pub fn clamp_width(w: f32) -> f32 {
    if w.is_nan() {
        MIN_WIDTH
    } else {
        w.clamp(MIN_WIDTH, MAX_WIDTH)
    }
}

/// A column layout by column *name*, for the GUI state file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutSnapshot {
    /// Display order.
    pub order: Vec<String>,
    /// Hidden columns.
    pub hidden: Vec<String>,
    /// Pinned columns.
    pub pinned: Vec<String>,
    /// Widths in characters.
    pub widths: BTreeMap<String, f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(names: &[&str]) -> Schema {
        Schema::from_names(names, &BTreeMap::new())
    }

    fn layout(names: &[&str]) -> (Schema, ColumnLayout) {
        let s = schema(names);
        let order: Vec<usize> = (0..names.len()).collect();
        let l = ColumnLayout::new(&s, &order);
        (s, l)
    }

    fn order_of(l: &ColumnLayout) -> Vec<usize> {
        l.cols.iter().map(|c| c.col).collect()
    }

    #[test]
    fn a_new_layout_has_every_column_once_and_a_message_column() {
        let s = schema(&["ts", "level", "message", "extra"]);
        let l = ColumnLayout::new(&s, &[2, 2, 0, 99]);
        assert_eq!(order_of(&l), vec![2, 0, 1, 3]);
        assert_eq!(l.message, Some(2));
        // Without a message column the last column stretches.
        let s = schema(&["a", "b"]);
        assert_eq!(ColumnLayout::new(&s, &[]).message, Some(1));
    }

    #[test]
    fn resize_is_clamped() {
        let (_, mut l) = layout(&["a", "b"]);
        l.resize(0, 1.0);
        assert_eq!(l.cols[0].width, MIN_WIDTH);
        l.resize(0, 1e9);
        assert_eq!(l.cols[0].width, MAX_WIDTH);
        l.resize(0, f32::NAN);
        assert_eq!(l.cols[0].width, MIN_WIDTH);
        l.resize(9, 10.0); // out of range: ignored
    }

    #[test]
    fn the_last_visible_column_cannot_be_hidden() {
        let (_, mut l) = layout(&["a", "b"]);
        l.set_visible(0, false);
        assert_eq!(l.visible_count(), 1);
        l.set_visible(1, false);
        assert_eq!(l.visible_count(), 1);
        l.set_visible(0, true);
        assert_eq!(l.visible_count(), 2);
    }

    #[test]
    fn reorder_moves_columns() {
        let (_, mut l) = layout(&["a", "b", "c", "d"]);
        assert_eq!(l.reorder(0, 3), 2);
        assert_eq!(order_of(&l), vec![1, 2, 0, 3]);
        assert_eq!(l.reorder(3, 0), 0);
        assert_eq!(order_of(&l), vec![3, 1, 2, 0]);
        assert_eq!(l.reorder(1, 1), 1);
        assert_eq!(l.reorder(9, 0), 9);
    }

    #[test]
    fn pinned_columns_stay_in_front() {
        let (_, mut l) = layout(&["a", "b", "c", "d"]);
        assert_eq!(l.set_pinned(2, true), 0);
        assert_eq!(order_of(&l), vec![2, 0, 1, 3]);
        assert_eq!(l.set_pinned(3, true), 1);
        assert_eq!(order_of(&l), vec![2, 3, 0, 1]);
        // Dragging an unpinned column into the pinned block pins it.
        let p = l.reorder(3, 0);
        assert_eq!(p, 0);
        assert!(l.cols[0].pinned);
        // Dragging a pinned column out of the block unpins it.
        let p = l.reorder(0, 4);
        assert!(!l.cols[p].pinned);
        // Unpinning moves it right behind the pinned block.
        l.set_pinned(0, false);
        let pinned = l.cols.iter().filter(|c| c.pinned).count();
        assert!(l.cols.iter().take(pinned).all(|c| c.pinned));
    }

    #[test]
    fn autofit_uses_header_or_content() {
        let (_, mut l) = layout(&["a", "b"]);
        l.autofit(0, 5, 12);
        assert_eq!(l.cols[0].width, 12.0 + FIT_PADDING);
        l.autofit(1, 30, 4);
        assert_eq!(l.cols[1].width, 30.0 + FIT_PADDING);
        l.autofit(1, 0, 0);
        assert_eq!(l.cols[1].width, MIN_WIDTH.max(FIT_PADDING));
    }

    #[test]
    fn the_message_column_takes_the_remaining_width() {
        let s = schema(&["ts", "message", "code"]);
        let mut l = ColumnLayout::new(&s, &[0, 1, 2]);
        for (i, w) in [10.0, 20.0, 5.0].into_iter().enumerate() {
            l.resize(i, w);
        }
        // 35 chars * 10 px = 350 px used; 500 available: 150 px extra.
        let p = l.placements(500.0, 10.0);
        assert_eq!(p.len(), 3);
        assert_eq!((p[0].x, p[0].w), (0.0, 100.0));
        assert_eq!((p[1].x, p[1].w), (100.0, 200.0 + 150.0));
        assert_eq!((p[2].x, p[2].w), (450.0, 50.0));
        // Too narrow: nothing stretches and the table scrolls.
        let p = l.placements(200.0, 10.0);
        assert_eq!(p[1].w, 200.0);
        assert_eq!(l.total_width(10.0), 350.0);
    }

    #[test]
    fn hidden_columns_take_no_space() {
        let (_, mut l) = layout(&["a", "b", "c"]);
        l.set_visible(1, false);
        let p = l.placements(0.0, 10.0);
        assert_eq!(p.iter().map(|p| p.col).collect::<Vec<_>>(), vec![0, 2]);
        assert_eq!(p[1].x, p[0].w);
    }

    #[test]
    fn drop_index_follows_the_pointer_over_visible_columns() {
        let (_, mut l) = layout(&["a", "b", "c"]);
        for i in 0..3 {
            l.resize(i, 10.0);
        }
        let p = l.placements(0.0, 10.0);
        assert_eq!(l.drop_index(&p, 0.0, 10.0), 0);
        assert_eq!(l.drop_index(&p, 0.0, 120.0), 1);
        assert_eq!(l.drop_index(&p, 0.0, 999.0), 3);
        // Scrolled by 100 px: column 1 starts at 0.
        assert_eq!(l.drop_index(&p, 100.0, 60.0), 2);
        // Hidden columns are skipped.
        l.set_visible(0, false);
        let p = l.placements(0.0, 10.0);
        assert_eq!(l.drop_index(&p, 0.0, 10.0), 1);
    }

    #[test]
    fn snapshots_round_trip_and_tolerate_schema_changes() {
        let (s, mut l) = layout(&["a", "b", "c"]);
        l.resize(1, 33.0);
        l.set_visible(2, false);
        l.set_pinned(1, true);
        let snap = l.snapshot(&s);
        let json = serde_json::to_string(&snap).unwrap();
        let back: LayoutSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, snap);
        let mut fresh = ColumnLayout::new(&s, &[0, 1, 2]);
        fresh.apply_snapshot(&s, &back);
        assert_eq!(fresh, l);
        // The schema gained a column and lost `a`.
        let s2 = schema(&["b", "c", "z"]);
        let mut l2 = ColumnLayout::new(&s2, &[0, 1, 2]);
        l2.apply_snapshot(&s2, &back);
        assert_eq!(l2.cols.len(), 3);
        assert!(l2.cols[0].pinned && l2.cols[0].col == 0);
        assert_eq!(l2.cols[0].width, 33.0);
        assert!(l2.cols.iter().any(|c| c.col == 2));
        assert!(!l2.cols.iter().find(|c| c.col == 1).unwrap().visible);
    }

    #[test]
    fn a_snapshot_cannot_hide_everything() {
        let (s, mut l) = layout(&["a", "b"]);
        let snap = LayoutSnapshot {
            hidden: vec!["a".into(), "b".into()],
            ..LayoutSnapshot::default()
        };
        l.apply_snapshot(&s, &snap);
        assert!(l.visible_count() >= 1);
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// Any sequence of edits keeps every column exactly once, the
            /// pinned block in front, widths in range and one column visible.
            #[test]
            fn edits_keep_the_invariants(
                n in 1usize..8,
                ops in proptest::collection::vec((0u8..5, 0usize..10, 0usize..10, -50f32..500.0), 0..40),
            ) {
                let names: Vec<String> = (0..n).map(|i| format!("c{i}")).collect();
                let s = Schema::from_names(&names, &BTreeMap::new());
                let mut l = ColumnLayout::new(&s, &[]);
                for (op, a, b, w) in ops {
                    match op {
                        0 => { l.reorder(a, b); }
                        1 => { l.set_pinned(a, b % 2 == 0); }
                        2 => l.set_visible(a, b % 2 == 0),
                        3 => l.resize(a, w),
                        _ => l.autofit(a, b, a),
                    }
                    let mut cols: Vec<usize> = l.cols.iter().map(|c| c.col).collect();
                    cols.sort_unstable();
                    prop_assert_eq!(cols, (0..n).collect::<Vec<_>>());
                    let pinned = l.cols.iter().filter(|c| c.pinned).count();
                    prop_assert!(l.cols.iter().take(pinned).all(|c| c.pinned));
                    prop_assert!(l.cols.iter().all(|c| (MIN_WIDTH..=MAX_WIDTH).contains(&c.width)));
                    prop_assert!(l.visible_count() >= 1);
                    let p = l.placements(300.0, 8.0);
                    for w in p.windows(2) {
                        prop_assert!((w[0].x + w[0].w - w[1].x).abs() < 0.01);
                    }
                }
            }
        }
    }
}
