# 16. CUDA Backend

How GraftX will remote NVIDIA CUDA — driver vs runtime API, module/kernel loading, streams, events, device memory, UVM, and the cuBLAS/cuDNN libraries riding on top — from a Linux guest to a Windows server holding the physical GPU.

The CUDA backend is the first compute API GraftX will support after the graphics stack, and it sets patterns reused by the OpenCL backend (Ch.17), the ROCm/HIP backend (Ch.18), the Level Zero backend (Ch.19), and the OptiX backend in the Tier-3 chapter (Ch.21) (the OptiX relationship is detailed at the end of this chapter and in the Tier-3 chapter (Ch.21)). CUDA is attractive as a first compute target because its surface is large but extremely regular: almost every entry point is a flat C function returning a `cudaError_t`/`CUresult`, taking opaque handles and PODs. That regularity lets the GraftX code-generation layer (the Build/dist chapter (Ch.28)) produce most of the shim (the Client shim chapter (Ch.09)) and replay code (the Server core chapter (Ch.10)) mechanically, leaving only a handful of "interesting" verbs — module load, kernel launch, memory transfer, and UVM — to be hand-written. This chapter specifies those interesting verbs and the policies around them.

## 16.1 Two APIs, one server

CUDA exposes two C ABIs the guest may link against:

| Layer | Library (Linux guest) | Symbol style | Handle types | Init model |
|-------|----------------------|--------------|--------------|------------|
| Driver API | `libcuda.so.1` | `cu*` (e.g. `cuMemAlloc`) | `CUcontext`, `CUmodule`, `CUfunction`, `CUstream`, `CUdeviceptr` | explicit `cuInit`, explicit contexts |
| Runtime API | `libcudart.so.12` | `cuda*` (e.g. `cudaMalloc`) | implicit "primary context", `cudaStream_t` | lazy, per-thread |

The runtime API is a thin convenience layer that, in the real stack, sits *on top of* the driver API inside the same process. GraftX will **not** preserve that layering on the client side. Instead each library is shimmed independently (two cdylibs from `graftx-client`: `libcuda.so.1` and `libcudart.so.12`), and **both** are lowered to a single unified driver-level command vocabulary in `graftx-protocol` before transport. The server replays everything against the driver API only.

Rationale and tradeoffs:

- **Pro:** one replay path. `cudaMalloc` and `cuMemAlloc` decode to the same `MemAlloc` command; the server links `libcuda` and never needs `libcudart`. Fewer server-side surfaces to validate (priority: breadth, then perf — a single path is also easier to optimize).
- **Pro:** the runtime's "primary context" and lazy-init semantics are reconstructed on the *client* where per-thread TLS is cheap, so we never round-trip a hidden `cuDevicePrimaryCtxRetain` per call.
- **Con:** the client shim must itself implement runtime semantics (default stream, primary-context refcounting, last-error stickiness). This is non-trivial state but it is *local* — see §16.7.

```text
 Linux guest                                    Windows server
 ┌───────────────┐  ┌───────────────┐
 │ app → cudart  │  │ app → libcuda │
 │  shim         │  │  shim         │
 └──────┬────────┘  └──────┬────────┘
        │ lower to driver verbs │
        └───────────┬───────────┘
                    ▼
         graftx-protocol (Cmd::Cuda(..))
                    │  vsock ctrl  +  ivshmem bulk
                    ▼
          graftx-server cuda replay  ──►  real libcuda.so / nvcuda.dll
```

## 16.2 Command taxonomy

CUDA verbs partition cleanly by how they touch the transport. The protocol enum will group them so the encoder in the Serialization chapter (Ch.07) can pick a fast path per group:

```rust
/// graftx-protocol::cuda
#[non_exhaustive]
pub enum CudaCmd {
    // --- pure control, tiny payload, may be async-batched (§16.5) ---
    Init        { flags: u32 },
    CtxCreate   { flags: u32, dev: i32 },
    StreamCreate{ flags: u32, priority: i32 },
    EventCreate { flags: u32 },
    EventRecord { stream: Handle, event: Handle },
    // --- module / code transfer (bulk, §16.4) ---
    ModuleLoadData   { image: BulkRef },          // PTX or cubin/fatbin blob
    ModuleGetFunction{ module: Handle, name: SmallString },
    // --- memory ---
    MemAlloc    { bytes: u64 },                    // server mints the real CUdeviceptr, reported to the guest
    MemcpyHtoD  { dst: DevPtr, src: BulkRef, bytes: u64, stream: Handle },
    MemcpyDtoH  { dst: BulkRef, src: DevPtr, bytes: u64, stream: Handle },
    MemcpyDtoD  { dst: DevPtr, src: DevPtr, bytes: u64, stream: Handle },
    // --- UVM (§16.6) ---
    MemAllocManaged { bytes: u64, flags: u32 },
    MemPrefetchAsync{ ptr: DevPtr, bytes: u64, dst_dev: i32, stream: Handle },
    // --- launch (§16.3) ---
    LaunchKernel(Box<LaunchDesc>),
    // --- sync ---
    StreamSynchronize { stream: Handle },
    EventSynchronize  { event: Handle },
}
```

`Handle` is the server-authoritative wire Handle minted by the server: the wire-format Handle defined in the Protocol chapter (Ch.06) and the object model in the Handles chapter (Ch.11) — a 64-bit value (kind/generation/slot), never a raw guest pointer, and never an id the client invents. `DevPtr` carries the *real* server `CUdeviceptr` (an opaque integer the guest never dereferences), validated against the server's allocation table (see §16.6 for why device pointers need special care). `BulkRef` references a region in the ivshmem bulk plane via the canonical `ShmSlice` descriptor (the Transport chapter (Ch.08); see also the Memory chapter (Ch.12)). `SmallString` is an inline-or-heap UTF-8 name to avoid an allocation for the common short kernel name.

## 16.3 Kernel launch — the hot verb

`cuLaunchKernel`/`cudaLaunchKernel` is the single most performance-critical and most awkward verb, because its arguments are *type-erased*: the driver receives `void** kernelParams`, an array of pointers to argument values whose sizes and meanings are known only to the kernel. There is no runtime metadata describing the parameter buffer layout. GraftX cannot serialize `void**` blindly.

The plan resolves this with the same trick `cudart` uses internally: the **launch is described by the cubin's per-function parameter table**, which GraftX recovers by *parsing the cubin/fatbin ELF metadata* — the `.nv.info.<func>` section, specifically the `EIATTR_KPARAM_INFO` entries (per-parameter ordinal/offset/size) and `EIATTR_CBANK_PARAM_SIZE` (total param-bank size). This is the authoritative source; GraftX does **not** rely on `cuFuncGetAttribute`, which does not expose the per-parameter layout. Two ABIs exist in practice:

1. **`kernelParams` form** — `void** params`, each entry points to one argument. The client does not know individual sizes, but it knows the per-parameter offsets/sizes and the *total* param-buffer size, which GraftX records at `ModuleGetFunction` time by having the server parse the cubin's `.nv.info.<func>` metadata (`EIATTR_KPARAM_INFO` / `EIATTR_CBANK_PARAM_SIZE`) and report the resulting parameter offsets/sizes back to the client.
2. **`CU_LAUNCH_PARAM_BUFFER_POINTER` extra form** — a single contiguous blob plus a size. This is trivially serializable.

Design: the client shim normalizes form (1) into form (2). When a kernel is first resolved, the server returns a `ParamLayout { total_bytes, offsets: Vec<(u32 /*off*/, u32 /*size*/)> }`. The client caches it keyed by the server-minted `CUfunction` Handle and, on each launch, gathers the `void**` entries into one packed buffer:

