//! Exercises the generated channel client and server end to end over a loopback transport, and
//! pins the worked example of docs/spec/OIDL.md byte for byte. Compiled and run by
//! tests/generated.rs against the generated crate.

use oidl_generated::oidl_rt as rt;
use oidl_generated::origin_fixture_echo as e;
use oidl_generated::origin_fixture_echo::echo::client::EchoClient;
use oidl_generated::origin_fixture_echo::echo::server as s;
use oidl_generated::origin_kernel::Error;

const CHANNEL: u32 = 4;
const ALL_RIGHTS: u32 = 0xFFFF;

#[derive(Default)]
struct Service {
    calls: Vec<&'static str>,
    admin: bool,
    fail_put: bool,
    oversize: bool,
    noted: Vec<String>,
    received_extra: Option<Option<u32>>,
    data: Vec<u8>,
    borrowed_bits: Option<u16>,
}

impl s::EchoServer for Service {
    type Caller = ();

    fn holds(&self, _caller: &(), capability: e::Capability) -> bool {
        match capability {
            e::Capability::EchoAdmin => self.admin,
            _ => false,
        }
    }

    fn put(&mut self, _c: &(), r: s::PutRequest<'_>) -> Result<s::PutResponseArgs, s::PutError> {
        self.calls.push("put");
        assert!(r.flag);
        assert_eq!(r.count, 0x0102);
        assert_eq!(r.name, "hi");
        assert_eq!(r.h.value.to_abi(), 9);
        if self.fail_put {
            return Err(s::PutError::PeerClosed);
        }
        Ok(s::PutResponseArgs {})
    }

    fn fixed(&mut self, _c: &(), r: s::FixedRequest) -> Result<s::FixedResponseArgs, s::FixedError> {
        self.calls.push("fixed");
        assert!(r.b);
        assert_eq!(r.small, -3);
        assert_eq!(r.mid, 500);
        assert_eq!(r.big, -70_000);
        assert_eq!(r.wide, u64::MAX - 1);
        assert_eq!(r.c, e::Color::Green);
        assert_eq!(r.f, e::Flags::A.union(e::Flags::B));
        assert_eq!(r.p, e::Point { x: 7, y: 8 });
        assert_eq!(r.trio, [1, 2, 3]);
        assert_eq!(r.nz, 11);
        assert_eq!(r.nzf, e::Flags::A);
        Ok(s::FixedResponseArgs {
            c2: e::Color::Red,
            f2: e::Flags::B,
            sum: 40,
        })
    }

    fn vectors(
        &mut self,
        _c: &(),
        r: s::VectorsRequest<'_>,
    ) -> Result<s::VectorsResponseArgs<'_>, s::VectorsError> {
        self.calls.push("vectors");
        assert_eq!(r.points.count(), 2);
        assert_eq!(r.points.get(1), Some(e::Point { x: 2, y: 20 }));
        assert_eq!(r.data.as_bytes(), b"abcde");
        assert_eq!(r.name, "name");
        assert_eq!(r.opt.map(|h| h.value.to_abi()), Some(5));
        assert_eq!(r.handles.len(), 2);
        assert_eq!(r.handles[0].value.to_abi(), 6);
        self.received_extra = Some(r.extra);
        self.data = r.data.as_bytes().to_vec();
        if self.oversize {
            static OVER: [u8; 65] = [1; 65];
            return Ok(s::VectorsResponseArgs { total: 1, echoed: &OVER, more: None });
        }
        Ok(s::VectorsResponseArgs {
            total: 2,
            echoed: &self.data,
            more: Some([1, 2, 3, 4]),
        })
    }

    fn borrowed(
        &mut self,
        _c: &(),
        r: s::BorrowedRequest,
    ) -> Result<s::BorrowedResponseArgs<'_>, s::BorrowedError> {
        self.calls.push("borrowed");
        self.borrowed_bits = Some(r.m.bits());
        Ok(s::BorrowedResponseArgs { note: Some("late") })
    }

    fn tiles(&mut self, _c: &(), r: s::TilesRequest) -> Result<s::TilesResponseArgs, s::TilesError> {
        self.calls.push("tiles");
        assert_eq!(r.b.grid, [[1, 2], [3, 4], [5, 6]]);
        assert_eq!(r.b.tail, 0x0708);
        Ok(s::TilesResponseArgs {})
    }

    fn note(&mut self, _c: &(), r: s::NoteRequest<'_>) {
        self.calls.push("note");
        self.noted.push(r.text.to_string());
    }

    fn ping(&mut self, _c: &(), _r: s::PingRequest) -> Result<s::PingResponseArgs, s::PingError> {
        self.calls.push("ping");
        Ok(s::PingResponseArgs { value: 77 })
    }
}

