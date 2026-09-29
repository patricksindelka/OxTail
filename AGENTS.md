# AGENTS.md

Guidance for AI coding agents (Claude Code, Codex, Cursor, Copilot, …) working in
this repository. Humans are welcome too. `PLAN.md` is the source of truth for
*what* to build; this file says *how* to work on it.

## Project in one paragraph

OxTail is a portable, cross-platform, real-time log viewer written in Rust: a
modern successor to BareTail. It opens multi-gigabyte files instantly, follows
them as they grow (surviving truncation and rotation), searches and filters
them fast, highlights them with rules and profiles, and parses lines into
columns. The GUI is `egui`/`eframe`; all heavy lifting lives in GUI-free crates.

## Repository map

| Path | Contents |
|---|---|
| `crates/oxtail-core` | `ByteSource`, block cache, sparse line index, encodings, follow/truncation/rotation, `Document` actor |
| `crates/oxtail-search` | Literal/regex search, prefilters, match sets, stacked filter views |
| `crates/oxtail-highlight` | Rule engine, style spans, ANSI SGR parsing, built-in presets |
| `crates/oxtail-columns` | Column parsers, auto-detection, multi-line records, query language, column stats |
| `crates/oxtail-time` | Timestamp detection/parsing, go-to-time, k-way merged view |
| `crates/oxtail-config` | Settings, profiles, themes, session, data-folder resolution (portable mode) |
| `crates/oxtail-gui` | `eframe` application: views, widgets, input, theming |
| `crates/oxtail-cli` | The `oxtail` binary: CLI parsing, single-instance IPC, launches the GUI |
| `xtask/` | Dev tasks: fixture generator, dependency allowlist check |
| `benches/`, `fuzz/`, `tests/fixtures/` | Benchmarks, fuzz targets, test data |

Dependency direction is strictly downward:
`cli → gui → {config, time, columns, highlight, search} → core`.
Never add a GUI dependency (`egui`, `eframe`, `winit`, `wgpu`) to any crate other
than `oxtail-gui` and `oxtail-cli`.

## Commands

Run these before claiming any change is done. All must pass.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Useful extras:

```sh
cargo run -p oxtail-cli -- path/to/file.log          # run the app
cargo run -p xtask -- gen-log --size 1G --format nginx out.log   # big fixture
cargo run -p xtask -- check-deps                     # portability allowlist check
cargo bench -p oxtail-core                           # criterion benches
```

The GUI cannot be seen in a headless container. Test GUI logic through
`egui_kittest` or by keeping logic in plain functions with unit tests.

## Non-negotiable rules

1. **The UI thread never does I/O and never waits on a scan.** File reads,
   indexing, searching and parsing run on worker threads and report back through
   channels. Anything in `oxtail-gui` that touches `std::fs` or blocks on a
   channel `recv()` is a bug. Use `try_recv` and request a repaint.
2. **Never block the log writer.** Open files read-only with shared access
   (Windows: `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`).
   Do not memory-map files that are being followed (see PLAN.md §5.3).
3. **Bounded memory.** Memory must not scale with file size except through the
   sparse index (one checkpoint per 64 KB) and match sets. No `read_to_end` or
   `read_to_string` on user files, no `Vec<String>` of all lines.
4. **Portable.** No system libraries beyond the allowlist in
   `xtask/deps-allowlist.toml`, no registry writes, no files outside the data
   folder, and assets embedded with `include_bytes!`. Adding a dependency that links
   a native library needs a note in the PR explaining why. See PLAN.md §3.
5. **Cancellable long work.** Every background job takes a generation token
   (`Arc<AtomicU64>` or similar) and stops promptly when it is stale.
6. **No panics on user data.** Malformed UTF-8, binary data, 1 GB lines, empty
   files, files that shrink mid-read: all must be handled. Use `Result`, lossy
   decoding and clamping. `unwrap()` and `expect()` are fine in tests, and in
   code only for true invariants, with a comment saying why.
7. **No `unsafe`** outside `oxtail-core::source` (and only with a `// SAFETY:`
   comment). Crates declare `#![forbid(unsafe_code)]` where possible.

## Coding conventions

- Rust edition 2024 and the stable toolchain. Shared dependency versions go in
  `[workspace.dependencies]` in the root `Cargo.toml`, and crates refer to them
  with `dep = { workspace = true }`.
- Errors: `thiserror` in library crates, `anyhow` only in `oxtail-cli` and `xtask`.
- Logging: `tracing`, never `println!` (except in CLI output and xtask).
- Line numbers are 0-based `u64` internally and shown 1-based. Byte offsets are `u64`.
- Keep public APIs small and documented (`///` on every `pub` item in library
  crates). Add module-level docs for anything non-obvious.
- Prefer plain data plus pure functions, which are testable without the GUI.
- Match the surrounding code's style. Don't reformat unrelated code.

## Testing expectations

- New logic comes with unit tests in the same file (`#[cfg(test)] mod tests`).
- Line index, search and parsers get **property tests** (`proptest`) against a
  naive reference implementation.
- Parsers and anything parsing untrusted input need a fuzz target in `fuzz/`
  (it only has to compile in CI).
- Bugs get a regression test first.
- Tests must not depend on timing where avoidable. When a test must wait for a
  watcher or thread, poll with a timeout (max 10 s), never with a bare `sleep`.

## Workflow for agents

- Read the relevant section of `PLAN.md` before starting on a feature.
- Work on one crate or feature at a time. Keep diffs focused.
- Run the three commands above. Report failures honestly, with output.
- Update the milestone status table below when a milestone item lands.
- Commit messages: imperative subject ≤ 72 chars, a body that explains why,
  scoped by crate when useful (`core: detect rotation via file id on Windows`).

### Claude Code specifics

`.claude/agents/` defines specialised subagents. Delegate by area:

| Agent | Use for |
|---|---|
| `core-engineer` | `oxtail-core`: file access, index, encodings, follow/rotation |
| `search-engineer` | `oxtail-search`, `oxtail-highlight` |
| `structure-engineer` | `oxtail-columns`, `oxtail-time`, `oxtail-config` |
| `gui-engineer` | `oxtail-gui`, `oxtail-cli` |
| `test-engineer` | Property tests, fuzz targets, torture tests, benchmarks, xtask |
| `reviewer` | Read-only review of a diff against this file and PLAN.md |

Skills in `.claude/skills/`: `verify` (full check suite) and `milestone`
(how to take a PLAN.md milestone from spec to merged code).

## Milestone status

| Milestone | Status |
|---|---|
| M0 Foundations | not started |
| M1 Core viewer | not started |
| M2 Search and filter | not started |
| M3 Highlighting and profiles | not started |
| M4 Columns | not started |
| M5 Time and multi-file | not started |
| M6 Polish and 1.0 | not started |
