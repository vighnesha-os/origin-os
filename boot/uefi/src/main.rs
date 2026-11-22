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
// TASKS item 128 (ADR-0002 Amendment 5): a minimal-mode bench build never reaches the full path
// (`boot` and what only it calls), which the linker then leaves out of the image.
#![cfg_attr(origin_bench_minimal, allow(dead_code))]

// TASKS item 131's review, R131-11: the two modes are one or the other, never both.
#[cfg(all(origin_bench_minimal, origin_bench_bisect))]
compile_error!("a bench loader is built in the minimal mode or the bisection mode, not both");

use core::ffi::c_void;
use core::fmt::Write;
use core::mem::size_of;
use core::panic::PanicInfo;
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

use origin_boot_info::{
    BOOT_SECRET_LEN, BOOT_SEED_LEN, BootInfo, EarlyUart, FLAG_KASLR, Framebuffer, KernelSegment,
    MAX_KERNEL_SEGMENTS, MemoryRegion,
};
#[cfg(origin_bench)]
use origin_boot_uefi::bench;
use origin_boot_uefi::console::ConOut;
#[cfg(origin_bench)]
use origin_boot_uefi::fch;
use origin_boot_uefi::protocols::{
    ACPI_20_TABLE, ALLOCATE_ANY_PAGES, BOOT_SERVICES_SIGNATURE, BootServices, CONSOLE_OUT_DEVICE,
    ConfigurationTable, FILE_INFO, FILE_INFO_FILE_SIZE_OFFSET, FILE_MODE_READ, FileProtocol,
    GRAPHICS_OUTPUT_PROTOCOL, GetMemoryMapFn, GraphicsOutputProtocol, Guid, LOADED_IMAGE_PROTOCOL,
    LOCATE_BY_PROTOCOL, LoadedImageProtocol, SIMPLE_FILE_SYSTEM_PROTOCOL, SimpleFileSystemProtocol,
    memory_type,
};
use origin_boot_uefi::uefi::{
    self, ExitOutcome, Handle, Revision, SimpleTextOutputProtocol, Status, SystemTable, TableHeader,
};
use origin_boot_uefi::{framebuffer, kernel, memmap, place, spcr, trust, uart};
use origin_paging::layout::{self, Placement};
use origin_paging::{Access, Attributes, Table, TablePool};

mod random;

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
/// The kernel stack's and the boot info's memory types: B3's OS-defined types, or, in the
/// bisection mode only, `EfiLoaderData` (ADR-0002 Amendment 6, step 2).
#[cfg(not(origin_bench_bisect))]
const STACK_TYPE: u32 = memory_type::OS_KERNEL_STACK;
#[cfg(not(origin_bench_bisect))]
const BOOT_INFO_TYPE: u32 = memory_type::OS_BOOT_INFO;
#[cfg(origin_bench_bisect)]
const STACK_TYPE: u32 = memory_type::LOADER_DATA;
#[cfg(origin_bench_bisect)]
const BOOT_INFO_TYPE: u32 = memory_type::LOADER_DATA;