struct Loopback {
    service: Service,
    connection: rt::Connection,
    discarded: Vec<u32>,
}

fn received(handles: &[u32]) -> Vec<rt::RecvHandle> {
    handles
        .iter()
        .map(|h| rt::RecvHandle {
            value: rt::Handle::from_abi(*h),
            object: CHANNEL,
            rights: ALL_RIGHTS,
        })
        .collect()
}

impl rt::Transport for Loopback {
    fn call(
        &mut self,
        buffer: &mut [u8],
        request: usize,
        handles: &[u32],
        out: &mut [rt::RecvHandle],
    ) -> Result<(usize, usize), i64> {
        let incoming = received(handles);
        let mut reply = vec![0u8; 65536];
        let mut reply_handles = vec![0u32; 64];
        match s::dispatch(
            &mut self.service,
            &(),
            &mut self.connection,
            &buffer[..request],
            &incoming,
            &mut reply,
            &mut reply_handles,
        ) {
            rt::Dispatched::Reply { bytes, handles, .. } => {
                buffer[..bytes].copy_from_slice(&reply[..bytes]);
                for (slot, h) in out.iter_mut().zip(received(&reply_handles[..handles])) {
                    *slot = h;
                }
                Ok((bytes, handles))
            }
            other => {
                let closed = matches!(other, rt::Dispatched::Close { .. });
                Err(i64::from(closed) * -999)
            }
        }
    }

    fn send(&mut self, buffer: &[u8], handles: &[u32]) -> Result<(), i64> {
        let incoming = received(handles);
        let mut reply = vec![0u8; 256];
        let mut reply_handles = vec![0u32; 8];
        match s::dispatch(
            &mut self.service,
            &(),
            &mut self.connection,
            buffer,
            &incoming,
            &mut reply,
            &mut reply_handles,
        ) {
            rt::Dispatched::NoReply { taken: true } => Ok(()),
            other => panic!("one-way message: {other:?}"),
        }
    }

    fn discard(&mut self, handles: &[rt::RecvHandle]) {
        self.discarded.extend(handles.iter().map(|h| h.value.to_abi()));
    }
}

fn loopback(admin: bool) -> Loopback {
    Loopback {
        service: Service { admin, ..Service::default() },
        connection: rt::Connection::new(),
        discarded: Vec::new(),
    }
}

fn scratch<'a>(bytes: &'a mut [u8], handles: &'a mut [u32], received: &'a mut [rt::RecvHandle]) -> rt::Scratch<'a> {
    rt::Scratch { bytes, handles, received }
}

fn nothing() -> rt::RecvHandle {
    rt::RecvHandle { value: rt::Handle::INVALID, object: 0, rights: 0 }
}

macro_rules! buffers {
    ($b:ident, $h:ident, $r:ident) => {
        let mut $b = vec![0u8; 65536];
        let mut $h = vec![0u32; 64];
        let mut $r = vec![nothing(); 64];
    };
}

fn known_answer() {
    let mut out = [0u8; 64];
    let mut handles = [0u32; 4];
    let mut sink = rt::HandleSink::new(&mut handles);
    let args = s::PutRequestArgs {
        flag: true,
        count: 0x0102,
        name: "hi",
        h: rt::Handle::from_abi(9),
    };
    let (head, tail) = out.split_at_mut(16);
    let size = args.encode(tail, &mut sink, 2).unwrap();
    let total = 16 + size;
    rt::write_header(head, rt::KIND_REQUEST, 5, 7, total).unwrap();
    assert_eq!(sink.used(), 1);
    assert_eq!(handles[0], 9, "the value goes out of band");
    // docs/spec/OIDL.md §7.9, "A worked example".
    let expected: &[u8] = &[
        0x28, 0, 0, 0, // size = 40
        1,    // wire
        1,    // kind = request
        0, 0, // reserved
        5, 0, 0, 0, // ordinal
        7, 0, 0, 0, // txid
        1,    // flag
        0,    // padding
        2, 1, // count = 0x0102
        2, 0, 0, 0, // name count
        0, 0, 0, 0, // slot: handle 0
        0, 0, 0, 0, // padding to 8
        b'h', b'i', 0, 0, 0, 0, 0, 0, // "hi", padded to 8
    ];
    assert_eq!(&out[..total], expected, "the worked example");
    let header = rt::read_header(&out[..total]).unwrap();
    assert_eq!((header.ordinal, header.txid, header.kind), (5, 7, rt::KIND_REQUEST));
    let recv = received(&handles[..1]);
    let mut list = rt::HandleList::new(&recv);
    let decoded = s::PutRequest::decode(&out[16..total], &mut list, 2).unwrap();
    assert_eq!(decoded.name, "hi");
    assert_eq!(decoded.count, 0x0102);
    assert!(decoded.flag);
    assert_eq!(decoded.h.value.to_abi(), 9);
}

