# 25. Performance Engineering

How GraftX will keep remoting overhead small: a per-call latency budget, command batching and pipelining, zero-copy bulk paths, techniques for avoiding round-trips, the profiling methodology, the micro- and macro-benchmark suite, and the CI regression gates that defend all of it.

Performance is GraftX's *second* priority (after breadth, before stability and safety). That ordering matters here: this chapter never proposes a technique that closes off an API surface or that cannot be made correct. Every optimization below has an "off" switch so a backend can opt out while it stabilizes. The mechanisms this chapter measures and tunes are designed in the Serialization chapter (Ch. 07), the Transport chapter (Ch. 08), the Memory chapter (Ch. 12), and the Sync chapter (Ch. 13); here we treat them as knobs and ask *how fast, how do we know, and how do we keep it fast*.

## 25.1 The latency budget

The fundamental cost in API remoting is the **round-trip**: a guest call that must produce a value before the guest can continue (e.g. `cudaMalloc` returning a pointer, `glMapBuffer` returning an address, any `glGet*`, `vkAcquireNextImageKHR`). A round-trip pays the full path twice. A *fire-and-forget* call (`glDrawArrays`, `cuLaunchKernel`, most state setters) pays almost nothing on the hot path because it is batched (§25.2).

The budget below is the design target for a single synchronous round-trip on the vsock control plane, paired guests on one host, no bulk payload. These are goals, not measurements — v0.0.0 has no numbers yet.

```text
 stage                                         target    cumulative   notes
 ─────────────────────────────────────────────────────────────────────────
 client shim entry (TLS, dispatch)              0.10 us       0.10     §25.5 hot path
 serialize command into frame                   0.15 us       0.25     Ch. 07 fixed layout
 enqueue on vsock ring (no syscall if batched)  0.20 us       0.45     Ch. 08
 ── syscall + vmexit (guest→host vhost)         ~2.0 us       2.45     dominant term
 host vhost-vsock copy + schedule peer          ~1.5 us       3.95
 ── syscall return into server guest            ~2.0 us       5.95
 server decode + validate                       0.20 us       6.15     Ch. 10
 server replay into native driver               varies        —        driver-bound
 (return path, same stages, reversed)          ~6.0 us      ~12        symmetric
 ─────────────────────────────────────────────────────────────────────────
 GraftX overhead per round-trip (excl. driver)            ~10–14 us    target band
```

The headline: **~80% of the budget is the two vmexits and the host-side vsock copy**, none of which GraftX code can shrink. The only winning move is to *not pay it*. The whole performance strategy is therefore "amortize or eliminate round-trips," not "shave microseconds off serialization." Concretely:

- **Amortize:** batch N fire-and-forget calls into one transport flush, so the per-call vmexit cost is `~10us / N` (§25.2).
- **Eliminate:** answer round-trips locally where semantics allow — handle minting, caches, speculative returns (§25.4).
- **Overlap:** pipeline so the guest does useful work while a transfer or replay is in flight (§25.2, the Sync chapter (Ch. 13)).

A secondary budget governs bulk transfers; there the cost model is throughput, not latency:

```text
 path             per-byte cost        fixed cost/transfer    when chosen
 vsock inline     2 kernel copies      one frame             payload < 4 KiB inline threshold
 ivshmem bulk     1 copy (server-side  doorbell + descriptor payload ≥ threshold, Ch. 12
                  validate-into-priv)  ring slot
```

The inline-vs-bulk threshold default is **4 KiB** (the single documented default owned by the Memory chapter (Ch. 12), per-API tunable); §25.6 measures the crossover empirically rather than guessing it.

## 25.2 Batching and pipelining

### Command stream batching

Most GPU APIs are *streaming*: long runs of state-setting and draw/dispatch calls whose return values nobody inspects. The client shim (the Client shim chapter (Ch. 09)) will not flush these individually. It appends each encoded command to a per-thread **command buffer** and flushes only on a *barrier*:

- an explicit round-trip call (something that needs a reply now),
- a queue/stream submit / `glFlush` / `vkQueueSubmit`,
- the buffer reaching `MAX_BATCH_BYTES` (default 256 KiB) or `MAX_BATCH_CMDS` (default 4096),
- a coalescing timer firing (`BATCH_LINGER`, default 100 us) so a quiet stream still drains.

