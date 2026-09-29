//! log4j / logback layout patterns converted to regular expressions.
//!
//! Supported conversions: `%d %date %p %level %t %thread %c %logger %C %class
//! %M %method %L %line %F %file %m %msg %message %r %X{key} %n %%`, with
//! width/alignment modifiers (`%-5p`, `%20.30c`). Exception conversions
//! (`%ex`, `%throwable`) and wrappers (`%highlight(...)`) are accepted and
//! ignored. Unknown conversions become a lazy catch-all column named after
//! the conversion word.

use crate::error::ColumnsError;

enum Piece {
    Lit(String),
    Group {
        name: String,
        re: String,
        pad_left: bool,
        pad_right: bool,
    },
}

/// Converts a log4j/logback pattern like `%d{ISO8601} %-5p [%t] %c{1} - %m%n`
/// into an anchored regular expression with named groups
/// (`ts`, `level`, `thread`, `logger`, `msg`, ...).
pub fn log4j_pattern_to_regex(pattern: &str) -> Result<String, ColumnsError> {
    if pattern.len() > 4096 {
        return Err(ColumnsError::Pattern("pattern is too long".into()));
    }
    let chars: Vec<char> = pattern.chars().collect();
    let mut pieces: Vec<Piece> = Vec::new();
    let mut lit = String::new();
    let mut i = 0;
    let mut paren_skip: Vec<usize> = Vec::new();
    let mut depth = 0usize;

    let flush = |lit: &mut String, pieces: &mut Vec<Piece>| {
        if !lit.is_empty() {
            pieces.push(Piece::Lit(std::mem::take(lit)));
        }
    };

    while i < chars.len() {
        let c = chars[i];
        if c == '(' {
            depth += 1;
        }
        if c == ')' && paren_skip.last() == Some(&depth) {
            paren_skip.pop();
            depth -= 1;
            i += 1;
            continue;
        }
        if c == ')' {
            depth = depth.saturating_sub(1);
        }
        if c != '%' {
            lit.push(c);
            i += 1;
            continue;
        }
        i += 1;
        if chars.get(i) == Some(&'%') {
            lit.push('%');
            i += 1;
            continue;
        }
        // modifiers
        let mut left = false;
        let mut has_min = false;
        if chars.get(i) == Some(&'-') {
            left = true;
            i += 1;
        }
        while chars.get(i).is_some_and(char::is_ascii_digit) {
            has_min = true;
            i += 1;
        }
        if chars.get(i) == Some(&'.') {
            i += 1;
            if chars.get(i) == Some(&'-') {
                i += 1;
            }
            while chars.get(i).is_some_and(char::is_ascii_digit) {
                i += 1;
            }
        }
        let start = i;
        while chars.get(i).is_some_and(|c| c.is_ascii_alphabetic()) {
            i += 1;
        }
        let word: String = chars[start..i].iter().collect();
        if word.is_empty() {
            return Err(ColumnsError::Pattern(format!(
                "expected a conversion character after '%' at position {start}"
            )));
        }
        // options: {..}{..}
        let mut options: Vec<String> = Vec::new();
        while chars.get(i) == Some(&'{') {
            let mut d = 0;
            let s = i + 1;
            let mut j = i;
            while j < chars.len() {
                match chars[j] {
                    '{' => d += 1,
                    '}' => {
                        d -= 1;
                        if d == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            if j >= chars.len() {
                return Err(ColumnsError::Pattern(format!(
                    "unclosed '{{' after %{word}"
                )));
            }
            options.push(chars[s..j].iter().collect());
            i = j + 1;
        }
        let opt = options.first().map(String::as_str);
        let (name, re): (String, String) = match word.as_str() {
            "d" | "date" => ("ts".into(), date_regex(opt)),
            "p" | "level" | "le" => ("level".into(), r"[A-Za-z]+".into()),
            "t" | "thread" | "tn" | "threadName" => ("thread".into(), r".*?".into()),
            "c" | "logger" | "lo" => ("logger".into(), r"[^\s\[\]:]+".into()),
            "C" | "class" => ("class".into(), r"[^\s\[\]:]+".into()),
            "M" | "method" => ("method".into(), r"[^\s\[\]:]+".into()),
            "F" | "file" => ("file".into(), r"[^\s\[\]:]+".into()),
            "L" | "line" => ("line".into(), r"\d+".into()),
            "r" | "relative" => ("elapsed".into(), r"\d+".into()),
            "m" | "msg" | "message" => ("msg".into(), r".*?".into()),
            "X" | "mdc" | "MDC" => (sanitize(opt.unwrap_or("mdc")), r".*?".into()),
            "n" => {
                flush(&mut lit, &mut pieces);
                continue;
            }
            "ex" | "exception" | "throwable" | "xEx" | "rEx" | "xException" | "rException" => {
                continue;
            }
            "highlight" | "style" | "color" | "boldRed" | "boldGreen" | "boldYellow"
            | "boldBlue" | "red" | "green" | "yellow" | "blue" | "cyan" | "magenta" | "white"
            | "gray" | "boldMagenta" | "boldCyan" | "boldWhite" | "faint" => {
                if chars.get(i) == Some(&'(') {
                    depth += 1;
                    paren_skip.push(depth);
                    i += 1;
                }
                continue;
            }
            other => (sanitize(other), r".*?".into()),
        };
        flush(&mut lit, &mut pieces);
        pieces.push(Piece::Group {
            name,
            re,
            pad_left: has_min && !left,
            pad_right: has_min && left,
        });
    }
    flush(&mut lit, &mut pieces);

    if !pieces.iter().any(|p| matches!(p, Piece::Group { .. })) {
        return Err(ColumnsError::Pattern(
            "pattern contains no conversions".into(),
        ));
    }

    let last_is_group = matches!(pieces.last(), Some(Piece::Group { .. }));
    let n = pieces.len();
    // Padding next to literal spaces is folded into the literal (` {k,}`) so
    // the regex stays unambiguous, which lets the regex engine use its fast
    // one-pass matcher for captures.
    let mut flex_lead = vec![false; n];
    let mut flex_trail = vec![false; n];
    for i in 0..n {
        if let Piece::Group {
            pad_left,
            pad_right,
            ..
        } = &mut pieces[i]
        {
            let (l, r) = (*pad_left, *pad_right);
            if r && matches!(pieces.get(i + 1), Some(Piece::Lit(s)) if s.starts_with(' ')) {
                flex_lead[i + 1] = true;
                if let Piece::Group { pad_right, .. } = &mut pieces[i] {
                    *pad_right = false;
                }
            }
            if l && i > 0 && matches!(&pieces[i - 1], Piece::Lit(s) if s.ends_with(' ')) {
                flex_trail[i - 1] = true;
                if let Piece::Group { pad_left, .. } = &mut pieces[i] {
                    *pad_left = false;
                }
            }
        }
    }
    let next_lit_first: Vec<Option<char>> = (0..n)
        .map(|i| match pieces.get(i + 1) {
            Some(Piece::Lit(s)) => s.chars().next(),
            _ => None,
        })
        .collect();
    let mut out = String::from("^");
    let mut used: Vec<String> = Vec::new();
    for (idx, p) in pieces.into_iter().enumerate() {
        match p {
            Piece::Lit(s) => {
                let mut body: &str = &s;
                let mut lead = 0;
                let mut trail = 0;
                if flex_lead[idx] {
                    lead = body.len() - body.trim_start_matches(' ').len();
                    body = &body[lead..];
                }
                if flex_trail[idx] {
                    trail = body.len() - body.trim_end_matches(' ').len();
                    body = &body[..body.len() - trail];
                }
                if lead > 0 {
                    out.push_str(&format!(" {{{lead},}}"));
                }
                out.push_str(&regex::escape(body));
                if trail > 0 {
                    out.push_str(&format!(" {{{trail},}}"));
                }
            }
            Piece::Group {
                mut name,
                re,
                pad_left,
                pad_right,
            } => {
                let base = name.clone();
                let mut k = 2;
                while used.contains(&name) {
                    name = format!("{base}_{k}");
                    k += 1;
                }
                used.push(name.clone());
                let re = if re == ".*?" && base != "msg" {
                    match next_lit_first[idx] {
                        Some(c) if c != ' ' => format!("[^{}]*", regex::escape(&c.to_string())),
                        _ => re,
                    }
                } else {
                    re
                };
                let re = if idx == n - 1 && last_is_group && re == ".*?" {
                    ".*".to_string()
                } else {
                    re
                };
                if pad_left {
                    out.push_str(" *");
                }
                out.push_str(&format!("(?P<{name}>{re})"));
                if pad_right {
                    out.push_str(" *");
                }
            }
        }
    }
    out.push('$');
    Ok(out)
}

fn sanitize(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

const ISO_DATE: &str = r"\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(?:[,.]\d+)?";

fn date_regex(fmt: Option<&str>) -> String {
    let Some(fmt) = fmt else {
        return ISO_DATE.into();
    };
    match fmt {
        "ISO8601" | "DEFAULT" | "ISO8601_OFFSET_DATE_TIME_HH" | "ISO8601_BASIC" => {
            return ISO_DATE.into();
        }
        "ABSOLUTE" => return r"\d{2}:\d{2}:\d{2}[,.]\d{3}".into(),
        "DATE" => return r"\d{2} [A-Za-z]{3} \d{4} \d{2}:\d{2}:\d{2}[,.]\d{3}".into(),
        "COMPACT" => return r"\d{14,17}".into(),
        _ => {}
    }
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            i += 1;
            let mut s = String::new();
            while i < chars.len() && chars[i] != '\'' {
                s.push(chars[i]);
                i += 1;
            }
            i += 1;
            if s.is_empty() {
                out.push('\'');
            } else {
                out.push_str(&regex::escape(&s));
            }
            continue;
        }
        if !c.is_ascii_alphabetic() {
            out.push_str(&regex::escape(&c.to_string()));
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() && chars[j] == c {
            j += 1;
        }
        let len = j - i;
        match c {
            'y' | 'Y' | 'u' => out.push_str(if len == 2 { r"\d{2}" } else { r"\d{4}" }),
            'M' | 'L' if len >= 3 => out.push_str(r"[A-Za-z]{3,9}"),
            'M' | 'L' | 'd' | 'H' | 'h' | 'm' | 's' | 'k' | 'K' => {
                out.push_str(if len == 1 { r"\d{1,2}" } else { r"\d{2}" })
            }
            'S' => out.push_str(&format!(r"\d{{{len}}}")),
            'a' => out.push_str(r"[AaPp][Mm]"),
            'E' => out.push_str(r"[A-Za-z]{3,9}"),
            'z' | 'Z' | 'X' | 'x' | 'V' => out.push_str(r"\S+"),
            'D' | 'w' | 'W' | 'F' | 'n' | 'A' | 'N' => out.push_str(r"\d+"),
            _ => out.push_str(r"\w+"),
        }
        i = j;
    }
    out
}
