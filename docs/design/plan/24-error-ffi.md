# 24. Error Handling & FFI Safety

How GraftX will model errors with `thiserror`, translate them into each API's native error code across the C ABI, and keep the unsafe FFI boundary panic-safe, leak-safe, and recoverable.

## 24.1 Scope and threat surface

GraftX straddles three trust domains, and each has a different error
discipline. The *client shim* (a `cdylib` exporting C symbols like
`glGetError`, `vkCreateInstance`, `clEnqueueNDRangeKernel`) is loaded into an
**untrusted-to-us, trusted-by-the-app** address space: the guest application
expects it to behave exactly like the real driver, including the exact error
codes. The *transport* (the Transport chapter (Ch. 08)) and *protocol* (the
Protocol chapter (Ch. 06)) layers are pure
Rust and use idiomatic `Result<T, E>`. The *server* (the Server core chapter
(Ch. 10)) replays a
fully untrusted command stream against real drivers and must convert *every*
failure — validation rejection, driver error, OOM, transport drop — back into
something the client can render as a native code.

The cardinal rule: **a Rust `panic` must never unwind across an `extern "C"`
boundary in either direction.** Doing so is undefined behavior on most ABIs and
will corrupt the guest application or the server process. This chapter defines
how that is structurally prevented and how every other error class is mapped
deterministically. It does not re-derive the wire encoding (the Protocol
chapter (Ch. 06)) nor the per-API command catalogs (the Vulkan through Level
Zero chapters (Ch. 14–19)); it consumes them.

```text
 guest app ──C ABI──> client shim ──Result──> transport ══vsock/ivshmem══>
   ^                      |                                                |
   | native code         | catch_unwind                                   v
   +──────────────────────+                                          server replay
                          translate(Rust err) -> GLenum/VkResult/...   |  validate
                                                                       |  native call
                                                                       v
                                                              ProtoError on wire
```

## 24.2 The error model: a layered `thiserror` hierarchy

GraftX will define **one error enum per crate layer**, never a single
god-enum. Lower layers convert *up* via `#[from]`; the FFI edge converts *out*
to native codes. Per the workspace rules: `thiserror` for all library errors,
no `unwrap`/`expect` in library paths, every variant carries enough context to
diagnose without a debugger.

```rust
// graftx-transport
#[derive(thiserror::Error, Debug)]
pub enum TransportError {
    #[error("vsock connect failed (cid={cid}, port={port})")]
    Connect { cid: u32, port: u32, #[source] io: std::io::Error },
    #[error("peer closed channel mid-message ({read} of {expected} bytes)")]
    ShortRead { read: usize, expected: usize },
    #[error("ivshmem region too small: need {need}, have {have}")]
    ShmTooSmall { need: usize, have: usize },
    #[error("ring buffer producer stalled past deadline ({0:?})")]
    Backpressure(std::time::Duration),
    #[error("channel poisoned by prior fatal error")]
    Poisoned,
}

// graftx-protocol
#[derive(thiserror::Error, Debug)]
pub enum ProtoError {
    #[error("unknown opcode {0:#06x}")]
    UnknownOpcode(u16),
    #[error("frame length {len} exceeds cap {cap}")]
    Oversize { len: u32, cap: u32 },
    #[error("malformed argument #{index} for {op}: {reason}")]
    BadArg { op: &'static str, index: u8, reason: &'static str },
    #[error(transparent)]
    Transport(#[from] TransportError),
}
```

The client adds a *boundary* enum that is the single type every FFI shim
collapses into. It is deliberately small and `#[non_exhaustive]` so new
variants do not break match arms in the per-API translators:

```rust
// graftx-client
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum ClientError {
    #[error("null pointer passed for required argument `{0}`")]
    NullArg(&'static str),
    #[error("invalid handle {handle:#x} for {kind}")]
    BadHandle { kind: &'static str, handle: u64 },
    #[error("argument out of range: {0}")]
    OutOfRange(&'static str),
    #[error("remote rejected command: {0}")]
    Remote(RemoteStatus),          // server-side validation/driver failure
    #[error(transparent)]
    Proto(#[from] ProtoError),     // includes Transport via #[from]
    #[error("server panic recovered at op {0}")]
    ServerFault(&'static str),
    #[error("local invariant violated (bug): {0}")]
    Internal(&'static str),
}
```

