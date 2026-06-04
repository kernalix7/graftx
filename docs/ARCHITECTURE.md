# GraftX Architecture

**English** | [한국어](ARCHITECTURE.ko.md)

A design deep-dive into how GraftX remotes GPU API calls from a Linux guest to
a Windows guest that owns the physical GPU.

> **Status:** early development (`0.0.0`). The wire protocol, transport, and
> internals are **not stable**. Sections describing behavior that is not yet
> implemented are marked **(planned)**. Today the workspace contains the crate
> skeletons, a negotiated `PROTOCOL_VERSION`, a `Transport` trait, and a
> server that prints the protocol version it speaks — everything else here is
> the design those skeletons are growing into.

---

## Overview

A single physical GPU is often handed to **one** virtual machine through PCI
passthrough — typically a **Windows guest**, where vendor drivers and tooling
are best supported. Every other guest on the same host is then left without
acceleration.

GraftX is an **API-remoting layer** that closes that gap. A Linux-guest
application keeps calling its normal GPU APIs (OpenGL, OpenGL ES, EGL, Vulkan,
CUDA, OpenCL, ROCm/HIP, Level Zero, video codecs, …) completely unmodified.
GraftX intercepts those calls in the Linux guest, **serializes** them, ships
them across a fast **guest-to-guest transport**, and **replays** them on the
Windows guest against the native driver. Results and surfaces are returned to
the Linux side.

This is *API remoting*, not paravirtualization and not pixel streaming:

- It forwards **API calls**, not a host-virtualized GPU device (unlike
  virtio-GPU / Venus / VirGL).
- It forwards **API calls**, not a finished framebuffer (unlike Looking Glass)
  or an encoded video stream (unlike Sunshine / Moonlight).
- It targets a **local guest-to-guest** channel between two VMs on the same
  host, not a network link (unlike rCUDA).

### Data flow

```
        LINUX GUEST (untrusted client)                    WINDOWS GUEST (owns the GPU)
  ┌───────────────────────────────────────┐        ┌──────────────────────────────────────┐
  │  application                           │        │  graftx-server (replay engine)         │
  │     │  calls libvulkan.so / libGL.so / │        │     ▲   decode + validate + replay      │
  │     │  libcuda.so / … (unmodified)     │        │     │                                    │
  │     ▼                                  │        │     │                                    │
  │  graftx-client shim (cdylib)           │        │  graftx-protocol (decode)              │
  │     │  intercept C entry point         │        │     ▲                                  │
  │     ▼                                  │        │     │  command frames                  │
  │  graftx-protocol (encode)              │        │     │                                  │
  │     │  command frames                  │        │  graftx-transport ::recv()             │
  │     ▼                                  │        │     ▲                                  │
  │  graftx-transport ::send() ────────────┼──┐     │     │                                  │
  └───────────────────────────────────────┘  │     └─────┼──────────────────────────────────┘
                                              │           │            ▲ native driver
       guest-to-guest transport              │           │            │ (Vulkan / CUDA / …)
       (virtio-vsock | ivshmem) ─────────────┘           │            ▼
                                              ▲           │      ┌──────────────┐
  return path: results / surfaces / status ───┘           │      │ physical GPU │
       graftx-transport ::recv()  ◀──── graftx-transport ::send() │ (passthrough)│
                                                                  └──────────────┘
```

Conceptually:

```
Linux client  ──encode──▶  transport  ──decode──▶  Windows server  ──replay──▶  GPU
      ▲                                                                            │
      └──────────────────────  results / surfaces / status  ◀─────────────────────┘
```

---

## Components

The system is a Cargo workspace of four crates under `crates/`. The split is
deliberate: the protocol and transport are shared, host-neutral libraries; the
client and server are the two opposite ends of the link.

### `graftx-protocol` (lib)

Owns the **wire format** and the encode/decode logic shared by both ends.

- Defines `PROTOCOL_VERSION` (currently `0`, the pre-stable marker — bumped on
  every breaking change until the format is frozen). Both sides negotiate this
  during the handshake before any GPU call is forwarded.
- Defines `ProtocolError` (e.g. `UnexpectedEof`, `UnknownOpcode`) raised while
  encoding or decoding.
- **(planned)** Command opcodes per API, argument marshalling, framing,
  handle/object mapping, and the handshake message itself.

This crate is **pure, host-neutral, and contains no FFI and no I/O**. It is the
single source of truth for what a byte on the wire means; the client and server
must agree on it exactly. Keeping it free of unsafe code and platform
assumptions makes the wire format independently testable.

### `graftx-transport` (lib)

Owns the **guest-to-guest channel**. Exposes the `Transport` trait — a
bidirectional, framed byte channel:

```rust
pub trait Transport {
    fn send(&mut self, frame: &[u8]) -> io::Result<()>;
    fn recv(&mut self) -> io::Result<Vec<u8>>;
}
```

