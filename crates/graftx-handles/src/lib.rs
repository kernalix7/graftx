//! Generational handle table.
//!
//! A [`HandleTable`] maps opaque 64-bit wire [`Handle`]s (see
//! [`graftx_protocol::Handle`]) to server-side resources of type `T`. Each
//! storage slot tracks a *generation* counter that is bumped every time the
//! slot is freed. A handle carries the generation that was current when it was
//! issued, so a stale handle pointing at a slot that has since been reused no
//! longer matches and is rejected — this is the use-after-free defense.
//!
//! When a slot's generation would exceed [`Handle::GENERATION_MAX`] it is
//! *retired*: its value is cleared but the slot is never added back to the
//! free-list, so the generation can never wrap around to a value that an old,
//! still-circulating handle holds.

#![forbid(unsafe_code)]

use graftx_protocol::Handle;
use thiserror::Error;

/// Errors that can arise when operating on a [`HandleTable`].
///
/// The primary `get`/`get_mut`/`remove` accessors use an `Option` API, but this
/// enum is provided for callers that want to distinguish *why* a handle failed
/// to resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HandleError {
    /// The slot index encoded in the handle is outside the table.
    #[error("handle slot index {0} is out of range")]
    SlotOutOfRange(u32),
    /// The slot is empty (the value was removed).
    #[error("handle slot {0} is empty")]
    EmptySlot(u32),
    /// The handle's generation does not match the slot's current generation,
    /// indicating a stale handle (use-after-free).
    #[error("handle slot {slot} generation mismatch: handle={handle}, slot={current}")]
    GenerationMismatch {
        /// The slot index.
        slot: u32,
        /// The generation carried by the (stale) handle.
        handle: u32,
        /// The generation currently held by the slot.
        current: u32,
    },
}

/// A single storage slot in the table.
struct Slot<T> {
    /// The generation currently associated with this slot. A handle resolves
    /// only if it carries this exact value.
    generation: u32,
    /// The `kind` byte the live value was inserted under. Recorded so a live
    /// slot's [`Handle`] can be reconstructed for read-only introspection
    /// (see [`HandleTable::iter`]). Meaningful only while `value` is `Some`.
    kind: u8,
    /// The stored value, or `None` if the slot is free or retired.
    value: Option<T>,
    /// Once `true`, the slot's generation has been exhausted and it is never
    /// reused (kept out of the free-list).
    retired: bool,
}

/// A generational table mapping [`Handle`]s to values of type `T`.
///
/// See the [module documentation](crate) for the generational scheme.
pub struct HandleTable<T> {
    slots: Vec<Slot<T>>,
    /// Indices of free, reusable (non-retired) slots.
    free: Vec<u32>,
    /// Number of live (occupied) slots.
    live: usize,
}

