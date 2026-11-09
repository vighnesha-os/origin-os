//! The block layer (CLAUDE.md §39 the page cache and virtual filesystem foundations, §40):
//! a device-independent cache of fixed-size pages over the NVMe namespace, with LRU
//! eviction and write-back. The cache's bookkeeping is [`framekernel::blockcache`],
//! host-tested; here each slot is a 4 KiB DMA page, and the misses do the I/O
//! (`crate::storage`).
//!
//! A read returns the cached page, reading it from the disk on a miss (after writing back
//! the dirty page it evicts). A write does a read-modify-write of the page and marks it
//! dirty; a flush writes every dirty page back and flushes the controller. This is the
//! page cache a filesystem will sit on.
//!
//! Driven from the boot flow, over the polled I/O queue: the proof below holds the block
//! lock across the I/O it does. A production block layer routes each request to the
//! submitting processor's queue and blocks instead of polling (not done).
#![forbid(unsafe_code)]

use core::fmt::Write;

use framekernel::blockcache::{BlockCache, Request, Transfer};

use crate::console::Console;

/// A DMA page used as one cache slot: where the kernel reaches it, and the address the
/// controller uses (an IOVA in its IOMMU domain, or the physical address without one).
#[derive(Clone, Copy)]
struct Page {
    virt: u64,
    bus: u64,
}

/// The block cache over the bound NVMe namespace.
struct Block {
    index: BlockCache,
    slots: alloc::vec::Vec<Page>,
    /// Device block size, and device blocks per 4 KiB cache page.
    block_size: u64,
    page_blocks: u64,
    /// Device reads and writes the cache issued.
    reads: u64,
    writes: u64,
}

/// One of the cache's transfers on the device: a whole cache page between its slot and the
/// disk, through the polled I/O queue.
fn transfer(
    slots: &[Page],
    page_blocks: u64,
    reads: &mut u64,
    writes: &mut u64,
    request: Request,
) -> Transfer {
    let (write, page, slot) = match request {
        Request::WriteBack { slot, block } => {
            *writes += 1;
            (true, block, slot)
        }
        Request::Read { slot, block } => {
            *reads += 1;
            (false, block, slot)
        }
    };
    let Some(p) = slots.get(slot) else {
        return Transfer::Failed;
    };
    crate::storage::transfer(write, page * page_blocks, p.bus, page_blocks as u32)
}

impl Block {
    /// Builds a cache of `slots` pages over the bound namespace, or `None` if there is no
    /// controller or no memory.
    fn new(slots: usize) -> Option<Self> {
        let (block_size, _) = crate::storage::geometry()?;
        let block_size = u64::from(block_size);
        if block_size == 0 || !(crate::storage::PAGE).is_multiple_of(block_size) {
            return None;
        }
        let mut pages = alloc::vec::Vec::with_capacity(slots);
        for _ in 0..slots {
            let (virt, bus) = crate::storage::dma_page()?;
            pages.push(Page { virt, bus });
        }
        Some(Self {
            index: BlockCache::new(slots),
            slots: pages,
            block_size,
            page_blocks: crate::storage::PAGE / block_size,
            reads: 0,
            writes: 0,
        })
    }

    /// Makes cache page `page`'s data present in a slot, reading it from the disk on a miss
    /// (writing back the evicted dirty page first). Returns the slot, or `None` if that
    /// failed. The index decides what a failed or stuck transfer leaves behind
    /// ([`BlockCache::bring_in`]): a page a command never finished with is quarantined.
    fn bring_in(&mut self, page: u64) -> Option<usize> {
        self.index.bring_in(page, |request| {
            transfer(
                &self.slots,
                self.page_blocks,
                &mut self.reads,
                &mut self.writes,
                request,
            )
        })
    }

    /// Reads device block `lba` into `out` (one block). Returns whether it succeeded.
    fn read_block(&mut self, lba: u64, out: &mut [u8]) -> bool {
        let Some(slot) = self.bring_in(lba / self.page_blocks) else {
            return false;
        };
        let offset = ((lba % self.page_blocks) * self.block_size) as usize;
        crate::storage::dma_read(self.slots[slot].virt, offset, out);
        true
    }

    /// Writes `data` (one block) to device block `lba` through the cache (read-modify-write
    /// if the page is not resident): the page is dirtied, not yet written to the disk.
    fn write_block(&mut self, lba: u64, data: &[u8]) -> bool {
        let Some(slot) = self.bring_in(lba / self.page_blocks) else {
            return false;
        };
        let offset = ((lba % self.page_blocks) * self.block_size) as usize;
        crate::storage::dma_write(self.slots[slot].virt, offset, data);
        self.index.mark_dirty(slot);
        true
    }

