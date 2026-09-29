//! Built-in patterns for syslog and Apache/Nginx access logs.

use std::sync::OnceLock;

use regex::Regex;

const SYSLOG3164: &str = r"^(?:<(?P<pri>\d{1,3})>)?(?P<ts>[A-Z][a-z]{2} [ \d]\d \d\d:\d\d:\d\d) (?P<host>\S+) (?P<tag>[^\s:\[]+)(?:\[(?P<pid>\d+)\])?:? ?(?P<msg>.*)$";

const SYSLOG5424: &str = r"^<(?P<pri>\d{1,3})>(?P<version>\d{1,2}) (?P<ts>\S+) (?P<host>\S+) (?P<app>\S+) (?P<procid>\S+) (?P<msgid>\S+) (?P<sd>-|(?:\[(?:[^\]\\]|\\.)*\])+)(?: (?P<msg>.*))?$";

// The request line is split into method/path/proto when it looks like one;
// otherwise (scanner garbage, "-") those three stay empty.
const REQUEST: &str =
    r#""(?:(?P<method>[A-Z]+) (?P<path>[^\s"]+)(?: (?P<proto>[^"]*))?|(?:[^"\\]|\\.)*)""#;

fn access_common_pattern() -> String {
    format!(
        r"^(?P<remote>\S+) (?P<ident>\S+) (?P<user>\S+) \[(?P<ts>[^\]]+)\] {REQUEST} (?P<status>\d{{3}}) (?P<size>\d+|-)$"
    )
}

fn access_combined_pattern() -> String {
    format!(
        r#"^(?P<remote>\S+) (?P<ident>\S+) (?P<user>\S+) \[(?P<ts>[^\]]+)\] {REQUEST} (?P<status>\d{{3}}) (?P<size>\d+|-) "(?P<referer>(?:[^"\\]|\\.)*)" "(?P<agent>(?:[^"\\]|\\.)*)"(?: .*)?$"#
    )
}

macro_rules! cached {
    ($name:ident, $pat:expr) => {
        pub(crate) fn $name() -> &'static Regex {
            static R: OnceLock<Regex> = OnceLock::new();
            R.get_or_init(|| {
                // INVARIANT: the pattern is a compile-time constant, verified by tests.
                Regex::new(&$pat).expect("built-in pattern is valid")
            })
        }
    };
}

cached!(syslog3164, SYSLOG3164);
cached!(syslog5424, SYSLOG5424);
cached!(access_common, access_common_pattern());
cached!(access_combined, access_combined_pattern());
