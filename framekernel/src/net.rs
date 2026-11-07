//! The network packet layer (CLAUDE.md §54: Ethernet, IPv4, ICMP, ARP). Header parsing
//! and building, and the Internet checksum (RFC 1071). Pure, no `unsafe`, bounds-checked,
//! tested on the host; a NIC driver will move these bytes once one exists, so the stack is
//! proven here by building, parsing and an in-memory loopback echo.
//!
//! All multi-byte header fields are network byte order (big-endian). Every parser checks
//! lengths before reading and never panics on a short or malformed buffer.

/// The Internet checksum (RFC 1071): the one's-complement sum of 16-bit big-endian words,
/// complemented. A trailing odd byte is padded with zero. Summing a block that already
/// carries its correct checksum yields 0.
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let (pairs, remainder) = data.as_chunks::<2>();
    for pair in pairs {
        sum += u32::from(u16::from_be_bytes(*pair));
    }
    if let [last] = remainder {
        sum += u32::from(u16::from_be_bytes([*last, 0]));
    }
    // Fold the carries in, twice (the first fold can itself carry).
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// A checksum computed over several slices in sequence (a header and its payload, say),
/// without copying them together. Equivalent to the checksum of their concatenation.
pub fn checksum_parts(parts: &[&[u8]]) -> u16 {
    let mut sum: u32 = 0;
    let mut carry_byte: Option<u8> = None;
    for part in parts {
        let mut data = *part;
        if let Some(high) = carry_byte.take() {
            if let Some((&low, rest)) = data.split_first() {
                sum += u32::from(u16::from_be_bytes([high, low]));
                data = rest;
            } else {
                carry_byte = Some(high);
                continue;
            }
        }
        let (pairs, remainder) = data.as_chunks::<2>();
        for pair in pairs {
            sum += u32::from(u16::from_be_bytes(*pair));
        }
        if let [last] = remainder {
            carry_byte = Some(*last);
        }
    }
    if let Some(high) = carry_byte {
        sum += u32::from(u16::from_be_bytes([high, 0]));
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// A 48-bit MAC address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mac(pub [u8; 6]);

impl Mac {
    pub const BROADCAST: Mac = Mac([0xFF; 6]);
    pub fn is_broadcast(&self) -> bool {
        self.0 == [0xFF; 6]
    }
}

/// An IPv4 address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ipv4(pub [u8; 4]);

/// EtherTypes this stack handles.
pub mod ethertype {
    pub const IPV4: u16 = 0x0800;
    pub const ARP: u16 = 0x0806;
}

/// IPv4 protocol numbers.
pub mod protocol {
    pub const ICMP: u8 = 1;
    pub const TCP: u8 = 6;
    pub const UDP: u8 = 17;
}

/// A parsed Ethernet II frame: its header fields and the payload slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EthFrame<'a> {
    pub dst: Mac,
    pub src: Mac,
    pub ethertype: u16,
    pub payload: &'a [u8],
}

/// The 14-byte Ethernet II header length.
pub const ETH_HEADER: usize = 14;

impl<'a> EthFrame<'a> {
    pub fn parse(buf: &'a [u8]) -> Option<Self> {
        if buf.len() < ETH_HEADER {
            return None;
        }
        let mut dst = [0u8; 6];
        let mut src = [0u8; 6];
        dst.copy_from_slice(&buf[0..6]);
        src.copy_from_slice(&buf[6..12]);
        Some(Self {
            dst: Mac(dst),
            src: Mac(src),
            ethertype: u16::from_be_bytes([buf[12], buf[13]]),
            payload: &buf[ETH_HEADER..],
        })
    }

    /// Writes the header into `out` (which must hold the header plus `payload_len`) and
    /// returns the total frame length, or `None` if `out` is too small.
    pub fn write_header(dst: Mac, src: Mac, ethertype: u16, out: &mut [u8]) -> Option<usize> {
        let h = out.get_mut(..ETH_HEADER)?;
        h[0..6].copy_from_slice(&dst.0);
        h[6..12].copy_from_slice(&src.0);
        h[12..14].copy_from_slice(&ethertype.to_be_bytes());
        Some(ETH_HEADER)
    }
}

/// A parsed IPv4 header (no options) and its payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ipv4Packet<'a> {
    pub protocol: u8,
    pub src: Ipv4,
    pub dst: Ipv4,
    pub ttl: u8,
    pub payload: &'a [u8],
}

