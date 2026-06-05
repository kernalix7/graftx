# Roadmap

GraftX is at **v0.0.0**: the protocol and transport spine is in place and the
end-to-end remoting path is validated with stub backends — every backend below
exercises the client/server round-trip and handle lifetime, but none calls a
native driver yet. This document tracks the path from that skeleton to a broad
GPU API-remoting layer. The Linux guest forwards GPU calls over a transport to a
Windows guest that owns the physical GPU.

**Guiding priorities:** breadth > performance > stability > safety.

See [../FEATURES.md](../FEATURES.md) for the full API matrix and
[../ARCHITECTURE.md](../ARCHITECTURE.md) for the design.

## Milestones

- [x] **M0 — Protocol + transport skeleton.** Define the wire protocol, stand up
  the transport, complete a client/server handshake, and round-trip a no-op
  call from the Linux guest to the Windows guest and back.
- [ ] **M1 — Vulkan forwarding (the spine).** Forward Vulkan; this is the
  backbone the rest of the project builds on. *(Protocol and a stub server
  backend with handle lifetime for instance / physical device / device / queue /
  memory / buffer are done; real ash-driver replay pending.)*
- [ ] **M2 — OpenGL / OpenGL ES / EGL / GLX.** Forward the OpenGL family,
  including OpenGL ES and the EGL and GLX context/window-system layers.
  *(OpenGL context/buffer stub done; OpenGL ES, EGL, and GLX pending.)*
- [ ] **M3 — CUDA + OpenCL compute.** Forward the CUDA and OpenCL compute APIs.
  *(CUDA context/alloc/free and OpenCL context/buffer stub backends done; native
  driver replay pending.)*
- [ ] **M4 — Tier 2.** Forward HIP, Level Zero, and video codecs. *(HIP
  alloc/free/stream, Level Zero context/mem, and the video protocol are done;
  video server backend in progress; native driver replay pending.)*
- [ ] **M5 — Tier 3 reach APIs.** Forward the remaining reach APIs.

## Infrastructure

Cross-cutting plumbing shared by every milestone above.

- [x] Transports: in-process Loopback and length-prefixed `StreamTransport`.
- [x] TCP serve loop.
- [x] Generational `HandleTable` for backend object lifetime.
- [x] Observability primitives.
- [x] `xtask` tooling: `gen-opcodes`, `check-xrefs`, `coverage`, `opcodes-lock`,
  `verify`.
- [x] CI `lint-docs` gate.

## Performance

Performance work runs as a track alongside the milestones above.

- [ ] Zero-copy buffers.
- [ ] Command batching.
- [ ] Async submission.
