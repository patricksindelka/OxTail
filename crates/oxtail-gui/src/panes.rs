//! The split layout of the tab area: a binary tree of panes.
//!
//! Every leaf is a pane with its own tab strip; the tree only decides how the
//! area is divided. Everything here is pure (rectangles in, rectangles out)
//! and unit-tested; the drawing lives in `appui`.

use egui::{Pos2, Rect, pos2};
use oxtail_config::{LayoutNode, SplitDirection};
use serde::{Deserialize, Serialize};

/// Thickness of the draggable divider between two panes.
pub const SPLITTER: f32 = 5.0;
/// Panes never get smaller than this (per axis) when a divider is dragged.
pub const MIN_PANE: f32 = 120.0;
/// Fraction of a pane at each edge that counts as an "edge drop" target.
pub const EDGE_ZONE: f32 = 0.25;

/// A pane identifier (stable while the app runs).
pub type PaneId = u32;

/// The split tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PaneTree {
    /// One pane.
    Leaf {
        /// The pane.
        pane: PaneId,
    },
    /// Two areas next to (`Horizontal`) or above (`Vertical`) each other.
    Split {
        /// How the area is divided.
        direction: SplitDirection,
        /// Share of the space given to `first`.
        ratio: f32,
        /// Left / top area.
        first: Box<PaneTree>,
        /// Right / bottom area.
        second: Box<PaneTree>,
    },
}

/// Where a tab was dropped on a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// Left edge: a new pane to the left.
    Left,
    /// Right edge: a new pane to the right.
    Right,
    /// Top edge: a new pane above.
    Top,
    /// Bottom edge: a new pane below.
    Bottom,
}

/// A divider between two areas.
#[derive(Debug, Clone, PartialEq)]
pub struct Splitter {
    /// The path from the root to the split (`false` = first child).
    pub path: Vec<bool>,
    /// The draggable strip.
    pub rect: Rect,
    /// Direction of the split it belongs to.
    pub direction: SplitDirection,
    /// The rectangle of the whole split (for turning pointer positions into
    /// ratios).
    pub area: Rect,
}

/// The result of laying a tree out in a rectangle.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layout {
    /// The rectangle of every pane.
    pub panes: Vec<(PaneId, Rect)>,
    /// The dividers.
    pub splitters: Vec<Splitter>,
}

impl PaneTree {
    /// A tree with one pane.
    pub fn leaf(pane: PaneId) -> Self {
        PaneTree::Leaf { pane }
    }

    /// The panes, left to right / top to bottom.
    pub fn panes(&self) -> Vec<PaneId> {
        match self {
            PaneTree::Leaf { pane } => vec![*pane],
            PaneTree::Split { first, second, .. } => {
                let mut v = first.panes();
                v.extend(second.panes());
                v
            }
        }
    }

    /// Whether `pane` is a leaf of the tree.
    pub fn contains(&self, pane: PaneId) -> bool {
        self.panes().contains(&pane)
    }

    /// Splits the pane `target`: it and `new_pane` share its area, `new_pane`
    /// on the `new_first` side. Returns `false` when `target` is not in the
    /// tree.
    pub fn split(
        &mut self,
        target: PaneId,
        new_pane: PaneId,
        direction: SplitDirection,
        new_first: bool,
    ) -> bool {
        match self {
            PaneTree::Leaf { pane } if *pane == target => {
                let old = PaneTree::Leaf { pane: target };
                let new = PaneTree::Leaf { pane: new_pane };
                let (first, second) = if new_first { (new, old) } else { (old, new) };
                *self = PaneTree::Split {
                    direction,
                    ratio: 0.5,
                    first: Box::new(first),
                    second: Box::new(second),
                };
                true
            }
            PaneTree::Leaf { .. } => false,
            PaneTree::Split { first, second, .. } => {
                first.split(target, new_pane, direction, new_first)
                    || second.split(target, new_pane, direction, new_first)
            }
        }
    }

    /// Splits `target` at the edge a tab was dropped on.
    pub fn split_at_edge(&mut self, target: PaneId, new_pane: PaneId, edge: Edge) -> bool {
        let (dir, new_first) = match edge {
            Edge::Left => (SplitDirection::Horizontal, true),
            Edge::Right => (SplitDirection::Horizontal, false),
            Edge::Top => (SplitDirection::Vertical, true),
            Edge::Bottom => (SplitDirection::Vertical, false),
        };
        self.split(target, new_pane, dir, new_first)
    }