/// Image entry point; the `x86_64-unknown-uefi` target links it as the PE entry.
///
/// # Safety
///
/// Only the firmware may call this, as the image entry point (UEFI 2.11 §4.1), with boot
/// services active, on the boot processor. `image` and `system_table` must be the values
/// the firmware passes there; the loader validates the table before use.
#[unsafe(no_mangle)]
pub unsafe extern "efiapi" fn efi_main(image: Handle, system_table: *const SystemTable) -> Status {
    // TASKS item 127, a measurement build only: the stack UEFI promises is painted first, from
    // this function's own RSP, and calibrated.
    #[cfg(origin_stack_measure)]
    measure::begin(measure::rsp());
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
    #[cfg(origin_stack_measure)]
    measure::say_calibration(&mut out);
    // TASKS item 128 (decision item 347, ADR-0002 Amendment 5): a minimal-mode bench build does
    // only what the mode lists.
    #[cfg(origin_bench_minimal)]
    let status = minimal::run(image, system_table, &st, &mut out);
    #[cfg(not(origin_bench_minimal))]
    let status = match boot(image, system_table, &st, &mut out) {
        Ok(never) => match never {},
        Err(status) => status,
    };
    status
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
    // TASKS item 131 (decision item 352, ADR-0002 Amendment 6): the bisection mode says so first.
    #[cfg(origin_bench_bisect)]
    let _ = writeln!(out, "{}", bench::BISECT_LINE);

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
    // TASKS item 112: in a bench build, whether a QEMU scenario asks for stale map keys. A
    // rented machine's volume never holds the file, so its boots take their natural path.
    #[cfg(origin_bench)]
    let stale = {
        let switch = read_bench_file(image, &bs, bench::STALE_KEY_PATH)
            .map(|(contents, len)| bench::stale_calls(contents.get(..len).unwrap_or(&[])));
        let (stale, said) = match switch {
            None => (
                0,
                "no BENCH-STALE-KEY: ExitBootServices gets the firmware's own key",
            ),
            Some(Some(1)) => (
                1,
                "BENCH-STALE-KEY: the first ExitBootServices call gets a stale key",
            ),
            Some(Some(_)) => (
                2,
                "BENCH-STALE-KEY: both ExitBootServices calls get a stale key",
            ),
            Some(None) => (
                0,
                "BENCH-STALE-KEY not understood: ExitBootServices gets the firmware's own key",
            ),
        };
        let _ = writeln!(out, "{}{said}", bench::PREFIX);
        stale
    };
    // TASKS item 116: what a bench loader does once `ExitBootServices` has returned. A rented
    // machine's volume never holds the file, so a rental resets the machine and never enters the
    // kernel (`bench::After::Reset`).
    #[cfg(origin_bench)]
    let after = {
        let found = read_bench_file(image, &bs, bench::AFTER_PATH)
            .map(|(contents, len)| bench::After::from_contents(contents.get(..len).unwrap_or(&[])));
        let _ = writeln!(out, "{}{}", bench::PREFIX, bench::After::said(found));
        // The bisection mode reads it as the full loader does, and ignores its value (ruling Q3 of
        // TASKS item 131).
        #[cfg(origin_bench_bisect)]
        let _ = writeln!(out, "{}", bench::BISECT_AFTER_IGNORED);
        found.flatten().unwrap_or(bench::After::Reset)
    };
    // TASKS item 121: values in place of the FCH's registers, for QEMU scenarios only. A rented
    // machine's volume never holds the file, so a rental reads the registers where the firmware's
    // tables allow it.
    #[cfg(all(origin_bench, not(origin_bench_bisect)))]
    let pretend = {
        let found = read_bench_file(image, &bs, fch::SWITCH_PATH)
            .map(|(contents, len)| fch::Pretend::from_contents(contents.get(..len).unwrap_or(&[])));
        let _ = writeln!(out, "{}{}", bench::PREFIX, fch::Pretend::said(found));
        found.flatten()
    };
    // The bisection mode reads the same file with the same calls; its `mine` names a shutdown,
    // and `noreset` keeps `ResetSystem` out of Amendment 3's order (ruling Q2 of TASKS item 131).
    #[cfg(origin_bench_bisect)]
    let (pretend, after) = {
        let found = read_bench_file(image, &bs, fch::SWITCH_PATH).map(|(contents, len)| {
            fch::BisectSwitch::from_contents(contents.get(..len).unwrap_or(&[]))
        });
        let said = found.map(|s| s.map(|s| s.pretend));
        let _ = writeln!(out, "{}{}", bench::PREFIX, fch::Pretend::said(said));
        let switch = found.flatten();
        let _ = after;
        let after = if switch.is_some_and(|s| s.no_reset_system) {
            bench::After::SwitchNoReset
        } else {
            bench::After::Reset
        };
        if after == bench::After::SwitchNoReset {
            let _ = writeln!(
                out,
                "{}{}",
                bench::PREFIX,
                bench::After::said(Some(Some(after)))
            );
        }
        (switch.map(|s| s.pretend), after)
    };

    #[cfg(origin_stack_measure)]
    measure::phase_end(measure::Phase::Read, out);
    // B2: verify it.
    if DEVELOPER_MODE {
        let _ = writeln!(
            out,
            "origin-boot: developer mode: unsigned kernel images are accepted"
        );
    }
    if trust::HOLDS_TEST_KEY {
        let _ = writeln!(
            out,
            "origin-boot: TEST trust store: this loader trusts a development key and must never ship"
        );
    }
    let v = kernel::verify(file, DEVELOPER_MODE, trust::STORE).map_err(|e| {
        fail(
            out,
            Status::SECURITY_VIOLATION,
            format_args!("kernel rejected: {e}"),
        )
    })?;
    if let Some(id) = v.signer {
        let [a, b, c, d, e, f, g, h, ..] = id;
        let short = u64::from_be_bytes([a, b, c, d, e, f, g, h]);
        let _ = writeln!(
            out,
            "origin-boot: kernel signature verified: origin-kernel, Ed25519 and ML-DSA-87 both good, key ID {short:016x}..."
        );
    }
    let _ = writeln!(
        out,
        "origin-boot: kernel verified: {} sections, boot info {}.{}, entry RVA {:#x}",
        v.image.section_count(),
        v.manifest.boot_info_abi.0,
        v.manifest.boot_info_abi.1,
        v.manifest.entry
    );

    #[cfg(origin_stack_measure)]
    measure::phase_end(measure::Phase::Verify, out);
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
    // ADR-0016 R10: the loader's own random-number generator, started before the kernel's
    // place is chosen: a known-answer test, a start-up test of each instruction the processor
    // reports, and a first seed from their draws.
    let storage = allocate(&bs, memory_type::LOADER_DATA, random::PAGES).map_err(|s| {
        fail(
            out,
            s,
            format_args!("cannot allocate the random-number generator's storage"),
        )
    })?;
    let rdseed = cpuid(0)[0] >= 7 && cpuid(7)[1] & (1 << 18) != 0;
    let rdrand = cpuid(1)[2] & (1 << 30) != 0;
    // SAFETY: `allocate` returned `random::PAGES` zeroed, identity-mapped pages at `storage`
    // that only this generator uses, until it is finished below.
    let (mut generator, started) = unsafe { random::Generator::start(storage, rdseed, rdrand) };
    let _ = writeln!(
        out,
        "origin-boot: random: known-answer test {}; RDSEED {}, RDRAND {}; {}; the firmware's generator is not asked in this build",
        if started.self_test {
            "passed"
        } else {
            "FAILED"
        },
        random::condition_name(started.rdseed),
        random::condition_name(started.rdrand),
        if started.seeded {
            "the loader's generator is seeded"
        } else {
            "no generator: the kernel is placed at a fixed address and is handed no seed"
        }
    );
    // ADR-0004 H3, as ADR-0016 R10 amends it: two slides, two requests. The kernel's virtual
    // slot is chosen now, because the relocations depend on it; the direct-map slot waits for
    // the memory map.
    let kernel_slide = generator.next_u64();
    let (_, kernel_virtual) =
        layout::place(kernel_slide.unwrap_or(0), 0, span).ok_or(Status::LOAD_ERROR)?;
    let relocations = place::place(&v.image, dest, kernel_virtual).map_err(|e| {
        fail(
            out,
            Status::LOAD_ERROR,
            format_args!("cannot place the kernel: {e:?}"),
        )
    })?;
    let _ = writeln!(
        out,
        "origin-boot: kernel placed at {base:#x}, virtual {kernel_virtual:#x} ({span_pages} pages, {relocations} relocations)"
    );
    let entry = kernel_virtual
        .checked_add(v.manifest.entry)
        .ok_or(Status::LOAD_ERROR)?;

    #[cfg(origin_stack_measure)]
    measure::phase_end(measure::Phase::Place, out);
    // B4 (boot info 0.2): the kernel's segment table, so it can map its own image with the
    // permissions the verified image declares.
    let mut segments = [KernelSegment {
        rva: 0,
        pages: 0,
        access: 0,
        reserved: 0,
    }; MAX_KERNEL_SEGMENTS as usize];
    let segment_count = place::kernel_segments(&v.image, &mut segments).map_err(|e| {
        fail(
            out,
            Status::LOAD_ERROR,
            format_args!("kernel segments: {e:?}"),
        )
    })?;
    let segments = segments.get(..segment_count).unwrap_or(&[]);
    origin_boot_info::check_segments(segments, span).map_err(|e| {
        fail(
            out,
            Status::LOAD_ERROR,
            format_args!("kernel segments rejected: {e:?}"),
        )
    })?;

    // B4: stack, boot info and memory-map buffer, all allocated before the final map.
    // One extra page below the stack, in the same KernelStack region: the kernel leaves it
    // unmapped as a guard (boot info 0.2).
    let stack = allocate(&bs, STACK_TYPE, STACK_PAGES + 1)
        .map_err(|s| fail(out, s, format_args!("cannot allocate the kernel stack")))?;
    let stack = stack + PAGE as u64;
    let (probe_size, descriptor_size) = probe_memory_map(&bs)
        .map_err(|s| fail(out, s, format_args!("cannot size the memory map: {s:?}")))?;
    let capacity = probe_size / descriptor_size.max(1) + MAP_SLACK;
    let segments_bytes = MAX_KERNEL_SEGMENTS as usize * size_of::<KernelSegment>();
    // After the region array, the secret area the kernel is handed (boot info 0.6): the seed
    // for its generator, then the value its stacks' region is placed from.
    let info_bytes = size_of::<BootInfo>()
        + segments_bytes
        + capacity * size_of::<MemoryRegion>()
        + BOOT_SECRET_LEN as usize;
    let info_pages = info_bytes.div_ceil(PAGE);
    let info_addr = allocate(&bs, BOOT_INFO_TYPE, info_pages)
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
    // The firmware's framebuffer, for the kernel's console from its first line on (ADR-0010).
    // The memory map says below whether it can be used; missing or unusable, the boot goes on.
    let mut framebuffer = find_framebuffer(&bs, out);

    #[cfg(origin_stack_measure)]
    measure::phase_end(measure::Phase::Allocate, out);
    // B5: final memory map, translation, ExitBootServices (one retry, UEFI 2.11 §7.4).
    // Layout: BootInfo, then MAX_KERNEL_SEGMENTS segment slots, then the region array.
    let segments_addr = info_addr + size_of::<BootInfo>() as u64;
    // SAFETY: `allocate` zeroed `info_pages` pages at `info_addr`; the segment slots start
    // after the `BootInfo`, are 8-byte aligned, and fit in those pages (`info_bytes`).
    let segment_slots = unsafe {
        core::slice::from_raw_parts_mut(
            ptr::with_exposed_provenance_mut::<KernelSegment>(segments_addr as usize),
            MAX_KERNEL_SEGMENTS as usize,
        )
    };
    for (slot, segment) in segment_slots.iter_mut().zip(segments) {
        *slot = *segment;
    }
    let regions_addr = segments_addr + segments_bytes as u64;
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

    // ADR-0004 H1: the tables below are 4-level, so 5-level paging left on is refused here,
    // while there is still a console to say so.
    if place::five_level_paging(read_cr4()) {
        return Err(fail(
            out,
            Status::UNSUPPORTED,
            format_args!(
                "5-level paging is on (CR4.LA57) and this loader builds 4-level page tables only"
            ),
        ));
    }
    // ADR-0004 H4: the first higher-half tables, from a memory map taken now (everything
    // the kernel needs is already allocated, and building the tables calls no firmware).
    if !nx_supported() {
        return Err(fail(
            out,
            Status::UNSUPPORTED,
            format_args!("the processor has no NX bit"),
        ));
    }
    // TASKS item 116: the FADT's reset register, read in the same walk in a bench build.
    #[cfg(origin_bench)]
    let mut reset_register = Err(bench::NoReset::NoMap);
    // TASKS item 121: the FCH's registers, read in the same walk in a bench build.
    #[cfg(origin_bench)]
    let mut fch_seen = FchSeen::not_read(fch::NotRead::NoFadt(bench::NoReset::NoMap));
    // ADR-0010 Amendment 3 (decision item 317; TASKS item 84): the UART the firmware's SPCR
    // names, for the kernel's first line. Read under boot services, only from memory the map
    // describes; no outcome stops the boot (ADR-0010 E5).
    let early_uart = match read_memory_map(get_map, map, regions) {
        Ok((_, n)) => {
            let described = regions.get(..n).unwrap_or(&[]);
            let (mut reads, mut read) = (0u32, 0usize);
            let start = random::rdtsc();
            let found = spcr::find(rsdp, &mut |at, bytes| {
                reads += 1;
                read += bytes.len();
                read_described(described, at, bytes)
            });
            let cycles = random::rdtsc().wrapping_sub(start);
            let _ = writeln!(out, "origin-boot: console: {found}");
            // What the walk cost (cost ledger row CL46): the time-stamp counter's rate is not
            // known here, so it is counted, not timed.
            let _ = writeln!(
                out,
                "origin-boot: console: the walk read {read} bytes of firmware memory in {reads} reads, {cycles} time-stamp counter ticks (cost ledger row CL46)"
            );
            #[cfg(origin_bench)]
            {
                reset_register = bench::find_reset(
                    rsdp,
                    &mut |at, bytes| read_described(described, at, bytes),
                    |at| bench::in_mmio(described, at),
                );
                fch_seen = bench_fch_read(rsdp, described, pretend, false, out);
                // The bisection mode's `mine` names a shutdown (bit 23), as the minimal mode's.
                #[cfg(origin_bench_bisect)]
                if let Some(p) = fch_seen.pretend {
                    fch_seen.registers = Some(p.values_minimal(fch_seen.pm1a_event));
                }
            }
            found.early_uart()
        }
        Err(_) => {
            let _ = writeln!(
                out,
                "origin-boot: console: no SPCR read (the memory map could not be read); the kernel's console starts on COM1 and the screen"
            );
            EarlyUart::NONE
        }
    };
    // The bisection mode names a register at port 0xCF9 as skipped (Amendment 6, step 5).
    #[cfg(origin_bench_bisect)]
    let reset_register = bench::bisect_register(reset_register);
    #[cfg(origin_bench)]
    let _ = writeln!(out, "{}", bench::ResetLine(reset_register));
    // The table pool, sized from the memory map as it stands: the last allocation before the
    // tables are built, so the map they are built from differs from this one by very little.
    let (_, n) = read_memory_map(get_map, map, regions).map_err(|e| e.report(out))?;
    let pool_pages = regions
        .get(..n)
        .and_then(|sizing| layout::table_frames(sizing, capacity as u64, span))
        .and_then(|frames| usize::try_from(frames).ok())
        .ok_or_else(|| {
            fail(
                out,
                Status::OUT_OF_RESOURCES,
                format_args!("the memory map is too large to size page tables for"),
            )
        })?;
    let pool_phys = allocate(&bs, memory_type::LOADER_DATA, pool_pages).map_err(|s| {
        fail(
            out,
            s,
            format_args!("cannot allocate the page-table pool ({pool_pages} pages)"),
        )
    })?;
    let seed_addr = regions_addr + (capacity * size_of::<MemoryRegion>()) as u64;
    // The bisection mode counts this map's OS-defined types: the last it translates before its
    // last line (Amendment 6, step 2; ruling Q6 of TASKS item 131).
    #[cfg(origin_bench_bisect)]
    let os_types;
    let (root, direct_map_base, table_count, kaslr) = {
        #[cfg(not(origin_bench_bisect))]
        let (_, n) = read_memory_map(get_map, map, regions).map_err(|e| e.report(out))?;
        #[cfg(origin_bench_bisect)]
        let n = {
            let (n, types) = bisect::read_memory_map_counting(get_map, map, regions)
                .map_err(|e| e.report(out))?;
            os_types = Some(types);
            n
        };
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
                format_args!("memory map rejected: {e:?}"),
            )
        })?;
        let regions = regions.get(..n).unwrap_or(&[]);
        let max_physical = regions
            .iter()
            .filter(|r| layout::direct_mapped(r.kind))
            .map(|r| r.base + r.pages * PAGE as u64)
            .max()
            .unwrap_or(0);
        let direct_slide = generator.next_u64();
        let kaslr = kernel_slide.is_some() && direct_slide.is_some();
        let (direct, _) = layout::place(direct_slide.unwrap_or(0), max_physical, span)
            .ok_or(Status::OUT_OF_RESOURCES)?;
        let framebuffer_map = layout::framebuffer_mapping(&framebuffer).filter(|m| {
            let in_ram = origin_boot_info::overlaps_ram(regions, m.phys, m.bytes);
            if in_ram {
                let _ = writeln!(
                    out,
                    "origin-boot: framebuffer at {:#x}: it overlaps RAM in the memory map; not used",
                    framebuffer.base
                );
            }
            !in_ram
        });
        if framebuffer_map.is_none() {
            framebuffer = Framebuffer::NONE;
        }
        // SAFETY: `allocate` zeroed `pool_pages` identity-mapped pages at `pool_phys` that only
        // this pool uses; `Table` is 4 KiB of `AtomicU64`, valid for any bits.
        let tables = unsafe {
            core::slice::from_raw_parts(
                ptr::with_exposed_provenance::<Table>(pool_phys as usize),
                pool_pages,
            )
        };
        let mut pool = TablePool::new(tables, pool_phys);
        let at = Placement {
            direct_map_base: direct,
            kernel_virtual_base: kernel_virtual,
            kernel_physical_base: base,
            stack_guard: stack - PAGE as u64,
        };
        let (mut space, stats) = layout::build(&mut pool, &at, regions, segments)
            .map_err(|e| fail(out, Status::LOAD_ERROR, format_args!("page tables: {e:?}")))?;
        // The entry stub runs its last instructions after loading CR3: map its pages at
        // their identity address, read/execute, in the new tables.
        #[cfg(not(origin_bench))]
        let stub = (enter_kernel as *const ()).addr() as u64 & !(PAGE as u64 - 1);
        #[cfg(origin_bench)]
        let stub = (enter_kernel_bench as *const ()).addr() as u64 & !(PAGE as u64 - 1);
        for page in [stub, stub + PAGE as u64] {
            space
                .map(
                    &mut pool,
                    page,
                    page,
                    Attributes::kernel(Access::ReadExecute),
                )
                .map_err(|e| fail(out, Status::LOAD_ERROR, format_args!("entry stub: {e:?}")))?;
        }
        // Mapped last, so running out of tables here costs only the framebuffer (ADR-0010).
        match framebuffer_map {
            Some(m) if layout::map_framebuffer(&mut space, &mut pool, &m) => {
                framebuffer.virtual_base = m.first_pixel;
                let _ = writeln!(
                    out,
                    "origin-boot: framebuffer: {}x{} at {:#x}, stride {}, red {:#x} green {:#x} blue {:#x}; mapped at {:#x}",
                    framebuffer.width,
                    framebuffer.height,
                    framebuffer.base,
                    framebuffer.stride,
                    framebuffer.red_mask,
                    framebuffer.green_mask,
                    framebuffer.blue_mask,
                    framebuffer.virtual_base
                );
            }
            Some(m) => {
                let _ = writeln!(
                    out,
                    "origin-boot: framebuffer at {:#x}: could not map {} bytes; not used",
                    m.phys, m.bytes
                );
                framebuffer = Framebuffer::NONE;
            }
            None => {}
        }
        let _ = writeln!(
            out,
            "origin-boot: KASLR: kernel at {kernel_virtual:#x}, direct map at {direct:#x} ({})",
            if kaslr {
                "two requests to the loader's generator"
            } else {
                "no generator: fixed placement"
            }
        );
        let _ = writeln!(
            out,
            "origin-boot: page tables built: {} tables, {} MiB direct map",
            pool.used(),
            stats.direct_bytes >> 20
        );
        (space.root(), direct, pool.used(), kaslr)
    };
    let _ = table_count;
    // ADR-0016 R10 step 4 and Amendment 1 (decision item 166): last before `ExitBootServices`,
    // the secret area goes into the boot-info region: 64 bytes for the kernel's generator and,
    // as a request of its own, the 8 bytes its stacks' region is placed from. Then the loader's
    // generator is uninstantiated and overwritten with its stack.
    // SAFETY: `allocate` zeroed `info_pages` pages at `info_addr`; these 72 bytes lie after the
    // region array's `capacity` entries and inside those pages (`info_bytes`), and nothing else
    // refers to them.
    let secret = unsafe {
        core::slice::from_raw_parts_mut(
            ptr::with_exposed_provenance_mut::<u8>(seed_addr as usize),
            BOOT_SECRET_LEN as usize,
        )
    };
    let (seed, stack_value) = secret.split_at_mut(BOOT_SEED_LEN as usize);
    // Both or neither: a kernel that is told of an area finds all of it.
    let handed = generator.fill(seed) && generator.fill(stack_value);
    if !handed {
        seed.fill(0);
        stack_value.fill(0);
    }
    let boot_seed = if handed { seed_addr } else { 0 };
    if !generator.finish() {
        return Err(fail(
            out,
            Status::LOAD_ERROR,
            format_args!("the random-number generator ran over its stack: nothing it gave is used"),
        ));
    }
    let _ = writeln!(
        out,
        "origin-boot: random: {}; the loader's generator is uninstantiated, and its state, seed area and stack are overwritten",
        if boot_seed == 0 {
            "no seed for the kernel"
        } else {
            "64 bytes for the kernel's generator and 8 for its stacks' region are in the boot info"
        }
    );
    #[cfg(origin_stack_measure)]
    measure::phase_end(measure::Phase::Walks, out);
    // TASKS item 112: the processor state the kernel inherits, in a bench build.
    #[cfg(origin_bench)]
    say_entry_state(out);
    // TASKS item 116: the UART as the firmware left it, a line straight to it, and how the reset
    // will go.
    // ADR-0002 Amendment 4's addition (item 348): the live boot services table against the copy,
    // before the last line.
    #[cfg(origin_bench)]
    let _ = writeln!(out, "{}", bench_table_line(system_table, st, &bs));
    #[cfg(origin_bench_bisect)]
    let _ = writeln!(out, "{}", bench::OsTypesLine(os_types));
    #[cfg(origin_bench)]
    let plan = bench_before_call(st, &bs, early_uart, after, reset_register, fch_seen, out);
    #[cfg(not(origin_bench_bisect))]
    let _ = writeln!(
        out,
        "origin-boot: exiting boot services; entering the Framekernel"
    );
    #[cfg(origin_bench_bisect)]
    let _ = writeln!(out, "{}", bench::BISECT_LAST_LINE);
    PANIC_CONSOLE.store(ptr::null_mut(), Ordering::Release);
    // Each call's status, for the line the loader writes if it halts (decision item 342).
    let mut calls = uart::ExitCalls::default();
    let outcome: ExitOutcome<MapFailure> = uefi::exit_boot_services(
        || {
            let (key, n) = read_memory_map(get_map, map, regions)?;
            // The kernel image is EfiLoaderCode (executable under the firmware's NX policy), so
            // it is marked as KernelImage by address (ADR-0002 B3).
            let n = memmap::carve(
                regions,
                n,
                base,
                span_pages as u64,
                origin_boot_info::RegionKind::KernelImage,
            )
            .map_err(MapFailure::KernelRegion)?;
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
                kernel_segments: segments_addr,
                kernel_segment_count: segment_count as u64,
                page_table_root: root,
                direct_map_base,
                kernel_virtual_base: kernel_virtual,
                flags: if kaslr { FLAG_KASLR } else { 0 },
                framebuffer,
                boot_seed,
                early_uart,
            };
            // SAFETY: `info_addr` is the start of this loader's zeroed, 4 KiB-aligned boot-info
            // allocation, large enough for a `BootInfo`.
            unsafe { ptr::with_exposed_provenance_mut::<BootInfo>(info_addr as usize).write(info) };
            Ok(key)
        },
        |key| {
            // A bench build gives the first `stale` calls a stale key (QEMU scenarios only).
            #[cfg(origin_bench)]
            let key = bench::key_for(calls.made(), stale, key);
            // TASKS item 116: the map was read and this call is being made.
            #[cfg(origin_bench)]
            if plan.marks {
                say_on_uart(
                    early_uart,
                    if calls.made() == 0 {
                        bench::MARK_FIRST
                    } else {
                        bench::MARK_RETRY
                    },
                );
            }
            // TASKS item 127, a measurement build only: the last phase's depth, straight to the
            // SPCR's UART (port writes leave the map, and its key, as they were), before the
            // first call.
            #[cfg(origin_stack_measure)]
            if calls.made() == 0 {
                measure::before_call(early_uart);
            }
            // SAFETY: valid `ExitBootServices` with this image's handle and the key of the map
            // just obtained; nothing has been allocated since `GetMemoryMap`.
            let status = unsafe { exit(image, key) };
            // TASKS item 121 (decision item 345, ADR-0002 Amendment 4): in a run the FCH's record
            // sends to the call, a call that returned EFI_SUCCESS is followed by this reset and
            // nothing before it: no line, no register read, no write-back. The one thing between
            // is a compare of the status in RAX. After an error, the loader's retry, with this
            // same check after it; after a second error, its normal path (the coordinating
            // session's ruling on R121-3).
            #[cfg(all(origin_bench, not(origin_bench_bisect)))]
            if status == Status::SUCCESS
                && let Some(free) = plan.free
            {
                reset_without_firmware(free);
            }
            // ADR-0002 Amendment 6, step 5, as Amendment 5's step 6 and item 349: after
            // EFI_SUCCESS, the status compare, then the triple fault, with nothing between them.
            #[cfg(origin_bench_bisect)]
            if status == Status::SUCCESS {
                triple_fault();
            }
            calls.record(status);
            status
        },
    );
    match outcome {
        // The bisection mode, as the minimal mode: a status that is neither EFI_SUCCESS nor an
        // error, which UEFI 2.11 §7.4 does not give; no firmware call (item 349), a line, the halt.
        #[cfg(origin_bench_bisect)]
        ExitOutcome::Exited => {
            let _ = (direct_map_base, stack, root, entry, info_addr);
            mask_interrupts();
            say_on_uart(
                early_uart,
                "origin-boot: bench build: bisect: ExitBootServices returned neither EFI_SUCCESS nor an error: halting",
            );
            halt()
        }
        #[cfg(not(origin_bench_bisect))]
        ExitOutcome::Exited => {
            let stack_top = direct_map_base + stack + (STACK_PAGES * PAGE) as u64;
            #[cfg(origin_bench)]
            {
                mask_interrupts();
                bench_disturb(early_uart, &plan);
                let port = bench_exited(early_uart, bench::Exited { calls, stale });
                // TASKS item 116: the registers again, the firmware's values written back, and,
                // unless BENCH-AFTER asks for the kernel, the reset.
                bench_after_call(early_uart, &plan, calls);
                // SAFETY: as for `enter_kernel` below; `crumbs` names a port the SPCR named
                // and the shared parser accepted, or is 0.
                unsafe {
                    enter_kernel_bench(
                        entry,
                        direct_map_base + info_addr,
                        stack_top,
                        root,
                        origin_boot_info::bench::register(port),
                    )
                }
            }
            #[cfg(not(origin_bench))]
            // SAFETY: boot services have exited; `root` holds complete tables that map the
            // kernel image at `kernel_virtual` (so `entry`, the verified entry point inside
            // its RX segment, ADR-0002 B2), the stack and the boot info in the direct map,
            // and the entry stub at its identity address; `stack_top` is 16-byte aligned.
            unsafe {
                enter_kernel(entry, direct_map_base + info_addr, stack_top, root)
            }
        }
        // `ExitBootServices` has not been called, so the firmware's console may still be used.
        ExitOutcome::NotAttempted(failure) => Err(failure.report(out)),
        // The bisection mode, as the minimal mode (Amendment 6, step 5; ruling Q5 of TASKS item
        // 131): Amendment 2's line, then Amendment 3's order, with the FADT step skipped at port
        // 0xCF9; no UART register read or write-back after the call.
        #[cfg(origin_bench_bisect)]
        ExitOutcome::Halt => {
            mask_interrupts();
            say_halting(early_uart, calls);
            bench_reset(early_uart, &plan)
        }
        #[cfg(not(origin_bench_bisect))]
        ExitOutcome::Halt => {
            #[cfg(origin_bench)]
            mask_interrupts();
            #[cfg(origin_bench)]
            bench_disturb(early_uart, &plan);
            // Decision item 342: why, on the UART boot info would name, before it halts.
            say_halting(early_uart, calls);
            // TASKS item 116: in a bench build, as after a successful call.
            #[cfg(origin_bench)]
            bench_after_call(early_uart, &plan, calls);
            halt()
        }
    }
}

