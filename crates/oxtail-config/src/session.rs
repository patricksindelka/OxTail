//! `session.json`: what to restore at the next start.
//!
//! In memory all paths are absolute. On disk, paths on the same volume as the
//! executable are stored relative to the executable's directory with an
//! `@exe/` prefix, so a USB stick that gets another drive letter (or mount
//! point) still restores correctly. See [`PathMapper`] for the exact rule.
//!
//! Loading is tolerant: a missing, truncated or garbage file yields an empty
//! session, and individual bad tabs or bookmarks are skipped.

use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ConfigError, write_atomic};

/// Prefix marking an executable-relative stored path.
const REL_PREFIX: &str = "@exe/";
const MAX_TABS: usize = 256;
const MAX_BOOKMARKS_PER_FILE: usize = 10_000;
const MAX_RECENT: usize = 500;
const MAX_FILES_WITH_BOOKMARKS: usize = 5_000;

/// One open tab.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TabState {
    /// The file (absolute in memory).
    pub path: PathBuf,
    /// Encoding chosen by the user, if any.
    pub encoding: Option<String>,
    /// Profile chosen by the user (by name), if any.
    pub profile: Option<String>,
    /// 0-based line at the top of the view.
    pub scroll_anchor_line: u64,
    /// Whether the tab was following the file.
    pub follow: bool,
    /// Stacked filters as query strings, outermost first.
    pub filters: Vec<String>,
    /// The last search query.
    pub search_query: Option<String>,
}

/// Direction of a split.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDirection {
    /// Panes side by side.
    #[default]
    Horizontal,
    /// Panes stacked.
    Vertical,
}

/// The split layout tree; leaves refer to tabs by index into
/// [`Session::tabs`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum LayoutNode {
    /// A pane showing one tab.
    Leaf {
        /// Index into `Session::tabs`.
        tab: usize,
    },
    /// Two panes.
    Split {
        /// Split direction.
        direction: SplitDirection,
        /// Fraction (0..1) of the space given to `first`.
        ratio: f32,
        /// First pane.
        first: Box<LayoutNode>,
        /// Second pane.
        second: Box<LayoutNode>,
    },
}

impl LayoutNode {
    fn depth_ok(&self, max_tab: usize, budget: &mut usize) -> bool {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        match self {
            LayoutNode::Leaf { tab } => *tab < max_tab,
            LayoutNode::Split {
                first,
                second,
                ratio,
                ..
            } => {
                ratio.is_finite()
                    && first.depth_ok(max_tab, budget)
                    && second.depth_ok(max_tab, budget)
            }
        }
    }
}

/// A bookmarked line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bookmark {
    /// 0-based line number.
    pub line: u64,
    /// User label.
    pub label: String,
}

/// Window position and size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowGeometry {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
    /// Maximised.
    pub maximized: bool,
}

/// The complete restorable state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// Session format version.
    pub version: u32,
    /// Open tabs in order.
    pub tabs: Vec<TabState>,
    /// Index of the active tab.
    pub active_tab: usize,
    /// Split layout, if any.
    pub layout: Option<LayoutNode>,
    /// Bookmarks per file (absolute path in memory).
    pub bookmarks: BTreeMap<PathBuf, Vec<Bookmark>>,
    /// Recently opened files, most recent first.
    pub recent_files: Vec<PathBuf>,
    /// Window geometry.
    pub window: Option<WindowGeometry>,
}

/// Current session format version.
pub const SESSION_VERSION: u32 = 1;

/// Converts paths between absolute (in memory) and executable-relative
/// (on disk).
///
/// A path is stored relative when it is on the **same volume** as the
/// executable:
///
/// * Windows: same drive letter, or same UNC server and share.
/// * Unix: there are no drive letters, so the heuristic is that the path lies
///   under the executable directory's *parent* (a USB stick holding
///   `stick/oxtail/oxtail` and `stick/logs/x.log`); if the parent has fewer
///   than two components (e.g. `/opt`) the executable directory itself is
///   used, so that `/` is never treated as "the same volume".
///
/// Everything else stays absolute. Resolution back is purely lexical
/// (`..` is folded without touching the file system).
#[derive(Clone, Debug)]
pub struct PathMapper {
    exe_dir: PathBuf,
}

impl PathMapper {
    /// A mapper for an executable located at `exe_path`.
    pub fn for_exe(exe_path: &Path) -> Self {
        Self {
            exe_dir: exe_path.parent().map(Path::to_path_buf).unwrap_or_default(),
        }
    }

    /// A mapper for the running executable.
    pub fn current() -> Self {
        Self::for_exe(&std::env::current_exe().unwrap_or_default())
    }

