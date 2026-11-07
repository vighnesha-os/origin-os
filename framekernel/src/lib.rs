//! Origin OS Framekernel: M1 entry, M2, and the first M3 steps (CLAUDE.md §19, §24–§28).
//!
//! The Origin boot loader jumps to `_start` (`src/main.rs`) after `ExitBootServices`, with
//! the physical address of an [`origin_boot_info::BootInfo`] in `rdi`. The kernel brings up
//! COM1, checks that the loader applied its relocations, validates the boot info, prints a
//! summary of it, loads its own GDT, TSS and IDT, proves exception handling with a
//! breakpoint, starts the physical frame allocator, switches to its own page tables (W^X
//! kernel image, no null page, a never-executable direct map), proves those protections
//! with deliberate faults and the allocator through the new mapping, starts the kernel heap
//! (so the `alloc` crate works) and proves it leak-free, brings up the local
//! APIC in x2APIC mode with its timer calibrated against the PIT and proven by taking
//! periodic interrupts, then starts every application processor the ACPI MADT lists (M3),
//! proves preemptive kernel threads on every processor, and halts.
//!
//! This library holds everything that can be unit-tested on the build host, all of it safe
//! code (`forbid(unsafe_code)`):
//! - [`acpi`]: RSDP, XSDT and MADT parsing (checksummed) for processor discovery;
//! - [`apic`]: x2APIC and 8254 PIT encodings and timer calibration arithmetic;
//! - [`boot`]: boot-info validation and the memory summary;
//! - [`channel`]: one channel's queues, limits and signals (ADR-0005 P8);
//! - [`clock`]: monotonic-clock arithmetic (ticks to nanoseconds and back) and the CPUID
//!   TSC rate;
//! - [`descriptors`]: GDT, TSS and IDT descriptor encodings;
//! - [`frames`]: the physical frame allocator over the boot-info regions;
//! - [`handles`]: per-process handle tables with rights (ADR-0005 P1–P2);
//! - [`heap`]: kernel heap bookkeeping (slabs and frame runs), addresses only;
//! - [`paging`]: 4-level page tables with structural W^X (the `origin-paging` crate,
//!   shared with the loader);
//! - [`pci`]: the MCFG, ECAM addressing, configuration headers, enumeration and BAR
//!   sizing over an abstract configuration space;
//! - [`runqueue`]: the per-CPU round-robin run queue (scheduler policy);
//! - [`serial`]: a polled 16550 UART driver over an abstract register interface;
//! - [`report`]: the exact console text;
//! - [`rtc`]: CMOS RTC decoding and UTC calendar arithmetic.
//!
//! The binary holds the entry point, x86 port I/O, the descriptor tables, exception and
//! interrupt stubs, MSR access and fault probes (`src/cpu.rs`), the APIC timer bring-up
//! (`src/timer.rs`), the spin lock (`src/sync.rs`), the global allocator (`src/memory.rs`),
//! the live address space (`src/vm.rs`), AP start-up and its trampoline (`src/smp.rs`), the
//! per-CPU scheduler (`src/sched.rs`), and the panic handler. All of it is original code written for this
//! project, with no third-party crates (CLAUDE.md §3, §5).
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod acpi;
pub mod apic;
pub mod boot;
pub mod channel;
pub mod clock;
pub mod descriptors;
pub mod frames;
pub mod handles;
pub mod heap;
pub use origin_paging as paging;
pub mod pci;
pub mod report;
pub mod rtc;
pub mod runqueue;
pub mod serial;
