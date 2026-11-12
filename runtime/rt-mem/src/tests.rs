//! The `mem*` bodies against `core`'s own copy, fill and comparison (S4-1's review, finding F6).
//!
//! Each property is checked over the whole of a bounded domain rather than at random: every
//! length to 40 bytes and some longer ones; for each, every distance from a copy's source to its
//! destination from one past the length below to one past it above, so every overlap in both
//! directions and both kinds of neighbour (longer copies try the distances around zero, a word,
//! a cache line and the length); and the source at each of 8 byte offsets, so most pointers are
//! unaligned.

use std::vec::Vec;

use super::*;

/// The lengths each property is checked at.
fn lengths() -> impl Iterator<Item = usize> {
    (0..=40).chain([63, 64, 65, 255, 256, 257, 4095, 4096, 4097])
}

/// The distances from source to destination tried for a copy of `n` bytes.
fn distances(n: usize) -> Vec<isize> {
    let n = isize::try_from(n).unwrap();
    let mut d: Vec<isize> = if n <= 40 {
        (-(n + 1)..=n + 1).collect()
    } else {
        [0, 1, 7, 8, 9, 15, 16, 17, 63, 64, 65, n - 1, n, n + 1]
            .into_iter()
            .flat_map(|d| [d, -d])
            .collect()
    };
    d.sort_unstable();
    d.dedup();
    d
}

/// `len` bytes that look random, so that a byte copied from the wrong place shows.
fn pattern(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (u32::try_from(i).unwrap().wrapping_mul(0x9E37_79B9) >> 24) as u8)
        .collect()
}

/// Runs `check(n, size, src, dst)` for every length, offset and distance: a buffer of `size`
/// bytes holds both `n`-byte ranges, at `src` and at `dst`.
fn each_copy(mut check: impl FnMut(usize, usize, usize, usize)) -> usize {
    let mut cases = 0;
    for n in lengths() {
        let size = 3 * n + 32;
        for offset in 0..8 {
            // At least `n + 1 + 8`, so that a destination `n + 1` below is still in the buffer.
            let src = n + 9 + offset;
            for d in distances(n) {
                let dst = src.checked_add_signed(d).unwrap();
                assert!(src + n <= size && dst + n <= size);
                check(n, size, src, dst);
                cases += 1;
            }
        }
    }
    cases
}

#[test]
fn copy_agrees_with_core_ptr_copy_for_every_length_overlap_and_offset() {
    let cases = each_copy(|n, size, src, dst| {
        let mut ours = pattern(size);
        let mut theirs = ours.clone();
        let at = ours.as_mut_ptr();
        // SAFETY: both `n`-byte ranges are inside the `size` bytes at `at` (`each_copy`).
        unsafe { copy(at.wrapping_add(dst), at.wrapping_add(src), n) };
        let at = theirs.as_mut_ptr();
        // SAFETY: as above; `core::ptr::copy` allows the ranges to overlap.
        unsafe { core::ptr::copy(at.wrapping_add(src), at.wrapping_add(dst), n) };
        assert_eq!(ours, theirs, "{n} bytes from {src} to {dst}");
    });
    assert!(cases > 10_000, "{cases} cases");
}

#[test]
fn a_copy_up_agrees_wherever_the_destination_does_not_start_inside_the_source() {
    let cases = each_copy(|n, size, src, dst| {
        if dst > src && dst < src + n {
            return;
        }
        let mut ours = pattern(size);
        let mut theirs = ours.clone();
        let at = ours.as_mut_ptr();
        // SAFETY: both ranges are inside the buffer, and `dst` does not start inside `src`'s
        // bytes after the first of them.
        unsafe { copy_up(at.wrapping_add(dst), at.wrapping_add(src), n) };
        let at = theirs.as_mut_ptr();
        // SAFETY: as above.
        unsafe { core::ptr::copy(at.wrapping_add(src), at.wrapping_add(dst), n) };
        assert_eq!(ours, theirs, "{n} bytes from {src} up to {dst}");
    });
    assert!(cases > 5_000, "{cases} cases");
}

#[test]
fn a_fill_writes_its_bytes_and_nothing_else() {
    for n in lengths() {
        for offset in 0..8 {
            for byte in [0x00, 0x5A, 0x80, 0xFF] {
                let at = 8 + offset;
                let mut ours = pattern(n + 32);
                let mut wanted = ours.clone();
                wanted[at..at + n].fill(byte);
                let base = ours.as_mut_ptr();
                // SAFETY: `n` bytes from `at` are inside the `n + 32`.
                unsafe { fill(base.wrapping_add(at), byte, n) };
                assert_eq!(ours, wanted, "{n} bytes of {byte:#x} at {at}");
            }
        }
    }
}

#[test]
fn a_comparison_is_the_first_difference_as_unsigned_bytes() {
    let pairs: [(u8, u8); 6] = [
        (0x00, 0x01),
        (0x01, 0x00),
        (0x7F, 0x80),
        (0x80, 0x7F),
        (0x00, 0xFF),
        (0xFF, 0x00),
    ];
    for n in lengths().filter(|&n| n <= 257) {
        for offset in 0..8 {
            let a = pattern(n + 16);
            let same = a.clone();
            // SAFETY: `n` bytes from `offset` are inside each `n + 16`.
            let equal = unsafe { compare(a.as_ptr().wrapping_add(offset), same.as_ptr(), 0) };
            assert_eq!(equal, 0, "no bytes");
            // SAFETY: as above.
            let equal = unsafe {
                compare(
                    a.as_ptr().wrapping_add(offset),
                    same.as_ptr().wrapping_add(offset),
                    n,
                )
            };
            assert_eq!(equal, 0, "{n} equal bytes");
            for i in 0..n {
                for (x, y) in pairs {
                    let (mut a, mut b) = (a.clone(), a.clone());
                    a[offset + i] = x;
                    b[offset + i] = y;
                    // A later difference the other way must not count.
                    if i + 1 < n {
                        b[offset + i + 1] = !a[offset + i + 1];
                    }
                    let (a, b) = (&a[offset..offset + n], &b[offset..offset + n]);
                    // SAFETY: `a` and `b` are `n` bytes each.
                    let ours = unsafe { compare(a.as_ptr(), b.as_ptr(), n) };
                    assert_eq!(ours, i32::from(x) - i32::from(y), "{n} bytes, at {i}");
                    assert_eq!(ours.signum(), a.cmp(b) as i32, "{n} bytes, at {i}");
                }
            }
        }
    }
}

/// The direction flag is clear after a copy down, on the processor itself.
#[cfg(target_arch = "x86_64")]
#[test]
fn the_direction_flag_is_clear_after_a_copy_down() {
    let mut bytes = pattern(64);
    let at = bytes.as_mut_ptr();
    // SAFETY: 32 bytes from 0 and from 8 are inside the 64; the destination starts inside the
    // source, so this copies down.
    unsafe { copy(at.wrapping_add(8), at, 32) };
    let flags: u64;
    // SAFETY: reads RFLAGS through the stack, which `pushfq` and `pop` leave as they found it.
    unsafe { core::arch::asm!("pushfq", "pop {}", out(reg) flags) };
    assert_eq!(flags & (1 << 10), 0, "RFLAGS.DF");
}
