# 07. Serialization & Encoding Strategy

How GraftX will turn intercepted GPU API calls into bytes on the wire — zero-copy where the layout allows it, validated decode always, codegen for the bulk of the surface.

GraftX intercepts thousands of C entry points across a dozen GPU APIs and must serialize each call into a command frame, ship it over the Transport chapter (Ch.08), and decode it on the Windows server for replay (the Server-core chapter, Ch.10). Serialization sits on the hottest path in the system: a single frame in a Vulkan render loop may carry a `vkCmdDrawIndexed` plus its bound descriptor state, and a tight loop can issue tens of thousands per second. The design priority order is *breadth > performance > stability > safety*, but serialization is the one subsystem where performance and breadth pull hardest against each other: a fully generic, reflection-driven encoder would cover everything cheaply in developer time but allocate and copy on every argument; a fully hand-tuned per-call encoder is fast but unmaintainable across thousands of entry points. This chapter defines the middle path. It owns the in-memory-to-wire transform; the frame *header* layout, opcode space, and version negotiation are defined in the Protocol chapter (Ch.06), and handle/object mapping (turning a guest `VkBuffer` into a server-side handle) is the Handles chapter (Ch.11). We describe only the *encoding* of argument payloads here.

## 7.1 The two-class model

Every command argument falls into one of two classes, and the encoder is built around that split:

| Class | Examples | Strategy | Cost |
|-------|----------|----------|------|
| **POD / fixed-layout** | scalars, enums, `VkViewport`, `VkRect2D`, contiguous arrays of POD | reinterpret bytes directly (zero-copy) | memcpy of a known span |
| **Pointer-rich / variable** | structs with embedded pointers (`VkGraphicsPipelineCreateInfo`), `pNext` chains, NUL-terminated strings, count-prefixed slices, sparse handle tables | flatten into a self-contained byte region with relative offsets | walk + copy + fixup |

The vast majority of *argument bytes* by volume (vertex data, texture uploads, uniform buffer contents, command-buffer recording) are POD and ride the zero-copy path. The vast majority of *distinct call shapes* by count are pointer-rich (every `*CreateInfo` struct). So we optimize bytes for POD and developer-time for pointer-rich, and we use codegen (§7.6) to amortize the pointer-rich work.

## 7.2 Zero-copy POD encoding with bytemuck / zerocopy

For POD arguments the planned approach is to avoid any field-by-field serialization and instead reinterpret the existing C struct (already populated by the calling application) as a byte slice. Two crate families are candidates:

- **`bytemuck`** — trait-based (`Pod`, `Zeroable`, `AnyBitPattern`, `NoUninit`), `#[derive]` support, `cast_slice`/`bytes_of`/`try_from_bytes` returning `Result`. Lightweight, widely used, no proc-macro for the core casts. Note `NoUninit` forbids *any* padding hole (uninitialized bytes can't be exposed), so it is derivable only on the *generated* wire structs — which carry explicit zeroed pad fields (§7.3) — never on the application's own C struct, which may contain implementation padding.
- **`zerocopy`** — `IntoBytes`/`FromBytes`/`Immutable`/`KnownLayout`, richer alignment story, `Ref<B, T>` for borrowing a `T` out of a `&[u8]` with a checked or unchecked split. Stronger on the *decode* side because of `KnownLayout` and explicit unaligned-read support.

The proposed split: use `bytemuck` for the *encode* side (cheap `bytes_of`) and `zerocopy` for the *decode* side (`Ref::from_prefix` gives a borrowed, alignment-checked view into the receive buffer without copying). Both can coexist; a type can derive the marker traits of both. The encoder API is a thin trait so the choice stays swappable:

```rust
/// A POD command argument whose in-memory layout *is* its wire layout.
///
/// # Safety
/// Implementors must be `#[repr(C)]` (or transparent) **generated** wire
/// structs with explicit, zeroed pad fields so there are no implicit padding
/// holes (this is what makes `bytemuck::NoUninit` sound — see §7.3), and must
/// be valid for any bit pattern that survives decode validation. The
/// application's raw C struct is *not* a `WirePod`; the generator emits a
/// distinct wire struct for it.
pub unsafe trait WirePod: Copy + 'static {
    /// Compile-time wire size in bytes (== size_of::<Self>()).
    const WIRE_SIZE: usize;
}

