# 31. Risks, Open Questions & Decision Log

This chapter catalogs the top technical risks to GraftX with likelihood/impact/mitigation, the design questions still open at v0.0.0, and an ADR-style decision log (template plus seed decisions) that will be maintained as the source of truth for "why we chose X".

## 31.1 Why this chapter exists

GraftX is an ambitious surface — a dozen GPU API families, two transport planes, an untrusted command stream replayed against native drivers. Most of the program's failure modes are *predictable* today even though almost nothing is implemented. This chapter exists to make those failure modes explicit, attach owners and triggers to them, and record the cross-cutting decisions that shaped the rest of `docs/design/plan/` so that later contributors do not silently relitigate them. It is forward-looking: every "we will" is a planned action, not a delivered one.

Risk scoring uses a 1–5 scale on two axes. Likelihood is the probability the risk materializes within the v0.x window if unmitigated; Impact is the blast radius (schedule, scope, or a hard architectural wall). The product `L×I` gives a crude priority, but the prose mitigation matters more than the number.

```
L (likelihood)         I (impact)
1 rare                 1 cosmetic / local
2 unlikely             2 one subsystem slows
3 plausible            3 a crate must be reworked
4 likely               4 cross-crate redesign
5 near-certain         5 program-level wall / pivot
```

## 31.2 Top technical risks

### R-01 ivshmem availability & ABI churn (L5 × I5 = 25)

The bulk plane depends on `ivshmem` (and the `ivshmem-doorbell` / shared-BAR semantics) being exposed by the hypervisor and stable across QEMU and any future VMM. ivshmem is a niche device; its memory layout, interrupt vectors, and the very existence of a `/dev/uioN` or `/sys/.../resource2` mapping vary by VMM and version. If a target deployment lacks ivshmem entirely (e.g., a cloud nested-virt host, or a VMM that only ships virtio), the entire bulk-plane design (chapters on transport) is unavailable.

Mitigation:
- The `Transport` trait in `graftx-transport` will be the only ivshmem-aware boundary; **no other crate may name ivshmem types**. This keeps the blast radius to one crate.
- We will define a **`ShmemBackend` enum** so the control plane works standalone:

```rust
pub enum BulkPlane {
    Ivshmem(IvshmemRegion),   // preferred, zero-copy
    VsockStream(VsockBulk),   // fallback: large payloads over the control socket
    HostSharedFile(MmapRegion), // dev/test on a single host, no VM
}
```

- `VsockStream` is a *correctness* fallback (slow, but the protocol does not change). It de-risks the program from "ivshmem missing" turning into a wall — it degrades to a perf risk instead. See chapter on transport for the bandwidth deltas.
- A capability probe at session handshake negotiates which `BulkPlane` is live; the protocol carries a `bulk_caps` bitfield so a mismatch is a clean refusal, not a crash.

### R-02 Untrusted command stream → driver memory corruption (L4 × I5 = 20)

The server replays attacker-controllable commands against vendor drivers that assume a cooperative caller. A malformed `glBufferData` size, an out-of-range descriptor index, a Vulkan handle forged by the client, or a CUDA pointer that was never allocated can corrupt driver state or the server process. Safety is priority 4 of 4, but a corrupted server *also* breaks stability (priority 3) and breadth (a crashed server serves no APIs), so this risk is rated high regardless of priority ordering.

Mitigation:
- **Server-authoritative handles**: the server mints every wire handle; clients never put an invented handle on the wire (ADR-0004 / ADR-0008). Every `VkBuffer`, `GLuint`, `cudaStream_t`, `cl_mem` is a 64-bit opaque ID — top 8 bits kind/API-namespace, middle generation, low slot index — resolved through a per-session `HandleTable` (chapter on object model). A forged or stale ID resolves to `Err(InvalidHandle)`, never to a native pointer.

```rust
struct HandleTable<H> {
    slots: Vec<Option<Live<H>>>, // index = low bits of the 64-bit wire Handle
    generation: Vec<u32>,        // ABA guard; retire a slot before its gen wraps
}
impl<H> HandleTable<H> {
    fn resolve(&self, id: u64) -> Result<&H, ProtoError> { /* check kind + gen + Some */ }
}
```

