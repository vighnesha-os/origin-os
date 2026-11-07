//! A polled 16550-compatible UART: the Framekernel's first console.
//!
//! Register offsets and bit meanings follow the PC16550D UART datasheet (National
//! Semiconductor, now Texas Instruments literature number SNLS378). The driver never
//! enables UART interrupts and never waits without bound: a byte the transmitter does not
//! accept within [`TX_POLL_LIMIT`] polls of the line-status register is dropped and the
//! write fails. A missing or wedged UART therefore costs one bounded wait per `write_str`
//! call, not a hang.
//!
//! The driver reaches the hardware only through [`UartRegisters`]. The kernel binary
//! implements that trait with x86 port I/O; the unit tests implement it with a recording
//! mock, so the programming sequence and the polling bounds are tested on the build host.

use core::fmt;

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

/// Line-status polls the driver spends on one byte, or on draining the transmitter before
/// [`Uart16550::init`], before it gives up.
pub const TX_POLL_LIMIT: u32 = 100_000;

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
    /// Offset 2: the FIFO control register (FCR) when written.
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

/// The register writes that program the UART for 115200 baud, 8N1, FIFOs on, interrupts
/// off, in order.
pub const INIT_SEQUENCE: [(Register, u8); 7] = [
    // No UART interrupts: the kernel polls.
    (Register::InterruptEnable, 0x00),
    (Register::LineControl, LCR_DLAB),
    // The divisor latch, low byte then high byte.
    (Register::Data, DIVISOR.to_le_bytes()[0]),
    (Register::InterruptEnable, DIVISOR.to_le_bytes()[1]),
    // 8N1, which also clears DLAB.
    (Register::LineControl, LCR_8N1),
    (Register::FifoControl, FCR_ENABLE_CLEAR_14),
    (Register::ModemControl, MCR_DTR_RTS_OUT2),
];

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
}

impl<R: UartRegisters> Uart16550<R> {
    /// Wraps a UART's registers. Call [`Self::init`] before writing, unless the UART is
    /// already programmed.
    pub const fn new(regs: R) -> Self {
        Self { regs }
    }

    /// Programs the UART with [`INIT_SEQUENCE`].
    ///
    /// It first waits, for at most [`TX_POLL_LIMIT`] polls, until the transmitter is idle.
    /// Clearing the FIFO or changing the line settings would otherwise cut off bytes that
    /// the firmware or the loader queued just before the kernel started. If the transmitter
    /// never becomes idle, the UART is programmed anyway.
    pub fn init(&mut self) {
        let _ = self.poll_line_status(LSR_TRANSMITTER_EMPTY);
        for (reg, value) in INIT_SEQUENCE {
            self.regs.write(reg, value);
        }
    }

    /// Sends one byte once the transmitter has room for it. Fails, and drops the byte, if
    /// there is still no room after [`TX_POLL_LIMIT`] polls.
    pub fn write_byte(&mut self, byte: u8) -> fmt::Result {
        self.poll_line_status(LSR_THR_EMPTY)?;
        self.regs.write(Register::Data, byte);
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
        for byte in s.bytes() {
            if byte == b'\n' {
                self.write_byte(b'\r')?;
            }
            self.write_byte(byte)?;
        }
        Ok(())
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
        // (offset, value) exactly as the M1 COM1 bring-up specifies it.
        let expected: [(u16, u8); 7] = [
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
        assert_eq!(log.len(), 4 + INIT_SEQUENCE.len());
        assert!(
            log[..4]
                .iter()
                .all(|a| *a == Access::Read(Register::LineStatus))
        );
        let writes: Vec<Access> = INIT_SEQUENCE
            .iter()
            .map(|&(reg, value)| Access::Write(reg, value))
            .collect();
        assert_eq!(&log[4..], writes.as_slice());
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
            TX_POLL_LIMIT as usize + INIT_SEQUENCE.len()
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
    fn a_failed_byte_ends_the_write_after_one_bounded_wait() {
        let mut uart = Uart16550::new(Mock::new(u32::MAX));
        assert_eq!(uart.write_str("hello\n"), Err(fmt::Error));
        assert_eq!(uart.regs.status_reads(), TX_POLL_LIMIT as usize);
        assert!(uart.regs.data_bytes().is_empty());
    }
}
