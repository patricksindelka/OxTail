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

## Integration and updates

### System integration (opt-in)

OxTail never registers itself with your system. If you want it in the "Open with"
menu of your file manager, use **Integrate with system** in the settings window
(or run `oxtail --integrate`). **Remove** (`oxtail --remove-integration`) undoes
exactly what was done. It never makes OxTail the default program for any file type.

| System | What is created |
|---|---|
| Windows | Per user, no administrator rights: entries under `HKEY_CURRENT_USER\Software\Classes` (an application entry, the `OxTail.LogFile` type, and OxTail added to the "Open with" list of `.log`, `.txt`, `.out`, `.err` and `.trace`) and a Start menu shortcut. Explorer may need a moment to show them. |
| Linux | `~/.local/share/applications/io.github.patricksindelka.OxTail.desktop` and the icon under `~/.local/share/icons/hicolor/` (`$XDG_DATA_HOME` is honoured). Your default applications (`mimeapps.list`) are not changed. |
| macOS | Not available from the program. Move `OxTail.app` to Applications; Finder then lists it under "Open With". |

Everything OxTail creates is recorded in `integration.json` in the data folder, and
Remove uses only that record, so it works even if you moved or deleted the
executable. Remove only ever touches what OxTail itself could have created: it
ignores any other entry in the record. It never overwrites a file of yours (a
`.desktop` file, icon or shortcut with the same name that OxTail did not
create is left alone and reported), it never deletes a registry key that
existed before (only the values OxTail set, and keys it created that are
empty again), and it refuses to act if the data folder was made on another
computer or account (for example a portable folder on a USB stick), so it can
not delete another machine's entries. The entries point at the executable's current path: if you move a
portable copy, run Integrate again (it updates the entries). Integration needs a
writable data folder, because that is where the record is kept.

If the data folder is lost, remove the entries by hand. On Linux delete
`~/.local/share/applications/io.github.patricksindelka.OxTail.desktop` and
`~/.local/share/icons/hicolor/256x256/apps/io.github.patricksindelka.OxTail.png`.
On Windows delete `%APPDATA%\Microsoft\Windows\Start Menu\Programs\OxTail.lnk`,
the registry keys `HKEY_CURRENT_USER\Software\Classes\OxTail.LogFile` and
`...\Classes\Applications\oxtail.exe`, and the value `OxTail.LogFile` under
`...\Classes\<ext>\OpenWithProgids` for `.log`, `.txt`, `.out`, `.err` and `.trace`.

On Windows the release build is a GUI program without a console, so the
Settings buttons are the primary way to integrate or remove. The command-line
forms report success or failure through the exit status (0 or 1), and their text
output is visible when it is redirected, for example `oxtail --check-update | more`
or `oxtail --integrate > result.txt`. (Redirection relies on the standard
handles the shell passes in; it is expected to work but has not been verified on
every Windows shell.)

### Update check

When `update_check` is on (the default for an installed copy; off for a portable
one), OxTail asks GitHub at most once a day whether a newer release exists, using
the `curl` program that comes with Windows 10 and later, macOS and most Linux
systems. On Windows it runs `System32\curl.exe`. If `curl` is missing or you are offline, nothing happens. OxTail only
tells you about a newer version and links to the release page; it never downloads
or replaces anything. `oxtail --check-update` runs the check once and prints the
result. The time of the last check is kept in `update-check.json` in the data folder.

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
