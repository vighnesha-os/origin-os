//! Boot proof of the service directory (ADR-0005 C11, C12; ADR-0011 N9, S9; task S3-4): a
//! launcher starts a directory, a service and a client through the launch path, and every message
//! between them is one that `origin.directory`'s generated client or stub made.
//!
//! The launcher is kernel code acting as a process with no threads, as in the launch proof. The
//! client's start record names a DIRECTORY channel whose other end is the directory's. The
//! directory holds one provider: a channel to the service, for `origin.directory` major 0. That
//! is the only channel interface there is, so the service is itself a directory, as a launcher
//! might pass a child a narrower one. The client connects for that interface and major, and at
//! once, before any reply, writes its first request on its own end; then it connects for major 1,
//! which the directory does not hold.
//!
//! How a launcher fills a directory and how a provider receives a connection are not decided
//! (step 4, ABI decisions then; threat D11). Here, as an arrangement of this proof only: the
//! launcher writes the provider and the client's channel on the directory's BOOTSTRAP, and the
//! directory passes each connection on as the same `connect` call, on the provider's channel.
//!
//! The ring-3 programs are scripts that carry real messages; the lookup itself is proven on the
//! host (`services/directory`) until step 4 brings Rust to ring 3.

use alloc::vec::Vec;
use core::fmt::Write;

use origin_abi::{
    Error, OK, ObjectType, read_sizes, received_handle, rights, signals, syscall as sys,
};
use origin_directory::oidl_rt::{self as rt, CallError, Handle, RecvHandle};
use origin_directory::origin_directory::directory::client::DirectoryClient;
use origin_directory::origin_directory::directory::server::{
    self as stub, ConnectError, ConnectRequest, ConnectRequestArgs, ConnectResponse,
    ConnectResponseArgs, DirectoryServer, INTERFACE_ID,
};
use origin_frame::objref::ObjectRef;
use origin_frame::proof_programs::{self, DATA, STACK};

use super::Arg::{Handle as Hd, Imm};
use super::{
    Arg, CODE, Image, LIMITS, SIZES, Script, Step, Want, await_end, close, create, entry_handle, h,
    read, readable, report_identity, reported_u64, slot_addr, step, value_at, wait, write,
};
use crate::console::Console;
use crate::cpu::{self, CpuIndex};
use crate::handles::Need;
use crate::ipc::{self, Object};
use crate::launch::{self, Given};
use crate::memory;
use crate::paging::PAGE;
use crate::process::{Ending, Process};
use crate::sched;

/// The launcher's principal, both halves non-zero: each child acts for it (C1).
const PRINCIPAL_LAUNCHER: u64 = 0x1A0C_4E50_0000_0004;

// Where each message is kept in every child's scratch memory: 64 bytes each.
const HELLO_AT: u64 = DATA + 0x800;
const HELLO_REPLY_AT: u64 = DATA + 0x840;
const CONNECT_HELD_AT: u64 = DATA + 0x880;
const CONNECT_OK_AT: u64 = DATA + 0x8C0;
const CONNECT_MISSING_AT: u64 = DATA + 0x900;
const NOT_FOUND_AT: u64 = DATA + 0x940;
/// Room for one message read.
const ROOM: u64 = 64;

/// Where read `k` puts its message.
const fn inbox(k: u64) -> u64 {
    DATA + 0xA00 + ROOM * k
}

/// Where read `k` puts its handles' records: four at most.
const fn records(k: u64) -> u64 {
    DATA + 0xC00 + ROOM * k
}

/// A `read_sizes` with the writer's identity: its `size`, 32, is written before the child starts
/// (C2).
const SIZES_ID: u64 = DATA + 0xE00;

/// The proof's messages, each made by `origin.directory`'s generated client or stub.
struct Messages {
    /// A client's hello: `origin.directory` 0.1, transaction 1.
    hello: Vec<u8>,
    /// The stub's answer to it.
    hello_reply: Vec<u8>,
    /// `connect` for `origin.directory` major 0, transaction 2.
    connect_held: Vec<u8>,
    /// The stub's answer from a directory holding it: ok.
    connect_ok: Vec<u8>,
    /// `connect` for major 1, transaction 3.
    connect_missing: Vec<u8>,
    /// The stub's answer from a directory not holding it: `not_found`.
    not_found: Vec<u8>,
}

