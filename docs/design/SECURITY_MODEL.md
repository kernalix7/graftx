# GraftX Security Model

Engineering reference for the GraftX server's security posture: the threat model
it is built against, the validation it enforces **today**, and the layered
defenses that are still **planned**. This document describes what the code does
now; the forward-looking design rationale lives in the security chapter of the
implementation plan, [`plan/23-security.md`](plan/23-security.md), and the wire
contract it builds on is [`PROTOCOL.md`](PROTOCOL.md).

The bottom line up front: GraftX treats the client as fully untrusted and the
server as the policy gate. The server already enforces a *protocol-aware*
validation layer — framing, opcode routing, and generational/parent handle
checks — before any command reaches a backend. But **every backend today is a
pure-Rust stub**: it tracks object lifetimes in handle tables and performs **no
native driver work, no FFI, and no GPU access**. The most security-critical
defenses (copy-to-private, sandboxing, auth, quotas) are therefore documented
here as *planned*, because the asset they protect — a native GPU driver replay
path — does not exist in the codebase yet.

## 1. Threat model: the client is untrusted

GraftX is designed to span two guests on one host: a Linux guest emits GPU API
calls through client shims, and a Windows server replays them against the
physical GPU it owns via PCI passthrough. The full four-adversary model (a buggy
guest app, a raw-protocol in-guest attacker, a rogue co-resident guest, and a
post-driver-exploit compromised server) is laid out in
[`plan/23-security.md`](plan/23-security.md) §23.1.

The single load-bearing assumption, which the implemented code already honors,
is this:

> The byte stream arriving at the server is fully attacker-controlled. The
> client shim is **not** a trust boundary. Anything the server believes only
> because "the shim wouldn't send that" is a vulnerability.

This is why `Session::handle` validates framing before acting on anything, and
why every backend re-checks handle ownership and parentage rather than trusting
the handles a client hands back. See
[`crates/graftx-server/src/session.rs`](../../crates/graftx-server/src/session.rs).
The result/event direction (server → client) is deliberately *not* treated as
adversarial; the asymmetry is argued in [`plan/23-security.md`](plan/23-security.md)
§23.1.

## 2. What the server enforces today (IMPLEMENTED)

All current enforcement lives on the server's `decode → validate → route` path.
The governing rule — *validate before any command reaches a backend* — is stated
in the `Session::handle` doc comment and is structural: the session never hands a
backend anything until the framing checks below pass.

### 2.1 Frame decoding and framing validation

Every inbound frame is decoded by `graftx_protocol::decode_frame` and then
validated in
[`crates/graftx-server/src/session.rs`](../../crates/graftx-server/src/session.rs)
before dispatch:

- **Frame-size ceiling.** `h.body_len > self.max_frame_body` is rejected with
  `ProtocolError::FrameTooLarge`. The ceiling is the value negotiated at
  handshake (see §2.2), so an oversized length is rejected in O(1) without
  attempting to read it.
- **Declared-vs-actual body length.** `h.body_len as usize != body.len()` is
  rejected with `ProtocolError::BodyLenMismatch { declared, actual }`. The
  decoder hands back a slice; the session is the layer that confirms the header's
  claimed length matches the bytes actually present, closing the gap
  [`PROTOCOL.md`](PROTOCOL.md) §1 calls out as the caller's responsibility.
- **Bounds-checked body decode.** Per-opcode body structs decode through the
  protocol crate, where a body shorter than its declared fields yields
  `ProtocolError::UnexpectedEof` rather than reading out of bounds
  ([`PROTOCOL.md`](PROTOCOL.md) §4.3, §7).

### 2.2 Handshake and frame-body negotiation

The opening `HELLO`/`WELCOME` exchange enforces two things in
`Session::handle`:

- **Version gate.** A `HELLO` whose `proto_major` differs from
  `PROTOCOL_MAJOR` is rejected with `ProtocolError::BadVersion`.
