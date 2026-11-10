//! The lookup through `origin.directory`'s generated client and stub, over a model of the
//! directory's handles: the host tests the ledger defines before the code
//! (`docs/research/2026-11-10-service-directory.md`), and threats D2, D3 and D6-D9 of
//! `docs/engineering/THREAT_MODEL_SYSCALLS.md`.

extern crate std;

use core::cell::Cell;
use std::vec::Vec;

use origin_directory::oidl_rt::{self as rt, CallError, Handle, RecvHandle};
use origin_directory::origin_directory::directory::client::DirectoryClient;
use origin_directory::origin_directory::directory::server::{
    ConnectError, ConnectRequestArgs, ConnectResponse, INTERFACE_ID, dispatch, ordinal,
};
use origin_directory::origin_kernel::{CHANNEL_RIGHTS, Error, ObjectType, Rights};

use super::*;

/// The interface the tests' provider serves, and another.
const SERVICE: u64 = 0x5E21_11CE_0000_0001;
const OTHER: u64 = 0x5E21_11CE_0000_0002;
/// The provider's channel, as the directory holds it.
const PROVIDER: u32 = 0x40;
/// A client's server ends, with a channel's rights (from `END` up); one without `transfer`; and
/// an event.
const END: u32 = 0x100;
const WEAK_END: u32 = 0x201;
const EVENT: u32 = 0x202;

/// What the directory did with a handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Did {
    Passed {
        provider: u32,
        interface_id: u64,
        major: u16,
        server: u32,
    },
    Closed(u32),
}

/// The directory's handles: every pass and close is recorded, and a provider refuses as told
/// (a test sets that while a client holds the directory).
#[derive(Debug, Default)]
struct Model {
    log: Vec<Did>,
    refusing: Cell<Option<(u32, Refused)>>,
}

impl Providers for Model {
    fn pass(
        &mut self,
        provider: Handle,
        interface_id: u64,
        major: u16,
        server: Handle,
    ) -> Result<(), Refused> {
        if let Some((refuser, why)) = self.refusing.get()
            && refuser == provider.to_abi()
        {
            return Err(why);
        }
        self.log.push(Did::Passed {
            provider: provider.to_abi(),
            interface_id,
            major,
            server: server.to_abi(),
        });
        Ok(())
    }

    fn close(&mut self, handle: Handle) {
        self.log.push(Did::Closed(handle.to_abi()));
    }
}

/// What the kernel reports for a handle the directory receives (ADR-0005 B3).
fn received(value: u32) -> RecvHandle {
    let channel = ObjectType::Channel.raw();
    let (object, rights) = match value {
        WEAK_END => (channel, CHANNEL_RIGHTS.bits() & !Rights::TRANSFER.bits()),
        EVENT => (ObjectType::Event.raw(), Rights::WAIT.bits()),
        _ => (channel, CHANNEL_RIGHTS.bits()),
    };
    RecvHandle {
        value: Handle::from_abi(value),
        object,
        rights,
    }
}

/// A client's transport: each message goes through the generated stub to the directory, with its
/// handles as the kernel would report them, and the stub's answer comes back.
#[derive(Debug)]
struct Loopback<'d> {
    directory: &'d mut Directory<Model, 2>,
    connection: rt::Connection,
    /// The handles of messages the stub refused, which its caller closes (OIDL §7.8).
    closed_by_stub: Vec<u32>,
}