    /// Writes every dirty page back and flushes the controller's cache. Returns whether
    /// all of it succeeded.
    fn flush(&mut self) -> bool {
        let written = self.index.write_back_all(|request| {
            transfer(
                &self.slots,
                self.page_blocks,
                &mut self.reads,
                &mut self.writes,
                request,
            )
        });
        written && crate::storage::flush_device()
    }
}

/// Proves the block cache against the NVMe test disk, and on no other: every step reads its
/// pattern, and the last one writes to it (CLAUDE.md P0; `storage` says when it is skipped):
/// - cold reads miss and fetch from the disk; re-reads of resident pages hit without I/O;
/// - reads return the pattern the boot test wrote;
/// - more pages than slots force LRU eviction;
/// - a write is buffered, a flush writes it back, and after eviction the block reads back
///   changed.
pub(crate) fn prove(com1: &mut Console) -> bool {
    if crate::storage::geometry().is_none() {
        let _ = writeln!(com1, "origin-kernel: block cache: no block device");
        return true;
    }
    if !crate::storage::on_test_disk() {
        return true;
    }
    const SLOTS: usize = 4;
    let Some(mut b) = Block::new(SLOTS) else {
        let _ = writeln!(
            com1,
            "origin-kernel: block cache: no memory: block cache failed"
        );
        return false;
    };
    let ppb = b.page_blocks;
    let mut buf = alloc::vec![0u8; b.block_size as usize];
    let mut expected = alloc::vec![0u8; b.block_size as usize];

    // Read every block of the first SLOTS pattern pages (LBA 2048 is page 2048/ppb).
    let first_page = 2048 / ppb;
    let mut matched = 0u64;
    let mut blocks = 0u64;
    for page in first_page..first_page + SLOTS as u64 {
        for k in 0..ppb {
            let lba = page * ppb + k;
            if !(2048..2048 + 64).contains(&lba) {
                continue; // only the 64 pattern blocks carry a pattern
            }
            if b.read_block(lba, &mut buf) {
                crate::storage::pattern(lba, &mut expected);
                if buf == expected {
                    matched += 1;
                }
            }
            blocks += 1;
        }
    }
    let cold_reads = b.reads;
    // Re-read the same pages: every access must hit, so no device read.
    for page in first_page..first_page + SLOTS as u64 {
        let lba = page * ppb;
        b.read_block(lba, &mut buf);
    }
    let (hits, misses) = b.index.stats();
    let reread_did_io = b.reads != cold_reads;
    let reads_ok = matched == blocks && blocks > 0 && cold_reads == SLOTS as u64 && !reread_did_io;
    let _ = writeln!(
        com1,
        "origin-kernel: block cache: {SLOTS} pages ({} B blocks, {ppb} per page): {matched} of {blocks} blocks matched the pattern; {cold_reads} device reads cold, re-reading {SLOTS} resident pages did {} more ({hits} hits, {misses} misses){}",
        b.block_size,
        b.reads - cold_reads,
        if reads_ok { "" } else { ": block cache failed" }
    );

    // A fifth page evicts the least-recently-used one (all clean: no write-back).
    let before = (b.reads, b.writes);
    b.read_block((first_page + SLOTS as u64) * ppb, &mut buf);
    let evict_ok = b.reads == before.0 + 1 && b.writes == before.1;

    // Write a block in the data partition, flush, evict it, and read it back.
    let scratch = 10_240; // first block of "Origin data"
    crate::storage::pattern(scratch ^ 0x3C3C, &mut expected);
    let wrote = b.write_block(scratch, &expected);
    let writes_before_flush = b.writes;
    let flushed = b.flush();
    let flush_wrote = b.writes > writes_before_flush;
    // Touch SLOTS other pages so the scratch page is evicted, then read it back.
    for i in 0..SLOTS as u64 + 1 {
        b.read_block((first_page + 10 + i) * ppb, &mut buf);
    }
    let read_back = b.read_block(scratch, &mut buf) && buf == expected;
    let write_ok = wrote && flushed && flush_wrote && read_back;
    let _ = writeln!(
        com1,
        "origin-kernel: block cache: a 5th page evicted the LRU clean page (one read, no write-back: {evict_ok}); wrote LBA {scratch}, flushed ({} device writes), evicted and read back changed: {read_back}{}",
        b.writes - writes_before_flush,
        if write_ok && evict_ok {
            ""
        } else {
            ": block cache failed"
        }
    );
    reads_ok && evict_ok && write_ok
}
