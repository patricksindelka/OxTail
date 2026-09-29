//! ANSI SGR parsing: turns escape-laden text into plain text plus spans.
//!
//! Supported: reset, bold, dim, italic, underline and their "off" codes, the
//! 16 standard colours (normal and bright), 256-colour (`38;5;n`) and 24-bit
//! (`38;2;r;g;b`) colours, in both `;` and `:` parameter forms. Every other
//! CSI sequence (cursor movement, erase, ...), OSC/DCS/APC strings and stray
//! escapes are removed silently. Malformed or truncated sequences never
//! panic: unterminated ones are dropped up to the end of the line.
//!
//! # Theme-aware colours
//!
//! The 16 standard colours map to semantic colours so they adapt to the
//! theme (red is `error`, green `success`, yellow `warn`, blue `info`,
//! magenta `accent1`, cyan `accent2`, white `accent8`, black and bright black
//! `muted`). Background variants use the `*.subtle` colours. 256-colour and
//! 24-bit values keep their exact RGB.

use crate::color::{ColorRef, SemanticColor};
use crate::style::{Style, StyledSpan};

const ESC: u8 = 0x1b;

/// Removes ANSI escapes from `line`, returning the plain text and the styled
/// spans (byte ranges into the returned text; sorted, non-overlapping,
/// aligned to char boundaries, never empty, never default-styled).
pub fn parse(line: &str) -> (String, Vec<StyledSpan>) {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut spans: Vec<StyledSpan> = Vec::new();
    let mut style = Style::default();
    let mut span_start = 0usize;
    let mut i = 0usize;
    // Start of the not-yet-copied plain run.
    let mut run = 0usize;

    macro_rules! close_span {
        () => {
            if !style.is_empty() && out.len() > span_start {
                let range = span_start..out.len();
                match spans.last_mut() {
                    Some(last) if last.range.end == range.start && last.style == style => {
                        last.range.end = range.end;
                    }
                    _ => spans.push(StyledSpan { range, style }),
                }
            }
        };
    }

    while i < bytes.len() {
        if bytes[i] != ESC {
            i += 1;
            continue;
        }
        // Copy the plain run before the escape (ESC is ASCII, so `i` and
        // `run` are char boundaries).
        out.push_str(&line[run..i]);
        let (next, sgr) = scan_escape(bytes, i);
        if let Some(params) = sgr {
            close_span!();
            apply_sgr(&mut style, &params);
            span_start = out.len();
        }
        i = next;
        run = next;
    }
    out.push_str(&line[run..]);
    close_span!();
    (out, spans)
}

/// Scans one escape sequence starting at `bytes[start] == ESC`. Returns the
/// index after it and, for an SGR sequence, its parameter bytes.
fn scan_escape(bytes: &[u8], start: usize) -> (usize, Option<Vec<u8>>) {
    let n = bytes.len();
    let mut i = start + 1;
    let Some(&kind) = bytes.get(i) else {
        return (n, None); // lone ESC at the end
    };
    match kind {
        b'[' => {
            // CSI: parameter bytes 0x30..=0x3F, intermediates 0x20..=0x2F,
            // final byte 0x40..=0x7E.
            i += 1;
            let params_start = i;
            while i < n && (0x30..=0x3f).contains(&bytes[i]) {
                i += 1;
            }
            let params_end = i;
            while i < n && (0x20..=0x2f).contains(&bytes[i]) {
                i += 1;
            }
            match bytes.get(i) {
                Some(&f) if (0x40..=0x7e).contains(&f) => {
                    let sgr = (f == b'm' && i == params_end)
                        .then(|| bytes[params_start..params_end].to_vec());
                    (i + 1, sgr)
                }
                // Malformed: drop what we consumed, keep the offending byte
                // (it is not part of the sequence).
                _ => (i, None),
            }
        }
        b']' | b'P' | b'X' | b'^' | b'_' => {
            // OSC / DCS / SOS / PM / APC: a string ended by BEL or ST.
            i += 1;
            while i < n {
                match bytes[i] {
                    0x07 => return (i + 1, None),
                    ESC if bytes.get(i + 1) == Some(&b'\\') => return (i + 2, None),
                    _ => i += 1,
                }
            }
            (n, None)
        }
        _ => {
            // Two-byte or nF escape: intermediates then one final byte.
            while i < n && (0x20..=0x2f).contains(&bytes[i]) {
                i += 1;
            }
            if i < n && (0x30..=0x7e).contains(&bytes[i]) {
                i += 1;
            }
            (i, None)
        }
    }
}

