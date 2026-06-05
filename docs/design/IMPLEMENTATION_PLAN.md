# GraftX Implementation Plan

A complete, chapter-by-chapter engineering plan for building GraftX — the Rust GPU API-remoting layer that lets a Linux guest run GPU workloads on a GPU owned by a Windows guest. This is forward-looking design at **v0.0.0**: it describes how the system *will* be built, not what exists today.

The plan is split into 32 chapters under [`plan/`](plan/), grouped into seven parts. Read top-to-bottom for the full picture, or jump to a backend chapter once you have read Parts I–III. Each chapter is self-contained and cross-references its neighbours by number.

> **Scope.** ~88k words across 32 chapters (~200–320 printed pages depending on layout). Engineering-internal; the audience is contributors and maintainers. User-facing docs live one level up in [`../`](../).

## How to read this plan

1. **Part I** establishes *why* and *what* — read first.
2. **Parts II–III** define the protocol/transport contract and the client/server cores that every backend depends on. Read before any backend chapter.
3. **Part IV** is per-API; read the backend you are working on. Vulkan (Ch. 14) is the spine — read it even if you start elsewhere.
4. **Parts V–VI** are cross-cutting (security, performance, testing, delivery) and apply to all backends.
5. **Part VII** is reference material.

## Table of contents

### Part I — Foundations
| # | Chapter | Focus |
|---|---------|-------|
| 01 | [Introduction, Vision & Document Scope](plan/01-introduction.md) | Why GraftX exists, how to read this plan |
| 02 | [Goals, Non-Goals & Success Metrics](plan/02-goals.md) | breadth > performance > stability > safety |
| 03 | [Background & Prior Art Analysis](plan/03-prior-art.md) | virtio-GPU/Venus, Looking Glass, rCUDA, DXVK |
| 04 | [High-Level Architecture & Data Flow](plan/04-architecture.md) | Control vs data plane; the call lifecycle |

### Part II — Protocol & Transport
| # | Chapter | Focus |
|---|---------|-------|
| 05 | [Workspace, Crate Layout & Module Boundaries](plan/05-workspace.md) | The four crates and their seams |
| 06 | [Wire Protocol: Framing, Opcodes & Versioning](plan/06-protocol.md) | Frame format, opcodes, negotiation |
| 07 | [Serialization & Encoding Strategy](plan/07-serialization.md) | Zero-copy encoding, layout, safety |
| 08 | [Transport Layer: vsock Control + ivshmem Bulk](plan/08-transport.md) | Channels, rings, backpressure |

### Part III — Client & Server Cores
| # | Chapter | Focus |
|---|---------|-------|
| 09 | [Client Shim: Interception & Dispatch](plan/09-client-shim.md) | cdylib symbol export, dispatch tables |
| 10 | [Server Core: Decode, Validate, Replay](plan/10-server-core.md) | Event loop, validate-before-replay |
| 11 | [Handle & Object Lifetime Management](plan/11-handles.md) | Generational id tables, destruction order |
| 12 | [Memory Management & Zero-Copy Bulk Transfer](plan/12-memory.md) | Shared region, copy-to-private (TOCTOU) |
| 13 | [Synchronization, Fences & Async Submission](plan/13-sync.md) | Ordering, batching, latency hiding |

### Part IV — API Backends
| # | Chapter | Focus |
|---|---------|-------|
| 14 | [Vulkan Backend (M1 — The Spine)](plan/14-vulkan.md) | The backbone backend |
| 15 | [OpenGL / GLES / EGL / GLX Backend](plan/15-opengl.md) | GL state machine, native vs Zink |
| 16 | [CUDA Backend](plan/16-cuda.md) | Driver/runtime API, modules, streams |
| 17 | [OpenCL Backend](plan/17-opencl.md) | Cross-vendor compute |
| 18 | [ROCm / HIP Backend](plan/18-rocm-hip.md) | AMD via the portable HIP layer |
| 19 | [Intel Level Zero / oneAPI Backend](plan/19-level-zero.md) | Intel low-level compute |
| 20 | [Video Codec Backends](plan/20-video.md) | VA-API / VDPAU / NVENC / NVDEC / Vulkan Video |
| 21 | [Tier-3 Backends: SYCL, OptiX, AMF, WebGPU](plan/21-tier3.md) | Reach APIs |
| 22 | [Presentation, Surfaces & Display Path](plan/22-presentation.md) | Returning frames to the Linux guest |

### Part V — Security & Robustness
| # | Chapter | Focus |
|---|---------|-------|
| 23 | [Security Architecture & Threat Model](plan/23-security.md) | Untrusted stream, validation, sandboxing |
| 24 | [Error Handling & FFI Safety](plan/24-error-ffi.md) | thiserror, ABI boundary, unsafe isolation |

### Part VI — Performance, QA & Delivery
| # | Chapter | Focus |
|---|---------|-------|
| 25 | [Performance Engineering](plan/25-performance.md) | Latency budget, batching, benchmarks |
| 26 | [Testing & Quality Assurance](plan/26-testing.md) | CTS via remoting, fuzzing, golden traces |
| 27 | [Observability: Logging, Tracing & Metrics](plan/27-observability.md) | tracing spans, metrics, replay tooling |
| 28 | [Build, Packaging, Distribution & Configuration](plan/28-build-dist.md) | .so shims, server binary, pairing |
| 29 | [Versioning, Compatibility & Deprecation](plan/29-versioning.md) | SemVer, wire-protocol skew |
| 30 | [Detailed Milestone Plan (M0–M5)](plan/30-milestones.md) | Task breakdown & acceptance criteria |

### Part VII — Reference
| # | Chapter | Focus |
|---|---------|-------|
| 31 | [Risks, Open Questions & Decision Log](plan/31-risks.md) | Risk register, ADR-style decisions |
| 32 | [Appendices: Opcode Tables, Glossary & References](plan/32-appendices.md) | Templates, glossary, crate inventory |

## Related documents

- [ROADMAP.md](ROADMAP.md) — the condensed milestone checklist (Ch. 30 is the detailed expansion).
- [../ARCHITECTURE.md](../ARCHITECTURE.md) — the user-facing architecture overview.
- [../FEATURES.md](../FEATURES.md) — the API coverage matrix.
- [../../SECURITY.md](../../SECURITY.md) — the security policy (Ch. 23 is the detailed threat model).
