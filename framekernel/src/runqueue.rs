//! A per-CPU round-robin run queue of saved contexts (M3, first scheduler; CLAUDE.md §25
//! thread lifecycle and termination, §26: "begin with a correct per-CPU scheduler", "keep a
//! safe scheduler fallback").
//!
//! A context is the stack pointer at which an interrupted thread's full register frame is
//! saved: the timer interrupt (or a yield, sleep or exit request, which enter the kernel the
//! same way) saves the running thread's frame on its own stack, and the queue answers which
//! saved frame to resume. Slot 0 is the context that was running when the queue started
//! (the CPU's boot flow or idle loop); it can never sleep or exit, so there is always
//! something to run. Time is counted in this CPU's timer ticks. This is policy only: plain
//! integers, no `unsafe`, tested on the host. Priorities, fairness accounting and load
//! balancing come later (an EEVDF-style design is to be evaluated, §26).
//!
//! Choosing the next context is stride scheduling (C. A. Waldspurger and W. E. Weihl,
//! "Stride Scheduling: Deterministic Proportional-Share Resource Management", MIT
//! LCS/TM-528, 1995), a deterministic proportional-share policy:
//! - every context has a weight (1–[`MAX_WEIGHT`], default [`DEFAULT_WEIGHT`]) and a pass
//!   value; each quantum it uses (a tick, or a yield) advances its pass by
//!   [`STRIDE_ONE`]` / weight`;
//! - the ready context with the lowest pass runs next, ties going round-robin from the
//!   current one, so equal weights are exactly round-robin and over any long stretch each
//!   context's share of the processor is its share of the weights;
//! - a context that becomes ready (new, woken, or due after a sleep) starts at no less than
//!   the lowest pass among the ready ones: sleeping earns no credit, so it can neither
//!   starve the others nor be starved;
//! - slot 0 can be the processor's idle context ([`RunQueue::set_idle`]), which runs only
//!   when nothing else is ready.
//!
//! Blocking: a thread [`RunQueue::block`]s until another [`RunQueue::wake`]s it. A wake-up
//! names its thread by slot and the slot's generation (bumped by every [`RunQueue::push`]),
//! so a stale one never wakes a later thread in the same slot. A wake-up that arrives
//! before its thread has blocked (it registered with a wait queue and is about to block) is
//! kept, and its next `block` returns at once: no wake-up is lost.
//!
//! An interrupt or IPI that may have woken a thread goes through
//! [`RunQueue::preempt_for_wake`]: it charges nothing, and switches only if a thread woken
//! since the last choice should run now.

/// Contexts one queue holds, its boot context included.
pub const CAPACITY: usize = 16;
/// The pass a weight-1 context advances per quantum.
pub const STRIDE_ONE: u64 = 1 << 20;
/// The weight of a context nobody chose one for; the largest weight.
pub const DEFAULT_WEIGHT: u32 = 8;
pub const MAX_WEIGHT: u32 = 1024;

/// `part` of `whole`, in parts per thousand (a `whole` of 0 counts as 1).
pub fn per_mille(part: u64, whole: u64) -> u64 {
    part.saturating_mul(1000) / whole.max(1)
}

/// Whether a measured `share` (per mille) is near the share `weight` is due of `total`
/// weights: within a third of that ideal, plus 15 per mille. This is the bound of the
/// weighted-share boot proof (`sched::prove_weights`).
pub fn share_near_weight(share: u64, weight: u32, total: u32) -> bool {
    let ideal = u64::from(weight) * 1000 / u64::from(total.max(1));
    share.abs_diff(ideal) <= ideal / 3 + 15
}

/// The bound of the wake-up ping-pong boot proof (`wait::prove`): the round trip between
/// two processors, each woken by the other, in microseconds. Woken only by the other's
/// timer tick (10 ms), a round trip would take milliseconds.
pub const ROUND_TRIP_LIMIT_MICROS: u64 = 2000;

/// A ping-pong run over the bound is measured again if the boot flow went without a turn
/// for at least this much of it, in parts per thousand: on a loaded host, a virtual CPU
/// descheduled. A tenth, as for the weighted-share proof. The boot flow shares its processor
/// with a thread under test, which can produce the same reading just by running, so this can
/// earn a repeat and nothing more.
pub const DISTURBED_PER_MILLE: u64 = 100;

/// What one run of the ping-pong boot proof calls for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundTrips {
    /// The mean round trip is under the bound.
    Pass,
    /// Over the bound, on a run that shows a disturbance: it says nothing, measure again.
    Repeat,
    /// Over the bound, on a run that shows none.
    Fail,
}

