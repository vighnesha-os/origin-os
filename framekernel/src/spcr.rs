//! The ACPI Serial Port Console Redirection table (ADR-0010): where the firmware
//! says its serial console is, and how it is set.
//!
//! References: Microsoft, "Serial Port Console Redirection Table (SPCR)", revision history
//! through 1.10 (May 2023), table revision 4; Microsoft, "Debug Port Table 2 (DBG2)", Table 3
//! (the serial port subtypes of SPCR revision 2 and later) and its notes on the Generic
//! Address Structure of 16550-based UARTs, April 2023.
//!
//! Firmware tables are untrusted input. Like the other ACPI parsers this reads a byte slice
//! the caller checked with `acpi::check_table`, looks at every field before using it, needs
//! no `unsafe` and never panics. It refuses anything this kernel cannot drive or that would
//! make it touch the wrong thing: an address space or access it does not implement, an
//! unaligned register, and I/O ports below `0x1000` other than the four standard COM blocks
//! (see [`parse`]). A table that is refused leaves the console where it was (ADR-0010).

use crate::acpi;
use crate::serial::{self, LineSetup};

/// Bytes of the table that every revision defines.
const MINIMUM_LENGTH: usize = 80;

/// Offset of the Interface Type byte.
const INTERFACE_TYPE: usize = 36;
/// Offset of the Generic Address Structure.
const BASE_ADDRESS: usize = 40;
/// Offset of the Configured Baud Rate byte.
const CONFIGURED_BAUD_RATE: usize = 58;
/// Offset of the UART Clock Frequency field (revision 3 and later).
const UART_CLOCK_FREQUENCY: usize = 76;
/// Offset of the Precise Baud Rate field (revision 4).
const PRECISE_BAUD_RATE: usize = 80;

/// Generic Address Structure space ID: system memory.
const SPACE_MEMORY: u8 = 0;
/// Generic Address Structure space ID: system I/O.
const SPACE_IO: u8 = 1;

/// Why a table cannot be used. None of these stops a boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpcrError {
    /// The table is too short to hold the fields every revision defines.
    Length(usize),
    /// Revision 0, which no specification defines.
    Revision(u8),
    /// The firmware set the base address to 0: console redirection is disabled.
    RedirectionDisabled,
    /// An interface this kernel cannot drive (not 16550-compatible).
    Interface(u8),
    /// The register block is not in I/O or memory space.
    AddressSpace(u8),
    /// The Generic Address Structure's width, offset or access size makes no sense for a
    /// 16550 (see [`parse`]).
    RegisterLayout,
    /// The address is not one a UART can have (see [`parse`]).
    Address(u64),
}

/// How wide each register access is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// 8-bit accesses.
    Byte,
    /// 16-bit accesses.
    Word,
    /// 32-bit accesses.
    Dword,
}

impl Access {
    /// The access width in bytes.
    pub const fn bytes(self) -> u8 {
        match self {
            Self::Byte => 1,
            Self::Word => 2,
            Self::Dword => 4,
        }
    }
}

/// Where a UART's registers are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UartAddress {
    /// In I/O space, one byte apart.
    Port(u16),
    /// In memory: `stride` bytes between registers, each read and written `access` wide.
    Mmio {
        /// Physical address of the first register.
        base: u64,
        /// Bytes between registers: 1, 2, 4 or 8.
        stride: u8,
        /// The width of each access, at most `stride`.
        access: Access,
    },
}

impl UartAddress {
    /// Bytes of address space the eight registers occupy.
    pub const fn span(&self) -> u64 {
        match *self {
            Self::Port(_) => 8,
            Self::Mmio { stride, .. } => 8 * stride as u64,
        }
    }
}

/// What the table says about the firmware's serial console.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spcr {
    /// The table's revision.
    pub revision: u8,
    /// The Interface Type byte, for reports.
    pub interface: u8,
    /// Where the UART is.
    pub address: UartAddress,
    /// The firmware's baud rate, or `None` for "leave the UART as it is".
    pub baud: Option<u32>,
    /// The UART's input clock in Hz, when the table gives one (revision 3 and later).
    pub clock_hz: Option<u32>,
}

