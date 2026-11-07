//! The kernel console: COM1, shared by every processor.
//!
//! A [`Console`] collects one line and writes it to COM1 holding the console lock, with
//! interrupts disabled. So lines from different processors, or from an interrupt handler
//! on the same processor, never interleave. Lines longer than [`LINE`] bytes go out in
//! [`LINE`]-byte pieces, each one whole.
//!
//! Exception and panic reports use [`Console::urgent`], which waits for the lock only
//! about as long as another processor needs to send one line. A processor that faulted
//! while holding the lock, or that waits on a processor that died holding it, still gets
//! its report out, at the risk of mixing it into another line.
//!
//! Interrupts stay disabled while a line is sent. In QEMU that is microseconds; at 115 200
//! baud on real hardware it is about 9 ms for 100 bytes. Production logging will go to a
//! memory ring drained by a thread; this serial console is for development (CLAUDE.md §69).

use core::fmt;

use framekernel::serial::Uart16550;
use origin_sync::SpinLock;

use crate::Com1;

/// The programmed UART, once [`init`] has run.
static UART: SpinLock<Option<Uart16550<Com1>>> = SpinLock::new(None);

/// Bytes a [`Console`] holds before it sends them.
const LINE: usize = 160;
/// Lock attempts an urgent report makes before it writes without the lock: on the order of
/// 100 ms, longer than another processor needs for one line.
const URGENT_ATTEMPTS: u32 = 1 << 22;

/// Hands the programmed UART to the console. Called once by `_start`.
pub(crate) fn init(uart: Uart16550<Com1>) {
    crate::cpu::without_interrupts(|| *UART.lock() = Some(uart));
}

/// Sends `bytes`, turning each LF into CR LF.
fn emit(uart: &mut Uart16550<Com1>, bytes: &[u8]) {
    for &b in bytes {
        if b == b'\n' {
            let _ = uart.write_byte(b'\r');
        }
        let _ = uart.write_byte(b);
    }
}

/// One line at a time to COM1. See the module documentation.
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
                    let g = UART.try_lock();
                    if g.is_none() {
                        core::hint::spin_loop();
                    }
                    g
                })
            } else {
                Some(UART.lock())
            };
            match guard {
                Some(mut held) => {
                    // A report before `init` (a very early panic) programs the UART first.
                    let uart = held.get_or_insert_with(|| {
                        let mut uart = unowned_uart();
                        uart.init();
                        uart
                    });
                    emit(uart, bytes);
                }
                // The holder is stuck (this processor faulted inside `flush`, or another
                // died holding the lock): send without it.
                None => emit(&mut unowned_uart(), bytes),
            }
        });
        self.len = 0;
    }
}

/// COM1 outside the console's own UART: for urgent reports that cannot get the lock, and
/// to program the UART for a report made before [`init`].
fn unowned_uart() -> Uart16550<Com1> {
    // SAFETY: CPL 0. Used only under the console lock before `init`, or by an urgent
    // report that could not get the lock; the worst a concurrent writer can then cause is
    // mixed bytes on the line, never memory unsafety, since only COM1's registers are
    // reached.
    Uart16550::new(unsafe { Com1::new() })
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
    print_lines(crate::cpu::cpu_id());
    PRINTERS_DONE.fetch_add(1, Ordering::AcqRel);
    crate::sched::exit()
}

/// Every online processor prints [`PROOF_LINES`] lines at the same time; the boot test
/// checks that every one of them arrived whole. Runs from the boot flow with every
/// scheduler active.
pub(crate) fn prove(com1: &mut Console, cpus: &[u32], bsp: usize, tsc_hz: u64) -> bool {
    use core::fmt::Write as _;
    use core::sync::atomic::Ordering;
    let mut aps = 0u64;
    for &id in cpus.iter().filter(|&&id| id as usize != bsp) {
        if crate::sched::spawn(id as usize, printer, 0).is_err() {
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
    print_lines(bsp as u64);
    crate::sched::yield_until(|| PRINTERS_DONE.load(Ordering::Acquire) == aps, tsc_hz, 5);
    let ok = PRINTERS_DONE.load(Ordering::Acquire) == aps;
    let _ = writeln!(
        com1,
        "origin-kernel: console: {} processors printed {PROOF_LINES} lines each at once{}",
        cpus.len(),
        if ok { "" } else { ": console failed" }
    );
    ok
}
