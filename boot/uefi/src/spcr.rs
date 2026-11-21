//! The firmware's serial console, named for the kernel's first line (boot info 0.8; ADR-0010
//! Amendment 3, decision item 317; TASKS item 84).
//!
//! Before `ExitBootServices` the loader follows the RSDP to the XSDT, takes the first table the
//! XSDT lists as `SPCR` whose header checks, and parses it with `origin-spcr`, the kernel's own
//! parser. A 16550 in I/O space is named in boot info's [`EarlyUart`], and the kernel writes to
//! it from its first line. Anything else names none: the kernel still reads the SPCR itself once
//! its page tables are live (ADR-0010 E4).
//!
//! Every byte is read through the caller's `read`, which refuses memory the firmware's map does
//! not describe ([`described`]), and every read is bounded by a length checked first. Nothing
//! here stops the loader: every outcome is one line and, at worst, no early UART (ADR-0010 E5).
//! The RSDP and XSDT checks are the kernel's (`origin_frame::acpi::parse_rsdp`, `check_table`):
//! signature, both RSDP checksums, revision 2 or later, lengths, and a whole-table checksum.

use core::fmt;

use origin_acpi_tables::ErrorKind;
use origin_acpi_tables::header::{HEADER_LEN as HEADER_LENGTH, check_signature};
use origin_boot_info::{
    EARLY_UART_BAUD, EARLY_UART_CLOCK, EarlyUart, EarlyUartError, MemoryRegion, RegionKind,
};
use origin_spcr::{SpcrError, UartAddress};

/// Bytes of an ACPI 2.0 or later RSDP that Origin reads (ACPI 6.6 §5.2.5.3).
pub const RSDP_LENGTH: usize = 36;
/// The longest XSDT walked: 64 KiB, room for 8,187 tables, far more than any firmware lists. The
/// kernel reads up to 16 MiB itself; a longer one names no UART here, and keeps the walk short
/// under boot services (TASKS item 84's review, T84-15).
pub const MAX_XSDT_LENGTH: u32 = 64 << 10;
/// The longest SPCR read. Revision 4 with its namespace string is about a hundred bytes; a
/// table declaring more names no early UART, and the kernel still reads it itself.
pub const MAX_SPCR_LENGTH: usize = 1024;
/// Bytes read at a time while summing the XSDT.
const CHUNK: usize = 64;
const PAGE: u64 = 4096;

/// Why the RSDP is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RsdpError {
    /// Not `"RSD PTR "`.
    Signature,
    /// Its 20-byte or its extended checksum is wrong.
    Checksum,
    /// Below 2: ACPI 1.0, with no XSDT.
    Revision(u8),
    /// Its length is below 36 bytes.
    Length(u32),
}

/// Why the XSDT is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XsdtError {
    /// The RSDP's XSDT address holds a table with another signature.
    Signature([u8; 4]),
    /// Its length is below the header or above [`MAX_XSDT_LENGTH`].
    Length(u32),
    /// Its bytes do not sum to zero.
    Checksum,
}

/// Why there is no SPCR to parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Missing {
    /// The configuration table holds no ACPI 2.0 or later RSDP.
    NoRsdp,
    /// The RSDP's bytes are not in memory the firmware's map describes.
    RsdpUnreadable(u64),
    /// The RSDP is refused.
    Rsdp(RsdpError),
    /// The XSDT's bytes, from this address, are not in memory the map describes.
    XsdtUnreadable(u64),
    /// The XSDT is refused.
    Xsdt(XsdtError),
    /// The XSDT lists no SPCR.
    NotListed,
    /// The XSDT lists no SPCR the loader could read, and the table at this address could not be
    /// read: it might have been one.
    EntryUnreadable(u64),
}

/// Why the SPCR found names no early UART.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Its header does not check (`origin-acpi-tables`' rules), and no later SPCR in the XSDT
    /// does.
    Header(HeaderFault),
    /// It declares more than [`MAX_SPCR_LENGTH`] bytes: no later SPCR is looked at, since the
    /// kernel's first that checks may be this one (T84-8).
    TooLong(u32),
    /// Its bytes, from this address, are not in memory the map describes: no later SPCR is
    /// looked at (T84-8).
    Unreadable(u64),
    /// The parser refuses it, as the kernel's would.
    Parse(SpcrError),
    /// The structure made from it fails boot info's own check (a parser that let through what
    /// boot info refuses: security invariant 1 of TASKS item 84).
    Uart(EarlyUartError),
}

/// Why an SPCR's header does not check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderFault {
    /// Its declared length is below the header's 36 bytes, or not the bytes read.
    Length,
    /// Its bytes do not sum to zero.
    Checksum,
    /// Anything else the header's check refuses.
    Other,
}

