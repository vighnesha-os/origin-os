//! TCP segments and a minimal connection state machine (CLAUDE.md §54: TCP). Header
//! parsing and building with the IPv4 pseudo-header checksum, and a transmission control
//! block that drives the three-way handshake, one in-order data exchange and the FIN
//! teardown. Pure, no `unsafe`, tested on the host by connecting two blocks through an
//! in-memory loopback.
//!
//! This is the connection core, not a complete TCP: there is no retransmission,
//! congestion control, out-of-order reassembly, window scaling or option handling beyond
//! ignoring options on receive. It establishes, carries ordered data and closes, which is
//! what the socket layer and the loopback proof need first.

use crate::net::{Ipv4, checksum_parts};

/// TCP flag bits.
pub mod flag {
    pub const FIN: u8 = 1 << 0;
    pub const SYN: u8 = 1 << 1;
    pub const RST: u8 = 1 << 2;
    pub const PSH: u8 = 1 << 3;
    pub const ACK: u8 = 1 << 4;
}

/// The minimum TCP header length (no options).
pub const TCP_HEADER: usize = 20;

/// A parsed TCP segment header and its payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub window: u16,
    pub payload: &'a [u8],
}

impl<'a> Segment<'a> {
    /// Parses a TCP segment carried in an IPv4 packet from `src` to `dst`, checking the
    /// checksum over the pseudo-header. Options are skipped; a data offset past the buffer
    /// is rejected.
    pub fn parse(buf: &'a [u8], src: Ipv4, dst: Ipv4) -> Option<Self> {
        let header = buf.get(..TCP_HEADER)?;
        if pseudo_checksum(src, dst, buf) != 0 {
            return None;
        }
        let data_offset = usize::from(header[12] >> 4) * 4;
        if data_offset < TCP_HEADER || data_offset > buf.len() {
            return None;
        }
        Some(Self {
            src_port: u16::from_be_bytes([header[0], header[1]]),
            dst_port: u16::from_be_bytes([header[2], header[3]]),
            seq: u32::from_be_bytes([header[4], header[5], header[6], header[7]]),
            ack: u32::from_be_bytes([header[8], header[9], header[10], header[11]]),
            flags: header[13],
            window: u16::from_be_bytes([header[14], header[15]]),
            payload: buf.get(data_offset..)?,
        })
    }

    /// Builds a TCP segment into `out` (a 20-byte header, no options, plus `payload`),
    /// with the pseudo-header checksum. Returns the total length.
    #[allow(clippy::too_many_arguments)]
    pub fn write(
        src: Ipv4,
        dst: Ipv4,
        src_port: u16,
        dst_port: u16,
        seq: u32,
        ack: u32,
        flags: u8,
        window: u16,
        payload: &[u8],
        out: &mut [u8],
    ) -> Option<usize> {
        let total = TCP_HEADER.checked_add(payload.len())?;
        let buf = out.get_mut(..total)?;
        buf.fill(0);
        buf[0..2].copy_from_slice(&src_port.to_be_bytes());
        buf[2..4].copy_from_slice(&dst_port.to_be_bytes());
        buf[4..8].copy_from_slice(&seq.to_be_bytes());
        buf[8..12].copy_from_slice(&ack.to_be_bytes());
        buf[12] = (5 << 4) & 0xF0; // data offset 5 words, no options
        buf[13] = flags;
        buf[14..16].copy_from_slice(&window.to_be_bytes());
        buf[20..].copy_from_slice(payload);
        let c = pseudo_checksum(src, dst, buf);
        buf[16..18].copy_from_slice(&c.to_be_bytes());
        Some(total)
    }

    /// The sequence length a segment consumes: its payload, plus one each for SYN and FIN.
    pub fn seq_len(&self) -> u32 {
        self.payload.len() as u32
            + u32::from(self.flags & flag::SYN != 0)
            + u32::from(self.flags & flag::FIN != 0)
    }
}

