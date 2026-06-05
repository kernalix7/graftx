# 15. OpenGL / GLES / EGL / GLX Backend

This chapter specifies how GraftX will forward the OpenGL / OpenGL ES state machine, manage contexts through EGL and GLX, choose between native server-side GL and a Zink-to-Vulkan funnel, and handle buffers, textures, GLSL, readback, and display integration.

## 15.1 Scope and shape of the problem

OpenGL is the hardest API family in the GraftX matrix because it is the largest *implicit state machine* we will remote. Unlike Vulkan (the Vulkan chapter, Ch.14) — where almost all state is bundled into explicit objects that we can serialize once and replay — classic GL keeps thousands of enables, bindings, matrix-stack remnants, blend equations, and per-texture sampler parameters in a per-context server-side object. Every entry point reads or mutates that hidden state. Naively forwarding each of the ~3000 GL entry points one-for-one over vsock would be correct but unusably slow: a frame can issue 50k+ calls.

The backend therefore has two layers:

1. **Client shims** (in `graftx-client`) that export the C ABI symbols `gl*`, `glX*`, `egl*` and either record commands into a per-context batch or answer locally from a mirrored cache (see the client-shim chapter (Ch.09) for symbol-export mechanics and the client-shim chapter (Ch.09) plus the sync chapter (Ch.13) for command batching).
2. **Server replay** (in `graftx-server`) that decodes the batch, validates it (the security chapter, Ch.23), and drives either the Windows native GL ICD (via WGL) or a Zink/Vulkan path.

```
Linux guest                              Windows guest
+-----------------------------+          +-----------------------------+
| app -> libGL.so (our shim)  |          | graftx-server               |
|   record + local cache      | vsock/   |   decode + validate         |
|   |                         | ivshmem  |   |                         |
|   +-- batch encoder --------->-------->-+--> GL replay context        |
|        bulk uploads (shm) -->|          |     (WGL native) OR         |
| <-- readback / fences -------<----------+     (Zink -> Vulkan Ch.14)  |
+-----------------------------+          +-----------------------------+
```

We deliberately target **GL up to 4.6 core + compatibility, GLES 2.0/3.0/3.1/3.2**, plus the EGL 1.5 and GLX 1.4 windowing layers. Coverage breadth is priority #1 (per project priorities), so the shim will advertise a generous extension string and fail gracefully (the error/FFI chapter, Ch.24) rather than crash on an unsupported call.

## 15.2 Context model and the client-side state mirror

A GL context is the unit of remoting. The shim assigns each EGL/GLX context a `RemoteCtxId(u32)` and tracks it in a thread-local "current context" slot, mirroring the GL rule that exactly one context is current per thread.

```rust
pub struct RemoteCtx {
    id: RemoteCtxId,
    server_handle: u64,          // opaque handle on the replay side
    api: ClientApi,              // OpenGL | OpenGLES
    version: (u8, u8),           // e.g. (4,6) or (3,2)
    profile: Profile,            // Core | Compatibility
    share_group: Option<RemoteCtxId>,
    mirror: StateMirror,         // client-side shadow of queryable state
    objects: ObjectTable,        // name -> ObjectInfo
}

#[derive(Default)]
pub struct StateMirror {
    // Values we can answer WITHOUT a server round-trip.
    enables: BitSet,             // GL_BLEND, GL_DEPTH_TEST, ...
    bound: [u32; BIND_SLOTS],    // current bindings per target
    pixel_store: PixelStoreState,// UNPACK/PACK_ALIGNMENT etc.
    viewport: [i32; 4],
    error_queue: VecDeque<u32>,  // synthesized GL errors
    limits: GlLimits,            // GL_MAX_*; fetched once at ctx create
}
```

The mirror is the single most important performance decision in this chapter. `glGet*`, `glIsEnabled`, `glGetIntegerv(GL_MAX_TEXTURE_SIZE)` and friends are extremely common and are *blocking* in semantics — an app calls them and immediately branches on the result. If every such query went to the server we would serialize the pipeline on each one. Instead:

- **Static limits** (`GL_MAX_*`, `GL_NUM_EXTENSIONS`, vendor/renderer strings) are fetched **once** at context creation in a single round-trip and cached forever.
- **Settable scalar/enum state** (enables, blend func, depth func, viewport, pixel-store) is mirrored as it is set, so queries are answered from `mirror`.
- **State we cannot cheaply mirror** (e.g. `GL_TIMESTAMP`, occlusion query results, `glGetTexImage` for a texture the app may have re-uploaded) forces a *flush + round-trip*.

Decision table for `glGet`-class calls:

| Query | Source | Round-trip? |
|-------|--------|-------------|
| `GL_MAX_*`, strings, `GL_EXTENSIONS` | cached limits | No |
| enables / blend / depth / viewport | `StateMirror` | No |
| `glGetError` | synthesized queue + lazy server check | Conditional |
| query object results (`GL_QUERY_RESULT`) | server | Yes (with wait) |
| `glGetTexImage`, `glReadPixels` | server (PBO-aware, §15.7) | Yes unless async PBO |
| program/shader logs | server | Yes (rare, fine) |

`glGetError` deserves care: GL requires it return errors *this context generated*. Because the server replays asynchronously, the shim synthesizes errors for conditions it can detect locally (bad enum, unbound object) and pushes them onto `error_queue`. For server-detected errors, the batch reply carries an "error generated at command index N" tag; the shim splices those into the queue at flush time. Apps that poll `glGetError` every call (common in debug builds) will see slightly delayed-but-correct errors; we document this as a known relaxation.

## 15.3 EGL and GLX: window and surface management

EGL and GLX are the connective tissue between GL and the Linux windowing system (X11 / Wayland / surfaceless). The server lives on Windows and has *no* X display, so the shim must translate Linux surfaces into something the server can render into and the client can present locally.

Two surface strategies:

```
Strategy A (offscreen + blit-back, default):
  server renders to a server-side FBO / pbuffer
  -> contents copied over ivshmem -> client presents via DRM/KMS or
     X SHM / Wayland dmabuf  (display integration, §15.8)

Strategy B (passthrough scanout, opt-in):
  server presents directly on the Windows-attached display
  (used when the user wants output on the GPU's physical monitor)
```

`eglCreateWindowSurface(dpy, config, native_window, attrs)` is intercepted; the shim records the surface geometry and pixel format and creates a *paired* server render target, returning a synthetic `EGLSurface` handle. The actual `native_window` (an X11 `Window` or `wl_surface`) is kept on the client for the presentation step and is never sent to the server.

```rust
pub enum SurfaceKind {
    Window { native: NativeWindowId, w: u32, h: u32, fmt: PixelFormat },
    Pbuffer { w: u32, h: u32, fmt: PixelFormat },
    Surfaceless,
}

fn egl_create_window_surface(/* ... */) -> EglSurface {
    let kind = SurfaceKind::Window { /* parse attrs */ };
    let server = rpc_create_surface(&kind);     // 1 round-trip at create time
    register_surface(server, kind)
}
```

`eglSwapBuffers` / `glXSwapBuffers` is the frame boundary. It will: (1) flush the current command batch, (2) instruct the server to resolve the default framebuffer into a transferable image, (3) DMA the pixels over ivshmem, (4) hand them to the presentation backend (§15.8), and (5) honor swap interval / vsync semantics. Crucially the swap is the one mandatory synchronization point per frame; everything between swaps streams asynchronously.

Config selection (`eglChooseConfig`, `glXChooseFBConfig`) is resolved by the **server** at startup: the server enumerates the real GPU's configs, the client caches the list, and `eglChooseConfig` filters that cached list locally with the standard sorting rules — zero round-trips per call.

