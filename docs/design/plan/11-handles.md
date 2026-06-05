# 11. Handle & Object Lifetime Management

How GraftX will map opaque guest-side handles to server-side native GPU objects, track lifetime and refcounts, order destruction safely, detect leaks, and share objects across API namespaces.

Every GPU API hands the application opaque references — `VkBuffer`, `GLuint`, `cl_mem`, `CUdeviceptr`, `cudaStream_t`, `ze_kernel_handle_t`. In a non-remoted stack these reference driver-owned memory in the same address space. In GraftX the application runs on the Linux guest, but the real object lives in the Windows server's address space behind a real driver. The handle is **server-authoritative** (decision D1): the server mints the wire `Handle` when it creates the native object, and the client shim (see the Client shim chapter (Ch. 09)) hands the application a *guest token* that is meaningful on the Linux side but is never an invented authority the client puts on the wire. The server (see the Server core chapter (Ch. 10)) translates that token back into the native pointer/handle it created. (For async/deferred replies the client may keep a purely local provisional proxy token, reconciled deterministically when the server's real handle arrives; it is never sent as authority — see 11.7.) This chapter defines that translation layer, its lifetime rules, and the cross-API sharing model. It assumes the protocol encoding from the Protocol chapter (Ch. 06) and the command-validation duties from the Server core chapter (Ch. 10).

## 11.1 Why not pass native pointers across the wire

The naive design forwards the driver's 64-bit handle verbatim: server creates `VkBuffer`, returns its bits, client stores and re-sends them. This is rejected for three reasons.

1. **Security.** The server replays an *untrusted* stream (see the Server core chapter (Ch. 10) and the Security chapter (Ch. 23)). A raw native pointer in a command is an unvalidated, attacker-chosen pointer dereferenced against a real driver. A guest could forge `VkBuffer = 0xdeadbeef` and trigger a driver-side use of arbitrary memory.
2. **Validation cost.** To make raw handles safe the server would need a membership set anyway, so we would build the table regardless — better to make the table the canonical identity.
3. **Type confusion.** Native handle values carry no type tag. A guest could submit a `VkImage` value where a `VkBuffer` is expected. We want a single lookup that proves *this id names a live object of the expected type in this session*.

So the wire never carries native handles. It carries the **GraftX wire `Handle`**: a 64-bit generational key that indexes a per-session, per-namespace slab. This is the single canonical handle layout (decision D5): top 8 bits = kind/API-namespace, middle bits = generation, low bits = slot index — the same `Handle` defined by the Protocol chapter (Ch. 06); `HandleId` here is that one layout, not a divergent struct. Only the server mints it (D1), and translation back to the native object is the server's job, mandatory before any native call.

## 11.2 The generational handle

A handle packs a slab index, a generation counter, and a small namespace tag, in the canonical D5 layout shared with the Protocol chapter (Ch. 06). Packing keeps the wire token to 8 bytes (cheap to serialize — see the Serialization chapter (Ch. 07)) while making stale-handle reuse statistically detectable.

```rust
/// 64-bit opaque token exchanged on the wire — the one canonical `Handle`
/// layout (decision D5), shared with the wire `Handle` in the Protocol chapter (Ch. 06).
/// Layout (MSB..LSB):
///   bits 63..56  kind / API-namespace tag (8 bits, 256 namespaces)
///   bits 55..32  generation               (24 bits, wraps at 16,777,216)
///   bits 31..0   slot index               (32 bits, ~4.29B live slots/ns)
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct HandleId(pub u64);

impl HandleId {
    pub const NULL: HandleId = HandleId(0);
    #[inline] pub fn ns(self) -> Namespace { Namespace((self.0 >> 56) as u8) }
    #[inline] pub fn generation(self) -> u32 { ((self.0 >> 32) & 0x00FF_FFFF) as u32 }
    #[inline] pub fn index(self) -> u32 { self.0 as u32 }
    #[inline] pub fn pack(ns: Namespace, gen: u32, idx: u32) -> Self {
        HandleId(((ns.0 as u64) << 56) | (((gen & 0x00FF_FFFF) as u64) << 32) | idx as u64)
    }
}
```

The slot itself lives in a slab. Each slot stores the current generation, an occupancy/`free` discriminant, and — when occupied — the native object descriptor plus its lifetime metadata.

