//! The loader's one direct write to a UART: why it halts after `ExitBootServices` failed
//! (decision item 342; ADR-0002 B5 and ADR-0010, as TASKS item 112 amends them).
//!
//! Until `ExitBootServices`, the loader prints only through the firmware's console. After a
//! failed call it may not use the firmware any more (UEFI 2.11 §7.4), so a loader that halts
//! there says nothing, and a machine whose console is the SPCR's UART, as a rented server's is,
//! shows no reason. So before it halts, the loader writes one line, [`Halting`], to the UART boot
//! info would name: a 16550 in I/O space that the loader's walk found. It writes straight to that
//! UART's registers, as the firmware left them programmed ([`Uart`]): no firmware service is
//! called, each byte waits for room at most [`POLLS`] line-status reads, and a byte that never
//! finds room ends the line. A boot whose call succeeds writes nothing there.
//!
//! The port instructions are `main.rs`'s, through [`Port`], so this module is tested on the build
//! host. A bench build (TASKS item 112, `crate::bench`) writes its own lines through [`Uart`] too.
#![forbid(unsafe_code)]

use core::fmt;

use crate::uefi::Status;

/// Reads and writes of I/O ports, which `main.rs` does with `in` and `out`.
pub trait Port {
    /// Reads the byte at `port`.
    fn read(&mut self, port: u16) -> u8;
    /// Writes `value` to `port`.
    fn write(&mut self, port: u16, value: u8);
}

/// Line-status reads a byte may wait for room before the line is given up, as the kernel's
/// `serial::TX_POLL_LIMIT`.
pub const POLLS: u32 = 100_000;
/// The line-status register's offset, and its "transmitter holding register empty" bit
/// (PC16550D).
const LSR: u16 = 5;
const THRE: u8 = 0x20;

/// A 16550 at an I/O port, written to as the firmware left it programmed: each byte waits for
/// room, at most [`POLLS`] reads, and a byte that never finds room ends the line and every later
/// one. LF goes out as CR LF.
pub struct Uart<'a, P: Port> {
    io: &'a mut P,
    base: u16,
    stopped: bool,
}

impl<P: Port> fmt::Debug for Uart<'_, P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Uart")
            .field("base", &self.base)
            .field("stopped", &self.stopped)
            .finish_non_exhaustive()
    }
}

impl<'a, P: Port> Uart<'a, P> {
    /// The UART whose registers start at `base`.
    pub fn new(io: &'a mut P, base: u16) -> Self {
        Self {
            io,
            base,
            stopped: false,
        }
    }

    /// Whether a byte found no room, so nothing more is sent.
    pub const fn stopped(&self) -> bool {
        self.stopped
    }

    fn byte(&mut self, byte: u8) -> fmt::Result {
        if self.stopped {
            return Err(fmt::Error);
        }
        for _ in 0..POLLS {
            if self.io.read(self.base.wrapping_add(LSR)) & THRE != 0 {
                self.io.write(self.base, byte);
                return Ok(());
            }
            core::hint::spin_loop();
        }
        self.stopped = true;
        Err(fmt::Error)
    }
}

impl<P: Port> fmt::Write for Uart<'_, P> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if b == b'\n' {
                self.byte(b'\r')?;
            }
            self.byte(b)?;
        }
        Ok(())
    }
}

/// Each `ExitBootServices` call's status, as the loader's `exit` saw it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExitCalls {
    /// The first call's status, once it was made.
    pub first: Option<Status>,
    /// The retry's status, once it was made.
    pub retry: Option<Status>,
}

impl ExitCalls {
    /// Records the status of the call just made.
    pub const fn record(&mut self, status: Status) {
        if self.first.is_none() {
            self.first = Some(status);
        } else {
            self.retry = Some(status);
        }
    }

    /// The calls made so far: the index of the next one.
    pub const fn made(&self) -> u8 {
        match (self.first, self.retry) {
            (None, _) => 0,
            (Some(_), None) => 1,
            (Some(_), Some(_)) => 2,
        }
    }

    /// The line the loader writes before it halts, for calls that did not succeed.
    pub const fn halting(self) -> Halting {
        Halting(self)
    }
}

/// The line a loader writes to the UART boot info would name before it halts after a failed
/// `ExitBootServices` (decision item 342): each call's status, or why there was no retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Halting(ExitCalls);

