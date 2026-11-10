//! Kernel objects and channels (ADR-0005 P1, P2, P3, P8, P9): the objects handles name, the
//! moves between handle tables and queues, and blocking until a signal on any of them.
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
//! - A queued message is charged to the process that sent it, by the kernel heap it holds
//!   ([`message_cost`], ADR-0005 P11), until it is read or the end it was queued for closes;
//!   a sender over its `Limits::message_bytes` gets `LIMIT` and the peer is unaffected.
//!
//! Lock order: a handle table, then [`TRANSFERS`], then a channel's state, then its charge
//! list, then an end's holder link. A wait queue's lock is taken before whatever a wait's
//! condition reads (a table, a channel's state); a waker holds none of these locks while it
//! wakes.
#![forbid(unsafe_code)]

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::channel::{Channel, Identity, Message, QUEUE_BYTES, QUEUE_DEPTH, ReadError, other};
use crate::handles::{HandleTable, Need, Typed};
use crate::heap::{MAX_BLOCK, MIN_BLOCK, PAGE};
use origin_abi::{Error, Handle, ObjectType};
use origin_frame::objref::ObjectRef;
use origin_frame::sync::SpinLock;

use crate::cpu::without_interrupts;
use crate::memory_object::MemoryObject;
use crate::process::{Account, Process, ThreadObject};
use crate::wait::WaitQueue;

/// What a handle names (ADR-0005 P3; channels, processes and threads so far).
#[derive(Clone)]
pub(crate) enum Object {
    /// One end of a channel.
    Channel(ChannelEnd),
    /// A process.
    Process(ObjectRef<Process>),
    /// A thread.
    Thread(ObjectRef<ThreadObject>),
    /// A memory object (C7).
    Memory(ObjectRef<MemoryObject>),
}

impl Typed for Object {
    fn object_type(&self) -> u32 {
        (match self {
            Self::Channel(_) => ObjectType::Channel,
            Self::Process(_) => ObjectType::Process,
            Self::Thread(_) => ObjectType::Thread,
            Self::Memory(_) => ObjectType::MemoryObject,
        }) as u32
    }
}

/// What a wait, or the wake-up of one, needs of an object: counted references that keep its
/// signals and its waiters alive, and are not one more handle to it (an end's reference count
/// is what closes it).
enum Waitable {
    /// A channel and which of its two sides.
    Channel(ObjectRef<KernelChannel>, usize),
    /// A process.
    Process(ObjectRef<Process>),
    /// A thread.
    Thread(ObjectRef<ThreadObject>),
    /// An object with no signals and no waiters: a memory object. No handle to one has `wait`.
    Silent,
}

impl Waitable {
    fn of(object: &Object) -> Self {
        match object {
            Object::Channel(end) => Self::Channel(end.channel.clone(), end.side),
            Object::Process(process) => Self::Process(process.clone()),
            Object::Thread(thread) => Self::Thread(thread.clone()),
            Object::Memory(_) => Self::Silent,
        }
    }

    /// The signals the object has raised.
    fn signals(&self) -> u32 {
        match self {
            Self::Channel(channel, side) => {
                without_interrupts(|| channel.state.lock().signals(*side))
            }
            Self::Process(process) => process.termination().signals(),
            Self::Thread(thread) => thread.termination().signals(),
            Self::Silent => 0,
        }
    }

    /// The threads waiting on it.
    fn waiters(&self) -> Option<&WaitQueue> {
        match self {
            Self::Channel(channel, side) => channel.waiters.get(*side),
            Self::Process(process) => Some(process.termination().waiters()),
            Self::Thread(thread) => Some(thread.termination().waiters()),
            Self::Silent => None,
        }
    }

    /// Wakes everything waiting on it.
    fn wake(&self) {
        if let Some(waiters) = self.waiters() {
            note_cross_cpu_wakes(waiters.wake_all());
        }
    }
}

/// A handle in a message: its object and its rights, preserved in transit (P2).
pub(crate) type Entry = (Object, u32);

