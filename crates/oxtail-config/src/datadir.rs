//! Data-folder resolution (PLAN.md §3.2).
//!
//! Resolution order:
//!
//! 1. `--data-dir <path>` ([`DataMode::Cli`]).
//! 2. **Portable**: a `portable` marker file or an `oxtail-data/` directory
//!    next to the executable; data lives in `<exe dir>/oxtail-data/`
//!    ([`DataMode::Portable`]).
//! 3. **Installed**: the platform config directory ([`DataMode::Installed`]).
//!
//! If the chosen folder cannot be created or written (read-only share, macOS
//! App Translocation of an unmoved download, ...) the result is
//! [`DataMode::InMemory`] with a human-readable reason, and `root` is `None`.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use crate::atomic::TEMP_SUFFIX;

/// Name of the data directory used in portable mode.
pub const PORTABLE_DIR: &str = "oxtail-data";
/// Name of the marker file that enables portable mode.
pub const PORTABLE_MARKER: &str = "portable";

/// How the data folder was chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataMode {
    /// Given with `--data-dir`.
    Cli,
    /// Next to the executable.
    Portable,
    /// The platform config directory.
    Installed,
    /// No writable folder: settings live only in memory.
    InMemory {
        /// Why no folder is used, suitable for a banner.
        reason: String,
    },
}

/// Inputs to [`DataDir::resolve`]; everything environmental is injectable so
/// tests need no real platform directories.
#[derive(Clone, Debug)]
pub struct ResolveInput {
    /// `--data-dir` value, if given.
    pub cli_override: Option<PathBuf>,
    /// Path of the running executable.
    pub exe_path: PathBuf,
    /// The platform config directory for installed mode (`None` if the
    /// platform has none). See [`platform_config_dir`].
    pub installed_dir: Option<PathBuf>,
}

impl ResolveInput {
    /// Input for the real environment: `current_exe()` and the platform
    /// config directory.
    pub fn from_environment(cli_override: Option<PathBuf>) -> Self {
        Self {
            cli_override,
            exe_path: std::env::current_exe().unwrap_or_default(),
            installed_dir: platform_config_dir(),
        }
    }
}

/// The platform config directory (`directories::ProjectDirs` with
/// qualifier `""`, organisation and application `"OxTail"`).
pub fn platform_config_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "OxTail", "OxTail").map(|d| d.config_dir().to_path_buf())
}

/// The resolved data folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataDir {
    /// The folder, or `None` for [`DataMode::InMemory`].
    pub root: Option<PathBuf>,
    /// How it was chosen.
    pub mode: DataMode,
}

impl DataDir {
    /// Resolves the data folder and makes sure `profiles/`, `themes/` and
    /// `tmp/` exist. Never fails: problems become [`DataMode::InMemory`].
    pub fn resolve(input: ResolveInput) -> DataDir {
        if let Some(p) = input.cli_override {
            return Self::prepare(p, DataMode::Cli);
        }
        if let Some(exe_dir) = input
            .exe_path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
        {
            let marker = exe_dir.join(PORTABLE_MARKER).is_file();
            let dir = exe_dir.join(PORTABLE_DIR);
            if marker || dir.is_dir() {
                if is_translocated(&input.exe_path) {
                    return Self::in_memory(
                        "OxTail is running from a temporary read-only location (macOS App \
                         Translocation). Move OxTail.app out of Downloads (e.g. to \
                         Applications) and start it again to keep your settings.",
                    );
                }
                return Self::prepare(dir, DataMode::Portable);
            }
        }
        match input.installed_dir {
            Some(d) => Self::prepare(d, DataMode::Installed),
            None => Self::in_memory("no per-user configuration folder is available"),
        }
    }

    /// A data folder without persistence.
    pub fn in_memory(reason: impl Into<String>) -> DataDir {
        DataDir {
            root: None,
            mode: DataMode::InMemory {
                reason: reason.into(),
            },
        }
    }

    fn prepare(root: PathBuf, mode: DataMode) -> DataDir {
        match ensure_layout(&root) {
            Ok(()) => DataDir {
                root: Some(root),
                mode,
            },
            Err(e) => Self::in_memory(format!(
                "the data folder {} is not writable ({e}); settings will not be saved. \
                 Choose another folder with --data-dir.",
                root.display()
            )),
        }
    }

    /// `true` unless in-memory.
    pub fn is_persistent(&self) -> bool {
        self.root.is_some()
    }

