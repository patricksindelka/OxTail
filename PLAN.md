# OxTail — Project Plan

A modern, cross-platform, Rust-based successor to BareTail: a real-time log
viewer that opens multi-gigabyte files instantly, follows them as they grow,
searches them fast, highlights them richly, and understands their structure
(columns).

---

## 1. Goals and non-goals

### What BareTail gets right (keep)
- Opens instantly, tiny footprint, single executable, no install required.
- Follows files in real time ("tail -f") without locking them for the writer.
- Handles files of any size.
- Simple line highlighting by keyword.
- Tabs for multiple files.

### What BareTail lacks (fix)
| Area | BareTail | OxTail |
|---|---|---|
| Platforms | Windows only | Windows, macOS, Linux |
| Search | Free version: none; Pro: basic, separate tool (BareGrep) | Built-in incremental literal/regex search, filter views, match minimap |
| Highlighting | Whole-line color per keyword | Per-match or per-line, regex, priorities, per-column rules, profiles, ANSI colors |
| Structure | Plain text only | Column parsing: CSV/TSV, regex, JSON Lines, logfmt, syslog, Apache/Nginx, W3C/IIS, fixed-width |
| Encodings | Limited | UTF-8, UTF-16 LE/BE, Windows-125x, Latin-1, auto-detect |
| UI | 2000s-era Win32 | GPU-rendered, dark/light themes, split views, command palette, keyboard-first |
| Multiple files | Tabs only | Tabs, splits, and a merged, timestamp-interleaved view |

### Non-goals (v1)
- Not a log aggregation platform (no server, no ingestion pipeline, no database).
- Not an editor: files are read-only.
- No remote sources in v1 (SSH / Docker / journald are post-1.0 stretch goals, section 12).

---

## 2. Performance targets

These targets are the acceptance criteria, measured on a mid-range 2024 laptop (NVMe SSD, 8 cores):

| Scenario | Target |
|---|---|
| Open a 10 GB file, first screen visible (top or tail) | < 150 ms |
| Full line index of a 10 GB file (background) | ≥ 2 GB/s (disk-bound) |
| Memory for a 10 GB / 100M-line file, indexed | < 150 MB total |
| Literal search, cold cache, 10 GB | disk-bound; first hit < 50 ms if near the viewport |
| Regex search, warm cache | ≥ 1 GB/s across all cores |
| Follow latency (write → pixels) | < 50 ms |
| Scrolling / rendering | 60 fps (or display refresh rate), never blocked by I/O |
| Idle CPU while following a quiet file | ~0% |
| Cold start of the binary | < 200 ms, single binary < 20 MB |

Rule for the whole project: **the UI thread never does I/O and never waits for a
scan.** Every expensive operation is chunked, cancellable, and reports progress.

---

## 3. Technology choices

| Concern | Choice | Why |
|---|---|---|
| Language | Rust (stable, edition 2024) | Speed, memory safety, one static binary |
| GUI | `egui` + `eframe` (wgpu backend, glow fallback) | Immediate mode works well for virtualized views of millions of rows; GPU text; mature; cross-platform. Alternatives considered: `iced` (retained, more boilerplate for huge virtual lists), `Slint` (declarative, licensing), Tauri (web stack, larger memory footprint) |
| Byte scanning | `memchr` (SIMD newline and literal search) | Line indexing at memory bandwidth |
| Regex | `regex` / `regex-automata`, `aho-corasick` for many literals | Linear-time, no catastrophic backtracking; multi-pattern highlight in one pass |
| Encoding | `encoding_rs` (+ `chardetng` for detection) | Fast, battle-tested (Firefox) |
| File watching | `notify` + polling fallback | Network shares and some filesystems don't deliver events |
| Parallelism | `rayon` for scans, `crossbeam-channel` for messaging | Parallel search/index over chunks |
| Caching | `lru` block cache | Bounded memory regardless of file size |
| Config | `serde` + TOML; `directories` for paths | Human-editable, versioned |
| Timestamps | `jiff` | Time-zone-correct parsing for the merged view |
| JSON | `serde_json` (optionally `simd-json` behind a feature flag) | JSON Lines columns and the detail pane |
| CSV | `csv-core` | Streaming, allocation-free, handles quotes |
| Notifications | `notify-rust` | Desktop alert on a watched pattern |
| Diagnostics | `tracing` | Structured internal logging |
| Test/bench | `criterion`, `proptest`, `cargo-fuzz`, `insta` (snapshots) | Parsers and index must be correct on hostile input |
| Distribution | `cargo-dist` | Signed installers plus portable archives |

