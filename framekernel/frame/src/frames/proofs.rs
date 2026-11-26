//! Kani's proofs of the frame allocator (CLAUDE.md §73; TASKS item 143). Built only by
//! `cargo kani` (`cfg(kani)`), which no build of the kernel, the loader or any image sets.
//!
//! # What is proved, and within which bounds
//!
//! **The invariant** ([`check_whole`]), over the allocator's own fields:
//! - *accounting:* `free` is the number of clear bits below `frames`;
//! - *ownership:* every clear bit below `frames` is an [`owned`] frame. [`owned`] is written
//!   here, from the allocator's documentation, not from its code: a frame lies wholly in a
//!   region the allocator owns (`Usable`, or a kind `reclaim` took over), is not frame 0, and
//!   holds no byte of a reserved range;
//! - *cover:* `frames` is the end of the memory map's highest region of a kind the allocator
//!   may ever own, in frames ([`map_end`], also written here from the documentation of
//!   `covered_end` and not from its code), and the bitmap holds a bit for each.
//!
//! Accounting is checked over the whole bitmap. Ownership is a statement about every frame,
//! so it is checked at one frame that Kani chooses freely (`frame`, `kani::any()`): since the
//! proof holds for whichever frame Kani picks, it holds for each. Where a harness assumes
//! the invariant beforehand, it assumes ownership at that same frame, which assumes less than
//! the invariant does, so the proof covers more states, not fewer.
//!
//! **The method.** [`new_frees_exactly_the_owned_frames`] and
//! [`new_refuses_a_bitmap_too_small`] prove that `new` establishes the invariant from any
//! memory map `boot::summarize` accepts. Every other harness starts from **any** state that
//! satisfies it ([`any_state`]: the bitmap's words, `next`, the reclaimed kinds and the first
//! 2 of the 8 reserved slots are all `kani::any()`, and the other 6 are empty, as `new` leaves
//! them), runs one method on arbitrary arguments, and proves the invariant holds after it,
//! with the method's own postcondition. Every `&mut self` method of `FrameAllocator` is
//! covered but `reclaim`:
//!
//! | Method | Harness | Memory |
//! |---|---|---|
//! | `new` (establishes it) | [`new_frees_exactly_the_owned_frames`] (a bitmap of one word), [`new_refuses_a_bitmap_too_small`] (of no word), [`new_refuses_more_than_eight_ranges`] | 5 frames; 2 for the nine ranges |
//! | `allocate` | [`allocate_hands_out_a_free_frame`] (two words), [`allocate_hands_out_a_free_frame_one_word`] | 128 frames; 64 |
//! | `free` | [`free_takes_back_only_what_was_handed_out`] (two words), [`free_takes_back_only_what_was_handed_out_one_word`] | 128 frames; 64 |
//! | `claim` | [`claim_takes_only_a_free_frame`] (two words), [`claim_takes_only_a_free_frame_one_word`] | 128 frames; 64 |
//! | `allocate_run` | [`allocate_run_hands_out_a_free_aligned_run`] (one word) | 8 frames |
//! | `free_run` | [`free_run_is_all_or_nothing`] (one word) | 16 frames |
//! | `reclaim` | **not covered** (below) | |
//! | `set_free` (private; called only by the above) | through each of them | |
//!
//! [`two_hand_outs_never_overlap`] (8 frames) states "no frame is handed out twice" directly.
//!
//! **What the induction covers.** Each row's memory is the bound of that one step, from any
//! valid state of that size. A sequence of calls starts where `new` leaves the allocator, and
//! `new` is proved over at most 5 frames with a bitmap of one word. Every step has a harness
//! over one word whose bound is at least that, so, by induction on the number of calls, the
//! invariant holds after any sequence of `allocate`, `free`, `claim`, `allocate_run` (runs of
//! at most 4 frames) and `free_run` on an allocator `new` built over a map of at most 5
//! frames, 3 regions and 2 reserved ranges. No sequence past 5 frames is proved. The
//! two-word harnesses show their step across a word's boundary.
//!
//! **`reclaim` is not proved.** It walks every frame of every region of the kind it takes
//! over, and checks each with `allocatable`, which walks all 8 reserved slots: walks inside
//! walks, each of which CBMC unrolls to the bound, and which need a bound of at least 9. At 9
//! its harness had not finished after 600 seconds with a map of 3 regions or of 2; at 8 (267
//! seconds, 7.5 GB) it fails the unwinding check (TASKS item 143, ruling FA-7). So a sequence
//! that includes `reclaim` is covered only as far as `reclaim` keeps the invariant, which Kani
//! does not show; its host tests check it on examples (F-1's among them). The states the
//! other harnesses start from do include every set of kinds already reclaimed. TASKS item 154
//! is a Kani proof of `reclaim`.
//!
//! **Kani's own checks** hold in each harness too: no arithmetic overflow, no index out of
//! bounds, no division by zero, no panic, and every loop unwound within its bound (the
//! unwinding assertions are on, so a bound too small fails the proof).
//!
//! **The bounds:** memory of the size each harness names above, which a bitmap of one or two
//! words covers; a memory map of 3 regions of any kind values (a region of no pages stands for
//! one that is absent), which `boot::summarize` accepts, and which lies within that memory; 2
//! reserved ranges of any base and size, not necessarily frame aligned (a range of no pages
//! stands for one that is absent, and the rest of the 8 slots are empty, as `new` leaves
//! them); runs of at most 4 frames where a harness says so. The lengths are fixed, so that
//! CBMC unrolls each walk no further than it runs.
//!
//! **Not proved:** sizes past the bounds; the kernel's use of the allocator (`memory.rs`'s
//! lock, `GlobalAlloc`, the pointer conversion and `OwnedFrame`; `early.rs`' placement of the
//! bitmap); concurrency; maps that `boot::summarize` refuses (the kernel never passes one);
//! the machine code rustc 1.100.0 makes: Kani compiles with its own nightly
//! (`nightly-2026-08-21`) for the host it runs on.
#![forbid(unsafe_code)]

