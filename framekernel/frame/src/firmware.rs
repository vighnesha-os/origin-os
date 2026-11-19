//! Firmware-provided ACPI tables, read through the direct map (ACPI 6.6 §5.2).
//!
//! The firmware leaves its tables in ACPI-reclaim or ACPI-NVS memory, which the kernel's
//! direct map may not cover; [`bytes`] maps them read-only and never executable first. The
//! table parsers (`framekernel::acpi`) treat every byte as untrusted.
#![forbid(unsafe_code)]

use crate::acpi;
use crate::addr::PhysAddr;
use crate::once::SetOnce;

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
/// The rule is enforced, not only written. Nothing is lent before [`acpi_table`]'s first call
/// has located those structures and recorded them as never lent (TASKS item 71), and a range that
/// overlaps one is refused, wherever it was named from, the middle of the FACS included. Before
/// lending, the first four bytes are also copied and compared with
/// `origin_acpi_tables::CHANGES_AFTER_BOOT`, so a structure the tables did not lead to is refused
/// by its signature (the review's addendum, N3). Its reader copies it instead.
pub fn bytes(addr: u64, len: usize) -> Result<&'static [u8], &'static str> {
    let Some(end) = addr.checked_add(len as u64).filter(|_| addr != 0) else {
        return Err("bad table address");
    };
    if vm::names_mmio(addr, end) {
        return Err(MMIO);
    }
    guard(addr, end, len)?;
    vm::firmware_bytes(PhysAddr::new(addr), len as u64).map_err(|_| "cannot map an ACPI table")
}

/// [`bytes`]'s own check of the guard over the firmware's structures that change after boot,
/// before the lending registry's: nothing before they are located, no range that overlaps one,
/// and nothing whose first four bytes are such a structure's signature (N3).
fn guard(addr: u64, end: u64, len: usize) -> Result<(), &'static str> {
    if !vm::changing_located() {
        return Err("nothing is lent before the firmware's changing structures are located");
    }
    if vm::never_lent(addr, end) {
        return Err(
            "it overlaps a structure the firmware changes after boot: copy it, never lend it",
        );
    }
    if let Some(mut signature) = (len >= 4).then_some([0u8; 4]) {
        copy(addr, &mut signature)?;
        if origin_acpi_tables::CHANGES_AFTER_BOOT.contains(&&signature) {
            return Err("the firmware changes it after boot: copy it, never lend it");
        }
    }
    Ok(())
}

/// Whether [`bytes`] refuses to lend `len` bytes at physical `addr` for its guard over the
/// firmware's structures that change after boot, lending nothing: its own check and the lending
/// registry's ([`vm::lend_guarded`]), the very two a lend goes through. For the ACPI census's
/// check of the guard at every boot, which must not make the lend it tests (the review of TASKS
/// item 71, T71-7). A range that wraps or starts at zero counts as refused, as [`bytes`] refuses
/// it.
pub fn guards(addr: u64, len: usize) -> bool {
    let Some(end) = addr.checked_add(len as u64).filter(|_| addr != 0) else {
        return true;
    };
    guard(addr, end, len).is_err() || vm::lend_guarded(addr, end)
}

/// Copies `out.len()` bytes of firmware memory at physical `addr` into `out`, with volatile
/// reads, mapping it read-only first if the kernel does not map it, and lends nothing: for the
/// firmware's structures that change after boot, which no reference may cover (see [`bytes`]).
/// Refused, as [`bytes`] is, for memory the memory map does not call the firmware's and for a
/// range it calls MMIO; never for what [`bytes`] will not lend, since this lends nothing.
pub fn copy(addr: u64, out: &mut [u8]) -> Result<(), &'static str> {
    let Some(end) = addr.checked_add(out.len() as u64).filter(|_| addr != 0) else {
        return Err("bad table address");
    };
    if vm::names_mmio(addr, end) {
        return Err(MMIO);
    }
    vm::read_firmware(PhysAddr::new(addr), out).map_err(|_| "cannot map an ACPI table")
}

/// Why [`bytes`] and [`copy`] refuse a range the memory map calls MMIO: device registers hold no
/// firmware table, and reading them could have effects (TASKS item 71, the review's F14).
const MMIO: &str = "the memory map calls it MMIO, which holds no table";

/// What [`acpi_table`]'s first call located as the firmware's structures that change after boot,
/// and what the lending registry did with them: for the ACPI census to report.
static CHANGING: SetOnce<(acpi::ChangingRanges, crate::reserved::Never)> = SetOnce::new();

/// What the first table lookup located and recorded as never lent, once it has run.
pub fn changing() -> Option<&'static (acpi::ChangingRanges, crate::reserved::Never)> {
    CHANGING.get()
}

/// The first valid table with signature `sig` that the XSDT lists, checked (signature, length,
/// checksum): RSDP → XSDT → table ([`acpi::lookup`]). On the first call, before anything is lent,
/// the firmware's structures that change after boot are located by copy alone
/// ([`acpi::changing_ranges`]: no lend, no lookup, no allocation) and recorded as never lent
/// ([`vm::never_lend`]), which also lets lending begin; a later call finds them recorded. Each
/// entry's header is compared by copy and only the table asked for is lent, so an XSDT that names
/// the FACS or a performance table still lets an absent table read as [`acpi::NOT_LISTED`] (the
/// review's Addendum 2, A2-L1). An entry that could not be read might have been this table, so a
/// search that finds nothing then says so. A null entry names nothing.
pub fn acpi_table(rsdp_addr: u64, sig: &[u8; 4]) -> Result<&'static [u8], &'static str> {
    let mut copy_header = |addr: u64, out: &mut [u8]| copy(addr, out);
    let mut lend = |addr: u64, len: usize| bytes(addr, len);
    let mut record = |found: &acpi::ChangingRanges| {
        let mut ranges = [(0u64, 0u64); acpi::CHANGING_MAX];
        let mut n = 0;
        for (slot, &(_, base, end)) in ranges.iter_mut().zip(found.ranges()) {
            *slot = (base, end);
            n += 1;
        }
        let kept = vm::never_lend(ranges.get(..n).unwrap_or(&[]));
        // The first call's record is the one reported; a racing second finds the same.
        let _ = CHANGING.set((*found, kept));
    };
    acpi::lookup(
        rsdp_addr,
        sig,
        &mut vm::changing_located,
        &mut record,
        &mut copy_header,
        &mut lend,
    )
}
