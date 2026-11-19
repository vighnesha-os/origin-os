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

/// The bits of the divide configuration register that hold the divisor (Intel SDM Vol. 3A,
/// "Divide Configuration Register": bits 0, 1 and 3; the rest are reserved).
pub const DIVIDE_BITS: u64 = 0b1011;

/// The bits of the LVT timer entry that say how the timer interrupts: the vector, the mask and
/// the timer mode. Bit 12, the delivery status, is the hardware's and is left out.
pub const LVT_TIMER_BITS: u64 = 0xFF | LVT_MASKED | (0b11 << 17);

/// A local APIC timer's programming: its initial count, its divide configuration's divisor
/// bits ([`DIVIDE_BITS`]) and its LVT timer entry's vector, mask and mode ([`LVT_TIMER_BITS`]).
/// For the preemption proof's check that every processor's tick is programmed as the boot
/// processor's timed proof ran it (TASKS item 66, the review's T66-R1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimerProgramming {
    pub initial: u64,
    pub divide: u64,
    pub lvt: u64,
}

impl TimerProgramming {
    /// The programming from the three registers' raw values, keeping only the bits that set
    /// the rate and the interrupt.
    pub const fn from_registers(initial: u64, divide: u64, lvt: u64) -> Self {
        Self {
            initial,
            divide: divide & DIVIDE_BITS,
            lvt: lvt & LVT_TIMER_BITS,
        }
    }

    /// The first register that differs from `expected`, by name, with this value and the
    /// expected one; `None` if all three agree.
    pub const fn first_difference(&self, expected: &Self) -> Option<(&'static str, u64, u64)> {
        if self.initial != expected.initial {
            Some(("initial count", self.initial, expected.initial))
        } else if self.divide != expected.divide {
            Some(("divide configuration", self.divide, expected.divide))
        } else if self.lvt != expected.lvt {
            Some(("LVT entry", self.lvt, expected.lvt))
        } else {
            None
        }
    }
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

/// What one run of the periodic timer's proof showed (TASKS item 66): the timer's rate, from
/// its count against the TSC ([`judge_period`]), and the delivery of its interrupts, stamped with
/// the TSC at the poll that saw each ([`judge_ticks`]); [`tick_verdict`] puts the two together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TickVerdict {
    /// All of them came, within 95 % to 150 % of their nominal time, and the count's period is
    /// within [`RATE_LOW_PPM`] to [`RATE_HIGH_PPM`] of nominal.
    InBounds,
    /// The count's period is short of [`RATE_LOW_PPM`], or all the interrupts came in under 95 %
    /// of their nominal time: the rate is wrong, and it fails at once. A stall can only make
    /// interrupts later, and cannot move the count.
    TooFast,
    /// The run says nothing about the rate, so it is measured again, and never passes: no clean
    /// pair of count reads in [`PERIOD_PAIRS`]' worth, or interrupts too slow or too few with
    /// the median gap between them not over 150 % of the period and some gap of two periods or
    /// more (expiries merged while the processor was away: a virtual processor the host left
    /// unscheduled, or an SMI).
    Stalled,
    /// The count's period is over [`RATE_HIGH_PPM`] of nominal, or the interrupts are too slow
    /// or too few with nothing to show a stall, or with a median gap over 150 % of the period:
    /// the rate is wrong, or the interrupts do not come.
    Wrong,
}

/// What [`judge_ticks`] found in one run, for the boot's line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TickRun {
    pub verdict: TickVerdict,
    /// From the timer's start to the last interrupt taken, in TSC ticks.
    pub elapsed: u64,
    /// The median gap between consecutive interrupts (the first measured from the start).
    pub median_gap: u64,
    /// The longest such gap.
    pub longest_gap: u64,
}

/// The most interrupts [`judge_ticks`] looks at.
pub const MAX_TICK_STAMPS: usize = 64;