    /// The human-readable reason when running in memory.
    pub fn in_memory_reason(&self) -> Option<&str> {
        match &self.mode {
            DataMode::InMemory { reason } => Some(reason),
            _ => None,
        }
    }

    fn sub(&self, name: &str) -> Option<PathBuf> {
        self.root.as_ref().map(|r| r.join(name))
    }

    /// `settings.toml`.
    pub fn settings_path(&self) -> Option<PathBuf> {
        self.sub("settings.toml")
    }
    /// `session.json`.
    pub fn session_path(&self) -> Option<PathBuf> {
        self.sub("session.json")
    }
    /// `profiles/`.
    pub fn profiles_dir(&self) -> Option<PathBuf> {
        self.sub("profiles")
    }
    /// `themes/`.
    pub fn themes_dir(&self) -> Option<PathBuf> {
        self.sub("themes")
    }
    /// `tmp/`, for spool files.
    pub fn tmp_dir(&self) -> Option<PathBuf> {
        self.sub("tmp")
    }

    /// A stable identifier derived from the data folder, for naming the
    /// single-instance lock or pipe so that two portable copies never talk to
    /// each other. Same folder gives the same key; it is a 64-bit FNV-1a hash
    /// in hex (`"memory"` when there is no folder).
    pub fn instance_key(&self) -> String {
        let Some(root) = &self.root else {
            return "memory".to_string();
        };
        let canon = fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in canon.to_string_lossy().as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        format!("{h:016x}")
    }

    /// Deletes leftovers of crashed runs: everything in `tmp/` and dangling
    /// atomic-write temporaries (`*.oxtail-tmp`) in the data folder and its
    /// `profiles/` and `themes/`. Returns the number of entries removed.
    ///
    /// Call once at startup, after the single-instance lock is held: no other
    /// live OxTail uses this folder then, so everything in `tmp/` is stale.
    pub fn cleanup_stale_temp(&self) -> usize {
        let mut removed = 0;
        if let Some(tmp) = self.tmp_dir()
            && let Ok(rd) = fs::read_dir(&tmp)
        {
            for e in rd.flatten() {
                let p = e.path();
                let ok = if e.file_type().is_ok_and(|t| t.is_dir()) {
                    fs::remove_dir_all(&p)
                } else {
                    fs::remove_file(&p)
                };
                match ok {
                    Ok(()) => removed += 1,
                    Err(err) => tracing::warn!("cannot remove {}: {err}", p.display()),
                }
            }
        }
        for dir in [self.root.clone(), self.profiles_dir(), self.themes_dir()]
            .into_iter()
            .flatten()
        {
            let Ok(rd) = fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let name = e.file_name();
                if name.to_string_lossy().ends_with(TEMP_SUFFIX)
                    && fs::remove_file(e.path()).is_ok()
                {
                    removed += 1;
                }
            }
        }
        removed
    }
}

fn is_translocated(exe: &Path) -> bool {
    exe.to_string_lossy()
        .replace('\\', "/")
        .contains("/AppTranslocation/")
}

fn ensure_layout(root: &Path) -> io::Result<()> {
    for sub in ["", "profiles", "themes", "tmp"] {
        fs::create_dir_all(root.join(sub))?;
    }
    probe_writable(root)
}