    /// Removes `pane`; its sibling takes the space. `None` when the tree
    /// becomes empty (the last pane was removed).
    #[must_use]
    pub fn remove(self, pane: PaneId) -> Option<PaneTree> {
        match self {
            PaneTree::Leaf { pane: p } => (p != pane).then_some(PaneTree::Leaf { pane: p }),
            PaneTree::Split {
                direction,
                ratio,
                first,
                second,
            } => match (first.remove(pane), second.remove(pane)) {
                (Some(a), Some(b)) => Some(PaneTree::Split {
                    direction,
                    ratio,
                    first: Box::new(a),
                    second: Box::new(b),
                }),
                (a, b) => a.or(b),
            },
        }
    }

    /// Sets the ratio of the split at `path`, clamped so both sides keep at
    /// least [`MIN_PANE`] pixels of `extent` (the size of that split along its
    /// direction).
    pub fn set_ratio(&mut self, path: &[bool], ratio: f32, extent: f32) {
        match (self, path.split_first()) {
            (PaneTree::Split { ratio: r, .. }, None) => *r = clamp_ratio(ratio, extent),
            (PaneTree::Split { first, second, .. }, Some((&second_child, rest))) => {
                if second_child {
                    second.set_ratio(rest, ratio, extent);
                } else {
                    first.set_ratio(rest, ratio, extent);
                }
            }
            _ => {}
        }
    }

    /// Divides `rect` among the panes.
    pub fn layout(&self, rect: Rect) -> Layout {
        let mut out = Layout::default();
        self.layout_into(rect, &mut Vec::new(), &mut out);
        out
    }

    fn layout_into(&self, rect: Rect, path: &mut Vec<bool>, out: &mut Layout) {
        match self {
            PaneTree::Leaf { pane } => out.panes.push((*pane, rect)),
            PaneTree::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let ratio = if ratio.is_finite() {
                    ratio.clamp(0.05, 0.95)
                } else {
                    0.5
                };
                let (a, bar, b) = match direction {
                    SplitDirection::Horizontal => {
                        let usable = (rect.width() - SPLITTER).max(0.0);
                        let w = (usable * ratio).round();
                        (
                            Rect::from_min_max(rect.min, pos2(rect.left() + w, rect.bottom())),
                            Rect::from_min_max(
                                pos2(rect.left() + w, rect.top()),
                                pos2(rect.left() + w + SPLITTER, rect.bottom()),
                            ),
                            Rect::from_min_max(
                                pos2((rect.left() + w + SPLITTER).min(rect.right()), rect.top()),
                                rect.max,
                            ),
                        )
                    }
                    SplitDirection::Vertical => {
                        let usable = (rect.height() - SPLITTER).max(0.0);
                        let h = (usable * ratio).round();
                        (
                            Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + h)),
                            Rect::from_min_max(
                                pos2(rect.left(), rect.top() + h),
                                pos2(rect.right(), rect.top() + h + SPLITTER),
                            ),
                            Rect::from_min_max(
                                pos2(rect.left(), (rect.top() + h + SPLITTER).min(rect.bottom())),
                                rect.max,
                            ),
                        )
                    }
                };
                out.splitters.push(Splitter {
                    path: path.clone(),
                    rect: bar,
                    direction: *direction,
                    area: rect,
                });
                path.push(false);
                first.layout_into(a, path, out);
                path.pop();
                path.push(true);
                second.layout_into(b, path, out);
                path.pop();
            }
        }
    }

    /// Converts to the session's layout tree; `tab_of` gives the tab index a
    /// pane shows (panes without a tab vanish).
    pub fn to_layout_node(&self, tab_of: &dyn Fn(PaneId) -> Option<usize>) -> Option<LayoutNode> {
        match self {
            PaneTree::Leaf { pane } => tab_of(*pane).map(|tab| LayoutNode::Leaf { tab }),
            PaneTree::Split {
                direction,
                ratio,
                first,
                second,
            } => match (first.to_layout_node(tab_of), second.to_layout_node(tab_of)) {
                (Some(a), Some(b)) => Some(LayoutNode::Split {
                    direction: *direction,
                    ratio: *ratio,
                    first: Box::new(a),
                    second: Box::new(b),
                }),
                (a, b) => a.or(b),
            },
        }
    }

    /// Whether the tree is a plausible layout: bounded depth, finite ratios,
    /// every pane once.
    pub fn is_sane(&self) -> bool {
        fn go(t: &PaneTree, budget: &mut usize) -> bool {
            if *budget == 0 {
                return false;
            }
            *budget -= 1;
            match t {
                PaneTree::Leaf { .. } => true,
                PaneTree::Split {
                    ratio,
                    first,
                    second,
                    ..
                } => ratio.is_finite() && go(first, budget) && go(second, budget),
            }
        }
        let mut budget = 64;
        let panes = self.panes();
        let mut sorted = panes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        go(self, &mut budget) && sorted.len() == panes.len()
    }
}

