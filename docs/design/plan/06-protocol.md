# 06. Wire Protocol: Framing, Opcodes & Versioning

This chapter specifies the GraftX wire protocol — frame layout, the per-API opcode namespace, request/response/event framing, version negotiation, handle references, command batching, and error encoding — as the contract both the client shim and the replay server must honor.

The wire protocol is the contract that lets a Linux client shim (the Client shim chapter, Ch. 09) and the Windows replay server (the Server core chapter, Ch. 10) agree on the meaning of every byte exchanged over the transport (the Transport chapter, Ch. 08). It lives in the `graftx-protocol` crate, which today exposes only `PROTOCOL_VERSION: u32 = 0` and a stub `ProtocolError`. This chapter expands that skeleton into a full design. The codec itself (zero-copy serialization, alignment, the `Encode`/`Decode` traits) is detailed in the Serialization chapter (Ch. 07); here we define the *structure* the codec serializes, not the byte-packing mechanics. Schema-generation tooling that emits opcodes and stub bodies from API headers is the Build/dist chapter (Ch. 28).

## 6.1 Layering and terminology

GraftX separates the **control plane** (small, ordered, reliable messages over virtio-vsock) from the **bulk plane** (large payloads over ivshmem shared memory). The wire protocol governs the control plane and the *descriptors* that reference bulk-plane regions. Three message kinds flow over the control plane:

- **Request** — a forwarded API call (`vkCreateBuffer`, `glDrawArrays`, …), client → server.
- **Response** — the server's reply to a request that needs a return value, server → client.
- **Event** — an unsolicited server → client notification (device-lost, debug-callback, async-completion). Events carry no client-supplied correlation id.

```text
+-----------------------------------------------------------------+
|  Transport frame (control plane, vsock) — owned by Ch. 08       |
|  {magic, channel, len}  wraps an OPAQUE body                    |
|  +-----------------------------------------------------------+  |
|  |  Protocol frame (this chapter)                            |  |
|  |  +------------+   +----------------------------------+     |  |
|  |  | FrameHeader|   | Body: Request | Response | Event |     |  |
|  |  +------------+   +----------------------------------+     |  |
|  |         |                          |                       |  |
|  |         | inline payload <= 4 KiB  | bulk descriptor       |  |
|  |         v                          v                       |  |
|  |   (bytes in frame)          ShmSlice{offset,len,gen}       |  |
|  +-----------------------------------------------------------+  |
+-----------------------------------------------------------------+
```

The **magic** lives only in the transport header (the Transport chapter, Ch. 08); the protocol `FrameHeader` defined below does not repeat it. The transport frame `{magic, channel, len}` is a thin wrapper whose body is the opaque protocol frame this chapter specifies (frame layering decision D2). The canonical bulk descriptor `ShmSlice{ offset:u32, len:u32, gen:u32 }` is defined once in `graftx-transport` (the Transport chapter, Ch. 08); this chapter only references it.

## 6.2 Frame format

Every control-plane transmission carries a **protocol frame**: a fixed header followed by a body. This `FrameHeader` is the single canonical definition (frame layering decision D2) — it is defined here in `graftx-protocol` and is never redeclared elsewhere. It is 32 bytes, little-endian, naturally aligned, and *never* changes shape across protocol versions — it is the one structure that must be parseable by any version, so version skew can still produce an intelligible error. There is **no `magic` field** here: the magic belongs to the transport header (the Transport chapter, Ch. 08), which wraps this frame as an opaque body.

```text
 byte 0    2     4     6    8        12        16              24       28    32
 +-----+-----+-----+-----+--------+--------+----------------+--------+------+
 | ver |flags|kind |_pad | opcode | req_id |      seq       |body_len| pad  |
 | u16 | u16 | u16 | u16 |  u32   |  u32   |      u64       |  u32   | u32  |
 +-----+-----+-----+-----+--------+--------+----------------+--------+------+
  minor  bits  enum  --  api<<24|call corr.   ordering/fence   body    align
```