/// The TCP checksum over the IPv4 pseudo-header and the segment. Zero when the segment
/// already carries its correct checksum.
pub fn pseudo_checksum(src: Ipv4, dst: Ipv4, segment: &[u8]) -> u16 {
    let len = (segment.len() as u16).to_be_bytes();
    let pseudo = [
        src.0[0],
        src.0[1],
        src.0[2],
        src.0[3],
        dst.0[0],
        dst.0[1],
        dst.0[2],
        dst.0[3],
        0,
        crate::net::protocol::TCP,
        len[0],
        len[1],
    ];
    checksum_parts(&[&pseudo, segment])
}

/// A segment the state machine wants sent: its sequence and ack numbers, flags, and how
/// many bytes of the endpoint's pending send data to attach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outgoing {
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub data_len: usize,
}

/// What an incoming segment delivered to the application, and the reply (if any).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Incoming {
    /// Bytes of the segment's payload to deliver to the application.
    pub deliver: usize,
    /// A segment to send in response.
    pub reply: Option<Outgoing>,
}

/// A TCP connection's state (RFC 793).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    LastAck,
    Closing,
    TimeWait,
}

/// A transmission control block: one endpoint's sequence state.
#[derive(Clone, Copy, Debug)]
pub struct Tcb {
    pub state: State,
    /// Next sequence number to send (the seq a new outgoing segment carries).
    pub snd_nxt: u32,
    /// Oldest unacknowledged sequence number.
    pub snd_una: u32,
    /// Next sequence number expected to receive (the ack we send).
    pub rcv_nxt: u32,
}

impl Tcb {
    /// A fresh block with initial send sequence `iss`.
    pub fn new(iss: u32) -> Self {
        Self {
            state: State::Closed,
            snd_nxt: iss,
            snd_una: iss,
            rcv_nxt: 0,
        }
    }

    /// Consumes `len` sequence numbers for an outgoing segment and returns the seq it
    /// should carry (the value before the advance).
    fn take_seq(&mut self, len: u32) -> u32 {
        let seq = self.snd_nxt;
        self.snd_nxt = self.snd_nxt.wrapping_add(len);
        seq
    }

    /// Actively opens: SYN-SENT, and a SYN to send.
    pub fn connect(&mut self) -> Outgoing {
        self.state = State::SynSent;
        let seq = self.take_seq(1);
        Outgoing {
            seq,
            ack: 0,
            flags: flag::SYN,
            data_len: 0,
        }
    }

    /// Passively opens: LISTEN.
    pub fn listen(&mut self) {
        self.state = State::Listen;
    }

    /// Sends `len` bytes of application data (only valid once established). Returns the
    /// segment to carry them.
    pub fn send(&mut self, len: usize) -> Outgoing {
        let seq = self.take_seq(len as u32);
        Outgoing {
            seq,
            ack: self.rcv_nxt,
            flags: flag::ACK | flag::PSH,
            data_len: len,
        }
    }

    /// Begins or continues a close: a FIN to send, or `None` if closing is not possible
    /// from the current state.
    pub fn close(&mut self) -> Option<Outgoing> {
        let next = match self.state {
            State::Established => State::FinWait1,
            State::CloseWait => State::LastAck,
            _ => return None,
        };
        self.state = next;
        let seq = self.take_seq(1);
        Some(Outgoing {
            seq,
            ack: self.rcv_nxt,
            flags: flag::FIN | flag::ACK,
            data_len: 0,
        })
    }

    /// A bare ACK of the current receive sequence.
    fn ack(&self) -> Outgoing {
        Outgoing {
            seq: self.snd_nxt,
            ack: self.rcv_nxt,
            flags: flag::ACK,
            data_len: 0,
        }
    }