- **Server-capped frame body.** The negotiated `max_frame_body` is
  `hello.max_frame_body.min(DEFAULT_MAX_FRAME_BODY)` — the handshake may only
  *lower* the 1 MiB default, never raise it. This is the one resource ceiling the
  server enforces today (the broader quota table is planned; see §3.4). The wire
  layout of the exchange is in [`PROTOCOL.md`](PROTOCOL.md) §5.

### 2.3 Opcode routing to a registered backend only

Core opcodes (`HELLO`, `NOOP`) are handled inline. Every other opcode is split
into an API namespace byte with `proto::opcode_api` and routed **only** to a
backend explicitly registered for that namespace
([`crates/graftx-server/src/session.rs`](../../crates/graftx-server/src/session.rs)).
If no backend is registered for the namespace, the frame is rejected with
`ProtocolError::UnknownOpcode` — an unregistered API cannot be reached, and an
opcode the server has no handler for is rejected, never skipped. Each backend
*additionally* re-rejects any opcode whose namespace byte does not match its own
`api()` as a defense-in-depth check (see, e.g., the namespace guard at the top of
`CudaBackend::handle` in
[`crates/graftx-server/src/cuda_backend.rs`](../../crates/graftx-server/src/cuda_backend.rs)).

### 2.4 Generational handle validation (use-after-free defense)

Backends track every server object they mint in a
`graftx_handles::HandleTable`
([`crates/graftx-handles/src/lib.rs`](../../crates/graftx-handles/src/lib.rs)).
A wire `Handle` is a 64-bit value carrying a `kind`, a 24-bit `generation`, and a
32-bit slot index ([`PROTOCOL.md`](PROTOCOL.md) §6). The table's `get`/`get_mut`/
`remove` accessors validate, in order, that the slot index is in range, the slot
is occupied, and **the handle's generation matches the slot's current
generation**. A slot's generation is bumped every time it is freed, so a stale
handle pointing at a since-reused slot no longer matches and resolves to `None`.

This is the implemented use-after-free / forged-handle defense:

- A handle a backend never issued (wrong slot, or a fabricated generation) does
  not resolve — see the `mem_alloc_with_bogus_context_errors` test in
  [`crates/graftx-server/src/cuda_backend.rs`](../../crates/graftx-server/src/cuda_backend.rs).
- A handle for an object that has since been freed does not resolve — see
  `mem_free_of_already_freed_dptr_errors` in the same file, and
  `stale_handle_after_reuse_returns_none` in
  [`crates/graftx-handles/src/lib.rs`](../../crates/graftx-handles/src/lib.rs).
- When a slot's generation would exceed `GENERATION_MAX`, the slot is **retired**
  and never reused, so the generation cannot wrap to a value an old handle still
  holds.

### 2.5 Parent-handle validation before child creation

Object-creating opcodes validate that the parent handle they are given resolves
to a live object **this backend created** before minting a child. The model is
the GPU object hierarchy (instance → physical device → logical device → queue /
memory / buffer for Vulkan; context → allocation for CUDA, etc.). Examples:

- The CUDA backend rejects `MEM_ALLOC` whose `context` handle does not resolve in
  its context table, and `MEM_FREE` whose `dptr` is not a live allocation it
  issued ([`crates/graftx-server/src/cuda_backend.rs`](../../crates/graftx-server/src/cuda_backend.rs)).
- The Vulkan backend rejects `ENUMERATE_PHYSICAL_DEVICES`, `CREATE_DEVICE`,
  `GET_DEVICE_QUEUE`, `ALLOCATE_MEMORY`, `CREATE_BUFFER`, and
  `BIND_BUFFER_MEMORY` whose parent (instance / physical device / device /
  memory) handle does not resolve — and `BIND_BUFFER_MEMORY` *also* rejects a
  re-bind of an already-bound buffer
  ([`crates/graftx-server/src/backend.rs`](../../crates/graftx-server/src/backend.rs)).

