# GraftX backend status matrix

This document tracks, per GPU API, how far each API has progressed through the
GraftX remoting pipeline: protocol codecs → server backend → client shim →
end-to-end test. It is a snapshot of the implementation, kept in step with the
code under [`../../crates/`](../../crates/); see
[`PROTOCOL.md`](PROTOCOL.md) for the wire format the codecs implement and
[`ROADMAP.md`](ROADMAP.md) for where each API sits on the milestone plan.

## Everything here is a remoting-path stub

> **All server backends are pure-Rust stubs. None of them calls a native GPU
> driver, links a vendor SDK, or touches a physical GPU.**

Each backend exercises the *remoting path*, not the GPU:

- It decodes the forwarded request body with [`graftx_protocol`](../../crates/graftx-protocol),
  **validates** it (the command stream is untrusted), and replies with a
  well-formed response frame.
- It tracks object **handle lifetimes** in generational handle tables
  (create mints a handle; destroy/free removes it; operations on an unknown or
  stale handle are rejected) — see [`../../crates/graftx-handles`](../../crates/graftx-handles).
- Calls it does not yet model are reported as
  [`ProtocolError::UnknownOpcode`](../../crates/graftx-protocol/src/lib.rs),
  not silently accepted.

The native driver bridge (real FFI into Vulkan/CUDA/etc.) lands in a later
milestone. What is validated today is the decode → validate → handle-lifecycle →
reply contract across the client shim, transport, protocol, and server session
state machine.

## Pipeline columns

- **ApiId** — the API-namespace byte occupying the high byte of every opcode,
  from the [`ApiId`](../../crates/graftx-protocol/src/lib.rs) enum.
- **Codecs** — request/response structs and opcode constants exist in
  [`graftx-protocol`](../../crates/graftx-protocol/src/lib.rs).
- **Server backend** — a `Backend` impl exists in
  [`graftx-server`](../../crates/graftx-server/src) and is re-exported from
  [`lib.rs`](../../crates/graftx-server/src/lib.rs).
- **Client shim** — an entry-point module exists in
  [`graftx-client`](../../crates/graftx-client/src).
- **E2E** — at least one client-shim ↔ server-backend round-trip test exists
  under [`graftx-server/tests/`](../../crates/graftx-server/tests), driven over
  the in-process loopback transport.

## Status matrix

| API        | ApiId  | Codecs | Server backend | Client shim | E2E | Notes |
|------------|:------:|:------:|----------------|-------------|-----|-------|
| Vulkan     | `0x01` | yes | [`backend.rs`](../../crates/graftx-server/src/backend.rs) (`VulkanBackend`) | [`vk.rs`](../../crates/graftx-client/src/vk.rs) | yes | Most-developed surface (instance/device/queue/memory/buffer/command pool). Covered by `e2e_vulkan`, `e2e_vulkan_memory`, `e2e_tcp`, and `e2e_multi`. |
| OpenGL     | `0x02` | yes | [`gl_backend.rs`](../../crates/graftx-server/src/gl_backend.rs) | [`gl.rs`](../../crates/graftx-client/src/gl.rs) | yes | Context / make-current / buffer-name lifecycle. Covered by `e2e_gl` and `e2e_multi`. |
| CUDA       | `0x03` | yes | [`cuda_backend.rs`](../../crates/graftx-server/src/cuda_backend.rs) | [`cuda.rs`](../../crates/graftx-client/src/cuda.rs) | yes | Context create + device-pointer alloc/free. Covered by `e2e_cuda` and `e2e_multi`. |
| OpenCL     | `0x04` | yes | [`cl_backend.rs`](../../crates/graftx-server/src/cl_backend.rs) | [`cl.rs`](../../crates/graftx-client/src/cl.rs) | yes | Context + buffer create/release. Covered by `e2e_cl`. |
| HIP        | `0x05` | yes | [`hip_backend.rs`](../../crates/graftx-server/src/hip_backend.rs) | [`hip.rs`](../../crates/graftx-client/src/hip.rs) | yes | Malloc/free + stream create. Covered by `e2e_hip`. |
| Level Zero | `0x06` | yes | [`l0_backend.rs`](../../crates/graftx-server/src/l0_backend.rs) | [`l0.rs`](../../crates/graftx-client/src/l0.rs) | yes | Context create + device-memory alloc/free. Covered by `e2e_l0`. |
| Video      | `0x07` | yes | [`video_backend.rs`](../../crates/graftx-server/src/video_backend.rs) | [`video.rs`](../../crates/graftx-client/src/video.rs) | yes | Decode-session create / decode-frame / destroy. Covered by `e2e_video`. |
| WebGPU     | `0x08` | yes | [`wgpu_backend.rs`](../../crates/graftx-server/src/wgpu_backend.rs) | [`wgpu.rs`](../../crates/graftx-client/src/wgpu.rs) | yes | Request-device + buffer create/destroy. Covered by `e2e_wgpu` and `e2e_multi`. |
| OptiX      | `0x09` | yes | [`optix_backend.rs`](../../crates/graftx-server/src/optix_backend.rs) (`OptixBackend`) | [`optix.rs`](../../crates/graftx-client/src/optix.rs) | yes | Context + pipeline create / destroy. Covered by `e2e_optix_sycl_amf`. |
| SYCL       | `0x0A` | yes | [`sycl_backend.rs`](../../crates/graftx-server/src/sycl_backend.rs) (`SyclBackend`) | [`sycl.rs`](../../crates/graftx-client/src/sycl.rs) | yes | Queue create + device malloc/free. Covered by `e2e_optix_sycl_amf`. |
| AMF        | `0x0B` | yes | [`amf_backend.rs`](../../crates/graftx-server/src/amf_backend.rs) (`AmfBackend`) | [`amf.rs`](../../crates/graftx-client/src/amf.rs) | yes | Encoder create / encode-frame (echoes packet_len) / destroy. Covered by `e2e_optix_sycl_amf`. |

The `Core` namespace (`0x00`) is not a GPU API: it carries the
`Hello`/`Welcome` handshake and `Noop`, handled inline by the session before
any backend is consulted (see
[`session.rs`](../../crates/graftx-server/src/session.rs)). The
handshake/no-op round-trip is validated by
[`e2e.rs`](../../crates/graftx-server/tests/e2e.rs).

## How the pieces connect

A request flows client shim → transport → server
[`Session`](../../crates/graftx-server/src/session.rs). The session validates
framing, handles `Core` opcodes inline, and routes every other opcode to the
backend registered for the opcode's `ApiId` namespace. An opcode whose
namespace has no registered backend is rejected with `UnknownOpcode` — all eleven GPU-API namespaces now have a stub backend (OptiX, SYCL, and AMF landed alongside the rest).

The `e2e_multi` test registers the Vulkan, OpenGL, CUDA, and WebGPU backends in
a single session to confirm namespace routing keeps independent backends
isolated within one connection.