impl Messages {
    /// Each message and where it is kept.
    fn placed(&self) -> [(u64, &[u8]); 6] {
        [
            (HELLO_AT, &self.hello),
            (HELLO_REPLY_AT, &self.hello_reply),
            (CONNECT_HELD_AT, &self.connect_held),
            (CONNECT_OK_AT, &self.connect_ok),
            (CONNECT_MISSING_AT, &self.connect_missing),
            (NOT_FOUND_AT, &self.not_found),
        ]
    }
}

/// A directory holding one provider, of `origin.directory` major 0: what the generated stub turns
/// into the answers the ring-3 directory sends.
struct Holding;

impl DirectoryServer for Holding {
    type Caller = ();

    fn connect(
        &mut self,
        _caller: &(),
        request: ConnectRequest,
    ) -> Result<ConnectResponseArgs, ConnectError> {
        if (request.interface_id, request.major) == (INTERFACE_ID, 0) {
            Ok(ConnectResponseArgs {})
        } else {
            Err(ConnectError::NotFound)
        }
    }
}

/// A transport that keeps each request and answers it through the generated stub, as a
/// directory holding [`Holding`]'s provider would.
struct Recording {
    connection: rt::Connection,
    kept: Vec<(Vec<u8>, Vec<u8>)>,
}

impl rt::Transport for Recording {
    fn call(
        &mut self,
        buffer: &mut [u8],
        request: usize,
        handles: &[u64],
        _received: &mut [RecvHandle],
    ) -> Result<(usize, usize), i64> {
        let refused = Error::InvalidArgs.raw();
        let message = buffer.get(..request).ok_or(refused)?.to_vec();
        // Each end as the kernel reports one a directory receives (B3).
        let list: Vec<RecvHandle> = handles
            .iter()
            .map(|&value| RecvHandle {
                value: Handle::from_abi(value),
                object: ObjectType::Channel as u32,
                rights: rights::CHANNEL,
            })
            .collect();
        let mut reply = [0u8; ROOM as usize];
        let mut reply_handles = [0u64; 4];
        let rt::Dispatched::Reply {
            bytes, handles: 0, ..
        } = stub::dispatch(
            &mut Holding,
            &(),
            &mut self.connection,
            &message,
            &list,
            &mut reply,
            &mut reply_handles,
        )
        else {
            return Err(refused);
        };
        let answer = reply.get(..bytes).ok_or(refused)?;
        buffer
            .get_mut(..bytes)
            .ok_or(refused)?
            .copy_from_slice(answer);
        self.kept.push((message, answer.to_vec()));
        Ok((bytes, 0))
    }

    fn send(&mut self, _buffer: &[u8], _handles: &[u64]) -> Result<(), i64> {
        Err(Error::NotSupported.raw())
    }

    fn discard(&mut self, _handles: &[RecvHandle]) {}
}

/// A transport that answers each call with what a ring-3 client read, in turn.
struct Replay<'a> {
    answers: &'a [Vec<u8>],
    next: usize,
}

impl rt::Transport for Replay<'_> {
    fn call(
        &mut self,
        buffer: &mut [u8],
        _request: usize,
        _handles: &[u64],
        _received: &mut [RecvHandle],
    ) -> Result<(usize, usize), i64> {
        let answer = self.answers.get(self.next).ok_or(Error::PeerClosed.raw())?;
        self.next += 1;
        buffer
            .get_mut(..answer.len())
            .ok_or(Error::InvalidArgs.raw())?
            .copy_from_slice(answer);
        Ok((answer.len(), 0))
    }

    fn send(&mut self, _buffer: &[u8], _handles: &[u64]) -> Result<(), i64> {
        Err(Error::NotSupported.raw())
    }

    fn discard(&mut self, _handles: &[RecvHandle]) {}
}

/// What one connect asks for: `origin.directory`, at `major`.
const fn asking(major: u16) -> ConnectRequestArgs {
    ConnectRequestArgs {
        interface_id: INTERFACE_ID,
        major,
        server: Handle::from_abi(1),
    }
}

/// What a client sees of one connect.
type Answer = Result<ConnectResponse, CallError<ConnectError>>;

