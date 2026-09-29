//! Opt-in system integration and its removal (PLAN.md §3.1).
//!
//! OxTail is portable: it never touches the registry, the desktop database or
//! any file outside its data folder on its own. Only [`integrate`], called
//! from an explicit "Integrate with system" action (or `oxtail --integrate`),
//! does, and [`remove`] undoes exactly what it did.
//!
//! Everything [`integrate`] creates is recorded in `integration.json` in the
//! data folder (exact file paths, registry keys it created and registry
//! values it set). [`remove`] reads that manifest, so it works even if the
//! executable has moved or was deleted.
//!
//! # Safety of removal
//!
//! A manifest is just a file, so it is never trusted blindly. Both
//! [`integrate`] and [`remove`] only act on entries that `is_ours` accepts:
//! the two registry trees OxTail owns, its `OpenWithProgids` values, and its
//! three well-known file names directly inside the expected folders. Any
//! other entry is skipped and reported, and dropped from the manifest.
//! Existing things are not clobbered: a `.desktop`, icon or shortcut file that
//! exists and was not created by OxTail is left alone, registry keys that
//! existed before are never deleted (only the values we set, and keys we
//! created that are empty afterwards). The manifest carries a fingerprint of
//! the computer and account; if a portable data folder is moved to another
//! machine, [`remove`] refuses instead of deleting that machine's entries.
//!
//! What is written, per platform:
//!
//! * **Windows** (per user, no administrator rights), all under
//!   `HKCU\Software\Classes`: `Applications\oxtail.exe` (friendly name, open
//!   command, supported types), the ProgId `OxTail.LogFile`, and an
//!   `OpenWithProgids` value under `.log .txt .out .err .trace` so OxTail is
//!   offered in "Open with". The default association is never taken over.
//!   Plus a Start menu shortcut `OxTail.lnk` (skipped, with a message, if the
//!   path is not a plain UTF-8 drive path). Explorer is not notified (that
//!   needs `unsafe`), so it may take a moment to show the entries.
//! * **Linux / other Unix**: `applications/io.github.patricksindelka.OxTail.desktop`
//!   and the 256 px icon under `$XDG_DATA_HOME` (default `~/.local/share`).
//!   `update-desktop-database` runs if available (at most 5 seconds).
//!   `mimeapps.list` is left alone, so OxTail is offered in "Open with" but
//!   never the default.
//! * **macOS**: unsupported; Finder handles "Open With" for `OxTail.app`.

use std::{
    io::Read,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::{ConfigError, DataDir, write_atomic};

/// File name of the manifest inside the data folder.
pub const MANIFEST_FILE: &str = "integration.json";
/// Reverse-DNS application id (desktop file name and icon name).
const APP_ID: &str = "io.github.patricksindelka.OxTail";
/// The 256 px application icon, embedded.
static ICON_256: &[u8] = include_bytes!("../../../packaging/icons/oxtail-256.png");
/// Extensions OxTail is offered for on Windows.
const WINDOWS_EXTENSIONS: [&str; 5] = [".log", ".txt", ".out", ".err", ".trace"];
/// The Windows ProgId.
const PROG_ID: &str = "OxTail.LogFile";
const APP_KEY: &str = "Software\\Classes\\Applications\\oxtail.exe";
const APPS_KEY: &str = "Software\\Classes\\Applications";
const PROG_KEY: &str = "Software\\Classes\\OxTail.LogFile";
const CLASSES_KEY: &str = "Software\\Classes";
const LNK_NAME: &str = "OxTail.lnk";
/// Marker line in the `.desktop` file that proves the file is ours.
const MARKER_KEY: &str = "X-OxTail-Integration=";
const MACOS_MESSAGE: &str = "On macOS, Finder handles \u{201c}Open With\u{201d} for OxTail.app. \
    Move OxTail.app to Applications; a bare binary cannot be registered.";
const MAX_READ: u64 = 1_000_000;
const HOOK_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether OxTail is currently registered with the system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntegrationState {
    /// Integrated; `items` describes each file or registry entry (human readable).
    Integrated {
        /// One line per entry recorded in the manifest.
        items: Vec<String>,
    },
    /// Nothing is registered (or the manifest is unreadable).
    NotIntegrated,
    /// Integration cannot be managed here; the text says why. This is also
    /// reported when the data folder holds an integration made on another
    /// computer or account (a moved portable copy).
    Unsupported(String),
}

/// Current integration state, from the manifest in the data folder.
pub fn state(dd: &DataDir) -> IntegrationState {
    state_for(Platform::current(), dd.root.as_deref(), &Env::real())
}

/// Registers OxTail (running as `exe`, an absolute path) with the system.
/// Returns a human-readable list of what was done (and of what was refused or
/// skipped). Repeating it is safe and updates the entries (for example after
/// the executable moved).
///
/// # Errors
/// [`ConfigError::NotPersistent`] for an in-memory data folder (there would
/// be nowhere to record what was done), [`ConfigError::Integration`] for an
/// unsupported platform, a relative `exe` or an unreadable existing manifest,
/// and I/O or registry errors. On a partial failure the entries already made
/// stay in the manifest, so [`remove`] still cleans them up.
pub fn integrate(dd: &DataDir, exe: &Path) -> Result<Vec<String>, ConfigError> {
    integrate_with(Platform::current(), dd, exe, &Env::real(), true)
}

/// Undoes [`integrate`], using only the manifest and only for entries that
/// OxTail could have created. Does nothing (and returns an empty list) if
/// nothing is integrated.
///
/// # Errors
/// An unreadable manifest, a manifest made on another computer or account,
/// or a failure to delete an entry; entries that could not be removed stay in
/// the manifest for a retry.
pub fn remove(dd: &DataDir) -> Result<Vec<String>, ConfigError> {
    remove_with(Platform::current(), dd, &Env::real(), true)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Platform {
    Windows,
    MacOs,
    Unix,
}

impl Platform {
    fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Unix
        }
    }
}

/// Environment the plan depends on, injectable for tests.
#[derive(Clone, Debug)]
struct Env {
    /// `$XDG_DATA_HOME` (Unix) or `%APPDATA%` (Windows).
    data_home: Option<PathBuf>,
    /// Identifies this computer and account.
    fingerprint: String,
}

