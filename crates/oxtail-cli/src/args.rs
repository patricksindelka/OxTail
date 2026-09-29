//! Command-line parsing (`lexopt`).

use std::ffi::OsString;
use std::path::PathBuf;

use oxtail_config::Renderer;
use oxtail_gui::OpenRequest;

/// Usage text for `--help`.
pub const HELP: &str = "\
OxTail: a fast, portable, real-time log viewer

USAGE:
    oxtail [OPTIONS] [FILE|-]...

ARGS:
    FILE            A log file to open in a tab (opened at the tail, following).
    -               Read standard input (buffered to a spool file in the data folder).

OPTIONS:
    -n, --lines N        Start with the last N lines instead of the default tail view
        --profile NAME   Highlight profile for the opened files
        --filter QUERY   Filter the opened files (only lines containing QUERY)
        --merge          Open all the files as one merged tab, interleaved by timestamp
        --data-dir PATH  Use PATH as the data folder (settings, profiles, session)
        --renderer R     auto (default), wgpu or glow
        --new-instance   Do not hand the files to a running instance; start a new one
    -V, --version        Print the version
    -h, --help           Print this help
";

/// What the user asked for on the command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cli {
    /// Files (and `-` for stdin) in the order given.
    pub files: Vec<PathBuf>,
    /// `-` was given.
    pub stdin: bool,
    /// `-n N`.
    pub lines: Option<usize>,
    /// `--profile`.
    pub profile: Option<String>,
    /// `--filter`.
    pub filter: Option<String>,
    /// `--merge`: the files form one merged tab.
    pub merge: bool,
    /// `--data-dir`.
    pub data_dir: Option<PathBuf>,
    /// `--renderer` (none means "use the setting").
    pub renderer: Option<Renderer>,
    /// `--new-instance`.
    pub new_instance: bool,
    /// `--version`.
    pub version: bool,
    /// `--help`.
    pub help: bool,
}

impl Cli {
    /// The open request described by the arguments.
    pub fn to_request(&self) -> OpenRequest {
        OpenRequest {
            files: self.files.clone(),
            stdin: self.stdin,
            tail_lines: self.lines,
            profile: self.profile.clone(),
            filter: self.filter.clone(),
            merge: self.merge,
        }
    }
}

/// Parses a renderer name.
pub fn parse_renderer(s: &str) -> Result<Renderer, String> {
    match s.to_ascii_lowercase().as_str() {
        "auto" => Ok(Renderer::Auto),
        "wgpu" => Ok(Renderer::Wgpu),
        "glow" | "opengl" | "gl" => Ok(Renderer::Glow),
        other => Err(format!(
            "unknown renderer '{other}' (expected auto, wgpu or glow)"
        )),
    }
}

/// Parses the arguments (without the program name).
pub fn parse<I>(args: I) -> Result<Cli, String>
where
    I: IntoIterator,
    I::Item: Into<OsString>,
{
    use lexopt::prelude::*;

    let mut cli = Cli::default();
    let mut parser = lexopt::Parser::from_args(args);
    while let Some(arg) = parser.next().map_err(|e| e.to_string())? {
        match arg {
            Short('h') | Long("help") => cli.help = true,
            Short('V') | Long("version") => cli.version = true,
            Short('n') | Long("lines") => {
                let v = parser.value().map_err(|e| e.to_string())?;
                let text = v.to_string_lossy().into_owned();
                let n: usize = text
                    .trim_start_matches('+')
                    .parse()
                    .map_err(|_| format!("invalid number of lines: '{text}'"))?;
                cli.lines = Some(n);
            }
            Long("profile") => cli.profile = Some(value_string(&mut parser)?),
            Long("filter") => cli.filter = Some(value_string(&mut parser)?),
            Long("data-dir") => {
                cli.data_dir = Some(PathBuf::from(
                    parser.value().map_err(|e| e.to_string())?.as_os_str(),
                ));
            }
            Long("renderer") => {
                cli.renderer = Some(parse_renderer(&value_string(&mut parser)?)?);
            }
            Long("new-instance") => cli.new_instance = true,
            Long("merge") => cli.merge = true,
            Value(v) => {
                if v == "-" {
                    cli.stdin = true;
                } else {
                    cli.files.push(PathBuf::from(v));
                }
            }
            other => return Err(other.unexpected().to_string()),
        }
    }
    Ok(cli)
}

fn value_string(parser: &mut lexopt::Parser) -> Result<String, String> {
    parser
        .value()
        .map_err(|e| e.to_string())
        .map(|v| v.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Cli, String> {
        parse(args.iter().map(|s| OsString::from(*s)))
    }

    #[test]
    fn files_and_stdin() {
        let c = p(&["a.log", "-", "b.log"]).unwrap();
        assert_eq!(
            c.files,
            vec![PathBuf::from("a.log"), PathBuf::from("b.log")]
        );
        assert!(c.stdin);
    }

    #[test]
    fn options() {
        let c = p(&[
            "-n",
            "500",
            "--profile",
            "nginx",
            "--filter=level:ERROR",
            "--data-dir",
            "/tmp/x",
            "--renderer",
            "glow",
            "--new-instance",
            "f.log",
        ])
        .unwrap();
        assert_eq!(c.lines, Some(500));
        assert_eq!(c.profile.as_deref(), Some("nginx"));
        assert_eq!(c.filter.as_deref(), Some("level:ERROR"));
        assert_eq!(c.data_dir, Some(PathBuf::from("/tmp/x")));
        assert_eq!(c.renderer, Some(Renderer::Glow));
        assert!(c.new_instance);
        assert_eq!(c.files.len(), 1);
    }

    #[test]
    fn attached_short_value() {
        assert_eq!(p(&["-n20"]).unwrap().lines, Some(20));
    }

    #[test]
    fn errors_are_reported() {
        assert!(p(&["-n", "abc"]).is_err());
        assert!(p(&["--renderer", "vulkan"]).is_err());
        assert!(p(&["--bogus"]).is_err());
        assert!(p(&["-n"]).is_err());
    }

    #[test]
    fn version_and_help() {
        assert!(p(&["--version"]).unwrap().version);
        assert!(p(&["-h"]).unwrap().help);
    }

    #[test]
    fn request_carries_options() {
        let c = p(&["-n", "5", "--profile", "x", "a.log"]).unwrap();
        let r = c.to_request();
        assert_eq!(r.tail_lines, Some(5));
        assert_eq!(r.profile.as_deref(), Some("x"));
        assert_eq!(r.files.len(), 1);
    }

    #[test]
    fn merge_flag_marks_the_request() {
        let c = p(&["--merge", "a.log", "b.log"]).unwrap();
        assert!(c.merge);
        let r = c.to_request();
        assert!(r.merge);
        assert_eq!(r.files.len(), 2);
        assert!(!p(&["a.log"]).unwrap().to_request().merge);
    }

    #[test]
    fn no_arguments_is_fine() {
        assert_eq!(p(&[]).unwrap(), Cli::default());
    }
}
