//! **TEST ONLY, never shipped.** `efi_main` of the application that gives the boot test
//! firmware tables with reserved memory (TASKS item 40; the library's documentation says why
//! and what). It runs as `\EFI\BOOT\BOOTX64.EFI`, before Origin's loader:
//!
//! 1. reads `\EFI\ORIGIN\RESERVED.CFG` (the regions, from `cargo xtask`) and
//!    `\EFI\ORIGIN\LOADER.EFI` (the unchanged test loader) from its own volume;
//! 2. allocates each region: reserved pages (filled with [`fill_byte`], or with another page's
//!    fill), loader-data pages (RAM the kernel will own, which it must refuse), a fixed
//!    address, or the pages of the table copy itself;
//! 3. copies the firmware's DMAR or IVRS, with an RMRR or IVMD per region appended, into new
//!    `EfiReservedMemoryType` pages (a firmware may keep its tables there; then a range over
//!    the copy passes the memory-map rule, and only the kernel's refusal of writable ranges over
//!    tables it holds stops it), and points the XSDT's entry at the copy;
//! 4. shapes the tables as rental B's board has them, if the configuration asks (TASKS item 83):
//!    reserved pages at the top of the RAM; a copy of the FADT naming the FACS by its 32-bit
//!    field alone; an FPDT whose FBPT and S3PT are in reserved pages; and copies of the XSDT and
//!    the RSDP in ACPI NVS memory, with the configuration table pointed at the RSDP's copy;
//! 5. prints each region and each change on the console, so the boot test can hold the
//!    kernel's report against it, and starts the loader with this volume as its device.
//!
//! Any failure is printed and returned: the loader never starts, and the boot test fails.
//! The `unsafe` here is the firmware's calling convention and its memory, each with a SAFETY
//! note; written against UEFI 2.11 (§4.3, §4.4, §7.2, §7.4, §9.1, §13.4, §13.5).
//!
//! **Aimed at OVMF, with one departure from UEFI 2.11 §7.2.** §7.2 asks that an application not
//! aimed at one implementation allocate the runtime memory types (reserved, ACPI reclaim, ACPI
//! NVS) with `AllocateAnyPages`. This one allocates them below 4 GiB (`AllocateMaxAddress`), where
//! firmware keeps its tables and the RSDT can name them with its 32-bit entries, and takes the
//! reserved pages at the top of the RAM at their address (`AllocateAddress`), which is the shape
//! itself (TASKS item 83; the review's T83-6). OVMF allows both.
#![no_std]
#![no_main]

use core::ffi::c_void;
use core::fmt::Write;
use core::ptr;

use origin_boot_uefi::console::ConOut;
use origin_boot_uefi::protocols::BOOT_SERVICES_SIGNATURE;
use origin_boot_uefi::protocols::{
    ACPI_20_TABLE, BootServices, ConfigurationTable, FILE_INFO, FILE_INFO_FILE_SIZE_OFFSET,
    FILE_MODE_READ, FileProtocol, Guid, LOADED_IMAGE_PROTOCOL, LoadedImageProtocol,
    SIMPLE_FILE_SYSTEM_PROTOCOL, SimpleFileSystemProtocol,
};
use origin_boot_uefi::uefi::{Handle, Status, SystemTable};
use origin_test_reserved_memory::{
    FBPT_LEN, FPDT_LEN, MAX_REGIONS, PAGE, Placement, RSDP_LEN, Region, S3PT_LEN, STRUCTURE, Shape,
    Table, decode, facs_by_32_bit_field, facs_fields, fbpt, fill_byte, fpdt, grow, names_an_xsdt,
    rsdp_naming, rsdt_entries, rsdt_set, s3pt, structure, xsdt_entries, xsdt_set,
};

/// `AllocateMaxAddress`: at or below the address given; `AllocateAddress`: at it (§7.2).
const ALLOCATE_MAX_ADDRESS: u32 = 1;
const ALLOCATE_ADDRESS: u32 = 2;
/// `EfiReservedMemoryType`, `EfiLoaderData`, `EfiConventionalMemory`, `EfiACPIReclaimMemory`,
/// `EfiACPIMemoryNVS` (§7.2).
const RESERVED_MEMORY: u32 = 0;
const LOADER_DATA: u32 = 2;
const CONVENTIONAL_MEMORY: u32 = 7;
const ACPI_RECLAIM: u32 = 9;
const ACPI_NVS: u32 = 10;
/// Bytes of the buffer the memory map is read into: room for 1,024 descriptors of 48 bytes.
const MAP_BUFFER: usize = 48 * 1024;
/// Regions are placed below 4 GiB, where firmware puts its legacy reservations.
const BELOW_4G: u64 = 0xFFFF_F000;
/// The largest file read (the loader is about 300 KiB).
const MAX_FILE: u64 = 16 << 20;

