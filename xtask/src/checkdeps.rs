//! `check-deps`: verify a binary's dynamic dependencies against an allowlist.

use anyhow::{Context, Result, bail};
use object::{Endianness, FileKind, elf, macho, read};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const HELP: &str = "\
Verify a built binary only links allowed system libraries and stays small.

USAGE:
    cargo xtask check-deps --binary <path> [--allowlist <file>]

OPTIONS:
    --binary <path>      Binary to inspect (ELF, Mach-O or PE; the OS family is
                         taken from the file format, not the host)
    --allowlist <file>   TOML allowlist [default: xtask/deps-allowlist.toml]
    -h, --help           Print this help
";

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Family {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Allowlist {
    pub max_size_mb: f64,
    pub linux: Family,
    pub macos: Family,
    pub windows: Family,
}

impl Default for Allowlist {
    fn default() -> Self {
        Self {
            max_size_mb: 20.0,
            linux: Family::default(),
            macos: Family::default(),
            windows: Family::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    Mac,
    Windows,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Allowed,
    Denied,
    NotAllowed,
}

/// Case-insensitive glob match supporting `*` (any run) and `?` (any one char).
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

impl Family {
    pub fn judge(&self, lib: &str) -> Verdict {
        if self.deny.iter().any(|d| glob_match(d, lib)) {
            Verdict::Denied
        } else if self.allow.iter().any(|a| glob_match(a, lib)) {
            Verdict::Allowed
        } else {
            Verdict::NotAllowed
        }
    }
}

impl Allowlist {
    pub fn family(&self, os: Os) -> &Family {
        match os {
            Os::Linux => &self.linux,
            Os::Mac => &self.macos,
            Os::Windows => &self.windows,
        }
    }
}

fn elf_needed<E: read::elf::FileHeader<Endian = Endianness>>(data: &[u8]) -> Result<Vec<String>> {
    let header = E::parse(data)?;
    let endian = header.endian()?;
    let sections = header.sections(endian, data)?;
    let mut out = Vec::new();
    for section in sections.iter() {
        use object::read::elf::{Dyn, SectionHeader};
        if let Some((dyns, link)) = section.dynamic(endian, data)? {
            let strings = sections.strings(endian, data, link)?;
            for d in dyns {
                if d.is_string(endian) && d.tag(endian) == elf::DT_NEEDED {
                    let name = d.string(endian, strings)?;
                    out.push(String::from_utf8_lossy(name).into_owned());
                }
            }
        }
    }
    Ok(out)
}

fn macho_dylibs<M: read::macho::MachHeader<Endian = Endianness>>(
    data: &[u8],
) -> Result<Vec<String>> {
    let header = M::parse(data, 0)?;
    let endian = header.endian()?;
    let mut cmds = header.load_commands(endian, data, 0)?;
    let mut out = Vec::new();
    while let Some(cmd) = cmds.next()? {
        if cmd.cmd() == macho::LC_ID_DYLIB {
            continue;
        }
        if let Some(dylib) = cmd.dylib()? {
            let name = cmd.string(endian, dylib.dylib.name)?;
            out.push(String::from_utf8_lossy(name).into_owned());
        }
    }
    Ok(out)
}

fn pe_imports<P: read::pe::ImageNtHeaders>(data: &[u8]) -> Result<Vec<String>> {
    let file = read::pe::PeFile::<P>::parse(data)?;
    let mut out = Vec::new();
    if let Some(table) = file.import_table()? {
        let mut descs = table.descriptors()?;
        while let Some(desc) = descs.next()? {
            let name = table.name(desc.name.get(object::LittleEndian))?;
            out.push(String::from_utf8_lossy(name).into_owned());
        }
    }
    Ok(out)
}

/// Return the OS family and the list of dynamically linked libraries of a binary image.
pub fn dependencies(data: &[u8]) -> Result<(Os, Vec<String>)> {
    Ok(match FileKind::parse(data)? {
        FileKind::Elf32 => (
            Os::Linux,
            elf_needed::<elf::FileHeader32<Endianness>>(data)?,
        ),
        FileKind::Elf64 => (
            Os::Linux,
            elf_needed::<elf::FileHeader64<Endianness>>(data)?,
        ),
        FileKind::MachO32 => (
            Os::Mac,
            macho_dylibs::<macho::MachHeader32<Endianness>>(data)?,
        ),
        FileKind::MachO64 => (
            Os::Mac,
            macho_dylibs::<macho::MachHeader64<Endianness>>(data)?,
        ),
        FileKind::Pe32 => (
            Os::Windows,
            pe_imports::<object::pe::ImageNtHeaders32>(data)?,
        ),
        FileKind::Pe64 => (
            Os::Windows,
            pe_imports::<object::pe::ImageNtHeaders64>(data)?,
        ),
        k => bail!("unsupported binary format {k:?} (fat Mach-O archives are not supported)"),
    })
}

pub struct Report {
    pub os: Os,
    pub rows: Vec<(String, Verdict)>,
    pub size_mb: f64,
    pub max_size_mb: f64,
}

impl Report {
    pub fn failures(&self) -> Vec<String> {
        let mut f = Vec::new();
        for (lib, v) in &self.rows {
            match v {
                Verdict::Allowed => {}
                Verdict::Denied => {
                    let hint = if self.os == Os::Windows {
                        " (dynamic C runtime: build with `-C target-feature=+crt-static`)"
                    } else {
                        ""
                    };
                    f.push(format!("`{lib}` is explicitly denied{hint}"));
                }
                Verdict::NotAllowed => f.push(format!("`{lib}` is not in the allowlist")),
            }
        }
        if self.size_mb > self.max_size_mb {
            f.push(format!(
                "binary is {:.2} MB, larger than max_size_mb = {}",
                self.size_mb, self.max_size_mb
            ));
        }
        f
    }
}

pub fn analyze(data: &[u8], list: &Allowlist) -> Result<Report> {
    let (os, mut libs) = dependencies(data)?;
    libs.sort_by_key(|l| l.to_lowercase());
    libs.dedup();
    let fam = list.family(os);
    let rows = libs
        .into_iter()
        .map(|l| {
            let v = fam.judge(&l);
            (l, v)
        })
        .collect();
    Ok(Report {
        os,
        rows,
        size_mb: data.len() as f64 / (1024.0 * 1024.0),
        max_size_mb: list.max_size_mb,
    })
}

pub fn run(mut p: lexopt::Parser) -> Result<()> {
    use lexopt::prelude::*;
    let mut binary: Option<PathBuf> = None;
    let mut allowlist = PathBuf::from("xtask/deps-allowlist.toml");
    while let Some(arg) = p.next()? {
        match arg {
            Long("binary") => binary = Some(p.value()?.into()),
            Long("allowlist") => allowlist = p.value()?.into(),
            Short('h') | Long("help") => {
                print!("{HELP}");
                return Ok(());
            }
            _ => return Err(arg.unexpected().into()),
        }
    }
    let binary = binary.context("missing --binary <path>")?;
    check(&binary, &allowlist)
}

pub fn check(binary: &Path, allowlist: &Path) -> Result<()> {
    let text = std::fs::read_to_string(allowlist)
        .with_context(|| format!("reading allowlist {}", allowlist.display()))?;
    let list: Allowlist = toml::from_str(&text)
        .with_context(|| format!("parsing allowlist {}", allowlist.display()))?;
    let data = std::fs::read(binary).with_context(|| format!("reading {}", binary.display()))?;
    let report = analyze(&data, &list)?;

    println!(
        "{}: {:?}, {:.2} MB (max {} MB)",
        binary.display(),
        report.os,
        report.size_mb,
        report.max_size_mb
    );
    let width = report
        .rows
        .iter()
        .map(|(l, _)| l.len())
        .max()
        .unwrap_or(7)
        .max(7);
    println!("{:<width$}  STATUS", "LIBRARY");
    for (lib, v) in &report.rows {
        let s = match v {
            Verdict::Allowed => "ok",
            Verdict::Denied => "DENIED",
            Verdict::NotAllowed => "NOT ALLOWED",
        };
        println!("{lib:<width$}  {s}");
    }
    let failures = report.failures();
    if failures.is_empty() {
        println!("check-deps: OK");
        Ok(())
    } else {
        let mut msg = String::from("check-deps failed:\n");
        for f in failures {
            msg.push_str(&format!("  - {f}\n"));
        }
        msg.push_str(&format!(
            "Fix the build, or (if the dependency is truly a system library) update {}.",
            allowlist.display()
        ));
        bail!(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("libc.so.*", "libc.so.6"));
        assert!(glob_match("LIBC.SO.*", "libc.so.6"));
        assert!(!glob_match("libc.so.*", "libcrypto.so.3"));
        assert!(glob_match("ld-linux-*.so.*", "ld-linux-x86-64.so.2"));
        assert!(glob_match(
            "/System/Library/Frameworks/*",
            "/System/Library/Frameworks/AppKit.framework/Versions/C/AppKit"
        ));
        assert!(glob_match(
            "api-ms-win-core-*.dll",
            "API-MS-WIN-CORE-synch-l1-2-0.dll"
        ));
        assert!(!glob_match(
            "api-ms-win-core-*.dll",
            "api-ms-win-crt-heap-l1-1-0.dll"
        ));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(glob_match("*", ""));
        assert!(glob_match("a*b*c", "aXXbYYc"));
        assert!(!glob_match("a*b*c", "aXXbYY"));
    }

    fn fam() -> Family {
        Family {
            allow: vec!["kernel32.dll".into(), "api-ms-win-core-*.dll".into()],
            deny: vec!["vcruntime*.dll".into(), "api-ms-win-crt-*".into()],
        }
    }

    #[test]
    fn judging() {
        let f = fam();
        assert_eq!(f.judge("KERNEL32.dll"), Verdict::Allowed);
        assert_eq!(f.judge("api-ms-win-core-file-l1-1-0.dll"), Verdict::Allowed);
        assert_eq!(f.judge("VCRUNTIME140.dll"), Verdict::Denied);
        assert_eq!(
            f.judge("api-ms-win-crt-runtime-l1-1-0.dll"),
            Verdict::Denied
        );
        assert_eq!(f.judge("foo.dll"), Verdict::NotAllowed);
    }

    #[test]
    fn deny_wins_over_allow() {
        let f = Family {
            allow: vec!["*".into()],
            deny: vec!["bad*".into()],
        };
        assert_eq!(f.judge("bad.dll"), Verdict::Denied);
        assert_eq!(f.judge("good.dll"), Verdict::Allowed);
    }

    #[test]
    fn size_limit_reported() {
        let r = Report {
            os: Os::Linux,
            rows: vec![],
            size_mb: 21.0,
            max_size_mb: 20.0,
        };
        assert_eq!(r.failures().len(), 1);
    }

    #[test]
    fn shipped_allowlist_parses() {
        let text = include_str!("../deps-allowlist.toml");
        let l: Allowlist = toml::from_str(text).unwrap();
        assert!(l.linux.judge("libc.so.6") == Verdict::Allowed);
        assert!(l.linux.judge("libX11.so.6") == Verdict::NotAllowed);
        assert!(l.windows.judge("ucrtbase.dll") == Verdict::Denied);
        assert!(l.windows.judge("api-ms-win-crt-stdio-l1-1-0.dll") == Verdict::Denied);
        assert!(l.macos.judge("/usr/lib/libSystem.B.dylib") == Verdict::Allowed);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn checks_own_binary() {
        let exe = std::env::current_exe().unwrap();
        let data = std::fs::read(exe).unwrap();
        let (os, libs) = dependencies(&data).unwrap();
        assert_eq!(os, Os::Linux);
        assert!(libs.iter().any(|l| l == "libc.so.6"), "{libs:?}");
    }
}
