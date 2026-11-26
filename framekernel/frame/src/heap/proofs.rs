//! Kani's proofs of the kernel heap's bookkeeping (CLAUDE.md §73; TASKS item 143). Built only
//! by `cargo kani` (`cfg(kani)`), which no build of the kernel, the loader or any image sets.
//!
//! # What is proved, and within which bounds
//!
//! **The invariant** ([`check_whole`]):
//! - *the table:* the live slabs (`..len`) are sorted by frame, strictly, so `find`'s binary
//!   search is right, and each frame is 4 KiB aligned;
//! - *each slab:* its block is a power of two from 16 to 2048 bytes; no bit at or past its
//!   block count is set; `used` is the number of set bits, and at least 1 (an empty slab is
//!   given back at once);
//! - *the totals:* `stats.slabs` is the number of live slabs, and `stats.bytes_in_use` is the
//!   live blocks' bytes plus `stats.large_pages` whole frames.
//!
//! **The method.** `Heap::new` makes an empty table, which satisfies the invariant at once
//! (no slab, totals zero). Every other harness starts from **any** state that satisfies it
//! ([`any_heap`]: each live slab's frame, block size and bitmap, the table's length, and
//! the large frames are all `kani::any()`), runs one method on arbitrary arguments, and proves
//! the invariant holds after it, with the method's own postcondition. By induction on the
//! number of calls, it then holds after any sequence of calls of the methods so covered,
//! within the bounds below. Every `&mut self` method of `Heap` is covered:
//!
//! | Method | Harness |
//! |---|---|
//! | `new` (establishes it) | by construction: no slab, totals zero |
//! | `allocate` | [`allocate_keeps_the_heap_whole`] |
//! | `free` | [`free_takes_back_only_a_live_block`] |
//! | `allocate_block`, `new_slab` (private; called only by `allocate`) | through `allocate` |
//! | `release` (private; called only by `free`) | through `free` |
//!
//! **Core's rotation, stubbed.** `new_slab` and `release` keep the table sorted with
//! `rotate_right(1)` and `rotate_left(1)`. In the three harnesses that reach them, Kani stubs
//! both with plain walks ([`rotate_right_by_walk`], [`rotate_left_by_walk`]), which
//! [`the_rotation_stubs_equal_core_s`] proves equal to core's for tables of 1 to 3 entries. So
//! those proofs rest on core's rotation doing what its documentation says: an assumption, not
//! a result.
//!
//! Two pure functions are proved for **every** input, with no bound: `plan`
//! ([`plan_fits_every_layout`]) and `take_free_bit` ([`take_free_bit_takes_the_lowest_free_bit`]).
//! [`two_allocations_never_overlap`] states "no byte is handed out twice" directly.
//!
//! **Kani's own checks** hold in each harness too: no arithmetic overflow, no index out of
//! bounds, no panic, and every loop unwound within its bound.
//!
//! **The bounds:** a slab table of 2 entries. Each slab's bitmap is its whole 4 words, so a
//! slab's size is not bounded.
//!
//! **Assumed, not proved: the page source's contract** ([`Source`]). It hands out frames
//! aligned as asked, that do not wrap past the end of memory and that no large run of the
//! heap holds, or `None`. In [`allocate_keeps_the_heap_whole`], when the heap asks for a slab's
//! frame, it may hand out a frame the heap already holds as a slab, which the heap must refuse
//! and give back: the one check the heap makes of its source. For a large run the heap trusts
//! the source, and a source that broke the contract there could make a run overlap a slab. The
//! source takes back what it gave, and takes a large run back only while the heap's count of
//! large frames covers it. The frames the heap holds number below 2^52, as they must in a
//! 64-bit address space. The frame allocator's own proofs (`frames::proofs`) cover the
//! kernel's source, the frame allocator, within their bounds and except its `reclaim`; the
//! glue between the two (`memory.rs`) is not proved. That the heap's caller frees only what
//! it allocated, with the same layout, is `GlobalAlloc`'s contract on its caller.
//!
//! **Not proved:** tables past 2 slabs; `memory.rs` (the lock, `GlobalAlloc`, the pointers);
//! concurrency; the machine code rustc 1.100.0 makes: Kani compiles with its own nightly
//! (`nightly-2026-08-21`) for the host it runs on.
#![forbid(unsafe_code)]

