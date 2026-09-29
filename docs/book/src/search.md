# Search

## The find bar

Press `Ctrl+F` (`Cmd+F` on macOS), or use **Edit > Find**, to open the find bar
above the log. Type, and the search starts about 150 ms after you stop typing.
All matches are highlighted, and the view jumps to the first match at or after
the line at the top of the screen.

| Control | What it does |
|---|---|
| Text box | The text or pattern to find. `Enter` goes to the next match, `Shift+Enter` to the previous one. `Up` and `Down` step through your earlier searches (the last 50 are remembered). |
| `.*` | Treat the text as a regular expression. |
| `Aa` / `aa` | Case handling. Click to cycle: **smart** (ignore case unless the pattern contains an uppercase letter), **sensitive**, **insensitive**. The button is lit when it is not smart. |
| `W` | Whole word only. |
| Up and down arrows | Previous and next match (`Shift+F3` and `F3`). |
| `Filter` | Open the [filter panel](filters.md). |
| `x` | Close the find bar (`Esc`). |

`F3` and `Shift+F3` also work when the find bar is closed: they reopen it and step
through the matches of the last search (with no search yet, they just put the
cursor in the find box).

Next and previous wrap around at the ends of the file.

The counter next to the buttons reads like `1,204 of ≥ 58,311` while the scan
is still running (the `≥` means "at least", the number only grows) and
`1,204 of 58,311` when it is done. `No matches` and `Searching…` are shown
where they apply. The search keeps running while you scroll, and it keeps up
with a file that is growing.

### Regular expressions

Patterns use the syntax of the Rust [`regex`](https://docs.rs/regex/latest/regex/#syntax)
crate: Perl-like, without backreferences and look-around. In exchange, a search
cannot take exponential time. An invalid pattern is reported under the find bar
with the offending part marked, for example `Invalid regular expression: ...
(near "(")`.

A match never spans lines: the pattern is applied to one line at a time.

### How fast is it

Searching runs on all cores in the background, in chunks around the part of the
file you are looking at first, so "next match" is quick even in a very large
file before the whole file has been scanned. Literal text is found with SIMD
byte search, and for a regular expression OxTail extracts the literals the
pattern must contain and uses them to skip most of the file without running the
full expression. Typing a new query cancels the running search at once.

The memory used by results is only the list of matching line positions.

## Minimap

The narrow strip at the right of the log is the **minimap**. It shows where the
matches of the current search (and the lines hit by highlight rules that define
a `minimap` colour) are in the whole file, as coloured ticks; a busy file is
binned so one row of the strip can stand for many lines. Click it to jump to that
place.

## Search in all open tabs

`Ctrl+Alt+F` (**Edit > Search in all tabs...**) opens a window that searches
every open file tab for a text or regular expression, one after the other.
The results are grouped by file: how many lines match (counted up to
1,000,000) and the first 100 matching lines of each file. Click a line to switch
to that tab and jump to the line. The window has the same `.*` and case buttons
as the find bar.

## Go to

* **Go to line** (`Ctrl+G`): `1234`, `+100`, `-50` or `50%`.
* **Go to time** (`Ctrl+Shift+G`): see [Time and multiple files](time.md).
* **Bookmarks** (`Ctrl+F2`, `F2`, `Shift+F2`): see
  [Opening and following files](opening.md#selecting-copying-bookmarks-and-marks).

## Searching structured logs

The find bar searches the text of whole lines. To ask "status is at least 500"
or "level is ERROR or FATAL", use a **column query** in the
[filter panel](filters.md) after the file is shown as [columns](columns.md).
