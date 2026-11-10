//! Overwriting secret material without `unsafe` (CLAUDE.md §59; ADR-0016).
//!
//! **What this does.** Each function writes zeros over a buffer that held secret material and
//! then hands the buffer to [`core::hint::black_box`], so the compiler has to assume the zeros
//! are read. The functions are never inlined: a caller holds a call to code with an opaque
//! use in it, which the optimizer cannot drop as a dead store.
//!
//! **What it does not promise.** `black_box` is documented as best effort, and as giving no
//! guarantee for cryptographic or security purposes (`core::hint`, Rust 1.99.1). A write the
//! compiler may never remove is a volatile write, which is `unsafe`, and this crate forbids
//! `unsafe`. The compiler may also keep copies of a secret in registers and in stack slots of
//! its own, which no source-level overwrite reaches.
//!
//! So this is the first of two layers. The second is the caller's: the Framekernel keeps the
//! generator's state in storage it overwrites with volatile writes, and overwrites the stack a
//! request ran on before it lets anything else run there (ADR-0016).

use core::hint::black_box;

/// Overwrites `bytes` with zeros.
#[inline(never)]
pub(crate) fn wipe(bytes: &mut [u8]) {
    bytes.fill(0);
    black_box(bytes);
}

/// Overwrites `words` with zeros.
#[inline(never)]
pub(crate) fn wipe_words(words: &mut [u64]) {
    words.fill(0);
    black_box(words);
}
