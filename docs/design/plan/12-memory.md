# 12. Memory Management & Zero-Copy Bulk Transfer

How GraftX will move bulk GPU payloads across ivshmem with the fewest copies the security model allows, map and unmap guest memory, and handle large and persistently-mapped buffers.

The control plane (virtio-vsock) carries small, ordered command frames; it is described in the Architecture chapter (Ch.04) and the Protocol chapter (Ch.06). This chapter is about the **bulk plane**: the megabytes of vertex data, texture pixels, compute inputs, shader binaries, and mapped-buffer regions that must not be funneled through a copy-heavy socket. The bulk plane will be the ivshmem shared-memory BAR (the Transport chapter (Ch.08) covers the device and BAR layout, and owns the canonical `ShmHeader` and bulk descriptor). Here we design the allocator that lives inside that BAR, the lifecycle of a bulk transfer, and the one copy we are forced to keep for security.

## 12.1 The copy budget

The whole point of ivshmem is to remove copies. A naive socket-only path for an OpenGL `glBufferData` of a 64 MiB vertex buffer costs at minimum: client app buffer → client serialization buffer → vsock kernel buffer → server kernel buffer → server deserialization buffer → driver upload buffer. Six copies. The ivshmem design collapses the client side to a single write into shared memory and the server side to one validated copy out. Our target copy budget per direction:

| Path | Copies (client) | Copies (server) | Notes |
|------|-----------------|-----------------|-------|
| vsock inline (small frames) | 1 | 1 | for payloads below the inline threshold |
| ivshmem staging (default) | 1 (app → shm) | 1 (shm → private) | the security copy on the server side |
| ivshmem in-place (opt-in) | 0 (app writes shm directly) | 1 (shm → private) | app maps shm as its GL/VK buffer |
| persistent-mapped | 0 amortized | 1 per flush | mapping kept live across many ops |

The server-side "copy to private" (12.5) is the one copy we will not eliminate — even on the zero-client-copy in-place and persistent paths, the server still performs its mandatory copy-to-private per flush. Everything else the design tries to drive to zero on the *client* side. The inline-vs-bulk decision threshold default is **4 KiB**: below it, the payload rides inline in the vsock command frame (the round-trip to publish a shm offset costs more than the copy); at or above it, it goes through the bulk allocator. This is the single documented default; it is tunable per-API at negotiation time. The Performance chapter (Ch.25) states the same 4 KiB number.

## 12.2 The shared-region allocator

The ivshmem BAR is a single contiguous region (proposed default 256 MiB, negotiated, see the Transport chapter (Ch.08)). GraftX will carve it into a small fixed header followed by a slab-and-buddy hybrid heap. The header is the only structure both guests touch with atomics; everything else is owned by exactly one side at a time, gated by the control plane.

```text
ivshmem BAR
┌──────────────┬───────────────────────────────────────────────┐
│ ShmHeader    │            bulk heap (allocator-managed)        │
│ (4 KiB page) │  slabs (small, fixed classes) | buddy (large)  │
└──────────────┴───────────────────────────────────────────────┘
```

The `ShmHeader` (first page of the BAR: `magic`, `layout_version`, `bar_len`, `heap_off`, `heap_len`, an `epoch` bumped on any reset/reattach) is the **single canonical structure owned by the Transport chapter (Ch.08)** — defined once in `graftx-transport`. This chapter references it and does not redefine it. It is `#[repr(C)]`, cache-line aligned, accessed with Acquire/Release atomics, and version-checked against the negotiated layout version before any other field is read.

Allocations within the heap are described not by raw pointers but by an **offset handle** that is meaningless without the BAR base. Pointers cannot be shared between guests (the BAR maps at different virtual addresses on each side), so the wire type is always an offset. The canonical bulk descriptor is `ShmSlice`, defined once in `graftx-transport` (the Transport chapter (Ch.08) owns it):

```rust
/// THE canonical bulk descriptor (defined in graftx-transport, owned by Ch.08).
/// `offset` is relative to the BAR base. The shared region is <= 4 GiB, so the
/// u32 offset/len fields are sufficient (256 MiB default BAR is well within range).
/// Never contains a host/guest virtual address — see 12.4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShmSlice {
    pub offset: u32, // byte offset from BAR base (shared region <= 4 GiB)
    pub len: u32,    // length in bytes
    pub gen: u32,    // slot/epoch generation guard (ABA guard)
}
```

