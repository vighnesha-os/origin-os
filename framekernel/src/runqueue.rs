//! A per-CPU round-robin run queue of saved contexts (M3, first scheduler; CLAUDE.md §26:
//! "begin with a correct per-CPU scheduler", "keep a safe scheduler fallback").
//!
//! A context is the stack pointer at which an interrupted thread's full register frame is
//! saved: the timer interrupt saves the running thread's frame on its own stack, and the
//! queue answers which saved frame to resume. Slot 0 is the context that was running when
//! the queue started (the CPU's boot flow or idle loop); its stack pointer becomes known at
//! the first switch. This is policy only: plain integers, no `unsafe`, tested on the host.
//! Priorities, fairness accounting and load balancing come later (an EEVDF-style design is
//! to be evaluated once this baseline passes, §26).

/// Contexts one queue holds, its boot context included.
pub const CAPACITY: usize = 16;

/// Why a thread could not be queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueError {
    /// The queue holds [`CAPACITY`] contexts.
    Full,
    /// A saved context's stack pointer is not 8-byte aligned or is null.
    BadContext(u64),
}

/// One CPU's run queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunQueue {
    saved: [u64; CAPACITY],
    len: usize,
    current: usize,
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
        Self {
            saved: [0; CAPACITY],
            len: 1,
            current: 0,
            switches: 0,
        }
    }

    /// Adds a thread whose initial frame is saved at `context`.
    pub fn push(&mut self, context: u64) -> Result<(), QueueError> {
        if context == 0 || !context.is_multiple_of(8) {
            return Err(QueueError::BadContext(context));
        }
        let slot = self.saved.get_mut(self.len).ok_or(QueueError::Full)?;
        *slot = context;
        self.len += 1;
        Ok(())
    }

    /// Contexts in the queue, the running one included.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Always false: the running context is always in the queue.
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// Context switches made so far.
    pub const fn switches(&self) -> u64 {
        self.switches
    }

    /// The timer interrupted the running context, whose frame is saved at `interrupted`.
    /// Records it and returns the frame to resume: the next context in round-robin order,
    /// or `interrupted` itself when it is alone.
    pub fn tick(&mut self, interrupted: u64) -> u64 {
        if let Some(slot) = self.saved.get_mut(self.current) {
            *slot = interrupted;
        }
        if self.len <= 1 {
            return interrupted;
        }
        self.current = (self.current + 1) % self.len;
        self.switches += 1;
        self.saved.get(self.current).copied().unwrap_or(interrupted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lone_context_keeps_running() {
        let mut q = RunQueue::new();
        assert_eq!(q.tick(0x1000), 0x1000);
        assert_eq!(q.tick(0x2000), 0x2000);
        assert_eq!(q.switches(), 0);
        assert_eq!(q.len(), 1);
        assert!(!q.is_empty());
    }

    #[test]
    fn round_robin_resumes_each_saved_frame_in_turn() {
        let mut q = RunQueue::new();
        q.push(0xA000).unwrap();
        q.push(0xB000).unwrap();
        // The boot context is interrupted at 0x1000: thread A runs.
        assert_eq!(q.tick(0x1000), 0xA000);
        // A is interrupted with its frame now at 0xA100: B runs.
        assert_eq!(q.tick(0xA100), 0xB000);
        // B at 0xB200: back to the boot context, at the frame saved first.
        assert_eq!(q.tick(0xB200), 0x1000);
        // Then A resumes exactly where it was interrupted.
        assert_eq!(q.tick(0x1100), 0xA100);
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
        let mut running = 0u64; // the frame of the context currently on the CPU
        for _ in 0..CAPACITY * 100 {
            let next = q.tick(running);
            runs[(next / 0x1000) as usize] += 1;
            running = next;
        }
        assert!(runs.iter().all(|&r| r == 100), "{runs:?}");
    }

    #[test]
    fn bad_contexts_are_refused() {
        let mut q = RunQueue::new();
        assert_eq!(q.push(0), Err(QueueError::BadContext(0)));
        assert_eq!(q.push(0x1004), Err(QueueError::BadContext(0x1004)));
        assert_eq!(q.len(), 1);
    }
}