/// Runs a generated client over `transport`: its hello, then a connect for major 0 and one for
/// major 1. `None` if the hello fails.
fn run_client<T: rt::Transport>(transport: T) -> Option<(DirectoryClient<T>, Answer, Answer)> {
    let mut bytes = [0u8; ROOM as usize];
    let mut handles = [0u64; 4];
    let unused = RecvHandle {
        value: Handle::INVALID,
        object: 0,
        rights: 0,
    };
    let mut received = [unused; 4];
    let mut scratch = rt::Scratch {
        bytes: &mut bytes,
        handles: &mut handles,
        received: &mut received,
    };
    let mut client = DirectoryClient::new(transport, &mut scratch).ok()?;
    let held = client.connect(&mut scratch, asking(0));
    let missing = client.connect(&mut scratch, asking(1));
    Some((client, held, missing))
}

/// The proof's messages, from the generated client and stub. `None` if they did not make the
/// exchange the proof expects.
fn messages() -> Option<Messages> {
    let recording = Recording {
        connection: rt::Connection::new(),
        kept: Vec::new(),
    };
    let (client, held, missing) = run_client(recording)?;
    if held != Ok(ConnectResponse {}) || missing != Err(CallError::Method(ConnectError::NotFound)) {
        return None;
    }
    let [
        (hello, hello_reply),
        (connect_held, connect_ok),
        (connect_missing, not_found),
    ] = client.transport().kept.as_slice()
    else {
        return None;
    };
    Some(Messages {
        hello: hello.clone(),
        hello_reply: hello_reply.clone(),
        connect_held: connect_held.clone(),
        connect_ok: connect_ok.clone(),
        connect_missing: connect_missing.clone(),
        not_found: not_found.clone(),
    })
}

impl Script {
    /// Reports the writer the read into [`SIZES_ID`] saw; returns where it starts.
    fn report_writer(&mut self) -> usize {
        let start = self.reported();
        self.steps.extend(report_identity(SIZES_ID));
        start
    }
}

/// Reads the next message on `x` into `bytes`, its handles' records into `into`.
fn read_with_handles(x: Arg, bytes: u64, into: u64) -> Step {
    step(
        "",
        sys::CHANNEL_READ,
        &[x, Imm(bytes), Imm(ROOM), Imm(into), Imm(4), Imm(SIZES)],
        OK,
    )
}

/// Reads the next message on `x` into `bytes`, with its writer into [`SIZES_ID`].
fn read_with_writer(x: Arg, bytes: u64) -> Step {
    step(
        "",
        sys::CHANNEL_READ,
        &[x, Imm(bytes), Imm(ROOM), Imm(0), Imm(0), Imm(SIZES_ID)],
        OK,
    )
}

/// Writes `len` bytes at `bytes` on `x` with the one handle whose value is the `u32` at `handle`.
fn write_with(x: Arg, bytes: u64, len: usize, handle: u64) -> Step {
    step(
        "",
        sys::CHANNEL_WRITE,
        &[x, Imm(bytes), Imm(len as u64), Imm(handle), Imm(1)],
        OK,
    )
}

/// Where the client's reports are.
struct ClientMarks {
    /// The directory's three replies.
    replies: [usize; 3],
    /// The service's reply on the client's own end.
    service_reply: usize,
    /// Who wrote that reply.
    writer: usize,
    /// The status of its wait for the refused connection's end to close.
    closed: usize,
}

/// The client: its start record's DIRECTORY entry is the third (SELF_PROCESS, SELF_THREAD,
/// DIRECTORY). It writes everything first, then reads the answers. The connect the directory
/// holds is followed at once by a request on the client's own end. The connect it refuses comes
/// after the request on that connection's own end: the directory closes the end it is passed
/// without waiting, so a write after the connect races with that close.
fn client_script(record: u64, m: &Messages) -> (Script, ClientMarks) {
    let directory = Hd(entry_handle(record, 2));
    let (held, held_server, missing, missing_server) = (8, 9, 10, 11);
    let mut s = Script::default();
    s.call(write("", directory, HELLO_AT, m.hello.len() as u64, OK));
    s.call(create(held));
    s.call(write_with(
        directory,
        CONNECT_HELD_AT,
        m.connect_held.len(),
        slot_addr(held_server),
    ));
    s.call(write("", h(held), HELLO_AT, m.hello.len() as u64, OK));
    s.call(create(missing));
    // Before the other end goes, not after: the directory closes that end without waiting, and
    // once it has, a write on this one is `peer_closed`.
    s.call(write("", h(missing), HELLO_AT, m.hello.len() as u64, OK));
    s.call(write_with(
        directory,
        CONNECT_MISSING_AT,
        m.connect_missing.len(),
        slot_addr(missing_server),
    ));
    for k in 0..3 {
        s.call(readable(directory));
        s.call(read("", directory, inbox(k), ROOM, OK));
    }
    s.call(readable(h(held)));
    s.call(read_with_writer(h(held), inbox(3)));
    let closed = s.reported();
    // A poll: the directory closed this end before it wrote the `not_found` just read, and the
    // last close of an end raises its peer's PEER_CLOSED at once.
    s.call(wait("", h(missing), signals::PEER_CLOSED, 0, OK));
    let replies = [
        s.report_bytes(inbox(0), m.hello_reply.len()),
        s.report_bytes(inbox(1), m.connect_ok.len()),
        s.report_bytes(inbox(2), m.not_found.len()),
    ];
    let service_reply = s.report_bytes(inbox(3), m.hello_reply.len());
    let writer = s.report_writer();
    (
        s,
        ClientMarks {
            replies,
            service_reply,
            writer,
            closed,
        },
    )
}

