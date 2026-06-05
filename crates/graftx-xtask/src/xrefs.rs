//! Cross-reference linter for the repo's Markdown docs.
//!
//! `check-xrefs` walks `docs/` and flags two kinds of stale cross-reference:
//!
//! * **Broken links** — a relative Markdown link `](target)` whose resolved
//!   path does not exist on disk. External links (`http://`, `https://`,
//!   `mailto:`) and pure in-page anchors (`#section`) are left alone.
//! * **Out-of-range chapter refs** — a `(Ch. NN)` reference whose number falls
//!   outside the design plan's chapter range (`1..=32`). The plan lives in
//!   `docs/design/plan/NN-*.md`; a ref past the end is almost always a typo or
//!   a chapter that was renumbered away.
//!
//! The scan layer (filesystem walking, link resolution) is deliberately thin so
//! the interesting parts — extracting links and chapter refs from Markdown — are
//! pure functions that the unit tests can drive directly on string fixtures.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// First valid chapter number in `docs/design/plan/`.
const MIN_CHAPTER: u32 = 1;
/// Last valid chapter number in `docs/design/plan/` (`32-appendices.md`).
const MAX_CHAPTER: u32 = 32;

/// A single problem found while linting one file.
///
/// Carries the source line (1-based) and the detail needed to render the report
/// row; the owning file is tracked separately so the report can group by file.
enum Issue {
    /// A relative link whose resolved target does not exist on disk.
    BrokenLink { line: usize, target: String },
    /// A `(Ch. NN)` reference outside [`MIN_CHAPTER`]..=[`MAX_CHAPTER`].
    OutOfRangeChapter { line: usize, chapter: u32 },
}

impl Issue {
    /// The 1-based source line this issue was found on.
    fn line(&self) -> usize {
        match self {
            Issue::BrokenLink { line, .. } => *line,
            Issue::OutOfRangeChapter { line, .. } => *line,
        }
    }

    /// One-line description for the report, without the file prefix.
    fn describe(&self) -> String {
        match self {
            Issue::BrokenLink { line, target } => {
                format!("line {line}: BROKEN LINK -> {target}")
            }
            Issue::OutOfRangeChapter { line, chapter } => {
                format!(
                    "line {line}: OUT-OF-RANGE chapter ref (Ch. {chapter:02}) (valid {MIN_CHAPTER}..={MAX_CHAPTER})"
                )
            }
        }
    }
}

/// Extract Markdown links from `md`, returning `(line, target)` pairs.
///
/// Recognizes the inline link form `](target)`: everything between the `](` and
/// the closing `)` is the target. The link text before `](` is ignored, which
/// is all we need for reference checking. `line` is 1-based.
///
/// Targets that span a balanced-paren title (e.g. `](path "a (b) c")`) are out
/// of scope: the first `)` ends the target, matching how a strict reader would
/// see a malformed link, which is exactly the sort of thing we want to flag.
pub fn extract_links(md: &str) -> Vec<(usize, String)> {
    let mut links = Vec::new();
    for (idx, line) in md.lines().enumerate() {
        let line_no = idx + 1;
        let bytes = line.as_bytes();
        let mut i = 0;
        while i + 1 < bytes.len() {
            if bytes[i] == b']' && bytes[i + 1] == b'(' {
                let start = i + 2;
                if let Some(rel) = line[start..].find(')') {
                    let end = start + rel;
                    links.push((line_no, line[start..end].to_string()));
                    // Resume after the closing paren so a later link on the same
                    // line is still seen.
                    i = end + 1;
                    continue;
                }
            }
            i += 1;
        }
    }
    links
}

/// Extract chapter references of the form `(Ch. NN)` / `(Ch.NN)` from `md`.
///
/// Returns `(line, chapter)` pairs with 1-based line numbers. The space after
/// `Ch.` is optional, and the number is taken to be the run of ASCII digits
/// immediately preceding the closing `)`. Numbers that overflow [`u32`] are
/// skipped rather than reported, since they cannot be a real chapter and are
/// vanishingly unlikely to appear in prose.
pub fn extract_chapter_refs(md: &str) -> Vec<(usize, u32)> {
    let mut refs = Vec::new();
    for (idx, line) in md.lines().enumerate() {
        let line_no = idx + 1;
        let bytes = line.as_bytes();
        let mut search_from = 0;
        while let Some(rel) = line[search_from..].find("(Ch.") {
            let open = search_from + rel;
            // Position just past the "(Ch." marker.
            let mut cursor = open + 4;
            // Allow a single optional space (matching both `(Ch. NN)` and
            // `(Ch.NN)`); any other layout simply will not parse a number.
            if cursor < bytes.len() && bytes[cursor] == b' ' {
                cursor += 1;
            }
            let digits_start = cursor;
            while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
                cursor += 1;
            }
            // A well-formed ref is `(Ch.<digits>)`: at least one digit followed
            // immediately by the closing paren.
            if cursor > digits_start && cursor < bytes.len() && bytes[cursor] == b')' {
                if let Ok(chapter) = line[digits_start..cursor].parse::<u32>() {
                    refs.push((line_no, chapter));
                }
            }
            // Advance past this `(Ch.` so overlapping/adjacent refs are scanned.
            search_from = open + 4;
        }
    }
    refs
}

