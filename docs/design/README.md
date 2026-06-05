# docs/design/

This directory holds **engineering-internal** documentation for GraftX: design
specs, roadmaps, protocol and transport reference, and security reviews. It is
written for contributors and maintainers who work on the implementation, not for
end users.

End-user and operator documentation lives one level up in [`../`](../) — start
there if you want to install, configure, or run GraftX. See
[`../README.md`](../README.md).

## What belongs here

A document belongs in `docs/design/` when:

- It describes **how GraftX is built or intended to be built**, rather than how
  to use it — architecture decisions, subsystem designs, data-flow models.
- It targets **contributors and maintainers**, and assumes familiarity with the
  codebase and the GPU API-remoting model (Linux guest forwarding GPU calls to a
  Windows guest that owns the GPU).
- It defines a **contract that the implementation must honor** — wire protocol,
  transport framing, versioning, or compatibility rules.
- It records **planning or review work** — milestone roadmaps, security threat
  models, design trade-off analyses.

If a document is meant to help someone *run* GraftX rather than *change* it, it
belongs in `../`, not here.

## Index

- [`IMPLEMENTATION_PLAN.md`](IMPLEMENTATION_PLAN.md) — the master implementation
  plan: a 32-chapter, ~200+ page engineering design covering architecture,
  protocol, transport, every API backend, security, performance, testing, and
  delivery. The full chapters live under [`plan/`](plan/).
- [`ROADMAP.md`](ROADMAP.md) — condensed milestone plan (M0–M5 plus performance
  work). Chapter 30 of the implementation plan is the detailed expansion.
- [`PROTOCOL.md`](PROTOCOL.md) — wire-protocol reference: transport vs. protocol
  frame layering, the 28-byte `FrameHeader`, `FrameKind`, the opcode scheme and
  `ApiId` table, the `Hello`/`Welcome` handshake, and the 64-bit `Handle`
  layout, as implemented in `crates/graftx-protocol`.
- [`BACKENDS.md`](BACKENDS.md) — per-API status matrix tracking each GPU API
  through the remoting pipeline (protocol codecs → server backend → client shim
  → end-to-end test), as implemented in the `graftx-protocol`, `graftx-server`,
  and `graftx-client` crates. All backends are remoting-path stubs.
- [`SECURITY_MODEL.md`](SECURITY_MODEL.md) — security posture as implemented vs.
  planned: the untrusted-client threat model and the validation the server
  enforces today (framing checks, opcode-to-registered-backend routing,
  generational and parent-handle validation in `graftx-server`/`graftx-handles`),
  plus the planned defenses (copy-to-private/TOCTOU, sandboxing, per-session
  auth, quotas) cross-referenced to [`plan/23-security.md`](plan/23-security.md).

Protocol and transport design specs are covered in detail by the implementation
plan ([`plan/06-protocol.md`](plan/06-protocol.md),
[`plan/08-transport.md`](plan/08-transport.md)); standalone specs may be split
out here as those subsystems stabilize.
