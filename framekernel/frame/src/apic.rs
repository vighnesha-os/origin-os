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
#![forbid(unsafe_code)]

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

/// How far a free-running counter of `mask`'s width advanced from `from` to `to`, across
/// one wrap if it wrapped (`mask` is `0xFF_FFFF` for a 24-bit counter).
pub const fn counter_elapsed(from: u64, to: u64, mask: u64) -> u64 {
    to.wrapping_sub(from) & mask
}

/// The counter ticks of a `micros`-microsecond window on a counter running at `hz`, or `None`
/// if it rounds to nothing.
pub const fn window_ticks(hz: u64, micros: u64) -> Option<u64> {
    let ticks = hz.saturating_mul(micros) / 1_000_000;
    if ticks == 0 { None } else { Some(ticks) }
}

/// The nanoseconds `ticks` ticks of a counter at `hz` take, or `None` if `hz` is zero or the
/// result does not fit.
pub const fn ticks_nanos(ticks: u64, hz: u64) -> Option<u64> {
    if hz == 0 {
        return None;
    }
    let nanos = ticks as u128 * 1_000_000_000 / hz as u128;
    if nanos > u64::MAX as u128 {
        None
    } else {
        Some(nanos as u64)
    }
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

/// The largest share of a PIT calibration window that its start, its end and the reading
/// of the count-down may each be uncertain by: 1/500 (20 µs of a 10 ms window).
const CALIBRATION_MAX_SKEW_SHARE: u64 = 500;

/// Readings of the counter being calibrated around one PIT one-shot, enough to tell
/// whether the window was observed closely.
///
/// A polled calibration has two instants that matter, the gate write that starts the
/// PIT and the first poll that sees OUT2 high, and a stall on either (an SMI, or a
/// virtual CPU descheduled on a loaded host) corrupts the rate by as much as the stall:
/// in a 10 ms window a 3.5 ms stall reads as a 35 % faster TSC. The readings bracket those
/// instants, so a spoiled window can be recognised and measured again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalibrationWindow {
    /// Before the gate write that starts the PIT and the APIC count-down.
    pub before_start: u64,
    /// After the gate write and the start of the APIC count-down.
    pub after_start: u64,
    /// Before the last poll of OUT2 that still read low.
    pub before_end: u64,
    /// After the poll of OUT2 that read high.
    pub after_end: u64,
    /// After reading the APIC's current count.
    pub after_read: u64,
}

impl CalibrationWindow {
    /// The window's length in counter ticks, taking each end at the middle of its
    /// bracket: the best estimate, however disturbed the window was.
    pub const fn ticks(&self) -> u64 {
        let start = self.before_start + self.after_start.saturating_sub(self.before_start) / 2;
        let end = self.before_end + self.after_end.saturating_sub(self.before_end) / 2;
        end.saturating_sub(start)
    }

    /// How much the window's start, end and APIC reading could together be off by, in
    /// counter ticks: the measure of how disturbed the window was.
    pub const fn skew(&self) -> u64 {
        self.after_start
            .saturating_sub(self.before_start)
            .saturating_add(self.after_end.saturating_sub(self.before_end))
            .saturating_add(self.after_read.saturating_sub(self.after_end))
    }

