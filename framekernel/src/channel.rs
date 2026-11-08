//! The logic of one channel (ADR-0005 P8): two ends, a bounded FIFO toward each, messages of
//! bytes and transferred objects. Generic over what a transferred handle is, safe, tested on
//! the host; the kernel wraps it in a lock and its endpoints in reference-counted objects.
//!
//! The queue toward each end is bounded in messages and in bytes, so a sender that outruns
//! its reader gets [`Error::ShouldWait`] rather than growing the kernel's memory (backpressure,
//! P8). Queue storage is taken once at creation, with `try_reserve`: writing never allocates.
//! Message limits are the ABI's; the depth and byte bound are this kernel's choice.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use origin_abi::limits::{MAX_MESSAGE_BYTES, MAX_MESSAGE_HANDLES};
use origin_abi::{Error, signals};

/// Messages one queue holds.
pub const QUEUE_DEPTH: usize = 16;
/// Bytes one queue holds: four full-size messages. Always at least one full-size message,
/// or a maximal write could never succeed.
pub const QUEUE_BYTES: usize = 4 * MAX_MESSAGE_BYTES;

/// One message: bytes and the objects moved with it.
#[derive(Debug, PartialEq, Eq)]
pub struct Message<H> {
    /// The payload, at most 64 KiB.
    pub bytes: Vec<u8>,
    /// Transferred objects, at most 64.
    pub handles: Vec<H>,
}

/// Why a read returned no message.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadError {
    /// Empty and the peer is open ([`Error::ShouldWait`]), or empty and the peer closed
    /// ([`Error::PeerClosed`]).
    Status(Error),
    /// The next message does not fit; it stays queued. Its sizes are given.
    TooSmall {
        /// Bytes the caller must make room for.
        bytes: usize,
        /// Handles the caller must make room for.
        handles: usize,
    },
}

/// A channel. Side 0 and side 1 are its two ends; `queues[s]` holds the messages waiting
/// to be read at side `s`.
#[derive(Debug)]
pub struct Channel<H> {
    queues: [VecDeque<Message<H>>; 2],
    open: [bool; 2],
    /// Bytes queued toward each side.
    queued_bytes: [usize; 2],
    depth: usize,
    byte_limit: usize,
}

/// The other side.
pub const fn other(side: usize) -> usize {
    (side & 1) ^ 1
}

impl<H> Channel<H> {
    /// A channel whose queues hold at most `depth` messages and `byte_limit` bytes each. The
    /// queues' storage is taken now, so later writes never allocate.
    pub fn try_new(depth: usize, byte_limit: usize) -> Result<Self, Error> {
        let depth = depth.max(1);
        let mut queues = [VecDeque::new(), VecDeque::new()];
        for q in &mut queues {
            q.try_reserve_exact(depth).map_err(|_| Error::NoMemory)?;
        }
        Ok(Self {
            queues,
            open: [true, true],
            queued_bytes: [0, 0],
            depth,
            byte_limit: byte_limit.max(MAX_MESSAGE_BYTES),
        })
    }

    /// Whether side `from` could write a message of `len` bytes now.
    pub fn can_write(&self, from: usize, len: usize) -> Result<(), Error> {
        let to = other(from);
        if !self.open.get(to).copied().unwrap_or(false) {
            return Err(Error::PeerClosed);
        }
        let queued = self.queued_bytes.get(to).copied().unwrap_or(usize::MAX);
        if self.queues.get(to).is_none_or(|q| q.len() >= self.depth)
            || queued.saturating_add(len) > self.byte_limit
        {
            return Err(Error::ShouldWait);
        }
        Ok(())
    }

    /// Queues `message` for the other side. On failure it is handed back unchanged.
    pub fn write(&mut self, from: usize, message: Message<H>) -> Result<(), (Error, Message<H>)> {
        if message.bytes.len() > MAX_MESSAGE_BYTES || message.handles.len() > MAX_MESSAGE_HANDLES {
            return Err((Error::InvalidArgs, message));
        }
        if let Err(e) = self.can_write(from, message.bytes.len()) {
            return Err((e, message));
        }
        let to = other(from);
        if let Some(queued) = self.queued_bytes.get_mut(to) {
            *queued = queued.saturating_add(message.bytes.len());
        }
        match self.queues.get_mut(to) {
            Some(q) => {
                q.push_back(message);
                Ok(())
            }
            None => Err((Error::InvalidArgs, message)),
        }
    }