/// `EFI_IMAGE_LOAD` (§7.4).
type LoadImageFn = unsafe extern "efiapi" fn(
    boot_policy: u8,
    parent: Handle,
    device_path: *const c_void,
    source: *const c_void,
    size: usize,
    image: *mut Handle,
) -> Status;
/// `EFI_IMAGE_START` (§7.4).
type StartImageFn =
    unsafe extern "efiapi" fn(image: Handle, exit_size: *mut usize, exit: *mut *mut u16) -> Status;
/// `EFI_INSTALL_CONFIGURATION_TABLE` (§4.4).
type InstallConfigurationTableFn =
    unsafe extern "efiapi" fn(guid: *const Guid, table: *mut c_void) -> Status;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

/// The entry point.
///
/// # Safety
///
/// Called by the firmware with this image's handle and a valid system table.
#[unsafe(no_mangle)]
pub unsafe extern "efiapi" fn efi_main(image: Handle, system_table: *const SystemTable) -> Status {
    // SAFETY: the firmware passes a valid system table that lives while boot services do.
    let st = unsafe { &*system_table };
    // SAFETY: `con_out` is the firmware's live console protocol, or null (refused by `new`).
    let Some(mut out) = (unsafe { ConOut::new(st.con_out) }) else {
        return Status::UNSUPPORTED;
    };
    match run(image, st, &mut out) {
        Ok(status) => status,
        Err((status, why)) => {
            let _ = writeln!(out, "origin-test-reserved: {why}: {status:?}");
            status
        }
    }
}

type Failure = (Status, &'static str);

fn run(image: Handle, st: &SystemTable, out: &mut ConOut) -> Result<Status, Failure> {
    let bs_ptr = st.boot_services.cast::<BootServices>().cast_const();
    if bs_ptr.is_null() || !bs_ptr.is_aligned() {
        return Err((Status::UNSUPPORTED, "no boot services"));
    }
    // SAFETY: the system table's boot services table is valid while boot services are.
    let bs = unsafe { &*bs_ptr };
    // The slots this application calls by position, up to `StartImage` at 208, must lie in the
    // table the firmware says it has (UEFI 2.11 §4.4; the review's T83-11).
    if bs.hdr.signature != BOOT_SERVICES_SIGNATURE || bs.hdr.header_size < 216 {
        return Err((
            Status::UNSUPPORTED,
            "the boot services table is not one this application knows",
        ));
    }
    let loaded = protocol::<LoadedImageProtocol>(bs, image, &LOADED_IMAGE_PROTOCOL)?;
    // SAFETY: `HandleProtocol` returned this image's live loaded-image protocol.
    let device = unsafe { (*loaded).device_handle };
    let root = open_volume(bs, device)?;
    let config = read_file(bs, root, "\\EFI\\ORIGIN\\RESERVED.CFG")?;
    let loader = read_file(bs, root, "\\EFI\\ORIGIN\\LOADER.EFI")?;
    let mut regions = [Region {
        table: Table::Dmar,
        ivmd_kind: 0,
        flags: 0,
        placement: Placement::Reserved,
        device: 0,
        last_device: 0,
        pages: 1,
    }; MAX_REGIONS];
    let (count, shape) = decode(config, &mut regions)
        .map_err(|_| (Status::LOAD_ERROR, "RESERVED.CFG is invalid"))?;
    let regions = &regions[..count];
    if count != 0 {
        let _ = writeln!(
            out,
            "origin-test-reserved: TEST ONLY: planting reserved memory in the firmware's tables"
        );
    }
    let rsdp = rsdp_address(st)?;
    let xsdt = xsdt_at(rsdp)?;
    for table in [Table::Dmar, Table::Ivrs] {
        if regions.iter().any(|r| r.table == table) {
            plant(bs, out, xsdt, table, regions)?;
        }
    }
    if shape.any() {
        shape_tables(bs, out, rsdp, xsdt, shape)?;
    }
    start_loader(bs, out, image, device, loader)
}

/// The ACPI 2.0 RSDP's address, from the configuration table.
fn rsdp_address(st: &SystemTable) -> Result<u64, Failure> {
    let table = st
        .configuration_table
        .cast::<ConfigurationTable>()
        .cast_const();
    let missing = (Status::NOT_FOUND, "no ACPI 2.0 RSDP");
    if table.is_null() || !table.is_aligned() {
        return Err(missing);
    }
    let mut rsdp = 0u64;
    for i in 0..st.number_of_table_entries.min(256) {
        // SAFETY: the configuration table holds `NumberOfTableEntries` entries (§4.3); `i` is
        // below that count and the base is aligned (checked).
        let entry = unsafe { table.wrapping_add(i).read() };
        if entry.vendor_guid == ACPI_20_TABLE {
            // The first, as the loader takes it.
            rsdp = entry.vendor_table as u64;
            break;
        }
    }
    if rsdp == 0 {
        return Err(missing);
    }
    Ok(rsdp)
}

/// The XSDT the RSDP at `rsdp` names, writable.
fn xsdt_at(rsdp: u64) -> Result<&'static mut [u8], Failure> {
    // SAFETY: the RSDP the firmware published is 36 bytes of identity-mapped memory (ACPI 6.6
    // §5.2.5.3); boot services keep it mapped.
    let rsdp_bytes = unsafe { physical(rsdp, 36) };
    if !names_an_xsdt(rsdp_bytes) {
        return Err((
            Status::LOAD_ERROR,
            "the RSDP is not an ACPI 2.0 one that names an XSDT",
        ));
    }
    if rsdp_bytes.get(..8) != Some(&b"RSD PTR "[..]) {
        return Err((Status::LOAD_ERROR, "the RSDP's signature is wrong"));
    }
    let mut a = [0u8; 8];
    a.copy_from_slice(&rsdp_bytes[24..32]);
    let at = u64::from_le_bytes(a);
    // SAFETY: the XSDT the RSDP names: its 36-byte header first, then its declared length.
    let head = unsafe { physical(at, 36) };
    let mut l = [0u8; 4];
    l.copy_from_slice(&head[4..8]);
    let len = u32::from_le_bytes(l) as usize;
    if head.get(..4) != Some(&b"XSDT"[..]) || !(36..=4096).contains(&len) {
        return Err((Status::LOAD_ERROR, "the XSDT is not one"));
    }
    // SAFETY: as above, over the XSDT's declared length, which only this application changes
    // while it runs.
    Ok(unsafe { physical_mut(at, len) })
}

