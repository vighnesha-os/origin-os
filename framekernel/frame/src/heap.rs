//! The kernel heap's bookkeeping (CLAUDE.md §28 kernel heap; §13: `alloc` only once a real
//! allocator exists).
//!
//! Small allocations (up to [`MAX_BLOCK`] bytes) come from *slabs*: one 4 KiB frame split
//! into equal power-of-two blocks of 16 to 2048 bytes. Each slab's free blocks are tracked
//! in a bitmap kept beside the slab, never inside it, so this module does arithmetic on
//! addresses only: it never reads or writes heap memory, needs no `unsafe`, and is tested on
//! the host. Larger allocations, and any alignment above 2048 bytes, are runs of whole
//! frames from a [`PageSource`]. A slab whose last block is freed returns its frame at once.
//!
//! Every free is checked: the address must lie in a known slab of the right size class, on
//! a block boundary, and the block must be allocated. A large free must not name a slab
//! frame. The frame allocator then checks large runs itself.
#![forbid(unsafe_code)]

/// Smallest block.
pub const MIN_BLOCK: u64 = 16;
/// Largest slab block; anything bigger is a run of frames.
pub const MAX_BLOCK: u64 = 2048;
/// Size of a slab and of a frame.
pub const PAGE: u64 = 4096;
/// Bitmap words per slab: 4096 / 16 = 256 blocks at most.
const WORDS: usize = 4;

/// One slab's bookkeeping, 48 bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Slab {
    page: u64,
    block: u32,
    used: u32,
    bitmap: [u64; WORDS],
}

impl Slab {
    /// An unused table entry, for static slab tables.
    pub const ZERO: Self = Self {
        page: 0,
        block: 0,
        used: 0,
        bitmap: [0; WORDS],
    };
}

/// Where slab frames and large runs come from: the frame allocator in the kernel.
pub trait PageSource {
    /// `count` contiguous frames, the first aligned to `align` frames; the address of the
    /// first, or `None`.
    fn allocate_pages(&mut self, count: u64, align: u64) -> Option<u64>;
    /// Returns frames from [`Self::allocate_pages`]; `false` if the source refused.
    fn free_pages(&mut self, addr: u64, count: u64) -> bool;
}

/// Why a free was refused. Each one means the caller broke the allocation contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeapError {
    /// No slab holds this address, or a large free named a slab frame.
    NotAllocated(u64),
    /// The slab's block size does not match the layout being freed.
    WrongLayout(u64),
    /// The address is not on a block boundary.
    Misaligned(u64),
    /// The block is already free.
    DoubleFree(u64),
    /// The page source refused to take a large run back.
    Pages(u64),
}

/// Running totals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeapStats {
    /// Bytes handed out, rounded up to block or page size.
    pub bytes_in_use: u64,
    /// Slabs (frames) holding small blocks.
    pub slabs: u64,
    /// Frames in large runs.
    pub large_pages: u64,
}

/// What [`Heap::allocate`] does for a layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Plan {
    Block(u64),
    Run { pages: u64, align: u64 },
}

/// The plan for `size` bytes aligned to `align` (a power of two), or `None` if it cannot
/// be represented.
fn plan(size: u64, align: u64) -> Option<Plan> {
    if !align.is_power_of_two() {
        return None;
    }
    let block = size.max(align).max(MIN_BLOCK).checked_next_power_of_two()?;
    if block <= MAX_BLOCK {
        Some(Plan::Block(block))
    } else {
        Some(Plan::Run {
            pages: size.max(1).div_ceil(PAGE),
            align: (align / PAGE).max(1),
        })
    }
}

/// The kernel heap: slab bookkeeping in caller-provided storage, sorted by frame address.
#[derive(Debug)]
pub struct Heap<'a> {
    slabs: &'a mut [Slab],
    len: usize,
    stats: HeapStats,
}

