//! Tabs and the session snapshot built from them.

use std::path::PathBuf;

use oxtail_config::{Bookmark, ProfileSet, Session, TabState, WindowGeometry};

use crate::docview::{BookmarkInfo, DocView};
use crate::highlight::choose_profile;

/// What a tab was asked to do when its document arrives.
#[derive(Debug, Clone, Default)]
pub struct TabInit {
    /// Follow the end of the file.
    pub follow: bool,
    /// Start with the last N lines.
    pub start_lines: Option<usize>,
    /// Filters (query strings).
    pub filters: Vec<String>,
    /// A profile chosen by the user.
    pub profile: Option<String>,
    /// Scroll to this 0-based line.
    pub initial_line: Option<u64>,
    /// Restored bookmarks.
    pub bookmarks: Vec<Bookmark>,
    /// Restored search query.
    pub search: Option<String>,
    /// Encoding label chosen by the user.
    pub encoding: Option<String>,
    /// Wrap lines.
    pub wrap: bool,
}

/// The state of a tab.
pub enum TabContent {
    /// The document is being opened on a worker thread.
    Opening(TabInit),
    /// Opening failed.
    Failed(String),
    /// The document is open.
    Ready(Box<DocView>),
}

/// One tab.
pub struct Tab {
    /// Unique id (stable while the app runs).
    pub id: u64,
    /// Title without decorations.
    pub title: String,
    /// The file, if the tab shows one.
    pub path: Option<PathBuf>,
    /// The tab's state.
    pub content: TabContent,
    /// Alert hits since the tab was last shown.
    pub badge: u32,
    /// The profile the user picked for this tab (kept in the session).
    pub forced_profile: Option<String>,
    /// The encoding label the user picked (kept in the session).
    pub forced_encoding: Option<String>,
}

impl Tab {
    /// A tab that is being opened.
    pub fn opening(id: u64, title: String, path: Option<PathBuf>, init: TabInit) -> Self {
        Self {
            id,
            title,
            path,
            forced_profile: init.profile.clone(),
            forced_encoding: init.encoding.clone(),
            content: TabContent::Opening(init),
            badge: 0,
        }
    }

    /// The view, if the document is open.
    pub fn view(&self) -> Option<&DocView> {
        match &self.content {
            TabContent::Ready(v) => Some(v),
            _ => None,
        }
    }

    /// The view, mutably.
    pub fn view_mut(&mut self) -> Option<&mut DocView> {
        match &mut self.content {
            TabContent::Ready(v) => Some(v),
            _ => None,
        }
    }

    /// The label shown on the tab: title plus an alert badge.
    pub fn label(&self) -> String {
        let base = match &self.content {
            TabContent::Opening(_) => format!("{} (opening\u{2026})", self.title),
            TabContent::Failed(_) => format!("{} (failed)", self.title),
            TabContent::Ready(_) => self.title.clone(),
        };
        if self.badge > 0 {
            format!("({}) {base}", self.badge)
        } else {
            base
        }
    }
}

/// Selects and applies the profile of a freshly read first-lines sample.
pub fn apply_sniffed_profile(tab: &mut Tab, profiles: &ProfileSet, lines: &[String]) {
    let forced = tab.forced_profile.clone();
    let path = tab
        .path
        .clone()
        .unwrap_or_else(|| PathBuf::from(&tab.title));
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let chosen = choose_profile(profiles, forced.as_deref(), &path, &refs);
    if let Some(view) = tab.view_mut() {
        view.hl.borrow_mut().set_profile(chosen);
    }
}

