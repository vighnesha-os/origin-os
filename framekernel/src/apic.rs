//! Local APIC in x2APIC mode, and the 8254 PIT used to calibrate its timer (M2:
//! interrupts; CLAUDE.md §24, §68).
//!
//! References: Intel SDM Vol. 3A chapter "Advanced Programmable Interrupt Controller
//! (APIC)": IA32_APIC_BASE and its legal mode transitions, the x2APIC MSR map, the LVT
//! timer register, the divide configuration register and the spurious-interrupt vector
//! register; Intel 82C54 CHMOS Programmable Interval Timer data sheet: control word, mode 0
//! and the 1.193182 MHz input clock of the PC platform.
//!
//! Origin targets 2026-era Intel and AMD platforms, which all implement x2APIC, so only
//! x2APIC mode (MSR access, no MMIO mapping) is supported. Everything here is arithmetic
//! and register encodings, tested on the host; the binary does the MSR and port I/O.

/// IA32_APIC_BASE MSR.
pub const IA32_APIC_BASE: u32 = 0x1B;
/// IA32_APIC_BASE bit 11: the local APIC is enabled.
pub const APIC_GLOBAL_ENABLE: u64 = 1 << 11;
/// IA32_APIC_BASE bit 10: x2APIC mode.
pub const APIC_X2APIC: u64 = 1 << 10;

/// x2APIC ID register.
pub const X2APIC_ID: u32 = 0x802;
/// x2APIC version register.
pub const X2APIC_VERSION: u32 = 0x803;
/// x2APIC end-of-interrupt register (write 0).
pub const X2APIC_EOI: u32 = 0x80B;
/// x2APIC spurious-interrupt vector register.
pub const X2APIC_SVR: u32 = 0x80F;
/// x2APIC LVT timer register.
pub const X2APIC_LVT_TIMER: u32 = 0x832;
/// x2APIC timer initial-count register.
pub const X2APIC_TIMER_INITIAL: u32 = 0x838;
/// x2APIC timer current-count register.
pub const X2APIC_TIMER_CURRENT: u32 = 0x839;
/// x2APIC timer divide-configuration register.
pub const X2APIC_TIMER_DIVIDE: u32 = 0x83E;

/// LVT mask bit.
pub const LVT_MASKED: u64 = 1 << 16;

/// IDT vector of the local APIC timer. Vectors 0–31 are exceptions.
pub const TIMER_VECTOR: u8 = 0x20;
/// IDT vector the local APIC uses for spurious interrupts. The low four bits are writable
/// on every x2APIC, so 0xFF is always valid.
pub const SPURIOUS_VECTOR: u8 = 0xFF;

/// The IA32_APIC_BASE values to write, in order, to reach x2APIC mode from `current`.
///
/// The architecture allows disabled → xAPIC → x2APIC, but not disabled → x2APIC in one
/// write, and the reserved state (x2APIC bit without the enable bit) is never written.
/// Returns the writes and how many of them are used (0 if already in x2APIC mode).
pub const fn x2apic_transition(current: u64) -> ([u64; 2], usize) {
    let mode = current & (APIC_GLOBAL_ENABLE | APIC_X2APIC);
    let enabled = current | APIC_GLOBAL_ENABLE;
    let x2 = enabled | APIC_X2APIC;
    if mode == APIC_GLOBAL_ENABLE | APIC_X2APIC {
        ([0, 0], 0)
    } else if mode == APIC_GLOBAL_ENABLE {
        ([x2, 0], 1)
    } else {
        // Disabled, or the invalid "x2APIC without enable" state: clear x2APIC, enable
        // xAPIC, then switch.
        ([enabled & !APIC_X2APIC, x2], 2)
    }
}

/// Spurious-interrupt vector register: APIC software-enabled, with `vector` for spurious
/// interrupts.
pub const fn svr(vector: u8) -> u64 {
    (1 << 8) | vector as u64
}

/// LVT timer modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimerMode {
    /// Counts down once from the initial count.
    OneShot,
    /// Reloads the initial count and counts down again, interrupting each time.
    Periodic,
}

/// LVT timer register value.
pub const fn lvt_timer(vector: u8, mode: TimerMode, masked: bool) -> u64 {
    let mode_bits = match mode {
        TimerMode::OneShot => 0,
        TimerMode::Periodic => 1 << 17,
    };
    let mask = if masked { LVT_MASKED } else { 0 };
    vector as u64 | mode_bits | mask
}

/// Divide-configuration register value for `divisor` (1, 2, 4, …, 128), or `None`.
///
/// The encoding is bits 3, 1 and 0: 0b000 divides by 2 up to 0b110 by 128, and 0b111 by 1.
pub const fn divide_config(divisor: u32) -> Option<u64> {
    let code = match divisor {
        1 => 0b111,
        2 => 0b000,
        4 => 0b001,
        8 => 0b010,
        16 => 0b011,
        32 => 0b100,
        64 => 0b101,
        128 => 0b110,
        _ => return None,
    };
    Some(((code & 0b100) << 1) | (code & 0b011))
}

