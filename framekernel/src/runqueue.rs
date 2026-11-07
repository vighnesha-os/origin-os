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
//! Blocking: a thread [`RunQueue::block`]s until another [`RunQueue::wake`]s it. A wake-up
//! names its thread by slot and the slot's generation (bumped by every [`RunQueue::push`]),
//! so a stale one never wakes a later thread in the same slot. A wake-up that arrives
//! before its thread has blocked (it registered with a wait queue and is about to block) is
//! kept, and its next `block` returns at once: no wake-up is lost.

/// Contexts one queue holds, its boot context included.
pub const CAPACITY: usize = 16;

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
    /// Waiting for a [`RunQueue::wake`].
    Blocked,
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
            current: 0,
            ticks: 0,
            switches: 0,
        }
    }

    /// Adds a thread whose initial frame is saved at `context`; returns its slot.
    pub fn push(&mut self, context: u64) -> Result<usize, QueueError> {
        if context == 0 || !context.is_multiple_of(8) {
            return Err(QueueError::BadContext(context));
        }
        let slot = (1..CAPACITY)
            .find(|&i| self.state.get(i) == Some(&State::Free))
            .ok_or(QueueError::Full)?;
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
        Ok(slot)
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

    /// Saves the running context and returns the next ready one in round-robin order
    /// (possibly the same).
    fn switch(&mut self, interrupted: u64) -> u64 {
        if let Some(slot) = self.saved.get_mut(self.current) {
            *slot = interrupted;
        }
        let now = self.ticks;
        for s in &mut self.state {
            if let State::Sleeping(until) = *s
                && until <= now
            {
                *s = State::Ready;
            }
        }
        let next = (1..=CAPACITY)
            .map(|k| (self.current + k) % CAPACITY)
            .find(|&i| self.state.get(i) == Some(&State::Ready))
            .unwrap_or(0);
        if next != self.current {
            self.switches += 1;
        }
        self.current = next;
        self.saved.get(next).copied().unwrap_or(interrupted)
    }

    /// The timer interrupted the running context, whose frame is at `interrupted`: counts
    /// the tick, wakes sleepers that are due, and returns the frame to resume.
    pub fn tick(&mut self, interrupted: u64) -> u64 {
        self.ticks += 1;
        self.switch(interrupted)
    }

    /// The running context gives up the processor.
    pub fn yield_now(&mut self, interrupted: u64) -> u64 {
        self.switch(interrupted)
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
        Ok(self.switch(interrupted))
    }

    /// The running context blocks until [`RunQueue::wake`]. If a wake-up is already
    /// pending, it is consumed and the running context goes on (`interrupted` is returned).
    pub fn block(&mut self, interrupted: u64) -> Result<u64, QueueError> {
        if self.current == 0 {
            return Err(QueueError::BootContext);
        }
        if let Some(pending) = self.wake_pending.get_mut(self.current)
            && *pending
        {
            *pending = false;
            return Ok(interrupted);
        }
        if let Some(s) = self.state.get_mut(self.current) {
            *s = State::Blocked;
        }
        Ok(self.switch(interrupted))
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
            Some(State::Blocked) => {
                if let Some(s) = self.state.get_mut(slot) {
                    *s = State::Ready;
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
        Ok(self.switch(interrupted))
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
        assert_eq!(q.tick(0xB100), 0x1000);
        assert_eq!(q.tick(0x1100), 0xC000);
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

    #[test]
    fn bad_contexts_are_refused() {
        let mut q = RunQueue::new();
        assert_eq!(q.push(0), Err(QueueError::BadContext(0)));
        assert_eq!(q.push(0x1004), Err(QueueError::BadContext(0x1004)));
        assert_eq!(q.live(), 1);
    }
}
