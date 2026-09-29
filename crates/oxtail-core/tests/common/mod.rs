//! Shared helpers for the integration tests (polling with a 10 s cap, never bare sleeps).
#![allow(dead_code)]

use std::time::{Duration, Instant};

use oxtail_core::{DocEvent, Document, Line, LineRequest};

pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Polls `cond` every few milliseconds until it is true or 10 s pass.
pub fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for: {what}");
}

/// Waits until the document has indexed everything and reports `utf8_len` bytes.
pub fn wait_ready(doc: &Document, utf8_len: u64) {
    wait_until(&format!("ready with {utf8_len} bytes"), || {
        let s = doc.snapshot();
        s.utf8_len == utf8_len && s.lines.exact && s.indexed_bytes == utf8_len
    });
}

/// Drains events until `pred` matches one; returns it. Panics after 10 s.
pub fn wait_event(doc: &Document, what: &str, mut pred: impl FnMut(&DocEvent) -> bool) -> DocEvent {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match doc.events().recv_timeout(left) {
            Ok(ev) if pred(&ev) => return ev,
            Ok(_) => {}
            Err(_) => panic!("timed out waiting for event: {what}"),
        }
    }
}

/// Sends a request and waits for its `Lines` answer.
pub fn request(doc: &Document, req: LineRequest) -> Vec<Line> {
    let id = doc.request_lines(req);
    match wait_event(
        doc,
        "lines answer",
        |e| matches!(e, DocEvent::Lines { id: i, .. } if *i == id),
    ) {
        DocEvent::Lines { lines, .. } => lines,
        _ => unreachable!(),
    }
}

/// The texts of `lines`.
pub fn texts(lines: &[Line]) -> Vec<&str> {
    lines.iter().map(|l| l.text.as_str()).collect()
}