/// The minimum IPv4 header length (no options).
pub const IPV4_HEADER: usize = 20;

impl<'a> Ipv4Packet<'a> {
    /// Parses an IPv4 header, requiring version 4, a header length of exactly 20 bytes (no
    /// options), a total length within the buffer, and a correct header checksum.
    pub fn parse(buf: &'a [u8]) -> Option<Self> {
        let header = buf.get(..IPV4_HEADER)?;
        if header[0] >> 4 != 4 || header[0] & 0x0F != 5 {
            return None; // not IPv4, or has options
        }
        if checksum(header) != 0 {
            return None;
        }
        let total = u16::from_be_bytes([header[2], header[3]]) as usize;
        if total < IPV4_HEADER || total > buf.len() {
            return None;
        }
        Some(Self {
            protocol: header[9],
            src: Ipv4([header[12], header[13], header[14], header[15]]),
            dst: Ipv4([header[16], header[17], header[18], header[19]]),
            ttl: header[8],
            payload: &buf[IPV4_HEADER..total],
        })
    }

    /// Builds an IPv4 packet into `out`: the header (with its checksum) followed by
    /// `payload`. Returns the total length, or `None` if `out` is too small.
    pub fn write(
        src: Ipv4,
        dst: Ipv4,
        protocol: u8,
        ttl: u8,
        identification: u16,
        payload: &[u8],
        out: &mut [u8],
    ) -> Option<usize> {
        let total = IPV4_HEADER.checked_add(payload.len())?;
        if total > 0xFFFF || out.len() < total {
            return None;
        }
        let (header, body) = out.split_at_mut(IPV4_HEADER);
        header.fill(0);
        header[0] = 0x45; // version 4, header length 5 words
        header[2..4].copy_from_slice(&(total as u16).to_be_bytes());
        header[4..6].copy_from_slice(&identification.to_be_bytes());
        header[6] = 0x40; // don't fragment
        header[8] = ttl;
        header[9] = protocol;
        header[12..16].copy_from_slice(&src.0);
        header[16..20].copy_from_slice(&dst.0);
        let c = checksum(header);
        header[10..12].copy_from_slice(&c.to_be_bytes());
        body[..payload.len()].copy_from_slice(payload);
        Some(total)
    }
}

/// ICMP message types this stack handles.
pub mod icmp_type {
    pub const ECHO_REPLY: u8 = 0;
    pub const ECHO_REQUEST: u8 = 8;
}

/// A parsed ICMP echo message (request or reply).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IcmpEcho<'a> {
    pub kind: u8,
    pub identifier: u16,
    pub sequence: u16,
    pub data: &'a [u8],
}

/// The ICMP header length.
pub const ICMP_HEADER: usize = 8;

impl<'a> IcmpEcho<'a> {
    /// Parses an ICMP echo request or reply, checking the checksum.
    pub fn parse(buf: &'a [u8]) -> Option<Self> {
        let header = buf.get(..ICMP_HEADER)?;
        let kind = header[0];
        if kind != icmp_type::ECHO_REQUEST && kind != icmp_type::ECHO_REPLY {
            return None;
        }
        if header[1] != 0 || checksum(buf) != 0 {
            return None;
        }
        Some(Self {
            kind,
            identifier: u16::from_be_bytes([header[4], header[5]]),
            sequence: u16::from_be_bytes([header[6], header[7]]),
            data: &buf[ICMP_HEADER..],
        })
    }

    /// Builds an ICMP echo message into `out` (header plus `data`), with its checksum.
    /// Returns the length, or `None` if `out` is too small.
    pub fn write(
        kind: u8,
        identifier: u16,
        sequence: u16,
        data: &[u8],
        out: &mut [u8],
    ) -> Option<usize> {
        let total = ICMP_HEADER.checked_add(data.len())?;
        let buf = out.get_mut(..total)?;
        buf.fill(0);
        buf[0] = kind;
        buf[4..6].copy_from_slice(&identifier.to_be_bytes());
        buf[6..8].copy_from_slice(&sequence.to_be_bytes());
        buf[ICMP_HEADER..].copy_from_slice(data);
        let c = checksum(buf);
        buf[2..4].copy_from_slice(&c.to_be_bytes());
        Some(total)
    }

