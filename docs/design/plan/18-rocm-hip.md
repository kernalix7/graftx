# 18. ROCm / HIP Backend

This chapter specifies how GraftX will forward AMD GPU compute by remoting the **HIP** portable runtime rather than the lower ROCm stack, how the HIP module/stream/memory model maps onto AMD's native Windows driver across the passthrough boundary, where HIP diverges from the CUDA backend (the CUDA chapter, Ch.16), and how compiled code objects move from the Linux guest to the Windows replay side.

## 18.1 Why HIP, and why not raw ROCm

ROCm is a vertical stack: at the bottom is the kernel driver (`amdgpu` / KFD on Linux, the WDDM kernel-mode driver on Windows), above it the **ROCr** runtime (`libhsa-runtime64.so`, an HSA implementation talking to KFD via ioctls), then **ROCclr** (the common language runtime), then **HIP** (`libamdhip64.so`), and finally libraries (rocBLAS, MIOpen, rocFFT, RCCL). The temptation is to intercept low — at HSA — because that is the narrowest, most stable ABI. We will **not** do that, for four reasons that follow directly from the project priorities (breadth > performance > stability > safety):

1. **HSA is Linux-only as a guest/host contract.** KFD ioctls assume a Linux kernel with `amdgpu` and an IOMMU mapping of doorbell/queue memory into the *local* process. Our physical GPU lives on the **Windows** guest behind PCI passthrough; the Windows AMD driver does not expose KFD. Replaying HSA packets would require us to re-implement the doorbell/AQL queue protocol against a driver that does not speak it. HIP, by contrast, has a fully supported Windows runtime (`amdhip64.dll`) that targets WDDM. Forwarding *at HIP* lets the server replay against a first-class, vendor-maintained Windows runtime.

2. **HIP is the portability layer applications actually link.** PyTorch-ROCm, TensorFlow-ROCm, llama.cpp, Blender's HIP backend, and the ROC libraries all sit on `libamdhip64.so`. Intercepting one ABI (~700 `hip*` symbols) captures the entire ecosystem. Intercepting HSA would force us to additionally re-host ROCclr's scheduling, signal, and code-object logic on our side — enormous surface for no breadth gain.

3. **HIP's surface mirrors CUDA's**, so the CUDA chapter's (Ch.16) encoder/decoder, handle table (the Handles chapter, Ch.11), and stream/event machinery are ~80% reusable. `hipMalloc` ≈ `cuMemAlloc`, `hipModuleLaunchKernel` ≈ `cuLaunchKernel`, `hipStream_t` ≈ `CUstream`. We get a second compute backend for a fraction of the cost.

4. **Stability.** HSA's queue/signal ABI churns across ROCm releases; the HIP runtime API is comparatively frozen and versioned (`HIP_VERSION`). Pinning to HIP reduces the validation surface we must re-verify per driver bump (the Server core chapter, Ch.10).

The cost we accept: HIP hides the HSA queue, so we cannot do the lowest-overhead doorbell-ring remoting; every launch is a runtime call we must serialize. That is the right tradeoff — breadth and a maintained Windows target beat a marginal latency win.

```
Linux guest                                Windows guest (owns GPU)
+-------------------------------+          +------------------------------+
| app -> libamdhip64.so (shim)  |          | graftx-server                |
|   record hip* calls           | vsock    |   decode + validate (Ch.10)  |
|   local handle mirror         |  ctrl    |   |                          |
|   bulk H2D/D2H via ivshmem ----+--------->-+-> amdhip64.dll (native)     |
| <-- results / events ---------<----------+     -> WDDM AMD driver        |
+-------------------------------+ shm bulk  +------------------------------+
        INTERCEPT POINT = HIP                REPLAY TARGET = HIP (Windows)
```

## 18.2 Client shim surface

The shim is a `cdylib` exporting the `hip*` C symbols (symbol-export mechanics in the Client shim chapter, Ch.09) plus a `SONAME` of `libamdhip64.so` so the dynamic linker substitutes us for the real runtime. We additionally provide thin shims for **hipBLAS/hipFFT/MIOpen** that forward to their HIP-runtime calls so library-level work flows through the same path; where a library issues kernels directly we capture those at the HIP launch boundary.

Calls fall into the same three latency classes the CUDA backend defines (the CUDA chapter, Ch.16 §16.5):

