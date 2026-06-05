//! Opcode-table source of truth and Markdown renderer.
//!
//! The entries here mirror the opcode constants defined in `graftx-protocol`
//! (`core_op::*`, `vk_op::*`, `gl_op::*`, `cuda_op::*`, `cl_op::*`, `hip_op::*`,
//! `l0_op::*`, `video_op::*`, `wgpu_op::*`, `optix_op::*`, `sycl_op::*`, and
//! `amf_op::*`). They are duplicated rather than
//! imported so
//! that `gen-opcodes` stays a leaf tool with no dependency on the protocol
//! crate; the `check-xrefs` companion is responsible for catching drift.
//!
//! Each entry pairs a human-facing API name with its numeric [`ApiId`] byte and
//! a 24-bit call id. The full opcode is `(api_id << 24) | call_id`, matching the
//! `opcode()` helper in `graftx-protocol`.

use std::path::Path;

/// A single opcode, with enough context to render one table row.
pub struct OpcodeEntry {
    /// Display name of the owning API (the `API` column).
    pub api_name: &'static str,
    /// High byte of the opcode: the API discriminant.
    pub api_id: u8,
    /// Display name of the call (the `Name` column).
    pub name: &'static str,
    /// Low 24 bits of the opcode: the call id within the API.
    pub call_id: u32,
}

impl OpcodeEntry {
    /// The full 32-bit opcode: API byte in the high 8 bits, call id in the low
    /// 24. The call id is masked to 24 bits so a malformed entry cannot bleed
    /// into the API byte.
    pub const fn opcode(&self) -> u32 {
        ((self.api_id as u32) << 24) | (self.call_id & 0x00FF_FFFF)
    }
}

