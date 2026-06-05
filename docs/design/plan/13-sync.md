# 13. Synchronization, Fences & Async Submission

How GraftX will replicate GPU synchronization primitives across the guest-to-guest boundary, submit work asynchronously, batch and coalesce commands, preserve per-API ordering, and hide the round-trip latency that naive remoting would otherwise expose.

## 13.1 The core problem

Every GPU API GraftX targets (see the Introduction chapter (Ch.01) for the full list) exposes some flavor of synchronization object: GL fence sync (`GLsync`), EGL/GLX sync, Vulkan `VkFence`/`VkSemaphore`/`VkEvent` (binary and timeline), CUDA events and streams, OpenCL events, Level Zero fences/events, HIP events. In a local driver these are cheap: the object lives in the same address space, the driver schedules submissions onto in-order or out-of-order hardware queues, and the application blocks on a memory poll or a kernel wait.

GraftX splits this in two. The *real* synchronization object only ever exists on the server (Windows guest, native driver) — which also means the server is authoritative for its wire `Handle` (D1; the wire `Handle` layout lives in the Protocol chapter (Ch.06) and the object model in the Handles chapter (Ch.11)). The client (Linux guest) holds a *local proxy* and must answer client-side queries — "is this fence signaled?", "wait until it is" — without a real object. The naive implementation makes every API call a synchronous RPC: serialize, push over vsock, wait for the reply, deserialize. For a workload issuing tens of thousands of draw calls per frame, the accumulated half-round-trips (each ~5-50 µs even on the fast control plane, see the Transport chapter (Ch.08)) dominate everything and the GPU starves. This chapter's job is to make the common path *never block on the wire* while still honoring the ordering and visibility contracts each API promises.

```
 GUEST (Linux, client shim)            HOST (Windows, server replay)
 ┌──────────────────────────┐          ┌────────────────────────────┐
 app → shim → cmd encoder    │          │  decoder → validator → GPU  │
 │      │                    │          │     │                       │
 │   handle table (proxies)  │  vsock   │  object table (real objs)   │
 │   fence ledger            │ <======> │  fence tracker              │
 │   submission ring         │ ivshmem  │  submission scheduler       │
 └──────────────────────────┘          └────────────────────────────┘
```

## 13.2 Two classes of API calls

The first design decision is to partition every intercepted entry point into one of three latency classes. This classification (a const property of each command opcode, encoded in the protocol descriptor of the Protocol chapter (Ch.06)) drives everything downstream.

| Class | Meaning | Wire behavior | Examples |
|-------|---------|---------------|----------|
| `Async` | No return value the app inspects; pure state mutation or work submission | Encode, enqueue, return immediately. Never flushed alone. | `glDrawArrays`, `glUniform*`, `vkCmdDraw`, `cuLaunchKernel`, `clEnqueueNDRangeKernel` |
| `DeferredReply` | Returns a value that is a *handle*; the client hands back a LOCAL provisional proxy token now and reconciles to the server-minted wire `Handle` later (D1) | Mint local provisional token, enqueue, return token to the app. Reconcile when the server's real `Handle` arrives. | `glFenceSync`, `vkCreateFence`, `glCreateShader`, `cuEventCreate` |
| `Sync` | App reads back real device/driver state; must round-trip | Flush pending batch, block for reply | `glClientWaitSync(timeout>0)`, `vkGetFenceStatus`, `glGetError` (in debug), `glMapBuffer`, `cuMemcpyDtoH` |

The whole performance strategy is to push as many entry points as possible into `Async`/`DeferredReply` and pay the round trip only at genuine `Sync` points. Sync objects are the linchpin because they are precisely the API's mechanism for converting asynchronous GPU work into observable host events — so if GraftX models them well, almost everything else can stay async.

## 13.3 Sequence numbers: the spine of ordering

