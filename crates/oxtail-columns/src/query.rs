//! The small typed query language (PLAN.md section 8.4).
//!
//! ```text
//! level:ERROR                  column equals (case-insensitive; `*` is a wildcard)
//! level:(ERROR|FATAL)          alternatives
//! status>=500  duration>250ms  numeric and unit-aware comparisons
//! msg~"timeout after \d+"      regex on a column
//! -path:/health   NOT ...      exclude
//! status!=200                  not equal
//! ts>2026-09-29T10:00 ts<+15m  time ranges (absolute, or relative)
//! foo "bar baz"                case-insensitive substring over the whole line
//! a b   a AND b   a OR b   (a OR b) c
//! ```
//!
//! * Implicit `AND` between terms; `AND`, `OR`, `NOT` are upper-case keywords.
//! * `ts>-1h` is relative to *now* (captured when the query is parsed);
//!   `ts<+15m` is relative to the lower bound (`ts>...`) in the same query.
//! * Quoted values use `"..."`; inside quotes only `\"` is an escape, so regex
//!   escapes such as `\d` pass through untouched.
//! * A word only counts as a column term when it starts with a letter, `_` or
//!   `@` and is immediately followed by an operator. Quote anything else that
//!   should be searched literally (`"http://x"`).
//!
//! Positions in [`QueryError`] are byte offsets into the query string.

use std::cmp::Ordering;
use std::sync::{Arc, OnceLock};

use regex::{Regex, RegexBuilder};
use thiserror::Error;

use crate::level::Level;
use crate::model::{ColumnKind, Record, Schema};
use crate::quantity::{Quantity, Unit, canonical_pair, parse_quantity};
use crate::timestamp::TimestampParser;

/// Maximum parenthesis nesting depth.
const MAX_DEPTH: usize = 64;

/// A syntax (or regex) error in a query.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message} (at position {position})")]
pub struct QueryError {
    /// Byte offset into the query string where the problem was found.
    pub position: usize,
    /// Human-readable explanation, suitable for showing under the query box.
    pub message: String,
}

/// A compiled query.
#[derive(Debug, Clone)]
pub struct Query {
    root: Node,
    now_ns: i64,
}

#[derive(Debug, Clone)]
enum Node {
    All,
    /// Lower-cased needle for the whole-line substring search.
    Text(String),
    Cmp(Box<Cmp>),
    Not(Box<Node>),
    And(Vec<Node>),
    Or(Vec<Node>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Lt,
    Le,
    Gt,
    Ge,
    Regex,
}

#[derive(Debug, Clone)]
struct Cmp {
    column: String,
    pos: usize,
    op: Op,
    values: Vec<Arc<Value>>,
    regex: Option<Regex>,
    /// Lower bound that a `+15m` value is relative to.
    base: Option<Arc<Value>>,
}

#[derive(Debug)]
struct Value {
    text: String,
    lower: String,
    quantity: Option<Quantity>,
    glob: bool,
    level: Option<Level>,
    time: OnceLock<Option<i64>>,
    pos: usize,
}

impl Value {
    fn new(text: String, pos: usize) -> Value {
        Value {
            lower: text.to_lowercase(),
            quantity: parse_quantity(&text),
            glob: text.contains('*'),
            level: Level::parse(&text),
            time: OnceLock::new(),
            text,
            pos,
        }
    }