/// Bytes of identity-mapped firmware memory.
///
/// # Safety
///
/// `[addr, addr + len)` must be mapped and readable for as long as the slice is used, and not
/// written meanwhile.
unsafe fn physical(addr: u64, len: usize) -> &'static [u8] {
    // SAFETY: the caller's contract.
    unsafe { core::slice::from_raw_parts(ptr::with_exposed_provenance::<u8>(addr as usize), len) }
}

/// Bytes of identity-mapped firmware memory, writable.
///
/// # Safety
///
/// `[addr, addr + len)` must be mapped and writable for as long as the slice is used, and
/// reached through nothing else meanwhile.
unsafe fn physical_mut(addr: u64, len: usize) -> &'static mut [u8] {
    // SAFETY: the caller's contract.
    unsafe {
        core::slice::from_raw_parts_mut(ptr::with_exposed_provenance_mut::<u8>(addr as usize), len)
    }
}

/// Allocates every region of `table` among `regions`, appends a structure for each to a copy
/// of the firmware's table of that signature, and points the XSDT at the copy.
fn plant(
    bs: &BootServices,
    out: &mut ConOut,
    xsdt: &mut [u8],
    table: Table,
    regions: &[Region],
) -> Result<(), Failure> {
    let sig: &[u8; 4] = match table {
        Table::Dmar => b"DMAR",
        Table::Ivrs => b"IVRS",
    };
    let (index, addr) = xsdt_entries(xsdt)
        .find(|&(_, a)| {
            // SAFETY: every XSDT entry names a table whose 4-byte signature is readable.
            a != 0 && unsafe { physical(a, 4) } == sig
        })
        .ok_or((
            Status::NOT_FOUND,
            "the firmware has no table to plant regions in",
        ))?;
    // SAFETY: the table's header, then its declared length, as for the XSDT.
    let head = unsafe { physical(addr, 8) };
    let mut l = [0u8; 4];
    l.copy_from_slice(&head[4..8]);
    let old_len = u32::from_le_bytes(l) as usize;
    if !(36..=0x1_0000).contains(&old_len) {
        return Err((
            Status::LOAD_ERROR,
            "the firmware's table has a strange length",
        ));
    }
    // SAFETY: as above.
    let old = unsafe { physical(addr, old_len) };
    let mut extra = [0u8; STRUCTURE * MAX_REGIONS];
    let mut used = 0;
    // The copy's pages first, so a region can name them (Placement::Table).
    let new_len = old_len + STRUCTURE * regions.iter().filter(|r| r.table == table).count();
    let new = allocate(bs, RESERVED_MEMORY, new_len.div_ceil(PAGE as usize))?;
    for r in regions.iter().filter(|r| r.table == table) {
        let pages = r.pages as usize;
        let base = match r.placement {
            Placement::Reserved | Placement::ReservedOther => {
                let base = allocate(bs, RESERVED_MEMORY, pages)?;
                // Each page's own fill, or, for the pages the kernel must leave alone, another
                // page's.
                let other = if r.placement == Placement::Reserved {
                    0
                } else {
                    0x10_0000
                };
                // SAFETY: the pages just allocated, identity-mapped and owned by nothing else.
                let mem = unsafe { physical_mut(base, pages * PAGE as usize) };
                for (o, b) in mem.iter_mut().enumerate() {
                    let o = o as u64;
                    *b = fill_byte(base + o / PAGE * PAGE + other, o % PAGE);
                }
                base
            }
            Placement::LoaderData => allocate(bs, LOADER_DATA, pages)?,
            Placement::Fixed(at) => at,
            Placement::Table => new,
        };
        extra[used..used + STRUCTURE].copy_from_slice(&structure(r, base));
        used += STRUCTURE;
        let what = match r.placement {
            Placement::Reserved => "reserved pages",
            Placement::ReservedOther => "reserved pages without the test fill",
            Placement::LoaderData => "loader-data pages",
            Placement::Fixed(_) => "a fixed address",
            Placement::Table => "the table's own pages",
        };
        let last = base + u64::from(r.pages) * PAGE - 1;
        let (bus, dev, fun) = (r.device >> 8, (r.device >> 3) & 0x1F, r.device & 7);
        match table {
            Table::Dmar => {
                let _ = writeln!(
                    out,
                    "origin-test-reserved: RMRR {base:#x}-{last:#x} ({what}) for {bus:02x}:{dev:02x}.{fun}"
                );
            }
            Table::Ivrs => {
                let _ = writeln!(
                    out,
                    "origin-test-reserved: IVMD type {:#x} flags {:#06b} {base:#x}-{last:#x} ({what}) for DeviceID {:#06x}",
                    r.ivmd_kind, r.flags, r.device
                );
            }
        }
    }
    if old_len + used != new_len {
        return Err((Status::LOAD_ERROR, "the structures do not fill the copy"));
    }
    // SAFETY: the pages allocated above, at least `new_len` bytes, owned by nothing else.
    let dst = unsafe { physical_mut(new, new_len) };
    grow(old, &extra[..used], dst).ok_or((Status::LOAD_ERROR, "the copy does not fit"))?;
    xsdt_set(xsdt, index, new).ok_or((Status::LOAD_ERROR, "the XSDT cannot be changed"))?;
    let _ = writeln!(
        out,
        "origin-test-reserved: the XSDT now names {} at {new:#x} ({new_len} bytes) in place of the firmware's at {addr:#x}",
        core::str::from_utf8(sig).unwrap_or("the table")
    );
    Ok(())
}

