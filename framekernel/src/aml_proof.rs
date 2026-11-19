//! The boot proof of the AML interpreter (ADR-0019 A3; TASKS item 63, step B3; the "AML
//! interpreter" proof, ADR-0013).
//!
//! **QEMU's tables only.** ADR-0019 A3 gives this proof "QEMU's own DSDT and SSDTs", and A2 gives
//! every other platform's AML to the user-mode ACPI service. So the proof first reads the DSDT's
//! header, checked by `origin-acpi-tables`, and runs only if its OEM ID and OEM table ID are
//! QEMU's ([`QEMU_DSDT`]). On any other firmware it prints one line, with the IDs escaped
//! ([`Escaped`]), runs no AML and passes, as ADR-0013 R6 counts a proof whose subject is absent
//! (the coordinating session's ruling on the review of B3, B3-2).
//!
//! On QEMU's tables, the DSDT, then every SSDT and PSDT the XSDT lists, in its order and each
//! address once, are loaded into `origin-aml` against [`BootHost`]: a host that refuses every
//! operation-region access, every bridge's bus number and every table, grants the Global Lock
//! without touching the FACS, keeps the monotonic clock, and returns at once from every wait and
//! report. So AML run at boot reads and writes nothing. Each table's header, length and checksum
//! are checked by `origin-acpi-tables` first, as the census of the tables checks them
//! (`acpi_census`); a table that fails is not loaded, and the proof fails.
//!
//! Then the proof reads named results QEMU's tables give without hardware, walks the namespace's
//! `_STA` and `_INI` ([`origin_aml::Interpreter::initialize`]), and checks that every device that
//! failed failed by a refusal of the kinds QEMU's AML must cause: an access to SystemMemory,
//! SystemIO or PCI_Config refused, or a bridge's bus number refused. The refusals QEMU's tables
//! must cause are required: the HPET's `_STA` in SystemMemory (where an HPET is declared), each
//! processor's in SystemIO, each PCI interrupt link's (`\_SB.LNKx`) in PCI_Config. Counts are
//! printed, not judged: they follow the machine's processors. Which devices are processors is read
//! without running anything ([`origin_aml::Interpreter::plain`]): a `_HID` that is not a string
//! after its aliases (a method, a field unit, a buffer field, a package) is not read.
//!
//! **Bounded as a whole.** Each evaluation is bounded by the limits, and the walk as a whole. The
//! proof as a whole is bounded too, at [`WHOLE`] times one evaluation's time and steps, checked
//! before each XSDT entry, each named result, the walk and each classification (the review of B3,
//! B3-1). So it ends within that bound and one more evaluation, and cannot hold the boot longer.
//!
//! The verdict rests on what the host answered, not on what it says of itself: [`Recorder`]
//! wraps whatever host the proof is given and counts every answer, so a host that granted
//! anything fails the proof. Last, the interpreter is dropped, and the kernel heap must hold
//! what it held before the proof, to the byte. That check holds because nothing else allocates
//! while the proof runs: it runs before the first thread, with the other processors idle and
//! interrupt handlers allocating nothing. Moved after `sched::prove`, it would not hold.
//!
//! **The Global Lock** is granted without the FACS for this proof only: it is the only one asking
//! at boot, before the ACPI event model (ADR-0019 A6, area C) exists, and the lock's real path is
//! that area's. The proof requires as many releases as grants, and the host touches no firmware
//! memory itself.
//!
//! **The stack.** At boot the proof runs on the loader's stack of 64 KiB, with an unmapped guard
//! page below it that the page-protection proof shows faulting, so an overflow faults rather
//! than corrupts. The interpreter keeps its frames on the heap; its Rust depth is fixed by its
//! code (the index-field chain recurses, to a depth the chain's limit fixes). The proof reports
//! how far below its own entry the deepest host call was made, and how far above the guard page:
//! samples taken at host calls, so the depth is a lower bound on what the proof used and the room
//! an upper bound on what was left, not measurements (painting the stack is TASKS item 68's).
//!
//! Nothing the kernel does depends on the verdict (Report, ADR-0013). `Load` and `LoadTable` are
//! not run (step B4), and nothing grants a region.

use alloc::vec::Vec;
use core::fmt::{self, Write};

use origin_acpi_tables::definition::{DefinitionBlock as Checked, Kind};
use origin_acpi_tables::fadt::Fadt;
use origin_acpi_tables::header::{self, HEADER_LEN, Header};
use origin_aml::{
    BridgeBus, BufferAccess, Config, DefinitionBlock, Error, ErrorKind, Host, HostError,
    InitOutcome, IntegerWidth, Interpreter, Limit, Limits, Node, Plain, RegionAccess, Report,
    Space, TableKey, Trace, Value,
};

use crate::acpi_census::Firmware;
use crate::acpi_text::{Ascii, Refused, Sig};

/// Where every line of the proof starts.
pub const LINE: &str = "origin-kernel: aml: ";

/// The object type of a processor (ACPI 6.6 Table 19.36).
const PROCESSOR: u64 = 12;

/// A processor device's `_HID` (ACPI 6.6 §8.4).
const PROCESSOR_HID: &[u8] = b"ACPI0007";

/// `PNP0A08` as an EISA ID: a PCI Express root bridge.
const PCIE_ROOT_BRIDGE: u64 = 0x080A_D041;

/// The kernel's firmware memory.
#[cfg(target_os = "none")]
struct Boot;

#[cfg(target_os = "none")]
impl Firmware for Boot {
    fn lend(&mut self, addr: u64, len: usize) -> Result<&'static [u8], &'static str> {
        crate::firmware::bytes(addr, len)
    }
    fn copy(&mut self, addr: u64, out: &mut [u8]) -> Result<(), &'static str> {
        crate::firmware::copy(addr, out)
    }
    fn now(&self) -> u64 {
        crate::time::monotonic_nanos()
    }
}

/// The proof at boot, over the kernel's firmware memory and heap, with the boot's limits;
/// `stack_floor` is the lowest address of the stack it runs on, the loader's, above its guard
/// page.
#[cfg(target_os = "none")]
pub(crate) fn prove(com1: &mut crate::console::Console, rsdp_addr: u64, stack_floor: u64) -> bool {
    let mut heap = || crate::memory::stats().map(|(h, _)| h.bytes_in_use);
    run(
        com1,
        &mut Boot,
        rsdp_addr,
        BootHost::new(crate::time::monotonic_nanos),
        Limits::BOOT_PROOF,
        &mut heap,
        usize::try_from(stack_floor).ok(),
    )
    .passed()
}

/// The boot proof's host (the contract of step B3, §4): everything that reaches hardware or a
/// table is refused, the Global Lock is granted, and nothing waits.
#[derive(Clone, Copy, Debug)]
pub struct BootHost {
    /// The monotonic clock, in nanoseconds.
    clock: fn() -> u64,
}

