//! The kernel console: a UART and, when the firmware offers one, its framebuffer; shared by
//! every processor (ADR-0010; CLAUDE.md §19, §69).
//!
//! Output goes to the framebuffer and then the UART, from the first line on. The UART starts
//! at the PC's COM1 port; [`adopt_firmware_uart`], once the ACPI tables can be read, moves it
//! to the UART the firmware names in its SPCR table, if it names one this kernel can drive,
//! and sends the UART everything printed before the move. Neither sink is required: a missing
//! or unusable framebuffer or UART never stops the boot, and a UART that stops taking bytes is
//! given up on after one bounded wait, reported on the framebuffer.
//!
//! A [`Console`] collects one line and writes it holding the console lock, with interrupts
//! disabled. So lines from different processors, or from an interrupt handler on the same
//! processor, never interleave. Lines longer than [`LINE`] bytes go out in [`LINE`]-byte
//! pieces, each one whole. The framebuffer is drawn first and the UART second, so a UART that
//! holds a line up cannot delay what reaches the screen. The drawing itself is
//! `framekernel::fbconsole`, where the rules are tested.
//!
//! Exception and panic reports use [`Console::urgent`], which waits for the lock only
//! about as long as another processor needs to send one line. A processor that faulted
//! while holding the lock, or that waits on a processor that died holding it, still gets
//! its report out, at the risk of mixing it into another line: the cursor and the UART's
//! address are kept outside the lock for that. A mixed-up report is all that can happen
//! there: every pixel goes through the sink's bounds check, and the UART registers are the
//! console's own.
//!
//! Interrupts stay disabled while a line is sent. In QEMU that is microseconds; at 115 200
//! baud on real hardware it is about 9 ms for 100 bytes. Production logging will go to a
//! memory ring drained by a thread; this serial console is for development (CLAUDE.md §69).
#![forbid(unsafe_code)]

use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use framekernel::fbconsole::{Cursor, PixelSink, Screen, ScreenError};
use framekernel::paging::layout;
use framekernel::serial::{
    DIVISOR, EarlyLog, LCR_8N1, LineSettings, LineSetup, Register, Uart16550, UartRegisters,
};
use framekernel::spcr::{self, Access, Spcr, UartAddress};
use origin_boot_info::{BootInfo, Framebuffer, MemoryRegion, overlaps_ram};
use origin_frame::addr::PhysAddr;
use origin_frame::mmio::MmioRegion;
use origin_frame::once::SetOnce;
use origin_frame::port::PortRange;
use origin_frame::sync::SpinLock;

use crate::COM1_BASE;

/// How the kernel reaches a UART's registers.
#[derive(Clone, Copy, Debug)]
pub(crate) enum KernelUart {
    /// Port-mapped, one byte apart.
    Port(PortRange),
    /// In the MMIO window: `stride` bytes between registers, each access `access` wide.
    Mmio {
        region: MmioRegion,
        stride: u8,
        access: Access,
    },
    /// No usable UART: reads as all ones, like a floating bus; writes go nowhere.
    Absent,
}

/// COM1's ports, claimed at the first use of the UART (idempotent).
static COM1_PORTS: SetOnce<PortRange> = SetOnce::new();

fn com1_ports() -> Option<PortRange> {
    if let Some(&ports) = COM1_PORTS.get() {
        return Some(ports);
    }
    if let Some(ports) = PortRange::claim(COM1_BASE, 8) {
        let _ = COM1_PORTS.set(ports);
    }
    COM1_PORTS.get().copied()
}

impl KernelUart {
    /// The PC's COM1 UART (claimed once, here).
    pub(crate) fn com1() -> Self {
        com1_ports().map_or(Self::Absent, Self::Port)
    }

    /// The UART at I/O port `base`, if the ports can be claimed (a port the SPCR named and
    /// `spcr::parse` allowed; COM1 keeps its one claim).
    fn port(base: u16) -> Option<Self> {
        if base == COM1_BASE {
            return com1_ports().map(Self::Port);
        }
        PortRange::claim(base, 8).map(Self::Port)
    }