- **Decode-time validation**: lengths, counts, and enum discriminants are validated in `graftx-protocol` before any FFI; the decoder rejects anything that would index past a slice. No `unsafe` runs on un-validated input.
- **Copy-to-private** (see ADR-0003): bulk payloads are copied out of shared memory into server-owned buffers *before* validation, closing the TOCTOU window where a malicious client mutates shared memory after the size check.
- **Sandbox** the server process (job object / restricted token / AppContainer on Windows) so a driver-level escape is contained.
- Residual risk is real: we cannot fully validate semantic correctness of every draw call, and a hostile guest with the GPU can likely DoS it. We accept DoS within a paired-guest trust boundary; we do *not* accept host escape.

### R-03 Vulkan-as-spine leverage fails for legacy APIs (L3 × I4 = 12)

ADR-0002 makes Vulkan the canonical internal representation and routes GL/GLES/EGL/CUDA-interop/WebGPU through it where practical. The bet is that one well-modeled backend amortizes across many front-ends. The risk: some APIs (fixed-function GL 1.x, compute-only ROCm/HIP, OptiX ray pipelines, hardware video on AMF/NVENC) map *poorly* onto Vulkan, forcing per-API native backends anyway — eroding the leverage and adding a translation layer that is pure overhead for those paths.

Mitigation:
- The spine is **opt-in per API family**, not mandatory. The server's `ReplayBackend` trait lets a family bypass the spine and call its native driver directly:

```rust
trait ReplayBackend {
    fn family(&self) -> ApiFamily;
    fn replay(&mut self, cmd: DecodedCmd, ctx: &mut SessionCtx) -> Result<Reply, ReplayError>;
}
```

- GL/GLES go through the spine first (Zink-style) because the win is largest there; CUDA/ROCm/codecs default to native backends from day one. We validate the spine bet on GL before extending it.
- Decision is **reversible** per family without touching the wire protocol, because the protocol is API-shaped, not Vulkan-shaped (chapter on protocol).

### R-04 Cross-API interop / sharing (L4 × I4 = 16)

Real workloads mix APIs: CUDA writes a buffer that GL samples; a Vulkan image is imported into OpenCL; a decoded video frame (AMF/NVDEC) is fed to a GL texture. On a single host this uses external-memory / `dma-buf` / `VK_KHR_external_memory` / `cudaImportExternalMemory`. Across our remoting boundary, "share" must mean "the *server* shares natively while we present *one* consistent handle namespace to the guest." Getting the synchronization (timeline semaphores across API boundaries) and lifetime right is subtle and a frequent source of GPU hangs.

Mitigation:
- A unified server-side **`ResourceGraph`** tracks every allocation and the set of API views onto it; cross-API import is a server-local operation using native external-memory extensions, never re-uploaded over the wire.
- All cross-API sync funnels through a single **timeline-semaphore** abstraction; we forbid binary semaphores in the internal model to avoid signal-ordering hazards.
- Deferred to a milestone *after* single-API breadth is proven; flagged here so the object model (chapter on object model) reserves namespace and lifetime hooks now.

### R-05 Performance: per-call round-trips dominate (L4 × I3 = 12)

Naive remoting issues one vsock round-trip per API call. GL/Vulkan apps make 10k–100k calls/frame; at even 5 µs RTT that is catastrophic. The program priority is breadth > performance, but a layer this slow is unusable and therefore fails stability-in-practice.

Mitigation:
- **Command batching**: clients buffer non-returning calls and flush on a sync point (`glFlush`, queue submit, fence wait). Only ~returning~ calls (queries, maps, `glGet*`) force a round-trip.
- **Asynchronous reply futures** for calls whose return value is not immediately observed.
- **Shared-memory ring** for the command stream so flush is a pointer bump, not a syscall (chapter on transport).
- We will track a **calls-per-round-trip** metric in CI perf gates; a regression below threshold fails the gate.

### R-06 Windows/Linux ABI & calling-convention mismatch (L3 × I3 = 9)

Client shims export Linux C ABI symbols (`libGL.so.1` etc.); the server links Windows drivers (`d3d`/`nvcuda.dll`/`vulkan-1.dll`). Struct packing, `long` width (LP64 vs LLP64), enum sizes, and pointer width differ. A struct serialized on Linux and re-inflated for a Windows driver must be re-marshaled, not byte-copied.

