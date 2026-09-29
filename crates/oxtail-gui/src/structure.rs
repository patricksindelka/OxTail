//! The structure of a tab: which parser turns its lines into columns, whether
//! the table view is showing, the column layout and the time parser.
//!
//! Deciding takes a sample of the first lines, so it runs on a worker thread
//! ([`spawn_decide`]); the decision itself, [`decide`], is a pure function.
//! The order of precedence is: what the user chose for this file before
//! (`gui-state.json`), the profile's `columns` table, then auto-detection
//! (which only ever produces a *suggestion*: the parser is not applied until
//! the user accepts it).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use oxtail_columns::{ColumnKind, Detection, Parser, ParserSpec, Record, detect};
use oxtail_config::TimezoneSetting;
use oxtail_core::Document;
use oxtail_time::jiff::Timestamp;
use oxtail_time::jiff::tz::TimeZone;
use oxtail_time::{TimeParser, helpers};

use crate::collayout::ColumnLayout;
use crate::colspec::{ColumnsConfig, TimestampConfig, apply_order, build_time_parser, zone_of};
use crate::guistate::SavedStructure;

/// Lines sampled for detection and format learning.
pub const SAMPLE_LINES: usize = 1000;
/// How long the worker waits for an empty file to get its first lines.
const EMPTY_RETRIES: u32 = 240;

/// Where the parser came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// The columns table of this profile.
    Profile(String),
    /// Auto-detection, accepted by the user.
    Detected,
    /// Picked by the user in the chooser.
    Chosen,
    /// Restored from the previous session.
    Restored,
}

/// What the tab could show as columns, waiting for the user's yes.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// The format's name ("Nginx/Apache combined").
    pub name: String,
    /// The parser to apply.
    pub spec: ParserSpec,
    /// Preferred column order (profile suggestions).
    pub order: Vec<String>,
    /// A profile that defines the columns (otherwise detected).
    pub profile: Option<String>,
    /// Detection score, `0.0..=1.0` (1.0 for profiles).
    pub score: f32,
}

/// Input of [`decide`].
#[derive(Debug, Clone, Default)]
pub struct StructureInput {
    /// The tab's profile.
    pub profile_name: Option<String>,
    /// Its `columns` table, converted.
    pub columns: Option<ColumnsConfig>,
    /// Its `timestamp` table, converted.
    pub timestamp: Option<TimestampConfig>,
    /// What the user chose for this file before.
    pub saved: Option<SavedStructure>,
    /// The zone for zone-less timestamps.
    pub tz: Option<TimezoneSetting>,
}

/// A parser that is to be applied.
#[derive(Debug, Clone)]
pub struct ActiveParser {
    /// The definition (kept for saving).
    pub spec: ParserSpec,
    /// The compiled parser.
    pub parser: Parser,
    /// Display name.
    pub name: String,
    /// Where it came from.
    pub origin: Origin,
    /// Preferred column order.
    pub order: Vec<String>,
    /// Show the table view right away.
    pub table: bool,
    /// Offer the table view as a suggestion (profile parsers).
    pub offer_table: bool,
    /// Saved layout to apply.
    pub layout: Option<crate::collayout::LayoutSnapshot>,
}

/// The outcome of [`decide`].
#[derive(Debug, Clone)]
pub enum Outcome {
    /// Apply this parser.
    Active(Box<ActiveParser>),
    /// Ask the user about these (best first).
    Suggest(Vec<Detection>),
    /// Plain text.
    Nothing,
    /// The profile's parser does not work on this file.
    Failed(String),
}

/// What the worker reports.
#[derive(Debug, Clone)]
pub struct StructureResult {
    /// The decision.
    pub outcome: Outcome,
    /// The time parser for this file.
    pub time: TimeParser,
    /// The timestamp column named by the profile.
    pub ts_column: Option<String>,
    /// How many lines were sampled.
    pub sampled: usize,
}