/// The canonical opcode list, in the order it should appear in the table:
/// Core opcodes first, then Vulkan, OpenGL, CUDA, OpenCL, HIP, Level Zero,
/// Video, WebGPU, OptiX, SYCL, and AMF, each in ascending call-id order within
/// its API.
pub const OPCODES: &[OpcodeEntry] = &[
    OpcodeEntry {
        api_name: "Core",
        api_id: 0x00,
        name: "HELLO",
        call_id: 0x000001,
    },
    OpcodeEntry {
        api_name: "Core",
        api_id: 0x00,
        name: "WELCOME",
        call_id: 0x000002,
    },
    OpcodeEntry {
        api_name: "Core",
        api_id: 0x00,
        name: "NOOP",
        call_id: 0x000003,
    },
    OpcodeEntry {
        api_name: "Vulkan",
        api_id: 0x01,
        name: "CREATE_INSTANCE",
        call_id: 0x0001,
    },
    OpcodeEntry {
        api_name: "Vulkan",
        api_id: 0x01,
        name: "DESTROY_INSTANCE",
        call_id: 0x0002,
    },
    OpcodeEntry {
        api_name: "Vulkan",
        api_id: 0x01,
        name: "ENUMERATE_PHYSICAL_DEVICES",
        call_id: 0x0003,
    },
    OpcodeEntry {
        api_name: "Vulkan",
        api_id: 0x01,
        name: "GET_PHYSICAL_DEVICE_PROPERTIES",
        call_id: 0x0004,
    },
    OpcodeEntry {
        api_name: "Vulkan",
        api_id: 0x01,
        name: "CREATE_DEVICE",
        call_id: 0x0005,
    },
    OpcodeEntry {
        api_name: "Vulkan",
        api_id: 0x01,
        name: "DESTROY_DEVICE",
        call_id: 0x0006,
    },
    OpcodeEntry {
        api_name: "Vulkan",
        api_id: 0x01,
        name: "GET_DEVICE_QUEUE",
        call_id: 0x0007,
    },
    OpcodeEntry {
        api_name: "Vulkan",
        api_id: 0x01,
        name: "DEVICE_WAIT_IDLE",
        call_id: 0x0008,
    },
    OpcodeEntry {
        api_name: "Vulkan",
        api_id: 0x01,
        name: "ALLOCATE_MEMORY",
        call_id: 0x0010,
    },
    OpcodeEntry {
        api_name: "Vulkan",
        api_id: 0x01,
        name: "CREATE_BUFFER",
        call_id: 0x0011,
    },
    OpcodeEntry {
        api_name: "Vulkan",
        api_id: 0x01,
        name: "BIND_BUFFER_MEMORY",
        call_id: 0x0012,
    },
    OpcodeEntry {
        api_name: "OpenGl",
        api_id: 0x02,
        name: "CREATE_CONTEXT",
        call_id: 0x0001,
    },
    OpcodeEntry {
        api_name: "OpenGl",
        api_id: 0x02,
        name: "MAKE_CURRENT",
        call_id: 0x0002,
    },
    OpcodeEntry {
        api_name: "OpenGl",
        api_id: 0x02,
        name: "GEN_BUFFER",
        call_id: 0x0003,
    },
    OpcodeEntry {
        api_name: "Cuda",
        api_id: 0x03,
        name: "CTX_CREATE",
        call_id: 0x0001,
    },
    OpcodeEntry {
        api_name: "Cuda",
        api_id: 0x03,
        name: "MEM_ALLOC",
        call_id: 0x0002,
    },
    OpcodeEntry {
        api_name: "Cuda",
        api_id: 0x03,
        name: "MEM_FREE",
        call_id: 0x0003,
    },
    OpcodeEntry {
        api_name: "OpenCl",
        api_id: 0x04,
        name: "CREATE_CONTEXT",
        call_id: 0x0001,
    },
    OpcodeEntry {
        api_name: "OpenCl",
        api_id: 0x04,
        name: "CREATE_BUFFER",
        call_id: 0x0002,
    },
    OpcodeEntry {
        api_name: "OpenCl",
        api_id: 0x04,
        name: "RELEASE_BUFFER",
        call_id: 0x0003,
    },
    OpcodeEntry {
        api_name: "Hip",
        api_id: 0x05,
        name: "MALLOC",
        call_id: 0x0001,
    },
    OpcodeEntry {
        api_name: "Hip",
        api_id: 0x05,
        name: "FREE",
        call_id: 0x0002,
    },
    OpcodeEntry {
        api_name: "Hip",
        api_id: 0x05,
        name: "STREAM_CREATE",
        call_id: 0x0003,
    },
    OpcodeEntry {
        api_name: "LevelZero",
        api_id: 0x06,
        name: "CONTEXT_CREATE",
        call_id: 0x0001,
    },
    OpcodeEntry {
        api_name: "LevelZero",
        api_id: 0x06,
        name: "MEM_ALLOC_DEVICE",
        call_id: 0x0002,
    },
    OpcodeEntry {
        api_name: "LevelZero",
        api_id: 0x06,
        name: "MEM_FREE",
        call_id: 0x0003,
    },
    OpcodeEntry {
        api_name: "Video",
        api_id: 0x07,
        name: "CREATE_DECODE_SESSION",
        call_id: 0x0001,
    },
    OpcodeEntry {
        api_name: "Video",
        api_id: 0x07,
        name: "DECODE_FRAME",
        call_id: 0x0002,
    },
    OpcodeEntry {
        api_name: "Video",
        api_id: 0x07,
        name: "DESTROY_SESSION",
        call_id: 0x0003,
    },
    OpcodeEntry {
        api_name: "WebGpu",
        api_id: 0x08,
        name: "REQUEST_DEVICE",
        call_id: 0x0001,
    },
    OpcodeEntry {
        api_name: "WebGpu",
        api_id: 0x08,
        name: "CREATE_BUFFER",
        call_id: 0x0002,
    },
    OpcodeEntry {
        api_name: "WebGpu",
        api_id: 0x08,
        name: "DESTROY_BUFFER",
        call_id: 0x0003,
    },
    OpcodeEntry {
        api_name: "OptiX",
        api_id: 0x09,
        name: "CONTEXT_CREATE",
        call_id: 0x0001,
    },
    OpcodeEntry {
        api_name: "OptiX",
        api_id: 0x09,
        name: "PIPELINE_CREATE",
        call_id: 0x0002,
    },
    OpcodeEntry {
        api_name: "OptiX",
        api_id: 0x09,
        name: "DESTROY",
        call_id: 0x0003,
    },
    OpcodeEntry {
        api_name: "Sycl",
        api_id: 0x0A,
        name: "QUEUE_CREATE",
        call_id: 0x0001,
    },
    OpcodeEntry {
        api_name: "Sycl",
        api_id: 0x0A,
        name: "MALLOC_DEVICE",
        call_id: 0x0002,
    },
    OpcodeEntry {
        api_name: "Sycl",
        api_id: 0x0A,
        name: "FREE",
        call_id: 0x0003,
    },
    OpcodeEntry {
        api_name: "Amf",
        api_id: 0x0B,
        name: "CREATE_ENCODER",
        call_id: 0x0001,
    },
    OpcodeEntry {
        api_name: "Amf",
        api_id: 0x0B,
        name: "ENCODE_FRAME",
        call_id: 0x0002,
    },
    OpcodeEntry {
        api_name: "Amf",
        api_id: 0x0B,
        name: "DESTROY_ENCODER",
        call_id: 0x0003,
    },
];

