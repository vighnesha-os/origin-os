//! Origin OS Framekernel: M1 entry, M2, and the first M3 steps (CLAUDE.md §19, §24–§28).
//!
//! The Origin boot loader jumps to `_start` (`frame/entry/main.rs`) after `ExitBootServices`, with
//! the physical address of an [`origin_boot_info::BootInfo`] in `rdi`. The kernel brings up
//! its console (the firmware's framebuffer and a UART, ADR-0010), checks that the loader
//! applied its relocations, validates the boot info, prints a summary of it, loads its own
//! GDT, TSS and IDT, proves exception handling with a
//! breakpoint, starts the physical frame allocator, switches to its own page tables (W^X
//! kernel image, no null page, a never-executable direct map), proves those protections
//! with deliberate faults and the allocator through the new mapping, starts the kernel heap
//! (so the `alloc` crate works) and proves it leak-free, brings up the local
//! APIC in x2APIC mode with its timer calibrated against the PIT and proven by taking
//! periodic interrupts, then starts every application processor the ACPI MADT lists (M3),
//! proves preemptive kernel threads on every processor, and halts.
//!
//! This library is the kernel above the privileged framework (`origin-frame`, the
//! `framekernel/frame/` crate, ADR-0011 N6), all of it safe code: `forbid(unsafe_code)` at the
//! crate root covers every module. What the framework needs is re-exported here from it.
//! The logic unit-tested on the build host:
//! - [`acpi`]: RSDP, XSDT and MADT parsing (checksummed) for processor discovery;
//! - [`amdvi`]: AMD IOMMU (AMD-Vi) register, table, command and event encodings and a host
//!   page-table walker (DMA and interrupt remapping);
//! - [`apic`]: x2APIC and 8254 PIT encodings and timer calibration arithmetic;
//! - [`apstart`]: the start-up handshake with one application processor, settled once by
//!   the AP's answer or the boot processor's deadline;
//! - [`base64`]: RFC 4648 Base64 encode/decode (whitespace-tolerant), for PEM and tokens;
//! - [`bitmap`]: a fixed-size bit allocator (free-space, ids) with lowest-free alloc;
//! - [`boot`]: boot-info validation and the memory summary;
//! - [`blockcache`]: the block cache's LRU index and write-back bookkeeping;
//! - [`cache`]: the CPU cache hierarchy from CPUID leaf 04h;
//! - [`channel`]: one channel's queues, limits and signals (ADR-0005 P8);
//! - [`clock`]: monotonic-clock arithmetic (ticks to nanoseconds and back) and the CPUID
//!   TSC rate;
//! - [`cpumap`]: dense CPU indexes for the processors the MADT enables (per-CPU arrays are
//!   indexed by them; APIC IDs stay the IPI and interrupt destinations);
//! - [`deflate`]: DEFLATE (RFC 1951) decompression with a decompression-bomb guard;
//! - [`descriptors`]: GDT, TSS and IDT descriptor encodings;
//! - [`dhcp`]: a DHCP client (message build/parse and the lease state machine);
//! - [`dmar`]: the ACPI DMAR table (Intel VT-d units, device scopes, reserved memory);
//! - [`dns`]: DNS message parsing with bounded name decompression;
//! - [`editbuffer`]: a gap buffer for editable text (caret, insert/delete);
//! - [`entropy`]: bounded-retry hardware entropy draw/fill over an abstract source;
//! - [`fbconsole`]: the text console on the firmware's framebuffer (geometry, glyph placement
//!   and the byte rules, drawn through an abstract pixel sink);
//! - [`features`]: CPUID feature decoding and the x86-64-v3 baseline;
//! - [`font`]: the original 8 by 16 ASCII console font;
//! - [`frames`]: the physical frame allocator over the boot-info regions;
//! - [`glob`]: glob pattern matching (`*`, `?`, `[...]`, escapes) for names and paths;
//! - [`gpt`]: GUID Partition Table validation (both copies, CRC-32) and partitions;
//! - [`handles`]: per-process handle tables with rights (ADR-0005 P1–P2);
//! - [`hashmap`]: a string-keyed open-addressing hash map (FNV-1a) for name tables;
//! - [`heap`]: kernel heap bookkeeping (slabs and frame runs), addresses only;
//! - [`hpet`]: the ACPI HPET table and main-counter arithmetic;
//! - [`initramfs`]: extracting a ustar archive into the VFS, with a path-traversal guard;
//! - [`ioapic`]: I/O APIC redirection-table encoding;
//! - [`ivrs`]: the ACPI IVRS table (AMD IOMMUs, their device entries, memory definitions);
//! - [`json`]: a depth-limited JSON parser (config, manifests, SDK data);
//! - [`klog`]: a bounded kernel log ring (level + inline message, drop-oldest);
//! - [`net`]: Ethernet/IPv4/ICMP/ARP headers and the Internet checksum;
//! - [`numa`]: a locality map (CPU/address → node, nearest-node order) from the SRAT/SLIT;
//! - [`originfs`]: a read-only OriginFS on-disk parser and image builder (ADR-0006,
//!   Proposed; host-tested, not mounted by the kernel pending owner approval, §84);
//! - [`tcp`]: TCP segments and a minimal connection state machine;
//! - [`text`]: a streaming, error-locating UTF-8 decoder for the text engine;
//! - [`tftp`]: a TFTP reader (RFC 1350): read requests, acknowledgements and a download
//!   state machine;
//! - [`timers`]: a monotonic-deadline timer queue;
//! - [`topology`]: CPU topology (thread/core/module/tile/die/package, and core types) from
//!   CPUID leaves 1Fh, 0Bh and 80000026h;
//! - [`nvme`]: NVMe controller registers, queue entries and indexes, Identify data;
//! - [`paging`]: 4-level page tables with structural W^X (the `origin-paging` crate,
//!   shared with the loader);
//! - [`pci`]: the MCFG, ECAM addressing, configuration headers, enumeration and BAR
//!   sizing over an abstract configuration space;
//! - [`ps2`]: PS/2 set-1 scancode decoding and the US keyboard layout;
//! - [`raster`]: a 2D software rasterizer (fills, blits, alpha blend) for the UI;
//! - [`runqueue`]: the per-CPU round-robin run queue (scheduler policy);
//! - [`semver`]: semantic-version parsing and precedence (package dependency resolution);
//! - [`serial`]: a polled 16550 UART driver over an abstract register interface;
//! - [`report`]: the exact console text;
//! - [`spcr`]: the ACPI SPCR table (the firmware's serial console, its address and speed);
//! - [`ring`]: a fixed-capacity ring buffer (input/audio/log queues);
//! - [`route`]: an IPv4 longest-prefix-match routing table (a bit trie);
//! - [`rtc`]: CMOS RTC decoding and UTC calendar arithmetic;
//! - [`unwind`]: frame-pointer stack unwinding for panic diagnostics;
//! - [`usercopy`]: user-pointer range and page checks for system calls (ADR-0005 P4);
//! - [`ustar`]: a read-only ustar (tar) archive reader for an initramfs/package payload;
//! - [`utf16`]: UTF-16 ↔ scalar transcoding (surrogate pairs) for UEFI/CHAR16 interop;
//! - [`vfs`]: an in-memory virtual filesystem (paths, directories, files);
//! - [`virtio`]: virtio status/feature negotiation and the virtio-net config/header;
//! - [`virtqueue`]: the virtio split-virtqueue ring transport (descriptors, avail/used);
//! - [`vtd`]: Intel VT-d register, table and descriptor encodings and a second-level
//!   page-table walker (DMA and interrupt remapping).
//!
//! For the kernel's own target it also holds the kernel proper: the boot flow ([`kernel_main`])
//! and the services (scheduler, processes, IPC, system calls, drivers), on top of the
//! framework's typed interfaces. All of it is original code written for this project, with no
//! third-party crates (CLAUDE.md §3, §5).
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod base64;
pub mod blockcache;
pub mod cache;
pub mod channel;
pub mod deflate;
pub mod dhcp;
pub mod dns;
pub mod editbuffer;
pub mod entropy;
pub mod fbconsole;
pub mod font;
pub mod glob;
pub mod gpt;
pub mod handles;
pub mod hashmap;
pub mod hpet;
pub mod initramfs;
pub mod ioapic;
pub mod json;
pub mod klog;
pub mod net;
pub mod numa;
pub mod nvme;
pub mod originfs;
pub use origin_frame::{
    acpi, amdvi, apic, apstart, bitmap, boot, clock, cpumap, descriptors, dmar, features, frames,
    heap, ivrs, pci, topology, unwind, usercopy, vtd,
};
pub use origin_paging as paging;
pub mod ps2;
pub mod raster;
pub mod report;
pub mod ring;
pub mod route;
pub mod rtc;
pub mod runqueue;
pub mod semver;
pub mod serial;
pub mod spcr;
pub mod tcp;
#[cfg(test)]
#[path = "../frame/src/testing.rs"]
mod testing;
pub mod text;
pub mod tftp;
pub mod timers;
pub mod ustar;
pub mod utf16;
pub mod vfs;
pub mod virtio;
pub mod virtqueue;