    fn relative_sign(&self) -> Option<char> {
        let q = self.quantity?;
        match (q.unit, q.sign) {
            (Some(Unit::Duration(_)), Some(s)) => Some(s),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------- parsing

#[derive(Clone, Copy, PartialEq, Eq)]
enum RawOp {
    Colon,
    Eq,
    Ne,
    Re,
    NRe,
    Gt,
    Ge,
    Lt,
    Le,
}

fn raw_op_at(s: &str) -> Option<(RawOp, usize)> {
    let b = s.as_bytes();
    let two = b.get(..2);
    if two == Some(b">=") {
        return Some((RawOp::Ge, 2));
    }
    if two == Some(b"<=") {
        return Some((RawOp::Le, 2));
    }
    if two == Some(b"!=") {
        return Some((RawOp::Ne, 2));
    }
    if two == Some(b"!~") {
        return Some((RawOp::NRe, 2));
    }
    match b.first()? {
        b':' => Some((RawOp::Colon, 1)),
        b'=' => Some((RawOp::Eq, 1)),
        b'~' => Some((RawOp::Re, 1)),
        b'>' => Some((RawOp::Gt, 1)),
        b'<' => Some((RawOp::Lt, 1)),
        _ => None,
    }
}

fn op_text(op: RawOp) -> &'static str {
    match op {
        RawOp::Colon => ":",
        RawOp::Eq => "=",
        RawOp::Ne => "!=",
        RawOp::Re => "~",
        RawOp::NRe => "!~",
        RawOp::Gt => ">",
        RawOp::Ge => ">=",
        RawOp::Lt => "<",
        RawOp::Le => "<=",
    }
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | '@' | '-' | '$')
}

struct Parser<'s> {
    src: &'s str,
    pos: usize,
    depth: usize,
}

type PResult<T> = Result<T, QueryError>;

impl<'s> Parser<'s> {
    fn err<T>(&self, position: usize, message: impl Into<String>) -> PResult<T> {
        Err(QueryError {
            position,
            message: message.into(),
        })
    }

    fn rest(&self) -> &'s str {
        &self.src[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn eof(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }

    /// True if an upper-case keyword starts here as a whole word.
    fn at_keyword(&self, kw: &str) -> bool {
        let r = self.rest();
        if !r.starts_with(kw) {
            return false;
        }
        match r[kw.len()..].chars().next() {
            None => kw != "NOT",
            Some(c) => c.is_whitespace() || c == '(' || (c == ')' && kw != "NOT"),
        }
    }

    fn parse_query(mut self) -> PResult<Node> {
        self.skip_ws();
        if self.eof() {
            return Ok(Node::All);
        }
        let n = self.parse_or()?;
        self.skip_ws();
        if !self.eof() {
            // parse_or only stops early at an unmatched ')'.
            return self.err(self.pos, "unmatched ')' (no '(' to close)");
        }
        Ok(n)
    }

    fn parse_or(&mut self) -> PResult<Node> {
        let mut parts = vec![self.parse_and()?];
        loop {
            self.skip_ws();
            if self.at_keyword("OR") {
                self.pos += 2;
                parts.push(self.parse_and()?);
            } else {
                break;
            }
        }
        Ok(if parts.len() == 1 {
            parts.remove(0)
        } else {
            Node::Or(parts)
        })
    }

    fn parse_and(&mut self) -> PResult<Node> {
        let mut parts: Vec<Node> = Vec::new();
        loop {
            self.skip_ws();
            let at_or = self.at_keyword("OR");
            if self.eof() || self.peek() == Some(')') || at_or {
                if parts.is_empty() {
                    let what = if at_or {
                        "expected a search term before 'OR'"
                    } else if self.eof() {
                        "expected a search term at the end of the query"
                    } else {
                        "expected a search term before ')'"
                    };
                    return self.err(self.pos, what);
                }
                break;
            }
            if self.at_keyword("AND") {
                let kw = self.pos;
                self.pos += 3;
                self.skip_ws();
                if self.eof() || self.peek() == Some(')') || self.at_keyword("OR") {
                    return self.err(kw, "expected a search term after 'AND'");
                }
                if parts.is_empty() {
                    return self.err(kw, "expected a search term before 'AND'");
                }
                continue;
            }
            parts.push(self.parse_unary()?);
        }
        Ok(if parts.len() == 1 {
            parts.remove(0)
        } else {
            Node::And(parts)
        })
    }

    fn parse_unary(&mut self) -> PResult<Node> {
        let mut negated = false;
        loop {
            if self.at_keyword("NOT") {
                let kw = self.pos;
                self.pos += 3;
                negated = !negated;
                self.skip_ws();
                if self.eof() || self.peek() == Some(')') {
                    return self.err(kw, "expected a search term after 'NOT'");
                }
                continue;
            }
            if self.peek() == Some('-') {
                let mut it = self.rest().chars();
                it.next();
                if let Some(n) = it.next() {
                    if !n.is_whitespace() && n != ')' {
                        self.pos += 1;
                        negated = !negated;
                        continue;
                    }
                }
            }
            break;
        }
        let n = self.parse_primary()?;
        Ok(if negated { Node::Not(Box::new(n)) } else { n })
    }

    fn parse_primary(&mut self) -> PResult<Node> {
        match self.peek() {
            Some('(') => {
                let open = self.pos;
                self.pos += 1;
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return self.err(open, "parentheses are nested too deeply");
                }
                let n = self.parse_or()?;
                self.skip_ws();
                if self.peek() == Some(')') {
                    self.pos += 1;
                } else {
                    return self.err(open, "missing ')' to close the '(' opened here");
                }
                self.depth -= 1;
                Ok(n)
            }
            Some('"') => {
                let (s, _) = self.parse_quoted()?;
                Ok(Node::Text(s.to_lowercase()))
            }
            _ => self.parse_word_or_term(),
        }
    }

