//! A small glob matcher for profile `match_files`.
//!
//! Syntax: `*` (any run of characters except `/`), `**` (any run including
//! `/`; `**/` also matches zero directories), `?` (one character except
//! `/`), `[abc]`, `[a-z]`, `[!abc]` / `[^abc]` (character classes). A
//! backslash in a pattern is a path separator (Windows habit); to match a literal
//! `*`, `?` or `[` write it as a class (`[*]`). Matching is done on the whole text;
//! path separators are normalised to `/` by the caller ([`Glob::is_match_path`]
//! does it). Matching is `O(pattern * text)`: no exponential backtracking.

use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Lit(char),
    /// `?`
    One,
    /// `*`
    Star,
    /// `**`
    Deep,
    /// `**/`
    DeepSlash,
    Class {
        negate: bool,
        ranges: Vec<(char, char)>,
    },
}

/// Longest supported pattern, in tokens. A longer pattern compiles to a glob
/// that matches nothing.
pub const MAX_TOKENS: usize = 1024;

/// A compiled glob pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glob {
    toks: Vec<Tok>,
    ci: bool,
    has_slash: bool,
    specificity: usize,
    /// Pattern exceeded [`MAX_TOKENS`]: matches nothing.
    never: bool,
}

impl Glob {
    /// Compiles `pattern`; `case_insensitive` folds ASCII and simple Unicode
    /// case. Never fails: an unterminated `[` is taken literally, and a pattern
    /// longer than [`MAX_TOKENS`] tokens matches nothing.
    pub fn new(pattern: &str, case_insensitive: bool) -> Glob {
        let chars: Vec<char> = pattern.replace('\\', "\u{0}").chars().collect();
        // A NUL stands for a backslash separator and becomes `/` below.
        let mut toks = Vec::new();
        let mut i = 0;
        let mut never = false;
        while i < chars.len() {
            if toks.len() > MAX_TOKENS {
                never = true;
                toks.clear();
                break;
            }
            let c = chars[i];
            match c {
                '*' => {
                    let mut j = i;
                    while j < chars.len() && chars[j] == '*' {
                        j += 1;
                    }
                    if j - i >= 2 {
                        if chars.get(j) == Some(&'/') {
                            toks.push(Tok::DeepSlash);
                            j += 1;
                        } else {
                            toks.push(Tok::Deep);
                        }
                    } else {
                        toks.push(Tok::Star);
                    }
                    i = j;
                }
                '?' => {
                    toks.push(Tok::One);
                    i += 1;
                }
                '[' => match parse_class(&chars, i + 1, case_insensitive) {
                    Some((t, next)) => {
                        toks.push(t);
                        i = next;
                    }
                    None => {
                        toks.push(Tok::Lit('['));
                        i += 1;
                    }
                },
                '\u{0}' => {
                    toks.push(Tok::Lit('/'));
                    i += 1;
                }
                c => {
                    toks.push(Tok::Lit(fold(c, case_insensitive)));
                    i += 1;
                }
            }
        }
        if toks.len() > MAX_TOKENS {
            never = true;
            toks.clear();
        }
        let has_slash = toks
            .iter()
            .any(|t| matches!(t, Tok::Lit('/') | Tok::DeepSlash));
        let specificity = toks
            .iter()
            .filter(|t| matches!(t, Tok::Lit(_) | Tok::Class { .. }))
            .count();
        Glob {
            toks,
            ci: case_insensitive,
            has_slash,
            specificity,
            never,
        }
    }

    /// Number of literal characters: a rough "how specific" score used to
    /// pick between several matching profiles.
    pub fn specificity(&self) -> usize {
        self.specificity
    }

    /// Whether `text` matches the whole pattern.
    ///
    /// Iterative two-row dynamic programme: constant stack, `O(pattern * text)`
    /// time, `O(text)` memory.
    pub fn is_match(&self, text: &str) -> bool {
        if self.never {
            return false;
        }
        let text: Vec<char> = text.chars().map(|c| fold(c, self.ci)).collect();
        let m = text.len();
        if self.toks.len().saturating_mul(m + 1) > 20_000_000 {
            return false;
        }
        // next[j]: does toks[i+1..] match text[j..]?  Start with i = n.
        let mut next = vec![false; m + 1];
        next[m] = true;
        let mut cur = vec![false; m + 1];
        for tok in self.toks.iter().rev() {
            let mut any_slash_then = false; // DeepSlash helper, see below
            for j in (0..=m).rev() {
                let ch = text.get(j).copied();
                cur[j] = match tok {
                    Tok::Lit(c) => ch == Some(*c) && next[j + 1],
                    Tok::One => ch.is_some_and(|c| c != '/') && next[j + 1],
                    Tok::Class { negate, ranges } => {
                        ch.is_some_and(|c| {
                            c != '/' && (ranges.iter().any(|&(a, b)| a <= c && c <= b) != *negate)
                        }) && next[j + 1]
                    }
                    Tok::Star => next[j] || (ch.is_some_and(|c| c != '/') && cur[j + 1]),
                    Tok::Deep => next[j] || (ch.is_some() && cur[j + 1]),
                    Tok::DeepSlash => {
                        // Zero directories, or any prefix ending in '/'.
                        if ch == Some('/') && next[j + 1] {
                            any_slash_then = true;
                        }
                        next[j] || any_slash_then
                    }
                };
            }
            std::mem::swap(&mut next, &mut cur);
        }
        next[0]
    }