// The kernel proper, for its own target only: the boot flow and the services, on the
// framework's typed interfaces.
#[cfg(target_os = "none")]
mod block;
#[cfg(target_os = "none")]
mod console;
#[cfg(target_os = "none")]
mod dma;
#[cfg(target_os = "none")]
mod i8042;
#[cfg(target_os = "none")]
mod ipc;
#[cfg(target_os = "none")]
mod ipc_proof;
#[cfg(target_os = "none")]
mod irq;
#[cfg(target_os = "none")]
mod kernel;
#[cfg(target_os = "none")]
mod pcie;
#[cfg(target_os = "none")]
mod process;
#[cfg(target_os = "none")]
mod sched;
#[cfg(target_os = "none")]
mod storage;
#[cfg(target_os = "none")]
mod syscall;
#[cfg(target_os = "none")]
mod time;
#[cfg(target_os = "none")]
mod tlb;
#[cfg(target_os = "none")]
mod user;
#[cfg(target_os = "none")]
mod virtionet;
#[cfg(target_os = "none")]
mod wait;

#[cfg(target_os = "none")]
pub use kernel::kernel_main;
#[cfg(target_os = "none")]
pub(crate) use kernel::{COM1_BASE, INITRAMFS_IMAGE, check_initramfs};
#[cfg(target_os = "none")]
pub(crate) use origin_frame::{cpu, firmware, iommu, memory, phys, smp, timer, vm};
