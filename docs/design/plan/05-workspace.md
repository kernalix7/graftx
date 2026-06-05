# 05. Workspace, Crate Layout & Module Boundaries

This chapter fixes the Cargo workspace shape, the public surface and internal modules of every crate, the dependency graph and feature-flag matrix, the build profiles, and the cross-compilation contract that lets one shared protocol crate serve a Linux client `cdylib` and a Windows server binary.

## 5.1 Goals and constraints

The crate layout is the load-bearing structural decision of GraftX: it decides what is reusable, what is platform-locked, what an attacker-controlled stream can reach, and where the `unsafe` FFI surface lives. We will optimize the workspace for four properties, in priority order matching the project's overall priorities (breadth > performance > stability > safety):

1. **Breadth-friendly growth.** Adding a new API family (e.g. Level Zero after CUDA) must touch a bounded set of files and not force edits across unrelated crates. This argues for *per-API submodules behind a stable internal trait*, not a new crate per API (which would explode the workspace into dozens of members).
2. **A single source of truth for the wire format.** Client encode and server decode must be derived from the same types, in one crate, compiled identically for both targets. Divergence here is a silent corruption bug.
3. **A hard target split.** `graftx-client` only ever compiles for Linux; `graftx-server` only ever compiles for Windows. The split must be enforced by `cfg`/CI, not by convention, so a `cargo build --workspace` on either host degrades gracefully rather than failing on an irrelevant target.
4. **Containment of `unsafe`.** All FFI (the exported C symbols on the client, the native-driver calls on the server) must be reachable only through a thin, audited module so `// SAFETY:` review has a finite surface.

The existing scaffold already encodes the right top-level decision: four crates, `resolver = "2"`, workspace-inherited package metadata, and `thiserror` as the only shared third-party dependency. This chapter keeps that skeleton and specifies the interior.

## 5.2 Workspace topology

```text
graftx/                         workspace root (virtual manifest, no [package])
├── Cargo.toml                  [workspace] members + [workspace.package] + [workspace.dependencies]
├── rust-toolchain.toml         channel = stable, components = rustfmt+clippy
└── crates/
    ├── graftx-protocol/        lib  — wire types, encode/decode, versioning. NO I/O, NO platform code.
    ├── graftx-transport/       lib  — Transport trait, vsock + ivshmem channels, framing.
    ├── graftx-client/          cdylib+rlib — Linux shims, exported C symbols, dispatch table.
    └── graftx-server/          bin  — Windows replay engine, native-driver bindings, sandbox.
```

The root manifest stays a **virtual manifest** (no `[package]` of its own). This keeps `cargo build -p <crate>` and per-target builds clean, and means the root cannot accidentally pull a platform-specific dependency into the shared graph.

**MSRV (single source of truth).** The workspace pins a Minimum Supported Rust Version of **`rust-version = "1.75"`**, declared once in `[workspace.package]` and inherited by every member crate:

```toml
# graftx/Cargo.toml
[workspace.package]
rust-version = "1.75"
```

This is the authoritative MSRV for the whole project; the Testing chapter (Ch. 26) references this number (it does not define its own) and CI enforces it with a `1.75` toolchain job in addition to `stable`. `rust-toolchain.toml` pins the *development* channel (`stable`), while `rust-version` is the *floor* the crates promise to compile on; raising the MSRV is a deliberate, documented change here.

Dependency direction is strictly acyclic and points "inward" toward `graftx-protocol`:

```text
        graftx-client (Linux cdylib)        graftx-server (Windows bin)
                  │   │                            │   │
                  │   └──────────┐      ┌──────────┘   │
                  ▼              ▼      ▼              ▼
          graftx-transport   graftx-protocol   graftx-transport
                  │                                   │
                  └──────────────► graftx-protocol ◄──┘

  protocol  : depends on nothing in the workspace (leaf).
  transport : depends on protocol (frames carry protocol-defined headers).
  client    : depends on protocol + transport.
  server    : depends on protocol + transport.
```

`graftx-protocol` is the leaf. `graftx-transport` depends on it because the framing layer needs the protocol's session-header and command-header types to length-prefix and route frames (the transport must not redefine them). Client and server sit at the top and never depend on each other — the only thing they share is `protocol` (types) and `transport` (the channel). This is what makes the wire format a true contract: there is exactly one definition, in one crate, and both ends `use` it.

**Why not a fifth "common/util" crate?** Tempting for shared error helpers, but it tends to become a dumping ground that re-introduces cycles. We will instead keep tiny shared helpers in `graftx-protocol` (it is already a universal dependency) and accept a small amount of duplication for genuinely platform-specific helpers.

## 5.3 `graftx-protocol` — the shared contract

Public surface (re-exported from `lib.rs`):

