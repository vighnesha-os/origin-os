//! The block cache's index (CLAUDE.md §39 page cache, §40 the block layer): which cache
//! slot holds which block, LRU order, and what must be written back before a slot is
//! reused. Plain bookkeeping, no data buffers and no `unsafe`, tested on the host; the
//! kernel (`src/block.rs`) owns the slots' memory and does the I/O.
//!
//! The index maps a block number to one of `capacity` slots. A lookup that hits moves the
//! slot to the most-recently-used end. A miss chooses the least-recently-used slot, and if
//! that slot is dirty the caller must write it back (to its old block) before filling it
//! with the new one. Slots start empty and are filled before any eviction.

/// A slot's position in the cache.
pub type Slot = usize;

/// What a block number maps to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lookup {
    /// The block is cached in this slot.
    Hit(Slot),
    /// The block is not cached; use this slot, which is empty.
    Fill(Slot),
    /// The block is not cached; use this slot after writing its current block
    /// (`evicted`) back, because it is dirty.
    Evict { slot: Slot, evicted: u64 },
    /// The block is not cached; use this slot, whose current block is clean and may be
    /// dropped.
    Replace { slot: Slot, evicted: u64 },
}

impl Lookup {
    /// The slot to use, whatever the outcome.
    pub const fn slot(&self) -> Slot {
        match *self {
            Lookup::Hit(s)
            | Lookup::Fill(s)
            | Lookup::Evict { slot: s, .. }
            | Lookup::Replace { slot: s, .. } => s,
        }
    }

    /// Whether this was a cache hit.
    pub const fn hit(&self) -> bool {
        matches!(self, Lookup::Hit(_))
    }
}

/// What one slot holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    /// The block cached here, if any.
    block: Option<u64>,
    /// Changed since it was read, so it must be written back before eviction.
    dirty: bool,
    /// Newer and older neighbours in the LRU list (`NONE` at an end).
    newer: usize,
    older: usize,
}

/// The sentinel for "no neighbour".
const NONE: usize = usize::MAX;

/// A fixed-capacity LRU index over `capacity` slots.
#[derive(Debug)]
pub struct BlockCache {
    entries: alloc::vec::Vec<Entry>,
    /// Most- and least-recently-used slots (`NONE` when empty).
    newest: usize,
    oldest: usize,
    /// Slots never yet filled, newest first (a simple stack).
    free: alloc::vec::Vec<usize>,
    hits: u64,
    misses: u64,
}

