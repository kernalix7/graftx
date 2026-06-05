# 20. Video Codec Backends (VA-API / VDPAU / NVENC / NVDEC / Vulkan Video)

This chapter specifies how GraftX will remote hardware video decode and encode: session lifecycle, bitstream and surface transfer, the per-vendor API surfaces (VA-API, VDPAU, NVENC/NVDEC, AMF, Vulkan Video), the cross-vendor Vulkan Video path, and the format negotiation and zero-copy strategy that keep per-frame latency bounded.

## 20.1 Why video is its own backend

Video codecs are not "just another GPU API". They differ from the OpenGL/Vulkan compute paths (the Vulkan chapter (Ch. 14) and the OpenGL/GLES/EGL/GLX chapter (Ch. 15)) in three ways that dominate the design:

1. **Streaming, not one-shot.** A session lives for thousands of frames. Setup cost (probing, parameter-set parsing, surface-pool allocation) is amortized, so per-call overhead matters more than start-up overhead.
2. **Asymmetric payloads.** Decode takes a *small* compressed bitstream chunk (kilobytes) and produces a *large* surface (a 4K NV12 frame is ~12 MiB). Encode is the mirror. The transfer plane must move the big side over ivshmem (the Transport chapter (Ch. 08)) and the small side inline over vsock (the Transport chapter (Ch. 08)).
3. **Real-time deadlines.** A 60 fps decoder has a 16.6 ms frame budget; a low-latency encoder (WebRTC-style) wants sub-frame latency. Every avoidable round-trip is a dropped frame.

GraftX will therefore implement video as a dedicated backend family with its own command opcodes (the Protocol chapter (Ch. 06)), its own surface-pool manager, and a vendor-abstraction trait that lets one remoted session map onto whatever the Windows host GPU actually exposes — which is frequently *not* the API the Linux guest asked for.