/// Whether `chapter` is a valid design-plan chapter number.
fn chapter_in_range(chapter: u32) -> bool {
    (MIN_CHAPTER..=MAX_CHAPTER).contains(&chapter)
}

/// Whether a link `target` points outside the repo and so should not be checked
/// against the filesystem.
///
/// External schemes (`http`, `https`, `mailto`) and pure in-page anchors (`#…`)
/// are skipped. An empty target is also skipped: it carries no path to resolve.
fn is_external_or_anchor(target: &str) -> bool {
    target.is_empty()
        || target.starts_with("http://")
        || target.starts_with("https://")
        || target.starts_with("mailto:")
        || target.starts_with('#')
}

/// Resolve a relative link `target` against the directory of `from_file` and
/// report whether the result exists on disk.
///
/// Any `#anchor` suffix is stripped before resolving, since the anchor selects a
/// location *within* the target rather than part of its path. A target with no
/// path component left after stripping (a bare `#anchor`, already filtered by
/// [`is_external_or_anchor`]) is treated as existing.
fn link_target_exists(from_file: &Path, target: &str) -> bool {
    let path = target.split('#').next().unwrap_or(target);
    if path.is_empty() {
        return true;
    }
    let base = from_file.parent().unwrap_or_else(|| Path::new("."));
    base.join(path).exists()
}

/// Lint the Markdown text of a single file at `path`, returning the issues found.
///
/// `path` is used both to resolve relative links and (by the caller) to label
/// the report; this function does no filesystem access of its own beyond the
/// existence checks for link targets.
fn lint_markdown(path: &Path, contents: &str) -> Vec<Issue> {
    let mut issues = Vec::new();

    for (line, target) in extract_links(contents) {
        if is_external_or_anchor(&target) {
            continue;
        }
        if !link_target_exists(path, &target) {
            issues.push(Issue::BrokenLink { line, target });
        }
    }

    for (line, chapter) in extract_chapter_refs(contents) {
        if !chapter_in_range(chapter) {
            issues.push(Issue::OutOfRangeChapter { line, chapter });
        }
    }

    // Stable, line-ordered output regardless of which pass produced the issue.
    issues.sort_by_key(Issue::line);
    issues
}

/// Recursively collect `*.md` files under `dir`, sorted for deterministic output.
///
/// Errors reading a directory entry are surfaced to the caller as a single
/// message string so the command can decide how to report them; a missing
/// `docs/` directory is treated as "no files" rather than an error, since a
/// fresh checkout without docs should not fail the linter.
fn collect_markdown_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    if !dir.exists() {
        return Ok(files);
    }
    collect_into(dir, &mut files)?;
    files.sort();
    Ok(files)
}

/// Depth-first helper for [`collect_markdown_files`].
fn collect_into(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("dir entry under {}: {e}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("file_type {}: {e}", path.display()))?;
        if file_type.is_dir() {
            collect_into(&path, out)?;
        } else if file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(path);
        }
    }
    Ok(())
}