```rust
#[repr(u16)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameKind {
    Request  = 0,
    Response = 1,
    Event    = 2,
    Batch    = 3, // body is a length-prefixed run of sub-frames (§6.8)
    Control  = 4, // handshake, version negotiation, ping (§6.6)
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    pub struct FrameFlags: u16 {
        const HAS_BULK   = 1 << 0; // body references >=1 ivshmem descriptor (ShmSlice)
        const NO_REPLY   = 1 << 1; // fire-and-forget request; server must not respond
        const COMPRESSED = 1 << 2; // body compressed (codec-defined; see the Serialization chapter, Ch. 07)
        const LAST_BATCH = 1 << 3; // final sub-frame in a Batch body
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FrameHeader {
    pub version: u16, // protocol minor; major is fixed per connection (§6.6)
    pub flags: u16,   // FrameFlags bits
    pub kind: u16,    // FrameKind
    pub _pad: u16,    // alignment to the u32 opcode
    pub opcode: u32,  // (ApiId << 24) | call_id (§6.3); 0 for non-Request kinds
    pub req_id: u32,  // request/response correlation id, owned here (§6.4)
    pub seq: u64,     // per-session monotonic ordering/fence sequence (owned by the Sync chapter, Ch. 13)
    pub body_len: u32,
    pub _pad2: u32,   // tail padding -> fixed 32-byte, 8-byte-aligned header
}
```

`body_len` is bounded by a negotiated `max_frame_body` (default 1 MiB; §6.6). Anything larger must travel on the bulk plane via a `ShmSlice` descriptor, never inline. The decoder rejects `body_len > max_frame_body` with `ProtocolError::FrameTooLarge` *before* allocating, so a hostile stream cannot force a large allocation — this is the first line of the untrusted-stream defense the security model demands (the Security chapter, Ch. 23).

Two distinct ids ride the header (ordering-vs-correlation decision D3): `req_id:u32` correlates a Response to its Request and is owned by this chapter; `seq:u64` is the per-session monotonic ordering/fence sequence owned by the Sync chapter (Ch. 13). They are different fields with different purposes — there is no shared u32 "sequence" doing double duty.

`version` in the header is the **minor** version (§6.6). The **major** is fixed for the lifetime of a connection by the handshake; a frame whose implied major differs is a protocol violation and tears down the session. Carrying minor per-frame is essentially free and lets a server log exactly which minor a misbehaving client used.

## 6.3 Opcode namespace per API

An opcode is a `u32` = `(ApiId << 24) | call_id` (opcode-scheme decision D4). The high byte is the **API id**; the low 24 bits are the **call id** within that API. The opcode lives in the `FrameHeader` (§6.2), so the dispatcher (the Server core chapter, Ch. 10) routes on a single byte before touching the body. This canonical `ApiId` table is co-owned by this chapter and the Appendices chapter (Ch. 32).

```rust
#[repr(u8)]
pub enum ApiId {
    Core   = 0x00, // handshake, ping, handle lifecycle — never an external API
    Vulkan = 0x01,
    OpenGl = 0x02, // GL + GLES + EGL/GLX glue share one table; ES is a profile flag
    Cuda   = 0x03,
    OpenCl = 0x04,
    Hip    = 0x05, // ROCm/HIP
    LevelZero = 0x06,
    Video  = 0x07, // NVENC/NVDEC/VA-API/AMF-video umbrella
    // 0x08..=0xFE reserved (WebGPU/OptiX/AMF/SYCL tier-3 assigned here as they land);
    // 0xFF = Vendor/experimental escape
}

#[inline]
pub const fn opcode(api: ApiId, call: u32) -> u32 {
    debug_assert!(call < (1 << 24));
    ((api as u32) << 24) | (call & 0x00FF_FFFF)
}
pub const fn api_of(op: u32) -> u8 { (op >> 24) as u8 }
pub const fn call_of(op: u32) -> u32 { op & 0x00FF_FFFF }
```

Within an API, call ids are assigned by the schema generator (the Build/dist chapter, Ch. 28) in **source order of the API's command table**, never alphabetically, so appending a new entrypoint to a header only appends call ids and never renumbers existing ones. The generator emits a frozen `opcodes.lock` manifest checked into the repo; CI fails if a previously assigned `(api, name) -> call` mapping changes. This makes opcode stability a build-time invariant rather than a convention. M0 (the Milestones chapter, Ch. 30) uses this exact scheme: Vulkan opcodes occupy `0x01_000000..`.

| API block | API id | Approx. call count | Notes |
|-----------|--------|--------------------|-------|
| Core      | 0x00   | ~16                | handshake, ping, handle free, flush |
| Vulkan    | 0x01   | ~400               | the spine (M1); largest table |
| OpenGL(+ES)| 0x02  | ~700               | GL 4.6 + GLES 3.2 union, plus EGL/GLX glue |
| CUDA      | 0x03   | ~300               | driver + a subset of runtime |
| OpenCL    | 0x04   | ~150               | 1.2/2.x core |
| HIP       | 0x05   | ~150               | tier 2 |
| Level Zero| 0x06   | ~100               | tier 2 |
| Video     | 0x07   | varies             | NVENC/NVDEC/VA-API umbrella |
| Tier-3    | 0x08+  | varies             | WebGPU/OptiX/AMF/SYCL reach APIs |