    /// Parses `"..."` starting at the opening quote. Only `\"` is an escape.
    fn parse_quoted(&mut self) -> PResult<(String, usize)> {
        let open = self.pos;
        self.pos += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return self.err(open, "unterminated quote: missing closing '\"'"),
                Some('"') => {
                    self.pos += 1;
                    return Ok((out, open));
                }
                Some('\\') if self.rest().as_bytes().get(1) == Some(&b'"') => {
                    out.push('"');
                    self.pos += 2;
                }
                Some(c) => {
                    out.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
    }

    fn parse_word_or_term(&mut self) -> PResult<Node> {
        let start = self.pos;
        let first = self.peek();
        let mut i = start;
        if first.is_some_and(|c| c.is_alphabetic() || c == '_' || c == '@') {
            for c in self.src[start..].chars() {
                if is_ident_char(c) {
                    i += c.len_utf8();
                } else {
                    break;
                }
            }
            if let Some((op, len)) = raw_op_at(&self.src[i..]) {
                let is_url = op == RawOp::Colon && self.src[i + len..].starts_with("//");
                if !is_url {
                    return self.parse_term(start, i, op, len);
                }
            }
        }
        // Bare word: plain text search.
        let mut end = start;
        for c in self.src[start..].chars() {
            if c.is_whitespace() || c == ')' {
                break;
            }
            end += c.len_utf8();
        }
        if end == start {
            return self.err(start, "expected a search term");
        }
        self.pos = end;
        Ok(Node::Text(self.src[start..end].to_lowercase()))
    }

    fn parse_term(&mut self, start: usize, ident_end: usize, op: RawOp, oplen: usize) -> PResult<Node> {
        let column = self.src[start..ident_end].to_string();
        self.pos = ident_end + oplen;
        match self.peek() {
            None => {
                return self.err(
                    self.pos,
                    format!("expected a value after '{column}{}'", op_text(op)),
                );
            }
            Some(c) if c.is_whitespace() || c == ')' => {
                return self.err(
                    self.pos,
                    format!(
                        "expected a value after '{column}{}' (no spaces around operators; quote values that contain spaces)",
                        op_text(op)
                    ),
                );
            }
            _ => {}
        }
        let allow_alts = matches!(op, RawOp::Colon | RawOp::Eq | RawOp::Ne);
        let is_regex = matches!(op, RawOp::Re | RawOp::NRe);
        let raw = self.parse_values(allow_alts, is_regex, op)?;
        let values: Vec<Arc<Value>> = raw
            .into_iter()
            .map(|(t, p)| Arc::new(Value::new(t, p)))
            .collect();
        let mut regex = None;
        if is_regex {
            let v = &values[0];
            match RegexBuilder::new(&v.text).size_limit(1 << 20).nest_limit(64).build() {
                Ok(r) => regex = Some(r),
                Err(e) => {
                    let s = e.to_string();
                    let last = s.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
                    return self.err(
                        v.pos,
                        format!(
                            "invalid regular expression: {}",
                            last.trim().trim_start_matches("error: ")
                        ),
                    );
                }
            }
        }
        let (cop, negate) = match op {
            RawOp::Colon | RawOp::Eq => (Op::Eq, false),
            RawOp::Ne => (Op::Eq, true),
            RawOp::Re => (Op::Regex, false),
            RawOp::NRe => (Op::Regex, true),
            RawOp::Gt => (Op::Gt, false),
            RawOp::Ge => (Op::Ge, false),
            RawOp::Lt => (Op::Lt, false),
            RawOp::Le => (Op::Le, false),
        };
        let node = Node::Cmp(Box::new(Cmp {
            column,
            pos: start,
            op: cop,
            values,
            regex,
            base: None,
        }));
        Ok(if negate { Node::Not(Box::new(node)) } else { node })
    }

    /// Parses the value(s) after an operator: a word, a quoted string or
    /// `(a|b|c)`. A bare `a|b` is also alternatives (except for regexes, where
    /// parentheses are balanced and `|` is regex alternation).
    fn parse_values(
        &mut self,
        allow_alts: bool,
        is_regex: bool,
        op: RawOp,
    ) -> PResult<Vec<(String, usize)>> {
        if self.peek() == Some('(') && !is_regex {
            let open = self.pos;
            if !allow_alts {
                return self.err(
                    open,
                    format!(
                        "alternatives like (a|b) can only be used with ':' or '=', not '{}'",
                        op_text(op)
                    ),
                );
            }
            self.pos += 1;
            let mut items = Vec::new();
            loop {
                self.skip_ws();
                items.push(self.parse_item()?);
                self.skip_ws();
                match self.peek() {
                    Some('|') => self.pos += 1,
                    Some(')') => {
                        self.pos += 1;
                        break;
                    }
                    None => return self.err(open, "missing ')' to close the alternatives opened here"),
                    Some(_) => return self.err(self.pos, "expected '|' or ')' in the list of alternatives"),
                }
            }
            return Ok(items);
        }
        if self.peek() == Some('"') {
            let (s, p) = self.parse_quoted()?;
            return Ok(vec![(s, p)]);
        }
        let start = self.pos;
        let mut end = start;
        let mut balance = 0usize;
        for c in self.src[start..].chars() {
            if c.is_whitespace() {
                break;
            }
            if c == '(' && is_regex {
                balance += 1;
            } else if c == ')' {
                if is_regex && balance > 0 {
                    balance -= 1;
                } else {
                    break;
                }
            }
            end += c.len_utf8();
        }
        self.pos = end;
        let word = &self.src[start..end];
        if allow_alts && !is_regex && word.contains('|') {
            let mut items = Vec::new();
            let mut off = start;
            for part in word.split('|') {
                if part.is_empty() {
                    return self.err(off, "empty alternative in 'a|b'");
                }
                items.push((part.to_string(), off));
                off += part.len() + 1;
            }
            return Ok(items);
        }
        Ok(vec![(word.to_string(), start)])
    }

    /// One alternative inside `( | )`: a quoted string or a bare word.
    fn parse_item(&mut self) -> PResult<(String, usize)> {
        if self.peek() == Some('"') {
            return self.parse_quoted();
        }
        let start = self.pos;
        let mut end = start;
        for c in self.src[start..].chars() {
            if c.is_whitespace() || c == '|' || c == ')' {
                break;
            }
            end += c.len_utf8();
        }
        if end == start {
            return self.err(start, "expected a value in the list of alternatives");
        }
        self.pos = end;
        Ok((self.src[start..end].to_string(), start))
    }
}

/// Collects `column > value` / `>=` bounds usable as the base of `+15m`.
fn collect_bases(node: &Node, out: &mut Vec<(String, Arc<Value>)>) {
    match node {
        Node::Cmp(c) => {
            if matches!(c.op, Op::Gt | Op::Ge)
                && c.values.len() == 1
                && c.values[0].relative_sign() != Some('+')
            {
                out.push((c.column.to_ascii_lowercase(), c.values[0].clone()));
            }
        }
        Node::Not(n) => collect_bases(n, out),
        Node::And(v) | Node::Or(v) => v.iter().for_each(|n| collect_bases(n, out)),
        Node::All | Node::Text(_) => {}
    }
}

fn assign_bases(node: &mut Node, bases: &[(String, Arc<Value>)]) {
    match node {
        Node::Cmp(c) => {
            if matches!(c.op, Op::Lt | Op::Le | Op::Gt | Op::Ge)
                && c.values.len() == 1
                && c.values[0].relative_sign() == Some('+')
            {
                let col = c.column.to_ascii_lowercase();
                c.base = bases.iter().find(|(k, _)| *k == col).map(|(_, v)| v.clone());
            }
        }
        Node::Not(n) => assign_bases(n, bases),
        Node::And(v) | Node::Or(v) => v.iter_mut().for_each(|n| assign_bases(n, bases)),
        Node::All | Node::Text(_) => {}
    }
}

// --------------------------------------------------------------- matching

struct Ctx<'a> {
    line: &'a str,
    record: Option<&'a Record<'a>>,
    schema: Option<&'a Schema>,
    ts: &'a dyn TimestampParser,
    now: i64,
}

fn ascii_needle_in(hay: &[u8], needle: &[u8]) -> bool {
    let first = needle[0];
    let upper = first.to_ascii_uppercase();
    let mut from = 0;
    while let Some(off) = memchr::memchr2(first, upper, &hay[from..]) {
        let p = from + off;
        if p + needle.len() > hay.len() {
            return false;
        }
        if hay[p..p + needle.len()].eq_ignore_ascii_case(needle) {
            return true;
        }
        from = p + 1;
    }
    false
}

/// Case-insensitive substring test; `needle_lower` must already be lower-case.
fn contains_ci(hay: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if needle_lower.is_ascii() {
        ascii_needle_in(hay.as_bytes(), needle_lower.as_bytes())
    } else {
        hay.to_lowercase().contains(needle_lower)
    }
}

fn eq_ci(cell: &str, v: &Value) -> bool {
    if cell.is_ascii() && v.text.is_ascii() {
        cell.eq_ignore_ascii_case(&v.text)
    } else {
        cell.chars().flat_map(char::to_lowercase).eq(v.lower.chars())
    }
}

fn cmp_ci(cell: &str, v: &Value) -> Ordering {
    cell.chars()
        .flat_map(char::to_lowercase)
        .cmp(v.lower.chars())
}

/// Case-insensitive glob where `*` matches any run of characters.
fn glob_ci(pattern_lower: &str, cell: &str) -> bool {
    let text = cell.to_lowercase();
    let mut parts = pattern_lower.split('*');
    let first = parts.next().unwrap_or("");
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let segs: Vec<&str> = parts.collect();
    let Some((last, middle)) = segs.split_last() else {
        return rest.is_empty();
    };
    for m in middle {
        match rest.find(m) {
            Some(i) => rest = &rest[i + m.len()..],
            None => return false,
        }
    }
    rest.ends_with(last) && rest.len() >= last.len()
}

fn approx_eq(a: f64, b: f64) -> bool {
    a == b || (a - b).abs() <= 1e-9 * a.abs().max(b.abs())
}

impl Query {
    /// Parses a query. Time-relative terms (`ts>-1h`) are relative to the
    /// moment of parsing.
    pub fn parse(src: &str) -> Result<Query, QueryError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_nanos()).ok())
            .unwrap_or(0);
        Query::parse_with_now(src, now)
    }

    /// Like [`Query::parse`] with an explicit "now" (unix nanoseconds), for
    /// deterministic tests and replays.
    pub fn parse_with_now(src: &str, now_ns: i64) -> Result<Query, QueryError> {
        let mut root = Parser {
            src,
            pos: 0,
            depth: 0,
        }
        .parse_query()?;
        let mut bases = Vec::new();
        collect_bases(&root, &mut bases);
        assign_bases(&mut root, &bases);
        Ok(Query { root, now_ns })
    }

    /// True for an empty query, which matches every line.
    pub fn is_empty(&self) -> bool {
        matches!(self.root, Node::All)
    }

    /// Evaluates the query against a line.
    ///
    /// Column terms need `record` and `schema`; without them (plain-text
    /// files) they do not match. Plain-text terms search `line`.
    pub fn matches(
        &self,
        line: &str,
        record: Option<&Record<'_>>,
        schema: Option<&Schema>,
        ts: &dyn TimestampParser,
    ) -> bool {
        let ctx = Ctx {
            line,
            record,
            schema,
            ts,
            now: self.now_ns,
        };
        eval(&self.root, &ctx)
    }

    /// Reports column names that do not exist in `schema`, and relative
    /// times (`ts<+15m`) that lack a lower bound. Empty means all good.
    pub fn check(&self, schema: &Schema) -> Vec<QueryError> {
        let mut out = Vec::new();
        check_node(&self.root, schema, &mut out);
        out
    }
}