    /// Whether this is COM1 at its usual port.
    fn is_com1(self) -> bool {
        matches!(self, Self::Port(ports) if ports.base() == COM1_BASE)
    }
}

impl UartRegisters for KernelUart {
    fn read(&mut self, reg: Register) -> u8 {
        match *self {
            Self::Port(ports) => ports.read8(reg.offset()),
            Self::Mmio {
                region,
                stride,
                access,
            } => {
                let at = usize::from(reg.offset()) * usize::from(stride);
                match access {
                    Access::Byte => region.read8(at),
                    Access::Word => region.read16(at) as u8,
                    Access::Dword => region.read32(at) as u8,
                }
            }
            Self::Absent => 0xFF,
        }
    }

    fn write(&mut self, reg: Register, value: u8) {
        match *self {
            Self::Port(ports) => ports.write8(reg.offset(), value),
            Self::Mmio {
                region,
                stride,
                access,
            } => {
                let at = usize::from(reg.offset()) * usize::from(stride);
                match access {
                    Access::Byte => region.write8(at, value),
                    Access::Word => region.write16(at, u16::from(value)),
                    Access::Dword => region.write32(at, u32::from(value)),
                }
            }
            Self::Absent => {}
        }
    }
}

/// The framebuffer as a pixel sink: 32-bit volatile writes through its typed region.
#[derive(Clone, Copy, Debug)]
struct FramebufferMemory {
    region: MmioRegion,
}

impl PixelSink for FramebufferMemory {
    fn write32(&mut self, offset: usize, value: u32) {
        self.region.write32(offset, value);
    }
}

/// The framebuffer console, once [`attach_framebuffer`] has run (set once; the kernel's own
/// page tables may then force [`withdraw_framebuffer`]).
static SCREEN: SetOnce<(Screen, FramebufferMemory)> = SetOnce::new();
/// The screen was withdrawn: the kernel's tables could not map the framebuffer.
static SCREEN_WITHDRAWN: AtomicBool = AtomicBool::new(false);

/// The screen, unless it is unset or was withdrawn.
fn screen() -> Option<(Screen, FramebufferMemory)> {
    if SCREEN_WITHDRAWN.load(Ordering::Acquire) {
        return None;
    }
    SCREEN.get().copied()
}
/// Where the next glyph goes, kept outside every lock for urgent reports (see the module
/// documentation).
static CURSOR: Cursor = Cursor::new();

/// The UART an urgent report writes to when it cannot take the console lock: COM1 until
/// [`adopt_firmware_uart`] moves it. Written only while the boot processor runs alone.
static MOVED_UART: SetOnce<KernelUart> = SetOnce::new();
/// The move was undone (the new UART took no bytes): back to COM1.
static FELL_BACK_TO_COM1: AtomicBool = AtomicBool::new(false);

/// Bytes of output kept for the UART that [`adopt_firmware_uart`] moves to.
const EARLY_BYTES: usize = 8 * 1024;

/// The console's state, under [`SINKS`].
struct Sinks {
    uart: Uart16550<KernelUart>,
    /// Cleared when the UART stops taking bytes (a transmit poll ran out): the kernel gave up
    /// on it after one bounded wait, so it costs nothing from then on.
    uart_alive: bool,
    /// The firmware's own UART settings on the COM1 port, read before the console's first
    /// programming, so "as is" in an SPCR can be honoured for that port.
    firmware_line: LineSettings,
    /// What has been sent, until the UART is settled ([`adopt_firmware_uart`]).
    early: EarlyLog<EARLY_BYTES>,
    early_open: bool,
}

impl Sinks {
    /// The state before `init` (a very early report): COM1, programmed the usual way.
    fn default_console() -> Self {
        let mut uart = unowned_uart();
        uart.init();
        Self {
            uart,
            uart_alive: true,
            firmware_line: LineSettings {
                line_control: 0,
                divisor: DIVISOR,
            },
            early: EarlyLog::new(),
            early_open: true,
        }
    }

