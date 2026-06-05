# 32. Appendices: Opcode Tables, Glossary & References

Reference material that the rest of the plan points at: the canonical opcode-table template each API chapter will instantiate, a project-wide glossary, the crate inventory with licenses, external specifications, and the document changelog.

This chapter is a reference appendix, not a design narrative. It collects artifacts that other chapters generate or consume so they live in exactly one place and stay versioned together. The opcode-table format is the rendered, human-readable view of the frozen `opcodes.lock` manifest defined in the Protocol chapter (Ch. 06) and produced by the schema generator owned by the Build/dist chapter (Ch. 28); the glossary fixes vocabulary used across all 31 preceding chapters; the crate inventory is the dependency surface that the licensing and supply-chain policy in the Build/dist chapter (Ch. 28) and CI in the Testing chapter (Ch. 26) audit. Nothing here introduces new mechanism — it freezes presentation and terminology so cross-references resolve unambiguously.

## 32.1 Opcode-table template (per API)

Every API chapter (14 Vulkan, 15 OpenGL, 16 CUDA, … 21 SYCL) will end with an **opcode table** rendered from `opcodes.lock`. The table is generated, never hand-edited: `cargo xtask gen-opcode-docs` reads the lockfile and emits one Markdown file per `ApiId` under `docs/design/plan/opcodes/<api>.md`, which the API chapter `{{include}}`s. This guarantees the docs and the wire contract can never drift — CI in the Testing chapter (Ch. 26) re-renders and diffs against the committed copy.

### 32.1.1 Lockfile row schema

The lockfile is a deterministic, append-only TOML array. Each row is the frozen mapping from a C entrypoint to its 24-bit call id (Protocol chapter, Ch. 06), plus the marshalling metadata the generator derived. The opcode is the `u32` value `(ApiId << 24) | call_id` per decision D4 (high byte = `ApiId`, low 24 bits = `call`):

```rust
/// One frozen entrypoint binding. Serialized in `opcodes.lock`.
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct OpcodeEntry {
    pub api:      u8,        // ApiId high byte: Vulkan=0x01, GL=0x02, CUDA=0x03,
                            //   OpenCL=0x04, HIP=0x05, LevelZero=0x06, Video=0x07 (Ch. 06)
    pub call:     u32,       // low 24 bits; never reused, never renumbered
    pub name:     String,    // C symbol, e.g. "vkCreateBuffer"
    pub since:    String,    // protocol minor that introduced it; "0.0" pre-stable (sample
                            //   values below are illustrative — everything is "0.0" at v0.0.0)
    pub reply:    ReplyKind, // None | Value | OutParams | Both
    pub bulk:     BulkUse,   // None | In | Out | InOut  (ivshmem descriptors)
    pub tier:     Tier,      // M1 | M2 | M3 — coverage milestone (Ch. 02)
    pub status:   Status,    // Stubbed | Marshalled | Replayed | Validated
    pub flags:    Vec<String>, // "danger", "deprecated", "ext:VK_KHR_*"
    pub arg_hash: u64,       // FNV-1a of the marshalled arg signature
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum ReplyKind { None, Value, OutParams, Both }
```

`arg_hash` is the second half of opcode stability: renumbering a call id is caught by name→call diffing, but *silently changing an argument layout* under a stable call id would corrupt every peer that negotiated the old minor. The hash folds the ordered, codec-normalized argument types (Serialization chapter, Ch. 07) so any layout change forces a new call id or an explicit `--accept-arg-break` waiver in CI.

### 32.1.2 Rendered table format

Each rendered chapter table uses a fixed column set so a reader can scan any API the same way. Opcodes are shown as the full `u32` (`(ApiId << 24) | call_id`, per D4) in hex, matching what the dispatcher in the Server core chapter (Ch. 10) routes on:

| Column     | Source field        | Meaning                                                        |
|------------|---------------------|----------------------------------------------------------------|
| `Opcode`   | `opcode(api,call)`  | full 32-bit value, hex, e.g. `0x01_000042`                     |
| `Call`     | `call`              | decimal call id within the API block                           |
| `Entrypoint` | `name`            | the C symbol the shim exports / server replays                 |
| `Reply`    | `reply`             | `—` / `ret` / `out` / `ret+out` — whether a Response is sent   |
| `Bulk`     | `bulk`              | `—` / `in` / `out` / `io` — ivshmem descriptor direction       |
| `Since`    | `since`             | first protocol minor that carried it                           |
| `Tier`     | `tier`              | M1 / M2 / M3 coverage milestone                                |
| `Status`   | `status`            | implementation state (see legend)                              |
| `Notes`    | `flags`             | danger/ext/deprecated badges                                   |