    /// The echo reply to this request: the same identifier, sequence and data, type 0.
    pub fn reply_into(&self, out: &mut [u8]) -> Option<usize> {
        Self::write(
            icmp_type::ECHO_REPLY,
            self.identifier,
            self.sequence,
            self.data,
            out,
        )
    }
}

/// ARP operations.
pub mod arp_op {
    pub const REQUEST: u16 = 1;
    pub const REPLY: u16 = 2;
}

/// A parsed ARP packet for IPv4 over Ethernet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arp {
    pub operation: u16,
    pub sender_mac: Mac,
    pub sender_ip: Ipv4,
    pub target_mac: Mac,
    pub target_ip: Ipv4,
}

/// The ARP packet length for IPv4 over Ethernet.
pub const ARP_LEN: usize = 28;

impl Arp {
    /// Parses an IPv4-over-Ethernet ARP packet (hardware type 1, protocol type 0x0800,
    /// 6-byte MACs, 4-byte IPs).
    pub fn parse(buf: &[u8]) -> Option<Self> {
        let b = buf.get(..ARP_LEN)?;
        let htype = u16::from_be_bytes([b[0], b[1]]);
        let ptype = u16::from_be_bytes([b[2], b[3]]);
        if htype != 1 || ptype != ethertype::IPV4 || b[4] != 6 || b[5] != 4 {
            return None;
        }
        let mac = |at: usize| {
            let mut m = [0u8; 6];
            m.copy_from_slice(&b[at..at + 6]);
            Mac(m)
        };
        let ip = |at: usize| Ipv4([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        Some(Self {
            operation: u16::from_be_bytes([b[6], b[7]]),
            sender_mac: mac(8),
            sender_ip: ip(14),
            target_mac: mac(18),
            target_ip: ip(24),
        })
    }

    /// Writes this ARP packet into `out`; returns [`ARP_LEN`] or `None` if too small.
    pub fn write(&self, out: &mut [u8]) -> Option<usize> {
        let b = out.get_mut(..ARP_LEN)?;
        b[0..2].copy_from_slice(&1u16.to_be_bytes());
        b[2..4].copy_from_slice(&ethertype::IPV4.to_be_bytes());
        b[4] = 6;
        b[5] = 4;
        b[6..8].copy_from_slice(&self.operation.to_be_bytes());
        b[8..14].copy_from_slice(&self.sender_mac.0);
        b[14..18].copy_from_slice(&self.sender_ip.0);
        b[18..24].copy_from_slice(&self.target_mac.0);
        b[24..28].copy_from_slice(&self.target_ip.0);
        Some(ARP_LEN)
    }

    /// The reply to this request, answering with `our_mac` for `target_ip`.
    pub fn reply(&self, our_mac: Mac) -> Arp {
        Arp {
            operation: arp_op::REPLY,
            sender_mac: our_mac,
            sender_ip: self.target_ip,
            target_mac: self.sender_mac,
            target_ip: self.sender_ip,
        }
    }
}

/// A parsed UDP datagram.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Udp<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub payload: &'a [u8],
}

/// The UDP header length.
pub const UDP_HEADER: usize = 8;

impl<'a> Udp<'a> {
    /// Parses a UDP datagram inside an IPv4 packet from `src` to `dst`, checking the length
    /// field and the checksum (a zero checksum means "not computed" and is accepted, as
    /// RFC 768 allows).
    pub fn parse(buf: &'a [u8], src: Ipv4, dst: Ipv4) -> Option<Self> {
        let header = buf.get(..UDP_HEADER)?;
        let length = u16::from_be_bytes([header[4], header[5]]) as usize;
        if length < UDP_HEADER || length > buf.len() {
            return None;
        }
        let datagram = buf.get(..length)?;
        let stored = u16::from_be_bytes([header[6], header[7]]);
        if stored != 0 && udp_checksum(src, dst, datagram) != 0 {
            return None;
        }
        Some(Self {
            src_port: u16::from_be_bytes([header[0], header[1]]),
            dst_port: u16::from_be_bytes([header[2], header[3]]),
            payload: buf.get(UDP_HEADER..length)?,
        })
    }