    /// Processes an incoming segment and returns what to deliver and what to reply.
    pub fn on_segment(&mut self, seg: &Segment<'_>) -> Incoming {
        let (syn, ack, fin) = (
            seg.flags & flag::SYN != 0,
            seg.flags & flag::ACK != 0,
            seg.flags & flag::FIN != 0,
        );
        if ack {
            self.snd_una = seg.ack;
        }
        match self.state {
            State::Listen if syn => {
                self.rcv_nxt = seg.seq.wrapping_add(1);
                let seq = self.take_seq(1);
                self.state = State::SynReceived;
                Incoming {
                    deliver: 0,
                    reply: Some(Outgoing {
                        seq,
                        ack: self.rcv_nxt,
                        flags: flag::SYN | flag::ACK,
                        data_len: 0,
                    }),
                }
            }
            State::SynSent if syn && ack => {
                self.rcv_nxt = seg.seq.wrapping_add(1);
                self.state = State::Established;
                Incoming {
                    deliver: 0,
                    reply: Some(self.ack()),
                }
            }
            State::SynReceived if ack => {
                self.state = State::Established;
                Incoming::default()
            }
            State::Established => {
                let mut out = Incoming::default();
                if !seg.payload.is_empty() && seg.seq == self.rcv_nxt {
                    self.rcv_nxt = self.rcv_nxt.wrapping_add(seg.payload.len() as u32);
                    out.deliver = seg.payload.len();
                }
                if fin && seg.seq.wrapping_add(seg.payload.len() as u32) == self.rcv_nxt {
                    self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
                    self.state = State::CloseWait;
                }
                if out.deliver > 0 || fin {
                    out.reply = Some(self.ack());
                }
                out
            }
            State::FinWait1 => {
                if ack && seg.ack == self.snd_nxt {
                    self.state = State::FinWait2;
                }
                if fin && seg.seq == self.rcv_nxt {
                    self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
                    self.state = if self.state == State::FinWait2 {
                        State::TimeWait
                    } else {
                        State::Closing
                    };
                    return Incoming {
                        deliver: 0,
                        reply: Some(self.ack()),
                    };
                }
                Incoming::default()
            }
            State::FinWait2 if fin && seg.seq == self.rcv_nxt => {
                self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
                self.state = State::TimeWait;
                Incoming {
                    deliver: 0,
                    reply: Some(self.ack()),
                }
            }
            State::Closing if ack && seg.ack == self.snd_nxt => {
                self.state = State::TimeWait;
                Incoming::default()
            }
            State::LastAck if ack && seg.ack == self.snd_nxt => {
                self.state = State::Closed;
                Incoming::default()
            }
            _ => Incoming::default(),
        }
    }
}

/// An endpoint for [`loopback_demo`]: its TCB, addressing and queues.
struct Endpoint {
    tcb: Tcb,
    ip: Ipv4,
    peer_ip: Ipv4,
    port: u16,
    peer_port: u16,
    send_queue: alloc::vec::Vec<u8>,
    received: alloc::vec::Vec<u8>,
}

impl Endpoint {
    fn frame(&mut self, out: Outgoing) -> alloc::vec::Vec<u8> {
        let data: alloc::vec::Vec<u8> = self.send_queue.drain(..out.data_len).collect();
        let mut buf = alloc::vec![0u8; TCP_HEADER + data.len()];
        let _ = Segment::write(
            self.ip,
            self.peer_ip,
            self.port,
            self.peer_port,
            out.seq,
            out.ack,
            out.flags,
            4096,
            &data,
            &mut buf,
        );
        buf
    }

    fn recv(&mut self, bytes: &[u8]) -> Option<alloc::vec::Vec<u8>> {
        let seg = Segment::parse(bytes, self.peer_ip, self.ip)?;
        let inc = self.tcb.on_segment(&seg);
        if inc.deliver > 0 {
            let start = seg.payload.len() - inc.deliver;
            self.received.extend_from_slice(&seg.payload[start..]);
        }
        inc.reply.map(|r| self.frame(r))
    }
}

