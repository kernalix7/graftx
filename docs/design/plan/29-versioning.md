# 29. Versioning, Compatibility & Deprecation

This chapter defines how GraftX will version its crates and wire protocol, negotiate capabilities across a client/server skew, maintain forward/backward compatibility windows, and retire features safely.

## 29.1 The three independent version axes

GraftX has *three* version namespaces that evolve at different rates and MUST NOT be conflated. Treating them as one number is the single most common source of remoting-layer breakage, so the design keeps them explicit.

| Axis | Carrier | Granularity | Who reads it | Bumped when |
|------|---------|-------------|--------------|-------------|
| **Crate SemVer** | `Cargo.toml` per crate | `MAJOR.MINOR.PATCH` | Rust compiler, downstream linkers | Public Rust API of a crate changes |
| **Wire protocol version** | `proto_major`/`proto_minor` in the `Welcome` handshake frame | `(major, minor)` u16 pair | client shim ↔ server at connect | Frame layout, opcode space, or transport framing changes |
| **API-surface capability bits** | `CapabilitySet` negotiated post-handshake | per-API feature flags | each API replay backend | a GL/Vulkan/CUDA/… opcode group is added, changed, or removed |

The crate SemVer governs the *Rust* contract (defined per the build/dist chapter (Ch. 28)). The wire protocol version governs whether two binaries can even speak to each other. The capability set governs *which subset* of intercepted APIs both ends actually support — this is where GraftX's "breadth of API coverage" priority lives, because a new OpenCL or Level Zero command group can ship as additive capability bits without touching the wire major version at all.

```text
graftx-client v0.0.0  (crate SemVer)
        │  links graftx-protocol v0.0.x
        ▼
  ┌─────────────────────────────────────────────┐
  │ Welcome frame: wire proto (0,4)              │  ← protocol version
  │ caps: GL=0x3F EGL=0x07 VK=0x1FF CUDA=0x03 …  │  ← capability bits
  └─────────────────────────────────────────────┘
        ▼
graftx-server v0.0.0  (crate SemVer, independent of client patch)
```

A key invariant: **the wire protocol version is owned by `graftx-protocol` but is NOT the same as that crate's SemVer.** `graftx-protocol` may go from `0.0.3` to `0.0.4` (a docs fix) without touching wire `(0,4)`. Conversely a wire bump from `(0,4)` to `(0,5)` is an additive `graftx-protocol` MINOR bump pre-1.0, or could even be a PATCH if the new opcodes are purely optional. The mapping is encoded as a constant table, never derived from `CARGO_PKG_VERSION`:

```rust
// graftx-protocol/src/version.rs

/// Wire protocol version: bumped on frame/opcode/framing changes only.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

/// The version this build *prefers* to speak. At crate v0.0.0 the wire
/// major is 0; the client cdylib SONAME tracks it as libgraftx_client.so.0.
pub const WIRE_VERSION: ProtocolVersion = ProtocolVersion { major: 0, minor: 4 };

/// The oldest wire major this build can still decode (back-compat floor).
pub const MIN_WIRE_MAJOR: u16 = 0;

impl ProtocolVersion {
    /// Two peers can talk iff majors match. Minor is negotiated down.
    pub const fn compatible_with(self, other: ProtocolVersion) -> bool {
        self.major == other.major
    }
}
```

## 29.2 SemVer policy, pre- and post-1.0

GraftX is at `v0.0.0`. Cargo's SemVer rules treat `0.x` specially: a bump of `x` (the MINOR field) is allowed to break API. The project will exploit this **only for the Rust crate surface**, and will hold the *wire* contract to a stricter standard even pre-1.0, because a broken wire contract is invisible at compile time and only fails at runtime against a peer the developer may not control.

**Pre-1.0 (the current and near-term regime):**

- `graftx-protocol`, `graftx-transport`: breaking Rust API changes are permitted on `0.MINOR` bumps, but each such bump MUST be accompanied by a CHANGELOG entry and a migration note. These two crates are the most-depended-upon, so churn here is the most expensive.
- `graftx-client`: its *Rust* API is nearly irrelevant (it is a cdylib exporting C symbols — see the client shim chapter (Ch. 09)); the *C ABI* it exports is the real contract and is governed by the symbol-versioning rules in §29.6, not Cargo SemVer.
- `graftx-server`: a binary, so it has no library SemVer obligations to external consumers; its only contract is the wire protocol.

**Post-1.0 (target regime, declared once the GL/EGL/Vulkan core paths are stable):**