A worked excerpt — what the Vulkan chapter's table (Ch. 14) will render for a handful of rows — illustrates the intended density. The `Since` values are **illustrative**: at v0.0.0 every row is `0.0` (pre-stable), and the opcodes follow D4 with Vulkan's `ApiId = 0x01`:

```text
Opcode       Call  Entrypoint              Reply   Bulk  Since  Tier  Status      Notes
0x01_000000     0  vkCreateInstance        ret+out   —   0.0    M1    Validated
0x01_000005     5  vkCreateDevice          ret+out   —   0.0    M1    Validated
0x01_000042    66  vkCreateBuffer          ret+out   —   0.0    M1    Replayed
0x01_000051    81  vkCmdCopyBuffer           —       —   0.0    M1    Marshalled  danger
0x01_0000A3   163  vkMapMemory             ret+out  out  0.0    M1    Replayed    danger
0x01_0001F0   496  vkCmdDrawIndirect         —       in  0.0    M2    Stubbed     ext:VK_KHR_draw_indirect_count
```

**Status legend** (the lifecycle each entrypoint moves through, Ch. 09/10):

| Status       | Client shim       | Server replay         | CI gate                          |
|--------------|-------------------|-----------------------|----------------------------------|
| `Stubbed`    | symbol exported, returns error | opcode rejected | symbol-presence test only        |
| `Marshalled` | args serialized   | decoded, not executed | round-trip codec test (Ch. 07)   |
| `Replayed`   | full forward      | native call issued    | conformance smoke test (Ch. 26)  |
| `Validated`  | full forward      | replay + input validation (Ch. 23) | fuzz + validation test     |

The `danger` flag marks entrypoints whose arguments index device memory, command buffers, or pointer-bearing structs and therefore *must* reach `Validated` before being enabled in a non-developer build — the security posture from the project brief ("validate decoded commands") is encoded directly in the table so an auditor can grep `danger` rows and confirm none are stuck at `Marshalled`.

### 32.1.3 Coverage roll-up

The same `xtask` emits a per-API coverage summary that the goals chapter (Ch. 02) and roadmap (Ch. 30) cite. The roll-up is a simple count by `status` over `tier`, so progress is a single query against the lockfile rather than a manually maintained spreadsheet:

```text
API        Total  Stubbed  Marshalled  Replayed  Validated   M1 done?
Vulkan       412      48          90       210         64       62%
OpenGL       703     701           2         0          0        0%
CUDA         298     298           0         0          0        0%
Core          16       0           0         4         12      100%
```

At v0.0.0 every row is `Stubbed` except a hand-written Core handshake; the table is the burn-down surface for the whole project.

## 32.2 Glossary

Terms are defined once here; chapters use them without re-defining. Where a term has a precise GraftX meaning that differs from common usage, that distinction is called out.

