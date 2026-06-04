# 변경 이력

[English](../CHANGELOG.md) | **한국어**

이 프로젝트의 모든 주요 변경 사항은 이 파일에 기록됩니다.

이 형식은 [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)를 기반으로 하며,
이 프로젝트는 [Semantic Versioning](https://semver.org/spec/v2.0.0.html)을 따릅니다.

이 프로젝트는 버전 0.0.0의 초기 개발 단계이며 안정화 이전 상태입니다. 아직
릴리스된 것이 없으므로 아래에는 태그가 지정된 버전이 없습니다.

## [Unreleased]

### Added

- 네 개의 crate로 구성된 Cargo workspace 골격 (edition 2021, MSRV 1.75):
  - `graftx-protocol` — wire format 및 커맨드 인코딩/디코딩, `PROTOCOL_VERSION`, `ProtocolError`.
  - `graftx-transport` — virtio-vsock / ivshmem 위에서 동작하는 `Transport` trait (send/recv).
  - `graftx-client` — 가로챈 C 심볼을 export하는 Linux API shim (cdylib + rlib).
  - `graftx-server` — 네이티브 GPU 드라이버에 대한 Windows 측 replay.
- MIT 라이선스 (Copyright (c) 2026 Kim DaeHyun).
- GPU API-remoting 아키텍처와 프로젝트 목표를 문서화한 README.
- 기여자 문서 (no-AI-attribution 정책을 포함한 CONTRIBUTING.md) 및 보안 정책.
- `cargo build`, `cargo test`, `cargo clippy -D warnings`, `cargo fmt --check`를 실행하는 지속적 통합(CI).
