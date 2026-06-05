# GraftX Wire Protocol

Engineering reference for the GraftX control-plane wire format, as implemented
in [`crates/graftx-protocol/src/lib.rs`](../../crates/graftx-protocol/src/lib.rs).
That crate is the single source of truth for the wire contract; both the Linux
client shim and the Windows server link it. This document describes what the
code does today — where the broader design rationale lives, it points at the
implementation-plan chapters [`plan/06-protocol.md`](plan/06-protocol.md) and
[`plan/08-transport.md`](plan/08-transport.md).

All multi-byte integers on the wire are **little-endian**. Sizes below are in
bytes.

## 1. Frame layering: transport vs. protocol

GraftX uses two nested frame layers:

```
┌─────────────────────────────────────────────────────────────┐
│ Transport frame:  magic + channel + length + ────────────┐   │
│                                                           ▼   │
│                   ┌───────────────────────────────────────┐  │
│                   │ Protocol frame (opaque to transport):  │  │
│                   │   FrameHeader (28 bytes) + body        │  │
│                   └───────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

- The **transport frame** carries a magic marker, a channel id, and a body
  length, and wraps an *opaque* body. The magic lives in the transport layer,
  **not** in the protocol header. Transport framing is specified in
  [`plan/08-transport.md`](plan/08-transport.md).
- The **protocol frame** is that opaque body: a fixed-size [`FrameHeader`](#2-frameheader-28-bytes)
  followed by a per-opcode payload. This is what `graftx-protocol` defines.

The protocol crate provides `encode_frame(header, body) -> Vec<u8>` and
`decode_frame(buf) -> (FrameHeader, &[u8])`. `decode_frame` returns the header
plus a slice of the bytes that follow it; the caller is responsible for
validating `body_len` against the slice it actually received (a mismatch maps to
`ProtocolError::BodyLenMismatch`).

## 2. FrameHeader (28 bytes)

Every protocol frame begins with this fixed 28-byte header. The encoded order
and sizes are:

| Offset | Size | Field      | Type       | Notes                                            |
|-------:|-----:|------------|------------|--------------------------------------------------|
|      0 |    2 | `version`  | `u16`      | Protocol major version of the sender.            |
|      2 |    2 | `flags`    | `u16`      | Reserved flag bits; none defined yet.            |
|      4 |    2 | `kind`     | `u16`      | [`FrameKind`](#3-framekind) discriminant.        |
|      6 |    2 | (reserved) | `u16`      | Always written as `0`; ignored on decode.        |
|      8 |    4 | `opcode`   | `u32`      | Entrypoint opcode (see [§4](#4-opcode-scheme)).  |
|     12 |    4 | `req_id`   | `u32`      | Request/response correlation id.                 |
|     16 |    8 | `seq`      | `u64`      | Per-session monotonic ordering/fence sequence.   |
|     24 |    4 | `body_len` | `u32`      | Length of the payload following this header.     |

Total: `2 + 2 + 2 + 2 + 4 + 4 + 8 + 4 = 28` bytes, exported as the constant
`HEADER_LEN = 28`.

Note that the 2-byte reserved slot at offset 6 is a wire-only field: it has no
corresponding struct member. The encoder writes `0u16` there, and the decoder
reads and discards it. The `FrameHeader` struct itself exposes `version`,
`flags`, `kind`, `opcode`, `req_id`, `seq`, and `body_len`.

### req_id vs. seq

These are two distinct ids with distinct roles:

- **`req_id`** (`u32`) is the request/response **correlation** id. A `Response`
  frame echoes the `req_id` of the `Request` it answers, so the client can match
  a reply to its outstanding call.
- **`seq`** (`u64`) is the per-session **monotonic ordering / fence** sequence.
  It establishes ordering across frames on a session and is the basis for
  fencing; it is not used for request/response matching.

## 3. FrameKind

`FrameKind` is the `kind` field, encoded as a `u16`:

| Value | Variant    | Meaning                                                    |
|------:|------------|------------------------------------------------------------|
|   `0` | `Request`  | A client-issued request.                                   |
|   `1` | `Response` | A server reply, correlated to a request by `req_id`.       |
|   `2` | `Event`    | An unsolicited server → client event (e.g. async completion). |

Decoding any other value yields `ProtocolError::BadKind(value)`.

## 4. Opcode scheme

An `opcode` is a `u32` partitioned into an API namespace and a call id:

```
opcode = (ApiId << 24) | (call_id & 0x00FF_FFFF)
```

- The **high byte** (`bits 24..32`) is the [`ApiId`](#41-apiid-table) — the API
  namespace.
- The **low 24 bits** (`bits 0..24`) are the call id within that API.

Helpers in the crate: `opcode(api, call)` builds one, `opcode_api(op)` extracts
the namespace byte, and `opcode_call(op)` extracts the 24-bit call id. So, for
example, every Vulkan opcode has the form `0x01_xxxxxx`. An opcode that maps to
no known entrypoint decodes to `ProtocolError::UnknownOpcode(op)`.

### 4.1 ApiId table

| `ApiId`     | Value  | Namespace                                                       |
|-------------|-------:|-----------------------------------------------------------------|
| `Core`      | `0x00` | Core protocol entrypoints (handshake, no-op, control).          |
| `Vulkan`    | `0x01` | Vulkan.                                                         |
| `OpenGl`    | `0x02` | OpenGL / OpenGL ES / EGL / GLX.                                 |
| `Cuda`      | `0x03` | CUDA.                                                           |
| `OpenCl`    | `0x04` | OpenCL.                                                         |
| `Hip`       | `0x05` | ROCm / HIP.                                                     |
| `LevelZero` | `0x06` | Intel Level Zero.                                               |
| `Video`     | `0x07` | Video codecs (VA-API / VDPAU / NVENC / NVDEC / Vulkan Video).   |
| `WebGpu`    | `0x08` | WebGPU.                                                         |
| `OptiX`     | `0x09` | NVIDIA OptiX ray tracing.                                       |
| `Sycl`      | `0x0A` | SYCL.                                                           |
| `Amf`       | `0x0B` | AMD Advanced Media Framework (AMF) video encode.                |

The namespaces `OptiX`, `Sycl`, and `Amf` are reserved in the `ApiId` enum but
do not yet have call modules or opcodes defined.

### 4.2 Defined opcodes

Opcodes live in per-API modules. The authoritative, machine-checked list of
assigned opcodes is [`opcodes.lock`](opcodes.lock); renumbering an opcode there
fails the `opcodes-lock --check` gate. The currently defined entrypoints are:

| Module     | API         | Opcode (`call_id`) | Constant                         |
|------------|-------------|-------------------:|----------------------------------|
| `core_op`  | `Core`      | `0x000001`         | `HELLO`                          |
| `core_op`  | `Core`      | `0x000002`         | `WELCOME`                        |
| `core_op`  | `Core`      | `0x000003`         | `NOOP`                           |
| `vk_op`    | `Vulkan`    | `0x0001`           | `CREATE_INSTANCE`                |
| `vk_op`    | `Vulkan`    | `0x0002`           | `DESTROY_INSTANCE`               |
| `vk_op`    | `Vulkan`    | `0x0003`           | `ENUMERATE_PHYSICAL_DEVICES`     |
| `vk_op`    | `Vulkan`    | `0x0004`           | `GET_PHYSICAL_DEVICE_PROPERTIES` |
| `vk_op`    | `Vulkan`    | `0x0005`           | `CREATE_DEVICE`                  |
| `vk_op`    | `Vulkan`    | `0x0006`           | `DESTROY_DEVICE`                 |
| `vk_op`    | `Vulkan`    | `0x0007`           | `GET_DEVICE_QUEUE`               |
| `vk_op`    | `Vulkan`    | `0x0008`           | `DEVICE_WAIT_IDLE`               |
| `vk_op`    | `Vulkan`    | `0x0010`           | `ALLOCATE_MEMORY`                |
| `vk_op`    | `Vulkan`    | `0x0011`           | `CREATE_BUFFER`                  |
| `vk_op`    | `Vulkan`    | `0x0012`           | `BIND_BUFFER_MEMORY`             |
| `gl_op`    | `OpenGl`    | `0x0001`           | `CREATE_CONTEXT`                 |
| `gl_op`    | `OpenGl`    | `0x0002`           | `MAKE_CURRENT`                   |
| `gl_op`    | `OpenGl`    | `0x0003`           | `GEN_BUFFER`                     |
| `cuda_op`  | `Cuda`      | `0x0001`           | `CTX_CREATE`                     |
| `cuda_op`  | `Cuda`      | `0x0002`           | `MEM_ALLOC`                      |
| `cuda_op`  | `Cuda`      | `0x0003`           | `MEM_FREE`                       |
| `hip_op`   | `Hip`       | `0x0001`           | `MALLOC`                         |
| `hip_op`   | `Hip`       | `0x0002`           | `FREE`                           |
| `hip_op`   | `Hip`       | `0x0003`           | `STREAM_CREATE`                  |
| `cl_op`    | `OpenCl`    | `0x0001`           | `CREATE_CONTEXT`                 |
| `cl_op`    | `OpenCl`    | `0x0002`           | `CREATE_BUFFER`                  |
| `cl_op`    | `OpenCl`    | `0x0003`           | `RELEASE_BUFFER`                 |
| `l0_op`    | `LevelZero` | `0x0001`           | `CONTEXT_CREATE`                 |
| `l0_op`    | `LevelZero` | `0x0002`           | `MEM_ALLOC_DEVICE`               |
| `l0_op`    | `LevelZero` | `0x0003`           | `MEM_FREE`                       |
| `video_op` | `Video`     | `0x0001`           | `CREATE_DECODE_SESSION`          |
| `video_op` | `Video`     | `0x0002`           | `DECODE_FRAME`                   |
| `video_op` | `Video`     | `0x0003`           | `DESTROY_SESSION`                |
| `wgpu_op`  | `WebGpu`    | `0x0001`           | `REQUEST_DEVICE`                 |
| `wgpu_op`  | `WebGpu`    | `0x0002`           | `CREATE_BUFFER`                  |
| `wgpu_op`  | `WebGpu`    | `0x0003`           | `DESTROY_BUFFER`                 |

`opcodes.lock` is regenerated by `cargo xtask opcodes-lock --write` and may lag
the source until the lock is regenerated.

### 4.3 Body encoding conventions

Each opcode has a canonical request/response body, defined by the encode/decode
structs in the matching API module (`vk`, `gl`, `cuda`, `hip`, `cl`, `l0`,
`video`, `wgpu`). Conventions across all of them:

- Every multi-byte field is little-endian.
- A [`Handle`](#6-handle-layout) is carried as its raw 64-bit value (`u64`).
- A variable-length list is a `u32` count followed by that many elements (e.g.
  `EnumeratePhysicalDevicesResponse` is a count followed by that many `u64`
  device handles).
- Some calls have an empty request and/or an empty (ack) response body, in which
  case no struct is defined for that side.
- A body shorter than its declared fields decodes to
  `ProtocolError::UnexpectedEof` (all reads are bounds-checked).

## 5. Handshake

A session opens with a `Core` `HELLO`/`WELCOME` exchange.

### Hello (client → server, opcode `core_op::HELLO`)

| Offset | Size | Field            | Type  | Notes                                          |
|-------:|-----:|------------------|-------|------------------------------------------------|
|      0 |    2 | `proto_major`    | `u16` | Highest protocol major the client supports.    |
|      2 |    2 | `proto_minor`    | `u16` | Highest protocol minor the client supports.    |
|      4 |    4 | `features`       | `u32` | Optional feature bitset the client requests.   |
|      8 |    4 | `max_frame_body` | `u32` | Largest control-frame body the client accepts. |

### Welcome (server → client, opcode `core_op::WELCOME`)

| Offset | Size | Field            | Type  | Notes                                       |
|-------:|-----:|------------------|-------|---------------------------------------------|
|      0 |    2 | `proto_major`    | `u16` | Protocol major the server selected.         |
|      2 |    2 | `proto_minor`    | `u16` | Protocol minor the server selected.         |
|      4 |    4 | `features`       | `u32` | Feature bitset the server granted.          |
|      8 |    4 | `max_frame_body` | `u32` | Negotiated maximum control-frame body.      |
|     12 |    8 | `session_id`     | `u64` | Server-assigned session id.                 |

### Version and frame-body negotiation

- **Versions.** The crate exports `PROTOCOL_MAJOR` and `PROTOCOL_MINOR`, both
  currently `0`. While the major is `0` the wire format is pre-stable and may
  change freely; major is bumped on a breaking wire change, minor on a
  backward-compatible addition. A peer offering an incompatible major maps to
  `ProtocolError::BadVersion { major, minor }`.
- **`max_frame_body`.** Both sides advertise the largest control-frame body they
  are willing to receive. The default is `DEFAULT_MAX_FRAME_BODY = 1 MiB`
  (`1 << 20`). The handshake may **lower** this cap; the server's `Welcome`
  carries the negotiated value. A frame body exceeding the negotiated cap is a
  `ProtocolError::FrameTooLarge`. Payloads larger than the negotiated cap are
  not sent as control frames — they move over the bulk plane instead (see
  [`plan/08-transport.md`](plan/08-transport.md)).

## 6. Handle layout

A `Handle` is a 64-bit value that names a server-side resource. Its layout
(most-significant bit first):

```
 bits 56..64   bits 32..56    bits 0..32
