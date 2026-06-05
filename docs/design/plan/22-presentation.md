# 22. Presentation, Surfaces & Display Path

How a frame rendered on the Windows-side GPU will travel back to the Linux guest and become visible — swapchain image lifetimes, the present-to-Linux-window strategies GraftX will offer, compositor integration, the contrast with a Looking Glass framebuffer relay, and where the latency goes.

Every other backend chapter (15 OpenGL, 16 Vulkan, 17 CUDA/compute) ends at the moment the GPU has produced pixels. This chapter is about the *return leg*: getting those pixels from server-private GPU memory on Windows back to a window the Linux user actually sees. It is the one data flow in GraftX where the bulk plane runs **host → guest** at sustained high bandwidth, frame after frame, and where human-perceptible latency is decided. The control-plane present *call* (`eglSwapBuffers`, `vkQueuePresentKHR`, `glXSwapBuffers`) is trivial to forward; the hard part is the pixel pipeline behind it.

## 22.1 The shape of the problem

Local rendering ends in a *zero-copy scanout*: the compositor or DRM hands the GPU a buffer, the GPU renders into it, and the display controller scans it out of the same memory. GraftX cannot do that — the pixels are produced in a *different physical GPU's* VRAM, owned by the Windows guest, and the Linux display controller cannot scan out of it. So every presented frame must cross the guest boundary as data.

```
 LINUX GUEST                           WINDOWS GUEST (owns GPU)
 ┌──────────────┐  present cmd (vsock) ┌────────────────────────┐
 │ app: Swap()  │ ───────────────────► │ server: replay present │
 │ client shim  │                      │  swapchain img N ready  │
 └──────┬───────┘                      └───────────┬────────────┘
        │                                          │ GPU copy/blit
        │            frame bytes (ivshmem)         ▼ to readback
        │ ◄─────────────────────────────  staging surface in shm
        ▼
 ┌──────────────┐
 │ presenter:   │  import as dmabuf / texture / SHM buffer
 │ → compositor │  attach to wl_surface / X window / GBM scanout
 └──────────────┘
```

The defining constraints:

- **One real swapchain exists, on the server.** The client holds a *proxy swapchain* whose images are placeholders (see the Handles chapter (Ch. 11) for handle proxying). The application's `vkAcquireNextImageKHR` / GL default framebuffer must be answered without a real image existing locally.
- **The transport is the bulk plane** (the Memory chapter (Ch. 12)): ivshmem shared memory for the pixel payload, vsock for the present command and acquire/release signalling. Write-revocation and the mandatory server-side validation copy from the Memory chapter (Ch. 12) apply here too — but in the *reverse* direction (server writes shm, client reads), which changes the trust calculus (22.6).
- **Cadence is fixed by the display, not the workload.** A 60 Hz output gives a 16.67 ms budget per frame for the *entire* round trip plus the copy-back plus compositor handoff. At 4K RGBA8 that is 33.2 MiB/frame, ~2.0 GiB/s sustained at 60 Hz — comfortably inside ivshmem BAR bandwidth but not free.

## 22.2 The proxy swapchain

GraftX will model presentation uniformly across APIs with a single server-side abstraction, fronted by per-API shims. The unifying object:

```rust
pub struct PresentSurface {
    pub id: SurfaceId,
    pub width: u32,
    pub height: u32,
    pub format: PresentFormat,     // BGRA8_UNORM, RGBA8, RGB10A2, etc.
    pub colorspace: ColorSpace,    // sRGB nonlinear, scRGB, HDR10
    pub mode: PresentMode,         // Fifo, FifoRelaxed, Mailbox, Immediate
    pub images: SmallVec<[ServerImage; 4]>, // real server-side images
    pub ring: ShmFrameRing,        // host→guest staging ring in ivshmem
}

pub enum PresentMode { Fifo, FifoRelaxed, Mailbox, Immediate }

pub trait Presenter: Send {
    /// Server-side: GPU produced image `idx`; copy it into the shm ring slot.
    fn capture(&mut self, surf: &PresentSurface, idx: u32) -> Result<FrameToken, PresentError>;
    /// Client-side: a FrameToken arrived; turn the shm slot into something
    /// the Linux compositor can show. Returns when the slot may be reused.
    fn display(&mut self, tok: FrameToken) -> Result<(), PresentError>;
}
```