/// Where the directory's reports are.
struct DirectoryMarks {
    /// What it read from the client: the hello and the two connects.
    from_client: [usize; 3],
    /// What it read from the provider: the answers to its hello and to the connect it passed on.
    from_provider: [usize; 2],
}

/// The directory: BOOTSTRAP is its third entry. It reads the launcher's arrangement, answers the
/// client's hello, says its own hello to the provider, passes the held connect's end on and
/// answers ok without waiting for the provider, closes the other connect's end and answers
/// `not_found`, then reads the provider's answers.
fn directory_script(record: u64, m: &Messages) -> (Script, DirectoryMarks) {
    let bootstrap = Hd(entry_handle(record, 2));
    let provider = Hd(records(0) + received_handle::HANDLE_AT as u64);
    let client = Hd(records(0) + (received_handle::SIZE + received_handle::HANDLE_AT) as u64);
    let mut s = Script::default();
    s.call(readable(bootstrap));
    s.call(read_with_handles(bootstrap, inbox(0), records(0)));
    s.call(readable(client));
    s.call(read("", client, inbox(1), ROOM, OK));
    s.call(write(
        "",
        client,
        HELLO_REPLY_AT,
        m.hello_reply.len() as u64,
        OK,
    ));
    s.call(write("", provider, HELLO_AT, m.hello.len() as u64, OK));
    s.call(readable(client));
    s.call(read_with_handles(client, inbox(2), records(1)));
    s.call(write_with(
        provider,
        CONNECT_HELD_AT,
        m.connect_held.len(),
        records(1) + received_handle::HANDLE_AT as u64,
    ));
    s.call(write(
        "",
        client,
        CONNECT_OK_AT,
        m.connect_ok.len() as u64,
        OK,
    ));
    s.call(readable(client));
    s.call(read_with_handles(client, inbox(3), records(2)));
    s.call(close(
        "",
        Hd(records(2) + received_handle::HANDLE_AT as u64),
        OK,
    ));
    s.call(write(
        "",
        client,
        NOT_FOUND_AT,
        m.not_found.len() as u64,
        OK,
    ));
    for k in 4..6 {
        s.call(readable(provider));
        s.call(read("", provider, inbox(k), ROOM, OK));
    }
    let from_client = [
        s.report_bytes(inbox(1), m.hello.len()),
        s.report_bytes(inbox(2), m.connect_held.len()),
        s.report_bytes(inbox(3), m.connect_missing.len()),
    ];
    let from_provider = [
        s.report_bytes(inbox(4), m.hello_reply.len()),
        s.report_bytes(inbox(5), m.connect_ok.len()),
    ];
    (
        s,
        DirectoryMarks {
            from_client,
            from_provider,
        },
    )
}

/// Where the service's reports are.
struct ServiceMarks {
    /// What it read from the directory: its hello and the connect it passed on.
    from_directory: [usize; 2],
    /// The passed end's object type and rights, as its record says.
    passed: usize,
    /// The request it read on the passed end.
    request: usize,
    /// Who wrote that request.
    writer: usize,
}

