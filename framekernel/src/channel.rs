//! The logic of one channel (ADR-0005 P8): two ends, a bounded FIFO toward each, messages of
//! bytes and transferred objects. Generic over what a transferred handle is, safe, tested on
//! the host; the kernel wraps it in a lock and its endpoints in reference-counted objects.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use origin_abi::limits::{MAX_MESSAGE_BYTES, MAX_MESSAGE_HANDLES};
use origin_abi::{Error, signals};

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
    depth: usize,
}

fn other(side: usize) -> usize {
    (side & 1) ^ 1
}

impl<H> Channel<H> {
    /// A channel whose queues hold at most `depth` messages each.
    pub fn new(depth: usize) -> Self {
        Self {
            queues: [VecDeque::new(), VecDeque::new()],
            open: [true, true],
            depth: depth.max(1),
        }
    }

    /// Whether side `from` could write now.
    pub fn can_write(&self, from: usize) -> Result<(), Error> {
        let to = other(from);
        if !self.open.get(to).copied().unwrap_or(false) {
            return Err(Error::PeerClosed);
        }
        if self.queues.get(to).is_none_or(|q| q.len() >= self.depth) {
            return Err(Error::ShouldWait);
        }
        Ok(())
    }

    /// Queues `message` for the other side. On failure it is handed back unchanged.
    pub fn write(&mut self, from: usize, message: Message<H>) -> Result<(), (Error, Message<H>)> {
        if message.bytes.len() > MAX_MESSAGE_BYTES || message.handles.len() > MAX_MESSAGE_HANDLES {
            return Err((Error::InvalidArgs, message));
        }
        if let Err(e) = self.can_write(from) {
            return Err((e, message));
        }
        match self.queues.get_mut(other(from)) {
            Some(q) => {
                q.push_back(message);
                Ok(())
            }
            None => Err((Error::InvalidArgs, message)),
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
        let peer_open = self.open.get(other(at)).copied().unwrap_or(false);
        let queue = self
            .queues
            .get_mut(at & 1)
            .ok_or(ReadError::Status(Error::InvalidArgs))?;
        let Some(next) = queue.front() else {
            return Err(ReadError::Status(if peer_open {
                Error::ShouldWait
            } else {
                Error::PeerClosed
            }));
        };
        if next.bytes.len() > byte_capacity || next.handles.len() > handle_capacity {
            return Err(ReadError::TooSmall {
                bytes: next.bytes.len(),
                handles: next.handles.len(),
            });
        }
        queue
            .pop_front()
            .ok_or(ReadError::Status(Error::ShouldWait))
    }

    /// Closes side `side`. Returns the messages that were waiting to be read there, which
    /// the caller drops outside any lock (they may hold objects whose drop takes locks).
    pub fn close(&mut self, side: usize) -> VecDeque<Message<H>> {
        if let Some(o) = self.open.get_mut(side & 1) {
            *o = false;
        }
        self.queues
            .get_mut(side & 1)
            .map(core::mem::take)
            .unwrap_or_default()
    }

    /// The signals side `side` sees.
    pub fn signals(&self, side: usize) -> u32 {
        let mut s = 0;
        if self.queues.get(side & 1).is_some_and(|q| !q.is_empty()) {
            s |= signals::READABLE;
        }
        if self.can_write(side).is_ok() {
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

    fn msg(b: &[u8], h: &[u32]) -> Message<u32> {
        Message {
            bytes: b.to_vec(),
            handles: h.to_vec(),
        }
    }

    #[test]
    fn messages_arrive_in_order_on_the_other_side_with_their_handles() {
        let mut c = Channel::new(4);
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
        let mut c: Channel<u32> = Channel::new(1);
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
        let mut c = Channel::new(2);
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
        assert_eq!(c.read(0, 5, 2), Ok(msg(b"hello", &[1, 2])));
    }

    #[test]
    fn the_limits_are_enforced() {
        let mut c: Channel<u32> = Channel::new(2);
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
}
