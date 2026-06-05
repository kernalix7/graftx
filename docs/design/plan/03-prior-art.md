# 03. Background & Prior Art Analysis

A deep technical comparison of the systems GraftX learns from — virtio-GPU/Venus/VirGL, Looking Glass, Sunshine/Moonlight, rCUDA/cricket, DXVK/VKD3D, gVirtuS, and the broader API-remoting literature — recording exactly what GraftX will borrow, where it will diverge, and which pitfalls it must avoid.

GraftX occupies a specific point in the GPU-virtualization design space: **guest-to-guest API remoting** of *unmodified, multi-vendor, multi-API* GPU calls over a *local shared-memory + vsock* link. No single prior project sits at that point, but each adjacent project has solved (or stumbled on) a problem GraftX will also face. This chapter studies them as engineering precedent rather than as competitors. The high-level positioning lives in `docs/COMPARISON.md`; here we go down to wire formats, command-stream structure, synchronization, and failure modes. The Goals chapter (Ch. 02) framed the problem space; the Workspace chapter (Ch. 05) through the Client shim chapter (Ch. 09) specify GraftX's own protocol, transport, and server design — this chapter only establishes *why* those later decisions are shaped the way they are.

## 3.1 A taxonomy of GPU virtualization techniques

It helps to fix vocabulary first, because "GPU virtualization" conflates four mechanically distinct strategies. GraftX is firmly in the last row.

| Technique | What crosses the boundary | Guest driver | Examples | Multi-API? |
| --- | --- | --- | --- | --- |
| Full device emulation | MMIO/PCI register accesses | real, unaware | QEMU std-vga, virtio-vga | n/a (no accel) |
| Mediated passthrough (SR-IOV / vGPU) | partitioned hardware | real vendor driver | NVIDIA vGPU, Intel GVT-g | yes (native) |
| Paravirtualized GPU | a *virtualized GPU device* abstraction | custom paravirt driver | virtio-GPU, Venus, VirGL | per-renderer |
| **API remoting** | **serialized API calls** | **none — API shim** | rCUDA, cricket, gVirtuS, **GraftX** | yes (per API) |

The decisive property for GraftX is the bottom row's *"guest driver: none"*. The Linux guest keeps its stock libraries' headers and call conventions; GraftX inserts a shim (`graftx-client`, see the Client shim chapter (Ch. 09)) that exports the same C symbols (`vkCreateInstance`, `cuLaunchKernel`, `glDrawArrays`, …) and forwards them. No kernel module, no DRM driver, no paravirt device model in the guest. That is both the source of GraftX's breadth (any API with a stable ABI can be shimmed) and its central hazard (the server replays an *untrusted* command stream — see the Server core chapter (Ch. 10) on decode/validate).

## 3.2 virtio-GPU + Venus / VirGL

**What it is.** virtio-GPU is the QEMU/Linux paravirtual GPU. The guest runs a `virtio_gpu` DRM/KMS driver; Mesa's `virgl` (for GL) or `venus` (for Vulkan) Gallium/ICD frontend encodes GL or Vulkan into a command stream that the host-side `virglrenderer` library decodes and replays against the host's real GL/Vulkan driver. So virtio-GPU *is itself an API-remoting system* — it is the closest architectural relative to GraftX, and the most important to study.

**The Venus encoding.** Venus is the lesson-richest piece. It does *not* invent an ad-hoc wire format; it auto-generates encoders/decoders from the Vulkan XML registry (`vk.xml`). Each command becomes `(VkCommandTypeEXT id, u32 byte_size, payload...)` where the payload is the flattened argument list, with pointers chased and `pNext` chains walked recursively. Handles are translated through a per-context object table. GraftX will borrow this *registry-driven codegen* idea directly (the Serialization chapter (Ch. 07) and the Vulkan chapter (Ch. 14)): hand-writing serializers for Vulkan's ~700 entrypoints and CUDA's hundreds of calls is unmaintainable and error-prone, so GraftX plans a `build.rs`-time generator emitting Rust `encode`/`decode` for each command from machine-readable API descriptions.

```text
Venus command frame (conceptual):
+----------------+----------------+--------------------------------+
| cmd_type (u32) | cmd_size (u32) | flattened args + chased pNext  |
+----------------+----------------+--------------------------------+
```

```rust
// GraftX will mirror the tag+size discipline but in typed Rust.
// (Full wire spec is the Protocol chapter (Ch. 06); this is the prior-art-derived shape.)
#[repr(u16)]
enum Api { Vulkan = 1, Gl = 2, Egl = 3, Cuda = 4, OpenCl = 5, /* … */ }

struct CmdHeader {
    api: Api,          // which dispatch table decodes the rest
    opcode: u16,       // command id within that API
    flags: u16,        // OOL-payload-follows, reply-expected, fence-tied …
    len: u32,          // inline arg-block length, validated before decode
}
```

