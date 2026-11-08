//! Kernel channels (ADR-0005 P1, P2, P8, P9): the objects handles name, the moves between
//! handle tables and queues, and blocking until a signal.
//!
//! The queues and limits are the library's `framekernel::channel`; this module adds the
//! locks, the handle moves, the waiting and the lifetime:
//! - A [`ChannelEnd`] is one reference to one end: a handle in a table or a handle in a
//!   message. An end closes when its last reference goes, which is when its peer sees
//!   `PEER_CLOSED`.
//! - Closing never recurses. A message left in a closed end's queue can hold another end
//!   whose queue holds another, as deep as a hostile process cares to build, and the
//!   kernel stack is 16 KiB. So a close only parks its undelivered messages; each system
//!   call and each process exit drains them iteratively ([`reap_closed`]) and outside any
//!   lock.
//! - Sending a channel end is refused when that could make it unreachable (ADR-0005
//!   amendment A3): it must not be an end of the channel it is sent on, must be
//!   the only handle to that end, and must not end up inside its own queue. So every end in
//!   flight has exactly one reference, the graph of ends held in queues is a forest rooted at
//!   ends held in handle tables, and no reference cycle can keep a dead end alive.
//!
//! Lock order: a handle table, then [`TRANSFERS`], then a channel's state, then an end's
//! holder link. A wait queue's lock is taken before whatever a wait's condition reads (a
//! table, a channel's state); a waker holds none of these locks while it wakes.

use alloc::alloc::{Layout, alloc, dealloc};
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Deref;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering, fence};

use framekernel::channel::{Channel, Message, QUEUE_BYTES, QUEUE_DEPTH, ReadError, other};
use framekernel::handles::HandleTable;
use origin_abi::{Error, Handle, rights};
use origin_sync::SpinLock;

use crate::cpu::without_interrupts;
use crate::process::Account;
use crate::wait::WaitQueue;

/// A counted reference to a kernel object, created with [`ObjectRef::try_new`], which
/// reports an allocation failure instead of stopping the kernel the way `Arc::new` would
/// (`Arc::try_new` is unstable on Rust 1.99.1).
pub(crate) struct ObjectRef<T> {
    inner: NonNull<Counted<T>>,
}

struct Counted<T> {
    count: AtomicUsize,
    value: T,
}

// SAFETY: an `ObjectRef` shares `T` between its holders exactly like `Arc<T>` does: `T: Send`
// because the last holder, on any thread, drops it, and `T: Sync` because all holders read it.
unsafe impl<T: Send + Sync> Send for ObjectRef<T> {}
// SAFETY: as above.
unsafe impl<T: Send + Sync> Sync for ObjectRef<T> {}

impl<T> ObjectRef<T> {
    /// A new object, or the value back if the kernel heap has no room.
    fn try_new(value: T) -> Result<Self, T> {
        let layout = Layout::new::<Counted<T>>();
        // SAFETY: the layout has a non-zero size: `Counted` holds an `AtomicUsize`.
        let raw = unsafe { alloc(layout) }.cast::<Counted<T>>();
        let Some(inner) = NonNull::new(raw) else {
            return Err(value);
        };
        // SAFETY: `inner` is a fresh allocation with the layout of `Counted<T>`, so writing
        // one there is in bounds, aligned, and overwrites nothing that needs dropping.
        unsafe {
            inner.write(Counted {
                count: AtomicUsize::new(1),
                value,
            })
        };
        Ok(Self { inner })
    }

    /// Whether two references name the same object.
    fn same(a: &Self, b: &Self) -> bool {
        a.inner == b.inner
    }
}

impl<T> Deref for ObjectRef<T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: `inner` points at a `Counted<T>` that `try_new` initialized, kept alive by
        // this reference's count: `Drop` frees it only when the count reaches zero.
        unsafe { &self.inner.as_ref().value }
    }
}

impl<T> Clone for ObjectRef<T> {
    fn clone(&self) -> Self {
        // SAFETY: as in `deref`.
        let counted = unsafe { self.inner.as_ref() };
        counted.count.fetch_add(1, Ordering::Relaxed);
        Self { inner: self.inner }
    }
}

