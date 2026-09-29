//! Hand-written Apache/Nginx common and combined log parser.
//!
//! Equivalent to the regular expressions in the tests, but roughly ten times
//! faster, because access logs are usually the largest files people open.

use crate::model::Record;

/// Column names, in order.
pub(crate) const COLUMNS: [&str; 11] = [
    "remote", "ident", "user", "ts", "method", "path", "proto", "status", "size", "referer",
    "agent",
];

const REMOTE: usize = 0;
const IDENT: usize = 1;
const USER: usize = 2;
const TS: usize = 3;
const METHOD: usize = 4;
const PATH: usize = 5;
const PROTO: usize = 6;
const STATUS: usize = 7;
const SIZE: usize = 8;
const REFERER: usize = 9;
const AGENT: usize = 10;

#[derive(Debug, Clone)]
pub(crate) struct Access {
    combined: bool,
}

/// A non-empty run of non-whitespace bytes followed by one space.
fn token(b: &[u8], i: &mut usize) -> Option<(usize, usize)> {
    let start = *i;
    while *i < b.len() && !b[*i].is_ascii_whitespace() {
        *i += 1;
    }
    if *i == start || b.get(*i) != Some(&b' ') {
        return None;
    }
    let r = (start, *i);
    *i += 1;
    Some(r)
}

/// A `"..."` string with backslash escapes; returns the inner range.
fn quoted(b: &[u8], i: &mut usize) -> Option<(usize, usize)> {
    if b.get(*i) != Some(&b'"') {
        return None;
    }
    *i += 1;
    let start = *i;
    while *i < b.len() {
        match b[*i] {
            b'\\' => *i += 2,
            b'"' => {
                let r = (start, *i);
                *i += 1;
                return Some(r);
            }
            _ => *i += 1,
        }
    }
    None
}

impl Access {
    pub(crate) fn new(combined: bool) -> Self {
        Access { combined }
    }

