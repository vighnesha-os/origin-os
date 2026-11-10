//! The system-call layer (ADR-0005 P4, P5; CLAUDE.md §29, §42): what a `syscall` from ring 3
//! does once `cpu::dispatch` hands it over.
//!
//! - RAX is the call number and RDI, RSI, RDX, R10, R8, R9 the arguments. RAX returns `OK` or
//!   a negative status; everything else goes through out pointers.
//! - The call table, the decoding of the arguments and the statuses each call may return come
//!   from `sdk/interfaces/origin.kernel.oidl`, through the generated crate `origin-syscalls`
//!   (ADR-0011 N14). This module implements its `Calls` for [`Process`] and converts at that
//!   boundary: the layers below keep `origin-abi`'s types.
//! - What a call asks of a handle, its rights and object type, is declared in the definition
//!   and enforced by the handle table (`docs/spec/OIDL.md` §6): the decoder carries it here,
//!   and [`need`] hands it to the lookup, so there is no second list of rights.
//! - A call runs with interrupts on, on its thread's kernel stack, so a long copy or close
//!   stays preemptible and shootdowns are served; every kernel lock it takes disables them
//!   for the moment it is held.
//! - User memory is checked against the **calling process's own tables** and copied with
//!   fault recovery (P4). Kernel buffers are copied out of or into once, and nothing is
//!   read from user memory twice, so a second thread cannot change a value between a
//!   check and its use.
//! - A call that would fail leaves the process and its objects as they were. Pointers are
//!   checked before anything is consumed, so a bad out pointer does not lose a message.
//! - Only a call from the process's system-call code range is accepted (P4, ONBF §6.1,
//!   "syscall-origin pinning"). Until `origin.core` exists the creator names that range.
#![forbid(unsafe_code)]

use alloc::vec::Vec;
use core::fmt::Write as _;

use crate::handles::Need;
use crate::usercopy::UserAccess;
use core::sync::atomic::{AtomicU64, Ordering};
use origin_abi::{Error, Handle};
use origin_frame::once::SetOnce;
use origin_frame::trap::SyscallOutcome;
use origin_syscalls::oidl_rt as rt;
use origin_syscalls::origin_kernel::kernel::server;
use origin_syscalls::origin_kernel::types as oidl;

use crate::channel::Identity;
use crate::console::Console;
use crate::mapping::Request;
use crate::memory_object::{self, MemoryObject};
use crate::process::{Ending, UserContext};
use crate::{cpu, ipc, sched};
use origin_frame::objref::ObjectRef;

/// The most handles in one message, as a length.
const MAX_HANDLES: usize = oidl::MAX_MESSAGE_HANDLES as usize;
/// Bytes of one handle in user memory.
const HANDLE_BYTES: usize = <rt::Handle as rt::Fixed>::SIZE;
/// Bytes of one received handle's record in user memory (ADR-0005 B3).
const RECORD_BYTES: usize = <oidl::ReceivedHandle as rt::Fixed>::SIZE;

/// Runs one system call from ring 3: the number, its six arguments, and the address after
/// the `syscall` instruction. Interrupts are off on entry and on return.
pub(crate) fn run(call: u64, args: [u64; 6], rip: u64) -> SyscallOutcome {
    let Some(caller) = sched::current_user() else {
        // A thread in user mode always has a process. If that ever failed, ending the
        // thread is the only safe thing: it can do nothing without one.
        return SyscallOutcome::ThreadExit;
    };
    if !caller.process.may_call_from(rip) {
        return SyscallOutcome::Status(oidl::Error::AccessDenied.raw());
    }
    cpu::enable_interrupts();
    let outcome = match server::dispatch(&*caller, call, args) {
        server::Outcome::Status(status) => SyscallOutcome::Status(status),
        server::Outcome::ThreadExit => SyscallOutcome::ThreadExit,
    };
    // Anything a close left behind is dropped here, with no lock held, and anything an exit
    // on this processor left behind is freed here too: otherwise a processor kept busy by
    // user threads would never get to its idle loop to do it.
    ipc::reap_closed();
    sched::reap();
    cpu::disable_interrupts();
    outcome
}