impl<T> Drop for ObjectRef<T> {
    fn drop(&mut self) {
        // SAFETY: as in `deref`; this reference is still counted here.
        let counted = unsafe { self.inner.as_ref() };
        if counted.count.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }
        // Everything the other holders did with the object happens before it is destroyed.
        fence(Ordering::Acquire);
        // SAFETY: this was the last reference, so nothing else can reach the object.
        unsafe { self.inner.drop_in_place() };
        // SAFETY: allocated by `try_new` with this layout, destroyed just above, never freed
        // before: only the holder that takes the count to zero gets here.
        unsafe { dealloc(self.inner.as_ptr().cast(), Layout::new::<Counted<T>>()) };
    }
}

/// What a handle names (ADR-0005 P3; only channels exist so far).
#[derive(Clone)]
pub(crate) enum Object {
    /// One end of a channel.
    Channel(ChannelEnd),
}

/// A handle in a message: its object and its rights, preserved in transit (P2).
pub(crate) type Entry = (Object, u32);

/// One end of a channel, as a reference: a handle in a table or a handle in a message.
pub(crate) struct ChannelEnd {
    channel: ObjectRef<KernelChannel>,
    side: usize,
}

impl Clone for ChannelEnd {
    /// A second reference to this end: a duplicated handle.
    fn clone(&self) -> Self {
        if let Some(open) = self.channel.open.get(self.side) {
            open.fetch_add(1, Ordering::Relaxed);
        }
        Self {
            channel: self.channel.clone(),
            side: self.side,
        }
    }
}

impl Drop for ChannelEnd {
    fn drop(&mut self) {
        let Some(open) = self.channel.open.get(self.side) else {
            return;
        };
        if open.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        if let Some(holder) = self.channel.holder.get(self.side) {
            // No longer in flight, if it was. Unlinking only removes an edge from the graph
            // the transfer check walks, so it needs no global lock.
            let link = without_interrupts(|| holder.lock().take());
            drop(link);
        }
        close_side(&self.channel, self.side);
    }
}

/// One end of a channel, named for the cycle check.
type EndId = (ObjectRef<KernelChannel>, usize);

/// A channel: its queues, and what the kernel keeps for each of its two ends.
pub(crate) struct KernelChannel {
    state: SpinLock<Channel<Entry>>,
    /// References to each end: handles in tables, handles in messages.
    open: [AtomicU32; 2],
    /// Threads in `wait_one` on each end.
    waiters: [WaitQueue; 2],
    /// Messages left when an end closed, until [`reap_closed`] drops them.
    undelivered: [SpinLock<VecDeque<Message<Entry>>>; 2],
    /// Where each end is in flight: the channel and side whose queue holds it.
    holder: [SpinLock<Option<EndId>>; 2],
    /// The next channel on the list [`reap_closed`] drains.
    next_closed: SpinLock<Option<ObjectRef<KernelChannel>>>,
    /// Whether this channel is on that list.
    listed: AtomicBool,
    /// Its creator's account, charged once for this channel's lifetime.
    account: Arc<Account>,
}

impl Drop for KernelChannel {
    fn drop(&mut self) {
        self.account.refund_channel();
    }
}

/// Serializes every addition of a channel end to the graph of ends held in queues, so the
/// cycle check always sees the whole graph. The walk under it visits at most one end per
/// channel in the chain, and each process's channels are bounded by its quota (today's are
/// small; a higher quota would also need a limit on the chain's depth).
static TRANSFERS: SpinLock<()> = SpinLock::new(());

/// Channels with undelivered messages waiting for [`reap_closed`].
static CLOSED: SpinLock<Option<ObjectRef<KernelChannel>>> = SpinLock::new(None);
/// How many channels are on that list: the common case is none, checked without the lock.
static CLOSED_COUNT: AtomicU64 = AtomicU64::new(0);

/// Wake-ups of threads blocked on another processor, for the boot proof.
static CROSS_CPU_WAKES: AtomicU64 = AtomicU64::new(0);

/// How many blocked threads a channel operation woke on another processor (by IPI).
pub(crate) fn cross_cpu_wakes() -> u64 {
    CROSS_CPU_WAKES.load(Ordering::Relaxed)
}

