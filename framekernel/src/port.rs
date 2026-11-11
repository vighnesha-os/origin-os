//! Ports and the bindings that fill them (ADR-0005 C9, P9; Amendment 7 F2–F4, F7, F9, F11–F16,
//! F18; task S3-6): the kernel's side of the framework's protocol,
//! `origin_frame::sync::ports` (task S3-6a), and the four calls built on it.
//!
//! - A port is a [`KernelPort`]: the protocol's [`Port`], how many handles reach it, and the
//!   account of the process that made it, charged one port until the port is freed (F7).
//! - Each handle to a port, in a table or in a message, is a [`PortHandle`]. When the last one
//!   goes the port closes: every binding armed for it and every packet queued in it goes, silently,
//!   and each one's charge comes back (F11). An armed binding holds the port's memory
//!   ([`PortRef`]) and not a handle, so it never keeps a port open.
//! - What can be bound is what `wait_one` can wait on (F18), named by the same counted reference,
//!   `ipc::Waitable`: it is also the port's way back to the object.
//! - A binding's storage carries a [`Binding`]: one binding charged to the binder's account from
//!   the bind until its packet has left the port (F7, F12). Dropping it is the refund, so whoever
//!   ends up with a binding's storage refunds by dropping it, with no lock held.
//! - A binding's owner is the binder's process and the handle it was made through ([`Owner`]).
//!   Closing or moving that handle cancels the binding implicitly (F16, `ipc::handle_gone`), and an
//!   exit cancels every binding the process made (`ipc::close_table`).
//!
//! Lock order, the protocol's: a source's lock, then a handle table (a bind asks whether its
//! handles are still there), then a channel's state (what a source's signals read). A port's lock
//! is a leaf. So nothing calls into the protocol while it holds a handle table or a channel's
//! lock, and a port's last handle is never dropped under one.
#![forbid(unsafe_code)]
#![expect(
    dead_code,
    reason = "the system calls of task S3-6 use it: the interface step, the next commit"
)]

use core::ops::Deref;
use core::sync::atomic::{AtomicU32, Ordering};

use origin_abi::{Error, Handle};
use origin_frame::objref::ObjectRef;
use origin_frame::sync::SpinLock;
use origin_frame::sync::ports::{
    self, Bound, NoMemory, Packet, Port, Refused, Request, Reservation, Source, TakeError,
};

use crate::cpu::without_interrupts;
use crate::handles::{HandleTable, Need};
use crate::ipc::{Object, Waitable};
use crate::process::Account;
use crate::wait::KernelThreads;

/// The kernel, as the ports' protocol sees it.
pub(crate) struct Ports;

impl ports::Kernel for Ports {
    type Scheduler = KernelThreads;
    type Payload = Binding;
    type Owner = Owner;
    type PortRef = PortRef;
    type SourceRef = Waitable;
}

/// Who made a binding: an implicit cancel picks it by this (F16).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Owner {
    /// The binder's process ID, never reused (C1).
    pub(crate) process: u64,
    /// The handle it was made through, in that process's table. A value names one generation of
    /// one slot, so no later handle has it (P1).
    pub(crate) handle: Handle,
}

/// One binding charged to its binder's account (F7, F12). Dropped, it is refunded.
pub(crate) struct Binding(ObjectRef<Account>);

impl Binding {
    /// One more binding on `account`, or [`Error::Limit`] past its quota.
    fn charge(account: &ObjectRef<Account>) -> Result<Self, Error> {
        account.objects().bindings.charge()?;
        Ok(Self(account.clone()))
    }
}

impl Drop for Binding {
    fn drop(&mut self) {
        self.0.objects().bindings.refund();
    }
}

/// A port, as the kernel keeps it.
pub(crate) struct KernelPort {
    port: Port<Ports>,
    /// Handles that reach it, in tables and in messages.
    open: AtomicU32,
    /// Its creator's account, charged one port while this exists (F7).
    account: ObjectRef<Account>,
}

impl Drop for KernelPort {
    fn drop(&mut self) {
        self.account.objects().ports.refund();
    }
}

/// A counted reference to a port's memory, not one of its handles: what an armed binding and a
/// waiting thread hold.
#[derive(Clone)]
pub(crate) struct PortRef(ObjectRef<KernelPort>);

impl Deref for PortRef {
    type Target = Port<Ports>;

    fn deref(&self) -> &Port<Ports> {
        &self.0.port
    }
}

/// One handle to a port, in a table or in a message.
pub(crate) struct PortHandle(ObjectRef<KernelPort>);

impl Clone for PortHandle {
    /// A second handle to this port: a duplicate.
    fn clone(&self) -> Self {
        self.0.open.fetch_add(1, Ordering::Relaxed);
        Self(self.0.clone())
    }
}

impl Drop for PortHandle {
    /// The last handle closes the port (F11): every binding armed for it and every packet queued
    /// in it goes, and the charges come back as their storage is dropped here.
    fn drop(&mut self) {
        if self.0.open.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        let removed = self.0.port.close(&KernelThreads);
        drop(removed);
    }
}

impl PortHandle {
    /// The port's memory, not one more handle: for waking the threads waiting through a handle
    /// that has gone (F3).
    pub(crate) fn memory(&self) -> PortRef {
        PortRef(self.0.clone())
    }
}

/// `port_create` (F17): a new port charged to `account`, and its one handle. [`Error::Limit`]
/// past the account's quota of ports; [`Error::NoMemory`] without the memory, with the charge
/// given back.
pub(crate) fn create(account: &ObjectRef<Account>) -> Result<PortHandle, Error> {
    account.objects().ports.charge()?;
    let port = KernelPort {
        port: Port::new(),
        open: AtomicU32::new(1),
        account: account.clone(),
    };
    // A failure hands the port back, and its drop gives the charge back.
    let port = ObjectRef::try_new(port).map_err(|_| Error::NoMemory)?;
    Ok(PortHandle(port))
}

