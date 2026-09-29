//! Resolving `--profile` input to the exact name of a profile.

use std::path::Path;

use oxtail_config::{Profile, ProfileSet, builtin_stems};

/// A profile that `--profile` can name: its display name and the file stem
/// (`nginx-access` for `nginx-access.toml`), if it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The profile's `name`.
    pub name: String,
    /// The file stem, when known.
    pub stem: Option<String>,
}

/// Collects the profiles available in `profiles_dir` (user files) plus the
/// built-in ones. Unreadable or invalid files are skipped.
pub fn candidates(profiles_dir: Option<&Path>) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    if let Some(dir) = profiles_dir
        && let Ok(rd) = std::fs::read_dir(dir)
    {
        let mut files: Vec<_> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("toml"))
            })
            .collect();
        files.sort();
        for f in files {
            let Ok(bytes) = std::fs::read(&f) else {
                continue;
            };
            let Ok(p) = Profile::from_toml_str(&String::from_utf8_lossy(&bytes)) else {
                continue;
            };
            if p.name.trim().is_empty() {
                continue;
            }
            let stem = f.file_stem().map(|s| s.to_string_lossy().into_owned());
            out.push(Candidate { name: p.name, stem });
        }
    }
    let stems = builtin_stems();
    for p in ProfileSet::builtin().profiles() {
        // A user profile with the same name replaces the built-in one.
        if out.iter().any(|c| c.name == p.name) {
            continue;
        }
        let stem = stems
            .iter()
            .find(|(_, n)| *n == p.name)
            .map(|(s, _)| s.clone());
        out.push(Candidate {
            name: p.name.clone(),
            stem,
        });
    }
    out
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn names_list(cands: &[Candidate]) -> String {
    cands
        .iter()
        .map(|c| format!("  {}", c.name))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Resolves `input` to the exact name of one candidate: exact name, name
/// ignoring case, file stem, then a unique prefix or word prefix of the name
/// or stem (case-insensitive). Ambiguous or unknown input is an error that
/// lists the profile names.
pub fn resolve(input: &str, cands: &[Candidate]) -> Result<String, String> {
    let want = input.trim();
    let lower = want.to_lowercase();
    if let Some(c) = cands.iter().find(|c| c.name == want) {
        return Ok(c.name.clone());
    }
    if let Some(c) = cands.iter().find(|c| c.name.to_lowercase() == lower) {
        return Ok(c.name.clone());
    }
    if let Some(c) = cands
        .iter()
        .find(|c| c.stem.as_ref().is_some_and(|s| s.to_lowercase() == lower))
    {
        return Ok(c.name.clone());
    }
    let hits: Vec<&Candidate> = if lower.is_empty() {
        Vec::new()
    } else {
        cands
            .iter()
            .filter(|c| {
                c.name.to_lowercase().starts_with(&lower)
                    || words(&c.name).iter().any(|w| w.starts_with(&lower))
                    || c.stem
                        .as_ref()
                        .is_some_and(|s| s.to_lowercase().starts_with(&lower))
            })
            .collect()
    };
    match hits.as_slice() {
        [one] => Ok(one.name.clone()),
        [] => Err(format!(
            "unknown profile '{input}'. Available profiles:\n{}",
            names_list(cands)
        )),
        many => {
            let owned: Vec<Candidate> = many.iter().map(|c| (*c).clone()).collect();
            Err(format!(
                "profile '{input}' is ambiguous. It matches:\n{}",
                names_list(&owned)
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str, stem: &str) -> Candidate {
        Candidate {
            name: name.into(),
            stem: Some(stem.into()),
        }
    }

    fn set() -> Vec<Candidate> {
        vec![
            c("Nginx access", "nginx-access"),
            c("Apache access", "apache-access"),
            c("logfmt", "logfmt"),
            c("Java / log4j", "log4j"),
            c("Syslog", "syslog"),
        ]
    }

    #[test]
    fn exact_and_case_insensitive() {
        assert_eq!(resolve("Nginx access", &set()).unwrap(), "Nginx access");
        assert_eq!(resolve("nginx ACCESS", &set()).unwrap(), "Nginx access");
    }

    #[test]
    fn file_stem() {
        assert_eq!(resolve("nginx-access", &set()).unwrap(), "Nginx access");
        assert_eq!(resolve("log4j", &set()).unwrap(), "Java / log4j");
    }

    #[test]
    fn unique_prefix_or_word() {
        assert_eq!(resolve("nginx", &set()).unwrap(), "Nginx access");
        assert_eq!(resolve("sys", &set()).unwrap(), "Syslog");
        assert_eq!(resolve("java", &set()).unwrap(), "Java / log4j");
    }

    #[test]
    fn ambiguous_lists_the_matches() {
        let e = resolve("access", &set()).unwrap_err();
        assert!(e.contains("ambiguous"), "{e}");
        assert!(
            e.contains("Nginx access") && e.contains("Apache access"),
            "{e}"
        );
        assert!(!e.contains("Syslog"), "{e}");
        // "log" is a prefix of logfmt and of log4j's stem.
        assert!(resolve("log", &set()).unwrap_err().contains("ambiguous"));
    }

    #[test]
    fn unknown_lists_all_names() {
        let e = resolve("nope", &set()).unwrap_err();
        assert!(e.contains("unknown profile 'nope'"), "{e}");
        assert!(e.contains("Syslog") && e.contains("logfmt"), "{e}");
        assert!(resolve("", &set()).is_err());
    }

    #[test]
    fn real_builtins_resolve() {
        let cands = candidates(None);
        assert_eq!(resolve("nginx", &cands).unwrap(), "Nginx access");
        assert_eq!(resolve("nginx-access", &cands).unwrap(), "Nginx access");
        assert_eq!(resolve("JSON lines", &cands).unwrap(), "JSON lines");
        assert_eq!(resolve("syslog", &cands).unwrap(), "Syslog");
    }

    #[test]
    fn user_profiles_are_candidates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("myapp.toml"),
            "name = \"My App\"\nmatch_files = [\"*.x\"]\n",
        )
        .unwrap();
        let cands = candidates(Some(dir.path()));
        assert_eq!(resolve("myapp", &cands).unwrap(), "My App");
        assert_eq!(resolve("my", &cands).unwrap(), "My App");
    }
}
