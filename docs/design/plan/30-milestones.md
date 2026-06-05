# 30. Detailed Milestone Plan (M0-M5)

A task-level decomposition of the six delivery milestones plus the two cross-cutting tracks, with explicit dependencies, acceptance criteria, and rough effort estimates so the project can be sequenced and progress can be measured.

This chapter turns the high-level roadmap (`docs/design/ROADMAP.md`) into an executable backlog. It does not re-derive the Architecture chapter (Ch. 04), the wire Protocol and Serialization chapters (Ch. 06-07), the Transport chapter (Ch. 08), the Client shim chapter (Ch. 09), the Server core chapter (Ch. 10), or the per-API backend chapters (the Handles through CUDA chapters, Ch. 11-16, and the Tier-2/Tier-3 chapters); instead it references them by number and states *what must be done, in what order, and how we know it is done*. All work is forward-looking from v0.0.0 — every milestone below is *planned*.

## 30.1 Scope, conventions, and how to read this chapter

The plan is organized as six sequential milestones (M0-M5) and two parallel tracks (Performance `P`, Security `S`). Milestones are gated: M(n+1) will not start its dependent tasks until the named M(n) acceptance criteria pass in CI. The tracks run *alongside* milestones — a small amount of P/S work lands every milestone rather than being deferred to the end, because retrofitting zero-copy and validation late is the single biggest risk identified in the Goals chapter (Ch. 02).

Conventions used throughout:

- **Task IDs** are `M<milestone>-T<n>` (e.g. `M1-T4`) and `P-T<n>` / `S-T<n>` for the tracks. They are referenced in commit footers (`Refs: M1-T4`) and issue titles.
- **Effort** is expressed in *ideal engineer-days* (`d`) for a single developer familiar with Rust and the relevant GPU API. These are planning estimates, not commitments; the breadth-first priority means many tasks are wide-but-shallow (lots of thin wrappers) rather than deep.
- **Dependency** lists the task IDs that must be *merged* before this task can be *completed* (it may often start earlier).
- **AC** = acceptance criteria: objective, CI-checkable where possible.

Effort summary (rough, planning-grade):

| Milestone | Theme | Tasks | Effort (ideal-days) |
|-----------|-------|-------|---------------------|
| M0 | Protocol + transport skeleton, no-op round-trip | 10 | ~30 |
| M1 | Vulkan (the spine) | 11 | ~70 |
| M2 | OpenGL / GLES / EGL / GLX | 9 | ~75 |
| M3 | CUDA + OpenCL | 8 | ~55 |
| M4 | Tier-2: HIP, Level Zero, video codecs | 7 | ~60 |
| M5 | Tier-3 reach APIs | 6 | ~70 |
| P  | Performance (zero-copy, batching, async) | 6 | ~40 |
| S  | Security (validation, sandbox, quotas) | 6 | ~45 |

The critical path is `M0 -> M1 -> {M2, M3} -> M4 -> M5`. M2 and M3 are independent once M1 lands and can be developed in parallel by separate contributors.

## 30.2 M0 — Protocol + transport skeleton and no-op round-trip

**Goal.** A Linux client shim can connect to a Windows server, complete a version handshake, send a `NoOp` command, and receive a matching reply — proving the full plumbing (framing, serialization, vsock control plane, ivshmem discovery, dispatch loop, and observability bring-up) end to end with zero real GPU work. M0 is the foundation gate: nothing else proceeds until a no-op round-trips.

```text
Linux guest (client)                         Windows guest (server)
  shim (graftx-client cdylib)                  graftx-server bin
        |  vsock connect (cid:port)                  |
        |------------------------------------------> | accept
        |  Hello{proto_major, proto_minor, features} |
        |------------------------------------------> |
        |  Welcome{proto_major, proto_minor,         |
        |    features, max_frame_body, session_id}   |
        | <------------------------------------------|
        |  Frame[NoOp]                               |
        |------------------------------------------> | decode -> dispatch -> reply
        |               Frame[NoOpReply{echo}]       |
        | <------------------------------------------|
```

