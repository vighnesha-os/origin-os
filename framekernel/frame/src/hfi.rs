//! The thermal LVT's sources: Intel's Hardware Feedback Interface (ADR-0020 S9, source 1), the
//! performance-capability registers HWP and AMD CPPC report where the firmware enabled them
//! (source 3), and Intel's core thermal status (S17's heat). TASKS item 106.
//!
//! **Each processor's start only reads** (`start_this_processor`, from `smp::ap_entry` and the boot
//! processor's path after it records its topology; the coordinating session's ruling Q7 of
//! 2026-11-21). It reads CPUID.06H, 01H and 8000_0008H, and then, only where CPUID claims them, its
//! thermal LVT entry, IA32_PM_ENABLE and HWP's Highest, and CPPC_ENABLE and CPPC's HighestPerf.
//! Each register goes through the MSR probe (`cpu::probe_rdmsr`): one CPUID claims that raises
//! #GP, as under a hypervisor that does not emulate it, is reported, and that source is not used
//! (ruling Q4). It writes nothing to the processor, and puts itself in its package's slot, where
//! the processors of one package are checked against each other.
//!
//! **The round takes the sources** (`round`), once every processor has started and its scheduler
//! runs, so that what a package uses is decided by all of its processors (the review's R106-2):
//!
//! 1. A package is taken only where its processors report the same table ([`Words`]) and every
//!    one that would use the thermal LVT found it free: masked, and not for SMI
//!    ([`crate::apic::lvt_thermal_free`]). Where the firmware holds one, nothing is written to that
//!    package (ruling Q5). Where the package has the HFI, the boot processor allocates its table
//!    then: a run of frames given up for ever (`memory::allocate_table_run`), zeroed, reached only
//!    through the direct map and never mapped to a program.
//! 2. The thermal vector goes to every processor once. Each, in its own handler, takes its own
//!    LVT (fixed delivery to `cpu::THERMAL_VECTOR`), sets heat's two enables where it has a sensor
//!    (ruling Q12), and HWP's Highest-change enable where the firmware has HWP on.
//! 3. Where every processor of a package with the HFI took its LVT, its first processor is sent the
//!    vector again, and enables the HFI: a stale bit 26 acknowledged, the pointer, configuration
//!    bit 0 (Thread Director's bit 1 never), then IA32_PACKAGE_THERM_INTERRUPT bit 25, each read
//!    back. So every LVT of the package is unmasked before the hardware's initial notification,
//!    and the enable comes after every processor's INIT and SIPI, so its own read-back is the one
//!    SDM -093 §17.6.9 asks for (ruling Q11). The HFI is never disabled at M3, so the table is
//!    never freed.
//!
//! **HWP and AMD CPPC** are never enabled, and no enable or request register is ever written
//! (ruling Q2): their capability registers read zero, or may not be read, until they are. The
//! handler reads HWP's Highest again on a Highest-change interrupt, and clears IA32_HWP_STATUS's
//! bit 3, which Intel says software must clear for the next notification (SDM Vol. 3, §17.4.5 and
//! §17.4.6). AMD documents no thermal event the operating system enables: AMD heat is not
//! supported at M3.
//!
//! **The notification** ([`service`]): IA32_PACKAGE_THERM_STATUS bit 26 says the processor wrote
//! the package's table, and it writes no more while the bit is set (SDM Vol. 3, §17.6.6). Under the
//! package's lock, with interrupts off, the handler reads the bit; if set, it copies the flags and
//! every member's row, rescales every processor's capacity, and only then acknowledges, by writing
//! the status with bit 26 cleared, every read-only bit as read, every sticky log bit as 1 (which
//! leaves it as it is) and every reserved bit as 0 ([`acknowledgement`]). A table whose timestamp
//! is still 0 has not been written, and is acknowledged unread (R106-7). Which processor takes a
//! package's interrupt is not stated, so every member holds its LVT unmasked, and whichever takes
//! it first does the work: the others find bit 26 clear. Until a package's first update is read,
//! its processors give no value (R97-14): an unwritten table never reads as "avoid".
//!
//! **Locks**, always with interrupts off: a package's lock, then the rescale's, never the other way
//! (the review's R106-9); the joining lock at start only, alone.
//!
//! **The writes**, all of them (the contract's security invariant 6, with the ruling on R106-4):
//! IA32_HW_FEEDBACK_PTR, IA32_HW_FEEDBACK_CONFIG bit 0, IA32_PACKAGE_THERM_INTERRUPT bit 25, the
//! bit-26 acknowledgement, IA32_THERM_INTERRUPT bits 0 and 1, the thermal LVT entry where free,
//! IA32_HWP_INTERRUPT bit 2 and IA32_HWP_STATUS bit 3's clearing where HWP is already on; and the
//! round's fixed IPIs of the thermal vector.
//!
//! The decoding and the notification's protocol are pure and tested on the host, the protocol
//! against a model of the hardware's rule ([`PackageHardware`]); the rest runs on the kernel's
//! target only.

use crate::capacity::{HFI_FLAGS_AT, HfiFlags, HfiRow, HfiShape, hfi_flags_from, hfi_row_from};

/// IA32_THERM_INTERRUPT (SDM Vol. 4, Table 2-2): exists where CPUID.01H:EDX\[22\] is set.
pub const IA32_THERM_INTERRUPT: u32 = 0x19B;
/// IA32_THERM_STATUS: exists where CPUID.01H:EDX\[22\] is set.
pub const IA32_THERM_STATUS: u32 = 0x19C;
/// IA32_PACKAGE_THERM_STATUS: exists where CPUID.06H:EAX\[6\] is set.
pub const IA32_PACKAGE_THERM_STATUS: u32 = 0x1B1;
/// IA32_PACKAGE_THERM_INTERRUPT: exists where CPUID.06H:EAX\[6\] is set.
pub const IA32_PACKAGE_THERM_INTERRUPT: u32 = 0x1B2;
/// IA32_PM_ENABLE: exists where CPUID.06H:EAX\[7\] (HWP) is set. Read only.
pub const IA32_PM_ENABLE: u32 = 0x770;
/// IA32_HWP_CAPABILITIES: may be read only once HWP is enabled (SDM Vol. 3, §17.4.2).
pub const IA32_HWP_CAPABILITIES: u32 = 0x771;
/// IA32_HWP_INTERRUPT: exists where CPUID.06H:EAX\[8\] is set.
pub const IA32_HWP_INTERRUPT: u32 = 0x773;
/// IA32_HWP_STATUS: may be accessed only once HWP is enabled.
pub const IA32_HWP_STATUS: u32 = 0x777;
/// IA32_HW_FEEDBACK_PTR: package scope; exists where CPUID.06H:EAX\[19\] is set.
pub const IA32_HW_FEEDBACK_PTR: u32 = 0x17D0;
/// IA32_HW_FEEDBACK_CONFIG: package scope; exists where CPUID.06H:EAX\[19\] is set.
pub const IA32_HW_FEEDBACK_CONFIG: u32 = 0x17D1;
/// AMD's CPPC_CAPABILITY_1 (APM Vol. 2, §17.6.4.1): zero until CPPC is enabled.
pub const CPPC_CAPABILITY_1: u32 = 0xC001_02B0;
/// AMD's CPPC_ENABLE (APM Vol. 2, §17.6.4.3): exists where CPUID Fn8000_0008_EBX\[27\] is set.
/// Read only.
pub const CPPC_ENABLE: u32 = 0xC001_02B1;

/// IA32_PACKAGE_THERM_STATUS bit 26: the processor wrote the package's table (R/WC0).
pub const STATUS_CHANGED: u64 = 1 << 26;
/// IA32_PACKAGE_THERM_STATUS's sticky log bits, R/WC0: 1, 3, 5, 7, 9 and 11 (SDM Vol. 3, §17.9).
const STATUS_LOGS: u64 = 0b1010_1010_1010;
/// IA32_PACKAGE_THERM_STATUS's read-only bits: 0, 2, 4, 6, 8, 10 and the readout, 22:16.
const STATUS_READ_ONLY: u64 = 0b0101_0101_0101 | (0x7F << 16);
/// IA32_PACKAGE_THERM_INTERRUPT bit 25: interrupt when the table is written.
pub const PACKAGE_INTERRUPT_HFI: u64 = 1 << 25;
/// IA32_THERM_INTERRUPT bits 0 and 1: interrupt on the high- and the low-temperature transition.
pub const THERM_INTERRUPT_ENABLES: u64 = 0b11;
/// IA32_THERM_INTERRUPT's other interrupt enables (SDM Vol. 3, §17.8.5, Figure 17-32): PROCHOT#
/// (bit 2), FORCEPR# (3), the critical temperature (4), the two thresholds (15, 23) and the power
/// limit (24). The kernel sets none; one the firmware set is reported when the LVT is taken, since
/// it then interrupts too (the review's R106-5).
pub const OTHER_THERM_ENABLES: u64 =
    (1 << 2) | (1 << 3) | (1 << 4) | (1 << 15) | (1 << 23) | (1 << 24);
