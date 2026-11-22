//! Bench builds only (TASKS item 112): what the bench loader says about the hand-over, and where.
//!
//! Rentals B and C showed nothing between the loader's last line and the kernel's first. A bench
//! loader says more, around that gap:
//! - before `ExitBootServices`, on the firmware's console, the processor state the kernel
//!   inherits ([`Registers`], then [`Shadow`]), reading only the registers CPUID says exist
//!   ([`reads`]);
//! - after it, straight to the UART boot info names, which call succeeded ([`Exited`]), through
//!   `crate::uart::Uart`, which calls no firmware service (ADR-0002 B5). Every loader, bench or
//!   not, writes there why it halts when both calls fail (`crate::uart::Halting`, decision item
//!   342);
//! - for the QEMU scenarios, `ExitBootServices` given a stale map key once or twice
//!   ([`stale_calls`], [`key_for`]), so the retry, and the halt, run under real firmware code.
//!
//! The entry stub and the kernel write one byte after each step (`RS12345678`).
//!
//! TASKS item 116 adds what separates rental D's two readings, that `ExitBootServices` does not
//! return on the rented firmware or that nothing written straight to its UART after the call
//! reaches the console:
//! - before the call, the UART's registers through the firmware's console ([`UartState`],
//!   [`UnderFirmware`]), then one line straight to the UART ([`BEFORE_CALL`]), and one more just
//!   before each call ([`MARK_FIRST`], [`MARK_RETRY`]);
//! - after it, the registers read again, the firmware's values written back, and a line naming
//!   what was read ([`AfterCall`]);
//! - then, unless [`AFTER_PATH`] asks for the kernel, a reset however the call went: the
//!   firmware's `ResetSystem` (decision item 344, ADR-0002 Amendment 3, a bench-only exception to
//!   B5), and if the machine still runs, the FADT's reset register ([`find_reset`]). A reset
//!   shows on the rented console as a new firmware start, which needs no UART.
//!
//! This module exists only under `cfg(origin_bench)`, and in the host tests; no image a normal
//! build makes holds it (`cargo xtask check`, F1 and F2).

use core::fmt;

use origin_acpi_tables::ErrorKind;
use origin_acpi_tables::fadt::Fadt;
use origin_acpi_tables::gas::AddressSpace;
use origin_acpi_tables::header::HEADER_LEN;
use origin_boot_info::{MemoryRegion, RegionKind};

use crate::spcr::{self, Missing};
use crate::uart::{ExitCalls, POLLS, Port};

/// How every line the bench loader adds begins: the text F2 looks for in images.
pub const PREFIX: &str = "origin-boot: bench build: ";

/// The file on the loader's volume that asks for stale map keys, in QEMU scenarios only.
pub const STALE_KEY_PATH: &str = "\\EFI\\ORIGIN\\BENCH-STALE-KEY";

/// Which model-specific registers the state line may read. Intel's SDM Volume 4 (Table 2-2) and
/// AMD's APM Volume 2 (§7.8.3, §18.12) tie them to the same bits, so the decision does not depend
/// on the vendor: `IA32_PAT` (0x277) to CPUID 01H:EDX[16]; `S_CET` (0x6A2), `PL0_SSP` (0x6A4)
/// and the interrupt SSP table address (0x6A8) to CPUID 07H.0:ECX[7], CET_SS. Intel also defines
/// `S_CET` under CET_IBT alone, but not the other two, so all three wait for CET_SS. A register
/// read where it does not exist would take a #GP under the firmware's IDT before any breadcrumb.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reads {
    /// `IA32_PAT` exists.
    pub pat: bool,
    /// The three supervisor shadow-stack registers exist.
    pub shadow_stacks: bool,
}

/// [`Reads`] from CPUID: `max_basic_leaf` is leaf 0's EAX, `leaf1_edx` leaf 1's EDX, and
/// `leaf7_ecx` leaf 7 sub-leaf 0's ECX (ignored unless leaf 7 exists).
pub const fn reads(max_basic_leaf: u32, leaf1_edx: u32, leaf7_ecx: u32) -> Reads {
    Reads {
        pat: max_basic_leaf >= 1 && leaf1_edx & (1 << 16) != 0,
        shadow_stacks: max_basic_leaf >= 7 && leaf7_ecx & (1 << 7) != 0,
    }
}

/// The supervisor shadow-stack registers, where they exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShadowStacks {
    /// `S_CET`: bit 0 turns supervisor shadow stacks on.
    pub s_cet: u64,
    /// `PL0_SSP`.
    pub pl0_ssp: u64,
    /// The interrupt SSP table's address (`IA32_INTERRUPT_SSP_TABLE_ADDR`, AMD's `ISST_ADDR`).
    pub ssp_table: u64,
}

/// The processor state the kernel inherits from the firmware, which the entry stub changes only
/// in EFER.NXE, CR0.WP and CR3 (ADR-0002 B6): the registers every processor has, and PAT where it
/// exists. The state line's first part; [`Shadow`] ends it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Registers {
    /// CR0.
    pub cr0: u64,
    /// CR4.
    pub cr4: u64,
    /// EFER (0xC000_0080), which every x86-64 processor has.
    pub efer: u64,
    /// `IA32_PAT`, where [`Reads::pat`].
    pub pat: Option<u64>,
}

impl fmt::Display for Registers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "entry state: CR0 {:#x}, CR4 {:#x}, EFER {:#x}, ",
            self.cr0, self.cr4, self.efer
        )?;
        match self.pat {
            Some(pat) => write!(f, "PAT {pat:#x}; "),
            None => f.write_str("PAT not reported (CPUID 01H:EDX[16] = 0); "),
        }
    }
}

/// The state line's last part: the shadow-stack registers, where [`Reads::shadow_stacks`]. The
/// loader prints [`Registers`] before it reads these, so a fault here leaves those on the console.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shadow(pub Option<ShadowStacks>);

impl fmt::Display for Shadow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(s) => write!(
                f,
                "shadow stacks: S_CET {:#x}, PL0_SSP {:#x}, SSP table {:#x} (CPUID 07H.0:ECX[7] = 1)",
                s.s_cet, s.pl0_ssp, s.ssp_table
            ),
            None => f.write_str("shadow stacks: not reported (CPUID 07H.0:ECX[7] = 0)"),
        }
    }
}

/// The bench loader's line on the SPCR's UART once `ExitBootServices` succeeded: which call
/// succeeded, and whether a stale key was asked for ([`stale_calls`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exited {
    /// The calls made.
    pub calls: ExitCalls,
    /// Calls given a stale key: 0, 1 or 2.
    pub stale: u8,
}

impl fmt::Display for Exited {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(PREFIX)?;
        match (self.calls.first, self.calls.retry) {
            (Some(_), None) => f.write_str("boot services exited on the first call")?,
            (Some(first), Some(_)) => write!(
                f,
                "boot services exited on the retry; the first call returned {first:?}"
            )?,
            (None, _) => f.write_str("boot services exited, no call recorded")?,
        }
        match self.stale {
            0 => Ok(()),
            1 => f.write_str(" [the first call was given a stale key: BENCH-STALE-KEY]"),
            _ => f.write_str(" [both calls were given a stale key: BENCH-STALE-KEY]"),
        }
    }
}

/// How many `ExitBootServices` calls [`STALE_KEY_PATH`]'s contents ask to be given a stale key:
/// `1` or `2`, with any whitespace after it. Anything else asks for none.
pub fn stale_calls(contents: &[u8]) -> Option<u8> {
    match contents.trim_ascii() {
        b"1" => Some(1),
        b"2" => Some(2),
        _ => None,
    }
}

/// The key call `call` (0 for the first, 1 for the retry) passes to `ExitBootServices`: the one
/// the firmware gave, or, for the first `stale` calls, that key plus one. A key that is not the
/// current one is refused with `EFI_INVALID_PARAMETER` (UEFI 2.11 §7.4).
pub const fn key_for(call: u8, stale: u8, key: usize) -> usize {
    if call < stale {
        key.wrapping_add(1)
    } else {
        key
    }
}

/// The file on the loader's volume that says what the bench loader does once `ExitBootServices`
/// has returned, in QEMU scenarios only (TASKS item 116). A rented machine's volume never holds
/// it: the rental tool refuses an ESP that does, so a rental takes [`After::Reset`].
pub const AFTER_PATH: &str = "\\EFI\\ORIGIN\\BENCH-AFTER";

/// What the bench loader does once `ExitBootServices` has returned, either way (TASKS item 116).
/// Every mode writes the lines before the call and [`AfterCall`] after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    /// No [`AFTER_PATH`]: the reset, and the kernel is not entered. What a rental runs.
    Reset,
    /// `kernel`: TASKS item 112's path. The kernel is entered after a successful call, and the
    /// loader halts after a failed one, with no reset.
    Kernel,
    /// `fadt`: as [`After::Reset`], but `ResetSystem` is not called, as if it had returned, so
    /// the FADT's reset register is what resets the machine.
    Fadt,
    /// `disturb`: as [`After::Reset`], once the loader itself has turned the UART's loopback on
    /// and changed its divisor as soon as the call returns, as a firmware might as it exits.
    /// Rental D's reading B1, made under QEMU, so that writing the firmware's values back is
    /// seen to matter.
    Disturb,
    /// The minimal mode's stand-in table's `noreset` (ADR-0002 Amendment 5, QEMU only): as
    /// [`After::Fadt`], `ResetSystem` is not called, so the FADT's step is reached. Never read from
    /// [`AFTER_PATH`].
    StandInNoReset,
    /// The bisection mode's `BENCH-FCH` word `noreset` (ADR-0002 Amendment 6, QEMU only): as
    /// [`After::StandInNoReset`]. Never read from [`AFTER_PATH`].
    SwitchNoReset,
}

