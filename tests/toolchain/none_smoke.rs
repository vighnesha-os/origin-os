//! Freestanding x86-64 smoke test for the Framekernel target (CLAUDE.md §9).
//!
//! `developer-tools/toolchain/verify.sh` builds this for `x86_64-unknown-none` at opt-level 3,
//! once through the rustup proxy and once by running the toolchain's own `rustc` directly,
//! which exercised the macOS `rust-lld` regression in Rust 1.99.0 (rust-lang/rust#163947).
#![no_std]
#![no_main]

use core::panic::PanicInfo;

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

/// Entry symbol the linker expects for this target.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let mut buf = [0u8; 256];
    let src = core::hint::black_box([0x5Au8; 256]);
    // Copies through the target's compiler-builtins memory routines.
    buf.copy_from_slice(&src);
    core::hint::black_box(&buf);
    loop {
        core::hint::spin_loop();
    }
}
