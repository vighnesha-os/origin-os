//! Upcalls from the trap entry into the layers above it (ADR-0011 N6): the scheduler, the
//! system-call layer, the console that reports a fatal exception.
//!
//! The trap entry lives in the framework and may not name those layers (dependencies run one
//! way), so they install their entry points once at boot, before interrupts can arrive. A
//! *frame* here is the address of a thread's saved register frame on its kernel stack: the trap
//! entry passes the interrupted one, and the scheduler hook answers which to resume.

use core::marker::PhantomData;

use crate::once::SetOnce;

/// The scheduler's switch point: the moment, inside the trap entry with interrupts off, when
/// it chooses what this processor runs next. Only the trap entry makes one, for the scheduling
/// hooks, and only for the length of the call (it is not `Send`, and the hooks get a borrow).
///
/// What is loaded with it stays: the address space for the thread about to resume
/// (`UserSpace::activate`). Anywhere else, a space loaded on a processor would be undone the
/// next time the scheduler resumed the thread, or would carry a system call back to ring 3 on
/// another process's tables (review finding 2); so nothing but the scheduler can do it, and the
/// proofs use a visit that puts back what was there (`UserSpace::probe_read`).
#[derive(Debug)]
pub struct Switch {
    _here: PhantomData<*const ()>,
}

impl Switch {
    /// For the trap entry only (which exists on the kernel's target only).
    #[cfg(target_os = "none")]
    pub(crate) const fn new() -> Self {
        Self { _here: PhantomData }
    }
}

/// What a system call's upcall asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyscallOutcome {
    /// Return `status` to the caller.
    Status(i64),
    /// The calling thread ends.
    ThreadExit,
}

/// One exception to report: what the trap entry knows about it. A report is printed by the
/// console above; the entry decides afterwards whether to resume (a breakpoint) or stop.
#[derive(Clone, Copy)]
pub struct ExceptionReport<'a> {
    pub vector: u8,
    pub error_code: u64,
    pub rip: u64,
    pub rsp: u64,
    pub rflags: u64,
    pub cs: u64,
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    /// Why a machine check cannot be recovered from, if this is one.
    pub machine_check: Option<&'a dyn core::fmt::Display>,
}

impl core::fmt::Debug for ExceptionReport<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ExceptionReport")
            .field("vector", &self.vector)
            .field("error_code", &self.error_code)
            .field("rip", &self.rip)
            .finish_non_exhaustive()
    }
}

/// The layers above's trap entry points. Each runs with interrupts disabled.
#[derive(Clone, Copy, Debug)]
pub struct Hooks {
    /// The timer interrupted the context at this frame; answers the frame to resume.
    pub tick: fn(u64, &Switch) -> u64,
    /// A device interrupt woke someone, or another processor rescheduled this one.
    pub reschedule: fn(u64, &Switch) -> u64,
    /// The running context gives up the processor.
    pub yield_now: fn(u64, &Switch) -> u64,
    /// The running context sleeps for this many timer ticks.
    pub sleep: fn(u64, u64, &Switch) -> u64,
    /// The running context blocks (for this many ticks at most; 0 is no limit).
    pub block: fn(u64, u64, &Switch) -> u64,
    /// The running context ends.
    pub exit: fn(u64, &Switch) -> u64,
    /// A system call: the number, the six arguments, and its return address.
    pub syscall: fn(u64, [u64; 6], u64) -> SyscallOutcome,
    /// A ring-3 exception ended the running thread: vector, error code, faulting address (0
    /// but for a page fault) and whether it arrived on its own stack (#DB).
    pub user_fault: fn(u64, u64, u64, bool),
    /// Print one exception report.
    pub report_exception: fn(&ExceptionReport<'_>),
}

static HOOKS: SetOnce<Hooks> = SetOnce::new();

/// Installs the upcalls. Only the first call has effect; the kernel's entry makes it before the
/// first exception can arrive.
pub fn install(hooks: Hooks) {
    let _ = HOOKS.set(hooks);
}

/// The installed upcalls, if any yet.
pub fn hooks() -> Option<&'static Hooks> {
    HOOKS.get()
}
