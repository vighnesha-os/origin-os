//! `origin-acpi-tables`: the system description tables of ACPI 6.6 §5.2 that a platform in
//! Origin's declared scope (x86-64) can present, parsed and checked as untrusted input
//! (ADR-0019 A1 and A6, area A; TASKS item 62).
//!
//! Parser contract, as `onbf-format`'s: `no_std`, no allocation, no `unsafe`, no panics on any
//! input, typed errors that name the table and the byte offset at fault. The lints below make
//! indexing, unchecked arithmetic and truncating casts compile errors outside tests, and
//! `cargo xtask check` builds this crate's release object for the kernel's target and refuses any
//! reference to a panic path in it.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        clippy::cast_possible_truncation
    )
)]

pub mod census;
pub mod definition;
mod error;
pub mod facs;
pub mod fadt;
pub mod gas;
pub mod header;
pub mod madt;
mod read;
pub mod root;
mod walk;

#[cfg(test)]
mod testing;

pub use error::{Error, ErrorKind, Signature};
pub use read::{Guid, sums_to_zero};
pub use walk::Raw;