/// Decides the structure from a sample of the first lines. Pure.
pub fn decide(input: &StructureInput, sample: &[String]) -> StructureResult {
    let refs: Vec<&str> = sample.iter().map(String::as_str).collect();
    let tz = input.tz.clone().unwrap_or_default();
    let time = build_time_parser(input.timestamp.as_ref(), &tz, &refs);
    let ts_column = input.timestamp.as_ref().and_then(|t| t.column.clone());
    let outcome = decide_outcome(input, &refs);
    StructureResult {
        outcome,
        time,
        ts_column,
        sampled: sample.len(),
    }
}

fn decide_outcome(input: &StructureInput, refs: &[&str]) -> Outcome {
    // 1. What the user chose before.
    if let Some(saved) = &input.saved {
        match &saved.spec {
            Some(spec) => match spec.compile() {
                Ok(parser) => {
                    return Outcome::Active(Box::new(ActiveParser {
                        name: spec.display_name().to_string(),
                        spec: spec.clone(),
                        parser,
                        origin: Origin::Restored,
                        order: Vec::new(),
                        table: saved.table,
                        offer_table: false,
                        layout: saved.layout.clone(),
                    }));
                }
                Err(e) => tracing::warn!("saved parser no longer compiles: {e}"),
            },
            None if saved.dismissed => return Outcome::Nothing,
            None => {}
        }
    }
    // 2. The profile's columns.
    if let (Some(cfg), Some(name)) = (&input.columns, &input.profile_name) {
        match cfg.resolve(refs) {
            Ok((spec, parser)) => {
                let dismissed = input.saved.as_ref().is_some_and(|s| s.dismissed);
                return Outcome::Active(Box::new(ActiveParser {
                    name: format!("{name} ({})", spec.display_name()),
                    spec,
                    parser,
                    origin: Origin::Profile(name.clone()),
                    order: cfg.order.clone(),
                    table: false,
                    offer_table: !dismissed,
                    layout: input.saved.as_ref().and_then(|s| s.layout.clone()),
                }));
            }
            Err(e) => {
                // The profile's format does not fit: fall through to detection,
                // but remember why.
                let detected = detect(refs);
                if detected.is_empty() {
                    return Outcome::Failed(format!("profile '{name}': {e}"));
                }
                return Outcome::Suggest(detected);
            }
        }
    }
    // 3. Detection.
    if input.saved.as_ref().is_some_and(|s| s.dismissed) {
        return Outcome::Nothing;
    }
    let detected = detect(refs);
    if detected.is_empty() {
        Outcome::Nothing
    } else {
        Outcome::Suggest(detected)
    }
}

