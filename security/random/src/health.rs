//! The tests every draw of one instruction on one processor passes before it is used
//! (ADR-0016 R4; NIST SP 800-90B §4.3 and §4.4, as the ledger's section 5 works them).
//!
//! **What these are.** RDSEED and RDRAND return a conditioner's and a generator's output. The
//! tests SP 800-90B requires run on raw noise, inside the processor. What runs here are further
//! tests on what the instruction hands out, which the standard allows. They catch gross failure:
//! a constant, a repeat, a value that comes round again, a bit that never moves. They cannot
//! catch a source that looks random and is predictable. Origin claims no SP 800-90B conformance
//! (CLAUDE.md §59, decision item 149).
//!
//! **The tests, on 64-bit draws:**
//!
//! 1. Zero and all ones are not values: such a draw is set aside and the instruction is tried
//!    again. (Zero is AMD's own workaround for AMD-SB-7055; all ones is a second stuck pattern.)
//! 2. Repetition count, cutoff 2: a draw equal to the one before it fails.
//! 3. Adaptive proportion, a window of 512 draws, cutoff 2: the window's first draw coming again
//!    inside the window fails.
//! 4. Stuck bit, Origin's own: in each window of 512 draws every one of the 64 bit positions
//!    must take both values.
//!
//! A start-up test is 1,024 consecutive draws that pass all four: two whole windows.
//!
//! **What is remembered.** The draw before, the window's first draw, and two masks of the bits
//! seen. Each remembered draw is one of at least eight that go into any seed. They live with the
//! generator's state and are overwritten with it.

/// Draws in one window of the adaptive proportion and stuck-bit tests (SP 800-90B §4.4.2: 512
/// for a source that is not binary).
pub const WINDOW: u16 = 512;
/// Consecutive passing draws a start-up test takes (SP 800-90B §4.3 item 4 asks for at least
/// 1,024).
pub const STARTUP_DRAWS: u16 = 1024;

/// What a health test found wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// A draw equal to the draw before it.
    Repeat,
    /// A window's first draw came again inside the window.
    Proportion,
    /// A bit position that kept one value through a whole window.
    StuckBit,
    /// A draw whose tries gave nothing but zero or all ones, with the instruction reporting
    /// success.
    StuckValue,
}

/// Where one instruction stands on one processor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Condition {
    /// The processor does not report the instruction.
    Absent,
    /// Reported, and no start-up test has been run.
    Untested,
    /// A start-up test is running: no draw is used unless it completes.
    Testing,
    /// A start-up test passed, and every draw since has passed the continuous tests.
    Healthy,
    /// A test failed, or a start-up test could not be finished. Not drawn from for use until a
    /// whole new start-up test passes.
    Unhealthy,
}

/// What became of one value the instruction returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offer {
    /// It passed: the caller may use it, or keep it until the running start-up test completes.
    Accepted,
    /// Zero or all ones: not a value. Try the instruction again.
    SetAside,
    /// A test failed. The instruction is unhealthy, and what was gathered from it is to be
    /// thrown away.
    Failed(Fault),
}

/// One instruction's tests on one processor.
pub struct Health {
    condition: Condition,
    /// Draws the running start-up test has passed.
    passed: u16,
    /// Whether `last` holds a draw.
    have_last: bool,
    /// The draw before.
    last: u64,
    /// Draws taken into the current window.
    position: u16,
    /// The current window's first draw.
    first: u64,
    /// Bits seen set in the current window.
    ones: u64,
    /// Bits seen clear in the current window.
    zeros: u64,
    /// Draws that passed, ever.
    accepted: u64,
    /// Zero or all-ones values set aside, ever.
    set_aside: u32,
    /// Failures, ever.
    faults: u32,
    /// The last failure.
    last_fault: Option<Fault>,
}

impl Health {
    /// The tests of an instruction the processor does not report.
    pub const fn absent() -> Self {
        Self::in_condition(Condition::Absent)
    }

    /// The tests of an instruction the processor reports, before any start-up test.
    pub const fn untested() -> Self {
        Self::in_condition(Condition::Untested)
    }