GraftX will stamp every encoded command with the **per-session monotonic 64-bit sequence number** `seq` (D3: `seq:u64` is the ordering/fence-sequence field owned by this chapter; it is distinct from `req_id:u32`, the request/response correlation id owned by the Protocol chapter (Ch.06), and there is no u32 ordering seq). `seq` is the universal ordering token: fences, batching, and stall detection all key off it, while reply matching keys off `req_id`. `seq` is allocated from a single per-session counter so it doubles as the canonical wire-order position; per-stream ordering is then derived from it (below).

```rust
/// Per-session monotonic command sequence (D3). Allocated once per encoded
/// command from a single session counter. Never wraps in practice (2^64 cmds).
/// Distinct from req_id:u32 (Ch.06), which correlates a request to its reply.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Seq(pub u64);

/// A "stream" is the ordering domain. One per GL context, per VkQueue,
/// per CUDA stream, per CL command-queue. Work within a stream is ordered;
/// work across streams is only ordered through explicit sync objects.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct StreamId(pub u32);
```

`seq` is session-global and strictly increasing, which makes it identical to the order in which commands are written into the per-session transport ring (the Transport chapter (Ch.08) ring) and the order in which the server replays them (the Server core chapter (Ch.10) in-order replay). A *stream* is a logical sub-ordering carved out of that one wire order: because every command also carries its `StreamId`, the in-order session wire trivially preserves per-stream order (the subsequence of `seq` values for any one stream is monotonic). Multiple streams therefore interleave on a single session channel without a separate per-stream counter — the per-session `seq` *is* the per-stream order restricted to that stream's commands. Channel multiplexing in the transport header (the `channel` field, the Transport chapter (Ch.08)) separates control from bulk planes, not streams; all streams of a session share the same ordered command channel so that the server's single in-order replay loop honors every stream's ordering for free.

The mapping from API concept to `StreamId` matters:

- **OpenGL / GLES / EGL / GLX**: one stream per GL context. GL has a single implicit command stream per context, in-order, so this is exact.
- **Vulkan**: one stream per `VkQueue`. Command buffer *recording* is client-local and unordered relative to other recording; only `vkQueueSubmit` injects work into the queue stream. Cross-queue ordering rides on semaphores (13.5).
- **CUDA / HIP**: one stream per CUDA stream; the default stream (stream 0) has its legacy global-sync semantics modeled as a stream that, on submit, inserts implicit waits on all other streams of the context.
- **OpenCL / Level Zero**: one stream per command queue / command list; out-of-order queues are modeled by *not* implying per-stream `seq` ordering and relying purely on event dependencies (the wire `seq` still records arrival order; the server simply does not treat it as an execution dependency for these streams).

