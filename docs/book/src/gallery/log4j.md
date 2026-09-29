# Java / log4j

log4j / Log4j 2 / logback / Python logging style, e.g.

```text
2026-09-29 10:01:02,123 INFO message                       (%d %-5p %m%n)
2026-09-22 14:42:31.962 [main] INFO  app.Db - message      (%d [%t] %-5level %logger - %msg%n,
the Log4j 2 and logback default with a date)
```

The thread and logger columns are empty when a layout has none. Other layouts: use parser = "log4j" with your own PatternLayout as \`pattern\`.

This profile is **built into OxTail**. Its source is `crates/oxtail-config/profiles/log4j.toml` in the repository.

## Which files it is used for

* File name patterns (`match_files`): `*log4j*`, `catalina*.out`, `catalina*.log`, `server.log`, `application.log`, `spring*.log`. A pattern without `/` is compared with the file name, one with `/` with the whole path.
* Content (`match_content`): a regular expression tried on the first lines of the file: `^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+\s+(?:\[[^\]]*\]\s+)?\[?(TRACE|DEBUG|INFO|WARN|WARNING|ERROR|FATAL)\b`.

## Columns and timestamps

Parser: **Regex** (`parser = "regex"`).

| Column | Kind |
|---|---|
| `time` | timestamp |
| `thread` | text |
| `level` | level |
| `logger` | text |
| `message` | text |

Display order (`order`): `time`, `level`, `thread`, `logger`, `message`. `*` stands for all other columns; names the parser does not produce are ignored.

Timestamps: column `time`, format `iso8601`. They drive go to time, time gaps and the merged view.

## Highlight rules

Higher priority wins when rules overlap; on a tie the later rule wins. Colours are semantic names resolved by the active theme.

| Priority | Rule | Matches | Scope | Style | Actions |
|---:|---|---|---|---|---|
| 50 | FATAL | regex `\bFATAL\b` | `line` | fg `fatal`, bg `fatal.subtle`, bold |  |
| 40 | ERROR | regex `\bERROR\b` | `line` | fg `error`, bg `error.subtle` |  |
| 30 | WARN | regex `\bWARN(ING)?\b` | `match` | fg `warn`, bold |  |
| 10 | Stack trace | regex ` ^\s+at [\w$.]+\(.*\)$\|^Caused by:  ` | `line` | fg `muted` |  |

## The full profile

```toml
# log4j / Log4j 2 / logback / Python logging style, e.g.
#   2026-09-29 10:01:02,123 INFO message                       (%d %-5p %m%n)
#   2026-09-22 14:42:31.962 [main] INFO  app.Db - message      (%d [%t] %-5level %logger - %msg%n,
#                                                               the Log4j 2 and logback default with a date)
# The thread and logger columns are empty when a layout has none. Other
# layouts: use parser = "log4j" with your own PatternLayout as `pattern`.
name = "Java / log4j"
match_files = ["*log4j*", "catalina*.out", "catalina*.log", "server.log", "application.log", "spring*.log"]
match_content = '^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+\s+(?:\[[^\]]*\]\s+)?\[?(TRACE|DEBUG|INFO|WARN|WARNING|ERROR|FATAL)\b'

[columns]
parser = "regex"
pattern = '^(?P<time>\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+)\s+(?:\[(?P<thread>[^\]]*)\]\s+)?\[?(?P<level>TRACE|DEBUG|INFO|WARN|WARNING|ERROR|FATAL)\]?\s+(?:(?P<logger>[\w$]+(?:\.[\w$]+)*)\s+-\s+)?(?P<message>.*)$'
order = ["time", "level", "thread", "logger", "message"]

[timestamp]
column = "time"
format = "iso8601"

[[rules]]
name = "FATAL"
match = { regex = '\bFATAL\b' }
scope = "line"
style = { fg = "fatal", bg = "fatal.subtle", bold = true }
priority = 50

[[rules]]
name = "ERROR"
match = { regex = '\bERROR\b' }
scope = "line"
style = { fg = "error", bg = "error.subtle" }
priority = 40

[[rules]]
name = "WARN"
match = { regex = '\bWARN(ING)?\b' }
scope = "match"
style = { fg = "warn", bold = true }
priority = 30

[[rules]]
name = "Stack trace"
match = { regex = '^\s+at [\w$.]+\(.*\)$|^Caused by: ' }
scope = "line"
style = { fg = "muted" }
priority = 10
```

## Using it

Nothing to do: it is active already. To customize it, copy the text above to `<data folder>/profiles/log4j.toml` and edit it. A user profile with the same `name` replaces the built-in one; give it a different `name` to keep both. See [Installation and portable mode](../install.md) for where the data folder is.
