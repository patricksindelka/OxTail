# Installation and portable mode

OxTail is one executable. There is nothing to install: copy it anywhere,
including a USB stick, a network share or a locked-down server, and run it. It
needs no administrator rights, writes nothing to system locations (no registry,
no `Program Files`, `/usr` or similar), and leaves nothing behind outside its
own data folder.

## Getting OxTail

**Build from source.** You need the Rust toolchain; the repository pins the
version in `rust-toolchain.toml`, so `rustup` picks the right one for you.

```sh
git clone https://github.com/patricksindelka/OxTail
cd OxTail
cargo build --release -p oxtail-cli
```

The executable is `target/release/oxtail` (`oxtail.exe` on Windows). The release
profile uses LTO and strips symbols, so the first build takes a while.
Run it directly, or copy it wherever you like:

```sh
target/release/oxtail /var/log/syslog
```

On Windows the C runtime is linked statically, so the executable needs no Visual
C++ redistributable. On Linux the window system libraries (X11 or Wayland, OpenGL
or Vulkan) are loaded at run time, so one binary works on both X11 and Wayland.
The repository's `cargo run -p xtask -- check-deps --binary <path>` command
checks that a built binary only links libraries from an allowlist.

Signed downloads, installers and package manager entries are described under
[Coming in 1.0](introduction.md#coming-in-10).

## Where OxTail keeps its data

OxTail stores its settings, profiles, themes and session in one **data folder**.
It chooses the folder when it starts, in this order:

1. **`--data-dir <path>`** on the command line. Use it to keep a data folder on
   a USB stick, in a project, or to run several separate configurations.
2. **Portable mode.** If a file named `portable` or a folder named
   `oxtail-data` sits next to the executable, the data folder is
   `oxtail-data/` next to the executable. It is created when needed.
3. **Installed mode.** Otherwise the per-user configuration folder of the
   platform:

   | Platform | Folder |
   |---|---|
   | Windows | `%APPDATA%\OxTail\OxTail\config` |
   | macOS | `~/Library/Application Support/OxTail.OxTail` |
   | Linux | `$XDG_CONFIG_HOME/oxtail` (usually `~/.config/oxtail`) |

   These are what the `directories` library reports for the organization and
   application name `OxTail`. The Settings window shows the folder in use at
   the bottom (`Data folder: ...`).

To make a copy of OxTail portable, put an empty file called `portable` next to the
executable, or start it with `--data-dir`.

### What is in the data folder

| Path | Contents |
|---|---|
| `settings.toml` | Your preferences. See [Settings reference](settings.md). |
| `profiles/*.toml` | Your profiles. See [Highlighting and profiles](profiles.md). |
| `themes/*.toml` | Your themes. See [Themes](themes.md). |
| `session.json` | Open tabs, layout, bookmarks, recent files and window position, restored at the next start. |
| `gui-state.json` | Column choices per file, panes and merged tabs. |
| `search-history.json` | The find bar history. |
| `oxtail.log`, `oxtail.log.1` | OxTail's own log. It is rotated when it passes 1 MB. |
| `.oxtail-spool/` | Temporary files: standard input and transcoded copies of files in other encodings. |

Files are written atomically (to a temporary file, then renamed), so pulling a
USB stick in the middle of a save cannot leave a half-written file behind.
Profiles, themes and settings are watched: edit them in a text editor and OxTail
reloads them while it runs.

In portable mode, paths in the session and the recent files list are stored
relative to the executable (as `@exe/...`) when the file is on the same volume as
OxTail, so a USB stick that is `E:` today and `F:` tomorrow still restores
correctly. Other modes store absolute paths.

### Single instance

Starting `oxtail file.log` while OxTail is already running with the same data
folder opens the file in a new tab of the running window instead of starting a
second one. The name of the local socket (a named pipe on Windows) is derived
from the data folder, so two portable copies never talk to each other. Use
`--new-instance` to force a separate window. See [Command line](cli.md).

### Clean-up

Leftovers from a crashed run (spool files named `oxtail-spool-*`, half-written
`*.oxtail-tmp` files) are removed at the next start. OxTail only deletes files
with those names, so pointing `--data-dir` at a folder that holds other things is
safe.

### When the folder cannot be written

If the chosen folder cannot be created or written (a read-only share, a
locked-down machine, macOS App Translocation of an app that was run straight
from Downloads), OxTail keeps running with **settings in memory only**: nothing is
saved, and a notice explains why. Start it with `--data-dir` pointing at a
writable folder, or move the application to a writable place. See
[Troubleshooting](troubleshooting.md).