/// Wakes everything waiting on `side`.
fn wake(channel: &KernelChannel, side: usize) {
    if let Some(waiters) = channel.waiters.get(side) {
        let crossed = waiters.wake_all();
        CROSS_CPU_WAKES.fetch_add(crossed as u64, Ordering::Relaxed);
    }
}

/// `side` lost its last reference: it closes. Its waiters and the peer's are woken, and
/// its undelivered messages go on the list [`reap_closed`] drains.
fn close_side(channel: &ObjectRef<KernelChannel>, side: usize) {
    let undelivered = without_interrupts(|| channel.state.lock().close(side));
    if !undelivered.is_empty()
        && let Some(slot) = channel.undelivered.get(side)
    {
        without_interrupts(|| *slot.lock() = undelivered);
        list_closed(channel);
    }
    wake(channel, 0);
    wake(channel, 1);
}

/// Puts `channel` on the list [`reap_closed`] drains, unless it is there already.
fn list_closed(channel: &ObjectRef<KernelChannel>) {
    if channel.listed.swap(true, Ordering::AcqRel) {
        return;
    }
    without_interrupts(|| {
        let mut head = CLOSED.lock();
        *channel.next_closed.lock() = head.take();
        *head = Some(channel.clone());
    });
    CLOSED_COUNT.fetch_add(1, Ordering::Release);
}

/// Drops every undelivered message on the closed list, iteratively, with no lock held. Each
/// message's handles may close further ends, which only join this list: however deep the
/// chain, the stack stays flat.
pub(crate) fn reap_closed() {
    while CLOSED_COUNT.load(Ordering::Acquire) != 0 {
        let taken = without_interrupts(|| {
            let mut head = CLOSED.lock();
            let channel = head.take()?;
            *head = channel.next_closed.lock().take();
            Some(channel)
        });
        let Some(channel) = taken else {
            return;
        };
        // Off the list. A later close lists the channel again; one that saw it still listed
        // stored its messages before this swap, so the loop below takes them.
        channel.listed.swap(false, Ordering::AcqRel);
        CLOSED_COUNT.fetch_sub(1, Ordering::AcqRel);
        for slot in &channel.undelivered {
            let undelivered = without_interrupts(|| core::mem::take(&mut *slot.lock()));
            drop(undelivered);
        }
    }
}

/// Creates a channel charged to `account`: its two ends, or why not.
pub(crate) fn create_channel(account: &Arc<Account>) -> Result<(ChannelEnd, ChannelEnd), Error> {
    account.charge_channel()?;
    let refund = || account.refund_channel();
    let state = Channel::try_new(QUEUE_DEPTH, QUEUE_BYTES).inspect_err(|_| refund())?;
    let channel = KernelChannel {
        state: SpinLock::new(state),
        open: [AtomicU32::new(1), AtomicU32::new(1)],
        waiters: [WaitQueue::new(), WaitQueue::new()],
        undelivered: [
            SpinLock::new(VecDeque::new()),
            SpinLock::new(VecDeque::new()),
        ],
        holder: [SpinLock::new(None), SpinLock::new(None)],
        next_closed: SpinLock::new(None),
        listed: AtomicBool::new(false),
        account: account.clone(),
    };
    // The failure case hands `channel` back and its `Drop` does the refund.
    let channel = ObjectRef::try_new(channel).map_err(|_| Error::NoMemory)?;
    let first = ChannelEnd {
        channel: channel.clone(),
        side: 0,
    };
    let second = ChannelEnd { channel, side: 1 };
    Ok((first, second))
}

/// Which object `handle` names, if it is a channel and has `need`.
fn channel_of(
    table: &HandleTable<Object>,
    handle: Handle,
    need: u32,
) -> Result<(ObjectRef<KernelChannel>, usize), Error> {
    let (object, _) = table.get(handle, need)?;
    let Object::Channel(end) = object;
    Ok((end.channel.clone(), end.side))
}