use super::{
    Heap, HeapError, HeapStats, MAX_BLOCK, MIN_BLOCK, PAGE, PageSource, Plan, Slab, WORDS, plan,
    take_free_bit,
};

/// The slab table's entries.
const SLABS: usize = 2;
/// Frames the heap may hold at most: every frame of a 64-bit address space but one.
const HELD_MAX: u64 = (1 << 52) - 1;

/// Blocks of `block` bytes in one slab.
fn blocks(block: u32) -> u64 {
    PAGE >> block.trailing_zeros()
}

/// The bits of bitmap word `word` that stand for blocks below `n`. Every walk over a bitmap
/// here is by word, four of them, so CBMC unrolls no loop past its length.
fn below(word: usize, n: u64) -> u64 {
    let base = word as u64 * 64;
    if n <= base {
        0
    } else if n - base >= 64 {
        u64::MAX
    } else {
        (1 << (n - base)) - 1
    }
}

/// The bit of bitmap word `word` that stands for block `index`, or 0.
fn only(word: usize, index: u64) -> u64 {
    below(word, index.saturating_add(1)) & !below(word, index)
}

/// Set bits in a slab's bitmap.
fn set_bits(bitmap: &[u64; WORDS]) -> u64 {
    bitmap.iter().map(|w| u64::from(w.count_ones())).sum()
}

/// Whether bit `index` is set.
fn bit(bitmap: &[u64; WORDS], index: u64) -> bool {
    (0..WORDS).any(|i| bitmap[i] & only(i, index) != 0)
}

/// Whether `slab` is whole by itself: frame aligned, a block size of the heap's, no bit past
/// its blocks, and `used` its set bits, at least 1.
fn slab_whole(slab: &Slab) -> bool {
    let size_ok = slab.block.is_power_of_two()
        && u64::from(slab.block) >= MIN_BLOCK
        && u64::from(slab.block) <= MAX_BLOCK;
    size_ok
        && slab.page % PAGE == 0
        && (0..WORDS).all(|i| slab.bitmap[i] & !below(i, blocks(slab.block)) == 0)
        && u64::from(slab.used) == set_bits(&slab.bitmap)
        && slab.used >= 1
}

/// The live blocks' bytes of the first `len` slabs of `table`.
fn block_bytes(table: &[Slab; SLABS], len: usize) -> u64 {
    (0..SLABS)
        .filter(|&i| i < len)
        .map(|i| u64::from(table[i].used) * u64::from(table[i].block))
        .sum()
}

/// Whether two slabs are equal, field by field and word by word.
fn slab_eq(a: &Slab, b: &Slab) -> bool {
    a.page == b.page
        && a.block == b.block
        && a.used == b.used
        && (0..WORDS).all(|w| a.bitmap[w] == b.bitmap[w])
}

/// Whether the first `len` entries of two tables are equal.
fn same(a: &[Slab], b: &[Slab; SLABS], len: usize) -> bool {
    (0..SLABS).all(|i| i >= len || slab_eq(&a[i], &b[i]))
}

/// Whether `value` is a multiple of `power`, a power of two.
fn aligned(value: u64, power: u64) -> bool {
    value & (power - 1) == 0
}

/// A copy of a heap's table.
fn table(h: &Heap<'_>) -> [Slab; SLABS] {
    [h.slabs[0], h.slabs[1]]
}

/// Asserts the invariant: the table, each slab, and the totals.
fn check_whole(h: &Heap<'_>) {
    let t = table(h);
    kani::assert(h.len <= SLABS, "the table: its length fits its storage");
    kani::assert(
        (1..SLABS).all(|i| i >= h.len || t[i - 1].page < t[i].page),
        "the table: sorted by frame, strictly",
    );
    kani::assert(
        (0..SLABS).all(|i| i >= h.len || slab_whole(&t[i])),
        "each slab: whole",
    );
    kani::assert(
        h.stats.slabs == h.len as u64
            && h.stats.bytes_in_use == block_bytes(&t, h.len) + h.stats.large_pages * PAGE,
        "the totals: slabs and bytes in use are counted",
    );
}