fn check_node(node: &Node, schema: &Schema, out: &mut Vec<QueryError>) {
    match node {
        Node::Cmp(c) => match schema.find(&c.column) {
            None => {
                let names: Vec<&str> = schema.columns.iter().map(|c| c.name.as_str()).take(12).collect();
                let hint = if names.is_empty() {
                    "this file has no columns".to_string()
                } else {
                    format!("available: {}", names.join(", "))
                };
                out.push(QueryError {
                    position: c.pos,
                    message: format!("unknown column '{}' ({hint})", c.column),
                });
            }
            Some(i) => {
                if schema.columns[i].kind == ColumnKind::Timestamp
                    && c.base.is_none()
                    && c.values.len() == 1
                    && c.values[0].relative_sign() == Some('+')
                    && matches!(c.op, Op::Lt | Op::Le | Op::Gt | Op::Ge)
                {
                    out.push(QueryError {
                        position: c.values[0].pos,
                        message: format!(
                            "relative time '{}' needs a lower bound in the same query, e.g. {}>2026-09-29T10:00",
                            c.values[0].text, c.column
                        ),
                    });
                }
            }
        },
        Node::Not(n) => check_node(n, schema, out),
        Node::And(v) | Node::Or(v) => v.iter().for_each(|n| check_node(n, schema, out)),
        Node::All | Node::Text(_) => {}
    }
}