/// Judges a ping-pong run by its mean round trip (microseconds). A disturbance
/// (`away_per_mille`, see [`DISTURBED_PER_MILLE`]) can earn a run over the bound a repeat;
/// it can never pass one.
pub const fn judge_round_trips(mean_micros: u64, away_per_mille: u64) -> RoundTrips {
    if mean_micros < ROUND_TRIP_LIMIT_MICROS {
        RoundTrips::Pass
    } else if away_per_mille >= DISTURBED_PER_MILLE {
        RoundTrips::Repeat
    } else {
        RoundTrips::Fail
    }
}

/// Why a request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueError {
    /// The queue holds [`CAPACITY`] contexts.
    Full,
    /// A saved context's stack pointer is not 8-byte aligned or is null.
    BadContext(u64),
    /// The boot context (slot 0) may not sleep or exit.
    BootContext,
}

/// What a slot holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Free,
    Ready,
    /// Asleep until this tick.
    Sleeping(u64),
    /// Waiting for a [`RunQueue::wake`], or until this tick if there is one.
    Blocked(Option<u64>),
}

/// One CPU's run queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunQueue {
    saved: [u64; CAPACITY],
    state: [State; CAPACITY],
    /// Bumped each time a slot gets a new thread.
    generation: [u32; CAPACITY],
    /// A wake-up that arrived while the slot's thread was not blocked.
    wake_pending: [bool; CAPACITY],
    /// The slots a wake-up made ready since this queue last chose what to run.
    woken: [bool; CAPACITY],
    /// Each slot's weight and pass (stride scheduling).
    weight: [u32; CAPACITY],
    pass: [u64; CAPACITY],
    /// Slot 0 is the idle context: it runs only when nothing else is ready.
    idle: bool,
    current: usize,
    ticks: u64,
    switches: u64,
}

impl Default for RunQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl RunQueue {
    /// A queue holding only the running (boot) context.
    pub const fn new() -> Self {
        let mut state = [State::Free; CAPACITY];
        state[0] = State::Ready;
        Self {
            saved: [0; CAPACITY],
            state,
            generation: [0; CAPACITY],
            wake_pending: [false; CAPACITY],
            woken: [false; CAPACITY],
            weight: [DEFAULT_WEIGHT; CAPACITY],
            pass: [0; CAPACITY],
            idle: false,
            current: 0,
            ticks: 0,
            switches: 0,
        }
    }

    /// Adds a thread whose initial frame is saved at `context`; returns its slot.
    pub fn push(&mut self, context: u64) -> Result<usize, QueueError> {
        self.push_weighted(context, DEFAULT_WEIGHT)
    }

    /// As [`RunQueue::push`], with `weight` (clamped to 1–[`MAX_WEIGHT`]).
    pub fn push_weighted(&mut self, context: u64, weight: u32) -> Result<usize, QueueError> {
        if context == 0 || !context.is_multiple_of(8) {
            return Err(QueueError::BadContext(context));
        }
        let slot = (1..CAPACITY)
            .find(|&i| self.state.get(i) == Some(&State::Free))
            .ok_or(QueueError::Full)?;
        // The floor before this slot joins: its pass is a previous thread's.
        let floor = self.floor();
        if let (Some(saved), Some(state)) = (self.saved.get_mut(slot), self.state.get_mut(slot)) {
            *saved = context;
            *state = State::Ready;
        }
        if let (Some(g), Some(w)) = (
            self.generation.get_mut(slot),
            self.wake_pending.get_mut(slot),
        ) {
            *g = g.wrapping_add(1);
            *w = false;
        }
        if let (Some(w), Some(p)) = (self.weight.get_mut(slot), self.pass.get_mut(slot)) {
            *w = weight.clamp(1, MAX_WEIGHT);
            *p = floor;
        }
        Ok(slot)
    }

    /// Makes slot 0 the idle context (it runs only when nothing else is ready), or a
    /// context like any other.
    pub fn set_idle(&mut self, idle: bool) {
        if self.idle && !idle {
            // The idle context was charged only for the quanta nothing else wanted: it
            // joins the others at the floor, without credit.
            let floor = self.floor();
            self.admit(0, floor);
        }
        self.idle = idle;
    }

    /// The weight of `slot`.
    pub fn weight(&self, slot: usize) -> u32 {
        self.weight.get(slot).copied().unwrap_or(DEFAULT_WEIGHT)
    }