/// Creates and removes a probe file in `dir`.
fn probe_writable(dir: &Path) -> io::Result<()> {
    let probe = dir.join(format!(".oxtail-probe-{}", std::process::id()));
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&probe)?;
    f.write_all(b"x")?;
    drop(f);
    fs::remove_file(&probe)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(exe: &Path) -> ResolveInput {
        ResolveInput {
            cli_override: None,
            exe_path: exe.to_path_buf(),
            installed_dir: None,
        }
    }

    #[test]
    fn cli_override_wins() {
        let t = tempfile::tempdir().expect("tempdir");
        let exe_dir = t.path().join("app");
        fs::create_dir_all(exe_dir.join(PORTABLE_DIR)).expect("mkdir");
        let cli = t.path().join("custom");
        let d = DataDir::resolve(ResolveInput {
            cli_override: Some(cli.clone()),
            exe_path: exe_dir.join("oxtail"),
            installed_dir: Some(t.path().join("inst")),
        });
        assert_eq!(d.mode, DataMode::Cli);
        assert_eq!(d.root.as_deref(), Some(cli.as_path()));
        for s in ["profiles", "themes", "tmp"] {
            assert!(cli.join(s).is_dir(), "{s}");
        }
    }

    #[test]
    fn portable_by_marker_file() {
        let t = tempfile::tempdir().expect("tempdir");
        fs::write(t.path().join(PORTABLE_MARKER), "").expect("marker");
        let d = DataDir::resolve(input(&t.path().join("oxtail")));
        assert_eq!(d.mode, DataMode::Portable);
        assert_eq!(d.root, Some(t.path().join(PORTABLE_DIR)));
        assert!(d.tmp_dir().expect("tmp").is_dir());
    }

    #[test]
    fn portable_by_existing_directory() {
        let t = tempfile::tempdir().expect("tempdir");
        fs::create_dir(t.path().join(PORTABLE_DIR)).expect("mkdir");
        let d = DataDir::resolve(input(&t.path().join("oxtail")));
        assert_eq!(d.mode, DataMode::Portable);
    }

    #[test]
    fn installed_when_no_marker() {
        let t = tempfile::tempdir().expect("tempdir");
        let inst = t.path().join("cfg/oxtail");
        let mut i = input(&t.path().join("bin/oxtail"));
        i.installed_dir = Some(inst.clone());
        let d = DataDir::resolve(i);
        assert_eq!(d.mode, DataMode::Installed);
        assert_eq!(d.root, Some(inst));
    }

    #[test]
    fn in_memory_without_any_folder() {
        let t = tempfile::tempdir().expect("tempdir");
        let d = DataDir::resolve(input(&t.path().join("oxtail")));
        assert!(matches!(d.mode, DataMode::InMemory { .. }));
        assert!(d.root.is_none() && d.settings_path().is_none());
        assert_eq!(d.instance_key(), "memory");
    }

    #[test]
    fn translocation_is_in_memory() {
        let t = tempfile::tempdir().expect("tempdir");
        let exe = t
            .path()
            .join("AppTranslocation/ABC/d/OxTail.app/Contents/MacOS/oxtail");
        fs::create_dir_all(exe.parent().expect("parent")).expect("mkdir");
        fs::write(exe.parent().expect("parent").join(PORTABLE_MARKER), "").expect("marker");
        // The path must contain "/AppTranslocation/" as a component pair.
        let d = DataDir::resolve(input(&exe));
        let reason = d.in_memory_reason().expect("in memory");
        assert!(reason.contains("Translocation"), "{reason}");
    }

    #[cfg(unix)]
    #[test]
    fn read_only_folder_is_in_memory() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().expect("tempdir");
        let ro = t.path().join("ro");
        fs::create_dir(&ro).expect("mkdir");
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).expect("chmod");
        // Root ignores permission bits; skip then.
        if fs::write(ro.join("probe"), "x").is_ok() {
            let _ = fs::remove_file(ro.join("probe"));
            eprintln!("skipped: running as root");
            return;
        }
        fs::write(t.path().join(PORTABLE_MARKER), "").expect("marker");
        let d = DataDir::resolve(ResolveInput {
            cli_override: Some(ro.join("data")),
            exe_path: t.path().join("oxtail"),
            installed_dir: None,
        });
        let reason = d.in_memory_reason().expect("in memory");
        assert!(reason.contains("not writable"), "{reason}");
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).expect("chmod back");
    }

    #[test]
    fn cleanup_removes_only_stale_files() {
        let t = tempfile::tempdir().expect("tempdir");
        let d = DataDir::resolve(ResolveInput {
            cli_override: Some(t.path().join("d")),
            exe_path: PathBuf::new(),
            installed_dir: None,
        });
        let root = d.root.clone().expect("root");
        fs::write(d.tmp_dir().expect("tmp").join("spool-1.bin"), "x").expect("w");
        fs::create_dir(d.tmp_dir().expect("tmp").join("sub")).expect("mkdir");
        fs::write(root.join(format!(".settings.toml.1.0{TEMP_SUFFIX}")), "x").expect("w");
        fs::write(root.join("settings.toml"), "keep").expect("w");
        assert_eq!(d.cleanup_stale_temp(), 3);
        assert!(root.join("settings.toml").exists());
        assert_eq!(
            fs::read_dir(d.tmp_dir().expect("tmp")).expect("rd").count(),
            0
        );
    }

    #[test]
    fn instance_key_is_stable_and_distinct() {
        let t = tempfile::tempdir().expect("tempdir");
        let mk = |n: &str| {
            DataDir::resolve(ResolveInput {
                cli_override: Some(t.path().join(n)),
                exe_path: PathBuf::new(),
                installed_dir: None,
            })
        };
        assert_eq!(mk("a").instance_key(), mk("a").instance_key());
        assert_ne!(mk("a").instance_key(), mk("b").instance_key());
    }
}