Concrete backends (**virtio-vsock**, **ivshmem shared memory** — both planned)
implement this trait so the client and server are written against the
abstraction, not a specific channel. Transport moves **opaque frames**; it does
not understand the protocol. Any backend-specific unsafe code (mapping shared
memory, raw socket handles) is isolated in dedicated modules with `// SAFETY:`
comments, per repo convention.

### `graftx-client` (cdylib + rlib)

The Linux-side **API shims**. Built as a `cdylib` so it can be loaded into an
application in place of the real driver libraries (`libvulkan.so`, `libGL.so`,
`libEGL.so`, `libcuda.so`, …), exporting the **C symbols** of the API it
intercepts. The `rlib` form lets the rest of the workspace and tests link
against it as a normal Rust library.

Responsibilities:

- Intercept each API's C entry points (the application sees the real symbols).
- Serialize each call via `graftx-protocol` (`PROTOCOL_VERSION` is the version
  this client speaks and is checked against the server at handshake).
- Forward frames over a `graftx-transport::Transport`, await the reply, and
  reconstruct return values / output parameters for the caller.

All symbol interception and pointer marshalling is FFI-heavy and is confined to
dedicated `unsafe` modules with `// SAFETY:` comments. Library paths avoid
`unwrap()` / `expect()`.

### `graftx-server` (bin)

The Windows-side **replay engine**, running on the guest that owns the GPU. It:

- Accepts a transport connection from a paired client and performs the version
  handshake.
- Decodes the incoming command stream via `graftx-protocol`.
- **Validates and bounds-checks** every decoded command (the stream is
  untrusted — see *Security*).
- Replays each call against the **native vendor driver** on the real GPU.
- Returns results, surfaces, and status back over the transport.

This is the component that touches real drivers with attacker-influenced data,
so it is the primary sandboxing and hardening target.

---

## Call lifecycle

A single forwarded GPU call moves through six stages:

1. **Intercept** — the application calls an API entry point; the `graftx-client`
   shim, loaded in place of the real driver, receives the call with its
   arguments. Local-only or trivially cacheable calls *may* be answered without
   a round-trip **(planned)**; everything else is forwarded.

2. **Encode** — the shim serializes the call into a protocol command frame via
   `graftx-protocol`: an opcode plus marshalled arguments. Pointers are
   translated into either inline payloads (input buffers) or
   server-side-allocated handles (objects, output buffers).

3. **Transport (send)** — the frame is handed to `Transport::send` and crosses
   the guest-to-guest channel (virtio-vsock or ivshmem) to the Windows guest.

4. **Decode + validate** — the server reads the frame with `Transport::recv`,
   decodes it via `graftx-protocol`, and **validates** it: known opcode,
   in-range lengths/handles, sane buffer sizes. Invalid frames are rejected
   (`ProtocolError`) rather than passed to the driver.

5. **Replay** — the server invokes the corresponding native driver entry point
   on the real GPU with the reconstructed arguments, mapping client-side handles
   to real driver objects.

6. **Return** — results, output buffers, and surfaces are encoded and sent back
   over the transport; the client decodes them and returns to the application as
   if the call had executed locally.

```
app → [intercept] → [encode] → [send] ──▶ [recv] → [decode+validate] → [replay] → GPU
                                                                                    │
app ← [decode]   ←  [recv]   ← [send] ◀── [encode results] ◀───────────────────────┘
```

A guiding constraint throughout: a **call that needs a result** (a synchronous
query) costs a full round-trip, whereas a **fire-and-forget** call (most
command-buffer recording) can be pipelined. The protocol and shims are designed
to keep the synchronous, round-trip-bound calls as rare as possible (see
*Performance strategy*).

---

## Transport options

`graftx-transport` abstracts the channel behind the `Transport` trait so the
backend can be chosen per deployment without touching the client or server. Two
backends are targeted; both are **planned** (the trait exists, concrete
implementations do not yet).

### virtio-vsock

A socket-style, host-mediated channel between guests (`AF_VSOCK`), addressed by
context ID + port.

- **Pros:** simple stream/datagram semantics; natural framing and
  backpressure; straightforward connection setup and teardown; no custom shared
  device required beyond what the hypervisor already offers.
- **Cons:** every payload is **copied** through the vsock path (guest → host →
  guest), which hurts the large-buffer transfers (textures, vertex data,
  compute inputs) that dominate GPU traffic; latency is higher than a shared
  mapping.
- **Best for:** control-plane traffic, the handshake, small command frames, and
  bring-up — it is the easiest backend to get a correct round-trip working
  first.

### ivshmem shared memory

A shared-memory region mapped into both guests (inter-VM shared memory), with a
small doorbell/ring for signaling.

