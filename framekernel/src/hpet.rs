//! The High Precision Event Timer (CLAUDE.md §68: high-resolution timers, timer
//! calibration). This module parses the ACPI HPET table and does the counter arithmetic;
//! the kernel (`src/time.rs`) maps the registers and reads the counter.
//!
//! References: the IA-PC HPET Specification 1.0a (the General Capabilities register's
//! `COUNTER_CLK_PERIOD` in femtoseconds, the main counter, `ENABLE_CNF`); ACPI 6.6 §5.2.9.4
//! (the HPET description table: the base address is the 64-bit field of the GAS at offset
//! 44).

/// Register offsets from the HPET's MMIO base.
pub mod register {
    /// General Capabilities and ID (64-bit, read-only).
    pub const CAPABILITIES: u64 = 0x000;
    /// General Configuration (64-bit).
    pub const CONFIG: u64 = 0x010;
    /// Main Counter Value (64-bit).
    pub const COUNTER: u64 = 0x0F0;
}

/// General Configuration bit 0: enable the main counter.
pub const ENABLE_CNF: u64 = 1 << 0;

/// Why the HPET table was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HpetError {
    /// The table is shorter than the fields the kernel reads.
    Length(usize),
    /// The base address's GAS is not system memory (address-space id 0), or the address is
    /// zero or not page-aligned.
    BadAddress,
}

/// What the kernel needs from the HPET table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hpet {
    /// MMIO base of the HPET registers.
    pub base: u64,
    /// The HPET block's hardware revision and vendor, from the event-timer-block id.
    pub block_id: u32,
}

/// The HPET table ends after this many bytes of fields the kernel reads (through the base
/// address at offset 44).
const MIN_LEN: usize = 52;

/// Parses a checked HPET table (`acpi::check_table(b"HPET")` already passed).
pub fn parse(table: &[u8]) -> Result<Hpet, HpetError> {
    if table.len() < MIN_LEN {
        return Err(HpetError::Length(table.len()));
    }
    let u32_at = |at: usize| {
        let mut v = [0u8; 4];
        v.copy_from_slice(&table[at..at + 4]);
        u32::from_le_bytes(v)
    };
    let u64_at = |at: usize| {
        let mut v = [0u8; 8];
        v.copy_from_slice(&table[at..at + 8]);
        u64::from_le_bytes(v)
    };
    // The GAS at offset 40: address_space_id (byte 40), then base at byte 44.
    let address_space = table[40];
    let base = u64_at(44);
    if address_space != 0 || base == 0 || base & 0xFFF != 0 {
        return Err(HpetError::BadAddress);
    }
    Ok(Hpet {
        base,
        block_id: u32_at(36),
    })
}

/// The counter period in femtoseconds, from the high 32 bits of the capabilities register.
pub const fn period_fs(capabilities: u64) -> u32 {
    (capabilities >> 32) as u32
}

/// The counter's frequency in Hz from its period (`10^15 / period_fs`), or `None` for a
/// zero or absurd period (the spec caps it at 100 ns = 10^8 fs).
pub fn frequency_hz(capabilities: u64) -> Option<u64> {
    let period = u64::from(period_fs(capabilities));
    if period == 0 || period > 100_000_000 {
        return None;
    }
    Some(1_000_000_000_000_000 / period)
}

/// Nanoseconds for a main-counter delta of `ticks` at `period_fs` per tick
/// (`ticks * period_fs / 10^6`), saturating.
pub fn ticks_to_nanos(ticks: u64, period_fs: u32) -> u64 {
    let fs = u128::from(ticks) * u128::from(period_fs);
    u64::try_from(fs / 1_000_000).unwrap_or(u64::MAX)
}

/// Whether two rate estimates agree within `parts_per_thousand`.
pub fn agree(a: u64, b: u64, parts_per_thousand: u64) -> bool {
    let (lo, hi) = (a.min(b), a.max(b));
    u128::from(hi - lo) * 1000 <= u128::from(lo) * u128::from(parts_per_thousand)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A QEMU-like HPET table: base 0xFED00000, a 100 MHz counter.
    fn table(base: u64, asid: u8) -> Vec<u8> {
        let mut t = alloc::vec![0u8; 56];
        t[0..4].copy_from_slice(b"HPET");
        t[36..40].copy_from_slice(&0x8086_A201u32.to_le_bytes()); // block id
        t[40] = asid;
        t[44..52].copy_from_slice(&base.to_le_bytes());
        t
    }

    #[test]
    fn parses_a_valid_table() {
        let t = table(0xFED0_0000, 0);
        assert_eq!(
            parse(&t),
            Ok(Hpet {
                base: 0xFED0_0000,
                block_id: 0x8086_A201
            })
        );
    }

    #[test]
    fn rejects_bad_addresses_and_short_tables() {
        assert_eq!(
            parse(&table(0xFED0_0000, 1)),
            Err(HpetError::BadAddress),
            "not system memory"
        );
        assert_eq!(parse(&table(0, 0)), Err(HpetError::BadAddress), "null base");
        assert_eq!(
            parse(&table(0xFED0_0800, 0)),
            Err(HpetError::BadAddress),
            "not page aligned"
        );
        let short = alloc::vec![0u8; 44];
        assert_eq!(parse(&short), Err(HpetError::Length(44)));
    }

    #[test]
    fn counter_arithmetic() {
        // A 100 MHz HPET: period 10,000,000 fs; 100 MHz.
        let cap = (10_000_000u64 << 32) | 0x8086_A201;
        assert_eq!(period_fs(cap), 10_000_000);
        assert_eq!(frequency_hz(cap), Some(100_000_000));
        // 100 million ticks at 100 MHz is one second.
        assert_eq!(ticks_to_nanos(100_000_000, 10_000_000), 1_000_000_000);
        assert_eq!(ticks_to_nanos(1, 10_000_000), 10, "one tick is 10 ns");
        // A 14.318 MHz HPET (period 69,841,279 fs) is about 14.318 MHz.
        let slow = (69_841_279u64 << 32) | 1;
        assert_eq!(frequency_hz(slow), Some(14_318_179));
        // Guard rails.
        assert_eq!(frequency_hz(0), None);
        assert_eq!(frequency_hz(200_000_000u64 << 32), None, "period too long");
        assert_eq!(ticks_to_nanos(u64::MAX, u32::MAX), u64::MAX, "saturates");
    }

    #[test]
    fn rate_agreement() {
        assert!(agree(100_000_000, 100_050_000, 1));
        assert!(!agree(100_000_000, 100_200_000, 1));
        assert!(agree(1, 1, 0));
    }
}