impl Env {
    fn real() -> Self {
        Self {
            data_home: directories::BaseDirs::new().map(|b| b.data_dir().to_path_buf()),
            fingerprint: machine_fingerprint(),
        }
    }
}

/// Host name, user name and home folder, from the environment (no extra deps).
fn machine_fingerprint() -> String {
    let var = |names: &[&str]| {
        names
            .iter()
            .find_map(|n| std::env::var(n).ok().filter(|v| !v.is_empty()))
    };
    let host = var(&["HOSTNAME", "COMPUTERNAME"])
        .or_else(|| {
            ["/proc/sys/kernel/hostname", "/etc/hostname"]
                .iter()
                .find_map(|p| std::fs::read_to_string(p).ok())
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default();
    let user = var(&["USER", "USERNAME", "LOGNAME"]).unwrap_or_default();
    let home = directories::BaseDirs::new()
        .map(|b| b.home_dir().display().to_string())
        .unwrap_or_default();
    format!("host={host};user={user};home={home}")
}

fn new_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{nanos:x}-{:x}", std::process::id())
}

/// One thing to create.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Change {
    /// Write a file (creating parent folders).
    File { path: PathBuf, contents: Vec<u8> },
    /// A Windows `.lnk` shortcut.
    Shortcut { path: PathBuf, target: PathBuf },
    /// Registry values under an HKCU subtree; keys are created as needed and
    /// only the ones that did not exist are recorded as ours.
    RegTree { root: String, values: Vec<RegValue> },
    /// One value under a registry key OxTail may not own.
    RegSingle {
        key: String,
        name: String,
        value: String,
    },
    /// Not done, with the reason (reported, never recorded).
    Skip(String),
}

/// A value inside a [`Change::RegTree`].
#[derive(Clone, Debug, PartialEq, Eq)]
struct RegValue {
    /// Sub-key relative to the tree root ("" for the root itself).
    sub: String,
    /// Value name ("" is the default value).
    name: String,
    value: String,
}

/// What the manifest records; enough to undo without knowing the exe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Item {
    /// A file (or shortcut) that was written.
    File { path: PathBuf },
    /// An HKCU subkey that OxTail created (removed only if empty).
    RegKey { key: String },
    /// A value OxTail set in an HKCU key.
    RegValue { key: String, name: String },
}

impl Item {
    fn describe(&self) -> String {
        match self {
            Item::File { path } => format!("file {}", path.display()),
            Item::RegKey { key } => format!("registry key HKCU\\{key}"),
            Item::RegValue { key, name } => {
                format!("registry value HKCU\\{key} [{name}]")
            }
        }
    }
}

/// The manifest file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    /// Identifies this integration; also written into the `.desktop` file.
    #[serde(default)]
    id: String,
    /// Computer and account that made it.
    #[serde(default)]
    fingerprint: String,
    /// Executable the entries point at (informational).
    exe: Option<PathBuf>,
    items: Vec<Item>,
}

fn manifest_path(root: &Path) -> PathBuf {
    root.join(MANIFEST_FILE)
}

/// `Ok(None)` if there is no manifest.
fn read_manifest(root: &Path) -> Result<Option<Manifest>, ConfigError> {
    let path = manifest_path(root);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ConfigError::io(path, e)),
    };
    let m: Manifest = serde_json::from_str(&text)?;
    if m.version != 1 {
        return Err(ConfigError::Integration(format!(
            "{}: unknown manifest version {}",
            path.display(),
            m.version
        )));
    }
    Ok(Some(m))
}

fn write_manifest(root: &Path, m: &Manifest) -> Result<(), ConfigError> {
    let path = manifest_path(root);
    let json = serde_json::to_vec_pretty(m)?;
    write_atomic(&path, &json).map_err(|e| ConfigError::io(path, e))
}

fn foreign_message() -> String {
    format!(
        "the integration recorded in this data folder was made on another computer or \
         account, so OxTail will not touch this one. Delete {MANIFEST_FILE} to start over \
         here (the other computer keeps its entries until removed there)"
    )
}

fn state_for(platform: Platform, root: Option<&Path>, env: &Env) -> IntegrationState {
    if platform == Platform::MacOs {
        return IntegrationState::Unsupported(MACOS_MESSAGE.into());
    }
    let Some(root) = root else {
        return IntegrationState::NotIntegrated;
    };
    match read_manifest(root) {
        Ok(Some(m)) if m.items.is_empty() => IntegrationState::NotIntegrated,
        Ok(Some(m)) if m.fingerprint != env.fingerprint => {
            IntegrationState::Unsupported(foreign_message())
        }
        Ok(Some(m)) => IntegrationState::Integrated {
            items: m.items.iter().map(Item::describe).collect(),
        },
        Ok(None) => IntegrationState::NotIntegrated,
        Err(e) => {
            tracing::warn!("ignoring the integration manifest: {e}");
            IntegrationState::NotIntegrated
        }
    }
}

// ------------------------------------------------------ what is ours ---

fn split_key(key: &str) -> Option<Vec<&str>> {
    let parts: Vec<&str> = key.split('\\').collect();
    parts
        .iter()
        .all(|p| !p.is_empty() && *p != "." && *p != "..")
        .then_some(parts)
}

/// `key` equals `base` or lies below it (registry names are case-insensitive).
fn key_within(key: &str, base: &str) -> bool {
    let (k, b) = (key.to_ascii_lowercase(), base.to_ascii_lowercase());
    k == b || (k.starts_with(&b) && k[b.len()..].starts_with('\\'))
}

fn is_openwith_key(key: &str) -> bool {
    WINDOWS_EXTENSIONS.iter().any(|e| {
        let k = key.to_ascii_lowercase();
        k == format!("{CLASSES_KEY}\\{e}\\openwithprogids").to_ascii_lowercase()
    })
}

/// Registry keys OxTail may have created (and so may remove when empty).
fn reg_key_allowed(key: &str) -> bool {
    if split_key(key).is_none() {
        return false;
    }
    let l = key.to_ascii_lowercase();
    key_within(&l, APP_KEY)
        || key_within(&l, PROG_KEY)
        || l == APPS_KEY.to_ascii_lowercase()
        || is_openwith_key(key)
        || WINDOWS_EXTENSIONS
            .iter()
            .any(|e| l == format!("{CLASSES_KEY}\\{e}").to_ascii_lowercase())
}

