//! `gen-log` and `append-log` subcommands.

use crate::logfmt::{Encoding, Format, Generator, parse_rfc3339, parse_size};
use anyhow::{Context, Result, bail};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const GEN_HELP: &str = "\
Generate a deterministic synthetic log file.

USAGE:
    cargo xtask gen-log --format <FMT> (--size <SIZE> | --lines <N>) [OPTIONS] <out>

OPTIONS:
    --format <FMT>          plain|nginx|apache|jsonl|logfmt|log4j|syslog|syslog5424|csv|iis
    --size <SIZE>           Approximate output size, e.g. 500M, 10G (1024-based)
    --lines <N>             Number of log entries instead of a size
    --seed <N>              RNG seed [default: 1]
    --start <RFC3339>       First timestamp [default: 2024-01-01T00:00:00Z]
    --rate <LINES/S>        Simulated lines per second of log time [default: 100]
    --multiline-ratio <R>   Fraction of entries followed by a Java stack trace
                            (plain, log4j, logfmt, syslog, syslog5424) [default: 0.01]
    --crlf                  Use CRLF line endings
    --encoding <ENC>        utf8|utf16le|latin1 [default: utf8]
    -h, --help              Print this help
";

pub const APPEND_HELP: &str = "\
Simulate a live writer for manual follow/tail testing.

USAGE:
    cargo xtask append-log --format <FMT> --rate <LINES/S> [OPTIONS] <path>

OPTIONS:
    --format <FMT>          Same formats as gen-log
    --rate <LINES/S>        Real-time lines per second to append
    --rotate-every <N>      Rotate the file after every N lines
    --rotate-mode <MODE>    rename (move to <path>.1, recreate) | copytruncate [default: rename]
    --duration <SECS>       Stop after this many seconds [default: run until interrupted]
    --seed <N>              RNG seed [default: 1]
    -h, --help              Print this help
";

const CHUNK: usize = 1 << 20;

pub fn run_gen(mut p: lexopt::Parser) -> Result<()> {
    use lexopt::prelude::*;
    let mut format = None;
    let mut size = None;
    let mut lines: Option<u64> = None;
    let mut seed = 1u64;
    let mut start = parse_rfc3339("2024-01-01T00:00:00Z")?;
    let mut rate = 100.0f64;
    let mut ml = 0.01f64;
    let mut crlf = false;
    let mut enc = Encoding::Utf8;
    let mut out: Option<PathBuf> = None;
    while let Some(arg) = p.next()? {
        match arg {
            Long("format") => format = Some(Format::parse(&p.value()?.string()?)?),
            Long("size") => size = Some(parse_size(&p.value()?.string()?)?),
            Long("lines") => lines = Some(p.value()?.parse()?),
            Long("seed") => seed = p.value()?.parse()?,
            Long("start") => start = parse_rfc3339(&p.value()?.string()?)?,
            Long("rate") => rate = p.value()?.parse()?,
            Long("multiline-ratio") => ml = p.value()?.parse()?,
            Long("crlf") => crlf = true,
            Long("encoding") => enc = Encoding::parse(&p.value()?.string()?)?,
            Short('h') | Long("help") => {
                print!("{GEN_HELP}");
                return Ok(());
            }
            Value(v) if out.is_none() => out = Some(v.into()),
            _ => return Err(arg.unexpected().into()),
        }
    }
    let format = format.context("missing --format")?;
    let out = out.context("missing output path")?;
    if size.is_none() == lines.is_none() {
        bail!("specify exactly one of --size or --lines");
    }
    if rate <= 0.0 {
        bail!("--rate must be positive");
    }
    if !(0.0..=1.0).contains(&ml) {
        bail!("--multiline-ratio must be within 0..=1");
    }

    let mut g = Generator::new(format, seed, start, rate, ml, crlf);
    let mut file = File::create(&out).with_context(|| format!("creating {}", out.display()))?;
    let t0 = Instant::now();
    let mut written = 0u64;
    let mut entries = 0u64;
    let mut buf = Vec::with_capacity(CHUNK + 64 * 1024);
    let mut enc_buf = Vec::new();

    file.write_all(enc.bom())?;
    written += enc.bom().len() as u64;
    g.header(&mut buf);
    loop {
        // Fill one chunk.
        let mut done = false;
        while buf.len() < CHUNK {
            if let Some(n) = lines
                && entries >= n
            {
                done = true;
                break;
            }
            if let Some(s) = size {
                let pending = match enc {
                    Encoding::Utf16Le => buf.len() as u64 * 2,
                    _ => buf.len() as u64,
                };
                if written + pending >= s {
                    done = true;
                    break;
                }
            }
            g.line(&mut buf);
            entries += 1;
        }
        if enc == Encoding::Utf8 {
            file.write_all(&buf)?;
            written += buf.len() as u64;
        } else {
            enc.encode(&buf, &mut enc_buf);
            file.write_all(&enc_buf)?;
            written += enc_buf.len() as u64;
        }
        buf.clear();
        if done {
            break;
        }
    }
    file.flush()?;
    let secs = t0.elapsed().as_secs_f64().max(1e-9);
    println!(
        "wrote {} entries, {:.1} MiB to {} in {:.2}s ({:.0} MiB/s)",
        entries,
        written as f64 / 1_048_576.0,
        out.display(),
        secs,
        written as f64 / 1_048_576.0 / secs
    );
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RotateMode {
    Rename,
    CopyTruncate,
}

fn open_append(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))
}

