# 26. Testing & Quality Assurance

How GraftX will prove that an intercepted GPU call replayed across two guests produces the same result a native call would — from per-frame unit tests up through full Khronos conformance suites driven over the remoting path, plus the fuzzing, golden-trace, and CI machinery that keeps it true.

GraftX sits between an application and a real GPU driver. A bug anywhere — a mis-encoded enum, a dropped fence, an off-by-one in a bulk descriptor — can silently corrupt a frame, hang a swapchain, or crash a driver against attacker-influenced data. Because the project's first priority is *breadth* of API coverage, the test strategy must scale across dozens of APIs without writing bespoke harnesses per call. This chapter designs a layered approach where the cheapest layers (unit, golden-trace, fuzz) run on every commit on a single host, and the expensive layers (CTS over real passthrough) gate releases. It does not re-specify the wire format (the Serialization chapter (Ch. 07)), the `Transport` trait (the Transport chapter (Ch. 08)), the validator (the Server core chapter (Ch. 10)), or sandboxing (the Security chapter (Ch. 23)); it consumes them.

## 26.1 The test pyramid and where the seams are

```
            ┌───────────────────────────────────┐
   slow,    │  E2E conformance: Vulkan/GL/CL CTS │  release gate, real GPU
   costly   │  over vsock+ivshmem, 2-guest VM    │  (nightly / pre-tag)
            ├───────────────────────────────────┤
            │  Golden command-trace replay       │  per-PR, host-only
            │  Decoder/validator fuzzing         │  per-PR + continuous
            ├───────────────────────────────────┤
   fast,    │  Integration: client↔server over   │  per-PR, host-only
   cheap    │  LoopbackTransport (in-process)    │
            ├───────────────────────────────────┤
            │  Unit: encode/decode roundtrip,    │  per-commit
            │  handle table, ring, validator     │
            └───────────────────────────────────┘
```

The single most important design decision is the **loopback seam**. The Architecture chapter (Ch. 04) and the Workspace chapter (Ch. 05) already reserve `graftx-transport::loopback` (feature `loopback`) as an in-process `Transport` implementation. Everything above the bottom tier runs through that seam, so the vast majority of GraftX logic — encoding, decoding, validation, handle mapping, batching, async fence resolution, error propagation — is tested *without a Windows guest, without a GPU, and without root*, in milliseconds, on the same `x86_64-unknown-linux-gnu` host that runs `cargo test`. Only the top tier needs real hardware.

## 26.2 Loopback transport for host-only testing

The loopback transport implements the same `Transport` trait the vsock/ivshmem backends do, so client and server code is byte-identical between test and production. The proposed shape connects two endpoints with crossbeam channels plus a `Vec<u8>`-backed "shared region" standing in for ivshmem:

```rust
// graftx-transport/src/loopback.rs   (cfg(feature = "loopback"))
pub struct LoopbackPair {
    pub client: LoopbackTransport,
    pub server: LoopbackTransport,
}

pub struct LoopbackTransport {
    tx: Sender<Vec<u8>>,            // control frames out
    rx: Receiver<Vec<u8>>,         // control frames in
    bulk: Arc<BulkArena>,          // shared "ivshmem" — Arc, NOT a separate copy
    fault: Arc<FaultPolicy>,       // inject reorder/drop/corrupt/latency
}

impl Transport for LoopbackTransport {
    fn send(&mut self, frame: &[u8]) -> Result<(), TransportError> {
        self.fault.maybe_perturb(frame, &self.tx)   // honour injected faults
    }
    fn recv(&mut self) -> Result<Vec<u8>, TransportError> {
        self.rx.recv().map_err(|_| TransportError::PeerClosed)
    }
    fn bulk_alloc(&mut self, len: usize) -> Result<BulkWriter<'_>, TransportError> {
        self.bulk.alloc(len)       // hands back a slice into the shared arena
    }
}
```

Because the bulk arena is a shared `Arc<BulkArena>`, loopback faithfully reproduces the **TOCTOU hazard** of real ivshmem: the "client" can mutate a `BulkRef`'s bytes after the "server" has validated them. A test can therefore assert that the server's copy-and-validate path (the Server core chapter (Ch. 10) and the Memory chapter (Ch. 12)) actually defends against a racing writer — a property impossible to test on a transport that copied on `send`. `FaultPolicy` lets a test deterministically reorder, drop, duplicate, delay, or bit-flip frames to exercise sequence-number gaps, epoch rejection, and timeout/backpressure logic without flaky timing.