fn apply_sgr(style: &mut Style, params: &[u8]) {
    // Split into `;`-separated groups, each split on `:` (sub-parameters).
    let groups: Vec<Vec<u32>> = if params.is_empty() {
        vec![vec![0]]
    } else {
        params
            .split(|&b| b == b';')
            .map(|g| {
                g.split(|&b| b == b':')
                    .map(|p| {
                        p.iter().fold(0u32, |acc, &d| {
                            if d.is_ascii_digit() {
                                acc.saturating_mul(10).saturating_add(u32::from(d - b'0'))
                            } else {
                                acc
                            }
                        })
                    })
                    .collect()
            })
            .collect()
    };
    let mut gi = 0;
    while gi < groups.len() {
        let g = &groups[gi];
        gi += 1;
        match g[0] {
            0 => *style = Style::default(),
            1 => style.bold = true,
            2 => style.dim = true,
            3 => style.italic = true,
            4 => style.underline = true,
            22 => {
                style.bold = false;
                style.dim = false;
            }
            23 => style.italic = false,
            24 => style.underline = false,
            n @ 30..=37 => style.fg = Some(ansi16(n - 30, false)),
            39 => style.fg = None,
            n @ 40..=47 => style.bg = Some(ansi16(n - 40, true)),
            49 => style.bg = None,
            n @ 90..=97 => style.fg = Some(ansi16(n - 90 + 8, false)),
            n @ 100..=107 => style.bg = Some(ansi16(n - 100 + 8, true)),
            n @ (38 | 48) => {
                let color = if g.len() > 1 {
                    extended_color(&g[1..])
                } else {
                    // `;` form: the colour spec is spread over the next groups.
                    let rest: Vec<u32> = groups[gi..].iter().map(|x| x[0]).collect();
                    let used = match rest.first() {
                        Some(5) => 2,
                        Some(2) => 4,
                        _ => rest.len().min(1),
                    };
                    // Only the params this form consumes: trailing ones are
                    // separate attributes (`38;2;r;g;b;1` is colour + bold).
                    let c = extended_color(&rest[..used.min(rest.len())]);
                    gi = (gi + used).min(groups.len());
                    c
                };
                if let Some(c) = color {
                    if n == 38 {
                        style.fg = Some(c);
                    } else {
                        style.bg = Some(c);
                    }
                }
            }
            _ => {} // unsupported attribute: ignore
        }
    }
}

/// Parses the tail of a `38`/`48` spec: `5;n` or `2;r;g;b` (the colon form
/// may carry an extra colour-space id: `2::r:g:b` / `2:cs:r:g:b`).
fn extended_color(p: &[u32]) -> Option<ColorRef> {
    match p.first()? {
        5 => Some(ansi256(u8::try_from(*p.get(1)?).ok()?)),
        2 => {
            let rgb = if p.len() >= 5 { &p[2..5] } else { p.get(1..4)? };
            Some(ColorRef::rgb(
                u8::try_from(rgb[0]).ok()?,
                u8::try_from(rgb[1]).ok()?,
                u8::try_from(rgb[2]).ok()?,
            ))
        }
        _ => None,
    }
}

fn ansi16(n: u32, background: bool) -> ColorRef {
    let color = match n % 8 {
        0 => SemanticColor::Muted,
        1 => SemanticColor::Error,
        2 => SemanticColor::Success,
        3 => SemanticColor::Warn,
        4 => SemanticColor::Info,
        5 => SemanticColor::Accent1,
        6 => SemanticColor::Accent2,
        _ => SemanticColor::Accent8,
    };
    if background {
        ColorRef::subtle(color)
    } else {
        ColorRef::solid(color)
    }
}

