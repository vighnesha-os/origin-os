//! Sizes of the kernel's fixed per-CPU tables, and the one reserved APIC ID.

/// Processors the kernel indexes: every per-CPU array has this many slots.
pub const MAX_CPUS: usize = 64;

/// The x2APIC broadcast destination in physical mode (all ones): an IPI to it reaches every
/// processor, so it is never one processor's ID.
pub const BROADCAST: u32 = u32::MAX;
