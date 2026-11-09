//! The kernel console: a UART and, when the firmware offers one, its framebuffer; shared by
//! every processor (ADR-0010; CLAUDE.md §19, §69).
//!
//! Every line goes to the UART and to the framebuffer, from the first line on. The UART starts
//! at the PC's COM1 port; [`adopt_firmware_uart`], once the ACPI tables can be read, moves it
//! to the UART the firmware names in its SPCR table, if it names one this kernel can drive,
//! and sends the UART everything printed before the move. Neither sink is required: a missing
//! or unusable framebuffer or UART never stops the boot, and a UART that stops taking bytes is
//! given up on after one bounded wait, reported on the framebuffer.
//!
//! A [`Console`] collects one line and hands it over in one critical section, holding the
//! console lock with interrupts disabled: it joins the queue of text for the UART and the
//! backlog of text for the screen, and no device is touched. So lines from different
//! processors, or from an interrupt handler on the same processor, never interleave on either
//! sink, and both sinks get them in the same order. Lines longer than [`LINE`] bytes go out in
//! [`LINE`]-byte pieces, each one whole. The UART comes first (ADR-0010 Amendment 1), so the
//! screen can never hold a line back from the log the machine's owner has to read when
//! something goes wrong: a writer that can wait has its line on the wire before it draws any
//! of it. The slow UART is what delays the screen, by what is queued ahead of the line, not
//! the reverse. (One writer's last section of drawing can run on into the line queued behind
//! its own, so a little of a line may be drawn before that line has gone out.)
//!
//! Both devices are slow in their own way, so each is worked at a section at a time, after the
//! line was handed over. Every section holds the console lock with interrupts off, and nothing
//! is held between sections, so any processor can take the next one, interrupts are taken
//! between them and no preempted holder can block anyone.
//!
//! The UART's time is the wire's: at 115 200 baud a byte takes 87 us, so 100 bytes take 8.7 ms,
//! as long as the kernel's 10 ms timer tick. A section does not wait for it. It reads the
//! line status once and, if the transmitter reports empty, writes one load, at most
//! [`TX_FIFO_BYTES`] bytes: a section is at most 17 register accesses, however slow the line,
//! because the 16 bytes of a 16550's FIFO need no further look at the status. (A UART with no
//! working FIFO takes a byte at a time.) Waiting for those 16 bytes to leave, 1.4 ms at 115 200
//! baud, is done between sections, with the lock free and interrupts as the writer has them.
//!
//! The framebuffer's time is uncached writes: under KVM on a real AMD EPYC 7763 one console
//! line, about 40,960 pixels, took 4.3 to 4.6 ms and up to 10 ms. A section writes at most
//! [`SECTION_PIXELS`] pixels; at the cost measured there (105 to 245 ns a pixel), that is about
//! 27 to 63 us.
//!
//! What a caller may do between sections depends on where it runs (see [`Flow`]): with
//! interrupts on they are simply back on between sections; the boot flow, which runs with them
//! off, opens a window once they can be taken at all ([`allow_interrupt_windows`]). Either way
//! it goes on until its line is on the UART, before it returns, and then until the line is on
//! the screen. A caller that has interrupts off for its own reasons, an interrupt handler, may
//! not wait: it hands its line over, sends one section and leaves. The rest of its line goes
//! out with the next writer that can wait, or the next report: the queue keeps its place.
//!
//! Exception and panic reports use [`Console::urgent`]. A report keeps interrupts off from
//! start to finish and takes the lock a section at a time like everyone else, so two reports at
//! once queue behind each other instead of waiting each other out. It goes on until everything
//! queued before it, and the report itself, are on the UART and on the screen, in that order.
//! Its wait for the lock is bounded, since a section is short, and needs no other processor:
//! one that faulted while holding the lock, or that waits on a processor that died holding it,
//! still gets its report out, at the risk of mixing it into another line: the cursor and the
//! UART's address are kept outside the lock for that. A mixed-up report is all that can happen
//! there: every pixel goes through the sink's bounds check, and the UART registers are the
//! console's own.
//!
//! The replay of the early log ([`adopt_firmware_uart`]) is not sent in sections: it runs once,
//! before the timer starts and before any other processor does, when no interrupt can arrive
//! and nobody can be waiting. Production logging will go to a memory ring drained by a thread;
//! this serial console is for development (CLAUDE.md §69).

use core::cell::UnsafeCell;
use core::fmt;
use core::mem::MaybeUninit;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use framekernel::fbconsole::{Backlog, Cursor, PixelSink, Screen, ScreenError};
use framekernel::paging::layout;
use framekernel::serial::{
    DIVISOR, EarlyLog, LCR_8N1, LineSettings, LineSetup, Pumped, Register, TX_FIFO_BYTES, TxQueue,
    Uart16550, UartRegisters,
};
use framekernel::spcr::{self, Access, Spcr, UartAddress};
use origin_boot_info::{BootInfo, Framebuffer, MemoryRegion, overlaps_ram};
use origin_sync::SpinLock;

use crate::COM1_BASE;

/// How the kernel reaches a UART's registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KernelUart {
    /// Port-mapped, one byte apart.
    Port(u16),
    /// In the MMIO window: `stride` bytes between registers, each access `access` wide.
    Mmio {
        base: u64,
        stride: u8,
        access: Access,
    },
}

impl KernelUart {
    /// The UART at I/O port `base`.
    ///
    /// # Safety
    ///
    /// The caller must run at CPL 0. The ports `base` to `base + 7` are written and read as
    /// a UART; the caller must have no reason to think something else lives there (COM1 by
    /// the PC platform's convention, or a port the SPCR named and `spcr::parse` allowed).
    pub(crate) const unsafe fn port(base: u16) -> Self {
        Self::Port(base)
    }

    /// The UART whose registers start at the kernel virtual address `base`.
    ///
    /// # Safety
    ///
    /// `[base, base + 8 * stride)` must be mapped uncached, read/write and belong to the
    /// UART `spcr::parse` accepted: it is written and read as one, and nothing else uses it.
    pub(crate) const unsafe fn mmio(base: u64, stride: u8, access: Access) -> Self {
        Self::Mmio {
            base,
            stride,
            access,
        }
    }

    /// `(word, shape)`, for [`UNOWNED`].
    fn pack(self) -> (u64, u64) {
        match self {
            Self::Port(port) => (u64::from(port), 0),
            Self::Mmio {
                base,
                stride,
                access,
            } => (
                base,
                u64::from(stride) << 8
                    | match access {
                        Access::Byte => 1,
                        Access::Word => 2,
                        Access::Dword => 3,
                    },
            ),
        }
    }

