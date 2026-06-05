# 14. Vulkan Backend (M1 — The Spine)

> How GraftX will forward the full Vulkan call graph — instances, devices, queues, command buffers, memory, descriptors, pipelines, render passes, WSI, queries and sync — across the guest-to-guest transport, and why Vulkan is chosen as the backbone every other API leans on.

Vulkan is M1, "the spine," because it exercises every hard problem the rest of GraftX must solve: opaque dispatchable/non-dispatchable handles that must be remapped between guests, explicit memory ownership, host-side pointer lifetimes (`pNext` chains, mapped memory), multi-threaded command recording, an enormous and version-gated extension surface, and a window-system layer (WSI) that crosses the guest boundary. Once Vulkan forwarding works end to end, the OpenGL/GLES/EGL/GLX chapter (Ch. 15), the CUDA chapter (Ch. 16), the OpenCL chapter (Ch. 17), the Level Zero chapter (Ch. 19) and the reach APIs reuse the same handle-table, encoding and validation machinery. This chapter specifies the *client shim* on Linux (`graftx-client`) and the *server replay* on Windows (`graftx-server`) for the Vulkan loader ICD surface. It assumes the wire format from the Protocol chapter (Ch. 06) and the Serialization chapter (Ch. 07), the `Transport` trait and channels from the Transport chapter (Ch. 08), and the dispatch/command-decode loop from the Server core chapter (Ch. 10) are already defined; it does not re-describe them.

## 14.1 Surface and scope

The client crate will export the Vulkan loader's *ICD* entry points, not the loader API. GraftX presents itself to the Linux Vulkan loader as an Installable Client Driver:

```text
linux app
  → libvulkan.so.1 (Khronos loader)
    → graftx_icd.so   (graftx-client cdylib)   [vk_icdGetInstanceProcAddr]
      → encode + transport
        ──────────────── vsock control + ivshmem bulk ────────────────
          → graftx-server (Windows)
            → real vulkan-1.dll  (vendor ICD via loader, or direct ICD)
              → physical GPU
```

The client ships a `graftx_icd.json` manifest pointing the loader at the cdylib and declaring `"api_version": "1.3.x"`. Three negotiated entry points anchor everything:

```rust
// graftx-client/src/vk/icd.rs  — exported C symbols, no name mangling
#[no_mangle]
pub unsafe extern "system" fn vk_icdNegotiateLoaderICDInterfaceVersion(
    p_supported: *mut u32,
) -> VkResult { /* clamp to ICD interface v5..=v7 */ }

#[no_mangle]
pub unsafe extern "system" fn vk_icdGetInstanceProcAddr(
    instance: VkInstance,
    p_name: *const c_char,
) -> PFN_vkVoidFunction { /* SAFETY: p_name is a NUL-terminated C string owned by the loader */ }

#[no_mangle]
pub unsafe extern "system" fn vk_icdGetPhysicalDeviceProcAddr(
    instance: VkInstance,
    p_name: *const c_char,
) -> PFN_vkVoidFunction { /* ... */ }
```

We will *not* hand-write 1000+ trampolines. Vulkan command and struct definitions will be generated from `vk.xml` (the Khronos registry) by a build-script generator (see the Serialization chapter (Ch. 07)). The generator emits, per command: (a) the `extern "system"` client trampoline, (b) a `#[repr(C)]` wire-arg struct, (c) the serialize/deserialize glue, and (d) the server-side replay arm. We bind to `vk.xml` rather than to `ash`'s pre-generated bindings so that the *same* registry walk produces wire structs and validation metadata, and so unknown-version handling is centralized.

## 14.2 The handle problem and the per-session handle table

Vulkan handles are 64-bit opaque values that are only meaningful inside the process that created them. A guest `VkBuffer` value is the *server's* native handle and must never be dereferenced or trusted on either side as a real pointer. The wire `Handle` is **server-authoritative** (see the Protocol chapter (Ch. 06) and the Handles chapter (Ch. 11)): the server mints it, and the client never puts an invented handle on the wire.

