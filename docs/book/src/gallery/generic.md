# Generic

Fallback for any text log: log levels. Never auto-selected; the application uses it by name when nothing else matches.

This profile is **built into OxTail**. Its source is `crates/oxtail-config/profiles/generic.toml` in the repository.

## Which files it is used for

The profile has no `match_files` and no `match_content`, so OxTail never selects it automatically. Pick it by name from the profile menu in the status bar or with `--profile`.


## Columns and timestamps

This profile defines no `[columns]`. OxTail still offers to detect a parser from the file contents.

Timestamps: the first timestamp on each line, format `auto`. They drive go to time, time gaps and the merged view.

## Highlight rules

Higher priority wins when rules overlap; on a tie the later rule wins. Colours are semantic names resolved by the active theme.

| Priority | Rule | Matches | Scope | Style | Actions |
|---:|---|---|---|---|---|
| -10 | FATAL | regex `\b(FATAL\|CRITICAL\|PANIC\|EMERG(?:ENCY)?)\b` | `group:1` | fg `error`, bold, underline | minimap `error` |
| -10 | FATAL row | regex `^.{0,80}?\b(FATAL\|CRITICAL\|PANIC\|EMERG(?:ENCY)?)\b` | `line` | bg `error.subtle` |  |
| -15 | ERROR | regex `\b(ERROR\|ERR\|SEVERE\|Exception)\b` | `group:1` | fg `error`, bold | minimap `error` |
| -15 | ERROR row | regex `^.{0,80}?\b(ERROR\|ERR\|SEVERE)\b` | `line` | bg `error.subtle` |  |
| -20 | WARN | regex `\b(WARN(?:ING)?)\b` | `group:1` | fg `warn`, bold | minimap `warn` |
| -30 | INFO | regex `\b(INFO(?:RMATION)?\|NOTICE)\b` | `group:1` | fg `info` |  |
| -30 | DEBUG | regex `\b(DEBUG\|DBG)\b` | `group:1` | fg `debug` |  |
| -30 | TRACE | regex `\b(TRACE\|TRC\|VERBOSE)\b` | `group:1` | fg `trace` |  |
| -40 | timestamp | regex `^\s*\[?(\d{4}-\d\d-\d\d[T ]\d\d:\d\d:\d\d(?:[.,]\d+)?(?:Z\|[+-]\d\d:?\d\d)?\|[A-Z][a-z]{2} [ \d]\d \d\d:\d\d:\d\d\|\d\d:\d\d:\d\d(?:[.,]\d+)?)` | `group:1` | fg `muted` |  |

## The full profile

```toml
# Fallback for any text log: log levels. Never auto-selected; the application
# uses it by name when nothing else matches.
name = "Generic"

[timestamp]
format = "auto"

# Built-in rules use negative priorities (-40 to -10), so a rule you add
# (priority 0) wins where they overlap. Level colours: TRACE, DEBUG and INFO
# use the theme's trace, debug and info colours, WARN warning, ERROR error,
# FATAL error with underline; ERROR and
# FATAL rows get a subtle background; timestamps and keys are muted. In the
# table, level and timestamp cells take the same colours from their column kind.

[[rules]]
name = "timestamp"
match = { regex = '^\s*\[?(\d{4}-\d\d-\d\d[T ]\d\d:\d\d:\d\d(?:[.,]\d+)?(?:Z|[+-]\d\d:?\d\d)?|[A-Z][a-z]{2} [ \d]\d \d\d:\d\d:\d\d|\d\d:\d\d:\d\d(?:[.,]\d+)?)' }
scope = "group:1"
style = { fg = "muted" }
priority = -40

[[rules]]
name = "FATAL"
match = { regex = '\b(FATAL|CRITICAL|PANIC|EMERG(?:ENCY)?)\b' }
scope = "group:1"
style = { fg = "error", bold = true, underline = true }
priority = -10
minimap = "error"

[[rules]]
name = "ERROR"
match = { regex = '\b(ERROR|ERR|SEVERE|Exception)\b' }
scope = "group:1"
style = { fg = "error", bold = true }
priority = -15
minimap = "error"

[[rules]]
name = "WARN"
match = { regex = '\b(WARN(?:ING)?)\b' }
scope = "group:1"
style = { fg = "warn", bold = true }
priority = -20
minimap = "warn"

[[rules]]
name = "INFO"
match = { regex = '\b(INFO(?:RMATION)?|NOTICE)\b' }
scope = "group:1"
style = { fg = "info" }
priority = -30

[[rules]]
name = "DEBUG"
match = { regex = '\b(DEBUG|DBG)\b' }
scope = "group:1"
style = { fg = "debug" }
priority = -30

[[rules]]
name = "TRACE"
match = { regex = '\b(TRACE|TRC|VERBOSE)\b' }
scope = "group:1"
style = { fg = "trace" }
priority = -30

[[rules]]
name = "FATAL row"
match = { regex = '^.{0,80}?\b(FATAL|CRITICAL|PANIC|EMERG(?:ENCY)?)\b' }
scope = "line"
style = { bg = "error.subtle" }
priority = -10

[[rules]]
name = "ERROR row"
match = { regex = '^.{0,80}?\b(ERROR|ERR|SEVERE)\b' }
scope = "line"
style = { bg = "error.subtle" }
priority = -15
```

## Using it

Nothing to do: it is active already. To customize it, copy the text above to `<data folder>/profiles/generic.toml` and edit it. A user profile with the same `name` replaces the built-in one; give it a different `name` to keep both. See [Installation and portable mode](../install.md) for where the data folder is.
