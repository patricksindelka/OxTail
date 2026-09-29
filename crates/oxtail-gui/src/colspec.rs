//! Adapters between the opaque `columns` and `timestamp` tables of a profile
//! (`oxtail-config` keeps them as raw TOML) and the typed forms of
//! `oxtail-columns` / `oxtail-time`.
//!
//! ```toml
//! [columns]
//! parser = "logfmt"        # logfmt | jsonl | nginx_combined | apache_common | syslog | w3c
//!                          # | regex | log4j | csv | tsv | delimited | fixed
//! order  = ["time", "level", "msg", "*"]      # display order; "*" = the rest
//! pattern = '...'          # regex / log4j
//! delimiter = ","          # csv / delimited
//! has_header = true        # csv / delimited (names come from the first line)
//! columns = ["a", "b"]     # csv / delimited / logfmt / jsonl (known keys)
//! kinds = { status = "number", took = "duration" }
//!
//! [timestamp]
//! column = "time"
//! format = "rfc3339"       # auto | iso8601 | apache | syslog | epoch_ms | ...
//! zone   = "UTC"           # optional, for zone-less timestamps
//! ```
//!
//! Everything here is pure: the sample lines a parser may need (a W3C
//! `#Fields:` directive, a CSV header) are passed in by the caller.

use std::collections::BTreeMap;

use oxtail_columns::{
    ColumnKind, FixedColumn, Parser, ParserSpec, Schema, discover_json_columns,
    discover_logfmt_columns,
};
use oxtail_config::TimezoneSetting;
use oxtail_time::jiff::tz::TimeZone;
use oxtail_time::{TimeContext, TimeParser, TimestampFormat};

/// What a profile's `columns` table asks for.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnsConfig {
    /// Parser definitions to try, in order of preference. Most parsers have
    /// exactly one; `syslog` has both RFC flavours.
    pub candidates: Vec<ParserSpec>,
    /// Display order; `"*"` stands for all columns not named.
    pub order: Vec<String>,
    /// W3C: the fields come from the sample's `#Fields:` directive.
    pub needs_fields_line: bool,
    /// Delimited with `has_header`: the names come from the first line.
    pub needs_header_line: Option<char>,
    /// logfmt / JSON Lines with `*` in `order`: the columns are the named
    /// ones that occur in the sample plus every other key found there.
    pub discover: bool,
}

