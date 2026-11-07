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

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;

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
