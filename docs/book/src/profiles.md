# Highlighting and profiles

A **profile** is a TOML file that bundles three things for one kind of log:

* how to recognise the files it is for (file name patterns, and a pattern for the
  first lines);
* how to split lines into columns and read timestamps;
* highlight **rules**.

OxTail picks a profile per file when it opens it, and you can switch it per tab in
the status bar (**Profile: ...**). Eight profiles are built in (see the
[profile gallery](gallery/index.md), which also has more examples to copy).

## Where profiles live

* Built-in profiles are inside the executable.
* Your own go in `<data folder>/profiles/*.toml`, one profile per file, any file
  name. See [Installation and portable mode](install.md) for the data folder.
  The folder is watched: save the file and OxTail reloads it while running.
* A user profile with the same `name` as a built-in profile **replaces** it.
* A file that is not valid TOML, or has no `name`, is skipped, and OxTail says
  why at start-up (and in its log). Everything else keeps working.

The **rule editor** (**View > Highlight rules...** or **Edit rules...** in the
profile menu) edits the rules of the current tab with a live preview on the lines
in view. It can apply the rules to the tab only, or save them as a user profile;
a saved copy of a built-in profile does not inherit its file patterns, so it does
not take over from the original.

## How a profile is chosen

For a file, OxTail looks at your profiles first and only then at the built-in
ones. Within each group:

1. a profile that matches by **both** a file pattern and the content beats one
   that matches only by content, which beats one that matches only by file name;
2. among file name matches, the pattern with the most literal characters wins;
3. remaining ties go to the first profile (user profiles are ordered by file
   name).

If nothing matches, the **Generic** profile (log levels) is used. A tab can also
be told explicitly: choose a profile in the status bar, or start with
`--profile nginx-access` (see [Command line](cli.md)).

## Profile reference

```toml
name = "Go service (logfmt)"          # required, also the identity of the profile
match_files = ["*service*.log"]       # optional list of glob patterns
match_content = '^time=\S+ level='    # optional regex tried on the first lines

[columns]                             # optional, see below
parser = "logfmt"
order  = ["time", "level", "msg", "*"]

[timestamp]                           # optional, see below
column = "time"
format = "rfc3339"

[[rules]]                             # any number of rules
name  = "errors"
match = { column = "level", op = "eq", value = "error" }
scope = "line"
style = { bg = "error.subtle" }
alert = true
```

Unknown keys are ignored by OxTail (so a typo does nothing). The gallery check
in the repository rejects unknown keys in `[[rules]]`, `[columns]` and
`[timestamp]`, which catches typos in shared profiles.

### Top-level keys

