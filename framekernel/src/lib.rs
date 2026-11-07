//! Origin OS Framekernel: M1 entry and the first M2 steps (CLAUDE.md §19, §24, §28).
//!
//! The Origin boot loader jumps to `_start` (`src/main.rs`) after `ExitBootServices`, with
//! the physical address of an [`origin_boot_info::BootInfo`] in `rdi`. The kernel brings up
//! COM1, checks that the loader applied its relocations, validates the boot info, prints a
//! summary of it, loads its own GDT, TSS and IDT, proves exception handling with a
//! breakpoint, starts the physical frame allocator and proves it, and halts. It still runs
//! on the firmware's page tables; [`paging`] builds the kernel's own, which a later step
//! loads.
//!
//! This library holds everything that can be unit-tested on the build host, all of it safe
//! code (`forbid(unsafe_code)`):
//! - [`boot`]: boot-info validation and the memory summary;
//! - [`descriptors`]: GDT, TSS and IDT descriptor encodings;
//! - [`frames`]: the physical frame allocator over the boot-info regions;
//! - [`paging`]: 4-level page tables with structural W^X;
//! - [`serial`]: a polled 16550 UART driver over an abstract register interface;
//! - [`report`]: the exact console text.
//!
//! The binary holds the entry point, x86 port I/O, the descriptor tables and exception
//! stubs (`src/cpu.rs`), and the panic handler. All of it is original code written for this
//! project, with no third-party crates (CLAUDE.md §3, §5).
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod boot;
pub mod descriptors;
pub mod frames;
pub mod paging;
pub mod report;
pub mod serial;
