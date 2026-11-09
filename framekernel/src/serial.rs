//! A polled 16550-compatible UART: the Framekernel's first console.
//!
//! Register offsets and bit meanings follow the PC16550D UART datasheet (National
//! Semiconductor, now Texas Instruments literature number SNLS378). The driver never
//! enables UART interrupts and never waits without bound: a byte the transmitter does not
//! accept within [`TX_POLL_LIMIT`] polls of the line-status register is dropped and the
//! write fails. A missing or wedged UART therefore costs one bounded wait per `write_str`
//! call, not a hang.
//!
//! Waiting for the wire is the slow part of a UART: at 115 200 baud a byte takes 87 µs, so
//! 100 bytes take 8.7 ms, and [`Uart16550::write_bytes`] spends that time polling. A caller
//! that must not stay put that long uses [`TxQueue`] instead: [`TxQueue::pump`] looks at the
//! line status once and, if the transmitter is empty, writes what it can take without looking
//! again, [`TX_FIFO_BYTES`] at most. It never waits, so the caller decides what happens while
//! the bytes leave.
//!
//! The driver reaches the hardware only through [`UartRegisters`]. The kernel binary
//! implements that trait with x86 port I/O; the unit tests implement it with a recording
//! mock, so the programming sequence and the polling bounds are tested on the build host.

use core::fmt;

use crate::linequeue::LineQueue;

/// The UART input clock on PC-compatible platforms: 1.8432 MHz.
pub const INPUT_CLOCK_HZ: u32 = 1_843_200;
/// Line speed in bits per second.
pub const BAUD: u32 = 115_200;
/// Baud-rate divisor. The UART samples each bit 16 times, so the divisor is
/// `INPUT_CLOCK_HZ / (16 * BAUD)`.
pub const DIVISOR: u16 = (INPUT_CLOCK_HZ / (16 * BAUD)) as u16;
const _: () = {
    assert!(
        INPUT_CLOCK_HZ.is_multiple_of(16 * BAUD),
        "16 * BAUD must divide the input clock exactly"
    );
    assert!(INPUT_CLOCK_HZ / (16 * BAUD) <= u16::MAX as u32);
    assert!(DIVISOR == 1);
};

/// Line control: divisor latch access bit (DLAB). While set, offsets 0 and 1 address the
/// divisor latch instead of the data and interrupt-enable registers.
pub const LCR_DLAB: u8 = 0x80;
/// Line control: 8 data bits (bits 0–1 = 0b11), 1 stop bit (bit 2 clear), no parity (bit 3
/// clear), DLAB clear.
pub const LCR_8N1: u8 = 0x03;
/// FIFO control: enable the FIFOs (bit 0), clear the receive (bit 1) and transmit (bit 2)
/// FIFOs, receive trigger level 14 bytes (bits 6–7 = 0b11).
pub const FCR_ENABLE_CLEAR_14: u8 = 0xC7;
/// Modem control: DTR (bit 0), RTS (bit 1) and OUT2 (bit 3).
pub const MCR_DTR_RTS_OUT2: u8 = 0x0B;
/// Line status: the transmit holding register (in FIFO mode, the transmit FIFO) is empty.
pub const LSR_THR_EMPTY: u8 = 0x20;
/// Line status: the transmitter is idle; both the FIFO and the shift register are empty.
pub const LSR_TRANSMITTER_EMPTY: u8 = 0x40;
/// Interrupt identification, read from [`Register::FifoControl`]: both bits are set when the
/// FIFOs are enabled and usable. A 16550 whose FIFO does not work sets only bit 7, and a UART
/// with no FIFO sets neither.
pub const IIR_FIFOS_ENABLED: u8 = 0xC0;

/// Line-status polls the driver spends on one byte, or on draining the transmitter before
/// [`Uart16550::init`], before it gives up.
pub const TX_POLL_LIMIT: u32 = 100_000;

/// The most bytes the driver writes without reading the line status again: the 16 bytes of a
/// 16550's transmit FIFO. [`LSR_THR_EMPTY`] means the FIFO is empty (PC16550D, line status bit
/// 5, in FIFO mode), so 16 bytes written one after another cannot overrun it, whatever the
/// line's speed; the first may move to the shift register at once and that only makes room.
/// A UART with a larger FIFO still has at least these 16, and one with none takes a byte at a
/// time ([`Uart16550::detect_fifo`]).
pub const TX_FIFO_BYTES: u32 = 16;

/// A UART register, by its offset from the UART's base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Register {
    /// Offset 0: the transmit holding register when written, or the divisor latch low byte
    /// (DLL) while [`LCR_DLAB`] is set.
    Data = 0,
    /// Offset 1: the interrupt-enable register (IER), or the divisor latch high byte (DLM)
    /// while [`LCR_DLAB`] is set.
    InterruptEnable = 1,
    /// Offset 2: the FIFO control register (FCR) when written, the interrupt identification
    /// register (IIR) when read.
    FifoControl = 2,
    /// Offset 3: the line control register (LCR).
    LineControl = 3,
    /// Offset 4: the modem control register (MCR).
    ModemControl = 4,
    /// Offset 5: the line status register (LSR).
    LineStatus = 5,
}

impl Register {
    /// The register's offset from the UART's base.
    pub const fn offset(self) -> u16 {
        self as u16
    }
}

/// The register writes that program the UART for the given baud-rate divisor, 8N1, FIFOs on,
/// interrupts off, in order.
pub const fn program_sequence(divisor: u16) -> [(Register, u8); 8] {
    [
        // DLAB off first, in case the firmware left it on: until it is, the next write
        // would reach the divisor instead of the interrupt-enable register.
        (Register::LineControl, LCR_8N1),
        // No UART interrupts: the kernel polls.
        (Register::InterruptEnable, 0x00),
        (Register::LineControl, LCR_DLAB),
        // The divisor latch, low byte then high byte.
        (Register::Data, divisor.to_le_bytes()[0]),
        (Register::InterruptEnable, divisor.to_le_bytes()[1]),
        // 8N1, which also clears DLAB.
        (Register::LineControl, LCR_8N1),
        (Register::FifoControl, FCR_ENABLE_CLEAR_14),
        (Register::ModemControl, MCR_DTR_RTS_OUT2),
    ]
}