    /// Sends `bytes` to every sink: framebuffer, the log kept for the UART, the UART.
    fn write(&mut self, bytes: &[u8]) {
        draw(bytes);
        if self.early_open {
            self.early.push(bytes);
        }
        if self.uart_alive && emit(&mut self.uart, bytes).is_err() {
            self.uart_alive = false;
            draw(b"origin-kernel: console: the UART stopped taking bytes; serial output off\n");
        }
    }
}

/// The programmed UART and the rest of the console, once [`init`] has run.
static SINKS: SpinLock<Option<Sinks>> = SpinLock::new(None);

/// Bytes a [`Console`] holds before it sends them.
const LINE: usize = 160;
/// Lock attempts an urgent report makes before it writes without the lock: on the order of
/// 100 ms, longer than another processor needs for one line.
const URGENT_ATTEMPTS: u32 = 1 << 22;

/// Hands the programmed UART to the console. `firmware_line` is how the firmware had left it.
/// Called once by `_start`.
pub(crate) fn init(uart: Uart16550<KernelUart>, firmware_line: LineSettings) {
    crate::cpu::without_interrupts(|| {
        *SINKS.lock() = Some(Sinks {
            uart,
            uart_alive: true,
            firmware_line,
            early: EarlyLog::new(),
            early_open: true,
        });
    });
}

/// Sends `bytes`, turning each LF into CR LF. Stops at the first byte the UART will not take,
/// which costs one bounded wait.
fn emit(uart: &mut Uart16550<KernelUart>, bytes: &[u8]) -> fmt::Result {
    uart.write_bytes(bytes)
}

/// Lines drawn and the cycles they took, for the report in [`prove`] (CLAUDE.md rule 21).
static DRAWN_LINES: AtomicU64 = AtomicU64::new(0);
static DRAW_CYCLES: AtomicU64 = AtomicU64::new(0);
static DRAW_WORST: AtomicU64 = AtomicU64::new(0);

/// Draws `bytes` on the framebuffer, if there is one.
fn draw(bytes: &[u8]) {
    if let Some((screen, mut memory)) = screen() {
        let start = crate::cpu::rdtsc();
        screen.write(&CURSOR, &mut memory, bytes);
        let cycles = crate::cpu::rdtsc().wrapping_sub(start);
        DRAWN_LINES.fetch_add(1, Ordering::Relaxed);
        DRAW_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        DRAW_WORST.fetch_max(cycles, Ordering::Relaxed);
    }
}

/// Why the framebuffer the loader described will not be the console.
#[derive(Clone, Copy, Debug)]
enum Unusable {
    /// The description, or the mode it gives.
    Description(ScreenError),
    /// The loader did not map it.
    NotMapped,
    /// The loader mapped it at an address this kernel would not have.
    Misplaced,
    /// The memory map says it is memory.
    OverlapsRam,
}

impl fmt::Display for Unusable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Description(why) => write!(f, "{why:?}"),
            Self::NotMapped => f.write_str("the loader did not map it"),
            Self::Misplaced => f.write_str("the loader mapped it somewhere else"),
            Self::OverlapsRam => f.write_str("it overlaps RAM"),
        }
    }
}

fn screen_for(
    framebuffer: &Framebuffer,
    regions: &[MemoryRegion],
) -> Result<(Screen, FramebufferMemory), Unusable> {
    let screen = Screen::new(framebuffer).map_err(Unusable::Description)?;
    if framebuffer.virtual_base == 0 {
        return Err(Unusable::NotMapped);
    }
    let mapping = layout::framebuffer_mapping(framebuffer).ok_or(Unusable::NotMapped)?;
    if mapping.first_pixel != framebuffer.virtual_base {
        return Err(Unusable::Misplaced);
    }
    if overlaps_ram(regions, framebuffer.base, framebuffer.visible_bytes()) {
        return Err(Unusable::OverlapsRam);
    }
    let region = crate::vm::framebuffer_region(framebuffer, regions).ok_or(Unusable::Misplaced)?;
    Ok((screen, FramebufferMemory { region }))
}