    /// Changes the weight of `slot` (clamped to 1–[`MAX_WEIGHT`]); it applies from the
    /// slot's next quantum.
    pub fn set_weight(&mut self, slot: usize, weight: u32) {
        if let Some(w) = self.weight.get_mut(slot) {
            *w = weight.clamp(1, MAX_WEIGHT);
        }
    }

    /// The pass `slot` advances per quantum.
    fn stride(&self, slot: usize) -> u64 {
        STRIDE_ONE / u64::from(self.weight(slot))
    }

    /// The lowest pass among the ready contexts (the idle one aside), or the running one's
    /// if none is ready: where a context that becomes ready starts at the earliest.
    fn floor(&self) -> u64 {
        let ready = (0..CAPACITY)
            .filter(|&i| !(self.idle && i == 0) && self.state.get(i) == Some(&State::Ready))
            .filter_map(|i| self.pass.get(i).copied())
            .min();
        ready.unwrap_or_else(|| self.pass.get(self.current).copied().unwrap_or(0))
    }

    /// Raises `slot`'s pass to at least `floor`: it is becoming ready.
    fn admit(&mut self, slot: usize, floor: u64) {
        if let Some(p) = self.pass.get_mut(slot) {
            *p = (*p).max(floor);
        }
    }

    /// The generation of the thread in `slot` (0 for slot 0, the boot context).
    pub fn generation(&self, slot: usize) -> u32 {
        self.generation.get(slot).copied().unwrap_or(0)
    }

    /// Contexts in the queue (ready or asleep), the boot context included.
    pub fn live(&self) -> usize {
        self.state.iter().filter(|s| **s != State::Free).count()
    }

    /// Context switches made so far.
    pub const fn switches(&self) -> u64 {
        self.switches
    }

    /// Timer ticks this queue has seen.
    pub const fn ticks(&self) -> u64 {
        self.ticks
    }

    /// The slot running now.
    pub const fn current(&self) -> usize {
        self.current
    }

    /// Saves the running context (charging it one quantum if `charge`), makes due sleepers
    /// ready, and returns the next context: the ready one with the lowest pass, ties going
    /// round-robin from the current one (possibly the same context).
    fn switch(&mut self, interrupted: u64, charge: bool) -> u64 {
        if let Some(slot) = self.saved.get_mut(self.current) {
            *slot = interrupted;
        }
        if charge {
            let stride = self.stride(self.current);
            if let Some(p) = self.pass.get_mut(self.current) {
                *p = p.saturating_add(stride);
            }
        }
        let now = self.ticks;
        let floor = self.floor();
        for i in 0..CAPACITY {
            let due = match self.state.get(i) {
                Some(&State::Sleeping(until) | &State::Blocked(Some(until))) => until <= now,
                _ => false,
            };
            if due {
                if let Some(s) = self.state.get_mut(i) {
                    *s = State::Ready;
                }
                self.admit(i, floor);
            }
        }
        let ready = |i: usize| self.state.get(i) == Some(&State::Ready);
        let others = (1..CAPACITY).any(ready);
        let next = self
            .lowest_pass(|i| ready(i) && !(self.idle && i == 0 && others))
            .unwrap_or(0);
        // Whatever was woken has had its say in this choice.
        self.woken = [false; CAPACITY];
        if next != self.current {
            self.switches += 1;
        }
        self.current = next;
        self.saved.get(next).copied().unwrap_or(interrupted)
    }

    /// Of the contexts `eligible` admits, the one with the lowest pass, ties going
    /// round-robin from the current one (so the current one wins only if alone at the lowest).
    fn lowest_pass(&self, eligible: impl Fn(usize) -> bool) -> Option<usize> {
        let mut found = None;
        let mut best = u64::MAX;
        for i in (1..=CAPACITY).map(|k| (self.current + k) % CAPACITY) {
            let pass = self.pass.get(i).copied().unwrap_or(u64::MAX);
            if eligible(i) && pass < best {
                best = pass;
                found = Some(i);
            }
        }
        found
    }

