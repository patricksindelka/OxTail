//! Follow tests on real temp files: append, truncate, rotate, delete.
//! Every wait polls for the expected state with a 10 s cap.

mod common;

use std::fs::{self, OpenOptions as FsOpen};
use std::io::Write;
use std::path::Path;

use common::*;
use oxtail_core::{
    DocEvent, DocState, Document, EncodingChoice, FollowMode, LineRequest, NoticeKind, OpenOptions,
};

fn append(path: &Path, bytes: &[u8]) {
    let mut f = FsOpen::new().append(true).open(path).unwrap();
    f.write_all(bytes).unwrap();
}

fn opts() -> OpenOptions {
    OpenOptions {
        index_spacing: 128,
        ..OpenOptions::default()
    }
}

#[test]
fn open_missing_file_and_directory_fail_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    assert!(Document::open(dir.path().join("nope.log"), opts()).is_err());
    assert!(Document::open(dir.path(), opts()).is_err());
}

#[test]
fn follows_appends() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    fs::write(&path, b"one\ntwo\n").unwrap();
    let doc = Document::open(&path, opts()).unwrap();
    assert_eq!(doc.snapshot().display_name, "app.log");
    wait_ready(&doc, 8);
    append(&path, b"three\nfour\n");
    wait_event(&doc, "Grew to 19", |e| {
        matches!(e, DocEvent::Grew { utf8_len: 19 })
    });
    wait_ready(&doc, 19);
    let t = request(&doc, LineRequest::Tail { count: 2 });
    assert_eq!(texts(&t), ["three", "four"]);
    assert_eq!(doc.snapshot().lines.known, 4);
    assert_eq!(doc.snapshot().generation, 0);
}

#[test]
fn detects_truncation_and_keeps_following() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.log");
    fs::write(&path, "old line\n".repeat(100)).unwrap();
    let doc = Document::open(&path, opts()).unwrap();
    wait_ready(&doc, 900);
    // Truncate in place and write something shorter.
    fs::write(&path, b"new 1\n").unwrap();
    let ev = wait_event(&doc, "Truncated", |e| {
        matches!(e, DocEvent::Truncated { .. })
    });
    let DocEvent::Truncated { generation, .. } = ev else {
        unreachable!()
    };
    assert_eq!(generation, 1);
    wait_ready(&doc, 6);
    assert_eq!(
        doc.snapshot().last_event.unwrap().kind,
        NoticeKind::Truncated
    );
    append(&path, b"new 2\n");
    wait_ready(&doc, 12);
    let r = request(
        &doc,
        LineRequest::Range {
            first: 0,
            count: 10,
        },
    );
    assert_eq!(texts(&r), ["new 1", "new 2"]);
    assert_eq!(doc.snapshot().generation, 1);
}

#[test]
fn rotation_by_name_reopens_the_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r.log");
    fs::write(&path, b"a1\na2\na3\n").unwrap();
    let doc = Document::open(&path, opts()).unwrap();
    wait_ready(&doc, 9);
    // Classic rotation: rename away, create a new file with the same name.
    fs::rename(&path, dir.path().join("r.log.1")).unwrap();
    fs::write(&path, b"b1\n").unwrap();
    wait_event(&doc, "Rotated", |e| matches!(e, DocEvent::Rotated { .. }));
    wait_ready(&doc, 3);
    let s = doc.snapshot();
    assert_eq!(s.generation, 1);
    assert_eq!(s.last_event.unwrap().kind, NoticeKind::Rotated);
    append(&path, b"b2\n");
    wait_ready(&doc, 6);
    let r = request(
        &doc,
        LineRequest::Range {
            first: 0,
            count: 10,
        },
    );
    assert_eq!(texts(&r), ["b1", "b2"]);
}

#[test]
fn rotation_to_a_larger_file_is_not_mistaken_for_growth() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.log");
    fs::write(&path, b"x\n").unwrap();
    let doc = Document::open(&path, opts()).unwrap();
    wait_ready(&doc, 2);
    fs::rename(&path, dir.path().join("big.log.1")).unwrap();
    fs::write(&path, "y\n".repeat(50)).unwrap();
    wait_event(&doc, "Rotated", |e| matches!(e, DocEvent::Rotated { .. }));
    wait_ready(&doc, 100);
    let r = request(&doc, LineRequest::Range { first: 0, count: 1 });
    assert_eq!(texts(&r), ["y"]);
    assert_eq!(doc.snapshot().lines.known, 50);
}

