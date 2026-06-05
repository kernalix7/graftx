# 01. Introduction, Vision & Document Scope

> The charter for the GraftX implementation plan: what we are building, the gap it fills, who this document set is for, how to read it, the conventions it obeys, and what "done" means for the plan itself.

## 1.1 Vision

GraftX exists to answer a single, concrete question: *when one physical GPU is
handed to one virtual machine through PCI passthrough, how does every other
guest on that host get to use it?* The common arrangement on a workstation-class
host is to pass the GPU to a **Windows guest** — where vendor drivers, control
panels, and tooling are best supported — and leave the Linux guests on the same
host with no acceleration at all. GraftX bridges that gap without a second card
and without rebooting to re-assign the device.

The mechanism is **API remoting**. A Linux-guest application keeps calling its
normal GPU APIs — OpenGL, OpenGL ES, EGL, GLX, Vulkan, CUDA, OpenCL, ROCm/HIP,
Level Zero, video codecs (VA-API, VDPAU, NVENC/NVDEC, Vulkan Video), WebGPU,
OptiX, AMF, SYCL — **completely unmodified**. A client shim intercepts each
call at its C entry point, serializes it, ships it across a fast guest-to-guest
transport, and a server on the GPU-owning Windows guest decodes, validates, and
replays it against the native driver. Results and surfaces flow back.

```
   LINUX GUEST                                       WINDOWS GUEST
   (no GPU of its own)                               (owns the GPU via passthrough)
 ┌────────────────────┐                            ┌──────────────────────────────┐
 │ app → libvulkan.so │   serialized API calls     │ graftx-server → real driver  │
 │     (GraftX shim)  │  ───────────────────────▶  │      → physical GPU          │
 │                    │  ◀───────────────────────  │                              │
 └────────────────────┘   results / surfaces       └──────────────────────────────┘
                       guest-to-guest transport
                  (virtio-vsock control + ivshmem bulk)
```

The vision in one sentence: **make the GPU in the Windows guest look, to an
unmodified Linux-guest application, like a GPU the Linux guest owns** — at an
overhead that is a small constant on top of native driver time rather than a
per-call latency tax.

This is deliberately *not*:

- **Paravirtualization** (virtio-GPU / Venus / VirGL) — those expose a
  host-virtualized GPU device; GraftX forwards API *calls* to a real driver.
- **Pixel / framebuffer streaming** (Looking Glass, Sunshine/Moonlight) — those
  ship a finished framebuffer or an encoded video stream; GraftX ships the
  *commands*, so the remote side does the rendering with full fidelity.
- **Network GPU remoting** (rCUDA) — GraftX targets a **local, paired
  guest-to-guest** channel between two VMs on the same host, not a network link;
  this shapes both the transport design and the threat model.

The full conceptual framing, the data-flow diagram, and the comparison to
neighboring technologies live in [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md)
and [`../../COMPARISON.md`](../../COMPARISON.md). This plan does not re-derive
them; it turns that architecture into an ordered, testable construction sequence.

## 1.2 The gap GraftX fills

GraftX sits in a niche that existing tools leave open. The table below positions
it; the deeper treatment is in the comparison doc, so this is a summary only.

| Need on a single-GPU passthrough host        | Existing answer            | Why it falls short for the Linux guest                 |
| --------------------------------------------- | -------------------------- | ------------------------------------------------------ |
| Linux guest runs real GPU compute/graphics    | Buy a second GPU           | Cost, slots, power; not always possible on a laptop    |
| Share one GPU across VMs                       | SR-IOV / vGPU              | Vendor-/SKU-locked, licensed, rarely on consumer cards |
| Linux guest sees the host's GPU               | virtio-GPU / Venus         | Host must own the GPU; here Windows owns it            |
| Watch the Windows desktop from Linux           | Looking Glass              | Streams *pixels*, gives the Linux app no GPU API       |
| Run CUDA over a link                           | rCUDA                      | Network-oriented, CUDA-only, not maintained for this   |

The unfilled cell is: *a Linux guest, with no GPU and no host access to the
card, that wants to run arbitrary GPU APIs against a card a sibling Windows guest
owns.* That is GraftX's target. The defining constraints that follow from it —
local same-host channel, untrusted-client-to-driver replay, breadth across many
APIs — are what the rest of this plan is organized around.

