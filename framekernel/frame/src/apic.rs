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
/// A run reads the timer's count for at most this many fifths of the period, a fifth being a
/// pair's usual span: 64, about 128 ms at 100 Hz (budget B, the coordinating session's ruling on
/// TASKS item 78's diagnostic round). A step broken by a gap uses time but is not a pair.
pub const PERIOD_PAIRS: u32 = 64;
/// Steps of at most this share of the period make a stretch: a twentieth. No stretch can hide a
/// reload of a timer under 20 times fast (TASKS item 78, C4/20B).
pub const STRETCH_SHARE: u64 = 20;
/// A pair undecided at a fifth of the period goes on toward the reload, up to this many tenths
/// of it.
const LONGEST_PAIR_TENTHS: u64 = 9;
/// Reads a run takes at the most, whatever each costs and whatever the TSC does. It binds only
/// where the TSC does not pass the budget, a stopped or very slow TSC, which the calibration
/// refuses first: at 1 µs a read, 2^24 reads take about 17 s.
const MOST_READS: u32 = 1 << 24;

/// Measures a running periodic timer's period from its count, reloaded with `initial`, in pairs
/// of reads within one period (the addendum review's T66-R13), each read taken by `read`.
///
/// - **A pair** runs from one read until a fifth of the `nominal` period has passed, or to the
///   read before the next reload if that comes first. A pair is never judged under a twentieth
///   of the period.
/// - **The verdict** is the period's bounds: between the shortest and the longest time its reads
///   can be apart, and a fall of one count more and one less. The pair decides only when those
///   bounds lie wholly inside 97 % to 103 %, or wholly beyond a bound
///   ([`PeriodSample::verdict`]; TASKS item 78, C2).
/// - **A gap,** from one read's bracket's end to the next one's start, over a hundredth of the
///   period breaks the pair: a reload could have passed unseen.
/// - **Stretches (TASKS item 78, C4/20B).** A stall inside a read's own bracket is no gap, and on
///   a fast timer it can hide a reload: the count reads lower after it, as if no reload had
///   come. So the pair's reads are cut into stretches, wherever a step from one read's bracket's
///   start to the next one's end exceeds a [`STRETCH_SHARE`]th of the period. A stretch cannot
///   hide a reload of a timer under 20 times fast, and its reads bound the count's true slope.
///   - A pair whose own slope misses its stretches' bounds is set aside, never judged.
///   - A pair is judged only when its stretches' upper slope is within twice its own lower
///     slope, so they have measured the slope.
///   - So a hidden reload is never judged, for any span and any number of reloads hidden. A
///     pair's observed fall is at most the initial count, so a pair that hides a reload fell
///     truly by more than twice its observed fall less one count. Its stretches hide none, so
///     their upper slope is at least the true one, and so more than twice the pair's lower slope
///     (the independent review's argument, `REVIEW_T78_2026-11-21.md`).
/// - **Longer pairs.** A pair still undecided at a fifth of the period goes on toward the
///   reload, up to 0.9 of it, so reads that each cost up to about 200 µs still decide. That
///   bounds only how long an undecided pair goes on: a pair is judged at whatever span its last
///   read gives, which one long step can carry past 0.9 of the period.
/// - **The budget (B).** A run ends after [`PERIOD_PAIRS`] fifths of the period, about 128 ms at
///   100 Hz, or [`MOST_READS`] reads.
///
/// Returns the first pair that decides, and how many pairs were tried, the deciding one
/// included. It also returns what the reads showed ([`CountFigures`]), which judges nothing.
///
/// From 20 times fast (strictly, 20 × (1 − 1/`initial`)) a stretch could hide a reload. The timer
/// then delivers 50 interrupts in under 25 ms, which delivery fails unless the same run's
/// interrupts are held off for about 450 ms or more.
///
/// The rule assumes a count that falls linearly between reloads. A correct timer whose count
/// stays at 0 for a while after an expiry before it reloads can be judged slow, and so fail the
/// run at once: a stated limit, with no guard (the review's T78-R7; TASKS item 115).
pub fn sample_period(
    initial: u64,
    nominal: u64,
    mut read: impl FnMut() -> CountRead,
) -> (Option<PeriodSample>, u32, CountFigures) {
    let target = nominal / 5;
    let least = nominal / 20;
    let gap = nominal / 100;
    let stretch = nominal / STRETCH_SHARE;
    let longest = nominal / 10 * LONGEST_PAIR_TENTHS;
    let budget = target.saturating_mul(u64::from(PERIOD_PAIRS));
    let mut tried = 0u32;
    let mut first = read();
    let start = first.before;
    let mut prev = first;
    let mut stretches = Stretches::new(first);
    let mut figures = CountFigures::default();
    figures.saw(None, first);
    for _ in 1..MOST_READS {
        let next = read();
        figures.saw(Some(prev), next);
        if next.after.wrapping_sub(start) > budget {
            break;
        }
        if next.before.wrapping_sub(prev.after) > gap {
            // Away between two reads for long enough that a reload could have passed unseen.
            figures.gap = figures.gap.saturating_add(1);
            first = next;
            prev = next;
            stretches = Stretches::new(next);
            continue;
        }
        let reloaded = next.count > prev.count;
        if !reloaded {
            stretches.step(prev, next, stretch);
        }
        let last = if reloaded { prev } else { next };
        let span = last.twice_middle().wrapping_sub(first.twice_middle()) / 2;
        if reloaded || span >= target {
            let outcome = if span < least {
                Pair::Short
            } else {
                judge_pair(initial, nominal, first, last, &stretches, &mut figures)
            };
            match outcome {
                Pair::Decided(sample) => return (Some(sample), tried.saturating_add(1), figures),
                // Toward the reload, to narrow the bounds.
                Pair::Undecided if !reloaded && span < longest => {}
                given_up => {
                    tried = tried.saturating_add(1);
                    let count = match given_up {
                        Pair::Short => &mut figures.short,
                        Pair::Flat => &mut figures.flat,
                        Pair::Aside => &mut figures.aside,
                        Pair::Undecided | Pair::Decided(_) => &mut figures.wide,
                    };
                    *count = count.saturating_add(1);
                    first = next;
                    stretches = Stretches::new(next);
                }
            }
        }
        prev = next;
    }
    (None, tried, figures)
}

