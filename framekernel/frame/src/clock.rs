//! Clock arithmetic (CLAUDE.md §68: monotonic clock, high-resolution timers, timer
//! calibration): counter ticks to nanoseconds and back, and the TSC rate CPUID reports.
//!
//! A read of the monotonic clock must be cheap and must never go backwards. [`Scale`]
//! converts ticks with one 64×64→128-bit multiply and a shift, with no division. The
//! result is the exact `ticks × 10⁹ / hz` rounded down or up (so exact whenever that is a
//! whole number of nanoseconds), and it never decreases as `ticks` grows. The kernel binary (`src/clock.rs`) holds the clock
//! state and the hardware access.
#![forbid(unsafe_code)]

use core::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};

/// Nanoseconds per second.
pub const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// A fixed-point conversion from a counter running at `hz` to nanoseconds:
/// `nanos = ticks × mult >> shift`.
///
/// `shift` is the largest value (at most 64) for which `mult = ceil(10⁹ × 2^shift / hz)`
/// still fits in 64 bits, so `mult` keeps at least 63 significant bits whenever
/// `shift < 64`. Rounding `mult` up makes the product exceed the exact value by less than
/// `ticks / 2^shift`, so for any `ticks` below `2^shift` (centuries at any realistic rate)
/// the result lies between the exact value rounded down and rounded up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scale {
    hz: u64,
    mult: u64,
    shift: u32,
}

impl Scale {
    /// The scale for a counter at `hz` ticks per second, or `None` if `hz` is zero.
    pub fn new(hz: u64) -> Option<Self> {
        if hz == 0 {
            return None;
        }
        // At shift 0, mult = ceil(10⁹ / hz) ≤ 10⁹ for every hz ≥ 1, so the search ends.
        let mut shift = 64u32;
        loop {
            let mult = (u128::from(NANOS_PER_SECOND) << shift).div_ceil(u128::from(hz));
            if let Ok(mult) = u64::try_from(mult) {
                return Some(Self { hz, mult, shift });
            }
            shift = shift.checked_sub(1)?;
        }
    }

    /// The counter rate this scale converts from.
    pub const fn hz(&self) -> u64 {
        self.hz
    }

    /// `ticks` in nanoseconds: the exact value rounded down or up (see the type's
    /// documentation), saturating at `u64::MAX` (584 years).
    pub fn nanos(&self, ticks: u64) -> u64 {
        let product = u128::from(ticks) * u128::from(self.mult);
        u64::try_from(product >> self.shift).unwrap_or(u64::MAX)
    }

    /// The fewest ticks that last at least `nanos` nanoseconds (rounded up), saturating at
    /// `u64::MAX`. This is the deadline direction, which may divide: deadlines are set far
    /// less often than the clock is read.
    pub fn ticks_for(&self, nanos: u64) -> u64 {
        let wanted = u128::from(nanos) * u128::from(self.hz);
        u64::try_from(wanted.div_ceil(u128::from(NANOS_PER_SECOND))).unwrap_or(u64::MAX)
    }
}

/// A [`Scale`] that one processor publishes once and every processor then reads without
/// a lock (the clock's read path).
#[derive(Debug)]
pub struct SharedScale {
    /// 0: empty; 1: being written; 2: published.
    state: AtomicU8,
    hz: AtomicU64,
    mult: AtomicU64,
    shift: AtomicU32,
}

impl SharedScale {
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(0),
            hz: AtomicU64::new(0),
            mult: AtomicU64::new(0),
            shift: AtomicU32::new(0),
        }
    }

    /// Publishes `scale`. Only the first call takes effect; returns whether this one did.
    pub fn publish(&self, scale: Scale) -> bool {
        if self
            .state
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return false;
        }
        self.hz.store(scale.hz, Ordering::Relaxed);
        self.mult.store(scale.mult, Ordering::Relaxed);
        self.shift.store(scale.shift, Ordering::Relaxed);
        // Release: a reader that sees state 2 sees the three fields.
        self.state.store(2, Ordering::Release);
        true
    }

    /// The published scale, or `None` before [`SharedScale::publish`] completes.
    pub fn get(&self) -> Option<Scale> {
        (self.state.load(Ordering::Acquire) == 2).then(|| Scale {
            hz: self.hz.load(Ordering::Relaxed),
            mult: self.mult.load(Ordering::Relaxed),
            shift: self.shift.load(Ordering::Relaxed),
        })
    }
}

