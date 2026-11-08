//! A monotonic-deadline timer queue (CLAUDE.md §68: high-resolution timers, timer
//! cancellation). Timers are armed with a deadline on the monotonic clock (nanoseconds)
//! and an opaque id; the owner polls [`TimerQueue::expire`] with the current time to take
//! the timers that are due, in deadline order. Pure, no `unsafe`, tested on the host; the
//! kernel drives it from the monotonic clock (`src/time.rs`).
//!
//! The queue is kept sorted by deadline, so the earliest is O(1) to find and expiry is a
//! prefix. Arming or cancelling is O(n); a binary heap would improve that and is a later
//! optimization. An id armed again moves its timer to the new deadline.

extern crate alloc;
use alloc::vec::Vec;

/// One armed timer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Timer {
    deadline: u64,
    id: u64,
}

/// A set of timers ordered by deadline.
#[derive(Debug, Default)]
pub struct TimerQueue {
    /// Sorted by `deadline` ascending, ties by `id` so the order is total and stable.
    timers: Vec<Timer>,
}

impl TimerQueue {
    pub const fn new() -> Self {
        Self { timers: Vec::new() }
    }

    /// Timers currently armed.
    pub fn len(&self) -> usize {
        self.timers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.timers.is_empty()
    }

    /// Arms a timer `id` to fire at `deadline`. If `id` is already armed, it is moved to
    /// the new deadline. Returns whether this replaced an existing timer.
    pub fn arm(&mut self, id: u64, deadline: u64) -> bool {
        let existed = self.cancel(id);
        let timer = Timer { deadline, id };
        // Insert keeping the (deadline, id) order.
        let at = self
            .timers
            .partition_point(|t| (t.deadline, t.id) < (timer.deadline, timer.id));
        self.timers.insert(at, timer);
        existed
    }

    /// Cancels timer `id`; returns whether it was armed.
    pub fn cancel(&mut self, id: u64) -> bool {
        if let Some(at) = self.timers.iter().position(|t| t.id == id) {
            self.timers.remove(at);
            true
        } else {
            false
        }
    }

    /// The earliest deadline, or `None` if no timer is armed.
    pub fn next_deadline(&self) -> Option<u64> {
        self.timers.first().map(|t| t.deadline)
    }

    /// Removes and returns the ids of every timer whose deadline is at or before `now`, in
    /// deadline order (ties by id), into `out`; returns how many fired. A timer still in
    /// the future is left armed.
    pub fn expire(&mut self, now: u64, out: &mut Vec<u64>) -> usize {
        let fired = self.timers.partition_point(|t| t.deadline <= now);
        out.extend(self.timers.drain(..fired).map(|t| t.id));
        fired
    }
}

/// What the polls around one timer's firing can say about how late the queue delivered it.
///
/// The queue only looks at the time when it is polled, so a poll that comes late is a late
/// *look*: it says nothing about a queue that fired at the first poll after the deadline.
/// A firing's lateness therefore lies between what the poll before it saw and what the poll
/// that fired it saw, and only a poll that did **not** fire a timer that was already overdue
/// shows the queue kept a due timer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Firing {
    /// When the timer was due, on the monotonic clock (nanoseconds).
    pub deadline: u64,
    /// The last poll that did not fire it: the poll before `fired_at`, or for a timer that
    /// never fired, the last poll of the run (at the run's start, before the first poll).
    pub previous_poll: u64,
    /// The poll that fired it, if one did.
    pub fired_at: Option<u64>,
}

/// How a [`Firing`], or a whole run of them, compares with a lateness limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The poll that fired the timer was within the limit of its deadline. Whatever else
    /// the polling loop did, the queue delivered it on time.
    OnTime,
    /// A poll that did not fire the timer was already at least the limit past its
    /// deadline: the queue kept a due timer that long. A stall cannot explain this.
    Late,
    /// Neither: the gap between the two polls straddles the limit, so the poller was away
    /// when it mattered. This firing says nothing about the queue.
    Undecided,
}

