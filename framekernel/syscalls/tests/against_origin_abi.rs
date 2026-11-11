//! The generated values against `origin-abi`'s (ADR-0011 N14).
//!
//! The kernel takes its call table and statuses from the definition. `origin-abi` keeps its public
//! items for what still uses them, the layers below the system-call boundary and the ring-3 proof
//! programs, so the two must stay equal. Moving the call numbers out of the stable crate waits for
//! step 4.

use std::cell::Cell;

use origin_syscalls::oidl_rt as rt;
use origin_syscalls::origin_kernel::kernel::server::{self, Calls, number};
use origin_syscalls::origin_kernel::types as generated;

#[test]
fn the_statuses_are_the_same() {
    assert_eq!(generated::OK, origin_abi::OK);
    assert_eq!(generated::Error::ALL.len(), origin_abi::Error::ALL.len());
    for (ours, theirs) in generated::Error::ALL.iter().zip(origin_abi::Error::ALL) {
        assert_eq!(ours.raw(), theirs.raw());
        assert_eq!(ours.name(), theirs.name());
    }
    for raw in -20..=5 {
        assert_eq!(
            generated::status_name(raw),
            origin_abi::status_name(raw),
            "status {raw}"
        );
    }
}

#[test]
fn the_rights_are_the_same() {
    use generated::Rights as R;
    use origin_abi::rights as abi;
    for (ours, theirs) in [
        (R::READ, abi::READ),
        (R::WRITE, abi::WRITE),
        (R::MAP, abi::MAP),
        (R::EXECUTE, abi::EXECUTE),
        (R::DUPLICATE, abi::DUPLICATE),
        (R::TRANSFER, abi::TRANSFER),
        (R::WAIT, abi::WAIT),
        (R::SIGNAL, abi::SIGNAL),
        (R::MANAGE, abi::MANAGE),
        (R::ALL, abi::ALL),
        (generated::CHANNEL_RIGHTS, abi::CHANNEL),
    ] {
        assert_eq!(ours.bits(), theirs);
    }
}

#[test]
fn the_signals_are_the_same() {
    use generated::Signals as S;
    use origin_abi::signals as abi;
    for (ours, theirs) in [
        (S::READABLE, abi::READABLE),
        (S::WRITABLE, abi::WRITABLE),
        (S::PEER_CLOSED, abi::PEER_CLOSED),
        (S::SIGNALED, abi::SIGNALED),
        (S::TERMINATED, abi::TERMINATED),
        (S::ALL, abi::ALL),
    ] {
        assert_eq!(ours.bits(), theirs);
    }
}

#[test]
fn the_object_types_are_the_same() {
    use generated::ObjectType as O;
    use origin_abi::ObjectType as Abi;
    let pairs = [
        (O::Process, Abi::Process),
        (O::Thread, Abi::Thread),
        (O::MemoryObject, Abi::MemoryObject),
        (O::Channel, Abi::Channel),
        (O::Event, Abi::Event),
        (O::Port, Abi::Port),
        (O::Timer, Abi::Timer),
    ];
    assert_eq!(O::ALL.len(), pairs.len());
    for (ours, theirs) in pairs {
        assert_eq!(ours.raw(), theirs as u32);
    }
}

