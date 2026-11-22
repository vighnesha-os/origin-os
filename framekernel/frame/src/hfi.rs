//! The thermal LVT's sources: Intel's Hardware Feedback Interface (ADR-0020 S9, source 1), the
//! performance-capability registers HWP and AMD CPPC report where the firmware enabled them
//! (source 3), and Intel's core thermal status (S17's heat). TASKS item 106.
//!
//! **What is read, and where.** Every processor runs `start_this_processor` on itself once, as it
//! starts (`smp::ap_entry`, and the boot processor's path after it records its topology). It reads
//! CPUID.06H, 01H and 8000_0008H, and then, only where CPUID claims them, its thermal LVT entry and
//! the registers below, each through the MSR probe (`cpu::probe_rdmsr`): a register CPUID claims
//! that faults, as under a hypervisor that does not emulate it, is reported and that source is
//! not used (the coordinating session's ruling Q4 of 2026-11-21).
//!
//! - **The thermal LVT** is taken only where the firmware left it masked, and not for SMI
//!   ([`crate::apic::lvt_thermal_free`]): fixed delivery to `cpu::THERMAL_VECTOR`. Where the
//!   firmware holds it, no thermal source is used on that processor's package (ruling Q5).
//! - **The HFI** is enabled on a package by the first of its processors to arrive, and used only
//!   while every processor of the package reports the same table and holds its LVT ([`Words`]).
//!   The table is a run of frames given up for ever (`memory::allocate_table_run`), zeroed before
//!   IA32_HW_FEEDBACK_PTR names it, reached only through the direct map and never mapped to a
//!   program; it is never disabled at M3, so it is never freed. Only IA32_HW_FEEDBACK_CONFIG's
//!   bit 0 is set: Thread Director's bit 1 never is.
//! - **Heat:** IA32_THERM_INTERRUPT's high- and low-temperature enables (bits 0 and 1), read,
//!   modified and written, the thresholds left as the firmware set them (ruling Q12). Each
//!   transition is one interrupt, whose handler reads IA32_THERM_STATUS bit 0 into the processor's
//!   "throttled" flag ([`crate::capacity::throttled`]). AMD documents no thermal event the
//!   operating system enables: AMD heat is not supported at M3.
//! - **HWP and AMD CPPC** are never enabled, and no enable or request register is ever written
//!   (ruling Q2): their capability registers read zero, or may not be read, until they are. Highest
//!   is read only where IA32_PM_ENABLE or CPPC_ENABLE already reads 1. Where HWP also reports
//!   Highest-change interrupts, IA32_HWP_INTERRUPT's bit 2 is set, and the handler reads Highest
//!   again and clears IA32_HWP_STATUS's bit 3, which Intel says software must clear for the next
//!   notification (SDM Vol. 3, §17.4.5 and §17.4.6).
//!
//! **The notification** ([`service`]): IA32_PACKAGE_THERM_STATUS bit 26 says the processor wrote
//! the package's table, and it writes no more while the bit is set (SDM Vol. 3, §17.6.6). Under the
//! package's lock, with interrupts off, the handler reads the bit; if set, it copies the flags and
//! every member's row, rescales every processor's capacity, and only then acknowledges, by writing
//! the status with bit 26 cleared, every read-only bit as read, every sticky log bit as 1 (which
//! leaves it as it is) and every reserved bit as 0 ([`acknowledgement`]). Which processor takes a
//! package's interrupt is not stated, so every member holds its LVT unmasked, and whichever takes
//! it first does the work: the others find bit 26 clear. Until a package's first update is read,
//! its processors give no value (R97-14): an unwritten table never reads as "avoid".
//!
//! **The round** (`round`): after every processor has started, the boot sends the thermal vector
//! to each, once. Each takes it through the same handler: the catch-up for a notification that
//! arrived before an LVT was unmasked, and, on a package with the HFI, the read-back of its pointer
//! and configuration that SDM -093 §17.6.9 asks for "when processing a SIPI event" (ruling Q11).
//! Under QEMU, where no source exists, it shows the vector's registration, dispatch and the
//! handler's absence path on every processor.
//!
//! **The writes**, all of them (the contract's security invariant 6): IA32_HW_FEEDBACK_PTR,
//! IA32_HW_FEEDBACK_CONFIG bit 0, IA32_PACKAGE_THERM_INTERRUPT bit 25, the bit-26
//! acknowledgement, IA32_THERM_INTERRUPT bits 0 and 1, the thermal LVT entry where free,
//! IA32_HWP_INTERRUPT bit 2 and IA32_HWP_STATUS bit 3's clearing where HWP is already on.
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