    /// An interrupt or IPI that may have woken a thread arrived while the context whose
    /// frame is at `interrupted` ran. That context is charged nothing. If a thread woken
    /// since the last choice should run now, it runs: the woken one with the lowest pass, if
    /// the running context is the idle one or its pass is no lower than that thread's (what a
    /// choice now would decide, the woken thread winning a tie). Otherwise `interrupted` is
    /// returned and nothing here changes.
    pub fn preempt_for_wake(&mut self, interrupted: u64) -> u64 {
        let woken = core::mem::replace(&mut self.woken, [false; CAPACITY]);
        let pass = |slot: usize| self.pass.get(slot).copied().unwrap_or(u64::MAX);
        let running = pass(self.current);
        let idle = self.idle && self.current == 0;
        let next = self
            .lowest_pass(|i| {
                woken.get(i) == Some(&true) && self.state.get(i) == Some(&State::Ready)
            })
            .filter(|&slot| idle || pass(slot) <= running);
        let Some(next) = next else {
            return interrupted;
        };
        if let Some(slot) = self.saved.get_mut(self.current) {
            *slot = interrupted;
        }
        self.switches += 1;
        self.current = next;
        self.saved.get(next).copied().unwrap_or(interrupted)
    }

    /// The timer interrupted the running context, whose frame is at `interrupted`: counts
    /// the tick, wakes sleepers that are due, and returns the frame to resume.
    pub fn tick(&mut self, interrupted: u64) -> u64 {
        self.ticks += 1;
        self.switch(interrupted, true)
    }

    /// The running context gives up the processor.
    pub fn yield_now(&mut self, interrupted: u64) -> u64 {
        // A yield gives up the rest of the quantum: charged like a tick, so equal-weight
        // contexts alternate.
        self.switch(interrupted, true)
    }

    /// The running context sleeps for `ticks` timer ticks (at least one).
    pub fn sleep(&mut self, interrupted: u64, ticks: u64) -> Result<u64, QueueError> {
        if self.current == 0 {
            return Err(QueueError::BootContext);
        }
        let until = self.ticks.saturating_add(ticks.max(1));
        if let Some(s) = self.state.get_mut(self.current) {
            *s = State::Sleeping(until);
        }
        Ok(self.switch(interrupted, false))
    }

    /// The running context blocks until [`RunQueue::wake`]. If a wake-up is already
    /// pending, it is consumed and the running context goes on (`interrupted` is returned).
    pub fn block(&mut self, interrupted: u64) -> Result<u64, QueueError> {
        self.block_for(interrupted, None)
    }

    /// As [`RunQueue::block`], but the context also becomes ready after `ticks` timer ticks
    /// (at least one) if no wake-up came first; `None` waits for ever. The caller tells the
    /// two apart by checking what it waited for.
    pub fn block_for(&mut self, interrupted: u64, ticks: Option<u64>) -> Result<u64, QueueError> {
        if self.current == 0 {
            return Err(QueueError::BootContext);
        }
        if let Some(pending) = self.wake_pending.get_mut(self.current)
            && *pending
        {
            *pending = false;
            return Ok(interrupted);
        }
        let until = ticks.map(|t| self.ticks.saturating_add(t.max(1)));
        if let Some(s) = self.state.get_mut(self.current) {
            *s = State::Blocked(until);
        }
        Ok(self.switch(interrupted, false))
    }

    /// Wakes the thread in `slot` if it is still the one `generation` names. A blocked
    /// thread becomes ready, and `true` is returned: the processor running this queue
    /// should reschedule. Any other live thread keeps the wake-up for its next
    /// [`RunQueue::block`]. A stale or free slot is left alone.
    pub fn wake(&mut self, slot: usize, generation: u32) -> bool {
        if slot == 0 || self.generation.get(slot) != Some(&generation) {
            return false;
        }
        match self.state.get(slot) {
            Some(State::Blocked(_)) => {
                let floor = self.floor();
                if let Some(s) = self.state.get_mut(slot) {
                    *s = State::Ready;
                }
                self.admit(slot, floor);
                if let Some(w) = self.woken.get_mut(slot) {
                    *w = true;
                }
                true
            }
            Some(State::Ready | State::Sleeping(_)) => {
                if let Some(p) = self.wake_pending.get_mut(slot) {
                    *p = true;
                }
                false
            }
            _ => false,
        }
    }