### M0 task breakdown

| ID | Task | Depends | Effort |
|----|------|---------|--------|
| M0-T1 | Protocol frame body in `graftx-protocol`: the canonical `FrameHeader{version, flags, kind, opcode, req_id, seq, body_len}`, opcode enum stub (the Protocol chapter, Ch. 06) | — | 3d |
| M0-T2 | Serialization codec scaffold + `Encode`/`Decode` traits (the Serialization chapter, Ch. 07) | M0-T1 | 3d |
| M0-T3 | `Transport` trait + transport frame header `{magic, channel, len}` wrapping the opaque protocol body + in-process loopback impl for tests (the Transport chapter, Ch. 08) | — | 2d |
| M0-T4 | vsock control-plane transport (connect/listen, framed read/write) | M0-T3 | 4d |
| M0-T5 | ivshmem device discovery + ring/region mmap stub (no zero-copy yet) | M0-T3 | 4d |
| M0-T6 | Handshake state machine: `Hello`/`Welcome`, version + `max_frame_body` negotiation (the Protocol chapter, Ch. 06) | M0-T1,T4 | 3d |
| M0-T7 | Server dispatch loop skeleton: decode frame, match opcode, dispatch table (the Server core chapter, Ch. 10) | M0-T2,T6 | 3d |
| M0-T8 | Client send/recv path + `NoOp` shim entry point | M0-T2,T6 | 2d |
| M0-T9 | Observability bring-up: structured logging + span/trace scaffold on both ends, `req_id`/`session_id` correlation (the Observability chapter, Ch. 27) | M0-T6 | 2d |
| M0-T10 | End-to-end no-op integration test over loopback + over vsock | all above | 4d |

### Key M0 sketches

The frame layering and the dispatch skeleton are the load-bearing pieces. M0 does **not** declare its own header: it consumes the single canonical `FrameHeader` defined once in `graftx-protocol` (the Protocol chapter, Ch. 06) and the single transport header defined in `graftx-transport` (the Transport chapter, Ch. 08). The transport frame is a small `{magic, channel, len}` header wrapping an opaque body; that body is the protocol frame. `MAGIC` lives only in the transport header. These must be fixed early because every later milestone serializes into them:

```rust
// Defined once in graftx-protocol (Ch. 06) — referenced here, never redeclared.
// pub struct FrameHeader {
//     pub version: u16,    // proto_major/minor negotiated in Welcome
//     pub flags: u16,      // BULK_REF, RESPONSE_EXPECTED, ...
//     pub kind: u16,       // request / response / event
//     pub opcode: u32,     // (ApiId << 24) | call_id  — see Opcode below
//     pub req_id: u32,     // request/response correlation id (Ch. 06)
//     pub seq: u64,        // per-session monotonic ordering/fence seq (Ch. 13)
//     pub body_len: u32,   // bytes of payload following the header (<= max_frame_body)
// }

#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Opcode {
    NoOp = 0,
    // opcode = (ApiId << 24) | call_id; ApiId table owned by Ch. 06 + Ch. 32.
    // ... API opcode ranges: Vulkan 0x01_000000.., GL 0x02_000000.., CUDA 0x03_000000.., etc.
}
```

```rust
// graftx-server/src/dispatch.rs
pub trait Handler {
    fn handle(&mut self, hdr: &FrameHeader, payload: &[u8], out: &mut ReplyBuf)
        -> Result<(), ServerError>;
}

// M0 dispatch is a flat match; M1+ replaces with a generated table keyed by Opcode.
fn dispatch(hdr: &FrameHeader, payload: &[u8], out: &mut ReplyBuf) -> Result<(), ServerError> {
    match Opcode::try_from(hdr.opcode)? {
        // reply correlation uses req_id (Ch. 06); seq is ordering/fence (Ch. 13)
        Opcode::NoOp => { out.write_noop_reply(hdr.req_id); Ok(()) }
        other => Err(ServerError::UnknownOpcode(other as u32)),
    }
}
```