/// Registry values OxTail may have set.
fn reg_value_allowed(key: &str, name: &str) -> bool {
    if split_key(key).is_none() {
        return false;
    }
    key_within(key, APP_KEY)
        || key_within(key, PROG_KEY)
        || (is_openwith_key(key) && name == PROG_ID)
}

/// The (folder, file name) pairs [`integrate`] can write.
fn allowed_files(platform: Platform, env: &Env) -> Vec<(PathBuf, String)> {
    let Some(home) = env.data_home.as_deref() else {
        return Vec::new();
    };
    match platform {
        Platform::Unix => vec![
            (home.join("applications"), format!("{APP_ID}.desktop")),
            (
                home.join("icons/hicolor/256x256/apps"),
                format!("{APP_ID}.png"),
            ),
        ],
        Platform::Windows => vec![(
            home.join("Microsoft/Windows/Start Menu/Programs"),
            LNK_NAME.to_string(),
        )],
        Platform::MacOs => Vec::new(),
    }
}

fn file_is_ours(platform: Platform, env: &Env, path: &Path) -> bool {
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return false;
    };
    let name = name.to_string_lossy();
    for (dir, allowed) in allowed_files(platform, env) {
        let same_name = if platform == Platform::Windows {
            name.eq_ignore_ascii_case(&allowed)
        } else {
            name == allowed.as_str()
        };
        if parent != dir || !same_name {
            continue;
        }
        // A symlinked parent folder must still lead to the expected folder.
        let (Some(home), Ok(rel)) = (
            env.data_home.as_deref(),
            dir.strip_prefix(env.data_home.as_deref().unwrap_or(Path::new(""))),
        ) else {
            return false;
        };
        return match (std::fs::canonicalize(parent), std::fs::canonicalize(home)) {
            (Ok(p), Ok(h)) => p == h.join(rel),
            (Err(_), _) => true, // the folder is gone; nothing to delete
            _ => false,
        };
    }
    false
}

/// Whether OxTail's [`integrate`] could have created `item`. Everything else
/// in a manifest is ignored.
fn is_ours(platform: Platform, item: &Item, env: &Env) -> bool {
    match (platform, item) {
        (Platform::MacOs, _) => false,
        (_, Item::File { path }) => file_is_ours(platform, env, path),
        (Platform::Windows, Item::RegKey { key }) => reg_key_allowed(key),
        (Platform::Windows, Item::RegValue { key, name }) => reg_value_allowed(key, name),
        (Platform::Unix, _) => false,
    }
}

// ------------------------------------------------------------- planning ---

/// Escapes one `Exec=` argument: quoted, with the reserved characters
/// escaped, then backslashes doubled for the desktop-entry string level.
fn exec_quote(arg: &str) -> String {
    let mut quoted = String::from("\"");
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            '%' => quoted.push_str("%%"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted.replace('\\', "\\\\")
}

fn desktop_file(exe: &Path, id: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=OxTail\n\
         GenericName=Log Viewer\n\
         Comment=Fast, portable, real-time log viewer\n\
         Exec={} %F\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=Utility;TextTools;\n\
         Keywords=log;tail;logs;viewer;follow;\n\
         MimeType=text/plain;text/x-log;\n\
         {MARKER_KEY}{id}\n",
        exec_quote(&exe.to_string_lossy())
    )
}

/// `true` for `X:\...` and `\\?\X:\...` drive paths in valid UTF-8 (what the
/// shortcut writer handles); UNC paths and other prefixes are refused.
fn shortcut_path_ok(p: &Path) -> bool {
    let Some(s) = p.to_str() else {
        return false;
    };
    let s = s.strip_prefix("\\\\?\\").unwrap_or(s);
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

/// The changes to make. Pure: no I/O.
fn plan(platform: Platform, exe: &Path, env: &Env, id: &str) -> Result<Vec<Change>, ConfigError> {
    if !exe.is_absolute() {
        return Err(ConfigError::Integration(format!(
            "the executable path must be absolute: {}",
            exe.display()
        )));
    }
    if exe.to_string_lossy().contains(['\n', '\r']) {
        return Err(ConfigError::Integration(
            "the executable path contains a line break and cannot be registered".into(),
        ));
    }
    let Some(data_home) = env.data_home.as_deref() else {
        return Err(ConfigError::Integration(
            "cannot find the per-user application data folder".into(),
        ));
    };
    match platform {
        Platform::MacOs => Err(ConfigError::Integration(MACOS_MESSAGE.into())),
        Platform::Unix => {
            let files = allowed_files(platform, env);
            let contents = [desktop_file(exe, id).into_bytes(), ICON_256.to_vec()];
            Ok(files
                .into_iter()
                .zip(contents)
                .map(|((dir, name), contents)| Change::File {
                    path: dir.join(name),
                    contents,
                })
                .collect())
        }
        Platform::Windows => {
            let exe_s = exe.to_string_lossy();
            let command = format!("\"{exe_s}\" \"%1\"");
            let rv = |sub: &str, name: &str, value: &str| RegValue {
                sub: sub.into(),
                name: name.into(),
                value: value.into(),
            };
            let mut app = vec![
                rv("", "FriendlyAppName", "OxTail"),
                rv("shell\\open", "FriendlyAppName", "OxTail"),
                rv("shell\\open\\command", "", &command),
            ];
            app.extend(
                WINDOWS_EXTENSIONS
                    .iter()
                    .map(|e| rv("SupportedTypes", e, "")),
            );
            let mut changes = vec![
                Change::RegTree {
                    root: APP_KEY.into(),
                    values: app,
                },
                Change::RegTree {
                    root: PROG_KEY.into(),
                    values: vec![
                        rv("", "", "Log file (OxTail)"),
                        rv("DefaultIcon", "", &format!("\"{exe_s}\",0")),
                        rv("shell\\open\\command", "", &command),
                    ],
                },
            ];
            changes.extend(WINDOWS_EXTENSIONS.iter().map(|e| Change::RegSingle {
                key: format!("{CLASSES_KEY}\\{e}\\OpenWithProgids"),
                name: PROG_ID.into(),
                value: String::new(),
            }));
            let lnk = data_home
                .join("Microsoft/Windows/Start Menu/Programs")
                .join(LNK_NAME);
            changes.push(if shortcut_path_ok(exe) && shortcut_path_ok(&lnk) {
                Change::Shortcut {
                    path: lnk,
                    target: exe.to_path_buf(),
                }
            } else {
                Change::Skip(
                    "no Start menu shortcut: the path is not a plain drive path in UTF-8".into(),
                )
            });
            Ok(changes)
        }
    }
}

// ------------------------------------------------------------ applying ---

fn read_capped(path: &Path) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_READ)
        .read_to_end(&mut buf)
        .ok()?;
    Some(buf)
}

