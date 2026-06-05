//! Opcode-table source of truth and Markdown renderer.
//!
//! The entries here mirror the opcode constants defined in `graftx-protocol`
//! (`core_op::*` and `vk_op::*`). They are duplicated rather than imported so
//! that `gen-opcodes` stays a leaf tool with no dependency on the protocol
//! crate; the `check-xrefs` companion is responsible for catching drift.
//!
//! Each entry pairs a human-facing API name with its numeric [`ApiId`] byte and
//! a 24-bit call id. The full opcode is `(api_id << 24) | call_id`, matching the
//! `opcode()` helper in `graftx-protocol`.

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
/// Core opcodes first, then Vulkan, OpenGL, and CUDA, each in ascending
/// call-id order within its API.
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
        assert_eq!(data_rows, 20);
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
        let total = table.find("| TOTAL |").expect("TOTAL row present");
        assert!(
            core < vulkan && vulkan < opengl && opengl < cuda && cuda < total,
            "rows should be ordered Core, Vulkan, OpenGl, Cuda, TOTAL:\n{table}"
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
    }
}