fn eval(node: &Node, ctx: &Ctx<'_>) -> bool {
    match node {
        Node::All => true,
        Node::Text(needle) => contains_ci(ctx.line, needle),
        Node::Not(n) => !eval(n, ctx),
        Node::And(v) => v.iter().all(|n| eval(n, ctx)),
        Node::Or(v) => v.iter().any(|n| eval(n, ctx)),
        Node::Cmp(c) => eval_cmp(c, ctx),
    }
}

fn eval_cmp(c: &Cmp, ctx: &Ctx<'_>) -> bool {
    let Some(rec) = ctx.record else {
        return false;
    };
    let (cell, kind) = match ctx.schema.and_then(|s| s.find(&c.column).map(|i| (s, i))) {
        Some((s, i)) => match rec.get(i) {
            Some(v) => (v, s.columns[i].kind),
            None => return false,
        },
        None => match rec.extra_value(&c.column) {
            Some(v) => (v, ColumnKind::guess(&c.column)),
            None => return false,
        },
    };
    match c.op {
        Op::Regex => c.regex.as_ref().is_some_and(|r| r.is_match(cell)),
        Op::Eq => c.values.iter().any(|v| eq_value(v, cell, kind, ctx)),
        Op::Lt | Op::Le | Op::Gt | Op::Ge => {
            let Some(v) = c.values.first() else {
                return false;
            };
            match ord_value(c, v, cell, kind, ctx) {
                Some(o) => match c.op {
                    Op::Lt => o == Ordering::Less,
                    Op::Le => o != Ordering::Greater,
                    Op::Gt => o == Ordering::Greater,
                    _ => o != Ordering::Less,
                },
                None => false,
            }
        }
    }
}

