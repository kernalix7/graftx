# 04. High-Level Architecture & Data Flow

How a Linux-guest GPU call travels through interception, encoding, transport, decode, validation, and replay on the Windows guest — and how every component, plane, and host boundary along that path will be carved up.

This chapter fixes the system-level shape that the rest of the plan elaborates: the four crates and their responsibilities, the split between a control plane and a bulk data plane, the eight-stage lifecycle of a single forwarded call, and the deployment topology across two guests and one host. It is the contract the Protocol chapter (Ch. 06), the Transport chapter (Ch. 08), the Client shim chapter (Ch. 09), and the Server core chapter (Ch. 10) all plug into. Where this chapter names a mechanism but defers the detail, it cross-references the owning chapter rather than re-specifying it.

## 4.1 The remoting model in one picture

GraftX will interpose between an unmodified Linux application and the GPU it cannot see. The physical GPU is bound by PCI passthrough to a Windows guest; the Linux guest holds only client shims. A call crosses the guest boundary, executes against the native vendor driver on Windows, and its results return. No host-side virtual GPU device, no framebuffer scrape, no video stream — API calls in, API results out.

```text
        LINUX GUEST  (untrusted)                  HOST / HYPERVISOR              WINDOWS GUEST  (owns GPU)
  ┌──────────────────────────────┐          ┌───────────────────────┐     ┌──────────────────────────────┐
  │ app                          │          │  QEMU/KVM              │     │ graftx-server (bin)          │
  │   │ libvulkan.so/libcuda.so  │          │   vhost-vsock device   │     │   ▲ decode→validate→replay   │
  │   ▼ (real C symbols)         │          │   ivshmem PCI device   │     │   │                          │
  │ graftx-client (cdylib)       │          │   (BAR2 shared RAM)    │     │ graftx-protocol (decode)     │
  │   │ marshal via protocol     │          └───────────┬───────────┘     │   ▲                          │
  │   ▼                          │   control   ┌────────┘                 │ graftx-transport::recv       │
  │ graftx-protocol (encode)     │ ──vsock──▶  │                          │   ▲      │ FFI to vendor      │
  │   │                          │             │       bulk (ivshmem)     │   │      ▼ driver           │
  │ graftx-transport::send ──────┼─────────────┘  ◀═══ shared region ═══▶ │ native libvulkan/cuda/…     │
  └──────────────────────────────┘                                        │      │                       │
        ▲ results / status / surfaces  ◀───── control + bulk return ──────┘      ▼  physical GPU         │
                                                                          └──────────────────────────────┘
```

The four workspace crates map onto this picture exactly: `graftx-protocol` defines what a byte means and lives on both ends; `graftx-transport` moves opaque frames and bulk regions and lives on both ends; `graftx-client` is the Linux endpoint; `graftx-server` is the Windows endpoint. The transport frame is a small header `{magic, channel, len}` wrapping an opaque body, and that body is the protocol frame; `magic` lives only in the transport header (see the Protocol chapter (Ch. 06) and the Transport chapter (Ch. 08)). See `docs/ARCHITECTURE.md` for the per-crate prose; this chapter focuses on their *runtime interaction*.

## 4.2 Two planes: control vs data

The single most important structural decision is splitting traffic into a **control plane** and a **data plane**, because the two have opposite cost profiles and opposite safety profiles.

| Property        | Control plane                              | Data plane                                      |
| --------------- | ------------------------------------------ | ----------------------------------------------- |
| Carries         | handshake, command frames, replies, status | bulk buffers (textures, vertex/index, compute, frames) |
| Default backend | virtio-vsock (copied, framed, ordered)     | ivshmem shared region (zero-copy, mapped)       |
| Sizes           | tens of bytes to a few KiB                 | KiB to hundreds of MiB                          |
| Latency model   | round-trip-bound for queries               | one-way streaming, signaled by doorbell         |
| Trust posture   | copied into server RAM on `recv` already   | client-writable after the check — TOCTOU hazard |
| Owner chapter   | Ch. 06 (frames), Ch. 08 (vsock)            | Ch. 08 (ivshmem ring), Ch. 12 (zero-copy), Ch. 23 (security)|

