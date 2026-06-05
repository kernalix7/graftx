//! GraftX in-repo task runner.
//!
//! Invoked through the `cargo xtask <subcommand>` alias (see `.cargo/config.toml`).
//! This is the home for build-time chores that should live in the repo rather
//! than in ad-hoc shell scripts — generating the opcode table from the protocol
//! definitions and checking that cross-references in the docs stay valid.
//!
//! `gen-opcodes` renders the opcode table to stdout. `check-xrefs` lints the
//! Markdown under `docs/` for broken relative links and out-of-range chapter
//! references, exiting non-zero when it finds problems so CI can gate on it.
#![forbid(unsafe_op_in_unsafe_fn)]

mod opcodes;
mod xrefs;

use std::path::Path;
use std::process::ExitCode;

/// Exit code returned for an unknown or malformed subcommand.
const EXIT_USAGE: u8 = 2;

/// Directory scanned by `check-xrefs`, relative to the repo root (the working
/// directory `cargo xtask` is invoked from).
const DOCS_DIR: &str = "docs";

fn main() -> ExitCode {
    // Skip argv[0] (the binary path); the dispatcher only cares about the
    // subcommand and its arguments.
    let args: Vec<String> = std::env::args().skip(1).collect();
    run(&args)
}

/// Dispatch a single xtask invocation.
///
/// `args` is the argument list *without* the program name. The first element
/// selects the subcommand; with no arguments we fall back to `help`.
///
/// Returns [`ExitCode::SUCCESS`] for a recognized subcommand (including `help`),
/// and an exit code of [`EXIT_USAGE`] for an unknown one — matching the
/// convention that a usage error is distinct from a clean run.
fn run(args: &[String]) -> ExitCode {
    let command = args.first().map(String::as_str).unwrap_or("help");

    match command {
        "gen-opcodes" => {
            print!("{}", opcodes::render_opcode_table(opcodes::OPCODES));
            ExitCode::SUCCESS
        }
        "check-xrefs" => {
            let issues = xrefs::check_xrefs(Path::new(DOCS_DIR), &mut std::io::stdout());
            // Exit non-zero on any issue so the check can gate CI; a clean run
            // exits zero.
            if issues == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        "help" | "-h" | "--help" => {
            print_usage(&mut std::io::stdout());
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("graftx-xtask: unknown subcommand `{other}`");
            print_usage(&mut std::io::stderr());
            ExitCode::from(EXIT_USAGE)
        }
    }
}

/// Write the usage banner to `out`.
///
/// Takes the writer so the same text can go to stdout (for `help`) or stderr
/// (for an unknown subcommand). Write errors are ignored: there is nothing
/// useful to do if printing usage itself fails, and propagating would just
/// complicate the exit-code contract.
fn print_usage<W: std::io::Write>(out: &mut W) {
    let _ = writeln!(
        out,
        "\
graftx-xtask — GraftX in-repo task runner

Usage:
    cargo xtask <subcommand>

Subcommands:
    gen-opcodes    Generate the opcode-table source from graftx-protocol
    check-xrefs    Validate cross-references between the docs and the protocol
    help           Show this message"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Turn a slice of string literals into the owned `Vec<String>` that
    /// [`run`] expects, mirroring what `std::env::args` would hand us.
    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_args_defaults_to_help() {
        assert_eq!(run(&argv(&[])), ExitCode::SUCCESS);
    }

    #[test]
    fn help_succeeds() {
        assert_eq!(run(&argv(&["help"])), ExitCode::SUCCESS);
        assert_eq!(run(&argv(&["-h"])), ExitCode::SUCCESS);
        assert_eq!(run(&argv(&["--help"])), ExitCode::SUCCESS);
    }

    #[test]
    fn gen_opcodes_succeeds() {
        assert_eq!(run(&argv(&["gen-opcodes"])), ExitCode::SUCCESS);
    }

    #[test]
    fn check_xrefs_is_a_known_subcommand() {
        // `check-xrefs` returns SUCCESS (no issues) or FAILURE (issues found)
        // depending on the docs as seen from the test's working directory, but
        // never the usage error reserved for unknown subcommands.
        assert_ne!(run(&argv(&["check-xrefs"])), ExitCode::from(EXIT_USAGE));
    }

    #[test]
    fn extra_args_after_known_subcommand_are_tolerated() {
        assert_eq!(run(&argv(&["gen-opcodes", "--dry-run"])), ExitCode::SUCCESS);
    }

    #[test]
    fn unknown_subcommand_is_usage_error() {
        assert_eq!(run(&argv(&["frobnicate"])), ExitCode::from(EXIT_USAGE));
    }
}
