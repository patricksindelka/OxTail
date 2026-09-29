# Apache access

Apache httpd access log (common / combined format).

This profile is **built into OxTail**. Its source is `crates/oxtail-config/profiles/apache-access.toml` in the repository.

## Which files it is used for

* File name patterns (`match_files`): `*apache*access*`, `access_log`, `access_log.*`, `*httpd*access*`. A pattern without `/` is compared with the file name, one with `/` with the whole path.
* Content (`match_content`): a regular expression tried on the first lines of the file: `^\S+ \S+ \S+ \[\d{1,2}/\w{3}/\d{4}:\d\d:\d\d:\d\d [+-]\d{4}\] "`.

## Columns and timestamps

Parser: **Apache/Nginx combined** (`parser = "apache_combined"`).

| Column | Kind |
|---|---|
| `remote` | text |
| `ident` | text |
| `user` | text |
| `ts` | timestamp |
| `method` | text |
| `path` | text |
| `proto` | text |
| `status` | number |
| `size` | bytes |
| `referer` | text |
| `agent` | text |

Display order (`order`): `remote`, `ts`, `method`, `path`, `status`, `size`, `*`. `*` stands for all other columns; names the parser does not produce are ignored.

Timestamps: column `ts`, format `apache`. They drive go to time, time gaps and the merged view.

## Highlight rules

Higher priority wins when rules overlap; on a tie the later rule wins. Colours are semantic names resolved by the active theme.

| Priority | Rule | Matches | Scope | Style | Actions |
|---:|---|---|---|---|---|
| -15 | HTTP 5xx | regex ` " (5\d\d)  ` | `group:1` | fg `error`, bold | minimap `error` |
| -15 | HTTP 5xx row | column `status` `ge` `500` | `line` | bg `error.subtle` |  |
| -20 | HTTP 4xx | regex ` " (4\d\d)  ` | `group:1` | fg `warn`, bold |  |
| -30 | HTTP 2xx/3xx | regex ` " ([23]\d\d)  ` | `group:1` | fg `success` |  |
| -40 | timestamp | regex `\[(\d{1,2}/\w{3}/\d{4}:\d\d:\d\d:\d\d [+-]\d{4})\]` | `group:1` | fg `muted` |  |

## The full profile

```toml
# Apache httpd access log (common / combined format).
name = "Apache access"
match_files = ["*apache*access*", "access_log", "access_log.*", "*httpd*access*"]
match_content = '^\S+ \S+ \S+ \[\d{1,2}/\w{3}/\d{4}:\d\d:\d\d:\d\d [+-]\d{4}\] "'

[columns]
parser = "apache_combined"
order = ["remote", "ts", "method", "path", "status", "size", "*"]

[timestamp]
column = "ts"
format = "apache"

# Built-in rules use negative priorities (-40 to -10), so a rule you add
# (priority 0) wins where they overlap.

[[rules]]
name = "timestamp"
match = { regex = '\[(\d{1,2}/\w{3}/\d{4}:\d\d:\d\d:\d\d [+-]\d{4})\]' }
scope = "group:1"
style = { fg = "muted" }
priority = -40

[[rules]]
name = "HTTP 5xx"
match = { regex = '" (5\d\d) ' }
scope = "group:1"
style = { fg = "error", bold = true }
priority = -15
minimap = "error"

[[rules]]
name = "HTTP 4xx"
match = { regex = '" (4\d\d) ' }
scope = "group:1"
style = { fg = "warn", bold = true }
priority = -20

[[rules]]
name = "HTTP 2xx/3xx"
match = { regex = '" ([23]\d\d) ' }
scope = "group:1"
style = { fg = "success" }
priority = -30

[[rules]]
name = "HTTP 5xx row"
match = { column = "status", op = "ge", value = "500" }
scope = "line"
style = { bg = "error.subtle" }
priority = -15
```

## Using it

Nothing to do: it is active already. To customize it, copy the text above to `<data folder>/profiles/apache-access.toml` and edit it. A user profile with the same `name` replaces the built-in one; give it a different `name` to keep both. See [Installation and portable mode](../install.md) for where the data folder is.
