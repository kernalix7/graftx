# Changelog

**English** | [한국어](docs/CHANGELOG.ko.md)

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The project is in early development at version 0.0.0 and is pre-stable; nothing
is released yet, so there are no tagged versions below.

## [Unreleased]

### Added

- Cargo workspace (edition 2021, MSRV 1.75) with seven crates:
  - `graftx-protocol` — wire protocol: frame header, opcode scheme, `Hello`/`Welcome` handshake, generational `Handle`, command encode/decode, `PROTOCOL_VERSION`, `ProtocolError`.
  - `graftx-transport` — `Transport` trait with an in-process `Loopback`, a length-prefixed `StreamTransport`, and a TCP serve loop.
  - `graftx-client` — Linux API shims (cdylib + rlib) exporting intercepted C symbols.
  - `graftx-server` — `Session` with per-API backend dispatch over the transport.
  - `graftx-handles` — generational `HandleTable` for safe handle lifetime tracking.
  - `graftx-obs` — observability primitives (`CallStats`, `ObsRegistry`, `CallGuard`).
  - `graftx-xtask` — developer tooling (`gen-opcodes`, `check-xrefs`, `coverage`).
- Stub GPU backends wired through the remoting path with generational handle lifetime, covering:
  - Vulkan — instance, physical device, device, queue, memory, buffer.
  - OpenGL — context, buffer.
  - CUDA — context, alloc, free.
  - HIP — alloc, free, stream.
- MIT license (Copyright (c) 2026 Kim DaeHyun).
- README documenting the GPU API-remoting architecture and project goals.
- Contributor documentation (CONTRIBUTING.md, including the no-AI-attribution policy) and security policy.
- Continuous integration running `cargo build`, `cargo test`, `cargo clippy -D warnings`, and `cargo fmt --check`.

### Notes

- The project is pre-stable at version 0.0.0 with no released versions. All GPU backends are
  currently stubs that validate the end-to-end remoting path; they do not yet make native driver calls.
