# 21. Tier-3 Backends: SYCL, OptiX, AMF, WebGPU

This chapter specifies how GraftX will remote four "tier-3" GPU API families — SYCL (atop Level Zero), OptiX (atop CUDA), AMD's AMF encoder, and WebGPU/wgpu — by composing the tier-1/tier-2 backends rather than building bespoke wire surfaces, and defines the prioritization and shared scaffolding that keeps these backends cheap to grow.

## 21.1 What "tier-3" means and why these four

GraftX ranks API families by *return on remoting effort*: how much coverage breadth (priority #1) a backend buys per line of validated replay code. Tier-1 families (the OpenGL/GLES/EGL/GLX chapter (Ch. 15), the CUDA chapter (Ch. 16), the Vulkan chapter (Ch. 14), the Level Zero chapter (Ch. 19)) get hand-written, fully validated wire surfaces because they are large, popular, and have no cheaper substrate underneath them. Tier-3 families share a defining property: **each is a thin-ish layer that can be expressed almost entirely in terms of a tier-1 backend we will already have.** Remoting them at their *own* ABI would duplicate hundreds of entry points; remoting the layer underneath and letting the real upstream runtime run client-side or server-side gives us most of the coverage for a fraction of the cost.

| Family | Real substrate | GraftX strategy | Effort |
|--------|----------------|-----------------|--------|
| SYCL (DPC++/AdaptiveCpp) | Level Zero (Intel), CUDA, HIP | run SYCL runtime **client-side**, remote its L0/CUDA backend | Low |
| OptiX | CUDA driver API + RTX | run host OptiX **server-side**, remote a *narrow OptiX opcode set* | Medium |
| AMF | D3D11/Vulkan + VCN ASIC | remote AMF object model directly (small, COM-like) | Medium |
| WebGPU / wgpu | Vulkan / D3D12 / Metal | run wgpu **server-side**, remote a flat WebGPU command set | Medium |

The unifying decision: **do not invent a new transport-level concept for any of these.** Every tier-3 backend reuses the framing/opcodes of the Protocol chapter (Ch. 06), the serialization of the Serialization chapter (Ch. 07), the bulk path of the Memory chapter (Ch. 12), the fence model of the Sync chapter (Ch. 13), and the validation gates of the Security chapter (Ch. 23). A tier-3 backend is, structurally, a `Backend` impl plus a `proto` opcode block, nothing more.

## 21.2 SYCL atop Level Zero

SYCL is C++ template metaprogramming over a heterogeneous runtime; there is no stable SYCL "C ABI" to intercept the way we intercept `libcuda.so`. The compiler (Intel DPC++ or AdaptiveCpp) bakes kernels into the host binary and the SYCL runtime selects a backend plugin at runtime — almost always **Level Zero** on the GPUs GraftX targets, sometimes CUDA/HIP.

The design decision is therefore **not to remote SYCL at all.** We let the unmodified SYCL runtime execute inside the Linux guest, and we remote *its chosen backend*. Concretely, the SYCL runtime's Unified Runtime / plugin layer loads `libze_loader.so`; that loader is the GraftX Level Zero shim (the Client shim chapter (Ch. 09) export mechanics). So SYCL → UR → Level Zero shim → GraftX → server-side real L0 driver.

```
Linux guest                                     Windows guest
+------------------------------------------+    +----------------------+
| app.cpp (SYCL)                           |    | graftx-server        |
|   DPC++ runtime  ->  Unified Runtime     |    |   L0 replay backend  |
|     UR_L0 plugin -> libze_loader (SHIM) -+--->-+--> real ze* driver  |
|   (kernels already compiled into binary) |    |    (Intel ICD)       |
+------------------------------------------+    +----------------------+
```

The one place SYCL leaks above Level Zero is **kernel provenance**. DPC++ ships SPIR-V (or AOT GEN binaries) embedded in the host ELF; the runtime hands these to `zeModuleCreate`. Because we intercept at L0, the SPIR-V blob is just a `zeModuleCreate` input we forward as a bulk payload (the Memory chapter (Ch. 12)) and validate as opaque SPIR-V (the Security chapter (Ch. 23) SPIR-V validator, shared with the Vulkan backend). No SYCL-specific parsing is needed.

```rust
/// SYCL needs *no* new opcodes. It is satisfied entirely by the
/// Level Zero backend. This marker exists only so capability
/// negotiation (§21.6) can advertise SYCL when L0 + SPIR-V are present.
pub struct SyclCapability;

impl Tier3Capability for SyclCapability {
    fn family(&self) -> ApiFamily { ApiFamily::Sycl }
    /// SYCL is *derived*: it is available iff its substrate is.
    fn requires(&self) -> &'static [ApiFamily] {
        &[ApiFamily::LevelZero, ApiFamily::SpirV]
    }
    fn opcodes(&self) -> OpcodeRange { OpcodeRange::EMPTY }
}
```

Two real risks remain and shape the plan:

1. **USM (Unified Shared Memory).** SYCL leans on `zeMemAllocShared`. Across a guest-to-guest boundary, "shared" host/device memory cannot be physically shared — the device is on the Windows guest. The L0 backend (its own chapter) will service shared/host USM allocations out of the ivshmem region so the guest pointer is valid client-side and the server maps the same pages; *device* USM stays server-private. SYCL inherits whatever the L0 backend decides; this chapter only flags that SYCL programs are USM-heavy and must be a correctness test target.
2. **`reqd_work_group_size` / sub-group queries** are answered by the real driver server-side and round-trip once per kernel at first launch, then cache (mirror pattern, the OpenGL/GLES/EGL/GLX chapter (Ch. 15) §15.2).

Open question for SYCL-over-CUDA: when the runtime selects the CUDA UR plugin, the substrate becomes the CUDA backend (the CUDA chapter (Ch. 16)) instead. We will support whichever substrate the runtime picks; the only GraftX-visible difference is *which* shim `dlopen` resolves. No extra design surface.

## 21.3 OptiX atop CUDA

OptiX is the opposite shape from SYCL. It is a real C ABI (`optix_function_table`) obtained via `optixInit()`, but it is **inseparably fused to CUDA**: every OptiX object lives in a `CUcontext`, all inputs/outputs are `CUdeviceptr`, and the SBT (shader binding table), GAS/IAS acceleration structures, and pipeline all reference CUDA allocations. Trying to run the OptiX *runtime* client-side is impossible — it needs the driver and RTX cores that only exist server-side.

Strategy: **run the host OptiX runtime server-side**, and remote a *narrow, explicit* OptiX opcode block that piggybacks on the CUDA backend's handle table (the Handles chapter (Ch. 11)) and bulk transfers (the Memory chapter (Ch. 12)). The OptiX opcodes are few because the API is small (~40 entry points) and dominated by three heavy operations.

```rust
#[repr(u16)]
pub enum OptixOp {
    DeviceContextCreate = 0x2100,
    ModuleCreate,             // PTX/OptiX-IR blob -> server module
    ProgramGroupCreate,
    PipelineCreate,
    PipelineSetStackSize,
    AccelComputeMemoryUsage,  // query: returns sizes
    AccelBuild,               // the big one: builds GAS/IAS
    AccelCompact,
    SbtRecordPackHeader,      // header packing helper (server-side)
    Launch,                   // optixLaunch: SBT + pipeline + dims
    DenoiserCreate,
    DenoiserInvoke,
    DeviceContextDestroy,
}
```

The interception target is `optixGetFunctionTable` (the symbol behind `optixInit`). The shim returns a table of GraftX thunks. Each thunk records an `OptixOp` into the active CUDA stream's command batch (the CUDA chapter (Ch. 16) stream model), so OptiX and CUDA commands interleave in submission order on the same stream — this is essential, because OptiX launches and `cuMemcpy`s have real ordering dependencies.

### Acceleration structures and the SBT

`optixAccelBuild` is the cost center. Inputs are arrays of `OptixBuildInput` referencing vertex/index `CUdeviceptr`s already resident server-side; output is a GAS handle plus a `CUdeviceptr` blob. We do **not** ship geometry over the wire for builds — it is already in device memory from prior `cuMemcpyHtoD` (the whole point of fusing to CUDA). Note that per D13 the guest holds the *real* server `CUdeviceptr` as an opaque integer, so these device-pointer slots already carry real device addresses; the opcode carries only the build-input *descriptors* (offsets, strides, flags, device pointers resolved through the Handles chapter (Ch. 11) handle table), and the build runs entirely server-side.

```rust
/// Server side: descriptors are validated, device pointers resolved
/// through the CUDA handle table, then handed to the real runtime.
fn replay_accel_build(&mut self, msg: &AccelBuildMsg) -> Result<GasHandle> {
    // Ch.23: reject overlapping/out-of-range device ranges, NaN AABBs,
    // primitive counts that exceed quota, mismatched stride/format.
    self.validate_build_inputs(&msg.inputs)?;
    let inputs = self.lower_build_inputs(&msg.inputs)?; // handles -> CUdeviceptr
    // SAFETY: inputs validated above; buffers are server-private copies
    // of guest data per Ch.12 (write-revocation semantics).
    let gas = unsafe { self.optix.accel_build(self.ctx, &inputs, &msg.opts)? };
    Ok(self.handles.insert_gas(gas))
}
```

The **SBT** is a packed array of records (header + per-record data). Records embed program-group headers produced by `optixSbtRecordPackHeader`, which must be computed *with the server-side program groups* — the client cannot know the opaque header bytes. So `SbtRecordPackHeader` is a server round-trip that returns the 32-byte headers; the client then assembles the SBT in an ivshmem buffer and `Launch` references it by handle. Launch carries `(pipeline_handle, sbt_layout, width, height, depth, params_devptr)`.

Tradeoff: OptiX-IR vs PTX modules. We forward whichever blob the app supplies (`optixModuleCreate` takes either) as an opaque bulk payload, validated as PTX (the CUDA chapter (Ch. 16) PTX validator) or as an opaque OptiX-IR blob (limited validation — size/quota only, since OptiX-IR is undocumented). We accept reduced validation on OptiX-IR as a coverage-over-safety call consistent with project priorities, and log the concession for the hardening pass (the Security chapter (Ch. 23)).

## 21.4 AMF (AMD Media Framework) encode

AMF is AMD's encode/transcode SDK. Unlike the codec interop layers covered with the video chapter, AMF exposes its *own* COM-like object model: you get an `AMFFactory`, create an `AMFContext`, then `AMFComponent`s (encoder, converter), and push `AMFSurface`/`AMFData` through `SubmitInput`/`QueryOutput`. On Linux the entry point is `AMFQueryVersion`/`AMFInit` in `libamfrt64.so.1`.

Because AMF is a small, vtable-driven object model, we remote it **directly** — there is no cheaper substrate (running AMF client-side is impossible; it needs VCN hardware server-side). The plan mirrors the COM-ish shape with a handle table and a `Property` bag, which is most of AMF's surface area.

```rust
#[repr(u16)]
pub enum AmfOp {
    FactoryCreateContext = 0x2200,
    ContextInitDx11,          // or ContextInitVulkan — picks interop
    ComponentCreate,          // "AMFVideoEncoderVCE_AVC" / HEVC / AV1
    ComponentInit,
    SetProperty,              // typed key/value into the property bag
    GetProperty,
    SurfaceCreate,            // alloc input surface (server-side pool)
    SubmitInput,              // push a frame
    QueryOutput,              // pull an encoded packet (may be EAGAIN)
    Drain,
    ComponentRelease,
    ContextTerminate,
}
```

Three design points:

**Property bag.** AMF configures everything (bitrate, rate-control mode, GOP, codec profile) via `SetProperty(name, AMFVariant)`. We serialize a tagged `AmfVariant` enum (bool/i64/f64/AMFRate/AMFRatio/AMFSize/string/interface-ref) and validate keys against an allowlist server-side (the Security chapter (Ch. 23)): reject unknown or dangerous properties rather than passing arbitrary strings to the driver.

```rust
pub enum AmfVariant {
    Bool(bool), Int64(i64), Double(f64),
    Rate { num: u32, den: u32 },
    Ratio { num: u32, den: u32 },
    Size { w: i32, h: i32 },
    Rect { l: i32, t: i32, r: i32, b: i32 },
    String(BoundedString),       // length-capped, UTF-8 validated
    Interface(AmfHandle),        // ref to another remoted object
}
```

**Surface flow & zero-copy.** Raw input frames (NV12/RGBA) are large and per-frame; they ride the ivshmem bulk plane (the Memory chapter (Ch. 12)). The client writes the frame into a bulk buffer; `SubmitInput` references it by region; the server validates dimensions/format/stride against the component's configured input, copies into a server-private `AMFSurface`, and submits. Encoded output is comparatively tiny and returns inline on the control plane (or via a small bulk slab for large IDR frames).

**Async output.** AMF's `QueryOutput` returns `AMF_REPEAT` when no packet is ready. We map this to the Sync chapter (Ch. 13) async model: the client issues `QueryOutput` non-blocking and polls, or registers a completion fence so the encoder can run ahead of the consumer. The decision table:

| AMF return | GraftX wire result | Client action |
|------------|--------------------|---------------|
| `AMF_OK` + data | packet (inline/bulk) | deliver |
| `AMF_REPEAT` | `Pending` | poll / wait on fence |
| `AMF_EOF` | `Eof` | stop draining |
| `AMF_NEED_MORE_INPUT` | `NeedInput` | submit next frame |

Interop choice (`ContextInitDx11` vs `ContextInitVulkan`) is server-side policy; the default will be D3D11 (most stable AMF path on Windows) with Vulkan as a fallback that lets AMF surfaces share storage with a Vulkan backend output for transcode pipelines.

## 21.5 WebGPU / wgpu passthrough

WebGPU has a clean, *flat*, handle-based C ABI (`webgpu.h` / Dawn / wgpu-native): no implicit global state, explicit command encoders, explicit pipelines, and an asynchronous request model (`wgpuInstanceRequestAdapter`, `...RequestDevice` use callbacks). This is the friendliest tier-3 family to remote.

Strategy: **run the real wgpu/Dawn runtime server-side** over the server's Vulkan/D3D12, and remote the flat WebGPU command set. The shim exports the `wgpu*` C symbols (the Client shim chapter (Ch. 09)); each maps near-1:1 to an opcode. Because WebGPU IDs are already opaque pointers/`u64` handles, they are reconciled against the Handles chapter (Ch. 11) handle table — the server mints the wire `Handle` (D1/D5), and any synchronous-return-shaped wgpu IDs map to local provisional proxy tokens until the server's real handle arrives.

The interesting parts are async callbacks and WGSL:

```rust
#[repr(u16)]
pub enum WgpuOp {
    CreateInstance = 0x2300,
    RequestAdapter,           // async -> server resolves, returns handle
    RequestDevice,            // async
    CreateShaderModule,       // WGSL or SPIR-V blob (bulk)
    CreateRenderPipeline,     // may be async (...Async variant)
    CreateBuffer,
    QueueWriteBuffer,         // bulk upload (immediate, not map-based)
    QueueSubmit,              // command buffers
    BufferMapAsync,           // map for Read (readback) or Write (upload)
    BufferUnmap,              // flush dirty (write-map) / release slab (read-map)
    DevicePoll,               // drives async completion
}
```

**Async request model.** `RequestAdapter`/`RequestDevice`/`CreateRenderPipelineAsync` take user callbacks. We turn each into a request carrying a `request_id`; the server resolves it and posts a completion event back over the control plane; the shim's event pump invokes the stored callback on the next `wgpuDevicePoll`/`wgpuInstanceProcessEvents`. This matches WebGPU's own "completions deliver on a poll tick" contract, so we are spec-faithful, not papering over it.

```rust
struct PendingRequest<T> {
    id: RequestId,
    callback: Box<dyn FnOnce(WgpuResult<T>)>,
}
// Completion events arrive out of band; the shim matches by RequestId
// and fires the callback during the next Process/Poll call.
```

**WGSL.** `wgpuCreateShaderModule` may carry WGSL *source* or SPIR-V. WGSL is forwarded as a bounded UTF-8 bulk payload; we do **not** compile it client-side. Validation is delegated to the server-side wgpu runtime (Naga), which already rejects malformed WGSL safely — we add only size/quota gates (the Security chapter (Ch. 23)). SPIR-V modules reuse the shared SPIR-V validator (§21.2, the Security chapter (Ch. 23)).

**Mapping: readback and upload.** `BufferMapAsync` is GraftX's `GetMappedRange`-equivalent and serves *both* directions; which one applies is decided by the `MapMode` (Read vs Write) the app passed at map time. Both reuse the Memory chapter (Ch. 12) bulk plane verbatim, but differ in copy direction and in what `Unmap` does:

- **Read-map (readback).** The server completes the map, copies the mapped range *server→ivshmem* into a read slab, and the completion event hands the client a pointer into that slab. The client reads through it; `BufferUnmap` simply releases the slab (no flush — the client made no writes the server needs).
- **Write-map (upload).** The map resolves to a *writable* ivshmem slab sized to the requested range; the server does **not** pre-copy GPU contents (WebGPU `MapMode::Write` ranges are defined as having undefined initial contents). The client writes its upload data directly into the slab, then calls `wgpuBufferUnmap`. `BufferUnmap` is the flush point: the shim marks the slab dirty and the server copies *ivshmem→GPU buffer* before releasing the slab, so the upload is observable to any subsequent `QueueSubmit`. This keeps the map-write upload path zero-extra-copy on the client side and symmetric with the readback path, while `QueueWriteBuffer` remains the separate immediate (non-map) upload op for one-shot writes.

Slab lifetime in both directions is guarded by the canonical `ShmSlice{offset,len,gen}` descriptor (D6) so a stale map cannot alias a recycled slab.

## 21.6 Shared infrastructure and the `Tier3Capability` trait

Every tier-3 backend reduces to: a `proto` opcode block, a `Backend` server impl, optional client thunks, and a capability descriptor. The shared scaffolding is small but deliberate.

```rust
/// Implemented once per tier-3 family. The dispatcher (ch.10) routes
/// an opcode to a Backend; this trait only governs *negotiation*.
pub trait Tier3Capability: Send + Sync {
    fn family(&self) -> ApiFamily;
    /// Substrates that MUST be present for this family to be offered.
    fn requires(&self) -> &'static [ApiFamily];
    fn opcodes(&self) -> OpcodeRange;
}
```

Negotiation (extending ch.06's version handshake): at session start the server probes which substrates exist (real L0 ICD? CUDA+RTX? AMF runtime? wgpu?) and advertises each tier-3 family **only if its `requires()` set is satisfiable**. SYCL is advertised iff L0+SPIR-V present; OptiX iff CUDA+RTX; etc. The client shim, on `dlopen`, checks the negotiated set and either installs real thunks or fails the load gracefully (ch.20) so an app missing a backend gets a clean "no device" rather than a crash.

Three shared facilities are reused, not rebuilt:

- **SPIR-V validator** (ch.19) — shared by SYCL (L0 modules), WebGPU (SPIR-V path), and Vulkan.
- **PTX validator** (ch.16) — shared by OptiX modules.
- **Bulk slab + completion-event pump** (ch.12, ch.13) — used identically by AMF surfaces, WebGPU buffer maps, OptiX SBT assembly, and SYCL USM.

### Prioritization within tier-3

Implementation order follows coverage-per-effort and dependency readiness:

| Order | Family | Blocked on | Rationale |
|-------|--------|-----------|-----------|
| 1 | SYCL | L0 backend done | ~zero new code once L0 ships; instant breadth |
| 2 | WebGPU | server wgpu | clean ABI, growing ecosystem, low risk |
| 3 | OptiX | CUDA backend (ch.16) | high value (RT), medium effort, narrow opcodes |
| 4 | AMF | video plumbing | vendor-narrow (AMD only), most validation surface |

The guiding tradeoff for all four: we accept **reduced or delegated validation** (OptiX-IR opaqueness, WGSL/Naga delegation, AMF property allowlists) and **server-side runtime trust** in exchange for large coverage gains with little bespoke wire code — exactly the breadth-over-safety ranking in the project priorities, with every concession logged for the hardening pass (ch.19).
