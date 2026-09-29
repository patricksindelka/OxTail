# IIS W3C

IIS / W3C extended log format (columns come from the #Fields: directive).

This profile is **built into OxTail**. Its source is `crates/oxtail-config/profiles/iis-w3c.toml` in the repository.

## Which files it is used for

* File name patterns (`match_files`): `u_ex*.log`, `*iis*.log`, `W3SVC*.log`. A pattern without `/` is compared with the file name, one with `/` with the whole path.
* Content (`match_content`): a regular expression tried on the first lines of the file: `^#(Software|Version|Fields):`.

## Columns and timestamps

Parser: **W3C / IIS** (`parser = "w3c"`).

The column names come from the file's `#Fields:` directive, so they differ per file.

Display order (`order`): `date`, `time`, `*`. `*` stands for all other columns; names the parser does not produce are ignored.

Timestamps: the first timestamp on each line, format `auto`. They drive go to time, time gaps and the merged view.

## Highlight rules

Higher priority wins when rules overlap; on a tie the later rule wins. Colours are semantic names resolved by the active theme.

| Priority | Rule | Matches | Scope | Style | Actions |
|---:|---|---|---|---|---|
| 20 | HTTP 5xx | column `sc-status` `ge` `500` | `line` | bg `error.subtle` |  |
| 0 | Directives | regex `^#` | `line` | fg `muted` |  |

## The full profile

```toml
# IIS / W3C extended log format (columns come from the #Fields: directive).
name = "IIS W3C"
match_files = ["u_ex*.log", "*iis*.log", "W3SVC*.log"]
match_content = '^#(Software|Version|Fields):'

[columns]
parser = "w3c"
order = ["date", "time", "*"]

[timestamp]
format = "auto"

[[rules]]
name = "Directives"
match = { regex = '^#' }
scope = "line"
style = { fg = "muted" }
priority = 0

[[rules]]
name = "HTTP 5xx"
match = { column = "sc-status", op = "ge", value = "500" }
scope = "line"
style = { bg = "error.subtle" }
priority = 20
```

## Using it

Nothing to do: it is active already. To customize it, copy the text above to `<data folder>/profiles/iis-w3c.toml` and edit it. A user profile with the same `name` replaces the built-in one; give it a different `name` to keep both. See [Installation and portable mode](../install.md) for where the data folder is.
