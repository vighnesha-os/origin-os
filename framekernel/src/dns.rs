//! DNS message parsing (CLAUDE.md §54 DNS). Decodes a wire-format message — the header,
//! the questions and the resource records — into owned values, resolving the label
//! compression pointers of RFC 1035 §4.1.4. Every length is checked and, crucially, name
//! decompression is bounded: a pointer must point strictly backwards and the number of
//! jumps is capped, so a malicious self-referential or forward pointer is rejected rather
//! than looped on (untrusted input, §34/§54). Pure, no `unsafe`; `alloc` for the owned
//! names and record lists. A small no-compression builder lives in the tests.
//!
//! This is the read path a resolver client builds on; it does not itself send queries or
//! cache answers.

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

use crate::net::Ipv4;

/// The fixed 12-byte header.
pub const HEADER_LEN: usize = 12;
/// The longest a decoded name may be (RFC 1035 §2.3.4), in bytes of label text plus dots.
const MAX_NAME: usize = 255;
/// A hard cap on compression-pointer jumps, in case the backward-only rule is somehow
/// evaded; the backward rule already guarantees termination.
const MAX_JUMPS: usize = 128;

/// Record and question types this module names (others are kept as their numeric type).
pub mod rtype {
    pub const A: u16 = 1;
    pub const NS: u16 = 2;
    pub const CNAME: u16 = 5;
    pub const PTR: u16 = 12;
    pub const AAAA: u16 = 28;
}

/// The decoded header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub id: u16,
    pub flags: u16,
    pub qdcount: u16,
    pub ancount: u16,
    pub nscount: u16,
    pub arcount: u16,
}

impl Header {
    /// A response (QR bit set) rather than a query.
    pub fn is_response(&self) -> bool {
        self.flags & 0x8000 != 0
    }
    /// The recursion-desired bit.
    pub fn recursion_desired(&self) -> bool {
        self.flags & 0x0100 != 0
    }
    /// The 4-bit response code.
    pub fn rcode(&self) -> u8 {
        (self.flags & 0x000F) as u8
    }
}

/// One question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub name: String,
    pub qtype: u16,
    pub qclass: u16,
}

/// A record's decoded data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RData {
    A(Ipv4),
    Aaaa([u8; 16]),
    /// A domain name (CNAME, NS, PTR), decompressed.
    Name(String),
    /// Any other type: its length (the bytes are left in the message).
    Other(usize),
}

/// One resource record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    pub rtype: u16,
    pub rclass: u16,
    pub ttl: u32,
    pub rdata: RData,
}

/// A fully parsed message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub header: Header,
    pub questions: Vec<Question>,
    pub records: Vec<Record>,
}

fn be16(msg: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*msg.get(at)?, *msg.get(at + 1)?]))
}
fn be32(msg: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *msg.get(at)?,
        *msg.get(at + 1)?,
        *msg.get(at + 2)?,
        *msg.get(at + 3)?,
    ]))
}

/// Parses the header.
pub fn parse_header(msg: &[u8]) -> Option<Header> {
    Some(Header {
        id: be16(msg, 0)?,
        flags: be16(msg, 2)?,
        qdcount: be16(msg, 4)?,
        ancount: be16(msg, 6)?,
        nscount: be16(msg, 8)?,
        arcount: be16(msg, 10)?,
    })
}

/// Decodes the name beginning at `start`, following compression pointers. Returns the name
/// (labels joined by `.`, the root as the empty string) and the offset in the record just
/// past the name — the byte after the first pointer taken, or after the terminating zero.
/// Returns `None` on a malformed name: a non-backward or capped pointer, a label running
/// past the message, a reserved length code, or a name over [`MAX_NAME`].
pub fn read_name(msg: &[u8], start: usize) -> Option<(String, usize)> {
    let mut name = String::new();
    let mut pos = start;
    let mut after: Option<usize> = None;
    let mut jumps = 0;
    loop {
        let len = *msg.get(pos)?;
        match len & 0xC0 {
            0x00 => {
                if len == 0 {
                    pos += 1;
                    break; // the root label ends the name
                }
                let l = usize::from(len);
                let label = msg.get(pos + 1..pos + 1 + l)?;
                if !name.is_empty() {
                    name.push('.');
                }
                name.push_str(core::str::from_utf8(label).ok()?);
                if name.len() > MAX_NAME {
                    return None;
                }
                pos += 1 + l;
            }
            0xC0 => {
                // A 14-bit pointer; the record cursor stops just after it.
                let target = ((usize::from(len & 0x3F)) << 8) | usize::from(*msg.get(pos + 1)?);
                after.get_or_insert(pos + 2);
                if target >= pos || jumps >= MAX_JUMPS {
                    return None; // must point strictly backwards; never loop
                }
                jumps += 1;
                pos = target;
            }
            _ => return None, // 0x40 and 0x80 are reserved
        }
    }
    Some((name, after.unwrap_or(pos)))
}