Design choice: **server mints the wire handle.** When the client calls `vkCreateBuffer` it must return *something* to the app synchronously where the spec allows. To avoid a round-trip stall, the client allocates a purely **local provisional proxy token** (a `u64` from a per-session counter) that it hands the app immediately; this token is local-only and is *never sent on the wire as authority*. When the server's real wire `Handle` arrives, the client reconciles the proxy token to the server handle deterministically, and the server records `wire_handle → native VkBuffer`. Subsequent calls referencing the object resolve the proxy to the real wire handle before sending. Dispatchable handles (`VkInstance`, `VkDevice`, `VkQueue`, `VkCommandBuffer`, `VkPhysicalDevice`) need extra care: the loader *reads the first pointer-sized word* of a dispatchable handle to find the dispatch table, so the client cannot hand the loader a bare integer.

```text
dispatchable handle layout the loader expects:
  ┌────────────────────────┬───────────────────────────┐
  │ *mut loader_dispatch    │ driver-private payload ... │   (real ICD object)
  └────────────────────────┴───────────────────────────┘
```

The client will therefore allocate a small `#[repr(C)]` shim object whose first field is the loader dispatch pointer (filled by the loader/our magic), and whose payload carries the GraftX session id + the reconciled wire handle (or, transiently, the local proxy token before reconciliation):

```rust
#[repr(C)]
struct DispatchableShim {
    loader_magic: usize,     // VK_LOADER_DATA: first word, set via set_loader_magic_value
    handle: u64,             // reconciled server-minted wire Handle (proxy token until reconciled)
    kind: HandleKind,        // Instance | Device | Queue | CommandBuffer | PhysicalDevice
}
```

Non-dispatchable handles (`VkBuffer`, `VkImage`, `VkDeviceMemory`, `VkPipeline`, …) are plain `u64` and travel as their server-minted wire handle directly; no shim object is needed. The handle table lives per session:

```rust
pub struct HandleTable {
    next_proxy: AtomicU64,                        // client-side LOCAL provisional proxy tokens only
    proxy_to_wire: DashMap<u64, u64>,             // client: proxy token → server wire Handle
    // server side only:
    to_native: DashMap<u64, NativeHandle>,        // wire Handle → server VkXxx
    // parent tracking for cascade-destroy + validation:
    parent: DashMap<u64, u64>,                    // child handle → owning device/instance handle
    kind: DashMap<u64, HandleKind>,
}
```

The server keeps `to_native`; the client keeps `proxy_to_wire` plus whatever it must return to the app. On the server, **every decoded handle argument is validated** against `to_native` before being passed to the native driver — an unknown or wrong-kind handle is rejected as a protocol error (see the Security chapter (Ch. 23)), never blindly cast. This is the first line of the "untrusted command stream" defense for Vulkan: the native driver never sees a handle GraftX did not vend.

The wire `Handle` layout is the one canonical 64-bit layout shared with the Handles chapter (Ch. 11): the **top 8 bits are the kind/API-namespace**, the **middle bits are the generation**, and the **low bits are the slot index**. The kind byte lets the server reject a `VkBuffer` handle passed where a `VkImage` is expected without a map lookup, and the generation field lets use-after-free of a recycled slot be detected (a slot whose generation is about to wrap is retired rather than reused).

```text
 63        56 55                              N N-1     0
 ┌──────────┬─────────────────────────────────┬────────┐
 │ kind(8)   │ generation                       │ slot   │
 └──────────┴─────────────────────────────────┴────────┘
```

## 14.3 Instance and physical-device enumeration — capability filtering

`vkCreateInstance` is the first call where GraftX shapes what the guest sees. The client forwards the `VkInstanceCreateInfo` (with its `pNext` chain and enabled extension/layer lists) to the server, which creates the real instance against the Windows vendor ICD. But the *enabled extensions* must be intersected with what GraftX can actually forward — see §14.10.

`vkEnumeratePhysicalDevices` is **capability-filtered**. The server enumerates the real GPUs on Windows, but GraftX presents a curated set to the guest:

```text
guest vkEnumeratePhysicalDevices
  → server enumerates real devices
  → for each: query VkPhysicalDeviceProperties2, features, memory, queues, limits
  → GraftX filter:
       • drop devices we cannot safely forward (e.g. no graphics+compute queue)
       • clamp limits we cannot honor across the transport
       • rewrite VkPhysicalDeviceProperties.deviceName → "GraftX <real name>"
       • force pipelineCacheUUID / deviceUUID to GraftX-stable values
  → cache the (filtered) result per instance; return ids to guest
```