impl BootHost {
    /// A host on the monotonic clock `clock_nanos`.
    #[must_use]
    pub const fn new(clock_nanos: fn() -> u64) -> Self {
        Self { clock: clock_nanos }
    }
}

impl Host for BootHost {
    fn now_100ns(&mut self) -> u64 {
        (self.clock)() / 100
    }

    fn read(&mut self, _: &RegionAccess<'_>) -> Result<u64, HostError> {
        Err(HostError::Refused)
    }

    fn write(&mut self, _: &RegionAccess<'_>, _: u64) -> Result<(), HostError> {
        Err(HostError::Refused)
    }

    /// Refused: no `OperationRegion` names a bridge's bus number (`host.rs`), so every
    /// PCI_Config region below a bridge fails, by a refusal.
    fn bridge_bus(&mut self, _: &BridgeBus) -> Result<u8, HostError> {
        Err(HostError::Refused)
    }

    fn transfer(&mut self, _: &BufferAccess<'_>, _: &mut [u8]) -> Result<usize, HostError> {
        Err(HostError::Refused)
    }

    /// Returns at once: the library charged it against the time left before calling, and
    /// nothing at boot is waited for.
    fn sleep_ms(&mut self, _: u64) {}

    /// As [`BootHost::sleep_ms`].
    fn stall_us(&mut self, _: u8) {}

    /// Granted at once, without the FACS: see the module's documentation.
    fn acquire_global_lock(&mut self, _: u16) -> Result<bool, HostError> {
        Ok(true)
    }

    fn release_global_lock(&mut self) -> Result<(), HostError> {
        Ok(())
    }

    fn notify(&mut self, _: &[[u8; 4]], _: u64) {}

    fn fatal(&mut self, _: u8, _: u32, _: u64) {}

    fn debug(&mut self, _: &[u8]) {}

    /// Nothing to do: the [`Recorder`] around it counts every report for the verdict.
    fn report(&mut self, _: &Report<'_>) {}

    /// Refused: `DataTableRegion` and `LoadTable` get nothing at boot (the review of B2's
    /// Addendum 2: the host decides it knowingly).
    fn table(&mut self, _: &TableKey, _: &mut dyn FnMut(u64, &[u8])) -> Result<bool, HostError> {
        Err(HostError::Refused)
    }
}

/// What a host was asked and what it answered, as [`Recorder`] counted it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    /// Region accesses refused in SystemMemory, SystemIO and PCI_Config.
    pub refused: [u32; 3],
    /// Region accesses refused in any other space, and buffer transfers refused.
    pub refused_other: u32,
    /// Bridges' secondary bus numbers refused.
    pub bridges: u32,
    /// Tables refused.
    pub tables: u32,
    /// Requests for hardware or a table answered with anything but a refusal.
    pub answered: u32,
    /// Times the Global Lock was granted.
    pub lock_taken: u32,
    /// Times it was given back.
    pub lock_given_back: u32,
    /// Times it was refused or timed out, or could not be given back.
    pub lock_other: u32,
    /// `Sleep` and `Stall`.
    pub waits: u32,
    /// `Notify`, `Fatal`, `Debug` and `BreakPoint`.
    pub reports: u32,
    /// Tolerances the interpreter reported (TASKS item 63, step B5).
    pub tolerated: u32,
    /// The first of them, for the proof's line.
    pub first_tolerance: Option<Tolerance>,
    /// Stores the interpreter reported making in pieces ([`Report::Pieces`]): not tolerances,
    /// counted apart.
    pub pieces: u32,
    /// The lowest stack address a host call was made at.
    pub lowest: Option<usize>,
}

impl Seen {
    /// Every refusal of a request for hardware or a table.
    #[must_use]
    pub const fn refusals(&self) -> u32 {
        self.refused[0]
            .saturating_add(self.refused[1])
            .saturating_add(self.refused[2])
            .saturating_add(self.refused_other)
            .saturating_add(self.bridges)
            .saturating_add(self.tables)
    }

    /// Counts a region access that ended `refused` or not.
    fn region(&mut self, space: Space, refused: bool) {
        let slot = match space {
            Space::SYSTEM_MEMORY => self.refused.get_mut(0),
            Space::SYSTEM_IO => self.refused.get_mut(1),
            Space::PCI_CONFIG => self.refused.get_mut(2),
            _ => None,
        };
        let count = match (refused, slot) {
            (false, _) => &mut self.answered,
            (true, Some(slot)) => slot,
            (true, None) => &mut self.refused_other,
        };
        *count = count.saturating_add(1);
    }

    /// Notes the stack address the host is called at.
    fn note_stack(&mut self) {
        let here = stack_mark();
        self.lowest = Some(self.lowest.map_or(here, |l| l.min(here)));
    }
}

/// The address of a local of this function's frame: where the stack is, roughly, when it is
/// called.
#[inline(never)]
fn stack_mark() -> usize {
    let here = 0u8;
    core::ptr::from_ref(core::hint::black_box(&here)).addr()
}

/// A tolerance the interpreter reported, as the proof prints it (TASKS item 63, step B5): what,
/// where, and the error of an abandoned statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tolerance {
    /// The report's kind.
    pub what: &'static str,
    /// Where, if the report says.
    pub at: Option<origin_aml::At>,
    /// An abandoned statement's error.
    pub error: Option<ErrorKind>,
}

impl Tolerance {
    fn of(r: &Report<'_>) -> Self {
        let (what, at, error) = match *r {
            Report::Abandoned {
                error, statement, ..
            } => ("a statement abandoned", Some(statement), Some(error.kind)),
            Report::AlreadyLoaded { .. } => ("a table already loaded", None, None),
            Report::UnboundElement { at } => ("a package name bound to nothing", Some(at), None),
            Report::ExtraElements { at, .. } => ("a package with extra elements", Some(at), None),
            Report::Unterminated { at } => ("an unterminated string", Some(at), None),
            Report::HighBytes { at } => ("a string byte above 0x7F", Some(at), None),
            Report::LongStall { at, .. } => ("a Stall over 100 us", Some(at), None),
            Report::Released { .. } => ("a mutex held to the end", None, None),
            Report::Pieces { .. } => ("a store in pieces", None, None),
            Report::Suppressed { .. } => ("reports past the bound", None, None),
        };
        Self { what, at, error }
    }
}

impl fmt::Display for Tolerance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.what)?;
        if let Some(at) = self.at {
            write!(f, " at table {} offset {:#x}", at.table, at.offset)?;
        }
        if let Some(e) = self.error {
            write!(f, " ({e:?})")?;
        }
        Ok(())
    }
}

/// Counts every answer of the host it wraps, for the verdict (see the module's documentation).
#[derive(Debug)]
pub struct Recorder<H> {
    inner: H,
    seen: Seen,
}