fn eq_value(v: &Value, cell: &str, kind: ColumnKind, ctx: &Ctx<'_>) -> bool {
    if kind == ColumnKind::Level {
        if let (Some(a), Some(b)) = (Level::parse(cell), v.level) {
            return a == b;
        }
    }
    if v.glob {
        return glob_ci(&v.lower, cell);
    }
    match kind {
        ColumnKind::Number | ColumnKind::Duration | ColumnKind::Bytes => {
            if let (Some(cq), Some(rq)) = (parse_quantity(cell), v.quantity) {
                if let Some((a, b)) = canonical_pair(&cq, &rq) {
                    return approx_eq(a, b);
                }
            }
        }
        ColumnKind::Timestamp => {
            if cell.len() >= v.text.len()
                && cell.is_char_boundary(v.text.len())
                && cell[..v.text.len()].eq_ignore_ascii_case(&v.text)
            {
                return true;
            }
            if let (Some(a), Some(b)) = (ctx.ts.parse_ns(cell), ctx.ts.parse_ns(&v.text)) {
                return a == b;
            }
        }
        _ => {}
    }
    eq_ci(cell, v)
}

fn base_time(base: &Value, ctx: &Ctx<'_>) -> Option<i64> {
    if let (Some(q), Some('-')) = (base.quantity, base.relative_sign()) {
        return ctx.now.checked_add(rel_ns(&q));
    }
    *base.time.get_or_init(|| ctx.ts.parse_ns(&base.text))
}

