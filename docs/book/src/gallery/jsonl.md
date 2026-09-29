# JSON lines

JSON Lines: one JSON object per line.

This profile is **built into OxTail**. Its source is `crates/oxtail-config/profiles/jsonl.toml` in the repository.

## Which files it is used for

* File name patterns (`match_files`): `*.jsonl`, `*.ndjson`, `*.jsonl.*`, `*.ndjson.*`. A pattern without `/` is compared with the file name, one with `/` with the whole path.
* Content (`match_content`): a regular expression tried on the first lines of the file: `^\s*\{\s*"`.

## Columns and timestamps

Parser: **JSON Lines** (`parser = "jsonl"`).

The columns are the keys found in the first lines of the file (nested JSON keys are flattened with dots); the keys named in `order` come first.

Display order (`order`): `time`, `timestamp`, `ts`, `level`, `msg`, `message`, `*`. `*` stands for all other columns; names the parser does not produce are ignored.

Timestamps: column `time`, format `auto`. They drive go to time, time gaps and the merged view.

## Highlight rules

Higher priority wins when rules overlap; on a tie the later rule wins. Colours are semantic names resolved by the active theme.

| Priority | Rule | Matches | Scope | Style | Actions |
|---:|---|---|---|---|---|
| -10 | FATAL (text) | regex `"(?:level\|lvl\|severity)"\s*:\s*"(FATAL\|CRITICAL\|CRIT\|PANIC\|EMERG(?:ENCY)?\|ALERT)"` (ignore case) | `group:1` | fg `error`, bold, underline | minimap `error` |
| -10 | FATAL row | regex `"(?:level\|lvl\|severity)"\s*:\s*"(?:FATAL\|CRITICAL\|CRIT\|PANIC\|EMERG(?:ENCY)?\|ALERT)"` (ignore case) | `line` | bg `error.subtle` |  |
| -15 | ERROR (text) | regex `"(?:level\|lvl\|severity)"\s*:\s*"(ERROR\|ERR\|SEVERE)"` (ignore case) | `group:1` | fg `error`, bold | minimap `error` |
| -15 | ERROR row | regex `"(?:level\|lvl\|severity)"\s*:\s*"(?:ERROR\|ERR\|SEVERE)"` (ignore case) | `line` | bg `error.subtle` |  |
| -20 | WARN (text) | regex `"(?:level\|lvl\|severity)"\s*:\s*"(WARN(?:ING)?)"` (ignore case) | `group:1` | fg `warn`, bold | minimap `warn` |
| -20 | WARN row | regex `"(?:level\|lvl\|severity)"\s*:\s*"(?:WARN(?:ING)?)"` (ignore case) | `line` | bg `warn.subtle` |  |
| -30 | INFO (text) | regex `"(?:level\|lvl\|severity)"\s*:\s*"(INFO(?:RMATION)?\|NOTICE)"` (ignore case) | `group:1` | fg `info` |  |
| -30 | DEBUG (text) | regex `"(?:level\|lvl\|severity)"\s*:\s*"(DEBUG\|DBG)"` (ignore case) | `group:1` | fg `debug` |  |
| -30 | TRACE (text) | regex `"(?:level\|lvl\|severity)"\s*:\s*"(TRACE\|TRC\|VERBOSE)"` (ignore case) | `group:1` | fg `trace` |  |
| -40 | keys | regex `"[A-Za-z_@][\w.\-@]*"\s*:` | `match` | fg `muted` |  |

## The full profile

```toml
# JSON Lines: one JSON object per line.
name = "JSON lines"
match_files = ["*.jsonl", "*.ndjson", "*.jsonl.*", "*.ndjson.*"]
match_content = '^\s*\{\s*"'

[columns]
parser = "jsonl"
order = ["time", "timestamp", "ts", "level", "msg", "message", "*"]

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
match = { regex = '"[A-Za-z_@][\w.\-@]*"\s*:' }
scope = "match"
style = { fg = "muted" }
priority = -40

[[rules]]
name = "FATAL (text)"
match = { regex = '"(?:level|lvl|severity)"\s*:\s*"(FATAL|CRITICAL|CRIT|PANIC|EMERG(?:ENCY)?|ALERT)"', case_sensitive = false }
scope = "group:1"
style = { fg = "error", bold = true, underline = true }
priority = -10
minimap = "error"

[[rules]]
name = "ERROR (text)"
match = { regex = '"(?:level|lvl|severity)"\s*:\s*"(ERROR|ERR|SEVERE)"', case_sensitive = false }
scope = "group:1"
style = { fg = "error", bold = true }
priority = -15
minimap = "error"

[[rules]]
name = "WARN (text)"
match = { regex = '"(?:level|lvl|severity)"\s*:\s*"(WARN(?:ING)?)"', case_sensitive = false }
scope = "group:1"
style = { fg = "warn", bold = true }
priority = -20
minimap = "warn"

[[rules]]
name = "INFO (text)"
match = { regex = '"(?:level|lvl|severity)"\s*:\s*"(INFO(?:RMATION)?|NOTICE)"', case_sensitive = false }
scope = "group:1"
style = { fg = "info" }
priority = -30

[[rules]]
name = "DEBUG (text)"
match = { regex = '"(?:level|lvl|severity)"\s*:\s*"(DEBUG|DBG)"', case_sensitive = false }
scope = "group:1"
style = { fg = "debug" }
priority = -30

[[rules]]
name = "TRACE (text)"
match = { regex = '"(?:level|lvl|severity)"\s*:\s*"(TRACE|TRC|VERBOSE)"', case_sensitive = false }
scope = "group:1"
style = { fg = "trace" }
priority = -30

[[rules]]
name = "FATAL row"
match = { regex = '"(?:level|lvl|severity)"\s*:\s*"(?:FATAL|CRITICAL|CRIT|PANIC|EMERG(?:ENCY)?|ALERT)"', case_sensitive = false }
scope = "line"
style = { bg = "error.subtle" }
priority = -10

[[rules]]
name = "ERROR row"
match = { regex = '"(?:level|lvl|severity)"\s*:\s*"(?:ERROR|ERR|SEVERE)"', case_sensitive = false }
scope = "line"
style = { bg = "error.subtle" }
priority = -15

[[rules]]
name = "WARN row"
match = { regex = '"(?:level|lvl|severity)"\s*:\s*"(?:WARN(?:ING)?)"', case_sensitive = false }
scope = "line"
style = { bg = "warn.subtle" }
priority = -20
```

## Using it

Nothing to do: it is active already. To customize it, copy the text above to `<data folder>/profiles/jsonl.toml` and edit it. A user profile with the same `name` replaces the built-in one; give it a different `name` to keep both. See [Installation and portable mode](../install.md) for where the data folder is.