/// Judges one run of the periodic timer's proof: the timer started at TSC `start`, `stamps` holds
/// the TSC at which each interrupt was seen (in order, at most [`MAX_TICK_STAMPS`] are used), the
/// run wanted `wanted` of them, and the programmed period is `period` TSC ticks.
///
/// The stamps judge the interrupts' delivery only (the addendum review's T66-R13): the rate
/// comes from the count ([`judge_period`]). It checks exactly this, in order, on the first
/// `wanted` stamps only: too fast, if all `wanted` came in under 95 % of their nominal time;
/// wrong, if the median gap is over 150 % of the period; in bounds, if all `wanted` came within
/// 150 % of their nominal time; stalled, if some gap is two periods or more; else wrong. A stall
/// widens a few gaps and leaves the median where it was, and a slow delivery moves every gap, so
/// a slow median fails at once. The median is not used for the fast side: under QEMU's software
/// processor the poll-stamped gaps of a correct timer have a median of 90 to 91 % of the period
/// (TASKS item 66's controls), while their total holds.
#[must_use]
pub fn judge_ticks(start: u64, stamps: &[u64], wanted: usize, period: u64) -> TickRun {
    // The first `wanted` only: the run is judged against their nominal time (the review's
    // T66-R4).
    let stamps = stamps
        .get(..stamps.len().min(wanted).min(MAX_TICK_STAMPS))
        .unwrap_or(&[]);
    let mut gaps = [0u64; MAX_TICK_STAMPS];
    let mut previous = start;
    for (gap, &stamp) in gaps.iter_mut().zip(stamps) {
        *gap = stamp.saturating_sub(previous);
        previous = stamp;
    }
    let gaps = gaps.get_mut(..stamps.len()).unwrap_or(&mut []);
    let longest_gap = gaps.iter().copied().max().unwrap_or(0);
    gaps.sort_unstable();
    let median_gap = gaps.get(gaps.len() / 2).copied().unwrap_or(0);
    let elapsed = stamps.last().map_or(0, |&last| last.saturating_sub(start));
    let nominal = period.saturating_mul(wanted as u64);
    let all = stamps.len() >= wanted;
    let verdict = if wanted == 0 || period == 0 || stamps.is_empty() {
        TickVerdict::Wrong
    } else if all && elapsed < nominal / 100 * 95 {
        TickVerdict::TooFast
    } else if median_gap > period / 2 * 3 {
        TickVerdict::Wrong
    } else if all && elapsed <= nominal / 2 * 3 {
        TickVerdict::InBounds
    } else if longest_gap >= period.saturating_mul(2) {
        TickVerdict::Stalled
    } else {
        TickVerdict::Wrong
    };
    TickRun {
        verdict,
        elapsed,
        median_gap,
        longest_gap,
    }
}

/// The least period the timer's count may show, in parts per million of nominal: 97 %.
pub const RATE_LOW_PPM: u64 = 970_000;
/// The most: 103 %. Three times the widest deviation seen under QEMU's software processor
/// (−0.45 % to +0.55 % in TASKS item 66's diagnostic, uncertainty included), and a 1.1× fast
/// timer is outside it (the coordinating session's ruling on the addendum review's T66-R13).
pub const RATE_HIGH_PPM: u64 = 1_030_000;
/// Pairs of count reads a run may try for a clean one before it is undecided.
pub const PERIOD_PAIRS: u32 = 64;
/// A pair of count reads is clean when its two brackets together are within this share of its
/// interval: 1 % (the addendum review's T66-R13).
pub const PERIOD_SHARE: u64 = 100;

/// Measures a running periodic timer's period from its count, reloaded with `initial`, in pairs
/// of reads within one period (the addendum review's T66-R13), each read taken by `read`: from
/// one read until a fifth of the `nominal` period has passed, or to the read before the next
/// reload if that comes first. A pair is read again if its brackets together are wider than
/// 1/[`PERIOD_SHARE`] of its interval, if it is shorter than a twentieth of the nominal period,
/// or if two consecutive reads span more than a hundredth of it, from the first's bracket's start
/// to the second's bracket's end: time in which a reload could pass unseen, a stall inside either
/// read's bracket included (the second addendum review's T66-R17). The first clean pair, and how
/// many pairs were tried; `None` after [`PERIOD_PAIRS`].
pub fn sample_period(
    initial: u64,
    nominal: u64,
    mut read: impl FnMut() -> CountRead,
) -> (Option<PeriodSample>, u32) {
    let target = nominal / 5;
    let least = nominal / 20;
    let gap = nominal / 100;
    let mut tried = 0;
    let mut first = read();
    let mut prev = first;
    while tried < PERIOD_PAIRS {
        let next = read();
        if next.after.wrapping_sub(prev.before) > gap {
            // Long enough, between the two reads or inside either, for a reload to have passed
            // unseen.
            tried += 1;
            first = next;
            prev = next;
            continue;
        }
        let reloaded = next.count > prev.count;
        let last = if reloaded { prev } else { next };
        let span = last.twice_middle().wrapping_sub(first.twice_middle()) / 2;
        if reloaded || span >= target {
            tried += 1;
            if span >= least
                && let Some(sample) = PeriodSample::from_reads(initial, first, last)
                && sample.clean(PERIOD_SHARE)
            {
                return (Some(sample), tried);
            }
            first = next;
        }
        prev = next;
    }
    (None, tried)
}