- **Pros:** **zero-copy** for bulk data — the client writes a buffer once and
  the server reads it in place, no host bounce; lowest latency; ideal for the
  large surface/buffer transfers that dominate GPU workloads.
- **Cons:** more complex and more dangerous — both ends share writable memory,
  so it demands careful synchronization (ring indices, fences) and validation;
  it is the larger attack surface and the larger source of `unsafe` code.
- **Best for:** the data plane — bulk buffers, frames, and the hot path once
  correctness is established on vsock.

### Tradeoff summary

| Aspect            | virtio-vsock              | ivshmem shared memory          |
| ----------------- | ------------------------- | ------------------------------ |
| Data movement     | copied (guest↔host↔guest) | zero-copy (mapped region)      |
| Latency           | higher                    | lowest                         |
| Setup complexity  | low                       | higher (ring + doorbell)       |
| Bulk-buffer cost  | poor                      | excellent                      |
| Attack surface    | smaller                   | larger (shared writable RAM)   |
| Best fit          | control plane / bring-up  | data plane / hot path          |

**Direction (planned):** bring up correctness on virtio-vsock first, then move
the bulk data path to ivshmem; a hybrid where small control frames use vsock and
large buffers ride shared memory is the likely steady state.

---

## API coverage strategy

Coverage is the **top priority**, so the strategy balances breadth against the
cost of implementing each API. There are two implementation styles:

- **Native forward (per-API).** Intercept an API and replay it against the
  *same* native API on the server (Vulkan → Vulkan, CUDA → CUDA). Highest
  fidelity and lowest overhead, but each API is a distinct, large surface to
  marshal.
- **Funnel-through (translation).** Translate one API onto another that is
  already forwarded — the approach taken by **Zink** (OpenGL → Vulkan) and
  **clvk** (OpenCL → Vulkan). One serialized backend (Vulkan) then carries
  several front-end APIs. Far less per-API marshalling work, at some fidelity
  and overhead cost.

GraftX takes a **hybrid approach**: forward the high-value, performance-critical
APIs natively, and funnel breadth APIs through an already-forwarded backend
where a mature translation layer exists. **Vulkan is the spine** — thin,
explicit, lowest overhead, and the natural funnel target for graphics and
compute front-ends.

### Tiered rollout

The breadth-first goal is realized as tiers (mirroring the README and roadmap):

- **Tier 1 — backbone:** **Vulkan** (the spine), **OpenGL**, **OpenGL ES**,
  **EGL**, **CUDA**, **OpenCL**. Vulkan is forwarded natively first
  (milestone M1); GL/GLES/EGL follow; CUDA and OpenCL bring cross-vendor
  compute.
- **Tier 2 — breadth:** **HIP** (AMD/ROCm), **Level Zero** (Intel oneAPI),
  and video codecs — **VA-API**, **VDPAU**, **NVENC/NVDEC**, **Vulkan Video**.
- **Tier 3 — reach:** **SYCL** (rides Level Zero), **OptiX** (rides CUDA),
  **AMF**, **WebGPU/wgpu**, **GLX**. These lean heavily on funneling through a
  lower API already covered.

Where an API "rides" another (SYCL on Level Zero, OptiX on CUDA, and the
GL-family on Vulkan via a Zink-style path **(planned)**), GraftX gets that
coverage largely for free once the lower API is forwarded — which is exactly why
Vulkan and the compute backends come first.

---

## Performance strategy

Performance is the second priority. The remoting overhead is dominated by two
costs: **round-trip latency** across the guest boundary, and **data copies** of
large buffers. The strategy attacks both. All items below are **planned**.

- **Zero-copy buffers.** Move bulk data (textures, vertex/index buffers,
  compute inputs, decoded frames) through the ivshmem shared region so it is
  written once and read in place, instead of being copied through the vsock
  path. This is the single biggest lever for GPU-style traffic.

- **Command batching.** Coalesce many recorded calls — especially
  command-buffer recording, which is naturally deferred in explicit APIs like
  Vulkan — into a single transport frame. Amortizes per-frame framing and
  signaling overhead over many calls.

- **Asynchronous submission.** Forward fire-and-forget calls without blocking
  the application thread on a reply. Only calls that *return* a result
  (synchronous queries, fences, map-readbacks) force a round-trip; the rest are
  pipelined so the client races ahead of the server.

- **Minimizing round-trips.** The protocol and shims are designed to avoid
  synchronous queries on the hot path: allocate object handles client-side and
  reconcile lazily, defer error/status reporting where the API's contract
  allows, and cache immutable query results (device properties, limits,
  extension lists) after the first call. Each avoided round-trip removes one
  full guest-to-guest latency from the critical path.

The north star: keep the steady-state hot path **batched, asynchronous, and
zero-copy**, so that remoting overhead is a small constant added to native
driver time rather than a per-call latency tax.