/// Shapes the firmware's tables as rental B's board has them (TASKS item 83), in this order:
/// reserved pages at the top of the RAM; a copy of the FADT whose FACS is named by
/// `FIRMWARE_CTRL` alone; an FPDT whose FBPT and S3PT are in reserved pages; then a copy of the
/// XSDT (in ACPI NVS memory if asked, with the FPDT's entry appended), and an RSDP naming it:
/// a copy in ACPI NVS memory, which the configuration table then names, or the firmware's own,
/// rewritten in place.
fn shape_tables(
    bs: &BootServices,
    out: &mut ConOut,
    rsdp: u64,
    xsdt: &mut [u8],
    shape: Shape,
) -> Result<(), Failure> {
    let _ = writeln!(
        out,
        "origin-test-reserved: TEST ONLY: shaping the firmware's tables as rental B's board has them"
    );
    if shape.top_reserved_pages != 0 {
        let pages = shape.top_reserved_pages as usize;
        let base = allocate_at_top(bs, RESERVED_MEMORY, pages)?;
        let last = base + pages as u64 * PAGE - 1;
        let _ = writeln!(
            out,
            "origin-test-reserved: reserved {base:#x}-{last:#x} ({pages} pages) at the top of the RAM"
        );
    }
    let mut fadt_copy = None;
    if shape.facs_32 {
        let (index, addr, old) = find_table(xsdt, b"FACP")?;
        if let Some((legacy, x)) = facs_fields(old) {
            let _ = writeln!(
                out,
                "origin-test-reserved: the firmware's FADT names the FACS by FIRMWARE_CTRL {legacy:#x} and X_FIRMWARE_CTRL {x:#x}"
            );
        }
        let new = allocate(bs, ACPI_RECLAIM, old.len().div_ceil(PAGE as usize))?;
        // SAFETY: the pages just allocated, identity-mapped, owned by nothing else, at least
        // `old.len()` bytes.
        let copy = unsafe { physical_mut(new, old.len()) };
        copy.copy_from_slice(old);
        let facs = facs_by_32_bit_field(copy)
            .ok_or((Status::LOAD_ERROR, "the FADT names no FACS below 4 GiB"))?;
        xsdt_set(xsdt, index, new).ok_or((Status::LOAD_ERROR, "the XSDT cannot be changed"))?;
        fadt_copy = Some((addr, new));
        let _ = writeln!(
            out,
            "origin-test-reserved: the XSDT now names FACP at {new:#x} in place of the firmware's at {addr:#x}: the FACS at {facs:#x} by FIRMWARE_CTRL alone"
        );
    }
    let mut appended = None;
    if shape.fpdt {
        let fbpt_at = allocate(bs, RESERVED_MEMORY, FBPT_LEN.div_ceil(PAGE as usize))?;
        let s3pt_at = allocate(bs, RESERVED_MEMORY, S3PT_LEN.div_ceil(PAGE as usize))?;
        let fpdt_at = allocate(bs, ACPI_RECLAIM, FPDT_LEN.div_ceil(PAGE as usize))?;
        // SAFETY: each the pages just allocated for it, identity-mapped and owned by nothing
        // else, at least as long as what is written.
        unsafe { physical_mut(fbpt_at, FBPT_LEN) }.copy_from_slice(&fbpt());
        // SAFETY: as above.
        unsafe { physical_mut(s3pt_at, S3PT_LEN) }.copy_from_slice(&s3pt());
        // SAFETY: as above.
        unsafe { physical_mut(fpdt_at, FPDT_LEN) }.copy_from_slice(&fpdt(fbpt_at, s3pt_at));
        let _ = writeln!(
            out,
            "origin-test-reserved: FPDT at {fpdt_at:#x}, its FBPT at {fbpt_at:#x} and S3PT at {s3pt_at:#x} in reserved pages"
        );
        appended = Some(fpdt_at);
    }
    if !shape.acpi_nvs && appended.is_none() {
        return Ok(());
    }
    // The XSDT's copy, with the FPDT's entry after the firmware's entries.
    let old_len = xsdt.len();
    let new_len = old_len + appended.map_or(0, |_| 8);
    let (kind, name) = if shape.acpi_nvs {
        (ACPI_NVS, "ACPI NVS")
    } else {
        (ACPI_RECLAIM, "ACPI reclaim")
    };
    let new_xsdt = allocate(bs, kind, new_len.div_ceil(PAGE as usize))?;
    // SAFETY: the pages just allocated, identity-mapped, owned by nothing else, at least
    // `new_len` bytes.
    let dst = unsafe { physical_mut(new_xsdt, new_len) };
    match appended {
        Some(fpdt_at) => {
            grow(xsdt, &fpdt_at.to_le_bytes(), dst)
                .ok_or((Status::LOAD_ERROR, "the XSDT's copy does not fit"))?;
        }
        None => dst.copy_from_slice(xsdt),
    }
    let _ = writeln!(
        out,
        "origin-test-reserved: the XSDT is copied to {new_xsdt:#x} ({name}), {new_len} bytes"
    );
    // SAFETY: the RSDP the firmware published, 36 bytes (`xsdt_at` checked it names an XSDT).
    let old_rsdp = unsafe { physical(rsdp, RSDP_LEN) };
    let new_rsdt = rsdt_copy(bs, out, old_rsdp, kind, name, fadt_copy, appended)?;
    let new_rsdp = rsdp_naming(old_rsdp, new_rsdt, new_xsdt)
        .ok_or((Status::LOAD_ERROR, "the RSDP is not an ACPI 2.0 one"))?;
    if shape.acpi_nvs {
        // TASKS item 84: in loader data, the loader still reads it, and the kernel refuses it.
        let (kind, name) = if shape.rsdp_in_ram {
            (LOADER_DATA, "loader data, RAM the kernel owns")
        } else {
            (ACPI_NVS, "ACPI NVS")
        };
        let at = allocate(bs, kind, 1)?;
        // SAFETY: the page just allocated, identity-mapped and owned by nothing else.
        unsafe { physical_mut(at, RSDP_LEN) }.copy_from_slice(&new_rsdp);
        install_configuration_table(bs, &ACPI_20_TABLE, at)?;
        let _ = writeln!(
            out,
            "origin-test-reserved: the RSDP is copied to {at:#x} ({name}), and the configuration table names it"
        );
    } else {
        // SAFETY: the firmware's RSDP, 36 bytes of identity-mapped memory this application
        // alone writes while it runs.
        unsafe { physical_mut(rsdp, RSDP_LEN) }.copy_from_slice(&new_rsdp);
        let _ = writeln!(
            out,
            "origin-test-reserved: the RSDP at {rsdp:#x} now names it"
        );
    }
    Ok(())
}