impl Spcr {
    /// How to set the UART's speed, given what its divisor latch holds now:
    ///
    /// 1. If the table states a rate and the UART's clock, program that rate.
    /// 2. Otherwise, if the latch has been programmed (not zero), leave the speed alone: the
    ///    rate in the table is "the baud rate the BIOS used", so a programmed latch already
    ///    holds it, whatever the clock behind it.
    /// 3. A latch of zero was never programmed, so "the rate the BIOS used" is not in it. For a
    ///    UART in I/O space, program the rate on the PC-compatible clock of 1.8432 MHz. In
    ///    memory space the clock could be anything, so leave it.
    pub fn line_setup(&self, latch: u16) -> LineSetup {
        let divisor = |clock| serial::divisor_for(self.baud?, clock);
        let setup = match (self.clock_hz, self.address) {
            (Some(hz), _) => divisor(hz),
            (None, _) if latch != 0 => None,
            (None, UartAddress::Port(_)) => divisor(serial::INPUT_CLOCK_HZ),
            (None, UartAddress::Mmio { .. }) => None,
        };
        setup.map_or(LineSetup::Keep, LineSetup::Divisor)
    }
}

/// `true` if the block of I/O ports `[port, port + 8)` is one a UART may have: one of the four
/// standard COM blocks, or above `0xFFF`, where PCI I/O BARs live. Everything else below
/// `0x1000` is system space the kernel's output must never reach, the legacy devices and the
/// PCI configuration and reset ports, and the ACPI power-management block (typically
/// `0x400`..`0xFFF`), where a few bytes of a corrupt table's "UART" can power the machine off.
fn port_block_allowed(port: u16) -> bool {
    let end = u32::from(port) + 8;
    matches!(port, 0x3F8 | 0x2F8 | 0x3E8 | 0x2E8) || (port >= 0x1000 && end <= 0x1_0000)
}

