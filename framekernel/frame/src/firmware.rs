//! Firmware-provided ACPI tables, read through the direct map (ACPI 6.6 §5.2).
//!
//! The firmware leaves its tables in ACPI-reclaim or ACPI-NVS memory, which the kernel's
//! direct map may not cover; [`bytes`] maps them read-only and never executable first. The
//! table parsers (`framekernel::acpi`) treat every byte as untrusted.
#![forbid(unsafe_code)]

use crate::acpi;
use crate::addr::PhysAddr;

use crate::vm;

/// Reads `len` bytes of firmware memory at physical `addr`, mapping it read-only first if
/// the kernel does not map it. Refused unless the memory map says the range is the
/// firmware's own (ACPI or reserved memory): the kernel never reads its own RAM this way.
///
/// Lent for the kernel's life, so only for what the firmware does not change after boot: the
/// RSDP and the system description tables with a header that §5.2 lays out (the RSDT, XSDT,
/// DSDT, SSDTs and every table they list). Never for the FACS, whose global lock firmware in SMM
/// takes and releases (ACPI 6.6 §5.2.10.1), the FPDT's FBPT and S3PT, which the firmware
/// rewrites on resume (§5.2.24.8, §5.2.24.10), or a channel's shared memory: [`copy`] reads
/// those (the independent review of TASKS item 62, F2).
///
/// The rule is enforced, not only written (the review's addendum, N3): before lending, the first
/// four bytes are copied and compared with `origin_acpi_tables::CHANGES_AFTER_BOOT`, and a range
/// that starts with one of those signatures is refused, wherever it was named from. Its reader
/// copies it instead.
pub fn bytes(addr: u64, len: usize) -> Result<&'static [u8], &'static str> {
    if addr == 0 || addr.checked_add(len as u64).is_none() {
        return Err("bad table address");
    }
    if let Some(mut signature) = (len >= 4).then_some([0u8; 4]) {
        copy(addr, &mut signature)?;
        if origin_acpi_tables::CHANGES_AFTER_BOOT.contains(&&signature) {
            return Err("the firmware changes it after boot: copy it, never lend it");
        }
    }
    vm::firmware_bytes(PhysAddr::new(addr), len as u64).map_err(|_| "cannot map an ACPI table")
}

/// Copies `out.len()` bytes of firmware memory at physical `addr` into `out`, with volatile
/// reads, mapping it read-only first if the kernel does not map it, and lends nothing: for the
/// firmware's structures that change after boot, which no reference may cover (see [`bytes`]).
/// Refused as [`bytes`] is.
pub fn copy(addr: u64, out: &mut [u8]) -> Result<(), &'static str> {
    if addr == 0 || addr.checked_add(out.len() as u64).is_none() {
        return Err("bad table address");
    }
    vm::read_firmware(PhysAddr::new(addr), out).map_err(|_| "cannot map an ACPI table")
}

/// The first valid table with signature `sig` that the XSDT lists, checked (signature, length,
/// checksum): RSDP → XSDT → table ([`acpi::find_table`]). Each entry's header is compared by copy
/// and only the table asked for is lent, so an XSDT that names the FACS or a performance table,
/// which [`bytes`] never lends, still lets an absent table read as [`acpi::NOT_LISTED`] (the
/// review's Addendum 2, A2-L1). An entry that could not be read might have been this table, so a
/// search that finds nothing then says so. A null entry names nothing.
pub fn acpi_table(rsdp_addr: u64, sig: &[u8; 4]) -> Result<&'static [u8], &'static str> {
    let rsdp =
        acpi::parse_rsdp(bytes(rsdp_addr, acpi::RSDP_LENGTH)?).map_err(|_| "invalid RSDP")?;
    let mut copy_header = |addr: u64, out: &mut [u8]| copy(addr, out);
    let mut lend = |addr: u64, len: usize| bytes(addr, len);
    let xsdt = acpi::lend_table(rsdp.xsdt, b"XSDT", &mut copy_header, &mut lend)?;
    acpi::find_table(xsdt, sig, &mut copy_header, &mut lend)
}