```rust
struct Slot {
    generation: u32,          // bumped on every free; odd = (optional) tombstone window
    state: SlotState,
}

enum SlotState {
    Free { next_free: u32 },  // intrusive free-list link (sentinel = u32::MAX)
    Live(Box<ObjectEntry>),   // boxed: keeps Slot small, payload size varies by API
}

struct ObjectEntry {
    native: NativeObject,     // tagged union of native handles, see 11.6
    kind: ObjectKind,         // strong type tag (VkBuffer vs VkImage vs ...)
    refcount: u32,            // explicit, API-defined refs (see 11.4)
    parent: Option<HandleId>, // device/context/pool this belongs to (destruction order)
    children: u32,            // count of live dependents (block destroy while > 0)
    bytes: u64,               // accounted resource size for quotas (Ch. 08 / Ch. 23)
    debug_origin: CmdSeq,     // command sequence number that created it (leak reports)
}
```

```rust
pub struct HandleTable {
    slots: Vec<Slot>,
    free_head: u32,           // head of intrusive free-list, u32::MAX if empty
    live_count: u32,
    ns: Namespace,
}

impl HandleTable {
    /// Insert a native object, return a fresh generational id. O(1) amortized.
    fn insert(&mut self, entry: ObjectEntry) -> Result<HandleId, HandleError> {
        let idx = if self.free_head != u32::MAX {
            let i = self.free_head;
            match &self.slots[i as usize].state {
                SlotState::Free { next_free } => self.free_head = *next_free,
                SlotState::Live(_) => return Err(HandleError::Corrupt(i)),
            }
            i
        } else {
            let i = u32::try_from(self.slots.len()).map_err(|_| HandleError::TableFull)?;
            self.slots.push(Slot { generation: 1, state: SlotState::Free { next_free: u32::MAX } });
            i
        };
        let slot = &mut self.slots[idx as usize];
        slot.state = SlotState::Live(Box::new(entry));
        self.live_count += 1;
        Ok(HandleId::pack(self.ns, slot.generation, idx))
    }

    /// Resolve a wire id to a native object, proving liveness, generation, and type.
    fn resolve(&self, id: HandleId, want: ObjectKind) -> Result<&ObjectEntry, HandleError> {
        if id == HandleId::NULL { return Err(HandleError::NullHandle); }
        let idx = id.index() as usize;
        let slot = self.slots.get(idx).ok_or(HandleError::OutOfRange(idx))?;
        match &slot.state {
            SlotState::Free { .. } => Err(HandleError::UseAfterFree(id)),
            SlotState::Live(e) => {
                if slot.generation != id.generation() { return Err(HandleError::Stale(id)); }
                if e.kind != want { return Err(HandleError::TypeMismatch { got: e.kind, want }); }
                Ok(e)
            }
        }
    }

    fn remove(&mut self, id: HandleId) -> Result<ObjectEntry, HandleError> {
        let idx = id.index() as usize;
        // resolve-style checks elided; on success:
        let slot = &mut self.slots[idx];
        let new_gen = slot.generation.wrapping_add(1).max(1); // never reuse gen 0
        let taken = match std::mem::replace(&mut slot.state, SlotState::Free { next_free: self.free_head }) {
            SlotState::Live(e) => *e,
            SlotState::Free { .. } => return Err(HandleError::DoubleFree(id)),
        };
        slot.generation = new_gen;
        self.free_head = idx as u32;
        self.live_count -= 1;
        Ok(taken)
    }
}
```

**Generation wrap.** 24 bits means a single slot must be freed 16.7M times before a stale id collides. Because the index must *also* match, a colliding forged id requires guessing both 32-bit index and the exact wrapped generation. We treat collision probability as negligible for the threat model, but we additionally clear `bytes`/`parent` on free so a resurrected-looking id still fails type/parent checks downstream.

### Resolve error taxonomy

```rust
#[derive(thiserror::Error, Debug)]
pub enum HandleError {
    #[error("null handle where a live object was required")] NullHandle,
    #[error("handle index {0} out of table range")] OutOfRange(usize),
    #[error("use-after-free of handle {0:?}")] UseAfterFree(HandleId),
    #[error("stale generation for handle {0:?}")] Stale(HandleId),
    #[error("type mismatch: got {got:?}, expected {want:?}")] TypeMismatch { got: ObjectKind, want: ObjectKind },
    #[error("double free of handle {0:?}")] DoubleFree(HandleId),
    #[error("handle table full")] TableFull,
    #[error("destroy blocked: {0} live children")] HasChildren(u32),
    #[error("corrupt free-list at slot {0}")] Corrupt(u32),
}
```

