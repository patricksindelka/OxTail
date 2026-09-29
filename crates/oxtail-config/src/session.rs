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

use crate::{ConfigError, DataMode, write_atomic};

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
    /// Rewrites tab indices through `remap`; leaves of removed tabs vanish and
    /// their split collapses to the sibling.
    fn remap(self, remap: &[Option<usize>]) -> Option<LayoutNode> {
        match self {
            LayoutNode::Leaf { tab } => remap
                .get(tab)
                .copied()
                .flatten()
                .map(|tab| LayoutNode::Leaf { tab }),
            LayoutNode::Split {
                direction,
                ratio,
                first,
                second,
            } => match (first.remap(remap), second.remap(remap)) {
                (Some(first), Some(second)) => Some(LayoutNode::Split {
                    direction,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (a, b) => a.or(b),
            },
        }
    }

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
    /// Whether [`PathMapper::to_stored`] may write `@exe/` paths. Resolving
    /// existing `@exe/` entries works either way.
    relativise: bool,
}

impl PathMapper {
    /// A mapper for an executable located at `exe_path`.
    pub fn for_exe(exe_path: &Path) -> Self {
        Self {
            exe_dir: exe_path.parent().map(Path::to_path_buf).unwrap_or_default(),
            relativise: true,
        }
    }

    /// A mapper for the given data-folder mode. Only [`DataMode::Portable`]
    /// stores executable-relative paths: in any other mode the session file
    /// may be shared by installs at different locations (e.g. two copies
    /// using one `%APPDATA%\OxTail\session.json`), where `@exe/../..` would
    /// resolve differently. Existing `@exe/` entries are still understood.
    pub fn for_mode(mode: &DataMode, exe_path: &Path) -> Self {
        let mut m = Self::for_exe(exe_path);
        m.relativise = matches!(mode, DataMode::Portable);
        m
    }

    /// A mapper for the running executable.
    pub fn current() -> Self {
        Self::for_exe(&std::env::current_exe().unwrap_or_default())
    }

    /// A mapper that never relativises (in-memory mode / tests).
    pub fn absolute_only() -> Self {
        Self {
            exe_dir: PathBuf::new(),
            relativise: false,
        }
    }

    fn base(&self) -> Option<PathBuf> {
        if !self.relativise || !self.exe_dir.is_absolute() {
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
    ///
    /// Paths that are not valid UTF-8 cannot be stored in JSON without
    /// changing them, so they are **skipped** with a warning: tabs with such a
    /// path are dropped (the layout and active tab are remapped), bookmarks
    /// and recent-file entries are dropped. One odd file name never blocks
    /// saving the rest of the session.
    ///
    /// Entries stored executable-relative also get their absolute path in a
    /// top-level `absolute_paths` object (stored string to absolute path);
    /// loading prefers it when that path exists.
    pub fn to_json(&self, mapper: &PathMapper) -> Result<String, ConfigError> {
        let clean = self.without_non_utf8_paths();
        let mut absolute = serde_json::Map::new();
        let mut map = |p: &Path| {
            let stored = mapper.to_stored(p);
            if stored.starts_with(REL_PREFIX)
                && let Some(abs) = p.to_str()
            {
                absolute.insert(stored.clone(), Value::String(abs.to_string()));
            }
            stored
        };
        let mut v = serde_json::to_value(&clean)?;
        // Rewrite the path-carrying fields.
        if let Some(tabs) = v.get_mut("tabs").and_then(Value::as_array_mut) {
            for (t, orig) in tabs.iter_mut().zip(&clean.tabs) {
                t["path"] = Value::String(map(&orig.path));
            }
        }
        v["bookmarks"] = Value::Object(
            clean
                .bookmarks
                .iter()
                .map(|(p, b)| Ok((map(p), serde_json::to_value(b)?)))
                .collect::<Result<_, serde_json::Error>>()?,
        );
        v["recent_files"] = Value::Array(
            clean
                .recent_files
                .iter()
                .map(|p| Value::String(map(p)))
                .collect(),
        );
        if !absolute.is_empty() {
            v["absolute_paths"] = Value::Object(absolute);
        }
        Ok(serde_json::to_string_pretty(&v)?)
    }

    /// A copy without any path that is not valid UTF-8.
    fn without_non_utf8_paths(&self) -> Session {
        let mut s = self.clone();
        let mut remap: Vec<Option<usize>> = Vec::with_capacity(s.tabs.len());
        let mut kept = Vec::new();
        for t in s.tabs.drain(..) {
            if t.path.to_str().is_some() {
                remap.push(Some(kept.len()));
                kept.push(t);
            } else {
                tracing::warn!("session: skipping tab with non-UTF-8 path {:?}", t.path);
                remap.push(None);
            }
        }
        s.tabs = kept;
        s.active_tab = remap.get(s.active_tab).copied().flatten().unwrap_or(0);
        s.layout = s.layout.take().and_then(|l| l.remap(&remap));
        s.bookmarks.retain(|p, _| {
            let ok = p.to_str().is_some();
            if !ok {
                tracing::warn!("session: skipping bookmarks of non-UTF-8 path {p:?}");
            }
            ok
        });
        s.recent_files.retain(|p| {
            let ok = p.to_str().is_some();
            if !ok {
                tracing::warn!("session: skipping recent file with non-UTF-8 path {p:?}");
            }
            ok
        });
        s
    }

    /// Parses leniently: anything unusable is dropped, never an error.
    pub fn from_json_lenient(text: &str, mapper: &PathMapper) -> Session {
        let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(text) else {
            return Session::new();
        };
        let mut s = Session::new();
        let absolute: BTreeMap<&str, &str> = obj
            .get("absolute_paths")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.as_str(), v.as_str()?)))
                    .collect()
            })
            .unwrap_or_default();
        // Prefer the stored absolute path when it still exists.
        let resolve = |stored: &str| -> PathBuf {
            if let Some(abs) = absolute.get(stored) {
                let abs = Path::new(abs);
                if abs.is_absolute() && abs.exists() {
                    return abs.to_path_buf();
                }
            }
            mapper.from_stored(stored)
        };
        if let Some(Value::Array(tabs)) = obj.get("tabs") {
            for t in tabs.iter().take(MAX_TABS) {
                if let Ok(mut tab) = serde_json::from_value::<TabState>(t.clone()) {
                    let stored = t.get("path").and_then(Value::as_str).unwrap_or_default();
                    if stored.is_empty() {
                        continue;
                    }
                    tab.path = resolve(stored);
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
                    s.bookmarks.insert(resolve(path), list);
                }
            }
        }
        if let Some(Value::Array(r)) = obj.get("recent_files") {
            s.recent_files = r
                .iter()
                .filter_map(Value::as_str)
                .take(MAX_RECENT)
                .map(resolve)
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

    // Unix path semantics (`/media/usb` is not absolute on Windows); see
    // `windows_paths` below for the drive-letter equivalents.
    #[cfg(unix)]
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
        assert!(json.contains("absolute_paths"), "{json}");
        let back = Session::from_json_lenient(&json, &mapper());
        assert_eq!(back, s);
    }

    // Unix path semantics (`/media/usb` is not absolute on Windows); see
    // `windows_paths` below for the drive-letter equivalents.
    #[cfg(unix)]
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

    // Unix path semantics (`/media/usb` is not absolute on Windows); see
    // `windows_paths` below for the drive-letter equivalents.
    #[cfg(unix)]
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

    // Unix path semantics (`/media/usb` is not absolute on Windows); see
    // `windows_paths` below for the drive-letter equivalents.
    #[cfg(unix)]
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

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_are_skipped_not_fatal() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let bad = PathBuf::from(OsStr::from_bytes(b"/var/log/\xff\xfe.log"));
        let mut s = Session::new();
        s.tabs.push(TabState {
            path: "/var/log/a.log".into(),
            ..TabState::default()
        });
        s.tabs.push(TabState {
            path: bad.clone(),
            ..TabState::default()
        });
        s.tabs.push(TabState {
            path: "/var/log/c.log".into(),
            ..TabState::default()
        });
        s.active_tab = 2;
        s.layout = Some(LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            ratio: 0.5,
            first: Box::new(LayoutNode::Leaf { tab: 1 }),
            second: Box::new(LayoutNode::Leaf { tab: 2 }),
        });
        s.bookmarks.insert(bad.clone(), vec![Bookmark::default()]);
        s.recent_files = vec![bad, "/var/log/a.log".into()];
        let m = PathMapper::absolute_only();
        let json = s.to_json(&m).expect("must not fail");
        let back = Session::from_json_lenient(&json, &m);
        let paths: Vec<_> = back.tabs.iter().map(|t| t.path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/var/log/a.log"),
                PathBuf::from("/var/log/c.log")
            ]
        );
        assert_eq!(back.active_tab, 1);
        assert_eq!(back.layout, Some(LayoutNode::Leaf { tab: 1 }));
        assert!(back.bookmarks.is_empty());
        assert_eq!(back.recent_files, vec![PathBuf::from("/var/log/a.log")]);
    }

    // Unix path semantics (`/media/usb` is not absolute on Windows); see
    // `windows_paths` below for the drive-letter equivalents.
    #[cfg(unix)]
    #[test]
    fn installed_mode_never_relativises() {
        let exe = Path::new("/media/usb/oxtail/oxtail");
        let m = PathMapper::for_mode(&DataMode::Installed, exe);
        assert_eq!(
            m.to_stored(Path::new("/media/usb/logs/x.log")),
            "/media/usb/logs/x.log"
        );
        let m = PathMapper::for_mode(&DataMode::Cli, exe);
        assert_eq!(
            m.to_stored(Path::new("/media/usb/logs/x.log")),
            "/media/usb/logs/x.log"
        );
        let m = PathMapper::for_mode(&DataMode::Portable, exe);
        assert_eq!(
            m.to_stored(Path::new("/media/usb/logs/x.log")),
            "@exe/../logs/x.log"
        );
    }

    #[test]
    fn absolute_path_preferred_when_it_exists() {
        let t = tempfile::tempdir().expect("tempdir");
        let stick = t.path().join("stick");
        let exe = stick.join("oxtail/oxtail");
        let logs = stick.join("logs");
        fs::create_dir_all(&logs).expect("mkdir");
        let file = logs.join("x.log");
        fs::write(&file, "x").expect("w");
        let mut s = Session::new();
        s.tabs.push(TabState {
            path: file.clone(),
            ..TabState::default()
        });
        s.recent_files = vec![file.clone()];
        let portable = PathMapper::for_mode(&DataMode::Portable, &exe);
        let json = s.to_json(&portable).expect("json");
        assert!(json.contains("@exe/"), "{json}");
        // Same session file read by an install at another depth: the absolute
        // path (which exists) wins over the mis-resolving relative one.
        let other = PathMapper::for_exe(&t.path().join("deep/er/oxtail"));
        let back = Session::from_json_lenient(&json, &other);
        assert_eq!(back.tabs[0].path, file);
        assert_eq!(back.recent_files, vec![file]);
        // When the absolute path is gone the relative form is used.
        let json = json.replace(&t.path().to_string_lossy().to_string(), "/nonexistent");
        let back = Session::from_json_lenient(&json, &portable);
        assert_eq!(back.tabs[0].path, logs.join("x.log"));
    }

    /// Drive-letter and UNC equivalents of the Unix-only mapper tests.
    #[cfg(windows)]
    mod windows_paths {
        use super::*;

        fn exe() -> &'static Path {
            Path::new(r"C:\tools\oxtail\oxtail.exe")
        }

        #[test]
        fn same_drive_is_relative_other_drive_absolute() {
            let m = PathMapper::for_exe(exe());
            assert_eq!(
                m.to_stored(Path::new(r"C:\tools\oxtail\a.log")),
                "@exe/a.log"
            );
            assert_eq!(
                m.to_stored(Path::new(r"C:\tools\oxtail\sub\a.log")),
                "@exe/sub/a.log"
            );
            assert_eq!(
                m.to_stored(Path::new(r"C:\logs\a.log")),
                "@exe/../../logs/a.log"
            );
            assert_eq!(m.to_stored(Path::new(r"D:\logs\a.log")), r"D:\logs\a.log");
            assert_eq!(
                m.to_stored(Path::new(r"\\server\share\a.log")),
                r"\\server\share\a.log"
            );
        }

        #[test]
        fn round_trip_and_remount_on_another_drive() {
            let m = PathMapper::for_exe(exe());
            let stored = m.to_stored(Path::new(r"C:\logs\a.log"));
            assert_eq!(m.from_stored(&stored), PathBuf::from(r"C:\logs\a.log"));
            // The stick shows up as E: next time.
            let moved = PathMapper::for_exe(Path::new(r"E:\tools\oxtail\oxtail.exe"));
            assert_eq!(moved.from_stored(&stored), PathBuf::from(r"E:\logs\a.log"));
        }

        #[test]
        fn dotdot_stops_at_the_drive_root() {
            let m = PathMapper::for_exe(Path::new(r"C:\a\b\oxtail.exe"));
            assert_eq!(
                m.from_stored("@exe/../../../../../x"),
                PathBuf::from(r"C:\x")
            );
        }

        #[test]
        fn installed_mode_never_relativises() {
            let p = Path::new(r"C:\logs\x.log");
            for mode in [DataMode::Installed, DataMode::Cli] {
                assert_eq!(
                    PathMapper::for_mode(&mode, exe()).to_stored(p),
                    r"C:\logs\x.log"
                );
            }
            assert_eq!(
                PathMapper::for_mode(&DataMode::Portable, exe()).to_stored(p),
                "@exe/../../logs/x.log"
            );
        }
    }
}
