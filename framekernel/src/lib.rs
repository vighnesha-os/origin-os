//! Origin OS Framekernel, M1 entry (CLAUDE.md §19, §24).
//!
//! The Origin boot loader jumps to `_start` (`src/main.rs`) after `ExitBootServices`, with
//! the physical address of an [`origin_boot_info::BootInfo`] in `rdi`. At this milestone the
//! kernel brings up COM1, checks that the loader applied its relocations, validates the boot
//! info, prints a summary of it and halts. It installs no page tables, GDT or IDT yet.
//!
//! This library holds everything that can be unit-tested on the build host:
//! - [`boot`]: boot-info validation and the memory summary;
//! - [`serial`]: a polled 16550 UART driver over an abstract register interface;
//! - [`report`]: the exact console text.
//!
//! The binary holds only the entry point, x86 port I/O and the panic handler. All of it is
//! original code written for this project, with no third-party crates (CLAUDE.md §3, §5).
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod boot;
pub mod report;
pub mod serial;
