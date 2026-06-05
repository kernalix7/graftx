# 23. Security Architecture & Threat Model

The complete threat model for replaying an untrusted GPU command stream against native drivers, and the layered defenses — decode-time validation, copy-to-private TOCTOU mitigation, server sandboxing, quotas/backpressure, per-session auth, hypervisor write-revocation, and decoder fuzzing — that GraftX will use to contain it.

This chapter is the security spine of the project. It does not re-derive the wire format (the Protocol chapter (Ch. 06)), the ivshmem BAR layout and allocator (the Memory chapter (Ch. 12)), the server decode→validate→replay loop (the Server core chapter (Ch. 10)), or the handle table (the Handles chapter (Ch. 11)); it states the *adversary* those mechanisms must withstand and the *invariants* they must collectively uphold. Everything below is forward-looking design at v0.0.0. The priority order for the whole project is breadth > performance > stability > safety, but inside the server's trust boundary that order inverts: the server replays a stream it must assume is hostile, so for the server core, safety dominates.

## 23.1 Trust boundaries and assets

GraftX spans two guests on one host, connected by a paired control plane (virtio-vsock) and a bulk plane (ivshmem). The asset we are protecting is the **Windows guest that owns the GPU via PCI passthrough**, and transitively the host: a GPU driver compromise is frequently a kernel-level compromise, and a compromised passed-through device can DMA. The server is the gate in front of that asset.

```text
   LINUX GUEST (client side)                 WINDOWS GUEST (server side)
 ┌───────────────────────────┐            ┌────────────────────────────────┐
 │ guest app (UNTRUSTED)      │            │ graftx-server (sandboxed)       │
 │  └ graftx-client shim      │            │   decode → VALIDATE → replay    │
 │      serialize calls       │  vsock     │      │                          │
 │      write bulk to shm ────┼───────────►│   copy-to-private ── native     │
 │                            │  ivshmem   │      drivers ── PHYSICAL GPU     │
 └───────────────────────────┘  (shared)  └────────────────────────────────┘
        TRUST = none                              TRUST = enforces policy
```

Four distinct adversaries fall out of this picture:

| # | Adversary | Capability | Primary defense |
|---|-----------|-----------|-----------------|
| A1 | Malicious/buggy guest app | Emits arbitrary opcode bytes & bulk contents through the shim | Decode-time validation (§23.4), copy-to-private (§23.5) |
| A2 | In-guest attacker on Linux | Bypasses the shim, speaks the wire protocol raw, races shared memory | Same validation + TOCTOU mitigation + per-session auth (§23.7) |
| A3 | Compromised/rogue *other* guest on the host | Reaches the vsock CID or maps a misconfigured ivshmem region | Reachability scoping + auth + hypervisor isolation (§23.8) |
| A4 | Compromised server post-driver-exploit | Already inside the Windows guest | Sandbox containment (§23.6) limits blast radius |

The shim is explicitly **not** a trust boundary. We assume the byte stream arriving at the server is fully attacker-controlled, because an attacker who controls the app (A1) controls everything the shim produces, and a raw-protocol attacker (A2) skips the shim entirely. Anything the server believes only because "the shim wouldn't send that" is a vulnerability. The shim's job is *correctness and performance for cooperative clients*, never enforcement.

Conversely, the client side is **not** something the server tries to protect: if the Windows guest is compromised it can lie to the Linux guest, and that is out of scope (the Linux guest is the lower-value asset and is the one we already assume is hostile). Result/surface data flowing back to Linux is validated for framing sanity only, not treated as adversarial — see §23.9 for the asymmetry.

## 23.2 The core problem: replaying an untrusted stream against native drivers