    const fn in_condition(condition: Condition) -> Self {
        Self {
            condition,
            passed: 0,
            have_last: false,
            last: 0,
            position: 0,
            first: 0,
            ones: 0,
            zeros: 0,
            accepted: 0,
            set_aside: 0,
            faults: 0,
            last_fault: None,
        }
    }

    /// Where the instruction stands.
    pub const fn condition(&self) -> Condition {
        self.condition
    }

    /// Whether its draws may be used.
    pub const fn is_healthy(&self) -> bool {
        matches!(self.condition, Condition::Healthy)
    }

    /// Draws the running start-up test has passed.
    pub const fn passed(&self) -> u16 {
        self.passed
    }

    /// Draws that have passed, since the processor started.
    pub const fn accepted(&self) -> u64 {
        self.accepted
    }

    /// Values set aside as zero or all ones, since the processor started.
    pub const fn set_aside(&self) -> u32 {
        self.set_aside
    }

    /// Failures since the processor started.
    pub const fn faults(&self) -> u32 {
        self.faults
    }

    /// The last failure.
    pub const fn last_fault(&self) -> Option<Fault> {
        self.last_fault
    }

    /// Starts a start-up test from nothing: what an earlier test remembered is forgotten.
    /// Does nothing for an instruction the processor does not report.
    pub fn begin_test(&mut self) {
        if matches!(self.condition, Condition::Absent) {
            return;
        }
        self.forget();
        self.condition = Condition::Testing;
    }

    /// Gives up the running start-up test without a fault: the instruction was not ready for
    /// too long. It is unhealthy until a later test passes.
    pub fn abandon_test(&mut self) {
        if matches!(self.condition, Condition::Testing) {
            self.forget();
            self.condition = Condition::Unhealthy;
        }
    }

    /// Runs the tests on one value the instruction returned with success.
    ///
    /// Only an instruction that is being tested or is healthy is offered values; for any other
    /// the value is set aside, which uses nothing.
    pub fn offer(&mut self, value: u64) -> Offer {
        if !matches!(self.condition, Condition::Testing | Condition::Healthy) {
            return Offer::SetAside;
        }
        if value == 0 || value == u64::MAX {
            self.set_aside = self.set_aside.saturating_add(1);
            return Offer::SetAside;
        }
        if self.have_last && value == self.last {
            return self.fail(Fault::Repeat);
        }
        if self.position != 0 && value == self.first {
            return self.fail(Fault::Proportion);
        }
        if self.position == 0 {
            self.first = value;
        }
        self.ones |= value;
        self.zeros |= !value;
        self.position = self.position.saturating_add(1);
        if self.position >= WINDOW {
            if self.ones != u64::MAX || self.zeros != u64::MAX {
                return self.fail(Fault::StuckBit);
            }
            self.position = 0;
            self.first = 0;
            self.ones = 0;
            self.zeros = 0;
        }
        self.last = value;
        self.have_last = true;
        self.accepted = self.accepted.saturating_add(1);
        if matches!(self.condition, Condition::Testing) {
            self.passed = self.passed.saturating_add(1);
            if self.passed >= STARTUP_DRAWS {
                self.condition = Condition::Healthy;
            }
        }
        Offer::Accepted
    }

    /// Records that a draw's every usable try gave zero or all ones: the instruction is stuck.
    pub fn stuck(&mut self) -> Offer {
        if matches!(self.condition, Condition::Testing | Condition::Healthy) {
            self.fail(Fault::StuckValue)
        } else {
            Offer::SetAside
        }
    }

    fn fail(&mut self, fault: Fault) -> Offer {
        self.forget();
        self.condition = Condition::Unhealthy;
        self.faults = self.faults.saturating_add(1);
        self.last_fault = Some(fault);
        Offer::Failed(fault)
    }

    /// Forgets the remembered draws and the running test's progress.
    fn forget(&mut self) {
        self.passed = 0;
        self.have_last = false;
        self.last = 0;
        self.position = 0;
        self.first = 0;
        self.ones = 0;
        self.zeros = 0;
    }
}

impl core::fmt::Debug for Health {
    /// Prints no remembered draw (CLAUDE.md §61).
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Health { .. }")
    }
}