A `0xFF` vendor escape lets out-of-tree forks add opcodes without colliding with the assigned ranges; such frames are rejected by a stock server unless an extension is registered, satisfying the "validate decoded commands" rule.

## 6.4 Request / Response / Event framing

The opcode rides in the `FrameHeader` (§6.2), so a **Request** body is just the marshalled arguments in declaration order, then optional trailing bulk descriptors. Argument layout is codec-defined (the Serialization chapter, Ch. 07); the protocol mandates only the header opcode and a `reply` discriminator the generator derives from whether the C function returns a value or has out-parameters.

```rust
// Request body has no header of its own — opcode + req_id + seq are in FrameHeader.
// Body = args... (codec-encoded, see the Serialization chapter, Ch. 07)

pub struct ResponseHeader {
    pub req_id: u32,     // echoes the Request's FrameHeader.req_id (§6.2)
    pub status: i32,     // ApiStatus: 0 = ok, <0 = protocol/transport err,
                         //            >0 = native API error code (§6.9)
    // return value + out-params... (codec-encoded)
}

pub struct EventHeader {
    pub api: u8,
    pub event_id: u16,   // per-API event table
    // payload...
}
```

**Correlation.** `req_id` in `FrameHeader` is a per-connection monotonic counter assigned by the client (ordering-vs-correlation decision D3); the independent `seq:u64` carries ordering/fences and is owned by the Sync chapter (Ch. 13). Responses echo `req_id`; the client matches replies to outstanding requests via a slab keyed by `req_id`. `req_id` wraps at `u32::MAX`; the client guarantees no more than `2^31` requests are in flight, so a wrapped value never aliases a live one. Events use `req_id = 0` and are demultiplexed by `(api, event_id)`.

**Reply elision.** Most calls do not need a synchronous reply — `glUniform4f`, `vkCmdDraw`, and the bulk of recording-style APIs return `void` and report errors out-of-band. Such requests set `NO_REPLY` and the client never blocks on them; they stream into a batch (§6.8). Only calls that return a value, allocate a handle, or have out-parameters demand a Response. The generator classifies each entrypoint into `Reply` vs `NoReply` at build time and the classification is part of `opcodes.lock`.

### Sequence walk-through: `vkCreateBuffer`

```text
client                                          server
  | Request req_id=42 op=Vulkan:CreateBuffer     |
  |   args: device=H#7, VkBufferCreateInfo{...}  |
  |--------------------------------------------->|  decode + validate size/usage
  |                                              |  call native vkCreateBuffer
  |                                              |  server MINTS handle H#88
  | Response req_id=42 status=0 buffer=H#88      |
  |<---------------------------------------------|
  | (client records the server-minted H#88;      |
  |  any local provisional token is reconciled)  |
```

## 6.5 Handle references on the wire