fn round_trips() {
    buffers!(b, h, r);
    let mut s0 = scratch(&mut b, &mut h, &mut r);
    let mut client = EchoClient::connect(loopback(true), &mut s0).unwrap();
    assert_eq!(client.minor(), 2);

    client
        .put(&mut s0, s::PutRequestArgs { flag: true, count: 0x0102, name: "hi", h: rt::Handle::from_abi(9) })
        .unwrap();

    let reply = client
        .fixed(
            &mut s0,
            s::FixedRequestArgs {
                b: true,
                small: -3,
                mid: 500,
                big: -70_000,
                wide: u64::MAX - 1,
                c: e::Color::Green,
                f: e::Flags::A.union(e::Flags::B),
                p: e::Point { x: 7, y: 8 },
                trio: [1, 2, 3],
                s: e::Segment { a: e::Point { x: 1, y: 2 }, b: e::Point { x: 3, y: 4 }, width: 9 },
                nz: 11,
                nzf: e::Flags::A,
            },
        )
        .unwrap();
    assert_eq!(reply.c2, e::Color::Red);
    assert_eq!(reply.f2, e::Flags::B);
    assert_eq!(reply.sum, 40);

    let points = [e::Point { x: 1, y: 10 }, e::Point { x: 2, y: 20 }];
    let handles = [rt::Handle::from_abi(6), rt::Handle::from_abi(7)];
    let reply = client
        .vectors(
            &mut s0,
            s::VectorsRequestArgs {
                points: &points,
                data: b"abcde",
                name: "name",
                opt: Some(rt::Handle::from_abi(5)),
                handles: &handles,
                extra: Some(3),
                late_name: Some("late"),
            },
        )
        .unwrap();
    assert_eq!(reply.total, 2);
    assert_eq!(reply.echoed.as_bytes(), b"abcde");
    assert_eq!(reply.more, Some([1, 2, 3, 4]));

    client.note(&mut s0, s::NoteRequestArgs { text: "hello" }).unwrap();
    assert_eq!(client.ping(&mut s0, s::PingRequestArgs {}).unwrap().value, 77);

    // A type from another library crosses unchanged, and a later result arrives at minor 2.
    let later = oidl_generated::origin_fixture_calls::Mode::from_bits(0x0201).unwrap();
    let reply = client
        .borrowed(&mut s0, s::BorrowedRequestArgs { m: later })
        .unwrap();
    assert_eq!(reply.note, Some("late"));

    // Nested arrays round trip.
    client
        .tiles(
            &mut s0,
            s::TilesRequestArgs {
                b: e::Board { grid: [[1, 2], [3, 4], [5, 6]], tail: 0x0708 },
            },
        )
        .unwrap();
}

