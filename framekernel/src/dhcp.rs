//! A DHCP client: message build/parse and the lease state machine (CLAUDE.md §54 DHCP).
//! Builds a DISCOVER and a REQUEST, parses the server's OFFER/ACK/NAK (RFC 2131 BOOTP
//! layout plus the options after the magic cookie), and drives the INIT → SELECTING →
//! REQUESTING → BOUND states, yielding the leased address, mask, router, DNS servers and
//! lease time. Every field and option is bounds-checked, so a malformed reply is ignored,
//! never a panic (untrusted input, §34). Pure, no `unsafe`; `alloc` for the built packets.
//!
//! This is the client *logic*; a NIC driver and the UDP socket that carry it on the wire
//! come later. The options this client sends and reads are the common ones (message type,
//! requested address, server id, lease time, subnet mask, router, DNS).

extern crate alloc;
use alloc::vec::Vec;

use crate::net::{EthFrame, Ipv4, Ipv4Packet, Mac, Udp, ethertype, protocol};

/// The UDP ports of the exchange (RFC 2131 §4.1): a reply comes from the server's to the
/// client's.
pub const SERVER_PORT: u16 = 67;
pub const CLIENT_PORT: u16 = 68;

/// How many times one acquisition may start over after a NAK before it gives up.
pub const MAX_RESTARTS: u32 = 3;

/// The DHCP options magic cookie (RFC 2131 §3), at offset 236.
const MAGIC: [u8; 4] = [99, 130, 83, 99];
/// The fixed BOOTP area length, before the magic cookie and options.
const BOOTP_LEN: usize = 236;
const OFF_XID: usize = 4;
const OFF_YIADDR: usize = 16;
const OFF_CHADDR: usize = 28;

/// BOOTP op codes.
const BOOTREQUEST: u8 = 1;
const BOOTREPLY: u8 = 2;

/// DHCP option codes (RFC 2132).
mod opt {
    pub(super) const SUBNET: u8 = 1;
    pub(super) const ROUTER: u8 = 3;
    pub(super) const DNS: u8 = 6;
    pub(super) const REQUESTED_IP: u8 = 50;
    pub(super) const LEASE_TIME: u8 = 51;
    pub(super) const MESSAGE_TYPE: u8 = 53;
    pub(super) const SERVER_ID: u8 = 54;
    pub(super) const PARAM_LIST: u8 = 55;
    pub(super) const END: u8 = 255;
}

/// DHCP message types (option 53).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageType {
    Discover,
    Offer,
    Request,
    Decline,
    Ack,
    Nak,
    Release,
    Inform,
}

impl MessageType {
    fn code(self) -> u8 {
        match self {
            MessageType::Discover => 1,
            MessageType::Offer => 2,
            MessageType::Request => 3,
            MessageType::Decline => 4,
            MessageType::Ack => 5,
            MessageType::Nak => 6,
            MessageType::Release => 7,
            MessageType::Inform => 8,
        }
    }
    fn from_code(c: u8) -> Option<MessageType> {
        Some(match c {
            1 => MessageType::Discover,
            2 => MessageType::Offer,
            3 => MessageType::Request,
            4 => MessageType::Decline,
            5 => MessageType::Ack,
            6 => MessageType::Nak,
            7 => MessageType::Release,
            8 => MessageType::Inform,
            _ => return None,
        })
    }
}

/// The lease a server granted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub address: Ipv4,
    pub subnet_mask: Option<Ipv4>,
    pub router: Option<Ipv4>,
    pub dns: Vec<Ipv4>,
    pub lease_secs: Option<u32>,
    pub server: Option<Ipv4>,
}

/// A parsed server reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub xid: u32,
    pub your_address: Ipv4,
    pub message_type: Option<MessageType>,
    pub lease: Lease,
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *b.get(at)?,
        *b.get(at + 1)?,
        *b.get(at + 2)?,
        *b.get(at + 3)?,
    ]))
}
fn ipv4(b: &[u8], at: usize) -> Option<Ipv4> {
    Some(Ipv4([
        *b.get(at)?,
        *b.get(at + 1)?,
        *b.get(at + 2)?,
        *b.get(at + 3)?,
    ]))
}

