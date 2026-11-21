//! Upcalls from the trap entry into the layers above it (ADR-0011 N6): the scheduler, the
//! system-call layer, the console that reports a fatal exception.
//!
//! The trap entry lives in the framework and may not name those layers (dependencies run one
//! way), so they install their entry points once at boot, before interrupts can arrive. A
//! *frame* here is the address of a thread's saved register frame on its kernel stack: the trap
//! entry passes the interrupted one, and the scheduler hook answers which to resume.
#![forbid(unsafe_code)]

use core::marker::PhantomData;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::once::SetOnce;
use crate::told::Shown;
#[cfg(not(target_os = "none"))]
use origin_paging::layout::Told;

/// The scheduler's switch point: the moment, inside the trap entry with interrupts off, when
/// it chooses what this processor runs next. Only the trap entry makes one, for the scheduling
/// hooks, and only for the length of the call (it is not `Send`, and the hooks get a borrow).
/// The trap entry also takes its witness, [`Switch::here`], for the code it runs itself and for
/// the device handlers (ADR-0020 S3; TASKS item 93): `Switch::new` is its one caller.
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

    /// The witness of this switch point (ADR-0020 S3): the trap entry runs with interrupts off,
    /// and a scheduling hook chooses what this processor runs next without leaving the processor
    /// itself, so nothing that holds this borrow can be moved before the hook returns.
    pub fn here(&self) -> Here<'_> {
        Here { _here: PhantomData }
    }
}

/// A witness that the code holding it runs on one processor, and cannot leave it while the witness
/// lives (ADR-0020 S3; TASKS item 93). The processor index is read only under one
/// (`cpu::this_cpu`), and this processor's entry of a per-processor table is lent only for its
/// life (`PerCpu::this`).
///
/// **Who makes one.** Three makers, each in this module, and nothing else (TASKS item 93, the
/// coordinating session's ruling on `cba7462b`):
/// - An interrupts-off section, `section` (which `cpu::without_interrupts_here` forwards to). Its
///   closure takes the witness for any lifetime it is given, so nothing bound to that lifetime can
///   be returned from it.
/// - The trap entry, through its [`Switch`]: [`Switch::here`], for the scheduling hooks, the
///   device handlers and its own checks.
/// - [`Pinned::here`].
///
/// Its field and its constructor are private to this module, so no other module, in this crate or
/// another, can make one. It is zero-sized and `Copy`, and neither `Send` nor `Sync`, so it never
/// reaches another thread.
///
/// **What a section's or the trap entry's witness rests on.** It holds only while nothing its
/// holder runs switches the holder out. Code holding one never enables interrupts, opens an
/// interrupt window, idles, yields, sleeps, blocks or exits. A software interrupt is not masked by
/// the interrupt flag, so a yield inside a section would still switch the thread out. Code whose
/// body may be switched out takes a [`Pinned`] instead (the coordinating session's ruling Q2).
///
/// The witness is valid inside its section:
///
/// ```
/// let read = origin_frame::trap::host_section(|here| {
///     let _ = here;
///     1
/// });
/// assert_eq!(read, 1);
/// ```
///
/// It cannot be returned from it:
///
/// ```compile_fail
/// let escaped = origin_frame::trap::host_section(|here| here);
/// ```
///
/// Nor sent to another thread:
///
/// ```compile_fail,E0277
/// fn send<T: Send>(_: T) {}
/// origin_frame::trap::host_section(|here| send(here));
/// ```
#[derive(Clone, Copy)]
pub struct Here<'a> {
    _here: PhantomData<(*const (), &'a ())>,
}

// Never an address or an index: there is nothing in it to show.
impl core::fmt::Debug for Here<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Here")
    }
}

impl Here<'_> {
    /// For [`section`] only (the kernel's target only, where it exists). Private: a call from any
    /// other module does not build.
    #[cfg(target_os = "none")]
    const fn new() -> Self {
        Self { _here: PhantomData }
    }
}

/// A witness that the code holding it stays on its processor while the witness lives, **even if it
/// is switched out meanwhile** (ADR-0020 S2; TASKS item 93, the coordinating session's ruling Q2).
///
/// Two things give one:
/// - **a pin the running thread holds** (`sched::pin`), turned into a witness only through the
///   [`PinMint`];
/// - **the boot or idle flow** (`flow`, which `cpu::flow` forwards to), only once its check has
///   found the processor running its flow. A flow has no record, and never moves (S2, point 4).
///
/// Its field and its constructor are private to this module, so no other module, in this crate or
/// another, can make one any other way (TASKS item 93, the coordinating session's ruling on
/// `cba7462b`).
///
/// It gives a [`Here`] for the same life. A function whose body may be switched out takes a
/// `Pinned`, never a `Here`.
///
/// It cannot outlive the pin it was made from:
///
/// ```compile_fail,E0505
/// struct Guard;
/// let mint = origin_frame::trap::host_mint();
/// let guard = Guard;
/// let pinned = mint.pinned(&guard);
/// drop(guard);
/// let _ = pinned.here();
/// ```
///
/// Nor be sent to another thread:
///
/// ```compile_fail,E0277
/// fn send<T: Send>(_: T) {}
/// struct Guard;
/// let mint = origin_frame::trap::host_mint();
/// let guard = Guard;
/// send(mint.pinned(&guard));
/// ```
#[derive(Clone, Copy)]
pub struct Pinned<'a> {
    _here: PhantomData<(*const (), &'a ())>,
}