/// Parses the `columns` table of a profile.
pub fn columns_from_table(t: &toml::Table) -> Result<ColumnsConfig, String> {
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
    let kinds = kinds_from(t)?;
    let names = string_list(t.get("columns"));
    let known: Vec<String> = if names.is_empty() {
        order.iter().filter(|n| *n != "*").cloned().collect()
    } else {
        names.clone()
    };
    let mut cfg = ColumnsConfig {
        candidates: Vec::new(),
        order,
        needs_fields_line: false,
        needs_header_line: None,
        discover: false,
    };
    match parser.as_str() {
        "logfmt" => {
            cfg.discover = names.is_empty() && cfg.order.iter().any(|o| o == "*");
            cfg.candidates.push(ParserSpec::Logfmt {
                columns: known,
                kinds,
            });
        }
        "jsonl" | "json" | "jsonlines" | "ndjson" => {
            cfg.discover = names.is_empty() && cfg.order.iter().any(|o| o == "*");
            cfg.candidates.push(ParserSpec::JsonLines {
                columns: known,
                kinds,
            });
        }
        "nginx_combined" | "apache_combined" | "combined" | "access_combined" => {
            cfg.candidates.push(ParserSpec::AccessCombined);
        }
        "nginx_common" | "apache_common" | "common" | "access_common" | "clf" => {
            cfg.candidates.push(ParserSpec::AccessCommon);
        }
        "syslog" | "syslog3164" | "syslog_3164" | "syslog_bsd" => {
            cfg.candidates.push(ParserSpec::Syslog3164);
            if parser == "syslog" {
                cfg.candidates.push(ParserSpec::Syslog5424);
            }
        }
        "syslog5424" | "syslog_5424" => cfg.candidates.push(ParserSpec::Syslog5424),
        "w3c" | "iis" | "w3c_extended" => {
            let fields = string_list(t.get("fields"));
            if fields.is_empty() {
                cfg.needs_fields_line = true;
                // Placeholder so `candidates` is never empty; replaced on resolve.
                cfg.candidates.push(ParserSpec::W3c {
                    fields: vec!["line".into()],
                    kinds,
                });
            } else {
                cfg.candidates.push(ParserSpec::W3c { fields, kinds });
            }
        }
        "regex" => {
            let pattern = t
                .get("pattern")
                .and_then(toml::Value::as_str)
                .ok_or("the regex parser needs a `pattern`")?;
            cfg.candidates.push(ParserSpec::Regex {
                pattern: pattern.to_string(),
                kinds,
            });
        }
        "log4j" | "logback" => {
            let pattern = t
                .get("pattern")
                .and_then(toml::Value::as_str)
                .ok_or("the log4j parser needs a `pattern`")?;
            cfg.candidates.push(ParserSpec::Log4j {
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
                cfg.needs_header_line = Some(delimiter);
                cfg.candidates
                    .push(ParserSpec::delimited_from_header_line("col1", delimiter));
            } else {
                cfg.candidates.push(ParserSpec::Delimited {
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
            cfg.candidates.push(ParserSpec::FixedWidth { columns });
        }
        other => return Err(format!("unknown parser '{other}'")),
    }
    Ok(cfg)
}

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

fn kinds_from(t: &toml::Table) -> Result<BTreeMap<String, ColumnKind>, String> {
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

impl ColumnsConfig {
    /// Picks and compiles the parser, using `sample` (the first lines of the
    /// file) where the definition needs it. Returns the spec and the compiled
    /// parser.
    pub fn resolve(&self, sample: &[&str]) -> Result<(ParserSpec, Parser), String> {
        let mut specs: Vec<ParserSpec> = self.candidates.clone();
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
                .ok_or("no `#Fields:` line among the first lines")?;
            specs = vec![found];
        }
        if let Some(d) = self.needs_header_line {
            let first = sample
                .iter()
                .find(|l| !l.trim().is_empty())
                .ok_or("the file has no lines yet")?;
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
}

/// The named columns that occur in `found`, then every other found key.
/// With nothing found the named columns are kept as they are.
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

/// Orders the columns of `schema` by `order` (names may use aliases; `*` marks
/// where unnamed columns go, default at the end). Returns schema indices, each
/// exactly once.
pub fn apply_order(schema: &Schema, order: &[String]) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::with_capacity(schema.len());
    let mut star_at: Option<usize> = None;
    for name in order {
        if name == "*" {
            star_at = Some(out.len());
        } else if let Some(i) = schema.find(name)
            && !out.contains(&i)
        {
            out.push(i);
        }
    }
    let rest: Vec<usize> = (0..schema.len()).filter(|i| !out.contains(i)).collect();
    let at = star_at.unwrap_or(out.len());
    let tail = out.split_off(at);
    out.extend(rest);
    out.extend(tail);
    out
}

/// What a profile's `timestamp` table asks for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TimestampConfig {
    /// The column holding the timestamp, if named.
    pub column: Option<String>,
    /// A fixed format; `None` means auto-detect (learn from the first lines).
    pub format: Option<TimestampFormat>,
    /// Zone for zone-less timestamps (overrides the setting).
    pub zone: Option<String>,
}

/// Parses the `timestamp` table of a profile. Unknown formats are an error.
pub fn timestamp_from_table(t: &toml::Table) -> Result<TimestampConfig, String> {
    let column = t
        .get("column")
        .and_then(toml::Value::as_str)
        .map(str::to_string);
    let zone = t
        .get("zone")
        .or_else(|| t.get("timezone"))
        .and_then(toml::Value::as_str)
        .map(str::to_string);
    let format = match t.get("format").and_then(toml::Value::as_str) {
        None => None,
        Some(f) => format_by_name(f).map_err(|()| format!("unknown timestamp format '{f}'"))?,
    };
    Ok(TimestampConfig {
        column,
        format,
        zone,
    })
}

/// Maps a format name to a format; `Ok(None)` is "auto".
#[allow(clippy::result_unit_err)] // the caller adds the name to the message
pub fn format_by_name(name: &str) -> Result<Option<TimestampFormat>, ()> {
    use TimestampFormat as F;
    Ok(Some(match name.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => return Ok(None),
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
        _ => return Err(()),
    }))
}

/// Resolves a zone setting to a time zone. Unknown names fall back to UTC.
pub fn zone_of(setting: &TimezoneSetting) -> TimeZone {
    match setting {
        TimezoneSetting::Local => TimeZone::system(),
        TimezoneSetting::Utc => TimeZone::UTC,
        TimezoneSetting::Named(n) => TimeZone::get(n).unwrap_or(TimeZone::UTC),
    }
}

/// Builds the time parser of a tab: the profile's format when given, else
/// learned from `sample`. The zone of the profile wins over the setting.
pub fn build_time_parser(
    cfg: Option<&TimestampConfig>,
    setting: &TimezoneSetting,
    sample: &[&str],
) -> TimeParser {
    let tz = match cfg.and_then(|c| c.zone.as_deref()) {
        Some(z) => zone_of(&TimezoneSetting::Named(z.to_string())),
        None => zone_of(setting),
    };
    let ctx = TimeContext::new(tz);
    match cfg.and_then(|c| c.format) {
        Some(f) => TimeParser::with_format(ctx, f),
        None => {
            let mut p = TimeParser::new(ctx);
            p.learn(sample.iter().copied());
            p
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(s: &str) -> toml::Table {
        s.parse().unwrap()
    }

    #[test]
    fn logfmt_profile_maps_order_to_known_columns() {
        let t = table(
            r#"parser = "logfmt"
order = ["time", "level", "msg", "*"]"#,
        );
        let cfg = columns_from_table(&t).unwrap();
        match &cfg.candidates[0] {
            ParserSpec::Logfmt { columns, .. } => {
                assert_eq!(columns, &["time", "level", "msg"]);
            }
            other => panic!("{other:?}"),
        }
        // `*` in the order: every other key of the sample becomes a column too.
        assert!(cfg.discover);
        let (_, parser) = cfg.resolve(&["time=1 level=info msg=hi extra=1"]).unwrap();
        assert_eq!(parser.schema().len(), 4);
        assert!(parser.schema().find("extra").is_some());
        // Named columns that the sample does not have are dropped (`time`, `msg`).
        let (_, parser) = cfg.resolve(&["ts=1 level=info took=5"]).unwrap();
        let names: Vec<&str> = parser
            .schema()
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, ["level", "ts", "took"]);
        // Nothing to discover from: the named columns stay.
        let (_, parser) = cfg.resolve(&[]).unwrap();
        assert_eq!(parser.schema().len(), 3);
    }

    #[test]
    fn builtin_profiles_all_convert() {
        let set = oxtail_config::ProfileSet::builtin();
        let mut seen = 0;
        for p in set.profiles() {
            if let Some(t) = &p.columns {
                columns_from_table(t).unwrap_or_else(|e| panic!("{}: {e}", p.name));
                seen += 1;
            }
            if let Some(t) = &p.timestamp {
                timestamp_from_table(t).unwrap_or_else(|e| panic!("{}: {e}", p.name));
            }
        }
        assert!(seen >= 5, "{seen}");
    }

    #[test]
    fn access_log_profile_resolves_and_orders_by_alias() {
        let t = table(
            r#"parser = "nginx_combined"
order = ["remote_addr", "time_local", "request", "status", "body_bytes_sent", "*"]"#,
        );
        let cfg = columns_from_table(&t).unwrap();
        let (_, p) = cfg.resolve(&[]).unwrap();
        let ord = apply_order(p.schema(), &cfg.order);
        // Every column exactly once, `status` first among the named ones.
        let mut sorted = ord.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..p.schema().len()).collect::<Vec<_>>());
        let status = p.schema().find("status").unwrap();
        assert!(ord.contains(&status));
    }

    #[test]
    fn w3c_needs_the_fields_line() {
        let t = table(
            r#"parser = "w3c"
order = ["date", "time", "*"]"#,
        );
        let cfg = columns_from_table(&t).unwrap();
        assert!(cfg.resolve(&["2026-01-01 00:00:00 1.2.3.4"]).is_err());
        let sample = ["#Fields: date time c-ip", "2026-01-01 00:00:00 1.2.3.4"];
        let (spec, p) = cfg.resolve(&sample).unwrap();
        assert!(matches!(spec, ParserSpec::W3c { .. }));
        assert_eq!(p.schema().len(), 3);
    }

    #[test]
    fn csv_takes_its_names_from_the_header() {
        let t = table(r#"parser = "csv""#);
        let cfg = columns_from_table(&t).unwrap();
        let (_, p) = cfg.resolve(&["id,name,city", "1,ann,Oslo"]).unwrap();
        assert_eq!(
            p.schema()
                .columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["id", "name", "city"]
        );
    }

    #[test]
    fn syslog_picks_the_flavour_that_parses() {
        let cfg = columns_from_table(&table(r#"parser = "syslog""#)).unwrap();
        let five = "<165>1 2026-09-29T10:00:00Z host app 1 ID47 - hello";
        let (spec, _) = cfg.resolve(&[five]).unwrap();
        assert_eq!(spec, ParserSpec::Syslog5424);
        let three = "<34>Oct 11 22:14:15 mymachine su[12]: 'su root' failed";
        let (spec, _) = cfg.resolve(&[three]).unwrap();
        assert_eq!(spec, ParserSpec::Syslog3164);
    }

    #[test]
    fn errors_are_reported_not_panicked() {
        assert!(columns_from_table(&table(r#"order = ["a"]"#)).is_err());
        assert!(columns_from_table(&table(r#"parser = "nope""#)).is_err());
        assert!(columns_from_table(&table(r#"parser = "regex""#)).is_err());
        let bad = table("parser = \"regex\"\npattern = \"(unclosed\"");
        let cfg = columns_from_table(&bad).unwrap();
        assert!(cfg.resolve(&["x"]).is_err());
        assert!(
            columns_from_table(&table("parser = \"logfmt\"\nkinds = { a = \"wat\" }")).is_err()
        );
    }

    #[test]
    fn kinds_and_fixed_fields_convert() {
        let t = table(
            r#"parser = "logfmt"
columns = ["took", "n"]
kinds = { took = "duration", n = "number" }"#,
        );
        let cfg = columns_from_table(&t).unwrap();
        let (_, p) = cfg.resolve(&["took=5 n=1"]).unwrap();
        assert_eq!(p.schema().columns[0].kind, ColumnKind::Duration);
        let t = table(
            r#"parser = "fixed"
fields = [{ name = "a", start = 0, end = 3 }, { name = "b", start = 3 }]"#,
        );
        let cfg = columns_from_table(&t).unwrap();
        let (_, p) = cfg.resolve(&["abcdef"]).unwrap();
        let r = p.parse("abcdef").unwrap();
        assert_eq!(r.get(0), Some("abc"));
        assert_eq!(r.get(1), Some("def"));
    }

    #[test]
    fn order_applies_star_and_ignores_unknown_names() {
        let schema = Schema::from_names(&["a", "b", "c", "d"], &BTreeMap::new());
        let o = |v: &[&str]| {
            apply_order(
                &schema,
                &v.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            )
        };
        assert_eq!(o(&["c", "*", "a"]), vec![2, 1, 3, 0]);
        assert_eq!(o(&["zzz"]), vec![0, 1, 2, 3]);
        assert_eq!(o(&["d", "d", "b"]), vec![3, 1, 0, 2]);
        assert_eq!(o(&[]), vec![0, 1, 2, 3]);
    }

    #[test]
    fn timestamp_tables_convert() {
        let t = table("column = \"time\"\nformat = \"rfc3339\"\nzone = \"UTC\"");
        let c = timestamp_from_table(&t).unwrap();
        assert_eq!(c.column.as_deref(), Some("time"));
        assert_eq!(c.format, Some(TimestampFormat::Iso8601));
        assert_eq!(c.zone.as_deref(), Some("UTC"));
        let auto = timestamp_from_table(&table("format = \"auto\"")).unwrap();
        assert_eq!(auto.format, None);
        assert!(timestamp_from_table(&table("format = \"wat\"")).is_err());
    }

    #[test]
    fn time_parser_uses_profile_format_or_learns() {
        let cfg = TimestampConfig {
            format: Some(TimestampFormat::Apache),
            ..TimestampConfig::default()
        };
        let p = build_time_parser(Some(&cfg), &TimezoneSetting::Utc, &[]);
        assert!(p.parse("[29/Sep/2026:10:00:00 +0000] x").is_some());
        let p = build_time_parser(
            None,
            &TimezoneSetting::Utc,
            &["2026-09-29 10:00:00,001 a", "2026-09-29 10:00:01,001 b"],
        );
        assert_eq!(p.format(), Some(TimestampFormat::Iso8601Comma));
        // A zone-less timestamp honours the configured zone.
        let cfg = TimestampConfig {
            zone: Some("Europe/Amsterdam".into()),
            ..TimestampConfig::default()
        };
        let ams = build_time_parser(Some(&cfg), &TimezoneSetting::Utc, &[]);
        let utc = build_time_parser(None, &TimezoneSetting::Utc, &[]);
        let l = "2026-09-29 10:00:00 x";
        let (a, b) = (ams.parse(l).unwrap(), utc.parse(l).unwrap());
        assert_eq!(b.as_second() - a.as_second(), 2 * 3600);
    }
}
