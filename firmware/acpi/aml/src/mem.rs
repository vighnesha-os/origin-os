//! Every allocation the interpreter makes, and the count of bytes against the memory limit
//! (ADR-0019 A3; the contract's §1 and §5).
//!
//! This is the only file of the library that may call a collection's growing methods
//! (`push`, `extend_from_slice`, `resize`); `cargo xtask check`'s allocation gate refuses them
//! anywhere else. Each growth here is preceded by a `try_reserve_exact` that succeeded, so the
//! growing method that follows never reallocates: `alloc`'s capacity-overflow and
//! allocation-failure paths, which would end the program, are never reached. (They remain in
//! the compiled object, because Rust 1.100.0's `Vec` has no stable non-growing push; the no-panic
//! check allows `alloc::raw_vec::handle_error` for this crate alone, for that reason.)
//!
//! Before anything is reserved, its bytes are charged to the [`Budget`]; a charge that would pass
//! the limit is refused with [`Limit::Memory`] and nothing is allocated. What is freed is credited
//! back by [`release`].

use alloc::vec::Vec;
use core::mem::size_of;

use crate::error::{Error, ErrorKind, Limit, Result};

/// The bytes the interpreter holds, against its limit.
#[derive(Debug)]
pub(crate) struct Budget {
    used: usize,
    limit: usize,
    peak: usize,
    /// Every byte ever charged, and every byte a reallocation moved: the engine counts a step for
    /// each 64, as work done.
    charged: u64,
}

const fn memory() -> Error {
    Error::new(ErrorKind::Limit(Limit::Memory))
}

impl Budget {
    pub(crate) const fn new(limit: usize) -> Self {
        Self {
            used: 0,
            limit,
            peak: 0,
            charged: 0,
        }
    }

    pub(crate) const fn used(&self) -> usize {
        self.used
    }

    pub(crate) const fn peak(&self) -> usize {
        self.peak
    }

    pub(crate) const fn limit(&self) -> usize {
        self.limit
    }

    /// Every byte ever charged.
    pub(crate) const fn charged(&self) -> u64 {
        self.charged
    }

    /// Charges `bytes`, or refuses them if the limit would be passed.
    fn charge(&mut self, bytes: usize) -> Result<()> {
        let used = self.used.checked_add(bytes).ok_or_else(memory)?;
        // Planted, the limit is checked at twice its value: weakened, not removed, so that a
        // test planting it never asks the machine for more than twice the limit.
        #[cfg(test)]
        let used_checked = if crate::tests::planted(crate::tests::Fault::LooseMemoryLimit) {
            used / 2
        } else {
            used
        };
        #[cfg(not(test))]
        let used_checked = used;
        if used_checked > self.limit {
            return Err(memory());
        }
        self.used = used;
        if used > self.peak {
            self.peak = used;
        }
        self.charged = self
            .charged
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        Ok(())
    }

    /// Counts `bytes` moved (a reallocation's copy) as work, with what is allocated.
    fn note_moved(&mut self, bytes: usize) {
        self.charged = self
            .charged
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
    }

    fn credit(&mut self, bytes: usize) {
        self.used = self.used.saturating_sub(bytes);
    }

    /// The peak and the work count before a charge, for [`Budget::refused`].
    const fn mark(&self) -> (usize, u64) {
        (self.peak, self.charged)
    }

    /// Takes back `bytes` charged for an allocation the allocator then refused (a heap too
    /// small for the limit: the coordinating session's ruling on step B3's limit, 2026-11-14).
    /// They were never held and nothing was moved, so the count, the peak and the work done
    /// return to what `mark` kept.
    fn refused(&mut self, bytes: usize, mark: (usize, u64)) {
        self.credit(bytes);
        (self.peak, self.charged) = mark;
    }

    /// Sets the count to what is held, counted afresh by the caller. Called when an evaluation
    /// ends, so that a charge a failure path did not credit is never kept (the review of B1, F2).
    pub(crate) fn recount(&mut self, held: usize) {
        self.used = held;
        if held > self.peak {
            self.peak = held;
        }
    }
}

