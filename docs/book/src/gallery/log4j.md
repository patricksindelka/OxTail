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
* Content (`match_content`): a regular expression tried on the first lines of the file: `^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z|[+-]\d\d:?\d\d)?\s+(?:\[[^\]]*\]\s+)?\[?(TRACE|DEBUG|INFO|WARN|WARNING|ERROR|FATAL)\b`.

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
| -10 | FATAL (text) | regex `^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z\|[+-]\d\d:?\d\d)?\s+(?:\[[^\]]*\]\s+)?\[?(FATAL\|CRITICAL\|CRIT\|PANIC\|EMERG(?:ENCY)?\|ALERT)\b` | `group:1` | fg `error`, bold, underline | minimap `error` |
| -10 | FATAL row | regex `^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z\|[+-]\d\d:?\d\d)?\s+(?:\[[^\]]*\]\s+)?\[?(?:FATAL\|CRITICAL\|CRIT\|PANIC\|EMERG(?:ENCY)?\|ALERT)\b` | `line` | bg `error.subtle` |  |
| -11 | FATAL (fallback) | regex `^(?:[^A-Z]\|[A-Z]{1,2}[^A-Z]){0,80}?\b(FATAL\|CRITICAL\|PANIC)\b` | `group:1` | fg `error`, bold, underline | minimap `error` |
| -11 | FATAL row (fallback) | regex `^(?:[^A-Z]\|[A-Z]{1,2}[^A-Z]){0,80}?\b(?:FATAL\|CRITICAL\|PANIC)\b` | `line` | bg `error.subtle` |  |
| -15 | ERROR (text) | regex `^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z\|[+-]\d\d:?\d\d)?\s+(?:\[[^\]]*\]\s+)?\[?(ERROR\|ERR\|SEVERE)\b` | `group:1` | fg `error`, bold | minimap `error` |
| -15 | ERROR row | regex `^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z\|[+-]\d\d:?\d\d)?\s+(?:\[[^\]]*\]\s+)?\[?(?:ERROR\|ERR\|SEVERE)\b` | `line` | bg `error.subtle` |  |
| -16 | ERROR (fallback) | regex `^(?:[^A-Z]\|[A-Z]{1,2}[^A-Z]){0,80}?\b(ERROR\|SEVERE)\b` | `group:1` | fg `error`, bold | minimap `error` |
| -16 | ERROR row (fallback) | regex `^(?:[^A-Z]\|[A-Z]{1,2}[^A-Z]){0,80}?\b(?:ERROR\|SEVERE)\b` | `line` | bg `error.subtle` |  |
| -20 | WARN (text) | regex `^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z\|[+-]\d\d:?\d\d)?\s+(?:\[[^\]]*\]\s+)?\[?(WARN(?:ING)?)\b` | `group:1` | fg `warn`, bold | minimap `warn` |
| -21 | WARN (fallback) | regex `^(?:[^A-Z]\|[A-Z]{1,2}[^A-Z]){0,80}?\b(WARN(?:ING)?)\b` | `group:1` | fg `warn`, bold | minimap `warn` |
| -30 | INFO (text) | regex `^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z\|[+-]\d\d:?\d\d)?\s+(?:\[[^\]]*\]\s+)?\[?(INFO(?:RMATION)?\|NOTICE)\b` | `group:1` | fg `info` |  |
| -30 | DEBUG (text) | regex `^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z\|[+-]\d\d:?\d\d)?\s+(?:\[[^\]]*\]\s+)?\[?(DEBUG\|DBG)\b` | `group:1` | fg `debug` |  |
| -30 | TRACE (text) | regex `^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z\|[+-]\d\d:?\d\d)?\s+(?:\[[^\]]*\]\s+)?\[?(TRACE\|TRC\|VERBOSE)\b` | `group:1` | fg `trace` |  |
| -35 | Stack trace | regex ` ^\s+at [\w$.]+\(.*\)$\|^Caused by:  ` | `line` | fg `muted` |  |
| -40 | timestamp | regex `^(\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z\|[+-]\d\d:?\d\d)?)` | `group:1` | fg `muted` |  |

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
match_content = '^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z|[+-]\d\d:?\d\d)?\s+(?:\[[^\]]*\]\s+)?\[?(TRACE|DEBUG|INFO|WARN|WARNING|ERROR|FATAL)\b'

[columns]
parser = "regex"
pattern = '^(?P<time>\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z|[+-]\d\d:?\d\d)?)\s+(?:\[(?P<thread>[^\]]*)\]\s+)?\[?(?P<level>TRACE|DEBUG|INFO|WARN|WARNING|ERROR|FATAL)\]?\s+(?:(?P<logger>[\w$]+(?:\.[\w$]+)*)\s+-\s+)?(?P<message>.*)$'
order = ["time", "level", "thread", "logger", "message"]

[timestamp]
column = "time"
format = "iso8601"

