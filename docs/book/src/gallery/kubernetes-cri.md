# Kubernetes container (CRI)

Kubernetes container logs in the CRI format written by containerd and CRI-O: "&lt;RFC 3339 time&gt; &lt;stdout\|stderr&gt; &lt;P\|F&gt; &lt;message&gt;". P marks a partial line (the runtime split a long line), F a full one.

This is a **gallery example**: it is not built in. Its source is `docs/profiles/kubernetes-cri.toml` in the repository; the full text is at the bottom of this page.

## Which files it is used for

* File name patterns (`match_files`): `/var/log/pods/**/*.log`, `/var/log/containers/*.log`. A pattern without `/` is compared with the file name, one with `/` with the whole path.
* Content (`match_content`): a regular expression tried on the first lines of the file: ` ^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?Z (stdout|stderr) [PF]  `.

## Columns and timestamps

Parser: **Regex** (`parser = "regex"`).

| Column | Kind |
|---|---|
| `time` | timestamp |
| `stream` | text |
| `tag` | text |
| `message` | text |

Display order (`order`): `time`, `stream`, `message`, `tag`. `*` stands for all other columns; names the parser does not produce are ignored.

Timestamps: column `time`, format `rfc3339`. They drive go to time, time gaps and the merged view.

## Highlight rules

Higher priority wins when rules overlap; on a tie the later rule wins. Colours are semantic names resolved by the active theme.

| Priority | Rule | Matches | Scope | Style | Actions |
|---:|---|---|---|---|---|
| 50 | Kubernetes events | regex `\b(CrashLoopBackOff\|OOMKilled\|ImagePullBackOff\|Evicted\|Back-off restarting)\b` | `match` | fg `fatal`, bold, underline | bookmark |
| 40 | error words | regex `\b(FATAL\|CRITICAL\|PANIC\|ERROR\|level=error)\b` | `match` | fg `error`, bold |  |
| 30 | warning words | regex `\b(WARN\|WARNING\|level=warn)\b` | `match` | fg `warn`, bold |  |
| 5 | stderr | column `stream` `eq` `stderr` | `line` | bg `warn.subtle` |  |

## Sample lines

The examples in this gallery are tested against these lines.

```text
2026-09-29T10:01:02.123456789Z stdout F server listening on :8080
2026-09-29T10:01:03.004211980Z stdout F level=info msg="request served" path=/api/users status=200
2026-09-29T10:01:04.981200000Z stderr F WARN cache miss ratio above threshold: 0.42
2026-09-29T10:01:05.310045221Z stderr P panic: runtime error: invalid memory address or nil pointer dereference
2026-09-29T10:01:05.310112009Z stderr F goroutine 1 [running]:
2026-09-29T10:01:07.000001000Z stdout F Back-off restarting failed container app in pod web-7d9f
```

## The full profile

```toml
# Kubernetes container logs in the CRI format written by containerd and
# CRI-O: "<RFC 3339 time> <stdout|stderr> <P|F> <message>".
# P marks a partial line (the runtime split a long line), F a full one.
name = "Kubernetes container (CRI)"
match_files = ["/var/log/pods/**/*.log", "/var/log/containers/*.log"]
match_content = '^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?Z (stdout|stderr) [PF] '

[columns]
parser = "regex"
pattern = '^(?P<time>\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z) (?P<stream>stdout|stderr) (?P<tag>[PF]) (?P<message>.*)$'
order = ["time", "stream", "message", "tag"]

[timestamp]
column = "time"
format = "rfc3339"

# Everything a container writes to stderr gets a tinted background.
[[rules]]
name = "stderr"
match = { column = "stream", op = "eq", value = "stderr" }
scope = "line"
style = { bg = "warn.subtle" }
priority = 5

# The application's own level, whatever the logging library prints.
[[rules]]
name = "error words"
match = { regex = '\b(FATAL|CRITICAL|PANIC|ERROR|level=error)\b' }
scope = "match"
style = { fg = "error", bold = true }
priority = 40

[[rules]]
name = "warning words"
match = { regex = '\b(WARN|WARNING|level=warn)\b' }
scope = "match"
style = { fg = "warn", bold = true }
priority = 30

[[rules]]
name = "Kubernetes events"
match = { regex = '\b(CrashLoopBackOff|OOMKilled|ImagePullBackOff|Evicted|Back-off restarting)\b' }
scope = "match"
style = { fg = "fatal", bold = true, underline = true }
priority = 50
bookmark = true
```

## Using it

Copy the text above to `<data folder>/profiles/kubernetes-cri.toml`. OxTail reloads the profiles folder while it runs, so the profile is used for matching files right away, and it can be chosen by name from the status bar. See [Installation and portable mode](../install.md) for where the data folder is. Adjust `match_files` to your own file names.