/// What a queued message of `bytes` bytes and `handles` handles costs the account that sent
/// it: the heap blocks they occupy, rounded as the heap rounds them (`framekernel::heap`).
pub(crate) fn message_cost(bytes: usize, handles: usize) -> usize {
    heap_block(bytes) + heap_block(handles * core::mem::size_of::<Entry>())
}

/// What the kernel heap sets aside for an allocation of `bytes` (see `framekernel::heap`).
fn heap_block(bytes: usize) -> usize {
    let (min, max, page) = (MIN_BLOCK as usize, MAX_BLOCK as usize, PAGE as usize);
    match bytes {
        0 => 0,
        _ if bytes <= max => bytes.max(min).next_power_of_two(),
        _ => bytes.next_multiple_of(page),
    }
}

/// What a queued message cost its sender. Its drop is the refund, when the message leaves the
/// queue: read, or left behind when the end it was queued for closed.
struct Charge {
    account: ObjectRef<Account>,
    bytes: usize,
}

impl Drop for Charge {
    fn drop(&mut self) {
        self.account.refund_queued(self.bytes);
    }
}

/// An empty list of charges with room for a whole queue, so writing never allocates.
fn charge_list() -> Result<VecDeque<Charge>, Error> {
    let mut list = VecDeque::new();
    list.try_reserve_exact(QUEUE_DEPTH)
        .map_err(|_| Error::NoMemory)?;
    Ok(list)
}

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
    /// Who was charged for each message queued toward each side, in queue order, kept in
    /// step with the queues: locked only under `state`.
    charges: SpinLock<[VecDeque<Charge>; 2]>,
    /// Messages left when an end closed, until [`reap_closed`] drops them.
    undelivered: [SpinLock<VecDeque<Message<Entry>>>; 2],
    /// Where each end is in flight: the channel and side whose queue holds it.
    holder: [SpinLock<Option<EndId>>; 2],
    /// The next channel on the list [`reap_closed`] drains.
    next_closed: SpinLock<Option<ObjectRef<KernelChannel>>>,
    /// Whether this channel is on that list.
    listed: AtomicBool,
    /// Its creator's account, charged once for this channel's lifetime.
    account: ObjectRef<Account>,
}

impl Drop for KernelChannel {
    fn drop(&mut self) {
        self.account.refund_channel();
    }
}

/// Serializes every addition of a channel end to the graph of ends held in queues, so the
/// cycle check always sees the whole graph. The walk under it stops at [`MAX_HOLDER_DEPTH`]
/// links (the quotas of however many processes built the chain would allow far more).
static TRANSFERS: SpinLock<()> = SpinLock::new(());

/// The most links the transfer check follows up the chain of queues holding an end (ADR-0005
/// A3): this bounds the walk under the global lock with interrupts off, whatever the depth of
/// nesting other processes' quotas would allow. A transfer into an end nested deeper gets
/// [`Error::Limit`].
pub(crate) const MAX_HOLDER_DEPTH: usize = 16;

/// Channels with undelivered messages waiting for [`reap_closed`].
static CLOSED: SpinLock<Option<ObjectRef<KernelChannel>>> = SpinLock::new(None);
/// How many channels are on that list: the common case is none, checked without the lock.
static CLOSED_COUNT: AtomicU64 = AtomicU64::new(0);

/// Wake-ups of threads blocked on another processor, for the boot proof.
static CROSS_CPU_WAKES: AtomicU64 = AtomicU64::new(0);

/// How many blocked threads an operation woke on another processor (by IPI).
pub(crate) fn cross_cpu_wakes() -> u64 {
    CROSS_CPU_WAKES.load(Ordering::Relaxed)
}

/// Counts `woken` wake-ups that crossed processors, for the boot proof.
pub(crate) fn note_cross_cpu_wakes(woken: usize) {
    CROSS_CPU_WAKES.fetch_add(woken as u64, Ordering::Relaxed);
}