fn any_slab() -> Slab {
    let block = 1u32 << kani::any_where(|&s: &u32| (4..=11).contains(&s));
    let n = blocks(block);
    let mut bitmap: [u64; WORDS] = kani::any();
    for (i, word) in bitmap.iter_mut().enumerate() {
        *word &= below(i, n);
    }
    let used = set_bits(&bitmap) as u32;
    Slab {
        page: kani::any::<u64>() & !(PAGE - 1),
        block,
        used,
        bitmap,
    }
}

/// Any heap state that satisfies the invariant, over `storage`. The entries past `len` are
/// left as drawn: nothing may depend on them.
fn any_heap(storage: &mut [Slab; SLABS]) -> Heap<'_> {
    *storage = [any_slab(), any_slab()];
    let len = kani::any_where(|&n: &usize| n <= SLABS);
    kani::assume((0..SLABS).all(|i| i >= len || storage[i].used >= 1));
    kani::assume((1..SLABS).all(|i| i >= len || storage[i - 1].page < storage[i].page));
    let large_pages = kani::any_where(|&p: &u64| p.saturating_add(len as u64) <= HELD_MAX);
    let bytes = block_bytes(storage, len);
    Heap {
        slabs: storage,
        len,
        stats: HeapStats {
            bytes_in_use: bytes + large_pages * PAGE,
            slabs: len as u64,
            large_pages,
        },
    }
}

/// A page source that keeps the contract the heap relies on (the module's documentation),
/// and counts what it hands out and takes back.
struct Source {
    /// The slab frames the heap held at the start: the source may hand one out again.
    slab_pages: [u64; SLABS],
    live: usize,
    /// Runs handed out during the harness, as (address, frames).
    given: [(u64, u64); 2],
    handed: usize,
    /// Frames the heap holds, slabs and large runs, kept below `HELD_MAX`.
    held: u64,
    /// The heap's large frames that a free may still take back.
    credit: u64,
    /// Frames handed out, and frames taken back, during the harness.
    out: u64,
    back: u64,
    /// Times each starting slab's frame was taken back.
    slab_back: [u32; SLABS],
    /// Whether it may hand out a frame the heap holds as a slab: set only where the heap asks
    /// for a slab's frame, the one request it checks (`new_slab`).
    may_alias: bool,
}

impl Source {
    fn new(h: &Heap<'_>) -> Self {
        let t = table(h);
        Self {
            slab_pages: [t[0].page, t[1].page],
            live: h.len,
            given: [(0, 0); 2],
            handed: 0,
            held: h.stats.large_pages + h.len as u64,
            credit: h.stats.large_pages,
            out: 0,
            back: 0,
            slab_back: [0; SLABS],
            may_alias: false,
        }
    }

    /// Whether `[addr, addr + bytes)` overlaps a starting slab's frame or a run handed out.
    fn overlaps(&self, addr: u64, bytes: u64) -> bool {
        let meets = |start: u64, len: u64| addr < start.saturating_add(len) && start < addr + bytes;
        (0..SLABS).any(|i| i < self.live && meets(self.slab_pages[i], PAGE))
            || (0..2).any(|i| i < self.handed && meets(self.given[i].0, self.given[i].1 * PAGE))
    }

    /// The starting slab whose frame is `addr`.
    fn slab_at(&self, addr: u64) -> Option<usize> {
        (0..SLABS).find(|&i| i < self.live && self.slab_pages[i] == addr)
    }
}