Per priorities (safety is last but use-after-free against a real driver is a security issue, not a mere safety nicety), any `HandleError` during command decode aborts that command, emits a protocol error reply (see the Protocol chapter (Ch. 06)), and — for `Corrupt`, repeated `UseAfterFree`, or `DoubleFree` — may escalate to session teardown (see the Security chapter (Ch. 23)), since those indicate either a buggy or hostile client.

## 11.3 Namespaces per API

Each API family owns a namespace tag so that ids are only ever resolved through the table that created them, and so an `OpenGL` id can never be accidentally accepted by the Vulkan dispatcher. This tag is the kind/API-namespace field in the top 8 bits of the canonical D5 `Handle` layout. It is a handle-table concept and is distinct from (though it mirrors) the `ApiId` byte used to build opcodes in the Protocol chapter (Ch. 06, decision D4): EGL gets its own handle namespace here because it has a separate object model, even though it has no standalone opcode `ApiId`.

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Namespace(pub u8);
impl Namespace {
    // GPU-API tags mirror the D4 ApiId values from the Protocol chapter (Ch. 06).
    pub const VULKAN: Namespace = Namespace(1);
    pub const GL: Namespace = Namespace(2);   // GL / GLES share GL object model
    pub const CUDA: Namespace = Namespace(3);
    pub const OPENCL: Namespace = Namespace(4);
    pub const HIP: Namespace = Namespace(5);
    pub const LEVELZERO: Namespace = Namespace(6);
    pub const VIDEO: Namespace = Namespace(7); // codecs
    // Handle-only namespaces (no standalone opcode ApiId):
    pub const EGL: Namespace = Namespace(8);   // EGL has a separate object model
    // ... reserved up to 255
}
```

A `SessionHandles` aggregate holds one `HandleTable` per namespace plus the cross-API alias map (11.6):

```rust
pub struct SessionHandles {
    tables: [Option<Box<HandleTable>>; 256], // lazily allocated per namespace
    aliases: HashMap<ExternalKey, HandleId>, // 11.6 shared-object registry
    limits: HandleLimits,                    // Ch. 08 / Ch. 23 quotas
}
```

Tables are allocated lazily: a CUDA-only session never pays for a Vulkan slab. The dispatcher selects the table from the *command's* API family, not from the id's embedded tag — then asserts they match. This double check (command family == id tag) catches a forged tag cheaply.

**GL caveat.** OpenGL is unusual: object names are `GLuint` values *the application can choose* (`glBindBuffer(GL_ARRAY_BUFFER, 7)` may implicitly create name 7). GraftX cannot let the guest pick raw names that index our slab directly. The GL namespace therefore keeps a second `HashMap<GlName, HandleId>` per GL context mapping the guest-visible `GLuint` to our internal `HandleId`; the shim presents `GLuint`s the application expects while the server stores the real driver name. `glGenBuffers` allocates both. This GL-name indirection is documented further in the OpenGL/GLES/EGL/GLX chapter (Ch. 15).

## 11.4 Lifetime, refcounts, and destruction ordering

GPU APIs disagree on lifetime semantics, so the table stores enough metadata to enforce the *strictest* model and emulate the looser ones.

| API     | Lifetime model                              | GraftX enforcement |
|---------|---------------------------------------------|--------------------|
| Vulkan  | Explicit create/destroy, app-managed order, parent pools | `refcount` fixed at 1; `parent`/`children` enforce "destroy children first" |
| OpenGL  | Names deleted but kept alive while bound/in-use | defer free until `children == 0`; driver keeps real object |
| CUDA    | Context owns module owns function; streams/events explicit | `parent` chain context→module→function |
| OpenCL  | Reference-counted (`clRetain*`/`clRelease*`) | `refcount` drives actual destroy |
| LevelZero | Explicit, scoped to context/module          | `parent` chain |

The unified rule set:

1. **Create** inserts with `refcount = 1`, records `parent`, increments parent's `children`.
2. **Retain** (`clRetainMemObject`, etc.) increments `refcount`; non-refcounted APIs reject retain.
3. **Release/Destroy** decrements `refcount`; native destruction fires only when `refcount` hits 0 **and** `children == 0`. If `children > 0`, return `HandleError::HasChildren` (Vulkan, which the spec says is UB but we choose to reject) or *defer* (GL semantics).
4. On native destroy, decrement the parent's `children` and clear the alias entry if any (11.6).

```rust
fn destroy(&mut self, id: HandleId) -> Result<DestroyOutcome, HandleError> {
    let table = self.table_mut(id.ns())?;
    let entry = table.resolve_mut(id, /*any kind*/)?;
    if entry.children > 0 {
        return match entry.kind.lifetime_policy() {
            LifetimePolicy::RejectWithChildren => Err(HandleError::HasChildren(entry.children)),
            LifetimePolicy::DeferUntilIdle => { entry.deferred = true; Ok(DestroyOutcome::Deferred) }
        };
    }
    entry.refcount -= 1;
    if entry.refcount > 0 { return Ok(DestroyOutcome::RefcountStillPositive(entry.refcount)); }
    let parent = entry.parent;
    let removed = table.remove(id)?;
    self.invoke_native_destroy(&removed)?;          // calls real vkDestroyBuffer, etc.
    if let Some(p) = parent { self.dec_children(p)?; }
    if let Some(k) = removed.external_key() { self.aliases.remove(&k); }
    Ok(DestroyOutcome::Destroyed)
}
```

### Destruction ordering walk-through

The dangerous case is parent destroyed before children. Sequence for a Vulkan device tear-down where the guest is buggy:

```text
guest -> server: vkDestroyDevice(dev_id)            // device still has 3 buffers
server: resolve dev_id -> ObjectEntry{children=3}
server: children>0 && policy=RejectWithChildren
server -> guest: ProtocolError(HasChildren{3})       // device NOT destroyed
            // guest must destroy buffers first; native driver never sees bad order