### M0 acceptance criteria

- `cargo test -p graftx-transport` passes a no-op round-trip over the loopback transport.
- A manual two-guest run (Linux client, Windows server) round-trips `NoOp` over vsock and the client observes the echoed `seq`.
- Handshake rejects a mismatched `proto_ver` with a typed error (no panic, no `unwrap` in lib paths — enforced by `clippy -D warnings` and a grep gate in CI).
- ivshmem region is discovered and mmapped on both ends (verified by a test that writes a sentinel from the server and reads it from the client); zero-copy *use* is deferred to track P.
- `rustfmt --check` and `clippy -D warnings` are green; CI runs on a Linux runner for the client/transport crates and a Windows runner for the server crate.

## 30.3 M1 — Vulkan forwarding (the spine)

**Goal.** Forward enough of Vulkan that a real headless compute and a simple triangle render run on the Windows GPU and return results to Linux. Chapter 14 (Vulkan backend) owns the API detail; this milestone sequences it. Vulkan is first because its explicit object model (instances, devices, queues, command buffers, explicit memory, explicit sync) maps most cleanly onto the protocol and forces the handle, memory, and sync subsystems (Ch. 11-13) to be built correctly. Everything after M1 reuses these subsystems.

### M1 task breakdown

| ID | Task | Depends | Effort |
|----|------|---------|--------|
| M1-T1 | Vulkan opcode block + command structs for instance/device/queue creation | M0 | 6d |
| M1-T2 | Loader shim `libvulkan.so.1`: export `vkGetInstanceProcAddr`, intercept dispatch (Ch. 9) | M0 | 6d |
| M1-T3 | Handle registry: map guest handles <-> server-native handles (Ch. 11) | M0-T7 | 6d |
| M1-T4 | Device memory: `vkAllocateMemory`/`vkMapMemory` over ivshmem-backed bulk (Ch. 12) | M1-T1,T3 | 8d |
| M1-T5 | Buffer/image creation + binding commands | M1-T3,T4 | 6d |
| M1-T6 | Command buffer recording: capture + replay `vkCmd*` stream (Ch. 13) | M1-T5 | 8d |
| M1-T7 | Queue submit + fences/semaphores; async submission hook (Ch. 13, track P) | M1-T6 | 8d |
| M1-T8 | Shader module + compute pipeline create/dispatch | M1-T5 | 5d |
| M1-T9 | Graphics pipeline + render pass + swapchain-to-readback (headless) | M1-T6 | 8d |
| M1-T10 | Validation pass on decoded Vulkan structs (track S, Ch. 10) | M1-T1 | 5d |
| M1-T11 | Conformance: run vkcube (offscreen) + a compute sample end to end | T1..T10 | 4d |

### M1 design notes and tradeoffs

The central decision in M1 is **command-buffer replay strategy**. Two options:

| Strategy | Pros | Cons | Decision |
|----------|------|------|----------|
| Per-`vkCmd*` forwarding (each command is its own frame) | Simple, streams immediately | One round-trip-ish cost per command; chatty | Rejected for hot path |
| Record client-side into a serialized command list, ship on `vkEndCommandBuffer` | Single bulk transfer; matches Vulkan's explicit recording model | Must buffer; secondary command buffers need care | **Chosen** |

Command buffer recording will batch `vkCmd*` calls into a contiguous encoded buffer placed in the ivshmem bulk region and reference it from a single `EndCommandBuffer` frame, which dovetails with track-P batching (P-T2). Sketch of the client-side recorder:

```rust
// graftx-client/src/vulkan/cmdbuf.rs
struct RecordedCmd { opcode: Opcode, args_off: u32, args_len: u32 }

struct CmdBufferRecorder {
    handle: GuestHandle,        // the vkCommandBuffer this records into
    cmds: Vec<RecordedCmd>,     // ordered command list
    arena: BulkArena,           // ivshmem-backed; holds serialized args
}

impl CmdBufferRecorder {
    // SAFETY: caller guarantees args point to valid Vulkan structs for `op`.
    unsafe fn push(&mut self, op: Opcode, args: &[u8]) {
        let (off, len) = self.arena.append(args);
        self.cmds.push(RecordedCmd { opcode: op, args_off: off, args_len: len });
    }
    fn flush_on_end(self) -> Frame { /* one EndCommandBuffer frame w/ bulk ref */ }
}
```

`pNext` chains and pointer-rich Vulkan structs are the chief serialization hazard; M1-T1 must define a deep-copy walker for `pNext` and reject unknown `sType` values (this is both correctness and a security concern, hence M1-T10 cross-links track S).

### M1 acceptance criteria

- A headless Vulkan compute sample (e.g. a saxpy compute shader) produces bit-identical results on the remoted path vs. a native run, verified by an in-repo test harness.
- `vkcube` rendered offscreen produces a frame that, read back to Linux, matches a golden image within a tolerance (allowing for driver-specific rasterization).
- Object lifetimes are correct: a torture test that creates/destroys 10k buffers shows zero handle leaks on the server (registry size returns to baseline).
- No `unwrap`/`expect` on the client or transport lib paths; all decode errors are `thiserror` variants.
- Validation rejects malformed structs (oversized counts, dangling `pNext`) without crashing the server (track S minimal bar met).

## 30.4 M2 — OpenGL / OpenGL ES / EGL / GLX

**Goal.** Forward the OpenGL family. Chapter 15 owns the backend; this milestone sequences it. GL is harder than Vulkan in two specific ways the tasks must address: (1) the **implicit, mutable global context state machine** (no explicit command buffers), and (2) the **window-system glue** (EGL/GLX) that binds GL to surfaces. The breadth-first priority means M2 targets a broad-but-not-exhaustive GL function set first (FBOs, VBOs, shaders, textures, draw calls) and fills the long tail later.

### M2 task breakdown

| ID | Task | Depends | Effort |
|----|------|---------|--------|
| M2-T1 | GL opcode block; generate thin wrappers from the GL XML registry (gl.xml) | M1 | 10d |
| M2-T2 | `libGL.so` / `libGLESv2.so` shim + `eglGetProcAddr`/`glXGetProcAddress` dispatch | M1-T2 | 8d |
| M2-T3 | EGL: display/config/context/surface create; bind to server offscreen surface (Ch. 15) | M2-T1,T2 | 8d |
| M2-T4 | GLX: equivalent context/drawable handling for X11 clients | M2-T2 | 8d |
| M2-T5 | Context state tracking: per-context current-binding + thread-affinity (`MakeCurrent`) | M2-T3 | 8d |
| M2-T6 | Buffer/texture/VAO upload over ivshmem bulk (Ch. 12) | M2-T1 | 8d |
| M2-T7 | Shader compile/link forwarding; uniform + draw-call path | M2-T6 | 8d |
| M2-T8 | Readback / present: blit FBO to a surface returned to the Linux compositor | M2-T5 | 9d |
| M2-T9 | Conformance: glmark2-style scenes + a GLES2 sample run offscreen | T1..T8 | 8d |

### M2 design notes

The defining problem is **`glGetError` and synchronous query semantics**. Many GL calls have no return value (fire-and-forget) but `glGetError`, `glGetIntegerv`, `glReadPixels`, `glMapBuffer`, etc. are synchronous and force a round-trip. The plan:

- Fire-and-forget calls (the overwhelming majority of draw/state calls) are *batched* into the bulk region and flushed lazily (a flush is triggered by any synchronous call, by a swap/present, or by a watermark). This is the GL analogue of M1's command-buffer batching and again cross-links track P.
- Synchronous calls flush the pending batch, then issue a round-trip. A decision table governs flush points:

| Call class | Examples | Behavior |
|------------|----------|----------|
| State / draw (void) | `glDrawArrays`, `glBindTexture`, `glUniform*` | append to batch, no round-trip |
| Error query | `glGetError` | flush, round-trip, return server error |
| Readback | `glReadPixels`, `glGetTexImage` | flush, round-trip, bulk-return pixels |
| Map | `glMapBufferRange` | flush, round-trip, alias ivshmem region |
| Sync object | `glFenceSync`/`glClientWaitSync` | flush, round-trip (Ch. 13) |
| Present | `eglSwapBuffers`/`glXSwapBuffers` | flush, present, optional vsync wait |

`glGetError` is special: forwarding every `glGetError` would destroy performance. The proposed mitigation is a **client-side error shadow** — the shim assumes `GL_NO_ERROR` for calls it can prove are valid client-side, and only round-trips when the application actually calls `glGetError` *and* the batch contained calls that could error. This is a correctness/perf tradeoff documented in Chapter 15.

```rust
// graftx-client/src/gl/context.rs
struct GlContext {
    server_ctx: GuestHandle,
    bound: BindingState,        // current VAO/program/textures/FBO (shadowed)
    pending: BulkBatch,         // unflushed void calls
    error_shadow: GlEnum,       // best-effort client-side error tracking
}
impl GlContext {
    fn flush(&mut self) -> Result<(), Error> { /* ship batch as one frame */ }
    fn get_error(&mut self) -> GlEnum { self.flush().ok(); /* round-trip */ ... }
}
```

### M2 acceptance criteria

- A GLES2 hello-triangle and a textured-quad sample render correctly offscreen and read back to a golden image within tolerance.
- A glmark2 subset runs to completion through EGL with no protocol errors; a separate run validates the GLX path under X11.
- `MakeCurrent` thread affinity is correct: a two-thread test with two contexts shows no cross-context state bleed on the server.
- The error-shadow optimization is proven safe by a negative test: a deliberately invalid call (e.g. bad enum to `glEnable`) is reported by the next `glGetError` with the correct GL error code.

## 30.5 M3 — CUDA + OpenCL compute

**Goal.** Forward the two principal compute APIs. Chapter 16 owns CUDA; OpenCL is its sibling here. M3 depends only on M1 (handles, memory, sync, async submission) and can run in parallel with M2. Compute is structurally simpler than graphics — no window system — but introduces **module/kernel loading** (cubin/PTX, SPIR-V/program binaries) and **stream/event ordering** as new concerns.

### M3 task breakdown

| ID | Task | Depends | Effort |
|----|------|---------|--------|
| M3-T1 | CUDA Driver API opcode block + `libcuda.so` shim (`cuGetProcAddress`) | M1 | 7d |
| M3-T2 | CUDA Runtime API shim (`libcudart.so`) layered on the driver path | M3-T1 | 6d |
| M3-T3 | Device memory + H2D/D2H copies over ivshmem bulk (Ch. 12) | M1-T4 | 6d |
| M3-T4 | Module load (cubin/PTX/fatbin) + kernel launch (`cuLaunchKernel`) | M3-T1 | 8d |
| M3-T5 | Streams + events: ordering, `cuStreamSynchronize`, async semantics (Ch. 13) | M3-T4 | 7d |
| M3-T6 | OpenCL opcode block + ICD shim; platform/device/context/queue | M1 | 8d |
| M3-T7 | OpenCL buffers, program build (source/SPIR-V), kernel enqueue | M3-T6 | 8d |
| M3-T8 | Conformance: vectorAdd (CUDA) + a CL kernel; numeric parity tests | T1..T7 | 5d |

### M3 design notes

