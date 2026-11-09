//! A fixed-capacity ring buffer (CLAUDE.md §56 input queues, §55 audio buffering, §69
//! structured-log retention). A bounded FIFO over a pre-sized backing array: `push` fails
//! when full, `force_push` overwrites the oldest (the behaviour a keyboard or log ring
//! wants — never block, drop the stalest), and `pop`/`peek` take from the front. No
//! reallocation after construction, so it is safe to use on a bounded kernel budget. Pure,
//! no `unsafe`; the backing vector is allocated once.

extern crate alloc;
use alloc::vec::Vec;

/// A bounded FIFO of `capacity` elements.
pub struct Ring<T> {
    buf: Vec<T>,
    head: usize,
    len: usize,
}

impl<T: Copy + Default> Ring<T> {
    /// A ring holding up to `capacity` elements (at least 1). The backing array is allocated
    /// once, here.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            buf: alloc::vec![T::default(); capacity],
            head: 0,
            len: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn is_full(&self) -> bool {
        self.len == self.buf.len()
    }

    fn slot(&self, i: usize) -> usize {
        // `head + i` is at most `head + len <= head + capacity < 2 * capacity`, so one
        // subtraction (cheaper than `%`) brings it into range.
        let raw = self.head + i;
        if raw >= self.buf.len() {
            raw - self.buf.len()
        } else {
            raw
        }
    }

    /// Appends `item`; returns `false` (without storing it) when the ring is full.
    pub fn push(&mut self, item: T) -> bool {
        if self.is_full() {
            return false;
        }
        let at = self.slot(self.len);
        self.buf[at] = item;
        self.len += 1;
        true
    }

    /// Appends `item`, dropping and returning the oldest element when the ring is full.
    pub fn force_push(&mut self, item: T) -> Option<T> {
        let dropped = if self.is_full() { self.pop() } else { None };
        let at = self.slot(self.len);
        self.buf[at] = item;
        self.len += 1;
        dropped
    }

    /// Removes and returns the oldest element, or `None` when empty.
    pub fn pop(&mut self) -> Option<T> {
        if self.is_empty() {
            return None;
        }
        let item = self.buf[self.head];
        self.head = self.slot(1);
        self.len -= 1;
        Some(item)
    }

    /// The oldest element without removing it.
    pub fn peek(&self) -> Option<T> {
        if self.is_empty() {
            None
        } else {
            Some(self.buf[self.head])
        }
    }

    /// Discards all elements (keeping the capacity).
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
}

impl<T> core::fmt::Debug for Ring<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Ring")
            .field("len", &self.len)
            .field("capacity", &self.buf.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use alloc::collections::VecDeque;

    #[test]
    fn push_pop_fifo_order_and_fullness() {
        let mut r = Ring::<u32>::new(3);
        assert!(r.is_empty() && !r.is_full() && r.capacity() == 3);
        assert!(r.push(1) && r.push(2) && r.push(3));
        assert!(r.is_full());
        assert!(!r.push(4), "a full ring refuses push");
        assert_eq!(r.peek(), Some(1));
        assert_eq!(r.pop(), Some(1));
        assert_eq!(r.pop(), Some(2));
        assert!(r.push(4), "room again after popping");
        assert_eq!(r.pop(), Some(3));
        assert_eq!(r.pop(), Some(4));
        assert_eq!(r.pop(), None);
    }

    #[test]
    fn force_push_drops_the_oldest() {
        let mut r = Ring::<u8>::new(3);
        assert_eq!(r.force_push(1), None);
        assert_eq!(r.force_push(2), None);
        assert_eq!(r.force_push(3), None);
        assert_eq!(r.force_push(4), Some(1), "oldest dropped");
        assert_eq!(r.force_push(5), Some(2));
        // The ring now holds 3, 4, 5.
        assert_eq!(r.pop(), Some(3));
        assert_eq!(r.pop(), Some(4));
        assert_eq!(r.pop(), Some(5));
        assert!(r.is_empty());
    }

    #[test]
    fn capacity_one_and_clear() {
        let mut r = Ring::<u32>::new(0); // clamped to 1
        assert_eq!(r.capacity(), 1);
        assert!(r.push(7));
        assert!(r.is_full());
        assert_eq!(r.force_push(8), Some(7));
        r.clear();
        assert!(r.is_empty() && r.pop().is_none());
    }

    /// Model-based test: thousands of random operations must track a `VecDeque` with the
    /// same bounded, drop-oldest policy.
    #[test]
    fn matches_a_vecdeque_model() {
        const CAP: usize = 8;
        let mut r = Ring::<u32>::new(CAP);
        let mut model: VecDeque<u32> = VecDeque::new();
        let mut x = 0x1357_9BDF_2468_ACE0u64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };

        for _ in 0..80_000 {
            let v = next() as u32;
            match next() % 5 {
                0 | 1 => {
                    let ok = r.push(v);
                    if model.len() < CAP {
                        assert!(ok);
                        model.push_back(v);
                    } else {
                        assert!(!ok);
                    }
                }
                2 => {
                    let dropped = r.force_push(v);
                    let expect = if model.len() == CAP {
                        model.pop_front()
                    } else {
                        None
                    };
                    assert_eq!(dropped, expect);
                    model.push_back(v);
                }
                3 => assert_eq!(r.pop(), model.pop_front()),
                _ => assert_eq!(r.peek(), model.front().copied()),
            }
            assert_eq!(r.len(), model.len());
            assert_eq!(r.is_full(), model.len() == CAP);
        }
    }
}
