# Syslog

Syslog, RFC 3164 (BSD) and RFC 5424.

This profile is **built into OxTail**. Its source is `crates/oxtail-config/profiles/syslog.toml` in the repository.

## Which files it is used for

* File name patterns (`match_files`): `syslog`, `syslog.*`, `messages`, `messages.*`, `auth.log`, `auth.log.*`, `kern.log`, `daemon.log`, `*.syslog`. A pattern without `/` is compared with the file name, one with `/` with the whole path.
* Content (`match_content`): a regular expression tried on the first lines of the file: ` ^(<\d{1,3}>(1 )?)?(\w{3} [ \d]\d \d\d:\d\d:\d\d|\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\S*) \S+  `.

## Columns and timestamps

Parser: **Syslog (RFC 3164)** (`parser = "syslog"`).

Syslog (RFC 3164):

| Column | Kind |
|---|---|
| `pri` | text |
| `ts` | timestamp |
| `host` | text |
| `tag` | text |
| `pid` | number |
| `msg` | text |

Syslog (RFC 5424):

| Column | Kind |
|---|---|
| `pri` | text |
| `version` | text |
| `ts` | timestamp |
| `host` | text |
| `app` | text |
| `procid` | text |
| `msgid` | text |
| `sd` | text |
| `msg` | text |

Display order (`order`): `ts`, `host`, `app`, `tag`, `pid`, `procid`, `msg`. `*` stands for all other columns; names the parser does not produce are ignored.

Timestamps: the first timestamp on each line, format `auto`. They drive go to time, time gaps and the merged view.

## Highlight rules

Higher priority wins when rules overlap; on a tie the later rule wins. Colours are semantic names resolved by the active theme.

| Priority | Rule | Matches | Scope | Style | Actions |
|---:|---|---|---|---|---|
| -15 | Errors | regex `(?i)\b(error\|fail(ed\|ure)?\|denied\|refused\|panic\|fatal\|segfault)\b` | `match` | fg `error`, bold | minimap `error` |
| -20 | Warnings | regex `(?i)\b(warn(ing)?\|timeout\|retry)\b` | `match` | fg `warn`, bold | minimap `warn` |
| -40 | timestamp | regex `^(?:<\d{1,3}>(?:1 )?)?(\w{3} [ \d]\d \d\d:\d\d:\d\d\|\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\S*)` | `group:1` | fg `muted` |  |

## The full profile

```toml
# Syslog, RFC 3164 (BSD) and RFC 5424.
name = "Syslog"
match_files = ["syslog", "syslog.*", "messages", "messages.*", "auth.log", "auth.log.*", "kern.log", "daemon.log", "*.syslog"]
match_content = '^(<\d{1,3}>(1 )?)?(\w{3} [ \d]\d \d\d:\d\d:\d\d|\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\S*) \S+ '

[columns]
parser = "syslog"
order = ["ts", "host", "app", "tag", "pid", "procid", "msg"]

[timestamp]
format = "auto"

# Built-in rules use negative priorities (-40 to -10), so a rule you add
# (priority 0) wins where they overlap.

[[rules]]
name = "timestamp"
match = { regex = '^(?:<\d{1,3}>(?:1 )?)?(\w{3} [ \d]\d \d\d:\d\d:\d\d|\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\S*)' }
scope = "group:1"
style = { fg = "muted" }
priority = -40

[[rules]]
name = "Errors"
match = { regex = '(?i)\b(error|fail(ed|ure)?|denied|refused|panic|fatal|segfault)\b' }
scope = "match"
style = { fg = "error", bold = true }
priority = -15
minimap = "error"

[[rules]]
name = "Warnings"
match = { regex = '(?i)\b(warn(ing)?|timeout|retry)\b' }
scope = "match"
style = { fg = "warn", bold = true }
priority = -20
minimap = "warn"
```

## Using it

Nothing to do: it is active already. To customize it, copy the text above to `<data folder>/profiles/syslog.toml` and edit it. A user profile with the same `name` replaces the built-in one; give it a different `name` to keep both. See [Installation and portable mode](../install.md) for where the data folder is.