impl After {
    /// The mode [`AFTER_PATH`]'s contents ask for, with any whitespace around the word; `None`
    /// for anything else, which the loader takes as [`After::Reset`] and says so.
    pub fn from_contents(contents: &[u8]) -> Option<Self> {
        match contents.trim_ascii() {
            b"kernel" => Some(Self::Kernel),
            b"fadt" => Some(Self::Fadt),
            b"disturb" => Some(Self::Disturb),
            _ => None,
        }
    }

    /// Whether the kernel is entered after a successful call.
    pub const fn enters_kernel(self) -> bool {
        matches!(self, Self::Kernel)
    }

    /// Whether Amendment 3's order calls no `ResetSystem` in this mode.
    pub const fn skips_reset_system(self) -> bool {
        matches!(
            self,
            Self::Fadt | Self::StandInNoReset | Self::SwitchNoReset
        )
    }

    /// The loader's line on its console about the switch: `found` is `None` with no
    /// [`AFTER_PATH`], `Some(None)` for contents not understood.
    pub const fn said(found: Option<Option<Self>>) -> &'static str {
        match found {
            None => {
                "no BENCH-AFTER: after ExitBootServices the loader resets the machine, and the kernel is not entered"
            }
            Some(None) => {
                "BENCH-AFTER not understood: after ExitBootServices the loader resets the machine, and the kernel is not entered"
            }
            Some(Some(Self::Reset)) => {
                "BENCH-AFTER: after ExitBootServices the loader resets the machine, and the kernel is not entered"
            }
            Some(Some(Self::Kernel)) => {
                "BENCH-AFTER kernel: after ExitBootServices the loader enters the kernel, or halts after a failed call"
            }
            Some(Some(Self::Fadt)) => {
                "BENCH-AFTER fadt: after ExitBootServices the loader resets the machine through the FADT's reset register, without calling ResetSystem"
            }
            Some(Some(Self::Disturb)) => {
                "BENCH-AFTER disturb: after ExitBootServices the loader turns the UART's loopback on and changes its divisor, then writes the firmware's values back, then resets the machine"
            }
            Some(Some(Self::StandInNoReset)) => {
                "the stand-in table's noreset: Amendment 3's order calls no ResetSystem"
            }
            Some(Some(Self::SwitchNoReset)) => {
                "BENCH-FCH's noreset: Amendment 3's order calls no ResetSystem"
            }
        }
    }
}

/// A 16550's registers, as offsets from its base (PC16550D): the transmit and receive buffer, or
/// the divisor's low byte with DLAB set; the interrupt enable, or the divisor's high byte; the
/// FIFO control (write only); line control; modem control; line status; modem status; scratch.
const DATA: u16 = 0;
const IER_DLM: u16 = 1;
const FCR: u16 = 2;
const LCR: u16 = 3;
const MCR: u16 = 4;
const LSR: u16 = 5;
const MSR: u16 = 6;
const SCR: u16 = 7;
/// LCR's divisor-latch access bit.
const DLAB: u8 = 0x80;
/// MCR's loopback bit: what is sent goes to the receiver instead of the line.
const LOOPBACK: u8 = 0x10;
/// LSR's "transmitter empty": the holding register and the shift register are both empty.
const TEMT: u8 = 0x40;
/// FCR: the FIFOs on, both cleared.
const FIFOS_ON_CLEARED: u8 = 0x07;

/// A 16550's registers as the bench loader reads them, before `ExitBootServices` and after.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UartState {
    /// The interrupt enable register.
    pub ier: u8,
    /// Line control, as read: the value put back after reading the divisor.
    pub lcr: u8,
    /// Modem control.
    pub mcr: u8,
    /// Line status.
    pub lsr: u8,
    /// Modem status.
    pub msr: u8,
    /// The scratch register.
    pub scratch: u8,
    /// The divisor latch.
    pub divisor: u16,
}

impl UartState {
    /// Reads the UART at `base`. LCR is read first and written back as found last; the IER is read
    /// with DLAB clear and the divisor with it set. Nothing else is written, and the receive
    /// buffer is not read, so no byte is taken from it. A UART that is not there reads as all
    /// ones, and the writes to LCR then go nowhere.
    pub fn read<P: Port>(io: &mut P, base: u16) -> Self {
        let at = |offset: u16| base.wrapping_add(offset);
        let lcr = io.read(at(LCR));
        io.write(at(LCR), lcr & !DLAB);
        let ier = io.read(at(IER_DLM));
        let mcr = io.read(at(MCR));
        let lsr = io.read(at(LSR));
        let msr = io.read(at(MSR));
        let scratch = io.read(at(SCR));
        io.write(at(LCR), lcr | DLAB);
        let low = io.read(at(DATA));
        let high = io.read(at(IER_DLM));
        io.write(at(LCR), lcr);
        Self {
            ier,
            lcr,
            mcr,
            lsr,
            msr,
            scratch,
            divisor: u16::from_le_bytes([low, high]),
        }
    }

    /// Writes these values back to the UART at `base`: the divisor, then LCR with DLAB clear, the
    /// IER, MCR, and the FIFOs on and cleared, since FCR cannot be read. Waits first, for at most
    /// [`POLLS`] line-status reads, until the transmitter is empty, so no byte of a line just
    /// written is cleared away. Returns whether it emptied.
    pub fn write_back<P: Port>(&self, io: &mut P, base: u16) -> bool {
        let at = |offset: u16| base.wrapping_add(offset);
        let emptied = drain(io, base);
        let [low, high] = self.divisor.to_le_bytes();
        io.write(at(LCR), self.lcr | DLAB);
        io.write(at(DATA), low);
        io.write(at(IER_DLM), high);
        io.write(at(LCR), self.lcr & !DLAB);
        io.write(at(IER_DLM), self.ier);
        io.write(at(MCR), self.mcr);
        io.write(at(FCR), FIFOS_ON_CLEARED);
        emptied
    }
}

impl fmt::Display for UartState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "IER {:#04x}, LCR {:#04x}, MCR {:#04x}, LSR {:#04x}, MSR {:#04x}, scratch {:#04x}, divisor {:#06x}",
            self.ier, self.lcr, self.mcr, self.lsr, self.msr, self.scratch, self.divisor
        )
    }
}

/// Waits, for at most [`POLLS`] line-status reads, until the UART at `base` has sent everything
/// it holds. Returns whether it did.
pub fn drain<P: Port>(io: &mut P, base: u16) -> bool {
    (0..POLLS).any(|_| io.read(base.wrapping_add(LSR)) & TEMT != 0)
}

/// The divisor [`disturb`] sets: one that is not `divisor`.
pub const fn disturbed_divisor(divisor: u16) -> u16 {
    if divisor == 0x000C { 0x0018 } else { 0x000C }
}

/// [`After::Disturb`]: what a firmware might do to the UART as it exits, done by the loader
/// itself under QEMU. Loopback on, so nothing reaches the line, and another divisor.
pub fn disturb<P: Port>(io: &mut P, base: u16, under: &UartState) {
    let at = |offset: u16| base.wrapping_add(offset);
    let [low, high] = disturbed_divisor(under.divisor).to_le_bytes();
    io.write(at(LCR), under.lcr | DLAB);
    io.write(at(DATA), low);
    io.write(at(IER_DLM), high);
    io.write(at(LCR), under.lcr & !DLAB);
    io.write(at(MCR), under.mcr | LOOPBACK);
}

/// The bench loader's line on the firmware's console, before `ExitBootServices`: the UART the
/// SPCR named, as the firmware left it, or that none was named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnderFirmware {
    /// The UART's I/O port, 0 for none.
    pub port: u16,
    /// What was read there.
    pub state: Option<UartState>,
}

impl fmt::Display for UnderFirmware {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(PREFIX)?;
        match self.state {
            Some(state) => write!(
                f,
                "the UART at I/O port {:#x} under the firmware: {state}",
                self.port
            ),
            None => f.write_str(
                "the SPCR names no UART in I/O space: no register is read, and no line goes straight to a UART",
            ),
        }
    }
}

/// The line written straight to the UART before `ExitBootServices`, while the firmware still
/// owns it.
pub const BEFORE_CALL: &str =
    "origin-boot: bench build: this line went straight to the UART, before ExitBootServices";
/// Written straight to the UART just before the first call and before the retry, each on a line
/// of its own: the memory map was read and the call is being made. Port writes leave memory, and
/// so the map key, as they were.
pub const MARK_FIRST: &str = "X";
pub const MARK_RETRY: &str = "Y";

/// The line written straight to the UART after `ExitBootServices` returned, either way, and after
/// the registers were read again and the firmware's values written back. It names how the calls
/// ended too: under rental D's reading B1 the line before it, written to the UART as the call
/// left it, may not be readable, and this one is the first that is (TASKS item 116's review,
/// R116-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AfterCall {
    /// Each call's status.
    pub calls: ExitCalls,
    /// What was read after the call.
    pub read: UartState,
    /// Whether the values read under the firmware were written back.
    pub wrote_back: bool,
}