| Key | Type | Meaning |
|---|---|---|
| `name` | string | Display name and identity. Required. |
| `match_files` | list of strings | Glob patterns. A pattern **without** `/` is compared with the file name only; one with a `/` is compared with the whole path (a pattern like `logs/*.log` also matches `/var/logs/a.log`). Syntax: `*` (anything except `/`), `**` (anything including `/`; `**/` also matches zero folders), `?` (one character), `[abc]`, `[a-z]`, `[!abc]`. A backslash in a pattern is read as `/`. Matching ignores case on Windows only. |
| `match_content` | string | A regular expression tried on each of the first lines of the file. An invalid pattern is reported and never matches. |
| `default_filter` | string | Accepted and stored, but **not applied** by this version. |
| `columns` | table | The column parser. See [Columns table](#columns-table). |
| `timestamp` | table | Where and how to read the time. See [Timestamp table](#timestamp-table). |
| `rules` | array of tables | Highlight rules. See [Rules](#rules). |

### Columns table

`[columns]` tells OxTail how to split a line into columns, so the file can be
shown as a table and used in column rules and [queries](columns.md). Without a
`[columns]` table OxTail still offers to detect a parser (see
[Columns](columns.md#auto-detection)).

| Key | Used by | Meaning |
|---|---|---|
| `parser` | all | Which parser, see the table below. Required. |
| `order` | all | Display order of the columns. `"*"` stands for all columns not named; names the parser does not produce are ignored. With no `"*"`, unnamed columns come last. |
| `pattern` | `regex`, `log4j` | The regex with named groups (`(?P<name>...)`), or the log4j layout pattern. Required for those parsers. |
| `delimiter` | `csv`, `tsv`, `delimited`, `psv` | One character; `"\\t"` or `"tab"` for a tab. Defaults: `,` (`csv`, `delimited`), tab (`tsv`), `\|` (`psv`). |
| `columns` | `csv`, `tsv`, `delimited`, `psv`, `logfmt`, `jsonl` | Column names. For delimited files they replace the header; for logfmt and JSON Lines they name the known keys. |
| `has_header` | `csv`, ... with `columns` | The first line of the file is a header row (skipped). Without `columns`, the names are taken from the first line and it is treated as the header. |
| `fields` | `w3c`, `fixed` | For `w3c`: the field names (normally read from the file's `#Fields:` line). For `fixed`: the layout, an array of `{ name = "...", start = N, end = M }` (character positions, 0-based; `end` is optional and means "to the end of the line"). |
| `kinds` | all | A table from column name to kind: `text`, `number`, `timestamp`, `duration`, `bytes`, `json` or `level`. The kind decides alignment, formatting, statistics and how queries compare values. Names not listed are guessed from the column name (`ts`, `time`, `timestamp` become `timestamp`; `level`, `severity` become `level`; `status`, `pid` become `number`; `size`, `bytes` become `bytes`; `duration`, `latency`, `took` become `duration`). |

Parser names (and their aliases):

| `parser` | Format | Columns |
|---|---|---|
| `regex` | Your regex; named groups become the columns. | The group names. |
| `log4j`, `logback` | A layout such as `%d{ISO8601} %-5p [%t] %c{1} - %m%n`, converted to a regex. Supports `%d %p %level %t %thread %c %logger %C %M %L %F %m %msg %r %X{key} %n %%` with width modifiers. | `ts`, `level`, `thread`, `logger`, `msg` and so on, named after the conversion. |
| `jsonl`, `json`, `jsonlines`, `ndjson` | One JSON object per line. Nested keys are flattened with dots (`http.status`). | The keys. With `"*"` in `order`, every key found in the first lines is a column. |
| `logfmt` | `key=value key2="quoted value"`. | The keys, discovered like JSON Lines. |
| `syslog` | RFC 3164 (BSD) and RFC 5424, whichever fits better. `syslog3164` / `syslog_bsd` and `syslog5424` pick one. | `pri`, `ts`, `host`, `tag`, `pid`, `msg` (3164); `pri`, `version`, `ts`, `host`, `app`, `procid`, `msgid`, `sd`, `msg` (5424). |
| `nginx_combined`, `apache_combined`, `combined`, `access_combined` | Common log format plus referer and user agent. | `remote`, `ident`, `user`, `ts`, `method`, `path`, `proto`, `status`, `size`, `referer`, `agent`. |
| `nginx_common`, `apache_common`, `common`, `access_common`, `clf` | Common log format. | The same, without `referer` and `agent`. |
| `w3c`, `iis`, `w3c_extended` | W3C Extended (IIS). | The names in the file's `#Fields:` line. |
| `csv`, `tsv`, `delimited`, `psv` | Delimited text with quoting. | The header names, or `columns`. |
| `fixed`, `fixed_width`, `fixedwidth` | Fixed-width columns. | The `fields` names. |

### Timestamp table

```toml
[timestamp]
column = "time"        # the column to read; without it the first timestamp on the line is used
format = "rfc3339"     # a name from the table below; "auto" or absent detects
zone = "Europe/Amsterdam"   # optional; for timestamps without a zone (also spelled timezone)
```

The timestamp drives [go to time](time.md#go-to-time), [time gaps](time.md#time-gaps-and-relative-time),
the merged view and time terms in column queries. `zone` may be `UTC`, `local` or
an IANA name; it overrides the `timezone` setting for this profile.

| `format` (aliases) | Reads | Example |
|---|---|---|
| `rfc3339` (`iso8601`, `iso`) | ISO 8601 / RFC 3339, `T` or space, optional fraction and offset | `2026-09-29T10:01:02.123Z` |
| `iso8601_comma` (`iso8601comma`, `log4j`) | The same with a comma before the fraction | `2026-09-29 10:01:02,123` |
| `slashed` (`go`) | Go's `log` package | `2026/09/29 10:01:02` |
| `compact` | Compact ISO | `20260929T100102Z` |
| `syslog` (`rfc3164`) | Syslog, no year | `Sep 29 10:01:02` |
| `apache` (`clf`, `nginx`) | Common log format | `29/Sep/2026:10:01:02 +0000` |
| `rfc2822` | Mail and HTTP headers | `Tue, 29 Sep 2026 10:01:02 +0000` |
| `us` (`us_datetime`) | Month first, 12 or 24 hour | `9/29/2026 10:01:02 AM` |
| `time_only` (`time`) | Time of day; the date comes from the file's other lines | `10:01:02.123` |
| `epoch` (`epoch_s`, `epoch_seconds`, `unix`) | Seconds since 1970, optional fraction | `1790676062` |
| `epoch_ms` (`epoch_millis`) | Milliseconds | `1790676062123` |
| `epoch_us` (`epoch_micros`) | Microseconds | `1790676062123456` |
| `epoch_ns` (`epoch_nanos`) | Nanoseconds | `1790676062123456789` |

If the `[timestamp]` table has an unknown format name, OxTail ignores the whole
table (and says so in its log) and detects the format from the first lines
instead. An unknown zone name falls back to UTC.

### Rules

Each `[[rules]]` table is one highlight rule:

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | empty | Shown in the rule editor. |
| `enabled` | bool | `true` | Set to `false` to keep a rule but switch it off. |
| `match` | table | required | What to look for. See [Matchers](#matchers). |
| `scope` | string | `"match"` | Which part to style. See [Scopes](#scopes). |
| `style` | table | none | See [Styles](#styles). |
| `priority` | integer | `0` | Higher wins when styles overlap; on a tie the later rule wins. |
| `alert`, `hide`, `bookmark` | bool | `false` | See [Actions](#actions). They can also be written in an `actions = { alert = true }` table. |
| `minimap` | colour | none | Colour of this rule's tick in the [minimap](search.md#minimap). |
| `gutter` | bool | `false` | Draw a marker for the rule's lines in the gutter. (`gutter_marker` is an alias.) |

#### Matchers

`match` is a table with one of:

| Form | Meaning |
|---|---|
| `{ regex = '...' }` | A regular expression (Rust `regex` syntax) searched in the line. **Case sensitive** unless you add `case_sensitive = false` (or `ignore_case = true`), or use `(?i)` in the pattern. |
| `{ literal = "..." }` | Plain text (must not be empty). **Ignores case** unless you add `case_sensitive = true`. |
| `{ column = "level", op = "eq", value = "error" }` | A condition on a parsed column. Needs the file to be shown with a column parser; without one the rule never matches. The column name is compared ignoring case. |

`op` for column conditions:

| `op` | Meaning |
|---|---|
| `eq` (default) | Equal. Numeric if both sides are numbers, else text ignoring case. |
| `ne` | Not equal. |
| `contains` | The column contains the value (ignoring case). |
| `regex` | The column matches the value as a regular expression. |
| `gt`, `ge`, `lt`, `le` | Numeric comparison (`>`, `>=`, `<`, `<=`). |

`value` may be a string or a number.

Single-quoted TOML strings (`'...'`) are the easiest way to write regular
expressions, because backslashes are not special in them.

#### Scopes

| `scope` | Styles |
|---|---|
| `"line"` | The whole line (as a background layer under everything else). |
| `"match"` | Each match. For a column condition, the tested column. |
| `"group:N"` | Capture group `N` of each match of a regex (`group:0` is the whole match). For a column condition, the tested column. |
| `"column:NAME"` | The whole column `NAME` (needs a column parser). |

Styles are layered: first the line background, then column styles, then match
styles; within a layer, higher `priority` wins.

#### Styles

```toml
style = { fg = "error", bg = "error.subtle", bold = true, italic = false, underline = false, dim = false }
```

`fg` and `bg` are colours; `bold`, `italic`, `underline` and `dim` are booleans.
Anything omitted is left as it is.

A **colour** is either a **semantic name** or a fixed `#rrggbb`:

* `error`, `warn`, `info`, `debug`, `trace`, `success`, `muted`;
* `accent1` to `accent8` for anything else;
* each of these with a `.subtle` suffix (`error.subtle`, `accent3.subtle`) is a
  faint version meant for **backgrounds**, under which normal text stays readable;
* `fatal` and `fatal.subtle` are accepted and currently drawn like `error`.

Semantic colours are resolved by the active [theme](themes.md), so the same profile
looks right on the light, dark and high-contrast themes. A fixed `#rrggbb` is
the same everywhere; prefer semantic names.

#### Actions

* `alert = true`: when a new line matching the rule appears while a file is
  followed, OxTail shows a desktop notification (if `notifications_enabled` is on
  in the settings). To avoid floods, each rule notifies at most once every five
  seconds; matches inside that window are counted and reported as "+N more".
* `hide = true`: lines matching the rule are folded away from the view, like an
  exclude filter. The filter panel and the View menu have **Show hidden lines**
  to bring them back.
* `bookmark = true`: matching lines that appear while a file is followed are
  bookmarked automatically.

### ANSI colours

Escape sequences that set colours (SGR) in the log text are drawn as colours
instead of garbage. **View > Render ANSI colours** switches to showing the raw
text. ANSI styling combines with rules.

## Examples

The [profile gallery](gallery/index.md) lists every built-in profile with a
table of its rules and the complete file, and has extra profiles for Kubernetes
(CRI) logs, Python logging, Spring Boot, PostgreSQL, Windows Event Log exports and
Node.js pino. Copy one to your `profiles` folder and adjust it.