The two-call `vkEnumeratePhysicalDevices(count=null)` / `(count, array)` idiom is handled entirely on the server: GraftX caches the filtered physical-device list at first enumeration so the count and the array are guaranteed consistent even though the guest issues two separate forwarded calls. The same caching pattern applies to every `vkEnumerate*`/`vkGet*` two-call query (queue family properties, extension properties, surface formats).

`vkGetPhysicalDeviceFeatures2`, `…Properties2`, `…MemoryProperties2` and friends carry `pNext` chains of feature/property structs. The generator emits a **`pNext` walker** keyed by `sType`; GraftX serializes only struct types it recognizes and *unlinks* unknown ones (logging them), because forwarding a struct whose layout GraftX does not know would corrupt the stream. Capability filtering then runs on the recognized structs — e.g. clamping `maxMemoryAllocationCount` or masking a feature bit for a feature GraftX will not yet forward (sparse residency, in early M1).

```rust
/// Walks a const pNext chain, invoking `f` per known struct, dropping unknowns.
unsafe fn for_each_pnext(mut p: *const VkBaseInStructure, mut f: impl FnMut(VkStructureType, *const VkBaseInStructure)) {
    while !p.is_null() {
        // SAFETY: every Vulkan pNext struct begins with { sType, pNext }.
        let base = &*p;
        if SUPPORTED_STYPES.contains(&base.s_type) { f(base.s_type, p); }
        p = base.p_next as *const VkBaseInStructure;
    }
}
```

## 14.4 Logical device and queues

`vkCreateDevice` mirrors instance creation: forward `VkDeviceCreateInfo`, intersect enabled device extensions and requested features against the GraftX-supported set, create the native device, and record the device id. The requested `VkDeviceQueueCreateInfo[]` is forwarded verbatim after validation that each `queueFamilyIndex` and `queueCount` is within the (filtered) family properties GraftX previously reported.

`vkGetDeviceQueue` / `vkGetDeviceQueue2` are interesting: the spec says repeated calls with the same family/index return the *same* queue object. The client therefore caches queue shim objects keyed by `(device_id, family, index, flags)` and only forwards the *first* request to the server, which likewise caches the native `VkQueue`. Subsequent client calls are served locally, costing zero round-trips.

`vkQueueSubmit` / `vkQueueSubmit2` are the hot path. A submit references command-buffer ids, wait/signal semaphore ids, and an optional fence id; all are remapped through the handle table on the server. The submission’s `pNext` (timeline semaphore submit info, etc.) is walked. Crucially, the GraftX command-buffer recording model (next section) means that by submit time, all the recorded commands for those buffers already live in server memory; submit just references them. Submit is *fire-and-forget by default* (the client returns `VK_SUCCESS` optimistically and surfaces a later device-lost as an asynchronous error) — see the Sync chapter (Ch. 13) for the async-submission performance track and the ordering guarantees.

## 14.5 Command buffers — the recording model

Command buffers are where naive per-call forwarding would be catastrophic: a frame can contain tens of thousands of `vkCmd*` calls, and a round-trip per call would make the guest unusable. GraftX will **batch command recording** into the ivshmem bulk plane.

```text
vkBeginCommandBuffer(cb)
  client: allocate a recording arena in ivshmem (or a spill buffer);
          tag it with cb's client_id; reset write cursor.
vkCmdBindPipeline / vkCmdDraw / vkCmdCopyBuffer / ...
  client: append each command's wire encoding to the arena. NO transport I/O.
vkEndCommandBuffer(cb)
  client: finalize the arena; emit a single CmdBufferRecord control message
          referencing the arena region {offset,len} in shared memory.
  server: decode every command in the region, validating handles/sizes,
          and replay them into the real VkCommandBuffer in order.
```