fn an_older_client_uses_the_older_layout() {
    buffers!(b, h, r);
    let mut s0 = scratch(&mut b, &mut h, &mut r);
    // Negotiate minor 0: send the hello by hand with minor 0.
    let mut hello = [0u8; 64];
    let total = rt::write_hello(
        &mut hello,
        rt::KIND_REQUEST,
        1,
        rt::Hello { interface_id: s::INTERFACE_ID, major: 1, minor: 0 },
    )
    .unwrap();
    let mut transport = loopback(true);
    let mut reply = [0u8; 64];
    let mut none = [0u32; 1];
    let disposition = s::dispatch(
        &mut transport.service,
        &(),
        &mut transport.connection,
        &hello[..total],
        &[],
        &mut reply,
        &mut none,
    );
    assert!(matches!(disposition, rt::Dispatched::Reply { taken: false, .. }));
    assert_eq!(transport.connection.negotiated(), Some(0));
    let header = rt::read_header(&reply[..32]).unwrap();
    assert_eq!(header.kind, rt::KIND_RESPONSE);
    assert_eq!(rt::read_hello_body(&reply[16..32]).unwrap().minor, 0, "min(0, 2)");

    // The layout at minor 0 has no extra field. Encode at minor 0.
    let points = [e::Point { x: 2, y: 20 }];
    let handles = [rt::Handle::from_abi(6), rt::Handle::from_abi(7)];
    let args = s::VectorsRequestArgs {
        points: &points,
        data: b"abcde",
        name: "name",
        opt: Some(rt::Handle::from_abi(5)),
        handles: &handles,
        extra: None,
        late_name: None,
    };
    let bytes = &mut s0.bytes;
    let (head, tail) = bytes.split_at_mut(16);
    let mut sink = rt::HandleSink::new(s0.handles);
    let size = args.encode(tail, &mut sink, 0).unwrap();
    let total = 16 + size;
    rt::write_header(head, rt::KIND_REQUEST, 7, 3, total).unwrap();
    let count = sink.used();
    assert_eq!(count, 3);
    let incoming = received(&s0.handles[..count]);
    let mut list = rt::HandleList::new(&incoming);
    let decoded = s::VectorsRequest::decode(&bytes[16..total], &mut list, 0).unwrap();
    assert_eq!(decoded.extra, None, "not in the layout at minor 0");
    assert_eq!(decoded.late_name, None);

    // The same bytes are the wrong size at minor 1.
    let mut list = rt::HandleList::new(&incoming);
    assert!(s::VectorsRequest::decode(&bytes[16..total], &mut list, 1).is_err());

    // A member added later is refused at an older minor.
    let mut body = [0u8; 8];
    body[0] = 3; // blue, since 1.2
    let mut list = rt::HandleList::new(&[]);
    assert!(s::FixedRequest::decode(&body, &mut list, 0).is_err());

    // Encoding a later field without a value fails rather than sending a wrong message.
    let mut out = [0u8; 256];
    let mut slots = [0u32; 8];
    let mut sink = rt::HandleSink::new(&mut slots);
    let bad = s::VectorsRequestArgs {
        points: &points,
        data: b"",
        name: "n",
        opt: None,
        handles: &[],
        extra: None,
        late_name: None,
    };
    assert_eq!(bad.encode(&mut out, &mut sink, 1), Err(rt::WireError::Missing));
}

fn errors_are_reported_separately() {
    buffers!(b, h, r);
    let mut s0 = scratch(&mut b, &mut h, &mut r);
    let mut transport = loopback(false);
    transport.service.fail_put = true;
    let mut client = EchoClient::connect(transport, &mut s0).unwrap();
    let put = s::PutRequestArgs { flag: true, count: 0x0102, name: "hi", h: rt::Handle::from_abi(9) };
    assert_eq!(
        client.put(&mut s0, put),
        Err(rt::CallError::Method(s::PutError::PeerClosed))
    );
    // The admin capability is missing: refused before the service runs.
    let points = [e::Point { x: 2, y: 20 }];
    let handles = [rt::Handle::from_abi(6)];
    let reply = client.vectors(
        &mut s0,
        s::VectorsRequestArgs {
            points: &points,
            data: b"",
            name: "n",
            opt: None,
            handles: &handles,
            extra: Some(1),
            late_name: Some("x"),
        },
    );
    assert_eq!(reply, Err(rt::CallError::Refused(Error::AccessDenied.raw())));
    // A string over its bound never leaves the client.
    let long = "x".repeat(33);
    assert_eq!(
        client.note(&mut s0, s::NoteRequestArgs { text: &long }),
        Err(rt::CallError::Protocol(rt::WireError::Count))
    );
    // A NUL never leaves the client either.
    assert_eq!(
        client.note(&mut s0, s::NoteRequestArgs { text: "a\0b" }),
        Err(rt::CallError::Protocol(rt::WireError::Nul))
    );
}

fn request(ordinal: u32, txid: u32, body: &[u8]) -> Vec<u8> {
    let total = 16 + body.len();
    let mut message = vec![0u8; total];
    rt::write_header(&mut message, rt::KIND_REQUEST, ordinal, txid, total).unwrap();
    message[16..].copy_from_slice(body);
    message
}

