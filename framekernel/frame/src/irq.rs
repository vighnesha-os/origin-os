//! Device interrupt registration (ADR-0011 N6): a driver names the vector its device raises and
//! the handler to run, instead of the trap entry calling up into drivers by name.
//!
//! The handler runs with interrupts disabled, on the kernel stack of whatever the interrupt
//! interrupted, after the interrupt has been acknowledged. It says whether the interrupt may
//! have made a thread ready ([`Action::Reschedule`], for instance by waking a waiter), in which
//! case the scheduler gets to choose what runs next.
//!
//! Vectors have one owner each: registering a vector twice, or one outside [`FIRST`]..=[`LAST`],
//! is refused. Registration comes before the device is told to send the interrupt.

use core::sync::atomic::{AtomicUsize, Ordering};

/// The first device vector: below it are the 32 exceptions.
pub const FIRST: u8 = 0x40;
/// The last device vector: 16 of them (above are the scheduler's and the IPIs).
pub const LAST: u8 = 0x4F;

/// What an interrupt handler asks for when it returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Resume whatever was interrupted.
    Resume,
    /// A thread may have become ready: let the scheduler choose what runs next.
    Reschedule,
}

/// A device interrupt handler.
pub type Handler = fn() -> Action;

/// Why [`register`] refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterError {
    /// The vector is outside [`FIRST`]..=[`LAST`].
    NotADeviceVector,
    /// Another handler already owns this vector.
    Taken,
}

const COUNT: usize = (LAST - FIRST) as usize + 1;
/// Each vector's handler as a `usize` (a `fn` pointer), or 0 for none.
static HANDLERS: [AtomicUsize; COUNT] = [const { AtomicUsize::new(0) }; COUNT];

/// Makes `handler` the handler of device vector `vector`. The vector is taken for good.
pub fn register(vector: u8, handler: Handler) -> Result<(), RegisterError> {
    let slot = index(vector).ok_or(RegisterError::NotADeviceVector)?;
    HANDLERS
        .get(slot)
        .ok_or(RegisterError::NotADeviceVector)?
        .compare_exchange(0, handler as usize, Ordering::AcqRel, Ordering::Acquire)
        .map(drop)
        .map_err(|_| RegisterError::Taken)
}

/// The slot of device vector `vector`, if it is one.
fn index(vector: u8) -> Option<usize> {
    (FIRST..=LAST)
        .contains(&vector)
        .then(|| usize::from(vector - FIRST))
}

/// Whether `vector` is a device vector, one that has a handler slot.
pub const fn is_device_vector(vector: u8) -> bool {
    vector >= FIRST && vector <= LAST
}

/// Runs `vector`'s handler if it has one; a vector no driver claimed resumes what it
/// interrupted (a device that interrupts before its driver registers is lost, as an
/// interrupt with no handler always is).
pub fn run(vector: u8) -> Action {
    let raw = index(vector)
        .and_then(|i| HANDLERS.get(i))
        .map_or(0, |slot| slot.load(Ordering::Acquire));
    if raw == 0 {
        return Action::Resume;
    }
    // SAFETY: non-zero values in `HANDLERS` are `Handler` function pointers: only `register`
    // writes them, and it stores `handler as usize`, which round-trips for a function pointer
    // of the same type.
    let handler = unsafe { core::mem::transmute::<usize, Handler>(raw) };
    handler()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resume() -> Action {
        Action::Resume
    }

    fn resched() -> Action {
        Action::Reschedule
    }

    // One test: `HANDLERS` is one global table, and vectors used here stay taken.
    #[test]
    fn a_vector_has_one_owner_and_runs_its_handler() {
        assert_eq!(register(0x20, resume), Err(RegisterError::NotADeviceVector));
        assert_eq!(register(0x50, resume), Err(RegisterError::NotADeviceVector));
        assert_eq!(register(0x44, resched), Ok(()));
        assert_eq!(register(0x44, resume), Err(RegisterError::Taken));
        assert_eq!(run(0x44), Action::Reschedule);
        assert_eq!(register(0x4F, resume), Ok(()));
        assert_eq!(run(0x4F), Action::Resume);
        // A vector no driver claimed resumes what it interrupted.
        assert_eq!(run(0x45), Action::Resume);
        assert_eq!(run(0x20), Action::Resume);
        assert!(is_device_vector(FIRST) && is_device_vector(LAST));
        assert!(!is_device_vector(FIRST - 1) && !is_device_vector(LAST + 1));
    }
}
