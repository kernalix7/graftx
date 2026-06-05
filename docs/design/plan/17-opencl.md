# 17. OpenCL Backend

This chapter specifies how GraftX will remote the OpenCL runtime API: the platform/device/context/queue object model, program build from source and SPIR-V, kernel argument marshalling and NDRange enqueue, buffer/image memory objects, the event/dependency graph, cross-vendor quirks, and the choice between a native server-side ICD and a funnel through Vulkan via clvk/clspv.

## 17.1 Scope and shape of the problem

OpenCL is, like Vulkan (the Vulkan chapter, Ch. 14) and unlike classic GL (the OpenGL/GLES/EGL/GLX chapter, Ch. 15), an *explicit-object* API: almost every piece of state lives in a named runtime object (`cl_context`, `cl_command_queue`, `cl_mem`, `cl_program`, `cl_kernel`, `cl_event`, `cl_sampler`). That is good news for remoting — objects are created once and referenced by handle, so we serialize their creation and then forward enqueue calls that are themselves small. The hard parts are different from GL:

1. **Handles are opaque pointers** (`cl_mem` is a `struct _cl_mem*`). Handles are server-authoritative (D1): the server mints the wire Handle and the shim never puts an invented handle on the wire. Because applications dereference-compare CL objects and store them in hash maps, the shim still hands the app a *local* pointer — a purely client-side proxy token, never sent as authority — and maps that pointer to the server's real handle, reconciling deterministically when an async/deferred reply delivers the server handle.
2. **The runtime is synchronous-by-default but pipelined.** `clEnqueue*` returns immediately with an event; correctness depends on the event dependency graph, not call order across queues.
3. **Reference counting** (`clRetain*`/`clRelease*`) is part of the contract and must be mirrored exactly or we leak or use-after-free server objects.
4. **Program build is the heavy operation** and has two ingestion paths (OpenCL C source vs SPIR-V/IR) plus an opaque-binary path.

We target **OpenCL 1.2 as the conformance floor and 2.x/3.0 as the feature ceiling**, advertising 3.0 with optional features gated by what the server backend actually supports (3.0 makes most 2.x features optional, which fits breadth-over-depth). The shim ships as part of `graftx-client`, exporting the full `cl*` C ABI and registering as an ICD so the Khronos ICD loader (`libOpenCL.so`) routes to us; the replay side lives in `graftx-server`.

```
Linux guest                                   Windows guest
+-------------------------------+             +------------------------------+
| app -> ICD loader -> our shim |             | graftx-server                |
|   handle map + refcount mirror| vsock ctrl  |   decode + validate (Ch.10/23)|
|   |                           |------------>+--> cl object table           |
|   +-- enqueue encoder --------+   ivshmem   |     +-- Backend N (native ICD)|
|        bulk buf/img (shm) ----+------------>+     +-- Backend V (clvk/Vulkan)|
| <-- events / map-back --------+<------------+         -> Ch.14 Vulkan replay|
+-------------------------------+             +------------------------------+
```

## 17.2 The handle and reference-count mirror

Every OpenCL handle is a pointer-width opaque value. GraftX will not pass raw server pointers to the guest — they are meaningless across the address-space boundary and leak server layout. The server mints the authoritative wire Handle (D1); the shim wraps each one in a stable local proxy pointer for the app and keeps a bidirectional table. The proxy pointer is never sent back as authority — the wire always carries the server's real handle.

```rust
/// One entry per live OpenCL object. The pointer we hand the app is
/// `&entry as *const _ as cl_mem` (stable for the object's lifetime).
#[repr(C)]
pub struct ClObject {
    kind: ClKind,            // Platform|Device|Context|Queue|Mem|Program|Kernel|Event|Sampler
    server_handle: u64,      // opaque id on the replay side
    refcount: AtomicU32,     // mirrors clRetain/clRelease (CL_*_REFERENCE_COUNT)
    parent: u64,             // owning context/program id, for scope validation
    info: ClInfoCache,       // cached CL_*_INFO answers (see §17.7)
}

pub struct HandleTable {
    // Box keeps the address stable so the raw pointer we return stays valid.
    objs: SlabMap<u64, Box<ClObject>>,   // server_handle -> object
    by_ptr: HashMap<usize, u64>,         // returned ptr -> server_handle (validation)
}
```

Reference counting is mirrored locally so `clGetMemObjectInfo(CL_MEM_REFERENCE_COUNT)` answers without a round-trip and so we can detect over-release. The rule:

- `clRetainX` increments `refcount` locally and **defers** the server retain — the server already holds a reference for as long as we have not released to zero, so we only send a `Release` op to the server when the *local* count crosses 1→0. This collapses retain/release churn (common in C++ wrappers like `cl2.hpp`) into zero round-trips for the steady state.
- On 1→0 we enqueue a `ReleaseObject{server_handle}` op into the batch; the actual server free happens lazily at flush, after any in-flight enqueues referencing it have an event dependency satisfied.
- Over-release (release at count 0) is a client-side `CL_INVALID_*` returned immediately, never forwarded — matches §17.9 error policy.

`cl_callback` registrations (`clSetEventCallback`, `clSetMemObjectDestructorCallback`, context error callbacks) keep the function pointer **client-side**; the server signals a "callback fired for event E, status S" control message and the shim invokes the app's callback on a dedicated callback thread, preserving the requirement that callbacks may call back into CL.

## 17.3 Platform, device, context, and queue

Platform/device enumeration is resolved once. At `clGetPlatformIDs` the shim performs a single round-trip that returns the server's platform list *as filtered by GraftX policy* — GraftX presents itself as one platform ("GraftX Remote OpenCL") whose devices are the server GPUs exposed to this session, regardless of how many vendor ICDs the server has. This gives a stable, vendor-neutral device list and lets us hide devices the session is not entitled to.

```rust
pub struct DeviceDesc {
    server_handle: u64,
    dtype: cl_device_type,            // GPU|CPU|ACCELERATOR|CUSTOM
    info: DeviceInfoBlob,             // ALL CL_DEVICE_* queried in one shot
    backend: Backend,                 // N (native) | V (clvk)
}
```

All `CL_DEVICE_*` info is fetched in **one batched round-trip per device** at enumeration and cached forever (these are immutable), so the hundreds of `clGetDeviceInfo` calls a framework makes at startup cost zero round-trips after the first. `clCreateContext`/`clCreateContextFromType` create a server context bound to those devices. Command queues map 1:1 to server queues; `CL_QUEUE_PROPERTIES` (out-of-order, profiling) is forwarded verbatim. OpenCL 2.0 `clCreateCommandQueueWithProperties` and on-device queues are forwarded but on-device (device-side enqueue) is gated behind a capability bit because Backend V cannot express it (see §17.10 decision table).

Out-of-order queues matter for the replay loop: an in-order queue lets the server execute the batch in order with no per-op event bookkeeping; an out-of-order queue forces the server to honor the explicit wait-list (`event_wait_list`) per enqueue. The shim tags each enqueue op with its queue's ordering mode so the server picks the cheap path when possible.

## 17.4 Program build: source and SPIR-V

`clCreateProgramWithSource`, `clCreateProgramWithBinary`, `clCreateProgramWithIL` (SPIR-V), and the build/compile/link trio are the heaviest operations and the main correctness surface.

```rust
pub enum ProgramSource {
    Clc { sources: Vec<BulkRef> },        // OpenCL C text, large -> ivshmem
    Il  { spirv: BulkRef },               // SPIR-V or other IL
    Binary { per_device: Vec<BulkRef>,    // opaque device binaries
             cookie: ServerIdentity },    // see below
}
```

Source and SPIR-V blobs travel over the **bulk path** (ivshmem; the Memory chapter, Ch. 12) when above the 4 KiB inline-vs-bulk threshold (D7); small kernels inline into the control batch. Build is a deliberate round-trip: `clBuildProgram` flushes and waits, because the app immediately queries build status/logs and creates kernels from the result. We accept this latency — builds are rare relative to enqueues.

The two ingestion paths diverge by backend:

| Path | Backend N (native ICD) | Backend V (clvk + clspv) |
|------|------------------------|--------------------------|
| `WithSource` (OpenCL C) | handed to vendor CL compiler | clspv compiles OpenCL C → SPIR-V → Vulkan |
| `WithIL` (SPIR-V) | vendor `clCreateProgramWithIL` | SPIR-V validated, fed to clvk |
| `WithBinary` | vendor binary path | reject unless cookie matches |

