# 08. Transport Layer: vsock Control + ivshmem Bulk

How GraftX will move opaque command frames and bulk GPU payloads between the two guests: a virtio-vsock control plane, an ivshmem shared-memory bulk plane, and the `Transport` trait that hides both behind a framed byte channel.

This chapter designs the `graftx-transport` crate. It owns the **guest-to-guest channel** and nothing else: it moves opaque frames and does not understand the wire protocol (the Protocol chapter (Ch. 06)) or the GPU command opcodes (the per-API chapters, Vulkan through Video, Ch. 14-20). The split is deliberate — the protocol is pure and host-neutral, while everything platform-specific (raw vsock sockets, mapped shared memory, atomics, interrupts) is isolated here behind one trait and guarded with `// SAFETY:` comments per repo convention. This chapter is the **owner** of the canonical `ShmHeader`, the canonical bulk descriptor `ShmSlice`, and the negotiated cap field `max_frame_body`; other chapters reference these definitions and never redeclare them.

## 8.1 Two planes, two transports

A naive design uses one channel for everything. GraftX will not, because the two kinds of traffic have opposite profiles:

| Traffic | Examples | Size | Latency need | Frequency |
|---|---|---|---|---|
| **Control** | handshake, opcodes, small args, return codes, fences | tens of bytes – few KB | round-trip latency dominates | every call |
| **Bulk** | vertex/index buffers, textures, CUDA H2D/D2H copies, mapped staging, swapchain images | KB – hundreds of MB | throughput dominates; zero-copy matters | per-resource |

virtio-vsock and ivshmem each win on exactly one axis:

```
                 latency        throughput     copies     setup
virtio-vsock     good           limited        2x kernel  trivial (socket)
ivshmem          excellent      excellent      0–1        fixed BAR + sync
```

- **virtio-vsock** is a socket family (`AF_VSOCK`) tunnelled through virtio. It is connection-oriented, reliable, ordered, and gives us a stream/datagram API for free. But every byte crosses the guest kernel into the host vhost-vsock device and back into the peer guest — a copy on each side plus a vmexit. Fine for small messages, wasteful for a 256 MB texture.
- **ivshmem** maps a host-allocated region into both guests' physical address space via a PCI BAR. Once mapped, a write by one guest is *visible* to the other with no kernel involvement and no copy. Synchronization (who may read/write, and when) is our problem; ivshmem only provides the shared bytes plus an optional MSI-X doorbell mechanism (ivshmem-doorbell) for cross-guest interrupts.

**Decision.** GraftX will use **virtio-vsock as the control plane** (handshake, command frames, completions, fences) and **ivshmem as the bulk plane** (large payloads referenced by control frames). This matches the priority ordering breadth > performance > stability > safety: vsock gets us a working channel quickly (M0), ivshmem is the optimization that removes the copy tax on big transfers (later milestone). Both implement the same `Transport` trait, so the client and server are written once.

A subtle security consequence drives the Client shim chapter (Ch. 09) design and is restated here: bulk data lands in a region the **untrusted client can still mutate** after the server has seen it. Write-revocation is enforceable only at the hypervisor/ivshmem-device layer, so the server must **copy validated bulk data into server-private memory before use** and never trust the shared region across a validation boundary. The transport layer exposes the raw region; the *copy-then-validate* discipline lives in the server.

## 8.2 The `Transport` trait

The crate already exposes a minimal synchronous trait:

```rust
pub trait Transport {
    fn send(&mut self, frame: &[u8]) -> io::Result<()>;
    fn recv(&mut self) -> io::Result<Vec<u8>>;
}
```

This is the right *contract* — opaque framed bytes — but two extensions are planned so the bulk plane can avoid the `Vec<u8>` allocation/copy that `recv` implies, and so callers can express bulk transfers explicitly:

```rust
/// A borrowed view into the bulk region, valid until `release` is called.
/// Backed by ivshmem when available, or by a heap buffer on the vsock-only path.
pub struct BulkRef<'a> {
    pub id: BulkId,          // descriptor index in the bulk ring
    pub bytes: &'a [u8],     // SAFETY: lifetime tied to the lease guard below
}

pub trait Transport {
    /// Send one control frame. Blocks until queued (not until delivered).
    fn send(&mut self, frame: &[u8]) -> Result<(), TransportError>;

    /// Receive one control frame. Blocks until a whole frame is available.
    fn recv(&mut self) -> Result<Vec<u8>, TransportError>;

    /// Reserve `len` bytes of bulk space; returns a writable staging slice.
    /// On the ivshmem backend this is zero-copy into the shared BAR.
    fn bulk_alloc(&mut self, len: usize) -> Result<BulkWriter<'_>, TransportError>;

    /// Borrow a received bulk payload by id (zero-copy on ivshmem).
    fn bulk_view(&mut self, id: BulkId) -> Result<BulkRef<'_>, TransportError>;

    /// Hint that no more traffic will be sent; lets the peer drain and close.
    fn shutdown(&mut self) -> Result<(), TransportError>;
}
```