```rust
pub struct CmdBatch {
    buf: Vec<u8>,            // pre-sized to MAX_BATCH_BYTES, reused (no realloc)
    count: u32,
    epoch: u64,              // monotonic; tags replies (§25.4)
    deadline: Option<Instant>, // set on first append, drives BATCH_LINGER
}

impl CmdBatch {
    #[inline]
    pub fn push(&mut self, cmd: &impl Encode) -> Flush {
        cmd.encode_into(&mut self.buf); // Ch. 07: bump-append, no per-cmd alloc
        self.count += 1;
        if self.is_round_trip(cmd) || self.buf.len() >= MAX_BATCH_BYTES
            || self.count >= MAX_BATCH_CMDS {
            Flush::Now
        } else {
            self.deadline.get_or_insert_with(|| Instant::now() + BATCH_LINGER);
            Flush::Deferred
        }
    }
}
```

The amortization is dramatic for the common case. A frame that issues 2000 GL calls and one `SwapBuffers` becomes **one** flush: one vmexit pair instead of 2001. Effective per-call overhead drops from ~10 us to ~5 ns of `Vec` append.

**Tradeoff — latency vs throughput.** Batching trades a small added latency (up to `BATCH_LINGER`) for throughput. Interactive workloads that *do* round-trip frequently (e.g. an app polling `glGetError` after every call — sadly common) defeat batching; §25.4's local error cache and §25.4's debug-group elision exist specifically to neutralize that pattern.

### Pipelining (multiple batches in flight)

Even with batching, the guest blocks at every barrier. Pipelining lets the *next* batch be built and even sent while the previous one is still replaying. The transport is full-duplex (the Transport chapter (Ch. 08)), so the client maintains a small window of unacked batches:

```text
 guest:   [build B0]──flush──▶ [build B1]──flush──▶ [build B2]·····(window full, wait)
 wire:           B0 ───────────▶  B1 ───────────▶
 server:         replay B0 ─────────▶ replay B1 ─────────▶
 acks:    ◀─── ack(B0) ──────── ◀─── ack(B1) ────────
```

`PIPELINE_DEPTH` (default 4) bounds in-flight batches. A larger window hides server-side replay jitter but raises tail latency and memory; an interactive 3D app wants a *shallow* window (≤2) to keep input-to-photon latency low, while an offline CUDA pipeline wants it deep (8–16). The depth is therefore per-session policy negotiated at handshake (the Protocol chapter (Ch. 06)) and observable in the profiler (§25.5).

## 25.3 Zero-copy and copy minimization

The bulk plane's goal is **one copy total**, and that copy is mandatory: it is the security copy (the Transport chapter (Ch. 08) and the Memory chapter (Ch. 12)) where the server moves validated bytes out of the client-mutable shared region into server-private memory before handing them to a native driver. We never trust ivshmem contents across a validation boundary, so "true zero-copy into the driver" is rejected for the untrusted path.

What GraftX *will* eliminate is every *other* copy:

| Copy site | Naive | GraftX plan |
|---|---|---|
| client app buffer → frame | memcpy into `Vec` | for bulk: client writes app data straight into the ivshmem staging slot via `reserve_bulk()` (the Transport chapter (Ch. 08)); zero client-side copy |
| frame → kernel (vsock) | unavoidable on inline path | route ≥ threshold through ivshmem instead (§25.1) |
| shared region → driver | trust shared mem (unsafe) | single validate-and-copy into server-private arena (the Memory chapter (Ch. 12)) |
| driver → return surface | extra staging | DMA/readback directly into a return bulk slot |

The client-side reservation API makes the producer write once, in place:

```rust
// the Transport chapter (Ch. 08) surface, used by client shims for H2D-style transfers.
let mut slot = transport.reserve_bulk(len)?;   // borrows an ivshmem slot
driver_call_fills(slot.as_mut_slice());        // e.g. glBufferData's src copied ONCE, here
let id = slot.commit();                          // hands ownership to the wire
batch.push(&Cmd::BufferData { target, id, len });
```

For **map-style** APIs (`glMapBufferRange`, `cuMemHostAlloc`, persistent-mapped Vulkan memory) GraftX maps the ivshmem slot *as* the pointer returned to the app, so app writes land directly in the shared region with no intermediate buffer; the unmap/flush is what triggers the validate-and-copy on the server. This is detailed in the Memory chapter (Ch. 12); here it is a performance lever: it converts a 256 MB upload from "app→buf→frame→kernel→kernel→server→driver" (4+ copies) into "app→shared→driver-copy" (1 copy).

Small-payload note: copies below the **4 KiB** inline threshold (the canonical default owned by the Memory chapter (Ch. 12)) are *faster inline* than the descriptor/doorbell overhead of a bulk slot, so the encoder inlines them into the command frame (the Serialization chapter (Ch. 07)). The 4 KiB inline → bulk transition is benchmarked around its default, not assumed (§25.6).

## 25.4 Avoiding round-trips

The single highest-leverage class of optimization. Four techniques, each with a correctness guard.