impl Default for SharedScale {
    fn default() -> Self {
        Self::new()
    }
}

/// The TSC rate that CPUID leaf 15h reports (Intel SDM Vol. 2A, CPUID, "Time Stamp
/// Counter and Nominal Core Crystal Clock Information Leaf"): `crystal × EBX / EAX`.
/// Returns `None` when a field is zero (the ratio or the crystal frequency is not
/// enumerated), so the caller must measure the rate instead.
pub fn cpuid_tsc_hz(eax: u32, ebx: u32, ecx: u32) -> Option<u64> {
    if eax == 0 || ebx == 0 || ecx == 0 {
        return None;
    }
    let hz = u64::from(ecx).checked_mul(u64::from(ebx))? / u64::from(eax);
    (hz != 0).then_some(hz)
}

/// A reading of a counter paired with a time stamp from around it: `read` returns the
/// time stamp before the read, the value, and the time stamp after. Reads are retried, at
/// most `attempts` times, until one is bracketed within `tight` ticks; otherwise the
/// narrowest bracket is used. Returns the value and the middle of its bracket, so that a
/// stall between reading a counter and taking the time cannot make two clocks seem to
/// disagree.
pub fn closest_read(
    attempts: u32,
    tight: u64,
    mut read: impl FnMut() -> (u64, u64, u64),
) -> (u64, u64) {
    let mut best: Option<(u64, u64, u64)> = None;
    for _ in 0..attempts.max(1) {
        let (before, value, after) = read();
        let width = after.saturating_sub(before);
        if best.is_none_or(|(narrowest, ..)| width < narrowest) {
            best = Some((width, value, before.saturating_add(width / 2)));
        }
        if width <= tight {
            break;
        }
    }
    best.map_or((0, 0), |(_, value, at)| (value, at))
}

