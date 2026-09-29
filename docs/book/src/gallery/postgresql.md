# PostgreSQL

PostgreSQL server log with the prefix "%m \[%p\] " (the default of log\_line\_prefix in the Debian/Ubuntu packages):

```text
2026-09-29 10:01:02.123 UTC [4242] LOG:  checkpoint starting: time
```

The severity is followed by a colon and two spaces.

This is a **gallery example**: it is not built in. Its source is `docs/profiles/postgresql.toml` in the repository; the full text is at the bottom of this page.

## Which files it is used for

* File name patterns (`match_files`): `postgresql*.log`, `postgres*.log`, `pg_log/*.log`. A pattern without `/` is compared with the file name, one with `/` with the whole path.
* Content (`match_content`): a regular expression tried on the first lines of the file: `^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d\.\d+ \S+ \[\d+\] (DEBUG\d?|INFO|NOTICE|WARNING|ERROR|LOG|FATAL|PANIC|STATEMENT|DETAIL|HINT|CONTEXT):`.

## Columns and timestamps

Parser: **Regex** (`parser = "regex"`).

| Column | Kind |
|---|---|
| `time` | timestamp |
| `zone` | text |
| `pid` | number |
| `severity` | text |
| `message` | text |

Display order (`order`): `time`, `pid`, `severity`, `message`, `zone`. `*` stands for all other columns; names the parser does not produce are ignored.

Timestamps: column `time`, format `iso8601`. They drive go to time, time gaps and the merged view.

## Highlight rules

Higher priority wins when rules overlap; on a tie the later rule wins. Colours are semantic names resolved by the active theme.

| Priority | Rule | Matches | Scope | Style | Actions |
|---:|---|---|---|---|---|
| 50 | FATAL / PANIC | column `severity` `regex` `^(FATAL\|PANIC)$` | `line` | fg `fatal`, bg `fatal.subtle`, bold |  |
| 40 | ERROR | column `severity` `eq` `ERROR` | `line` | bg `error.subtle` |  |
| 35 | deadlock and lock waits | regex `(?i)\b(deadlock detected\|still waiting for\|canceling statement due to)\b` | `match` | fg `error`, underline | alert |
| 30 | WARNING | column `severity` `eq` `WARNING` | `column:severity` | fg `warn`, bold |  |
| 25 | slow statements (1 s and more) | regex `duration: (\d{4,}\.\d+ ms)` | `group:1` | fg `warn`, bold |  |
| 5 | DETAIL / HINT / STATEMENT | column `severity` `regex` `^(DETAIL\|HINT\|STATEMENT\|CONTEXT)$` | `line` | fg `muted` |  |

## Sample lines

The examples in this gallery are tested against these lines.

```text
2026-09-29 10:01:02.123 UTC [4242] LOG:  checkpoint starting: time
2026-09-29 10:01:02.981 UTC [4242] LOG:  checkpoint complete: wrote 312 buffers (1.9%); 0 WAL file(s) added
2026-09-29 10:01:15.400 UTC [5310] LOG:  duration: 2310.552 ms  statement: SELECT * FROM orders WHERE customer_id = 42
2026-09-29 10:01:20.017 UTC [5311] ERROR:  duplicate key value violates unique constraint "orders_pkey"
2026-09-29 10:01:20.017 UTC [5311] DETAIL:  Key (id)=(1042) already exists.
2026-09-29 10:01:20.017 UTC [5311] STATEMENT:  INSERT INTO orders (id) VALUES (1042)
2026-09-29 10:01:31.760 UTC [5312] WARNING:  there is already a transaction in progress
2026-09-29 10:01:40.002 UTC [5313] FATAL:  terminating connection due to administrator command
```

## The full profile

```toml
# PostgreSQL server log with the prefix "%m [%p] " (the default of
# log_line_prefix in the Debian/Ubuntu packages):
#   2026-09-29 10:01:02.123 UTC [4242] LOG:  checkpoint starting: time
# The severity is followed by a colon and two spaces.
name = "PostgreSQL"
match_files = ["postgresql*.log", "postgres*.log", "pg_log/*.log"]
match_content = '^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d\.\d+ \S+ \[\d+\] (DEBUG\d?|INFO|NOTICE|WARNING|ERROR|LOG|FATAL|PANIC|STATEMENT|DETAIL|HINT|CONTEXT):'

[columns]
parser = "regex"
pattern = '^(?P<time>\d{4}-\d\d-\d\d \d\d:\d\d:\d\d\.\d+) (?P<zone>\S+) \[(?P<pid>\d+)\] (?P<severity>DEBUG\d?|INFO|NOTICE|WARNING|ERROR|LOG|FATAL|PANIC|STATEMENT|DETAIL|HINT|CONTEXT):\s+(?P<message>.*)$'
order = ["time", "pid", "severity", "message", "zone"]
kinds = { severity = "text" }

[timestamp]
column = "time"
format = "iso8601"

[[rules]]
name = "FATAL / PANIC"
match = { column = "severity", op = "regex", value = "^(FATAL|PANIC)$" }
scope = "line"
style = { fg = "fatal", bg = "fatal.subtle", bold = true }
priority = 50

[[rules]]
name = "ERROR"
match = { column = "severity", op = "eq", value = "ERROR" }
scope = "line"
style = { bg = "error.subtle" }
priority = 40

[[rules]]
name = "WARNING"
match = { column = "severity", op = "eq", value = "WARNING" }
scope = "column:severity"
style = { fg = "warn", bold = true }
priority = 30

# log_min_duration_statement lines: "duration: 1234.567 ms  statement: ..."
[[rules]]
name = "slow statements (1 s and more)"
match = { regex = 'duration: (\d{4,}\.\d+ ms)' }
scope = "group:1"
style = { fg = "warn", bold = true }
priority = 25

[[rules]]
name = "deadlock and lock waits"
match = { regex = '(?i)\b(deadlock detected|still waiting for|canceling statement due to)\b' }
scope = "match"
style = { fg = "error", underline = true }
priority = 35
alert = true

[[rules]]
name = "DETAIL / HINT / STATEMENT"
match = { column = "severity", op = "regex", value = "^(DETAIL|HINT|STATEMENT|CONTEXT)$" }
scope = "line"
style = { fg = "muted" }
priority = 5
```

## Using it

Copy the text above to `<data folder>/profiles/postgresql.toml`. OxTail reloads the profiles folder while it runs, so the profile is used for matching files right away, and it can be chosen by name from the status bar. See [Installation and portable mode](../install.md) for where the data folder is. Adjust `match_files` to your own file names.
