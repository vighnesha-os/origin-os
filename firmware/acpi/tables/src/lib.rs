//! `origin-acpi-tables`: the system description tables of ACPI 6.6 §5.2 that a platform in
//! Origin's declared scope (x86-64) can present, parsed and checked as untrusted input
//! (ADR-0019 A1 and A6, area A; TASKS item 62).
//!
//! Parser contract, as `onbf-format`'s: `no_std`, no allocation, no `unsafe`, no panics on any
//! input, typed errors that name the table and the byte offset at fault. The lints below make
//! indexing, unchecked arithmetic and truncating casts compile errors outside tests, and
//! `cargo xtask check` builds this crate's release object for the kernel's target and refuses any
//! reference to a panic path in it.
//!
//! # The tables
//!
//! | Module | Tables (ACPI 6.6 section) |
//! |---|---|
//! | [`root`] | RSDT (§5.2.7), XSDT (§5.2.8) |
//! | [`fadt`] | FADT `FACP` (§5.2.9) |
//! | [`facs`] | FACS (§5.2.10), which has no header and no checksum |
//! | [`definition`] | DSDT, SSDT, PSDT (§5.2.11): headers checked, AML handed on unread |
//! | [`madt`] | MADT `APIC` (§5.2.12), global system interrupts (§5.2.13) |
//! | [`platform`] | SBST (§5.2.14), ECDT (§5.2.15), SLIT (§5.2.17), CPEP (§5.2.18), MSCT (§5.2.19) |
//! | [`srat`] | SRAT (§5.2.16) |
//! | [`ras`] | RASF (§5.2.20), RAS2 (§5.2.21), and their channels' shared memory as decoders |
//! | [`memory`] | MPST (§5.2.22) and its channel as a decoder, PMTT (§5.2.22.12) |
//! | [`boot`] | BGRT (§5.2.23), FPDT (§5.2.24) and the FBPT, S3PT and MBPT it points to |
//! | [`nfit`] | NFIT (§5.2.26) |
//! | [`nhlt`] | NHLT (§5.2.27) |
//! | [`sdev`] | SDEV (§5.2.28) |
//! | [`hmat`] | HMAT (§5.2.29) |
//! | [`pptt`] | PPTT (§5.2.31) |
//! | [`viot`] | VIOT (§5.2.33) |
//! | [`records`] | PDTT (§5.2.30), PHAT (§5.2.32), MISC (§5.2.34), CCEL (§5.2.35), SVKL (§5.2.36) |
//!
//! [`census::check`] takes any table that has a header and accounts for it: a table above is
//! parsed whole, and any other has its header, length and checksum checked and is named as ACPI
//! 6.6's chapter 14 or 18 (ADR-0019 areas H and G), another specification's (§5.2.6, Table 5.6),
//! a table for other processors, an OEM's, or unknown.
//!
//! # Out of scope (ADR-0019 A1)
//!
//! The content for Arm, RISC-V and LoongArch: the GTDT (§5.2.25), the RHCT (§5.2.37 to §5.2.39),
//! the MADT's GIC, LoongArch and RISC-V structures (types 11 to 15 and 17 to 27), the SRAT's GICC,
//! ITS and RINTC affinities (types 3, 4 and 7), and the meaning of the FADT's `ARM_BOOT_ARCH`.
//! Itanium's: the MADT's I/O SAPIC, local SAPIC and platform interrupt sources (types 6 to 8).
//! Each such structure is still walked by its length and returned as such, so a malformed one
//! cannot hide what follows it; none is decoded.
//!
//! # Strictness (§5.2.1, and the coordinating session's ruling of 2026-11-14)
//!
//! Reserved bits are ignored, as §5.2.1.1 tells the OS to. A reserved value in an enumerated
//! field comes back typed (a `Reserved(n)` arm) and is refused only where acting on it would be
//! wrong. A structure type a table reserves is walked by its length and returned. A table or
//! structure longer than the layout known here is accepted and its tail ignored (§5.2.2); one
//! shorter than its revision's fields is refused. Every rule on a value, table by table: it
//! refuses the table unless marked **reported**, when it marks only that structure or field
//! unusable, and the census counts it ([`census::Summary::reported`]):
//!
//! - Generic address structures the OS accesses: a reserved address space; a reserved access
//!   size in system memory, system I/O or PCI configuration space (elsewhere the space defines
//!   the field: the review's F11).
//! - FADT, refused: no DSDT. Each of the rest **reported**, and that field not used, while the
//!   rest of the table is (the review's F3; `fadt::Field`): no FACS on a platform that is not
//!   hardware-reduced; a FACS not 64-byte aligned; a reset register (when its flag says it is
//!   used) or sleep register that is not 8 bits at bit 0 in system I/O, system memory or bus 0's
//!   configuration space; on a platform with fixed hardware, no PM1a event or control block,
//!   `PM1_EVT_LEN` below 4, `PM1_CNT_LEN` below 2, a PM2 block of length 0, a PM timer not 4
//!   bytes long, an odd GPE block length, a GPE block outside system I/O and memory (its width,
//!   offset and access size are not judged: §5.2.9 tells the OS to ignore them), a month alarm
//!   without a day alarm.
//! - MADT, each **reported**, and the structure not used, while the walk goes on (the review's
//!   F5): a structure too short for its type; an interrupt override for a bus other than ISA; the
//!   reserved polarity or trigger encoding (binary 10); a local APIC input other than LINT0 and
//!   LINT1; a wakeup mailbox not 4 KiB-aligned; a second address override; an I/O APIC at an
//!   address or interrupt base an earlier one has; I/O APICs past the 256th. Refused: only a
//!   length that cannot locate the next structure.
//! - SRAT, each **reported**, and the structure not used, as the MADT's: a structure too short for
//!   its type; an enabled memory range that wraps past the top of the address space. A disabled
//!   memory affinity is accepted whatever its range (§5.2.16.2; the review's F4).
//! - ECDT: a register outside system I/O and memory; a namespace path that is empty, not
//!   NUL-terminated or holds a character no path can.
//! - SLIT: no localities; a distance of 0 to 9 (reserved); a diagonal other than 10.
//! - MSCT: domain ranges out of order or overlapping, a last domain below its first, a domain up
//!   to the maximum that no range covers.
//! - RAS2 channel: a version other than 0. RASF, RAS2 and MPST channels: the wrong signature.
//! - MPST: a power state characteristics structure of a format other than 1; a power state index
//!   that names nothing in an enabled node; an enabled node's range that wraps. A disabled node's
//!   contents are ignored (Table 5.110; the review's F4).
//! - PMTT: revision 1, which the census names as a layout ACPI 6.6 no longer gives (deprecated
//!   since ACPI 6.4) instead of refusing it (the review's F10); a top-level aggregator below the
//!   top level.
//! - FPDT: no FBPT pointer; two pointers of one kind. FBPT and S3PT: their required records
//!   missing or repeated.
//! - NFIT: an interleave index of 0; the interleave index and ways pairs §5.2.26.3 forbids, in a
//!   region mapping that names an SPA range (without one, §5.2.26.3 says they are ignored); a
//!   block data window naming control region 0; the capability bit 0 without bit 1; a range that
//!   wraps.
//! - NHLT: a format that is not `WAVEFORMATEXTENSIBLE` (tag 0xFFFE, extension 22); a container
//!   not a whole number of bytes or smaller than its valid bits; a block align that is not one
//!   container per channel.
//! - SDEV: a hardware or subsystem ID that is neither a PNP nor an ACPI ID; an empty or
//!   unterminated string; secure access components with no identification component; a PCI path
//!   that is empty or not whole pairs.
//! - HMAT: an entry base unit of 0; extended-linear addressing on a cache not direct-mapped.
//! - PPTT: a thread that is not a leaf; a leaf with children; a valid cache ID of 0.
//! - VIOT: a node offset inside the fixed fields or not 8-byte aligned; a node not 8-byte aligned;
//!   a PCI range whose end is below its start. Bytes after the counted nodes are not walked as
//!   nodes; the census hands them on (the review's F12).
//! - PDTT: no trigger. MISC: no entry. CCEL: a log area that wraps. SVKL: a key that wraps.
//!
//! # Where §5.2 defers
//!
//! Named here and in the census line, and not supported until a task reads the document:
//! the CCEL's and SVKL's full definitions (Table 5.5 sends them to the confidential computing
//! event log's and Intel TDX's documents); the NFIT device handle's encoding (§9.19.3); the
//! BGRT's bitmap formats (a document on uefi.org); the PCC channels the RASF, RAS2, MPST and PDTT
//! name (chapter 14); the NHLT's configuration bytes, which are the DSP firmware's; the FPDT's
//! GUID event record and microcontroller boot record, which §5.2.24 names and does not lay out.
//! Where §5.2 contradicts itself the reading taken is in its module: the string event record's
//! length (60 printed, 68 laid out), the MPST characteristics' autonomy bits, the SVKL's
//! signature (`SVKL`, as Table 5.5 gives it, not `SKVL`), the VIOT virtio-mmio node's type (4).
//!
//! # Bounds on work
//!
//! Checks between structures are bounded so that no table, however hostile, costs unbounded
//! time: at most [`madt::MAX_IO_APICS`] I/O APICs, [`nfit::MAX_STRUCTURES`] NFIT structures
//! (counted before any check between them, so that a parse takes at most [`nfit::MAX_STEPS`]
//! walk steps whatever the table's length: the review's F1),
//! [`pptt::MAX_STRUCTURES`] PPTT structures (its chains [`pptt::MAX_DEPTH`] and
//! [`pptt::MAX_CACHE_LEVELS`] long), [`viot::MAX_NODES`] VIOT nodes, and PMTT devices nested
//! [`memory::PMTT_MAX_DEPTH`] deep. The PPTT checks each cache chain once, from the cache itself,
//! so a parse takes at most [`pptt::MAX_LOOKUPS`] lookups a structure, about 8 × 10⁷ walk steps
//! at 8,192 structures, where the review estimated 10⁹ before (its F18; TASKS item 71). The 12
//! tables the hostile soak generates (the MADT, SRAT, CPEP, NFIT, HMAT, SDEV, PPTT, FPDT, PHAT,
//! VIOT, PMTT and MSCT) each have a test that builds a large one, checks it whole and counts its
//! steps against its bound; the NFIT's copies are refused by design, and its large case is F1's
//! test (F13; TASKS item 71). The other tables this crate parses have no such test.
//!
//! # Iterators
//!
//! A table's structure iterator (`entries`, `nodes`, `records`, `walk`) gives each structure
//! whose contents break a rule as an `Err` item and goes on past it; it ends at the first error
//! that leaves the next structure unlocated (a length too short for the type and length fields,
//! or one that runs past the table). Each table's `parse`, or the census, has already judged every
//! structure (the review's F15: the narrower contract, recorded rather than fused).
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

pub mod boot;
pub mod census;
pub mod definition;
mod error;
pub mod facs;
pub mod fadt;
pub mod gas;
pub mod header;
pub mod hmat;
pub mod madt;
pub mod memory;
pub mod nfit;
pub mod nhlt;
pub mod platform;
pub mod pptt;
pub mod ras;
mod read;
pub mod records;
pub mod root;
pub mod sdev;
pub mod srat;
pub mod viot;
mod walk;

#[cfg(test)]
mod hostile;
#[cfg(test)]
mod testing;

/// The signatures of the firmware's structures that change after boot, which a reader must copy
/// and never hold by reference, wherever it finds them named (the independent review's addendum,
/// N3): the FACS, whose global lock firmware in SMM takes and releases (§5.2.10.1), and the
/// FPDT's FBPT, S3PT and MBPT, which the firmware rewrites on resume (§5.2.24.8, §5.2.24.10).
pub const CHANGES_AFTER_BOOT: [&[u8; 4]; 4] = [b"FACS", b"FBPT", b"S3PT", b"MBPT"];

pub use error::{Error, ErrorKind, Signature};
pub use read::{Guid, sums_to_zero};
pub use walk::Raw;
