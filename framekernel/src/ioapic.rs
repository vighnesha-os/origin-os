//! I/O APIC redirection-table encoding (CLAUDE.md §24 interrupt routing, §31 interrupt
//! management, §32 interrupt remapping). The register-index arithmetic and the
//! redirection-entry bit layouts; the kernel (`src/irq.rs` in the binary) does the indirect
//! MMIO.
//!
//! Reference: the 82093AA I/O APIC datasheet and the ACPI MADT. Registers are reached
//! indirectly: write the register index to IOREGSEL (offset 0x00), read or write the data
//! at IOWIN (offset 0x10). Each redirection entry is 64 bits across two 32-bit registers
//! at indexes `0x10 + 2n` (low) and `0x11 + 2n` (high) for global system interrupt `n`
//! relative to the I/O APIC's GSI base.
//!
//! An entry has two formats. **Compatibility format** ([`redirection_entry`]) names the
//! destination APIC and the vector itself. **Remappable format** ([`RemappableEntry`], Intel
//! VT-d Architecture Specification Rev 5.20, chapter 5, programming the I/OxAPIC) names an
//! interrupt remapping table entry instead, which holds the vector, the destination and the
//! source-ID check. Once a VT-d unit remaps interrupts, only remappable-format entries are
//! allowed through: with compatibility-format interrupts blocked (GSTS.CFIS clear), a
//! compatibility-format entry's interrupts never arrive.

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

/// Low-dword bits of a redirection entry: the polarity (13), trigger mode (15) and mask (16);
/// and the read-only status bits, delivery status (12) and remote IRR (14), which the I/O
/// APIC sets and a write cannot change.
const POLARITY: u32 = 1 << 13;
const TRIGGER: u32 = 1 << 15;
const MASK: u32 = 1 << 16;
/// Remappable format, low dword: bit 11 holds the index's bit 15, bits 10:8 must be 000, and
/// bits 31:17 are reserved.
const INDEX_HIGH_BIT: u32 = 1 << 11;
const DELIVERY_BITS: u32 = 0b111 << 8;
const LOW_RESERVED: u32 = 0xFFFE_0000;
/// Remappable format, high dword: bits 31:17 (entry bits 63:49) hold the index's bits 14:0,
/// bit 16 (entry bit 48) is the format bit, and bits 15:0 (entry bits 47:32) are reserved.
const FORMAT_REMAPPABLE: u32 = 1 << 16;
const HIGH_RESERVED: u32 = 0xFFFF;

/// Whether the entry whose high dword is `high` is in remappable format (entry bit 48).
pub const fn is_remappable(high: u32) -> bool {
    high & FORMAT_REMAPPABLE != 0
}

/// A redirection entry in remappable format (VT-d Rev 5.20 chapter 5, programming the
/// I/OxAPIC). It carries a handle — the index of an interrupt remapping table entry (IRTE) —
/// instead of a destination. The IRTE holds what is delivered (vector, destination, delivery
/// mode) and checks the I/O APIC's source ID. The pin's polarity, trigger mode and mask stay
/// here, because they govern how the I/O APIC samples its pin.
///
/// Layout: bits 63:49 the index's bits 14:0; 48 the format bit (1); 47:17 reserved (0); 16
/// mask; 15 trigger mode; 14 remote IRR (read-only); 13 polarity; 12 delivery status
/// (read-only); 11 the index's bit 15; 10:8 000; 7:0 the vector. Bits 10:8 are 000 (fixed)
/// so the message's SHV bit stays clear and the handle alone selects the IRTE. The I/O APIC
/// itself needs no knowledge of remapping: it turns entry bits 63:48 into message address
/// bits 19:4 and bit 11 into address bit 2, exactly where the remappable message format keeps
/// the handle and the format bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemappableEntry {
    /// The IRTE index: the message's handle.
    pub index: u16,
    /// The vector. The IRTE's vector is the one delivered; this one is what the I/O APIC
    /// matches an EOI broadcast against, so for a level-triggered pin it must equal the
    /// IRTE's ([`RemappableEntry::agrees_with_irte`]).
    pub vector: u8,
    /// The pin is active-low (else active-high).
    pub active_low: bool,
    /// The pin is level-triggered (else edge-triggered). It must equal the IRTE's trigger
    /// mode.
    pub level: bool,
    pub masked: bool,
}

impl RemappableEntry {
    /// The entry's `(low, high)` register values. `None` for a vector below 16: the
    /// processor reserves vectors 0–15, no IRTE delivers one, and a level-triggered pin with
    /// such a vector could never be acknowledged.
    pub fn encode(&self) -> Option<(u32, u32)> {
        if self.vector < 16 {
            return None;
        }
        let mut low = u32::from(self.vector);
        if self.index & 0x8000 != 0 {
            low |= INDEX_HIGH_BIT;
        }
        if self.active_low {
            low |= POLARITY;
        }
        if self.level {
            low |= TRIGGER;
        }
        if self.masked {
            low |= MASK;
        }
        let high = u32::from(self.index & 0x7FFF) << 17 | FORMAT_REMAPPABLE;
        Some((low, high))
    }

