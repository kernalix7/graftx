# Contributing to GraftX

**English** | [한국어](docs/CONTRIBUTING.ko.md)

Thanks for your interest in contributing to GraftX!

GraftX is a GPU API-remoting layer: a Linux guest's GPU API calls (OpenGL,
OpenGL ES, EGL, Vulkan, CUDA, OpenCL, ROCm/HIP, Level Zero, video codecs, and
more) are intercepted by client shims, serialized, sent over a fast
guest-to-guest transport (virtio-vsock / ivshmem), and replayed by a server on
the Windows guest that owns the physical GPU via PCI passthrough. Results and
surfaces return to the Linux side.

The project is in **early development** (version `0.0.0`); nothing is stable
yet. Project priorities, in order, are:

1. Breadth of API coverage
2. Performance (minimal remoting overhead)
3. Stability
4. Safety

Please keep these priorities in mind when proposing changes.

## Prerequisites

- **Rust (stable)** installed via [rustup](https://rustup.rs/). The project
  targets edition 2021 with a minimum `rust-version` of `1.75`.
- The `rustfmt` and `clippy` components:

  ```sh
  rustup component add rustfmt clippy
  ```

### Workspace layout

GraftX is a Cargo workspace with four crates:

| Crate              | Kind            | Responsibility                                                            |
| ------------------ | --------------- | ------------------------------------------------------------------------- |
| `graftx-protocol`  | lib             | Wire format, command encode/decode, `PROTOCOL_VERSION`, `ProtocolError`.  |
| `graftx-transport` | lib             | `Transport` trait (send/recv) over vsock / ivshmem.                       |
| `graftx-client`    | cdylib + rlib   | Linux API shims (`libvulkan.so`, `libGL.so`, `libcuda.so`, ...).          |
| `graftx-server`    | bin             | Windows-side replay against native GPU drivers.                           |

## Building and testing

All commands run from the workspace root:

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Your branch must build cleanly and pass all four of these before you open a
pull request.

## Code style

- **Formatting**: rustfmt defaults. Run `cargo fmt` before committing; CI
  enforces `cargo fmt --check`.
- **Linting**: clippy must pass with warnings denied
  (`cargo clippy --all-targets -- -D warnings`). Fix the lint rather than
  silencing it unless there is a clear, documented reason for an
  `#[allow(...)]`.
- **Unsafe / FFI isolation**: all `unsafe` and FFI code lives in dedicated
  modules. Every `unsafe` block must carry a `// SAFETY:` comment explaining
  why the invariants hold. Keep unsafe surface area minimal and contained.
- **No `unwrap()` / `expect()` in library paths**: library code
  (`graftx-protocol`, `graftx-transport`, and the `graftx-client` /
  `graftx-server` library/runtime paths) must propagate errors with `Result`
  instead of panicking. `unwrap` / `expect` are acceptable only in tests,
  examples, and one-off tooling.
- **Client shims** are `cdylib` crates that export the intercepted API's C
  symbols. Match the upstream C ABI exactly.

### Security-sensitive code

The server replays an **untrusted** command stream against native GPU drivers,
which is a real attack surface (driver bugs, out-of-bounds buffers, resource
exhaustion). Treat the client as untrusted: validate and bounds-check commands,
and never assume the wire data is well-formed. See
[`SECURITY.md`](SECURITY.md) for the full threat model before touching
protocol decoding, transport, or replay code.

## Commit conventions

- **Conventional Commits**: prefix each commit subject with one of
  `feat:`, `fix:`, `refactor:`, `docs:`, `chore:`, or `test:`.

  ```text
  feat(protocol): add vkCreateBuffer command encoding
  fix(transport): handle short reads on vsock recv
  ```

- **Branch naming**: use `feature/<name>`, `fix/<name>`, or `chore/<name>`.
- **Merging**: pull requests are **squash-merged** to `main`. Keep the PR
  title in Conventional Commit form, since it becomes the squash commit
  subject.

### No AI attribution policy

This is a **hard repository policy**. Commits and pull requests must **not**
contain any AI or assistant attribution. Specifically, do **not** include:

- `Co-Authored-By: Claude` (or any AI/assistant co-author trailer)
- `Generated with [Claude Code]` (or any "made with AI" footer / link)
- Any robot emoji or similar "made with AI" marker

This applies to commit messages, PR titles, PR descriptions, issue titles and
bodies, code comments, GitHub comments, CHANGELOG entries, and release notes.
Contributors are the sole human authors of their contributions. PRs containing
AI attribution will be asked to remove it before merge.

## Pull request process

1. Fork the repo (or create a branch if you have write access) using the
   branch-naming convention above.
2. Make your change in focused, logically scoped commits.
3. Run the full local check suite (build, test, clippy, fmt) and make it pass.
4. Open a PR against `main` with a Conventional Commit-style title and a clear
   description of the what and the why.
5. Address review feedback; the PR will be squash-merged once approved.

### PR checklist

Before requesting review, confirm:

- [ ] `cargo build --workspace` succeeds.
- [ ] `cargo test --workspace` passes.
- [ ] `cargo clippy --all-targets -- -D warnings` is clean.
- [ ] `cargo fmt --check` reports no diffs.
- [ ] No `unwrap()` / `expect()` added to library paths.
- [ ] All new `unsafe` / FFI is isolated and carries `// SAFETY:` comments.
- [ ] Security-sensitive changes account for the untrusted command stream
      (see [`SECURITY.md`](SECURITY.md)).
- [ ] The PR and its commits contain **no AI attribution** (see policy above).
- [ ] Commit subjects and PR title follow Conventional Commits.

## Code of conduct

By participating in this project you agree to abide by the
[`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).

## Security

Please do not file public issues for security vulnerabilities. Follow the
reporting process in [`SECURITY.md`](SECURITY.md). Note that GraftX is **not
yet hardened** and must not be exposed to untrusted clients during early
development.