Native APIs return opaque pointers/handles (`VkDevice`, `cudaStream_t`, GL `GLuint` names). These pointers are *server-side* addresses and must never cross to the client raw — leaking a host pointer is both a security and a portability problem. The protocol uses **virtual handles**: 64-bit tokens minted by the server, opaque to the client. This is the one canonical wire `Handle` layout (handle-layout decision D5), shared by this chapter and the Handles chapter (Ch. 11), where it is also named `HandleId`.

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Handle(pub u64);
//  bits 63..56 (8 bits): HandleKind / API-namespace (device, buffer, stream, ...)
//  bits 55..32 (24 bits): generation (incremented on free; catches use-after-free)
//  bits 31..0  (32 bits): slot index into the server handle table
```

The server keeps a per-session handle table (slot + generation; the object model and id tables are owned by the Handles chapter, Ch. 11, and resolved by the Server core chapter, Ch. 10). On creation it returns a `Handle`; on every subsequent request the client passes the `Handle` and the server resolves it to the native object, checking `kind` and `generation`. A stale generation yields `ProtocolError::StaleHandle` rather than a wild dereference — central to validating an untrusted stream. GL integer names (`GLuint`) are likewise wrapped: the client never sees the server's real name, only a `Handle` whose low bits the client may reuse as the "name" it hands the application, with a client-side bimap.

**Generation-wrap handling.** The 24-bit generation field counts free/reuse cycles of a slot. When a slot's generation is about to wrap past `2^24 - 1`, the server *retires* that slot — it is removed from the free list and never reissued for the session — rather than wrapping the counter and risking a stale handle aliasing a fresh object. With 32-bit slot indices the address space is large enough that retiring the rare wrap-prone slot is cheap; this keeps the use-after-free guard sound without widening the field.

Handles are **server-authoritative** (handle-authority decision D1): the server mints every wire `Handle`, and the client never puts an invented handle on the wire as authority — so a malicious client cannot forge a handle to an object it never created. For async/deferred replies the client may keep a purely **local provisional proxy token** (never sent as authority) that it reconciles deterministically when the server's real handle arrives in the Response. The cost is one extra map lookup per argument, acceptable given breadth > performance and amortized by the batch path.

## 6.6 Version negotiation

A connection opens with a `Core` handshake exchange before any API frame: the client sends **Hello**, the server replies **Welcome** (handshake decision D9). The major version gates wire-incompatible changes; minor versions are additive (new opcodes, new optional flags).

```rust
#[repr(u8)] pub enum CoreOp { Hello = 0, Welcome = 1, Goodbye = 2, Ping = 3, Pong = 4 }

pub struct Hello {  // client -> server
    pub proto_major: u16,      // no magic here — magic is the transport header's (Ch. 08)
    pub proto_minor_max: u16,  // highest minor the client speaks
    pub features: u64,         // capability bitset (compression, bulk-plane, ...)
    pub max_frame_body: u32,   // largest inline body the client will send
}
pub struct Welcome { // server -> client; fields agreed with the Milestones chapter (Ch. 30)
    pub proto_major: u16,      // MUST equal Hello.proto_major or session aborts
    pub proto_minor: u16,      // min(client_max, server_max) — the agreed minor
    pub features: u64,         // intersection of both sides' capabilities
    pub max_frame_body: u32,   // min of both sides (name + 1 MiB default, decision D8)
    pub session_id: u64,
}
```

Negotiation rule: **major must match exactly; minor is `min(client_max, server_max)`; features are the intersection; `max_frame_body` is the min of both sides (default 1 MiB).** A server that speaks minor 5 talking to a minor-3 client downshifts to 3 and must not emit any opcode or flag introduced after minor 3. Because call ids are append-only (§6.3), "introduced after minor N" is a simple per-opcode `since_minor` field in `opcodes.lock`, and the server can reject an opcode the negotiated minor predates with `ProtocolError::OpcodeNotInMinor`.

```text
proto_major mismatch  -> Goodbye{reason=MajorMismatch}; close
proto_major match     -> Welcome{minor=min(...), features=A&B}; proceed
```

`PROTOCOL_VERSION` (currently `0`) encodes major in the high 16 bits and minor in the low 16 bits; at v0.0.0 both are 0, signalling the pre-stable format that may break without a major bump until M1 freezes it.

## 6.7 Schema evolution

The protocol must absorb new entrypoints and new struct fields without breaking peers. Rules, in priority order:

1. **Append-only opcodes.** Never reuse or renumber a call id (§6.3, enforced by `opcodes.lock`).
2. **Append-only struct fields.** Marshalled structs that mirror evolving C structs (e.g. `VkPhysicalDeviceFeatures2` chains) are length-prefixed; a reader that knows fewer fields reads what it understands and skips the tail. New fields go at the end with a documented `since_minor`.
3. **Optional via flags, not new opcodes.** Behavior toggles ride `FrameFlags` / `features` bits so the same opcode keeps working.
4. **Reserved enum tail.** Each `#[repr(u*)]` enum reserves its top value as `Unknown`; decoding an out-of-range discriminant maps to `Unknown` and the server fails the *call*, not the *connection*.

The pNext extension chains of Vulkan are handled specially: each `sType` is mapped to an opcode-like `StructId` in the same `opcodes.lock`, and an unknown `StructId` in a chain is skipped (matching driver behavior) rather than aborting, preserving forward compatibility with extensions the server predates.

## 6.8 Command batching

Round-tripping one frame per call would make the control plane the bottleneck for record-heavy APIs. The client coalesces consecutive `NO_REPLY` requests into a single `Batch` frame, flushing on (a) a reply-requiring call, (b) an explicit `glFlush`/`vkQueueSubmit`-class barrier, (c) buffer pressure, or (d) a time fuse (default 200 µs).

