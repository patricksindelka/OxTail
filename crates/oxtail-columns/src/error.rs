//! Error types.

use thiserror::Error;

/// Errors from compiling a parser definition.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ColumnsError {
    /// A regular expression did not compile.
    #[error("invalid regular expression: {0}")]
    Regex(String),
    /// The parser definition is inconsistent (e.g. no columns).
    #[error("invalid parser definition: {0}")]
    InvalidSpec(String),
    /// A log4j/logback pattern could not be converted.
    #[error("invalid log pattern: {0}")]
    Pattern(String),
}

impl From<regex::Error> for ColumnsError {
    fn from(e: regex::Error) -> Self {
        // regex errors are multi-line with a caret diagram; keep the last line.
        let s = e.to_string();
        let msg = s
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("unknown error")
            .trim()
            .to_string();
        ColumnsError::Regex(msg)
    }
}
