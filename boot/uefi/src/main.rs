//! `efi_main`, the entry point of the Origin OS UEFI loader (M1 step 2, ADR-0002).
//!
//! The loader:
//! 1. validates the system table and boot services;
//! 2. reads `\EFI\ORIGIN\KERNEL.ONBF` from its own volume (B1);
//! 3. verifies the image completely (B2);
//! 4. places it with its relocations (B3);
//! 5. fills in the boot info (B4);
//! 6. exits boot services (B5);
//! 7. enters the Framekernel (B6).
//!
//! It prints each step to the UEFI console; `cargo xtask boot-test` checks those lines and
//! the kernel's serial output in QEMU.
#![no_std]
#![no_main]

use core::ffi::c_void;
use core::fmt::Write;
use core::mem::size_of;
use core::panic::PanicInfo;
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

use origin_boot_info::{BootInfo, MemoryRegion};
use origin_boot_uefi::console::ConOut;
use origin_boot_uefi::protocols::{
    ACPI_20_TABLE, ALLOCATE_ANY_PAGES, BOOT_SERVICES_SIGNATURE, BootServices, ConfigurationTable,
    FILE_INFO, FILE_INFO_FILE_SIZE_OFFSET, FILE_MODE_READ, FileProtocol, Guid,
    LOADED_IMAGE_PROTOCOL, LoadedImageProtocol, SIMPLE_FILE_SYSTEM_PROTOCOL,
    SimpleFileSystemProtocol, memory_type,
};
use origin_boot_uefi::uefi::{
    self, Handle, Revision, SimpleTextOutputProtocol, Status, SystemTable, TableHeader,
};
use origin_boot_uefi::{kernel, memmap, place};

/// The console the panic handler writes to. Set once the system table is validated, and
/// cleared just before `ExitBootServices` (ADR-0002 B5).
static PANIC_CONSOLE: AtomicPtr<SimpleTextOutputProtocol> = AtomicPtr::new(ptr::null_mut());

/// Longest firmware vendor name the loader prints, in `CHAR16` units.
const VENDOR_UNITS: usize = 64;
/// Where the Framekernel image lives on the loader's volume (ADR-0002 B1).
const KERNEL_PATH: &str = "\\EFI\\ORIGIN\\KERNEL.ONBF";
/// Largest kernel image the loader will read.
const MAX_KERNEL_FILE: u64 = 64 << 20;
/// Kernel stack: 64 KiB.
const STACK_PAGES: usize = 16;
/// Extra memory-map descriptors to allow for allocations made after the size probe.
const MAP_SLACK: usize = 32;
const PAGE: usize = 4096;
const DEVELOPER_MODE: bool = cfg!(feature = "developer-mode");

/// Image entry point; the `x86_64-unknown-uefi` target links it as the PE entry.
///
/// # Safety
///
/// Only the firmware may call this, as the image entry point (UEFI 2.11 §4.1), with boot
/// services active, on the boot processor. `image` and `system_table` must be the values
/// the firmware passes there; the loader validates the table before use.
#[unsafe(no_mangle)]
pub unsafe extern "efiapi" fn efi_main(image: Handle, system_table: *const SystemTable) -> Status {
    // SAFETY: the caller contract above is exactly `read_system_table`'s precondition;
    // the function checks null, alignment, signature, revision and size before use.
    let st = match unsafe { uefi::read_system_table(system_table) } {
        Ok(st) => st,
        Err(e) => return e.status(),
    };
    // SAFETY: `con_out` comes from the validated system table, and boot services are
    // active until `ExitBootServices`, after which this console is never used again.
    let Some(mut out) = (unsafe { ConOut::new(st.con_out) }) else {
        return Status::UNSUPPORTED;
    };
    PANIC_CONSOLE.store(st.con_out, Ordering::Release);
    if report_firmware(&st, &mut out).is_err() {
        return out.last_error().unwrap_or(Status::DEVICE_ERROR);
    }
    match boot(image, system_table, &st, &mut out) {
        Ok(never) => match never {},
        Err(status) => status,
    }
}