/// Starts the worker that reads the first lines of `doc` and decides. The
/// result arrives on `tx`; `wake` requests a repaint. The worker stops when
/// `cancel` is set (the tab closed). An empty file is retried for a while so a
/// log that is still being created gets its structure once it has lines.
pub fn spawn_decide(
    doc: Arc<Document>,
    input: StructureInput,
    cancel: Arc<AtomicBool>,
    tx: Sender<StructureResult>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    let spawned = std::thread::Builder::new()
        .name("oxtail-structure".into())
        .spawn(move || {
            let mut tries = 0;
            let sample = loop {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let (_, lines) = doc.read_lines_blocking_with_generation(0, SAMPLE_LINES);
                if !lines.is_empty() || tries >= EMPTY_RETRIES {
                    break lines;
                }
                tries += 1;
                std::thread::sleep(Duration::from_millis(250));
            };
            let text: Vec<String> = sample.into_iter().map(|l| l.text).collect();
            let result = decide(&input, &text);
            if !cancel.load(Ordering::Relaxed) {
                let _ = tx.send(result);
                wake();
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot start the structure thread: {e}");
    }
}

/// The structure state of one tab.
pub struct Structure {
    /// The active parser.
    pub parser: Option<Arc<Parser>>,
    /// Its definition.
    pub spec: Option<ParserSpec>,
    /// Display name of the format.
    pub name: String,
    /// Where the parser came from.
    pub origin: Option<Origin>,
    /// The table view is showing.
    pub table: bool,
    /// Column order, widths, visibility, pinning.
    pub layout: ColumnLayout,
    /// A question to the user.
    pub suggestion: Option<Suggestion>,
    /// Everything detection found (for the chooser).
    pub detections: Vec<Detection>,
    /// The time parser.
    pub time: Arc<TimeParser>,
    /// The zone timestamps are displayed in.
    pub display_tz: TimeZone,
    /// The column holding the line's timestamp.
    pub ts_column: Option<usize>,
    /// Name of the timestamp column from the profile.
    ts_column_name: Option<String>,
    /// Bumped whenever something that affects painting changes.
    pub epoch: u64,
    /// The worker has answered.
    pub decided: bool,
    /// The user's choice changed and should be saved.
    pub dirty: bool,
    /// The user dismissed the suggestion.
    pub dismissed: bool,
    /// Why the profile's parser failed.
    pub problem: Option<String>,
    cancel: Arc<AtomicBool>,
    rx: Option<Receiver<StructureResult>>,
}

impl Default for Structure {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Structure {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Structure {
    /// No structure yet (plain text).
    pub fn new() -> Self {
        Self {
            parser: None,
            spec: None,
            name: String::new(),
            origin: None,
            table: false,
            layout: ColumnLayout::default(),
            suggestion: None,
            detections: Vec::new(),
            time: Arc::new(TimeParser::new(oxtail_time::TimeContext::utc())),
            display_tz: TimeZone::UTC,
            ts_column: None,
            ts_column_name: None,
            epoch: 0,
            decided: false,
            dirty: false,
            dismissed: false,
            problem: None,
            cancel: Arc::new(AtomicBool::new(false)),
            rx: None,
        }
    }

    /// Starts deciding on a worker thread.
    pub fn begin(
        &mut self,
        doc: &Arc<Document>,
        input: StructureInput,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) {
        // A newer decision replaces a running one.
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = unbounded();
        self.rx = Some(rx);
        self.decided = false;
        spawn_decide(Arc::clone(doc), input, Arc::clone(&self.cancel), tx, wake);
    }

    /// The worker's answer, if it has arrived. Never blocks.
    pub fn poll(&mut self) -> Option<StructureResult> {
        let r = self.rx.as_ref()?.try_recv().ok()?;
        self.rx = None;
        Some(r)
    }

    /// Whether a decision is pending.
    pub fn is_deciding(&self) -> bool {
        self.rx.is_some()
    }

    /// Applies a decision. Returns the parser now in use (or `None`).
    pub fn apply(&mut self, res: StructureResult, tz: &TimezoneSetting) -> Option<Arc<Parser>> {
        self.decided = true;
        self.time = Arc::new(res.time);
        self.display_tz = zone_of(tz);
        self.ts_column_name = res.ts_column;
        self.epoch += 1;
        match res.outcome {
            Outcome::Active(a) => {
                let ActiveParser {
                    spec,
                    parser,
                    name,
                    origin,
                    order,
                    table,
                    offer_table,
                    layout,
                } = *a;
                // A new parser that arrives while the table is showing (e.g. the
                // user switched profiles after accepting columns) keeps the table
                // instead of switching it off and asking again.
                let keep_table = self.table && self.parser.is_some();
                if offer_table && !keep_table {
                    self.suggestion = Some(Suggestion {
                        name: name.clone(),
                        spec: spec.clone(),
                        order: order.clone(),
                        profile: match &origin {
                            Origin::Profile(p) => Some(p.clone()),
                            _ => None,
                        },
                        score: 1.0,
                    });
                }
                self.install(spec, parser, name, origin, &order, layout.as_ref());
                self.table = table || keep_table;
            }
            Outcome::Suggest(list) => {
                if let Some(best) = list.first() {
                    self.suggestion = Some(Suggestion {
                        name: best.name.clone(),
                        spec: best.spec.clone(),
                        order: Vec::new(),
                        profile: None,
                        score: best.score,
                    });
                }
                self.detections = list;
            }
            Outcome::Nothing => {}
            Outcome::Failed(msg) => self.problem = Some(msg),
        }
        self.parser.clone()
    }

    fn install(
        &mut self,
        spec: ParserSpec,
        parser: Parser,
        name: String,
        origin: Origin,
        order: &[String],
        snapshot: Option<&crate::collayout::LayoutSnapshot>,
    ) {
        let schema = parser.schema().clone();
        let mut layout = ColumnLayout::new(&schema, &apply_order(&schema, order));
        if let Some(s) = snapshot {
            layout.apply_snapshot(&schema, s);
        }
        self.layout = layout;
        self.ts_column = self
            .ts_column_name
            .as_deref()
            .and_then(|n| schema.find(n))
            .or_else(|| {
                schema
                    .columns
                    .iter()
                    .position(|c| c.kind == ColumnKind::Timestamp)
            });
        self.parser = Some(Arc::new(parser));
        self.spec = Some(spec);
        self.name = name;
        self.origin = Some(origin);
        self.epoch += 1;
    }

    /// The user accepted the suggestion: use the parser and show the table.
    /// Returns the parser now in use.
    pub fn accept_suggestion(&mut self) -> Option<Arc<Parser>> {
        let s = self.suggestion.take()?;
        let parser = match s.spec.compile() {
            Ok(p) => p,
            Err(e) => {
                self.problem = Some(e.to_string());
                return self.parser.clone();
            }
        };
        let origin = match &s.profile {
            Some(p) => Origin::Profile(p.clone()),
            None => Origin::Detected,
        };
        // A profile parser may already be installed: keep its layout.
        if self.spec.as_ref() != Some(&s.spec) {
            self.install(s.spec, parser, s.name, origin, &s.order, None);
        }
        self.table = true;
        self.dismissed = false;
        self.dirty = true;
        self.epoch += 1;
        self.parser.clone()
    }

    /// The user does not want the suggestion.
    pub fn dismiss_suggestion(&mut self) {
        if self.suggestion.take().is_some() {
            self.dismissed = true;
            self.dirty = true;
        }
    }

    /// The user picked a parser (from the chooser). Returns it.
    pub fn choose(&mut self, spec: ParserSpec) -> Result<Arc<Parser>, String> {
        let parser = spec.compile().map_err(|e| e.to_string())?;
        let name = spec.display_name().to_string();
        self.install(spec, parser, name, Origin::Chosen, &[], None);
        self.suggestion = None;
        self.table = true;
        self.dismissed = false;
        self.dirty = true;
        self.parser.clone().ok_or_else(|| "no parser".to_string())
    }

    /// Back to plain text.
    pub fn clear(&mut self) {
        self.parser = None;
        self.spec = None;
        self.name.clear();
        self.origin = None;
        self.table = false;
        self.suggestion = None;
        self.dismissed = true;
        self.dirty = true;
        self.epoch += 1;
    }

    /// Shows or hides the table (only with a parser).
    pub fn set_table(&mut self, on: bool) {
        let on = on && self.parser.is_some();
        if self.table != on {
            self.table = on;
            self.dirty = true;
            self.epoch += 1;
        }
    }

    /// Changes the zone timestamps are displayed and read in.
    pub fn set_zone(&mut self, tz: &TimezoneSetting) {
        let zone = zone_of(tz);
        let mut t = (*self.time).clone();
        t.set_timezone(zone.clone());
        self.time = Arc::new(t);
        self.display_tz = zone;
        self.epoch += 1;
    }

    /// What to remember about this file.
    pub fn saved(&self) -> SavedStructure {
        SavedStructure {
            spec: self.spec.clone(),
            table: self.table,
            dismissed: self.dismissed,
            layout: self
                .parser
                .as_ref()
                .map(|p| self.layout.snapshot(p.schema())),
        }
    }

    /// The text shown in a cell: timestamps are re-rendered in the display
    /// zone, durations, sizes and levels are humanised.
    pub fn cell_text(&self, col: usize, rec: &Record<'_>) -> String {
        let Some(parser) = &self.parser else {
            return String::new();
        };
        let Some(v) = rec.get(col) else {
            return String::new();
        };
        let kind = parser
            .schema()
            .columns
            .get(col)
            .map_or(ColumnKind::Text, |c| c.kind);
        if kind == ColumnKind::Timestamp
            && let Some(ts) = self.time.parse(v)
        {
            return format_timestamp(ts, &self.display_tz);
        }
        oxtail_columns::format::format_cell(kind, v).into_owned()
    }

    /// What column queries need, when there is a parser.
    pub fn query_context(&self) -> Option<Arc<crate::qfilter::QueryContext>> {
        self.parser.as_ref().map(|p| {
            Arc::new(crate::qfilter::QueryContext {
                parser: Arc::clone(p),
                time: Arc::clone(&self.time),
            })
        })
    }

    /// The timestamp of a parsed record: the timestamp column when there is
    /// one, else the first timestamp in the line.
    pub fn record_time(&self, rec: Option<&Record<'_>>, line: &str) -> Option<Timestamp> {
        if let (Some(rec), Some(c)) = (rec, self.ts_column)
            && let Some(v) = rec.get(c)
            && let Some(ts) = self.time.parse(v)
        {
            return Some(ts);
        }
        self.time.parse(line)
    }
}

/// Formats a timestamp for a cell: seconds, milliseconds or microseconds,
/// whichever the value needs.
pub fn format_timestamp(ts: Timestamp, tz: &TimeZone) -> String {
    let ns = ts.subsec_nanosecond();
    let pattern = if ns == 0 {
        "%Y-%m-%d %H:%M:%S"
    } else if ns % 1_000_000 == 0 {
        "%Y-%m-%d %H:%M:%S%.3f"
    } else {
        "%Y-%m-%d %H:%M:%S%.6f"
    };
    helpers::format_in_zone(ts, tz, pattern)
}

/// Parses a line into an owned record (`None` for continuation lines).
pub fn parse_owned(parser: &Parser, text: &str) -> Option<Record<'static>> {
    parser.parse(text).map(Record::into_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxtail_config::ProfileSet;
    use oxtail_core::MemSource;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    const NGINX: &str = "127.0.0.1 - - [10/Oct/2000:13:55:36 -0700] \"GET /a HTTP/1.0\" 200 2326 \"-\" \"curl\"\n\
        127.0.0.1 - - [10/Oct/2000:13:55:37 -0700] \"GET /b HTTP/1.0\" 404 12 \"-\" \"curl\"\n\
        127.0.0.2 - - [10/Oct/2000:13:55:38 -0700] \"POST /c HTTP/1.0\" 500 0 \"-\" \"curl\"\n";

    fn nginx_input() -> StructureInput {
        let set = ProfileSet::builtin();
        let p = set
            .profiles()
            .find(|p| p.name.to_lowercase().contains("nginx"))
            .unwrap();
        StructureInput {
            profile_name: Some(p.name.clone()),
            columns: p
                .columns
                .as_ref()
                .map(|t| crate::colspec::columns_from_table(t).unwrap()),
            timestamp: p
                .timestamp
                .as_ref()
                .map(|t| crate::colspec::timestamp_from_table(t).unwrap()),
            saved: None,
            tz: Some(TimezoneSetting::Utc),
        }
    }

    #[test]
    fn a_profile_with_columns_applies_its_parser_and_offers_the_table() {
        let r = decide(&nginx_input(), &lines(NGINX));
        let Outcome::Active(a) = r.outcome else {
            panic!("expected an active parser");
        };
        assert!(matches!(a.origin, Origin::Profile(_)));
        assert!(!a.table && a.offer_table);
        assert_eq!(a.spec, ParserSpec::AccessCombined);
        assert!(r.time.format().is_some() || r.time.parse("10/Oct/2000:13:55:36 -0700").is_some());
    }

    #[test]
    fn no_profile_columns_means_a_detection_suggestion() {
        let input = StructureInput::default();
        let r = decide(&input, &lines(NGINX));
        let Outcome::Suggest(list) = r.outcome else {
            panic!("expected a suggestion");
        };
        assert_eq!(list[0].spec, ParserSpec::AccessCombined);
        // Plain prose gives nothing to suggest.
        let r = decide(&input, &lines("hello there\nthis is prose\nnothing here\n"));
        assert!(matches!(r.outcome, Outcome::Nothing));
    }

    /// Regression (CI run 17): switching the profile after accepting columns
    /// switched the table off again and re-offered it.
    #[test]
    fn a_profile_parser_arriving_while_the_table_shows_keeps_the_table() {
        let tz = TimezoneSetting::default();
        let mut st = Structure::new();
        // No table yet: the profile's parser is offered, not forced.
        st.apply(decide(&nginx_input(), &lines(NGINX)), &tz);
        assert!(!st.table && st.suggestion.is_some() && st.parser.is_some());
        st.accept_suggestion();
        assert!(st.table && st.suggestion.is_none());
        // Same kind of result again (a profile change): the table stays.
        st.apply(decide(&nginx_input(), &lines(NGINX)), &tz);
        assert!(st.table, "the table must stay on");
        assert!(
            st.suggestion.is_none(),
            "nothing to ask: columns are showing"
        );
    }

    #[test]
    fn a_remembered_choice_wins_and_dismissal_is_respected() {
        let mut input = nginx_input();
        input.saved = Some(SavedStructure {
            spec: Some(ParserSpec::AccessCommon),
            table: true,
            ..SavedStructure::default()
        });
        let Outcome::Active(a) = decide(&input, &lines(NGINX)).outcome else {
            panic!()
        };
        assert_eq!(a.spec, ParserSpec::AccessCommon);
        assert!(a.table);
        assert_eq!(a.origin, Origin::Restored);
        // Dismissed: plain text, even though detection would find something.
        let input = StructureInput {
            saved: Some(SavedStructure {
                dismissed: true,
                ..SavedStructure::default()
            }),
            ..StructureInput::default()
        };
        assert!(matches!(
            decide(&input, &lines(NGINX)).outcome,
            Outcome::Nothing
        ));
    }

    #[test]
    fn a_profile_that_does_not_fit_falls_back_to_detection() {
        let mut input = nginx_input();
        input.columns = Some(
            crate::colspec::columns_from_table(
                &"parser = \"regex\"\npattern = \"^NEVER(?P<x>.)$\""
                    .parse::<toml::Table>()
                    .unwrap(),
            )
            .unwrap(),
        );
        // The regex compiles but matches nothing: still "active" (the user
        // wrote it); a *broken* pattern falls back to detection.
        input.columns = Some(
            crate::colspec::columns_from_table(
                &"parser = \"regex\"\npattern = \"(broken\""
                    .parse::<toml::Table>()
                    .unwrap(),
            )
            .unwrap(),
        );
        let r = decide(&input, &lines(NGINX));
        assert!(matches!(r.outcome, Outcome::Suggest(_)), "{:?}", r.outcome);
        let r = decide(&input, &lines("plain prose only\n"));
        assert!(matches!(r.outcome, Outcome::Failed(_)));
    }

    #[test]
    fn accepting_installs_the_parser_and_shows_the_table() {
        let mut st = Structure::new();
        let r = decide(&StructureInput::default(), &lines(NGINX));
        assert!(st.apply(r, &TimezoneSetting::Utc).is_none());
        assert!(st.suggestion.is_some() && !st.table);
        let p = st.accept_suggestion().expect("parser");
        assert!(st.table && st.dirty && st.suggestion.is_none());
        assert_eq!(p.schema().len(), st.layout.cols.len());
        assert!(st.ts_column.is_some());
        let saved = st.saved();
        assert!(saved.table && saved.spec.is_some() && saved.layout.is_some());
        // Dismissing is remembered.
        let mut st = Structure::new();
        st.apply(
            decide(&StructureInput::default(), &lines(NGINX)),
            &TimezoneSetting::Utc,
        );
        st.dismiss_suggestion();
        assert!(st.dismissed && st.saved().dismissed && st.suggestion.is_none());
    }

    #[test]
    fn a_profile_parser_is_active_before_the_table_is_accepted() {
        let mut st = Structure::new();
        let r = decide(&nginx_input(), &lines(NGINX));
        assert!(st.apply(r, &TimezoneSetting::Utc).is_some());
        assert!(!st.table);
        let sug = st.suggestion.clone().expect("offer");
        assert!(sug.profile.is_some());
        st.accept_suggestion();
        assert!(st.table);
    }

    #[test]
    fn cells_are_formatted_by_kind_and_timestamps_in_the_display_zone() {
        let mut st = Structure::new();
        st.apply(
            decide(&StructureInput::default(), &lines(NGINX)),
            &TimezoneSetting::Utc,
        );
        st.accept_suggestion();
        let parser = st.parser.clone().unwrap();
        let first = NGINX.lines().next().unwrap();
        let rec = parse_owned(&parser, first).unwrap();
        let ts_col = st.ts_column.unwrap();
        // 13:55:36 -0700 is 20:55:36 UTC.
        assert_eq!(st.cell_text(ts_col, &rec), "2000-10-10 20:55:36");
        let size = parser.schema().find("size").unwrap();
        assert_eq!(st.cell_text(size, &rec), "2.27 KiB");
        st.set_zone(&TimezoneSetting::Named("Europe/Amsterdam".into()));
        assert_eq!(st.cell_text(ts_col, &rec), "2000-10-10 22:55:36");
        let t = st.record_time(Some(&rec), first).unwrap();
        assert_eq!(t.as_second(), 971_211_336);
    }

    #[test]
    fn timestamps_keep_the_precision_they_have() {
        let z = TimeZone::UTC;
        let t = |ms: i64| Timestamp::from_millisecond(ms).unwrap();
        assert_eq!(format_timestamp(t(1_000), &z), "1970-01-01 00:00:01");
        assert_eq!(format_timestamp(t(1_250), &z), "1970-01-01 00:00:01.250");
        let us = Timestamp::from_microsecond(1_000_123).unwrap();
        assert_eq!(format_timestamp(us, &z), "1970-01-01 00:00:01.000123");
    }

    #[test]
    fn the_worker_decides_from_a_document() {
        let doc = Arc::new(Document::from_source(
            Arc::new(MemSource::new(NGINX.as_bytes().to_vec())),
            "a.log",
        ));
        let mut st = Structure::new();
        st.begin(&doc, StructureInput::default(), Arc::new(|| {}));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let res = loop {
            if let Some(r) = st.poll() {
                break r;
            }
            assert!(std::time::Instant::now() < deadline, "no answer");
            std::thread::sleep(Duration::from_millis(2));
        };
        assert!(matches!(res.outcome, Outcome::Suggest(_)));
        assert!(res.sampled >= 3);
    }

    #[test]
    fn dropping_the_structure_cancels_the_worker() {
        let doc = Arc::new(Document::from_source(
            Arc::new(MemSource::new(Vec::new())),
            "empty.log",
        ));
        let mut st = Structure::new();
        st.begin(&doc, StructureInput::default(), Arc::new(|| {}));
        let flag = Arc::clone(&st.cancel);
        drop(st);
        assert!(flag.load(Ordering::Relaxed));
    }
}
