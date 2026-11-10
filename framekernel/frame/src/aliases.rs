//! W^X across the mappings of one frame (CLAUDE.md §14, §58; review finding 5 of S3-5a).
//!
//! A page-table entry is never writable and executable at once (`origin_paging::Access` has no
//! such value). A frame that several mappings share could still be writable through one and
//! executable through another, in one space or two, and so be written as data and run as code.
//! [`Aliases`] counts a shared frame's writable and executable mappings, and refuses a mapping
//! that would give it both. Read-only mappings are not counted.
//!
//! The counts include every mapping a processor may still use: a mapping is counted before its
//! entry is written, and given back only after the shootdown that removes the access has
//! completed. So at no moment can one processor write the frame while another can execute it.
#![forbid(unsafe_code)]

use core::sync::atomic::{AtomicU64, Ordering};

use origin_paging::Access;

/// What one mapping lets a processor do with a frame, as far as W^X is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Read only: compatible with anything.
    Plain,
    /// Writable.
    Write,
    /// Executable.
    Execute,
}

impl Mode {
    /// The mode of a mapping with `access`.
    #[must_use]
    pub const fn of(access: Access) -> Self {
        match access {
            Access::Read => Self::Plain,
            Access::ReadWrite => Self::Write,
            Access::ReadExecute => Self::Execute,
        }
    }
}

/// The writable and the executable mappings of one frame: the low half of the word counts the
/// first, the high half the second, so that one atomic step checks and changes both.
#[derive(Debug, Default)]
pub struct Aliases {
    counts: AtomicU64,
}

const ONE_WRITER: u64 = 1;
const ONE_EXECUTOR: u64 = 1 << 32;
const WRITERS: u64 = (1 << 32) - 1;

impl Aliases {
    /// No mapping counted.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            counts: AtomicU64::new(0),
        }
    }

    /// Counts one more mapping with `mode`, unless the frame would then be writable through one
    /// mapping and executable through another, or a count would overflow. Says whether it did.
    pub fn take(&self, mode: Mode) -> bool {
        let step = match mode {
            Mode::Plain => return true,
            Mode::Write => ONE_WRITER,
            Mode::Execute => ONE_EXECUTOR,
        };
        self.counts
            .try_update(Ordering::AcqRel, Ordering::Acquire, |counts| {
                let (writers, executors) = (counts & WRITERS, counts >> 32);
                let allowed = match mode {
                    Mode::Write => executors == 0 && writers < WRITERS,
                    Mode::Execute => writers == 0 && executors < u64::from(u32::MAX),
                    Mode::Plain => true,
                };
                allowed.then(|| counts + step)
            })
            .is_ok()
    }

    /// Gives back a mapping counted with `mode`: only once no processor can use it any more.
    pub fn give_back(&self, mode: Mode) {
        let step = match mode {
            Mode::Plain => return,
            Mode::Write => ONE_WRITER,
            Mode::Execute => ONE_EXECUTOR,
        };
        // A mode is given back once for each time it was taken; the caller keeps that pairing.
        let _ = self
            .counts
            .try_update(Ordering::AcqRel, Ordering::Acquire, |counts| {
                let present = match mode {
                    Mode::Write => counts & WRITERS != 0,
                    Mode::Execute => counts >> 32 != 0,
                    Mode::Plain => false,
                };
                present.then(|| counts - step)
            });
    }

    /// The writable and the executable mappings counted now.
    #[must_use]
    pub fn counts(&self) -> (u32, u32) {
        let counts = self.counts.load(Ordering::Acquire);
        ((counts & WRITERS) as u32, (counts >> 32) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_writable_or_executable_through_its_mappings_never_both() {
        let aliases = Aliases::new();
        assert!(aliases.take(Mode::Plain));
        assert!(aliases.take(Mode::Write));
        assert!(aliases.take(Mode::Write), "two writable mappings are fine");
        assert!(
            !aliases.take(Mode::Execute),
            "not while it is writable anywhere"
        );
        assert_eq!(aliases.counts(), (2, 0));
        aliases.give_back(Mode::Write);
        assert!(!aliases.take(Mode::Execute), "one writable mapping is left");
        aliases.give_back(Mode::Write);
        assert!(aliases.take(Mode::Execute));
        assert!(aliases.take(Mode::Execute), "code can be shared");
        assert!(
            !aliases.take(Mode::Write),
            "not while it is executable anywhere"
        );
        assert!(aliases.take(Mode::Plain), "read-only is always fine");
        assert_eq!(aliases.counts(), (0, 2));
        aliases.give_back(Mode::Execute);
        aliases.give_back(Mode::Execute);
        aliases.give_back(Mode::Plain);
        assert_eq!(aliases.counts(), (0, 0));
        assert!(aliases.take(Mode::Write));
    }

    #[test]
    fn giving_back_what_was_never_taken_changes_nothing() {
        let aliases = Aliases::new();
        aliases.give_back(Mode::Write);
        aliases.give_back(Mode::Execute);
        assert_eq!(aliases.counts(), (0, 0));
        assert!(aliases.take(Mode::Execute));
        aliases.give_back(Mode::Write);
        assert_eq!(
            aliases.counts(),
            (0, 1),
            "the executable count is untouched"
        );
    }

    #[test]
    fn modes_follow_access() {
        assert_eq!(Mode::of(Access::Read), Mode::Plain);
        assert_eq!(Mode::of(Access::ReadWrite), Mode::Write);
        assert_eq!(Mode::of(Access::ReadExecute), Mode::Execute);
    }

    /// Many threads taking writable and executable mappings at once: whoever wins, the frame
    /// never has both.
    #[test]
    fn racing_takers_never_get_both() {
        for _ in 0..200 {
            let aliases = Aliases::new();
            let got: Vec<(Mode, bool)> = std::thread::scope(|s| {
                let handles: Vec<_> = (0..8)
                    .map(|i| {
                        let mode = if i % 2 == 0 {
                            Mode::Write
                        } else {
                            Mode::Execute
                        };
                        let aliases = &aliases;
                        s.spawn(move || (mode, aliases.take(mode)))
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });
            let writers = got
                .iter()
                .filter(|(m, ok)| *ok && *m == Mode::Write)
                .count();
            let executors = got
                .iter()
                .filter(|(m, ok)| *ok && *m == Mode::Execute)
                .count();
            assert!(writers == 0 || executors == 0, "{writers} and {executors}");
            assert_eq!(aliases.counts(), (writers as u32, executors as u32));
        }
    }
}