/// Why the memory map could not be used.
#[derive(Clone, Copy, Debug)]
enum MapFailure {
    /// `GetMemoryMap` failed.
    Firmware(Status),
    /// The map does not translate into boot-info regions.
    Rejected(memmap::MapError),
    /// The kernel image is not inside a single region.
    KernelRegion(memmap::MapError),
}

impl MapFailure {
    /// Says what went wrong on the console. Only while boot services are fully up.
    fn report(self, out: &mut ConOut) -> Status {
        match self {
            Self::Firmware(status) => {
                fail(out, status, format_args!("GetMemoryMap failed: {status:?}"))
            }
            Self::Rejected(e) => fail(
                out,
                Status::LOAD_ERROR,
                format_args!("memory map rejected: {e:?}"),
            ),
            Self::KernelRegion(e) => fail(
                out,
                Status::LOAD_ERROR,
                format_args!("kernel region: {e:?}"),
            ),
        }
    }
}

/// `GetMemoryMap` into `map`, then that map translated into `regions`: returns the map key and
/// the number of regions. Allocates nothing, so the key stays valid, and calls nothing but the
/// memory service, so it may run again after a failed `ExitBootServices` (UEFI 2.11 §7.4).
fn read_memory_map(
    get_map: GetMemoryMapFn,
    map: &mut [u8],
    regions: &mut [MemoryRegion],
) -> Result<(usize, usize), MapFailure> {
    let mut size = map.len();
    let (mut key, mut dsize, mut dversion) = (0usize, 0usize, 0u32);
    // SAFETY: valid `GetMemoryMap` from the validated boot services, which stay callable until
    // `ExitBootServices` succeeds (§7.4 keeps the memory services after a failed call); every
    // pointer is to a live local or to the `map` buffer of `size` bytes.
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
        return Err(MapFailure::Firmware(status));
    }
    let raw = map.get(..size).unwrap_or(&[]);
    let n = memmap::translate(raw, dsize, regions).map_err(MapFailure::Rejected)?;
    Ok((key, n))
}

