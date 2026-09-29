# Filters

A **filter view** shows only the lines you care about, with their original line
numbers, and it keeps following: new lines that match appear live. Open the
panel with `Ctrl+Shift+F` (**Edit > Filter view...**) or with the `Filter` button
of the find bar.

## The filter panel

Filters are a **stack**. Each row is one condition, and a line is shown when it
passes all of them:

* an **Include** row keeps only lines that match it;
* an **Exclude** row drops lines that match it.

So "include `ERROR`, exclude `healthcheck`, include `user=42`" is three rows.
Rows that are empty or unticked are ignored.

Each row has:

| Control | Meaning |
|---|---|
| Checkbox | Turn the row on or off without deleting it. |
| Include / Exclude | Keep or drop the matching lines. |
| Text box | The text, pattern or column query. |
| `Q` | The text is a **column query** (see below). |
| `.*` | Regular expression (not for column queries). |
| `Aa` / `aa` | Case: smart, sensitive or insensitive (click to cycle). |
| `W` | Whole word only. |
| `x` | Remove the row. |

Above the rows, **Show only matching lines** switches between the filtered view
and the full view; your position is kept, so you can find a line in the
filtered view and then see what surrounds it in the full file. **Add** appends a
row, **Clear** removes them all. The number of lines that pass so far is shown
next to it ("12,431 lines so far" while the scan runs).

**Context** lets you show N lines **before** and **after** every matching line
(0 to 200 each). Context lines are dimmed.

Editing waits until you pause typing (about 300 ms), then the filter restarts;
an invalid regular expression or query is flagged on its row and skipped.

If the active profile has rules with `hide = true`, those lines are folded away
like an extra exclude filter, and a **Show hidden lines** checkbox appears in the
panel and the View menu. See [Highlighting and profiles](profiles.md#actions).

## Column queries

With the `Q` toggle on, the row is a query in the small typed language described
in [Columns](columns.md#the-query-language): `level:(ERROR|FATAL)`,
`status>=500 duration>250ms`, `msg~"timeout after \d+"`, `-path:/health`.
Column queries need a file that is shown as columns; on a plain text file only
the plain-text parts of a query (words and quoted phrases) can match. A query
that names a column the file does not have is flagged with the list of columns
that exist.

## The filter string grammar

Filters are also written as short strings: in the session file (so they are
restored), and after `--filter` on the command line ([Command line](cli.md)).

```text
ERROR            include lines containing "ERROR" (smart case)
-health          exclude lines containing "health"
/user=\d+/r      include, regular expression   (flags after the last '/')
-/GET \/ping/    exclude, literal text "GET \/ping"
?level:ERROR     include lines matching the column query
-?path:/health   exclude lines matching the column query
```

Rules:

* A leading `-` (or `!`) makes the row an **exclude** row.
* A `?` right after the optional `-` marks a **column query**; its text runs to
  the end of the string.
* `/text/flags` gives the text explicitly, and is needed when the text starts
  with `-`, `/`, `!` or `?`, or has leading or trailing spaces. The flags are
  any of `r` (regular expression), `i` (ignore case), `s` (case sensitive) and
  `w` (whole word). Without flags, the text between the slashes is a literal.
* Anything else is a literal, case-smart include.

The parser never fails: text it does not recognise is simply taken as a literal.

A `--filter` value is a single row. To stack several, add rows in the panel; the
session remembers them.