use super::{BITS, FRAME, FrameAllocator, FrameError, MAX_RESERVED};
use crate::boot::summarize;
use origin_boot_info::{MemoryRegion, RegionKind};

/// Bitmap words at most: 128 frames.
const WORDS: usize = 2;
/// Regions in the memory map.
const REGIONS: usize = 3;
/// Reserved ranges (of `MAX_RESERVED` slots).
const RESERVED: usize = 2;
/// The memory of the harnesses whose method walks no frame by frame, in frames.
const LARGE: u64 = WORDS as u64 * BITS;
/// The memory of the harnesses whose method walks a range of frames (`free_run`), in frames:
/// CBMC unrolls each walk to the bound, whatever the map.
const MEDIUM: u64 = 16;
/// The memory of the run harnesses, in frames: their search walks a run inside a walk over
/// the frames.
const SMALL: u64 = 8;
/// The memory of the harnesses whose method walks the frames of each region it picks out
/// (`new`), in frames. CBMC cannot tell where a filtered walk over the regions
/// stands, so it unrolls the walk, its search for the next region and the walk over that
/// region's frames each to the bound. At 16 frames `new` ran past 5 minutes still unrolling;
/// at 8 it took 9.8 GB; at 6 one run of it or of its controls reached 8.8 GB, past the 8 GB a
/// run may take (TASKS item 143).
const MAP: u64 = 5;
/// The memory of the harness whose method refuses before it reads the map (`new` given nine
/// ranges), in frames. Its control lets `new` go on to walk the map and all nine ranges: at 16
/// frames that run had not finished after 30 minutes, at 2 it takes under 3 (TASKS item 143).
const TINY: u64 = 2;
/// The longest run the run harnesses ask for.
const RUN: u64 = 4;

/// A memory map, a bitmap of `N` words and reserved ranges, all drawn by Kani. The lengths are
/// fixed: a map of fewer regions is one whose other slots are empty (no pages, a kind the
/// allocator does not own), and a slot of no pages reserves nothing.
struct Model<const N: usize> {
    bitmap: [u64; N],
    regions: [MemoryRegion; REGIONS],
    reserved: [(u64, u64); MAX_RESERVED],
}

