# 02. Goals, Non-Goals & Success Metrics

This chapter fixes what "done" means for GraftX: the priority order that breaks every design tie, the numbers each subsystem is measured against, and the things we deliberately refuse to build.

## 2.1 The priority order is the contract

Every other chapter inherits one rule: **breadth of API coverage > performance > stability > safety**. This is not a slogan; it is the tie-breaker that resolves design conflicts mechanically, so that two contributors reasoning independently reach the same decision. When a choice trades one axis against another, the higher-priority axis wins by default, and the lower-priority axis may only override it with an explicit, written justification recorded in the design doc or PR.

The ordering looks surprising — most graphics-remoting projects put performance or safety first. GraftX inverts this on purpose, because its reason to exist is *reach*: a Linux guest that can run the long tail of GPU software (OpenGL, GLES, EGL, GLX, Vulkan, CUDA, OpenCL, ROCm/HIP, Level Zero, video codecs, WebGPU, OptiX, AMF, SYCL) against a GPU it does not physically own. A fast, rock-solid remoting layer that only forwards Vulkan is a worse product, for our target user, than a slightly slower one that also forwards CUDA and NVENC. The market is already full of the former.

```text
breadth      ── why the project exists; a missing API = a dead workload
   >
performance  ── the only reason to remote locally instead of running native; a 50x
                slowdown makes breadth academic
   >
stability    ── a forwarded API that crashes every minute is not "covered"
   >
safety       ── the server replays an UNTRUSTED stream; this floor is non-negotiable
                in absolute terms but yields the *schedule* to the axes above
```

### 2.1.1 Why breadth outranks performance

Coverage is binary per call. If `vkCreateGraphicsPipelines` or `cuLaunchKernel` is unimplemented, the workload does not run *slower* — it does not run *at all*, and the user falls back to a native machine or another product. Performance, by contrast, is continuous: an over-the-wire round-trip that costs 40 us instead of 20 us still completes the workload. We will therefore accept a *correct but unoptimized* forwarder for a new API (synchronous, one round-trip per call, no batching) and merge it before its fast path exists, then optimize behind benchmarks. Concretely: a PR that adds 30 new Vulkan entry points with naive per-call round-trips is preferred over a PR that shaves 5 us off an entry point we already forward.

### 2.1.2 Why performance still outranks stability and safety on the schedule

Local API-remoting only makes sense if the overhead is small relative to running native. If forwarding a frame costs more than rendering it twice, the user runs the GPU workload on the Windows guest directly. So performance is *load-bearing for the value proposition*, which is why it sits second. But note the wording: performance outranks stability and safety **for scheduling and for resolving ties**, not for correctness. We never ship a fast path that returns wrong pixels, and we never disable a safety floor (see 2.1.3) to hit a latency number.

### 2.1.3 Safety is last in priority but has a hard floor

Safety being fourth does *not* mean "optional". The server replays a stream produced by an untrusted guest against native GPU drivers — a class of code notorious for shallow input validation. The priority order means: when safety hardening would *delay* breadth, we may ship the broader feature first **provided the non-negotiable floor below still holds**. The floor is invariant across all milestones:

| Floor invariant | Always enforced from M0 |
|---|---|
| No decoded length/offset/count is trusted; all are bounds-checked before use | yes |
| ivshmem bulk payloads are copied into server-private memory before validation/replay | yes |
| The server process runs sandboxed (reduced token/job-object on Windows) | yes |
| All `unsafe`/FFI is isolated and annotated with `// SAFETY:` | yes |
| No `panic!`/`unwrap`/`expect` reachable from a remote command in library paths | yes |

What is *deferred* (and explicitly allowed to be) is depth: per-session authentication, cryptographic integrity, fine-grained per-API semantic validation, and formal fuzzing coverage. Those raise the bar above the floor and are scheduled as their own milestone slices (the Security chapter (Ch.23) carries the threat model and validation strategy in full).

## 2.2 Concrete success metrics

Metrics are split into **coverage**, **performance (overhead)**, and **stability SLOs**. Each is defined so it can be wired into CI or a benchmark harness rather than argued about.

### 2.2.1 API coverage metrics

Coverage is measured per API family as *forwarded entry points / total public entry points in the targeted version*, weighted by a hit-frequency table derived from tracing real applications (a `glXGetProcAddress` no-op is not worth the same as `vkQueueSubmit`). Two numbers are tracked per family: **raw coverage** (fraction of symbols implemented at all, even as a stub that returns `UNSUPPORTED`) and **effective coverage** (fraction of *call volume* in the reference trace suite that is fully forwarded and correct).

```text
coverage_raw(family)       = implemented_symbols / total_symbols
coverage_effective(family) = Σ weight(call)  over fully-forwarded calls
                             ─────────────────────────────────────────
                             Σ weight(call)   over all calls in trace suite
```

