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

/// The disk the boot test builds (`developer-tools/xtask/src/disk.rs`), and the mark that lets
/// the boot proofs write to part of it (CLAUDE.md P0; TASKS item 171). The disk is named
/// positively, by its geometry and its pattern, so that anything else, a real disk first of
/// all, is only read; and on it the proofs write only inside the one partition that carries the
/// mark, whose bounds the GPT gives ([`mark`]). Here because this is the block layer's library
/// half and the logic is tested on the host against the image xtask really builds.
pub mod test_disk {
    use crate::gpt::{self, GptError, Header, Partition};
    use crate::nvme::Writable;

    /// Its blocks, and their size.
    pub const BLOCKS: u64 = 32768;
    pub const BLOCK_SIZE: u32 = 512;
    /// The type GUID of the partition the boot proofs may overwrite, the mark of Origin's tests,
    /// 8d4070b7-7072-4d27-9689-81467d6a5735, as stored on disk. PROVISIONAL: generated (UUID
    /// version 4) on 2026-11-27 and accepted the same day by the owner, owner item 384, on test
    /// disks only, to be fixed with M9's partition-layout decision. xtask's `disk.rs` holds its
    /// text.
    pub const MARK: gpt::Guid = gpt::Guid([
        0xB7, 0x70, 0x40, 0x8D, 0x72, 0x70, 0x27, 0x4D, 0x96, 0x89, 0x81, 0x46, 0x7D, 0x6A, 0x57,
        0x35,
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

    /// Whether the namespace is the boot test's disk: its geometry, and every pattern block in
    /// `area` ([`PATTERN_BYTES`] from [`PATTERN_FIRST`]). Anything less says no. The proofs that
    /// read run only on it, since they check what they read against the pattern; the proofs
    /// that write need the mark as well ([`mark`]).
    pub fn holds_pattern(blocks: u64, block_size: u32, area: &[u8]) -> bool {
        const BLOCK: usize = BLOCK_SIZE as usize;
        if !geometry_matches(blocks, block_size) || area.len() != PATTERN_BYTES {
            return false;
        }
        let mut expected = [0u8; BLOCK];
        area.as_chunks::<BLOCK>()
            .0
            .iter()
            .zip(PATTERN_FIRST..)
            .all(|(block, lba)| {
                pattern(lba, &mut expected);
                *block == expected
            })
    }

    /// One GPT copy as the kernel read it: its validated header and its used entries, or why it
    /// is not valid.
    pub type Copy<'a> = Result<(Header, &'a [Partition]), GptError>;

    /// Why the boot proofs only read a namespace: the first condition of [`mark`] it fails.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum NoMark {
        /// Not the boot test's geometry and pattern ([`holds_pattern`]): any real disk.
        NotTheTestDisk,
        /// LBA 0 is not a protective MBR (`gpt::is_protective_mbr`).
        NoProtectiveMbr,
        /// Neither GPT copy is valid.
        NoGpt,
        /// Only one copy is valid (the primary if `primary`). UEFI 2.11 §5.3.2 (pp. 118–119)
        /// asks software to restore the other, asking the user first; the boot proofs are not
        /// that software, and a restore is a write.
        OneCopy { primary: bool },
        /// Both copies are valid but describe different disks or partitions.
        Disagree,
        /// No used entry has the mark's type.
        Unmarked,
        /// This many used entries have it: no single span to trust.
        Marks(usize),
        /// The marked partition, numbered from 1, has attribute bit 0, Required Partition, set
        /// (UEFI 2.11 Table 5.8, p. 120).
        Required(u32),
    }