    fn unpack(word: u64, shape: u64) -> Self {
        let access = match shape & 0xFF {
            0 => return Self::Port(word as u16),
            1 => Access::Byte,
            2 => Access::Word,
            _ => Access::Dword,
        };
        Self::Mmio {
            base: word,
            stride: (shape >> 8) as u8,
            access,
        }
    }

    /// The address of register `reg` in the MMIO window.
    fn register(base: u64, stride: u8, reg: Register) -> *mut u8 {
        ptr::with_exposed_provenance_mut(
            (base + u64::from(reg.offset()) * u64::from(stride)) as usize,
        )
    }
}

impl UartRegisters for KernelUart {
    fn read(&mut self, reg: Register) -> u8 {
        match *self {
            // SAFETY: `port`'s contract: CPL 0 and these are this UART's ports, which cannot
            // affect memory.
            Self::Port(base) => unsafe { crate::inb(base + reg.offset()) },
            Self::Mmio {
                base,
                stride,
                access,
            } => {
                let at = Self::register(base, stride, reg);
                match access {
                    // SAFETY: `mmio`'s contract: this is a mapped register of this UART.
                    Access::Byte => unsafe { at.read_volatile() },
                    // SAFETY: as above; `spcr::parse` checked the alignment for this access.
                    Access::Word => unsafe { at.cast::<u16>().read_volatile() as u8 },
                    // SAFETY: as above.
                    Access::Dword => unsafe { at.cast::<u32>().read_volatile() as u8 },
                }
            }
        }
    }

    fn write(&mut self, reg: Register, value: u8) {
        if reg == Register::Data {
            UART_BYTES.fetch_add(1, Ordering::Relaxed);
        }
        match *self {
            // SAFETY: as for `read`.
            Self::Port(base) => unsafe { crate::outb(base + reg.offset(), value) },
            Self::Mmio {
                base,
                stride,
                access,
            } => {
                let at = Self::register(base, stride, reg);
                match access {
                    // SAFETY: `mmio`'s contract: this is a mapped register of this UART.
                    Access::Byte => unsafe { at.write_volatile(value) },
                    // SAFETY: as above; `spcr::parse` checked the alignment for this access.
                    Access::Word => unsafe { at.cast::<u16>().write_volatile(u16::from(value)) },
                    // SAFETY: as above.
                    Access::Dword => unsafe { at.cast::<u32>().write_volatile(u32::from(value)) },
                }
            }
        }
    }
}

/// The framebuffer as a pixel sink: 32-bit volatile writes at its mapped address.
#[derive(Clone, Copy, Debug)]
struct FramebufferMemory {
    first_pixel: u64,
    bytes: usize,
}

impl PixelSink for FramebufferMemory {
    fn write32(&mut self, offset: usize, value: u32) {
        let Some(end) = offset.checked_add(4) else {
            return;
        };
        if end > self.bytes || !offset.is_multiple_of(4) {
            return;
        }
        let at = ptr::with_exposed_provenance_mut::<u32>(self.first_pixel as usize + offset);
        // SAFETY: `attach_framebuffer` checked the framebuffer's description and that its
        // first page is mapped uncached and writable at `first_pixel`; `layout::build` maps
        // the visible rows, so every byte below `self.bytes` is mapped, and this write is
        // inside them, 4-byte aligned (both checked above). Video memory has no Rust object
        // on it and no side effects a pixel write could harm.
        unsafe { at.write_volatile(value) };
    }
}

/// [`FramebufferMemory`], counting what really gets written, so that the proof of
/// [`SECTION_PIXELS`] does not take the budget's word for it.
struct Counted {
    memory: FramebufferMemory,
    written: u64,
}

impl PixelSink for Counted {
    fn write32(&mut self, offset: usize, value: u32) {
        self.written += 1;
        self.memory.write32(offset, value);
    }
}

/// The screen, set at most once.
struct ScreenCell {
    ready: AtomicBool,
    value: UnsafeCell<MaybeUninit<(Screen, FramebufferMemory)>>,
}

// SAFETY: `value` is written only by `set`, which runs once on the boot processor before any
// other processor is started, before `ready` is released. It is read only after `ready` is
// acquired and never written again, and the value is `Copy` data, so readers never race a
// write. `withdraw` only clears `ready`.
unsafe impl Sync for ScreenCell {}

impl ScreenCell {
    const fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// Stores the screen. Runs once, on the boot processor, before the application processors
    /// exist.
    fn set(&self, screen: Screen, memory: FramebufferMemory) {
        // SAFETY: nothing reads `value` before `ready` is set below, and `set` runs once.
        unsafe { (*self.value.get()).write((screen, memory)) };
        self.ready.store(true, Ordering::Release);
    }

    /// The screen, unless it is unset or was withdrawn.
    fn get(&self) -> Option<(Screen, FramebufferMemory)> {
        if !self.ready.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: `ready` was set after `value` was written, and `value` is never written
        // again (see the `Sync` implementation), so this shared reference aliases nothing
        // that is being mutated.
        let value = unsafe { &*self.value.get() };
        // SAFETY: as above, `value` was fully written by `set` before `ready` was set.
        Some(unsafe { value.assume_init() })
    }

    fn withdraw(&self) {
        self.ready.store(false, Ordering::Release);
    }
}

/// The framebuffer console, once [`attach_framebuffer`] has run.
static SCREEN: ScreenCell = ScreenCell::new();
/// Where the next glyph goes, kept outside every lock for urgent reports (see the module
/// documentation).
static CURSOR: Cursor = Cursor::new();

/// The UART an urgent report writes to when it cannot take the console lock, as
/// `(word, shape)` (see [`KernelUart::pack`]): COM1 until [`adopt_firmware_uart`] moves it.
/// Written only while the boot processor runs alone.
static UNOWNED: [AtomicU64; 2] = [AtomicU64::new(COM1_BASE as u64), AtomicU64::new(0)];

/// Writes to the UART's data register: counted at the sink, so that the proof of
/// [`TX_FIFO_BYTES`] does not take the queue's word for it.
static UART_BYTES: AtomicU64 = AtomicU64::new(0);

