# Handles

The **generational handle table** is GraftX's mechanism for naming server-side
resources over the wire without ever exposing a server pointer. The client only
ever sees an opaque 64-bit integer; the server resolves it back to a live object
through a [`HandleTable<T>`], and rejects any handle that no longer points at the
object it was issued for. This is the layer that turns "the client sent me an
arbitrary `u64`" into "I have a validated `&mut GlBuffer`" — or into a clean
`None` that the dispatcher maps to a protocol error.

This document describes the `Handle` wire type (from `crates/graftx-protocol`)
and the `HandleTable<T>` implementation in `crates/graftx-handles`. For where
handles sit in the wider threat model, see [`SECURITY_MODEL.md`](SECURITY_MODEL.md);
for the wire frame that carries them, see [`PROTOCOL.md`](PROTOCOL.md).

## The `Handle` wire type

A [`Handle`] is a newtype around a `u64` (`Handle(u64)`, decision D5). It is
carried on the wire as its raw little-endian 64-bit value and is packed from
three fields:

```text
 bits 56..64   bits 32..56     bits 0..32
┌───────────┬───────────────┬───────────────┐
│ kind (8)  │ generation(24)│ slot idx (32) │
└───────────┴───────────────┴───────────────┘
```

| Field        | Bits | Accessor          | Meaning                                                        |
| ------------ | ---- | ----------------- | -------------------------------------------------------------- |
| `kind`       | 8    | `Handle::kind`    | Which resource family this handle names (per-backend tag).     |
| `generation` | 24   | `Handle::generation` | Slot lifetime counter; bumped each time the slot is freed.  |
| `slot`       | 32   | `Handle::slot`    | Index into the table's slot vector.                            |