#[inline]
pub fn encode_pod<T: WirePod + bytemuck::NoUninit>(dst: &mut FrameWriter, v: &T) {
    dst.push_bytes(bytemuck::bytes_of(v)); // single memcpy, no per-field work
}

#[inline]
pub fn decode_pod<'a, T>(src: &mut FrameReader<'a>) -> Result<&'a T, ProtocolError>
where
    T: zerocopy::FromBytes + zerocopy::KnownLayout + zerocopy::Immutable,
{
    // borrows out of the receive buffer; no allocation, alignment-checked
    src.take_ref::<T>()
}
```

`encode_pod` is one `memcpy`; `decode_pod` is a pointer adjustment plus an alignment/length check. On the ivshmem bulk path the encoder can go further and *not even memcpy* into a frame — it can hand the transport the original pointer span (the application's vertex buffer) and let the transport place it directly in shared memory. That is the single biggest performance lever serialization has, and it only works for POD spans, which is the other reason the two-class split matters.

## 7.3 Layout, alignment, endianness

**`#[repr(C)]` everywhere, with explicit pad fields.** Every wire-mapped struct in `graftx-protocol` will be `#[repr(C)]` (or `#[repr(transparent)]` for newtypes), and every alignment gap is materialized as an *explicit* zeroed pad field (e.g. `_pad0: [u8; 4]`) rather than left as implicit `#[repr(C)]` padding. This is what lets the generator derive `bytemuck::NoUninit` on the wire struct: with no implicit padding holes there are no uninitialized bytes to leak, and the encode-side `bytes_of` is sound. The application's raw C struct is never used directly for this; the generator emits a separate wire struct (or asserts the app struct already has no holes for the simplest POD). `repr(Rust)` is forbidden for wire types because the field order and padding are unspecified and may differ between compiler versions — and the client (built for Linux) and server (built for Windows) are *different compilations*. `#[repr(C)]` makes both agree on C ABI layout rules, which both targets implement identically for the integer/float types we use.

**Alignment is the hazard, not endianness.** GraftX is a same-host, guest-to-guest design: both guests run on the same x86-64 physical CPU, so both are little-endian. We therefore *define the wire as little-endian* and assert it rather than byte-swap. A `const` guard documents and enforces the assumption:

```rust
const _: () = assert!(cfg!(target_endian = "little"), "GraftX wire format is LE-only");
```

This is a deliberate breadth/perf-over-portability tradeoff: byte-swapping every field would defeat zero-copy entirely (you can no longer reinterpret the C struct; you must rewrite each field). If a big-endian guest is ever required, it would get a non-zero-copy fallback codec selected at handshake, not a change to the LE wire definition. The cost of the assumption is exactly zero today.

Alignment is the real problem. A receive buffer is a `&[u8]` whose start address is whatever the transport allocator gave us; reinterpreting `&buf[7..]` as a `&VkViewport` (4-byte alignment) is UB. Self-alignment of fields *within* a frame is necessary but not sufficient — the frame *base* must itself be over-aligned, otherwise every relative offset inherits the base's misalignment. The decode buffer is therefore allocated from an **over-aligned arena**: the transport hands the decoder a region whose base is 16-byte aligned (the max alignment we encode, the Transport chapter (Ch.08) owns the arena allocation), so a frame placed at the arena base keeps every interior field at its natural alignment. When a frame cannot be guaranteed 16-aligned in place (e.g. it straddles a ring wrap and was reassembled), the decoder copies it once into a 16-byte-aligned scratch buffer before borrowing typed views from it. Three layered defenses build on that:

1. **Self-alignment of frames.** The frame writer keeps a running offset and inserts padding so each POD field begins at its natural alignment *relative to frame start*, and frame start is over-aligned to 16 bytes (max alignment we encode) by the over-aligned arena above. Padding bytes are zero-filled (so they are reproducible and don't leak server memory on the return path).
2. **Checked decode into aligned memory.** `zerocopy::Ref` / `bytemuck::try_from_bytes` return `Err` on misalignment instead of UB; with the over-aligned arena (or the aligned scratch copy) the check passes by construction, and a failure means a genuine protocol bug. The decoder treats it as `ProtocolError::Misaligned` (a new variant alongside `UnexpectedEof`/`UnknownOpcode`).
3. **Unaligned read fallback.** For rare fields we cannot align cheaply, use `zerocopy`'s unaligned types or `ptr::read_unaligned` behind a `// SAFETY:` note — correct but slower, used only where alignment padding would bloat the frame.

```text
frame layout (offsets relative to a 16-byte-aligned frame base):

  +0   header (Protocol chapter Ch.06, fixed size, 16B-aligned)
  +H   arg0 : u32 opcode operand          (align 4)
  +..  pad to align 8
  +..  arg1 : VkDeviceSize (u64)           (align 8)
  +..  arg2 : [VkViewport; n] POD span     (align 4, len = 4*4*n)
  +..  pad to align 4
  +..  varlen region: pNext chain, strings (relative offsets)
  +..  pad to 16  (so the *next* frame is aligned)
```

A decision worth recording: we pad *inside* frames rather than 1-byte-pack everything and force unaligned reads. Padding wastes a few bytes per frame; unaligned reads waste cycles on every field. Since serialization is hot, we pay bytes (cheap on a shared-memory or vsock link) to save cycles.

## 7.4 Variable-length and pointer-rich structures

C GPU APIs are full of pointers, and pointers are meaningless across the address-space boundary. The encoder *flattens* a pointer-rich structure into a single self-contained byte region using **relative offsets**: every pointer in the source struct becomes a `u32` offset (from the start of the varlen region) plus a `u32` length, and the pointed-to bytes are appended to the region. A null pointer encodes as a sentinel offset (`u32::MAX`) so it round-trips faithfully — null vs. empty-but-non-null is semantically distinct in several APIs (`pNext = NULL` vs an empty chain).

```rust
/// Encodes a borrowed slice of POD as (offset,len) into the varlen region.
fn encode_slice<T: WirePod + bytemuck::NoUninit>(
    region: &mut VarLenRegion,
    slice: Option<&[T]>,
) -> SliceRef {
    match slice {
        None => SliceRef::NULL,
        Some(s) => {
            let off = region.align_and_offset(align_of::<T>());
            region.extend(bytemuck::cast_slice(s));
            SliceRef { off, len: s.len() as u32 }
        }
    }
}
```

**Strings.** C strings (`const char*`, e.g. extension names, shader entry-point names) encode as a length-prefixed span; we store the explicit length and keep the trailing NUL so the server can hand the pointer straight to the native driver without re-copying. The decoder validates that the declared length stays inside the frame and (for strings) that a NUL exists where claimed.

**`pNext` chains.** Vulkan's extensibility chains are linked lists of heterogeneous structs discriminated by an `sType`. They are encoded as a *flattened sequence*: each node is `{ sType: u32, size: u32, body... , next_off: u32 }`. The encoder walks the guest chain, emits each node into the varlen region, and rewrites `next` as a relative offset; the decoder rebuilds a server-side chain by allocating server-private node storage and relinking pointers. Unknown `sType` values are *dropped* on decode (with a counter) rather than rejected, because driver-specific extension structs vary by host — but a config flag can flip this to strict-reject for fuzzing.

**Recursion and depth limits.** Some structs nest (a `pNext` node can itself contain pointers). The encoder is recursive with an explicit depth cap; the decoder enforces the *same* cap. Exceeding it is `ProtocolError::DepthExceeded`. This is a security control, not just hygiene — the server replays an untrusted stream, and an attacker-crafted frame must not be able to drive unbounded recursion or allocation. All length and offset fields are validated against the frame's actual byte length before any dereference (see §7.5).

## 7.5 Safety of casting and untrusted decode

Encode-side casts are safe by construction: we only ever reinterpret structs *we* declared `WirePod`, and the data is owned by the calling application. The dangerous direction is **decode**, because the byte buffer originates from an untrusted guest (the threat model: the server replays an untrusted command stream).

The rules, enforced in `graftx-protocol`:

- **No `unsafe` transmute on decode.** All POD decode goes through `zerocopy`/`bytemuck` *checked* APIs that return `Result`. There is exactly one place `unsafe` is permitted — `ptr::read_unaligned` for the documented unaligned fallback — and it carries a `// SAFETY:` comment proving the source span length and provenance. This honors the repo rule that all unsafe/FFI is isolated and commented.
- **Validate before dereference.** Every `(offset, len)` pair is checked: `offset.checked_add(len) <= region.len()`, alignment satisfied, and for typed slices `len % size_of::<T>() == 0`. Failures map to `ProtocolError` variants, never panics. No `unwrap` in library paths (repo rule).
- **Bit-pattern validity.** `bytemuck::Pod` requires *all* bit patterns be valid, which is true for integers/floats but **false for enums** with a restricted set of discriminants. Wire-decoded enums therefore decode as their underlying `u32`/`i32` and convert through a checked `TryFrom` (codegen-generated, §7.6); an out-of-range enum is `ProtocolError::InvalidEnum { opcode, field }`, not UB. This is the most common real-world hazard with a naive zero-copy scheme and the one we most explicitly guard.
- **Copy-out on the bulk path.** Per the security design, validated data on the ivshmem bulk plane is copied into server-private memory *after* validation and *before* the native driver touches it, because write-revocation cannot be enforced from the server side — a malicious client could mutate shared memory after the validation check (a TOCTOU). The decoder returns owned `Vec`/`Box` for anything the driver will read asynchronously; only synchronously-consumed POD may stay borrowed.

```rust
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("unexpected end of buffer")]                 UnexpectedEof,
    #[error("unknown opcode: {0}")]                      UnknownOpcode(u32),
    #[error("misaligned access for {ty} at offset {off}")] Misaligned { ty: &'static str, off: usize },
    #[error("offset {off}+{len} exceeds region {cap}")]  OutOfBounds { off: usize, len: usize, cap: usize },
    #[error("invalid enum value {val} for {field}")]     InvalidEnum { val: u32, field: &'static str },
    #[error("nesting depth exceeded ({max})")]           DepthExceeded { max: u32 },
}
```

A `FrameReader` wraps the borrowed buffer and a cursor and centralizes the checks so individual decoders cannot forget them:

```rust
pub struct FrameReader<'a> { buf: &'a [u8], pos: usize }

impl<'a> FrameReader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        let end = self.pos.checked_add(n).ok_or(ProtocolError::UnexpectedEof)?;
        let s = self.buf.get(self.pos..end).ok_or(ProtocolError::UnexpectedEof)?;
        self.pos = end; Ok(s)
    }
    fn take_ref<T: zerocopy::FromBytes + zerocopy::KnownLayout + zerocopy::Immutable>(
        &mut self,
    ) -> Result<&'a T, ProtocolError> {
        let need = size_of::<T>();
        let rem = &self.buf[self.pos..];
        // Distinguish the two distinct failure modes `from_prefix` collapses:
        //   - too few bytes  -> truncation (UnexpectedEof / OutOfBounds)
        //   - bytes present but base misaligned -> Misaligned
        if rem.len() < need {
            return Err(ProtocolError::OutOfBounds { off: self.pos, len: need, cap: self.buf.len() });
        }
        let (r, rest) = zerocopy::Ref::<_, T>::from_prefix(rem)
            .map_err(|_| ProtocolError::Misaligned { ty: type_name::<T>(), off: self.pos })?;
        self.pos = self.buf.len() - rest.len();
        Ok(zerocopy::Ref::into_ref(r))
    }
}
```

## 7.6 Codegen vs. hand-written encoders

With thousands of entry points, hand-writing each encoder/decoder is infeasible and error-prone, and hand-written code is exactly where validation bugs hide. The proposed strategy is **generate the boring 90%, hand-write the dangerous 10%.**

**Source of truth.** The GPU APIs publish machine-readable specs: Vulkan (`vk.xml`), OpenGL/GLES/EGL/GLX (the Khronos `gl.xml`/`egl.xml` registries), OpenCL headers, and for CUDA/HIP/Level Zero the C headers (parsed via `bindgen`-style extraction). A build-time generator (a `build.rs` or, preferably, an offline `xtask` whose output is checked into the repo for reviewability and reproducible builds) consumes these and emits, per command:

- the `#[repr(C)]` wire struct(s) and their marker-trait derives,
- the `encode_<cmd>` function (POD fields via `encode_pod`, pointer params via `encode_slice`/string/chain helpers),
- the `decode_<cmd>` function with all bounds/enum/depth checks,
- the `TryFrom<u32>` for each enum,
- a dispatch-table entry keyed by opcode.

Generated code is committed (not generated on every build) so reviewers can diff it, fuzzers see stable targets, and a registry change produces an explicit, auditable PR. The generator emits `rustfmt`-clean, `clippy -D warnings`-clean code (repo rules).

**Hand-written escapes.** Some calls cannot be mechanically derived and get a hand-written override the generator defers to:

| Case | Why generation fails |
|------|----------------------|
| `void*` of dynamic size (`vkCmdUpdateBuffer` data, `glBufferData`) | size comes from a *sibling* argument; needs a per-call rule |
| Union members (`VkClearValue`, `VkPipelineExecutableStatistic`) | discriminant lives in another field |
| Output params (`pProperties` two-call enumerate idiom) | direction is semantic, not in the type |
| Callbacks / function pointers | cannot cross the boundary; need stub registration (the Handles chapter, Ch.11) |
| `pNext` node bodies with their own pointers | needs the recursive chain walker, registered per `sType` |

The override mechanism: the generator checks a per-command hand-written-overrides manifest; if a command (or a specific argument) is listed, it emits a call to the hand-written function instead of the derived body. This keeps the generated/hand-written boundary explicit and prevents regeneration from clobbering hand-tuned code.

**Tradeoff table.**

| Approach | Breadth | Perf | Maint. | Safety review surface |
|----------|---------|------|--------|----------------------|
| All hand-written | low (slow to add) | high | terrible | huge (every fn) |
| All reflection/serde | high | low (alloc+copy) | great | small but opaque |
| **Codegen + POD zero-copy (chosen)** | high (regen) | high (POD fast path) | good | medium, concentrated in generator + overrides |

The chosen approach concentrates the safety-critical logic in *one* generator and a small override set, which is the right place to focus fuzzing and code review.

## 7.7 Versioned structs and evolution

`PROTOCOL_VERSION` (the Protocol chapter, Ch.06) is `0` (pre-stable) and bumps on every breaking wire change until freeze. Serialization participates in versioning at two granularities:

- **Whole-protocol version**, negotiated at handshake — if peers disagree beyond a supported range, the session is refused. Generated code is tagged with the registry version it was built from.
- **Per-struct evolution.** New API versions append fields to existing structs. Since wire structs are `#[repr(C)]` and length-prefixed at the frame level, an *older decoder* reading a *newer frame* can be allowed to ignore trailing bytes, and a *newer decoder* reading an *older frame* must detect the short length and treat absent trailing fields as their documented defaults. The decoder therefore never assumes a struct fills the remaining buffer; it reads exactly `min(declared_len, known_size)` and zero-fills the rest. This gives forward/backward tolerance within a major protocol version without per-field version tags.

For genuinely incompatible changes (a field's *meaning* changes, an enum value is reused), we bump `PROTOCOL_VERSION` and gate the new encoder behind the negotiated version. The generator can emit multiple encoder versions side by side, selected by a function pointer chosen once at handshake, so the hot path has no per-frame version branch.

The net design: POD rides a single-memcpy zero-copy fast path with little-endian, `#[repr(C)]`, alignment-padded frames; pointer-rich data is flattened with validated relative offsets; decode is always checked and never UB on hostile input; and the whole surface is generated from the published API registries with a small, auditable hand-written override set. This is the encoding contract the client and server must honor exactly (the Protocol chapter (Ch.06), the Transport chapter (Ch.08), and the Server-core/decode chapter (Ch.10) build directly on it).
