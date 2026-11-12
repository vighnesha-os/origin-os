//! Indices and addresses that come from ring 3, kept in range even when the processor has
//! guessed the check wrong (ADR-0017 Z9).
//!
//! A processor runs ahead of a bounds check on a guess. If the guess is wrong, the work is
//! thrown away, but a load that ran with an out-of-range index has already touched the cache,
//! and a program can measure that. So a lookup whose index a program chose does two things:
//! the check, and something that holds on the wrongly guessed path too.
//!
//! There are two forms of that something, and the kernel picks one at boot from the CPU
//! identification table's column for it ([`crate::cpuident`], [`set_form`]):
//!
//! - **[`Form::Mask`].** A conditional move that depends on the comparison itself puts 0 in
//!   place of an out-of-range value. AMD says this holds on all its processors. Intel said so
//!   in 2018 and 2021 for the family 6 processors current then, and has not said it since for
//!   a newer family.
//! - **[`Form::Fence`].** An LFENCE after the check: nothing later runs, even on a guess,
//!   until the check has completed. Both vendors guarantee it. It costs more.
//!
//! Until the kernel has chosen, the form is Fence.
//!
//! The sequences are assembly in `cpu.rs`. This file is safe code, and on a host, where the
//! kernel's unit tests run, it uses plain Rust with the same results: a host test checks what
//! the functions return, and the boot proof runs the instructions (`kernel.rs`).
//! Neither can show what a processor does on a wrong guess. That rests on the vendors'
//! documents (`docs/research/2026-11-10-speculation-processor-hardening.md` §3.11).
//!
//! **The rule.** Any lookup whose index or address comes from ring 3 goes through here. No
//! lint can tell where a value came from, so `THREAT_MODEL_SYSCALLS.md` keeps the list.

#![forbid(unsafe_code)]

use core::sync::atomic::{AtomicU8, Ordering};

use crate::usercopy::USER_END;

/// How a value is kept in range on a wrongly guessed path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// A conditional move that depends on the comparison.
    Mask,
    /// The check, then LFENCE.
    Fence,
}

const MASK: u8 = 0;
const FENCE: u8 = 1;

/// The form in use. Fence until [`set_form`] says otherwise: LFENCE is in every x86-64
/// processor and both vendors stand behind it.
static FORM: AtomicU8 = AtomicU8::new(FENCE);

/// Sets the form every later lookup uses. The boot processor does this once, before any
/// program runs.
pub fn set_form(form: Form) {
    let raw = match form {
        Form::Mask => MASK,
        Form::Fence => FENCE,
    };
    FORM.store(raw, Ordering::Relaxed);
}

/// The form in use.
#[inline]
#[must_use]
pub fn form() -> Form {
    if FORM.load(Ordering::Relaxed) == MASK {
        Form::Mask
    } else {
        Form::Fence
    }
}

/// `index` if it is below `len`, and nothing if it is not. The index returned is also in
/// range on a path the processor took by guessing this check wrong: it is 0 there (Mask), or
/// that path does not run (Fence).
///
/// An empty table has no index in range, and 0 is what a wrong guess gets. Whatever reads
/// the table next must not turn that into a load from memory that matters: a `Vec`'s
/// `get` reads through its dangling pointer there, which is a small number and not mapped.
#[inline]
#[must_use]
pub fn index(index: usize, len: usize) -> Option<usize> {
    index_in(form(), index, len)
}

/// [`index`] in a form the caller names. The boot proof runs both.
#[inline]
#[must_use]
pub fn index_in(form: Form, index: usize, len: usize) -> Option<usize> {
    // `usize` is 64 bits on the kernel's target and on every host its tests run on.
    let (wide, limit) = (index as u64, len as u64);
    match form {
        Form::Mask => {
            let kept = below_or_zero(wide, limit);
            (index < len).then_some(kept as usize)
        }
        Form::Fence => (index < len).then(|| after_fence(wide) as usize),
    }
}

/// `len` if it is at most `max`, and nothing if it is more: for a count a program supplies
/// that sizes a piece of a kernel buffer. On a path the processor took by guessing this check
/// wrong the count is 0 (Mask), so nothing is copied past the buffer, or that path does not
/// run (Fence).
#[inline]
#[must_use]
pub fn length(len: usize, max: usize) -> Option<usize> {
    length_in(form(), len, max)
}

/// [`length`] in a form the caller names.
#[inline]
#[must_use]
pub fn length_in(form: Form, len: usize, max: usize) -> Option<usize> {
    match max.checked_add(1) {
        // `len <= max` is `len < max + 1`.
        Some(limit) => index_in(form, len, limit),
        // Every length is at most the largest: there is nothing to keep in range.
        None => Some(len),
    }
}

/// An address for a copy to or from a program's memory, **after** the range check has passed.
/// On a path the processor took by guessing that check wrong, an address at or above the end
/// of the user half is 0 (Mask), which is never mapped, or the path does not run (Fence).
///
/// Only the start needs this. A copy that starts in the user half cannot reach the kernel's
/// half without crossing the addresses between them, which no processor can read.
#[inline]
#[must_use]
pub fn user_address(address: u64) -> u64 {
    user_address_in(form(), address)
}