**Binary programs are server-specific.** `clGetProgramInfo(CL_PROGRAM_BINARIES)` returns a blob produced by the server's compiler for the server's device. The shim tags every binary it hands out with a `ServerIdentity` cookie (server build id + device uuid). A later `clCreateProgramWithBinary` whose cookie does not match the current server is rejected with `CL_INVALID_BINARY` *for that device*, mirroring how vendor runtimes reject foreign binaries — apps that cache binaries fall back to source rebuild. SPIR-V is portable across our backends, so `clCreateProgramWithIL` is the preferred caching format and we steer toolchains toward it where we can influence them.

Separate compilation (`clCompileProgram` + `clLinkProgram`) is forwarded as two ops; embedded headers in `clCompileProgram` (`input_headers`/`header_include_names`) are bundled into the compile op as a small name→BulkRef map. Build options strings are forwarded verbatim; the validator (the Server core and Security chapters, Ch. 10/23) scrubs `-I`/`-cl-` flags that could reach the server filesystem (`-I` path includes are stripped because the server has no view of the guest FS).

To avoid recompiling identical kernels every run, the server maintains a **build cache keyed by `hash(IL_or_source + build_options + device_uuid)`**. The shim also caches the resulting program-binary cookie client-side so a re-`clBuildProgram` of byte-identical source short-circuits to a cache hit confirmation (one tiny round-trip, no compile).

## 17.5 Kernels, arguments, and NDRange enqueue

`clCreateKernel`/`clCreateKernelsInProgram` create server kernel objects. The interesting work is `clSetKernelArg`, which is **stateful**: it stores an argument on the kernel object and is not thread-safe across concurrent enqueues of the same kernel. GraftX preserves this by buffering args client-side and shipping them *with* the enqueue, so the server sets args and enqueues atomically.

```rust
pub enum KernelArg {
    Mem(u64),                 // cl_mem server handle (buffer/image/pipe)
    Sampler(u64),
    Local { bytes: usize },   // __local size, no data
    Svm(u64),                 // SVM pointer handle (2.0)
    Value(SmallVec<[u8; 32]>),// POD by value, copied inline
}

pub struct EnqueueKernel {
    queue: u64,
    kernel: u64,
    args: Vec<(u32, KernelArg)>,   // index -> value, full arg snapshot
    work_dim: u32,
    global_offset: [usize; 3],
    global_size: [usize; 3],
    local_size: Option<[usize; 3]>,
    wait_list: Vec<u64>,           // event server handles
    want_event: bool,
}
```

Key behaviors:

- **Arg snapshotting.** The shim keeps a per-kernel arg array mirror; each `clSetKernelArg` updates it locally (zero round-trips). At `clEnqueueNDRangeKernel` it serializes the full current arg set into the enqueue op. This makes the kernel object safe to re-arg for the next enqueue immediately, matching the spec's "the implementation may copy the argument" allowance while being strictly correct for the common set-then-enqueue loop.
- **By-value args** (`Value`) are copied inline; the validator checks `arg_size` against the kernel's introspected signature (from program reflection) to reject overflow.
- **`cl_mem` args** are translated client→server handles before serialization; an unknown or wrong-context handle is a local `CL_INVALID_MEM_OBJECT`.
- **NDRange validation:** `work_dim ∈ 1..=3`, `local_size` divides `global_size` per dimension, total local work-items ≤ `CL_DEVICE_MAX_WORK_GROUP_SIZE` (from cached device info) — all checked client-side so a malformed launch never reaches the driver.

The returned `cl_event` is a freshly minted client handle pre-bound to a server event id the server promises to produce; the shim does **not** wait. This is what keeps enqueue cheap: a kernel launch is one batched op with no round-trip, and dependent enqueues reference the event by id.

## 17.6 Memory objects: buffers, images, maps, SVM

`clCreateBuffer`, `clCreateImage`, sub-buffers, and pipes (2.0) create server `cl_mem` objects. The host-pointer flags drive data flow:

| Flag combination | Strategy |
|------------------|----------|
| `CL_MEM_COPY_HOST_PTR` | snapshot guest data into bulk region at create, copy server-side |
| `CL_MEM_USE_HOST_PTR` | shim owns a shadow; writes tracked, synced on map/enqueue (no true zero-copy across the boundary) |
| `CL_MEM_ALLOC_HOST_PTR` | server allocates; map returns a guest-visible bulk slot |
| none / `READ_WRITE` | server-private allocation, no initial copy |