/// What the loader found, and says on its console.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    /// A 16550 in I/O space: the kernel writes to it from its first line.
    Port(EarlyUart),
    /// A UART in memory space at this address: the kernel moves to it once its page tables are
    /// live, and names none before that.
    Memory(u64),
    /// No SPCR to parse.
    Missing(Missing),
    /// The SPCR names no early UART.
    Refused(Refused),
}

impl Found {
    /// What boot info carries: the UART for [`Found::Port`], [`EarlyUart::NONE`] for the rest.
    pub const fn early_uart(&self) -> EarlyUart {
        match *self {
            Self::Port(uart) => uart,
            _ => EarlyUart::NONE,
        }
    }
}

/// `true` if `[base, base + len)` lies wholly in regions of `regions` (sorted by base, as
/// `memmap::translate` leaves them), with no gap: memory the firmware's map describes, which
/// UEFI identity-maps while boot services run. Refused: an empty or wrapping range, one in the
/// first page, which firmware may leave unmapped to catch null pointers, and one that touches
/// memory-mapped I/O, where a read can change a device, or unaccepted memory, which faults in a
/// confidential guest (the kernel's walk refuses both; TASKS item 84's review, T84-6).
pub fn described(regions: &[MemoryRegion], base: u64, len: usize) -> bool {
    let Some(end) = u64::try_from(len)
        .ok()
        .and_then(|len| base.checked_add(len))
    else {
        return false;
    };
    if base < PAGE || len == 0 {
        return false;
    }
    let refused = [RegionKind::Mmio as u32, RegionKind::Unaccepted as u32];
    let mut at = base;
    for r in regions {
        let r_end = r.base.saturating_add(r.pages.saturating_mul(PAGE));
        if r_end <= at {
            continue;
        }
        if r.base > at || refused.contains(&r.kind) {
            return false;
        }
        at = r_end;
        if at >= end {
            return true;
        }
    }
    false
}

/// Finds the SPCR from the RSDP at `rsdp` (0: none), reading each range through `read`, which
/// fills its buffer and returns `true`, or returns `false` for memory it will not read.
pub fn find(rsdp: u64, read: &mut impl FnMut(u64, &mut [u8]) -> bool) -> Found {
    match walk(rsdp, read) {
        Ok(found) | Err(found) => found,
    }
}

fn walk(rsdp: u64, read: &mut impl FnMut(u64, &mut [u8]) -> bool) -> Result<Found, Found> {
    let missing = |m| Found::Missing(m);
    if rsdp == 0 {
        return Err(missing(Missing::NoRsdp));
    }
    let mut bytes = [0u8; RSDP_LENGTH];
    if !read(rsdp, &mut bytes) {
        return Err(missing(Missing::RsdpUnreadable(rsdp)));
    }
    let xsdt = parse_rsdp(&bytes).map_err(|e| missing(Missing::Rsdp(e)))?;
    let entries = check_xsdt(xsdt, read).map_err(missing)?;
    let mut unreadable = None;
    let mut refused = None;
    for i in 0..entries {
        // Inside the XSDT, whose bytes were all read above: `check_xsdt` bounds `entries`.
        let at = xsdt.wrapping_add(HEADER_LENGTH as u64 + 8 * u64::from(i));
        let mut entry = [0u8; 8];
        if !read(at, &mut entry) {
            unreadable.get_or_insert(at);
            continue;
        }
        let table = u64::from_le_bytes(entry);
        // A null entry names nothing, as the kernel's walk has it (T84-8).
        if table == 0 {
            continue;
        }
        let mut header = [0u8; HEADER_LENGTH];
        if !read(table, &mut header) {
            unreadable.get_or_insert(table);
            continue;
        }
        if header.get(..4) != Some(&b"SPCR"[..]) {
            continue;
        }
        match spcr_at(table, &header, read) {
            Ok(found) => return Ok(found),
            // A header that does not check: a later SPCR may (the kernel's first that checks).
            Err(why @ Refused::Header(_)) => {
                refused.get_or_insert(why);
            }
            // One the loader cannot check in full may be the kernel's first: none is named
            // (T84-8).
            Err(why) => return Err(Found::Refused(why)),
        }
    }
    Err(match (refused, unreadable) {
        (Some(why), _) => Found::Refused(why),
        (None, Some(at)) => missing(Missing::EntryUnreadable(at)),
        (None, None) => missing(Missing::NotListed),
    })
}