A command frame on the control plane is self-contained: opcode plus marshalled scalar arguments plus *descriptors* of where bulk payloads live. A small input buffer can be inlined directly into the frame; a large one is **not** copied into the frame — instead the frame carries a `ShmSlice { offset, len, gen }` and the bytes live in the ivshmem region. The server reads the frame from vsock (already a private copy), then dereferences each `ShmSlice` into the shared region. This is where the security tension lives: the frame is trustworthy by the time it is decoded, but the bytes a `ShmSlice` points at are not, because the client can rewrite the shared region after validation. The resolution — copy-validate-then-replay against the private copy — is stated in §4.5 and owned in full by the Security chapter (Ch. 23).

```rust
/// A marshalled call as it appears on the control plane.
/// `args` holds scalars + inlined small buffers; `bulk` references
/// large payloads parked in the shared data-plane region.
pub struct CommandFrame<'a> {
    pub opcode: u32,           // (ApiId<<24) | call_id — see the Protocol chapter (Ch. 06)
    pub session: SessionId,    // multiplexing key — see §4.7
    pub seq: u64,              // per-session monotonic ordering/fence seq — owned by the Sync chapter (Ch. 13)
    pub req_id: u32,           // request/response correlation id — owned by the Protocol chapter (Ch. 06)
    pub flags: FrameFlags,     // ASYNC | EXPECTS_REPLY | BATCH_CONT | ...
    pub args: &'a [u8],        // borrowed view into the recv buffer
    pub bulk: &'a [ShmSlice],  // 0..N descriptors into the ivshmem region
}

// Canonical bulk descriptor, defined once in graftx-transport (see the
// Transport chapter (Ch. 08)). Shared region <= 4 GiB, so offset/len are u32.
#[repr(C)]
pub struct ShmSlice {
    pub offset: u32,
    pub len: u32,
    pub gen: u32,              // slot/epoch guard; rejects stale/reused slots
}
```

Why not push everything through one channel? Putting bulk on vsock means a guest→host→guest copy of every texture (the Transport chapter (Ch. 08) measures this as the dominant cost for GPU traffic); putting control frames on ivshmem means hand-rolling ordered, reliable framing over raw shared memory for tiny messages where vsock already gives it for free. The hybrid takes the cheap, safe property from each: vsock's framing/backpressure/ordering for control, ivshmem's zero-copy for bulk.

## 4.3 The eight-stage call lifecycle

`docs/ARCHITECTURE.md` summarises the lifecycle as six stages — intercept, encode, transport, decode/validate, replay, return; the plan refines those same six into eight by splitting the single *transport* stage into send and recv and by pulling *return* apart from *replay*, because each split is a distinct failure and backpressure point. The eight stages are exactly the six expanded, not a different pipeline. A synchronous call (one that returns a value the application reads) traverses all eight and pays one full round-trip. A fire-and-forget call (command-buffer recording, async submit) stops at stage 3 from the application's point of view — the shim returns immediately and the later stages happen off the caller's thread.

```text
 (1) INTERCEPT   app → shim C entry point; capture args
 (2) ENCODE      shim → CommandFrame; inline small bufs, park large bufs as ShmSlice
 (3) SEND        Transport::send(frame) over vsock; bulk already staged in ivshmem
        ─────────────────── guest boundary ───────────────────
 (4) RECV        server Transport::recv(); frame is now a server-private copy
 (5) DECODE+VALIDATE  protocol::decode; bounds-check opcode/lens/handles; copy bulk
                      out of shared region into private RAM, re-validate the copy
 (6) REPLAY      map client handles → real driver objects; call native driver FFI
 (7) ENCODE RESULT    marshal return value + output params + surface handles
 (8) RETURN      Transport::send reply over vsock (+ bulk readback via ivshmem);
                 client decodes, fills caller's out-params, returns
```