| Class | Examples | Remoting policy |
|-------|----------|-----------------|
| Synchronous-blocking | `hipMalloc`, `hipModuleLoad`, `hipMemcpy` (sync), `hipDeviceSynchronize` | round-trip; block caller until server acks |
| Fire-and-forget | `hipLaunchKernel`, `hipMemcpyAsync`, `hipMemsetAsync` | record into stream batch, flush lazily (Ch.13) |
| Local-answerable | `hipGetDeviceCount`, `hipDeviceGetAttribute`, `hipGetLastError` | answer from mirror cached at init |

```rust
/// Per-process HIP shim state (graftx-client).
pub struct HipState {
    devices: Vec<HipDeviceInfo>,      // queried once at first hipInit
    ctx_stack: ThreadLocal<Vec<RemoteCtxId>>, // primary/secondary ctx stack
    streams: HandleTable<HipStream>,  // hipStream_t -> remote handle
    events: HandleTable<HipEvent>,    // hipEvent_t  -> remote handle
    modules: HandleTable<HipModule>,  // hipModule_t -> remote handle + code-object hash
    allocs: AllocTable,               // device ptr range registry (Ch.12)
    last_error: ThreadLocal<hipError_t>,
}

#[repr(C)]
pub struct HipDeviceInfo {
    pub remote_id: u32,
    pub name: [u8; 256],
    pub gcn_arch: GfxArch,            // e.g. Gfx1100, Gfx90a
    pub total_mem: u64,
    pub warp_size: u32,              // "wavefront" 32 or 64
    pub max_threads_per_block: u32,
    pub attrs: HipAttrCache,          // full hipDeviceAttribute_t snapshot
}
```