/// A copy of the RSDT the RSDP names, made as the XSDT's is (TASKS item 83; the review's T83-5):
/// the FADT's entry pointed at its copy, and the FPDT's appended, in pages of `kind`. Returns the
/// copy's address, or the firmware's RSDT's unchanged when it names none (0). Every address it
/// writes lies below 4 GiB, where `allocate` takes pages.
fn rsdt_copy(
    bs: &BootServices,
    out: &mut ConOut,
    rsdp: &[u8],
    kind: u32,
    name: &str,
    fadt: Option<(u64, u64)>,
    fpdt: Option<u64>,
) -> Result<u32, Failure> {
    let rsdt = u32::from_le_bytes([rsdp[16], rsdp[17], rsdp[18], rsdp[19]]);
    if rsdt == 0 {
        return Ok(0);
    }
    // SAFETY: the RSDT the RSDP names, its 36-byte header first.
    let head = unsafe { physical(u64::from(rsdt), 36) };
    let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
    if &head[..4] != b"RSDT" || !(36..=4096).contains(&len) {
        return Err((Status::LOAD_ERROR, "the RSDT is not one"));
    }
    // SAFETY: as above, over its declared length.
    let old = unsafe { physical(u64::from(rsdt), len) };
    let below_4g =
        |a: u64| u32::try_from(a).map_err(|_| (Status::LOAD_ERROR, "a table above 4 GiB"));
    let (extra, extra_len) = match fpdt {
        Some(at) => (below_4g(at)?.to_le_bytes(), 4),
        None => ([0u8; 4], 0),
    };
    let extra = &extra[..extra_len];
    let new_len = len + extra.len();
    let new = allocate(bs, kind, new_len.div_ceil(PAGE as usize))?;
    // SAFETY: the pages just allocated, identity-mapped, owned by nothing else, at least
    // `new_len` bytes.
    let dst = unsafe { physical_mut(new, new_len) };
    grow(old, extra, dst).ok_or((Status::LOAD_ERROR, "the RSDT's copy does not fit"))?;
    if let Some((firmware, copy)) = fadt {
        let index = rsdt_entries(dst)
            .find(|&(_, a)| u64::from(a) == firmware)
            .map(|(i, _)| i)
            .ok_or((Status::LOAD_ERROR, "the RSDT does not name the FADT"))?;
        rsdt_set(dst, index, below_4g(copy)?)
            .ok_or((Status::LOAD_ERROR, "the RSDT cannot be changed"))?;
    }
    let _ = writeln!(
        out,
        "origin-test-reserved: the RSDT is copied to {new:#x} ({name}), {new_len} bytes"
    );
    below_4g(new)
}