impl PageSource for Source {
    fn allocate_pages(&mut self, count: u64, align: u64) -> Option<u64> {
        if kani::any() || self.handed == 2 {
            return None;
        }
        if self.may_alias && count == 1 && self.live > 0 && kani::any() {
            // A faulty source: a frame the heap already holds as a slab.
            let i = kani::any_where(|&i: &usize| i < self.live);
            self.out += 1;
            return Some(self.slab_pages[i]);
        }
        let bytes = count.checked_mul(PAGE)?;
        let step = align.max(1).checked_mul(PAGE)?;
        if self
            .held
            .checked_add(count)
            .is_none_or(|held| held > HELD_MAX)
        {
            return None;
        }
        kani::assert(
            step.is_power_of_two(),
            "the heap asks for runs aligned to a power of two",
        );
        let addr: u64 = kani::any();
        kani::assume(aligned(addr, step) && addr.checked_add(bytes).is_some());
        kani::assume(!self.overlaps(addr, bytes));
        self.given[self.handed] = (addr, count);
        self.handed += 1;
        self.held += count;
        self.out += count;
        Some(addr)
    }

    fn free_pages(&mut self, addr: u64, count: u64) -> bool {
        if let Some(i) = self.slab_at(addr) {
            // A slab's frame, released, or refused as an alias.
            kani::assert(count == 1, "a slab's frame goes back as one frame");
            self.slab_back[i] += 1;
            self.back += 1;
            return true;
        }
        if let Some(i) = (0..2).find(|&i| i < self.handed && self.given[i] == (addr, count)) {
            // A run or a slab's frame handed out during the harness.
            self.given[i] = (0, 0);
            self.back += count;
            return true;
        }
        // A large run the heap held at the start: taken back only while its count covers it.
        if kani::any() && count <= self.credit {
            self.credit -= count;
            self.back += count;
            return true;
        }
        false
    }
}

/// Whether block `index` of the slab at `page`, of `block` bytes, is live in the first `len`
/// slabs of `t`.
fn live_block(t: &[Slab; SLABS], len: usize, page: u64, block: u32, index: u64) -> bool {
    (0..SLABS)
        .any(|i| i < len && t[i].page == page && t[i].block == block && bit(&t[i].bitmap, index))
}

/// Whether every live block of the first `from_len` slabs of `from` is live in the first
/// `to_len` of `to`.
fn kept_blocks(from: &[Slab; SLABS], from_len: usize, to: &[Slab; SLABS], to_len: usize) -> bool {
    (0..SLABS).all(|i| {
        i >= from_len
            || (0..SLABS).any(|j| {
                j < to_len
                    && to[j].page == from[i].page
                    && to[j].block == from[i].block
                    && (0..WORDS).all(|w| to[j].bitmap[w] & from[i].bitmap[w] == from[i].bitmap[w])
            })
    })
}

/// The live blocks of the first `len` slabs of `t`.
fn live_blocks(t: &[Slab; SLABS], len: usize) -> u64 {
    (0..SLABS)
        .filter(|&i| i < len)
        .map(|i| u64::from(t[i].used))
        .sum()
}

/// Core's `rotate_right` on a table, as a plain walk: the heap's harnesses stub
/// `<[Slab]>::rotate_right` with it (Kani's `-Z stubbing`). Core's rotation moves entries by
/// pointers to offsets CBMC cannot fix, which made those harnesses' formulas several times
/// larger; this walk swaps neighbours by index. It is generic because Kani requires a stub to
/// take the generic parameters of the method it replaces. It rotates by at most one, as
/// `new_slab` does, and [`the_rotation_stubs_equal_core_s`] proves it equal to core's for
/// tables of 1 to 3 entries.
fn rotate_right_by_walk<T>(table: &mut [T], k: usize) {
    kani::assert(k <= table.len(), "a rotation by at most the table's length");
    if k == 0 {
        return;
    }
    kani::assert(k == 1, "the heap rotates by one");
    let mut i = table.len() - 1;
    while i > 0 {
        table.swap(i - 1, i);
        i -= 1;
    }
}

/// Core's `rotate_left` on a table, as a plain walk, as [`rotate_right_by_walk`] is for
/// `rotate_right`: what `release` does.
fn rotate_left_by_walk<T>(table: &mut [T], k: usize) {
    kani::assert(k <= table.len(), "a rotation by at most the table's length");
    if k == 0 {
        return;
    }
    kani::assert(k == 1, "the heap rotates by one");
    let mut i = 1;
    while i < table.len() {
        table.swap(i - 1, i);
        i += 1;
    }
}