## 1.3 Priorities (the ordering that decides every trade-off)

When two designs conflict, this fixed priority order breaks the tie. It is
repeated here because nearly every chapter cites it:

1. **Breadth** of API coverage — cover as many GPU APIs and versions as
   possible. A design that forwards more APIs beats a faster one that forwards
   fewer.
2. **Performance** — minimize remoting overhead (round-trip latency and bulk
   data copies).
3. **Stability** — correct, reliable behavior under real workloads.
4. **Safety** — validate the untrusted command stream before it reaches native
   drivers.

Safety being last in *priority* does **not** mean last in *attention*: it is a
hard correctness constraint on a security-critical component (the server replays
an untrusted stream against privileged C/C++ drivers). The ordering means that
when breadth and a safety *nicety* genuinely conflict, breadth wins — but never
at the cost of the non-negotiable validate-before-replay boundary defined in the
Security chapter (Ch.23).

## 1.4 Who this document is for

This plan targets three audiences, in descending frequency:

- **Implementers / maintainers** building GraftX. They need the exact contracts:
  wire framing, the `Transport` trait, handle mapping, shim symbol layout,
  validation rules. They read a chapter end-to-end before writing the code it
  describes.
- **Reviewers** evaluating a design decision or a PR against the plan. They jump
  to one chapter, check the decision tables and trade-off sections, and confirm
  the code matches the documented contract.
- **Informed newcomers** who have read the README and ARCHITECTURE and want to
  understand *how* and *in what order* the system gets built.

Assumed background: working Rust (traits, FFI, `unsafe`, Cargo workspaces); a
mental model of at least one GPU API (Vulkan is used as the running example);
basic familiarity with VM passthrough, virtio-vsock, and shared-memory IPC. The
plan does **not** re-teach Vulkan or QEMU; it cites references where a reader
needs them. It is **not** end-user or operator documentation — install/run/
configure guidance lives one level up in [`../../README.md`](../../../README.md),
per the boundary defined in [`../README.md`](../README.md).

## 1.5 How to read the plan

The chapters form a layered, mostly bottom-up build order. Read straight through
for the full picture, or treat the dependency graph below as a "what must exist
before this works" map.

```
            ┌──────────────────────────────────────────────┐
   Ch.01 ─▶ │ Introduction, Vision & Scope  (this chapter)  │
            └──────────────────────────────────────────────┘
                         │
   Ch.02 ─▶  Goals, scope, non-goals, success criteria
                         │
   Ch.05 ─▶  Workspace & crate layout (graftx-protocol / -transport / -client / -server)
                         │
        ┌────────────────┼─────────────────────────────────┐
        ▼                ▼                                  ▼
   Ch.06 protocol    Ch.08 transport                    Ch.11 handle/object model
   (wire format)     (Transport trait, vsock, ivshmem)  (client↔server handles)
        │                │                                  │
        └────────────────┴──────────────┬───────────────────┘
                                         ▼
   M0 ────▶  round-trip (handshake + no-op end to end)  — Milestones chapter (Ch.30)
                                         │
   M1 ────▶  Vulkan forwarding, the spine (Ch.14)
                                         │
        ┌────────────────────────────────┼───────────────────────────┐
        ▼                                ▼                            ▼
   M2 GL/GLES/EGL/GLX (Ch.15)    M3 CUDA/OpenCL (Ch.16/Ch.17)  Tier 2/3 breadth (Ch.21)
        └────────────────────────────────┴───────────────────────────┘
                                         │
   Ch.25 ─▶  Performance (zero-copy, batching, async)  — runs as a track, not a phase
   Ch.23 ─▶  Security & threat model hardening
   Ch.26 ─▶  Testing, CI, conformance & bring-up
```

(The node labels above mix chapter numbers, which name a subsystem chapter, with
milestone labels M0–M5, which name a build stage in the Milestones chapter
(Ch.30); the authoritative chapter index lives in the plan's `README`/`00` front
matter. Each chapter opens with its own H1 and a one-line abstract, then states
which earlier chapters it depends on.)

Reading guidance by goal:

- **"I want to implement M0 (round-trip)."** Read the Introduction chapter
  (Ch.01) → the Goals chapter (Ch.02) for the success bar, then the Workspace,
  Protocol, Serialization, Transport, and Milestones chapters (Ch.05, Ch.06,
  Ch.07, Ch.08, Ch.30). M0 is the milestone defined in
  [`../ROADMAP.md`](../ROADMAP.md); every chapter that contributes to it
  cross-references that milestone.
- **"I want to add a new forwarded API."** Read the Protocol chapter (Ch.06) for
  wire framing + opcode conventions, the Handles chapter (Ch.11), and the Vulkan
  chapter (Ch.14) as the worked template, then the Tier-3 chapter (Ch.21) for the
  native-forward-vs-funnel-through decision.
- **"I'm reviewing the transport."** Read the Transport chapter (Ch.08) and the
  Security chapter (Ch.23) together — the ivshmem zero-copy/validation tension
  spans both.

Each chapter is self-contained on *its* subsystem and **cross-references
neighbors by chapter number rather than re-explaining them**, so the plan stays
DRY and a change to one subsystem touches one chapter.

## 1.6 Document conventions

These conventions hold across every chapter; they are stated once here and not
repeated.

**Language & tense.** Design docs (everything under `docs/design/`) are
**English-only**, matching the rest of the design tree. End-user docs are
mirrored to Korean (`*.ko.md`); the plan is not. Because the project is at
**v0.0.0** with only skeletons in place, the plan is written in the
**forward-looking / proposed** voice: "the server **will** validate", "the
ivshmem backend **is planned** to…", never "the server validates". When a
sentence describes something that already exists in the skeleton (the four
crates, `PROTOCOL_VERSION`, the `Transport` trait), it says so explicitly and in
the present tense.

**Status markers.** Anything not yet built is tagged **(planned)** the first
time it appears in a chapter, mirroring ARCHITECTURE. A code sketch is a *design
proposal*, not a transcript of existing code.

**Code sketches.** Rust appears in fenced ```rust blocks and is illustrative:
signatures, struct/enum layouts, and key fn bodies. Sketches obey the repo
conventions (no `unwrap()`/`expect()` in library paths; `thiserror` for error
types; `unsafe`/FFI carries a `// SAFETY:` comment), so they double as a style
reference. Wire layouts, sequence walk-throughs, and diagrams use ```text.

```rust
// Convention example: error types use thiserror; library code returns Result.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("unexpected end of frame: need {need} bytes, have {have}")]
    UnexpectedEof { need: usize, have: usize },
    #[error("unknown opcode {0:#010x}")]
    UnknownOpcode(u32),
}
```

**Cross-references.** Chapters cite each other as "the <Subsystem> chapter
(Ch. NN)" and cite the canonical docs by relative path: README ([`../../README.md`](../../../README.md)),
ARCHITECTURE ([`../../ARCHITECTURE.md`](../../ARCHITECTURE.md)), FEATURES
([`../../FEATURES.md`](../../FEATURES.md)), ROADMAP
([`../ROADMAP.md`](../ROADMAP.md)), SECURITY
([`../../SECURITY.md`](../../../SECURITY.md)).

**Terminology** (used consistently throughout):

| Term            | Meaning in this plan                                                       |
| --------------- | -------------------------------------------------------------------------- |
| **client**      | the Linux-guest `graftx-client` shim; **untrusted** from the server's view |
| **server**      | the Windows-guest `graftx-server` replay engine that owns the GPU          |
| **frame**       | one opaque, length-delimited unit handed to `Transport::send`              |
| **command**     | one decoded, validated API call awaiting replay                            |
| **control plane** | small command/handshake frames (virtio-vsock)                            |
| **bulk / data plane** | large buffers/surfaces (ivshmem shared memory)                       |
| **round-trip**  | a call that blocks the app thread awaiting a server reply                   |
| **handle**      | an opaque token mapping a client-side object to a real driver object        |
| **funnel-through** | forwarding API X by translating it onto already-forwarded API Y          |
| **native forward** | forwarding API X by replaying it against the same API X on the server    |

**Versioning.** `PROTOCOL_VERSION` is `0` while pre-stable and is bumped on every
breaking wire change; both ends negotiate it at handshake (see the Protocol
chapter (Ch.06) and the Milestones chapter (Ch.30)). The plan does not promise
wire stability until that version is frozen.

## 1.7 Relationship to README, ARCHITECTURE, FEATURES, ROADMAP

The plan is one node in a small, deliberately layered document graph. To avoid
drift, each document owns a distinct layer and the plan defers to the others
rather than duplicating them.

```text
 README.md ............ what GraftX is + how to build (entry point, end users)
 FEATURES.md .......... the API coverage matrix (which APIs, which tiers)
 ARCHITECTURE.md ...... the *what/why* design: components, data flow, threat model
 design/ROADMAP.md .... the milestone ladder M0..M5 + perf track
 design/plan/ (THIS) .. the *how/in-what-order*: per-subsystem implementation design
