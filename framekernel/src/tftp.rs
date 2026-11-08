//! A TFTP reader (RFC 1350; CLAUDE.md §54): the read request in octet (binary) mode, the
//! acknowledgement, the packets a reader receives, and a download state machine.
//!
//! [`Download`] accepts each DATA block exactly once and in order, acknowledges the previous
//! block again when the server repeats it (our acknowledgement was lost), ignores stale,
//! out-of-order and malformed packets, completes at the first block shorter than
//! [`BLOCK`] bytes (so a file whose size is a multiple of 512 ends with an empty block),
//! fails on a server ERROR, and refuses a file larger than the caller's limit. It is pure:
//! the kernel carries the packets over UDP.

use alloc::vec::Vec;

/// TFTP opcodes (RFC 1350 §5; OACK is RFC 2347's option acknowledgement).
pub mod opcode {
    pub const RRQ: u16 = 1;
    pub const WRQ: u16 = 2;
    pub const DATA: u16 = 3;
    pub const ACK: u16 = 4;
    pub const ERROR: u16 = 5;
    pub const OACK: u16 = 6;
}

/// The server's well-known port.
pub const PORT: u16 = 69;
/// The data block size: a DATA packet carries at most this many bytes, and a shorter one
/// is the last.
pub const BLOCK: usize = 512;

/// Builds a read request for `filename` in octet mode into `out`. Returns the length, or
/// `None` if the name is empty or contains a NUL, or `out` is too small.
pub fn read_request(filename: &str, out: &mut [u8]) -> Option<usize> {
    const MODE: &[u8] = b"octet";
    let name = filename.as_bytes();
    if name.is_empty() || name.contains(&0) {
        return None;
    }
    let mode_at = 2 + name.len() + 1;
    let total = mode_at + MODE.len() + 1;
    let buf = out.get_mut(..total)?;
    buf[..2].copy_from_slice(&opcode::RRQ.to_be_bytes());
    buf[2..mode_at - 1].copy_from_slice(name);
    buf[mode_at - 1] = 0;
    buf[mode_at..total - 1].copy_from_slice(MODE);
    buf[total - 1] = 0;
    Some(total)
}

/// Builds the acknowledgement of `block` into `out` (4 bytes).
pub fn ack(block: u16, out: &mut [u8]) -> Option<usize> {
    let buf = out.get_mut(..4)?;
    buf[..2].copy_from_slice(&opcode::ACK.to_be_bytes());
    buf[2..].copy_from_slice(&block.to_be_bytes());
    Some(4)
}

/// A packet a reader receives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Packet<'a> {
    /// A data block: its number and at most [`BLOCK`] bytes.
    Data { block: u16, data: &'a [u8] },
    /// An error from the server: its code and its message without the terminating NUL.
    Error { code: u16, message: &'a [u8] },
    /// Another opcode (a reader has no use for it).
    Other(u16),
}

/// Parses a packet a reader receives. `None` for a packet shorter than its header, a DATA
/// block longer than [`BLOCK`], or an ERROR whose message is not NUL-terminated.
pub fn parse(buf: &[u8]) -> Option<Packet<'_>> {
    let op = u16::from_be_bytes([*buf.first()?, *buf.get(1)?]);
    match op {
        opcode::DATA => {
            let block = u16::from_be_bytes([*buf.get(2)?, *buf.get(3)?]);
            let data = buf.get(4..)?;
            (data.len() <= BLOCK).then_some(Packet::Data { block, data })
        }
        opcode::ERROR => {
            let code = u16::from_be_bytes([*buf.get(2)?, *buf.get(3)?]);
            let rest = buf.get(4..)?;
            let end = rest.iter().position(|&b| b == 0)?;
            Some(Packet::Error {
                code,
                message: rest.get(..end)?,
            })
        }
        other => Some(Packet::Other(other)),
    }
}

/// What a reader sends after a packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Acknowledge `block`; `last` when it is the final (short) block, after which the
    /// file is complete.
    Ack { block: u16, last: bool },
    /// Nothing (a stale, out-of-order or malformed packet).
    Ignore,
    /// The transfer failed.
    Failed(Failure),
}

/// Why a transfer failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The server sent an ERROR with this code (1: file not found, 2: access violation, …).
    Server(u16),
    /// The file is larger than the reader's limit.
    TooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Receiving,
    Complete,
    Failed(Failure),
}

/// A file being read: the blocks accepted so far, in order.
#[derive(Debug)]
pub struct Download {
    data: Vec<u8>,
    limit: usize,
    /// The block expected next while receiving; the final block's number once complete.
    next: u16,
    state: State,
}

