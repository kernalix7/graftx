//! GraftX in-repo task runner.
//!
//! Invoked through the `cargo xtask <subcommand>` alias (see `.cargo/config.toml`).
//! This is the home for build-time chores that should live in the repo rather
//! than in ad-hoc shell scripts — generating the opcode table from the protocol
//! definitions and checking that cross-references in the docs stay valid.
//!
//! `gen-opcodes` renders the opcode table to stdout. `opcodes-lock` freezes the
//! opcode table to `docs/design/opcodes.lock` (`--write`) and verifies it
//! (`--check`), so an accidental opcode renumbering surfaces as a failed check.
//! `check-xrefs` lints the Markdown under `docs/` for broken relative links and
//! out-of-range chapter references, exiting non-zero when it finds problems so
//! CI can gate on it. `verify` runs `check-xrefs` and `opcodes-lock --check`
//! together and exits zero only when both pass, so CI can gate on one command.
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

/// Lockfile written and verified by `opcodes-lock`, relative to the repo root.
const OPCODES_LOCK: &str = "docs/design/opcodes.lock";

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
        "coverage" => {
            print!("{}", opcodes::render_coverage(opcodes::OPCODES));
            ExitCode::SUCCESS
        }
        "stats" => {
            print!("{}", opcodes::render_stats(opcodes::OPCODES));
            ExitCode::SUCCESS
        }
        "opcodes-lock" => opcodes_lock(&args[1..], Path::new(OPCODES_LOCK)),
        "verify" => verify(
            Path::new(DOCS_DIR),
            Path::new(OPCODES_LOCK),
            &mut std::io::stdout(),
        ),
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

/// The mode `opcodes-lock` runs in, selected by its flags.
enum LockMode {
    /// `--write`: render the lockfile and write it to disk.
    Write,
    /// `--check` (the default): render the lockfile and compare it to disk,
    /// failing on any difference or a missing file.
    Check,
}

/// Parse the `opcodes-lock` flags into a [`LockMode`].
///
/// `--check` is the default when no flag is given, so a bare `opcodes-lock` is a
/// read-only verification — safe to wire into CI. An unrecognized flag is a usage
/// error rather than a silent fallback.
fn parse_lock_mode(args: &[String]) -> Result<LockMode, String> {
    match args.first().map(String::as_str) {
        None | Some("--check") => Ok(LockMode::Check),
        Some("--write") => Ok(LockMode::Write),
        Some(other) => Err(format!(
            "unknown flag `{other}` (expected --write or --check)"
        )),
    }
}