A failed ownership or parentage check surfaces as `ProtocolError::UnknownOpcode`
today (the stubs reuse that variant for "not a thing I can act on"); the
planned validator gains a dedicated reject taxonomy (§3.1).

### 2.6 What this maps to in the planned model

In the language of [`plan/23-security.md`](plan/23-security.md) §23.4, today's
server implements parts of validation tiers 1 (structural / decode), 3 (handle
ownership), and the single negotiated frame-body limit from tier 4. Tier 2
(range/enum), the rest of tier 4 (quotas), and tier 5 (semantic/dependency)
checks are not yet implemented because no backend interprets command arguments
against driver semantics.

## 3. What is still planned (NOT YET IMPLEMENTED)

Everything in this section is forward-looking design. The reason these defenses
are absent is the same reason they are critical later: **the backends are stubs
with no native driver calls.** There is currently no driver entry point to feed
unvalidated data to, no shared-memory bulk plane wired up, and no multi-tenant
process to sandbox. Each item below is gated on that machinery landing. The
authoritative design is [`plan/23-security.md`](plan/23-security.md); the
mapping:

### 3.1 The validator and the `ValidatedCommand` invariant

The planned core invariant — *no native driver entry point is ever called with a
value the validator has not range-, type-, and resource-checked* — is enforced by
making a backend's driver dispatch accept **only** a `ValidatedCommand` that the
validator alone can mint (the raw decoded command cannot be passed to a driver).
See [`plan/23-security.md`](plan/23-security.md) §23.2, §23.4. None of this type
machinery exists yet; the current backends route raw decoded bodies straight into
stub logic because that logic touches no driver.

### 3.2 Copy-to-private and the TOCTOU window

The bulk (ivshmem) plane is shared memory the client can rewrite at any instant,
creating a time-of-check-to-time-of-use race: validate the shared bytes, and the
client mutates them before the driver consumes them. The planned mitigation is
**copy-to-private first, then validate the copy, then use only the copy**, with
the copy bounds taken from the single-snapshot control-plane frame and *never*
from a length field living inside the shared payload. See
[`plan/23-security.md`](plan/23-security.md) §23.5. There is no bulk plane and no
`copy_in` in the codebase today, so this race is not yet reachable — and not yet
defended.

### 3.3 Sandboxing the server

Validation lowers the probability of a driver exploit; the sandbox limits its
blast radius. Planned Windows-guest containment includes a least-privilege
service account, an AppContainer / Job Object, per-session GPU-context isolation
(with process-per-session as a hardening option), rejection of any host file path
named by a decoded command, and crash containment that converts a native driver
fault (caught at the FFI seam, not as a Rust panic) into a single-session
teardown. See [`plan/23-security.md`](plan/23-security.md) §23.6. None of this
exists; the server is a single in-process command router with no FFI seam to
guard.

### 3.4 Resource limits, quotas, and backpressure

The only enforced ceiling today is the negotiated `max_frame_body` (§2.2). The
planned quota set — bulk payload per command, private scratch arena high-water,
live objects per kind, per-session VRAM budget, bounded in-flight queue depth,
and a command-rate token bucket — is negotiated per session and server-capped,
with a **bounded reader→replay queue** providing backpressure so a flooding
client blocks itself rather than growing server memory without bound. See
[`plan/23-security.md`](plan/23-security.md) §23.7.

The handle layer has already grown the *primitive* for one of these quotas ahead
of the rest: `HandleTable::try_insert(kind, value, max_live)` inserts only while
the table is below a caller-supplied live-object cap and otherwise returns `None`
without inserting (see
[`crates/graftx-handles/src/lib.rs`](../../crates/graftx-handles/src/lib.rs)). No
backend wires a quota into it yet — every current call site uses the
unconditional `insert` — so the per-kind object cap is *available but not
enforced*.

### 3.5 Per-session authentication and channel integrity