fn dispatch_once(
    transport: &mut Loopback,
    message: &[u8],
    handles: &[rt::RecvHandle],
    reply: &mut [u8],
) -> rt::Dispatched {
    let mut none = [0u32; 8];
    s::dispatch(
        &mut transport.service,
        &(),
        &mut transport.connection,
        message,
        handles,
        reply,
        &mut none,
    )
}

fn connected(admin: bool) -> Loopback {
    let mut transport = loopback(admin);
    transport.connection.set(2);
    transport
}

fn status_of(reply: &[u8]) -> i64 {
    rt::read_error_status(&reply[16..24]).unwrap()
}

fn the_server_refuses_before_any_service_runs() {
    let mut reply = vec![0u8; 256];
    // A body of the wrong size: refused with invalid_args, the service not run, the handles
    // left for the caller to close.
    let mut transport = connected(false);
    let handles = received(&[1]);
    let message = request(5, 3, &[0u8; 8]);
    let outcome = dispatch_once(&mut transport, &message, &handles, &mut reply);
    assert_eq!(outcome, rt::Dispatched::Reply { bytes: 24, handles: 0, taken: false });
    assert_eq!(status_of(&reply), Error::InvalidArgs.raw());
    assert!(transport.service.calls.is_empty());

    // An unknown ordinal: not_supported.
    let message = request(99, 3, &[]);
    let outcome = dispatch_once(&mut transport, &message, &handles, &mut reply);
    assert_eq!(outcome, rt::Dispatched::Reply { bytes: 24, handles: 0, taken: false });
    assert_eq!(status_of(&reply), Error::NotSupported.raw());

    // A two-way request with no transaction id, and a one-way with one: the header is broken.
    assert_eq!(
        dispatch_once(&mut transport, &request(9, 0, &[]), &[], &mut reply),
        rt::Dispatched::Close { taken: false }
    );
    assert_eq!(
        dispatch_once(&mut transport, &request(8, 5, &[0; 8]), &[], &mut reply),
        rt::Dispatched::Close { taken: false }
    );

    // A message that is not a request.
    let mut response = request(9, 3, &[]);
    response[5] = rt::KIND_RESPONSE;
    assert_eq!(
        dispatch_once(&mut transport, &response, &[], &mut reply),
        rt::Dispatched::Close { taken: false }
    );

    // Not a whole header, or a wrong size field: close, without a reply.
    assert_eq!(dispatch_once(&mut transport, &[0u8; 8], &[], &mut reply), rt::Dispatched::Close { taken: false });
    let mut wrong = request(9, 3, &[]);
    wrong[0] = 8;
    assert_eq!(dispatch_once(&mut transport, &wrong, &[], &mut reply), rt::Dispatched::Close { taken: false });
    assert!(transport.service.calls.is_empty());

    // A one-way message that fails validation is dropped, with no reply.
    let bad_note = request(8, 0, &[0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0]);
    assert_eq!(
        dispatch_once(&mut transport, &bad_note, &[], &mut reply),
        rt::Dispatched::NoReply { taken: false }
    );

    // An oversized reply from the service is the server's defect: the connection closes.
    let mut transport = connected(true);
    transport.service.oversize = true;
    let args = {
        let points = [e::Point { x: 1, y: 10 }, e::Point { x: 2, y: 20 }];
        let handles = [rt::Handle::from_abi(6), rt::Handle::from_abi(7)];
        let mut body = [0u8; 256];
        let mut slots = [0u32; 8];
        let mut sink = rt::HandleSink::new(&mut slots);
        let n = s::VectorsRequestArgs {
            points: &points,
            data: b"abcde",
            name: "name",
            opt: Some(rt::Handle::from_abi(5)),
            handles: &handles,
            extra: Some(1),
            late_name: Some("x"),
        }
        .encode(&mut body, &mut sink, 2)
        .unwrap();
        let used = sink.used();
        (body[..n].to_vec(), slots[..used].to_vec())
    };
    let message = request(7, 3, &args.0);
    let handles = received(&args.1);
    assert_eq!(
        dispatch_once(&mut transport, &message, &handles, &mut reply),
        rt::Dispatched::Close { taken: true },
        "the service ran: the received handles are its own now"
    );

    // The transaction-id rule comes before the version check (§7.5), so a method too new for
    // the negotiated minor with a bad transaction id closes the connection.
    let mut transport = connected(false);
    transport.connection = rt::Connection::new();
    transport.connection.set(0);
    assert_eq!(
        dispatch_once(&mut transport, &request(9, 0, &[]), &[], &mut reply),
        rt::Dispatched::Close { taken: false }
    );
    let outcome = dispatch_once(&mut transport, &request(9, 3, &[]), &[], &mut reply);
    assert_eq!(outcome, rt::Dispatched::Reply { bytes: 24, handles: 0, taken: false });
    assert_eq!(status_of(&reply), Error::NotSupported.raw());
}