/// What [`Uart16550::init`] writes: 115200 baud on the PC-compatible clock.
pub const INIT_SEQUENCE: [(Register, u8); 8] = program_sequence(DIVISOR);

/// The first bytes a console sent, kept to send again when it moves to another UART.
/// Bytes past the capacity are dropped and counted; the first lines of a boot are the
/// ones worth having.
#[derive(Debug)]
pub struct EarlyLog<const N: usize> {
    bytes: [u8; N],
    used: usize,
    dropped: u64,
}

impl<const N: usize> EarlyLog<N> {
    /// An empty log.
    pub const fn new() -> Self {
        Self {
            bytes: [0; N],
            used: 0,
            dropped: 0,
        }
    }

    /// Appends what fits of `bytes` and counts the rest as dropped.
    pub fn push(&mut self, bytes: &[u8]) {
        let free = N - self.used;
        let take = bytes.len().min(free);
        if let Some(slot) = self.bytes.get_mut(self.used..self.used + take)
            && let Some(source) = bytes.get(..take)
        {
            slot.copy_from_slice(source);
            self.used += take;
        }
        self.dropped += (bytes.len() - take) as u64;
    }

    /// Everything kept, oldest first.
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.used).unwrap_or(&[])
    }

    /// Bytes that did not fit.
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Empties it.
    pub fn clear(&mut self) {
        self.used = 0;
        self.dropped = 0;
    }
}

impl<const N: usize> Default for EarlyLog<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// How [`Uart16550::configure`] sets the line speed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineSetup {
    /// Program this baud-rate divisor and 8N1.
    Divisor(u16),
    /// Leave the speed and format as the firmware set them.
    Keep,
}

/// A UART's line format and divisor, as [`Uart16550::line_settings`] read them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineSettings {
    /// The line control register.
    pub line_control: u8,
    /// The baud-rate divisor.
    pub divisor: u16,
}

/// The divisor for `baud` given the UART's input clock, or `None` if the clock cannot produce
/// the rate to within 2 %. A UART samples each bit 16 times.
pub fn divisor_for(baud: u32, clock_hz: u32) -> Option<u16> {
    let per_tick = u64::from(baud).checked_mul(16)?;
    if per_tick == 0 || clock_hz == 0 {
        return None;
    }
    let clock = u64::from(clock_hz);
    let divisor = (clock + per_tick / 2) / per_tick;
    if divisor == 0 || divisor > u64::from(u16::MAX) {
        return None;
    }
    let actual = clock / (16 * divisor);
    // Within 2 % of the rate; a UART tolerates a few percent, so this is safely usable.
    (actual.abs_diff(u64::from(baud)) * 50 <= u64::from(baud)).then_some(divisor as u16)
}

/// Access to the registers of one 16550-compatible UART.
pub trait UartRegisters {
    /// Reads the register `reg`.
    fn read(&mut self, reg: Register) -> u8;
    /// Writes `value` to the register `reg`.
    fn write(&mut self, reg: Register, value: u8);
}

/// A polled 16550 driver. Its [`fmt::Write`] implementation translates `"\n"` into
/// `"\r\n"`.
#[derive(Debug)]
pub struct Uart16550<R> {
    regs: R,
    /// Bytes the transmitter takes in one go once it reports empty ([`Self::tx_load`]).
    tx_load: u32,
}

impl<R: UartRegisters> Uart16550<R> {
    /// Wraps a UART's registers. Call [`Self::init`] before writing, unless the UART is
    /// already programmed. Until [`Self::detect_fifo`] has run, or [`Self::configure`], it is
    /// taken to have no FIFO, which is always safe.
    pub const fn new(regs: R) -> Self {
        Self { regs, tx_load: 1 }
    }

    /// Bytes the transmitter takes in one go once it reports empty: [`TX_FIFO_BYTES`], or 1
    /// when it has no working FIFO.
    pub const fn tx_load(&self) -> u32 {
        self.tx_load
    }

    /// Finds out how much the transmitter takes in one go, from what the FIFO enable left in
    /// the interrupt identification register. Only a UART that says its FIFOs are on and
    /// usable is given more than a byte at a time: writing 16 bytes to a 16450, or to a first
    /// 16550 whose FIFO does not work, would lose them.
    pub fn detect_fifo(&mut self) {
        let fifos = self.regs.read(Register::FifoControl) & IIR_FIFOS_ENABLED;
        self.tx_load = if fifos == IIR_FIFOS_ENABLED {
            TX_FIFO_BYTES
        } else {
            1
        };
    }

    /// Programs the UART with [`INIT_SEQUENCE`].
    ///
    /// It first waits, for at most [`TX_POLL_LIMIT`] polls, until the transmitter is idle.
    /// Clearing the FIFO or changing the line settings would otherwise cut off bytes that
    /// the firmware or the loader queued just before the kernel started. If the transmitter
    /// never becomes idle, the UART is programmed anyway.
    pub fn init(&mut self) {
        self.configure(LineSetup::Divisor(DIVISOR));
    }

    /// Sets the UART up for polled output: interrupts off, FIFOs on and cleared, DTR, RTS and
    /// OUT2 on, and `line` for the speed. Waits for the transmitter first, as [`Self::init`]
    /// does.
    pub fn configure(&mut self, line: LineSetup) {
        let _ = self.poll_line_status(LSR_TRANSMITTER_EMPTY);
        match line {
            LineSetup::Divisor(divisor) => {
                for (reg, value) in program_sequence(divisor) {
                    self.regs.write(reg, value);
                }
            }
            LineSetup::Keep => {
                // DLAB off first, in case the firmware left it set: until then, offset 1 is
                // the divisor's high byte, not the interrupt-enable register.
                let line_control = self.regs.read(Register::LineControl);
                self.regs
                    .write(Register::LineControl, line_control & !LCR_DLAB);
                self.regs.write(Register::InterruptEnable, 0);
                self.regs.write(Register::FifoControl, FCR_ENABLE_CLEAR_14);
                self.regs.write(Register::ModemControl, MCR_DTR_RTS_OUT2);
            }
        }
        self.detect_fifo();
    }

