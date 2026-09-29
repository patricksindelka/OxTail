# Node.js pino (JSON)

Node.js pino / bunyan style JSON Lines: numeric levels and an epoch time in milliseconds. 10 trace, 20 debug, 30 info, 40 warn, 50 error, 60 fatal.

```text
{"level":30,"time":1790676062123,"pid":4821,"hostname":"web-1","msg":"listening"}
```

This is a **gallery example**: it is not built in. Its source is `docs/profiles/node-pino.toml` in the repository; the full text is at the bottom of this page.

## Which files it is used for

* File name patterns (`match_files`): `*pino*.log`, `*pino*.jsonl`, `node*.log`. A pattern without `/` is compared with the file name, one with `/` with the whole path.
* Content (`match_content`): a regular expression tried on the first lines of the file: `^\{"level":\d+,"time":\d{13}`.

## Columns and timestamps

Parser: **JSON Lines** (`parser = "jsonl"`).

The columns are the keys found in the first lines of the file (nested JSON keys are flattened with dots); the keys named in `order` come first.

Display order (`order`): `time`, `level`, `msg`, `hostname`, `pid`, `*`. `*` stands for all other columns; names the parser does not produce are ignored.

Timestamps: column `time`, format `epoch_ms`. They drive go to time, time gaps and the merged view.

## Highlight rules

Higher priority wins when rules overlap; on a tie the later rule wins. Colours are semantic names resolved by the active theme.

| Priority | Rule | Matches | Scope | Style | Actions |
|---:|---|---|---|---|---|
| 50 | fatal | column `level` `ge` `60` | `line` | fg `fatal`, bg `fatal.subtle`, bold |  |
| 45 | 5xx responses | column `res.statusCode` `ge` `500` | `column:res.statusCode` | fg `error`, bold |  |
| 40 | error | column `level` `eq` `50` | `line` | bg `error.subtle` |  |
| 30 | warn | column `level` `eq` `40` | `line` | bg `warn.subtle` |  |
| 5 | debug and trace | column `level` `le` `20` | `line` | fg `muted` |  |

## Sample lines

The examples in this gallery are tested against these lines.

```text
{"level":30,"time":1790676062123,"pid":4821,"hostname":"web-1","msg":"server listening at http://0.0.0.0:3000"}
{"level":20,"time":1790676062410,"pid":4821,"hostname":"web-1","msg":"pool ready","size":10}
{"level":30,"time":1790676063077,"pid":4821,"hostname":"web-1","req":{"method":"GET","url":"/api/users"},"res":{"statusCode":200},"responseTime":12,"msg":"request completed"}
{"level":40,"time":1790676064512,"pid":4821,"hostname":"web-1","msg":"slow upstream","upstream":"payments","ms":1800}
{"level":50,"time":1790676065001,"pid":4821,"hostname":"web-1","res":{"statusCode":502},"responseTime":30012,"msg":"request errored"}
{"level":60,"time":1790676066000,"pid":4821,"hostname":"web-1","msg":"uncaught exception, exiting"}
```

## The full profile

```toml
# Node.js pino / bunyan style JSON Lines: numeric levels and an epoch time in
# milliseconds. 10 trace, 20 debug, 30 info, 40 warn, 50 error, 60 fatal.
#   {"level":30,"time":1790676062123,"pid":4821,"hostname":"web-1","msg":"listening"}
name = "Node.js pino (JSON)"
match_files = ["*pino*.log", "*pino*.jsonl", "node*.log"]
match_content = '^\{"level":\d+,"time":\d{13}'

[columns]
parser = "jsonl"
order = ["time", "level", "msg", "hostname", "pid", "*"]
kinds = { level = "number", time = "timestamp", pid = "number", "res.statusCode" = "number", "responseTime" = "duration" }

[timestamp]
column = "time"
format = "epoch_ms"

[[rules]]
name = "fatal"
match = { column = "level", op = "ge", value = "60" }
scope = "line"
style = { fg = "fatal", bg = "fatal.subtle", bold = true }
priority = 50

[[rules]]
name = "error"
match = { column = "level", op = "eq", value = "50" }
scope = "line"
style = { bg = "error.subtle" }
priority = 40

[[rules]]
name = "warn"
match = { column = "level", op = "eq", value = "40" }
scope = "line"
style = { bg = "warn.subtle" }
priority = 30

[[rules]]
name = "debug and trace"
match = { column = "level", op = "le", value = "20" }
scope = "line"
style = { fg = "muted" }
priority = 5

[[rules]]
name = "5xx responses"
match = { column = "res.statusCode", op = "ge", value = "500" }
scope = "column:res.statusCode"
style = { fg = "error", bold = true }
priority = 45
```

## Using it

Copy the text above to `<data folder>/profiles/node-pino.toml`. OxTail reloads the profiles folder while it runs, so the profile is used for matching files right away, and it can be chosen by name from the status bar. See [Installation and portable mode](../install.md) for where the data folder is. Adjust `match_files` to your own file names.