impl<H: Host> Recorder<H> {
    /// Wraps `inner`.
    pub const fn new(inner: H) -> Self {
        Self {
            inner,
            seen: Seen {
                refused: [0; 3],
                refused_other: 0,
                bridges: 0,
                tables: 0,
                answered: 0,
                lock_taken: 0,
                lock_given_back: 0,
                lock_other: 0,
                waits: 0,
                reports: 0,
                tolerated: 0,
                first_tolerance: None,
                pieces: 0,
                lowest: None,
            },
        }
    }

    /// What it has counted.
    pub const fn seen(&self) -> &Seen {
        &self.seen
    }
}

/// Counts one answer in `count` if it is a refusal, in `answered` if not.
fn refusal<T>(r: &Result<T, HostError>, count: &mut u32, answered: &mut u32) {
    let c = if matches!(r, Err(HostError::Refused)) {
        count
    } else {
        answered
    };
    *c = c.saturating_add(1);
}

impl<H: Host> Host for Recorder<H> {
    fn now_100ns(&mut self) -> u64 {
        self.seen.note_stack();
        self.inner.now_100ns()
    }

    fn read(&mut self, access: &RegionAccess<'_>) -> Result<u64, HostError> {
        self.seen.note_stack();
        let r = self.inner.read(access);
        self.seen
            .region(access.space, matches!(r, Err(HostError::Refused)));
        r
    }

    fn write(&mut self, access: &RegionAccess<'_>, value: u64) -> Result<(), HostError> {
        self.seen.note_stack();
        let r = self.inner.write(access, value);
        self.seen
            .region(access.space, matches!(r, Err(HostError::Refused)));
        r
    }

    fn bridge_bus(&mut self, request: &BridgeBus) -> Result<u8, HostError> {
        self.seen.note_stack();
        let r = self.inner.bridge_bus(request);
        refusal(&r, &mut self.seen.bridges, &mut self.seen.answered);
        r
    }

    fn transfer(&mut self, access: &BufferAccess<'_>, data: &mut [u8]) -> Result<usize, HostError> {
        self.seen.note_stack();
        let r = self.inner.transfer(access, data);
        refusal(&r, &mut self.seen.refused_other, &mut self.seen.answered);
        r
    }

    fn sleep_ms(&mut self, ms: u64) {
        self.seen.note_stack();
        self.seen.waits = self.seen.waits.saturating_add(1);
        self.inner.sleep_ms(ms);
    }

    fn stall_us(&mut self, us: u8) {
        self.seen.note_stack();
        self.seen.waits = self.seen.waits.saturating_add(1);
        self.inner.stall_us(us);
    }

    fn acquire_global_lock(&mut self, timeout_ms: u16) -> Result<bool, HostError> {
        self.seen.note_stack();
        let r = self.inner.acquire_global_lock(timeout_ms);
        let count = if r == Ok(true) {
            &mut self.seen.lock_taken
        } else {
            &mut self.seen.lock_other
        };
        *count = count.saturating_add(1);
        r
    }

    fn release_global_lock(&mut self) -> Result<(), HostError> {
        self.seen.note_stack();
        let r = self.inner.release_global_lock();
        let count = if r.is_ok() {
            &mut self.seen.lock_given_back
        } else {
            &mut self.seen.lock_other
        };
        *count = count.saturating_add(1);
        r
    }

    fn notify(&mut self, node: &[[u8; 4]], value: u64) {
        self.seen.note_stack();
        self.seen.reports = self.seen.reports.saturating_add(1);
        self.inner.notify(node, value);
    }

    fn fatal(&mut self, kind: u8, code: u32, arg: u64) {
        self.seen.note_stack();
        self.seen.reports = self.seen.reports.saturating_add(1);
        self.inner.fatal(kind, code, arg);
    }

    fn debug(&mut self, text: &[u8]) {
        self.seen.note_stack();
        self.seen.reports = self.seen.reports.saturating_add(1);
        self.inner.debug(text);
    }

    fn breakpoint(&mut self) {
        self.seen.note_stack();
        self.seen.reports = self.seen.reports.saturating_add(1);
        self.inner.breakpoint();
    }

    fn report(&mut self, report: &Report<'_>) {
        self.seen.note_stack();
        if matches!(report, Report::Pieces { .. }) {
            self.seen.pieces = self.seen.pieces.saturating_add(1);
        } else {
            self.seen.tolerated = self.seen.tolerated.saturating_add(1);
            if self.seen.first_tolerance.is_none() {
                self.seen.first_tolerance = Some(Tolerance::of(report));
            }
        }
        self.inner.report(report);
    }

    fn tracing(&self) -> bool {
        self.inner.tracing()
    }

    fn trace(&mut self, event: &Trace<'_>) {
        self.inner.trace(event);
    }

    fn table(
        &mut self,
        key: &TableKey,
        out: &mut dyn FnMut(u64, &[u8]),
    ) -> Result<bool, HostError> {
        self.seen.note_stack();
        let r = self.inner.table(key, out);
        refusal(&r, &mut self.seen.tables, &mut self.seen.answered);
        r
    }
}

/// What the walk reached, by outcome, and the refusals QEMU's tables must cause.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Walked {
    /// Devices and processors reported (and `\_SB` if it has an `_INI`).
    pub devices: u32,
    /// Present, and `_INI` ran or there was none.
    pub initialized: u32,
    /// Not present but functioning.
    pub children_only: u32,
    /// Neither.
    pub absent: u32,
    /// `_STA` refused, as the proof allows.
    pub sta_refused: u32,
    /// `_INI` refused, as the proof allows.
    pub ini_refused: u32,
    /// Failed otherwise.
    pub failed: u32,
    /// The HPET's `_STA` was refused in SystemMemory; `None` where no HPET is declared.
    pub hpet: Option<bool>,
    /// Processors reached.
    pub processors: u32,
    /// Of them, those whose `_STA` was refused in SystemIO.
    pub processors_refused: u32,
    /// PCI interrupt links (`\_SB.LNKx`) reached.
    pub links: u32,
    /// Of them, those whose `_STA` was refused in PCI_Config.
    pub links_refused: u32,
}

impl Walked {
    /// Every processor's `_STA` refused in SystemIO, and there was one.
    #[must_use]
    pub const fn processors_as_required(&self) -> bool {
        self.processors > 0 && self.processors_refused == self.processors
    }

    /// Every link's `_STA` refused in PCI_Config, and there was one.
    #[must_use]
    pub const fn links_as_required(&self) -> bool {
        self.links > 0 && self.links_refused == self.links
    }
}

