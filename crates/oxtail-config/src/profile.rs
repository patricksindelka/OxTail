//! Profiles: `profiles/*.toml` plus the built-in ones.
//!
//! See the crate docs for the file shape. `columns`, `rules` and `timestamp`
//! are opaque TOML here.

use std::{fs, path::Path};

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::{ConfigError, Glob, write_atomic};

/// A profile: columns, highlight rules and a default filter, auto-selected by
/// file name and/or content.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    /// Display name, also the identity of the profile.
    pub name: String,
    /// Glob patterns matched against the file (see [`Glob`]).
    pub match_files: Vec<String>,
    /// Regex tried on each of the first lines of the file.
    pub match_content: Option<String>,
    /// Column definition (typed form lives in `oxtail-columns`).
    pub columns: Option<toml::Table>,
    /// Highlight rules (typed form lives in `oxtail-highlight`).
    pub rules: Vec<toml::Table>,
    /// Filter expression applied when the profile is selected.
    pub default_filter: Option<String>,
    /// Timestamp settings (column, format, zone).
    pub timestamp: Option<toml::Table>,
}

impl Profile {
    /// Parses one profile file.
    pub fn from_toml_str(text: &str) -> Result<Profile, toml::de::Error> {
        toml::from_str(text)
    }

    /// Serialises to TOML.
    pub fn to_toml_string(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Saves as `<dir>/<sanitised name>.toml` atomically and returns the path.
    pub fn save_to_dir(&self, dir: &Path) -> Result<std::path::PathBuf, ConfigError> {
        let stem: String = self
            .name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let stem = if stem.is_empty() {
            "profile".into()
        } else {
            stem
        };
        let path = dir.join(format!("{stem}.toml"));
        write_atomic(&path, self.to_toml_string()?.as_bytes())
            .map_err(|e| ConfigError::io(&path, e))?;
        Ok(path)
    }
}

/// A loaded value together with the non-fatal problems met while loading it.
#[derive(Clone, Debug, Default)]
pub struct Loaded<T> {
    /// The value.
    pub value: T,
    /// Human-readable warnings (skipped files, bad regexes, ...).
    pub warnings: Vec<String>,
}

struct Entry {
    profile: Profile,
    builtin: bool,
    content: Option<Regex>,
    globs: Vec<Glob>,
}

/// A set of profiles with compiled matchers.
pub struct ProfileSet {
    entries: Vec<Entry>,
}

const BUILTIN: &[(&str, &str)] = &[
    (
        "nginx-access",
        include_str!("../profiles/nginx-access.toml"),
    ),
    (
        "apache-access",
        include_str!("../profiles/apache-access.toml"),
    ),
    ("syslog", include_str!("../profiles/syslog.toml")),
    ("jsonl", include_str!("../profiles/jsonl.toml")),
    ("logfmt", include_str!("../profiles/logfmt.toml")),
    ("log4j", include_str!("../profiles/log4j.toml")),
    ("iis-w3c", include_str!("../profiles/iis-w3c.toml")),
    ("generic", include_str!("../profiles/generic.toml")),
];

impl ProfileSet {
    /// Builds a set from profiles (`builtin` marks the origin, which decides
    /// precedence in [`select`](Self::select)). Invalid regexes are dropped
    /// with a warning; the profile stays but never matches by content.
    fn build(profiles: Vec<(Profile, bool)>, warnings: &mut Vec<String>) -> Self {
        let entries = profiles
            .into_iter()
            .map(|(profile, builtin)| {
                let content =
                    profile.match_content.as_deref().and_then(|p| {
                        match RegexBuilder::new(p).size_limit(1 << 20).build() {
                            Ok(r) => Some(r),
                            Err(e) => {
                                warnings.push(format!(
                                    "profile '{}': invalid match_content: {e}",
                                    profile.name
                                ));
                                None
                            }
                        }
                    });
                let globs = profile
                    .match_files
                    .iter()
                    .map(|g| Glob::new(g, cfg!(windows)))
                    .collect();
                Entry {
                    profile,
                    builtin,
                    content,
                    globs,
                }
            })
            .collect();
        ProfileSet { entries }
    }

    /// A set of user profiles (in order) followed by the built-ins.
    /// Built-ins with the same name as a user profile are replaced.
    pub fn from_user_profiles(user: Vec<Profile>) -> Loaded<ProfileSet> {
        let mut warnings = Vec::new();
        let mut all: Vec<(Profile, bool)> = Vec::new();
        for (name, text) in BUILTIN {
            match Profile::from_toml_str(text) {
                Ok(p) => {
                    if !user.iter().any(|u| u.name == p.name) {
                        all.push((p, true));
                    }
                }
                // A broken built-in is a bug, caught by the unit tests.
                Err(e) => warnings.push(format!("built-in profile {name}: {e}")),
            }
        }
        let mut ordered: Vec<(Profile, bool)> = user.into_iter().map(|p| (p, false)).collect();
        ordered.extend(all);
        let set = Self::build(ordered, &mut warnings);
        Loaded {
            value: set,
            warnings,
        }
    }

