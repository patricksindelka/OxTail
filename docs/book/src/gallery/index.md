# Profile gallery

A profile bundles the file patterns it applies to, a column parser and highlight rules. This gallery has one page per profile: what it matches, which columns it produces, the rules as a table and the complete TOML file. The chapter [Highlighting and profiles](../profiles.md) explains every field.

The pages are generated from the profile files themselves (`cargo run -p xtask -- gen-docs`), and CI loads every profile with the real profile loader, so what you see here is what OxTail reads.

To use a profile, copy its file to `<data folder>/profiles/`. See [Installation and portable mode](../install.md) for the location of the data folder.

## Built-in profiles

These ship inside the OxTail executable and need no setup.

| Profile | Files | Parser |
|---|---|---|
| [Apache access](apache-access.md) | `*apache*access*`, `access_log`, `access_log.*`, ... | Apache/Nginx combined |
| [Generic](generic.md) | by name only | none |
| [IIS W3C](iis-w3c.md) | `u_ex*.log`, `*iis*.log`, `W3SVC*.log` | W3C / IIS |
| [JSON lines](jsonl.md) | `*.jsonl`, `*.ndjson`, `*.jsonl.*`, ... | JSON Lines |
| [Java / log4j](log4j.md) | `*log4j*`, `catalina*.out`, `catalina*.log`, ... | Regex |
| [logfmt](logfmt.md) | `*.logfmt` | logfmt |
| [Nginx access](nginx-access.md) | `*nginx*access*.log`, `*nginx*access*.log.*`, `access.log` | Apache/Nginx combined |
| [Syslog](syslog.md) | `syslog`, `syslog.*`, `messages`, ... | Syslog (RFC 3164) |

## More examples

These are not built in. Copy the ones you need.

| Profile | Files | Parser |
|---|---|---|
| [Kubernetes container (CRI)](kubernetes-cri.md) | `/var/log/pods/**/*.log`, `/var/log/containers/*.log` | Regex |
| [Node.js pino (JSON)](node-pino.md) | `*pino*.log`, `*pino*.jsonl`, `node*.log` | JSON Lines |
| [PostgreSQL](postgresql.md) | `postgresql*.log`, `postgres*.log`, `pg_log/*.log` | Regex |
| [Python logging](python-logging.md) | `*.py.log`, `python*.log`, `django*.log`, ... | Regex |
| [Spring Boot](spring-boot.md) | `spring*.log`, `application*.log`, `*-app.log` | Regex |
| [Windows Event Log (CSV)](windows-event-csv.md) | `*event*.csv`, `Application.csv`, `System.csv`, ... | CSV |