/// What the proof found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Findings {
    /// The DSDT is not QEMU's, so the proof ran nothing and passed (ADR-0013 R6's convention for
    /// an absent subject; the review of B3, B3-2).
    pub not_run: bool,
    /// Definition blocks loaded.
    pub tables: u32,
    /// Bytes of AML in them.
    pub aml_bytes: u64,
    /// Nanoseconds their loads took.
    pub load_ns: u64,
    /// Every named result as expected.
    pub named: bool,
    /// The walk.
    pub walked: Walked,
    /// Nanoseconds the walk took.
    pub walk_ns: u64,
    /// What the host was asked and answered.
    pub seen: Seen,
    /// The most bytes the interpreter held.
    pub peak: usize,
    /// Nodes in the namespace at the end.
    pub nodes: usize,
    /// How far below the proof's entry the deepest host call was made, in bytes.
    pub depth: usize,
    /// How far above the stack's lowest byte (above its guard page) that call was made, where the
    /// caller knows the stack.
    pub room: Option<usize>,
    /// The kernel heap held what it held before.
    pub heap_back: bool,
    /// Why the proof failed: the first reason found, or `None` if it passed.
    pub failed: Option<&'static str>,
}

impl Findings {
    /// The verdict.
    #[must_use]
    pub const fn passed(&self) -> bool {
        self.failed.is_none()
    }

    /// Records `why` the proof failed, if it is the first reason.
    fn fail(&mut self, why: &'static str) {
        if self.failed.is_none() {
            self.failed = Some(why);
        }
    }
}

/// QEMU's DSDT, as its header names it: the OEM ID and OEM table ID that every machine `boot-test`
/// and `boot-matrix` boot showed in the proof's first boots (QEMU 11.1.3 with OVMF, 2026-11-14:
/// `aml: DSDT rev 1, ... OEM "BOCHS ", "BXPC    "`, the discovery machine's 496,594-byte DSDT
/// included; TASKS item 63's B3 paragraph). The proof runs only on these tables: ADR-0019 A3 and
/// decision item 303 give it QEMU's own DSDT and SSDTs, and A2 gives every other platform's AML to
/// the user-mode service (the coordinating session's ruling on the review of B3, B3-2).
pub const QEMU_DSDT: ([u8; 6], [u8; 8]) = (*b"BOCHS ", *b"BXPC    ");

/// How many times one evaluation's time and steps the whole proof may take (the review of B3,
/// B3-1). The bound is checked before each XSDT entry, each named result, the walk and each
/// classification, so the proof ends within it and one more evaluation, which the limits bound on
/// its own (the walk as a whole): with the boot's limits, 3 s and 30,000,000 steps, then at most
/// one more second and 10,000,000 steps.
pub const WHOLE: u64 = 3;

/// The proof's bound over everything it runs, from [`run`]'s start.
#[derive(Clone, Copy, Debug)]
struct Bound {
    started_ns: u64,
    time_ns: u64,
    steps: u64,
}

impl Bound {
    fn of(limits: &Limits, now_ns: u64) -> Self {
        Self {
            started_ns: now_ns,
            time_ns: limits.time_100ns.saturating_mul(100).saturating_mul(WHOLE),
            steps: limits.steps.saturating_mul(WHOLE),
        }
    }

    /// Whether the proof has passed its bound at `now_ns`, `steps` counted so far.
    const fn passed(&self, now_ns: u64, steps: u64) -> bool {
        now_ns.saturating_sub(self.started_ns) > self.time_ns || steps > self.steps
    }
}

/// Whether the proof has passed its bound before `what`; if so, it says so and fails, once.
fn cut_off(
    out: &mut impl Write,
    fw: &impl Firmware,
    bound: &Bound,
    steps: u64,
    what: &str,
    found: &mut Findings,
) -> bool {
    if !bound.passed(fw.now(), steps) {
        return false;
    }
    if found.failed != Some(CUT_OFF) {
        let _ = writeln!(
            out,
            "{LINE}cut off by the proof's bound ({} us, {} steps) before {what}",
            bound.time_ns / 1000,
            bound.steps
        );
    }
    found.fail(CUT_OFF);
    true
}

/// The reason a proof past its bound gives.
const CUT_OFF: &str = "the proof was cut off by its bound";

/// The reason a proof gives when the interpreter reported a tolerance on QEMU's own tables (TASKS
/// item 63, step B5).
const TOLERATED: &str = "a tolerance was reported on QEMU's own tables";

/// The reason a proof gives when the interpreter stored data in pieces on QEMU's own tables, where
/// ACPICA would cut it (TASKS item 63, step B5's point 1).
const IN_PIECES: &str = "a store in pieces was reported on QEMU's own tables";

/// The reason a proof gives when the DSDT's checked read is not the one whose IDs were judged
/// (the review of B4, B4-9).
const CHANGED: &str = "the DSDT changed between two reads";

/// The proof: the definition blocks from the RSDP at `rsdp_addr` through `fw`, loaded and run
/// against `host` (wrapped in a [`Recorder`]) under `limits`, if they are QEMU's; `heap` reads the
/// kernel heap's bytes in use, before the proof and after the interpreter is dropped;
/// `stack_floor`, where the caller knows it, is the lowest address of the stack the proof runs on,
/// just above its guard page. Prints a line for each table, the named results, the walk, the
/// host's answers and the verdict; on tables that are not QEMU's, one line, and runs nothing.
pub fn run<H: Host>(
    out: &mut impl Write,
    fw: &mut impl Firmware,
    rsdp_addr: u64,
    host: H,
    limits: Limits,
    heap: &mut dyn FnMut() -> Option<u64>,
    stack_floor: Option<usize>,
) -> Findings {
    let entry = stack_mark();
    let before = heap();
    let mut found = Findings::default();
    interpret(out, fw, rsdp_addr, host, limits, &mut found);
    if found.not_run {
        return found;
    }
    found.depth = found.seen.lowest.map_or(0, |l| entry.saturating_sub(l));
    found.room = found
        .seen
        .lowest
        .zip(stack_floor)
        .map(|(l, floor)| l.saturating_sub(floor));
    let after = heap();
    found.heap_back = before.is_some() && before == after;
    if !found.heap_back {
        found.fail("the kernel heap did not return to what it held before the proof");
    }
    let seen = &found.seen;
    let _ = write!(
        out,
        "{LINE}the host: refused SystemMemory {}, SystemIO {}, PCI_Config {}, other spaces {}, bridges {}, tables {}; answered otherwise {}; Global Lock taken {}, given back {}, refused or failed {}; waits {}, reports {}; the deepest host call {} bytes below the proof's entry",
        seen.refused[0],
        seen.refused[1],
        seen.refused[2],
        seen.refused_other,
        seen.bridges,
        seen.tables,
        seen.answered,
        seen.lock_taken,
        seen.lock_given_back,
        seen.lock_other,
        seen.waits,
        seen.reports,
        found.depth,
    );
    if let Some(room) = found.room {
        let _ = write!(out, " and {room} bytes above the stack's guard page");
    }
    let _ = writeln!(
        out,
        " (sampled at host calls: the depth a lower bound, the room an upper one); the interpreter's peak {} bytes of {}, {} nodes",
        found.peak, limits.memory_bytes, found.nodes,
    );
    let _ = writeln!(
        out,
        "{LINE}{} tables, {} bytes of AML loaded in {} us; {} devices walked in {} us, {} refusals (SystemMemory {}, SystemIO {}, PCI_Config {}, bridges {}); named results {}; heap back {}; {}",
        found.tables,
        found.aml_bytes,
        found.load_ns / 1000,
        found.walked.devices,
        found.walk_ns / 1000,
        seen.refusals(),
        seen.refused[0],
        seen.refused[1],
        seen.refused[2],
        seen.bridges,
        found.named,
        found.heap_back,
        Verdict(found.failed),
    );
    found
}