impl<'a> Heap<'a> {
    /// A heap whose slab table lives in `storage` (its capacity bounds the slab count).
    pub const fn new(storage: &'a mut [Slab]) -> Self {
        Self {
            slabs: storage,
            len: 0,
            stats: HeapStats {
                bytes_in_use: 0,
                slabs: 0,
                large_pages: 0,
            },
        }
    }

    /// Running totals.
    pub const fn stats(&self) -> HeapStats {
        self.stats
    }

    /// Index of the slab for frame `page`, or where it would be inserted.
    fn find(&self, page: u64) -> Result<usize, usize> {
        self.slabs
            .get(..self.len)
            .unwrap_or(&[])
            .binary_search_by_key(&page, |s| s.page)
    }

    /// Allocates `size` bytes aligned to `align`; returns the address or `None`.
    pub fn allocate(&mut self, source: &mut impl PageSource, size: u64, align: u64) -> Option<u64> {
        match plan(size, align)? {
            Plan::Block(block) => self.allocate_block(source, block),
            Plan::Run { pages, align } => {
                let addr = source.allocate_pages(pages, align)?;
                self.stats.large_pages += pages;
                self.stats.bytes_in_use += pages * PAGE;
                Some(addr)
            }
        }
    }

    fn allocate_block(&mut self, source: &mut impl PageSource, block: u64) -> Option<u64> {
        let blocks = PAGE / block;
        let len = self.len;
        let found = self
            .slabs
            .get_mut(..len)?
            .iter_mut()
            .find(|s| u64::from(s.block) == block && u64::from(s.used) < blocks);
        let slab = match found {
            Some(slab) => slab,
            None => self.new_slab(source, block)?,
        };
        let index = take_free_bit(&mut slab.bitmap, blocks)?;
        slab.used += 1;
        let addr = slab.page + index * block;
        self.stats.bytes_in_use += block;
        Some(addr)
    }

    /// Adds an empty slab of `block`-byte blocks, keeping the table sorted.
    fn new_slab(&mut self, source: &mut impl PageSource, block: u64) -> Option<&mut Slab> {
        if self.len >= self.slabs.len() {
            return None;
        }
        let page = source.allocate_pages(1, 1)?;
        let Err(at) = self.find(page) else {
            // The source handed out a frame that is already a slab: refuse to alias it.
            source.free_pages(page, 1);
            return None;
        };
        let len = self.len;
        let table = self.slabs.get_mut(at..=len)?;
        table.rotate_right(1);
        let slab = table.first_mut()?;
        *slab = Slab {
            page,
            block: u32::try_from(block).ok()?,
            used: 0,
            bitmap: [0; WORDS],
        };
        self.len += 1;
        self.stats.slabs += 1;
        Some(slab)
    }

    /// Frees `addr`, allocated with the same `size` and `align`.
    pub fn free(
        &mut self,
        source: &mut impl PageSource,
        addr: u64,
        size: u64,
        align: u64,
    ) -> Result<(), HeapError> {
        let page = addr & !(PAGE - 1);
        match plan(size, align).ok_or(HeapError::WrongLayout(addr))? {
            Plan::Run { pages, .. } => {
                if self.find(page).is_ok() || addr != page {
                    return Err(HeapError::NotAllocated(addr));
                }
                if !source.free_pages(addr, pages) {
                    return Err(HeapError::Pages(addr));
                }
                self.stats.large_pages -= pages;
                self.stats.bytes_in_use -= pages * PAGE;
                Ok(())
            }
            Plan::Block(block) => {
                let at = self.find(page).map_err(|_| HeapError::NotAllocated(addr))?;
                let slab = self
                    .slabs
                    .get_mut(at)
                    .ok_or(HeapError::NotAllocated(addr))?;
                if u64::from(slab.block) != block {
                    return Err(HeapError::WrongLayout(addr));
                }
                let offset = addr - page;
                if !offset.is_multiple_of(block) {
                    return Err(HeapError::Misaligned(addr));
                }
                let index = offset / block;
                let (word, bit) = ((index / 64) as usize, index % 64);
                let w = slab
                    .bitmap
                    .get_mut(word)
                    .ok_or(HeapError::Misaligned(addr))?;
                if *w & (1 << bit) == 0 {
                    return Err(HeapError::DoubleFree(addr));
                }
                *w &= !(1 << bit);
                slab.used -= 1;
                self.stats.bytes_in_use -= block;
                if slab.used == 0 {
                    self.release(source, at);
                }
                Ok(())
            }
        }
    }