`RemoteStatus` is the *wire-encoded* outcome the server returns (§24.5). Note
the chain: a `TransportError::ShortRead` becomes `ProtoError::Transport`
becomes `ClientError::Proto`, and the `Display` impl yields a full causal
string for logging — while the *FFI translation* discards the string and emits
only a native code.

## 24.3 Mapping Rust errors to native API codes

Each remoted API has its own error vocabulary and its own *delivery
mechanism*. The shim must reproduce both. We classify APIs into four delivery
styles:

| Delivery style | APIs | How the app reads errors |
|---|---|---|
| Sticky global flag | OpenGL, GLES | `glGetError()` returns + clears a per-context flag |
| Return-value enum | Vulkan, OpenCL, Level Zero, OptiX | function returns `VkResult`/`cl_int`/`ze_result_t` |
| Out-param + null return | EGL, GLX, CUDA driver | `eglGetError()`, return handle is null/0 |
| Status struct/last-error | AMF, WebGPU, video codecs | `AMF_RESULT`, `WGPUErrorType` via callback |

A central trait converts a `ClientError` into the native code for whichever
API the calling shim belongs to. The translator is a `const`/`match` table —
no allocation, no panic, callable from any thread:

```rust
pub trait NativeCode {
    type Code: Copy;
    fn success() -> Self::Code;
    fn translate(err: &ClientError) -> Self::Code;
}

pub struct GlApi;
impl NativeCode for GlApi {
    type Code = u32; // GLenum
    fn success() -> u32 { 0 } // GL_NO_ERROR
    fn translate(err: &ClientError) -> u32 {
        match err {
            ClientError::NullArg(_)
            | ClientError::BadHandle { .. } => 0x0501, // GL_INVALID_VALUE
            ClientError::OutOfRange(_)      => 0x0501,
            ClientError::Remote(RemoteStatus::OutOfMemory) => 0x0505, // GL_OUT_OF_MEMORY
            ClientError::Remote(RemoteStatus::InvalidEnum) => 0x0500, // GL_INVALID_ENUM
            ClientError::Remote(RemoteStatus::InvalidOp)   => 0x0502, // GL_INVALID_OPERATION
            // transport/server faults have no GL equivalent: surface as
            // the closest "device lost"-like code the app can survive.
            ClientError::Proto(_)
            | ClientError::ServerFault(_)
            | ClientError::Internal(_)
            | ClientError::Remote(_) => 0x0505, // GL_OUT_OF_MEMORY (recoverable-ish)
        }
    }
}

pub struct VkApi;
impl NativeCode for VkApi {
    type Code = i32; // VkResult
    fn success() -> i32 { 0 } // VK_SUCCESS
    fn translate(err: &ClientError) -> i32 {
        match err {
            ClientError::NullArg(_)        => -7,  // VK_ERROR_INITIALIZATION_FAILED
            ClientError::BadHandle { .. }  => -7,
            ClientError::OutOfRange(_)     => -7,
            ClientError::Remote(RemoteStatus::OutOfMemory) => -2, // _OUT_OF_DEVICE_MEMORY
            ClientError::Remote(RemoteStatus::DeviceLost)  => -4, // VK_ERROR_DEVICE_LOST
            // Connection loss maps to DEVICE_LOST: well-behaved Vulkan apps
            // already have a recreation path for it.
            ClientError::Proto(_) | ClientError::ServerFault(_) => -4,
            _ => -7,
        }
    }
}
```

**Design tradeoff — transport faults have no native analog.** When the vsock
channel drops or the server crashes, there is no "remoting failed" code in the
GL or Vulkan vocabulary. We deliberately map these to the *most recoverable*
native code each API offers (`VK_ERROR_DEVICE_LOST`, `GL_OUT_OF_MEMORY`,
`CL_OUT_OF_RESOURCES`) so that apps with existing reset logic recover instead
of corrupting. The true cause is always emitted to the GraftX log via the
`Display` chain before translation, so debuggability is not lost.

### 24.3.1 Sticky-flag emulation for OpenGL

OpenGL has no return value; the app polls `glGetError`. The shim therefore
keeps a per-context sticky error cell. GL semantics require that the *first*
error since the last `glGetError` wins and is not overwritten:

```rust
thread_local! {
    static GL_ERR: Cell<u32> = const { Cell::new(0) }; // GL_NO_ERROR
}
fn gl_set_error(code: u32) {
    GL_ERR.with(|c| if c.get() == 0 { c.set(code); }); // first-wins
}
#[no_mangle]
pub extern "C" fn glGetError() -> u32 {
    GL_ERR.with(|c| c.replace(0))
}
```

Per the spec the error is per *context*, not per thread, but because a GL
context is current to at most one thread at a time, thread-local storage is a
correct and lock-free approximation. When the context-thread binding changes
(`glXMakeCurrent`/`eglMakeCurrent`) the shim migrates the cell into the
context object so a context made current on another thread sees its own
pending error.

## 24.4 Panic safety at the boundary

Two complementary mechanisms guarantee no unwind crosses C. Crucially, the
client and the server are built by **separate `cargo` invocations** with
different panic strategies — we do *not* use an illegal
`[profile.*.package.*]` panic override to mix strategies in one build
(see the Build/dist chapter (Ch. 28) and the Versioning chapter (Ch. 29)):

1. **Client cdylib built `panic = "abort"`.** The client shim is compiled in
   its own invocation with `-C panic=abort`. With aborting panics there is *no*
   unwinding at all, so a panic can never reach the C frame — it terminates the
   process instead. This is the strongest guarantee but the bluntest: a single
   bug kills the guest app. Because nothing unwinds, locks can never be
   poisoned, so the client never has to reason about `PoisonError`.
2. **Server bin built with default unwind + `catch_unwind` wrappers.** The
   server is compiled in a separate invocation that keeps unwinding, because we
   want graceful per-session teardown rather than process death. Debug and
   fuzzing builds likewise keep unwinding. Every exported / replayed entry
   routes through one macro so the pattern cannot be forgotten. The unwinding
   server uses `parking_lot` mutexes throughout, which do not poison on panic,
   so a recovered panic in one session never bricks a lock shared with another.

```rust
/// Wrap a shim body. Returns `on_panic` if the closure panics or returns Err,
/// the translated native code otherwise. Never unwinds.
fn ffi_guard<A, F>(op: &'static str, f: F) -> A::Code
where
    A: NativeCode,
    F: FnOnce() -> Result<(), ClientError> + std::panic::UnwindSafe,
{
    match std::panic::catch_unwind(f) {
        Ok(Ok(()))   => A::success(),
        Ok(Err(e))   => { log::warn!("{op}: {e}"); A::translate(&e) }
        Err(payload) => {
            // payload is the panic value; log then synthesize an error.
            let msg = panic_msg(&payload);
            log::error!("{op}: PANIC in shim: {msg}");
            A::translate(&ClientError::Internal("shim panic"))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn vkCreateBuffer(
    device: VkDevice, info: *const VkBufferCreateInfo,
    alloc: *const VkAllocationCallbacks, out: *mut VkBuffer,
) -> i32 {
    ffi_guard::<VkApi, _>("vkCreateBuffer", || {
        let info = nn_ref("info", &info)?;            // §24.6 null check
        let dev  = handles().resolve_device(device)?; // §24.6 handle check
        let h = remote::create_buffer(dev, info)?;    // -> Result<.., ClientError>
        // SAFETY: `out` validated non-null by nn_mut; writing one VkBuffer.
        *nn_mut("out", &out)? = h;
        Ok(())
    })
}
```

A subtlety: `catch_unwind` requires the closure be `UnwindSafe`. Raw pointers
and `&mut` captured from FFI are not automatically `UnwindSafe`; the shim wraps
them in `AssertUnwindSafe` *after* the null/handle validation has run, because
the validation is what makes "the pointer was bad and we panicked midway"
non-observable to the caller — we never partially write the out-param before
the fallible work completes.

**Tradeoff matrix:**

| Build (own `cargo` invocation) | `panic` | Boundary guard | Rationale |
|---|---|---|---|
| client cdylib (release) | abort | thin wrapper (logs, no catch) | Smallest code; bug in shim is a bug, fail fast; poison-free locks |
| server bin (release) | unwind | `catch_unwind` per command + `parking_lot` | One bad command must not kill other sessions; no lock poisoning |
| `dev` / `test` | unwind | `catch_unwind` | Keep stack traces, keep fuzzer alive |
| `fuzz` | unwind | `catch_unwind` + re-arm | Corpus minimization needs survival |