# Layouts the anchored rules do not cover (Tomcat's `29-Sep-2026 10:01:02.123
# WARNING [main]`, Python's `... - ERROR - ...`) still get error/warn colours
# from the "(fallback)" rules below: a level word within the first 80
# characters, with no other capitalised word (INFO, DEBUG...) before it.
#
# Built-in rules use negative priorities (-40 to -10), so a rule you add
# (priority 0) wins where they overlap. Level colours: TRACE, DEBUG and INFO
# use the theme's trace, debug and info colours, WARN warning, ERROR error,
# FATAL error with underline; ERROR and
# FATAL rows get a subtle background; timestamps and keys are muted. In the
# table, level and timestamp cells take the same colours from their column kind.

[[rules]]
name = "timestamp"
match = { regex = '^(\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z|[+-]\d\d:?\d\d)?)' }
scope = "group:1"
style = { fg = "muted" }
priority = -40

[[rules]]
name = "FATAL (text)"
match = { regex = '^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z|[+-]\d\d:?\d\d)?\s+(?:\[[^\]]*\]\s+)?\[?(FATAL|CRITICAL|CRIT|PANIC|EMERG(?:ENCY)?|ALERT)\b' }
scope = "group:1"
style = { fg = "error", bold = true, underline = true }
priority = -10
minimap = "error"

[[rules]]
name = "ERROR (text)"
match = { regex = '^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z|[+-]\d\d:?\d\d)?\s+(?:\[[^\]]*\]\s+)?\[?(ERROR|ERR|SEVERE)\b' }
scope = "group:1"
style = { fg = "error", bold = true }
priority = -15
minimap = "error"

[[rules]]
name = "WARN (text)"
match = { regex = '^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z|[+-]\d\d:?\d\d)?\s+(?:\[[^\]]*\]\s+)?\[?(WARN(?:ING)?)\b' }
scope = "group:1"
style = { fg = "warn", bold = true }
priority = -20
minimap = "warn"

[[rules]]
name = "INFO (text)"
match = { regex = '^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z|[+-]\d\d:?\d\d)?\s+(?:\[[^\]]*\]\s+)?\[?(INFO(?:RMATION)?|NOTICE)\b' }
scope = "group:1"
style = { fg = "info" }
priority = -30

[[rules]]
name = "DEBUG (text)"
match = { regex = '^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z|[+-]\d\d:?\d\d)?\s+(?:\[[^\]]*\]\s+)?\[?(DEBUG|DBG)\b' }
scope = "group:1"
style = { fg = "debug" }
priority = -30

[[rules]]
name = "TRACE (text)"
match = { regex = '^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z|[+-]\d\d:?\d\d)?\s+(?:\[[^\]]*\]\s+)?\[?(TRACE|TRC|VERBOSE)\b' }
scope = "group:1"
style = { fg = "trace" }
priority = -30

[[rules]]
name = "FATAL row"
match = { regex = '^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z|[+-]\d\d:?\d\d)?\s+(?:\[[^\]]*\]\s+)?\[?(?:FATAL|CRITICAL|CRIT|PANIC|EMERG(?:ENCY)?|ALERT)\b' }
scope = "line"
style = { bg = "error.subtle" }
priority = -10

[[rules]]
name = "ERROR row"
match = { regex = '^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d[,.]\d+(?:Z|[+-]\d\d:?\d\d)?\s+(?:\[[^\]]*\]\s+)?\[?(?:ERROR|ERR|SEVERE)\b' }
scope = "line"
style = { bg = "error.subtle" }
priority = -15

[[rules]]
name = "Stack trace"
match = { regex = '^\s+at [\w$.]+\(.*\)$|^Caused by: ' }
scope = "line"
style = { fg = "muted" }
priority = -35

[[rules]]
name = "FATAL (fallback)"
match = { regex = '^(?:[^A-Z]|[A-Z]{1,2}[^A-Z]){0,80}?\b(FATAL|CRITICAL|PANIC)\b' }
scope = "group:1"
style = { fg = "error", bold = true, underline = true }
priority = -11
minimap = "error"

[[rules]]
name = "ERROR (fallback)"
match = { regex = '^(?:[^A-Z]|[A-Z]{1,2}[^A-Z]){0,80}?\b(ERROR|SEVERE)\b' }
scope = "group:1"
style = { fg = "error", bold = true }
priority = -16
minimap = "error"

[[rules]]
name = "WARN (fallback)"
match = { regex = '^(?:[^A-Z]|[A-Z]{1,2}[^A-Z]){0,80}?\b(WARN(?:ING)?)\b' }
scope = "group:1"
style = { fg = "warn", bold = true }
priority = -21
minimap = "warn"

[[rules]]
name = "FATAL row (fallback)"
match = { regex = '^(?:[^A-Z]|[A-Z]{1,2}[^A-Z]){0,80}?\b(?:FATAL|CRITICAL|PANIC)\b' }
scope = "line"
style = { bg = "error.subtle" }
priority = -11

[[rules]]
name = "ERROR row (fallback)"
match = { regex = '^(?:[^A-Z]|[A-Z]{1,2}[^A-Z]){0,80}?\b(?:ERROR|SEVERE)\b' }
scope = "line"
style = { bg = "error.subtle" }
priority = -16
```

## Using it

Nothing to do: it is active already. To customize it, copy the text above to `<data folder>/profiles/log4j.toml` and edit it. A user profile with the same `name` replaces the built-in one; give it a different `name` to keep both. See [Installation and portable mode](../install.md) for where the data folder is.