/// Bytes of output kept for the UART that [`adopt_firmware_uart`] moves to.
const EARLY_BYTES: usize = 8 * 1024;
/// Bytes of text that can be waiting for the UART: room for 25 lines of the longest kind, and
/// 355 ms of them at 115 200 baud, which is how long an urgent report can wait behind them on
/// a UART that is working. (A UART that keeps taking a load just before it would have been
/// given up on can stretch that, load by load; the wait stays bounded, in polls.) Writers that
/// can wait stay behind their own line and, if this is ever full, wait for room, so only
/// writers that cannot wait can ever lose a line, and the UART is told when one did.
const UART_QUEUE_BYTES: usize = 4 * 1024;
const _: () = assert!(
    UART_QUEUE_BYTES >= 2 * LINE,
    "a line must always fit, with any notice in front of it"
);
/// Bytes of text the screen can be behind the UART by: about 80 lines of ordinary length. A
/// writer that can wait stays behind its own line and, if this is ever full, waits for room,
/// so only writers that cannot wait, and only on the screen, can ever lose a line.
const BACKLOG_BYTES: usize = 8 * 1024;
/// The most pixels the console writes in one stretch with interrupts off: 256 pixels, 1 KiB of
/// uncached memory. See the module documentation for what that costs.
const SECTION_PIXELS: u32 = 256;

/// What a line's writer can do while its line waits for the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    /// Interrupts are on: they come back on after every section.
    InterruptsOn,
    /// The boot flow ([`Console::boot_flow`]): interrupts are off until `windows`, and after
    /// that a window is opened between sections.
    Boot { windows: bool },
    /// Interrupts are off for a reason this code cannot see, and must stay off: an interrupt
    /// handler. Such a writer hands its line over, sends one section of the UART's queue and
    /// leaves.
    NoWaiting,
}

/// Which of a line's sections has just been done, for [`prove_sections`] and
/// [`prove_uart_sections`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sink {
    Uart,
    Screen,
}

/// What became of a line handed to the console.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sent {
    /// Accepted: the positions at which it will have reached each sink, `None` for a sink it
    /// is not going to (that sink is gone, or had no room and the writer cannot wait).
    Queued {
        uart: Option<u64>,
        screen: Option<u64>,
    },
    /// Nowhere yet: a queue is full and this writer can wait for room.
    NoRoom,
}

/// What the screen is told when the UART goes.
const UART_STOPPED: &[u8] =
    b"origin-kernel: console: the UART stopped taking bytes; serial output off\n";

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
    /// What the UART has not taken yet.
    tx: TxQueue<UART_QUEUE_BYTES>,
    /// What the screen has not shown yet.
    backlog: Backlog<BACKLOG_BYTES>,
    /// Where the screen has shown that the UART stopped, once it has got that far: no writer
    /// goes on before it has, so the machine's owner is told even if nothing is printed again.
    /// 0 until the UART stops.
    stop_notice: u64,
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
            tx: TxQueue::new(),
            backlog: Backlog::new(),
            stop_notice: 0,
        }
    }

    /// Hands `bytes` over: into the queue for the UART and the backlog for the screen,
    /// together, so the two sinks get lines in one order, and no device is touched. A line
    /// goes to both or, if a writer that can wait finds no room in one, to neither: `may_wait`
    /// says whether the writer waits for room or the line is lost on that sink alone.
    fn send(&mut self, bytes: &[u8], may_wait: bool) -> Sent {
        let has_screen = SCREEN.get().is_some();
        if may_wait
            && ((self.uart_alive && !self.tx.has_room(bytes.len()))
                || (has_screen && !self.backlog.has_room(bytes.len())))
        {
            return Sent::NoRoom;
        }
        let uart = if self.uart_alive {
            let position = self.tx.push(bytes);
            if position.is_none() {
                self.tx.note_lost();
            }
            position
        } else {
            None
        };
        let screen = if has_screen {
            let position = self.backlog.push(bytes);
            if position.is_none() {
                self.backlog.note_lost();
            }
            position
        } else {
            None
        };
        if self.early_open {
            self.early.push(bytes);
        }
        if screen.is_some() {
            note_line();
        }
        Sent::Queued { uart, screen }
    }

    /// One section of the sink that has no room for `needed` more bytes.
    fn make_room(&mut self, needed: usize) {
        if self.uart_alive && !self.tx.has_room(needed) {
            UART_NO_ROOM.fetch_add(1, Ordering::Relaxed);
            self.pump_uart();
        } else {
            self.paint_section();
        }
    }

    /// Sends one section of the queue, the most the console writes to the UART in one stretch
    /// with interrupts off: at most [`TX_FIFO_BYTES`] bytes, with no waiting in it. Returns
    /// whether more is waiting. See the module documentation.
    fn pump_uart(&mut self) -> bool {
        if !self.uart_alive {
            return false;
        }
        let (start, written) = (crate::cpu::rdtsc(), UART_BYTES.load(Ordering::Relaxed));
        let pumped = self.tx.pump(&mut self.uart);
        let cycles = crate::cpu::rdtsc().wrapping_sub(start);
        match pumped {
            Pumped::Stopped { polls } => {
                UART_GIVEN_UP_AFTER.fetch_add(u64::from(polls), Ordering::Relaxed);
                self.give_up_on_uart();
                return false;
            }
            Pumped::Idle => {}
            Pumped::Busy | Pumped::Sent(_) => note_uart_section(
                cycles,
                UART_BYTES.load(Ordering::Relaxed).wrapping_sub(written),
            ),
        }
        !self.tx.is_empty()
    }

    /// The UART stopped taking bytes: nothing more goes to it, and the screen says so.
    fn give_up_on_uart(&mut self) {
        self.uart_alive = false;
        self.tx.clear();
        if SCREEN.get().is_some() {
            match self.backlog.push(UART_STOPPED) {
                Some(end) => self.stop_notice = end,
                None => self.backlog.note_lost(),
            }
        }
    }

    /// Sends everything queued, and waits for that here: before the console moves to another
    /// UART, when nothing else is running. Bounded: the queue's size, and the one wait a UART
    /// that stops taking bytes is given.
    fn drain_uart(&mut self) {
        while self.pump_uart() {
            core::hint::spin_loop();
        }
    }

    /// Paints one section of the backlog: at most [`SECTION_PIXELS`] pixels, the most the
    /// console writes in one stretch with interrupts off. Returns whether more is waiting.
    fn paint_section(&mut self) -> bool {
        let Some((screen, memory)) = SCREEN.get() else {
            self.backlog.clear();
            return false;
        };
        let mut counted = Counted { memory, written: 0 };
        let start = crate::cpu::rdtsc();
        let more = self
            .backlog
            .paint(&screen, &CURSOR, &mut counted, SECTION_PIXELS);
        note_section(crate::cpu::rdtsc().wrapping_sub(start), counted.written);
        more
    }
}

