//! `origin-frame`: the Framekernel's privileged framework (ADR-0011 N6; ADR-0001 R1;
//! CLAUDE.md §13, §31).
//!
//! This is the one crate of the kernel where `unsafe` may appear. Everything else, the
//! drivers and the services included, is safe Rust (`#![forbid(unsafe_code)]`) built on the
//! typed interfaces below, so a memory-safety argument for the kernel only has to be made
//! here. `cargo xtask check` enforces that: `unsafe` outside `framekernel/frame/` fails the
//! build (the kernel image's link root, `entry/main.rs`, is here for that reason). Dependencies
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

pub mod acpi;
pub mod addr;
pub mod aliases;
pub mod amdvi;
pub mod apic;
pub mod apstart;
pub mod bitmap;
pub mod boot;
pub mod clock;
pub mod cpuident;
pub mod cpumap;
pub mod descriptors;
pub mod dmar;
pub mod features;
pub mod frames;
pub mod heap;
pub mod irq;
pub mod ivrs;
pub mod limits;
pub mod mmio;
pub mod nospec;
pub mod objref;
pub mod once;
pub mod pci;
pub mod percpu;
pub mod reserved;
pub mod speculation;
pub mod tablebound;
pub mod told;
pub mod topology;
pub mod trap;
pub mod unwind;
pub mod usercopy;
pub mod vtd;

#[cfg(test)]
mod testing;

#[cfg(target_os = "none")]
pub mod cpu;
#[cfg(target_os = "none")]
pub mod dma;
#[cfg(target_os = "none")]
pub mod early;
#[cfg(target_os = "none")]
pub mod firmware;
#[cfg(target_os = "none")]
pub mod iommu;
#[cfg(target_os = "none")]
pub mod memory;
#[cfg(target_os = "none")]
pub mod monotonic;
#[cfg(target_os = "none")]
pub mod phys;
pub mod port;
#[cfg(target_os = "none")]
pub mod proof_programs;
#[cfg(target_os = "none")]
pub mod random;
#[cfg(target_os = "none")]
pub mod smp;
#[cfg(target_os = "none")]
pub mod stack;
#[cfg(target_os = "none")]
pub mod timer;
#[cfg(target_os = "none")]
pub mod tlb;
#[cfg(target_os = "none")]
pub mod userspace;
#[cfg(target_os = "none")]
pub mod vm;