The allocator itself lives **only on the client side**. The client owns the heap and decides where to place data; the server never allocates into the bulk heap, it only reads from it. This asymmetry is deliberate: a single owner of the free list means no cross-guest locking on the hot allocation path, and the server's view is strictly read-only-with-validation, which is exactly what the threat model wants (the Security chapter (Ch.23)). The allocator design:

- **Small classes (≤ 64 KiB):** segregated free lists per power-of-two size class. O(1) alloc/free, no fragmentation within a class. Suits per-draw uniform/vertex spans.
- **Large allocations (> 64 KiB):** a buddy allocator over the remainder of the heap. Splits and coalesces in O(log n). Suits texture mips and big buffers.
- **Huge allocations (> heap can satisfy, e.g. an 800 MiB buffer in a 256 MiB BAR):** never live wholly in shm. They are *chunked* (12.6) — streamed through a bounded ring of staging slices.

```rust
pub trait BulkAllocator {
    /// Reserve `len` bytes; returns a handle the caller may write into.
    fn alloc(&self, len: usize) -> Result<ShmSlice, BulkError>;
    /// Release a previously-allocated slice. Bumps its generation.
    fn free(&self, slice: ShmSlice);
    /// Map a handle to a writable byte slice in *this guest's* address space.
    /// # Safety: caller must hold exclusive logical ownership of `slice`.
    unsafe fn as_mut(&self, slice: ShmSlice) -> &mut [u8];
}

#[derive(thiserror::Error, Debug)]
pub enum BulkError {
    #[error("bulk heap exhausted: needed {needed}, free {free}")]
    Exhausted { needed: usize, free: usize },
    #[error("slice {0:?} failed bounds/alignment validation")]
    InvalidSlice(ShmSlice),
    #[error("epoch mismatch (region reset under us)")]
    StaleEpoch,
}
```

## 12.3 Staging vs in-place

Two transfer modes will coexist; the client picks per call based on how the application produced the data.

**Staging mode** is the default. The application gives the shim a pointer to its own private buffer (e.g. the `data` argument of `glBufferData`, or a host-visible Vulkan mapping the app filled). The shim allocates a `ShmSlice`, copies app bytes into shared memory, and publishes the slice on the control plane. This is one client-side copy. It is robust: the app's memory layout, alignment, and lifetime are entirely the app's business, and the shim is free to choose any shm placement.

**In-place mode** is opt-in and eliminates the *client* copy. When the application *asks the driver* for a buffer to fill — `glMapBufferRange`, `vkMapMemory`, OpenCL `clEnqueueMapBuffer` — the shim can hand back a pointer that already lives inside the ivshmem heap. The app writes its vertices/pixels directly into shared memory; at unmap/flush there is nothing to copy on the client side, only a publish. This is the zero-copy *client* path: "zero-copy" is a client-side property only. The server still performs its mandatory copy-to-private on every flush/publish (12.5) — that copy is never eliminated on any path.

```text
STAGING                              IN-PLACE
app priv buf                         app calls glMapBufferRange
   │ memcpy (1 copy)                    │
   ▼                                    ▼ shim returns &shm[off]
shm slice  ──publish offset──▶          app writes here directly
                                     shm slice (0 client copies)
                                        │ unmap → publish offset ──▶
```

The tradeoff table:

| Aspect | Staging | In-place |
|--------|---------|----------|
| Client copies | 1 | 0 |
| Works for app-owned buffers | yes | no (must be map-origin) |
| Heap pressure | transient (freed after send) | held until unmap |
| Alignment control | shim chooses | must satisfy driver + shim |
| Failure mode if heap full | fall back to chunking | must fall back to a real host pointer + staging |

In-place is only safe when the mapped range fits the heap and the API contract lets us return an arbitrary pointer (all three map calls above do). When the heap cannot satisfy an in-place map, the shim transparently returns a normal heap pointer and reverts that buffer to staging — correctness over speed.

## 12.4 Mapping and unmapping guest memory

Each guest mmaps the ivshmem BAR once at session start (via the UIO/VFIO device node, see the Transport chapter (Ch.08)) and keeps it mapped for the session lifetime. The base address differs per guest, so all addressing is offset-relative.

```rust
pub struct BarMapping {
    base: NonNull<u8>,     // mmap of the BAR in THIS guest
    len: usize,
    _dev: BarDeviceFd,     // keeps the fd alive; munmap on drop
}

impl BarMapping {
    /// Translate a wire offset into a guest pointer, bounds-checked.
    /// # Safety: returned pointer is valid only while `self` lives and
    /// only for `len` bytes; caller must respect ownership handoff.
    pub unsafe fn ptr(&self, offset: u32, len: u32) -> Result<NonNull<u8>, BulkError> {
        let end = (offset as usize).checked_add(len as usize)
            .ok_or(BulkError::InvalidSlice(ShmSlice { offset, len, gen: 0 }))?;
        if end > self.len { return Err(BulkError::InvalidSlice(/* … */)); }
        // SAFETY: bounds checked above; base is a valid mmap of >= self.len.
        Ok(NonNull::new_unchecked(self.base.as_ptr().add(offset as usize)))
    }
}
```