/// [`user_address`] in a form the caller names.
#[inline]
#[must_use]
pub fn user_address_in(form: Form, address: u64) -> u64 {
    match form {
        Form::Mask => below_or_zero(address, USER_END),
        Form::Fence => after_fence(address),
    }
}

#[cfg(target_os = "none")]
use crate::cpu::{after_fence, below_or_zero};

/// What `cpu::below_or_zero` computes, for a host.
#[cfg(not(target_os = "none"))]
const fn below_or_zero(value: u64, limit: u64) -> u64 {
    if value < limit { value } else { 0 }
}

/// What `cpu::after_fence` returns, for a host.
#[cfg(not(target_os = "none"))]
const fn after_fence(value: u64) -> u64 {
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usercopy::USER_START;

    const FORMS: [Form; 2] = [Form::Mask, Form::Fence];

    #[test]
    fn an_index_is_returned_only_in_range_in_both_forms() {
        for form in FORMS {
            for len in [0usize, 1, 2, 3, 255, 256, 0xFFFF] {
                for index in [0usize, 1, 2, 3, 254, 255, 256, 0xFFFE, 0xFFFF, 0x1_0000] {
                    let got = index_in(form, index, len);
                    let want = (index < len).then_some(index);
                    assert_eq!(got, want, "{form:?}: index {index} of {len}");
                }
            }
            // The ends of the range: nothing wraps.
            assert_eq!(index_in(form, usize::MAX, usize::MAX), None);
            assert_eq!(
                index_in(form, usize::MAX - 1, usize::MAX),
                Some(usize::MAX - 1)
            );
            assert_eq!(index_in(form, usize::MAX, 0), None);
        }
    }

    #[test]
    fn a_length_may_reach_its_limit_and_not_pass_it() {
        for form in FORMS {
            for max in [0usize, 1, 64, 256, usize::MAX - 1] {
                for len in [
                    0usize,
                    1,
                    63,
                    64,
                    65,
                    255,
                    256,
                    257,
                    usize::MAX - 1,
                    usize::MAX,
                ] {
                    let want = (len <= max).then_some(len);
                    assert_eq!(length_in(form, len, max), want, "{form:?}: {len} of {max}");
                }
            }
            assert_eq!(length_in(form, usize::MAX, usize::MAX), Some(usize::MAX));
            assert_eq!(length_in(form, 0, usize::MAX), Some(0));
        }
    }

    #[test]
    fn random_indexes_and_lengths_agree_with_the_plain_check() {
        // A fixed seed: the same hundred thousand pairs on every run.
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for round in 0..100_000 {
            // Whole-width values, and small ones, where in range and out of range are both
            // common.
            let (index, len) = if round % 2 == 0 {
                (next() as usize, next() as usize)
            } else {
                ((next() % 300) as usize, (next() % 300) as usize)
            };
            for form in FORMS {
                assert_eq!(
                    index_in(form, index, len),
                    (index < len).then_some(index),
                    "{form:?}: index {index} of {len}"
                );
                assert_eq!(
                    length_in(form, index, len),
                    (index <= len).then_some(index),
                    "{form:?}: length {index} of {len}"
                );
            }
        }
    }

    #[test]
    fn a_user_address_passes_and_any_other_is_zero_under_the_mask() {
        // `USER_END - PAGE` is the largest page a check lets through: the last a walk of a
        // program's tables visits (`UserSpace::checked`) and the last a mapping call starts at
        // (`map_shared_run`, `unmap`, `protect`). Those four clamps change nothing there.
        for address in [
            USER_START,
            0x40_0000,
            USER_END - origin_paging::PAGE,
            USER_END - 1,
        ] {
            for form in FORMS {
                assert_eq!(user_address_in(form, address), address, "{form:?}");
            }
        }
        // The first address past the user half, the unreadable addresses, the kernel's half.
        for address in [
            USER_END,
            0x0000_8000_0000_0000,
            0xFFFF_8000_0000_0000,
            u64::MAX,
        ] {
            assert_eq!(user_address_in(Form::Mask, address), 0, "{address:#x}");
            // The fence changes no value: it is the check before it that refuses these.
            assert_eq!(user_address_in(Form::Fence, address), address);
        }
    }

    #[test]
    fn the_form_starts_as_fence_and_can_be_set() {
        // The one test that touches the shared setting.
        assert_eq!(form(), Form::Fence);
        set_form(Form::Mask);
        assert_eq!(form(), Form::Mask);
        assert_eq!(index(3, 4), Some(3));
        assert_eq!(index(4, 4), None);
        set_form(Form::Fence);
        assert_eq!(form(), Form::Fence);
        assert_eq!(index(3, 4), Some(3));
        assert_eq!(index(4, 4), None);
    }
}