```rust
struct LaunchDesc {
    func: Handle,
    grid:  [u32; 3],
    block: [u32; 3],
    shared_mem_bytes: u32,
    stream: Handle,
    /// packed argument bytes, layout per cached ParamLayout
    arg_blob: SmallVec<[u8; 256]>, // inline for typical small kernels
}

// client side
fn pack_args(layout: &ParamLayout, params: *const *const c_void) -> SmallVec<[u8;256]> {
    let mut buf = smallvec![0u8; layout.total_bytes as usize];
    // SAFETY: params is a kernelParams array of `layout.offsets.len()` entries,
    // each pointing to `size` readable bytes per the cached cubin param table.
    for (i, &(off, size)) in layout.offsets.iter().enumerate() {
        unsafe {
            let src = *params.add(i) as *const u8;
            core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr().add(off as usize), size as usize);
        }
    }
    buf
}
```

On the server, the replay turns the packed blob back into the `extra` form so it never needs the per-arg pointer dance:

```rust
let cfg = [
    CU_LAUNCH_PARAM_BUFFER_POINTER, blob.as_ptr() as *mut c_void,
    CU_LAUNCH_PARAM_BUFFER_SIZE,    &mut sz as *mut _ as *mut c_void,
    CU_LAUNCH_PARAM_END,
];
// SAFETY: func validated against session module table; cfg follows the
// documented extra-options contract; blob size == cached total_bytes.
unsafe { cuLaunchKernel(func, gx,gy,gz, bx,by,bz, shmem, stream, ptr::null_mut(), cfg.as_ptr() as *mut _) }
```

**Pointer arguments inside the blob.** A kernel arg may itself be a `CUdeviceptr`. Because the guest already holds the *real* server `CUdeviceptr` for every device allocation (§16.6), the packed blob already carries real device addresses in its pointer slots — there is no surrogate→real rewrite of the blob on the server. The server's job is therefore *validation*, not translation: the `ParamLayout` records which offsets are pointer-typed (derived from the cubin's `EIATTR_KPARAM_INFO` metadata where available; otherwise from a heuristic plus a client-supplied `is_ptr` bitmask the higher-level wrapper passes through), and the server checks each pointer-typed slot falls inside a known allocation `[base, base+len)` before launch (defense against a forged device address, per the security model). This pointer-validation step is the crux of correctness for compute remoting and is shared with the OpenCL backend (Ch.17). Note that the OpenCL backend (Ch.17) instead passes `cl_mem` objects, which *are* surrogate handles resolved server-side; only the validation discipline is shared, not the address representation.

**Client-side launch-config validation.** Grid/block dimensions, shared-memory size, and dynamic-launch limits are validated on the *client* against the cached function/device limits so that an invalid configuration returns `CUDA_ERROR_INVALID_VALUE` / `cudaErrorInvalidConfiguration` **synchronously**, matching native semantics, instead of being silently optimistically-batched and surfaced late at the next sync (§16.5).

**Latency.** A launch is one control message; arg blobs for typical kernels are <256 B and travel inline on the vsock control plane, not the bulk plane (the inline-vs-bulk threshold is 4 KiB per the Memory chapter (Ch.12)). Launches are fire-and-forget (no return value except an error code that is *deferred* — see §16.5), so they batch aggressively.

## 16.4 Module and code transfer

Modules arrive as PTX text, `cubin` (architecture-specific machine code), or `fatbin` (a container of multiple cubins + optional PTX). GraftX treats all three as opaque byte blobs flowing over the bulk plane:

- `cudaGetModule`/`__cudaRegisterFatBinary` (runtime) and `cuModuleLoadData`/`cuModuleLoadFatBinary` (driver) all decode to `ModuleLoadData { image: BulkRef }`.
- The blob is copied into **server-private memory** before being handed to `cuModuleLoadData`, per the security model — the server must never let the driver JIT/relocate directly out of guest-visible ivshmem (a malicious guest could mutate it mid-load).
- The server keeps a per-session `HashMap<Handle, CUmodule>` and `HashMap<Handle, (CUfunction, ParamLayout)>`. `ParamLayout` is parsed once from the cubin's `.nv.info.<func>` ELF metadata (`EIATTR_KPARAM_INFO` / `EIATTR_CBANK_PARAM_SIZE`) at `ModuleGetFunction` and shipped back so launches are cheap (§16.3).