`hipGetLastError`/`hipPeekAtLastError` are thread-local and **must not** round-trip: HIP defines the error as sticky per-thread until read. The shim mirrors it, updating from the worst error returned by any flushed batch (the Sync chapter's, Ch.13, completion records carry a `hipError_t`). `hipDeviceGetAttribute` is answered from `HipAttrCache`, a full snapshot fetched in one round-trip at `hipInit`.

## 18.3 Stream and event model

HIP streams are FIFO ordered queues; events are markers. This maps cleanly onto the generic submission/sync layer (the Sync chapter, Ch.13), but with HIP-specific rules the validator and encoder must honour:

- **The null/default stream (`hipStreamDefault`, handle `0`)** has legacy-synchronizing semantics unless the app set `hipStreamNonBlocking` or built with per-thread default streams. Work on the null stream synchronizes against all blocking streams. The shim tags stream `0` and the server must enforce the implicit barrier; we will *not* silently treat it as just another stream.
- **`hipStreamWaitEvent` / `hipEventRecord`** become cross-stream dependency edges in the server's DAG (the Sync chapter, Ch.13). We forward the `(stream, event)` pair as handles; the server resolves them against native `hipEvent_t`.
- **`hipStreamAddCallback` / `hipLaunchHostFunc`** schedule *Linux-side* host callbacks. These are dangerous: the host function pointer is meaningless on the server. The shim records a `HostCallbackMarker(u64)` token; when the server completes the preceding work it sends a completion record carrying the token, and the shim invokes the real callback **on the guest**. This keeps the closure execution on the side that owns the address space.

```text
Stream timeline (one hipStream_t), client batches flushed to server:

 client:  malloc  H2D    launchK1  launchK2  eventRecord(e)  D2H(sync)
            |       |        |         |           |            |
 batch:   [ ........ async, coalesced ........ ][ flush-on-sync ]
 server:  alloc -> dma-in -> K1 -> K2 -> signal(e) -> dma-out -> ack
                                              \--> completion record -> client unblocks D2H
```

`hipEventElapsedTime` requires two recorded events' GPU timestamps. We cannot answer locally; the shim flushes and round-trips, returning the server-measured delta. Because this is a known sync point apps call it sparingly (per-iteration timing), so the round-trip cost is acceptable.

## 18.4 Memory model and bulk transfer

HIP's pointer space is the same flat device-address abstraction as CUDA, and the memory chapter (the Memory chapter, Ch.12) machinery applies almost verbatim. Key allocation entry points and their remoting:

| HIP call | Maps to | Bulk path |
|----------|---------|-----------|
| `hipMalloc` | server `hipMalloc`, register range in AllocTable | n/a (no host data) |
| `hipHostMalloc` (pinned) | server pinned alloc + ivshmem-backed staging | yes — host buffer lives in shm so H2D is zero-copy on the guest side |
| `hipMallocManaged` | server `hipMallocManaged` | page-migration faults handled server-side; see below |
| `hipMemcpyHtoD/DtoH` | bulk copy over ivshmem | yes — validated copy into server-private buffer (security, §18.8) |
| `hipMemcpyPeer` | server-side device-to-device | yes — stays on server, no guest traffic |

**Managed (unified) memory** is the hard case. On real HIP, `hipMallocManaged` returns a pointer the *CPU and GPU both fault on*, with the driver migrating pages. Across the remoting boundary the CPU faulting happens on the **Linux guest** but the GPU is on **Windows** — there is no shared page table. We will support managed memory in a **degraded, correct** mode: the shim allocates a guest-side shadow buffer in ivshmem and registers it; on any HIP API sync point (`hipDeviceSynchronize`, `hipStreamSynchronize`, kernel launch reading the range) we reconcile by copying dirty pages in the direction implied by the last writer. This is slower than hardware migration but preserves semantics. The dirty-tracking granularity is the ivshmem page (the Memory chapter, Ch.12); writes are detected via `mprotect`+`SIGSEGV` userfault on the guest shadow. We document this as a coverage-first compromise; raw async page migration is explicitly out of scope at v0.0.0.

```rust
pub enum HipMemKind {
    Device { remote_ptr: u64, len: u64 },
    Pinned { remote_ptr: u64, shm_off: u64, len: u64 }, // shm-backed, zero-copy
    Managed { remote_ptr: u64, shadow: ShmRange, dirty: DirtyBitmap },
}
```

Pinned host memory is the fast path and what well-written ROCm apps use: by placing the pinned buffer directly in the ivshmem region, an `hipMemcpyAsync(HtoD)` becomes "tell the server which shm offset to DMA from" with no extra guest copy. The server still copies into device memory through the native runtime, but the guest→server hop is free.

## 18.5 Code-object transfer

This is the defining problem of the HIP backend. CUDA ships PTX (a virtual ISA) that the *driver* JITs per-GPU; HIP ships **GCN/RDNA code objects** — ELF containers (`.hsaco`, or fat binaries holding multiple `gfxNNNN` slices) that are *already* compiled to a specific architecture. The flow differs by load path:

- **`hipModuleLoad(path)` / `hipModuleLoadData(image)`** — the app hands a code object. The shim must transfer the **bytes** to the server. We hash the image (BLAKE3, see the Handles chapter, Ch.11, caching) and check a server-side module cache by hash before sending; identical modules (common in PyTorch which reloads the same kernels) transfer once.
- **`__hipRegisterFatBinary`** (the implicit static path) — kernels embedded in the app binary via `__attribute__((constructor))` registration. The shim intercepts the registration symbols (`__hipRegisterFatBinary`, `__hipRegisterFunction`, `__hipRegisterVar`) and forwards the fat-binary blob plus the name→stub mapping to the server at load, so subsequent `hipLaunchKernel(stub_ptr, ...)` can be resolved by name.

```rust
/// Sent once per unique code object; server caches by hash.
pub struct ModuleUpload {
    pub hash: [u8; 32],            // BLAKE3 of image bytes
    pub image: BulkRef,            // ivshmem region holding the ELF/fatbin
    pub declared_arch: Vec<GfxArch>, // archs the fatbin claims to contain
}
```

The **architecture-match invariant** is critical and is where HIP bites harder than CUDA. A `.hsaco` compiled for `gfx1030` will not load on a `gfx1100` GPU; HIP returns `hipErrorNoBinaryForGpu`. Because the *guest* application was built expecting whatever GPU the guest thinks it has, but the *real* GPU is whatever sits on the Windows host, the declared archs in the fatbin must include the host GPU's arch. GraftX cannot recompile code objects (we are not shipping a compiler). The mitigation is **honest device advertisement**: the device info we report at `hipInit` (§18.2) is the **real** host GPU's `gcn_arch`, queried from the server. Applications and the HIP runtime select the matching fatbin slice based on what we advertise, so a correctly built fat binary will contain the right slice. If it does not, the server returns the native `hipErrorNoBinaryForGpu` and we propagate it faithfully (the Error/FFI chapter, Ch.24) — we do not fabricate a fake arch, because that would make launches fail later and more confusingly. This is a deliberate tradeoff: we trade "works with mismatched single-arch binaries" for "honest, debuggable failures."

For the JIT-able case — HIP can also carry **LLVM IR / SPIR-V bitcode** in a "code object v5 bundle" that ROCclr finalizes at load — the finalization happens **on the server** against the native runtime, which is exactly what we want: the server's HIP runtime owns the correct target. We forward the bundle bytes unmodified.

## 18.6 Kernel launch path

`hipModuleLaunchKernel` and `hipLaunchKernel` carry a kernel handle, a grid/block geometry, dynamic shared-memory size, a stream, and an **opaque argument buffer**. The argument buffer is a packed struct whose layout the *client compiler* decided; the server cannot reinterpret it without ABI knowledge, so we forward it as raw bytes and let the native runtime apply it to the resolved kernel.

```rust
pub struct LaunchKernel {
    pub module: RemoteHandle,      // resolved hipModule_t / fatbin function
    pub func_name: SmallString,    // for hipModuleGetFunction-style resolution
    pub grid:  [u32; 3],
    pub block: [u32; 3],
    pub shared_mem_bytes: u32,
    pub stream: RemoteHandle,
    pub args: BulkRef,             // packed arg blob, validated for size only
}
```

Validation (the Server core chapter, Ch.10) checks: grid/block within device limits from the attr cache; `shared_mem_bytes <= maxSharedMemoryPerBlock`; `args` length matches the kernel's declared signature size *if known* (from `__hipRegisterFunction` metadata) — otherwise we bound it to a configured maximum and let the native runtime reject mismatches. We never dereference pointers inside `args` on the server beyond confirming any device pointers they contain fall within ranges in the server's AllocTable.

## 18.7 Differences from the CUDA backend (the CUDA chapter, Ch.16)

The HIP backend reuses the CUDA chapter's (Ch.16) structure but must diverge here:

| Aspect | CUDA (Ch.16) | HIP (this chapter) |
|--------|--------------|--------------------|
| Shipped code | PTX virtual ISA, driver-JIT | Pre-compiled GCN/RDNA `.hsaco` fatbin (or SPIR-V/IR bundle) |
| Arch flexibility | high — PTX retargets at load | low — must contain host-arch slice |
| Warp/wave size | 32 | 32 (RDNA) or 64 (CDNA `gfx9xx`) — must report real value |
| Error type | `CUresult` / `cudaError_t` (two APIs) | single `hipError_t` |
| Context model | explicit `CUcontext` stack (driver API) | primary-context centric; explicit ctx rarely used |
| Streams | identical FIFO + null-stream rules | identical (HIP copied the model) |
| Library coverage | cuBLAS/cuDNN/cuFFT | hipBLAS/MIOpen/hipFFT |

The wavefront-size divergence matters: kernels query `warpSize` and size their reductions accordingly. Reporting `32` for a CDNA `gfx90a` (which is `64`) would silently corrupt results. The attr cache (§18.2) therefore carries the **server-measured** `warpSize`, never a hardcoded constant.

A shared concern with CUDA: HIP can be built as the **HIP-over-CUDA NVIDIA backend** (`HIP_PLATFORM=nvidia`). If the real GPU is NVIDIA, the Windows server's HIP runtime forwards to CUDA. GraftX is agnostic — we forward HIP calls; whichever native runtime the server links handles them. The shim does not need to know which platform backs it.

## 18.8 Security and validation

The server replays an untrusted HIP stream against a native driver, so the validator (the Server core chapter, Ch.10) enforces:

- **Code-object scanning is impractical** (GCN ELF is arbitrary machine code), so the defense is containment: the server runs sandboxed (the Security chapter, Ch.23) and every device pointer in launch args / memcpy is range-checked against the AllocTable. A malformed kernel that reads out of bounds is the driver's/hardware's bounds problem; we cannot validate kernel semantics, only API-level arguments.
- **Bulk H2D data** lands in the ivshmem region the guest can still write. Per the project security model (the Security chapter, Ch.23), the server **copies validated bytes into server-private memory** before handing them to the native runtime, defeating TOCTOU rewrites of the shared buffer (the Memory chapter, Ch.12).
- **Resource quotas**: per-session caps on total `hipMalloc` bytes, live module count, and in-flight stream depth, enforced before forwarding, to bound a hostile guest (backpressure in the Sync chapter, Ch.13).
- **Host-callback tokens** (§18.3) are validated as opaque `u64`s the shim issued; the server never executes guest code, only echoes the token back.

## 18.9 Testing and phasing

Phase 1: `hipInit`/device query, `hipMalloc`/`hipMemcpy`, `hipModuleLoad` + `hipModuleLaunchKernel` for a hand-written `.hsaco` (vector-add) — proves the code-object pipe and arch-match path end to end. Phase 2: static fatbin path (`__hipRegister*`), streams, events, async copies. Phase 3: hipBLAS/MIOpen forwarding and a real workload (a small llama.cpp HIP build). Conformance is checked by running each phase's binary against a local native HIP and diffing device outputs bit-exactly for integer kernels and within tolerance for FP; the decoder is fuzzed with malformed `LaunchKernel`/`ModuleUpload` records to prove no `unwrap`/UB on the replay side (the Testing chapter, Ch.26).