The headline coverage matrix (breadth is priority #1):

| Guest API (Linux) | Native host backend (Windows) | Decode | Encode |
|-------------------|-------------------------------|--------|--------|
| VA-API (libva)    | D3D11VA / NVDEC / AMF / Vulkan Video | yes | yes (VAEnc) |
| VDPAU             | D3D11VA / NVDEC               | yes    | n/a    |
| NVENC/NVDEC (NVIDIA Video Codec SDK) | NVENC/NVDEC native | yes | yes |
| Vulkan Video (`VK_KHR_video_*`) | Vulkan Video native | yes | yes |
| AMF (guest-side AMF shim) | AMF native            | yes    | yes    |

The crucial idea is the middle column: GraftX decouples the *guest-requested* API from the *host-executed* one. A Linux app using VA-API to decode H.264 may be replayed on the host via NVDEC, Vulkan Video, or D3D11VA depending on what the passed-through GPU supports. This translation lives entirely in `graftx-server`; the client shim (the Client shim chapter (Ch. 09)) only needs to faithfully forward the guest API.

## 20.2 Session lifecycle

A video session is the unit of remoting, analogous to a GL context (the OpenGL/GLES/EGL/GLX chapter (Ch. 15)) or a Vulkan device queue (the Vulkan chapter (Ch. 14)). The shim assigns a `VideoSessionId(u32)`; the server owns the real codec object.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoSessionId(pub u32);

pub enum SessionKind {
    Decode,
    Encode,
}

pub struct VideoSessionDesc {
    pub kind: SessionKind,
    pub codec: Codec,                 // H264, H265, AV1, VP9, MPEG2, ...
    pub profile: CodecProfile,        // Main, High, Main10, ...
    pub coded_extent: Extent2D,       // padded to codec macroblock/CTU size
    pub max_dpb_slots: u8,            // decoded-picture-buffer depth
    pub chroma: ChromaFormat,         // 420 | 422 | 444
    pub bit_depth: u8,                // 8 | 10 | 12
    pub guest_api: GuestVideoApi,     // VaApi | Vdpau | Nvcodec | VulkanVideo | Amf
    pub rate_control: Option<RateControl>, // encode only
}
```

Lifecycle phases, with the round-trip cost of each marked:

```text
phase            client action                     server action                  RT?
-----            -------------                      -------------                  ---
1 probe          query supported profiles/formats   enumerate native caps          yes (cached)
2 create         CreateVideoSession(desc)           pick host backend, alloc obj   yes
3 alloc-pool     CreateSurfacePool(n, fmt)          alloc DPB + output surfaces    yes
4 submit (loop)  SubmitDecode/Encode(refs, bits)    enqueue on native codec        no (batched)
5 retire (loop)  MapSurface / ReadBitstream         copy result to ivshmem         conditional
6 destroy        DestroyVideoSession(id)            free obj + pools               no (fire&forget)
```

Phases 1–3 happen once per session and are allowed to round-trip. Phase 4 is the hot loop and must be pipelined: the shim posts work without blocking, and only phase 5 (consuming a result) may block — and only if the result is not yet ready. The `probe` results are cached per host-GPU in a `VideoCaps` structure fetched at server attach (the Server core chapter (Ch. 10)) so that step 1 is usually a local lookup.

The server keeps per-session state in a table keyed by `VideoSessionId`, with a generation counter for ABA safety (same pattern as the handle table, the Handles chapter (Ch. 11)):

```rust
struct ServerVideoSession {
    gen: u32,
    backend: Box<dyn VideoBackend>,   // vendor abstraction (20.5)
    pool: SurfacePool,                // DPB + output surfaces
    in_flight: VecDeque<FrameTicket>, // submitted-but-not-retired
    quota: VideoQuota,                // backpressure (20.8)
}
```

## 20.3 Surface pools and the DPB

Both decode and encode revolve around a pool of GPU surfaces. For decode this is the **Decoded Picture Buffer** (reference frames the codec needs to reconstruct later frames) plus output surfaces; for encode it is the input surfaces plus reconstructed references. GraftX will model the pool explicitly so reference-frame relationships are forwarded as *handles*, never as pixel copies.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceId(pub u32);          // index into the session's pool

struct SurfacePool {
    surfaces: Vec<SurfaceSlot>,
    free: Vec<SurfaceId>,
    fmt: SurfaceFormat,                  // NV12, P010, YUV444, RGBA, ...
    extent: Extent2D,
}

struct SurfaceSlot {
    state: SurfaceState,                 // Free | InUse | Reference | Mapped
    native: NativeSurface,               // ID3D11Texture2D / CUarray / VkImage
    last_writer: Option<FrameTicket>,    // GPU work that last touched it
    dirty: bool,                         // needs copy-out before client read
}
```

The decisive optimization: **reference frames never leave the host.** When the guest decodes frame N referencing frames N-1 and N-2, the bitstream's reference indices are translated by the shim into `SurfaceId`s, sent in the command, and the server passes the corresponding native surfaces straight to the codec. No reference surface is ever serialized back to the guest unless the guest explicitly maps it (e.g. to display it). This keeps the per-frame uplink at "small bitstream + a few u32 handles".

```text
guest decode order            wire (uplink)                 host
------------------            -------------                 ----
decode frame 12 using         SubmitDecode {                NVDEC reads bitstream,
  refs [10, 11]                 session, out=12,            writes surface 12,
                                 refs=[10,11],              keeps 10,11 as native
                                 bitstream@shmoff }          DPB textures (no copy)
present frame 12              MapSurface { 12 }              copy surface 12 -> ivshmem
```

## 20.4 Bitstream and surface transfer

The two payload directions use different planes, chosen by the inline-vs-bulk threshold from the Memory chapter (Ch. 12) (default 4 KiB).

**Bitstream (decode in / encode out):** compressed slices are usually small. A single H.264 slice NAL is often a few KiB; large I-frames can be hundreds of KiB. The shim sends sub-threshold NALs inline in the vsock command frame and larger ones as an ivshmem `ShmSlice`. The command carries slice-parameter and picture-parameter structures decoded by the shim into a compact GraftX form so the server does not re-parse vendor-opaque blobs.

**Surfaces (decode out / encode in):** always bulk. A decoded NV12 4K frame is luma 3840×2160 + chroma 3840×1080 ≈ 12.4 MiB; copying that through vsock per frame at 60 fps would be ~745 MiB/s of pure socket copy. Instead, surfaces ride the ivshmem bulk plane:

```rust
pub enum VideoPayload {
    Inline(SmallVec<[u8; 256]>),   // tiny bitstream chunks
    Shm(ShmSlice),                 // bitstream >= threshold, or any surface
}

pub struct MapSurfaceReply {
    pub plane_layout: SmallVec<[PlaneDesc; 3]>, // offset/pitch per plane
    pub data: ShmSlice,                         // packed planes in ivshmem
    pub fence: FenceId,                         // ready-signal (the Sync chapter (Ch. 13))
}

pub struct PlaneDesc {
    pub offset: u32,   // byte offset within `data`
    pub pitch: u32,    // row stride (>= width*bpp, often hw-aligned)
    pub height: u32,
}
```

The `pitch` field matters: hardware codecs produce surfaces with alignment padding (e.g. 256-byte row pitch on NVDEC). The server reports the real pitch so the client can either consume the padded layout directly (zero-copy) or repack to a tight layout (one copy). The default is to expose the padded layout and let the consumer (often a GL/Vulkan texture import, the OpenGL/GLES/EGL/GLX chapter (Ch. 15) and the Vulkan chapter (Ch. 14)) take it as-is.

## 20.5 Per-vendor backends behind one trait

The server abstracts each native codec behind a `VideoBackend` trait. This is where API translation (20.1) is realized.

```rust
/// Implemented per host codec API in graftx-server. All methods are
/// invoked only on the server after the command stream is validated
/// (the Server core chapter (Ch. 10)). No client-controlled pointer ever reaches a driver.
pub trait VideoBackend: Send {
    fn caps(&self) -> &VideoCaps;

    /// Submit one coded picture (decode) or one raw frame (encode).
    /// Returns a ticket the server polls/fences against; never blocks.
    fn submit(&mut self, frame: &FrameSubmit) -> Result<FrameTicket, VideoError>;

    /// Has the ticket's GPU work finished? (non-blocking)
    fn poll(&mut self, t: FrameTicket) -> Poll<FrameResult>;

    /// Copy a finished output into a server-private staging buffer,
    /// then into the ivshmem region. Validated layout only.
    fn map_output(&mut self, s: SurfaceId, dst: &mut ShmRegion)
        -> Result<MapSurfaceReply, VideoError>;

    fn reconfigure(&mut self, rc: &RateControl) -> Result<(), VideoError>;
}
```

Concrete implementors:

- **`NvdecBackend` / `NvencBackend`** — wrap the NVIDIA Video Codec SDK via CUDA interop. Decode output lands in a `CUarray`; `map_output` uses `cuMemcpy2D` into pinned staging then DMA to ivshmem. Encode input is uploaded from ivshmem into a registered CUDA resource. Reference management uses NVENC's `NV_ENC_PIC_PARAMS` reference picture lists, populated from the translated `SurfaceId`s.
- **`D3d11VaBackend`** — uses the Windows DXVA2 / D3D11 video decode path (`ID3D11VideoDecoder`). This is the broadest decode path on Windows and the default fallback when a specific vendor SDK is unavailable. Surfaces are `ID3D11Texture2D` in NV12/P010.
- **`AmfBackend`** — AMD AMF for encode/decode on Radeon passthrough GPUs. Also reachable directly from a guest-side AMF shim (the Tier-3 chapter (Ch. 21) lists AMF among exported APIs).
- **`VulkanVideoBackend`** — the cross-vendor path (20.6).

Backend selection is a policy resolved at session create:

```rust
fn pick_backend(desc: &VideoSessionDesc, caps: &HostVideoCaps)
    -> Result<Box<dyn VideoBackend>, VideoError>
{
    // 1. Honor explicit guest API if host can do it natively.
    // 2. Else prefer a vendor SDK matching the passed-through GPU.
    // 3. Else fall back to Vulkan Video, then D3D11VA.
    // 4. Else NotSupported -> shim degrades gracefully (the Error/FFI chapter (Ch. 24)).
}
```

The tradeoff table the policy encodes:

| Backend     | Coverage | Latency | Zero-copy interop | Notes |
|-------------|----------|---------|-------------------|-------|
| Vendor SDK (NVENC/AMF) | narrow (1 vendor) | best | best (CUDA/D3D interop) | preferred when GPU matches |
| Vulkan Video | broad (cross-vendor) | good | excellent (VkImage shared with the Vulkan chapter (Ch. 14)) | newest, codec coverage still growing |
| D3D11VA     | broad decode | good | D3D11-only | decode-heavy, no encode here |

## 20.6 Vulkan Video as the cross-vendor spine

Vulkan Video (`VK_KHR_video_queue`, `VK_KHR_video_decode_*`, `VK_KHR_video_encode_*`) is the strategic long-term path because it unifies codec access under the same device and memory model GraftX already builds for Vulkan (the Vulkan chapter (Ch. 14)). One `VkDevice`, one allocator, one synchronization story (the Sync chapter (Ch. 13)) covers compute, graphics, *and* video — which means a decoded frame can be handed to a Vulkan render pass with **no copy and no format conversion**, just an image-memory barrier.

```text
Vulkan Video pipeline (server side)
+--------------------------------------------------------------+
| vkCreateVideoSessionKHR  --> session bound to DPB VkImages   |
| vkCmdBeginVideoCodingKHR                                     |
|   vkCmdDecodeVideoKHR { bitstream buffer, ref slots, dst }   |
| vkCmdEndVideoCodingKHR                                       |
|   -> dst VkImage usable as sampled image (barrier only)      |
+--------------------------------------------------------------+
```

GraftX will map its `VideoBackend` onto Vulkan Video as follows:

- `VideoSessionDesc` → `VkVideoSessionCreateInfoKHR` + `VkVideoProfileInfoKHR`. Codec/profile/chroma/bit-depth translate directly.
- The DPB is a set of `VkImage`s in a `VK_IMAGE_USAGE_VIDEO_DECODE_DPB_BIT_KHR` pool; `SurfaceId` indexes them.
- Bitstream arrives in a `VkBuffer` filled from the ivshmem `ShmSlice` (one validated copy, the Memory chapter (Ch. 12)).
- Synchronization reuses the Vulkan timeline-semaphore design from the Sync chapter (Ch. 13); the `FenceId` in `MapSurfaceReply` is a wrapped timeline value.

The cost is that codec/profile coverage in drivers is still maturing (AV1 encode, 4:4:4, 12-bit are uneven across vendors as of v0.0.0 planning). So Vulkan Video is the *preferred* path but not the *only* one — the policy in 20.5 falls back to vendor SDKs where Vulkan Video lacks a profile, preserving breadth.

## 20.7 Format negotiation

The guest API and the host backend rarely agree on surface format out of the box, so the session-create handshake includes an explicit negotiation, cached in `VideoCaps`.

```rust
pub struct VideoCaps {
    pub decode: Vec<CodecCap>,
    pub encode: Vec<CodecCap>,
}

pub struct CodecCap {
    pub codec: Codec,
    pub profiles: SmallVec<[CodecProfile; 4]>,
    pub max_extent: Extent2D,
    pub surface_formats: SmallVec<[SurfaceFormat; 4]>, // NV12, P010, ...
    pub min_dpb: u8,
    pub max_dpb: u8,
    pub max_ref_frames: u8,
}
```

The negotiation algorithm at create:

1. Shim sends the guest's requested `(codec, profile, chroma, bit_depth, extent)`.
2. Server intersects with `VideoCaps` for the chosen backend.
3. If the exact surface format is supported, use it (zero conversion).
4. If not, the server picks the closest superset (e.g. guest asks I420, host offers NV12) and records a **conversion shim** to run on `map_output`. The conversion is reported to the client so it knows the delivered layout.
5. If no profile matches at all, return `NotSupported`; the guest API call fails with the vendor's "unsupported profile" error code rather than crashing (graceful-degradation contract, the Error/FFI chapter (Ch. 24)).

Format mismatch decision table:

| Guest wants | Host gives | Action |
|-------------|-----------|--------|
| NV12 | NV12 | pass-through, zero-copy |
| I420 (planar) | NV12 (semi-planar) | server-side plane interleave on map_output |
| P010 (10-bit) | P010 | pass-through |
| YUV444 | not supported | NotSupported (no silent downgrade — would corrupt color) |
| RGBA (encode in) | NV12 expected | server-side RGB→YUV (color matrix from session metadata) |

A subtle correctness point: color-conversion direction, range (limited vs full), and matrix (BT.601/709/2020) are part of the session metadata and must be forwarded, or the host would guess and produce washed-out frames. GraftX will require the guest to supply `ColorSpace { matrix, range, primaries, transfer }` at session create, defaulting to BT.709 limited if the guest API leaves it unspecified.

## 20.8 Latency, pipelining, and zero-copy

The frame budget forces an asynchronous, credit-based pipeline. The shim must keep the host codec fed without blocking the guest thread, and only block when the guest actually consumes a not-yet-ready output.

```text
guest thread          shim ring            server               host codec
------------          ---------            ------               ----------
SubmitDecode(13) ---> enqueue, return ---> validate ----------> NVDEC busy
SubmitDecode(14) ---> enqueue, return ---> validate ----------> NVDEC busy
MapSurface(12) ------ blocks IFF fence(12) not signaled
                <---- copy 12 -> shm <---- fence(12) signaled <- done(12)
```

Key mechanisms, each cross-referenced to its owning chapter:

- **Submit is non-blocking.** `SubmitDecode/Encode` returns as soon as the command is batched (the Client shim chapter (Ch. 09)). The shim tracks a `FrameTicket` per submission.
- **Credit-based backpressure.** Each session has a quota of in-flight frames (`VideoQuota`, default proposed 8). When the guest exceeds it, `submit` blocks until the server retires a frame — this is the resource-limit requirement from the security model and prevents an abusive guest from exhausting host VRAM via unbounded DPB growth.
- **Fences over polling.** Output readiness is signaled with the same fence/timeline machinery as the rest of GraftX (the Sync chapter (Ch. 13)), so a guest waiting on a frame is woken precisely, not spun.
- **Zero-copy on the consumer side.** When a decoded frame's next use is a GL or Vulkan texture (the OpenGL/GLES/EGL/GLX chapter (Ch. 15) and the Vulkan chapter (Ch. 14)), the server can keep the surface as a native `VkImage`/`ID3D11Texture2D` and *never* round-trip it to the guest at all — the guest's `SurfaceId` and the renderer's texture handle reference the same server-side object. The pixel copy to ivshmem only happens when the guest genuinely needs the bytes (CPU read, screenshot, software post-processing).

```rust
pub struct VideoQuota {
    max_in_flight: u8,         // credit count
    issued: u8,
    vram_budget_bytes: u64,    // hard cap on pool allocation
    vram_used: u64,
}

impl VideoQuota {
    fn try_acquire(&mut self) -> Result<(), Backpressure> {
        if self.issued >= self.max_in_flight {
            return Err(Backpressure::TooManyInFlight);
        }
        self.issued += 1;
        Ok(())
    }
    fn release(&mut self) { self.issued = self.issued.saturating_sub(1); }
}
```

The end-to-end latency target for the default decode path (vendor SDK or Vulkan Video, surface kept host-side until presented) is **one submit RTT amortized away by pipelining, plus one fence wait, plus one ivshmem copy only at present time**. For a 4K60 stream that is well inside the 16.6 ms budget; the dominant cost is the single 12 MiB ivshmem copy at present, ~hundreds of microseconds at PCIe shared-memory bandwidth, not the network-style copy a vsock-only design would incur.

## 20.9 Errors, validation, and open questions

Decoded bitstreams are *untrusted input replayed against native drivers* — the central security concern of the whole project. The video backend's validation duties (detailed in the Security chapter (Ch. 23)) include: clamping `coded_extent` and `max_dpb_slots` to host caps before allocation; bounds-checking every `SurfaceId` reference index against the live pool; verifying bitstream `ShmSlice` offset/length lie inside the negotiated heap (the Memory chapter (Ch. 12)); and copying the bitstream into server-private memory before handing it to a parser, since some hardware parsers will read past a malformed buffer. Errors map through `thiserror`:

```rust
#[derive(thiserror::Error, Debug)]
pub enum VideoError {
    #[error("codec/profile not supported by host backend")]
    NotSupported,
    #[error("surface id {0:?} out of range for session")]
    BadSurface(SurfaceId),
    #[error("bitstream region {0:?} outside negotiated heap")]
    BadBitstream(ShmSlice),
    #[error("session quota exceeded")]
    Backpressure,
    #[error("native codec error: {0}")]
    Native(String),
}
```

Open questions deferred to implementation: (1) whether to expose film-grain synthesis (AV1) on the host or apply it client-side; (2) how to map VA-API's video-postprocessing (`VAProcPipeline`: scaling, deinterlace, tone-map) — likely a separate VPP backend reusing this pool model; (3) split-frame / multi-slice parallel decode submission ordering guarantees. These extend the trait in 20.5 without changing the session/transfer model above.