impl fmt::Display for Halting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("origin-boot: ExitBootServices failed (")?;
        match (self.0.first, self.0.retry) {
            (None, _) => f.write_str("no call recorded")?,
            (Some(first), Some(retry)) => write!(f, "first call {first:?}, retry {retry:?}")?,
            (Some(first), None) if first == Status::INVALID_PARAMETER => write!(
                f,
                "first call {first:?}; the memory map could not be read again for the retry"
            )?,
            (Some(first), None) => write!(f, "first call {first:?}, which allows no retry")?,
        }
        f.write_str("): halting")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::fmt::Write as _;

    /// A UART whose line status says "busy" for `busy` reads before each byte, or forever.
    struct Mock {
        busy: Option<u32>,
        waited: u32,
        reads: usize,
        sent: Vec<(u16, u8)>,
    }

    impl Mock {
        fn new(busy: Option<u32>) -> Self {
            Self {
                busy,
                waited: 0,
                reads: 0,
                sent: Vec::new(),
            }
        }
    }

    impl Port for Mock {
        fn read(&mut self, port: u16) -> u8 {
            assert_eq!(port, 0x2FD, "only the line status is read");
            self.reads += 1;
            match self.busy {
                Some(n) if self.waited >= n => {
                    self.waited = 0;
                    THRE
                }
                _ => {
                    self.waited += 1;
                    0
                }
            }
        }

        fn write(&mut self, port: u16, value: u8) {
            self.sent.push((port, value));
        }
    }

    #[test]
    fn a_line_goes_out_with_cr_lf_once_there_is_room() {
        let mut io = Mock::new(Some(3));
        let mut uart = Uart::new(&mut io, 0x2F8);
        writeln!(uart, "ok").unwrap();
        assert!(!uart.stopped());
        let bytes: Vec<u8> = io
            .sent
            .iter()
            .map(|&(port, b)| {
                assert_eq!(port, 0x2F8);
                b
            })
            .collect();
        assert_eq!(bytes, b"ok\r\n");
        assert_eq!(io.reads, 4 * 4);
    }

    #[test]
    fn a_uart_that_never_has_room_costs_one_bounded_wait() {
        let mut io = Mock::new(None);
        let mut uart = Uart::new(&mut io, 0x2F8);
        assert!(writeln!(uart, "a long line that is never sent").is_err());
        assert!(uart.stopped());
        assert!(writeln!(uart, "nor this one").is_err());
        assert!(io.sent.is_empty());
        assert_eq!(io.reads, POLLS as usize);
    }

    #[test]
    fn each_way_exit_boot_services_fails_has_its_line() {
        let stale = Status::INVALID_PARAMETER;
        let device = Status::DEVICE_ERROR;
        let line = |calls: &[Status]| {
            let mut c = ExitCalls::default();
            for &s in calls {
                c.record(s);
            }
            c.halting().to_string()
        };
        assert_eq!(
            line(&[stale, stale]),
            "origin-boot: ExitBootServices failed (first call EFI_INVALID_PARAMETER, retry EFI_INVALID_PARAMETER): halting"
        );
        assert_eq!(
            line(&[stale, device]),
            "origin-boot: ExitBootServices failed (first call EFI_INVALID_PARAMETER, retry EFI_DEVICE_ERROR): halting"
        );
        assert_eq!(
            line(&[stale]),
            "origin-boot: ExitBootServices failed (first call EFI_INVALID_PARAMETER; the memory map could not be read again for the retry): halting"
        );
        assert_eq!(
            line(&[device]),
            "origin-boot: ExitBootServices failed (first call EFI_DEVICE_ERROR, which allows no retry): halting"
        );
        assert_eq!(
            line(&[]),
            "origin-boot: ExitBootServices failed (no call recorded): halting"
        );
    }

    #[test]
    fn the_calls_are_counted() {
        let mut calls = ExitCalls::default();
        assert_eq!(calls.made(), 0);
        calls.record(Status::INVALID_PARAMETER);
        assert_eq!(calls.made(), 1);
        calls.record(Status::SUCCESS);
        assert_eq!(calls.made(), 2);
        assert_eq!(calls.retry, Some(Status::SUCCESS));
    }
}