/// Prints the loader banner and the firmware's identity.
fn report_firmware(st: &SystemTable, out: &mut ConOut) -> core::fmt::Result {
    let mut vendor = [0u16; VENDOR_UNITS];
    // SAFETY: `firmware_vendor` is the firmware's NUL-terminated vendor string from the
    // validated system table (UEFI 2.11 §4.3); the copy is bounded by `vendor.len()`.
    let n = unsafe { uefi::copy_char16_string(st.firmware_vendor, &mut vendor) };
    let vendor = vendor.get(..n).unwrap_or(&[]);
    // The release identity is generated from origin-version.toml (CLAUDE.md §62).
    writeln!(
        out,
        "{} {} UEFI loader (build {})",
        origin_version::PRODUCT,
        origin_version::VERSION,
        origin_version::BUILD
    )?;
    writeln!(
        out,
        "origin-boot: UEFI {} system table",
        Revision(st.hdr.revision)
    )?;
    out.write_str("origin-boot: firmware ")?;
    out.write_char16(vendor)?;
    writeln!(out, ", revision {:#010x}", st.firmware_revision)
}

/// Prints `origin-boot: <message>` and returns `status`, for `?` chains.
fn fail(out: &mut ConOut, status: Status, args: core::fmt::Arguments<'_>) -> Status {
    let _ = writeln!(out, "origin-boot: {args}");
    status
}