/// The RSDP's checks, as the kernel's `parse_rsdp`: the XSDT's address. The bench loader's walk
/// to the FADT uses it too (TASKS item 116).
pub(crate) fn parse_rsdp(b: &[u8; RSDP_LENGTH]) -> Result<u64, RsdpError> {
    let sums_to_zero = |b: &[u8]| b.iter().fold(0u8, |s, &x| s.wrapping_add(x)) == 0;
    if b.get(..8) != Some(&b"RSD PTR "[..]) {
        return Err(RsdpError::Signature);
    }
    if !b.get(..20).is_some_and(sums_to_zero) {
        return Err(RsdpError::Checksum);
    }
    let revision = b.get(15).copied().unwrap_or(0);
    if revision < 2 {
        return Err(RsdpError::Revision(revision));
    }
    let length = u32_at(b, 20);
    if (length as usize) < RSDP_LENGTH {
        return Err(RsdpError::Length(length));
    }
    // The extended checksum covers `length` bytes; the 36 defined ones are what Origin reads.
    if !sums_to_zero(b) {
        return Err(RsdpError::Checksum);
    }
    Ok(u64_at(b, 24))
}

/// Checks the XSDT at `xsdt` (signature, length, whole-table checksum) and returns how many
/// entries it lists. The bench loader's walk to the FADT uses it too (TASKS item 116).
pub(crate) fn check_xsdt(
    xsdt: u64,
    read: &mut impl FnMut(u64, &mut [u8]) -> bool,
) -> Result<u32, Missing> {
    let mut header = [0u8; HEADER_LENGTH];
    if !read(xsdt, &mut header) {
        return Err(Missing::XsdtUnreadable(xsdt));
    }
    let mut signature = [0u8; 4];
    signature.copy_from_slice(header.get(..4).unwrap_or(&[0; 4]));
    if &signature != b"XSDT" {
        return Err(Missing::Xsdt(XsdtError::Signature(signature)));
    }
    let length = u32_at(&header, 4);
    if (length as usize) < HEADER_LENGTH || length > MAX_XSDT_LENGTH {
        return Err(Missing::Xsdt(XsdtError::Length(length)));
    }
    let mut sum = 0u8;
    let mut chunk = [0u8; CHUNK];
    let mut done = 0u32;
    while done < length {
        let n = (length - done).min(CHUNK as u32) as usize;
        let at = xsdt.wrapping_add(u64::from(done));
        let part = chunk.get_mut(..n).unwrap_or(&mut []);
        if !read(at, part) {
            return Err(Missing::XsdtUnreadable(at));
        }
        sum = part.iter().fold(sum, |s, &x| s.wrapping_add(x));
        done += n as u32;
    }
    if sum != 0 {
        return Err(Missing::Xsdt(XsdtError::Checksum));
    }
    Ok((length - HEADER_LENGTH as u32) / 8)
}

/// The SPCR at `at`, whose 36-byte header is `header`: read whole, checked, parsed.
/// `Err` is an SPCR that names nothing: one whose header does not check, after which the caller
/// may go on to a later SPCR, or one it cannot read in full, after which it may not; once one
/// checks, its parse decides.
fn spcr_at(
    at: u64,
    header: &[u8; HEADER_LENGTH],
    read: &mut impl FnMut(u64, &mut [u8]) -> bool,
) -> Result<Found, Refused> {
    let declared = u32_at(header, 4);
    let len = declared as usize;
    if len > MAX_SPCR_LENGTH {
        return Err(Refused::TooLong(declared));
    }
    let mut bytes = [0u8; MAX_SPCR_LENGTH];
    // The header alone when the declared length is shorter: the check then refuses it.
    let whole = bytes.get_mut(..len.max(HEADER_LENGTH)).unwrap_or(&mut []);
    if !read(at, whole) {
        return Err(Refused::Unreadable(at));
    }
    // The header's rules have one implementation, the kernel's (T84-13).
    let table = check_signature(whole, b"SPCR")
        .map_err(|e| {
            Refused::Header(match e.kind {
                ErrorKind::Length { .. } | ErrorKind::TooShort { .. } => HeaderFault::Length,
                ErrorKind::Checksum => HeaderFault::Checksum,
                _ => HeaderFault::Other,
            })
        })?
        .bytes();
    Ok(match origin_spcr::parse(table) {
        Err(why) => Found::Refused(Refused::Parse(why)),
        Ok(spcr) => match spcr.address {
            UartAddress::Mmio { base, .. } => Found::Memory(base),
            UartAddress::Port(port) => {
                let mut flags = 0;
                if spcr.baud.is_some() {
                    flags |= EARLY_UART_BAUD;
                }
                if spcr.clock_hz.is_some() {
                    flags |= EARLY_UART_CLOCK;
                }
                let uart = EarlyUart {
                    port,
                    interface: spcr.interface,
                    flags,
                    baud: spcr.baud.unwrap_or(0),
                    clock_hz: spcr.clock_hz.unwrap_or(0),
                    reserved: 0,
                };
                match uart.check() {
                    Ok(()) => Found::Port(uart),
                    Err(why) => Found::Refused(Refused::Uart(why)),
                }
            }
        },
    })
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    let mut v = [0u8; 4];
    if let Some(src) = at.checked_add(4).and_then(|end| b.get(at..end)) {
        v.copy_from_slice(src);
    }
    u32::from_le_bytes(v)
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    if let Some(src) = at.checked_add(8).and_then(|end| b.get(at..end)) {
        v.copy_from_slice(src);
    }
    u64::from_le_bytes(v)
}

