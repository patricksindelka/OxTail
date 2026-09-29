# Settings reference

Settings live in `settings.toml` in the [data folder](install.md#where-oxtail-keeps-its-data).
Every key is optional: a missing key takes its default, so a file with one line
is fine. The **Settings** window (**File > Settings...**) edits some of the keys;
the others are edited by hand. The file is watched, so a change made in a text
editor takes effect while OxTail is running.

```toml
# settings.toml: a minimal example
theme = "dark"
font_size = 14.0
timezone = "Europe/Amsterdam"
time_gap_threshold_secs = 5.0
```

How the file is read:

* An unknown key is ignored. A key with an invalid value is dropped on its own
  (the others are kept) and OxTail shows a notice listing what it ignored.
* Numbers outside the allowed range are clamped to it.
* A file that is not valid TOML is ignored as a whole, with a notice, and the
  defaults are used. OxTail does not overwrite it until a setting is changed
  in the app.
* When OxTail saves (because you changed something in the app) it rewrites the
  whole file, so comments and formatting are not preserved.
* `schema_version` is written by OxTail. Older files, which used other key names
  (`fontsize`, `word_wrap`, `show_line_numbers`, `poll_ms`, `time_zone`,
  `notifications`), are upgraded automatically.

## Keys

| Key | Default | Meaning |
|---|---|---|
| `schema_version` | `1` | Version of the file format. Leave it alone. |
| `theme` | `"system"` | `"system"` (follow the operating system's dark or light setting), `"dark"`, `"light"`, `"high-contrast"` or `"custom"`. See [Themes](themes.md). Window: **Theme**. |
| `custom_theme` | `""` | The name of the theme to use when `theme = "custom"`. Window: **Custom theme**. |
| `font_size` | `13.0` | Font size in points, 6 to 72. Also changed by zoom (`Ctrl+=`, `Ctrl+-`, `Ctrl+0`, `Ctrl+wheel`). Window: **Font size**. |
| `line_height` | `1.25` | Line height as a multiple of the font size, 1.0 to 3.0. Window: **Line height** (1.0 to 2.5). |
| `wrap` | `false` | Wrap long lines instead of scrolling sideways. `Alt+Z`. Window: **Wrap long lines**. |
| `line_numbers` | `true` | Show line numbers. View menu: **Line numbers**. Window: **Line numbers**. |
| `max_display_line_length` | `10000` | Lines longer than this many characters are cut for display, 80 to 10,000,000. Search, filters and columns still see the whole line. |
| `cache_size_mb` | `64` | Size of the block cache per open file, in megabytes, 4 to 65,536. Memory use does not otherwise grow with file size. |
| `default_encoding` | `"auto"` | Encoding used for files unless you pick one for the tab: `auto` detects; any encoding label works (`utf-8`, `utf-16le`, `windows-1252`, `shift_jis`, ...). |
| `timezone` | `"local"` | Zone for timestamps without one, and for display: `"local"`, `"UTC"` or an IANA name such as `"Europe/Amsterdam"`. An unknown name means UTC. Window: **Time zone**. |
| `time_gap_threshold_secs` | `0.0` | Show a separator between lines further apart than this many seconds; `0` turns it off. View menu: **Show time gaps** (5 seconds). Window: **Time gap separator**. |
| `notifications_enabled` | `true` | Desktop notifications for rules with `alert = true`. Window: **Desktop notifications for alert rules**. |
| `renderer` | `"auto"` | `"auto"`, `"wgpu"` or `"glow"`. Read at start-up; `--renderer` overrides it. See [Troubleshooting](troubleshooting.md). |
| `recent_files_limit` | `20` | How many recent files to remember, at most 500. |
| `keymap` | `"standard"` | `"standard"` or `"less"`. Stored, but the keys are fixed in this version. See [Coming in 1.0](introduction.md#coming-in-10). |
| `custom_keybindings` | empty | A table from action name to key. Stored, not used yet. |
| `update_check` | `false` | Stored, not used yet. When the key is missing, the default is `true` for an installed copy and `false` for a portable one, so a portable copy never checks. |
| `follow_poll_interval_ms` | `250` | Stored (20 to 60,000). In this version OxTail polls adaptively, from 250 ms to about a second, and does not read this key. |
| `font_family` | `""` | Stored, not used yet: the built-in monospace font is always used. |

Things that are **not** settings: the profile of a file, its encoding, its
columns, its filters and its scroll position are saved per file in
`session.json` and `gui-state.json`. To change the profile of every file of a kind,
write a [profile](profiles.md).
