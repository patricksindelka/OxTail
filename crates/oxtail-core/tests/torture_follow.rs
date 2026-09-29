//! Follow torture test: a writer thread hammers a real file (appends of random
//! size, in-place truncation, rename+recreate rotation, copy-truncate,
//! delete+recreate) while a `Document` follows the path. The writer keeps a
//! record of the file's exact content (the ground truth). At every checkpoint
//! the writer pauses, and the test waits (polling, 10 s cap) until the
//! document's line count, generation and last lines equal the ground truth.
//!
//! Reproduce a failure with `OXTAIL_TORTURE_SEED=<seed>`; the seed is printed.
//! The default run takes a few seconds; `cargo test -p oxtail-core --test
//! torture_follow -- --ignored --nocapture` runs the long variant.

use std::fs::{self, File, OpenOptions as FsOpen};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

use oxtail_core::{
    DocEvent, DocState, Document, FollowMode, Line, LineRequest, OpenOptions, RequestId,
};

const TIMEOUT: Duration = Duration::from_secs(10);
const TAIL: usize = 50;

/// What the writer just did, so the checker knows what to expect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Only appended: the generation must not change.
    Append,
    /// Truncated in place, copy-truncated or rotated: generation + 1.
    Reset,
    /// The path was removed (the old handle is still followed): generation
    /// unchanged, `file_missing` set.
    Deleted,
}

struct Checkpoint {
    kind: Kind,
    /// Exact content of the current file.
    content: Vec<u8>,
    /// Human-readable trail of the last operations, for failure messages.
    trail: String,
}

struct Writer {
    dir: PathBuf,
    path: PathBuf,
    file: File,
    content: Vec<u8>,
    rng: fastrand::Rng,
    seq: u64,
    rotations: u64,
    tx: SyncSender<Checkpoint>,
    ack: Receiver<()>,
    trail: Vec<String>,
}

impl Writer {
    fn new(dir: &Path, seed: u64, tx: SyncSender<Checkpoint>, ack: Receiver<()>) -> Writer {
        let path = dir.join("app.log");
        Writer {
            dir: dir.to_path_buf(),
            file: open_append(&path),
            path,
            content: Vec::new(),
            rng: fastrand::Rng::with_seed(seed),
            seq: 0,
            rotations: 0,
            tx,
            ack,
            trail: Vec::new(),
        }
    }

    /// Sends a checkpoint and waits for the checker; `false` means stop.
    fn checkpoint(&mut self, kind: Kind) -> bool {
        let keep = self.trail.len().saturating_sub(8);
        let cp = Checkpoint {
            kind,
            content: self.content.clone(),
            trail: self.trail[keep..].join(" | "),
        };
        self.tx.send(cp).is_ok() && self.ack.recv().is_ok()
    }

    fn note(&mut self, what: impl Into<String>) {
        self.trail.push(what.into());
    }

    fn write(&mut self, bytes: &[u8]) {
        self.file.write_all(bytes).unwrap();
        self.content.extend_from_slice(bytes);
    }

    fn make_line(&mut self) -> Vec<u8> {
        self.seq += 1;
        let len = match self.rng.u32(0..100) {
            0..=1 => self.rng.usize(1000..5000),
            2 => self.rng.usize(65_000..70_000), // longer than an index block
            3..=10 => 0,
            _ => self.rng.usize(1..160),
        };
        let mut line = format!("seq={} ", self.seq).into_bytes();
        for _ in 0..len {
            line.push(self.rng.u8(b'a'..=b'z'));
        }
        line.push(b'\n');
        line
    }

    /// A burst of appends at full speed: single lines, batches, and lines
    /// written in two halves (a partial line is visible for a moment).
    fn burst(&mut self, max_lines: usize) {
        let n = self.rng.usize(1..=max_lines);
        self.note(format!("append x{n}"));
        let mut i = 0;
        while i < n {
            match self.rng.u32(0..10) {
                0..=5 => {
                    let l = self.make_line();
                    self.write(&l);
                    i += 1;
                }
                6..=7 => {
                    let mut batch = Vec::new();
                    for _ in 0..self.rng.usize(2..40) {
                        batch.extend(self.make_line());
                        i += 1;
                    }
                    self.write(&batch);
                }
                _ => {
                    let l = self.make_line();
                    let cut = self.rng.usize(0..l.len());
                    self.write(&l[..cut]);
                    thread::yield_now();
                    self.write(&l[cut..]);
                    i += 1;
                }
            }
        }
    }

    fn truncate_in_place(&mut self) {
        self.note("truncate-in-place");
        self.file.set_len(0).unwrap();
        self.content.clear();
    }

