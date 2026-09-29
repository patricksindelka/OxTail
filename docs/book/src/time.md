# Time and multiple files

## How OxTail reads time

OxTail finds a timestamp on each line, either from the `[timestamp]` table of the
file's [profile](profiles.md#timestamp-table) (a column and a format), or by
learning the dominant format from the first lines of the file. It reads ISO 8601
and RFC 3339, `2026-09-29 10:01:02,123` (log4j and Python), Go's
`2026/09/29 10:01:02`, syslog (`Sep 29 10:01:02`, no year), Apache and Nginx
(`29/Sep/2026:10:01:02 +0000`), RFC 2822, US style (`9/29/2026 10:01:02 AM`), a
bare time of day, and epoch seconds, milliseconds, microseconds and
nanoseconds.

Timestamps that carry no zone are read in the time zone of the **`timezone`**
setting (`local` by default, or `UTC`, or an IANA name such as
`Europe/Amsterdam`); the Settings window has a **Time zone** control. A profile
can override it with `zone`. Timestamps shown in the table are displayed in the
same zone.

## Go to time

`Ctrl+Shift+G` (**Edit > Go to time...**) opens a box where you type where to go:

| You type | Goes to |
|---|---|
| `2026-09-29 10:00`, `2026-09-29T10:00:00Z`, `2026-09-29` | That moment (a date alone means midnight). |
| `10:15`, `10:15:05`, `10:15:05.25` | That time of day, today. |
| Anything else the format detector knows: `Sep 29 10:15:00`, `29/Sep/2026:10:15:00 +0000`, an epoch number | That moment. |
| `-15m`, `-1h30m`, `-90s` | 15 minutes, 1.5 hours or 90 seconds **before the end** of the file. |
| `+1h`, `+2d` | 1 hour or 2 days **after the start** of the file. |

Times without a zone are read in the configured time zone. OxTail jumps to the
first line whose timestamp is at or after the target.

For a log whose timestamps only ever grow (the usual case) the search is a binary
search, so it is fast even in a huge file. OxTail checks the answer, and if the
file turns out not to be in time order it falls back to a scan of the whole file.
The search runs in the background, shows a progress indicator in the dialog, and
can be cancelled. Lines without a timestamp (stack trace lines) are skipped
over.

## Time gaps and relative time

Two aids show where time passes in a log:

* **Time gaps.** **View > Show time gaps** draws a separator between two lines
  that are at least a number of seconds apart, which makes restarts and stalls
  easy to spot. The View menu switches it on with 5 seconds; the Settings window
  (**Time gap separator**, seconds, `0` = off) or `time_gap_threshold_secs` in
  `settings.toml` sets any value. The separator says how long the gap was.
  Jumps backwards in time are not gaps.
* **Relative time.** **View > Columns > Relative time** adds a column to the
  gutter with the time since **the previous line** (`+00:00.153`) or since **the
  selected line**. Lines without a timestamp show nothing.

## Merged view

A **merged view** interleaves several files into one list ordered by timestamp,
like a single log of several services.

Ways to make one:

* From the command line: `oxtail --merge worker-1.log worker-2.log`.
* From open tabs: right-click a tab and choose **Merge with...**, or use
  **View > Merge tabs...** (it needs at least two open files), then tick the tabs.
* Restoring a session that had a merged tab.

Up to 256 files can be merged. The new tab is titled `a.log + b.log (merged)`.

In the merged tab:

* Every line has a coloured **badge** with a letter for its source file. Hover the
  badge for the file name.
* The merge is a k-way merge over each file's own order: lines from one file are
  never reordered. A line without a timestamp stays with the line before it, so a
  stack trace stays under its header.
* It **follows all the files live**. A line that arrives late from a quiet file
  can appear after newer lines from another; the merge does not rewrite lines it
  has already shown. If any file is truncated or rotated, the merge is rebuilt.
* The find bar (`Ctrl+F`) searches across the merged order.
* Memory use is small: 8 bytes per merged line, plus the lines in view.

Each file's timestamp format is learned separately, so files with different
formats can be merged. Timestamps without a zone are read in the `timezone`
setting.

## Splits

You can look at the same file twice: **View > Split right** and **Split down**
(also in a tab's right-click menu) show the active file again in a new pane. A
typical use is a filtered view on top and the full file below.

With **View > Sync cursors between panes** on, panes that show the same file
follow each other's selected line. Every pane has its own tab strip. Drag a tab to reorder it, onto another pane's
strip to move it there, or onto the edge of a pane to split. A pane that loses
its last tab is removed. The split layout is part of the session.

## Search across tabs

`Ctrl+Alt+F` searches all open file tabs at once and groups the results by file.
See [Search](search.md#search-in-all-open-tabs).