/// Starts drawing on the firmware's framebuffer, if the loader passed a usable one. Called
/// once, by the boot processor, as soon as the boot info is validated. From here on, every
/// line also goes to the screen.
pub(crate) fn attach_framebuffer(out: &mut Console, info: &BootInfo, regions: &[MemoryRegion]) {
    use core::fmt::Write as _;
    let framebuffer = info.framebuffer;
    if !framebuffer.is_present() {
        let _ = writeln!(
            out,
            "origin-kernel: console: the loader found no framebuffer; output goes to the UART only"
        );
        return;
    }
    match screen_for(&framebuffer, regions) {
        Ok((screen, mut memory)) => {
            let _ = SCREEN.set((screen, memory));
            screen.clear(&CURSOR, &mut memory);
            let _ = writeln!(
                out,
                "origin-kernel: console: framebuffer {}x{} at {:#x}, stride {}, red {:#x} green {:#x} blue {:#x}: {}x{} text cells, scale {}",
                framebuffer.width,
                framebuffer.height,
                framebuffer.base,
                framebuffer.stride,
                framebuffer.red_mask,
                framebuffer.green_mask,
                framebuffer.blue_mask,
                screen.columns(),
                screen.rows(),
                screen.scale()
            );
        }
        Err(reason) => {
            let _ = writeln!(
                out,
                "origin-kernel: console: framebuffer at {:#x} not used ({reason}); output goes to the UART only",
                framebuffer.base
            );
        }
    }
}

/// Stops drawing: the kernel's own page tables could not map the framebuffer. Runs before
/// those tables are loaded.
pub(crate) fn withdraw_framebuffer(out: &mut Console) {
    use core::fmt::Write as _;
    // Said while the old tables still map the screen, so the reason appears on it too.
    let _ = writeln!(
        out,
        "origin-kernel: console: the kernel's page tables cannot map the framebuffer; output goes to the UART only"
    );
    SCREEN_WITHDRAWN.store(true, Ordering::Release);
}

/// Whether the console draws on the framebuffer.
pub(crate) fn has_screen() -> bool {
    screen().is_some()
}

/// Moves the console's UART to the one the firmware names in its ACPI SPCR table, when it names
/// one this kernel can drive: the UART's address comes from there, and 0x3F8 is only used
/// when the firmware gives nothing better (ADR-0010). What was printed before this goes to
/// the new UART too, so its log is complete. Called once, when the kernel's page tables are
/// loaded, because only then can firmware tables be mapped; anything printed before this
/// still went to COM1 and to the framebuffer.
pub(crate) fn adopt_firmware_uart(out: &mut Console, rsdp: u64, regions: &[MemoryRegion]) {
    use core::fmt::Write as _;
    let settle = |out: &mut Console, message: fmt::Arguments<'_>| {
        let _ = writeln!(out, "origin-kernel: console: {message}");
        crate::cpu::without_interrupts(|| {
            if let Some(sinks) = SINKS.lock().as_mut() {
                sinks.early_open = false;
                sinks.early.clear();
            }
        });
    };

    let table = if rsdp == 0 {
        Err("no ACPI RSDP")
    } else {
        crate::firmware::acpi_table(rsdp, b"SPCR")
    };
    let table = match table {
        Ok(table) => table,
        Err(why) => {
            settle(
                out,
                format_args!(
                    "no usable ACPI SPCR table ({why}); the UART stays at I/O port {COM1_BASE:#x}"
                ),
            );
            return;
        }
    };
    let spcr = match spcr::parse(table) {
        Ok(spcr) => spcr,
        Err(why) => {
            settle(
                out,
                format_args!(
                    "the ACPI SPCR table was refused ({why:?}); the UART stays at I/O port {COM1_BASE:#x}"
                ),
            );
            return;
        }
    };
    match uart_for(&spcr, regions) {
        Ok(uart) => move_to(out, &spcr, uart),
        Err(why) => settle(
            out,
            format_args!(
                "the ACPI SPCR names a UART at {} that cannot be used ({why}); the UART stays at I/O port {COM1_BASE:#x}",
                Named(&spcr)
            ),
        ),
    }
}

/// How an SPCR's UART is written in the boot log.
struct Named<'a>(&'a Spcr);

