# 10. Server Core: Decode, Validate, Replay

The server's central loop: how it accepts a session, decodes wire opcodes into typed commands, validates each command against an untrusted-stream threat model, replays it against a native driver, and ships results back — with the threading, ordering, and concurrency rules that make this correct and fast.

This chapter describes `graftx-server`'s runtime heart. It assumes the wire format from the Protocol chapter (Ch.06), the `Transport` trait and dual-plane (vsock control + ivshmem bulk) model from the Transport chapter (Ch.08), and the per-API backend dispatch tables from the Vulkan through Video chapters (Ch.14–Ch.20). Sandbox/process model is the Security chapter (Ch.23); resource quotas and backpressure are the Memory chapter (Ch.12) and the Performance chapter (Ch.25). Here we cover only the decode→validate→replay pipeline and its session machinery. Everything below is forward-looking design at v0.0.0.

## 10.1 Responsibilities and non-goals

The server core is the single owner of three invariants:

1. **No native call without a prior validated decode.** A raw byte never reaches a driver entry point; only a typed, range-checked `Command` does. This is the security boundary (§10.6).
2. **Per-session ordering of effects.** Commands within one session that have data dependencies (e.g. `glBindTexture` then `glTexImage2D`) replay in submission order. Cross-session calls are isolated by separate GPU contexts.
3. **Reply correlation.** Every command that expects a reply gets exactly one reply carrying the originating sequence number, or one error frame.

Non-goals for the core: it does not parse the wire bit-layout (that is the protocol decoder, the Protocol chapter (Ch.06), with serialization owned by the Serialization chapter (Ch.07)), does not know GL/Vulkan/CUDA semantics (delegated to backends), and does not own the ivshmem ring arithmetic (the Transport chapter (Ch.08)). It orchestrates.

## 10.2 Process and thread model

GraftX runs **one server process per host**, accepting many sessions. The proposed threading topology:

```text
                 ┌──────────────────────────────────────────────┐
                 │  graftx-server process (sandboxed, Ch.23)     │
                 │                                                │
  vsock listen ──┤  [Acceptor thread]                            │
                 │      │ spawn on connect                        │
                 │      ▼                                         │
                 │  ┌─────────────── Session worker ───────────┐  │
                 │  │ [Reader] decode frames → cmd queue        │  │
                 │  │      │                                     │  │
                 │  │      ▼                                     │  │
                 │  │ [Replay] validate → dispatch → driver     │  │
                 │  │      │                                     │  │
                 │  │      ▼                                     │  │
                 │  │ [Writer] encode replies → transport       │  │
                 │  └───────────────────────────────────────────┘ │
                 │   (one Session worker group per connection)     │
                 └──────────────────────────────────────────────┘
```

**Decision: thread-per-session, not a global async runtime.** Native GPU drivers are thread-affine and frequently blocking: a GL context is bound to one OS thread (`MakeCurrent`), and many driver calls block on the GPU. An `async` model would force `spawn_blocking` around nearly every call, giving us the cost of async with none of the benefit. A dedicated OS thread per session lets us call `MakeCurrent` once and keep the context resident. Sessions are coarse-grained (tens, not thousands), so thread count is bounded.

Within a session we further split Reader / Replay / Writer when profiling shows the decode or encode cost is non-trivial; the **default** is a single fused loop (`reader == replayer == writer`) because most commands are tiny and a split adds queue-handoff latency. The split is a tuning knob, not a contract:

```rust
/// One of these per connection. Owns the GPU contexts for the session.
pub struct SessionWorker {
    id: SessionId,
    transport: Box<dyn Transport>,   // control plane (Ch.08)
    bulk: BulkReader,                // ivshmem view (Ch.08)
    state: SessionState,             // §10.4
    backends: BackendRegistry,       // API -> dispatch table (Ch.14–Ch.20)
    limits: SessionLimits,           // Ch.12 / Ch.25
}
```

**Fairness across sessions** is the OS scheduler's job, with one refinement: each `SessionWorker` honors a cooperative yield point and a per-session in-flight budget (the Performance chapter (Ch.25)) so one chatty guest cannot monopolize a shared driver lock.

## 10.3 The replay loop

The fused loop, in skeleton form:

```rust
impl SessionWorker {
    pub fn run(mut self) -> Result<(), ServerError> {
        self.handshake()?;                       // Hello/Welcome negotiate (Ch.06)
        loop {
            // 1. Read one framed control message (header + inline body or
            //    a bulk-plane descriptor). Blocks; returns Eof on clean close.
            let frame = match self.transport.recv_frame() {
                Ok(f) => f,
                Err(TransportError::Eof) => break,
                Err(e) => return Err(ServerError::Transport(e)),
            };

            // 2. Decode wire bytes -> typed Command (no driver touch).
            let decoded = self.decode(&frame)?;  // Protocol decoder (Ch.06)

            // 3. Validate against the untrusted-stream policy (§10.6).
            let cmd = match self.validate(decoded) {
                Ok(c) => c,
                Err(v) => { self.reply_error(frame.seq, v)?; continue; }
            };

            // 4. Replay against the native driver via the backend table.
            let outcome = self.dispatch(cmd);

            // 5. Reply path (§10.7): result, error, or nothing (fire-and-forget).
            self.respond(frame.seq, outcome)?;
        }
        self.teardown()        // destroy GPU contexts, free arenas
    }
}
```

The five numbered steps are the chapter's spine; §10.4–§10.7 expand them.

### 10.3.1 Batched submission