Per-allocation `mmap`/`munmap` is explicitly rejected: it would syscall on the hot path and fragment the process address space. The single session-lifetime mapping is the only `mmap` of the BAR. "Mapping" and "unmapping" a *buffer* in the GraftX sense is therefore purely logical — allocating/freeing a `ShmSlice` and transferring ownership over the control plane — not a kernel operation.

Ownership of a slice is a strict handoff protocol, enforced by the control plane, not by hardware:

```text
client                                  server
  alloc slice S, write bytes
  ── BulkPublish{S, op} ─────────────▶  (S now logically owned by server)
                                        validate + copy S → private (12.5)
  ◀──────────── BulkAck{S} ──────────   (S returned to client)
  free(S)  // safe: server no longer reads S
```

Between `BulkPublish` and `BulkAck`, the client must treat `S` as read-only and must not free or rewrite it. This is a cooperative contract on the client side; the server does not trust it (12.5). The `gen` field in `ShmSlice` guards against an ABA hazard where a slice is freed and reallocated before a stale ack arrives.

## 12.5 The copy-to-private TOCTOU mitigation

This is the load-bearing security mechanism of the bulk plane, and it is intentionally a copy we keep. The server is replaying an **untrusted** command stream against real drivers. A malicious or buggy client shares the ivshmem BAR and can, in principle, mutate any byte of it at any instant — including *after* the server has validated a region but *before* the driver consumes it. That is a classic time-of-check-to-time-of-use (TOCTOU) race.