| Term | Definition |
|------|------------|
| **API remoting** | Intercepting a high-level graphics/compute API call on one host and executing it on another, returning results — as opposed to remoting at the framebuffer (pixel-streaming) or driver-command (e.g. virtio-gpu Venus) layer. |
| **Shim** | The client-side `cdylib`+`rlib` (`graftx-client`, Ch. 09) that exports the C symbols of a target API, marshals each call, and forwards it. Synonym: *interposer*. |
| **Server / replay server** | `graftx-server` (Ch. 10), the Windows-guest binary that decodes the command stream and re-issues calls against the native driver that owns the passed-through GPU. |
| **Control plane** | The small, ordered, reliable message channel over virtio-vsock carrying Requests/Responses/Events (Ch. 06, Ch. 08). |
| **Bulk plane** | The large-payload path over ivshmem shared memory; control-plane frames carry *descriptors* that reference bulk regions (Ch. 08, Ch. 12). |
| **Frame** | One control-plane message: a `FrameHeader` plus a Request/Response/Event body (Ch. 06 §6.2). |
| **Opcode** | The `u32` identifying an entrypoint: high byte = `ApiId`, low 24 bits = call id (Ch. 06 §6.3). |
| **Call id** | The 24-bit id of an entrypoint within its API block; assigned in source order, never reused (Ch. 06, this chapter §32.1). |
| **`opcodes.lock`** | The frozen manifest binding `(api, name)` → `call` + arg layout; CI fails on any change (§32.1.1). |
| **Reply elision** | Omitting a Response for void, no-out-param calls to save a round-trip; ordering still preserved (Ch. 06 §6.4). |
| **Handle** | A guest-visible opaque token (`VkBuffer`, `cudaStream_t`, GL name) mapped to a server-side native object via the handle table (Ch. 11). |
| **Handle table** | The per-session bidirectional map (guest token ↔ server object) with generation counters to detect use-after-free (Ch. 11). |
| **Descriptor (bulk)** | A `{region, offset, len, dir}` tuple in a frame referencing a bulk-plane region; distinct from a Vulkan *descriptor set* (disambiguated as **VkDescriptor** where ambiguous). |
| **Write-revocation** | Hypervisor/ivshmem-device-enforced removal of guest write access to a bulk region after the server has snapshotted it; cannot be enforced in software alone (project brief; Ch. 12, Ch. 23). |
| **Server-private copy** | The defensive copy the server makes of validated bulk data into memory the guest cannot mutate, defeating TOCTOU on the shared region (Ch. 12 §, Ch. 23). |
| **Paired-guest channel** | The vsock reachability scoping between exactly two guests; it is *reachability*, not *authentication* (project brief; Ch. 23). |
| **Session** | One client↔server connection lifetime, owning its handle table, quotas, and negotiated protocol minor (Ch. 10, Ch. 23). |
| **Tier (M1/M2/M3)** | Coverage milestones: M1 = Vulkan spine; M2 = GL/CUDA/compute breadth; M3 = reach APIs (video, OptiX, AMF, SYCL, WebGPU) (Ch. 02, Ch. 30). |
| **Tail call / async-completion** | A server→client Event delivering the result of a call whose completion is asynchronous on the native side (fences, CUDA events) (Ch. 06, Ch. 13). |
| **Schema generator** | The `xtask`/build tool that parses API headers and emits opcodes, marshalling stubs, and the lockfile (Ch. 28). |
| **Conformance harness** | The test rig replaying captured or synthetic streams against a real GPU to check correctness (Ch. 26). |
| **Backpressure** | Flow control that stalls the client when the server's queues or bulk arena near limits, instead of dropping or OOMing (Ch. 08, Ch. 23). |
| **Quota** | A per-session resource cap (open handles, bulk bytes, in-flight frames) enforced by the server (Ch. 23). |
| **WSI** | Window System Integration — how rendered surfaces reach a displayable window; cross-host WSI is the surface-return problem (Ch. 22). |
| **PCI passthrough (VFIO)** | Assigning the physical GPU to the Windows guest so the server has native driver access (Ch. 03, Ch. 04). |

## 32.3 Crate inventory (current + planned)

The workspace today (v0.0.0) contains four skeleton crates. The table records both the present members and the planned support crates, with the license each is published under or the license class an added dependency must satisfy. The full supply-chain and `cargo-deny` policy is Ch. 28; this is the inventory it operates on.

### 32.3.1 Workspace members

| Crate | Kind | License | Status | Role |
|-------|------|---------|--------|------|
| `graftx-protocol` | lib | MIT | exists (stub) | wire format, opcodes, framing (Ch. 06/07) |
| `graftx-transport` | lib | MIT | exists (stub) | `Transport` trait, vsock + ivshmem channels (Ch. 08) |
| `graftx-client` | cdylib + rlib | MIT | exists (stub) | Linux shims exporting C symbols (Ch. 09) |
| `graftx-server` | bin | MIT | exists (stub) | Windows replay server (Ch. 10) |
| `graftx-xtask` | bin | MIT | planned | schema generator, opcode-doc renderer, codegen (Ch. 28) |
| `graftx-validate` | lib | MIT | planned | decoded-command validation (Ch. 23) |
| `graftx-handles` | lib | MIT | planned | handle-table data structures (Ch. 11) |
| `graftx-test-harness` | lib + bin | MIT | planned | conformance replay rig (Ch. 26) |

All first-party crates are MIT, matching the repository license. The `cdylib` member is the only one producing a `.so` with a stable C ABI; everything else is internal Rust.

### 32.3.2 External dependencies (planned classes)

Dependencies are admitted only from the permissive allow-list (`cargo-deny`, Ch. 28). The intent below is illustrative; exact versions are pinned in `Cargo.lock` and not duplicated here.

| Dependency | License | Plane | Purpose | Notes |
|------------|---------|-------|---------|-------|
| `thiserror` | MIT/Apache-2.0 | all | error enums (mandated by project conventions) | no proc-macro at runtime |
| `bytemuck` | MIT/Apache-2.0/Zlib | protocol | zero-copy POD casts for the codec (Ch. 07) | `#[derive(Pod)]` audited |
| `bitflags` | MIT/Apache-2.0 | protocol | `FrameFlags`, API flag enums | |
| `nix` | MIT | transport | `AF_VSOCK` sockets on Linux client | Linux-only target cfg |
| `windows`/`windows-sys` | MIT/Apache-2.0 | server | Win32 + WDDM access on the server | Windows-only target cfg |
| `memmap2` | MIT/Apache-2.0 | transport | ivshmem region mapping | |
| `tracing` | MIT | all | structured logging/spans (Ch. 27) | |
| `bindgen` (build-dep) | BSD-3-Clause | xtask | parse C API headers for codegen (Ch. 28) | build-time only |
| `ash` | MIT | server | safe Vulkan loader bindings on the server | candidate; vs. raw `vk-sys` |
| `proptest` (dev-dep) | MIT/Apache-2.0 | tests | round-trip + fuzz of the codec | dev-only |

