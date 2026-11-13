//! Today's generated code for `origin.kernel`, linked into each of the kernel's Rust proof
//! programs (task S4-1; ADR-0011 Amendment 2, L8).
//!
//! `oidl generate` writes it from `sdk/interfaces/origin.kernel.oidl` into the target directory
//! at each ring-3 build (`developer-tools/xtask/src/ring3.rs`), and this file includes it: the
//! generator's runtime (`oidl_rt`), the library's types, its safe client, and the two shims
//! (`abi.rs`, the imports the client calls, and `core_stubs.rs`, one `syscall` per call). The
//! included `modules.rs` names each module's file by its absolute path.
//!
//! A proof program that links this runs under ADR-0005 A6: the kernel accepts its calls from
//! anywhere in its one read-execute segment. Task S4-2 moves the stubs into `origin.core`, with
//! each call pinned to its own address (L6, L7); proof programs keep A6 (L8).
#![no_std]

include!(concat!(
    env!(
        "ORIGIN_KERNEL_STUBS",
        "build the ring-3 crates with `cargo xtask`, which generates origin.kernel's code and names its directory in ORIGIN_KERNEL_STUBS"
    ),
    "/modules.rs"
));