/// What became of a pair of count reads.
enum Pair {
    Decided(PeriodSample),
    /// Its bounds straddle a bound, or its stretches have not measured the slope.
    Undecided,
    /// It spanned under a twentieth of the period.
    Short,
    /// The count did not fall.
    Flat,
    /// Its slope misses its stretches': a reload may be hidden in it.
    Aside,
}

/// Judges the pair from `first` to `last` against its stretches (see [`sample_period`]).
fn judge_pair(
    initial: u64,
    nominal: u64,
    first: CountRead,
    last: CountRead,
    stretches: &Stretches,
    figures: &mut CountFigures,
) -> Pair {
    let (Some(sample), Some((pair_low, pair_high))) = (
        PeriodSample::from_reads(initial, first, last),
        Slope::between(first, last),
    ) else {
        return Pair::Flat;
    };
    figures.closed(&sample);
    let (low, high) = stretches.met_to(last);
    if !(low.at_most(pair_high) && pair_low.at_most(high)) {
        return Pair::Aside;
    }
    if !high.at_most(pair_low.doubled()) {
        return Pair::Undecided;
    }
    match sample.verdict(nominal) {
        RateVerdict::Undecided => Pair::Undecided,
        _ => Pair::Decided(sample),
    }
}

/// A slope of the timer's count against the TSC: `counts` over `ticks`, with no `ticks` for
/// one without bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Slope {
    counts: u64,
    ticks: u64,
}

impl Slope {
    const NONE: Self = Self {
        counts: 0,
        ticks: 1,
    };
    const UNBOUNDED: Self = Self {
        counts: 1,
        ticks: 0,
    };

    /// The count's slope between reads `a` and `b`, `b` the later, with no reload between: at
    /// least its fall less one count over the longest time the reads can be apart, at most its
    /// fall and one count over the shortest; `None` if the count rose.
    fn between(a: CountRead, b: CountRead) -> Option<(Self, Self)> {
        let fall = a.count.checked_sub(b.count)?;
        let longest = b.after.saturating_sub(a.before).max(1);
        let shortest = b.before.saturating_sub(a.after);
        let low = Self {
            counts: fall.saturating_sub(1),
            ticks: longest,
        };
        let high = if shortest == 0 {
            Self::UNBOUNDED
        } else {
            Self {
                counts: fall.saturating_add(1),
                ticks: shortest,
            }
        };
        Some((low, high))
    }

    /// Whether this slope is at most `other`.
    fn at_most(self, other: Self) -> bool {
        u128::from(self.counts) * u128::from(other.ticks)
            <= u128::from(other.counts) * u128::from(self.ticks)
    }

    const fn doubled(self) -> Self {
        Self {
            counts: self.counts.saturating_mul(2),
            ticks: self.ticks,
        }
    }

    /// The tighter of two lower bounds.
    fn higher(self, other: Self) -> Self {
        if self.at_most(other) { other } else { self }
    }

    /// The tighter of two upper bounds.
    fn lower(self, other: Self) -> Self {
        if self.at_most(other) { self } else { other }
    }
}

/// What a pair's stretches say of the count's slope (see [`sample_period`]).
#[derive(Clone, Copy, Debug)]
struct Stretches {
    /// The open stretch's first read.
    start: CountRead,
    /// The closed stretches' bounds met: the highest lower bound and the lowest upper one.
    low: Slope,
    high: Slope,
}

impl Stretches {
    const fn new(at: CountRead) -> Self {
        Self {
            start: at,
            low: Slope::NONE,
            high: Slope::UNBOUNDED,
        }
    }

    /// Takes in a step from `prev` to `next`, with no reload between: one over `limit` TSC
    /// ticks closes the open stretch at `prev` and opens another at `next`.
    fn step(&mut self, prev: CountRead, next: CountRead, limit: u64) {
        if next.after.saturating_sub(prev.before) > limit {
            let (low, high) = self.met_to(prev);
            *self = Self {
                start: next,
                low,
                high,
            };
        }
    }

    /// The bounds with the open stretch closed at `end`.
    fn met_to(&self, end: CountRead) -> (Slope, Slope) {
        match Slope::between(self.start, end) {
            Some((low, high)) if self.start != end => (self.low.higher(low), self.high.lower(high)),
            _ => (self.low, self.high),
        }
    }
}