A representative integration test:

```rust
#[test]
fn create_buffer_roundtrips_handle() {
    let LoopbackPair { client, server } = LoopbackPair::new(FaultPolicy::none());
    let srv = std::thread::spawn(move || graftx_server::serve_one(server));
    let mut gl = graftx_client::GlContext::on(client);
    let buf = gl.gen_buffers(1)[0];          // local handle, async create
    gl.bind_buffer(GL_ARRAY_BUFFER, buf);
    gl.buffer_data(GL_ARRAY_BUFFER, &VERTS, GL_STATIC_DRAW);
    let echoed = gl.get_buffer_parameter(GL_ARRAY_BUFFER, GL_BUFFER_SIZE); // EXPECTS_REPLY
    assert_eq!(echoed, VERTS.len() as i32); // forces a real round-trip through the seam
    drop(gl); srv.join().unwrap().unwrap();
}
```

For tiers above loopback that still need no GPU, the server is built with a **mock backend** (`backend/mock.rs`, feature `mock-backend`) that records the decoded, validated call sequence instead of calling a driver. This lets golden-trace and fuzz tests assert *exactly which validated commands reached the backend boundary* without any native driver present — the assertion target is the validated command, not pixels.

## 26.3 Unit tests

Each crate carries `#[cfg(test)]` modules. The non-negotiable unit invariants:

| Area | Property under test |
|---|---|
| protocol | `decode(encode(x)) == x` for every command type (proptest-driven) |
| protocol | encoded length matches the declared `wire_len()`; no trailing bytes |
| protocol | every reserved/unknown opcode and oversize length is rejected, not panicked |
| transport | ring `push`/`pop` FIFO order; `epoch` increments and rejects stale slots |
| handles | client handle alloc is collision-free; server map rejects unknown/forged handles |
| validator | each `limits::*` bound rejects the boundary+1 case and accepts the boundary |

The roundtrip property is the workhorse and is generated, not hand-written, via `proptest`:

```rust
proptest! {
    #[test]
    fn cmd_roundtrip(cmd in any::<Command>()) {   // Arbitrary derived per enum variant
        let mut buf = Vec::new();
        cmd.encode(&mut buf)?;
        let (decoded, rest) = Command::decode(&buf)?;
        prop_assert_eq!(cmd, decoded);
        prop_assert!(rest.is_empty());            // no slack, no overread
    }
}
```

Repo conventions (no `unwrap` in lib paths, `thiserror` errors, `clippy -D warnings`) are themselves a tested property: a `tests/no_unwrap.rs` lint test greps the lib source for `unwrap(`/`expect(`/`panic!` outside `#[cfg(test)]` and fails CI on a hit, since a panic in a shim runs inside the host application's address space and is never acceptable.

## 26.4 Golden command-trace replay

A *golden trace* is a recorded, validated byte stream of the control frames (and a manifest of bulk-region contents) that a known-good run of GraftX produced for a fixed input. Replay testing catches **wire-format regressions** the proptest layer cannot: a roundtrip test proves encode/decode are mutually consistent, but if *both* drift the same way the test still passes while old recordings break. Golden traces pin the format to a frozen artifact.

```text
tests/golden/
  gl_triangle.gtrace        # framed control stream, length-prefixed records
  gl_triangle.manifest.json # { version, api, bulk: [{id, sha256, len}], frames }
  vk_clear.gtrace
  cl_saxpy.gtrace
```

Two replay directions run in CI:

1. **Decode replay (host-only):** feed `*.gtrace` straight into the server decoder+validator with the mock backend; assert the resulting validated command list matches a checked-in `*.expected.ron`. Detects accidental wire changes and validator regressions. Runs per-PR, no GPU.
2. **Re-encode replay:** drive the client shims with the recorded high-level call list and assert the freshly encoded bytes equal the golden `.gtrace` (modulo documented non-determinism — sequence numbers, locally-allocated handles, timestamps — which a normalizer strips first).

```rust
fn normalize(frames: &mut [Frame]) {
    for f in frames {
        f.hdr.seq = 0;          // monotonic, run-dependent
        f.hdr.epoch = 0;        // ring slot generation
        scrub_local_handles(f); // client-assigned handle ids vary per run
    }
}
```

