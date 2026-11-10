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
    ];
    assert_eq!(server::CALLS.len(), pairs.len());
    for (ours, theirs) in pairs {
        assert_eq!(ours, theirs);
    }
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