fn clamp_ratio(ratio: f32, extent: f32) -> f32 {
    if !ratio.is_finite() {
        return 0.5;
    }
    let usable = (extent - SPLITTER).max(1.0);
    let min = (MIN_PANE / usable).min(0.45);
    ratio.clamp(min, 1.0 - min)
}

/// Which edge zone of `rect` the pointer is in, if any. The corners go to the
/// nearer edge; the middle is not an edge.
pub fn edge_at(rect: Rect, p: Pos2) -> Option<Edge> {
    if !rect.contains(p) || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    let fx = (p.x - rect.left()) / rect.width();
    let fy = (p.y - rect.top()) / rect.height();
    let candidates = [
        (fx, Edge::Left),
        (1.0 - fx, Edge::Right),
        (fy, Edge::Top),
        (1.0 - fy, Edge::Bottom),
    ];
    candidates
        .into_iter()
        .filter(|(d, _)| *d < EDGE_ZONE)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, e)| e)
}

/// The ratio a pointer position `p` asks for on a splitter.
pub fn ratio_at(s: &Splitter, p: Pos2) -> f32 {
    match s.direction {
        SplitDirection::Horizontal => {
            (p.x - s.area.left() - SPLITTER * 0.5) / (s.area.width() - SPLITTER).max(1.0)
        }
        SplitDirection::Vertical => {
            (p.y - s.area.top() - SPLITTER * 0.5) / (s.area.height() - SPLITTER).max(1.0)
        }
    }
}

/// The extent of a splitter's area along its direction.
pub fn extent_of(s: &Splitter) -> f32 {
    match s.direction {
        SplitDirection::Horizontal => s.area.width(),
        SplitDirection::Vertical => s.area.height(),
    }
}

