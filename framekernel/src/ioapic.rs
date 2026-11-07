//! I/O APIC redirection-table encoding (CLAUDE.md §24 interrupt routing, §31 interrupt
//! management). The register-index arithmetic and the redirection-entry bit layout; the
//! kernel (`src/ioapic.rs` in the binary) does the indirect MMIO.
//!
//! Reference: the 82093AA I/O APIC datasheet and the ACPI MADT. Registers are reached
//! indirectly: write the register index to IOREGSEL (offset 0x00), read or write the data
//! at IOWIN (offset 0x10). Each redirection entry is 64 bits across two 32-bit registers
//! at indexes `0x10 + 2n` (low) and `0x11 + 2n` (high) for global system interrupt `n`
//! relative to the I/O APIC's GSI base.

/// IOREGSEL and IOWIN MMIO offsets from the I/O APIC base.
pub const IOREGSEL: u64 = 0x00;
pub const IOWIN: u64 = 0x10;

/// The IOAPICVER register index; its bits 16–23 are the maximum redirection entry.
pub const REG_VERSION: u32 = 0x01;

/// The low and high register indexes for redirection entry `n`.
pub const fn redirection_low(n: u32) -> u32 {
    0x10 + 2 * n
}
pub const fn redirection_high(n: u32) -> u32 {
    0x11 + 2 * n
}

/// The maximum redirection entry (so the count is this plus one) from the version register.
pub const fn max_redirection_entry(version: u32) -> u32 {
    (version >> 16) & 0xFF
}

/// Delivery modes (redirection entry bits 8–10).
pub mod delivery {
    pub const FIXED: u32 = 0b000;
    pub const LOWEST_PRIORITY: u32 = 0b001;
    pub const NMI: u32 = 0b100;
}

/// Decodes an ACPI MADT interrupt-source-override flags word (MPS INTI flags): bits 1:0 are
/// polarity (01 active-high, 11 active-low, 00 bus default), bits 3:2 trigger (01 edge, 11
/// level, 00 bus default). The bus default for ISA is active-high, edge-triggered.
pub fn decode_flags(flags: u16) -> (bool, bool) {
    let active_low = matches!(flags & 0b11, 0b11);
    let level = matches!((flags >> 2) & 0b11, 0b11);
    (active_low, level)
}

/// Builds a redirection entry's `(low, high)` 32-bit register values: `vector` with fixed
/// delivery to the local APIC `apic_id` in physical destination mode, with the given
/// polarity, trigger and mask.
pub fn redirection_entry(
    vector: u8,
    apic_id: u8,
    active_low: bool,
    level: bool,
    masked: bool,
) -> (u32, u32) {
    let mut low = u32::from(vector) | (delivery::FIXED << 8);
    // bit 11 destination mode 0 = physical; bit 13 polarity; bit 15 trigger; bit 16 mask.
    if active_low {
        low |= 1 << 13;
    }
    if level {
        low |= 1 << 15;
    }
    if masked {
        low |= 1 << 16;
    }
    // The destination APIC ID is bits 56–63 of the 64-bit entry: bits 24–31 of the high
    // dword.
    let high = u32::from(apic_id) << 24;
    (low, high)
}

/// Whether a redirection entry's low dword has the mask bit set.
pub const fn is_masked(low: u32) -> bool {
    low & (1 << 16) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_indexes() {
        assert_eq!((redirection_low(0), redirection_high(0)), (0x10, 0x11));
        assert_eq!((redirection_low(2), redirection_high(2)), (0x14, 0x15));
        assert_eq!(redirection_low(23), 0x3E);
        // A 24-entry I/O APIC reports max entry 23 in version bits 16-23.
        assert_eq!(max_redirection_entry(0x0017_0011), 23);
    }

    #[test]
    fn flag_decoding() {
        assert_eq!(
            decode_flags(0b0000),
            (false, false),
            "bus default: active-high edge"
        );
        assert_eq!(decode_flags(0b0001), (false, false), "active-high edge");
        assert_eq!(decode_flags(0b0011), (true, false), "active-low edge");
        assert_eq!(decode_flags(0b1111), (true, true), "active-low level");
        assert_eq!(decode_flags(0b1101), (false, true), "active-high level");
    }

    #[test]
    fn redirection_entry_layout() {
        // Vector 0x41 to APIC 0, active-high edge, unmasked.
        let (low, high) = redirection_entry(0x41, 0, false, false, false);
        assert_eq!(low, 0x41);
        assert_eq!(high, 0);
        assert!(!is_masked(low));
        // Masked, active-low, level, to APIC 3.
        let (low, high) = redirection_entry(0x50, 3, true, true, true);
        assert_eq!(low & 0xFF, 0x50);
        assert!(low & (1 << 13) != 0 && low & (1 << 15) != 0 && low & (1 << 16) != 0);
        assert!(is_masked(low));
        assert_eq!(high >> 24, 3);
        // NMI delivery mode is distinct in bits 8-10.
        assert_eq!((delivery::NMI << 8) >> 8, 0b100);
    }
}