```

Contrast GL, where deletion of an in-use texture is legal:

```text
guest -> server: glDeleteTextures([tex_id])
server: entry.children>0 (bound to live FBO) -> mark deferred, keep native alive
... later ...
guest -> server: glDeleteFramebuffers([fbo_id])      // last referent gone
server: dec_children(tex) -> children==0 && deferred -> fire glDeleteTextures now
```

This lets GraftX honor each API's contract through one mechanism, differing only by `LifetimePolicy`.

## 11.5 Session teardown and leak detection

When a session ends — clean disconnect, transport drop, or kill (see the Security chapter (Ch. 23)) — every live object the guest created must be released against the native driver, in dependency order, or the Windows GPU leaks until reboot.

**Ordered drain.** We cannot free in arbitrary order. The table drains by repeatedly destroying all leaves (`children == 0`) until empty; because `parent`/`children` form a DAG (no cycles by construction — a child always has an earlier-created parent), this terminates. A topological drain pseudocode:

```rust
fn drain_session(&mut self) -> LeakReport {
    let mut report = LeakReport::default();
    loop {
        let leaves: Vec<HandleId> = self.iter_live()
            .filter(|e| e.children == 0)
            .map(|e| e.id).collect();
        if leaves.is_empty() { break; }
        for id in leaves {
            report.record(id, /*entry meta*/);
            let _ = self.force_native_destroy(id); // ignore driver errors during teardown
        }
    }
    // anything still live is a cycle/corruption bug -> log loudly
    report.orphans = self.live_count_all();
    report
}
```

`force_native_destroy` ignores `refcount`/`children` guards (the session is gone; correctness no longer matters, only freeing the GPU) but still logs each forced free.

**Leak attribution.** Every `ObjectEntry` stores `debug_origin: CmdSeq` and `bytes`. The `LeakReport` summarizes per-namespace live counts and bytes at teardown, plus the create-site command sequence numbers of the longest-lived objects:

```rust
#[derive(Default)]
pub struct LeakReport {
    pub per_ns: [NsLeak; 256],
    pub orphans: u32,             // non-zero => internal bug
    pub top_origins: Vec<(CmdSeq, ObjectKind, u64 /*bytes*/)>,
}
```

In debug builds a periodic watchdog can also flag objects whose live duration exceeds a threshold and whose namespace has surpassed a count high-water mark, surfacing client-side leaks while the session is still running (these feed quota enforcement in the Transport chapter (Ch. 08) and the Security chapter (Ch. 23)). Production builds keep this off the hot path; the table only counts.

## 11.6 Cross-API object sharing

Real workloads share objects across APIs: a Vulkan image imported into CUDA (`VK_KHR_external_memory` + `cudaExternalMemory`), a GL texture shared with OpenCL (`cl_khr_gl_sharing`), or a DXGI/Win32 shared handle. In GraftX both APIs run against the *same* native driver stack on the server, so the underlying object can genuinely be shared — we must not double-create or double-free it.

The design: a shared object has *one* `ObjectEntry` (in the namespace that created it) and *N* **alias handles** in other namespaces that point at the same native resource via an `external_key`.

```rust
/// Stable cross-API identity for a shareable resource.
#[derive(Clone, PartialEq, Eq, Hash)]
enum ExternalKey {
    OpaqueFd(u64),          // emulated fd token (vsock can't pass real fds)
    Win32Name(String),      // NT shared handle name
    GlSharingId(u64),       // CL<->GL bridge
}