Each encoded command is a `[u16 opcode][u16 flags][payload…]` TLV, opcode-numbered by the generator. The arena is a bump allocator over an ivshmem ring slot (see the Transport chapter (Ch. 08)); when a slot fills, the client either grabs another slot or, for pathological buffers, spills to a control-plane chunk. The server replays the *whole* recorded buffer atomically when it is first submitted, or eagerly at `vkEndCommandBuffer` time if the server has spare capacity — a tunable (eager replay hides latency; lazy replay saves work for buffers that are never submitted).

Secondary command buffers (`vkCmdExecuteCommands`) record the same way; the server stitches them via the native `vkCmdExecuteCommands` using remapped ids. `vkResetCommandBuffer` / `vkResetCommandPool` drop the corresponding arenas. A decision table for where a recorded command’s bulk data goes:

| Command class                         | Inline in arena | Reference into bulk slot | Round-trip now |
|--------------------------------------|:---------------:|:------------------------:|:--------------:|
| State-setting (`vkCmdBind*`, `Set*`)  | yes             | no                       | no             |
| Draw / dispatch                       | yes             | no                       | no             |
| `vkCmdUpdateBuffer` (≤64 KiB inline)  | yes             | no                       | no             |
| `vkCmdPushConstants`                  | yes             | no                       | no             |
| Copy/blit (data already in GPU mem)   | yes (handles)   | no                       | no             |
| Indirect (params in a buffer)         | yes (handles)   | no                       | no             |

Note that `vkCmdUpdateBuffer` carries host data inline; above 64 KiB the spec forbids it anyway. Real host→device bulk transfer happens through staging buffers + mapped memory (next section), not through command recording.

## 14.6 Memory allocation and host-visible mapping

`vkAllocateMemory` forwards the `VkMemoryAllocateInfo`; the server allocates native memory and records the id. The hard part is `vkMapMemory`: the app expects a CPU pointer it can write to, but the real allocation lives in the *Windows* process address space.

GraftX maps host-visible memory through **ivshmem-backed shadow regions**:

```text
vkMapMemory(mem, off, size)
  client: reserve a region of the ivshmem BAR for this mapping;
          return a pointer into the guest's mmap of that BAR to the app.
  app writes into the BAR region (zero-copy to the shared device).
vkFlushMappedMemoryRanges(...)     // or coherent: implicit
  client: send a Flush control msg {mem_id, ranges...}.
  server: copy validated bytes from the shared region into the native
          host-visible mapping, then flush the native range.
vkInvalidateMappedMemoryRanges(...)
  server: read native mapping → copy into shared region; client reads.
vkUnmapMemory(mem)
  client: release the BAR region; server unmaps native.
```

For `VK_MEMORY_PROPERTY_HOST_COHERENT_BIT` memory the spec promises no explicit flush is required, and Vulkan *requires* that at least one `HOST_VISIBLE | HOST_COHERENT` memory type always be advertised. GraftX therefore **always advertises at least one `HOST_VISIBLE | HOST_COHERENT` memory type** and does not mask it. Since GraftX cannot make a guest write instantly visible in another guest's address space, the **primary coherent mechanism is flush-mapped-range-on-submit**: on every `vkQueueSubmit`/`vkQueuePresentKHR` that could observe a host-coherent mapping, the client flushes the affected mapped ranges from the shared region into the native host-visible mapping before the work runs. Explicit `vkFlushMappedMemoryRanges` calls (which apps issue for non-coherent types) are still intercepted and copied as they arrive, narrowing the flush-on-submit set. This is correct for both coherent and non-coherent types; the only cost is the conservative range copy on submit for coherent allocations, logged as a perf note when it is large.