impl BlockCache {
    /// An empty cache of `capacity` slots (at least one).
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            entries: alloc::vec![
                Entry {
                    block: None,
                    dirty: false,
                    newer: NONE,
                    older: NONE,
                };
                capacity
            ],
            newest: NONE,
            oldest: NONE,
            free: (0..capacity).rev().collect(),
            hits: 0,
            misses: 0,
        }
    }

    /// Slots in the cache.
    pub fn capacity(&self) -> usize {
        self.entries.len()
    }

    /// Hit and miss counts.
    pub const fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    /// The block cached in `slot`, if any.
    pub fn block_at(&self, slot: Slot) -> Option<u64> {
        self.entries.get(slot).and_then(|e| e.block)
    }

    /// Unlinks `slot` from the LRU list (it must be in it).
    fn unlink(&mut self, slot: usize) {
        let (newer, older) = {
            let e = &self.entries[slot];
            (e.newer, e.older)
        };
        if newer != NONE {
            self.entries[newer].older = older;
        } else {
            self.newest = older;
        }
        if older != NONE {
            self.entries[older].newer = newer;
        } else {
            self.oldest = newer;
        }
    }

    /// Links `slot` in at the most-recently-used end.
    fn link_newest(&mut self, slot: usize) {
        let old_newest = self.newest;
        {
            let e = &mut self.entries[slot];
            e.newer = NONE;
            e.older = old_newest;
        }
        if old_newest != NONE {
            self.entries[old_newest].newer = slot;
        } else {
            self.oldest = slot;
        }
        self.newest = slot;
    }

    /// Looks `block` up, recording a hit or miss and updating the LRU order. On a hit the
    /// caller may use the slot's data; on a miss the caller fills the slot with `block`'s
    /// data (first writing back an `Evict`'s dirty block), then calls
    /// [`BlockCache::filled`].
    pub fn access(&mut self, block: u64) -> Lookup {
        if let Some(slot) = self.find(block) {
            self.hits += 1;
            self.touch(slot);
            return Lookup::Hit(slot);
        }
        self.misses += 1;
        if let Some(slot) = self.free.pop() {
            return Lookup::Fill(slot);
        }
        // Evict the least-recently-used slot.
        let slot = self.oldest;
        let evicted = self.entries[slot].block.unwrap_or(0);
        if self.entries[slot].dirty {
            Lookup::Evict { slot, evicted }
        } else {
            Lookup::Replace { slot, evicted }
        }
    }

    /// Records that `slot` now holds `block` (read in, or written back then refilled) and
    /// is clean, and moves it to the most-recently-used end.
    pub fn filled(&mut self, slot: Slot, block: u64) {
        if slot >= self.entries.len() {
            return;
        }
        let in_list = self.entries[slot].block.is_some();
        if in_list {
            self.unlink(slot);
        }
        {
            let e = &mut self.entries[slot];
            e.block = Some(block);
            e.dirty = false;
        }
        self.link_newest(slot);
    }

    /// Marks `slot`'s block changed, so it is written back before it is evicted.
    pub fn mark_dirty(&mut self, slot: Slot) {
        if let Some(e) = self.entries.get_mut(slot)
            && e.block.is_some()
        {
            e.dirty = true;
        }
    }

    /// Records that `slot`'s block was written back: clean again.
    pub fn mark_clean(&mut self, slot: Slot) {
        if let Some(e) = self.entries.get_mut(slot) {
            e.dirty = false;
        }
    }

    /// Whether `slot` holds a block that has unwritten changes.
    pub fn is_dirty(&self, slot: Slot) -> bool {
        self.entries.get(slot).is_some_and(|e| e.dirty)
    }

    /// Every dirty slot and its block, for a flush, oldest first.
    pub fn dirty_slots(&self) -> alloc::vec::Vec<(Slot, u64)> {
        let mut out = alloc::vec::Vec::new();
        let mut slot = self.oldest;
        while slot != NONE {
            let e = &self.entries[slot];
            if e.dirty
                && let Some(block) = e.block
            {
                out.push((slot, block));
            }
            slot = e.newer;
        }
        out
    }

    fn find(&self, block: u64) -> Option<usize> {
        self.entries.iter().position(|e| e.block == Some(block))
    }

    /// Moves `slot` to the most-recently-used end.
    fn touch(&mut self, slot: usize) {
        if self.newest == slot {
            return;
        }
        self.unlink(slot);
        self.link_newest(slot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::collections::HashMap;

    /// A model of a cache over `blocks` blocks: the authoritative contents, checked against
    /// the index after every operation.
    struct Model {
        cache: BlockCache,
        /// Each slot's block and dirtiness, mirrored.
        slots: std::vec::Vec<Option<(u64, bool)>>,
    }

    impl Model {
        fn new(capacity: usize) -> Self {
            let cache = BlockCache::new(capacity);
            let slots = std::vec![None; cache.capacity()];
            Self { cache, slots }
        }

        /// One cache access for `block`: returns its slot and whether it hit, having
        /// written back and refilled as the lookup demanded.
        fn access(&mut self, block: u64) -> (Slot, bool) {
            let lookup = self.cache.access(block);
            let slot = lookup.slot();
            if lookup.hit() {
                assert_eq!(self.slots[slot].map(|(b, _)| b), Some(block));
                return (slot, true);
            }
            match lookup {
                Lookup::Fill(s) => assert_eq!(self.slots[s], None, "fill reuses an empty slot"),
                Lookup::Evict { slot: s, evicted } => {
                    assert_eq!(
                        self.slots[s],
                        Some((evicted, true)),
                        "evict names a dirty block"
                    );
                    self.cache.mark_clean(s); // written back
                }
                Lookup::Replace { slot: s, evicted } => {
                    assert_eq!(
                        self.slots[s],
                        Some((evicted, false)),
                        "replace names a clean block"
                    );
                }
                Lookup::Hit(_) => unreachable!(),
            }
            self.cache.filled(slot, block);
            self.slots[slot] = Some((block, false));
            (slot, false)
        }

        /// Reads `block`: returns whether it hit.
        fn read(&mut self, block: u64) -> bool {
            self.access(block).1
        }

        fn write(&mut self, block: u64) {
            let (slot, _) = self.access(block);
            self.cache.mark_dirty(slot);
            self.slots[slot] = Some((block, true));
        }

        /// Every slot's block matches the model, and no block is cached twice.
        fn check(&self) {
            let mut seen = HashMap::new();
            for (slot, entry) in self.slots.iter().enumerate() {
                assert_eq!(
                    self.cache.block_at(slot),
                    entry.map(|(b, _)| b),
                    "slot {slot}"
                );
                assert_eq!(
                    self.cache.is_dirty(slot),
                    entry.is_some_and(|(_, d)| d),
                    "slot {slot}"
                );
                if let Some((b, _)) = entry {
                    assert!(seen.insert(*b, slot).is_none(), "block {b} cached twice");
                }
            }
        }
    }

    #[test]
    fn hits_misses_and_lru_eviction() {
        let mut m = Model::new(3);
        for b in [10, 20, 30] {
            assert!(!m.read(b), "cold miss");
        }
        m.check();
        assert!(m.read(10), "10 still cached"); // 10 now MRU; 20 is LRU
        assert!(!m.read(40), "miss evicts 20");
        m.check();
        assert_eq!(m.cache.block_at(1), Some(40), "40 took 20's slot");
        assert!(!m.read(20), "20 was evicted");
        assert_eq!(m.cache.stats(), (1, 5), "one hit (read 10), five misses");
    }

    #[test]
    fn dirty_blocks_are_written_back_on_eviction_only() {
        let mut m = Model::new(2);
        m.write(10); // dirty
        m.read(20);
        assert_eq!(
            m.cache.access(30),
            Lookup::Evict {
                slot: 0,
                evicted: 10
            },
            "10 is dirty and LRU"
        );
        // The caller wrote 10 back, then fills.
        m.cache.mark_clean(0);
        m.cache.filled(0, 30);
        m.slots[0] = Some((30, false));
        m.check();
        // 20 is clean, so evicting it is a Replace (no write-back).
        assert_eq!(
            m.cache.access(40),
            Lookup::Replace {
                slot: 1,
                evicted: 20
            }
        );
    }

    #[test]
    fn a_clean_block_is_replaced_without_write_back() {
        let mut m = Model::new(1);
        m.read(10);
        assert_eq!(
            m.cache.access(20),
            Lookup::Replace {
                slot: 0,
                evicted: 10
            }
        );
    }

    #[test]
    fn flush_lists_dirty_slots_oldest_first() {
        let mut m = Model::new(4);
        m.write(10);
        m.read(20);
        m.write(30);
        m.write(40);
        // LRU order oldest->newest after the writes: 10, 20, 30, 40; 20 is clean.
        let dirty: std::vec::Vec<u64> = m.cache.dirty_slots().into_iter().map(|(_, b)| b).collect();
        assert_eq!(dirty, [10, 30, 40]);
        for (slot, _) in m.cache.dirty_slots() {
            m.cache.mark_clean(slot);
        }
        assert!(m.cache.dirty_slots().is_empty());
    }

    #[test]
    fn a_deterministic_workload_keeps_the_model_and_index_in_step() {
        let mut m = Model::new(8);
        let mut x = 0x1234_5678u64;
        for _ in 0..20_000 {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let block = (x >> 33) % 20;
            if x & 1 == 0 {
                m.read(block);
            } else {
                m.write(block);
            }
            m.check();
            // The cache never holds more blocks than its capacity.
            let held = (0..m.cache.capacity())
                .filter(|&s| m.cache.block_at(s).is_some())
                .count();
            assert!(held <= 8);
        }
        let (hits, misses) = m.cache.stats();
        assert_eq!(hits + misses, 20_000);
        assert!(
            hits > 5_000,
            "a 20-block working set in 8 slots should hit often: {hits}"
        );
    }

    #[test]
    fn capacity_one_and_repeated_access() {
        let mut m = Model::new(0); // clamped to 1
        assert_eq!(m.cache.capacity(), 1);
        assert!(!m.read(5));
        for _ in 0..10 {
            assert!(m.read(5), "the one slot keeps hitting");
        }
        m.check();
    }
}
