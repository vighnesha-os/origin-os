//! Text waiting for one console sink: a bounded ring of bytes, with a position for every line
//! (ADR-0010 Amendment 1; CLAUDE.md §69).
//!
//! The console hands a line over in one short critical section, and each sink then works
//! through it a bounded piece at a time with nothing held in between: the screen in
//! [`crate::fbconsole::Backlog`], the UART in [`crate::serial::TxQueue`]. Each keeps its own
//! queue, so a slow sink never makes the other lose its place.
//!
//! Positions count bytes from the start of time. [`LineQueue::push`] returns the position the
//! sink must reach for that text to be complete, so a writer can wait for its own line whoever
//! else is queued behind it, and whichever processor happens to do the work.
//!
//! The caller serializes everything. Nothing here waits and nothing here can panic.

/// Text accepted and not yet taken, oldest first.
#[derive(Debug)]
pub struct LineQueue<const N: usize> {
    /// A ring of bytes, `len` of them from index `start`.
    text: [u8; N],
    start: usize,
    len: usize,
    /// Bytes ever accepted.
    queued: u64,
    /// A line was refused: the next one that fits says so first.
    lost: bool,
    /// What it says.
    notice: &'static [u8],
}

impl<const N: usize> LineQueue<N> {
    /// An empty queue, which writes `notice` in place of lines it had no room for.
    pub const fn new(notice: &'static [u8]) -> Self {
        const { assert!(N > 0, "a queue needs room for at least one byte") };
        Self {
            text: [0; N],
            start: 0,
            len: 0,
            queued: 0,
            lost: false,
            notice,
        }
    }

    /// Whether [`Self::push`] would take `bytes` more bytes now.
    pub const fn has_room(&self, bytes: usize) -> bool {
        let notice = if self.lost { self.notice.len() } else { 0 };
        N - self.len >= notice.saturating_add(bytes)
    }

    /// Accepts `text`, whole or not at all, and returns the position at which it will be
    /// complete. `None` when there is no room.
    pub fn push(&mut self, text: &[u8]) -> Option<u64> {
        if !self.has_room(text.len()) {
            return None;
        }
        if self.lost {
            self.lost = false;
            self.push_bytes(self.notice);
        }
        self.push_bytes(text);
        Some(self.queued)
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if let Some(slot) = self.text.get_mut((self.start + self.len) % N) {
                *slot = byte;
                self.len += 1;
                self.queued += 1;
            }
        }
    }

    /// Records that a line had to be dropped because a caller could not wait for room: the
    /// next line that fits is preceded by the notice.
    pub const fn note_lost(&mut self) {
        self.lost = true;
    }

    /// The oldest byte, left where it is.
    pub fn peek(&self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        self.text.get(self.start).copied()
    }

    /// Takes the oldest byte.
    pub fn pop(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.start = (self.start + 1) % N;
        self.len -= 1;
        Some(byte)
    }

    /// Whether nothing is waiting.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Bytes ever accepted: the position the sink reaches when nothing is left.
    pub const fn queued(&self) -> u64 {
        self.queued
    }

    /// Forgets everything waiting. Positions already given out stay valid: nothing is
    /// left to wait for.
    pub const fn clear(&mut self) {
        self.start = 0;
        self.len = 0;
        self.lost = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTICE: &[u8] = b"lost\n";

    fn drain<const N: usize>(queue: &mut LineQueue<N>) -> Vec<u8> {
        core::iter::from_fn(|| queue.pop()).collect()
    }

    #[test]
    fn text_comes_back_in_order_across_the_end_of_the_ring() {
        let mut queue = LineQueue::<8>::new(NOTICE);
        assert_eq!(queue.push(b"abcde"), Some(5));
        assert_eq!(queue.peek(), Some(b'a'));
        assert_eq!(queue.pop(), Some(b'a'));
        assert_eq!(queue.pop(), Some(b'b'));
        assert_eq!(queue.push(b"fghij"), Some(10));
        assert_eq!(drain(&mut queue), b"cdefghij");
        assert_eq!(queue.peek(), None);
        assert!(queue.is_empty());
        assert_eq!(queue.queued(), 10);
    }

    #[test]
    fn a_line_is_taken_whole_or_not_at_all() {
        let mut queue = LineQueue::<8>::new(NOTICE);
        assert_eq!(queue.push(b"abcdef"), Some(6));
        assert!(queue.has_room(2));
        assert!(!queue.has_room(3));
        assert_eq!(queue.push(b"ghi"), None);
        assert_eq!(queue.queued(), 6, "nothing of the refused line was taken");
        assert_eq!(drain(&mut queue), b"abcdef");
    }

    #[test]
    fn a_loss_is_owned_up_to_once_before_the_next_line_that_fits() {
        let mut queue = LineQueue::<16>::new(NOTICE);
        queue.note_lost();
        // The notice needs room too: 5 + 12 is more than 16, and neither goes in.
        assert!(!queue.has_room(12));
        assert_eq!(queue.push(b"twelve bytes"), None);
        assert_eq!(queue.push(b"short\n"), Some(11));
        assert_eq!(queue.push(b"next\n"), Some(16));
        assert_eq!(drain(&mut queue), b"lost\nshort\nnext\n");
    }

    #[test]
    fn clearing_forgets_the_text_and_keeps_the_count() {
        let mut queue = LineQueue::<8>::new(NOTICE);
        queue.push(b"abc");
        queue.note_lost();
        queue.clear();
        assert!(queue.is_empty());
        assert_eq!(queue.queued(), 3);
        assert_eq!(queue.push(b"d"), Some(4), "the loss went with the text");
        assert_eq!(drain(&mut queue), b"d");
    }
}
