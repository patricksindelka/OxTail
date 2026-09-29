//! Alerts: rules with `actions.alert` raise desktop notifications for new
//! lines while a file grows; rules with `actions.bookmark` bookmark them.
//!
//! The scan runs on a worker thread (it reads lines with
//! `Document::read_lines_blocking`, which the UI thread must not call). The
//! UI submits [`AlertJob`]s for line ranges that appeared since the last
//! check and drains [`AlertEvent`]s. Notifications are rate limited to one
//! per rule per five seconds; matches inside that window are counted and
//! reported as "+N more" with the next notification (or on their own once
//! the window ends).

use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use oxtail_core::{Document, Line};
use oxtail_highlight::{CompiledRules, Rule};

/// Minimum time between two notifications of the same rule.
pub const RATE_LIMIT: Duration = Duration::from_secs(5);
/// Lines read per step.
const CHUNK: u64 = 5_000;
/// Most lines one job looks at (the newest ones when more arrived).
pub const MAX_JOB_LINES: u64 = 50_000;
/// Longest line excerpt in a notification.
const EXCERPT: usize = 200;

/// What the rate limiter decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Show a notification; `more` matches were suppressed since the last one.
    Notify {
        /// Suppressed matches to mention ("+N more").
        more: u32,
    },
    /// Stay silent (counted for the next notification).
    Suppress,
}

#[derive(Debug, Default, Clone, Copy)]
struct RuleState {
    last: Option<Instant>,
    suppressed: u32,
}

/// Per-rule rate limiting.
#[derive(Debug, Default)]
pub struct RateLimiter {
    rules: HashMap<usize, RuleState>,
}

impl RateLimiter {
    /// A match of `rule` happened at `now`.
    pub fn hit(&mut self, rule: usize, now: Instant) -> Decision {
        let st = self.rules.entry(rule).or_default();
        match st.last {
            Some(t) if now.saturating_duration_since(t) < RATE_LIMIT => {
                st.suppressed = st.suppressed.saturating_add(1);
                Decision::Suppress
            }
            _ => {
                st.last = Some(now);
                let more = std::mem::take(&mut st.suppressed);
                Decision::Notify { more }
            }
        }
    }

    /// Rules with suppressed matches whose window has ended: `(rule, count)`.
    /// The counts are reset (a summary notification counts as a notification).
    pub fn due_summaries(&mut self, now: Instant) -> Vec<(usize, u32)> {
        let mut out = Vec::new();
        for (rule, st) in &mut self.rules {
            if st.suppressed > 0
                && st
                    .last
                    .is_none_or(|t| now.saturating_duration_since(t) >= RATE_LIMIT)
            {
                out.push((*rule, std::mem::take(&mut st.suppressed)));
                st.last = Some(now);
            }
        }
        out.sort_unstable();
        out
    }
}

/// A line that triggered a rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Index of the rule in the rule list.
    pub rule: usize,
    /// 0-based line number.
    pub number: u64,
    /// The line text (cut to an excerpt).
    pub text: String,
}

/// What scanning a batch of lines found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ScanResult {
    /// Alert hits, in line order.
    pub hits: Vec<Hit>,
    /// Lines to bookmark as `(number, offset)`.
    pub bookmarks: Vec<(u64, u64)>,
}

/// Runs the rules over `lines`. `rules` must be the slice `compiled` was
/// compiled from.
pub fn scan_lines(lines: &[Line], compiled: &CompiledRules, rules: &[Rule]) -> ScanResult {
    let mut out = ScanResult::default();
    for line in lines {
        let hl = compiled.highlight(&line.text, None);
        if !hl.alert && !hl.bookmark {
            continue;
        }
        if hl.bookmark && line.number_exact {
            out.bookmarks.push((line.number, line.offset));
        }
        if hl.alert {
            for &i in &hl.matched_rules {
                if rules.get(i).is_some_and(|r| r.enabled && r.actions.alert) {
                    out.hits.push(Hit {
                        rule: i,
                        number: line.number,
                        text: crate::util::shorten(&line.text, EXCERPT),
                    });
                }
            }
        }
    }
    out
}

