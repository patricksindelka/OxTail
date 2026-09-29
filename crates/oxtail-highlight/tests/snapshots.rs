//! Snapshot tests: spans produced by the default presets and by ANSI parsing
//! for representative log lines.

use oxtail_highlight::{CompiledRules, LineHighlight, Style, StyledSpan, ansi, presets};

fn tag(s: &Style) -> String {
    let mut t = String::new();
    if let Some(fg) = s.fg {
        t.push_str(&fg.to_string());
    }
    if let Some(bg) = s.bg {
        t.push_str(&format!("/{bg}"));
    }
    for (on, c) in [
        (s.bold, "+b"),
        (s.italic, "+i"),
        (s.underline, "+u"),
        (s.dim, "+d"),
    ] {
        if on {
            t.push_str(c);
        }
    }
    t
}

fn markup(text: &str, spans: &[StyledSpan]) -> String {
    let mut out = String::new();
    let mut pos = 0;
    for s in spans {
        out.push_str(&text[pos..s.range.start]);
        out.push_str(&format!("[{}|{}]", tag(&s.style), &text[s.range.clone()]));
        pos = s.range.end;
    }
    out.push_str(&text[pos..]);
    out
}

fn render(lines: &[&str]) -> String {
    let rules = CompiledRules::compile(&presets::default_rules()).unwrap();
    let mut out = String::new();
    for line in lines {
        let h: LineHighlight = rules.highlight(line, None);
        out.push_str(&markup(line, &h.spans));
        out.push('\n');
        let mut extra = Vec::new();
        if let Some(s) = &h.line_style {
            extra.push(format!("line={}", tag(s)));
        }
        if let Some(m) = h.minimap {
            extra.push(format!("minimap={m}"));
        }
        if let Some(g) = h.gutter {
            extra.push(format!("gutter={g}"));
        }
        if !extra.is_empty() {
            out.push_str(&format!("  ({})\n", extra.join(", ")));
        }
    }
    out
}

#[test]
fn application_log_lines() {
    insta::assert_snapshot!(render(&[
        "2026-09-29T12:00:01.123Z ERROR [worker-3] request 8f14e45f-ceea-467f-a0e6-3d4bd8f3c1a2 failed after 250 ms",
        "2026-09-29 12:00:01,456 WARN  Retrying https://example.com/api/v1/items?id=42 (attempt 3)",
        "12:00:01.500 INFO server listening on 10.0.0.5:8080 and [::1]",
        "12:00:02 DEBUG loaded /etc/oxtail/config.toml:12 in 0.5s",
        "12:00:03 TRACE tick",
        "12:00:04 FATAL out of memory, exiting",
        "connect to [2001:db8::1]:443 failed: timeout (fe80::1ff:fe23:4567:890a)",
    ]));
}

#[test]
fn single_letter_levels() {
    insta::assert_snapshot!(render(&[
        "09-29 12:00:01.123  1234  5678 E AndroidRuntime: boom",
        "09-29 12:00:01.123  1234  5678 W Tag: careful",
        "12:00:00 [I] started",
        "12:00:00 [W] slow disk",
        "12:00:00 [E] crashed",
    ]));
}

#[test]
fn access_and_system_logs() {
    insta::assert_snapshot!(render(&[
        r#"203.0.113.7 - - [29/Sep/2026:12:00:01 +0000] "GET /api/v1/users?id=42 HTTP/1.1" 200 612 "-" "curl/8.0""#,
        r#"198.51.100.2 - bob [29/Sep/2026:12:00:02 +0000] "POST /login HTTP/1.1" 503 0"#,
        r#"127.0.0.1 "DELETE /x HTTP/1.1" 404 9"#,
        "Sep 29 12:00:01 host sshd[123]: Failed password for root from 10.0.0.1 port 22 ssh2",
        "1759147201123 epoch millis event",
    ]));
}

#[test]
fn stack_traces() {
    insta::assert_snapshot!(render(&[
        "java.lang.NullPointerException: Cannot invoke \"String.length()\"",
        "\tat com.foo.Bar.baz(Bar.java:42)",
        "    at org.springframework.web.Servlet.service(Servlet.java:733)",
        "Caused by: java.io.IOException: disk full",
        "Traceback (most recent call last):",
        "  File \"app.py\", line 12, in main",
        "thread 'main' panicked at src/main.rs:4:5:",
    ]));
}

#[test]
fn ansi_sequences() {
    let mut out = String::new();
    for line in [
        "\x1b[31mERROR\x1b[0m plain \x1b[1;32mok\x1b[0m",
        "\x1b[38;5;208morange\x1b[0m \x1b[38;2;10;20;30mrgb\x1b[0m \x1b[4;44munder\x1b[0m",
        "\x1b[2Kcleared \x1b]0;title\x07line \x1b[Hhome",
        "\x1b[31mtruncated \x1b[3",
    ] {
        let (text, spans) = ansi::parse(line);
        out.push_str(&markup(&text, &spans));
        out.push('\n');
    }
    insta::assert_snapshot!(out);
}