Intentional wire changes are expected to bump `protocol::WIRE_VERSION` and regenerate goldens via `cargo run -p xtask -- bless-traces`, which re-records from the live shims and rewrites the artifacts in one reviewed commit. A `.gtrace` whose header version is older than `WIRE_VERSION` is replayed through the versioned decoder to also exercise backward-compat decoding (the Versioning chapter (Ch. 29)'s versioning contract).

## 26.5 Decoder and validator fuzzing

The server decodes and validates an **untrusted** stream (per the security model: a compromised guest can send arbitrary bytes). Fuzzing is therefore a security control, not just a quality nicety, and targets the exact safe-Rust boundary `recv -> decode -> validate` *before* any `unsafe`/FFI runs.

```rust
// fuzz/fuzz_targets/decode_validate.rs
#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    // MUST NOT panic, abort, OOM, or infinite-loop on ANY input.
    if let Ok(cmd) = graftx_protocol::Command::decode(data) {
        // Decoded => must survive validation as a clean accept/reject, never UB.
        let _ = graftx_server::validate::check(&cmd, &Limits::strict());
    }
});
```

The invariant the fuzzer enforces: **no input may cause a panic, an unbounded allocation, or pathological CPU.** Self-describing length fields are the classic trap — a frame claiming a 4 GiB bulk payload must be rejected by `limits` long before anything tries to allocate it, so a dedicated `decode_lengths` target seeds inputs with adversarial size fields. `cargo-fuzz` (libFuzzer) drives coverage-guided exploration; the corpus is seeded from every golden trace so the fuzzer starts from valid structure and mutates outward. A complementary **structure-aware** target uses `arbitrary` to build *syntactically valid but semantically hostile* `Command`s (forged handles, mismatched lengths, illegal enum combinations), exercising the validator's semantic checks rather than the byte parser.

| Target | Entry point | Catches |
|---|---|---|
| `decode_validate` | raw bytes → decode → validate | parser panics, validator UB, OOM |
| `decode_lengths` | adversarial size/count fields | allocation bombs, integer overflow |
| `arbitrary_command` | `Arbitrary` → validate | semantic-rule gaps, forged handles |
| `bulk_descriptor` | descriptor + arena bounds | OOB slice, epoch confusion |

CI runs each target for a bounded budget per PR (e.g. 60s) for fast feedback; a nightly job runs them for hours and any new crash is minimized (`cargo fuzz tmin`), committed as a regression seed under `fuzz/corpus/`, and filed. Crashes are reproducible because the input is the artifact. `RUSTFLAGS=-Zsanitizer=address` builds (nightly toolchain, fuzz-only) catch memory errors that escape Rust's safety net through the eventual FFI.

## 26.6 End-to-end conformance: Vulkan / OpenGL / OpenCL CTS

The ultimate correctness oracle is the vendors' own conformance suites, run *through* GraftX. The principle: GraftX is correct for an API when an unmodified CTS binary, loading GraftX's client shim instead of the native ICD/loader, passes the same tests it passes natively against the GPU.

```text
[ Linux guest ]                                   [ Windows guest ]
 Vulkan-CTS (deqp-vk)                               graftx-server.exe
   │ loads libvulkan via                              │ replays into the
   │ VK_LOADER → graftx ICD JSON                      │ real vendor driver
   ▼                                                  ▼
 libgraftx_client.so ── frames over vsock ──▶ decode/validate ──▶ vkCmd...
   ▲                  ◀── results / fences ───        │
   └────────── ivshmem bulk (images, buffers) ────────┘
```

Wiring per suite:

- **Vulkan CTS (`deqp-vk` / `VK-GL-CTS`):** install a GraftX ICD manifest so the Khronos loader dispatches into `libgraftx_client.so`; point `VK_ICD_FILENAMES` at it. The server hosts the real vendor ICD. Run by `--deqp-caselist-file`, sharded.
- **OpenGL / OpenGL ES CTS (same `VK-GL-CTS` tree, `glcts`/`deqp-gles*`):** GraftX provides the EGL/GLX entry points (the OpenGL/GLES/EGL/GLX chapter (Ch. 15)) so the dEQP EGL harness creates contexts through the remoting path.
- **OpenCL CTS (`OpenCL-CTS`):** GraftX ships an ICD entry in `/etc/OpenCL/vendors/`; the suite selects the GraftX platform.

Conformance is tracked as a **waiver-list** model, not pass/fail-all, because breadth-first coverage means many tests will legitimately fail early (unimplemented entry points, optional features). The CI artifact is a diff against a checked-in baseline:

```text
testing/cts/baselines/vulkan-1.3.json
  { "expected_pass": 312004, "known_fail": [ "dEQP-VK.sparse_resources.*", ... ],
    "waivers": { "dEQP-VK.api.external.*": "no shared-memory handle remoting yet" } }
```

A run **fails CI only if a test that previously passed now fails (a regression) or a waiver's reason no longer holds**. Newly-passing tests are reported as progress and auto-promoted into `expected_pass` on merge. This keeps a 300k-case suite usable as a daily signal while breadth fills in. `deqp-vk` results (`TestResults.qpa`) are parsed by an `xtask` that emits the JSON diff and a per-feature coverage rollup, which feeds the API-coverage matrix tracked elsewhere in the plan.

A crucial subtlety: CTS validates *rendered output*, so the ivshmem return path for images and the fence/sync semantics (the Sync chapter (Ch. 13)) get exercised hard. Image-comparison tests are the real proof that surfaces survive the round trip pixel-accurate; sync tests are the proof that `EXPECTS_REPLY` fences and async batching (the Architecture chapter (Ch. 04) and the Server core chapter (Ch. 10)) preserve ordering. A "loopback CTS smoke" subset also runs host-only against the mock+software-rasterizer backend (e.g. lavapipe/llvmpipe on the server side) so a chunk of conformance signal exists without passthrough hardware in the common CI path.

## 26.7 CI matrix

GitHub Actions, three tiers keyed to cost:

| Stage | Runner | Triggers | Contents |
|---|---|---|---|
| **fast** | `ubuntu-latest` | every push/PR | `fmt --check`, `clippy -D warnings`, unit, loopback integration, golden decode-replay, 60s fuzz each |
| **cross** | linux + a Windows runner | PR + merge | build `graftx-server` for `x86_64-pc-windows-msvc`; server unit tests; mock-backend E2E |
| **hw** | self-hosted: Linux guest + Windows guest + passthrough GPU | nightly + pre-tag | full vsock+ivshmem bring-up; Vulkan/GL/CL CTS shards; perf smoke |

```yaml
jobs:
  fast:
    strategy: { matrix: { toolchain: [1.75.0, stable] } }   # MSRV row tracks the Workspace chapter (Ch. 05)
    steps:
      - run: cargo fmt --all -- --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo test --workspace --features loopback,mock-backend
      - run: cargo test -p graftx-protocol golden::decode_replay
      - run: ./xtask fuzz-ci --max-total-time 60
```

The MSRV row and the stable row both run in `fast`, enforcing the MSRV that the Workspace chapter (Ch. 05) declares as the single source of truth (`rust-version = "1.75"`) — this chapter pins the toolchain to that number rather than defining its own. The Windows server target builds in `cross` even though its full E2E needs hardware, so a Windows-only compile break is caught per-PR rather than at release. The `hw` tier is the only place needing a self-hosted runner with the two-guest topology; it gates tags via branch protection. Flaky-hardware results are quarantined (re-run once, label `flaky`) rather than blocking merges, since the deterministic loopback/fuzz tiers carry the per-PR correctness burden and the `hw` tier carries the *integration* burden.

## 26.8 Tradeoffs

- **Loopback fidelity vs. realism.** Loopback can never reproduce real PCIe/driver latency, vendor-specific driver bugs, or genuine cross-guest memory protection — it intentionally reproduces only the *logical* hazards (TOCTOU, reorder, epoch). Realism is delegated to the `hw` tier; loopback buys speed and determinism for everything else.
- **Golden traces vs. churn.** They pin the wire format tightly, which is the point, but make intentional protocol changes a two-step (`bless-traces`) ritual. Accepted: breaking the wire silently is worse than a regenerate step.
- **CTS-as-truth vs. cost.** Conformance suites are the strongest correctness oracle available and avoid maintaining a parallel home-grown matrix, but they are slow, hardware-bound, and noisy during breadth-first development — hence the baseline-diff model and the nightly cadence rather than per-PR.
- **Fuzz budget.** 60s/PR is a deliberate floor for fast feedback; depth comes from the nightly long-run plus a persistent, ever-growing corpus, so coverage compounds over time rather than restarting each run.
