//! Requests to open files, from the command line, another instance (IPC),
//! drag and drop or the file dialog.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Asks the application to open some files in new tabs. This is also the
/// wire format between OxTail instances (one JSON document per connection).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenRequest {
    /// Files to open. Should be absolute: the receiving instance has a
    /// different working directory.
    pub files: Vec<PathBuf>,
    /// Open standard input as a tab (only meaningful inside the process that
    /// owns stdin; never sent to another instance).
    #[serde(skip)]
    pub stdin: bool,
    /// Start at the last `n` lines instead of following from the tail.
    pub tail_lines: Option<usize>,
    /// Profile to use for these tabs (by name).
    pub profile: Option<String>,
    /// A filter query to apply as soon as the tabs open.
    pub filter: Option<String>,
}

impl OpenRequest {
    /// A request for one file.
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self {
            files: vec![path.into()],
            ..Self::default()
        }
    }

    /// Whether the request opens nothing.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && !self.stdin
    }

    /// Makes every path absolute (relative to the current directory), so the
    /// request can be handed to another process.
    #[must_use]
    pub fn absolutized(mut self) -> Self {
        for f in &mut self.files {
            if let Ok(abs) = std::path::absolute(&*f) {
                *f = abs;
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip_drops_stdin() {
        let r = OpenRequest {
            files: vec!["/a/b.log".into()],
            stdin: true,
            tail_lines: Some(50),
            profile: Some("nginx".into()),
            filter: None,
        };
        let text = serde_json::to_string(&r).unwrap();
        let back: OpenRequest = serde_json::from_str(&text).unwrap();
        assert!(!back.stdin);
        assert_eq!(back.files, r.files);
        assert_eq!(back.tail_lines, Some(50));
        assert_eq!(back.profile.as_deref(), Some("nginx"));
    }

    #[test]
    fn partial_json_uses_defaults() {
        let r: OpenRequest = serde_json::from_str(r#"{"files":["x.log"]}"#).unwrap();
        assert_eq!(r.files.len(), 1);
        assert!(r.tail_lines.is_none());
        assert!(!r.is_empty());
        assert!(OpenRequest::default().is_empty());
    }

    #[test]
    fn absolutized_makes_relative_paths_absolute() {
        let r = OpenRequest::file("rel.log").absolutized();
        assert!(r.files[0].is_absolute());
    }
}