/// Stops this processor for good. A failed `ExitBootServices` may have shut part of the
/// firmware down, so none of it may be used again, not even to print (UEFI 2.11 §7.4): the line
/// saying why goes straight to the SPCR's UART first ([`say_halting`], decision item 342).
fn halt() -> ! {
    loop {
        // SAFETY: the loader runs at CPL 0; masking interrupts and halting touch no memory.
        unsafe { core::arch::asm!("cli", "hlt", options(nomem, nostack)) };
    }
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

/// Copies `bytes.len()` bytes of firmware memory at physical `at` into `bytes`, if the memory
/// map `described` covers the whole range (`spcr::described`); `false` if not.
fn read_described(described: &[MemoryRegion], at: u64, bytes: &mut [u8]) -> bool {
    if !spcr::described(described, at, bytes.len()) {
        return false;
    }
    for (address, byte) in (at..).zip(bytes.iter_mut()) {
        // SAFETY: `described` checked that the range lies in memory the firmware's memory map
        // describes, which UEFI 2.11 §2.3.4 keeps identity-mapped while boot services run, and
        // that it neither wraps nor starts at 0. Each byte is read once, volatile, with no
        // reference held to firmware memory.
        *byte = unsafe { ptr::with_exposed_provenance::<u8>(address as usize).read_volatile() };
    }
    true
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

/// The linear framebuffer of the firmware's display, or [`Framebuffer::NONE`] if there is none
/// the kernel could use. Which one: every handle with a Graphics Output Protocol is looked at,
/// and the first whose current mode is usable and which the firmware uses as a console output
/// device wins, else the first usable one. A display that is on but not a console is better
/// than none. Nothing here changes a mode, and nothing here can stop the boot.
fn find_framebuffer(bs: &BootServices, out: &mut ConOut) -> Framebuffer {
    let Some(locate) = bs.locate_handle else {
        return Framebuffer::NONE;
    };
    let mut handles = [ptr::null_mut(); 32];
    let mut bytes = size_of_val(&handles);
    // SAFETY: valid `LocateHandle` from the validated boot services, which are active; the
    // GUID, `bytes` and `handles` are live, and `bytes` holds the buffer's size. A search key
    // is only read for `ByRegisterNotify`, not this `ByProtocol` search.
    let status = unsafe {
        locate(
            LOCATE_BY_PROTOCOL,
            &GRAPHICS_OUTPUT_PROTOCOL,
            ptr::null_mut(),
            &mut bytes,
            handles.as_mut_ptr(),
        )
    };
    if status.is_error() {
        let _ = writeln!(
            out,
            "origin-boot: no Graphics Output Protocol ({status:?}): the kernel will have no framebuffer"
        );
        return Framebuffer::NONE;
    }
    let found = bytes / size_of::<Handle>();
    let (mut any, mut unusable) = (None, 0u32);
    for &handle in handles.get(..found).unwrap_or(&[]) {
        match framebuffer_of(bs, handle) {
            Ok(framebuffer) => {
                if console_device(bs, handle) {
                    return framebuffer;
                }
                any.get_or_insert(framebuffer);
            }
            Err(reason) => {
                unusable += 1;
                let _ = writeln!(out, "origin-boot: graphics mode not usable: {reason:?}");
            }
        }
    }
    if any.is_none() {
        let _ = writeln!(
            out,
            "origin-boot: {found} Graphics Output handle(s), {unusable} without a usable framebuffer: the kernel will have no framebuffer"
        );
    }
    any.unwrap_or(Framebuffer::NONE)
}

/// Whether the firmware uses `handle` as a console output device.
fn console_device(bs: &BootServices, handle: Handle) -> bool {
    let Some(f) = bs.handle_protocol else {
        return false;
    };
    let mut iface: *mut c_void = ptr::null_mut();
    // SAFETY: valid `HandleProtocol` from the validated boot services, which are active; the
    // GUID and `iface` are live. The interface of this tag protocol is NULL, so only the status
    // says whether it is there.
    !unsafe { f(handle, &CONSOLE_OUT_DEVICE, &mut iface) }.is_error()
}

/// The framebuffer of the graphics device on `handle`, in the mode it is in now.
fn framebuffer_of(bs: &BootServices, handle: Handle) -> Result<Framebuffer, framebuffer::Unusable> {
    use framebuffer::Unusable;
    let gop = protocol::<GraphicsOutputProtocol>(bs, handle, &GRAPHICS_OUTPUT_PROTOCOL)
        .map_err(|_| Unusable::NoMode)?;
    // SAFETY: `HandleProtocol` returned a live `EFI_GRAPHICS_OUTPUT_PROTOCOL`; `Mode` is a
    // plain pointer field, valid for any bits.
    let mode = unsafe { (*gop).mode };
    if mode.is_null() || !mode.is_aligned() {
        return Err(Unusable::NoMode);
    }
    // SAFETY: non-null and aligned (checked), from a live protocol; the firmware keeps it
    // readable while boot services are active, and it holds only integers and a raw pointer.
    let mode = unsafe { mode.read() };
    if mode.info.is_null() || !mode.info.is_aligned() {
        return Err(Unusable::NoMode);
    }
    // SAFETY: non-null and aligned (checked), from a live protocol; `Info` holds only integers.
    let info = unsafe { mode.info.read() };
    framebuffer::describe(&info, mode.frame_buffer_base, mode.frame_buffer_size as u64)
}

/// Enters the kernel (ADR-0002 B6, ADR-0004 H4): interrupts off; EFER.NXE and CR0.WP on;
/// the loader-built tables in CR3; the kernel stack; `call entry` with RDI = boot info
/// (System V). Halts if the kernel ever returns. Naked, so its whole body is this code,
/// which the new tables map at its identity address.
///
/// # Safety
///
/// Boot services must have exited and NX must be supported. `cr3` must hold complete tables
/// that map this function's pages at their identity address, `entry` (the kernel's verified
/// entry point), `stack_top` (16-byte aligned) and `boot_info` (a complete `BootInfo`).
#[cfg(not(origin_bench))]
#[unsafe(naked)]
unsafe extern "sysv64" fn enter_kernel(entry: u64, boot_info: u64, stack_top: u64, cr3: u64) -> ! {
    core::arch::naked_asm!(
        "cli",
        "cld",
        "mov r8, rcx",
        "mov r9, rdx",
        "mov r10, rdi",
        "mov r11, rsi",
        "mov ecx, 0xC0000080",
        "rdmsr",
        "or eax, 0x800",
        "wrmsr",
        "mov rax, cr0",
        "or rax, 0x10000",
        "mov cr0, rax",
        "mov cr3, r8",
        "mov rsp, r9",
        "xor ebp, ebp",
        "mov rdi, r11",
        "call r10",
        "2:",
        "cli",
        "hlt",
        "jmp 2b",
    );
}

/// The value of CR4.
fn read_cr4() -> u64 {
    let cr4: u64;
    // SAFETY: the loader runs at CPL 0, where reading CR4 is allowed. Its one effect is on the
    // flags, which Intel leaves undefined after a move from a control register
    // (docs/research/2026-11-10-asm-flags.md), so the block does not claim to preserve them.
    unsafe {
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
    }
    cr4
}

/// TASKS item 112's entry stub, in a bench build: `enter_kernel`'s steps, with one byte to the
/// UART `crumbs` names after the CR3 load (`R`) and after the stack switch (`S`), each after a
/// wait for room of at most 100,000 line-status reads; and `crumbs` handed on in RSI, where
/// `_start` leaves it for `kernel_main` (`origin_boot_info::bench`). With `crumbs` 0 it writes
/// nothing. The bytes go to the port in `crumbs`' low 16 bits, which the SPCR named and the
/// shared parser accepted.
///
/// # Safety
///
/// As for `enter_kernel`, and `crumbs` is 0 or `origin_boot_info::bench::register` of such a port.
#[cfg(origin_bench)]
#[unsafe(naked)]
unsafe extern "sysv64" fn enter_kernel_bench(
    entry: u64,
    boot_info: u64,
    stack_top: u64,
    cr3: u64,
    crumbs: u64,
) -> ! {
    core::arch::naked_asm!(
        "cli",
        "cld",
        "mov r9, rdx",
        "mov r10, rdi",
        "mov r11, rsi",
        "mov rsi, r8",
        "mov r8, rcx",
        "mov ecx, 0xC0000080",
        "rdmsr",
        "or eax, 0x800",
        "wrmsr",
        "mov rax, cr0",
        "or rax, 0x10000",
        "mov cr0, rax",
        "mov cr3, r8",
        // `R`: the tables are loaded.
        "test rsi, rsi",
        "jz 7f",
        "mov dx, si",
        "add dx, 5",
        "mov ecx, 100000",
        "3:",
        "in al, dx",
        "test al, 0x20",
        "jnz 4f",
        "dec ecx",
        "jnz 3b",
        "4:",
        "sub dx, 5",
        "mov al, 0x52",
        "out dx, al",
        "7:",
        "mov rsp, r9",
        // `S`: on the kernel stack, which this write proves mapped and writable (the slot the
        // `call` below writes its return address to; TASKS item 112's review, R112-1).
        "mov qword ptr [rsp - 8], 0",
        "test rsi, rsi",
        "jz 8f",
        "mov dx, si",
        "add dx, 5",
        "mov ecx, 100000",
        "5:",
        "in al, dx",
        "test al, 0x20",
        "jnz 6f",
        "dec ecx",
        "jnz 5b",
        "6:",
        "sub dx, 5",
        "mov al, 0x53",
        "out dx, al",
        "8:",
        "xor ebp, ebp",
        "mov rdi, r11",
        "call r10",
        "2:",
        "cli",
        "hlt",
        "jmp 2b",
    );
}

/// The loader's port I/O for [`uart::Uart`]: the line before a halt (decision item 342) and,
/// in a bench build, the bench lines (TASKS item 112).
struct LoaderPorts;

impl uart::Port for LoaderPorts {
    fn read(&mut self, port: u16) -> u8 {
        let value: u8;
        // SAFETY: CPL 0; the port is one of the eight registers of the UART the SPCR named and
        // the shared parser accepted (`port_block_allowed`): its line status, and in a bench
        // build (TASKS item 116) the others but the receive buffer, so no byte is taken. Reading
        // the line status clears its error bits and reading the modem status its change bits;
        // nothing else changes. IN leaves the flags as they were.
        unsafe {
            core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
        }
        value
    }

    fn write(&mut self, port: u16, value: u8) {
        // SAFETY: CPL 0; the port is the transmit register of that UART, written after
        // `ExitBootServices` was called, when the firmware no longer drives it. In a bench build
        // (TASKS item 116) also before the call, between the firmware's own writes, and its other
        // registers: its line control, divisor, interrupt enable and modem control, set to what was
        // read under the firmware (or, in the QEMU-only `disturb` mode, to loopback and another
        // divisor), and its FIFO control, written 0x07. OUT leaves the flags as they were.
        unsafe {
            core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
        }
    }
}

/// Writes `line` and a line break to the UART boot info names, if it names one, straight to its
/// registers: no firmware service is called (ADR-0002 B5). Returns that UART's port, or 0.
fn say_on_uart(early_uart: EarlyUart, line: impl core::fmt::Display) -> u16 {
    if !early_uart.is_present() {
        return 0;
    }
    let mut io = LoaderPorts;
    let mut uart = uart::Uart::new(&mut io, early_uart.port);
    let _ = writeln!(uart, "{line}");
    early_uart.port
}

/// Decision item 342: after `ExitBootServices` failed, the one line saying why, before the halt.
fn say_halting(early_uart: EarlyUart, calls: uart::ExitCalls) {
    say_on_uart(early_uart, calls.halting());
}

/// TASKS item 112, a bench build: which call succeeded, on the SPCR's UART. Returns its port. Not
/// compiled into the bisection mode, which triple-faults after a successful call (TASKS item 131's
/// review, R131-10).
#[cfg(all(origin_bench, not(origin_bench_bisect)))]
fn bench_exited(early_uart: EarlyUart, exited: bench::Exited) -> u16 {
    say_on_uart(early_uart, exited)
}

/// The processor state the kernel inherits, on the firmware's console, reading only the
/// registers CPUID says exist ([`bench::reads`]). CR0, CR4, EFER and PAT are printed before the
/// shadow-stack registers are read, so a read that faults there still leaves the rest on the
/// console (TASKS item 112's review, R112-2).
#[cfg(origin_bench)]
fn say_entry_state(out: &mut ConOut) {
    let reads = bench::reads(cpuid(0)[0], cpuid(1)[3], cpuid(7)[2]);
    let registers = bench::Registers {
        cr0: read_cr0(),
        cr4: read_cr4(),
        efer: rdmsr(0xC000_0080),
        pat: reads.pat.then(|| rdmsr(0x277)),
    };
    let _ = write!(out, "{}{registers}", bench::PREFIX);
    let shadow_stacks = reads.shadow_stacks.then(|| bench::ShadowStacks {
        s_cet: rdmsr(0x6A2),
        pl0_ssp: rdmsr(0x6A4),
        ssp_table: rdmsr(0x6A8),
    });
    let _ = writeln!(out, "{}", bench::Shadow(shadow_stacks));
}

/// The value of CR0.
#[cfg(origin_bench)]
fn read_cr0() -> u64 {
    let cr0: u64;
    // SAFETY: as for `read_cr4`: CPL 0, and the flags are left undefined, so not claimed.
    unsafe {
        core::arch::asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack));
    }
    cr0
}

/// Model-specific register `msr`. Only for a register CPUID says exists ([`bench::reads`]): any
/// other takes a #GP, under the firmware's IDT.
#[cfg(origin_bench)]
fn rdmsr(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: CPL 0; the callers read only EFER, which every x86-64 processor has, and the
    // registers `bench::reads` found enumerated. RDMSR leaves the flags as they were.
    unsafe {
        core::arch::asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags));
    }
    (u64::from(high) << 32) | u64::from(low)
}

/// Reads a bench switch, [`bench::STALE_KEY_PATH`], [`bench::AFTER_PATH`] or
/// [`fch::SWITCH_PATH`], from the loader's volume, in a bench build: `None` if there is no such
/// file, else its first bytes and how many were read. Reads at most 32 bytes into the stack:
/// nothing is allocated.
#[cfg(origin_bench)]
fn read_bench_file(image: Handle, bs: &BootServices, name: &str) -> Option<([u8; 32], usize)> {
    let loaded = protocol::<LoadedImageProtocol>(bs, image, &LOADED_IMAGE_PROTOCOL).ok()?;
    // SAFETY: `HandleProtocol` returned a live `EFI_LOADED_IMAGE_PROTOCOL` for this image.
    let device = unsafe { (*loaded).device_handle };
    let fs = protocol::<SimpleFileSystemProtocol>(bs, device, &SIMPLE_FILE_SYSTEM_PROTOCOL).ok()?;
    // SAFETY: `HandleProtocol` returned a live `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL`.
    let open_volume = unsafe { (*fs).open_volume }?;
    let mut root: *mut FileProtocol = ptr::null_mut();
    // SAFETY: valid `OpenVolume` of a live protocol instance; `root` is a live local.
    let status = unsafe { open_volume(fs, &mut root) };
    if status.is_error() || root.is_null() {
        return None;
    }
    let mut path = [0u16; 64];
    // The array must end in a NUL after the name (TASKS item 116's review, R116-11).
    if name.len() >= path.len() {
        return None;
    }
    for (dst, b) in path.iter_mut().zip(name.bytes()) {
        *dst = u16::from(b);
    }
    let mut contents = [0u8; 32];
    let mut len = 0usize;
    let mut found = false;
    // SAFETY: `root` is a live file handle from `OpenVolume`.
    if let Some(open) = unsafe { (*root).open } {
        let mut file: *mut FileProtocol = ptr::null_mut();
        // SAFETY: valid `Open` on a live handle; `path` is NUL-terminated (the array is longer
        // than the ASCII path); `file` is a live local.
        let status = unsafe { open(root, &mut file, path.as_ptr(), FILE_MODE_READ, 0) };
        if !status.is_error() && !file.is_null() {
            found = true;
            // SAFETY: `file` is the live handle `Open` just returned.
            if let Some(read) = unsafe { (*file).read } {
                len = contents.len();
                // SAFETY: valid `Read` on a live handle into `contents`, which has `len` bytes.
                if unsafe { read(file, &mut len, contents.as_mut_ptr().cast()) }.is_error() {
                    len = 0;
                }
            }
            // SAFETY: `file` is the handle `Open` returned; it is not used afterwards.
            if let Some(close) = unsafe { (*file).close } {
                // SAFETY: closing the handle this function opened.
                let _ = unsafe { close(file) };
            }
        }
    }
    // SAFETY: `root` is the open volume handle from `OpenVolume`.
    if let Some(close) = unsafe { (*root).close } {
        // SAFETY: closing the handle this function opened; it is not used afterwards.
        let _ = unsafe { close(root) };
    }
    found.then_some((contents, len.min(contents.len())))
}

/// `EFI_STALL` (UEFI 2.11 §7.5.2).
#[cfg(origin_bench)]
type StallFn = unsafe extern "efiapi" fn(microseconds: usize) -> Status;
/// `EFI_RESET_SYSTEM` (UEFI 2.11 §8.5.1.1). It does not return; a firmware whose does is what the
/// FADT's register is for.
#[cfg(origin_bench)]
type ResetSystemFn =
    unsafe extern "efiapi" fn(reset_type: u32, status: Status, size: usize, data: *const c_void);

/// What a bench loader does after `ExitBootServices` (TASKS item 116), settled before the call.
#[cfg(origin_bench)]
struct BenchPlan {
    /// The mode `BENCH-AFTER` asked for.
    after: bench::After,
    /// The UART's registers under the firmware, if the SPCR named one in I/O space. Read only
    /// after the call, which the bisection mode does not do (R131-10).
    #[cfg_attr(origin_bench_bisect, allow(dead_code))]
    under: Option<bench::UartState>,
    /// The firmware's `ResetSystem`, if its runtime services table checks.
    reset_system: Option<ResetSystemFn>,
    /// The FADT's reset register.
    register: Result<bench::ResetRegister, bench::NoReset>,
    /// The time-stamp counter's ticks a millisecond, for the waits.
    ticks_per_ms: u64,
    /// Whether the marks before each call are written: only when the line before the call went
    /// out whole and the UART then emptied, so a mark never waits between `GetMemoryMap` and
    /// `ExitBootServices` (TASKS item 116's review, R116-7).
    marks: bool,
    /// TASKS item 121: the reset with no firmware code after a call that returns `EFI_SUCCESS`, in
    /// a run whose last reset was a `0x06` to `0xCF9` or that finds no record of it. Always `None`
    /// in the bisection mode, which never reads it (R131-10).
    #[cfg_attr(origin_bench_bisect, allow(dead_code))]
    free: Option<fch::Free>,
}