impl Download {
    /// A download that refuses a file larger than `limit` bytes.
    pub fn new(limit: usize) -> Self {
        Self {
            data: Vec::new(),
            limit,
            next: 1,
            state: State::Receiving,
        }
    }

    /// Takes a packet the server sent (the UDP payload) and says what to send back.
    pub fn on_packet(&mut self, buf: &[u8]) -> Step {
        let Some(packet) = parse(buf) else {
            return Step::Ignore;
        };
        match (self.state, packet) {
            (State::Failed(failure), _) => Step::Failed(failure),
            (State::Receiving, Packet::Error { code, .. }) => self.fail(Failure::Server(code)),
            (State::Receiving, Packet::Data { block, data }) if block == self.next => {
                let fits = self
                    .data
                    .len()
                    .checked_add(data.len())
                    .is_some_and(|n| n <= self.limit);
                if !fits {
                    return self.fail(Failure::TooLarge);
                }
                self.data.extend_from_slice(data);
                let last = data.len() < BLOCK;
                if last {
                    self.state = State::Complete;
                } else {
                    self.next = self.next.wrapping_add(1);
                }
                Step::Ack { block, last }
            }
            // The previous block again: our acknowledgement was lost.
            (State::Receiving, Packet::Data { block, .. })
                if block != 0 && block == self.next.wrapping_sub(1) =>
            {
                Step::Ack { block, last: false }
            }
            // The final block again, after completion: our last acknowledgement was lost.
            (State::Complete, Packet::Data { block, .. }) if block == self.next => {
                Step::Ack { block, last: true }
            }
            _ => Step::Ignore,
        }
    }

    fn fail(&mut self, failure: Failure) -> Step {
        self.state = State::Failed(failure);
        Step::Failed(failure)
    }

    /// Whether the final block has arrived.
    pub fn is_complete(&self) -> bool {
        self.state == State::Complete
    }

    /// The failure, if the transfer failed.
    pub fn failure(&self) -> Option<Failure> {
        match self.state {
            State::Failed(failure) => Some(failure),
            _ => None,
        }
    }

    /// The bytes accepted so far, taken out of the download (the whole file once complete).
    pub fn into_data(self) -> Vec<u8> {
        self.data
    }