/// Runs a whole connection over an in-memory loopback: an active opener (A) and a passive
/// listener (B) complete the handshake, A sends `payload` to B, then A closes and B
/// follows. Returns whether B received exactly `payload` and the teardown reached the
/// expected end states (B closed, A tearing down). Uses the kernel heap.
pub fn loopback_demo(payload: &[u8]) -> bool {
    use alloc::vec::Vec;
    const A_IP: Ipv4 = Ipv4([10, 0, 0, 1]);
    const B_IP: Ipv4 = Ipv4([10, 0, 0, 2]);
    const A_PORT: u16 = 49152;
    const B_PORT: u16 = 80;
    let mut a = Endpoint {
        tcb: Tcb::new(1000),
        ip: A_IP,
        peer_ip: B_IP,
        port: A_PORT,
        peer_port: B_PORT,
        send_queue: Vec::new(),
        received: Vec::new(),
    };
    let mut b = Endpoint {
        tcb: Tcb::new(5000),
        ip: B_IP,
        peer_ip: A_IP,
        port: B_PORT,
        peer_port: A_PORT,
        send_queue: Vec::new(),
        received: Vec::new(),
    };
    b.tcb.listen();

    // Deliver `wire` and its chain of replies to completion; B closes when it reaches
    // CloseWait and the wire falls quiet.
    let pump = |a: &mut Endpoint, b: &mut Endpoint, mut wire: Option<Vec<u8>>| {
        for _ in 0..40 {
            let Some(bytes) = wire.take() else { break };
            let to_b = u16::from_be_bytes([bytes[2], bytes[3]]) == B_PORT;
            wire = if to_b { b.recv(&bytes) } else { a.recv(&bytes) };
            if wire.is_none() && b.tcb.state == State::CloseWait {
                wire = b.tcb.close().map(|o| b.frame(o));
            }
        }
    };

    let connect = a.tcb.connect();
    let syn = a.frame(connect);
    pump(&mut a, &mut b, Some(syn));
    if a.tcb.state != State::Established || b.tcb.state != State::Established {
        return false;
    }
    a.send_queue.extend_from_slice(payload);
    let push = a.tcb.send(payload.len());
    let data = a.frame(push);
    pump(&mut a, &mut b, Some(data));
    let fin = a.tcb.close().map(|o| a.frame(o));
    pump(&mut a, &mut b, fin);

    b.received == payload
        && b.tcb.state == State::Closed
        && matches!(
            a.tcb.state,
            State::TimeWait | State::Closing | State::FinWait2
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    const A_IP: Ipv4 = Ipv4([10, 0, 0, 1]);
    const B_IP: Ipv4 = Ipv4([10, 0, 0, 2]);
    const A_PORT: u16 = 49152;
    const B_PORT: u16 = 80;

    #[test]
    fn segment_round_trips_with_the_pseudo_header_checksum() {
        let mut buf = [0u8; 64];
        let data = b"GET /";
        let n = Segment::write(
            A_IP,
            B_IP,
            A_PORT,
            B_PORT,
            100,
            200,
            flag::ACK | flag::PSH,
            1024,
            data,
            &mut buf,
        )
        .unwrap();
        let seg = Segment::parse(&buf[..n], A_IP, B_IP).unwrap();
        assert_eq!(
            (seg.src_port, seg.dst_port, seg.seq, seg.ack),
            (A_PORT, B_PORT, 100, 200)
        );
        assert_eq!(seg.flags, flag::ACK | flag::PSH);
        assert_eq!(
            (seg.window, seg.payload, seg.seq_len()),
            (1024, &data[..], 5)
        );
        let mut bad = buf;
        bad[4] ^= 1;
        assert_eq!(
            Segment::parse(&bad[..n], A_IP, B_IP),
            None,
            "corrupt checksum"
        );
        assert_eq!(
            Segment::parse(&buf[..n], A_IP, A_IP),
            None,
            "the checksum binds the addresses"
        );
    }

    #[test]
    fn seq_len_counts_syn_and_fin() {
        let mut buf = [0u8; 40];
        let n = Segment::write(A_IP, B_IP, 1, 2, 0, 0, flag::SYN, 0, &[], &mut buf).unwrap();
        assert_eq!(Segment::parse(&buf[..n], A_IP, B_IP).unwrap().seq_len(), 1);
        let n = Segment::write(
            A_IP,
            B_IP,
            1,
            2,
            0,
            0,
            flag::FIN | flag::ACK,
            0,
            b"xyz",
            &mut buf,
        )
        .unwrap();
        assert_eq!(Segment::parse(&buf[..n], A_IP, B_IP).unwrap().seq_len(), 4);
    }

    /// One endpoint and its wire framing.
    struct End {
        tcb: Tcb,
        ip: Ipv4,
        peer_ip: Ipv4,
        port: u16,
        peer_port: u16,
        send_queue: Vec<u8>,
        received: Vec<u8>,
    }

    impl End {
        /// Serializes `out`, draining `data_len` bytes from the send queue.
        fn frame(&mut self, out: Outgoing) -> Vec<u8> {
            let data: Vec<u8> = self.send_queue.drain(..out.data_len).collect();
            let mut buf = std::vec![0u8; TCP_HEADER + data.len()];
            Segment::write(
                self.ip,
                self.peer_ip,
                self.port,
                self.peer_port,
                out.seq,
                out.ack,
                out.flags,
                4096,
                &data,
                &mut buf,
            )
            .unwrap();
            buf
        }

        /// Receives a wire segment, delivering data and returning any reply framed.
        fn recv(&mut self, bytes: &[u8]) -> Option<Vec<u8>> {
            let seg = Segment::parse(bytes, self.peer_ip, self.ip).unwrap();
            let inc = self.tcb.on_segment(&seg);
            if inc.deliver > 0 {
                let start = seg.payload.len() - inc.deliver;
                self.received.extend_from_slice(&seg.payload[start..]);
            }
            inc.reply.map(|r| self.frame(r))
        }
    }

    /// Delivers `wire` to its destination and keeps delivering replies until the wire is
    /// quiet, letting `b` close once it reaches CloseWait.
    fn pump(a: &mut End, b: &mut End, mut wire: Option<Vec<u8>>, b_closes: bool) {
        let mut steps = 0;
        while let Some(bytes) = wire.take() {
            steps += 1;
            assert!(
                steps < 30,
                "did not converge: a={:?} b={:?}",
                a.tcb.state,
                b.tcb.state
            );
            let to_b = u16::from_be_bytes([bytes[2], bytes[3]]) == B_PORT;
            wire = if to_b { b.recv(&bytes) } else { a.recv(&bytes) };
            if b_closes && b.tcb.state == State::CloseWait && wire.is_none() {
                wire = b.tcb.close().map(|o| b.frame(o));
            }
        }
    }

    #[test]
    fn loopback_demo_succeeds_for_various_payloads() {
        assert!(super::loopback_demo(b"hello tcp"));
        assert!(
            super::loopback_demo(b""),
            "a zero-length payload still establishes and closes"
        );
        assert!(super::loopback_demo(&[0x5A; 200]));
        assert!(
            super::loopback_demo(&[0u8; 1000]),
            "a larger payload within the window"
        );
    }

    #[test]
    fn a_loopback_connection_establishes_exchanges_data_and_closes() {
        let mut a = End {
            tcb: Tcb::new(1000),
            ip: A_IP,
            peer_ip: B_IP,
            port: A_PORT,
            peer_port: B_PORT,
            send_queue: Vec::new(),
            received: Vec::new(),
        };
        let mut b = End {
            tcb: Tcb::new(5000),
            ip: B_IP,
            peer_ip: A_IP,
            port: B_PORT,
            peer_port: A_PORT,
            send_queue: Vec::new(),
            received: Vec::new(),
        };
        b.tcb.listen();
        // Handshake.
        let connect = a.tcb.connect();
        let syn = a.frame(connect);
        pump(&mut a, &mut b, Some(syn), false);
        assert_eq!(
            (a.tcb.state, b.tcb.state),
            (State::Established, State::Established)
        );
        // A sends data; B receives it.
        a.send_queue.extend_from_slice(b"hello tcp");
        let push = a.tcb.send(9);
        let data_seg = a.frame(push);
        pump(&mut a, &mut b, Some(data_seg), false);
        assert_eq!(b.received, b"hello tcp");
        // A closes; B follows.
        let fin = a.tcb.close().map(|o| a.frame(o));
        pump(&mut a, &mut b, fin, true);
        assert_eq!(
            b.tcb.state,
            State::Closed,
            "B closed after its FIN was acked"
        );
        assert!(
            matches!(
                a.tcb.state,
                State::TimeWait | State::Closing | State::FinWait2
            ),
            "A tearing down: {:?}",
            a.tcb.state
        );
    }
}