**1. Client-side provisional handle tokens.** Object-creating calls (`glGenBuffers`, `cuMemAlloc` of a *named* object, `vkCreateBuffer`) normally must round-trip to learn the server's handle. Handles are server-authoritative — the server mints the wire Handle and the client never puts an invented handle on the wire (the Protocol chapter (Ch. 06) and the Handles chapter (Ch. 11)). For these deferred replies the client keeps a purely **local provisional proxy token** from a per-session allocator, hands it to the app immediately, and batches the create command fire-and-forget; the proxy token is never sent as authority. The client reconciles its provisional token to the server's real handle deterministically when the real handle arrives (the Handles chapter (Ch. 11)). The app never sees the real handle, so there is nothing to wait for. Guard: a provisional token used before its create command has replayed is still ordered correctly because both travel the same in-order stream — the create always precedes the use in the batch.

```rust
// client: no round-trip
pub fn gl_gen_buffers(&mut self, n: i32, out: *mut u32) {
    for i in 0..n {
        // local provisional proxy token (never sent as authority); reconciled
        // to the server's real handle on reply — see the Handles chapter (Ch. 11).
        let tok = self.handles.provisional(HandleKind::GlBuffer);
        unsafe { *out.add(i as usize) = tok.as_gl_name(); }
        self.batch.push(&Cmd::GenBuffer { token: tok });   // fire-and-forget
    }
}
```

**2. Sticky error / state caches.** `glGetError`, `glGetIntegerv(GL_*_BINDING)`, `cudaGetLastError`, `vkGetPhysicalDeviceProperties` are answerable from client-side mirrored state for the vast majority of queries. The shim tracks bound objects, the last-error word, and immutable device properties (fetched once at init). A cache hit returns in nanoseconds with no flush. Guard: any query the cache cannot prove (e.g. errors that only the driver can raise after a fire-and-forget draw) forces a flush + round-trip and *also* drains pending errors so the cache re-syncs. The default is conservative; backends widen the cache as they prove correctness.

