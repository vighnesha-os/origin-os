//! The block cache's index (CLAUDE.md §39 page cache, §40 the block layer): which cache
//! slot holds which block, LRU order, and what must be written back before a slot is
//! reused. Plain bookkeeping, no data buffers and no `unsafe`, tested on the host; the
//! kernel (`src/block.rs`) owns the slots' memory and does the I/O.
//!
//! The index maps a block number to one of `capacity` slots. A lookup that hits moves the
//! slot to the most-recently-used end. A miss chooses the least-recently-used slot, and if
//! that slot is dirty the caller must write it back (to its old block) before filling it
//! with the new one. Slots start empty and are filled before any eviction.
//!
//! A slot is labelled with a block only while its page holds that block's data.
//! [`BlockCache::bring_in`] keeps that true across failed I/O: no read runs into a labelled
//! page, and a slot whose read failed goes back on the free list. A transfer that never
//! completed leaves its page in the device's hands, so its slot is quarantined: never
//! labelled again and never reused.

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

/// One transfer of a whole slot's page that [`BlockCache::bring_in`] and
/// [`BlockCache::write_back_all`] ask the device for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// Write the page of `slot` to `block`. The device only reads the page.
    WriteBack { slot: Slot, block: u64 },
    /// Read `block` into the page of `slot`. The page's old contents are gone whatever the
    /// outcome.
    Read { slot: Slot, block: u64 },
}

