//! GraftX in-repo task runner.
//!
//! Invoked through the `cargo xtask <subcommand>` alias (see `.cargo/config.toml`).
//! This is the home for build-time chores that should live in the repo rather
//! than in ad-hoc shell scripts — generating the opcode table from the protocol
//! definitions and checking that cross-references in the docs stay valid.
//!
//! The subcommands are scaffolding for now: each one announces what it will do
//! and exits cleanly so the wiring (workspace member, cargo alias, dispatch) can
//! be exercised before the real codegen lands.
#![forbid(unsafe_op_in_unsafe_fn)]

use std::process::ExitCode;

/// Exit code returned for an unknown or malformed subcommand.
const EXIT_USAGE: u8 = 2;

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
            not_yet_implemented(
                "gen-opcodes",
                "generate the opcode-table source from the graftx-protocol definitions",
            );
            ExitCode::SUCCESS
        }
        "check-xrefs" => {
            not_yet_implemented(
                "check-xrefs",
                "validate cross-references between the docs and the protocol",
            );
            ExitCode::SUCCESS
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

/// Announce a subcommand that exists but has no implementation yet.
fn not_yet_implemented(command: &str, plan: &str) {
    println!("graftx-xtask: `{command}` not yet implemented (planned: {plan})");
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
    fn known_subcommands_succeed() {
        assert_eq!(run(&argv(&["gen-opcodes"])), ExitCode::SUCCESS);
        assert_eq!(run(&argv(&["check-xrefs"])), ExitCode::SUCCESS);
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