    /// Builds a UDP datagram into `out` (header plus `payload`) with its checksum. Returns
    /// the total length.
    pub fn write(
        src: Ipv4,
        dst: Ipv4,
        src_port: u16,
        dst_port: u16,
        payload: &[u8],
        out: &mut [u8],
    ) -> Option<usize> {
        let total = UDP_HEADER.checked_add(payload.len())?;
        if total > 0xFFFF {
            return None;
        }
        let buf = out.get_mut(..total)?;
        buf[0..2].copy_from_slice(&src_port.to_be_bytes());
        buf[2..4].copy_from_slice(&dst_port.to_be_bytes());
        buf[4..6].copy_from_slice(&(total as u16).to_be_bytes());
        buf[6..8].copy_from_slice(&[0, 0]); // checksum placeholder
        buf[UDP_HEADER..].copy_from_slice(payload);
        let mut c = udp_checksum(src, dst, buf);
        if c == 0 {
            c = 0xFFFF; // RFC 768: a computed checksum of zero is sent as all ones
        }
        buf[6..8].copy_from_slice(&c.to_be_bytes());
        Some(total)
    }
}

/// The UDP checksum over the IPv4 pseudo-header and the datagram.
pub fn udp_checksum(src: Ipv4, dst: Ipv4, datagram: &[u8]) -> u16 {
    let len = (datagram.len() as u16).to_be_bytes();
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
        protocol::UDP,
        len[0],
        len[1],
    ];
    checksum_parts(&[&pseudo, datagram])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_zeroes_out_and_matches_parts() {
        // A buffer whose 16-bit words sum to 0xFFFF checksums to 0.
        let data = [0x00, 0x01, 0xF2, 0x03, 0xF4, 0xF5, 0xF6, 0xF7];
        let c = checksum(&data);
        // Appending the checksum (as two bytes) makes the whole sum to zero.
        let mut with = data.to_vec();
        with.extend_from_slice(&c.to_be_bytes());
        assert_eq!(
            checksum(&with),
            0,
            "a block carrying its checksum sums to 0"
        );
        // The streaming checksum matches the contiguous one, including an odd split.
        assert_eq!(checksum_parts(&[&data]), c);
        assert_eq!(checksum_parts(&[&data[..3], &data[3..]]), c);
        assert_eq!(checksum_parts(&[&data[..5], &data[5..]]), c, "odd boundary");
        // A known small case: one word 0x0001 -> ~0x0001 = 0xFFFE.
        assert_eq!(checksum(&[0x00, 0x01]), 0xFFFE);
        // An odd trailing byte is padded with zero.
        assert_eq!(checksum(&[0xFF]), checksum(&[0xFF, 0x00]));
    }

    #[test]
    fn ipv4_round_trips_with_a_valid_checksum() {
        let mut buf = [0u8; 100];
        let payload = b"hello origin";
        let n = Ipv4Packet::write(
            Ipv4([10, 0, 0, 2]),
            Ipv4([10, 0, 0, 1]),
            protocol::ICMP,
            64,
            0x1234,
            payload,
            &mut buf,
        )
        .unwrap();
        assert_eq!(n, IPV4_HEADER + payload.len());
        // The header checksum is valid, so parsing accepts it.
        let p = Ipv4Packet::parse(&buf[..n]).unwrap();
        assert_eq!(p.protocol, protocol::ICMP);
        assert_eq!(p.src, Ipv4([10, 0, 0, 2]));
        assert_eq!(p.dst, Ipv4([10, 0, 0, 1]));
        assert_eq!(p.ttl, 64);
        assert_eq!(p.payload, payload);
        // Corrupting any header byte fails the checksum.
        let mut bad = buf;
        bad[12] ^= 0x01;
        assert_eq!(Ipv4Packet::parse(&bad[..n]), None);
        // A header claiming options (IHL != 5) is refused.
        let mut opts = buf;
        opts[0] = 0x46;
        assert_eq!(Ipv4Packet::parse(&opts[..n]), None);
    }

    #[test]
    fn icmp_echo_builds_parses_and_replies() {
        let mut req = [0u8; 64];
        let data = b"ping-payload-1234";
        let n = IcmpEcho::write(icmp_type::ECHO_REQUEST, 0xABCD, 7, data, &mut req).unwrap();
        let parsed = IcmpEcho::parse(&req[..n]).unwrap();
        assert_eq!(
            (parsed.kind, parsed.identifier, parsed.sequence),
            (icmp_type::ECHO_REQUEST, 0xABCD, 7)
        );
        assert_eq!(parsed.data, data);
        // Build the reply, which must parse as an echo reply with the same id/seq/data.
        let mut reply = [0u8; 64];
        let m = parsed.reply_into(&mut reply).unwrap();
        let r = IcmpEcho::parse(&reply[..m]).unwrap();
        assert_eq!(
            (r.kind, r.identifier, r.sequence, r.data),
            (icmp_type::ECHO_REPLY, 0xABCD, 7, &data[..])
        );
        // A corrupted ICMP checksum is rejected.
        let mut bad = req;
        bad[9] ^= 0xFF;
        assert_eq!(IcmpEcho::parse(&bad[..n]), None);
    }

