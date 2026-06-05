<div align="center">

<h1>GraftX</h1>

### Run GPU workloads on a Linux guest — using a GPU that lives in a Windows guest.

<p>An API-remoting layer in Rust. A Linux guest's OpenGL / Vulkan / CUDA / ROCm calls are<br>
serialized, shipped over a fast guest-to-guest channel, and replayed on the Windows guest<br>
that owns the physical GPU via PCI passthrough.</p>

<pre><code># Build the workspace (Rust stable via rustup)
cargo build --workspace

# Test + lint
cargo test --workspace
cargo clippy --all-targets -- -D warnings</code></pre>

[![status](https://img.shields.io/badge/status-early%20development-red?style=for-the-badge)](#status-early-development)

[![license](https://img.shields.io/github/license/kernalix7/graftx?style=flat-square&color=blue)](LICENSE)
[![rust](https://img.shields.io/badge/rust-stable-orange?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![CI](https://img.shields.io/github/actions/workflow/status/kernalix7/graftx/ci.yml?branch=main&style=flat-square&label=CI)](https://github.com/kernalix7/graftx/actions/workflows/ci.yml)
[![stars](https://img.shields.io/github/stars/kernalix7/graftx?style=flat-square&color=FFD93D&logo=github&logoColor=white)](https://github.com/kernalix7/graftx/stargazers)

###### Forwards GPU APIs for

[![NVIDIA](https://img.shields.io/badge/NVIDIA-76B900?style=flat-square&logo=nvidia&logoColor=white)](docs/FEATURES.md)
[![AMD](https://img.shields.io/badge/AMD-ED1C24?style=flat-square&logo=amd&logoColor=white)](docs/FEATURES.md)
[![Intel](https://img.shields.io/badge/Intel-0071C5?style=flat-square&logo=intel&logoColor=white)](docs/FEATURES.md)
[![Vulkan](https://img.shields.io/badge/Vulkan-A41E22?style=flat-square&logo=vulkan&logoColor=white)](docs/FEATURES.md)

<sub>**English** &nbsp;·&nbsp; [한국어](docs/README.ko.md) &nbsp;·&nbsp; [Features](docs/FEATURES.md) &nbsp;·&nbsp; [Architecture](docs/ARCHITECTURE.md) &nbsp;·&nbsp; [Comparison](docs/COMPARISON.md)</sub>

</div>

---

> ### Status: Early development
> GraftX is at **v0.0.0** and **pre-stable** — none of the wire protocol, transport, or APIs are frozen. The Cargo workspace spans several crates (`graftx-protocol`, `graftx-handles`, `graftx-transport`, `graftx-client`, `graftx-server`, plus tooling), and the remoting path now runs end-to-end over a real TCP serve loop: a client encodes calls, the server decodes, validates, and dispatches them, and results flow back. Eleven GPU-API backends — Vulkan, OpenGL, CUDA, OpenCL, HIP, Level Zero, Video, WebGPU, OptiX, SYCL, AMF — are wired into that path, but they are **pure-Rust stubs**: they track object lifetimes in generational handle tables and make **no native driver, GPU, or FFI calls** yet. Nothing here is production-ready. See the [roadmap](docs/design/ROADMAP.md). Please file issues at <https://github.com/kernalix7/graftx/issues>.

In many virtualization setups a single GPU is handed to **one** VM through PCI passthrough — usually a Windows guest, where vendor drivers and tooling are best supported. Other guests on the same host are left without acceleration.

GraftX bridges that gap: a Linux guest keeps using its normal GPU APIs unmodified, while the actual work runs on the GPU-owning Windows guest. No second GPU, no rebooting to switch which VM gets the card.

## Goals

In priority order:

1. **Breadth** — cover as many GPU APIs and versions as possible.
2. **Performance** — minimize the overhead the remoting layer adds.
3. **Stability** — correct, reliable behavior under real workloads.
4. **Safety** — validate the untrusted command stream before replaying it against native drivers.

## How it works

```
┌─────────────────┐        transport        ┌──────────────────────┐
│  Linux guest    │  ───────────────────▶   │  Windows guest        │
│                 │   serialized API calls  │  (owns physical GPU)  │
│  app ─▶ GraftX  │                         │  GraftX ─▶ real driver│
│  client shim    │  ◀───────────────────   │  GPU executes         │
└─────────────────┘     results / surfaces  └──────────────────────┘
```

- **Client shim** (Linux): intercepts the GPU API, encodes calls.
- **Transport**: low-latency guest-to-guest channel (shared memory / virtio-vsock).
- **Server** (Windows): decodes and replays calls against the native driver, returns results.

Full design: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Target APIs

Coverage rolls out in tiers — Tier 1 is the backbone (NVIDIA + AMD + Intel + cross-vendor compute). The full matrix lives in [docs/FEATURES.md](docs/FEATURES.md).

- **Tier 1** — Vulkan (the spine), OpenGL, OpenGL ES, EGL, CUDA, OpenCL
- **Tier 2** — HIP (ROCm), Level Zero, VA-API, VDPAU, NVENC/NVDEC, Vulkan Video
- **Tier 3** — SYCL, OptiX, AMF, WebGPU, GLX

## Requirements

- A host running **both** guests (e.g. QEMU/KVM) with a low-latency guest-to-guest channel — **virtio-vsock** or **ivshmem** shared memory.
- A **Windows guest** with a physical GPU via **PCI passthrough** and vendor drivers installed.
- A **Linux guest** (the API consumer).
- Rust stable toolchain to build (`rust-toolchain.toml` pins the channel + components).

## Building

```bash
cargo build --workspace          # build all crates
cargo test --workspace           # run tests
cargo clippy --all-targets -- -D warnings   # lint
cargo fmt --check                # format check
```

The workspace splits into several crates under `crates/` — see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Documentation

| Topic | English | 한국어 |
|---|---|---|
| Feature / API matrix | [FEATURES.md](docs/FEATURES.md) | [FEATURES.ko.md](docs/FEATURES.ko.md) |
| Architecture | [ARCHITECTURE.md](docs/ARCHITECTURE.md) | [ARCHITECTURE.ko.md](docs/ARCHITECTURE.ko.md) |
| Comparison vs. prior art | [COMPARISON.md](docs/COMPARISON.md) | [COMPARISON.ko.md](docs/COMPARISON.ko.md) |
| README | — | [README.ko.md](docs/README.ko.md) |
| Wire-protocol reference | [PROTOCOL.md](docs/design/PROTOCOL.md) | — |
| Backend status matrix | [BACKENDS.md](docs/design/BACKENDS.md) | — |
| Security model | [SECURITY_MODEL.md](docs/design/SECURITY_MODEL.md) | — |

Engineering / design notes live under [docs/design/](docs/design/).

## Contributing

Contributions welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for build/test setup and the PR process, [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md), and [SECURITY.md](SECURITY.md) for reporting vulnerabilities (do not open a public issue). Third-party licenses: [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md).

## Star History

<a href="https://star-history.com/#kernalix7/graftx&Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=kernalix7/graftx&type=Date&theme=dark" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=kernalix7/graftx&type=Date" />
    <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=kernalix7/graftx&type=Date" />
  </picture>
</a>

## Support

[![GitHub Sponsors](https://img.shields.io/badge/Sponsor-GitHub-EA4AAA?logo=githubsponsors&logoColor=white&style=for-the-badge)](https://github.com/sponsors/kernalix7)
[![Ko-fi](https://img.shields.io/badge/Ko--fi-F16061?logo=ko-fi&logoColor=white&style=for-the-badge)](https://ko-fi.com/kernalix7)
[![Fairy](https://img.shields.io/badge/🧚_Fairy-EE6E73?style=for-the-badge&logoColor=white)](https://fairy.hada.io/@kernalix7)

GitHub Sponsors supports recurring or one-time sponsorship; Ko-fi handles international cards and PayPal; fairy.hada.io is a Korean tipping platform. Bug reports, PRs, and stars on the repo are equally appreciated and free.

## License

[MIT](LICENSE) — Kim DaeHyun (kernalix7@kodenet.io)
