# 27. Observability: Logging, Tracing & Metrics

How GraftX will instrument itself end to end: structured logging and per-call timing spans built on `tracing`, a low-overhead metrics layer (calls/s, bytes moved, queue depth, latency histograms), a binary trace-record/replay tool for offline reproduction, and a disciplined approach to diagnosing the hardest class of bug — a divergence between what the guest expected and what the server actually produced.

Observability is not a feature, it is the only practical way to debug a system whose two halves run in different guest VMs, link different native drivers, and exchange an opaque binary stream. When a frame is corrupt, a kernel returns garbage, or latency spikes, the developer cannot attach one debugger across both guests. GraftX therefore treats logs, spans, and metrics as first-class wire-correlated artifacts. This chapter designs that instrumentation across all four crates (`graftx-protocol`, `graftx-transport`, `graftx-client`, `graftx-server`) and the cross-cutting `graftx-otel` glue. It depends on the correlation-ID (`req_id`) design from the Protocol chapter (Ch. 06), the `Transport` trait from the Transport chapter (Ch. 08), the client dispatch path from the Client shim chapter (Ch. 09), the server decode/validate/replay loop from the Server core chapter (Ch. 10), and the fence/ordering model from the Sync chapter (Ch. 13) — it does not redefine them.

## 27.1 Goals, constraints, and the overhead budget

Three constraints shape every decision below.

1. **The client shim is in the application's hot path and its address space.** Every microsecond and every allocation in the shim is charged to the guest application. Instrumentation that is cheap on a server is unacceptable inside a `glDrawArrays` wrapper called 100k times per frame.
2. **There is no shared clock.** The two guests have independent monotonic clocks. Any latency we attribute to "the wire" must be measured with care (see §27.5) — naive subtraction of timestamps from two hosts is meaningless.
3. **Logs may contain attacker-influenced data.** The server processes an untrusted stream. Decoded command arguments must never be logged with format strings that could be abused, and large/binary payloads must be summarized, not dumped, by default.

The overhead budget GraftX commits to: with the default `info` level and metrics on, instrumentation must add **< 1% wall-clock** to a draw-heavy and a compute-heavy benchmark, and **zero heap allocations on the per-call fast path** when the span level is disabled. This is achievable because `tracing` spans compile to a cheap enabled-check when the subscriber filters them out, and `tracing`'s macros are zero-cost when the level is statically or dynamically disabled.

## 27.2 The `tracing` stack

GraftX standardizes on the `tracing` ecosystem (not `log`) so that spans nest and carry structured fields. A `log`→`tracing` bridge (`tracing-log`) will capture any `log::` records emitted by dependency crates (e.g. `ash`, `wgpu`) so nothing is lost.

```
                   tracing macros (info!, span!, event!)
                                 │
                          tracing::Subscriber
                                 │  (Registry stores span data)
        ┌────────────────────────┼───────────────────────────┐
   fmt layer              metrics layer               otel/file layer
 (human or JSON         (counts spans/events,       (export spans as a
  to stderr/file)        records histograms)         binary trace, §27.4)
```

A `Registry` from `tracing-subscriber` is the backbone; we attach **layers** rather than swapping subscribers, so each concern is independent and individually toggleable. The proposed initialization, called once per process from `main`/shim-ctor:

```rust
// crate: graftx-otel
pub struct ObsConfig {
    pub level: tracing::Level,        // GRAFTX_LOG, default INFO
    pub format: LogFormat,            // Pretty | Json | Compact
    pub metrics: bool,                // GRAFTX_METRICS, default true on server
    pub trace_file: Option<PathBuf>,  // GRAFTX_TRACE → enable record (§27.4)
    pub sample: f32,                  // span sampling fraction, 0.0..=1.0
}

pub fn init(cfg: &ObsConfig) -> Result<ObsGuard, ObsError> {
    let env = EnvFilter::try_from_env("GRAFTX_LOG")
        .unwrap_or_else(|_| EnvFilter::new(cfg.level.as_str()));
    let fmt = fmt_layer(cfg.format);                 // Layer
    let metrics = cfg.metrics.then(MetricsLayer::new);
    let recorder = cfg.trace_file.as_deref().map(TraceRecordLayer::open)
        .transpose()?;                               // §27.4
    tracing_subscriber::registry()
        .with(env)
        .with(SamplingLayer::new(cfg.sample))
        .with(fmt)
        .with(metrics)
        .with(recorder)
        .try_init()?;
    Ok(ObsGuard { /* flush handles, owns the file writer */ })
}
```