#[test]
fn deletion_is_reported_and_recreation_rotates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("d.log");
    fs::write(&path, b"keep\n").unwrap();
    let doc = Document::open(&path, opts()).unwrap();
    wait_ready(&doc, 5);
    fs::remove_file(&path).unwrap();
    wait_event(&doc, "Removed", |e| matches!(e, DocEvent::Removed { .. }));
    wait_until("file_missing in snapshot", || doc.snapshot().file_missing);
    // The content we already have stays readable.
    assert_eq!(
        texts(&request(&doc, LineRequest::Tail { count: 1 })),
        ["keep"]
    );
    fs::write(&path, b"reborn\n").unwrap();
    wait_event(&doc, "Rotated", |e| matches!(e, DocEvent::Rotated { .. }));
    wait_ready(&doc, 7);
    assert!(!doc.snapshot().file_missing);
    assert_eq!(
        texts(&request(&doc, LineRequest::Tail { count: 1 })),
        ["reborn"]
    );
}

#[test]
fn handle_mode_ignores_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("h.log");
    fs::write(&path, b"h1\n").unwrap();
    let doc = Document::open(
        &path,
        OpenOptions {
            follow_mode: FollowMode::Handle,
            ..opts()
        },
    )
    .unwrap();
    wait_ready(&doc, 3);
    let moved = dir.path().join("h.log.1");
    fs::rename(&path, &moved).unwrap();
    fs::write(&path, b"unrelated new file\n").unwrap();
    // Writer keeps appending to the renamed file; we keep following its handle.
    append(&moved, b"h2\n");
    wait_ready(&doc, 6);
    assert_eq!(doc.snapshot().generation, 0);
    let r = request(
        &doc,
        LineRequest::Range {
            first: 0,
            count: 10,
        },
    );
    assert_eq!(texts(&r), ["h1", "h2"]);
}

#[test]
fn follows_utf16_file_through_append_and_truncate() {
    let dir = tempfile::tempdir().unwrap();
    let spool = tempfile::tempdir().unwrap();
    let path = dir.path().join("u16.log");
    let mut raw = vec![0xFF, 0xFE];
    for u in "héllo\n".encode_utf16() {
        raw.extend_from_slice(&u.to_le_bytes());
    }
    fs::write(&path, &raw).unwrap();
    let doc = Document::open(
        &path,
        OpenOptions {
            spool_dir: Some(spool.path().to_path_buf()),
            ..opts()
        },
    )
    .unwrap();
    wait_ready(&doc, "héllo\n".len() as u64);
    assert!(doc.snapshot().spooled);
    let mut more = Vec::new();
    for u in "wörld\n".encode_utf16() {
        more.extend_from_slice(&u.to_le_bytes());
    }
    append(&path, &more);
    wait_ready(&doc, "héllo\nwörld\n".len() as u64);
    assert_eq!(
        texts(&request(&doc, LineRequest::Tail { count: 2 })),
        ["héllo", "wörld"]
    );
    // Truncate and start over with plain ASCII: the encoding is re-detected.
    fs::write(&path, b"plain ascii\n").unwrap();
    wait_event(&doc, "Truncated", |e| {
        matches!(e, DocEvent::Truncated { .. })
    });
    wait_until("plain view", || {
        let s = doc.snapshot();
        !s.spooled && s.utf8_len == 12 && s.lines.exact && s.lines.known == 1
    });
    assert_eq!(
        texts(&request(&doc, LineRequest::Tail { count: 1 })),
        ["plain ascii"]
    );
    drop(doc);
    assert_eq!(fs::read_dir(spool.path()).unwrap().count(), 0);
}

#[test]
fn start_at_tail_and_manual_encoding_on_a_real_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.log");
    let body: String = (0..20_000).map(|i| format!("row {i}\n")).collect();
    fs::write(&path, &body).unwrap();
    let doc = Document::open(
        &path,
        OpenOptions {
            start_at_tail: Some(2),
            encoding: EncodingChoice::Auto,
            ..OpenOptions::default()
        },
    )
    .unwrap();
    let DocEvent::Lines { lines, .. } = wait_event(&doc, "initial tail", |e| {
        matches!(e, DocEvent::Lines { .. })
    }) else {
        unreachable!()
    };
    assert_eq!(texts(&lines), ["row 19998", "row 19999"]);
    wait_ready(&doc, body.len() as u64);
    assert_eq!(doc.snapshot().state, DocState::Ready);
    assert_eq!(doc.snapshot().lines.known, 20_000);
    let r = doc.read_lines_blocking(12_345, 1);
    assert_eq!(r[0].text, "row 12345");
}

#[test]
fn drop_stops_the_actor_promptly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("drop.log");
    fs::write(&path, b"x\n").unwrap();
    let doc = Document::open(&path, opts()).unwrap();
    wait_ready(&doc, 2);
    let start = std::time::Instant::now();
    drop(doc);
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
}