/// Whether two independent rate estimates agree within `parts_per_thousand`.
pub fn rates_agree(a: u64, b: u64, parts_per_thousand: u64) -> bool {
    let (lo, hi) = (a.min(b), a.max(b));
    let diff = u128::from(hi - lo) * 1000;
    diff <= u128::from(lo) * u128::from(parts_per_thousand)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact value rounded down, from 128-bit arithmetic.
    fn exact(ticks: u64, hz: u64) -> u128 {
        u128::from(ticks) * u128::from(NANOS_PER_SECOND) / u128::from(hz)
    }

    /// The exact value rounded up.
    fn exact_up(ticks: u64, hz: u64) -> u128 {
        (u128::from(ticks) * u128::from(NANOS_PER_SECOND)).div_ceil(u128::from(hz))
    }

    const RATES: [u64; 9] = [
        1,
        24_000_000, // the development host's counter, as QEMU's TCG TSC on Apple silicon
        999_999_999,
        1_000_000_000,
        1_000_000_001,
        2_100_000_000,
        3_187_654_321,
        5_800_000_000,
        u64::MAX,
    ];

    /// Any CPUID leaf 15h words: a rate is produced only from three nonzero fields and is
    /// exactly `crystal × numerator / denominator` (CLAUDE.md §72).
    #[test]
    fn any_cpuid_tsc_leaf_is_total() {
        use crate::testing::Rng;
        let mut rng = Rng::new(0x5453_4331);
        for case in 0..crate::testing::cases(20_000) {
            let (eax, ebx, ecx) = (rng.edge_u32(), rng.edge_u32(), rng.edge_u32());
            let got = cpuid_tsc_hz(eax, ebx, ecx);
            if eax == 0 || ebx == 0 || ecx == 0 {
                assert_eq!(got, None, "case {case}");
            } else {
                let exact = u128::from(ecx) * u128::from(ebx) / u128::from(eax);
                assert_eq!(
                    got.map(u128::from),
                    (exact != 0).then_some(exact),
                    "case {case}"
                );
            }
            let (a, b) = (rng.edge_u64(), rng.edge_u64());
            let ppt = rng.edge_u64();
            assert_eq!(
                rates_agree(a, b, ppt),
                rates_agree(b, a, ppt),
                "case {case}"
            );
            if let Some(s) = Scale::new(rng.edge_u64()) {
                assert_ne!(s.hz(), 0, "case {case}");
                let t = rng.edge_u64();
                let _ = (s.nanos(t), s.ticks_for(t));
            }
        }
    }

    #[test]
    fn nanos_is_the_exact_value_rounded_either_way_and_never_decreases() {
        // A deterministic spread of tick counts: small values, powers of two and their
        // neighbours, and a multiplicative walk.
        let mut ticks: Vec<u64> = (0..2000).collect();
        for b in 0..64 {
            let p = 1u64 << b;
            ticks.extend([p - 1, p, p + 1]);
        }
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..5000 {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            ticks.push(x >> (x % 40));
        }
        for hz in RATES {
            let s = Scale::new(hz).unwrap();
            let limit = 1u128 << s.shift;
            for &t in &ticks {
                let got = u128::from(s.nanos(t));
                let (down, up) = (exact(t, hz), exact_up(t, hz));
                if u128::from(t) < limit && up < u128::from(u64::MAX) {
                    assert!(
                        (down..=up).contains(&got),
                        "hz {hz} ticks {t}: {got} vs {down}..={up}"
                    );
                }
            }
            let mut sorted = ticks.clone();
            sorted.sort_unstable();
            let values: Vec<u64> = sorted.iter().map(|&t| s.nanos(t)).collect();
            assert!(values.windows(2).all(|w| w[0] <= w[1]), "hz {hz}");
        }
    }

    #[test]
    fn the_multiplier_keeps_its_precision() {
        for hz in RATES {
            let s = Scale::new(hz).unwrap();
            assert!(s.shift == 64 || s.mult >= 1 << 63, "hz {hz}: {s:?}");
        }
        // 24 MHz: one tick is 41.666… ns; whole microseconds and a whole day come out
        // exact (a rounded-down multiplier would make each one nanosecond short).
        let s = Scale::new(24_000_000).unwrap();
        assert_eq!(s.nanos(24), 1000);
        assert_eq!(s.nanos(24_000_000 * 86_400), 86_400 * NANOS_PER_SECOND);
        assert_eq!(s.nanos(1), 41);
        assert_eq!(s.nanos(2), 83);
        // 1 GHz is the identity.
        let s = Scale::new(1_000_000_000).unwrap();
        for t in [0, 1, 12_345, u64::MAX / 3] {
            assert_eq!(s.nanos(t), t);
        }
    }

    #[test]
    fn large_values_saturate() {
        let s = Scale::new(1).unwrap();
        assert_eq!(s.nanos(u64::MAX), u64::MAX);
        assert_eq!(s.ticks_for(u64::MAX), u64::MAX / NANOS_PER_SECOND + 1);
        let s = Scale::new(u64::MAX).unwrap();
        assert_eq!(s.ticks_for(u64::MAX), u64::MAX);
        assert_eq!(Scale::new(0), None);
    }

    #[test]
    fn ticks_for_rounds_up_and_inverts_nanos() {
        for hz in RATES {
            let s = Scale::new(hz).unwrap();
            for nanos in [0u64, 1, 41, 42, 999, 1_000_000, 10_000_000, 1 << 40] {
                let t = s.ticks_for(nanos);
                let needed =
                    (u128::from(nanos) * u128::from(hz)).div_ceil(u128::from(NANOS_PER_SECOND));
                if needed > u128::from(u64::MAX) {
                    assert_eq!(t, u64::MAX, "saturates: hz {hz} nanos {nanos}");
                    continue;
                }
                // Enough ticks: their exact duration is at least `nanos`...
                assert!(exact(t, hz) >= u128::from(nanos), "hz {hz} nanos {nanos}");
                // ...and one fewer would not be.
                if t > 0 {
                    assert!(
                        exact(t - 1, hz) < u128::from(nanos),
                        "hz {hz} nanos {nanos}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_shared_scale_is_published_once() {
        let shared = SharedScale::new();
        assert_eq!(shared.get(), None);
        let first = Scale::new(24_000_000).unwrap();
        assert!(shared.publish(first));
        assert!(!shared.publish(Scale::new(1_000_000_000).unwrap()));
        assert_eq!(shared.get(), Some(first));
        // Readers on other threads see the whole value.
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| assert_eq!(shared.get(), Some(first)));
            }
        });
    }

    #[test]
    fn cpuid_leaf_15h() {
        // A 38.4 MHz crystal at a ratio of 125/2: 2.4 GHz.
        assert_eq!(cpuid_tsc_hz(2, 125, 38_400_000), Some(2_400_000_000));
        // A 25 MHz crystal at 88/2.
        assert_eq!(cpuid_tsc_hz(2, 88, 25_000_000), Some(1_100_000_000));
        assert_eq!(cpuid_tsc_hz(0, 125, 38_400_000), None);
        assert_eq!(cpuid_tsc_hz(2, 0, 38_400_000), None);
        assert_eq!(cpuid_tsc_hz(2, 125, 0), None, "crystal not enumerated");
        assert_eq!(cpuid_tsc_hz(u32::MAX, 1, 1), None, "rounds to zero");
        assert_eq!(
            cpuid_tsc_hz(1, u32::MAX, u32::MAX),
            Some(u64::from(u32::MAX) * u64::from(u32::MAX))
        );
    }

    #[test]
    fn a_read_paired_with_the_clock_is_retried_until_it_is_close() {
        // The first read is close: taken at once, at the middle of its bracket.
        let mut calls = 0;
        let mut once = || {
            calls += 1;
            (100, 7, 110)
        };
        assert_eq!(closest_read(8, 20, &mut once), (7, 105));
        assert_eq!(calls, 1);
        // Two stalled reads, then a close one.
        let mut calls = 0;
        let mut stalled = || {
            calls += 1;
            if calls < 3 {
                (0, 1, 500)
            } else {
                (1000, 2, 1010)
            }
        };
        assert_eq!(closest_read(8, 20, &mut stalled), (2, 1005));
        assert_eq!(calls, 3);
        // Never close enough: the narrowest of the attempts is used, and every attempt
        // was made.
        let mut widths = [900u64, 300, 700].iter().cycle();
        let mut calls = 0;
        let mut wide = || {
            calls += 1;
            let w = *widths.next().unwrap();
            (10_000, w, 10_000 + w)
        };
        assert_eq!(closest_read(5, 20, &mut wide), (300, 10_150));
        assert_eq!(calls, 5);
        // At least one attempt is always made.
        assert_eq!(closest_read(0, 20, || (5, 6, 5)), (6, 5));
    }

    /// Any values at all: nothing overflows or panics, and the value returned is the one
    /// read with the narrowest bracket of the attempts made (CLAUDE.md §13).
    #[test]
    fn any_paired_reads_are_handled() {
        use crate::testing::Rng;
        let mut rng = Rng::new(0x5041_4952);
        for case in 0..crate::testing::cases(50_000) {
            let reads: Vec<(u64, u64, u64)> = (0..1 + rng.below(6))
                .map(|_| (rng.edge_u64(), rng.edge_u64(), rng.edge_u64()))
                .collect();
            let mut i = 0;
            let (value, at) = closest_read(reads.len() as u32, rng.edge_u64(), || {
                let r = reads[i.min(reads.len() - 1)];
                i += 1;
                r
            });
            let matches_a_read = reads.iter().any(|&(before, v, after)| {
                v == value && at == before.saturating_add(after.saturating_sub(before) / 2)
            });
            assert!(matches_a_read, "case {case}");
        }
    }

    #[test]
    fn rate_agreement() {
        assert!(rates_agree(2_400_000_000, 2_402_000_000, 1));
        assert!(!rates_agree(2_400_000_000, 2_403_000_000, 1));
        assert!(rates_agree(24_000_000, 24_000_000, 0));
        assert!(rates_agree(u64::MAX, u64::MAX - 1, 1));
        assert!(!rates_agree(0, 1, 1000));
    }
}
