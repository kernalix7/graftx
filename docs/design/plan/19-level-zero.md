# 19. Intel Level Zero / oneAPI Backend

This chapter specifies how GraftX will remote the Intel Level Zero (`libze_loader.so`) API — its driver/device/context model, command-list/queue submission, Unified Shared Memory, module/kernel dispatch, and event-based synchronization — and how that backend underpins SYCL and the wider oneAPI stack on Intel GPUs under PCI passthrough.

## 19.1 Why Level Zero, and where it sits

Level Zero ("L0", `ze_*`) is Intel's low-level, explicit GPU API — the Vulkan-equivalent of the compute world for Intel Arc / Data Center GPU Max / integrated Xe hardware. It is the *bottom* of the oneAPI stack: the DPC++/SYCL runtime, oneMKL, oneDNN, and OpenMP-offload all funnel down to `ze_*` calls when they pick the Level Zero backend (the alternative SYCL backend on Intel is OpenCL, the OpenCL chapter (Ch.17)). Remoting L0 well therefore buys us SYCL and a large fraction of oneAPI for free, which is exactly the breadth-first bet the project priorities reward.

The shape of L0 is deliberately close to Vulkan (the Vulkan chapter (Ch.14), in the broader Vulkan-spine sense the project uses): explicit objects, application-managed lifetimes, explicit command lists recorded once and replayed, explicit synchronization via events and fences, and no hidden global state machine of the OpenGL kind (the OpenGL/GLES/EGL/GLX chapter (Ch.15)). That makes L0 one of the *easier* compute APIs to remote: almost everything is an opaque handle plus a POD descriptor struct, and the call graph is record-then-execute rather than query-then-branch. The hard parts are USM (host-visible device pointers that an unmodified app dereferences directly) and the fact that the L0 loader does *driver discovery* the client must spoof.

```
Linux guest                                   Windows guest
+----------------------------------+          +------------------------------+
| app / SYCL rt / oneMKL           |          | graftx-server                |
|   -> libze_loader.so (our shim)  | vsock    |   decode + validate (Ch.10)  |
|      record ze* commands         | (ctrl)   |   |                          |
|      USM ptr shadow              |          |   +-> native Intel L0 driver |
|      ivshmem for H2D/D2H bulk -->|  shm     |        (ze_*, real Xe GPU)   |
| <-- events / readback -----------<----------+                              |
+----------------------------------+          +------------------------------+
```

Targeted surface: **Level Zero 1.x core** (`ze_api.h`) plus the most-used experimental/extension entry points an SYCL workload actually hits — `ze` core, `zes` (Sysman, throttled to read-only metrics, see §19.9), and the `zet` tools API only as far as device properties. Targeted spec baseline is L0 spec **1.9+**; the shim advertises a conservative `ZE_API_VERSION` (§19.2) and degrades gracefully (the Error/FFI chapter (Ch.24)) on calls outside the supported set.

## 19.2 Loader interception and driver discovery

Unlike CUDA (one vendor library) the L0 client side is a *loader* (`libze_loader.so`) that enumerates installable client drivers (ICDs) via `zeInit` → `zeDriverGet`. The shim will replace `libze_loader.so` itself and export the full `ze*` C ABI, so it sits where the real loader would and never loads a local ICD (there is no Intel GPU on the Linux side). The single synthetic driver we present corresponds to the real driver on the Windows host.

```rust
// graftx-client: exported C entry points (subset)
#[no_mangle]
pub unsafe extern "C" fn zeInit(flags: ze_init_flags_t) -> ze_result_t { /* ... */ }

#[no_mangle]
pub unsafe extern "C" fn zeDriverGet(
    p_count: *mut u32,
    ph_drivers: *mut ze_driver_handle_t,
) -> ze_result_t { /* return 1 synthetic driver */ }
```

`zeInit` is the moment we open the session: the shim performs the vsock handshake (the Transport chapter (Ch.08)), negotiates protocol version, asks the server to call the real `zeInit`/`zeDriverGet`/`zeDeviceGet` once, and caches the resulting topology. The cache matters because SYCL runtimes call discovery repeatedly. We snapshot at init:

```rust
pub struct L0Topology {
    drivers: Vec<DriverInfo>,            // usually 1
    devices: Vec<DeviceSnapshot>,        // root devices
    sub_devices: HashMap<DevId, Vec<DevId>>, // tiles
}

pub struct DeviceSnapshot {
    server_handle: u64,                  // opaque ze_device_handle_t on server
    props: ze_device_properties_t,       // name, type, vendorId, uuid, flags
    compute_props: ze_device_compute_properties_t,
    memory_props: Vec<ze_device_memory_properties_t>,
    cache_props: Vec<ze_device_cache_properties_t>,
    image_props: ze_device_image_properties_t,
    module_props: ze_device_module_properties_t, // SPIR-V support, fp64/fp16, etc.
}
```

Every `zeDeviceGetProperties`, `zeDeviceGetComputeProperties`, etc. is then answered **from the snapshot with zero round-trips** — the L0 analog of the OpenGL state mirror (§15.2). The UUID must be returned byte-identical to the host so SYCL device caching and binary-cache lookups stay coherent across runs.

Version negotiation is a decision the shim makes once:

| Host driver `ZE_API_VERSION` | Shim advertises | Calls outside set |
|------------------------------|-----------------|-------------------|
| ≥ negotiated max we support  | our supported max | `ZE_RESULT_ERROR_UNSUPPORTED_VERSION` |
| < our max                    | host's version  | host-native error passthrough |
| extension present on host    | mirror extension | forwarded |
| extension absent on host     | hide from `zeDriverGetExtensionProperties` | n/a |

## 19.3 Handle model and the object table

L0 hands the app opaque pointers (`ze_context_handle_t`, `ze_command_list_handle_t`, `ze_kernel_handle_t`, …). Handles are server-authoritative (D1): the server mints the 64-bit wire Handle (D5 layout — kind in the top 8 bits, generation, slot index) and the shim presents that as the opaque `ze_*_handle_t` to the app, mapping it through the shared handle table (the Handles chapter (Ch.11)); a real native host handle never crosses the guest boundary. For the async-creation entry points the shim may hold a purely local provisional proxy token, reconciled when the server's real Handle arrives (never put on the wire as authority). The descriptor structs are POD and serialize trivially; we validate `stype`/`pNext` chains on decode (the Server core chapter (Ch.10)) and *reject* unknown `pNext` extension nodes rather than blindly forwarding them (the sandbox policy is owned by the Security chapter (Ch.23)).

```rust
#[repr(transparent)]
pub struct L0Handle(u64);

pub enum L0Object {
    Driver,
    Device(DevId),
    Context { devices: SmallVec<[DevId; 2]> },
    CommandQueue { ctx: L0Handle, ordinal: u32, mode: QueueMode },
    CommandList  { ctx: L0Handle, immediate: bool, recording: bool },
    EventPool    { count: u32, flags: ze_event_pool_flags_t },
    Event        { pool: L0Handle, index: u32 },
    Fence        { queue: L0Handle },
    Module       { ctx: L0Handle, format: ModuleFormat },
    Kernel       { module: L0Handle, name: CString, group: [u32; 3] },
    Sampler, Image, /* ... */
}
```

Lifetime is reference-counted on both sides; `zeXxxDestroy` decrements and the server defers the real destroy until no in-flight command list references the object (the Handles chapter (Ch.11) covers the deferred-destroy queue that prevents use-after-free when an untrusted stream destroys an object that a not-yet-replayed command still uses).

## 19.4 Command lists, queues, and the two submission modes

This is the heart of the backend. L0 has two flavors of command list, and they remote very differently:

* **Standard command list** — recorded by a sequence of `zeCommandListAppend*` calls, closed with `zeCommandListClose`, then submitted with `zeCommandQueueExecuteCommandLists`. This is *record-then-replay*, which is ideal for batching: the shim accumulates every append into a single protocol batch and ships nothing until `Close`/`Execute`. One round-trip per submission instead of one per append.
* **Immediate command list** — created with a queue descriptor; every `Append*` executes "immediately" (asynchronously) as if appended-and-executed. SYCL's in-order queues use these heavily. We cannot make each append a synchronous round-trip without destroying throughput, so the shim treats an immediate list as an *implicit auto-flushing batch*: appends are coalesced into a small ring and flushed (a) when an event the app might wait on is signaled, (b) at an explicit synchronize, or (c) when the ring hits a byte/count watermark.