```

Layer rules the plan follows:

- **ROADMAP owns the milestones.** M0 (protocol+transport skeleton, handshake,
  no-op round-trip) through M5 (Tier-3 reach APIs) plus the performance track
  are defined there. Plan chapters *map onto* those milestones and cite them;
  they never re-number or redefine them.
- **ARCHITECTURE owns the conceptual model.** The four-crate split, the call
  lifecycle's six stages, the transport trade-off table, the API-coverage
  hybrid strategy (native-forward vs funnel-through with Vulkan as the spine),
  and the trust-boundary diagram all live there. The plan turns each into a
  concrete construction recipe — data structures, function signatures, ordered
  steps — without restating the rationale ARCHITECTURE already gives.
- **FEATURES owns the coverage matrix.** When a plan chapter says "Tier 1
  backbone," the canonical list of which APIs and versions that means is in
  FEATURES; the plan links rather than copies.
- **README owns the user-facing pitch and build commands** (`cargo build
  --workspace`, `cargo test --workspace`, `cargo clippy --all-targets -- -D
  warnings`). The plan reuses those exact commands in its CI/testing chapter.

If any of these documents disagree, the resolution order is: an explicit
**ADR/decision in the plan** supersedes ARCHITECTURE for *implementation*
detail; ARCHITECTURE supersedes the plan for *conceptual* framing; ROADMAP is
authoritative for *milestone scope*. Conflicts are bugs to be reconciled, not
left standing.

## 1.8 Definition of done — for the plan itself

The plan is a deliverable with its own acceptance bar, separate from the code it
describes. The plan is **done** when:

1. **Coverage.** Every crate (`graftx-protocol`, `graftx-transport`,
   `graftx-client`, `graftx-server`) and every milestone (M0–M5 + perf track)
   has at least one chapter that specifies its design to implementation depth —
   concrete signatures, data layouts, and an ordered build sequence, not prose
   alone.
2. **Buildability.** An implementer can take any chapter and produce code from
   it without inventing a contract the plan left undefined. Every wire layout,
   every trait, every handle-mapping rule is pinned down or explicitly deferred
   with a named open question.
3. **Consistency.** No chapter contradicts ARCHITECTURE's conceptual model,
   ROADMAP's milestones, or FEATURES' matrix; cross-references resolve; the
   priority order (breadth > performance > stability > safety) is applied the
   same way everywhere.
4. **Convention compliance.** All code sketches honor the repo conventions
   (edition 2021, MSRV 1.75, no `unwrap()`/`expect()` in lib paths, `thiserror`
   errors, `// SAFETY:` on unsafe, `cdylib` shims exporting C symbols). The docs
   are English-only and use the forward-looking voice.
5. **Self-contained scope per chapter.** Each chapter covers its own subsystem
   fully and defers to neighbors by number — no subsystem is explained twice and
   none is left to "some other chapter" without a number.
6. **Traceability to the security boundary.** Every chapter that moves
   attacker-influenced data states where it sits relative to the
   validate-before-replay boundary, so the threat model in the Security chapter
   (Ch.23) has no gaps to fill in after the fact.

A reviewer signs off the plan against this checklist. What the plan is explicitly
**not** responsible for: it is not the code, not the test suite, and not a
guarantee that the design as written will hit a given performance number — those
are validated by the implementation and the M0–M5 milestones, not by this
document. The plan's job is to make the build *unambiguous and ordered*; proving
it *correct and fast* is the milestones' job.

With the charter set, the Goals chapter (Ch.02) fixes the precise goals,
non-goals, and per-milestone success criteria that the rest of the plan is
measured against.
