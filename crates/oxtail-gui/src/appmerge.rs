//! Opening merged tabs: from several open tabs ("Merge with..."), from a
//! list of files (`oxtail --merge a.log b.log`, restoring a session) and the
//! actions on a merged tab. The merging itself is in [`crate::merge`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use egui::Context;
use oxtail_core::Document;

use crate::app::{AppMsg, Notice, OxTailApp};
use crate::colspec::zone_of;
use crate::keymap::Action;
use crate::merge::MergeSource;
use crate::mergeview::MergedView;
use crate::tab::{Tab, TabContent, TabInit};

/// Most sources one merged tab takes (the packed entry has 8 bits for it).
pub const MAX_MERGE: usize = oxtail_time::MAX_SOURCES;

/// The "Merge tabs" dialog.
#[derive(Debug, Default)]
pub struct MergeDialog {
    /// Ids of the tabs that are ticked.
    pub selected: Vec<u64>,
}

/// A short title for a merged tab.
pub fn merged_title(names: &[String]) -> String {
    let joined = names.join(" + ");
    if joined.chars().count() <= 60 {
        format!("{joined} (merged)")
    } else {
        format!("{} files (merged)", names.len())
    }
}

impl OxTailApp {
    /// Tabs that can be merged: open documents (id and title).
    pub fn merge_candidates(&self) -> Vec<(u64, String)> {
        self.tabs
            .iter()
            .filter(|t| t.view().is_some())
            .map(|t| (t.id, t.title.clone()))
            .collect()
    }

    /// Opens a merged tab over ready-made sources. Returns the tab id.
    pub fn open_merged(&mut self, mut sources: Vec<MergeSource>) -> u64 {
        sources.truncate(MAX_MERGE);
        let id = self.next_id();
        let mv = MergedView::new(sources, zone_of(&self.settings.timezone), self.waker());
        let mut tab = Tab::opening(id, mv.title(), None, TabInit::default());
        tab.content = TabContent::Merged(Box::new(mv));
        tab.pane = self.focused_pane;
        self.tabs.push(tab);
        self.select_tab(self.tabs.len() - 1);
        id
    }

    /// Merges the documents of open tabs (they stay open; the merged tab
    /// shares their documents). Returns the new tab's id.
    pub fn merge_tabs(&mut self, tab_ids: &[u64]) -> Option<u64> {
        let sources: Vec<MergeSource> = tab_ids
            .iter()
            .filter_map(|id| self.tabs.iter().find(|t| t.id == *id))
            .filter_map(|t| {
                t.view().map(|v| MergeSource {
                    name: t.title.clone(),
                    doc: Arc::clone(&v.doc),
                    path: t.path.clone(),
                    owns: false,
                })
            })
            .collect();
        if sources.len() < 2 {
            self.notices.push(Notice {
                text: "Pick at least two open files to merge".into(),
                error: false,
            });
            return None;
        }
        Some(self.open_merged(sources))
    }

    /// Opens `paths` as one merged tab. The files are opened on a worker
    /// thread; the tab shows "Opening..." until they are ready.
    pub fn open_merged_paths(&mut self, paths: Vec<PathBuf>) -> u64 {
        let paths: Vec<PathBuf> = paths
            .into_iter()
            .take(MAX_MERGE)
            .map(|p| std::path::absolute(&p).unwrap_or(p))
            .collect();
        let names: Vec<String> = paths
            .iter()
            .map(|p| {
                p.file_name().map_or_else(
                    || p.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                )
            })
            .collect();
        let id = self.next_id();
        let mut tab = Tab::opening(id, merged_title(&names), None, TabInit::default());
        tab.pane = self.focused_pane;
        self.tabs.push(tab);
        self.select_tab(self.tabs.len() - 1);
        // No initial tail read: nobody would consume it.
        let opts = self.open_options(
            &TabInit {
                initial_line: Some(0),
                ..TabInit::default()
            },
            false,
        );
        let tx = self.msg_sender();
        let wake = self.waker();
        let spawned = std::thread::Builder::new()
            .name("oxtail-open-merge".into())
            .spawn(move || {
                let mut sources = Vec::new();
                let mut result = None;
                for (p, name) in paths.iter().zip(&names) {
                    match Document::open(p, opts.clone()) {
                        Ok(d) => {
                            let d = Arc::new(d);
                            let w = Arc::clone(&wake);
                            d.set_waker(Box::new(move || w()));
                            sources.push(MergeSource {
                                name: name.clone(),
                                doc: d,
                                path: Some(p.clone()),
                                owns: true,
                            });
                        }
                        Err(e) => {
                            result = Some(Err(format!("{}: {e}", p.display())));
                            break;
                        }
                    }
                }
                let result = result.unwrap_or(Ok(sources));
                let _ = tx.send(AppMsg::MergeOpened { tab_id: id, result });
                wake();
            });
        if let Err(e) = spawned {
            self.finish_merge_open(id, Err(format!("cannot start a thread: {e}")));
        }
        id
    }

