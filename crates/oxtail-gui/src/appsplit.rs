//! Splits: the state operations behind the pane layout (which tab lives in
//! which pane, splitting, moving and closing panes, cursor sync between
//! panes) and their persistence. Drawing is in `appui`.
//!
//! Model: `OxTailApp::tabs` stays one flat list; every tab has a `pane`. The
//! pane tree ([`PaneTree`]) only divides the area. Each pane remembers its
//! active tab; `OxTailApp::active` is the index of the focused pane's active
//! tab, so everything that acts on "the active tab" keeps working.

use std::collections::HashMap;

use oxtail_config::SplitDirection;

use crate::app::{Notice, OxTailApp};
use crate::guistate::PaneState;
use crate::panes::{Edge, PaneId, PaneTree};
use crate::tab::{Tab, TabInit};

impl OxTailApp {
    /// Number of panes.
    pub fn pane_count(&self) -> usize {
        self.panes.panes().len()
    }

    /// The pane with the keyboard focus.
    pub fn focused_pane(&self) -> PaneId {
        self.focused_pane
    }

    /// Indices of the tabs shown in `pane`, in tab order.
    pub fn tabs_in_pane(&self, pane: PaneId) -> Vec<usize> {
        self.tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| t.pane == pane)
            .map(|(i, _)| i)
            .collect()
    }

    /// The pane a tab is in.
    pub fn pane_of_tab(&self, tab_id: u64) -> Option<PaneId> {
        self.tabs.iter().find(|t| t.id == tab_id).map(|t| t.pane)
    }

    /// The index of the active tab of `pane`.
    pub fn active_index_in_pane(&self, pane: PaneId) -> Option<usize> {
        let id = self.pane_active.get(&pane)?;
        self.tabs
            .iter()
            .position(|t| t.id == *id && t.pane == pane)
            .or_else(|| self.tabs_in_pane(pane).first().copied())
    }

    /// Recomputes `active` from the focused pane's active tab.
    pub(crate) fn sync_active(&mut self) {
        if !self.panes.contains(self.focused_pane) {
            self.focused_pane = self.panes.panes().first().copied().unwrap_or(0);
        }
        if let Some(i) = self.active_index_in_pane(self.focused_pane) {
            self.active = i;
            self.pane_active.insert(self.focused_pane, self.tabs[i].id);
        } else if !self.tabs.is_empty() {
            // The focused pane has no tabs: focus one that has.
            if let Some(t) = self.tabs.first() {
                self.focused_pane = t.pane;
            }
            self.active = self.active_index_in_pane(self.focused_pane).unwrap_or(0);
        } else {
            self.active = 0;
        }
    }

    /// Gives the keyboard focus to `pane`.
    pub fn focus_pane(&mut self, pane: PaneId) {
        if self.focused_pane == pane || !self.panes.contains(pane) {
            return;
        }
        self.focused_pane = pane;
        self.sync_active();
    }

    /// Removes an empty pane from the layout (never the last one).
    pub(crate) fn remove_pane(&mut self, pane: PaneId) {
        self.pane_active.remove(&pane);
        if self.panes.panes().len() <= 1 {
            return;
        }
        let tree = std::mem::replace(&mut self.panes, PaneTree::leaf(0));
        self.panes = tree.remove(pane).unwrap_or_else(|| PaneTree::leaf(0));
        if self.focused_pane == pane {
            self.focused_pane = self.panes.panes().first().copied().unwrap_or(0);
        }
    }

    fn new_pane_id(&mut self) -> PaneId {
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        id
    }

    /// Cycles through the tabs of the focused pane.
    pub(crate) fn cycle_tab(&mut self, delta: isize) {
        let list = self.tabs_in_pane(self.focused_pane);
        if list.is_empty() {
            return;
        }
        let pos = list.iter().position(|&i| i == self.active).unwrap_or(0) as isize;
        let next = (pos + delta).rem_euclid(list.len() as isize) as usize;
        self.select_tab(list[next]);
    }

    /// Splits the focused pane: the active tab's file is opened again in a
    /// new pane on the given side (a second view of the same file, e.g. a
    /// filtered one above the full one). Tabs without a file (stdin, merged)
    /// cannot be duplicated: the tab is moved instead when the pane has
    /// others, else nothing happens.
    pub fn split_active(&mut self, direction: SplitDirection) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let edge = match direction {
            SplitDirection::Horizontal => Edge::Right,
            SplitDirection::Vertical => Edge::Bottom,
        };
        let pane = tab.pane;
        let Some(path) = tab.path.clone() else {
            if self.tabs_in_pane(pane).len() > 1 {
                let id = tab.id;
                self.move_tab_to_new_pane(id, pane, edge);
            } else {
                self.notices.push(Notice {
                    text: "This tab has no file to show twice".into(),
                    error: false,
                });
            }
            return;
        };
        let new_pane = self.new_pane_id();
        if !self.panes.split_at_edge(pane, new_pane, edge) {
            return;
        }
        // The copy starts like the original: same profile, encoding, filter.
        let init = self.duplicate_init(self.active);
        self.open_path_in(path, init, true, new_pane);
    }

    fn duplicate_init(&self, index: usize) -> TabInit {
        let tab = &self.tabs[index];
        let mut init = TabInit {
            follow: true,
            wrap: self.settings.wrap,
            profile: tab.forced_profile.clone(),
            encoding: tab.forced_encoding.clone(),
            ..TabInit::default()
        };
        if let Some(v) = tab.view() {
            init.follow = v.follow;
            init.filters = v.filter.to_query_strings();
            init.wrap = v.wrap;
        }
        init
    }

    /// Moves a tab into a new pane next to `target` (a tab dropped on the
    /// edge of a pane).
    pub fn move_tab_to_new_pane(&mut self, tab_id: u64, target: PaneId, edge: Edge) {
        let Some(from) = self.pane_of_tab(tab_id) else {
            return;
        };
        // Splitting a pane off its only tab would leave the old pane empty.
        if from == target && self.tabs_in_pane(from).len() <= 1 {
            return;
        }
        let new_pane = self.new_pane_id();
        if !self.panes.split_at_edge(target, new_pane, edge) {
            return;
        }
        self.assign_tab(tab_id, new_pane);
        self.pane_active.insert(new_pane, tab_id);
        self.focused_pane = new_pane;
        self.sync_active();
    }

    /// Moves a tab into an existing pane.
    pub fn move_tab_to_pane(&mut self, tab_id: u64, pane: PaneId) {
        if !self.panes.contains(pane) || self.pane_of_tab(tab_id) == Some(pane) {
            return;
        }
        self.assign_tab(tab_id, pane);
        self.pane_active.insert(pane, tab_id);
        self.focused_pane = pane;
        self.sync_active();
    }

    /// Puts a tab in `pane`, fixing up the pane it leaves.
    fn assign_tab(&mut self, tab_id: u64, pane: PaneId) {
        let Some(i) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        let from = self.tabs[i].pane;
        self.tabs[i].pane = pane;
        let left = self.tabs_in_pane(from);
        if left.is_empty() {
            self.remove_pane(from);
        } else if self.pane_active.get(&from) == Some(&tab_id) {
            // The tab before the one that left, else the first.
            let pick = left
                .iter()
                .rev()
                .find(|&&j| j < i)
                .copied()
                .unwrap_or(left[0]);
            self.pane_active.insert(from, self.tabs[pick].id);
        }
    }

    /// Switches cursor sync between panes on or off.
    pub fn set_sync_cursor(&mut self, on: bool) {
        self.sync_cursor = on;
        self.sync_last.clear();
    }

    /// Whether cursor sync is on.
    pub fn sync_cursor(&self) -> bool {
        self.sync_cursor
    }

    /// With sync on, moving the cursor in one pane moves it in the other
    /// panes that show the same file. Cheap: only the visible tab of each
    /// pane is looked at, and nothing happens with a single pane.
    pub(crate) fn sync_cursors(&mut self) {
        if !self.sync_cursor || self.pane_count() < 2 {
            return;
        }
        let visible: Vec<usize> = self
            .panes
            .panes()
            .iter()
            .filter_map(|&p| self.active_index_in_pane(p))
            .collect();
        let mut now: HashMap<u64, Option<u64>> = HashMap::new();
        for &i in &visible {
            if let Some(v) = self.tabs[i].view() {
                now.insert(self.tabs[i].id, v.user_cursor());
            }
        }
        // The leader is the one whose cursor changed since last frame
        // (preferring the focused pane's tab).
        let changed = |id: &u64| now.get(id) != self.sync_last.get(id);
        let leader = visible
            .iter()
            .copied()
            .filter(|&i| changed(&self.tabs[i].id))
            .find(|&i| i == self.active)
            .or_else(|| visible.iter().copied().find(|&i| changed(&self.tabs[i].id)));
        if let Some(l) = leader
            && let Some(cursor) = now.get(&self.tabs[l].id).copied().flatten()
        {
            let path = self.tabs[l].path.clone();
            for &i in &visible {
                if i == l || self.tabs[i].path != path || path.is_none() {
                    continue;
                }
                if let Some(v) = self.tabs[i].view_mut()
                    && v.user_cursor() != Some(cursor)
                {
                    v.jump_offset(cursor, true);
                    now.insert(self.tabs[i].id, Some(cursor));
                }
            }
        }
        self.sync_last = now;
    }

    // ------------------------------------------------------------ persisting

    /// The pane layout as it would be saved.
    pub(crate) fn pane_state(&self) -> Option<PaneState> {
        // Only tabs with a file are part of the session.
        let session_tabs: Vec<&Tab> = self.tabs.iter().filter(|t| t.path.is_some()).collect();
        if self.pane_count() <= 1 {
            return None;
        }
        let tab_panes: Vec<u32> = session_tabs.iter().map(|t| t.pane).collect();
        let pane_active: Vec<(u32, usize)> = self
            .panes
            .panes()
            .into_iter()
            .filter_map(|p| {
                let id = self.pane_active.get(&p)?;
                let idx = session_tabs.iter().position(|t| t.id == *id)?;
                Some((p, idx))
            })
            .collect();
        Some(PaneState {
            tree: Some(self.panes.clone()),
            tab_panes,
            focused: self.focused_pane,
            pane_active,
        })
    }

    /// The layout for the session file (`Session::layout`): leaves are tab
    /// indices of the session, so each pane shows its active tab.
    pub(crate) fn session_layout(&self) -> Option<oxtail_config::LayoutNode> {
        if self.pane_count() <= 1 {
            return None;
        }
        let session_tabs: Vec<&Tab> = self.tabs.iter().filter(|t| t.path.is_some()).collect();
        self.panes.to_layout_node(&|p| {
            let id = self.pane_active.get(&p)?;
            session_tabs.iter().position(|t| t.id == *id)
        })
    }

    /// Applies the saved pane layout after the session's tabs were created
    /// (in session order, so tab `i` of the session is `tabs[i]`).
    pub(crate) fn restore_layout(&mut self) {
        let Some(state) = self.gui_state.panes.clone() else {
            self.sync_active();
            return;
        };
        let session_count = self.tabs.len();
        let ok = state.tree.as_ref().is_some_and(|t| {
            t.is_sane()
                && state.tab_panes.len() == session_count
                && state.tab_panes.iter().all(|p| t.contains(*p))
        });
        if !ok {
            // Inconsistent (a file could not be listed, the file is from an
            // older version): everything in one pane.
            for t in &mut self.tabs {
                t.pane = 0;
            }
            self.panes = PaneTree::leaf(0);
            self.focused_pane = 0;
            self.pane_active.clear();
            if let Some(t) = self.tabs.get(self.active) {
                self.pane_active.insert(0, t.id);
            }
            self.sync_active();
            return;
        }
        let Some(tree) = state.tree else { return };
        for (t, p) in self.tabs.iter_mut().zip(&state.tab_panes) {
            t.pane = *p;
        }
        self.next_pane_id = tree.panes().into_iter().max().map_or(1, |m| m + 1);
        self.panes = tree;
        self.pane_active.clear();
        for (pane, idx) in &state.pane_active {
            if let Some(t) = self.tabs.get(*idx)
                && t.pane == *pane
            {
                self.pane_active.insert(*pane, t.id);
            }
        }
        self.focused_pane = state.focused;
        // The restored active tab of the session wins for the focused pane.
        if let Some(t) = self.tabs.get(self.active) {
            self.focused_pane = t.pane;
            self.pane_active.insert(t.pane, t.id);
        }
        self.sync_active();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use oxtail_config::DataDir;
    use oxtail_core::{Document, MemSource};

    use super::*;
    use crate::request::OpenRequest;
    use crate::startup::{AppInit, ExternalOpen, load_startup};

    fn app() -> OxTailApp {
        OxTailApp::from_init(AppInit {
            startup: load_startup(DataDir::in_memory("test")),
            request: OpenRequest::default(),
            external: ExternalOpen::disconnected(),
        })
    }

    fn doc() -> Arc<Document> {
        Arc::new(Document::from_source(
            Arc::new(MemSource::new(b"a\nb\nc\n".to_vec())),
            "x.log",
        ))
    }

    fn titles(a: &OxTailApp, pane: PaneId) -> Vec<String> {
        a.tabs_in_pane(pane)
            .into_iter()
            .map(|i| a.tabs[i].title.clone())
            .collect()
    }

    #[test]
    fn moving_a_tab_to_an_edge_creates_a_pane() {
        let mut a = app();
        a.open_document("a", doc());
        a.open_document("b", doc());
        a.open_document("c", doc());
        assert_eq!(a.pane_count(), 1);
        let b = a.tabs[1].id;
        a.move_tab_to_new_pane(b, 0, Edge::Right);
        assert_eq!(a.pane_count(), 2);
        assert_eq!(titles(&a, 0), vec!["a", "c"]);
        assert_eq!(titles(&a, 1), vec!["b"]);
        // The moved tab is active and its pane focused.
        assert_eq!(a.tabs[a.active].title, "b");
        assert_eq!(a.focused_pane(), 1);
        // Splitting off a pane's only tab does nothing.
        a.move_tab_to_new_pane(b, 1, Edge::Bottom);
        assert_eq!(a.pane_count(), 2);
    }

    #[test]
    fn closing_the_last_tab_of_a_pane_removes_the_pane() {
        let mut a = app();
        a.open_document("a", doc());
        a.open_document("b", doc());
        let b = a.tabs[1].id;
        a.move_tab_to_new_pane(b, 0, Edge::Bottom);
        assert_eq!(a.pane_count(), 2);
        let i = a.tabs.iter().position(|t| t.id == b).unwrap();
        a.close_tab(i);
        assert_eq!(a.pane_count(), 1);
        assert_eq!(a.focused_pane(), 0);
        assert_eq!(a.tabs[a.active].title, "a");
    }

    #[test]
    fn moving_a_tab_between_panes_keeps_both_consistent() {
        let mut a = app();
        for n in ["a", "b", "c"] {
            a.open_document(n, doc());
        }
        let (b, c) = (a.tabs[1].id, a.tabs[2].id);
        a.move_tab_to_new_pane(c, 0, Edge::Right);
        a.move_tab_to_pane(b, 1);
        assert_eq!(titles(&a, 0), vec!["a"]);
        assert_eq!(titles(&a, 1), vec!["b", "c"]);
        assert_eq!(a.tabs[a.active].title, "b");
        // Emptying pane 0 by moving its last tab collapses the split.
        let id_a = a.tabs[0].id;
        a.move_tab_to_pane(id_a, 1);
        assert_eq!(a.pane_count(), 1);
        assert_eq!(titles(&a, 1).len(), 3);
    }

    #[test]
    fn tabs_cycle_within_the_focused_pane() {
        let mut a = app();
        for n in ["a", "b", "c", "d"] {
            a.open_document(n, doc());
        }
        let d = a.tabs[3].id;
        a.move_tab_to_new_pane(d, 0, Edge::Right);
        a.focus_pane(0);
        assert_eq!(a.tabs[a.active].title, "c");
        a.cycle_tab(1);
        assert_eq!(a.tabs[a.active].title, "a");
        a.cycle_tab(-1);
        assert_eq!(a.tabs[a.active].title, "c");
        a.focus_pane(1);
        assert_eq!(a.tabs[a.active].title, "d");
        a.cycle_tab(1);
        assert_eq!(a.tabs[a.active].title, "d");
    }

    #[test]
    fn a_tab_without_a_file_cannot_be_duplicated() {
        let mut a = app();
        a.open_document("only", doc());
        a.split_active(SplitDirection::Horizontal);
        // No path: nothing was split, the user got a notice.
        assert_eq!(a.pane_count(), 1);
        assert!(!a.notices.is_empty());
    }

    #[test]
    fn the_layout_round_trips_through_the_gui_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for n in ["a.log", "b.log", "c.log"] {
            let p = dir.path().join(n);
            std::fs::write(&p, "x\n").unwrap();
            paths.push(p);
        }
        let mut a = app();
        for p in &paths {
            a.open_document("x", doc());
            let i = a.tabs.len() - 1;
            a.tabs[i].path = Some(p.clone());
            a.tabs[i].title = p.file_name().unwrap().to_string_lossy().into_owned();
        }
        let c = a.tabs[2].id;
        a.move_tab_to_new_pane(c, 0, Edge::Right);
        let state = a.pane_state().expect("two panes");
        assert_eq!(state.tab_panes, vec![0, 0, 1]);
        let layout = a.session_layout().expect("a layout");
        assert!(matches!(layout, oxtail_config::LayoutNode::Split { .. }));

        // A fresh app with the same tabs restores the layout.
        let mut b = app();
        b.gui_state.panes = Some(state);
        for (i, p) in paths.iter().enumerate() {
            b.open_document("x", doc());
            let last = b.tabs.len() - 1;
            b.tabs[last].path = Some(p.clone());
            b.tabs[last].title = format!("t{i}");
            // Tabs are created in the focused pane; restore assigns panes.
            b.tabs[last].pane = 0;
        }
        b.active = 0;
        b.restore_layout();
        assert_eq!(b.pane_count(), 2);
        assert_eq!(b.tabs[2].pane, 1);
        assert_eq!(b.tabs[0].pane, 0);
    }

    #[test]
    fn an_inconsistent_saved_layout_falls_back_to_one_pane() {
        let mut a = app();
        a.open_document("a", doc());
        a.gui_state.panes = Some(PaneState {
            tree: Some(PaneTree::leaf(7)),
            tab_panes: vec![3, 4, 5],
            ..PaneState::default()
        });
        a.restore_layout();
        assert_eq!(a.pane_count(), 1);
        assert!(a.tabs.iter().all(|t| t.pane == 0));
    }
}