/// Every step from reading the kernel to entering it. Returns only on failure, with boot
/// services still active.
fn boot(
    image: Handle,
    system_table: *const SystemTable,
    st: &SystemTable,
    out: &mut ConOut,
) -> Result<core::convert::Infallible, Status> {
    let bs = boot_services(st)
        .map_err(|s| fail(out, s, format_args!("boot services table is invalid")))?;

    // B1: read the kernel image.
    let file = read_kernel(image, &bs).map_err(|s| {
        if s == Status::NOT_FOUND {
            fail(
                out,
                s,
                format_args!("no Framekernel image at {KERNEL_PATH}"),
            )
        } else {
            fail(out, s, format_args!("cannot read {KERNEL_PATH}: {s:?}"))
        }
    })?;
    let _ = writeln!(
        out,
        "origin-boot: read {KERNEL_PATH} ({} bytes)",
        file.len()
    );

    // B2: verify it.
    if DEVELOPER_MODE {
        let _ = writeln!(
            out,
            "origin-boot: developer mode: unsigned kernel images are accepted"
        );
    }
    let v = kernel::verify(file, DEVELOPER_MODE).map_err(|e| {
        fail(
            out,
            Status::SECURITY_VIOLATION,
            format_args!("kernel rejected: {e}"),
        )
    })?;
    let _ = writeln!(
        out,
        "origin-boot: kernel verified: {} sections, boot info {}.{}, entry RVA {:#x}",
        v.image.section_count(),
        v.manifest.boot_info_abi.0,
        v.manifest.boot_info_abi.1,
        v.manifest.entry
    );

    // B3: place it.
    let span =
        place::span(&v.image).map_err(|e| fail(out, Status::LOAD_ERROR, format_args!("{e:?}")))?;
    let span_pages =
        usize::try_from(span.div_ceil(PAGE as u64)).map_err(|_| Status::OUT_OF_RESOURCES)?;
    let base = allocate(&bs, memory_type::LOADER_CODE, span_pages).map_err(|s| {
        fail(
            out,
            s,
            format_args!("cannot allocate {span_pages} pages for the kernel"),
        )
    })?;
    let span_bytes = usize::try_from(span).map_err(|_| Status::OUT_OF_RESOURCES)?;
    // SAFETY: `allocate` returned `span_pages` zeroed, identity-mapped pages at `base` that
    // only this loader uses; `span_bytes` fits in them.
    let dest = unsafe {
        core::slice::from_raw_parts_mut(
            ptr::with_exposed_provenance_mut::<u8>(base as usize),
            span_bytes,
        )
    };
    let relocations = place::place(&v.image, dest, base).map_err(|e| {
        fail(
            out,
            Status::LOAD_ERROR,
            format_args!("cannot place the kernel: {e:?}"),
        )
    })?;
    let _ = writeln!(
        out,
        "origin-boot: kernel placed at {base:#x} ({span_pages} pages, {relocations} relocations)"
    );
    let entry = base
        .checked_add(v.manifest.entry)
        .ok_or(Status::LOAD_ERROR)?;

    // B4: stack, boot info and memory-map buffer, all allocated before the final map.
    let stack = allocate(&bs, memory_type::OS_KERNEL_STACK, STACK_PAGES)
        .map_err(|s| fail(out, s, format_args!("cannot allocate the kernel stack")))?;
    let (probe_size, descriptor_size) = probe_memory_map(&bs)
        .map_err(|s| fail(out, s, format_args!("cannot size the memory map: {s:?}")))?;
    let capacity = probe_size / descriptor_size.max(1) + MAP_SLACK;
    let info_bytes = size_of::<BootInfo>() + capacity * size_of::<MemoryRegion>();
    let info_pages = info_bytes.div_ceil(PAGE);
    let info_addr = allocate(&bs, memory_type::OS_BOOT_INFO, info_pages)
        .map_err(|s| fail(out, s, format_args!("cannot allocate the boot info")))?;
    let map_bytes = probe_size + MAP_SLACK * descriptor_size;
    let map_pages = map_bytes.div_ceil(PAGE);
    let map_addr = allocate(&bs, memory_type::LOADER_DATA, map_pages).map_err(|s| {
        fail(
            out,
            s,
            format_args!("cannot allocate the memory-map buffer"),
        )
    })?;
    let rsdp = find_rsdp(st);

    let _ = writeln!(
        out,
        "origin-boot: exiting boot services; entering the Framekernel"
    );
    PANIC_CONSOLE.store(ptr::null_mut(), Ordering::Release);

    // B5: final memory map, translation, ExitBootServices (one retry, UEFI 2.11 §7.4).
    let regions_addr = info_addr + size_of::<BootInfo>() as u64;
    // SAFETY: `allocate` zeroed `info_pages` pages at `info_addr`; the region array starts
    // after the `BootInfo`, is 8-byte aligned, and `capacity` entries fit in those pages.
    let regions = unsafe {
        core::slice::from_raw_parts_mut(
            ptr::with_exposed_provenance_mut::<MemoryRegion>(regions_addr as usize),
            capacity,
        )
    };
    // SAFETY: `allocate` zeroed `map_pages` pages at `map_addr`, which only this loader uses.
    let map = unsafe {
        core::slice::from_raw_parts_mut(
            ptr::with_exposed_provenance_mut::<u8>(map_addr as usize),
            map_pages * PAGE,
        )
    };
    let get_map = bs.get_memory_map.ok_or(Status::UNSUPPORTED)?;
    let exit = bs.exit_boot_services.ok_or(Status::UNSUPPORTED)?;
    let mut last = Status::INVALID_PARAMETER;
    for _attempt in 0..2 {
        let mut size = map.len();
        let mut key = 0usize;
        let mut dsize = 0usize;
        let mut dversion = 0u32;
        // SAFETY: valid `GetMemoryMap` from the validated boot services, which are still
        // active; every pointer is to a live local or to the `map` buffer of `size` bytes.
        let status = unsafe {
            get_map(
                &mut size,
                map.as_mut_ptr(),
                &mut key,
                &mut dsize,
                &mut dversion,
            )
        };
        if status.is_error() {
            return Err(fail(
                out,
                status,
                format_args!("GetMemoryMap failed: {status:?}"),
            ));
        }
        let raw = map.get(..size).unwrap_or(&[]);
        let n = memmap::translate(raw, dsize, regions).map_err(|e| {
            fail(
                out,
                Status::LOAD_ERROR,
                format_args!("memory map rejected: {e:?}"),
            )
        })?;
        // The kernel image is EfiLoaderCode (executable under the firmware's NX policy), so
        // it is marked as KernelImage by address (ADR-0002 B3).
        let n = memmap::carve(
            regions,
            n,
            base,
            span_pages as u64,
            origin_boot_info::RegionKind::KernelImage,
        )
        .map_err(|e| {
            fail(
                out,
                Status::LOAD_ERROR,
                format_args!("kernel region: {e:?}"),
            )
        })?;
        let info = BootInfo {
            magic: origin_boot_info::MAGIC,
            version_major: origin_boot_info::VERSION.0,
            version_minor: origin_boot_info::VERSION.1,
            size: size_of::<BootInfo>() as u32,
            regions: regions_addr,
            region_count: n as u64,
            kernel_base: base,
            kernel_span: span,
            acpi_rsdp: rsdp,
            uefi_system_table: system_table as u64,
            stack_base: stack,
            stack_size: (STACK_PAGES * PAGE) as u64,
        };
        // SAFETY: `info_addr` is the start of this loader's zeroed, 4 KiB-aligned boot-info
        // allocation, large enough for a `BootInfo`.
        unsafe { ptr::with_exposed_provenance_mut::<BootInfo>(info_addr as usize).write(info) };
        // SAFETY: valid `ExitBootServices` with this image's handle and the key of the map
        // just obtained; nothing has been allocated since `GetMemoryMap`.
        last = unsafe { exit(image, key) };
        if !last.is_error() {
            let stack_top = stack + (STACK_PAGES * PAGE) as u64;
            // SAFETY: boot services have exited; `entry` is the verified entry point inside
            // the placed RX segment (ADR-0002 B2, B3); `stack_top` is the 16-byte-aligned
            // top of the kernel stack; `info_addr` holds a complete `BootInfo`.
            unsafe { enter_kernel(entry, info_addr, stack_top) }
        }
        if last != Status::INVALID_PARAMETER {
            break;
        }
    }
    Err(fail(
        out,
        last,
        format_args!("ExitBootServices failed: {last:?}"),
    ))
}