`ObsGuard` is returned so its `Drop` flushes the trace file and any buffered exporter. In the client shim, `init` runs from a `ctor`-registered constructor guarded by `Once`, reading config from environment variables only (no config file parsing in a `dlopen`'d library, to avoid surprising the host app). On the server, config comes from CLI flags layered over env vars.

**Why not `tokio-console` only?** Async task introspection (the Sync chapter's (Ch. 13) submission queues) is valuable, and a `console-subscriber` layer can be slotted in under a feature flag. But it does not capture per-call GPU semantics, so it complements rather than replaces the layers above.

## 27.3 Structured logs and per-call spans

Every remoted call is wrapped in a span carrying the fields that make a log line greppable and joinable across both guests. The single most important field is the **correlation id** (`cid`) — the `req_id:u32` request/response correlation id from the Protocol chapter (Ch. 06), assigned by the client when it enqueues a frame and echoed verbatim in the completion frame. This is distinct from `seq:u64`, the per-session monotonic ordering/fence sequence owned by the Sync chapter (Ch. 13); spans record both, but `cid`/`req_id` is the join key between a client span and the matching server span. A span also carries `seq` so fence-ordering analysis (§27.7) can use it.

Client-side span, emitted by the generated shim wrappers (the Client shim chapter (Ch. 09)):

```rust
// inside the generated wrapper for, e.g., vkQueueSubmit
let span = tracing::span!(
    target: "graftx::call",
    Level::DEBUG,
    "call",
    cid = cid,          // req_id:u32 correlation id (Ch. 06)
    seq = seq,          // seq:u64 ordering/fence sequence (Ch. 13)
    op = opcode as u32, // (ApiId<<24)|call_id, e.g. Vulkan vkQueueSubmit (Ch. 06)
    op_name = "vkQueueSubmit",
    session = session_id,
);
let _e = span.enter();
// ... serialize, send, await completion ...
tracing::event!(Level::TRACE, bytes_ctrl = ctrl_len, bytes_bulk = bulk_len);
```

The span is `DEBUG`-level, so at the default `info` level it costs only an enabled-check and is not created — meeting the zero-allocation budget. Field naming follows a fixed schema (`cid`, `seq`, `op`, `op_name`, `session`, `bytes_ctrl`, `bytes_bulk`, `result`, `dur_us`) so JSON logs from client and server can be loaded into the same query engine and joined on `(session, cid)`.

Decision table for what is logged at which level:

| Level | Client | Server |
|-------|--------|--------|
| ERROR | shim init failure, transport fatal, panic-hook | validation reject, driver error mapped to non-success, OOM/quota trip |
| WARN  | reconnect/backpressure stall, fallback to slow path | slow call (> p99 threshold), handle table near limit, suspicious-but-allowed command |
| INFO  | session open/close, negotiated protocol version, transport mode (vsock/ivshmem) | session lifecycle, device enumeration, replay backend selected |
| DEBUG | one `call` span per remoted call | one `replay` span per decoded command, with validation outcome |
| TRACE | per-call byte counts, bulk descriptor ids, fence ids | full decoded argument struct (truncated), per-handle remap |

**Argument logging is summarized by default.** At TRACE we log decoded structs via a `#[derive(LogSummary)]`-style helper that prints scalar fields inline but renders slices/pointers/blobs as `&[u8; 4096]@0x…` rather than dumping bytes — bounding log volume and avoiding leaking large textures into log files. A `GRAFTX_LOG_BLOBS=1` escape hatch dumps full payloads for deep debugging only.

## 27.4 Metrics: counters, gauges, histograms

Metrics answer "is the system healthy and where is the time going?" without log spelunking. GraftX will not pull in a heavyweight metrics backend by default; instead a small `MetricsLayer` (a `tracing` layer) maintains lock-light atomics and exposes them two ways: a Prometheus text endpoint on the server (feature `metrics-prom`) and an in-process snapshot API usable by tests and the trace tool.

```rust
#[derive(Default)]
pub struct Metrics {
    pub calls_total:    [AtomicU64; OPCODE_COUNT], // per-opcode counter
    pub calls_failed:   AtomicU64,
    pub bytes_ctrl_tx:  AtomicU64,
    pub bytes_bulk_tx:  AtomicU64,
    pub bytes_bulk_rx:  AtomicU64,
    pub submit_q_depth: AtomicU64,  // gauge: in-flight commands (Sync chapter, Ch. 13)
    pub bulk_ring_used: AtomicU64,  // gauge: bytes outstanding in ivshmem ring
    pub latency_us:     Histogram,  // HDR-style, per the call span's dur_us
}
```

The per-call critical-section is a single relaxed `fetch_add` per counter and one histogram record — cheap enough to leave on by default on the server. On the **client** the heaviest gauge (`submit_q_depth`) is already maintained by the dispatch layer (the Client shim chapter (Ch. 09)), so the metrics layer reads, rather than recomputes, it.

The metrics that matter most, and why:

| Metric | Type | Surface | What it diagnoses |
|--------|------|---------|-------------------|
| calls/s by opcode | counter (rate) | both | which API dominates; unexpected chatty opcodes |
| failed calls/s | counter | both | correctness regressions, validation rejects |
| ctrl bytes/s, bulk bytes/s | counter | both | whether vsock or ivshmem is the bottleneck (Transport, Ch. 08) |
| submit queue depth | gauge | client | backpressure: client outrunning server |
| bulk ring used | gauge | both | zero-copy ring saturation (Memory, Ch. 12) |
| call latency p50/p99 | histogram | client | end-to-end stall, tail latency |
| replay latency p50/p99 | histogram | server | server-side driver time vs. wire time |

The histogram uses fixed exponential buckets (1µs..1s) so it is allocation-free and mergeable across sessions. The server exposes `GET /metrics` (Prometheus) and `GET /healthz`; both bind to a loopback-or-vsock admin endpoint only, never the data plane, so metrics scraping cannot interfere with or observe untrusted command traffic.

```
client                          server
calls_total ───┐                ┌─── calls_total
latency_us  ───┤  join on cid   ├─── replay_latency_us
submit_q    ───┘  in dashboards └─── bulk_ring_used
   the difference (client latency − replay latency) ≈ wire + queue time
```

## 27.5 The two-clock problem and span correlation

Because client and server clocks are independent, GraftX measures three distinct durations and never mixes them:

- `t_client_total` — measured entirely on the client: from frame-enqueue to completion-received. Includes serialize + wire + server time + wire back.
- `t_server_replay` — measured entirely on the server: from decode-start to driver-call-return. Reported back inside the completion frame as a `u32` microsecond field.
- `t_wire ≈ t_client_total − t_server_replay` — derived, never measured directly. This is the only sound way to separate transport cost from driver cost without a shared clock.

To make spans joinable in a viewer, the server's `replay` span records `cid` and its `dur_us`, and the completion frame carries `dur_us` back so the client span can attach `server_us` as a field. A periodic, lightweight clock-offset probe (a dedicated ping opcode timestamped on both ends, NTP-style four-timestamp exchange) estimates the offset and one-way delay so that an offline merge tool can shift server spans onto the client timeline for visualization. The estimate is advisory — used for drawing waterfall diagrams, not for correctness.

## 27.6 The trace record/replay tool

The flagship debugging artifact is `graftx-trace`, a binary that records the exact command stream of a session and replays it deterministically later, decoupled from the live application. This is how a guest crash becomes a reproducible regression test.

**Record.** `TraceRecordLayer` (§27.2) and a tap in the transport write the *post-serialization* frames — control frames plus referenced bulk payloads — into a self-describing container:

```
graftx-trace v1 file layout
┌────────────────────────────────────────────────┐
│ Header: magic "GFXT", file ver, protocol ver,    │
│         session params, negotiated caps          │
├──────────────┬───────────────────────────────────┤
│ Record       │ Record … (one per frame)           │
│  ├ dir: C→S / S→C                                  │
│  ├ cid                                             │
│  ├ wall_ns (recorder's clock)                      │
│  ├ ctrl_len, ctrl_bytes                            │
│  └ bulk_len, bulk_bytes (inline or offset)         │
├──────────────┴───────────────────────────────────┤
│ Trailer: per-opcode counts, byte totals, crc      │
└────────────────────────────────────────────────┘
```

Records are length-prefixed and the whole file is optionally zstd-framed. Because frames are captured *after* serialization, a trace is exactly what crossed the wire — replaying it cannot accidentally diverge through a re-serialization change, and the protocol version in the header lets the tool refuse mismatched streams.

```rust
pub enum TraceMode {
    /// Feed recorded C→S frames into a real server; compare S→C to the recording.
    ReplayClient { server: TransportAddr, compare: bool },
    /// Stand in as a server: answer a live client with recorded S→C frames.
    MockServer { trace: TraceFile },
    /// Offline: decode and pretty-print/diff without any transport.
    Inspect { filter: Option<OpSet> },
}
```

`ReplayClient` is the workhorse: it pushes the recorded client frames at a fresh server, captures the server's real responses, and diffs them against the recorded responses (§27.7). `MockServer` lets a client developer run against a deterministic recording with no GPU at all. `Inspect` dumps a human-readable timeline and is the offline analog of the live `Inspect`-level logs.

A trace can also be *minimized*: `graftx-trace minimize` performs delta-debugging, repeatedly dropping spans of the command stream and re-running `ReplayClient` until it finds the smallest prefix/subsequence that still reproduces a mismatch or crash — turning a 200k-call session into a 12-call repro.

## 27.7 Diagnosing guest-expectation vs. server-result mismatches

This is the hardest bug class: the guest believed a call would yield value *X* (a return code, an output buffer, a fence signal, a queried capability), but the server produced *Y*. Causes include driver behavioral differences between the guest's stubbed expectation and the real Windows driver, an unvalidated/under-validated command (the Server core chapter (Ch. 10)), handle-remap errors (the Handles chapter (Ch. 11)), or a serialization round-trip defect (the Serialization chapter (Ch. 07)).

GraftX provides a layered detection strategy:

1. **Result-code mismatch.** The client knows the *type* of a call's result. When `compare` mode replays a trace, any difference in the returned status code is flagged immediately with the `cid`, opcode, recorded-vs-actual codes. This catches the cheapest 80% of divergences.

2. **Output-payload digest.** For calls that return bulk data (readbacks, `glReadPixels`, `cudaMemcpyDtoH`, query results), the recorder stores a **digest** (e.g. xxh3) of the output, not necessarily the full bytes. On replay the digests are compared; a mismatch localizes the divergent call without bloating the trace. Full-byte capture is opt-in per opcode for pixel-exact diffs.

3. **Canary fields.** An optional debug mode injects, for selected query opcodes, a client-side *assertion frame* carrying the value the guest's local stub assumed (e.g. an `EGLConfig` attribute or a `vkGetPhysicalDeviceProperties` field). The server compares the live driver's value and emits a `WARN` `mismatch` event with both. This surfaces silent capability lies before they cascade into a crash.

4. **Fence/ordering divergence.** Using the `seq` ordering sequence and fence ids from the Sync chapter (Ch. 13), the tool checks that the *order and timing* of signal events matches the recording; a fence that signals out of recorded order points at an async-submission or dependency bug rather than a per-call data bug.

The standard mismatch event has a fixed shape so it is machine-parseable:

```rust
tracing::event!(
    Level::WARN, target: "graftx::mismatch",
    cid, op_name,
    kind = "result_code",          // result_code | payload_digest | canary | fence_order
    expected = ?recorded,
    actual = ?live,
);
```

**Walk-through.** A guest reports corrupted geometry after a driver update. Recording was on; the engineer runs `graftx-trace minimize --reproduce payload_digest trace.gfxt`. Delta-debugging reduces the session to a `MapBuffer`/`UnmapBuffer`/`DrawElements` triple whose readback digest diverges. `Inspect` shows the `MapBuffer` length the client sent differs from the bytes the server copied into private memory (the Transport chapter's (Ch. 08) copy-then-validate boundary), revealing an off-by-one in a generated length field. The minimized 3-frame trace becomes a committed regression test that `ReplayClient --compare` runs in CI, so the bug can never silently return.

## 27.8 Tradeoffs and milestone phasing

| Decision | Alternative rejected | Why |
|----------|---------------------|-----|
| `tracing` + layers | `log` + ad-hoc counters | spans nest, carry `cid`, and join client/server timelines |
| frames captured post-serialization | capture decoded structs | replays exactly what crossed the wire; immune to re-serialization drift |
| digests for readbacks | always store full payloads | bounds trace size; full bytes opt-in per opcode |
| derived `t_wire` | shared-clock timestamps | no trustworthy cross-guest clock exists |
| summarized arg logging | full arg dumps | bounds volume, avoids leaking untrusted/large payloads |

Phasing maps onto the roadmap, but the authoritative task IDs, ordering, and acceptance criteria live in the Milestones chapter (Ch. 30); this section only states the intended shape so the two stay consistent. Intended: M0 ships `init`, structured logs, and per-call spans so the first round-trip is observable; M1 (Vulkan) adds the metrics layer and the histogram-based latency split, since Vulkan is the performance spine, and the `graftx-trace` record/inspect modes land alongside it. `ReplayClient --compare`, canary fields, and `minimize` follow as the validation surface (the Server core chapter (Ch. 10)) and backends mature, becoming the backbone of the cross-guest regression suite. If Ch. 30 does not yet carry explicit obs-init/spans (M0) and metrics/`graftx-trace` (M1) tasks, they should be added there — the Milestones chapter owns the backlog, not this chapter.