/// A user fault ended the calling thread. Called with interrupts off, on a stack the fault
/// has left to this thread alone; the exception stacks (`stack_fixed` is true) take no
/// further interrupts.
pub(crate) fn user_fault(vector: u64, error: u64, address: u64, stack_fixed: bool) {
    let Some(caller) = sched::current_user() else {
        return;
    };
    if !stack_fixed {
        cpu::enable_interrupts();
    }
    caller.process.thread_ended(
        &caller.thread,
        Ending::Faulted {
            vector,
            error,
            address,
        },
    );
    cpu::disable_interrupts();
}

/// How often a call's implementation produced a status its definition does not declare.
static UNDECLARED: AtomicU64 = AtomicU64::new(0);
/// The first such status, and which call's error type it did not fit.
static FIRST_UNDECLARED: SetOnce<(&'static str, Error)> = SetOnce::new();

/// A status from the layers below, as one of the errors the definition declares for this call.
/// Every status they can return for it should be declared, so one that does not fit is a kernel
/// bug. The caller still gets a failure its call can report, `otherwise`, and that is all it
/// sees; the bug is counted, and a boot in which any happened fails its IPC proof.
fn declared<E: rt::MethodError>(otherwise: E) -> impl FnOnce(Error) -> E {
    move |e| {
        E::from_status(e.raw()).unwrap_or_else(|| {
            UNDECLARED.fetch_add(1, Ordering::Relaxed);
            let _ = FIRST_UNDECLARED.set((core::any::type_name::<E>(), e));
            otherwise
        })
    }
}

/// How often a system call's implementation has produced a status its definition does not
/// declare, and the first it produced: the error type it did not fit, and the status.
pub(crate) fn undeclared_statuses() -> (u64, Option<(&'static str, Error)>) {
    (
        UNDECLARED.load(Ordering::Relaxed),
        FIRST_UNDECLARED.get().copied(),
    )
}

/// What the definition asks of this handle.
const fn need(handle: rt::HandleArg) -> Need {
    Need {
        rights: handle.need_rights(),
        object: handle.need_object(),
    }
}

/// The kernel's side of the generated interface: each call on behalf of this thread.
impl server::Calls for UserContext {
    fn holds(&self, capability: oidl::Capability) -> bool {
        match capability {
            oidl::Capability::KernelDebug => self.process.debug_calls(),
            // A permission this kernel does not know is not held.
            _ => false,
        }
    }

    fn thread_exit(&self, code: u64) {
        self.process
            .thread_ended(&self.thread, Ending::Exited(code));
    }

    fn debug_report(&self, value: u64) -> Result<(), server::DebugReportError> {
        self.process.report(value);
        Ok(())
    }

    fn debug_write(&self, text: rt::UserSlice<u8>) -> Result<(), server::DebugWriteError> {
        debug_write(self, text).map_err(declared(server::DebugWriteError::Fault))
    }

    fn handle_close(&self, handle: rt::HandleArg) -> Result<(), server::HandleCloseError> {
        ipc::handle_close(&self.process.handles, handle.value(), need(handle))
            .map_err(declared(server::HandleCloseError::BadHandle))
    }

    fn handle_duplicate(
        &self,
        handle: rt::HandleArg,
        rights: oidl::Rights,
        new_handle: rt::UserRef<rt::Handle>,
    ) -> Result<(), server::HandleDuplicateError> {
        handle_duplicate(self, handle, rights, new_handle)
            .map_err(declared(server::HandleDuplicateError::InvalidArgs))
    }

    fn channel_create(
        &self,
        ends: rt::UserRef<[rt::Handle; 2]>,
    ) -> Result<(), server::ChannelCreateError> {
        channel_create(self, ends).map_err(declared(server::ChannelCreateError::NoMemory))
    }

    fn channel_write(
        &self,
        end: rt::HandleArg,
        bytes: rt::UserSlice<u8>,
        handles: rt::HandleSlice,
    ) -> Result<(), server::ChannelWriteError> {
        channel_write(self, end, bytes, handles)
            .map_err(declared(server::ChannelWriteError::InvalidArgs))
    }

    fn channel_read(
        &self,
        end: rt::HandleArg,
        bytes: rt::UserBuffer<u8>,
        handles: rt::UserBuffer<oidl::ReceivedHandle>,
        sizes: rt::UserRef<oidl::ReadSizes>,
    ) -> Result<(), server::ChannelReadError> {
        channel_read(self, end, bytes, handles, sizes)
            .map_err(declared(server::ChannelReadError::InvalidArgs))
    }

    fn wait_one(
        &self,
        handle: rt::HandleArg,
        signals: oidl::Signals,
        deadline: u64,
        observed: rt::UserRef<oidl::Signals>,
    ) -> Result<(), server::WaitOneError> {
        wait_one(self, handle, signals, deadline, observed)
            .map_err(declared(server::WaitOneError::InvalidArgs))
    }

    fn memory_object_create(
        &self,
        request: rt::UserRef<oidl::ObjectRequest>,
        object: rt::UserRef<rt::Handle>,
    ) -> Result<(), server::MemoryObjectCreateError> {
        memory_object_create(self, request, object)
            .map_err(declared(server::MemoryObjectCreateError::InvalidArgs))
    }

    fn memory_map(
        &self,
        process: rt::HandleArg,
        object: rt::HandleArg,
        request: rt::UserRef<oidl::MapRequest>,
        address: rt::UserRef<u64>,
    ) -> Result<(), server::MemoryMapError> {
        memory_map(self, process, object, request, address)
            .map_err(declared(server::MemoryMapError::InvalidArgs))
    }

    fn memory_unmap(
        &self,
        process: rt::HandleArg,
        address: u64,
        length: u64,
    ) -> Result<(), server::MemoryUnmapError> {
        memory_object::process_of(&self.process, process.value(), need(process))
            .and_then(|target| memory_object::unmap(&self.process, &target, address, length))
            .map_err(declared(server::MemoryUnmapError::InvalidArgs))
    }

    fn memory_protect(
        &self,
        process: rt::HandleArg,
        address: u64,
        length: u64,
        access: oidl::MapAccess,
    ) -> Result<(), server::MemoryProtectError> {
        memory_object::process_of(&self.process, process.value(), need(process))
            .and_then(|target| {
                memory_object::protect(&self.process, &target, address, length, access.bits())
            })
            .map_err(declared(server::MemoryProtectError::InvalidArgs))
    }
}

/// A pointer and length, checked against the caller's tables.
fn checked(p: &UserContext, addr: u64, len: u64, access: UserAccess) -> Result<(), Error> {
    p.space.check_user(addr, len, access)
}

/// Fills `dst` from the caller's memory at `src`.
fn copy_in(p: &UserContext, dst: &mut [u8], src: u64) -> Result<(), Error> {
    p.space.copy_in(dst, src)
}

/// Copies `src` into the caller's memory at `dst`.
fn copy_out(p: &UserContext, dst: u64, src: &[u8]) -> Result<(), Error> {
    p.space.copy_out(dst, src)
}

/// The little-endian `u32` at `addr` in the caller's memory, read once.
fn read_u32(p: &UserContext, addr: u64) -> Result<u32, Error> {
    let mut bytes = [0; 4];
    copy_in(p, &mut bytes, addr)?;
    Ok(u32::from_le_bytes(bytes))
}

/// Writes the handles in `values` to the caller's memory at `addr`, as little-endian `u32`s.
fn copy_out_handles(p: &UserContext, addr: u64, values: &[Handle]) -> Result<(), Error> {
    let mut bytes = [0; HANDLE_BYTES * MAX_HANDLES];
    let dst = bytes
        .get_mut(..HANDLE_BYTES * values.len())
        .ok_or(Error::InvalidArgs)?;
    let (chunks, _) = dst.as_chunks_mut::<HANDLE_BYTES>();
    for (chunk, value) in chunks.iter_mut().zip(values) {
        *chunk = value.to_le_bytes();
    }
    copy_out(p, addr, dst)
}

/// Writes a `received_handle` record for each of `received` to the caller's memory at `addr`
/// (ADR-0005 B3): each field little-endian at its offset in the definition. The table holds
/// only object types and rights the definition declares (`origin-abi`'s values are the
/// definition's, which `origin-syscalls` tests), so the reader accepts every record.
fn copy_out_records(p: &UserContext, addr: u64, received: &[ipc::Received]) -> Result<(), Error> {
    let mut bytes = [0; RECORD_BYTES * MAX_HANDLES];
    let dst = bytes
        .get_mut(..RECORD_BYTES * received.len())
        .ok_or(Error::InvalidArgs)?;
    let (records, _) = dst.as_chunks_mut::<RECORD_BYTES>();
    for (record, r) in records.iter_mut().zip(received) {
        let fields = [
            (oidl::ReceivedHandle::HANDLE_AT, r.handle),
            (oidl::ReceivedHandle::OBJECT_TYPE_AT, r.object_type),
            (oidl::ReceivedHandle::RIGHTS_AT, r.rights),
        ];
        for (at, value) in fields {
            rt::put_field(record, at, value).map_err(|_| Error::InvalidArgs)?;
        }
    }
    copy_out(p, addr, dst)
}

/// A kernel buffer of `len` zero bytes, or [`Error::NoMemory`].
fn zeroed(len: usize) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(len).map_err(|_| Error::NoMemory)?;
    bytes.resize(len, 0);
    Ok(bytes)
}

/// `debug_write`: one console line, developer builds only.
fn debug_write(p: &UserContext, text: rt::UserSlice<u8>) -> Result<(), Error> {
    let mut bytes = [0u8; oidl::DEBUG_WRITE_MAX as usize];
    let dst = bytes.get_mut(..text.count()).ok_or(Error::InvalidArgs)?;
    copy_in(p, dst, text.addr())?;
    let mut line = Console::new();
    let _ = line.write_str("origin-kernel: user mode: write: ");
    for &b in dst.iter() {
        let c = if (0x20..0x7F).contains(&b) {
            b as char
        } else {
            '?'
        };
        let _ = line.write_char(c);
    }
    let _ = line.write_str("\n");
    Ok(())
}

/// `handle_duplicate`.
fn handle_duplicate(
    p: &UserContext,
    handle: rt::HandleArg,
    rights: oidl::Rights,
    new_handle: rt::UserRef<rt::Handle>,
) -> Result<(), Error> {
    checked(p, new_handle.addr(), HANDLE_BYTES as u64, UserAccess::Write)?;
    let table = &p.process.handles;
    let new = ipc::handle_duplicate(table, handle.value(), need(handle), rights.bits())?;
    copy_out(p, new_handle.addr(), &new.to_le_bytes()).inspect_err(|_| {
        let _ = ipc::handle_close(table, new, Need::NOTHING);
    })
}

/// `channel_create`: the two handles, one after the other.
fn channel_create(p: &UserContext, ends: rt::UserRef<[rt::Handle; 2]>) -> Result<(), Error> {
    checked(p, ends.addr(), 2 * HANDLE_BYTES as u64, UserAccess::Write)?;
    let process = &p.process;
    let (first, second) = ipc::create_channel(&process.account)?;
    let rights = oidl::CHANNEL_RIGHTS.bits();
    let a = process.grant(ipc::Object::Channel(first), rights)?;
    let b = match process.grant(ipc::Object::Channel(second), rights) {
        Ok(b) => b,
        Err(e) => {
            let _ = ipc::handle_close(&process.handles, a, Need::NOTHING);
            return Err(e);
        }
    };
    copy_out_handles(p, ends.addr(), &[a, b]).inspect_err(|_| {
        let _ = ipc::handle_close(&process.handles, a, Need::NOTHING);
        let _ = ipc::handle_close(&process.handles, b, Need::NOTHING);
    })
}

/// `channel_write`. The decoder has checked both counts against their limits.
fn channel_write(
    p: &UserContext,
    end: rt::HandleArg,
    bytes: rt::UserSlice<u8>,
    handles: rt::HandleSlice,
) -> Result<(), Error> {
    // Copied once, into kernel buffers, before anything else: a second thread of the caller
    // cannot change either list after the checks below.
    let mut payload = zeroed(bytes.count())?;
    copy_in(p, &mut payload, bytes.addr())?;
    let count = handles.slice.count();
    let mut raw = [0; HANDLE_BYTES * MAX_HANDLES];
    let raw_used = raw
        .get_mut(..HANDLE_BYTES * count)
        .ok_or(Error::InvalidArgs)?;
    copy_in(p, raw_used, handles.slice.addr())?;
    let mut values = [0; MAX_HANDLES];
    let (chunks, _) = raw_used.as_chunks::<HANDLE_BYTES>();
    for (value, chunk) in values.iter_mut().zip(chunks) {
        *value = u32::from_le_bytes(*chunk);
    }
    let transfer = values.get(..count).ok_or(Error::InvalidArgs)?;
    // The writer's identity comes from its process, never from an argument (B1).
    ipc::channel_write(
        &p.process.handles,
        &p.process.account,
        p.process.identity(),
        end.value(),
        need(end),
        payload,
        transfer,
        Need {
            rights: handles.need_rights,
            object: handles.need_object,
        },
    )
}

/// `channel_read`. The decoder has clamped both capacities to what a message can hold.
fn channel_read(
    p: &UserContext,
    end: rt::HandleArg,
    bytes: rt::UserBuffer<u8>,
    handles: rt::UserBuffer<oidl::ReceivedHandle>,
    out_sizes: rt::UserRef<oidl::ReadSizes>,
) -> Result<(), Error> {
    // The out pointers are checked before a message is taken: a bad one leaves it queued. The
    // handles' room is whole records (B3).
    checked(p, bytes.addr(), bytes.byte_len(), UserAccess::Write)?;
    checked(p, handles.addr(), handles.byte_len(), UserAccess::Write)?;
    let size = read_u32(p, out_sizes.addr())?;
    if !oidl::ReadSizes::accepts_result_size(size) {
        return Err(Error::InvalidArgs);
    }
    // The fields the caller knows, and never its own `size`.
    let limit = oidl::ReadSizes::writable(size);
    let first = oidl::ReadSizes::BYTES_AT;
    let sizes_to = out_sizes
        .addr()
        .checked_add(first as u64)
        .ok_or(Error::Fault)?;
    checked(
        p,
        sizes_to,
        u64::from(limit) - first as u64,
        UserAccess::Write,
    )?;
    // The counts and the writer (B1, C2), as far as the caller's `size` reaches: a caller
    // that gives the first version's 12 gets the counts and nothing past them.
    let sizes = |p: &UserContext, byte_count: usize, handle_count: usize, sender: Identity| {
        let mut fields = [0; oidl::ReadSizes::SIZE as usize];
        let reported = oidl::ReadSizes {
            bytes: byte_count as u32,
            handles: handle_count as u32,
            sender_process: Some(sender.process),
            sender_principal: Some(sender.principal),
        };
        reported
            .encode_fields(&mut fields, limit)
            .map_err(|_| Error::Fault)?;
        copy_out(
            p,
            sizes_to,
            fields.get(first..limit as usize).ok_or(Error::Fault)?,
        )
    };
    let table = &p.process.handles;
    match ipc::channel_read(
        table,
        end.value(),
        need(end),
        bytes.capacity(),
        handles.capacity(),
    )? {
        // Too small: the counts and the writer still reach the caller (C2).
        ipc::Read::TooSmall {
            bytes: byte_count,
            handles: handle_count,
            sender,
        } => {
            sizes(p, byte_count, handle_count, sender)?;
            Err(Error::BufferTooSmall)
        }
        ipc::Read::Message {
            bytes: payload,
            handles: installed,
            sender,
        } => {
            let delivered = copy_out(p, bytes.addr(), &payload)
                .and_then(|()| copy_out_records(p, handles.addr(), &installed))
                .and_then(|()| sizes(p, payload.len(), installed.len(), sender));
            // The ranges were checked above; only an address space changing under the
            // call could fail here. Then the handles the program cannot see are closed.
            delivered.inspect_err(|_| {
                for received in &installed {
                    let _ = ipc::handle_close(table, received.handle, Need::NOTHING);
                }
            })
        }
    }
}

/// Copies a sized struct a call reads (OIDL §5.3, ADR-0005 P4): its `size` is read once and must
/// be a version this kernel knows, which `accepts` says; then exactly that many bytes are copied,
/// once, into `bytes`, to be decoded from the copy. Returns the size.
fn copy_request(
    p: &UserContext,
    addr: u64,
    accepts: fn(u32) -> bool,
    bytes: &mut [u8],
) -> Result<u32, Error> {
    let size = read_u32(p, addr)?;
    if !accepts(size) {
        return Err(Error::InvalidArgs);
    }
    let len = usize::try_from(size).map_err(|_| Error::InvalidArgs)?;
    copy_in(p, bytes.get_mut(..len).ok_or(Error::InvalidArgs)?, addr)?;
    Ok(size)
}

/// `memory_object_create` (C7): the out pointer is checked first, so a bad one costs nothing.
fn memory_object_create(
    p: &UserContext,
    request: rt::UserRef<oidl::ObjectRequest>,
    object: rt::UserRef<rt::Handle>,
) -> Result<(), Error> {
    checked(p, object.addr(), HANDLE_BYTES as u64, UserAccess::Write)?;
    let mut bytes = [0; oidl::ObjectRequest::SIZE as usize];
    let size = copy_request(
        p,
        request.addr(),
        oidl::ObjectRequest::accepts_param_size,
        &mut bytes,
    )?;
    let asked = oidl::ObjectRequest::decode(&bytes, size, oidl::VERSION.1)
        .map_err(|_| Error::InvalidArgs)?;
    let length = asked.length.ok_or(Error::InvalidArgs)?;
    let made = MemoryObject::create(&p.process.account, length)?;
    let made = ObjectRef::try_new(made).map_err(|_| Error::NoMemory)?;
    let table = &p.process.handles;
    let handle = p
        .process
        .grant(ipc::Object::Memory(made), oidl::MEMORY_OBJECT_RIGHTS.bits())?;
    copy_out(p, object.addr(), &handle.to_le_bytes()).inspect_err(|_| {
        let _ = ipc::handle_close(table, handle, Need::NOTHING);
    })
}

/// `memory_map` (C7, C8): the out pointer and the request are checked and copied before
/// anything is mapped.
fn memory_map(
    p: &UserContext,
    process: rt::HandleArg,
    object: rt::HandleArg,
    request: rt::UserRef<oidl::MapRequest>,
    address: rt::UserRef<u64>,
) -> Result<(), Error> {
    checked(p, address.addr(), 8, UserAccess::Write)?;
    let mut bytes = [0; oidl::MapRequest::SIZE as usize];
    let size = copy_request(
        p,
        request.addr(),
        oidl::MapRequest::accepts_param_size,
        &mut bytes,
    )?;
    let asked =
        oidl::MapRequest::decode(&bytes, size, oidl::VERSION.1).map_err(|_| Error::InvalidArgs)?;
    let request = Request {
        access: asked.access.ok_or(Error::InvalidArgs)?.bits(),
        flags: asked.flags.ok_or(Error::InvalidArgs)?.bits(),
        offset: asked.offset.ok_or(Error::InvalidArgs)?,
        length: asked.length.ok_or(Error::InvalidArgs)?,
        address: asked.address.ok_or(Error::InvalidArgs)?,
    };
    let target = memory_object::process_of(&p.process, process.value(), need(process))?;
    let (object, held) = memory_object::object_of(&p.process, object.value(), need(object))?;
    // The pointer was checked above; only the caller's own space changing under the call can
    // make this write fail. The map then takes the mapping back, which nothing else could
    // touch before its address was written, and fails with the write's error: a result is
    // written only by a call that succeeds (OIDL `@also_on`).
    let tell = |at: u64| copy_out(p, address.addr(), &at.to_le_bytes());
    memory_object::map(&p.process, &target, &object, held, &request, &tell).map(|_| ())
}

/// `wait_one`. The decoder has refused an empty mask and undefined signals.
fn wait_one(
    p: &UserContext,
    handle: rt::HandleArg,
    signals: oidl::Signals,
    deadline: u64,
    observed: rt::UserRef<oidl::Signals>,
) -> Result<(), Error> {
    // Checked before waiting, so a bad pointer fails at once.
    checked(p, observed.addr(), 4, UserAccess::Write)?;
    let waited = ipc::wait_one(
        &p.process.handles,
        &p.process.waiting,
        handle.value(),
        need(handle),
        signals.bits(),
        deadline,
    )?;
    p.process.count_blocked(waited.blocked);
    copy_out(p, observed.addr(), &waited.observed.to_le_bytes())?;
    waited.status
}