/// IA32_PACKAGE_THERM_INTERRUPT's enables other than the HFI's bit 25 (§17.9, p. 575): the high and
/// low temperatures (0, 1), PROCHOT# (2), the critical temperature (4), the thresholds (15, 23) and
/// the power limit (24).
pub const OTHER_PACKAGE_ENABLES: u64 = 0b111 | (1 << 4) | (1 << 15) | (1 << 23) | (1 << 24);
/// IA32_PM_ENABLE bit 0 and CPPC_ENABLE bit 0: HWP, or CPPC, is on.
pub const ENABLED: u64 = 1;
/// IA32_HWP_INTERRUPT bit 2: interrupt when Highest changes.
pub const HWP_EN_HIGHEST_CHANGE: u64 = 1 << 2;
/// IA32_HWP_STATUS bit 3: Highest changed (R/WC0).
pub const HWP_HIGHEST_CHANGED: u64 = 1 << 3;
/// IA32_HW_FEEDBACK_CONFIG bit 0: the HFI is on. Bit 1, Thread Director, is never set here.
pub const FEEDBACK_ENABLE: u64 = 1;
/// The frame size the table is counted in.
pub const PAGE: u64 = 4096;

/// The value that acknowledges a table update: bit 26 cleared, every read-only bit as `status`
/// read it, every log bit written as 1, which leaves it as it was, and every reserved bit 0.
pub const fn acknowledgement(status: u64) -> u64 {
    (status & STATUS_READ_ONLY) | STATUS_LOGS
}

/// The value that clears IA32_HWP_STATUS's Highest_Change alone: its other R/WC0 bits (0, 2, 4, 5)
/// written as 1, which leaves them as they were, and bit 1 and every reserved bit as 0.
pub const fn hwp_acknowledgement() -> u64 {
    0b11_0101
}

/// IA32_HW_FEEDBACK_PTR for a table at `table`: its address, and the valid bit.
pub const fn feedback_pointer(table: u64) -> u64 {
    table | 1
}

/// Whether IA32_THERM_STATUS says this core's PROCHOT# is active now (bit 0, RO).
pub const fn hot(status: u64) -> bool {
    status & 1 != 0
}

/// What one processor's CPUID says about the thermal LVT's sources (SDM Vol. 1, Table 21-16;
/// AMD APM Vol. 3, Fn8000_0008).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Words {
    /// CPUID.06H:EAX.
    pub eax6: u32,
    /// CPUID.06H:ECX.
    pub ecx6: u32,
    /// CPUID.06H:EDX.
    pub edx6: u32,
    /// CPUID.01H:EDX.
    pub edx1: u32,
    /// CPUID.80000008H:EBX.
    pub ebx_ext8: u32,
}

impl Words {
    /// CPUID.01H:EDX\[22\]: IA32_THERM_STATUS, IA32_THERM_INTERRUPT and the thermal LVT entry exist
    /// (SDM Vol. 3, §17.8.4).
    pub const fn thermal_registers(&self) -> bool {
        self.edx1 & (1 << 22) != 0
    }

    /// A source of thermal transitions for IA32_THERM_INTERRUPT's bits 0 and 1: the thermal monitor,
    /// CPUID.01H:EDX\[29\] (TM1; SDM Vol. 3, §17.8.4), or the digital thermal sensor,
    /// CPUID.06H:EAX\[0\] (§17.8.5.1). The thermal interrupt comes from one of them (§17.8.2.5,
    /// §17.8.5.2): the registers alone (CPUID.01H:EDX\[22\]) are not one. QEMU 11.1.3's software
    /// processor reports the registers and neither sensor, and drops the enables written there.
    pub const fn thermal_sensor(&self) -> bool {
        self.edx1 & (1 << 29) != 0 || self.eax6 & 1 != 0
    }

    /// Whether this processor's thermal transitions can interrupt it: the registers, and a sensor.
    pub const fn heat(&self) -> bool {
        self.thermal_registers() && self.thermal_sensor()
    }

    /// Whether some source would use this processor's thermal LVT entry: heat, the HFI, or HWP's
    /// Highest-change interrupt where the firmware has HWP on (`hwp_on`). Where none would, the
    /// kernel leaves the entry as it is.
    pub const fn wants_lvt(&self, hwp_on: bool) -> bool {
        self.thermal_registers()
            && (self.heat() || self.hfi_usable() || (hwp_on && self.hwp_highest_interrupt()))
    }

    /// CPUID.06H:EAX\[6\]: the package thermal registers exist.
    pub const fn package_thermal(&self) -> bool {
        self.eax6 & (1 << 6) != 0
    }

    /// CPUID.06H:EAX\[19\]: the HFI.
    pub const fn hfi(&self) -> bool {
        self.eax6 & (1 << 19) != 0
    }

    /// CPUID.06H:EAX\[7\]: HWP, and IA32_PM_ENABLE.
    pub const fn hwp(&self) -> bool {
        self.eax6 & (1 << 7) != 0
    }

    /// CPUID.06H:EAX\[8\] and \[15\]: IA32_HWP_INTERRUPT, with its Highest-change interrupt.
    pub const fn hwp_highest_interrupt(&self) -> bool {
        self.eax6 & (1 << 8) != 0 && self.eax6 & (1 << 15) != 0
    }

    /// CPUID Fn8000_0008_EBX\[27\]: AMD's CPPC registers.
    pub const fn cppc(&self) -> bool {
        self.ebx_ext8 & (1 << 27) != 0
    }

    /// The table's shape, if this processor has an HFI that reports performance.
    pub const fn shape(&self) -> Option<HfiShape> {
        HfiShape::from_cpuid(self.eax6, self.ecx6, self.edx6)
    }

    /// The table's size in pages: CPUID.06H:EDX\[11:8\] plus one (package domain).
    pub const fn pages(&self) -> u64 {
        ((self.edx6 >> 8) & 0xF) as u64 + 1
    }

    /// This processor's row in its package's table: CPUID.06H:EDX\[31:16\] (logical-processor
    /// domain; rows may be shared, and may have gaps).
    pub const fn row(&self) -> u16 {
        (self.edx6 >> 16) as u16
    }

    /// Whether this processor can use the HFI: the HFI, the package thermal registers, the thermal
    /// LVT, and a table that reports performance.
    pub const fn hfi_usable(&self) -> bool {
        self.hfi() && self.package_thermal() && self.thermal_registers() && self.shape().is_some()
    }

    /// What every processor of a package must report alike for the HFI to be used there: whether
    /// it can use it, and the table's shape and size (EAX\[19\], \[6\] and \[23\], the thermal LVT,
    /// ECX\[15:8\], EDX\[11:0\]).
    pub const fn package_signature(&self) -> u64 {
        let eax = self.eax6 & ((1 << 19) | (1 << 6) | (1 << 23));
        let ecx = self.ecx6 & 0xFF00;
        let edx = self.edx6 & 0xFFF;
        (self.hfi_usable() as u64) << 63
            | (self.thermal_registers() as u64) << 62
            | (eax as u64) << 24
            | (ecx as u64) << 12
            | edx as u64
    }

    /// Whether row `row`'s class-0 cells lie inside a table of [`Words::pages`] pages.
    pub fn row_fits(&self, shape: HfiShape) -> bool {
        let size = self.pages() * PAGE;
        shape
            .row_offset(self.row())
            .and_then(|at| at.checked_add(shape.cells()))
            .is_some_and(|end| end as u64 <= size)
    }
}

/// Bytes of the table's timestamp, at its start (Tables 17-4 and 17-7): 0 until the processor first
/// writes the table, since the kernel zeroes it.
pub const TIMESTAMP: usize = 8;

/// A reserved address in the x2APIC's MSR range: 80EH, where xAPIC mode's Destination Format
/// Register sits and x2APIC mode has none (SDM Vol. 3, §13.12.1.2: RDMSR and WRMSR of a reserved
/// address in 800H to 8FFH raise #GP). QEMU 11.1.3's software processor reads 831H, the SDM's
/// other named example, without a fault (2026-11-22).
pub const RESERVED_X2APIC: u32 = 0x80E;

/// What the notification needs of a package's hardware: its status register and its table.
pub trait PackageHardware {
    /// IA32_PACKAGE_THERM_STATUS, or `None` if the read faulted.
    fn status(&mut self) -> Option<u64>;
    /// Acknowledges the update `status` read: writes [`acknowledgement`]`(status)`. `false` if the
    /// write faulted.
    fn acknowledge(&mut self, status: u64) -> bool;
    /// Copies `out.len()` bytes of the table from byte `at`; `false`, with nothing copied, if they
    /// do not lie inside it.
    fn copy(&mut self, at: usize, out: &mut [u8]) -> bool;
}

