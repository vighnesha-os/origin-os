//! FFI `bool` return regression test (CLAUDE.md §10; rust-lang/rust#163911).
//!
//! The function is the public reproducer from that issue. On Rust 1.99.0 for x86_64 it
//! compiled to `mov %edi,%eax; ret`, so a C caller received the unmasked byte (16 for 0x10)
//! instead of 0 or 1. `developer-tools/toolchain/verify.sh` disassembles it for every
//! x86-64 target Origin uses and requires the value to be reduced to 0/1 before returning.
#![no_std]

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_fn_returning_bool(flags: u8) -> bool {
    (flags & 0x1) == 0x1
}
