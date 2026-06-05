# 변경 이력

[English](../CHANGELOG.md) | **한국어**

이 프로젝트의 모든 주요 변경 사항은 이 파일에 기록됩니다.

이 형식은 [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)를 기반으로 하며,
이 프로젝트는 [Semantic Versioning](https://semver.org/spec/v2.0.0.html)을 따릅니다.

이 프로젝트는 버전 0.0.0의 초기 개발 단계이며 안정화 이전 상태입니다. 아직
릴리스된 것이 없으므로 아래에는 태그가 지정된 버전이 없습니다.

## [Unreleased]

### Added

- 일곱 개의 crate로 구성된 Cargo workspace (edition 2021, MSRV 1.75):
  - `graftx-protocol` — wire protocol: 프레임 헤더, opcode 체계, `Hello`/`Welcome` 핸드셰이크, 세대형(generational) `Handle`, 커맨드 인코딩/디코딩, `PROTOCOL_VERSION`, `ProtocolError`.
  - `graftx-transport` — 인프로세스 `Loopback`, 길이 접두(length-prefixed) `StreamTransport`, TCP serve 루프를 갖춘 `Transport` trait.
  - `graftx-client` — 가로챈 C 심볼을 export하는 Linux API shim (cdylib + rlib).
  - `graftx-server` — transport 위에서 API별 백엔드 디스패치를 수행하는 `Session`.
  - `graftx-handles` — 안전한 핸들 수명 추적을 위한 세대형 `HandleTable`.
  - `graftx-obs` — 관측성(observability) 기본 요소 (`CallStats`, `ObsRegistry`, `CallGuard`).
  - `graftx-xtask` — 개발자 도구 (`gen-opcodes`, `check-xrefs`, `coverage`).
- 세대형 핸들 수명을 갖추고 remoting 경로로 연결된 스텁(stub) GPU 백엔드들:
  - Vulkan — instance, physical device, device, queue, memory, buffer.
  - OpenGL — context, buffer.
  - CUDA — context, alloc, free.
  - HIP — alloc, free, stream.
- MIT 라이선스 (Copyright (c) 2026 Kim DaeHyun).
- GPU API-remoting 아키텍처와 프로젝트 목표를 문서화한 README.
- 기여자 문서 (no-AI-attribution 정책을 포함한 CONTRIBUTING.md) 및 보안 정책.
- `cargo build`, `cargo test`, `cargo clippy -D warnings`, `cargo fmt --check`를 실행하는 지속적 통합(CI).

### Notes

- 이 프로젝트는 릴리스된 버전이 없는, 버전 0.0.0의 안정화 이전 상태입니다. 모든 GPU 백엔드는
  현재 종단 간(end-to-end) remoting 경로를 검증하는 스텁이며, 아직 네이티브 드라이버를 호출하지 않습니다.