/// TASKS item 116, before `ExitBootServices`, under the firmware: the counter's rate over the
/// firmware's `Stall`, `ResetSystem` from the runtime services table, and the UART the SPCR named
/// read as the firmware left it, all said on the firmware's console; then one line straight to
/// that UART.
#[cfg(origin_bench)]
fn bench_before_call(
    st: &SystemTable,
    bs: &BootServices,
    early_uart: EarlyUart,
    after: bench::After,
    register: Result<bench::ResetRegister, bench::NoReset>,
    fch_seen: FchSeen,
    out: &mut ConOut,
) -> BenchPlan {
    let (ticks_per_ms, counted) = bench::ticks_per_ms(count_ticks(bs));
    let reset_system = runtime_reset_system(st);
    let _ = writeln!(
        out,
        "{}",
        bench::ResetPlan {
            reset_system: reset_system.map(|f| f as usize as u64),
            ticks_per_ms,
            counted,
        }
    );
    // TASKS item 121: the FCH's registers, the control in a boot's first run, or the reset after
    // the call in a run that follows a `0x06` to `0xCF9` or finds no record.
    #[cfg(not(origin_bench_bisect))]
    let free = bench_fch_decide(fch_seen, ticks_per_ms, early_uart, out);
    // The bisection mode's decision and control are the minimal mode's (Amendment 6, step 3), and
    // no reset of Amendment 4's follows the call.
    #[cfg(origin_bench_bisect)]
    let free = bisect::decide(fch_seen, early_uart, out);
    // With interrupts masked: a firmware timer callback that wrote the UART while DLAB is set
    // would change its divisor (TASKS item 116's review, R116-6).
    let under = early_uart.is_present().then(|| {
        with_interrupts_masked(|| bench::UartState::read(&mut LoaderPorts, early_uart.port))
    });
    let _ = writeln!(
        out,
        "{}",
        bench::UnderFirmware {
            port: early_uart.port,
            state: under,
        }
    );
    let marks = early_uart.is_present() && {
        let mut io = LoaderPorts;
        let mut uart = uart::Uart::new(&mut io, early_uart.port);
        let whole = writeln!(uart, "{}", bench::BEFORE_CALL).is_ok();
        whole && bench::drain(&mut io, early_uart.port)
    };
    BenchPlan {
        after,
        under,
        reset_system: reset_system.ok(),
        register,
        ticks_per_ms,
        marks,
        free,
    }
}

/// Masks interrupts: once `ExitBootServices` has returned, nothing of the firmware's may take one
/// (TASKS item 116's review, R116-5).
#[cfg(origin_bench)]
fn mask_interrupts() {
    // SAFETY: CPL 0; masking interrupts touches no memory. CLI changes the flags, so the block does
    // not claim to keep them.
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };
}

/// Runs `f` with interrupts masked, then unmasks them if they were on before.
#[cfg(origin_bench)]
fn with_interrupts_masked<T>(f: impl FnOnce() -> T) -> T {
    let flags: u64;
    // SAFETY: CPL 0; PUSHFQ and POP read the flags through the stack, which the block uses and
    // leaves as it found it.
    unsafe {
        core::arch::asm!("pushfq", "pop {}", out(reg) flags, options(nomem, preserves_flags))
    };
    mask_interrupts();
    let value = f();
    if flags & (1 << 9) != 0 {
        // SAFETY: CPL 0; interrupts were on before, under the firmware, and are turned on again.
        // STI changes the flags, so the block does not claim to keep them.
        unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
    }
    value
}

/// The time-stamp counter's ticks over [`bench::CALIBRATION_US`] of the firmware's `Stall`, or
/// `None` if there is no `Stall` or it fails.
#[cfg(origin_bench)]
fn count_ticks(bs: &BootServices) -> Option<u64> {
    // SAFETY: `misc[1]` is `Stall` in the validated boot services table (UEFI 2.11 §4.4, whose
    // `HeaderSize` covers it); a raw pointer and an optional function pointer have the same size,
    // and null is `None`.
    let stall = unsafe { core::mem::transmute::<*const c_void, Option<StallFn>>(bs.misc[1]) }?;
    let start = random::rdtsc();
    // SAFETY: valid `Stall` from the validated boot services, which are active.
    let status = unsafe { stall(bench::CALIBRATION_US) };
    let ticks = random::rdtsc().wrapping_sub(start);
    (!status.is_error()).then_some(ticks)
}

/// `ResetSystem` from the system table's runtime services table, checked: aligned, signed
/// "RUNTSERV", long enough to hold it, and not null (UEFI 2.11 §4.5.1).
#[cfg(origin_bench)]
fn runtime_reset_system(st: &SystemTable) -> Result<ResetSystemFn, &'static str> {
    let p = st.runtime_services.cast::<u8>().cast_const();
    if p.is_null() || !p.cast::<TableHeader>().is_aligned() {
        return Err("the system table names no runtime services table, or a misaligned one");
    }
    // SAFETY: non-null and aligned, from the validated system table; the runtime services table
    // lies in runtime memory, readable before and after `ExitBootServices` (UEFI 2.11 §4.5).
    // Every field of the header is an integer, so any bit pattern is valid.
    let hdr = unsafe { p.cast::<TableHeader>().read() };
    if hdr.signature != bench::RUNTIME_SERVICES_SIGNATURE {
        return Err("the runtime services table's signature is not RUNTSERV");
    }
    if (hdr.header_size as usize) < bench::RESET_SYSTEM_OFFSET + size_of::<usize>() {
        return Err("the runtime services table is too short to hold ResetSystem");
    }
    let at = p
        .wrapping_add(bench::RESET_SYSTEM_OFFSET)
        .cast::<Option<ResetSystemFn>>();
    // SAFETY: `at` is inside the table, as its `HeaderSize` says, and 8-byte aligned (an aligned
    // table and an offset that is a multiple of 8); an optional function pointer is valid for any
    // bits, null being `None`.
    let f = unsafe { at.read() };
    f.ok_or("the runtime services table's ResetSystem is null")
}

/// ADR-0002 Amendment 4's addition (item 348): the boot services table the firmware's system table
/// names now, compared byte for byte with the loader's copy over the copy's size, once its
/// signature and `HeaderSize` check.
#[cfg(origin_bench)]
fn bench_table_line(
    system_table: *const SystemTable,
    st: &SystemTable,
    bs: &BootServices,
) -> bench::TableLine {
    let size = size_of::<BootServices>();
    let exit_at = core::mem::offset_of!(BootServices, exit_boot_services);
    let crc_at = core::mem::offset_of!(TableHeader, crc32);
    // SAFETY: `bs` is the loader's own copy, a `#[repr(C)]` struct of a header of integers and
    // pointer-sized members, with no padding; its `size` bytes are initialized.
    let copy = unsafe { core::slice::from_raw_parts(ptr::from_ref(bs).cast::<u8>(), size) };
    let word = |bytes: &[u8], at: usize| {
        let mut w = [0u8; 8];
        w.copy_from_slice(bytes.get(at..at + 8).unwrap_or(&[0; 8]));
        u64::from_le_bytes(w)
    };
    let copy_crc = bs.hdr.crc32;
    // SAFETY: `system_table` is the firmware's own table, checked at entry (`read_system_table`);
    // only the member's address is formed.
    let member = unsafe { &raw const (*system_table).boot_services };
    // SAFETY: that member, in a table that stays readable while boot services run; one volatile
    // pointer-sized load.
    let live = unsafe { member.read_volatile() }.cast::<u8>().cast_const();
    let mut line = bench::TableLine {
        live_at: live as u64,
        copied_from: st.boot_services as u64,
        size,
        live: bench::Live::Unchecked,
        copy_exit: word(copy, exit_at),
        copy_crc,
    };
    if live.is_null() || !live.cast::<TableHeader>().is_aligned() {
        return line;
    }
    // SAFETY: non-null and aligned; the firmware's boot services table stays readable while boot
    // services run, and its header's members are integers, valid for any bits.
    let hdr = unsafe { live.cast::<TableHeader>().read_volatile() };
    if hdr.signature != BOOT_SERVICES_SIGNATURE || (hdr.header_size as usize) < size {
        return line;
    }
    let byte = |i: usize| {
        // SAFETY: `i` is below `size`, which the live table's `HeaderSize` covers; one volatile
        // byte load of firmware memory, with no reference held to it.
        unsafe { live.wrapping_add(i).read_volatile() }
    };
    let mut exit = [0u8; 8];
    for (k, b) in exit.iter_mut().enumerate() {
        *b = byte(exit_at + k);
    }
    line.live = bench::Live::Read {
        first_difference: bench::first_difference(copy, byte),
        exit: u64::from_le_bytes(exit),
        crc: {
            let mut c = [0u8; 4];
            for (k, b) in c.iter_mut().enumerate() {
                *b = byte(crc_at + k);
            }
            u32::from_le_bytes(c)
        },
    };
    line
}

/// [`bench::After::Disturb`]: right after the call returns, what a firmware might do to the UART.
/// Not compiled into the bisection mode (R131-10).
#[cfg(all(origin_bench, not(origin_bench_bisect)))]
fn bench_disturb(early_uart: EarlyUart, plan: &BenchPlan) {
    if plan.after == bench::After::Disturb
        && let Some(under) = plan.under
    {
        bench::disturb(&mut LoaderPorts, early_uart.port, &under);
    }
}

/// TASKS item 116, after `ExitBootServices` returned, either way, and after its line: the UART's
/// registers read again, the values read under the firmware written back, and a line naming what
/// was read; then the reset, unless `BENCH-AFTER` asks for the kernel. Not compiled into the
/// bisection mode, whose path after a second failed call is the minimal mode's, with no UART
/// register read or write-back (ADR-0002 Amendment 6, step 5; ruling Q5 of TASKS item 131; R131-10).
#[cfg(all(origin_bench, not(origin_bench_bisect)))]
fn bench_after_call(early_uart: EarlyUart, plan: &BenchPlan, calls: uart::ExitCalls) {
    if early_uart.is_present() {
        let mut io = LoaderPorts;
        let read = bench::UartState::read(&mut io, early_uart.port);
        if let Some(under) = plan.under {
            let _ = under.write_back(&mut io, early_uart.port);
        }
        say_on_uart(
            early_uart,
            bench::AfterCall {
                calls,
                read,
                wrote_back: plan.under.is_some(),
            },
        );
    }
    if !plan.after.enters_kernel() {
        bench_reset(early_uart, plan)
    }
}

/// TASKS item 116: the reset, whatever the call did. A cold `ResetSystem` about 5 s after its
/// line (decision item 344, ADR-0002 Amendment 3: a bench-only exception to B5); if the machine
/// still runs 2 s later, the FADT's reset register; then the halt. Each step is said on the UART
/// first. A reset shows on a rented console as a new firmware start, which needs no UART.
#[cfg(origin_bench)]
fn bench_reset(early_uart: EarlyUart, plan: &BenchPlan) -> ! {
    mask_interrupts();
    let call = plan
        .reset_system
        .filter(|_| !plan.after.skips_reset_system());
    say_on_uart(
        early_uart,
        bench::ResetSoon {
            reset_system: call.is_some(),
            register: plan.register.is_ok(),
        },
    );
    wait_ms(plan.ticks_per_ms, bench::RESET_DELAY_MS);
    let tried = match call {
        Some(reset_system) => {
            // SAFETY: `ResetSystem` from the checked runtime services table, which stays callable
            // after `ExitBootServices`; the firmware's own page tables are still loaded, so its
            // physical addresses are what it was built for (UEFI 2.11 §4.5, §8.5.1.1). No reset
            // data is passed.
            unsafe { reset_system(bench::RESET_COLD, Status::SUCCESS, 0, ptr::null()) };
            // It returned, which it should not: whatever it turned on, interrupts stay off.
            mask_interrupts();
            bench::Tried::Returned
        }
        None => bench::Tried::not_called(plan.after),
    };
    wait_ms(plan.ticks_per_ms, bench::FALLBACK_DELAY_MS);
    say_on_uart(
        early_uart,
        bench::Fallback {
            tried,
            register: plan.register,
        },
    );
    if let Ok(register) = plan.register {
        write_reset_register(register);
        wait_ms(plan.ticks_per_ms, bench::AFTER_REGISTER_MS);
        say_on_uart(early_uart, bench::NOT_RESET);
    }
    halt()
}

/// Writes the FADT's reset value to its reset register (ACPI 6.6 §4.8.3.6).
#[cfg(origin_bench)]
fn write_reset_register(register: bench::ResetRegister) {
    match register {
        bench::ResetRegister::Io { port, value } => {
            // SAFETY: CPL 0; the port is the FADT's reset register, which `bench::find_reset`
            // checked (system I/O, 8 bits at bit 0, a 16-bit port), and writing its value is
            // meant to reset the machine. OUT leaves the flags as they were.
            unsafe {
                core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
            }
        }
        bench::ResetRegister::Memory { address, value } => {
            // SAFETY: the FADT's reset register, a byte of memory-mapped I/O the firmware's map
            // describes (`bench::find_reset`), so the firmware's page tables, still loaded, map it
            // at its physical address; one volatile byte store, as §4.8.3.6 asks.
            unsafe {
                ptr::with_exposed_provenance_mut::<u8>(address as usize).write_volatile(value)
            };
        }
    }
}