---

## Security / threat model

> **Not yet hardened.** GraftX must **not** be exposed to untrusted clients
> during early development. The protections below are the intended design;
> several are **planned**.

The core danger: the **server replays an untrusted command stream against
native GPU drivers**. The Linux client — and therefore everything arriving over
the transport — is treated as **untrusted**. GPU drivers are large, complex,
privileged C/C++ code; feeding them attacker-controlled commands and buffers is
a real attack surface:

- **Driver bugs** — malformed arguments can trigger memory-safety faults deep
  in vendor drivers.
- **Out-of-bounds buffers** — sizes, offsets, and handles in the stream must
  never be trusted to be in range.
- **Resource exhaustion** — a malicious or buggy client can try to exhaust GPU
  memory, handles, or server CPU/RAM.

### Defenses

- **Validate and bounds-check every decoded command.** The server treats all
  decoded input as hostile: verify opcodes, lengths, offsets, and handle
  references against known limits *before* anything reaches a driver. Decode
  failures surface as `ProtocolError` and the command is rejected, never
  forwarded.

- **Validate a private copy on the ivshmem path — never in place.** The
  zero-copy ivshmem region stays mapped **writable on the client side**, so any
  byte the server reads from it can be changed by the client *after* a check and
  *before* (or during) the driver read. Validating data in place over that
  region is therefore a classic **double-fetch / TOCTOU** hazard: the value the
  server validates is not guaranteed to be the value the driver later sees.
  Validate-in-place over client-writable shared memory is **unsafe** and must
  not be used. The **recommended/default path** is therefore (a): for any command
  arriving over ivshmem the server **must** copy the length, offset, and handle
  fields — and any security-relevant buffer the validation depends on — into
  **server-private memory**, validate that private copy, and hand *only* the
  private copy to the driver. Write-revocation (b) is **not** something the
  server can do by changing its own view: with ivshmem each guest maps the
  shared device BAR independently, so re-mapping the server's *own* mapping
  read-only does nothing to stop the client (a separate guest) from writing the
  region. Revoking a peer guest's write access can only be enforced **at the
  hypervisor / ivshmem-device layer** — e.g. via QEMU ivshmem configuration or
  host-mediated arbitration of the BAR's writability — never by the server
  alone; and even then it is costly and often infeasible to toggle on the
  per-call hot path. So copy-and-validate (a) is preferred. This makes explicit
  that "zero-copy" and "validated" are in tension on the bulk data path; the
  design resolves that tension by validating a private copy, **not** by trusting
  in-place data. (Mechanism **planned**.)

- **Sandbox the server.** Run the replay engine with the least privilege the
  GPU work allows, isolated so that a driver compromise is contained and cannot
  pivot to the rest of the Windows guest or the host. (Mechanism **planned**.)

- **Paired-guest channel only.** Restrict the transport to the **specific
  paired guests** for the deployment — both virtio-vsock (CID/port scoping) and
  ivshmem (which guests map the region) are configured so an arbitrary or
  remote party cannot connect. There is no networked or multi-tenant mode.

- **Version handshake.** Client and server negotiate `PROTOCOL_VERSION` before
  any GPU call is forwarded; a mismatch aborts the session rather than
  attempting a best-effort, ambiguous decode.

- **Contain unsafe code.** All FFI and raw-memory access (client symbol
  interception, server driver calls, ivshmem mapping) is isolated in dedicated
  `unsafe` modules with `// SAFETY:` comments; library paths avoid
  `unwrap()` / `expect()`. Keeping unsafe surface small and explicit keeps it
  auditable.

### Trust boundaries

```
   trusted-by-its-owner            UNTRUSTED stream            replays against
   Linux application      ──────▶  across transport   ──────▶  native drivers
   (its own client shim)           (paired guests)             (server: validate
                                                                + sandbox here)
```

The hard boundary is at the **server's decode step**: everything before it is
untrusted, everything the driver sees must already have been validated.

---

## Conventions (for contributors)

- Rust edition 2021, `rust-version` 1.75, stable toolchain with `rustfmt` +
  `clippy`. CI gate: `cargo clippy --all-targets -- -D warnings` and
  `cargo fmt --check`.
- All `unsafe` / FFI isolated in dedicated modules with `// SAFETY:` comments.
- No `unwrap()` / `expect()` in library paths.
- Client shims are `cdylib`s exporting the intercepted API's C symbols.
- Conventional Commits (`feat:` / `fix:` / `refactor:` / `docs:` / `chore:` /
  `test:`); branches `feature|fix|chore/<name>`; squash-merge to `main`.

See [FEATURES.md](FEATURES.md) for the API coverage tables and
[design/ROADMAP.md](design/ROADMAP.md) for the roadmap, and
[SECURITY.md](../SECURITY.md) for reporting.