    /// The bytes accepted so far: the whole file once complete.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// How many blocks were accepted.
    pub fn blocks(&self) -> u16 {
        match self.state {
            State::Complete => self.next,
            _ => self.next.wrapping_sub(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn data(block: u16, bytes: &[u8]) -> Vec<u8> {
        let mut p = vec![0, 3];
        p.extend_from_slice(&block.to_be_bytes());
        p.extend_from_slice(bytes);
        p
    }

    fn error(code: u16, message: &[u8]) -> Vec<u8> {
        let mut p = vec![0, 5];
        p.extend_from_slice(&code.to_be_bytes());
        p.extend_from_slice(message);
        p.push(0);
        p
    }

    /// A file of `len` bytes whose content depends on the offset.
    fn file(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 % 251) as u8).collect()
    }

    /// Serves `content` block by block to a fresh download, as a server would, checking
    /// every acknowledgement; returns the download.
    fn serve(content: &[u8], limit: usize) -> Download {
        let mut d = Download::new(limit);
        let mut block = 1u16;
        let mut at = 0;
        loop {
            let chunk = &content[at..(at + BLOCK).min(content.len())];
            let last = chunk.len() < BLOCK;
            assert_eq!(
                d.on_packet(&data(block, chunk)),
                Step::Ack { block, last },
                "block {block}"
            );
            if last {
                return d;
            }
            at += BLOCK;
            block += 1;
        }
    }

    #[test]
    fn read_request_is_octet_mode_and_nul_terminated() {
        let mut out = [0u8; 64];
        let n = read_request("initramfs.tar", &mut out).unwrap();
        assert_eq!(&out[..n], b"\x00\x01initramfs.tar\x00octet\x00");
        assert_eq!(read_request("", &mut out), None);
        assert_eq!(read_request("a\0b", &mut out), None);
        assert_eq!(read_request("initramfs.tar", &mut out[..10]), None);
    }

    #[test]
    fn ack_encodes_the_block() {
        let mut out = [0u8; 4];
        assert_eq!(ack(0x0102, &mut out), Some(4));
        assert_eq!(out, [0, 4, 1, 2]);
        assert_eq!(ack(1, &mut [0u8; 3]), None);
    }

    #[test]
    fn parses_data_and_errors_and_rejects_malformed_packets() {
        assert_eq!(
            parse(&data(7, b"abc")),
            Some(Packet::Data {
                block: 7,
                data: b"abc"
            })
        );
        assert_eq!(
            parse(&data(1, &[])),
            Some(Packet::Data {
                block: 1,
                data: &[]
            })
        );
        assert_eq!(parse(&data(1, &[0; BLOCK + 1])), None, "oversized block");
        assert_eq!(
            parse(&error(1, b"File not found")),
            Some(Packet::Error {
                code: 1,
                message: b"File not found"
            })
        );
        assert_eq!(parse(&[0, 5, 0, 1, b'x']), None, "unterminated message");
        assert_eq!(parse(&[0, 6, 0]), Some(Packet::Other(opcode::OACK)));
        assert_eq!(parse(&[0, 3, 0]), None, "short header");
        assert_eq!(parse(&[0]), None);
        assert_eq!(parse(&[]), None);
    }

    #[test]
    fn downloads_a_file_in_order_and_ends_at_a_short_block() {
        let content = file(1200); // 512 + 512 + 176
        let d = serve(&content, 4096);
        assert!(d.is_complete());
        assert_eq!(d.blocks(), 3);
        assert_eq!(d.data(), &content[..]);
        assert_eq!(d.failure(), None);
        assert_eq!(d.into_data(), content, "the file moves out whole");
    }

    #[test]
    fn a_multiple_of_the_block_size_ends_with_an_empty_block() {
        for blocks in [0usize, 1, 2, 9] {
            let content = file(blocks * BLOCK);
            let d = serve(&content, 8192);
            assert!(d.is_complete());
            assert_eq!(usize::from(d.blocks()), blocks + 1, "{blocks} full blocks");
            assert_eq!(d.data(), &content[..]);
        }
    }

    #[test]
    fn duplicates_are_reacknowledged_and_strays_ignored() {
        let content = file(1100);
        let mut d = Download::new(4096);
        let b1 = data(1, &content[..512]);
        assert_eq!(
            d.on_packet(&b1),
            Step::Ack {
                block: 1,
                last: false
            }
        );
        // The server repeats block 1 (our ACK was lost): acknowledged again, not appended.
        assert_eq!(
            d.on_packet(&b1),
            Step::Ack {
                block: 1,
                last: false
            }
        );
        // A block from the future and a block 0 are ignored.
        assert_eq!(d.on_packet(&data(3, &content[1024..])), Step::Ignore);
        assert_eq!(d.on_packet(&data(0, b"x")), Step::Ignore);
        // Garbage is ignored.
        assert_eq!(d.on_packet(&[0xFF; 9]), Step::Ignore);
        assert_eq!(d.on_packet(&[0, 3, 0]), Step::Ignore);
        assert_eq!(
            d.on_packet(&data(2, &content[512..1024])),
            Step::Ack {
                block: 2,
                last: false
            }
        );
        let b3 = data(3, &content[1024..]);
        assert_eq!(
            d.on_packet(&b3),
            Step::Ack {
                block: 3,
                last: true
            }
        );
        // After completion: the final block repeated is acknowledged again; anything else,
        // including an ERROR, is ignored.
        assert_eq!(
            d.on_packet(&b3),
            Step::Ack {
                block: 3,
                last: true
            }
        );
        assert_eq!(d.on_packet(&error(0, b"late")), Step::Ignore);
        assert_eq!(d.data(), &content[..]);
        assert_eq!(d.blocks(), 3);
    }

    #[test]
    fn a_server_error_or_the_size_limit_fails_the_transfer() {
        let mut d = Download::new(4096);
        assert_eq!(
            d.on_packet(&error(1, b"File not found")),
            Step::Failed(Failure::Server(1))
        );
        assert_eq!(d.failure(), Some(Failure::Server(1)));
        // A failed transfer stays failed.
        assert_eq!(
            d.on_packet(&data(1, b"late")),
            Step::Failed(Failure::Server(1))
        );
        assert!(!d.is_complete());

        let mut d = Download::new(1000);
        assert_eq!(
            d.on_packet(&data(1, &[1; BLOCK])),
            Step::Ack {
                block: 1,
                last: false
            }
        );
        assert_eq!(
            d.on_packet(&data(2, &[2; BLOCK])),
            Step::Failed(Failure::TooLarge)
        );
        assert_eq!(d.data().len(), BLOCK, "nothing past the limit is kept");
    }

    #[test]
    fn mutated_packets_never_panic() {
        let content = file(700);
        let seeds = [
            data(1, &content[..512]),
            data(2, &content[512..]),
            error(2, b"no"),
        ];
        let mut rng = crate::testing::Rng::new(0x5446_5450);
        for _ in 0..crate::testing::cases(20_000) {
            let mut d = Download::new(2048);
            for seed in &seeds {
                let mut packet = seed.clone();
                crate::testing::mutate(&mut packet, &mut rng);
                let _ = parse(&packet);
                let _ = d.on_packet(&packet);
                assert!(d.data().len() <= 2048);
            }
        }
    }
}