impl fmt::Display for Named<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.address {
            UartAddress::Port(port) => write!(f, "I/O port {port:#x}"),
            UartAddress::Mmio {
                base,
                stride,
                access,
            } => write!(
                f,
                "physical address {base:#x} (registers {stride} bytes apart, {} bytes wide)",
                access.bytes()
            ),
        }
    }
}

/// Maps the UART `spcr` names and checks something answers there.
fn uart_for(spcr: &Spcr, regions: &[MemoryRegion]) -> Result<KernelUart, &'static str> {
    let uart = match spcr.address {
        UartAddress::Port(port) => KernelUart::port(port).ok_or("its ports are not available")?,
        UartAddress::Mmio {
            base,
            stride,
            access,
        } => {
            if overlaps_ram(regions, base, spcr.address.span()) {
                return Err("its registers are in RAM");
            }
            let region = crate::vm::map_mmio(PhysAddr::new(base), spcr.address.span())
                .map_err(|_| "its registers cannot be mapped")?;
            KernelUart::Mmio {
                region,
                stride,
                access,
            }
        }
    };
    let mut probe = uart;
    // A floating bus reads as all ones, where a 16550 never sets every status bit at once:
    // nothing answers there, so moving would lose the console.
    if probe.read(Register::LineStatus) == 0xFF {
        return Err("nothing answers at that address");
    }
    Ok(uart)
}

/// The move itself. `uart` is the SPCR's UART, already mapped and probed.
fn move_to(out: &mut Console, spcr: &Spcr, uart: KernelUart) {
    use core::fmt::Write as _;
    // Printed before the move, so someone watching the old UART sees where it went.
    let _ = writeln!(
        out,
        "origin-kernel: console: the ACPI SPCR (revision {}, interface {:#x}) names a UART at {}: moving the console there",
        spcr.revision,
        spcr.interface,
        Named(spcr)
    );
    let moved = crate::cpu::without_interrupts(|| {
        let mut sinks = SINKS.lock();
        let sinks = sinks.as_mut()?;
        Some(move_uart(sinks, spcr, uart))
    });
    let (setup, replayed, dropped, stayed) = match moved {
        Some(result) => result,
        None => return,
    };
    if stayed {
        let _ = writeln!(
            out,
            "origin-kernel: console: the UART at {} takes no bytes; the UART stays at I/O port {COM1_BASE:#x}",
            Named(spcr)
        );
        return;
    }
    let speed = match setup {
        LineSetup::Divisor(_) => "programmed to the firmware's rate",
        LineSetup::Keep => "as the firmware set it",
    };
    let _ = writeln!(
        out,
        "origin-kernel: console: the UART is now at {}, {speed}; {replayed} bytes of earlier output sent to it again, {dropped} did not fit",
        Named(spcr)
    );
}

/// [`move_to`]'s work under the lock: the new UART takes over, or, if it takes no bytes, the
/// old one stays. Returns the speed setup used, the bytes replayed, the bytes that did not
/// fit in the log, and whether the console stayed where it was.
fn move_uart(sinks: &mut Sinks, spcr: &Spcr, uart: KernelUart) -> (LineSetup, usize, u64, bool) {
    let same_port = uart.is_com1();
    let mut new = Uart16550::new(uart);
    // COM1 is the UART the console has been writing to: its latch was read before the
    // console's own programming, so the firmware's value is the one that counts. Elsewhere,
    // the latch still holds what the firmware left.
    let latch = if same_port {
        sinks.firmware_line.divisor
    } else {
        new.line_settings().divisor
    };
    let setup = spcr.line_setup(latch);
    let reprogrammed = if same_port {
        match setup {
            LineSetup::Divisor(divisor) => {
                sinks.uart.configure(setup);
                divisor != DIVISOR
            }
            LineSetup::Keep if latch != 0 => {
                // "As is": what the firmware left, not the console's own 115 200 baud,
                // and the log so far went out at another speed or format.
                sinks.uart.restore(sinks.firmware_line);
                latch != DIVISOR || sinks.firmware_line.line_control != LCR_8N1
            }
            // The latch reads zero and the table names no rate: the firmware left nothing
            // usable behind, so the console's own programming stays.
            LineSetup::Keep => false,
        }
    } else {
        new.configure(setup);
        true
    };
    if !same_port {
        sinks.uart = new;
        let _ = MOVED_UART.set(uart);
        sinks.uart_alive = true;
    }
    // A new UART, or a new speed on the same one: the log so far has not been readable there.
    // Send it again. If a new UART will not take it, the old one stays.
    let log = core::mem::replace(&mut sinks.early, EarlyLog::new());
    sinks.early_open = false;
    let dropped = log.dropped();
    let mut replayed = 0;
    if reprogrammed {
        match emit(&mut sinks.uart, log.as_bytes()) {
            Ok(()) => replayed = log.as_bytes().len(),
            Err(_) if !same_port => {
                sinks.uart = unowned_com1();
                FELL_BACK_TO_COM1.store(true, Ordering::Release);
                sinks.uart_alive = true;
                return (setup, 0, dropped, true);
            }
            Err(_) => sinks.uart_alive = false,
        }
    }
    (setup, replayed, dropped, false)
}

