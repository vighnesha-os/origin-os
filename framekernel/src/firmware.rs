//! Firmware-provided ACPI tables, read through the direct map (ACPI 6.6 §5.2).
//!
//! The firmware leaves its tables in ACPI-reclaim or ACPI-NVS memory, which the kernel's
//! direct map may not cover; [`bytes`] maps them read-only and never executable first. The
//! table parsers (`framekernel::acpi`) treat every byte as untrusted.

use core::ptr;

use framekernel::acpi;

use crate::vm;

/// Reads `len` bytes of firmware memory at physical `addr`, mapping it read-only first if
/// the kernel does not map it.
pub(crate) fn bytes(addr: u64, len: usize) -> Result<&'static [u8], &'static str> {
    if addr == 0 || addr.checked_add(len as u64).is_none() {
        return Err("bad table address");
    }
    let virt = vm::map_firmware(addr, len as u64).map_err(|_| "cannot map an ACPI table")?;
    // SAFETY: `[addr, addr + len)` is readable at its direct-map address `virt` (just
    // ensured), does not wrap, and holds ACPI tables, which the firmware does not change
    // after boot; `u8` has no alignment or validity requirement.
    Ok(unsafe {
        core::slice::from_raw_parts(ptr::with_exposed_provenance::<u8>(virt as usize), len)
    })
}

/// The first table with signature `sig` that the XSDT lists, checked (signature, length,
/// checksum): RSDP → XSDT → table.
pub(crate) fn acpi_table(rsdp_addr: u64, sig: &[u8; 4]) -> Result<&'static [u8], &'static str> {
    let rsdp =
        acpi::parse_rsdp(bytes(rsdp_addr, acpi::RSDP_LENGTH)?).map_err(|_| "invalid RSDP")?;
    let table = |addr: u64, sig: &[u8; 4]| -> Result<&'static [u8], &'static str> {
        let header = bytes(addr, acpi::HEADER_LENGTH)?;
        let len = acpi::table_length(header).map_err(|_| "invalid table length")?;
        let all = bytes(addr, len as usize)?;
        acpi::check_table(all, sig).map_err(|_| "invalid table")?;
        Ok(all)
    };
    let xsdt = table(rsdp.xsdt, b"XSDT")?;
    for addr in acpi::xsdt_entries(xsdt) {
        let header = bytes(addr, acpi::HEADER_LENGTH)?;
        if acpi::signature(header) == Some(*sig) {
            return table(addr, sig);
        }
    }
    Err("table not listed in the XSDT")
}