fn rotated_name(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".1");
    PathBuf::from(s)
}

pub fn run_append(mut p: lexopt::Parser) -> Result<()> {
    use lexopt::prelude::*;
    let mut format = None;
    let mut rate: Option<f64> = None;
    let mut rotate_every: Option<u64> = None;
    let mut mode = RotateMode::Rename;
    let mut duration: Option<f64> = None;
    let mut seed = 1u64;
    let mut path: Option<PathBuf> = None;
    while let Some(arg) = p.next()? {
        match arg {
            Long("format") => format = Some(Format::parse(&p.value()?.string()?)?),
            Long("rate") => rate = Some(p.value()?.parse()?),
            Long("rotate-every") => rotate_every = Some(p.value()?.parse()?),
            Long("rotate-mode") => {
                mode = match p.value()?.string()?.as_str() {
                    "rename" => RotateMode::Rename,
                    "copytruncate" => RotateMode::CopyTruncate,
                    other => bail!("unknown rotate mode `{other}` (rename|copytruncate)"),
                }
            }
            Long("duration") => duration = Some(p.value()?.parse()?),
            Long("seed") => seed = p.value()?.parse()?,
            Short('h') | Long("help") => {
                print!("{APPEND_HELP}");
                return Ok(());
            }
            Value(v) if path.is_none() => path = Some(v.into()),
            _ => return Err(arg.unexpected().into()),
        }
    }
    let format = format.context("missing --format")?;
    let rate = rate.context("missing --rate")?;
    let path = path.context("missing path")?;
    if rate <= 0.0 {
        bail!("--rate must be positive");
    }
    if rotate_every == Some(0) {
        bail!("--rotate-every must be at least 1");
    }

    let now_micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_micros() as i64;
    let mut g = Generator::new(format, seed, now_micros, rate, 0.01, false);
    let mut file = open_append(&path)?;
    let mut buf = Vec::new();
    if file.metadata()?.len() == 0 {
        g.header(&mut buf);
        file.write_all(&buf)?;
        buf.clear();
    }

    let t0 = Instant::now();
    let mut total = 0u64;
    let mut since_rotate = 0u64;
    let mut rotations = 0u64;
    eprintln!(
        "appending {rate} lines/s to {} (Ctrl-C to stop)",
        path.display()
    );
    loop {
        let elapsed = t0.elapsed().as_secs_f64();
        if duration.is_some_and(|d| elapsed >= d) {
            break;
        }
        let due = (elapsed * rate) as u64;
        while total < due {
            let mut n = due - total;
            if let Some(r) = rotate_every {
                n = n.min(r - since_rotate);
            }
            buf.clear();
            for _ in 0..n {
                g.line(&mut buf);
            }
            file.write_all(&buf)?;
            file.flush()?;
            total += n;
            since_rotate += n;
            if rotate_every.is_some_and(|r| since_rotate >= r) {
                since_rotate = 0;
                rotations += 1;
                let rotated = rotated_name(&path);
                match mode {
                    RotateMode::Rename => {
                        drop(file);
                        let _ = std::fs::remove_file(&rotated);
                        std::fs::rename(&path, &rotated)?;
                        file = open_append(&path)?;
                        buf.clear();
                        g.header(&mut buf);
                        file.write_all(&buf)?;
                    }
                    RotateMode::CopyTruncate => {
                        std::fs::copy(&path, &rotated)?;
                        file.set_len(0)?;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    eprintln!("done: {total} lines, {rotations} rotations");
    Ok(())
}