/// Builds the session from the open tabs. Tabs without a file (stdin) are
/// not restorable and are skipped. Bookmarks of files that are not open are
/// kept from `base`.
pub fn build_session(
    tabs: &[Tab],
    active: usize,
    base: &Session,
    window: Option<WindowGeometry>,
    recent_limit: usize,
) -> Session {
    let mut s = Session::new();
    s.recent_files = base.recent_files.clone();
    s.recent_files.truncate(recent_limit);
    s.bookmarks = base.bookmarks.clone();
    s.window = window.or(base.window);
    let mut active_index = 0;
    for (i, tab) in tabs.iter().enumerate() {
        let Some(path) = &tab.path else { continue };
        if i == active {
            active_index = s.tabs.len();
        }
        let mut st = TabState {
            path: path.clone(),
            encoding: tab.forced_encoding.clone(),
            profile: tab.forced_profile.clone(),
            follow: true,
            ..TabState::default()
        };
        match &tab.content {
            TabContent::Ready(v) => {
                st.follow = v.follow;
                st.scroll_anchor_line = v
                    .cache
                    .get(v.pos.top)
                    .filter(|l| l.number_exact)
                    .map_or(0, |l| l.number);
                st.filters = v.filter.to_query_strings();
                st.search_query = (!v.find.text.is_empty()).then(|| v.find.text.clone());
                let marks: Vec<Bookmark> = v
                    .bookmarks
                    .iter()
                    .map(|(n, b): (&u64, &BookmarkInfo)| Bookmark {
                        line: *n,
                        label: b.label.clone(),
                    })
                    .collect();
                if marks.is_empty() {
                    s.bookmarks.remove(path);
                } else {
                    s.bookmarks.insert(path.clone(), marks);
                }
            }
            TabContent::Opening(init) => {
                st.follow = init.follow;
                st.scroll_anchor_line = init.initial_line.unwrap_or(0);
                st.filters.clone_from(&init.filters);
                st.search_query.clone_from(&init.search);
            }
            TabContent::Failed(_) => {}
        }
        s.tabs.push(st);
    }
    s.active_tab = active_index.min(s.tabs.len().saturating_sub(1));
    s
}

/// Where a dragged tab should be dropped, given the horizontal centres of the
/// tabs and the pointer position.
pub fn drop_index(centers: &[f32], pointer_x: f32) -> usize {
    centers.iter().take_while(|&&c| c < pointer_x).count()
}

