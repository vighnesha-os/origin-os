//! UEFI smoke test (CLAUDE.md §9): the compiler alone, without Cargo, links an EFI application.
//!
//! `developer-tools/toolchain/verify.sh` builds this for `x86_64-unknown-uefi` at opt-level 3
//! and reads the image's PE header.
//!
//! It still holds a NUL-terminated 16-bit length loop, which LLVM's loop-idiom pass rewrites
//! into a `wcslen` call, and which Rust 1.99.0 failed to link ("undefined symbol: wcslen",
//! rust-lang/rust#163614). §10's test for that regression is no longer here: it is
//! `char16_len` in the loader's library (`boot/uefi/src/uefi.rs`), linked into the loader
//! itself, as §10 asks.
#![no_std]
#![no_main]

use core::ffi::c_void;
use core::panic::PanicInfo;

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

/// Number of `u16` units before the terminating NUL of a UEFI `CHAR16` string.
///
/// # Safety
/// `s` must point to readable memory that contains a `0u16` terminator.
#[inline(never)]
unsafe fn char16_len(s: *const u16) -> usize {
    let mut n = 0usize;
    // SAFETY: the caller guarantees every unit up to and including the terminator is readable.
    while unsafe { *s.add(n) } != 0 {
        n += 1;
    }
    n
}

/// "Origin" as UCS-2, NUL-terminated.
static NAME: [u16; 7] = [0x4F, 0x72, 0x69, 0x67, 0x69, 0x6E, 0];

/// UEFI entry point. Returns `EFI_SUCCESS` (0) when the length is computed correctly.
#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(_image: *mut c_void, _system_table: *mut c_void) -> usize {
    let p = core::hint::black_box(NAME.as_ptr());
    // SAFETY: `NAME` is a static array ending in a 0 terminator.
    let len = unsafe { char16_len(p) };
    if len == 6 { 0 } else { 1 }
}
