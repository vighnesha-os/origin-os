//! The health tests on hand-made sequences (ADR-0016 R4), and the cutoffs against the
//! standard's formulas (ledger §5).

use super::world::splitmix;
use crate::{Condition, Fault, Health, Offer, STARTUP_DRAWS, WINDOW};

/// A healthy sequence's draw `n`.
fn good(n: u64) -> u64 {
    splitmix(0xC0FF_EE00, n)
}

/// A test that has been started and fed `draws` healthy draws.
fn fed(draws: u64) -> Health {
    let mut health = Health::untested();
    health.begin_test();
    for n in 0..draws {
        assert_eq!(health.offer(good(n)), Offer::Accepted, "draw {n}");
    }
    health
}

#[test]
fn a_start_up_test_is_exactly_1024_passing_draws() {
    assert_eq!(STARTUP_DRAWS, 1024);
    assert_eq!(WINDOW, 512);
    let mut health = Health::untested();
    assert_eq!(health.condition(), Condition::Untested);
    // Nothing is taken from an instruction that is not being tested.
    assert_eq!(health.offer(good(0)), Offer::SetAside);
    assert_eq!(health.accepted(), 0);
    health.begin_test();
    for n in 0..1023 {
        assert_eq!(health.offer(good(n)), Offer::Accepted);
        assert_eq!(health.condition(), Condition::Testing, "after draw {n}");
        assert!(!health.is_healthy());
    }
    assert_eq!(health.passed(), 1023);
    assert_eq!(health.offer(good(1023)), Offer::Accepted);
    assert_eq!(health.condition(), Condition::Healthy);
    assert_eq!(health.accepted(), 1024);
    // The continuous tests go on.
    for n in 1024..3000 {
        assert_eq!(health.offer(good(n)), Offer::Accepted);
    }
    assert!(health.is_healthy());
    assert_eq!(health.faults(), 0);
    assert_eq!(health.last_fault(), None);
}

#[test]
fn an_absent_instruction_is_never_tested() {
    let mut health = Health::absent();
    health.begin_test();
    assert_eq!(health.condition(), Condition::Absent);
    assert_eq!(health.offer(good(0)), Offer::SetAside);
    assert_eq!(health.stuck(), Offer::SetAside);
    assert_eq!(health.condition(), Condition::Absent);
}

#[test]
fn zero_and_all_ones_are_set_aside_and_break_nothing() {
    let mut health = fed(10);
    for value in [0, u64::MAX, 0, 0, u64::MAX] {
        assert_eq!(health.offer(value), Offer::SetAside);
    }
    assert_eq!(health.set_aside(), 5);
    assert_eq!(health.passed(), 10);
    assert_eq!(health.condition(), Condition::Testing);
    // The sequence goes on where it was: the values set aside were not draws.
    assert_eq!(health.offer(good(10)), Offer::Accepted);
    assert_eq!(health.passed(), 11);
}

#[test]
fn a_draw_of_nothing_but_stuck_values_is_a_failure() {
    let mut health = fed(1024);
    assert_eq!(health.stuck(), Offer::Failed(Fault::StuckValue));
    assert_eq!(health.condition(), Condition::Unhealthy);
    assert_eq!(health.last_fault(), Some(Fault::StuckValue));
    assert_eq!(health.faults(), 1);
}

#[test]
fn a_repeat_fails_at_every_place() {
    for at in [1u64, 2, 100, 511, 512, 513, 1023, 1024, 1500] {
        let mut health = fed(at);
        assert_eq!(
            health.offer(good(at - 1)),
            Offer::Failed(Fault::Repeat),
            "after {at} draws"
        );
        assert_eq!(health.condition(), Condition::Unhealthy);
        assert_eq!(health.passed(), 0);
        // An unhealthy instruction is offered nothing more.
        assert_eq!(health.offer(good(at)), Offer::SetAside);
    }
}

#[test]
fn a_windows_first_draw_coming_again_fails() {
    // In the first window, the second and the third: the first draw of each is the window's.
    for window in [0u64, 1, 2] {
        let first = window * 512;
        for again in [2u64, 3, 255, 510, 511] {
            let mut health = fed(first + again);
            assert_eq!(
                health.offer(good(first)),
                Offer::Failed(Fault::Proportion),
                "window {window}, position {again}"
            );
        }
        // In the next window the same value is not that window's first: it passes.
        let mut health = fed(first + 512 + 5);
        assert_eq!(
            health.offer(good(first)),
            Offer::Accepted,
            "window {window}"
        );
    }
}

#[test]
fn a_bit_that_never_moves_fails_at_the_windows_end_and_no_sooner() {
    for bit in 0..64 {
        for set in [false, true] {
            let stuck = |n: u64| {
                let value = good(n);
                if set {
                    value | (1 << bit)
                } else {
                    value & !(1 << bit)
                }
            };
            let mut health = Health::untested();
            health.begin_test();
            for n in 0..511 {
                assert_eq!(
                    health.offer(stuck(n)),
                    Offer::Accepted,
                    "bit {bit}, draw {n}"
                );
            }
            assert_eq!(
                health.offer(stuck(511)),
                Offer::Failed(Fault::StuckBit),
                "bit {bit} stuck at {set}"
            );
            assert_eq!(health.condition(), Condition::Unhealthy);
        }
    }
}