/// What the timer's count says of its rate (the addendum review's T66-R13).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateVerdict {
    /// Its period is within [`RATE_LOW_PPM`] to [`RATE_HIGH_PPM`] of nominal.
    InBounds,
    /// Shorter: the timer runs fast.
    Fast,
    /// Longer: it runs slow.
    Slow,
    /// No clean pair of reads: nothing is known.
    Undecided,
}

/// Judges the timer's rate from a clean pair of reads of its count, `None` if none came, against
/// the `nominal` period in TSC ticks. The count falls at the timer's own rate whatever its
/// interrupts do, so neither their delivery nor a host's stalls move this verdict.
#[must_use]
pub fn judge_period(sample: Option<PeriodSample>, nominal: u64) -> RateVerdict {
    match sample.map(|s| s.ppm_of(nominal)) {
        None => RateVerdict::Undecided,
        Some(ppm) if ppm < RATE_LOW_PPM => RateVerdict::Fast,
        Some(ppm) if ppm > RATE_HIGH_PPM => RateVerdict::Slow,
        Some(_) => RateVerdict::InBounds,
    }
}

/// A run's verdict: a wrong rate fails at once, whatever the interrupts did; then a fault in
/// their delivery fails at once; then an undecided rate or a stalled delivery is measured again;
/// only a rate in bounds with every interrupt delivered in bounds passes.
#[must_use]
pub const fn tick_verdict(delivery: TickVerdict, rate: RateVerdict) -> TickVerdict {
    match (rate, delivery) {
        (RateVerdict::Fast, _) => TickVerdict::TooFast,
        (RateVerdict::Slow, _) => TickVerdict::Wrong,
        (_, TickVerdict::TooFast) => TickVerdict::TooFast,
        (_, TickVerdict::Wrong) => TickVerdict::Wrong,
        (RateVerdict::Undecided, _) | (_, TickVerdict::Stalled) => TickVerdict::Stalled,
        (RateVerdict::InBounds, TickVerdict::InBounds) => TickVerdict::InBounds,
    }
}

/// One read of the APIC timer's current count, bracketed by the TSC (TASKS item 66, the
/// review's T66-R13).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CountRead {
    pub before: u64,
    pub count: u64,
    pub after: u64,
}

impl CountRead {
    /// Twice the TSC at the read's middle, kept whole.
    const fn twice_middle(&self) -> u64 {
        self.before.saturating_add(self.after)
    }
}

/// The periodic timer's period as two reads of its count within one period show it, against
/// the TSC (the review's T66-R13): the count falls at the timer's own rate whatever its
/// interrupts do, so neither their delivery nor a host's stall between the reads moves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeriodSample {
    /// TSC ticks between the reads' middles.
    pub interval: u64,
    /// How far the count fell between them.
    pub fall: u64,
    /// The two brackets' widths added: the interval is known to within half of this.
    pub spread: u64,
    /// The period in TSC ticks: the initial count at the measured rate.
    pub period: u64,
}

impl PeriodSample {
    /// From reads `first` and `last` of a timer reloaded with `initial`, with no reload between
    /// them; `None` if the count did not fall or no time passed.
    pub fn from_reads(initial: u64, first: CountRead, last: CountRead) -> Option<Self> {
        let fall = first.count.checked_sub(last.count).filter(|&f| f > 0)?;
        let interval = last.twice_middle().checked_sub(first.twice_middle())? / 2;
        if interval == 0 {
            return None;
        }
        let spread = first
            .after
            .saturating_sub(first.before)
            .saturating_add(last.after.saturating_sub(last.before));
        let period =
            u64::try_from(u128::from(initial) * u128::from(interval) / u128::from(fall)).ok()?;
        Some(Self {
            interval,
            fall,
            spread,
            period,
        })
    }