/// The first table with signature `sig` the XSDT lists: its index there, its address, and its
/// bytes over its declared length.
fn find_table(xsdt: &[u8], sig: &[u8; 4]) -> Result<(usize, u64, &'static [u8]), Failure> {
    let (index, addr) = xsdt_entries(xsdt)
        .find(|&(_, a)| {
            // SAFETY: every XSDT entry names a table whose 4-byte signature is readable.
            a != 0 && unsafe { physical(a, 4) } == sig
        })
        .ok_or((Status::NOT_FOUND, "the firmware has no such table"))?;
    // SAFETY: the table's header, then its declared length.
    let head = unsafe { physical(addr, 8) };
    let mut l = [0u8; 4];
    l.copy_from_slice(&head[4..8]);
    let len = u32::from_le_bytes(l) as usize;
    if !(36..=0x1_0000).contains(&len) {
        return Err((Status::LOAD_ERROR, "a table has a strange length"));
    }
    // SAFETY: as above.
    Ok((index, addr, unsafe { physical(addr, len) }))
}

/// `pages` pages of `memory_type` at the top of the RAM: the end of the highest
/// `EfiConventionalMemory` range the memory map lists that holds them (§7.2).
fn allocate_at_top(bs: &BootServices, memory_type: u32, pages: usize) -> Result<u64, Failure> {
    let get = bs
        .get_memory_map
        .ok_or((Status::UNSUPPORTED, "no GetMemoryMap"))?;
    let buffer = allocate(bs, LOADER_DATA, MAP_BUFFER.div_ceil(PAGE as usize))?;
    let (mut size, mut key, mut descriptor, mut version) = (MAP_BUFFER, 0usize, 0usize, 0u32);
    // SAFETY: valid `GetMemoryMap` into this application's buffer of `size` bytes, with live
    // out-parameters.
    let status = unsafe {
        get(
            &mut size,
            ptr::with_exposed_provenance_mut::<u8>(buffer as usize),
            &mut key,
            &mut descriptor,
            &mut version,
        )
    };
    if status.is_error() || descriptor < 40 || size > MAP_BUFFER {
        return Err((status, "the memory map cannot be read"));
    }
    // SAFETY: the `size` bytes `GetMemoryMap` just wrote into this application's buffer.
    let map = unsafe { physical(buffer, size) };
    let want = pages as u64 * PAGE;
    let field = |d: &[u8], at: usize| {
        let mut v = [0u8; 8];
        v.copy_from_slice(&d[at..at + 8]);
        u64::from_le_bytes(v)
    };
    // `EFI_MEMORY_DESCRIPTOR`: Type at 0, PhysicalStart at 8, NumberOfPages at 24.
    let top = map
        .chunks_exact(descriptor)
        .filter(|d| u32::from_le_bytes([d[0], d[1], d[2], d[3]]) == CONVENTIONAL_MEMORY)
        .map(|d| (field(d, 8), field(d, 24).saturating_mul(PAGE)))
        .filter(|&(_, bytes)| bytes >= want)
        .map(|(start, bytes)| start + bytes)
        .max()
        .ok_or((
            Status::OUT_OF_RESOURCES,
            "no free range holds the top's pages",
        ))?;
    let f = bs
        .allocate_pages
        .ok_or((Status::UNSUPPORTED, "no AllocatePages"))?;
    let mut addr = top - want;
    // SAFETY: valid `AllocatePages` at the address in `addr`, a live local.
    let status = unsafe { f(ALLOCATE_ADDRESS, memory_type, pages, &mut addr) };
    if status.is_error() {
        return Err((status, "AllocatePages at the top of the RAM failed"));
    }
    Ok(addr)
}

