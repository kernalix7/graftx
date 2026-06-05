# GraftX Transport Layer

Engineering reference for the GraftX guest-to-guest transport, as implemented in
[`crates/graftx-transport/src/`](../../crates/graftx-transport/src/). The
transport owns the **channel** that carries opaque frames between the two guests
and nothing else: it moves bytes and does not understand the wire protocol (see
[`PROTOCOL.md`](PROTOCOL.md)) or the GPU command opcodes. This document
describes what the code does today; where the broader design rationale lives, it
points at the implementation-plan chapter
[`plan/08-transport.md`](plan/08-transport.md).

The transport's contract is **one framed message per call**: each
[`send`](#1-the-transport-trait) delivers exactly one frame and each
[`recv`](#1-the-transport-trait) returns exactly one. All multi-byte length
fields on the wire are **little-endian**, matching the protocol layer.

## 1. The `Transport` trait

The crate exposes a minimal synchronous trait — a bidirectional,
message-framed byte channel between two endpoints:

```rust
pub trait Transport {
    /// Send one framed message to the peer.
    fn send(&mut self, frame: &[u8]) -> io::Result<()>;

    /// Receive the next framed message from the peer, blocking until one
    /// arrives or the peer closes.
    fn recv(&mut self) -> io::Result<Vec<u8>>;
}
```

The contract is deliberately small and opaque:

- **One frame per call.** `send` takes one complete frame; `recv` returns one
  complete frame. The transport never splits, merges, or inspects frame
  contents — the body is the protocol frame
  ([`FrameHeader`](PROTOCOL.md#2-frameheader-28-bytes) + payload), which the
  transport treats as opaque bytes.
- **Blocking semantics.** `recv` blocks until a whole frame is available or the
  peer closes; a closed peer surfaces as an
  [`io::ErrorKind::UnexpectedEof`](https://doc.rust-lang.org/std/io/enum.ErrorKind.html)
  (or `BrokenPipe` on the send side).
- **Errors via `io::Result`.** The implemented trait returns `io::Result`. The
  plan proposes a richer `thiserror` `TransportError` enum and a zero-copy bulk
  extension (`bulk_alloc`/`bulk_view`); those are **planned, not present** —
  see [§4](#4-planned-vsock--ivshmem-mapping).

`&mut self` is required because both real backends (a socket cursor, a
shared-memory ring) carry mutable framing state.

## 2. Implemented backends

Four concrete implementations of `Transport` exist today, all in-process or
stream-based. None require a hypervisor; they support host-only testing and
exercising the client/server logic before the vsock/ivshmem backends land.

### Loopback — in-process channel

[`lib.rs`](../../crates/graftx-transport/src/lib.rs) defines `Loopback`, a
message-preserving transport built on a pair of
[`std::sync::mpsc`](https://doc.rust-lang.org/std/sync/mpsc/index.html)
channels. The free function `loopback() -> (Loopback, Loopback)` returns a
*crossed pair*: bytes `send`-ed on one endpoint are `recv`-ed on the other.
Each frame is copied into its own `Vec<u8>`, so message boundaries are
preserved exactly (no reframing is needed — `mpsc` already delivers discrete
items).

- `send` maps a closed peer to `BrokenPipe`.
- `recv` blocks on the channel and maps a closed peer to `UnexpectedEof`.

This is the fastest path to a working `Transport` and is what most unit tests in
the workspace build on.

### StreamTransport — length-prefixed framing over a byte stream

[`stream.rs`](../../crates/graftx-transport/src/stream.rs) defines
`StreamTransport<S>` for any `S: Read + Write` (a TCP or vsock socket, a pipe,
an in-memory `Cursor`). A raw byte stream has no message boundaries, so
`StreamTransport` adds them with a **4-byte little-endian length prefix**:

```text
 ┌───────────────────────┬──────────────────────────────┐
 │ length: u32 LE (4 B)   │ frame body (length bytes)     │
 └───────────────────────┴──────────────────────────────┘
```

- **`send`** writes the length header, then the frame bytes, then `flush`es.
  A frame longer than `u32::MAX` is rejected with `InvalidInput` before any
  write.
- **`recv`** reads the 4-byte header, then exactly that many body bytes. A clean
  EOF *on the header boundary* (the peer closed between frames) is normalized to
  `UnexpectedEof` ("stream closed before frame header") rather than surfacing as
  a partial read; a truncated body propagates the underlying `UnexpectedEof`.
- **`MAX_FRAME` cap.** A declared length above `MAX_FRAME` (`64 * 1024 * 1024`,
  64 MiB) is rejected with `InvalidData` **before any allocation**. This bounds
  the memory a peer can force the receiver to reserve from a single header — a
  malicious or corrupt length cannot trigger an arbitrary `Vec` allocation.

The framing is symmetric: two `StreamTransport`s over the two ends of a
connected stream interoperate. `new(inner)` wraps a stream and `into_inner()`
recovers it. This is the backend a real vsock or TCP connection plugs into.

### CountingTransport — byte/frame instrumentation

[`counting.rs`](../../crates/graftx-transport/src/counting.rs) defines
`CountingTransport<T>`, a wrapper over any `T: Transport` that tallies traffic
without changing behaviour:

- Tracks four cumulative `u64` counters — `bytes_sent`, `bytes_recv`,
  `frames_sent`, `frames_recv` — exposed via getters.
- Counters advance **only on a successful** `send`/`recv`; a failed operation
  leaves every counter untouched.
- All increments **saturate** (`saturating_add`), so a counter pins at
  `u64::MAX` rather than wrapping.
- `new(inner)` starts every counter at zero; `into_inner()` returns the wrapped
  transport.

Because it is generic over `T: Transport`, it composes with any other backend
(e.g. `CountingTransport<StreamTransport<TcpStream>>`).

### NullTransport — sink with no peer

[`null.rs`](../../crates/graftx-transport/src/null.rs) defines `NullTransport`, a
zero-sized sink:

- `send` always succeeds and discards the bytes.
- `recv` always fails with `UnexpectedEof` ("null transport has no peer"), since
  no data will ever arrive.

Use it to benchmark the encode path in isolation, or in tests that exercise only
the send side, where a real backend would add noise.

## 3. The `roundtrip` helper

[`lib.rs`](../../crates/graftx-transport/src/lib.rs) provides a
request/response convenience for blocking clients that issue one message and
wait for exactly one answer:

```rust
pub fn roundtrip<T: Transport>(t: &mut T, frame: &[u8]) -> io::Result<Vec<u8>>;
```

It is exactly `send(frame)?` followed by `recv()`; either step's error is
propagated unchanged. It adds no framing of its own — it only sequences the two
trait calls.

## 4. Planned: vsock + ivshmem mapping

The implemented backends are sufficient for host-only bring-up. Production will
add two hypervisor-backed backends, both implementing the same `Transport`
trait, so the client and server are written once. The full design is in
[`plan/08-transport.md`](plan/08-transport.md); the points relevant to this
trait are summarized here. **All of this is planned, not yet implemented.**

GraftX splits traffic across two planes because control and bulk traffic have
opposite profiles:

| Plane | Backend (planned) | Carries | Optimized for |
|---|---|---|---|
| **Control** | `VsockTransport` over `AF_VSOCK` | handshake, command frames, completions, fences | round-trip latency |
| **Bulk** | `IvshmemTransport` over shared memory | large payloads referenced by control frames | throughput, zero-copy |

- **`VsockTransport` (control plane).** virtio-vsock `SOCK_STREAM` is a byte
  stream, so it needs reframing exactly as `StreamTransport` does today — the
  planned wire frame is a fixed 8-byte header `{ MAGIC "GX", channel, reserved,
  length: u32 LE }` wrapping the opaque protocol body. This generalizes the
  current 4-byte length prefix by adding a magic marker (catches desync) and a
  `channel` byte (multiplexing; see [`plan/08-transport.md`](plan/08-transport.md)
  §8.7). The length is bounded by the negotiated `max_frame_body` (default 1 MiB)
  rather than the fixed `MAX_FRAME`.
- **`IvshmemTransport` (bulk plane).** ivshmem maps a host-allocated region into
  both guests via a PCI BAR; a write by one guest is visible to the other with
  no kernel copy. The region is carved into a `ShmHeader`, two single-producer
  /single-consumer descriptor rings, and a data arena. This plane needs the
  planned trait extensions (`bulk_alloc`/`bulk_view`) to express zero-copy
  transfers, which the current `recv -> Vec<u8>` signature cannot.
- **`SplitTransport { ctrl, bulk }`.** Production composes the two: control
  frames travel over vsock, large payloads over ivshmem referenced by descriptor
  from a control frame. On hosts without ivshmem, the system degrades gracefully
  to vsock-only (paying the copy cost).

A security invariant constrains the bulk plane: shared-memory payloads land in a
region the untrusted client can still mutate after the server has read the
descriptor, so the server must **copy validated bulk data into private memory
before use**. The transport exposes the raw region; the copy-then-validate
discipline lives in the server (see [`SECURITY_MODEL.md`](SECURITY_MODEL.md)).

The relationship to the implemented code:

| Concept | Implemented today | Planned |
|---|---|---|
| `Transport` trait (`send`/`recv`, one frame per call) | yes | extended with bulk + `shutdown` |
| Error type | `io::Result` | `TransportError` (`thiserror`) |
| Stream reframing | `StreamTransport` (4-byte LE prefix) | `VsockTransport` (8-byte `GX` header) |
| Frame-size cap | `MAX_FRAME` (64 MiB, fixed) | `max_frame_body` (negotiated, default 1 MiB) |
| Shared-memory bulk plane | — | `IvshmemTransport` + `ShmHeader`/`ShmSlice` rings |
| Channel multiplexing | — | `channel` byte in transport header |

See also [`PROTOCOL.md`](PROTOCOL.md) for the protocol frame the transport
carries, and [`plan/06-protocol.md`](plan/06-protocol.md) for the handshake that
negotiates `max_frame_body` and the ivshmem layout.
