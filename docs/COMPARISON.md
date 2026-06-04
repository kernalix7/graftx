# Comparison

**English** | [한국어](COMPARISON.ko.md)

How GraftX relates to existing GPU virtualization and remoting projects. GraftX's distinguishing position: **guest-to-guest**, **multi-API (including compute)**, forwarding **API calls** rather than pixels or framebuffers.

## Prior art

| Project | What it does | How GraftX differs |
| --- | --- | --- |
| **virtio-GPU + Venus / VirGL** | Host → guest OpenGL/Vulkan paravirtualization | Guest → guest, multi-API including compute (CUDA/OpenCL/ROCm) |
| **Looking Glass** | Low-latency framebuffer relay from a Windows guest to the host | Forwards API calls, not just the framebuffer |
| **Sunshine / Moonlight** | Encoded video game streaming | Remotes API calls, not pixels |
| **rCUDA / cricket** | CUDA remoting over a network | Multi-API, local guest-to-guest transport |
| **DXVK / VKD3D** | Translate Direct3D → Vulkan inside one machine | Cross-guest transport of unmodified GPU APIs, not an in-process API translation |

## Why guest-to-guest

The usual passthrough setup gives the physical GPU to exactly one VM — typically a Windows guest, where vendor drivers (NVIDIA/AMD/Intel) and tooling are most complete. Everything else on the host loses acceleration.

GraftX takes the opposite tack from "relay the Windows screen to the host" tools (Looking Glass) or "stream encoded video" tools (Sunshine): instead of moving **pixels**, it moves the **API calls themselves** from the Linux guest to the Windows guest, and only the results come back. That keeps the Linux application unmodified and lets compute APIs (CUDA, OpenCL, ROCm) work, not just graphics.

## Trade-offs

- **Versus passthrough to the Linux guest:** GraftX adds remoting overhead but needs no second GPU and no reboot to reassign the card.
- **Versus framebuffer/video relay:** GraftX supports compute and unmodified APIs, but must implement and maintain per-API forwarding surfaces.
- **Versus network GPU remoting (rCUDA):** GraftX is local guest-to-guest (shared memory / virtio-vsock), so latency is far lower, but it is scoped to co-located VMs rather than a cluster.