fn has_marker(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(bytes)
        .lines()
        .any(|l| l.starts_with(MARKER_KEY))
}

/// `Some(reason)` if `change` would overwrite a file that is not ours.
fn overwrite_refusal(change: &Change, old: &[Item]) -> Option<String> {
    let (Change::File { path, .. } | Change::Shortcut { path, .. }) = change else {
        return None;
    };
    if path.symlink_metadata().is_err() {
        return None; // nothing there
    }
    let recorded = old.contains(&Item::File { path: path.clone() });
    let ours = recorded
        || match change {
            Change::File { contents, .. } => read_capped(path)
                .is_some_and(|e| &e == contents || (has_marker(&e) && has_marker(contents))),
            _ => false,
        };
    (!ours).then(|| {
        format!(
            "refusing to overwrite {}: it already exists and was not created by OxTail",
            path.display()
        )
    })
}

fn item_of(change: &Change) -> Option<Item> {
    match change {
        Change::File { path, .. } | Change::Shortcut { path, .. } => {
            Some(Item::File { path: path.clone() })
        }
        Change::RegSingle { key, name, .. } => Some(Item::RegValue {
            key: key.clone(),
            name: name.clone(),
        }),
        Change::RegTree { .. } | Change::Skip(_) => None,
    }
}

fn integrate_with(
    platform: Platform,
    dd: &DataDir,
    exe: &Path,
    env: &Env,
    hooks: bool,
) -> Result<Vec<String>, ConfigError> {
    if platform == Platform::MacOs {
        return Err(ConfigError::Integration(MACOS_MESSAGE.into()));
    }
    let Some(root) = dd.root.as_deref() else {
        return Err(ConfigError::NotPersistent(
            "there is no data folder to record the integration in".into(),
        ));
    };
    let mut messages = Vec::new();
    let mut manifest = match read_manifest(root) {
        Ok(Some(m)) if m.fingerprint == env.fingerprint => m,
        Ok(Some(_)) => {
            messages.push(
                "ignored entries recorded by another computer or account (they are not touched)"
                    .to_string(),
            );
            Manifest::default()
        }
        Ok(None) => Manifest::default(),
        Err(e) => {
            return Err(ConfigError::Integration(format!(
                "the existing integration manifest is unreadable ({e}); fix or delete {} first",
                manifest_path(root).display()
            )));
        }
    };
    // Carry forward only entries this program could have written.
    let carried = std::mem::take(&mut manifest.items);
    for item in carried {
        if is_ours(platform, &item, env) {
            manifest.items.push(item);
        } else {
            messages.push(format!(
                "dropped {} from the manifest: not created by OxTail",
                item.describe()
            ));
        }
    }
    if manifest.id.is_empty() {
        manifest.id = new_id();
    }
    manifest.version = 1;
    manifest.fingerprint = env.fingerprint.clone();
    manifest.exe = Some(exe.to_path_buf());

    let changes = plan(platform, exe, env, &manifest.id)?;
    let old = manifest.items.clone();
    let mut result = Ok(());
    for change in &changes {
        if let Change::Skip(reason) = change {
            messages.push(format!("skipped: {reason}"));
            continue;
        }
        if let Some(reason) = overwrite_refusal(change, &old) {
            messages.push(reason);
            continue;
        }
        let mut made = Vec::new();
        // Record before applying: a half-made file is still cleaned up.
        if let Some(item) = item_of(change) {
            made.push(item);
        }
        let outcome = apply(change, &mut made);
        for item in made {
            if !manifest.items.contains(&item) {
                messages.push(format!("created {}", item.describe()));
                manifest.items.push(item);
            }
        }
        if let Err(e) = outcome {
            result = Err(e);
            break;
        }
    }
    write_manifest(root, &manifest)?;
    result?;
    if hooks && platform == Platform::Unix {
        run_desktop_database(env);
    }
    if platform == Platform::Windows {
        messages.push("Explorer may need a moment to show the new entries".into());
    }
    Ok(messages)
}

/// Applies one change. Registry items that were created are pushed to `made`
/// as they happen, so a failure part-way still leaves an accurate record.
fn apply(change: &Change, made: &mut Vec<Item>) -> Result<(), ConfigError> {
    match change {
        Change::File { path, contents } => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| ConfigError::io(parent, e))?;
            }
            write_atomic(path, contents).map_err(|e| ConfigError::io(path, e))
        }
        Change::Shortcut { path, target } => sys::write_shortcut(path, target),
        Change::RegTree { root, values } => sys::reg_write_tree(root, values, made),
        Change::RegSingle { key, name, value } => sys::reg_set(key, name, value, made),
        Change::Skip(_) => Ok(()),
    }
}

enum Undo {
    Done,
    Refused(String),
}

fn remove_with(
    platform: Platform,
    dd: &DataDir,
    env: &Env,
    hooks: bool,
) -> Result<Vec<String>, ConfigError> {
    let Some(root) = dd.root.as_deref() else {
        return Ok(Vec::new());
    };
    let Some(mut manifest) = read_manifest(root)? else {
        return Ok(Vec::new());
    };
    if manifest.fingerprint != env.fingerprint {
        return Err(ConfigError::Integration(foreign_message()));
    }
    let mut messages = Vec::new();
    let mut failed = Vec::new();
    let mut first_error = None;
    // Newest first: values before the keys that hold them.
    for item in std::mem::take(&mut manifest.items).into_iter().rev() {
        if !is_ours(platform, &item, env) {
            messages.push(format!(
                "refusing to remove {}: not created by OxTail",
                item.describe()
            ));
            continue;
        }
        match undo(&item) {
            Ok(Undo::Done) => messages.push(format!("removed {}", item.describe())),
            Ok(Undo::Refused(why)) => messages.push(why),
            Err(e) => {
                first_error.get_or_insert(e);
                failed.push(item);
            }
        }
    }
    if failed.is_empty() {
        let path = manifest_path(root);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(ConfigError::io(path, e)),
        }
    } else {
        failed.reverse();
        manifest.items = failed;
        write_manifest(root, &manifest)?;
    }
    if hooks && platform == Platform::Unix {
        run_desktop_database(env);
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(messages),
    }
}

