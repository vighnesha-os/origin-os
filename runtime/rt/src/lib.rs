//! The Origin runtime for Rust programs in ring 3 (task S4-1; CLAUDE.md §11, §44; ADR-0005 P6).
//!
//! What a `no_std` program needs from Origin before it can run, and nothing more:
//!
//! - `_start`, where the kernel starts the first thread with the start record's address in RDI
//!   (P6). It aligns the stack, decodes the record with `origin-abi` and calls the program's
//!   `main` (named with [`main!`]), then ends the thread with what `main` returned.
//! - A panic handler that ends the thread with [`PANIC_EXIT`] (ONBF §6.1.1: `panic=abort`).
//! - `memcpy`, `memmove`, `memset`, `memcmp` and `bcmp`, which the compiler calls and which no
//!   C library supplies here (§11).
//!
//! The runtime ends a thread with three codes of its own: [`PANIC_EXIT`] (`u64::MAX`),
//! [`BAD_RECORD_EXIT`] (`u64::MAX - 1`) and [`UNWIND_EXIT`] (`u64::MAX - 2`). They share the space
//! of the codes a program's `main` returns: a program that returns one of them cannot be told
//! from the runtime's ending by its code alone (S4-1's review, finding F11). They are this
//! runtime's own convention and nothing may depend on them: `origin.core` reserves no exit code
//! at major 0, and `thread_exit`'s code is the program's, the whole `u64` (ADR-0011 Amendment 3,
//! L14). How a process's end is reported, an exit apart from a panic and from a fault, is
//! decided with ADR-0005 P10's crash record.
//!
//! The runtime reaches the kernel as every program does: through `origin.core`, by the imports
//! of `origin-kernel-client`, which the launch path binds (ADR-0011 Amendment 2, L3). It holds
//! no stub and no `syscall`.
//!
//! No allocator, no thread-local storage and nothing that reads the environment (ADR-0005 E2).
//! `unsafe` is only in `entry.rs` and `mem.rs`, the two files `cargo xtask check`'s gate lists for
//! this crate; the rest denies it.
#![no_std]
// The `mem*` functions must not be compiled into calls to themselves.
#![no_builtins]
#![deny(unsafe_code)]

#[allow(unsafe_code)]
mod entry;
#[allow(unsafe_code)]
mod mem;
mod panic;

pub use entry::{BAD_RECORD_EXIT, Start, UNWIND_EXIT, entry_address};
pub use panic::PANIC_EXIT;