#[test]
fn the_call_numbers_are_the_same() {
    use origin_abi::syscall as abi;
    let pairs = [
        (number::THREAD_EXIT, abi::THREAD_EXIT),
        (number::DEBUG_REPORT, abi::DEBUG_REPORT),
        (number::DEBUG_WRITE, abi::DEBUG_WRITE),
        (number::HANDLE_CLOSE, abi::HANDLE_CLOSE),
        (number::HANDLE_DUPLICATE, abi::HANDLE_DUPLICATE),
        (number::CHANNEL_CREATE, abi::CHANNEL_CREATE),
        (number::CHANNEL_WRITE, abi::CHANNEL_WRITE),
        (number::CHANNEL_READ, abi::CHANNEL_READ),
        (number::WAIT_ONE, abi::WAIT_ONE),
        (number::EVENT_CREATE, abi::EVENT_CREATE),
        (number::OBJECT_SIGNAL, abi::OBJECT_SIGNAL),
        (number::MEMORY_OBJECT_CREATE, abi::MEMORY_OBJECT_CREATE),
        (number::MEMORY_MAP, abi::MEMORY_MAP),
        (number::MEMORY_UNMAP, abi::MEMORY_UNMAP),
        (number::MEMORY_PROTECT, abi::MEMORY_PROTECT),
        (number::PORT_CREATE, abi::PORT_CREATE),
        (number::OBJECT_WAIT_ASYNC, abi::OBJECT_WAIT_ASYNC),
        (number::PORT_WAIT, abi::PORT_WAIT),
        (number::PORT_CANCEL, abi::PORT_CANCEL),
        (number::CLOCK_READ, abi::CLOCK_READ),
    ];
    assert_eq!(server::CALLS.len(), pairs.len());
    for (ours, theirs) in pairs {
        assert_eq!(ours, theirs);
    }
}

#[test]
fn the_start_record_is_the_same() {
    use origin_abi::start;
    use origin_abi::start::role;
    type Header = generated::StartHeader;
    type Entry = generated::StartEntry;
    // The hand-written codec writes the layout the definition states (ADR-0005 C4).
    assert_eq!(<Header as rt::Fixed>::SIZE, start::HEADER);
    assert_eq!(<Entry as rt::Fixed>::SIZE, start::ENTRY);
    for (ours, theirs) in [
        (Header::SIZE_AT, start::SIZE_AT),
        (Header::ABI_MAJOR_AT, start::ABI_MAJOR_AT),
        (Header::ABI_MINOR_AT, start::ABI_MINOR_AT),
        (Header::ABI_MIN_MINOR_AT, start::ABI_MIN_MINOR_AT),
        (Header::RESERVED_AT, start::RESERVED_AT),
        (Header::HANDLE_COUNT_AT, start::HANDLE_COUNT_AT),
        (Header::ARG_COUNT_AT, start::ARG_COUNT_AT),
        (Header::ARG_BYTES_AT, start::ARG_BYTES_AT),
        (Header::ENV_COUNT_AT, start::ENV_COUNT_AT),
        (Header::ENV_BYTES_AT, start::ENV_BYTES_AT),
        (Entry::HANDLE_AT, start::ENTRY_HANDLE_AT),
        (Entry::OBJECT_TYPE_AT, start::ENTRY_OBJECT_TYPE_AT),
        (Entry::RIGHTS_AT, start::ENTRY_RIGHTS_AT),
        (Entry::ROLE_AT, start::ENTRY_ROLE_AT),
    ] {
        assert_eq!(ours, theirs);
    }
    assert_eq!(generated::START_MAX_HANDLES as usize, start::MAX_HANDLES);
    assert_eq!(generated::START_MAX_BYTES as usize, start::MAX_SIZE);
    // The roles (C5), with 4 retired, and the rights of a process and a thread (C6).
    assert_eq!(generated::StartRole::Bootstrap.raw(), role::BOOTSTRAP);
    assert_eq!(generated::StartRole::SelfProcess.raw(), role::SELF_PROCESS);
    assert_eq!(generated::StartRole::SelfThread.raw(), role::SELF_THREAD);
    assert_eq!(generated::StartRole::Directory.raw(), role::DIRECTORY);
    assert_eq!(
        generated::StartRole::from_raw(4),
        None,
        "4 was SERVICE, and is retired"
    );
    assert_eq!(
        generated::PROCESS_RIGHTS.bits(),
        origin_abi::rights::PROCESS
    );
    assert_eq!(generated::THREAD_RIGHTS.bits(), origin_abi::rights::THREAD);
}