Security: on flush, the server **copies validated bytes into server-private memory** before handing them to the driver (per the project's ivshmem threat model). The copy bounds-checks `(offset,size)` against the recorded allocation size; an out-of-range flush is a protocol error. The server never lets the native driver read directly out of guest-writable shared memory, because the guest could mutate it concurrently (TOCTOU). Write-revocation is the hypervisor/ivshmem-device's job; GraftX assumes it cannot enforce it and copies defensively.

`vkGetBufferMemoryRequirements`/`…2`, `vkBindBufferMemory`/`…2`, image equivalents, and `vkGetDeviceMemoryCommitment` forward straightforwardly with handle remapping. Sparse binding (`vkQueueBindSparse`) is deferred past M1 (masked out) because its residency semantics interact badly with the copy-on-flush model.

## 14.7 Descriptor sets

Descriptor management is forwarded with handle remapping but has a subtlety: `vkUpdateDescriptorSets` and the `vkUpdateDescriptorSetWithTemplate` path both reference *other* handles (buffers, images, samplers, buffer views) inside their `VkWriteDescriptorSet`/`pData` payloads, all of which must be remapped on the server.

```rust
// generated wire form of a single descriptor write (handles are client ids on the wire)
#[repr(C)]
struct WireWriteDescriptorSet {
    dst_set: u64,            // descriptor set id
    dst_binding: u32,
    dst_array_element: u32,
    descriptor_count: u32,
    descriptor_type: VkDescriptorType,
    // exactly one of these arrays is populated per descriptor_type:
    image_info: WireSlice<WireDescriptorImageInfo>,   // {sampler_id, image_view_id, layout}
    buffer_info: WireSlice<WireDescriptorBufferInfo>, // {buffer_id, offset, range}
    texel_buffer_view: WireSlice<u64>,                // buffer view ids
}
```

`vkCreateDescriptorPool`/`AllocateDescriptorSets`/`FreeDescriptorSets`/`ResetDescriptorPool` forward with parent tracking so the server can cascade-invalidate set ids when a pool is reset. `vkCreateDescriptorUpdateTemplate` stores the template entry layout on *both* sides: the client records the offsets/strides so it can serialize the opaque `pData` blob the app passes to `vkUpdateDescriptorSetWithTemplate`, walking it into typed entries; the server reconstructs a native `pData` after remapping ids. This avoids shipping the raw, layout-dependent host blob (whose stride the server would otherwise have to re-derive). Push descriptors (`vkCmdPushDescriptorSetKHR`) record into the command arena like any other `vkCmd*`.

## 14.8 Pipelines, render passes, shaders

Pipeline creation is the largest single serialization job. `vkCreateGraphicsPipelines` takes an array of `VkGraphicsPipelineCreateInfo`, each a deep tree: shader stages (each referencing a `VkShaderModule` id and an entry-point name), vertex input, viewport, rasterization, multisample, depth-stencil, color-blend, dynamic state, plus `pNext` chains for rendering-info (dynamic rendering), pipeline-library, etc. The generator emits a recursive serializer for the whole tree. Handle fields (`layout`, `renderPass`, `basePipelineHandle`, shader modules) are remapped on the server.

`vkCreateShaderModule` ships the SPIR-V `pCode` as a bulk blob (it can be large) referenced from the control message. **SPIR-V is validated on the server** before being handed to the native driver: at minimum the magic number, version, bound and instruction-stream well-formedness are checked, and ideally a `spirv-val`-equivalent pass runs. A malformed module is rejected as a protocol error rather than crashing the driver. This is a key untrusted-input checkpoint (see the Security chapter (Ch. 23)).

Pipeline cache: `vkCreatePipelineCache`/`vkGetPipelineCacheData`/`vkMergePipelineCaches` are forwarded so the *server's* native cache does the real work. GraftX will additionally cache compiled-pipeline blobs keyed by a hash of the create-info tree to skip re-forwarding identical pipelines across sessions (a perf track item). Because pipeline compilation can be slow, `vkCreate*Pipelines` is one of the few create calls that may be *synchronous* (the client waits for the server's result) when the app requested no `VK_PIPELINE_CREATE_EARLY_RETURN_ON_FAILURE`/deferred-compile flags; otherwise GraftX honors `VK_PIPELINE_COMPILE_REQUIRED` async semantics.

Render passes: `vkCreateRenderPass`/`…2` serialize attachment/subpass/dependency arrays; framebuffers reference image-view ids. Dynamic rendering (`VK_KHR_dynamic_rendering`, core in 1.3) records `vkCmdBeginRendering`/`vkCmdEndRendering` into the command arena and is preferred internally because it removes the render-pass/framebuffer object graph.

## 14.9 Swapchain / WSI — crossing the guest boundary

WSI is the hardest forwarding problem in M1, because the *surface to present to* lives on Linux but the *GPU rendering* lives on Windows. The presented image must travel back. GraftX will support a layered strategy:

```text
Strategy A (M1 baseline): "headless render + copy-back".
  - vkCreateXcbSurfaceKHR/XlibSurfaceKHR/WaylandSurfaceKHR on the client are
    INTERCEPTED, not forwarded. The client creates a GraftX-managed surface id
    bound to the guest's real window (it talks to X/Wayland itself, on Linux).
  - vkCreateSwapchainKHR: the server creates OFF-SCREEN images (a VkImage array,
    not a native VkSwapchainKHR) sized to the requested extent/format.
  - vkAcquireNextImageKHR: GraftX hands the client an index immediately (managed
    ring of N images). The acquire-supplied semaphore/fence is NOT signaled
    purely client-side; the client forwards an acquire-signal op so the SERVER
    injects the real semaphore signal (or a dummy signaling submit) that the
    later vkQueueSubmit wait observes — keeping the native sync object the
    source of truth (per the GraftX WSI design decision).
  - vkQueuePresentKHR: the server copies the just-rendered image into a staging
    buffer → ivshmem bulk slot → client blits it to the guest window surface
    (via the guest's own GL/Vulkan/XShm path) and pages-flips locally.
```

This decouples Windows-side rendering from Linux-side presentation and reuses the exact memory copy-back path from §14.6. Because the acquire-supplied semaphore must be honored by the subsequent submit, GraftX forwards an **acquire-signal op** at acquire time: the server injects the real semaphore signal (or, if no rendering precedes the wait, a dummy signaling submit on the native queue) so that the later `vkQueueSubmit` wait observes a genuine native signal rather than a fabricated client-side one. Latency is one extra copy + transport hop per frame; acceptable for M1 correctness, optimized later (see the Presentation chapter (Ch. 22) — possibly DMA-BUF / shared-image import once a hypervisor-level shared allocator exists; see also the Memory chapter (Ch. 12)).

`vkGetPhysicalDeviceSurfaceCapabilitiesKHR`/`…FormatsKHR`/`…PresentModesKHR` are answered by GraftX from the *guest* window's properties intersected with what the copy-back path can produce: GraftX advertises `FIFO` (always) and `MAILBOX`/`IMMEDIATE` only if the managed ring can satisfy them. `minImageCount`/`maxImageCount` are set by GraftX's ring size, not the driver's. `VK_KHR_swapchain` is advertised; `VK_KHR_display`/direct-mode is *not* (masked) in M1.

```rust
struct ManagedSwapchain {
    id: u64,
    extent: VkExtent2D,
    format: VkFormat,
    images: Vec<u64>,            // server-side offscreen image ids
    acquire_ring: AcquireRing,   // index handout + per-image fence/semaphore state
    present_mode: VkPresentModeKHR,
}
```

Out-of-date handling: a guest window resize makes the managed swapchain return `VK_ERROR_OUT_OF_DATE_KHR` from acquire/present, driving the app through its normal recreate path; GraftX rebuilds the offscreen image set at the new extent.

## 14.10 Extension handling and capability filtering

The advertised extension set is the contract between the guest app and GraftX. GraftX maintains an explicit **allow-list** of extensions whose commands and struct chains the generator knows how to serialize. The filter runs at two points: enumeration (what the guest *sees*) and creation (what the guest may *enable*).

```rust
enum ExtSupport {
    Forwarded,                 // fully serialized + replayed
    Emulated,                  // GraftX implements client-side (e.g. WSI surfaces)
    Masked,                    // hidden from guest entirely
    PassthroughUnvalidated,    // forwarded but commands not individually validated (disallowed by default)
}
```

| Extension                         | M1 status   | Notes |
|-----------------------------------|-------------|-------|
| `VK_KHR_swapchain`                | Emulated    | §14.9 copy-back path |
| `VK_KHR_surface` + platform surf. | Emulated    | client owns the real surface |
| `VK_KHR_dynamic_rendering`        | Forwarded   | core 1.3; preferred |
| `VK_KHR_timeline_semaphore`       | Forwarded   | needed for async submit (see the Sync chapter (Ch. 13)) |
| `VK_KHR_synchronization2`         | Forwarded   | preferred sync path |
| `VK_EXT_descriptor_indexing`      | Forwarded   | |
| `VK_KHR_buffer_device_address`    | **Masked**  | exposes raw GPU VAs the guest could forge; deferred until validated |
| `VK_KHR_external_memory_*`        | Masked      | needs shared-allocator; perf track |
| `VK_EXT_debug_utils`              | Forwarded   | names/labels are strings; cheap and useful for tracing |
| `VK_KHR_pipeline_executable_props`| Masked      | low value, large surface |
| sparse residency feature          | Masked      | §14.6 |