/// The programmed UART and the rest of the console, once [`init`] has run.
static SINKS: SpinLock<Option<Sinks>> = SpinLock::new(None);

/// Bytes a [`Console`] holds before it sends them.
const LINE: usize = 160;
/// Lock attempts an urgent report makes before it writes without the lock: on the order of
/// 100 ms, far longer than a section, so only a holder that is stuck makes a report go without
/// it.
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
            tx: TxQueue::new(),
            backlog: Backlog::new(),
            stop_notice: 0,
        });
    });
}

/// Sends `bytes` at once, turning each LF into CR LF and waiting for each byte in turn: only
/// where nothing can wait behind it (the replay of the early log) or nothing else is left (a
/// report that could not get the lock). Stops at the first byte the UART will not take, which
/// costs one bounded wait.
fn emit(uart: &mut Uart16550<KernelUart>, bytes: &[u8]) -> fmt::Result {
    uart.write_bytes(bytes)
}

/// Lets the boot flow open interrupt windows between sections (see [`Console::boot_flow`]).
static WINDOWS_ALLOWED: AtomicBool = AtomicBool::new(false);

/// From here on, interrupts can be taken: the boot flow may open windows, and the cost of
/// drawing is counted for the report in [`prove`] (before this nothing competes for the
/// processor, and no timer runs for interrupts to be held off). Called once, by the boot
/// processor, when the timer is running and every vector that can arrive has a gate.
pub(crate) fn allow_interrupt_windows() {
    WINDOWS_ALLOWED.store(true, Ordering::Release);
}

/// Lines queued for the screen since [`allow_interrupt_windows`], and the sections that drew
/// them, for the report in [`prove`] (CLAUDE.md rule 21).
static LINES_QUEUED: AtomicU64 = AtomicU64::new(0);
static SECTIONS: AtomicU64 = AtomicU64::new(0);
static SECTION_CYCLES: AtomicU64 = AtomicU64::new(0);
static SECTION_WORST_CYCLES: AtomicU64 = AtomicU64::new(0);
/// The most pixels one section wrote: counted at the sink, not taken from the budget.
static SECTION_WORST_PIXELS: AtomicU64 = AtomicU64::new(0);

/// UART sections since [`allow_interrupt_windows`] that wrote bytes, and the time they kept
/// interrupts off, for the report in [`prove_uart_sections`] (CLAUDE.md rule 21).
static UART_SECTIONS: AtomicU64 = AtomicU64::new(0);
static UART_SECTION_CYCLES: AtomicU64 = AtomicU64::new(0);
static UART_SECTION_WORST_CYCLES: AtomicU64 = AtomicU64::new(0);
/// The most bytes one section wrote: counted at the sink, not taken from the queue.
static UART_SECTION_WORST_BYTES: AtomicU64 = AtomicU64::new(0);
/// Sections that found the transmitter busy and wrote nothing.
static UART_BUSY_POLLS: AtomicU64 = AtomicU64::new(0);
/// The polls spent giving up on the UART, added up: one bounded wait's worth at most, and 0 if
/// it was never given up on.
static UART_GIVEN_UP_AFTER: AtomicU64 = AtomicU64::new(0);
/// Times a writer that can wait found the UART's queue full, for [`prove_room`].
static UART_NO_ROOM: AtomicU64 = AtomicU64::new(0);
/// Whether the writer that found the UART had stopped went on only with that on the screen.
static STOP_SHOWN_AT_ONCE: AtomicBool = AtomicBool::new(false);

fn note_line() {
    if WINDOWS_ALLOWED.load(Ordering::Relaxed) {
        LINES_QUEUED.fetch_add(1, Ordering::Relaxed);
    }
}

fn note_uart_section(cycles: u64, bytes: u64) {
    if !WINDOWS_ALLOWED.load(Ordering::Relaxed) {
        return;
    }
    if bytes == 0 {
        UART_BUSY_POLLS.fetch_add(1, Ordering::Relaxed);
        return;
    }
    UART_SECTIONS.fetch_add(1, Ordering::Relaxed);
    UART_SECTION_CYCLES.fetch_add(cycles, Ordering::Relaxed);
    UART_SECTION_WORST_CYCLES.fetch_max(cycles, Ordering::Relaxed);
    UART_SECTION_WORST_BYTES.fetch_max(bytes, Ordering::Relaxed);
}

fn note_section(cycles: u64, pixels: u64) {
    if WINDOWS_ALLOWED.load(Ordering::Relaxed) {
        SECTIONS.fetch_add(1, Ordering::Relaxed);
        SECTION_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        SECTION_WORST_CYCLES.fetch_max(cycles, Ordering::Relaxed);
        SECTION_WORST_PIXELS.fetch_max(pixels, Ordering::Relaxed);
    }
}

/// Draws `bytes` on the framebuffer at once, if there is one, without the lock: only for an
/// urgent report that could not get it.
fn draw(bytes: &[u8]) {
    if let Some((screen, mut memory)) = SCREEN.get() {
        screen.write(&CURSOR, &mut memory, bytes);
    }
}

/// Runs `f` on the console's state, under the lock and with interrupts off. A writer that can
/// wait waits with interrupts as they were: the lock is tried, and retried after a [`pause`].
/// One that cannot only spins: the lock is held for at most a section.
fn with_sinks<T>(flow: Flow, f: impl FnOnce(&mut Sinks) -> T) -> T {
    let mut f = Some(f);
    loop {
        let result = crate::cpu::without_interrupts(|| {
            let mut guard = match flow {
                Flow::NoWaiting => Some(SINKS.lock()),
                Flow::InterruptsOn | Flow::Boot { .. } => SINKS.try_lock(),
            }?;
            let f = f.take()?;
            // A report before `init` (a very early panic) programs the UART first.
            Some(f(guard.get_or_insert_with(Sinks::default_console)))
        });
        if let Some(value) = result {
            return value;
        }
        pause(flow);
    }
}

/// Between two tries at the lock, or two sections: takes whatever interrupts were waiting. A
/// writer with interrupts on has already had them turned back on; the boot flow opens a
/// window, if the time for that has come.
fn pause(flow: Flow) {
    if flow == (Flow::Boot { windows: true }) {
        crate::cpu::interrupt_window();
    } else {
        core::hint::spin_loop();
    }
}