vsock pairing and a shared ivshmem region provide *reachability scoping*, not
authentication: they stop unrelated guests but do not prove the peer is the
legitimate client, nor handle a misconfigured third VM on the same device. The
planned layer adds a mutual handshake keyed by an out-of-band per-pair secret, an
optional per-frame MAC plus replay protection on the monotonic `seq`, and epoch
fencing on the shared region. See [`plan/23-security.md`](plan/23-security.md)
§23.8. The implemented handshake ([`PROTOCOL.md`](PROTOCOL.md) §5) negotiates
version, features, and frame size only — it carries **no** authentication today.

### 3.6 Hypervisor write-revocation and fuzzing

Two further planned items round out the model: **hypervisor-level
write-revocation** (true client-write exclusivity during a copy, a
hypervisor/ivshmem-layer property the server software cannot provide;
copy-to-private gives consistency, the hypervisor gives exclusivity — see
[`plan/23-security.md`](plan/23-security.md) §23.9), and **continuous fuzzing of
the decoder and validator** as a CI gate, with zero panics and zero sanitizer
findings as the acceptance bar before any backend is production-ready
([`plan/23-security.md`](plan/23-security.md) §23.10).

## 4. Status summary

| Defense | Status | Where |
|---|---|---|
| Untrusted-client threat model | Assumed by design | [`session.rs`](../../crates/graftx-server/src/session.rs), [`plan/23-security.md`](plan/23-security.md) §23.1 |
| Frame-size ceiling (`FrameTooLarge`) | **Implemented** | [`session.rs`](../../crates/graftx-server/src/session.rs) |
| Declared-vs-actual body length (`BodyLenMismatch`) | **Implemented** | [`session.rs`](../../crates/graftx-server/src/session.rs) |
| Bounds-checked body decode (`UnexpectedEof`) | **Implemented** | [`PROTOCOL.md`](PROTOCOL.md) §4.3 |
| Version gate (`BadVersion`) | **Implemented** | [`session.rs`](../../crates/graftx-server/src/session.rs) |
| Server-capped frame body (only enforced quota) | **Implemented** | [`session.rs`](../../crates/graftx-server/src/session.rs) |
| Opcode routed only to a registered backend | **Implemented** | [`session.rs`](../../crates/graftx-server/src/session.rs) |
| Generational handle validation (UAF / forged handle) | **Implemented** | [`graftx-handles`](../../crates/graftx-handles/src/lib.rs) |
| Parent-handle validation before child creation | **Implemented** | [`backend.rs`](../../crates/graftx-server/src/backend.rs), [`cuda_backend.rs`](../../crates/graftx-server/src/cuda_backend.rs) |
| Per-kind live-object quota (`try_insert`) | Primitive present, **not wired** | [`graftx-handles`](../../crates/graftx-handles/src/lib.rs) |
| `ValidatedCommand` driver invariant | Planned | [`plan/23-security.md`](plan/23-security.md) §23.2 |
| Range/enum & semantic validation (tiers 2, 5) | Planned | [`plan/23-security.md`](plan/23-security.md) §23.4 |
| Copy-to-private / TOCTOU mitigation | Planned | [`plan/23-security.md`](plan/23-security.md) §23.5 |
| Server sandboxing & crash containment | Planned | [`plan/23-security.md`](plan/23-security.md) §23.6 |
| Quotas & backpressure | Planned | [`plan/23-security.md`](plan/23-security.md) §23.7 |
| Per-session auth & frame integrity | Planned | [`plan/23-security.md`](plan/23-security.md) §23.8 |
| Hypervisor write-revocation | Planned | [`plan/23-security.md`](plan/23-security.md) §23.9 |
| Decoder/validator fuzzing as CI gate | Planned | [`plan/23-security.md`](plan/23-security.md) §23.10 |

**All API backends are remoting-path stubs with no native driver calls.** The
implemented validation above guards a replay path that does not yet reach a
driver; it is the foundation the planned defenses build on once the driver
bridge, bulk plane, and multi-tenant runtime land. For operator-facing security
reporting and policy, see the top-level [`SECURITY.md`](../../SECURITY.md).
