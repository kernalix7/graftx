# Changelog

**English** | [한국어](docs/CHANGELOG.ko.md)

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The project is in early development at version 0.0.0 and is pre-stable; nothing
is released yet, so there are no tagged versions below.

## [Unreleased]

### Added

- Cargo workspace scaffolding (edition 2021, MSRV 1.75) with four crates:
  - `graftx-protocol` — wire format and command encode/decode, `PROTOCOL_VERSION`, `ProtocolError`.
  - `graftx-transport` — `Transport` trait (send/recv) over virtio-vsock / ivshmem.
  - `graftx-client` — Linux API shims (cdylib + rlib) exporting intercepted C symbols.
  - `graftx-server` — Windows-side replay against native GPU drivers.
- MIT license (Copyright (c) 2026 Kim DaeHyun).
- README documenting the GPU API-remoting architecture and project goals.
- Contributor documentation (CONTRIBUTING.md, including the no-AI-attribution policy) and security policy.
- Continuous integration running `cargo build`, `cargo test`, `cargo clippy -D warnings`, and `cargo fmt --check`.
