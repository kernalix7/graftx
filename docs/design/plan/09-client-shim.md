# 09. Client Shim: Interception & Dispatch

How GraftX's `graftx-client` cdylib will impersonate each native GPU library, capture entrypoint calls, route them to the remoting backend, and transparently fall back to the real driver when needed.

The client shim is the guest-side front door. It must look bit-for-bit like the real `libvulkan.so`, `libGL.so`, `libEGL.so`, `libcuda.so`, etc. to the application and to loaders (the Vulkan loader, the GLVND dispatch layer, OpenCL ICD loader, the CUDA driver consumer), while internally turning every exported C function into a serialized command (the Protocol chapter (Ch. 06) wire format) shipped over the transport (the Transport chapter (Ch. 08)). This chapter covers *interception* (how the call reaches our code) and *dispatch* (how we pick the handler and forward it). Marshalling of argument payloads is the Serialization chapter (Ch. 07); handle/object lifetime tracking is the Handles chapter (Ch. 11); the per-API symbol tables are the API chapters (Ch. 14-20).

## 9.1 Interception strategies — one size does not fit all

GPU APIs are discovered by applications through three distinct mechanisms, so GraftX will support three coexisting interception modes selectable per API at build/run time.

| Mode | Used for | How the app reaches us | Pros | Cons |
|------|----------|------------------------|------|------|
| `LD_PRELOAD` / SONAME shadowing | OpenGL/GLX/EGL/GLES, CUDA driver API, OpenCL, generic `dlopen` | App `dlopen`s a SONAME we replace on the search path | Universal, no loader cooperation | Must export *every* symbol; brittle vs. versioned symbols |
| Loader-driver/ICD plug-in | Vulkan ICD, OpenCL ICD, EGL `_egl_External` | Loader reads our JSON manifest and calls one bootstrap symbol | Clean, loader does dispatch, multi-vendor coexistence | Only works where a loader exists; must honor loader ABI |
| Vulkan layer | Vulkan (alternative/augment) | Loader inserts us in the call chain | Selective hooking, chain-through to real ICD | Layer ABI churn; not a remoting primitive on its own |

The decision rule GraftX will follow:

```text
Vulkan      -> ICD plug-in (primary)   + layer (optional debug/passthrough)
OpenCL      -> ICD plug-in (primary)   + LD_PRELOAD (for clGetPlatformIDs probing)
EGL/GLES    -> LD_PRELOAD SONAME shadow (libEGL.so.1, libGLESv2.so.2)
GLX/desktop GL -> GLVND vendor lib OR LD_PRELOAD libGL.so.1
CUDA driver -> LD_PRELOAD libcuda.so.1     (no public ICD)
HIP/L0/etc. -> LD_PRELOAD per SONAME
```

ICD/loader modes are strongly preferred where available because the loader owns trampolining and we only have to answer "which entrypoints do you implement?" — eliminating an entire class of versioned-symbol bugs. `LD_PRELOAD` remains the fallback for APIs with no loader (CUDA driver, raw GLES).

### 9.1.1 SONAME shadowing details

For `LD_PRELOAD` we ship one cdylib per emulated SONAME, each with the correct `DT_SONAME` so a dependent that links against `libcuda.so.1` resolves to us. The build (the Build/dist chapter (Ch. 28)) sets per-target link flags:

```toml
# crates/graftx-client/Cargo.toml (planned)
[lib]
crate-type = ["cdylib", "rlib"]   # rlib for unit tests, cdylib for the shim
```

```text
# planned build.rs / linker args per emulated lib
-Wl,-soname,libvulkan.so.1
-Wl,--version-script=vulkan.map   # control symbol versions (GLIBC-style)
```

A version script lets us export e.g. `glXGetProcAddress@@GLX_1.4` with the exact version tags the original library used; without it, apps using `dlvsym`/versioned references break. The script is generated (the Build/dist chapter (Ch. 28)) from each API's reference `.symbols` dump.

## 9.2 The Vulkan ICD path — the reference design

The Vulkan loader finds ICDs through `/usr/share/vulkan/icd.d/*.json`. GraftX installs:

```json
{ "file_format_version": "1.0.0",
  "ICD": { "library_path": "/opt/graftx/lib/libgraftx_vk_icd.so",
           "api_version": "1.3.280" } }
```

The loader then calls exactly one symbol, and the rest is negotiated:

```rust
/// Loader v2+ entrypoint. SAFETY: called by the trusted Vulkan loader with a
/// valid C string name; we return a fn pointer or null.
#[no_mangle]
pub unsafe extern "C" fn vk_icdGetInstanceProcAddr(
    instance: VkInstance,
    p_name: *const c_char,
) -> PFN_vkVoidFunction {
    // SAFETY: loader guarantees p_name is a valid NUL-terminated string.
    let name = unsafe { CStr::from_ptr(p_name) };
    dispatch::resolve_instance(instance, name.to_bytes())
}

#[no_mangle]
pub unsafe extern "C" fn vk_icdNegotiateLoaderICDInterfaceVersion(
    p_version: *mut u32,
) -> VkResult {
    // SAFETY: loader passes a writable u32 holding its max supported version.
    let loader = unsafe { *p_version };
    let agreed = loader.min(ICD_INTERFACE_VERSION); // we support v5
    unsafe { *p_version = agreed };
    VK_SUCCESS
}
```

`resolve_instance` returns the address of a *trampoline* for each requested name. Device-level functions go through a second tier: `vkGetDeviceProcAddr` returns device trampolines that read a per-device dispatch table so we never pay a global lookup on hot calls like `vkQueueSubmit`.

```text
app ─► loader ─► vk_icdGetInstanceProcAddr("vkCreateDevice")
                      │ returns &trampoline_vkCreateDevice
app ─► trampoline_vkCreateDevice(phys, ci, alloc, out_dev)
                      │ serialize -> transport.send(); recv reply
                      │ install device dispatch table keyed by *out_dev
app ─► vkGetDeviceProcAddr(dev,"vkQueueSubmit") -> &dev_tramp_vkQueueSubmit
app ─► dev_tramp_vkQueueSubmit  ── O(1) table[dev].queue_submit ──► remote
```

## 9.3 Dispatch tables and trampolines

For loader-mediated APIs the loader stores our table; for `LD_PRELOAD` APIs we *are* the symbols, so the dispatch table is internal. GraftX will use a generated table of opcode-bound handlers.

```rust
/// One per API family. Generated from the API symbol table (the API chapters (Ch. 14-20)).
pub struct DispatchTable {
    /// Indexed by the API's compile-time opcode enum -> remoting handler.
    handlers: [Handler; OPCODE_COUNT],
}
type Handler = unsafe extern "C" fn(args: *const Frame) -> isize;
```

Each exported C symbol is a thin shim that (1) reads thread-local session state, (2) appends an opcode + encoded args to the outbound command ring, and (3) either returns immediately (async/void) or blocks for a reply (synchronous, must-return-value calls).

```rust
#[no_mangle]
pub unsafe extern "C" fn glClear(mask: GLbitfield) {
    // Hot path: no allocation, no lock — TLS context already cached.
    let ctx = tls::current_gl();          // &mut ThreadCtx, cheap
    if ctx.passthrough {                  // §9.7 fallback
        return (ctx.real_glClear)(mask);
    }
    ctx.enc.opcode(Op::GlClear).u32(mask).flush_if_full();
    // Fire-and-forget: glClear has no return; reply is batched/elided.
}

#[no_mangle]
pub unsafe extern "C" fn glGetError() -> GLenum {
    let ctx = tls::current_gl();
    if ctx.passthrough { return (ctx.real_glGetError)(); }
    ctx.enc.opcode(Op::GlGetError);
    ctx.flush_and_wait().read_u32()       // synchronous round-trip
}
```

The encoder writes into a per-thread staging buffer that is flushed to the transport ring either when full, on an explicit sync call, or at a frame/flush boundary. Batching is the single biggest performance lever (the Sync chapter (Ch. 13)); the dispatch layer's job is to make batching the default and synchronous round-trips the exception.

### 9.3.1 The classification table

Every entrypoint is tagged at codegen time with a *call class* that determines dispatch behavior:

| Class | Examples | Behavior |
|-------|----------|----------|
| `Void` | `glClear`, `vkCmdDraw` | encode, no reply, batchable |
| `Returns` | `glGetError`, `vkCreateBuffer` | encode, flush, wait for reply |
| `Creates` | `vkCreateDevice`, `clCreateContext` | reply carries the server-minted handle, register in the Handles chapter (Ch. 11) map |
| `Destroys` | `vkDestroyBuffer` | encode, unregister handle locally |
| `MapsMemory` | `glMapBuffer`, `vkMapMemory` | reserve ivshmem region, return guest ptr (the Serialization chapter (Ch. 07) / the Memory chapter (Ch. 12)) |
| `GetProc` | `eglGetProcAddress` | return our trampoline, never the real one |
| `Enumerate` | `vkEnumeratePhysicalDevices` | two-phase count/fill, cached |

The codegen (the Vulkan chapter (Ch. 14)) emits a `const CLASS: [CallClass; OPCODE_COUNT]` array so the runtime never branches on string names.

Handles are server-authoritative: the client never invents a wire handle. A `Creates` call's reply carries the server-minted handle, which the shim registers in the Handles chapter (Ch. 11) map. When a `Creates` call must return synchronously but the reply has not yet arrived (deferred/async dispatch), the shim may hand the app a purely *local* provisional proxy token — never placed on the wire as authority — and reconcile it deterministically with the real server handle once the reply lands.

## 9.4 `GetProcAddress`-family handling

`eglGetProcAddress`, `glXGetProcAddress`, `vkGetInstanceProcAddr`, `clGetExtensionFunctionAddressForPlatform` are the trapdoors that defeat naive SONAME shadowing: even if we export every static symbol, an app that resolves `glDrawArraysInstanced` through `glXGetProcAddress` would get the *real* driver's pointer if we forwarded blindly. GraftX must intercept these and return *our own* trampolines.

```rust
#[no_mangle]
pub unsafe extern "C" fn glXGetProcAddressARB(
    name: *const u8,
) -> Option<unsafe extern "C" fn()> {
    let n = unsafe { CStr::from_ptr(name as *const c_char) }.to_bytes();
    // 1. Known GraftX-handled entrypoint? return our trampoline.
    if let Some(p) = dispatch::gl_trampoline(n) { return Some(p); }
    // 2. Unknown extension: register a generic forwarding trampoline that
    //    serializes "call extension by stable id" (Ch. 07 extension table),
    //    OR return None to signal "unsupported" (conservative default).
    dispatch::register_unknown_gl(n)
}
```

Unknown extension functions are the hard case: we cannot synthesize a typed marshaller for a signature we do not know. The proposed policy is conservative-by-default — return `None` (unsupported) unless the extension is on a server-confirmed allowlist, because returning a non-null pointer the app then calls with arguments we cannot serialize would corrupt the stream. A diagnostic mode will instead route the unknown call straight to passthrough (§9.7) so developers can see what an app actually needs.

## 9.5 Per-thread and per-context state

GPU APIs are stateful and thread-affine. GLX/EGL bind a "current context" per thread; Vulkan command buffers are recorded from one thread at a time; CUDA has a per-thread current context (and a legacy default-stream-per-thread mode). The shim therefore keeps a thread-local that is the hottest data structure in the system.

```rust
thread_local! {
    static TLS: UnsafeCell<ThreadState> = UnsafeCell::new(ThreadState::new());
}

pub struct ThreadState {
    /// Currently-bound GL/EGL context (null => no context, calls are no-ops/errs).
    gl_current: *mut GlContext,
    /// CUDA current context stack (cuCtxPushCurrent/Pop).
    cu_ctx_stack: SmallVec<[CuCtx; 4]>,
    /// Per-thread encode staging buffer (avoids cross-thread locking).
    enc: Encoder,
    /// Session/transport channel handle (shared Arc, cloned cheaply).
    session: Option<SessionRef>,
    /// Passthrough resolved fn pointers (lazily populated, §9.7).
    real: RealVtable,
    passthrough: bool,
}
```

Design rules:

- **No global lock on the hot path.** Each thread owns its `Encoder`; the only shared object is the transport ring, whose multi-producer enqueue is lock-free or sharded (the Transport chapter (Ch. 08) / the Memory chapter (Ch. 12)).
- **`current` reads are a single TLS load.** `glXMakeCurrent` updates `gl_current`; subsequent draw calls do not re-validate.
- **Context objects are `Arc`-shared, thread state is not.** Two threads can be current on different contexts simultaneously; the `GlContext` carries the remote handle and is reference-counted because contexts can be shared between threads via `glXMakeCurrent`.
- **`UnsafeCell` not `RefCell`.** The hot path cannot afford borrow-flag checks; access is statically single-threaded by virtue of being thread-local. All access is wrapped in `// SAFETY:` accessors that document the single-owner invariant.

CUDA's "primary context" and per-thread-default-stream semantics are subtle: the shim must mirror the host's notion of current context so that an implicit-stream `cuLaunchKernel` lands on the right remote stream. The TLS context stack is replayed verbatim onto the server so host and guest agree.

## 9.6 Lazy connection and bootstrap ordering

The shim must not connect at `dlopen` time. Constructors (`#[ctor]`/`.init_array`) run before `main`, possibly before the app has set env vars, and definitely before the app has decided it even wants the GPU. Connecting eagerly would (a) add latency to every process that merely links the lib, and (b) risk deadlocks because the transport may use threads/`malloc` while the dynamic loader still holds its lock.

The plan: a `OnceCell`-guarded lazy init triggered by the *first* meaningful API call.

```rust
static SESSION: OnceCell<SessionRef> = OnceCell::new();

fn session() -> Result<&'static SessionRef, GraftxError> {
    SESSION.get_or_try_init(|| {
        let cfg = Config::from_env();         // GRAFTX_VSOCK_CID, etc.
        let transport = Transport::connect(&cfg)?;  // Ch. 08
        handshake(&transport)?;               // Hello/Welcome negotiate, Ch. 06
        Ok(SessionRef::new(transport))
    })
}
```

Bootstrap ordering constraints:

```text
.init_array  : register passthrough symbol resolution ONLY (dlsym RTLD_NEXT,
               lazy/deferred); set up atexit for clean session teardown.
first call   : OnceCell init -> connect -> handshake -> negotiate feature set.
              If connect fails AND policy=fallback -> latch session passthrough,
              log once. The latch happens BEFORE any context is created, so each
              context created thereafter inherits an immutable passthrough flag
              (§9.7) — lazy connect never flips a live context mid-stream.
fork()       : pthread_atfork child handler resets SESSION (fd is not shared-safe);
              child re-lazy-connects on next call. Parent untouched.
```

The `atfork` child handler is essential: a forked child inheriting a connected vsock fd and shared-memory mapping would corrupt the parent's stream. The child clears `SESSION` and any TLS so it reconnects with a fresh `session_id` from the Welcome reply (the Protocol chapter (Ch. 06)). `dlclose`/`atexit` flushes pending commands and sends a session-close so the server can reap resources (the Handles chapter (Ch. 11) / the Sync chapter (Ch. 13)).

## 9.7 Passthrough fallback

Passthrough is both a robustness feature and a debugging tool. When GraftX cannot or should not remote a call, it forwards to the genuine driver loaded via `RTLD_NEXT`.

```rust
pub struct RealVtable { /* lazily-resolved fn pointers */ }

unsafe fn real_glClear() -> unsafe extern "C" fn(GLbitfield) {
    // SAFETY: dlsym(RTLD_NEXT,..) returns the next lib in search order,
    // i.e. the genuine driver behind our shadow. Cached after first resolve.
    static P: OnceCell<usize> = OnceCell::new();
    let a = P.get_or_init(|| unsafe {
        libc::dlsym(libc::RTLD_NEXT, c"glClear".as_ptr()) as usize
    });
    mem::transmute(*a)
}
```

Passthrough triggers:

| Trigger | Action |
|---------|--------|
| `GRAFTX_DISABLE=1` env | Whole library is a pure pass-through forwarder (zero remoting) |
| Connect/handshake failure + `policy=fallback` | Mark `passthrough=true`, run locally on guest software/GPU |
| Per-API allowlist excludes an entrypoint | That single call forwards; rest remote |
| Unknown `GetProcAddress` extension in diag mode | That call forwards |