fn undo(item: &Item) -> Result<Undo, ConfigError> {
    match item {
        Item::File { path } => {
            if path.extension().is_some_and(|e| e == "desktop") {
                match read_capped(path) {
                    Some(b) if has_marker(&b) => {}
                    Some(_) => {
                        return Ok(Undo::Refused(format!(
                            "refusing to remove {}: it does not carry OxTail's marker",
                            path.display()
                        )));
                    }
                    None => return Ok(Undo::Done), // gone or unreadable
                }
            }
            match std::fs::remove_file(path) {
                Ok(()) => Ok(Undo::Done),
                // Gone already (or a parent is no longer a directory).
                Err(_) if path.symlink_metadata().is_err() => Ok(Undo::Done),
                Err(e) => Err(ConfigError::io(path, e)),
            }
        }
        Item::RegKey { key } => sys::reg_delete_key_if_empty(key).map(|()| Undo::Done),
        Item::RegValue { key, name } => sys::reg_delete_value(key, name).map(|()| Undo::Done),
    }
}

/// Runs `update-desktop-database` on the applications folder if it exists,
/// for at most [`HOOK_TIMEOUT`]. Failure (including the tool being absent)
/// is ignored.
fn run_desktop_database(env: &Env) {
    let Some(dir) = env.data_home.as_ref().map(|d| d.join("applications")) else {
        return;
    };
    if !dir.is_dir() {
        return;
    }
    let Ok(mut child) = std::process::Command::new("update-desktop-database")
        .arg(&dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return;
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if start.elapsed() < HOOK_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

// ------------------------------------------------ platform primitives ---

#[cfg(windows)]
mod sys {
    use std::{io, path::Path};

    use winreg::{RegKey, enums::*};

    use super::{CLASSES_KEY, ConfigError, Item, RegValue};

    fn reg_err(what: &str, e: io::Error) -> ConfigError {
        ConfigError::Integration(format!("registry {what}: {e}"))
    }

    pub(super) fn write_shortcut(path: &Path, target: &Path) -> Result<(), ConfigError> {
        // `plan` only produces shortcuts for UTF-8 drive paths, which is what
        // `mslnk` can handle without panicking; check again to be sure.
        if !super::shortcut_path_ok(target) || !super::shortcut_path_ok(path) {
            return Err(ConfigError::Integration(
                "shortcut paths must be UTF-8 drive paths".into(),
            ));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ConfigError::io(parent, e))?;
        }
        let link = mslnk::ShellLink::new(target)
            .map_err(|e| ConfigError::Integration(format!("shortcut: {e}")))?;
        link.create_lnk(path)
            .map_err(|e| ConfigError::Integration(format!("shortcut {}: {e}", path.display())))
    }

    /// Creates every key of `path` below `Software\Classes` one by one and
    /// records those that did not exist before.
    fn ensure_key(path: &str, made: &mut Vec<Item>) -> Result<RegKey, ConfigError> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let mut cur = CLASSES_KEY.to_string();
        let rest = path.strip_prefix(CLASSES_KEY).unwrap_or(path);
        let mut last = None;
        for part in rest.split('\\').filter(|p| !p.is_empty()) {
            cur = format!("{cur}\\{part}");
            let (key, disp) = hkcu
                .create_subkey(&cur)
                .map_err(|e| reg_err(&format!("create {cur}"), e))?;
            if disp == RegDisposition::REG_CREATED_NEW_KEY {
                made.push(Item::RegKey { key: cur.clone() });
            }
            last = Some(key);
        }
        match last {
            Some(k) => Ok(k),
            None => hkcu
                .open_subkey_with_flags(CLASSES_KEY, KEY_SET_VALUE)
                .map_err(|e| reg_err("open Classes", e)),
        }
    }

    pub(super) fn reg_write_tree(
        root: &str,
        values: &[RegValue],
        made: &mut Vec<Item>,
    ) -> Result<(), ConfigError> {
        for v in values {
            let path = if v.sub.is_empty() {
                root.to_string()
            } else {
                format!("{root}\\{}", v.sub)
            };
            let key = ensure_key(&path, made)?;
            key.set_value(&v.name, &v.value)
                .map_err(|e| reg_err(&format!("set {path}"), e))?;
            made.push(Item::RegValue {
                key: path,
                name: v.name.clone(),
            });
        }
        Ok(())
    }

    pub(super) fn reg_set(
        key: &str,
        name: &str,
        value: &str,
        made: &mut Vec<Item>,
    ) -> Result<(), ConfigError> {
        let k = ensure_key(key, made)?;
        k.set_value(name, &value)
            .map_err(|e| reg_err(&format!("set {key}"), e))?;
        made.push(Item::RegValue {
            key: key.to_string(),
            name: name.to_string(),
        });
        Ok(())
    }

    pub(super) fn reg_delete_key_if_empty(key: &str) -> Result<(), ConfigError> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let k = match hkcu.open_subkey(key) {
            Ok(k) => k,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(reg_err(&format!("open {key}"), e)),
        };
        let empty = k.enum_keys().next().is_none() && k.enum_values().next().is_none();
        drop(k);
        if !empty {
            return Ok(()); // still used by something else: leave it
        }
        match hkcu.delete_subkey(key) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(reg_err(&format!("delete {key}"), e)),
        }
    }

    pub(super) fn reg_delete_value(key: &str, name: &str) -> Result<(), ConfigError> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let k = match hkcu.open_subkey_with_flags(key, KEY_SET_VALUE) {
            Ok(k) => k,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(reg_err(&format!("open {key}"), e)),
        };
        match k.delete_value(name) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(reg_err(&format!("delete value in {key}"), e)),
        }
    }
}

#[cfg(not(windows))]
mod sys {
    use std::path::Path;

    use super::{ConfigError, Item, RegValue};

    fn windows_only() -> ConfigError {
        ConfigError::Integration("registry entries exist only on Windows".into())
    }