```rust
// crates/graftx-protocol/src/lib.rs
#![forbid(unsafe_code)] // protocol is pure data; FFI lives only in client/server.

pub mod version;   // ProtocolVersion, handshake constants, compat rules
pub mod header;    // SessionHeader, CommandHeader, frame tags
pub mod command;   // Command enum (the API-agnostic envelope) + per-API submodules
pub mod codec;     // Encode / Decode traits + wire (de)serialization
pub mod error;     // ProtocolError (thiserror)
pub mod limits;    // hard caps consulted by the server's validator

pub use error::ProtocolError;
pub use version::ProtocolVersion;
```

The central type is an envelope `enum` whose variants are *namespaced by API family*, so breadth grows by adding a variant and a submodule, never by editing the others:

```rust
// crates/graftx-protocol/src/command/mod.rs
#[non_exhaustive]
pub enum Command {
    Vulkan(vulkan::VkCommand),
    OpenGl(opengl::GlCommand),
    Cuda(cuda::CudaCommand),
    OpenCl(opencl::ClCommand),
    // M4+: Hip, LevelZero, VideoCodec, ...
    Control(control::ControlCommand), // handshake, flush, teardown — not an API call
}
```

`#[non_exhaustive]` on `Command` (and on each per-API command enum) is deliberate: it forces every `match` in the server's dispatcher to carry a wildcard arm, so adding an API family in a later crate version cannot turn into a non-compiling exhaustiveness break in downstream match sites or in tests. The cost — you cannot construct it by struct-literal exhaustiveness checks — is irrelevant for a wire type.

Encode/decode is expressed as traits so the chosen serializer (see the Protocol chapter (Ch. 06) for the wire format itself) is swappable and so client/server share one implementation:

```rust
// crates/graftx-protocol/src/codec.rs
pub trait Encode {
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), ProtocolError>;
}
pub trait Decode: Sized {
    /// Decodes from an UNTRUSTED buffer. Every impl MUST bound-check and
    /// reject malformed input via ProtocolError — never panic, never unwrap.
    fn decode(buf: &mut &[u8]) -> Result<Self, ProtocolError>;
}
```

Internal module map:

```text
graftx-protocol/src/
├── lib.rs
├── version.rs        ProtocolVersion { major, minor }, NEGOTIATED_MIN/MAX
├── header.rs         SessionHeader, CommandHeader { api, opcode, len, flags }
├── codec.rs          Encode/Decode + LE primitive helpers (read_u32, etc.)
├── error.rs          ProtocolError { Truncated, BadTag, LimitExceeded, ... }
├── limits.rs         MAX_CMD_BYTES, MAX_BATCH, MAX_HANDLE_TABLE, ...
└── command/
    ├── mod.rs        Command enum + dispatch tag
    ├── control.rs    handshake / flush / teardown
    ├── vulkan.rs     VkCommand (M1)
    ├── opengl.rs     GlCommand (M2)
    ├── cuda.rs       CudaCommand (M3)
    └── opencl.rs     ClCommand (M3)
```

`#![forbid(unsafe_code)]` at the crate root is the single most valuable lint here: because this crate is the one piece compiled identically on both targets and is the one that parses untrusted bytes, keeping it provably `unsafe`-free shrinks the audit surface dramatically. The crate has **zero platform `cfg`** — it must compile byte-identically for `x86_64-unknown-linux-gnu` and `x86_64-pc-windows-gnu`, which is exactly what makes it a valid contract.

## 5.4 `graftx-transport` — the channel

Public surface centers on a `Transport` trait so client and server are written against an abstraction, and so vsock vs ivshmem (or a future in-process loopback for tests) is a runtime/feature choice:

```rust
// crates/graftx-transport/src/lib.rs
pub mod transport;   // Transport trait + Endpoint role
pub mod framing;     // length-prefix framing over the control plane
pub mod vsock;       // virtio-vsock control-plane channel  (cfg(feature="vsock"))
pub mod ivshmem;     // shared-memory bulk plane            (cfg(feature="ivshmem"))
pub mod loopback;    // in-process channel for tests        (cfg(feature="loopback"))
pub mod error;       // TransportError (thiserror)

pub use transport::{Transport, Endpoint};
pub use error::TransportError;
```

```rust
// crates/graftx-transport/src/transport.rs
use graftx_protocol::header::CommandHeader;

pub trait Transport: Send {
    /// Control-plane: send a framed, length-prefixed message.
    fn send_frame(&mut self, hdr: &CommandHeader, body: &[u8]) -> Result<(), TransportError>;
    fn recv_frame(&mut self, buf: &mut Vec<u8>) -> Result<CommandHeader, TransportError>;

    /// Bulk-plane: claim a shared-memory region for a large payload (textures,
    /// buffers). Returns a handle the control-plane message references.
    fn bulk_alloc(&mut self, len: usize) -> Result<BulkRegion, TransportError>;
}
```