A small Rust harness in `graftx-protocol`'s test support will own the symbol census so the denominator is not hand-maintained:

```rust
/// One row of the coverage census, generated from the API registry
/// (e.g. vk.xml, the CUDA driver headers) and the client's exported symbols.
pub struct CoverageRow {
    pub family: ApiFamily,        // Vulkan, Gl, Egl, Cuda, OpenCl, Hip, ...
    pub symbol: &'static str,     // "vkQueueSubmit"
    pub state: SymbolState,       // Forwarded | Stubbed | Missing
    pub call_weight: u32,         // hits in the reference trace suite
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SymbolState { Forwarded, Stubbed, Missing }

pub fn coverage_effective(rows: &[CoverageRow], f: ApiFamily) -> f64 {
    let (mut hit, mut total) = (0u64, 0u64);
    for r in rows.iter().filter(|r| r.family == f) {
        total += r.call_weight as u64;
        if r.state == SymbolState::Forwarded {
            hit += r.call_weight as u64;
        }
    }
    if total == 0 { 0.0 } else { hit as f64 / total as f64 }
}
```

Per-milestone coverage targets (effective coverage on the reference trace suite):

| Milestone | Family scope | Target effective coverage |
|---|---|---|
| M0 | no-op control call only | round-trip succeeds |
| M1 | Vulkan 1.2 core | >= 90% of trace-suite call volume |
| M2 | OpenGL 4.x / GLES 3.x + EGL/GLX | >= 85% GL, >= 95% EGL/GLX windowing |
| M3 | CUDA driver + runtime, OpenCL 3.0 | >= 80% CUDA, >= 75% OpenCL |
| M4 | HIP, Level Zero, video codecs (NVENC/NVDEC/VA) | >= 70% per family |
| M5 | WebGPU, OptiX, AMF, SYCL | >= 50% per family (reach tier) |

The descending targets encode the priority order: the spine (Vulkan) must be near-complete because everything downstream leans on it (the Vulkan chapter (Ch.14)), while reach-tier APIs are allowed to be partial because *some* coverage there still unlocks workloads that have zero alternatives.

### 2.2.2 Performance / overhead metrics

Overhead is defined as **remoting cost relative to native execution on the GPU-owning guest**, never as an absolute latency, because the GPU itself is identical in both cases — we are only measuring the tax of serialize + transport + deserialize + replay + return.

Three primitives are benchmarked:

```text
1. round-trip latency  : time for a synchronous call requiring a server reply
                         (e.g. vkGetPhysicalDeviceProperties), p50 / p99
2. fire-and-forget cost: marginal CPU cost to enqueue a no-reply command
                         (e.g. vkCmdDraw) into a batch
3. bulk bandwidth      : throughput of the ivshmem path for buffer/texture upload,
                         as a fraction of a memcpy on the same shared region
```

Targets:

| Metric | M1 (naive baseline) | Optimized target (perf track) |
|---|---|---|
| Synchronous round-trip p50 | <= 60 us | <= 20 us |
| Synchronous round-trip p99 | <= 250 us | <= 80 us |
| Fire-and-forget enqueue (per cmd) | <= 1.0 us | <= 0.2 us |
| Batched submit amortized per cmd | n/a | <= 0.1 us |
| Bulk upload vs local memcpy | >= 0.4x | >= 0.85x |
| End-to-end frame overhead, 1080p GL app | <= 35% | <= 12% |

The "naive baseline" column is the bar a *newly forwarded* API must clear to merge; the "optimized target" column is the bar the performance track (zero-copy buffers, command batching, async submission per the roadmap) must reach before that API is declared performance-complete. This two-column structure is the operational form of "breadth > performance": breadth ships against column one, performance is a follow-up tracked against column two.

A microbenchmark crate (`benches/` via Criterion) will lock these in, and a regression gate fails CI if round-trip p50 grows by more than 15% versus the recorded baseline for that commit's API set.

### 2.2.3 Stability SLOs

Stability is measured by a soak harness that replays the trace suite continuously. SLOs are expressed per session-hour:

| SLO | Target |
|---|---|
| Server crashes (any unhandled fault) per 1000 session-hours | 0 — a crash on untrusted input is a security bug, not a stability one |
| Client shim crashes attributable to GraftX per 1000 session-hours | < 1 |
| Forwarded-call correctness mismatches (vs native golden output) | 0 in CI; any escape is a release blocker |
| Reconnect success after transient transport loss | >= 99% within 3 retries |
| Memory growth over a 24h soak (steady-state workload) | < 1% RSS drift after warm-up |
| Graceful degradation on unsupported call | always returns a defined error, never UB |

