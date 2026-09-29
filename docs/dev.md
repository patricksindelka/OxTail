# Developer guide

## `cargo xtask`

`.cargo/config.toml` defines `[alias] xtask = "run -q -p xtask --"`, so
`cargo xtask <command>` runs the developer tooling in `xtask/`. Every command
accepts `--help`. xtask depends on no OxTail crate.

Note: use `cargo run --release -p xtask -- ...` for large fixtures; the debug
build is much slower.

### `gen-log`

Deterministic synthetic log generator (seeded with `fastrand`; same flags give
the same bytes).

```sh
cargo xtask gen-log --format nginx --size 1G out.log
cargo xtask gen-log --format log4j --lines 100000 --multiline-ratio 0.05 --crlf out.log
```

| Flag | Meaning |
|---|---|
| `--format` | `plain nginx apache jsonl logfmt log4j syslog syslog5424 csv iis` |
| `--size` / `--lines` | Exactly one: approximate bytes (`500M`, `10G`, 1024-based) or entry count |
| `--seed N` | RNG seed (default 1) |
| `--start` | First timestamp, RFC 3339 (default `2024-01-01T00:00:00Z`) |
| `--rate` | Simulated lines per second of log time (default 100) |
| `--multiline-ratio` | Fraction of entries followed by a Java stack trace (text formats), default 0.01 |
| `--crlf` | CRLF line endings |
| `--encoding` | `utf8` (default), `utf16le` (with BOM), `latin1` |

Timestamps increase monotonically, levels are mostly INFO with some WARN/ERROR,
`iis` gets a `#Fields:` header and `csv` a header row. It prints throughput when
done (well above 200 MB/s in release builds).

### `append-log`

Simulates a live writer for manual follow testing.

```sh
cargo xtask append-log --format nginx --rate 50 --rotate-every 1000 --rotate-mode copytruncate /tmp/live.log
```

`--rotate-mode rename` moves the file to `<path>.1` and recreates it;
`copytruncate` copies to `<path>.1` and truncates in place. `--duration <secs>`
stops it; otherwise it runs until interrupted.

### `check-deps`

Portability gate (PLAN.md section 3). Reads a built binary (ELF `DT_NEEDED`,
Mach-O dylib load commands, PE imports; the OS family comes from the file
format), compares each dependency case-insensitively with the matching section
of `xtask/deps-allowlist.toml` (glob entries, `deny` beats `allow`), prints a
table and fails on anything not allowed or if the binary exceeds `max_size_mb`.

```sh
cargo build --release -p oxtail-cli
cargo xtask check-deps --binary target/release/oxtail [--allowlist xtask/deps-allowlist.toml]
```

On Windows a dynamic C runtime (`vcruntime*`, `msvcp*`, `ucrtbase`,
`api-ms-win-crt-*`) is denied: build with `-C target-feature=+crt-static`
(set in `.cargo/config.toml`). On Linux, X11/Wayland/GL/Vulkan must be
dlopen'ed, never `DT_NEEDED`.