    /// A mapper that never relativises (in-memory mode / tests).
    pub fn absolute_only() -> Self {
        Self {
            exe_dir: PathBuf::new(),
        }
    }

    fn base(&self) -> Option<PathBuf> {
        if !self.exe_dir.is_absolute() {
            return None;
        }
        if cfg!(windows) {
            return Some(self.exe_dir.clone());
        }
        let parent = self.exe_dir.parent()?;
        let normals = parent
            .components()
            .filter(|c| matches!(c, Component::Normal(_)))
            .count();
        Some(if normals >= 2 {
            parent.to_path_buf()
        } else {
            self.exe_dir.clone()
        })
    }

    /// The string to store for `path`.
    pub fn to_stored(&self, path: &Path) -> String {
        if path.is_absolute()
            && let Some(rel) = self.relativize(path)
        {
            return format!("{REL_PREFIX}{rel}");
        }
        path.to_string_lossy().into_owned()
    }

    /// The absolute path for a stored string.
    pub fn from_stored(&self, stored: &str) -> PathBuf {
        match stored.strip_prefix(REL_PREFIX) {
            Some(rel) if self.exe_dir.is_absolute() => {
                let mut out = self.exe_dir.clone();
                for part in rel.split('/') {
                    match part {
                        "" | "." => {}
                        ".." => {
                            // Do not climb above the root/prefix.
                            let has_parent = out.parent().is_some();
                            if has_parent {
                                out.pop();
                            }
                        }
                        p => out.push(p),
                    }
                }
                out
            }
            // Relative marker but no executable directory: keep as a relative path.
            Some(rel) => PathBuf::from(rel),
            None => PathBuf::from(stored),
        }
    }

    fn relativize(&self, path: &Path) -> Option<String> {
        let base = self.base()?;
        let (pk, ek) = (volume_key(path), volume_key(&self.exe_dir));
        if pk != ek {
            return None;
        }
        if !cfg!(windows) && !path.starts_with(&base) {
            return None;
        }
        let pc: Vec<Component> = normal_components(path);
        let ec: Vec<Component> = normal_components(&self.exe_dir);
        let same = |a: &Component, b: &Component| {
            if cfg!(windows) {
                a.as_os_str().to_string_lossy().to_lowercase()
                    == b.as_os_str().to_string_lossy().to_lowercase()
            } else {
                a == b
            }
        };
        let common = pc.iter().zip(&ec).take_while(|(a, b)| same(a, b)).count();
        let mut parts: Vec<String> = vec!["..".into(); ec.len() - common];
        parts.extend(
            pc[common..]
                .iter()
                .map(|c| c.as_os_str().to_string_lossy().into_owned()),
        );
        if parts.is_empty() {
            parts.push(".".into());
        }
        Some(parts.join("/"))
    }
}

/// Components after the root/prefix, with `.` removed.
fn normal_components(p: &Path) -> Vec<Component<'_>> {
    p.components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .collect()
}

/// Identifies the volume of an absolute path: drive or UNC share on Windows,
/// always `None` (one namespace) on Unix.
fn volume_key(p: &Path) -> Option<String> {
    match p.components().next() {
        Some(Component::Prefix(pre)) => Some(pre.as_os_str().to_string_lossy().to_lowercase()),
        _ => None,
    }
}

impl Session {
    /// An empty session.
    pub fn new() -> Self {
        Self {
            version: SESSION_VERSION,
            ..Self::default()
        }
    }

    /// Serialises with paths converted through `mapper`.
    pub fn to_json(&self, mapper: &PathMapper) -> Result<String, ConfigError> {
        let map = |p: &Path| mapper.to_stored(p);
        let mut v = serde_json::to_value(self)?;
        // Rewrite the path-carrying fields.
        if let Some(tabs) = v.get_mut("tabs").and_then(Value::as_array_mut) {
            for (t, orig) in tabs.iter_mut().zip(&self.tabs) {
                t["path"] = Value::String(map(&orig.path));
            }
        }
        v["bookmarks"] = Value::Object(
            self.bookmarks
                .iter()
                .map(|(p, b)| Ok((map(p), serde_json::to_value(b)?)))
                .collect::<Result<_, serde_json::Error>>()?,
        );
        v["recent_files"] = Value::Array(
            self.recent_files
                .iter()
                .map(|p| Value::String(map(p)))
                .collect(),
        );
        Ok(serde_json::to_string_pretty(&v)?)
    }