GLX mirrors EGL with `glXCreateContextAttribsARB`, `glXMakeContextCurrent`, `glXSwapBuffers`. The shim implements GLX over the same `RemoteCtx` machinery; the only divergence is the indirect-GLX protocol, which we will *not* support (we own `libGL.so`, so all GLX is "direct" from the app's view).

## 15.4 Native forward vs Zink-to-Vulkan: the backend choice

A key architectural decision is *how the server actually executes GL*. Two server-side execution backends are proposed, selectable per session via config and capability negotiation (the protocol/handshake chapter, Ch.06):

**Backend N — Native WGL forward.** The server creates a real WGL context against the Windows GPU vendor ICD and replays GL calls 1:1.

**Backend Z — Zink funnel.** The server runs the decoded GL stream through [Zink](https://docs.mesa3d.org/drivers/zink.html) (Mesa's GL-on-Vulkan driver) targeting the Vulkan ICD, sharing the Vulkan device/queue infrastructure already built for the Vulkan chapter (Ch.14).

| Aspect | Native (N) | Zink (Z) |
|--------|-----------|----------|
| GL conformance | highest (vendor driver) | very good, lags slightly |
| Reuse of Vulkan transport/sync (Ch.14) | none | full |
| Interop with our Vulkan backend (shared images) | hard | natural (same `VkDevice`) |
| Driver bugs surface | vendor GL stack | Vulkan stack only |
| Compute / GLES edge features | depends on vendor | uniform via Vulkan |
| Extra translation cost | none | GL->NIR->SPIR-V, pipeline cache |

The **proposed default is Backend N** for breadth/conformance, with **Backend Z available** when a session also uses Vulkan and wants a single device to share textures between GL and Vulkan without a copy (e.g. a compositor using GL for UI and Vulkan for the scene). The two backends sit behind one trait:

```rust
pub trait GlExecutor: Send {
    fn create_context(&mut self, desc: &CtxDesc) -> Result<ServerCtx, GlError>;
    fn make_current(&mut self, ctx: ServerCtx, draw: ServerSurface, read: ServerSurface)
        -> Result<(), GlError>;
    fn replay(&mut self, ctx: ServerCtx, cmds: &CmdBatch) -> ReplayOutcome;
    fn resolve_default_fb(&mut self, surf: ServerSurface) -> Result<ImageView, GlError>;
    fn flush_fence(&mut self, ctx: ServerCtx) -> FenceId;
}
```

`ReplayOutcome` carries the per-command error tags (§15.2), any synchronous reply values, and a fence for ordering. Selecting N vs Z only swaps the trait object; the decode/validate front-end is identical.

## 15.5 Command batching and the replay loop

The shim records calls into a `CmdBatch` — a length-prefixed, little-endian byte stream of `(opcode: u32, args...)` (wire format in the protocol chapter (Ch.06), argument serialization in the serialization chapter (Ch.07)). Most GL functions are *fire-and-forget*: `glDrawArrays`, `glUniform4f`, `glBindTexture`. These append to the batch and return immediately. Only the categories below force a flush:

1. Explicit sync: `glFinish`, `glClientWaitSync`, `glFenceSync` followed by a wait.
2. Synchronous readback without a PBO (§15.7).
3. Mapping a buffer for read (`glMapBufferRange` with `GL_MAP_READ_BIT`).
4. Frame boundary (`SwapBuffers`).
5. Query result fetch.

Server-side replay sketch:

```rust
fn replay(&mut self, ctx: ServerCtx, batch: &CmdBatch) -> ReplayOutcome {
    let mut cur = self.activate(ctx);
    let mut out = ReplayOutcome::default();
    let mut decoder = Decoder::new(batch.bytes());
    while let Some(op) = decoder.next_op() {
        match op {
            Op::DrawArrays { mode, first, count } => {
                if let Err(e) = validate_draw(&cur, mode, first, count) {
                    out.tag_error(decoder.index(), e); continue; // skip, keep going
                }
                unsafe { gl::DrawArrays(mode, first, count); } // SAFETY: validated above
            }
            Op::BufferData { target, size, src } => {
                let data = self.bulk.checked_slice(src, size)?; // copy out of shm (the memory chapter, Ch.12)
                unsafe { gl::BufferData(target, size, data.as_ptr().cast(), /*usage*/ ); }
            }
            // ... ~3000 opcodes, generated from the GL XML registry (§15.10)
        }
    }
    out.fence = self.flush_fence(ctx);
    out
}
```

Validation never `unwrap`s and never trusts a pointer from the stream; bulk data is *copied* out of ivshmem into server-private memory before the driver sees it (per the security model — write-revocation is only enforceable at the hypervisor layer). A bad command is tagged and skipped rather than aborting the batch, preserving as much of the frame as possible.

## 15.6 Buffer and texture uploads (the bulk path)

Large data — `glBufferData`, `glBufferSubData`, `glTexImage2D/3D`, `glTexSubImage*`, `glCompressedTexImage*` — must not travel inline in the vsock control stream. The shim routes any payload above the inline-vs-bulk threshold (default 4 KiB, owned by the memory chapter (Ch.12)) to the **ivshmem bulk plane** (the transport chapter, Ch.08): it writes the bytes into a shared-memory ring slot and records only a `ShmSlice { offset, len, gen }` in the command. The server validates the ref's bounds against the current ring generation, copies the bytes into private memory, then calls the driver.

Pixel-store state (`GL_UNPACK_ALIGNMENT`, `GL_UNPACK_ROW_LENGTH`, etc.) is mirrored client-side so the shim can compute the exact source-image size and pass a tight, validated length — the server independently recomputes the expected size from the mirrored pixel-store ops in the same batch and rejects mismatches (defense against a malicious client over-reading).

```rust
fn gl_tex_sub_image_2d(target: u32, level: i32, /*...*/ format: u32, ty: u32, pixels: *const c_void) {
    let bytes = pixel_image_size(&mirror.pixel_store, width, height, format, ty);
    if pixels.is_null() {                 // bound to a PBO -> server-side copy
        record(Op::TexSubImage2DFromPbo { /*...*/ offset: pixels as usize });
    } else if bytes <= INLINE_MAX {
        record(Op::TexSubImage2DInline { /*...*/ data: copy_inline(pixels, bytes) });
    } else {
        let bref = bulk_write(pixels, bytes); // SAFETY: bytes computed from validated state
        record(Op::TexSubImage2DBulk { /*...*/ data: bref });
    }
}
```

For *streaming* updates (per-frame dynamic VBOs) the bulk ring amortizes allocation; backpressure (the transport chapter, Ch.08) stalls the shim when the ring is full rather than dropping data. Persistent-mapped buffers (`GL_MAP_PERSISTENT_BIT` / `GL_MAP_COHERENT_BIT`) are the worst case: the app writes directly into a mapped region and expects coherence. The plan is to back a persistent mapping with a dedicated ivshmem region and let the server poll/flush it at draw/flush boundaries; true coherent semantics are relaxed to "coherent at flush", documented as a limitation.

## 15.7 Pixel readback

`glReadPixels`, `glGetTexImage`, and `glReadnPixels` move data the *wrong* way (server -> client), which is inherently latency-bound. Strategy:

- **Synchronous readback (no PBO bound):** flush the batch, server renders, resolves, DMAs the result into a readback ring slot, signals; the shim blocks, copies into the app's buffer, and returns. One full round-trip — unavoidable, matches GL semantics.
- **Asynchronous readback (PBO bound):** `glReadPixels` into a bound `GL_PIXEL_PACK_BUFFER` records a server-side read into the PBO and does **not** stall. The transfer to the client happens only when the app later `glMapBufferRange`s that PBO — at which point we DMA the now-complete data. This is the high-throughput path (screen capture, GPGPU result fetch) and we will steer toolkits toward it via documentation.

```rust
fn gl_read_pixels(x,y,w,h, fmt,ty, dst: *mut c_void) {
    if mirror.bound[PACK_PBO] != 0 {
        record(Op::ReadPixelsToPbo { x,y,w,h,fmt,ty, pbo: mirror.bound[PACK_PBO] });
        return; // async, no stall
    }
    flush_batch();
    let n = pixel_image_size(&mirror.pixel_store, w,h, fmt,ty);
    let slot = rpc_read_pixels_sync(x,y,w,h,fmt,ty); // blocks
    unsafe { copy_from_readback(slot, dst, n); }     // SAFETY: n validated, slot bounds checked
}
```

## 15.8 GLSL handling

Shaders are *source strings*. `glShaderSource` records the source into the batch (bulk path if large); `glCompileShader`, `glLinkProgram` execute server-side against the chosen backend. The decision is **where compilation happens**, not whether we translate:

- **Backend N:** source is handed verbatim to the vendor GL compiler. The compile/link logs come back lazily (queried only via `glGetShaderInfoLog`).
- **Backend Z:** Zink/Mesa compiles GLSL -> NIR -> SPIR-V internally; we do not pre-translate. We *will* maintain a SPIR-V/pipeline cache keyed by `hash(source + specialization)` so repeated compiles across runs are fast.

Because compile/link is rare relative to draws, we accept a round-trip at first link but pipeline against subsequent calls. `glGetProgramBinary` / `glProgramBinary` are forwarded but the binary blob is *server-specific*; the shim tags blobs with a server-identity cookie and refuses a `glProgramBinary` whose cookie does not match the current server, falling back to recompilation (matches `GL_PROGRAM_BINARY_RETRIEVABLE_HINT` semantics where drivers may reject binaries). `glSpecializeShader` (SPIR-V GL path) is forwarded as a bulk SPIR-V upload.

## 15.9 Synchronization and fences

GL sync objects (`glFenceSync`/`glClientWaitSync`) and implicit ordering map onto the server `FenceId` returned by each `replay`. `glClientWaitSync(GL_SYNC_FLUSH_COMMANDS_BIT)` flushes the batch then waits on that fence over the control plane. `glWaitSync` (GPU-side wait) records an op so the server inserts a server-side wait without stalling the client. Cross-context sharing within a share group is handled server-side: shared objects live in one server share group, so a fence in context A is visible to context B with no extra protocol.

## 15.10 Code generation, testing, and tradeoffs

The ~3000 entry points will be **generated** from the Khronos `gl.xml` / `egl.xml` / `glx.xml` registries by a `build.rs`/xtask that emits: (a) the C-ABI shim stubs, (b) the opcode enum + encoder, (c) the server decoder match arms, and (d) the validation hooks. Hand-writing each is infeasible and error-prone; generation keeps client and server wire-compatible by construction (the protocol chapter (Ch.06) owns the opcode-stability contract). Hot paths (`glDraw*`, `glUniform*`, `glBindTexture`, `glVertexAttribPointer`) get hand-tuned encoders; the long tail uses the generic generated path.

Testing: replay-trace capture (record a real app's `CmdBatch`, replay offline against both backends, diff framebuffers via SSIM); piglit/dEQP-GLES conformance run through the shim against a known-good local GL to catch divergence; fuzzing the decoder with malformed batches to prove no `unwrap`/UB (the testing chapter, Ch.26).

Key tradeoffs accepted: relaxed `glGetError` timing; "coherent at flush" for persistent maps; no indirect GLX; readback latency unless PBOs are used. Each is documented as a known limitation rather than a correctness bug, consistent with breadth-over-perfection priority. Backend Z is the strategic bet for GL/Vulkan interop; Backend N is the conformance safety net, and both ship behind one `GlExecutor` trait so a session can pick per workload.