/// How a [`Request`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transfer {
    /// It completed successfully.
    Done,
    /// It failed, and the device has finished with the page (or never got it).
    Failed,
    /// No completion arrived for it: the command may still be live, and the device may read
    /// or write the page at any time.
    Stuck,
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
    /// Handed to a transfer that never completed: out of use for good.
    quarantined: bool,
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
    /// Slots holding no block, the last one pushed first (a simple stack).
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
                    quarantined: false,
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
    /// [`BlockCache::filled`]. `None` if no slot is free and none can be evicted (every slot
    /// is quarantined, or handed out and not yet filled).
    pub fn access(&mut self, block: u64) -> Option<Lookup> {
        if let Some(slot) = self.find(block) {
            self.hits += 1;
            self.touch(slot);
            return Some(Lookup::Hit(slot));
        }
        self.misses += 1;
        if let Some(slot) = self.free.pop() {
            return Some(Lookup::Fill(slot));
        }
        // Evict the least-recently-used slot (`NONE`, which has no entry, if there is none).
        let slot = self.oldest;
        let entry = self.entries.get(slot)?;
        let evicted = entry.block.unwrap_or(0);
        Some(if entry.dirty {
            Lookup::Evict { slot, evicted }
        } else {
            Lookup::Replace { slot, evicted }
        })
    }

    /// Makes `block`'s data present in a slot, asking `io` for whatever transfers that
    /// takes, and returns the slot. `None`: it could not be done, and no slot may be used.
    ///
    /// The old label goes before a read can touch the page (on a `Replace` at once, on an
    /// `Evict` once the write-back is done), so a failed read leaves no slot labelled with
    /// data it may have overwritten. A write-back that failed leaves its slot as it was: the
    /// device only read the page. A [`Transfer::Stuck`] quarantines the slot.
    pub fn bring_in(
        &mut self,
        block: u64,
        mut io: impl FnMut(Request) -> Transfer,
    ) -> Option<Slot> {
        let lookup = self.access(block)?;
        let slot = lookup.slot();
        match lookup {
            Lookup::Hit(_) => return Some(slot),
            Lookup::Fill(_) => {}
            Lookup::Replace { .. } => self.invalidate(slot),
            Lookup::Evict { evicted, .. } => match io(Request::WriteBack {
                slot,
                block: evicted,
            }) {
                Transfer::Done => self.invalidate(slot),
                Transfer::Failed => return None,
                Transfer::Stuck => {
                    self.quarantine(slot);
                    return None;
                }
            },
        }
        match io(Request::Read { slot, block }) {
            Transfer::Done => {
                self.filled(slot, block);
                Some(slot)
            }
            Transfer::Failed => {
                self.invalidate(slot);
                None
            }
            Transfer::Stuck => {
                self.quarantine(slot);
                None
            }
        }
    }

    /// Writes every dirty slot back through `io`, oldest first, and returns whether all of
    /// it succeeded. A slot whose write-back failed stays dirty; a [`Transfer::Stuck`]
    /// quarantines it.
    pub fn write_back_all(&mut self, mut io: impl FnMut(Request) -> Transfer) -> bool {
        let mut ok = true;
        for (slot, block) in self.dirty_slots() {
            match io(Request::WriteBack { slot, block }) {
                Transfer::Done => self.mark_clean(slot),
                Transfer::Failed => ok = false,
                Transfer::Stuck => {
                    self.quarantine(slot);
                    ok = false;
                }
            }
        }
        ok
    }

    /// Drops `slot` from the index: off the LRU list, no block, clean, and back on the free
    /// list, once. No lookup finds the block it held after this, so nothing reads the page
    /// as that block's data. A quarantined slot stays out of use.
    pub fn invalidate(&mut self, slot: Slot) {
        if self.entries.get(slot).is_none_or(|e| e.quarantined) {
            return;
        }
        self.unlabel(slot);
        if !self.free.contains(&slot) {
            self.free.push(slot);
        }
    }

    /// Takes `slot` out of use for good: no label, and neither free nor evictable, so no
    /// lookup ever returns it. For a slot whose page was handed to a transfer that never
    /// completed: the device may still reach that page, so it must not be reused or freed.
    pub fn quarantine(&mut self, slot: Slot) {
        self.unlabel(slot);
        self.free.retain(|&s| s != slot);
        if let Some(e) = self.entries.get_mut(slot) {
            e.quarantined = true;
        }
    }

    /// How many slots are quarantined.
    pub fn quarantined(&self) -> usize {
        self.entries.iter().filter(|e| e.quarantined).count()
    }

    /// Removes `slot`'s label, if it has one: off the LRU list, no block, clean.
    fn unlabel(&mut self, slot: usize) {
        if self.entries.get(slot).is_some_and(|e| e.block.is_some()) {
            self.unlink(slot);
        }
        if let Some(e) = self.entries.get_mut(slot) {
            e.block = None;
            e.dirty = false;
        }
    }

    /// Records that `slot` now holds `block` (read in, or written back then refilled) and
    /// is clean, and moves it to the most-recently-used end. A quarantined slot stays out of
    /// use.
    pub fn filled(&mut self, slot: Slot, block: u64) {
        if self.entries.get(slot).is_none_or(|e| e.quarantined) {
            return;
        }
        self.free.retain(|&s| s != slot);
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

/// Whether an NVMe queue can take a command (CLAUDE.md §33): the plain logic of the driver's
/// rule, here so that it is tested on the host.
///
/// The driver runs one command at a time on a queue, so a second caller waits its turn. A
/// queue whose command never completed is dead: the command may still be live on the
/// controller, its completion may arrive at any time, and without an abort or a controller
/// reset the queue cannot be brought back in step. Every later command fails at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueGate {
    Idle,
    Busy,
    Dead,
}

impl QueueGate {
    /// Claims the queue for one command, or says why not ([`QueueGate::Busy`] or
    /// [`QueueGate::Dead`]).
    pub fn claim(&mut self) -> Result<(), Self> {
        match *self {
            Self::Idle => {
                *self = Self::Busy;
                Ok(())
            }
            refused => Err(refused),
        }
    }

    /// The claimed command is finished with: the queue is free. Nothing revives a dead one.
    pub fn release(&mut self) {
        if *self == Self::Busy {
            *self = Self::Idle;
        }
    }

    /// The claimed command never completed: the queue is dead until the controller is reset.
    pub fn kill(&mut self) {
        *self = Self::Dead;
    }

    pub const fn is_dead(self) -> bool {
        matches!(self, Self::Dead)
    }
}

