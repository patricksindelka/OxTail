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
| 20 | error level | column `level` `eq` `error` | `line` | bg `error.subtle` |  |
| 10 | warn level | column `level` `eq` `warn` | `line` | bg `warn.subtle` |  |

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

[[rules]]
name = "error level"
match = { column = "level", op = "eq", value = "error" }
scope = "line"
style = { bg = "error.subtle" }
priority = 20
alert = false

[[rules]]
name = "warn level"
match = { column = "level", op = "eq", value = "warn" }
scope = "line"
style = { bg = "warn.subtle" }
priority = 10
```

## Using it

Nothing to do: it is active already. To customize it, copy the text above to `<data folder>/profiles/jsonl.toml` and edit it. A user profile with the same `name` replaces the built-in one; give it a different `name` to keep both. See [Installation and portable mode](../install.md) for where the data folder is.