    pub(super) fn write_shortcut(_: &Path, _: &Path) -> Result<(), ConfigError> {
        Err(windows_only())
    }
    pub(super) fn reg_write_tree(
        _: &str,
        _: &[RegValue],
        _: &mut Vec<Item>,
    ) -> Result<(), ConfigError> {
        Err(windows_only())
    }
    pub(super) fn reg_set(_: &str, _: &str, _: &str, _: &mut Vec<Item>) -> Result<(), ConfigError> {
        Err(windows_only())
    }
    pub(super) fn reg_delete_key_if_empty(_: &str) -> Result<(), ConfigError> {
        Err(windows_only())
    }
    pub(super) fn reg_delete_value(_: &str, _: &str) -> Result<(), ConfigError> {
        Err(windows_only())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DataMode;

    const FP: &str = "host=h;user=u;home=/h";

    fn dd(root: &Path) -> DataDir {
        DataDir {
            root: Some(root.to_path_buf()),
            mode: DataMode::Cli,
        }
    }

    fn env(home: &Path) -> Env {
        Env {
            data_home: Some(home.to_path_buf()),
            fingerprint: FP.into(),
        }
    }

    fn desktop_path(home: &Path) -> PathBuf {
        home.join("applications/io.github.patricksindelka.OxTail.desktop")
    }

    fn icon_path(home: &Path) -> PathBuf {
        home.join("icons/hicolor/256x256/apps/io.github.patricksindelka.OxTail.png")
    }

    fn integrate_unix(d: &DataDir, home: &Path, exe: &str) -> Vec<String> {
        integrate_with(Platform::Unix, d, Path::new(exe), &env(home), false).unwrap()
    }

    #[test]
    fn unix_plan_writes_desktop_file_and_icon() {
        let exe = Path::new("/opt/ox tail/oxtail");
        let plan = plan(
            Platform::Unix,
            exe,
            &env(Path::new("/h/.local/share")),
            "ID1",
        )
        .unwrap();
        assert_eq!(plan.len(), 2);
        let Change::File { path, contents } = &plan[0] else {
            panic!("not a file")
        };
        assert!(path.ends_with("applications/io.github.patricksindelka.OxTail.desktop"));
        let text = String::from_utf8(contents.clone()).unwrap();
        assert!(text.contains("Exec=\"/opt/ox tail/oxtail\" %F\n"));
        assert!(text.contains("MimeType=text/plain;text/x-log;\n"));
        assert!(text.contains("Icon=io.github.patricksindelka.OxTail\n"));
        assert!(text.contains("X-OxTail-Integration=ID1\n"));
        let Change::File { path, contents } = &plan[1] else {
            panic!("not a file")
        };
        assert!(path.ends_with("icons/hicolor/256x256/apps/io.github.patricksindelka.OxTail.png"));
        assert_eq!(&contents[1..4], b"PNG");
    }

    #[test]
    fn exec_quoting_escapes_reserved_characters() {
        assert_eq!(exec_quote("/a b/c"), "\"/a b/c\"");
        assert_eq!(exec_quote("/a$b"), "\"/a\\\\$b\"");
        assert_eq!(exec_quote("/100%"), "\"/100%%\"");
        assert_eq!(exec_quote("/q\"x"), "\"/q\\\\\"x\"");
    }

    #[test]
    fn windows_plan_adds_open_with_but_no_default() {
        let exe = PathBuf::from("/c/Tools/oxtail.exe");
        let home = Path::new("/c/Users/u/AppData/Roaming");
        let changes = plan(Platform::Windows, &exe, &env(home), "ID").unwrap();
        let exe_s = exe.to_string_lossy();
        let (mut trees, mut progids) = (0, 0);
        for c in &changes {
            match c {
                Change::RegTree { root, values } => {
                    trees += 1;
                    assert!(reg_key_allowed(root));
                    assert!(
                        values
                            .iter()
                            .any(|v| v.sub.ends_with("shell\\open\\command")
                                && v.value == format!("\"{exe_s}\" \"%1\""))
                    );
                }
                Change::RegSingle { key, name, .. } => {
                    assert!(reg_value_allowed(key, name), "{key}");
                    progids += 1;
                }
                // Unix-style test paths are not drive paths, so it is skipped.
                Change::Skip(_) => {}
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!((trees, progids), (2, WINDOWS_EXTENSIONS.len()));
        assert!(!changes.iter().any(|c| matches!(
            c,
            Change::RegSingle { name, .. } if name.is_empty()
        )));
    }

    #[test]
    fn shortcut_path_table() {
        for ok in [
            "C:\\Tools\\oxtail.exe",
            "d:/x/oxtail.exe",
            "\\\\?\\C:\\a\\b.exe",
        ] {
            assert!(shortcut_path_ok(Path::new(ok)), "{ok}");
        }
        for bad in [
            "\\\\server\\share\\oxtail.exe",
            "\\\\?\\UNC\\server\\share\\a.exe",
            "/usr/bin/oxtail",
            "C:oxtail.exe",
            "",
        ] {
            assert!(!shortcut_path_ok(Path::new(bad)), "{bad}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let p = Path::new(std::ffi::OsStr::from_bytes(b"C:\\a\xff.exe"));
            assert!(!shortcut_path_ok(p));
        }
    }

    #[test]
    fn windows_plan_uses_a_shortcut_for_drive_paths() {
        let changes = plan(
            Platform::Windows,
            Path::new("C:\\Tools\\oxtail.exe"),
            &env(Path::new("C:\\Users\\u\\AppData\\Roaming")),
            "ID",
        );
        // `is_absolute` is false for drive paths on Unix hosts; only check on Windows.
        if cfg!(windows) {
            assert!(
                changes
                    .unwrap()
                    .iter()
                    .any(|c| matches!(c, Change::Shortcut { .. }))
            );
        }
    }

    #[test]
    fn plan_rejects_relative_exe_line_breaks_and_macos() {
        let e = env(Path::new("/h"));
        assert!(plan(Platform::Unix, Path::new("oxtail"), &e, "i").is_err());
        assert!(plan(Platform::MacOs, Path::new("/x/oxtail"), &e, "i").is_err());
        assert!(plan(Platform::Unix, Path::new("/x/ox\ntail"), &e, "i").is_err());
        let none = Env {
            data_home: None,
            fingerprint: FP.into(),
        };
        assert!(plan(Platform::Unix, Path::new("/x/oxtail"), &none, "i").is_err());
    }

    #[test]
    fn macos_is_unsupported() {
        let t = tempfile::tempdir().unwrap();
        assert!(matches!(
            state_for(Platform::MacOs, Some(t.path()), &env(t.path())),
            IntegrationState::Unsupported(m) if m.contains("Finder")
        ));
    }

    #[test]
    fn manifest_round_trips() {
        let t = tempfile::tempdir().unwrap();
        let m = Manifest {
            version: 1,
            id: "abc".into(),
            fingerprint: FP.into(),
            exe: Some("/x/oxtail".into()),
            items: vec![
                Item::File { path: "/a".into() },
                Item::RegKey { key: "K".into() },
                Item::RegValue {
                    key: "K2".into(),
                    name: "N".into(),
                },
            ],
        };
        write_manifest(t.path(), &m).unwrap();
        assert_eq!(read_manifest(t.path()).unwrap(), Some(m));
    }

    #[test]
    fn integrate_then_remove_on_unix() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let d = dd(data.path());
        let e = env(home.path());
        assert_eq!(
            state_for(Platform::Unix, d.root.as_deref(), &e),
            IntegrationState::NotIntegrated
        );
        let done = integrate_unix(&d, home.path(), "/opt/oxtail");
        assert!(done.len() >= 2);
        assert!(desktop_path(home.path()).is_file() && icon_path(home.path()).is_file());
        assert!(matches!(
            state_for(Platform::Unix, d.root.as_deref(), &e),
            IntegrationState::Integrated { items } if items.len() == 2
        ));
        let other = home.path().join("applications/other.desktop");
        std::fs::write(&other, "x").unwrap();

        let removed = remove_with(Platform::Unix, &d, &e, false).unwrap();
        assert_eq!(removed.len(), 2);
        assert!(!desktop_path(home.path()).exists() && !icon_path(home.path()).exists());
        assert!(other.exists());
        assert!(!manifest_path(data.path()).exists());
        assert_eq!(
            state_for(Platform::Unix, d.root.as_deref(), &e),
            IntegrationState::NotIntegrated
        );
    }

    #[test]
    fn remove_works_after_the_exe_moved_and_reintegrate_does_not_duplicate() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let d = dd(data.path());
        integrate_unix(&d, home.path(), "/old/oxtail");
        integrate_unix(&d, home.path(), "/new/oxtail");
        let m = read_manifest(data.path()).unwrap().unwrap();
        assert_eq!(m.items.len(), 2);
        assert_eq!(m.exe, Some(PathBuf::from("/new/oxtail")));
        let text = std::fs::read_to_string(desktop_path(home.path())).unwrap();
        assert!(text.contains("/new/oxtail") && text.contains(&m.id));
        assert_eq!(
            remove_with(Platform::Unix, &d, &env(home.path()), false)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn remove_with_nothing_integrated_is_a_no_op() {
        let data = tempfile::tempdir().unwrap();
        let e = env(data.path());
        assert!(
            remove_with(Platform::Unix, &dd(data.path()), &e, false)
                .unwrap()
                .is_empty()
        );
        assert!(
            remove_with(Platform::Unix, &DataDir::in_memory("ro"), &e, false)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn already_deleted_files_are_not_an_error() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let d = dd(data.path());
        integrate_unix(&d, home.path(), "/o/oxtail");
        std::fs::remove_dir_all(home.path().join("applications")).unwrap();
        assert!(remove_with(Platform::Unix, &d, &env(home.path()), false).is_ok());
    }

    #[test]
    fn corrupt_manifest_does_not_panic() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let d = dd(data.path());
        let e = env(home.path());
        for junk in [
            "",
            "{",
            "[1,2]",
            "\u{0}\u{1}",
            "{\"version\":9,\"items\":[]}",
        ] {
            std::fs::write(manifest_path(data.path()), junk).unwrap();
            assert_eq!(
                state_for(Platform::Unix, d.root.as_deref(), &e),
                IntegrationState::NotIntegrated
            );
            assert!(remove_with(Platform::Unix, &d, &e, false).is_err());
            assert!(integrate_with(Platform::Unix, &d, Path::new("/o/oxtail"), &e, false).is_err());
            assert!(!home.path().join("applications").exists());
        }
    }

    #[test]
    fn in_memory_data_folder_cannot_integrate() {
        let home = tempfile::tempdir().unwrap();
        let r = integrate_with(
            Platform::Unix,
            &DataDir::in_memory("ro"),
            Path::new("/o/oxtail"),
            &env(home.path()),
            false,
        );
        assert!(matches!(r, Err(ConfigError::NotPersistent(_))));
    }

    #[test]
    fn partial_failure_is_recorded_for_removal() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        // A file where the icons directory must go makes the second change fail.
        std::fs::write(home.path().join("icons"), "in the way").unwrap();
        let d = dd(data.path());
        let e = env(home.path());
        let r = integrate_with(Platform::Unix, &d, Path::new("/o/oxtail"), &e, false);
        assert!(r.is_err());
        assert!(desktop_path(home.path()).is_file());
        assert!(remove_with(Platform::Unix, &d, &e, false).is_ok());
        assert!(!desktop_path(home.path()).exists());
    }

    #[test]
    fn tampered_manifest_cannot_delete_anything_else() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let d = dd(data.path());
        let e = env(home.path());
        integrate_unix(&d, home.path(), "/o/oxtail");
        let bashrc = home.path().join(".bashrc");
        std::fs::write(&bashrc, "precious").unwrap();
        let victim = data.path().join("victim.txt");
        std::fs::write(&victim, "precious").unwrap();
        // ../ traversal out of the allowed folder, a right-named file in a
        // wrong folder, a wrong name in the right folder, and registry keys.
        let sneaky = home
            .path()
            .join("applications/../../victim.txt")
            .to_path_buf();
        let wrong_dir = home.path().join("io.github.patricksindelka.OxTail.desktop");
        std::fs::write(&wrong_dir, "keep").unwrap();
        let wrong_name = home.path().join("applications/other.desktop");
        std::fs::write(&wrong_name, "keep").unwrap();
        let mut m = read_manifest(data.path()).unwrap().unwrap();
        m.items.extend([
            Item::File {
                path: bashrc.clone(),
            },
            Item::File { path: sneaky },
            Item::File {
                path: wrong_dir.clone(),
            },
            Item::File {
                path: wrong_name.clone(),
            },
            Item::RegKey {
                key: "Software".into(),
            },
            Item::RegKey {
                key: "Software\\Classes".into(),
            },
            Item::RegValue {
                key: "Software\\Microsoft".into(),
                name: "X".into(),
            },
        ]);
        write_manifest(data.path(), &m).unwrap();

        let msgs = remove_with(Platform::Unix, &d, &e, false).unwrap();
        assert_eq!(
            msgs.iter().filter(|m| m.starts_with("refusing")).count(),
            7,
            "{msgs:?}"
        );
        assert!(bashrc.exists() && victim.exists() && wrong_dir.exists() && wrong_name.exists());
        assert!(!desktop_path(home.path()).exists());
        assert!(!manifest_path(data.path()).exists());

        // integrate() drops such entries instead of carrying them forward.
        integrate_unix(&d, home.path(), "/o/oxtail");
        let mut m = read_manifest(data.path()).unwrap().unwrap();
        m.items.push(Item::File {
            path: bashrc.clone(),
        });
        write_manifest(data.path(), &m).unwrap();
        integrate_unix(&d, home.path(), "/o/oxtail");
        let m = read_manifest(data.path()).unwrap().unwrap();
        assert_eq!(m.items.len(), 2);
        assert!(bashrc.exists());
    }

    #[test]
    fn registry_allow_list() {
        let ok_keys = [
            "Software\\Classes\\Applications\\oxtail.exe",
            "software\\classes\\applications\\OXTAIL.EXE\\shell\\open\\command",
            "Software\\Classes\\OxTail.LogFile\\DefaultIcon",
            "Software\\Classes\\Applications",
            "Software\\Classes\\.log",
            "Software\\Classes\\.log\\OpenWithProgids",
        ];
        for k in ok_keys {
            assert!(reg_key_allowed(k), "{k}");
        }
        for k in [
            "Software",
            "Software\\Classes",
            "Software\\Classes\\Applications\\notepad.exe",
            "Software\\Classes\\Applications\\oxtail.exe\\..\\..\\x",
            "Software\\Classes\\.log\\shell",
            "Software\\Classes\\.exe",
            "Software\\Classes\\Applications\\oxtail.exe2",
            "",
            "\\Software",
        ] {
            assert!(!reg_key_allowed(k), "{k}");
        }
        assert!(reg_value_allowed(
            "Software\\Classes\\.txt\\OpenWithProgids",
            PROG_ID
        ));
        assert!(!reg_value_allowed(
            "Software\\Classes\\.txt\\OpenWithProgids",
            "Other"
        ));
        assert!(!reg_value_allowed("Software\\Classes\\.txt", PROG_ID));
    }

    #[test]
    fn windows_items_are_checked_on_the_windows_platform() {
        let e = env(Path::new("/x"));
        let key = Item::RegKey {
            key: "Software\\Classes\\OxTail.LogFile".into(),
        };
        assert!(is_ours(Platform::Windows, &key, &e));
        assert!(!is_ours(Platform::Unix, &key, &e));
        assert!(!is_ours(
            Platform::Windows,
            &Item::RegKey {
                key: "Software".into()
            },
            &e
        ));
    }

    #[test]
    fn foreign_files_are_not_overwritten() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("applications")).unwrap();
        std::fs::write(desktop_path(home.path()), "[Desktop Entry]\nName=Mine\n").unwrap();
        let d = dd(data.path());
        let msgs = integrate_unix(&d, home.path(), "/o/oxtail");
        assert!(msgs.iter().any(|m| m.starts_with("refusing to overwrite")));
        assert_eq!(
            std::fs::read_to_string(desktop_path(home.path())).unwrap(),
            "[Desktop Entry]\nName=Mine\n"
        );
        // The icon was still installed, and only it is recorded.
        assert!(icon_path(home.path()).is_file());
        let m = read_manifest(data.path()).unwrap().unwrap();
        assert_eq!(
            m.items,
            vec![Item::File {
                path: icon_path(home.path())
            }]
        );
        // A marked desktop file is ours and gets updated.
        remove_with(Platform::Unix, &d, &env(home.path()), false).unwrap();
        std::fs::write(
            desktop_path(home.path()),
            "[Desktop Entry]\nX-OxTail-Integration=old\n",
        )
        .unwrap();
        integrate_unix(&d, home.path(), "/o/oxtail");
        assert!(
            std::fs::read_to_string(desktop_path(home.path()))
                .unwrap()
                .contains("Exec=")
        );
    }