- Standard strict SemVer for all crates. A breaking Rust API change requires a MAJOR bump.
- The wire protocol `major` and the lead crate `MAJOR` will be *loosely coupled but not locked*: a wire major bump forces at least a crate minor bump, but a crate major bump (e.g. an internal trait refactor) does NOT force a wire major bump.

The reason for decoupling, stated as a tradeoff: locking wire-major to crate-major is simpler to explain but forces a flag-day across every deployed guest pair whenever the Rust internals are refactored. Since GraftX deployments are paired-guest VMs that may be upgraded independently (the Windows server guest and the Linux client guest are patched on different cadences by ops), an unnecessary flag-day is a real operational cost. Decoupling lets the server be rebuilt with new internals while still speaking wire `(1,4)` to an un-upgraded client.

## 29.3 Frame-level forward compatibility

Wire frames are designed so that a *minor* version bump never breaks an older decoder. The two mechanisms are **length-prefixed self-describing fields** and a **reserved tail**. The protocol `FrameHeader` (owned by the Protocol chapter (Ch. 06): `{version, flags, kind, opcode, req_id, seq, body_len}`) wraps each opcode payload; the relevant fields for forward compatibility are sketched below:

```text
 +--------+--------+----------------+
 | opcode | flags  |  body_len (LE) |   opcode is u32 = (ApiId<<24)|call_id
 +--------+--------+----------------+
 |  ... body_len bytes of opcode payload ...     |
 +-----------------------------------------------+
```

Rules that make minor bumps non-breaking:

1. **Unknown opcodes are skippable, not fatal — conditionally.** Optionality is carried by a frame `flags` bit, not by stealing an opcode bit (the opcode is the full `u32 = (ApiId<<24) | call_id` from the Protocol chapter (Ch. 06), so no high bit is free). An opcode flagged *optional* is skipped by a decoder that does not recognize it (it advances `body_len` bytes and continues). A *mandatory* opcode that is unknown is a protocol error and tears the session down. New, additive features (a new query, a new hint) are introduced as optional opcodes first; only after a wire-major bump may they become mandatory.
2. **`flags` reserves spare bits.** Unknown flag bits MUST be ignored by older decoders, never rejected.
3. **Trailing payload bytes are reserved.** A v`(0,4)` decoder reading a `glDrawElements` payload reads exactly the fields it knows and ignores any bytes between its known-field-end and `body_len`. A v`(0,5)` encoder may append a new field; the v`(0,4)` server simply does not see it.

This is the classic "ignore-the-tail" forward-compat scheme. The explicit tradeoff: it forbids ever *reinterpreting* an existing byte offset within a major version. Once `glDrawElements` payload byte 8 means `index_type`, it means that for all of wire major 0. Repurposing it requires `major` 1. The protocol crate enforces this with a frozen-layout test (see §29.7).

## 29.4 The handshake and negotiation sequence

Connection setup negotiates, in order: wire version, then capability sets. The control-plane handshake runs over the vsock channel before any ivshmem bulk region is mapped (the bulk transport, chapter 6, is set up only after caps are agreed, because the shared-memory layout itself is capability-dependent).

```text
client (Linux shim)                         server (Windows replay)
   │                                              │
   │  HELLO { wire_pref=(1,4), wire_min_major=1,  │
   │          client_caps=CapabilitySet,          │
   │          session_nonce }                     │
   │─────────────────────────────────────────────▶
   │                                              │  pick wire = min(minor) on matching major
   │                                              │  intersect caps
   │  HELLO_ACK { wire=(1,4),                      │
   │              agreed_caps=CapabilitySet,       │
   │              server_limits=ResourceQuota }    │
   ◀─────────────────────────────────────────────│
   │  (or REJECT { reason } and close)            │
   │                                              │
   │  ── now both encode/decode at wire (1,4) ──  │
   │  ── only agreed_caps opcodes are legal ──    │
```

Negotiation logic on the server:

```rust
pub enum HandshakeOutcome {
    Accept { wire: ProtocolVersion, caps: CapabilitySet },
    Reject(RejectReason),
}

#[derive(thiserror::Error, Debug)]
pub enum RejectReason {
    #[error("wire major mismatch: client {client}, server supports {lo}..={hi}")]
    WireMajorMismatch { client: u16, lo: u16, hi: u16 },
    #[error("no overlapping capabilities for any usable API")]
    EmptyCapabilityIntersection,
    #[error("client requires mandatory opcode group {0:?} server lacks")]
    MissingMandatoryCaps(ApiGroup),
}

pub fn negotiate(hello: &Hello, server: &ServerCaps) -> HandshakeOutcome {
    // 1. Wire major MUST match exactly.
    if hello.wire_pref.major < server.min_major
        || hello.wire_pref.major > server.wire.major
    {
        return HandshakeOutcome::Reject(RejectReason::WireMajorMismatch {
            client: hello.wire_pref.major,
            lo: server.min_major,
            hi: server.wire.major,
        });
    }
    // 2. Minor negotiated DOWN to what both can speak.
    let minor = hello.wire_pref.minor.min(server.wire.minor);
    let wire = ProtocolVersion { major: hello.wire_pref.major, minor };

    // 3. Capabilities intersected; client's mandatory caps must be satisfied.
    let caps = hello.client_caps.intersect(&server.caps);
    if let Some(missing) = hello.client_caps.unmet_mandatory(&caps) {
        return HandshakeOutcome::Reject(RejectReason::MissingMandatoryCaps(missing));
    }
    if caps.is_empty() {
        return HandshakeOutcome::Reject(RejectReason::EmptyCapabilityIntersection);
    }
    HandshakeOutcome::Accept { wire, caps }
}
```

`CapabilitySet` is a packed bitset keyed by `ApiGroup`, designed to grow without reallocating the handshake frame for small additions but to spill into a length-prefixed extension block for large ones:

```rust
#[repr(u16)]
pub enum ApiGroup {
    GlCore, GlEs, Egl, Glx, Vulkan, Cuda, OpenCl,
    RocmHip, LevelZero, VideoCodec, WebGpu, OptiX, Amf, Sycl,
}

pub struct CapabilitySet {
    /// Fixed inline groups: one u64 of feature bits per known group.
    inline: [u64; ApiGroup::COUNT],
    /// Extension TLVs for groups added after this build; ignored if unknown.
    ext: Vec<CapExtension>, // { group_id: u16, bits_len: u16, bits: SmallVec<[u8; 16]> }
}
```

The inline array gives O(1) cheap negotiation for the established APIs; the `ext` vector is the forward-compat escape hatch so a newer client can advertise a brand-new API group to an older server, which copies it through `intersect` as "unknown, therefore absent" without choking.

## 29.5 Compatibility windows and skew matrix

The supported skew between a client build and a server build is bounded, not infinite — keeping an unbounded back-compat tail would force the server to carry decoders for every historical frame layout, which conflicts with the "validate the untrusted stream" security requirement (every legacy path is extra attack surface).

| Relationship | Wire majors | Policy |
|--------------|-------------|--------|
| Same major, client minor ≤ server minor | match | Full support. Server understands everything the client sends. |
| Same major, client minor > server minor | match | Supported via downshift. Server advertises lower minor in `HELLO_ACK`; client MUST NOT emit opcodes/fields above the agreed minor. |
| Major differs by 1 | mismatch | **Not** wire-compatible. Rejected at handshake. Operators bridge with a transitional build (§29.8). |
| Major differs by ≥2 | mismatch | Rejected. No transitional path; full re-deploy. |

The committed window, post-1.0: **the server supports the current wire major and accepts clients within `N-2` minor versions of itself, where the floor is the minor at which the most recent deprecation completed.** Pre-1.0 the practical window is "latest two tagged releases," because there is no installed base to protect yet.

Skew direction matters and is asymmetric. A *newer client / older server* is the common ops case (clients in many Linux guests, one server image updated less often). This is handled by minor-downshift: the client throttles itself to the agreed minor. A *newer server / older client* is also fine within the same major because the server retains decoders for all minors of its major. The unsupported case is cross-major, which the handshake refuses cleanly rather than corrupting state.

## 29.6 C ABI versioning for the client shim

Because `graftx-client` is injected as a `cdylib` interposing real driver symbols (chapter 12), its compatibility contract toward the *guest application and loader* is the C ABI, governed independently of Rust SemVer:

- Exported interposer symbols (`glDrawArrays`, `vkQueueSubmit`, …) match the upstream API signatures verbatim — GraftX does not version these; the host API specs do.
- GraftX's *own* control entrypoints (e.g. `graftx_session_reset`, `graftx_diag_dump`) are versioned with a linker version script (`graftx.map`) so symbol additions are non-breaking and a removal forces an soname bump:

```text
GRAFTX_1 { global: graftx_session_reset; graftx_diag_dump; local: *; };
GRAFTX_2 { global: graftx_set_quota; } GRAFTX_1;
```

