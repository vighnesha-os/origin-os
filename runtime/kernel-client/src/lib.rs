//! The Rust bindings of `origin.kernel` for a program in ring 3 (tasks S4-1 and S4-2; ADR-0011
//! N5 and Amendment 2, L3).
//!
//! `oidl generate` writes them from `sdk/interfaces/origin.kernel.oidl` into the target
//! directory at each ring-3 build (`developer-tools/xtask/src/ring3.rs`), and this file includes
//! them: the generator's runtime (`oidl_rt`), the library's types, its safe client, and
//! `abi.rs`, the C-ABI imports the client calls. The included `modules.rs` names each module's
//! file by its absolute path.
//!
//! There is no stub here, and so no `syscall` and no call number. Each import,
//! `origin_kernel_<method>`, is an entry point of `origin.core`: the program is linked against
//! that library, the linker gives each import a slot in the program's
//! read-only-after-relocation data, and the kernel's launch path writes the entry point's
//! address into the slot before the program's first thread exists (L3). A call goes through
//! the slot into `origin.core`, whose stub alone may make it (ADR-0005 B2).
#![no_std]

include!(concat!(
    env!(
        "ORIGIN_KERNEL_CLIENT",
        "build the ring-3 crates with `cargo xtask`, which generates origin.kernel's client and names its directory in ORIGIN_KERNEL_CLIENT"
    ),
    "/modules.rs"
));