**Where GraftX diverges from Venus.**

- *Topology.* Venus is **host↔guest** with the renderer in a host userspace process (`virglrenderer` linked into QEMU). GraftX is **guest↔guest**: the replay server runs in a *second* VM (Windows) that owns the GPU via passthrough. The host neither sees nor touches the GPU. This is the entire raison d'être (the Goals chapter (Ch. 02)) — it lets the GPU stay bound to Windows where vendor drivers and tooling are strongest.
- *Transport.* Venus rides the virtio-gpu virtqueue plus host-allocated `VIRTIO_GPU_BLOB` shmem resources. GraftX will use virtio-vsock for the control plane and ivshmem for the bulk plane (the Transport chapter (Ch. 08)) — there is no virtio-gpu device in GraftX's model.
- *Scope.* Venus is Vulkan-only; VirGL is GL-only; they are separate renderers. GraftX multiplexes *many* APIs over *one* session with a per-API dispatch tag, including compute and codec APIs that virtio-GPU does not address.
- *Trust.* `virglrenderer` historically assumed a *cooperative* guest and accumulated CVEs (e.g. resource-id confusion, OOB in TGSI/command decode). GraftX must assume a *hostile* stream from line one (the Security chapter (Ch. 23)). This is a hard divergence, not an optimization.

**Pitfalls inherited.** virglrenderer's CVE history is the single most instructive warning: decoder bugs in an API-remoting renderer are *driver-reachable memory-corruption* primitives. GraftX's decode path will therefore be `#![forbid(unsafe)]` where possible, bounds-check every length/count/offset before use, and never trust an embedded size field (the `len` above is validated against the received frame, not believed).

## 3.3 Looking Glass

**What it is.** Looking Glass solves the "I passed my GPU to a Windows VM, now how do I *see* it" problem by capturing the Windows guest's *framebuffer* (via a DXGI Desktop Duplication / NvFBC capture app) into a shared-memory ring (the KVMFR device / `ivshmem`) and presenting it in a host client with sub-frame latency. It moves **pixels**, not API calls.

**What GraftX borrows.** The *transport substrate*. Looking Glass is the canonical proof that **ivshmem can move full-resolution frames between a Windows guest and another endpoint with very low, predictable latency** and zero host-CPU copy in the steady state. GraftX's bulk plane (the Transport chapter (Ch. 08)) adopts the same physical mechanism — a BAR-mapped shared region — for the inverse direction and a different payload (vertex buffers, textures, kernel arguments, mapped staging memory). Looking Glass's `KVMFRFrame` ring with sequence numbers and a host/guest cursor pair is a direct model for GraftX's bulk-plane descriptor ring.

```text
Looking Glass dataflow            GraftX bulk plane (planned, the Transport chapter (Ch. 08))
Win guest  --capture-->  ivshmem  Linux guest --serialized cmd--> vsock ctrl
              frame ring   |                 --bulk payload-----> ivshmem ring
Host client <--present--   +      Win server  <--validated copy-- ivshmem ring
```

**Where GraftX diverges.** Looking Glass is *one-directional pixel transport for the display path only*; it has no notion of GL/Vulkan/compute semantics and cannot run a headless compute job. GraftX forwards the API itself, so a CUDA reduction with no display works identically to a windowed GL app. Also, Looking Glass capture is at the *final framebuffer*; GraftX intercepts at the *API boundary*, so it never depends on a display being composited at all.

**Pitfall inherited.** Looking Glass taught the community that *ivshmem write-revocation is unenforceable from within the guest* — once a region is shared, either side can scribble it. GraftX therefore must **copy validated bulk data into server-private memory before replay** (the Security chapter (Ch. 23) and the Memory chapter (Ch. 12)) and treat the shared region as adversary-writable at all times; the hypervisor/ivshmem-device layer is the only place real write-protection could ever live.

## 3.4 Sunshine / Moonlight

**What it is.** A self-hosted game-streaming pair: Sunshine (host) captures + H.264/HEVC/AV1-encodes the desktop and streams over RTP/ENet to Moonlight (client), which decodes and renders, sending input back. It is *pixel streaming with a codec*, like a self-hosted GeForce NOW.

**What GraftX takes.** Almost nothing architecturally — and that contrast is itself the point. Sunshine spends its complexity budget on the *video pipeline*: capture, encode, congestion control, jitter buffers, packet loss recovery, frame pacing. GraftX deliberately spends *zero* there for the general case because it never encodes a frame. The one borrowed idea is *optional* presentation-path encoding: for the windowed display surface specifically, GraftX may later add a Sunshine-style encode hop (the Presentation chapter (Ch. 22)) when raw surface copy over ivshmem is bandwidth-bound at 4K/high-refresh. That would be a leaf optimization, not the core path.

