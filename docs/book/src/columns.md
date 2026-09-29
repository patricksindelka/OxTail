# Columns

OxTail can show a log as a **table**: one column per field (time, level, thread,
message, status code and so on). Columns make logs easier to read, let rules and
filters talk about fields instead of text, give you statistics, and let you
export the result.

Columns are optional and never change the file. A file without a parser is just
text.

## Supported formats

| Format | Notes |
|---|---|
| Delimited (CSV, TSV, pipe, semicolon) | Quoted fields; the header row can name the columns. |
| Regex with named groups | `(?P<time>\S+ \S+) (?P<level>\w+) (?P<msg>.*)`. Any text log. |
| JSON Lines | One object per line. Keys become columns; nested keys are flattened with dots (`http.status`); other keys appear in the detail pane. |
| logfmt | `key=value key2="quoted value"`. |
| Syslog | RFC 3164 and RFC 5424. |
| Apache and Nginx | Common and combined access log formats. |
| W3C Extended (IIS) | Column names come from the `#Fields:` line. |
| log4j / logback pattern | Paste a layout such as `%d{ISO8601} %-5p [%t] %c{1} - %m%n`; it is turned into a regex. |
| Fixed width | Columns at character positions (through a profile's `fields`). |

The full option list for each parser is in
[Highlighting and profiles](profiles.md#columns-table).

## Getting columns

**Automatically.** A few moments after a file opens, OxTail looks at its first
1,000 lines. If the file's profile defines columns, or if one of the formats above
fits, a bar appears:

> Looks like Nginx/Apache combined. Show as columns?  [Accept] [Choose parser...] [Dismiss]

**Accept** turns the table on, **Dismiss** leaves the file as text (the choice is
remembered for that file), and **Choose parser...** opens the chooser. The status
bar then has a **Columns** button to switch between table and text, and
**View > Columns > Show as columns** does the same.

### Auto-detection

Each candidate format is tried on the sample lines and scored as

`coverage * (0.6 + 0.4 * consistency) * specificity`

where *coverage* is the share of lines the format accepts (blank lines,
`#` directive lines and continuation lines such as stack traces do not count
against it), *consistency* is how many of the accepted records fill the same set
of fields, and *specificity* prefers exact built-in formats over generic ones.
Anything scoring below 0.5 is dropped, so plain prose gets no suggestion. The
detected format's name is shown in the bar and in the tooltip of the **Columns**
button. Detection looks at the start of the file only.

### Choose parser

**View > Columns > Choose parser...** (or the button in the bar) opens a window
that lists every detected format with its score, and lets you build one:

* a **regex** with named groups (each group is a column);
* a **log4j** layout pattern;
* **JSON Lines** or **logfmt** (columns are discovered from the first lines);
* **Delimited** with a comma, tab, pipe or semicolon, taking the names from the
  first line;
* **Plain text (no columns)**.

Errors (an invalid regex, a pattern with no named groups) are shown in the
window. The choice is remembered per file. To keep a parser for all files of a
kind, put it in a [profile](profiles.md).

## The table

* Drag a **column header** to reorder columns; drag the right edge of a header to
  resize; double-click that edge to fit the width to the content.
* **Right-click a header** for **Hide column**, **Pin column** (pinned columns
  stay in place while you scroll sideways) and **Auto-fit width**; right-click the
  header strip for the list of columns with **Show all**.
* Numbers, sizes and durations are right-aligned. Cells are shown in a readable
  form: sizes as `1.5 KiB`, durations as `1.5 ms` or `3m 5s`, levels in a
  canonical spelling (`w` shows as `WARN`), and timestamps in the time zone of
  the `timezone` setting. Other text appears as it is in the file, and a value
  that does not fit its kind is shown unchanged.
* Rules that use column conditions or `scope = "column:..."` colour cells.
* Column widths, order, hiding and pinning are remembered per file.

### Multi-line records

A line that the parser does not accept is not an error. It is treated as a
**continuation** of the record above: it is drawn indented and dimmed across all
columns, so a Java stack trace stays together with the log line that caused it.
The same lines have no columns for queries (see below), but ordinary text search
and plain-text terms in queries still see them.

## The detail pane

Select a line and open the detail pane with **View > Columns > Detail pane**, or
right-click and tick **Detail pane**. It shows the selected record as a list of
fields and values, including keys that are not columns (for JSON and logfmt),
with JSON values pretty-printed. **Copy as JSON** and **Copy as CSV** copy the
record (the right-click menu of a line has the same as **Copy record as JSON**
and **Copy record as CSV**).

## Statistics

**View > Columns > Statistics...** opens a window that answers "what are all
these errors?". Pick a column, choose **Whole file** or **Current filter**, and
press **Compute**. A background scan streams progress, and you can cancel it. It
shows:

* the **top values** with counts (memory stays bounded even for columns with
  millions of distinct values);
* for numeric, size and duration columns, the minimum, maximum, mean and the 50th,
  90th and 99th percentiles.

Click a value in the list to add `column:"value"` as an include filter, which is
often the quickest way to narrow down a problem.

## Export

**View > Columns > Export...** writes the records to a file as **CSV** or **JSON
Lines**. Choose the columns (in the order shown in the table), and optionally
**only the lines of the current filter**. The export streams from the file in the
background with constant memory and can be cancelled; a cancelled export removes
the partial file. Lines that are not records (continuations, headers) are skipped.

## The query language

Column queries are used in the [filter panel](filters.md) (with the `Q` toggle)
and with `oxtail --filter '?...'`. Plain text with no `column:` prefix searches
the whole line, so you never need the syntax for simple things.

```text
level:ERROR                  the column equals a value (ignoring case)
level:(ERROR|FATAL)          any of several values
status>=500  duration>250ms  numeric and unit-aware comparisons
msg~"timeout after \d+"      a regular expression on a column
-path:/health                exclude (or NOT path:/health)
ts>2026-09-29T10:00 ts<+15m  time ranges, absolute or relative
timeout "connection reset"   words and phrases, searched in the whole line
```

### Terms

A **column term** is `name`, an operator, and a value, written without spaces
(quote values that contain spaces):

| Operator | Meaning |
|---|---|
| `:` or `=` | Equal. Ignores case. `*` in the value is a wildcard (`host:web-*`). Several values with `(a\|b)` or `a\|b` are alternatives. |
| `!=` | Not equal (also accepts alternatives). |
| `~` | The column matches a regular expression. |
| `!~` | The column does not match a regular expression. |
| `>`, `>=`, `<`, `<=` | Greater, greater or equal, less, less or equal. |

The column name starts with a letter, `_` or `@` and may contain letters, digits
and `_ . @ - $`. Names are matched exactly, then ignoring case, then through
aliases: `ts`, `time`, `timestamp`, `@timestamp`, `datetime` and `asctime` are
the same column; so are `level`, `lvl`, `severity`, `loglevel` and `levelname`;
and `msg` and `message`. For JSON Lines and logfmt, keys that are not columns can
be queried too.

Quoted values use `"..."`; inside quotes only `\"` is an escape, so regular
expression escapes like `\d` pass through.

A word that is not followed by an operator, and any quoted string on its own, is a
**text term**: a case-insensitive substring search of the whole line. A
`name:` followed by `//` is taken as a URL, not a column term.

### Combining terms

* Terms next to each other mean **AND**. `AND`, `OR` and `NOT` are keywords and
  must be upper case. `-` in front of a term is the same as `NOT`.
* Parentheses group: `(level:ERROR OR level:FATAL) -path:/health`.
  Nesting up to 64 levels deep is allowed.

### Values that understand their column

How a value is compared depends on the kind of the column:

* **level** columns: `level:warn` matches `W`, `WARN` and `warning`; the comparison
  operators use severity, so `level>=warn` includes errors and fatals. The levels
  are, from low to high, trace, debug, info, warn, error, fatal (with spellings
  such as `verbose`, `notice`, `severe`, `critical`, `emerg`, `panic`).
* **number**, **duration** and **bytes** columns compare numerically and
  understand units on both sides: `duration>250ms` matches a cell of `1.2 s`;
  `size>=1MiB`. Duration units: `ns`, `us`, `ms`, `s`, `m`, `h`, `d`; sizes: `b`,
  `kb`, `mb`, `gb`, `tb` (powers of 1000) and `kib`, `mib`, `gib`, `tib`
  (powers of 1024). A bare number is milliseconds in a duration column and bytes
  in a bytes column.
* **timestamp** columns compare as points in time, read with the tab's time format
  and time zone: `ts>2026-09-29T10:00`. A **relative**
  value with a minus sign, `ts>-1h`, means "within the last hour", measured from
  the moment the query is parsed. A **relative** value with a plus sign,
  `ts<+15m`, is measured from the lower bound of the same query, so
  `ts>2026-09-29T10:00 ts<+15m` is the quarter of an hour after 10:00. A `+`
  value without a lower bound in the same query is reported as an error.
  For equality, a cell that starts with the value matches, so `ts:2026-09-29`
  matches every cell that begins with that date.
* Everything else is text and compares ignoring case.

### Errors and lines without columns

The filter panel marks the problem in a query as you type (unbalanced quotes or
parentheses, a missing value, an invalid regular expression, an unknown column
with a list of the ones that exist). Lines that the parser does not accept (see
[Multi-line records](#multi-line-records)) have no columns, so column terms do
not match them, and `NOT`/`-` on a column term therefore keeps them.

### Examples

```text
level:(ERROR|FATAL) -msg~"health ?check"
status>=500 status<600 duration>1s
host:web-* level>=warn
ts>-15m level:error
(method:POST OR method:PUT) path~"^/api/v[12]/"
```