**3. Speculative / deferred replies.** For round-trips whose result is *usually* "success" (`vkQueueSubmit`, buffer-orphaning maps), the client may return optimistically and reconcile asynchronously, surfacing a deferred error on the next barrier (ch13's fence model). This is opt-in per call and disabled under a `--strict` session flag.

**4. Async fences instead of blocking sync.** `glFinish`/`cuStreamSynchronize` need not block the client thread on a full round-trip if the app's *next* meaningful action is itself a submit — the client inserts a fence (ch13) and only blocks if the app reads back a result that depends on it. This converts a hard stall into a pipeline bubble that pipelining (§25.2) often hides entirely.

Decision table for a call site:

| Call shape | Returns value app reads? | Strategy |
|---|---|---|
| state setter / draw / dispatch | no | batch fire-and-forget |
| object create | handle only | mint locally (§25.4.1) |
| query of immutable/mirrored state | yes | cache (§25.4.2) |
| query of live driver state | yes | flush + round-trip |
| submit / present | error code | speculative (§25.4.3) unless `--strict` |
| readback (`glReadPixels`, D2H copy) | bulk data | round-trip, bulk return slot |

## 25.5 Profiling methodology

GraftX must be measurable end to end, in production, at low cost. Three layers:

- **Inline counters.** Every flush records `(epoch, cmds, bytes, queue_wait, wire_rtt, server_replay)` into a lock-free per-thread ring. Cost is one `rdtsc` pair per flush, not per call — negligible. Counters are exported on a control opcode and as a `tracing` span tree.
- **`tracing` spans behind a feature.** The client and server are instrumented with `tracing` spans (`graftx::flush`, `graftx::bulk`, `graftx::replay`) gated by a `profiling` Cargo feature so release builds with the feature off pay nothing. With it on, a `tracing-chrome` layer emits Chrome-trace JSON viewable in Perfetto, letting us see batch boundaries, pipeline depth, and stalls on one timeline.
- **Out-of-band perf.** `perf`, `flamegraph`, and `cargo flamegraph` profile the shim and server processes; on the server, GPU-side tools (Nsight, GPUView, RGP) attribute time inside the native driver — that time is *not* GraftX overhead and is reported separately so we never blame the driver's work on ourselves.

The key methodological rule: **always report GraftX overhead with driver time subtracted.** A benchmark that says "frame took 9 ms" is useless; the gate cares about the delta between remoted and native execution of the *same* trace.

```text
 overhead_ratio = (t_remoted - t_native_local) / t_native_local
 per_call_us    = (t_remoted - t_native_local) / n_calls
```

Both are computed by a harness that runs the identical captured API trace (ch10 trace-replay format) once natively on the server host and once through the full GraftX path, on the same GPU, back to back.

## 25.6 Microbenchmarks and macro workloads

**Microbenchmarks** (Criterion, in `benches/`, per crate) isolate one cost each so a regression points at one cause:

| Bench | Measures | Target |
|---|---|---|
| `encode_cmd` | serialize one command (ch07) | < 20 ns |
| `batch_push_1k` | append 1000 fire-and-forget cmds | < 5 us total |
| `roundtrip_vsock` | one synchronous round-trip, no payload | < 14 us |
| `bulk_xfer/{4K,64K,1M,256M}` | throughput vs size, finds threshold | ≥ 80% of raw ivshmem memcpy bw |
| `handle_mint` | local virtual-handle alloc | < 30 ns |
| `decode_validate` | server decode+validate one cmd (ch10) | < 50 ns |

Criterion's statistical mode gives confidence intervals so a 2% real shift is distinguishable from noise — essential for the CI gate (§25.7).

**Macro workloads** (in `xtask bench`, run against a real paired-guest pair or a loopback transport in CI) exercise whole backends:

- `glmark2` / a captured GL trace — graphics throughput and frame pacing.
- A Vulkan triangle + a glTF sample — submit/present round-trip behavior.
- A CUDA SAXPY + a small GEMM (cuBLAS) — H2D/D2H bulk bandwidth and kernel-launch batching.
- A "chatty" trace that calls `glGetError` after every call — proves the §25.4 caches actually neutralize the worst real-world pattern.

Each macro run reports `overhead_ratio`, `per_call_us`, p50/p95/p99 frame or op latency, achieved bulk bandwidth, and average batch size (a low average batch size is itself a regression signal — it means batching stopped working).

## 25.7 Regression gates in CI

Performance that is not gated rots. GraftX will treat perf like correctness: a PR that regresses a tracked metric beyond tolerance fails CI, the same way `clippy -D warnings` and rustfmt already gate (per repo convention).

Mechanism:

1. **Baselines** for every micro/macro metric are stored as JSON in `bench/baselines/<arch>.json`, committed to the repo and updated only by a deliberate "bless" commit.
2. A `cargo xtask perf-gate` step runs Criterion + the loopback macro suite, then compares against the baseline with per-metric tolerance bands:

```text
 metric class          tolerance   rationale
 micro (Criterion)     +5%         tight; isolated, low-noise
 macro overhead_ratio  +8%         noisier; whole-stack
 bulk bandwidth        −8%         (lower is worse)
 avg batch size        −15%        detects batching breakage
```

3. The gate uses Criterion's noise model: only a regression whose confidence interval lies *entirely* above the tolerance band fails, so flaky CI runners do not produce false reds.
4. On hardware-bound metrics CI cannot reproduce (real ivshmem bandwidth needs the two-guest rig), CI runs the **loopback transport** (an in-process `Transport` impl) to gate *software* overhead, and a nightly job on the real rig gates *hardware* numbers and posts results to a tracked dashboard.

```text
 PR push ──▶ build ──▶ clippy/fmt ──▶ unit tests ──▶ cargo xtask perf-gate
                                                          │
                              ┌───────────────────────────┴───────────────┐
                       loopback micro+macro                 (nightly) real-rig macro
                              │                                           │
                       compare vs baseline                        compare vs baseline
                              │                                           │
                    pass / FAIL(>tol) ◀── confidence-interval check ──▶ dashboard
```

A regression that is *intentional* (a correctness fix that costs cycles, or a breadth-first feature that adds a round-trip) is accepted by committing a new baseline in the same PR with a message explaining the trade — making every perf change reviewable and auditable rather than silent.

## 25.8 Summary of levers and their costs

| Lever | Wins | Costs / risk | Default | Off switch |
|---|---|---|---|---|
| command batching | per-call vmexit → ~0 | +up to `BATCH_LINGER` latency | on | flush-per-call |
| pipelining | hides replay jitter | tail latency, memory | depth 4 | depth 1 |
| local handle minting | kills create round-trips | client↔server map drift if buggy (ch11) | on | round-trip create |
| state/error cache | kills query round-trips | staleness if guard wrong | conservative | `--no-cache` |
| speculative reply | kills submit round-trips | deferred error surfacing | on | `--strict` |
| ivshmem bulk | ~1 copy, high bw | needs device + fixed BAR | ≥ threshold | vsock inline |

The through-line: GraftX's performance comes almost entirely from **not making round-trips**, defended by a CI gate that proves each release still does not make them. Latency-microsurgery on serialization is deliberately deprioritized because §25.1 shows it cannot move the needle. Every lever here is reversible, per-session-tunable, and observable in the profiler — so when a backend's correctness needs the slow path, it can take it without code changes, and the gate will simply record the cost.