fn decode_rdata(msg: &[u8], rtype: u16, rdstart: usize, rdlen: usize) -> Option<RData> {
    let rdata = msg.get(rdstart..rdstart + rdlen)?;
    Some(match rtype {
        rtype::A if rdlen == 4 => RData::A(Ipv4([rdata[0], rdata[1], rdata[2], rdata[3]])),
        rtype::AAAA if rdlen == 16 => {
            let mut a = [0u8; 16];
            a.copy_from_slice(rdata);
            RData::Aaaa(a)
        }
        // The name may jump backwards, but its own bytes must lie inside the RDATA: a name
        // that overruns RDLENGTH reads the next record as if it were this one's.
        rtype::CNAME | rtype::NS | rtype::PTR => {
            let (name, end) = read_name(msg, rdstart)?;
            if end > rdstart + rdlen {
                return None;
            }
            RData::Name(name)
        }
        _ => RData::Other(rdlen),
    })
}

/// Parses a whole DNS message.
pub fn parse(msg: &[u8]) -> Option<Message> {
    let header = parse_header(msg)?;
    let mut pos = HEADER_LEN;

    let mut questions = Vec::new();
    for _ in 0..header.qdcount {
        let (name, next) = read_name(msg, pos)?;
        pos = next;
        let qtype = be16(msg, pos)?;
        let qclass = be16(msg, pos + 2)?;
        pos += 4;
        questions.push(Question {
            name,
            qtype,
            qclass,
        });
    }

    let total =
        usize::from(header.ancount) + usize::from(header.nscount) + usize::from(header.arcount);
    let mut records = Vec::new();
    for _ in 0..total {
        let (name, next) = read_name(msg, pos)?;
        pos = next;
        let rtype = be16(msg, pos)?;
        let rclass = be16(msg, pos + 2)?;
        let ttl = be32(msg, pos + 4)?;
        let rdlen = usize::from(be16(msg, pos + 8)?);
        let rdstart = pos + 10;
        let rdata = decode_rdata(msg, rtype, rdstart, rdlen)?;
        pos = rdstart + rdlen;
        records.push(Record {
            name,
            rtype,
            rclass,
            ttl,
            rdata,
        });
    }
    Some(Message {
        header,
        questions,
        records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A no-compression message builder for round-trip tests.
    struct Builder {
        buf: Vec<u8>,
    }
    impl Builder {
        fn new(id: u16, flags: u16, qd: u16, an: u16) -> Self {
            let mut buf = Vec::new();
            buf.extend_from_slice(&id.to_be_bytes());
            buf.extend_from_slice(&flags.to_be_bytes());
            buf.extend_from_slice(&qd.to_be_bytes());
            buf.extend_from_slice(&an.to_be_bytes());
            buf.extend_from_slice(&[0, 0, 0, 0]); // ns, ar
            Self { buf }
        }
        fn name(&mut self, name: &str) {
            if !name.is_empty() {
                for label in name.split('.') {
                    self.buf.push(label.len() as u8);
                    self.buf.extend_from_slice(label.as_bytes());
                }
            }
            self.buf.push(0);
        }
        fn question(&mut self, name: &str, qtype: u16, qclass: u16) {
            self.name(name);
            self.buf.extend_from_slice(&qtype.to_be_bytes());
            self.buf.extend_from_slice(&qclass.to_be_bytes());
        }
        fn a(&mut self, name: &str, ttl: u32, ip: Ipv4) {
            self.rr_head(name, rtype::A, 1, ttl, 4);
            self.buf.extend_from_slice(&ip.0);
        }
        fn cname(&mut self, name: &str, ttl: u32, target: &str) {
            // rdlength is the encoded target name's length.
            let mut tmp = Builder { buf: Vec::new() };
            tmp.name(target);
            self.rr_head(name, rtype::CNAME, 1, ttl, tmp.buf.len() as u16);
            self.buf.extend_from_slice(&tmp.buf);
        }
        fn rr_head(&mut self, name: &str, rtype: u16, rclass: u16, ttl: u32, rdlen: u16) {
            self.name(name);
            self.buf.extend_from_slice(&rtype.to_be_bytes());
            self.buf.extend_from_slice(&rclass.to_be_bytes());
            self.buf.extend_from_slice(&ttl.to_be_bytes());
            self.buf.extend_from_slice(&rdlen.to_be_bytes());
        }
    }

    #[test]
    fn round_trips_a_query_and_answers() {
        let mut b = Builder::new(0x1234, 0x8180, 1, 2); // response, RD+RA
        b.question("example.com", rtype::A, 1);
        b.a("example.com", 300, Ipv4([93, 184, 216, 34]));
        b.cname("www.example.com", 600, "example.com");
        let msg = parse(&b.buf).unwrap();

        assert_eq!(msg.header.id, 0x1234);
        assert!(msg.header.is_response() && msg.header.recursion_desired());
        assert_eq!(msg.header.rcode(), 0);
        assert_eq!(msg.questions.len(), 1);
        assert_eq!(msg.questions[0].name, "example.com");
        assert_eq!(msg.questions[0].qtype, rtype::A);
        assert_eq!(msg.records.len(), 2);
        assert_eq!(msg.records[0].rdata, RData::A(Ipv4([93, 184, 216, 34])));
        assert_eq!(msg.records[0].ttl, 300);
        assert_eq!(msg.records[1].name, "www.example.com");
        assert_eq!(msg.records[1].rdata, RData::Name("example.com".into()));
    }

    #[test]
    fn follows_a_backward_compression_pointer() {
        // Header: 1 question, 1 answer.
        let mut m = alloc::vec![0x00, 0x01, 0x81, 0x80, 0x00, 0x01, 0x00, 0x01, 0, 0, 0, 0];
        // Question "example.com" starts at offset 12.
        let q_off = m.len();
        assert_eq!(q_off, 12);
        for label in ["example", "com"] {
            m.push(label.len() as u8);
            m.extend_from_slice(label.as_bytes());
        }
        m.push(0);
        m.extend_from_slice(&rtype::A.to_be_bytes());
        m.extend_from_slice(&1u16.to_be_bytes());
        // Answer: name is a pointer back to offset 12, then A record.
        m.push(0xC0);
        m.push(12);
        m.extend_from_slice(&rtype::A.to_be_bytes());
        m.extend_from_slice(&1u16.to_be_bytes());
        m.extend_from_slice(&300u32.to_be_bytes());
        m.extend_from_slice(&4u16.to_be_bytes());
        m.extend_from_slice(&[1, 2, 3, 4]);

        let msg = parse(&m).unwrap();
        assert_eq!(msg.questions[0].name, "example.com");
        assert_eq!(
            msg.records[0].name, "example.com",
            "resolved via the pointer"
        );
        assert_eq!(msg.records[0].rdata, RData::A(Ipv4([1, 2, 3, 4])));
    }

    #[test]
    fn rejects_hostile_and_truncated_names() {
        // A pointer at offset 12 that points to itself: must be refused, not looped.
        let mut m = alloc::vec![0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0, 0, 0, 0];
        m.push(0xC0);
        m.push(12); // points to itself (not strictly backwards)
        m.extend_from_slice(&[0, 1, 0, 1]);
        assert_eq!(parse(&m), None, "a self-pointer is rejected");

        // A forward pointer (target after the pointer) is also refused.
        let name = read_name(&[0xC0, 0x05, 0, 0, 0, 0, 0], 0);
        assert_eq!(name, None);

        // A label that runs past the message end.
        assert_eq!(read_name(&[0x05, b'a', b'b'], 0), None);

        // A truncated header.
        assert_eq!(parse(&[0, 1, 2]), None);
    }

    #[test]
    fn a_long_chain_of_backward_pointers_terminates() {
        // Each pointer points to the previous one; strictly backwards, so it must resolve
        // (to the empty root name) without looping.
        let mut m = alloc::vec![0u8; 12];
        m[5] = 0; // qdcount 0
        // At offset 12: root label (0). Then pointers each to the one before.
        m.push(0); // offset 12: root
        let mut prev = 12u16;
        for _ in 0..50 {
            let here = m.len() as u16;
            m.push(0xC0);
            m.push(prev as u8);
            prev = here;
        }
        // Reading the last pointer walks back 50 hops to the root: an empty name.
        let (name, _) = read_name(&m, prev as usize).unwrap();
        assert_eq!(name, "");
    }

    /// A name that runs past its RDLENGTH must not be read out of the following record.
    #[test]
    fn a_name_may_not_overrun_its_rdata() {
        // A CNAME whose RDLENGTH (2) cuts its name short: the name's labels continue past it.
        let mut b = Builder::new(1, 0x8180, 0, 1);
        b.rr_head("a.example", rtype::CNAME, 1, 60, 2);
        b.name("b.example");
        assert_eq!(parse(&b.buf), None);
        // With the true length, the same record parses.
        let mut b = Builder::new(1, 0x8180, 0, 1);
        b.cname("a.example", 60, "b.example");
        assert!(parse(&b.buf).is_some());
    }

    /// What `parse` returns is consistent with the bytes: names within RFC 1035's bound, a
    /// count that matches the header, record data the right size (CLAUDE.md §72).
    #[test]
    fn mutated_messages_keep_their_invariants() {
        use crate::testing::{Rng, mutate};
        let mut b = Builder::new(0x1234, 0x8180, 1, 2);
        b.question("example.com", rtype::A, 1);
        b.a("example.com", 300, Ipv4([93, 184, 216, 34]));
        b.cname("www.example.com", 600, "example.com");
        let base = b.buf;
        let mut rng = Rng::new(0x444E_5332);
        let (mut parsed, mut names) = (0u32, 0u32);
        for case in 0..crate::testing::cases(20_000) {
            let mut data = base.clone();
            mutate(&mut data, &mut rng);
            if let Some(h) = parse_header(&data) {
                assert_eq!(h.rcode(), (h.flags & 0xF) as u8, "case {case}");
                let _ = (h.is_response(), h.recursion_desired());
            }
            if let Some(m) = parse(&data) {
                parsed += 1;
                assert_eq!(
                    m.questions.len(),
                    usize::from(m.header.qdcount),
                    "case {case}"
                );
                let total = usize::from(m.header.ancount)
                    + usize::from(m.header.nscount)
                    + usize::from(m.header.arcount);
                assert_eq!(m.records.len(), total, "case {case}");
                for q in &m.questions {
                    assert!(q.name.len() <= MAX_NAME, "case {case}");
                }
                for r in &m.records {
                    assert!(r.name.len() <= MAX_NAME, "case {case}");
                    match &r.rdata {
                        RData::Name(n) => assert!(n.len() <= MAX_NAME, "case {case}"),
                        RData::A(_) => assert_eq!(r.rtype, rtype::A, "case {case}"),
                        RData::Aaaa(_) => assert_eq!(r.rtype, rtype::AAAA, "case {case}"),
                        RData::Other(n) => assert!(*n <= data.len(), "case {case}"),
                    }
                }
            }
            for off in [0usize, 12, data.len() / 2, data.len().saturating_sub(1)] {
                if let Some((name, next)) = read_name(&data, off) {
                    names += 1;
                    assert!(name.len() <= MAX_NAME, "case {case}");
                    assert!(next <= data.len(), "case {case}");
                }
            }
        }
        assert!(parsed > 500 && names > 1000, "{parsed}/{names}");
    }

    #[test]
    fn mutations_never_panic() {
        // A valid response with a question and both an A and a CNAME, then thousands of
        // mutated copies through `parse` and `read_name`: nothing may panic, however the
        // bytes are corrupted, and compression can never loop (CLAUDE.md §72, §54).
        let mut b = Builder::new(0x1234, 0x8180, 1, 2);
        b.question("example.com", rtype::A, 1);
        b.a("example.com", 300, Ipv4([93, 184, 216, 34]));
        b.cname("www.example.com", 600, "example.com");
        let base = b.buf;

        let mut rng = crate::testing::Rng::new(0x5D_5D);
        for _ in 0..crate::testing::cases(20_000) {
            let mut data = base.clone();
            crate::testing::mutate(&mut data, &mut rng);
            let _ = parse(&data);
            for off in [0usize, 12, data.len() / 2, data.len().saturating_sub(1)] {
                let _ = read_name(&data, off);
            }
        }
    }
}
