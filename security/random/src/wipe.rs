//! Overwriting what held a draw or a seed, without `unsafe` (ADR-0016 R9).
//!
//! Everything this crate overwrites is one of two things.
//!
//! - **A field of a [`crate::Node`] or a [`crate::Root`], or the caller's seed area.** Those
//!   are reached through a `&mut` the caller keeps, in storage that is read again later, so the
//!   zeros written here are stores the compiler has to make. The Framekernel keeps that storage
//!   static and overwrites it once more with volatile writes when a generator is uninstantiated
//!   (R9 Z1).
//! - **Nothing on a stack.** This crate keeps no draw, seed or state in a local of its own that
//!   it could overwrite afterwards. A draw passes through registers and through stack slots the
//!   compiler chooses, and those are the caller's to overwrite: every step runs on a stack the
//!   Framekernel overwrites whole before it lets anything else run (R9 Z3; TASKS 18b S5).
//!
//! [`core::hint::black_box`] is used as `origin-crypto` uses it: it is documented as best
//! effort and as no guarantee for security (`core::hint`, Rust 1.99.1), and it costs nothing
//! to add.

use core::hint::black_box;

/// Overwrites `bytes` with zeros.
#[inline(never)]
pub(crate) fn wipe(bytes: &mut [u8]) {
    bytes.fill(0);
    black_box(bytes);
}

/// Copies as many bytes as both slices hold. It cannot panic: a length that does not match
/// copies the shorter length, and every caller here passes equal lengths.
pub(crate) fn copy(to: &mut [u8], from: &[u8]) {
    for (to, from) in to.iter_mut().zip(from) {
        *to = *from;
    }
}
