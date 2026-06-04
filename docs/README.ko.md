<div align="center">

<h1>GraftX</h1>

### Windows 게스트에 있는 GPU를 사용해 Linux 게스트에서 GPU 워크로드를 실행하세요.

<p>Rust로 작성된 API 리모팅 계층입니다. Linux 게스트의 OpenGL / Vulkan / CUDA / ROCm 호출을<br>
직렬화하여 빠른 게스트 간 채널로 전송하고, PCI passthrough를 통해 물리 GPU를 소유한<br>
Windows 게스트에서 다시 재생합니다.</p>

<pre><code># Build the workspace (Rust stable via rustup)
cargo build --workspace

# Test + lint
cargo test --workspace
cargo clippy --all-targets -- -D warnings</code></pre>

[![status](https://img.shields.io/badge/status-early%20development-red?style=for-the-badge)](#status-early-development)

[![license](https://img.shields.io/github/license/kernalix7/graftx?style=flat-square&color=blue)](../LICENSE)
[![rust](https://img.shields.io/badge/rust-stable-orange?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![CI](https://img.shields.io/github/actions/workflow/status/kernalix7/graftx/ci.yml?branch=main&style=flat-square&label=CI)](https://github.com/kernalix7/graftx/actions/workflows/ci.yml)
[![stars](https://img.shields.io/github/stars/kernalix7/graftx?style=flat-square&color=FFD93D&logo=github&logoColor=white)](https://github.com/kernalix7/graftx/stargazers)

###### Forwards GPU APIs for

[![NVIDIA](https://img.shields.io/badge/NVIDIA-76B900?style=flat-square&logo=nvidia&logoColor=white)](FEATURES.ko.md)
[![AMD](https://img.shields.io/badge/AMD-ED1C24?style=flat-square&logo=amd&logoColor=white)](FEATURES.ko.md)
[![Intel](https://img.shields.io/badge/Intel-0071C5?style=flat-square&logo=intel&logoColor=white)](FEATURES.ko.md)
[![Vulkan](https://img.shields.io/badge/Vulkan-A41E22?style=flat-square&logo=vulkan&logoColor=white)](FEATURES.ko.md)

<sub>[English](../README.md) &nbsp;·&nbsp; **한국어** &nbsp;·&nbsp; [Features](FEATURES.ko.md) &nbsp;·&nbsp; [Architecture](ARCHITECTURE.ko.md) &nbsp;·&nbsp; [Comparison](COMPARISON.ko.md)</sub>

</div>

---

> ### Status: Early development
> GraftX는 **v0.0.0** 단계입니다 — Cargo workspace와 네 개의 crate(`graftx-protocol`, `graftx-transport`, `graftx-client`, `graftx-server`)는 골격이 잡혀 있지만, wire 프로토콜, transport, API 백엔드는 **아직 구현되지 않았거나 안정화되지 않았습니다**. 여기 있는 어떤 것도 프로덕션에 사용할 수 없습니다. 첫 번째 마일스톤은 transport를 통한 Vulkan 왕복(round-trip)입니다. [로드맵](design/ROADMAP.md)을 참고하세요. 이슈는 <https://github.com/kernalix7/graftx/issues>에 등록해 주세요.

많은 가상화 환경에서는 단일 GPU가 PCI passthrough를 통해 **하나의** VM에만 전달됩니다 — 보통 벤더 드라이버와 도구 지원이 가장 좋은 Windows 게스트입니다. 같은 호스트의 다른 게스트들은 가속 없이 남겨집니다.

GraftX는 이 간극을 메웁니다. Linux 게스트는 평소의 GPU API를 수정 없이 그대로 사용하지만, 실제 작업은 GPU를 소유한 Windows 게스트에서 실행됩니다. 두 번째 GPU도, 어느 VM이 카드를 차지할지 전환하기 위한 재부팅도 필요 없습니다.

## Goals

우선순위 순서대로:

1. **Breadth(범위)** — 가능한 한 많은 GPU API와 버전을 지원합니다.
2. **Performance(성능)** — 리모팅 계층이 추가하는 오버헤드를 최소화합니다.
3. **Stability(안정성)** — 실제 워크로드에서 올바르고 신뢰할 수 있는 동작을 보장합니다.
4. **Safety(안전성)** — 네이티브 드라이버에 대해 재생하기 전에 신뢰할 수 없는 명령 스트림을 검증합니다.

## How it works

```
┌─────────────────┐        transport        ┌──────────────────────┐
│  Linux guest    │  ───────────────────▶   │  Windows guest        │
│                 │   serialized API calls  │  (owns physical GPU)  │
│  app ─▶ GraftX  │                         │  GraftX ─▶ real driver│
│  client shim    │  ◀───────────────────   │  GPU executes         │
└─────────────────┘     results / surfaces  └──────────────────────┘
```

- **Client shim** (Linux): GPU API를 가로채어 호출을 인코딩합니다.
- **Transport**: 저지연 게스트 간 채널(shared memory / virtio-vsock)입니다.
- **Server** (Windows): 호출을 디코딩하여 네이티브 드라이버에 대해 재생하고 결과를 반환합니다.

전체 설계: [ARCHITECTURE.ko.md](ARCHITECTURE.ko.md).

## Target APIs

지원 범위는 단계(tier)별로 확장됩니다 — Tier 1이 근간입니다(NVIDIA + AMD + Intel + 벤더 간 compute). 전체 매트릭스는 [FEATURES.ko.md](FEATURES.ko.md)에 있습니다.

- **Tier 1** — Vulkan(척추 역할), OpenGL, OpenGL ES, EGL, CUDA, OpenCL
- **Tier 2** — HIP (ROCm), Level Zero, VA-API, VDPAU, NVENC/NVDEC, Vulkan Video
- **Tier 3** — SYCL, OptiX, AMF, WebGPU, GLX

## Requirements

- 저지연 게스트 간 채널(**virtio-vsock** 또는 **ivshmem** shared memory)을 갖춘, **두** 게스트를 모두 실행하는 호스트(예: QEMU/KVM).
- **PCI passthrough**를 통한 물리 GPU와 벤더 드라이버가 설치된 **Windows 게스트**.
- **Linux 게스트**(API 소비자).
- 빌드를 위한 Rust stable 툴체인(`rust-toolchain.toml`이 채널 + 컴포넌트를 고정합니다).

## Building

```bash
cargo build --workspace          # build all crates
cargo test --workspace           # run tests
cargo clippy --all-targets -- -D warnings   # lint
cargo fmt --check                # format check
```

workspace는 `crates/` 아래 네 개의 crate로 나뉩니다 — [ARCHITECTURE.ko.md](ARCHITECTURE.ko.md)를 참고하세요.

## Documentation

| 주제 | English | 한국어 |
|---|---|---|
| 기능 / API 매트릭스 | [FEATURES.md](FEATURES.md) | [FEATURES.ko.md](FEATURES.ko.md) |
| 아키텍처 | [ARCHITECTURE.md](ARCHITECTURE.md) | [ARCHITECTURE.ko.md](ARCHITECTURE.ko.md) |
| 기존 기술과의 비교 | [COMPARISON.md](COMPARISON.md) | [COMPARISON.ko.md](COMPARISON.ko.md) |
| README | — | [README.ko.md](README.ko.md) |

엔지니어링 / 설계 노트는 [design/](design/) 아래에 있습니다.

## Contributing

기여를 환영합니다. 빌드/테스트 설정과 PR 절차는 [CONTRIBUTING.ko.md](CONTRIBUTING.ko.md), 행동 강령은 [CODE_OF_CONDUCT.ko.md](CODE_OF_CONDUCT.ko.md), 취약점 보고는 [SECURITY.ko.md](SECURITY.ko.md)를 참고하세요(공개 이슈를 열지 마세요). 서드파티 라이선스: [THIRD_PARTY_LICENSES.ko.md](THIRD_PARTY_LICENSES.ko.md).

## Star History

<a href="https://star-history.com/#kernalix7/graftx&Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=kernalix7/graftx&type=Date&theme=dark" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=kernalix7/graftx&type=Date" />
    <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=kernalix7/graftx&type=Date" />
  </picture>
</a>

## 후원 / Support

[![GitHub Sponsors](https://img.shields.io/badge/Sponsor-GitHub-EA4AAA?logo=githubsponsors&logoColor=white&style=for-the-badge)](https://github.com/sponsors/kernalix7)
[![Ko-fi](https://img.shields.io/badge/Ko--fi-F16061?logo=ko-fi&logoColor=white&style=for-the-badge)](https://ko-fi.com/kernalix7)
[![Fairy](https://img.shields.io/badge/🧚_Fairy-EE6E73?style=for-the-badge&logoColor=white)](https://fairy.hada.io/@kernalix7)

GitHub Sponsors 는 정기 / 일시 후원; Ko-fi 는 해외 카드 / PayPal 결제; fairy.hada.io 는 국내 결제용. 버그 리포트, PR, 스타도 똑같이 환영하며 무료입니다.

## 라이선스

[MIT](../LICENSE) — Kim DaeHyun (kernalix7@kodenet.io)