/// Starts a BOOTREQUEST with the fixed fields and the magic cookie; options follow.
fn base_request(xid: u32, mac: [u8; 6]) -> Vec<u8> {
    let mut m = alloc::vec![0u8; BOOTP_LEN];
    m[0] = BOOTREQUEST;
    m[1] = 1; // htype: Ethernet
    m[2] = 6; // hlen
    m[OFF_XID..OFF_XID + 4].copy_from_slice(&xid.to_be_bytes());
    m[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // broadcast flag
    m[OFF_CHADDR..OFF_CHADDR + 6].copy_from_slice(&mac);
    m.extend_from_slice(&MAGIC);
    m
}

fn push_option(m: &mut Vec<u8>, code: u8, value: &[u8]) {
    m.push(code);
    m.push(value.len() as u8);
    m.extend_from_slice(value);
}

/// Builds a DHCPDISCOVER.
pub fn discover(xid: u32, mac: [u8; 6]) -> Vec<u8> {
    let mut m = base_request(xid, mac);
    push_option(&mut m, opt::MESSAGE_TYPE, &[MessageType::Discover.code()]);
    push_option(
        &mut m,
        opt::PARAM_LIST,
        &[opt::SUBNET, opt::ROUTER, opt::DNS],
    );
    m.push(opt::END);
    m
}

/// Builds a DHCPREQUEST for `requested` from `server` (the SELECTING-state request).
pub fn request(xid: u32, mac: [u8; 6], requested: Ipv4, server: Ipv4) -> Vec<u8> {
    let mut m = base_request(xid, mac);
    push_option(&mut m, opt::MESSAGE_TYPE, &[MessageType::Request.code()]);
    push_option(&mut m, opt::REQUESTED_IP, &requested.0);
    push_option(&mut m, opt::SERVER_ID, &server.0);
    push_option(
        &mut m,
        opt::PARAM_LIST,
        &[opt::SUBNET, opt::ROUTER, opt::DNS],
    );
    m.push(opt::END);
    m
}

/// Parses a server reply (BOOTREPLY). Returns `None` if it is not a well-formed reply.
pub fn parse(msg: &[u8]) -> Option<Reply> {
    if msg.first().copied()? != BOOTREPLY {
        return None;
    }
    if msg.get(BOOTP_LEN..BOOTP_LEN + 4)? != MAGIC {
        return None;
    }
    let xid = be32(msg, OFF_XID)?;
    let your_address = ipv4(msg, OFF_YIADDR)?;

    let mut lease = Lease {
        address: your_address,
        subnet_mask: None,
        router: None,
        dns: Vec::new(),
        lease_secs: None,
        server: None,
    };
    let mut message_type = None;

    // Walk the options (TLV; PAD=0 has no length, END=255 stops).
    let mut i = BOOTP_LEN + 4;
    while let Some(&code) = msg.get(i) {
        match code {
            opt::END => break,
            0 => {
                i += 1; // PAD
                continue;
            }
            _ => {}
        }
        let len = usize::from(*msg.get(i + 1)?);
        let value = msg.get(i + 2..i + 2 + len)?;
        match code {
            opt::MESSAGE_TYPE if len == 1 => message_type = MessageType::from_code(value[0]),
            opt::SUBNET if len == 4 => lease.subnet_mask = ipv4(value, 0),
            opt::ROUTER if len >= 4 => lease.router = ipv4(value, 0),
            opt::DNS => {
                let mut j = 0;
                while j + 4 <= value.len() {
                    lease
                        .dns
                        .push(Ipv4([value[j], value[j + 1], value[j + 2], value[j + 3]]));
                    j += 4;
                }
            }
            opt::LEASE_TIME if len == 4 => lease.lease_secs = be32(value, 0),
            opt::SERVER_ID if len == 4 => lease.server = ipv4(value, 0),
            _ => {}
        }
        i += 2 + len;
    }

    Some(Reply {
        xid,
        your_address,
        message_type,
        lease,
    })
}

/// The DHCP message in a received Ethernet frame, or `None` unless the frame is a whole,
/// checksummed IPv4/UDP datagram from the server port to the client port, addressed to this
/// client. Until it has an address the client is reached at the limited broadcast address or
/// at the address the message offers (RFC 2131 §4.1), and at its own MAC or the broadcast MAC.
pub fn message_in_frame(frame: &[u8], mac: [u8; 6]) -> Option<&[u8]> {
    let eth = EthFrame::parse(frame)?;
    if eth.ethertype != ethertype::IPV4 || !(eth.dst == Mac(mac) || eth.dst.is_broadcast()) {
        return None;
    }
    let packet = Ipv4Packet::parse(eth.payload)?;
    if packet.protocol != protocol::UDP {
        return None;
    }
    let udp = Udp::parse(packet.payload, packet.src, packet.dst)?;
    if udp.src_port != SERVER_PORT || udp.dst_port != CLIENT_PORT {
        return None;
    }
    let addressed = packet.dst == Ipv4([255; 4]) || packet.dst == ipv4(udp.payload, OFF_YIADDR)?;
    addressed.then_some(udp.payload)
}

/// Whether a host can use `address` as its own, or a client can send to it as a server:
/// not "this network" (0.0.0.0/8), loopback (127.0.0.0/8), multicast or reserved (224.0.0.0
/// and up, the limited broadcast among them; RFC 1122 §3.2.1.3), and, under a `mask` wider
/// than a point-to-point /31, not the subnet's own network or broadcast address.
fn usable_address(address: Ipv4, mask: Option<Ipv4>) -> bool {
    let first = address.0[0];
    if first == 0 || first == 127 || first >= 224 {
        return false;
    }
    let Some(mask) = mask else {
        return true;
    };
    let host_mask = !u32::from_be_bytes(mask.0);
    let host = u32::from_be_bytes(address.0) & host_mask;
    host_mask <= 1 || (host != 0 && host != host_mask)
}

/// The client's state.
#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    Init,
    Selecting {
        xid: u32,
    },
    /// The REQUEST for `offered` from `server` is out: only their ACK binds.
    Requesting {
        xid: u32,
        offered: Ipv4,
        server: Ipv4,
    },
    Bound(Lease),
}