/// `InstallConfigurationTable` for `guid`, naming the table at physical `table`.
fn install_configuration_table(bs: &BootServices, guid: &Guid, table: u64) -> Result<(), Failure> {
    // `LocateDevicePath` and `InstallConfigurationTable` follow `LocateHandle` (§4.4).
    let install = bs.device_paths[1];
    if install.is_null() {
        return Err((Status::UNSUPPORTED, "no InstallConfigurationTable"));
    }
    // SAFETY: the boot services table's slot at offset 192, after `LocateDevicePath` and
    // before `LoadImage` at 200, holds `InstallConfigurationTable` (§4.4).
    let install: InstallConfigurationTableFn =
        unsafe { core::mem::transmute::<*const c_void, InstallConfigurationTableFn>(install) };
    // SAFETY: valid `InstallConfigurationTable` with a live GUID and a table this application
    // allocated, which stays allocated.
    let status = unsafe {
        install(
            guid,
            ptr::with_exposed_provenance_mut::<c_void>(table as usize),
        )
    };
    if status.is_error() {
        return Err((status, "InstallConfigurationTable failed"));
    }
    Ok(())
}

/// Zero pages of `memory_type` below 4 GiB.
fn allocate(bs: &BootServices, memory_type: u32, pages: usize) -> Result<u64, Failure> {
    let f = bs
        .allocate_pages
        .ok_or((Status::UNSUPPORTED, "no AllocatePages"))?;
    let mut addr = BELOW_4G;
    // SAFETY: valid `AllocatePages`; `addr` is a live local that holds the highest address
    // allowed and receives the allocation's.
    let status = unsafe { f(ALLOCATE_MAX_ADDRESS, memory_type, pages, &mut addr) };
    if status.is_error() {
        return Err((status, "AllocatePages failed"));
    }
    // SAFETY: the pages just allocated, identity-mapped and owned by nothing else.
    unsafe { physical_mut(addr, pages * PAGE as usize) }.fill(0);
    Ok(addr)
}

/// `HandleProtocol` for `guid` on `handle`.
fn protocol<T>(bs: &BootServices, handle: Handle, guid: &Guid) -> Result<*mut T, Failure> {
    let f = bs
        .handle_protocol
        .ok_or((Status::UNSUPPORTED, "no HandleProtocol"))?;
    let mut iface: *mut c_void = ptr::null_mut();
    // SAFETY: valid `HandleProtocol` with a live GUID and a live out-pointer.
    let status = unsafe { f(handle, guid, &mut iface) };
    if status.is_error() || iface.is_null() {
        return Err((Status::NOT_FOUND, "a protocol is missing"));
    }
    Ok(iface.cast())
}

/// The root directory of the volume on `device`.
fn open_volume(bs: &BootServices, device: Handle) -> Result<*mut FileProtocol, Failure> {
    let fs = protocol::<SimpleFileSystemProtocol>(bs, device, &SIMPLE_FILE_SYSTEM_PROTOCOL)?;
    // SAFETY: a live simple-file-system protocol from `HandleProtocol`.
    let open = unsafe { (*fs).open_volume }.ok_or((Status::UNSUPPORTED, "no OpenVolume"))?;
    let mut root: *mut FileProtocol = ptr::null_mut();
    // SAFETY: valid `OpenVolume` on a live instance; `root` is a live local.
    let status = unsafe { open(fs, &mut root) };
    if status.is_error() || root.is_null() {
        return Err((Status::NOT_FOUND, "the volume cannot be opened"));
    }
    Ok(root)
}

