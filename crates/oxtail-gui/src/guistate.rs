//! GUI-side state that `oxtail-config`'s `Session` has no room for, kept in
//! `gui-state.json` inside the data folder: the column choice per file, the
//! pane assignment of tabs and the merged tabs.
//!
//! Like the session it is tolerant: a missing or garbage file gives the
//! defaults, and it is written atomically by the persistence worker.

use std::path::{Path, PathBuf};

use oxtail_columns::ParserSpec;
use serde::{Deserialize, Serialize};

use crate::collayout::LayoutSnapshot;
use crate::panes::PaneTree;

/// File name inside the data folder.
pub const GUI_STATE_FILE: &str = "gui-state.json";
/// Files whose column choice is remembered.
pub const MAX_REMEMBERED: usize = 200;
/// Current format version.
pub const GUI_STATE_VERSION: u32 = 1;

/// What the user decided about the structure of one file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedStructure {
    /// The parser in use (`None` with `dismissed`: plain text was chosen).
    pub spec: Option<ParserSpec>,
    /// Show the table view.
    pub table: bool,
    /// The user dismissed the suggestion (do not ask again).
    pub dismissed: bool,
    /// Column order, widths, visibility and pinning.
    pub layout: Option<LayoutSnapshot>,
}

/// A saved merged tab.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MergedDef {
    /// The merged files, in source order.
    pub paths: Vec<PathBuf>,
    /// The filter entries of the tab, in the query grammar of
    /// [`crate::filter`].
    pub filters: Vec<String>,
}

/// The pane layout: a tree of panes, and which pane each session tab is in.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PaneState {
    /// The split tree.
    pub tree: Option<PaneTree>,
    /// Pane of each tab of `Session::tabs`, by index.
    pub tab_panes: Vec<u32>,
    /// The focused pane.
    pub focused: u32,
    /// The active tab (index into `Session::tabs`) of each pane.
    pub pane_active: Vec<(u32, usize)>,
}

/// Everything in `gui-state.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiState {
    /// Format version.
    pub version: u32,
    /// Structure choices, most recently used first (keys are paths).
    pub structures: Vec<(String, SavedStructure)>,
    /// Pane layout.
    pub panes: Option<PaneState>,
    /// Merged tabs.
    pub merged: Vec<MergedDef>,
}

fn key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

impl GuiState {
    /// The remembered structure of `path`.
    pub fn structure(&self, path: &Path) -> Option<&SavedStructure> {
        let k = key(path);
        self.structures
            .iter()
            .find(|(p, _)| *p == k)
            .map(|(_, s)| s)
    }

    /// Remembers `saved` for `path` (moves it to the front).
    pub fn set_structure(&mut self, path: &Path, saved: SavedStructure) {
        let k = key(path);
        self.structures.retain(|(p, _)| *p != k);
        self.structures.insert(0, (k, saved));
        self.structures.truncate(MAX_REMEMBERED);
    }

    /// Parses the file contents; anything unreadable gives the defaults.
    pub fn parse(bytes: &[u8]) -> GuiState {
        let mut s: GuiState = serde_json::from_slice(bytes).unwrap_or_default();
        s.structures.truncate(MAX_REMEMBERED);
        s.version = GUI_STATE_VERSION;
        s
    }

    /// Serialises for writing.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_gives_defaults_and_round_trips() {
        assert_eq!(GuiState::parse(b"nope").structures.len(), 0);
        let mut s = GuiState::default();
        s.set_structure(
            Path::new("/a.log"),
            SavedStructure {
                spec: Some(ParserSpec::AccessCombined),
                table: true,
                ..SavedStructure::default()
            },
        );
        s.set_structure(Path::new("/b.log"), SavedStructure::default());
        s.merged.push(MergedDef {
            paths: vec!["/a.log".into(), "/b.log".into()],
            filters: vec!["ERROR".into(), "-health".into()],
        });
        let back = GuiState::parse(&s.to_bytes());
        assert_eq!(back.structures, s.structures);
        assert_eq!(back.merged, s.merged);
        assert!(back.structure(Path::new("/a.log")).unwrap().table);
        // Most recently set comes first; updating moves to the front.
        assert_eq!(back.structures[0].0, "/b.log");
    }

    #[test]
    fn the_list_is_bounded_and_unknown_fields_are_ignored() {
        let mut s = GuiState::default();
        for i in 0..(MAX_REMEMBERED + 20) {
            s.set_structure(Path::new(&format!("/f{i}.log")), SavedStructure::default());
        }
        assert_eq!(s.structures.len(), MAX_REMEMBERED);
        let s = GuiState::parse(br#"{"future": 1, "merged": [{"paths": ["/x"]}]}"#);
        assert_eq!(s.merged.len(), 1);
    }
}