    #[test]
    fn arp_request_and_reply() {
        let req = Arp {
            operation: arp_op::REQUEST,
            sender_mac: Mac([0x52, 0x54, 0, 0, 0, 1]),
            sender_ip: Ipv4([10, 0, 0, 1]),
            target_mac: Mac([0; 6]),
            target_ip: Ipv4([10, 0, 0, 2]),
        };
        let mut buf = [0u8; 28];
        assert_eq!(req.write(&mut buf), Some(28));
        assert_eq!(Arp::parse(&buf), Some(req));
        let our = Mac([0x52, 0x54, 0, 0, 0, 2]);
        let reply = req.reply(our);
        assert_eq!(reply.operation, arp_op::REPLY);
        assert_eq!(reply.sender_mac, our);
        assert_eq!(reply.sender_ip, Ipv4([10, 0, 0, 2]));
        assert_eq!(reply.target_mac, req.sender_mac);
        assert_eq!(reply.target_ip, Ipv4([10, 0, 0, 1]));
        // A non-Ethernet ARP (hardware type != 1) is refused.
        let mut bad = buf;
        bad[1] = 0;
        assert_eq!(Arp::parse(&bad), None);
        assert_eq!(Arp::parse(&buf[..27]), None, "short");
    }

    /// Every parser survives arbitrary mutated and random input without panicking, and
    /// anything it accepts is self-consistent (CLAUDE.md §72).
    #[test]
    fn mutations_never_panic_and_accepted_packets_are_consistent() {
        use crate::testing::{Rng, mutate};
        let a = Ipv4([10, 0, 0, 1]);
        let b = Ipv4([10, 0, 0, 2]);
        let mut ip = [0u8; 128];
        let ip_len = Ipv4Packet::write(a, b, protocol::ICMP, 64, 1, b"payload", &mut ip).unwrap();
        let mut icmp = [0u8; 64];
        let icmp_len = IcmpEcho::write(icmp_type::ECHO_REQUEST, 1, 2, b"abc", &mut icmp).unwrap();
        let mut udp = [0u8; 64];
        let udp_len = Udp::write(a, b, 1, 2, b"abc", &mut udp).unwrap();
        let mut arp = [0u8; 28];
        Arp {
            operation: arp_op::REQUEST,
            sender_mac: Mac([1; 6]),
            sender_ip: a,
            target_mac: Mac([0; 6]),
            target_ip: b,
        }
        .write(&mut arp)
        .unwrap();

        let mut rng = Rng::new(0x4E_4554);
        let (mut ips, mut udps) = (0u64, 0u64);
        for case in 0..crate::testing::cases(4000) {
            let mut v = match case % 5 {
                0 => ip[..ip_len].to_vec(),
                1 => icmp[..icmp_len].to_vec(),
                2 => udp[..udp_len].to_vec(),
                3 => arp.to_vec(),
                _ => alloc::vec![rng.byte(); (rng.next() % 40) as usize],
            };
            mutate(&mut v, &mut rng);
            // None of these may panic on any input.
            let _ = EthFrame::parse(&v);
            let _ = IcmpEcho::parse(&v);
            let _ = Arp::parse(&v);
            if let Some(p) = Ipv4Packet::parse(&v) {
                ips += 1;
                // An accepted IPv4 packet: its payload lies within what we fed.
                assert!(p.payload.len() <= v.len(), "case {case}");
            }
            if let Some(d) = Udp::parse(&v, a, b) {
                udps += 1;
                assert!(UDP_HEADER + d.payload.len() <= v.len(), "case {case}");
            }
        }
        // The valid seeds, unmutated, still parse (the mutator can also leave them alone).
        assert!(
            ips > 0 && udps > 0,
            "{ips} ipv4, {udps} udp accepted across the run"
        );
    }