    fn copy_truncate(&mut self) {
        self.rotations += 1;
        let copy = self.dir.join(format!("app.log.{}", self.rotations));
        self.note(format!("copy-truncate -> {}", copy.display()));
        fs::copy(&self.path, &copy).unwrap();
        self.file.set_len(0).unwrap();
        self.content.clear();
    }

    fn rotate(&mut self) {
        self.rotations += 1;
        let old = self.dir.join(format!("app.log.{}", self.rotations));
        self.note(format!("rename -> {} + recreate", old.display()));
        fs::rename(&self.path, &old).unwrap();
        self.file = open_append(&self.path);
        self.content.clear();
    }

    /// Runs `phases` phases; returns `false` if the checker asked to stop.
    fn run(&mut self, phases: usize, max_burst: usize) -> bool {
        self.burst(max_burst);
        if !self.checkpoint(Kind::Append) {
            return false;
        }
        for _ in 0..phases {
            // A reset of an already empty file is not observable; append first.
            let structural = if self.content.is_empty() {
                0
            } else {
                self.rng.u32(0..100)
            };
            let ok = match structural {
                0..=39 => {
                    self.burst(max_burst);
                    self.checkpoint(Kind::Append)
                }
                40..=54 => {
                    self.truncate_in_place();
                    self.checkpoint(Kind::Reset)
                }
                55..=69 => {
                    self.copy_truncate();
                    self.checkpoint(Kind::Reset)
                }
                70..=84 => {
                    self.rotate();
                    self.checkpoint(Kind::Reset)
                }
                _ => {
                    self.note("delete");
                    fs::remove_file(&self.path).unwrap();
                    // Wait until the follower has noticed the deletion, then
                    // recreate (a recreate that races the poll and reuses the
                    // inode is indistinguishable from an append, in any tail).
                    self.checkpoint(Kind::Deleted) && {
                        self.note("recreate");
                        self.file = open_append(&self.path);
                        self.content.clear();
                        self.checkpoint(Kind::Reset)
                    }
                }
            };
            if !ok {
                return false;
            }
            // After a reset, keep writing into the fresh file.
            if self.content.is_empty() {
                self.burst(max_burst);
                if !self.checkpoint(Kind::Append) {
                    return false;
                }
            }
        }
        true
    }
}

fn open_append(path: &Path) -> File {
    FsOpen::new().create(true).append(true).open(path).unwrap()
}

fn naive_lines(content: &[u8]) -> Vec<String> {
    let text = String::from_utf8(content.to_vec()).unwrap();
    text.lines().map(str::to_owned).collect()
}

struct Checker {
    doc: Document,
    errors: Vec<String>,
    resets_seen: u64,
}

impl Checker {
    fn drain(&mut self) {
        while let Ok(ev) = self.doc.events().try_recv() {
            self.observe(ev);
        }
    }

    fn observe(&mut self, ev: DocEvent) {
        match ev {
            DocEvent::Error(e) => self.errors.push(e),
            DocEvent::Truncated { .. } | DocEvent::Rotated { .. } => self.resets_seen += 1,
            _ => {}
        }
    }