/// What one service of a package found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Serviced {
    /// Bit 26 was clear: nothing was written since the last acknowledgement.
    Quiet,
    /// Bit 26 was set, but the table's timestamp is still 0: nothing was written to it, so nothing
    /// was read, and the bit was acknowledged so that the processor may write (R106-7).
    Unwritten,
    /// The table was read, and the update acknowledged.
    Updated(HfiFlags),
    /// The table was read, but the acknowledgement faulted: no further update will come.
    Unacknowledged(HfiFlags),
    /// The status could not be read, or the table's first bytes could not be copied, or a stale
    /// bit could not be acknowledged.
    Faulted,
}

/// Most bytes of one class's cells: eight capabilities at most (CPUID.06H:EDX\[7:0\]).
const MOST_CELLS: usize = 8;

/// Services one notification of a package whose table has `shape` (TASKS item 106): if the status
/// says the processor wrote the table, copies its flags and, for each `(cpu, row)` of `members`,
/// that row's class-0 cells, given to `row` (`None` where the row does not lie inside the table);
/// then runs `after`, the rescale; and only then acknowledges. So every row is read while the
/// hardware writes nothing, and before the acknowledgement lets it write again.
pub fn service<H: PackageHardware>(
    hw: &mut H,
    shape: HfiShape,
    members: impl Iterator<Item = (usize, u16)>,
    mut row: impl FnMut(usize, Option<HfiRow>),
    after: impl FnOnce(HfiFlags),
) -> Serviced {
    let Some(status) = hw.status() else {
        return Serviced::Faulted;
    };
    if status & STATUS_CHANGED == 0 {
        return Serviced::Quiet;
    }
    let mut stamp = [0u8; TIMESTAMP];
    if !hw.copy(0, &mut stamp) {
        return Serviced::Faulted;
    }
    if stamp == [0; TIMESTAMP] {
        return if hw.acknowledge(status) {
            Serviced::Unwritten
        } else {
            Serviced::Faulted
        };
    }
    let mut cells = [0u8; MOST_CELLS];
    let Some(cells) = cells.get_mut(..shape.cells()) else {
        return Serviced::Faulted;
    };
    if !hw.copy(HFI_FLAGS_AT, cells) {
        return Serviced::Faulted;
    }
    let Some(flags) = hfi_flags_from(cells, shape) else {
        return Serviced::Faulted;
    };
    for (cpu, index) in members {
        let read = shape
            .row_offset(index)
            .filter(|&at| hw.copy(at, cells))
            .and_then(|_| hfi_row_from(cells, shape));
        row(cpu, read);
    }
    after(flags);
    if hw.acknowledge(status) {
        Serviced::Updated(flags)
    } else {
        Serviced::Unacknowledged(flags)
    }
}

/// Whether `flags` ask for idle to be forced: bit 1 set in every capability's flag of class 0 the
/// table reports, performance's and, where `shape` reports it, efficiency's (SDM Vol. 3, §17.6.7;
/// the review's R106-11). Only class 0 is filled here: the kernel never sets Thread Director's
/// bit 1 (§17.6.5).
pub const fn force_idle(flags: HfiFlags, shape: HfiShape) -> bool {
    flags.performance_force_idle && (!shape.reports_efficiency() || flags.efficiency_force_idle)
}

/// What the round found (`round`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Round {
    /// Whether the thermal handler is registered: without it, nothing is sent.
    pub registered: bool,
    /// Processors the thermal vector was sent to.
    pub sent: u32,
    /// Of them, those that took it before the deadline: at least once, since a real thermal event
    /// may land in the round too.
    pub handled: u32,
    /// Of those, those that had a source to read.
    pub sourced: u32,
    /// Packages taken: every processor agrees, and every LVT a source would use was free.
    pub taken: u32,
    /// Of them, those with the HFI whose first processor was asked to enable it.
    pub asked: u32,
    /// Of those, those that answered within the deadline.
    pub answered: u32,
    /// The package ID of the first that did not: its HFI stays off, with no retry (the coordinating
    /// session's ruling on R106-2).
    pub unanswered: Option<u32>,
    /// Nanoseconds the round waited, both steps.
    pub waited_ns: u64,
}

/// What every processor of `cpus` reported and set (for the hardware capacity proof).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Census {
    /// Processors counted.
    pub processors: u32,
    /// Of them, those that ran `start_this_processor`.
    pub started: u32,
    /// Those whose CPUID.01H:EDX\[22\] says the thermal registers exist.
    pub thermal_registers: u32,
    /// Those with a thermal sensor as well (TM1 or the digital thermal sensor): heat's processors.
    pub sensors: u32,
    /// Those whose thermal LVT entry the firmware left free.
    pub lvt_free: u32,
    /// Those whose thermal LVT the firmware holds.
    pub lvt_firmware: u32,
    /// Those whose thermal LVT the kernel took.
    pub lvt_taken: u32,
    /// Of them, those where the firmware had set other thermal interrupt enables (R106-5).
    pub other_enables: u32,
    /// Those whose thermal transitions interrupt them.
    pub heat: u32,
    /// Of them, those hot when last read.
    pub hot: u32,
    /// Those whose CPUID reports the HFI.
    pub hfi: u32,
    /// Those that read their row of a table the kernel uses.
    pub hfi_members: u32,
    /// Those whose row lies past their table.
    pub rows_past: u32,
    /// Those whose CPUID reports HWP.
    pub hwp: u32,
    /// Of them, those whose firmware had it on.
    pub hwp_on: u32,
    /// Of them, those whose Highest changes interrupt them.
    pub hwp_notify: u32,
    /// Those whose CPUID reports AMD's CPPC.
    pub cppc: u32,
    /// Of them, those whose firmware had it on.
    pub cppc_on: u32,
    /// Those that recorded no topology.
    pub no_topology: u32,
    /// Those on which a register CPUID claims raised #GP.
    pub faulted: u32,
    /// The first such register.
    pub first_fault: Option<u32>,
    /// Those on which an enable did not read back.
    pub mismatched: u32,
    /// Thermal interrupts that had a source and found nothing changed (R106-5).
    pub quiet: u64,
    /// Packages found.
    pub packages: u32,
    /// Of them, those taken at the round.
    pub taken: u32,
    /// Of them, those whose HFI the kernel enabled.
    pub enabled: u32,
    /// Of those, those whose table the processor has written.
    pub written: u32,
    /// Packages whose processors disagree on the table.
    pub disagree: u32,
    /// Packages with a processor whose LVT a source would use and the firmware holds.
    pub no_lvt: u32,
    /// Packages whose firmware had the HFI on.
    pub firmware_hfi: u32,
    /// Packages whose firmware had set other package thermal interrupt enables (R106-5).
    pub other_package_enables: u32,
    /// Packages whose enable faulted or did not read back.
    pub failed: u32,
    /// Packages with no frames for a table.
    pub no_memory: u32,
    /// Packages whose acknowledgement faulted.
    pub unacknowledged: u32,
    /// Updates read and acknowledged, every package's.
    pub updates: u64,
}

impl Census {
    /// Whether nothing found is a failure (ADR-0013 R3: the proof reports it): no register CPUID
    /// claims faulted, every enable read back, no package disagrees, failed, lacks memory or went
    /// unacknowledged, no row lies past its table, and every processor counted started.
    pub const fn sound(&self) -> bool {
        self.faulted == 0
            && self.mismatched == 0
            && self.disagree == 0
            && self.failed == 0
            && self.no_memory == 0
            && self.unacknowledged == 0
            && self.rows_past == 0
            && self.no_topology == 0
            && self.started == self.processors
    }
}

/// The heat clause of the hardware capacity proof's line, from a census: where no processor has a
/// thermal sensor, heat is said to be not supported, and why, rather than counted as 0 (the
/// coordinating session's ruling on the first turn's M1, 2026-11-22).
#[derive(Clone, Copy, Debug)]
pub struct HeatClause<'a>(pub &'a Census);

impl core::fmt::Display for HeatClause<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let c = self.0;
        match (c.thermal_registers, c.sensors) {
            (0, _) => f.write_str("heat: not supported: no thermal registers"),
            (_, 0) => f.write_str("heat: not supported: no thermal sensor"),
            (_, sensors) => write!(
                f,
                "heat on {} of {sensors} with a sensor, {} hot",
                c.heat, c.hot
            ),
        }
    }
}