fn ansi256(n: u8) -> ColorRef {
    match n {
        0..=15 => ansi16(u32::from(n), false),
        16..=231 => {
            let n = n - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            ColorRef::rgb(level(n / 36), level(n / 6 % 6), level(n % 6))
        }
        232..=255 => {
            let v = 8 + 10 * (n - 232);
            ColorRef::rgb(v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(range: std::ops::Range<usize>, style: Style) -> StyledSpan {
        StyledSpan { range, style }
    }

    fn red() -> Style {
        Style::fg(ColorRef::solid(SemanticColor::Error))
    }

    #[test]
    fn extended_colour_followed_by_more_params() {
        let rgb = |r, g, b| Style::fg(ColorRef::rgb(r, g, b));
        // `;` form: trailing params are separate attributes.
        let (_, s) = parse("\x1b[38;2;255;0;0;1mX");
        assert_eq!(s, vec![span(0..1, rgb(255, 0, 0).bold())]);
        let (_, s) = parse("\x1b[38;5;196;1mX");
        assert_eq!(s, vec![span(0..1, Style::fg(ansi256(196)).bold())]);
        let (_, s) = parse("\x1b[38;2;1;2;3;4;5mX");
        assert_eq!(s, vec![span(0..1, rgb(1, 2, 3).underline())]);
        // Colon form, with and without a colour-space id.
        let (_, s) = parse("\x1b[38:2:10:20:30mX");
        assert_eq!(s, vec![span(0..1, rgb(10, 20, 30))]);
        let (_, s) = parse("\x1b[38:2:0:10:20:30mX");
        assert_eq!(s, vec![span(0..1, rgb(10, 20, 30))]);
        let (_, s) = parse("\x1b[38:2::10:20:30;1mX");
        assert_eq!(s, vec![span(0..1, rgb(10, 20, 30).bold())]);
    }

    #[test]
    fn plain_text_passes_through() {
        let (t, s) = parse("hello wörld");
        assert_eq!(t, "hello wörld");
        assert!(s.is_empty());
    }

    #[test]
    fn basic_colour_and_reset() {
        let (t, s) = parse("a\x1b[31mred\x1b[0m b");
        assert_eq!(t, "ared b");
        assert_eq!(s, vec![span(1..4, red())]);
    }

    #[test]
    fn attributes_combine_and_turn_off() {
        let (t, s) = parse("\x1b[1;3;4mX\x1b[22mY\x1b[23;24mZ");
        assert_eq!(t, "XYZ");
        assert_eq!(
            s,
            vec![
                span(0..1, Style::default().bold().italic().underline()),
                span(1..2, Style::default().italic().underline()),
            ]
        );
    }

    #[test]
    fn bright_and_background_colours() {
        let (_, s) = parse("\x1b[91;102mX");
        assert_eq!(s[0].style.fg, Some(ColorRef::solid(SemanticColor::Error)));
        assert_eq!(
            s[0].style.bg,
            Some(ColorRef::subtle(SemanticColor::Success))
        );
    }

    #[test]
    fn extended_colours_semicolon_and_colon_forms() {
        let (_, s) = parse(
            "\x1b[38;5;196mA\x1b[0m\x1b[38;2;1;2;3mB\x1b[0m\x1b[48:2::9:8:7mC\x1b[0m\x1b[38:5:232mD",
        );
        assert_eq!(s[0].style.fg, Some(ColorRef::rgb(255, 0, 0)));
        assert_eq!(s[1].style.fg, Some(ColorRef::rgb(1, 2, 3)));
        assert_eq!(s[2].style.bg, Some(ColorRef::rgb(9, 8, 7)));
        assert_eq!(s[3].style.fg, Some(ColorRef::rgb(8, 8, 8)));
    }

    #[test]
    fn extended_colour_followed_by_more_codes() {
        let (_, s) = parse("\x1b[38;5;21;1mX");
        assert_eq!(s[0].style.fg, Some(ColorRef::rgb(0, 0, 255)));
        assert!(s[0].style.bold);
    }

    #[test]
    fn other_sequences_are_dropped() {
        let (t, s) = parse(
            "a\x1b[2Kb\x1b[10;20Hc\x1b]0;window title\x07d\x1b]8;;http://x\x1b\\e\x1b(Bf\x1b=g",
        );
        assert_eq!(t, "abcdefg");
        assert!(s.is_empty());
    }

    #[test]
    fn malformed_and_truncated_sequences() {
        assert_eq!(parse("a\x1b").0, "a");
        assert_eq!(parse("a\x1b[").0, "a");
        assert_eq!(parse("a\x1b[31").0, "a");
        assert_eq!(parse("a\x1b]title never ends").0, "a");
        assert_eq!(parse("a\x1b[\u{e9}b").0, "a\u{e9}b");
        assert_eq!(parse("\x1b[38;5mX").0, "X");
        assert_eq!(parse("\x1b[99999999999999999999mX").0, "X");
        assert_eq!(parse("\x1b[m\x1b[;m").0, "");
    }

    #[test]
    fn trailing_style_covers_the_rest_and_adjacent_equal_spans_merge() {
        let (t, s) = parse("\x1b[31ma\x1b[31mb\x1b[0mc\x1b[31md");
        assert_eq!(t, "abcd");
        assert_eq!(s, vec![span(0..2, red()), span(3..4, red())]);
    }

    #[test]
    fn multibyte_text_between_escapes_keeps_char_boundaries() {
        let (t, s) = parse("\x1b[31m\u{e9}\u{1f600}\x1b[0m\u{e9}");
        assert_eq!(t, "\u{e9}\u{1f600}\u{e9}");
        assert_eq!(s, vec![span(0..6, red())]);
    }
}
