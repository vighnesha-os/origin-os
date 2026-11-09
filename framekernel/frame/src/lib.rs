//! `origin-frame`: the Framekernel's privileged framework (ADR-0011 N6; ADR-0001 R1;
//! CLAUDE.md §13, §31).
//!
//! This is the one crate of the kernel where `unsafe` may appear. Everything else, the
//! drivers and the services included, is safe Rust (`#![forbid(unsafe_code)]`) built on the
//! typed interfaces below, so a memory-safety argument for the kernel only has to be made
//! here. `cargo xtask check` enforces that: `unsafe` outside `framekernel/frame/` fails the
//! build, except in the files named in `developer-tools/xtask/unsafe-allowlist.txt`, which
//! are the modules not yet migrated and which that list can only ever shrink by. Dependencies
//! run one way: nothing this crate depends on may depend on the rest of the kernel.
//!
//! The rule for every safe item exported here: **no safe caller can break memory safety
//! through it**, whatever arguments it passes, in whatever order, from whichever processor.
//! An operation that cannot be typed that way is an `unsafe fn` with a stated contract, used
//! only inside this crate. Each `unsafe` block carries a `SAFETY:` argument.
//!
//! The locks live in the package `origin-sync`, in this directory, re-exported as [`sync`]:
//! it stays its own package because the Loom models build it alone (`cargo xtask loom`).
//!
//! Host builds: this crate compiles on the build host so that its logic is unit-tested
//! there; everything that touches the processor is compiled for the kernel target only
//! (`#[cfg(target_os = "none")]`).
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub use origin_sync as sync;

pub mod addr;
pub mod apic;
pub mod clock;
pub mod descriptors;
pub mod features;
pub mod frames;
pub mod heap;
pub mod irq;
pub mod limits;
pub mod mmio;
pub mod once;
pub mod trap;
pub mod unwind;
pub mod usercopy;

#[cfg(test)]
mod testing;

#[cfg(target_os = "none")]
pub mod cpu;
#[cfg(target_os = "none")]
pub mod dma;
#[cfg(target_os = "none")]
pub mod memory;
#[cfg(target_os = "none")]
pub mod monotonic;
#[cfg(target_os = "none")]
pub mod phys;
#[cfg(target_os = "none")]
pub mod port;
#[cfg(target_os = "none")]
pub mod timer;
#[cfg(target_os = "none")]
pub mod tlb;
#[cfg(target_os = "none")]
pub mod vm;
