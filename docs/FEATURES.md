# Features

**English** | [한국어](FEATURES.ko.md)

GraftX forwards GPU API calls from a Linux guest to a GPU-owning Windows guest. This page lists the API coverage matrix and the rollout plan. Everything is **planned** until marked otherwise — GraftX is at v0.0.0.

## API coverage

Coverage rolls out in tiers. Tier 1 is the backbone (NVIDIA + AMD + Intel + cross-vendor compute); Tiers 2–3 extend breadth. Vulkan is the spine — thin, explicit, lowest overhead — and other APIs may either be forwarded natively or funneled through it.

### Tier 1 — backbone

| API | Domain | Vendor | Status |
| --- | --- | --- | --- |
| **Vulkan** | Graphics / compute | cross — the spine | Planned |
| **OpenGL** | Graphics | cross | Planned |
| **OpenGL ES** | Graphics (embedded subset) | cross | Planned |
| **EGL** | Context / surface management | cross | Planned |
| **CUDA** | Compute | NVIDIA | Planned |
| **OpenCL** | Compute | cross-vendor | Planned |

### Tier 2 — breadth

| API | Domain | Vendor | Status |
| --- | --- | --- | --- |
| **HIP** | Compute (portable layer over ROCm) | AMD | Planned |
| **Level Zero (oneAPI)** | Low-level compute | Intel | Planned |
| **VA-API** | Video decode / encode | Intel / AMD | Planned |
| **VDPAU** | Video decode | NVIDIA | Planned |
| **NVENC / NVDEC** | Video encode / decode | NVIDIA | Planned |
| **Vulkan Video** | Video decode / encode | cross | Planned |

### Tier 3 — reach

| API | Domain | Vendor | Status |
| --- | --- | --- | --- |
| **SYCL / oneAPI** | Compute (rides Level Zero) | cross | Planned |
| **OptiX** | Ray tracing (rides CUDA) | NVIDIA | Planned |
| **AMF** | Video encode | AMD | Planned |
| **WebGPU / wgpu** | Graphics / compute | cross | Planned |
| **GLX** | X11 GL context glue | cross | Planned |

## Design principles

- **Breadth first.** The headline goal is supporting as many GPU APIs and versions as possible.
- **Low overhead.** Zero-copy buffers, command batching, and async submission keep the remoting cost down.
- **Stable under load.** Correctness on real workloads outranks micro-optimizations.
- **Safe by default.** The server treats the client's command stream as untrusted (see [SECURITY.md](../SECURITY.md)).

## Forwarding strategy

Two strategies, chosen per API:

- **Native forward** — one shim per API, replayed on the native Windows driver. Maximum breadth and performance; more server surface to maintain.
- **Funnel-through-Vulkan** — translate legacy GL → Vulkan (Zink), OpenCL → Vulkan (clvk/clspv). Less server code, with some translation cost.

A hybrid is expected: native forwarding for CUDA / Vulkan / compute, funneling for legacy GL. See [ARCHITECTURE.md](ARCHITECTURE.md) for details.