**Where GraftX diverges.** Sunshine cannot accelerate compute, cannot run an unmodified Vulkan/CUDA binary's *logic* elsewhere — it can only show you a screen. GraftX runs the application's actual GPU work on the remote GPU and returns *results* (compute buffers, query answers, mapped memory), of which a framebuffer is just one special case.

**Pitfall avoided.** Lossy codecs are unacceptable for compute correctness and for any pixel-exact workflow (CAD, color grading, ML). GraftX's default is lossless API replay; encoding, if ever added, is opt-in and confined to the display surface.

## 3.5 rCUDA and cricket — single-API remoting done seriously

**rCUDA.** The reference design for *CUDA API remoting*: it interposes the CUDA Runtime/Driver API in the guest, ships calls over TCP or InfiniBand to a daemon on a GPU-bearing node, and replays them. It demonstrated that *transport latency dominates* CUDA remoting: chatty APIs with many small synchronous calls (`cudaMemcpy`, `cuStreamSynchronize`) collapse on a network RTT. rCUDA's mitigations — batching, asynchronous memcpy promotion, pinned-memory staging — are exactly GraftX's playbook, but GraftX gets a structural head start: a *local* ivshmem + vsock link has microsecond-class latency versus rCUDA's network milliseconds.

**cricket.** An academic CUDA-remoting + *checkpoint/restore* system. Its most relevant contribution is rigorous treatment of **remote resource handle management**: device pointers, streams, events, and modules live on the server, so a *handle translation table* with strict lifetime tracking is mandatory. (GraftX keeps the table but inverts cricket's naming: the server, not the client, mints the wire handle — see the Handles chapter (Ch. 11).) cricket also showed GPU-state checkpointing is feasible, which informs GraftX's much later live-migration ambitions.

**What GraftX borrows.** The handle-indirection table and the latency-hiding techniques. Unlike cricket — where the client names the object — GraftX is *server-authoritative*: the server mints a 64-bit opaque wire handle (kind/generation/slot) for every object it creates, and the client only ever echoes that handle back. For async/deferred replies a client may hold a purely *local* provisional proxy token, reconciled when the server's real handle arrives, but that token is never put on the wire as authority. Every server-side object is therefore addressed by a server-minted handle resolved through a generation-checked table:

```rust
// Per-session, per-API handle table on the server (the Handles chapter (Ch. 11)).
// GraftX handles are server-authoritative: the server mints the 64-bit wire
// Handle (kind/generation/slot), the client never invents one on the wire.
struct HandleTable<H> {
    // server-minted handle slot -> validated native handle + generation
    map: hashbrown::HashMap<u64, Slot<H>>,
}
struct Slot<H> { native: H, generation: u32, kind: ObjKind /* Buffer|Stream|Event|… */ }
// Reuse of a stale id fails generation check -> protocol error, never a UAF.
```

**Where GraftX diverges.** rCUDA/cricket are **CUDA-only and network-first**. GraftX is multi-API and local-first. Crucially, GraftX must reconcile *graphics* APIs (swapchains, present, windowing) that pure-compute remoting never confronts — a much harder synchronization and surface-ownership problem (the Sync chapter (Ch. 13) and the Presentation chapter (Ch. 22)).

**Pitfall inherited.** rCUDA's weakest point operationally was *synchronous round-trips*. GraftX's protocol (the Protocol chapter (Ch. 06)) will classify each command as `Fire`/`FireAck`/`Roundtrip`, defaulting to fire-and-forget with deferred error reporting at sync points (ordering owned by the Sync chapter (Ch. 13)), so that only genuine reads block the client thread.

## 3.6 DXVK / VKD3D — translation, not remoting

**What it is.** DXVK translates Direct3D 9/10/11 → Vulkan; VKD3D-Proton translates Direct3D 12 → Vulkan. Both run *in-process* on one machine; nothing crosses a VM boundary. They are not remoting at all.

**Why they matter to GraftX.** Two reasons. First, they are the gold standard for *high-fidelity API interception via symbol replacement* — DXVK ships `d3d11.dll`/`dxgi.dll` replacements that export the exact COM/C ABI and are loaded transparently. GraftX's `graftx-client` does the analogous thing for `libvulkan.so.1`, `libcuda.so.1`, `libGL.so.1`, etc. (the Client shim chapter (Ch. 09)). DXVK's discipline — match the ABI *exactly*, including obscure flags and undefined-behavior quirks apps rely on — is the bar GraftX shims must clear. Second, DXVK's **state-cache** and **pipeline-library** work shows how to amortize expensive object creation; GraftX can cache server-side pipeline/program objects keyed by a content hash so re-creation across sessions is cheap.

**Where GraftX diverges.** DXVK collapses one API onto another *locally*; GraftX transports an *unmodified* API across guests and does **not** translate it (Vulkan-in stays Vulkan-out on the server). GraftX could, in principle, sit *underneath* DXVK: a Windows-only D3D title running under Proton on Linux would hit DXVK→Vulkan→GraftX shim→remote Vulkan. That layering is a validation target, not a design goal.

## 3.7 gVirtuS and the API-remoting literature

**gVirtuS** generalized the rCUDA idea into a *pluggable framework*: a frontend interposer, a backend replayer, and a communicator abstraction (TCP, VMCI, shared memory) with per-API "handler" plugins (CUDA, OpenCL, cuDNN). Its layered shape is almost exactly GraftX's crate split:

| gVirtuS concept | GraftX equivalent (this design) |
| --- | --- |
| Frontend interposer | `graftx-client` shims (the Client shim chapter (Ch. 09)) |
| Communicator (pluggable) | `graftx-transport` `Transport` trait (the Transport chapter (Ch. 08)) |
| Backend + per-API handlers | `graftx-server` dispatch + per-API replay (the Server core chapter (Ch. 10)) |
| Marshalling buffers | `graftx-protocol` wire format (the Protocol chapter (Ch. 06)) |

GraftX borrows gVirtuS's **plugin-per-API** structure (each API is a self-contained encode/dispatch/replay module so breadth scales without touching the core) and its **transport-abstraction** so vsock vs. ivshmem vs. a future TCP fallback are swappable. The broader literature (GViM, vCUDA, Distributed-Shared-Memory GPU studies) repeatedly converges on the same three findings, which GraftX treats as design axioms:

1. **Marshalling cost, not bandwidth, is usually the bottleneck** for chatty APIs → minimize per-call copies, batch aggressively, codegen the marshalling.
2. **Pointer-rich, deeply-nested argument structs** (Vulkan `pNext`, descriptor writes) dominate serializer complexity → registry-driven codegen is mandatory, not optional.
3. **Synchronization semantics must be preserved precisely** or apps deadlock/corrupt → fences, events, and stream ordering need a first-class protocol model (the Sync chapter (Ch. 13)).

## 3.8 Synthesis — GraftX's borrowed-vs-divergent ledger

| Property | virtio/Venus | Looking Glass | Sunshine | rCUDA/cricket | DXVK | gVirtuS | **GraftX** |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Crosses boundary | host↔guest | Win→host px | net px | network | none | network/shm | **guest↔guest** |
| Unit transported | API cmds | framebuffer | video | API cmds | (local) | API cmds | **API cmds** |
| APIs | GL+VK | none | none | CUDA | D3D→VK | CUDA/CL | **many incl. compute+codec** |
| Transport | virtqueue | ivshmem | RTP | TCP/IB | n/a | pluggable | **vsock + ivshmem** |
| Trust model | cooperative | shared-rw | n/a | trusted net | local | trusted | **untrusted stream** |
| Codegen marshalling | yes (vk.xml) | n/a | n/a | partial | n/a | per-API | **planned, registry-driven** |

**What GraftX uniquely combines and therefore must invent itself:** a *single multi-API session* whose *untrusted* command stream rides a *guest-to-guest* vsock-control + ivshmem-bulk link with *server-side validation and private-memory copy* before native replay. No prior project carries all of those at once, so the protocol (the Protocol chapter (Ch. 06)), the bulk-plane safety model (the Transport chapter (Ch. 08), the Memory chapter (Ch. 12), and the Security chapter (Ch. 23)), and the multi-API dispatch/handle/sync machinery (the Server core chapter (Ch. 10), the Handles chapter (Ch. 11), and the Sync chapter (Ch. 13)) are the genuinely novel surfaces — everything else is consciously inherited.

**Top pitfalls, distilled.** (a) Decoder bugs are driver-reachable corruption — bounds-check everything, never trust embedded sizes (virglrenderer CVEs). (b) Shared memory is adversary-writable — copy-then-validate-then-replay, never replay in place (Looking Glass). (c) Synchronous round-trips kill throughput — fire-and-forget with deferred errors (rCUDA). (d) Hand-written marshalling does not scale to breadth — generate it (Venus/literature). (e) Handle lifetime errors become use-after-free — generation-checked handle tables (cricket). These five lessons are load-bearing inputs to the Protocol chapter (Ch. 06), the Transport chapter (Ch. 08), the Server core chapter (Ch. 10), the Handles chapter (Ch. 11), the Memory chapter (Ch. 12), the Sync chapter (Ch. 13), and the Security chapter (Ch. 23).