impl<T> HandleTable<T> {
    /// Create an empty table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            live: 0,
        }
    }

    /// Insert `value` and return a fresh [`Handle`] tagged with `kind`.
    ///
    /// A free, non-retired slot is reused when one is available; otherwise a new
    /// slot is appended. The returned handle is stamped with the slot's current
    /// generation so that it remains valid only until the slot is freed.
    pub fn insert(&mut self, kind: u8, value: T) -> Handle {
        self.live += 1;
        if let Some(slot_idx) = self.free.pop() {
            // Reuse a free slot. Its generation was already bumped on removal.
            let slot = &mut self.slots[slot_idx as usize];
            slot.value = Some(value);
            slot.kind = kind;
            Handle::new(kind, slot.generation, slot_idx)
        } else {
            let slot_idx = self.slots.len() as u32;
            self.slots.push(Slot {
                generation: 0,
                kind,
                value: Some(value),
                retired: false,
            });
            Handle::new(kind, 0, slot_idx)
        }
    }

    /// Resolve `h` to a shared reference, validating slot range, occupancy, and
    /// generation. Returns `None` for an out-of-range, empty, or stale handle.
    #[must_use]
    pub fn get(&self, h: Handle) -> Option<&T> {
        let slot = self.slots.get(h.slot() as usize)?;
        if slot.generation != h.generation() {
            return None;
        }
        slot.value.as_ref()
    }

    /// Resolve `h` to a mutable reference. See [`get`](Self::get) for the
    /// validation rules.
    #[must_use]
    pub fn get_mut(&mut self, h: Handle) -> Option<&mut T> {
        let slot = self.slots.get_mut(h.slot() as usize)?;
        if slot.generation != h.generation() {
            return None;
        }
        slot.value.as_mut()
    }

    /// Remove and return the value referenced by `h`.
    ///
    /// The handle is validated exactly as in [`get`](Self::get). On success the
    /// slot's generation is bumped; if the new generation would exceed
    /// [`Handle::GENERATION_MAX`] the slot is *retired* and not returned to the
    /// free-list. Otherwise the slot is pushed onto the free-list for reuse.
    pub fn remove(&mut self, h: Handle) -> Option<T> {
        let slot_idx = h.slot();
        let slot = self.slots.get_mut(slot_idx as usize)?;
        if slot.generation != h.generation() {
            return None;
        }
        let value = slot.value.take()?;
        self.live -= 1;

        if slot.generation >= Handle::GENERATION_MAX {
            // Generation exhausted: retire the slot, never reuse it.
            slot.retired = true;
        } else {
            slot.generation += 1;
            self.free.push(slot_idx);
        }
        Some(value)
    }

    /// The number of live (occupied) entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.live
    }

    /// Whether the table holds no live entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Iterate over every live entry as `(handle, &value)` pairs.
    ///
    /// Empty and retired slots are skipped. Each yielded [`Handle`] is
    /// reconstructed from the live slot's recorded `kind`, current generation,
    /// and slot index, so it resolves via [`get`](Self::get) for as long as the
    /// slot is not freed. Iteration order follows the underlying slot indices
    /// and is otherwise unspecified.
    pub fn iter(&self) -> impl Iterator<Item = (Handle, &T)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(slot_idx, slot)| {
                let value = slot.value.as_ref()?;
                let handle = Handle::new(slot.kind, slot.generation, slot_idx as u32);
                Some((handle, value))
            })
    }

    /// Count the live entries whose handle carries the given `kind` byte.
    ///
    /// Empty and retired slots are not counted.
    #[must_use]
    pub fn count_by_kind(&self, kind: u8) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.value.is_some() && slot.kind == kind)
            .count()
    }

    /// The total number of storage slots allocated by the table.
    ///
    /// This counts every slot — live, free, and retired — and so never shrinks
    /// over the table's lifetime except via [`clear`](Self::clear), which leaves
    /// the slot vector intact (so `capacity` is unchanged by `clear`). It is
    /// always `>= len()`.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// The number of slots that have been permanently retired.
    ///
    /// A slot is retired when its generation reaches [`Handle::GENERATION_MAX`]
    /// and it is freed: it is never returned to the free-list, so the generation
    /// can never wrap to a value an old handle still holds. Retired slots hold no
    /// value and are never reused.
    #[must_use]
    pub fn retired(&self) -> usize {
        self.slots.iter().filter(|slot| slot.retired).count()
    }

    /// Drop every live value and reset the table to hold no live entries.
    ///
    /// # Semantics
    ///
    /// Each currently-occupied slot is freed exactly as [`remove`](Self::remove)
    /// would free it: its value is dropped and its generation is bumped (or, if
    /// the generation is already exhausted, the slot is retired). Bumping the
    /// generation guarantees that every handle issued before the `clear` — for
    /// every slot, not just the live ones — is now stale and resolves to `None`,
    /// so no future handle can alias a pre-clear handle. Freed (non-retired)
    /// slots are returned to the free-list and reused by later inserts.
    ///
    /// The underlying slot storage is retained, so [`capacity`](Self::capacity)
    /// is unchanged; only [`len`](Self::len) is reset to `0`.
    pub fn clear(&mut self) {
        for (slot_idx, slot) in self.slots.iter_mut().enumerate() {
            if slot.value.take().is_none() {
                // Already free or retired: leave its generation and free-list
                // membership untouched so existing bookkeeping stays correct.
                continue;
            }
            // Mirror `remove`'s generational bookkeeping for the live slot.
            if slot.generation >= Handle::GENERATION_MAX {
                slot.retired = true;
            } else {
                slot.generation += 1;
                self.free.push(slot_idx as u32);
            }
        }
        self.live = 0;
    }
}