Mitigation:
- The wire format uses **fixed-width, little-endian, explicitly-padded** structs (`#[repr(C)]` with explicit pad fields, all `u32/u64`, never `c_long`). The protocol layer owns all platform structs; neither shim nor server passes a guest struct straight to a driver.
- A `build.rs` static-assert (`const _: () = assert!(size_of::<WireFoo>() == N);`) pins every wire struct size so an accidental layout change fails compilation.

### R-07 Driver/extension version skew (L4 × I2 = 8)

The guest app expects extensions/limits the *passthrough GPU's Windows driver* may not expose, or exposes differently. Reporting Linux-side capabilities the server can't honor causes late, confusing failures.

Mitigation: capabilities are **always queried from the real driver at session start** and reported verbatim to the guest (no synthesized caps). Unsupported extension strings are filtered out, not faked.

### R-08 Team scope: 12 API families is enormous (L4 × I3 = 12)

Breadth is priority 1, but each family is a multi-month effort. Risk: spreading thin yields a dozen half-working backends and zero shippable path.

Mitigation: a **strict family ordering** (chapter on roadmap) — Vulkan + EGL/GLES first (the spine), then GL, then compute (CUDA/OpenCL), then codecs/ROCm/L0/WebGPU/OptiX/AMF/SYCL. A family ships behind a feature flag and a conformance gate before the next starts.

### Risk summary table

| ID | Risk | L | I | Score | Primary mitigation | Owner crate |
|----|------|---|---|-------|--------------------|-------------|
| R-01 | ivshmem availability/ABI | 5 | 5 | 25 | `BulkPlane` enum + vsock fallback | transport |
| R-02 | Untrusted stream corruption | 4 | 5 | 20 | handle indirection + copy-to-private + sandbox | server |
| R-04 | Cross-API interop/sync | 4 | 4 | 16 | server `ResourceGraph` + timeline-only | server |
| R-03 | Vulkan spine leverage | 3 | 4 | 12 | per-family opt-in backends | server |
| R-05 | Per-call round-trips | 4 | 3 | 12 | batching + ring + async replies | client/transport |
| R-08 | 12-family scope | 4 | 3 | 12 | strict ordering + per-family gates | all |
| R-06 | ABI/calling-convention | 3 | 3 | 9 | fixed-width re-marshaled wire structs | protocol |
| R-07 | Driver version skew | 4 | 2 | 8 | query-real-caps, never synthesize | server |

## 31.3 Open design questions

These are unresolved; each has a proposed lean and the decision it blocks.