impl rt::Transport for Loopback<'_> {
    fn call(
        &mut self,
        buffer: &mut [u8],
        request: usize,
        handles: &[u32],
        _received: &mut [RecvHandle],
    ) -> Result<(usize, usize), i64> {
        let message = buffer[..request].to_vec();
        let list: Vec<RecvHandle> = handles.iter().map(|&value| received(value)).collect();
        let mut reply = [0u8; 64];
        let mut reply_handles = [0u32; 4];
        let outcome = dispatch(
            self.directory,
            &Caller::default(),
            &mut self.connection,
            &message,
            &list,
            &mut reply,
            &mut reply_handles,
        );
        match outcome {
            rt::Dispatched::Reply {
                bytes,
                handles: count,
                taken,
            } => {
                if !taken {
                    self.closed_by_stub.extend_from_slice(handles);
                }
                buffer[..bytes].copy_from_slice(&reply[..bytes]);
                Ok((bytes, count))
            }
            rt::Dispatched::NoReply { taken } | rt::Dispatched::Close { taken } => {
                if !taken {
                    self.closed_by_stub.extend_from_slice(handles);
                }
                Err(Error::PeerClosed.raw())
            }
        }
    }

    fn send(&mut self, _buffer: &[u8], _handles: &[u32]) -> Result<(), i64> {
        Err(Error::NotSupported.raw())
    }

    fn discard(&mut self, _handles: &[RecvHandle]) {}
}

/// Buffers for one call.
struct Buffers {
    bytes: [u8; 64],
    handles: [u32; 4],
    received: [RecvHandle; 4],
}

impl Buffers {
    fn new() -> Self {
        Self {
            bytes: [0; 64],
            handles: [0; 4],
            received: [received(0); 4],
        }
    }

    fn scratch(&mut self) -> rt::Scratch<'_> {
        rt::Scratch {
            bytes: &mut self.bytes,
            handles: &mut self.handles,
            received: &mut self.received,
        }
    }
}

/// A directory holding `PROVIDER` for `SERVICE` at major 1.
fn directory() -> Directory<Model, 2> {
    let mut directory = Directory::new(Model::default());
    assert_eq!(
        directory.add(SERVICE, 1, Handle::from_abi(PROVIDER)),
        Ok(())
    );
    directory
}

/// A client of `directory`, past the hello.
fn client(directory: &mut Directory<Model, 2>) -> DirectoryClient<Loopback<'_>> {
    let transport = Loopback {
        directory,
        connection: rt::Connection::new(),
        closed_by_stub: Vec::new(),
    };
    let mut buffers = Buffers::new();
    let client = DirectoryClient::new(transport, &mut buffers.scratch()).expect("the hello");
    assert_eq!(client.minor(), 1, "origin.directory 0.1");
    client
}

fn connect(
    client: &mut DirectoryClient<Loopback<'_>>,
    interface_id: u64,
    major: u16,
    server: u32,
) -> Result<ConnectResponse, CallError<ConnectError>> {
    let mut buffers = Buffers::new();
    client.connect(
        &mut buffers.scratch(),
        ConnectRequestArgs {
            interface_id,
            major,
            server: Handle::from_abi(server),
        },
    )
}

#[test]
fn a_held_interface_and_major_gets_the_end_and_the_reply_is_ok() {
    let mut directory = directory();
    let mut client = client(&mut directory);
    assert_eq!(
        connect(&mut client, SERVICE, 1, END),
        Ok(ConnectResponse {})
    );
    assert!(client.transport().closed_by_stub.is_empty());
    drop(client);
    assert_eq!(
        directory.providers().log,
        [Did::Passed {
            provider: PROVIDER,
            interface_id: SERVICE,
            major: 1,
            server: END,
        }]
    );
}

#[test]
fn another_interface_or_major_is_not_found_and_the_end_is_closed() {
    let mut directory = directory();
    let mut client = client(&mut directory);
    let not_found = Err(CallError::Method(ConnectError::NotFound));
    assert_eq!(connect(&mut client, SERVICE, 2, END), not_found);
    assert_eq!(connect(&mut client, SERVICE, 0, END + 1), not_found);
    assert_eq!(connect(&mut client, OTHER, 1, END + 2), not_found);
    drop(client);
    assert_eq!(
        directory.providers().log,
        [Did::Closed(END), Did::Closed(END + 1), Did::Closed(END + 2)]
    );
    // C11's status, on the wire.
    assert_eq!(ConnectError::NotFound.error().raw(), -14);
    assert_eq!(
        directory.provider(SERVICE, 1),
        Some(Handle::from_abi(PROVIDER))
    );
}

