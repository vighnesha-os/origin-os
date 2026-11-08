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

use crate::net::Ipv4;

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

/// The client's state.
#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    Init,
    Selecting { xid: u32 },
    Requesting { xid: u32 },
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
}

impl Client {
    pub fn new(mac: [u8; 6]) -> Self {
        Self {
            mac,
            state: State::Init,
        }
    }

    /// Begins acquisition with `xid`; returns the DISCOVER to broadcast.
    pub fn start(&mut self, xid: u32) -> Vec<u8> {
        self.state = State::Selecting { xid };
        discover(xid, self.mac)
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
        match (&self.state, reply.message_type) {
            (State::Selecting { xid }, Some(MessageType::Offer)) if *xid == reply.xid => {
                let server = reply.lease.server.unwrap_or(Ipv4([0, 0, 0, 0]));
                let packet = request(*xid, self.mac, reply.your_address, server);
                self.state = State::Requesting { xid: *xid };
                Action::Send(packet)
            }
            (State::Requesting { xid }, Some(MessageType::Ack)) if *xid == reply.xid => {
                self.state = State::Bound(reply.lease.clone());
                Action::Bound(reply.lease)
            }
            (State::Requesting { xid }, Some(MessageType::Nak)) if *xid == reply.xid => {
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

    /// Builds a server reply (OFFER/ACK/NAK) with the given options.
    fn server_reply(ty: MessageType, xid: u32, yiaddr: Ipv4, server: Ipv4, lease: u32) -> Vec<u8> {
        let mut m = alloc::vec![0u8; BOOTP_LEN];
        m[0] = BOOTREPLY;
        m[1] = 1;
        m[2] = 6;
        m[OFF_XID..OFF_XID + 4].copy_from_slice(&xid.to_be_bytes());
        m[OFF_YIADDR..OFF_YIADDR + 4].copy_from_slice(&yiaddr.0);
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
        let mut client = Client::new([1, 2, 3, 4, 5, 6]);
        client.start(0x1111_1111);
        // An OFFER with the wrong xid is ignored.
        let offer_bad = server_reply(
            MessageType::Offer,
            0x2222_2222,
            ip(10, 0, 0, 2),
            ip(10, 0, 0, 1),
            60,
        );
        assert_eq!(client.on_receive(&offer_bad), Action::Ignore);
        // An ACK while still SELECTING (no OFFER seen) is ignored.
        let ack_early = server_reply(
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
        let mut client = Client::new([1, 2, 3, 4, 5, 6]);
        client.start(7);
        let offer = server_reply(MessageType::Offer, 7, ip(10, 0, 0, 9), ip(10, 0, 0, 1), 60);
        assert!(matches!(client.on_receive(&offer), Action::Send(_)));
        let nak = server_reply(MessageType::Nak, 7, ip(0, 0, 0, 0), ip(10, 0, 0, 1), 0);
        assert_eq!(client.on_receive(&nak), Action::Restart);
        assert!(client.lease().is_none());
    }

    /// Mutated replies through every client state: what `parse` returns came from the
    /// bytes, and the state machine only ever binds a lease for its own transaction
    /// (CLAUDE.md §72).
    #[test]
    fn mutated_replies_keep_their_invariants_through_the_client() {
        use crate::testing::{Rng, mutate};
        let base = server_reply(MessageType::Offer, 7, ip(10, 0, 0, 9), ip(10, 0, 0, 1), 60);
        let ack = server_reply(MessageType::Ack, 7, ip(10, 0, 0, 9), ip(10, 0, 0, 1), 60);
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
                    }
                    Action::Send(_) | Action::Restart | Action::Ignore => {}
                }
            }
        }
        assert!(parsed > 1000 && bound > 100, "{parsed}/{bound}");
    }

    #[test]
    fn mutated_replies_never_panic() {
        let base = server_reply(MessageType::Ack, 42, ip(10, 0, 0, 2), ip(10, 0, 0, 1), 3600);
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