/// Whether `target` is the end `(channel, side)` or holds it, through any number of queues:
/// the walk goes up from that end, along which queue holds it, towards the end held in a
/// table. Called under [`TRANSFERS`], so no link can be added meanwhile.
fn inside(target: &ChannelEnd, mut channel: ObjectRef<KernelChannel>, mut side: usize) -> bool {
    loop {
        if ObjectRef::same(&channel, &target.channel) && side == target.side {
            return true;
        }
        let Some(holder) = channel.holder.get(side) else {
            return false;
        };
        let link = holder.lock().clone();
        let Some((next, next_side)) = link else {
            return false;
        };
        channel = next;
        side = next_side;
    }
}

/// `channel_write`: queues `bytes` and moves the handles in `transfer` out of `table`, or
/// does nothing and says why.
pub(crate) fn channel_write(
    table: &SpinLock<HandleTable<Object>>,
    handle: Handle,
    bytes: Vec<u8>,
    transfer: &[Handle],
) -> Result<(), Error> {
    // Threads blocked on a handle this message takes away must wake and see it gone (P9),
    // and that happens after the locks are released.
    let mut moved_ends: Vec<EndId> = Vec::new();
    moved_ends
        .try_reserve_exact(transfer.len())
        .map_err(|_| Error::NoMemory)?;
    let wake_side = without_interrupts(|| {
        let mut table = table.lock();
        let (channel, side) = channel_of(&table, handle, rights::WRITE)?;
        let reader = other(side);
        table.check_all(transfer, rights::TRANSFER)?;
        // An end may be sent only if that keeps every end reachable (amendment A3). Every
        // listed handle was just checked to exist.
        let moves_an_end = !transfer.is_empty();
        let _order = moves_an_end.then(|| TRANSFERS.lock());
        for &value in transfer {
            let (object, _) = table.get(value, 0)?;
            let Object::Channel(end) = object;
            let references = end.channel.open.get(end.side);
            if ObjectRef::same(&end.channel, &channel)
                || references.is_none_or(|n| n.load(Ordering::Relaxed) != 1)
                || inside(end, channel.clone(), reader)
            {
                return Err(Error::InvalidArgs);
            }
        }
        let mut state = channel.state.lock();
        state.can_write(side, bytes.len())?;
        let moved = table.take_all(transfer, rights::TRANSFER)?;
        // The reader's queue holds them now, recorded before anyone can see the message.
        for (object, _) in &moved {
            let Object::Channel(end) = object;
            if let Some(holder) = end.channel.holder.get(end.side) {
                *holder.lock() = Some((channel.clone(), reader));
            }
            moved_ends.push((end.channel.clone(), end.side));
        }
        let message = Message {
            bytes,
            handles: moved,
        };
        // `can_write` and the handle count were checked just above, under this lock, and
        // the queue's storage was taken at creation, so this cannot be refused. Were that
        // ever wrong, the message goes back outside the locks, since dropping it may close
        // the handles it carried.
        let refused = state.write(side, message).err();
        drop(state);
        Ok::<_, Error>((channel, reader, refused))
    });
    let (channel, reader, refused) = wake_side?;
    if let Some((error, message)) = refused {
        drop(message);
        return Err(error);
    }
    wake(&channel, reader);
    for (end, side) in &moved_ends {
        wake(end, *side);
    }
    Ok(())
}

/// What a `channel_read` found.
pub(crate) enum Read {
    /// A message, now out of the queue, with the values of its handles in the reader's table.
    Message {
        /// The payload.
        bytes: Vec<u8>,
        /// The handles' new values.
        handles: Vec<Handle>,
    },
    /// The next message does not fit; it stays queued. Its sizes.
    TooSmall {
        /// Bytes of the message.
        bytes: usize,
        /// Handles with it.
        handles: usize,
    },
}