/// The kernel's console start, said after each outcome.
const STARTS_ON_COM1: &str = "the kernel's console starts on COM1 and the screen";

impl fmt::Display for Found {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Port(uart) => {
                write!(
                    f,
                    "the SPCR names a UART at I/O port {:#x} (interface {:#x}, ",
                    uart.port, uart.interface
                )?;
                match uart.stated_baud() {
                    Some(baud) => write!(f, "{baud} baud")?,
                    None => f.write_str("the speed as the firmware set it")?,
                }
                f.write_str("): the kernel writes to it from its first line")
            }
            Self::Memory(base) => write!(
                f,
                "the SPCR names a UART in memory space at {base:#x}: the kernel moves to it once its page tables are live; {STARTS_ON_COM1}"
            ),
            Self::Missing(why) => write!(f, "no SPCR ({why}); {STARTS_ON_COM1}"),
            Self::Refused(why) => write!(f, "the SPCR was refused ({why}); {STARTS_ON_COM1}"),
        }
    }
}

impl fmt::Display for Missing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const UNMAPPED: &str = "not in memory the firmware's map describes";
        match *self {
            Self::NoRsdp => f.write_str("the configuration table has no ACPI 2.0 RSDP"),
            Self::RsdpUnreadable(at) => write!(f, "the RSDP at {at:#x} is {UNMAPPED}"),
            Self::Rsdp(RsdpError::Signature) => f.write_str("the RSDP's signature is wrong"),
            Self::Rsdp(RsdpError::Checksum) => f.write_str("the RSDP's checksum is wrong"),
            Self::Rsdp(RsdpError::Revision(r)) => {
                write!(f, "the RSDP is revision {r}, with no XSDT")
            }
            Self::Rsdp(RsdpError::Length(l)) => write!(f, "the RSDP's length is {l}"),
            Self::XsdtUnreadable(at) => write!(f, "the XSDT's bytes at {at:#x} are {UNMAPPED}"),
            Self::Xsdt(XsdtError::Signature(s)) => write!(
                f,
                "the RSDP's XSDT address holds a table signed {:?}",
                Signature(s)
            ),
            Self::Xsdt(XsdtError::Length(l)) => write!(f, "the XSDT's length is {l}"),
            Self::Xsdt(XsdtError::Checksum) => f.write_str("the XSDT's checksum is wrong"),
            Self::NotListed => f.write_str("the XSDT lists none"),
            Self::EntryUnreadable(at) => {
                write!(
                    f,
                    "the XSDT lists none readable; the table at {at:#x} is {UNMAPPED}"
                )
            }
        }
    }
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Header(HeaderFault::Length) => {
                f.write_str("its declared length is not a whole table")
            }
            Self::Header(HeaderFault::Checksum) => f.write_str("its checksum is wrong"),
            Self::Header(HeaderFault::Other) => f.write_str("its header does not check"),
            Self::TooLong(n) => write!(
                f,
                "it declares {n} bytes, more than the loader reads ({MAX_SPCR_LENGTH})"
            ),
            Self::Unreadable(at) => write!(
                f,
                "its bytes at {at:#x} are not in memory the firmware's map describes"
            ),
            Self::Parse(why) => write!(f, "{why:?}"),
            Self::Uart(why) => write!(f, "boot info's check: {why:?}"),
        }
    }
}

/// A table signature, printed as text where it is ASCII.
struct Signature([u8; 4]);

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = core::str::from_utf8(&self.0).ok();
        match text.filter(|t| t.bytes().all(|b| b.is_ascii_graphic())) {
            Some(text) => write!(f, "\"{text}\""),
            None => write!(f, "{:02x?}", self.0),
        }
    }
}

#[cfg(test)]
mod tests;