/// What a run's reads of the timer's count showed (TASKS item 78), for a line of its own on
/// every run: they judge nothing. Under KVM on one runner's processor (a Xeon 8370C, CI run
/// 44837410914) no pair was clean in 64 tries while the interrupts came on time, and the
/// proof's lines could not say why; these figures can.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CountFigures {
    /// The narrowest read's bracket, in TSC ticks: what a read costs at the least.
    pub least_bracket: Option<u64>,
    /// The longest step from one read's bracket's start to the next one's end, in TSC ticks: a
    /// stall inside a bracket counts here, and not in the gap that [`sample_period`] judges.
    pub longest_step: u64,
    /// Steps broken by a gap between two reads over a hundredth of the period: each starts the
    /// pair again, and is no pair itself.
    pub gap: u32,
    /// Pairs ended by a rise of the count (a reload, or a count that jumps back) before they
    /// spanned a twentieth of the period.
    pub short: u32,
    /// Pairs given up undecided, for one of two reasons: their period's bounds straddled 97 % or
    /// 103 % (end brackets too wide, or the timer too near a bound; [`PeriodSample::verdict`]),
    /// or their stretches never measured the slope, every step being over a twentieth of the
    /// period. The line's longest step tells the two apart: over a twentieth, it is the second.
    pub wide: u32,
    /// Pairs over which the count did not fall.
    pub flat: u32,
    /// Pairs set aside, their slope not their stretches' (a reload may be hidden in them).
    pub aside: u32,
    /// The least uncertainty of any pair that spanned enough and fell, in parts per million of
    /// its period ([`PeriodSample::uncertainty_ppm`]): set-aside and undecided pairs included,
    /// so where one decides, this may be under the deciding pair's own. It shows how narrow the
    /// reads' brackets were where they were narrowest.
    pub best_ppm: Option<u64>,
}

impl CountFigures {
    /// Takes in one read, and the read before it if there was one.
    fn saw(&mut self, prev: Option<CountRead>, read: CountRead) {
        let bracket = read.after.saturating_sub(read.before);
        self.least_bracket = Some(self.least_bracket.map_or(bracket, |b| b.min(bracket)));
        if let Some(prev) = prev {
            self.longest_step = self
                .longest_step
                .max(read.after.saturating_sub(prev.before));
        }
    }

    /// Takes in a pair that spanned enough to be measured.
    fn closed(&mut self, sample: &PeriodSample) {
        let ppm = sample.uncertainty_ppm();
        self.best_ppm = Some(self.best_ppm.map_or(ppm, |b| b.min(ppm)));
    }

    /// The line for run number `run`, with TSC ticks at `tsc_hz` per second. Every figure is
    /// capped, so the line stays under the console's 160 bytes at the widest it can print
    /// while `run` has one digit (the tick proof's runs are fewer, which `timer.rs` asserts). It
    /// names no failure and holds no address.
    #[must_use]
    pub const fn line(&self, run: u32, tsc_hz: u64) -> CountLine {
        CountLine {
            figures: *self,
            run,
            tsc_hz,
        }
    }
}

/// [`CountFigures::line`]'s line.
#[derive(Debug)]
pub struct CountLine {
    figures: CountFigures,
    run: u32,
    tsc_hz: u64,
}

/// The largest bracket printed in full, in nanoseconds: just under 1 ms.
const LINE_MAX_NS: u64 = 999_999;
/// The largest step, in microseconds: just under 100 ms.
const LINE_MAX_US: u64 = 99_999;
/// The largest uncertainty, in parts per million: just under 10 %.
const LINE_MAX_PPM: u64 = 99_999;
/// The largest count of pairs or steps printed in full: since budget B they are bounded by time,
/// not by [`PERIOD_PAIRS`].
const LINE_MAX_COUNT: u64 = 99;

/// A figure printed as itself up to `cap`, and as the cap with a `+` above it.
struct Capped(u64, u64);

impl core::fmt::Display for Capped {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.0 > self.1 {
            write!(f, "{}+", self.1)
        } else {
            write!(f, "{}", self.0)
        }
    }
}

impl core::fmt::Display for CountLine {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let per = |ticks: u64, unit: u64| {
            u64::try_from(u128::from(ticks) * u128::from(unit) / u128::from(self.tsc_hz.max(1)))
                .unwrap_or(u64::MAX)
        };
        let c = &self.figures;
        write!(f, "origin-kernel: APIC timer: run {}: ", self.run)?;
        match c.least_bracket {
            Some(b) => write!(
                f,
                "brackets from {} ns",
                Capped(per(b, 1_000_000_000), LINE_MAX_NS)
            )?,
            None => write!(f, "no reads")?,
        }
        let count = |n: u32| Capped(u64::from(n), LINE_MAX_COUNT);
        write!(
            f,
            ", steps to {} us; spoiled: {} gap, {} short, {} wide, {} flat, {} aside; ",
            Capped(per(c.longest_step, 1_000_000), LINE_MAX_US),
            count(c.gap),
            count(c.short),
            count(c.wide),
            count(c.flat),
            count(c.aside)
        )?;
        match c.best_ppm {
            Some(ppm) => write!(f, "best +/-{} ppm", Capped(ppm, LINE_MAX_PPM)),
            None => write!(f, "best none"),
        }
    }
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
    /// No pair of reads decided: nothing is known.
    Undecided,
}

