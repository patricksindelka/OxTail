# Windows Event Log (CSV)

Windows Event Viewer export ("Save All Events As..." to CSV). The first line is the header row, so the column names come from the file itself:

```text
Level,Date and Time,Source,Event ID,Task Category
```

The timestamp format "us" reads month-first dates such as "9/29/2026 10:01:02 AM"; other locales export other shapes.

This is a **gallery example**: it is not built in. Its source is `docs/profiles/windows-event-csv.toml` in the repository; the full text is at the bottom of this page.

## Which files it is used for

* File name patterns (`match_files`): `*event*.csv`, `Application.csv`, `System.csv`, `Security.csv`. A pattern without `/` is compared with the file name, one with `/` with the whole path.
* Content (`match_content`): a regular expression tried on the first lines of the file: `^"?Level"?,"?Date and Time"?,"?Source"?,"?Event ID"?`.

## Columns and timestamps

Parser: **CSV** (`parser = "csv"`).

The column names come from the first line of the file (the header row).

Timestamps: column `Date and Time`, format `us`. They drive go to time, time gaps and the merged view.

## Highlight rules

Higher priority wins when rules overlap; on a tie the later rule wins. Colours are semantic names resolved by the active theme.

| Priority | Rule | Matches | Scope | Style | Actions |
|---:|---|---|---|---|---|
| 40 | Critical / Error | column `Level` `regex` `^(Critical\|Error)$` | `line` | bg `error.subtle` |  |
| 35 | Audit failure | text `Audit Failure` | `match` | fg `error`, bold | alert |
| 30 | Warning | column `Level` `eq` `Warning` | `column:Level` | fg `warn`, bold |  |
| 5 | Information | column `Level` `eq` `Information` | `line` | fg `muted` |  |

## Sample lines

The examples in this gallery are tested against these lines.

```text
Level,Date and Time,Source,Event ID,Task Category
Information,9/29/2026 10:01:02 AM,Service Control Manager,7036,None
Warning,9/29/2026 10:03:17 AM,Microsoft-Windows-DNS-Client,1014,(1014)
Error,9/29/2026 10:05:44 AM,Application Error,1000,(100)
Information,9/29/2026 10:06:01 AM,Microsoft-Windows-Kernel-General,12,None
Error,9/29/2026 10:07:30 AM,disk,7,None
```

## The full profile

```toml
# Windows Event Viewer export ("Save All Events As..." to CSV). The first
# line is the header row, so the column names come from the file itself:
#   Level,Date and Time,Source,Event ID,Task Category
# The timestamp format "us" reads month-first dates such as
# "9/29/2026 10:01:02 AM"; other locales export other shapes.
name = "Windows Event Log (CSV)"
match_files = ["*event*.csv", "Application.csv", "System.csv", "Security.csv"]
match_content = '^"?Level"?,"?Date and Time"?,"?Source"?,"?Event ID"?'

[columns]
parser = "csv"
delimiter = ","

[timestamp]
column = "Date and Time"
format = "us"

[[rules]]
name = "Critical / Error"
match = { column = "Level", op = "regex", value = "^(Critical|Error)$" }
scope = "line"
style = { bg = "error.subtle" }
priority = 40

[[rules]]
name = "Warning"
match = { column = "Level", op = "eq", value = "Warning" }
scope = "column:Level"
style = { fg = "warn", bold = true }
priority = 30

[[rules]]
name = "Audit failure"
match = { literal = "Audit Failure" }
scope = "match"
style = { fg = "error", bold = true }
priority = 35
alert = true

[[rules]]
name = "Information"
match = { column = "Level", op = "eq", value = "Information" }
scope = "line"
style = { fg = "muted" }
priority = 5
```

## Using it

Copy the text above to `<data folder>/profiles/windows-event-csv.toml`. OxTail reloads the profiles folder while it runs, so the profile is used for matching files right away, and it can be chosen by name from the status bar. See [Installation and portable mode](../install.md) for where the data folder is. Adjust `match_files` to your own file names.
