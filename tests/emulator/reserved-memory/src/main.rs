//! **TEST ONLY, never shipped.** `efi_main` of the application that gives the boot test
//! firmware tables with reserved memory (TASKS item 40; the library's documentation says why
//! and what). It runs as `\EFI\BOOT\BOOTX64.EFI`, before Origin's loader:
//!
//! 1. reads `\EFI\ORIGIN\RESERVED.CFG` (the regions, from `cargo xtask`) and
//!    `\EFI\ORIGIN\LOADER.EFI` (the unchanged test loader) from its own volume;
//! 2. allocates each region: reserved pages (filled with [`fill_byte`]), loader-data pages
//!    (RAM the kernel will own, which it must refuse), or a fixed address;
//! 3. copies the firmware's DMAR or IVRS, with an RMRR or IVMD per region appended, into new
//!    ACPI-reclaim pages, and points the XSDT's entry at the copy;
//! 4. prints each region on the console, so the boot test can hold the kernel's report against
//!    it, and starts the loader with this volume as its device.
//!
//! Any failure is printed and returned: the loader never starts, and the boot test fails.
//! The `unsafe` here is the firmware's calling convention and its memory, each with a SAFETY
//! note; written against UEFI 2.11 (§4.3, §4.4, §7.2, §7.4, §9.1, §13.4, §13.5).
#![no_std]
#![no_main]

use core::ffi::c_void;
use core::fmt::Write;
use core::ptr;

use origin_boot_uefi::console::ConOut;
use origin_boot_uefi::protocols::{
    ACPI_20_TABLE, BootServices, ConfigurationTable, FILE_INFO, FILE_INFO_FILE_SIZE_OFFSET,
    FILE_MODE_READ, FileProtocol, Guid, LOADED_IMAGE_PROTOCOL, LoadedImageProtocol,
    SIMPLE_FILE_SYSTEM_PROTOCOL, SimpleFileSystemProtocol,
};
use origin_boot_uefi::uefi::{Handle, Status, SystemTable};
use origin_test_reserved_memory::{
    MAX_REGIONS, PAGE, Placement, Region, STRUCTURE, Table, decode, fill_byte, grow, structure,
    xsdt_entries, xsdt_set,
};

/// `AllocateMaxAddress`: at or below the address given.
const ALLOCATE_MAX_ADDRESS: u32 = 1;
/// `EfiReservedMemoryType`, `EfiLoaderData`, `EfiACPIReclaimMemory` (§7.2).
const RESERVED_MEMORY: u32 = 0;
const LOADER_DATA: u32 = 2;
const ACPI_RECLAIM: u32 = 9;
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
    let _ = writeln!(
        out,
        "origin-test-reserved: TEST ONLY: planting reserved memory in the firmware's tables"
    );
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
    let count = decode(config, &mut regions)
        .map_err(|_| (Status::LOAD_ERROR, "RESERVED.CFG is invalid"))?;
    let regions = &regions[..count];
    let xsdt = xsdt(st)?;
    for table in [Table::Dmar, Table::Ivrs] {
        if regions.iter().any(|r| r.table == table) {
            plant(bs, out, xsdt, table, regions)?;
        }
    }
    start_loader(bs, out, image, device, loader)
}

/// The firmware's XSDT, writable: found through the ACPI 2.0 RSDP in the configuration table.
fn xsdt(st: &SystemTable) -> Result<&'static mut [u8], Failure> {
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
            rsdp = entry.vendor_table as u64;
        }
    }
    if rsdp == 0 {
        return Err(missing);
    }
    // SAFETY: the RSDP the firmware published is 36 bytes of identity-mapped memory (ACPI 6.6
    // §5.2.5.3); boot services keep it mapped.
    let rsdp_bytes = unsafe { physical(rsdp, 36) };
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
    for (i, r) in regions.iter().enumerate().filter(|(_, r)| r.table == table) {
        let pages = r.pages as usize;
        let base = match r.placement {
            Placement::Reserved => {
                let base = allocate(bs, RESERVED_MEMORY, pages)?;
                // SAFETY: the pages just allocated, identity-mapped and owned by nothing else.
                let mem = unsafe { physical_mut(base, pages * PAGE as usize) };
                for (o, b) in mem.iter_mut().enumerate() {
                    *b = fill_byte(i, o as u64);
                }
                base
            }
            Placement::LoaderData => allocate(bs, LOADER_DATA, pages)?,
            Placement::Fixed(at) => at,
        };
        extra[used..used + STRUCTURE].copy_from_slice(&structure(r, base));
        used += STRUCTURE;
        let what = match r.placement {
            Placement::Reserved => "reserved pages",
            Placement::LoaderData => "loader-data pages",
            Placement::Fixed(_) => "a fixed address",
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
    let new_len = old_len + used;
    let new = allocate(bs, ACPI_RECLAIM, new_len.div_ceil(PAGE as usize))?;
    // SAFETY: the pages just allocated, at least `new_len` bytes, owned by nothing else.
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
