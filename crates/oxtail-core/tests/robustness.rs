//! Regression tests for review findings: shutdown latency, spooled tails,
//! silent rewrites, encoding re-detection.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use oxtail_core::{Document, LineRequest, MemSource};

use common::*;

fn numbered(n: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..n {
        v.extend_from_slice(format!("line {i}\n").as_bytes());
    }
    v
}

#[test]
fn dropping_a_document_does_not_wait_for_queued_requests() {
    let data = numbered(300_000);
    let src = Arc::new(MemSource::new(data.clone()));
    let doc = Document::from_source(src, "busy");
    wait_ready(&doc, data.len() as u64);
    let first = doc.request_lines(LineRequest::Range {
        first: 0,
        count: 20_000,
    });
    for _ in 0..3000 {
        doc.request_lines(LineRequest::Range {
            first: 0,
            count: 20_000,
        });
    }
    // Once the first answer is out, the actor is draining the backlog.
    wait_lines(&doc, first);
    let t = Instant::now();
    drop(doc);
    let took = t.elapsed();
    assert!(took < Duration::from_millis(50), "drop took {took:?}");
}