/// What a [`Console`] does with a line that is not urgent: hands it over and, if the writer
/// can wait, sends it and then draws it, a section at a time. See the module documentation.
/// `between` runs after each section but the last, with interrupts as the caller has them:
/// for [`prove`].
fn send_and_draw(bytes: &[u8], flow: Flow, between: &mut dyn FnMut(Sink, u32)) {
    let (uart, screen) = loop {
        let sent = with_sinks(flow, |sinks| {
            let sent = sinks.send(bytes, flow != Flow::NoWaiting);
            match sent {
                Sent::NoRoom => sinks.make_room(bytes.len()),
                // What it can do for its line without waiting: it has interrupts off anyway.
                Sent::Queued { .. } if flow == Flow::NoWaiting => {
                    sinks.pump_uart();
                }
                Sent::Queued { .. } => {}
            }
            sent
        });
        match sent {
            Sent::Queued { uart, screen } => break (uart, screen),
            Sent::NoRoom => pause(flow),
        }
    };
    if flow == Flow::NoWaiting {
        return;
    }
    let polls_given = UART_GIVEN_UP_AFTER.load(Ordering::Relaxed);
    if let Some(end) = uart {
        let mut section = 0;
        while with_sinks(flow, |sinks| sinks.pump_uart() && sinks.tx.sent() < end) {
            pause(flow);
            section += 1;
            between(Sink::Uart, section);
        }
    }
    if let Some(end) = screen {
        let mut section = 0;
        while with_sinks(flow, |sinks| {
            sinks.paint_section() && sinks.backlog.painted() < end.max(sinks.stop_notice)
        }) {
            pause(flow);
            section += 1;
            between(Sink::Screen, section);
        }
        // For the report in [`prove_uart_sections`]: this writer found that the UART had
        // stopped, and it has drawn everything up to the line that says so.
        if UART_GIVEN_UP_AFTER.load(Ordering::Relaxed) != polls_given {
            let shown = with_sinks(flow, |sinks| sinks.backlog.painted() >= sinks.stop_notice);
            STOP_SHOWN_AT_ONCE.store(shown, Ordering::Relaxed);
        }
    }
}

/// Runs `f` on the console's state for a report: with the lock, which it waits for only
/// [`URGENT_ATTEMPTS`] times, and `None` if it could not be got.
fn urgent_section<T>(f: impl FnOnce(&mut Sinks) -> T) -> Option<T> {
    let mut guard = (0..URGENT_ATTEMPTS).find_map(|_| {
        let guard = SINKS.try_lock();
        if guard.is_none() {
            core::hint::spin_loop();
        }
        guard
    })?;
    // A report before `init` (a very early panic) programs the UART first.
    Some(f(guard.get_or_insert_with(Sinks::default_console)))
}

/// A report through the console's own queues: handed over, sent, then drawn, each a section
/// at a time, with the lock let go between them. `None` if the lock could not be got.
fn send_report(bytes: &[u8]) -> Option<()> {
    let (uart, screen) = loop {
        let sent = urgent_section(|sinks| {
            let sent = sinks.send(bytes, true);
            if sent == Sent::NoRoom {
                sinks.make_room(bytes.len());
            }
            sent
        })?;
        if let Sent::Queued { uart, screen } = sent {
            break (uart, screen);
        }
    };
    if let Some(end) = uart {
        while urgent_section(|sinks| sinks.pump_uart() && sinks.tx.sent() < end)? {
            core::hint::spin_loop();
        }
    }
    if let Some(end) = screen {
        while urgent_section(|sinks| {
            sinks.paint_section() && sinks.backlog.painted() < end.max(sinks.stop_notice)
        })? {}
    }
    Some(())
}

/// An exception or panic report: on the UART first and then, whatever it takes, on the screen,
/// after everything queued before it. See the module documentation.
fn send_urgent(bytes: &[u8]) {
    crate::cpu::without_interrupts(|| {
        if send_report(bytes).is_none() {
            // The holder is stuck (this processor faulted inside a section, or another died
            // holding the lock): send without it.
            let _ = emit(&mut unowned_uart(), bytes);
            draw(bytes);
        }
    });
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
    let memory = FramebufferMemory {
        first_pixel: framebuffer.virtual_base,
        bytes: framebuffer.visible_bytes() as usize,
    };
    Ok((screen, memory))
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
            SCREEN.set(screen, memory);
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
    SCREEN.withdraw();
}

/// Whether the console draws on the framebuffer.
pub(crate) fn has_screen() -> bool {
    SCREEN.get().is_some()
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
        // SAFETY: CPL 0; `spcr::parse` kept the system's own ports out of the UART's block.
        UartAddress::Port(port) => unsafe { KernelUart::port(port) },
        UartAddress::Mmio {
            base,
            stride,
            access,
        } => {
            if overlaps_ram(regions, base, spcr.address.span()) {
                return Err("its registers are in RAM");
            }
            let virt = crate::vm::map_mmio(base, spcr.address.span())
                .map_err(|_| "its registers cannot be mapped")?;
            // SAFETY: `map_mmio` mapped these registers uncached and read/write, and the
            // memory map just said they are not RAM.
            unsafe { KernelUart::mmio(virt, stride, access) }
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
    // Nothing should be queued (the boot flow waits for its own lines and nothing else runs
    // yet), but a byte accepted for one UART must not go to another.
    sinks.drain_uart();
    sinks.tx.clear();
    let com1_alive = sinks.uart_alive;
    let same_port = matches!(uart, KernelUart::Port(COM1_BASE));
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
    let (word, shape) = uart.pack();
    if !same_port {
        sinks.uart = new;
        UNOWNED[0].store(word, Ordering::Release);
        UNOWNED[1].store(shape, Ordering::Release);
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
                UNOWNED[0].store(u64::from(COM1_BASE), Ordering::Release);
                UNOWNED[1].store(0, Ordering::Release);
                sinks.uart_alive = com1_alive;
                return (setup, 0, dropped, true);
            }
            Err(_) => sinks.give_up_on_uart(),
        }
    }
    (setup, replayed, dropped, false)
}

/// One line at a time to the console's sinks. See the module documentation.
pub(crate) struct Console {
    line: [u8; LINE],
    len: usize,
    urgent: bool,
    boot_flow: bool,
}

impl Console {
    pub(crate) const fn new() -> Self {
        Self {
            line: [0; LINE],
            len: 0,
            urgent: false,
            boot_flow: false,
        }
    }

    /// For exception and panic reports: never waits on the lock for long.
    pub(crate) const fn urgent() -> Self {
        Self {
            line: [0; LINE],
            len: 0,
            urgent: true,
            boot_flow: false,
        }
    }