    /// Matches a file path. Patterns containing a `/` are matched against the
    /// whole path (separators normalised to `/`); others only against the file
    /// name.
    pub fn is_match_path(&self, path: &Path) -> bool {
        if self.has_slash {
            let s = path.to_string_lossy().replace('\\', "/");
            // Allow a pattern like "logs/*.log" to match "/var/logs/a.log".
            self.is_match(&s)
                || s.match_indices('/')
                    .any(|(i, _)| self.is_match(&s[i + 1..]))
        } else {
            path.file_name()
                .is_some_and(|n| self.is_match(&n.to_string_lossy()))
        }
    }
}

fn fold(c: char, ci: bool) -> char {
    if ci {
        c.to_lowercase().next().unwrap_or(c)
    } else {
        c
    }
}

fn parse_class(chars: &[char], start: usize, ci: bool) -> Option<(Tok, usize)> {
    let mut i = start;
    let negate = matches!(chars.get(i), Some('!' | '^'));
    if negate {
        i += 1;
    }
    let mut ranges = Vec::new();
    let mut first = true;
    loop {
        let c = *chars.get(i)?;
        if c == ']' && !first {
            i += 1;
            break;
        }
        first = false;
        let lo = fold(c, ci);
        if chars.get(i + 1) == Some(&'-') && chars.get(i + 2).is_some_and(|&e| e != ']') {
            let hi = fold(chars[i + 2], ci);
            ranges.push((lo.min(hi), lo.max(hi)));
            i += 3;
        } else {
            ranges.push((lo, lo));
            i += 1;
        }
    }
    Some((Tok::Class { negate, ranges }, i))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pathological_pattern_does_not_overflow_stack() {
        let h = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let g = Glob::new(&"**/".repeat(75_000), false);
                assert!(!g.is_match(&"a/".repeat(1000)));
                let g = Glob::new(&"**/".repeat(500), false);
                let text = "d/".repeat(3000) + "x";
                assert!(g.is_match(&text) || !g.is_match(&text)); // must simply return
                let g = Glob::new(&"*".repeat(3000), false);
                assert!(!g.is_match("abc"));
                let g = Glob::new("**/x", false);
                assert!(g.is_match(&("d/".repeat(100_000) + "x")));
            })
            .expect("spawn");
        h.join().expect("no overflow or panic");
    }

    #[test]
    fn over_long_pattern_never_matches() {
        assert!(
            !Glob::new(&"a".repeat(MAX_TOKENS + 1), false).is_match(&"a".repeat(MAX_TOKENS + 1))
        );
        assert!(Glob::new(&"a".repeat(MAX_TOKENS), false).is_match(&"a".repeat(MAX_TOKENS)));
    }

    fn m(p: &str, t: &str) -> bool {
        Glob::new(p, false).is_match(t)
    }

    #[test]
    fn table() {
        let cases: &[(&str, &str, bool)] = &[
            ("*.log", "a.log", true),
            ("*.log", ".log", true),
            ("*.log", "a.txt", false),
            ("*.log", "dir/a.log", false),
            ("a?c", "abc", true),
            ("a?c", "ac", false),
            ("a?c", "a/c", false),
            ("[abc].txt", "b.txt", true),
            ("[abc].txt", "d.txt", false),
            ("[a-c].txt", "b.txt", true),
            ("[!a-c].txt", "d.txt", true),
            ("[!a-c].txt", "b.txt", false),
            ("[^a]x", "bx", true),
            ("[]a]x", "]x", true),
            ("[unterminated", "[unterminated", true),
            ("**/*.log", "a.log", true),
            ("**/*.log", "x/y/a.log", true),
            ("**/*.log", "x/y/a.txt", false),
            ("logs/**", "logs/a/b", true),
            ("logs/**/x.log", "logs/x.log", true),
            ("logs/**/x.log", "logs/a/b/x.log", true),
            ("logs/*/x.log", "logs/a/b/x.log", false),
            ("*service*.log", "my-service-1.log", true),
            ("*service*.log", "my-servic-1.log", false),
            ("", "", true),
            ("", "a", false),
            ("*", "", true),
            ("**", "a/b/c", true),
            ("a*b*c*d*e*f*g", "aXbXcXdXeXfXg", true),
        ];
        for (p, t, want) in cases {
            assert_eq!(m(p, t), *want, "{p} vs {t}");
        }
    }

    #[test]
    fn case_insensitive() {
        let g = Glob::new("*.LOG", true);
        assert!(g.is_match("Server.log"));
        assert!(!Glob::new("*.LOG", false).is_match("Server.log"));
        assert!(Glob::new("[A-C]x", true).is_match("bx"));
    }

    #[test]
    fn path_matching() {
        let g = Glob::new("*.log", false);
        assert!(g.is_match_path(Path::new("/var/log/nginx/access.log")));
        let g = Glob::new("nginx/*.log", false);
        assert!(g.is_match_path(Path::new("/var/log/nginx/access.log")));
        assert!(!g.is_match_path(Path::new("/var/log/apache/access.log")));
        let g = Glob::new("nginx\\*.log", false);
        assert!(g.is_match_path(Path::new("C:\\logs\\nginx\\access.log")));
    }

    #[test]
    fn specificity_orders() {
        assert!(
            Glob::new("access.log", false).specificity() > Glob::new("*.log", false).specificity()
        );
    }

    #[test]
    fn pathological_pattern_is_fast() {
        let g = Glob::new(&"a*".repeat(30), false);
        assert!(!g.is_match(&"a".repeat(29)));
    }
}