#[test]
fn half_a_draw_stuck_is_caught_though_every_value_differs() {
    // What a 32-bit form of the instruction would give: the high half always zero.
    let mut health = Health::untested();
    health.begin_test();
    let mut failed = None;
    for n in 0..512 {
        if let Offer::Failed(fault) = health.offer(good(n) & 0xFFFF_FFFF) {
            failed = Some((n, fault));
            break;
        }
    }
    assert_eq!(failed, Some((511, Fault::StuckBit)));
}

#[test]
fn a_failure_forgets_the_test_and_a_new_one_starts_from_nothing() {
    let mut health = fed(1000);
    assert_eq!(health.offer(good(999)), Offer::Failed(Fault::Repeat));
    health.begin_test();
    assert_eq!(health.condition(), Condition::Testing);
    assert_eq!(health.passed(), 0);
    // The draw that failed is forgotten too: the same value is a first draw now.
    assert_eq!(health.offer(good(999)), Offer::Accepted);
    for n in 2000..3023 {
        assert_eq!(health.offer(good(n)), Offer::Accepted);
    }
    assert!(health.is_healthy());
    assert_eq!(health.faults(), 1);
}

#[test]
fn a_test_given_up_leaves_the_instruction_unhealthy() {
    let mut health = fed(500);
    health.abandon_test();
    assert_eq!(health.condition(), Condition::Unhealthy);
    assert_eq!(health.faults(), 0, "not being ready is not a fault");
    // Only a running test can be given up.
    let mut healthy = fed(1024);
    healthy.abandon_test();
    assert!(healthy.is_healthy());
}

#[test]
fn debug_prints_no_draw() {
    let health = fed(3);
    assert_eq!(format!("{health:?}"), "Health { .. }");
}

/// The upper tail of a binomial distribution: the probability of `k` or more successes in `n`
/// trials of probability `p`. Summed from the top, so nothing is lost to cancellation.
fn tail(n: u32, p: f64, k: u32) -> f64 {
    let (ln_p, ln_q) = (p.ln(), (-p).ln_1p());
    let mut total = 0.0;
    for j in (k..=n).rev() {
        // ln C(n, j), as a sum of logarithms.
        let mut ln_c = 0.0;
        for i in 0..j.min(n - j) {
            ln_c += f64::from(n - i).ln() - f64::from(i + 1).ln();
        }
        total += (ln_c + f64::from(j) * ln_p + f64::from(n - j) * ln_q).exp();
    }
    total
}

/// SP 800-90B §4.4.2's cutoff for a window of `window` samples of `h` bits of min-entropy at a
/// false-alarm rate of 2^-20: `1 + CRITBINOM(W, 2^-H, 1 - alpha)`.
fn proportion_cutoff(window: u32, h: f64) -> u32 {
    let alpha = 2f64.powi(-20);
    let p = 2f64.powf(-h);
    // CRITBINOM is the smallest c with P(X <= c) >= 1 - alpha, which is P(X >= c + 1) <= alpha.
    let c = (0..=window)
        .find(|&c| c == window || tail(window, p, c + 1) <= alpha)
        .unwrap();
    1 + c
}

/// §4.4.1's cutoff: `1 + ceil(-log2(alpha) / H)`.
fn repetition_cutoff(h: f64) -> u32 {
    1 + (20.0 / h).ceil() as u32
}

#[test]
fn the_formula_gives_the_standards_own_table() {
    // SP 800-90B Table 2, non-binary data, as the ledger's section 5 records it.
    for (h, cutoff) in [(0.5, 410), (1.0, 311), (2.0, 177), (4.0, 62), (8.0, 13)] {
        assert_eq!(proportion_cutoff(512, h), cutoff, "H = {h}");
    }
}

#[test]
fn the_cutoffs_in_use_are_what_the_formulas_give_for_a_64_bit_draw() {
    // Both cutoffs are 2 for a draw assumed to hold anywhere from 20 to 28 bits.
    for h in 20..=28 {
        assert_eq!(repetition_cutoff(f64::from(h)), 2, "H = {h}");
        assert_eq!(proportion_cutoff(512, f64::from(h)), 2, "H = {h}");
    }
    // Below that they are looser; this is where 2 stops being the formula's answer.
    assert_eq!(repetition_cutoff(16.0), 3);
    assert_eq!(proportion_cutoff(512, 16.0), 3);
    // At the entropy the vendors claim, the proportion formula gives 1, which every window
    // meets at its first draw: 2 is the tightest cutoff that means anything.
    for h in [29, 32, 48, 64] {
        assert_eq!(repetition_cutoff(f64::from(h)), 2, "H = {h}");
        assert_eq!(proportion_cutoff(512, f64::from(h)), 1, "H = {h}");
    }
}