The hardest correctness issue is **the CUDA fatbin / `__cudaRegisterFatBinary` mechanism** used by the Runtime API. Compiled CUDA programs embed device code in host binaries and register it at load time via constructor functions. The shim must intercept these registration calls (M3-T2) so the server can register the same fatbin against the native driver, and maintain a guest->server map of the opaque `__cudaFatCudaBinary` and kernel host-function-pointer stubs. Tradeoff table:

| Approach | Pros | Cons | Decision |
|----------|------|------|----------|
| Forward Runtime API directly | Less work | Runtime is a moving, partly-undocumented target | Secondary |
| Reimplement Runtime atop Driver API client-side | Stable, documented surface; one server path | Significant client-side logic to mirror runtime | **Primary for kernels** |

Pointer-as-handle is another subtlety: CUDA device pointers (`CUdeviceptr`) are integers in a server-side address space. They must be treated as opaque handles in the registry (Ch. 11) and never dereferenced client-side; the shim returns server-assigned values and validates them on the way back in (track S).

```rust
// graftx-client/src/cuda/module.rs
struct ModuleRegistry {
    fatbins: HashMap<GuestFatbinId, ServerModule>,
    kernels: HashMap<HostFnPtr, (GuestFatbinId, KernelName)>, // runtime-API stubs
}
// cuLaunchKernel args: opaque device ptrs + scalar params are serialized;
// each param's storage class is validated server-side before launch.
```

### M3 acceptance criteria

- CUDA `vectorAdd` and a small matrix-multiply produce numerically identical results vs. native (exact for integer kernels, within ULP tolerance for float).
- A Runtime-API program (using `<<<>>>` launch syntax, compiled with nvcc) runs unmodified through the shim.
- OpenCL `clEnqueueNDRangeKernel` with a SPIR-V program returns correct results; a source-build path also works.
- Stream ordering test: two dependent kernels on the same stream observe correct ordering; events across streams synchronize correctly.
- Device pointers are never dereferenced on the client (verified by an ASAN/Valgrind run of the shim).

## 30.6 M4 — Tier-2: HIP, Level Zero, video codecs

**Goal.** Forward the Tier-2 APIs. HIP and Level Zero leverage M3's compute machinery heavily; video codecs are a distinct surface (encode/decode session lifecycles, bitstream/frame bulk transfer). These have their own backend chapters; M4 sequences them.

### M4 task breakdown

| ID | Task | Depends | Effort |
|----|------|---------|--------|
| M4-T1 | HIP shim (`libamdhip64.so`); map HIP runtime onto compute machinery | M3 | 9d |
| M4-T2 | HIP module/kernel + hipMemcpy over bulk; stream/event parity | M4-T1 | 8d |
| M4-T3 | Level Zero opcode block + `libze_loader.so` shim; driver/device/context | M3 | 9d |
| M4-T4 | Level Zero command lists + immediate command lists; memory + events | M4-T3 | 9d |
| M4-T5 | Video decode session lifecycle (codec-agnostic frame); bitstream-in/frame-out bulk | M1 | 9d |
| M4-T6 | Video encode session lifecycle; frame-in/bitstream-out bulk | M4-T5 | 8d |
| M4-T7 | Conformance: HIP vectorAdd, L0 compute sample, decode+encode of a short clip | T1..T6 | 8d |

### M4 design notes

HIP's surface is deliberately close to CUDA's, so M4-T1/T2 can reuse much of M3's module/stream/event modeling with a renamed opcode block — the main risk is HIP's host-compilation model (HIP-Clang) and the fact that the server must own an AMD-capable native driver path distinct from the NVIDIA one. The server's backend registry (Ch. 10) selects the native vendor library at session init based on a negotiated capability flag.

Video codecs are the structurally novel part of M4. Encode/decode are long-lived stateful sessions with large, frequent bulk transfers, so they are the second-biggest consumer of zero-copy (track P) after rendering. The proposed model treats a codec session as a handle owning input and output bulk rings:

```text
decode:  [bitstream chunk in ivshmem] --frame--> server decoder --> [NV12/RGBA frame out]
encode:  [raw frame in ivshmem]       --frame--> server encoder --> [coded bitstream out]
```

Tradeoff: codec APIs are vendor-specific (NVDEC/NVENC, VA-API, AMF, Media Foundation). M4 will define a thin codec-agnostic protocol layer and a vendor adapter on the server; only one decode and one encode adapter need to pass M4-T7, with the rest queued for M5/backlog.

### M4 acceptance criteria

- HIP `vectorAdd` and a Level Zero compute sample produce results identical to native.
- A short clip decodes to frames that read back to Linux and match a reference within PSNR tolerance; re-encoding those frames produces a playable bitstream.
- Session leak test: opening/closing 100 codec sessions returns server resource usage to baseline.
- Vendor selection is correct: the same client binary drives an AMD path (HIP) and an NVIDIA path (CUDA) depending on server capability negotiation.

## 30.7 M5 — Tier-3 reach APIs

**Goal.** Forward the remaining reach APIs: WebGPU (Dawn/wgpu native), OptiX, AMF, SYCL, and any residual codec adapters. These are *reach* targets — broad coverage over depth — and each largely composes prior machinery (WebGPU and SYCL ultimately sit on Vulkan/L0/compute; OptiX on CUDA; AMF on the codec layer).

### M5 task breakdown

| ID | Task | Depends | Effort |
|----|------|---------|--------|
| M5-T1 | WebGPU native (`webgpu.h` / wgpu-native) shim; map onto Vulkan-style command model | M1, M2 | 14d |
| M5-T2 | OptiX shim; map onto CUDA module/launch + acceleration-structure builds | M3 | 14d |
| M5-T3 | AMF shim; map onto the M4 codec session model + AMF component graph | M4-T6 | 12d |
| M5-T4 | SYCL: forward via the chosen backend (DPC++ -> L0, or via OpenCL) | M3, M4-T4 | 12d |
| M5-T5 | Residual codec vendor adapters (VA-API / Media Foundation gaps) | M4-T7 | 8d |
| M5-T6 | Reach conformance suite: one minimal working sample per API | T1..T5 | 10d |

### M5 design notes

M5's risk is breadth without regression. Rather than deep conformance, each API needs a single end-to-end "hello" sample plus a documented coverage matrix in `docs/FEATURES.md` marking which entry points are wired vs. stubbed. Stubs must fail loudly with a typed `NotImplemented` error carrying the opcode name, never silently no-op — silent no-ops corrupt application state and are explicitly banned.

```rust
// graftx-server/src/dispatch.rs
fn dispatch_reach(op: Opcode, ...) -> Result<(), ServerError> {
    match op {
        // wired
        Opcode::WgpuQueueSubmit => webgpu::queue_submit(...),
        // explicitly unimplemented — loud, typed, never a no-op
        other => Err(ServerError::NotImplemented { opcode: other.name() }),
    }
}
```

WebGPU (M5-T1) is the most valuable Tier-3 target and is sequenced first because its command-encoder model maps almost directly onto M1's command-buffer batching, giving it good performance for low incremental cost.

### M5 acceptance criteria

- Each Tier-3 API has at least one sample running end to end (WebGPU compute, OptiX ray-cast triangle, AMF encode, SYCL vector add).
- `docs/FEATURES.md` coverage matrix is updated; CI fails if a dispatched opcode is marked "wired" but routes to `NotImplemented`.
- No silent no-ops: a fuzz of unimplemented opcodes returns `NotImplemented` with the correct name and does not crash the server.

## 30.8 Cross-cutting track P — Performance

Performance is a *track*, not a milestone: a slice lands each milestone so the hot paths are designed for zero-copy/batching from day one (Chapters 12-13). The full optimization push concentrates after M1 establishes the spine.

