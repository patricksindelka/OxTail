# Keyboard shortcuts

The shortcuts are the same in every tab. On macOS, `Ctrl` in the tables below
means the `Cmd` key (except `Ctrl+Tab`, which stays `Ctrl`). The list is also in
the app: **Help > Keyboard shortcuts** (`F1`), and every key can be changed in
**Settings > Keyboard**.

## Command palette

`Ctrl+Shift+P` opens the command palette: type a few letters of any command
(`wrap`, `go time`, `dark`), pick it with `Up`/`Down` and press `Enter`. It lists
every command in the menus with its shortcut, and puts the ones you ran recently
at the top.

Shortcuts made only of a letter, an arrow or a paging key (`F`, `Home`, `End`,
`PageUp`, `Up` and so on) belong to the text box while you type in one, for
example in the find bar. Shortcuts with `Ctrl` or `Alt`, and `F2`, `F3` and
`Esc`, work everywhere.


## Files and tabs

| Shortcut | Action |
|---|---|
| `Ctrl+O` | Open a file |
| `Ctrl+Shift+O` | Open a folder |
| `Ctrl+W` | Close the tab |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous tab |
| `Ctrl+V` | Open the clipboard text in a new tab |
| `Ctrl+,` | Settings |
| `Ctrl+Q` | Quit |

## Search and navigation

| Shortcut | Action |
|---|---|
| `Ctrl+F` | Find |
| `F3` / `Shift+F3` | Next / previous match |
| `Enter` / `Shift+Enter` | Next / previous match (in the find box) |
| `Up` / `Down` | Earlier / later searches (in the find box) |
| `Ctrl+Shift+F` | Filter view |
| `Ctrl+Alt+F` | Search in all open tabs |
| `Ctrl+G` | Go to line (`123`, `+100`, `-50`, `50%`) |
| `Ctrl+Shift+G` | Go to time (`2026-09-29 10:00`, `10:15`, `-15m` from the end, `+1h` from the start) |
| `Ctrl+F2` | Toggle a bookmark on the selected line |
| `F2` / `Shift+F2` | Next / previous bookmark |
| `Ctrl+M` | Insert a mark after the last line |
| `Esc` | Close the find bar or a dialog, clear the selection |

## Moving around

| Shortcut | Action |
|---|---|
| `F` | Toggle following |
| `Home` or `Ctrl+Home` | First line |
| `End` or `Ctrl+End` | Last line, and follow |
| `PageUp` / `PageDown` | One page up / down |
| `Up` / `Down` | One line up / down |
| `Left` / `Right` | Scroll sideways |

## Selecting and copying

| Shortcut | Action |
|---|---|
| `Ctrl+A` | Select all lines |
| `Ctrl+C` | Copy the selected lines |
| `Ctrl+Shift+C` | Copy the selected lines with line numbers |

## View

| Shortcut | Action |
|---|---|
| `Alt+Z` | Wrap lines |
| `Ctrl+=` or `Ctrl++` | Zoom in |
| `Ctrl+-` | Zoom out |
| `Ctrl+0` | Reset zoom |
| `Ctrl` + mouse wheel | Zoom |

Zoom changes the font size, and the new size is saved in `font_size`.

## Mouse

| Action | Effect |
|---|---|
| Click a line | Select it |
| `Shift`+click, or drag | Select a range |
| Double-click a line number | Add a bookmark and edit its label |
| Right-click a line | Copy, select all, copy the record as JSON or CSV, detail pane, bookmark, add a mark |
| Right-click a tab | Split right, split down, merge with..., close |
| Click the minimap or the scrollbar | Jump there |
| Drag a tab | Reorder it, move it to another pane, or split at a pane edge |
| Right-click a column header | Hide, pin, auto-fit |

## The `less` preset

Set **Settings > Keyboard > Key bindings** to *less-style* (or `keymap = "less"`
in `settings.toml`) to add the keys of `less` on top of the standard ones:

| Key | Action |
|---|---|
| `g` / `G` | First line / last line (and follow) |
| `/` or `?` | Find |
| `n` / `N` | Next / previous match |
| `F` | Toggle following |
| `j` / `k` | One line down / up |
| `Space` / `b` | One page down / up |

Like the other single keys they only work while no text box has the focus. `q`
is not bound, so it never closes anything by accident.

## Changing keys

In **Settings > Keyboard**, click **Change** next to a command and press the new
keys (`Esc` cancels). **Clear** removes the shortcut and **Reset** brings back
the preset's. If the keys already belong to another command, that command
loses them and the list says so. Copy (`Ctrl+C`) and paste (`Ctrl+V`) come from
the system and cannot be changed.

The changes are saved in `settings.toml` under `custom_keybindings`, one line per
command id. `Mod` means `Ctrl` on Windows and Linux and `Cmd` on macOS, so a
portable folder works on every system:

```toml
[custom_keybindings]
"view.wrap" = "Mod+Shift+W"   # instead of Alt+Z
"go.time" = "F6"
"view.follow" = "none"        # no shortcut
```

Keys are letters, digits, `F1`..`F12`, arrows, `Home`, `End`, `PageUp`,
`PageDown`, `Enter`, `Esc`, `Space`, `Tab`, `Backspace`, `Delete`, `Insert` and
punctuation, after any of `Mod`, `Ctrl`, `Cmd`, `Alt` and `Shift`. Hover over a
command in the settings window to see its id. A line that
cannot be read is skipped with a warning in the settings window; the other keys
keep working.