impl Firing {
    /// The most the queue can have been late: the poll that fired it, against the deadline.
    /// For a timer that never fired there is no upper bound.
    #[must_use]
    pub const fn latest(&self) -> Option<u64> {
        match self.fired_at {
            Some(at) => Some(at.saturating_sub(self.deadline)),
            None => None,
        }
    }

    /// The least the queue can have been late: the last poll that did not fire the timer,
    /// against the deadline (zero if that poll came before the deadline).
    #[must_use]
    pub const fn earliest(&self) -> u64 {
        self.previous_poll.saturating_sub(self.deadline)
    }

    /// Judges this firing against `limit` nanoseconds of lateness. A timer is on time only
    /// if the whole interval of its possible lateness is inside the limit, and late only if
    /// the whole interval is outside it; anything else is undecided.
    #[must_use]
    pub const fn verdict(&self, limit: u64) -> Verdict {
        if self.earliest() >= limit {
            return Verdict::Late;
        }
        match self.latest() {
            Some(latest) if latest < limit => Verdict::OnTime,
            _ => Verdict::Undecided,
        }
    }
}

/// Judges a whole run: late if any firing is, on time only if all of them are.
#[must_use]
pub fn judge(firings: &[Firing], limit: u64) -> Verdict {
    let verdicts = || firings.iter().map(|f| f.verdict(limit));
    if verdicts().any(|v| v == Verdict::Late) {
        Verdict::Late
    } else if verdicts().all(|v| v == Verdict::OnTime) {
        Verdict::OnTime
    } else {
        Verdict::Undecided
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;

    /// The bound `prove_timers` uses, 2 ms.
    const LIMIT: u64 = 2_000_000;

    #[test]
    fn a_firing_within_the_limit_of_its_deadline_is_on_time_however_long_the_gap() {
        // The poll that fired it is what counts: 1.999 ms after the deadline, whatever the
        // poll before it was doing.
        let f = Firing {
            deadline: 5_000_000,
            previous_poll: 4_999_990,
            fired_at: Some(5_000_000 + LIMIT - 1),
        };
        assert_eq!(f.verdict(LIMIT), Verdict::OnTime);
        assert_eq!(f.latest(), Some(LIMIT - 1));
        assert_eq!(f.earliest(), 0);
    }

    #[test]
    fn a_stall_straddling_the_limit_is_undecided_not_late() {
        // The poll before was well before the deadline, the next poll a stall later: a
        // late look, not a late timer.
        let f = Firing {
            deadline: 5_000_000,
            previous_poll: 4_999_000,
            fired_at: Some(5_000_000 + LIMIT),
        };
        assert_eq!(f.verdict(LIMIT), Verdict::Undecided);
        let f = Firing {
            deadline: 5_000_000,
            previous_poll: 5_000_000 + LIMIT - 1,
            fired_at: Some(5_000_000 + 9 * LIMIT),
        };
        assert_eq!(f.verdict(LIMIT), Verdict::Undecided);
    }

    #[test]
    fn a_timer_already_overdue_by_the_limit_at_a_poll_that_did_not_fire_it_is_late() {
        let f = Firing {
            deadline: 5_000_000,
            previous_poll: 5_000_000 + LIMIT,
            fired_at: Some(5_000_000 + LIMIT + 50_000),
        };
        assert_eq!(f.verdict(LIMIT), Verdict::Late);
        assert_eq!(f.earliest(), LIMIT);
    }

    #[test]
    fn a_timer_that_never_fired_is_late_only_if_the_last_poll_saw_it_overdue() {
        let held = |previous_poll| Firing {
            deadline: 5_000_000,
            previous_poll,
            fired_at: None,
        };
        // Still before the deadline, or overdue by less than the limit: says nothing.
        assert_eq!(held(4_000_000).verdict(LIMIT), Verdict::Undecided);
        assert_eq!(
            held(5_000_000 + LIMIT - 1).verdict(LIMIT),
            Verdict::Undecided
        );
        // Overdue by the limit at a poll that did not fire it.
        assert_eq!(held(5_000_000 + LIMIT).verdict(LIMIT), Verdict::Late);
    }

    #[test]
    fn a_run_is_on_time_only_if_every_firing_is_and_late_if_any_firing_is() {
        let on_time = Firing {
            deadline: 1,
            previous_poll: 0,
            fired_at: Some(2),
        };
        let undecided = Firing {
            deadline: 1,
            previous_poll: 0,
            fired_at: Some(LIMIT + 10),
        };
        let late = Firing {
            deadline: 1,
            previous_poll: LIMIT + 1,
            fired_at: Some(LIMIT + 2),
        };
        assert_eq!(judge(&[on_time, on_time, on_time], LIMIT), Verdict::OnTime);
        assert_eq!(
            judge(&[on_time, undecided, on_time], LIMIT),
            Verdict::Undecided
        );
        assert_eq!(judge(&[on_time, undecided, late], LIMIT), Verdict::Late);
        assert_eq!(judge(&[], LIMIT), Verdict::OnTime);
    }

    /// With the poller looking closely, the verdict is the truth in both directions.
    #[test]
    fn closely_polled_runs_decide_correctly() {
        for (hold, expected) in [
            (0, Verdict::OnTime),
            (LIMIT - 100_000, Verdict::OnTime),
            (LIMIT + 100_000, Verdict::Late),
            (3 * LIMIT, Verdict::Late),
        ] {
            let deadlines = [5_000_000u64, 10_000_000, 15_000_000];
            let mut q = TimerQueue::new();
            for (i, &d) in deadlines.iter().enumerate() {
                q.arm(i as u64, d + hold);
            }
            let mut firings = deadlines.map(|deadline| Firing {
                deadline,
                previous_poll: 0,
                fired_at: None,
            });
            let (mut now, mut last_poll, mut order) = (0u64, 0u64, 0usize);
            let mut fired = Vec::new();
            while !q.is_empty() {
                now += 50_000; // a poll every 50 µs
                if q.expire(now, &mut fired) > 0 {
                    for _ in fired.drain(..) {
                        firings[order].previous_poll = last_poll;
                        firings[order].fired_at = Some(now);
                        order += 1;
                    }
                }
                last_poll = now;
            }
            assert_eq!(judge(&firings, LIMIT), expected, "hold {hold}");
        }
    }

    /// A simulated poll loop over a correct queue with stalls of any length never produces
    /// a `Late` verdict, and a queue that holds timers for longer than the limit never
    /// produces an `OnTime` verdict.
    #[test]
    fn verdicts_are_sound_under_any_pattern_of_stalls() {
        const STEP: u64 = 5_000_000;
        let mut x = 0xDEAD_BEEF_u64;
        let mut next = |bound: u64| {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (x >> 33) % bound.max(1)
        };
        for round in 0..4000u64 {
            // A queue that delivers each timer `hold` nanoseconds after it is due: zero for
            // a correct queue, at least the limit for one that is genuinely late.
            let hold = if round % 2 == 0 {
                0
            } else {
                LIMIT + next(LIMIT)
            };
            let mut q = TimerQueue::new();
            let base = 1_000_000 + next(1_000_000);
            let deadlines: [u64; 3] = core::array::from_fn(|i| base + (i as u64 + 1) * STEP);
            for (i, &d) in deadlines.iter().enumerate() {
                // A held queue is modelled by arming the timer `hold` later.
                q.arm(i as u64, d + hold);
            }
            let mut firings = deadlines.map(|deadline| Firing {
                deadline,
                previous_poll: base,
                fired_at: None,
            });
            let mut now = base;
            let mut last_poll = base;
            let mut fired = alloc::vec::Vec::new();
            let mut order = 0usize;
            while !q.is_empty() {
                // A poll gap: usually tiny, sometimes a stall of up to 30 ms.
                now += if next(4) == 0 {
                    next(30_000_000)
                } else {
                    20 + next(100)
                };
                if q.expire(now, &mut fired) > 0 {
                    for _ in fired.drain(..) {
                        if let Some(f) = firings.get_mut(order) {
                            f.previous_poll = last_poll;
                            f.fired_at = Some(now);
                        }
                        order += 1;
                    }
                }
                last_poll = now;
            }
            match judge(&firings, LIMIT) {
                Verdict::Late => {
                    assert!(hold >= LIMIT, "a correct queue was called late");
                }
                Verdict::OnTime => {
                    assert_eq!(
                        hold, 0,
                        "a queue holding timers {hold} ns was called on time"
                    );
                }
                Verdict::Undecided => {}
            }
        }
    }

    #[test]
    fn arm_order_and_next_deadline() {
        let mut q = TimerQueue::new();
        assert_eq!(q.next_deadline(), None);
        assert!(!q.arm(1, 300));
        assert!(!q.arm(2, 100));
        assert!(!q.arm(3, 200));
        assert_eq!(q.len(), 3);
        assert_eq!(q.next_deadline(), Some(100));
        // Re-arming an id moves it and reports the replacement.
        assert!(q.arm(2, 500));
        assert_eq!(q.next_deadline(), Some(200), "id 2 moved to 500");
        assert_eq!(q.len(), 3, "re-arm does not add a timer");
    }

    #[test]
    fn expiry_is_a_deadline_ordered_prefix() {
        let mut q = TimerQueue::new();
        for (id, d) in [(1, 100), (2, 100), (3, 150), (4, 400)] {
            q.arm(id, d);
        }
        let mut out = Vec::new();
        // Nothing due before 100.
        assert_eq!(q.expire(99, &mut out), 0);
        assert!(out.is_empty());
        // At 150, ids 1, 2, 3 fire in (deadline, id) order; id 4 stays.
        assert_eq!(q.expire(150, &mut out), 3);
        assert_eq!(out, [1, 2, 3]);
        assert_eq!(q.len(), 1);
        assert_eq!(q.next_deadline(), Some(400));
        out.clear();
        assert_eq!(q.expire(1000, &mut out), 1);
        assert_eq!(out, [4]);
        assert!(q.is_empty());
    }

    #[test]
    fn cancel_removes_only_that_timer() {
        let mut q = TimerQueue::new();
        q.arm(10, 100);
        q.arm(20, 200);
        assert!(q.cancel(10));
        assert!(!q.cancel(10), "already gone");
        assert_eq!(q.next_deadline(), Some(200));
        let mut out = Vec::new();
        q.expire(1000, &mut out);
        assert_eq!(out, [20], "the cancelled timer never fires");
    }

    #[test]
    fn a_deterministic_workload_matches_a_model() {
        // A reference model: id -> deadline, the source of truth.
        let mut model: BTreeMap<u64, u64> = BTreeMap::new();
        let mut q = TimerQueue::new();
        let mut x = 0x1234_5678u64;
        let mut now = 0u64;
        for _ in 0..5000 {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            match x >> 62 {
                0 | 1 => {
                    let id = (x >> 20) % 40;
                    let deadline = now + 1 + (x >> 30) % 1000;
                    model.insert(id, deadline);
                    q.arm(id, deadline);
                }
                2 => {
                    let id = (x >> 20) % 40;
                    let had = model.remove(&id).is_some();
                    assert_eq!(q.cancel(id), had);
                }
                _ => {
                    now += 1 + (x >> 40) % 300;
                    let mut fired = Vec::new();
                    q.expire(now, &mut fired);
                    // The model's due timers, in (deadline, id) order.
                    let mut expected: Vec<(u64, u64)> = model
                        .iter()
                        .filter_map(|(&i, &d)| (d <= now).then_some((d, i)))
                        .collect();
                    expected.sort();
                    let expected_ids: Vec<u64> = expected.iter().map(|&(_, i)| i).collect();
                    assert_eq!(fired, expected_ids, "at now={now}");
                    for id in fired {
                        model.remove(&id);
                    }
                }
            }
            assert_eq!(q.len(), model.len());
            assert_eq!(q.next_deadline(), model.values().copied().min());
        }
    }
}