fn rel_ns(q: &Quantity) -> i64 {
    // f64 -> i64 `as` casts saturate, which is what we want for huge offsets.
    q.canonical() as i64
}

fn resolve_time(c: &Cmp, v: &Value, ctx: &Ctx<'_>) -> Option<i64> {
    match (v.quantity, v.relative_sign()) {
        (Some(q), Some('-')) => ctx.now.checked_add(rel_ns(&q)),
        (Some(q), Some('+')) => {
            let base = base_time(c.base.as_deref()?, ctx)?;
            base.checked_add(rel_ns(&q))
        }
        _ => *v.time.get_or_init(|| ctx.ts.parse_ns(&v.text)),
    }
}

fn ord_value(c: &Cmp, v: &Value, cell: &str, kind: ColumnKind, ctx: &Ctx<'_>) -> Option<Ordering> {
    match kind {
        ColumnKind::Timestamp => {
            let rhs = resolve_time(c, v, ctx)?;
            let lhs = ctx.ts.parse_ns(cell)?;
            return Some(lhs.cmp(&rhs));
        }
        ColumnKind::Level => {
            if let (Some(a), Some(b)) = (Level::parse(cell), v.level) {
                return Some(a.cmp(&b));
            }
        }
        _ => {}
    }
    if let (Some(cq), Some(rq)) = (parse_quantity(cell), v.quantity) {
        let (a, b) = canonical_pair(&cq, &rq)?;
        return a.partial_cmp(&b);
    }
    if matches!(kind, ColumnKind::Number | ColumnKind::Duration | ColumnKind::Bytes) {
        return None;
    }
    Some(cmp_ci(cell, v))
}

#[cfg(test)]
mod tests;