/// Validates the boot-services table and returns a copy of it.
fn boot_services(st: &SystemTable) -> Result<BootServices, Status> {
    let p = st.boot_services.cast::<BootServices>().cast_const();
    if p.is_null() || !p.is_aligned() {
        return Err(Status::INVALID_PARAMETER);
    }
    // SAFETY: non-null and aligned, from the validated system table; the firmware keeps the
    // table readable while boot services are active. Every field is a raw pointer or an
    // integer, so any bit pattern is valid.
    let hdr = unsafe { p.cast::<TableHeader>().read() };
    if hdr.signature != BOOT_SERVICES_SIGNATURE
        || (hdr.header_size as usize) < size_of::<BootServices>()
    {
        return Err(Status::INCOMPATIBLE_VERSION);
    }
    // SAFETY: as above, and `HeaderSize` covers every member read.
    Ok(unsafe { p.read() })
}

/// `AllocatePages(AllocateAnyPages, memory_type, pages)`, zero-filled.
fn allocate(bs: &BootServices, memory_type: u32, pages: usize) -> Result<u64, Status> {
    let f = bs.allocate_pages.ok_or(Status::UNSUPPORTED)?;
    let bytes = pages.checked_mul(PAGE).ok_or(Status::OUT_OF_RESOURCES)?;
    let mut addr = 0u64;
    // SAFETY: valid `AllocatePages` from the validated boot services, which are active;
    // `addr` is a live local.
    let status = unsafe { f(ALLOCATE_ANY_PAGES, memory_type, pages, &mut addr) };
    if status.is_error() {
        return Err(status);
    }
    // SAFETY: the firmware just gave this loader `pages` pages at `addr`; UEFI identity-maps
    // memory on x86-64 (UEFI 2.11 §2.3.4), so the address is directly writable.
    unsafe { ptr::with_exposed_provenance_mut::<u8>(addr as usize).write_bytes(0, bytes) };
    Ok(addr)
}

/// Locates a protocol on `handle` with `HandleProtocol`.
fn protocol<T>(bs: &BootServices, handle: Handle, guid: &Guid) -> Result<*mut T, Status> {
    let f = bs.handle_protocol.ok_or(Status::UNSUPPORTED)?;
    let mut iface: *mut c_void = ptr::null_mut();
    // SAFETY: valid `HandleProtocol` from the validated boot services, which are active;
    // `guid` and `iface` are live.
    let status = unsafe { f(handle, guid, &mut iface) };
    if status.is_error() || iface.is_null() {
        return Err(if status.is_error() {
            status
        } else {
            Status::NOT_FOUND
        });
    }
    Ok(iface.cast())
}