Sequence walk-through, synchronous query (`vkGetPhysicalDeviceProperties`-style):

```text
app        client-shim       transport(vsock)        server          native driver
 │  call ──▶ │                                                       
 │           │ encode frame{op,seq=N,req_id=R,EXPECTS_REPLY}        
 │           │ send ──────────────▶ recv ──────────▶ decode+validate 
 │           │                                       replay ───────▶ │ driver call
 │           │                                       encode result ◀─ │ result
 │           │ recv ◀───────────── send ◀──────────── return         
 │ ◀── ret ──│ decode, fill *out                                     
```

Sequence walk-through, fire-and-forget with batching (`vkCmdDraw`×K then one submit):

```text
app        client-shim                         server
 │ cmd ─▶  │ append to batch buffer (no send)   
 │ cmd ─▶  │ append ...                          
 │ submit ▶│ flush: one frame, BATCH_CONT chain, ASYNC ──▶ decode+validate ×K
 │ ◀──────  │ return immediately (no reply wait)         replay ×K against driver
```

The application thread blocks only at stage 8, and only for `EXPECTS_REPLY` frames. Everything else is pipelined: the client races ahead while the server drains its queue. The Server core chapter (Ch. 10) owns the batching/async machinery; the Handles chapter (Ch. 11) owns the handle-mapping. Handles are server-authoritative — the server mints the wire `Handle`; the client never puts an invented handle on the wire. For async "create" calls the client may keep a purely local provisional proxy token (never sent as authority) that is reconciled deterministically when the server's real handle arrives.

## 4.4 Component responsibilities at runtime

Each crate has one job and a hard interface to the next. The boundaries are chosen so that the *trust* boundary (4.6) and the *unsafe* boundary coincide with crate edges wherever possible.

```rust
// graftx-transport — opaque framing, no protocol knowledge.
pub trait Transport {
    fn send(&mut self, frame: &[u8]) -> io::Result<()>;
    fn recv(&mut self) -> io::Result<Vec<u8>>;
}

// Bulk data plane is a separate trait so a vsock-only deployment
// (bring-up, see the Transport chapter (Ch. 08)) can run without ivshmem at all.
pub trait BulkChannel {
    /// Client side: reserve a slot, get a writable view + a ShmSlice to send.
    fn stage(&mut self, len: usize) -> io::Result<(BulkSlot<'_>, ShmSlice)>;
    /// Server side: copy a referenced region into server-private memory.
    fn fetch_private(&self, r: &ShmSlice) -> Result<Vec<u8>, BulkError>;
}
```

- **`graftx-protocol`** — pure, host-neutral, no I/O, no `unsafe`. Owns the `u32` opcode scheme `(ApiId<<24) | call_id`, frame encode/decode, `PROTOCOL_VERSION`, the `Hello`/`Welcome` handshake messages, and `ProtocolError`. It is the single source of truth for byte meaning; both ends link it. Detail: the Protocol chapter (Ch. 06).
- **`graftx-transport`** — owns the `Transport` and `BulkChannel` traits and their vsock/ivshmem backends, and defines the one canonical `ShmSlice` descriptor and `ShmHeader`. It moves bytes and signals readiness; it never inspects a frame's contents. All raw-socket and shared-memory `unsafe` is confined here behind `// SAFETY:` comments. Detail: the Transport chapter (Ch. 08).
- **`graftx-client`** (cdylib + rlib) — the Linux endpoint. Exports the real C symbols of each intercepted API, marshals each call into a `CommandFrame`, stages bulk, sends, and reconstructs results into the caller's out-parameters. Holds the client-side handle table (local provisional proxies only) and the batch buffer. FFI/symbol-interception `unsafe` is confined to per-API modules. Detail: the Client shim chapter (Ch. 09).
- **`graftx-server`** (bin) — the Windows endpoint and the replay engine. Decodes, validates, copies bulk to private memory, mints and maps the authoritative handles to real driver objects, calls the native driver, and returns. This is the only component that touches a real driver with attacker-influenced data, so it is the sandboxing and hardening target. Detail: the Server core chapter (Ch. 10); hardening in the Security chapter (Ch. 23).