    /// The entry read back from its `(low, high)` register values. `None` if it is not in
    /// remappable format, its bits 10:8 are not 000, a reserved bit is set, or its vector is
    /// below 16. The read-only status bits (delivery status, remote IRR) are ignored.
    pub fn decode(low: u32, high: u32) -> Option<Self> {
        if !is_remappable(high)
            || high & HIGH_RESERVED != 0
            || low & (LOW_RESERVED | DELIVERY_BITS) != 0
        {
            return None;
        }
        let vector = (low & 0xFF) as u8;
        if vector < 16 {
            return None;
        }
        let index = (high >> 17) as u16 | u16::from(low & INDEX_HIGH_BIT != 0) << 15;
        Some(Self {
            index,
            vector,
            active_low: low & POLARITY != 0,
            level: low & TRIGGER != 0,
            masked: low & MASK != 0,
        })
    }

    /// Whether this entry agrees with the IRTE it names, which delivers `irte_vector` with
    /// trigger mode `irte_level` (VT-d chapter 5): the trigger modes must be equal, and a
    /// level-triggered entry's vector must equal the IRTE's, so the EOI that the processor
    /// broadcasts for the delivered vector clears this pin's remote IRR.
    pub fn agrees_with_irte(&self, irte_vector: u8, irte_level: bool) -> bool {
        self.level == irte_level && (!self.level || self.vector == irte_vector)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vtd::{self, MsiMessage};

    /// The interrupt message an I/O APIC sends for an entry, as the hardware forms it: the
    /// address is `FEEh` in bits 31:20, entry bits 63:48 in bits 19:4, entry bit 11 in bit 2,
    /// and bit 3 set only for lowest-priority delivery; the data carries the vector, the
    /// delivery mode and the trigger mode. With a compatibility-format entry this is the
    /// destination, the destination mode and the redirection hint; with a remappable-format
    /// entry the same wiring yields the handle, the format bit and a clear SHV.
    fn message(low: u32, high: u32) -> MsiMessage {
        let delivery = (low >> 8) & 0b111;
        let hint = if delivery == delivery::LOWEST_PRIORITY {
            1 << 3
        } else {
            0
        };
        MsiMessage {
            address: 0xFEE0_0000
                | u64::from(high >> 16) << 4
                | u64::from((low >> 11) & 1) << 2
                | hint,
            data: (low & 0xFF) | delivery << 8 | (low & TRIGGER),
        }
    }

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

    /// The PIT's pin as QEMU's q35 routes it (GSI 2, active-high, edge), to vector 0x41.
    const PIT: RemappableEntry = RemappableEntry {
        index: 0,
        vector: 0x41,
        active_low: false,
        level: false,
        masked: false,
    };

    #[test]
    fn remappable_entry_layout() {
        // Index 0: the format bit alone in the high dword, the vector alone in the low.
        assert_eq!(PIT.encode(), Some((0x41, 0x0001_0000)));
        // Index bits 14:0 sit in entry bits 63:49 (high dword 31:17).
        let e = RemappableEntry { index: 5, ..PIT };
        assert_eq!(e.encode(), Some((0x41, 5 << 17 | 1 << 16)));
        assert_eq!(e.encode(), Some((0x41, 0x000B_0000)));
        // Index bit 15 sits in entry bit 11, where compatibility format keeps the
        // destination mode.
        let e = RemappableEntry {
            index: 0x8000,
            ..PIT
        };
        assert_eq!(e.encode(), Some((0x41 | 1 << 11, 0x0001_0000)));
        let e = RemappableEntry {
            index: 0xFFFF,
            ..PIT
        };
        assert_eq!(e.encode(), Some((0x841, 0xFFFF_0000)));
        // Polarity, trigger and mask stay where compatibility format has them; bits 10:8
        // stay 000 (never lowest priority, so the message's SHV stays clear).
        let e = RemappableEntry {
            index: 0x1234,
            vector: 0xA0,
            active_low: true,
            level: true,
            masked: true,
        };
        let (low, high) = e.encode().unwrap();
        assert_eq!(low, 0xA0 | 1 << 13 | 1 << 15 | 1 << 16);
        assert_eq!(high, 0x1234 << 17 | 1 << 16);
        assert_eq!(low & 0x700, 0, "delivery-mode bits 000");
        assert!(is_masked(low) && is_remappable(high));
        // The polarity, trigger and mask bits are the compatibility-format ones.
        let (compat_low, compat_high) = redirection_entry(0xA0, 0, true, true, true);
        assert_eq!(low & 0x1_E000, compat_low & 0x1_E000);
        assert!(!is_remappable(compat_high));
        // Vectors 0-15 are refused.
        for vector in [0, 2, 15] {
            assert_eq!(RemappableEntry { vector, ..PIT }.encode(), None, "{vector}");
        }
        assert!(RemappableEntry { vector: 16, ..PIT }.encode().is_some());
    }

    #[test]
    fn remappable_entries_round_trip() {
        let mut rng = crate::testing::Rng::new(0x10A9);
        for _ in 0..crate::testing::cases(20_000) {
            let r = rng.next();
            let e = RemappableEntry {
                index: r as u16,
                vector: 16 + ((r >> 16) as u8) % 240,
                active_low: r & 1 << 24 != 0,
                level: r & 1 << 25 != 0,
                masked: r & 1 << 26 != 0,
            };
            let (low, high) = e.encode().unwrap();
            assert_eq!(RemappableEntry::decode(low, high), Some(e));
            // The read-only status bits (delivery status, remote IRR) do not change it.
            let busy = low | 1 << 12 | 1 << 14;
            assert_eq!(RemappableEntry::decode(busy, high), Some(e));
        }
    }

    #[test]
    fn remappable_entry_refusals() {
        let (low, high) = RemappableEntry { index: 7, ..PIT }.encode().unwrap();
        assert!(RemappableEntry::decode(low, high).is_some());
        // A compatibility-format entry (format bit clear) is not one.
        let (compat_low, compat_high) = redirection_entry(0x41, 3, false, false, false);
        assert_eq!(RemappableEntry::decode(compat_low, compat_high), None);
        assert_eq!(RemappableEntry::decode(low, high & !(1 << 16)), None);
        // Bits 10:8 must be 000: any delivery mode there is refused.
        for mode in 1..8 {
            assert_eq!(
                RemappableEntry::decode(low | mode << 8, high),
                None,
                "{mode}"
            );
        }
        // Reserved bits 47:17: low dword 31:17 and high dword 15:0.
        for bit in 17..32 {
            assert_eq!(
                RemappableEntry::decode(low | 1 << bit, high),
                None,
                "low {bit}"
            );
        }
        for bit in 0..16 {
            assert_eq!(
                RemappableEntry::decode(low, high | 1 << bit),
                None,
                "high {bit}"
            );
        }
        // A vector below 16.
        assert_eq!(RemappableEntry::decode((low & !0xFF) | 0x0F, high), None);
        // Any register contents decode without panicking, and whatever decodes re-encodes to
        // the same bits (the status bits aside).
        let mut rng = crate::testing::Rng::new(0x10AB);
        for _ in 0..crate::testing::cases(20_000) {
            let (low, high) = (rng.next() as u32, rng.next() as u32);
            let low = if rng.below(2) == 0 {
                low & 0x1_FFFF
            } else {
                low
            };
            let high = if rng.below(2) == 0 {
                (high & !0xFFFF) | 1 << 16
            } else {
                high
            };
            if let Some(e) = RemappableEntry::decode(low, high) {
                assert_eq!(e.encode(), Some((low & !(1 << 12 | 1 << 14), high)));
            }
        }
    }

    #[test]
    fn remappable_entry_message_selects_its_irte() {
        // The I/O APIC's message for a remappable entry is a remappable-format interrupt
        // request whose handle is the index, with SHV clear (so the data is not a
        // sub-handle).
        for index in [0u16, 1, 2, 0x41, 0x7FFF, 0x8000, 0x8001, 0xFFFF] {
            for level in [false, true] {
                let e = RemappableEntry {
                    index,
                    level,
                    ..PIT
                };
                let (low, high) = e.encode().unwrap();
                let m = message(low, high);
                assert_eq!(m.address & (1 << 4), 1 << 4, "remappable format");
                assert_eq!(m.address & (1 << 3), 0, "SHV clear");
                assert_eq!(vtd::interrupt_index(m), Some(index));
                assert_eq!(m.data & 0xFF, 0x41);
                assert_eq!(m.data & (1 << 15) != 0, level);
            }
        }
        // A compatibility-format entry's message selects no IRTE: under interrupt remapping
        // with compatibility format blocked, it never arrives.
        let (low, high) = redirection_entry(0x41, 0, false, false, false);
        assert_eq!(vtd::interrupt_index(message(low, high)), None);
        // A lowest-priority entry would set SHV, so the data's low half would be added to the
        // handle: the reason remappable entries keep bits 10:8 at 000.
        let (low, high) = PIT.encode().unwrap();
        let lowest = message(low | delivery::LOWEST_PRIORITY << 8, high);
        assert_eq!(lowest.address & (1 << 3), 1 << 3);
        assert_eq!(vtd::interrupt_index(lowest), Some(0x141));
    }

    #[test]
    fn remappable_entries_agree_with_their_irte() {
        // Edge: the trigger modes must match; the vector need not (the IRTE's is delivered,
        // and an edge pin has no remote IRR to clear).
        assert!(PIT.agrees_with_irte(0x41, false));
        assert!(PIT.agrees_with_irte(0x50, false));
        assert!(!PIT.agrees_with_irte(0x41, true));
        // Level: the trigger modes and the vectors must both match.
        let level = RemappableEntry {
            level: true,
            active_low: true,
            ..PIT
        };
        assert!(level.agrees_with_irte(0x41, true));
        assert!(!level.agrees_with_irte(0x50, true));
        assert!(!level.agrees_with_irte(0x41, false));
    }
}