/// Judges the timer's rate from the pair of reads of its count that decided, `None` if none did,
/// against the `nominal` period in TSC ticks ([`PeriodSample::verdict`]). The count falls at the
/// timer's own rate whatever its interrupts do, so neither their delivery nor a host's stalls
/// move this verdict.
#[must_use]
pub fn judge_period(sample: Option<PeriodSample>, nominal: u64) -> RateVerdict {
    sample.map_or(RateVerdict::Undecided, |s| s.verdict(nominal))
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
    /// The least period the reads allow, in TSC ticks: the shortest time they can be apart, from
    /// the first's bracket's end to the last's start, over a fall of one count more.
    pub period_low: u64,
    /// The most: the longest time, from the first's start to the last's end, over a fall of one
    /// count less; `u64::MAX` for a fall of one count. Each read's count is the whole count at
    /// some instant inside its bracket, so the fall is known to within one count.
    pub period_high: u64,
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
        let shortest = last.before.saturating_sub(first.after);
        let longest = last.after.saturating_sub(first.before);
        // Rounded down and up, so the bounds hold the period the reads allow.
        let period_low =
            u64::try_from(u128::from(initial) * u128::from(shortest) / (u128::from(fall) + 1))
                .unwrap_or(u64::MAX);
        let period_high = match fall - 1 {
            0 => u64::MAX,
            less => u64::try_from(
                (u128::from(initial) * u128::from(longest)).div_ceil(u128::from(less)),
            )
            .unwrap_or(u64::MAX),
        };
        Some(Self {
            interval,
            fall,
            spread,
            period,
            period_low,
            period_high,
        })
    }

    /// What its reads decide of the rate against the `nominal` period in TSC ticks (TASKS item
    /// 78): in bounds only when the whole span from [`Self::period_low`] to
    /// [`Self::period_high`] lies within [`RATE_LOW_PPM`] to [`RATE_HIGH_PPM`] of nominal; fast
    /// or slow only when all of it lies beyond a bound; otherwise undecided. A timer exactly at a
    /// bound is never decided, so it is measured again and never passes.
    pub fn verdict(&self, nominal: u64) -> RateVerdict {
        let at_least = |period: u64, ppm: u64| {
            u128::from(period) * 1_000_000 >= u128::from(nominal) * u128::from(ppm)
        };
        let at_most = |period: u64, ppm: u64| {
            u128::from(period) * 1_000_000 <= u128::from(nominal) * u128::from(ppm)
        };
        if at_least(self.period_low, RATE_LOW_PPM) && at_most(self.period_high, RATE_HIGH_PPM) {
            RateVerdict::InBounds
        } else if !at_least(self.period_high, RATE_LOW_PPM) {
            RateVerdict::Fast
        } else if !at_most(self.period_low, RATE_HIGH_PPM) {
            RateVerdict::Slow
        } else {
            RateVerdict::Undecided
        }
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

    /// A pair of count reads, 5 µs brackets, of a timer whose period is exactly `percent` % of
    /// nominal: 125 000 of its 625 000 counts fall over a fifth of that period. Its period's
    /// bounds reach about 0.25 % either side, so it decides from 98 % to 102 %.
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
    fn a_stall_inside_a_read_that_hides_a_reload_is_set_aside() {
        // The second addendum review's T66-R17 (TASKS item 78): a timer 10 times fast (a period
        // of 1 ms against 10 ms nominal), read from 111 µs before a reload; the read 61 µs on
        // stalls 1.01 ms inside its own bracket and samples a count below the one before, so the
        // reload falls between them unseen. The rule before TASKS item 78 took that pair near
        // nominal (1,009,090 ppm). Its slope is now a tenth of its stretches': it is set aside, and
        // the next pair measures the timer as it is. Each figure was derived first by an
        // independent model of the sampler (TASKS item 78's `c4.py`).
        let (initial, nominal) = (625_000, PERIOD);
        let fast = nominal / 10;
        let outcome = |period, start, stalled, stall| {
            let (sample, tried, figures) = sample_period(
                initial,
                nominal,
                simulated_reads(initial, period, start, stalled, stall),
            );
            (
                sample.map(|s| s.ppm_of(nominal)),
                judge_period(sample, nominal),
                tried,
                figures.aside,
            )
        };
        assert_eq!(
            outcome(fast, 3 * fast - 111_000, 61, 1_010_000),
            (Some(100_000), RateVerdict::Fast, 2, 1)
        );
        // A correct timer with the same stall passes, after a pair the reload cut short and one
        // its stretches had not yet measured when the next reload ended it; and both
        // undisturbed are measured as they are.
        assert_eq!(
            outcome(nominal, 3 * nominal - 111_000, 61, 1_010_000),
            (Some(1_000_000), RateVerdict::InBounds, 3, 0)
        );
        assert_eq!(
            outcome(nominal, 0, usize::MAX, 0),
            (Some(1_000_000), RateVerdict::InBounds, 1, 0)
        );
        assert_eq!(
            outcome(fast, 0, usize::MAX, 0),
            (Some(100_000), RateVerdict::Fast, 1, 0)
        );
    }

    /// Reads of a timer whose count at TSC `t` is `count(t)`, sampled at each bracket's middle:
    /// every bracket `bracket` ticks wide, the next opening `gap` ticks after it closes.
    fn reads_of(bracket: u64, gap: u64, count: impl Fn(u64) -> u64) -> impl FnMut() -> CountRead {
        let mut at = 0;
        move || {
            let before = at;
            let after = before + bracket;
            at = after + gap;
            CountRead {
                before,
                count: count(before + bracket / 2),
                after,
            }
        }
    }

    /// A correct timer of 625,000 counts at the tests' period, reloaded at TSC 0.
    fn correct(t: u64) -> u64 {
        625_000 - (t % PERIOD) * 625_000 / PERIOD
    }

    #[test]
    fn the_count_figures_say_why_no_pair_decided() {
        // TASKS item 78: the figures judge nothing; they say why a run's period was or was not
        // decided. Each case's numbers were derived first by an independent model of the
        // sampler (TASKS item 78's evidence, `c4.py`).
        let figures = |bracket, gap, count: fn(u64) -> u64| {
            let (sample, tried, figures) =
                sample_period(625_000, PERIOD, reads_of(bracket, gap, count));
            (sample.map(|s| s.ppm_of(PERIOD)), tried, figures)
        };
        let none = CountFigures::default();
        // Undisturbed: the first pair decides, its 800 ticks of brackets over 2 ms.
        assert_eq!(
            figures(400, 600, correct),
            (
                Some(1_000_000),
                1,
                CountFigures {
                    least_bracket: Some(400),
                    longest_step: 1_400,
                    best_ppm: Some(200),
                    ..none
                }
            )
        );
        // Every read 120 µs wide, as a slow read under a hypervisor would be: the first pair
        // goes on past a fifth of the period until its bounds lie within 97 % to 103 %.
        assert_eq!(
            figures(120_000, 600, correct),
            (
                Some(1_000_000),
                1,
                CountFigures {
                    least_bracket: Some(120_000),
                    longest_step: 240_600,
                    best_ppm: Some(29_265),
                    ..none
                }
            )
        );
        // 300 µs wide: every step is over a twentieth of the period, so no stretch measures the
        // slope, and not even 0.9 of a period would narrow the bounds enough; 24 pairs in the
        // run's 128 ms, each given up undecided.
        assert_eq!(
            figures(300_000, 600, correct),
            (
                None,
                24,
                CountFigures {
                    least_bracket: Some(300_000),
                    longest_step: 600_600,
                    wide: 24,
                    best_ppm: Some(33_266),
                    ..none
                }
            )
        );
        // A gap over a hundredth of the period between every two reads: 1,274 broken steps in
        // 128 ms, and no pair.
        assert_eq!(
            figures(400, PERIOD / 100 + 1, correct),
            (
                None,
                0,
                CountFigures {
                    least_bracket: Some(400),
                    longest_step: 100_801,
                    gap: 1_274,
                    ..none
                }
            )
        );
        // A count that does not move.
        assert_eq!(
            figures(400, 600, |_| 5_000),
            (
                None,
                63,
                CountFigures {
                    least_bracket: Some(400),
                    longest_step: 1_400,
                    flat: 63,
                    ..none
                }
            )
        );
        // A count that jumps back by 1,000 every 200 µs: each pair ends at the jump, short.
        assert_eq!(
            figures(400, 600, |t| correct(t) + (t / 100_000) % 2 * 1_000),
            (
                None,
                652,
                CountFigures {
                    least_bracket: Some(400),
                    longest_step: 1_400,
                    short: 652,
                    ..none
                }
            )
        );
    }

    #[test]
    fn the_count_figures_line_is_pinned_and_short_and_names_no_failure() {
        let undisturbed = CountFigures {
            least_bracket: Some(400),
            longest_step: 1_400,
            best_ppm: Some(200),
            ..CountFigures::default()
        };
        assert_eq!(
            alloc::format!("{}", undisturbed.line(1, 1_000_000_000)),
            "origin-kernel: APIC timer: run 1: brackets from 400 ns, steps to 1 us; spoiled: \
             0 gap, 0 short, 0 wide, 0 flat, 0 aside; best +/-200 ppm"
        );
        let undecided = CountFigures {
            least_bracket: Some(840_000),
            longest_step: 1_681_680,
            wide: 24,
            aside: 2,
            best_ppm: Some(33_266),
            ..CountFigures::default()
        };
        assert_eq!(
            alloc::format!("{}", undecided.line(4, 2_800_000_000)),
            "origin-kernel: APIC timer: run 4: brackets from 300000 ns, steps to 600 us; spoiled: \
             0 gap, 0 short, 24 wide, 0 flat, 2 aside; best +/-33266 ppm"
        );
        // Every figure at its widest: each is capped, and a run's number has one digit (`timer.rs`
        // asserts it), so the line stays under the console's 160 bytes (the review's T66-R7).
        let widest = CountFigures {
            least_bracket: Some(u64::MAX),
            longest_step: u64::MAX,
            gap: u32::MAX,
            short: u32::MAX,
            wide: u32::MAX,
            flat: u32::MAX,
            aside: u32::MAX,
            best_ppm: Some(u64::MAX),
        };
        assert_eq!(
            alloc::format!("{}", widest.line(9, 1)),
            "origin-kernel: APIC timer: run 9: brackets from 999999+ ns, steps to 99999+ us; \
             spoiled: 99+ gap, 99+ short, 99+ wide, 99+ flat, 99+ aside; best +/-99999+ ppm"
        );
        let nothing = CountFigures::default();
        for figures in [undisturbed, undecided, widest, nothing] {
            for tsc_hz in [0, 1, 1_000_000_000, u64::MAX] {
                for run in [1, 4, 9] {
                    let line = alloc::format!("{}", figures.line(run, tsc_hz));
                    assert!(line.len() < 160, "{} bytes: {line}", line.len());
                    // ADR-0013 R8: no failure's words, no address (TASKS item 66's invariant 4).
                    for word in ["fail", "FAIL", "WRONG", "wrong", "refused", "panic", "0x"] {
                        assert!(!line.contains(word), "{word:?} in {line}");
                    }
                }
            }
        }
        assert_eq!(
            alloc::format!("{}", nothing.line(1, 1)),
            "origin-kernel: APIC timer: run 1: no reads, steps to 0 us; spoiled: 0 gap, 0 short, \
             0 wide, 0 flat, 0 aside; best none"
        );
    }

    /// Reads of a timer `fast_x100` / 100 times fast, reloaded at TSC 0: the first at once; the
    /// second opening right after it and sampling its count at once, its bracket then held open
    /// to `held_to`; then reads every 1.2 µs with 200-tick brackets, the first `gap` ticks after.
    fn held_open(fast_x100: u64, held_to: u64, gap: u64) -> impl FnMut() -> CountRead {
        let period = PERIOD * 100 / fast_x100;
        let count = move |t: u64| 625_000 - 625_000 * (t % period) / period;
        let mut n = 0;
        let mut at = held_to + gap;
        move || {
            n += 1;
            match n {
                1 => CountRead {
                    before: 0,
                    count: count(100),
                    after: 200,
                },
                2 => CountRead {
                    before: 250,
                    count: count(260),
                    after: held_to,
                },
                _ => {
                    let before = at;
                    at += 1_200;
                    CountRead {
                        before,
                        count: count(before + 100),
                        after: before + 200,
                    }
                }
            }
        }
    }

    #[test]
    fn a_held_read_no_longer_hides_a_reload() {
        // The rule before TASKS item 78 passed these two, near 103 %: a read held open to about
        // 0.4 of a period, its count sampled at its start, hid one reload of a timer 3.42 times
        // fast (with the gap allowed after it) and 3.5 times (without). Now the pair across the
        // held read is set aside and the next measures the timer as it is (figures from the
        // independent model, TASKS item 78's `c4.py`).
        let outcome = |fast_x100, held_to, gap| {
            let (sample, tried, figures) =
                sample_period(625_000, PERIOD, held_open(fast_x100, held_to, gap));
            (
                sample.map(|s| s.ppm_of(PERIOD)),
                judge_period(sample, PERIOD),
                tried,
                figures.aside,
            )
        };
        assert_eq!(
            outcome(342, 3_984_000, PERIOD / 100),
            (Some(292_397), RateVerdict::Fast, 2, 1)
        );
        assert_eq!(
            outcome(350, 3_954_000, 50),
            (Some(285_714), RateVerdict::Fast, 2, 1)
        );
        // A correct timer read the same way passes (TASKS item 78's invariant I2): its reload
        // needs a whole period between two reads.
        for held_to in (3_000_000..4_200_000).step_by(1_000) {
            let (sample, _, _) = sample_period(625_000, PERIOD, held_open(100, held_to, 50));
            assert_eq!(
                judge_period(sample, PERIOD),
                RateVerdict::InBounds,
                "held to {held_to}"
            );
        }
    }

    #[test]
    fn no_hidden_reload_passes_below_twenty_times_fast() {
        // TASKS item 78's sweep (the coordinating session's ruling on the round): 2.2 to 19 times
        // fast, one read held open to each of 351 places from 0.05 to 1.8 periods, the gap the
        // rule allows after it. None is ever in bounds. From 1.9 times, holds aimed at a reading
        // in bounds are the next test's.
        let mut checked = 0;
        for fast_x100 in [220, 250, 342, 500, 1_000, 1_500, 1_900] {
            let mut held_to = PERIOD / 20;
            while held_to <= PERIOD * 18 / 10 {
                let (sample, _, _) =
                    sample_period(625_000, PERIOD, held_open(fast_x100, held_to, PERIOD / 100));
                assert_ne!(
                    judge_period(sample, PERIOD),
                    RateVerdict::InBounds,
                    "{fast_x100} held to {held_to}"
                );
                checked += 1;
                held_to += PERIOD / 200;
            }
        }
        assert_eq!(checked, 7 * 351);
    }

    #[test]
    fn no_hidden_reload_passes_from_under_twice_as_fast() {
        // The independent review's T78-R6 (its search R-B): 1.9 to 19.99 times fast, with 1, 2 or
        // 5 reloads hidden by one held read. The hold is timed so that the pair from the first
        // read to the one after the held read reads 97.05 %, 100 % or 102.95 % of nominal: a
        // phase past the hidden reloads with (hidden + phase) / (fast × phase) = the reading.
        // 0 or 20 reads in a stretch come before it and 20 after, then one every 50 µs, every
        // count taken at its bracket's start or at its end. None is ever in bounds, though in
        // every case the pair's own reading is: the cases have teeth.
        const N: u64 = 625_000;
        let (mut cases, mut reading_in_bounds) = (0u32, 0u32);
        for fast_x100 in (190..2_000).step_by(13) {
            let period = PERIOD * 100 / fast_x100;
            for hidden in [1, 2, 5] {
                for reading_ppm in [970_500, 1_000_000, 1_029_500] {
                    // reading × fast, scaled by 10^8; the phase is under one period only if it
                    // exceeds hidden + 1.
                    let scaled = reading_ppm * fast_x100;
                    if scaled <= 100_000_000 * (hidden + 1) {
                        continue;
                    }
                    let past =
                        hidden * period + period * hidden * 100_000_000 / (scaled - 100_000_000);
                    for pre in [0, 20] {
                        for at_end in [false, true] {
                            // The timer reloads as the first read's count is taken.
                            let first = if at_end { 200 } else { 0 };
                            let count = move |t: u64| N - ((t - first) % period) * N / period;
                            let t_last = first + past;
                            let held_from = 800 * (pre + 1);
                            let last_from = t_last - if at_end { 200 } else { 0 };
                            let mut n = 0u64;
                            let read = move || {
                                n += 1;
                                let (before, after) = if n <= pre + 1 {
                                    (800 * (n - 1), 800 * (n - 1) + 200)
                                } else if n == pre + 2 {
                                    (held_from, last_from - 600)
                                } else {
                                    let k = n - pre - 3;
                                    let before = if k <= 20 {
                                        last_from + 800 * k
                                    } else {
                                        last_from + 16_000 + 50_000 * (k - 20)
                                    };
                                    (before, before + 200)
                                };
                                CountRead {
                                    before,
                                    count: count(if at_end { after } else { before }),
                                    after,
                                }
                            };
                            let fall = count(first) - count(t_last);
                            let own = u128::from(t_last - first) * u128::from(N) * 1_000_000
                                / (u128::from(PERIOD) * u128::from(fall));
                            reading_in_bounds += u32::from((970_000..=1_030_000).contains(&own));
                            let (sample, _, _) = sample_period(N, PERIOD, read);
                            assert_ne!(
                                judge_period(sample, PERIOD),
                                RateVerdict::InBounds,
                                "{fast_x100} {hidden} {reading_ppm} {pre} {at_end}"
                            );
                            cases += 1;
                        }
                    }
                }
            }
        }
        assert_eq!((cases, reading_in_bounds), (4_532, 4_532));
    }

    #[test]
    fn wide_reads_decide_and_a_rate_six_percent_out_never_passes() {
        // Reads 60, 120 and 200 µs wide decide in bounds (the diagnostic round's (p1)); a timer
        // 6 % fast or slow read as widely is never in bounds; 300 µs decides nothing.
        let verdict = |bracket, period: u64, phase: u64| {
            let count = move |t: u64| 625_000 - ((t + phase) % period) * 625_000 / period;
            let (sample, _, _) = sample_period(625_000, PERIOD, reads_of(bracket, 600, count));
            judge_period(sample, PERIOD)
        };
        for bracket in [60_000, 120_000, 200_000] {
            assert_eq!(
                verdict(bracket, PERIOD, 0),
                RateVerdict::InBounds,
                "{bracket}"
            );
            for period in [PERIOD * 100 / 106, PERIOD * 106 / 100] {
                // At eight phases of the reload (the review's T78-R11).
                for eighth in 0..8 {
                    assert_ne!(
                        verdict(bracket, period, period * eighth / 8),
                        RateVerdict::InBounds,
                        "{bracket} {period} {eighth}"
                    );
                }
            }
        }
        assert_eq!(verdict(300_000, PERIOD, 0), RateVerdict::Undecided);
    }

    #[test]
    fn a_count_that_pauses_at_zero_can_be_judged_slow() {
        // A stated limit, not a guarantee (the independent review's T78-R7; the coordinating
        // session's ruling: no guard now, TASKS item 115). A correct timer whose count stays at 0
        // for a while after each expiry before it reloads is outside I2's model, a count that
        // falls linearly. A pair across the pause sees too little fall for its time, and at some
        // phases it is judged slow, which fails the run at once; never fast, since a pause only
        // slows the count. Over 100 phases of the cycle, as an independent model gave them:
        // (bracket, pause, phases judged slow); every other phase passes.
        for (bracket, pause, slow) in [
            (5_000, 60_000, 12),
            (5_000, 1_000_000, 17),
            (120_000, 1_000_000, 37),
        ] {
            let cycle = PERIOD + pause;
            let (mut in_bounds, mut judged_slow, mut other) = (0u32, 0u32, 0u32);
            for i in 0..100 {
                let phase = cycle * i / 100;
                let count = move |t: u64| {
                    let into = (t + phase) % cycle;
                    if into < PERIOD {
                        625_000 - into * 625_000 / PERIOD
                    } else {
                        0
                    }
                };
                let (sample, _, _) = sample_period(625_000, PERIOD, reads_of(bracket, 600, count));
                match judge_period(sample, PERIOD) {
                    RateVerdict::InBounds => in_bounds += 1,
                    RateVerdict::Slow => judged_slow += 1,
                    RateVerdict::Fast | RateVerdict::Undecided => other += 1,
                }
            }
            assert_eq!(
                (in_bounds, judged_slow, other),
                (100 - slow, slow, 0),
                "{bracket} {pause}"
            );
        }
    }

    /// Reads of a correct timer, each bracket `bracket` ticks wide and the next `gap` after it;
    /// every `every`th read stalls `stall` ticks inside its bracket, its count sampled in the
    /// middle of the stall.
    fn stalled_reads(bracket: u64, every: u64, stall: u64) -> impl FnMut() -> CountRead {
        let mut at = 0;
        let mut n = 0u64;
        move || {
            n += 1;
            let extra = if n.is_multiple_of(every) { stall } else { 0 };
            let before = at;
            let after = before + bracket + extra;
            at = after + 600;
            CountRead {
                before,
                count: correct(before + (bracket + extra) / 2),
                after,
            }
        }
    }

    #[test]
    fn no_correct_timer_is_judged_wrong_under_stalls() {
        // TASKS item 78's invariant I2: stalls of 10 µs to 1.2 periods inside every 3rd to
        // 333rd read, with brackets up to 120 µs: a correct timer is in bounds or undecided,
        // never fast or slow.
        for every in [3, 7, 50, 333] {
            for stall in [
                10_000, 150_000, 1_000_000, 3_000_000, 6_000_000, 9_000_000, 12_000_000,
            ] {
                for bracket in [400, 5_000, 40_000, 120_000] {
                    let (sample, _, _) =
                        sample_period(625_000, PERIOD, stalled_reads(bracket, every, stall));
                    let verdict = judge_period(sample, PERIOD);
                    assert!(
                        matches!(verdict, RateVerdict::InBounds | RateVerdict::Undecided),
                        "every {every}, stall {stall}, bracket {bracket}: {verdict:?}"
                    );
                }
            }
        }
    }

    /// A small xorshift generator for the randomized model: no crate (CLAUDE.md §5).
    struct Random(u64);

    impl Random {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        /// Uniform in `low..=high`.
        fn within(&mut self, low: u64, high: u64) -> u64 {
            low + self.next() % (high - low + 1)
        }
    }

    #[test]
    fn a_randomized_model_never_judges_a_rate_wrong() {
        // TASKS item 78's randomized model (the coordinating session's ruling on the round: it
        // stands in below 2.2 times fast, where the sweep stops). Timers from just under 20 times
        // fast (50,100 ppm: at exactly 20 times a stretch could hide a reload, the review's
        // T78-R11) to 2.5 times slow, reloaded at a random phase, read with brackets of 0 to 150 µs and gaps
        // of up to a hundredth of the period; one read in 2 to 40 held open from 0 to 1.8
        // periods, its count sampled at a random instant inside its bracket. The verdict is
        // never wrong: in bounds only between 97 % and 103 %, fast only below, slow only above.
        let mut random = Random(0x7478_0017_0000_0078);
        let (mut cases, mut decided, mut passed) = (0u32, 0u32, 0u32);
        for _ in 0..1_500 {
            // The true period in parts per million of nominal: a third of the cases fast, near
            // or below twice the rate, a third close to the bounds, a third anywhere.
            let ppm = match random.within(0, 2) {
                0 => random.within(50_100, 520_000),
                1 => random.within(940_000, 1_060_000),
                _ => random.within(50_100, 2_500_000),
            };
            let period = PERIOD * ppm / 1_000_000;
            let phase = random.within(0, period);
            let bracket = random.within(0, 150_000);
            let gap = random.within(0, PERIOD / 100);
            let every = random.within(2, 40);
            let mut at = 0u64;
            let mut n = 0u64;
            let mut seed = Random(random.next() | 1);
            let read = move || {
                n += 1;
                let width = if n.is_multiple_of(every) {
                    bracket + seed.within(0, PERIOD * 18 / 10)
                } else {
                    bracket
                };
                let before = at;
                let sampled = before + seed.within(0, width);
                let after = before + width;
                at = after + gap;
                let into = (sampled + phase) % period;
                CountRead {
                    before,
                    count: 625_000 - into * 625_000 / period,
                    after,
                }
            };
            let (sample, _, _) = sample_period(625_000, PERIOD, read);
            let verdict = judge_period(sample, PERIOD);
            match verdict {
                RateVerdict::InBounds => {
                    assert!(
                        (RATE_LOW_PPM..=RATE_HIGH_PPM).contains(&ppm),
                        "{ppm} ppm judged in bounds"
                    );
                    passed += 1;
                }
                RateVerdict::Fast => assert!(ppm < RATE_LOW_PPM, "{ppm} ppm judged fast"),
                RateVerdict::Slow => assert!(ppm > RATE_HIGH_PPM, "{ppm} ppm judged slow"),
                RateVerdict::Undecided => {}
            }
            cases += 1;
            decided += u32::from(verdict != RateVerdict::Undecided);
        }
        // Not vacuous: most cases decide, and some in bounds pass.
        assert_eq!(cases, 1_500);
        assert!(decided > 1_000, "{decided} of {cases} decided");
        assert!(passed > 100, "{passed} of {cases} in bounds");
    }

    #[test]
    fn the_count_judges_the_rate_at_its_edges() {
        assert_eq!(judge_period(sample_at(100), PERIOD), RateVerdict::InBounds);
        assert_eq!(judge_period(sample_at(98), PERIOD), RateVerdict::InBounds);
        assert_eq!(judge_period(sample_at(102), PERIOD), RateVerdict::InBounds);
        // At a bound the period's bounds straddle it: undecided, so measured again and never
        // passed (TASKS item 78).
        assert_eq!(judge_period(sample_at(97), PERIOD), RateVerdict::Undecided);
        assert_eq!(judge_period(sample_at(103), PERIOD), RateVerdict::Undecided);
        assert_eq!(judge_period(sample_at(96), PERIOD), RateVerdict::Fast);
        assert_eq!(judge_period(sample_at(104), PERIOD), RateVerdict::Slow);
        // The exact edges, with brackets of no width, so only the fall's one count is uncertain:
        // the least interval whose bounds lie within 97 %, and the most within 103 %, and one
        // tick beyond each (derived apart, with exact fractions: TASKS item 78's `figures_c2.py`).
        let pair = |interval| {
            PeriodSample::from_reads(625_000, read(0, 600_000, 0), read(interval, 475_000, 0))
                .unwrap()
                .verdict(PERIOD)
        };
        assert_eq!(pair(1_940_015), RateVerdict::Undecided);
        assert_eq!(pair(1_940_016), RateVerdict::InBounds);
        assert_eq!(pair(2_059_983), RateVerdict::InBounds);
        assert_eq!(pair(2_059_984), RateVerdict::Undecided);
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
        // No pair that decides: undecided, which is measured again and never passes, whatever
        // the interrupts did.
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
        // Its period's bounds: 1,998,000 ticks at the least over 125,001 counts, 2,002,000 at the
        // most over 124,999.
        assert_eq!((s.period_low, s.period_high), (9_989_920, 10_010_081));
        assert_eq!(s.verdict(10_000_000), RateVerdict::InBounds);
        // Twice the rate: the same fall in half the time.
        let fast = PeriodSample::from_reads(
            625_000,
            read(0, 600_000, 1_000),
            read(1_000_000, 475_000, 1_000),
        )
        .unwrap();
        assert_eq!(fast.ppm_of(10_000_000), 500_000);
        // A bracket a stall widened widens the period's bounds: at 30 µs they still lie within
        // 97 % to 103 %; at 120 µs they reach below 97 %, and the pair decides nothing.
        let wide = |width| {
            PeriodSample::from_reads(
                625_000,
                read(0, 600_000, width),
                read(2_000_000, 475_000, 1_000),
            )
            .unwrap()
            .verdict(10_000_000)
        };
        assert_eq!(wide(30_000), RateVerdict::InBounds);
        assert_eq!(wide(120_000), RateVerdict::Undecided);
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
        // places, its count read as that rate: InBounds only with the count's period decided
        // within 97 % to 103 % and the whole run within 95 % to 150 %; Stalled only with the
        // count's period in bounds and a gap of two periods, or undecided, at a bound; a period
        // out of bounds fails, whatever the stalls.
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
                    // `sample_at`'s bounds straddle 97 % and 103 % themselves.
                    let at_bound = percent == 97 || percent == 103;
                    assert_eq!(rate == RateVerdict::Undecided, at_bound, "{percent}%");
                    match verdict {
                        TickVerdict::InBounds => assert!(
                            rate_in
                                && !at_bound
                                && elapsed >= nominal / 100 * 95
                                && elapsed <= nominal / 2 * 3,
                            "{percent}% +{extra} at {at}: {run:?}"
                        ),
                        TickVerdict::Stalled => assert!(
                            at_bound || (rate_in && run.longest_gap >= 2 * PERIOD),
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