**Fat-binary registration interception.** Runtime apps don't call `cudaMalloc` first; the compiler emits `__cudaRegisterFatBinary` / `__cudaRegisterFunction` constructors that run at load time and register a host-stub-address → device-function mapping. The `libcudart` shim must export these hidden symbols. The shim records `host_stub_ptr → (fatbin_handle, mangled_name)` in a guest-local table; when the app later calls `cudaLaunchKernel(host_stub, ...)`, the shim translates the stub pointer into a `ModuleGetFunction` (lazily, once) and then the normal launch path. This stub-table is pure client state and never crosses the wire.

```text
load time:  __cudaRegisterFatBinary(img) ──► ModuleLoadData(img)  [bulk]
            __cudaRegisterFunction(stub,"_Z3addPf") ──► record stub→name (local)
launch:     cudaLaunchKernel(stub,...) ──► (first time) ModuleGetFunction(name)
                                       ──► LaunchKernel(func, packed args)
```

## 16.5 Streams, events, and the async/error model

CUDA's defining behavior is *asynchronous, ordered* execution per stream. GraftX must preserve ordering without paying a round-trip per async call. The plan:

- Each `CUstream` Handle maps to a server-side stream. All commands carrying that stream handle are enqueued **in submission order** on a per-stream command queue (the ordering guarantees in the Sync chapter (Ch.13), keyed by the per-session monotonic `seq`). The transport delivers them in order; the server replays them in order. No reordering across a single stream is ever introduced.
- Async calls (`MemcpyHtoDAsync`, `LaunchKernel`, `EventRecord`) return immediately on the client with `CUDA_SUCCESS`/`cudaSuccess` *optimistically*. Their real error code is captured server-side and folded into a **deferred-error** slot per stream.
- A synchronizing call (`StreamSynchronize`, `EventSynchronize`, `cudaDeviceSynchronize`, any blocking `MemcpyDtoH`) flushes the batch, waits, and returns the worst deferred error observed since the last sync. This matches CUDA's own "sticky error surfaces at next sync" semantics closely enough for correctness.

```text
client stream queue (batched on vsock, flushed on sync or buffer-full):
  [ MemcpyHtoDAsync ][ LaunchKernel ][ EventRecord ][ LaunchKernel ] ──flush──►
server replays in order; first non-success CUresult stored in stream.deferred_err
StreamSynchronize ──► server cuStreamSynchronize ──► returns deferred_err, clears it
```