/// What a received reply tells the caller to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Send this packet (a REQUEST in response to an OFFER).
    Send(Vec<u8>),
    /// The lease is bound; configure the interface with it.
    Bound(Lease),
    /// The server refused (NAK); the caller should restart from DISCOVER.
    Restart,
    /// Nothing to do (wrong xid, a stray or unexpected message).
    Ignore,
}

/// A DHCP client bound to one interface's MAC.
#[derive(Clone, Debug)]
pub struct Client {
    mac: [u8; 6],
    state: State,
    /// Restarts this acquisition has used (see [`Client::restart`]).
    restarts: u32,
}

impl Client {
    pub fn new(mac: [u8; 6]) -> Self {
        Self {
            mac,
            state: State::Init,
            restarts: 0,
        }
    }

    /// Begins acquisition with `xid`; returns the DISCOVER to broadcast.
    pub fn start(&mut self, xid: u32) -> Vec<u8> {
        self.restarts = 0;
        self.state = State::Selecting { xid };
        discover(xid, self.mac)
    }

    /// Starts over after a NAK ([`Action::Restart`]) as a new transaction `xid`; returns the
    /// DISCOVER to broadcast. `None` once this acquisition has used [`MAX_RESTARTS`], or when
    /// no NAK is outstanding: a server that refuses every request cannot keep it going.
    pub fn restart(&mut self, xid: u32) -> Option<Vec<u8>> {
        if self.state != State::Init || self.restarts >= MAX_RESTARTS {
            return None;
        }
        self.restarts += 1;
        self.state = State::Selecting { xid };
        Some(discover(xid, self.mac))
    }

    /// Whether the client has a bound lease.
    pub fn lease(&self) -> Option<&Lease> {
        match &self.state {
            State::Bound(l) => Some(l),
            _ => None,
        }
    }