/// An entry of any contents, valid or not.
fn any_entry() -> Slab {
    Slab {
        page: kani::any(),
        block: kani::any(),
        used: kani::any(),
        bitmap: kani::any(),
    }
}

/// Core's `rotate_right` and `rotate_left`, by 0 and by 1, equal the two walks the heap's
/// harnesses stub them with, on tables of 1 to 3 entries of any contents. The heap's proofs
/// then rest on core's rotation being correct, as its documentation states: an assumption the
/// records name, and not proved here for longer tables.
#[kani::proof]
#[kani::unwind(5)]
fn the_rotation_stubs_equal_core_s() {
    let table = [any_entry(), any_entry(), any_entry()];
    for len in 1..=3 {
        for k in 0..=1 {
            let (mut core_right, mut walk_right) = (table, table);
            core_right[..len].rotate_right(k);
            rotate_right_by_walk(&mut walk_right[..len], k);
            let (mut core_left, mut walk_left) = (table, table);
            core_left[..len].rotate_left(k);
            rotate_left_by_walk(&mut walk_left[..len], k);
            kani::assert(
                (0..3).all(|i| {
                    slab_eq(&core_right[i], &walk_right[i]) && slab_eq(&core_left[i], &walk_left[i])
                }),
                "the walks rotate as core's rotation does",
            );
        }
    }
    kani::cover(!slab_eq(&table[0], &table[1]), "entries that differ");
}

/// `plan`, for every size and every alignment: it refuses exactly an alignment that is not a
/// power of two and a size whose block would not fit in 64 bits; a block is a power of two
/// from 16 to 2048 bytes, at least the size and the alignment; a run is at least one frame,
/// holds the size, and its frame alignment meets the byte alignment.
#[kani::proof]
fn plan_fits_every_layout() {
    let size: u64 = kani::any();
    let align: u64 = kani::any();
    match plan(size, align) {
        None => {
            kani::cover(true, "a layout refused");
            kani::assert(
                !align.is_power_of_two() || size.max(align) > 1 << 63,
                "plan refuses only what it cannot represent",
            );
        }
        Some(Plan::Block(block)) => {
            kani::cover(true, "a block");
            kani::assert(
                align.is_power_of_two()
                    && block.is_power_of_two()
                    && (MIN_BLOCK..=MAX_BLOCK).contains(&block)
                    && block >= size
                    && block >= align,
                "a block holds the size and meets the alignment",
            );
        }
        Some(Plan::Run {
            pages,
            align: frames,
        }) => {
            kani::cover(true, "a run");
            kani::assert(
                align.is_power_of_two()
                    && size.max(align) > MAX_BLOCK
                    && pages >= 1
                    && pages.checked_mul(PAGE).is_some_and(|bytes| bytes >= size)
                    && frames.is_power_of_two()
                    && frames
                        .checked_mul(PAGE)
                        .is_some_and(|bytes| bytes % align == 0),
                "a run holds the size and meets the alignment",
            );
        }
    }
}

/// `take_free_bit`, for every bitmap and every limit: it takes the lowest clear bit below
/// the limit (and below 256) and sets it alone, or returns `None` with nothing changed when
/// every such bit is set.
#[kani::proof]
#[kani::unwind(6)]
fn take_free_bit_takes_the_lowest_free_bit() {
    let mut bitmap: [u64; WORDS] = kani::any();
    let before = bitmap;
    let limit: u64 = kani::any();
    let top = limit.min(256);
    match take_free_bit(&mut bitmap, limit) {
        Some(index) => {
            kani::cover(index > 64, "a bit past the first word");
            kani::assert(
                index < top
                    && !bit(&before, index)
                    && (0..WORDS).all(|i| before[i] & below(i, index) == below(i, index)),
                "the lowest clear bit below the limit",
            );
            kani::assert(
                (0..WORDS).all(|i| bitmap[i] == before[i] | only(i, index)),
                "only that bit is set",
            );
        }
        None => {
            kani::cover(top > 0, "a full bitmap");
            kani::assert(
                (0..WORDS)
                    .all(|i| before[i] & below(i, top) == below(i, top) && bitmap[i] == before[i]),
                "none only when every bit below the limit is set, with nothing changed",
            );
        }
    }
}