/// Every CPU index's slot (ADR-0008 Amendment 1, D1): its processor's record, and the package its
/// index numbers among the packages found.
#[cfg(target_os = "none")]
pub(crate) static SLOTS: crate::percpu::PerCpu<kernel::Slot> = crate::percpu::PerCpu::new(
    "origin_frame::hfi::SLOTS",
    kernel::Slot::new(),
    kernel::Slot::new,
);

#[cfg(target_os = "none")]
pub use kernel::{
    census, reserved_x2apic_read_faults, round, source, start_capacity, start_this_processor,
};

/// The kernel's state and paths: everything that reads the processor, the table or the live store.
#[cfg(target_os = "none")]
mod kernel {
    use alloc::boxed::Box;
    use core::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering};

    use super::*;
    use crate::capacity::{Reports, Source};
    use crate::sync::SpinLock;
    use crate::{
        apic,
        cpu::{self, ThisCpu},
        irq::Action,
        timer::{self, X2Apic},
        trap::Here,
    };

    // --- The kernel's state --------------------------------------------------------------------

    /// A row value no row gave.
    const NO_VALUE: u16 = u16::MAX;
    /// The package slot of a processor in none, and the first processor of a package with none.
    const NONE: usize = usize::MAX;

    // A processor's state.
    /// `start_this_processor` has run on it.
    const STARTED: u32 = 1;
    /// Its thermal LVT entry was free when it started: masked, and not for SMI.
    const LVT_FREE: u32 = 1 << 1;
    /// The firmware holds its thermal LVT entry: unmasked, or for SMI.
    const LVT_FIRMWARE: u32 = 1 << 2;
    /// Some source would use its thermal LVT entry (`Words::wants_lvt`).
    const WANTS_LVT: u32 = 1 << 3;
    /// The kernel took its thermal LVT entry, at the round.
    const LVT_TAKEN: u32 = 1 << 4;
    /// The firmware had set other thermal interrupt enables than the two the kernel sets.
    const OTHER_ENABLES: u32 = 1 << 5;
    /// Its core's thermal transitions interrupt it (IA32_THERM_INTERRUPT bits 0 and 1).
    const HEAT: u32 = 1 << 6;
    /// IA32_THERM_STATUS bit 0 was set when last read: its core is hot.
    const HOT: u32 = 1 << 7;
    /// CPUID claims HWP.
    const HWP_PRESENT: u32 = 1 << 8;
    /// The firmware had HWP on.
    const HWP_ON: u32 = 1 << 9;
    /// Highest changes interrupt it (IA32_HWP_INTERRUPT bit 2).
    const HWP_NOTIFY: u32 = 1 << 10;
    /// CPUID claims AMD's CPPC.
    const CPPC_PRESENT: u32 = 1 << 11;
    /// The firmware had CPPC on.
    const CPPC_ON: u32 = 1 << 12;
    /// It reads its row of its package's HFI table.
    const HFI_MEMBER: u32 = 1 << 13;
    /// Its row lies past its package's table: it gives no value.
    const ROW_PAST: u32 = 1 << 14;
    /// A register CPUID claims raised #GP.
    const FAULTED: u32 = 1 << 15;
    /// An enable it wrote did not read back.
    const MISMATCH: u32 = 1 << 16;
    /// It recorded no topology, so no package.
    const NO_TOPOLOGY: u32 = 1 << 17;

    // A package's state.
    /// Its processors do not report the same table.
    const P_DISAGREE: u32 = 1;
    /// A processor of it whose LVT a source would use found it the firmware's, or unreadable.
    const P_NO_LVT: u32 = 1 << 1;
    /// Taken at the round: every processor agrees, and every LVT a source would use was free.
    const P_TAKEN: u32 = 1 << 2;
    /// It has the HFI, and its table is allocated.
    const P_HFI: u32 = 1 << 3;
    /// Its first processor is asked to enable the HFI.
    const P_ENABLE: u32 = 1 << 4;
    /// Its HFI was enabled, and every write read back.
    const P_ENABLED: u32 = 1 << 5;
    /// The processor has written its table, and the update was read.
    const P_WRITTEN: u32 = 1 << 6;
    /// The firmware had its HFI enabled already: left as it was, and not used.
    const P_FIRMWARE_HFI: u32 = 1 << 7;
    /// A write faulted or did not read back.
    const P_FAILED: u32 = 1 << 8;
    /// No run of frames for its table.
    const P_NO_MEMORY: u32 = 1 << 9;
    /// An acknowledgement faulted: no further update will come.
    const P_UNACKNOWLEDGED: u32 = 1 << 10;
    /// Its latest update asked for idle to be forced ([`force_idle`]).
    const P_FORCE_IDLE: u32 = 1 << 11;
    /// The firmware had set other package thermal interrupt enables than bit 25.
    const P_OTHER_ENABLES: u32 = 1 << 12;
    /// The bits that keep a package's HFI from being used.
    const P_UNUSABLE: u32 = P_DISAGREE | P_NO_LVT | P_FIRMWARE_HFI | P_FAILED | P_NO_MEMORY;

    /// One processor's record.
    #[derive(Debug)]
    struct Record {
        /// Its CPUID words, as [`Words`] in order.
        words: [AtomicU32; 5],
        /// Its state bits.
        state: AtomicU32,
        /// Its package's slot, or [`NONE`].
        package: AtomicUsize,
        /// Its row's class-0 performance, or [`NO_VALUE`].
        performance: AtomicU16,
        /// Its row's class-0 efficiency, or [`NO_VALUE`].
        efficiency: AtomicU16,
        /// HWP's or CPPC's Highest, or 0.
        highest: AtomicU32,
        /// Thermal interrupts it took.
        taken: AtomicU64,
        /// Of them, those it had a source for.
        sourced: AtomicU64,
        /// Of those, those that found nothing changed.
        quiet: AtomicU64,
        /// The first register CPUID claimed that raised #GP, or 0.
        fault: AtomicU32,
    }

    impl Record {
        const fn new() -> Self {
            Self {
                words: [const { AtomicU32::new(0) }; 5],
                state: AtomicU32::new(0),
                package: AtomicUsize::new(NONE),
                performance: AtomicU16::new(NO_VALUE),
                efficiency: AtomicU16::new(NO_VALUE),
                highest: AtomicU32::new(0),
                taken: AtomicU64::new(0),
                sourced: AtomicU64::new(0),
                quiet: AtomicU64::new(0),
                fault: AtomicU32::new(0),
            }
        }

        fn words(&self) -> Words {
            let w = |i: usize| self.words[i].load(Ordering::Relaxed);
            Words {
                eax6: w(0),
                ecx6: w(1),
                edx6: w(2),
                edx1: w(3),
                ebx_ext8: w(4),
            }
        }

        fn state(&self) -> u32 {
            self.state.load(Ordering::Acquire)
        }

        fn has(&self, bits: u32) -> bool {
            self.state() & bits == bits
        }

        fn set(&self, bits: u32) {
            self.state.fetch_or(bits, Ordering::AcqRel);
        }

        fn clear(&self, bits: u32) {
            self.state.fetch_and(!bits, Ordering::AcqRel);
        }

        /// Records that `msr`, which CPUID claims, raised #GP.
        fn fault(&self, msr: u32) {
            self.set(FAULTED);
            let _ = self
                .fault
                .compare_exchange(0, msr, Ordering::AcqRel, Ordering::Relaxed);
        }
    }

    /// One package's state, kept in the slot its number among the packages found names.
    struct Package {
        /// Its package ID (`topology::Placement::package`).
        id: AtomicU32,
        /// Its first processor's [`Words::package_signature`].
        signature: AtomicU64,
        /// Its table's size in pages, as its first processor reports it.
        pages: AtomicU64,
        /// Its processors.
        members: AtomicU32,
        /// Its first processor by CPU index: the one that enables the HFI.
        first: AtomicUsize,
        /// Its state bits.
        state: AtomicU32,
        /// Its table's physical address, or 0.
        table: AtomicU64,
        /// Updates read and acknowledged.
        updates: AtomicU64,
        /// Held while its notification is serviced, with interrupts off; before the rescale's.
        lock: SpinLock<()>,
    }

    // Written by hand so that the table's physical address is never shown (the review's R106-14).
    impl core::fmt::Debug for Package {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_struct("Package")
                .field("state", &self.state.load(Ordering::Relaxed))
                .field("members", &self.members.load(Ordering::Relaxed))
                .finish_non_exhaustive()
        }
    }

    impl Package {
        const fn new() -> Self {
            Self {
                id: AtomicU32::new(0),
                signature: AtomicU64::new(0),
                pages: AtomicU64::new(0),
                members: AtomicU32::new(0),
                first: AtomicUsize::new(NONE),
                state: AtomicU32::new(0),
                table: AtomicU64::new(0),
                updates: AtomicU64::new(0),
                lock: SpinLock::new(()),
            }
        }

        fn state(&self) -> u32 {
            self.state.load(Ordering::Acquire)
        }

        fn set(&self, bits: u32) {
            self.state.fetch_or(bits, Ordering::AcqRel);
        }

        fn clear(&self, bits: u32) {
            self.state.fetch_and(!bits, Ordering::AcqRel);
        }

        fn usable(&self) -> bool {
            self.state() & (P_ENABLED | P_UNUSABLE) == P_ENABLED
        }

        /// The signature's bit for "can use the HFI" (`Words::package_signature`, bit 63).
        fn has_hfi(&self) -> bool {
            self.signature.load(Ordering::Relaxed) >> 63 != 0
        }
    }

    /// One CPU index's slot: its processor's record, and the package its index numbers among the
    /// packages found, where there are that many (each package has a processor, so never more).
    #[derive(Debug)]
    pub(crate) struct Slot {
        record: Record,
        package: Package,
    }

    impl Slot {
        /// A slot with nothing recorded.
        pub(crate) const fn new() -> Self {
            Self {
                record: Record::new(),
                package: Package::new(),
            }
        }
    }

    impl Default for Slot {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Packages found: slots `0..PACKAGES` hold one each.
    static PACKAGES: AtomicUsize = AtomicUsize::new(0);
    /// Held, with interrupts off, while a processor joins its package (start only), alone.
    static JOINING: SpinLock<()> = SpinLock::new(());
    /// The handler's registration: 0 not tried, 1 registered, 2 refused.
    static REGISTERED: AtomicU8 = AtomicU8::new(0);
    /// The rescale's buffers, allocated once when the live store starts, and held, with interrupts
    /// off, while a rescale runs: so a rescale allocates nothing. Taken after a package's lock,
    /// never before one.
    static RESCALE: SpinLock<Option<Scratch>> = SpinLock::new(None);
    /// The source the last rescale chose, as [`Source`] in order: 0 HFI, 1 `_CPC`, 2 register,
    /// 3 equal.
    static SOURCE: AtomicU8 = AtomicU8::new(3);

    /// The rescale's buffers, one entry per online processor.
    struct Scratch {
        /// The online processors' CPU indexes, as the boot found them.
        online: Box<[usize]>,
        hfi: Box<[Option<u32>]>,
        register: Box<[Option<u32>]>,
        out: Box<[u16]>,
    }

    fn slot(k: usize) -> Option<&'static Slot> {
        SLOTS.get(k)
    }

    // --- Start: reads only -----------------------------------------------------------------------

    /// The CPUID words this module reads, on this processor.
    fn read_words() -> Words {
        let six = if cpu::cpuid(0)[0] >= 6 {
            cpu::cpuid(6)
        } else {
            [0; 4]
        };
        let eight = if cpu::cpuid(0x8000_0000)[0] >= 0x8000_0008 {
            cpu::cpuid(0x8000_0008)
        } else {
            [0; 4]
        };
        Words {
            eax6: six[0],
            ecx6: six[2],
            edx6: six[3],
            edx1: cpu::cpuid(1)[3],
            ebx_ext8: eight[1],
        }
    }

    /// Reads `msr` through the probe, recording a fault against `r`. Only this module's registers
    /// are passed, each where CPUID claims it.
    fn read(r: &Record, msr: u32) -> Option<u64> {
        // SAFETY: CPL 0. Every register this module reads has no read side effect (SDM Vol. 4,
        // Table 2-2; APM Vol. 2, §17.6), and is read only where CPUID claims it; one that raises
        // #GP is recovered and recorded.
        let value = unsafe { cpu::probe_rdmsr(msr) };
        if value.is_none() {
            r.fault(msr);
        }
        value
    }

    /// Writes `value` to `msr` through the probe, recording a fault against `r`.
    ///
    /// # Safety
    ///
    /// As [`cpu::probe_wrmsr`]: the write's effect, where it is taken, is one the caller intends
    /// and keeps memory-safe.
    unsafe fn write(r: &Record, msr: u32, value: u64) -> bool {
        // SAFETY: the caller's contract.
        let written = unsafe { cpu::probe_wrmsr(msr, value) };
        if !written {
            r.fault(msr);
        }
        written
    }

    /// Writes `value | bits` to `msr`, `value` being what it read, and says whether `bits` read
    /// back set; a mismatch is recorded against `r`.
    ///
    /// # Safety
    ///
    /// As [`write`].
    unsafe fn set_bits(r: &Record, msr: u32, value: u64, bits: u64) -> bool {
        // SAFETY: the caller's contract.
        let held = unsafe { write(r, msr, value | bits) }
            && read(r, msr).is_some_and(|v| v & bits == bits);
        if !held {
            r.set(MISMATCH);
        }
        held
    }

    /// Registers the thermal handler, once: whether it is registered.
    fn registered() -> bool {
        cpu::without_interrupts(|| {
            let _joining = JOINING.lock();
            if REGISTERED.load(Ordering::Acquire) == 0 {
                let ok = crate::irq::register(cpu::THERMAL_VECTOR, thermal).is_ok();
                REGISTERED.store(if ok { 1 } else { 2 }, Ordering::Release);
            }
            REGISTERED.load(Ordering::Acquire) == 1
        })
    }

    /// Reads what this processor reports about the thermal LVT's sources, on itself, and writes
    /// nothing (the module's documentation; the coordinating session's ruling Q7). Called once by
    /// each processor as it starts: `me` is its start-up flow's index. A second call does nothing.
    pub fn start_this_processor(me: ThisCpu<'_>) {
        let Some(slot) = SLOTS.this(me) else {
            return;
        };
        let r = &slot.record;
        if r.has(STARTED) {
            return;
        }
        let words = read_words();
        for (cell, value) in r.words.iter().zip([
            words.eax6,
            words.ecx6,
            words.edx6,
            words.edx1,
            words.ebx_ext8,
        ]) {
            cell.store(value, Ordering::Relaxed);
        }
        // Registered on every processor's start, whatever its sources, so the round reaches every
        // processor's handler.
        let _ = registered();

        // The thermal LVT entry, where CPUID says it exists: whose it is, read only.
        if words.thermal_registers()
            && let Some(lapic) = X2Apic::here(me)
        {
            match lapic.thermal_entry() {
                None => r.fault(apic::X2APIC_LVT_THERMAL),
                Some(entry) if apic::lvt_thermal_free(entry) => r.set(LVT_FREE),
                Some(_) => r.set(LVT_FIRMWARE),
            }
        }

        // HWP, never enabled here: Highest only where the firmware turned it on.
        let mut hwp_on = false;
        if words.hwp() {
            r.set(HWP_PRESENT);
            if read(r, IA32_PM_ENABLE).is_some_and(|v| v & ENABLED != 0) {
                hwp_on = true;
                r.set(HWP_ON);
                if let Some(c) = read(r, IA32_HWP_CAPABILITIES) {
                    r.highest
                        .store(crate::capacity::hwp_highest(c), Ordering::Relaxed);
                }
            }
        }

        // AMD's CPPC, never enabled here: HighestPerf only where the firmware turned it on.
        if words.cppc() {
            r.set(CPPC_PRESENT);
            if read(r, CPPC_ENABLE).is_some_and(|v| v & ENABLED != 0) {
                r.set(CPPC_ON);
                if let Some(c) = read(r, CPPC_CAPABILITY_1) {
                    r.highest
                        .store(crate::capacity::amd_cppc_highest(c), Ordering::Relaxed);
                }
            }
        }

        let wants = words.wants_lvt(hwp_on);
        if wants {
            r.set(WANTS_LVT);
        }
        // Its package, checked against the package's first processor.
        match crate::smp::topology_of(me.index()) {
            Some(t) => join(r, me.index().get(), t.place().package, words, wants),
            None => r.set(NO_TOPOLOGY),
        }
        r.set(STARTED);
    }

    /// Puts the processor `index` of `r` in package `id`'s slot, finding it or making it, and
    /// checks it against the package's first processor: the same table, and, where a source would
    /// use it (`wants`), a free LVT. It writes nothing to the processor.
    fn join(r: &Record, index: usize, id: u32, words: Words, wants: bool) {
        let signature = words.package_signature();
        let joined = cpu::without_interrupts(|| {
            let _joining = JOINING.lock();
            let count = PACKAGES.load(Ordering::Acquire);
            let found = (0..count)
                .find(|&k| slot(k).is_some_and(|s| s.package.id.load(Ordering::Relaxed) == id));
            let k = found.unwrap_or(count);
            let p = &slot(k)?.package;
            if found.is_none() {
                p.id.store(id, Ordering::Relaxed);
                p.signature.store(signature, Ordering::Relaxed);
                p.pages.store(words.pages(), Ordering::Relaxed);
                PACKAGES.store(count + 1, Ordering::Release);
            } else if p.signature.load(Ordering::Relaxed) != signature {
                p.set(P_DISAGREE);
            }
            if wants && !r.has(LVT_FREE) {
                p.set(P_NO_LVT);
            }
            p.members.fetch_add(1, Ordering::AcqRel);
            p.first.fetch_min(index, Ordering::AcqRel);
            Some(k)
        });
        if let Some(k) = joined {
            r.package.store(k, Ordering::Release);
        }
    }

    // --- The round: the sources taken ----------------------------------------------------------

    /// Takes this processor's sources (the round's step 2), on itself, in its thermal handler: its
    /// LVT entry, heat's enables where it has a sensor, and HWP's Highest-change enable where the
    /// firmware has HWP on. Once only, and only where its package was taken.
    fn take(me: ThisCpu<'_>, r: &Record, p: &Package) {
        if !r.has(WANTS_LVT) || r.state() & (LVT_TAKEN | MISMATCH) != 0 {
            return;
        }
        let words = r.words();
        let Some(lapic) = X2Apic::here(me) else {
            return r.set(MISMATCH);
        };
        if !lapic.take_thermal() {
            return r.set(MISMATCH);
        }
        r.set(LVT_TAKEN);
        if words.heat()
            && let Some(before) = read(r, IA32_THERM_INTERRUPT)
        {
            if before & OTHER_THERM_ENABLES != 0 {
                r.set(OTHER_ENABLES);
            }
            // SAFETY: bits 0 and 1 only ask this core for an interrupt at its high- and
            // low-temperature transitions, through the LVT entry the kernel now holds; the
            // thresholds and every other bit are written as read.
            if unsafe { set_bits(r, IA32_THERM_INTERRUPT, before, THERM_INTERRUPT_ENABLES) } {
                r.set(HEAT);
                if read(r, IA32_THERM_STATUS).is_some_and(hot) {
                    r.set(HOT);
                }
            }
        }
        if r.has(HWP_ON)
            && words.hwp_highest_interrupt()
            && let Some(before) = read(r, IA32_HWP_INTERRUPT)
        {
            // SAFETY: bit 2 only asks for an interrupt, through the LVT entry the kernel holds,
            // when Highest changes; every other bit is written as read.
            if unsafe { set_bits(r, IA32_HWP_INTERRUPT, before, HWP_EN_HIGHEST_CHANGE) } {
                r.set(HWP_NOTIFY);
            }
        }
        if p.has_hfi() && r.has(LVT_TAKEN) {
            let fits = words.shape().is_some_and(|shape| words.row_fits(shape));
            r.set(if fits { HFI_MEMBER } else { ROW_PAST });
        }
    }

    /// Enables package `p`'s HFI from its first processor, `r`'s (the round's step 3): a stale bit
    /// 26 acknowledged, the pointer to the table the boot processor allocated and zeroed,
    /// configuration bit 0, and the package interrupt's bit 25, each read back.
    fn enable(r: &Record, p: &Package) {
        match read(r, IA32_HW_FEEDBACK_CONFIG) {
            None => return p.set(P_FAILED),
            // The firmware's, if it enabled it: left alone, and not used.
            Some(c) if c & FEEDBACK_ENABLE != 0 => return p.set(P_FIRMWARE_HFI),
            Some(_) => {}
        }
        // A bit 26 already set would hold back the processor's first write (§17.6.5, §17.6.6;
        // R106-7): acknowledged first, with the same write as any update's.
        match read(r, IA32_PACKAGE_THERM_STATUS) {
            None => return p.set(P_FAILED),
            Some(status) if status & STATUS_CHANGED != 0 => {
                // SAFETY: the acknowledgement clears bit 26 alone (`acknowledgement`).
                if !unsafe { write(r, IA32_PACKAGE_THERM_STATUS, acknowledgement(status)) } {
                    return p.set(P_FAILED);
                }
            }
            Some(_) => {}
        }
        let table = p.table.load(Ordering::Acquire);
        let pointer = feedback_pointer(table);
        // SAFETY: the pointer names the table the boot processor allocated and zeroed for this
        // package, before it asked for this enable, which the processor may write from now on and
        // nothing else uses, ever.
        let mut done = table != 0 && unsafe { write(r, IA32_HW_FEEDBACK_PTR, pointer) };
        // SAFETY: bit 0 alone turns the HFI on, writing the table the pointer names; Thread
        // Director's bit 1 stays clear.
        done = done && unsafe { write(r, IA32_HW_FEEDBACK_CONFIG, FEEDBACK_ENABLE) };
        done = done
            && read(r, IA32_PACKAGE_THERM_INTERRUPT).is_some_and(|before| {
                if before & OTHER_PACKAGE_ENABLES != 0 {
                    p.set(P_OTHER_ENABLES);
                }
                // SAFETY: bit 25 only asks for an interrupt, through the LVT entries the kernel
                // holds on every processor of this package, when the table is written; every
                // other bit is written as read.
                unsafe {
                    write(
                        r,
                        IA32_PACKAGE_THERM_INTERRUPT,
                        before | PACKAGE_INTERRUPT_HFI,
                    )
                }
            });
        let held = done
            && read(r, IA32_HW_FEEDBACK_PTR) == Some(pointer)
            && read(r, IA32_HW_FEEDBACK_CONFIG).is_some_and(|c| c & 0b11 == FEEDBACK_ENABLE)
            && read(r, IA32_PACKAGE_THERM_INTERRUPT)
                .is_some_and(|v| v & PACKAGE_INTERRUPT_HFI != 0);
        p.set(if held { P_ENABLED } else { P_FAILED });
    }

    // --- The handler -------------------------------------------------------------------------------

    /// The thermal interrupt's handler (`irq::register`'s, on `cpu::THERMAL_VECTOR`): the round's
    /// requests, then heat, HWP's Highest and the package's table, each only where this processor
    /// holds that source. Allocates nothing, and holds no lock but its package's and then the
    /// rescale's, with interrupts off.
    fn thermal(here: Here<'_>) -> Action {
        let me = cpu::this_cpu(here);
        let index = me.index().get();
        let Some(slot) = SLOTS.this(me) else {
            return Action::Resume;
        };
        let r = &slot.record;
        r.taken.fetch_add(1, Ordering::Relaxed);
        let k = r.package.load(Ordering::Acquire);
        let package = slot_package(k);
        // The round's requests.
        if let Some(p) = package
            && p.state() & P_TAKEN != 0
        {
            take(me, r, p);
            if p.state() & P_ENABLE != 0
                && p.first.load(Ordering::Acquire) == index
                && p.state.fetch_and(!P_ENABLE, Ordering::AcqRel) & P_ENABLE != 0
            {
                enable(r, p);
            }
        }
        let state = r.state();
        let (mut sourced, mut changed) = (false, false);
        if state & HEAT != 0 {
            sourced = true;
            if let Some(status) = read(r, IA32_THERM_STATUS) {
                let h = hot(status);
                changed |= h != (state & HOT != 0);
                if h {
                    r.set(HOT);
                } else {
                    r.clear(HOT);
                }
                crate::capacity::LIVE.set_throttled(index, h);
            }
        }
        if state & HWP_NOTIFY != 0 {
            sourced = true;
            if read(r, IA32_HWP_STATUS).is_some_and(|s| s & HWP_HIGHEST_CHANGED != 0) {
                changed = true;
                if let Some(c) = read(r, IA32_HWP_CAPABILITIES) {
                    r.highest
                        .store(crate::capacity::hwp_highest(c), Ordering::Relaxed);
                }
                // SAFETY: clears Highest_Change alone, which Intel says software must clear for
                // the next change to interrupt; the other status bits are written as 1, which
                // leaves them (the coordinating session's ruling on R106-4).
                unsafe { write(r, IA32_HWP_STATUS, hwp_acknowledgement()) };
                rescale();
            }
        }
        if state & HFI_MEMBER != 0
            && let Some(p) = package
        {
            sourced = true;
            changed |= check(r, k, p);
        }
        if sourced {
            r.sourced.fetch_add(1, Ordering::Relaxed);
            if !changed {
                r.quiet.fetch_add(1, Ordering::Relaxed);
            }
        }
        Action::Resume
    }

    fn slot_package(k: usize) -> Option<&'static Package> {
        slot(k).map(|s| &s.package)
    }

    /// Package `p`'s table, as the notification reads it: the status register on this processor,
    /// and the table through the direct map, byte by byte and volatile, as memory a device writes.
    struct Table<'a> {
        record: &'a Record,
        table: u64,
        size: u64,
    }

    impl PackageHardware for Table<'_> {
        fn status(&mut self) -> Option<u64> {
            read(self.record, IA32_PACKAGE_THERM_STATUS)
        }

        fn acknowledge(&mut self, status: u64) -> bool {
            // SAFETY: the acknowledgement clears bit 26 alone: the log bits are written as 1, which
            // leaves them, the read-only bits as read and the reserved bits as 0.
            unsafe {
                write(
                    self.record,
                    IA32_PACKAGE_THERM_STATUS,
                    acknowledgement(status),
                )
            }
        }

        fn copy(&mut self, at: usize, out: &mut [u8]) -> bool {
            let fits = (at as u64)
                .checked_add(out.len() as u64)
                .is_some_and(|end| end <= self.size);
            if !fits || self.table == 0 {
                return false;
            }
            let base =
                core::ptr::with_exposed_provenance::<u8>(crate::phys::to_virt(self.table) as usize);
            for (i, byte) in out.iter_mut().enumerate() {
                // SAFETY: `at + i` lies inside the table (checked above), a run of frames this
                // module holds for ever and the direct map maps. The processor writes it only while
                // bit 26 is clear, and this reads it only after the status read found bit 26 set:
                // RDMSR does not serialize, but the probe's disarm after it is a sequentially
                // consistent store (a locked exchange) that waits for RDMSR's result, so no load
                // here passes it (the review's R106-15).
                *byte = unsafe { base.add(at + i).read_volatile() };
            }
            true
        }
    }

    /// The members of package slot `k`, with their rows: every processor that reads its row there.
    fn members(k: usize) -> impl Iterator<Item = (usize, u16)> {
        (0..cpu::count()).filter_map(move |i| {
            let r = &slot(i)?.record;
            (r.has(HFI_MEMBER) && r.package.load(Ordering::Acquire) == k)
                .then(|| (i, r.words().row()))
        })
    }

    /// Services package `p`'s notification, from `r`'s processor, under the package's lock, and
    /// says whether it read an update.
    fn check(r: &Record, k: usize, p: &Package) -> bool {
        let _held = p.lock.lock();
        if !p.usable() {
            return false;
        }
        let Some(shape) = r.words().shape() else {
            return false;
        };
        let mut table = Table {
            record: r,
            table: p.table.load(Ordering::Relaxed),
            size: p.pages.load(Ordering::Relaxed) * PAGE,
        };
        let serviced = service(
            &mut table,
            shape,
            members(k),
            |cpu, row| {
                if let Some(m) = slot(cpu).map(|s| &s.record) {
                    let (performance, efficiency) = match row {
                        Some(row) => (
                            u16::from(row.performance),
                            row.efficiency.map_or(NO_VALUE, u16::from),
                        ),
                        None => (NO_VALUE, NO_VALUE),
                    };
                    m.performance.store(performance, Ordering::Relaxed);
                    m.efficiency.store(efficiency, Ordering::Relaxed);
                }
            },
            |flags| {
                if force_idle(flags, shape) {
                    p.set(P_FORCE_IDLE);
                } else {
                    p.clear(P_FORCE_IDLE);
                }
                p.set(P_WRITTEN);
                rescale();
            },
        );
        match serviced {
            Serviced::Updated(_) => {
                p.updates.fetch_add(1, Ordering::Relaxed);
                true
            }
            Serviced::Unacknowledged(_) => {
                p.set(P_UNACKNOWLEDGED);
                true
            }
            Serviced::Quiet | Serviced::Unwritten | Serviced::Faulted => false,
        }
    }

    // --- Capacity --------------------------------------------------------------------------------

    /// Processor `r`'s HFI value, where its package's table has been read and its row lies in it.
    fn hfi_value(r: &Record) -> Option<HfiRow> {
        if !r.has(HFI_MEMBER) {
            return None;
        }
        let p = slot_package(r.package.load(Ordering::Acquire))?;
        if !p.usable() || p.state() & P_WRITTEN == 0 {
            return None;
        }
        let performance = u8::try_from(r.performance.load(Ordering::Relaxed)).ok()?;
        let efficiency = match r.efficiency.load(Ordering::Relaxed) {
            NO_VALUE => None,
            e => u8::try_from(e).ok(),
        };
        Some(HfiRow {
            performance,
            efficiency,
        })
    }

    /// [`crate::capacity::flags`] for processor `r`, from its HFI value.
    fn flags_of(r: &Record, row: Option<HfiRow>) -> u8 {
        use crate::capacity::flags;
        let Some(row) = row else {
            return 0;
        };
        let force_idle = slot_package(r.package.load(Ordering::Acquire))
            .is_some_and(|p| p.state() & P_FORCE_IDLE != 0);
        let mut f = 0;
        if row.avoid() {
            f |= flags::AVOID;
            if force_idle {
                f |= flags::FORCE_IDLE;
            }
        }
        if row.consolidate() {
            f |= flags::CONSOLIDATE;
        }
        f
    }

    /// Chooses every online processor's capacity again from what each reports, and publishes it
    /// (`capacity::choose_into`): O(processors), no allocation, under the rescale's lock. A
    /// processor that has not started gives no source a value, so "every processor or none" covers
    /// it (the review's R106-8). Nothing before the live store starts.
    fn rescale() -> Option<Source> {
        let mut guard = RESCALE.lock();
        let s = guard.as_mut()?;
        let n = s.online.len();
        for (j, &i) in s.online.iter().enumerate() {
            let r = slot(i).map(|s| &s.record).filter(|r| r.has(STARTED));
            let (Some(hfi), Some(register)) = (s.hfi.get_mut(j), s.register.get_mut(j)) else {
                break;
            };
            *hfi = r.and_then(hfi_value).map(|row| u32::from(row.performance));
            let highest = r.map_or(0, |r| r.highest.load(Ordering::Relaxed));
            *register = (highest > 0).then_some(highest);
        }
        let reports = Reports {
            hfi: s.hfi.get(..n),
            cpc: None,
            register: s.register.get(..n),
        };
        let out = s.out.get_mut(..n)?;
        let source = crate::capacity::choose_into(&reports, out);
        for (&cpu, &capacity) in s.online.iter().zip(out.iter()) {
            let flags = slot(cpu).map_or(0, |s| flags_of(&s.record, hfi_value(&s.record)));
            crate::capacity::LIVE.publish(cpu, capacity, flags);
        }
        SOURCE.store(source as u8, Ordering::Relaxed);
        Some(source)
    }

    /// Starts the live store (`capacity::LIVE`) and the rescale's buffers for the processors
    /// `online`, once, and publishes their capacity as the boot reads it now: what the domains
    /// proof keeps as the boot value (the coordinating session's ruling Q6). It first waits, up to
    /// `within_ns` of `now_ns`, for every one of them to have started (R106-8). The source chosen
    /// and how many had started, or `None` if the store had started already.
    pub fn start_capacity(
        online: &[cpu::CpuIndex],
        now_ns: impl Fn() -> u64,
        within_ns: u64,
    ) -> Option<(Source, usize)> {
        let started = || {
            online
                .iter()
                .filter(|c| slot(c.get()).is_some_and(|s| s.record.has(STARTED)))
                .count()
        };
        let begun = now_ns();
        while started() < online.len() && now_ns().saturating_sub(begun) < within_ns {
            core::hint::spin_loop();
        }
        let boot = alloc::vec![crate::capacity::FULL; cpu::count()];
        if !crate::capacity::LIVE.start(&boot) {
            return None;
        }
        let n = online.len();
        let scratch = Scratch {
            online: online.iter().map(|c| c.get()).collect(),
            hfi: alloc::vec![None; n].into_boxed_slice(),
            register: alloc::vec![None; n].into_boxed_slice(),
            out: alloc::vec![0; n].into_boxed_slice(),
        };
        cpu::without_interrupts(|| *RESCALE.lock() = Some(scratch));
        let source = cpu::without_interrupts(rescale)?;
        Some((source, started()))
    }

    /// The source the last rescale chose.
    pub fn source() -> Source {
        match SOURCE.load(Ordering::Relaxed) {
            0 => Source::Hfi,
            1 => Source::Cpc,
            2 => Source::Register,
            _ => Source::Equal,
        }
    }

    // --- The round ---------------------------------------------------------------------------------

    /// The round (the module's documentation): decides which packages are taken, sends the thermal
    /// vector to each processor of `cpus` once and waits, opening interrupt windows on this
    /// processor, until each has taken it; then asks the first processor of each package with the
    /// HFI whose every LVT was taken to enable it, and waits for each. Each wait ends at `within_ns`
    /// of `now_ns`. `flow` is the boot flow's witness: the round may be switched out while it waits.
    pub fn round(
        flow: crate::trap::Pinned<'_>,
        cpus: &[cpu::CpuIndex],
        now_ns: impl Fn() -> u64,
        within_ns: u64,
    ) -> Round {
        let mut found = Round {
            registered: REGISTERED.load(Ordering::Acquire) == 1,
            ..Round::default()
        };
        let begun = now_ns();
        // Step 1: which packages are taken, and their tables.
        for k in 0..PACKAGES.load(Ordering::Acquire) {
            let Some(p) = slot_package(k) else {
                continue;
            };
            let wanted = members_of(k).any(|r| r.has(WANTS_LVT));
            if !wanted || p.state() & (P_DISAGREE | P_NO_LVT) != 0 {
                continue;
            }
            if p.has_hfi() {
                let pages = p.pages.load(Ordering::Relaxed);
                match crate::memory::allocate_table_run(pages) {
                    Some(table) => {
                        let bytes = usize::try_from(pages * PAGE).unwrap_or(0);
                        let at = core::ptr::with_exposed_provenance_mut::<u8>(
                            crate::phys::to_virt(table) as usize,
                        );
                        // SAFETY: `table` is a run of `pages` frames given up for ever to this
                        // package's table: nothing else holds them or maps them, and the direct
                        // map makes them writable at `at`. No processor has its address yet.
                        unsafe { at.write_bytes(0, bytes) };
                        p.table.store(table, Ordering::Release);
                        p.set(P_HFI);
                    }
                    None => p.set(P_NO_MEMORY),
                }
            }
            p.set(P_TAKEN);
            found.taken += 1;
        }
        if !found.registered {
            return found;
        }
        // Step 2: every processor takes its own sources, in its handler.
        let before = taken_counts(cpus);
        for c in cpus {
            if c.apic_id()
                .is_some_and(|apic| timer::send_thermal_vector(cpu::this_cpu(flow.here()), apic))
            {
                found.sent += 1;
            }
        }
        wait_until(&now_ns, begun, within_ns, || {
            cpus.iter()
                .zip(&before)
                .all(|(c, b)| counts_of(c.get()).0 > b.0)
        });
        for (c, b) in cpus.iter().zip(&before) {
            let (t, s) = counts_of(c.get());
            if t > b.0 {
                found.handled += 1;
                found.sourced += u32::from(s > b.1);
            }
        }
        // Step 3: the HFI, enabled by each package's first processor once every LVT is taken.
        let mut asked = alloc::vec::Vec::new();
        for k in 0..PACKAGES.load(Ordering::Acquire) {
            let Some(p) = slot_package(k) else {
                continue;
            };
            let all_taken = members_of(k).all(|r| r.has(LVT_TAKEN));
            let first = p.first.load(Ordering::Acquire);
            if p.state() & P_HFI == 0 || !all_taken {
                continue;
            }
            let Some(apic) = cpu::CpuIndex::new(first).and_then(cpu::CpuIndex::apic_id) else {
                continue;
            };
            p.set(P_ENABLE);
            found.asked += 1;
            if timer::send_thermal_vector(cpu::this_cpu(flow.here()), apic) {
                asked.push(k);
            }
        }
        wait_until(&now_ns, begun, within_ns, || {
            asked
                .iter()
                .all(|&k| slot_package(k).is_some_and(|p| p.state() & P_ENABLE == 0))
        });
        // A package whose first processor has not started the enable by now never enables: the
        // request is taken back, so no late handler enables it, and the HFI stays off there.
        for &k in &asked {
            let Some(p) = slot_package(k) else {
                continue;
            };
            if p.state.fetch_and(!P_ENABLE, Ordering::AcqRel) & P_ENABLE == 0 {
                found.answered += 1;
            } else if found.unanswered.is_none() {
                found.unanswered = Some(p.id.load(Ordering::Relaxed));
            }
        }
        found.waited_ns = now_ns().saturating_sub(begun);
        found
    }

    /// The records of package slot `k`'s processors.
    fn members_of(k: usize) -> impl Iterator<Item = &'static Record> {
        (0..cpu::count()).filter_map(move |i| {
            let r = &slot(i)?.record;
            (r.package.load(Ordering::Acquire) == k).then_some(r)
        })
    }

    /// Processor `i`'s thermal interrupts taken, and of them those with a source.
    fn counts_of(i: usize) -> (u64, u64) {
        slot(i).map_or((0, 0), |s| {
            (
                s.record.taken.load(Ordering::Relaxed),
                s.record.sourced.load(Ordering::Relaxed),
            )
        })
    }

    fn taken_counts(cpus: &[cpu::CpuIndex]) -> alloc::vec::Vec<(u64, u64)> {
        cpus.iter().map(|c| counts_of(c.get())).collect()
    }

    /// Opens interrupt windows on this processor until `done`, or until `within_ns` past `begun`.
    fn wait_until(now_ns: &impl Fn() -> u64, begun: u64, within_ns: u64, done: impl Fn() -> bool) {
        cpu::without_interrupts(|| {
            while !done() && now_ns().saturating_sub(begun) < within_ns {
                cpu::interrupt_window();
            }
        });
    }

    /// Whether an RDMSR of [`RESERVED_X2APIC`] raised #GP and the probe recovered it: the #GP path
    /// of `cpu::probe_rdmsr`, shown on every boot where the processor faults there. `None` where
    /// this processor's local APIC is not in x2APIC mode: it is not run there. A read that returns
    /// a value shows nothing, and is no failure (the coordinating session's ruling of 2026-11-22).
    pub fn reserved_x2apic_read_faults(me: ThisCpu<'_>) -> Option<bool> {
        X2Apic::here(me)?;
        // SAFETY: CPL 0, and an access to a reserved register has no effect but the #GP; a
        // hypervisor that answered anyway gave a value, which is dropped.
        Some(unsafe { cpu::probe_rdmsr(RESERVED_X2APIC) }.is_none())
    }

    // --- The census ------------------------------------------------------------------------------

    /// Counts what every processor of `cpus`, and every package, reported and set.
    pub fn census(cpus: &[cpu::CpuIndex]) -> Census {
        let mut c = Census::default();
        let count = |bit: bool| u32::from(bit);
        for i in cpus {
            let Some(r) = slot(i.get()).map(|s| &s.record) else {
                continue;
            };
            let (state, words) = (r.state(), r.words());
            let used = slot_package(r.package.load(Ordering::Acquire)).is_some_and(Package::usable);
            c.processors += 1;
            c.started += count(state & STARTED != 0);
            c.thermal_registers += count(words.thermal_registers());
            c.sensors += count(words.heat());
            c.lvt_free += count(state & LVT_FREE != 0);
            c.lvt_firmware += count(state & LVT_FIRMWARE != 0);
            c.lvt_taken += count(state & LVT_TAKEN != 0);
            c.other_enables += count(state & OTHER_ENABLES != 0);
            c.heat += count(state & HEAT != 0);
            c.hot += count(state & (HEAT | HOT) == HEAT | HOT);
            c.hfi += count(words.hfi());
            c.hfi_members += count(state & HFI_MEMBER != 0 && used);
            c.rows_past += count(state & ROW_PAST != 0);
            c.hwp += count(state & HWP_PRESENT != 0);
            c.hwp_on += count(state & HWP_ON != 0);
            c.hwp_notify += count(state & HWP_NOTIFY != 0);
            c.cppc += count(state & CPPC_PRESENT != 0);
            c.cppc_on += count(state & CPPC_ON != 0);
            c.no_topology += count(state & NO_TOPOLOGY != 0);
            c.faulted += count(state & FAULTED != 0);
            c.mismatched += count(state & MISMATCH != 0);
            c.quiet += r.quiet.load(Ordering::Relaxed);
            if c.first_fault.is_none() {
                c.first_fault = Some(r.fault.load(Ordering::Relaxed)).filter(|&m| m != 0);
            }
        }
        for k in 0..PACKAGES.load(Ordering::Acquire) {
            let Some(p) = slot_package(k) else {
                continue;
            };
            let state = p.state();
            c.packages += 1;
            c.taken += count(state & P_TAKEN != 0);
            c.enabled += count(state & P_ENABLED != 0);
            c.written += count(state & (P_ENABLED | P_WRITTEN) == P_ENABLED | P_WRITTEN);
            c.disagree += count(state & P_DISAGREE != 0);
            c.no_lvt += count(state & P_NO_LVT != 0);
            c.firmware_hfi += count(state & P_FIRMWARE_HFI != 0);
            c.other_package_enables += count(state & P_OTHER_ENABLES != 0);
            c.failed += count(state & P_FAILED != 0);
            c.no_memory += count(state & P_NO_MEMORY != 0);
            c.unacknowledged += count(state & P_UNACKNOWLEDGED != 0);
            c.updates += p.updates.load(Ordering::Relaxed);
        }
        c
    }
}

#[cfg(test)]
mod tests;