```
Standard list                         Immediate list (auto-flush)
 Append k0  ┐                          Append k0 ─┐ coalesce
 Append k1  ├ buffered (0 RT)          Append k1 ─┤
 Append memcpy ┘                       Append memcpy ─┘
 Close      ─ buffer                   (watermark hit) ──► flush batch (1 RT)
 Execute    ──► 1 round-trip           Append k2 ─┐
                                       Synchronize ──► flush + wait (1 RT)
```

The append opcodes we must cover, in rough frequency order:

| Append op | Server action | Bulk plane? |
|-----------|---------------|-------------|
| `AppendLaunchKernel` | record kernel + group dims + args snapshot | no (args inline) |
| `AppendMemoryCopy` | H2D / D2H / D2D copy | yes for H2D/D2H |
| `AppendMemoryFill` | pattern fill | small pattern inline |
| `AppendBarrier` | scoped barrier, event deps | no |
| `AppendSignalEvent` / `AppendWaitOnEvents` | event graph edges | no |
| `AppendMemoryRangesBarrier` | cache/coherency hint | no |
| `AppendImageCopy*` | image blit | yes |

Kernel argument capture deserves care: `zeKernelSetArgumentValue(kernel, idx, size, pArgValue)` is called *before* append and mutates kernel state. Because the same kernel handle can be re-armed and re-launched with different args, the shim **snapshots all bound arguments at append time** into the command record, rather than referencing live kernel state — otherwise a later `SetArgumentValue` would retroactively corrupt an already-recorded launch. USM-pointer arguments are translated at snapshot time (§19.5).

```rust
struct LaunchRecord {
    kernel: L0Handle,
    group: [u32; 3],            // from zeKernelSetGroupSize
    grid:  ze_group_count_t,    // from the launch call
    args:  Vec<ArgSnapshot>,    // captured, not referenced
    signal_event: Option<L0Handle>,
    wait_events:  SmallVec<[L0Handle; 4]>,
}

enum ArgSnapshot {
    Scalar(SmallVec<[u8; 16]>),     // by-value POD
    UsmPtr { alloc: AllocId, off: u64 }, // translated device ptr
    Local(u64),                      // shared-local-memory size (null ptr)
    SamplerOrImage(L0Handle),
}
```

## 19.5 Memory: USM and the pointer-translation problem

L0 exposes three allocation kinds via `zeMemAllocDevice`, `zeMemAllocHost`, `zeMemAllocShared`. The hard one is **host** and **shared** USM: the app gets back a raw `void*` that it may dereference *directly on the CPU* and also pass to a kernel. Device USM is easier — its pointer is opaque to the host CPU and only meaningful to the GPU, so we never need to back it with real guest memory.

Strategy, building on the zero-copy bulk plane (the Memory chapter (Ch.12)):

* **Device USM** — allocate on the server, return a *synthetic* pointer to the client that is never dereferenced on the Linux side. The shim records `AllocId → {server_ptr, size, kind=Device}`. Kernel args carrying a device pointer are translated client→server by table lookup; the server validates the pointer lies within a known allocation before forwarding (the Server core chapter (Ch.10) validation: no arbitrary pointers reach the native driver).
* **Host / Shared USM** — back the allocation with an **ivshmem slice** (the Memory chapter (Ch.12) `ShmSlice`) so the guest CPU genuinely reads/writes it. The returned `void*` is the mapped BAR address. The server allocates a real host/shared USM region and the bulk plane keeps them in sync: writes are flushed via a `DirtyTracker` at the next GPU-visible sync point, exactly as in the Memory chapter (Ch.12) (we inherit the documented relaxed-coherency-at-sync semantics — full CPU/GPU coherency for shared USM cannot be honored at memory speed across guests).