// As `Here`'s.
impl core::fmt::Debug for Pinned<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Pinned")
    }
}

impl<'a> Pinned<'a> {
    /// For [`flow`] only, once its check has passed (the kernel's target only). Private: a call
    /// from any other module does not build.
    #[cfg(target_os = "none")]
    const fn new() -> Self {
        Self { _here: PhantomData }
    }

    /// The same witness, for what a section's witness reads.
    pub fn here(self) -> Here<'a> {
        Here { _here: PhantomData }
    }
}

/// The right to turn the running thread's pin into a [`Pinned`] (ADR-0020 S2 and S3; TASKS item
/// 93, the coordinating session's ruling Q3).
///
/// Exactly one is ever made:
/// - [`take_pin_mint`] hands it out once, to the scheduler, before any thread exists.
/// - The scheduler keeps it in a `static` of its own, a [`MintHolder`] that no other module names.
/// - A second take stops the kernel, and so does a pin asked for before the mint was kept.
///
/// So the only witnesses that survive a switch are the scheduler's pins and the flows'. The
/// scheduler raises the running thread's pin count before it asks for a witness, on every path.
#[derive(Debug)]
pub struct PinMint {
    _one: (),
}

impl PinMint {
    /// The witness of a pin that the caller raised on the running thread and holds as `pin`, for
    /// as long as `pin` is borrowed.
    pub fn pinned<'a, G>(&self, _pin: &'a G) -> Pinned<'a> {
        Pinned { _here: PhantomData }
    }

    /// The witness of a pin that the caller raised on the running thread and never lowers: an
    /// ending thread's (ADR-0020 S2, point 2).
    pub fn pinned_for_good(&self) -> Pinned<'static> {
        Pinned { _here: PhantomData }
    }
}

/// Whether the kernel's one [`PinMint`] has been handed out.
static PIN_MINT_TAKEN: AtomicBool = AtomicBool::new(false);

/// The kernel's one [`PinMint`]. The scheduler takes it, before any thread exists. A second call
/// stops the kernel: the mint is the scheduler's alone.
pub fn take_pin_mint() -> PinMint {
    take_mint_from(&PIN_MINT_TAKEN)
}

/// The mint `taken` says has not been handed out yet, or a stop.
fn take_mint_from(taken: &AtomicBool) -> PinMint {
    assert!(
        !taken.swap(true, Ordering::AcqRel),
        "the pin's mint was taken twice"
    );
    PinMint { _one: () }
}

/// Where the scheduler keeps the [`PinMint`]: one `static` of its own (the ruling's Q3).
#[derive(Debug)]
pub struct MintHolder(SetOnce<PinMint>);

impl MintHolder {
    /// A holder with no mint yet.
    pub const fn new() -> Self {
        Self(SetOnce::new())
    }

    /// Keeps `mint`. The kernel stops if this holder already holds one.
    pub fn keep(&self, mint: PinMint) {
        assert!(self.0.set(mint).is_ok(), "a second pin mint was kept");
    }

    /// The mint. The kernel stops if none was kept: a pin was asked for before the scheduler had
    /// taken the mint.
    pub fn mint(&self) -> &PinMint {
        match self.0.get() {
            Some(mint) => mint,
            None => panic!("a pin was asked for before the scheduler took the pin's mint"),
        }
    }
}

impl Default for MintHolder {
    fn default() -> Self {
        Self::new()
    }
}

/// Runs `f` with interrupts off on this processor, with the section's witness (ADR-0020 S3; TASKS
/// item 93). `f` reads the processor index, and this processor's entries of per-processor tables,
/// under it. The closure takes the witness for any lifetime it is given, so nothing bound to that
/// lifetime can leave it. `cpu::without_interrupts_here` forwards here.
///
/// `f` must not enable interrupts, open an interrupt window, idle, yield, sleep, block or exit.
/// Any of those would let the thread be switched out inside the section, where the witness says it
/// cannot be ([`Here`]).
#[cfg(target_os = "none")]
pub(crate) fn section<T>(f: impl for<'a> FnOnce(Here<'a>) -> T) -> T {
    crate::cpu::without_interrupts(|| f(Here::new()))
}

