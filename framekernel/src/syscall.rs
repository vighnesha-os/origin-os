//! The system-call layer (ADR-0005 P4, P5; CLAUDE.md §29, §42): what a `syscall` from ring 3
//! does once `cpu::dispatch` hands it over.
//!
//! - RAX is the call number and RDI, RSI, RDX, R10, R8, R9 the arguments. RAX returns `OK` or
//!   a negative [`Error`]; everything else goes through out pointers. The numbers and codes
//!   are `origin-abi`'s, the one definition.
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

use crate::usercopy::UserAccess;
use origin_abi::limits::{MAX_MESSAGE_BYTES, MAX_MESSAGE_HANDLES};
use origin_abi::{Error, Handle, OK, read_sizes, signals, syscall as number};
use origin_frame::trap::SyscallOutcome;

use crate::console::Console;
use crate::process::{Ending, Process};
use crate::{cpu, ipc, sched};

/// Runs one system call from ring 3: the number, its six arguments, and the address after
/// the `syscall` instruction. Interrupts are off on entry and on return.
pub(crate) fn run(call: u64, args: [u64; 6], rip: u64) -> SyscallOutcome {
    let Some(process) = sched::current_process() else {
        // A thread in user mode always has a process. If that ever failed, ending the
        // thread is the only safe thing: it can do nothing without one.
        return SyscallOutcome::ThreadExit;
    };
    if !process.may_call_from(rip) {
        return SyscallOutcome::Status(Error::AccessDenied.raw());
    }
    cpu::enable_interrupts();
    let outcome = dispatch(&process, call, args);
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

fn dispatch(p: &Process, call: u64, [a0, a1, a2, a3, a4, a5]: [u64; 6]) -> SyscallOutcome {
    let result = match call {
        number::THREAD_EXIT => {
            p.thread_ended(Ending::Exited(a0));
            return SyscallOutcome::ThreadExit;
        }
        number::DEBUG_REPORT | number::DEBUG_WRITE if !p.debug_calls() => Err(Error::NotSupported),
        number::DEBUG_REPORT => {
            p.report(a0);
            Ok(())
        }
        number::DEBUG_WRITE => debug_write(p, a0, a1),
        number::HANDLE_CLOSE => handle_close(p, a0),
        number::HANDLE_DUPLICATE => handle_duplicate(p, a0, a1, a2),
        number::CHANNEL_CREATE => channel_create(p, a0),
        number::CHANNEL_WRITE => channel_write(p, a0, a1, a2, a3, a4),
        number::CHANNEL_READ => channel_read(p, a0, a1, a2, a3, a4, a5),
        number::WAIT_ONE => wait_one(p, a0, a1, a2, a3),
        _ => Err(Error::NotSupported),
    };
    SyscallOutcome::Status(match result {
        Ok(()) => OK,
        Err(e) => e.raw(),
    })
}

/// A handle argument: a value with bits above 32 is no handle.
fn handle_arg(raw: u64) -> Result<Handle, Error> {
    Handle::try_from(raw).map_err(|_| Error::BadHandle)
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
    let mut bytes = [0; 4 * MAX_MESSAGE_HANDLES];
    let dst = bytes
        .get_mut(..4 * values.len())
        .ok_or(Error::InvalidArgs)?;
    let (chunks, _) = dst.as_chunks_mut::<4>();
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

/// `write(buffer, length)`: one console line, developer builds only.
fn debug_write(p: &Process, buffer: u64, length: u64) -> Result<(), Error> {
    let mut text = [0u8; 128];
    let dst = usize::try_from(length)
        .ok()
        .and_then(|n| text.get_mut(..n))
        .ok_or(Error::InvalidArgs)?;
    copy_in(p, dst, buffer)?;
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

/// `close(handle)`.
fn handle_close(p: &Process, handle: u64) -> Result<(), Error> {
    ipc::handle_close(&p.handles, handle_arg(handle)?)
}

/// `duplicate(handle, rights, out_handle)`.
fn handle_duplicate(p: &Process, handle: u64, rights: u64, out_handle: u64) -> Result<(), Error> {
    let handle = handle_arg(handle)?;
    let rights = u32::try_from(rights).map_err(|_| Error::InvalidArgs)?;
    checked(p, out_handle, 4, UserAccess::Write)?;
    let new = ipc::handle_duplicate(&p.handles, handle, rights)?;
    copy_out(p, out_handle, &new.to_le_bytes()).inspect_err(|_| {
        let _ = ipc::handle_close(&p.handles, new);
    })
}

/// `channel_create(out_handles)`: the two handles, one after the other.
fn channel_create(p: &Process, out_handles: u64) -> Result<(), Error> {
    checked(p, out_handles, 8, UserAccess::Write)?;
    let (first, second) = ipc::create_channel(&p.account)?;
    let a = p.grant(ipc::Object::Channel(first), origin_abi::rights::CHANNEL)?;
    let b = match p.grant(ipc::Object::Channel(second), origin_abi::rights::CHANNEL) {
        Ok(b) => b,
        Err(e) => {
            let _ = ipc::handle_close(&p.handles, a);
            return Err(e);
        }
    };
    copy_out_handles(p, out_handles, &[a, b]).inspect_err(|_| {
        let _ = ipc::handle_close(&p.handles, a);
        let _ = ipc::handle_close(&p.handles, b);
    })
}

/// `channel_write(handle, bytes, length, handles, count)`.
fn channel_write(
    p: &Process,
    handle: u64,
    bytes: u64,
    length: u64,
    handles: u64,
    count: u64,
) -> Result<(), Error> {
    let handle = handle_arg(handle)?;
    let length = usize::try_from(length).map_err(|_| Error::InvalidArgs)?;
    let count = usize::try_from(count).map_err(|_| Error::InvalidArgs)?;
    if length > MAX_MESSAGE_BYTES || count > MAX_MESSAGE_HANDLES {
        return Err(Error::InvalidArgs);
    }
    // Copied once, into kernel buffers, before anything else: a second thread of the caller
    // cannot change either list after the checks below.
    let mut payload = zeroed(length)?;
    copy_in(p, &mut payload, bytes)?;
    let mut raw = [0; 4 * MAX_MESSAGE_HANDLES];
    let raw_used = raw.get_mut(..4 * count).ok_or(Error::InvalidArgs)?;
    copy_in(p, raw_used, handles)?;
    let mut values = [0; MAX_MESSAGE_HANDLES];
    let (chunks, _) = raw_used.as_chunks::<4>();
    for (value, chunk) in values.iter_mut().zip(chunks) {
        *value = u32::from_le_bytes(*chunk);
    }
    let transfer = values.get(..count).ok_or(Error::InvalidArgs)?;
    ipc::channel_write(&p.handles, &p.account, handle, payload, transfer)
}

/// `channel_read(handle, bytes, capacity, handles, capacity, out_sizes)`.
fn channel_read(
    p: &Process,
    handle: u64,
    bytes: u64,
    byte_capacity: u64,
    handles: u64,
    handle_capacity: u64,
    out_sizes: u64,
) -> Result<(), Error> {
    let handle = handle_arg(handle)?;
    // Anything above the limits can never be needed.
    let byte_capacity = byte_capacity.min(MAX_MESSAGE_BYTES as u64) as usize;
    let handle_capacity = handle_capacity.min(MAX_MESSAGE_HANDLES as u64) as usize;
    // The out pointers are checked before a message is taken: a bad one leaves it queued.
    checked(p, bytes, byte_capacity as u64, UserAccess::Write)?;
    checked(p, handles, 4 * handle_capacity as u64, UserAccess::Write)?;
    if read_u32(p, out_sizes)? < read_sizes::SIZE {
        return Err(Error::InvalidArgs);
    }
    let sizes_to = out_sizes
        .checked_add(read_sizes::BYTES_AT as u64)
        .ok_or(Error::Fault)?;
    checked(
        p,
        sizes_to,
        u64::from(read_sizes::SIZE) - 4,
        UserAccess::Write,
    )?;
    let sizes = |p: &Process, byte_count: usize, handle_count: usize| {
        let mut fields = [0; 8];
        fields
            .get_mut(..4)
            .ok_or(Error::Fault)?
            .copy_from_slice(&(byte_count as u32).to_le_bytes());
        fields
            .get_mut(4..)
            .ok_or(Error::Fault)?
            .copy_from_slice(&(handle_count as u32).to_le_bytes());
        copy_out(p, sizes_to, &fields)
    };
    match ipc::channel_read(&p.handles, handle, byte_capacity, handle_capacity)? {
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
            let delivered = copy_out(p, bytes, &payload)
                .and_then(|()| copy_out_handles(p, handles, &installed))
                .and_then(|()| sizes(p, payload.len(), installed.len()));
            // The ranges were checked above; only an address space changing under the
            // call could fail here. Then the handles the program cannot see are closed.
            delivered.inspect_err(|_| {
                for &value in &installed {
                    let _ = ipc::handle_close(&p.handles, value);
                }
            })
        }
    }
}

/// `wait_one(handle, signals, deadline, out_observed)`.
fn wait_one(
    p: &Process,
    handle: u64,
    mask: u64,
    deadline: u64,
    out_observed: u64,
) -> Result<(), Error> {
    let handle = handle_arg(handle)?;
    let mask = u32::try_from(mask).map_err(|_| Error::InvalidArgs)?;
    if mask == 0 || mask & !signals::ALL != 0 {
        return Err(Error::InvalidArgs);
    }
    // Checked before waiting, so a bad pointer fails at once.
    checked(p, out_observed, 4, UserAccess::Write)?;
    let waited = ipc::wait_one(&p.handles, &p.waiting, handle, mask, deadline)?;
    p.count_blocked(waited.blocked);
    copy_out(p, out_observed, &waited.observed.to_le_bytes())?;
    waited.status
}
