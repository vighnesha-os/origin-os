//! Origin OS UEFI boot loader, M1 step 2 (CLAUDE.md §19; ADR-0002, Proposed).
//!
//! The loader validates the system table, reads `\EFI\ORIGIN\KERNEL.ONBF` from its own
//! volume, verifies it completely (ADR-0002 B2), places it in memory with its relocations,
//! fills in the boot info, exits boot services and enters the Framekernel.
//!
//! The library holds the UEFI definitions and every step that can be unit-tested on the
//! build host: memory-map translation, placement and relocation, and verification.
//! `src/main.rs` holds the firmware calls, the entry point and the panic handler. It is
//! original code written against the UEFI 2.11 specification, with no third-party crates
//! (CLAUDE.md §3, §5).
#![cfg_attr(not(test), no_std)]

pub mod console;
pub mod kernel;
pub mod memmap;
pub mod place;
pub mod protocols;
pub mod text;
pub mod uefi;