| ID | Task | Lands with | Effort |
|----|------|-----------|--------|
| P-T1 | ivshmem bulk ring allocator + region lifecycle (Ch. 12) | M0/M1 | 8d |
| P-T2 | Command batching: coalesce calls into one frame + one bulk transfer | M1, reused M2/M5 | 6d |
| P-T3 | Async submission + reply futures; decouple submit from completion (Ch. 13) | M1 | 8d |
| P-T4 | Zero-copy mapped memory: alias ivshmem region into guest address space | M1-T4, M2-T6 | 8d |
| P-T5 | Backpressure + flow control on the bulk ring (also serves track S quotas) | M3 | 5d |
| P-T6 | Benchmark harness + regression gate (per-call latency, throughput) | M1 | 5d |

P-T6 establishes a CI benchmark that records p50/p99 per-call overhead and bulk throughput; a regression beyond a threshold (e.g. +15% latency) fails the build. The headline metric is *added round-trip latency per forwarded call* and *effective bulk bandwidth as a fraction of raw ivshmem bandwidth*.

## 30.9 Cross-cutting track S — Security

The server replays an *untrusted* command stream against native drivers (Chapter 10), so validation cannot be bolted on at the end. A minimal validation bar ships in M1 (M1-T10); the track hardens it over time.

| ID | Task | Lands with | Effort |
|----|------|-----------|--------|
| S-T1 | Decode-time validation framework: bounds, counts, enum ranges, `pNext` walk | M1 | 8d |
| S-T2 | Bulk copy-in: copy validated data into server-private memory before use | M1, M3 | 7d |
| S-T3 | Handle ownership enforcement: per-session handle table, no cross-session access | M1-T3 | 6d |
| S-T4 | Resource quotas + limits (memory, handles, sessions) with backpressure (uses P-T5) | M3 | 7d |
| S-T5 | Server sandbox profile (least privilege, restricted syscalls/job object) | M4 | 9d |
| S-T6 | Fuzzing harness over the decode path; CI fuzz smoke run | M2 | 8d |

Key principle restated from the project charter: the paired-guest channel is *reachability scoping, not authentication*; per-session auth/integrity is planned but lives in the backlog beyond M5. S-T2 is non-negotiable on the bulk path — because write-revocation can only be enforced at the hypervisor/ivshmem-device layer, the server must defensively copy validated bytes out of shared memory into server-private buffers before any driver call touches them, so a malicious guest cannot mutate data after validation but before use (a TOCTOU defense).

## 30.10 Sequencing summary and risk gates

```text
      track P (perf): P-T1 ... P-T6  ───────────────────────────────►
      track S (sec):  S-T1 ... S-T6  ───────────────────────────────►
 time ─────────────────────────────────────────────────────────────►
  M0 ──► M1 ──┬──► M2 ─────────────────┐
              └──► M3 ──► M4 ──────────┴──► M5
```

Hard gates (CI-enforced before the next milestone's dependent tasks merge):

1. **M0 gate:** no-op round-trips over vsock; ivshmem region mapped both ends.
2. **M1 gate:** Vulkan compute + offscreen render match golden outputs; handle torture test leak-free; S-T1 minimal validation present.
3. **M2/M3 gate (independent):** respective conformance samples pass; M2 error-shadow negative test passes; M3 numeric parity holds.
4. **M4 gate:** HIP + L0 parity; one decode + one encode adapter pass; session leak test clean.
5. **M5 gate:** one sample per reach API; no opcode marked "wired" routes to `NotImplemented`.

Top risks and the tasks that retire them: (a) Vulkan `pNext`/pointer serialization — M1-T1, S-T1; (b) GL implicit-state correctness — M2-T5, error-shadow negative test; (c) CUDA fatbin registration — M3-T2; (d) zero-copy TOCTOU safety — S-T2; (e) performance regressions creeping in unnoticed — P-T6's CI gate. Each risk is owned by a named task so it cannot be silently dropped.