#[test]
fn the_limits_layouts_and_version_are_the_same() {
    use origin_abi::limits as abi;
    assert_eq!(
        generated::MAX_MESSAGE_BYTES as usize,
        abi::MAX_MESSAGE_BYTES
    );
    assert_eq!(
        generated::MAX_MESSAGE_HANDLES as usize,
        abi::MAX_MESSAGE_HANDLES
    );
    assert_eq!(generated::DEADLINE_INFINITE, abi::DEADLINE_INFINITE);
    assert_eq!(generated::ReadSizes::SIZE, origin_abi::read_sizes::SIZE);
    assert_eq!(
        generated::ReadSizes::SIZE_AT,
        origin_abi::read_sizes::SIZE_AT
    );
    assert_eq!(
        generated::ReadSizes::BYTES_AT,
        origin_abi::read_sizes::BYTES_AT
    );
    assert_eq!(
        generated::ReadSizes::HANDLES_AT,
        origin_abi::read_sizes::HANDLES_AT
    );
    assert_eq!(
        generated::ReadSizes::BASE_SIZE,
        origin_abi::read_sizes::BASE_SIZE
    );
    assert_eq!(
        generated::ReadSizes::SENDER_PROCESS_AT,
        origin_abi::read_sizes::SENDER_PROCESS_AT
    );
    assert_eq!(
        generated::ReadSizes::SENDER_PRINCIPAL_AT,
        origin_abi::read_sizes::SENDER_PRINCIPAL_AT
    );
    // The record `channel_read` writes for each handle (ADR-0005 B3).
    assert_eq!(
        <generated::ReceivedHandle as rt::Fixed>::SIZE,
        origin_abi::received_handle::SIZE
    );
    assert_eq!(
        generated::ReceivedHandle::HANDLE_AT,
        origin_abi::received_handle::HANDLE_AT
    );
    assert_eq!(
        generated::ReceivedHandle::OBJECT_TYPE_AT,
        origin_abi::received_handle::OBJECT_TYPE_AT
    );
    assert_eq!(
        generated::ReceivedHandle::RIGHTS_AT,
        origin_abi::received_handle::RIGHTS_AT
    );
    assert_eq!(generated::VERSION, origin_abi::ABI_VERSION);
    assert_eq!(rt::Handle::INVALID.to_abi(), origin_abi::INVALID_HANDLE);
    for raw in 0..=u8::MAX {
        assert_eq!(
            rt::decode_bool(raw),
            origin_abi::decode_bool(raw),
            "byte {raw:#x}"
        );
    }
}

/// Events and ports (ADR-0005 C9, C10, Amendment 7 F1 and F6): their rights and the packet.
#[test]
fn events_and_ports_are_the_same() {
    use origin_abi::{packet_kind, port_packet};
    assert_eq!(generated::EVENT_RIGHTS.bits(), origin_abi::rights::EVENT);
    assert_eq!(generated::PORT_RIGHTS.bits(), origin_abi::rights::PORT);
    assert_eq!(
        <generated::PortPacket as rt::Fixed>::SIZE,
        port_packet::SIZE
    );
    let fields = [
        (generated::PortPacket::KEY_AT, port_packet::KEY_AT),
        (generated::PortPacket::KIND_AT, port_packet::KIND_AT),
        (generated::PortPacket::TRIGGER_AT, port_packet::TRIGGER_AT),
        (generated::PortPacket::OBSERVED_AT, port_packet::OBSERVED_AT),
        (generated::PortPacket::RESERVED_AT, port_packet::RESERVED_AT),
        (generated::PortPacket::DATA_AT, port_packet::DATA_AT),
    ];
    for (ours, theirs) in fields {
        assert_eq!(ours, theirs);
    }
    assert_eq!(generated::PacketKind::Signal.raw(), packet_kind::SIGNAL);
    assert_eq!(generated::PacketKind::Canceled.raw(), packet_kind::CANCELED);
    assert_eq!(
        generated::PacketKind::from_raw(0),
        None,
        "0 is never a kind"
    );
}