```rust
pub struct UsmAlloc {
    id: AllocId,
    kind: UsmKind,            // Device | Host | Shared
    size: usize,
    align: usize,
    server_ptr: u64,          // real ptr in native driver address space
    shm: Option<ShmSlice>,    // Some for Host/Shared, None for Device
    dirty: Option<DirtyTracker>,
}
```

Pointer translation must handle *interior pointers*: kernels frequently receive `base + offset`. On `SetArgumentValue` the shim looks up which allocation the pointer falls inside (an interval map keyed on synthetic base address) and emits `UsmPtr { alloc, off }`. Out-of-range pointers are a validation failure, not a forward.

```rust
fn translate_usm(p: *const c_void) -> Result<(AllocId, u64), L0Error> {
    let addr = p as u64;
    match ALLOC_INTERVALS.range(..=addr).next_back() {
        Some((base, a)) if addr < base + a.size as u64 => Ok((a.id, addr - base)),
        _ => Err(L0Error::InvalidUsmPointer(addr)),
    }
}
```

`zeMemGetAllocProperties` / `zeMemGetAddressRange` are answered from the client table without a round-trip. `zeMemGetIpcHandle` (cross-process sharing) is **out of scope initially** — it implies a second guest process, which the paired-channel model does not span; the shim returns `ZE_RESULT_ERROR_UNSUPPORTED_FEATURE`.

## 19.6 Modules and kernels (SPIR-V)

`zeModuleCreate` takes a SPIR-V (or native-binary) blob plus build flags. The blob can be large (megabytes) so it travels on the bulk plane; the build itself (JIT) runs on the server against the native driver. Build log retrieval (`zeModuleBuildLogGetString`) is supported because SYCL surfaces compile errors to the user.

```rust
struct ModuleCreate {
    ctx: L0Handle,
    format: ModuleFormat,        // IL_SPIRV | NATIVE
    input: ShmSlice,             // the blob, bulk-plane
    build_flags: CString,
    constants: Vec<SpecConstant>,// zeModuleCreate specialization constants
}
```

The server **validates the SPIR-V**: it is an untrusted blob handed to the JIT. Minimum validation is structural (magic, version, bounded sizes) plus rejecting capabilities outside an allowlist; the goal per the Security chapter (Ch.23) model is to not feed pathological IL straight into the driver compiler. Native-format modules are riskier (opaque binary) and gated behind a session policy flag, defaulting off.

`zeModuleGetKernelNames`, `zeKernelGetProperties`, and the group-size suggestion call `zeKernelSuggestGroupSize` are forwarded once and cached per kernel; `zeKernelSetGroupSize` updates client-side state consumed at append (§19.4).

## 19.7 Events, fences, and synchronization

L0 separates **events** (fine-grained, app-visible, signaled by appended ops or hosts, used for kernel-to-kernel deps) from **fences** (coarse, one per `ExecuteCommandLists`, host-waitable). Both plug into the unified `Timeline`/`seq` model of the Sync chapter (Ch.13) (`seq` here is the per-session monotonic ordering/fence sequence of D3, not a correlation id):

* A command queue maps to one Sync-chapter (Ch.13) stream; `ExecuteCommandLists` enqueues with a monotonic `seq`.
* `zeEventHostSynchronize(event, timeout)` and `zeFenceHostSynchronize` are blocking host waits → they flush the pending batch and round-trip, blocking until the server reports the `seq` reached (the Sync chapter (Ch.13) wait protocol, with timeout honored).
* `zeEventQueryStatus` is the *non-blocking* poll SYCL spins on. Servicing every poll with a round-trip would melt the link, so the shim keeps a **client-side event-status cache** updated by the completion notifications the server already streams back for sync (the Sync chapter (Ch.13)). A poll reads the cache; only a stale-and-overdue cache triggers a lazy probe.

```rust
fn ze_event_query_status(&self, e: L0Handle) -> ze_result_t {
    match self.event_cache.get(e) {
        EventState::Signaled => ZE_RESULT_SUCCESS,
        EventState::Pending if self.cache_is_fresh(e) => ZE_RESULT_NOT_READY,
        _ => self.lazy_probe(e), // rare: ask server, refresh cache
    }
}
```

