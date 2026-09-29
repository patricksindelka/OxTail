//! `gen-docs`: the sample profile gallery of the user guide.
//!
//! * Reads every built-in profile (`crates/oxtail-config/profiles/*.toml`) and
//!   every gallery-only example (`docs/profiles/*.toml`).
//! * **Validates** them with the real loader (`oxtail_config`) plus the typed
//!   checks the application applies later (parser names, column kinds, rule
//!   matchers, scopes, colours, timestamp formats, regexes). Any warning or
//!   error fails the command. Examples that ship sample lines
//!   (`docs/profiles/samples/<stem>.log`) are also run through their parser.
//! * Generates `docs/book/src/gallery/*.md` (one page per profile plus an
//!   index) and the gallery block of `docs/book/src/SUMMARY.md`.
//!
//! `--check` writes nothing and fails when a generated file is out of date;
//! CI runs it, and so does the unit test at the bottom of this file.
//!
//! The typed checks mirror `oxtail-gui` (`colspec.rs`, `rules.rs`), which is
//! where profiles are interpreted at run time. xtask cannot depend on the GUI
//! crate, so when those adapters learn a new parser or option, update the
//! mirror here; the tests fail loudly on names they do not know.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use oxtail_columns::{
    ColumnKind, FixedColumn, Parser, ParserSpec, Schema, discover_json_columns,
    discover_logfmt_columns,
};
use oxtail_config::{Glob, Profile, ProfileSet};
use oxtail_highlight::{ColorRef, ColumnOp};
use oxtail_time::jiff::tz::TimeZone;
use oxtail_time::{TimeContext, TimestampFormat};

pub const HELP: &str = "\
Generate the sample profile gallery of the user guide.

USAGE:
    cargo xtask gen-docs [--check] [--root <dir>]