fn a_later_first_parameter_is_absent_at_the_older_minor() {
    // `borrowed`'s only result arrived in minor 1: at minor 0 the body is empty.
    let body: [u8; 0] = [];
    let mut list = rt::HandleList::new(&[]);
    assert_eq!(
        s::BorrowedResponse::decode(&body, &mut list, 0).unwrap().note,
        None
    );
    assert_eq!(
        s::BorrowedResponse::decode(&[3, 0, 0, 0, 0, 0, 0, 0], &mut list, 0),
        Err(rt::WireError::Size),
        "the field does not exist at minor 0"
    );
    let at_one = [2, 0, 0, 0, 0, 0, 0, 0, b'h', b'i', 0, 0, 0, 0, 0, 0];
    let mut list = rt::HandleList::new(&[]);
    assert_eq!(
        s::BorrowedResponse::decode(&at_one, &mut list, 1).unwrap().note,
        Some("hi")
    );
    let mut list = rt::HandleList::new(&[]);
    assert!(s::BorrowedResponse::decode(&at_one, &mut list, 0).is_err());

    // A later member of another library's bits is never refused for this library's minor.
    let mut out = [0u8; 8];
    out[0..2].copy_from_slice(&0x0201u16.to_le_bytes());
    let mut list = rt::HandleList::new(&[]);
    assert_eq!(
        s::BorrowedRequest::decode(&out, &mut list, 0).unwrap().m.bits(),
        0x0201,
        "that library's minor is not this one"
    );
}

fn the_hello_is_checked() {
    let mut reply = vec![0u8; 256];
    let hello = |interface_id: u64, major: u16, minor: u16| {
        let mut message = vec![0u8; 32];
        let total = rt::write_hello(
            &mut message,
            rt::KIND_REQUEST,
            1,
            rt::Hello { interface_id, major, minor },
        )
        .unwrap();
        message.truncate(total);
        message
    };
    // Before the hello, nothing else is accepted.
    let mut transport = loopback(false);
    assert_eq!(
        dispatch_once(&mut transport, &request(9, 3, &[]), &[], &mut reply),
        rt::Dispatched::Close { taken: false }
    );
    // The wrong interface or major is refused and does not negotiate.
    let wrong_interface = hello(s::INTERFACE_ID ^ 1, 1, 0);
    assert!(matches!(
        dispatch_once(&mut transport, &wrong_interface, &[], &mut reply),
        rt::Dispatched::Reply { bytes: 24, .. }
    ));
    assert_eq!(status_of(&reply), Error::NotSupported.raw());
    assert_eq!(transport.connection.negotiated(), None);
    let wrong_major = hello(s::INTERFACE_ID, 2, 0);
    assert!(matches!(
        dispatch_once(&mut transport, &wrong_major, &[], &mut reply),
        rt::Dispatched::Reply { bytes: 24, .. }
    ));
    assert_eq!(status_of(&reply), Error::NotSupported.raw());
    // A good hello negotiates the smaller minor, and a second hello closes the connection.
    let good = hello(s::INTERFACE_ID, 1, 9);
    assert!(matches!(
        dispatch_once(&mut transport, &good, &[], &mut reply),
        rt::Dispatched::Reply { bytes: 32, .. }
    ));
    assert_eq!(transport.connection.negotiated(), Some(2), "min(9, 2)");
    assert_eq!(dispatch_once(&mut transport, &good, &[], &mut reply), rt::Dispatched::Close { taken: false });
    // A hello with a reserved field set, or handles attached, is malformed.
    let mut transport = loopback(false);
    let mut bad = hello(s::INTERFACE_ID, 1, 0);
    bad[28] = 1;
    assert_eq!(dispatch_once(&mut transport, &bad, &[], &mut reply), rt::Dispatched::Close { taken: false });
    let mut transport = loopback(false);
    let good = hello(s::INTERFACE_ID, 1, 0);
    assert_eq!(
        dispatch_once(&mut transport, &good, &received(&[1]), &mut reply),
        rt::Dispatched::Close { taken: false }
    );
}