    /// The opened sources of a merged tab arrived (or opening failed).
    pub(crate) fn finish_merge_open(
        &mut self,
        tab_id: u64,
        result: Result<Vec<MergeSource>, String>,
    ) {
        let waker = self.waker();
        let tz = zone_of(&self.settings.timezone);
        let Some(i) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        match result {
            Ok(sources) if !sources.is_empty() => {
                let paths: Vec<PathBuf> = sources.iter().filter_map(|s| s.path.clone()).collect();
                let mut mv = MergedView::new(sources, tz, waker);
                // A restored merged tab gets its filter back.
                if let Some(def) = self.gui_state.merged.iter().find(|d| d.paths == paths)
                    && !def.filters.is_empty()
                {
                    mv.filter.set_from_query_strings(&def.filters);
                }
                self.tabs[i].title = mv.title();
                self.tabs[i].content = TabContent::Merged(Box::new(mv));
            }
            Ok(_) => {
                self.tabs[i].content = TabContent::Failed("nothing to merge".into());
            }
            Err(msg) => {
                self.notices.push(Notice {
                    text: format!("Cannot merge: {msg}"),
                    error: true,
                });
                self.tabs[i].content = TabContent::Failed(msg);
            }
        }
    }

    /// The merge definitions to remember: the files of each merged tab.
    pub fn merged_defs(&self) -> Vec<crate::guistate::MergedDef> {
        self.tabs
            .iter()
            .filter_map(Tab::merged)
            .map(|m| crate::guistate::MergedDef {
                paths: m.sources.iter().filter_map(|s| s.path.clone()).collect(),
                filters: m.filter.to_query_strings(),
            })
            .filter(|d| d.paths.len() >= 2)
            .collect()
    }