`USE_HOST_PTR` cannot be honored literally — the server has no access to guest memory — so GraftX downgrades it to a shadow-copy model: the shim treats the user pointer as the authoritative host copy and synchronizes it across `clEnqueueMap/Unmap` and read/write enqueues. This is a documented relaxation (security note: per project policy the server *must* copy validated data into server-private memory anyway; true guest memory pinning would require hypervisor write-revocation we do not have).

`clEnqueueReadBuffer`/`WriteBuffer`/`Copy`/`ReadImage`/`WriteImage` move data over the bulk path. Reads are the latency-sensitive case: a blocking `clEnqueueReadBuffer(CL_TRUE,...)` flushes the batch and waits; a non-blocking read records the op and signals via event, with the destination filled from the bulk region when the event completes. `clEnqueueMapBuffer` returns a pointer into a guest-visible ivshmem slot; the server copies the mapped range in (for read maps) and `clEnqueueUnmapMemObject` copies dirty bytes back (for write maps). We track map ranges and access flags to avoid copying regions the app cannot have touched.

**SVM** (shared virtual memory, 2.0) is the awkward case. Coarse-grained SVM is emulated as `ALLOC_HOST_PTR`-style buffers with explicit `clEnqueueSVMMap`/`Unmap` boundaries — workable over a copy plane. Fine-grained system SVM (any pointer, coherent) is **not offered**: it presumes a shared address space we do not have. The device reports SVM capability bits accordingly (coarse-grained buffer SVM only), so well-behaved apps fall back automatically. Image format queries (`clGetSupportedImageFormats`) are answered from the cached per-device format list gathered at enumeration.

## 17.7 Events, profiling, and the dependency graph

Events are the spine of OpenCL ordering. Each `clEnqueue*` (and user events via `clCreateUserEvent`) gets a client event handle bound to a server event id. The shim maintains the wait-list translation and a local status mirror.

```text
in-order queue Q:          out-of-order queue Q':
  e1 -- e2 -- e3            e1   e2 (no dep)
  (server runs in order)     \   /
                              e3 (waits {e1,e2})  <- explicit wait_list
```

- `clWaitForEvents` flushes and blocks on the server fence backing those events (control-plane wait).
- `clGetEventInfo(CL_EVENT_COMMAND_EXECUTION_STATUS)` is answered from the local mirror, which the server updates via piggybacked status messages on batch replies; a poll loop does not round-trip each iteration.
- `clCreateUserEvent`/`clSetUserEventStatus` let the *guest* gate server execution: the shim sends a `SetUserEvent{id,status}` control op that releases server work waiting on it. This is the one case where guest→server ordering is driven by an app call mid-stream.
- **Profiling** (`CL_QUEUE_PROFILING_ENABLE`, `clGetEventProfilingInfo`) returns the four timestamps (QUEUED/SUBMIT/START/END) captured *server-side* on the GPU timeline. These are server-clock nanoseconds; we forward them verbatim rather than rebasing to the guest clock, documenting that absolute values are server-relative while deltas (the useful quantity) are exact.

`clEnqueueMarkerWithWaitList`/`clEnqueueBarrierWithWaitList` map to server marker/barrier ops; `clFinish` flushes and waits on the queue's tail fence; `clFlush` flushes the batch without waiting.

## 17.8 Backend choice: native ICD vs funnel-through-Vulkan

Two `ClExecutor` implementations sit behind one trait, selectable per session/device:

```rust
pub trait ClExecutor {
    fn build_program(&mut self, p: &ProgramSource, opts: &str) -> Result<ProgramId, ClErr>;
    fn enqueue_ndrange(&mut self, e: &EnqueueKernel) -> Result<EventId, ClErr>;
    fn read_mem(&mut self, m: MemId, range: Range<usize>, dst: BulkSlot) -> Result<EventId, ClErr>;
    fn caps(&self) -> ClCaps;   // SVM level, fp64, images, on-device queue, version
}
```

- **Backend N (native ICD):** the server links the Windows vendor OpenCL ICD (NVIDIA/AMD/Intel) and forwards calls almost 1:1. Highest conformance and coverage (this is the breadth-priority default). Cost: behavior varies by vendor (see §17.9) and we inherit driver bugs.
- **Backend V (clvk + clspv):** OpenCL C → SPIR-V (clspv) → clvk maps OpenCL onto the Vulkan compute path already built in the Vulkan chapter (Ch. 14). Value: one compute backend for OpenCL *and* Vulkan, useful where the Windows side exposes a strong Vulkan driver but a weak/absent OpenCL ICD, and it lets the validator reason about a single SPIR-V IR. Cost: clvk targets ~OpenCL 1.2/3.0-subset; no images-with-some-formats, no on-device enqueue, fp64 only if the Vulkan device has `shaderFloat64`. The shim narrows advertised caps to match.