**Default stream caveat.** The legacy default stream (`stream == 0`) is implicitly synchronizing against all other streams unless `--default-stream per-thread` is used. The client shim records the compilation mode (queried at init via the runtime's behavior) and, for legacy mode, inserts the implicit serialization point into the command ordering before flushing. This is a correctness-over-speed choice; per-thread default streams (the modern default) avoid the penalty.

**Callbacks.** `cuLaunchHostFunc`/`cudaStreamAddCallback` run host code *on the GPU's timeline*. Since the host code lives in the guest, the server cannot run it. The plan: the server replays a host-func that signals back to the client over the control plane (a `StreamCallbackFired { token }` event on the server→client event channel defined in the Protocol chapter (Ch.06) and dispatched by the Client shim chapter (Ch.09)); the client thread pool then invokes the user callback. This adds a guest round-trip but callbacks are rare and inherently latency-tolerant.

## 16.6 Device memory and pointers

`cudaMalloc` returns a `void*` and `cuMemAlloc` returns a `CUdeviceptr` (a `uintptr_t`). The app may do **pointer arithmetic** on these (`ptr + offset`) and pass sub-ranges to kernels and memcpys. GraftX therefore cannot hand back an opaque index disguised as a pointer — arithmetic would corrupt it.

Plan: the server allocates real device memory and reports the **real base `CUdeviceptr` plus length** back to the client. The client hands the app the *real* device-pointer value (it is just an integer; the guest never dereferences it on the CPU). The client maintains an interval tree of valid `[base, base+len)` regions:

```rust
struct DevRegion { base: u64, len: u64, server_alloc_id: Handle, managed: bool }
struct DevSpace { regions: BTreeMap<u64 /*base*/, DevRegion> } // interval lookup
```

When a device pointer appears as a memcpy target or kernel arg, the client validates it falls inside a known region; because the recorded `base` already *is* the real server `CUdeviceptr`, the value on the wire is the real device address itself (no surrogate→real reconstruction is needed). The server independently re-validates each incoming address against its own allocation table — `[base_real, base_real+len)` — before use (defense against a forged device address, per the security model). Reporting the real base keeps app-side arithmetic correct *and* gives the server this validation handle. The small information leak (real device addresses become guest-visible) is acceptable: the guest cannot touch that memory directly across the PCI/passthrough boundary.

**Unified Virtual Memory (UVM / managed memory).** `cudaMallocManaged` is the hard case: managed memory is page-migrated on demand between host and device, and the *host* (guest CPU) is expected to read/write it directly. In a remoting world the guest CPU and the device are on different machines, so true UVM coherency is impossible. The plan offers two modes, selected per session:

| Mode | Semantics | Cost | Default |
|------|-----------|------|---------|
| `uvm-emulate` | Treat managed alloc as device memory backed by a guest shadow page in ivshmem; sync shadow↔device at kernel-launch / sync boundaries (like explicit memcpy). CPU access to the shadow works; coherency is *bulk-synchronous*, not page-fault-granular. | shadow copy per sync | yes |
| `uvm-reject` | Return `CUDA_ERROR_NOT_SUPPORTED` for managed allocs; force apps onto explicit `cudaMalloc` paths. | none | opt-in for perf-critical sessions |

`cudaMemPrefetchAsync` and `cudaMemAdvise` become hints the emulation layer uses to decide *when* to push the shadow. Page-fault-driven access patterns (writing managed memory inside a long-running kernel and reading it on the host before sync) are documented as **unsupported** under emulation — a breadth-first compromise. True page-fault UVM would require a hypervisor-level memory device and is out of scope for v0.x.

## 16.7 Runtime-API state reconstruction

The `libcudart` shim must locally reproduce the runtime's hidden state so it can lower to driver verbs:

- **Primary context refcount** per device — `cuDevicePrimaryCtxRetain`/`Release` are issued to the server only on 0↔1 transitions.
- **Per-thread current device** (`cudaSetDevice`) — TLS; selects which context subsequent verbs bind to.
- **Last error** (`cudaGetLastError`/`cudaPeekAtLastError`) — sticky per-thread error, fed by the deferred-error mechanism of §16.5.
- **Default stream resolution** — maps `0` to either the legacy or per-thread server stream.

All of this is `thread_local!` + an atomic refcount map; none of it crosses the wire beyond the driver verbs it generates. No `unwrap` on these paths; errors map through the table in §16.8.

## 16.8 Error-code mapping

Driver (`CUresult`) and runtime (`cudaError_t`) use different numeric spaces for the same conditions. GraftX keeps a canonical internal `GxCudaError` and maps both directions:

```rust
pub enum GxCudaError { Success, NotInitialized, OutOfMemory, InvalidValue,
    InvalidHandle, NotSupported, LaunchFailure, IllegalAddress, Transport, Unknown(i32) }
```

- Server replay catches the native `CUresult`, maps to `GxCudaError`, and ships the canonical code (a small `u16`) — never the raw native integer, which could differ by driver version.
- The client maps `GxCudaError` back to whichever ABI the calling shim needs: `libcuda` → `CUresult`, `libcudart` → `cudaError_t`.
- **Transport failures** (vsock drop, server crash) map to `CUDA_ERROR_LAUNCH_FAILED`/`cudaErrorLaunchFailure` for in-flight async work and `CUDA_ERROR_NOT_INITIALIZED` for fresh calls after a dead session — both are codes real apps already handle.
- `cudaGetErrorString`/`cuGetErrorName` are answered *locally* from a static table so no round-trip is needed for diagnostics.

## 16.9 Libraries riding on CUDA: cuBLAS / cuDNN

cuBLAS, cuDNN, cuFFT, cuSPARSE etc. are separate `.so`s that internally call the driver/runtime API. Two strategies:

1. **Passthrough (default for v0.x):** do *not* shim the library at the API level. Run the real `libcublas.so` **on the server**, and shim only its public entry points on the client as thin command verbs (`CublasSgemm{ handle, ... , a: DevPtr, b: DevPtr, c: DevPtr }`). The `DevPtr` arguments carry real server `CUdeviceptr` values validated exactly like kernel-arg pointers (§16.6). This needs per-library codegen but is mechanical (the Build/dist chapter (Ch.28)) and keeps the heavy math on the GPU side with zero extra data movement — the matrices already live in device memory the server allocated.
2. **Naive interception (rejected):** shimming cuBLAS by capturing its *internal* CUDA calls would force every BLAS-internal allocation and launch across the wire — catastrophic for performance.

So the rule is: **libraries are remoted at their own public ABI, not by trapping the CUDA calls underneath them.** Each riding library contributes its **own family of opaque handle types** that become server-authoritative wire `Handle`s (the Handles chapter (Ch.11)) bound to server-side objects: not just `cublasHandle_t` / `cudnnHandle_t` but the per-library descriptor families (`cudnnTensorDescriptor_t`, `cudnnConvolutionDescriptor_t`, cuBLASLt `cublasLtMatmulDesc_t` / `cublasLtMatrixLayout_t`), algorithm/plan handles, and server-allocated **workspace** buffers (each a `DevPtr` into device memory the server owns). Stream association reuses the §16.5 stream queues so BLAS/DNN work orders correctly against raw kernels. This generalizes to OptiX (§16.10) and is the same pattern the OpenCL chapter (Ch.17) uses for OpenCL's BLAS-like libraries.

## 16.10 Relationship to OptiX

OptiX is a host library distributed as a header-only API over a function table fetched via `optixInit`; under the hood it builds acceleration structures and launches ray-generation kernels through the CUDA **driver** API, sharing `CUcontext`, `CUstream`, and `CUdeviceptr`. Consequences for GraftX:

- OptiX **must** be layered on top of this CUDA backend: it consumes the same `CUcontext`/`CUstream` Handles and `DevPtr` (real server `CUdeviceptr`) values and the same stream-ordering and pointer-validation machinery. The OptiX backend in the Tier-3 chapter (Ch.21) will *not* re-implement memory or stream remoting; it depends on §16.5–16.6.
- `optixLaunch` takes a CUDA stream and a device pointer to a "shader binding table" + a launch-params blob — both resolved through the §16.6 region table.
- OptiX modules are PTX compiled by OptiX itself on the server; the PTX blob travels over the bulk plane exactly like §16.4 module images.

Thus the OptiX backend in the Tier-3 chapter (Ch.21) is a *thin* backend: its only novel verbs are pipeline/SBT construction; everything below the API surface reuses the CUDA backend specified here.

## 16.11 Open questions

- **Graph API (`cuGraph*`):** capturing a stream into a graph and relaunching it is a natural fit for remoting (one upload, many cheap replays). Deferred to a later phase; the §16.5 stream queue is the substrate a captured graph would record into.
- **IPC handles (`cuIpcGetMemHandle`):** cross-process device sharing assumes a shared driver; meaningless across the remoting boundary and will be rejected with `NotSupported`.
- **Driver-version negotiation:** `cuDriverGetVersion` returns the *server's* version; apps that branch on it get correct behavior, but a guest compiled for a newer toolkit than the server hosts will see missing entry points — handled by the Hello/Welcome capability handshake (the Protocol chapter (Ch.06)) and the compatibility policy in the Versioning chapter (Ch.29).
