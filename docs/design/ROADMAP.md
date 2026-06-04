# Roadmap

GraftX is at **v0.0.0**: the Cargo workspace is scaffolded, and no API
forwarding works yet. This document tracks the path from an empty skeleton to a
broad GPU API-remoting layer. The Linux guest forwards GPU calls over a
transport to a Windows guest that owns the physical GPU.

**Guiding priorities:** breadth > performance > stability > safety.

See [../FEATURES.md](../FEATURES.md) for the full API matrix and
[../ARCHITECTURE.md](../ARCHITECTURE.md) for the design.

## Milestones

- [ ] **M0 — Protocol + transport skeleton.** Define the wire protocol, stand up
  the transport, complete a client/server handshake, and round-trip a no-op
  call from the Linux guest to the Windows guest and back.
- [ ] **M1 — Vulkan forwarding (the spine).** Forward Vulkan; this is the
  backbone the rest of the project builds on.
- [ ] **M2 — OpenGL / OpenGL ES / EGL / GLX.** Forward the OpenGL family,
  including OpenGL ES and the EGL and GLX context/window-system layers.
- [ ] **M3 — CUDA + OpenCL compute.** Forward the CUDA and OpenCL compute APIs.
- [ ] **M4 — Tier 2.** Forward HIP, Level Zero, and video codecs.
- [ ] **M5 — Tier 3 reach APIs.** Forward the remaining reach APIs.

## Performance

Performance work runs as a track alongside the milestones above.

- [ ] Zero-copy buffers.
- [ ] Command batching.
- [ ] Async submission.