    /// Reads the line format and divisor the firmware left, without changing them.
    pub fn line_settings(&mut self) -> LineSettings {
        let line_control = self.regs.read(Register::LineControl);
        self.regs
            .write(Register::LineControl, line_control | LCR_DLAB);
        let low = self.regs.read(Register::Data);
        let high = self.regs.read(Register::InterruptEnable);
        self.regs.write(Register::LineControl, line_control);
        LineSettings {
            line_control,
            divisor: u16::from_le_bytes([low, high]),
        }
    }

    /// Puts back settings that [`Self::line_settings`] read, as they were.
    pub fn restore(&mut self, saved: LineSettings) {
        let _ = self.poll_line_status(LSR_TRANSMITTER_EMPTY);
        let [low, high] = saved.divisor.to_le_bytes();
        self.regs
            .write(Register::LineControl, saved.line_control | LCR_DLAB);
        self.regs.write(Register::Data, low);
        self.regs.write(Register::InterruptEnable, high);
        self.regs
            .write(Register::LineControl, saved.line_control & !LCR_DLAB);
    }

    /// Sends one byte once the transmitter has room for it. Fails, and drops the byte, if
    /// there is still no room after [`TX_POLL_LIMIT`] polls.
    pub fn write_byte(&mut self, byte: u8) -> fmt::Result {
        self.poll_line_status(LSR_THR_EMPTY)?;
        self.regs.write(Register::Data, byte);
        Ok(())
    }

    /// Sends `bytes`, turning LF into CR LF, stopping at the first byte the UART will not
    /// take: one bounded wait, however long the line.
    pub fn write_bytes(&mut self, bytes: &[u8]) -> fmt::Result {
        for &byte in bytes {
            if byte == b'\n' {
                self.write_byte(b'\r')?;
            }
            self.write_byte(byte)?;
        }
        Ok(())
    }

    /// Polls the line status until any bit in `mask` is set, at most [`TX_POLL_LIMIT`]
    /// times.
    fn poll_line_status(&mut self, mask: u8) -> fmt::Result {
        for _ in 0..TX_POLL_LIMIT {
            if self.regs.read(Register::LineStatus) & mask != 0 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(fmt::Error)
    }
}

impl<R: UartRegisters> fmt::Write for Uart16550<R> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_bytes(s.as_bytes())
    }
}

/// What the UART is told in place of lines its queue had no room for.
const TX_LOST_NOTICE: &[u8] =
    b"origin-kernel: console: some lines were lost here: the UART fell behind\n";

/// What one [`TxQueue::pump`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pumped {
    /// Nothing was waiting. No register was touched.
    Idle,
    /// The transmitter took this many bytes.
    Sent(u32),
    /// The transmitter had no room yet, and nothing was written.
    Busy,
    /// The transmitter has had no room for the whole of the wait the driver allows it (see
    /// [`TxQueue::pump`]), `polls` line-status reads in all: it has stopped.
    Stopped {
        /// How many reads that was.
        polls: u32,
    },
}

/// Text waiting for a UART, sent a bounded amount at a time ([`Self::pump`]), with each LF
/// turned into CR LF. The caller serializes everything, and decides what happens between
/// one call and the next.
///
/// Positions count the bytes of the text as it was queued, from the start of time, as for
/// [`LineQueue`]: a line is on the UART when [`Self::sent`] has reached the position
/// [`Self::push`] returned, which is when the last of its bytes has been written to the UART.
#[derive(Debug)]
pub struct TxQueue<const N: usize> {
    lines: LineQueue<N>,
    /// Bytes of the queued text the UART has taken.
    sent: u64,
    /// The line feed at the front of the queue has had its carriage return sent, and is next.
    cr_sent: bool,
    /// Polls in a row that found the transmitter busy.
    stalled: u32,
    /// How many bytes the transmitter took the last time: what it has been busy with since.
    load: u32,
}

impl<const N: usize> TxQueue<N> {
    /// An empty queue.
    pub const fn new() -> Self {
        Self {
            lines: LineQueue::new(TX_LOST_NOTICE),
            sent: 0,
            cr_sent: false,
            stalled: 0,
            load: 0,
        }
    }

    /// Whether [`Self::push`] would take `bytes` more bytes now.
    pub const fn has_room(&self, bytes: usize) -> bool {
        self.lines.has_room(bytes)
    }

    /// Accepts `text`, whole or not at all, and returns the position at which it will be on
    /// the UART. `None` when there is no room.
    pub fn push(&mut self, text: &[u8]) -> Option<u64> {
        self.lines.push(text)
    }

    /// Records that a line had to be dropped because a caller could not wait for room: the
    /// next line that fits is preceded by a notice saying so.
    pub const fn note_lost(&mut self) {
        self.lines.note_lost();
    }

    /// Writes as much of the waiting text as the transmitter can take without being asked
    /// again, and returns at once. One read of the line status and, if the transmitter says
    /// it is empty, at most [`Uart16550::tx_load`] writes, so never more than
    /// [`TX_FIFO_BYTES`] bytes: this is the whole of what a call does, however long the
    /// text, and however slow the line.
    ///
    /// A UART that stops taking bytes is given up on after one bounded wait, spread over
    /// however many calls it takes: [`TX_POLL_LIMIT`] polls in a row that find it busy, for
    /// each byte of the last load it took. That is what polling a byte at a time allows a
    /// transmitter: a full FIFO takes 16 times as long to get through as one byte. The very
    /// first bytes have no load before them, and get the wait for one.
    pub fn pump<R: UartRegisters>(&mut self, uart: &mut Uart16550<R>) -> Pumped {
        if self.lines.is_empty() {
            return Pumped::Idle;
        }
        if uart.regs.read(Register::LineStatus) & LSR_THR_EMPTY == 0 {
            self.stalled = self.stalled.saturating_add(1);
            return if self.stalled >= TX_POLL_LIMIT.saturating_mul(self.load.max(1)) {
                Pumped::Stopped {
                    polls: self.stalled,
                }
            } else {
                Pumped::Busy
            };
        }
        self.stalled = 0;
        let mut written = 0;
        while written < uart.tx_load {
            let Some(byte) = self.lines.peek() else {
                break;
            };
            if byte == b'\n' && !self.cr_sent {
                uart.regs.write(Register::Data, b'\r');
                self.cr_sent = true;
            } else {
                uart.regs.write(Register::Data, byte);
                self.cr_sent = false;
                self.lines.pop();
                self.sent += 1;
            }
            written += 1;
        }
        self.load = written;
        Pumped::Sent(written)
    }