`AppendWaitOnEvents` / `AppendSignalEvent` become dependency edges in the server's per-stream scheduler. Cross-queue waits where the producing queue has not yet flushed are resolved by forcing a flush of the producer batch, mirroring the cross-stream rule in the Sync chapter (Ch.13). Timestamp events (`ZE_EVENT_POOL_FLAG_KERNEL_TIMESTAMP`) carry GPU clock values back in the completion notification; we translate the host GPU timebase to the client using the device timestamp frequency captured at discovery, so SYCL profiling reports plausible numbers.

## 19.8 Relationship to SYCL / DPC++

SYCL is *not* a separate wire protocol in GraftX — the Intel DPC++ runtime is just another consumer of `libze_loader.so`. When the user’s app is built with DPC++ and runs with `ONEAPI_DEVICE_SELECTOR=level_zero:*`, every SYCL `queue`, `buffer`, `malloc_shared`, and kernel submit decomposes into the `ze*` calls this chapter already remotes. The implications:

* **SYCL in-order queues** → L0 immediate command lists (§19.4 auto-flush path). This is the dominant SYCL pattern and the one whose latency our coalescing most affects.
* **SYCL buffers** with accessors → device USM + `AppendMemoryCopy` for the host-staging, or shared USM. Our USM strategy (§19.5) covers both.
* **SYCL kernel bundles** → modules (§19.6); the SYCL ahead-of-time SPIR-V is exactly the blob we JIT on the server, and the SYCL on-disk kernel cache keys on the device UUID we must echo verbatim (§19.2).
* The OpenCL SYCL backend is handled by the OpenCL chapter (Ch.17), not here; a device-selector that picks `opencl:*` routes through that backend. Documenting the L0-vs-OpenCL backend split (and that we default-advertise L0 for Intel) is a coverage note for the OpenCL chapter (Ch.17).

No SYCL-specific shim is planned for v1; "SYCL support" is an emergent property of a correct L0 backend, which is why L0 ranks high in the breadth-first ordering.

## 19.9 Intel-specific surface, Sysman, and tradeoffs

Beyond core compute, Intel devices expose **Sysman** (`zes_*`): power, frequency, temperature, memory bandwidth, RAS counters, throttling. GraftX will forward **read-only** Sysman queries (metrics SYCL/oneMKL sometimes read for tuning) and **reject control operations** (`zesDeviceReset`, frequency overclock, ECC toggles) under the sandbox policy — an untrusted stream must not reset or re-clock the host GPU. Multi-tile devices (Data Center GPU Max) expose sub-devices; we mirror the tile topology (§19.2) so workloads that pin to a tile behave, but tile affinity is advisory across the remote boundary.

Key tradeoffs recorded for this backend:

| Decision | Choice | Cost / risk |
|----------|--------|-------------|
| Immediate list latency | coalesce + auto-flush | added latency vs true immediacy; mitigated by event-driven flush |
| Shared USM coherency | flush-at-sync (Ch.12) | not bit-exact CPU/GPU coherent; documented limitation |
| SPIR-V trust | structural + capability allowlist validate | rejects some exotic-but-valid IL; native binaries gated off |
| IPC / cross-process USM | unsupported v1 | blocks multi-process SYCL; rare in target workloads |
| Sysman control | read-only only | tuning apps that re-clock will see failures (intentional) |
| Discovery | snapshot once, answer locally | stale if host hot-plugs a GPU (not expected under passthrough) |

The net position: Level Zero is structurally one of the cleanest compute APIs to remote (explicit, record-replay, no GL-style hidden state), the two genuinely hard problems — host/shared USM pointer fidelity and immediate-list latency — are both solved by mechanisms GraftX already builds for other backends (the Memory chapter (Ch.12) and the Sync chapter (Ch.13)), and a correct L0 backend delivers SYCL and much of oneAPI as a side effect, making it a high-leverage target under the project's breadth-first priorities.