**Q-01 — Wire encoding: hand-rolled `#[repr(C)]` vs a schema (FlatBuffers/Cap'n Proto)?**
A schema buys versioning and tooling; hand-rolled buys zero-copy and exact layout control on the ivshmem path. *Lean:* hand-rolled `#[repr(C)]` for the hot per-call commands (perf), with a small versioned envelope for the handshake/control messages. Blocks the protocol crate's module layout.

**Q-02 — One vsock connection multiplexed, or a connection per GPU context?**
Multiplexing simplifies pairing but adds head-of-line blocking; per-context isolates stalls but multiplies sockets. *Lean:* single control connection + logical channels (stream IDs) in the protocol, so we get isolation semantics without N sockets. Blocks session/multiplexing chapter.

**Q-03 — How is the ivshmem region carved between sessions?**
Static equal partition is simple but wastes memory; a server-side allocator is flexible but is itself attack surface. *Lean:* fixed per-session arenas negotiated at handshake, sub-allocated by a bump+free-list inside each arena. Blocks transport memory-layout chapter.

**Q-04 — Synchronous map/readback: copy or true zero-copy?**
`glMapBuffer`/`vkMapMemory` semantics want a pointer into GPU-visible memory. True zero-copy across the boundary is unsafe (R-02 TOCTOU). *Lean:* map returns a *staging copy* in the guest's shared region; writes are flushed back on unmap. Accepts a copy cost for safety. Blocks object-model map handling.

**Q-05 — Error model across the boundary: replicate driver errno/`VkResult`, or a GraftX error space?**
Apps branch on exact `GL_OUT_OF_MEMORY`/`VK_ERROR_DEVICE_LOST`. *Lean:* pass the native result code through verbatim in the reply, plus a GraftX transport-level error band that cannot collide with any driver code. Blocks protocol reply layout.

**Q-06 — Server: single process multi-session, or process-per-session?**
Process-per-session is the strongest sandbox and crash-isolation story but costs driver-init time and VRAM per process. *Lean:* process-per-session for the isolation win (aligns with R-02), with a warm-pool to hide init latency. Blocks server architecture chapter.

**Q-07 — Async client model: block the guest thread, or shadow-object + lazy sync?**
GL is largely synchronous-looking even when async underneath. *Lean:* shadow objects for state-query-able handles so most calls return without a round-trip; force sync only on observable reads (ties to R-05). Blocks client shim design.

## 31.4 Decision log (ADR-style)

Decisions live as numbered ADRs under `docs/design/plan/` (or a future `docs/adr/`). Each is immutable once `Accepted`; a reversal is a *new* ADR that `Supersedes` the old one. Status ∈ {Proposed, Accepted, Deprecated, Superseded}.

### ADR template

```markdown
# ADR-NNNN: <short title>
- Status: Proposed | Accepted | Deprecated | Superseded by ADR-MMMM
- Date: YYYY-MM-DD
- Deciders: <names/roles>
- Tags: transport | protocol | server | client | security

## Context
What forces are at play? Constraints, requirements, the problem.

## Decision
The choice, stated in one or two sentences, active voice.

## Consequences
Positive, negative, and neutral. What becomes easier; what becomes harder.

## Alternatives considered
Each option with the one reason it lost.

## Links
Related ADRs, chapters, issues.
```

### Seed decisions

#### ADR-0001: Split transport into control plane (virtio-vsock) + bulk plane (ivshmem)

- Status: Accepted — Date: 2026-06-04 — Tags: transport

**Context.** Remoting needs both tiny, ordered, reliable control messages (handshake, small commands, replies, fences) and large opaque payloads (vertex/texture/buffer uploads, mapped readbacks) that must not pay a per-byte copy/syscall tax. No single primitive serves both: vsock is a clean reliable stream but copies through the kernel; ivshmem is zero-copy shared memory but has no built-in framing or reliability.

**Decision.** Use **virtio-vsock as the control plane** (framing, ordering, connection lifecycle, doorbell-style notifications) and **ivshmem as the bulk plane** (zero-copy payload arenas), with the control plane carrying descriptors (offset+len+arena) that reference bulk data.

**Consequences.** (+) Each plane is optimal for its traffic; small calls stay cheap, big payloads avoid copies. (+) Reliability/ordering live in one place (vsock). (−) Two transports to implement, probe, and keep in sync; the `BulkPlane` fallback (R-01) is required so a missing ivshmem does not wall the program. (−) Cross-plane consistency (a control message must not be processed before its bulk data is visible) needs explicit memory barriers/sequence numbers.

**Alternatives considered.** *vsock-only*: simplest, but per-byte copy kills upload-heavy workloads. *ivshmem-only with a hand-rolled ring for control*: reinvents reliable framing and connection teardown. *virtio-gpu/venus*: tied to a specific stack and not guest-to-guest with a Windows owner.

**Links.** R-01, R-05, transport chapter; superseded-by: none.

#### ADR-0002: Adopt Vulkan as the internal "spine" representation

- Status: Accepted — Date: 2026-06-04 — Tags: server, protocol

**Context.** Twelve API families is the program's headline scope (R-08). Implementing a fully independent native backend per family multiplies effort and bugs. Many graphics front-ends (GL, GLES, EGL, WebGPU) are expressible over Vulkan (Zink, Dawn precedent), and Vulkan's explicit object/sync model is the cleanest target for our handle-indirection and timeline-sync needs (R-04).

**Decision.** Make **Vulkan the canonical internal model** for the server's graphics path. GL/GLES/EGL/WebGPU translate to it where the translation is sound; compute and codec families (CUDA, ROCm/HIP, OpenCL, Level Zero, NVENC/AMF, OptiX, SYCL) use native backends behind the same `ReplayBackend` trait. The choice is **per-family opt-in**, not global.

**Consequences.** (+) One hardened graphics backend amortizes across several front-ends; one sync model (timelines) for interop. (+) Vulkan handles map naturally onto the `HandleTable` indirection. (−) Translation overhead on paths that map poorly (fixed-function GL) — accepted, and reversible per family (R-03). (−) The team must hold deep Vulkan expertise.

**Alternatives considered.** *Native backend per API*: maximal fidelity, but unbounded effort. *Translate everything to Vulkan including compute*: CUDA/HIP semantics don't fit; rejected.

**Links.** R-03, R-04, R-08; object-model and server chapters.

#### ADR-0003: Copy validated bulk data into server-private memory before use

- Status: Accepted — Date: 2026-06-04 — Tags: security, server

**Context.** The bulk plane is shared memory the untrusted guest can mutate at any time. Validating a payload in-place and then handing the *same* shared bytes to a driver is a classic TOCTOU: the guest can flip the size or contents between the check and the driver read. Write-revocation on shared memory can only be enforced at the hypervisor/ivshmem-device layer, which we do not control from the server.

**Decision.** For every bulk payload that feeds a driver call, the server **copies the bytes out of shared memory into a server-private buffer, then validates, then uses** only the private copy. Shared memory is treated as read-once, always-hostile.

**Consequences.** (+) Closes the TOCTOU window; the driver only ever sees immutable, validated, server-owned bytes (directly mitigates R-02). (+) Simplifies reasoning — validation invariants hold for the buffer's whole lifetime. (−) A mandatory copy on the hot upload path costs bandwidth/latency; partially offset by reusing pooled private buffers and by skipping the copy for *outbound* (server→guest) data where the trust direction is reversed. (−) Higher server memory pressure under many concurrent large uploads — bounded by per-session quotas/backpressure.

**Alternatives considered.** *Validate-in-place*: fast but TOCTOU-vulnerable; rejected on safety. *Hypervisor write-revocation per frame*: correct in theory but needs an ivshmem-device feature we cannot assume (R-01); revisit if/when available — would be a superseding ADR.

**Links.** R-01, R-02; transport bulk-plane and security chapters.

### Cross-cutting decisions (one-line ADRs)

These ratify the program-wide invariants the rest of `docs/design/plan/` is built on; each is Accepted and owned by the chapters named in parentheses.

- **ADR-0004 — Server-authoritative handles.** The server mints every wire handle; the client never puts an invented handle on the wire — for async/deferred replies the client may hold a purely *local* provisional proxy token (never sent as authority) reconciled deterministically when the real handle arrives. *Rationale:* a forged handle can never reach a driver, closing the R-02 forgery class. (protocol wire-Handle + object-model chapters)
- **ADR-0005 — Two-layer framing.** The transport frame is a small header `{magic, channel, len}` wrapping an opaque body; that body is the protocol frame `FrameHeader{version, flags, kind, opcode, req_id, seq, body_len}` + payload, with MAGIC only in the transport header. *Rationale:* one struct defined once per layer keeps transport and protocol independently evolvable without divergent re-declarations. (transport + protocol chapters)
- **ADR-0006 — `seq` and `req_id` are distinct.** `seq:u64` is the per-session monotonic ordering/fence sequence (sync chapter); `req_id:u32` is the request/response correlation id (protocol chapter); there is no u32 ordering seq. *Rationale:* conflating ordering with correlation breaks fence semantics and out-of-order async replies. (sync + protocol chapters)
- **ADR-0007 — Opcode is `u32 = (ApiId<<24) | call_id`.** ApiId table: Vulkan=0x01, GL=0x02, CUDA=0x03, OpenCL=0x04, HIP=0x05, LevelZero=0x06, Video=0x07. *Rationale:* a fixed namespace lets the decoder dispatch per family without collisions as families are added. (protocol + appendices chapters)
- **ADR-0008 — 64-bit wire Handle layout.** Top 8 bits = kind/API-namespace, middle bits = generation, low bits = slot index; one layout shared by the wire `Handle` and the object-model `HandleId`, retiring any slot whose generation is about to wrap. *Rationale:* a single layout makes the ABA guard and forged-handle rejection identical everywhere. (protocol + object-model chapters)
- **ADR-0009 — One canonical bulk descriptor.** `ShmSlice{ offset:u32, len:u32, gen:u32 }` (shared region ≤ 4 GiB; `gen` = slot/epoch guard) and one canonical `ShmHeader`, both defined in `graftx-transport`. *Rationale:* the memory chapter references these rather than redefining them, so the bulk ABI has exactly one source of truth. (transport owns; memory chapter references)
- **ADR-0010 — Inline-vs-bulk threshold = 4 KiB.** Single documented default, per-API tunable, owned by the memory chapter and quoted identically by the performance chapter. *Rationale:* one number avoids the two chapters drifting and mis-sizing the copy budget. (memory + performance chapters)
- **ADR-0011 — Negotiated cap `max_frame_body`, default 1 MiB.** Named and defaulted consistently across the protocol and transport chapters. *Rationale:* a shared cap name prevents handshake-time disagreement on the maximum protocol body. (protocol + transport chapters)
- **ADR-0012 — Handshake messages are `Hello`/`Welcome`.** `Welcome` carries `{proto_major, proto_minor, features, max_frame_body, session_id}`. *Rationale:* fixed names/fields let the protocol and roadmap chapters describe the same handshake. (protocol + roadmap chapters)
- **ADR-0013 — Native driver faults caught via SEH/VEH, not `siglongjmp`.** A Windows-server access violation is caught by `__try/__except` or a vectored exception handler at the FFI seam; `catch_unwind` catches only Rust panics in glue, never a hardware fault, and after a driver access violation the safe blast radius is the whole driver/session (tear it down). *Rationale:* a corrupted driver cannot be safely resumed, so containment, not recovery, is the contract. (server + security + error/FFI chapters)
- **ADR-0014 — Panic strategy: client `panic=abort`, server unwinds.** The client cdylib is built `panic=abort` and the server bin with default unwind via *separate* cargo invocations (no illegal `[profile.*.package.*]` panic override); the unwinding server uses `parking_lot` mutexes so there is no lock poisoning. *Rationale:* a guest-side panic must not unwind across the C ABI, while the server stays recoverable. (error/FFI + build chapters)
- **ADR-0015 — Client cdylib SONAME = `libgraftx_client.so.<proto_major>`.** At v0.0.0 `proto_major=0`, so it is `libgraftx_client.so.0`. *Rationale:* tying the SONAME to the protocol major lets the loader refuse an incompatible shim. (build + versioning chapters)
- **ADR-0016 — CUDA kernel-param layout parsed from cubin/fatbin ELF metadata.** Read `.nv.info.<func>` (`EIATTR_KPARAM_INFO` / `EIATTR_CBANK_PARAM_SIZE`), not `cuFuncGetAttribute`; the guest receives the real server `CUdeviceptr` (an opaque integer it never dereferences) so kernel-arg pointer slots already hold real device addresses, and launch config is validated client-side so invalid-config errors return synchronously. *Rationale:* ELF metadata gives exact parameter offsets the driver query cannot, and real device pointers avoid a surrogate-pointer reconciliation pass. (CUDA chapter)
- **ADR-0017 — Vulkan WSI acquire-signal forwarding + guaranteed host-visible memory.** The acquire-supplied semaphore/fence is handled by forwarding an acquire-signal op so the server injects the real semaphore signal (or a dummy signaling submit) a later `vkQueueSubmit` wait observes; GraftX always advertises ≥1 `HOST_VISIBLE|HOST_COHERENT` memory type, with flush-mapped-range-on-submit as the primary coherent mechanism. *Rationale:* WSI sync objects created guest-side must map to real server signals, and Vulkan requires a host-visible coherent type to exist. (Vulkan + presentation chapters)
- **ADR-0018 — Presentation default capture = GPU→staging→ivshmem (2-copy).** The always-correct 2-copy path is the default; direct GPU readback into the imported shared region is an opportunistic optimization probed at negotiation, not the default. *Rationale:* the 2-copy path works on every driver, so correctness ships first and the zero-copy fast path is a negotiated bonus. (presentation chapter)

### Maintaining the log

New ADRs are numbered monotonically; the next free number is reserved at PR time. An ADR is never edited after `Accepted` except to set `Status: Superseded by ADR-MMMM`. PRs that introduce a cross-cutting choice (anything touching the `Transport` trait, the wire format, the handle model, or the sandbox boundary) **must** land an ADR in the same change; reviewers reject "silent" architectural decisions. The risk table in 31.2 is re-scored each milestone, and a risk that drops to `L1` or is fully mitigated is moved to a "Retired risks" appendix with the ADR/commit that closed it, preserving the audit trail rather than deleting history.