/// The port behind `handle`, if it meets `need` and names a port.
fn port_of(table: &HandleTable<Object>, handle: Handle, need: Need) -> Result<PortRef, Error> {
    match table.get(handle, need)? {
        (Object::Port(port), _) => Ok(PortRef(port.0.clone())),
        _ => Err(Error::WrongType),
    }
}

/// What one call names: a port, and maybe an object's signals.
fn port_and_object(
    table: &SpinLock<HandleTable<Object>>,
    port: (Handle, Need),
    object: (Handle, Need),
) -> Result<(PortRef, Option<Waitable>), Error> {
    without_interrupts(|| {
        let table = table.lock();
        // The port's handle, then the object's (D5).
        let port = port_of(&table, port.0, port.1)?;
        let (object, _) = table.get(object.0, object.1)?;
        Ok((port, Waitable::of(object)))
    })
}

/// `object_wait_async` (F2, F9, F13, F18): binds the signals in `mask` of the object behind
/// `object` to the port behind `port`, with `key`, for `process`, charged to `account`. The
/// decoder has refused an empty mask and undeclared signals.
///
/// The first failure wins: the port's handle, then the object's; an object that raises no
/// signals ([`Error::BadHandle`], as `wait_one` answers); the quota ([`Error::Limit`]); the
/// memory ([`Error::NoMemory`], with the charge given back). A handle closed by another thread
/// during the call, either of the two, is [`Error::BadHandle`], and nothing stays bound.
pub(crate) fn bind(
    table: &SpinLock<HandleTable<Object>>,
    account: &ObjectRef<Account>,
    process: u64,
    port: (Handle, Need),
    object: (Handle, Need),
    key: u64,
    mask: u32,
) -> Result<(), Error> {
    let (port_ref, source) = port_and_object(table, port, object)?;
    let source = source.ok_or(Error::BadHandle)?;
    let binding = Binding::charge(account)?;
    // On failure the binding is dropped, which gives the charge back.
    let reservation = Reservation::<Ports>::try_new(&KernelThreads, &port_ref, binding)
        .map_err(|NoMemory| Error::NoMemory)?;
    let request = Request {
        owner: Owner {
            process,
            handle: object.0,
        },
        key,
        mask,
    };
    // Asked under the source's lock: a handle closed before this binds nothing, and one closed
    // after cancels what this bound (F16), or leaves it, for the port's (F16, its port side).
    let still_open = || {
        without_interrupts(|| {
            let table = table.lock();
            table.get(object.0, Need::NOTHING).is_ok() && table.get(port.0, Need::NOTHING).is_ok()
        })
    };
    let bound = Source::bind(
        &source,
        &KernelThreads,
        reservation,
        request,
        still_open,
        || source.signals(),
    );
    match bound {
        Ok(Bound::Armed | Bound::Queued) => Ok(()),
        // The reservation goes here, and its charge with it.
        Err((Refused::HandleClosed | Refused::PortClosed, _)) => Err(Error::BadHandle),
        // Never while each reservation keeps its room (S3-6a); reported rather than allocated.
        Err((Refused::NoRoom, _)) => Err(Error::NoMemory),
    }
}

/// `port_wait` (F3, F14, F15): the oldest packet queued in the port behind `handle`, waiting
/// until the monotonic clock reaches `deadline` (0 polls). [`Error::TimedOut`] with nothing
/// queued by then; [`Error::Canceled`] if `handle` is closed or moved meanwhile, even while a
/// duplicate keeps the port open, and such a wait takes no packet. A packet taken has left the
/// port: its binding's charge comes back here (F12). `waiting` counts the caller's threads in a
/// wait, for the boot proofs.
pub(crate) fn wait(
    table: &SpinLock<HandleTable<Object>>,
    waiting: &AtomicU32,
    handle: Handle,
    need: Need,
    deadline: u64,
) -> Result<Packet, Error> {
    let port = without_interrupts(|| port_of(&table.lock(), handle, need))?;
    let still_open = || without_interrupts(|| table.lock().get(handle, Need::NOTHING).is_ok());
    waiting.fetch_add(1, Ordering::Release);
    let taken = port.take(&KernelThreads, deadline, still_open);
    waiting.fetch_sub(1, Ordering::AcqRel);
    let (packet, storage) = taken.map_err(|e| match e {
        TakeError::TimedOut => Error::TimedOut,
        TakeError::Canceled => Error::Canceled,
    })?;
    drop(storage);
    Ok(packet)
}

/// `port_cancel` (F4, F9): removes every binding armed for the port behind `port` on the object
/// behind `object` with `key`, and every packet from such a binding queued in the port, of either
/// kind, silently; each one's charge comes back. [`Error::NotFound`] when nothing matched, which
/// is always so for an object that raises no signals.
pub(crate) fn cancel(
    table: &SpinLock<HandleTable<Object>>,
    port: (Handle, Need),
    object: (Handle, Need),
    key: u64,
) -> Result<(), Error> {
    let (port_ref, source) = port_and_object(table, port, object)?;
    let source = source.ok_or(Error::NotFound)?;
    let removed = source.cancel(&KernelThreads, &port_ref, key);
    let found = !removed.is_empty();
    // Their charges come back here, with no lock held.
    drop(removed);
    if found { Ok(()) } else { Err(Error::NotFound) }
}