/// Reads `KERNEL_PATH` from the volume this loader was loaded from into fresh pages.
fn read_kernel(image: Handle, bs: &BootServices) -> Result<&'static [u8], Status> {
    let loaded = protocol::<LoadedImageProtocol>(bs, image, &LOADED_IMAGE_PROTOCOL)?;
    // SAFETY: `HandleProtocol` returned a live `EFI_LOADED_IMAGE_PROTOCOL` for this image.
    let device = unsafe { (*loaded).device_handle };
    let fs = protocol::<SimpleFileSystemProtocol>(bs, device, &SIMPLE_FILE_SYSTEM_PROTOCOL)?;
    // SAFETY: `HandleProtocol` returned a live `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL`.
    let open_volume = unsafe { (*fs).open_volume }.ok_or(Status::UNSUPPORTED)?;
    let mut root: *mut FileProtocol = ptr::null_mut();
    // SAFETY: valid `OpenVolume` of a live protocol instance; `root` is a live local.
    let status = unsafe { open_volume(fs, &mut root) };
    if status.is_error() || root.is_null() {
        return Err(Status::NOT_FOUND);
    }
    let result = read_file(bs, root);
    // SAFETY: `root` is the open volume handle from `OpenVolume`.
    if let Some(close) = unsafe { (*root).close } {
        // SAFETY: closing the handle this function opened; it is not used afterwards.
        let _ = unsafe { close(root) };
    }
    result
}

/// Opens `KERNEL_PATH` under `root`, reads all of it, and closes it.
fn read_file(bs: &BootServices, root: *mut FileProtocol) -> Result<&'static [u8], Status> {
    let mut path = [0u16; 64];
    for (dst, b) in path.iter_mut().zip(KERNEL_PATH.bytes()) {
        *dst = u16::from(b);
    }
    // SAFETY: `root` is a live file handle from `OpenVolume`.
    let open = unsafe { (*root).open }.ok_or(Status::UNSUPPORTED)?;
    let mut file: *mut FileProtocol = ptr::null_mut();
    // SAFETY: valid `Open` on a live handle; `path` is NUL-terminated (the array is longer
    // than the ASCII path); `file` is a live local.
    let status = unsafe { open(root, &mut file, path.as_ptr(), FILE_MODE_READ, 0) };
    if status.is_error() || file.is_null() {
        return Err(if status.is_error() {
            status
        } else {
            Status::NOT_FOUND
        });
    }
    let result = read_open_file(bs, file);
    // SAFETY: `file` is the handle `Open` just returned.
    if let Some(close) = unsafe { (*file).close } {
        // SAFETY: closing the handle this function opened; it is not used afterwards.
        let _ = unsafe { close(file) };
    }
    result
}

fn read_open_file(bs: &BootServices, file: *mut FileProtocol) -> Result<&'static [u8], Status> {
    // SAFETY: `file` is a live file handle from `Open`.
    let get_info = unsafe { (*file).get_info }.ok_or(Status::UNSUPPORTED)?;
    let mut info = [0u64; 64]; // EFI_FILE_INFO plus a file name, 8-byte aligned
    let mut info_size = size_of::<[u64; 64]>();
    // SAFETY: valid `GetInfo` on a live handle; `info` is a live, aligned buffer of
    // `info_size` bytes.
    let status = unsafe { get_info(file, &FILE_INFO, &mut info_size, info.as_mut_ptr().cast()) };
    if status.is_error() {
        return Err(status);
    }
    let file_size = info
        .get(FILE_INFO_FILE_SIZE_OFFSET / 8)
        .copied()
        .ok_or(Status::LOAD_ERROR)?;
    if file_size == 0 || file_size > MAX_KERNEL_FILE {
        return Err(Status::LOAD_ERROR);
    }
    let len = usize::try_from(file_size).map_err(|_| Status::LOAD_ERROR)?;
    let addr = allocate(bs, memory_type::LOADER_DATA, len.div_ceil(PAGE))?;
    // SAFETY: `file` is a live handle.
    let read = unsafe { (*file).read }.ok_or(Status::UNSUPPORTED)?;
    let mut done = 0usize;
    while done < len {
        let mut chunk = len - done;
        let dst = ptr::with_exposed_provenance_mut::<u8>(addr as usize + done);
        // SAFETY: valid `Read` on a live handle into this loader's zeroed allocation, which
        // has at least `len - done` bytes left at `dst`.
        let status = unsafe { read(file, &mut chunk, dst.cast()) };
        if status.is_error() {
            return Err(status);
        }
        if chunk == 0 {
            return Err(Status::LOAD_ERROR); // end of file before FileSize bytes
        }
        done += chunk;
    }
    // SAFETY: the `len` bytes at `addr` were zeroed by `allocate` and then filled by `Read`;
    // the pages stay allocated for the rest of the boot.
    Ok(unsafe {
        core::slice::from_raw_parts(ptr::with_exposed_provenance::<u8>(addr as usize), len)
    })
}