/// Memory objects (ADR-0005 C6-C8): their rights, bits, page size and request layouts.
#[test]
fn memory_objects_are_the_same() {
    use origin_abi::{map_access, map_flags, map_request, object_request};
    assert_eq!(
        generated::MEMORY_OBJECT_RIGHTS.bits(),
        origin_abi::rights::MEMORY_OBJECT
    );
    assert_eq!(generated::PAGE_SIZE, origin_abi::PAGE_SIZE);
    assert_eq!(generated::MapAccess::READ.bits(), map_access::READ);
    assert_eq!(generated::MapAccess::WRITE.bits(), map_access::WRITE);
    assert_eq!(generated::MapAccess::EXECUTE.bits(), map_access::EXECUTE);
    assert_eq!(generated::MapAccess::ALL.bits(), map_access::ALL);
    assert_eq!(generated::MapFlags::FIXED.bits(), map_flags::FIXED);
    assert_eq!(generated::MapFlags::ALL.bits(), map_flags::ALL);
    assert_eq!(generated::ObjectRequest::SIZE, object_request::SIZE);
    assert_eq!(
        generated::ObjectRequest::LENGTH_AT,
        object_request::LENGTH_AT
    );
    assert_eq!(generated::MapRequest::SIZE, map_request::SIZE);
    assert_eq!(generated::MapRequest::ACCESS_AT, map_request::ACCESS_AT);
    assert_eq!(generated::MapRequest::FLAGS_AT, map_request::FLAGS_AT);
    assert_eq!(generated::MapRequest::OFFSET_AT, map_request::OFFSET_AT);
    assert_eq!(generated::MapRequest::LENGTH_AT, map_request::LENGTH_AT);
    assert_eq!(generated::MapRequest::ADDRESS_AT, map_request::ADDRESS_AT);
    // Only the sizes of versions the kernel knows are taken (OIDL §5.3).
    assert!(generated::MapRequest::accepts_param_size(map_request::SIZE));
    assert!(!generated::MapRequest::accepts_param_size(
        map_request::SIZE - 1
    ));
    assert!(!generated::MapRequest::accepts_param_size(
        map_request::SIZE + 8
    ));
}

/// An implementation of the interface that only records that it ran.
#[derive(Default)]
struct Recorder {
    ran: Cell<bool>,
}