/// The verdict's last words: `passed`, or `aml failed: ` and the first reason.
struct Verdict(Option<&'static str>);

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            None => f.write_str("passed"),
            Some(why) => write!(f, "aml failed: {why}"),
        }
    }
}

/// Bytes a table gives, printed so that they cannot forge a line or a marker: printable ASCII as
/// it is, but `"` and `\`, and every other byte, as `\xNN` (the review of B3, B3-2).
#[derive(Clone, Copy, Debug)]
pub struct Escaped<'a>(pub &'a [u8]);

impl fmt::Display for Escaped<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in self.0 {
            if (0x20..=0x7E).contains(&b) && b != b'"' && b != b'\\' {
                f.write_char(char::from(b))?;
            } else {
                write!(f, "\\x{b:02x}")?;
            }
        }
        Ok(())
    }
}

/// Lends the table at `addr` if the header read by copy names one of `want`: `Ok(None)` if it
/// names another (which is not lent: it may be a structure the firmware changes after boot).
fn lend_if(
    fw: &mut impl Firmware,
    addr: u64,
    want: &[&[u8; 4]],
) -> Result<Option<(&'static [u8], Header)>, &'static str> {
    let mut first = [0u8; HEADER_LEN];
    fw.copy(addr, &mut first)?;
    let header = Header::read(&first).map_err(|_| "a short header")?;
    if !want.iter().any(|w| header.signature.is(w)) {
        return Ok(None);
    }
    if header.length > crate::acpi::MAX_TABLE_LENGTH {
        return Err("longer than the kernel maps a table");
    }
    let length = usize::try_from(header.length)
        .unwrap_or(HEADER_LEN)
        .max(HEADER_LEN);
    Ok(Some((fw.lend(addr, length)?, header)))
}

/// Why [`find_dsdt`] found no DSDT.
enum NoDsdt {
    /// The proof's bound passed while the XSDT was searched for a FADT: the proof fails with the
    /// bound's own reason (the review of B3's addendum, B3-14).
    Cut,
    /// Anything else, said.
    Because(&'static str),
}

impl From<&'static str> for NoDsdt {
    fn from(why: &'static str) -> Self {
        Self::Because(why)
    }
}

/// The DSDT's address, from the first FADT the XSDT at `rsdp_addr` lists, and the XSDT. The
/// search stops at the proof's time bound.
fn find_dsdt(
    fw: &mut impl Firmware,
    rsdp_addr: u64,
    bound: &Bound,
) -> Result<(&'static [u8], u64), NoDsdt> {
    let rsdp = fw.lend(rsdp_addr, crate::acpi::RSDP_LENGTH)?;
    let rsdp = crate::acpi::parse_rsdp(rsdp).map_err(|_| "the RSDP is invalid")?;
    let (xsdt, _) = lend_if(fw, rsdp.xsdt, &[b"XSDT"])?.ok_or("the XSDT is not one")?;
    header::check_signature(xsdt, b"XSDT").map_err(|_| "the XSDT is refused")?;
    for addr in crate::acpi::xsdt_entries(xsdt).filter(|&a| a != 0) {
        if bound.passed(fw.now(), 0) {
            return Err(NoDsdt::Cut);
        }
        if let Ok(Some((fadt, _))) = lend_if(fw, addr, &[b"FACP"]) {
            let fadt = Fadt::parse(fadt).map_err(|_| "the FADT is refused")?;
            return Ok((xsdt, fadt.dsdt_address().ok_or("the FADT names no DSDT")?));
        }
    }
    Err(NoDsdt::Because("the XSDT lists no FADT"))
}

/// Loads the tables, checks the named results and walks the namespace, recording what was found
/// in `found`, if the DSDT is QEMU's. The interpreter is dropped before it returns.
fn interpret<H: Host>(
    out: &mut impl Write,
    fw: &mut impl Firmware,
    rsdp_addr: u64,
    host: H,
    limits: Limits,
    found: &mut Findings,
) {
    let bound = Bound::of(&limits, fw.now());
    let (xsdt, dsdt_addr) = match find_dsdt(fw, rsdp_addr, &bound) {
        Ok(found) => found,
        Err(NoDsdt::Cut) => {
            let _ = writeln!(
                out,
                "{LINE}cut off by the proof's bound ({} us, {} steps) before the XSDT's search for a FADT ended",
                bound.time_ns / 1000,
                bound.steps
            );
            found.fail(CUT_OFF);
            return;
        }
        Err(NoDsdt::Because(why)) => {
            let _ = writeln!(out, "{LINE}no DSDT: {why}");
            found.fail("no DSDT");
            return;
        }
    };
    // Only QEMU's own tables (B3-2), judged by the IDs in the header as copied, before any
    // checksum (the review of B3's addendum, B3-13): trusting unchecked IDs to decide not to run
    // is the safe direction, and only QEMU's IDs then need the table checked whole. A header that
    // cannot be copied tells nobody whose tables these are, and fails as before (Q1 of B4's
    // contract).
    let mut first = [0u8; HEADER_LEN];
    let copied = fw
        .copy(dsdt_addr, &mut first)
        .ok()
        .and_then(|()| Header::read(&first).ok());
    let Some(ids) = copied else {
        let _ = writeln!(
            out,
            "{LINE}the DSDT: not readable: its header cannot be copied"
        );
        found.fail("no DSDT");
        return;
    };
    if (ids.oem_id, ids.oem_table_id) != QEMU_DSDT {
        let _ = writeln!(
            out,
            "{LINE}the DSDT's OEM ID \"{}\" and OEM table ID \"{}\" are not QEMU's: the proof runs only on QEMU's own tables (ADR-0019 A3) and ran no AML; passed",
            Escaped(&ids.oem_id),
            Escaped(&ids.oem_table_id)
        );
        found.not_run = true;
        return;
    }
    let dsdt = match lend_if(fw, dsdt_addr, &[b"DSDT"]) {
        Ok(Some((bytes, header))) => checked(out, "the DSDT", bytes, &header),
        Ok(None) => {
            let _ = writeln!(out, "{LINE}the DSDT: not one");
            None
        }
        Err(why) => {
            let _ = writeln!(out, "{LINE}the DSDT: not readable: {why}");
            None
        }
    };
    let Some(dsdt) = dsdt.filter(|d| d.kind == Kind::Dsdt) else {
        found.fail("no DSDT");
        return;
    };
    // The IDs that decided to run were read from one copy of the header, and the table that is
    // checked and run is another read: its IDs must be QEMU's too, or the rule "only QEMU's
    // tables run" would rest on bytes the AML did not come from (the review of B4, B4-9).
    let h = dsdt.table().header();
    if (h.oem_id, h.oem_table_id) != QEMU_DSDT {
        let _ = writeln!(
            out,
            "{LINE}the DSDT changed between two reads: the checked table's OEM ID \"{}\" and OEM table ID \"{}\" are not QEMU's, though its first header's were",
            Escaped(&h.oem_id),
            Escaped(&h.oem_table_id)
        );
        found.fail(CHANGED);
        return;
    }
    let width = IntegerWidth::from_dsdt_revision(dsdt.table().revision());
    let mut i = match Interpreter::new(Recorder::new(host), Config { limits }, width) {
        Ok(i) => i,
        Err(e) => {
            let _ = writeln!(out, "{LINE}no interpreter: {}", Failure(e));
            found.fail("the interpreter could not be made");
            return;
        }
    };
    load(out, fw, &mut i, dsdt, found);
    let entries = crate::acpi::xsdt_entries(xsdt).enumerate();
    for (k, addr) in entries.filter(|&(_, a)| a != 0) {
        if cut_off(out, fw, &bound, i.steps_done(), "an XSDT entry", found) {
            break;
        }
        match lend_if(fw, addr, &[b"SSDT", b"PSDT"]) {
            Ok(None) => {}
            // Each table once (B3-1): an XSDT may list one twice, and a second load of it would
            // run its table-level code again.
            Ok(Some(_)) if crate::acpi::xsdt_entries(xsdt).take(k).any(|a| a == addr) => {
                let _ = writeln!(
                    out,
                    "{LINE}XSDT entry {k} names the table at {addr:#x} again: loaded once"
                );
            }
            Ok(Some((bytes, header))) => match checked(out, "an XSDT entry", bytes, &header) {
                Some(block) => load(out, fw, &mut i, block, found),
                None => found.fail("a definition block was refused"),
            },
            // An entry that cannot be read might have been one.
            Err(why) => {
                let _ = writeln!(out, "{LINE}an XSDT entry: not readable: {why}");
                found.fail("an XSDT entry could not be read");
            }
        }
    }
    // Every table the proof loads is in: the names its load-time code put in packages are bound
    // (TASKS item 63, step B5; `initialize` would bind them too, but the named results are read
    // first).
    if let Err(e) = i.bind_deferred() {
        let _ = writeln!(out, "{LINE}binding package names failed {}", Failure(e));
        found.fail("the names in QEMU's packages could not be bound");
    }
    found.named = named_results(out, fw, &bound, &mut i, found);
    if !found.named {
        found.fail("a named result was not as QEMU's tables give it");
    }
    let walk = walk(out, fw, &bound, &mut i, found);
    found.seen = *i.host().seen();
    found.peak = i.memory_peak();
    found.nodes = i.node_count();
    found.walked = walk.walked;
    // The reasons in an order that lets each come first (the review of B3, B3-5): the walk's
    // bound and its record, then what the host answered, then the devices.
    if walk.cut {
        found.fail("the walk was cut off by its bound");
    }
    if !walk.kept {
        found.fail("no memory to keep what the walk reached");
    }
    let seen = found.seen;
    if seen.answered != 0 {
        found.fail("the host answered a request for hardware other than by refusing it");
    }
    if seen.tables != 0 {
        found.fail("the AML asked for a table");
    }
    // QEMU's own tables are known clean: a tolerance there is a regression to learn of (TASKS
    // item 63, step B5; the coordinating session's ruling on B5's contract, Q9).
    if seen.tolerated != 0 {
        if let Some(t) = seen.first_tolerance {
            let _ = writeln!(
                out,
                "{LINE}tolerated {} time(s); the first: {t}",
                seen.tolerated
            );
        }
        found.fail(TOLERATED);
    }
    // Nor do QEMU's tables store data longer than a field: one that did would be a place where
    // Origin and ACPICA leave the hardware different, to learn of (step B5's point 1).
    if seen.pieces != 0 {
        let _ = writeln!(out, "{LINE}stored in pieces {} time(s)", seen.pieces);
        found.fail(IN_PIECES);
    }
    if seen.lock_other != 0 || seen.lock_taken != seen.lock_given_back {
        found.fail("the Global Lock was not given back as often as it was granted");
    }
    let w = walk.walked;
    if w.failed != 0 {
        found.fail("a device failed other than by a refusal");
    }
    if w.hpet == Some(false) {
        found.fail("the HPET's _STA was not refused in SystemMemory");
    }
    if !w.processors_as_required() {
        found.fail("not every processor's _STA was refused in SystemIO, or there was none");
    }
    if !w.links_as_required() {
        found.fail("not every LNKx _STA was refused in PCI_Config, or there was none");
    }
}

/// A definition block checked by `origin-acpi-tables`, with its line if it is refused.
fn checked(
    out: &mut impl Write,
    what: &str,
    bytes: &'static [u8],
    header: &Header,
) -> Option<Checked<'static>> {
    match Checked::parse(bytes) {
        Ok(block) => Some(block),
        Err(e) => {
            let _ = writeln!(
                out,
                "{LINE}{what}: {} rev {}, {} bytes: {}",
                Sig(header.signature),
                header.revision,
                header.length,
                Refused(e)
            );
            None
        }
    }
}

