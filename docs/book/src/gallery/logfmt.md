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
| 20 | error level | column `level` `eq` `error` | `line` | bg `error.subtle` |  |
| 10 | warn level | column `level` `eq` `warn` | `line` | bg `warn.subtle` |  |
| 5 | slow durations | regex `\bduration=(\d{4,})ms` | `group:1` | fg `warn`, bold |  |

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

[[rules]]
name = "error level"
match = { column = "level", op = "eq", value = "error" }
scope = "line"
style = { bg = "error.subtle" }
priority = 20

[[rules]]
name = "warn level"
match = { column = "level", op = "eq", value = "warn" }
scope = "line"
style = { bg = "warn.subtle" }
priority = 10

[[rules]]
name = "slow durations"
match = { regex = '\bduration=(\d{4,})ms' }
scope = "group:1"
style = { fg = "warn", bold = true }
priority = 5
```

## Using it

Nothing to do: it is active already. To customize it, copy the text above to `<data folder>/profiles/logfmt.toml` and edit it. A user profile with the same `name` replaces the built-in one; give it a different `name` to keep both. See [Installation and portable mode](../install.md) for where the data folder is.