`TransportError` (a `thiserror` enum, no `unwrap` in lib paths) distinguishes recoverable from fatal:

```rust
#[derive(thiserror::Error, Debug)]
pub enum TransportError {
    #[error("peer closed the connection")]      PeerClosed,
    #[error("frame exceeds negotiated max ({0} > {1})")] FrameTooLarge(u32, u32),
    #[error("bulk region exhausted (need {0}, free {1})")] BulkExhausted(usize, usize),
    #[error("backend i/o error")]                Io(#[from] io::Error),
    #[error("protocol framing violation: {0}")]  Framing(&'static str),
    #[error("transport closed, retry on new session")] Retry,
}
```

Backends: `VsockTransport` (M0), `IvshmemTransport` and the composite `SplitTransport { ctrl: VsockTransport, bulk: IvshmemTransport }` (later). The composite is what production uses; the two pure backends exist for bring-up and for hosts where ivshmem is unavailable (graceful degradation to vsock-only, paying the copy cost).

## 8.3 Framing over the vsock stream

`AF_VSOCK` `SOCK_STREAM` is a byte stream: `recv` may return a partial frame or several coalesced frames. The transport must reframe. Per the frame-layering decision (D2), the **transport frame** is a small header `{magic, channel, len}` wrapping an **opaque body** — that body is the protocol frame (the Protocol chapter (Ch. 06): `FrameHeader{version, flags, kind, opcode, req_id, seq, body_len}` + payload). The transport never inspects the body; framing flags such as `BULK_REF`, `FENCE`, and `LAST` live in the protocol `FrameHeader`, not here. `MAGIC` lives **only** in this transport header. The planned wire framing is a fixed 8-byte header:

```text
  0               1               2               3
 ┌───────────────┬───────────────┬───────────────┬───────────────┐
 │  MAGIC 'G'     │  MAGIC 'X'    │  channel (u8) │  reserved (u8)│
 ├───────────────┴───────────────┴───────────────┴───────────────┤
 │                     body length (u32, LE)                      │
 ├────────────────────────────────────────────────────────────────┤
 │                opaque body  (length bytes = protocol frame)    │
 └────────────────────────────────────────────────────────────────┘
```

- `MAGIC` ("GX") catches desync after a bug; a mismatch is a fatal `Framing` error, not a resync attempt — silently hunting for the next magic on an untrusted stream is a DoS vector.
- `channel`: multiplex id (see 8.7); the only field the transport routes on.
- `reserved`: zero on the wire, reserved for transport-level use; the protocol's own `flags` (BULK_REF/FENCE/LAST) ride inside the opaque body.
- `length` is the body length, bounded by the negotiated `max_frame_body` (handshake, the Protocol chapter (Ch. 06); default 1 MiB). `FrameTooLarge` is fatal — it means a malformed or hostile sender.

Read loop sketch, accumulating into a reusable buffer to avoid per-frame allocation churn:

```rust
fn recv(&mut self) -> Result<Vec<u8>, TransportError> {
    loop {
        if let Some(frame) = self.try_split_frame()? { return Ok(frame); }
        let n = self.sock.read(&mut self.scratch)?;     // may be partial
        if n == 0 { return Err(TransportError::PeerClosed); }
        self.inbuf.extend_from_slice(&self.scratch[..n]);
    }
}

fn try_split_frame(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
    if self.inbuf.len() < HDR { return Ok(None); }
    if &self.inbuf[0..2] != b"GX" { return Err(TransportError::Framing("magic")); }
    let len = u32::from_le_bytes(self.inbuf[4..8].try_into().unwrap()) as usize;
    if len as u32 > self.max_frame_body { return Err(TransportError::FrameTooLarge(len as u32, self.max_frame_body)); }
    if self.inbuf.len() < HDR + len { return Ok(None); } // need more bytes
    let frame = self.inbuf[HDR..HDR + len].to_vec();
    self.inbuf.drain(..HDR + len);                        // O(remaining); see note
    Ok(Some(frame))
}
```