/// Moves the item at `from` so it ends up at insertion index `to` (an index
/// into the original list, as returned by [`drop_index`]). Returns the item's
/// new index.
pub fn move_item<T>(items: &mut Vec<T>, from: usize, to: usize) -> usize {
    if from >= items.len() {
        return from;
    }
    let item = items.remove(from);
    let target = if to > from { to - 1 } else { to }.min(items.len());
    items.insert(target, item);
    target
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docview::ViewInit;
    use oxtail_core::{Document, MemSource};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn ready_tab(id: u64, path: &str, lines: usize) -> Tab {
        let text: String = (0..lines).map(|i| format!("line {i}\n")).collect();
        let doc = Arc::new(Document::from_source(
            Arc::new(MemSource::new(text.into_bytes())),
            "x",
        ));
        let mut view = DocView::new(
            doc,
            &ViewInit {
                follow: true,
                ..ViewInit::default()
            },
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        let m = crate::docview::Metrics {
            view_h: 160.0,
            row_h: 16.0,
        };
        while !view.snapshot.lines.exact
            || view.snapshot.lines.known != lines as u64
            || view.last_rows.is_empty()
        {
            view.pump(Instant::now());
            view.update_rows(m, &|_| 16.0);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        Tab {
            id,
            title: path.into(),
            path: Some(path.into()),
            content: TabContent::Ready(Box::new(view)),
            badge: 0,
            forced_profile: None,
            forced_encoding: None,
        }
    }

    #[test]
    fn labels_show_state_and_badge() {
        let mut t = Tab::opening(1, "a.log".into(), None, TabInit::default());
        assert_eq!(t.label(), "a.log (opening\u{2026})");
        t.badge = 3;
        assert_eq!(t.label(), "(3) a.log (opening\u{2026})");
        t.content = TabContent::Failed("nope".into());
        t.badge = 0;
        assert_eq!(t.label(), "a.log (failed)");
        assert!(t.view().is_none());
    }

    #[test]
    fn session_captures_tab_state() {
        let mut a = ready_tab(1, "/logs/a.log", 50);
        {
            let v = a.view_mut().unwrap();
            v.follow = false;
            v.find.text = "needle".into();
            v.filter
                .set_from_query_strings(&["ERROR".to_string(), "-health".to_string()]);
            v.bookmarks.insert(
                7,
                BookmarkInfo {
                    label: "seven".into(),
                    offset: None,
                },
            );
            // Pretend the view is scrolled to line 10.
            let off = v.cache.get_number(10).map(|l| l.offset);
            if let Some(o) = off {
                v.pos.top = o;
            }
        }
        a.forced_profile = Some("nginx".into());
        a.forced_encoding = Some("utf-16le".into());
        let mut stdin_tab = ready_tab(2, "<stdin>", 3);
        stdin_tab.path = None;
        let b = ready_tab(3, "/logs/b.log", 5);
        let base = Session {
            bookmarks: [(
                "/logs/old.log".into(),
                vec![Bookmark {
                    line: 1,
                    label: String::new(),
                }],
            )]
            .into_iter()
            .collect(),
            recent_files: vec!["/logs/a.log".into(), "/logs/b.log".into()],
            ..Session::new()
        };
        let win = WindowGeometry {
            x: 1.0,
            y: 2.0,
            width: 800.0,
            height: 600.0,
            maximized: false,
        };
        let s = build_session(&[a, stdin_tab, b], 2, &base, Some(win), 10);
        assert_eq!(s.tabs.len(), 2);
        assert_eq!(s.active_tab, 1);
        let t0 = &s.tabs[0];
        assert_eq!(t0.path, PathBuf::from("/logs/a.log"));
        assert!(!t0.follow);
        assert_eq!(t0.profile.as_deref(), Some("nginx"));
        assert_eq!(t0.encoding.as_deref(), Some("utf-16le"));
        assert_eq!(t0.search_query.as_deref(), Some("needle"));
        assert_eq!(t0.filters, vec!["ERROR", "-health"]);
        assert_eq!(t0.scroll_anchor_line, 10);
        assert!(s.tabs[1].follow);
        assert_eq!(s.bookmarks[&PathBuf::from("/logs/a.log")][0].line, 7);
        assert_eq!(s.bookmarks[&PathBuf::from("/logs/a.log")][0].label, "seven");
        assert!(s.bookmarks.contains_key(&PathBuf::from("/logs/old.log")));
        assert!(!s.bookmarks.contains_key(&PathBuf::from("/logs/b.log")));
        assert_eq!(s.window, Some(win));
        assert_eq!(s.recent_files.len(), 2);
    }

    #[test]
    fn opening_tabs_keep_their_request_in_the_session() {
        let init = TabInit {
            follow: false,
            initial_line: Some(42),
            filters: vec!["x".into()],
            ..TabInit::default()
        };
        let t = Tab::opening(1, "a".into(), Some("/a.log".into()), init);
        let s = build_session(&[t], 0, &Session::new(), None, 5);
        assert_eq!(s.tabs[0].scroll_anchor_line, 42);
        assert!(!s.tabs[0].follow);
        assert_eq!(s.tabs[0].filters, vec!["x"]);
    }

    #[test]
    fn dragging_reorders_tabs() {
        let centers = [50.0, 150.0, 250.0];
        assert_eq!(drop_index(&centers, 10.0), 0);
        assert_eq!(drop_index(&centers, 100.0), 1);
        assert_eq!(drop_index(&centers, 500.0), 3);
        let mut v = vec!['a', 'b', 'c', 'd'];
        assert_eq!(move_item(&mut v, 0, 3), 2);
        assert_eq!(v, vec!['b', 'c', 'a', 'd']);
        assert_eq!(move_item(&mut v, 3, 0), 0);
        assert_eq!(v, vec!['d', 'b', 'c', 'a']);
        assert_eq!(move_item(&mut v, 1, 1), 1);
        assert_eq!(move_item(&mut v, 9, 0), 9);
        assert_eq!(move_item(&mut v, 0, 99), 3);
        assert_eq!(v, vec!['b', 'c', 'a', 'd']);
    }
}