impl fmt::Display for AfterCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(PREFIX)?;
        f.write_str("after ExitBootServices (")?;
        match (self.calls.first, self.calls.retry) {
            (Some(first), Some(retry)) => write!(f, "first call {first:?}, retry {retry:?}")?,
            (Some(first), None) => write!(f, "first call {first:?}")?,
            (None, _) => f.write_str("no call recorded")?,
        }
        write!(
            f,
            ") the UART read {}; {}",
            self.read,
            if self.wrote_back {
                "the firmware's values are written back"
            } else {
                "nothing was read under the firmware, so nothing is written back"
            }
        )
    }
}

/// The FADT's reset register, once checked: where to write [`ResetRegister::value`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetRegister {
    /// A system I/O port.
    Io {
        /// The port.
        port: u16,
        /// The value that resets the machine.
        value: u8,
    },
    /// A byte of system memory, in memory-mapped I/O the firmware's map describes.
    Memory {
        /// Its physical address.
        address: u64,
        /// The value that resets the machine.
        value: u8,
    },
}

/// Why there is no reset register the bench loader will write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoReset {
    /// The firmware's memory map could not be read, so no table was.
    NoMap,
    /// The walk found no FADT (`spcr`'s reasons, the same walk).
    Missing(Missing),
    /// The FADT declares more than [`MAX_FADT_LENGTH`] bytes.
    TooLong(u32),
    /// The FADT's bytes, from this address, are not in memory the firmware's map describes.
    Unreadable(u64),
    /// The shared parser refuses the FADT (`origin_acpi_tables::fadt`, the kernel's).
    Table(ErrorKind),
    /// The FADT's flags say there is no reset register (`RESET_REG_SUP` clear), or the table
    /// ends before it.
    NotSupported,
    /// `RESET_REG` breaks ACPI 6.6's rules (§5.2.9: 8 bits at bit 0 in system I/O, system memory
    /// or bus 0's configuration space).
    Breaks,
    /// It is in this address space, which the bench loader does not write: only system I/O and
    /// system memory.
    Space(AddressSpace),
    /// I/O port 0, or one above `0xFFFF`.
    Port(u64),
    /// A memory address the firmware's map does not describe as memory-mapped I/O, so the
    /// firmware's page tables may not map it.
    NotMmio(u64),
    /// I/O port `0xCF9`, which the minimal mode never writes (ADR-0002 Amendment 5: "there is no
    /// `0xCF9` write anywhere in this mode"; the coordinating session's ruling on TASKS item
    /// 128's Q3). Under QEMU the FADT's reset register is that port; on the rented board it is
    /// the SMI command port `0xB2`.
    Cf9InMinimal,
    /// I/O port `0xCF9`, which the bisection mode never writes either (ADR-0002 Amendment 6, step
    /// 5: "with the FADT step skipped where its register is I/O port `0xCF9`").
    Cf9InBisect,
}

/// The FADT's reset register as the minimal mode may use it in Amendment 3's order: the same,
/// unless it is I/O port `0xCF9`, which the mode skips ([`NoReset::Cf9InMinimal`]), going
/// straight to the halt.
pub const fn minimal_register(
    register: Result<ResetRegister, NoReset>,
) -> Result<ResetRegister, NoReset> {
    match register {
        Ok(ResetRegister::Io { port: 0xCF9, .. }) => Err(NoReset::Cf9InMinimal),
        other => other,
    }
}

/// [`minimal_register`] for the bisection mode (ADR-0002 Amendment 6, step 5).
pub const fn bisect_register(
    register: Result<ResetRegister, NoReset>,
) -> Result<ResetRegister, NoReset> {
    match register {
        Ok(ResetRegister::Io { port: 0xCF9, .. }) => Err(NoReset::Cf9InBisect),
        other => other,
    }
}

/// The bisection mode's first line, after the banner (ADR-0002 Amendment 6; ruling Q8 of TASKS
/// item 131). The rental tool tells a bisection-mode image by its text, `bench build: bisect`.
pub const BISECT_LINE: &str = "origin-boot: bench build: bisect: the bisection mode (ADR-0002 Amendment 6): the full bench loader's work before the call, with the kernel stack and the boot info allocated as EfiLoaderData, not Origin's OS-defined types; the call through the loader's copy of the boot services table; a triple-fault control; after a successful call, a triple fault and nothing before it; no 0xCF9 write";

/// The bisection mode's line about `BENCH-AFTER`, which it reads as the full loader does and
/// whose value it ignores (ruling Q3 of TASKS item 131).
pub const BISECT_AFTER_IGNORED: &str = "origin-boot: bench build: bisect: BENCH-AFTER's value is ignored: this mode never enters the kernel, and after a successful call it triple-faults";

/// The bisection mode's last line through the firmware's console, in place of the full loader's
/// "exiting boot services; entering the Framekernel".
pub const BISECT_LAST_LINE: &str = "origin-boot: bench build: bisect: ExitBootServices next, through the loader's copy of the boot services table";

/// What the bisection mode counted in the last memory map it translated before its last line
/// (ADR-0002 Amendment 6, step 2; rulings Q6 and Q7 of TASKS item 131): the descriptors of an
/// OS-defined type, `0x8000_0000` or above, and the first such type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OsTypesLine(pub Option<crate::memmap::OsTypes>);

impl fmt::Display for OsTypesLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("origin-boot: bench build: bisect: ")?;
        match self.0 {
            None => f.write_str(
                "the memory map the page tables were built from was not read, so its types were not counted; this run does not show that the map holds no OS-defined type",
            ),
            Some(t) if t.count == 0 => write!(
                f,
                "the memory map the page tables were built from holds no region of an OS-defined type (0x80000000 and above), in {} descriptors",
                t.descriptors
            ),
            Some(t) => write!(
                f,
                "the memory map the page tables were built from holds {} region(s) of an OS-defined type (0x80000000 and above), the first {:#010x}, in {} descriptors: this run does not answer whether the call returns without Origin's OS-defined types",
                t.count, t.first, t.descriptors
            ),
        }
    }
}

/// The line the minimal-mode loader writes first, after the banner (ADR-0002 Amendment 5). The
/// rental tool and the records tell a minimal-mode image by its text, `bench build: minimal`.
pub const MINIMAL_LINE: &str = "origin-boot: bench build: minimal: the minimal-program mode (ADR-0002 Amendment 5): no kernel read, no verification, no generator, no allocation, no page tables, no framebuffer search, no copy of the boot services table; after a successful call, a triple fault, whatever the shutdown bits say (decision item 349)";

/// The time-stamp counter's ticks a millisecond the minimal mode takes for its waits, since it
/// calls no `Stall` to count them (ADR-0002 Amendment 5, "no `Stall`"): [`ASSUMED_TICKS_PER_MS`],
/// above any rate a processor's counter runs at, so each wait is at least as long as its figure
/// (TASKS item 128's review, R128-7). Under QEMU, whose counter is slower, the waits are longer.
pub const MINIMAL_TICKS_PER_MS: u64 = ASSUMED_TICKS_PER_MS;

/// Amendment 4's addition (item 348), the full mode: the live boot services table against the
/// loader's copy, before its last line. The minimal mode makes no copy, so it has no such line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableLine {
    /// Where the system table says the live table is, now.
    pub live_at: u64,
    /// Where the loader's copy was taken from.
    pub copied_from: u64,
    /// The copy's size, over which the two are compared.
    pub size: usize,
    /// How the live table compares.
    pub live: Live,
    /// The copy's `ExitBootServices`.
    pub copy_exit: u64,
    /// The copy's header CRC32 field.
    pub copy_crc: u32,
}

/// The live boot services table, as [`TableLine`] found it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Live {
    /// Its signature or `HeaderSize` does not check, so its bytes were not read.
    Unchecked,
    /// Read over the copy's size: the first byte that differs, if one does, its
    /// `ExitBootServices` and its header CRC32 field.
    Read {
        /// The first byte that differs from the copy.
        first_difference: Option<usize>,
        /// Its `ExitBootServices`.
        exit: u64,
        /// Its header CRC32 field.
        crc: u32,
    },
}

/// The first of `copy.len()` bytes where `live(i)` differs from `copy[i]`.
pub fn first_difference(copy: &[u8], mut live: impl FnMut(usize) -> u8) -> Option<usize> {
    copy.iter()
        .enumerate()
        .find(|&(i, &b)| live(i) != b)
        .map(|(i, _)| i)
}

impl fmt::Display for TableLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{PREFIX}the boot services table (ADR-0002 Amendment 4's addition): the system table names it at {:#x}",
            self.live_at
        )?;
        if self.live_at != self.copied_from {
            write!(
                f,
                ", not at {:#x}, where the loader's copy was taken from",
                self.copied_from
            )?;
        }
        match self.live {
            Live::Unchecked => write!(
                f,
                "; the pointer is null or misaligned, or the table's signature or size does not check, so it was not compared; the copy's ExitBootServices {:#x}, header CRC32 {:#010x}",
                self.copy_exit, self.copy_crc
            ),
            Live::Read {
                first_difference,
                exit,
                crc,
            } => {
                match first_difference {
                    None => write!(
                        f,
                        "; the same as the loader's copy, byte for byte over its {} bytes",
                        self.size
                    )?,
                    Some(at) => write!(
                        f,
                        "; it differs from the loader's copy from byte {at} of {}",
                        self.size
                    )?,
                }
                write!(
                    f,
                    "; ExitBootServices live {exit:#x}, copy {:#x}; header CRC32 live {crc:#010x}, copy {:#010x}",
                    self.copy_exit, self.copy_crc
                )
            }
        }
    }
}