The field widths are exposed as the constants `Handle::KIND_BITS` (8),
`Handle::GENERATION_BITS` (24), and `Handle::SLOT_BITS` (32). The largest
representable generation is `Handle::GENERATION_MAX = 2^24 - 1`; a slot whose
generation would exceed this is **retired** rather than wrapped (see
[Retirement](#retirement-near-generation_max)).

The packing/unpacking API is small and total:

- `Handle::new(kind, generation, slot)` packs the three fields. The generation
  is masked to `GENERATION_BITS`, so any high bits are discarded and the value
  always round-trips back through `Handle::generation`.
- `Handle::kind()`, `Handle::generation()`, `Handle::slot()` extract the fields.
- `Handle::raw()` returns the underlying `u64`; `Handle::from_raw(u64)` wraps a
  value received off the wire. **`from_raw` performs no validation** — it merely
  reinterprets the bits. Validation happens only when the handle is resolved
  against a table (next section), which is exactly why a forged handle is harmless
  until it is looked up.

`Handle` is `Copy` and cheap to pass by value; the table accessors take it by
value.

## `HandleTable<T>` design

[`HandleTable<T>`] maps wire handles to server-side resources of type `T`. The
crate is `#![forbid(unsafe_code)]` — the whole UAF defense is built from safe
generational bookkeeping, not raw pointers.

### Slots and the free-list

Internally the table holds a `Vec<Slot<T>>` plus a free-list. Each `Slot<T>`
records:

- `generation: u32` — the generation a handle must carry to resolve against this
  slot.
- `kind: u8` — the kind byte the live value was inserted under. Recorded so that
  a live slot's full `Handle` can be reconstructed for read-only introspection
  (`iter`, `iter_kind`, `retain`); meaningful only while the slot is occupied.
- `value: Option<T>` — `Some` while live, `None` when free or retired.
- `retired: bool` — once `true`, the slot is permanently out of service.

The table also keeps `free: Vec<u32>`, the indices of free, **non-retired** slots
available for reuse, and `live: usize`, the count of occupied slots (so
[`len`](#introspection-and-quota-api) is `O(1)`).

### Insert

`insert(kind, value) -> Handle`:

1. If the free-list is non-empty, pop a slot index, drop the new value into it,
   stamp it with `kind`, and hand back `Handle::new(kind, slot.generation, idx)`.
   The slot's generation was already bumped when it was freed, so the returned
   handle carries the *new* generation — distinct from any handle that named the
   slot's previous occupant.
2. Otherwise append a fresh slot at generation `0` and return a handle for it.

The returned handle is valid only until the slot is freed.

### Resolve

`get(h) -> Option<&T>` and `get_mut(h) -> Option<&mut T>` resolve a handle in
three checks, returning `None` on the first failure:

1. **Range** — `slots.get(h.slot())`; out-of-range slot index ⇒ `None`.
2. **Generation** — `slot.generation == h.generation()`; mismatch ⇒ `None`.
3. **Occupancy** — `slot.value.as_ref()`; an empty slot yields `None`.

Notably, the `kind` byte is **not** consulted during resolution. Kind isolation
is achieved structurally (one table per kind family — see
[Per-kind namespacing](#per-kind-namespacing)), not by comparing the kind field;
the field exists so a handle can be reconstructed and so the dispatcher can route
to the right backend before resolving.

The `Option` API is what the dispatcher uses on the hot path. For callers that
want to know *why* a handle failed, the crate also defines
[`HandleError`] with `SlotOutOfRange`, `EmptySlot`, and `GenerationMismatch`
(carrying the slot index, the handle's generation, and the slot's current
generation) variants.

### Remove and the generation bump

`remove(h) -> Option<T>` validates `h` exactly as `get` does, then takes the
value out, decrements the live count, and **bumps the slot's generation**:

- If `slot.generation < GENERATION_MAX`, increment it and push the slot index
  onto the free-list for reuse.
- If `slot.generation >= GENERATION_MAX`, retire the slot (set `retired = true`)
  and do **not** return it to the free-list.

Because the generation is bumped on every free, the handle that was just removed
— and every other copy of it the client still holds — now carries a stale
generation and will never resolve again, even after the slot is reused under a
fresh generation. Removing an already-stale or empty handle is a safe no-op that
returns `None` and leaves the live count unchanged.

## Use-after-free and forged-handle defense

The table treats every incoming handle as **untrusted client input**. Two attack
shapes are covered:

### Use-after-free (stale handle)

A client frees a resource, then submits a later request still naming the freed
handle — either by mistake or to try to reach whatever object now occupies that
slot. The slot may already have been reused by a different object (potentially a
different `kind`, owned conceptually by a different logical resource). The
generation bump on `remove` defeats this: the freed handle carries the old
generation, the reused slot carries the bumped generation, the generations
differ, and `get`/`get_mut`/`remove`/`is_live` all return `None`/`false`. The
client cannot reach the new occupant through the old handle.

### Forged handle

A client fabricates a `u64` and sends it as a handle. `Handle::from_raw` accepts
any bit pattern, so the forgery costs nothing to construct — but it must still
survive the three resolution checks. To resolve, a forged handle would have to
guess a slot index that is in range, currently occupied, **and** carrying the
exact 24-bit generation that slot holds right now. A wrong slot index is
out-of-range or empty; a right slot index with the wrong generation is a
mismatch. The generation field is what makes a blind forgery improbable rather
than a coin flip on the slot index.

> Note: the handle table is **not** a confidentiality boundary by itself — a
> determined client could brute-force the 24-bit generation for a known-occupied
> slot. Per-session isolation, quotas, and parent-handle ownership checks (in
> `graftx-server`) are layered on top; see [`SECURITY_MODEL.md`](SECURITY_MODEL.md).
> The table's job is to guarantee that *no* handle ever silently aliases a
> different object than the one it was issued for.

### Retirement near `GENERATION_MAX`

The generation field is finite (24 bits). If a slot were freed and reused
`2^24` times, its generation would wrap back to a value that some long-lived,
still-circulating handle might hold — re-opening the use-after-free hole. To
close it, a slot whose generation reaches `GENERATION_MAX` is **retired** on its
final removal: its value is cleared, it is kept off the free-list, and it is
never allocated again. The slot index is effectively burned. The
[`retired`](#introspection-and-quota-api) accessor counts how many slots have met
this fate; `capacity` keeps counting them (they still occupy the slot vector)
while `len` does not. Retirement trades a small, bounded amount of address space
for a hard guarantee that generations never wrap.

## Per-kind namespacing

The `kind` byte lets the protocol and the server tell resource families apart.
The convention in `graftx-server` is **one `HandleTable<T>` per resource family**,
with a distinct `kind` constant per family, rather than one shared table keyed by
kind. For example the GL backend keeps `contexts: HandleTable<CtxState>` and
`buffers: HandleTable<BufState>` as separate tables, and stamps inserts with
`KIND_GL_CONTEXT` / `KIND_GL_BUFFER` respectively.

The kind constants are assigned per backend in `graftx-server`; the families in
use today include (kind byte → family):

| Kind | Family                         |
| ---- | ------------------------------ |
| 1–8  | Vulkan: instance, physical device, device, queue, device memory, buffer, command pool, command buffer |
| 10–11 | OpenGL: context, buffer       |
| 20–21 | CUDA: context, device pointer |
| 30–31 | HIP: device pointer, stream   |
| 40–41 | OpenCL: context, memory       |
| 50–51 | Level Zero: context, device memory |
| 60   | Video session                  |
| 70–71 | WebGPU: device, buffer        |
| 80–81 | OptiX: context, pipeline      |
| 90–91 | SYCL: queue, device pointer   |
| 100  | AMF encoder                    |

Because resolution ignores the kind field, a handle issued by one table only
ever resolves against *that* table; sending a GL-buffer handle to the CUDA
backend resolves against the CUDA tables, where its slot index either misses or
hits an unrelated generation — it does not silently cross families. The kind byte
is also what lets the dispatcher and introspection (`iter_kind`, `count_by_kind`,
`kinds`) report and filter by family. A slot reused under a *different* kind
records the new kind, so introspection never reports the stale kind of a prior
occupant.

## Introspection and quota API

Beyond the core `insert` / `get` / `get_mut` / `remove`, `HandleTable<T>` exposes
a read-mostly API for quotas, observability, and bulk cleanup. None of these
methods can resurrect a stale handle; all of them honor the live/free/retired
distinction.

### Occupancy

- `len() -> usize` — number of live (occupied) entries; `O(1)`.
- `is_empty() -> bool` — `true` when no entries are live.
- `capacity() -> usize` — total allocated slots (live + free + retired). It never
  shrinks except is left intact by `clear`, and is always `>= len()`.
- `retired() -> usize` — number of permanently retired slots.
- `is_live(h) -> bool` — whether `h` currently resolves; same validation as
  `get`, so it returns `false` for out-of-range, empty, retired, or stale handles.

### Per-kind queries

- `count_by_kind(kind) -> usize` — number of live entries tagged with `kind`
  (empty and retired slots are not counted).
- `kinds() -> Vec<u8>` — the sorted, de-duplicated kind bytes among live entries;
  empty when nothing is live.

### Iteration

- `iter() -> impl Iterator<Item = (Handle, &T)>` — every live entry as a
  `(handle, &value)` pair. Empty and retired slots are skipped, and each yielded
  handle is reconstructed from the live slot, so it resolves via `get` for as
  long as the slot stays live. Order follows slot indices and is otherwise
  unspecified.
- `iter_kind(kind) -> impl Iterator<Item = (Handle, &T)>` — same, restricted to
  live entries whose recorded kind matches; each yielded handle's `kind()` equals
  the argument.

### Snapshot

- `stats() -> HandleStats` — a point-in-time bundle of `live`, `retired`,
  `capacity`, and the sorted `kinds`. Each field equals what its dedicated
  accessor would return at the same instant. Useful for a single observability
  read without four separate calls.

### Quota-enforcing insert

- `try_insert(kind, value, max_live) -> Option<Handle>` — behaves like `insert`
  and returns `Some(handle)` while `len() < max_live`; otherwise the value is
  dropped, the table is left unchanged, and `None` is returned. Callers map the
  `None` case to a resource-exhausted protocol error. A `max_live` of `0` always
  returns `None`. This is the per-table quota knob: it bounds how many live
  objects of a family a client can hold at once.

### Bulk cleanup

- `clear()` — frees every live entry exactly as `remove` would: values are
  dropped and each slot's generation is bumped (or the slot retired). Bumping the
  generation guarantees every handle issued before the `clear` is now stale, so
  no future handle can alias a pre-`clear` handle. Slot storage is retained
  (`capacity` unchanged); only `len` resets to `0`. This is how a session tears
  down all of a client's resources at disconnect without leaking the right for
  the client to reach any of them again.
- `retain(keep) -> usize` — visits each live entry in slot-index order, calling
  `keep(handle, &value)`; entries for which `keep` returns `false` are freed with
  the same generational bookkeeping as `remove` (their handles become stale),
  while the rest are left valid. Empty and retired slots are skipped. Returns the
  number of entries removed. This is the building block for garbage-collection
  passes (e.g. dropping a session's objects, or sweeping objects whose parent was
  destroyed).

## Where this is used

- `crates/graftx-protocol` defines `Handle` and carries it in every request/
  response body that names a resource.
- `crates/graftx-handles` provides `HandleTable<T>`, used by each backend in
  `crates/graftx-server` to track its live objects.
- The server's dispatcher resolves incoming handles through these tables; a
  `None` resolution becomes a protocol error rather than a panic or a UAF. See
  [`SECURITY_MODEL.md`](SECURITY_MODEL.md) and [`BACKENDS.md`](BACKENDS.md).

[`Handle`]: ../../crates/graftx-protocol/src/lib.rs
[`HandleTable<T>`]: ../../crates/graftx-handles/src/lib.rs
[`HandleError`]: ../../crates/graftx-handles/src/lib.rs