Loads every built-in profile (crates/oxtail-config/profiles) and every gallery
example (docs/profiles) with the real profile loader, fails on any warning or
error, and writes docs/book/src/gallery/*.md plus the gallery entries of
docs/book/src/SUMMARY.md.

OPTIONS:
    --check         Write nothing; fail if a generated file is missing, stale
                    or unexpected (used in CI)
    --root <dir>    Repository root [default: the workspace root]
    -h, --help      Print this help
";

/// Markers around the generated block in `SUMMARY.md`.
const SUMMARY_START: &str = "<!-- gen-docs:gallery:start -->";
const SUMMARY_END: &str = "<!-- gen-docs:gallery:end -->";

const BUILTIN_DIR: &str = "crates/oxtail-config/profiles";
const EXAMPLE_DIR: &str = "docs/profiles";
const SAMPLE_DIR: &str = "docs/profiles/samples";
const GALLERY_DIR: &str = "docs/book/src/gallery";
const SUMMARY_FILE: &str = "docs/book/src/SUMMARY.md";

/// Timestamp format names the application accepts (`colspec::format_by_name`).
const FORMAT_NAMES: &[&str] = &[
    "auto",
    "rfc3339",
    "iso8601",
    "iso",
    "iso8601_comma",
    "iso8601comma",
    "log4j",
    "slashed",
    "go",
    "compact",
    "syslog",
    "rfc3164",
    "apache",
    "clf",
    "nginx",
    "rfc2822",
    "us",
    "us_datetime",
    "time_only",
    "time",
    "epoch",
    "epoch_s",
    "epoch_seconds",
    "unix",
    "epoch_ms",
    "epoch_millis",
    "epoch_us",
    "epoch_micros",
    "epoch_ns",
    "epoch_nanos",
];

// ---------------------------------------------------------------- CLI

pub fn run(mut parser: lexopt::Parser) -> Result<()> {
    use lexopt::prelude::*;
    let mut check = false;
    let mut root: Option<PathBuf> = None;
    while let Some(arg) = parser.next()? {
        match arg {
            Long("check") => check = true,
            Long("root") => root = Some(PathBuf::from(parser.value()?)),
            Short('h') | Long("help") => {
                print!("{HELP}");
                return Ok(());
            }
            other => return Err(other.unexpected().into()),
        }
    }
    let root = root.unwrap_or_else(workspace_root);
    if check {
        check_docs(&root)?;
        println!("docs are up to date");
    } else {
        let n = write_docs(&root)?;
        println!("wrote {n} files under {GALLERY_DIR} and updated {SUMMARY_FILE}");
    }
    Ok(())
}

fn workspace_root() -> PathBuf {
    // xtask/ sits directly below the workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

// ------------------------------------------------------------ loading

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    Builtin,
    Example,
}

/// One profile file with everything the pages need.
struct Entry {
    origin: Origin,
    /// File stem, also the page name.
    stem: String,
    /// Repository-relative path of the profile file (with `/`).
    path: String,
    /// The file text, `\n` line endings.
    text: String,
    profile: Profile,
    /// Sample lines (examples only).
    samples: Option<String>,
}

fn read_text(path: &Path) -> Result<String> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    // A Windows checkout may have CRLF; generated pages always use LF.
    Ok(text.replace("\r\n", "\n"))
}

fn toml_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("listing {}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    Ok(files)
}

/// Reads all profile files. Parse errors are reported through `problems`.
fn load_entries(root: &Path, problems: &mut Vec<String>) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    for (origin, dir) in [
        (Origin::Builtin, BUILTIN_DIR),
        (Origin::Example, EXAMPLE_DIR),
    ] {
        for path in toml_files(&root.join(dir))? {
            let stem = path
                .file_stem()
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
            let text = read_text(&path)?;
            let rel = format!("{dir}/{stem}.toml");
            let profile = match Profile::from_toml_str(&text) {
                Ok(p) => p,
                Err(e) => {
                    problems.push(format!("{rel}: {e}"));
                    continue;
                }
            };
            let samples = if origin == Origin::Example {
                let sp = root.join(SAMPLE_DIR).join(format!("{stem}.log"));
                sp.is_file().then(|| read_text(&sp)).transpose()?
            } else {
                None
            };
            out.push(Entry {
                origin,
                stem,
                path: rel,
                text,
                profile,
                samples,
            });
        }
    }
    Ok(out)
}

// --------------------------------------------------------- validation

/// Loads and checks everything. Returns the entries or the list of problems.
fn validate(root: &Path) -> Result<std::result::Result<Vec<Entry>, Vec<String>>> {
    let mut problems = Vec::new();
    let entries = load_entries(root, &mut problems)?;

    // The real loader, on the built-ins and on the examples as a user folder.
    let builtin_files = entries
        .iter()
        .filter(|e| e.origin == Origin::Builtin)
        .count();
    let builtin = ProfileSet::from_user_profiles(Vec::new());
    for w in &builtin.warnings {
        problems.push(format!("built-in profiles: {w}"));
    }
    let n = builtin.value.profiles().count();
    if n != builtin_files {
        problems.push(format!(
            "{BUILTIN_DIR} has {builtin_files} files but oxtail-config embeds {n} built-in \
             profiles: register the new file in oxtail-config/src/profile.rs"
        ));
    }
    let examples = entries
        .iter()
        .filter(|e| e.origin == Origin::Example)
        .count();
    let loaded = ProfileSet::load_dir(&root.join(EXAMPLE_DIR));
    for w in &loaded.warnings {
        problems.push(format!("{EXAMPLE_DIR}: {w}"));
    }
    let total = loaded.value.profiles().count();
    if total != builtin_files + examples {
        problems.push(format!(
            "{EXAMPLE_DIR}: expected {examples} examples next to {builtin_files} built-ins but \
             the loader produced {total} profiles: names must be unique and must not reuse the \
             name of a built-in profile"
        ));
    }

    let mut stems: BTreeMap<&str, &str> = BTreeMap::new();
    for e in &entries {
        if let Some(prev) = stems.insert(&e.stem, &e.path) {
            problems.push(format!(
                "{}: page name `{}` is already used by {prev}",
                e.path, e.stem
            ));
        }
        if e.profile.name.trim().is_empty() {
            problems.push(format!("{}: the profile has no name", e.path));
        }
        if description(&e.text).is_empty() {
            problems.push(format!(
                "{}: start the file with a `#` comment that describes the profile",
                e.path
            ));
        }
        let samples: Option<Vec<&str>> = e
            .samples
            .as_deref()
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).collect());
        if e.origin == Origin::Example && samples.is_none() {
            problems.push(format!(
                "{}: add sample lines in {SAMPLE_DIR}/{}.log",
                e.path, e.stem
            ));
        }
        for p in check_profile(&e.profile, samples.as_deref()) {
            problems.push(format!("{}: {p}", e.path));
        }
    }
    if problems.is_empty() {
        Ok(Ok(entries))
    } else {
        Ok(Err(problems))
    }
}

/// The typed checks of one profile. With `samples`, the parser and the
/// timestamp settings are also exercised on real lines.
fn check_profile(profile: &Profile, samples: Option<&[&str]>) -> Vec<String> {
    let mut out = Vec::new();
    for g in &profile.match_files {
        if g.trim().is_empty() {
            out.push("match_files contains an empty pattern".into());
        }
        let _ = Glob::new(g, false); // never fails, kept to fail loudly if that changes
    }
    let mut plan = None;
    if let Some(t) = &profile.columns {
        match parse_columns(t) {
            Ok(p) => plan = Some(p),
            Err(e) => out.push(format!("[columns]: {e}")),
        }
    }
    let mut format = None;
    if let Some(t) = &profile.timestamp {
        match parse_timestamp(t) {
            Ok(tc) => format = Some(tc),
            Err(e) => out.push(format!("[timestamp]: {e}")),
        }
    }
    let mut column_names: Vec<String> = Vec::new();
    for (i, r) in profile.rules.iter().enumerate() {
        match check_rule(r) {
            Ok(c) => column_names.extend(c),
            Err(e) => out.push(format!("rule {}: {e}", i + 1)),
        }
    }
    if let (Some(plan), None) = (&plan, samples) {
        check_static_columns(profile, plan, format.as_ref(), &column_names, &mut out);
    }
    if let (Some(plan), Some(samples)) = (&plan, samples) {
        check_samples(
            profile,
            plan,
            format.as_ref(),
            &column_names,
            samples,
            &mut out,
        );
    }
    out
}

/// Without sample lines: when every candidate parser has a fixed set of
/// columns, `order`, `timestamp.column` and rule columns must name a column of
/// at least one candidate (aliases such as `ts`/`time` count, as in the app).
fn check_static_columns(
    profile: &Profile,
    plan: &ColumnsPlan,
    ts: Option<&TimestampCfg>,
    rule_columns: &[String],
    out: &mut Vec<String>,
) {
    if plan.schema_is_dynamic() {
        return;
    }
    let mut names: Vec<String> = Vec::new();
    for spec in &plan.candidates {
        if let Ok(p) = spec.compile() {
            for c in &p.schema().columns {
                if !names.contains(&c.name) {
                    names.push(c.name.clone());
                }
            }
        }
    }
    let schema = Schema::from_names(&names, &BTreeMap::new());
    let list = names.join(", ");
    let _ = profile;
    for name in plan.order.iter().filter(|n| *n != "*") {
        if schema.find(name).is_none() {
            out.push(format!(
                "`order` names the column '{name}' which the parser does not produce \
                 (columns: {list})"
            ));
        }
    }
    for name in rule_columns {
        if schema.find(name).is_none() {
            out.push(format!(
                "a rule refers to the column '{name}' which the parser does not produce \
                 (columns: {list})"
            ));
        }
    }
    if let Some(col) = ts.and_then(|t| t.column.as_ref())
        && schema.find(col).is_none()
    {
        out.push(format!(
            "timestamp.column '{col}' is not a column of the parser (columns: {list})"
        ));
    }
}

// ---- [columns]

/// The columns table as the application reads it (mirror of
/// `oxtail_gui::colspec::columns_from_table`).
struct ColumnsPlan {
    parser: String,
    candidates: Vec<ParserSpec>,
    order: Vec<String>,
    needs_fields_line: bool,
    needs_header_line: Option<char>,
    discover: bool,
}

const COLUMNS_KEYS: &[&str] = &[
    "parser",
    "order",
    "pattern",
    "delimiter",
    "has_header",
    "columns",
    "kinds",
    "fields",
];

fn string_list(v: Option<&toml::Value>) -> Vec<String> {
    v.and_then(toml::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .filter(|s| s != "*")
                .collect()
        })
        .unwrap_or_default()
}

fn parse_kinds(t: &toml::Table) -> std::result::Result<BTreeMap<String, ColumnKind>, String> {
    let mut out = BTreeMap::new();
    if let Some(k) = t.get("kinds") {
        let kt = k.as_table().ok_or("`kinds` must be a table")?;
        for (name, v) in kt {
            let s = v.as_str().ok_or("a kind must be a string")?;
            let kind: ColumnKind = toml::Value::String(s.to_string())
                .try_into()
                .map_err(|_| format!("unknown column kind '{s}'"))?;
            out.insert(name.clone(), kind);
        }
    }
    Ok(out)
}

fn parse_columns(t: &toml::Table) -> std::result::Result<ColumnsPlan, String> {
    for k in t.keys() {
        if !COLUMNS_KEYS.contains(&k.as_str()) {
            return Err(format!("unknown key `{k}`"));
        }
    }
    let parser = t
        .get("parser")
        .and_then(toml::Value::as_str)
        .ok_or("the columns table needs a `parser` name")?
        .trim()
        .to_ascii_lowercase();
    let order: Vec<String> = match t.get("order") {
        Some(v) => v
            .as_array()
            .ok_or("`order` must be a list of column names")?
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
        None => Vec::new(),
    };
    let kinds = parse_kinds(t)?;
    let names = string_list(t.get("columns"));
    let known: Vec<String> = if names.is_empty() {
        order.iter().filter(|n| *n != "*").cloned().collect()
    } else {
        names.clone()
    };
    let mut plan = ColumnsPlan {
        parser: parser.clone(),
        candidates: Vec::new(),
        order,
        needs_fields_line: false,
        needs_header_line: None,
        discover: false,
    };
    match parser.as_str() {
        "logfmt" => {
            plan.discover = names.is_empty() && plan.order.iter().any(|o| o == "*");
            plan.candidates.push(ParserSpec::Logfmt {
                columns: known,
                kinds,
            });
        }
        "jsonl" | "json" | "jsonlines" | "ndjson" => {
            plan.discover = names.is_empty() && plan.order.iter().any(|o| o == "*");
            plan.candidates.push(ParserSpec::JsonLines {
                columns: known,
                kinds,
            });
        }
        "nginx_combined" | "apache_combined" | "combined" | "access_combined" => {
            plan.candidates.push(ParserSpec::AccessCombined);
        }
        "nginx_common" | "apache_common" | "common" | "access_common" | "clf" => {
            plan.candidates.push(ParserSpec::AccessCommon);
        }
        "syslog" | "syslog3164" | "syslog_3164" | "syslog_bsd" => {
            plan.candidates.push(ParserSpec::Syslog3164);
            if parser == "syslog" {
                plan.candidates.push(ParserSpec::Syslog5424);
            }
        }
        "syslog5424" | "syslog_5424" => plan.candidates.push(ParserSpec::Syslog5424),
        "w3c" | "iis" | "w3c_extended" => {
            let fields = string_list(t.get("fields"));
            if fields.is_empty() {
                plan.needs_fields_line = true;
                plan.candidates.push(ParserSpec::W3c {
                    fields: vec!["line".into()],
                    kinds,
                });
            } else {
                plan.candidates.push(ParserSpec::W3c { fields, kinds });
            }
        }
        "regex" => {
            let pattern = t
                .get("pattern")
                .and_then(toml::Value::as_str)
                .ok_or("the regex parser needs a `pattern`")?;
            plan.candidates.push(ParserSpec::Regex {
                pattern: pattern.to_string(),
                kinds,
            });
        }
        "log4j" | "logback" => {
            let pattern = t
                .get("pattern")
                .and_then(toml::Value::as_str)
                .ok_or("the log4j parser needs a `pattern`")?;
            plan.candidates.push(ParserSpec::Log4j {
                pattern: pattern.to_string(),
                kinds,
            });
        }
        "csv" | "tsv" | "delimited" | "psv" => {
            let delimiter = match t.get("delimiter").and_then(toml::Value::as_str) {
                Some(d) if d == "\\t" || d.eq_ignore_ascii_case("tab") => '\t',
                Some(d) => d.chars().next().unwrap_or(','),
                None => match parser.as_str() {
                    "tsv" => '\t',
                    "psv" => '|',
                    _ => ',',
                },
            };
            let has_header = t
                .get("has_header")
                .and_then(toml::Value::as_bool)
                .unwrap_or(false);
            if names.is_empty() {
                plan.needs_header_line = Some(delimiter);
                plan.candidates
                    .push(ParserSpec::delimited_from_header_line("col1", delimiter));
            } else {
                plan.candidates.push(ParserSpec::Delimited {
                    delimiter,
                    quote: '"',
                    has_header,
                    columns: names,
                    kinds,
                });
            }
        }
        "fixed" | "fixed_width" | "fixedwidth" => {
            let arr = t
                .get("fields")
                .and_then(toml::Value::as_array)
                .ok_or("the fixed parser needs `fields = [{ name, start, end }]`")?;
            let mut columns = Vec::new();
            for f in arr {
                let ft = f.as_table().ok_or("each fixed field must be a table")?;
                let name = ft
                    .get("name")
                    .and_then(toml::Value::as_str)
                    .ok_or("a fixed field needs a `name`")?;
                let start = ft
                    .get("start")
                    .and_then(toml::Value::as_integer)
                    .and_then(|v| usize::try_from(v).ok())
                    .ok_or("a fixed field needs a non-negative `start`")?;
                let end = ft
                    .get("end")
                    .and_then(toml::Value::as_integer)
                    .and_then(|v| usize::try_from(v).ok());
                columns.push(FixedColumn {
                    name: name.to_string(),
                    start,
                    end,
                });
            }
            plan.candidates.push(ParserSpec::FixedWidth { columns });
        }
        other => return Err(format!("unknown parser '{other}'")),
    }
    for spec in &plan.candidates {
        spec.compile()
            .map_err(|e| format!("the parser does not compile: {e}"))?;
    }
    Ok(plan)
}

fn merge_discovered(named: &[String], found: Vec<String>) -> Vec<String> {
    if found.is_empty() {
        return named.to_vec();
    }
    let mut out: Vec<String> = named
        .iter()
        .filter(|n| found.contains(n))
        .cloned()
        .collect();
    for f in found {
        if !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

impl ColumnsPlan {
    /// Picks the parser for `sample` (mirror of `ColumnsConfig::resolve`).
    fn resolve(&self, sample: &[&str]) -> std::result::Result<(ParserSpec, Parser), String> {
        let mut specs = self.candidates.clone();
        if self.discover {
            for spec in &mut specs {
                match spec {
                    ParserSpec::Logfmt { columns, .. } => {
                        *columns = merge_discovered(columns, discover_logfmt_columns(sample));
                    }
                    ParserSpec::JsonLines { columns, .. } => {
                        *columns = merge_discovered(columns, discover_json_columns(sample));
                    }
                    _ => {}
                }
            }
        }
        if self.needs_fields_line {
            let found = sample
                .iter()
                .find_map(|l| ParserSpec::w3c_from_fields_line(l))
                .ok_or("no `#Fields:` line among the sample lines")?;
            specs = vec![found];
        }
        if let Some(d) = self.needs_header_line {
            let first = sample
                .iter()
                .find(|l| !l.trim().is_empty())
                .ok_or("the sample has no lines")?;
            specs = vec![ParserSpec::delimited_from_header_line(first, d)];
        }
        let mut best: Option<(usize, ParserSpec, Parser)> = None;
        let mut last_err = String::from("no parser");
        for spec in specs {
            match spec.compile() {
                Ok(p) => {
                    let hits = sample.iter().filter(|l| p.is_match(l)).count();
                    if best.as_ref().is_none_or(|(h, _, _)| hits > *h) {
                        best = Some((hits, spec, p));
                    }
                }
                Err(e) => last_err = e.to_string(),
            }
        }
        best.map(|(_, s, p)| (s, p)).ok_or(last_err)
    }

    /// True when the columns are only known once real lines are seen.
    fn schema_is_dynamic(&self) -> bool {
        self.discover
            || self.needs_fields_line
            || self.needs_header_line.is_some()
            || self
                .candidates
                .iter()
                .any(|c| matches!(c, ParserSpec::JsonLines { .. } | ParserSpec::Logfmt { .. }))
    }
}

// ---- [timestamp]

struct TimestampCfg {
    column: Option<String>,
    format: Option<TimestampFormat>,
}

fn parse_timestamp(t: &toml::Table) -> std::result::Result<TimestampCfg, String> {
    for k in t.keys() {
        if !["column", "format", "zone", "timezone"].contains(&k.as_str()) {
            return Err(format!("unknown key `{k}`"));
        }
    }
    let column = t
        .get("column")
        .and_then(toml::Value::as_str)
        .map(str::to_string);
    let format = match t.get("format").and_then(toml::Value::as_str) {
        None => None,
        Some(f) => format_by_name(f).ok_or_else(|| {
            format!(
                "unknown timestamp format '{f}' (known: {})",
                FORMAT_NAMES.join(", ")
            )
        })?,
    };
    if let Some(z) = t
        .get("zone")
        .or_else(|| t.get("timezone"))
        .and_then(toml::Value::as_str)
        && !z.eq_ignore_ascii_case("utc")
        && !z.eq_ignore_ascii_case("local")
        && TimeZone::get(z).is_err()
    {
        return Err(format!("unknown time zone '{z}'"));
    }
    Ok(TimestampCfg { column, format })
}

/// `Some(None)` is `auto`; `None` is an unknown name.
fn format_by_name(name: &str) -> Option<Option<TimestampFormat>> {
    use TimestampFormat as F;
    Some(Some(match name.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => return Some(None),
        "rfc3339" | "iso8601" | "iso" => F::Iso8601,
        "iso8601_comma" | "iso8601comma" | "log4j" => F::Iso8601Comma,
        "slashed" | "go" => F::Slashed,
        "compact" => F::Compact,
        "syslog" | "rfc3164" => F::Syslog,
        "apache" | "clf" | "nginx" => F::Apache,
        "rfc2822" => F::Rfc2822,
        "us" | "us_datetime" => F::UsDateTime,
        "time_only" | "time" => F::TimeOnly,
        "epoch" | "epoch_s" | "epoch_seconds" | "unix" => F::EpochSeconds,
        "epoch_ms" | "epoch_millis" => F::EpochMillis,
        "epoch_us" | "epoch_micros" => F::EpochMicros,
        "epoch_ns" | "epoch_nanos" => F::EpochNanos,
        _ => return None,
    }))
}

// ---- [[rules]]

const RULE_KEYS: &[&str] = &[
    "name",
    "enabled",
    "match",
    "scope",
    "style",
    "priority",
    "alert",
    "hide",
    "bookmark",
    "actions",
    "minimap",
    "gutter",
    // Alias of `gutter`, read by the application.
    "gutter_marker",
];
const MATCH_KEYS: &[&str] = &[
    "regex",
    "literal",
    "column",
    "op",
    "value",
    "case_sensitive",
    "ignore_case",
];
const STYLE_KEYS: &[&str] = &["fg", "bg", "bold", "italic", "underline", "dim"];

fn check_color(v: &toml::Value) -> std::result::Result<(), String> {
    let s = v.as_str().ok_or("a colour must be a string")?;
    let t = s.trim();
    let aliased = match t.to_ascii_lowercase().as_str() {
        "fatal" => "error".to_string(),
        "fatal.subtle" => "error.subtle".to_string(),
        _ => t.to_string(),
    };
    ColorRef::from_str(&aliased)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Checks one rule table (mirror of `oxtail_gui::rules::rule_from_table`).
/// Returns the column names the rule refers to.
fn check_rule(t: &toml::Table) -> std::result::Result<Vec<String>, String> {
    if t.contains_key("matcher") {
        return Err("use the documented `match = { ... }` shape, not `matcher`".into());
    }
    for k in t.keys() {
        if !RULE_KEYS.contains(&k.as_str()) {
            return Err(format!("unknown key `{k}`"));
        }
    }
    let mut columns = Vec::new();
    let m = t
        .get("match")
        .ok_or("missing `match`")?
        .as_table()
        .ok_or("`match` must be a table")?;
    for k in m.keys() {
        if !MATCH_KEYS.contains(&k.as_str()) {
            return Err(format!("unknown key `{k}` in `match`"));
        }
    }
    if let Some(p) = m.get("regex") {
        let p = p.as_str().ok_or("regex must be a string")?;
        regex::Regex::new(p).map_err(|e| format!("invalid regex: {e}"))?;
    } else if let Some(p) = m.get("literal") {
        let p = p.as_str().ok_or("literal must be a string")?;
        if p.is_empty() {
            return Err("a literal must not be empty".into());
        }
    } else if let Some(c) = m.get("column") {
        let column = c.as_str().ok_or("column must be a string")?;
        columns.push(column.to_string());
        let op = match m.get("op").and_then(toml::Value::as_str) {
            Some(op) => toml::Value::String(op.to_string())
                .try_into::<ColumnOp>()
                .map_err(|_| format!("unknown operator '{op}'"))?,
            None => ColumnOp::Eq,
        };
        let value = match m.get("value") {
            Some(toml::Value::String(s)) => s.clone(),
            Some(toml::Value::Integer(i)) => i.to_string(),
            Some(toml::Value::Float(f)) => f.to_string(),
            Some(toml::Value::Boolean(b)) => b.to_string(),
            _ => return Err("a column condition needs a `value`".into()),
        };
        if matches!(op, ColumnOp::Regex) {
            regex::Regex::new(&value).map_err(|e| format!("invalid regex: {e}"))?;
        }
        if matches!(
            op,
            ColumnOp::Gt | ColumnOp::Ge | ColumnOp::Lt | ColumnOp::Le
        ) && value.trim().parse::<f64>().is_err()
        {
            return Err(format!(
                "operator `{}` compares numbers but the value '{value}' is not one",
                op_name(op)
            ));
        }
    } else {
        return Err("`match` needs `regex`, `literal` or `column`".into());
    }
    if let Some(s) = t.get("scope") {
        let s = s.as_str().ok_or("scope must be a string")?;
        let s = s.trim();
        let ok = s.eq_ignore_ascii_case("line")
            || s.eq_ignore_ascii_case("match")
            || s.strip_prefix("group:")
                .is_some_and(|n| n.trim().parse::<usize>().is_ok())
            || s.strip_prefix("column:").is_some();
        if !ok {
            return Err(format!(
                "unknown scope '{s}' (use line, match, group:N or column:NAME)"
            ));
        }
        if let Some(c) = s.strip_prefix("column:") {
            columns.push(c.trim().to_string());
        }
    }
    if let Some(v) = t.get("style") {
        let st = v.as_table().ok_or("style must be a table")?;
        for (k, val) in st {
            if !STYLE_KEYS.contains(&k.as_str()) {
                return Err(format!("unknown style key `{k}`"));
            }
            match k.as_str() {
                "fg" | "bg" => check_color(val)?,
                _ => {
                    val.as_bool()
                        .ok_or_else(|| format!("{k} must be true or false"))?;
                }
            }
        }
    }
    if let Some(v) = t.get("priority") {
        let p = v.as_integer().ok_or("priority must be an integer")?;
        i32::try_from(p).map_err(|_| "priority out of range".to_string())?;
    }
    if let Some(v) = t.get("minimap") {
        check_color(v)?;
    }
    for k in [
        "enabled",
        "alert",
        "hide",
        "bookmark",
        "gutter",
        "gutter_marker",
    ] {
        if let Some(v) = t.get(k) {
            v.as_bool()
                .ok_or_else(|| format!("{k} must be true or false"))?;
        }
    }
    if let Some(a) = t.get("actions") {
        let a = a.as_table().ok_or("actions must be a table")?;
        for (k, v) in a {
            if !["alert", "hide", "bookmark"].contains(&k.as_str()) {
                return Err(format!("unknown action `{k}`"));
            }
            v.as_bool()
                .ok_or_else(|| format!("{k} must be true or false"))?;
        }
    }
    Ok(columns)
}

fn op_name(op: ColumnOp) -> &'static str {
    match op {
        ColumnOp::Eq => "eq",
        ColumnOp::Ne => "ne",
        ColumnOp::Contains => "contains",
        ColumnOp::Regex => "regex",
        ColumnOp::Gt => "gt",
        ColumnOp::Ge => "ge",
        ColumnOp::Lt => "lt",
        ColumnOp::Le => "le",
    }
}

// ---- samples

fn check_samples(
    profile: &Profile,
    plan: &ColumnsPlan,
    ts: Option<&TimestampCfg>,
    rule_columns: &[String],
    samples: &[&str],
    out: &mut Vec<String>,
) {
    if samples.len() < 3 {
        out.push("the sample file needs at least 3 lines".into());
    }
    if let Some(p) = &profile.match_content {
        match regex::Regex::new(p) {
            Ok(re) if samples.iter().any(|l| re.is_match(l)) => {}
            Ok(_) => out.push("match_content matches none of the sample lines".into()),
            Err(e) => out.push(format!("match_content is not a valid regex: {e}")),
        }
    }
    let (spec, parser) = match plan.resolve(samples) {
        Ok(x) => x,
        Err(e) => {
            out.push(format!("the parser cannot be resolved on the samples: {e}"));
            return;
        }
    };
    let hits = samples.iter().filter(|l| parser.is_match(l)).count();
    // Continuation lines (stack traces) are allowed to not match.
    if hits * 2 < samples.len() {
        out.push(format!(
            "the {} parser accepts only {hits} of {} sample lines",
            spec.display_name(),
            samples.len()
        ));
    }
    let schema = parser.schema();
    if !plan.schema_is_dynamic() {
        for name in plan.order.iter().filter(|n| *n != "*") {
            if schema.find(name).is_none() {
                out.push(format!(
                    "`order` names the column '{name}' which the parser does not produce \
                     (columns: {})",
                    column_list(schema)
                ));
            }
        }
        for name in rule_columns {
            if schema.find(name).is_none() {
                out.push(format!(
                    "a rule refers to the column '{name}' which the parser does not produce \
                     (columns: {})",
                    column_list(schema)
                ));
            }
        }
    }
    if let Some(ts) = ts
        && let Some(col) = &ts.column
    {
        match schema.find(col) {
            None if !plan.schema_is_dynamic() => {
                out.push(format!("timestamp.column '{col}' is not a parsed column"));
            }
            idx => check_timestamp_values(ts, idx, &parser, samples, out),
        }
    }
}

fn check_timestamp_values(
    ts: &TimestampCfg,
    idx: Option<usize>,
    parser: &Parser,
    samples: &[&str],
    out: &mut Vec<String>,
) {
    let Some(format) = ts.format else { return };
    // Strict: the whole cell must be in this format (the application's own
    // parser would fall back to detection and hide a wrong `format`).
    let ctx = TimeContext::new(TimeZone::UTC);
    let (mut seen, mut parsed) = (0, 0);
    for l in samples {
        let Some(rec) = parser.parse(l) else { continue };
        let cell = match idx {
            Some(i) => rec.get(i).map(str::to_string),
            None => ts
                .column
                .as_deref()
                .and_then(|c| rec.extra_value(c).map(str::to_string)),
        };
        if let Some(cell) = cell {
            seen += 1;
            if format.parse(cell.trim(), &ctx).is_some() {
                parsed += 1;
            }
        }
    }
    if seen == 0 || parsed < seen {
        out.push(format!(
            "the timestamp format does not parse the timestamp column of the samples \
             ({parsed} of {seen} values parsed)"
        ));
    }
}

fn column_list(schema: &Schema) -> String {
    schema
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

// --------------------------------------------------------- rendering

/// The leading `#` comment block of a profile file, as markdown. Lines
/// indented by two or more spaces after the `#` become a code block.
fn description(text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for l in text.lines() {
        match l.strip_prefix('#') {
            Some(rest) => lines.push(rest.strip_prefix(' ').unwrap_or(rest)),
            None if l.trim().is_empty() && lines.is_empty() => {}
            None => break,
        }
    }
    let mut out = String::new();
    let mut para: Vec<&str> = Vec::new();
    let mut code: Vec<&str> = Vec::new();
    let flush_para = |out: &mut String, para: &mut Vec<&str>| {
        if !para.is_empty() {
            let text = escape_md(&para.join(" "));
            let _ = write!(out, "{text}\n\n");
            para.clear();
        }
    };
    let flush_code = |out: &mut String, code: &mut Vec<&str>| {
        if !code.is_empty() {
            out.push_str("```text\n");
            for c in code.iter() {
                out.push_str(c.trim_start());
                out.push('\n');
            }
            out.push_str("```\n\n");
            code.clear();
        }
    };
    for l in lines {
        if l.starts_with("  ") && !l.trim().is_empty() {
            flush_para(&mut out, &mut para);
            code.push(l);
        } else if l.trim().is_empty() {
            flush_para(&mut out, &mut para);
            flush_code(&mut out, &mut code);
        } else {
            flush_code(&mut out, &mut code);
            para.push(l.trim());
        }
    }
    flush_para(&mut out, &mut para);
    flush_code(&mut out, &mut code);
    out.trim_end().to_string()
}

/// Escapes markdown and HTML metacharacters of running text.
fn escape_md(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\\' | '*' | '_' | '[' | ']' | '`' | '|' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// An inline code span that survives backticks and table pipes.
fn code(s: &str) -> String {
    let longest = s
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let fence = "`".repeat(longest + 1);
    let pad = if s.starts_with('`') || s.ends_with('`') || s.starts_with(' ') || s.ends_with(' ') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{s}{pad}{fence}")
}

/// [`code`] for a table cell, where `|` must be escaped even inside a span.
fn code_cell(s: &str) -> String {
    code(s).replace('|', "\\|")
}

/// A fenced code block that survives backticks inside the text.
fn fenced(lang: &str, body: &str) -> String {
    let longest = body
        .lines()
        .map(|l| l.chars().take_while(|c| *c == '`').count())
        .max()
        .unwrap_or_default();
    let fence = "`".repeat(longest.max(2) + 1);
    let body = body.trim_end_matches('\n');
    format!("{fence}{lang}\n{body}\n{fence}\n")
}

fn kind_name(k: ColumnKind) -> &'static str {
    match k {
        ColumnKind::Text => "text",
        ColumnKind::Number => "number",
        ColumnKind::Timestamp => "timestamp",
        ColumnKind::Duration => "duration",
        ColumnKind::Bytes => "bytes",
        ColumnKind::Json => "json",
        ColumnKind::Level => "level",
    }
}

fn parser_label(plan: &ColumnsPlan) -> String {
    plan.candidates
        .first()
        .map_or_else(|| plan.parser.clone(), |s| s.display_name().to_string())
}

fn match_summary(t: &toml::Table) -> String {
    let Some(m) = t.get("match").and_then(toml::Value::as_table) else {
        return String::new();
    };
    if let Some(p) = m.get("regex").and_then(toml::Value::as_str) {
        let ci = m.get("case_sensitive").and_then(toml::Value::as_bool) == Some(false)
            || m.get("ignore_case").and_then(toml::Value::as_bool) == Some(true);
        format!(
            "regex {}{}",
            code_cell(p),
            if ci { " (ignore case)" } else { "" }
        )
    } else if let Some(p) = m.get("literal").and_then(toml::Value::as_str) {
        let cs = m.get("case_sensitive").and_then(toml::Value::as_bool) == Some(true);
        format!(
            "text {}{}",
            code_cell(p),
            if cs { " (case sensitive)" } else { "" }
        )
    } else if let Some(c) = m.get("column").and_then(toml::Value::as_str) {
        let op = m.get("op").and_then(toml::Value::as_str).unwrap_or("eq");
        let v = match m.get("value") {
            Some(toml::Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
            None => String::new(),
        };
        format!(
            "column {} {} {}",
            code_cell(c),
            code_cell(op),
            code_cell(&v)
        )
    } else {
        String::new()
    }
}

fn style_summary(t: &toml::Table) -> String {
    let Some(s) = t.get("style").and_then(toml::Value::as_table) else {
        return String::new();
    };
    let mut parts = Vec::new();
    for k in ["fg", "bg"] {
        if let Some(v) = s.get(k).and_then(toml::Value::as_str) {
            parts.push(format!("{k} {}", code_cell(v)));
        }
    }
    for k in ["bold", "italic", "underline", "dim"] {
        if s.get(k).and_then(toml::Value::as_bool) == Some(true) {
            parts.push(k.to_string());
        }
    }
    parts.join(", ")
}

fn actions_summary(t: &toml::Table) -> String {
    let flag = |k: &str| {
        t.get(k).and_then(toml::Value::as_bool) == Some(true)
            || t.get("actions")
                .and_then(toml::Value::as_table)
                .and_then(|a| a.get(k))
                .and_then(toml::Value::as_bool)
                == Some(true)
    };
    let mut parts = Vec::new();
    for k in ["alert", "hide", "bookmark"] {
        if flag(k) {
            parts.push(k.to_string());
        }
    }
    if let Some(c) = t.get("minimap").and_then(toml::Value::as_str) {
        parts.push(format!("minimap {}", code_cell(c)));
    }
    if flag("gutter") || flag("gutter_marker") {
        parts.push("gutter marker".into());
    }
    parts.join(", ")
}

fn rules_table(profile: &Profile) -> String {
    if profile.rules.is_empty() {
        return "This profile has no highlight rules.\n".into();
    }
    let mut rows: Vec<(i64, usize, &toml::Table)> = profile
        .rules
        .iter()
        .enumerate()
        .map(|(i, r)| {
            (
                r.get("priority")
                    .and_then(toml::Value::as_integer)
                    .unwrap_or(0),
                i,
                r,
            )
        })
        .collect();
    // Highest priority first (it wins conflicts); ties keep file order.
    rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut s = String::from(
        "| Priority | Rule | Matches | Scope | Style | Actions |\n|---:|---|---|---|---|---|\n",
    );
    for (prio, _, r) in rows {
        let name = r.get("name").and_then(toml::Value::as_str).unwrap_or("");
        let disabled = r.get("enabled").and_then(toml::Value::as_bool) == Some(false);
        let scope = r
            .get("scope")
            .and_then(toml::Value::as_str)
            .unwrap_or("match");
        let _ = writeln!(
            s,
            "| {prio} | {}{} | {} | {} | {} | {} |",
            escape_md(name),
            if disabled { " (disabled)" } else { "" },
            match_summary(r),
            code_cell(scope),
            style_summary(r),
            actions_summary(r),
        );
    }
    s
}

fn schema_of(plan: &ColumnsPlan) -> Vec<(String, Vec<(String, &'static str)>)> {
    plan.candidates
        .iter()
        .filter_map(|spec| {
            let p = spec.compile().ok()?;
            Some((
                spec.display_name().to_string(),
                p.schema()
                    .columns
                    .iter()
                    .map(|c| (c.name.clone(), kind_name(c.kind)))
                    .collect(),
            ))
        })
        .collect()
}

fn columns_section(profile: &Profile) -> String {
    let mut s = String::from("## Columns and timestamps\n\n");
    let plan = profile.columns.as_ref().and_then(|t| parse_columns(t).ok());
    match &plan {
        None => s.push_str(
            "This profile defines no `[columns]`. OxTail still offers to detect a parser \
             from the file contents.\n\n",
        ),
        Some(plan) => {
            let _ = writeln!(
                s,
                "Parser: **{}** (`parser = \"{}\"`).\n",
                escape_md(&parser_label(plan)),
                plan.parser
            );
            if plan.needs_fields_line {
                s.push_str(
                    "The column names come from the file's `#Fields:` directive, so they \
                     differ per file.\n\n",
                );
            } else if plan.needs_header_line.is_some() {
                s.push_str(
                    "The column names come from the first line of the file (the header row).\n\n",
                );
            } else if plan.schema_is_dynamic() {
                s.push_str(
                    "The columns are the keys found in the first lines of the file (nested \
                     JSON keys are flattened with dots); the keys named in `order` come first.\n\n",
                );
            } else {
                let schemas = schema_of(plan);
                for (name, cols) in &schemas {
                    if schemas.len() > 1 {
                        let _ = writeln!(s, "{}:\n", escape_md(name));
                    }
                    s.push_str("| Column | Kind |\n|---|---|\n");
                    for (c, k) in cols {
                        let _ = writeln!(s, "| {} | {k} |", code_cell(c));
                    }
                    s.push('\n');
                }
            }
            if !plan.order.is_empty() {
                let list = plan
                    .order
                    .iter()
                    .map(|c| code(c))
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = writeln!(
                    s,
                    "Display order (`order`): {list}. `*` stands for all other columns; names \
                     the parser does not produce are ignored.\n"
                );
            }
        }
    }
    match profile.timestamp.as_ref().and_then(|t| parse_timestamp(t).ok()) {
        Some(ts) => {
            let col = ts.column.as_deref().map_or_else(
                || "the first timestamp on each line".to_string(),
                |c| format!("column {}", code(c)),
            );
            let fmt = profile
                .timestamp
                .as_ref()
                .and_then(|t| t.get("format"))
                .and_then(toml::Value::as_str)
                .map_or_else(|| "auto".to_string(), str::to_string);
            let _ = writeln!(
                s,
                "Timestamps: {col}, format {}. They drive go to time, time gaps and the \
                 merged view.\n",
                code(&fmt)
            );
        }
        None => s.push_str(
            "Timestamps: no `[timestamp]` table, so the format is detected from the first lines.\n\n",
        ),
    }
    s
}

fn matching_section(profile: &Profile) -> String {
    let mut s = String::from("## Which files it is used for\n\n");
    if profile.match_files.is_empty() && profile.match_content.is_none() {
        s.push_str(
            "The profile has no `match_files` and no `match_content`, so OxTail never selects \
             it automatically. Pick it by name from the profile menu in the status bar or with \
             `--profile`.\n\n",
        );
    }
    if !profile.match_files.is_empty() {
        let list = profile
            .match_files
            .iter()
            .map(|g| code(g))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            s,
            "* File name patterns (`match_files`): {list}. A pattern without `/` is compared \
             with the file name, one with `/` with the whole path.",
        );
    }
    if let Some(c) = &profile.match_content {
        let _ = writeln!(
            s,
            "* Content (`match_content`): a regular expression tried on the first lines of the \
             file: {}.",
            code(c)
        );
    }
    if let Some(f) = &profile.default_filter {
        let _ = writeln!(
            s,
            "* Default filter (`default_filter`): {}. It is stored but not applied by this version.",
            code(f)
        );
    }
    s.push('\n');
    s
}

fn profile_page(e: &Entry) -> String {
    let p = &e.profile;
    let mut s = String::new();
    let _ = writeln!(s, "# {}\n", escape_md(&p.name));
    let desc = description(&e.text);
    if !desc.is_empty() {
        s.push_str(&desc);
        s.push_str("\n\n");
    }
    match e.origin {
        Origin::Builtin => {
            let _ = writeln!(
                s,
                "This profile is **built into OxTail**. Its source is `{}` in the repository.\n",
                e.path
            );
        }
        Origin::Example => {
            let _ = writeln!(
                s,
                "This is a **gallery example**: it is not built in. Its source is `{}` in the \
                 repository; the full text is at the bottom of this page.\n",
                e.path
            );
        }
    }
    s.push_str(&matching_section(p));
    s.push_str(&columns_section(p));
    s.push_str("## Highlight rules\n\n");
    if !p.rules.is_empty() {
        s.push_str(
            "Higher priority wins when rules overlap; on a tie the later rule wins. \
             Colours are semantic names resolved by the active theme.\n\n",
        );
    }
    s.push_str(&rules_table(p));
    s.push('\n');
    if let Some(samples) = &e.samples {
        s.push_str(
            "## Sample lines\n\nThe examples in this gallery are tested against these lines.\n\n",
        );
        s.push_str(&fenced("text", samples));
        s.push('\n');
    }
    s.push_str("## The full profile\n\n");
    s.push_str(&fenced("toml", &e.text));
    s.push_str("\n## Using it\n\n");
    let file = format!("{}.toml", e.stem);
    match e.origin {
        Origin::Builtin => {
            let _ = writeln!(
                s,
                "Nothing to do: it is active already. To customize it, copy the text above to \
                 `<data folder>/profiles/{file}` and edit it. A user profile with the same \
                 `name` replaces the built-in one; give it a different `name` to keep both. See \
                 [Installation and portable mode](../install.md) for where the data folder is."
            );
        }
        Origin::Example => {
            let _ = writeln!(
                s,
                "Copy the text above to `<data folder>/profiles/{file}`. OxTail reloads the \
                 profiles folder while it runs, so the profile is used for matching files right \
                 away, and it can be chosen by name from the status bar. See \
                 [Installation and portable mode](../install.md) for where the data folder is. \
                 Adjust `match_files` to your own file names."
            );
        }
    }
    s
}

fn index_row(e: &Entry) -> String {
    let plan = e
        .profile
        .columns
        .as_ref()
        .and_then(|t| parse_columns(t).ok());
    let parser = plan
        .as_ref()
        .map_or_else(|| "none".to_string(), parser_label);
    let files = if e.profile.match_files.is_empty() {
        "by name only".to_string()
    } else {
        e.profile
            .match_files
            .iter()
            .take(3)
            .map(|g| code(g))
            .collect::<Vec<_>>()
            .join(", ")
            + if e.profile.match_files.len() > 3 {
                ", ..."
            } else {
                ""
            }
    };
    format!(
        "| [{}]({}.md) | {} | {} |\n",
        escape_md(&e.profile.name),
        e.stem,
        files,
        escape_md(&parser)
    )
}

fn index_page(entries: &[Entry]) -> String {
    let mut s = String::from(
        "# Profile gallery\n\n\
         A profile bundles the file patterns it applies to, a column parser and highlight \
         rules. This gallery has one page per profile: what it matches, which columns it \
         produces, the rules as a table and the complete TOML file. The chapter [Highlighting \
         and profiles](../profiles.md) explains every field.\n\n\
         The pages are generated from the profile files themselves (`cargo run -p xtask -- \
         gen-docs`), and CI loads every profile with the real profile loader, so what you see \
         here is what OxTail reads.\n\n\
         To use a profile, copy its file to `<data folder>/profiles/`. See \
         [Installation and portable mode](../install.md) for the location of the data folder.\n\n",
    );
    for (origin, title, intro) in [
        (
            Origin::Builtin,
            "Built-in profiles",
            "These ship inside the OxTail executable and need no setup.",
        ),
        (
            Origin::Example,
            "More examples",
            "These are not built in. Copy the ones you need.",
        ),
    ] {
        let _ = writeln!(s, "## {title}\n\n{intro}\n");
        s.push_str("| Profile | Files | Parser |\n|---|---|---|\n");
        for e in entries.iter().filter(|e| e.origin == origin) {
            s.push_str(&index_row(e));
        }
        s.push('\n');
    }
    s.trim_end().to_string() + "\n"
}

fn summary_block(entries: &[Entry]) -> String {
    let mut s = String::from("- [Profile gallery](gallery/index.md)\n");
    for origin in [Origin::Builtin, Origin::Example] {
        for e in entries.iter().filter(|e| e.origin == origin) {
            let _ = writeln!(s, "  - [{}](gallery/{}.md)", e.profile.name, e.stem);
        }
    }
    s
}

/// Replaces the block between the markers of `existing`.
fn replace_summary_block(existing: &str, block: &str) -> Result<String> {
    let start = existing
        .find(SUMMARY_START)
        .with_context(|| format!("{SUMMARY_FILE} has no `{SUMMARY_START}` marker"))?;
    let end = existing
        .find(SUMMARY_END)
        .with_context(|| format!("{SUMMARY_FILE} has no `{SUMMARY_END}` marker"))?;
    if end < start {
        bail!("{SUMMARY_FILE}: the gallery markers are in the wrong order");
    }
    Ok(format!(
        "{}{SUMMARY_START}\n{block}{}",
        &existing[..start],
        &existing[end..]
    ))
}

// --------------------------------------------------------- generation

/// Every generated file, keyed by repository-relative path.
fn generate(root: &Path, entries: &[Entry]) -> Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    for e in entries {
        files.insert(format!("{GALLERY_DIR}/{}.md", e.stem), profile_page(e));
    }
    files.insert(format!("{GALLERY_DIR}/index.md"), index_page(entries));
    let summary = read_text(&root.join(SUMMARY_FILE))?;
    files.insert(
        SUMMARY_FILE.to_string(),
        replace_summary_block(&summary, &summary_block(entries))?,
    );
    Ok(files)
}

fn validated(root: &Path) -> Result<Vec<Entry>> {
    match validate(root)? {
        Ok(entries) => Ok(entries),
        Err(problems) => bail!(
            "{} problem(s) in the profiles:\n  - {}",
            problems.len(),
            problems.join("\n  - ")
        ),
    }
}

/// Stale generated pages: `.md` files in the gallery folder nobody generates.
fn stale_pages(root: &Path, files: &BTreeMap<String, String>) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(root.join(GALLERY_DIR)) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let rel = format!("{GALLERY_DIR}/{name}");
            if name.ends_with(".md") && !files.contains_key(&rel) {
                out.push(rel);
            }
        }
    }
    out.sort();
    out
}

/// Fails when any generated file differs from what is on disk.
fn check_docs(root: &Path) -> Result<()> {
    let entries = validated(root)?;
    let files = generate(root, &entries)?;
    let mut bad = Vec::new();
    for (rel, want) in &files {
        match read_text(&root.join(rel)) {
            Ok(have) if have == *want => {}
            Ok(_) => bad.push(format!("{rel} is out of date")),
            Err(_) => bad.push(format!("{rel} is missing")),
        }
    }
    for rel in stale_pages(root, &files) {
        bad.push(format!("{rel} is not generated any more; delete it"));
    }
    if !bad.is_empty() {
        bail!(
            "the generated docs are stale:\n  - {}\nrun `cargo run -p xtask -- gen-docs` and \
             commit the result",
            bad.join("\n  - ")
        );
    }
    Ok(())
}

fn write_docs(root: &Path) -> Result<usize> {
    let entries = validated(root)?;
    let files = generate(root, &entries)?;
    fs::create_dir_all(root.join(GALLERY_DIR))?;
    for rel in stale_pages(root, &files) {
        fs::remove_file(root.join(&rel)).with_context(|| format!("removing {rel}"))?;
    }
    let mut n = 0;
    for (rel, text) in &files {
        let path = root.join(rel);
        if fs::read_to_string(&path).ok().as_deref() != Some(text) {
            fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        }
        n += 1;
    }
    Ok(n)
}

// -------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(s: &str) -> Profile {
        Profile::from_toml_str(s).unwrap()
    }

    #[test]
    fn all_profiles_load_with_the_real_loader() {
        let root = workspace_root();
        match validate(&root).unwrap() {
            Ok(entries) => {
                let builtin = entries
                    .iter()
                    .filter(|e| e.origin == Origin::Builtin)
                    .count();
                let examples = entries
                    .iter()
                    .filter(|e| e.origin == Origin::Example)
                    .count();
                assert!(builtin >= 8, "built-in profiles: {builtin}");
                assert!(examples >= 4, "gallery examples: {examples}");
            }
            Err(problems) => panic!("profile problems:\n  - {}", problems.join("\n  - ")),
        }
    }

    #[test]
    fn the_generated_gallery_is_up_to_date() {
        if let Err(e) = check_docs(&workspace_root()) {
            panic!("{e:#}");
        }
    }

    #[test]
    fn broken_profiles_are_reported() {
        let bad_scope = profile(
            r#"
name = "x"
[[rules]]
match = { regex = "a" }
scope = "everything"
"#,
        );
        let p = check_profile(&bad_scope, None);
        assert!(p.iter().any(|m| m.contains("unknown scope")), "{p:?}");

        let bad_regex = profile(
            r#"
name = "x"
[[rules]]
match = { regex = "(" }
"#,
        );
        assert!(
            check_profile(&bad_regex, None)
                .iter()
                .any(|m| m.contains("invalid regex"))
        );

        let bad_parser = profile("name = \"x\"\n[columns]\nparser = \"nope\"\n");
        assert!(
            check_profile(&bad_parser, None)
                .iter()
                .any(|m| m.contains("unknown parser"))
        );

        let bad_color = profile(
            r#"
name = "x"
[[rules]]
match = { literal = "a" }
style = { fg = "chartreuse" }
"#,
        );
        assert!(
            check_profile(&bad_color, None)
                .iter()
                .any(|m| m.contains("colour"))
        );

        let bad_format = profile("name = \"x\"\n[timestamp]\nformat = \"julian\"\n");
        assert!(
            check_profile(&bad_format, None)
                .iter()
                .any(|m| m.contains("unknown timestamp format"))
        );

        let typo =
            profile("name = \"x\"\n[[rules]]\nmatch = { literal = \"a\" }\nscop = \"line\"\n");
        assert!(
            check_profile(&typo, None)
                .iter()
                .any(|m| m.contains("unknown key `scop`"))
        );
    }

    #[test]
    fn column_names_are_checked_without_samples() {
        let p = profile(
            r#"
name = "x"
[columns]
parser = "nginx_combined"
order = ["remote", "time_local"]
[timestamp]
column = "time_local"
[[rules]]
match = { column = "code", op = "ge", value = "500" }
"#,
        );
        let problems = check_profile(&p, None);
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems.iter().any(|m| m.contains("timestamp.column")));
        // Aliases and columns of any syslog flavour are fine.
        let ok = profile(
            r#"
name = "y"
[columns]
parser = "syslog"
order = ["timestamp", "app", "tag", "message"]
[timestamp]
column = "ts"
"#,
        );
        assert!(check_profile(&ok, None).is_empty());
    }

    #[test]
    fn samples_must_fit_the_parser() {
        let p = profile(
            r#"
name = "x"
match_content = '^\d+ '
[columns]
parser = "regex"
pattern = '^(?P<n>\d+) (?P<msg>.*)$'
order = ["n", "msg", "missing"]
"#,
        );
        let ok = ["1 a", "2 b", "3 c"];
        let problems = check_profile(&p, Some(&ok));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("'missing'"), "{problems:?}");

        let bad = ["a", "b", "c"];
        let problems = check_profile(&p, Some(&bad));
        assert!(
            problems.iter().any(|m| m.contains("match_content")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|m| m.contains("accepts only")),
            "{problems:?}"
        );
    }

    #[test]
    fn rule_columns_and_timestamps_are_checked_against_samples() {
        let p = profile(
            r#"
name = "x"
[columns]
parser = "regex"
pattern = '^(?P<t>\d{13}) (?P<msg>.*)$'
[timestamp]
column = "t"
format = "rfc3339"
[[rules]]
match = { column = "nope", op = "eq", value = "1" }
"#,
        );
        let lines = ["1790676062123 a", "1790676062124 b", "1790676062125 c"];
        let problems = check_profile(&p, Some(&lines));
        assert!(
            problems.iter().any(|m| m.contains("'nope'")),
            "{problems:?}"
        );
        assert!(
            problems
                .iter()
                .any(|m| m.contains("does not parse the timestamp")),
            "{problems:?}"
        );
    }

    #[test]
    fn code_spans_survive_backticks_and_pipes() {
        assert_eq!(code("a|b"), "`a|b`");
        assert_eq!(code_cell("a|b"), "`a\\|b`");
        assert_eq!(code("a`b"), "``a`b``");
        assert_eq!(code("`a"), "`` `a ``");
        assert_eq!(fenced("toml", "x\n"), "```toml\nx\n```\n");
        assert_eq!(fenced("toml", "```\nx\n"), "````toml\n```\nx\n````\n");
    }

    #[test]
    fn descriptions_become_paragraphs_and_code() {
        let text = "# First line\n# second <line>.\n#\n#   indented example\n# tail\nname = \"x\"\n# not part\n";
        let d = description(text);
        assert_eq!(
            d,
            "First line second &lt;line&gt;.\n\n```text\nindented example\n```\n\ntail"
        );
    }

    #[test]
    fn summary_block_is_replaced_between_markers() {
        let existing = format!("a\n{SUMMARY_START}\nold\n{SUMMARY_END}\nb\n");
        let out = replace_summary_block(&existing, "new\n").unwrap();
        assert_eq!(out, format!("a\n{SUMMARY_START}\nnew\n{SUMMARY_END}\nb\n"));
        assert!(replace_summary_block("no markers", "x").is_err());
    }

    #[test]
    fn rules_are_listed_by_priority() {
        let p = profile(
            r#"
name = "x"
[[rules]]
name = "low"
match = { literal = "a|b" }
priority = 1
[[rules]]
name = "high"
match = { column = "level", op = "ge", value = "50" }
scope = "line"
style = { fg = "error", bold = true }
alert = true
priority = 9
"#,
        );
        let t = rules_table(&p);
        let high = t.find("high").unwrap();
        let low = t.find("low").unwrap();
        assert!(high < low, "{t}");
        assert!(t.contains("a\\|b"), "{t}");
        assert!(t.contains("alert"), "{t}");
    }
}
