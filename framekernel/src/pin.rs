//! The per-call pin's decision (ADR-0005 A6, B2 and Amendment 8, G1; ADR-0011 Amendment 2,
//! L7 and L8; task S4-2), as one pure function the host tests: where a process may make system
//! calls from, and whether one call is accepted, refused with a status, or stops its thread.
//!
//! `syscall::run` asks [`judge`] before it does anything else with a call, through one function
//! of its own that is never inlined, so that `cargo xtask check`'s image gate can read the pin
//! as compiled (the independent review of 2026-11-13, findings K2 and K4).
//!
//! It reads nothing but its arguments: the process's [`CallOrigin`], the offsets the kernel
//! carries for `origin.core` ([`Pins`], read from the linked image by the build), the number in
//! RAX, and the return address the `syscall` instruction left in RCX. Every step on a value
//! from ring 3 is a wrapping subtraction or a comparison: nothing indexes a table, nothing is
//! read through an address, and no arithmetic can overflow, so the kernel's overflow checks
//! cannot be reached from here.
#![forbid(unsafe_code)]

use origin_syscalls::origin_kernel::kernel::server::{self, Pins};

/// Where a process's system calls may come from. Fixed when the process is made; the one change
/// it ever makes is [`mapped_at`](Self::mapped_at), once (ADR-0011 Amendment 2, L8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallOrigin {
    /// One of the kernel's own proof processes: any `syscall` instruction inside
    /// `[start, end)`, the range its creator named (ADR-0005 A6). A call from outside it is
    /// `access_denied`.
    Range {
        /// The range's first address.
        start: u64,
        /// One past its last.
        end: u64,
    },
    /// Pinned per call (ADR-0005 B2): each call number only from its own `syscall`
    /// instruction in `origin.core`. `base` is where `origin.core` is in this process, recorded
    /// when it was mapped there (L7); `None` until then, and until then no call is accepted.
    Core {
        /// `origin.core`'s base in this process.
        base: Option<u64>,
    },
}

impl CallOrigin {
    /// What this origin becomes once `origin.core` is mapped at `base`: a pinned process that
    /// has no base yet records it, and nothing else may. `None` for a proof process and for a
    /// pinned process that already has one: `origin.core` is mapped into a process once.
    #[must_use]
    pub const fn mapped_at(self, base: u64) -> Option<Self> {
        match self {
            Self::Core { base: None } => Some(Self::Core { base: Some(base) }),
            Self::Core { base: Some(_) } | Self::Range { .. } => None,
        }
    }
}

/// What the kernel does with one system call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Dispatch it.
    Accept,
    /// Refuse it with `access_denied`: a proof process's call from outside its range.
    Deny,
    /// Give no status and stop the thread, as a fault does (G1): a pinned process's call that
    /// is not its number's own. `address` is where the `syscall` instruction was, the return
    /// address less two, which the thread's ending records with the number.
    Stop {
        /// The return address less two, wrapping.
        address: u64,
    },
}

/// The pin's decision for a call of `number` whose `syscall` instruction returned to `rip`, by a
/// process whose calls may come from `origin`, with `origin.core`'s offsets `pins`.
///
/// - A proof process: accepted if the two bytes of its `syscall` lie inside its range,
///   otherwise [`Verdict::Deny`].
/// - A pinned process: accepted only if `rip - 2 - base` is the offset of `number`'s own
///   `syscall` in `origin.core` (the generated [`server::pinned`], one comparison an arm, every
///   other number refused). Anything else, a process with no base among it, is
///   [`Verdict::Stop`]. The number is compared whole: a number with bits set above bit 31 is
///   no method's, and a stub's `mov eax, imm32` cannot make one.
#[inline]
#[must_use]
pub fn judge(origin: CallOrigin, pins: &Pins, number: u64, rip: u64) -> Verdict {
    match origin {
        CallOrigin::Range { start, end } => {
            if rip
                .checked_sub(2)
                .is_some_and(|at| at >= start && rip <= end)
            {
                Verdict::Accept
            } else {
                Verdict::Deny
            }
        }
        CallOrigin::Core { base } => {
            // The `syscall` instruction is the two bytes before the return address. Wrapping:
            // an address below 2, or below the base, gives an offset no stub has.
            let address = rip.wrapping_sub(2);
            if base.is_some_and(|base| server::pinned(pins, number, address.wrapping_sub(base))) {
                Verdict::Accept
            } else {
                Verdict::Stop { address }
            }
        }
    }
}

#[cfg(test)]
mod tests;