fn any_region() -> MemoryRegion {
    MemoryRegion {
        base: kani::any(),
        pages: kani::any(),
        kind: kani::any(),
        reserved: 0,
    }
}

impl<const N: usize> Model<N> {
    /// Any bitmap contents, any map `boot::summarize` accepts within `limit` frames, and any
    /// reserved ranges in the first slots.
    fn any(limit: u64) -> Self {
        let regions: [MemoryRegion; REGIONS] = core::array::from_fn(|_| any_region());
        kani::assume(summarize(&regions).is_ok());
        // Bounded so that every walk over a region's frames is bounded too.
        for r in &regions {
            kani::assume(
                r.base
                    .checked_add(r.pages.saturating_mul(FRAME))
                    .is_some_and(|end| end <= limit * FRAME),
            );
        }
        let mut reserved = [(0, 0); MAX_RESERVED];
        for slot in reserved.iter_mut().take(RESERVED) {
            *slot = (kani::any(), kani::any());
        }
        Self {
            bitmap: kani::any(),
            regions,
            reserved,
        }
    }
}

/// The bits of bitmap word `word` that stand for frames below `n`.
fn below(word: usize, n: u64) -> u64 {
    let base = word as u64 * BITS;
    if n <= base {
        0
    } else if n - base >= BITS {
        u64::MAX
    } else {
        (1 << (n - base)) - 1
    }
}

/// The bits of bitmap word `word` that stand for frames `first..first + count`.
fn span(word: usize, first: u64, count: u64) -> u64 {
    below(word, first.saturating_add(count)) & !below(word, first)
}

/// Whether bit `frame` is clear in `words`. Frames past the words read as used.
fn clear(words: &[u64], frame: u64) -> bool {
    words
        .get((frame / BITS) as usize)
        .is_some_and(|w| w & (1 << (frame % BITS)) == 0)
}

/// Whether the allocator owns `frame`, from its documentation: the frame lies wholly in a
/// region of a kind it owns (`Usable`, or a kind whose bit is in `reclaimed`), it is not
/// frame 0, and no byte of it is in a reserved range.
fn owned(regions: &[MemoryRegion], reclaimed: u32, reserved: &[(u64, u64)], frame: u64) -> bool {
    let start = frame.saturating_mul(FRAME);
    let end = start.saturating_add(FRAME);
    let in_region = regions.iter().any(|r| {
        let kind =
            r.kind == RegionKind::Usable as u32 || (r.kind < 32 && reclaimed & (1 << r.kind) != 0);
        kind && start >= r.base && end <= r.base.saturating_add(r.pages.saturating_mul(FRAME))
    });
    let reserved_byte = reserved.iter().any(|&(base, pages)| {
        pages != 0 && start < base.saturating_add(pages.saturating_mul(FRAME)) && base < end
    });
    frame != 0 && in_region && !reserved_byte
}

/// [`owned`] for `a`'s own map, reclaimed kinds and reserved ranges. Only the first
/// `RESERVED` slots are read: in every state here the others are empty, as [`new_case`]
/// proves `new` leaves them and [`Model::any`] draws them. Reading all 8 would need an unwind
/// bound of 9 in the harnesses of `new`, whose cost grows with it.
fn owns(a: &FrameAllocator<'_>, frame: u64) -> bool {
    owned(a.regions, a.reclaimed, &a.reserved[..RESERVED], frame)
}

/// The clear bits below `a.frames`, word by word.
fn clear_bits(a: &FrameAllocator<'_>) -> u64 {
    a.bitmap
        .iter()
        .enumerate()
        .map(|(i, w)| u64::from((!w & below(i, a.frames)).count_ones()))
        .sum()
}