    /// The running context ends: its slot is freed (its stack is the caller's to reclaim)
    /// and the next ready context is returned.
    pub fn exit(&mut self, interrupted: u64) -> Result<u64, QueueError> {
        if self.current == 0 {
            return Err(QueueError::BootContext);
        }
        if let Some(s) = self.state.get_mut(self.current) {
            *s = State::Free;
        }
        Ok(self.switch(interrupted, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lone_context_keeps_running() {
        let mut q = RunQueue::new();
        assert_eq!(q.tick(0x1000), 0x1000);
        assert_eq!(q.yield_now(0x2000), 0x2000);
        assert_eq!(q.switches(), 0);
        assert_eq!(q.live(), 1);
        assert_eq!(q.sleep(0x2000, 3), Err(QueueError::BootContext));
        assert_eq!(q.exit(0x2000), Err(QueueError::BootContext));
    }

    #[test]
    fn round_robin_resumes_each_saved_frame_in_turn() {
        let mut q = RunQueue::new();
        assert_eq!(q.push(0xA000), Ok(1));
        assert_eq!(q.push(0xB000), Ok(2));
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(q.tick(0xA100), 0xB000);
        assert_eq!(q.tick(0xB200), 0x1000);
        assert_eq!(q.yield_now(0x1100), 0xA100);
        assert_eq!(q.tick(0xA300), 0xB200);
        assert_eq!(q.switches(), 5);
    }

    #[test]
    fn every_context_gets_an_equal_share() {
        let mut q = RunQueue::new();
        for i in 1..CAPACITY as u64 {
            q.push(i * 0x1000).unwrap();
        }
        assert_eq!(q.push(0x9_0000), Err(QueueError::Full));
        let mut runs = [0u32; CAPACITY];
        let mut running = 0u64;
        for _ in 0..CAPACITY * 100 {
            let next = q.tick(running);
            runs[(next / 0x1000) as usize] += 1;
            running = next;
        }
        assert!(runs.iter().all(|&r| r == 100), "{runs:?}");
    }

    #[test]
    fn sleepers_wake_after_their_ticks_and_not_before() {
        let mut q = RunQueue::new();
        q.push(0xA000).unwrap();
        assert_eq!(q.tick(0x1000), 0xA000); // tick 1: A runs
        assert_eq!(q.sleep(0xA100, 3), Ok(0x1000)); // A sleeps until tick 4
        assert_eq!(q.live(), 2);
        assert_eq!(q.tick(0x1100), 0x1100); // tick 2: only the boot context is ready
        assert_eq!(q.tick(0x1200), 0x1200); // tick 3
        assert_eq!(q.tick(0x1300), 0xA100, "tick 4: A is due again");
    }

    #[test]
    fn exit_frees_the_slot_for_reuse() {
        let mut q = RunQueue::new();
        q.push(0xA000).unwrap();
        q.push(0xB000).unwrap();
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(q.exit(0xA100), Ok(0xB000));
        assert_eq!(q.live(), 2);
        assert_eq!(q.push(0xC000), Ok(1), "A's slot is reused");
        // The new thread starts level with the others: equal shares from here on.
        let runs = run(&mut q, 30);
        assert_eq!((runs[0], runs[1], runs[2]), (10, 10, 10), "{runs:?}");
    }

    #[test]
    fn a_blocked_thread_runs_only_after_its_wake() {
        let mut q = RunQueue::new();
        let slot = q.push(0xA000).unwrap();
        let generation = q.generation(slot);
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(
            q.block(0xA100),
            Ok(0x1000),
            "A blocks; the boot context runs"
        );
        for t in 0..5 {
            assert_eq!(
                q.tick(0x1100 + t),
                0x1100 + t,
                "ticks do not wake a blocked thread"
            );
        }
        assert_eq!(q.live(), 2);
        assert!(q.wake(slot, generation), "a blocked thread: reschedule");
        assert_eq!(q.tick(0x1200), 0xA100, "A resumes where it blocked");
        assert!(!q.wake(slot, generation), "running: only a pending wake-up");
        assert_eq!(
            q.block(0xA200),
            Ok(0xA200),
            "the pending wake-up is consumed at once"
        );
        assert_eq!(q.block(0xA300), Ok(0x1200), "the next block really blocks");
    }

    #[test]
    fn a_timed_block_ends_at_its_deadline_or_at_a_wake_up() {
        let mut q = RunQueue::new();
        let slot = q.push(0xA000).unwrap();
        let generation = q.generation(slot);
        assert_eq!(q.tick(0x1000), 0xA000); // tick 1
        assert_eq!(
            q.block_for(0xA100, Some(3)),
            Ok(0x1000),
            "blocked until tick 4"
        );
        assert_eq!(q.tick(0x1100), 0x1100); // tick 2
        assert_eq!(q.tick(0x1200), 0x1200); // tick 3
        assert_eq!(q.tick(0x1300), 0xA100, "tick 4: the deadline passed");
        assert!(
            !q.wake(slot, generation),
            "running again: a late wake-up is only kept"
        );
        assert_eq!(
            q.block_for(0xA200, Some(100)),
            Ok(0xA200),
            "and consumed by the next block"
        );
        assert_eq!(q.block_for(0xA300, Some(100)), Ok(0x1300));
        assert!(q.wake(slot, generation), "a wake-up before the deadline");
        assert_eq!(q.tick(0x1400), 0xA300);
        assert_eq!(
            q.block_for(0xA400, Some(0)),
            Ok(0x1400),
            "zero ticks still waits one"
        );
        assert_eq!(q.tick(0x1500), 0xA400);
    }

    #[test]
    fn a_stale_wake_never_reaches_a_later_thread_in_the_slot() {
        let mut q = RunQueue::new();
        let slot = q.push(0xA000).unwrap();
        let old = q.generation(slot);
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(q.exit(0xA100), Ok(0x1000));
        assert!(!q.wake(slot, old), "free slot");
        assert_eq!(q.push(0xB000), Ok(slot));
        let new = q.generation(slot);
        assert_ne!(new, old);
        assert_eq!(q.tick(0x1100), 0xB000);
        assert!(!q.wake(slot, old), "the old thread's wake-up");
        assert_eq!(
            q.block(0xB100),
            Ok(0x1100),
            "and so no pending wake-up either"
        );
        assert!(q.wake(slot, new));
        assert!(!q.wake(0, 0), "the boot context is never blocked");
        assert_eq!(q.block(0x1200), Err(QueueError::BootContext));
    }

    /// Runs `ticks` ticks and counts how often each slot ran.
    fn run(q: &mut RunQueue, ticks: usize) -> [u32; CAPACITY] {
        let mut runs = [0u32; CAPACITY];
        let mut running = q.saved[q.current()];
        for _ in 0..ticks {
            running = q.tick(running);
            runs[q.current()] += 1;
        }
        runs
    }

    #[test]
    fn shares_follow_the_weights_exactly() {
        let mut q = RunQueue::new();
        q.set_idle(true);
        for (i, w) in [1u32, 4, 16].into_iter().enumerate() {
            q.push_weighted(0x1000 * (i as u64 + 1), w).unwrap();
        }
        // 21 quanta make one full cycle of strides 16 : 4 : 1.
        let runs = run(&mut q, 2100);
        assert_eq!(
            (runs[0], runs[1], runs[2], runs[3]),
            (0, 100, 400, 1600),
            "{runs:?}"
        );
        // Changing a weight changes the shares from then on.
        // Weights 16 : 4 : 16 make a 36-quantum cycle.
        q.set_weight(1, 16);
        let runs = run(&mut q, 3600);
        let (a, b, c) = (runs[1], runs[2], runs[3]);
        assert!(
            a.abs_diff(1600) <= 2 && b.abs_diff(400) <= 2 && c.abs_diff(1600) <= 2,
            "{runs:?}"
        );
    }

    #[test]
    fn the_idle_context_runs_only_when_nothing_else_is_ready() {
        let mut q = RunQueue::new();
        q.set_idle(true);
        let a = q.push(0xA000).unwrap();
        assert_eq!(run(&mut q, 50)[0], 0, "never while A is ready");
        let frame = q.saved[a];
        assert_eq!(
            q.sleep(frame, 10).map(|_| q.current()),
            Ok(0),
            "A sleeps: idle runs"
        );
        let runs = run(&mut q, 20);
        assert_eq!(
            (runs[0], runs[a]),
            (9, 11),
            "idle until A is due, then A only"
        );
        // Not idle: slot 0 takes its turn like any context.
        q.set_idle(false);
        let runs = run(&mut q, 20);
        assert_eq!((runs[0], runs[a]), (10, 10));
    }

    #[test]
    fn sleeping_earns_no_credit_and_new_threads_start_level() {
        let mut q = RunQueue::new();
        q.set_idle(true);
        let a = q.push(0xA000).unwrap();
        let b = q.push(0xB000).unwrap();
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(
            q.sleep(0xA100, 100),
            Ok(0xB000),
            "A sleeps until tick 101; B runs alone"
        );
        let runs = run(&mut q, 99);
        assert_eq!(runs[b], 99);
        // A is back: it does not get 100 quanta in a row to catch up; the two alternate.
        let runs = run(&mut q, 40);
        assert_eq!((runs[a], runs[b]), (20, 20), "{runs:?}");
        // A thread added now starts level with them: a third each.
        let c = q.push(0xC000).unwrap();
        let runs = run(&mut q, 300);
        assert_eq!((runs[a], runs[b], runs[c]), (100, 100, 100), "{runs:?}");
        // Weights are clamped.
        q.set_weight(c, 0);
        assert_eq!(q.weight(c), 1);
        q.set_weight(c, u32::MAX);
        assert_eq!(q.weight(c), MAX_WEIGHT);
    }

    #[test]
    fn the_share_bound_is_the_one_the_boot_proof_always_used() {
        assert_eq!(per_mille(1, 21), 47);
        assert_eq!(per_mille(4, 21), 190);
        assert_eq!(per_mille(16, 21), 761);
        assert_eq!(per_mille(0, 0), 0, "nothing of nothing is nothing");
        // Ideal 47: 17 to 77 per mille; 190: 112 to 268; 761: 493 to 1029.
        for (weight, low, high) in [(1, 17, 77), (4, 112, 268), (16, 493, 1029)] {
            assert!(share_near_weight(low, weight, 21), "{weight}: {low}");
            assert!(share_near_weight(high, weight, 21), "{weight}: {high}");
            assert!(!share_near_weight(low - 1, weight, 21), "{weight}: below");
            assert!(!share_near_weight(high + 1, weight, 21), "{weight}: above");
        }
    }

    #[test]
    fn a_disturbance_earns_a_repeat_and_never_a_pass() {
        assert_eq!(ROUND_TRIP_LIMIT_MICROS, 2000);
        // Under the bound passes, disturbed or not.
        assert_eq!(judge_round_trips(1999, 0), RoundTrips::Pass);
        assert_eq!(judge_round_trips(1999, 1000), RoundTrips::Pass);
        // At or over it, a run that shows no disturbance fails at once...
        assert_eq!(judge_round_trips(2000, 0), RoundTrips::Fail);
        assert_eq!(
            judge_round_trips(2000, DISTURBED_PER_MILLE - 1),
            RoundTrips::Fail
        );
        assert_eq!(judge_round_trips(50_000, 0), RoundTrips::Fail);
        // ... and one that shows a disturbance is measured again, however disturbed.
        assert_eq!(
            judge_round_trips(2000, DISTURBED_PER_MILLE),
            RoundTrips::Repeat
        );
        assert_eq!(judge_round_trips(50_000, 1000), RoundTrips::Repeat);
        // Over any mix of inputs, only a mean under the bound passes.
        for mean in [0, 1, 1999, 2000, 2001, 1_000_000, u64::MAX] {
            for away in [0, 99, 100, 101, 1000, u64::MAX] {
                assert_eq!(
                    judge_round_trips(mean, away) == RoundTrips::Pass,
                    mean < 2000
                );
            }
        }
        // The tenth of a run away that earns a repeat.
        assert_eq!(DISTURBED_PER_MILLE, 100);
        assert_eq!(per_mille(99, 1000), 99);
        assert_eq!(per_mille(100, 1000), DISTURBED_PER_MILLE);
    }

    #[test]
    fn an_interrupt_that_woke_nothing_charges_nothing_and_resumes_the_same_context() {
        let mut q = RunQueue::new();
        q.push(0xA000).unwrap();
        q.push(0xB000).unwrap();
        assert_eq!(q.tick(0x1000), 0xA000);
        let before = (q.switches(), q.pass);
        for _ in 0..1000 {
            assert_eq!(
                q.preempt_for_wake(0xA100),
                0xA100,
                "the same thread goes on"
            );
        }
        assert_eq!((q.switches(), q.pass), before, "no switch, nothing charged");
    }

    #[test]
    fn interrupts_change_neither_the_shares_nor_the_number_of_switches() {
        let mut quiet = RunQueue::new();
        let mut busy = RunQueue::new();
        for q in [&mut quiet, &mut busy] {
            q.set_idle(true);
            for (i, w) in [1u32, 4, 16].into_iter().enumerate() {
                q.push_weighted(0x1000 * (i as u64 + 1), w).unwrap();
            }
        }
        // 21 quanta make one full cycle of strides 16 : 4 : 1. On `busy`, the running thread
        // also takes ten interrupts every quantum, none of them waking anything.
        let mut runs = [0u32; CAPACITY];
        let mut running = busy.saved[busy.current()];
        for _ in 0..2100 {
            quiet.tick(quiet.saved[quiet.current()]);
            running = busy.tick(running);
            for _ in 0..10 {
                assert_eq!(busy.preempt_for_wake(running), running);
            }
            runs[busy.current()] += 1;
        }
        assert_eq!((runs[0], runs[1], runs[2], runs[3]), (0, 100, 400, 1600));
        assert_eq!(busy.switches(), quiet.switches());
        assert_eq!(busy.pass, quiet.pass);
    }

    #[test]
    fn a_woken_thread_that_should_run_now_takes_over_uncharged() {
        let mut q = RunQueue::new();
        q.set_idle(true);
        let a = q.push(0xA000).unwrap();
        q.push(0xB000).unwrap();
        let generation = q.generation(a);
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(q.block(0xA100), Ok(0xB000), "A blocks; B runs");
        let before = q.pass;
        assert!(q.wake(a, generation));
        assert_eq!(q.preempt_for_wake(0xB100), 0xA100, "A is level: it runs");
        assert_eq!(q.current(), a);
        assert_eq!(q.pass, before, "and neither of them is charged for it");
        // B's interrupted frame was saved: it is B's turn again after A blocks.
        assert_eq!(q.block(0xA200), Ok(0xB100));
    }

    #[test]
    fn a_woken_thread_behind_the_running_context_waits_for_its_turn() {
        let mut q = RunQueue::new();
        q.set_idle(true);
        let a = q.push(0xA000).unwrap();
        q.push(0xB000).unwrap();
        let generation = q.generation(a);
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(q.block(0xA100), Ok(0xB000));
        q.pass[a] = 5 * STRIDE_ONE; // A has used far more than B
        let switches = q.switches();
        assert!(q.wake(a, generation));
        assert_eq!(
            q.preempt_for_wake(0xB100),
            0xB100,
            "not ahead of B: B goes on"
        );
        assert_eq!(q.switches(), switches);
        // It is ready all the same: B runs until A's pass is the lowest.
        let mut running = 0xB100;
        for _ in 0..50 {
            running = q.tick(running);
            if q.current() == a {
                break;
            }
        }
        assert_eq!(q.current(), a, "A gets its turn at a tick");
    }

    #[test]
    fn the_idle_context_gives_way_to_any_woken_thread_whatever_its_pass() {
        let mut q = RunQueue::new();
        q.set_idle(true);
        let a = q.push(0xA000).unwrap();
        let generation = q.generation(a);
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(q.block(0xA100), Ok(0x1000), "A blocks; only idle is left");
        q.pass[a] = 1 << 40;
        q.pass[0] = 0;
        assert!(q.wake(a, generation));
        assert_eq!(q.preempt_for_wake(0x1100), 0xA100);
        assert_eq!(q.current(), a);
    }

    #[test]
    fn the_woken_thread_wins_a_tie_ahead_of_another_ready_one() {
        let mut q = RunQueue::new();
        q.set_idle(true);
        let woken = q.push(0xA000).unwrap();
        let running = q.push(0xB000).unwrap();
        let other = q.push(0xC000).unwrap();
        let generation = q.generation(woken);
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(q.block(0xA100), Ok(0xB000));
        assert_eq!(q.current(), running);
        assert_eq!(q.pass[running], q.pass[other]);
        assert!(q.wake(woken, generation));
        assert_eq!(q.pass[woken], q.pass[running]);
        // `other` comes first round-robin from `running`, but only `woken` was woken.
        assert_eq!(q.preempt_for_wake(0xB100), 0xA100);
        assert_eq!(q.current(), woken);
    }

    #[test]
    fn a_wake_up_a_later_choice_has_seen_does_not_preempt_again() {
        let mut q = RunQueue::new();
        q.set_idle(true);
        let a = q.push(0xA000).unwrap();
        q.push(0xB000).unwrap();
        let generation = q.generation(a);
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(q.block(0xA100), Ok(0xB000));
        assert!(q.wake(a, generation));
        // The next timer tick chooses before the IPI arrives; the IPI then finds nothing.
        q.tick(0xB100);
        let current = q.current();
        let switches = q.switches();
        assert_eq!(q.preempt_for_wake(0xF000), 0xF000);
        assert_eq!((q.current(), q.switches()), (current, switches));
    }

    #[test]
    fn bad_contexts_are_refused() {
        let mut q = RunQueue::new();
        assert_eq!(q.push(0), Err(QueueError::BadContext(0)));
        assert_eq!(q.push(0x1004), Err(QueueError::BadContext(0x1004)));
        assert_eq!(q.live(), 1);
    }
}