/// Spins until `ms` milliseconds have passed by the time-stamp counter.
#[cfg(origin_bench)]
fn wait_ms(ticks_per_ms: u64, ms: u64) {
    let goal = ticks_per_ms.saturating_mul(ms);
    let start = random::rdtsc();
    while random::rdtsc().wrapping_sub(start) < goal {
        core::hint::spin_loop();
    }
}

/// TASKS item 121: what the table walk saw of the FCH's power-management block, settled before
/// the page tables are built. Where it is is said in [`bench_fch_read`], before any register is
/// read; the values and the decision in [`bench_fch_decide`].
#[cfg(origin_bench)]
#[derive(Clone, Copy)]
struct FchSeen {
    /// Where the registers are, and whether they were read.
    where_line: fch::WhereLine,
    /// The registers, read or given by `BENCH-FCH`.
    registers: Option<fch::Registers>,
    /// The FADT's PM1a event block, in system I/O.
    pm1a_event: Option<u16>,
    /// `BENCH-FCH`, which only QEMU's ESP may hold, or the minimal mode's stand-in table.
    pretend: Option<fch::Pretend>,
    /// `SMIxC0` (ADR-0002 Amendment 4's addition, item 348).
    smi: fch::Smi,
    /// Where the registers' values came from.
    source: fch::Source,
}

#[cfg(origin_bench)]
impl FchSeen {
    const fn not_read(why: fch::NotRead) -> Self {
        Self {
            where_line: fch::WhereLine::NotRead(why),
            registers: None,
            pm1a_event: None,
            pretend: None,
            smi: fch::Smi::NotRead(why),
            source: fch::Source::Read,
        }
    }
}

/// TASKS item 121 (decision item 345, ADR-0002 Amendment 4), under boot services: the FCH's
/// power-management registers, read only where [`fch::gate`] allows it (an AMD processor, a
/// region of the firmware's DSDT holding them, and its page tables mapping them to themselves),
/// and only through loads; or `BENCH-FCH`'s values in their place. Every table and page-table byte
/// is read from memory the firmware's map describes. The line saying where they are, and which
/// check failed if one did, is written before any register is read (TASKS item 121's review,
/// R121-6).
#[cfg(origin_bench)]
fn bench_fch_read(
    rsdp: u64,
    described: &[MemoryRegion],
    pretend: Option<fch::Pretend>,
    minimal: bool,
    out: &mut ConOut,
) -> FchSeen {
    let [_, ebx, ecx, edx] = cpuid(0);
    let amd = fch::is_amd_vendor(ebx, ecx, edx);
    // Amendment 4's addition (item 348): whether `SMIxC0` passed its own gate; it is read once the
    // line about the power-management block is out.
    let mut smi_gated = false;
    let seen = if pretend.is_none() && !amd {
        // The DSDT is not scanned for a processor that has no AMD FCH.
        FchSeen::not_read(fch::NotRead::NotAmd)
    } else {
        let mut read = |at: u64, bytes: &mut [u8]| read_described(described, at, bytes);
        let located = fch::locate(rsdp, &mut read);
        let pm1a_event = located.ok().and_then(|l| l.pm1a_event);
        if let Some(p) = pretend {
            // The minimal mode's stand-in table (ADR-0002 Amendment 5), or `BENCH-FCH`.
            FchSeen {
                where_line: if minimal {
                    fch::WhereLine::StandIn
                } else {
                    fch::WhereLine::Switch
                },
                registers: Some(if minimal {
                    p.values_minimal(pm1a_event)
                } else {
                    p.values(pm1a_event)
                }),
                pm1a_event,
                pretend,
                smi: fch::Smi::Given(p.smi_value()),
                source: if minimal {
                    fch::Source::Table
                } else {
                    fch::Source::Switch
                },
            }
        } else {
            let la57 = read_cr4() & (1 << 12) != 0;
            let cr3 = read_cr3();
            let smi =
                match fch::gate_smi(amd, located, &mut |at| fch::walk(cr3, la57, at, &mut read)) {
                    Ok(_) => {
                        smi_gated = true;
                        fch::Smi::NotRead(fch::NotRead::NotAmd)
                    }
                    Err(why) => fch::Smi::NotRead(why),
                };
            match fch::gate(amd, located, &mut |at| fch::walk(cr3, la57, at, &mut read)) {
                Ok(region) => FchSeen {
                    where_line: fch::WhereLine::Read(
                        region,
                        fch::map_says(described, fch::PM_BLOCK),
                    ),
                    registers: None,
                    pm1a_event,
                    pretend: None,
                    smi,
                    source: fch::Source::Read,
                },
                Err(why) => FchSeen {
                    smi,
                    ..FchSeen::not_read(why)
                },
            }
        }
    };
    let _ = writeln!(out, "{}", seen.where_line);
    let smi = if smi_gated {
        // SAFETY: `fch::gate_smi` passed: the processor is AMD's, the firmware's DSDT describes a
        // system-memory region holding `SMIxC0`'s four bytes, and the firmware's page tables, still
        // loaded, map its first and last byte to themselves.
        fch::Smi::Read(unsafe { read_smi_control() })
    } else {
        seen.smi
    };
    if matches!(seen.where_line, fch::WhereLine::Read(..)) {
        // SAFETY: `fch::gate` passed: the processor is AMD's, the firmware's DSDT describes a
        // system-memory region holding every register read, and the firmware's page tables, still
        // loaded, map its first and last byte to themselves.
        let registers = unsafe { read_fch_registers() };
        return FchSeen {
            registers: Some(registers),
            smi,
            ..seen
        };
    }
    FchSeen { smi, ..seen }
}

/// `SMIxC0` (ADR-0002 Amendment 4's addition, item 348).
///
/// # Safety
///
/// [`fch::gate_smi`] must have passed: an AMD processor, a DSDT region holding the register, and
/// the firmware's page tables, still loaded, mapping it to itself.
#[cfg(origin_bench)]
unsafe fn read_smi_control() -> u32 {
    // SAFETY: the caller's contract: memory-mapped I/O the firmware maps to itself; one volatile
    // 32-bit load of a read-write control register, with no effect AMD's documents name.
    unsafe { ptr::with_exposed_provenance::<u32>(fch::SMI_CF9_CONTROL as usize).read_volatile() }
}

/// TASKS item 121, before the call, on the firmware's console: where the FCH's registers are,
/// their values, and what the run does. In a boot's first run, the control: `0x06` to `0xCF9`
/// once the UART has emptied, and if the machine still runs after the wait, a line saying so; the
/// run then makes the call and takes Amendment 3's path. In a run whose last reset was a `0x06` to
/// `0xCF9`, or that finds no record of the last reset, what [`reset_without_firmware`] does after
/// a call that returns `EFI_SUCCESS`.
#[cfg(all(origin_bench, not(origin_bench_bisect)))]
fn bench_fch_decide(
    seen: FchSeen,
    ticks_per_ms: u64,
    early_uart: EarlyUart,
    out: &mut ConOut,
) -> Option<fch::Free> {
    let pretend = seen.pretend;
    let _ = writeln!(out, "{}", fch::SmiLine(seen.smi));
    let Some(registers) = seen.registers else {
        let _ = writeln!(
            out,
            "{}",
            fch::DecisionLine {
                decision: fch::Decision::NotRead,
                pretend,
            }
        );
        return None;
    };
    let _ = writeln!(
        out,
        "{}",
        fch::RegistersLine {
            registers,
            source: seen.source,
        }
    );
    let decision = fch::Decision::of(&registers, seen.pm1a_event);
    let _ = writeln!(out, "{}", fch::DecisionLine { decision, pretend });
    let wait = pretend.map_or(fch::WAIT_MS, fch::Pretend::wait_ms);
    let cf9 = !pretend.is_some_and(|p| p.no_cf9);
    match decision {
        fch::Decision::Control(_) => {
            // The line just written is on the wire before the reset can cut it.
            if early_uart.is_present() {
                let _ = bench::drain(&mut LoaderPorts, early_uart.port);
            }
            if cf9 {
                write_cf9();
            }
            wait_ms(ticks_per_ms, wait);
            let _ = writeln!(out, "{}", fch::ControlFailed(wait));
            None
        }
        fch::Decision::AfterCall { shutdown, .. } => Some(fch::Free {
            cf9,
            shutdown: shutdown && !pretend.is_some_and(|p| p.no_shutdown),
            wait_ticks: ticks_per_ms.saturating_mul(wait),
        }),
        fch::Decision::NotRead | fch::Decision::Contradicts(_) => None,
    }
}

/// TASKS item 121, after `ExitBootServices` returned `EFI_SUCCESS`, in a run the FCH's record
/// sends to the call: `0x06` to `0xCF9` before anything else; interrupts masked; the wait by the
/// time-stamp counter; then a triple fault where the shutdown message reaches the FCH. If the
/// machine is still running after that (with the shutdown message off, or when `BENCH-FCH` leaves
/// the triple fault out), the caller goes on to TASKS item 116's path and Amendment 3's order.
#[cfg(all(origin_bench, not(origin_bench_bisect)))]
#[inline(always)]
fn reset_without_firmware(free: fch::Free) {
    if free.cf9 {
        write_cf9();
    }
    mask_interrupts();
    wait_ticks(free.wait_ticks);
    if free.shutdown {
        triple_fault();
    }
}

/// `0x06`, a warm reset, to the FCH's reset control port `0xCF9`. Not compiled into the bisection
/// mode, which writes nothing there (ADR-0002 Amendment 6, step 5).
#[cfg(all(origin_bench, not(origin_bench_bisect)))]
#[inline(always)]
fn write_cf9() {
    // SAFETY: CPL 0; port 0xCF9 is the FCH's reset control register, and 0x06 its documented
    // warm-reset command (AMD's FCH documents, TASKS item 121), written only to end a diagnostic
    // run on a bench machine (decision item 345). OUT leaves the flags as they were. The block is
    // not `nomem`, so the compiler moves no memory access across it (TASKS item 121's review,
    // R121-8).
    unsafe {
        core::arch::asm!("out dx, al", in("dx") fch::RESET_PORT, in("al") fch::WARM_RESET, options(nostack, preserves_flags));
    }
}

/// A triple fault: an IDT with a zero limit, then a breakpoint. Always inlined, so the minimal
/// mode's path from the call to it holds no `call` (ADR-0002 Amendment 5, step 6).
#[cfg(origin_bench)]
#[inline(always)]
fn triple_fault() -> ! {
    // SAFETY: CPL 0, and only to end a diagnostic run (decision items 345, 347 and 352): after
    // `ExitBootServices` in the full mode (interrupts masked); in the minimal and bisection modes
    // either before the call, as the control (interrupts masked, boot services live), or right
    // after a successful call, with interrupts as the firmware left them, since nothing may come
    // between (Amendment 5, step 6; Amendment 6, step 5; TASKS item 128's review, R128-5). Whatever arrives once the zero-limit IDT is
    // loaded faults too and ends in the same shutdown; before it, the firmware's IDT takes it. The
    // block pushes 16 zero bytes and loads them as the IDT: limit 0, base 0. `int3` then finds no gate within the limit, nor do the #GP and the double fault that
    // follow, so the processor shuts down (AMD64 APM Volume 2 §8.2.9). It does not return; `ud2`
    // is never reached.
    unsafe {
        core::arch::asm!(
            "push 0",
            "push 0",
            "lidt [rsp]",
            "int3",
            "ud2",
            options(noreturn)
        )
    }
}

/// Spins until `ticks` time-stamp counter ticks have passed. The bisection mode, which has no
/// Amendment 4 reset, does not use it (R131-10).
#[cfg(origin_bench)]
#[cfg_attr(origin_bench_bisect, allow(dead_code))]
fn wait_ticks(ticks: u64) {
    let start = random::rdtsc();
    while random::rdtsc().wrapping_sub(start) < ticks {
        core::hint::spin_loop();
    }
}

/// CR3: the firmware's page tables, which stay loaded until the kernel is entered.
#[cfg(origin_bench)]
fn read_cr3() -> u64 {
    let cr3: u64;
    // SAFETY: as for `read_cr4`: CPL 0, and the flags are left undefined, so not claimed.
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack));
    }
    cr3
}

/// The FCH's power-management registers, each one volatile load of its documented width
/// ([`fch::Registers`]).
///
/// # Safety
///
/// [`fch::gate`] has passed: the processor is AMD's, the firmware's DSDT describes a system-memory
/// region holding every register read, and the firmware's page tables, still loaded, map its first
/// and last byte to themselves (TASKS item 121's review, R121-7).
#[cfg(origin_bench)]
unsafe fn read_fch_registers() -> fch::Registers {
    fch::Registers {
        // SAFETY: as above; `PMx00` is 32 bits, 4-aligned.
        decode_en: unsafe { fch_read32(fch::DECODE_EN) },
        // SAFETY: as above; `PMx08` is 32 bits, 4-aligned.
        pci_control: unsafe { fch_read32(fch::PCI_CONTROL) },
        // SAFETY: as above; `PMx60` is 16 bits, 2-aligned.
        pm1_evt_blk: unsafe { fch_read16(fch::PM1_EVT_BLK) },
        // SAFETY: as above; `PMx74` is 32 bits, 4-aligned.
        acpi_config: unsafe { fch_read32(fch::ACPI_CONFIG) },
        // SAFETY: as above; `PMxC0` is 32 bits, 4-aligned.
        reset_status: unsafe { fch_read32(fch::RESET_STATUS) },
        // SAFETY: as above; `PMxD2` is 8 bits.
        pmio_debug: unsafe { fch_read8(fch::PMIO_DEBUG) },
    }
}