`buffer_device_address` is masked in M1 specifically because it lets shaders dereference raw device addresses the guest supplies — a direct path to driver memory corruption from the untrusted stream. It will return after GraftX can validate that every forwarded address belongs to an allocation GraftX vended (a non-trivial bookkeeping task tracked in the Security chapter (Ch. 23)). Unknown extension structs encountered in any `pNext` chain are *unlinked and logged*, never forwarded blind.

Version negotiation: `vkEnumerateInstanceVersion` is clamped to `min(server_real_version, GRAFTX_MAX_VK = 1.3.x)`. Commands promoted to core (e.g. `vkCmdBeginRendering` from the KHR extension) are routed to the same opcode regardless of whether the app uses the `KHR` alias or the core name; the generator records promotion aliases from `vk.xml`.

## 14.11 Query pools and synchronization objects

Query pools (`vkCreateQueryPool`, `vkCmdBeginQuery`/`EndQuery`, `vkCmdWriteTimestamp`, `vkCmdCopyQueryPoolResults`, `vkGetQueryPoolResults`) forward with handle remapping; `vkGetQueryPoolResults` is one of the genuinely *synchronous* read-back calls — the client must round-trip to fetch results, optionally blocking per `VK_QUERY_RESULT_WAIT_BIT`. Timestamp values are returned in the device's tick domain unchanged (the app applies `timestampPeriod` from the filtered limits).

Sync objects need careful semantics because the app waits *locally* on Linux but the work executes *remotely*:

- **Fences** (`vkCreateFence`, `vkWaitForFences`, `vkGetFenceStatus`, `vkResetFences`): GraftX keeps a *client-side mirror* of fence state. A submit that signals a fence sends, on completion, a control message flipping the client mirror to signaled. `vkWaitForFences` blocks on the mirror (a condvar/eventfd), so a guest thread that waits forever blocks correctly when work is in flight, and wakes when the server reports completion. `vkGetFenceStatus` reads the mirror without a round-trip.
- **Binary semaphores**: live entirely server-side for queue-to-queue ordering; the client never inspects their value. Only their *ids* travel, inside submit/present infos.
- **Timeline semaphores**: the client mirrors the 64-bit counter. `vkWaitSemaphores`/`vkGetSemaphoreCounterValue` consult the mirror; `vkSignalSemaphore` (host signal) forwards and bumps the mirror. The server pushes counter-advance notifications as work completes. Timeline semaphores are the backbone of async submission (see the Sync chapter (Ch. 13)), because the client can express "wait until timeline ≥ N" without round-tripping per submit.
- **Events** (`vkSetEvent`/`vkResetEvent`/`vkGetEventStatus`, host and device): host-side ops forward + mirror; device-side (`vkCmdSetEvent2`) record into the command arena.

```rust
struct FenceMirror { state: AtomicU8 /* UNSIGNALED|SIGNALED */, waiters: Notify }
struct TimelineMirror { value: AtomicU64, waiters: Notify }
// completion notifications drive these; vkWaitForFences/vkWaitSemaphores park on `waiters`.
```

The ordering guarantee GraftX must preserve (owned by the Sync chapter (Ch. 13), which defines the per-session monotonic `seq`): signals reported to the client mirror **must not arrive before** the corresponding wait could legally observe them, and submit-order on a single queue is preserved by the transport's in-order control channel (see the Transport chapter (Ch. 08)). Cross-queue and host-wait correctness rests on the server only emitting a completion notification *after* the native fence/semaphore actually signals.

## 14.12 Error handling, validation layers, and lost-device

Every forwarded command returns a `VkResult`; the generator maps transport/protocol failures onto `VK_ERROR_DEVICE_LOST` for in-flight work and `VK_ERROR_OUT_OF_HOST_MEMORY` for arena/transport exhaustion, so apps follow their existing recovery paths. `thiserror` enums in `graftx-server` carry the precise cause for logs; the wire only carries the `VkResult` the app expects (no `unwrap` in the lib path — a decode failure becomes a protocol error, not a panic).