/// The longest FADT read: ACPI 6.6's is 276 bytes.
pub const MAX_FADT_LENGTH: usize = 1024;

/// The FADT's reset register (ACPI 6.6 §4.8.3.6, §5.2.9), checked as untrusted input: the FADT
/// as [`with_fadt`] finds it; the kernel's own parser (`RESET_REG_SUP`, 8 bits at bit 0, the
/// value present); then system I/O or system memory only, an I/O port from 1 to `0xFFFF`, and
/// memory only where `is_mmio` says the firmware's map describes memory-mapped I/O.
pub fn find_reset(
    rsdp: u64,
    read: &mut impl FnMut(u64, &mut [u8]) -> bool,
    is_mmio: impl Fn(u64) -> bool,
) -> Result<ResetRegister, NoReset> {
    with_fadt(rsdp, read, |fadt, _| register(fadt, &is_mmio))?
}

/// The FADT, handed to `f` with `read` (TASKS items 116 and 121): the RSDP and XSDT as the SPCR's
/// walk checks them; the first FADT the XSDT lists, passed over only when the shared parser
/// refuses it, and the end of the walk when it is longer than [`MAX_FADT_LENGTH`] or cannot be
/// read whole, as the SPCR's walk does (T84-8). Every byte is read through `read`, as in
/// `spcr::find`.
pub fn with_fadt<T>(
    rsdp: u64,
    read: &mut impl FnMut(u64, &mut [u8]) -> bool,
    f: impl FnOnce(&Fadt<'_>, &mut dyn FnMut(u64, &mut [u8]) -> bool) -> T,
) -> Result<T, NoReset> {
    let missing = |m| NoReset::Missing(m);
    if rsdp == 0 {
        return Err(missing(Missing::NoRsdp));
    }
    let mut bytes = [0u8; spcr::RSDP_LENGTH];
    if !read(rsdp, &mut bytes) {
        return Err(missing(Missing::RsdpUnreadable(rsdp)));
    }
    let xsdt = spcr::parse_rsdp(&bytes).map_err(|e| missing(Missing::Rsdp(e)))?;
    let entries = spcr::check_xsdt(xsdt, read).map_err(missing)?;
    let mut refused = None;
    let mut unreadable = None;
    for i in 0..entries {
        let at = xsdt.wrapping_add(HEADER_LEN as u64 + 8 * u64::from(i));
        let mut entry = [0u8; 8];
        if !read(at, &mut entry) {
            unreadable.get_or_insert(at);
            continue;
        }
        let table = u64::from_le_bytes(entry);
        if table == 0 {
            continue;
        }
        let mut header = [0u8; HEADER_LEN];
        if !read(table, &mut header) {
            unreadable.get_or_insert(table);
            continue;
        }
        if header.get(..4) != Some(&b"FACP"[..]) {
            continue;
        }
        let declared = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        let len = declared as usize;
        if len > MAX_FADT_LENGTH {
            return Err(NoReset::TooLong(declared));
        }
        let mut whole = [0u8; MAX_FADT_LENGTH];
        let whole = whole.get_mut(..len.max(HEADER_LEN)).unwrap_or(&mut []);
        if !read(table, whole) {
            return Err(NoReset::Unreadable(table));
        }
        match Fadt::parse(whole) {
            Ok(fadt) => return Ok(f(&fadt, read)),
            // A header that does not check: a later FADT may.
            Err(e) => {
                refused.get_or_insert(NoReset::Table(e.kind));
            }
        }
    }
    Err(match (refused, unreadable) {
        (Some(why), _) => why,
        (None, Some(at)) => missing(Missing::EntryUnreadable(at)),
        (None, None) => missing(Missing::NotListed),
    })
}

/// The bench loader's rules on a parsed FADT's reset register.
fn register(fadt: &Fadt<'_>, is_mmio: &impl Fn(u64) -> bool) -> Result<ResetRegister, NoReset> {
    if !fadt.flags.reset_reg_sup() || fadt.reset_reg.is_none() || fadt.reset_value.is_none() {
        return Err(NoReset::NotSupported);
    }
    let (gas, value) = fadt.reset().ok_or(NoReset::Breaks)?;
    match gas.space {
        AddressSpace::SystemIo => match u16::try_from(gas.address) {
            Ok(port) if port != 0 => Ok(ResetRegister::Io { port, value }),
            _ => Err(NoReset::Port(gas.address)),
        },
        AddressSpace::SystemMemory if gas.address != 0 && is_mmio(gas.address) => {
            Ok(ResetRegister::Memory {
                address: gas.address,
                value,
            })
        }
        AddressSpace::SystemMemory => Err(NoReset::NotMmio(gas.address)),
        other => Err(NoReset::Space(other)),
    }
}

impl fmt::Display for ResetRegister {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Io { port, value } => write!(f, "{value:#x} to I/O port {port:#x}"),
            Self::Memory { address, value } => write!(f, "{value:#x} to memory at {address:#x}"),
        }
    }
}

impl fmt::Display for NoReset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoMap => f.write_str("the memory map could not be read, so no table was"),
            Self::Missing(Missing::NotListed) => f.write_str("the XSDT lists no FADT"),
            Self::Missing(why) => write!(f, "no FADT: {why}"),
            Self::TooLong(n) => write!(
                f,
                "the FADT declares {n} bytes, more than the loader reads ({MAX_FADT_LENGTH})"
            ),
            Self::Unreadable(at) => write!(
                f,
                "the FADT's bytes at {at:#x} are not in memory the firmware's map describes"
            ),
            Self::Table(kind) => write!(f, "the FADT is refused: {kind:?}"),
            Self::NotSupported => {
                f.write_str("the FADT's flags give no reset register (RESET_REG_SUP clear)")
            }
            Self::Breaks => f.write_str(
                "RESET_REG breaks ACPI 6.6's rules (8 bits at bit 0, in I/O, memory or bus 0's configuration space)",
            ),
            Self::Space(space) => write!(
                f,
                "RESET_REG is in {space:?}, and this loader writes only system I/O or system memory"
            ),
            Self::Port(0) => f.write_str("RESET_REG names I/O port 0"),
            Self::Port(port) => write!(f, "RESET_REG names I/O port {port:#x}, above 0xffff"),
            Self::NotMmio(at) => write!(
                f,
                "RESET_REG names memory at {at:#x}, which the firmware's map does not describe as memory-mapped I/O"
            ),
            Self::Cf9InMinimal => f.write_str(
                "RESET_REG is I/O port 0xcf9, which the minimal mode never writes (ADR-0002 Amendment 5)",
            ),
            Self::Cf9InBisect => f.write_str(
                "RESET_REG is I/O port 0xcf9, which the bisection mode never writes (ADR-0002 Amendment 6)",
            ),
        }
    }
}

/// The bench loader's line on the firmware's console about the FADT's reset register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResetLine(pub Result<ResetRegister, NoReset>);

impl fmt::Display for ResetLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(PREFIX)?;
        match self.0 {
            Ok(register) => write!(f, "the FADT's reset register: write {register}"),
            Err(why) => write!(f, "the FADT gives no usable reset register: {why}"),
        }
    }
}

/// `EFI_RUNTIME_SERVICES_SIGNATURE`, "RUNTSERV" (UEFI 2.11 §4.5.1).
pub const RUNTIME_SERVICES_SIGNATURE: u64 = 0x5652_4553_544E_5552;
/// `ResetSystem`'s offset in `EFI_RUNTIME_SERVICES` (UEFI 2.11 §4.5.1): after the 24-byte header,
/// the ten pointers from `GetTime` to `GetNextHighMonotonicCount`.
pub const RESET_SYSTEM_OFFSET: usize = 24 + 10 * 8;
/// `EfiResetCold` (UEFI 2.11 §8.5.1.1).
pub const RESET_COLD: u32 = 0;

/// How long after its line the bench loader calls `ResetSystem`, then how long it waits before
/// the FADT's register, then how long before it says that did not reset the machine either.
pub const RESET_DELAY_MS: u64 = 5_000;
pub const FALLBACK_DELAY_MS: u64 = 2_000;
pub const AFTER_REGISTER_MS: u64 = 1_000;
/// The firmware's `Stall` the time-stamp counter is counted over, before `ExitBootServices`.
pub const CALIBRATION_US: usize = 20_000;
/// The rate taken when it cannot be counted: above any rate a processor's counter runs at, so a
/// wait is longer than asked for, never shorter.
pub const ASSUMED_TICKS_PER_MS: u64 = 6_000_000;

/// The time-stamp counter's ticks a millisecond, from `ticks` counted over [`CALIBRATION_US`] of
/// the firmware's `Stall`, and whether it was counted: with no count, or a count below one tick a
/// millisecond, [`ASSUMED_TICKS_PER_MS`]. A count above that rate, from a `Stall` that ran long,
/// is taken as that rate, so a wait is never stretched beyond what the slowest-counted rate gives
/// (TASKS item 116's review, R116-4).
pub const fn ticks_per_ms(ticks: Option<u64>) -> (u64, bool) {
    let ms = (CALIBRATION_US / 1_000) as u64;
    match ticks {
        Some(t) if t >= ms => {
            let rate = t / ms;
            (
                if rate < ASSUMED_TICKS_PER_MS {
                    rate
                } else {
                    ASSUMED_TICKS_PER_MS
                },
                true,
            )
        }
        _ => (ASSUMED_TICKS_PER_MS, false),
    }
}