/// `allocate`, from any valid heap, of any size and alignment: an address it returns meets
/// the alignment; a block lies in a live slab of its plan's size, its bit was clear and is now
/// set, and every live block stays live; a run is counted. A failed allocation changes
/// nothing, and gives back every frame it took. The invariant holds after.
#[kani::proof]
#[kani::unwind(6)]
#[kani::stub(<[Slab]>::rotate_right, rotate_right_by_walk)]
#[kani::stub(<[Slab]>::rotate_left, rotate_left_by_walk)]
fn allocate_keeps_the_heap_whole() {
    let mut storage = [Slab::ZERO; SLABS];
    let mut h = any_heap(&mut storage);
    let before = table(&h);
    let (len, stats) = (h.len, h.stats);
    let mut source = Source::new(&h);
    let size: u64 = kani::any();
    let align: u64 = kani::any();
    // A block's frame is the one request the heap checks against its slabs.
    source.may_alias = matches!(plan(size, align), Some(Plan::Block(_)));
    let result = h.allocate(&mut source, size, align);
    let after = table(&h);
    match (plan(size, align), result) {
        (_, None) => {
            kani::cover(plan(size, align).is_some(), "an allocation that fails");
            kani::assert(
                h.len == len && h.stats == stats && same(h.slabs, &before, len),
                "a failed allocation changes nothing",
            );
            kani::assert(
                source.back == source.out,
                "a failed allocation gives back what it took",
            );
        }
        (None, Some(_)) => kani::assert(false, "no allocation for a layout plan refuses"),
        (Some(Plan::Block(block)), Some(addr)) => {
            kani::cover(h.len > len, "a block in a new slab");
            kani::cover(h.len == len && len > 0, "a block in a slab already there");
            let page = addr & !(PAGE - 1);
            let index = (addr - page) >> block.trailing_zeros();
            let size_class = block as u32;
            kani::assert(
                aligned(addr, align) && aligned(addr - page, block),
                "a block meets the alignment",
            );
            kani::assert(
                live_block(&after, h.len, page, size_class, index)
                    && !live_block(&before, len, page, size_class, index),
                "a block handed out was free and is now live",
            );
            kani::assert(
                kept_blocks(&before, len, &after, h.len)
                    && live_blocks(&after, h.len) == live_blocks(&before, len) + 1,
                "allocate adds that block and keeps every other",
            );
            kani::assert(
                h.len <= len + 1 && source.out == source.back + (h.len - len) as u64,
                "a new slab holds the one frame it took",
            );
        }
        (Some(Plan::Run { pages, .. }), Some(addr)) => {
            kani::cover(true, "a run");
            kani::assert(aligned(addr, align), "a run meets the alignment");
            kani::assert(
                h.len == len
                    && same(h.slabs, &before, len)
                    && h.stats.large_pages == stats.large_pages + pages,
                "a run is counted and the table is unchanged",
            );
        }
    }
    check_whole(&h);
}