```text
Batch frame body:
  +-------------+----------------+----------------+ ... +
  | sub_count u32| sub-frame 0   | sub-frame 1    |     |
  +-------------+----------------+----------------+ ... +
  each sub-frame: [u32 len][FrameHeader (§6.2) + args]
```

```rust
pub struct BatchBuilder { buf: Vec<u8>, count: u32, started: Instant }
impl BatchBuilder {
    pub fn push(&mut self, req: &EncodedRequest) -> Result<(), ProtocolError> {
        if self.would_exceed(req.len()) { return Err(ProtocolError::BatchFull); }
        self.buf.extend_from_slice(&(req.len() as u32).to_le_bytes());
        self.buf.extend_from_slice(req.bytes());
        self.count += 1;
        Ok(())
    }
    pub fn should_flush(&self, mtu: usize) -> bool {
        self.buf.len() >= mtu || self.started.elapsed() >= BATCH_FUSE
    }
}
```

The server replays a batch **in order, atomically with respect to ordering**: sub-frame *k* is dispatched before *k+1*, preserving the API's command-stream semantics. If sub-frame *k* faults, the server records the failing index and either continues (for independent NO_REPLY calls) or aborts the remainder and surfaces an Event carrying `(batch_req_id, failed_index, status)` — the policy is per-API and recorded in `opcodes.lock`. Ordering guarantees and flow control on the transport are the Transport chapter (Ch. 08); the per-API replay/fault policy is the Server core chapter (Ch. 10).

Tradeoff: batching trades latency for throughput. The 200 µs fuse caps added latency on a sparse stream while letting a tight draw loop pack hundreds of calls per frame. Reply-requiring calls force an immediate flush, so latency-sensitive query paths are never delayed by a half-full batch.

## 6.9 Error encoding

Errors are split into three numeric ranges on `ResponseHeader.status` (`i32`) so a single field disambiguates *who* failed:

| Range | Meaning | Source | Example |
|-------|---------|--------|---------|
| `0` | success | — | call returned ok |
| `< 0` | GraftX protocol/transport error | `ProtocolError` | `-3` = StaleHandle |
| `> 0` | native API status | the driver | `VK_ERROR_OUT_OF_DEVICE_MEMORY` mapped |

```rust
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("unexpected end of buffer")]              UnexpectedEof,      // -1
    #[error("unknown opcode: {0}")]                   UnknownOpcode(u32), // -2
    #[error("stale or invalid handle: {0:#x}")]       StaleHandle(u64),   // -3
    #[error("frame body {0} exceeds negotiated max")] FrameTooLarge(u32), // -4
    #[error("opcode {0:#x} not in negotiated minor")] OpcodeNotInMinor(u32), // -5
    #[error("batch buffer full")]                     BatchFull,          // -6
    #[error("malformed argument for opcode {0:#x}")]  BadArgument(u32),   // -7
}
impl ProtocolError {
    pub const fn status_code(&self) -> i32 { /* maps each variant to its negative */ }
}
```

The two existing skeleton variants (`UnexpectedEof`, `UnknownOpcode`) are preserved with their meanings intact; the rest are the additions this design requires. Native API codes (`VkResult`, `cudaError_t`, `cl_int`) are *not* unified — each API's response carries its own native code in the positive range, because the client shim must hand the application back the exact code the application's error-handling logic expects. The server validates a decoded command before invoking the driver; validation failures surface as negative `ProtocolError` codes, never as a forged native error, so the client can distinguish "GraftX rejected this" from "the GPU rejected this." Transport-level failures (channel closed, ivshmem region revoked) are surfaced as Events plus a synthetic negative status on every outstanding request, letting the shim fail in-flight calls deterministically.

## 6.10 Open questions

- **Compression of inline bodies** is gated behind a feature bit; whether it is worth the CPU on a same-host vsock link is a perf-track question (the Performance chapter, Ch. 25), not a protocol one — the bit exists so the answer can change without a wire break.
- **Asynchronous completion** for fence/event-style APIs may need a dedicated lightweight Event subtype distinct from the general Event path; deferred until CUDA streams land in M3.
- **Per-session integrity/auth** (planned, the Security chapter, Ch. 23) will add an optional MAC trailer governed by a `features` bit; the 32-byte header reserves no space for it, so it rides as a fixed-size body suffix when negotiated.