`panic_msg` downcasts the `Box<dyn Any>` to `&str`/`String` only; it never
formats arbitrary types, so it cannot itself panic.

## 24.5 Server-side errors: validation, replay, and the wire status

The server (the Server core chapter (Ch. 10)) is where untrusted commands meet
real drivers. It distinguishes three failure origins and encodes them
uniformly:

```rust
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[repr(u16)]
pub enum RemoteStatus {
    Ok = 0,
    InvalidEnum, InvalidOp, OutOfMemory, DeviceLost,
    Rejected,        // validator refused (security)
    Unsupported,     // op not implemented for this driver
    QuotaExceeded,   // resource-limit / backpressure trip
    DriverError(u32),// opaque native code, passthrough for fidelity
}
```

Replay flow for a single command:

```text
decode frame ─► validate(cmd)            ──reject──► RemoteStatus::Rejected
                  │ ok                                  (log + maybe drop session)
                  ▼
            resolve server-side handles ──miss──► RemoteStatus::Rejected
                  │ ok
                  ▼
            SEH-guarded native call ─AV/panic─► RemoteStatus::DeviceLost
                  │ ok                              (+ poison session, §24.7)
                  ▼
            read native error code
                  │
        ┌─────────┴──────────┐
   native==success      native==error
        │                     │
   RemoteStatus::Ok    map_native(code) -> RemoteStatus::DriverError(code)
```

The server **must defend against hardware faults from native drivers, not just
Rust panics**: an FFI call into a closed-source GPU driver can fault (an access
violation, not a panic). `catch_unwind` catches *only* Rust panics raised in
the Rust glue — it never catches a CPU-level access violation, so it is not the
mechanism here. Because the GraftX server runs on Windows, the FFI seam wraps
every native driver call in **Structured Exception Handling**
(`__try`/`__except`, or equivalently a registered vectored exception handler)
that traps the access violation, converts it to `RemoteStatus::DeviceLost`, and
poisons the session. We deliberately do *not* use `siglongjmp`/POSIX signal
recovery. After a driver access violation the safe blast radius is the **whole
driver/session** — the driver's internal state is now unknowable — so recovery
always tears the session down rather than resuming the faulting call (this
agrees with the Security chapter (Ch. 23) and the recovery contract in §24.7).
The validator running *before* the native call is the primary defense; the SEH
seam is the backstop. The exception filter only records the fault and unwinds to
the per-call recovery point; it performs no driver work, so it cannot itself
re-fault into an infinite loop.

`DriverError(code)` preserves the exact native code so the client can pass it
straight through (highest API fidelity, the top priority) for the
return-value-style APIs. For GL it is funneled through `map_native` into a
`RemoteStatus` variant because the GL flag set is closed.

## 24.6 Null and handle validation helpers

All pointer and handle validation lives in two tiny `#[inline]` helpers so the
`// SAFETY:` reasoning is written once, audited once, and every shim reuses it:

The lifetime `'a` of the returned reference is **bound to a borrow of the
pointer argument** rather than left unconstrained. An unbounded `'a` (chosen
freely by the caller) would let the reference outlive the call and is unsound;
tying `'a` to `&'a *const T` forces every borrow to expire when the shim
returns, which is exactly the C ABI guarantee window.