---

## 4. Architecture

### 4.1 Workspace layout

```
oxtail/
├─ Cargo.toml                 (workspace)
├─ crates/
│  ├─ oxtail-core/            file source, block cache, line index, decoding, follow/rotation
│  ├─ oxtail-search/          search engine, filter views, match sets
│  ├─ oxtail-highlight/       rule engine, style spans, ANSI parsing, presets
│  ├─ oxtail-columns/         parsers (delimited, regex, JSONL, logfmt, formats), auto-detect
│  ├─ oxtail-time/            timestamp detection and parsing, merged view ordering
│  ├─ oxtail-config/          settings, profiles, themes, session persistence
│  ├─ oxtail-gui/             egui application
│  └─ oxtail-cli/             `oxtail` binary: argument parsing, launches the GUI (TUI later)
├─ benches/  fuzz/  tests/fixtures/
└─ docs/
```

The core crates have no GUI dependency, so they can be tested headlessly and
reused by a future TUI (`ratatui`) front end.

### 4.2 Threading model

```
            ┌──────────────┐  commands (open, scroll-to, search, cancel)
  UI thread │  egui frame  │ ─────────────────────────────────────────┐
            │  (never I/O) │ ◄───────────── snapshots / events ─────┐ │
            └──────────────┘                                        │ ▼
                                                        ┌───────────────────────┐
                                                        │ Document actor (1/file)│
                                                        │  state, index, cache   │
                                                        └──────────┬────────────┘
                     ┌──────────────────┬──────────────────────────┼─────────────────┐
                     ▼                  ▼                          ▼                 ▼
              Indexer task       Watcher (notify/poll)     Search workers      Column/time
              (sequential,       growth, truncation,       (rayon, chunked,    parse workers
               chunked)          rotation detection        cancellable)        (on demand)
```

- Each open file is a **Document** owned by an actor thread. The UI sends
  commands and reads immutable, cheaply cloned snapshots (`Arc`) of the index
  and viewport data.
- Long jobs carry a **generation token**. Starting a new search or reopening a
  file bumps the generation; stale workers see this and stop.
- Results arrive **incrementally** (e.g. search hits appear in the minimap as
  they are found).

### 4.3 Reading large files

**Positioned reads plus an LRU block cache, not a whole-file mmap.**
- On Windows, a memory-mapped view blocks other processes from truncating the
  file (`ERROR_USER_MAPPED_FILE`). That would break log rotation for the app
  that writes the log, which is the thing a tail tool must never do.
- mmap also turns I/O errors on network shares into `SIGBUS` / access violations.
- Positioned reads (`pread` / `seek_read`) of fixed-size blocks (256 KB) in an
  LRU cache (default 64 MB, configurable) give bounded memory and let the OS
  page cache do the rest. Parallel scans read their own blocks and bypass the
  UI cache.
- mmap stays available as an opt-in fast path for static files on Unix,
  behind the same `ByteSource` trait.

**Share modes.** On Windows, open with
`FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE` so writers, rotators and
deleters are never blocked. This is BareTail's key behavior and must be kept.

### 4.4 Line index

A dense index (one `u64` per line) costs 800 MB for 100M lines. Use a
**sparse checkpoint index** instead:

- A checkpoint every 64 KB of file: `(byte_offset: u64, first_line_no: u64)`.
  A 10 GB file needs ~160k checkpoints, about 2.5 MB.
- To find line *N*: binary-search the checkpoint, read that block (usually
  cached), then `memchr`-count newlines within it. Cost: one block scan, which
  is microseconds.
- A small dense per-block cache of line offsets for recently viewed regions,
  so scrolling is O(1).
- Indexing runs in the background at `memchr` speed. **Before it finishes**, the
  view already works:
  - Opening in follow mode seeks to EOF and scans **backwards** for the last
    screen of lines.
  - The scrollbar maps to **byte position** (like `less`), and line numbers show
    as "≈" until the index reaches them.
- Line endings: LF, CRLF and lone CR. UTF-16 uses a 2-byte-aligned newline search.
- **Very long lines** (minified JSON, binary junk) are truncated for display at a
  configurable length (default 16 KB) with a "show full line" action. The index
  treats them normally.

### 4.5 Following, truncation and rotation

- Growth: a `notify` event, or a poll at 250 ms (adaptive, backs off when idle),
  triggers an incremental index of only the new tail.
- **Truncation** (size shrinks): reset the index, show a banner ("file truncated
  at 12:03:44"), keep following.
- **Rotation** (path now points to a different file): detected by inode/device
  on Unix and volume serial + file index on Windows. Options: follow the name
  (default, like `tail -F`) or follow the handle.
- Follow mode auto-pauses when the user scrolls up and resumes with `End`/`F`.
  While paused, a "▼ 1,234 new lines" pill is shown.
- Optional: detect a file that is being replaced (e.g. `copytruncate`) and keep
  a "previous content" marker.

### 4.6 Encodings

- Detection order: BOM, then UTF-16 heuristics (NUL patterns), then `chardetng`
  on the first 64 KB, then fall back to UTF-8 with lossy replacement.
- Manual override per tab; remembered per path.
- Decoding happens per visible line only. Search on UTF-8/ASCII-compatible
  encodings works on raw bytes; for UTF-16, patterns are transcoded or chunks are
  decoded on the fly.

---

## 5. Search

### 5.1 Modes
- **Find** (`Ctrl+F`): incremental; highlights all matches; `F3`/`Shift+F3` next/prev;
  live match count ("1,204 of ≥ 58,311", refined as scanning continues).
- Options: literal / regex / whole word / case sensitivity (smart-case default),
  with invalid regexes flagged inline.
- **Filter view** (`Ctrl+Shift+F`, "grep mode"): a derived view that shows only
  matching lines, with original line numbers. It follows too: new matching
  lines appear live.
  - Filters **stack**: include `ERROR`, exclude `healthcheck`, and `user=42`.
  - Context lines (`-A/-B/-C` style), shown dimmed.
  - Toggle between the filtered view and the full view while keeping the cursor
    on the same source line.
- **Column-scoped search** once columns are defined: `level:ERROR`,
  `status>=500`, `duration>1s` (a small query language, section 7.4).
- **Search across open tabs** with a results panel grouped by file.
- History and saved searches, available from the command palette.

### 5.2 Engine
- Literal: `memchr::memmem` (SIMD). Multiple literals: `aho-corasick`.
- Regex: `regex-automata` meta engine. The **required-literal prefilter** is
  extracted and used to skip non-candidate blocks quickly, and only candidate
  lines are verified.
- The file is split into block-aligned chunks and searched in parallel with
  `rayon`. Results are merged in order and streamed back.
- **Viewport-first scheduling**: chunks around the current position are
  searched first, so "next match" is fast even in a 50 GB file before the
  full scan ends.
- **Match set storage**: a sorted `Vec<u64>` of line numbers (or Roaring
  bitmaps for dense result sets), so filter views are virtual lists over
  the match set. 10M matches ≈ 80 MB worst case, or much less with Roaring.
- Cancelled instantly when the query changes (generation token).

### 5.3 Navigation
- **Minimap / match scrollbar**: a vertical strip showing search hits and
  highlight-rule hits as colored ticks, density-binned for huge files. Click to jump.
- Bookmarks (`Ctrl+F2` toggle, `F2` next), with optional labels; persisted per file.
- Go to line (`Ctrl+G`), go to byte offset, and go to timestamp (section 8).

---

## 6. Highlighting

### 6.1 Rules
Each rule has:
- **Matcher**: literal, regex, or a column condition (`level == "WARN"`).
- **Scope**: whole line / match only / capture group(s) / a specific column.
- **Style**: foreground, background, bold, italic, underline, dim; an optional
  gutter marker and minimap color.
- **Priority** for conflicts. Styles compose by layering: line background, then
  column style, then match style.
- Optional actions: **alert** (desktop notification or sound when a new line
  matches while following, with rate limiting), **hide** (fold matching lines,
  e.g. noisy health checks), **bookmark automatically**.

### 6.2 Profiles
- Rules are grouped into **profiles** (e.g. "Java/log4j", "Nginx access",
  "Kubernetes JSON"). A profile also contains a column definition and a
  default filter.
- Profiles are **auto-selected** by filename glob and/or content sniffing, and can
  be switched per tab.
- Import/export as TOML so teams can share them. BareTail's highlight
  configuration can be imported.
- Built-in presets:
  - Log levels (TRACE…FATAL, including single-letter `E/W/I` forms)
  - Timestamps (ISO 8601, RFC 3339, syslog, epoch ms)
  - IPv4/IPv6, UUIDs, URLs, file paths, HTTP methods and status classes, numbers
  - Stack traces (`at com.foo…`, `Traceback`, `panicked at`), with continuation
    lines grouped with their parent line.

### 6.3 Rendering
- All rules compile into **one** multi-pattern pass per visible line
  (`aho-corasick` for literals plus a `RegexSet` for regexes), producing style
  spans. Only visible lines are styled, and results are cached per line.
- **ANSI escape sequences** (SGR colors) are rendered as colors instead of shown
  as garbage. This can be toggled to show raw text.
- Theme-aware palettes: rules refer to semantic colors (`error`, `warn`,
  `accent1`…) that resolve per theme, and colors are validated for contrast in
  both light and dark themes.
- A live preview of rules when editing them.

---

## 7. Columns (structured view)

### 7.1 Parsers
| Parser | Use | Notes |
|---|---|---|
| Delimited | CSV, TSV, `\|`, `;` | `csv-core`, quoted fields, header row detection |
| Regex with named groups | Arbitrary text logs | `(?P<ts>\S+ \S+) (?P<level>\w+) \[(?P<thread>[^\]]+)\] (?P<msg>.*)` |
| JSON Lines | Structured app logs | Keys become columns; nested keys flattened (`http.status`) |
| logfmt | Go and Heroku style | `key=value key2="quoted value"` |
| Syslog | RFC 3164 / RFC 5424 | Built-in |
| Apache / Nginx | Common and combined formats | Built-in |
| W3C Extended / IIS | Reads the `#Fields:` header | Built-in |
| log4j / logback pattern | Paste a `%d %-5p [%t] %c - %m%n` pattern | Converted to a regex parser |
| Fixed width | Mainframe or report output | Drag column boundaries on a ruler |

### 7.2 Behavior
- **Auto-detection**: sample the first and last ~1,000 lines, try each parser,
  and pick the best by coverage and consistency. The detected format is shown as
  a dismissible suggestion ("Looks like Nginx combined, show as columns?").
- **Lazy parsing**: lines are parsed only when visible, or when a column filter
  or sort needs them. Results are cached per block.
- **Non-matching lines** (e.g. stack trace continuation lines) attach to the
  previous record and span all columns, so multi-line records stay together.
- Column UI: resize, reorder, hide/show, pin (freeze) columns, auto-fit width,
  and a "message" column that takes the remaining width.
- Per-column value formatting: timestamps (re-render in local time or UTC),
  durations, byte sizes, and syntax-highlighted JSON cells.
- **Detail pane**: the selected record shown as a key/value list or pretty-printed
  JSON, with copy-as-JSON / copy-as-CSV.
- **Column statistics** (on demand, for the current filter): top values with
  counts, and min/max/percentiles for numeric columns. Click a value to add it as
  a filter. This is often the fastest way to find out "what are all these
  errors?".
- **Export** the filtered view with the selected columns to CSV or JSON Lines.

### 7.3 Sorting
Sorting a 100M-line file interactively is not realistic. Sort is offered on
**filtered views** below a threshold (default 1M rows) and runs in the
background. For whole files, use "go to timestamp" instead.

### 7.4 Query language (small, typed)
```
level:ERROR                        column equals (case-insensitive)
level:(ERROR|FATAL)                alternatives
status>=500  duration>250ms        numeric and unit-aware comparisons
msg~"timeout after \d+"            regex on a column
-path:/health                      exclude
ts>2026-09-29T10:00 ts<+15m        time ranges (absolute or relative)
```
Plain text with no `column:` prefix is a normal search across the whole line,
so beginners never need the syntax.

---

## 8. Time and multiple files

- **Timestamp detection** per profile or automatically (the first timestamp-like
  token on a line). Supports many common formats, epoch seconds/ms/µs, and a
  configurable time zone for zone-less logs.
- **Go to time**: binary search when timestamps are roughly monotonic (the common
  case), with a linear fallback otherwise.
- **Merged view**: select several tabs and merge them into one view,
  interleaved by timestamp (k-way merge over each file's index). Each line is
  tagged with a colored source badge. This view also follows all sources live.
- **Time gaps**: optionally show a separator when consecutive lines are more
  than *X* seconds apart, which makes restarts and stalls easy to spot.
- **Relative time column**: "+00:00.153 since the previous line / since the
  selected line".

---

## 9. User interface

### 9.1 Layout
```
┌──────────────────────────────────────────────────────────────────────────────┐
│ ☰  app.log ×  │ nginx/access.log ×  │ worker-*.log (merged) ×  │  +          │  tabs
├──────────────────────────────────────────────────────────────────────────────┤
│ 🔍 [ timeout            ] .* Aa ab  ◀ ▶  1,204 / 58,311   ⧩ Filter  ⚙ Profile│  search bar
├─────┬──────────────────────┬───────┬──────────────────────────────────────┬──┤
│   # │ Time                 │ Level │ Message                              │▓ │
│ 812 │ 2026-09-29 10:01:02  │ INFO  │ GET /api/users 200 12ms              │  │
│ 813 │ 2026-09-29 10:01:02  │ WARN  │ slow query 850ms                     │▒ │  minimap with
│ 814 │ 2026-09-29 10:01:03  │ ERROR │ upstream timeout after 30s           │█ │  match ticks
│     │                      │       │   at http.Client.do (client.go:612)  │  │
├─────┴──────────────────────┴───────┴──────────────────────────────────────┴──┤
│ Detail: { "ts": "...", "level": "ERROR", "msg": "upstream timeout ..." }     │  detail pane
├──────────────────────────────────────────────────────────────────────────────┤
│ ● Following  UTF-8  LF  3.2 GB  41,203,118 lines  Indexed 100%  Profile: Go  │  status bar
└──────────────────────────────────────────────────────────────────────────────┘
```

### 9.2 Features
- Tabs plus **splits** (horizontal/vertical, drag a tab onto an edge). Two views of
  the same file are possible, e.g. a filtered view on top and the full file below,
  with synchronized cursors.
- **Command palette** (`Ctrl+Shift+P`) for every action, with fuzzy search.
- Keyboard-first: `less`-style keys available (`g`, `G`, `/`, `n`, `N`, `F`) as an
  optional keymap next to standard Windows/macOS bindings. All keys are remappable.
- Dark, light and high-contrast themes that follow the system setting.
  Configurable monospace font, ligatures off by default, adjustable line height,
  and zoom (`Ctrl+wheel`).
- Word wrap toggle, horizontal scrolling, visible whitespace/control characters,
  and optional hex view for binary regions.
- Line numbers, relative timestamps, and gutter markers (bookmarks, rule hits).
- Drag and drop files or folders. Open a glob (`logs/*.log`) as tabs or as a merged view.
  Recent files, pinned files, and **session restore** (tabs, splits, scroll
  positions, filters).
- Copy the selection as plain text, with styling (HTML/RTF), or as columns (TSV).
- Clear/"mark" line: insert a visual marker at the current end (like pressing
  Enter a few times in a terminal) to separate "before my test" from "after".
- Accessibility: screen-reader labels via AccessKit (built into egui), full
  keyboard access, and a font size that scales with system DPI.
- Localization-ready (Fluent), English at launch.

### 9.3 CLI
```
oxtail app.log                         open (follow if the file is growing)
oxtail -n 500 app.log                  start at the last 500 lines
oxtail --profile nginx access.log
oxtail --filter 'level:ERROR' *.log
oxtail --merge worker-*.log
some_cmd | oxtail -                    view stdin (buffered to a temp spool file)
```
Single instance: a second invocation opens a tab in the running window (IPC over a
local socket or named pipe).

---

## 10. Configuration

- `config.toml` in the platform config directory (`directories` crate), or next to
  the executable in **portable mode** (as with BareTail, to run from a USB stick).
- `profiles/*.toml` for profiles, `themes/*.toml` for themes, and `session.json`
  for restore state.
- Settings UI edits the same files. Changes are hot-reloaded.
- Schema versioning with automatic migration.

Example profile:
```toml
name = "Go service (logfmt)"
match_files = ["*service*.log"]

[columns]
parser = "logfmt"
order  = ["time", "level", "msg", "*"]
timestamp = { column = "time", format = "rfc3339" }

[[rules]]
match = { column = "level", equals = "error" }
scope = "line"
style = { bg = "error.subtle", minimap = "error" }
alert = true

[[rules]]
match = { regex = '\bduration=(\d{4,})ms' }
scope = "group:1"
style = { fg = "warn", bold = true }
```

---

## 11. Milestones

Durations assume one or two developers working part-time. Each milestone ends in a usable release.

### M0: Foundations (2 weeks)
- Workspace, CI (Windows/macOS/Linux; fmt, clippy, tests), `cargo-dist` release pipeline.
- Big-file fixture generator (`xtask gen-log --size 10G --format nginx`).
- Criterion benchmark harness, tracked in CI to catch regressions.

### M1: Core viewer, the BareTail replacement (5 weeks)
- `ByteSource`, block cache, sparse line index, backward scan from EOF.
- Follow, truncation and rotation handling; Windows share modes.
- Encoding detection.
- egui virtualized text view: scroll, line numbers, wrap, select and copy, tabs,
  status bar, drag and drop, recent files.
- **Exit criteria:** meets section 2 targets for open, follow and scroll on a 10 GB file.
  Usable instead of BareTail every day.

### M2: Search and filter (4 weeks)
- Parallel literal/regex engine with prefilter and viewport-first scheduling.
- Find bar, match counts, minimap, bookmarks, go to line.
- Stacked filter views (include/exclude), context lines, live filtering while following.

### M3: Highlighting and profiles (3 weeks)
- Rule engine, multi-pattern single pass, style layering, ANSI rendering.
- Profiles with auto-selection, built-in presets, rule editor with live preview.
- Alerts (notifications) and hide rules. BareTail config import.

### M4: Columns (5 weeks)
- Parsers: delimited, regex, JSONL, logfmt, syslog, Apache/Nginx, W3C, log4j pattern, fixed width.
- Auto-detection, multi-line records, column UI, detail pane.
- Column query language, column statistics, export.

### M5: Time and multi-file (3 weeks)
- Timestamp detection, go-to-time, gap separators, relative time.
- Merged, timestamp-interleaved view; splits; search across tabs.

### M6: Polish and 1.0 (3 weeks)
- Command palette, keymaps (including `less` style), themes, settings UI, session restore.
- Single-instance IPC, stdin input, portable mode, accessibility pass.
- Signed installers (MSI, notarized DMG, AppImage/Flatpak, deb/rpm), `winget`/Homebrew/Scoop manifests.
- Documentation site and a sample profile gallery.

**Total: about 25 weeks to 1.0.** Public betas are released after M2 and M4.

---

## 12. Post-1.0 ideas (ranked)
1. **Remote sources**: `ssh://host/var/log/app.log` (runs `tail`/`dd` remotely and
   streams back; no agent needed), `docker logs`, `kubectl logs`, systemd journal.
2. **TUI front end** (`oxtail --tui`) sharing the core crates, for servers without a GUI.
3. Compressed files: `.gz` and `.zst`, with a seekable index built on first open
   (like `zran`), and `.zip` members.
4. Histogram timeline: events per minute over the whole file, colored by level,
   with brush selection to filter a time range.
5. Pattern clustering ("these 40,000 lines are 12 distinct templates"), e.g. a
   Drain-style algorithm, for finding the unusual line.
6. Plugin API (WASM, sandboxed) for custom parsers and actions.
7. Diff two logs (e.g. a good run and a bad run) after normalizing timestamps and IDs.

---

## 13. Testing and quality strategy

- **Unit and property tests** (`proptest`) for the line index: for random content,
  random chunk boundaries and random append/truncate sequences, the index must
  agree with a naive `lines()` reference implementation.
- **Fuzzing** (`cargo-fuzz`) for every parser, the query language, ANSI parsing and
  encoding detection.
- **Follow torture tests**: a writer process that appends, rotates (rename and
  create), copy-truncates, and deletes at high rates while the viewer's view is
  checked against the ground truth. Run on all three OSes in CI.
- **Snapshot tests** (`insta`) for highlighting spans and column parsing.
- **Benchmarks** in CI with regression thresholds for indexing, search and render
  frame time on generated 1 GB fixtures (larger fixtures run nightly).
- **UI tests** with `egui_kittest` for key flows (open, search, filter, columns).
- Dogfooding: tail OxTail's own `tracing` log with OxTail.

---

## 14. Risks and mitigations

| Risk | Mitigation |
|---|---|
| egui text rendering of very long lines or huge wraps is slow | Truncate long lines for display; lay out only visible glyph runs; cache galleys per line; fall back to a custom glyph-atlas renderer for the text area if needed |
| Timestamp and format auto-detection guesses wrong | Always show what was detected and let the user override in one click; remember the choice per path |
| Network shares: missing change events, stale sizes | Polling fallback; treat I/O errors as transient with retry and a visible banner |
| Regex performance traps (huge Unicode classes) | `regex` is linear-time; set size limits; show "slow pattern" hints; prefilter |
| Scope creep toward a full log platform | Non-goals in section 1; features after M6 go through the ranked list in section 12 |
| Windows-specific file semantics (share modes, locking, long paths) | Windows CI from day one; `\\?\` long-path handling; tests with a concurrent writer |

---

## 15. Immediate next steps
1. Create the workspace skeleton and CI (M0).
2. Write the fixture generator and the index property tests **before** the index
   itself.
3. Spike (1–2 days): egui virtual list with 100M rows, backed by a fake source,
   to confirm frame times and the byte-position scrollbar approach before building
   the rest of the UI on it.