The `ShmFrameRing` is a small ring of pre-registered shm regions (typically 3, matching a triple-buffered swapchain) carved from the bulk allocator (the Memory chapter (Ch. 12)). Each slot is exactly `aligned_stride(width, format) * height` bytes plus a header. Pre-registration avoids per-frame allocator traffic — the hottest path in the system must not touch the allocator lock.

```rust
#[repr(C)]
pub struct ShmFrameSlot {
    pub seq: AtomicU64,    // monotonic frame number; odd = server writing
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
    pub flags: u32,        // DAMAGE_FULL, HDR, Y_FLIP
    pub damage_off: u32,   // offset to optional damage-rect list
    pub _pad: u32,
    // pixel data follows, `stride*height` bytes
}
```

`seq` doubles as a seqlock: the server bumps it to an odd value before writing, to an even value after. The client reads it twice around its copy/import; a mismatch means the server raced and the client retries with the next slot. This avoids a hard lock on the per-frame path while keeping the read self-consistent.

## 22.3 Server-side capture: getting pixels out of GPU memory

When the server replays a present, the rendered image is in GPU-local VRAM, not in the ivshmem BAR. It must be copied. There are three capture strategies, chosen per session at negotiation time (the Protocol chapter (Ch. 6)) and per GPU vendor:

| Strategy | Mechanism (Windows server) | Copies | Notes |
|----------|---------------------------|--------|-------|
| **GPU→staging→ivshmem** | Vulkan `vkCmdCopyImageToBuffer` into a `HOST_VISIBLE` buffer, then `memcpy` into shm | 2 | **Default**: universal, always correct |
| **Direct readback into shm-backed buffer** | Allocate the readback buffer *inside* the ivshmem-mapped region so the GPU DMAs straight into shm | 1 | Opportunistic optimization, probed at negotiation; needs ivshmem BAR mappable as a Vulkan external memory host pointer |
| **DXGI Desktop Duplication / shared NT handle** | For native D3D/DXGI swapchains, open the backbuffer as a shared resource and copy out | 2 | Used when the app is a native D3D path funneled through GraftX |

The documented default is the always-correct two-copy **GPU→staging→ivshmem** path. **Direct readback** is an opportunistic optimization, probed at negotiation time: where the platform allows mapping the ivshmem BAR as `VK_EXTERNAL_MEMORY_HANDLE_TYPE_HOST_ALLOCATION_BIT_EXT` the GPU copy can land directly in the shared region, eliminating the staging `memcpy` — but GraftX always falls back to the default two-copy path when host-pointer import is unavailable or unverified. Format conversion (e.g. server-native `BGRA8` to a Linux-preferred `XRGB8888`) will be folded into the copy via a tiny compute/blit shader so it costs no extra pass.

Capture is issued on a **dedicated present queue/stream** so it does not serialize the render queue, and it is fenced (the Sync chapter (Ch. 13)): the `FrameToken` carries the server fence guarding "pixels are fully written to shm." The token is sent over vsock only *after* the GPU signals — never before, or the client would read a half-written frame.

```rust
pub struct FrameToken {
    pub surf: SurfaceId,
    pub slot: u32,         // index into ShmFrameRing
    pub seq: u64,
    pub present_id: u64,   // matches the client's present call (for FIFO/feedback)
    pub gpu_done: ServerFenceId, // already-signaled by the time client sees this
}
```

## 22.4 Client-side display strategies

On the Linux side, the presenter receives the `FrameToken` and must put the slot's pixels on screen. GraftX will support a ladder of strategies, negotiated against what the Linux compositor and shim API actually need. From lowest-overhead to most-compatible:

1. **dma-buf import (preferred, Wayland/GBM).** If the ivshmem region (or a copy of it) can be wrapped as a `dma-buf` via the Linux UDMA-BUF / GBM path, the presenter imports the slot as a `wl_buffer` through `zwp_linux_dmabuf_v1` and attaches it directly to the application's `wl_surface`. The compositor then composites it like any GPU client buffer — *no further copy on the Linux side.* This is the closest GraftX gets to zero-copy presentation.