/// Whether `at` lies in a region the firmware's map describes as memory-mapped I/O, which the
/// firmware's page tables map while its own run (`memmap::translate`'s `Mmio`).
pub fn in_mmio(regions: &[MemoryRegion], at: u64) -> bool {
    regions.iter().any(|r| {
        r.kind == RegionKind::Mmio as u32
            && at
                .checked_sub(r.base)
                .is_some_and(|offset| offset < r.pages.saturating_mul(4096))
    })
}

/// The bench loader's line on the firmware's console about the reset: whether `ResetSystem` can be
/// called, and the counter's rate its waits use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResetPlan {
    /// `ResetSystem`'s address, or why there is none.
    pub reset_system: Result<u64, &'static str>,
    /// The counter's ticks a millisecond.
    pub ticks_per_ms: u64,
    /// Whether that rate was counted, not assumed.
    pub counted: bool,
}

impl fmt::Display for ResetPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(PREFIX)?;
        match self.reset_system {
            Ok(at) => write!(f, "ResetSystem is at {at:#x}")?,
            Err(why) => write!(f, "ResetSystem will not be called: {why}")?,
        }
        write!(
            f,
            "; the time-stamp counter {} {} ticks a millisecond",
            if self.counted {
                "counted, over the firmware's Stall,"
            } else {
                "could not be counted, so the loader takes"
            },
            self.ticks_per_ms
        )
    }
}

/// The line straight to the UART when the reset begins: what comes first, and when.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResetSoon {
    /// Whether `ResetSystem` will be called.
    pub reset_system: bool,
    /// Whether the FADT gave a usable reset register.
    pub register: bool,
}

impl fmt::Display for ResetSoon {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (first, then) = (RESET_DELAY_MS / 1_000, FALLBACK_DELAY_MS / 1_000);
        let both = first + then;
        match (self.reset_system, self.register) {
            (true, true) => write!(
                f,
                "{PREFIX}a cold reset through the firmware's ResetSystem in {first} s, and the FADT's reset register {then} s later if the machine still runs"
            ),
            (true, false) => write!(
                f,
                "{PREFIX}a cold reset through the firmware's ResetSystem in {first} s, and a halt {then} s later if the machine still runs: the FADT gives no usable reset register"
            ),
            (false, true) => write!(
                f,
                "{PREFIX}a reset through the FADT's reset register in {both} s: ResetSystem is not called"
            ),
            (false, false) => write!(
                f,
                "{PREFIX}no reset: ResetSystem is not called and the FADT gives no usable reset register; a halt in {both} s"
            ),
        }
    }
}

/// Whether `ResetSystem` was called before the FADT's register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tried {
    /// It was called, and returned.
    Returned,
    /// [`After::Fadt`] asked for it not to be called.
    NotCalled,
    /// The minimal mode's stand-in table asked for it not to be called ([`After::StandInNoReset`]).
    NotCalledStandIn,
    /// The bisection mode's `BENCH-FCH` asked for it not to be called ([`After::SwitchNoReset`]).
    NotCalledSwitch,
    /// There was none to call.
    Unavailable,
}

/// The line straight to the UART when `ResetSystem` did not reset the machine: the FADT's
/// register next, or the halt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fallback {
    /// What became of `ResetSystem`.
    pub tried: Tried,
    /// The FADT's register.
    pub register: Result<ResetRegister, NoReset>,
}

impl fmt::Display for Fallback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(PREFIX)?;
        f.write_str(match self.tried {
            Tried::Returned => "ResetSystem returned",
            Tried::NotCalled => "ResetSystem was not called (BENCH-AFTER fadt)",
            Tried::NotCalledStandIn => "ResetSystem was not called (the stand-in table's noreset)",
            Tried::NotCalledSwitch => "ResetSystem was not called (BENCH-FCH's noreset)",
            Tried::Unavailable => "there was no ResetSystem to call",
        })?;
        match self.register {
            Ok(register) => write!(f, ": writing {register}, the FADT's reset register"),
            Err(why) => write!(
                f,
                ", and the FADT gives no usable reset register ({why}): halting"
            ),
        }
    }
}

