# Nginx access

Nginx access log (common / combined format).

This profile is **built into OxTail**. Its source is `crates/oxtail-config/profiles/nginx-access.toml` in the repository.

## Which files it is used for

* File name patterns (`match_files`): `*nginx*access*.log`, `*nginx*access*.log.*`, `access.log`. A pattern without `/` is compared with the file name, one with `/` with the whole path.
* Content (`match_content`): a regular expression tried on the first lines of the file: `^\S+ \S+ \S+ \[\d{1,2}/\w{3}/\d{4}:\d\d:\d\d:\d\d [+-]\d{4}\] "`.

## Columns and timestamps

Parser: **Apache/Nginx combined** (`parser = "nginx_combined"`).

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
| 30 | HTTP 5xx | regex ` " (5\d\d)  ` | `group:1` | fg `error`, bold |  |
| 20 | HTTP 4xx | regex ` " (4\d\d)  ` | `group:1` | fg `warn` |  |
| 10 | HTTP 2xx/3xx | regex ` " ([23]\d\d)  ` | `group:1` | fg `success` |  |
| 0 | Health checks | regex `"(GET\|HEAD) /(health\|healthz\|ping\|status)[/ ?]` | `line` | fg `muted` |  |

## The full profile

```toml
# Nginx access log (common / combined format).
name = "Nginx access"
match_files = ["*nginx*access*.log", "*nginx*access*.log.*", "access.log"]
match_content = '^\S+ \S+ \S+ \[\d{1,2}/\w{3}/\d{4}:\d\d:\d\d:\d\d [+-]\d{4}\] "'

[columns]
parser = "nginx_combined"
order = ["remote", "ts", "method", "path", "status", "size", "*"]

[timestamp]
column = "ts"
format = "apache"

[[rules]]
name = "HTTP 5xx"
match = { regex = '" (5\d\d) ' }
scope = "group:1"
style = { fg = "error", bold = true }
priority = 30

[[rules]]
name = "HTTP 4xx"
match = { regex = '" (4\d\d) ' }
scope = "group:1"
style = { fg = "warn" }
priority = 20

[[rules]]
name = "HTTP 2xx/3xx"
match = { regex = '" ([23]\d\d) ' }
scope = "group:1"
style = { fg = "success" }
priority = 10

[[rules]]
name = "Health checks"
match = { regex = '"(GET|HEAD) /(health|healthz|ping|status)[/ ?]' }
scope = "line"
style = { fg = "muted" }
priority = 0
hide = false
```

## Using it

Nothing to do: it is active already. To customize it, copy the text above to `<data folder>/profiles/nginx-access.toml` and edit it. A user profile with the same `name` replaces the built-in one; give it a different `name` to keep both. See [Installation and portable mode](../install.md) for where the data folder is.