Important constraint: passthrough is only *correct* for a whole context's lifetime. We cannot remote `glGenBuffers` then pass-through `glBindBuffer` on the same context — the remote and local GL states diverge. Therefore the `passthrough` flag is decided **per context at creation** and is immutable thereafter; mixing is forbidden except for genuinely stateless query calls (`glGetString(GL_VERSION)` in diag). This is consistent with the lazy-connect path (§9.6): the session-level fallback decision is latched on the first meaningful call, *before* any context object exists, so every context that is subsequently created reads a stable session passthrough state and seals its own immutable flag from it. Lazy connect can therefore never toggle a context that is already live. The ICD/loader case is cleaner: a Vulkan layer can chain through to the real ICD per-instance, which we will use for a "shadow" passthrough layer in testing.

For `LD_PRELOAD`, `RTLD_NEXT` works only if a real driver is actually present in the guest. In the target deployment the guest has no physical GPU, so passthrough resolves to a software rasterizer (llvmpipe / lavapipe) if installed, else returns an honest error. Passthrough is thus primarily a *development-host* affordance and a graceful-degradation path, not the production happy path.

## 9.8 Error surface and the `no-unwrap` rule

The shim sits on the C ABI boundary; a Rust panic unwinding into C is undefined behavior. Per the workspace rules (no `unwrap` in lib paths, `thiserror` errors), every `extern "C"` body will:

- be wrapped so that an internal `Result::Err` is converted to the API's native error sentinel (`VK_ERROR_DEVICE_LOST`, `CL_OUT_OF_RESOURCES`, `GL_INVALID_OPERATION` via the error queue, `CUDA_ERROR_*`), never a `panic!`;
- rely on the client cdylib being built `panic=abort` (the Versioning chapter (Ch. 29) / the Build/dist chapter (Ch. 28) — separate cargo invocation per D11) so a panic in shim glue terminates the process at the panic site rather than unwinding across the C ABI boundary, which is undefined behavior; `catch_unwind` is retained only as a belt-and-braces guard for builds that are *not* `panic=abort` (e.g. test/dev rlib builds), translating an unexpected panic into the device-lost sentinel and marking the session poisoned (the Sync chapter (Ch. 13)). It catches only Rust panics in glue — never a native driver fault (those are a server-side concern, handled per the Server core chapter (Ch. 10) / the Error/FFI chapter (Ch. 24));
- emit at most one rate-limited log line per distinct fault to avoid log storms on hot calls.

```rust
#[inline]
fn guard<R>(default: R, f: impl FnOnce() -> R) -> R {
    match std::panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => { session::poison(); default } // SAFETY: never unwind into C
    }
}
```

## 9.9 Open questions / tradeoffs

- **Generated vs. hand-written trampolines.** Codegen (the Vulkan chapter (Ch. 14)) keeps thousands of symbols consistent but produces large `.so`s and slow builds; a hand-written hot subset layered over generated cold symbols is the proposed compromise.
- **`catch_unwind` cost.** It is cheap when no panic occurs but adds a landing pad. Per D11 the shipped client cdylib is built `panic=abort` (a separate cargo invocation from the unwinding server), so unwinding is impossible there and the landing-pad cost vanishes; the `catch_unwind` guard is meaningful only in non-`panic=abort` test/dev rlib builds.
- **Versioned-symbol fidelity.** Fully reproducing GLIBC-style symbol versioning across all APIs is laborious; the plan is to version-script only the symbols apps actually resolve by version (measured, not exhaustive).
- **Layer vs. ICD for Vulkan.** ICD is the primary remoting path; the layer form is retained for selective interception and for chaining to a real ICD during conformance testing.

Downstream chapters consume this layer: the Serialization chapter (Ch. 07) defines how a dispatched call's arguments become bytes; the Handles chapter (Ch. 11) turns `Creates`/`Destroys` classes into a server-authoritative handle map; the Memory chapter (Ch. 12) governs when the per-thread encoder flushes and how `MapsMemory` calls touch ivshmem.
