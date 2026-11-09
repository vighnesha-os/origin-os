//! The system-call layer (ADR-0005 P4, P5; CLAUDE.md §29, §42): what a `syscall` from ring 3
//! does once `cpu::dispatch` hands it over.
//!
//! - RAX is the call number and RDI, RSI, RDX, R10, R8, R9 the arguments. RAX returns `OK` or
//!   a negative status; everything else goes through out pointers.
//! - The call table, the decoding of the arguments and the statuses each call may return come
//!   from `sdk/interfaces/origin.kernel.oidl`, through the generated crate `origin-syscalls`
//!   (ADR-0011 N14). This module implements its `Calls` for [`Process`] and converts at that
//!   boundary: the layers below keep `origin-abi`'s types.
//! - The rights a handle must have are declared in the definition and enforced by the handle
//!   table (`docs/spec/OIDL.md` §6), in `ipc` and `handles`, with the same rights; a host test
//!   in `origin-syscalls` holds the two equal.
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
use origin_abi::{Error, Handle};
use origin_frame::trap::SyscallOutcome;
use origin_syscalls::oidl_rt as rt;
use origin_syscalls::origin_kernel::kernel::server;
use origin_syscalls::origin_kernel::types as oidl;

use crate::console::Console;
use crate::process::{Ending, Process};
use crate::{cpu, ipc, sched};

/// The most handles in one message, as a length.
const MAX_HANDLES: usize = oidl::MAX_MESSAGE_HANDLES as usize;
/// Bytes of one handle in user memory.
const HANDLE_BYTES: usize = <rt::Handle as rt::Fixed>::SIZE;