/// Whether ownership holds at `frame` for the bits `words`, with `a`'s map, reserved ranges
/// and reclaimed kinds.
fn owned_if_free(a: &FrameAllocator<'_>, words: &[u64], frame: u64) -> bool {
    frame >= a.frames
        || !clear(words, frame)
        || owned(a.regions, a.reclaimed, &a.reserved[..RESERVED], frame)
}

/// FI-3's end, written from `covered_end`'s documentation and not its code: the end of the
/// highest region of a kind the allocator may ever own, `Usable` or `LoaderReclaimable` (which
/// `reclaim` may return), or 0 when there is none. Every map here is one `boot::summarize`
/// accepts within the harness's memory, so no end overflows.
fn map_end(regions: &[MemoryRegion]) -> u64 {
    let mut end = 0;
    for r in regions {
        if r.kind == RegionKind::Usable as u32 || r.kind == RegionKind::LoaderReclaimable as u32 {
            end = end.max(r.base + r.pages * FRAME);
        }
    }
    end
}

/// The bitmap words that hold a bit for each frame below `end`: a frame for each 4 KiB begun,
/// a word for each 64 frames begun.
fn words_needed(end: u64) -> u64 {
    let frames = end / FRAME + u64::from(end % FRAME != 0);
    frames / BITS + u64::from(frames % BITS != 0)
}

/// Asserts the invariant: cover and accounting over the whole bitmap, and ownership at
/// `frame`, which the harness lets Kani choose freely.
fn check_whole(a: &FrameAllocator<'_>, frame: u64) {
    kani::assert(
        a.frames == map_end(a.regions) / FRAME && a.frames <= a.bitmap.len() as u64 * BITS,
        "cover: the bitmap holds a bit for every frame of the map",
    );
    kani::assert(
        clear_bits(a) == a.free,
        "accounting: free counts the free frames",
    );
    kani::assert(
        owned_if_free(a, a.bitmap, frame),
        "ownership: every free frame is owned",
    );
}

/// Any state that satisfies the invariant's cover and accounting, over `m`'s map and bitmap,
/// with ownership assumed at `frame`. The bits at and past `frames` are left as drawn: nothing
/// may depend on them.
fn any_state<const N: usize>(m: &mut Model<N>, frame: u64) -> FrameAllocator<'_> {
    let frames = map_end(&m.regions) / FRAME;
    kani::assume(frames <= N as u64 * BITS);
    let mut a = FrameAllocator {
        bitmap: &mut m.bitmap,
        regions: &m.regions,
        reclaimed: kani::any(),
        reserved: m.reserved,
        frames,
        next: kani::any(),
        free: 0,
    };
    a.free = clear_bits(&a);
    kani::assume(owned_if_free(&a, a.bitmap, frame));
    a
}

/// A copy of the bitmap's words, for comparing before and after.
fn snapshot<const N: usize>(a: &FrameAllocator<'_>) -> [u64; N] {
    let mut words = [0; N];
    for (copy, word) in words.iter_mut().zip(a.bitmap.iter()) {
        *copy = *word;
    }
    words
}

/// Whether `a`'s bitmap equals `before`, word by word.
fn unchanged(a: &FrameAllocator<'_>, before: &[u64]) -> bool {
    a.bitmap
        .iter()
        .zip(before.iter())
        .all(|(after, was)| after == was)
}

/// Whether `a`'s bitmap equals `before` with the bits of frames `first..first + count` set
/// (`set`, each clear before) or cleared (each set before), and no other bit changed.
fn changed_only(a: &FrameAllocator<'_>, before: &[u64], first: u64, count: u64, set: bool) -> bool {
    a.bitmap
        .iter()
        .zip(before.iter())
        .enumerate()
        .all(|(i, (&after, &was))| {
            let mask = span(i, first, count);
            if set {
                after == was | mask && was & mask == 0
            } else {
                after == was & !mask && was & mask == mask
            }
        })
}

/// Whether frames `first..first + count` are all clear in `words`, inside `frames`.
fn free_run(words: &[u64], frames: u64, first: u64, count: u64) -> bool {
    first.checked_add(count).is_some_and(|end| end <= frames)
        && words
            .iter()
            .enumerate()
            .all(|(i, w)| w & span(i, first, count) == 0)
}

