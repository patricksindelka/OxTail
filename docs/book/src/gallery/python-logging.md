# Python logging

Python's logging module with its most common format string:

```text
"%(asctime)s - %(name)s - %(levelname)s - %(message)s"
```

The comma before the milliseconds is what \`asctime\` prints by default.

This is a **gallery example**: it is not built in. Its source is `docs/profiles/python-logging.toml` in the repository; the full text is at the bottom of this page.

## Which files it is used for

* File name patterns (`match_files`): `*.py.log`, `python*.log`, `django*.log`, `flask*.log`, `celery*.log`, `gunicorn*.log`. A pattern without `/` is compared with the file name, one with `/` with the whole path.
* Content (`match_content`): a regular expression tried on the first lines of the file: ` ^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d,\d{3} - [\w.\-]+ - (DEBUG|INFO|WARNING|ERROR|CRITICAL) -  `.

## Columns and timestamps

Parser: **Regex** (`parser = "regex"`).

| Column | Kind |
|---|---|
| `time` | timestamp |
| `logger` | text |
| `level` | level |
| `message` | text |

Display order (`order`): `time`, `level`, `logger`, `message`. `*` stands for all other columns; names the parser does not produce are ignored.

Timestamps: column `time`, format `iso8601_comma`. They drive go to time, time gaps and the merged view.

## Highlight rules

Higher priority wins when rules overlap; on a tie the later rule wins. Colours are semantic names resolved by the active theme.

| Priority | Rule | Matches | Scope | Style | Actions |
|---:|---|---|---|---|---|
| 50 | CRITICAL | column `level` `eq` `critical` | `line` | fg `fatal`, bg `fatal.subtle`, bold |  |
| 40 | ERROR | column `level` `eq` `error` | `line` | bg `error.subtle` |  |
| 30 | WARNING | column `level` `eq` `warning` | `column:level` | fg `warn`, bold |  |
| 20 | Traceback | regex ` ^Traceback \(most recent call last\):\|^\s+File "[^"]+", line \d+\|^\w+(\.\w+)*(Error\|Exception):  ` | `line` | fg `error` |  |
| 10 | DEBUG | column `level` `eq` `debug` | `line` | fg `muted` |  |

## Sample lines

The examples in this gallery are tested against these lines.

```text
2026-09-29 10:01:02,123 - app.main - INFO - starting worker pool with 4 workers
2026-09-29 10:01:02,410 - app.db - DEBUG - opened connection 3 to postgres://db:5432/app
2026-09-29 10:01:03,077 - app.http - WARNING - upstream answered in 1.8 s
2026-09-29 10:01:04,512 - app.http - ERROR - request failed
Traceback (most recent call last):
  File "/srv/app/http.py", line 88, in fetch
    resp = session.get(url, timeout=2)
requests.exceptions.ReadTimeout: HTTPSConnectionPool(host='api', port=443): Read timed out.
2026-09-29 10:01:05,001 - app.main - CRITICAL - giving up after 3 retries
```

## The full profile

```toml
# Python's logging module with its most common format string:
#   "%(asctime)s - %(name)s - %(levelname)s - %(message)s"
# The comma before the milliseconds is what `asctime` prints by default.
name = "Python logging"
match_files = ["*.py.log", "python*.log", "django*.log", "flask*.log", "celery*.log", "gunicorn*.log"]
match_content = '^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d,\d{3} - [\w.\-]+ - (DEBUG|INFO|WARNING|ERROR|CRITICAL) - '

[columns]
parser = "regex"
pattern = '^(?P<time>\d{4}-\d\d-\d\d \d\d:\d\d:\d\d,\d{3}) - (?P<logger>[\w.\-]+) - (?P<level>DEBUG|INFO|WARNING|ERROR|CRITICAL) - (?P<message>.*)$'
order = ["time", "level", "logger", "message"]

[timestamp]
column = "time"
format = "iso8601_comma"

[[rules]]
name = "CRITICAL"
match = { column = "level", op = "eq", value = "critical" }
scope = "line"
style = { fg = "fatal", bg = "fatal.subtle", bold = true }
priority = 50

[[rules]]
name = "ERROR"
match = { column = "level", op = "eq", value = "error" }
scope = "line"
style = { bg = "error.subtle" }
priority = 40

[[rules]]
name = "WARNING"
match = { column = "level", op = "eq", value = "warning" }
scope = "column:level"
style = { fg = "warn", bold = true }
priority = 30

[[rules]]
name = "DEBUG"
match = { column = "level", op = "eq", value = "debug" }
scope = "line"
style = { fg = "muted" }
priority = 10

# Tracebacks are continuation lines: they do not match the parser, so they
# attach to the record above and span all columns.
[[rules]]
name = "Traceback"
match = { regex = '^Traceback \(most recent call last\):|^\s+File "[^"]+", line \d+|^\w+(\.\w+)*(Error|Exception): ' }
scope = "line"
style = { fg = "error" }
priority = 20
```

## Using it

Copy the text above to `<data folder>/profiles/python-logging.toml`. OxTail reloads the profiles folder while it runs, so the profile is used for matching files right away, and it can be chosen by name from the status bar. See [Installation and portable mode](../install.md) for where the data folder is. Adjust `match_files` to your own file names.
