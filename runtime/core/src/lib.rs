//! `origin.core`: the kernel's only door (ADR-0011 N5; Amendment 2, L2 to L6; task S4-2).
//!
//! It holds the generated entry points of `origin.kernel` and nothing else: for each call one
//! naked function, `origin_kernel_<method>`, with the C calling convention on the outside and
//! the system-call convention of ADR-0005 P4 on the inside, and exactly one `syscall`
//! instruction, so that the kernel can accept each call number from its own address alone
//! (ADR-0005 B2). `oidl generate` writes them from `sdk/interfaces/origin.kernel.oidl` into the
//! target directory at each ring-3 build (`developer-tools/xtask/src/ring3.rs`), and this file
//! includes that one file. The call numbers are private to the release (CLAUDE.md §42): they
//! are here and in the kernel's dispatcher, and in no program.
//!
//! No state, no allocator, no helper, and nothing that reads an environment (L4, L5; ADR-0005
//! E2). It has no writable data and nothing to relocate (L2), which `onbf-pack corelib` holds
//! the linked image to, with the rule that its code is the listed stubs and the linker's padding
//! and not a byte more. `cargo xtask check` then reads the image again, instruction by
//! instruction, against the definition (`developer-tools/xtask/src/corestubs.rs`).
//!
//! This file holds no `unsafe` of its own. Each generated stub is `#[unsafe(naked)]`, with its
//! reason beside it.
#![no_std]

include!(concat!(
    env!(
        "ORIGIN_CORE_STUBS",
        "build origin.core with `cargo xtask`, which generates origin.kernel's stubs and names their directory in ORIGIN_CORE_STUBS"
    ),
    "/origin_kernel/kernel.rs"
));

/// What a `no_std` library linked by itself must name, and nothing ever reaches: the stubs are
/// assembly and cannot panic. Nothing refers to it, so the linker leaves it out, and the
/// description is refused if the image holds a byte that is not a stub's.
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
