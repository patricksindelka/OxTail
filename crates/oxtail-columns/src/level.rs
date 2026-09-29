//! Log level normalisation.

/// A normalised log level, ordered by severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// Trace / verbose.
    Trace,
    /// Debug.
    Debug,
    /// Info / notice.
    Info,
    /// Warn / warning.
    Warn,
    /// Error / severe.
    Error,
    /// Fatal / critical / emergency / panic.
    Fatal,
}

impl Level {
    /// Normalises many spellings: `W`, `warn`, `WARNING` all give [`Level::Warn`].
    ///
    /// Surrounding whitespace and brackets are ignored. Returns `None` for
    /// anything that is not a known level name.
    pub fn parse(s: &str) -> Option<Level> {
        let s = s.trim().trim_matches(|c| c == '[' || c == ']').trim();
        if s.is_empty() || s.len() > 12 {
            return None;
        }
        let mut buf = [0u8; 12];
        for (d, b) in buf.iter_mut().zip(s.bytes()) {
            *d = b.to_ascii_lowercase();
        }
        let l = std::str::from_utf8(&buf[..s.len()]).ok()?;
        Some(match l {
            "t" | "trace" | "trc" | "verbose" | "finest" | "finer" | "v" => Level::Trace,
            "d" | "debug" | "dbg" | "fine" => Level::Debug,
            "i" | "info" | "inf" | "information" | "informational" | "notice" | "config" => {
                Level::Info
            }
            "w" | "warn" | "warning" | "wrn" => Level::Warn,
            "e" | "err" | "error" | "severe" => Level::Error,
            "f" | "fatal" | "crit" | "critical" | "emerg" | "emergency" | "alert" | "panic"
            | "c" => Level::Fatal,
            _ => return None,
        })
    }

    /// Canonical upper-case label (`"WARN"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
            Level::Fatal => "FATAL",
        }
    }
}

/// Convenience wrapper for [`Level::parse`].
pub fn normalize_level(s: &str) -> Option<Level> {
    Level::parse(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spellings() {
        for s in ["W", "warn", "WARNING", " [Warn] "] {
            assert_eq!(Level::parse(s), Some(Level::Warn), "{s}");
        }
        assert_eq!(Level::parse("err"), Some(Level::Error));
        assert_eq!(Level::parse("CRITICAL"), Some(Level::Fatal));
        assert_eq!(Level::parse("hello"), None);
        assert_eq!(Level::parse(""), None);
        assert!(Level::Warn < Level::Error);
    }
}