/// One line at a time to the console's sinks. See the module documentation.
pub(crate) struct Console {
    line: [u8; LINE],
    len: usize,
    urgent: bool,
}

impl Console {
    pub(crate) const fn new() -> Self {
        Self {
            line: [0; LINE],
            len: 0,
            urgent: false,
        }
    }

    /// For exception and panic reports: never waits on the lock for long.
    pub(crate) const fn urgent() -> Self {
        Self {
            line: [0; LINE],
            len: 0,
            urgent: true,
        }
    }

    /// Sends the buffered bytes, if any.
    fn flush(&mut self) {
        let Some(bytes) = self.line.get(..self.len) else {
            return;
        };
        if bytes.is_empty() {
            return;
        }
        let urgent = self.urgent;
        crate::cpu::without_interrupts(|| {
            let guard = if urgent {
                (0..URGENT_ATTEMPTS).find_map(|_| {
                    let g = SINKS.try_lock();
                    if g.is_none() {
                        core::hint::spin_loop();
                    }
                    g
                })
            } else {
                Some(SINKS.lock())
            };
            match guard {
                Some(mut held) => {
                    // A report before `init` (a very early panic) programs the UART first.
                    held.get_or_insert_with(Sinks::default_console).write(bytes);
                }
                // The holder is stuck (this processor faulted inside `flush`, or another
                // died holding the lock): send without it.
                None => {
                    draw(bytes);
                    let _ = emit(&mut unowned_uart(), bytes);
                }
            }
        });
        self.len = 0;
    }
}

/// COM1, as the console left it: for the move that did not work.
fn unowned_com1() -> Uart16550<KernelUart> {
    Uart16550::new(KernelUart::com1())
}

/// The console's UART outside the console's own state: for urgent reports that cannot get
/// the lock, and to program the UART for a report made before [`init`]. The worst a concurrent
/// writer can then cause is mixed bytes on the line.
fn unowned_uart() -> Uart16550<KernelUart> {
    let uart = match (FELL_BACK_TO_COM1.load(Ordering::Acquire), MOVED_UART.get()) {
        (false, Some(&uart)) => uart,
        _ => KernelUart::com1(),
    };
    Uart16550::new(uart)
}

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if self.len == LINE {
                self.flush();
            }
            if let Some(slot) = self.line.get_mut(self.len) {
                *slot = b;
                self.len += 1;
            }
            if b == b'\n' {
                self.flush();
            }
        }
        Ok(())
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        self.flush();
    }
}

