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
| 50 | FATAL / CRITICAL | regex `\b(FATAL\|CRITICAL\|PANIC)\b` | `match` | fg `fatal`, bold |  |
| 40 | ERROR | regex `\b(ERROR\|ERR\|SEVERE\|Exception)\b` | `match` | fg `error`, bold |  |
| 30 | WARN | regex `\b(WARN\|WARNING)\b` | `match` | fg `warn`, bold |  |
| 20 | INFO | regex `\bINFO\b` | `match` | fg `info` |  |
| 10 | DEBUG / TRACE | regex `\b(DEBUG\|TRACE)\b` | `match` | fg `muted` |  |

## The full profile

```toml
# Fallback for any text log: log levels. Never auto-selected; the application
# uses it by name when nothing else matches.
name = "Generic"

[timestamp]
format = "auto"

[[rules]]
name = "FATAL / CRITICAL"
match = { regex = '\b(FATAL|CRITICAL|PANIC)\b' }
scope = "match"
style = { fg = "fatal", bold = true }
priority = 50

[[rules]]
name = "ERROR"
match = { regex = '\b(ERROR|ERR|SEVERE|Exception)\b' }
scope = "match"
style = { fg = "error", bold = true }
priority = 40

[[rules]]
name = "WARN"
match = { regex = '\b(WARN|WARNING)\b' }
scope = "match"
style = { fg = "warn", bold = true }
priority = 30

[[rules]]
name = "INFO"
match = { regex = '\bINFO\b' }
scope = "match"
style = { fg = "info" }
priority = 20

[[rules]]
name = "DEBUG / TRACE"
match = { regex = '\b(DEBUG|TRACE)\b' }
scope = "match"
style = { fg = "muted" }
priority = 10
```

## Using it

Nothing to do: it is active already. To customize it, copy the text above to `<data folder>/profiles/generic.toml` and edit it. A user profile with the same `name` replaces the built-in one; give it a different `name` to keep both. See [Installation and portable mode](../install.md) for where the data folder is.
