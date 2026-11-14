//! The interpreter's configuration: its limits (ADR-0019 A3; the contract's §5) and the integer
//! width a DSDT sets.

/// The hard limits. Each top-level evaluation (a load, an `evaluate`) has its own count of loops,
/// time, depth and steps, including every method it calls; memory is the interpreter's whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// `While` iterations, all loops together.
    pub loop_iterations: u64,
    /// Time by the host's clock, in 100 ns units.
    pub time_100ns: u64,
    /// Method calls nested.
    pub call_depth: u32,
    /// Bytes the interpreter holds: namespace, objects, frames and table copies.
    pub memory_bytes: usize,
    /// Opcodes executed.
    pub steps: u64,
}

impl Limits {
    /// The ACPI service's defaults (the contract's §5, accepted 2026-11-14).
    pub const SERVICE: Self = Self {
        loop_iterations: 1_000_000,
        time_100ns: 30 * 10_000_000,
        call_depth: 64,
        memory_bytes: 64 << 20,
        steps: 100_000_000,
    };

    /// The kernel boot proof's (step B3).
    pub const BOOT_PROOF: Self = Self {
        loop_iterations: 100_000,
        time_100ns: 10_000_000,
        call_depth: 32,
        memory_bytes: 4 << 20,
        steps: 10_000_000,
    };
}

/// The interpreter's configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// The limits.
    pub limits: Limits,
}

/// The width of AML integers, which the DSDT's revision sets for every table (ACPI 6.6
/// §19.6.29): 32 bits below revision 2, else 64.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntegerWidth {
    /// 32-bit integers (a DSDT revision below 2; QEMU 11.1.3's DSDT is revision 1).
    Bits32,
    /// 64-bit integers.
    Bits64,
}

impl IntegerWidth {
    /// The width a DSDT of `revision` sets.
    #[must_use]
    pub const fn from_dsdt_revision(revision: u8) -> Self {
        if revision < 2 {
            Self::Bits32
        } else {
            Self::Bits64
        }
    }

    /// The mask of an integer's bits.
    #[must_use]
    pub const fn mask(self) -> u64 {
        match self {
            Self::Bits32 => 0xFFFF_FFFF,
            Self::Bits64 => u64::MAX,
        }
    }

    /// Bits in an integer.
    #[must_use]
    pub const fn bits(self) -> u32 {
        match self {
            Self::Bits32 => 32,
            Self::Bits64 => 64,
        }
    }

    /// Bytes in an integer.
    #[must_use]
    pub const fn bytes(self) -> usize {
        match self {
            Self::Bits32 => 4,
            Self::Bits64 => 8,
        }
    }
}

/// A definition block whose header the caller has checked (with `origin-acpi-tables`, TASKS 62):
/// its identity and its term list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DefinitionBlock<'a> {
    /// `DSDT`, `SSDT` or `PSDT`, or the signature of a table `LoadTable` named.
    pub signature: [u8; 4],
    /// The OEM ID.
    pub oem_id: [u8; 6],
    /// The OEM table ID.
    pub oem_table_id: [u8; 8],
    /// The term list: the table's bytes after its 36-byte header.
    pub aml: &'a [u8],
}