The `drain` is O(remaining bytes); for the hot path the implementation will use a ring/cursor (`bytes::BytesMut::advance`) instead of shifting. The `.unwrap()` on `try_into` is on a slice of known length and is acceptable per the "no unwrap in lib paths" rule because it is provably infallible — but the implementation will prefer `from_le_bytes` over an explicitly sized array read to keep that guarantee local.

## 8.4 The ivshmem bulk region: rings and descriptors

ivshmem gives a flat, fixed-size shared region (the PCI BAR2, host-configured). GraftX will carve it into a header, two SPSC descriptor rings, and a data arena:

```text
 offset 0           ┌──────────────────────────────────────┐
                     │ ShmHeader: magic, version, layout,    │  one cache line, never moves
                     │  c2s_head/tail, s2c_head/tail (atomics)│
                     ├──────────────────────────────────────┤
                     │ c2s descriptor ring  (N * Descriptor) │  client → server
                     ├──────────────────────────────────────┤
                     │ s2c descriptor ring  (N * Descriptor) │  server → client (results)
                     ├──────────────────────────────────────┤
                     │ data arena (bump/slab allocator)      │  the actual payload bytes
 offset region_len  └──────────────────────────────────────┘
```

The canonical bulk descriptor is `ShmSlice` (D6): the **one** struct that names a span of the shared region. It is defined here and referenced (never redefined) by the Memory chapter (Ch. 12). The shared region is `<= 4 GiB`, so 32-bit offset/len suffice; `gen` is the slot/epoch guard that detects a stale or recycled slice.

```rust
/// Canonical bulk descriptor (D6). Defined once here; the Memory chapter
/// (Ch. 12) references this, it does not redeclare it.
#[repr(C)]
struct ShmSlice {
    offset: u32,     // byte offset into the data arena (region <= 4 GiB)
    len:    u32,     // payload length
    gen:    u32,     // slot/epoch guard — rejects a stale or recycled slice
}

/// Ring slot = canonical slice + transport routing metadata.
/// The fence/ordering `seq:u64` is NOT carried here; ordering is owned by the
/// Sync chapter (Ch. 13) and travels in the protocol FrameHeader (Ch. 06). The
/// slice is paired with its control frame by the protocol `req_id`/`seq`.
#[repr(C)]
struct Descriptor {
    slice: ShmSlice, // offset/len/gen
    kind:  u16,      // BUFFER / TEXTURE / READBACK / ...
    _pad:  u16,
}

/// Canonical shared-memory header (D6) — owned by this (Transport) chapter.
#[repr(C, align(64))]
struct ShmHeader {
    magic:    AtomicU64,   // "GRAFTXSM"
    version:  u32,
    ring_cap: u32,         // N (power of two)
    c2s_head: AtomicU32,   // producer index, written by client
    c2s_tail: AtomicU32,   // consumer index, written by server
    s2c_head: AtomicU32,   // producer index, written by server
    s2c_tail: AtomicU32,   // consumer index, written by client
    arena_off: u32, arena_len: u32,
}
```

The rings are **single-producer/single-consumer**: for `c2s` only the client advances `head`, only the server advances `tail`. This makes them lock-free with `Acquire`/`Release` fences and no CAS. Ring occupancy uses the standard "one slot reserved" convention (`head == tail` empty, `head + 1 == tail` full) so the empty/full ambiguity is avoided without a separate count.

**Visibility ordering** is the only subtle part and is the heart of the correctness argument:

1. Producer writes payload bytes into the arena at `offset`.
2. Producer writes the `Descriptor` fields.
3. Producer publishes by storing the new `head` with `Ordering::Release`.
4. Consumer loads `head` with `Ordering::Acquire`; the matching Acquire/Release pair guarantees that the payload and descriptor writes are visible before the consumer reads them.

Across two guests this relies on ivshmem mapping the same physical pages and on the architecture's cache coherence (x86-64 is coherent across the shared region). The `Release` store on `head` is the single synchronization point; everything written before it is observed by any thread that Acquire-loads the new `head`. The implementation will document this exact ordering in a `// SAFETY:` block and keep all raw pointer access in one `unsafe` module.

## 8.5 Doorbells and interrupts vs polling

After publishing to a ring, the producer must wake the consumer. Two mechanisms:

| Mechanism | Latency | CPU cost when idle | When |
|---|---|---|---|
| **Busy-poll** the ring head | ~ns | 100% of one core | bursty, latency-critical phases |
| **ivshmem doorbell** (MSI-X via eventfd) | ~µs (vmexit) | ~0 | idle / steady state |