    /// Whether its brackets together are within `1/share` of its interval.
    pub const fn clean(&self, share: u64) -> bool {
        self.spread.saturating_mul(share) <= self.interval
    }

    /// Its period in parts per million of `nominal` TSC ticks.
    pub fn ppm_of(&self, nominal: u64) -> u64 {
        if nominal == 0 {
            return 0;
        }
        u64::try_from(u128::from(self.period) * 1_000_000 / u128::from(nominal)).unwrap_or(u64::MAX)
    }

    /// Its uncertainty, in parts per million of the period: half the spread over the interval.
    pub fn uncertainty_ppm(&self) -> u64 {
        u64::try_from(u128::from(self.spread) * 500_000 / u128::from(self.interval.max(1)))
            .unwrap_or(u64::MAX)
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
    fn a_timer_s_programming_keeps_only_the_bits_that_set_the_rate_and_the_interrupt() {
        let periodic = lvt_timer(TIMER_VECTOR, TimerMode::Periodic, false);
        let p = TimerProgramming::from_registers(625_000, 0b0011 | 0b0100, periodic | (1 << 12));
        assert_eq!(
            p,
            TimerProgramming {
                initial: 625_000,
                divide: 0b0011,
                lvt: periodic
            }
        );
        assert_eq!(p.first_difference(&p), None);
        let one_shot = TimerProgramming::from_registers(
            625_000,
            0b0011,
            lvt_timer(TIMER_VECTOR, TimerMode::OneShot, false),
        );
        assert_eq!(
            one_shot.first_difference(&p),
            Some(("LVT entry", u64::from(TIMER_VECTOR), periodic))
        );
        let masked = TimerProgramming::from_registers(625_000, 0b0011, periodic | LVT_MASKED);
        assert_eq!(masked.first_difference(&p).map(|d| d.0), Some("LVT entry"));
        let divide = TimerProgramming::from_registers(625_000, 0b1010, periodic);
        assert_eq!(
            divide.first_difference(&p),
            Some(("divide configuration", 0b1010, 0b0011))
        );
        let count = TimerProgramming::from_registers(5_000_000, 0b0011, periodic);
        assert_eq!(
            count.first_difference(&p),
            Some(("initial count", 5_000_000, 625_000))
        );
    }

    /// The tick proof's period at 100 Hz on a 1 GHz TSC, and the run it wants.
    const PERIOD: u64 = 10_000_000;
    const WANTED: usize = 50;

    /// Stamps of `n` interrupts every `gap` ticks from 0, with `extra` added to interrupt
    /// `stall_at` and every one after it (a stall merging expiries there).
    fn stamps(n: usize, gap: u64, stall_at: usize, extra: u64) -> alloc::vec::Vec<u64> {
        (1..=n as u64)
            .map(|i| i * gap + if i as usize > stall_at { extra } else { 0 })
            .collect()
    }

    #[test]
    fn a_run_at_the_programmed_rate_is_in_bounds_and_its_edges_hold() {
        let run = judge_ticks(0, &stamps(WANTED, PERIOD, WANTED, 0), WANTED, PERIOD);
        assert_eq!(run.verdict, TickVerdict::InBounds);
        assert_eq!(
            (run.elapsed, run.median_gap, run.longest_gap),
            (500_000_000, PERIOD, PERIOD)
        );
        // 150 % exactly passes; a tick more is slow, and with even gaps it is wrong.
        let edge = judge_ticks(
            0,
            &stamps(WANTED, PERIOD * 3 / 2, WANTED, 0),
            WANTED,
            PERIOD,
        );
        assert_eq!(edge.verdict, TickVerdict::InBounds);
        let over = judge_ticks(0, &stamps(WANTED, PERIOD * 3 / 2, 49, 1), WANTED, PERIOD);
        assert_eq!(over.verdict, TickVerdict::Wrong);
        // 95 % exactly passes; under it fails at once.
        let fast = judge_ticks(
            0,
            &stamps(WANTED, PERIOD * 95 / 100, WANTED, 0),
            WANTED,
            PERIOD,
        );
        assert_eq!(fast.verdict, TickVerdict::InBounds);
        let faster = judge_ticks(
            0,
            &stamps(WANTED, PERIOD * 94 / 100, WANTED, 0),
            WANTED,
            PERIOD,
        );
        assert_eq!(faster.verdict, TickVerdict::TooFast);
    }

    #[test]
    fn a_stall_that_merges_expiries_earns_a_repeat_and_never_a_pass() {
        // 300 ms lost after the 10th interrupt: 800 ms in all, the median still the period.
        let run = judge_ticks(0, &stamps(WANTED, PERIOD, 10, 300_000_000), WANTED, PERIOD);
        assert_eq!(run.verdict, TickVerdict::Stalled);
        assert_eq!(run.median_gap, PERIOD);
        assert_eq!(run.longest_gap, PERIOD + 300_000_000);
        // A stall inside the bounds changes nothing: it passes as before.
        let small = judge_ticks(0, &stamps(WANTED, PERIOD, 10, 200_000_000), WANTED, PERIOD);
        assert_eq!(small.verdict, TickVerdict::InBounds);
        // Too few in the time, with a stall and the median in bounds: a repeat, not a pass.
        let short = judge_ticks(0, &stamps(30, PERIOD, 5, 1_500_000_000), WANTED, PERIOD);
        assert_eq!(short.verdict, TickVerdict::Stalled);
    }

    #[test]
    fn a_wrong_rate_fails_whatever_the_stalls() {
        // A divisor twice too large: every gap two periods. The median says so.
        let slow = judge_ticks(0, &stamps(WANTED, 2 * PERIOD, WANTED, 0), WANTED, PERIOD);
        assert_eq!(slow.verdict, TickVerdict::Wrong);
        let slow_and_stalled = judge_ticks(
            0,
            &stamps(WANTED, 2 * PERIOD, 10, 300_000_000),
            WANTED,
            PERIOD,
        );
        assert_eq!(slow_and_stalled.verdict, TickVerdict::Wrong);
        // Slow by 60 % with even gaps: no stall to blame.
        let uneven = judge_ticks(
            0,
            &stamps(WANTED, PERIOD * 16 / 10, WANTED, 0),
            WANTED,
            PERIOD,
        );
        assert_eq!(uneven.verdict, TickVerdict::Wrong);
        // No interrupt at all.
        assert_eq!(
            judge_ticks(0, &[], WANTED, PERIOD).verdict,
            TickVerdict::Wrong
        );
        // Too few, evenly spaced, no stall: the interrupts are slow or stop.
        let few = judge_ticks(0, &stamps(20, PERIOD, 20, 0), WANTED, PERIOD);
        assert_eq!(few.verdict, TickVerdict::Wrong);
    }

    #[test]
    fn more_stamps_than_wanted_are_judged_on_the_first_wanted() {
        // The review's T66-R4: 64 stamps at 75 % of the period, 50 wanted. Judged on all 64 the
        // total looked in bounds; on the first 50 it is too fast.
        let run = judge_ticks(0, &stamps(64, PERIOD * 3 / 4, 64, 0), WANTED, PERIOD);
        assert_eq!(run.verdict, TickVerdict::TooFast);
        assert_eq!(run.elapsed, 50 * PERIOD * 3 / 4);
        // 64 at the period: in bounds, on the first 50.
        let run = judge_ticks(0, &stamps(64, PERIOD, 64, 0), WANTED, PERIOD);
        assert_eq!(
            (run.verdict, run.elapsed),
            (TickVerdict::InBounds, 50 * PERIOD)
        );
    }

    /// A clean pair of count reads of a timer whose period is exactly `percent` % of nominal:
    /// 125 000 of its 625 000 counts fall over a fifth of that period.
    fn sample_at(percent: u64) -> Option<PeriodSample> {
        let interval = PERIOD / 5 * percent / 100;
        PeriodSample::from_reads(
            625_000,
            CountRead {
                before: 0,
                count: 600_000,
                after: 5_000,
            },
            CountRead {
                before: interval,
                count: 475_000,
                after: interval + 5_000,
            },
        )
    }

    #[test]
    fn a_fast_tick_masked_by_stalls_fails_on_its_count() {
        // The addendum review's T66-R13 probe: twice the rate with a stall of five periods in
        // every tenth gap. Its stamps' total is 500 ms, so their delivery looks right...
        let mut masked = alloc::vec::Vec::new();
        let mut at = 0;
        for i in 1..=WANTED as u64 {
            at += PERIOD / 2 + if i % 10 == 0 { 5 * PERIOD } else { 0 };
            masked.push(at);
        }
        let run = judge_ticks(0, &masked, WANTED, PERIOD);
        assert_eq!((run.elapsed, run.median_gap), (500_000_000, PERIOD / 2));
        assert_eq!(run.verdict, TickVerdict::InBounds);
        // ...and its count, which falls at twice the rate whatever the stalls, fails it.
        let rate = judge_period(sample_at(50), PERIOD);
        assert_eq!(rate, RateVerdict::Fast);
        assert_eq!(tick_verdict(run.verdict, rate), TickVerdict::TooFast);
        // 1.5 times the rate with stalls of 3.4 periods in every tenth gap, the review's second
        // probe: the same.
        let mut masked = alloc::vec::Vec::new();
        let mut at = 0;
        for i in 1..=WANTED as u64 {
            at += PERIOD * 2 / 3 + if i % 10 == 0 { PERIOD * 34 / 10 } else { 0 };
            masked.push(at);
        }
        let run = judge_ticks(0, &masked, WANTED, PERIOD);
        assert_eq!(run.verdict, TickVerdict::InBounds);
        let rate = judge_period(sample_at(66), PERIOD);
        assert_eq!(tick_verdict(run.verdict, rate), TickVerdict::TooFast);
        // 10 % fast with one 50 ms stall: the old limit, now caught by the count.
        let run = judge_ticks(
            0,
            &stamps(WANTED, PERIOD * 9 / 10, 10, 50_000_000),
            WANTED,
            PERIOD,
        );
        assert_eq!(run.verdict, TickVerdict::InBounds);
        let rate = judge_period(sample_at(90), PERIOD);
        assert_eq!(tick_verdict(run.verdict, rate), TickVerdict::TooFast);
        // Undisturbed, a fast rate fails by its total as well.
        let run = judge_ticks(0, &stamps(WANTED, PERIOD / 2, WANTED, 0), WANTED, PERIOD);
        assert_eq!(run.verdict, TickVerdict::TooFast);
    }

    /// A simulated periodic timer of `initial` counts and a true period of `period` TSC ticks,
    /// reloaded at TSC 0, read from TSC `start` 1 µs apart with 400 ns brackets; the read numbered
    /// `stalled` sampling its count `stall` ticks after its bracket opened.
    fn simulated_reads(
        initial: u64,
        period: u64,
        start: u64,
        stalled: usize,
        stall: u64,
    ) -> impl FnMut() -> CountRead {
        let mut at = start;
        let mut n = 0;
        move || {
            let before = at;
            let sampled = before + 200 + if n == stalled { stall } else { 0 };
            let into = sampled % period;
            let count = initial - into * initial / period;
            let after = sampled + 200;
            at = after + 600;
            n += 1;
            CountRead {
                before,
                count,
                after,
            }
        }
    }

    #[test]
    fn a_stall_inside_an_intermediate_read_cannot_hide_a_reload() {
        // The second addendum review's T66-R17: a timer 10 times fast (a period of 1 ms against
        // 10 ms nominal), read from 111 µs before a reload; the read 61 µs on stalls 1.01 ms
        // inside its bracket and samples a count below the one before, so the reload falls
        // between them unseen if the gap is measured only between the brackets. The pair would
        // then close at the next reload with clean end brackets and a period near nominal.
        let (initial, nominal) = (625_000, PERIOD);
        let fast = nominal / 10;
        let (sample, _) = sample_period(
            initial,
            nominal,
            simulated_reads(initial, fast, 3 * fast - 111_000, 61, 1_010_000),
        );
        // Measured across the brackets, the stall restarts the pair: the clean pair found
        // after it shows the timer's true period, and the run fails at once.
        let sample = sample.unwrap();
        assert!(
            (99_000..=101_000).contains(&sample.ppm_of(nominal)),
            "{sample:?}"
        );
        assert_eq!(judge_period(Some(sample), nominal), RateVerdict::Fast);
        // A correct timer with the same stall still passes.
        let (sample, _) = sample_period(
            initial,
            nominal,
            simulated_reads(initial, nominal, 3 * nominal - 111_000, 61, 1_010_000),
        );
        assert_eq!(judge_period(sample, nominal), RateVerdict::InBounds);
        // Undisturbed, both are measured as they are.
        let (sample, tried) = sample_period(
            initial,
            nominal,
            simulated_reads(initial, nominal, 0, usize::MAX, 0),
        );
        assert_eq!((sample.unwrap().ppm_of(nominal) / 1_000, tried), (1_000, 1));
    }

    #[test]
    fn the_count_judges_the_rate_at_its_edges() {
        assert_eq!(judge_period(sample_at(100), PERIOD), RateVerdict::InBounds);
        assert_eq!(judge_period(sample_at(97), PERIOD), RateVerdict::InBounds);
        assert_eq!(judge_period(sample_at(103), PERIOD), RateVerdict::InBounds);
        assert_eq!(judge_period(sample_at(96), PERIOD), RateVerdict::Fast);
        assert_eq!(judge_period(sample_at(104), PERIOD), RateVerdict::Slow);
        // 1.1 times fast is a period of 90.9 %.
        let fast = PeriodSample::from_reads(
            625_000,
            CountRead {
                before: 0,
                count: 600_000,
                after: 1_000,
            },
            CountRead {
                before: 2_000_000,
                count: 600_000 - 137_500,
                after: 2_001_000,
            },
        )
        .unwrap();
        assert_eq!(fast.ppm_of(PERIOD), 909_090);
        assert_eq!(judge_period(Some(fast), PERIOD), RateVerdict::Fast);
        // No clean pair: undecided, which is measured again and never passes, whatever the
        // interrupts did.
        assert_eq!(judge_period(None, PERIOD), RateVerdict::Undecided);
        for delivery in [
            TickVerdict::InBounds,
            TickVerdict::Stalled,
            TickVerdict::TooFast,
            TickVerdict::Wrong,
        ] {
            assert_ne!(
                tick_verdict(delivery, RateVerdict::Undecided),
                TickVerdict::InBounds
            );
            assert_eq!(
                tick_verdict(delivery, RateVerdict::Fast),
                TickVerdict::TooFast
            );
            assert_eq!(
                tick_verdict(delivery, RateVerdict::Slow),
                TickVerdict::Wrong
            );
        }
        assert_eq!(
            tick_verdict(TickVerdict::InBounds, RateVerdict::Undecided),
            TickVerdict::Stalled
        );
        assert_eq!(
            tick_verdict(TickVerdict::Stalled, RateVerdict::InBounds),
            TickVerdict::Stalled
        );
        assert_eq!(
            tick_verdict(TickVerdict::InBounds, RateVerdict::InBounds),
            TickVerdict::InBounds
        );
    }

    #[test]
    fn a_correct_timer_seen_unevenly_passes() {
        // Poll-stamped gaps that jitter, two in three of them short: the median is not judged on
        // the fast side.
        let mut jitter = alloc::vec::Vec::new();
        let mut at = 0;
        for i in 0..WANTED as u64 {
            at += [PERIOD * 9 / 10, PERIOD * 9 / 10, PERIOD * 12 / 10][(i % 3) as usize];
            jitter.push(at);
        }
        let run = judge_ticks(0, &jitter, WANTED, PERIOD);
        assert!(run.median_gap < PERIOD / 100 * 95);
        assert_eq!(run.verdict, TickVerdict::InBounds);
        // Some ticks seen late by just under a period, the most before the next is raised.
        let late: alloc::vec::Vec<u64> = (1..=WANTED as u64)
            .map(|i| i * PERIOD + if i % 7 == 1 { PERIOD - 1 } else { 0 })
            .collect();
        let run = judge_ticks(0, &late, WANTED, PERIOD);
        assert_eq!(run.verdict, TickVerdict::InBounds);
        // A stall inside the bounds passes, as before: a gap of two periods comes in almost every
        // run with load on the host.
        let run = judge_ticks(0, &stamps(WANTED, PERIOD, 10, 200_000_000), WANTED, PERIOD);
        assert_eq!(run.verdict, TickVerdict::InBounds);
        // A slow median with a total in bounds is wrong: 25 gaps of 1.6 periods.
        let mut s = stamps(25, PERIOD, 25, 0);
        let last = *s.last().unwrap();
        s.extend((1..=25).map(|i| last + i * PERIOD * 16 / 10));
        let run = judge_ticks(0, &s, WANTED, PERIOD);
        assert_eq!(run.verdict, TickVerdict::Wrong);
    }

    fn read(at: u64, count: u64, width: u64) -> CountRead {
        CountRead {
            before: at,
            count,
            after: at + width,
        }
    }

    #[test]
    fn two_count_reads_give_the_period() {
        // A timer reloaded with 625 000 counting 62.5 per microsecond on a 1 GHz TSC: a 10 ms
        // period. 2 ms between the reads' middles, 125 000 counts.
        let s = PeriodSample::from_reads(
            625_000,
            read(1_000_000, 600_000, 2_000),
            read(3_000_000, 475_000, 2_000),
        )
        .unwrap();
        assert_eq!((s.interval, s.fall, s.spread), (2_000_000, 125_000, 4_000));
        assert_eq!(s.period, 10_000_000);
        assert_eq!(s.ppm_of(10_000_000), 1_000_000);
        assert_eq!(s.uncertainty_ppm(), 1_000);
        assert!(s.clean(100));
        // Twice the rate: the same fall in half the time.
        let fast = PeriodSample::from_reads(
            625_000,
            read(0, 600_000, 1_000),
            read(1_000_000, 475_000, 1_000),
        )
        .unwrap();
        assert_eq!(fast.ppm_of(10_000_000), 500_000);
        // A bracket a stall widened is not clean.
        let wide = PeriodSample::from_reads(
            625_000,
            read(0, 600_000, 30_000),
            read(2_000_000, 475_000, 1_000),
        )
        .unwrap();
        assert!(!wide.clean(100));
        // A count that did not fall, or rose (a reload between), gives nothing.
        assert_eq!(
            PeriodSample::from_reads(625_000, read(0, 5, 1), read(10, 5, 1)),
            None
        );
        assert_eq!(
            PeriodSample::from_reads(625_000, read(0, 5, 1), read(10, 600_000, 1)),
            None
        );
        assert_eq!(
            PeriodSample::from_reads(625_000, read(10, 9, 1), read(10, 5, 1)),
            None
        );
    }

    #[test]
    fn over_a_grid_only_an_in_bounds_run_passes() {
        // Every rate from 50 % to 300 % of the period, with stalls of 0 to 1.5 s at three
        // places, its count read as that rate: InBounds only with the count's period within
        // 97 % to 103 % and the whole run within 95 % to 150 %; Stalled only with the count's
        // period in bounds and a gap of two periods; a period out of bounds fails, whatever the
        // stalls.
        for percent in (50..=300).step_by(1) {
            let gap = PERIOD * percent / 100;
            let rate = judge_period(sample_at(percent), PERIOD);
            for extra in [0, 20_000_000, 100_000_000, 400_000_000, 1_500_000_000] {
                for at in [0, 10, 49] {
                    let s = stamps(WANTED, gap, at, extra);
                    let run = judge_ticks(0, &s, WANTED, PERIOD);
                    let verdict = tick_verdict(run.verdict, rate);
                    let elapsed = *s.last().unwrap();
                    let nominal = PERIOD * WANTED as u64;
                    let rate_in = (97..=103).contains(&percent);
                    match verdict {
                        TickVerdict::InBounds => assert!(
                            rate_in && elapsed >= nominal / 100 * 95 && elapsed <= nominal / 2 * 3,
                            "{percent}% +{extra} at {at}: {run:?}"
                        ),
                        TickVerdict::Stalled => assert!(
                            rate_in && run.longest_gap >= 2 * PERIOD,
                            "{percent}% +{extra} at {at}: {run:?}"
                        ),
                        TickVerdict::TooFast => assert!(
                            percent < 97 || elapsed < nominal / 100 * 95,
                            "{percent}%: {run:?}"
                        ),
                        TickVerdict::Wrong => assert!(
                            percent > 103 || run.verdict == TickVerdict::Wrong,
                            "{percent}%: {run:?}"
                        ),
                    }
                    if !rate_in {
                        assert!(
                            matches!(verdict, TickVerdict::TooFast | TickVerdict::Wrong),
                            "{percent}% +{extra} at {at}: never passed, never repeated"
                        );
                    }
                }
            }
        }
    }

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