/// `new` with a bitmap of `N` words, from any map `boot::summarize` accepts within `limit`
/// frames, any bitmap contents and 2 reserved ranges of any base and size: it refuses exactly
/// a bitmap too short for the map, and otherwise establishes the invariant with every owned
/// frame free, so nothing it owns is lost and no byte of a reserved range is ever free.
fn new_case<const N: usize>(limit: u64) -> NewOutcome {
    let mut m = Model::<N>::any(limit);
    let list = [m.reserved[0], m.reserved[1]];
    let needed = words_needed(map_end(&m.regions));
    let frame: u64 = kani::any();
    match FrameAllocator::new(&mut m.bitmap, &m.regions, &list) {
        Err(FrameError::BitmapTooSmall {
            needed: n,
            provided,
        }) => {
            kani::assert(
                n == needed && provided == N as u64 && provided < needed,
                "refuses only a bitmap too small, and says so exactly",
            );
            NewOutcome::TooSmall
        }
        Err(_) => {
            kani::assert(false, "new has no other refusal for 2 ranges");
            NewOutcome::TooSmall
        }
        Ok(a) => {
            kani::assert(N as u64 >= needed, "accepts only a bitmap that fits");
            kani::assert(
                a.reserved[0] == list[0]
                    && a.reserved[1] == list[1]
                    && a.reserved[RESERVED..].iter().all(|&slot| slot == (0, 0)),
                "new keeps the ranges it is given, and no other",
            );
            check_whole(&a, frame);
            kani::assert(a.reclaimed == 0, "nothing is reclaimed at first");
            kani::assert(
                frame >= a.frames || clear(a.bitmap, frame) == owned(&m.regions, 0, &list, frame),
                "new frees exactly the owned frames",
            );
            NewOutcome::Built {
                free: a.free,
                unaligned: list
                    .iter()
                    .any(|&(base, pages)| pages != 0 && base % FRAME != 0),
            }
        }
    }
}

/// What [`new_case`] saw `new` do, for its harnesses' cover checks.
enum NewOutcome {
    /// Refused as too large for the bitmap.
    TooSmall,
    /// Built, with this many frames free, and whether a reserved range was unaligned.
    Built { free: u64, unaligned: bool },
}

/// [`new_case`] with a bitmap of one word, over 5 frames: every map is accepted.
#[kani::proof]
#[kani::unwind(7)]
fn new_frees_exactly_the_owned_frames() {
    let outcome = new_case::<1>(MAP);
    let (free, unaligned) = match outcome {
        NewOutcome::Built { free, unaligned } => (free, unaligned),
        NewOutcome::TooSmall => (0, false),
    };
    kani::cover(free > 0, "some frame is free");
    kani::cover(
        unaligned && free > 0,
        "an unaligned reserved range beside free frames",
    );
}

/// [`new_case`] with a bitmap of no word, over 5 frames: every map that holds a frame of its
/// own is refused as too large for it.
#[kani::proof]
#[kani::unwind(7)]
fn new_refuses_a_bitmap_too_small() {
    let outcome = new_case::<0>(MAP);
    kani::cover(
        matches!(outcome, NewOutcome::TooSmall),
        "a bitmap too small",
    );
}

/// `new` refuses more than 8 reserved ranges, whatever else it is given, and changes nothing.
#[kani::proof]
#[kani::unwind(10)]
fn new_refuses_more_than_eight_ranges() {
    let mut m = Model::<1>::any(TINY);
    let list: [(u64, u64); MAX_RESERVED + 1] = kani::any();
    let before = m.bitmap;
    let refused = matches!(
        FrameAllocator::new(&mut m.bitmap, &m.regions, &list),
        Err(FrameError::TooManyReserved)
    );
    kani::cover(refused, "nine ranges refused");
    kani::assert(
        refused && m.bitmap[0] == before[0],
        "refuses more than 8 ranges, changing nothing",
    );
}