    /// The sizes (bytes, handles) of the next message for side `at`; an empty queue is
    /// [`Error::ShouldWait`], or [`Error::PeerClosed`] once the peer has closed.
    pub fn peek(&self, at: usize) -> Result<(usize, usize), Error> {
        let peer_open = self.open.get(other(at)).copied().unwrap_or(false);
        match self.queues.get(at & 1).and_then(VecDeque::front) {
            Some(next) => Ok((next.bytes.len(), next.handles.len())),
            None if peer_open => Err(Error::ShouldWait),
            None => Err(Error::PeerClosed),
        }
    }

    /// Takes the next message for side `at` if it fits in `byte_capacity` and
    /// `handle_capacity`.
    pub fn read(
        &mut self,
        at: usize,
        byte_capacity: usize,
        handle_capacity: usize,
    ) -> Result<Message<H>, ReadError> {
        let (bytes, handles) = self.peek(at).map_err(ReadError::Status)?;
        if bytes > byte_capacity || handles > handle_capacity {
            return Err(ReadError::TooSmall { bytes, handles });
        }
        if let Some(queued) = self.queued_bytes.get_mut(at & 1) {
            *queued = queued.saturating_sub(bytes);
        }
        self.queues
            .get_mut(at & 1)
            .and_then(VecDeque::pop_front)
            .ok_or(ReadError::Status(Error::ShouldWait))
    }

    /// Closes side `side`. Returns the messages that were waiting to be read there, which
    /// the caller drops outside any lock (they may hold objects whose drop takes locks).
    pub fn close(&mut self, side: usize) -> VecDeque<Message<H>> {
        if let Some(o) = self.open.get_mut(side & 1) {
            *o = false;
        }
        if let Some(q) = self.queued_bytes.get_mut(side & 1) {
            *q = 0;
        }
        self.queues
            .get_mut(side & 1)
            .map(core::mem::take)
            .unwrap_or_default()
    }