    fn poll_until(&mut self, what: &str, ctx: &str, mut cond: impl FnMut(&Document) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.drain();
            if cond(&self.doc) {
                return;
            }
            if Instant::now() >= deadline {
                let s = self.doc.snapshot();
                panic!(
                    "timed out after 10 s waiting for: {what}\n  snapshot: gen={} utf8_len={} \
                     indexed={} lines={:?} state={:?} missing={}\n  recent ops: {ctx}",
                    s.generation, s.utf8_len, s.indexed_bytes, s.lines, s.state, s.file_missing
                );
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn request(&mut self, req: LineRequest) -> Vec<Line> {
        let id: RequestId = self.doc.request_lines(req);
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.doc.events().recv_timeout(left) {
                Ok(DocEvent::Lines { id: i, lines, .. }) if i == id => return lines,
                Ok(ev) => self.observe(ev),
                Err(_) => panic!("timed out waiting for a Lines answer"),
            }
        }
    }

    /// Verifies the document against the ground truth in `cp`.
    fn verify(&mut self, cp: &Checkpoint, expected_gen: u64) {
        let ctx = cp.trail.clone();
        let len = cp.content.len() as u64;
        let truth = naive_lines(&cp.content);
        let n = truth.len() as u64;

        if cp.kind == Kind::Deleted {
            self.poll_until("file_missing after deletion", &ctx, |d| {
                d.snapshot().file_missing
            });
            let s = self.doc.snapshot();
            assert_eq!(
                s.generation, expected_gen,
                "deletion must not bump generation"
            );
            assert_eq!(s.utf8_len, len, "the old handle must still be followed");
            return;
        }

        self.poll_until(
            &format!("gen={expected_gen} len={len} lines={n} ({:?})", cp.kind),
            &ctx,
            |d| {
                let s = d.snapshot();
                s.generation >= expected_gen
                    && s.utf8_len == len
                    && s.indexed_bytes == len
                    && s.lines.exact
                    && s.lines.known == n
                    && !s.file_missing
            },
        );
        let s = self.doc.snapshot();
        assert_eq!(
            s.generation, expected_gen,
            "generation after {:?} (recent ops: {ctx})",
            cp.kind
        );
        assert!(
            !matches!(s.state, DocState::Error(_)),
            "document in error state: {:?} (recent ops: {ctx})",
            s.state
        );

        // Last N lines equal the truth.
        let tail = self.request(LineRequest::Tail { count: TAIL });
        let want: Vec<&String> = truth.iter().rev().take(TAIL).rev().collect();
        let got: Vec<&String> = tail.iter().map(|l| &l.text).collect();
        assert_eq!(got.len(), want.len(), "tail length (recent ops: {ctx})");
        for (g, w) in got.iter().zip(&want) {
            assert_eq!(
                g.len().min(16 * 1024),
                w.len().min(16 * 1024),
                "tail line length"
            );
            assert!(
                w.starts_with(&g[..g.len().min(64)]),
                "tail line content: {g:.80} vs {w:.80}"
            );
        }
        if let Some(last) = tail.last() {
            assert_eq!(last.number, n - 1, "tail line numbers");
        }

        // A window in the middle, by line number.
        if n > 0 {
            let first = (n / 3).min(n - 1);
            let win = self.request(LineRequest::Range { first, count: 20 });
            for (i, l) in win.iter().enumerate() {
                assert_eq!(l.number, first + i as u64);
                let w = &truth[first as usize + i];
                assert!(
                    w.starts_with(&l.text[..l.text.len().min(64)]),
                    "range line {} content",
                    l.number
                );
            }
            assert_eq!(win.len(), 20.min((n - first) as usize));
        }
    }
}

fn seed() -> u64 {
    std::env::var("OXTAIL_TORTURE_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(1, |d| d.as_nanos() as u64)
        })
}

fn run(phases: usize, max_burst: usize) {
    let seed = seed();
    eprintln!("torture_follow: seed = {seed} (OXTAIL_TORTURE_SEED={seed} to reproduce)");
    let dir = tempfile::tempdir().unwrap();
    let spool = dir.path().join("spool");
    fs::create_dir(&spool).unwrap();
    let logs = dir.path().join("logs");
    fs::create_dir(&logs).unwrap();

    let (tx, rx) = sync_channel::<Checkpoint>(0);
    let (ack_tx, ack_rx) = sync_channel::<()>(0);
    let mut writer = Writer::new(&logs, seed, tx, ack_rx);
    let path = writer.path.clone();

    let doc = Document::open(
        &path,
        OpenOptions {
            follow_mode: FollowMode::Name,
            spool_dir: Some(spool),
            index_spacing: 256,
            ..OpenOptions::default()
        },
    )
    .unwrap();
    let mut checker = Checker {
        doc,
        errors: Vec::new(),
        resets_seen: 0,
    };

    let handle = thread::spawn(move || {
        let done = writer.run(phases, max_burst);
        drop(writer);
        done
    });

    let mut expected_gen = 0;
    let mut checkpoints = 0u64;
    let mut expected_resets = 0u64;
    while let Ok(cp) = rx.recv() {
        if cp.kind == Kind::Reset {
            expected_gen += 1;
            expected_resets += 1;
        }
        checker.verify(&cp, expected_gen);
        checkpoints += 1;
        if ack_tx.send(()).is_err() {
            break;
        }
    }
    let finished = handle.join().expect("writer thread panicked");
    assert!(finished, "writer stopped early");
    checker.drain();
    assert!(
        checker.errors.is_empty(),
        "document reported errors: {:?}",
        checker.errors
    );
    assert_eq!(
        checker.resets_seen, expected_resets,
        "Truncated/Rotated events vs resets performed"
    );
    eprintln!("torture_follow: {checkpoints} checkpoints, {expected_resets} resets, all verified");
}

#[test]
fn follow_survives_appends_truncation_rotation_and_deletion() {
    run(40, 60);
}

#[test]
#[ignore = "long-running variant; run with --ignored"]
fn follow_torture_long() {
    run(1500, 400);
}