/// [`allocate_case`] over 128 frames, a bitmap of two words.
#[kani::proof]
#[kani::unwind(10)]
fn allocate_hands_out_a_free_frame() {
    let handed = allocate_case::<WORDS>(LARGE);
    kani::cover(
        handed.is_some_and(|addr| addr >= BITS * FRAME),
        "a frame of the second word",
    );
}

/// [`allocate_case`] over 64 frames, a bitmap of one word, as `new` makes.
#[kani::proof]
#[kani::unwind(10)]
fn allocate_hands_out_a_free_frame_one_word() {
    allocate_case::<1>(BITS);
}

/// `allocate`, from any valid state with a bitmap of `N` words over `limit` frames: it hands
/// out a frame that was free, frame aligned and non-zero, marks it used and nothing else, and
/// counts it; it returns `None` only when no frame is free. (The frame is owned because it was
/// free: the invariant.) Returns what `allocate` returned.
fn allocate_case<const N: usize>(limit: u64) -> Option<u64> {
    let mut m = Model::<N>::any(limit);
    let frame: u64 = kani::any();
    let mut a = any_state(&mut m, frame);
    let before: [u64; N] = snapshot(&a);
    let free = a.free;
    let handed = a.allocate();
    match handed {
        Some(addr) => {
            let x = addr / FRAME;
            // The invariant held at the frame handed out too: assumed there as well.
            kani::assume(owned_if_free(&a, &before, x));
            kani::cover(true, "a frame is handed out");
            kani::assert(
                addr % FRAME == 0 && x != 0 && x < a.frames && clear(&before, x),
                "a handed-out frame was free, aligned, non-zero and in the map",
            );
            kani::assert(
                changed_only(&a, &before, x, 1, true) && a.free == free - 1,
                "allocate marks that frame used and nothing else",
            );
        }
        None => {
            kani::cover(true, "nothing to hand out");
            kani::assert(
                before
                    .iter()
                    .enumerate()
                    .all(|(i, w)| !w & below(i, a.frames) == 0),
                "allocate fails only when no frame is free",
            );
            kani::assert(
                unchanged(&a, &before) && a.free == free,
                "a failed allocate changes nothing",
            );
        }
    }
    check_whole(&a, frame);
    handed
}

/// [`free_case`] over 128 frames, a bitmap of two words.
#[kani::proof]
#[kani::unwind(10)]
fn free_takes_back_only_what_was_handed_out() {
    free_case::<WORDS>(LARGE);
}

/// [`free_case`] over 64 frames, a bitmap of one word, as `new` makes.
#[kani::proof]
#[kani::unwind(10)]
fn free_takes_back_only_what_was_handed_out_one_word() {
    free_case::<1>(BITS);
}

/// `free`, from any valid state with a bitmap of `N` words over `limit` frames, of any
/// address: it takes back exactly an owned frame that is in use, and refuses everything else
/// with the right error, changing nothing.
fn free_case<const N: usize>(limit: u64) {
    let mut m = Model::<N>::any(limit);
    let frame: u64 = kani::any();
    let mut a = any_state(&mut m, frame);
    let before: [u64; N] = snapshot(&a);
    let free = a.free;
    let addr: u64 = kani::any();
    let x = addr / FRAME;
    let in_map = x != 0 && x < a.frames;
    let expected = if addr % FRAME != 0 {
        Err(FrameError::Misaligned(addr))
    } else if !in_map {
        Err(FrameError::OutOfRange(addr))
    } else if !owns(&a, x) {
        Err(FrameError::NotAllocatable(addr))
    } else if clear(&before, x) {
        Err(FrameError::DoubleFree(addr))
    } else {
        Ok(())
    };
    let result = a.free(addr);
    kani::assert(
        result == expected,
        "free takes back exactly an owned frame in use",
    );
    match result {
        Ok(()) => {
            kani::cover(true, "a frame is taken back");
            kani::assert(
                changed_only(&a, &before, x, 1, false) && a.free == free + 1,
                "free marks that frame free and nothing else",
            );
        }
        Err(e) => {
            kani::cover(
                e == FrameError::DoubleFree(addr),
                "a double free is refused",
            );
            kani::cover(
                e == FrameError::NotAllocatable(addr),
                "an unowned frame is refused",
            );
            kani::assert(
                unchanged(&a, &before) && a.free == free,
                "a refused free changes nothing",
            );
        }
    }
    check_whole(&a, frame);
}

