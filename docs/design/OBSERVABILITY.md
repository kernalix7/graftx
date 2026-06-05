# Observability

This document describes the GraftX observability layer: the in-process metrics
and tracing building blocks used to instrument forwarded GPU-API call traffic.
It is engineering-internal reference for contributors working on the server,
client, or any code that wants to count calls, measure latency, or emit spans.

Everything here is implemented in
[`crates/graftx-obs`](../../crates/graftx-obs); the server-side integration that
feeds the registry per call lives in
[`crates/graftx-server`](../../crates/graftx-server). The crate is deliberately
small and dependency-light: counters use saturating arithmetic, locks recover
from poisoning, and the only required external dependency is
[`tracing`](https://docs.rs/tracing); `tracing-subscriber` is pulled in only
behind the optional [`fmt` feature](#tracing-integration).

## Design goals

- **Never crash the data plane.** A long-lived server records millions of calls.
  Every counter saturates at `u64::MAX` instead of wrapping or panicking, and a
  thread that panics while holding a metrics lock cannot wedge collection for
  the rest of the process.
- **Cheap and lock-local.** Metrics live behind a `Mutex` per map and are merged
  in place, so the registry grows one entry per distinct API name regardless of
  call volume; recording a call is a hash lookup and a few saturating adds.
- **Best-effort, not authoritative.** These numbers exist for operators and
  load tests, not billing. Clamping at `u64::MAX`, widening `usize` lengths to
  `u64`, and tolerating a slightly stale count under lock poisoning are all
  acceptable trade-offs in service of the first goal.

## Module map

| Module | Type / function | Role |
| --- | --- | --- |
| `lib` | [`CallStats`](#callstats) | Per-API call/byte accumulator. |
| `lib` | [`call_span`](#tracing-integration), [`install`](#tracing-integration) | Tracing helpers. |
| `registry` | [`ObsRegistry`](#obsregistry), [`global`/`record`](#the-process-global-registry) | Thread-safe per-API aggregation. |
| `guard` | [`CallGuard`](#callguard), `begin` | RAII span + record on drop. |
| `timer` | [`Timer`](#timer-and-time_call), `time_call` | Monotonic stopwatch. |
| `hist` | [`LatencyHist`](#latencyhist), `BUCKETS` | Fixed-bucket latency histogram. |

## CallStats

[`CallStats`](../../crates/graftx-obs/src/lib.rs) is the atomic unit of
accounting: a `Copy` struct of three `u64` counters.

```rust
pub struct CallStats {
    pub calls: u64,      // number of calls observed
    pub bytes_out: u64,  // total request/argument bytes
    pub bytes_in: u64,   // total response/result bytes
}
```

"Out" and "in" are relative to the *forwarded call*, not the socket:
`bytes_out` is what the caller sent (the request frame), `bytes_in` is what came
back (the reply frame).

Two mutating operations, both saturating at `u64::MAX`:

- `record(bytes_out, bytes_in)` — increment `calls` by one and add the byte
  counts for a single observed call.
- `merge(&other)` — fold another `CallStats` into this one, summing each field.
  This is how the registry aggregates per-API rows into a `TOTAL`.

Because every field saturates independently, a run that drives one counter to
its max simply pins it there; the others keep counting.

## ObsRegistry

[`ObsRegistry`](../../crates/graftx-obs/src/registry.rs) is the thread-safe
collection that most code interacts with. It keys [`CallStats`](#callstats) by a
`&'static str` API name and is `Send + Sync`, so it is meant to be shared across
worker threads (typically behind an `Arc`).

Internally it holds **two** maps behind **two** independent `Mutex`es:

- `inner: Mutex<HashMap<&'static str, CallStats>>` — the call/byte counters.
- `latency: Mutex<HashMap<&'static str, LatencyHist>>` — per-API latency
  histograms.

Splitting them keeps the two concerns independent: you can count a call without
an observed duration, or fold in a duration without touching the byte counters.

### Poison recovery

A panic while a metrics lock is held *poisons* that `Mutex`. Rather than turn
that into an `unwrap`/`expect` panic on every later access, both internal
lock helpers recover the guard with `PoisonError::into_inner`, so one
misbehaving thread cannot stop metrics collection for the rest of the process.
The stored counters already saturate, so the worst case is a slightly stale
count, never undefined behaviour. This is exercised by the
`record_recovers_from_poisoned_lock` and
`record_latency_recovers_from_poisoned_lock` tests.

### Recording

- `record(api, bytes_out, bytes_in)` — record one call, creating the API's entry
  on first use. Byte counts accumulate via [`CallStats::record`](#callstats).
- `record_frame(api, request_frame, reply_frame)` — convenience over `record`
  for callers that already hold the raw frame bytes: it counts the request
  frame as `bytes_out` and the reply frame as `bytes_in`, using each slice's
  length. Lengths are `usize` widened to `u64`; on the 64-bit targets GraftX
  runs on this is exact (and the counters saturate regardless).
- `record_latency(api, duration)` — fold one observed duration into the API's
  [`LatencyHist`](#latencyhist), creating it on first use. Independent of the
  call/byte counters.

### Reading back

- `snapshot()` — a point-in-time `HashMap<&'static str, CallStats>` clone.
- `snapshot_sorted()` — the same data as a `Vec<(&'static str, CallStats)>`
  sorted ascending by API name, for stable display or comparison. The lock is
  dropped before sorting so it is held only as long as the copy-out takes.
- `total()` — merge every per-API entry into a single aggregate `CallStats`
  (saturating via `merge`).
- `reset()` — clear all per-API call/byte statistics, leaving the registry
  empty and reusable. (Note: `reset` clears the call/byte map; the latency map
  is not cleared by it.)

### Rendering

Two human- and machine-readable renderers over the call/byte counters, both
emitting APIs in ascending name order for deterministic output:

- `report()` — a Markdown table with columns `| API | Calls | Bytes Out |
  Bytes In |`, one row per API followed by a `TOTAL` row. The empty registry
  still emits the header and a zeroed `TOTAL` row.
- `to_json()` — a compact JSON object of the shape

  ```json
  {"apis":{"<api>":{"calls":N,"bytes_out":N,"bytes_in":N}},"total":{"calls":N,"bytes_out":N,"bytes_in":N}}
  ```

  Counts are plain JSON integers. The serializer is hand-rolled (no
  serialization crate) because the schema is fixed and tiny; API names are
  `&'static str` ASCII, so the only escaping needed is for `"` and `\`. The
  empty registry renders as
  `{"apis":{},"total":{"calls":0,"bytes_out":0,"bytes_in":0}}`.

And one renderer over the latency histograms:

- `latency_report()` — for each API (ascending by name), a `### <api>` heading
  followed by that API's [`LatencyHist::report`](#latencyhist) table. With no
  latencies recorded, the result is the empty string.

### The process-global registry

Most call sites do not want to thread an `ObsRegistry` handle through their own
APIs, so the crate offers a single process-wide default:

- `global() -> &'static ObsRegistry` — lazily creates the shared registry on
  first call (backed by a `OnceLock`) and returns a reference to that same
  instance forever after. Because `ObsRegistry` is `Send + Sync`, the reference
  is usable from any thread.
- `record(api, bytes_out, bytes_in)` — a free function that records against
  `global()`, for the common case of using the shared default.

`global()` and the free `record` target the same instance, verified by the
`global_returns_a_stable_reference` and
`record_and_global_record_target_the_same_registry` tests.

## CallGuard

[`CallGuard`](../../crates/graftx-obs/src/guard.rs) is an RAII helper that ties
the lifecycle of one forwarded call to a scope. Construct it with `begin`:

```rust
let mut guard = graftx_obs::begin(&registry, "vulkan", opcode);
// ... do the call ...
guard.set_bytes(request_len, reply_len);
// guard drops here: exits the span and records exactly one call
```

On construction `begin` creates a [`call_span`](#tracing-integration) for the
given `api` and `opcode` and **enters** it for the guard's lifetime. Byte counts
start at zero; `set_bytes(bytes_out, bytes_in)` overwrites them with the final
values once known (calling it repeatedly just overwrites — the guard still
records exactly one call). When the guard drops, it flushes a single
`registry.record(api, bytes_out, bytes_in)`. The entered span is stored last in
the struct so it is dropped *after* the record runs.

Use `CallGuard` when a call's work is bounded by a scope and you want span entry
and metric recording to happen automatically even on early return. Use the
registry's `record`/`record_frame` directly when you already have the byte
counts in hand and don't need a span.

## Timer and time_call

[`Timer`](../../crates/graftx-obs/src/timer.rs) is a monotonic stopwatch
anchored at a single `Instant`:

- `Timer::start()` — anchor a timer at the current instant.
- `elapsed() -> Duration` — time since the anchor. The clock is monotonic, so
  successive reads never decrease.

`time_call(f)` is the convenience wrapper: it starts a timer, runs the closure
`f` exactly once, and returns `(result, duration)`. Feed that duration straight
into [`ObsRegistry::record_latency`](#obsregistry) to populate a histogram.

## LatencyHist

[`LatencyHist`](../../crates/graftx-obs/src/hist.rs) is a tiny fixed-bucket
histogram over call durations. It sorts each observed `Duration` into a
power-of-two **microsecond** bucket and counts the observations.

- `BUCKETS = 12` total buckets.
- Buckets are **upper-bound exclusive.** The non-overflow upper bounds run from
  `1us` (`2^0`) through `1024us` (`2^10`): bucket `i` counts durations whose
  whole-microsecond value is strictly less than `2^i` microseconds.
- The final bucket is an **open-ended overflow** bucket for everything at or
  above the largest tracked bound (`>=1024us`). A duration of exactly `1024us`
  lands here, since `1024 < 1024` is false.

API surface:

- `record(d)` — place a duration into its bucket (saturating the count).
- `count()` — total observations across all buckets (saturating sum).
- `bucket_counts() -> &[u64; BUCKETS]` — borrow the raw per-bucket counts,
  lowest bound first; the last index is the overflow bucket.
- `report()` — a two-column Markdown table (`| bucket | count |`) listing only
  the non-empty buckets to stay compact; the overflow row is shown with a `>=`
  bound. An empty histogram renders header-only.

This is what `ObsRegistry::latency_report` stitches together per API.

## Tracing integration

The crate integrates with [`tracing`](https://docs.rs/tracing) for structured,
nestable spans around individual calls.

- `call_span(api, opcode) -> tracing::Span` — build an `info`-level span named
  `graftx.call` carrying the `api` name and `opcode` as fields. Entering it
  scopes any nested events to that call. [`CallGuard`](#callguard) uses this
  internally.
- `install() -> bool` — convenience for binaries that want default
  human-readable logging without wiring up `tracing-subscriber` themselves. It
  is feature-gated:
  - With the optional **`fmt` feature** enabled, it installs a
    `tracing-subscriber` fmt subscriber as the global default and returns `true`
    if this call installed it (`false` if a global default was already set). It
    never panics, so it is safe to call from `main` or tests.
  - Without the `fmt` feature, it is a stub that always returns `false` and
    installs nothing (the optional `tracing-subscriber` dependency is not
    compiled in). Keeping the function present means callers can refer to
    `install` regardless of features.

Library code should emit spans/events via `tracing` and let the binary decide
whether to `install()` a subscriber; nothing in `graftx-obs` installs a
subscriber on its own unless you call `install`.

## Server integration

The server-side glue lives in
[`serve_with_metrics`](../../crates/graftx-server/src/serve.rs) in
`graftx-server`. It behaves exactly like the plain `serve` loop — same
clean-close (`UnexpectedEof`/`BrokenPipe` ends the loop with `Ok(())`) and
error-propagation semantics — but instruments each handled frame:

1. Receive one frame from the [`Transport`](TRANSPORT.md).
2. **Peek** the frame's opcode before handling: decode the header and map the
   opcode's API namespace (the high byte, via `proto::opcode_api`) to a stable
   `&'static str` through the crate-internal `api_name` table (`0x00 => "core"`,
   `0x01 => "vulkan"`, …, `0x0B => "amf"`, anything else `=> "unknown"`). A
   frame whose header cannot be decoded yields no API name and is left
   unattributed.
3. Hand the frame to `Session::handle`. On a **successful** handle, send the
   reply, then — if an API name was resolved — record one call via
   `registry.record(api, frame.len() as u64, reply.len() as u64)`. The inbound
   request frame length is `bytes_out` and the reply frame length is `bytes_in`,
   matching the [`ObsRegistry`](#obsregistry) "out"/"in" convention.
4. A protocol error ends the session cleanly (for now) and records no metric.

So the registry's keys are exactly the API names from `api_name`, and the
`bytes_out`/`bytes_in` columns correspond to request/reply frame sizes on the
wire. See [`PROTOCOL.md`](PROTOCOL.md) for the opcode/`ApiId` scheme and
[`BACKENDS.md`](BACKENDS.md) for the per-API status those names map to.

## See also

- [`PROTOCOL.md`](PROTOCOL.md) — opcode scheme and `ApiId` table behind the API
  names the registry keys on.
- [`TRANSPORT.md`](TRANSPORT.md) — the `Transport` trait that `serve_with_metrics`
  drives, including the `CountingTransport` byte counters.
- [`BACKENDS.md`](BACKENDS.md) — per-API status matrix matching the registry's
  API-name keys.