    /// Removes the empty slab at `at` and returns its frame.
    fn release(&mut self, source: &mut impl PageSource, at: usize) {
        let len = self.len;
        if let Some(table) = self.slabs.get_mut(at..len) {
            let page = table.first().map_or(0, |s| s.page);
            table.rotate_left(1);
            self.len -= 1;
            self.stats.slabs -= 1;
            // The source gave this frame out; were it to refuse it back, the frame only
            // stays allocated, which is safe.
            let _ = source.free_pages(page, 1);
        }
    }
}

/// Sets and returns the first clear bit below `limit`, or `None` if all are set.
fn take_free_bit(bitmap: &mut [u64; WORDS], limit: u64) -> Option<u64> {
    for (i, word) in bitmap.iter_mut().enumerate() {
        let base = i as u64 * 64;
        if base >= limit {
            break;
        }
        let free = !*word;
        if free != 0 {
            let bit = u64::from(free.trailing_zeros());
            if base + bit < limit {
                *word |= 1 << bit;
                return Some(base + bit);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Hands out fake frame addresses and checks every return.
    #[derive(Default)]
    struct Frames {
        next: u64,
        out: BTreeMap<u64, u64>,
    }

    impl PageSource for Frames {
        fn allocate_pages(&mut self, count: u64, align: u64) -> Option<u64> {
            let start = (self.next + 0x10_0000).div_ceil(align * PAGE) * align * PAGE;
            self.next = start + count * PAGE - 0x10_0000 + PAGE; // leave a guard gap
            self.out.insert(start, count);
            Some(start)
        }

        fn free_pages(&mut self, addr: u64, count: u64) -> bool {
            self.out.remove(&addr) == Some(count)
        }
    }

    fn heap(storage: &mut [Slab]) -> Heap<'_> {
        Heap::new(storage)
    }

    #[test]
    fn plans_cover_sizes_and_alignments() {
        assert_eq!(plan(1, 1), Some(Plan::Block(16)));
        assert_eq!(plan(17, 8), Some(Plan::Block(32)));
        assert_eq!(plan(24, 64), Some(Plan::Block(64)));
        assert_eq!(plan(2048, 8), Some(Plan::Block(2048)));
        assert_eq!(plan(2049, 8), Some(Plan::Run { pages: 1, align: 1 }));
        assert_eq!(plan(8, 4096), Some(Plan::Run { pages: 1, align: 1 }));
        assert_eq!(plan(10_000, 16_384), Some(Plan::Run { pages: 3, align: 4 }));
        assert_eq!(plan(0, 1), Some(Plan::Block(16)));
        assert_eq!(plan(8, 3), None);
        assert_eq!(plan(u64::MAX, 8), None);
    }

    #[test]
    fn blocks_never_overlap_and_respect_alignment() {
        // Up to two 2048-byte blocks per slab: room for every allocation below.
        let mut storage = vec![Slab::default(); 2048];
        let mut h = heap(&mut storage);
        let mut src = Frames::default();
        let mut live: BTreeMap<u64, u64> = BTreeMap::new(); // address -> bytes occupied
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        for _ in 0..3000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let size = 1 + state % 3000;
            let align = 1 << ((state >> 40) % 8);
            let addr = h.allocate(&mut src, size, align).unwrap();
            assert!(
                addr.is_multiple_of(align),
                "{addr:#x} not aligned to {align}"
            );
            let occupied = match plan(size, align).unwrap() {
                Plan::Block(block) => block,
                Plan::Run { pages, .. } => pages * PAGE,
            };
            assert!(occupied >= size);
            if let Some((&prev, &len)) = live.range(..=addr).next_back() {
                assert!(prev + len <= addr, "{addr:#x} overlaps {prev:#x}");
            }
            if let Some((&next, _)) = live.range(addr + 1..).next() {
                assert!(addr + occupied <= next, "{addr:#x} overlaps {next:#x}");
            }
            live.insert(addr, occupied);
        }
        let in_use: u64 = live.values().sum();
        assert_eq!(h.stats().bytes_in_use, in_use);
    }

    #[test]
    fn frees_return_slabs_and_runs() {
        let mut storage = [Slab::default(); 16];
        let mut h = heap(&mut storage);
        let mut src = Frames::default();
        let a: Vec<u64> = (0..300)
            .map(|_| h.allocate(&mut src, 16, 8).unwrap())
            .collect();
        assert_eq!(h.stats().slabs, 2, "256 blocks per 16-byte slab");
        let big = h.allocate(&mut src, 10_000, 8).unwrap();
        assert_eq!(h.stats().large_pages, 3);
        assert_eq!(h.stats().bytes_in_use, 300 * 16 + 3 * PAGE);
        for &x in &a {
            h.free(&mut src, x, 16, 8).unwrap();
        }
        h.free(&mut src, big, 10_000, 8).unwrap();
        assert_eq!(h.stats(), HeapStats::default());
        assert!(src.out.is_empty(), "every frame went back: {:?}", src.out);
    }

    #[test]
    fn bad_frees_are_refused_and_change_nothing() {
        let mut storage = [Slab::default(); 16];
        let mut h = heap(&mut storage);
        let mut src = Frames::default();
        let x = h.allocate(&mut src, 40, 8).unwrap(); // a 64-byte block
        let y = h.allocate(&mut src, 40, 8).unwrap();
        let before = h.stats();
        assert_eq!(h.free(&mut src, x, 100, 8), Err(HeapError::WrongLayout(x)));
        assert_eq!(
            h.free(&mut src, x + 8, 40, 8),
            Err(HeapError::Misaligned(x + 8))
        );
        assert_eq!(
            h.free(&mut src, x + 64 * 5, 40, 8),
            Err(HeapError::DoubleFree(x + 320))
        );
        assert_eq!(
            h.free(&mut src, 0xDEAD_0000, 40, 8),
            Err(HeapError::NotAllocated(0xDEAD_0000))
        );
        // A large free that names a slab frame.
        let page = x & !(PAGE - 1);
        assert_eq!(
            h.free(&mut src, page, 8000, 8),
            Err(HeapError::NotAllocated(page))
        );
        assert_eq!(h.stats(), before);
        h.free(&mut src, x, 40, 8).unwrap();
        assert_eq!(h.free(&mut src, x, 40, 8), Err(HeapError::DoubleFree(x)));
        h.free(&mut src, y, 40, 8).unwrap();
        // The slab is gone, so its frame no longer counts as a slab.
        assert_eq!(h.free(&mut src, y, 40, 8), Err(HeapError::NotAllocated(y)));
    }

    #[test]
    fn a_full_slab_table_fails_the_allocation_cleanly() {
        let mut storage = [Slab::default(); 2];
        let mut h = heap(&mut storage);
        let mut src = Frames::default();
        assert!(h.allocate(&mut src, 2048, 8).is_some());
        assert!(h.allocate(&mut src, 2048, 8).is_some());
        assert!(h.allocate(&mut src, 1024, 8).is_some()); // second slab
        assert!(
            h.allocate(&mut src, 2048, 8).is_none(),
            "both slabs used up"
        );
        assert_eq!(src.out.len(), 2, "no frame leaked by the failed allocation");
    }
}
