//! The wait-queue protocol (CLAUDE.md §25 thread lifecycle, §26 thread wake-ups, §30 wait
//! objects): a kernel thread blocks until a condition holds, and whoever makes it hold wakes
//! it, on any processor.
//!
//! [`WaitQueue::wait_until`] checks the condition under the queue's lock and registers the
//! thread there before it blocks. A waker changes the state first, then takes the same lock
//! to pick a waiter. So either the waiter sees the change, or it is registered by the time
//! the waker looks; and a wake-up that reaches the thread just before it blocks is kept by
//! its run queue, so the block returns at once. No wake-up is lost.
//!
//! This is the protocol alone. The kernel supplies its scheduler and clock through
//! [`Scheduler`]; `framekernel/src/wait.rs` instantiates it for kernel threads. Generic so the
//! Loom models (`models.rs`) run this code against the real run queue. Lock order: a wait
//! queue's lock, then whatever its condition reads. A waker never holds a wait queue's lock
//! while it takes another one.

use crate::SpinLock;

/// What a wait queue needs from the kernel, in the calling thread's view.
pub trait Scheduler {
    /// Names a thread, so that any processor can wake it after it has stopped running.
    type Thread: Copy + Eq;

    /// The running thread, or `None` for a flow that cannot block (boot, idle), which polls
    /// by yielding instead.
    fn current_thread(&self) -> Option<Self::Thread>;

    /// Runs `f` with interrupts disabled on this processor.
    fn without_interrupts<R>(&self, f: impl FnOnce() -> R) -> R;

    /// Blocks the running thread until [`Scheduler::wake`] names it. Returns at once if a
    /// wake-up reached it first.
    fn block(&self);

    /// As [`Scheduler::block`], but also returns once the monotonic clock reaches `deadline`
    /// (nanoseconds). The caller tells the two apart by checking what it waited for.
    fn block_until(&self, deadline: u64);

    /// Gives up the processor to the next ready context on this CPU.
    fn yield_now(&self);

    /// Makes `thread` ready, on whichever processor it runs.
    fn wake(&self, thread: Self::Thread);

    /// The monotonic clock, in nanoseconds.
    fn monotonic_nanos(&self) -> u64;
}

/// A timed wait whose deadline passed with its condition still false.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimedOut;

/// What one round of a timed wait decided.
enum Step {
    /// The condition held (`true`), or the deadline passed (`false`).
    Finished(bool),
    Block,
    Poll,
}

/// Threads waiting for one condition. `N` is how many one queue can hold: a thread that finds
/// it full polls by yielding instead.
pub struct WaitQueue<T, const N: usize> {
    waiters: SpinLock<[Option<T>; N]>,
}

impl<T, const N: usize> core::fmt::Debug for WaitQueue<T, N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WaitQueue").finish_non_exhaustive()
    }
}

impl<T: Copy + Eq, const N: usize> Default for WaitQueue<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + Eq, const N: usize> WaitQueue<T, N> {
    /// A queue with no waiters.
    #[cfg(not(all(test, loom)))]
    pub const fn new() -> Self {
        Self {
            waiters: SpinLock::new([None; N]),
        }
    }

    /// A queue with no waiters (under Loom, whose lock cannot be built in a `const`).
    #[cfg(all(test, loom))]
    pub fn new() -> Self {
        Self {
            waiters: SpinLock::new([None; N]),
        }
    }

    /// Returns once `condition()` holds, blocking the running kernel thread meanwhile.
    /// `condition` runs under this queue's lock, with interrupts disabled. Returns how often
    /// the thread blocked. The boot or idle flow, which cannot block, polls by yielding.
    pub fn wait_until<S: Scheduler<Thread = T>>(
        &self,
        scheduler: &S,
        condition: impl Fn() -> bool,
    ) -> u64 {
        let me = scheduler.current_thread();
        let mut blocked = 0;
        loop {
            let registered = scheduler.without_interrupts(|| {
                let mut waiters = self.waiters.lock();
                if condition() {
                    // Leave no registration behind (a wake-up that raced with the
                    // condition becoming true is harmless: the next block consumes it).
                    if let Some(slot) = waiters.iter_mut().find(|w| **w == me && me.is_some()) {
                        *slot = None;
                    }
                    return None;
                }
                let Some(thread) = me else {
                    return Some(false);
                };
                if waiters.contains(&Some(thread)) {
                    return Some(true);
                }
                match waiters.iter_mut().find(|w| w.is_none()) {
                    Some(free) => {
                        *free = Some(thread);
                        Some(true)
                    }
                    None => Some(false),
                }
            });
            match registered {
                None => return blocked,
                Some(true) => {
                    scheduler.block();
                    blocked += 1;
                }
                Some(false) => scheduler.yield_now(),
            }
        }
    }

    /// As [`WaitQueue::wait_until`], but gives up when the monotonic clock reaches
    /// `deadline` (nanoseconds) with the condition still false: `Err(TimedOut)`. The
    /// deadline is kept to within one timer tick (10 ms).
    pub fn wait_until_deadline<S: Scheduler<Thread = T>>(
        &self,
        scheduler: &S,
        condition: impl Fn() -> bool,
        deadline: u64,
    ) -> Result<u64, TimedOut> {
        let me = scheduler.current_thread();
        let mut blocked = 0;
        loop {
            let step = scheduler.without_interrupts(|| {
                let mut waiters = self.waiters.lock();
                let done = condition();
                if done || scheduler.monotonic_nanos() >= deadline {
                    if let Some(slot) = waiters.iter_mut().find(|w| **w == me && me.is_some()) {
                        *slot = None;
                    }
                    return Step::Finished(done);
                }
                let Some(thread) = me else {
                    return Step::Poll;
                };
                if waiters.contains(&Some(thread)) {
                    return Step::Block;
                }
                match waiters.iter_mut().find(|w| w.is_none()) {
                    Some(free) => {
                        *free = Some(thread);
                        Step::Block
                    }
                    None => Step::Poll,
                }
            });
            match step {
                Step::Finished(true) => return Ok(blocked),
                Step::Finished(false) => return Err(TimedOut),
                Step::Block => {
                    scheduler.block_until(deadline);
                    blocked += 1;
                }
                Step::Poll => scheduler.yield_now(),
            }
        }
    }

    /// Wakes one waiter, if there is one; returns whether there was.
    pub fn wake_one<S: Scheduler<Thread = T>>(&self, scheduler: &S) -> bool {
        let thread =
            scheduler.without_interrupts(|| self.waiters.lock().iter_mut().find_map(Option::take));
        if let Some(t) = thread {
            scheduler.wake(t);
        }
        thread.is_some()
    }
}