```rust
/// SAFETY PRECONDITION (caller / the shim): if `*p` is non-null it points to a
/// valid, initialized `T` that stays valid for the whole call, AND no other
/// live reference aliases `*p` for the duration of the returned borrow. The C
/// ABI contract of the wrapped function provides both. We only convert
/// null -> typed error.
#[inline]
fn nn_ref<'a, T>(name: &'static str, p: &'a *const T) -> Result<&'a T, ClientError> {
    if (*p).is_null() { Err(ClientError::NullArg(name)) }
    else { Ok(unsafe { &**p }) }
}

/// SAFETY PRECONDITION: as `nn_ref`, plus the returned `&mut` is *exclusive* —
/// the caller must prove `*p` is not aliased by any other reference (including
/// another `nn_ref`/`nn_mut` over an overlapping out-param) for `'a`.
#[inline]
fn nn_mut<'a, T>(name: &'static str, p: &'a *mut T) -> Result<&'a mut T, ClientError> {
    if (*p).is_null() { Err(ClientError::NullArg(name)) }
    else { Ok(unsafe { &mut **p }) }
}
```

Handles (e.g. `VkDevice`, `cl_mem`, `CUcontext`) are opaque integers the app
got from a *previous* shim call. The client keeps a side table mapping the
client-visible handle to its remote counterpart plus type tag, so a forged or
stale handle is caught locally before any wire traffic:

```rust
struct HandleTable {
    map: RwLock<HashMap<u64, HandleEntry>>, // client_handle -> entry
}
struct HandleEntry { remote: u64, kind: &'static str, gen: u32 }

impl HandleTable {
    fn resolve_device(&self, h: VkDevice) -> Result<RemoteHandle, ClientError> {
        let key = h as u64;
        self.map.read().get(&key)
            .filter(|e| e.kind == "VkDevice")
            .map(|e| RemoteHandle(e.remote))
            .ok_or(ClientError::BadHandle { kind: "VkDevice", handle: key })
    }
}
```

The `gen` (generation) field defeats use-after-free: when a handle is
destroyed the entry is removed, so a later call with the same integer (which a
driver might recycle) fails the lookup rather than aliasing a new object.
Validation order inside every shim is fixed: **null args → handle resolution →
range/enum checks → remote call**, so the cheapest, most local rejections
happen first and never touch the transport.

## 24.7 Poisoning and recovery

A *fatal* error makes further use of a resource unsound; GraftX poisons at the
narrowest scope that restores soundness:

- **Channel poison.** A `TransportError` that means the byte stream is
  desynchronized (short read mid-frame, framing checksum mismatch) sets a
  `poisoned: AtomicBool` on the channel. Every subsequent send/recv returns
  `TransportError::Poisoned` immediately instead of reading garbage. Poison is
  *not* cleared automatically; the session must be torn down and rebuilt
  (the Client shim chapter (Ch. 09) reconnect logic).
- **Session poison (server).** A recovered native fault or a validator
  rejection that indicates a hostile stream marks the session dead. In-flight
  commands drain to `RemoteStatus::DeviceLost`; the session's GPU resources are
  released by the server's RAII guards (object lifetimes per the Handles
  chapter (Ch. 11)).
- **Lock poison.** GraftX never has to propagate `std::sync::Mutex` poison: the
  client cdylib is built `panic = "abort"`, so a panic terminates rather than
  unwinds and no lock can ever be poisoned; the unwinding server uses
  `parking_lot` mutexes, which do not poison on panic (per D11). The only place
  `PoisonError` is even representable is an unwinding `std::sync::Mutex` in a
  test build, where it is treated as `ClientError::Internal`.

Recovery contract by API: a single non-fatal error (`InvalidEnum`, bad arg)
leaves the context fully usable — the app just sees the native code and
continues. A fatal error transitions the whole API object (context/device) to
a "lost" state; subsequent calls return the lost code (`VK_ERROR_DEVICE_LOST`,
`CL_INVALID_CONTEXT`) until the app destroys and recreates it, at which point a
fresh transport session is opened transparently.

```text
              non-fatal err            fatal err / channel poison
  Healthy ───────────────────► Healthy ──────────────────────► Lost
     ▲   (native code returned)                                  │
     │                                                            │
     └────────────── recreate device/context (new session) ◄─────┘
```

## 24.8 Testing the error paths

The plan mandates dedicated coverage that production code rarely exercises:
(1) a `proptest` corpus of malformed frames asserting every one decodes to a
`ProtoError` and never panics; (2) a fault-injection `Transport` impl that can
return `ShortRead`/`Poisoned` at any offset, asserting clients surface
`DEVICE_LOST`-class codes and remain unwound; (3) a shim fuzz target that calls
exported symbols with null pointers, recycled handles, and out-of-range enums,
asserting `ffi_guard` always returns a valid native code and the process
survives under the unwinding profile; (4) a golden table test pinning every
`(ClientError, Api) -> native_code` pair so an accidental remap is caught in
CI. Together these make the error surface — the part most likely to be wrong
under real GPU drivers — the most heavily tested code in GraftX.