Native GPU drivers are written assuming a *cooperative, in-process* caller. They were never hardened against an adversary choosing every argument. A real `vkCmdDraw` or `glTexImage2D` reaching the driver with attacker-chosen integers can: read out-of-bounds (info leak from another tenant's VRAM), write out-of-bounds (corruption / RCE), trigger driver assertions that crash the GPU context for *all* sessions, or wedge the device into a TDR (timeout detection & recovery) storm. The remoting layer turns "a process that links libGL" into "a network-reachable attacker," vastly widening the threat surface a driver faces.

We cannot fix the drivers. The design therefore treats the validator as a *protocol-aware firewall* sitting between the decoded command and the driver entry point. The governing invariant, inherited from the Server core chapter (Ch. 10) §10.1, is restated as a security rule:

> **No native driver entry point is ever called with a value the validator has not range-checked, type-checked, and resource-checked against the live session state.**

This is enforced structurally, not by discipline: the only function that may call a driver is `dispatch(cmd: ValidatedCommand)`, and `ValidatedCommand` is constructed solely by the validator. The raw decoded `Command` does not implement the trait that `dispatch` accepts. The type system makes "call the driver with unvalidated input" not express.

```rust
/// Output of the wire decoder (Ch. 06). Untrusted: every field is attacker-chosen.
pub struct Command { pub op: OpCode, pub args: RawArgs<'_>, pub bulk: Option<ShmSlice> }

/// Only the validator can mint this. Carries borrows into server-PRIVATE memory,
/// never into the shared region. Backends accept ONLY this type.
pub struct ValidatedCommand<'a> { op: OpCode, args: TypedArgs, payload: Option<&'a [u8]> }

impl Validator {
    pub fn check<'a>(&mut self, cmd: Command, st: &SessionState, scratch: &'a Arena)
        -> Result<ValidatedCommand<'a>, Reject> { /* §23.4 */ }
}
```

## 23.3 The decoder as attack surface

Before validation can run, the wire bytes must be decoded (the Protocol chapter (Ch. 06)). The decoder is the *first* code an attacker reaches and the most dangerous: it parses untrusted, variable-length, self-describing data. Classic remoting vulnerabilities live here — integer-overflow in a length field that then drives an allocation or a slice bound, an opcode index used to pick into a dispatch array, a "count" field multiplied by an element size.

Design rules for the decoder, all checkable by `clippy -D warnings` and review:

- **No `unsafe` indexing on untrusted input.** All slicing goes through `get(..)`/`split_at_checked`, never `[..]` on attacker-controlled bounds.
- **Checked arithmetic on every length-derived value.** `count.checked_mul(elem_size).ok_or(Reject::Overflow)?` — never bare `*`. A lint (`clippy::arithmetic_side_effects`) will be enabled for the decoder module specifically.
- **A hard frame-size ceiling** (the negotiated `max_frame_body`, default 1 MiB, for inline; bulk goes through shm not the frame) checked *before* any per-field parse, so a 4 GiB length field is rejected in O(1), not after attempting to read 4 GiB.
- **Opcode → handler via a dense, bounds-checked table**, with unknown opcodes rejected (not skipped), so an attacker cannot smuggle an opcode the validator does not know how to constrain.
- **No recursion on attacker-controlled depth.** Nested structures (e.g. Vulkan `pNext` chains) decode with an explicit depth counter and a ceiling; the alternative is a stack-overflow DoS.

The decoder is the highest-value fuzz target in the codebase; §23.10 covers the harness.

## 23.4 Validation strategy

Validation is per-opcode and stateful. It runs after decode, before dispatch, on the Replay thread, and it has access to live `SessionState` (the handle table from the Handles chapter (Ch. 11), bound objects, negotiated limits). The checks fall into five tiers, applied in this order so the cheapest reject fires first:

1. **Structural** — already partly enforced by the decoder: arg count and types match the opcode's signature.
2. **Range / enum** — every enum-typed argument is in the set the backend accepts (e.g. a GL `target` is one of the known targets; a Vulkan `VkFormat` is in range *and* in the negotiated feature set). Reject, never clamp — clamping silently changes program semantics and hides bugs.
3. **Handle ownership** — every object handle in the command resolves, through the session's handle table, to a live server object that *this session created*. Cross-session handle access is the canonical confused-deputy attack and is rejected here (the Handles chapter (Ch. 11) owns the table; this chapter mandates the check).
4. **Resource / quota** — the command would not exceed the session's negotiated limits (max texture dim, max allocation, max live objects). This dovetails with §23.6 quotas.
5. **Semantic / dependency** — the command is legal given current bound state (e.g. a draw call requires a bound pipeline; a `glTexSubImage2D` region fits inside the previously-declared texture extent recorded in `SessionState`).

```rust
fn check_tex_subimage(c: &Command, st: &SessionState, sc: &Arena)
    -> Result<ValidatedCommand<'_>, Reject>
{
    let a = c.args.tex_subimage()?;                                   // (1) structural
    if !FORMAT_TABLE.allows(a.format) { return Err(Reject::Enum); }   // (2) range/enum
    let tex = st.handles.texture(a.tex).ok_or(Reject::BadHandle)?;    // (3) ownership
    let bytes = a.width.checked_mul(a.height)                         // overflow guard
        .and_then(|p| p.checked_mul(a.format.bpp()))
        .ok_or(Reject::Overflow)?;
    if bytes as u64 > st.limits.max_upload { return Err(Reject::Quota); } // (4) quota
    if !tex.extent().contains(a.region()) { return Err(Reject::Range); } // (5) semantic
    // Bulk payload is copied to private memory HERE, then re-measured (§23.5).
    let priv_buf = st.copy_in(c.bulk, bytes, sc)?;
    Ok(ValidatedCommand::tex_subimage(a, priv_buf))
}
```

**Tradeoff — per-command cost vs. breadth.** Hand-writing a `check_*` for thousands of entry points across a dozen APIs is the dominant cost of the security model and directly taxes the "breadth" priority. The plan mitigates this two ways: (a) a *table-driven* validator for the large mechanical majority of calls, where an opcode's metadata declares its arg kinds, handle slots, and length-bearing fields, and a generic checker walks the table; (b) hand-written checks reserved for the genuinely stateful calls (draws, descriptor binds, memory maps). This keeps the bespoke surface to dozens of functions, not thousands.

```rust
/// Per-opcode validation metadata, generated where possible from API headers.
pub struct OpSpec {
    pub args:    &'static [ArgKind],        // Enum{table}, Handle{kind}, Scalar, Len{of}
    pub handles: &'static [ArgSlot],        // which args are object handles
    pub bulk:    Option<BulkSpec>,          // how to size & bound the bulk payload
    pub custom:  Option<fn(&Command,&SessionState,&Arena) -> Result<ValidatedCommand,Reject>>,
}
```

A rejected command does not crash the session by default; it emits an error frame correlated to the originating sequence number and the session continues (a buggy app should get an error back, like a real driver returning `GL_INVALID_VALUE`). A *malformed-at-the-decoder* frame, by contrast, is unrecoverable (we have lost stream sync) and tears the session down. The boundary between "recoverable reject" and "fatal" is itself a security decision recorded per-opcode.

## 23.5 Copy-to-private and the TOCTOU window

The bulk plane is shared memory the client can write at any instant. This creates a textbook **time-of-check-to-time-of-use** race: the validator reads a length/format header from a shared-region payload at time T_check, approves it, and the driver consumes the data at time T_use. Between those instants the client (A1/A2) can rewrite the bytes — shrinking a buffer the server already sized, changing a format the validator already approved, or smuggling a different shader. If the driver reads from shared memory directly, validation is meaningless.

The mitigation, mandated here and implemented in the Memory chapter (Ch. 12) §12.5, is **copy-to-private before validate-the-copy, then use only the copy**:

```text
   client writes shm  ──►  ┌─ T_copy: server memcpy shm → private buf ─┐
                           │  (single snapshot; client can't reach it)  │
   client may rewrite shm  │  T_check: validate the PRIVATE buffer      │
   (irrelevant now) ───────┘  T_use:   driver reads PRIVATE buffer      │
                                       check & use see identical bytes ─┘
```

The ordering is the whole point: copy first, then validate *the copy*, then use *the copy*. Validating the shared region and *then* copying would re-open the window during the copy. After the snapshot, the shared region is never read again for this command; any further client writes hit memory the server has already abandoned. The private buffer comes from a per-session bump arena (`scratch`/`Arena` above) sized by the quota system, reset per command or per batch, so the copy does not become an unbounded allocation channel.

Two subtleties the design must nail:

- **Self-referential payloads.** Some payloads embed their own length/offset fields (e.g. a serialized command buffer). The copy bounds must come from the *control-plane frame* (which is single-snapshot the moment we read it off vsock), not from a length field living *inside* the shared payload. A length read from shared memory is itself TOCTOU-able. Rule: **sizes that bound a shared-region read must originate from the control plane, never from the shared region.**
- **Persistently-mapped buffers** (the Memory chapter (Ch. 12) §12.7) appear to defeat copy-to-private because the app keeps the mapping live across many ops for performance. For these, the design accepts that the *guest* sees a live mapping but the *server* still snapshots-on-flush: each explicit flush/coherent-barrier triggers a bounded copy of the dirtied range into private memory, and the driver only ever sees the snapshot. The performance cost is one copy per flush, which §12 already budgets.

Even copy-to-private cannot stop a client from racing within a single payload *before* the copy — but that is harmless, because whatever bytes happen to be present at T_copy are exactly what gets validated and used; there is no inconsistency between check and use, which is the only property TOCTOU mitigation must guarantee. True write-revocation (preventing the client from writing at all during the window) is a hypervisor-layer concern, §23.8.

## 23.6 Sandboxing the server

Validation reduces the *probability* of a driver exploit; the sandbox reduces its *blast radius* (adversary A4). The server runs the riskiest code in the system: a parser plus a replay engine driving closed-source kernel-mode GPU drivers. We assume some such driver will eventually be exploitable and design so that a server compromise does not trivially become host compromise.

Proposed Windows-guest containment, defense-in-depth:

- **Least-privilege service account.** The server runs as a dedicated low-privilege account with no interactive logon, no admin group, write access only to its own state directory and the GPU device.
- **AppContainer / job object.** Run inside an AppContainer (or at minimum a Job Object with `JOB_OBJECT_LIMIT_*` set) to cap working set, CPU, and active processes, and to deny network egress beyond the vsock/ivshmem devices it needs. Token restricted to the capabilities the GPU stack requires.
- **Per-session GPU context isolation, not per-session process (default).** A process-per-session model is the strongest isolation but multiplies GPU context overhead and driver memory; the default is one server process with per-session GPU contexts and strict handle-table partitioning (the Handles chapter (Ch. 11)). A *process-per-session* mode is a planned hardening option for multi-tenant deployments where cross-session VRAM leakage must be structurally impossible.
- **No filesystem reach from decoded commands.** Some GPU APIs accept file paths (e.g. shader cache hints, certain CUDA/OptiX module loads). Any such argument is rejected or redirected to a sandboxed scratch path; the untrusted stream never names a host path.
- **Crash containment.** A driver-induced access violation in a session worker is a *hardware fault*, not a Rust panic, so it is caught with SEH (`__try`/`__except`) or a vectored exception handler at the FFI seam — never `siglongjmp`, and never `catch_unwind` (which catches only Rust panics in the glue, never a hardware fault). Once a native driver has faulted its state is presumed corrupt, so the safe blast radius is the whole driver/session: the handler converts the fault to a full session teardown plus error frame, not a process abort, so one hostile session cannot DoS all sessions. A GPU TDR is detected and the device context recreated where the driver permits. (See the Error/FFI chapter (Ch. 24) for the FFI-seam mechanism and the Server core chapter (Ch. 10) for the teardown path.)

```text
 host ── hypervisor ── Windows guest ── AppContainer ── graftx-server ── GPU driver
   ▲ A4 must pierce: SEH→teardown, AppContainer caps, low-priv token,
   │ guest OS boundary, hypervisor isolation — five layers below the driver bug
```

**Tradeoff.** Tighter sandboxing fights performance (priority #2) and breadth (some APIs genuinely want filesystem/IPC). The plan resolves conflicts in favor of containment *inside the server boundary only*; the client side stays fast and unsandboxed because it is already untrusted and protects nothing.

## 23.7 Resource limits, quotas, and backpressure

An attacker who cannot corrupt memory will try to exhaust it. Denial of service is the easiest attack and gets first-class limits. Every limit is **negotiated per session at handshake** (the Server core chapter (Ch. 10) §10.5) with server-enforced ceilings the client cannot raise, and is checked in validation tier 4 (§23.4).

| Resource | Limit (negotiated, server-capped) | Enforcement point |
|----------|-----------------------------------|-------------------|
| Inline frame size | ≤ `max_frame_body`, 1 MiB default | decoder (§23.3) |
| Bulk payload per command | ≤ `max_upload` | validator tier 4 |
| Private scratch arena | per-session high-water cap | `copy_in` (§23.5) |
| Live objects per kind | per-handle-table cap | handle table (Ch. 11) |
| Total VRAM / device allocs | per-session byte budget | allocator + validator |
| In-flight commands | bounded queue depth | reader → replay queue |
| Command rate | token bucket (cmds/s, bytes/s) | reader |

**Backpressure, not unbounded buffering.** The reader→replay queue is bounded. When it fills (replay is slower than the client submits — common, since GPU work dominates), the reader *stops reading the vsock socket*, which propagates flow-control back through vsock to the client, which blocks in its shim. This is the design's anti-DoS keystone: a flooding client cannot make the server allocate without bound; it can only make *itself* block. There is no scenario where accepting more input grows server memory past the negotiated caps. Quotas and backpressure are detailed in the dedicated quotas chapter; this chapter mandates that **every unbounded resource is a vulnerability** and must be tied to a negotiated, server-enforced ceiling.

A deliberately slow-draining client (Slowloris-style) is handled by an idle/stall timeout: a session making no replay progress while holding queue slots is reaped.

## 23.8 Per-session authentication, integrity, and channel scoping

The paired-guest channel is **reachability scoping, not authentication.** vsock pairing means only the paired Linux guest CID can connect, and ivshmem means only guests sharing that device can see the region. That stops *unrelated* guests, but it does not prove the peer is the legitimate client, and it does nothing if a third VM is misconfigured onto the same ivshmem device (A3). The plan therefore layers an explicit session protocol on top:

- **Handshake auth (planned).** At connect, client and server perform a mutual handshake keyed by a per-pair shared secret provisioned out-of-band (the VM orchestration layer that wires up the vsock/ivshmem pairing also drops a key into each guest). This authenticates *which* paired client is speaking, defeating an A3 that merely gained channel reachability.
- **Session integrity / replay protection (planned).** The per-session monotonic ordering sequence `seq` (already needed for fence/ordering, owned by the Sync chapter (Ch. 13); distinct from the `req_id` request/response correlation id owned by the Protocol chapter (Ch. 06)) plus an optional per-frame MAC under the session key, so an on-channel attacker cannot inject or replay frames into an established session. MAC is optional because for a *single trusted host* the channel is already host-internal; it becomes mandatory in deployments where the host or hypervisor is not fully trusted between the two guests.
- **Epoch fencing on the shared region.** The `ShmHeader.epoch` (the canonical `ShmHeader` is owned by the Transport chapter (Ch. 08); the Memory chapter (Ch. 12) references it) is bumped on any reset/reattach; the server rejects bulk slices whose `ShmSlice.gen` was minted under a stale epoch, preventing a reconnecting or rogue peer from referencing slices from a previous session's heap.

What auth explicitly does **not** buy us: it does not make the *content* trustworthy. An authenticated, legitimate client whose app is compromised (A1) sends fully malicious commands over a perfectly authenticated channel. Auth defeats A3 channel-hijack; only validation (§23.4) defeats A1/A2 content. The two are orthogonal and both required.

## 23.9 Hypervisor-level write-revocation and the residual gap

Copy-to-private (§23.5) guarantees *consistency* between check and use, but it cannot stop the client from *writing* the shared region during the window — it only stops those writes from mattering. There is one property software in the server cannot enforce: actually revoking the client's write access to a shared region while the server reads it. The BAR is mapped writable into the client guest by the ivshmem device; only the hypervisor / ivshmem-device layer can flip that mapping read-only.

The design's position:

- **For correctness, write-revocation is unnecessary** — copy-to-private already makes client races on shared memory semantically irrelevant, as argued in §23.5.
- **For defense-in-depth against side channels and for stronger multi-tenant isolation, write-revocation is desirable** and is listed as a hypervisor-layer enhancement, not a server feature. Where the ivshmem device or hypervisor supports per-region permission flips (e.g. a doorbell that transitions a sub-region to server-exclusive for the duration of a copy), the protocol will use it; where it does not, the server falls back to copy-to-private alone and documents the residual: a malicious co-resident guest with the same BAR mapping (A3 that pierced scoping) could observe or perturb in-flight bulk data. This residual is fundamental to shared memory and is called out so deployers can choose stronger hypervisor isolation when their threat model demands it.

```text
 layer              property it can enforce
 ─────────────────  ────────────────────────────────────────────
 server (software)  consistency  (copy-to-private)          ✔ planned, default
 ivshmem / hypervi.  write-revocation (true exclusivity)     ✔ planned, when supported
                     region permission flips, per-tenant BARs ✗ deployment-dependent
```

## 23.10 Fuzzing the decoder and validator

Because the decoder (§23.3) and validator (§23.4) are the entire software trust boundary, they get continuous fuzzing as a CI gate, not an afterthought. The plan:

- **`cargo fuzz` (libFuzzer) targets**, one per decode entry and one for the decode→validate pipeline, run in CI with a time budget and on a nightly long-soak. The targets take raw bytes (exactly what an attacker controls) and must never panic, never `unwrap`, never allocate unbounded, never read OOB.

```rust
fuzz_target!(|data: &[u8]| {
    // Property: decoding arbitrary bytes either yields a Command or a clean
    // Reject — never a panic, never an unbounded alloc, never UB.
    let _ = graftx_protocol::decode_frame(data);
});
```

- **Structure-aware fuzzing** via `arbitrary` once the wire types stabilize, so the fuzzer spends its budget on *semantically deep* inputs (well-formed frames with adversarial field values that reach tier-5 semantic checks) rather than bouncing off the length ceiling. Stateful fuzzing replays a *sequence* of commands against a mock backend to surface validator state-machine bugs (e.g. a handle freed then reused).
- **Differential / oracle checks.** A debug-build "paranoid validator" re-checks invariants the release validator assumes, and a CI job asserts the two agree; divergence is a bug in one of them.
- **Sanitizers.** Fuzz and the FFI-heavy backend tests build under ASan/UBSan where the toolchain allows, since the danger zone is precisely the `unsafe`/FFI seam (per project policy: all `unsafe` isolated with `// SAFETY:` comments, no `unwrap` in lib paths). Any fuzz crash is a release-blocking defect.
- **Corpus from real traces.** Captured (sanitized) command streams from M1–M5 bring-up seed the corpus so the fuzzer explores realistic opcode distributions, not just the synthetic ones.

The acceptance bar: the decoder and validator must survive an unbounded-time fuzz with zero panics and zero sanitizer findings before any API backend is declared production-ready. Fuzzing is how we gain confidence that the §23.2 invariant — *no driver call with an unvalidated value* — actually holds against inputs we did not think of.

## 23.11 Summary of invariants

The chapter's mandates, restated as testable invariants the rest of the codebase must satisfy:

1. The byte stream is untrusted; the shim is not a trust boundary (§23.1).
2. No driver entry point is called except through a `ValidatedCommand` minted only by the validator (§23.2, §23.4).
3. The decoder never panics, never indexes/arithmetics unchecked on untrusted input, and rejects unknown opcodes and oversized frames in O(1) (§23.3).
4. Every shared-region payload is copied to private memory *before* validation, and only the private copy is validated and used; bounds for that copy come from the control plane, never from the shared region (§23.5).
5. The server runs sandboxed, contains driver crashes to a session, and never lets an untrusted command name a host path (§23.6).
6. Every resource has a negotiated, server-capped ceiling; the input path is backpressured, never unboundedly buffered (§23.7).
7. Channel scoping is not authentication; a per-session auth handshake and optional integrity MAC defeat channel-level adversaries, orthogonally to content validation (§23.8).
8. Write-revocation is a hypervisor-layer property; the server provides consistency, the hypervisor (when available) provides exclusivity, and the residual gap is documented (§23.9).
9. The decoder and validator are fuzzed as a CI gate; zero panics, zero sanitizer findings is the bar (§23.10).