    /// Feeds a received server reply and advances the state machine.
    pub fn on_receive(&mut self, msg: &[u8]) -> Action {
        let Some(reply) = parse(msg) else {
            return Action::Ignore;
        };
        // A server echoes the client's hardware address; any other is another client's reply.
        if msg.get(OFF_CHADDR..OFF_CHADDR + self.mac.len()) != Some(&self.mac[..]) {
            return Action::Ignore;
        }
        match (&self.state, reply.message_type) {
            (State::Selecting { xid }, Some(MessageType::Offer)) if *xid == reply.xid => {
                // The REQUEST must name the server that made the offer (RFC 2131 §4.3.2): an
                // OFFER without one, or for an address no host can use, is passed over.
                let Some(server) = reply.lease.server else {
                    return Action::Ignore;
                };
                let offered = reply.your_address;
                if !usable_address(offered, reply.lease.subnet_mask)
                    || !usable_address(server, None)
                {
                    return Action::Ignore;
                }
                let xid = *xid;
                let packet = request(xid, self.mac, offered, server);
                self.state = State::Requesting {
                    xid,
                    offered,
                    server,
                };
                Action::Send(packet)
            }
            (
                State::Requesting {
                    xid,
                    offered,
                    server,
                },
                Some(MessageType::Ack),
            ) if *xid == reply.xid
                && reply.your_address == *offered
                && reply.lease.server == Some(*server)
                && usable_address(reply.your_address, reply.lease.subnet_mask) =>
            {
                self.state = State::Bound(reply.lease.clone());
                Action::Bound(reply.lease)
            }
            // A NAK naming another server is not an answer to this request. One naming none is
            // taken: RFC 2131 requires the option, but QEMU's server leaves it out.
            (State::Requesting { xid, server, .. }, Some(MessageType::Nak))
                if *xid == reply.xid && reply.lease.server.is_none_or(|s| s == *server) =>
            {
                self.state = State::Init;
                Action::Restart
            }
            _ => Action::Ignore,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(a: u8, b: u8, c: u8, d: u8) -> Ipv4 {
        Ipv4([a, b, c, d])
    }

    /// Builds a server reply (OFFER/ACK/NAK) to client `mac` with the given options.
    fn server_reply(
        mac: [u8; 6],
        ty: MessageType,
        xid: u32,
        yiaddr: Ipv4,
        server: Ipv4,
        lease: u32,
    ) -> Vec<u8> {
        let mut m = alloc::vec![0u8; BOOTP_LEN];
        m[0] = BOOTREPLY;
        m[1] = 1;
        m[2] = 6;
        m[OFF_XID..OFF_XID + 4].copy_from_slice(&xid.to_be_bytes());
        m[OFF_YIADDR..OFF_YIADDR + 4].copy_from_slice(&yiaddr.0);
        m[OFF_CHADDR..OFF_CHADDR + 6].copy_from_slice(&mac);
        m.extend_from_slice(&MAGIC);
        push_option(&mut m, opt::MESSAGE_TYPE, &[ty.code()]);
        push_option(&mut m, opt::SERVER_ID, &server.0);
        if ty != MessageType::Nak {
            push_option(&mut m, opt::SUBNET, &[255, 255, 255, 0]);
            push_option(&mut m, opt::ROUTER, &ip(192, 168, 1, 1).0);
            let mut dns = ip(8, 8, 8, 8).0.to_vec();
            dns.extend_from_slice(&ip(1, 1, 1, 1).0);
            push_option(&mut m, opt::DNS, &dns);
            push_option(&mut m, opt::LEASE_TIME, &lease.to_be_bytes());
        }
        m.push(opt::END);
        m
    }

    #[test]
    fn full_discover_offer_request_ack_exchange() {
        let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let mut client = Client::new(mac);

        // DISCOVER.
        let disc = client.start(0xA1B2_C3D4);
        let parsed_disc = {
            // The client sends a BOOTREQUEST, which `parse` (expecting BOOTREPLY) rejects;
            // check the fields directly instead.
            assert_eq!(disc[0], BOOTREQUEST);
            assert_eq!(&disc[OFF_XID..OFF_XID + 4], &0xA1B2_C3D4u32.to_be_bytes());
            assert_eq!(&disc[OFF_CHADDR..OFF_CHADDR + 6], &mac);
            assert_eq!(&disc[BOOTP_LEN..BOOTP_LEN + 4], &MAGIC);
            disc
        };
        let _ = parsed_disc;

        // OFFER → the client must send a REQUEST for the offered address.
        let offer = server_reply(
            mac,
            MessageType::Offer,
            0xA1B2_C3D4,
            ip(192, 168, 1, 50),
            ip(192, 168, 1, 1),
            3600,
        );
        let action = client.on_receive(&offer);
        let Action::Send(req) = action else {
            panic!("expected a REQUEST, got {action:?}");
        };
        let req_parsed = parse(&req);
        assert!(
            req_parsed.is_none(),
            "a REQUEST is a BOOTREQUEST, not a reply"
        );
        // Inspect the REQUEST options directly: type 3, requested 192.168.1.50, server id.
        assert_eq!(req[0], BOOTREQUEST);
        assert!(find_option(&req, opt::MESSAGE_TYPE) == Some(alloc::vec![3]));
        assert_eq!(
            find_option(&req, opt::REQUESTED_IP),
            Some(ip(192, 168, 1, 50).0.to_vec())
        );
        assert_eq!(
            find_option(&req, opt::SERVER_ID),
            Some(ip(192, 168, 1, 1).0.to_vec())
        );

        // ACK → bound, with the full lease.
        let ack = server_reply(
            mac,
            MessageType::Ack,
            0xA1B2_C3D4,
            ip(192, 168, 1, 50),
            ip(192, 168, 1, 1),
            3600,
        );
        let action = client.on_receive(&ack);
        let Action::Bound(lease) = action else {
            panic!("expected Bound, got {action:?}");
        };
        assert_eq!(lease.address, ip(192, 168, 1, 50));
        assert_eq!(lease.subnet_mask, Some(ip(255, 255, 255, 0)));
        assert_eq!(lease.router, Some(ip(192, 168, 1, 1)));
        assert_eq!(lease.dns, alloc::vec![ip(8, 8, 8, 8), ip(1, 1, 1, 1)]);
        assert_eq!(lease.lease_secs, Some(3600));
        assert_eq!(client.lease().map(|l| l.address), Some(ip(192, 168, 1, 50)));
    }

    /// Reads one option's value from a built message (test helper).
    fn find_option(msg: &[u8], want: u8) -> Option<Vec<u8>> {
        let mut i = BOOTP_LEN + 4;
        while let Some(&code) = msg.get(i) {
            if code == opt::END {
                break;
            }
            if code == 0 {
                i += 1;
                continue;
            }
            let len = usize::from(*msg.get(i + 1)?);
            let value = msg.get(i + 2..i + 2 + len)?;
            if code == want {
                return Some(value.to_vec());
            }
            i += 2 + len;
        }
        None
    }

    #[test]
    fn ignores_wrong_xid_and_unexpected_messages() {
        let mac = [1, 2, 3, 4, 5, 6];
        let mut client = Client::new(mac);
        client.start(0x1111_1111);
        // An OFFER with the wrong xid is ignored.
        let offer_bad = server_reply(
            mac,
            MessageType::Offer,
            0x2222_2222,
            ip(10, 0, 0, 2),
            ip(10, 0, 0, 1),
            60,
        );
        assert_eq!(client.on_receive(&offer_bad), Action::Ignore);
        // An ACK while still SELECTING (no OFFER seen) is ignored.
        let ack_early = server_reply(
            mac,
            MessageType::Ack,
            0x1111_1111,
            ip(10, 0, 0, 2),
            ip(10, 0, 0, 1),
            60,
        );
        assert_eq!(client.on_receive(&ack_early), Action::Ignore);
        // Garbage is ignored, not panicked on.
        assert_eq!(client.on_receive(&[0u8; 10]), Action::Ignore);
        assert_eq!(client.on_receive(&[]), Action::Ignore);
    }

    #[test]
    fn nak_restarts() {
        let mac = [1, 2, 3, 4, 5, 6];
        let mut client = Client::new(mac);
        client.start(7);
        let offer = server_reply(
            mac,
            MessageType::Offer,
            7,
            ip(10, 0, 0, 9),
            ip(10, 0, 0, 1),
            60,
        );
        assert!(matches!(client.on_receive(&offer), Action::Send(_)));
        let nak = server_reply(mac, MessageType::Nak, 7, ip(0, 0, 0, 0), ip(10, 0, 0, 1), 0);
        assert_eq!(client.on_receive(&nak), Action::Restart);
        assert!(client.lease().is_none());
    }

    /// The client's MAC in the tests below, and the transaction they run.
    const MAC: [u8; 6] = [0x52, 0x54, 0x00, 0xAA, 0xBB, 0xCC];
    const XID: u32 = 0x5EED_0001;

    fn reply(ty: MessageType, xid: u32, yiaddr: Ipv4, server: Ipv4) -> Vec<u8> {
        server_reply(MAC, ty, xid, yiaddr, server, 3600)
    }

    /// Replaces the value of option `code` in a built message with `value` (same length).
    fn patch_option(msg: &mut [u8], code: u8, value: &[u8]) {
        let mut i = BOOTP_LEN + 4;
        while msg[i] != opt::END {
            let len = usize::from(msg[i + 1]);
            if msg[i] == code {
                assert_eq!(len, value.len());
                msg[i + 2..i + 2 + len].copy_from_slice(value);
                return;
            }
            i += 2 + len;
        }
        panic!("option {code} is not in this message");
    }

    /// Blanks option `code` out of a built message with PAD bytes.
    fn remove_option(msg: &mut [u8], code: u8) {
        let mut i = BOOTP_LEN + 4;
        while msg[i] != opt::END {
            let len = usize::from(msg[i + 1]);
            if msg[i] == code {
                msg[i..i + 2 + len].fill(0);
                return;
            }
            i += 2 + len;
        }
        panic!("option {code} is not in this message");
    }

    /// A client that has requested 10.0.0.9 from 10.0.0.1.
    fn requesting() -> Client {
        let mut client = Client::new(MAC);
        client.start(XID);
        let offer = reply(MessageType::Offer, XID, ip(10, 0, 0, 9), ip(10, 0, 0, 1));
        assert!(matches!(client.on_receive(&offer), Action::Send(_)));
        client
    }

    /// `chaddr` must be this client's: a reply with the right xid for another station, or
    /// with no hardware address at all, is not for it.
    #[test]
    fn a_reply_for_another_hardware_address_is_ignored() {
        let mut client = Client::new(MAC);
        client.start(XID);
        let mut offer = reply(MessageType::Offer, XID, ip(10, 0, 0, 9), ip(10, 0, 0, 1));
        for other in [[0u8; 6], [0x52, 0x54, 0x00, 0xAA, 0xBB, 0xCD]] {
            offer[OFF_CHADDR..OFF_CHADDR + 6].copy_from_slice(&other);
            assert_eq!(client.on_receive(&offer), Action::Ignore, "{other:02x?}");
        }
        // Still selecting, so the client's own OFFER goes through.
        offer[OFF_CHADDR..OFF_CHADDR + 6].copy_from_slice(&MAC);
        assert!(matches!(client.on_receive(&offer), Action::Send(_)));
        // And an ACK for another client does not bind it.
        let mut ack = reply(MessageType::Ack, XID, ip(10, 0, 0, 9), ip(10, 0, 0, 1));
        ack[OFF_CHADDR..OFF_CHADDR + 6].copy_from_slice(&[9; 6]);
        assert_eq!(client.on_receive(&ack), Action::Ignore);
        assert!(client.lease().is_none());
    }

    /// Only the ACK for the address that was offered, from the server it was requested of,
    /// binds the lease; any other leaves the request outstanding.
    #[test]
    fn an_ack_must_match_the_offered_address_and_server() {
        let mut client = requesting();
        let other_address = reply(MessageType::Ack, XID, ip(10, 0, 0, 10), ip(10, 0, 0, 1));
        assert_eq!(client.on_receive(&other_address), Action::Ignore);
        let other_server = reply(MessageType::Ack, XID, ip(10, 0, 0, 9), ip(10, 0, 0, 2));
        assert_eq!(client.on_receive(&other_server), Action::Ignore);
        let mut no_server = reply(MessageType::Ack, XID, ip(10, 0, 0, 9), ip(10, 0, 0, 1));
        remove_option(&mut no_server, opt::SERVER_ID);
        assert_eq!(client.on_receive(&no_server), Action::Ignore);
        assert!(client.lease().is_none());
        let right = reply(MessageType::Ack, XID, ip(10, 0, 0, 9), ip(10, 0, 0, 1));
        assert!(matches!(client.on_receive(&right), Action::Bound(_)));
        assert_eq!(client.lease().map(|l| l.address), Some(ip(10, 0, 0, 9)));
    }

    /// An address no host can use never gets as far as a REQUEST, let alone Bound, and neither
    /// does a lease whose ACK turns the address into a broadcast or network address.
    #[test]
    fn unusable_addresses_are_refused() {
        let server = ip(10, 0, 0, 1);
        for bad in [
            ip(0, 0, 0, 0),
            ip(0, 1, 2, 3),
            ip(255, 255, 255, 255),
            ip(127, 0, 0, 1),
            ip(224, 0, 0, 1),
            ip(239, 1, 2, 3),
            ip(240, 0, 0, 1),
            // 10.0.0.0/24's own network and broadcast addresses (the fixture's mask is /24).
            ip(10, 0, 0, 0),
            ip(10, 0, 0, 255),
        ] {
            let mut client = Client::new(MAC);
            client.start(XID);
            let offer = reply(MessageType::Offer, XID, bad, server);
            assert_eq!(client.on_receive(&offer), Action::Ignore, "{bad:?}");
        }

        // Usable: an ordinary host, and the top of a /24 whose mask makes it a host address.
        for good in [ip(10, 0, 0, 9), ip(192, 168, 1, 254), ip(223, 1, 2, 3)] {
            let mut client = Client::new(MAC);
            client.start(XID);
            let offer = reply(MessageType::Offer, XID, good, server);
            assert!(
                matches!(client.on_receive(&offer), Action::Send(_)),
                "{good:?}"
            );
        }
        // A point-to-point /31 has no network or broadcast address.
        let mut client = Client::new(MAC);
        client.start(XID);
        let mut offer = reply(MessageType::Offer, XID, ip(10, 0, 0, 8), server);
        patch_option(&mut offer, opt::SUBNET, &[255, 255, 255, 254]);
        assert!(matches!(client.on_receive(&offer), Action::Send(_)));

        // The ACK's own mask governs the lease.
        let mut client = Client::new(MAC);
        client.start(XID);
        let offer = reply(MessageType::Offer, XID, ip(10, 0, 0, 128), server);
        assert!(matches!(client.on_receive(&offer), Action::Send(_)));
        let mut ack = reply(MessageType::Ack, XID, ip(10, 0, 0, 128), server);
        patch_option(&mut ack, opt::SUBNET, &[255, 255, 255, 128]);
        assert_eq!(client.on_receive(&ack), Action::Ignore);
        assert!(client.lease().is_none());
    }

    /// An OFFER that names no real server cannot be answered: the REQUEST would name 0.0.0.0.
    #[test]
    fn an_offer_with_no_usable_server_id_is_refused() {
        let mut client = Client::new(MAC);
        client.start(XID);
        let mut none = reply(MessageType::Offer, XID, ip(10, 0, 0, 9), ip(10, 0, 0, 1));
        remove_option(&mut none, opt::SERVER_ID);
        assert_eq!(client.on_receive(&none), Action::Ignore);
        for bad in [ip(0, 0, 0, 0), ip(255, 255, 255, 255), ip(127, 0, 0, 1)] {
            let offer = reply(MessageType::Offer, XID, ip(10, 0, 0, 9), bad);
            assert_eq!(client.on_receive(&offer), Action::Ignore, "{bad:?}");
        }
        // Still selecting.
        let offer = reply(MessageType::Offer, XID, ip(10, 0, 0, 9), ip(10, 0, 0, 1));
        assert!(matches!(client.on_receive(&offer), Action::Send(_)));
    }

    /// A NAK from the server the request went to restarts the exchange; one from elsewhere
    /// does not, and one naming no server (QEMU's) does.
    #[test]
    fn a_nak_must_come_from_the_requested_server() {
        let mut client = requesting();
        let elsewhere = reply(MessageType::Nak, XID, ip(0, 0, 0, 0), ip(10, 0, 0, 2));
        assert_eq!(client.on_receive(&elsewhere), Action::Ignore);
        let mut anonymous = reply(MessageType::Nak, XID, ip(0, 0, 0, 0), ip(10, 0, 0, 1));
        remove_option(&mut anonymous, opt::SERVER_ID);
        assert_eq!(client.on_receive(&anonymous), Action::Restart);

        let mut other_client = requesting();
        let mut nak = reply(MessageType::Nak, XID, ip(0, 0, 0, 0), ip(10, 0, 0, 1));
        nak[OFF_CHADDR..OFF_CHADDR + 6].copy_from_slice(&[9; 6]);
        assert_eq!(other_client.on_receive(&nak), Action::Ignore);
    }

    /// After a NAK the client starts over as a new transaction, and only `MAX_RESTARTS` times.
    #[test]
    fn a_nak_starts_a_new_transaction_a_bounded_number_of_times() {
        let server = ip(10, 0, 0, 1);
        let mut client = Client::new(MAC);
        let mut xid = XID;
        client.start(xid);
        assert_eq!(client.restart(xid + 1), None, "no NAK is outstanding");

        for round in 0..=MAX_RESTARTS {
            let offer = reply(MessageType::Offer, xid, ip(10, 0, 0, 9), server);
            assert!(matches!(client.on_receive(&offer), Action::Send(_)));
            let nak = reply(MessageType::Nak, xid, ip(0, 0, 0, 0), server);
            assert_eq!(client.on_receive(&nak), Action::Restart);

            let next = xid + 1;
            let Some(discover) = client.restart(next) else {
                assert_eq!(round, MAX_RESTARTS, "gave up early, in round {round}");
                assert!(client.lease().is_none());
                return;
            };
            assert!(round < MAX_RESTARTS, "still restarting in round {round}");
            // A DISCOVER for the new transaction, and the old one is over.
            assert_eq!(discover[BOOTP_LEN + 6], MessageType::Discover.code());
            assert_eq!(&discover[OFF_XID..OFF_XID + 4], &next.to_be_bytes());
            assert_eq!(client.on_receive(&offer), Action::Ignore, "a stale OFFER");
            xid = next;
        }
        panic!("the restarts never ran out");
    }

    /// A frame as a server would send one.
    fn frame(eth_dst: [u8; 6], ip_dst: Ipv4, src_port: u16, dst_port: u16, dhcp: &[u8]) -> Vec<u8> {
        use crate::net::{ETH_HEADER, IPV4_HEADER, UDP_HEADER};
        let server = ip(10, 0, 2, 2);
        let mut udp = alloc::vec![0u8; UDP_HEADER + dhcp.len()];
        Udp::write(server, ip_dst, src_port, dst_port, dhcp, &mut udp).unwrap();
        let mut packet = alloc::vec![0u8; IPV4_HEADER + udp.len()];
        Ipv4Packet::write(server, ip_dst, protocol::UDP, 64, 0, &udp, &mut packet).unwrap();
        let mut out = alloc::vec![0u8; ETH_HEADER + packet.len()];
        let from = Mac([0x52, 0x55, 0x0A, 0, 2, 2]);
        EthFrame::write_header(Mac(eth_dst), from, ethertype::IPV4, &mut out).unwrap();
        out[ETH_HEADER..].copy_from_slice(&packet);
        out
    }

    /// Only a well-formed datagram from port 67 to port 68, addressed to this client,
    /// reaches the state machine.
    #[test]
    fn only_a_valid_reply_addressed_to_the_client_leaves_the_frame() {
        use crate::net::{ETH_HEADER, IPV4_HEADER};
        let offered = ip(10, 0, 2, 15);
        let dhcp = reply(MessageType::Offer, XID, offered, ip(10, 0, 2, 2));
        let broadcast = ip(255, 255, 255, 255);
        let bcast_mac = [0xFF; 6];
        let message = |f: &[u8]| message_in_frame(f, MAC).map(<[u8]>::to_vec);

        // The shapes a server sends: unicast to the offered address, or broadcast.
        let good = frame(MAC, offered, SERVER_PORT, CLIENT_PORT, &dhcp);
        assert_eq!(message(&good).as_deref(), Some(&dhcp[..]));
        let to_all = frame(bcast_mac, broadcast, SERVER_PORT, CLIENT_PORT, &dhcp);
        assert_eq!(message(&to_all).as_deref(), Some(&dhcp[..]));

        // The wrong ports.
        let f = frame(MAC, offered, 1067, CLIENT_PORT, &dhcp);
        assert_eq!(message(&f), None, "a source port other than the server's");
        let f = frame(MAC, offered, SERVER_PORT, 69, &dhcp);
        assert_eq!(
            message(&f),
            None,
            "a destination port other than the client's"
        );

        // Addressed to someone else.
        let other_mac = [0x02, 1, 2, 3, 4, 5];
        let f = frame(other_mac, offered, SERVER_PORT, CLIENT_PORT, &dhcp);
        assert_eq!(message(&f), None, "another station's MAC");
        let f = frame(MAC, ip(10, 0, 2, 16), SERVER_PORT, CLIENT_PORT, &dhcp);
        assert_eq!(message(&f), None, "another host's address");

        // UDP that does not check out: a damaged payload, and a length past the datagram.
        const UDP_START: usize = ETH_HEADER + IPV4_HEADER;
        let mut f = good.clone();
        *f.last_mut().unwrap() ^= 0xFF;
        assert_eq!(message(&f), None, "a failing UDP checksum");
        let mut f = good.clone();
        f[UDP_START + 4..UDP_START + 6].copy_from_slice(&0xFFFFu16.to_be_bytes());
        assert_eq!(message(&f), None, "a UDP length past the datagram");

        // Not IPv4, not UDP, or too short to be anything.
        let mut f = good.clone();
        f[12..14].copy_from_slice(&ethertype::ARP.to_be_bytes());
        assert_eq!(message(&f), None, "an ARP ethertype");
        let mut f = good.clone();
        f[ETH_HEADER + 9] = protocol::TCP;
        f[ETH_HEADER + 10..ETH_HEADER + 12].copy_from_slice(&[0, 0]);
        let c = crate::net::checksum(&f[ETH_HEADER..ETH_HEADER + IPV4_HEADER]);
        f[ETH_HEADER + 10..ETH_HEADER + 12].copy_from_slice(&c.to_be_bytes());
        assert_eq!(message(&f), None, "TCP");
        assert_eq!(message(&good[..ETH_HEADER + 4]), None, "truncated");
    }

    /// Mutated replies through every client state: what `parse` returns came from the
    /// bytes, and the state machine only ever binds a lease for its own transaction
    /// (CLAUDE.md §72).
    #[test]
    fn mutated_replies_keep_their_invariants_through_the_client() {
        use crate::testing::{Rng, mutate};
        let mac = [2; 6];
        let base = server_reply(
            mac,
            MessageType::Offer,
            7,
            ip(10, 0, 0, 9),
            ip(10, 0, 0, 1),
            60,
        );
        let ack = server_reply(
            mac,
            MessageType::Ack,
            7,
            ip(10, 0, 0, 9),
            ip(10, 0, 0, 1),
            60,
        );
        let mut rng = Rng::new(0x4448_4332);
        let (mut parsed, mut bound) = (0u32, 0u32);
        for case in 0..crate::testing::cases(10_000) {
            let mut data = if case % 2 == 0 {
                base.clone()
            } else {
                ack.clone()
            };
            mutate(&mut data, &mut rng);
            if let Some(r) = parse(&data) {
                parsed += 1;
                assert_eq!(data[0], BOOTREPLY, "case {case}");
                assert_eq!(
                    r.xid.to_be_bytes(),
                    data[OFF_XID..OFF_XID + 4],
                    "case {case}"
                );
                assert_eq!(
                    r.your_address.0,
                    data[OFF_YIADDR..OFF_YIADDR + 4],
                    "case {case}"
                );
                assert!(r.lease.dns.len() <= data.len() / 4, "case {case}");
            }
            let mut client = Client::new([2; 6]);
            let _ = client.start(7);
            if matches!(client.on_receive(&base), Action::Send(_)) {
                match client.on_receive(&data) {
                    Action::Bound(l) => {
                        bound += 1;
                        assert_eq!(client.lease(), Some(&l), "case {case}");
                        let r = parse(&data).unwrap();
                        assert_eq!(r.xid, 7, "case {case}: a lease for another transaction");
                        assert_eq!(r.message_type, Some(MessageType::Ack), "case {case}");
                        assert_eq!(data[OFF_CHADDR..OFF_CHADDR + 6], mac, "case {case}");
                        assert_eq!(l.address, ip(10, 0, 0, 9), "case {case}: another address");
                        assert_eq!(l.server, Some(ip(10, 0, 0, 1)), "case {case}");
                    }
                    Action::Send(_) | Action::Restart | Action::Ignore => {}
                }
            }
        }
        assert!(parsed > 1000 && bound > 100, "{parsed}/{bound}");
    }

    #[test]
    fn mutated_replies_never_panic() {
        let base = server_reply(
            [0; 6],
            MessageType::Ack,
            42,
            ip(10, 0, 0, 2),
            ip(10, 0, 0, 1),
            3600,
        );
        let mut rng = crate::testing::Rng::new(0x4448_4350);
        for _ in 0..crate::testing::cases(20_000) {
            let mut data = base.clone();
            crate::testing::mutate(&mut data, &mut rng);
            let _ = parse(&data);
            let mut c = Client::new([0; 6]);
            c.start(42);
            let _ = c.on_receive(&data);
        }
    }
}