Validation layers (`VK_LAYER_KHRONOS_validation`) can run on *either* side. GraftX recommends running them server-side, where the real driver lives, so the untrusted stream is validated against a real device after GraftX's own structural checks; running them client-side validates the app's *intent* before forwarding. GraftX never relies on the validation layer for *security* — its own per-command handle/size/SPIR-V checks (§14.2, §14.6, §14.8) are the security boundary; validation layers are a debugging aid.

## 14.13 End-to-end sequence: one frame

```text
APP (Linux)                CLIENT shim            TRANSPORT            SERVER (Windows)          DRIVER/GPU
 vkAcquireNextImageKHR  →   ring hands index, signals sem locally  ───────────────────────────  (no GPU work)
 vkBeginCommandBuffer   →   open arena (ivshmem slot)
 vkCmdBeginRendering    →   append TLV
 vkCmdBindPipeline…Draw →   append TLVs  (thousands, zero I/O)
 vkEndCommandBuffer     →   finalize arena
 vkQueueSubmit          →   CmdBufferRef + submit ctrl msg  ──ctrl──▶ decode+validate handles  ─▶ vkBeginCommandBuffer
                                                                      replay arena TLVs in order  …vkCmd*…
                                                                                                  vkEndCommandBuffer
                                                                      vkQueueSubmit(native, fence) ─▶ GPU executes
                              (returns VK_SUCCESS optimistically)
 vkQueuePresentKHR      →   present ctrl msg  ──ctrl──▶ wait native fence; copy image→staging ─▶ bulk slot
                              blit bulk slot → guest window  ◀──bulk──  (image bytes, validated copy)
 vkWaitForFences        →   park on FenceMirror  ◀──ctrl── completion notify ─ flip mirror, wake
```

This single frame touches handle remapping, arena recording, bulk image copy-back, optimistic submit, and the fence mirror — the full spine.

## 14.14 Tradeoffs and open questions

- **Optimistic submit vs. correctness.** Returning `VK_SUCCESS` before the server confirms hides latency but defers error reporting to the next wait/present. The alternative (synchronous submit) is simpler and more spec-faithful but ruins throughput. M1 ships optimistic-by-default with a `GRAFTX_SYNC_SUBMIT` debug toggle.
- **Copy-back WSI vs. shared images.** Copy-back is universal and works with plain ivshmem today; shared/DMA-BUF imported images would be near-zero-copy but require hypervisor-level shared allocation and external-memory extensions (masked in M1). The managed-swapchain abstraction (§14.9) is deliberately built so the present path can be swapped without touching app-visible behavior.
- **Server-minted handles with local proxy tokens.** The wire `Handle` is server-authoritative (the client never invents a handle on the wire). To avoid a per-create round-trip stall, the client returns a *local provisional proxy token* to the app immediately and reconciles it to the server's real handle when it arrives; if the server later fails the create, the proxy becomes a poisoned handle that fails on first use. This matches Vulkan's "object may be invalid until first use" latitude and is preferred over per-create stalls.
- **Coherent memory and flush-on-submit.** GraftX always advertises at least one `HOST_VISIBLE | HOST_COHERENT` type (Vulkan requires it), and flush-mapped-range-on-submit is the primary coherent mechanism. The cost is a conservative range copy on each submit that could observe a coherent mapping; explicit flush/invalidate calls narrow that set when apps issue them.
- **`buffer_device_address`.** Masked until address-provenance validation exists; its absence may force some modern engines onto fallback paths.

With this backbone in place, OpenGL/GLES/EGL/GLX (see the OpenGL chapter (Ch. 15)) reuse the handle table, arena recording and copy-back present path; the compute APIs (the CUDA chapter (Ch. 16), the OpenCL chapter (Ch. 17), the Level Zero chapter (Ch. 19)) reuse memory mapping and async submit; and the security checkpoints defined here (handle validation, SPIR-V validation, defensive bulk copy) generalize directly to the Security chapter (Ch. 23) threat model.