    /// Whether this window was observed closely enough to trust: its start, its end and
    /// the reading of the count-down each within 1/500 of the window (0.2 %, against the
    /// 1 % the RTC proof allows), and not empty.
    pub const fn clean(&self) -> bool {
        let limit = self.after_end.saturating_sub(self.before_start) / CALIBRATION_MAX_SKEW_SHARE;
        limit != 0
            && self.after_start.saturating_sub(self.before_start) <= limit
            && self.after_end.saturating_sub(self.before_end) <= limit
            && self.after_read.saturating_sub(self.after_end) <= limit
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

    /// A 10 ms window on a 1 GHz counter, observed as closely as the code does.
    #[test]
    fn a_counter_wraps_at_its_own_width() {
        const PM24: u64 = 0xFF_FFFF;
        assert_eq!(counter_elapsed(100, 150, PM24), 50);
        assert_eq!(counter_elapsed(0xFF_FFF0, 0x10, PM24), 0x20);
        assert_eq!(counter_elapsed(5, 5, PM24), 0);
        const PM32: u64 = 0xFFFF_FFFF;
        assert_eq!(counter_elapsed(0xFFFF_FFF0, 0x10, PM32), 0x20);
        assert_eq!(counter_elapsed(10, 2, u64::MAX), u64::MAX - 7);
    }

    #[test]
    fn a_window_on_a_reference_is_as_long_as_its_rate_says() {
        // The ACPI PM timer's 3.579545 MHz over 10 ms.
        assert_eq!(window_ticks(3_579_545, 10_000), Some(35_795));
        assert_eq!(ticks_nanos(35_795, 3_579_545), Some(9_999_874));
        assert_eq!(window_ticks(0, 10_000), None);
        assert_eq!(window_ticks(1, 1), None, "rounds to nothing");
        assert_eq!(ticks_nanos(1, 0), None);
        assert_eq!(ticks_nanos(u64::MAX, 1), None, "does not fit");
        // A TSC rate as a reference: 10 ms of it is 10 ms.
        assert_eq!(ticks_nanos(10_000_000, 1_000_000_000), Some(10_000_000));
    }

    fn close_window() -> CalibrationWindow {
        CalibrationWindow {
            before_start: 1_000_000,
            after_start: 1_002_000,
            before_end: 11_001_000,
            after_end: 11_003_000,
            after_read: 11_004_000,
        }
    }

    #[test]
    fn a_closely_observed_window_is_clean() {
        let w = close_window();
        assert_eq!(w.ticks(), 10_001_000);
        assert_eq!(w.skew(), 2_000 + 2_000 + 1_000);
        assert!(w.clean());
    }

    #[test]
    fn a_window_with_a_stall_at_either_end_is_not_clean() {
        // A share of 1/500 is 20 µs of this 10 ms window: 15 µs is close enough, 25 µs is
        // not.
        let cases: [fn(&mut CalibrationWindow, u64); 3] = [
            |w, d| w.before_start = w.after_start - d,
            |w, d| w.before_end = w.after_end - d,
            |w, d| w.after_read = w.after_end + d,
        ];
        for (i, stall) in cases.iter().enumerate() {
            let mut close = close_window();
            stall(&mut close, 15_000);
            assert!(close.clean(), "case {i}: 15 us");
            let mut far = close_window();
            stall(&mut far, 25_000);
            assert!(!far.clean(), "case {i}: 25 us");
        }
        // A descheduling across the end, as a loaded host produces: the rate would be
        // about 35 % high without this check.
        let mut stalled = close_window();
        stalled.after_end += 3_500_000;
        stalled.after_read += 3_500_000;
        assert!(!stalled.clean());
        assert!(stalled.skew() > close_window().skew());
    }

    #[test]
    fn an_empty_window_is_never_clean() {
        let w = CalibrationWindow {
            before_start: 5,
            after_start: 5,
            before_end: 5,
            after_end: 5,
            after_read: 5,
        };
        assert_eq!(w.ticks(), 0);
        assert_eq!(w.skew(), 0);
        assert!(!w.clean());
    }

    /// Any readings at all: nothing overflows or panics, and a clean window is a window
    /// whose three uncertainties are within the share of its length (CLAUDE.md §13).
    #[test]
    fn any_readings_are_handled() {
        use crate::testing::Rng;
        let mut rng = Rng::new(0x5049_5431);
        for case in 0..crate::testing::cases(50_000) {
            let w = CalibrationWindow {
                before_start: rng.edge_u64(),
                after_start: rng.edge_u64(),
                before_end: rng.edge_u64(),
                after_end: rng.edge_u64(),
                after_read: rng.edge_u64(),
            };
            let (_, skew, clean) = (w.ticks(), w.skew(), w.clean());
            if clean {
                let limit = w.after_end.saturating_sub(w.before_start) / CALIBRATION_MAX_SKEW_SHARE;
                assert_ne!(limit, 0, "case {case}");
                assert!(
                    w.after_start.saturating_sub(w.before_start) <= limit
                        && w.after_end.saturating_sub(w.before_end) <= limit
                        && w.after_read.saturating_sub(w.after_end) <= limit,
                    "case {case}: {w:?}"
                );
                assert!(skew <= limit.saturating_mul(3), "case {case}: {w:?}");
            }
        }
    }
}
