//! Origin OS UEFI boot loader, M1 step 1 (CLAUDE.md §19; handoff task T8).
//!
//! This step validates the system table the firmware passes to `efi_main` and reports
//! itself on the UEFI console. Loading and verifying the Framekernel, the memory map and
//! `ExitBootServices` come in M1 step 2.
//!
//! The library holds the loader's UEFI definitions and everything that can be unit-tested
//! on the build host. `src/main.rs` holds only the entry point and the panic handler.
//! It is original code written against the UEFI 2.11 specification, with no third-party
//! crates (CLAUDE.md §3, §5).
#![cfg_attr(not(test), no_std)]

pub mod console;
pub mod text;
pub mod uefi;