    /// Only the built-in profiles.
    pub fn builtin() -> ProfileSet {
        Self::from_user_profiles(Vec::new()).value
    }

    /// Loads `dir/*.toml` (sorted by file name, invalid files skipped with a
    /// warning) plus the built-ins. A missing directory is not an error.
    pub fn load_dir(dir: &Path) -> Loaded<ProfileSet> {
        let mut warnings = Vec::new();
        let mut files: Vec<_> = fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.extension()
                            .is_some_and(|x| x.eq_ignore_ascii_case("toml"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        let mut user = Vec::new();
        for f in files {
            match fs::read(&f) {
                Ok(bytes) => match Profile::from_toml_str(&String::from_utf8_lossy(&bytes)) {
                    Ok(p) if !p.name.trim().is_empty() => user.push(p),
                    Ok(_) => warnings.push(format!("{}: profile has no name", f.display())),
                    Err(e) => warnings.push(format!("{}: {e}", f.display())),
                },
                Err(e) => warnings.push(format!("{}: {e}", f.display())),
            }
        }
        let mut loaded = Self::from_user_profiles(user);
        warnings.append(&mut loaded.warnings);
        loaded.warnings = warnings;
        loaded
    }

    /// All profiles, user ones first.
    pub fn profiles(&self) -> impl Iterator<Item = &Profile> {
        self.entries.iter().map(|e| &e.profile)
    }

    /// The profile with this exact name.
    pub fn by_name(&self, name: &str) -> Option<&Profile> {
        self.profiles().find(|p| p.name == name)
    }

    /// Picks the profile for a file.
    ///
    /// User profiles are considered before built-in ones. Within each group:
    /// a profile matching by both glob and content beats a content-only match,
    /// which beats a glob-only match; among glob matches the most specific
    /// pattern (most literal characters) wins; remaining ties go to the first
    /// profile in order. `first_lines` are the first lines of the file (the
    /// content regex must match at least one of them).
    pub fn select(&self, path: &Path, first_lines: &[&str]) -> Option<&Profile> {
        for builtin in [false, true] {
            let mut best: Option<(u8, usize, &Entry)> = None;
            for e in self.entries.iter().filter(|e| e.builtin == builtin) {
                let content = e
                    .content
                    .as_ref()
                    .is_some_and(|r| first_lines.iter().any(|l| r.is_match(l)));
                let glob = e
                    .globs
                    .iter()
                    .filter(|g| g.is_match_path(path))
                    .map(Glob::specificity)
                    .max();
                let score = match (content, glob) {
                    (true, Some(_)) => 3,
                    (true, None) => 2,
                    (false, Some(_)) => 1,
                    (false, None) => continue,
                };
                let spec = glob.unwrap_or(0);
                if best.is_none_or(|(s, sp, _)| (score, spec) > (s, sp)) {
                    best = Some((score, spec, e));
                }
            }
            if let Some((_, _, e)) = best {
                return Some(&e.profile);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str, files: &[&str], content: Option<&str>) -> Profile {
        Profile {
            name: name.into(),
            match_files: files.iter().map(|s| s.to_string()).collect(),
            match_content: content.map(str::to_string),
            ..Profile::default()
        }
    }

    #[test]
    fn builtins_parse_and_compile() {
        let l = ProfileSet::from_user_profiles(vec![]);
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
        assert_eq!(l.value.profiles().count(), BUILTIN.len());
        for name in [
            "Nginx access",
            "Apache access",
            "Syslog",
            "JSON lines",
            "logfmt",
            "Java / log4j",
            "IIS W3C",
            "Generic",
        ] {
            let prof = l.value.by_name(name).unwrap_or_else(|| panic!("{name}"));
            assert!(!prof.rules.is_empty(), "{name} has rules");
            for r in &prof.rules {
                assert!(
                    r.contains_key("match") && r.contains_key("scope"),
                    "{name}: {r:?}"
                );
            }
        }
        // Built-in rule regexes are valid.
        for prof in l.value.profiles() {
            for r in &prof.rules {
                if let Some(rx) = r
                    .get("match")
                    .and_then(|m| m.get("regex"))
                    .and_then(toml::Value::as_str)
                {
                    Regex::new(rx).unwrap_or_else(|e| panic!("{}: {rx}: {e}", prof.name));
                }
            }
        }
    }

    #[test]
    fn builtin_selection_by_content_and_name() {
        let set = ProfileSet::builtin();
        let sel =
            |path: &str, lines: &[&str]| set.select(Path::new(path), lines).map(|p| p.name.clone());
        let clf = r#"1.2.3.4 - - [29/Sep/2026:10:01:02 +0000] "GET / HTTP/1.1" 200 12"#;
        assert_eq!(
            sel("/var/log/nginx/access.log", &[clf]).as_deref(),
            Some("Nginx access")
        );
        assert_eq!(
            sel("/var/log/httpd/apache_access.log", &[clf]).as_deref(),
            Some("Apache access")
        );
        assert_eq!(
            sel("x.log", &[r#"{"level":"info","msg":"hi"}"#]).as_deref(),
            Some("JSON lines")
        );
        assert_eq!(
            sel("x.log", &["time=2026-09-29T10:00:00Z level=info msg=hello"]).as_deref(),
            Some("logfmt")
        );
        assert_eq!(
            sel("app.log", &["2026-09-29 10:01:02,123 ERROR boom"]).as_deref(),
            Some("Java / log4j")
        );
        assert_eq!(
            sel("x", &["Sep 29 10:01:02 host sshd[1]: hello"]).as_deref(),
            Some("Syslog")
        );
        assert_eq!(
            sel(
                "u_ex260929.log",
                &["#Software: Microsoft Internet Information Services 10.0"]
            )
            .as_deref(),
            Some("IIS W3C")
        );
        assert_eq!(sel("random.txt", &["hello"]), None);
        assert!(set.by_name("Generic").is_some());
    }

    #[test]
    fn precedence_content_over_glob_and_specificity() {
        let l = ProfileSet::from_user_profiles(vec![
            p("by-glob", &["*.log"], None),
            p("by-specific-glob", &["app-*.log"], None),
            p("by-content", &[], Some("^HELLO")),
        ]);
        let set = l.value;
        let sel = |path: &str, lines: &[&str]| {
            set.select(Path::new(path), lines).map(|p| p.name.as_str())
        };
        assert_eq!(sel("app-1.log", &["x"]), Some("by-specific-glob"));
        assert_eq!(sel("other.log", &["x"]), Some("by-glob"));
        assert_eq!(sel("other.log", &["HELLO"]), Some("by-content"));
        assert_eq!(sel("app-1.log", &["HELLO world"]), Some("by-content"));
        assert_eq!(sel("x.bin", &["x"]), None);
    }

    #[test]
    fn user_profiles_beat_builtins_and_replace_by_name() {
        let l = ProfileSet::from_user_profiles(vec![p("Syslog", &["mine.log"], None)]);
        let set = l.value;
        assert_eq!(set.profiles().filter(|x| x.name == "Syslog").count(), 1);
        let got = set
            .select(Path::new("mine.log"), &[r#"{"a":1}"#])
            .expect("match");
        assert_eq!(got.name, "Syslog");
        assert_eq!(got.match_files, vec!["mine.log".to_string()]);
    }

    #[test]
    fn bad_regex_warns_and_never_panics() {
        let l = ProfileSet::from_user_profiles(vec![p("bad", &["b.log"], Some("(unclosed"))]);
        assert_eq!(l.warnings.len(), 1);
        assert_eq!(
            l.value
                .select(Path::new("b.log"), &[])
                .map(|x| x.name.as_str()),
            Some("bad")
        );
    }

    #[test]
    fn load_dir_skips_garbage() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("a.toml"),
            "name = \"A\"\nmatch_files = [\"*.a\"]\n",
        )
        .expect("w");
        fs::write(dir.path().join("b.toml"), "this is = = not toml").expect("w");
        fs::write(dir.path().join("c.toml"), [0xff, 0xfe, 0, 1]).expect("w");
        fs::write(dir.path().join("d.toml"), "match_files = []").expect("w");
        fs::write(dir.path().join("e.txt"), "ignored").expect("w");
        let l = ProfileSet::load_dir(dir.path());
        assert!(l.value.by_name("A").is_some());
        assert_eq!(l.warnings.len(), 3, "{:?}", l.warnings);
        assert!(
            ProfileSet::load_dir(&dir.path().join("missing"))
                .warnings
                .is_empty()
        );
    }

    #[test]
    fn profile_round_trip_with_opaque_tables() {
        let text = r#"
name = "Go service (logfmt)"
match_files = ["*service*.log"]
default_filter = 'level != "debug"'

[columns]
parser = "logfmt"
order = ["time", "level", "msg", "*"]

[[rules]]
match = { column = "level", op = "eq", value = "error" }
scope = "line"
style = { bg = "error.subtle" }
alert = true
"#;
        let prof = Profile::from_toml_str(text).expect("parse");
        assert_eq!(prof.rules.len(), 1);
        let back = Profile::from_toml_str(&prof.to_toml_string().expect("ser")).expect("parse");
        assert_eq!(back, prof);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = prof.save_to_dir(dir.path()).expect("save");
        assert!(path.ends_with("Go_service__logfmt_.toml"), "{path:?}");
    }
}