The transport depends on `graftx-protocol` for `CommandHeader`/framing tags but contains no `Command` decoding — it moves opaque bodies and lets the endpoints decode. This keeps the layering clean: transport knows *how much* and *to whom*, protocol knows *what*. The `ivshmem` module is where the "copy validated data into server-private memory" rule from the security model is implemented on the receive side; the trait keeps that detail behind `bulk_alloc`/`BulkRegion`.

## 5.5 `graftx-client` — Linux shims (`cdylib` + `rlib`)

The `[lib] crate-type = ["cdylib", "rlib"]` is already set and is exactly right: the `cdylib` is the drop-in `.so` (`libvulkan.so.1`, `libGL.so.1`, `libcuda.so.1`, …) the guest application loads, and the `rlib` lets the workspace's own tests and tooling link the same code in-process.

```text
graftx-client/src/
├── lib.rs           crate root; declares modules, sets crate-level lints
├── exports/         the ONLY place exported C symbols live  (#[no_mangle])
│   ├── mod.rs
│   ├── vulkan.rs    #[no_mangle] extern "C" fn vkCreateInstance(...) -> ...
│   ├── opengl.rs    glGetString, eglSwapBuffers, ...
│   └── cuda.rs      cuInit, cuMemAlloc, ...
├── dispatch.rs      maps an exported call -> protocol::Command, sends, awaits reply
├── session.rs       lazy global session (handshake, transport handle, reconnect)
├── handles.rs       guest<->server handle remapping tables
└── ffi.rs           shared C-ABI glue (return-code mapping, struct layout asserts)
```

The discipline that makes this safe: **`#[no_mangle] extern "C"` functions exist only under `exports/`, and each one is a thin wrapper** that (a) marshals arguments into a `protocol::Command`, (b) calls into the safe `dispatch` layer, and (c) maps the reply back to the C return convention. All `unsafe` is in `exports/` and `ffi.rs`, each block carrying a `// SAFETY:` note explaining the C-side invariant (caller passes valid pointers, lengths match, etc.). `dispatch`, `session`, and `handles` are safe Rust with `#![deny(unsafe_code)]`-equivalent discipline — pointer dereferencing happens at the boundary and nowhere deeper. The crate root sets:

```rust
// crates/graftx-client/src/lib.rs
#![cfg(target_os = "linux")] // the cdylib is meaningless off Linux; fail loudly elsewhere
```

This `cfg` is what enforces the target split (5.7): on a Windows host, `cargo build -p graftx-client` produces an empty crate rather than a confusing FFI link error.

## 5.6 `graftx-server` — Windows replay engine

A `bin`, not a lib, because it is a long-running sandboxed process. Its modules separate the untrusted-input handling from the trusted native-driver calls:

```text
graftx-server/src/
├── main.rs          arg parse, sandbox bring-up, accept loop
├── session.rs       per-connection state machine, handshake
├── validate.rs      decode + VALIDATE the untrusted Command stream (security spine)
├── dispatch.rs      validated Command -> backend replay call
├── backend/         native-driver bindings (the trusted, unsafe edge)
│   ├── mod.rs       Backend trait
│   ├── vulkan.rs    real vkCreateInstance via loaded vulkan-1.dll
│   ├── opengl.rs    opengl32.dll / wgl
│   └── cuda.rs      nvcuda.dll
├── handles.rs       server-side object table; maps wire handles -> real handles
└── sandbox.rs       Windows job-object / token restriction
```

The flow `recv -> decode -> validate -> dispatch -> backend` is the security boundary: `validate.rs` is safe Rust that rejects anything violating `protocol::limits` *before* `dispatch.rs` is allowed to reach `backend/`. Only `backend/` contains `unsafe` (the FFI into native drivers). Crate root sets `#![cfg(target_os = "windows")]` for the same reason as the client's Linux guard.

## 5.7 Cross-compilation contract

The two endpoints never build for the same OS, but CI and contributors run on one host. The contract:

| Crate | Target(s) | Builds on a Linux dev host? | Builds on a Windows dev host? |
|-------|-----------|-----------------------------|-------------------------------|
| graftx-protocol | both (host-agnostic) | yes (native) | yes (native) |
| graftx-transport | both | yes (native) | yes (native) |
| graftx-client | `x86_64-unknown-linux-gnu` | yes (native) | only via `--target …linux-gnu` cross |
| graftx-server | `x86_64-pc-windows-gnu` | only via `--target …windows-gnu` cross | yes (native) |

Mechanics:

- The `#![cfg(target_os = …)]` crate guards mean a plain `cargo build --workspace` on Linux compiles protocol + transport + client natively and reduces server to an empty crate (and vice versa) — so the workspace is always green on either host without special flags.
- Full cross builds use the GNU triples to avoid an MSVC toolchain: `cargo build -p graftx-server --target x86_64-pc-windows-gnu` (needs the `mingw-w64` linker) and `cargo build -p graftx-client --target x86_64-unknown-linux-gnu`. CI will run both, plus `cargo build -p graftx-protocol --target` for both triples to prove byte-format portability.
- `.cargo/config.toml` (planned) pins the cross linkers (`x86_64-w64-mingw32-gcc`) so the command line stays uniform.

**Tradeoff:** we choose `windows-gnu` over `windows-msvc` for the server's default cross target to keep CI on Linux runners without a Windows image. Production server builds may still be produced natively with MSVC; because all wire-format logic lives in the `unsafe`-free `protocol` crate and is target-agnostic, the ABI of the *driver bindings* differs but the *wire contract* does not. This is the payoff of the single shared crate.

## 5.8 Feature flags

Features are kept minimal and additive (resolver 2 unifies them per-target, not across the whole graph, which matters because client and server are never in the same target graph):

```text
graftx-protocol features:
  default        = []                 # pure types always available
  serde          = ["dep:serde"]      # opt-in: expose types for tooling/tests
  (per-API gates are NOT features here — the Command enum always carries all
   variants so the wire format is stable; gating happens in client/server.)

graftx-transport features:
  default   = ["vsock", "ivshmem"]
  vsock     = []        # virtio-vsock control plane
  ivshmem   = []        # shared-memory bulk plane
  loopback  = []        # in-process Transport impl for unit/integration tests

graftx-client / graftx-server features:
  default  = ["vulkan"]                       # M1 spine on by default
  vulkan   = []                               # M1
  opengl   = []                               # M2
  cuda     = ["opencl"?]  opencl  hip  ...     # later milestones, additive
```

Rationale: the *protocol* never gates API variants behind features — gating the wire enum would make two builds speak different wires, which is the exact failure we are designing against. Per-API features live in client/server only, where they switch which shims/backends are compiled in, not what the wire can express. This lets a slim build ship only Vulkan while remaining wire-compatible with a full server.

## 5.9 Build profiles

```toml
# root Cargo.toml (planned additions)
[profile.release]
opt-level = 3
lto = "thin"          # cross-crate inlining: shim -> dispatch -> codec hot path
codegen-units = 1     # better optimization for the latency-critical path
# NOTE: panic is NOT set here. The client cdylib and the server bin need
# DIFFERENT panic strategies (see below), and an illegal per-package override
# (`[profile.release.package.*] panic = …`) is rejected by Cargo. The two
# panic modes are selected by SEPARATE cargo invocations, not by this profile.

[profile.bench]
inherits = "release"
debug = true          # keep symbols for perf/VTune on the hot path
```

The panic strategy is split per artifact and is set by separate cargo invocations, not a workspace-wide profile key (a `[profile.*.package.*]` panic override is illegal in Cargo). The **client `cdylib` is built `panic = "abort"`** (`cargo rustc -p graftx-client … -C panic=abort`): unwinding across the `extern "C"` boundary in the client shims is undefined behavior, and the `no unwrap in lib paths` rule plus `Result`-everywhere makes panics genuinely exceptional, so aborting is the honest response. The **server `bin` is built with the default unwind**, so it can catch a Rust panic in glue and tear down only the offending session rather than killing every connection; it uses `parking_lot` mutexes so a panic mid-lock does not poison shared state (native driver *hardware* faults are a separate concern handled at the FFI seam — see the Server core chapter (Ch. 10) and the Error/FFI chapter (Ch. 24)). The default `dev` profile keeps unwinding for ergonomic test backtraces, but integration tests that exercise the `cdylib` pin `panic = "abort"` too. `thin` LTO is chosen over `fat` to keep the cross-crate inlining benefit on the shim→codec hot path without the wall-clock cost of fat LTO across the full workspace.

## 5.10 Boundary summary and forward references

The module boundaries enforce three invariants the rest of the plan relies on: the wire format has exactly one definition (`graftx-protocol`, `unsafe`-free, target-agnostic — detailed in the Protocol chapter (Ch. 06)); the transport moves opaque bodies behind a `Transport` trait (detailed in the Transport chapter (Ch. 08)); and all `unsafe` FFI is confined to `client/src/exports` + `ffi.rs` and `server/src/backend` (the validation spine in `server/src/validate.rs` is covered by the Server core chapter (Ch. 10) and the Security chapter (Ch. 23)). New API families are added as a `Command` variant plus a per-API submodule in each of the three relevant crates, never as new workspace members — keeping breadth growth cheap, which is the project's top priority.