/// Wakes everything waiting on `side`.
fn wake(channel: &KernelChannel, side: usize) {
    if let Some(waiters) = channel.waiters.get(side) {
        note_cross_cpu_wakes(waiters.wake_all());
    }
}

/// `side` lost its last reference: it closes. Its waiters and the peer's are woken, and
/// its undelivered messages go on the list [`reap_closed`] drains.
fn close_side(channel: &ObjectRef<KernelChannel>, side: usize) {
    let (undelivered, charges) = without_interrupts(|| {
        let mut state = channel.state.lock();
        let undelivered = state.close(side);
        // Nothing can be queued toward a closed side any more: every charge for this side
        // goes with its message, refunded just below, outside the locks.
        let charges = channel
            .charges
            .lock()
            .get_mut(side)
            .map(core::mem::take)
            .unwrap_or_default();
        (undelivered, charges)
    });
    drop(charges);
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
pub(crate) fn create_channel(
    account: &ObjectRef<Account>,
) -> Result<(ChannelEnd, ChannelEnd), Error> {
    account.charge_channel()?;
    let refund = || account.refund_channel();
    let state = Channel::try_new(QUEUE_DEPTH, QUEUE_BYTES).inspect_err(|_| refund())?;
    let charges = charge_list()
        .and_then(|one| Ok([one, charge_list()?]))
        .inspect_err(|_| refund())?;
    let channel = KernelChannel {
        state: SpinLock::new(state),
        open: [AtomicU32::new(1), AtomicU32::new(1)],
        waiters: [WaitQueue::new(), WaitQueue::new()],
        charges: SpinLock::new(charges),
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

/// The channel and side `handle` names, if it meets `need` and names a channel. Every call
/// that reaches here asks for a channel, so a handle to anything else already failed the lookup;
/// this answer is for a definition that stopped asking.
fn channel_of(
    table: &HandleTable<Object>,
    handle: Handle,
    need: Need,
) -> Result<(ObjectRef<KernelChannel>, usize), Error> {
    let (object, _) = table.get(handle, need)?;
    match object {
        Object::Channel(end) => Ok((end.channel.clone(), end.side)),
        Object::Process(_) | Object::Thread(_) | Object::Memory(_) => Err(Error::WrongType),
    }
}

/// Whether `target` is the end `(channel, side)` or holds it, through up to
/// [`MAX_HOLDER_DEPTH`] queues: the walk goes up from that end, along which queue holds it,
/// towards the end held in a table, and a chain any longer than that is [`Error::Limit`].
/// Called under [`TRANSFERS`], so no link can be added meanwhile.
fn inside(
    target: &ChannelEnd,
    mut channel: ObjectRef<KernelChannel>,
    mut side: usize,
) -> Result<bool, Error> {
    for _ in 0..=MAX_HOLDER_DEPTH {
        if ObjectRef::same(&channel, &target.channel) && side == target.side {
            return Ok(true);
        }
        let Some(holder) = channel.holder.get(side) else {
            return Ok(false);
        };
        let link = holder.lock().clone();
        let Some((next, next_side)) = link else {
            return Ok(false);
        };
        channel = next;
        side = next_side;
    }
    Err(Error::Limit)
}

/// `channel_write`: queues `bytes` and moves the handles in `transfer` out of `table`, or
/// does nothing and says why. `need` is what the end must meet, and `transfer_need` what each
/// listed handle must. The message carries `sender`, the writing process's identity, which
/// the caller takes from that process and never from what the writer passed (ADR-0005 B1).
#[expect(
    clippy::too_many_arguments,
    reason = "each is one fact the write needs"
)]
pub(crate) fn channel_write(
    table: &SpinLock<HandleTable<Object>>,
    account: &ObjectRef<Account>,
    sender: Identity,
    handle: Handle,
    need: Need,
    bytes: Vec<u8>,
    transfer: &[Handle],
    transfer_need: Need,
) -> Result<(), Error> {
    // Threads blocked on a handle this message takes away must wake and see it gone (P9),
    // and that happens after the locks are released.
    let mut moved_away: Vec<Waitable> = Vec::new();
    moved_away
        .try_reserve_exact(transfer.len())
        .map_err(|_| Error::NoMemory)?;
    let wake_side = without_interrupts(|| {
        let mut table = table.lock();
        let (channel, side) = channel_of(&table, handle, need)?;
        let reader = other(side);
        table.check_all(transfer, transfer_need)?;
        // An end may be sent only if that keeps every end reachable (amendment A3). Every
        // listed handle was just checked to exist. Only an end can be held in a queue and so
        // be part of a cycle of queues: other objects are just carried.
        let moves_an_end = !transfer.is_empty();
        let _order = moves_an_end.then(|| TRANSFERS.lock());
        for &value in transfer {
            let (object, _) = table.get(value, Need::NOTHING)?;
            let Object::Channel(end) = object else {
                continue;
            };
            let references = end.channel.open.get(end.side);
            if ObjectRef::same(&end.channel, &channel)
                || references.is_none_or(|n| n.load(Ordering::Relaxed) != 1)
                || inside(end, channel.clone(), reader)?
            {
                return Err(Error::InvalidArgs);
            }
        }
        let mut state = channel.state.lock();
        state.can_write(side, bytes.len())?;
        // The sender pays, before anything is consumed: a refusal changes nothing.
        let cost = message_cost(bytes.len(), transfer.len());
        account.charge_queued(cost)?;
        let charge = Charge {
            account: account.clone(),
            bytes: cost,
        };
        let moved = table.take_all(transfer, transfer_need)?;
        // The reader's queue holds them now, recorded before anyone can see the message.
        for (object, _) in &moved {
            if let Object::Channel(end) = object
                && let Some(holder) = end.channel.holder.get(end.side)
            {
                *holder.lock() = Some((channel.clone(), reader));
            }
            moved_away.push(Waitable::of(object));
        }
        let message = Message {
            bytes,
            handles: moved,
            sender,
        };
        // Beside the message, in step with the queue; its storage was taken at creation.
        if let Some(list) = channel.charges.lock().get_mut(reader) {
            list.push_back(charge);
        }
        // `can_write` and the handle count were checked just above, under this lock, and
        // the queue's storage was taken at creation, so this cannot be refused. Were that
        // ever wrong, the charge and the message go back, the message outside the locks,
        // since dropping it may close the handles it carried.
        let refused = state.write(side, message).err();
        if refused.is_some()
            && let Some(list) = channel.charges.lock().get_mut(reader)
        {
            drop(list.pop_back());
        }
        drop(state);
        Ok::<_, Error>((channel, reader, refused))
    });
    let (channel, reader, refused) = wake_side?;
    if let Some((error, message)) = refused {
        drop(message);
        return Err(error);
    }
    wake(&channel, reader);
    for target in &moved_away {
        target.wake();
    }
    Ok(())
}

/// A handle `channel_read` put in the reader's table, with what the table holds for it: the
/// record the reader receives (ADR-0005 B3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Received {
    /// Its value in the reader's table.
    pub(crate) handle: Handle,
    /// The type of the object it names.
    pub(crate) object_type: u32,
    /// Its rights there: the ones it had in the message.
    pub(crate) rights: u32,
}

