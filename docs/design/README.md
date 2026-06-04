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

- [`ROADMAP.md`](ROADMAP.md) — milestone plan (M0–M5 plus performance work).

Protocol and transport design specs will be added here as those subsystems are
designed.