    /// For the boot flow: the one caller that runs with interrupts off without needing them
    /// off. Between the sections of a line it may open an interrupt window, once
    /// [`allow_interrupt_windows`] has run, so it holds no lock and must be ready to be
    /// interrupted and preempted at every line it prints, as it is at every
    /// `sched::yield_until`.
    pub(crate) const fn boot_flow() -> Self {
        Self {
            line: [0; LINE],
            len: 0,
            urgent: false,
            boot_flow: true,
        }
    }

    /// What this writer can do while it waits.
    fn flow(&self) -> Flow {
        if crate::cpu::interrupts_enabled() {
            Flow::InterruptsOn
        } else if self.boot_flow {
            Flow::Boot {
                windows: WINDOWS_ALLOWED.load(Ordering::Acquire),
            }
        } else {
            Flow::NoWaiting
        }
    }

    /// Sends the buffered bytes, if any.
    fn flush(&mut self) {
        self.flush_with(&mut |_, _| {});
    }

    /// As [`flush`](Self::flush), running `between` after each section but the last of this
    /// line, for [`prove`].
    fn flush_with(&mut self, between: &mut dyn FnMut(Sink, u32)) {
        let Some(bytes) = self.line.get(..self.len) else {
            return;
        };
        if bytes.is_empty() {
            return;
        }
        if self.urgent {
            send_urgent(bytes);
        } else {
            send_and_draw(bytes, self.flow(), between);
        }
        self.len = 0;
    }

    /// Ends the line, running `between` as for [`flush_with`](Self::flush_with).
    fn end_line_with(&mut self, between: &mut dyn FnMut(Sink, u32)) {
        if let Some(slot) = self.line.get_mut(self.len) {
            *slot = b'\n';
            self.len += 1;
        }
        self.flush_with(between);
    }
}

/// COM1, as the console left it: for the move that did not work.
fn unowned_com1() -> Uart16550<KernelUart> {
    // SAFETY: CPL 0; COM1 is the console's own UART, and only `move_uart`, under the console
    // lock, comes back to it.
    let mut com1 = Uart16550::new(unsafe { KernelUart::port(COM1_BASE) });
    com1.detect_fifo();
    com1
}