/// Parses a checked SPCR (`acpi::check_table(table, b"SPCR")` passed).
///
/// Accepted: interface types 0 and 1 in revision 1 (a 16550, and a 16450 that also accepts
/// writes to the FIFO register), and the 16550-compatible serial subtypes of the DBG2
/// specification in later revisions (0x0000, 0x0001, 0x0012 and 0x0014, Intel LPSS).
///
/// The Generic Address Structure must have a register bit offset of 0.
/// - In I/O space the register width must be 8 bits (or 0, as many tables leave it) and the
///   access 8 bits or undefined.
/// - In memory, the register width is the stride between registers and must be 8, 16, 32 or
///   64 bits. The access size is byte, word or dword and at most the stride; undefined means
///   the stride itself, up to dword, because a wide stride tells of a device that wants wide
///   accesses. QWORD accesses are refused: no 16550 needs them. The address must be aligned to
///   the access and the registers must end below 2^52.
pub fn parse(table: &[u8]) -> Result<Spcr, SpcrError> {
    let byte = |offset: usize| table.get(offset).copied().unwrap_or(0);
    let dword = |offset: usize| -> u32 {
        let mut v = [0u8; 4];
        if let Some(bytes) = table.get(offset..offset + 4) {
            v.copy_from_slice(bytes);
        }
        u32::from_le_bytes(v)
    };
    if table.len() < MINIMUM_LENGTH {
        return Err(SpcrError::Length(table.len()));
    }
    let revision = byte(acpi::HEADER_REVISION);
    if revision == 0 {
        return Err(SpcrError::Revision(0));
    }
    let interface = byte(INTERFACE_TYPE);
    let compatible = if revision == 1 {
        matches!(interface, 0 | 1)
    } else {
        matches!(interface, 0x00 | 0x01 | 0x12 | 0x14)
    };
    if !compatible {
        return Err(SpcrError::Interface(interface));
    }

    let (space, bit_width, bit_offset, access_size) = (
        byte(BASE_ADDRESS),
        byte(BASE_ADDRESS + 1),
        byte(BASE_ADDRESS + 2),
        byte(BASE_ADDRESS + 3),
    );
    let address = u64::from(dword(BASE_ADDRESS + 4)) | u64::from(dword(BASE_ADDRESS + 8)) << 32;
    if address == 0 {
        return Err(SpcrError::RedirectionDisabled);
    }
    if bit_offset != 0 {
        return Err(SpcrError::RegisterLayout);
    }
    let address = match space {
        SPACE_IO => {
            if !matches!(bit_width, 0 | 8) || !matches!(access_size, 0 | 1) {
                return Err(SpcrError::RegisterLayout);
            }
            match u16::try_from(address) {
                Ok(port) if port_block_allowed(port) => UartAddress::Port(port),
                _ => return Err(SpcrError::Address(address)),
            }
        }
        SPACE_MEMORY => {
            if !matches!(bit_width, 8 | 16 | 32 | 64) {
                return Err(SpcrError::RegisterLayout);
            }
            let stride = bit_width / 8;
            let access = match access_size {
                1 => Access::Byte,
                2 => Access::Word,
                3 => Access::Dword,
                0 if stride == 1 => Access::Byte,
                0 if stride == 2 => Access::Word,
                0 => Access::Dword,
                _ => return Err(SpcrError::RegisterLayout),
            };
            if access.bytes() > stride {
                return Err(SpcrError::RegisterLayout);
            }
            let end = address.checked_add(8 * u64::from(stride));
            if !address.is_multiple_of(u64::from(access.bytes())) || end.is_none_or(|e| e > 1 << 52)
            {
                return Err(SpcrError::Address(address));
            }
            UartAddress::Mmio {
                base: address,
                stride,
                access,
            }
        }
        other => return Err(SpcrError::AddressSpace(other)),
    };

    // A non-zero precise rate wins; a reserved configured value means "leave it as it is".
    let precise = (revision >= 4 && table.len() >= PRECISE_BAUD_RATE + 4)
        .then(|| dword(PRECISE_BAUD_RATE))
        .filter(|&baud| baud != 0);
    let baud = precise.or(match byte(CONFIGURED_BAUD_RATE) {
        3 => Some(9_600),
        4 => Some(19_200),
        6 => Some(57_600),
        7 => Some(115_200),
        _ => None,
    });
    let clock_hz = (revision >= 3)
        .then(|| dword(UART_CLOCK_FREQUENCY))
        .filter(|&hz| hz != 0);

    Ok(Spcr {
        revision,
        interface,
        address,
        baud,
        clock_hz,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{self, Rng};

    type Mutate = fn(&mut Vec<u8>);

    /// A valid revision 2 table for COM2 at 115 200 baud, checksum sealed by the caller.
    fn table() -> Vec<u8> {
        let mut t = vec![0u8; 80];
        t[..4].copy_from_slice(b"SPCR");
        t[4..8].copy_from_slice(&80u32.to_le_bytes());
        t[8] = 2;
        t[10..16].copy_from_slice(b"ORIGIN");
        t[16..24].copy_from_slice(b"ORIGINSP");
        t[36] = 0;
        t[40..52].copy_from_slice(&[1, 8, 0, 1, 0xF8, 0x02, 0, 0, 0, 0, 0, 0]);
        t[58] = 7;
        t[59] = 0;
        t[60] = 1;
        t[64..68].copy_from_slice(&[0xFF; 4]);
        t
    }

    fn seal(t: &mut [u8]) {
        t[9] = 0;
        let sum = t.iter().fold(0u8, |s, &b| s.wrapping_add(b));
        t[9] = 0u8.wrapping_sub(sum);
    }

    fn sealed(mut t: Vec<u8>) -> Vec<u8> {
        seal(&mut t);
        assert_eq!(acpi::check_table(&t, b"SPCR"), Ok(()));
        t
    }

    #[test]
    fn com2_at_115200_parses_exactly() {
        let parsed = parse(&sealed(table())).unwrap();
        assert_eq!(
            parsed,
            Spcr {
                revision: 2,
                interface: 0,
                address: UartAddress::Port(0x2F8),
                baud: Some(115_200),
                clock_hz: None,
            }
        );
        // The latch was programmed, by the firmware: leave it.
        assert_eq!(parsed.line_setup(12), LineSetup::Keep);
        // Never programmed: the PC clock divides 115 200 exactly.
        assert_eq!(parsed.line_setup(0), LineSetup::Divisor(1));
        assert_eq!(parsed.address.span(), 8);
    }

    /// The memory image of COM1's base address in Microsoft's example, and a stride of 4.
    #[test]
    fn the_specification_examples_decode() {
        let mut t = table();
        t[40..52].copy_from_slice(&[1, 8, 0, 0, 0xF8, 0x03, 0, 0, 0, 0, 0, 0]);
        let parsed = parse(&sealed(t)).unwrap();
        assert_eq!(parsed.address, UartAddress::Port(0x3F8));
    }

    #[test]
    fn an_mmio_uart_has_its_stride_and_access_width() {
        // DBG2 subtype 0x12 at a 32-bit, 32-bit-spaced register block: the shape of an
        // Intel LPSS or AMD FCH UART on the machines in ADR-0009.
        let mut t = table();
        t[8] = 4;
        t[36] = 0x12;
        t[40] = 0;
        t[41] = 32;
        t[43] = 3;
        t[44..52].copy_from_slice(&0xFEDC_9000u64.to_le_bytes());
        let parsed = parse(&sealed(t.clone())).unwrap();
        assert_eq!(
            parsed.address,
            UartAddress::Mmio {
                base: 0xFEDC_9000,
                stride: 4,
                access: Access::Dword
            }
        );
        assert_eq!(parsed.address.span(), 32);
        // Interface 0x14 (Intel LPSS) is accepted with a stride from the same structure.
        t[36] = 0x14;
        assert_eq!(parse(&sealed(t.clone())).unwrap().interface, 0x14);
        // An undefined access size follows the stride, up to a dword.
        t[43] = 0;
        assert_eq!(
            parse(&sealed(t.clone())).unwrap().address,
            UartAddress::Mmio {
                base: 0xFEDC_9000,
                stride: 4,
                access: Access::Dword
            }
        );
        // A byte access is allowed at a wider stride; 8-bit registers one byte apart too.
        t[43] = 1;
        assert_eq!(
            parse(&sealed(t.clone())).unwrap().address,
            UartAddress::Mmio {
                base: 0xFEDC_9000,
                stride: 4,
                access: Access::Byte
            }
        );
        t[41] = 8;
        t[43] = 0;
        assert_eq!(
            parse(&sealed(t)).unwrap().address,
            UartAddress::Mmio {
                base: 0xFEDC_9000,
                stride: 1,
                access: Access::Byte
            }
        );
    }

    #[test]
    fn baud_rates_and_clocks_become_divisors_or_are_left_alone() {
        let with = |baud: u8, rev: u8, clock: u32, mmio: bool| {
            let mut t = table();
            t[8] = rev;
            t[58] = baud;
            t[76..80].copy_from_slice(&clock.to_le_bytes());
            if mmio {
                t[36] = 0x12;
                t[40..52].copy_from_slice(&[0, 32, 0, 3, 0, 0x90, 0xDC, 0xFE, 0, 0, 0, 0]);
            }
            parse(&sealed(t)).unwrap()
        };
        // Every rate the table can name, on a PC clock, when the latch was never programmed.
        for (code, rate, divisor) in [
            (3u8, 9_600, 12),
            (4, 19_200, 6),
            (6, 57_600, 2),
            (7, 115_200, 1),
        ] {
            let s = with(code, 2, 0, false);
            assert_eq!(s.baud, Some(rate));
            assert_eq!(s.line_setup(0), LineSetup::Divisor(divisor), "{rate}");
            // A programmed latch is "the rate the BIOS used" already.
            assert_eq!(s.line_setup(7), LineSetup::Keep, "{rate}");
        }
        // "As is": nothing to program, even with an unprogrammed latch.
        assert_eq!(with(0, 2, 0, false).line_setup(0), LineSetup::Keep);
        // A reserved code is treated the same way, never as a rate.
        assert_eq!(with(1, 2, 0, false).baud, None);
        assert_eq!(with(5, 2, 0, false).line_setup(0), LineSetup::Keep);
        assert_eq!(with(8, 2, 0, false).baud, None);
        // A rate on a UART in memory space with no stated clock cannot be turned into a
        // divisor, programmed latch or not.
        assert_eq!(with(7, 2, 0, true).line_setup(0), LineSetup::Keep);
        assert_eq!(with(7, 2, 0, true).line_setup(5), LineSetup::Keep);
        // A stated clock works for either space, whatever the latch holds; below revision 3
        // the field is not read.
        assert_eq!(
            with(7, 3, 14_745_600, true).line_setup(5),
            LineSetup::Divisor(8)
        );
        assert_eq!(with(7, 2, 14_745_600, true).clock_hz, None);
        // A clock the rate does not divide to within 2 % is left alone.
        assert_eq!(with(7, 3, 1_000_000, false).line_setup(0), LineSetup::Keep);
    }

    #[test]
    fn a_precise_rate_wins_in_revision_4_and_is_ignored_before() {
        let mut t = table();
        t.resize(92, 0);
        t[4..8].copy_from_slice(&92u32.to_le_bytes());
        t[8] = 4;
        t[58] = 0;
        t[76..80].copy_from_slice(&48_000_000u32.to_le_bytes());
        t[80..84].copy_from_slice(&1_500_000u32.to_le_bytes());
        let parsed = parse(&sealed(t.clone())).unwrap();
        assert_eq!(parsed.baud, Some(1_500_000));
        assert_eq!(parsed.line_setup(1), LineSetup::Divisor(2));
        t[8] = 3;
        let parsed = parse(&sealed(t)).unwrap();
        assert_eq!(parsed.baud, None, "revision 3 has no precise rate field");
    }

    #[test]
    fn what_the_kernel_cannot_drive_is_refused_by_name() {
        let mut t = table();
        t[40] = 0;
        t[41] = 8;
        t[44..52].fill(0);
        assert_eq!(parse(&t), Err(SpcrError::RedirectionDisabled));

        let cases: [(Mutate, SpcrError); 18] = [
            (|t| t.truncate(79), SpcrError::Length(79)),
            (|t| t[8] = 0, SpcrError::Revision(0)),
            (|t| t[36] = 3, SpcrError::Interface(3)), // PL011
            (|t| t[36] = 0x0E, SpcrError::Interface(0x0E)), // SBSA
            (|t| t[36] = 0xFF, SpcrError::Interface(0xFF)),
            (
                |t| {
                    t[8] = 1;
                    t[36] = 2;
                },
                SpcrError::Interface(2),
            ),
            (|t| t[40] = 2, SpcrError::AddressSpace(2)), // PCI configuration space
            (|t| t[40] = 0x7F, SpcrError::AddressSpace(0x7F)),
            (|t| t[42] = 1, SpcrError::RegisterLayout), // bit offset
            (|t| t[41] = 16, SpcrError::RegisterLayout), // an I/O register is 8 bits
            (|t| t[43] = 2, SpcrError::RegisterLayout), // and accessed 8 bits wide
            // The system's own ports, the PCI configuration ports and the ACPI power
            // management block are never a UART.
            (
                |t| t[44..46].copy_from_slice(&0x60u16.to_le_bytes()),
                SpcrError::Address(0x60),
            ),
            (
                |t| t[44..46].copy_from_slice(&0xCF8u16.to_le_bytes()),
                SpcrError::Address(0xCF8),
            ),
            (
                |t| t[44..46].copy_from_slice(&0xCF4u16.to_le_bytes()),
                SpcrError::Address(0xCF4),
            ),
            // Ports run off the end of the 16-bit space.
            (
                |t| t[44..46].copy_from_slice(&0xFFF9u16.to_le_bytes()),
                SpcrError::Address(0xFFF9),
            ),
            (
                |t| t[44..52].copy_from_slice(&0x1_0000u64.to_le_bytes()),
                SpcrError::Address(0x1_0000),
            ),
            (
                |t| t[40..44].copy_from_slice(&[0, 0, 0, 3]),
                SpcrError::RegisterLayout,
            ), // memory: width 0
            (
                |t| t[40..44].copy_from_slice(&[0, 24, 0, 1]),
                SpcrError::RegisterLayout,
            ), // memory: width 24
        ];
        for (mutate, expected) in cases {
            let mut t = table();
            mutate(&mut t);
            assert_eq!(parse(&t), Err(expected));
        }
    }

    #[test]
    fn a_memory_uart_with_a_bad_layout_is_refused() {
        let good = || {
            let mut t = table();
            t[36] = 0x12;
            t[40..44].copy_from_slice(&[0, 32, 0, 3]);
            t[44..52].copy_from_slice(&0xFEDC_9000u64.to_le_bytes());
            t
        };
        assert!(parse(&good()).is_ok());
        let cases: [(Mutate, SpcrError); 6] = [
            (|t| t[43] = 4, SpcrError::RegisterLayout), // QWORD
            (|t| t[43] = 5, SpcrError::RegisterLayout), // not a size at all
            (
                |t| {
                    t[41] = 16;
                    t[43] = 3;
                },
                SpcrError::RegisterLayout, // a dword access wider than the stride
            ),
            (
                |t| t[44..52].copy_from_slice(&0xFEDC_9002u64.to_le_bytes()),
                SpcrError::Address(0xFEDC_9002), // unaligned for its access
            ),
            (
                |t| t[44..52].copy_from_slice(&((1u64 << 52) - 0x10).to_le_bytes()),
                SpcrError::Address((1 << 52) - 0x10), // past 2^52 with its registers
            ),
            (
                |t| t[44..52].copy_from_slice(&u64::MAX.to_le_bytes()),
                SpcrError::Address(u64::MAX), // wraps (and is unaligned)
            ),
        ];
        for (mutate, expected) in cases {
            let mut t = good();
            mutate(&mut t);
            assert_eq!(parse(&t), Err(expected));
        }
    }

    /// CLAUDE.md §13, §72: a table the firmware wrote is untrusted input. Every mutation is
    /// sealed again so the fields behind the checksum are reached. Nothing panics, and anything
    /// accepted keeps the promises the kernel relies on.
    #[test]
    fn hostile_tables_keep_every_promise() {
        let mut rng = Rng::new(0x5350_4352);
        let (mut accepted, mut refused) = (0u32, 0u32);
        for case in 0..testing::cases(200_000) {
            let mut t = if rng.below(5) == 0 {
                let mut t = table();
                t.extend((0..rng.below(64)).map(|_| rng.byte()));
                let len = t.len() as u32;
                t[4..8].copy_from_slice(&len.to_le_bytes());
                t[8] = 1 + rng.below(6) as u8;
                t
            } else {
                table()
            };
            testing::mutate(&mut t, &mut rng);
            if rng.below(2) == 0 {
                // Bias the mutation towards the fields that decide everything.
                let end = t.len().min(84);
                if end > 36 {
                    let mut window = t[36..end].to_vec();
                    testing::mutate(&mut window, &mut rng);
                    window.resize(end - 36, 0);
                    t[36..end].copy_from_slice(&window);
                }
            }
            if t.len() >= 12 {
                let len = t.len() as u32;
                t[4..8].copy_from_slice(&len.to_le_bytes());
                seal(&mut t);
            }
            let _ = acpi::check_table(&t, b"SPCR");
            let Ok(s) = parse(&t) else {
                refused += 1;
                continue;
            };
            accepted += 1;
            assert!(t.len() >= MINIMUM_LENGTH, "case {case}");
            match s.address {
                UartAddress::Port(p) => {
                    // A standard COM block, or a whole block above 0xFFF.
                    assert!(
                        matches!(p, 0x3F8 | 0x2F8 | 0x3E8 | 0x2E8)
                            || (p >= 0x1000 && u32::from(p) + 8 <= 0x1_0000),
                        "case {case}: {s:?}"
                    );
                }
                UartAddress::Mmio {
                    base,
                    stride,
                    access,
                } => {
                    assert!(matches!(stride, 1 | 2 | 4 | 8), "case {case}: {s:?}");
                    assert!(access.bytes() <= stride, "case {case}: {s:?}");
                    assert!(
                        base.is_multiple_of(u64::from(access.bytes())),
                        "case {case}"
                    );
                    assert!(base + s.address.span() <= 1 << 52, "case {case}: {s:?}");
                    assert_ne!(base, 0, "case {case}");
                }
            }
            // A divisor comes only from a stated rate and a known clock, and a clock must
            // be stated for one to replace a latch the firmware programmed.
            for latch in [0u16, 1, 0x1234] {
                match s.line_setup(latch) {
                    LineSetup::Keep => {}
                    LineSetup::Divisor(d) => {
                        assert!(d != 0 && s.baud.is_some(), "case {case}: {s:?}");
                        if latch != 0 || matches!(s.address, UartAddress::Mmio { .. }) {
                            assert!(s.clock_hz.is_some(), "case {case}: {s:?}");
                        }
                    }
                }
            }
        }
        assert!(
            accepted > 2000 && refused > 2000,
            "{accepted} accepted, {refused} refused"
        );
    }
}