/// The boot or idle flow's witness (ADR-0020 S2, point 4; TASKS item 93, the coordinating
/// session's rulings Q6 and on `cba7462b`): `Some` if the context running on this processor is the
/// processor's boot or idle flow, `None` on a thread. `cpu::flow` forwards here, and this is the
/// only place a flow's [`Pinned`] is made.
///
/// A flow has no record and never moves, and it never becomes a thread. So the witness holds for
/// the flow's whole life. Each flow takes it once, at its entry, and passes it down.
///
/// It is the flow if:
/// - the descriptor tables are not loaded yet, so nothing but the boot flow has run;
/// - this processor has no table of contexts, so no thread can have been switched in;
/// - or its table names no running home (no switch yet) or the flow's own ([`runs_a_flow`]).
///
/// It is read in a [`section`], since only this processor's switches write the running slot.
#[cfg(target_os = "none")]
pub(crate) fn flow() -> Option<Pinned<'static>> {
    let runs = section(|here| {
        !crate::cpu::tables_ready()
            || crate::cpu::CONTEXTS
                .this(crate::cpu::this_cpu(here))
                .is_none_or(|table| runs_a_flow(table.running_home()))
    });
    runs.then(Pinned::new)
}

/// Whether a processor whose table of contexts names `running_home` as its running context's home
/// runs its boot or idle flow ([`flow`]; TASKS item 93, the coordinating session's ruling Q6):
/// - `None`: no switch yet, so the flow is the only context that has run;
/// - the flow's own home, `BOOT_FLOW`.
///
/// A thread's home is refused.
#[cfg(any(test, target_os = "none"))]
pub(crate) fn runs_a_flow(running_home: Option<u64>) -> bool {
    running_home.is_none_or(|home| home == crate::sync::contexts::BOOT_FLOW)
}

/// For the host's tests and doctests only, where there is no processor index: runs `f` with a
/// [`Here`], as an interrupts-off section does on the kernel's target. It is absent from the
/// kernel's build, so no kernel code can make a witness this way.
#[cfg(not(target_os = "none"))]
pub fn host_section<T>(f: impl for<'a> FnOnce(Here<'a>) -> T) -> T {
    f(Here { _here: PhantomData })
}

/// For the host's tests and doctests only: a [`PinMint`] apart from the kernel's one. It is absent
/// from the kernel's build.
#[cfg(not(target_os = "none"))]
pub fn host_mint() -> PinMint {
    PinMint { _one: () }
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

    /// The mint is handed out once; a second take stops the kernel (the ruling's Q3).
    #[test]
    #[should_panic(expected = "the pin's mint was taken twice")]
    fn the_pin_mint_taken_twice_stops() {
        let taken = AtomicBool::new(false);
        let first = take_mint_from(&taken);
        let _ = first.pinned_for_good();
        let _second = take_mint_from(&taken);
    }

    /// A holder that never kept the mint stops the kernel when a pin asks for it: the scheduler
    /// had not taken it (the ruling's Q3, "not at all").
    #[test]
    #[should_panic(expected = "a pin was asked for before the scheduler took the pin's mint")]
    fn a_pin_without_the_mint_stops() {
        let holder = MintHolder::new();
        let _ = holder.mint();
    }

    /// A holder keeps one mint: a second stops the kernel.
    #[test]
    #[should_panic(expected = "a second pin mint was kept")]
    fn a_second_mint_kept_stops() {
        let holder = MintHolder::new();
        holder.keep(host_mint());
        holder.keep(host_mint());
    }

    /// The flow check over a real table of contexts (the ruling's Q6): no running home, then a
    /// thread's home, which is refused, then the flow's own.
    #[test]
    fn only_a_flow_passes_the_flow_check() {
        use crate::sync::contexts::{BOOT_FLOW, Contexts};
        let table = Contexts::new();
        assert_eq!(table.running_home(), None);
        assert!(runs_a_flow(table.running_home()), "no switch yet");
        // A thread's first frame, inside a stack of the region's kind: its home is that frame.
        let thread = 0xffff_c000_0001_ff40;
        assert!(table.publish(thread));
        let boot_flow_frame = 0x9000;
        let on_nothing = |_| false;
        assert!(
            table
                .switch(boot_flow_frame, thread, false, on_nothing)
                .is_ok()
        );
        assert_eq!(table.running_home(), Some(thread));
        assert!(
            !runs_a_flow(table.running_home()),
            "a thread's home is refused"
        );
        assert!(
            table
                .switch(thread - 0x200, boot_flow_frame, false, on_nothing)
                .is_ok()
        );
        assert_eq!(table.running_home(), Some(BOOT_FLOW));
        assert!(runs_a_flow(table.running_home()), "the flow's own home");
    }

    /// A holder that kept the mint lends it, and the witnesses carry no state.
    #[test]
    fn a_kept_mint_is_lent() {
        let taken = AtomicBool::new(false);
        let holder = MintHolder::new();
        holder.keep(take_mint_from(&taken));
        let pin = ();
        let pinned = holder.mint().pinned(&pin);
        let _ = pinned.here();
        assert_eq!(core::mem::size_of::<Here<'_>>(), 0);
        assert_eq!(core::mem::size_of::<Pinned<'_>>(), 0);
        assert_eq!(format!("{:?} {pinned:?}", pinned.here()), "Here Pinned");
    }
}