    #[test]
    fn a_desktop_file_without_the_marker_is_not_removed() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let d = dd(data.path());
        integrate_unix(&d, home.path(), "/o/oxtail");
        std::fs::write(desktop_path(home.path()), "user edited").unwrap();
        let msgs = remove_with(Platform::Unix, &d, &env(home.path()), false).unwrap();
        assert!(msgs.iter().any(|m| m.contains("marker")));
        assert!(desktop_path(home.path()).exists());
    }

    #[test]
    fn a_manifest_from_another_computer_is_not_acted_on() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let d = dd(data.path());
        integrate_unix(&d, home.path(), "/o/oxtail");
        let other = Env {
            fingerprint: "host=other;user=u;home=/h".into(),
            ..env(home.path())
        };
        assert!(matches!(
            state_for(Platform::Unix, d.root.as_deref(), &other),
            IntegrationState::Unsupported(m) if m.contains("another computer")
        ));
        assert!(remove_with(Platform::Unix, &d, &other, false).is_err());
        assert!(desktop_path(home.path()).exists());
        assert!(manifest_path(data.path()).exists());
        // Integrating on the new machine starts a fresh manifest.
        let msgs =
            integrate_with(Platform::Unix, &d, Path::new("/n/oxtail"), &other, false).unwrap();
        assert!(msgs.iter().any(|m| m.contains("another computer")));
        assert_eq!(
            read_manifest(data.path()).unwrap().unwrap().fingerprint,
            other.fingerprint
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_folder_pointing_elsewhere_is_not_ours() {
        let home = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), home.path().join("applications")).unwrap();
        std::fs::write(
            elsewhere
                .path()
                .join("io.github.patricksindelka.OxTail.desktop"),
            "x",
        )
        .unwrap();
        let item = Item::File {
            path: desktop_path(home.path()),
        };
        assert!(!is_ours(Platform::Unix, &item, &env(home.path())));
    }
}