The soname is `libgraftx_client.so.1`; bumping to `.so.2` is reserved for an incompatible C-ABI change and is treated with the same gravity as a wire-major bump.

## 29.7 Enforcement: frozen-layout and round-trip tests

Compatibility claims are only credible if they are mechanically tested. The plan mandates three test classes in `graftx-protocol`:

1. **Frozen byte-layout snapshots.** For each opcode, a checked-in `.bin` golden encodes a canonical message. A test decodes it with the *current* decoder and asserts field equality. Changing a field offset within wire major 1 breaks the snapshot test — the intended failure that forces a major bump.
2. **Forward-skip tests.** A frame encoded with a synthetic "future" tail (extra reserved bytes, an optional `0x8000` opcode) is fed to the current decoder, which must ignore the tail and the optional opcode without error.
3. **Negotiation property tests.** Using `proptest`, random `(client_pref, server_caps)` pairs are run through `negotiate`; invariants asserted: agreed minor ≤ both inputs, agreed caps ⊆ both inputs, accept ⟹ majors equal.

```rust
proptest! {
    #[test]
    fn negotiated_minor_never_exceeds_either(
        cmin in 0u16..50, smin in 0u16..50, maj in 1u16..3
    ) {
        let hello = Hello::test(ProtocolVersion{major:maj, minor:cmin});
        let srv   = ServerCaps::test(ProtocolVersion{major:maj, minor:smin});
        if let HandshakeOutcome::Accept{wire, ..} = negotiate(&hello, &srv) {
            prop_assert!(wire.minor <= cmin && wire.minor <= smin);
        }
    }
}
```

## 29.8 Deprecation process

Removing or changing an opcode/field/capability follows a staged lifecycle so no deployed peer breaks without warning:

```text
  ACTIVE ──announce──▶ DEPRECATED ──(≥2 minors)──▶ DISABLED-BY-DEFAULT ──▶ REMOVED
                          │                              │                    │
                  still encoded/decoded;          decode still present   wire-major bump;
                  CHANGELOG + runtime warn        but behind opt-in flag  decoder deleted
```

1. **Announce (minor N).** The opcode/field is marked `#[deprecated]` in the Rust API where applicable, flagged in the CHANGELOG, and the server emits a one-shot `DiagWarn` frame back to the client the first time a deprecated opcode is seen in a session. No behavior change.
2. **Deprecated window (minors N..N+2, minimum two minor releases).** Both ends still fully support it. New clients SHOULD stop emitting it; the replacement is advertised as an additive capability bit so clients can detect and prefer it.
3. **Disabled-by-default (minor ≥ N+2).** The server no longer accepts the opcode unless the operator sets a `legacy_opcodes = true` config; default sessions reject it with a clear `RejectReason`. This is the last station before removal and gives a measured "is anything still using this?" signal via metrics (chapter 24).
4. **Removed (next wire-major bump only).** The decoder code and the layout snapshot are deleted. Because removal changes the legal opcode space, it is gated on a wire-major increment, never shipped in a minor.

Deprecation metadata lives in a single registry so docs, the negotiator, and the warning machinery share one source of truth:

```rust
pub struct OpcodeLifecycle {
    pub opcode: u16,
    pub since: ProtocolVersion,           // when introduced
    pub deprecated_since: Option<ProtocolVersion>,
    pub replacement: Option<u16>,          // preferred successor opcode
    pub removed_in_major: Option<u16>,     // planned removal (major only)
}
```

## 29.9 Open questions and deferred decisions

- **Per-session capability re-negotiation mid-stream.** Useful when a guest hot-swaps a GPU context type, but it complicates the validated-stream model. Deferred to post-1.0; the handshake frame reserves a `RENEGOTIATE` optional opcode now so the door stays open without a major bump later.
- **Signed capability advertisements.** Once per-session auth (security requirements) lands, the `HELLO` should be integrity-protected so a compromised guest cannot downgrade-attack the wire minor to reach a retired, less-validated decode path. The minor-downshift logic in §29.4 is the obvious downgrade target and MUST be covered by the eventual auth design.
- **Coupling crate MAJOR to wire MAJOR at 1.0.** Decided loosely-coupled (§29.2); revisit if operators find the decoupling confusing in practice.

Cross-references: build/release tagging and CHANGELOG mechanics (ch. 28), wire frame format details (ch. 4–5), ivshmem bulk layout that depends on agreed caps (ch. 6), client interposition and C symbol export (ch. 12), metrics used to observe deprecated-opcode usage (ch. 24).