Decision table — where does responsibility X live?

| Responsibility                       | Crate            | Rationale                                  |
| ------------------------------------ | ---------------- | ------------------------------------------ |
| Opcode/argument wire layout          | graftx-protocol  | both ends must agree exactly               |
| Framing / ordering / backpressure    | graftx-transport | channel concern, protocol-agnostic         |
| Symbol interception (`dlsym`, IAT)   | graftx-client    | platform FFI, app-facing                   |
| Local provisional proxy tokens       | graftx-client    | enables async create — see Ch. 11          |
| Authoritative handle minting         | graftx-server    | handles are server-authoritative — see Ch. 11 |
| Command validation / bounds-check    | graftx-server    | the trust boundary lives here              |
| Bulk copy-to-private + re-validate   | graftx-server    | TOCTOU defense — see Ch. 23                 |
| Native driver dispatch               | graftx-server    | only place a real driver is touched        |

## 4.5 Bulk-data semantics and the validate-a-copy rule

The data plane is zero-copy *for the client*: it writes a buffer once into its ivshmem view and never copies it again. It is explicitly **not** zero-copy for the server. Because the shared region stays writable from the client side, any byte the server validates in place can be changed by the client between the check and the driver's read (a double-fetch / TOCTOU hazard). The architecture therefore mandates: for every `ShmSlice`, the server **copies the referenced bytes into server-private memory** (`BulkChannel::fetch_private`), validates that private copy, and hands only the private copy to the driver. The `gen` field rejects a slot the client recycled mid-flight. Write-revocation of a peer guest's mapping can only be enforced at the hypervisor / ivshmem-device layer, not by the server re-mapping its own view, so copy-and-validate is the default and only safe path. This is stated here as an architectural invariant; the Security chapter (Ch. 23) owns the mechanism and the cost analysis.

## 4.6 Trust boundary and where it sits in the flow

```text
   trusted-by-its-owner          UNTRUSTED across transport         replays against drivers
   Linux app + its shim   ─────▶  vsock frames + ivshmem bytes ────▶ server: VALIDATE + SANDBOX
                                  (paired guests, reachability)      then dispatch to driver
```

The hard line is at stage 5 (decode+validate). Everything upstream — the application, the shim, the frames in flight, the bytes in the shared region — is untrusted from the server's perspective; everything the native driver sees must already have passed validation against known limits. The paired-guest channel (vsock CID/port scoping, ivshmem region membership) is *reachability scoping*, not authentication: it keeps arbitrary parties from connecting, but it does not make a connected client trustworthy. Per-session auth/integrity is planned (the Security chapter (Ch. 23)), not assumed here.

## 4.7 Sessions, multiplexing, and ordering

One server process will serve one or more client connections; one client process may host multiple API contexts (several `VkDevice`s, a CUDA context plus an OpenCL context). The architecture multiplexes these onto the transport with a `SessionId` per logical client and a per-session monotonic `seq`.

```rust
pub struct SessionId(pub u32);

pub struct Session {
    pub id: SessionId,
    pub next_seq: u64,                 // per-session ordering/fence seq (the Sync chapter (Ch. 13))
    pub handles: HandleTable,          // server-authoritative object map (the Handles chapter (Ch. 11))
    pub inflight: HashMap<u32, Waiter>, // req_id → reply waiter (sync calls only)
}
```

Ordering rules the planes must honor:

- Within one session, frames are **delivered in `seq` order** — `seq` is the per-session monotonic ordering/fence sequence (owned by the Sync chapter (Ch. 13)) — required because driver state is order-sensitive (you cannot bind a pipeline before creating it). vsock's stream ordering gives this for free on the control plane.
- A `ShmSlice` is only valid *while its frame is being processed*; the client must not recycle the slot until the frame is acknowledged (sync) or the batch is flushed and drained (async). The `gen` field makes a violation detectable rather than silent.
- Cross-session ordering is **not** guaranteed and not needed — sessions are independent.

Replies are correlated by `(session, req_id)` — `req_id` is the request/response correlation id (owned by the Protocol chapter (Ch. 06)), distinct from the ordering `seq`: the server echoes it in the reply frame, the client looks up the matching `Waiter`, and the calling thread is unparked. For async frames there is no `Waiter`; errors are surfaced lazily on the next synchronous call or at an explicit flush/fence, as each API's contract allows.

## 4.8 Deployment topology

```text
                     ┌──────────────────────────── single physical host ───────────────────────────┐
                     │                                                                              │
   ┌─────────────────┴──────────────┐   QEMU/KVM    ┌──────────────────────────────────────────────┴┐
   │ LINUX GUEST                    │  hypervisor   │ WINDOWS GUEST                                   │
   │  - app + graftx-client shim    │               │  - graftx-server.exe (sandboxed)                │
   │  - vsock CID = A               │◀── vhost ────▶│  - vsock CID = B                                │
   │  - maps ivshmem BAR2 (RW)      │◀═ ivshmem ═══▶│  - maps ivshmem BAR2 (RW), copies to private RAM │
   │  - NO physical GPU             │   PCI dev     │  - physical GPU via VFIO PCI passthrough         │
   └────────────────────────────────┘               └─────────────────────────────────────────────────┘
```

The topology is fixed at three nodes: two sibling guests and the host that wires them. The host runs the hypervisor (QEMU/KVM is the reference target), exposes a `vhost-vsock` device to both guests for the control plane, and an `ivshmem` PCI device backed by a shared host memory object for the data plane. The Windows guest additionally owns the GPU through VFIO passthrough. There is deliberately **no network hop and no host-resident GraftX daemon** on the data path — the host's only role is to provide the vsock relay and the shared BAR; it never sees or routes GPU API content. This keeps the host out of the trusted computing base for *correctness* (it cannot corrupt a frame's meaning) while acknowledging it remains in the TCB for *isolation* (it enforces which guests can map the region — the only layer that can, per §4.5).

Bring-up will deploy a degraded topology first: vsock-only, no ivshmem, bulk inlined into frames (slow but correct), matching milestone M0 in `docs/design/ROADMAP.md`. The ivshmem data plane lights up as a performance track once a correct round-trip exists, after which the steady state is the hybrid shown above. A same-OS loopback variant (both ends on Linux, transport stubbed to an in-process channel) will exist purely for testing the protocol and shims without a Windows guest; it is a test fixture, not a deployment mode.

## 4.9 Failure domains and how the architecture contains them

| Failure                          | Detected at        | Containment                                   |
| -------------------------------- | ------------------ | --------------------------------------------- |
| Malformed/unknown opcode         | stage 5 decode     | `ProtocolError`, frame rejected, never replayed |
| Out-of-range len/offset/handle   | stage 5 validate   | rejected against per-session limits           |
| Client rewrites bulk after check | §4.5 copy-to-private| driver only sees the private copy; `gen` guard |
| Driver crash on hostile input    | stage 6 replay     | session torn down — the whole driver/session is the safe blast radius after a native fault (Ch. 23) |
| Transport disconnect             | any send/recv      | session torn down; handles reclaimed          |
| Version mismatch                 | `Hello`/`Welcome` handshake | session aborted before any GPU call (Ch. 06) |
| Resource exhaustion              | server admission   | per-session quotas/backpressure (Ch. 23)      |

Each domain is bounded at a crate edge, which is the point: a fault in the untrusted stream cannot propagate past stage 5 without passing validation, and a fault in the driver cannot propagate past the server sandbox. This is the architectural payoff of the plane split and the trust line — the rest of the plan fills in each box.