┌────────────┬──────────────┬───────────────┐
│  kind (8)  │  gen (24)    │  slot idx (32)│
└────────────┴──────────────┴───────────────┘
```

| Field        | Bits          | Width | Accessor              |
|--------------|---------------|------:|-----------------------|
| `kind`       | `56..64`      |     8 | `Handle::kind()`      |
| `generation` | `32..56`      |    24 | `Handle::generation()`|
| `slot`       | `0..32`       |    32 | `Handle::slot()`      |

Construct with `Handle::new(kind, generation, slot)`; the `generation` argument
is masked to 24 bits so the value always round-trips through
`Handle::generation()`. Convert to and from the wire value with `Handle::raw()`
and `Handle::from_raw()`. Relevant constants: `KIND_BITS = 8`,
`GENERATION_BITS = 24`, `SLOT_BITS = 32`, and `GENERATION_MAX = 2^24 - 1`.

The `generation` field guards against use-after-free of a reused slot: each time
a slot is reallocated its generation is bumped, so a stale handle carrying the
old generation no longer matches the slot. When a slot's generation would exceed
`GENERATION_MAX`, the slot is **retired** and never reused, so the generation
never wraps back to a value an old handle still holds. The broader handle-table
design is in [`plan/11-handles.md`](plan/11-handles.md).

## 7. Error model

Encoding and decoding raise `ProtocolError`:

| Variant                          | Raised when                                                   |
|----------------------------------|---------------------------------------------------------------|
| `UnexpectedEof`                  | The buffer ended before a full structure could be decoded.    |
| `UnknownOpcode(u32)`             | The decoded opcode maps to no known entrypoint.               |
| `BadKind(u16)`                   | The decoded frame kind is not `Request`/`Response`/`Event`.   |
| `BadVersion { major, minor }`    | The peer speaks an incompatible protocol major version.       |
| `FrameTooLarge(u32)`             | A frame body exceeded the negotiated maximum.                 |
| `BodyLenMismatch { declared, actual }` | The declared `body_len` did not match the bytes present. |