GraftX will use an **adaptive hybrid**: poll for a short spin window after the last activity, then arm the doorbell and block on its eventfd. This is the classic "spin then sleep" used by io_uring and DPDK and avoids paying a vmexit per frame under load while not burning a core when idle.

The `armed` flag lives in the shared region: the **consumer** sets it to announce "I am about to sleep, ring me"; the **producer** checks it after publishing to decide whether a doorbell is needed. The spin-then-doorbell race (producer publishes in the window between the consumer's last poll and its block on the eventfd) is closed by a symmetric `armed`-flag protocol with **explicit atomic ordering on both sides**:

- **Consumer** (about to sleep): store `armed = true` with `Ordering::SeqCst`, then **re-load the ring head** (`Acquire`) once more. The `SeqCst` store-then-load prevents the store from being reordered after the head re-check; if work appeared, clear `armed` (`Release`) and proceed without sleeping.
- **Producer** (after publishing): the `Release` store on the ring `head` (8.4) is followed by a `SeqCst` load of `armed`. The shared `SeqCst` total order guarantees that at least one of the two threads observes the other's write — so the producer either sees `armed == true` and rings the bell, or the consumer sees the published head and never sleeps. Neither can both miss.

```rust
// Consumer side.
fn wait_for_work(&self) -> Result<(), TransportError> {
    let spin_deadline = Instant::now() + self.spin_window; // e.g. 50 µs
    loop {
        if self.ring.has_pending() { return Ok(()); }      // Acquire-load of head
        if Instant::now() < spin_deadline { std::hint::spin_loop(); continue; }
        self.armed.store(true, Ordering::SeqCst);          // announce "interrupt me"
        if self.ring.has_pending() {                       // re-check (Acquire): closes the race
            self.armed.store(false, Ordering::Release);
            return Ok(());
        }
        self.eventfd.read_blocking()?;                     // sleeps until peer rings
        self.armed.store(false, Ordering::Release);
        return Ok(());
    }
}

// Producer side, after publishing the descriptor and Release-storing the new head.
fn notify(&self) {
    if self.peer_armed.load(Ordering::SeqCst) {            // pairs with consumer's SeqCst store
        self.eventfd.write_kick();                         // ring the doorbell (vmexit)
    }
}
```

Because the consumer's `armed = true` store and the producer's `armed` load are both `SeqCst`, and the producer's preceding `head` store and the consumer's re-check are an Acquire/Release pair (8.4), the lost-wakeup window is provably closed: if the producer published before its `armed` load, either it observes `armed == true` and kicks, or the consumer's re-check observes the new head and skips the sleep. The doorbell is rung only when the consumer has announced it is sleeping, so steady-state high throughput rings the bell rarely. This ordering will be documented in a `// SAFETY:` block alongside the ring atomics.

## 8.6 Flow control and backpressure

Resource limits and backpressure are an explicit security requirement, not an afterthought — an untrusted client must not be able to exhaust server memory by flooding frames or oversizing bulk allocations.

**Control plane (vsock).** The stream itself provides byte-level backpressure: `send` blocks (or returns `WouldBlock` in the future async backend) when the socket buffer is full, which propagates back to the client shim and naturally throttles call submission. On top of that, GraftX adds a **credit scheme**: the server grants the client N in-flight command credits during the handshake; each command consumes a credit, each completion returns one. The client must not exceed its credit window. This bounds the server's in-flight work queue independently of socket buffering and is the mechanism that enforces the per-session command quota.

**Bulk plane (ivshmem).** The data arena is finite. `bulk_alloc` fails with `BulkExhausted` when the arena cannot satisfy a request; the client must wait for the server to consume and free descriptors (advancing `c2s_tail`) before retrying. Allocation is a bump pointer with a free-list reclaiming completed descriptors; a single oversized request is rejected against the negotiated `max_bulk` cap. The arena therefore acts as a hard ceiling on outstanding bulk bytes — the client physically cannot stage more than the region holds.

```text
client                                   server
  │ bulk_alloc(len) ──► [arena has room?] ── no ──► BulkExhausted (caller waits)
  │                                  yes
  │ write payload, publish desc (slot=K)
  │ send ctrl frame {opcode, BULK_REF → slot K} ──vsock──►
  │                                        recv ctrl, read desc K
  │                                        COPY arena[K] → private buf  (§8.1 rule)
  │                                        validate + replay
  │                                        advance c2s_tail (frees arena slot)
  │ ◄──── completion (returns 1 credit) ────────────────
```

The decision table for "should the client block":

| Control credits | Arena free | Action |
|---|---|---|
| > 0 | enough | submit immediately |
| 0 | any | block on completion (credit return) |
| > 0 | too little | block on `c2s_tail` advance (doorbell) |

This dual-window scheme means neither plane can independently overwhelm the server, and the slowest stage (validation/replay on the server) sets the natural rate.

## 8.7 Multiplexing channels

A single GPU application has multiple logical streams that must not head-of-line-block each other: e.g. a Vulkan queue submission stream, an asynchronous transfer/readback stream, and out-of-band fence/event signals. The `channel` byte in the frame header (8.3) multiplexes these over the one vsock connection and one ivshmem region.

Channels are lightweight: each is a logical FIFO of frames tagged with the same `channel` id; the demux loop on the receiver routes by id into per-channel queues. The `LAST` flag (a protocol `FrameHeader` flag, §8.3) lets a single logical message span multiple frames (large inline args split to respect `max_frame_body`) while preserving message boundaries. A small fixed channel map is proposed:

```text
  ch 0  CONTROL   handshake, capability negotiation, teardown
  ch 1  COMMANDS  the main serialized GPU call stream (ordered)
  ch 2  TRANSFER  bulk-referencing copies / readbacks
  ch 3  FENCES    fence/semaphore signals, completions (low-latency, may overtake)
  ch 4+ reserved  future per-queue or per-context streams
```

Ordering guarantee: **within a channel, frames are strictly ordered**; **across channels, no ordering is implied**. This lets a fence signal on ch 3 bypass a long queue of buffered commands on ch 1, which is exactly the semantics GPU fences need. The protocol layer (the Protocol chapter (Ch. 06)) assigns opcodes to channels; the transport only routes bytes by `channel` and never inspects further.

## 8.8 Reconnection and lifecycle

The paired-guest channel is reachability scoping, not authentication; sessions are still explicit so that a crash on either side is recoverable.

- **Handshake** (Hello / Welcome, the Protocol chapter (Ch. 06)) negotiates `proto_major`/`proto_minor`, `max_frame_body` (default 1 MiB), `max_bulk`, credit window, `session_id`, and the ivshmem layout. Either side refusing yields a clean `Retry`/close, never a half-open channel.
- **Peer death.** vsock `recv` returning 0 bytes → `PeerClosed`. The client shim cannot transparently resume a GPU context across a server crash (GPU handles are gone), so reconnection establishes a **fresh session** and surfaces a context-loss error to the application (the same way `VK_ERROR_DEVICE_LOST` / `cudaErrorDeviceUnavailable` would). The transport's job is to fail fast and cleanly, not to fake continuity.
- **ivshmem after reconnect.** The new session re-zeroes ring indices via the `ShmHeader` (the `magic` is rewritten last, as a Release publish, so a peer never observes a half-initialized header). Stale descriptors from the dead session are discarded because the consumer starts from a freshly negotiated `tail`.
- **Backoff.** The client retries connection with bounded exponential backoff; the server's listener is always ready to accept exactly one paired session at a time (multi-session is out of scope for the transport; if added, it is N independent `Transport` instances, not shared rings).

## 8.9 Tradeoffs recorded

- **Sync trait now, async later.** The M0 `Transport` is blocking; the spin-then-doorbell waiter is a synchronous building block. An async backend (tokio `AsyncRead`/`AsyncWrite` over vsock) is feasible later without changing callers if we keep `send`/`recv` semantics. We start sync because the server replay loop is fundamentally sequential per context and async buys little there while adding complexity.
- **SPSC rings over a single MPMC queue.** Two SPSC rings are lock-free and trivially correct across the guest boundary; an MPMC design would need cross-guest CAS, which is harder to reason about and to prove safe. Multiplexing (8.7) handles the "many logical streams" need above the ring.
- **Copy on the bulk plane is mandatory, not optional.** Zero-copy is the *reason* for ivshmem, yet the server still copies into private memory before validation (8.1). The win is that the copy is a single, server-controlled, sequential read of coherent memory — far cheaper than two kernel copies through vhost-vsock — and it preserves the security invariant that the server never replays bytes the client can still mutate.

The result is a transport that is breadth-friendly (any API's frames are just opaque bytes on a channel), fast where it matters (bulk goes through shared memory with adaptive interrupts), and safe by construction (bounded windows, copy-before-trust, clean session lifecycle).
