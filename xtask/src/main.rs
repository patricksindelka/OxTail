//! Developer tooling for OxTail: `cargo xtask <command>`.

mod checkdeps;
mod genlog;
mod logfmt;

use anyhow::{Result, bail};

const HELP: &str = "\
OxTail developer tasks.

USAGE:
    cargo xtask <COMMAND> [OPTIONS]

COMMANDS:
    gen-log       Generate a deterministic synthetic log file
    append-log    Simulate a live log writer (with rotation) for follow testing
    check-deps    Verify a binary's dynamic dependencies and size against an allowlist

Run `cargo xtask <COMMAND> --help` for details.
";

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut parser = lexopt::Parser::from_env();
    match parser.next()? {
        Some(lexopt::Arg::Value(cmd)) => match cmd.string()?.as_str() {
            "gen-log" => genlog::run_gen(parser),
            "append-log" => genlog::run_append(parser),
            "check-deps" => checkdeps::run(parser),
            other => bail!("unknown command `{other}`\n\n{HELP}"),
        },
        Some(lexopt::Arg::Short('h') | lexopt::Arg::Long("help")) | None => {
            print!("{HELP}");
            Ok(())
        }
        Some(arg) => Err(arg.unexpected().into()),
    }
}