/// Loads `block` and says how it went.
fn load<H: Host>(
    out: &mut impl Write,
    fw: &mut impl Firmware,
    i: &mut Interpreter<Recorder<H>>,
    block: Checked<'static>,
    found: &mut Findings,
) {
    let h = block.table().header();
    let aml = block.aml();
    let started = fw.now();
    let loaded = i.load(DefinitionBlock {
        signature: h.signature.0,
        oem_id: h.oem_id,
        oem_table_id: h.oem_table_id,
        aml,
    });
    found.load_ns = found
        .load_ns
        .saturating_add(fw.now().saturating_sub(started));
    let _ = write!(
        out,
        "{LINE}{} rev {}, {} bytes, OEM \"{}\", \"{}\": ",
        Sig(h.signature),
        h.revision,
        h.length,
        Ascii(&h.oem_id),
        Ascii(&h.oem_table_id)
    );
    match loaded {
        Ok(table) => {
            found.tables = found.tables.saturating_add(1);
            found.aml_bytes = found
                .aml_bytes
                .saturating_add(u64::try_from(aml.len()).unwrap_or(u64::MAX));
            let _ = writeln!(out, "loaded as table {table}");
        }
        Err(e) => {
            let _ = writeln!(out, "load failed {}; rolled back", Failure(e));
            found.fail("a definition block did not load");
        }
    }
}