    #[test]
    fn udp_round_trips_and_checks_the_pseudo_header() {
        let a = Ipv4([192, 168, 1, 2]);
        let b = Ipv4([192, 168, 1, 1]);
        let mut buf = [0u8; 64];
        let payload = b"origin udp";
        let n = Udp::write(a, b, 12345, 53, payload, &mut buf).unwrap();
        assert_eq!(n, UDP_HEADER + payload.len());
        let d = Udp::parse(&buf[..n], a, b).unwrap();
        assert_eq!(
            (d.src_port, d.dst_port, d.payload),
            (12345, 53, &payload[..])
        );
        // The checksum binds the addresses and the bytes.
        assert_eq!(
            Udp::parse(&buf[..n], a, a),
            None,
            "wrong pseudo-header address"
        );
        let mut bad = buf;
        bad[9] ^= 0xFF;
        assert_eq!(Udp::parse(&bad[..n], a, b), None, "corrupted payload");
        // A zero stored checksum is accepted (checksum not computed).
        let mut zero = buf;
        zero[6..8].copy_from_slice(&[0, 0]);
        assert!(Udp::parse(&zero[..n], a, b).is_some());
        // A length field past the buffer is refused.
        let mut longlen = buf;
        longlen[4..6].copy_from_slice(&0xFFFFu16.to_be_bytes());
        assert_eq!(Udp::parse(&longlen[..n], a, b), None);
    }

    #[test]
    fn a_full_ethernet_ipv4_icmp_echo_is_handled_by_loopback() {
        // Build eth + IPv4 + ICMP echo request from host A to host B.
        let a_mac = Mac([0x52, 0x54, 0, 0, 0, 0x0A]);
        let b_mac = Mac([0x52, 0x54, 0, 0, 0, 0x0B]);
        let a_ip = Ipv4([10, 0, 0, 10]);
        let b_ip = Ipv4([10, 0, 0, 11]);
        let payload = b"origin-os ping";

        let mut icmp = [0u8; 64];
        let icmp_len = IcmpEcho::write(icmp_type::ECHO_REQUEST, 1, 1, payload, &mut icmp).unwrap();
        let mut ip = [0u8; 128];
        let ip_len = Ipv4Packet::write(
            a_ip,
            b_ip,
            protocol::ICMP,
            64,
            1,
            &icmp[..icmp_len],
            &mut ip,
        )
        .unwrap();
        let mut frame = [0u8; 160];
        let eth_len = EthFrame::write_header(b_mac, a_mac, ethertype::IPV4, &mut frame).unwrap();
        frame[eth_len..eth_len + ip_len].copy_from_slice(&ip[..ip_len]);
        let frame_len = eth_len + ip_len;

        // Host B receives the frame and produces an echo reply.
        let eth = EthFrame::parse(&frame[..frame_len]).unwrap();
        assert_eq!(eth.ethertype, ethertype::IPV4);
        assert_eq!(eth.dst, b_mac);
        let packet = Ipv4Packet::parse(eth.payload).unwrap();
        assert_eq!(packet.protocol, protocol::ICMP);
        let echo = IcmpEcho::parse(packet.payload).unwrap();
        assert_eq!(echo.kind, icmp_type::ECHO_REQUEST);

        // Reply: swap addresses, echo the data.
        let mut r_icmp = [0u8; 64];
        let r_icmp_len = echo.reply_into(&mut r_icmp).unwrap();
        let mut r_ip = [0u8; 128];
        let r_ip_len = Ipv4Packet::write(
            b_ip,
            a_ip,
            protocol::ICMP,
            64,
            2,
            &r_icmp[..r_icmp_len],
            &mut r_ip,
        )
        .unwrap();
        let mut r_frame = [0u8; 160];
        let r_eth = EthFrame::write_header(a_mac, b_mac, ethertype::IPV4, &mut r_frame).unwrap();
        r_frame[r_eth..r_eth + r_ip_len].copy_from_slice(&r_ip[..r_ip_len]);

        // Host A receives the reply and matches it to its request.
        let reply_eth = EthFrame::parse(&r_frame[..r_eth + r_ip_len]).unwrap();
        let reply_ip = Ipv4Packet::parse(reply_eth.payload).unwrap();
        assert_eq!((reply_ip.src, reply_ip.dst), (b_ip, a_ip));
        let reply_echo = IcmpEcho::parse(reply_ip.payload).unwrap();
        assert_eq!(reply_echo.kind, icmp_type::ECHO_REPLY);
        assert_eq!(
            reply_echo.data, payload,
            "the payload survived the round trip"
        );
    }
}