/// The service: BOOTSTRAP is its third entry, and the directory holds the other end. It answers
/// the directory's hello and connect, taking the end, then answers the request it finds on it.
fn service_script(record: u64, m: &Messages) -> (Script, ServiceMarks) {
    let bootstrap = Hd(entry_handle(record, 2));
    let passed = Hd(records(0) + received_handle::HANDLE_AT as u64);
    let mut s = Script::default();
    s.call(readable(bootstrap));
    s.call(read("", bootstrap, inbox(0), ROOM, OK));
    s.call(write(
        "",
        bootstrap,
        HELLO_REPLY_AT,
        m.hello_reply.len() as u64,
        OK,
    ));
    s.call(readable(bootstrap));
    s.call(read_with_handles(bootstrap, inbox(1), records(0)));
    s.call(write(
        "",
        bootstrap,
        CONNECT_OK_AT,
        m.connect_ok.len() as u64,
        OK,
    ));
    s.call(readable(passed));
    s.call(read_with_writer(passed, inbox(2)));
    s.call(write(
        "",
        passed,
        HELLO_REPLY_AT,
        m.hello_reply.len() as u64,
        OK,
    ));
    let from_directory = [
        s.report_bytes(inbox(0), m.hello.len()),
        s.report_bytes(inbox(1), m.connect_held.len()),
    ];
    let passed_at = s.reported();
    s.call(value_at(
        "",
        records(0) + received_handle::OBJECT_TYPE_AT as u64,
        0,
    ));
    s.call(value_at(
        "",
        records(0) + received_handle::RIGHTS_AT as u64,
        0,
    ));
    let request = s.report_bytes(inbox(2), m.hello.len());
    let writer = s.report_writer();
    (
        s,
        ServiceMarks {
            from_directory,
            passed: passed_at,
            request,
            writer,
        },
    )
}

/// A launched child, its script and where its reports are.
struct Child<T> {
    process: ObjectRef<Process>,
    steps: Vec<Step>,
    marks: T,
}

impl<T> Child<T> {
    /// It exited, and every call returned what its script says.
    fn ran_right(&self) -> bool {
        let reports = self.process.reports();
        let wanted: Vec<&Step> = self
            .steps
            .iter()
            .filter(|s| !matches!(s.want, Want::Nothing))
            .collect();
        self.process.ending() == Some(Ending::Exited(0))
            && reports.len() == wanted.len()
            && wanted.iter().zip(&reports).all(|(s, &got)| match s.want {
                Want::Status(status) => got == status as u64,
                _ => true,
            })
    }

    /// The `len` bytes it reported from `at`.
    fn bytes(&self, at: usize, len: usize) -> Option<Vec<u8>> {
        let reports = self.process.reports();
        let words = reports.get(at..at + len.div_ceil(4))?;
        let mut bytes: Vec<u8> = words
            .iter()
            .flat_map(|&word| (word as u32).to_le_bytes())
            .collect();
        bytes.truncate(len);
        Some(bytes)
    }

    /// The writer it reported from `at`: process, then principal.
    fn writer(&self, at: usize) -> Option<(u64, u64)> {
        let reports = self.process.reports();
        reported_u64(&reports, at).zip(reported_u64(&reports, at + 2))
    }

    /// The value it reported at `at`.
    fn value(&self, at: usize) -> Option<u64> {
        self.process.reports().get(at).copied()
    }
}

/// Launches a child of `launcher` through the launch path with `given`, on `cpu`: its scratch
/// memory holds `messages`, and it runs the script `build` writes for its start record's
/// address. `None` if it could not be launched; what `given` held is closed then.
fn launch_child<T>(
    launcher: &Process,
    given: Given<'_>,
    cpu: CpuIndex,
    messages: &Messages,
    build: impl FnOnce(u64, &Messages) -> (Script, T),
) -> Option<Child<T>> {
    let code = proof_programs::launched();
    let code_pages = code.len().div_ceil(PAGE as usize).max(1) as u64;
    let child = Process::proof_child_of(launcher, LIMITS, (CODE, CODE + code_pages * PAGE))?;
    let mut image = Image::from_process(child, code)?;
    let placed = launch::place(&mut image.process, &given).ok()?;
    let (script, marks) = build(placed.record, messages);
    image.scratch()?;
    image.write(SIZES_ID, &read_sizes::SIZE.to_le_bytes())?;
    for (at, bytes) in messages.placed() {
        image.write(at, bytes)?;
    }
    image.script(&script.steps)?;
    let launched = launch::start(image.process, placed, given, cpu, CODE, STACK + PAGE).ok()?;
    Some(Child {
        process: launched.process,
        steps: script.steps,
        marks,
    })
}

