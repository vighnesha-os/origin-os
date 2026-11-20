//! Upcalls from the trap entry into the layers above it (ADR-0011 N6): the scheduler, the
//! system-call layer, the console that reports a fatal exception.
//!
//! The trap entry lives in the framework and may not name those layers (dependencies run one
//! way), so they install their entry points once at boot, before interrupts can arrive. A
//! *frame* here is the address of a thread's saved register frame on its kernel stack: the trap
//! entry passes the interrupted one, and the scheduler hook answers which to resume.
#![forbid(unsafe_code)]

use core::marker::PhantomData;

use crate::once::SetOnce;
use crate::told::Shown;
#[cfg(not(target_os = "none"))]
use origin_paging::layout::Told;

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

/// Shows the vector, the error code, and `rip` as a crash report prints an address (ADR-0004
/// Amendment 2, A6.2; TASKS item 76). On the kernel's target that is `image +0x…`, never the
/// address, which would give the image's base away. On the host, where there is no image and no
/// base, it is the address itself. The rest is left out: `rsp` and `rax` to `rdx` may hold
/// addresses too.
/// `error_code` is never one: it is the processor's error code, or the 0 the entry pushes for a
/// vector that has none.
impl core::fmt::Debug for ExceptionReport<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.fields(f, told)
    }
}

impl ExceptionReport<'_> {
    /// `Debug`'s fields, with `tell` telling `rip`.
    fn fields(
        &self,
        f: &mut core::fmt::Formatter<'_>,
        tell: impl Fn(u64) -> Shown,
    ) -> core::fmt::Result {
        f.debug_struct("ExceptionReport")
            .field("vector", &self.vector)
            .field("error_code", &self.error_code)
            .field("rip", &format_args!("{}", tell(self.rip)))
            .finish_non_exhaustive()
    }
}

/// How the kernel tells an address: against its live bases.
#[cfg(target_os = "none")]
fn told(addr: u64) -> Shown {
    crate::phys::told(addr)
}

/// On the host there is no kernel image, and no base to keep out of a report: the address itself.
#[cfg(not(target_os = "none"))]
fn told(addr: u64) -> Shown {
    Shown(Told::Address(addr))
}

/// The layers above's trap entry points. Each runs with interrupts disabled.
#[derive(Clone, Copy)]
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

/// Shows none of its upcalls: a function's `Debug` is its address, in the kernel's image
/// (ADR-0004 Amendment 2, A6.2; TASKS item 76).
impl core::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Hooks").finish_non_exhaustive()
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use origin_paging::layout::{Bases, DIRECT_MAP_WINDOW, KERNEL_WINDOW, stack_region_base, tell};

    /// Slid bases, as `told.rs`'s test makes them.
    fn bases() -> Bases {
        Bases {
            image: KERNEL_WINDOW + 0x60_0000,
            direct_map: DIRECT_MAP_WINDOW + (7 << 30),
            stack_region: stack_region_base(0x1234_5678),
        }
    }

    /// A report's `Debug` as the kernel's: told against `bases`, as `phys::told` tells against
    /// the live ones.
    struct AsKernel<'r>(&'r ExceptionReport<'r>, Bases);

    impl core::fmt::Debug for AsKernel<'_> {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            self.0.fields(f, |addr| Shown(tell(addr, &self.1)))
        }
    }

    /// Whether `text` holds `value` in hexadecimal, either case, or in decimal.
    fn holds(text: &str, value: u64) -> bool {
        [
            format!("{value:x}"),
            format!("{value:X}"),
            format!("{value}"),
        ]
        .iter()
        .any(|shown| text.contains(shown.as_str()))
    }

    /// `rip` is an image offset, as the report's own line prints it (ADR-0013 C47); no field
    /// that may hold an address is printed, and no base: every one of them holds an address in
    /// a sliding window here, and the machine check's text names the image's base.
    ///
    /// What this shows is that `fields` tells `rip` through the teller it is given. That the
    /// kernel's `Debug` gives it `phys::told` (the target's `told`) is not tested: no host test
    /// reaches a `target_os = "none"` function, and no boot line prints this `Debug` (the
    /// review's T76-80-2, a known limit).
    #[test]
    fn debug_tells_rip_as_an_image_offset_and_prints_no_address() {
        let b = bases();
        let machine_check = format!("bank 4 at {:#x}", b.image);
        let report = ExceptionReport {
            vector: 14,
            error_code: 2,
            rip: b.image + 0x1f20,
            rsp: b.stack_region + 0x1_fff8,
            rflags: 0x246,
            cs: 0x8,
            rax: b.direct_map + 0x7e5_0000,
            rbx: b.image + 0x40,
            rcx: b.stack_region + 0x10,
            rdx: KERNEL_WINDOW + 0x1234_5678,
            machine_check: Some(&machine_check),
        };
        let text = format!("{:?}", AsKernel(&report, b));
        assert_eq!(
            text,
            "ExceptionReport { vector: 14, error_code: 2, rip: image +0x1f20, .. }"
        );
        let r = &report;
        for value in [
            r.rip,
            r.rsp,
            r.rax,
            r.rbx,
            r.rcx,
            r.rdx,
            b.image,
            b.stack_region,
            b.direct_map,
        ] {
            assert!(!holds(&text, value), "{text} holds {value:#x}");
        }
    }

    /// On the host the same fields, told against no base: `Debug` goes through `fields`.
    #[test]
    fn on_the_host_debug_has_no_base_to_tell_against() {
        let report = ExceptionReport {
            vector: 3,
            error_code: 0,
            rip: 0x40_1000,
            rsp: 0,
            rflags: 0,
            cs: 0,
            rax: 0,
            rbx: 0,
            rcx: 0,
            rdx: 0,
            machine_check: None,
        };
        assert_eq!(
            format!("{report:?}"),
            "ExceptionReport { vector: 3, error_code: 0, rip: 0x401000, .. }"
        );
    }

    fn frame_hook(_: u64, _: &Switch) -> u64 {
        0
    }
    fn timed_hook(_: u64, _: u64, _: &Switch) -> u64 {
        0
    }
    fn syscall_hook(_: u64, _: [u64; 6], _: u64) -> SyscallOutcome {
        SyscallOutcome::ThreadExit
    }
    fn fault_hook(_: u64, _: u64, _: u64, _: bool) {}
    fn report_hook(_: &ExceptionReport<'_>) {}

    /// The upcalls' `Debug` names none of them: a function's own `Debug` is its address.
    #[test]
    fn hooks_debug_prints_no_upcall_address() {
        let hooks = Hooks {
            tick: frame_hook,
            reschedule: frame_hook,
            yield_now: frame_hook,
            sleep: timed_hook,
            block: timed_hook,
            exit: frame_hook,
            syscall: syscall_hook,
            user_fault: fault_hook,
            report_exception: report_hook,
        };
        let text = format!("{hooks:?}");
        assert_eq!(text, "Hooks { .. }");
        let addresses = [
            (hooks.tick as *const ()).addr(),
            (hooks.sleep as *const ()).addr(),
            (hooks.syscall as *const ()).addr(),
            (hooks.user_fault as *const ()).addr(),
            (hooks.report_exception as *const ()).addr(),
        ];
        for addr in addresses {
            assert!(!holds(&text, addr as u64), "{text} holds {addr:#x}");
        }
        assert!(!text.contains("0x"), "{text}");
    }
}