Selection is per device at enumeration: a device is published with the caps of whichever backend serves it, so an app's `clGetDeviceInfo` already reflects reality and feature probing "just works." The session config (the Server core chapter, Ch. 10) may force `prefer = native|clvk|auto`.

## 17.9 Cross-vendor behavior and quirks

Because Backend N forwards to whatever ICD the server runs, GraftX must normalize known divergences rather than leak them:

- **`CL_DEVICE_MAX_WORK_GROUP_SIZE` vs per-kernel size.** NVIDIA reports a generous device max but the true limit is per-kernel (`clGetKernelWorkGroupInfo`). The shim always validates against the *kernel* limit when available, not just the device limit.
- **Compiler flag tolerance.** Intel/AMD accept some non-standard `-cl` flags NVIDIA rejects. The validator keeps an allowlist of standard flags and passes vendor extras through only when the target backend is the matching vendor.
- **fp64 / `cl_khr_fp64`.** Advertised only if the chosen backend's device truly supports it; Backend V gates on `shaderFloat64`.
- **Image support is optional in 3.0.** We report `CL_DEVICE_IMAGE_SUPPORT` per backend and clamp `clGetSupportedImageFormats` to the intersection the backend can actually sample/write.
- **Pointer size / `cl_long` packing.** The wire format (the Protocol chapter, Ch. 6) fixes `size_t` as `u64` and pointer-args as handles, so a 32-bit guest talking to a 64-bit server is consistent regardless of either ICD's native width.
- **Extension string** is synthesized from the backend caps, never copied raw from the vendor — we only list extensions GraftX can actually carry across the wire (e.g. we drop `cl_khr_gl_sharing` unless the GL backend of the OpenGL/GLES/EGL/GLX chapter (Ch. 15) is co-resident and interop is wired).

## 17.10 Code generation, testing, and tradeoffs

The `cl*` entry points (a few hundred, far fewer than GL) will be **generated** from the Khronos `cl.xml` registry by the same xtask used for GL/Vulkan (the Protocol chapter, Ch. 6, owns opcode stability): generation emits the ICD-shim stubs, the opcode enum + encoder, the server decode arms, and the validation hooks. Hot paths (`clSetKernelArg`, `clEnqueueNDRangeKernel`, `clEnqueueReadBuffer`/`WriteBuffer`, `clGetEventInfo`) get hand-tuned encoders; the long tail uses the generic path. The ICD registration manifest (`/etc/OpenCL/vendors/graftx.icd`) and `clGetExtensionFunctionAddressForPlatform` are wired so the loader and extension probing route to us.

Capability summary (advertised, by backend):

| Feature | Backend N | Backend V (clvk) |
|---------|-----------|------------------|
| OpenCL C source | yes | yes (via clspv) |
| SPIR-V (`WithIL`) | yes | yes |
| Opaque binary | yes (cookie) | no |
| Images | vendor-dependent | partial |
| fp64 | if vendor supports | if `shaderFloat64` |
| Coarse SVM | yes | emulated |
| Fine/system SVM | no | no |
| On-device enqueue | if vendor supports | no |

Testing: the Khronos OpenCL CTS run through the shim against a known-good local ICD to catch divergence; a capture/replay harness records a real `CmdBatch` and replays offline against both backends, diffing kernel outputs bit-exactly for integer kernels and within ULP tolerance for float; decoder fuzzing with malformed batches to prove no `unwrap`/UB in the replay path (the Server core and Security chapters, Ch. 10/23). A retain/release stress test validates the deferred-refcount mirror against vendor `CL_*_REFERENCE_COUNT`.

Key tradeoffs accepted: `USE_HOST_PTR` downgraded to shadow-copy; no fine-grained system SVM; profiling timestamps are server-clock; binary programs are server-specific (SPIR-V preferred for caching); on-device enqueue and some image formats unavailable on Backend V. Each is a documented limitation, not a correctness bug, consistent with the breadth-over-perfection priority. Backend V is the strategic bet for unifying compute with the Vulkan path of the Vulkan chapter (Ch. 14); Backend N is the conformance and coverage safety net — both ship behind one `ClExecutor` trait so a session selects per device and workload.