    /// The signals side `side` sees. `WRITABLE` means a maximal message would fit, so a
    /// writer that waits for it can always make progress.
    pub fn signals(&self, side: usize) -> u32 {
        let mut s = 0;
        if self.queues.get(side & 1).is_some_and(|q| !q.is_empty()) {
            s |= signals::READABLE;
        }
        if self.can_write(side, MAX_MESSAGE_BYTES).is_ok() {
            s |= signals::WRITABLE;
        }
        if !self.open.get(other(side)).copied().unwrap_or(false) {
            s |= signals::PEER_CLOSED;
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Rng, cases};
    use std::collections::VecDeque as Deque;

    fn msg(b: &[u8], h: &[u32]) -> Message<u32> {
        Message {
            bytes: b.to_vec(),
            handles: h.to_vec(),
        }
    }

    fn channel(depth: usize) -> Channel<u32> {
        Channel::try_new(depth, QUEUE_BYTES).unwrap()
    }

    #[test]
    fn messages_arrive_in_order_on_the_other_side_with_their_handles() {
        let mut c = channel(4);
        c.write(0, msg(b"one", &[7])).unwrap();
        c.write(0, msg(b"two", &[])).unwrap();
        assert_eq!(
            c.read(0, 64, 4),
            Err(ReadError::Status(Error::ShouldWait)),
            "own side"
        );
        assert_eq!(c.read(1, 64, 4), Ok(msg(b"one", &[7])));
        assert_eq!(c.read(1, 64, 4), Ok(msg(b"two", &[])));
        assert_eq!(c.read(1, 64, 4), Err(ReadError::Status(Error::ShouldWait)));
    }

    #[test]
    fn signals_follow_the_queues_and_the_peer() {
        let mut c: Channel<u32> = channel(1);
        assert_eq!(c.signals(1), signals::WRITABLE);
        c.write(0, msg(b"x", &[])).unwrap();
        assert_eq!(c.signals(1), signals::READABLE | signals::WRITABLE);
        assert_eq!(c.signals(0), 0, "side 0's outgoing queue is full");
        assert_eq!(
            c.write(0, msg(b"y", &[])),
            Err((Error::ShouldWait, msg(b"y", &[])))
        );
        let undelivered = c.close(1);
        assert_eq!(
            undelivered.len(),
            1,
            "the unread message comes back for dropping"
        );
        assert_eq!(c.signals(0), signals::PEER_CLOSED);
        assert_eq!(c.write(0, msg(b"z", &[])).unwrap_err().0, Error::PeerClosed);
        assert_eq!(c.read(0, 8, 0), Err(ReadError::Status(Error::PeerClosed)));
    }

    #[test]
    fn a_small_buffer_leaves_the_message_queued_and_gives_its_sizes() {
        let mut c = channel(2);
        c.write(1, msg(b"hello", &[1, 2])).unwrap();
        assert_eq!(
            c.read(0, 4, 2),
            Err(ReadError::TooSmall {
                bytes: 5,
                handles: 2
            })
        );
        assert_eq!(
            c.read(0, 5, 1),
            Err(ReadError::TooSmall {
                bytes: 5,
                handles: 2
            })
        );
        assert_eq!(c.peek(0), Ok((5, 2)));
        assert_eq!(c.read(0, 5, 2), Ok(msg(b"hello", &[1, 2])));
    }

    #[test]
    fn peek_reports_an_empty_queue_and_a_closed_peer() {
        let mut c = channel(2);
        assert_eq!(c.peek(1), Err(Error::ShouldWait));
        c.write(0, msg(b"last", &[])).unwrap();
        drop(c.close(0));
        // What was queued before the close can still be read; then the peer is closed.
        assert_eq!(c.peek(1), Ok((4, 0)));
        assert_eq!(c.read(1, 8, 0), Ok(msg(b"last", &[])));
        assert_eq!(c.peek(1), Err(Error::PeerClosed));
    }

    #[test]
    fn the_limits_are_enforced() {
        let mut c: Channel<u32> = channel(2);
        let big = Message {
            bytes: alloc::vec![0; MAX_MESSAGE_BYTES + 1],
            handles: Vec::new(),
        };
        assert_eq!(c.write(0, big).unwrap_err().0, Error::InvalidArgs);
        let many = Message {
            bytes: Vec::new(),
            handles: alloc::vec![0; MAX_MESSAGE_HANDLES + 1],
        };
        assert_eq!(c.write(0, many).unwrap_err().0, Error::InvalidArgs);
        let exact = Message {
            bytes: alloc::vec![1; MAX_MESSAGE_BYTES],
            handles: alloc::vec![0; MAX_MESSAGE_HANDLES],
        };
        assert!(c.write(0, exact).is_ok());
    }

    #[test]
    fn the_byte_bound_applies_writable_means_a_maximal_message_fits() {
        // Room for two full messages and no more.
        let mut c: Channel<u32> = Channel::try_new(8, 2 * MAX_MESSAGE_BYTES).unwrap();
        let full = |v: u8| Message {
            bytes: alloc::vec![v; MAX_MESSAGE_BYTES],
            handles: Vec::new(),
        };
        c.write(0, full(1)).unwrap();
        assert_ne!(c.signals(0) & signals::WRITABLE, 0);
        c.write(0, full(2)).unwrap();
        assert_eq!(c.signals(0) & signals::WRITABLE, 0);
        let refused = c.write(0, full(3));
        assert_eq!(refused.unwrap_err().0, Error::ShouldWait);
        // A small message still fits under the bound even though a maximal one would not:
        // the count is what limits it here.
        assert_eq!(c.can_write(0, 1), Err(Error::ShouldWait));
        // Reading makes room for one; WRITABLE comes back.
        assert_eq!(c.read(1, MAX_MESSAGE_BYTES, 0).unwrap().bytes[0], 1);
        assert_ne!(c.signals(0) & signals::WRITABLE, 0);
        c.write(0, full(3)).unwrap();
    }

    #[test]
    fn a_byte_limit_below_one_message_is_raised_so_a_maximal_write_can_succeed() {
        let mut c: Channel<u32> = Channel::try_new(1, 16).unwrap();
        let exact = Message {
            bytes: alloc::vec![9; MAX_MESSAGE_BYTES],
            handles: Vec::new(),
        };
        assert!(c.write(0, exact).is_ok());
    }

    #[test]
    fn closing_returns_the_queued_bytes_to_the_bound() {
        let mut c: Channel<u32> = channel(2);
        c.write(0, msg(b"abc", &[])).unwrap();
        assert_eq!(c.close(1).len(), 1);
        assert_eq!(c.queued_bytes[1], 0);
    }

    /// Any interleaving of writes, reads and closes, against a model of two queues. Order,
    /// the bounds, what each side sees and what a closure gives back must all agree.
    #[test]
    fn a_random_operation_sequence_matches_a_model() {
        const DEPTH: usize = 6;
        const LIMIT: usize = 200;
        for seed in 1..=cases(200) as u64 {
            let mut rng = Rng::new(seed);
            let mut c: Channel<u32> =
                Channel::try_new(DEPTH, LIMIT.max(MAX_MESSAGE_BYTES)).unwrap();
            c.byte_limit = LIMIT;
            let mut model: [Deque<(Vec<u8>, Vec<u32>)>; 2] = [Deque::new(), Deque::new()];
            let mut bytes = [0usize; 2];
            let mut open = [true, true];
            let mut tag = 0u32;
            for _ in 0..200 {
                let side = rng.below(2);
                let to = other(side);
                match rng.below(9) {
                    0..=3 => {
                        tag += 1;
                        let m = (vec![tag as u8; rng.below(80)], vec![tag; rng.below(4)]);
                        let len = m.0.len();
                        let got = c.write(
                            side,
                            Message {
                                bytes: m.0.clone(),
                                handles: m.1.clone(),
                            },
                        );
                        if !open[side] {
                            // The kernel never writes through a closed end; skip the model.
                            if got.is_ok() {
                                bytes[to] += len;
                                model[to].push_back(m);
                            }
                        } else if !open[to] {
                            assert_eq!(got.unwrap_err().0, Error::PeerClosed);
                        } else if model[to].len() >= DEPTH || bytes[to] + len > LIMIT {
                            assert_eq!(got.unwrap_err().0, Error::ShouldWait);
                        } else {
                            assert!(got.is_ok());
                            bytes[to] += len;
                            model[to].push_back(m);
                        }
                    }
                    4..=7 => {
                        let (bc, hc) = (rng.below(90), rng.below(5));
                        let got = c.read(side, bc, hc);
                        match model[side].front() {
                            None => assert_eq!(
                                got,
                                Err(ReadError::Status(if open[to] {
                                    Error::ShouldWait
                                } else {
                                    Error::PeerClosed
                                }))
                            ),
                            Some((b, h)) if b.len() > bc || h.len() > hc => assert_eq!(
                                got,
                                Err(ReadError::TooSmall {
                                    bytes: b.len(),
                                    handles: h.len()
                                })
                            ),
                            Some(_) => {
                                let (b, h) = model[side].pop_front().unwrap();
                                bytes[side] -= b.len();
                                assert_eq!(
                                    got,
                                    Ok(Message {
                                        bytes: b,
                                        handles: h
                                    })
                                );
                            }
                        }
                    }
                    _ => {
                        if open[side] {
                            let back = c.close(side);
                            assert_eq!(back.len(), model[side].len());
                            model[side].clear();
                            bytes[side] = 0;
                            open[side] = false;
                        }
                    }
                }
                for s in 0..2 {
                    let mut want = 0;
                    if !model[s].is_empty() {
                        want |= signals::READABLE;
                    }
                    let t = other(s);
                    if open[t] && model[t].len() < DEPTH && bytes[t] + MAX_MESSAGE_BYTES <= LIMIT {
                        want |= signals::WRITABLE;
                    }
                    if !open[t] {
                        want |= signals::PEER_CLOSED;
                    }
                    assert_eq!(c.signals(s), want, "seed {seed} side {s}");
                }
            }
        }
    }
}
