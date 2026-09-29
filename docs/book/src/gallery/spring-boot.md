# Spring Boot

Spring Boot's default console/file layout (Logback), for example:

```text
2026-09-29T10:01:02.123+02:00  INFO 4821 --- [           main] c.e.demo.DemoApplication : Started
```

The level is right-aligned to five characters, so INFO has two spaces in front of it.

This is a **gallery example**: it is not built in. Its source is `docs/profiles/spring-boot.toml` in the repository; the full text is at the bottom of this page.

## Which files it is used for

* File name patterns (`match_files`): `spring*.log`, `application*.log`, `*-app.log`. A pattern without `/` is compared with the file name, one with `/` with the whole path.
* Content (`match_content`): a regular expression tried on the first lines of the file: ` ^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+([+-]\d\d:\d\d|Z)\s+(TRACE|DEBUG|INFO|WARN|ERROR|FATAL)\s+\d+ ---  `.

## Columns and timestamps

Parser: **Regex** (`parser = "regex"`).

| Column | Kind |
|---|---|
| `time` | timestamp |
| `level` | level |
| `pid` | number |
| `app` | text |
| `thread` | text |
| `logger` | text |
| `message` | text |

Display order (`order`): `time`, `level`, `thread`, `logger`, `message`, `pid`, `app`. `*` stands for all other columns; names the parser does not produce are ignored.

Timestamps: column `time`, format `rfc3339`. They drive go to time, time gaps and the merged view.

## Highlight rules

Higher priority wins when rules overlap; on a tie the later rule wins. Colours are semantic names resolved by the active theme.

| Priority | Rule | Matches | Scope | Style | Actions |
|---:|---|---|---|---|---|
| 40 | ERROR / FATAL | column `level` `regex` `^(ERROR\|FATAL)$` | `line` | bg `error.subtle` |  |
| 30 | WARN | column `level` `eq` `warn` | `column:level` | fg `warn`, bold |  |
| 20 | Application startup | regex `Started \w+ in [\d.]+ seconds` | `match` | fg `success`, bold | bookmark |
| 15 | Slow query hint | regex `\b(took\|elapsed)[ =:]+(\d{4,}) ?ms` | `group:2` | fg `warn`, bold |  |
| 10 | Java stack trace | regex `^\s+at [\w$.]+\(.*\)$\|^Caused by: \|^\s+\.\.\. \d+ (common frames omitted\|more)$` | `line` | fg `muted` |  |

## Sample lines

The examples in this gallery are tested against these lines.

```text
2026-09-29T10:01:02.123+02:00  INFO 4821 --- [           main] com.example.demo.DemoApplication         : Starting DemoApplication using Java 21.0.4
2026-09-29T10:01:03.870+02:00  INFO 4821 --- [           main] com.example.demo.DemoApplication         : Started DemoApplication in 1.912 seconds (process running for 2.401)
2026-09-29T10:01:09.552+02:00  WARN 4821 --- [nio-8080-exec-2] o.h.engine.jdbc.spi.SqlExceptionHelper   : SQL Warning Code: 0, SQLState: 00000
2026-09-29T10:01:11.004+02:00 ERROR 4821 --- [nio-8080-exec-4] c.e.demo.web.OrderController             : Order 1042 failed
java.lang.IllegalStateException: stock is negative
	at com.example.demo.service.StockService.reserve(StockService.java:57)
	at com.example.demo.web.OrderController.create(OrderController.java:31)
	... 42 common frames omitted
2026-09-29T10:01:12.200+02:00  INFO 4821 --- [nio-8080-exec-5] c.e.demo.web.OrderController             : query took 2310 ms
```

## The full profile

```toml
# Spring Boot's default console/file layout (Logback), for example:
#   2026-09-29T10:01:02.123+02:00  INFO 4821 --- [           main] c.e.demo.DemoApplication : Started
# The level is right-aligned to five characters, so INFO has two spaces
# in front of it.
name = "Spring Boot"
match_files = ["spring*.log", "application*.log", "*-app.log"]
match_content = '^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+([+-]\d\d:\d\d|Z)\s+(TRACE|DEBUG|INFO|WARN|ERROR|FATAL)\s+\d+ --- '

[columns]
parser = "regex"
pattern = '^(?P<time>\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+(?:[+-]\d\d:\d\d|Z))\s+(?P<level>TRACE|DEBUG|INFO|WARN|ERROR|FATAL)\s+(?P<pid>\d+) --- (?:\[(?P<app>[^\]]*)\] )?\[\s*(?P<thread>[^\]]*?)\s*\] (?P<logger>\S+)\s*: (?P<message>.*)$'
order = ["time", "level", "thread", "logger", "message", "pid", "app"]
kinds = { pid = "number" }

[timestamp]
column = "time"
format = "rfc3339"

[[rules]]
name = "ERROR / FATAL"
match = { column = "level", op = "regex", value = "^(ERROR|FATAL)$" }
scope = "line"
style = { bg = "error.subtle" }
priority = 40

[[rules]]
name = "WARN"
match = { column = "level", op = "eq", value = "warn" }
scope = "column:level"
style = { fg = "warn", bold = true }
priority = 30

[[rules]]
name = "Java stack trace"
match = { regex = '^\s+at [\w$.]+\(.*\)$|^Caused by: |^\s+\.\.\. \d+ (common frames omitted|more)$' }
scope = "line"
style = { fg = "muted" }
priority = 10

[[rules]]
name = "Application startup"
match = { regex = 'Started \w+ in [\d.]+ seconds' }
scope = "match"
style = { fg = "success", bold = true }
priority = 20
bookmark = true

[[rules]]
name = "Slow query hint"
match = { regex = '\b(took|elapsed)[ =:]+(\d{4,}) ?ms' }
scope = "group:2"
style = { fg = "warn", bold = true }
priority = 15
```

## Using it

Copy the text above to `<data folder>/profiles/spring-boot.toml`. OxTail reloads the profiles folder while it runs, so the profile is used for matching files right away, and it can be chosen by name from the status bar. See [Installation and portable mode](../install.md) for where the data folder is. Adjust `match_files` to your own file names.