Forbidden classes (GPL/AGPL/LGPL-without-exception, "no license", and unvetted copyleft) are rejected at CI by `cargo-deny`; any vendor GPU SDK headers used by `bindgen` are consumed under their redistributable-headers terms and never vendored into the MIT tree.

## 32.4 External references and specifications

Authoritative sources for each remoted API and the transport substrate. Chapters cite these by short name (e.g. "Vulkan spec §11.2").

**APIs**

- Vulkan 1.3 specification and `vk.xml` registry — Khronos. (`vk.xml` is the machine-readable command table the schema generator parses, Ch. 28/14.)
- OpenGL 4.6 + OpenGL ES 3.2 specifications and `gl.xml` registry — Khronos. (Ch. 15)
- EGL 1.5 and GLX 1.4 specifications — Khronos. (Ch. 20/21)
- CUDA Driver & Runtime API references; PTX/ELF module formats — NVIDIA. (Ch. 16)
- OpenCL 3.0 specification and `cl.xml` — Khronos. (Ch. 22)
- ROCm/HIP runtime API reference — AMD. (Ch. 23)
- Level Zero (oneAPI) specification — Unified Acceleration Foundation. (Ch. 24)
- WebGPU + WGSL specifications — W3C GPU for the Web WG. (Ch. 25)
- NVENC/NVDEC Video Codec SDK; VA-API; AMD AMF SDK; OptiX programming guide; SYCL 2020 spec — respective vendors / Khronos. (Ch. 17, 25)

**Transport & virtualization**

- virtio specification (vsock device) — OASIS virtio TC. (Ch. 08)
- ivshmem device documentation — QEMU project. (Ch. 08/12)
- VFIO / PCI passthrough documentation — Linux kernel. (Ch. 03/04)

**Prior art / comparable systems**

- VirGL / Venus (virtio-gpu) — Mesa/virglrenderer. (Ch. 03)
- rCUDA, qCUDA, Bumblebee/VirtualGL — academic & OSS GPU-remoting. (Ch. 03)
- Microsoft GPU-PV / WDDM paravirtualization. (Ch. 03)

**Rust / tooling**

- The Rust Reference (edition 2021), the Rustonomicon (unsafe/FFI), and the Cargo Book — rust-lang.org. (Ch. 05, Ch. 29)
- The Rust API Guidelines and `cargo-deny`/`RustSec` advisory DB. (Ch. 28)
- Conventional Commits 1.0.0 and Keep a Changelog 1.1.0 — for repository conventions (§32.5, Ch. 27).
- Semantic Versioning 2.0.0 — for crate and protocol-minor versioning (Ch. 06 §6.5, Ch. 31).

## 32.5 Document changelog

This changelog tracks the *design plan* (the `docs/design/plan/` set), not the code or the wire protocol — the protocol's own versioned changes live in `opcodes.lock` and the `PROTOCOL_VERSION` history (Ch. 06 §6.5). It follows Keep a Changelog conventions; entries are added top-down, newest first. Because the plan is authored at v0.0.0, the initial entry covers the whole set.

```text
## [Unreleased]
### Planned
- Wire opcode tables (§32.1) once `graftx-xtask` (Ch. 28) can render from opcodes.lock.
- Fill the §32.3.2 dependency versions from the first committed Cargo.lock.

## [0.1.0-plan] — 2026-06-05
### Added
- Initial 32-chapter design plan covering protocol, transport, client/server,
  per-API coverage (Vulkan…SYCL), security, testing, ops, and roadmap.
- This appendix: opcode-table template, glossary, crate inventory, references.
### Notes
- Status of every artifact is "planned/proposed"; codebase is workspace + 4
  crate skeletons (graftx-protocol/transport/client/server), nothing replayed.
```

**Maintenance rules.** (1) Any chapter that changes a *contract* another chapter depends on — opcode encoding, frame layout, the `Transport` trait, handle-table semantics — adds a changelog line and bumps the `-plan` minor. (2) Renaming or renumbering anything in §32.1 or §32.2 is a breaking plan change and must update every cross-reference; a `cargo xtask check-xrefs` lint (Ch. 26) verifies that every "Ch. NN" pointer resolves to an existing chapter and that glossary terms used in chapters are defined here. (3) The crate inventory (§32.3) is regenerated from the actual workspace `Cargo.toml` members and `cargo metadata`, so it cannot silently diverge from reality once the support crates land. These rules keep the appendix authoritative rather than a stale copy of decisions made elsewhere.