/// The disk the boot test builds (`developer-tools/xtask/src/disk.rs`): the only disk the
/// boot proofs may write to (CLAUDE.md P0). It is named positively, so that anything else,
/// a real disk first of all, is left alone. Here because this is the block layer's library
/// half and the logic is tested on the host against the image xtask really builds.
pub mod test_disk {
    use crate::gpt;

    /// Its blocks, and their size.
    pub const BLOCKS: u64 = 32768;
    pub const BLOCK_SIZE: u32 = 512;
    /// Its GPT disk GUID, 28a71590-fa80-422c-a428-8dff96984d99, as stored on disk.
    pub const DISK_GUID: gpt::Guid = gpt::Guid([
        0x90, 0x15, 0xA7, 0x28, 0x80, 0xFA, 0x2C, 0x42, 0xA4, 0x28, 0x8D, 0xFF, 0x96, 0x98, 0x4D,
        0x99,
    ]);
    /// The first block of its pattern, and how many blocks carry one.
    pub const PATTERN_FIRST: u64 = 2048;
    pub const PATTERN_BLOCKS: u64 = 64;
    /// Bytes of the pattern blocks.
    pub const PATTERN_BYTES: usize = (PATTERN_BLOCKS * BLOCK_SIZE as u64) as usize;

    /// The bytes xtask writes in pattern block `lba`: `ORIGIN NVME LBA 00002048 ` repeated.
    pub fn pattern(lba: u64, out: &mut [u8]) {
        let mut unit = *b"ORIGIN NVME LBA 00000000 ";
        let mut v = lba;
        for i in (16..24).rev() {
            if let Some(d) = unit.get_mut(i) {
                *d = b'0' + (v % 10) as u8;
            }
            v /= 10;
        }
        for (o, u) in out.iter_mut().zip(unit.iter().cycle()) {
            *o = *u;
        }
    }

    /// Whether a namespace of `blocks` blocks of `block_size` bytes has the boot test disk's
    /// geometry. Checked first: no other disk needs to be read at all.
    pub const fn geometry_matches(blocks: u64, block_size: u32) -> bool {
        blocks == BLOCKS && block_size == BLOCK_SIZE
    }

    /// Whether the namespace is the boot test's disk: its geometry, a valid primary GPT
    /// header in `header` (one block, read from LBA 1) naming [`DISK_GUID`], and every
    /// pattern block in `area` ([`PATTERN_BYTES`] from [`PATTERN_FIRST`]). Anything less
    /// says no.
    pub fn identifies(blocks: u64, block_size: u32, header: &[u8], area: &[u8]) -> bool {
        const BLOCK: usize = BLOCK_SIZE as usize;
        if !geometry_matches(blocks, block_size)
            || header.len() != BLOCK
            || area.len() != PATTERN_BYTES
        {
            return false;
        }
        let mut expected = [0u8; BLOCK];
        gpt::parse_header(header, 1, blocks).is_ok_and(|h| h.disk == DISK_GUID)
            && area
                .as_chunks::<BLOCK>()
                .0
                .iter()
                .zip(PATTERN_FIRST..)
                .all(|(block, lba)| {
                    pattern(lba, &mut expected);
                    *block == expected
                })
    }
}