/// One exception report, from the trap entry's hook: it gets out even if this processor
/// faulted inside the console.
pub(crate) fn report_exception(r: &origin_frame::trap::ExceptionReport<'_>) {
    use core::fmt::Write as _;
    use framekernel::descriptors::exception_name;
    let mut com1 = Console::urgent();
    let _ = writeln!(
        com1,
        "origin-kernel: exception {} ({}) at rip {:#x}, error code {:#x}",
        r.vector,
        exception_name(r.vector),
        r.rip,
        r.error_code
    );
    if r.vector == 3 {
        let _ = writeln!(com1, "origin-kernel: breakpoint handled; resuming");
        return;
    }
    let _ = writeln!(
        com1,
        "origin-kernel: rsp {:#x} rflags {:#x} cs {:#x} rax {:#x} rbx {:#x} rcx {:#x} rdx {:#x}",
        r.rsp, r.rflags, r.cs, r.rax, r.rbx, r.rcx, r.rdx
    );
    if let Some(report) = r.machine_check {
        let _ = writeln!(com1, "origin-kernel: machine check: {report}");
    }
}

/// Lines each processor prints in the proof.
const PROOF_LINES: u64 = 20;
/// Letters after each proof line's prefix: one letter per processor, repeated, so a line
/// that mixed two processors' bytes shows it.
const PROOF_PAYLOAD: usize = 60;

static PRINTERS_READY: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static PRINTERS_DONE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static PRINT_GO: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Prints [`PROOF_LINES`] lines as processor `id`.
fn print_lines(id: u64) {
    use core::fmt::Write as _;
    let letter = char::from(b'a' + (id % 26) as u8);
    let mut out = Console::new();
    for k in 0..PROOF_LINES {
        let _ = write!(
            out,
            "origin-kernel: console: CPU {id} line {k} of {PROOF_LINES}: "
        );
        for _ in 0..PROOF_PAYLOAD {
            let _ = out.write_char(letter);
        }
        let _ = out.write_char('\n');
    }
}

/// An application processor's printer thread.
extern "sysv64" fn printer(_: u64) -> ! {
    use core::sync::atomic::Ordering;
    PRINTERS_READY.fetch_add(1, Ordering::AcqRel);
    while !PRINT_GO.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    print_lines(crate::cpu::this_cpu().get() as u64);
    PRINTERS_DONE.fetch_add(1, Ordering::AcqRel);
    crate::sched::exit()
}

/// Every online processor prints [`PROOF_LINES`] lines at the same time; the boot test
/// checks that every one of them arrived whole. Runs from the boot flow with every
/// scheduler active.
pub(crate) fn prove(
    com1: &mut Console,
    cpus: &[crate::cpu::CpuIndex],
    bsp: crate::cpu::CpuIndex,
    tsc_hz: u64,
) -> bool {
    use core::fmt::Write as _;
    use core::sync::atomic::Ordering;
    let mut aps = 0u64;
    for &id in cpus.iter().filter(|&&id| id != bsp) {
        if crate::sched::spawn(id, printer, 0).is_err() {
            let _ = writeln!(
                com1,
                "origin-kernel: console: cannot spawn on CPU {id}: console failed"
            );
            return false;
        }
        aps += 1;
    }
    crate::sched::yield_until(|| PRINTERS_READY.load(Ordering::Acquire) == aps, tsc_hz, 2);
    PRINT_GO.store(true, Ordering::Release);
    print_lines(bsp.get() as u64);
    crate::sched::yield_until(|| PRINTERS_DONE.load(Ordering::Acquire) == aps, tsc_hz, 5);
    let ok = PRINTERS_DONE.load(Ordering::Acquire) == aps;
    let _ = writeln!(
        com1,
        "origin-kernel: console: {} processors printed {PROOF_LINES} lines each at once{}",
        cpus.len(),
        if ok { "" } else { ": console failed" }
    );
    // What drawing costs, as measured on this machine (CLAUDE.md rule 21). Interrupts are off
    // while a line is drawn, so this is also the longest the framebuffer can hold them off.
    let lines = DRAWN_LINES.load(Ordering::Relaxed);
    if let Some(average) = DRAW_CYCLES.load(Ordering::Relaxed).checked_div(lines) {
        let micros = |cycles: u64| u128::from(cycles) * 1_000_000 / u128::from(tsc_hz.max(1));
        let _ = writeln!(
            com1,
            "origin-kernel: console: framebuffer: {lines} lines drawn so far, {} us each on average and {} us at worst",
            micros(average),
            micros(DRAW_WORST.load(Ordering::Relaxed))
        );
    }
    ok
}