impl<T> Default for HandleTable<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIND: u8 = 0x42;

    #[test]
    fn insert_get_roundtrip() {
        let mut table: HandleTable<u32> = HandleTable::new();
        let h = table.insert(KIND, 1234);
        assert_eq!(h.kind(), KIND);
        assert_eq!(h.slot(), 0);
        assert_eq!(h.generation(), 0);
        assert_eq!(table.get(h), Some(&1234));
        assert_eq!(table.get_mut(h), Some(&mut 1234));
    }

    #[test]
    fn get_mut_mutates_in_place() {
        let mut table: HandleTable<u32> = HandleTable::new();
        let h = table.insert(KIND, 1);
        if let Some(v) = table.get_mut(h) {
            *v = 99;
        }
        assert_eq!(table.get(h), Some(&99));
    }

    #[test]
    fn remove_then_get_returns_none() {
        let mut table: HandleTable<&str> = HandleTable::new();
        let h = table.insert(KIND, "value");
        assert_eq!(table.remove(h), Some("value"));
        assert_eq!(table.get(h), None);
        assert_eq!(table.get_mut(h), None);
        // Double remove is a no-op.
        assert_eq!(table.remove(h), None);
    }

    #[test]
    fn stale_handle_after_reuse_returns_none() {
        // Use-after-free defense: an old handle must not resolve once its slot
        // has been freed and reissued under a new generation.
        let mut table: HandleTable<u32> = HandleTable::new();
        let old = table.insert(KIND, 10);
        assert_eq!(table.remove(old), Some(10));

        // The freed slot is reused; the new handle shares the slot index but
        // carries a bumped generation.
        let fresh = table.insert(KIND, 20);
        assert_eq!(fresh.slot(), old.slot());
        assert_ne!(fresh.generation(), old.generation());

        // Stale handle resolves to nothing; fresh handle works.
        assert_eq!(table.get(old), None);
        assert_eq!(table.get_mut(old), None);
        assert_eq!(table.remove(old), None);
        assert_eq!(table.get(fresh), Some(&20));
    }

    #[test]
    fn slot_reuse_increments_generation() {
        let mut table: HandleTable<u32> = HandleTable::new();
        let h0 = table.insert(KIND, 0);
        assert_eq!(h0.generation(), 0);

        let h1 = {
            assert_eq!(table.remove(h0), Some(0));
            table.insert(KIND, 1)
        };
        assert_eq!(h1.slot(), h0.slot());
        assert_eq!(h1.generation(), 1);

        let h2 = {
            assert_eq!(table.remove(h1), Some(1));
            table.insert(KIND, 2)
        };
        assert_eq!(h2.slot(), h0.slot());
        assert_eq!(h2.generation(), 2);
    }

    #[test]
    fn len_tracks_live_count() {
        let mut table: HandleTable<u32> = HandleTable::new();
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);

        let a = table.insert(KIND, 1);
        let b = table.insert(KIND, 2);
        let c = table.insert(KIND, 3);
        assert_eq!(table.len(), 3);
        assert!(!table.is_empty());

        assert_eq!(table.remove(b), Some(2));
        assert_eq!(table.len(), 2);

        // Removing a stale/empty handle does not change the count.
        assert_eq!(table.remove(b), None);
        assert_eq!(table.len(), 2);

        // Reusing the freed slot brings the count back up.
        let d = table.insert(KIND, 4);
        assert_eq!(table.len(), 3);
        assert_eq!(d.slot(), b.slot());

        assert_eq!(table.remove(a), Some(1));
        assert_eq!(table.remove(c), Some(3));
        assert_eq!(table.remove(d), Some(4));
        assert!(table.is_empty());
    }

    #[test]
    fn distinct_slots_for_concurrent_live_entries() {
        let mut table: HandleTable<u32> = HandleTable::new();
        let a = table.insert(KIND, 1);
        let b = table.insert(KIND, 2);
        assert_ne!(a.slot(), b.slot());
        assert_eq!(table.get(a), Some(&1));
        assert_eq!(table.get(b), Some(&2));
    }

    #[test]
    fn exhausted_slot_is_retired_not_reused() {
        // Drive a single slot up to GENERATION_MAX and confirm that the final
        // removal retires it instead of returning it to the free-list.
        let mut table: HandleTable<u32> = HandleTable::new();

        // Insert/remove until the slot's generation reaches GENERATION_MAX.
        let mut last = table.insert(KIND, 0);
        while last.generation() < Handle::GENERATION_MAX {
            assert_eq!(table.remove(last), Some(0));
            last = table.insert(KIND, 0);
            // Same slot keeps being reused as long as it is not retired.
            assert_eq!(last.slot(), 0);
        }
        assert_eq!(last.generation(), Handle::GENERATION_MAX);

        // This removal exhausts the generation and retires slot 0.
        assert_eq!(table.remove(last), Some(0));
        assert!(table.free.is_empty(), "exhausted slot must not be freed");
        assert!(table.slots[0].retired, "exhausted slot must be retired");

        // The next insert allocates a brand-new slot rather than slot 0.
        let next = table.insert(KIND, 7);
        assert_eq!(next.slot(), 1);
        assert_eq!(next.generation(), 0);
        assert_eq!(table.get(next), Some(&7));

        // The retired handle never resolves.
        assert_eq!(table.get(last), None);
    }

    const KIND_A: u8 = 0x11;
    const KIND_B: u8 = 0x22;

    #[test]
    fn iter_yields_only_live_slots_with_roundtripping_handles() {
        let mut table: HandleTable<u32> = HandleTable::new();
        let a = table.insert(KIND_A, 1);
        let b = table.insert(KIND_B, 2);
        let c = table.insert(KIND_A, 3);

        // Collect (handle, value) pairs and confirm every yielded handle
        // resolves back to the same value via get().
        let mut seen: Vec<(u64, u32)> = table
            .iter()
            .map(|(h, &v)| {
                assert_eq!(table.get(h), Some(&v));
                (h.raw(), v)
            })
            .collect();
        seen.sort_unstable();

        let mut expected = vec![(a.raw(), 1), (b.raw(), 2), (c.raw(), 3)];
        expected.sort_unstable();
        assert_eq!(seen, expected);
    }

    #[test]
    fn iter_and_count_by_kind_reflect_removal() {
        let mut table: HandleTable<u32> = HandleTable::new();
        let a = table.insert(KIND_A, 1);
        let b = table.insert(KIND_B, 2);
        let _c = table.insert(KIND_A, 3);

        assert_eq!(table.iter().count(), 3);
        assert_eq!(table.count_by_kind(KIND_A), 2);
        assert_eq!(table.count_by_kind(KIND_B), 1);

        // Remove one KIND_A entry; iter() and the counts drop accordingly.
        assert_eq!(table.remove(a), Some(1));
        assert_eq!(table.iter().count(), 2);
        assert_eq!(table.count_by_kind(KIND_A), 1);
        assert_eq!(table.count_by_kind(KIND_B), 1);

        // The removed handle no longer appears among the live entries.
        assert!(table.iter().all(|(h, _)| h != a));

        // Remove the remaining KIND_B entry.
        assert_eq!(table.remove(b), Some(2));
        assert_eq!(table.iter().count(), 1);
        assert_eq!(table.count_by_kind(KIND_A), 1);
        assert_eq!(table.count_by_kind(KIND_B), 0);
    }

    #[test]
    fn count_by_kind_ignores_absent_kind_and_empty_table() {
        let mut table: HandleTable<u32> = HandleTable::new();
        assert_eq!(table.count_by_kind(KIND_A), 0);
        assert_eq!(table.iter().count(), 0);

        let _h = table.insert(KIND_A, 7);
        // A kind that was never inserted has no live entries.
        assert_eq!(table.count_by_kind(KIND_B), 0);
        assert_eq!(table.count_by_kind(KIND_A), 1);
    }

    #[test]
    fn reused_slot_reports_new_kind_in_iter_and_counts() {
        // A freed slot reused under a different kind must report the new kind,
        // not the stale one recorded by the prior occupant.
        let mut table: HandleTable<u32> = HandleTable::new();
        let a = table.insert(KIND_A, 1);
        assert_eq!(table.remove(a), Some(1));

        let b = table.insert(KIND_B, 2);
        assert_eq!(b.slot(), a.slot());

        assert_eq!(table.count_by_kind(KIND_A), 0);
        assert_eq!(table.count_by_kind(KIND_B), 1);

        let live: Vec<(u8, u32)> = table.iter().map(|(h, &v)| (h.kind(), v)).collect();
        assert_eq!(live, vec![(KIND_B, 2)]);
    }

    #[test]
    fn capacity_grows_with_distinct_inserts_and_outpaces_len() {
        let mut table: HandleTable<u32> = HandleTable::new();
        assert_eq!(table.capacity(), 0);

        let a = table.insert(KIND, 1);
        assert_eq!(table.capacity(), 1);
        let _b = table.insert(KIND, 2);
        assert_eq!(table.capacity(), 2);
        assert_eq!(table.capacity(), table.len());

        // Freeing a slot does not shrink capacity, and reusing the freed slot
        // does not grow it: capacity tracks allocated slots, not live ones.
        assert_eq!(table.remove(a), Some(1));
        assert_eq!(table.capacity(), 2);
        assert_eq!(table.len(), 1);
        let _c = table.insert(KIND, 3);
        assert_eq!(table.capacity(), 2);
        assert!(table.capacity() >= table.len());
    }

    #[test]
    fn retired_starts_zero_and_counts_exhausted_slots() {
        let mut table: HandleTable<u32> = HandleTable::new();
        assert_eq!(table.retired(), 0);

        // Ordinary insert/remove cycles never retire a slot.
        let h = table.insert(KIND, 1);
        assert_eq!(table.remove(h), Some(1));
        assert_eq!(table.retired(), 0);

        // Drive a single slot to GENERATION_MAX, then the final removal retires
        // it; `retired()` must observe that one slot.
        let mut last = table.insert(KIND, 0);
        while last.generation() < Handle::GENERATION_MAX {
            assert_eq!(table.remove(last), Some(0));
            last = table.insert(KIND, 0);
        }
        assert_eq!(table.retired(), 0);
        assert_eq!(table.remove(last), Some(0));
        assert_eq!(table.retired(), 1);
    }

    #[test]
    fn clear_empties_table_invalidates_handles_and_allows_reuse() {
        let mut table: HandleTable<u32> = HandleTable::new();
        let a = table.insert(KIND_A, 1);
        let b = table.insert(KIND_B, 2);
        let c = table.insert(KIND_A, 3);
        assert_eq!(table.len(), 3);
        let cap_before = table.capacity();

        table.clear();

        // The table reports no live entries and no live kinds.
        assert_eq!(table.len(), 0);
        assert!(table.is_empty());
        assert_eq!(table.iter().count(), 0);
        assert_eq!(table.count_by_kind(KIND_A), 0);
        assert_eq!(table.count_by_kind(KIND_B), 0);
        // Slot storage is retained: capacity is unchanged by clear.
        assert_eq!(table.capacity(), cap_before);

        // Every pre-clear handle is now stale.
        assert_eq!(table.get(a), None);
        assert_eq!(table.get(b), None);
        assert_eq!(table.get(c), None);
        assert_eq!(table.get_mut(a), None);
        assert_eq!(table.remove(a), None);

        // New inserts work and do not alias any pre-clear handle.
        let d = table.insert(KIND_A, 10);
        let e = table.insert(KIND_B, 20);
        assert_eq!(table.len(), 2);
        assert_eq!(table.get(d), Some(&10));
        assert_eq!(table.get(e), Some(&20));
        for old in [a, b, c] {
            assert_ne!(
                d.raw(),
                old.raw(),
                "reused handle aliases a pre-clear handle"
            );
            assert_ne!(
                e.raw(),
                old.raw(),
                "reused handle aliases a pre-clear handle"
            );
            // Pre-clear handles still resolve to nothing even after reuse.
            assert_eq!(table.get(old), None);
        }
    }

    #[test]
    fn clear_on_empty_table_is_a_noop() {
        let mut table: HandleTable<u32> = HandleTable::new();
        table.clear();
        assert_eq!(table.len(), 0);
        assert_eq!(table.capacity(), 0);
        assert_eq!(table.retired(), 0);

        // Clear after a full drain leaves the freed slots reusable.
        let h = table.insert(KIND, 1);
        assert_eq!(table.remove(h), Some(1));
        table.clear();
        assert_eq!(table.len(), 0);
        let h2 = table.insert(KIND, 2);
        assert_eq!(table.get(h2), Some(&2));
    }
}
