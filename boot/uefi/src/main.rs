//! `efi_main`, the entry point of the Origin OS UEFI loader (M1 step 1).
//!
//! The loader validates the system table, reports itself and the firmware on the UEFI
//! console, and returns `EFI_UNSUPPORTED` to the firmware: it cannot load a Framekernel
//! yet, and it does not pretend to. `cargo xtask boot-test` asserts on this output in QEMU.
#![no_std]
#![no_main]

use core::fmt::Write;
use core::panic::PanicInfo;
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

use origin_boot_uefi::console::ConOut;
use origin_boot_uefi::uefi::{
    self, Handle, Revision, SimpleTextOutputProtocol, Status, SystemTable,
};

/// The console the panic handler writes to. Set by `efi_main` once the system table has
/// been validated, and must be cleared before `ExitBootServices` (M1 step 2).
static PANIC_CONSOLE: AtomicPtr<SimpleTextOutputProtocol> = AtomicPtr::new(ptr::null_mut());

/// Longest firmware vendor name the loader prints, in `CHAR16` units.
const VENDOR_UNITS: usize = 64;

/// Image entry point; the `x86_64-unknown-uefi` target links it as the PE entry.
///
/// # Safety
///
/// Only the firmware may call this, as the image entry point (UEFI 2.11 §4.1), with boot
/// services active, on the boot processor. `system_table` must be the pointer the firmware
/// passes there: null or a readable `EFI_SYSTEM_TABLE`, which the loader then validates.
#[unsafe(no_mangle)]
pub unsafe extern "efiapi" fn efi_main(_image: Handle, system_table: *const SystemTable) -> Status {
    // SAFETY: the caller contract above is exactly `read_system_table`'s precondition;
    // the function checks null, alignment, signature, revision and size before use.
    let st = match unsafe { uefi::read_system_table(system_table) } {
        Ok(st) => st,
        Err(e) => return e.status(),
    };
    // SAFETY: `con_out` comes from the validated system table, and the loader runs on the
    // boot processor with boot services active for the whole of step 1.
    let Some(mut out) = (unsafe { ConOut::new(st.con_out) }) else {
        return Status::UNSUPPORTED;
    };
    PANIC_CONSOLE.store(st.con_out, Ordering::Release);

    match report(&st, &mut out) {
        Ok(()) => Status::UNSUPPORTED,
        Err(_) => out.last_error().unwrap_or(Status::DEVICE_ERROR),
    }
}

/// Prints the loader banner, the firmware's identity and the loader's status.
fn report(st: &SystemTable, out: &mut ConOut) -> core::fmt::Result {
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
    writeln!(out, ", revision {:#010x}", st.firmware_revision)?;
    writeln!(
        out,
        "origin-boot: Framekernel loading is not implemented yet (M1 step 2); returning EFI_UNSUPPORTED"
    )
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let p = PANIC_CONSOLE.load(Ordering::Acquire);
    // SAFETY: `p` is null or the console `efi_main` stored from the validated system
    // table, and boot services are still active: step 1 never exits them.
    if let Some(mut out) = unsafe { ConOut::new(p) } {
        let _ = writeln!(out, "\norigin-boot: panic: {info}");
    }
    loop {
        core::hint::spin_loop();
    }
}
