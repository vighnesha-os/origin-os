//! What the program reads only through relocated pointers, and the checks of the runtime's
//! `mem*` functions.
//!
//! Each element of [`WORDS`] and [`STEPS`] is a pointer the linker leaves as a relative
//! relocation in a RELRO page: the kernel's launch path must add the base to each before the
//! program runs, or reading one faults. `black_box` keeps the compiler from folding the tables
//! away, so the pointers are loaded from memory at run time.

use core::cmp::Ordering;
use core::hint::black_box;

/// Words read through relocated `&str`s.
static WORDS: [&str; 4] = ["ring", "three", "runs", "rust"];
/// Functions called through relocated pointers.
static STEPS: [fn(u64) -> u64; 3] = [add_one, double, square];

/// FNV-1a's offset basis and prime, for a checksum the compiler can also compute.
const FNV_OFFSET: u64 = 0xCBF2_9CE4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01B3;

/// FNV-1a over `bytes`, from `hash`.
const fn fnv(mut hash: u64, bytes: &[u8]) -> u64 {
    let mut i = 0;
    while i < bytes.len() {
        hash = (hash ^ bytes[i] as u64).wrapping_mul(FNV_PRIME);
        i += 1;
    }
    hash
}

/// The checksum of [`WORDS`], worked out by the compiler from one literal, with no pointer.
const WORDS_CHECKSUM: u64 = fnv(FNV_OFFSET, b"ringthreerunsrust");

/// Where [`WORDS`] is: relocated pointers, in a page the launch path maps read-only.
pub(crate) fn words_address() -> u64 {
    (&raw const WORDS) as usize as u64
}

/// Whether the words read through their pointers give [`WORDS_CHECKSUM`].
pub(crate) fn words_match() -> bool {
    let mut hash = FNV_OFFSET;
    for word in black_box(&WORDS) {
        hash = fnv(hash, word.as_bytes());
    }
    hash == WORDS_CHECKSUM
}

fn add_one(x: u64) -> u64 {
    x + 1
}

fn double(x: u64) -> u64 {
    x * 2
}

fn square(x: u64) -> u64 {
    x * x
}

/// 3 through [`STEPS`]: ((3 + 1) × 2)² = 64.
pub(crate) fn steps() -> u64 {
    black_box(&STEPS)
        .iter()
        .fold(black_box(3), |value, step| step(value))
}

/// Bytes in each buffer: more than the compiler inlines a copy of, at a length it cannot see.
const LEN: usize = 300;

/// A byte of the pattern at `i`.
fn pattern(i: usize) -> u8 {
    (i.wrapping_mul(7).wrapping_add(3) & 0xFF) as u8
}

/// The `mem*` checks, one bit each, all set when all pass: `memcpy` (a copy), `memmove` with the
/// destination above the source and below it (overlapping copies), `memset` (a fill) and
/// `memcmp` (an ordering and equality).
pub(crate) fn mem_checks() -> u64 {
    let n = black_box(LEN);
    let mut source = [0u8; LEN];
    for (i, byte) in source.iter_mut().enumerate() {
        *byte = pattern(i);
    }
    let mut bits = 0;

    let mut copy = [0u8; LEN];
    copy[..n].copy_from_slice(&source[..n]);
    if (0..LEN).all(|i| black_box(copy[i]) == pattern(i)) {
        bits |= 1;
    }

    // Up by 50: the copy must run from the top, or it reads bytes it has already written.
    let mut up = source;
    up.copy_within(0..black_box(200), 50);
    if (0..LEN).all(|i| {
        black_box(up[i])
            == if i < 50 {
                pattern(i)
            } else if i < 250 {
                pattern(i - 50)
            } else {
                pattern(i)
            }
    }) {
        bits |= 2;
    }

    // Down by 50: the copy must run from the bottom.
    let mut down = source;
    down.copy_within(50..black_box(250), 0);
    if (0..LEN).all(|i| black_box(down[i]) == if i < 200 { pattern(i + 50) } else { pattern(i) }) {
        bits |= 4;
    }

    let mut filled = [1u8; LEN];
    filled[..n].fill(black_box(0xA5));
    if (0..LEN).all(|i| black_box(filled[i]) == 0xA5) {
        bits |= 8;
    }

    let mut larger = source;
    larger[black_box(LEN - 1)] = pattern(LEN - 1).wrapping_add(1);
    // Each way round: `memcmp` must give a sign, not only "differ".
    let ordered = source[..n].cmp(&larger[..n]) == Ordering::Less
        && larger[..n].cmp(&source[..n]) == Ordering::Greater;
    if ordered && source[..n] == copy[..n] && source[..n] != larger[..n] {
        bits |= 16;
    }
    bits
}
