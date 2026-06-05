//! Doc-index completeness check for `docs/design/`.
//!
//! `doc-index` guards against a design doc being added under `docs/design/` but
//! never linked from its `README.md` index, so the index stays a complete table
//! of contents. It lists the `*.md` files in `docs/design/` (excluding the
//! `README.md` itself), reads the README text, and flags any filename the README
//! does not reference.
//!
//! The decision — which filenames are *not* referenced in the README — is the
//! pure function [`missing_index_links`], so the unit tests can drive it directly
//! on string fixtures. The scan layer ([`check_doc_index`]) is the thin wrapper
//! that reads the directory and the README off disk and reports the result.

use std::fs;
use std::path::Path;

/// The README within `docs/design/` that indexes the sibling design docs; it is
/// itself excluded from the completeness check.
const README_NAME: &str = "README.md";

/// Return the design-doc filenames in `doc_filenames` that are *not* referenced
/// anywhere in `readme`, preserving the input order.
///
/// A filename counts as referenced when it appears as a substring of the README
/// text — the index links each doc as `](FILENAME.md)`, so a plain substring scan
/// matches the link target without having to parse Markdown. The README's own
/// entry ([`README_NAME`]) is skipped: an index need not link itself. Comparison
/// is case-sensitive, matching how the files are named and linked.
pub fn missing_index_links(readme: &str, doc_filenames: &[String]) -> Vec<String> {
    doc_filenames
        .iter()
        .filter(|name| name.as_str() != README_NAME)
        .filter(|name| !readme.contains(name.as_str()))
        .cloned()
        .collect()
}

/// List the immediate `*.md` filenames in `dir`, sorted for deterministic output.
///
/// Only the top level of `dir` is scanned: the design index lists the docs that
/// sit beside it, not the chapter files nested under `plan/`. Subdirectories are
/// ignored. A missing directory is surfaced as an error so the caller can report
/// it rather than silently passing.
fn list_markdown_filenames(dir: &Path) -> Result<Vec<String>, String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("dir entry under {}: {e}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("file_type {}: {e}", path.display()))?;
        if !file_type.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            names.push(name.to_string());
        }
    }
    names.sort();
    Ok(names)
}

/// Run the `doc-index` check over `design_dir`, writing a report to `out`, and
/// return the number of design docs missing from the README index.
///
/// Reads the `*.md` filenames in `design_dir` and the text of its `README.md`,
/// then defers the decision to [`missing_index_links`]. A scan or read failure is
/// reported and counted as one problem so CI gates on it, mirroring how the xref
/// linter treats a failed scan. Write failures to `out` are ignored: the returned
/// count, not the printed text, drives the exit code.
pub fn check_doc_index<W: std::io::Write>(design_dir: &Path, out: &mut W) -> usize {
    let filenames = match list_markdown_filenames(design_dir) {
        Ok(filenames) => filenames,
        Err(e) => {
            let _ = writeln!(
                out,
                "doc-index: failed to scan {}: {e}",
                design_dir.display()
            );
            return 1;
        }
    };

    let readme_path = design_dir.join(README_NAME);
    let readme = match fs::read_to_string(&readme_path) {
        Ok(readme) => readme,
        Err(e) => {
            let _ = writeln!(out, "doc-index: cannot read {}: {e}", readme_path.display());
            return 1;
        }
    };

    let missing = missing_index_links(&readme, &filenames);
    if missing.is_empty() {
        let _ = writeln!(
            out,
            "doc-index: all design docs indexed in {}",
            readme_path.display()
        );
    } else {
        let _ = writeln!(
            out,
            "doc-index: {} design doc(s) not linked from {}:",
            missing.len(),
            readme_path.display()
        );
        for name in &missing {
            let _ = writeln!(out, "  {name}");
        }
    }
    missing.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Turn a slice of string literals into the owned `Vec<String>` the pure
    /// function expects.
    fn names(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn all_linked_yields_no_missing() {
        let readme = "Index:\n- [PROTOCOL](PROTOCOL.md)\n- [TRANSPORT](TRANSPORT.md)\n";
        let docs = names(&["PROTOCOL.md", "TRANSPORT.md"]);
        assert!(missing_index_links(readme, &docs).is_empty());
    }

    #[test]
    fn unlinked_doc_is_reported() {
        let readme = "Index:\n- [PROTOCOL](PROTOCOL.md)\n";
        let docs = names(&["PROTOCOL.md", "TRANSPORT.md"]);
        assert_eq!(
            missing_index_links(readme, &docs),
            vec!["TRANSPORT.md".to_string()]
        );
    }

    #[test]
    fn readme_itself_is_never_flagged() {
        // The README need not link itself even though it is in the directory.
        let readme = "no links here";
        let docs = names(&["README.md"]);
        assert!(missing_index_links(readme, &docs).is_empty());
    }

    #[test]
    fn missing_preserves_input_order() {
        let readme = "links: [B](B.md)";
        // A, then C are missing; B is linked. Order must follow the input.
        let docs = names(&["A.md", "B.md", "C.md"]);
        assert_eq!(
            missing_index_links(readme, &docs),
            vec!["A.md".to_string(), "C.md".to_string()]
        );
    }

    #[test]
    fn matching_is_case_sensitive() {
        // A lowercased mention does not count as a link to the real filename.
        let readme = "see protocol.md for details";
        let docs = names(&["PROTOCOL.md"]);
        assert_eq!(
            missing_index_links(readme, &docs),
            vec!["PROTOCOL.md".to_string()]
        );
    }

    #[test]
    fn empty_doc_list_is_trivially_complete() {
        assert!(missing_index_links("anything", &[]).is_empty());
    }

    #[test]
    fn check_doc_index_reports_scan_failure() {
        let mut out = Vec::new();
        let count = check_doc_index(Path::new("definitely/not/a/real/design/dir"), &mut out);
        assert_eq!(count, 1);
        let text = String::from_utf8(out).expect("utf8 report");
        assert!(text.contains("failed to scan"), "report was: {text}");
    }
}