/// What a `channel_read` found.
pub(crate) enum Read {
    /// A message, now out of the queue, with its handles in the reader's table.
    Message {
        /// The payload.
        bytes: Vec<u8>,
        /// The handles, as the reader's table now holds them.
        handles: Vec<Received>,
        /// Who wrote it (B1).
        sender: Identity,
    },
    /// The next message does not fit; it stays queued. Its sizes and its writer.
    TooSmall {
        /// Bytes of the message.
        bytes: usize,
        /// Handles with it.
        handles: usize,
        /// Who wrote it.
        sender: Identity,
    },
}

/// `channel_read`: the next message, its handles moved into `table`. `need` is what the end
/// must meet.
pub(crate) fn channel_read(
    table: &SpinLock<HandleTable<Object>>,
    handle: Handle,
    need: Need,
    byte_capacity: usize,
    handle_capacity: usize,
) -> Result<Read, Error> {
    let read = without_interrupts(|| {
        let mut table = table.lock();
        let (channel, side) = channel_of(&table, handle, need)?;
        let mut state = channel.state.lock();
        let next = state.peek(side)?;
        let (bytes, handles, sender) = (next.bytes.len(), next.handles.len(), next.sender);
        if bytes > byte_capacity || handles > handle_capacity {
            drop(state);
            let too_small = Read::TooSmall {
                bytes,
                handles,
                sender,
            };
            return Ok((None, too_small, None));
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
        // Off the queue: this message's charge goes with it, refunded outside the locks.
        let charge = channel
            .charges
            .lock()
            .get_mut(side)
            .and_then(VecDeque::pop_front);
        drop(state);
        let mut incoming = message.handles.into_iter();
        for (object, entry_rights) in incoming.by_ref() {
            if let Object::Channel(end) = &object
                && let Some(holder) = end.channel.holder.get(end.side)
            {
                *holder.lock() = None;
            }
            let object_type = object.object_type();
            match table.insert(object, entry_rights) {
                Ok(handle) => values.push(Received {
                    handle,
                    object_type,
                    rights: entry_rights,
                }),
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
            sender: message.sender,
        };
        Ok::<_, Error>((Some((channel, other(side))), read, charge))
    })?;
    let (woken, result, charge) = read;
    drop(charge);
    if let Some((channel, writer)) = &woken {
        wake(channel, *writer);
    }
    Ok(result)
}

/// Closes `handle`, so a thread waiting on it completes with `CANCELED`.
pub(crate) fn handle_close(
    table: &SpinLock<HandleTable<Object>>,
    handle: Handle,
    need: Need,
) -> Result<(), Error> {
    let (object, _) = without_interrupts(|| table.lock().remove(handle, need))?;
    // The handle is gone from the table: a waiter on it sees that when it is woken here. If
    // this was an end's last reference, the drop below closes the end and wakes its peer.
    Waitable::of(&object).wake();
    drop(object);
    Ok(())
}

/// `duplicate`: a new handle with `new_rights`, which may only remove rights (P2). The decoder
/// has refused a right the definition does not declare.
pub(crate) fn handle_duplicate(
    table: &SpinLock<HandleTable<Object>>,
    handle: Handle,
    need: Need,
    new_rights: u32,
) -> Result<Handle, Error> {
    without_interrupts(|| table.lock().duplicate(handle, need, new_rights))
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

/// `wait_one`: blocks until a signal in `mask` is raised on `handle`'s object, of any type, the
/// monotonic clock reaches `deadline`, or `handle` is closed (P9). `need` is what the handle
/// must meet.
pub(crate) fn wait_one(
    table: &SpinLock<HandleTable<Object>>,
    waiting: &AtomicU32,
    handle: Handle,
    need: Need,
    mask: u32,
    deadline: u64,
) -> Result<Waited, Error> {
    let target = without_interrupts(|| {
        table
            .lock()
            .get(handle, need)
            .map(|(object, _)| Waitable::of(object))
    })?;
    let waiters = target.waiters().ok_or(Error::BadHandle)?;
    waiting.fetch_add(1, Ordering::Release);
    let still_open = || without_interrupts(|| table.lock().get(handle, Need::NOTHING).is_ok());
    let result =
        waiters.wait_until_deadline(|| target.signals() & mask != 0 || !still_open(), deadline);
    waiting.fetch_sub(1, Ordering::AcqRel);
    let observed = target.signals();
    let status = if !still_open() {
        Err(Error::Canceled)
    } else if result.is_err() {
        Err(Error::TimedOut)
    } else {
        Ok(())
    };
    let blocked = result.unwrap_or(0);
    drop(target);
    Ok(Waited {
        status,
        observed,
        blocked,
    })
}