/// What the notification needs of a package's hardware: its status register and its table.
pub trait PackageHardware {
    /// IA32_PACKAGE_THERM_STATUS, or `None` if the read faulted.
    fn status(&mut self) -> Option<u64>;
    /// Writes IA32_PACKAGE_THERM_STATUS; `false` if the write faulted.
    fn write_status(&mut self, value: u64) -> bool;
    /// Copies `out.len()` bytes of the table from byte `at`; `false`, with nothing copied, if they
    /// do not lie inside it.
    fn copy(&mut self, at: usize, out: &mut [u8]) -> bool;
}

/// What one service of a package found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Serviced {
    /// Bit 26 was clear: nothing was written since the last acknowledgement.
    Quiet,
    /// The table was read, and the update acknowledged.
    Updated(HfiFlags),
    /// The table was read, but the acknowledgement faulted: no further update will come.
    Unacknowledged(HfiFlags),
    /// The status could not be read, or its flags could not be copied.
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
    if hw.write_status(acknowledgement(status)) {
        Serviced::Updated(flags)
    } else {
        Serviced::Unacknowledged(flags)
    }
}

/// What the round found (`round`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Round {
    /// Processors the thermal vector was sent to.
    pub sent: u32,
    /// Of them, those that took it before the deadline.
    pub handled: u32,
    /// Of those, those that had a source to read.
    pub sourced: u32,
    /// Packages whose pointer and configuration the round asked to be read back.
    pub asked: u32,
    /// Of them, those read back.
    pub read_back: u32,
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
    /// Those whose thermal LVT the kernel took.
    pub lvt_taken: u32,
    /// Those whose thermal LVT the firmware holds.
    pub lvt_firmware: u32,
    /// Those whose thermal transitions interrupt them.
    pub heat: u32,
    /// Of them, those hot when last read.
    pub hot: u32,
    /// Those whose CPUID reports the HFI.
    pub hfi: u32,
    /// Those that read their row of an HFI table.
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
    /// Packages found.
    pub packages: u32,
    /// Of them, those whose HFI the kernel enabled.
    pub enabled: u32,
    /// Of those, those whose table the processor has written.
    pub written: u32,
    /// Packages whose processors disagree on the table.
    pub disagree: u32,
    /// Packages with a processor that reports the HFI and whose LVT is not the kernel's.
    pub no_lvt: u32,
    /// Packages whose firmware had the HFI on.
    pub firmware_hfi: u32,
    /// Packages whose enable faulted or did not read back.
    pub failed: u32,
    /// Packages with no frames for a table.
    pub no_memory: u32,
    /// Packages the round enabled again.
    pub reenabled: u32,
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

/// Every CPU index's slot (ADR-0008 Amendment 1, D1): its processor's record, and the package its
/// index numbers among the packages found.
#[cfg(target_os = "none")]
pub(crate) static SLOTS: crate::percpu::PerCpu<kernel::Slot> = crate::percpu::PerCpu::new(
    "origin_frame::hfi::SLOTS",
    kernel::Slot::new(),
    kernel::Slot::new,
);