fn a_client_validates_what_it_receives() {
    use std::cell::Cell;
    use std::rc::Rc;

    // A response with a wrong transaction id, or one it cannot decode, is a protocol error,
    // and whatever handles it brought are discarded.
    struct Fake {
        connected: bool,
        reply: Vec<u8>,
        reply_handles: Vec<u32>,
        discarded: Rc<Cell<usize>>,
        hello: Option<Vec<u8>>,
        hello_handles: usize,
    }
    impl rt::Transport for Fake {
        fn call(
            &mut self,
            buffer: &mut [u8],
            _request: usize,
            _handles: &[u32],
            out: &mut [rt::RecvHandle],
        ) -> Result<(usize, usize), i64> {
            if !self.connected {
                self.connected = true;
                if let Some(hello) = &self.hello {
                    buffer[..hello.len()].copy_from_slice(hello);
                    for (slot, h) in out.iter_mut().zip(0..self.hello_handles) {
                        *slot = received(&[100 + h as u32])[0];
                    }
                    return Ok((hello.len(), self.hello_handles));
                }
                let total = rt::write_hello(
                    buffer,
                    rt::KIND_RESPONSE,
                    1,
                    rt::Hello { interface_id: s::INTERFACE_ID, major: 1, minor: 2 },
                )
                .unwrap();
                return Ok((total, 0));
            }
            buffer[..self.reply.len()].copy_from_slice(&self.reply);
            for (slot, h) in out.iter_mut().zip(received(&self.reply_handles)) {
                *slot = h;
            }
            Ok((self.reply.len(), self.reply_handles.len()))
        }
        fn send(&mut self, _buffer: &[u8], _handles: &[u32]) -> Result<(), i64> {
            Ok(())
        }
        fn discard(&mut self, handles: &[rt::RecvHandle]) {
            self.discarded.set(self.discarded.get() + handles.len());
        }
    }
    fn response(ordinal: u32, txid: u32, kind: u8, body: &[u8]) -> Vec<u8> {
        let total = 16 + body.len();
        let mut message = vec![0u8; total];
        rt::write_header(&mut message, kind, ordinal, txid, total).unwrap();
        message[16..].copy_from_slice(body);
        message
    }
    let fake = |reply: Vec<u8>, reply_handles: Vec<u32>, discarded: &Rc<Cell<usize>>| Fake {
        connected: false,
        reply,
        reply_handles,
        discarded: discarded.clone(),
        hello: None,
        hello_handles: 0,
    };
    let ping_reply = |message: Vec<u8>| {
        buffers!(b, h, r);
        let mut s0 = scratch(&mut b, &mut h, &mut r);
        let count = Rc::new(Cell::new(0));
        let mut client = EchoClient::connect(fake(message, vec![], &count), &mut s0).unwrap();
        client.ping(&mut s0, s::PingRequestArgs {}).map(|r| r.value)
    };
    // Two-way ping is txid 2 (the hello was 1).
    assert_eq!(ping_reply(response(9, 2, rt::KIND_RESPONSE, &[5, 0, 0, 0, 0, 0, 0, 0])), Ok(5));
    assert_eq!(
        ping_reply(response(9, 3, rt::KIND_RESPONSE, &[5, 0, 0, 0, 0, 0, 0, 0])),
        Err(rt::CallError::Protocol(rt::WireError::Ordinal)),
        "wrong transaction id"
    );
    assert_eq!(
        ping_reply(response(8, 2, rt::KIND_RESPONSE, &[5, 0, 0, 0, 0, 0, 0, 0])),
        Err(rt::CallError::Protocol(rt::WireError::Ordinal)),
        "wrong ordinal"
    );
    assert_eq!(
        ping_reply(response(9, 2, rt::KIND_RESPONSE, &[5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])),
        Err(rt::CallError::Protocol(rt::WireError::Size)),
        "one word too long"
    );
    assert_eq!(
        ping_reply(response(9, 2, rt::KIND_ERROR, &(-999i64).to_le_bytes())),
        Err(rt::CallError::Undeclared(-999))
    );
    assert_eq!(
        ping_reply(response(9, 2, rt::KIND_ERROR, &Error::NotSupported.raw().to_le_bytes())),
        Err(rt::CallError::Refused(Error::NotSupported.raw()))
    );
    assert_eq!(
        ping_reply(response(9, 2, rt::KIND_ERROR, &3i64.to_le_bytes())),
        Err(rt::CallError::Protocol(rt::WireError::Enum)),
        "a status must be negative"
    );

    // A reply with handles it should not carry: refused, and the handles are discarded.
    let discards = |message: Vec<u8>, handles: Vec<u32>| {
        buffers!(b, h, r);
        let mut s0 = scratch(&mut b, &mut h, &mut r);
        let count = Rc::new(Cell::new(0));
        let mut client = EchoClient::connect(fake(message, handles, &count), &mut s0).unwrap();
        let result = client.ping(&mut s0, s::PingRequestArgs {}).map(|r| r.value);
        (result, count.get())
    };
    let (result, discarded) = discards(
        response(9, 2, rt::KIND_ERROR, &Error::NotSupported.raw().to_le_bytes()),
        vec![11, 12],
    );
    assert_eq!(result, Err(rt::CallError::Protocol(rt::WireError::HandleCount)));
    assert_eq!(discarded, 2, "an error reply may not carry handles; they are closed");
    let (result, discarded) =
        discards(response(9, 2, rt::KIND_RESPONSE, &[5, 0, 0, 0, 0, 0, 0, 0]), vec![11]);
    assert_eq!(result, Err(rt::CallError::Protocol(rt::WireError::HandleCount)));
    assert_eq!(discarded, 1, "a reply that fails validation: its handles are closed");
    let (result, discarded) =
        discards(response(9, 2, rt::KIND_RESPONSE, &[5, 0, 0, 0, 0, 0, 0, 0]), vec![]);
    assert_eq!(result, Ok(5));
    assert_eq!(discarded, 0);

    // The hello reply is checked like any other.
    fn connect_with(hello: Vec<u8>, handles: usize) -> (Result<u16, rt::CallError<core::convert::Infallible>>, usize) {
        let mut b = vec![0u8; 65536];
        let mut h = vec![0u32; 64];
        let mut r = vec![nothing(); 64];
        let mut s0 = scratch(&mut b, &mut h, &mut r);
        let count = Rc::new(Cell::new(0));
        let transport = Fake {
            connected: false,
            reply: Vec::new(),
            reply_handles: Vec::new(),
            discarded: count.clone(),
            hello: Some(hello),
            hello_handles: handles,
        };
        let result = EchoClient::connect(transport, &mut s0).map(|client| client.minor());
        (result, count.get())
    }
    let good = |kind: u8, minor: u16| {
        let mut message = vec![0u8; 32];
        let total = rt::write_hello(
            &mut message,
            kind,
            1,
            rt::Hello { interface_id: s::INTERFACE_ID, major: 1, minor },
        )
        .unwrap();
        message.truncate(total);
        message
    };
    assert_eq!(connect_with(good(rt::KIND_RESPONSE, 2), 0), (Ok(2), 0));
    assert_eq!(connect_with(good(rt::KIND_RESPONSE, 1), 0), (Ok(1), 0), "the server's minor is lower");
    assert_eq!(
        connect_with(good(rt::KIND_REQUEST, 2), 0),
        (Err(rt::CallError::Protocol(rt::WireError::Direction)), 0),
        "a request is not a reply"
    );
    assert_eq!(
        connect_with(good(rt::KIND_RESPONSE, 999), 0),
        (Err(rt::CallError::Protocol(rt::WireError::Version)), 0),
        "a minor the client never offered"
    );
    assert_eq!(
        connect_with(good(rt::KIND_RESPONSE, 2), 3),
        (Err(rt::CallError::Protocol(rt::WireError::HandleCount)), 3),
        "a hello carries no handles, and those it brought are closed"
    );
}

fn main() {
    known_answer();
    round_trips();
    an_older_client_uses_the_older_layout();
    errors_are_reported_separately();
    the_server_refuses_before_any_service_runs();
    a_later_first_parameter_is_absent_at_the_older_minor();
    the_hello_is_checked();
    a_client_validates_what_it_receives();
    println!("fixture api: all checks passed");
}