impl Calls for Recorder {
    fn holds(&self, _: generated::Capability) -> bool {
        true
    }
    fn thread_exit(&self, _: u64) {
        self.ran.set(true);
    }
    fn debug_report(&self, _: u64) -> Result<(), server::DebugReportError> {
        self.ran.set(true);
        Ok(())
    }
    fn debug_write(&self, _: rt::UserSlice<u8>) -> Result<(), server::DebugWriteError> {
        self.ran.set(true);
        Ok(())
    }
    fn handle_close(&self, _: rt::HandleArg) -> Result<(), server::HandleCloseError> {
        self.ran.set(true);
        Ok(())
    }
    fn handle_duplicate(
        &self,
        _: rt::HandleArg,
        _: generated::Rights,
        _: rt::UserRef<rt::Handle>,
    ) -> Result<(), server::HandleDuplicateError> {
        self.ran.set(true);
        Ok(())
    }
    fn channel_create(
        &self,
        _: rt::UserRef<[rt::Handle; 2]>,
    ) -> Result<(), server::ChannelCreateError> {
        self.ran.set(true);
        Ok(())
    }
    fn channel_write(
        &self,
        _: rt::HandleArg,
        _: rt::UserSlice<u8>,
        _: rt::HandleSlice,
    ) -> Result<(), server::ChannelWriteError> {
        self.ran.set(true);
        Ok(())
    }
    fn channel_read(
        &self,
        _: rt::HandleArg,
        _: rt::UserBuffer<u8>,
        _: rt::UserBuffer<generated::ReceivedHandle>,
        _: rt::UserRef<generated::ReadSizes>,
    ) -> Result<(), server::ChannelReadError> {
        self.ran.set(true);
        Ok(())
    }
    fn wait_one(
        &self,
        _: rt::HandleArg,
        _: generated::Signals,
        _: u64,
        _: rt::UserRef<generated::Signals>,
    ) -> Result<(), server::WaitOneError> {
        self.ran.set(true);
        Ok(())
    }
    fn memory_object_create(
        &self,
        _: rt::UserRef<generated::ObjectRequest>,
        _: rt::UserRef<rt::Handle>,
    ) -> Result<(), server::MemoryObjectCreateError> {
        self.ran.set(true);
        Ok(())
    }
    fn memory_map(
        &self,
        _: rt::HandleArg,
        _: rt::HandleArg,
        _: rt::UserRef<generated::MapRequest>,
        _: rt::UserRef<u64>,
    ) -> Result<(), server::MemoryMapError> {
        self.ran.set(true);
        Ok(())
    }
    fn memory_unmap(
        &self,
        _: rt::HandleArg,
        _: u64,
        _: u64,
    ) -> Result<(), server::MemoryUnmapError> {
        self.ran.set(true);
        Ok(())
    }
    fn memory_protect(
        &self,
        _: rt::HandleArg,
        _: u64,
        _: u64,
        _: generated::MapAccess,
    ) -> Result<(), server::MemoryProtectError> {
        self.ran.set(true);
        Ok(())
    }
    fn event_create(&self, _: rt::UserRef<rt::Handle>) -> Result<(), server::EventCreateError> {
        self.ran.set(true);
        Ok(())
    }
    fn object_signal(
        &self,
        _: rt::HandleArg,
        _: generated::Signals,
        _: generated::Signals,
    ) -> Result<(), server::ObjectSignalError> {
        self.ran.set(true);
        Ok(())
    }
    fn port_create(&self, _: rt::UserRef<rt::Handle>) -> Result<(), server::PortCreateError> {
        self.ran.set(true);
        Ok(())
    }
    fn object_wait_async(
        &self,
        _: rt::HandleArg,
        _: rt::HandleArg,
        _: u64,
        _: generated::Signals,
    ) -> Result<(), server::ObjectWaitAsyncError> {
        self.ran.set(true);
        Ok(())
    }
    fn port_wait(
        &self,
        _: rt::HandleArg,
        _: u64,
        _: rt::UserRef<generated::PortPacket>,
    ) -> Result<(), server::PortWaitError> {
        self.ran.set(true);
        Ok(())
    }
    fn port_cancel(
        &self,
        _: rt::HandleArg,
        _: rt::HandleArg,
        _: u64,
    ) -> Result<(), server::PortCancelError> {
        self.ran.set(true);
        Ok(())
    }
    fn clock_read(&self, _: rt::UserRef<u64>) -> Result<(), server::ClockReadError> {
        self.ran.set(true);
        Ok(())
    }
}

/// `memory_protect`'s access is `@nonzero` and its bits are checked: zero, or a bit the
/// definition does not declare, is refused before the implementation runs.
#[test]
fn a_protect_with_no_access_or_an_undefined_bit_never_runs() {
    for access in [0, 1 << 3] {
        let recorder = Recorder::default();
        let outcome = server::dispatch(
            &recorder,
            number::MEMORY_PROTECT,
            [1, 0x1000, 0x1000, access, 0, 0],
        );
        assert_eq!(
            outcome,
            server::Outcome::Status(generated::Error::InvalidArgs.raw()),
            "{access:#x}"
        );
        assert!(
            !recorder.ran.get(),
            "the implementation ran for {access:#x}"
        );
    }
}

/// Item 5 of `docs/engineering/OIDL_KERNEL_COMPARISON.md`, adopted with the kernel's move to the
/// generated decoders: `rights` is decoded as the `rights` type, so an undefined bit is refused
/// before anything looks at the out pointer. The hand-written order answered `FAULT` to a call
/// with both faults.
#[test]
fn an_undefined_right_is_refused_before_the_out_pointer_is_looked_at() {
    let recorder = Recorder::default();
    let undefined_right = 1 << 30;
    let unmapped = 0x0000_2000_0000_0000;
    let outcome = server::dispatch(
        &recorder,
        number::HANDLE_DUPLICATE,
        [1, undefined_right, unmapped, 0, 0, 0],
    );
    assert_eq!(
        outcome,
        server::Outcome::Status(generated::Error::InvalidArgs.raw())
    );
    assert!(!recorder.ran.get(), "the implementation ran");
}
