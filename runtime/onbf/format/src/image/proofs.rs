//! Kani's proof of `check_padding` (ONBF §13 item 5; CLAUDE.md §73; TASKS item 143, step 3).
//! Declared in `image.rs` so that it reaches the private function. Built only by
//! `cargo kani` (`cfg(kani)`), which no build of the loader, the kernel or any image sets.
//!
//! # What is proved, and within which bounds
//!
//! For every input of up to [`PAD`] bytes and every `start` and `end` (any `u64`):
//! `check_padding` returns without a panic, a read out of bounds or an overflow (P1); it
//! returns `Ok` exactly when `start <= end <= len` and every byte of `start..end` is zero;
//! `Truncated` exactly when `start..end` does not lie inside the input; and
//! `PaddingNotZero(n)` only for the first byte of the range that is not zero. No other error.
//!
//! **The bound falls short of what `Image::parse` needs.** A real image's padding gaps run to
//! 4,095 bytes, inside inputs of 4 KiB and more; this proof stops at inputs of 128 bytes.
//! Each read starts at a position Kani chooses, so its cost grows with the gap's length times
//! the buffer's: 64 bytes took 26 s, 128 bytes 182 s and 3.3 GB, and 256 bytes ran past
//! 240 s (TASKS item 143's entry).
//!
//! **`Image::parse` itself is not proved by Kani**, nor are its accessors or
//! `KrnManifest::check_against` (the coordinating session's ruling (A), 2026-11-27). Every
//! image it can accept is at least 4 KiB (§4.1), and every harness over such an image ran
//! past 300 s, with the byte readers stubbed or not (the figures are in TASKS item 143's
//! entry). The proof of the whole image is TASKS item 159.
#![forbid(unsafe_code)]

use super::check_padding;
use crate::error::Error;

/// The longest input this proof covers.
const PAD: usize = 128;

/// `check_padding` on every input of up to [`PAD`] bytes and every range: P1 and its exact
/// result.
#[kani::proof]
#[kani::unwind(130)]
fn check_padding_is_exact() {
    let buf: [u8; PAD] = kani::any();
    let len: usize = kani::any_where(|&l| l <= PAD);
    let bytes = &buf[..len];
    let start: u64 = kani::any();
    let end: u64 = kani::any();
    let inside = start <= end && end <= len as u64;
    let i: u64 = kani::any();
    match check_padding(bytes, start, end) {
        Ok(()) => {
            kani::assert(inside, "Ok only for a range inside the input");
            if start <= i && i < end {
                kani::assert(bytes[i as usize] == 0, "Ok only when every byte is zero");
            }
            kani::cover(end - start == PAD as u64, "the longest range is zero");
        }
        Err(Error::Truncated) => kani::assert(!inside, "Truncated only for a range outside"),
        Err(Error::PaddingNotZero(n)) => {
            kani::assert(
                inside && start <= n && n < end,
                "the byte named is in the range",
            );
            kani::assert(bytes[n as usize] != 0, "the byte named is not zero");
            if start <= i && i < n {
                kani::assert(bytes[i as usize] == 0, "it names the first such byte");
            }
            kani::cover(
                n == end - 1 && start == 0,
                "the last byte of a range is named",
            );
        }
        Err(_) => kani::assert(false, "check_padding has no other error"),
    }
}