/// `free`, from any valid heap, of any address, size and alignment: it takes back exactly a
/// live block of the layout's size, giving its slab's frame back once when the slab empties,
/// or a large run the source takes back; anything else is refused, changing nothing and
/// giving nothing back. The invariant holds after.
#[kani::proof]
#[kani::unwind(6)]
#[kani::stub(<[Slab]>::rotate_right, rotate_right_by_walk)]
#[kani::stub(<[Slab]>::rotate_left, rotate_left_by_walk)]
fn free_takes_back_only_a_live_block() {
    let mut storage = [Slab::ZERO; SLABS];
    let mut h = any_heap(&mut storage);
    let before = table(&h);
    let (len, stats) = (h.len, h.stats);
    let mut source = Source::new(&h);
    let addr: u64 = kani::any();
    let size: u64 = kani::any();
    let align: u64 = kani::any();
    let page = addr & !(PAGE - 1);
    let result = h.free(&mut source, addr, size, align);
    let after = table(&h);
    match (plan(size, align), result) {
        (_, Err(e)) => {
            kani::cover(
                matches!(e, HeapError::DoubleFree(_)),
                "a double free is refused",
            );
            kani::cover(
                matches!(e, HeapError::WrongLayout(_)),
                "a wrong layout is refused",
            );
            kani::assert(
                h.len == len && h.stats == stats && same(h.slabs, &before, len),
                "a refused free changes nothing",
            );
            kani::assert(source.back == 0, "a refused free gives nothing back");
        }
        (None, Ok(())) => kani::assert(false, "no free for a layout plan refuses"),
        (Some(Plan::Block(block)), Ok(())) => {
            let index = (addr - page) >> block.trailing_zeros();
            let size_class = block as u32;
            let at = source.slab_at(page);
            kani::cover(h.len < len, "a slab emptied and given back");
            kani::cover(h.len == len, "a block taken back from a slab that stays");
            kani::assert(
                live_block(&before, len, page, size_class, index)
                    && aligned(addr - page, block)
                    && !live_block(&after, h.len, page, size_class, index),
                "free takes back exactly a live block of its size",
            );
            kani::assert(
                kept_blocks(&after, h.len, &before, len)
                    && live_blocks(&after, h.len) + 1 == live_blocks(&before, len),
                "free takes back that block and keeps every other",
            );
            let emptied = at.is_some_and(|i| before[i].used == 1);
            kani::assert(
                (h.len + 1 == len) == emptied
                    && at.is_some_and(|i| source.slab_back[i] == u32::from(emptied))
                    && source.back == u64::from(emptied),
                "a slab's frame goes back once, exactly when it empties",
            );
        }
        (Some(Plan::Run { pages, .. }), Ok(())) => {
            kani::cover(true, "a large run taken back");
            kani::assert(
                addr == page && source.slab_at(page).is_none(),
                "a large free names a frame that is no slab",
            );
            kani::assert(
                h.len == len
                    && same(h.slabs, &before, len)
                    && h.stats.large_pages + pages == stats.large_pages
                    && source.back == pages,
                "a large free is counted and the table is unchanged",
            );
        }
    }
    check_whole(&h);
}

/// The bytes an allocation of `size` and `align` occupies at `addr`, as (start, length).
fn occupied(addr: u64, size: u64, align: u64) -> (u64, u64) {
    match plan(size, align) {
        Some(Plan::Block(block)) => (addr, block),
        Some(Plan::Run { pages, .. }) => (addr, pages * PAGE),
        None => (addr, 0),
    }
}

/// From any valid heap, two allocations with no free between them never share a byte: no
/// byte is handed out twice.
#[kani::proof]
#[kani::unwind(6)]
#[kani::stub(<[Slab]>::rotate_right, rotate_right_by_walk)]
#[kani::stub(<[Slab]>::rotate_left, rotate_left_by_walk)]
fn two_allocations_never_overlap() {
    let mut storage = [Slab::ZERO; SLABS];
    let mut h = any_heap(&mut storage);
    let mut source = Source::new(&h);
    let (s1, a1, s2, a2): (u64, u64, u64, u64) = kani::any();
    let (Some(x), Some(y)) = (
        h.allocate(&mut source, s1, a1),
        h.allocate(&mut source, s2, a2),
    ) else {
        return;
    };
    let (x, n) = occupied(x, s1, a1);
    let (y, k) = occupied(y, s2, a2);
    kani::cover(n <= MAX_BLOCK && k <= MAX_BLOCK, "two blocks");
    kani::cover(
        n > MAX_BLOCK || k > MAX_BLOCK,
        "a block and a run, or two runs",
    );
    kani::assert(
        x.checked_add(n).is_some_and(|e| e <= y) || y.checked_add(k).is_some_and(|e| e <= x),
        "no byte is handed out twice",
    );
}