/// The console's UART outside the console's own state: for urgent reports that cannot get
/// the lock, and to program the UART for a report made before [`init`].
fn unowned_uart() -> Uart16550<KernelUart> {
    let (word, shape) = (
        UNOWNED[0].load(Ordering::Acquire),
        UNOWNED[1].load(Ordering::Acquire),
    );
    // SAFETY: CPL 0. Used only under the console lock before `init`, or by an urgent
    // report that could not get the lock; the worst a concurrent writer can then cause is
    // mixed bytes on the line, never memory unsafety, since `UNOWNED` only ever holds this
    // UART's ports or its uncached register mapping.
    Uart16550::new(KernelUart::unpack(word, shape))
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

/// Lines each processor prints in the proof.
const PROOF_LINES: u64 = 20;
/// Letters after each proof line's prefix: one letter per processor, repeated, so a line
/// that mixed two processors' bytes shows it.
const PROOF_PAYLOAD: usize = 60;
/// Filler after the prefix of a line the proof draws in sections: long enough that such a
/// line takes many sections.
const PROOF_FILL: usize = 60;
/// The section of its line after which the proof's breakpoint goes off.
const BREAK_AT: u32 = 3;

static PRINTERS_READY: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static PRINTERS_DONE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static PRINT_GO: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// Asks the proof's interrupt to print: see [`on_proof_interrupt`].
static PRINT_IN_INTERRUPT: AtomicBool = AtomicBool::new(false);
/// The thread of [`prove_thread_line`]: how many sections of the UART's and of the screen's it
/// counted, whether interrupts were on between all of them, and that it finished.
static THREAD_UART_SECTIONS: AtomicU64 = AtomicU64::new(0);
static THREAD_SECTIONS: AtomicU64 = AtomicU64::new(0);
static THREAD_INTERRUPTS_ON: AtomicBool = AtomicBool::new(false);
static THREAD_DONE: AtomicBool = AtomicBool::new(false);

/// Prints [`PROOF_LINES`] lines as processor `id`.
fn print_lines(out: &mut Console, id: u64) {
    use core::fmt::Write as _;
    let letter = char::from(b'a' + (id % 26) as u8);
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
    print_lines(&mut Console::new(), crate::cpu::this_cpu().get() as u64);
    PRINTERS_DONE.fetch_add(1, Ordering::AcqRel);
    crate::sched::exit()
}

/// Called by the handler of every interrupt on `cpu::IOAPIC_VECTOR`: if the proof has asked
/// for it, prints one line through an ordinary [`Console`] from a real interrupt handler.
pub(crate) fn on_proof_interrupt() {
    use core::fmt::Write as _;
    if PRINT_IN_INTERRUPT.swap(false, Ordering::AcqRel) {
        let _ = writeln!(
            Console::new(),
            "origin-kernel: console: this line comes from an interrupt handler, in the middle of the line before it"
        );
    }
}

/// A line with [`PROOF_FILL`] characters after `prefix`, with `between` run after each of its
/// sections but the last.
fn print_long(out: &mut Console, prefix: &str, between: &mut dyn FnMut(Sink, u32)) {
    use core::fmt::Write as _;
    let _ = out.write_str(prefix);
    for _ in 0..PROOF_FILL {
        let _ = out.write_char('.');
    }
    out.end_line_with(between);
}

/// A thread printing a line: threads run with interrupts on, so they must be on between the
/// line's sections, on both sinks.
extern "sysv64" fn section_printer(_: u64) -> ! {
    let (mut uart_sections, mut sections, mut interrupts_on) = (0, 0, true);
    print_long(
        &mut Console::new(),
        "origin-kernel: console: a line drawn by a thread: ",
        &mut |sink, section| {
            match sink {
                Sink::Uart => uart_sections = section,
                Sink::Screen => sections = section,
            }
            interrupts_on &= crate::cpu::interrupts_enabled();
        },
    );
    THREAD_UART_SECTIONS.store(u64::from(uart_sections), Ordering::Relaxed);
    THREAD_SECTIONS.store(u64::from(sections), Ordering::Relaxed);
    THREAD_INTERRUPTS_ON.store(interrupts_on, Ordering::Relaxed);
    THREAD_DONE.store(true, Ordering::Release);
    crate::sched::exit()
}

/// A thread (one on the first processor other than the boot processor, or on the boot
/// processor if it is alone) prints a line; interrupts must be on between its sections.
fn prove_thread_line(
    com1: &mut Console,
    cpus: &[crate::cpu::CpuIndex],
    bsp: crate::cpu::CpuIndex,
    tsc_hz: u64,
) -> bool {
    use core::fmt::Write as _;
    let far = cpus.iter().copied().find(|&id| id != bsp).unwrap_or(bsp);
    if crate::sched::spawn(far, section_printer, 0).is_ok() {
        crate::sched::yield_until(|| THREAD_DONE.load(Ordering::Acquire), tsc_hz, 5);
    }
    let uart_sections = THREAD_UART_SECTIONS.load(Ordering::Relaxed);
    let sections = THREAD_SECTIONS.load(Ordering::Relaxed);
    // A sink that is not there has no sections to count.
    let ok = THREAD_DONE.load(Ordering::Acquire)
        && (uart_sections > 1 || !uart_is_on())
        && (sections > 1 || !has_screen())
        && THREAD_INTERRUPTS_ON.load(Ordering::Relaxed);
    let _ = writeln!(
        com1,
        "origin-kernel: console: a thread's line took {uart_sections} sections on the UART and {sections} on the screen with interrupts on between them{}",
        if ok { "" } else { ": console failed" }
    );
    ok
}

/// Fills the UART's queue with lines from a writer that cannot wait, each of which sends one
/// section and leaves. Then a writer that can wait prints a line that does not fit: it must wait
/// for room, without its line being lost or sent twice, and go on only when everything queued
/// before it and the line itself are on the UART. The boot test reads every one of those lines
/// back from the UART, in order and whole.
///
/// Called from the boot flow, with interrupts off, so a plain [`Console`] really cannot wait.
fn prove_room(com1: &mut Console) -> bool {
    use core::fmt::Write as _;
    const ROOM: &str = "origin-kernel: console: a line that had to wait for room: ";
    /// More than the queue can hold, however many sections are sent meanwhile.
    const MOST_LINES: u32 = 100;
    if !uart_is_on() {
        return true;
    }
    let flow = com1.flow();
    let needed = ROOM.len() + PROOF_FILL + 1;
    let mut lines = 0;
    while lines < MOST_LINES && with_sinks(flow, |sinks| sinks.tx.has_room(needed)) {
        let mut out = Console::new();
        let _ = write!(
            out,
            "origin-kernel: console: a line that cannot wait: {lines}: "
        );
        for _ in 0..PROOF_FILL {
            let _ = out.write_char('.');
        }
        let _ = out.write_char('\n');
        lines += 1;
    }
    let no_room = UART_NO_ROOM.load(Ordering::Relaxed);
    let before = UART_BYTES.load(Ordering::Relaxed);
    print_long(com1, ROOM, &mut |_, _| {});
    let waited = UART_NO_ROOM.load(Ordering::Relaxed) > no_room;
    let written = UART_BYTES.load(Ordering::Relaxed).wrapping_sub(before);
    let emptied = with_sinks(com1.flow(), |sinks| sinks.tx.is_empty());
    // CR LF at the end of it, on top of what was in front of it.
    let ok = lines > 1 && waited && emptied && written > (needed + 1) as u64;
    let _ = writeln!(
        com1,
        "origin-kernel: console: {lines} lines from a writer that cannot wait filled the UART's queue; the next writer waited for room: {waited}, and when it went on the queue was empty: {emptied}{}",
        if ok { "" } else { ": console failed" }
    );
    ok
}

/// Whether the console still sends to its UART.
fn uart_is_on() -> bool {
    crate::cpu::without_interrupts(|| SINKS.lock().as_ref().is_some_and(|sinks| sinks.uart_alive))
}

/// Proves what [`TX_FIFO_BYTES`] promises, from the boot flow, just before [`prove_sections`]
/// and for the same reason: what it prints is still on the screen for the boot test to compare
/// with the serial log.
///
/// A line must be on the UART when its writer goes on, counted at the UART's data register. An
/// interrupt made pending while the boot flow has interrupts off can only come in through a
/// window the sending itself opens, so it must be taken while the line is still going out. Its
/// handler prints a line, which must come after that line on the wire, whole. A breakpoint in
/// the middle of the next line tests the urgent path the same way. The sections' sizes are
/// counted at the sink. QEMU's UART takes bytes instantly, so what this proves is the bound in
/// bytes, the windows and the order: what a section costs on a real UART it cannot show.
pub(crate) fn prove_uart_sections(com1: &mut Console) {
    use core::fmt::Write as _;
    if !WINDOWS_ALLOWED.load(Ordering::Acquire) {
        return;
    }
    if !uart_is_on() {
        let given_up = UART_GIVEN_UP_AFTER.load(Ordering::Relaxed);
        if given_up != 0 {
            let _ = writeln!(
                com1,
                "origin-kernel: console: the UART was given up on after {given_up} polls; nothing more is sent to it, and the screen said so at once: {}",
                STOP_SHOWN_AT_ONCE.load(Ordering::Relaxed)
            );
        }
        return;
    }
    const PLAIN: &str = "origin-kernel: console: a line sent on the UART a section at a time: ";
    let before = UART_BYTES.load(Ordering::Relaxed);
    print_long(com1, PLAIN, &mut |_, _| {});
    let written = UART_BYTES.load(Ordering::Relaxed).wrapping_sub(before);
    // CR LF at the end of it.
    let whole_line = (PLAIN.len() + PROOF_FILL + 2) as u64;
    let on_the_uart = written >= whole_line;

    let bsp = crate::cpu::this_cpu();
    let irqs = crate::cpu::ioapic_irqs_on(bsp);
    PRINT_IN_INTERRUPT.store(true, Ordering::Release);
    crate::interrupt_self(crate::cpu::IOAPIC_VECTOR);
    let (mut taken_after, mut sections) = (None, 0);
    print_long(
        com1,
        "origin-kernel: console: a line sent in sections while an interrupt waits: ",
        &mut |sink, section| {
            if sink == Sink::Uart {
                sections = section;
                if taken_after.is_none() && crate::cpu::ioapic_irqs_on(bsp) > irqs {
                    taken_after = Some(section);
                }
            }
        },
    );
    let handler_waited = with_sinks(com1.flow(), |sinks| !sinks.tx.is_empty());

    let flow = com1.flow();
    let mut report_out = false;
    print_long(
        com1,
        "origin-kernel: console: a line sent in sections with a breakpoint in the middle: ",
        &mut |sink, section| {
            if sink == Sink::Uart && section == BREAK_AT {
                crate::cpu::breakpoint();
                report_out = with_sinks(flow, |sinks| sinks.tx.is_empty());
            }
        },
    );

    let taken = taken_after.is_some_and(|after| after < sections);
    let worst_bytes = UART_SECTION_WORST_BYTES.load(Ordering::Relaxed);
    let bounded = (1..=u64::from(TX_FIFO_BYTES)).contains(&worst_bytes);
    let ok = on_the_uart && taken && handler_waited && report_out && bounded;
    let _ = writeln!(
        com1,
        "origin-kernel: console: a line took {} sections on the UART, none over {worst_bytes} bytes (the limit is {TX_FIFO_BYTES}); an interrupt waiting at its start was taken after section {}, and its handler's line waited its turn: {handler_waited}; a whole line ({written} of {whole_line} bytes) was on the UART before its writer went on: {on_the_uart}; a report left nothing queued behind it: {report_out}{}",
        sections + 1,
        taken_after.unwrap_or(0),
        if ok { "" } else { ": console failed" }
    );
    // What a section costs on this machine (CLAUDE.md rule 21). QEMU's emulated UART says little
    // about a real one: see the provenance record.
    let sent = UART_SECTIONS.load(Ordering::Relaxed);
    let cycles = UART_SECTION_CYCLES.load(Ordering::Relaxed);
    if let Some(per_section) = cycles.checked_div(sent) {
        let tsc_hz = u128::from(crate::time::tsc_hz().max(1));
        let micros = |cycles: u64| u128::from(cycles) * 1_000_000 / tsc_hz;
        let nanos = |cycles: u64| u128::from(cycles) * 1_000_000_000 / tsc_hz;
        let _ = writeln!(
            com1,
            "origin-kernel: console: UART: {sent} sections since the timer started, {} ns with interrupts off on average and {} us at worst, and {} polls found it busy",
            nanos(per_section),
            micros(UART_SECTION_WORST_CYCLES.load(Ordering::Relaxed)),
            UART_BUSY_POLLS.load(Ordering::Relaxed)
        );
    }
}

/// Proves what [`SECTION_PIXELS`] promises, from the boot flow, last thing before the halt:
/// that way what it prints is still on the screen when the boot test compares the screen with
/// the serial log, row by row, so the order the lines keep is checked there, too.
///
/// While the line is drawn an interrupt is made pending, with the boot flow's interrupts off
/// (after the line's UART sections, whose windows it would otherwise come in through), so only
/// a window the drawing itself opens can let it in: it must be taken while the line is still
/// being drawn. Its handler prints a line from interrupt context, which must queue behind that
/// line and leave the painting to the writer that can wait. A breakpoint in the middle of the
/// next line tests the urgent path the same way. The sections' sizes are counted at the sink.
pub(crate) fn prove_sections(com1: &mut Console) {
    use core::fmt::Write as _;
    if !has_screen() || !WINDOWS_ALLOWED.load(Ordering::Acquire) {
        return;
    }
    let bsp = crate::cpu::this_cpu();
    let irqs = crate::cpu::ioapic_irqs_on(bsp);
    let (mut taken_after, mut sections) = (None, 0);
    print_long(
        com1,
        "origin-kernel: console: a line drawn in sections while an interrupt waits: ",
        &mut |sink, section| {
            if sink != Sink::Screen {
                return;
            }
            if section == 1 {
                PRINT_IN_INTERRUPT.store(true, Ordering::Release);
                crate::interrupt_self(crate::cpu::IOAPIC_VECTOR);
            }
            sections = section;
            if taken_after.is_none() && crate::cpu::ioapic_irqs_on(bsp) > irqs {
                taken_after = Some(section);
            }
        },
    );
    let handler_left_work = with_sinks(com1.flow(), |sinks| !sinks.backlog.is_empty());

    print_long(
        com1,
        "origin-kernel: console: a line drawn in sections with a breakpoint in the middle: ",
        &mut |sink, section| {
            if sink == Sink::Screen && section == BREAK_AT {
                crate::cpu::breakpoint();
            }
        },
    );

    let taken = taken_after.is_some_and(|after| after < sections);
    let worst_pixels = SECTION_WORST_PIXELS.load(Ordering::Relaxed);
    let bounded = (1..=u64::from(SECTION_PIXELS)).contains(&worst_pixels);
    let ok = taken && handler_left_work && bounded;
    let _ = writeln!(
        com1,
        "origin-kernel: console: a line took {} sections on the screen, none over {worst_pixels} pixels (the limit is {SECTION_PIXELS}); an interrupt made pending after its first section was taken after section {}, and its handler's line waited its turn: {handler_left_work}{}",
        sections + 1,
        taken_after.unwrap_or(0),
        if ok { "" } else { ": console failed" }
    );
    // What drawing costs, as measured on this machine (CLAUDE.md rule 21): the pixel writing
    // of a line, and how long one section keeps interrupts off, on average and at worst. The
    // worst includes anything else that stopped this processor for a while (under emulation,
    // the host scheduling something else).
    let lines = LINES_QUEUED.load(Ordering::Relaxed);
    let cycles = SECTION_CYCLES.load(Ordering::Relaxed);
    if let (Some(per_line), Some(per_section)) = (
        cycles.checked_div(lines),
        cycles.checked_div(SECTIONS.load(Ordering::Relaxed)),
    ) {
        let tsc_hz = u128::from(crate::time::tsc_hz().max(1));
        let micros = |cycles: u64| u128::from(cycles) * 1_000_000 / tsc_hz;
        let nanos = |cycles: u64| u128::from(cycles) * 1_000_000_000 / tsc_hz;
        let _ = writeln!(
            com1,
            "origin-kernel: console: framebuffer: {lines} lines drawn since the timer started, {} us of drawing each on average; one section, with interrupts off, took {} ns on average and {} us at worst",
            micros(per_line),
            nanos(per_section),
            micros(SECTION_WORST_CYCLES.load(Ordering::Relaxed))
        );
    }
}

/// Every online processor prints [`PROOF_LINES`] lines at the same time; the boot test
/// checks that every one of them arrived whole. Then [`prove_thread_line`]. Runs from the
/// boot flow with every scheduler active.
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
    print_lines(com1, bsp.get() as u64);
    crate::sched::yield_until(|| PRINTERS_DONE.load(Ordering::Acquire) == aps, tsc_hz, 5);
    let printed = PRINTERS_DONE.load(Ordering::Acquire) == aps;
    let _ = writeln!(
        com1,
        "origin-kernel: console: {} processors printed {PROOF_LINES} lines each at once{}",
        cpus.len(),
        if printed { "" } else { ": console failed" }
    );
    let thread_line = prove_thread_line(com1, cpus, bsp, tsc_hz);
    let room = prove_room(com1);
    printed && thread_line && room
}