/// Run the `check-xrefs` linter over `docs_dir`, writing a grouped report to
/// `out`, and return the total number of issues found.
///
/// Write failures to `out` are ignored for the same reason the usage banner
/// ignores them: there is nothing useful to do if reporting itself fails, and
/// the issue count — not the printed text — drives the exit code.
pub fn check_xrefs<W: std::io::Write>(docs_dir: &Path, out: &mut W) -> usize {
    let files = match collect_markdown_files(docs_dir) {
        Ok(files) => files,
        Err(e) => {
            let _ = writeln!(
                out,
                "check-xrefs: failed to scan {}: {e}",
                docs_dir.display()
            );
            // A scan failure is itself a problem CI should gate on.
            return 1;
        }
    };

    let mut total = 0usize;
    let mut files_with_issues = 0usize;
    let mut report = String::new();

    for path in &files {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(e) => {
                let _ = writeln!(report, "{}:", path.display());
                let _ = writeln!(report, "  read error: {e}");
                total += 1;
                files_with_issues += 1;
                continue;
            }
        };
        let issues = lint_markdown(path, &contents);
        if issues.is_empty() {
            continue;
        }
        files_with_issues += 1;
        let _ = writeln!(report, "{}:", path.display());
        for issue in &issues {
            let _ = writeln!(report, "  {}", issue.describe());
        }
        total += issues.len();
    }

    let _ = write!(out, "{report}");
    if total == 0 {
        let _ = writeln!(
            out,
            "check-xrefs: scanned {} file(s), no issues found",
            files.len()
        );
    } else {
        let _ = writeln!(
            out,
            "check-xrefs: {total} issue(s) across {files_with_issues} file(s) ({} scanned)",
            files.len()
        );
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_links_finds_inline_target() {
        let md = "see [the plan](plan/06-protocol.md) for details";
        assert_eq!(
            extract_links(md),
            vec![(1, "plan/06-protocol.md".to_string())]
        );
    }

    #[test]
    fn extract_links_reports_line_numbers() {
        let md = "intro\n[a](one.md)\n\n[b](two.md)\n";
        assert_eq!(
            extract_links(md),
            vec![(2, "one.md".to_string()), (4, "two.md".to_string())]
        );
    }

    #[test]
    fn extract_links_handles_multiple_links_on_one_line() {
        let md = "[a](a.md) and [b](b.md)";
        assert_eq!(
            extract_links(md),
            vec![(1, "a.md".to_string()), (1, "b.md".to_string())]
        );
    }

    #[test]
    fn extract_links_keeps_anchor_and_external_targets() {
        // Extraction is dumb on purpose; filtering happens later.
        let md = "[x](https://example.com) [y](#section) [z](../README.md)";
        assert_eq!(
            extract_links(md),
            vec![
                (1, "https://example.com".to_string()),
                (1, "#section".to_string()),
                (1, "../README.md".to_string()),
            ]
        );
    }

    #[test]
    fn extract_links_ignores_unclosed_paren() {
        let md = "[broken](no-close";
        assert!(extract_links(md).is_empty());
    }

    #[test]
    fn extract_chapter_refs_handles_both_spacings() {
        let md = "transport (Ch. 08) and protocol (Ch.06) and appendices (Ch. 32)";
        assert_eq!(extract_chapter_refs(md), vec![(1, 8), (1, 6), (1, 32)]);
    }

    #[test]
    fn extract_chapter_refs_reports_line_numbers() {
        let md = "first\nsee (Ch. 13)\nlast (Ch.99)\n";
        assert_eq!(extract_chapter_refs(md), vec![(2, 13), (3, 99)]);
    }

    #[test]
    fn extract_chapter_refs_ignores_malformed() {
        // No digits, trailing text before the paren, and a lone marker.
        let md = "(Ch. ) (Ch. 7x) (Ch.) plain (Ch";
        assert!(extract_chapter_refs(md).is_empty());
    }

    #[test]
    fn chapter_range_check_bounds() {
        assert!(!chapter_in_range(0));
        assert!(chapter_in_range(1));
        assert!(chapter_in_range(32));
        assert!(!chapter_in_range(33));
        assert!(!chapter_in_range(99));
    }

    #[test]
    fn external_and_anchor_targets_are_skipped() {
        assert!(is_external_or_anchor("http://x"));
        assert!(is_external_or_anchor("https://x"));
        assert!(is_external_or_anchor("mailto:a@b.c"));
        assert!(is_external_or_anchor("#frag"));
        assert!(is_external_or_anchor(""));
        assert!(!is_external_or_anchor("../README.md"));
        assert!(!is_external_or_anchor("plan/06-protocol.md"));
    }

    #[test]
    fn link_target_strips_anchor_before_resolving() {
        // The anchor must not become part of the path; resolving against a
        // nonexistent base still yields "missing", which is the point here:
        // the function should look for `missing.md`, not `missing.md#x`.
        let from = Path::new("docs/design/README.md");
        assert!(!link_target_exists(
            from,
            "definitely-missing-file.md#anchor"
        ));
    }

    #[test]
    fn lint_markdown_flags_out_of_range_chapter() {
        let path = Path::new("docs/sample.md");
        let issues = lint_markdown(path, "ok (Ch. 06)\nbad (Ch. 40)\n");
        assert_eq!(issues.len(), 1);
        match &issues[0] {
            Issue::OutOfRangeChapter { line, chapter } => {
                assert_eq!(*line, 2);
                assert_eq!(*chapter, 40);
            }
            other => panic!("expected out-of-range chapter, got {}", other.describe()),
        }
    }

    #[test]
    fn lint_markdown_flags_broken_relative_link() {
        let path = Path::new("docs/sample.md");
        let issues = lint_markdown(path, "[gone](this-target-does-not-exist.md)\n");
        assert_eq!(issues.len(), 1);
        match &issues[0] {
            Issue::BrokenLink { line, target } => {
                assert_eq!(*line, 1);
                assert_eq!(target, "this-target-does-not-exist.md");
            }
            other => panic!("expected broken link, got {}", other.describe()),
        }
    }

    #[test]
    fn lint_markdown_orders_issues_by_line() {
        let path = Path::new("docs/sample.md");
        // Chapter ref on line 1, broken link on line 2: report must list line 1
        // first even though links are collected before chapter refs.
        let md = "bad (Ch. 40)\n[gone](this-target-does-not-exist.md)\n";
        let issues = lint_markdown(path, md);
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].line(), 1);
        assert_eq!(issues[1].line(), 2);
    }

    #[test]
    fn missing_docs_dir_reports_no_issues() {
        let mut out = Vec::new();
        let count = check_xrefs(Path::new("definitely/not/a/real/docs/dir"), &mut out);
        assert_eq!(count, 0);
        let text = String::from_utf8(out).expect("utf8 report");
        assert!(text.contains("no issues found"), "report was: {text}");
    }
}
