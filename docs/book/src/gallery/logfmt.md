# logfmt

logfmt: key=value pairs (Go services, Heroku, ...).

This profile is **built into OxTail**. Its source is `crates/oxtail-config/profiles/logfmt.toml` in the repository.

## Which files it is used for

* File name patterns (`match_files`): `*.logfmt`. A pattern without `/` is compared with the file name, one with `/` with the whole path.
* Content (`match_content`): a regular expression tried on the first lines of the file: `^(\w[\w.\-]*=("[^"]*"|\S*)\s+){2,}`.

## Columns and timestamps

Parser: **logfmt** (`parser = "logfmt"`).

The columns are the keys found in the first lines of the file (nested JSON keys are flattened with dots); the keys named in `order` come first.

Display order (`order`): `time`, `ts`, `level`, `msg`, `*`. `*` stands for all other columns; names the parser does not produce are ignored.

Timestamps: column `time`, format `auto`. They drive go to time, time gaps and the merged view.

## Highlight rules

Higher priority wins when rules overlap; on a tie the later rule wins. Colours are semantic names resolved by the active theme.

| Priority | Rule | Matches | Scope | Style | Actions |
|---:|---|---|---|---|---|
| -10 | FATAL (text) | regex `\b(?:level\|lvl\|severity)="?(FATAL\|CRITICAL\|CRIT\|PANIC\|EMERG(?:ENCY)?\|ALERT)\b` (ignore case) | `group:1` | fg `error`, bold, underline | minimap `error` |
| -10 | FATAL row | regex `\b(?:level\|lvl\|severity)="?(?:FATAL\|CRITICAL\|CRIT\|PANIC\|EMERG(?:ENCY)?\|ALERT)\b` (ignore case) | `line` | bg `error.subtle` |  |
| -15 | ERROR (text) | regex `\b(?:level\|lvl\|severity)="?(ERROR\|ERR\|SEVERE)\b` (ignore case) | `group:1` | fg `error`, bold | minimap `error` |
| -15 | ERROR row | regex `\b(?:level\|lvl\|severity)="?(?:ERROR\|ERR\|SEVERE)\b` (ignore case) | `line` | bg `error.subtle` |  |
| -20 | WARN (text) | regex `\b(?:level\|lvl\|severity)="?(WARN(?:ING)?)\b` (ignore case) | `group:1` | fg `warn`, bold | minimap `warn` |
| -20 | WARN row | regex `\b(?:level\|lvl\|severity)="?(?:WARN(?:ING)?)\b` (ignore case) | `line` | bg `warn.subtle` |  |
| -25 | slow durations | regex `\bduration=(\d{4,})ms` | `group:1` | fg `warn`, bold |  |
| -30 | INFO (text) | regex `\b(?:level\|lvl\|severity)="?(INFO(?:RMATION)?\|NOTICE)\b` (ignore case) | `group:1` | fg `info` |  |
| -30 | DEBUG (text) | regex `\b(?:level\|lvl\|severity)="?(DEBUG\|DBG)\b` (ignore case) | `group:1` | fg `debug` |  |
| -30 | TRACE (text) | regex `\b(?:level\|lvl\|severity)="?(TRACE\|TRC\|VERBOSE)\b` (ignore case) | `group:1` | fg `trace` |  |
| -40 | keys | regex `(?:^\|\s)([A-Za-z_][\w.\-]*=)` | `group:1` | fg `muted` |  |
| -40 | timestamp | regex `(?:^\|\s)(?:time\|ts\|timestamp)="?([^\s"]+)` | `group:1` | fg `muted` |  |

## The full profile

```toml
# logfmt: key=value pairs (Go services, Heroku, ...).
name = "logfmt"
match_files = ["*.logfmt"]
match_content = '^(\w[\w.\-]*=("[^"]*"|\S*)\s+){2,}'

[columns]
parser = "logfmt"
order = ["time", "ts", "level", "msg", "*"]

[timestamp]
column = "time"
format = "auto"

# Built-in rules use negative priorities (-40 to -10), so a rule you add
# (priority 0) wins where they overlap. Level colours: TRACE, DEBUG and INFO
# use the theme's trace, debug and info colours, WARN warning, ERROR error,
# FATAL error with underline; ERROR and
# FATAL rows get a subtle background; timestamps and keys are muted. In the
# table, level and timestamp cells take the same colours from their column kind.

[[rules]]
name = "keys"
match = { regex = '(?:^|\s)([A-Za-z_][\w.\-]*=)' }
scope = "group:1"
style = { fg = "muted" }
priority = -40

[[rules]]
name = "timestamp"
match = { regex = '(?:^|\s)(?:time|ts|timestamp)="?([^\s"]+)' }
scope = "group:1"
style = { fg = "muted" }
priority = -40

[[rules]]
name = "FATAL (text)"
match = { regex = '\b(?:level|lvl|severity)="?(FATAL|CRITICAL|CRIT|PANIC|EMERG(?:ENCY)?|ALERT)\b', case_sensitive = false }
scope = "group:1"
style = { fg = "error", bold = true, underline = true }
priority = -10
minimap = "error"

[[rules]]
name = "ERROR (text)"
match = { regex = '\b(?:level|lvl|severity)="?(ERROR|ERR|SEVERE)\b', case_sensitive = false }
scope = "group:1"
style = { fg = "error", bold = true }
priority = -15
minimap = "error"

[[rules]]
name = "WARN (text)"
match = { regex = '\b(?:level|lvl|severity)="?(WARN(?:ING)?)\b', case_sensitive = false }
scope = "group:1"
style = { fg = "warn", bold = true }
priority = -20
minimap = "warn"

[[rules]]
name = "INFO (text)"
match = { regex = '\b(?:level|lvl|severity)="?(INFO(?:RMATION)?|NOTICE)\b', case_sensitive = false }
scope = "group:1"
style = { fg = "info" }
priority = -30

[[rules]]
name = "DEBUG (text)"
match = { regex = '\b(?:level|lvl|severity)="?(DEBUG|DBG)\b', case_sensitive = false }
scope = "group:1"
style = { fg = "debug" }
priority = -30

[[rules]]
name = "TRACE (text)"
match = { regex = '\b(?:level|lvl|severity)="?(TRACE|TRC|VERBOSE)\b', case_sensitive = false }
scope = "group:1"
style = { fg = "trace" }
priority = -30

[[rules]]
name = "FATAL row"
match = { regex = '\b(?:level|lvl|severity)="?(?:FATAL|CRITICAL|CRIT|PANIC|EMERG(?:ENCY)?|ALERT)\b', case_sensitive = false }
scope = "line"
style = { bg = "error.subtle" }
priority = -10

[[rules]]
name = "ERROR row"
match = { regex = '\b(?:level|lvl|severity)="?(?:ERROR|ERR|SEVERE)\b', case_sensitive = false }
scope = "line"
style = { bg = "error.subtle" }
priority = -15

[[rules]]
name = "WARN row"
match = { regex = '\b(?:level|lvl|severity)="?(?:WARN(?:ING)?)\b', case_sensitive = false }
scope = "line"
style = { bg = "warn.subtle" }
priority = -20

[[rules]]
name = "slow durations"
match = { regex = '\bduration=(\d{4,})ms' }
scope = "group:1"
style = { fg = "warn", bold = true }
priority = -25
```

## Using it

Nothing to do: it is active already. To customize it, copy the text above to `<data folder>/profiles/logfmt.toml` and edit it. A user profile with the same `name` replaces the built-in one; give it a different `name` to keep both. See [Installation and portable mode](../install.md) for where the data folder is.