Write-revocation (making the client's view read-only mid-transfer) can only be enforced at the hypervisor/ivshmem-device layer, which GraftX cannot assume exists. So the server will not validate-in-place. Instead, for every bulk region it intends to feed a driver, it performs an **atomic snapshot**:

```rust
/// Server side. Copy a published slice into server-private memory, THEN
/// validate the private copy. After this returns, the client can mutate
/// shared memory all it likes — the driver only ever sees `private`.
fn snapshot_and_validate(
    bar: &BarMapping,
    s: ShmSlice,
    limits: &Quota,
) -> Result<Box<[u8]>, BulkError> {
    if s.len as usize > limits.max_bulk_bytes {
        return Err(BulkError::InvalidSlice(s));
    }
    // SAFETY: ptr() bounds-checks s against the BAR length.
    let src = unsafe { std::slice::from_raw_parts(bar.ptr(s.offset, s.len)?.as_ptr(), s.len as usize) };
    let private: Box<[u8]> = src.to_vec().into_boxed_slice(); // THE copy
    validate_payload(&private, s_op_context())?;              // check the copy
    Ok(private)
}
```

The ordering is the whole point:

```text
WRONG (racy):   validate(shm)  →  client mutates shm  →  driver reads shm   ☠ TOCTOU
RIGHT:          copy(shm→priv) →  validate(priv)       →  driver reads priv  ✓
```

Once bytes are in server-private memory, the client's continued access to the BAR is irrelevant; the validated bytes and the consumed bytes are byte-for-byte identical because they are the same allocation. The cost is exactly one `memcpy` per bulk region on the server, sized to the negotiated `max_bulk_bytes` quota. We accept this cost as the price of replaying untrusted input safely. Validation (`validate_payload`) is API-specific — bounds on buffer sizes, sane image dimensions, shader-binary structural checks — and is detailed per-API in the coverage chapters; here we only fix *where* it runs: on the private copy, never on shared memory.

A subtlety: regions the driver only *reads* (vertex data, texture uploads) need the copy. Regions the driver *writes back* (readback, mapped reads, query results) flow server→client and have the inverse property — the server fills a private buffer, copies it into a fresh shm slice, and publishes it; the client then copies out. The same one-copy-each-side budget holds in reverse.

## 12.6 Large allocations and chunked streaming

When a single payload exceeds what the heap can hold at once, GraftX will stream it through a bounded ring of staging slices rather than demanding a bigger BAR. A `glBufferData` of 800 MiB into a 256 MiB BAR becomes N chunks:

```rust
pub struct ChunkRing {
    slices: Vec<ShmSlice>,   // e.g. 4 slices of 16 MiB each
    head: usize,
}
// Pseudocode flow:
//   for chunk in payload.chunks(CHUNK):
//       let s = ring.next_free();           // blocks if all in flight (backpressure)
//       copy chunk -> s; publish(s, BulkChunk{ buffer_id, offset, last });
//   server: snapshot each chunk, append into the private buffer at `offset`,
//           and only call the driver once `last` chunk validated.
```

Chunking gives natural backpressure: the ring has a fixed number of in-flight slices, so a fast producer cannot outrun the server or overflow the BAR. The chunk size and ring depth are negotiated against the BAR size and the `max_bulk_bytes` quota. This same ring is the substrate for streaming uploads that are inherently large (compute datasets, video frames).

## 12.7 Persistent-mapped buffers

`GL_MAP_PERSISTENT_BIT`, `glMapBufferRange` with coherent persistent flags, and persistently-mapped Vulkan `vkMapMemory` keep a single mapping live for the buffer's whole lifetime; the app writes into it repeatedly without remapping. GraftX will back these with a dedicated, long-lived `ShmSlice` placed in a **pinned sub-region** of the heap that the allocator will not reclaim until the buffer is deleted.

```rust
pub struct PersistentMap {
    slice: ShmSlice,          // pinned for buffer lifetime
    server_buffer_id: u64,    // driver-side object on the Windows side
    dirty: DirtyTracker,      // sub-ranges the app touched since last flush
    coherent: bool,           // app requested coherent mapping
}
```

Re-uploading the entire region on every frame would defeat the purpose, so a `DirtyTracker` will record touched sub-ranges. Two flush triggers: an explicit `glFlushMappedBufferRange` / `vkFlushMappedMemoryRanges`, or, for coherent mappings (no explicit flush), a heuristic flush at the next draw/submit that consumes the buffer. Only dirty sub-ranges are published; each still passes through copy-to-private on the server. Coherent persistent mappings are the hardest case — the API contract says writes are visible "immediately" without a flush, which we cannot honor across guests at memory speed; GraftX will approximate it by flushing dirty ranges at every GPU-visible sync point (sync-point semantics are owned by the Sync chapter (Ch.13)) and documenting the relaxed-but-correct-at-sync semantics as a known coverage limitation in the OpenGL chapter (Ch.15) and the Vulkan chapter (Ch.14).

## 12.8 DMA considerations

A frequent question: can the driver DMA *directly out of* the ivshmem BAR, skipping even the server-side copy? On the Windows side, the BAR is just guest RAM mapped through the ivshmem PCI device; the passthrough GPU could in principle DMA from those physical pages. GraftX deliberately does **not** do this, for two reasons. First, it would reintroduce the TOCTOU race (12.5) directly into the DMA engine, where it is unfixable in software. Second, GPU DMA needs pinned, driver-allocated, often device-local staging memory with specific alignment and IOMMU mappings; the ivshmem heap satisfies none of those guarantees. So the server-private copy doubles as the DMA-safe staging buffer: validated, server-owned, allocated with the alignment the driver wants, and handed to the driver's normal upload path. The ivshmem BAR is a transport, not GPU-accessible memory. Alignment of `ShmSlice` allocations (16-byte minimum, larger classes naturally aligned) is chosen to keep the server-side copy and the driver's subsequent staging cheap, but the BAR is never the DMA source of record.

## 12.9 Summary of decisions

- Single session-lifetime `mmap` of the BAR per guest; all addressing via bounds-checked `ShmSlice` offsets, never pointers.
- Client-only allocator (slab + buddy + chunk-ring) over the heap; server is read-only-with-validation.
- Default staging (one client copy); opt-in in-place (zero client copy) for map-origin buffers; transparent fallback when the heap is full.
- Exactly one mandatory server-side copy: snapshot-to-private *before* validate, *before* driver — the TOCTOU mitigation, and simultaneously the DMA-safe staging buffer.
- Large payloads stream through a bounded chunk ring (backpressure); persistent maps use pinned slices with dirty-range flushing.
- The BAR is transport memory, never a GPU DMA source.

Cross-references: BAR/device setup in the Transport chapter (Ch.08) and the vsock control plane in the Protocol chapter (Ch.06); quota/backpressure enforcement in the Transport chapter (Ch.08); the validation functions invoked on private copies are specified per-API in the coverage chapters (the Vulkan chapter (Ch.14) through the Video chapter (Ch.20)); the threat model behind 12.5 is the Security chapter (Ch.23).