/// Something that can show a desktop notification.
pub trait Notifier: Send + Sync {
    /// Shows a notification.
    fn notify(&self, summary: &str, body: &str);
}

/// Notifications through the desktop's notification service.
pub struct DesktopNotifier;

impl Notifier for DesktopNotifier {
    fn notify(&self, summary: &str, body: &str) {
        let result = notify_rust::Notification::new()
            .appname("OxTail")
            .summary(summary)
            .body(body)
            .show();
        if let Err(e) = result {
            tracing::debug!("desktop notification failed: {e}");
        }
    }
}

/// Work for the alert thread: look at lines `first..first + count` of `doc`.
pub struct AlertJob {
    /// The tab the lines belong to.
    pub tab_id: u64,
    /// Tab name for the notification.
    pub tab_name: String,
    /// The document to read from.
    pub doc: Arc<Document>,
    /// The generation the line numbers refer to.
    pub generation: u64,
    /// First line.
    pub first: u64,
    /// Number of lines.
    pub count: u64,
    /// The rules in effect.
    pub rules: Arc<Vec<Rule>>,
    /// The rules, compiled.
    pub compiled: Arc<CompiledRules>,
    /// Show desktop notifications (otherwise only report hits).
    pub notify: bool,
}

/// What the alert thread reports back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlertEvent {
    /// `count` alert lines matched in tab `tab_id`.
    Hits {
        /// The tab.
        tab_id: u64,
        /// Number of matching lines.
        count: u32,
    },
    /// Lines to bookmark in tab `tab_id`.
    Bookmarks {
        /// The tab.
        tab_id: u64,
        /// The generation the numbers refer to.
        generation: u64,
        /// `(number, offset)` pairs.
        lines: Vec<(u64, u64)>,
    },
}

/// The alert thread. Dropping it lets the thread finish and exit.
pub struct AlertWorker {
    tx: Option<Sender<AlertJob>>,
    /// Results.
    pub events: Receiver<AlertEvent>,
    handle: Option<JoinHandle<()>>,
}

impl AlertWorker {
    /// Starts the thread. `wake` is called after each batch of events.
    pub fn spawn(wake: Arc<dyn Fn() + Send + Sync>, notifier: Arc<dyn Notifier>) -> Self {
        let (tx, rx) = unbounded::<AlertJob>();
        let (ev_tx, events) = unbounded::<AlertEvent>();
        let handle = std::thread::Builder::new()
            .name("oxtail-alerts".into())
            .spawn(move || worker_loop(&rx, &ev_tx, &*wake, &*notifier))
            .map_err(|e| tracing::warn!("cannot start the alert thread: {e}"))
            .ok();
        Self {
            tx: Some(tx),
            events,
            handle,
        }
    }

    /// Queues a job. Never blocks.
    pub fn submit(&self, job: AlertJob) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(job);
        }
    }
}

impl Drop for AlertWorker {
    fn drop(&mut self) {
        // Closing the channel ends the loop; do not wait for it (a notification
        // may be blocked on the desktop's message bus).
        self.tx.take();
        drop(self.handle.take());
    }
}

fn worker_loop(
    rx: &Receiver<AlertJob>,
    ev_tx: &Sender<AlertEvent>,
    wake: &(dyn Fn() + Send + Sync),
    notifier: &dyn Notifier,
) {
    // One limiter per tab: rule indices are only meaningful within a rule set.
    let mut limiters: HashMap<u64, (RateLimiter, String, Arc<Vec<Rule>>)> = HashMap::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(job) => run_job(&job, &mut limiters, ev_tx, wake, notifier),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let now = Instant::now();
        for (limiter, tab, rules) in limiters.values_mut() {
            for (rule, more) in limiter.due_summaries(now) {
                let name = rules.get(rule).map_or("alert", |r| r.name.as_str());
                notifier.notify(
                    &format!("OxTail: {name}"),
                    &format!("{tab}: {more} more matching lines"),
                );
            }
        }
    }
}