/// Input clock of the PC platform's 8254 PIT, in Hz.
pub const PIT_HZ: u64 = 1_193_182;
/// PIT channel 2 data port.
pub const PIT_CHANNEL2: u16 = 0x42;
/// PIT mode/command port.
pub const PIT_COMMAND: u16 = 0x43;
/// Port 0x61: bit 0 gates PIT channel 2, bit 1 enables the speaker, bit 5 reads OUT2.
pub const PIT_GATE_PORT: u16 = 0x61;
/// PIT control word: channel 2, low then high byte, mode 0 (interrupt on terminal count),
/// binary.
pub const PIT_CHANNEL2_MODE0: u8 = 0b1011_0000;

/// PIT count for a one-shot of `micros` microseconds, or `None` if it does not fit the
/// 16-bit counter (at most about 54.9 ms) or is zero.
pub const fn pit_count(micros: u64) -> Option<u16> {
    let count = PIT_HZ * micros / 1_000_000;
    if count == 0 || count > u16::MAX as u64 {
        None
    } else {
        Some(count as u16)
    }
}

/// The real length, in nanoseconds, of a PIT one-shot of `count` input clocks.
pub const fn pit_nanos(count: u16) -> u64 {
    count as u64 * 1_000_000_000 / PIT_HZ
}

/// Ticks per second of a counter that advanced `ticks` during `nanos` nanoseconds, or
/// `None` if the window is empty or the result overflows.
pub const fn rate(ticks: u64, nanos: u64) -> Option<u64> {
    if nanos == 0 {
        return None;
    }
    match (ticks as u128).checked_mul(1_000_000_000) {
        Some(scaled) => {
            let r = scaled / nanos as u128;
            if r > u64::MAX as u128 {
                None
            } else {
                Some(r as u64)
            }
        }
        None => None,
    }
}

/// Timer initial count for `hz` interrupts per second from a timer running at `rate` ticks
/// per second, or `None` if it rounds to zero or does not fit 32 bits.
pub const fn initial_count(rate: u64, hz: u64) -> Option<u32> {
    if hz == 0 {
        return None;
    }
    let count = rate / hz;
    if count == 0 || count > u32::MAX as u64 {
        None
    } else {
        Some(count as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions_follow_the_legal_mode_order() {
        let base = 0xFEE0_0000_u64 | (1 << 8); // BSP flag, xAPIC MMIO base
        // Disabled: enable xAPIC first, then x2APIC.
        let (w, n) = x2apic_transition(base);
        assert_eq!(n, 2);
        assert_eq!(w[0], base | APIC_GLOBAL_ENABLE);
        assert_eq!(w[1], base | APIC_GLOBAL_ENABLE | APIC_X2APIC);
        // xAPIC: one write.
        let (w, n) = x2apic_transition(base | APIC_GLOBAL_ENABLE);
        assert_eq!((n, w[0]), (1, base | APIC_GLOBAL_ENABLE | APIC_X2APIC));
        // Already x2APIC: nothing.
        assert_eq!(
            x2apic_transition(base | APIC_GLOBAL_ENABLE | APIC_X2APIC).1,
            0
        );
        // The invalid state never survives into a write.
        let (w, n) = x2apic_transition(base | APIC_X2APIC);
        assert_eq!(n, 2);
        assert_eq!(
            w[0] & (APIC_GLOBAL_ENABLE | APIC_X2APIC),
            APIC_GLOBAL_ENABLE
        );
    }

    #[test]
    fn register_encodings() {
        assert_eq!(svr(0xFF), 0x1FF);
        assert_eq!(lvt_timer(0x20, TimerMode::Periodic, false), 0x2_0020);
        assert_eq!(lvt_timer(0x20, TimerMode::OneShot, true), 0x1_0020);
        let codes: Vec<_> = [1, 2, 4, 8, 16, 32, 64, 128]
            .iter()
            .map(|&d| divide_config(d).unwrap())
            .collect();
        assert_eq!(
            codes,
            [
                0b1011, 0b0000, 0b0001, 0b0010, 0b0011, 0b1000, 0b1001, 0b1010
            ]
        );
        assert_eq!(divide_config(3), None);
        assert_eq!(divide_config(256), None);
    }

    #[test]
    fn pit_windows() {
        assert_eq!(pit_count(10_000), Some(11_931));
        assert_eq!(pit_count(54_000), Some(64_431));
        assert_eq!(pit_count(55_000), None, "beyond 16 bits");
        assert_eq!(pit_count(0), None);
        // 11 931 clocks of 838.1 ns is 9.9993 ms.
        assert_eq!(pit_nanos(11_931), 9_999_312);
        assert_eq!(PIT_CHANNEL2_MODE0, 0xB0);
    }

    #[test]
    fn calibration_arithmetic() {
        // 1 000 000 ticks in 10 ms is 100 MHz; 1000 Hz then needs 100 000 per period.
        let r = rate(1_000_000, 10_000_000).unwrap();
        assert_eq!(r, 100_000_000);
        assert_eq!(initial_count(r, 1000), Some(100_000));
        assert_eq!(rate(5, 0), None);
        assert_eq!(rate(u64::MAX, 1), None);
        assert_eq!(initial_count(999, 1000), None, "rounds to zero");
        assert_eq!(initial_count(u64::MAX, 1), None, "beyond 32 bits");
        assert_eq!(initial_count(1, 0), None);
    }
}