/// Run the `opcodes-lock` subcommand against the lockfile at `lock_path`.
///
/// `--write` renders the canonical lockfile and writes it, reporting what it
/// wrote. `--check` (the default) renders the same content and compares it to the
/// file on disk, returning [`ExitCode::FAILURE`] — with a short message naming the
/// first differing line, or noting the file is missing — when they diverge, and
/// [`ExitCode::SUCCESS`] when they match. The renderer is deterministic, so a
/// freshly written file always passes a subsequent check.
fn opcodes_lock(args: &[String], lock_path: &Path) -> ExitCode {
    let mode = match parse_lock_mode(args) {
        Ok(mode) => mode,
        Err(e) => {
            eprintln!("opcodes-lock: {e}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let rendered = opcodes::render_lock(opcodes::OPCODES);

    match mode {
        LockMode::Write => match std::fs::write(lock_path, &rendered) {
            Ok(()) => {
                let lines = rendered.lines().filter(|l| !l.starts_with('#')).count();
                println!(
                    "opcodes-lock: wrote {} ({lines} opcode(s))",
                    lock_path.display()
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("opcodes-lock: failed to write {}: {e}", lock_path.display());
                ExitCode::FAILURE
            }
        },
        // `--check` delegates the read-and-compare to `opcodes::check_lock` so the
        // same logic backs both this CLI arm and the `verify` aggregator; the
        // reporting (and exit code) stays here, unchanged.
        LockMode::Check => match opcodes::check_lock(lock_path, opcodes::OPCODES) {
            opcodes::LockStatus::UpToDate => {
                println!("opcodes-lock: {} is up to date", lock_path.display());
                ExitCode::SUCCESS
            }
            opcodes::LockStatus::Stale { on_disk, expected } => {
                eprintln!(
                    "opcodes-lock: {} is out of date — run `cargo xtask opcodes-lock --write`",
                    lock_path.display()
                );
                report_first_difference(&mut std::io::stderr(), &on_disk, &expected);
                ExitCode::FAILURE
            }
            opcodes::LockStatus::Unreadable { error } => {
                eprintln!(
                    "opcodes-lock: cannot read {}: {error} — run `cargo xtask opcodes-lock --write`",
                    lock_path.display()
                );
                ExitCode::FAILURE
            }
        },
    }
}

/// Aggregate result of the two checks `verify` runs.
///
/// Keeping the pass/fail decision in a small pure type — separate from the
/// filesystem and printing — means the "Ok only when both pass" rule can be unit
/// tested without touching disk. `xref_issues` is the count from
/// [`xrefs::check_xrefs`]; `lock_up_to_date` is whether the opcode lock matched.
struct VerifyOutcome {
    /// Number of cross-reference issues found under `docs/` (0 means clean).
    xref_issues: usize,
    /// Whether `opcodes-lock --check` would pass.
    lock_up_to_date: bool,
}

impl VerifyOutcome {
    /// Whether both sub-checks passed: no xref issues *and* an up-to-date lock.
    fn passed(&self) -> bool {
        self.xref_issues == 0 && self.lock_up_to_date
    }

    /// Map the outcome to a process exit code: success only when both pass.
    fn exit_code(&self) -> ExitCode {
        if self.passed() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }
}

/// Run the aggregate `verify` check: the xref lint over `docs_dir` *and* the
/// opcode-lock verification against `lock_path`.
///
/// Both sub-checks always run (no short-circuit) so a single invocation surfaces
/// every problem at once. Each reuses the existing logic — [`xrefs::check_xrefs`]
/// and [`opcodes::check_lock`] — rather than reimplementing it. A short
/// per-check summary is written to `out`, and the return value is
/// [`ExitCode::SUCCESS`] only when both pass, else [`ExitCode::FAILURE`].
fn verify<W: std::io::Write>(docs_dir: &Path, lock_path: &Path, out: &mut W) -> ExitCode {
    // `check_xrefs` prints its own per-file report to `out`; let it, then add the
    // aggregate one-liner below so the summary lines read consistently.
    let xref_issues = xrefs::check_xrefs(docs_dir, out);
    let lock_up_to_date = opcodes::check_lock(lock_path, opcodes::OPCODES).is_up_to_date();

    let outcome = VerifyOutcome {
        xref_issues,
        lock_up_to_date,
    };

    let _ = writeln!(out, "verify: xrefs: {} issue(s)", outcome.xref_issues);
    let _ = writeln!(
        out,
        "verify: opcodes-lock: {}",
        if outcome.lock_up_to_date {
            "up-to-date"
        } else {
            "STALE"
        }
    );
    let _ = writeln!(
        out,
        "verify: {}",
        if outcome.passed() {
            "OK (all checks passed)"
        } else {
            "FAILED"
        }
    );

    outcome.exit_code()
}

/// Write a short, diff-ish note about the first line where `have` and `want`
/// differ to `out`.
///
/// Lines are compared in order; the first mismatch (or a length difference once
/// the shorter side is exhausted) is reported with its 1-based line number and
/// both sides. Write failures are ignored for the same reason as the usage
/// banner: the exit code, not this text, is what callers gate on.
fn report_first_difference<W: std::io::Write>(out: &mut W, have: &str, want: &str) {
    for (idx, (have_line, want_line)) in have.lines().zip(want.lines()).enumerate() {
        if have_line != want_line {
            let _ = writeln!(out, "  first difference at line {}:", idx + 1);
            let _ = writeln!(out, "    on disk:   {have_line}");
            let _ = writeln!(out, "    expected:  {want_line}");
            return;
        }
    }
    // No differing line within the shared prefix: the files differ only in
    // length (one is a prefix of the other), e.g. an opcode was added or removed.
    let have_count = have.lines().count();
    let want_count = want.lines().count();
    if have_count != want_count {
        let _ = writeln!(
            out,
            "  line count differs: {have_count} on disk vs {want_count} expected"
        );
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
    coverage       Summarize opcode coverage per API as a Markdown roll-up
    stats          Print opcode totals, distinct API count, and per-API roll-up
    opcodes-lock   Freeze (--write) or verify (--check, default) the opcode lock
    check-xrefs    Validate cross-references between the docs and the protocol
    verify         Run check-xrefs and opcodes-lock --check together (CI gate)
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
    fn coverage_succeeds() {
        assert_eq!(run(&argv(&["coverage"])), ExitCode::SUCCESS);
    }

    #[test]
    fn stats_succeeds() {
        assert_eq!(run(&argv(&["stats"])), ExitCode::SUCCESS);
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

    #[test]
    fn opcodes_lock_is_a_known_subcommand() {
        // Dispatched (not a usage error); the default `--check` against the real
        // working directory may pass or fail depending on the checkout, so we
        // only assert it is recognized.
        assert_ne!(run(&argv(&["opcodes-lock"])), ExitCode::from(EXIT_USAGE));
    }

    #[test]
    fn parse_lock_mode_defaults_to_check() {
        assert!(matches!(parse_lock_mode(&[]), Ok(LockMode::Check)));
        assert!(matches!(
            parse_lock_mode(&argv(&["--check"])),
            Ok(LockMode::Check)
        ));
    }

    #[test]
    fn parse_lock_mode_recognizes_write() {
        assert!(matches!(
            parse_lock_mode(&argv(&["--write"])),
            Ok(LockMode::Write)
        ));
    }

    #[test]
    fn parse_lock_mode_rejects_unknown_flag() {
        assert!(parse_lock_mode(&argv(&["--frob"])).is_err());
    }

    /// A unique temp path for a lockfile fixture, so tests do not collide when
    /// run in parallel and leave nothing behind in the repo.
    fn temp_lock_path(tag: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        path.push(format!("graftx-xtask-opcodes-{tag}-{nanos}.lock"));
        path
    }

    #[test]
    fn write_then_check_roundtrips() {
        let path = temp_lock_path("roundtrip");
        assert_eq!(opcodes_lock(&argv(&["--write"]), &path), ExitCode::SUCCESS);
        // The freshly written file must satisfy a subsequent check.
        assert_eq!(opcodes_lock(&argv(&["--check"]), &path), ExitCode::SUCCESS);
        // Default (no flag) is also a check and must pass.
        assert_eq!(opcodes_lock(&[], &path), ExitCode::SUCCESS);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn check_fails_when_file_missing() {
        let path = temp_lock_path("missing");
        // Ensure the path does not exist before checking.
        let _ = std::fs::remove_file(&path);
        assert_eq!(opcodes_lock(&argv(&["--check"]), &path), ExitCode::FAILURE);
    }

    #[test]
    fn check_fails_when_content_differs() {
        let path = temp_lock_path("stale");
        std::fs::write(&path, "# stale\n0xFF_FFFFFF Bogus OPCODE\n").expect("seed stale lockfile");
        assert_eq!(opcodes_lock(&argv(&["--check"]), &path), ExitCode::FAILURE);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn opcodes_lock_rejects_unknown_flag() {
        let path = temp_lock_path("badflag");
        assert_eq!(
            opcodes_lock(&argv(&["--frob"]), &path),
            ExitCode::from(EXIT_USAGE)
        );
    }

    #[test]
    fn report_first_difference_names_the_line() {
        let mut out = Vec::new();
        report_first_difference(&mut out, "a\nb\nc\n", "a\nX\nc\n");
        let text = String::from_utf8(out).expect("utf8 report");
        assert!(text.contains("line 2"), "report was: {text}");
        assert!(text.contains("on disk:   b"), "report was: {text}");
        assert!(text.contains("expected:  X"), "report was: {text}");
    }

    #[test]
    fn report_first_difference_notes_length_mismatch() {
        let mut out = Vec::new();
        report_first_difference(&mut out, "a\nb\n", "a\nb\nc\n");
        let text = String::from_utf8(out).expect("utf8 report");
        assert!(text.contains("line count differs"), "report was: {text}");
    }

    #[test]
    fn verify_outcome_passes_only_when_both_pass() {
        // The whole point of the aggregate: Ok only when both sub-results are Ok.
        let both_ok = VerifyOutcome {
            xref_issues: 0,
            lock_up_to_date: true,
        };
        assert!(both_ok.passed());
        assert_eq!(both_ok.exit_code(), ExitCode::SUCCESS);

        let xref_fail = VerifyOutcome {
            xref_issues: 3,
            lock_up_to_date: true,
        };
        assert!(!xref_fail.passed());
        assert_eq!(xref_fail.exit_code(), ExitCode::FAILURE);

        let lock_fail = VerifyOutcome {
            xref_issues: 0,
            lock_up_to_date: false,
        };
        assert!(!lock_fail.passed());
        assert_eq!(lock_fail.exit_code(), ExitCode::FAILURE);

        let both_fail = VerifyOutcome {
            xref_issues: 2,
            lock_up_to_date: false,
        };
        assert!(!both_fail.passed());
        assert_eq!(both_fail.exit_code(), ExitCode::FAILURE);
    }

    #[test]
    fn verify_is_a_known_subcommand() {
        // Dispatched (not the usage error); pass/fail depends on the checkout.
        assert_ne!(run(&argv(&["verify"])), ExitCode::from(EXIT_USAGE));
    }

    #[test]
    fn verify_fails_when_lock_is_missing() {
        // A missing lockfile means opcodes-lock cannot pass, so verify must fail
        // even when the xref scan over a (nonexistent) docs dir is clean.
        let mut out = Vec::new();
        let code = verify(
            Path::new("definitely/not/a/real/docs/dir"),
            Path::new("definitely/not/a/real/opcodes.lock"),
            &mut out,
        );
        assert_eq!(code, ExitCode::FAILURE);
        let text = String::from_utf8(out).expect("utf8 report");
        assert!(text.contains("opcodes-lock: STALE"), "report was: {text}");
        assert!(text.contains("verify: FAILED"), "report was: {text}");
    }

    #[test]
    fn verify_summary_lists_both_checks() {
        // Drive verify with a clean docs dir and a freshly written lock so both
        // sub-checks pass; assert the summary names both and reports success.
        let lock = temp_lock_path("verify-ok");
        std::fs::write(&lock, opcodes::render_lock(opcodes::OPCODES))
            .expect("seed canonical lockfile");
        let mut out = Vec::new();
        let code = verify(Path::new("definitely/not/a/real/docs/dir"), &lock, &mut out);
        assert_eq!(code, ExitCode::SUCCESS);
        let text = String::from_utf8(out).expect("utf8 report");
        assert!(text.contains("verify: xrefs: 0 issue(s)"), "report: {text}");
        assert!(
            text.contains("verify: opcodes-lock: up-to-date"),
            "report: {text}"
        );
        assert!(text.contains("verify: OK"), "report: {text}");
        let _ = std::fs::remove_file(&lock);
    }
}
