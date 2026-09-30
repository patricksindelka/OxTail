//! The opt-in update check (PLAN.md §3.1: no phoning home).
//!
//! The check only ever runs when the user enabled `update_check` (or ran
//! `oxtail --check-update`); a portable copy never does it on its own. It asks
//! the GitHub Releases API for the latest release using the operating
//! system's `curl` (on Windows `System32\curl.exe`) (Windows 10 1803+, macOS and nearly every Linux), so the
//! binary needs no HTTP/TLS stack and links no extra system library.
//!
//! **OxTail only tells you a newer version exists.** It never downloads or
//! replaces the executable (self-update is post-1.0); [`Release::url`] is the
//! release page to open in a browser.
//!
//! Outcomes of [`check_latest`]:
//!
//! * newer release: `Ok(Some(release))`;
//! * up to date, no release published yet (HTTP 404), a draft/pre-release, or
//!   a version that cannot be parsed: `Ok(None)` (nothing actionable);
//!   [`check_latest_detailed`] separates "no release yet" ([`LatestCheck::NoRelease`]);
//! * `curl` missing, network failure, HTTP error other than 404, or an answer
//!   that is not the expected JSON: `Err(`[`ConfigError::Update`]`)`.
//!
//! [`check_due`] / [`record_check`] throttle the automatic check to once per
//! 24 hours using `update-check.json` in the data folder. With an in-memory
//! data folder a check is never due.

use std::{
    cmp::Ordering,
    io::Read,
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{ConfigError, DataDir, write_atomic};

/// The API endpoint asked for the latest release.
pub const LATEST_URL: &str = "https://api.github.com/repos/patricksindelka/OxTail/releases/latest";
/// File name of the throttle record in the data folder.
pub const THROTTLE_FILE: &str = "update-check.json";
/// Minimum time between automatic checks.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// The version of builds not made from a release tag: the repository keeps it
/// and the release workflow replaces it with the tag's version
/// (`packaging/version.sh`).
pub const DEV_VERSION: &str = "0.0.0-dev";
/// Answers larger than this are refused (memory stays bounded).
const MAX_ANSWER_BYTES: u64 = 1_000_000;
/// Release notes are cut to this many characters.
const MAX_NOTES_CHARS: usize = 4000;

/// A published release newer than the running version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// The version without a leading `v` (for example `1.2.0`).
    pub version: String,
    /// The release page to open in a browser.
    pub url: String,
    /// The release notes (Markdown), shortened, if the release has any.
    pub notes: Option<String>,
}

/// The outcome of a completed check, for callers that want to tell "no
/// release published yet" from "up to date".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LatestCheck {
    /// A newer release exists.
    Newer(Release),
    /// The latest release is not newer (or is a draft/pre-release, or its
    /// version cannot be compared).
    UpToDate,
    /// The project has not published any release yet (HTTP 404).
    NoRelease,
}

/// Blocking: asks GitHub for the latest release and returns it if it is newer
/// than `current_version`. Call it from a worker thread, never the UI thread.
/// See the module docs for what is an error and what is `Ok(None)`; `Ok(None)`
/// also covers "no release yet", which [`check_latest_detailed`] tells apart.
pub fn check_latest(current_version: &str) -> Result<Option<Release>, ConfigError> {
    Ok(match check_latest_detailed(current_version)? {
        LatestCheck::Newer(r) => Some(r),
        LatestCheck::UpToDate | LatestCheck::NoRelease => None,
    })
}

/// `true` for a development build ([`DEV_VERSION`], or any `-dev` version).
/// Every release is newer than it, so it is not compared with releases: the
/// automatic check skips it and a manual check says why.
pub fn is_dev_build(version: &str) -> bool {
    version.ends_with("-dev")
}

/// Like [`check_latest`], but distinguishes "no release published yet".
///
/// # Errors
/// Also [`ConfigError::Update`] for a development build ([`is_dev_build`]),
/// without asking the network.
pub fn check_latest_detailed(current_version: &str) -> Result<LatestCheck, ConfigError> {
    if is_dev_build(current_version) {
        return Err(ConfigError::Update(format!(
            "this is a development build ({current_version}); only release builds check for updates"
        )));
    }
    match fetch_latest() {
        Ok(json) => Ok(match parse_release(&json, current_version)? {
            Some(r) => LatestCheck::Newer(r),
            None => LatestCheck::UpToDate,
        }),
        Err(FetchError::NotFound) => Ok(LatestCheck::NoRelease),
        Err(FetchError::Other(msg)) => Err(ConfigError::Update(msg)),
    }
}

/// `true` if the last recorded check is more than 24 hours ago (or none was
/// recorded, or its record is unreadable) and the data folder is persistent.
pub fn check_due(dd: &DataDir) -> bool {
    due_at(dd, now_unix())
}