A single `recv_frame` may carry a **command batch** — a run of fire-and-forget calls (common for GL draw setup) coalesced by the client (the Client shim chapter (Ch.09)). The loop decodes and replays the batch as a unit, emitting at most one reply (for the batch's terminal sync point). This is the primary throughput lever; it amortizes transport round-trips.

```rust
match frame.kind {
    FrameKind::Single(_)  => self.replay_one(frame)?,
    FrameKind::Batch(n)   => self.replay_batch(frame, n)?, // n decoded cmds
}
```

Batching interacts with validation: each command in the batch is validated independently, but a validation failure on command *k* **poisons the rest of the batch** — the server stops, replies with an error naming the failing sub-sequence, and the session is marked degraded (the client must resync, §10.5.3). Partial batch replay with a hole would corrupt driver state, so we never do it.

## 10.4 Per-session state

Each session owns a `SessionState` that maps the guest's **handle namespace** (client-side IDs) to **server-side native objects**, plus the live driver contexts.

```rust
pub struct SessionState {
    /// Guest object id -> server resource. One table per object class so a
    /// guest texture id and buffer id can collide without aliasing.
    textures:  HandleTable<NativeTexture>,
    buffers:   HandleTable<NativeBuffer>,
    programs:  HandleTable<NativeProgram>,
    vk_objects: HandleTable<VkHandle>,     // Vulkan dispatchable/non-disp.
    cuda_mem:  HandleTable<CudaPtr>,
    // ... one table per API object class (Ch.14–Ch.20 populate these)
    contexts:  ContextSet,                 // GL/EGL/VK contexts, current binding
    bulk_arena: ServerArena,               // private copy target (§10.6.3)
    seq_high_water: u64,                   // last accepted sequence
}

/// Generational handle table: detects use-after-free of stale guest ids.
pub struct HandleTable<T> {
    slots: Vec<Slot<T>>,                   // index = guest id (sparse, capped)
    free:  Vec<u32>,
}
struct Slot<T> { gen: u32, value: Option<T> }
```

**Why per-class tables and not one map.** GL reuses small integer names per object class; collapsing them into one map would force a tagged key and lose the natural array indexing that makes lookup O(1) with good cache behavior. The generation counter turns a freed-then-reused guest id into a detectable mismatch rather than silent aliasing — important because the stream is untrusted.

**Handle translation is the core's job, validation included.** The canonical object model and the 64-bit wire `Handle` layout (kind / generation / slot) are owned by the Handles chapter (Ch.11); `SessionState` here is the server-side resolution of those server-minted handles. When a command references guest id `g`, the core resolves `g → native` *before* calling the backend; an unresolvable id is a validation error, never a driver call with a dangling pointer. Backends receive already-resolved native handles:

```rust
fn dispatch(&mut self, cmd: Command) -> Outcome {
    match cmd {
        Command::Gl(GlCmd::BindTexture { target, tex }) => {
            // tex was already resolved to a NativeTexture during validate().
            self.backends.gl.bind_texture(&mut self.contexts, target, tex)
        }
        // ...
    }
}
```

### 10.4.1 Context currency

GL/EGL semantics require a *current* context per thread. Because each session is a thread, the worker sets its context current at session start and only switches when the guest issues `MakeCurrent`. The `ContextSet` tracks the current binding so the core can reject calls that arrive with no current context (a guest bug or attack) instead of letting the driver fault.

## 10.5 Ordering, concurrency, and isolation

### 10.5.1 Intra-session ordering

The wire is a single ordered stream per session; the loop replays in receive order, so program order is preserved by construction for the common case. Two subtleties:

- **Fire-and-forget vs. sync.** Most GL calls expect no reply and are replayed without waiting on the GPU. A `glFinish`/`vkQueueWaitIdle`/`cuStreamSynchronize` is a barrier: the core must drain the driver before replying, and must not reorder around it.
- **Async backends.** CUDA streams and Vulkan queues are inherently asynchronous on the driver side. The core does **not** try to track GPU-side completion; it preserves *submission* order, which is what the guest's own stream/queue semantics demand. GPU-side ordering is the guest's responsibility expressed through stream/queue handles, which we faithfully forward.

### 10.5.2 Cross-session isolation

Sessions never share GPU objects. Each gets its own GL/Vulkan/CUDA context, so a handle from session A is meaningless in session B's tables. Where the native driver serializes globally (some GL drivers hold a global lock), sessions contend at the driver layer; the core adds no extra global lock of its own beyond what backends declare. A backend that is not thread-safe registers a `BackendLock` the core acquires around its calls:

```rust
pub enum Concurrency { ThreadSafe, GlobalLock, PerContext }
trait Backend { fn concurrency(&self) -> Concurrency; }
```

| Backend class      | Declared concurrency | Core behavior                      |
|--------------------|----------------------|------------------------------------|
| Vulkan             | `PerContext`         | no extra lock; per-VkDevice safe   |
| Modern GL (WGL)    | `GlobalLock`         | serialize across sessions          |
| CUDA driver API    | `ThreadSafe`         | no extra lock                      |
| Legacy/vendor ext  | `GlobalLock`         | conservative serialize             |

### 10.5.3 Session degradation and resync

If validation fails mid-stream, or a driver call returns an unrecoverable error, the session enters `Degraded`. In `Degraded` the core drops further commands (replying with a sticky error) until it sees a `Resync` control frame, which resets cursors but **not** object tables (the guest re-establishes only what it lost). A second fatal error escalates to `Closing` and teardown. This avoids the alternative — tearing the whole session on the first bad frame — which would make the protocol brittle against benign client desync.

## 10.6 The validate-before-replay boundary

This is the security heart (threat model in the Security chapter (Ch.23); the server replays an **untrusted** stream against native drivers).

### 10.6.1 What "validate" means

`validate()` transforms a *decoded-but-untrusted* command into a *replay-safe* command, or rejects it. Checks, in order of cost:

1. **Structural:** opcode known for the negotiated version; argument count/enums in range; lengths non-negative and within `SessionLimits`.
2. **Handle resolution:** every guest id resolves to a live native object of the right class (generation match). Failure ⇒ reject; never pass a stale pointer to a driver.
3. **Semantic range:** sizes, offsets, and counts checked against the resolved object (e.g. a `glTexSubImage2D` rectangle must lie inside the bound texture's dimensions; a Vulkan descriptor write must target an allocated set).
4. **Cross-argument coherence:** e.g. buffer offset + size must not overflow `usize` and must fit the buffer's capacity.

```rust
fn validate(&mut self, d: Decoded) -> Result<Command, Violation> {
    self.check_opcode_version(&d)?;            // 1
    let cmd = self.resolve_handles(d)?;        // 2  -> typed Command
    self.check_semantic_ranges(&cmd)?;         // 3 + 4
    Ok(cmd)
}
```

### 10.6.2 Decode/validate separation

Decoding (the Protocol chapter (Ch.06)) is total and pure: bytes in, `Decoded` out or a framing error. It performs **no** cross-reference to session state. Validation is the only place that consults `SessionState`. Keeping them separate means the decoder is fuzzable in isolation and the validation policy is auditable in one module. The boundary is enforced by types: `dispatch()` accepts only `Command` (post-validate), and `Command` cannot be constructed except by `validate()` (private constructor / sealed module).

### 10.6.3 The bulk-plane copy rule

For large payloads (texture uploads, vertex data) the client places bytes in ivshmem and sends only a descriptor on the control plane. The security model is explicit: **the server copies validated bulk data into server-private memory before use.** Write-revocation cannot be enforced at this layer (only the hypervisor/ivshmem device can), so the core treats the shared region as volatile and hostile.

```rust
fn ingest_bulk(&mut self, desc: BulkDesc) -> Result<&[u8], Violation> {
    desc.validate_against(&self.limits)?;            // bounds vs ring + quota
    // Copy out atomically: re-reading shared memory after a bounds check is a
    // TOCTOU; we copy once, then validate the *copy*.
    let dst = self.bulk_arena.alloc(desc.len)?;       // server-private
    self.bulk.copy_into(desc, dst)?;                  // single read pass
    Ok(dst)                                           // backend sees only this
}
```

The single-read-then-validate-the-copy pattern closes the time-of-check/time-of-use gap: a malicious guest mutating shared memory after our bounds check cannot affect the bytes the driver actually consumes, because the driver only ever sees `dst`.

## 10.7 Reply path

Three response shapes, encoded back onto the control plane (small results inline; large results via a server-allocated bulk region the client reads):

```rust
enum Outcome {
    None,                          // fire-and-forget, no reply frame
    Value(ReplyPayload),           // inline scalar / small struct
    Bulk(BulkHandle, ReplyMeta),   // large readback (e.g. glReadPixels)
    DriverError(NativeError),      // driver rejected the (valid) call
}

fn respond(&mut self, seq: u64, o: Outcome) -> Result<(), ServerError> {
    match o {
        Outcome::None => Ok(()),
        Outcome::Value(p)   => self.transport.send_reply(seq, p),
        Outcome::Bulk(h, m) => self.transport.send_bulk_reply(seq, h, m),
        Outcome::DriverError(e) => self.reply_error(seq, Violation::Driver(e)),
    }
}
```

**Correlation** uses the originating `seq`. Because the loop is in-order and synchronous per session, replies are naturally ordered; the client matches by sequence regardless. **Error semantics distinguish two classes:** a *validation* error (the stream was malformed/hostile) versus a *driver* error (a well-formed call the GPU rejected, e.g. out-of-memory). Both travel as error frames but carry different category codes so the client shim can forward a driver error to the application (as the real API would) while a validation error indicates a GraftX-internal protocol fault.

### 10.7.1 Readback and the reverse bulk path

For `glReadPixels`, `vkMapMemory` reads, or `cuMemcpyDtoH`, the result is large. The core writes it into a server-owned ivshmem region and replies with a `BulkHandle`; the client reads, then ACKs to release the region (the Transport chapter (Ch.08) owns the lifecycle). The core caps outstanding reverse-bulk regions per session (the Memory chapter (Ch.12)) so a slow client cannot pin unbounded shared memory.

## 10.8 Error handling and the no-unwrap rule

Per project policy, library paths use `Result` with `thiserror`; no `unwrap` in the loop. The top-level taxonomy:

```rust
#[derive(thiserror::Error, Debug)]
pub enum ServerError {
    #[error("transport: {0}")] Transport(#[from] TransportError),
    #[error("decode: {0}")]    Decode(#[from] DecodeError),
    #[error("fatal session error")] Fatal,
}
```

A `Violation` (validation/driver) is **recoverable**: it produces an error reply and the loop continues (or degrades). A `ServerError` is **fatal to the session**: the loop returns, teardown runs, GPU contexts are destroyed, arenas freed, and the acceptor reaps the thread.

**Two distinct fault classes, two distinct mechanisms (do not conflate them):**

1. **A Rust panic in our own glue** (a bug in decode/validate/marshalling, an `expect` we missed) unwinds. We catch it at the worker boundary with `catch_unwind` around `dispatch` and convert it to `ServerError::Fatal`, draining that one session without unwinding through the acceptor. `catch_unwind` catches **only** Rust panics in our glue — it cannot and does not catch a hardware fault inside the native driver.
2. **A native driver hardware fault** (an access violation / segfault raised *inside* the vendor `.dll`/`.so` while replaying a well-formed call) is **not** a Rust panic and is invisible to `catch_unwind`. On the Windows server it is intercepted by SEH (`__try`/`__except`) or a vectored exception handler installed at the FFI seam (per D10; agreed with the Security chapter (Ch.23) and the Error/FFI chapter (Ch.24)); `siglongjmp` is explicitly not used. Once a driver faults, its internal state is unknowable, so the safe blast radius is the **whole driver/session**: we surface it as `ServerError::Fatal` and tear the session down (destroy its contexts, free its arenas). We never attempt to resume the faulting call or keep the session alive.

Both mechanisms feed the same outcome — session teardown, never process death — so one session's failure (Rust bug or driver crash) cannot poison the process. This is defense in depth alongside the sandbox (the Security chapter (Ch.23)).