#[cfg(target_os = "none")]
pub use kernel::{census, round, source, start_capacity, start_this_processor};

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

    // --- The kernel's state ------------------------------------------------------------------------

    /// A row value no row gave.
    const NO_VALUE: u16 = u16::MAX;
    /// The package slot of a processor in none.
    const NO_PACKAGE: usize = usize::MAX;

    // A processor's state.
    /// [`start_this_processor`] has run on it.
    const STARTED: u32 = 1;
    /// The kernel holds its thermal LVT entry.
    const LVT_TAKEN: u32 = 1 << 1;
    /// The firmware holds its thermal LVT entry: unmasked, or for SMI.
    const LVT_FIRMWARE: u32 = 1 << 2;
    /// Its core's thermal transitions interrupt it (IA32_THERM_INTERRUPT bits 0 and 1).
    const HEAT: u32 = 1 << 3;
    /// IA32_THERM_STATUS bit 0 was set when last read: its core is hot.
    const HOT: u32 = 1 << 4;
    /// CPUID claims HWP.
    const HWP_PRESENT: u32 = 1 << 5;
    /// The firmware had HWP on.
    const HWP_ON: u32 = 1 << 6;
    /// Highest changes interrupt it (IA32_HWP_INTERRUPT bit 2).
    const HWP_NOTIFY: u32 = 1 << 7;
    /// CPUID claims AMD's CPPC.
    const CPPC_PRESENT: u32 = 1 << 8;
    /// The firmware had CPPC on.
    const CPPC_ON: u32 = 1 << 9;
    /// It reads its row of its package's HFI table.
    const HFI_MEMBER: u32 = 1 << 10;
    /// Its row lies past its package's table: it gives no value.
    const ROW_PAST: u32 = 1 << 11;
    /// A register CPUID claims raised #GP.
    const FAULTED: u32 = 1 << 12;
    /// An enable it wrote did not read back.
    const MISMATCH: u32 = 1 << 13;
    /// It recorded no topology, so no package.
    const NO_TOPOLOGY: u32 = 1 << 14;

    // A package's state.
    /// Its HFI was enabled here, and every write read back.
    const P_ENABLED: u32 = 1;
    /// The processor has written its table at least once, and the update was read.
    const P_WRITTEN: u32 = 1 << 1;
    /// Its processors do not report the same table.
    const P_DISAGREE: u32 = 1 << 2;
    /// A processor of it reports the HFI but its thermal LVT is not the kernel's.
    const P_NO_LVT: u32 = 1 << 3;
    /// The firmware had its HFI enabled already: left as it was, and not used.
    const P_FIRMWARE_HFI: u32 = 1 << 4;
    /// A write faulted or did not read back.
    const P_FAILED: u32 = 1 << 5;
    /// No run of frames for its table.
    const P_NO_MEMORY: u32 = 1 << 6;
    /// The round asks for its pointer and configuration to be read back.
    const P_RECHECK: u32 = 1 << 7;
    /// The round found them changed, and wrote them again.
    const P_REENABLED: u32 = 1 << 8;
    /// An acknowledgement faulted: no further update will come.
    const P_UNACKNOWLEDGED: u32 = 1 << 9;
    /// Its latest update asked for idle to be forced (Table 17-5's bit 1).
    const P_FORCE_IDLE: u32 = 1 << 10;
    /// The bits that keep a package's HFI from being used.
    const P_UNUSABLE: u32 = P_DISAGREE | P_NO_LVT | P_FIRMWARE_HFI | P_FAILED | P_NO_MEMORY;

    /// One processor's record.
    #[derive(Debug)]
    struct Record {
        /// Its CPUID words, as [`Words`] in order.
        words: [AtomicU32; 5],
        /// Its state bits.
        state: AtomicU32,
        /// Its package's slot, or [`NO_PACKAGE`].
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
        /// The first register CPUID claimed that raised #GP, or 0.
        fault: AtomicU32,
    }

    impl Record {
        const fn new() -> Self {
            Self {
                words: [const { AtomicU32::new(0) }; 5],
                state: AtomicU32::new(0),
                package: AtomicUsize::new(NO_PACKAGE),
                performance: AtomicU16::new(NO_VALUE),
                efficiency: AtomicU16::new(NO_VALUE),
                highest: AtomicU32::new(0),
                taken: AtomicU64::new(0),
                sourced: AtomicU64::new(0),
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
    #[derive(Debug)]
    struct Package {
        /// Its package ID (`topology::Placement::package`).
        id: AtomicU32,
        /// Its first processor's [`Words::package_signature`].
        signature: AtomicU64,
        /// Its state bits.
        state: AtomicU32,
        /// Its table's physical address, or 0.
        table: AtomicU64,
        /// Its table's size in pages.
        pages: AtomicU64,
        /// Updates read and acknowledged.
        updates: AtomicU64,
        /// Held while its notification is serviced, with interrupts off.
        lock: SpinLock<()>,
    }

    impl Package {
        const fn new() -> Self {
            Self {
                id: AtomicU32::new(0),
                signature: AtomicU64::new(0),
                state: AtomicU32::new(0),
                table: AtomicU64::new(0),
                pages: AtomicU64::new(0),
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

        fn usable(&self) -> bool {
            self.state() & (P_ENABLED | P_UNUSABLE) == P_ENABLED
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
    /// Held, with interrupts off, while a processor joins its package (boot only).
    static JOINING: SpinLock<()> = SpinLock::new(());
    /// The handler's registration: 0 not tried, 1 registered, 2 refused.
    static REGISTERED: AtomicU8 = AtomicU8::new(0);
    /// The rescale's buffers, allocated once when the live store starts, and held, with interrupts
    /// off, while a rescale runs: so a rescale allocates nothing.
    static RESCALE: SpinLock<Option<Scratch>> = SpinLock::new(None);
    /// The source the last rescale chose, as [`Source`] in order: 0 HFI, 1 `_CPC`, 2 register,
    /// 3 equal.
    static SOURCE: AtomicU8 = AtomicU8::new(3);

    /// The rescale's buffers, one entry per CPU index.
    struct Scratch {
        cpus: Box<[usize]>,
        hfi: Box<[Option<u32>]>,
        register: Box<[Option<u32>]>,
        out: Box<[u16]>,
    }

    // --- The kernel's paths -----------------------------------------------------------------------

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

    /// Reads `msr` through the probe, recording a fault against `r`.
    fn read(r: &Record, msr: u32) -> Option<u64> {
        // SAFETY: CPL 0. Every register this module reads has no read side effect (SDM Vol. 4, Table
        // 2-2; APM Vol. 2, §17.6), and is read only where CPUID claims it; one that raises #GP is
        // recovered and recorded.
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
    /// As [`cpu::probe_wrmsr`]: the write's effect, where it is taken, is one the caller intends and
    /// keeps memory-safe.
    unsafe fn write(r: &Record, msr: u32, value: u64) -> bool {
        // SAFETY: the caller's contract.
        let written = unsafe { cpu::probe_wrmsr(msr, value) };
        if !written {
            r.fault(msr);
        }
        written
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

    /// Reads what this processor reports about the thermal LVT's sources, on itself, and takes those
    /// the module documentation says it takes (TASKS item 106; the coordinating session's ruling Q7).
    /// Called once by each processor as it starts: `me` is its start-up flow's index. A second call
    /// does nothing.
    pub fn start_this_processor(me: ThisCpu<'_>) {
        let Some(slot) = SLOTS.this(me) else {
            return;
        };
        let r = &slot.record;
        if r.state() & STARTED != 0 {
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
        // The handler is registered on every processor's start, whatever its sources, so that the
        // round reaches it everywhere.
        let handler = registered();

        // The thermal LVT, where CPUID says it exists: taken only where the firmware left it free.
        let mut taken = false;
        if words.thermal_registers()
            && handler
            && let Some(lapic) = X2Apic::here(me)
        {
            match lapic.thermal_entry() {
                None => r.fault(apic::X2APIC_LVT_THERMAL),
                Some(entry) if apic::lvt_thermal_free(entry) => {
                    taken = lapic.take_thermal(cpu::THERMAL_VECTOR);
                    r.set(if taken { LVT_TAKEN } else { MISMATCH });
                }
                Some(_) => r.set(LVT_FIRMWARE),
            }
        }

        // Heat: this core's high- and low-temperature transitions, through the LVT the kernel holds.
        if taken && let Some(before) = read(r, IA32_THERM_INTERRUPT) {
            let wanted = before | THERM_INTERRUPT_ENABLES;
            // SAFETY: bits 0 and 1 only ask this core for an interrupt at its high- and low-temperature
            // transitions, through the LVT entry the kernel now holds; the thresholds and every other
            // bit are written as read.
            let held = unsafe { write(r, IA32_THERM_INTERRUPT, wanted) }
                && read(r, IA32_THERM_INTERRUPT)
                    .is_some_and(|v| v & THERM_INTERRUPT_ENABLES == THERM_INTERRUPT_ENABLES);
            if held {
                r.set(HEAT);
                if read(r, IA32_THERM_STATUS).is_some_and(hot) {
                    r.set(HOT);
                }
            } else {
                r.set(MISMATCH);
            }
        }

        // HWP, never enabled here: Highest only where the firmware turned it on.
        if words.hwp() {
            r.set(HWP_PRESENT);
            if read(r, IA32_PM_ENABLE).is_some_and(|v| v & ENABLED != 0) {
                r.set(HWP_ON);
                if let Some(c) = read(r, IA32_HWP_CAPABILITIES) {
                    r.highest
                        .store(crate::capacity::hwp_highest(c), Ordering::Relaxed);
                }
                if taken
                    && words.hwp_highest_interrupt()
                    && let Some(before) = read(r, IA32_HWP_INTERRUPT)
                {
                    // SAFETY: bit 2 only asks for an interrupt, through the LVT entry the kernel
                    // holds, when Highest changes; every other bit is written as read.
                    let held =
                        unsafe { write(r, IA32_HWP_INTERRUPT, before | HWP_EN_HIGHEST_CHANGE) }
                            && read(r, IA32_HWP_INTERRUPT)
                                .is_some_and(|v| v & HWP_EN_HIGHEST_CHANGE != 0);
                    r.set(if held { HWP_NOTIFY } else { MISMATCH });
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

        // Its package, and the package's HFI.
        match crate::smp::topology_of(me.index()) {
            Some(t) => join(r, t.place().package, words, taken),
            None => r.set(NO_TOPOLOGY),
        }
        r.set(STARTED);
    }

    /// Puts the processor of `r` in package `id`'s slot, finding it or making it, and checks it
    /// against the package's first processor. The first processor of a package that can use the HFI,
    /// and holds its LVT, enables it ([`enable`]).
    fn join(r: &Record, id: u32, words: Words, taken: bool) {
        let signature = words.package_signature();
        let joined = cpu::without_interrupts(|| {
            let _joining = JOINING.lock();
            let count = PACKAGES.load(Ordering::Acquire);
            let found = (0..count).find(|&k| {
                SLOTS
                    .get(k)
                    .is_some_and(|s| s.package.id.load(Ordering::Relaxed) == id)
            });
            let (k, first) = match found {
                Some(k) => (k, false),
                None => (count, true),
            };
            let p = &SLOTS.get(k)?.package;
            if first {
                p.id.store(id, Ordering::Relaxed);
                p.signature.store(signature, Ordering::Relaxed);
                PACKAGES.store(count + 1, Ordering::Release);
            } else if p.signature.load(Ordering::Relaxed) != signature {
                p.set(P_DISAGREE);
            }
            if words.hfi() && !taken {
                p.set(P_NO_LVT);
            }
            if first && taken && words.hfi_usable() {
                enable(r, p, words);
            }
            Some(k)
        });
        let Some(k) = joined else {
            return;
        };
        r.package.store(k, Ordering::Release);
        if taken && let Some(shape) = words.shape().filter(|_| words.hfi_usable()) {
            r.set(if words.row_fits(shape) {
                HFI_MEMBER
            } else {
                ROW_PAST
            });
        }
    }

    /// Enables the HFI of package `p` from its first processor, `r`'s: a zeroed table, its pointer,
    /// configuration bit 0, and the package interrupt's bit 25, each read back.
    fn enable(r: &Record, p: &Package, words: Words) {
        match read(r, IA32_HW_FEEDBACK_CONFIG) {
            None => return p.set(P_FAILED),
            // The firmware's, if it enabled it: left alone, and not used.
            Some(c) if c & FEEDBACK_ENABLE != 0 => return p.set(P_FIRMWARE_HFI),
            Some(_) => {}
        }
        let pages = words.pages();
        let Some(table) = crate::memory::allocate_table_run(pages) else {
            return p.set(P_NO_MEMORY);
        };
        let bytes = usize::try_from(pages * PAGE).unwrap_or(0);
        let at = core::ptr::with_exposed_provenance_mut::<u8>(crate::phys::to_virt(table) as usize);
        // SAFETY: `table` is a run of `pages` frames given up for ever to this package's table: nothing
        // else holds them or maps them, and the direct map makes them writable at `at`.
        unsafe { at.write_bytes(0, bytes) };
        p.table.store(table, Ordering::Relaxed);
        p.pages.store(pages, Ordering::Relaxed);
        let pointer = feedback_pointer(table);
        // SAFETY: the pointer names the zeroed table above, which the processor may write from now on
        // and nothing else uses, ever.
        let mut done = unsafe { write(r, IA32_HW_FEEDBACK_PTR, pointer) };
        // SAFETY: bit 0 alone turns the HFI on, writing the table the pointer names; Thread Director's
        // bit 1 stays clear.
        done = done && unsafe { write(r, IA32_HW_FEEDBACK_CONFIG, FEEDBACK_ENABLE) };
        done = done
            && read(r, IA32_PACKAGE_THERM_INTERRUPT).is_some_and(|before| {
                // SAFETY: bit 25 only asks for an interrupt, through the LVT entries the kernel holds,
                // when the table is written; every other bit is written as read.
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

    /// The thermal interrupt's handler (`irq::register`'s, on [`cpu::THERMAL_VECTOR`]): heat, HWP's
    /// Highest and the package's table, each only where this processor holds that source. Allocates
    /// nothing, and holds no lock but its package's and the rescale's, with interrupts off.
    fn thermal(here: Here<'_>) -> Action {
        let me = cpu::this_cpu(here);
        let index = me.index().get();
        let Some(slot) = SLOTS.this(me) else {
            return Action::Resume;
        };
        let r = &slot.record;
        r.taken.fetch_add(1, Ordering::Relaxed);
        let state = r.state();
        let mut sourced = false;
        if state & HEAT != 0 {
            sourced = true;
            if let Some(status) = read(r, IA32_THERM_STATUS) {
                let h = hot(status);
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
                if let Some(c) = read(r, IA32_HWP_CAPABILITIES) {
                    r.highest
                        .store(crate::capacity::hwp_highest(c), Ordering::Relaxed);
                }
                // SAFETY: clears Highest_Change alone, which Intel says software must clear for the
                // next change to interrupt; the other status bits are written as 1, which leaves them.
                unsafe { write(r, IA32_HWP_STATUS, hwp_acknowledgement()) };
                rescale();
            }
        }
        if state & HFI_MEMBER != 0 {
            sourced = true;
            let k = r.package.load(Ordering::Acquire);
            if let Some(p) = SLOTS.get(k).map(|s| &s.package) {
                check(r, k, p);
            }
        }
        if sourced {
            r.sourced.fetch_add(1, Ordering::Relaxed);
        }
        Action::Resume
    }

    /// Package `p`'s table, as the notification reads it: the status register on this processor, and
    /// the table through the direct map, byte by byte and volatile, as memory a device writes.
    struct Table<'a> {
        record: &'a Record,
        table: u64,
        size: u64,
    }

    impl PackageHardware for Table<'_> {
        fn status(&mut self) -> Option<u64> {
            read(self.record, IA32_PACKAGE_THERM_STATUS)
        }

        fn write_status(&mut self, value: u64) -> bool {
            // SAFETY: the acknowledgement clears bit 26 alone: the log bits are written as 1, which
            // leaves them, the read-only bits as read and the reserved bits as 0 (`acknowledgement`).
            unsafe { write(self.record, IA32_PACKAGE_THERM_STATUS, value) }
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
                // SAFETY: `at + i` lies inside the table (checked above), a run of frames this module
                // holds for ever and the direct map maps; the processor writes it only while bit 26
                // is clear, and this reads it only while bit 26 is set.
                *byte = unsafe { base.add(at + i).read_volatile() };
            }
            true
        }
    }

    /// The members of package slot `k`, with their rows: every processor that reads its row there.
    fn members(k: usize) -> impl Iterator<Item = (usize, u16)> {
        (0..cpu::count()).filter_map(move |i| {
            let r = &SLOTS.get(i)?.record;
            (r.state() & HFI_MEMBER != 0 && r.package.load(Ordering::Acquire) == k)
                .then(|| (i, r.words().row()))
        })
    }

    /// Services package `p`'s notification, from `r`'s processor, under the package's lock.
    fn check(r: &Record, k: usize, p: &Package) {
        let _held = p.lock.lock();
        if !p.usable() {
            return;
        }
        if p.state.fetch_and(!P_RECHECK, Ordering::AcqRel) & P_RECHECK != 0 {
            recheck(r, p);
        }
        let Some(shape) = r.words().shape() else {
            return;
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
                if let Some(m) = SLOTS.get(cpu).map(|s| &s.record) {
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
                if flags.performance_force_idle || flags.efficiency_force_idle {
                    p.set(P_FORCE_IDLE);
                } else {
                    p.state.fetch_and(!P_FORCE_IDLE, Ordering::AcqRel);
                }
                p.set(P_WRITTEN);
                rescale();
            },
        );
        match serviced {
            Serviced::Updated(_) => {
                p.updates.fetch_add(1, Ordering::Relaxed);
            }
            Serviced::Unacknowledged(_) => p.set(P_UNACKNOWLEDGED),
            Serviced::Quiet | Serviced::Faulted => {}
        }
    }

    /// The read-back the round asks for (SDM -093 §17.6.9; the coordinating session's ruling Q11):
    /// package `p`'s pointer and configuration, written again where they no longer read as written.
    fn recheck(r: &Record, p: &Package) {
        let pointer = feedback_pointer(p.table.load(Ordering::Relaxed));
        let held = read(r, IA32_HW_FEEDBACK_PTR) == Some(pointer)
            && read(r, IA32_HW_FEEDBACK_CONFIG).is_some_and(|c| c & 0b11 == FEEDBACK_ENABLE);
        if held {
            return;
        }
        // SAFETY: as when it was enabled: the same table, which nothing else ever uses, and bit 0
        // alone.
        let again = unsafe { write(r, IA32_HW_FEEDBACK_PTR, pointer) }
            && unsafe { write(r, IA32_HW_FEEDBACK_CONFIG, FEEDBACK_ENABLE) };
        p.set(if again { P_REENABLED } else { P_FAILED });
    }

    /// Processor `r`'s HFI value, where its package's table has been read and its row lies in it.
    fn hfi_value(r: &Record) -> Option<HfiRow> {
        if r.state() & HFI_MEMBER == 0 {
            return None;
        }
        let p = &SLOTS.get(r.package.load(Ordering::Acquire))?.package;
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
        let force_idle = SLOTS
            .get(r.package.load(Ordering::Acquire))
            .is_some_and(|s| s.package.state() & P_FORCE_IDLE != 0);
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

    /// Chooses every started processor's capacity again from what each reports, and publishes it
    /// (`capacity::choose_into`): O(processors), no allocation, under the rescale's lock. Nothing
    /// before the live store starts.
    fn rescale() -> Option<Source> {
        let mut guard = RESCALE.lock();
        let s = guard.as_mut()?;
        let mut n = 0;
        for i in 0..s.cpus.len() {
            let Some(r) = SLOTS.get(i).map(|s| &s.record) else {
                continue;
            };
            if r.state() & STARTED == 0 {
                continue;
            }
            let (Some(cpu), Some(hfi), Some(register)) =
                (s.cpus.get_mut(n), s.hfi.get_mut(n), s.register.get_mut(n))
            else {
                break;
            };
            *cpu = i;
            *hfi = hfi_value(r).map(|row| u32::from(row.performance));
            let highest = r.highest.load(Ordering::Relaxed);
            *register = (highest > 0).then_some(highest);
            n += 1;
        }
        let reports = Reports {
            hfi: s.hfi.get(..n),
            cpc: None,
            register: s.register.get(..n),
        };
        let out = s.out.get_mut(..n)?;
        let source = crate::capacity::choose_into(&reports, out);
        for (&cpu, &capacity) in s.cpus.iter().zip(out.iter()) {
            if let Some(r) = SLOTS.get(cpu).map(|s| &s.record) {
                crate::capacity::LIVE.publish(cpu, capacity, flags_of(r, hfi_value(r)));
            }
        }
        SOURCE.store(source as u8, Ordering::Relaxed);
        Some(source)
    }

    /// Starts the live store (`capacity::LIVE`) with every started processor's capacity as the boot
    /// reads it now, and the rescale's buffers, once: what the domains proof publishes as the boot
    /// value (TASKS item 106; the coordinating session's ruling Q6). The source chosen, or `None` if
    /// the store had started already.
    pub fn start_capacity() -> Option<Source> {
        let count = cpu::count();
        let boot = alloc::vec![crate::capacity::FULL; count];
        if !crate::capacity::LIVE.start(&boot) {
            return None;
        }
        let scratch = Scratch {
            cpus: alloc::vec![0; count].into_boxed_slice(),
            hfi: alloc::vec![None; count].into_boxed_slice(),
            register: alloc::vec![None; count].into_boxed_slice(),
            out: alloc::vec![0; count].into_boxed_slice(),
        };
        cpu::without_interrupts(|| *RESCALE.lock() = Some(scratch));
        for i in 0..count {
            if SLOTS
                .get(i)
                .is_some_and(|s| s.record.state() & (HEAT | HOT) == HEAT | HOT)
            {
                crate::capacity::LIVE.set_throttled(i, true);
            }
        }
        cpu::without_interrupts(rescale)
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

    /// The round (the module's documentation): asks every package with the HFI for its read-back,
    /// sends the thermal vector to each processor of `cpus` once, and waits, opening interrupt windows
    /// on this processor, until each has taken it, or `within_ns` of `now_ns` has passed. `flow` is the
    /// boot flow's witness: the round may be switched out while it waits.
    pub fn round(
        flow: crate::trap::Pinned<'_>,
        cpus: &[cpu::CpuIndex],
        now_ns: impl Fn() -> u64,
        within_ns: u64,
    ) -> Round {
        let mut found = Round::default();
        for k in 0..PACKAGES.load(Ordering::Acquire) {
            if let Some(p) = SLOTS.get(k).map(|s| &s.package)
                && p.usable()
            {
                p.set(P_RECHECK);
                found.asked += 1;
            }
        }
        let taken = |c: &cpu::CpuIndex| {
            SLOTS.get(c.get()).map_or((0, 0), |s| {
                (
                    s.record.taken.load(Ordering::Relaxed),
                    s.record.sourced.load(Ordering::Relaxed),
                )
            })
        };
        let before: alloc::vec::Vec<(u64, u64)> = cpus.iter().map(taken).collect();
        for c in cpus {
            let me = cpu::this_cpu(flow.here());
            if c.apic_id()
                .is_some_and(|apic| timer::send_device_vector(me, apic, cpu::THERMAL_VECTOR))
            {
                found.sent += 1;
            }
        }
        let start = now_ns();
        cpu::without_interrupts(|| {
            while !cpus.iter().zip(&before).all(|(c, b)| taken(c).0 > b.0)
                && now_ns().saturating_sub(start) < within_ns
            {
                cpu::interrupt_window();
            }
        });
        for (c, b) in cpus.iter().zip(&before) {
            let (t, s) = taken(c);
            if t > b.0 {
                found.handled += 1;
                if s > b.1 {
                    found.sourced += 1;
                }
            }
        }
        for k in 0..PACKAGES.load(Ordering::Acquire) {
            if let Some(p) = SLOTS.get(k).map(|s| &s.package)
                && p.usable()
                && p.state() & P_RECHECK == 0
            {
                found.read_back += 1;
            }
        }
        found
    }

    /// Counts what every processor of `cpus`, and every package, reported and set.
    pub fn census(cpus: &[cpu::CpuIndex]) -> Census {
        let mut c = Census::default();
        let count = |bit: bool| u32::from(bit);
        for i in cpus {
            let Some(r) = SLOTS.get(i.get()).map(|s| &s.record) else {
                continue;
            };
            let (state, words) = (r.state(), r.words());
            c.processors += 1;
            c.started += count(state & STARTED != 0);
            c.thermal_registers += count(words.thermal_registers());
            c.lvt_taken += count(state & LVT_TAKEN != 0);
            c.lvt_firmware += count(state & LVT_FIRMWARE != 0);
            c.heat += count(state & HEAT != 0);
            c.hot += count(state & (HEAT | HOT) == HEAT | HOT);
            c.hfi += count(words.hfi());
            c.hfi_members += count(state & HFI_MEMBER != 0);
            c.rows_past += count(state & ROW_PAST != 0);
            c.hwp += count(state & HWP_PRESENT != 0);
            c.hwp_on += count(state & HWP_ON != 0);
            c.hwp_notify += count(state & HWP_NOTIFY != 0);
            c.cppc += count(state & CPPC_PRESENT != 0);
            c.cppc_on += count(state & CPPC_ON != 0);
            c.no_topology += count(state & NO_TOPOLOGY != 0);
            c.faulted += count(state & FAULTED != 0);
            c.mismatched += count(state & MISMATCH != 0);
            if c.first_fault.is_none() {
                c.first_fault = Some(r.fault.load(Ordering::Relaxed)).filter(|&m| m != 0);
            }
        }
        for k in 0..PACKAGES.load(Ordering::Acquire) {
            let Some(p) = SLOTS.get(k).map(|s| &s.package) else {
                continue;
            };
            let state = p.state();
            c.packages += 1;
            c.enabled += count(state & P_ENABLED != 0);
            c.written += count(state & (P_ENABLED | P_WRITTEN) == P_ENABLED | P_WRITTEN);
            c.disagree += count(state & P_DISAGREE != 0);
            c.no_lvt += count(state & P_NO_LVT != 0);
            c.firmware_hfi += count(state & P_FIRMWARE_HFI != 0);
            c.failed += count(state & P_FAILED != 0);
            c.no_memory += count(state & P_NO_MEMORY != 0);
            c.reenabled += count(state & P_REENABLED != 0);
            c.unacknowledged += count(state & P_UNACKNOWLEDGED != 0);
            c.updates += p.updates.load(Ordering::Relaxed);
        }
        c
    }
}

#[cfg(test)]
mod tests;