    pub(crate) fn parse<'a>(&self, line: &'a str, n: usize) -> Option<Record<'a>> {
        if n != COLUMNS.len() {
            return None;
        }
        let b = line.as_bytes();
        let mut i = 0;
        let mut rec = Record::with_columns(n);
        let put = |rec: &mut Record<'a>, col: usize, (s, e): (usize, usize)| {
            if e > s {
                rec.set_slice(col, line, s..e);
            }
        };
        put(&mut rec, REMOTE, token(b, &mut i)?);
        put(&mut rec, IDENT, token(b, &mut i)?);
        put(&mut rec, USER, token(b, &mut i)?);
        // [timestamp]
        if b.get(i) != Some(&b'[') {
            return None;
        }
        i += 1;
        let ts_start = i;
        while i < b.len() && b[i] != b']' {
            i += 1;
        }
        if i == ts_start || b.get(i) != Some(&b']') || b.get(i + 1) != Some(&b' ') {
            return None;
        }
        put(&mut rec, TS, (ts_start, i));
        i += 2;
        // "METHOD /path PROTO"
        let (rs, re) = quoted(b, &mut i)?;
        if b.get(i) != Some(&b' ') {
            return None;
        }
        i += 1;
        let req = &line[rs..re];
        if !req.contains('\\') {
            let m_end = req.bytes().take_while(u8::is_ascii_uppercase).count();
            if m_end > 0 && req.as_bytes().get(m_end) == Some(&b' ') {
                let p_start = m_end + 1;
                let p_len = req[p_start..]
                    .bytes()
                    .take_while(|c| !c.is_ascii_whitespace())
                    .count();
                let p_end = p_start + p_len;
                let proto_ok = p_end == req.len() || req.as_bytes()[p_end] == b' ';
                if p_len > 0 && proto_ok {
                    put(&mut rec, METHOD, (rs, rs + m_end));
                    put(&mut rec, PATH, (rs + p_start, rs + p_end));
                    if p_end < req.len() {
                        put(&mut rec, PROTO, (rs + p_end + 1, re));
                    }
                }
            }
        }
        // status: exactly three digits
        if !(b.len() >= i + 4 && b[i..i + 3].iter().all(u8::is_ascii_digit) && b[i + 3] == b' ') {
            // Common format never has anything else here.
            return None;
        }
        put(&mut rec, STATUS, (i, i + 3));
        i += 4;
        // size: digits or '-'
        let ss = i;
        if b.get(i) == Some(&b'-') {
            i += 1;
        } else {
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i == ss {
            return None;
        }
        put(&mut rec, SIZE, (ss, i));
        if !self.combined {
            return (i == b.len()).then_some(rec);
        }
        if b.get(i) != Some(&b' ') {
            return None;
        }
        i += 1;
        put(&mut rec, REFERER, quoted(b, &mut i)?);
        if b.get(i) != Some(&b' ') {
            return None;
        }
        i += 1;
        put(&mut rec, AGENT, quoted(b, &mut i)?);
        // Optional trailing fields (e.g. nginx $request_time) are ignored.
        (i == b.len() || b[i] == b' ').then_some(rec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsers::ParserSpec;
    use proptest::prelude::*;
    use regex::Regex;

    const REQUEST: &str =
        r#""(?:(?P<method>[A-Z]+) (?P<path>[^\s"]+)(?: (?P<proto>[^"]*))?|(?:[^"\\]|\\.)*)""#;

    fn reference(combined: bool) -> Regex {
        let common = format!(
            r"^(?P<remote>\S+) (?P<ident>\S+) (?P<user>\S+) \[(?P<ts>[^\]]+)\] {REQUEST} (?P<status>\d{{3}}) (?P<size>\d+|-)"
        );
        let pat = if combined {
            format!(
                r#"{common} "(?P<referer>(?:[^"\\]|\\.)*)" "(?P<agent>(?:[^"\\]|\\.)*)"(?: .*)?$"#
            )
        } else {
            format!("{common}$")
        };
        Regex::new(&pat).unwrap()
    }

    fn ref_parse(re: &Regex, line: &str) -> Option<Vec<Option<String>>> {
        let caps = re.captures(line)?;
        Some(
            COLUMNS
                .iter()
                .map(|name| {
                    caps.name(name)
                        .filter(|m| !m.as_str().is_empty())
                        .map(|m| m.as_str().to_string())
                })
                .collect(),
        )
    }

    fn check(combined: bool, line: &str) -> Result<(), TestCaseError> {
        let spec = if combined {
            ParserSpec::AccessCombined
        } else {
            ParserSpec::AccessCommon
        };
        let p = spec.compile().unwrap();
        let mine = p.parse(line).map(|r| {
            r.fields
                .iter()
                .map(|f| f.as_ref().map(|c| c.to_string()))
                .collect::<Vec<_>>()
        });
        prop_assert_eq!(
            mine,
            ref_parse(&reference(combined), line),
            "line: {}",
            line
        );
        Ok(())
    }

    proptest! {
        #[test]
        fn matches_regex_reference(
            remote in "[0-9.a-f:]{1,12}",
            user in "[a-z-]{1,5}",
            ts in "[0-9A-Za-z/: +-]{1,26}",
            req in "([A-Z]{1,5} [/a-z0-9?=&.]{1,20}( HTTP/1\\.[01])?|-|[a-z\\\\ ]{0,8})",
            status in "[0-9]{3}",
            size in "([0-9]{1,6}|-)",
            referer in "[ -~]{0,12}",
            agent in "[ -~]{0,20}",
            tail in "( [0-9.]{1,5})?",
            combined in any::<bool>(),
            mangle in 0usize..4,
        ) {
            let mut line = if combined {
                format!("{remote} - {user} [{ts}] \"{req}\" {status} {size} \"{referer}\" \"{agent}\"{tail}")
            } else {
                format!("{remote} - {user} [{ts}] \"{req}\" {status} {size}")
            };
            match mangle {
                1 if line.len() > 3 => { let n = line.len() / 2; line.truncate(n); }
                2 => line.push_str(" x"),
                _ => {}
            }
            check(combined, &line)?;
        }
    }

    #[test]
    fn known_lines() {
        let l = r#"1.2.3.4 - - [10/Oct/2000:13:55:36 -0700] "GET /a b HTTP/1.0" 200 -"#;
        let p = ParserSpec::AccessCommon.compile().unwrap();
        let r = p.parse(l).unwrap();
        assert_eq!(r.get(5), Some("/a"));
        assert_eq!(r.get(6), Some("b HTTP/1.0"));
        assert!(p.parse("").is_none());
        assert!(p.parse("1.2.3.4 - -").is_none());
    }
}
