//! The one reserved APIC ID. No processor count is compiled into the kernel (ADR-0008 Amendment 1,
//! D5): per-processor tables are sized when the processors are discovered (`percpu`).
#![forbid(unsafe_code)]

/// The x2APIC broadcast destination in physical mode (all ones): an IPI to it reaches every
/// processor, so it is never one processor's ID.
pub const BROADCAST: u32 = u32::MAX;
