# Opening and following files

## Opening

There are several ways to open a file, and they all end up as a tab:

* **File > Open** (`Ctrl+O`, `Cmd+O` on macOS) shows the file dialog. You can pick
  several files; each becomes a tab.
* **Drag and drop** files onto the window. A dropped **folder** opens the files
  directly inside it, sorted by name, at most 50 per folder (subfolders are not
  searched).
* **File > Open recent** and the recent files list on the start screen. The list
  is kept in the session; **Clear list** empties it. `recent_files_limit` in the
  settings file sets its length.
* **The command line**: `oxtail app.log other.log`. See [Command line](cli.md).
  If OxTail is already running with the same data folder, the files open as new
  tabs in that window.
* **Standard input**: `some_command | oxtail -`. See [Standard input](#standard-input).

Files open read-only. OxTail never modifies them. It opens them with shared
access (on Windows with `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`)
and does not memory-map them, so the program that writes the log can keep
appending, rotate or delete it while you watch.

A file opens at the **end**, following it, like `tail -f`. Opening a file that is
already open in a tab just switches to that tab. With `-n 500` the view starts
500 lines from the end (the first of those lines is at the top) and following is
paused until you scroll to the end or press `End`. You can start looking at a huge file at once:
the view is available before the line index is complete, and the scrollbar then
maps to byte positions. Line numbers show with a leading `≈` until the index has
reached them. The status bar shows `Indexing 37%` while the index is built.

A file may be empty, still growing, or in the middle of being rewritten. None of
that is an error.

## Following

While a tab is **following**, new lines appear at the bottom as soon as they are
written. The status bar shows `Following` or `Paused`; click it, use the View
menu, or press `F` to toggle.

* Scrolling up (mouse wheel, `Up`, `PageUp`, dragging the scrollbar) pauses
  following. A "N new lines" pill at the bottom counts what arrived since. Click
  it, or press `End` (or `F`), to jump to the end and follow again.
* `Home` goes to the first line.

Changes are noticed in two ways at once: a file system watcher gives a prompt
hint, and OxTail also polls, every 250 ms while the file changes and backing off
to about a second while it is quiet, because file system events are unreliable on
network shares. A quiet file uses close to no CPU.

### Truncation, rotation and deletion

OxTail follows the file **by name**, like `tail -F`:

* **Truncation.** If the file becomes shorter (a `copytruncate` rotation, or
  someone emptying it), OxTail resets the view, shows a banner ("The file was
  truncated at 12:03:44 (UTC). Showing it from the start; still following."),
  and continues from the start of the new content.
* **Rotation.** If the path starts to name a different file (the old one was
  renamed and a new one created), OxTail reopens the path and shows "The file
  was rotated at ...". On Unix it recognizes this by device and inode, on Windows
  by volume serial number and file index.
* **Deletion.** If the file is removed, OxTail keeps following the handle it has
  open and says so in the banner. When a file appears at the path again, it is
  picked up as a rotation.
* **Encoding changes** (you pick another encoding) also produce a banner.

Banners have a **Dismiss** button. A search, filter or merged view that was
running is restarted on the new content, so results never mix old and new data.

## Encodings and line endings

OxTail detects the encoding of a file from its first 64 KB: a byte order mark
first, then UTF-16 (from the pattern of zero bytes), then valid UTF-8 or ASCII,
and finally a statistical guess (Windows code pages, Latin, Cyrillic, East Asian
encodings). Files in another encoding than UTF-8 are converted to UTF-8 in a
temporary spool file in the data folder, and appended data is converted as it
arrives. Invalid bytes are shown as replacement characters, never as an error.

Click the encoding in the status bar to choose one by hand: **Auto-detect**,
UTF-8, UTF-16 LE, UTF-16 BE, Windows-1252, Windows-1250, Windows-1251,
ISO-8859-2, ISO-8859-15, KOI8-R, Shift_JIS, GBK, Big5 or EUC-KR. The choice is
kept for the file. To change the default for files that OxTail cannot decide
about, set `default_encoding` in `settings.toml` to any encoding label (for
example `windows-1252`); `auto` detects.

Lines end with LF, CRLF or a lone CR (the status bar shows `LF` or `CR`; CRLF
counts as LF and the carriage return is not shown).

Very long lines (minified JSON, binary data) are cut for display at
`max_display_line_length` characters (10,000 by default). Search, filters and
columns still see the whole line.

## Selecting, copying, bookmarks and marks

* Click a line to select it, `Shift+click` or drag to select a range,
  `Ctrl+A` to select everything. The status bar shows how many lines are
  selected.
* `Ctrl+C` copies the selected lines as plain text; `Ctrl+Shift+C` adds line
  numbers. The right-click menu has the same entries, plus copying a record as
  JSON or CSV when the file is shown as columns.
* **Bookmarks.** `Ctrl+F2` toggles a bookmark on the selected line, `F2` and
  `Shift+F2` jump to the next and previous one, and **Edit > Bookmarks** has the
  same actions. Double-click a line number to add a bookmark and give it a label.
  Bookmarks are saved per file in the session.
* **Marks.** `Ctrl+M` inserts a visual separator after the last line, like
  pressing Enter a few times in a terminal, to separate "before my test" from
  "after".
* **Go to line** (`Ctrl+G`) accepts `1234`, `+100` or `-50` (relative to the
  current line) and `50%`. Digit separators (`1,234`, `1_234`) are fine.

## Sessions

OxTail saves the session when you close it, and periodically while it runs:
open tabs and their order, the active tab, split layout, scroll position, follow
state, encoding and profile per tab, filters and the last search, bookmarks per
file, recent files and the window position. The next start restores it, and the
files named on the command line open on top of it. Column choices, panes and
merged tabs are saved next to the session.

A file that is missing at restore time shows an error tab you can close. If you
do not want a session, start with a fresh `--data-dir`. See
[Installation and portable mode](install.md) for where the session lives.

## Standard input

`oxtail -` reads standard input:

```sh
kubectl logs -f deploy/web | oxtail -
journalctl -f | oxtail -
```

The input is copied into a spool file in the data folder (`.oxtail-spool/`, or
the system temporary folder when the data folder is in memory) and the tab, named
`<stdin>`, follows that file. Memory does not grow with the amount of data. The
spool file is deleted when the tab closes or OxTail exits, and leftovers of a
crashed run are removed at the next start.

Because it owns standard input, a run with `-` is always its own instance: it
does not hand the request to a running window.