fn bytes_for<T>(count: usize) -> Result<usize> {
    count.checked_mul(size_of::<T>()).ok_or_else(memory)
}

/// Makes room in `v` for `additional` more elements: the capacity doubles (at least 4); when
/// doubling would pass the limit it grows once to all the limit allows, and when even that is
/// short of what is needed it is refused. It never grows by what is asked alone, so a vector near
/// the limit is not copied whole for each element pushed (the review of B1, F1).
pub(crate) fn reserve<T>(v: &mut Vec<T>, additional: usize, budget: &mut Budget) -> Result<()> {
    let need = v.len().checked_add(additional).ok_or_else(memory)?;
    let have = v.capacity();
    if need <= have {
        return Ok(());
    }
    let doubled = have.saturating_mul(2).max(4).max(need);
    let room = budget
        .limit
        .saturating_sub(budget.used)
        .checked_div(size_of::<T>())
        .unwrap_or(usize::MAX);
    let target = doubled.min(have.saturating_add(room));
    if target < need {
        return Err(memory());
    }
    let mark = budget.mark();
    budget.charge(bytes_for::<T>(target.saturating_sub(have))?)?;
    // Growing a vector that holds something copies it: that is work, counted with what is
    // allocated (the review of B1, A1).
    budget.note_moved(bytes_for::<T>(v.len()).unwrap_or(usize::MAX));
    if v.try_reserve_exact(target.saturating_sub(v.len())).is_err() {
        budget.refused(
            bytes_for::<T>(target.saturating_sub(have)).unwrap_or(0),
            mark,
        );
        return Err(memory());
    }
    // `try_reserve_exact` gives exactly the capacity asked for an element type with a size; a
    // larger one is charged as well, so the count never under-states what is held.
    let got = v.capacity();
    if got > target {
        budget.charge(bytes_for::<T>(got.saturating_sub(target))?)?;
    }
    Ok(())
}

/// Appends `x` to `v`.
pub(crate) fn push<T>(v: &mut Vec<T>, x: T, budget: &mut Budget) -> Result<()> {
    reserve(v, 1, budget)?;
    v.push(x);
    Ok(())
}

/// An empty vector able to hold `count` elements.
pub(crate) fn with_capacity<T>(count: usize, budget: &mut Budget) -> Result<Vec<T>> {
    let mut v = Vec::new();
    if count > 0 {
        let mark = budget.mark();
        budget.charge(bytes_for::<T>(count)?)?;
        if v.try_reserve_exact(count).is_err() {
            budget.refused(bytes_for::<T>(count).unwrap_or(0), mark);
            return Err(memory());
        }
        let got = v.capacity();
        if got > count {
            budget.charge(bytes_for::<T>(got.saturating_sub(count))?)?;
        }
    }
    Ok(v)
}

/// A copy of `src`.
pub(crate) fn copy_bytes(src: &[u8], budget: &mut Budget) -> Result<Vec<u8>> {
    let mut v = with_capacity(src.len(), budget)?;
    v.extend_from_slice(src);
    Ok(v)
}

/// `count` zero bytes.
pub(crate) fn zeroed(count: usize, budget: &mut Budget) -> Result<Vec<u8>> {
    let mut v = with_capacity(count, budget)?;
    v.resize(count, 0);
    Ok(v)
}

/// `count` copies of `x`.
pub(crate) fn filled<T: Copy>(count: usize, x: T, budget: &mut Budget) -> Result<Vec<T>> {
    let mut v = with_capacity(count, budget)?;
    v.resize(count, x);
    Ok(v)
}

/// Appends `src` to `v`.
pub(crate) fn extend_bytes(v: &mut Vec<u8>, src: &[u8], budget: &mut Budget) -> Result<()> {
    reserve(v, src.len(), budget)?;
    v.extend_from_slice(src);
    Ok(())
}

/// Frees `v`, crediting what it held.
pub(crate) fn release<T>(v: Vec<T>, budget: &mut Budget) {
    budget.credit(bytes_for::<T>(v.capacity()).unwrap_or(0));
    drop(v);
}