/// [`claim_case`] over 128 frames, a bitmap of two words.
#[kani::proof]
#[kani::unwind(10)]
fn claim_takes_only_a_free_frame() {
    claim_case::<WORDS>(LARGE);
}

/// [`claim_case`] over 64 frames, a bitmap of one word, as `new` makes.
#[kani::proof]
#[kani::unwind(10)]
fn claim_takes_only_a_free_frame_one_word() {
    claim_case::<1>(BITS);
}

/// `claim`, from any valid state with a bitmap of `N` words over `limit` frames, of any
/// address: it takes exactly the frame `frame_is_free` names, owned and free, and otherwise
/// changes nothing.
fn claim_case<const N: usize>(limit: u64) {
    let mut m = Model::<N>::any(limit);
    let frame: u64 = kani::any();
    let mut a = any_state(&mut m, frame);
    let before: [u64; N] = snapshot(&a);
    let free = a.free;
    let addr: u64 = kani::any();
    let x = addr / FRAME;
    let takeable = addr % FRAME == 0 && x < a.frames && owns(&a, x) && clear(&before, x);
    kani::assert(
        a.frame_is_free(addr) == takeable,
        "frame_is_free names exactly an owned free frame",
    );
    let claimed = a.claim(addr);
    kani::assert(
        claimed == takeable,
        "claim takes exactly an owned free frame",
    );
    if claimed {
        kani::cover(true, "a frame is claimed");
        kani::assert(
            changed_only(&a, &before, x, 1, true) && a.free == free - 1,
            "claim marks that frame used and nothing else",
        );
    } else {
        kani::cover(true, "a claim is refused");
        kani::assert(
            unchanged(&a, &before) && a.free == free,
            "a refused claim changes nothing",
        );
    }
    check_whole(&a, frame);
}

/// `allocate_run`, from any valid state over 8 frames, with any alignment and a count of up
/// to 4: a run it hands out is `count` frames, all free before and used after, its first
/// frame a non-zero multiple of the alignment, with nothing else changed; it fails, changing
/// nothing, only when no such run exists.
#[kani::proof]
#[kani::unwind(10)]
fn allocate_run_hands_out_a_free_aligned_run() {
    let mut m = Model::<1>::any(SMALL);
    let frame: u64 = kani::any();
    let mut a = any_state(&mut m, frame);
    let before: [u64; 1] = snapshot(&a);
    let free = a.free;
    let count = kani::any_where(|&c: &u64| c <= RUN);
    let align: u64 = kani::any();
    let step = align.max(1);
    // A candidate run Kani chooses freely: if it is free and aligned, a run must be found.
    let candidate: u64 = kani::any();
    let fits = count != 0
        && candidate != 0
        && candidate % step == 0
        && free_run(&before, a.frames, candidate, count);
    match a.allocate_run(count, align) {
        Some(addr) => {
            kani::cover(count > 1 && align > 1, "an aligned run of several frames");
            let first = addr / FRAME;
            kani::assert(
                addr % FRAME == 0 && first != 0 && first % step == 0 && count != 0,
                "a run starts on a non-zero frame that is a multiple of its alignment",
            );
            kani::assert(
                free_run(&before, a.frames, first, count),
                "a run's frames were all free",
            );
            kani::assert(
                changed_only(&a, &before, first, count, true) && a.free == free - count,
                "allocate_run marks the run used and nothing else",
            );
        }
        None => {
            kani::cover(count != 0, "no run fits");
            kani::assert(
                unchanged(&a, &before) && a.free == free,
                "a failed allocate_run changes nothing",
            );
            kani::assert(!fits, "allocate_run fails only when no aligned run is free");
        }
    }
    check_whole(&a, frame);
}