/// Sizes the memory map: returns (bytes needed, descriptor size).
fn probe_memory_map(bs: &BootServices) -> Result<(usize, usize), Status> {
    let f = bs.get_memory_map.ok_or(Status::UNSUPPORTED)?;
    let (mut size, mut key, mut dsize, mut dversion) = (0usize, 0usize, 0usize, 0u32);
    // SAFETY: valid `GetMemoryMap`; a zero size with a null buffer asks only for the size.
    let status = unsafe {
        f(
            &mut size,
            ptr::null_mut(),
            &mut key,
            &mut dsize,
            &mut dversion,
        )
    };
    if status != Status::BUFFER_TOO_SMALL || dsize < memmap::DESCRIPTOR_MIN_SIZE {
        return Err(if status.is_error() {
            status
        } else {
            Status::LOAD_ERROR
        });
    }
    Ok((size, dsize))
}

/// The ACPI 2.0+ RSDP from the configuration table, or 0.
fn find_rsdp(st: &SystemTable) -> u64 {
    let table = st
        .configuration_table
        .cast::<ConfigurationTable>()
        .cast_const();
    if table.is_null() || !table.is_aligned() {
        return 0;
    }
    for i in 0..st.number_of_table_entries.min(256) {
        // Computing the address is safe; only the read below needs the table's contract.
        let at = table.wrapping_add(i);
        // SAFETY: the configuration table from the validated system table holds
        // `NumberOfTableEntries` entries (UEFI 2.11 §4.3); `i` is below that count, so `at`
        // is in bounds and aligned (checked for the base above).
        let entry = unsafe { at.read() };
        if entry.vendor_guid == ACPI_20_TABLE {
            return entry.vendor_table as u64;
        }
    }
    0
}

/// Enters the kernel: interrupts off, the kernel stack, `call entry` with `rdi` = boot info
/// (System V, ADR-0002 B6). Halts if the kernel ever returns.
///
/// # Safety
///
/// Boot services must have exited. `entry` must be the verified entry point of the placed
/// kernel, `stack_top` the 16-byte-aligned top of its stack, and `boot_info` the address of
/// a complete `BootInfo`.
unsafe fn enter_kernel(entry: u64, boot_info: u64, stack_top: u64) -> ! {
    // SAFETY: the caller guarantees the preconditions. The sequence never returns: the
    // stack is replaced, and a `hlt` loop follows the call.
    unsafe {
        core::arch::asm!(
            "cli",
            "cld",
            "mov rsp, {stack}",
            "xor ebp, ebp",
            "call {entry}",
            "2:",
            "cli",
            "hlt",
            "jmp 2b",
            stack = in(reg) stack_top,
            entry = in(reg) entry,
            in("rdi") boot_info,
            options(noreturn),
        )
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let p = PANIC_CONSOLE.load(Ordering::Acquire);
    // SAFETY: `p` is null or the console stored from the validated system table; it is
    // cleared before `ExitBootServices`, so it is only non-null while boot services run.
    if let Some(mut out) = unsafe { ConOut::new(p) } {
        let _ = writeln!(out, "\norigin-boot: panic: {info}");
    }
    loop {
        core::hint::spin_loop();
    }
}