/// Format a single opcode as `0x%02X_%06X`: the API byte, an underscore, then
/// the 24-bit call id (e.g. `0x01_000003`).
fn format_opcode(entry: &OpcodeEntry) -> String {
    // The low 24 bits of the packed opcode are the call id; `opcode()` already
    // masks them, so reading them back keeps the displayed call id consistent
    // with the full opcode even if an entry's `call_id` were over-wide.
    let call = entry.opcode() & 0x00FF_FFFF;
    format!("0x{:02X}_{:06X}", entry.api_id, call)
}

/// Render `entries` as a Markdown table with `| Opcode | API | Name |` columns.
///
/// Pure and deterministic: the output preserves the order of `entries` and ends
/// with a trailing newline so it composes cleanly when written to a file or
/// stdout.
pub fn render_opcode_table(entries: &[OpcodeEntry]) -> String {
    let mut out = String::new();
    out.push_str("| Opcode | API | Name |\n");
    out.push_str("| --- | --- | --- |\n");
    for entry in entries {
        out.push_str(&format!(
            "| {} | {} | {} |\n",
            format_opcode(entry),
            entry.api_name,
            entry.name
        ));
    }
    out
}

/// Render a Markdown coverage roll-up: one row per distinct `api_name` with the
/// number of opcodes recorded for it, followed by a `TOTAL` row.
///
/// Pure and deterministic. APIs are listed in a stable order — sorted by
/// `api_id`, ties broken by first appearance in `entries` — so the output does
/// not depend on hash iteration order and is reproducible across runs. The
/// output ends with a trailing newline so it composes cleanly with files or
/// stdout.
pub fn render_coverage(entries: &[OpcodeEntry]) -> String {
    // Accumulate per-API counts while remembering the order in which each
    // (api_id, api_name) pair was first seen, then sort by api_id keeping that
    // first-seen order as the tie-breaker.
    let mut apis: Vec<(u8, &'static str, usize)> = Vec::new();
    for entry in entries {
        if let Some(slot) = apis
            .iter_mut()
            .find(|(id, name, _)| *id == entry.api_id && *name == entry.api_name)
        {
            slot.2 += 1;
        } else {
            apis.push((entry.api_id, entry.api_name, 1));
        }
    }
    // `sort_by_key` is stable, so equal `api_id`s keep their first-seen order.
    apis.sort_by_key(|(id, _, _)| *id);

    let mut out = String::new();
    out.push_str("| API | Opcodes |\n");
    out.push_str("| --- | --- |\n");
    let mut total = 0usize;
    for (_, name, count) in &apis {
        out.push_str(&format!("| {name} | {count} |\n"));
        total += *count;
    }
    out.push_str(&format!("| TOTAL | {total} |\n"));
    out
}

/// Render a compact Markdown stats block summarizing `entries`: the total
/// opcode count, the number of distinct APIs, and the per-API coverage table.
///
/// Pure and deterministic. The summary line is followed by the same per-API
/// roll-up [`render_coverage`] produces, so the two stay consistent and the
/// per-API ordering rule (by `api_id`, ties by first appearance) is shared
/// rather than duplicated. "Distinct APIs" counts unique `(api_id, api_name)`
/// pairs, matching the rows [`render_coverage`] emits. The output ends with a
/// trailing newline so it composes cleanly with files or stdout.
pub fn render_stats(entries: &[OpcodeEntry]) -> String {
    let total = entries.len();
    // Count distinct (api_id, api_name) pairs the same way `render_coverage`
    // groups them, so "distinct APIs" equals the number of per-API rows below.
    let mut seen: Vec<(u8, &'static str)> = Vec::new();
    for entry in entries {
        let key = (entry.api_id, entry.api_name);
        if !seen.contains(&key) {
            seen.push(key);
        }
    }
    let distinct_apis = seen.len();

    let mut out = String::new();
    out.push_str("# Opcode stats\n\n");
    out.push_str(&format!("- Total opcodes: {total}\n"));
    out.push_str(&format!("- Distinct APIs: {distinct_apis}\n\n"));
    out.push_str(&render_coverage(entries));
    out
}

/// Header comment written at the top of the opcode lockfile.
///
/// The lockfile is generated; the comment says so and points at the command
/// that regenerates it, so a reader who stumbles on a diff knows not to hand-edit
/// it. Each line is a `#` comment, matching the lock-line body that follows.
const LOCK_HEADER: &str = "\
# GraftX opcode lock — generated by `cargo xtask opcodes-lock --write`.
# One line per opcode: `0x<api>_<call> <API> <NAME>`, sorted by (api_id, call_id).
# Do not edit by hand; renumbering an opcode here will fail `opcodes-lock --check`.";

/// Render `entries` as a stable opcode lockfile.
///
/// Each opcode becomes one line `0x%02X_%06X <api_name> <name>` (e.g.
/// `0x01_000003 Vulkan ENUMERATE_PHYSICAL_DEVICES`), preceded by the
/// [`LOCK_HEADER`] comment. Lines are sorted by `(api_id, call_id)` so the file
/// is independent of the order of `entries`, and the output ends with a trailing
/// newline. The sort gives a canonical form: freezing it and checking against it
/// catches an accidental opcode renumbering as a diff.
pub fn render_lock(entries: &[OpcodeEntry]) -> String {
    // Sort by (api_id, call_id) without disturbing `entries`; the lockfile's
    // whole point is a canonical order that does not depend on the source list.
    let mut sorted: Vec<&OpcodeEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| (e.api_id, e.opcode() & 0x00FF_FFFF));

    let mut out = String::new();
    out.push_str(LOCK_HEADER);
    out.push('\n');
    for entry in sorted {
        out.push_str(&format!(
            "{} {} {}\n",
            format_opcode(entry),
            entry.api_name,
            entry.name
        ));
    }
    out
}

/// Outcome of verifying the opcode lockfile against the canonical render.
///
/// This separates *deciding* whether the lock is current from *reporting* it, so
/// both the `opcodes-lock --check` CLI arm and the `verify` aggregator can reuse
/// the same comparison without duplicating the read-and-compare logic.
pub enum LockStatus {
    /// The file on disk matches the canonical render byte-for-byte.
    UpToDate,
    /// The file exists but differs; carries the on-disk and expected text so the
    /// caller can render a diff hint.
    Stale { on_disk: String, expected: String },
    /// The file could not be read (missing or otherwise inaccessible); carries
    /// the I/O error message for the caller to surface.
    Unreadable { error: String },
}

impl LockStatus {
    /// Whether the lock is current. `verify` and the CLI both gate on this.
    pub fn is_up_to_date(&self) -> bool {
        matches!(self, LockStatus::UpToDate)
    }
}

/// Verify the lockfile at `lock_path` against the canonical render of `entries`.
///
/// Reads the file and compares it to [`render_lock`], returning a [`LockStatus`]
/// that the caller turns into output and an exit code. This does no printing of
/// its own, which is what lets the `opcodes-lock --check` arm and the `verify`
/// aggregator share one source of truth for the comparison.
pub fn check_lock(lock_path: &Path, entries: &[OpcodeEntry]) -> LockStatus {
    let expected = render_lock(entries);
    match std::fs::read_to_string(lock_path) {
        Ok(on_disk) if on_disk == expected => LockStatus::UpToDate,
        Ok(on_disk) => LockStatus::Stale { on_disk, expected },
        Err(e) => LockStatus::Unreadable {
            error: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcode_packs_api_byte_and_call_id() {
        let entry = OpcodeEntry {
            api_name: "Vulkan",
            api_id: 0x01,
            name: "ENUMERATE_PHYSICAL_DEVICES",
            call_id: 0x000003,
        };
        assert_eq!(entry.opcode(), 0x0100_0003);
    }

    #[test]
    fn format_opcode_uses_padded_hex_layout() {
        let entry = OpcodeEntry {
            api_name: "Core",
            api_id: 0x00,
            name: "HELLO",
            call_id: 0x000001,
        };
        assert_eq!(format_opcode(&entry), "0x00_000001");
    }

    #[test]
    fn table_has_header_and_separator() {
        let table = render_opcode_table(OPCODES);
        let mut lines = table.lines();
        assert_eq!(lines.next(), Some("| Opcode | API | Name |"));
        assert_eq!(lines.next(), Some("| --- | --- | --- |"));
    }

    #[test]
    fn table_contains_known_vulkan_row() {
        let table = render_opcode_table(OPCODES);
        assert!(
            table.contains("| 0x01_000003 | Vulkan | ENUMERATE_PHYSICAL_DEVICES |"),
            "missing ENUMERATE_PHYSICAL_DEVICES row in:\n{table}"
        );
    }

    #[test]
    fn table_contains_known_core_row() {
        let table = render_opcode_table(OPCODES);
        assert!(
            table.contains("| 0x00_000001 | Core | HELLO |"),
            "missing HELLO row in:\n{table}"
        );
    }

    #[test]
    fn table_has_one_data_row_per_entry() {
        let table = render_opcode_table(OPCODES);
        // Total lines = header + separator + one row per entry, and every line
        // is a table row (trailing newline does not yield an empty `lines()`
        // entry).
        let data_rows = table.lines().count() - 2;
        assert_eq!(data_rows, OPCODES.len());
        assert_eq!(data_rows, 44);
    }

    #[test]
    fn rows_preserve_entry_order() {
        let table = render_opcode_table(OPCODES);
        let hello = table.find("HELLO").expect("HELLO present");
        let create_instance = table
            .find("CREATE_INSTANCE")
            .expect("CREATE_INSTANCE present");
        assert!(
            hello < create_instance,
            "Core rows should precede Vulkan rows"
        );
    }

    #[test]
    fn coverage_counts_per_api_and_total_on_fixture() {
        let fixture = &[
            OpcodeEntry {
                api_name: "Core",
                api_id: 0x00,
                name: "HELLO",
                call_id: 0x000001,
            },
            OpcodeEntry {
                api_name: "Core",
                api_id: 0x00,
                name: "WELCOME",
                call_id: 0x000002,
            },
            OpcodeEntry {
                api_name: "Vulkan",
                api_id: 0x01,
                name: "CREATE_INSTANCE",
                call_id: 0x0001,
            },
        ];
        let table = render_coverage(fixture);
        assert_eq!(
            table,
            "| API | Opcodes |\n| --- | --- |\n| Core | 2 |\n| Vulkan | 1 |\n| TOTAL | 3 |\n"
        );
    }

    #[test]
    fn coverage_has_header_and_separator() {
        let table = render_coverage(OPCODES);
        let mut lines = table.lines();
        assert_eq!(lines.next(), Some("| API | Opcodes |"));
        assert_eq!(lines.next(), Some("| --- | --- |"));
    }

    #[test]
    fn coverage_orders_apis_by_api_id() {
        let table = render_coverage(OPCODES);
        let core = table.find("| Core |").expect("Core row present");
        let vulkan = table.find("| Vulkan |").expect("Vulkan row present");
        let opengl = table.find("| OpenGl |").expect("OpenGl row present");
        let cuda = table.find("| Cuda |").expect("Cuda row present");
        let opencl = table.find("| OpenCl |").expect("OpenCl row present");
        let hip = table.find("| Hip |").expect("Hip row present");
        let level_zero = table.find("| LevelZero |").expect("LevelZero row present");
        let video = table.find("| Video |").expect("Video row present");
        let webgpu = table.find("| WebGpu |").expect("WebGpu row present");
        let optix = table.find("| OptiX |").expect("OptiX row present");
        let sycl = table.find("| Sycl |").expect("Sycl row present");
        let amf = table.find("| Amf |").expect("Amf row present");
        let total = table.find("| TOTAL |").expect("TOTAL row present");
        assert!(
            core < vulkan
                && vulkan < opengl
                && opengl < cuda
                && cuda < opencl
                && opencl < hip
                && hip < level_zero
                && level_zero < video
                && video < webgpu
                && webgpu < optix
                && optix < sycl
                && sycl < amf
                && amf < total,
            "rows should be ordered by api_id (Core, Vulkan, OpenGl, Cuda, \
             OpenCl, Hip, LevelZero, Video, WebGpu, OptiX, Sycl, Amf, TOTAL):\n{table}"
        );
    }

    #[test]
    fn coverage_total_equals_entry_count() {
        let table = render_coverage(OPCODES);
        assert!(
            table.contains(&format!("| TOTAL | {} |", OPCODES.len())),
            "TOTAL should equal the number of opcodes ({}):\n{table}",
            OPCODES.len()
        );
    }

    #[test]
    fn real_coverage_includes_all_apis_with_expected_minimum_counts() {
        let mut counts: std::collections::HashMap<&'static str, usize> =
            std::collections::HashMap::new();
        for entry in OPCODES {
            *counts.entry(entry.api_name).or_insert(0) += 1;
        }
        // Core plus the opcodes added for each API; counts must be at least the
        // numbers introduced here (later additions only grow them).
        assert!(*counts.get("Core").unwrap_or(&0) >= 3, "Core opcodes");
        assert!(*counts.get("Vulkan").unwrap_or(&0) >= 11, "Vulkan opcodes");
        assert!(*counts.get("OpenGl").unwrap_or(&0) >= 3, "OpenGl opcodes");
        assert!(*counts.get("Cuda").unwrap_or(&0) >= 3, "Cuda opcodes");
        assert!(*counts.get("OpenCl").unwrap_or(&0) >= 3, "OpenCl opcodes");
        assert!(*counts.get("Hip").unwrap_or(&0) >= 3, "Hip opcodes");
        assert!(
            *counts.get("LevelZero").unwrap_or(&0) >= 3,
            "LevelZero opcodes"
        );
        assert!(*counts.get("Video").unwrap_or(&0) >= 3, "Video opcodes");
        assert!(*counts.get("WebGpu").unwrap_or(&0) >= 3, "WebGpu opcodes");
        assert!(*counts.get("OptiX").unwrap_or(&0) >= 3, "OptiX opcodes");
        assert!(*counts.get("Sycl").unwrap_or(&0) >= 3, "Sycl opcodes");
        assert!(*counts.get("Amf").unwrap_or(&0) >= 3, "Amf opcodes");
    }

    #[test]
    fn stats_reports_total_and_distinct_apis_on_fixture() {
        let fixture = &[
            OpcodeEntry {
                api_name: "Core",
                api_id: 0x00,
                name: "HELLO",
                call_id: 0x000001,
            },
            OpcodeEntry {
                api_name: "Core",
                api_id: 0x00,
                name: "WELCOME",
                call_id: 0x000002,
            },
            OpcodeEntry {
                api_name: "Vulkan",
                api_id: 0x01,
                name: "CREATE_INSTANCE",
                call_id: 0x0001,
            },
        ];
        let stats = render_stats(fixture);
        assert!(stats.contains("- Total opcodes: 3"), "stats:\n{stats}");
        assert!(stats.contains("- Distinct APIs: 2"), "stats:\n{stats}");
        // The coverage roll-up is embedded, so the TOTAL row matches the count.
        assert!(stats.contains("| TOTAL | 3 |"), "stats:\n{stats}");
    }

    #[test]
    fn stats_total_and_distinct_match_real_opcodes() {
        let stats = render_stats(OPCODES);
        assert!(
            stats.contains(&format!("- Total opcodes: {}", OPCODES.len())),
            "total should equal the opcode count ({}):\n{stats}",
            OPCODES.len()
        );
        // Core, Vulkan, OpenGl, Cuda, OpenCl, Hip, LevelZero, Video, WebGpu,
        // OptiX, Sycl, Amf — twelve distinct APIs in the canonical list.
        assert!(stats.contains("- Distinct APIs: 12"), "stats:\n{stats}");
    }

    #[test]
    fn stats_ends_with_newline() {
        assert!(render_stats(OPCODES).ends_with('\n'));
    }

    #[test]
    fn lock_starts_with_header_comment() {
        let lock = render_lock(OPCODES);
        assert!(
            lock.starts_with("# GraftX opcode lock"),
            "lockfile should open with the generated-file header:\n{lock}"
        );
        // Every header line is a comment, and the body that follows is not.
        assert!(
            lock.contains("opcodes-lock --write"),
            "header names the command"
        );
    }

    #[test]
    fn lock_line_format_is_opcode_api_name() {
        let lock = render_lock(OPCODES);
        assert!(
            lock.contains("\n0x01_000003 Vulkan ENUMERATE_PHYSICAL_DEVICES\n"),
            "missing canonical Vulkan line in:\n{lock}"
        );
        assert!(
            lock.contains("\n0x00_000001 Core HELLO\n"),
            "missing canonical Core line in:\n{lock}"
        );
    }

    #[test]
    fn lock_ends_with_newline() {
        assert!(render_lock(OPCODES).ends_with('\n'));
    }

    #[test]
    fn lock_orders_lines_by_api_id_then_call_id() {
        // A fixture deliberately out of (api_id, call_id) order; the lockfile
        // must still emit Core-then-Vulkan, ascending call id within each API.
        let fixture = &[
            OpcodeEntry {
                api_name: "Vulkan",
                api_id: 0x01,
                name: "DESTROY_INSTANCE",
                call_id: 0x0002,
            },
            OpcodeEntry {
                api_name: "Vulkan",
                api_id: 0x01,
                name: "CREATE_INSTANCE",
                call_id: 0x0001,
            },
            OpcodeEntry {
                api_name: "Core",
                api_id: 0x00,
                name: "HELLO",
                call_id: 0x000001,
            },
        ];
        let lock = render_lock(fixture);
        let body: Vec<&str> = lock.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(
            body,
            vec![
                "0x00_000001 Core HELLO",
                "0x01_000001 Vulkan CREATE_INSTANCE",
                "0x01_000002 Vulkan DESTROY_INSTANCE",
            ]
        );
    }

    #[test]
    fn lock_is_independent_of_entry_order() {
        // Reversing the source slice must not change the rendered lockfile: the
        // canonical sort is what makes the lock a stable freeze.
        let reversed: Vec<OpcodeEntry> = OPCODES
            .iter()
            .rev()
            .map(|e| OpcodeEntry {
                api_name: e.api_name,
                api_id: e.api_id,
                name: e.name,
                call_id: e.call_id,
            })
            .collect();
        assert_eq!(render_lock(OPCODES), render_lock(&reversed));
    }

    #[test]
    fn lock_has_one_body_line_per_entry() {
        let lock = render_lock(OPCODES);
        let body_lines = lock.lines().filter(|l| !l.starts_with('#')).count();
        assert_eq!(body_lines, OPCODES.len());
    }

    /// A unique temp path for a lockfile fixture so parallel tests do not collide
    /// and nothing is left behind in the repo.
    fn temp_lock_path(tag: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        path.push(format!("graftx-xtask-checklock-{tag}-{nanos}.lock"));
        path
    }

    #[test]
    fn check_lock_reports_up_to_date_for_canonical_file() {
        let path = temp_lock_path("uptodate");
        std::fs::write(&path, render_lock(OPCODES)).expect("seed canonical lockfile");
        let status = check_lock(&path, OPCODES);
        assert!(status.is_up_to_date());
        assert!(matches!(status, LockStatus::UpToDate));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn check_lock_reports_stale_on_difference() {
        let path = temp_lock_path("stale");
        std::fs::write(&path, "# stale\n0xFF_FFFFFF Bogus OPCODE\n").expect("seed stale lockfile");
        let status = check_lock(&path, OPCODES);
        assert!(!status.is_up_to_date());
        match status {
            LockStatus::Stale { expected, .. } => {
                assert_eq!(expected, render_lock(OPCODES));
            }
            other => panic!("expected Stale, got {}", other.is_up_to_date()),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn check_lock_reports_unreadable_when_missing() {
        let path = temp_lock_path("missing");
        let _ = std::fs::remove_file(&path);
        let status = check_lock(&path, OPCODES);
        assert!(!status.is_up_to_date());
        assert!(matches!(status, LockStatus::Unreadable { .. }));
    }
}
