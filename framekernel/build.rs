//! Link arguments that give the Framekernel ELF the shape `onbf-pack` converts to KRN
//! (ONBF §6.1.2, §6.5).
//!
//! For `x86_64-unknown-none`, rustc 1.99.1 runs rust-lld with `-pie --eh-frame-hdr
//! -z noexecstack --gc-sections -z relro -z now -O1 --strip-debug` (observed with
//! `--print link-args`): a PIE, but without `--no-dynamic-linker` or `-z text`. The
//! arguments below come after rustc's own, and in lld the last of two opposing `-z` options
//! wins, so `-znorelro` overrides `-z relro`. They are scoped to this crate's binary and
//! emitted only for the kernel target; the library is also built for the host, for its
//! unit tests, where these options mean nothing.

/// The target the kernel binary is built for.
const KERNEL_TARGET: &str = "x86_64-unknown-none";

/// What each argument guarantees in the output ELF.
const LINK_ARGS: &[&str] = &[
    // The entry point is `_start` (lld's default, stated so it cannot drift).
    "--entry=_start",
    // No PT_INTERP: nothing loads this image but the Origin boot loader.
    "--no-dynamic-linker",
    // No text relocations: every relocation target is in a writable segment.
    "-ztext",
    // Every PT_LOAD starts on its own 4 KiB page, in the file and in memory, so no page
    // holds two segments with different permissions (ONBF §6.1).
    "-zseparate-loadable-segments",
    "-zmax-page-size=4096",
    "-zcommon-page-size=4096",
    // No PT_GNU_RELRO. The boot loader maps segments only as R, RX or RW.
    "-znorelro",
    // PT_GNU_STACK, if emitted, is not executable.
    "-znoexecstack",
];

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var("TARGET").as_deref() != Ok(KERNEL_TARGET) {
        return;
    }
    for arg in LINK_ARGS {
        println!("cargo::rustc-link-arg-bins={arg}");
    }
}