/// An interpreter's error, for a line: what, and where.
struct Failure(Error);

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.at {
            Some(at) => write!(
                f,
                "at table {} offset {:#x}: {:?}",
                at.table, at.offset, self.0.kind
            ),
            None => write!(f, "{:?}", self.0.kind),
        }
    }
}

/// What a named result is to be.
#[derive(Clone, Copy)]
enum Want {
    /// A package of four zeros (`\_S5_` on QEMU: the sleep type values of S5).
    FourZeros,
    /// This integer.
    Integer(u64),
    /// No value.
    Nothing,
    /// A buffer that is a resource template closed by its end tag.
    Template,
}

impl Want {
    fn holds(self, got: &Option<Value>) -> bool {
        match (self, got) {
            (Self::FourZeros, Some(Value::Package(p))) => {
                p.len() == 4 && p.iter().all(|v| *v == Value::Integer(0))
            }
            (Self::Integer(want), Some(Value::Integer(i))) => *i == want,
            (Self::Nothing, None) => true,
            (Self::Template, Some(Value::Buffer(b))) => ends_with_end_tag(b),
            _ => false,
        }
    }
}

/// Whether `b` is a resource template: small and large resource descriptors one after another,
/// the first end tag (small item 0Fh, whose length is 1: the byte 79h) closing it, with its
/// checksum byte, at the buffer's end (ACPI 6.6 §6.4). An end tag of another length is not one.
fn ends_with_end_tag(b: &[u8]) -> bool {
    let mut at = 0usize;
    while let Some(&tag) = b.get(at) {
        let (length, end) = if tag & 0x80 == 0 {
            (1 + usize::from(tag & 0x07), tag >> 3 == 0x0F)
        } else {
            let Some(&[lo, hi]) = at
                .checked_add(1)
                .and_then(|s| b.get(s..s.checked_add(2)?))
                .and_then(|s| <&[u8; 2]>::try_from(s).ok())
            else {
                return false;
            };
            (3 + usize::from(u16::from_le_bytes([lo, hi])), false)
        };
        let Some(next) = at.checked_add(length).filter(|&n| n <= b.len()) else {
            return false;
        };
        if end {
            return tag == 0x79 && next == b.len();
        }
        at = next;
    }
    false
}

/// What came back, briefly, for a named result that was wrong.
struct Got<'a>(&'a origin_aml::Result<Option<Value>>);

impl fmt::Display for Got<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Err(e) => write!(f, "{}", Failure(*e)),
            Ok(None) => f.write_str("nothing"),
            Ok(Some(Value::Integer(i))) => write!(f, "the integer {i:#x}"),
            Ok(Some(Value::String(s))) => write!(f, "a string of {} bytes", s.len()),
            Ok(Some(Value::Buffer(b))) => write!(f, "a buffer of {} bytes", b.len()),
            Ok(Some(Value::Package(p))) => write!(f, "a package of {} elements", p.len()),
            Ok(Some(Value::Uninitialized)) => f.write_str("no value"),
            Ok(Some(Value::Reference(_))) => f.write_str("a reference"),
        }
    }
}

/// Evaluates the named results QEMU's tables give without hardware (the contract of step B3,
/// §4), in order, and prints them on one line, each after a check of the proof's bound. Returns
/// whether every one is as expected.
fn named_results<H: Host>(
    out: &mut impl Write,
    fw: &impl Firmware,
    bound: &Bound,
    i: &mut Interpreter<H>,
    found: &mut Findings,
) -> bool {
    /// One named result: how it is printed, the path evaluated, its arguments, what it must be.
    struct Check {
        label: &'static str,
        path: &'static str,
        args: fn() -> Option<Vec<Value>>,
        want: Want,
    }
    const fn check(label: &'static str, path: &'static str, want: Want) -> Check {
        Check {
            label,
            path,
            args: || Some(Vec::new()),
            want,
        }
    }
    let ones = i.width().mask();
    let checks = [
        check("\\_S5_", "\\_S5_", Want::FourZeros),
        check("\\PICF 0", "\\PICF", Want::Integer(0)),
        Check {
            args: || fallible::one_arg(Value::Integer(1)),
            ..check("\\_PIC (1)", "\\_PIC", Want::Nothing)
        },
        check("\\PICF 1", "\\PICF", Want::Integer(1)),
        check(
            "\\_SB.PCI0._HID PNP0A08",
            "\\_SB.PCI0._HID",
            Want::Integer(PCIE_ROOT_BRIDGE),
        ),
        Check {
            args: || fallible::string_arg(b"Windows 2022"),
            ..check(
                "\\_OSI (\"Windows 2022\") Ones",
                "\\_OSI",
                Want::Integer(ones),
            )
        },
        Check {
            args: || fallible::string_arg(b"Linux"),
            ..check("\\_OSI (\"Linux\") Zero", "\\_OSI", Want::Integer(0))
        },
        check("\\_REV 2", "\\_REV", Want::Integer(origin_aml::REV)),
        // Last, so that a test can make the last result costly: QEMU's `_CRS` is a name, but a
        // table may make it a method (the review of B3's addendum, B3-14, the check before the
        // walk).
        check(
            "\\_SB.PCI0._CRS a template",
            "\\_SB.PCI0._CRS",
            Want::Template,
        ),
    ];
    let _ = write!(out, "{LINE}named results:");
    let mut all = true;
    for (k, c) in checks.iter().enumerate() {
        if bound.passed(fw.now(), i.steps_done()) {
            let _ = writeln!(out, "; cut off by the proof's bound before {}", c.label);
            found.fail(CUT_OFF);
            return false;
        }
        // An argument the heap cannot give is the memory limit, as the interpreter's own are.
        let got = (c.args)().map_or(
            Err(Error {
                kind: ErrorKind::Limit(Limit::Memory),
                at: None,
            }),
            |args| i.evaluate(c.path, &args),
        );
        let held = got.as_ref().is_ok_and(|v| c.want.holds(v));
        all &= held;
        let _ = write!(out, "{} {} {held}", if k == 0 { "" } else { ";" }, c.label);
        if !held {
            let _ = write!(out, " ({})", Got(&got));
        }
    }
    let _ = writeln!(out, "; all {all}");
    all
}