/// Rebuilds a tree from the session's layout, mapping each leaf's tab index
/// through `pane_of_tab`.
pub fn from_layout_node(
    node: &LayoutNode,
    pane_of_tab: &dyn Fn(usize) -> Option<PaneId>,
) -> Option<PaneTree> {
    match node {
        LayoutNode::Leaf { tab } => pane_of_tab(*tab).map(PaneTree::leaf),
        LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } => match (
            from_layout_node(first, pane_of_tab),
            from_layout_node(second, pane_of_tab),
        ) {
            (Some(a), Some(b)) => Some(PaneTree::Split {
                direction: *direction,
                ratio: *ratio,
                first: Box::new(a),
                second: Box::new(b),
            }),
            (a, b) => a.or(b),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1005.0, 505.0))
    }

    #[test]
    fn splitting_and_removing_panes() {
        let mut t = PaneTree::leaf(0);
        assert!(t.split_at_edge(0, 1, Edge::Right));
        assert_eq!(t.panes(), vec![0, 1]);
        assert!(t.split_at_edge(1, 2, Edge::Bottom));
        assert_eq!(t.panes(), vec![0, 1, 2]);
        assert!(t.split_at_edge(0, 3, Edge::Left));
        assert_eq!(t.panes(), vec![3, 0, 1, 2]);
        assert!(!t.split_at_edge(9, 4, Edge::Left));
        assert!(t.is_sane());
        let t = t.remove(1).unwrap();
        assert_eq!(t.panes(), vec![3, 0, 2]);
        let t = t.remove(3).unwrap().remove(0).unwrap();
        assert_eq!(t, PaneTree::leaf(2));
        assert!(t.remove(2).is_none());
    }

    #[test]
    fn layout_divides_the_area_and_leaves_room_for_dividers() {
        let mut t = PaneTree::leaf(0);
        t.split_at_edge(0, 1, Edge::Right);
        let l = t.layout(area());
        assert_eq!(l.panes.len(), 2);
        assert_eq!(l.splitters.len(), 1);
        let (_, a) = l.panes[0];
        let (_, b) = l.panes[1];
        assert_eq!(a.left(), 0.0);
        assert_eq!(b.right(), 1005.0);
        assert_eq!(b.left() - a.right(), SPLITTER);
        assert_eq!(a.height(), 505.0);
        assert_eq!(l.splitters[0].rect.width(), SPLITTER);
        // Vertical split.
        let mut t = PaneTree::leaf(0);
        t.split_at_edge(0, 1, Edge::Top);
        let l = t.layout(area());
        assert_eq!(l.panes[0].0, 1); // the new pane is on top
        assert_eq!(l.panes[0].1.width(), 1005.0);
        assert!(l.panes[0].1.bottom() < l.panes[1].1.top());
    }

    #[test]
    fn dragging_a_divider_changes_the_ratio_within_limits() {
        let mut t = PaneTree::leaf(0);
        t.split_at_edge(0, 1, Edge::Right);
        let l = t.layout(area());
        let s = &l.splitters[0];
        let r = ratio_at(s, pos2(300.0, 10.0));
        assert!((r - 0.3).abs() < 0.01, "{r}");
        t.set_ratio(&s.path, r, extent_of(s));
        let l = t.layout(area());
        assert!((l.panes[0].1.width() - 300.0).abs() < 3.0);
        // Far too small: clamped to the minimum pane size.
        t.set_ratio(&s.path, 0.0, extent_of(s));
        let l = t.layout(area());
        assert!(l.panes[0].1.width() >= MIN_PANE - 2.0);
        t.set_ratio(&s.path, f32::NAN, extent_of(s));
        assert!(t.is_sane());
    }

    #[test]
    fn nested_paths_address_the_right_split() {
        let mut t = PaneTree::leaf(0);
        t.split_at_edge(0, 1, Edge::Right);
        t.split_at_edge(1, 2, Edge::Bottom);
        let l = t.layout(area());
        assert_eq!(l.splitters.len(), 2);
        let inner = l.splitters.iter().find(|s| s.path == vec![true]).unwrap();
        assert_eq!(inner.direction, SplitDirection::Vertical);
        t.set_ratio(&inner.path, 0.8, extent_of(inner));
        let l2 = t.layout(area());
        let h1 = l2.panes.iter().find(|(p, _)| *p == 1).unwrap().1.height();
        let h2 = l2.panes.iter().find(|(p, _)| *p == 2).unwrap().1.height();
        assert!(h1 > h2 * 2.0);
    }

    #[test]
    fn edge_zones() {
        let r = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0));
        assert_eq!(edge_at(r, pos2(5.0, 50.0)), Some(Edge::Left));
        assert_eq!(edge_at(r, pos2(95.0, 50.0)), Some(Edge::Right));
        assert_eq!(edge_at(r, pos2(50.0, 5.0)), Some(Edge::Top));
        assert_eq!(edge_at(r, pos2(50.0, 96.0)), Some(Edge::Bottom));
        assert_eq!(edge_at(r, pos2(50.0, 50.0)), None);
        // Corner: the nearer edge wins.
        assert_eq!(edge_at(r, pos2(3.0, 10.0)), Some(Edge::Left));
        assert_eq!(edge_at(r, pos2(10.0, 3.0)), Some(Edge::Top));
        assert_eq!(edge_at(r, pos2(200.0, 50.0)), None);
    }

    #[test]
    fn session_layout_round_trip() {
        let mut t = PaneTree::leaf(0);
        t.split_at_edge(0, 1, Edge::Right);
        t.split_at_edge(1, 2, Edge::Bottom);
        let node = t
            .to_layout_node(&|p| Some(p as usize + 10))
            .expect("all panes have tabs");
        let back = from_layout_node(&node, &|tab| Some(tab as u32 - 10)).unwrap();
        assert_eq!(back, t);
        // Panes without a tab vanish and their split collapses.
        let node = t
            .to_layout_node(&|p| (p != 1).then_some(p as usize))
            .unwrap();
        assert_eq!(
            node,
            LayoutNode::Split {
                direction: SplitDirection::Horizontal,
                ratio: 0.5,
                first: Box::new(LayoutNode::Leaf { tab: 0 }),
                second: Box::new(LayoutNode::Leaf { tab: 2 }),
            }
        );
    }

    #[test]
    fn json_round_trip_and_sanity() {
        let mut t = PaneTree::leaf(0);
        t.split_at_edge(0, 1, Edge::Left);
        let json = serde_json::to_string(&t).unwrap();
        let back: PaneTree = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
        let dup = PaneTree::Split {
            direction: SplitDirection::Horizontal,
            ratio: 0.5,
            first: Box::new(PaneTree::leaf(1)),
            second: Box::new(PaneTree::leaf(1)),
        };
        assert!(!dup.is_sane());
    }
}