/// The proof's lines, each `: directory failed` if it is not all true.
fn verdict(ok: bool) -> &'static str {
    if ok { "" } else { ": directory failed" }
}

/// The launcher's channels and the three children, run to their end, then checked.
fn exchange(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let Some(messages) = messages() else {
        let _ = writeln!(
            com1,
            "origin-kernel: directory: the generated client and stub did not make the proof's messages{}",
            verdict(false)
        );
        return false;
    };
    let Some(launcher) = Process::proof(LIMITS, (CODE, CODE + PAGE), PRINCIPAL_LAUNCHER) else {
        return false;
    };
    // The service's BOOTSTRAP, whose other end the directory gets as the provider's channel; the
    // directory's BOOTSTRAP, whose other end the launcher keeps; and the client's DIRECTORY,
    // whose other end the directory gets as its first client.
    let (
        Ok((service_end, provider_end)),
        Ok((directory_end, launcher_end)),
        Ok((client_end, served_end)),
    ) = (
        ipc::create_channel(&launcher.account),
        ipc::create_channel(&launcher.account),
        ipc::create_channel(&launcher.account),
    )
    else {
        return false;
    };
    let (Ok(to_directory), Ok(provider), Ok(served)) = (
        launcher.grant(Object::Channel(launcher_end), rights::CHANNEL),
        launcher.grant(Object::Channel(provider_end), rights::CHANNEL),
        launcher.grant(Object::Channel(served_end), rights::CHANNEL),
    ) else {
        return false;
    };
    // The arrangement (D11): the provider of origin.directory major 0, and the first client.
    let mut table = Vec::new();
    table.extend_from_slice(&INTERFACE_ID.to_le_bytes());
    table.extend_from_slice(&0u64.to_le_bytes());
    let arranged = ipc::channel_write(
        &launcher.handles,
        &launcher.account,
        launcher.identity(),
        to_directory,
        Need::NOTHING,
        table,
        &[provider, served],
        Need::NOTHING,
    );
    if arranged.is_err() {
        return false;
    }
    let on = |k: usize| cpus.get(k % cpus.len().max(1)).copied().unwrap_or(bsp);
    let given = |bootstrap, directory| Given {
        bootstrap,
        directory,
        args: &[],
        env: &[],
    };
    let first = cpu::rdtsc();
    let service = launch_child(
        &launcher,
        given(Some(service_end), None),
        on(1),
        &messages,
        service_script,
    );
    let directory = launch_child(
        &launcher,
        given(Some(directory_end), None),
        on(2),
        &messages,
        directory_script,
    );
    let client = launch_child(
        &launcher,
        given(None, Some(client_end)),
        bsp,
        &messages,
        client_script,
    );
    let (Some(service), Some(directory), Some(client)) = (service, directory, client) else {
        let _ = writeln!(
            com1,
            "origin-kernel: directory: the launcher could not start all three{}",
            verdict(false)
        );
        return false;
    };
    let ended = await_end(&client.process, tsc_hz, 10)
        && await_end(&directory.process, tsc_hz, 10)
        && await_end(&service.process, tsc_hz, 10);
    let micros = cpu::rdtsc().wrapping_sub(first) / (tsc_hz / 1_000_000).max(1);
    let ran = ended && client.ran_right() && directory.ran_right() && service.ran_right();
    let m = &messages;
    // Each check reads only the reports it is about, so that a fault fails the line that names
    // it; whether every script ran as written is checked, and said, after them.

    // The client's messages reached the directory, and the end it passed reached the service as
    // a channel with a channel's rights, with the request the client wrote before any reply.
    let (d, s, c) = (&directory.marks, &service.marks, &client.marks);
    let directory_read = directory.bytes(d.from_client[0], m.hello.len()) == Some(m.hello.clone())
        && directory.bytes(d.from_client[1], m.connect_held.len()) == Some(m.connect_held.clone())
        && directory.bytes(d.from_client[2], m.connect_missing.len())
            == Some(m.connect_missing.clone());
    let service_read = service.bytes(s.from_directory[0], m.hello.len()) == Some(m.hello.clone())
        && service.bytes(s.from_directory[1], m.connect_held.len()) == Some(m.connect_held.clone())
        && service.value(s.passed) == Some(ObjectType::Channel as u64)
        && service.value(s.passed + 1) == Some(u64::from(rights::CHANNEL))
        && service.bytes(s.request, m.hello.len()) == Some(m.hello.clone());
    let through = directory_read && service_read;
    // Who the service saw, and who the client saw (B1, S9).
    let client_is = (client.process.id(), PRINCIPAL_LAUNCHER);
    let service_is = (service.process.id(), PRINCIPAL_LAUNCHER);
    let client_seen = service.writer(s.writer) == Some(client_is)
        && client.process.id() != directory.process.id();
    let service_seen = client.writer(c.writer) == Some(service_is)
        && client.bytes(c.service_reply, m.hello_reply.len()) == Some(m.hello_reply.clone());
    let _ = writeln!(
        com1,
        "origin-kernel: directory: a launcher started a directory, a service and a client; the client's connect, and the request it wrote on its own end before any reply, reached the service through the directory: {through}; the service read that request stamped with the client's process and principal, not the directory's: {client_seen}; the client read the service's reply stamped with the service's process: {service_seen}{}",
        verdict(through && client_seen && service_seen)
    );

    // What the client read from the directory, decoded by the generated client.
    let read: Option<Vec<Vec<u8>>> = [
        (c.replies[0], m.hello_reply.len()),
        (c.replies[1], m.connect_ok.len()),
        (c.replies[2], m.not_found.len()),
    ]
    .iter()
    .map(|&(at, len)| client.bytes(at, len))
    .collect();
    let decoded = read.as_deref().and_then(|answers| {
        run_client(Replay { answers, next: 0 })
            .map(|(replayed, held, missing)| (replayed.minor(), held, missing))
    });
    let ok = matches!(&decoded, Some((1, Ok(ConnectResponse {}), _)));
    let not_found = matches!(
        &decoded,
        Some((_, _, Err(CallError::Method(ConnectError::NotFound))))
    ) && ConnectError::NotFound.error().raw() == Error::NotFound.raw();
    let closed = client.value(c.closed) == Some(OK as u64);
    let taken = directory.bytes(d.from_provider[0], m.hello_reply.len())
        == Some(m.hello_reply.clone())
        && directory.bytes(d.from_provider[1], m.connect_ok.len()) == Some(m.connect_ok.clone());
    let _ = writeln!(
        com1,
        "origin-kernel: directory: the client's replies, read in ring 3 and decoded by the generated client: ok for the interface and major the directory holds: {ok}; not_found ({}) for a major it does not hold: {not_found}; that connection's end reports peer_closed: {closed}; the service answered ok for the end passed to it: {taken}{}",
        Error::NotFound.raw(),
        verdict(ok && not_found && closed && taken)
    );
    if !ran {
        let _ = writeln!(
            com1,
            "origin-kernel: directory: the scripts ran as written: client {:?} {}, directory {:?} {}, service {:?} {}{}",
            client.process.ending(),
            client.ran_right(),
            directory.process.ending(),
            directory.ran_right(),
            service.process.ending(),
            service.ran_right(),
            verdict(false)
        );
    }
    let _ = writeln!(
        com1,
        "origin-kernel: directory: the exchange took {micros} us, from the first launch until all three had ended (QEMU; the three waits for their ends included)"
    );
    ran && through && client_seen && service_seen && ok && not_found && closed && taken
}

/// The service directory's proof (ADR-0005 C11, C12; ADR-0011 N9), with a leak check of its own.
/// Called from the boot flow with every scheduler running, after [`super::prove_launch`].
pub(crate) fn prove_directory(
    com1: &mut Console,
    cpus: &[CpuIndex],
    bsp: CpuIndex,
    tsc_hz: u64,
) -> bool {
    let before = memory::stats();
    let ok = exchange(com1, cpus, bsp, tsc_hz);
    let same = || {
        memory::stats().map(|(h, f)| (h.bytes_in_use, f))
            == before.map(|(h, f)| (h.bytes_in_use, f))
    };
    ipc::reap_closed();
    sched::yield_until(same, tsc_hz, 2);
    let back = same();
    let (heap_now, frames_now) = memory::stats().map_or((0, 0), |(h, f)| (h.bytes_in_use, f));
    let (heap_before, frames_before) = before.map_or((0, 0), |(h, f)| (h.bytes_in_use, f));
    let _ = writeln!(
        com1,
        "origin-kernel: directory: the directory, the service, the client and all they held are gone: heap {heap_now} bytes (was {heap_before}), {frames_now} free frames (was {frames_before}){}",
        verdict(back)
    );
    ok && back
}