    impl core::fmt::Display for NoMark {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match *self {
                Self::NotTheTestDisk => f.write_str("this is not the boot test's disk"),
                Self::NoProtectiveMbr => f.write_str("LBA 0 is not a protective MBR"),
                Self::NoGpt => f.write_str("no valid GPT"),
                Self::OneCopy { primary } => write!(
                    f,
                    "only the {} copy of the GPT is valid, and the kernel repairs nothing during boot",
                    if primary { "primary" } else { "backup" }
                ),
                Self::Disagree => f.write_str("the GPT's two copies disagree"),
                Self::Unmarked => f.write_str("no partition carries it"),
                Self::Marks(n) => write!(f, "{n} partitions carry it"),
                Self::Required(i) => {
                    write!(f, "partition {i}, which carries it, is marked required")
                }
            }
        }
    }

    /// The marked partition, and the blocks of namespace `span.nsid` it lets the proofs write.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Mark {
        pub partition: Partition,
        pub span: Writable,
    }

    /// The partition of namespace `nsid` the boot proofs may write to, or why there is none,
    /// from what the kernel read of a disk that [`holds_pattern`]: `mbr` (LBA 0), and both GPT
    /// copies, `primary` from LBA 1 and `backup` from where the primary says (or the last block).
    /// Every condition must hold:
    /// - LBA 0 is a protective MBR, since behind a legacy MBR a GPT may be stale (UEFI 2.11
    ///   §5.3.2, p. 118);
    /// - both copies are valid and agree, entry for entry;
    /// - exactly one used entry has the type [`MARK`];
    /// - its Required Partition attribute is clear.
    ///
    /// The span is the entry's own bounds, which `gpt::parse_entries` has already found inside
    /// the usable range: never the MBR, a GPT header or an entry array.
    pub fn mark(
        nsid: u32,
        mbr: &[u8],
        primary: Copy<'_>,
        backup: Copy<'_>,
    ) -> Result<Mark, NoMark> {
        if !gpt::is_protective_mbr(mbr) {
            return Err(NoMark::NoProtectiveMbr);
        }
        let parts = match (primary, backup) {
            (Ok((p, mine)), Ok((b, theirs))) => {
                if !(p.agrees_with(&b) && mine == theirs) {
                    return Err(NoMark::Disagree);
                }
                mine
            }
            (Ok(_), Err(_)) => return Err(NoMark::OneCopy { primary: true }),
            (Err(_), Ok(_)) => return Err(NoMark::OneCopy { primary: false }),
            (Err(_), Err(_)) => return Err(NoMark::NoGpt),
        };
        let mut marked = parts.iter().filter(|p| p.kind == MARK);
        let Some(partition) = marked.next().copied() else {
            return Err(NoMark::Unmarked);
        };
        let more = marked.count();
        if more > 0 {
            return Err(NoMark::Marks(1 + more));
        }
        if partition.attributes & 1 != 0 {
            return Err(NoMark::Required(partition.index + 1));
        }
        Ok(Mark {
            partition,
            span: Writable {
                nsid,
                first: partition.first,
                last: partition.last,
            },
        })
    }

    /// Device blocks in a 4 KiB page of the test disk.
    pub const PAGE_BLOCKS: u64 = 4096 / BLOCK_SIZE as u64;

    /// Where in the marked partition each proof writes (TASKS item 171). Computed from the span
    /// alone, and never checked here: a write that does not fit is refused by the driver's guard
    /// (`nvme::permitted`), so a partition too small fails a proof and nothing is written outside
    /// it. Every write is on the test disk, whose blocks are 512 bytes.
    pub mod plan {
        use super::PAGE_BLOCKS;
        use crate::nvme::Writable;

        /// The block cache's page: the first whole page of the partition.
        pub const fn cache(span: Writable) -> u64 {
            span.first.next_multiple_of(PAGE_BLOCKS)
        }

        /// The reserved-memory proof's page of 8 blocks, the next one.
        pub const fn reserved(span: Writable) -> u64 {
            cache(span).saturating_add(PAGE_BLOCKS)
        }

        /// Processor `k`'s block (in queue order): one a page apart, from 64 blocks in.
        pub const fn cpu(span: Writable, k: u64) -> u64 {
            cache(span)
                .saturating_add(64)
                .saturating_add(k.saturating_mul(PAGE_BLOCKS))
        }

        /// The polled proof's block: the partition's last, so the guard's inclusive end is used.
        pub const fn polled(span: Writable) -> u64 {
            span.last
        }
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
        use super::super::test_disk::{self, Mark, NoMark, holds_pattern, mark, plan};
        use crate::gpt::{self, GptError, Header, Partition};
        use crate::nvme::Writable;
        use std::vec::Vec;

        const BS: u32 = test_disk::BLOCK_SIZE;
        const B: usize = 512;
        const LAST: u64 = xtask::BLOCKS - 1;

        /// The pattern blocks of an image.
        fn area(image: &[u8]) -> &[u8] {
            &image[(2048 * B)..((2048 + 64) * B)]
        }

        /// One GPT copy of `image`, from block `lba`, as the kernel reads it.
        fn copy(image: &[u8], lba: u64) -> Result<(Header, Vec<Partition>), GptError> {
            let at = lba as usize * B;
            let h = gpt::parse_header(&image[at..at + B], lba, xtask::BLOCKS)?;
            let from = h.entries_lba as usize * B;
            let array = &image[from..from + h.array_len() as usize];
            let mut out = [Partition {
                index: 0,
                kind: gpt::Guid::default(),
                unique: gpt::Guid::default(),
                first: 0,
                last: 0,
                attributes: 0,
                name: [0; 36],
            }; 16];
            let n = gpt::parse_entries(array, &h, &mut out)?;
            Ok((h, out[..n].to_vec()))
        }

        /// A copy as `mark` takes it.
        fn view(c: &Result<(Header, Vec<Partition>), GptError>) -> test_disk::Copy<'_> {
            match c {
                Ok((h, v)) => Ok((*h, v.as_slice())),
                Err(e) => Err(*e),
            }
        }

        /// The kernel's decision on `image`, from LBA 0 and both copies.
        fn decide(image: &[u8]) -> Result<Mark, NoMark> {
            let (p, b) = (copy(image, 1), copy(image, LAST));
            mark(1, &image[..B], view(&p), view(&b))
        }

        /// Edits entry `index` of both entry arrays with `f`, then seals both copies again:
        /// a GPT another tool might have written.
        fn edit_entry(image: &mut [u8], index: usize, f: impl Fn(&mut [u8])) {
            for (header, array) in [(1usize, 2usize), (LAST as usize, LAST as usize - 32)] {
                let at = array * B + index * 128;
                f(&mut image[at..at + 128]);
                let crc = gpt::crc32(&image[array * B..array * B + 128 * 128]);
                let h = header * B;
                image[h + 88..h + 92].copy_from_slice(&crc.to_le_bytes());
                seal(&mut image[h..h + B]);
            }
        }

        fn seal(header: &mut [u8]) {
            header[16..20].fill(0);
            let crc = gpt::crc32(&header[..92]);
            header[16..20].copy_from_slice(&crc.to_le_bytes());
        }

        #[test]
        fn the_disk_the_boot_test_builds_holds_the_pattern_and_the_mark() {
            // The same disk xtask writes, so the two halves cannot drift apart unnoticed.
            let image = xtask::image();
            assert!(holds_pattern(xtask::BLOCKS, BS, area(&image)));
            assert_eq!(test_disk::BLOCKS, xtask::BLOCKS);
            assert_eq!(u64::from(BS), xtask::BLOCK);
            assert_eq!(test_disk::MARK.0, xtask::guid(xtask::ORIGIN_PROOF_SCRATCH));
            assert_eq!(
                std::format!("{}", test_disk::MARK),
                xtask::ORIGIN_PROOF_SCRATCH
            );
            assert_eq!(test_disk::PATTERN_FIRST, xtask::PARTITIONS[0].2);
            assert_eq!(test_disk::PATTERN_BLOCKS, xtask::PATTERN_BLOCKS);
            for lba in [2048, 2049, 2111] {
                let mut block = [0u8; 512];
                test_disk::pattern(lba, &mut block);
                assert_eq!(block.as_slice(), xtask::pattern(lba));
            }
            let m = decide(&image).expect("the boot test's disk carries the mark");
            assert_eq!(
                m.span,
                Writable {
                    nsid: 1,
                    first: xtask::MARKED.0,
                    last: xtask::MARKED.1
                }
            );
            assert_eq!(m.partition.index, 2);
            assert_eq!(
                std::format!("{}", m.partition.name()),
                xtask::PARTITIONS[2].0
            );
            // The write plan, as xtask's markers and byte check expect it.
            assert_eq!(plan::cache(m.span), xtask::CACHE_LBA);
            assert_eq!(plan::reserved(m.span), xtask::RESERVED_LBA);
            assert_eq!(plan::polled(m.span), xtask::POLLED_LBA);
            for k in [0, 1, 61, 1777] {
                assert_eq!(plan::cpu(m.span, k), xtask::cpu_lba(k));
            }
        }

        /// ADR-0013's controls D1 and D2: the disks xtask builds for them.
        #[test]
        fn the_control_disks_decide_as_their_controls_need() {
            let unmarked = xtask::image_with(xtask::Variant::Unmarked);
            assert!(holds_pattern(xtask::BLOCKS, BS, area(&unmarked)));
            assert_eq!(decide(&unmarked), Err(NoMark::Unmarked));
            let small = xtask::image_with(xtask::Variant::Small);
            let m = decide(&small).expect("the small partition carries the mark");
            assert_eq!(
                (m.span.first, m.span.last),
                (xtask::MARKED.0, xtask::SMALL_LAST)
            );
            // Every fixed write fits and processor 0's; processor 1's does not, so the guard
            // refuses it.
            for lba in [
                plan::cache(m.span),
                plan::reserved(m.span) + 7,
                plan::polled(m.span),
                plan::cpu(m.span, 0),
            ] {
                assert!(m.span.contains(lba, 1), "{lba}");
            }
            assert!(!m.span.contains(plan::cpu(m.span, 1), 1));
        }

        #[test]
        fn every_gpt_state_decides() {
            let good = xtask::image();
            // Only one copy valid: the backup's header damaged, or the primary's.
            let mut d = good.clone();
            d[LAST as usize * B + 20] ^= 1;
            assert_eq!(decide(&d), Err(NoMark::OneCopy { primary: true }));
            let mut d = good.clone();
            d[B + 20] ^= 1;
            assert_eq!(decide(&d), Err(NoMark::OneCopy { primary: false }));
            // A damaged entry array is a damaged copy too.
            let mut d = good.clone();
            d[(LAST as usize - 32) * B] ^= 1;
            assert_eq!(decide(&d), Err(NoMark::OneCopy { primary: true }));
            // Neither, and no GPT at all.
            let mut d = good.clone();
            d[B + 20] ^= 1;
            d[LAST as usize * B + 20] ^= 1;
            assert_eq!(decide(&d), Err(NoMark::NoGpt));
            let mut blank = std::vec![0u8; good.len()];
            blank[..B].copy_from_slice(&good[..B]);
            assert_eq!(decide(&blank), Err(NoMark::NoGpt));
            // Both valid, describing different partitions: the small disk's backup array and
            // header behind the marked disk's primary.
            let small = xtask::image_with(xtask::Variant::Small);
            let mut d = good.clone();
            let back = (LAST as usize - 32) * B;
            d[back..].copy_from_slice(&small[back..]);
            assert!(copy(&d, 1).is_ok() && copy(&d, LAST).is_ok());
            assert_eq!(decide(&d), Err(NoMark::Disagree));
            // No protective MBR: blank, or a legacy partition beside it.
            let mut d = good.clone();
            d[..B].fill(0);
            assert_eq!(decide(&d), Err(NoMark::NoProtectiveMbr));
            let mut d = good.clone();
            d[446 + 16 + 4] = 0x07;
            assert_eq!(decide(&d), Err(NoMark::NoProtectiveMbr));
            // Two marked partitions: Origin data given the mark too.
            let mut d = good.clone();
            edit_entry(&mut d, 1, |e| e[0..16].copy_from_slice(&test_disk::MARK.0));
            assert_eq!(decide(&d), Err(NoMark::Marks(2)));
            // The marked partition flagged required; another attribute bit is no matter.
            let mut d = good.clone();
            edit_entry(&mut d, 2, |e| e[48] |= 1);
            assert_eq!(decide(&d), Err(NoMark::Required(3)));
            let mut d = good.clone();
            edit_entry(&mut d, 2, |e| e[55] |= 0x80);
            assert!(decide(&d).is_ok());
            // The mark moved to the data partition: that partition is the span.
            let mut d = good.clone();
            edit_entry(&mut d, 2, |e| {
                e[0..16].copy_from_slice(&xtask::guid(xtask::ORIGIN_DATA))
            });
            edit_entry(&mut d, 1, |e| e[0..16].copy_from_slice(&test_disk::MARK.0));
            let m = decide(&d).unwrap();
            assert_eq!((m.span.first, m.span.last), (10240, 18431));
        }

        #[test]
        fn a_no_mark_names_itself() {
            for (why, text) in [
                (NoMark::NotTheTestDisk, "this is not the boot test's disk"),
                (NoMark::NoProtectiveMbr, "LBA 0 is not a protective MBR"),
                (NoMark::NoGpt, "no valid GPT"),
                (
                    NoMark::OneCopy { primary: true },
                    "only the primary copy of the GPT is valid, and the kernel repairs nothing during boot",
                ),
                (
                    NoMark::OneCopy { primary: false },
                    "only the backup copy of the GPT is valid, and the kernel repairs nothing during boot",
                ),
                (NoMark::Disagree, "the GPT's two copies disagree"),
                (NoMark::Unmarked, "no partition carries it"),
                (NoMark::Marks(2), "2 partitions carry it"),
                (
                    NoMark::Required(3),
                    "partition 3, which carries it, is marked required",
                ),
            ] {
                assert_eq!(std::format!("{why}"), text);
            }
        }

        #[test]
        fn a_real_disk_does_not_hold_the_pattern() {
            let image = xtask::image();
            let area = area(&image);
            // Any other size or block format: not even read.
            assert!(!test_disk::geometry_matches(xtask::BLOCKS + 1, BS));
            assert!(!holds_pattern(xtask::BLOCKS + 1, BS, area));
            assert!(!holds_pattern(xtask::BLOCKS, 4096, area));
            // A blank disk, an all-ones disk.
            let zeros = std::vec![0u8; test_disk::PATTERN_BYTES];
            assert!(!holds_pattern(xtask::BLOCKS, BS, &zeros));
            let ones = std::vec![0xFFu8; test_disk::PATTERN_BYTES];
            assert!(!holds_pattern(xtask::BLOCKS, BS, &ones));
            // An area of the wrong length.
            assert!(!holds_pattern(xtask::BLOCKS, BS, &area[..512]));
            let longer = [area, &[0u8; 512]].concat();
            assert!(!holds_pattern(xtask::BLOCKS, BS, &longer));
        }

        /// A change of any byte of the pattern says no.
        #[test]
        fn a_change_of_any_pattern_byte_says_no() {
            let image = xtask::image();
            let mut a = area(&image).to_vec();
            for at in (0..test_disk::PATTERN_BYTES).step_by(251) {
                a[at] ^= 0x40;
                assert!(!holds_pattern(xtask::BLOCKS, BS, &a), "pattern byte {at}");
                a[at] ^= 0x40;
            }
            assert!(holds_pattern(xtask::BLOCKS, BS, &a), "restored");
        }
    }
}