/// The last line, when the FADT's register did not reset the machine either.
pub const NOT_RESET: &str =
    "origin-boot: bench build: the FADT's reset register did not reset the machine: halting";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uefi::Status;

    /// CPUID values for the gating tests: leaf 0's EAX, leaf 1's EDX, leaf 7.0's ECX and EDX.
    struct Processor {
        name: &'static str,
        max_basic_leaf: u32,
        leaf1_edx: u32,
        leaf7_ecx: u32,
    }

    #[test]
    fn amd_registers_are_read_only_where_cpuid_says_they_exist() {
        for (p, want) in [
            // The rented board, AMD EPYC 4244P (Zen 4), from rental C's dump: PAT and CET_SS.
            (
                Processor {
                    name: "AMD Zen 4, rented",
                    max_basic_leaf: 0x10,
                    leaf1_edx: 0x178B_FBFF,
                    leaf7_ecx: 0x0040_5FDE,
                },
                Reads {
                    pat: true,
                    shadow_stacks: true,
                },
            ),
            // An AMD processor without shadow stacks (CET_SS clear), PAT present.
            (
                Processor {
                    name: "AMD, no CET_SS",
                    max_basic_leaf: 0x10,
                    leaf1_edx: 0x178B_FBFF,
                    leaf7_ecx: 0x0040_5F5E,
                },
                Reads {
                    pat: true,
                    shadow_stacks: false,
                },
            ),
            // A leaf-7 value with bit 7 set behind a maximum leaf below 7: not read.
            (
                Processor {
                    name: "AMD, leaf 7 absent",
                    max_basic_leaf: 6,
                    leaf1_edx: 0x078B_FBFF,
                    leaf7_ecx: 0x80,
                },
                Reads {
                    pat: true,
                    shadow_stacks: false,
                },
            ),
        ] {
            assert_eq!(
                reads(p.max_basic_leaf, p.leaf1_edx, p.leaf7_ecx),
                want,
                "{}",
                p.name
            );
        }
    }

    #[test]
    fn intel_registers_are_read_only_where_cpuid_says_they_exist() {
        for (p, want) in [
            // CET_SS set: all three shadow-stack registers exist (SDM Vol. 4, Table 2-2).
            (
                Processor {
                    name: "Intel, CET_SS",
                    max_basic_leaf: 0x23,
                    leaf1_edx: 0xBFEB_FBFF,
                    leaf7_ecx: 0x1840_07EC | 0x80,
                },
                Reads {
                    pat: true,
                    shadow_stacks: true,
                },
            ),
            // CET_IBT alone (leaf 7 EDX[20], not looked at): S_CET exists then on Intel, but
            // PL0_SSP and the SSP table do not, so none of the three is read.
            (
                Processor {
                    name: "Intel, CET_IBT without CET_SS",
                    max_basic_leaf: 0x23,
                    leaf1_edx: 0xBFEB_FBFF,
                    leaf7_ecx: 0x1840_076C,
                },
                Reads {
                    pat: true,
                    shadow_stacks: false,
                },
            ),
            // No PAT bit: IA32_PAT is not read.
            (
                Processor {
                    name: "Intel, no PAT",
                    max_basic_leaf: 0x0D,
                    leaf1_edx: 0xBFEB_FBFF & !(1 << 16),
                    leaf7_ecx: 0,
                },
                Reads {
                    pat: false,
                    shadow_stacks: false,
                },
            ),
        ] {
            assert_eq!(
                reads(p.max_basic_leaf, p.leaf1_edx, p.leaf7_ecx),
                want,
                "{}",
                p.name
            );
        }
    }

    #[test]
    fn the_state_line_names_what_it_did_not_read() {
        let line = |registers: Registers, shadow: Shadow| format!("{registers}{shadow}");
        let registers = Registers {
            cr0: 0x8000_0033,
            cr4: 0x0036_06F8,
            efer: 0xD01,
            pat: Some(0x0007_0406_0007_0406),
        };
        let none = ShadowStacks {
            s_cet: 0,
            pl0_ssp: 0,
            ssp_table: 0,
        };
        assert_eq!(
            line(registers, Shadow(Some(none))),
            "entry state: CR0 0x80000033, CR4 0x3606f8, EFER 0xd01, PAT 0x7040600070406; shadow stacks: S_CET 0x0, PL0_SSP 0x0, SSP table 0x0 (CPUID 07H.0:ECX[7] = 1)"
        );
        assert_eq!(
            line(
                Registers {
                    pat: None,
                    ..registers
                },
                Shadow(None)
            ),
            "entry state: CR0 0x80000033, CR4 0x3606f8, EFER 0xd01, PAT not reported (CPUID 01H:EDX[16] = 0); shadow stacks: not reported (CPUID 07H.0:ECX[7] = 0)"
        );
    }

    #[test]
    fn the_exit_lines() {
        let mut calls = ExitCalls::default();
        calls.record(Status::SUCCESS);
        assert_eq!(
            Exited { calls, stale: 0 }.to_string(),
            "origin-boot: bench build: boot services exited on the first call"
        );
        let mut calls = ExitCalls::default();
        calls.record(Status::INVALID_PARAMETER);
        calls.record(Status::SUCCESS);
        assert_eq!(
            Exited { calls, stale: 1 }.to_string(),
            "origin-boot: bench build: boot services exited on the retry; the first call returned EFI_INVALID_PARAMETER [the first call was given a stale key: BENCH-STALE-KEY]"
        );
    }

    #[test]
    fn the_stale_key_switch() {
        assert_eq!(stale_calls(b"1"), Some(1));
        assert_eq!(stale_calls(b"2\n"), Some(2));
        assert_eq!(stale_calls(b" 2 \r\n"), Some(2));
        for other in [&b""[..], b"0", b"3", b"12", b"x", b"1 2"] {
            assert_eq!(stale_calls(other), None, "{other:?}");
        }
        assert_eq!(key_for(0, 0, 7), 7);
        assert_eq!(key_for(0, 1, 7), 8);
        assert_eq!(key_for(1, 1, 7), 7);
        assert_eq!(key_for(1, 2, 7), 8);
        assert_eq!(key_for(0, 2, usize::MAX), 0);
        let mut calls = ExitCalls::default();
        assert_eq!(calls.made(), 0);
        calls.record(Status::INVALID_PARAMETER);
        assert_eq!(calls.made(), 1);
        calls.record(Status::SUCCESS);
        assert_eq!(calls.made(), 2);
    }

    // TASKS item 116.

    const BASE: u16 = 0x2F8;

    /// A 16550 at `BASE` as PC16550D lays it out: the divisor behind DLAB, the FIFO control
    /// write-only, and "transmitter empty" withheld for `busy` line-status reads.
    struct Mock16550 {
        dll: u8,
        dlm: u8,
        ier: u8,
        lcr: u8,
        mcr: u8,
        lsr: u8,
        msr: u8,
        scr: u8,
        fcr: Vec<u8>,
        busy: u32,
        sent: Vec<u8>,
        received_reads: usize,
    }

    impl Mock16550 {
        /// As a firmware's console leaves it: 115 200 baud from a 1.8432 MHz clock, 8N1, DTR,
        /// RTS and OUT2, nothing pending.
        fn firmware() -> Self {
            Self {
                dll: 1,
                dlm: 0,
                ier: 0,
                lcr: 0x03,
                mcr: 0x0B,
                lsr: 0x60,
                msr: 0xB0,
                scr: 0,
                fcr: Vec::new(),
                busy: 0,
                sent: Vec::new(),
                received_reads: 0,
            }
        }

        fn dlab(&self) -> bool {
            self.lcr & DLAB != 0
        }
    }

    impl Port for Mock16550 {
        fn read(&mut self, port: u16) -> u8 {
            match port - BASE {
                0 if self.dlab() => self.dll,
                0 => {
                    self.received_reads += 1;
                    0
                }
                1 if self.dlab() => self.dlm,
                1 => self.ier,
                2 => 0xC1,
                3 => self.lcr,
                4 => self.mcr,
                5 if self.busy > 0 => {
                    self.busy -= 1;
                    self.lsr & !TEMT
                }
                5 => self.lsr,
                6 => self.msr,
                7 => self.scr,
                _ => panic!("{port:#x} is not one of the UART's ports"),
            }
        }

        fn write(&mut self, port: u16, value: u8) {
            match port - BASE {
                0 if self.dlab() => self.dll = value,
                0 => self.sent.push(value),
                1 if self.dlab() => self.dlm = value,
                1 => self.ier = value,
                2 => self.fcr.push(value),
                3 => self.lcr = value,
                4 => self.mcr = value,
                7 => self.scr = value,
                _ => panic!("{port:#x} is written, and is not writable"),
            }
        }
    }

    #[test]
    fn the_uart_is_read_whole_and_left_as_found() {
        let mut uart = Mock16550::firmware();
        uart.ier = 0x05;
        uart.scr = 0x5A;
        let state = UartState::read(&mut uart, BASE);
        assert_eq!(
            state,
            UartState {
                ier: 0x05,
                lcr: 0x03,
                mcr: 0x0B,
                lsr: 0x60,
                msr: 0xB0,
                scratch: 0x5A,
                divisor: 1,
            }
        );
        assert_eq!(uart.lcr, 0x03, "LCR is put back as found");
        assert_eq!(uart.received_reads, 0, "the receive buffer is never read");
        assert!(uart.sent.is_empty() && uart.fcr.is_empty());
        // A firmware that left DLAB set: the IER is still read with it clear, and LCR is put back.
        let mut uart = Mock16550::firmware();
        uart.lcr = 0x83;
        uart.ier = 0x01;
        uart.dll = 0x0C;
        let state = UartState::read(&mut uart, BASE);
        assert_eq!((state.ier, state.divisor, state.lcr), (0x01, 0x0C, 0x83));
        assert_eq!(uart.lcr, 0x83);
        assert_eq!(
            state.to_string(),
            "IER 0x01, LCR 0x83, MCR 0x0b, LSR 0x60, MSR 0xb0, scratch 0x00, divisor 0x000c"
        );
    }

    #[test]
    fn writing_back_undoes_what_a_firmware_might_do() {
        let mut uart = Mock16550::firmware();
        let under = UartState::read(&mut uart, BASE);
        disturb(&mut uart, BASE, &under);
        let after = UartState::read(&mut uart, BASE);
        assert_eq!(after.mcr, 0x0B | LOOPBACK);
        assert_eq!(after.divisor, 0x000C);
        assert_eq!(after.lcr, 0x03);
        assert!(under.write_back(&mut uart, BASE));
        assert_eq!(UartState::read(&mut uart, BASE), under);
        assert_eq!(uart.fcr, [FIFOS_ON_CLEARED]);
        assert!(uart.sent.is_empty());
        assert_eq!(disturbed_divisor(1), 0x000C);
        assert_eq!(disturbed_divisor(0x000C), 0x0018);
    }

    #[test]
    fn writing_back_waits_a_bounded_time_for_the_transmitter() {
        let mut uart = Mock16550::firmware();
        let under = UartState::read(&mut uart, BASE);
        uart.busy = 5;
        assert!(under.write_back(&mut uart, BASE));
        assert_eq!(uart.busy, 0);
        // Never empty: the values are written back anyway, after the bound.
        let mut uart = Mock16550::firmware();
        uart.mcr = 0x1B;
        uart.busy = POLLS + 10;
        assert!(!under.write_back(&mut uart, BASE));
        assert_eq!(uart.busy, 10);
        assert_eq!(uart.mcr, 0x0B);
        assert_eq!(uart.fcr, [FIFOS_ON_CLEARED]);
    }

    #[test]
    fn the_lines_around_the_call() {
        let state = UartState {
            ier: 0,
            lcr: 0x03,
            mcr: 0x0B,
            lsr: 0x60,
            msr: 0xB0,
            scratch: 0,
            divisor: 1,
        };
        assert_eq!(
            UnderFirmware {
                port: 0x2F8,
                state: Some(state)
            }
            .to_string(),
            "origin-boot: bench build: the UART at I/O port 0x2f8 under the firmware: IER 0x00, LCR 0x03, MCR 0x0b, LSR 0x60, MSR 0xb0, scratch 0x00, divisor 0x0001"
        );
        assert_eq!(
            UnderFirmware {
                port: 0,
                state: None
            }
            .to_string(),
            "origin-boot: bench build: the SPCR names no UART in I/O space: no register is read, and no line goes straight to a UART"
        );
        let mut calls = ExitCalls::default();
        calls.record(Status::SUCCESS);
        assert_eq!(
            AfterCall {
                calls,
                read: state,
                wrote_back: true
            }
            .to_string(),
            "origin-boot: bench build: after ExitBootServices (first call EFI_SUCCESS) the UART read IER 0x00, LCR 0x03, MCR 0x0b, LSR 0x60, MSR 0xb0, scratch 0x00, divisor 0x0001; the firmware's values are written back"
        );
        // Both calls failed: the line says so, since the halting line before it may be unreadable.
        let mut calls = ExitCalls::default();
        calls.record(Status::INVALID_PARAMETER);
        calls.record(Status::INVALID_PARAMETER);
        assert!(
            AfterCall {
                calls,
                read: state,
                wrote_back: false
            }
            .to_string()
            .contains(
                "(first call EFI_INVALID_PARAMETER, retry EFI_INVALID_PARAMETER) the UART read "
            )
        );
        assert!(BEFORE_CALL.starts_with(PREFIX));
        assert!(NOT_RESET.starts_with(PREFIX));
        assert_eq!((MARK_FIRST, MARK_RETRY), ("X", "Y"));
    }

    #[test]
    fn the_after_switch() {
        assert_eq!(After::from_contents(b"kernel\n"), Some(After::Kernel));
        assert_eq!(After::from_contents(b" fadt "), Some(After::Fadt));
        assert_eq!(After::from_contents(b"disturb\r\n"), Some(After::Disturb));
        for other in [&b""[..], b"reset", b"Kernel", b"kernel fadt", b"1"] {
            assert_eq!(After::from_contents(other), None, "{other:?}");
        }
        assert!(After::Kernel.enters_kernel());
        for mode in [After::Reset, After::Fadt, After::Disturb] {
            assert!(!mode.enters_kernel(), "{mode:?}");
        }
        assert!(After::said(None).starts_with("no BENCH-AFTER: "));
        assert!(After::said(Some(None)).contains("not understood"));
        assert!(After::said(Some(Some(After::Kernel))).contains("enters the kernel"));
    }

    #[test]
    fn the_counter_rate_and_the_runtime_table() {
        assert_eq!(ticks_per_ms(Some(76_000_000)), (3_800_000, true));
        assert_eq!(ticks_per_ms(None), (ASSUMED_TICKS_PER_MS, false));
        assert_eq!(ticks_per_ms(Some(5)), (ASSUMED_TICKS_PER_MS, false));
        // A Stall that ran long counts a rate above any processor's: taken as the assumed rate.
        assert_eq!(
            ticks_per_ms(Some(20 * 3 * ASSUMED_TICKS_PER_MS)),
            (ASSUMED_TICKS_PER_MS, true)
        );
        assert_eq!(&RUNTIME_SERVICES_SIGNATURE.to_le_bytes(), b"RUNTSERV");
        assert_eq!(RESET_SYSTEM_OFFSET, 104);
        let plan = ResetPlan {
            reset_system: Ok(0x7F00_1000),
            ticks_per_ms: 3_800_000,
            counted: true,
        };
        assert_eq!(
            plan.to_string(),
            "origin-boot: bench build: ResetSystem is at 0x7f001000; the time-stamp counter counted, over the firmware's Stall, 3800000 ticks a millisecond"
        );
    }

    const RSDP_AT: u64 = 0x9000_0014;
    const XSDT_AT: u64 = 0x9000_1000;
    const FADT_AT: u64 = 0x9000_2000;
    const OTHER_AT: u64 = 0x9000_3000;

    fn seal(t: &mut [u8], at: usize) {
        t[at] = 0;
        let sum = t.iter().fold(0u8, |s, &b| s.wrapping_add(b));
        t[at] = 0u8.wrapping_sub(sum);
    }

    fn rsdp(xsdt: u64) -> Vec<u8> {
        let mut r = vec![0u8; 36];
        r[..8].copy_from_slice(b"RSD PTR ");
        r[9..15].copy_from_slice(b"ORIGIN");
        r[15] = 2;
        r[20..24].copy_from_slice(&36u32.to_le_bytes());
        r[24..32].copy_from_slice(&xsdt.to_le_bytes());
        let first = r[..20].iter().fold(0u8, |s, &b| s.wrapping_add(b));
        r[8] = 0u8.wrapping_sub(first);
        let all = r.iter().fold(0u8, |s, &b| s.wrapping_add(b));
        r[32] = 0u8.wrapping_sub(all);
        r
    }

    fn xsdt(entries: &[u64]) -> Vec<u8> {
        let mut t = vec![0u8; 36];
        t[..4].copy_from_slice(b"XSDT");
        t[8] = 1;
        t[10..16].copy_from_slice(b"ORIGIN");
        t.extend(entries.iter().flat_map(|e| e.to_le_bytes()));
        let len = t.len() as u32;
        t[4..8].copy_from_slice(&len.to_le_bytes());
        seal(&mut t, 9);
        t
    }

    fn gas(space: u8, width: u8, offset: u8, access: u8, address: u64) -> [u8; 12] {
        let mut g = [0u8; 12];
        g[..4].copy_from_slice(&[space, width, offset, access]);
        g[4..].copy_from_slice(&address.to_le_bytes());
        g
    }

    /// An ACPI 6.6 FADT of 276 bytes with a DSDT, `flags`, and the reset register and value.
    fn fadt(flags: u32, reset: [u8; 12], value: u8) -> Vec<u8> {
        let mut t = vec![0u8; 276];
        t[..4].copy_from_slice(b"FACP");
        t[4..8].copy_from_slice(&276u32.to_le_bytes());
        t[8] = 6;
        t[10..16].copy_from_slice(b"ORIGIN");
        t[40..44].copy_from_slice(&0x7FF0_0000u32.to_le_bytes());
        t[112..116].copy_from_slice(&flags.to_le_bytes());
        t[116..128].copy_from_slice(&reset);
        t[128] = value;
        seal(&mut t, 9);
        t
    }

    const RESET_REG_SUP: u32 = 1 << 10;

    /// Reads `[at, at + out.len())` from `memory`, as the loader's `read` does from described
    /// memory.
    fn read_from(memory: &[(u64, Vec<u8>)], at: u64, out: &mut [u8]) -> bool {
        memory.iter().any(|(base, bytes)| {
            let src = at
                .checked_sub(*base)
                .and_then(|o| usize::try_from(o).ok())
                .and_then(|o| bytes.get(o..o.checked_add(out.len())?));
            src.map(|src| out.copy_from_slice(src)).is_some()
        })
    }

    fn reset_of(fadt_bytes: Vec<u8>) -> Result<ResetRegister, NoReset> {
        let memory = vec![
            (RSDP_AT, rsdp(XSDT_AT)),
            (XSDT_AT, xsdt(&[OTHER_AT, FADT_AT])),
            (OTHER_AT, xsdt(&[])),
            (FADT_AT, fadt_bytes),
        ];
        find_reset(RSDP_AT, &mut |at, out| read_from(&memory, at, out), |at| {
            (0xFED0_0000..0xFED0_1000).contains(&at)
        })
    }

    #[test]
    fn the_reset_register_of_qemu_and_of_the_rented_board() {
        // QEMU's q35: system I/O 0xCF9, the value 6.
        assert_eq!(
            reset_of(fadt(RESET_REG_SUP, gas(1, 8, 0, 1, 0xCF9), 6)),
            Ok(ResetRegister::Io {
                port: 0xCF9,
                value: 6
            })
        );
        // The rented board's shape (rental D's findings): system I/O 0xB2, the value 0xBE, the
        // access size left undefined.
        let board = reset_of(fadt(RESET_REG_SUP, gas(1, 8, 0, 0, 0xB2), 0xBE));
        assert_eq!(
            board,
            Ok(ResetRegister::Io {
                port: 0xB2,
                value: 0xBE
            })
        );
        assert_eq!(
            ResetLine(board).to_string(),
            "origin-boot: bench build: the FADT's reset register: write 0xbe to I/O port 0xb2"
        );
        // Memory-mapped I/O the map describes.
        assert_eq!(
            reset_of(fadt(RESET_REG_SUP, gas(0, 8, 0, 1, 0xFED0_0080), 0x0E)),
            Ok(ResetRegister::Memory {
                address: 0xFED0_0080,
                value: 0x0E
            })
        );
    }

    #[test]
    fn a_reset_register_that_breaks_a_rule_is_not_written() {
        let refused = [
            (fadt(0, gas(1, 8, 0, 1, 0xCF9), 6), NoReset::NotSupported),
            (
                fadt(RESET_REG_SUP, gas(1, 16, 0, 2, 0xCF9), 6),
                NoReset::Breaks,
            ),
            (
                fadt(RESET_REG_SUP, gas(1, 8, 1, 1, 0xCF9), 6),
                NoReset::Breaks,
            ),
            (
                fadt(RESET_REG_SUP, gas(3, 8, 0, 1, 0x66), 6),
                NoReset::Breaks,
            ),
            (
                fadt(RESET_REG_SUP, gas(2, 8, 0, 1, 0x0001_0000_0064), 6),
                NoReset::Space(AddressSpace::PciConfig),
            ),
            (
                fadt(RESET_REG_SUP, gas(1, 8, 0, 1, 0x1_0000), 6),
                NoReset::Port(0x1_0000),
            ),
            (fadt(RESET_REG_SUP, gas(1, 8, 0, 1, 0), 6), NoReset::Port(0)),
            (
                fadt(RESET_REG_SUP, gas(0, 8, 0, 1, 0x8000_0000), 6),
                NoReset::NotMmio(0x8000_0000),
            ),
            (
                fadt(RESET_REG_SUP, gas(0, 8, 0, 1, 0), 6),
                NoReset::NotMmio(0),
            ),
        ];
        for (table, want) in refused {
            assert_eq!(reset_of(table), Err(want), "{want:?}");
        }
        // A FADT that does not sum to zero is refused by the shared parser.
        let mut bad = fadt(RESET_REG_SUP, gas(1, 8, 0, 1, 0xCF9), 6);
        bad[200] ^= 1;
        assert_eq!(reset_of(bad), Err(NoReset::Table(ErrorKind::Checksum)));
        // A FADT declaring more than the loader reads.
        let mut long = fadt(RESET_REG_SUP, gas(1, 8, 0, 1, 0xCF9), 6);
        long[4..8].copy_from_slice(&2048u32.to_le_bytes());
        assert_eq!(reset_of(long), Err(NoReset::TooLong(2048)));
    }

    #[test]
    fn no_fadt_no_register() {
        let memory = vec![(RSDP_AT, rsdp(XSDT_AT)), (XSDT_AT, xsdt(&[OTHER_AT]))];
        let mut read = |at, out: &mut [u8]| read_from(&memory, at, out);
        assert_eq!(
            find_reset(RSDP_AT, &mut read, |_| false),
            Err(NoReset::Missing(Missing::EntryUnreadable(OTHER_AT)))
        );
        let memory = vec![
            (RSDP_AT, rsdp(XSDT_AT)),
            (XSDT_AT, xsdt(&[OTHER_AT])),
            (OTHER_AT, xsdt(&[])),
        ];
        let mut read = |at, out: &mut [u8]| read_from(&memory, at, out);
        let none = find_reset(RSDP_AT, &mut read, |_| false);
        assert_eq!(none, Err(NoReset::Missing(Missing::NotListed)));
        assert_eq!(
            ResetLine(none).to_string(),
            "origin-boot: bench build: the FADT gives no usable reset register: the XSDT lists no FADT"
        );
        assert_eq!(
            find_reset(0, &mut read, |_| false),
            Err(NoReset::Missing(Missing::NoRsdp))
        );
    }

    #[test]
    fn memory_mapped_io_is_what_the_map_says_it_is() {
        let region = |base: u64, pages: u64, kind: RegionKind| MemoryRegion {
            base,
            pages,
            kind: kind as u32,
            reserved: 0,
        };
        let map = [
            region(0x1000, 0x10, RegionKind::Usable),
            region(0xFED0_0000, 1, RegionKind::Mmio),
        ];
        assert!(in_mmio(&map, 0xFED0_0000));
        assert!(in_mmio(&map, 0xFED0_0FFF));
        assert!(!in_mmio(&map, 0xFED0_1000));
        assert!(!in_mmio(&map, 0x2000));
        assert!(!in_mmio(&map, 0));
    }

    #[test]
    fn the_reset_lines() {
        let soon = |reset_system, register| ResetSoon {
            reset_system,
            register,
        };
        assert_eq!(
            soon(true, true).to_string(),
            "origin-boot: bench build: a cold reset through the firmware's ResetSystem in 5 s, and the FADT's reset register 2 s later if the machine still runs"
        );
        assert_eq!(
            soon(false, true).to_string(),
            "origin-boot: bench build: a reset through the FADT's reset register in 7 s: ResetSystem is not called"
        );
        assert!(soon(true, false).to_string().contains("a halt 2 s later"));
        assert!(
            soon(false, false)
                .to_string()
                .contains("no reset: ResetSystem is not called")
        );
        let register = Ok(ResetRegister::Io {
            port: 0xCF9,
            value: 6,
        });
        assert_eq!(
            Fallback {
                tried: Tried::NotCalled,
                register
            }
            .to_string(),
            "origin-boot: bench build: ResetSystem was not called (BENCH-AFTER fadt): writing 0x6 to I/O port 0xcf9, the FADT's reset register"
        );
        assert_eq!(
            Fallback {
                tried: Tried::Returned,
                register: Err(NoReset::NotSupported)
            }
            .to_string(),
            "origin-boot: bench build: ResetSystem returned, and the FADT gives no usable reset register (the FADT's flags give no reset register (RESET_REG_SUP clear)): halting"
        );
    }

    // TASKS item 128.

    #[test]
    fn the_bisection_mode_skips_0xcf9_and_counts_os_types() {
        let cf9 = Ok(ResetRegister::Io {
            port: 0xCF9,
            value: 0x06,
        });
        assert_eq!(bisect_register(cf9), Err(NoReset::Cf9InBisect));
        let smi = Ok(ResetRegister::Io {
            port: 0xB2,
            value: 0xBE,
        });
        assert_eq!(bisect_register(smi), smi);
        let line = Fallback {
            tried: Tried::NotCalledSwitch,
            register: bisect_register(cf9),
        }
        .to_string();
        assert_eq!(
            line,
            "origin-boot: bench build: ResetSystem was not called (BENCH-FCH's noreset), and the FADT gives no usable reset register (RESET_REG is I/O port 0xcf9, which the bisection mode never writes (ADR-0002 Amendment 6)): halting"
        );
        assert!(After::SwitchNoReset.skips_reset_system() && !After::SwitchNoReset.enters_kernel());
        let none = crate::memmap::OsTypes {
            descriptors: 61,
            count: 0,
            first: 0,
        };
        assert_eq!(
            OsTypesLine(Some(none)).to_string(),
            "origin-boot: bench build: bisect: the memory map the page tables were built from holds no region of an OS-defined type (0x80000000 and above), in 61 descriptors"
        );
        let some = crate::memmap::OsTypes {
            descriptors: 61,
            count: 2,
            first: 0x8000_0003,
        };
        // Ruling Q7 of TASKS item 131: a count above zero is printed, and voids the second
        // question's answer for that run.
        assert!(OsTypesLine(Some(some)).to_string().ends_with(
            "holds 2 region(s) of an OS-defined type (0x80000000 and above), the first 0x80000003, in 61 descriptors: this run does not answer whether the call returns without Origin's OS-defined types"
        ));
        assert!(OsTypesLine(None).to_string().contains("was not read"));
        assert!(
            BISECT_LINE.contains("bench build: bisect")
                && BISECT_LAST_LINE.contains("bench build: bisect")
        );
    }

    #[test]
    fn the_minimal_mode_skips_a_reset_register_at_0xcf9_and_keeps_any_other() {
        let cf9 = Ok(ResetRegister::Io {
            port: 0xCF9,
            value: 0x06,
        });
        assert_eq!(minimal_register(cf9), Err(NoReset::Cf9InMinimal));
        // The rented board's: the SMI command port.
        let smi = Ok(ResetRegister::Io {
            port: 0xB2,
            value: 0xBE,
        });
        assert_eq!(minimal_register(smi), smi);
        let memory = Ok(ResetRegister::Memory {
            address: 0xFED8_0000,
            value: 0x06,
        });
        assert_eq!(minimal_register(memory), memory);
        assert_eq!(
            minimal_register(Err(NoReset::NotSupported)),
            Err(NoReset::NotSupported)
        );
        let line = Fallback {
            tried: Tried::NotCalled,
            register: minimal_register(cf9),
        }
        .to_string();
        assert!(
            line.ends_with("(RESET_REG is I/O port 0xcf9, which the minimal mode never writes (ADR-0002 Amendment 5)): halting"),
            "{line}"
        );
    }

    #[test]
    fn the_table_line_says_same_or_where_it_differs() {
        let copy = [1u8, 2, 3, 4];
        assert_eq!(first_difference(&copy, |i| copy[i]), None);
        assert_eq!(
            first_difference(&copy, |i| if i == 2 { 9 } else { copy[i] }),
            Some(2)
        );
        let same = TableLine {
            live_at: 0x9F00_0000,
            copied_from: 0x9F00_0000,
            size: 192,
            live: Live::Read {
                first_difference: None,
                exit: 0x9E00_1234,
                crc: 0xDEAD_BEEF,
            },
            copy_exit: 0x9E00_1234,
            copy_crc: 0xDEAD_BEEF,
        };
        assert_eq!(
            same.to_string(),
            "origin-boot: bench build: the boot services table (ADR-0002 Amendment 4's addition): the system table names it at 0x9f000000; the same as the loader's copy, byte for byte over its 192 bytes; ExitBootServices live 0x9e001234, copy 0x9e001234; header CRC32 live 0xdeadbeef, copy 0xdeadbeef"
        );
        let moved = TableLine {
            live_at: 0x9F10_0000,
            live: Live::Read {
                first_difference: Some(160),
                exit: 0x9E00_5678,
                crc: 0x1234_5678,
            },
            ..same
        };
        let line = moved.to_string();
        assert!(line.contains("at 0x9f100000, not at 0x9f000000, where the loader's copy was taken from; it differs from the loader's copy from byte 160 of 192; ExitBootServices live 0x9e005678, copy 0x9e001234"), "{line}");
        let unchecked = TableLine {
            live: Live::Unchecked,
            ..moved
        };
        assert!(
            unchecked
                .to_string()
                .contains("does not check, so it was not compared")
        );
    }

    /// An ACPI table with `signature` and `body`.
    fn table(signature: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut t = vec![0u8; 36];
        t[..4].copy_from_slice(signature);
        t[8] = 1;
        t[10..16].copy_from_slice(b"ORIGIN");
        t.extend_from_slice(body);
        let len = t.len() as u32;
        t[4..8].copy_from_slice(&len.to_le_bytes());
        seal(&mut t, 9);
        t
    }

    #[test]
    fn the_stand_in_table_is_found_by_its_signature_and_bounded() {
        use crate::fch::{STAND_IN_MAX, find_stand_in};
        const TABLE_AT: u64 = 0x9000_4000;
        let found = |entries: &[u64], body: &[u8]| {
            let memory = vec![
                (RSDP_AT, rsdp(XSDT_AT)),
                (XSDT_AT, xsdt(entries)),
                (OTHER_AT, table(b"SSDT", b"x")),
                (TABLE_AT, table(b"OXFC", body)),
            ];
            find_stand_in(RSDP_AT, &mut |at, out| read_from(&memory, at, out))
                .map(|(bytes, n)| bytes[..n].to_vec())
        };
        assert_eq!(
            found(&[OTHER_AT, TABLE_AT], b"mine stale1"),
            Some(b"mine stale1".to_vec())
        );
        assert_eq!(found(&[OTHER_AT], b"mine"), None, "not listed");
        assert_eq!(
            found(&[OTHER_AT, TABLE_AT], &[b'x'; STAND_IN_MAX + 1]),
            None,
            "too long"
        );
        assert_eq!(
            found(&[TABLE_AT], &[b'y'; STAND_IN_MAX]).map(|b| b.len()),
            Some(STAND_IN_MAX)
        );
        assert_eq!(find_stand_in(0, &mut |_, _| true), None, "no RSDP");
    }
}