    /// Carries out a keyboard or menu action on the active merged tab.
    pub(crate) fn perform_on_merged(&mut self, action: Action, ctx: &Context) {
        let Some(mv) = self.tabs.get_mut(self.active).and_then(Tab::merged_mut) else {
            return;
        };
        let now = std::time::Instant::now();
        match action {
            Action::Find => {
                mv.find.open = true;
                mv.find.focus = true;
                mv.find_changed(now - Duration::from_secs(1));
            }
            Action::FindNext | Action::FindPrev => {
                if mv.find.text.is_empty() {
                    mv.find.open = true;
                    mv.find.focus = true;
                } else {
                    if !mv.find.open {
                        mv.find.open = true;
                        mv.find_changed(now - Duration::from_secs(1));
                    }
                    mv.find_step(action == Action::FindNext);
                }
            }
            Action::ToggleFollow => mv.toggle_follow(),
            Action::ScrollTop => mv.jump_top(),
            Action::ScrollBottom => mv.jump_bottom(),
            Action::PageUp => mv.page(false),
            Action::PageDown => mv.page(true),
            Action::LineUp => {
                let h = mv.row_h;
                mv.scroll_px(-h);
            }
            Action::LineDown => {
                let h = mv.row_h;
                mv.scroll_px(h);
            }
            Action::ScrollLeft => mv.h_scroll = (mv.h_scroll - 40.0).max(0.0),
            Action::ScrollRight => mv.h_scroll += 40.0,
            Action::Copy | Action::CopyWithNumbers => match mv.selection_text() {
                Some(t) => ctx.copy_text(t),
                None => mv.toast("Nothing selected"),
            },
            Action::SelectAll => mv.select_all(),
            Action::Filter => mv.toggle_filter_panel(),
            Action::GotoLine | Action::GotoTime => {
                mv.toast("Go to line and time work in the single-file tabs");
            }
            _ => {}
        }
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use oxtail_config::DataDir;
    use oxtail_core::MemSource;

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

    fn doc(text: &str) -> Arc<Document> {
        Arc::new(Document::from_source(
            Arc::new(MemSource::new(text.as_bytes().to_vec())),
            "x.log",
        ))
    }

    #[test]
    fn titles_are_shortened_for_many_files() {
        assert_eq!(
            merged_title(&["a.log".into(), "b.log".into()]),
            "a.log + b.log (merged)"
        );
        let many: Vec<String> = (0..20).map(|i| format!("worker-{i}.log")).collect();
        assert_eq!(merged_title(&many), "20 files (merged)");
    }

    #[test]
    fn merging_tabs_shares_their_documents() {
        let mut a = app();
        a.open_document("a.log", doc("2026-09-29 10:00:00 a\n"));
        a.open_document("b.log", doc("2026-09-29 10:00:01 b\n"));
        assert_eq!(a.merge_candidates().len(), 2);
        let ids: Vec<u64> = a.merge_candidates().iter().map(|c| c.0).collect();
        // One tab is not enough.
        assert!(a.merge_tabs(&ids[..1]).is_none());
        assert!(!a.notices.is_empty());
        let id = a.merge_tabs(&ids).expect("a merged tab");
        assert_eq!(a.tab_count(), 3);
        let t = a.tabs.iter().find(|t| t.id == id).unwrap();
        let m = t.merged().unwrap();
        assert_eq!(t.label(), "a.log + b.log (merged)");
        assert!(m.sources.iter().all(|s| !s.owns));
        assert!(Arc::ptr_eq(
            &m.sources[0].doc,
            &a.tabs[0].view().unwrap().doc
        ));
        // The merged tab is active and is not restorable as a plain tab.
        assert_eq!(a.tabs[a.active].id, id);
        assert!(a.current_session().tabs.is_empty());
        // Closing a source tab makes the merged view drain the document.
        a.close_tab(0);
        let m = a.tabs.iter().find_map(Tab::merged).unwrap();
        assert!(m.sources[0].owns && !m.sources[1].owns);
    }

    #[test]
    fn merged_defs_list_the_files() {
        let mut a = app();
        a.open_document("a.log", doc("x\n"));
        a.open_document("b.log", doc("y\n"));
        let ids: Vec<u64> = a.merge_candidates().iter().map(|c| c.0).collect();
        a.merge_tabs(&ids);
        // Documents without a path give no definition to restore.
        assert!(a.merged_defs().is_empty());
        a.tabs[0].path = Some("/logs/a.log".into());
        a.tabs[1].path = Some("/logs/b.log".into());
        let ids: Vec<u64> = a.merge_candidates().iter().map(|c| c.0).collect();
        a.merge_tabs(&ids);
        assert_eq!(a.merged_defs().len(), 1);
        assert_eq!(a.merged_defs()[0].paths.len(), 2);
    }

    #[test]
    fn filters_are_saved_and_restored_with_the_merged_tab() {
        let mut a = app();
        let (pa, pb): (PathBuf, PathBuf) = ("/logs/a.log".into(), "/logs/b.log".into());
        let src = |name: &str, p: &PathBuf| MergeSource {
            name: name.into(),
            doc: doc("2026-09-29 10:00:00 x\n"),
            path: Some(p.clone()),
            owns: false,
        };
        let id = a.open_merged(vec![src("a.log", &pa), src("b.log", &pb)]);
        {
            let m = a.tabs.iter_mut().find_map(Tab::merged_mut).unwrap();
            m.filter.entries = vec![
                crate::filter::FilterEntry::include("ERROR"),
                crate::filter::FilterEntry {
                    include: false,
                    ..crate::filter::FilterEntry::include("health")
                },
            ];
        }
        let defs = a.merged_defs();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].filters, vec!["ERROR", "-health"]);
        // A restored tab (opened from paths) picks its filter up.
        a.close_tab(a.tabs.iter().position(|t| t.id == id).unwrap());
        a.gui_state.merged = defs;
        let id = a.open_merged_paths(vec![pa.clone(), pb.clone()]);
        a.finish_merge_open(id, Ok(vec![src("a.log", &pa), src("b.log", &pb)]));
        let m = a
            .tabs
            .iter()
            .find(|t| t.id == id)
            .unwrap()
            .merged()
            .unwrap();
        assert_eq!(m.filter.entries.len(), 2);
        assert!(!m.filter.entries[1].include);
    }
}