2. **wl_shm buffer.** When dma-buf import is unavailable, the presenter exposes the shm slot (or a `memcpy` of it into a `wl_shm`-pool fd) as a `wl_buffer`. The compositor uploads it to a texture itself. One extra copy (shm→wl_shm pool) unless the pool *is* the ivshmem region.

3. **Client-side GL/EGL texture (the common app case).** Most apps presenting through GraftX are themselves GL/Vulkan apps whose `eglSwapBuffers` we intercepted. Here the "Linux compositor" is reached through the app's *own* window system binding, and GraftX must make the default framebuffer's contents appear. The presenter uploads the shm slot into a real *local* GL texture (using a thin local GL context, EGL surfaceless, or the host driver's software rasterizer) and blits it to the actual window surface. This is the universal path and the one most apps will hit.

4. **X11 / XShm / Present extension.** On X, the presenter writes into an XShm image and issues `XShmPutImage` or, better, the X `Present` extension `PresentPixmap` for tear-free flips synced to vblank.

The strategy is chosen at surface creation and stored on the `PresentSurface`. The shim's `eglCreateWindowSurface` (the OpenGL/GLES/EGL/GLX chapter (Ch. 15)) / `vkCreateSwapchainKHR` (the Vulkan chapter (Ch. 14)) records *which native window handle* (`wl_surface*`, `xcb_window_t`, GBM surface) the app gave it; that handle decides which presenter applies.

```rust
pub enum DisplaySink {
    WaylandDmabuf { surface: WlSurfacePtr, dmabuf: DmabufExporter },
    WaylandShm    { surface: WlSurfacePtr, pool: WlShmPool },
    LocalGlBlit   { window: NativeWindow, blit_ctx: LocalGlCtx, tex: GlTexture },
    X11Present    { window: XWindow, shm_seg: XShmSegment },
}
```

## 22.5 Sequence walk-through: a Vulkan present at 60 Hz

```
t0  app: vkAcquireNextImageKHR(swapchain)
      shim: returns proxy image idx from free pool, NO wire trip
            (acquire is satisfied locally against the ShmFrameRing
             free-slot count; blocks only if all slots in flight)
t1  app: records cmds, vkQueueSubmit(render)
      shim: batches (the Sync chapter (Ch. 13)), forwarded async
t2  app: vkQueuePresentKHR(idx)
      shim: sends Present{surf, idx, present_id} over vsock; returns
            immediately (Mailbox/Immediate) or after fence (Fifo)
--- server side ---
t3  server: replays render submit on GPU queue
t4  server: present → enqueue capture on present queue, fenced
t5  GPU: render done → capture copy done → fence signals
t6  server: sends FrameToken{slot, seq, present_id} over vsock
--- client side ---
t7  presenter: receives token; seqlock-read slot; import as dmabuf
t8  presenter: wl_surface.attach + damage + commit (or GL blit)
t9  compositor: composites at next vblank → pixels on screen
t10 presenter: releases slot back to ShmFrameRing free pool
      (this is what eventually unblocks a future acquire at t0)
```

The critical insight: **acquire never blocks on the wire** as long as a free slot exists. The wire round trip (t2→t6) is absorbed by the swapchain depth. With 3 ring slots and `Mailbox`, the app can be a frame or two ahead of what is on screen, exactly as a local Mailbox swapchain behaves. Backpressure (the Sync chapter (Ch. 13)) kicks in only when the app outruns the copy-back bandwidth — then `acquire` stalls at t0 until t10 frees a slot, which is the correct, self-limiting behavior.

For `Fifo` (vsync, the default), the present call's return is gated on a *presentation-feedback* signal so the app pacing matches the Linux compositor's vblank, not the server's. GraftX will plumb Wayland `wp_presentation_feedback` / Vulkan `VK_KHR_present_wait` back into the proxy so `vkWaitForPresentKHR` and FIFO throttling stay honest end-to-end.

## 22.6 Comparison to Looking Glass

Looking Glass solves a superficially similar problem — getting a Windows VM's GPU output onto a Linux host — and GraftX deliberately diverges. The contrast clarifies the design:

| Dimension | Looking Glass | GraftX presentation path |
|-----------|---------------|--------------------------|
| What is captured | The *entire* Windows desktop framebuffer (full-frame, via DXGI/NvFBC) | A *specific application surface* the guest app rendered through our shims |
| Capture trigger | Host-side capture loop polling the desktop | The app's own `present` call — frame-accurate, no polling |
| Granularity | One framebuffer for the whole VM | Per-surface, per-app; many independent surfaces possible |
| Transport | KVMFR shared-memory ring (IVSHMEM) | ivshmem bulk plane (shared mechanism, different framing) |
| Direction of rendering | Windows app renders on Windows, you *watch* it | Linux app renders *via* remoting, output returns to Linux |
| Damage / partial update | Limited; mostly full frames | Per-surface damage rects in `ShmFrameSlot.flags` |
| Use case | Run a Windows VM, view it from Linux | Run a *Linux* app that uses the Windows GPU transparently |

The key architectural difference: Looking Glass is a **whole-desktop framebuffer relay** — it does not know about individual API calls or windows; it ships a flat image. GraftX is an **API-remoting** layer, so it captures at the surface level, is driven by the application's own present cadence (no wasteful polling of unchanged frames), and can carry damage rectangles and multiple independent surfaces. GraftX will, however, *reuse the proven KVMFR-style shared-memory framing* for the per-frame ring because it is the same hardware substrate (ivshmem); the Memory chapter's (Ch. 12) allocator and the Workspace chapter's (Ch. 5) BAR layout subsume it.

One inherited lesson: Looking Glass demonstrates that a tight shared-memory ring with a seqlock and host-signalled "frame ready" is sufficient for tear-free 4K60. GraftX adopts the same seqlock discipline (22.2) rather than inventing a heavier protocol.

## 22.7 Latency budget and mitigations

The end-to-end added latency over local rendering, broken down for 4K RGBA8 @ 60 Hz:

| Stage | Estimated cost | Notes |
|-------|---------------|-------|
| Present cmd over vsock (t2→t3) | 5–30 µs | small frame, control plane (the Transport chapter (Ch. 8)) |
| GPU capture copy (t4→t5) | ~0.5–1.5 ms | 33 MiB GPU→host; overlaps next render |
| FrameToken over vsock (t6→t7) | 5–30 µs | tiny |
| Client import / blit (t7→t8) | ~0 (dmabuf) to ~1 ms (GL upload) | strategy-dependent |
| Compositor wait for vblank (t8→t9) | 0–16.7 ms | unavoidable, same as local |

The structural overhead GraftX adds over a local app is therefore roughly **one capture copy (~1 ms) plus two control round trips (~tens of µs)** — typically under one frame of added latency, hidden by swapchain depth so it costs *throughput* nothing and adds at most ~1 frame of *display* latency. Mitigations the design will employ:

- **Direct-readback-into-shm** (22.3), an opportunistic negotiated optimization over the default two-copy path, to delete the staging copy where host-pointer import is available.
- **Damage rects**: only changed regions are copied and re-uploaded; idle UIs cost near zero.
- **Async capture queue** so capture never stalls render.
- **Triple-buffered ring** to fully overlap copy-back with the next frame's GPU work.
- **dma-buf zero-copy import** on Wayland to delete the Linux-side copy entirely.
- **FIFO pacing via presentation-feedback** so the app throttles to the *real* display, preventing wasted frames that would only inflate latency.

The remaining honest tradeoff: GraftX will never beat true local scanout, because the pixels physically live in the wrong GPU's memory. The goal (per project priorities: breadth > performance > stability) is *correct, sub-frame-overhead* presentation across every API, not zero overhead. The Sync chapter (Ch. 13) (sync/backpressure) and the Memory chapter (Ch. 12) (bulk transport) supply the machinery this chapter assembles into a coherent display path.
