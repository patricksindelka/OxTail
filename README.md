# OxTail

A fast, portable, real-time log viewer written in Rust: a modern successor to
BareTail for Windows, macOS and Linux.

OxTail opens multi-gigabyte files without loading them, follows them as they grow
(surviving truncation and rotation), searches and filters them in the background,
highlights them with rules, and can show structured logs as columns. It is one
executable that you can run from a USB stick, and it never locks the file the
application is writing to.

> Screenshots are not part of the repository yet. They will be added here and to
> the user guide once the interface has been reviewed on real displays.

## Highlights

* **Big files, small footprint.** Positioned reads with a bounded block cache and a
  sparse line index (one checkpoint per 64 KB), so memory does not grow with file
  size. The view is usable before the index is complete.
* **Follows logs properly.** Growth, truncation, rotation (by name, like `tail -F`)
  and deletion are detected. Files are opened read-only with shared access and
  are never memory-mapped, so the writer is never blocked.
* **Search and filter.** Incremental literal and regex search with a match
  minimap; stacked include and exclude filters with context lines that keep
  following; search across all open tabs.
* **Highlighting and profiles.** Rules per line, match, capture group or column,
  with priorities, alerts and hide rules; ANSI colours; profiles picked by file
  name or content, in plain TOML. Eight profiles are built in, and the
  [profile gallery](docs/book/src/gallery/index.md) has more.
* **Columns.** CSV/TSV, regex, JSON Lines, logfmt, syslog, Apache/Nginx, IIS (W3C),
  log4j patterns and fixed width, with auto-detection, multi-line records, a
  detail pane, column statistics, export and a small query language
  (`level:(ERROR|FATAL) status>=500 duration>250ms`).
* **Time.** Go to time, gap separators, relative times, and a merged view that
  interleaves several files by timestamp; splits and cross-tab search.
* **Encodings.** UTF-8, UTF-16, Windows code pages and more, detected
  automatically or chosen per tab.
* **Portable.** No installer, no runtime, no registry writes, no files outside its
  data folder. Settings travel with the executable in portable mode.

## Install

Portable use is the primary way to run OxTail: copy the executable anywhere and run
it. Signed release archives and installers are still being prepared; today you
build it from source (below).

### Portable mode

OxTail keeps its settings, profiles, themes and session in one data folder,
chosen at start-up in this order:

1. `--data-dir <path>` on the command line;
2. **portable mode**: if a file called `portable` or a folder called `oxtail-data`
   sits next to the executable, the data lives in `oxtail-data/` next to it;
3. otherwise the platform's per-user configuration folder
   (`%APPDATA%\OxTail\OxTail\config`, `~/Library/Application Support/OxTail.OxTail`,
   `$XDG_CONFIG_HOME/oxtail`).

To make a copy portable, create an empty file named `portable` next to the
executable. If the folder is read-only, OxTail still runs, with settings in
memory only, and says so. Details: [Installation and portable
mode](docs/book/src/install.md).

## Build from source

You need the Rust toolchain; `rust-toolchain.toml` pins the version, so `rustup`
selects it automatically.

```sh
cargo build --release -p oxtail-cli
target/release/oxtail path/to/app.log
```

```sh
cargo run -p oxtail-cli -- path/to/file.log                       # run the app
cargo run -p xtask -- gen-log --size 1G --format nginx out.log    # a big test file
```

On Linux the window system libraries (X11 or Wayland, OpenGL or Vulkan) are loaded
at run time, so nothing but a display is needed to run the binary.

## Use

```sh
oxtail app.log                          # open and follow
oxtail -n 500 app.log                   # start 500 lines from the end
oxtail --profile nginx access.log
oxtail --filter ERROR *.log             # only lines containing ERROR
oxtail --filter '?level:ERROR' *.log    # a column query (needs columns)
oxtail --merge worker-1.log worker-2.log
some_cmd | oxtail -                     # view standard input
```

Common keys: `Ctrl+F` find, `Ctrl+Shift+F` filter, `Ctrl+G` go to line,
`Ctrl+Shift+G` go to time, `F` follow, `Ctrl+O` open. The full list is in the
guide.

## Documentation

The user guide is an [mdBook](https://rust-lang.github.io/mdBook/) in
[`docs/book`](docs/book/src/introduction.md). It covers installation, following,
search, filters, profiles, columns, time, shortcuts, the command line, settings,
themes and troubleshooting, and it has the generated profile gallery. It is
published to GitHub Pages by `.github/workflows/docs.yml` (once Pages is enabled
for the repository, at <https://patricksindelka.github.io/OxTail/>). To read it
locally:

```sh
mdbook serve docs/book --open
```

Developer notes are in [`docs/dev.md`](docs/dev.md), and the design and roadmap in
[`PLAN.md`](PLAN.md).

## Project status

Milestones M0 to M5 (foundations, core viewer, search and filter, highlighting and
profiles, columns, time and multiple files) are implemented; M6 (polish, packaging
and documentation) is in progress. A visual check on real displays is still
pending. The user guide has a short list of what is not built yet.

## Contributing

`AGENTS.md` describes the rules of the codebase (the UI thread never does I/O,
bounded memory, never block the log writer, cancellable background work, no
panics on user data) and the checks to run before sending a change:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p xtask -- gen-docs --check   # after touching profiles or docs
```

## License

The crates declare `MIT OR Apache-2.0` in `Cargo.toml`. The repository does not
contain a `LICENSE` file yet.