/// `free_run`, from any valid state, of any address and count: it takes back a run, all at
/// once, exactly when the run is aligned, non-empty, in the map and every frame of it owned
/// and in use; otherwise it refuses, naming a real fault of the run, and changes nothing.
/// (Each refusal is checked against its own fault, so a run with none must be taken back.)
#[kani::proof]
#[kani::unwind(18)]
fn free_run_is_all_or_nothing() {
    let mut m = Model::<1>::any(MEDIUM);
    let frame: u64 = kani::any();
    let mut a = any_state(&mut m, frame);
    let before: [u64; 1] = snapshot(&a);
    let free = a.free;
    let addr: u64 = kani::any();
    let count: u64 = kani::any();
    let first = addr / FRAME;
    let in_map = count != 0
        && addr % FRAME == 0
        && first != 0
        && first.checked_add(count).is_some_and(|end| end <= a.frames);
    // A frame of the run Kani chooses freely: each must have been owned and in use.
    let member: u64 = kani::any();
    match a.free_run(addr, count) {
        Ok(()) => {
            kani::cover(count > 1, "a run of several frames is taken back");
            kani::assert(
                addr % FRAME == 0 && count != 0 && first != 0 && first + count <= a.frames,
                "a run taken back is aligned, non-empty and in the map",
            );
            kani::assert(
                member < first
                    || member >= first + count
                    || (owns(&a, member) && !clear(&before, member)),
                "every frame of a run taken back was owned and in use",
            );
            kani::assert(
                changed_only(&a, &before, first, count, false) && a.free == free + count,
                "free_run marks the run free and nothing else",
            );
        }
        Err(e) => {
            kani::cover(
                matches!(e, FrameError::DoubleFree(_)),
                "a run with a free frame is refused",
            );
            // The frame a refusal names lies in the run.
            let names = |x: u64| x % FRAME == 0 && x / FRAME >= first && x / FRAME - first < count;
            let real = match e {
                FrameError::EmptyRun => count == 0,
                FrameError::Misaligned(x) => x == addr && count != 0 && addr % FRAME != 0,
                FrameError::OutOfRange(x) => {
                    x == addr && count != 0 && addr % FRAME == 0 && !in_map
                }
                FrameError::NotAllocatable(x) => in_map && names(x) && !owns(&a, x / FRAME),
                FrameError::DoubleFree(x) => in_map && names(x) && clear(&before, x / FRAME),
                _ => false,
            };
            kani::assert(real, "a refusal names a real fault of the run");
            kani::assert(
                unchanged(&a, &before) && a.free == free,
                "a refused free_run changes nothing",
            );
        }
    }
    check_whole(&a, frame);
}

/// One hand-out drawn by Kani: `allocate`, `allocate_run` of up to 4 frames, or `claim`.
/// Returns the run handed out, as (first frame, count), or `None`.
fn hand_out(a: &mut FrameAllocator<'_>) -> Option<(u64, u64)> {
    match kani::any_where(|&op: &u8| op < 3) {
        0 => a.allocate().map(|addr| (addr / FRAME, 1)),
        1 => {
            let count = kani::any_where(|&c: &u64| c <= RUN);
            a.allocate_run(count, kani::any())
                .map(|addr| (addr / FRAME, count))
        }
        _ => {
            let addr: u64 = kani::any();
            a.claim(addr).then_some((addr / FRAME, 1))
        }
    }
}

/// From any valid state over 8 frames, two hand-outs with no free between them never share
/// a frame: no frame is handed out twice.
#[kani::proof]
#[kani::unwind(10)]
fn two_hand_outs_never_overlap() {
    let mut m = Model::<1>::any(SMALL);
    let frame: u64 = kani::any();
    let mut a = any_state(&mut m, frame);
    let (Some((x, n)), Some((y, k))) = (hand_out(&mut a), hand_out(&mut a)) else {
        return;
    };
    kani::cover(n > 1 || k > 1, "two hand-outs, one a run");
    kani::assert(x + n <= y || y + k <= x, "no frame is handed out twice");
}