/// `channel_read`: the next message, its handles moved into `table`.
pub(crate) fn channel_read(
    table: &SpinLock<HandleTable<Object>>,
    handle: Handle,
    byte_capacity: usize,
    handle_capacity: usize,
) -> Result<Read, Error> {
    let read = without_interrupts(|| {
        let mut table = table.lock();
        let (channel, side) = channel_of(&table, handle, rights::READ)?;
        let mut state = channel.state.lock();
        let (bytes, handles) = state.peek(side)?;
        if bytes > byte_capacity || handles > handle_capacity {
            drop(state);
            return Ok((None, Read::TooSmall { bytes, handles }));
        }
        // Room for its handles first: if the table is full, or memory is out, the message
        // stays queued.
        table.reserve(handles)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(handles)
            .map_err(|_| Error::NoMemory)?;
        let message = state
            .read(side, byte_capacity, handle_capacity)
            .map_err(|e| match e {
                ReadError::Status(e) => e,
                ReadError::TooSmall { .. } => Error::BufferTooSmall,
            })?;
        drop(state);
        let mut incoming = message.handles.into_iter();
        for (object, entry_rights) in incoming.by_ref() {
            let Object::Channel(end) = &object;
            if let Some(holder) = end.channel.holder.get(end.side) {
                *holder.lock() = None;
            }
            match table.insert(object, entry_rights) {
                Ok(h) => values.push(h),
                // `reserve` above makes this unreachable. Were it reached, nothing may
                // close here, under the table lock: the objects are leaked instead.
                Err((e, object)) => {
                    core::mem::forget(object);
                    core::mem::forget(incoming);
                    return Err(e);
                }
            }
        }
        let read = Read::Message {
            bytes: message.bytes,
            handles: values,
        };
        Ok::<_, Error>((Some((channel, other(side))), read))
    })?;
    if let (Some((channel, writer)), _) = &read {
        wake(channel, *writer);
    }
    Ok(read.1)
}

/// Closes `handle`, so a thread waiting on it completes with `CANCELED`.
pub(crate) fn handle_close(
    table: &SpinLock<HandleTable<Object>>,
    handle: Handle,
) -> Result<(), Error> {
    let (object, _) = without_interrupts(|| table.lock().remove(handle, 0))?;
    // The handle is gone from the table: a waiter on it sees that when it is woken here. If
    // this was the end's last reference, the drop below closes the end and wakes its peer.
    let Object::Channel(end) = &object;
    wake(&end.channel, end.side);
    drop(object);
    Ok(())
}

/// `duplicate`: a new handle with `new_rights`, which may only remove rights (P2).
pub(crate) fn handle_duplicate(
    table: &SpinLock<HandleTable<Object>>,
    handle: Handle,
    new_rights: u32,
) -> Result<Handle, Error> {
    if new_rights & !rights::ALL != 0 {
        return Err(Error::InvalidArgs);
    }
    without_interrupts(|| table.lock().duplicate(handle, new_rights))
}

/// What ended a `wait_one`.
pub(crate) struct Waited {
    /// `Ok(())`, [`Error::TimedOut`] or [`Error::Canceled`].
    pub(crate) status: Result<(), Error>,
    /// The signals raised on the object at the end of the wait.
    pub(crate) observed: u32,
    /// How often the thread blocked.
    pub(crate) blocked: u64,
}

/// `wait_one`: blocks until a signal in `mask` is raised on `handle`'s object, the monotonic
/// clock reaches `deadline`, or `handle` is closed (P9).
pub(crate) fn wait_one(
    table: &SpinLock<HandleTable<Object>>,
    waiting: &AtomicU32,
    handle: Handle,
    mask: u32,
    deadline: u64,
) -> Result<Waited, Error> {
    let (channel, side) = without_interrupts(|| channel_of(&table.lock(), handle, rights::WAIT))?;
    let waiters = channel.waiters.get(side).ok_or(Error::BadHandle)?;
    waiting.fetch_add(1, Ordering::Release);
    let signals = || without_interrupts(|| channel.state.lock().signals(side));
    let still_open = || without_interrupts(|| table.lock().get(handle, 0).is_ok());
    let result = waiters.wait_until_deadline(|| signals() & mask != 0 || !still_open(), deadline);
    waiting.fetch_sub(1, Ordering::AcqRel);
    let observed = signals();
    let status = if !still_open() {
        Err(Error::Canceled)
    } else if result.is_err() {
        Err(Error::TimedOut)
    } else {
        Ok(())
    };
    let blocked = result.unwrap_or(0);
    drop(channel);
    Ok(Waited {
        status,
        observed,
        blocked,
    })
}