#[test]
fn a_provider_that_cannot_take_the_end_now_is_limit_and_the_end_is_closed() {
    let mut directory = directory();
    let mut client = client(&mut directory);
    client
        .transport()
        .directory
        .providers
        .refusing
        .set(Some((PROVIDER, Refused::Full)));
    assert_eq!(
        connect(&mut client, SERVICE, 1, END),
        Err(CallError::Method(ConnectError::Limit))
    );
    drop(client);
    assert_eq!(directory.providers().log, [Did::Closed(END)]);
    // Still its provider: the next connect may find room.
    assert_eq!(
        directory.provider(SERVICE, 1),
        Some(Handle::from_abi(PROVIDER))
    );
}

#[test]
fn a_provider_that_is_gone_is_forgotten_and_its_channel_closed() {
    let mut directory = directory();
    let mut client = client(&mut directory);
    client
        .transport()
        .directory
        .providers
        .refusing
        .set(Some((PROVIDER, Refused::Gone)));
    let not_found = Err(CallError::Method(ConnectError::NotFound));
    assert_eq!(connect(&mut client, SERVICE, 1, END), not_found);
    // Forgotten: the next connect finds nothing and asks no provider.
    client.transport().directory.providers.refusing.set(None);
    assert_eq!(connect(&mut client, SERVICE, 1, END + 1), not_found);
    drop(client);
    assert_eq!(
        directory.providers().log,
        [
            Did::Closed(END),
            Did::Closed(PROVIDER),
            Did::Closed(END + 1)
        ]
    );
    assert!(directory.is_empty());
}

#[test]
fn an_end_that_is_not_a_channel_or_lacks_a_right_never_reaches_the_directory() {
    let mut directory = directory();
    let mut client = client(&mut directory);
    let refused = Err(CallError::Refused(Error::InvalidArgs.raw()));
    assert_eq!(connect(&mut client, SERVICE, 1, WEAK_END), refused);
    assert_eq!(connect(&mut client, SERVICE, 1, EVENT), refused);
    // The stub's caller closes them (OIDL §7.8); the directory saw nothing.
    assert_eq!(client.transport().closed_by_stub, [WEAK_END, EVENT]);
    drop(client);
    assert!(directory.providers().log.is_empty());
}

/// A request message on `connection`, made by hand: the client would not send these.
fn deliver(
    directory: &mut Directory<Model, 2>,
    connection: &mut rt::Connection,
    message: &[u8],
    handles: &[RecvHandle],
) -> (rt::Dispatched, Vec<u8>) {
    let mut reply = [0u8; 64];
    let mut reply_handles = [0u32; 4];
    let outcome = dispatch(
        directory,
        &Caller::default(),
        connection,
        message,
        handles,
        &mut reply,
        &mut reply_handles,
    );
    let bytes = match outcome {
        rt::Dispatched::Reply { bytes, .. } => reply[..bytes].to_vec(),
        _ => Vec::new(),
    };
    (outcome, bytes)
}

#[test]
fn a_hello_for_another_interface_or_major_is_not_supported_and_may_be_retried() {
    let mut directory = directory();
    let mut connection = rt::Connection::new();
    for (interface_id, major) in [(INTERFACE_ID ^ 1, 0), (INTERFACE_ID, 1)] {
        let mut hello = [0u8; 32];
        let size = rt::write_hello(
            &mut hello,
            rt::KIND_REQUEST,
            7,
            rt::Hello {
                interface_id,
                major,
                minor: 1,
            },
        )
        .expect("a hello");
        let (outcome, reply) = deliver(&mut directory, &mut connection, &hello[..size], &[]);
        assert!(
            matches!(outcome, rt::Dispatched::Reply { .. }),
            "{outcome:?}"
        );
        let header = rt::read_header(&reply).expect("a reply");
        assert_eq!(header.kind, rt::KIND_ERROR);
        assert_eq!(
            rt::read_error_status(&reply[rt::HEADER_SIZE..]),
            Ok(Error::NotSupported.raw())
        );
        assert_eq!(connection.negotiated(), None, "still to be negotiated");
    }
    // The connection is still open to the right hello.
    let mut hello = [0u8; 32];
    let size = rt::write_hello(
        &mut hello,
        rt::KIND_REQUEST,
        8,
        rt::Hello {
            interface_id: INTERFACE_ID,
            major: 0,
            minor: 1,
        },
    )
    .expect("a hello");
    let (outcome, _) = deliver(&mut directory, &mut connection, &hello[..size], &[]);
    assert!(
        matches!(outcome, rt::Dispatched::Reply { .. }),
        "{outcome:?}"
    );
    assert_eq!(connection.negotiated(), Some(1));
    assert!(directory.providers().log.is_empty());
}