The server maintains, per stream, a **completion watermark** `completed_seq`: the highest `seq` (in that stream's subsequence) whose effects are guaranteed visible on the device. A fence/event "signaled" query reduces to comparing the fence's recorded `seq` against the stream's `completed_seq` — covered next.

## 13.4 Fences and the client-side fence ledger

When the app creates a fence at a point in the stream, GraftX records the *position* and hands the app a local provisional token (D1 — the client never invents the wire `Handle`; the server mints it). The key insight: a GL/CUDA-style fence signals when "all work submitted before it completes", which is exactly "`completed_seq >= fence.seq`". So a fence is, on the wire, almost free — it is a sequence number plus the server-minted object handle that arrives with the reply.

```rust
pub struct FenceProxy {
    pub stream: StreamId,
    pub seq:    Seq,             // command position this fence guards
    pub server_handle: Option<Handle>, // server-minted wire Handle (Ch.06/D5);
                                       // None until the server's reply arrives
    pub state:  FenceState,      // client's cached belief
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FenceState { Unsignaled, ProbablySignaled, Signaled }
```

The client keeps a **fence ledger**: a `BTreeMap<Seq, FenceProxy>` per stream plus the last `completed_seq` the server reported. Client-side fence operations resolve against the ledger first:

```rust
impl FenceLedger {
    /// glClientWaitSync(timeout=0) / vkGetFenceStatus fast path.
    fn poll(&self, f: &FenceProxy) -> Poll {
        if f.seq <= self.completed_seq { Poll::Signaled }      // no round-trip
        else if self.in_flight_estimate(f.seq) { Poll::Pending } // no round-trip
        else { Poll::NeedRoundTrip }                            // ask server
    }
}
```

The `completed_seq` value is refreshed *opportunistically* — it piggybacks on every reply the server sends for any reason (a `DeferredReply` reconciliation, a genuine `Sync`, or a periodic heartbeat, see 13.9). This means a polling loop (`while (glClientWaitSync(s,0,0)!=SIGNALED){}`) often resolves entirely client-side once a recent reply has advanced the watermark, costing zero extra round-trips. Each such reply is matched to its request by `req_id` (Ch.06); the `completed_seq` it carries advances the ordering watermark independently of which `req_id` delivered it.

A *blocking* wait with a real timeout (`glClientWaitSync(GL_SYNC_FLUSH_COMMANDS_BIT, t>0)`, `vkWaitForFences`) is genuinely `Sync`: GraftX must flush the batch (so the work the fence guards is actually on the wire) and then either (a) wait for a server reply that the watermark reached `fence.seq`, or (b) time out. To avoid a busy server-poll, the server registers a **completion callback** and pushes a single `FenceSignaled{stream, seq}` notification when the underlying driver wait returns. The client's blocking wait is implemented as a condvar parked on the ledger, woken by the notification dispatcher.

```
client                              server
  glFenceSync()  --enqueue-->       (no traffic yet, batched)
  ...thousands of async cmds...
  glClientWaitSync(t>0)
     flush batch  =============>     replay all, install GL fence
     park on condvar                 glClientWaitSync on real GLsync (host thread)
                  <===FenceSignaled  fence completed_seq advanced
     wake, return SIGNALED
```

## 13.5 Semaphores: cross-stream and timeline

Binary semaphores (Vulkan binary `VkSemaphore`, CL events used as cross-queue deps) and timeline semaphores need a richer model than a single watermark because they encode *dependencies between* streams, possibly across the host/device boundary, and Vulkan timeline semaphores can be waited and signaled from the host (`vkSignalSemaphore`, `vkWaitSemaphores`) as well as the device.

GraftX will model each semaphore as a server-resident object (referenced on the wire by its server-minted `Handle`, Ch.06/D5) plus a client-side **timeline shadow**: for timeline semaphores, the last value the client *believes* is signaled (refreshed like `completed_seq`); for binary semaphores, a single pending/signaled bit per submission generation. The crucial rule: **wait-before-signal hazards must never be encoded as client-side blocking**. In Vulkan a `vkQueueSubmit` may legally wait on a semaphore that a *later* submit will signal (timeline only) — the driver handles the ordering. GraftX therefore forwards the wait/signal *intent* (semaphore handle + value) inside the submission record and lets the native driver schedule it; the client never blocks on a semaphore unless the app explicitly calls a host-side wait.

```rust
pub enum SyncOp {
    Wait   { sem: Handle, value: u64 },  // value ignored for binary
    Signal { sem: Handle, value: u64 },
}

pub struct SubmitRecord {
    pub stream:  StreamId,
    pub seq_lo:  Seq, pub seq_hi: Seq,   // range of cmds this submit covers
    pub waits:   SmallVec<[SyncOp; 4]>,
    pub signals: SmallVec<[SyncOp; 4]>,
}
```

Host-side timeline waits (`vkWaitSemaphores`) are `Sync` and use the same condvar-park + `SemaphoreSignaled{sem, value}` notification mechanism as fences. The server-side validator (the Server core chapter (Ch.10) decode/validate path, with the trust model in the Security chapter (Ch.23)) must check that referenced semaphore handles are owned by the session and that timeline values are monotonic, since the stream is untrusted.

## 13.6 Async submission and the command batch

Async commands are accumulated into a **batch buffer** rather than sent individually. A batch is a contiguous encoded run of commands for a single session (commands for multiple streams may interleave inside one batch — each carries its `StreamId`, and the per-session `seq` assigned at encode time fixes their wire order so the server's in-order replay (the Server core chapter (Ch.10)) preserves every stream's ordering). Each command body is the protocol frame defined in the Protocol chapter (Ch.06); the batch is what actually crosses the transport, wrapped in the transport frame and carried on the ivshmem bulk plane (the Transport chapter (Ch.08)) when it references large payloads, otherwise on the vsock control plane.

```rust
pub struct Batch {
    buf: BytesMut,          // encoded commands, length-prefixed
    cmd_count: u32,
    first_seq: Seq, last_seq: Seq,
    needs_reply: bool,      // true if any DeferredReply/Sync inside
    bulk_refs: SmallVec<[ShmSlice; 8]>, // shared-region slices referenced (Ch.08/D6)
}
```

A batch is **flushed** (sent) when any of these trigger fire — this is the heart of latency-vs-throughput tuning:

| Trigger | Rationale |
|---------|-----------|
| Byte threshold (default 256 KiB) | Bound memory; keep frames pipelined |
| Command-count threshold (default 4096) | Bound per-batch decode latency on server |
| A `Sync` command is encoded | Correctness: must round-trip now |
| `SwapBuffers`/`vkQueuePresent`/explicit `glFlush`/`glFinish` | Frame boundary; latency floor |
| Reconcile pressure: too many unresolved `DeferredReply` handles | Bound the ledger size |
| Idle timer (default 200 µs since last enqueue) | Avoid stranding a half-full batch when the app pauses |

`glFinish` and `vkDeviceWaitIdle` are full barriers: flush, then block until the server reports the stream drained. `glFlush` is a hint — GraftX treats it as "flush the batch but do not wait", which matches GL semantics (flush guarantees eventual execution, not completion).

## 13.7 Coalescing and redundancy elimination

Beyond batching, GraftX will *coalesce* within a batch to cut both wire bytes and server replay cost. Coalescing is opt-in per opcode and must be provably semantics-preserving:

- **Redundant state collapse**: consecutive `glBindTexture(GL_TEXTURE_2D, t)` with no intervening use, or repeated identical `glUniform*` on the same location, collapse to the last. A small per-stream "pending state" map tracks the latest value; a command flushes the map only when a draw/dispatch consumes that state.
- **Uniform/descriptor run merging**: a run of `glUniform4f` to contiguous locations becomes one `glUniform4fv`. CUDA `cuMemcpyHtoDAsync` to adjacent regions merge into one copy.
- **Draw merging**: not done blindly — only structurally identical consecutive `glDrawArrays`/`glDrawElements` with matching state may be promoted to `glMultiDraw*`. This is gated behind a feature flag (default off) because it can perturb timing-sensitive apps.

Coalescing never crosses a `Sync` boundary, a fence position, or a `SwapBuffers`, because those are observation points. Each coalescer is a pure function `fold(&mut PendingState, Command) -> Option<EmittedCommand>` and is unit-testable against a "replay equivalence" oracle. The ordering invariant is: *the sequence of commands the server replays must produce GPU/driver state observably identical to replaying the un-coalesced stream at every Sync point.*

## 13.8 Deferred replies and handle reconciliation

`DeferredReply` is what lets object-creating calls stay off the critical path. When the app calls `glCreateShader` or `vkCreateFence`, the client mints a **local provisional token** (D1 — purely client-local, never sent on the wire as authority), enqueues the create, and returns the token to the app immediately. The token lives in a client-owned namespace (high bit set) so it can never be confused with a server-minted wire `Handle`; it is a stand-in the app holds until the server's real `Handle` arrives.

```rust
/// Local-only stand-in for a not-yet-known server Handle (D1). Never serialized
/// onto the wire as a handle — the server is authoritative for wire Handles.
pub struct ProvisionalToken(pub u64);  // top bit = 1 marks provisional (client-local)
// Reconciliation maps the local token -> the server-minted wire Handle (Ch.06/D5)
// once the create's reply arrives.
pub struct HandleMap { table: HashMap<ProvisionalToken, Handle> }
```

How later commands that reference a not-yet-reconciled object stay off the critical path without the client inventing a wire handle: the create and the commands that use its result are carried by the *same in-order session stream*, so the server replays the create (minting the real `Handle`) before it reaches any dependent command. Within one batch the client encodes a dependent reference as a **back-reference to the creating command's `seq`** (an "object produced by `seq` N" token, resolved server-side), not as a fabricated handle value; the server, replaying in order, already holds the real `Handle` for that `seq` and substitutes it deterministically. The app only ever sees the local provisional token; the wire only ever carries server-authoritative handles or `seq` back-references. The create's reply (matched by `req_id`, Ch.06) is needed only when the *app* inspects a value that depends on driver state — e.g. `glGetShaderiv(COMPILE_STATUS)`, `vkCreate*` returning `VK_ERROR_*`. Those are `Sync`, and they also deliver the real `Handle` that reconciles the local token.

Failure handling: if a deferred create fails on the server (out of memory, validation reject), the server records the error against the creating `seq` and surfaces it at the next `Sync`/error-query touching that object (correlated back to the client's provisional token), mirroring how GL defers errors to `glGetError`. This preserves the API contract that most creates "can't fail synchronously" in the remoted view.

## 13.9 Stall avoidance and latency hiding

The remaining tools target the pathological cases:

1. **Watermark piggybacking** (13.4) keeps client beliefs fresh for free.
2. **Heartbeat watermark push**: if no reply has flowed for `T_idle` (default 1 ms) while batches are in flight, the server proactively pushes a `WatermarkUpdate{stream, completed_seq}`. This rescues poll loops on otherwise-async workloads.
3. **Speculative flush on detected wait pattern**: the client tracks a small histogram of "fence created then immediately polled". When it detects a tight poll loop forming, it pre-flushes so the guarded work reaches the server sooner, shrinking the eventual blocking wait.
4. **Pipeline depth cap / backpressure**: the client caps outstanding un-acked bytes (the Transport chapter (Ch.08) credit scheme). When the cap is hit, the next async call blocks — this is intentional backpressure, the only place an `Async` call may stall, and it bounds memory and matches the GPU's real throughput. Without it a fast producer would balloon the ring.
5. **Present pacing**: at `SwapBuffers`/`vkQueuePresent`, GraftX may hold the call until the previous present's completion notification arrives (configurable 1-2 frames deep), giving frame-rate stability instead of letting the guest race arbitrarily ahead of the host compositor.

Decision summary for "do I round-trip?":

```
on API call:
  classify(opcode):
    Async        -> encode into batch; maybe flush(triggers); return
    DeferredReply-> mint provisional; encode; return provisional
    Sync         -> flush(batch); block on reply or notification; return real value
```

## 13.10 Tradeoffs and open questions

- **Coalescing aggressiveness vs. fidelity**: draw-merge and state-collapse risk diverging from apps that depend on exact command counts (timing, GPU profilers). Default conservative; expose a per-session "fidelity" knob (the Build/dist chapter (Ch.28) config).
- **`seq` back-reference determinism**: resolving a dependent reference to the creating command's `seq` (13.8) requires strict in-order replay within a stream so the real `Handle` exists before any dependent command; any reordering breaks it. This forbids server-side out-of-order replay within a stream — an accepted constraint, since GPU streams are in-order anyway. The client still never fabricates a wire handle (D1); it only references a `seq` the server already owns.
- **Notification storm**: per-fence callbacks could flood the control plane for apps creating thousands of fences. Mitigation: coalesce notifications by stream (one `WatermarkUpdate` covers all fences below it) rather than per-object signals.
- **Timeline semaphore host signal/wait** introduces true bidirectional sync that can deadlock if the app waits host-side on a value only a future submit will signal; GraftX must forward such waits with the app's timeout and never invent its own blocking, deferring deadlock semantics to the native driver.

Interactions: ordering ties into the protocol opcode descriptors and `req_id` correlation (the Protocol chapter (Ch.06)) and the transport ring/credits/framing (the Transport chapter (Ch.08)); validation of untrusted handles and timeline values is the Server core chapter (Ch.10) decode/validate path under the Security chapter (Ch.23) trust model; per-API stream mapping detail lives in the API-specific chapters; tuning knobs surface in configuration (the Build/dist chapter (Ch.28)).
