//! Events (ADR-0005 C10, P3, P9; Amendment 7 F5, F6, F17; task S3-6): an object with one signal
//! a program raises and clears, `SIGNALED`. A thread waits for it with `wait_one`, or binds it to a
//! port (`port`).
//!
//! - An event starts with no signal raised (F17), and is charged to the account of the process
//!   that made it, one event until the event is freed (F7).
//! - `object_signal` changes its signals in one atomic step, `(old & !clear) | set` (F5). Then,
//!   with no lock held, every thread waiting on it wakes to look again, and every binding armed on
//!   it whose mask the new signals meet fires: the state first, then the wake-ups, as every waker
//!   does (`wait`, and the ports' protocol).
//! - There is no pulse: a signal raised and cleared again by the time a waiter looks may be
//!   missed by that waiter, as C10 says.
#![forbid(unsafe_code)]

use core::sync::atomic::{AtomicU32, Ordering};

use origin_abi::{Error, Handle, signals};
use origin_frame::objref::ObjectRef;
use origin_frame::sync::SpinLock;
use origin_frame::sync::ports::Source;

use crate::cpu::without_interrupts;
use crate::handles::{HandleTable, Need};
use crate::ipc::{Object, note_cross_cpu_wakes};
use crate::port::Ports;
use crate::process::Account;
use crate::wait::{KernelThreads, WaitQueue};

/// The event `handle` names in `table`, if the handle meets `need`: [`Error::WrongType`] for any
/// other object, after the handle's own checks (F5's order).
pub(crate) fn event_of(
    table: &SpinLock<HandleTable<Object>>,
    handle: Handle,
    need: Need,
) -> Result<ObjectRef<Event>, Error> {
    without_interrupts(|| match table.lock().get(handle, need)? {
        (Object::Event(event), _) => Ok(event.clone()),
        _ => Err(Error::WrongType),
    })
}

/// One event.
pub(crate) struct Event {
    /// Its signals: `SIGNALED` or none.
    signals: AtomicU32,
    /// Threads in `wait_one` on it.
    waiters: WaitQueue,
    /// The bindings armed on it.
    source: Source<Ports>,
    /// Its creator's account, charged one event while this exists (F7).
    account: ObjectRef<Account>,
}

impl Drop for Event {
    fn drop(&mut self) {
        self.account.objects().events.refund();
    }
}

impl Event {
    /// A new event charged to `account`, with no signal raised (F17): [`Error::Limit`] past the
    /// account's quota of events, [`Error::NoMemory`] without the memory, with the charge given
    /// back.
    pub(crate) fn create(account: &ObjectRef<Account>) -> Result<ObjectRef<Self>, Error> {
        account.objects().events.charge()?;
        let event = Self {
            signals: AtomicU32::new(0),
            waiters: WaitQueue::new(),
            source: Source::new(),
            account: account.clone(),
        };
        // A failure hands the event back, and its drop gives the charge back.
        ObjectRef::try_new(event).map_err(|_| Error::NoMemory)
    }

    /// Its signals now.
    pub(crate) fn signals(&self) -> u32 {
        self.signals.load(Ordering::Acquire)
    }

    /// The threads waiting on it.
    pub(crate) const fn waiters(&self) -> &WaitQueue {
        &self.waiters
    }

    /// The bindings armed on it.
    pub(crate) const fn source(&self) -> &Source<Ports> {
        &self.source
    }

    /// `object_signal` on this event (F5): clears `clear`, then sets `set`, as one step, so a
    /// signal in both ends set. Only `SIGNALED` may be named: any other signal in either mask is
    /// [`Error::InvalidArgs`] and changes nothing. Two empty masks change nothing and succeed.
    pub(crate) fn signal(&self, clear: u32, set: u32) -> Result<(), Error> {
        if (clear | set) & !signals::SIGNALED != 0 {
            return Err(Error::InvalidArgs);
        }
        if clear | set == 0 {
            return Ok(());
        }
        // One atomic step; the closure always gives a value, so this never fails.
        let _ = self
            .signals
            .try_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                Some((old & !clear) | set)
            });
        note_cross_cpu_wakes(self.waiters.wake_all());
        self.source.deliver(&KernelThreads, || self.signals());
        Ok(())
    }
}