#[test]
fn a_connect_before_the_hello_closes_the_connection_and_its_end_is_not_taken() {
    let mut directory = directory();
    let mut connection = rt::Connection::new();
    let mut message = [0u8; 32];
    let mut sent = [0u32; 1];
    let (head, tail) = message.split_at_mut(rt::HEADER_SIZE);
    let mut sink = rt::HandleSink::new(&mut sent);
    let size = ConnectRequestArgs {
        interface_id: SERVICE,
        major: 1,
        server: Handle::from_abi(END),
    }
    .encode(tail, &mut sink, 1)
    .expect("a connect");
    let total = rt::HEADER_SIZE + size;
    rt::write_header(head, rt::KIND_REQUEST, ordinal::CONNECT, 2, total).expect("a header");
    let (outcome, _) = deliver(
        &mut directory,
        &mut connection,
        &message[..total],
        &[received(END)],
    );
    assert_eq!(outcome, rt::Dispatched::Close { taken: false });
    assert!(directory.providers().log.is_empty());
}

#[test]
fn the_table_refuses_past_its_bound_a_second_provider_for_a_key_and_a_non_handle() {
    let mut directory: Directory<Model, 2> = Directory::new(Model::default());
    assert!(directory.is_empty());
    assert_eq!(directory.add(SERVICE, 1, Handle::from_abi(1)), Ok(()));
    assert_eq!(
        directory.add(SERVICE, 1, Handle::from_abi(2)),
        Err(AddError::Taken)
    );
    assert_eq!(
        directory.add(SERVICE, 2, Handle::INVALID),
        Err(AddError::Invalid)
    );
    // Another major is another key.
    assert_eq!(directory.add(SERVICE, 2, Handle::from_abi(3)), Ok(()));
    assert_eq!(
        directory.add(OTHER, 1, Handle::from_abi(4)),
        Err(AddError::Full)
    );
    assert_eq!(directory.len(), 2);
    assert_eq!(directory.provider(SERVICE, 1), Some(Handle::from_abi(1)));
    assert_eq!(directory.provider(SERVICE, 2), Some(Handle::from_abi(3)));
    assert_eq!(directory.provider(OTHER, 1), None);
}

#[test]
fn a_connect_never_changes_what_the_directory_holds() {
    let mut directory = directory();
    let mut client = client(&mut directory);
    for (k, (interface_id, major)) in [(SERVICE, 1), (OTHER, 1), (SERVICE, 1), (OTHER, 7)]
        .into_iter()
        .enumerate()
    {
        let _ = connect(&mut client, interface_id, major, END + k as u32);
    }
    drop(client);
    assert_eq!(directory.len(), 1);
    assert_eq!(
        directory.provider(SERVICE, 1),
        Some(Handle::from_abi(PROVIDER))
    );
    assert_eq!(directory.provider(OTHER, 1), None);
    // Every end it was given was passed on or closed, once.
    let ends: Vec<u32> = directory
        .providers()
        .log
        .iter()
        .map(|did| match *did {
            Did::Passed { server, .. } => server,
            Did::Closed(handle) => handle,
        })
        .collect();
    assert_eq!(ends, [END, END + 1, END + 2, END + 3]);
}
