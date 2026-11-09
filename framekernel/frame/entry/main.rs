//! The kernel image's crate root: `_start`, where the boot loader hands over (M1).
//!
//! Everything after the hand-over is the library's `framekernel::kernel_main`, safe code on
//! the framework's typed interfaces. This file is the link root, where the one thing safe code
//! cannot express (a symbol the loader jumps to, and how the machine state gets into Rust)
//! happens, and it sits in the framework's directory for that reason. It is a target of the
//! `framekernel` package and not of `origin-frame` because the image has to link the kernel,
//! which is above the framework.
#![no_std]
#![no_main]

/// The ELF entry. A tiny naked shim so every backtrace terminates here: it zeroes RBP (so the
/// frame-pointer chain ends), aligns the stack, and calls [`framekernel::kernel_main`], leaving
/// the boot-info pointer the loader put in RDI where that function's `sysv64` signature takes
/// its [`origin_frame::early::Entry`] (`repr(transparent)` over the pointer). It never returns.
///
/// # Safety
///
/// Only the Origin boot loader may jump here, once, under the entry contract in
/// `origin_frame::early`.
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "sysv64" fn _start() -> ! {
    core::arch::naked_asm!(
        "xor ebp, ebp",
        "and rsp, -16",
        "call {main}",
        "ud2",
        main = sym framekernel::kernel_main,
    );
}