/// The 32-bit register at `offset` in the FCH's power-management block.
///
/// # Safety
///
/// As for [`read_fch_registers`], and `offset` is a documented 32-bit register's, 4-aligned.
#[cfg(origin_bench)]
unsafe fn fch_read32(offset: u64) -> u32 {
    // SAFETY: the caller's contract: memory-mapped I/O the firmware maps to itself; one volatile
    // load, with no effect AMD's documents name.
    unsafe {
        ptr::with_exposed_provenance::<u32>((fch::PM_BLOCK + offset) as usize).read_volatile()
    }
}

/// The 16-bit register at `offset`.
///
/// # Safety
///
/// As for [`read_fch_registers`], and `offset` is a documented 16-bit register's, 2-aligned.
#[cfg(origin_bench)]
unsafe fn fch_read16(offset: u64) -> u16 {
    // SAFETY: as for `fch_read32`, for a 16-bit register.
    unsafe {
        ptr::with_exposed_provenance::<u16>((fch::PM_BLOCK + offset) as usize).read_volatile()
    }
}

/// The 8-bit register at `offset`.
///
/// # Safety
///
/// As for [`read_fch_registers`], and `offset` is a documented 8-bit register's.
#[cfg(origin_bench)]
unsafe fn fch_read8(offset: u64) -> u8 {
    // SAFETY: as for `fch_read32`, for an 8-bit register.
    unsafe { ptr::with_exposed_provenance::<u8>((fch::PM_BLOCK + offset) as usize).read_volatile() }
}

/// CPUID `leaf`, sub-leaf 0: `[eax, ebx, ecx, edx]`.
fn cpuid(leaf: u32) -> [u32; 4] {
    let (eax, ebx, ecx, edx): (u32, u32, u32, u32);
    // SAFETY: CPUID is available on every x86-64 processor and reads no memory; RBX is
    // reserved by LLVM, so it is moved out through another register and restored.
    unsafe {
        core::arch::asm!(
            "mov {save:r}, rbx",
            "cpuid",
            "xchg {save:r}, rbx",
            save = out(reg) ebx,
            inout("eax") leaf => eax,
            inout("ecx") 0u32 => ecx,
            out("edx") edx,
            options(nomem, nostack, preserves_flags),
        );
    }
    [eax, ebx, ecx, edx]
}

/// Whether the processor supports the execute-disable bit.
fn nx_supported() -> bool {
    cpuid(0x8000_0000)[0] >= 0x8000_0001 && cpuid(0x8000_0001)[3] & (1 << 20) != 0
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

/// TASKS item 127: how deep the stack goes before `ExitBootServices`, by paint, in a measurement
/// build only (`cargo xtask stack-depth`, into `target/stack-measure`; never signed and never
/// rented). The rules and the lines are `stackdepth`'s; this holds the volatile accesses to the
/// stack and the state between phases. The loader has one thread, so the atomics only hold state
/// between calls.
#[cfg(origin_stack_measure)]
mod measure {
    use core::fmt::Write;
    use core::ptr;
    use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

    use origin_boot_info::EarlyUart;
    use origin_boot_uefi::console::ConOut;
    pub(super) use origin_boot_uefi::stackdepth::Phase;
    use origin_boot_uefi::stackdepth::{
        self, CALIBRATION_BYTES, Calibration, Depth, PAINT, Painted, PhaseLine, Record, Summary,
    };

    /// The entry RSP, 0 until [`begin`].
    static TOP: AtomicU64 = AtomicU64::new(0);
    /// The current paint's end, 0 while nothing is painted.
    static END: AtomicU64 = AtomicU64::new(0);
    /// The calibration array's depth below [`TOP`], 0 until calibrated.
    static CALIBRATED: AtomicU64 = AtomicU64::new(0);
    /// What the calibration's scan found, [`Depth::encode`]d.
    static CALIBRATION_FOUND: AtomicU64 = AtomicU64::new(0);
    /// The deepest phase so far ([`Phase::number`], 0 for none) and its depth, encoded.
    static DEEPEST_PHASE: AtomicU8 = AtomicU8::new(0);
    static DEEPEST: AtomicU64 = AtomicU64::new(0);

    /// RSP, where this is inlined.
    #[inline(always)]
    pub(super) fn rsp() -> u64 {
        let rsp: u64;
        // SAFETY: copies RSP into a register; touches no memory and leaves the flags as they were.
        unsafe {
            core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags));
        }
        rsp
    }

    /// At entry, `top` being `efi_main`'s RSP: paints the stack UEFI promises, calibrates the
    /// scan on a known array, and paints again for the first phase. Each hook is kept out of line,
    /// so the frames of the functions that call it are those of the normal loader as nearly as
    /// a build with calls in them can be.
    #[inline(never)]
    pub(super) fn begin(top: u64) {
        TOP.store(top, Ordering::Relaxed);
        if paint().is_none() {
            return;
        }
        let array = calibrate();
        let found = scan().unwrap_or(Depth::Above(0));
        CALIBRATED.store(top.wrapping_sub(array), Ordering::Relaxed);
        CALIBRATION_FOUND.store(found.encode(), Ordering::Relaxed);
        let _ = paint();
    }

    /// The calibration's line, through the firmware's console.
    #[inline(never)]
    pub(super) fn say_calibration(out: &mut ConOut) {
        let expected = CALIBRATED.load(Ordering::Relaxed);
        if expected == 0 {
            let _ = writeln!(out, "{}", Summary::NotPainted);
        } else {
            let found = Depth::decode(CALIBRATION_FOUND.load(Ordering::Relaxed));
            let _ = writeln!(out, "{}", Calibration { expected, found });
        }
    }

    /// The end of `phase`: its depth, its line through the firmware's console, and a fresh paint
    /// below the current RSP for the next phase. The line is written before the paint, so its
    /// own stack is not counted in the next phase.
    #[inline(never)]
    pub(super) fn phase_end(phase: Phase, out: &mut ConOut) {
        let Some(depth) = scan() else {
            return;
        };
        note(phase, depth);
        let _ = writeln!(out, "{}", PhaseLine { phase, depth });
        let _ = paint();
    }

    /// The last phase, just before the first call: its line and the summary go straight to the
    /// SPCR's UART, after the last memory map, so no firmware call changes the map's key.
    #[inline(never)]
    pub(super) fn before_call(early_uart: EarlyUart) {
        if let Some(depth) = scan() {
            note(Phase::Call, depth);
            super::say_on_uart(
                early_uart,
                PhaseLine {
                    phase: Phase::Call,
                    depth,
                },
            );
        }
        super::say_on_uart(early_uart, record().summary());
    }

    fn record() -> Record {
        let deepest = Phase::from_number(DEEPEST_PHASE.load(Ordering::Relaxed))
            .map(|phase| (phase, Depth::decode(DEEPEST.load(Ordering::Relaxed))));
        Record::of(deepest)
    }

    fn note(phase: Phase, depth: Depth) {
        if let Some((phase, depth)) = record().note(phase, depth).deepest() {
            DEEPEST_PHASE.store(phase.number(), Ordering::Relaxed);
            DEEPEST.store(depth.encode(), Ordering::Relaxed);
        }
    }

    /// The paint as it stands, `None` while nothing is painted.
    fn painted() -> Option<Painted> {
        Painted::with_end(TOP.load(Ordering::Relaxed), END.load(Ordering::Relaxed))
    }

    /// Writes [`PAINT`] into every word from the promise's bottom up to [`stackdepth::MARGIN`]
    /// below this function's own RSP. No call is made while it paints, so nothing of its own lies
    /// below that RSP.
    #[inline(never)]
    fn paint() -> Option<Painted> {
        let painted = Painted::new(TOP.load(Ordering::Relaxed), rsp())?;
        for i in 0..painted.words() {
            let at = ptr::with_exposed_provenance_mut::<u64>(painted.word_address(i) as usize);
            // SAFETY: the word lies in the stack UEFI promises this image while boot services run
            // (UEFI 2.11 §2.3.4: 128 KiB or more below the entry RSP, `Painted::new`'s bottom),
            // and at least `MARGIN` bytes below this function's own RSP, so below every live
            // frame: the x64 calling convention has no red zone, and what an interrupt pushes
            // there is dead once it returns. It is 8-byte aligned (`Painted::new`).
            unsafe { at.write_volatile(PAINT) };
        }
        END.store(painted.end, Ordering::Relaxed);
        Some(painted)
    }

    /// The current paint's depth, scanned from its lowest word up.
    #[inline(never)]
    fn scan() -> Option<Depth> {
        let painted = painted()?;
        Some(stackdepth::scan(painted, |at| {
            let at = ptr::with_exposed_provenance::<u64>(at as usize);
            // SAFETY: `at` is a word of the paint [`paint`] wrote, inside the stack UEFI promises,
            // 8-byte aligned; reading it changes nothing.
            unsafe { at.read_volatile() }
        }))
    }

    /// Writes every word of a [`CALIBRATION_BYTES`] array on this function's own frame and
    /// returns the address of its lowest word: the calibration's known depth.
    #[inline(never)]
    fn calibrate() -> u64 {
        const WORDS: usize = CALIBRATION_BYTES / 8;
        let mut array = core::mem::MaybeUninit::<[u64; WORDS]>::uninit();
        let base = array.as_mut_ptr().cast::<u64>();
        for i in 0..WORDS {
            // SAFETY: `base` is the start of the local array, and `i` is below its length.
            let word = unsafe { base.add(i) };
            // SAFETY: `word` is a valid, aligned element of the local array; a volatile write so
            // the compiler keeps every one.
            unsafe { word.write_volatile(i as u64) };
        }
        base.addr() as u64
    }
}

/// TASKS item 128 (decision item 347, ADR-0002 Amendment 5): a bench build's minimal-program mode,
/// built only by `cargo xtask bench-build --minimal` (`cfg(origin_bench_minimal)`, beside
/// `origin_bench`). In order, after the first lines `efi_main` already wrote:
/// 1. the memory map, into a buffer static in the image, translated into a static region array:
///    no `AllocatePages` or `AllocatePool`;
/// 2. only the walks Amendment 4 needs (the SPCR, the FADT, the DSDT), its step 1 and its
///    addition's `SMIxC0`, behind the same gate; under QEMU, the stand-in table's values;
/// 3. the branch on `PMxC0`: the control (a triple fault, no call), or the call;
/// 4. the call through the live boot services table, after `X`;
/// 5. after `EFI_SUCCESS`, the status compare, then the triple fault, whatever the shutdown bits
///    say (owner item 349); after an error, the retry with `Y`; after a second, Amendment 2's
///    line and Amendment 3's order, with no `0xCF9` write.
///
/// It reads no kernel file, verifies nothing, starts no generator, allocates nothing, builds no
/// page tables, searches for no framebuffer, copies no boot services table, calls no `Stall`,
/// reads no UART register but the line status its lines wait on (the marks, and Amendment 3's
/// lines after the call), and writes nothing to `0xCF9`.
#[cfg(origin_bench_minimal)]
mod minimal {
    use core::fmt::Write;
    use core::mem::size_of;
    use core::ptr;
    use core::sync::atomic::Ordering;

    use origin_boot_info::{EarlyUart, MemoryRegion};
    use origin_boot_uefi::console::ConOut;
    use origin_boot_uefi::protocols::{
        BOOT_SERVICES_SIGNATURE, BootServices, ExitBootServicesFn, GetMemoryMapFn,
    };
    use origin_boot_uefi::uefi::{self, ExitOutcome, Handle, Status, SystemTable, TableHeader};
    use origin_boot_uefi::{bench, fch, spcr, uart};

    use super::{
        BenchPlan, FchSeen, MapFailure, PANIC_CONSOLE, bench_fch_read, bench_reset, find_rsdp,
        halt, mask_interrupts, read_described, read_memory_map, runtime_reset_system, say_halting,
        say_on_uart, triple_fault, wait_ticks,
    };

    /// The memory map's buffer, in 8-byte words: 64 KiB, static in the image (the coordinating
    /// session's ruling Q5).
    const MAP_WORDS: usize = 64 * 1024 / 8;
    /// The regions it is translated into.
    const REGIONS: usize = 1024;
    /// The control's spin after its line, so the line leaves the UART before the reset: 50 ms at
    /// [`bench::MINIMAL_TICKS_PER_MS`], by the counter, with no `Stall`.
    const CONTROL_SPIN: u64 = 50 * bench::MINIMAL_TICKS_PER_MS;
    /// The line before the call, the mode's last through the firmware's console.
    const LAST_LINE: &str = "origin-boot: bench build: minimal: ExitBootServices next, through the live boot services table";