/// Runs one system call from ring 3: the number, its six arguments, and the address after
/// the `syscall` instruction. Interrupts are off on entry and on return.
pub(crate) fn run(call: u64, args: [u64; 6], rip: u64) -> SyscallOutcome {
    let Some(process) = sched::current_process() else {
        // A thread in user mode always has a process. If that ever failed, ending the
        // thread is the only safe thing: it can do nothing without one.
        return SyscallOutcome::ThreadExit;
    };
    if !process.may_call_from(rip) {
        return SyscallOutcome::Status(oidl::Error::AccessDenied.raw());
    }
    cpu::enable_interrupts();
    let outcome = match server::dispatch(&*process, call, args) {
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
    let Some(process) = sched::current_process() else {
        return;
    };
    if !stack_fixed {
        cpu::enable_interrupts();
    }
    process.thread_ended(Ending::Faulted {
        vector,
        error,
        address,
    });
    cpu::disable_interrupts();
}

/// A status from the layers below, as one of the errors the definition declares for this call.
/// Every status they can return for it is declared (`docs/engineering/OIDL_KERNEL_COMPARISON.md`);
/// were that ever wrong, the caller still gets a failure its call can report: `otherwise`.
fn declared<E: rt::MethodError>(otherwise: E) -> impl FnOnce(Error) -> E {
    move |e| E::from_status(e.raw()).unwrap_or(otherwise)
}

/// What the definition asks of this handle.
const fn need(handle: rt::HandleArg) -> Need {
    Need {
        rights: handle.need_rights(),
        object: handle.need_object(),
    }
}

/// The kernel's side of the generated interface: each call on behalf of this process.
impl server::Calls for Process {
    fn holds(&self, capability: oidl::Capability) -> bool {
        match capability {
            oidl::Capability::KernelDebug => self.debug_calls(),
            // A permission this kernel does not know is not held.
            _ => false,
        }
    }

    fn thread_exit(&self, code: u64) {
        self.thread_ended(Ending::Exited(code));
    }

    fn debug_report(&self, value: u64) -> Result<(), server::DebugReportError> {
        self.report(value);
        Ok(())
    }

    fn debug_write(&self, text: rt::UserSlice<u8>) -> Result<(), server::DebugWriteError> {
        debug_write(self, text).map_err(declared(server::DebugWriteError::Fault))
    }

    fn handle_close(&self, handle: rt::HandleArg) -> Result<(), server::HandleCloseError> {
        ipc::handle_close(&self.handles, handle.value(), need(handle))
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
        handles: rt::UserBuffer<rt::Handle>,
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
}

/// A pointer and length, checked against the caller's tables.
fn checked(p: &Process, addr: u64, len: u64, access: UserAccess) -> Result<(), Error> {
    p.space().check_user(addr, len, access)
}

/// Fills `dst` from the caller's memory at `src`.
fn copy_in(p: &Process, dst: &mut [u8], src: u64) -> Result<(), Error> {
    p.space().copy_in(dst, src)
}

/// Copies `src` into the caller's memory at `dst`.
fn copy_out(p: &Process, dst: u64, src: &[u8]) -> Result<(), Error> {
    p.space().copy_out(dst, src)
}

/// The little-endian `u32` at `addr` in the caller's memory, read once.
fn read_u32(p: &Process, addr: u64) -> Result<u32, Error> {
    let mut bytes = [0; 4];
    copy_in(p, &mut bytes, addr)?;
    Ok(u32::from_le_bytes(bytes))
}

/// Writes the handles in `values` to the caller's memory at `addr`, as little-endian `u32`s.
fn copy_out_handles(p: &Process, addr: u64, values: &[Handle]) -> Result<(), Error> {
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

/// A kernel buffer of `len` zero bytes, or [`Error::NoMemory`].
fn zeroed(len: usize) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(len).map_err(|_| Error::NoMemory)?;
    bytes.resize(len, 0);
    Ok(bytes)
}

/// `debug_write`: one console line, developer builds only.
fn debug_write(p: &Process, text: rt::UserSlice<u8>) -> Result<(), Error> {
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
    p: &Process,
    handle: rt::HandleArg,
    rights: oidl::Rights,
    new_handle: rt::UserRef<rt::Handle>,
) -> Result<(), Error> {
    checked(p, new_handle.addr(), HANDLE_BYTES as u64, UserAccess::Write)?;
    let new = ipc::handle_duplicate(&p.handles, handle.value(), need(handle), rights.bits())?;
    copy_out(p, new_handle.addr(), &new.to_le_bytes()).inspect_err(|_| {
        let _ = ipc::handle_close(&p.handles, new, Need::NOTHING);
    })
}

/// `channel_create`: the two handles, one after the other.
fn channel_create(p: &Process, ends: rt::UserRef<[rt::Handle; 2]>) -> Result<(), Error> {
    checked(p, ends.addr(), 2 * HANDLE_BYTES as u64, UserAccess::Write)?;
    let (first, second) = ipc::create_channel(&p.account)?;
    let rights = oidl::CHANNEL_RIGHTS.bits();
    let a = p.grant(ipc::Object::Channel(first), rights)?;
    let b = match p.grant(ipc::Object::Channel(second), rights) {
        Ok(b) => b,
        Err(e) => {
            let _ = ipc::handle_close(&p.handles, a, Need::NOTHING);
            return Err(e);
        }
    };
    copy_out_handles(p, ends.addr(), &[a, b]).inspect_err(|_| {
        let _ = ipc::handle_close(&p.handles, a, Need::NOTHING);
        let _ = ipc::handle_close(&p.handles, b, Need::NOTHING);
    })
}

/// `channel_write`. The decoder has checked both counts against their limits.
fn channel_write(
    p: &Process,
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
    ipc::channel_write(&p.handles, &p.account, end.value(), payload, transfer)
}

/// `channel_read`. The decoder has clamped both capacities to what a message can hold.
fn channel_read(
    p: &Process,
    end: rt::HandleArg,
    bytes: rt::UserBuffer<u8>,
    handles: rt::UserBuffer<rt::Handle>,
    out_sizes: rt::UserRef<oidl::ReadSizes>,
) -> Result<(), Error> {
    // The out pointers are checked before a message is taken: a bad one leaves it queued.
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
    let sizes = |p: &Process, byte_count: usize, handle_count: usize| {
        let mut fields = [0; oidl::ReadSizes::SIZE as usize];
        let reported = oidl::ReadSizes {
            bytes: byte_count as u32,
            handles: handle_count as u32,
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
    match ipc::channel_read(
        &p.handles,
        end.value(),
        bytes.capacity(),
        handles.capacity(),
    )? {
        ipc::Read::TooSmall {
            bytes: byte_count,
            handles: handle_count,
        } => {
            sizes(p, byte_count, handle_count)?;
            Err(Error::BufferTooSmall)
        }
        ipc::Read::Message {
            bytes: payload,
            handles: installed,
        } => {
            let delivered = copy_out(p, bytes.addr(), &payload)
                .and_then(|()| copy_out_handles(p, handles.addr(), &installed))
                .and_then(|()| sizes(p, payload.len(), installed.len()));
            // The ranges were checked above; only an address space changing under the
            // call could fail here. Then the handles the program cannot see are closed.
            delivered.inspect_err(|_| {
                for &value in &installed {
                    let _ = ipc::handle_close(&p.handles, value, Need::NOTHING);
                }
            })
        }
    }
}

/// `wait_one`. The decoder has refused an empty mask and undefined signals.
fn wait_one(
    p: &Process,
    handle: rt::HandleArg,
    signals: oidl::Signals,
    deadline: u64,
    observed: rt::UserRef<oidl::Signals>,
) -> Result<(), Error> {
    // Checked before waiting, so a bad pointer fails at once.
    checked(p, observed.addr(), 4, UserAccess::Write)?;
    let waited = ipc::wait_one(
        &p.handles,
        &p.waiting,
        handle.value(),
        signals.bits(),
        deadline,
    )?;
    p.count_blocked(waited.blocked);
    copy_out(p, observed.addr(), &waited.observed.to_le_bytes())?;
    waited.status
}
