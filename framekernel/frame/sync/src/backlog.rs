//! Work handed to whoever drains it, counted until what it holds is given back (TASKS item 81).
//!
//! A closed channel's undelivered messages wait on a list for whoever reaps them: a system call
//! on any processor, a process's exit, a proof, or `settle` (`framekernel::ipc::reap_closed`).
//! Freeing them frees what they held: other channel ends, processes, their address spaces,
//! memory objects. A thread that is about to count the machine's memory must know that none of
//! that is still being freed on another processor. [`Backlog`] is the count it reads: the pieces
//! of work listed and not yet drained, and the ones being drained.
//!
//! - [`Backlog::added`]: after a piece is listed.
//! - [`Backlog::drained`]: runs one taken piece's `release`, then lowers the count.
//! - [`Backlog::pending`]: the count.
//!
//! Before TASKS item 81 the kernel lowered its count when it took a channel off the list, before
//! it freed the channel's messages, so a reader on another processor could find nothing pending
//! while those messages, and all they held, were still allocated (the review of TASKS item 79,
//! T79-6).
//!
//! # The ordering
//!
//! - Both changes of the count are AcqRel read-modify-writes, and [`Backlog::pending`] is an
//!   Acquire load.
//! - In C++20's model, which Rust's atomics follow, a release operation heads a release sequence
//!   that includes every later read-modify-write of the same location, by any thread
//!   ([intro.races]). An Acquire load that reads a value from that sequence synchronizes with the
//!   release operation that heads it. So a reader that reads the count lowered to 0, or any later
//!   value, synchronizes with every lowering before that value in the count's modification order,
//!   whichever processor made it. Each lowering comes after its `release` in its own thread, so
//!   everything every earlier drain freed happens before whatever the reader does next, such as
//!   counting the heap.
//! - A reader is ordered after a listing only through something the lister did after
//!   [`Backlog::added`] and the reader saw: in the kernel, an end it waited for (a thread's or a
//!   process's) that came after the close that listed the channel. Then the reader's Acquire load
//!   reads the raise or a later value, and a 0 means the piece was drained.
//!
//! The Loom model B1 (`models.rs`, "backlog") checks this, and its negative controls in xtask
//! break each part.
//!
//! **What a count of 0 does not say.** It does not say that nothing will be listed later: a
//! drain's release can close further ends, whose messages are listed and counted before the
//! release returns, so they are pending, not missed. And it does not pair a drain with the
//! listing it drains: the list's own lock does that. A drain with no listing behind it would
//! wrap the count, which every later settle would then find pending: never early.
//!
//! Its `Debug` writes its name alone: the kernel's no-panic check refuses formatting beyond
//! `write_str` in this crate's release object.
#![forbid(unsafe_code)]

use core::sync::atomic::Ordering::{AcqRel, Acquire};

use crate::prim;

/// The count of pieces of work listed and not yet drained, and of pieces being drained; see the
/// module documentation.
pub struct Backlog {
    count: prim::AtomicU64,
}

impl core::fmt::Debug for Backlog {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Backlog")
    }
}

impl Backlog {
    /// Nothing listed.
    #[cfg(not(all(test, loom)))]
    pub const fn new() -> Self {
        Self {
            count: prim::AtomicU64::new(0),
        }
    }

    /// Nothing listed.
    #[cfg(all(test, loom))]
    pub fn new() -> Self {
        Self {
            count: prim::AtomicU64::new(0),
        }
    }

    /// Counts a piece of work its caller has just listed.
    pub fn added(&self) {
        self.count.fetch_add(1, AcqRel);
    }

    /// Runs `release`, which frees one piece of work its caller took off the list, and only then
    /// stops counting it; returns what `release` returned.
    pub fn drained<R>(&self, release: impl FnOnce() -> R) -> R {
        let released = release();
        self.count.fetch_sub(1, AcqRel);
        released
    }

    /// The pieces listed and not yet drained, and those being drained.
    pub fn pending(&self) -> u64 {
        self.count.load(Acquire)
    }
}

impl Default for Backlog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[test]
    fn a_piece_is_pending_from_its_listing_until_its_release_has_run() {
        let backlog = Backlog::new();
        assert_eq!(backlog.pending(), 0);
        backlog.added();
        backlog.added();
        assert_eq!(backlog.pending(), 2);
        let seen_inside = backlog.drained(|| backlog.pending());
        assert_eq!(seen_inside, 2, "still counted while its release runs");
        assert_eq!(backlog.pending(), 1);
        backlog.drained(|| ());
        assert_eq!(backlog.pending(), 0);
    }

    #[test]
    fn a_release_that_lists_more_leaves_them_pending() {
        let backlog = Backlog::new();
        backlog.added();
        backlog.drained(|| backlog.added());
        assert_eq!(backlog.pending(), 1, "what the release listed is counted");
    }

    #[test]
    fn debug_names_nothing_it_counts() {
        let backlog = Backlog::new();
        backlog.added();
        assert_eq!(format!("{backlog:?}"), "Backlog");
    }
}