    /// Parses leniently: anything unusable is dropped, never an error.
    pub fn from_json_lenient(text: &str, mapper: &PathMapper) -> Session {
        let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(text) else {
            return Session::new();
        };
        let mut s = Session::new();
        if let Some(Value::Array(tabs)) = obj.get("tabs") {
            for t in tabs.iter().take(MAX_TABS) {
                if let Ok(mut tab) = serde_json::from_value::<TabState>(t.clone()) {
                    let stored = t.get("path").and_then(Value::as_str).unwrap_or_default();
                    if stored.is_empty() {
                        continue;
                    }
                    tab.path = mapper.from_stored(stored);
                    s.tabs.push(tab);
                }
            }
        }
        s.active_tab = obj
            .get("active_tab")
            .and_then(Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .filter(|&i| i < s.tabs.len())
            .unwrap_or(0);
        s.layout = obj
            .get("layout")
            .and_then(|l| serde_json::from_value::<LayoutNode>(l.clone()).ok())
            .filter(|l| l.depth_ok(s.tabs.len(), &mut 256));
        if let Some(Value::Object(b)) = obj.get("bookmarks") {
            for (path, list) in b.iter().take(MAX_FILES_WITH_BOOKMARKS) {
                if let Ok(mut list) = serde_json::from_value::<Vec<Bookmark>>(list.clone()) {
                    list.truncate(MAX_BOOKMARKS_PER_FILE);
                    s.bookmarks.insert(mapper.from_stored(path), list);
                }
            }
        }
        if let Some(Value::Array(r)) = obj.get("recent_files") {
            s.recent_files = r
                .iter()
                .filter_map(Value::as_str)
                .take(MAX_RECENT)
                .map(|p| mapper.from_stored(p))
                .collect();
        }
        s.window = obj
            .get("window")
            .and_then(|w| serde_json::from_value::<WindowGeometry>(w.clone()).ok())
            .filter(|w| {
                [w.x, w.y, w.width, w.height].iter().all(|v| v.is_finite())
                    && w.width > 0.0
                    && w.height > 0.0
            });
        s
    }

    /// Loads `path`; never fails (missing or broken file gives an empty
    /// session).
    pub fn load(path: &Path, mapper: &PathMapper) -> Session {
        match fs::read(path) {
            Ok(bytes) => Self::from_json_lenient(&String::from_utf8_lossy(&bytes), mapper),
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("cannot read session {}: {e}", path.display());
                }
                Session::new()
            }
        }
    }

    /// Saves atomically.
    pub fn save(&self, path: &Path, mapper: &PathMapper) -> Result<(), ConfigError> {
        let text = self.to_json(mapper)?;
        write_atomic(path, text.as_bytes()).map_err(|e| ConfigError::io(path, e))
    }

    /// Adds `path` to the recent list (most recent first, no duplicates,
    /// truncated to `limit`).
    pub fn add_recent(&mut self, path: &Path, limit: usize) {
        self.recent_files.retain(|p| p != path);
        self.recent_files.insert(0, path.to_path_buf());
        self.recent_files.truncate(limit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapper() -> PathMapper {
        PathMapper::for_exe(Path::new("/media/usb/oxtail/oxtail"))
    }

    fn sample() -> Session {
        let mut s = Session::new();
        s.tabs.push(TabState {
            path: "/media/usb/logs/app.log".into(),
            encoding: Some("utf-16le".into()),
            profile: Some("Nginx access".into()),
            scroll_anchor_line: 1234,
            follow: true,
            filters: vec!["level == \"error\"".into(), "timeout".into()],
            search_query: Some("foo".into()),
        });
        s.tabs.push(TabState {
            path: "/var/log/syslog".into(),
            ..TabState::default()
        });
        s.active_tab = 1;
        s.layout = Some(LayoutNode::Split {
            direction: SplitDirection::Vertical,
            ratio: 0.4,
            first: Box::new(LayoutNode::Leaf { tab: 0 }),
            second: Box::new(LayoutNode::Leaf { tab: 1 }),
        });
        s.bookmarks.insert(
            "/media/usb/oxtail/notes.log".into(),
            vec![Bookmark {
                line: 7,
                label: "start".into(),
            }],
        );
        s.recent_files = vec!["/media/usb/logs/app.log".into(), "/tmp/x.log".into()];
        s.window = Some(WindowGeometry {
            x: 10.0,
            y: 20.0,
            width: 800.0,
            height: 600.0,
            maximized: false,
        });
        s
    }

    #[test]
    fn relative_storage_and_round_trip() {
        let s = sample();
        let json = s.to_json(&mapper()).expect("json");
        assert!(json.contains("@exe/../logs/app.log"), "{json}");
        assert!(json.contains("@exe/notes.log"), "{json}");
        assert!(
            json.contains("\"/var/log/syslog\""),
            "outside the stick stays absolute: {json}"
        );
        assert!(
            !json.contains("/media/usb"),
            "no absolute stick path leaks: {json}"
        );
        let back = Session::from_json_lenient(&json, &mapper());
        assert_eq!(back, s);
    }

    #[test]
    fn stick_remounted_elsewhere_restores() {
        let json = sample().to_json(&mapper()).expect("json");
        let moved = PathMapper::for_exe(Path::new("/mnt/other/oxtail/oxtail"));
        let back = Session::from_json_lenient(&json, &moved);
        assert_eq!(back.tabs[0].path, PathBuf::from("/mnt/other/logs/app.log"));
        assert_eq!(back.tabs[1].path, PathBuf::from("/var/log/syslog"));
        assert!(
            back.bookmarks
                .contains_key(Path::new("/mnt/other/oxtail/notes.log"))
        );
    }

    #[test]
    fn mapper_cases() {
        let m = mapper();
        let cases = [
            ("/media/usb/oxtail/a.log", "@exe/a.log"),
            ("/media/usb/logs/a.log", "@exe/../logs/a.log"),
            ("/media/usb/oxtail/sub/deep/a.log", "@exe/sub/deep/a.log"),
            ("/media/other/a.log", "/media/other/a.log"),
            ("/etc/passwd", "/etc/passwd"),
            ("relative/x.log", "relative/x.log"),
        ];
        for (p, want) in cases {
            let stored = m.to_stored(Path::new(p));
            assert_eq!(stored, want, "{p}");
            assert_eq!(m.from_stored(&stored), PathBuf::from(p), "{p}");
        }
        // Shallow install: `/opt/oxtail/oxtail` does not treat all of `/opt` as the volume.
        let m = PathMapper::for_exe(Path::new("/opt/oxtail/oxtail"));
        assert_eq!(
            m.to_stored(Path::new("/opt/other/x.log")),
            "/opt/other/x.log"
        );
        assert_eq!(m.to_stored(Path::new("/opt/oxtail/x.log")), "@exe/x.log");
        let m = PathMapper::absolute_only();
        assert_eq!(m.to_stored(Path::new("/a/b")), "/a/b");
    }

    #[test]
    fn dotdot_cannot_escape_root() {
        let m = PathMapper::for_exe(Path::new("/a/b/exe"));
        assert_eq!(m.from_stored("@exe/../../../../../x"), PathBuf::from("/x"));
    }

    #[test]
    fn tolerant_loading() {
        let m = mapper();
        for junk in [
            "",
            "null",
            "[]",
            "42",
            "{",
            "\u{0}",
            "{\"tabs\": 5}",
            "{\"tabs\": [1, \"x\", null, {}]}",
            "{\"tabs\": [{\"path\": 5}]}",
            "{\"layout\": {\"kind\": \"nope\"}}",
            "{\"bookmarks\": {\"a\": \"nope\"}}",
            "{\"window\": {\"width\": -1}}",
        ] {
            let s = Session::from_json_lenient(junk, &m);
            assert!(s.tabs.len() <= 1, "{junk}");
        }
        let s = Session::from_json_lenient(
            r#"{"tabs":[{"path":"/a.log","follow":true,"scroll_anchor_line":"bad"},{"path":"/b.log","follow":true}],
                "active_tab": 9, "layout": {"kind":"leaf","tab":5}}"#,
            &m,
        );
        // The first tab has a wrongly typed field and is skipped, the second survives.
        assert_eq!(s.tabs.len(), 1);
        assert_eq!(s.active_tab, 0);
        assert!(s.layout.is_none(), "leaf refers to a missing tab");
    }

    #[test]
    fn file_round_trip_and_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("session.json");
        let m = PathMapper::absolute_only();
        assert_eq!(Session::load(&p, &m), Session::new());
        let s = sample();
        s.save(&p, &m).expect("save");
        assert_eq!(Session::load(&p, &m), s);
        fs::write(&p, [0xff, 0xfe, b'{']).expect("w");
        assert_eq!(Session::load(&p, &m), Session::new());
    }

    #[test]
    fn recent_files() {
        let mut s = Session::new();
        for n in ["a", "b", "c", "a"] {
            s.add_recent(Path::new(n), 3);
        }
        assert_eq!(
            s.recent_files,
            vec![PathBuf::from("a"), "c".into(), "b".into()]
        );
    }

    mod prop {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn garbage_never_panics(s in "\\PC{0,300}") {
                let _ = Session::from_json_lenient(&s, &mapper());
            }

            #[test]
            fn path_round_trip(parts in proptest::collection::vec("[a-z]{1,4}", 1..6)) {
                let m = mapper();
                let p: PathBuf = std::iter::once("/media/usb".to_string()).chain(parts).collect();
                prop_assert_eq!(m.from_stored(&m.to_stored(&p)), p);
            }
        }
    }
}