    /// How far the UART has got (see the type's documentation).
    pub const fn sent(&self) -> u64 {
        self.sent
    }

    /// Whether nothing is waiting.
    pub const fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Forgets everything waiting, and what the queue knew about the UART: the UART has
    /// stopped, or another has taken its place. Nothing is left for anyone to wait for.
    pub const fn clear(&mut self) {
        self.lines.clear();
        self.sent = self.lines.queued();
        self.cr_sent = false;
        self.stalled = 0;
        self.load = 0;
    }
}

impl<const N: usize> Default for TxQueue<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::fmt::Write;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Access {
        Read(Register),
        Write(Register, u8),
    }

    /// A UART that records every access. The line status reports "busy" (0) for the first
    /// `busy_polls` reads, then `ready`.
    #[derive(Debug)]
    struct Mock {
        log: Vec<Access>,
        busy_polls: u32,
        ready: u8,
    }

    impl Mock {
        fn new(busy_polls: u32) -> Self {
            Self {
                log: Vec::new(),
                busy_polls,
                ready: LSR_THR_EMPTY | LSR_TRANSMITTER_EMPTY,
            }
        }

        fn data_bytes(&self) -> Vec<u8> {
            self.log
                .iter()
                .filter_map(|a| match a {
                    Access::Write(Register::Data, b) => Some(*b),
                    _ => None,
                })
                .collect()
        }

        fn status_reads(&self) -> usize {
            self.log
                .iter()
                .filter(|a| **a == Access::Read(Register::LineStatus))
                .count()
        }
    }

    impl UartRegisters for Mock {
        fn read(&mut self, reg: Register) -> u8 {
            self.log.push(Access::Read(reg));
            match reg {
                Register::LineStatus if self.busy_polls > 0 => {
                    self.busy_polls -= 1;
                    0
                }
                Register::LineStatus => self.ready,
                _ => 0,
            }
        }

        fn write(&mut self, reg: Register, value: u8) {
            self.log.push(Access::Write(reg, value));
        }
    }

    #[test]
    fn init_sequence_is_115200_8n1() {
        // (offset, value): the M1 COM1 bring-up, after the write that clears a stray DLAB.
        let expected: [(u16, u8); 8] = [
            (3, 0x03),
            (1, 0x00),
            (3, 0x80),
            (0, 0x01),
            (1, 0x00),
            (3, 0x03),
            (2, 0xC7),
            (4, 0x0B),
        ];
        let actual = INIT_SEQUENCE.map(|(reg, value)| (reg.offset(), value));
        assert_eq!(actual, expected);
        assert_eq!(Register::LineStatus.offset(), 5);
    }

    #[test]
    fn init_waits_for_an_idle_transmitter_then_programs_in_order() {
        let mut uart = Uart16550::new(Mock::new(3));
        uart.init();
        let log = &uart.regs.log;
        assert_eq!(log.len(), 4 + INIT_SEQUENCE.len() + 1);
        assert!(
            log[..4]
                .iter()
                .all(|a| *a == Access::Read(Register::LineStatus))
        );
        let writes: Vec<Access> = INIT_SEQUENCE
            .iter()
            .map(|&(reg, value)| Access::Write(reg, value))
            .collect();
        assert_eq!(&log[4..log.len() - 1], writes.as_slice());
        assert_eq!(
            log[log.len() - 1],
            Access::Read(Register::FifoControl),
            "last of all it finds out whether the FIFOs came on"
        );
    }

    #[test]
    fn init_needs_the_transmitter_idle_bit_not_just_room() {
        let mut mock = Mock::new(0);
        mock.ready = LSR_THR_EMPTY;
        let mut uart = Uart16550::new(mock);
        uart.init();
        // It polled to the limit, then programmed the UART anyway.
        assert_eq!(uart.regs.status_reads(), TX_POLL_LIMIT as usize);
        assert_eq!(
            uart.regs.log.len(),
            TX_POLL_LIMIT as usize + INIT_SEQUENCE.len() + 1
        );
    }

    #[test]
    fn newline_becomes_crlf() {
        let mut uart = Uart16550::new(Mock::new(0));
        uart.write_str("a\nb\n\n").unwrap();
        assert_eq!(uart.regs.data_bytes(), b"a\r\nb\r\n\r\n");
        // One status poll before every byte sent.
        assert_eq!(uart.regs.status_reads(), 8);
    }

    #[test]
    fn formatting_goes_through_write_str() {
        let mut uart = Uart16550::new(Mock::new(0));
        writeln!(uart, "x={:#x}", 255).unwrap();
        assert_eq!(uart.regs.data_bytes(), b"x=0xff\r\n");
    }

    #[test]
    fn a_busy_transmitter_is_polled_until_ready() {
        let mut uart = Uart16550::new(Mock::new(TX_POLL_LIMIT - 1));
        assert_eq!(uart.write_byte(b'z'), Ok(()));
        assert_eq!(uart.regs.status_reads(), TX_POLL_LIMIT as usize);
        assert_eq!(uart.regs.data_bytes(), b"z");
    }

    #[test]
    fn a_wedged_transmitter_fails_after_the_poll_limit() {
        let mut uart = Uart16550::new(Mock::new(TX_POLL_LIMIT));
        assert_eq!(uart.write_byte(b'z'), Err(fmt::Error));
        assert_eq!(uart.regs.status_reads(), TX_POLL_LIMIT as usize);
        assert!(uart.regs.data_bytes().is_empty());
    }

    #[test]
    fn program_sequence_writes_the_divisor_it_is_given() {
        let seq = program_sequence(0x0102);
        assert_eq!(seq[3], (Register::Data, 0x02));
        assert_eq!(seq[4], (Register::InterruptEnable, 0x01));
        assert_eq!(program_sequence(DIVISOR), INIT_SEQUENCE);
    }

    #[test]
    fn keep_leaves_the_divisor_and_format_alone() {
        let mut uart = Uart16550::new(Mock::new(0));
        uart.configure(LineSetup::Keep);
        let writes: Vec<(Register, u8)> = uart
            .regs
            .log
            .iter()
            .filter_map(|a| match a {
                Access::Write(reg, value) => Some((*reg, *value)),
                _ => None,
            })
            .collect();
        // The mock reads LCR back as 0, so it is written back with DLAB clear; the divisor
        // latch is never touched and the format is not rewritten to 8N1.
        assert_eq!(
            writes,
            vec![
                (Register::LineControl, 0),
                (Register::InterruptEnable, 0),
                (Register::FifoControl, FCR_ENABLE_CLEAR_14),
                (Register::ModemControl, MCR_DTR_RTS_OUT2),
            ]
        );
    }

    #[test]
    fn keep_clears_a_stray_dlab_before_touching_the_interrupt_enable_register() {
        struct LatchFirst(Mock);
        impl UartRegisters for LatchFirst {
            fn read(&mut self, reg: Register) -> u8 {
                self.0.read(reg);
                match reg {
                    Register::LineStatus => self.0.ready,
                    Register::LineControl => LCR_DLAB | 0x03,
                    _ => 0,
                }
            }
            fn write(&mut self, reg: Register, value: u8) {
                self.0.write(reg, value);
            }
        }
        let mut uart = Uart16550::new(LatchFirst(Mock::new(0)));
        uart.configure(LineSetup::Keep);
        let log = &uart.regs.0.log;
        let dlab_off = log
            .iter()
            .position(|a| *a == Access::Write(Register::LineControl, 0x03))
            .expect("DLAB is cleared");
        let interrupts_off = log
            .iter()
            .position(|a| *a == Access::Write(Register::InterruptEnable, 0))
            .expect("interrupts are turned off");
        assert!(dlab_off < interrupts_off);
    }

    /// A UART that behaves like one for the registers `line_settings` and `restore` use.
    struct Registers {
        line_control: u8,
        divisor: [u8; 2],
    }

    impl UartRegisters for Registers {
        fn read(&mut self, reg: Register) -> u8 {
            match reg {
                Register::LineStatus => LSR_THR_EMPTY | LSR_TRANSMITTER_EMPTY,
                Register::LineControl => self.line_control,
                Register::Data if self.line_control & LCR_DLAB != 0 => self.divisor[0],
                Register::InterruptEnable if self.line_control & LCR_DLAB != 0 => self.divisor[1],
                _ => 0,
            }
        }

        fn write(&mut self, reg: Register, value: u8) {
            match reg {
                Register::LineControl => self.line_control = value,
                Register::Data if self.line_control & LCR_DLAB != 0 => self.divisor[0] = value,
                Register::InterruptEnable if self.line_control & LCR_DLAB != 0 => {
                    self.divisor[1] = value;
                }
                _ => {}
            }
        }
    }

    #[test]
    fn firmware_line_settings_can_be_read_and_put_back() {
        let mut uart = Uart16550::new(Registers {
            line_control: 0x03,
            divisor: [0x0C, 0x00], // 9600 baud
        });
        let saved = uart.line_settings();
        assert_eq!(
            saved,
            LineSettings {
                line_control: 0x03,
                divisor: 12
            }
        );
        assert_eq!(uart.regs.line_control, 0x03, "DLAB is back off");
        uart.init();
        assert_eq!(uart.regs.divisor, [1, 0], "init programs 115200");
        uart.restore(saved);
        assert_eq!(uart.regs.divisor, [0x0C, 0x00]);
        assert_eq!(uart.regs.line_control, 0x03);
    }

    #[test]
    fn the_early_log_keeps_order_and_counts_what_did_not_fit() {
        let mut log = EarlyLog::<10>::new();
        log.push(b"abc");
        log.push(b"defg");
        assert_eq!(log.as_bytes(), b"abcdefg");
        assert_eq!(log.dropped(), 0);
        log.push(b"hijklm");
        assert_eq!(log.as_bytes(), b"abcdefghij");
        assert_eq!(log.dropped(), 3);
        log.push(b"zz");
        assert_eq!(log.as_bytes(), b"abcdefghij");
        assert_eq!(log.dropped(), 5);
        log.clear();
        assert_eq!(log.as_bytes(), b"");
        assert_eq!(log.dropped(), 0);
        log.push(b"xy");
        assert_eq!(log.as_bytes(), b"xy");
    }

    #[test]
    fn divisors_come_from_the_rate_and_the_clock() {
        assert_eq!(divisor_for(115_200, INPUT_CLOCK_HZ), Some(1));
        assert_eq!(divisor_for(9_600, INPUT_CLOCK_HZ), Some(12));
        assert_eq!(divisor_for(115_200, 48_000_000), Some(26)); // 115 384 baud, 0.16 % fast
        assert_eq!(divisor_for(1_500_000, 48_000_000), Some(2));
        // The clock cannot reach the rate, or the divisor does not fit in 16 bits.
        assert_eq!(divisor_for(115_200, 1_000_000), None);
        assert_eq!(divisor_for(1, u32::MAX), None);
        assert_eq!(divisor_for(0, INPUT_CLOCK_HZ), None);
        assert_eq!(divisor_for(115_200, 0), None);
        // Never a panic on the extremes.
        assert_eq!(divisor_for(u32::MAX, u32::MAX), None);
    }

    #[test]
    fn a_wedged_uart_costs_one_bounded_wait_for_a_whole_line_of_bytes() {
        let mut uart = Uart16550::new(Mock::new(u32::MAX));
        assert_eq!(
            uart.write_bytes(b"a line of sixty bytes that a wedged UART will never take\n"),
            Err(fmt::Error)
        );
        assert_eq!(uart.regs.status_reads(), TX_POLL_LIMIT as usize);
        assert!(uart.regs.data_bytes().is_empty());
    }

    #[test]
    fn a_failed_byte_ends_the_write_after_one_bounded_wait() {
        let mut uart = Uart16550::new(Mock::new(u32::MAX));
        assert_eq!(uart.write_str("hello\n"), Err(fmt::Error));
        assert_eq!(uart.regs.status_reads(), TX_POLL_LIMIT as usize);
        assert!(uart.regs.data_bytes().is_empty());
    }

    /// A 16550 as its datasheet has it, its clock counted in register accesses: each byte
    /// takes `byte_time` accesses to leave, the FIFO holds 16 bytes (1 without a FIFO) and
    /// the shift register takes the next one the moment it is free. A write to a full FIFO is
    /// an overrun and the byte is lost.
    struct Wire {
        fifo: std::collections::VecDeque<u8>,
        capacity: usize,
        shifting: Option<u8>,
        left: u32,
        byte_time: u32,
        wire: Vec<u8>,
        overruns: u32,
        /// Register accesses so far.
        accesses: u64,
        /// Bytes written since the line status was last read.
        unchecked: u32,
        most_unchecked: u32,
        stopped: bool,
    }

    impl Wire {
        fn new(byte_time: u32) -> Self {
            Self {
                fifo: Default::default(),
                capacity: TX_FIFO_BYTES as usize,
                shifting: None,
                left: 0,
                byte_time,
                wire: Vec::new(),
                overruns: 0,
                accesses: 0,
                unchecked: 0,
                most_unchecked: 0,
                stopped: false,
            }
        }

        fn without_fifo(byte_time: u32) -> Self {
            Self {
                capacity: 1,
                ..Self::new(byte_time)
            }
        }

        fn clock(&mut self) {
            self.accesses += 1;
            if self.stopped {
                return;
            }
            if self.left > 0 {
                self.left -= 1;
            }
            if self.left == 0 {
                self.wire.extend(self.shifting.take());
                if let Some(next) = self.fifo.pop_front() {
                    self.shifting = Some(next);
                    self.left = self.byte_time;
                }
            }
        }

        /// Runs the wire until everything written has left.
        fn drain(&mut self) {
            while self.shifting.is_some() || !self.fifo.is_empty() {
                self.clock();
            }
        }
    }

    impl UartRegisters for Wire {
        fn read(&mut self, reg: Register) -> u8 {
            self.clock();
            match reg {
                Register::LineStatus => {
                    self.unchecked = 0;
                    let idle = self.shifting.is_none() && self.fifo.is_empty();
                    (if self.fifo.is_empty() {
                        LSR_THR_EMPTY
                    } else {
                        0
                    }) | (if idle { LSR_TRANSMITTER_EMPTY } else { 0 })
                }
                Register::FifoControl if self.capacity > 1 => 0xC1,
                Register::FifoControl => 0x01,
                _ => 0,
            }
        }

        fn write(&mut self, reg: Register, value: u8) {
            self.clock();
            if reg == Register::Data {
                self.unchecked += 1;
                self.most_unchecked = self.most_unchecked.max(self.unchecked);
                if self.fifo.len() == self.capacity {
                    self.overruns += 1;
                } else {
                    self.fifo.push_back(value);
                }
            }
        }
    }

    fn with_crlf(text: &[u8]) -> Vec<u8> {
        text.iter()
            .flat_map(|&b| {
                if b == b'\n' {
                    vec![b'\r', b'\n']
                } else {
                    vec![b]
                }
            })
            .collect()
    }

    fn configured(wire: Wire) -> Uart16550<Wire> {
        let mut uart = Uart16550::new(wire);
        uart.configure(LineSetup::Keep);
        uart
    }

    #[test]
    fn the_transmitter_gets_a_fifo_load_only_if_it_says_its_fifos_work() {
        struct Ident(u8);
        impl UartRegisters for Ident {
            fn read(&mut self, reg: Register) -> u8 {
                match reg {
                    Register::LineStatus => LSR_THR_EMPTY | LSR_TRANSMITTER_EMPTY,
                    Register::FifoControl => self.0,
                    _ => 0,
                }
            }
            fn write(&mut self, _: Register, _: u8) {}
        }
        for (ident, load) in [
            (0xC1, 16), // a working 16550A or later, nothing pending
            (0xC2, 16), // the low bits are the interrupt state
            (0x81, 1),  // the first 16550, whose FIFO does not work
            (0x41, 1),
            (0x01, 1), // a 16450: no FIFO
            (0x00, 1),
        ] {
            let mut uart = Uart16550::new(Ident(ident));
            assert_eq!(uart.tx_load(), 1, "no FIFO until one is seen");
            uart.init();
            assert_eq!(uart.tx_load(), load, "IIR {ident:#x}");
            let mut uart = Uart16550::new(Ident(ident));
            uart.configure(LineSetup::Keep);
            assert_eq!(
                uart.tx_load(),
                load,
                "IIR {ident:#x}, as the firmware left it"
            );
        }
    }

    #[test]
    fn a_section_reads_the_status_once_and_writes_at_most_a_fifo_load() {
        let mut uart = Uart16550::new(Mock::new(0));
        uart.tx_load = TX_FIFO_BYTES;
        let mut queue = TxQueue::<64>::new();
        queue.push(&[b'x'; 40]).unwrap();
        for (section, expected) in [16, 16, 8].into_iter().enumerate() {
            uart.regs.log.clear();
            assert_eq!(queue.pump(&mut uart), Pumped::Sent(expected), "{section}");
            assert_eq!(uart.regs.status_reads(), 1, "section {section}");
            assert_eq!(uart.regs.data_bytes().len(), expected as usize);
            assert_eq!(
                uart.regs.log.first(),
                Some(&Access::Read(Register::LineStatus)),
                "the status comes before any byte"
            );
        }
        assert_eq!(queue.pump(&mut uart), Pumped::Idle);
    }

    #[test]
    fn nothing_waiting_touches_no_register() {
        let mut uart = Uart16550::new(Mock::new(0));
        let mut queue = TxQueue::<64>::new();
        assert_eq!(queue.pump(&mut uart), Pumped::Idle);
        assert!(uart.regs.log.is_empty());
    }

    #[test]
    fn a_busy_transmitter_gets_no_bytes_and_one_read() {
        let mut uart = Uart16550::new(Mock::new(2));
        uart.tx_load = TX_FIFO_BYTES;
        let mut queue = TxQueue::<64>::new();
        queue.push(b"abc\n").unwrap();
        assert_eq!(queue.pump(&mut uart), Pumped::Busy);
        assert_eq!(queue.pump(&mut uart), Pumped::Busy);
        assert_eq!(uart.regs.status_reads(), 2);
        assert!(uart.regs.data_bytes().is_empty());
        assert_eq!(queue.pump(&mut uart), Pumped::Sent(5));
        assert_eq!(uart.regs.data_bytes(), b"abc\r\n");
    }

    #[test]
    fn a_line_feed_cut_by_the_end_of_a_section_keeps_its_carriage_return_and_its_place() {
        let mut uart = Uart16550::new(Mock::new(0));
        uart.tx_load = TX_FIFO_BYTES;
        let mut queue = TxQueue::<64>::new();
        // 15 bytes, and the LF at the end: the CR is the 16th byte of the section.
        let mut line = [b'a'; 16];
        line[15] = b'\n';
        let end = queue.push(&line).unwrap();
        queue.push(b"b\n").unwrap();
        assert_eq!(queue.pump(&mut uart), Pumped::Sent(16));
        assert_eq!(queue.sent(), 15, "the line is not out until its LF is");
        assert!(queue.sent() < end);
        assert_eq!(queue.pump(&mut uart), Pumped::Sent(4));
        assert_eq!(queue.sent(), end + 2);
        let mut expected = vec![b'a'; 15];
        expected.extend_from_slice(b"\r\nb\r\n");
        assert_eq!(uart.regs.data_bytes(), expected);
    }

    #[test]
    fn a_line_is_on_the_uart_at_its_position_and_not_before() {
        let mut uart = Uart16550::new(Mock::new(0));
        let mut queue = TxQueue::<64>::new();
        let first = queue.push(b"one\n").unwrap();
        let second = queue.push(b"two\n").unwrap();
        // A byte at a time, so that every position is seen.
        for pumps in 1..=10u32 {
            assert!(matches!(queue.pump(&mut uart), Pumped::Sent(1)));
            let wire = uart.regs.data_bytes();
            assert_eq!(wire.len(), pumps as usize);
            assert_eq!(
                queue.sent() >= first,
                wire.ends_with(b"one\r\n") || pumps > 5
            );
            assert_eq!(queue.sent() >= second, pumps == 10);
        }
        assert!(queue.is_empty());
        assert_eq!(uart.regs.data_bytes(), b"one\r\ntwo\r\n");
    }

    #[test]
    fn a_line_queued_while_another_is_on_its_way_goes_out_behind_it_whole() {
        let mut uart = Uart16550::new(Mock::new(0));
        uart.tx_load = TX_FIFO_BYTES;
        let mut queue = TxQueue::<256>::new();
        let first = [b'a'; 40];
        queue.push(&first).unwrap();
        queue.pump(&mut uart);
        // The interrupt handler's line, or another processor's, arrives here.
        let second = [b'b'; 40];
        queue.push(&second).unwrap();
        while queue.pump(&mut uart) != Pumped::Idle {}
        let mut expected = first.to_vec();
        expected.extend_from_slice(&second);
        assert_eq!(uart.regs.data_bytes(), expected);
    }

    #[test]
    fn a_full_queue_refuses_a_whole_line_and_owns_up_on_the_uart() {
        let mut uart = Uart16550::new(Mock::new(0));
        let mut queue = TxQueue::<128>::new();
        assert!(queue.push(&[b'a'; 100]).is_some());
        assert!(!queue.has_room(100));
        assert!(queue.push(&[b'b'; 100]).is_none());
        queue.note_lost();
        while queue.pump(&mut uart) != Pumped::Idle {}
        assert!(queue.push(b"next\n").is_some());
        while queue.pump(&mut uart) != Pumped::Idle {}
        let wire = uart.regs.data_bytes();
        assert_eq!(&wire[..100], [b'a'; 100].as_slice());
        let mut told = with_crlf(TX_LOST_NOTICE);
        told.extend_from_slice(b"next\r\n");
        assert_eq!(&wire[100..], told.as_slice());
    }

    #[test]
    fn a_wedged_uart_is_given_up_on_after_one_bounded_wait_over_many_sections() {
        // The status never reports room. The log would be enormous, so a count will do.
        struct Wedged(u64);
        impl UartRegisters for Wedged {
            fn read(&mut self, reg: Register) -> u8 {
                self.0 += u64::from(reg == Register::LineStatus);
                0
            }
            fn write(&mut self, reg: Register, _: u8) {
                assert_ne!(reg, Register::Data, "a wedged UART is given no bytes");
            }
        }
        let mut uart = Uart16550::new(Wedged(0));
        let mut queue = TxQueue::<256>::new();
        // However many lines are waiting, it is one wait.
        for _ in 0..10 {
            queue.push(b"a whole line that a wedged UART will never take\n");
        }
        for poll in 1..TX_POLL_LIMIT {
            assert_eq!(queue.pump(&mut uart), Pumped::Busy, "poll {poll}");
        }
        assert_eq!(
            queue.pump(&mut uart),
            Pumped::Stopped {
                polls: TX_POLL_LIMIT
            }
        );
        assert_eq!(uart.regs.0, u64::from(TX_POLL_LIMIT));
        let queued = queue.lines.queued();
        queue.clear();
        assert!(queue.is_empty());
        assert_eq!(queue.sent(), queued, "nobody is left waiting for it");
        assert_eq!(queue.pump(&mut uart), Pumped::Idle);
        assert_eq!(uart.regs.0, u64::from(TX_POLL_LIMIT), "and no more waits");
    }

    #[test]
    fn a_uart_that_stops_after_a_full_load_gets_as_long_as_that_load_needs() {
        struct StopsAfter(u32);
        impl UartRegisters for StopsAfter {
            fn read(&mut self, _: Register) -> u8 {
                if self.0 == 0 {
                    return 0;
                }
                self.0 -= 1;
                LSR_THR_EMPTY
            }
            fn write(&mut self, _: Register, _: u8) {}
        }
        let mut uart = Uart16550::new(StopsAfter(1));
        uart.tx_load = TX_FIFO_BYTES;
        let mut queue = TxQueue::<64>::new();
        queue.push(&[b'x'; 32]).unwrap();
        assert_eq!(queue.pump(&mut uart), Pumped::Sent(TX_FIFO_BYTES));
        let limit = TX_POLL_LIMIT * TX_FIFO_BYTES;
        for poll in 1..limit {
            assert_eq!(queue.pump(&mut uart), Pumped::Busy, "poll {poll}");
        }
        assert_eq!(queue.pump(&mut uart), Pumped::Stopped { polls: limit });
    }

    #[test]
    fn a_transmitter_that_recovers_starts_its_wait_again() {
        let mut uart = Uart16550::new(Wire::new(1));
        let mut queue = TxQueue::<64>::new();
        queue.push(b"ab").unwrap();
        // One poll short of the limit: a byte taken starts the count again.
        queue.stalled = TX_POLL_LIMIT - 1;
        assert_eq!(queue.pump(&mut uart), Pumped::Sent(1));
        assert_eq!(queue.stalled, 0);
        assert_eq!(queue.pump(&mut uart), Pumped::Sent(1));
    }

    /// Sends `lines` through a queue the way the console does: every call to `pump` is one
    /// section with interrupts off; between them the wire runs on.
    fn send_all(uart: &mut Uart16550<Wire>, queue: &mut TxQueue<512>, lines: &[Vec<u8>]) -> u64 {
        let mut worst = 0;
        for line in lines {
            let end = queue.push(line).expect("the queue has room");
            while queue.sent() < end {
                let before = uart.regs.accesses;
                assert!(!matches!(queue.pump(uart), Pumped::Stopped { .. }));
                worst = worst.max(uart.regs.accesses - before);
                uart.regs.clock(); // Interrupts on: the wire runs while no section is open.
            }
        }
        worst
    }

    #[test]
    fn however_slow_the_line_a_section_is_a_status_read_and_a_fifo_load() {
        use crate::testing::Rng;
        let mut rng = Rng::new(0x16550);
        for case in 0..crate::testing::cases(300) {
            // Accesses per byte on the wire: from a line that keeps up with the bus, through
            // 115 200 baud at a microsecond an access (87), to 9600 baud (1042).
            let byte_time = [1, 2, 5, 17, 87, 1042][rng.below(6)];
            let mut uart = configured(Wire::new(byte_time));
            let mut queue = TxQueue::<512>::new();
            let lines: Vec<Vec<u8>> = (0..1 + rng.below(8))
                .map(|_| {
                    let mut line: Vec<u8> =
                        (0..rng.below(160)).map(|_| rng.byte() & 0x7F).collect();
                    line.push(b'\n');
                    line
                })
                .collect();
            let worst = send_all(&mut uart, &mut queue, &lines);
            uart.regs.drain();
            assert!(
                worst <= 1 + u64::from(TX_FIFO_BYTES),
                "case {case}: {worst}"
            );
            assert!(
                uart.regs.most_unchecked <= TX_FIFO_BYTES,
                "case {case}: {} bytes between status reads",
                uart.regs.most_unchecked
            );
            assert_eq!(uart.regs.overruns, 0, "case {case}: bytes lost");
            assert_eq!(uart.regs.wire, with_crlf(&lines.concat()), "case {case}");
        }
    }

    #[test]
    fn without_a_fifo_a_section_is_a_status_read_and_one_byte() {
        let mut uart = configured(Wire::without_fifo(50));
        assert_eq!(uart.tx_load(), 1);
        let mut queue = TxQueue::<512>::new();
        let lines = [b"no FIFO at all\n".to_vec()];
        let worst = send_all(&mut uart, &mut queue, &lines);
        uart.regs.drain();
        assert_eq!(worst, 2);
        assert_eq!(uart.regs.overruns, 0);
        assert_eq!(uart.regs.wire, with_crlf(&lines[0]));
    }

    /// The negative control of the two tests above: on the model, a load larger than the
    /// FIFO loses bytes and more than one load between status reads is seen, so a queue that
    /// ignored its bound would fail them.
    #[test]
    fn the_model_catches_a_load_larger_than_the_fifo() {
        let mut uart = configured(Wire::new(1000));
        let mut queue = TxQueue::<512>::new();
        queue.push(b"a").unwrap();
        uart.tx_load = 1;
        assert_eq!(queue.pump(&mut uart), Pumped::Sent(1));
        // That byte moves to the shift register, which keeps it for a long time: the FIFO
        // reports empty, and has room for 16 and no more.
        uart.regs.clock();
        uart.tx_load = TX_FIFO_BYTES + 1;
        queue.push(&[b'x'; 100]).unwrap();
        assert_eq!(queue.pump(&mut uart), Pumped::Sent(TX_FIFO_BYTES + 1));
        uart.regs.drain();
        assert_eq!(uart.regs.most_unchecked, TX_FIFO_BYTES + 1);
        assert_eq!(uart.regs.overruns, 1, "the FIFO has room for 16 bytes");
        assert_eq!(uart.regs.wire.len(), 1 + TX_FIFO_BYTES as usize);
    }

    #[test]
    fn the_model_catches_a_fifo_load_written_to_a_uart_without_one() {
        let mut uart = Uart16550::new(Wire::without_fifo(1000));
        uart.tx_load = TX_FIFO_BYTES;
        let mut queue = TxQueue::<512>::new();
        queue.push(&[b'x'; 100]).unwrap();
        assert_eq!(queue.pump(&mut uart), Pumped::Sent(TX_FIFO_BYTES));
        uart.regs.drain();
        assert!(uart.regs.overruns > 0);
    }
}