fn run_job(
    job: &AlertJob,
    limiters: &mut HashMap<u64, (RateLimiter, String, Arc<Vec<Rule>>)>,
    ev_tx: &Sender<AlertEvent>,
    wake: &(dyn Fn() + Send + Sync),
    notifier: &dyn Notifier,
) {
    let entry = limiters.entry(job.tab_id).or_insert_with(|| {
        (
            RateLimiter::default(),
            job.tab_name.clone(),
            job.rules.clone(),
        )
    });
    entry.1.clone_from(&job.tab_name);
    entry.2 = Arc::clone(&job.rules);

    // Only the newest lines of a huge burst.
    let skip = job.count.saturating_sub(MAX_JOB_LINES);
    let mut first = job.first + skip;
    let end = job.first + job.count;
    let mut hits_total = 0u32;
    let mut bookmarks: Vec<(u64, u64)> = Vec::new();
    while first < end {
        if job.doc.generation() != job.generation {
            return;
        }
        let n = (end - first).min(CHUNK);
        let lines = job.doc.read_lines_blocking(first, n as usize);
        if lines.is_empty() {
            break;
        }
        let res = scan_lines(&lines, &job.compiled, &job.rules);
        let now = Instant::now();
        for hit in &res.hits {
            hits_total = hits_total.saturating_add(1);
            if !job.notify {
                continue;
            }
            if let Decision::Notify { more } = entry.0.hit(hit.rule, now) {
                let name = job.rules.get(hit.rule).map_or("alert", |r| r.name.as_str());
                let mut body = format!("{}: {}", job.tab_name, hit.text);
                if more > 0 {
                    body.push_str(&format!("\n+{more} more"));
                }
                notifier.notify(&format!("OxTail: {name}"), &body);
            }
        }
        bookmarks.extend(res.bookmarks);
        first += lines.len() as u64;
    }
    if hits_total > 0 {
        let _ = ev_tx.send(AlertEvent::Hits {
            tab_id: job.tab_id,
            count: hits_total,
        });
    }
    if !bookmarks.is_empty() {
        let _ = ev_tx.send(AlertEvent::Bookmarks {
            tab_id: job.tab_id,
            generation: job.generation,
            lines: bookmarks,
        });
    }
    if hits_total > 0 || !ev_tx.is_empty() {
        wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxtail_core::MemSource;
    use oxtail_highlight::RuleActions;
    use std::sync::Mutex;

    fn alert_rule(name: &str, text: &str) -> Rule {
        let mut r = Rule::literal(name, text);
        r.actions = RuleActions {
            alert: true,
            ..RuleActions::default()
        };
        r
    }

    fn line(n: u64, text: &str) -> Line {
        Line {
            number: n,
            number_exact: true,
            offset: n * 100,
            len: 100,
            text: text.into(),
            truncated: false,
        }
    }

    #[test]
    fn first_hit_notifies_then_suppresses() {
        let mut l = RateLimiter::default();
        let t0 = Instant::now();
        assert_eq!(l.hit(0, t0), Decision::Notify { more: 0 });
        assert_eq!(l.hit(0, t0 + Duration::from_secs(1)), Decision::Suppress);
        assert_eq!(l.hit(0, t0 + Duration::from_secs(4)), Decision::Suppress);
        // Another rule is independent.
        assert_eq!(
            l.hit(1, t0 + Duration::from_secs(2)),
            Decision::Notify { more: 0 }
        );
        // After the window the next hit carries the count.
        assert_eq!(
            l.hit(0, t0 + Duration::from_secs(6)),
            Decision::Notify { more: 2 }
        );
        assert_eq!(l.hit(0, t0 + Duration::from_secs(7)), Decision::Suppress);
    }

    #[test]
    fn leftover_suppressed_matches_get_a_summary() {
        let mut l = RateLimiter::default();
        let t0 = Instant::now();
        l.hit(3, t0);
        l.hit(3, t0 + Duration::from_secs(1));
        l.hit(3, t0 + Duration::from_secs(2));
        assert!(l.due_summaries(t0 + Duration::from_secs(3)).is_empty());
        assert_eq!(l.due_summaries(t0 + Duration::from_secs(5)), vec![(3, 2)]);
        // Reported once.
        assert!(l.due_summaries(t0 + Duration::from_secs(20)).is_empty());
        // And the window restarted from the summary.
        assert_eq!(l.hit(3, t0 + Duration::from_secs(6)), Decision::Suppress);
    }

    #[test]
    fn scanning_finds_alerts_and_bookmarks() {
        let mut bm = Rule::literal("bm", "checkpoint");
        bm.actions.bookmark = true;
        let rules = vec![
            alert_rule("boom", "boom"),
            bm,
            Rule::literal("plain", "info"),
        ];
        let compiled = CompiledRules::compile(&rules).unwrap();
        let lines = vec![
            line(0, "info: fine"),
            line(1, "BOOM happened"),
            line(2, "checkpoint reached"),
            line(3, "boom and checkpoint"),
        ];
        let res = scan_lines(&lines, &compiled, &rules);
        assert_eq!(res.hits.len(), 2);
        assert_eq!(res.hits[0].number, 1);
        assert_eq!(res.hits[0].rule, 0);
        assert_eq!(res.bookmarks, vec![(2, 200), (3, 300)]);
    }

    #[test]
    fn disabled_alert_rules_are_ignored() {
        let mut r = alert_rule("boom", "boom");
        r.enabled = false;
        let rules = vec![r];
        let compiled = CompiledRules::compile(&rules).unwrap();
        assert_eq!(
            scan_lines(&[line(0, "boom")], &compiled, &rules),
            ScanResult::default()
        );
    }

    #[derive(Default)]
    struct Recorder(Mutex<Vec<(String, String)>>);
    impl Notifier for Recorder {
        fn notify(&self, summary: &str, body: &str) {
            self.0.lock().unwrap().push((summary.into(), body.into()));
        }
    }

    fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn worker_notifies_once_per_window_and_reports_hits() {
        let text: String = (0..30)
            .map(|i| if i % 2 == 0 { "ERROR bad\n" } else { "ok\n" })
            .collect();
        let doc = Arc::new(Document::from_source(
            Arc::new(MemSource::new(text.into_bytes())),
            "t.log",
        ));
        wait_for(|| {
            let s = doc.snapshot();
            (s.lines.exact && s.lines.known == 30).then_some(())
        });
        let rules = Arc::new(vec![alert_rule("errors", "ERROR")]);
        let compiled = Arc::new(CompiledRules::compile(&rules).unwrap());
        let rec = Arc::new(Recorder::default());
        let worker = AlertWorker::spawn(Arc::new(|| {}), rec.clone());
        worker.submit(AlertJob {
            tab_id: 7,
            tab_name: "t.log".into(),
            doc: doc.clone(),
            generation: doc.generation(),
            first: 0,
            count: 30,
            rules,
            compiled,
            notify: true,
        });
        let ev = wait_for(|| worker.events.try_recv().ok());
        assert_eq!(
            ev,
            AlertEvent::Hits {
                tab_id: 7,
                count: 15
            }
        );
        let sent = rec.0.lock().unwrap().clone();
        // 15 matches in one burst: a single notification, the rest suppressed.
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(sent[0].0.contains("errors"));
        assert!(sent[0].1.contains("ERROR bad"));
    }

    #[test]
    fn stale_generation_jobs_are_dropped() {
        let doc = Arc::new(Document::from_source(
            Arc::new(MemSource::new(b"boom\n".to_vec())),
            "t.log",
        ));
        wait_for(|| (doc.snapshot().lines.known == 1).then_some(()));
        let rules = Arc::new(vec![alert_rule("boom", "boom")]);
        let compiled = Arc::new(CompiledRules::compile(&rules).unwrap());
        let rec = Arc::new(Recorder::default());
        let worker = AlertWorker::spawn(Arc::new(|| {}), rec.clone());
        worker.submit(AlertJob {
            tab_id: 1,
            tab_name: "t".into(),
            doc: doc.clone(),
            generation: doc.generation() + 5,
            first: 0,
            count: 1,
            rules,
            compiled,
            notify: true,
        });
        std::thread::sleep(Duration::from_millis(200));
        assert!(worker.events.try_recv().is_err());
        assert!(rec.0.lock().unwrap().is_empty());
    }
}