enum NativeObject {
    Vulkan(VkObject),
    Cuda(CuObject),
    // ...
    Alias { owner: HandleId, key: ExternalKey }, // resolves through owner
}
```

When the guest exports (e.g. `vkGetMemoryFdKHR`), the server mints an `ExternalKey`, stores it on the owning entry, and returns the opaque token to the client. When another API imports that token (`cudaImportExternalMemory`), the server looks up `aliases[key]`, finds the owner `HandleId`, and inserts a new `ObjectEntry::Alias` in the *CUDA* namespace pointing back. `resolve` on an alias transparently follows `owner`. Refcount/destroy rules:

- The alias holds a +1 ref on the owner (`owner.refcount += 1`).
- Destroying the alias decrements the owner's ref; native destroy fires only when *all* aliases and the original are released.
- This makes "free order across APIs" safe regardless of which side the guest releases first.

```text
guest: vkAllocateMemory -> mem_id (VULKAN ns, refcount=1)
guest: vkGetMemoryFdKHR(mem_id) -> server mints key=OpaqueFd(42), aliases[42]=mem_id
guest: cudaImportExternalMemory(42) -> insert Alias in CUDA ns, mem.refcount=2
guest: cudaDestroyExternalMemory(cuda_id) -> mem.refcount=1 (native NOT freed)
guest: vkFreeMemory(mem_id) -> refcount=0, children=0 -> native vkFreeMemory fires
```

**Tradeoff.** An alternative is per-namespace independent objects with explicit copy on the bulk plane. That avoids the alias bookkeeping but defeats the entire point of zero-copy sharing and would corrupt semantics for true shared images. We accept the alias-map complexity because breadth of correct API coverage (priority #1) demands genuine interop. The cost is one `HashMap` lookup per import/export and a slightly more involved teardown drain (aliases are leaves; they drain before their owner, naturally satisfying the topological order in 11.5).

## 11.7 Concurrency and open questions

The table is per session. Within a session, command streams from multiple guest threads may be multiplexed onto channels (see the Transport chapter (Ch. 08)). The server may dispatch in parallel, so `SessionHandles` will sit behind an `RwLock` (read-mostly: `resolve` is a shared read; `insert`/`destroy` take the write lock) or, if contention shows up, a sharded lock keyed by namespace. Resolve is on the hot path of every command, so the read path must stay allocation-free and branch-light — the `resolve` body above is deliberately a bounds check, two enum matches, and two integer compares.

**Provisional proxy tokens (decision D1).** For async/deferred replies — where the client issues a create and continues building commands before the server's real `Handle` arrives — the client shim (see the Client shim chapter (Ch. 09)) may hand the application a purely *local* provisional proxy token. This token is never placed on the wire as authority; when the server's minted `Handle` returns, the shim deterministically reconciles the provisional token to the real server-authoritative `Handle`. The server's table never sees the provisional value.

Open items deferred to implementation: (a) whether generation should be 24 vs 32 bits (trading namespace tag width); (b) optional per-handle HMAC for the per-session integrity goal noted in security scope, layered on top of the generational id rather than replacing it; (c) interaction with command batching (see the Client shim chapter (Ch. 09)), where a create and its first use arrive in one batch and `resolve` must see the just-inserted id within the same batch transaction.