The crash SLO is intentionally absolute (zero). Because the server consumes an untrusted stream, *any* server crash is reclassified as a safety-floor violation and triaged with security severity, regardless of the priority order — this is the one place where the lowest-priority axis (safety) produces a hard release blocker.

## 2.3 Non-goals

Stating non-goals is as load-bearing as stating goals: it stops scope creep from eroding the breadth budget. GraftX explicitly will **not**:

1. **Remote the display path / window compositing.** GraftX forwards GPU *API calls* and returns surfaces/results; it is not a remote desktop, a streaming encoder pipeline, nor a replacement for VNC/RDP/Looking Glass. Presentation handoff is out of scope beyond returning rendered surfaces to the Linux side.
2. **Translate between APIs.** A CUDA call is forwarded as CUDA, not transpiled to HIP or Vulkan compute. No Angle-style or Zink-style API translation. (Translation is a separate, much larger problem and would explode the validation surface.)
3. **Support host-to-guest or WAN remoting.** The transport is guest-to-guest on one host (virtio-vsock control plane + ivshmem bulk plane). We will not design for internet latency, NAT traversal, or untrusted-network confidentiality. The paired-guest channel is reachability scoping, not authentication.
4. **Run the server on Linux or macOS.** The server targets the Windows guest that owns the GPU via PCI passthrough. A Linux server is a hypothetical future, not a v0.x goal, and must not constrain current design.
5. **Guarantee bit-exact rendering across driver versions.** We forward calls faithfully; if the Windows driver renders differently than a Linux native driver would, that is the driver's prerogative, not a GraftX bug. Our correctness oracle is "same result as running the call natively on the *server's* GPU/driver", not "same as Mesa".
6. **Provide a stable ABI before v1.0.** The wire protocol (the Protocol chapter (Ch.06)) may break between minor versions during 0.x; client and server are expected to be deployed as a matched pair. Negotiated versioning exists to *detect* mismatch, not to promise forward compatibility yet.
7. **Sandbox or isolate the client shims.** The client runs inside the (trusted) Linux guest's own application; it is not a trust boundary. All isolation effort is spent on the server, which is where untrusted data lands.
8. **Implement GPU virtualization or scheduling fairness across multiple unrelated guests.** One server owns one GPU for its paired session(s); we are not building a multi-tenant GPU scheduler. Resource limits/quotas/backpressure exist to protect the server from a single misbehaving client, not to arbitrate between competing tenants.

## 2.4 Quality bars per milestone

Each milestone (M0–M5 in the roadmap) must clear a fixed gate before the next begins. The gate is the same shape every time; only the scope expands.

```text
MILESTONE GATE  (all must be green)
  ├─ coverage    : effective coverage >= the M-target in §2.2.1
  ├─ performance : new APIs clear the "naive baseline" column of §2.2.2
  ├─ stability   : soak harness meets §2.2.3 SLOs for the milestone's scope
  ├─ safety floor: every invariant in §2.1.3 holds (audited, not assumed)
  └─ hygiene     : rustfmt clean, clippy -D warnings clean, no unwrap in lib
                   paths, all unsafe annotated, conventional-commit history
```

Per-milestone specifics:

- **M0 (protocol + transport skeleton):** the bar is a single no-op call round-tripping Linux->Windows->Linux with the handshake and version negotiation working. Coverage is trivially "the one call"; the real gate is that the framing, error model (`thiserror`), and Transport trait are stable enough that M1 does not have to rewrite them.
- **M1 (Vulkan spine):** the highest bar in the project, because every later milestone reuses the codegen, batching, and object-table machinery proven here. Vulkan must hit >= 90% effective coverage *and* a real GPU workload (a Vulkan sample or vkcube-class app) must run for a 24h soak with zero server crashes.
- **M2 (GL/GLES/EGL/GLX):** adds the windowing surface; the added gate is that EGL/GLX context and surface lifecycle is correct enough that a real GL app reaches steady-state frames, and that GL's implicit-state model is faithfully serialized (the GL chapter (Ch.15)).
- **M3 (CUDA/OpenCL):** adds the compute/data-movement gate — bulk transfers must clear the >= 0.4x-of-memcpy baseline, since compute workloads are bandwidth-bound, not call-rate-bound.
- **M4 (HIP/Level Zero/codecs):** lower coverage target (70%) reflecting reach; the new gate is that video codec sessions hold the stability SLO under sustained bitrate, since codecs are long-lived stateful sessions.
- **M5 (reach tier):** 50% coverage is acceptable; the gate is "no regressions in M1–M4" — reach APIs must not destabilize the spine.

No milestone may "borrow" from a later one to look complete: a feature counts toward M*N* only if it clears the M*N* gate, not merely if it compiles. This keeps the published coverage numbers honest, which is the entire point of measuring them.