/// The whole of file `path` under `root`, in fresh loader-data pages.
fn read_file(
    bs: &BootServices,
    root: *mut FileProtocol,
    path: &str,
) -> Result<&'static [u8], Failure> {
    let mut name = [0u16; 64];
    for (d, b) in name.iter_mut().zip(path.bytes()) {
        *d = u16::from(b);
    }
    // SAFETY: `root` is a live file handle from `OpenVolume`.
    let open = unsafe { (*root).open }.ok_or((Status::UNSUPPORTED, "no Open"))?;
    let mut file: *mut FileProtocol = ptr::null_mut();
    // SAFETY: valid `Open`; `name` is NUL-terminated (longer than every path used); `file`
    // is a live local.
    let status = unsafe { open(root, &mut file, name.as_ptr(), FILE_MODE_READ, 0) };
    if status.is_error() || file.is_null() {
        return Err((Status::NOT_FOUND, "a file is missing"));
    }
    // SAFETY: `file` is the live handle `Open` returned.
    let get_info = unsafe { (*file).get_info }.ok_or((Status::UNSUPPORTED, "no GetInfo"))?;
    let mut info = [0u64; 64];
    let mut info_size = core::mem::size_of::<[u64; 64]>();
    // SAFETY: valid `GetInfo` into a live, aligned buffer of `info_size` bytes.
    let status = unsafe { get_info(file, &FILE_INFO, &mut info_size, info.as_mut_ptr().cast()) };
    let size = info
        .get(FILE_INFO_FILE_SIZE_OFFSET / 8)
        .copied()
        .unwrap_or(0);
    if status.is_error() || size == 0 || size > MAX_FILE {
        return Err((Status::LOAD_ERROR, "a file cannot be sized"));
    }
    let len = size as usize;
    let addr = allocate(bs, LOADER_DATA, len.div_ceil(PAGE as usize))?;
    // SAFETY: `file` is live.
    let read = unsafe { (*file).read }.ok_or((Status::UNSUPPORTED, "no Read"))?;
    let mut done = 0usize;
    while done < len {
        let mut chunk = len - done;
        let dst = ptr::with_exposed_provenance_mut::<u8>(addr as usize + done);
        // SAFETY: valid `Read` into this application's allocation, which has `len - done`
        // bytes left at `dst`.
        let status = unsafe { read(file, &mut chunk, dst.cast()) };
        if status.is_error() || chunk == 0 {
            return Err((Status::LOAD_ERROR, "a file cannot be read"));
        }
        done += chunk;
    }
    // SAFETY: `file` is the handle `Open` returned; it is not used afterwards.
    if let Some(close) = unsafe { (*file).close } {
        // SAFETY: as above.
        let _ = unsafe { close(file) };
    }
    // SAFETY: the `len` bytes just read into pages this application keeps.
    Ok(unsafe { physical(addr, len) })
}

/// Loads `loader` from memory and starts it with `device` as the volume it was loaded from,
/// where it finds `\EFI\ORIGIN\KERNEL.ONBF`. Returns only if it does.
fn start_loader(
    bs: &BootServices,
    out: &mut ConOut,
    parent: Handle,
    device: Handle,
    loader: &[u8],
) -> Result<Status, Failure> {
    // `LoadImage` and `StartImage` are the first two of the four image services (§4.4).
    let [load, start, ..] = bs.images;
    if load.is_null() || start.is_null() {
        return Err((Status::UNSUPPORTED, "no LoadImage or StartImage"));
    }
    // SAFETY: the boot services table's `LoadImage` slot holds that service (§4.4, at offset
    // 200, which the loader library pins).
    let load: LoadImageFn = unsafe { core::mem::transmute::<*const c_void, LoadImageFn>(load) };
    // SAFETY: as above, `StartImage` at offset 208.
    let start: StartImageFn = unsafe { core::mem::transmute::<*const c_void, StartImageFn>(start) };
    let mut child: Handle = ptr::null_mut();
    // SAFETY: valid `LoadImage` from a buffer this application keeps, with no device path;
    // `child` is a live local.
    let status = unsafe {
        load(
            0,
            parent,
            ptr::null(),
            loader.as_ptr().cast(),
            loader.len(),
            &mut child,
        )
    };
    if status.is_error() || child.is_null() {
        return Err((status, "LoadImage refused the loader"));
    }
    let loaded = protocol::<LoadedImageProtocol>(bs, child, &LOADED_IMAGE_PROTOCOL)?;
    // SAFETY: the loader's live loaded-image protocol, before it runs: it reads its volume
    // from this field.
    unsafe { (*loaded).device_handle = device };
    let _ = writeln!(out, "origin-test-reserved: starting the loader");
    // SAFETY: valid `StartImage` on the image just loaded; no exit data is asked for.
    let status = unsafe { start(child, ptr::null_mut(), ptr::null_mut()) };
    Ok(status)
}