/// Records that a check happened now.
///
/// # Errors
/// [`ConfigError::NotPersistent`] for an in-memory data folder, or an I/O error.
pub fn record_check(dd: &DataDir) -> Result<(), ConfigError> {
    record_at(dd, now_unix())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[derive(Serialize, Deserialize)]
struct Throttle {
    last_check_unix: u64,
}

fn due_at(dd: &DataDir, now: u64) -> bool {
    let Some(root) = dd.root.as_deref() else {
        return false;
    };
    let last = std::fs::read_to_string(root.join(THROTTLE_FILE))
        .ok()
        .and_then(|t| serde_json::from_str::<Throttle>(&t).ok())
        .map(|t| t.last_check_unix);
    match last {
        None => true,
        // A clock that went backwards must not silence the check forever.
        Some(last) if last > now => true,
        Some(last) => now - last >= CHECK_INTERVAL.as_secs(),
    }
}

fn record_at(dd: &DataDir, now: u64) -> Result<(), ConfigError> {
    let Some(root) = dd.root.as_deref() else {
        return Err(ConfigError::NotPersistent(
            "there is no data folder to record the update check in".into(),
        ));
    };
    let path = root.join(THROTTLE_FILE);
    let json = serde_json::to_vec(&Throttle {
        last_check_unix: now,
    })?;
    write_atomic(&path, &json).map_err(|e| ConfigError::io(path, e))
}

// ------------------------------------------------------------- network ---

enum FetchError {
    NotFound,
    Other(String),
}

/// The `curl` to run. On Windows the one in `%SystemRoot%\System32`, never a
/// lookup through the current folder or `PATH`.
fn curl_program() -> Result<std::path::PathBuf, FetchError> {
    if cfg!(windows) {
        let curl = std::env::var_os("SystemRoot")
            .map(|r| {
                std::path::PathBuf::from(r)
                    .join("System32")
                    .join("curl.exe")
            })
            .filter(|p| p.is_file());
        curl.ok_or_else(|| {
            FetchError::Other(
                "`curl` was not found (Windows 10 version 1803 or later includes it)".into(),
            )
        })
    } else {
        Ok("curl".into())
    }
}

fn fetch_latest() -> Result<String, FetchError> {
    let mut cmd = Command::new(curl_program()?);
    cmd.args([
        "-fsSL",
        "--max-time",
        "10",
        "--max-filesize",
        "1000000",
        "-H",
        "Accept: application/vnd.github+json",
        LATEST_URL,
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            FetchError::Other("`curl` was not found; install curl to check for updates".into())
        } else {
            FetchError::Other(format!("cannot run curl: {e}"))
        }
    })?;
    // Read at most MAX_ANSWER_BYTES; dropping the pipe stops a runaway curl.
    let mut body = Vec::new();
    if let Some(out) = child.stdout.take() {
        let _ = out.take(MAX_ANSWER_BYTES + 1).read_to_end(&mut body);
    }
    let mut err_text = String::new();
    if let Some(err) = child.stderr.take() {
        let mut buf = Vec::new();
        let _ = err.take(4096).read_to_end(&mut buf);
        err_text = String::from_utf8_lossy(&buf).into_owned();
    }
    let status = child
        .wait()
        .map_err(|e| FetchError::Other(format!("curl: {e}")))?;
    if !status.success() {
        return Err(classify_curl_failure(&err_text));
    }
    if body.len() as u64 > MAX_ANSWER_BYTES {
        return Err(FetchError::Other("the answer is unexpectedly large".into()));
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn classify_curl_failure(stderr: &str) -> FetchError {
    if stderr.contains("error: 404") {
        FetchError::NotFound
    } else {
        let msg = stderr.trim();
        FetchError::Other(if msg.is_empty() {
            "curl failed".into()
        } else {
            format!("curl: {msg}")
        })
    }
}

// ------------------------------------------------------------- parsing ---

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: Option<String>,
    html_url: Option<String>,
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// Parses the API answer; `Some` only if it is a published release newer than
/// `current`.
fn parse_release(json: &str, current: &str) -> Result<Option<Release>, ConfigError> {
    let r: ApiRelease = serde_json::from_str(json)
        .map_err(|e| ConfigError::Update(format!("unexpected answer from GitHub: {e}")))?;
    if r.draft || r.prerelease {
        return Ok(None);
    }
    let (Some(tag), Some(url)) = (r.tag_name, r.html_url) else {
        // e.g. the `{"message": "Not Found"}` body.
        return Ok(None);
    };
    let (Some(new), Some(cur)) = (Version::parse(&tag), Version::parse(current)) else {
        return Ok(None);
    };
    if new <= cur {
        return Ok(None);
    }
    let notes = r
        .body
        .map(|b| b.trim().chars().take(MAX_NOTES_CHARS).collect::<String>())
        .filter(|b| !b.is_empty());
    Ok(Some(Release {
        version: tag.trim().trim_start_matches(['v', 'V']).to_string(),
        url,
        notes,
    }))
}

/// A semver-ish version: numeric `major[.minor[.patch]]`, optional
/// `-prerelease`, ignored `+build`. A pre-release sorts below its release.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Version {
    nums: [u64; 3],
    pre: Option<Vec<Ident>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ident {
    Num(u64),
    Text(String),
}

impl Ord for Ident {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Ident::Num(a), Ident::Num(b)) => a.cmp(b),
            (Ident::Num(_), Ident::Text(_)) => Ordering::Less,
            (Ident::Text(_), Ident::Num(_)) => Ordering::Greater,
            (Ident::Text(a), Ident::Text(b)) => a.cmp(b),
        }
    }
}
impl PartialOrd for Ident {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Version {
    fn parse(text: &str) -> Option<Version> {
        let t = text.trim().trim_start_matches(['v', 'V']);
        let t = t.split('+').next()?;
        let (core, pre) = match t.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (t, None),
        };
        let mut nums = [0u64; 3];
        for (count, part) in core.split('.').enumerate() {
            if count == 3 || part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            nums[count] = part.parse().ok()?;
        }
        let pre = match pre {
            None => None,
            Some("") => return None,
            Some(p) => Some(
                p.split('.')
                    .map(|id| match id.parse::<u64>() {
                        Ok(n) => Ident::Num(n),
                        Err(_) => Ident::Text(id.to_string()),
                    })
                    .collect(),
            ),
        };
        Some(Version { nums, pre })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.nums
            .cmp(&other.nums)
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(b),
            })
    }
}
impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DataMode;
    use proptest::prelude::*;

    fn newer(a: &str, b: &str) -> bool {
        Version::parse(a).unwrap() > Version::parse(b).unwrap()
    }

    #[test]
    fn version_comparison_table() {
        for (a, b) in [
            ("1.0.1", "1.0.0"),
            ("v1.10.0", "1.9.9"),
            ("2.0", "1.99.99"),
            ("1.0.0", "1.0.0-rc.1"),
            ("1.0.0-rc.2", "1.0.0-rc.1"),
            ("1.0.0-rc.10", "1.0.0-rc.9"),
            ("1.0.0-rc.1", "1.0.0-beta.5"),
            ("1.0.0-beta.1", "1.0.0-alpha.9"),
            ("1.0.0-1.a", "1.0.0-1"),
            ("1.0.0-alpha.1", "1.0.0-alpha"),
        ] {
            assert!(newer(a, b), "{a} should be newer than {b}");
            assert!(!newer(b, a), "{b} should not be newer than {a}");
        }
        assert!(!newer("1.0", "1.0.0"));
        assert!(!newer("1.0.0+build5", "1.0.0"));
    }

    #[test]
    fn development_builds_are_not_compared() {
        assert!(is_dev_build(DEV_VERSION));
        assert!(is_dev_build("1.2.0-dev"));
        assert!(!is_dev_build("0.0.2"));
        assert!(!is_dev_build("0.0.3-rc.1"));
        // Answered without the network, and says why.
        let err = check_latest_detailed(DEV_VERSION).unwrap_err().to_string();
        assert!(err.contains("development build"), "{err}");
        assert!(check_latest(DEV_VERSION).is_err());
    }

    #[test]
    fn unparseable_versions() {
        for s in [
            "", "abc", "1..2", "1.2.3.4", "1.x", "-1", "1.0.0-", "v", "1.-2",
        ] {
            assert!(Version::parse(s).is_none(), "{s:?}");
        }
    }

    const RELEASE: &str = r#"{"tag_name":"v1.2.0","html_url":"https://github.com/patricksindelka/OxTail/releases/tag/v1.2.0",
        "name":"1.2","draft":false,"prerelease":false,"body":"  * faster\n* fixes  ","assets":[]}"#;

    #[test]
    fn newer_release_is_reported() {
        let r = parse_release(RELEASE, "1.1.0").unwrap().unwrap();
        assert_eq!(r.version, "1.2.0");
        assert!(r.url.ends_with("/v1.2.0"));
        assert_eq!(r.notes.as_deref(), Some("* faster\n* fixes"));
    }

    #[test]
    fn same_or_older_release_is_none() {
        assert_eq!(parse_release(RELEASE, "1.2.0").unwrap(), None);
        assert_eq!(parse_release(RELEASE, "2.0.0").unwrap(), None);
        assert_eq!(
            parse_release(RELEASE, "1.2.0-rc.1")
                .unwrap()
                .unwrap()
                .version,
            "1.2.0"
        );
    }

    #[test]
    fn draft_prerelease_and_not_found_bodies_are_none() {
        let draft = RELEASE.replace("\"draft\":false", "\"draft\":true");
        let pre = RELEASE.replace("\"prerelease\":false", "\"prerelease\":true");
        assert_eq!(parse_release(&draft, "0.1.0").unwrap(), None);
        assert_eq!(parse_release(&pre, "0.1.0").unwrap(), None);
        assert_eq!(
            parse_release(r#"{"message":"Not Found","status":"404"}"#, "0.1.0").unwrap(),
            None
        );
    }

    #[test]
    fn unparseable_tags_and_no_notes() {
        let odd = RELEASE.replace("v1.2.0", "nightly");
        assert_eq!(parse_release(&odd, "0.1.0").unwrap(), None);
        assert_eq!(parse_release(RELEASE, "garbage").unwrap(), None);
        let empty = r#"{"tag_name":"1.3.0","html_url":"u","body":null}"#;
        assert_eq!(parse_release(empty, "1.0.0").unwrap().unwrap().notes, None);
    }

    #[test]
    fn garbage_answers_are_errors_not_panics() {
        for s in ["", "<html>", "[1]", "null", "{"] {
            assert!(
                matches!(parse_release(s, "1.0.0"), Err(ConfigError::Update(_))),
                "{s:?}"
            );
        }
    }

    #[test]
    fn long_notes_are_cut() {
        let long = format!(
            r#"{{"tag_name":"9.0.0","html_url":"u","body":"{}"}}"#,
            "x".repeat(10_000)
        );
        let n = parse_release(&long, "1.0.0")
            .unwrap()
            .unwrap()
            .notes
            .unwrap();
        assert_eq!(n.chars().count(), MAX_NOTES_CHARS);
    }

    #[test]
    fn curl_failures_are_classified() {
        assert!(matches!(
            classify_curl_failure("curl: (22) The requested URL returned error: 404"),
            FetchError::NotFound
        ));
        assert!(matches!(
            classify_curl_failure("curl: (6) Could not resolve host: api.github.com"),
            FetchError::Other(m) if m.contains("resolve")
        ));
        assert!(matches!(classify_curl_failure(""), FetchError::Other(_)));
        assert!(matches!(
            classify_curl_failure("curl: (22) The requested URL returned error: 403"),
            FetchError::Other(_)
        ));
    }

    fn dd(root: &std::path::Path) -> DataDir {
        DataDir {
            root: Some(root.to_path_buf()),
            mode: DataMode::Cli,
        }
    }

    #[test]
    fn throttle_with_injected_time() {
        let t = tempfile::tempdir().unwrap();
        let d = dd(t.path());
        assert!(due_at(&d, 1_000_000));
        record_at(&d, 1_000_000).unwrap();
        assert!(!due_at(&d, 1_000_000));
        assert!(!due_at(&d, 1_000_000 + 86_399));
        assert!(due_at(&d, 1_000_000 + 86_400));
        // Clock went backwards.
        assert!(due_at(&d, 10));
    }

    #[test]
    fn corrupt_throttle_file_means_due() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join(THROTTLE_FILE), "not json").unwrap();
        assert!(due_at(&dd(t.path()), 5));
    }

    #[test]
    fn in_memory_is_never_due_and_cannot_record() {
        let d = DataDir::in_memory("read-only");
        assert!(!due_at(&d, u64::MAX));
        assert!(!check_due(&d));
        assert!(matches!(
            record_check(&d),
            Err(ConfigError::NotPersistent(_))
        ));
    }

    proptest! {
        #[test]
        fn never_newer_than_itself(a in 0u64..1000, b in 0u64..1000, c in 0u64..1000,
                                   pre in proptest::option::of("[a-z0-9]{1,4}(\\.[a-z0-9]{1,3}){0,2}")) {
            let s = match pre {
                Some(p) => format!("{a}.{b}.{c}-{p}"),
                None => format!("{a}.{b}.{c}"),
            };
            let v = Version::parse(&s);
            prop_assert!(v.is_some());
            let v = v.unwrap();
            prop_assert!(v.cmp(&v) == Ordering::Equal);
            let json = format!(r#"{{"tag_name":"{s}","html_url":"u"}}"#);
            prop_assert!(parse_release(&json, &s).unwrap().is_none());
        }

        #[test]
        fn ordering_is_antisymmetric(a in "[0-9a-z.+-]{0,12}", b in "[0-9a-z.+-]{0,12}") {
            if let (Some(x), Some(y)) = (Version::parse(&a), Version::parse(&b)) {
                prop_assert_eq!(x.cmp(&y), y.cmp(&x).reverse());
            }
        }

        #[test]
        fn parse_never_panics(s in "\\PC{0,30}") {
            let _ = Version::parse(&s);
            let _ = parse_release(&s, "1.0.0");
        }
    }
}