/// The failures the proof allows (the contract of step B3, §4): an access to SystemMemory,
/// SystemIO or PCI_Config refused, or, where the host refused a bridge's bus number and no
/// table, a refusal from the host that is not a region's.
///
/// The second rule is global, since the error does not say which request it answers (the
/// review of B3, B3-9). The interpreter has four sources of `Host(_)`: a bridge's bus number, a
/// table, and the Global Lock's acquire and release. It is sound because the proof fails on the
/// other three on their own: any table asked for (`Seen::tables`), and any lock answer but a grant
/// or a release (`Seen::lock_other`).
fn allowed(e: &Error, seen: &Seen) -> bool {
    match e.kind {
        ErrorKind::Region {
            space: 0..=2,
            cause: HostError::Refused,
            ..
        } => true,
        ErrorKind::Host(HostError::Refused) => seen.bridges > 0 && seen.tables == 0,
        _ => false,
    }
}

/// The space a refused region access was in, if `outcome` is a `_STA` that ended so.
const fn sta_refused_in(outcome: &InitOutcome) -> Option<u8> {
    match outcome {
        InitOutcome::StaFailed(Error {
            kind:
                ErrorKind::Region {
                    space,
                    cause: HostError::Refused,
                    ..
                },
            ..
        }) => Some(*space),
        _ => None,
    }
}

/// Whether `node` is a processor: a `Processor` object, or a device whose `_HID`, after its
/// aliases, is a string `ACPI0007`. Nothing is run and the host is not asked (the review of B3,
/// B3-1): a `_HID` that is a method, a field unit, a buffer field, a package or an alias to any of
/// them is not read, and the device is not a processor.
fn is_processor<H: Host>(i: &Interpreter<H>, node: Node) -> bool {
    if i.object_type(node).is_ok_and(|t| t == PROCESSOR) {
        return true;
    }
    let mut child = i.first_child(node);
    while let Some(c) = child {
        if i.name_of(c).is_ok_and(|n| n == *b"_HID") {
            return matches!(i.plain(c), Ok(Some(Plain::String(s))) if s == PROCESSOR_HID);
        }
        child = i.next_sibling(c);
    }
    false
}

/// Whether `node` is a PCI interrupt link as QEMU names them: `\_SB.LNKx`.
fn is_link<H: Host>(i: &Interpreter<H>, node: Node, sb: Option<Node>) -> bool {
    sb.is_some() && i.parent(node) == sb && i.name_of(node).is_ok_and(|n| n.starts_with(b"LNK"))
}

/// What the walk gave the verdict.
struct Walk {
    walked: Walked,
    /// The walk was cut off by its bound.
    cut: bool,
    /// Every outcome was kept.
    kept: bool,
}

/// Walks the namespace's `_STA` and `_INI`, classifies what each device reached, and prints it.
fn walk<H: Host>(
    out: &mut impl Write,
    fw: &mut impl Firmware,
    bound: &Bound,
    i: &mut Interpreter<Recorder<H>>,
    found: &mut Findings,
) -> Walk {
    let mut outcomes: Vec<(Node, InitOutcome)> = Vec::new();
    let (mut devices, mut kept) = (0u32, true);
    let mut result = Walk {
        walked: Walked::default(),
        cut: false,
        kept: true,
    };
    if cut_off(out, fw, bound, i.steps_done(), "the walk", found) {
        return result;
    }
    let started = fw.now();
    let walked = i.initialize(&mut |node, outcome| {
        devices = devices.saturating_add(1);
        kept &= fallible::keep(&mut outcomes, (node, outcome));
    });
    found.walk_ns = fw.now().saturating_sub(started);
    let mut w = Walked {
        devices,
        ..Walked::default()
    };
    if let Err(e) = walked {
        let _ = writeln!(out, "{LINE}walk: cut off by its bound: {}", Failure(e));
        result.cut = true;
    }
    result.kept = kept;
    let seen = *i.host().seen();
    let hpet = i
        .lookup("\\_SB.HPET")
        .ok()
        .flatten()
        .filter(|&n| outcomes.iter().any(|&(o, _)| o == n));
    let sb = i.lookup("\\_SB").ok().flatten();
    let mut first_failure = None;
    for &(node, outcome) in &outcomes {
        if cut_off(out, fw, bound, i.steps_done(), "a classification", found) {
            break;
        }
        let error = match outcome {
            InitOutcome::Initialized { .. } => {
                w.initialized = w.initialized.saturating_add(1);
                None
            }
            InitOutcome::ChildrenOnly { .. } => {
                w.children_only = w.children_only.saturating_add(1);
                None
            }
            InitOutcome::Absent { .. } => {
                w.absent = w.absent.saturating_add(1);
                None
            }
            InitOutcome::StaFailed(e) => {
                if allowed(&e, &seen) {
                    w.sta_refused = w.sta_refused.saturating_add(1);
                }
                Some(e)
            }
            InitOutcome::IniFailed { error, .. } => {
                if allowed(&error, &seen) {
                    w.ini_refused = w.ini_refused.saturating_add(1);
                }
                Some(error)
            }
        };
        if let Some(e) = error.filter(|e| !allowed(e, &seen)) {
            w.failed = w.failed.saturating_add(1);
            first_failure = first_failure.or(Some((node, e)));
        }
        let refused_in = sta_refused_in(&outcome);
        if Some(node) == hpet {
            w.hpet = Some(refused_in == Some(0));
        }
        if is_processor(i, node) {
            w.processors = w.processors.saturating_add(1);
            if refused_in == Some(1) {
                w.processors_refused = w.processors_refused.saturating_add(1);
            }
        }
        if is_link(i, node, sb) {
            w.links = w.links.saturating_add(1);
            if refused_in == Some(2) {
                w.links_refused = w.links_refused.saturating_add(1);
            }
        }
    }
    drop(outcomes);
    if let Some((node, e)) = first_failure {
        let _ = write!(out, "{LINE}walk: ");
        match i.path_of(node) {
            Ok(path) if !path.is_empty() => {
                for (k, seg) in path.iter().enumerate() {
                    let _ = write!(out, "{}{}", if k == 0 { "\\" } else { "." }, Ascii(seg));
                }
            }
            _ => {
                let _ = write!(out, "a device");
            }
        }
        let _ = writeln!(out, " failed other than by a refusal: {}", Failure(e));
    }
    let _ = writeln!(
        out,
        "{LINE}walk: {} devices: {} initialized, {} children only, {} absent, {} _STA refused, {} _INI refused, {} failed otherwise; HPET's _STA refused in SystemMemory {}; {} processors, each _STA refused in SystemIO {}; {} links (LNKx), each _STA refused in PCI_Config {}",
        w.devices,
        w.initialized,
        w.children_only,
        w.absent,
        w.sta_refused,
        w.ini_refused,
        w.failed,
        match w.hpet {
            None => "(no HPET)",
            Some(true) => "true",
            Some(false) => "false",
        },
        w.processors,
        w.processors_as_required(),
        w.links,
        w.links_as_required(),
    );
    result.walked = w;
    result
}

mod fallible;

#[cfg(test)]
mod tests;