/// The boot test's real disk image (`disk.rs`), which the identification is tested against.
#[cfg(test)]
#[allow(dead_code, unreachable_pub)]
#[path = "../../developer-tools/xtask/src/disk.rs"]
mod boot_test_disk;

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::collections::{HashMap, HashSet, VecDeque};

    /// What a page holds after a failed read: no block's value is ever this.
    const GARBAGE: u64 = u64::MAX;

    /// A block's contents before anything wrote it.
    fn initial(block: u64) -> u64 {
        block + 1
    }

    /// The simulated device behind [`Model::fetch`]: what the pages and the disk hold (a
    /// block's contents are one number), and the faults to inject.
    #[derive(Default)]
    struct Device {
        pages: std::vec::Vec<u64>,
        disk: HashMap<u64, u64>,
        /// The newest contents written through the cache, per block.
        newest: HashMap<u64, u64>,
        /// Slots whose page went to a transfer that never completed.
        stuck: HashSet<Slot>,
        /// How the next transfers end; once it is empty, they succeed.
        faults: VecDeque<Transfer>,
    }

    /// Carries out one request on the device, mirroring what the slot must look like next.
    fn serve(slots: &mut [Option<(u64, bool)>], device: &mut Device, request: Request) -> Transfer {
        let end = device.faults.pop_front().unwrap_or(Transfer::Done);
        match request {
            Request::WriteBack { slot, block } => {
                assert!(!device.stuck.contains(&slot), "slot {slot} reused");
                assert_eq!(slots[slot], Some((block, true)), "names a dirty block");
                assert_eq!(
                    Some(&device.pages[slot]),
                    device.newest.get(&block),
                    "the page still holds what was written"
                );
                match end {
                    Transfer::Done => {
                        device.disk.insert(block, device.pages[slot]);
                        slots[slot] = Some((block, false));
                    }
                    Transfer::Failed => {}
                    Transfer::Stuck => {
                        device.stuck.insert(slot);
                        device.newest.remove(&block);
                        slots[slot] = None;
                    }
                }
            }
            Request::Read { slot, block } => {
                assert!(!device.stuck.contains(&slot), "slot {slot} reused");
                assert_ne!(
                    slots[slot].map(|(_, dirty)| dirty),
                    Some(true),
                    "a read would overwrite unwritten data"
                );
                device.pages[slot] = GARBAGE;
                slots[slot] = None;
                match end {
                    Transfer::Done => {
                        device.pages[slot] =
                            device.disk.get(&block).copied().unwrap_or(initial(block));
                        slots[slot] = Some((block, false));
                    }
                    Transfer::Failed => {}
                    Transfer::Stuck => {
                        device.stuck.insert(slot);
                    }
                }
            }
        }
        end
    }

    /// A model of a cache over `blocks` blocks: the authoritative contents, checked against
    /// the index after every operation.
    struct Model {
        cache: BlockCache,
        /// Each slot's block and dirtiness, mirrored.
        slots: std::vec::Vec<Option<(u64, bool)>>,
        device: Device,
        /// A counter for fresh contents to write.
        written: u64,
    }

    impl Model {
        fn new(capacity: usize) -> Self {
            let cache = BlockCache::new(capacity);
            let slots = std::vec![None; cache.capacity()];
            let device = Device {
                pages: std::vec![0; cache.capacity()],
                ..Device::default()
            };
            Self {
                cache,
                slots,
                device,
                written: 0,
            }
        }

        /// `block` through [`BlockCache::bring_in`], over the simulated device with whatever
        /// faults are queued: its slot and whether it hit, or `None` if the cache said no.
        fn fetch(&mut self, block: u64) -> Option<(Slot, bool)> {
            let hits = self.cache.stats().0;
            let slot = self.cache.bring_in(block, |request| {
                serve(&mut self.slots, &mut self.device, request)
            })?;
            let hit = self.cache.stats().0 != hits;
            assert_eq!(self.slots[slot].map(|(b, _)| b), Some(block), "{hit}");
            Some((slot, hit))
        }

        /// What `block` must read as now.
        fn expected(&self, block: u64) -> u64 {
            let device = &self.device;
            let newest = device.newest.get(&block).or(device.disk.get(&block));
            newest.copied().unwrap_or(initial(block))
        }

        /// Reads `block` and checks the data, however it got there: whether it hit, or
        /// `None` if the cache said no.
        fn read_data(&mut self, block: u64) -> Option<bool> {
            let (slot, hit) = self.fetch(block)?;
            assert_eq!(
                self.device.pages[slot],
                self.expected(block),
                "block {block} in slot {slot}, hit {hit}"
            );
            Some(hit)
        }

        /// Writes fresh contents to `block` through the cache: whether it could.
        fn write_data(&mut self, block: u64) -> bool {
            let Some((slot, _)) = self.fetch(block) else {
                return false;
            };
            self.written += 1;
            self.device.pages[slot] = self.written << 16;
            self.device.newest.insert(block, self.written << 16);
            self.cache.mark_dirty(slot);
            self.slots[slot] = Some((block, true));
            true
        }

        /// Every dirty slot written back: whether it all succeeded.
        fn flush(&mut self) -> bool {
            self.cache
                .write_back_all(|request| serve(&mut self.slots, &mut self.device, request))
        }

        /// One cache access for `block`: returns its slot and whether it hit, having
        /// written back and refilled as the lookup demanded.
        fn access(&mut self, block: u64) -> (Slot, bool) {
            let lookup = self.cache.access(block).unwrap();
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
                // Every slot is exactly one of: labelled, free, quarantined. None is lost.
                let free = self.cache.free.iter().filter(|&&s| s == slot).count();
                let stuck = usize::from(self.device.stuck.contains(&slot));
                assert_eq!(
                    usize::from(entry.is_some()) + free + stuck,
                    1,
                    "slot {slot}: labelled {entry:?}, on the free list {free} times, stuck {stuck}"
                );
                assert_eq!(
                    self.cache.entries[slot].quarantined,
                    stuck == 1,
                    "slot {slot}"
                );
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
            Some(Lookup::Evict {
                slot: 0,
                evicted: 10
            }),
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
            Some(Lookup::Replace {
                slot: 1,
                evicted: 20
            })
        );
    }

    #[test]
    fn a_clean_block_is_replaced_without_write_back() {
        let mut m = Model::new(1);
        m.read(10);
        assert_eq!(
            m.cache.access(20),
            Some(Lookup::Replace {
                slot: 0,
                evicted: 10
            })
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

    /// The block cache's protocol with a device, the same transfers failing and succeeding
    /// as on the device; the bug was that a failed fill lost its slot, and that after
    /// `capacity` of them the next access indexed `entries` with `NONE`.
    #[test]
    fn failed_fills_give_the_slot_back_and_nothing_panics() {
        let mut m = Model::new(2);
        for _ in 0..10 {
            m.device.faults.push_back(Transfer::Failed);
            assert_eq!(m.fetch(7), None);
            m.check();
        }
        assert_eq!(m.read_data(7), Some(false), "a slot is still there");
        assert_eq!(m.read_data(8), Some(false), "and the other one");
        m.check();
    }

    /// The bug: a slot kept its old label through a failed read into its page, so the next
    /// lookup of the old block was a hit on partial data.
    #[test]
    fn a_failed_read_on_replace_leaves_no_label_on_the_partial_data() {
        let mut m = Model::new(1);
        assert_eq!(m.read_data(10), Some(false));
        m.device.faults.push_back(Transfer::Failed);
        assert_eq!(m.fetch(20), None);
        assert_eq!(m.cache.block_at(0), None, "the old block's label is gone");
        m.check();
        assert_eq!(
            m.read_data(10),
            Some(false),
            "a miss: read from the disk again"
        );
        m.check();
    }

    /// The label is gone by the time a read starts, not merely once it has failed: a device
    /// that stops mid-transfer shows what the index said at that moment.
    #[test]
    fn the_old_label_is_gone_before_a_read_starts() {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        // On a Replace.
        let mut c = BlockCache::new(1);
        c.filled(0, 10);
        let stopped = catch_unwind(AssertUnwindSafe(|| {
            c.bring_in(20, |_| panic!("the device stops"))
        }));
        assert!(stopped.is_err());
        assert_eq!(c.block_at(0), None);
        // On an Evict: only once the write-back is done.
        let mut c = BlockCache::new(1);
        c.filled(0, 10);
        c.mark_dirty(0);
        let stopped = catch_unwind(AssertUnwindSafe(|| {
            c.bring_in(20, |_| panic!("the device stops"))
        }));
        assert!(stopped.is_err());
        assert_eq!(c.block_at(0), Some(10), "still labelled, still dirty");
        let stopped = catch_unwind(AssertUnwindSafe(|| {
            c.bring_in(20, |request| match request {
                Request::WriteBack { .. } => Transfer::Done,
                Request::Read { .. } => panic!("the device stops"),
            })
        }));
        assert!(stopped.is_err());
        assert_eq!(c.block_at(0), None);
    }

    /// The same bug on the eviction path: written back safely, then the read failed.
    #[test]
    fn a_failed_read_after_a_write_back_leaves_no_label_either() {
        let mut m = Model::new(1);
        assert!(m.write_data(10));
        m.device.faults.extend([Transfer::Done, Transfer::Failed]);
        assert_eq!(m.fetch(20), None);
        assert_eq!(m.cache.block_at(0), None);
        assert!(!m.cache.is_dirty(0));
        m.check();
        assert_eq!(
            m.device.disk.get(&10),
            m.device.newest.get(&10),
            "the write-back did happen"
        );
        assert_eq!(m.read_data(10), Some(false), "a miss, and the written data");
        m.check();
    }

    /// A write-back that failed changed nothing: the device only read the page, so the data
    /// is still good, still labelled, still dirty.
    #[test]
    fn a_failed_write_back_keeps_the_dirty_block() {
        let mut m = Model::new(1);
        assert!(m.write_data(10));
        m.device.faults.push_back(Transfer::Failed);
        assert_eq!(m.fetch(20), None);
        assert_eq!(m.cache.block_at(0), Some(10));
        assert!(m.cache.is_dirty(0));
        m.check();
        assert_eq!(
            m.read_data(10),
            Some(true),
            "still served, still the new data"
        );
        // And a flush that fails the same way leaves it dirty too.
        m.device.faults.push_back(Transfer::Failed);
        assert!(!m.flush());
        assert!(m.cache.is_dirty(0));
        m.check();
        assert!(m.flush());
        assert!(!m.cache.is_dirty(0));
        m.check();
    }

    /// A page handed to a command that never completed is out of use for good, whichever
    /// transfer that was.
    #[test]
    fn a_stuck_transfer_quarantines_its_slot_for_good() {
        // A read into a free slot.
        let mut m = Model::new(3);
        m.device.faults.push_back(Transfer::Stuck);
        assert_eq!(m.fetch(1), None);
        m.check();
        assert_eq!(m.cache.quarantined(), 1);
        let first = *m.device.stuck.iter().next().unwrap();
        for block in 2..64 {
            m.read_data(block);
            m.check();
            assert_ne!(m.fetch(block).map(|(s, _)| s), Some(first));
        }
        // A read into a slot that was replaced.
        let mut m = Model::new(1);
        assert_eq!(m.read_data(10), Some(false));
        m.device.faults.push_back(Transfer::Stuck);
        assert_eq!(m.fetch(20), None);
        m.check();
        assert_eq!(m.cache.block_at(0), None);
        // A write-back during an eviction: gone from the cache, and refused from here on.
        let mut m = Model::new(1);
        assert!(m.write_data(10));
        m.device.faults.push_back(Transfer::Stuck);
        assert_eq!(m.fetch(20), None);
        m.check();
        assert_eq!(m.cache.block_at(0), None);
        assert_eq!(m.cache.quarantined(), 1);
        // A write-back during a flush.
        let mut m = Model::new(2);
        assert!(m.write_data(10) && m.write_data(11));
        m.device.faults.extend([Transfer::Stuck, Transfer::Done]);
        assert!(!m.flush());
        m.check();
        assert_eq!(m.cache.quarantined(), 1);
        assert!(m.flush(), "the other one went out, and nothing is left");
    }

    /// Once nothing can be evicted the cache refuses; it used to index with `NONE`.
    #[test]
    fn with_every_slot_quarantined_the_cache_refuses_and_does_not_panic() {
        let mut m = Model::new(2);
        for block in 0..2 {
            m.device.faults.push_back(Transfer::Stuck);
            assert_eq!(m.fetch(block), None);
        }
        m.check();
        assert_eq!(m.cache.quarantined(), 2);
        assert_eq!(m.cache.access(9), None);
        assert_eq!(m.fetch(9), None);
        assert!(!m.write_data(9));
        assert!(m.flush(), "nothing dirty");
        m.check();
        // A quarantined slot stays out of use whatever is asked of the index.
        for slot in 0..2 {
            m.cache.invalidate(slot);
            m.cache.filled(slot, 5);
            assert_eq!(m.cache.block_at(slot), None);
        }
        assert_eq!(m.cache.access(5), None);
    }

    /// A slot handed out by `access` and never filled must not leave `access` to index with
    /// `NONE` either.
    #[test]
    fn access_refuses_when_every_slot_is_handed_out() {
        let mut c = BlockCache::new(2);
        assert_eq!(c.access(1), Some(Lookup::Fill(0)));
        assert_eq!(c.access(2), Some(Lookup::Fill(1)));
        assert_eq!(c.access(3), None);
        c.invalidate(0);
        c.invalidate(0);
        assert_eq!(c.free.len(), 1, "on the free list once");
        assert_eq!(c.access(3), Some(Lookup::Fill(0)));
    }

    /// `invalidate` puts a slot on the free list before its read; `filled` takes it off.
    #[test]
    fn a_slot_invalidated_and_then_filled_is_not_also_free() {
        let mut c = BlockCache::new(1);
        c.filled(0, 10);
        c.invalidate(0);
        c.filled(0, 20);
        assert!(c.free.is_empty());
        assert_eq!(c.block_at(0), Some(20));
    }

    /// Every transfer may fail at random: a hit always returns the block's newest data, a
    /// failed transfer never leaves a wrong label or loses a slot, and a stuck slot is never
    /// used again, across every capacity.
    #[test]
    fn random_failures_never_leave_a_wrong_label_or_lose_a_slot() {
        let (mut failed, mut stuck, mut hits, mut refused) = (0u64, 0u64, 0u64, 0u64);
        for round in 0..300u64 {
            let mut m = Model::new(1 + (round % 8) as usize);
            let mut x = round.wrapping_mul(0x1234_5678_9ABC_DEF1) | 1;
            let mut next = |bound: u64| {
                x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                (x >> 33) % bound
            };
            for step in 0..300 {
                for _ in 0..2 {
                    match next(32) {
                        0..=7 => {
                            failed += 1;
                            m.device.faults.push_back(Transfer::Failed);
                        }
                        8 => {
                            stuck += 1;
                            m.device.faults.push_back(Transfer::Stuck);
                        }
                        _ => {}
                    }
                }
                let block = next(12);
                let result = match next(16) {
                    0 => Some(m.flush()),
                    1..=6 => Some(m.write_data(block)),
                    _ => m.read_data(block).inspect(|&hit| hits += u64::from(hit)),
                };
                refused += u64::from(result.is_none());
                m.check();
                assert!(
                    m.cache.quarantined() <= m.cache.capacity(),
                    "round {round} step {step}"
                );
                m.device.faults.clear();
            }
        }
        assert!(
            failed > 10_000 && stuck > 1_000 && hits > 1_000 && refused > 1_000,
            "the failures really happened: {failed} {stuck} {hits} {refused}"
        );
    }

    /// One command at a time; a dead queue stays dead, whatever arrives later.
    #[test]
    fn a_dead_queue_stays_dead_and_a_busy_one_takes_no_second_command() {
        let mut gate = QueueGate::Idle;
        assert_eq!(gate.claim(), Ok(()));
        assert_eq!(gate.claim(), Err(QueueGate::Busy), "one at a time");
        gate.release();
        assert_eq!(gate.claim(), Ok(()), "free again");
        gate.kill();
        assert!(gate.is_dead());
        assert_eq!(gate.claim(), Err(QueueGate::Dead), "dead");
        gate.release();
        assert_eq!(gate.claim(), Err(QueueGate::Dead), "a late completion");
        assert!(gate.is_dead());
    }

    mod test_disk_identification {
        use super::super::boot_test_disk as xtask;
        use super::super::test_disk::{self, identifies};

        /// The header and pattern blocks of the disk the boot test really builds.
        fn parts(image: &[u8]) -> (&[u8], &[u8]) {
            (&image[512..1024], &image[(2048 * 512)..((2048 + 64) * 512)])
        }

        fn seal(header: &mut [u8]) {
            header[16..20].fill(0);
            let crc = crate::gpt::crc32(&header[..92]);
            header[16..20].copy_from_slice(&crc.to_le_bytes());
        }

        const BS: u32 = test_disk::BLOCK_SIZE;

        #[test]
        fn the_disk_the_boot_test_builds_is_identified() {
            // The same disk xtask writes, so the two halves cannot drift apart unnoticed.
            let image = xtask::image();
            let (header, area) = parts(&image);
            assert!(identifies(xtask::BLOCKS, BS, header, area));
            assert_eq!(test_disk::BLOCKS, xtask::BLOCKS);
            assert_eq!(u64::from(BS), xtask::BLOCK);
            assert_eq!(test_disk::DISK_GUID.0, xtask::guid(xtask::DISK_GUID));
            assert_eq!(std::format!("{}", test_disk::DISK_GUID), xtask::DISK_GUID);
            assert_eq!(test_disk::PATTERN_FIRST, xtask::PARTITIONS[0].2);
            assert_eq!(test_disk::PATTERN_BLOCKS, xtask::PATTERN_BLOCKS);
            for lba in [2048, 2049, 2111] {
                let mut block = [0u8; 512];
                test_disk::pattern(lba, &mut block);
                assert_eq!(block.as_slice(), xtask::pattern(lba));
            }
        }

        #[test]
        fn a_real_disk_is_not_the_test_disk() {
            let image = xtask::image();
            let (header, area) = parts(&image);
            // Any other size or block format: not even read.
            assert!(!test_disk::geometry_matches(xtask::BLOCKS + 1, BS));
            assert!(!identifies(xtask::BLOCKS + 1, BS, header, area));
            assert!(!identifies(xtask::BLOCKS, 4096, header, area));
            // A blank disk, an all-ones disk.
            let blank = [0u8; 512];
            let zeros = std::vec![0u8; test_disk::PATTERN_BYTES];
            assert!(!identifies(xtask::BLOCKS, BS, &blank, &zeros));
            let ones = std::vec![0xFFu8; test_disk::PATTERN_BYTES];
            assert!(!identifies(xtask::BLOCKS, BS, &[0xFF; 512], &ones));
            // The right GUID but no pattern; the right pattern but no GPT.
            assert!(!identifies(xtask::BLOCKS, BS, header, &zeros));
            assert!(!identifies(xtask::BLOCKS, BS, &blank, area));
            // A valid GPT of another disk, the pattern present: the GUID alone says no.
            let mut other = header.to_vec();
            other[56] ^= 1;
            assert!(!identifies(xtask::BLOCKS, BS, &other, area), "a bad CRC");
            seal(&mut other);
            assert!(!identifies(xtask::BLOCKS, BS, &other, area), "another GUID");
            // A header or an area of the wrong length.
            assert!(!identifies(xtask::BLOCKS, BS, &header[..100], area));
            assert!(!identifies(xtask::BLOCKS, BS, header, &area[..512]));
            let longer = [area, &[0u8; 512]].concat();
            assert!(!identifies(xtask::BLOCKS, BS, header, &longer));
        }

        /// A change of any byte the identification looks at, in the header (which its CRC
        /// covers) or in the pattern, says no.
        #[test]
        fn a_change_of_any_byte_it_looks_at_says_no() {
            let image = xtask::image();
            let (header, area) = parts(&image);
            let (mut h, mut a) = (header.to_vec(), area.to_vec());
            for at in (0..92).step_by(7) {
                h[at] ^= 0x40;
                assert!(!identifies(xtask::BLOCKS, BS, &h, area), "header byte {at}");
                h[at] ^= 0x40;
            }
            for at in (0..test_disk::PATTERN_BYTES).step_by(251) {
                a[at] ^= 0x40;
                assert!(
                    !identifies(xtask::BLOCKS, BS, header, &a),
                    "pattern byte {at}"
                );
                a[at] ^= 0x40;
            }
            assert!(identifies(xtask::BLOCKS, BS, &h, &a), "restored");
        }
    }
}
