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
//!
//! # What a stub is not, recorded until each is decided
//!
//! From the independent review of 2026-11-13 (findings F11, F16, and its second question):
//!
//! - **No frame.** All Origin code is built with frame pointers (ONBF §6.1.1), and the flag is
//!   kept for this crate too, but a naked function has no frame. A frame-pointer walk that
//!   starts inside a stub therefore skips the stub's caller unless it takes the return address
//!   at RSP first. That is for whatever writes ADR-0005 P10's crash record to do.
//! - **No `ENDBR64`.** A stub is reached by an indirect call through an import slot. When
//!   indirect-branch tracking is turned on for ring 3 (CLAUDE.md §14) each stub gains one, and
//!   the generator, the gate's expected form and every offset the kernel holds change with it.
//! - **No trap after its `ret`.** The stubs are aligned to 4 bytes: of the 19 that return, 12
//!   are followed at once by the next stub's first instruction, 6 by the linker's `int3`, and
//!   the last by the rest of the page, which the kernel fills with `int3`. ADR-0017's rule on
//!   returns is written for the kernel and the framework and says nothing of ring 3 either way;
//!   it is TASKS item 49, to be decided before a ring-3 program that takes hostile input runs.
//!   By reading, a stub loads nothing from memory.
//! - **One interface of one library.** The build refuses a second `syscall` interface, with
//!   its reason (`ring3::one_interface`): this file includes one file of stubs by name.
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