    static mut MAP: [u64; MAP_WORDS] = [0; MAP_WORDS];
    static mut REGION_ARRAY: [MemoryRegion; REGIONS] = [MemoryRegion {
        base: 0,
        pages: 0,
        kind: 0,
        reserved: 0,
    }; REGIONS];

    /// The boot services table the firmware's system table names now, if its signature and
    /// `HeaderSize` check. Read each time it is needed: the mode keeps no copy.
    fn live(system_table: *const SystemTable) -> Option<*const BootServices> {
        // SAFETY: `system_table` is the firmware's own table, checked at entry; only the member's
        // address is formed.
        let member = unsafe { &raw const (*system_table).boot_services };
        // SAFETY: that member, in a table readable while boot services run; one volatile
        // pointer-sized load.
        let p = unsafe { member.read_volatile() }
            .cast::<BootServices>()
            .cast_const();
        if p.is_null() || !p.is_aligned() {
            return None;
        }
        // SAFETY: non-null and aligned; the firmware's table is readable while boot services run,
        // and its header's members are integers, valid for any bits.
        let hdr = unsafe { p.cast::<TableHeader>().read_volatile() };
        (hdr.signature == BOOT_SERVICES_SIGNATURE
            && hdr.header_size as usize >= size_of::<BootServices>())
        .then_some(p)
    }

    /// The live table's `GetMemoryMap`.
    fn live_get_memory_map(system_table: *const SystemTable) -> Option<GetMemoryMapFn> {
        let p = live(system_table)?;
        // SAFETY: inside the table `live` checked; only the member's address is formed.
        let member = unsafe { &raw const (*p).get_memory_map };
        // SAFETY: that member; an optional function pointer is valid for any bits, null being
        // `None`.
        unsafe { member.read_volatile() }
    }

    /// The live table's `ExitBootServices`.
    fn live_exit(system_table: *const SystemTable) -> Option<ExitBootServicesFn> {
        let p = live(system_table)?;
        // SAFETY: as for `live_get_memory_map`.
        let member = unsafe { &raw const (*p).exit_boot_services };
        // SAFETY: as for `live_get_memory_map`.
        unsafe { member.read_volatile() }
    }

    /// The mode, from its first map to the reset or the halt. Returns only if the boot services
    /// table does not check at the start, as `boot` does.
    pub(super) fn run(
        image: Handle,
        system_table: *const SystemTable,
        st: &SystemTable,
        out: &mut ConOut,
    ) -> Status {
        let _ = writeln!(out, "{}", bench::MINIMAL_LINE);
        if live(system_table).is_none() {
            let _ = writeln!(
                out,
                "origin-boot: bench build: minimal: the boot services table does not check"
            );
            return Status::INCOMPATIBLE_VERSION;
        }
        // SAFETY: the loader has one thread and `run` runs once, so these are the only references
        // to the two statics.
        let map =
            unsafe { core::slice::from_raw_parts_mut((&raw mut MAP).cast::<u8>(), MAP_WORDS * 8) };
        // SAFETY: as above.
        let regions = unsafe {
            core::slice::from_raw_parts_mut((&raw mut REGION_ARRAY).cast::<MemoryRegion>(), REGIONS)
        };
        let rsdp = find_rsdp(st);
        // Step 2: the first map.
        let first = live_get_memory_map(system_table)
            .ok_or(MapFailure::Firmware(Status::UNSUPPORTED))
            .and_then(|get_map| read_memory_map(get_map, map, regions));
        let (early_uart, register, seen, stand_in) = match first {
            Ok((_, n)) => {
                let described = regions.get(..n).unwrap_or(&[]);
                let mut read = |at: u64, bytes: &mut [u8]| read_described(described, at, bytes);
                let found = spcr::find(rsdp, &mut read);
                let _ = writeln!(out, "origin-boot: console: {found}");
                let register =
                    bench::find_reset(rsdp, &mut read, |at| bench::in_mmio(described, at));
                // Under QEMU only: the stand-in table (the coordinating session's ruling Q4).
                let table = fch::find_stand_in(rsdp, &mut read);
                let stand_in =
                    table.map(|(body, n)| fch::StandIn::from_table(body.get(..n).unwrap_or(&[])));
                let _ = writeln!(out, "{}", fch::stand_in_said(stand_in));
                let stand_in = stand_in.flatten();
                let seen = bench_fch_read(rsdp, described, stand_in.map(|s| s.pretend), true, out);
                (found.early_uart(), register, seen, stand_in)
            }
            Err(failure) => {
                let _ = failure.report(out);
                let _ = writeln!(
                    out,
                    "origin-boot: bench build: minimal: no first memory map (R128-13): no table walk and no FCH read, and the call is made with no control"
                );
                (
                    EarlyUart::NONE,
                    Err(bench::NoReset::NoMap),
                    FchSeen::not_read(fch::NotRead::NoFadt(bench::NoReset::NoMap)),
                    None,
                )
            }
        };
        // The register as this mode may use it: one at port 0xCF9 is named as skipped (R128-6).
        let _ = writeln!(
            out,
            "{}",
            bench::ResetLine(bench::minimal_register(register))
        );
        // Step 3: the branch.
        let _ = writeln!(out, "{}", fch::SmiLine(seen.smi));
        if let Some(registers) = seen.registers {
            let _ = writeln!(
                out,
                "{}",
                fch::RegistersLine {
                    registers,
                    source: seen.source,
                }
            );
        }
        let decision = fch::MinimalDecision::of(seen.registers.as_ref(), seen.pm1a_event);
        let _ = writeln!(out, "{}", fch::MinimalDecisionLine(decision));
        // The control only where both shutdown bits let a shutdown reset the system (refinement
        // 3); otherwise the call, whose triple fault follows whatever the bits say (item 349).
        if let fch::MinimalDecision::Control(_) = decision {
            wait_ticks(CONTROL_SPIN);
            mask_interrupts();
            triple_fault()
        }
        let stale = stand_in.map_or(0, |s| s.stale);
        let plan = BenchPlan {
            after: if stand_in.is_some_and(|s| s.no_reset_system) {
                bench::After::StandInNoReset
            } else {
                bench::After::Reset
            },
            under: None,
            reset_system: runtime_reset_system(st).ok(),
            register: bench::minimal_register(register),
            ticks_per_ms: bench::MINIMAL_TICKS_PER_MS,
            // Not read in this mode: Amendment 5, step 5, writes `X` straight before the call, with
            // no drain first, since the mode reads no UART register to drain it (R128-12).
            marks: false,
            free: None,
        };
        let _ = writeln!(out, "{LAST_LINE}");
        PANIC_CONSOLE.store(ptr::null_mut(), Ordering::Release);
        // Step 4: the call; step 5, after it.
        let mut calls = uart::ExitCalls::default();
        let outcome: ExitOutcome<MapFailure> = uefi::exit_boot_services(
            || {
                // No key without a live table whose `ExitBootServices` is there to call (R128-11).
                live_exit(system_table).ok_or(MapFailure::Firmware(Status::UNSUPPORTED))?;
                let get_map = live_get_memory_map(system_table)
                    .ok_or(MapFailure::Firmware(Status::UNSUPPORTED))?;
                read_memory_map(get_map, map, regions).map(|(key, _)| key)
            },
            |key| {
                // A stale key for the first `stale` calls (the stand-in table's, QEMU only).
                let key = bench::key_for(calls.made(), stale, key);
                say_on_uart(
                    early_uart,
                    if calls.made() == 0 {
                        bench::MARK_FIRST
                    } else {
                        bench::MARK_RETRY
                    },
                );
                let Some(exit) = live_exit(system_table) else {
                    return Status::UNSUPPORTED;
                };
                // SAFETY: valid `ExitBootServices` from the live boot services table, with this
                // image's handle and the key of the map just obtained; nothing has been allocated
                // since `GetMemoryMap`.
                let status = unsafe { exit(image, key) };
                // Amendment 5, step 6: after EFI_SUCCESS, the status compare, then the triple
                // fault; nothing comes between them, whatever the shutdown bits say (owner item
                // 349, "Forced fault anyway": no firmware call after a successful call).
                if status == Status::SUCCESS {
                    triple_fault();
                }
                calls.record(status);
                status
            },
        );
        match outcome {
            // A call that returned a status that is neither EFI_SUCCESS nor an error, which UEFI
            // 2.11 §7.4 does not give: boot services are gone, so no firmware call either (item
            // 349); a line, and the halt. Only EFI_SUCCESS is followed by the triple fault, so a
            // lost triple fault after a successful call shows as this halt (control MC-9).
            ExitOutcome::Exited => {
                mask_interrupts();
                say_on_uart(
                    early_uart,
                    "origin-boot: bench build: minimal: ExitBootServices returned neither EFI_SUCCESS nor an error: halting",
                );
                halt()
            }
            ExitOutcome::Halt => {
                mask_interrupts();
                // Decision item 342: why, on the UART, then Amendment 3's order, with the FADT's
                // step skipped at port 0xCF9 (the coordinating session's addition to Q3, which item
                // 349 keeps for the error path).
                say_halting(early_uart, calls);
                bench_reset(early_uart, &plan)
            }
            // `ExitBootServices` was not called, so the firmware's console may still be used.
            ExitOutcome::NotAttempted(failure) => {
                let _ = failure.report(out);
                let _ = writeln!(
                    out,
                    "origin-boot: bench build: minimal: no last memory map, or no live ExitBootServices, so no call can be made: halting"
                );
                halt()
            }
        }
    }
}

/// TASKS item 131 (decision item 352, ADR-0002 Amendment 6): the bisection mode, a bench build
/// made only by `cargo xtask bench-build --bisect` (`cfg(origin_bench_bisect)`, beside
/// `origin_bench`). It is the full bench loader's path, `boot`, with five changes, each at its own
/// `cfg` there:
/// 1. the kernel stack and the boot info allocated as `EfiLoaderData` ([`STACK_TYPE`],
///    [`BOOT_INFO_TYPE`]);
/// 2. a line, before the last one, counting the OS-defined types in the memory map the page tables
///    were built from ([`read_memory_map_counting`]);
/// 3. the minimal mode's decision and control on the FCH's record ([`decide`]), with
///    `BENCH-FCH`'s `mine` naming a shutdown;
/// 4. after `EFI_SUCCESS`, the triple fault and nothing before it; after a second error, Amendment
///    2's line and Amendment 3's order, with the FADT step skipped at port `0xCF9`;
/// 5. `BENCH-AFTER` read and its value ignored.
///
/// The call is made through the loader's copy of the boot services table, as every loader that
/// stopped made it. Nothing writes `0xCF9`: the full mode's control and reset are not compiled in.
#[cfg(origin_bench_bisect)]
mod bisect {
    use core::fmt::Write;

    use origin_boot_info::{EarlyUart, MemoryRegion};
    use origin_boot_uefi::console::ConOut;
    use origin_boot_uefi::protocols::GetMemoryMapFn;
    use origin_boot_uefi::{bench, fch, memmap};

    use super::{FchSeen, LoaderPorts, MapFailure, mask_interrupts, triple_fault};

    /// `GetMemoryMap` into `map` and its translation into `regions`, as `read_memory_map` makes
    /// them, the same one firmware call; and a count of the raw map's descriptors of an OS-defined
    /// type, read from `map` alone (Amendment 6, step 2; ruling Q6 of TASKS item 131). Returns the
    /// number of regions and the count.
    pub(super) fn read_memory_map_counting(
        get_map: GetMemoryMapFn,
        map: &mut [u8],
        regions: &mut [MemoryRegion],
    ) -> Result<(usize, memmap::OsTypes), MapFailure> {
        let mut size = map.len();
        let (mut key, mut dsize, mut dversion) = (0usize, 0usize, 0u32);
        // SAFETY: as in `read_memory_map`: a valid `GetMemoryMap` from the validated boot services,
        // still active; every pointer is to a live local or to the `map` buffer of `size` bytes.
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
            return Err(MapFailure::Firmware(status));
        }
        let raw = map.get(..size).unwrap_or(&[]);
        let n = memmap::translate(raw, dsize, regions).map_err(MapFailure::Rejected)?;
        Ok((n, memmap::os_defined(raw, dsize)))
    }

    /// Amendment 6, step 3, as Amendment 5's step 4: the FCH's lines and the decision on its record
    /// of the last reset. In the boot's first run, the control: the decision line on the wire,
    /// interrupts masked and a triple fault, and no call. Otherwise nothing of Amendment 4's follows
    /// the call, so there is no plan for it.
    pub(super) fn decide(
        seen: FchSeen,
        early_uart: EarlyUart,
        out: &mut ConOut,
    ) -> Option<fch::Free> {
        let _ = writeln!(out, "{}", fch::SmiLine(seen.smi));
        if let Some(registers) = seen.registers {
            let _ = writeln!(
                out,
                "{}",
                fch::RegistersLine {
                    registers,
                    source: seen.source,
                }
            );
        }
        let decision = fch::MinimalDecision::of(seen.registers.as_ref(), seen.pm1a_event);
        let _ = writeln!(out, "{}", fch::BisectDecisionLine(decision));
        if let fch::MinimalDecision::Control(_) = decision {
            // The line just written is on the wire before the reset can cut it, as the full mode's
            // control waits for it.
            if early_uart.is_present() {
                let _ = bench::drain(&mut LoaderPorts, early_uart.port);
            }
            mask_interrupts();
            triple_fault()
        }
        None
    }
}
