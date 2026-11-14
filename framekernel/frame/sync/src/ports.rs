//! Ports (ADR-0005 C9 and P9, task S3-6a): binding an object's signals to a port, raising them,
//! cancelling a binding and closing a port. This is the protocol alone, generic over the
//! scheduler and over what the kernel supplies ([`Kernel`]), so that the host tests and the Loom
//! models (`models.rs`) run the code the kernel runs. The kernel instantiates it for its events,
//! ports and the objects that raise signals, as `framekernel/src/wait.rs` instantiates
//! [`WaitQueue`].
//!
//! # The pieces
//!
//! - A [`Source`] is one thing that raises signals: a side of a channel, a process's or a thread's
//!   termination, an event. It holds the bindings armed on it. Its first bind gives it a number
//!   from a counter here that never repeats, and nothing here gives that number to anyone.
//! - A [`Port`] holds the packets queued in it, oldest first, the threads waiting for one, whether
//!   it has closed, and its *index*: one slot for each binding armed for it, holding that
//!   binding's way back to its source.
//! - A [`Reservation`] is what one binding needs, made by [`Reservation::try_new`] before any lock
//!   is taken, with a payload the kernel sets: the binder's account. **Nothing allocates after
//!   it.** Every operation below only moves what it made, and frees it.
//!
//! # What a reservation is made of
//!
//! - The **storage**, one allocation. It carries the payload, what the binding asked for, and the
//!   packet it becomes. While the binding is armed the source keeps it, with a counted reference
//!   to the port (`K::PortRef`): so a delivery walks only the bindings armed on its own object,
//!   and has each one's storage in hand. Once queued, the port keeps it.
//! - A **ticket**, one small allocation inside the storage. When a binding fires or is cancelled
//!   implicitly, its references leave the storage in the ticket. The ticket wakes the port and is
//!   freed after every lock is released. So no counted reference is dropped, and no thread is
//!   woken, under a lock, and a queued packet holds no counted reference at all.
//! - **Room** in the port's index: [`Reservation::try_new`] grows the index, fallibly, until it
//!   has a free slot for every reservation made for the port and not yet bound. A bind takes
//!   one without allocating. An armed binding's slot holds its way back to its source
//!   (`K::SourceRef`), so that a close finds every source with a binding armed for the port, and
//!   searches no other object. A binding gives its slot back, in constant time, when it fires or
//!   is cancelled.
//!
//! What each operation does:
//!
//! - [`Source::bind`] puts the storage on the source and its way back in a slot of the port's
//!   index. If the signals already meet the mask, the storage goes straight into the port's queue
//!   instead, and the slot is given back.
//! - [`Source::deliver`] and [`Source::cancel_owners`] take storage off the source and put it in
//!   the port's queue, as a [`Kind::Signal`] or a [`Kind::Canceled`] packet, and give its slot
//!   back.
//! - [`Source::cancel`] takes the matching storage off the source and out of the port's queue, and
//!   hands it back.
//! - [`Port::take`] hands back one packet and its storage.
//! - [`Port::close`] takes the queue and the index from the port. Then it takes the storage of
//!   every binding armed for the port off each source the index names, once each.
//!
//! A storage is one allocation moved by value, so it is in one place at a time. So one binding
//! makes at most one packet, of one kind, and its storage comes back once.
//!
//! # Lock order
//!
//! - A source's lock is taken where a wait queue's lock is: before whatever the closures read (a
//!   handle table, a channel's state).
//! - A port's lock comes after a source's, never before it. It is a leaf: nothing is taken under
//!   it, and no closure runs under it.
//! - The lock of a port's waiters (a [`WaitQueue`]) comes before the port's lock: a waiting
//!   thread's condition reads the queue.
//! - No thread is woken under a source's or a port's lock. Whoever queued a packet wakes the
//!   port's waiters after releasing both.
//! - A close holds the port's lock and a source's lock at different times, never together.
//!
//! Every lock is held with interrupts disabled ([`Scheduler::without_interrupts`]).
//!
//! # What the kernel must do
//!
//! - Change an object's state first, then call [`Source::deliver`], holding none of its own locks,
//!   so that no delivery is lost.
//! - Make every call outside its own locks. Storage, references and payloads come back to the
//!   caller, and dropping a reference may run the destructor of the object it names.
//! - Make a reservation for the port the binding goes to, after resolving the port's handle.
//! - Call [`Port::wake`] when it closes or moves a port handle a thread may be waiting through
//!   (ADR-0005 Amendment 7, F3).
//!
//! # Cost
//!
//! - A delivery walks the bindings armed on its source, under the source's lock. Each binding
//!   that fires costs a constant more, under its port's lock.
//! - An explicit cancel walks the bindings armed on its source and the port's queue.
//! - A close sorts the port's index and walks the bindings armed on each source it names.
//! - A reservation that finds the index full grows it to twice its size, and moves its slots
//!   under the port's lock. The index never shrinks while the port is open: its size is the most
//!   bindings ever armed for the port, or reserved for it, at once.
//!
//! None of these has a bound of its own here: the binders' quotas bound them.
#![forbid(unsafe_code)]

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ops::{Deref, DerefMut};
use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::wait::{Scheduler, WaitQueue};
use crate::{SpinLock, prim};

/// Threads one port holds waiting for a packet, as every kernel wait queue does
/// (`framekernel/src/wait.rs`). A thread that finds the port's queue of waiters full polls by
/// yielding instead.
pub const WAITERS: usize = 16;

/// The fewest slots a port's index grows to.
const FIRST_SLOTS: usize = 4;

/// What the kernel supplies.
pub trait Kernel: Sized {
    /// The scheduler: for threads waiting for a packet, and for holding locks with interrupts
    /// disabled.
    type Scheduler: Scheduler;
    /// What a binding's storage carries for the kernel: the binder's account, so that whoever
    /// gets the storage back knows whom to refund.
    type Payload;
    /// Who made a binding. The kernel uses a process's ID and the value of the handle the
    /// binding was made through.
    type Owner: Copy + Eq;
    /// A counted reference to a port's memory. It is not one of the port's handles: an armed
    /// binding holds one, and the port still closes when its last handle goes.
    type PortRef: Deref<Target = Port<Self>> + Clone;
    /// A counted reference to what holds a source: the way back from a port to a source with a
    /// binding armed for it.
    type SourceRef: Deref<Target = Source<Self>> + Clone;
}

/// The name of a thread waiting on a port.
type Thread<K> = <<K as Kernel>::Scheduler as Scheduler>::Thread;

/// Hands out sources' numbers. It is a plain atomic even under Loom: it only gives each source a
/// number of its own, and nothing is ordered by it. At one number a nanosecond it would take 584
/// years to wrap.
static NUMBERS: AtomicU64 = AtomicU64::new(1);

/// What made a packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The object's signals met the binding's mask.
    Signal,
    /// The binding was cancelled implicitly: the handle it was made through closed or moved.
    Canceled,
}

/// One packet: what the binder gave, what made the packet, and the object's signals then. It
/// holds no reference and no address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packet {
    /// The binder's key, unchanged.
    pub key: u64,
    /// What made the packet.
    pub kind: Kind,
    /// The mask the binding asked for.
    pub trigger: u32,
    /// All of the object's signals when the packet was queued. For a [`Kind::Canceled`] packet,
    /// when the binding was cancelled.
    pub observed: u32,
}

/// What a binder asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request<O> {
    /// Who makes the binding: an implicit cancel finds it by this.
    pub owner: O,
    /// Given back unchanged in the packet. Compared by an explicit cancel, never interpreted.
    pub key: u64,
    /// The signals that fire the binding: any one of them.
    pub mask: u32,
}

/// Why [`Reservation::try_new`] made no reservation. Nothing was taken, and its payload was
/// dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unreserved {
    /// The kernel heap had no room for what a binding needs.
    NoMemory,
    /// The port already has as many bindings armed and reservations in flight as its bound
    /// allows ([`Port::bounded`]; ADR-0005 Amendment 10, I1).
    Full,
}

/// Why a bind was refused. Nothing was bound, and the reservation is handed back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The handle the binding was to be made through had closed (`still_open` said so).
    HandleClosed,
    /// The port had closed.
    PortClosed,
    /// The port's index had no free slot. Never, while each reservation keeps the room it was
    /// made with: a bind reports it rather than allocate.
    NoRoom,
}

/// Where a bind left its binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bound {
    /// Armed on the source, waiting for its signals.
    Armed,
    /// Already queued: the signals met the mask when it was made.
    Queued,
}

/// Why [`Port::take`] returned no packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TakeError {
    /// The deadline passed with nothing queued.
    TimedOut,
    /// The handle waited through closed (`still_open` said so), or the port closed.
    Canceled,
}

/// One value in an allocation of its own, made fallibly: `Box::try_new` is unstable on Rust
/// 1.100.0. A one-element array is the shape a `Vec` converts to without allocating again.
struct Node<T>(Box<[T; 1]>);

impl<T> Node<T> {
    /// `value` in an allocation of its own, or `None` (and `value` dropped) if the heap has no
    /// room.
    fn try_new(value: T) -> Option<Self> {
        let mut cell = Vec::new();
        // Room for exactly one value. With more, the conversion below would allocate again,
        // and that allocation could not fail softly.
        if cell.try_reserve_exact(1).is_err() || cell.capacity() != 1 {
            return None;
        }
        cell.push(value);
        Box::<[T; 1]>::try_from(cell).ok().map(Self)
    }

    /// The value, out of its allocation.
    fn into_inner(self) -> T {
        let [value] = *self.0;
        value
    }
}

impl<T> Deref for Node<T> {
    type Target = T;

    fn deref(&self) -> &T {
        let [value] = &*self.0;
        value
    }
}

impl<T> DerefMut for Node<T> {
    fn deref_mut(&mut self) -> &mut T {
        let [value] = &mut *self.0;
        value
    }
}

/// A value that can be kept on a [`Stack`]: it holds the link to the next one.
trait Linked: Sized {
    fn next(&self) -> &Option<Node<Self>>;
    fn next_mut(&mut self) -> &mut Option<Node<Self>>;
}

/// A list of nodes linked through themselves. Pushing and popping at the top move a node and
/// allocate nothing. Dropped one node at a time, so a long list cannot overflow the stack.
struct Stack<T: Linked> {
    top: Option<Node<T>>,
    len: usize,
}

impl<T: Linked> Stack<T> {
    const fn new() -> Self {
        Self { top: None, len: 0 }
    }

    const fn len(&self) -> usize {
        self.len
    }

    const fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn push(&mut self, mut node: Node<T>) {
        *node.next_mut() = self.top.take();
        self.top = Some(node);
        self.len = self.len.saturating_add(1);
    }

    fn pop(&mut self) -> Option<Node<T>> {
        let mut node = self.top.take()?;
        self.top = node.next_mut().take();
        self.len = self.len.saturating_sub(1);
        Some(node)
    }

    /// Moves every node `select` picks onto `into`, and keeps the others in their order.
    /// Returns how many it moved.
    fn remove_where(&mut self, mut select: impl FnMut(&T) -> bool, into: &mut Self) -> usize {
        let mut kept = Self::new();
        let mut moved = 0usize;
        while let Some(node) = self.pop() {
            if select(&node) {
                into.push(node);
                moved = moved.saturating_add(1);
            } else {
                kept.push(node);
            }
        }
        while let Some(node) = kept.pop() {
            self.push(node);
        }
        moved
    }

    fn for_each(&self, mut visit: impl FnMut(&T)) {
        let mut cursor = self.top.as_ref();
        while let Some(node) = cursor {
            visit(node);
            cursor = node.next().as_ref();
        }
    }
}

impl<T: Linked> Drop for Stack<T> {
    fn drop(&mut self) {
        while self.pop().is_some() {}
    }
}

/// Nodes in the order they were pushed: two stacks, the older one popped from, refilled from the
/// newer when it runs out. Nothing allocates.
struct Queue<T: Linked> {
    /// The oldest node on top.
    older: Stack<T>,
    /// The newest node on top.
    newer: Stack<T>,
}

impl<T: Linked> Queue<T> {
    const fn new() -> Self {
        Self {
            older: Stack::new(),
            newer: Stack::new(),
        }
    }

    const fn len(&self) -> usize {
        self.older.len().saturating_add(self.newer.len())
    }

    fn push(&mut self, node: Node<T>) {
        self.newer.push(node);
    }

    fn pop(&mut self) -> Option<Node<T>> {
        if self.older.is_empty() {
            while let Some(node) = self.newer.pop() {
                self.older.push(node);
            }
        }
        self.older.pop()
    }

    /// Moves every node `select` picks onto `into`, and keeps the others in their order.
    fn remove_where(&mut self, mut select: impl FnMut(&T) -> bool, into: &mut Stack<T>) -> usize {
        let older = self.older.remove_where(&mut select, into);
        older.saturating_add(self.newer.remove_where(select, into))
    }

    /// Moves every node onto `into`.
    fn drain_into(&mut self, into: &mut Stack<T>) {
        while let Some(node) = self.pop() {
            into.push(node);
        }
    }
}

/// A binding's storage: the payload, what the binding asked for, and the packet it becomes.
struct Store<K: Kernel> {
    payload: K::Payload,
    /// The port, while the binding is armed on its source.
    port: Option<K::PortRef>,
    /// The binding's slot in its port's index, while it is armed.
    slot: usize,
    /// Set by the bind.
    owner: Option<K::Owner>,
    /// The source's number, set by the bind.
    source: u64,
    key: u64,
    trigger: u32,
    kind: Kind,
    observed: u32,
    /// What carries the binding's references out when it fires or is cancelled implicitly.
    ticket: Option<Node<Ticket<K>>>,
    next: Option<Node<Store<K>>>,
}

impl<K: Kernel> Store<K> {
    const fn new(payload: K::Payload) -> Self {
        Self {
            payload,
            port: None,
            slot: 0,
            owner: None,
            source: 0,
            key: 0,
            trigger: 0,
            kind: Kind::Signal,
            observed: 0,
            ticket: None,
            next: None,
        }
    }

    const fn packet(&self) -> Packet {
        Packet {
            key: self.key,
            kind: self.kind,
            trigger: self.trigger,
            observed: self.observed,
        }
    }

    /// Whether this binding is armed for `port`.
    fn aims_at(&self, port: &Port<K>) -> bool {
        self.port.as_deref().is_some_and(|own| ptr::eq(own, port))
    }
}

impl<K: Kernel> Linked for Store<K> {
    fn next(&self) -> &Option<Node<Self>> {
        &self.next
    }

    fn next_mut(&mut self) -> &mut Option<Node<Self>> {
        &mut self.next
    }
}

/// A binding's references, out of the locks: the port to wake, then both to drop.
struct Ticket<K: Kernel> {
    port: Option<K::PortRef>,
    way_back: Option<K::SourceRef>,
    next: Option<Node<Ticket<K>>>,
}

impl<K: Kernel> Linked for Ticket<K> {
    fn next(&self) -> &Option<Node<Self>> {
        &self.next
    }

    fn next_mut(&mut self) -> &mut Option<Node<Self>> {
        &mut self.next
    }
}

/// Wakes the waiters of each port a ticket names: after every lock is released.
fn wake_ports<K: Kernel>(scheduler: &K::Scheduler, tickets: &Stack<Ticket<K>>) {
    tickets.for_each(|ticket| {
        if let Some(port) = &ticket.port {
            port.wake(scheduler);
        }
    });
}

/// One slot of a port's index.
enum Slot<K: Kernel> {
    /// Free: the next free slot.
    Free(Option<usize>),
    /// An armed binding's: its source's number, and the way back to it.
    Used { source: u64, way_back: K::SourceRef },
}

impl<K: Kernel> Slot<K> {
    /// The number of the source it names; 0 for a free slot, which no source has.
    const fn source(&self) -> u64 {
        match self {
            Self::Free(_) => 0,
            Self::Used { source, .. } => *source,
        }
    }
}

/// A port's index: a slot for each binding armed for the port, and room for each reservation.
struct Index<K: Kernel> {
    slots: Vec<Slot<K>>,
    /// The first free slot; the others are linked through it.
    free: Option<usize>,
    /// How many slots are on the free list.
    freed: usize,
}

impl<K: Kernel> Index<K> {
    const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: None,
            freed: 0,
        }
    }

    /// Slots that hold a binding armed for the port.
    fn used(&self) -> usize {
        self.slots.len().saturating_sub(self.freed)
    }

    /// Slots a bind could take without allocating: free ones, and room never used.
    fn room(&self) -> usize {
        self.slots
            .capacity()
            .saturating_sub(self.slots.len())
            .saturating_add(self.freed)
    }

    /// A slot for a binding armed on source `source`, holding `way_back`. If there is none
    /// without allocating, it hands `way_back` back.
    fn take(&mut self, source: u64, way_back: K::SourceRef) -> Result<usize, K::SourceRef> {
        let used = Slot::Used { source, way_back };
        if let Some(at) = self.free
            && let Some(slot) = self.slots.get_mut(at)
            && let Slot::Free(next) = *slot
        {
            *slot = used;
            self.free = next;
            self.freed = self.freed.saturating_sub(1);
            return Ok(at);
        }
        if self.slots.len() < self.slots.capacity() {
            let at = self.slots.len();
            self.slots.push(used);
            return Ok(at);
        }
        match used {
            Slot::Used { way_back, .. } => Err(way_back),
            Slot::Free(_) => Ok(0),
        }
    }

    /// Frees slot `at` and hands back the way back it held.
    fn give_back(&mut self, at: usize) -> Option<K::SourceRef> {
        let slot = self.slots.get_mut(at)?;
        if let Slot::Free(_) = slot {
            return None;
        }
        let used = core::mem::replace(slot, Slot::Free(self.free));
        self.free = Some(at);
        self.freed = self.freed.saturating_add(1);
        match used {
            Slot::Used { way_back, .. } => Some(way_back),
            Slot::Free(_) => None,
        }
    }

    /// The capacity to grow to so that there is room for `reserved` reservations and one more.
    fn wanted(&self, reserved: usize) -> usize {
        self.slots
            .len()
            .saturating_add(reserved)
            .saturating_add(1)
            .max(self.slots.capacity().saturating_mul(2))
            .max(FIRST_SLOTS)
    }

    /// Moves every slot into `bigger`, which has room for them all, and keeps it. Hands back the
    /// old allocation, empty, to be freed outside the lock. If the index is already as big, it
    /// keeps its own and hands `bigger` back.
    fn grow(&mut self, mut bigger: Vec<Slot<K>>) -> Vec<Slot<K>> {
        if bigger.capacity() <= self.slots.capacity() {
            return bigger;
        }
        // Within the room `bigger` already has: no allocation.
        bigger.append(&mut self.slots);
        core::mem::swap(&mut self.slots, &mut bigger);
        bigger
    }
}

/// The room one reservation keeps in its port's index, until it is bound or dropped.
struct Claim<K: Kernel> {
    port: K::PortRef,
    /// Whether it still counts in the port's `reserved`.
    counted: bool,
}

impl<K: Kernel> Drop for Claim<K> {
    fn drop(&mut self) {
        if self.counted {
            self.port.reserved.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// What one binding needs, made before any lock is taken: [`Source::bind`] uses it.
pub struct Reservation<K: Kernel> {
    store: Node<Store<K>>,
    claim: Claim<K>,
}

impl<K: Kernel> core::fmt::Debug for Reservation<K> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Reservation").finish_non_exhaustive()
    }
}

impl<K: Kernel> Reservation<K> {
    /// The size and alignment of each allocation one binding makes: its storage and its ticket,
    /// each in a [`Node`] of its own. The kernel-wide bound counts each binding by them, with
    /// its slots in a port's index ([`Port::INDEX_SLOT`]; ADR-0005 G5).
    pub const ALLOCATIONS: [(usize, usize); 2] = [
        (
            core::mem::size_of::<[Store<K>; 1]>(),
            core::mem::align_of::<[Store<K>; 1]>(),
        ),
        (
            core::mem::size_of::<[Ticket<K>; 1]>(),
            core::mem::align_of::<[Ticket<K>; 1]>(),
        ),
    ];

    /// What one binding to `port` needs, carrying `payload`: its storage, and room in the port's
    /// index. [`Unreserved::Full`] if the port already has as many bindings armed and reserved
    /// as its bound allows, found before anything is allocated, so that a full port is refused
    /// as a limit even when memory is short (ADR-0005 I1, D5's order); and again, under the
    /// port's lock, when the room is reserved. [`Unreserved::NoMemory`] if the heap has no room.
    /// Either way it drops `payload`, and the two nodes it made if it got that far. It changes
    /// nothing else but this: under a race, the refusal can come after this reservation grew the
    /// port's index, when others took the room it made, and the index stays grown, within
    /// [`Port::index_at_bound`] (the independent review of TASKS 57, finding T8). For a port that
    /// has closed it makes no room: the bind will be refused.
    pub fn try_new(
        scheduler: &K::Scheduler,
        port: &K::PortRef,
        payload: K::Payload,
    ) -> Result<Self, Unreserved> {
        if port.full(scheduler) {
            return Err(Unreserved::Full);
        }
        let ticket = Node::try_new(Ticket {
            port: None,
            way_back: None,
            next: None,
        })
        .ok_or(Unreserved::NoMemory)?;
        let mut store = Node::try_new(Store::new(payload)).ok_or(Unreserved::NoMemory)?;
        store.ticket = Some(ticket);
        let counted = port.reserve(scheduler)?;
        Ok(Self {
            store,
            claim: Claim {
                port: port.clone(),
                counted,
            },
        })
    }

    /// What the kernel gave it.
    pub fn payload(&self) -> &K::Payload {
        &self.store.payload
    }

    /// The payload, with the reservation given up.
    pub fn into_payload(self) -> K::Payload {
        let Self { store, claim } = self;
        drop(claim);
        store.into_inner().payload
    }
}

/// A binding's storage, come back: from [`Port::take`], [`Source::cancel`] or [`Port::close`].
/// Its payload says whom to refund.
pub struct Storage<K: Kernel>(Node<Store<K>>);

impl<K: Kernel> core::fmt::Debug for Storage<K> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Storage")
            .field("packet", &self.0.packet())
            .finish_non_exhaustive()
    }
}

impl<K: Kernel> Storage<K> {
    /// What the kernel gave the reservation.
    pub fn payload(&self) -> &K::Payload {
        &self.0.payload
    }

    /// The payload, and the storage freed.
    pub fn into_payload(self) -> K::Payload {
        self.0.into_inner().payload
    }
}

/// The storage an explicit cancel or a close took back: how many, and each one in turn.
pub struct Removed<K: Kernel> {
    storage: Stack<Store<K>>,
}

impl<K: Kernel> core::fmt::Debug for Removed<K> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Removed")
            .field("len", &self.storage.len())
            .finish()
    }
}

impl<K: Kernel> Removed<K> {
    /// How many bindings' storage it holds.
    pub const fn len(&self) -> usize {
        self.storage.len()
    }

    /// Whether it holds none.
    pub const fn is_empty(&self) -> bool {
        self.storage.is_empty()
    }
}

impl<K: Kernel> Iterator for Removed<K> {
    type Item = Storage<K>;

    fn next(&mut self) -> Option<Storage<K>> {
        self.storage.pop().map(Storage)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.storage.len(), Some(self.storage.len()))
    }
}

/// A port's queue and index.
struct Queues<K: Kernel> {
    /// Queued packets, oldest first.
    queue: Queue<Store<K>>,
    index: Index<K>,
    closed: bool,
    /// A reservation is growing the index ([`Port::reserve`]): no other grows it until that one
    /// has installed its block, freed the block that came back and cleared this.
    growing: bool,
}

/// What this crate's tests and models count of one port's index: the slots of every block the
/// port's growths and its index hold at a moment, and the most there have been at once
/// (ADR-0005 I1; the independent review of TASKS 57, finding T1). `core`'s atomics even under
/// Loom: it orders nothing, and a model reads it only after joining its threads.
#[cfg(test)]
struct Blocks {
    slots: core::sync::atomic::AtomicUsize,
    most: core::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl Blocks {
    const fn new() -> Self {
        Self {
            slots: core::sync::atomic::AtomicUsize::new(0),
            most: core::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// A block of `slots` slots was made.
    fn made(&self, slots: usize) {
        let now = self.slots.fetch_add(slots, Ordering::Relaxed) + slots;
        self.most.fetch_max(now, Ordering::Relaxed);
    }

    /// A block of `slots` slots was freed.
    fn freed(&self, slots: usize) {
        self.slots.fetch_sub(slots, Ordering::Relaxed);
    }
}

/// What a reservation's look under the port's lock decided ([`Port::reserve`]).
enum Look {
    /// The port has closed: no room.
    Closed,
    /// Room kept.
    Kept,
    /// Another reservation is growing the index: wait for it, then look again.
    Wait,
    /// No room, and this reservation grows the index, to this many slots.
    Grow(usize),
}

/// A port: packets queued in order, the threads waiting for one, and the index of the bindings
/// armed for it.
pub struct Port<K: Kernel> {
    state: SpinLock<Queues<K>>,
    /// Reservations made for this port and not yet bound or dropped. Raised only under the
    /// port's lock, and only while the index has room for one more: so the index's room is
    /// never less than this. Lowered by a bind, under the lock, or by a dropped reservation.
    reserved: prim::AtomicUsize,
    /// The most bindings armed and reservations in flight it may have at once (ADR-0005 I1):
    /// fixed when it is made. Each holds a slot of the index, so the index stays within
    /// [`Port::index_at_bound`] of it.
    bound: usize,
    /// Threads waiting for a packet, and reservations waiting for another's growth of the index.
    waiters: WaitQueue<Thread<K>, WAITERS>,
    #[cfg(test)]
    blocks: Blocks,
}

impl<K: Kernel> core::fmt::Debug for Port<K> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Port").finish_non_exhaustive()
    }
}

impl<K: Kernel> Port<K> {
    /// The size and alignment of one slot of a port's index (row K7 of the system-call threat
    /// model; ADR-0005 G5 asks that it be measured).
    pub const INDEX_SLOT: (usize, usize) = (
        core::mem::size_of::<Slot<K>>(),
        core::mem::align_of::<Slot<K>>(),
    );

    /// The fewest slots a port's index has once it has grown at all. Each growth then takes it to
    /// at least twice its capacity, as a `Vec` grows.
    pub const FIRST_INDEX_SLOTS: usize = FIRST_SLOTS;

    /// The most slots a port's index holds while its bindings armed and reservations in flight
    /// stay within `bound`, and the most its old block held while it last grew (ADR-0005 I1):
    /// what the kernel-wide bound counts for a port's index (G5).
    ///
    /// The index grows only when a reservation finds no room ([`Port::reserve`]), one
    /// reservation at a time, and then to the larger of what it needs, twice its capacity and
    /// [`FIRST_SLOTS`]. A reservation is made only while the bindings armed and reserved are
    /// under `bound`, and the index's room is never less than the reservations, so the index
    /// grows only from a capacity under `bound`, to at most twice that and one more:
    /// `2 * bound - 1`, or `FIRST_SLOTS` when that is more. Since no other growth runs meanwhile,
    /// the block a growth replaces is the one its grower found, which held under `bound` slots;
    /// and no third block exists while it runs (the independent review of TASKS 57, finding T1).
    /// With a bound of 0 it never grows.
    #[must_use]
    pub const fn index_at_bound(bound: usize) -> (usize, usize) {
        if bound == 0 {
            return (0, 0);
        }
        let most = bound.saturating_mul(2).saturating_sub(1);
        let most = if most > FIRST_SLOTS {
            most
        } else {
            FIRST_SLOTS
        };
        (most, bound - 1)
    }

    /// An open port with nothing queued and nothing armed, that accepts at most `bound` bindings
    /// armed and reservations in flight at once (ADR-0005 Amendment 10, I1): the bound its
    /// creator was given. A bound of 0 accepts no binding. There is no other way to make a port:
    /// every port has a bound, which a creator's figure can count (the independent review of
    /// TASKS 57, finding T9).
    #[cfg(not(all(test, loom)))]
    pub const fn bounded(bound: usize) -> Self {
        Self {
            state: SpinLock::new(Queues {
                queue: Queue::new(),
                index: Index::new(),
                closed: false,
                growing: false,
            }),
            reserved: prim::AtomicUsize::new(0),
            bound,
            waiters: WaitQueue::new(),
            #[cfg(test)]
            blocks: Blocks::new(),
        }
    }

    /// As the other `bounded`, under Loom, whose lock cannot be built in a `const`.
    #[cfg(all(test, loom))]
    pub fn bounded(bound: usize) -> Self {
        Self {
            state: SpinLock::new(Queues {
                queue: Queue::new(),
                index: Index::new(),
                closed: false,
                growing: false,
            }),
            reserved: prim::AtomicUsize::new(0),
            bound,
            waiters: WaitQueue::new(),
            #[cfg(test)]
            blocks: Blocks::new(),
        }
    }

    /// The most slots this port's index blocks have held at once, its growths' and its index's
    /// together (ADR-0005 I1): for this crate's tests and models.
    #[cfg(test)]
    pub(crate) fn most_index_slots(&self) -> usize {
        self.blocks.most.load(Ordering::Relaxed)
    }

    /// Whether the port, open, already has as many bindings armed and reservations in flight as
    /// its bound allows: a look before a reservation allocates anything, which
    /// [`Port::reserve`] makes again under the lock.
    fn full(&self, scheduler: &K::Scheduler) -> bool {
        scheduler.without_interrupts(|| {
            let state = self.state.lock();
            !state.closed
                && state
                    .index
                    .used()
                    .saturating_add(self.reserved.load(Ordering::Relaxed))
                    >= self.bound
        })
    }

    /// How many packets are queued.
    pub fn queued(&self, scheduler: &K::Scheduler) -> usize {
        scheduler.without_interrupts(|| self.state.lock().queue.len())
    }

    /// How many bindings are armed for this port.
    pub fn armed(&self, scheduler: &K::Scheduler) -> usize {
        scheduler.without_interrupts(|| {
            let state = self.state.lock();
            state.index.used()
        })
    }

    /// Whether [`Port::close`] has run.
    pub fn is_closed(&self, scheduler: &K::Scheduler) -> bool {
        scheduler.without_interrupts(|| self.state.lock().closed)
    }

    /// Wakes every thread waiting in [`Port::take`], and every reservation waiting for another's
    /// growth of the index, each to look again at what it waits for. Whoever closes or moves a
    /// handle a thread may be waiting through calls it, as for any wait, and so does a growth
    /// when it ends.
    pub fn wake(&self, scheduler: &K::Scheduler) {
        for thread in self.waiters.take_all(scheduler).into_iter().flatten() {
            scheduler.wake(thread);
        }
    }

    /// Keeps room in the index for one more binding, growing it if it must, fallibly and with no
    /// lock held. Returns whether it kept any: a closed port gets none.
    ///
    /// [`Unreserved::Full`] if the bindings armed and the reservations in flight are already at
    /// the port's bound: checked under the lock that raises `reserved`, so reservations from
    /// several processors never pass it together (ADR-0005 I1).
    ///
    /// One reservation grows a port's index at a time (the independent review of TASKS 57,
    /// finding T1). The first to find no room marks the index as growing, under the lock, and
    /// only it allocates ([`Port::grow`]). Another that finds no room while the mark is set waits
    /// on the port's queue of waiters until the grower has installed its block, freed the block
    /// that came back and cleared the mark, then looks again. So the heap never holds more of the
    /// port's index than the index and the one block being made for it, or that block and the
    /// one it replaced: the two blocks the kernel-wide bound counts ([`Port::index_at_bound`]).
    ///
    /// The wait cannot last for ever, nor spin:
    ///
    /// - The grower holds no lock while it allocates, and nothing on its way to clearing the
    ///   mark waits for anything: it allocates (or is refused), takes the port's lock to install
    ///   the block, frees what came back, takes the lock again to clear the mark, then wakes the
    ///   waiters. It clears the mark on every path: grown, refused by the heap, the port closed.
    ///   A thread ends only by its own call or a fault in ring 3, never inside this one.
    /// - It never holds the port's lock and the waiters' queue's lock together. A waiter takes
    ///   the queue's lock and then the port's, to read the mark, as a wait for a packet does.
    /// - A waiter blocks rather than spins, and the wait-queue protocol loses no wake-up (its
    ///   Loom models; here `ports_two_reservations_race_to_grow_a_bound_of_two`). One that finds
    ///   the queue full, or the boot flow, polls by yielding, as a wait for a packet does.
    /// - Each growth at least doubles the index, and an index grows only while it holds fewer
    ///   slots than the bound, so a port grows at most about log2 of its bound times in its life:
    ///   one reservation waits at most that many times.
    fn reserve(&self, scheduler: &K::Scheduler) -> Result<bool, Unreserved> {
        loop {
            let look = scheduler.without_interrupts(|| {
                let mut state = self.state.lock();
                if state.closed {
                    return Ok(Look::Closed);
                }
                let reserved = self.reserved.load(Ordering::Relaxed);
                if state.index.used().saturating_add(reserved) >= self.bound {
                    return Err(Unreserved::Full);
                }
                if state.index.room() > reserved {
                    self.reserved.fetch_add(1, Ordering::Relaxed);
                    return Ok(Look::Kept);
                }
                if state.growing {
                    return Ok(Look::Wait);
                }
                state.growing = true;
                Ok(Look::Grow(state.index.wanted(reserved)))
            })?;
            match look {
                Look::Closed => return Ok(false),
                Look::Kept => return Ok(true),
                Look::Wait => {
                    self.waiters
                        .wait_until(scheduler, || !self.state.lock().growing);
                }
                Look::Grow(wanted) => self.grow(scheduler, wanted)?,
            }
        }
    }

    /// The one growth of the index under way ([`Port::reserve`]): makes a block of `wanted` slots
    /// with no lock held, installs it unless the port has closed meanwhile, frees the block that
    /// comes back (the old index, or this one unused), and only then clears the mark and wakes
    /// whoever waits. [`Unreserved::NoMemory`] if the heap has no room, with the mark cleared all
    /// the same.
    fn grow(&self, scheduler: &K::Scheduler, wanted: usize) -> Result<(), Unreserved> {
        let mut bigger = Vec::new();
        let made = bigger.try_reserve_exact(wanted).is_ok();
        if made {
            #[cfg(test)]
            self.blocks.made(bigger.capacity());
            let unused = scheduler.without_interrupts(|| {
                let mut state = self.state.lock();
                if state.closed {
                    return bigger;
                }
                state.index.grow(bigger)
            });
            #[cfg(test)]
            let freed = unused.capacity();
            drop(unused);
            #[cfg(test)]
            self.blocks.freed(freed);
        }
        scheduler.without_interrupts(|| self.state.lock().growing = false);
        self.wake(scheduler);
        if made {
            Ok(())
        } else {
            Err(Unreserved::NoMemory)
        }
    }

    /// Whether a waiting thread has something to look at: a packet, or the port closed.
    fn ready(&self, scheduler: &K::Scheduler) -> bool {
        scheduler.without_interrupts(|| {
            let state = self.state.lock();
            state.closed || state.queue.len() != 0
        })
    }

    /// The oldest packet and its storage, waiting until the monotonic clock reaches `deadline`
    /// (nanoseconds; 0 polls) through the wait protocol, so its resolution and its
    /// [`WAITERS`] are that protocol's.
    ///
    /// `still_open` says whether the handle waited through is still open. It is asked before a
    /// packet is taken, so a wait that ends because that handle closed takes nothing:
    /// [`TakeError::Canceled`]. A port that has closed answers the same.
    pub fn take(
        &self,
        scheduler: &K::Scheduler,
        deadline: u64,
        still_open: impl Fn() -> bool,
    ) -> Result<(Packet, Storage<K>), TakeError> {
        loop {
            let waited = self.waiters.wait_until_deadline(
                scheduler,
                || !still_open() || self.ready(scheduler),
                deadline,
            );
            if !still_open() {
                return Err(TakeError::Canceled);
            }
            let taken = scheduler.without_interrupts(|| {
                let mut state = self.state.lock();
                if state.closed {
                    Err(TakeError::Canceled)
                } else {
                    Ok(state.queue.pop())
                }
            });
            match taken {
                Ok(Some(store)) => return Ok((store.packet(), Storage(store))),
                // The deadline passed, and whatever was there went to another thread.
                Ok(None) if waited.is_err() => return Err(TakeError::TimedOut),
                // Another thread took it first: wait again.
                Ok(None) => {}
                Err(canceled) => return Err(canceled),
            }
        }
    }

    /// Closes the port, for when its last handle goes. It wakes every waiting thread, and takes
    /// back the storage of every binding queued in it or armed for it.
    ///
    /// Under the port's lock it takes the queue and the index. Then, for each source the index
    /// names, once each, it takes off that source, under its lock, the storage of every binding
    /// armed for this port. It never holds the two locks together.
    ///
    /// When it returns, nothing is queued in the port and nothing is armed for it, now or later.
    /// Each binding's storage is in what it returns, unless an explicit cancel took it first.
    pub fn close(&self, scheduler: &K::Scheduler) -> Removed<K> {
        let (mut storage, mut slots) = scheduler.without_interrupts(|| {
            let mut state = self.state.lock();
            state.closed = true;
            let mut queued = Stack::new();
            state.queue.drain_into(&mut queued);
            let slots = core::mem::take(&mut state.index.slots);
            state.index.free = None;
            state.index.freed = 0;
            (queued, slots)
        });
        // Each finds the port closed.
        self.wake(scheduler);
        // Each source once: its slots are together once sorted by its number.
        slots.sort_unstable_by_key(Slot::source);
        let mut visited = 0;
        for slot in &slots {
            if let Slot::Used { source, way_back } = slot
                && *source != visited
            {
                visited = *source;
                way_back.disarm(scheduler, self, &mut storage);
            }
        }
        // The ways back are dropped here, outside every lock.
        #[cfg(test)]
        self.blocks.freed(slots.capacity());
        drop(slots);
        Removed { storage }
    }
}

/// A source's bindings, and its number.
struct Arms<K: Kernel> {
    /// 0 until its first bind gives it one.
    number: u64,
    /// The storage of every binding armed on it.
    armed: Stack<Store<K>>,
}

/// One thing that raises signals, and the bindings armed on it.
pub struct Source<K: Kernel> {
    state: SpinLock<Arms<K>>,
}

impl<K: Kernel> core::fmt::Debug for Source<K> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Source").finish_non_exhaustive()
    }
}

impl<K: Kernel> Default for Source<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Kernel> Source<K> {
    /// A source with nothing armed. Its first bind gives it its number.
    #[cfg(not(all(test, loom)))]
    pub const fn new() -> Self {
        Self {
            state: SpinLock::new(Arms {
                number: 0,
                armed: Stack::new(),
            }),
        }
    }

    /// A source with nothing armed (under Loom, whose lock cannot be built in a `const`).
    #[cfg(all(test, loom))]
    pub fn new() -> Self {
        Self {
            state: SpinLock::new(Arms {
                number: 0,
                armed: Stack::new(),
            }),
        }
    }

    /// How many bindings are armed on it.
    pub fn armed(&self, scheduler: &K::Scheduler) -> usize {
        scheduler.without_interrupts(|| self.state.lock().armed.len())
    }

    /// Binds this source to the port `storage` was reserved for: `this` is the counted reference
    /// to what holds the source, which the port's index keeps as the binding's way back.
    ///
    /// Under the source's lock it asks `still_open`, whether the handle the binding is made
    /// through is still in the binder's table, and reads `signals` fresh. If the signals meet
    /// the mask, the packet is queued now ([`Bound::Queued`]) and the port's waiters are woken
    /// after the locks are released. If not, the binding is armed ([`Bound::Armed`]). Nothing
    /// allocates.
    ///
    /// A closed handle or a closed port refuses, and hands the reservation back: nothing is
    /// bound.
    pub fn bind(
        this: &K::SourceRef,
        scheduler: &K::Scheduler,
        storage: Reservation<K>,
        request: Request<K::Owner>,
        still_open: impl Fn() -> bool,
        signals: impl Fn() -> u32,
    ) -> Result<Bound, (Refused, Reservation<K>)> {
        let source: &Self = this;
        // Taken before any lock: one more count each. What is not kept is dropped after the
        // locks are released.
        let way_back = this.clone();
        let Reservation {
            store: mut stored,
            mut claim,
        } = storage;
        let way_on = claim.port.clone();
        let port: &Port<K> = &claim.port;
        let counted = claim.counted;
        let done = scheduler.without_interrupts(move || {
            let mut arms = source.state.lock();
            if !still_open() {
                return Done::Refused(Refused::HandleClosed, stored, way_back, way_on);
            }
            let observed = signals();
            let mut state = port.state.lock();
            if state.closed || !counted {
                return Done::Refused(Refused::PortClosed, stored, way_back, way_on);
            }
            if arms.number == 0 {
                arms.number = NUMBERS.fetch_add(1, Ordering::Relaxed);
            }
            let number = arms.number;
            stored.owner = Some(request.owner);
            stored.source = number;
            stored.key = request.key;
            stored.trigger = request.mask;
            if observed & request.mask != 0 {
                port.reserved.fetch_sub(1, Ordering::Relaxed);
                stored.kind = Kind::Signal;
                stored.observed = observed;
                state.queue.push(stored);
                return Done::Queued(way_back, way_on);
            }
            let slot = match state.index.take(number, way_back) {
                Ok(slot) => slot,
                Err(way_back) => {
                    return Done::Refused(Refused::NoRoom, stored, way_back, way_on);
                }
            };
            port.reserved.fetch_sub(1, Ordering::Relaxed);
            drop(state);
            stored.slot = slot;
            stored.port = Some(way_on);
            arms.armed.push(stored);
            Done::Armed
        });
        match done {
            Done::Armed => {
                claim.counted = false;
                Ok(Bound::Armed)
            }
            Done::Queued(way_back, way_on) => {
                claim.counted = false;
                port.wake(scheduler);
                drop((way_back, way_on));
                Ok(Bound::Queued)
            }
            Done::Refused(why, store, way_back, way_on) => {
                drop((way_back, way_on));
                Err((why, Reservation { store, claim }))
            }
        }
    }

    /// Fires every binding armed here whose mask the object's signals meet: each becomes a
    /// [`Kind::Signal`] packet in its port, and that port's waiters are woken after the locks are
    /// released. Called after every change of the object's state, holding none of the kernel's
    /// own locks. `signals` is read fresh, under this source's lock. Returns how many packets it
    /// queued.
    pub fn deliver(&self, scheduler: &K::Scheduler, signals: impl Fn() -> u32) -> usize {
        let (queued, fired) = self.fire(scheduler, Kind::Signal, signals, |store, observed| {
            store.trigger & observed != 0
        });
        wake_ports(scheduler, &fired);
        drop(fired);
        queued
    }

    /// The implicit cancel: every binding armed here whose owner `which` picks becomes a
    /// [`Kind::Canceled`] packet, with the signals read then. Its storage stays charged, as the
    /// packet, until the packet is read or cancelled or the port closes. For a handle closed or
    /// moved, `which` picks that handle's bindings; for an exiting process, every binding it
    /// made. Returns how many packets it queued.
    pub fn cancel_owners(
        &self,
        scheduler: &K::Scheduler,
        which: impl Fn(&K::Owner) -> bool,
        signals: impl Fn() -> u32,
    ) -> usize {
        let (queued, canceled) = self.fire(scheduler, Kind::Canceled, signals, |store, _| {
            store.owner.as_ref().is_some_and(&which)
        });
        wake_ports(scheduler, &canceled);
        drop(canceled);
        queued
    }

    /// Takes off every binding armed here that `select` picks and queues it in its port as a
    /// packet of `kind`. A binding whose port has closed stays, for the close to take. Returns how
    /// many packets it queued, and the tickets, to wake their ports and be dropped after the locks
    /// are released.
    fn fire(
        &self,
        scheduler: &K::Scheduler,
        kind: Kind,
        signals: impl Fn() -> u32,
        select: impl Fn(&Store<K>, u32) -> bool,
    ) -> (usize, Stack<Ticket<K>>) {
        scheduler.without_interrupts(|| {
            let mut arms = self.state.lock();
            if arms.armed.is_empty() {
                return (0, Stack::new());
            }
            let observed = signals();
            let mut picked = Stack::new();
            arms.armed
                .remove_where(|store| select(store, observed), &mut picked);
            let mut queued = 0usize;
            let mut tickets = Stack::new();
            while let Some(mut store) = picked.pop() {
                let Some(port) = store.port.take() else {
                    arms.armed.push(store);
                    continue;
                };
                let mut state = port.state.lock();
                if state.closed {
                    // The close has this binding's way back, and takes it off this source.
                    drop(state);
                    store.port = Some(port);
                    arms.armed.push(store);
                    continue;
                }
                let way_back = state.index.give_back(store.slot);
                store.kind = kind;
                store.observed = observed;
                let ticket = store.ticket.take();
                state.queue.push(store);
                drop(state);
                queued = queued.saturating_add(1);
                if let Some(mut ticket) = ticket {
                    ticket.port = Some(port);
                    ticket.way_back = way_back;
                    tickets.push(ticket);
                }
            }
            (queued, tickets)
        })
    }

    /// The explicit cancel: removes every binding of this source with `key` that is armed for
    /// `port`, and every packet from them queued in it, of either kind. Nothing is queued for
    /// it. When it returns, no packet from a binding it matched exists in the port or can
    /// appear there. It hands back the storage it removed: [`Removed::len`] is how many.
    pub fn cancel(&self, scheduler: &K::Scheduler, port: &Port<K>, key: u64) -> Removed<K> {
        let storage = scheduler.without_interrupts(|| {
            let mut arms = self.state.lock();
            let mut storage = Stack::new();
            arms.armed.remove_where(
                |store| store.key == key && store.aims_at(port),
                &mut storage,
            );
            let number = arms.number;
            if number == 0 {
                return storage;
            }
            let mut state = port.state.lock();
            if !state.closed {
                // Their slots, given back now; the ways back leave in the storage's tickets.
                let mut cursor = storage.top.as_mut();
                while let Some(store) = cursor {
                    let way_back = state.index.give_back(store.slot);
                    if let Some(ticket) = store.ticket.as_mut() {
                        ticket.way_back = way_back;
                    }
                    cursor = store.next.as_mut();
                }
            }
            state.queue.remove_where(
                |store| store.source == number && store.key == key,
                &mut storage,
            );
            storage
        });
        Removed { storage }
    }

    /// Takes off every binding armed here for `port`, onto `into`: [`Port::close`] has their
    /// ways back.
    fn disarm(&self, scheduler: &K::Scheduler, port: &Port<K>, into: &mut Stack<Store<K>>) {
        scheduler.without_interrupts(|| {
            self.state
                .lock()
                .armed
                .remove_where(|store| store.aims_at(port), into);
        });
    }
}

/// What a bind did, out of its critical section: what it did not keep is dropped after.
enum Done<K: Kernel> {
    Armed,
    Queued(K::SourceRef, K::PortRef),
    Refused(Refused, Node<Store<K>>, K::SourceRef, K::PortRef),
}

// Under `--cfg loom` the locks can only run inside a Loom model (`models.rs` has this protocol's).
#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::Relaxed};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::thread;
    use std::time::Instant;
    use std::vec::Vec;

    /// The host's stand-in for the kernel's scheduler: a flow that cannot block, so a wait polls
    /// by yielding, and a clock that starts when a test first reads it.
    #[derive(Debug)]
    struct Host;

    fn now() -> u64 {
        static START: OnceLock<Instant> = OnceLock::new();
        u64::try_from(START.get_or_init(Instant::now).elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    impl Scheduler for Host {
        type Thread = u32;

        fn current_thread(&self) -> Option<u32> {
            None
        }

        fn without_interrupts<R>(&self, f: impl FnOnce() -> R) -> R {
            f()
        }

        fn block(&self) {}

        fn block_until(&self, _deadline: u64) {}

        fn yield_now(&self) {
            thread::yield_now();
        }

        fn wake(&self, _thread: u32) {}

        fn monotonic_nanos(&self) -> u64 {
            now()
        }
    }

    /// What the tests' kernel supplies: an owner is a process's ID and a handle's value.
    struct Test;

    impl Kernel for Test {
        type Scheduler = Host;
        type Payload = Charge;
        type Owner = (u64, u32);
        type PortRef = Arc<Port<Test>>;
        type SourceRef = Arc<Source<Test>>;
    }

    /// An account: how many bindings it is charged for, and which reservations came back to it.
    #[derive(Debug, Default)]
    struct Account {
        charged: AtomicUsize,
        back: Mutex<Vec<u32>>,
    }

    impl Account {
        fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }

        fn charged(&self) -> usize {
            self.charged.load(Relaxed)
        }

        /// The reservations refunded so far, in order of their numbers.
        fn back(&self) -> Vec<u32> {
            let mut back = self.back.lock().unwrap().clone();
            back.sort_unstable();
            back
        }
    }

    /// One reservation's payload: its account, and a number of its own.
    #[derive(Debug)]
    struct Charge {
        account: Arc<Account>,
        id: u32,
    }

    /// The charge, then the reservation: as the kernel's bind will do.
    fn reserve(port: &Arc<Port<Test>>, account: &Arc<Account>, id: u32) -> Reservation<Test> {
        account.charged.fetch_add(1, Relaxed);
        Reservation::<Test>::try_new(
            &Host,
            port,
            Charge {
                account: Arc::clone(account),
                id,
            },
        )
        .unwrap()
    }

    /// What the kernel does with storage that comes back: the refund.
    fn refund(storage: Storage<Test>) -> u32 {
        let Charge { account, id } = storage.into_payload();
        account.charged.fetch_sub(1, Relaxed);
        account.back.lock().unwrap().push(id);
        id
    }

    /// Every piece of storage in `removed`, refunded; their numbers in order.
    fn refund_all(removed: Removed<Test>) -> Vec<u32> {
        let mut ids: Vec<u32> = removed.map(refund).collect();
        ids.sort_unstable();
        ids
    }

    /// One object that raises signals.
    struct Object {
        source: Arc<Source<Test>>,
        signals: AtomicU32,
    }

    impl Object {
        fn new() -> Self {
            Self {
                source: Arc::new(Source::new()),
                signals: AtomicU32::new(0),
            }
        }

        fn signals(&self) -> u32 {
            self.signals.load(Relaxed)
        }

        /// The state first, then the delivery, as the kernel's raisers do.
        fn raise(&self, bits: u32) -> usize {
            self.signals.fetch_or(bits, Relaxed);
            self.source.deliver(&Host, || self.signals())
        }

        fn clear(&self, bits: u32) {
            self.signals.fetch_and(!bits, Relaxed);
        }

        fn bind_as(
            &self,
            port: &Arc<Port<Test>>,
            account: &Arc<Account>,
            id: u32,
            owner: (u64, u32),
            key: u64,
            mask: u32,
        ) -> Result<Bound, (Refused, Reservation<Test>)> {
            Source::bind(
                &self.source,
                &Host,
                reserve(port, account, id),
                Request { owner, key, mask },
                || true,
                || self.signals(),
            )
        }

        fn bind(
            &self,
            port: &Arc<Port<Test>>,
            account: &Arc<Account>,
            id: u32,
            key: u64,
            mask: u32,
        ) -> Bound {
            match self.bind_as(port, account, id, ME, key, mask) {
                Ok(bound) => bound,
                Err((why, _)) => panic!("a bind was refused: {why:?}"),
            }
        }

        /// The handle `owner` names closed or moved.
        fn close_handle(&self, owner: (u64, u32)) -> usize {
            self.source
                .cancel_owners(&Host, |o| *o == owner, || self.signals())
        }

        fn cancel(&self, port: &Port<Test>, key: u64) -> Removed<Test> {
            self.source.cancel(&Host, port, key)
        }

        fn armed(&self) -> usize {
            self.source.armed(&Host)
        }
    }

    /// The binder: process 1, through its handle 10.
    const ME: (u64, u32) = (1, 10);
    const READABLE: u32 = 1 << 0;
    const WRITABLE: u32 = 1 << 1;
    const CLOSED: u32 = 1 << 2;

    /// A port bounded so far above what any of these tests arms that its bound never decides
    /// anything: the bound's own tests are under "a port's bound".
    fn port() -> Arc<Port<Test>> {
        Arc::new(Port::bounded(usize::MAX))
    }

    /// One look at the port: a packet, or nothing.
    fn poll(port: &Port<Test>) -> Result<(Packet, Storage<Test>), TakeError> {
        port.take(&Host, 0, || true)
    }

    /// Every packet queued, in queue order, each one's storage refunded.
    fn drain(port: &Port<Test>) -> Vec<Packet> {
        let mut packets = Vec::new();
        while let Ok((packet, storage)) = poll(port) {
            refund(storage);
            packets.push(packet);
        }
        packets
    }

    const fn signal(key: u64, trigger: u32, observed: u32) -> Packet {
        Packet {
            key,
            kind: Kind::Signal,
            trigger,
            observed,
        }
    }

    const fn canceled(key: u64, trigger: u32, observed: u32) -> Packet {
        Packet {
            key,
            kind: Kind::Canceled,
            trigger,
            observed,
        }
    }

    /// Nothing left anywhere: no binding armed, nothing queued, every charge refunded.
    fn nothing_left(port: &Port<Test>, objects: &[&Object], account: &Account) {
        assert_eq!(port.queued(&Host), 0, "a packet is still queued");
        assert_eq!(
            port.armed(&Host),
            0,
            "a binding is still armed for the port"
        );
        for object in objects {
            assert_eq!(object.armed(), 0, "a binding is still armed on a source");
        }
        assert_eq!(account.charged(), 0, "a charge was never refunded");
    }

    // ------------------------------------------------------------ C9: one shot, level-triggered

    #[test]
    fn a_binding_fires_once_and_a_second_signal_makes_no_second_packet() {
        let (port, account, object) = (port(), Account::new(), Object::new());
        assert_eq!(object.bind(&port, &account, 1, 7, READABLE), Bound::Armed);
        assert_eq!(account.charged(), 1, "charged at the bind");
        assert_eq!(object.raise(READABLE), 1);
        assert_eq!(
            object.raise(WRITABLE),
            0,
            "a second signal made a second packet"
        );
        object.clear(READABLE);
        assert_eq!(
            object.raise(READABLE),
            0,
            "a second edge made a second packet"
        );
        assert_eq!(drain(&port), [signal(7, READABLE, READABLE)]);
        nothing_left(&port, &[&object], &account);
    }

    #[test]
    fn a_signal_already_raised_queues_the_packet_before_the_bind_returns() {
        let (port, account, object) = (port(), Account::new(), Object::new());
        object.signals.store(READABLE | CLOSED, Relaxed);
        assert_eq!(
            object.bind(&port, &account, 1, 3, READABLE | WRITABLE),
            Bound::Queued
        );
        assert_eq!(object.armed(), 0);
        assert_eq!(port.queued(&Host), 1);
        // `observed` is every signal raised then, `trigger` the mask asked for.
        assert_eq!(
            drain(&port),
            [signal(3, READABLE | WRITABLE, READABLE | CLOSED)]
        );
        nothing_left(&port, &[&object], &account);
    }

    #[test]
    fn a_signal_the_mask_does_not_name_leaves_the_binding_armed() {
        let (port, account, object) = (port(), Account::new(), Object::new());
        object.bind(&port, &account, 1, 3, READABLE);
        assert_eq!(object.raise(WRITABLE | CLOSED), 0);
        assert_eq!(object.armed(), 1);
        assert_eq!(port.armed(&Host), 1);
        assert_eq!(object.raise(READABLE), 1);
        assert_eq!(
            drain(&port),
            [signal(3, READABLE, READABLE | WRITABLE | CLOSED)]
        );
        nothing_left(&port, &[&object], &account);
    }

    #[test]
    fn the_key_comes_back_unchanged() {
        let (port, account, object) = (port(), Account::new(), Object::new());
        for (id, key) in [(1, 0), (2, 1), (3, u64::MAX), (4, 0x8000_0000_0000_0001)] {
            object.bind(&port, &account, id, key, READABLE);
        }
        assert_eq!(object.raise(READABLE), 4);
        let mut keys: Vec<u64> = drain(&port).iter().map(|p| p.key).collect();
        keys.sort_unstable();
        assert_eq!(keys, [0, 1, 0x8000_0000_0000_0001, u64::MAX]);
        nothing_left(&port, &[&object], &account);
    }

    // ------------------------------------------------------------ refusals

    #[test]
    fn a_bind_through_a_closed_handle_is_refused_and_changes_nothing() {
        let (port, account, object) = (port(), Account::new(), Object::new());
        object.signals.store(READABLE, Relaxed);
        let refused = Source::bind(
            &object.source,
            &Host,
            reserve(&port, &account, 1),
            Request {
                owner: ME,
                key: 1,
                mask: READABLE,
            },
            || false,
            || object.signals(),
        );
        let Err((why, storage)) = refused else {
            panic!("a bind through a closed handle was not refused");
        };
        assert_eq!(why, Refused::HandleClosed);
        assert_eq!(
            storage.payload().id,
            1,
            "the reservation came back as it went"
        );
        assert_eq!(port.queued(&Host), 0, "a refused bind queued a packet");
        assert_eq!(object.armed(), 0);
        // Handed back whole: it binds once the handle is open.
        assert!(matches!(
            Source::bind(
                &object.source,
                &Host,
                storage,
                Request {
                    owner: ME,
                    key: 1,
                    mask: READABLE
                },
                || true,
                || object.signals(),
            ),
            Ok(Bound::Queued)
        ));
        assert_eq!(drain(&port), [signal(1, READABLE, READABLE)]);
        nothing_left(&port, &[&object], &account);
    }

    #[test]
    fn a_bind_to_a_closed_port_is_refused_and_changes_nothing() {
        let (port, account, object) = (port(), Account::new(), Object::new());
        let early = reserve(&port, &account, 1);
        assert!(port.close(&Host).is_empty());
        assert!(port.is_closed(&Host));
        for (id, storage) in [(1, early), (2, reserve(&port, &account, 2))] {
            let refused = Source::bind(
                &object.source,
                &Host,
                storage,
                Request {
                    owner: ME,
                    key: 1,
                    mask: READABLE,
                },
                || true,
                || READABLE,
            );
            let Err((why, storage)) = refused else {
                panic!("a bind to a closed port was not refused");
            };
            assert_eq!(why, Refused::PortClosed);
            assert_eq!(storage.payload().id, id);
            let charge = storage.into_payload();
            charge.account.charged.fetch_sub(1, Relaxed);
        }
        nothing_left(&port, &[&object], &account);
    }

    // ------------------------------------------------------------ implicit cancel (shapes 11, 12, 16)

    #[test]
    fn an_implicit_cancel_queues_a_canceled_packet_and_refunds_only_when_it_is_read() {
        let (port, account, object) = (port(), Account::new(), Object::new());
        object.bind(&port, &account, 1, 9, READABLE);
        object.signals.store(WRITABLE, Relaxed);
        assert_eq!(object.close_handle(ME), 1);
        assert_eq!(object.armed(), 0);
        assert_eq!(
            account.charged(),
            1,
            "an implicit cancel refunded the storage"
        );
        // The signals read at the cancel, the mask asked for.
        assert_eq!(drain(&port), [canceled(9, READABLE, WRITABLE)]);
        // Nothing more comes of it.
        assert_eq!(object.raise(READABLE), 0);
        assert_eq!(object.close_handle(ME), 0);
        nothing_left(&port, &[&object], &account);
    }

    #[test]
    fn an_implicit_cancel_follows_the_handle_not_the_object() {
        let (port, account, object) = (port(), Account::new(), Object::new());
        let duplicate = (1, 11);
        let other_process = (2, 10);
        object.bind_as(&port, &account, 1, ME, 1, READABLE).unwrap();
        object
            .bind_as(&port, &account, 2, duplicate, 2, READABLE)
            .unwrap();
        object
            .bind_as(&port, &account, 3, other_process, 3, READABLE)
            .unwrap();
        // Closing the duplicate cancels only what was bound through it.
        assert_eq!(object.close_handle(duplicate), 1);
        assert_eq!(object.armed(), 2);
        assert_eq!(object.raise(READABLE), 2);
        let mut packets = drain(&port);
        packets.sort_unstable_by_key(|p| p.key);
        assert_eq!(
            packets,
            [
                signal(1, READABLE, READABLE),
                canceled(2, READABLE, 0),
                signal(3, READABLE, READABLE)
            ]
        );
        nothing_left(&port, &[&object], &account);
    }

    #[test]
    fn an_exit_cancels_every_binding_its_process_made_on_each_object() {
        let (port, account) = (port(), Account::new());
        let (first, second) = (Object::new(), Object::new());
        first
            .bind_as(&port, &account, 1, (1, 10), 1, READABLE)
            .unwrap();
        first
            .bind_as(&port, &account, 2, (1, 11), 2, READABLE)
            .unwrap();
        second
            .bind_as(&port, &account, 3, (1, 12), 3, READABLE)
            .unwrap();
        second
            .bind_as(&port, &account, 4, (2, 10), 4, READABLE)
            .unwrap();
        // Process 1 exits: each object its table held is cancelled by the process's ID.
        for object in [&first, &second] {
            object
                .source
                .cancel_owners(&Host, |owner| owner.0 == 1, || object.signals());
        }
        assert_eq!((first.armed(), second.armed()), (0, 1));
        let mut kinds: Vec<(u64, Kind)> = drain(&port).iter().map(|p| (p.key, p.kind)).collect();
        kinds.sort_unstable_by_key(|k| k.0);
        assert_eq!(
            kinds,
            [
                (1, Kind::Canceled),
                (2, Kind::Canceled),
                (3, Kind::Canceled)
            ]
        );
        assert_eq!(
            second.raise(READABLE),
            1,
            "another process's binding was cancelled"
        );
        drain(&port);
        nothing_left(&port, &[&first, &second], &account);
    }

    #[test]
    fn a_packet_already_queued_stays_and_no_canceled_packet_follows_it() {
        let (port, account, object) = (port(), Account::new(), Object::new());
        object.bind(&port, &account, 1, 5, READABLE);
        assert_eq!(object.raise(READABLE), 1);
        assert_eq!(object.close_handle(ME), 0, "one binding made two packets");
        assert_eq!(drain(&port), [signal(5, READABLE, READABLE)]);
        nothing_left(&port, &[&object], &account);
    }

    // ------------------------------------------------------------ explicit cancel (shapes 4, 9, 12)

    #[test]
    fn an_explicit_cancel_is_silent_and_takes_armed_bindings_and_packets_of_either_kind() {
        let (port, account, object) = (port(), Account::new(), Object::new());
        let other = (1, 11);
        object.bind(&port, &account, 1, 7, READABLE); // fires: a signal packet
        object
            .bind_as(&port, &account, 2, other, 7, WRITABLE)
            .unwrap(); // cancelled: a canceled packet
        object.bind(&port, &account, 3, 7, CLOSED); // stays armed
        object.bind(&port, &account, 4, 8, CLOSED); // another key
        assert_eq!(object.raise(READABLE), 1);
        assert_eq!(object.close_handle(other), 1);
        assert_eq!(port.queued(&Host), 2);
        let removed = object.cancel(&port, 7);
        assert_eq!(removed.len(), 3);
        assert_eq!(refund_all(removed), [1, 2, 3]);
        assert_eq!(
            port.queued(&Host),
            0,
            "a cancel left a packet, or queued one"
        );
        assert_eq!(object.armed(), 1, "the other key's binding went too");
        // Nothing from the cancelled bindings later.
        assert_eq!(object.raise(CLOSED), 1);
        assert_eq!(drain(&port), [signal(8, CLOSED, READABLE | CLOSED)]);
        nothing_left(&port, &[&object], &account);
    }

    #[test]
    fn an_explicit_cancel_matches_the_object_the_port_and_the_key() {
        let (account, object, other) = (Account::new(), Object::new(), Object::new());
        let (port, elsewhere) = (port(), port());
        object.bind(&port, &account, 1, 7, READABLE);
        object.bind(&elsewhere, &account, 2, 7, READABLE);
        other.bind(&port, &account, 3, 7, READABLE);
        assert_eq!(other.raise(READABLE), 1);
        assert!(object.cancel(&port, 6).is_empty(), "another key matched");
        let removed = object.cancel(&port, 7);
        assert_eq!(refund_all(removed), [1]);
        assert!(
            object.cancel(&port, 7).is_empty(),
            "a second cancel found something"
        );
        assert_eq!(object.armed(), 1, "the binding for the other port went");
        assert_eq!(port.queued(&Host), 1, "another object's packet went");
        assert_eq!(object.raise(READABLE), 1);
        assert_eq!(drain(&elsewhere), [signal(7, READABLE, READABLE)]);
        assert_eq!(drain(&port), [signal(7, READABLE, READABLE)]);
        nothing_left(&port, &[&object, &other], &account);
        nothing_left(&elsewhere, &[], &account);
    }

    #[test]
    fn a_source_never_bound_cancels_nothing() {
        let (port, account, object, other) = (port(), Account::new(), Object::new(), Object::new());
        other.bind(&port, &account, 1, 0, READABLE);
        assert_eq!(other.raise(READABLE), 1);
        // Its number is 0 until it binds: it must not match a packet from a source numbered 0.
        assert!(object.cancel(&port, 0).is_empty());
        assert_eq!(drain(&port), [signal(0, READABLE, READABLE)]);
        nothing_left(&port, &[&object, &other], &account);
    }

    #[test]
    fn the_same_key_bound_again_after_a_cancel_fires_normally() {
        let (port, account, object) = (port(), Account::new(), Object::new());
        object.bind(&port, &account, 1, 4, READABLE);
        assert_eq!(refund_all(object.cancel(&port, 4)), [1]);
        object.bind(&port, &account, 2, 4, READABLE);
        assert_eq!(object.raise(READABLE), 1);
        assert_eq!(drain(&port), [signal(4, READABLE, READABLE)]);
        nothing_left(&port, &[&object], &account);
    }

    #[test]
    fn the_same_port_object_and_key_bound_twice_gives_two_packets() {
        let (port, account, object) = (port(), Account::new(), Object::new());
        object.bind(&port, &account, 1, 4, READABLE);
        object.bind(&port, &account, 2, 4, READABLE);
        assert_eq!(object.armed(), 2);
        assert_eq!(object.raise(READABLE), 2);
        assert_eq!(drain(&port).len(), 2);
        object.bind(&port, &account, 3, 4, WRITABLE);
        object.bind(&port, &account, 4, 4, WRITABLE);
        assert_eq!(
            refund_all(object.cancel(&port, 4)),
            [3, 4],
            "a cancel left one"
        );
        nothing_left(&port, &[&object], &account);
    }

    // ------------------------------------------------------------ order (shape 15)

    #[test]
    fn packets_leave_in_the_order_they_were_queued() {
        let (port, account) = (port(), Account::new());
        let objects: Vec<Object> = (0..6).map(|_| Object::new()).collect();
        for (key, object) in objects.iter().enumerate() {
            object.bind(&port, &account, key as u32, key as u64, READABLE);
        }
        let order = [3, 1, 4, 0, 5, 2];
        let mut taken = Vec::new();
        for (i, &at) in order.iter().enumerate() {
            assert_eq!(objects[at].raise(READABLE), 1);
            // Takes between queueings, so that both halves of the queue are used.
            if i == 2 {
                let (packet, storage) = poll(&port).unwrap();
                refund(storage);
                taken.push(packet.key);
            }
        }
        // A packet taken out of the middle keeps the others in their order.
        assert_eq!(refund_all(objects[0].cancel(&port, 0)), [0]);
        taken.extend(drain(&port).iter().map(|p| p.key));
        assert_eq!(taken, [3, 1, 4, 5, 2]);
        let all: Vec<&Object> = objects.iter().collect();
        nothing_left(&port, &all, &account);
    }

    // ------------------------------------------------------------ close (shapes 11, 12)

    #[test]
    fn a_close_takes_back_every_binding_armed_or_queued_and_nothing_comes_after() {
        let (port, account) = (port(), Account::new());
        let (first, second, third) = (Object::new(), Object::new(), Object::new());
        first.bind(&port, &account, 1, 1, READABLE);
        first.bind(&port, &account, 2, 2, WRITABLE);
        second.bind(&port, &account, 3, 3, READABLE);
        third.bind(&port, &account, 4, 4, READABLE);
        assert_eq!(second.raise(READABLE), 1);
        assert_eq!(third.close_handle(ME), 1);
        let removed = port.close(&Host);
        assert_eq!(removed.len(), 4);
        assert_eq!(refund_all(removed), [1, 2, 3, 4]);
        assert_eq!(account.back(), [1, 2, 3, 4], "a storage came back twice");
        for object in [&first, &second, &third] {
            assert_eq!(object.armed(), 0, "a binding stayed armed on a closed port");
            assert_eq!(object.raise(READABLE | WRITABLE), 0);
            assert_eq!(object.close_handle(ME), 0);
        }
        assert_eq!(poll(&port).err(), Some(TakeError::Canceled));
        nothing_left(&port, &[&first, &second, &third], &account);
    }

    #[test]
    fn a_close_leaves_a_sources_bindings_for_other_ports() {
        let (account, object) = (Account::new(), Object::new());
        let (port, elsewhere) = (port(), port());
        object.bind(&port, &account, 1, 1, READABLE);
        object.bind(&elsewhere, &account, 2, 2, READABLE);
        object.bind(&port, &account, 3, 3, READABLE);
        assert_eq!(refund_all(port.close(&Host)), [1, 3]);
        assert_eq!(object.armed(), 1);
        assert_eq!(object.raise(READABLE), 1);
        assert_eq!(drain(&elsewhere), [signal(2, READABLE, READABLE)]);
        nothing_left(&elsewhere, &[&object], &account);
    }

    // ------------------------------------------------------------ take

    #[test]
    fn a_wait_through_a_closed_handle_takes_nothing() {
        let (port, account, object) = (port(), Account::new(), Object::new());
        object.bind(&port, &account, 1, 1, READABLE);
        object.raise(READABLE);
        assert_eq!(
            port.take(&Host, u64::MAX, || false).err(),
            Some(TakeError::Canceled)
        );
        assert_eq!(port.queued(&Host), 1, "a cancelled wait took the packet");
        assert_eq!(drain(&port).len(), 1);
        nothing_left(&port, &[&object], &account);
    }

    #[test]
    fn a_wait_with_nothing_queued_times_out() {
        let port = port();
        assert_eq!(poll(&port).err(), Some(TakeError::TimedOut));
        let deadline = now() + 2_000_000;
        assert_eq!(
            port.take(&Host, deadline, || true).err(),
            Some(TakeError::TimedOut)
        );
        assert!(now() >= deadline, "it gave up before its deadline");
    }

    #[test]
    fn a_waiting_thread_gets_a_packet_raised_on_another() {
        let (port, account) = (port(), Account::new());
        let object = Arc::new(Object::new());
        object.bind(&port, &account, 1, 1, READABLE);
        let taker = {
            let port = Arc::clone(&port);
            thread::spawn(move || {
                port.take(&Host, u64::MAX, || true)
                    .map(|(p, s)| (p, refund(s)))
            })
        };
        while port.queued(&Host) == 0 && !taker.is_finished() {
            object.raise(READABLE);
            thread::yield_now();
        }
        let (packet, id) = taker.join().unwrap().unwrap();
        assert_eq!((packet, id), (signal(1, READABLE, READABLE), 1));
        nothing_left(&port, &[&object], &account);
    }

    #[test]
    fn a_wait_on_a_port_that_closes_ends_canceled() {
        let port = port();
        let open = Arc::new(AtomicBool::new(true));
        let taker = {
            let (port, open) = (Arc::clone(&port), Arc::clone(&open));
            thread::spawn(move || port.take(&Host, u64::MAX, || open.load(Relaxed)).err())
        };
        open.store(false, Relaxed);
        assert!(port.close(&Host).is_empty());
        assert_eq!(taker.join().unwrap(), Some(TakeError::Canceled));
    }

    // ------------------------------------------------------------ what the kernel relies on

    /// Reliance 9 and 10: an armed binding holds one count on its port and its index one on the
    /// source; a queued packet holds none; and every count is back once the binding is gone,
    /// whichever way it went.
    #[test]
    fn references_last_only_while_a_binding_is_armed() {
        let (port, account, object) = (port(), Account::new(), Object::new());
        let counts = |port: &Arc<Port<Test>>, object: &Object| {
            (Arc::strong_count(port), Arc::strong_count(&object.source))
        };
        let at_rest = counts(&port, &object);
        // Fired.
        let storage = reserve(&port, &account, 1);
        assert_eq!(
            counts(&port, &object),
            (at_rest.0 + 1, at_rest.1),
            "the reservation's claim"
        );
        assert!(matches!(
            Source::bind(
                &object.source,
                &Host,
                storage,
                Request {
                    owner: ME,
                    key: 1,
                    mask: READABLE
                },
                || true,
                || object.signals()
            ),
            Ok(Bound::Armed)
        ));
        assert_eq!(
            counts(&port, &object),
            (at_rest.0 + 1, at_rest.1 + 1),
            "armed"
        );
        object.raise(READABLE);
        assert_eq!(
            counts(&port, &object),
            at_rest,
            "a queued packet holds a reference"
        );
        drain(&port);
        // Cancelled implicitly, explicitly, and taken by a close.
        object.clear(READABLE);
        object.bind(&port, &account, 2, 2, READABLE);
        object.close_handle(ME);
        assert_eq!(
            counts(&port, &object),
            at_rest,
            "a canceled packet holds a reference"
        );
        drain(&port);
        object.bind(&port, &account, 3, 3, READABLE);
        let removed = object.cancel(&port, 3);
        drop(removed);
        assert_eq!(
            counts(&port, &object),
            at_rest,
            "an explicit cancel kept a reference"
        );
        object.bind(&port, &account, 4, 4, READABLE);
        let removed = port.close(&Host);
        assert_eq!(removed.len(), 1);
        drop(removed);
        assert_eq!(counts(&port, &object), at_rest, "a close kept a reference");
        // The two cancelled storages were dropped, not refunded: count them back by hand.
        account.charged.fetch_sub(2, Relaxed);
        nothing_left(&port, &[&object], &account);
    }

    /// Reliance 7: each storage comes back once, with its own payload, by every path.
    #[test]
    fn storage_comes_back_exactly_once_with_its_payload() {
        let (port, account) = (port(), Account::new());
        let objects: Vec<Object> = (0..4).map(|_| Object::new()).collect();
        let mut id = 0;
        for round in 0..8 {
            for object in &objects {
                id += 1;
                object.bind(&port, &account, id, round, READABLE);
            }
            match round % 4 {
                0 => {
                    for object in &objects {
                        object.raise(READABLE);
                        object.clear(READABLE);
                    }
                }
                1 => {
                    for object in &objects {
                        object.close_handle(ME);
                    }
                }
                2 => {
                    for object in &objects {
                        refund_all(object.cancel(&port, round));
                    }
                }
                _ => {}
            }
            if round % 2 == 1 {
                drain(&port);
            }
        }
        refund_all(port.close(&Host));
        drain(&port);
        assert_eq!(account.back(), (1..=id).collect::<Vec<u32>>());
        let all: Vec<&Object> = objects.iter().collect();
        nothing_left(&port, &all, &account);
    }

    /// Reliance 8, and the lists' drop: no bound of the framework's own, and long lists drop
    /// without recursion.
    #[test]
    fn nothing_bounds_the_bindings_and_long_lists_drop_flat() {
        const MANY: u32 = 20_000;
        let (port, account, object) = (port(), Account::new(), Object::new());
        for id in 0..MANY {
            object.bind(&port, &account, id, u64::from(id), READABLE);
        }
        assert_eq!(object.armed(), MANY as usize);
        assert_eq!(port.armed(&Host), MANY as usize);
        assert_eq!(object.raise(READABLE), MANY as usize);
        assert_eq!(port.queued(&Host), MANY as usize);
        // Dropped whole, without a close: one node at a time.
        account.charged.store(0, Relaxed);
        drop(port);
    }

    /// The index grows only in a reservation, and keeps room for every reservation made.
    #[test]
    fn a_reservation_keeps_room_and_a_bind_never_finds_the_index_full() {
        let (port, account) = (port(), Account::new());
        let objects: Vec<Object> = (0..40).map(|_| Object::new()).collect();
        let reservations: Vec<Reservation<Test>> =
            (0..40).map(|id| reserve(&port, &account, id)).collect();
        for (object, storage) in objects.iter().zip(reservations) {
            let bound = Source::bind(
                &object.source,
                &Host,
                storage,
                Request {
                    owner: ME,
                    key: 0,
                    mask: READABLE,
                },
                || true,
                || 0,
            );
            assert!(matches!(bound, Ok(Bound::Armed)), "a bind found no room");
        }
        // Dropped unused, a reservation gives its room back.
        drop(reserve(&port, &account, 99));
        account.charged.fetch_sub(1, Relaxed);
        for object in &objects {
            object.raise(READABLE);
        }
        drain(&port);
        let all: Vec<&Object> = objects.iter().collect();
        nothing_left(&port, &all, &account);
    }

    /// Reliance 3 under real threads: raising while binding, again and again, loses nothing.
    #[test]
    fn a_signal_raised_while_binding_is_never_lost() {
        let (port, account) = (port(), Account::new());
        for round in 0..2_000u32 {
            let object = Arc::new(Object::new());
            let raiser = {
                let object = Arc::clone(&object);
                thread::spawn(move || object.raise(READABLE))
            };
            object.bind(&port, &account, round, u64::from(round), READABLE);
            raiser.join().unwrap();
            assert_eq!(
                drain(&port),
                [signal(u64::from(round), READABLE, READABLE)],
                "a signal raised against a bind was lost, or made two packets"
            );
        }
        nothing_left(&port, &[], &account);
    }

    // ------------------------------------------------- a port's bound (ADR-0005 Amendment 10, I1)

    /// A reservation that may be refused: its charge comes back on a refusal, as the kernel's
    /// bind gives the binder's back.
    fn try_reserve(
        port: &Arc<Port<Test>>,
        account: &Arc<Account>,
        id: u32,
    ) -> Result<Reservation<Test>, Unreserved> {
        account.charged.fetch_add(1, Relaxed);
        Reservation::<Test>::try_new(
            &Host,
            port,
            Charge {
                account: Arc::clone(account),
                id,
            },
        )
        .inspect_err(|_| {
            account.charged.fetch_sub(1, Relaxed);
        })
    }

    /// The index's capacity, the bindings armed and the reservations in flight.
    fn counts(port: &Port<Test>) -> (usize, usize, usize) {
        let state = port.state.lock();
        (
            state.index.slots.capacity(),
            state.index.used(),
            port.reserved.load(Relaxed),
        )
    }

    #[test]
    fn a_bounded_port_refuses_one_more_than_its_bound_and_takes_nothing() {
        let account = Account::new();
        let port = Arc::new(Port::<Test>::bounded(2));
        let first = try_reserve(&port, &account, 1).unwrap();
        let second = try_reserve(&port, &account, 2).unwrap();
        let before = counts(&port);
        assert_eq!(
            try_reserve(&port, &account, 3).err(),
            Some(Unreserved::Full)
        );
        assert_eq!(counts(&port), before, "a refusal took nothing");
        assert_eq!(account.charged(), 2, "the refused charge came back");
        // A binding armed holds its place as its reservation did.
        let object = Object::new();
        let bound = Source::bind(
            &object.source,
            &Host,
            first,
            Request {
                owner: ME,
                key: 1,
                mask: READABLE,
            },
            || true,
            || object.signals(),
        );
        assert_eq!(bound.ok(), Some(Bound::Armed));
        assert_eq!(counts(&port).1, 1);
        assert_eq!(
            try_reserve(&port, &account, 4).err(),
            Some(Unreserved::Full)
        );
        // A reservation dropped makes room for exactly one.
        drop(second.into_payload());
        account.charged.fetch_sub(1, Relaxed);
        let third = try_reserve(&port, &account, 5).unwrap();
        assert_eq!(
            try_reserve(&port, &account, 6).err(),
            Some(Unreserved::Full)
        );
        // A binding that fires leaves the index: room again.
        assert_eq!(object.raise(READABLE), 1);
        assert_eq!(counts(&port).1, 0);
        let fourth = try_reserve(&port, &account, 7).unwrap();
        drop((third.into_payload(), fourth.into_payload()));
        account.charged.fetch_sub(2, Relaxed);
        drain(&port);
        nothing_left(&port, &[], &account);
    }

    #[test]
    fn a_port_bounded_at_zero_accepts_no_binding() {
        let account = Account::new();
        let port = Arc::new(Port::<Test>::bounded(0));
        assert_eq!(
            try_reserve(&port, &account, 1).err(),
            Some(Unreserved::Full)
        );
        assert_eq!(counts(&port), (0, 0, 0));
        assert_eq!(account.charged(), 0);
        assert_eq!(Port::<Test>::index_at_bound(0), (0, 0));
    }

    /// The rule the kernel-wide bound counts a port's index by (`Port::index_at_bound`), driven
    /// on the real index: for each bound, a long run of reservations, binds, fires, explicit
    /// cancels, implicit cancels (the binding's handle closed), dropped reservations and, for
    /// about half the bounds, the port's close at a random step, in a seeded order, never takes
    /// the index past the rule's most; it grows only from a capacity under the bound (the old
    /// block the rule counts), and the bindings armed and reserved never pass the bound.
    #[test]
    fn a_bounded_port_s_index_stays_within_its_rule() {
        let mut seed: u64 = 0x5EED_0057;
        let mut next = |n: usize| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((seed >> 33) as usize) % n
        };
        let bounds = (1..=40).chain([63, 64, 65, 100, 255, 256, 257, 500]);
        let mut closes = 0;
        for bound in bounds {
            let account = Account::new();
            let port = Arc::new(Port::<Test>::bounded(bound));
            let (most, old_most) = Port::<Test>::index_at_bound(bound);
            let steps = (bound * 12).max(400);
            // Past the run for about half the bounds: their ports never close.
            let close_at = next(steps * 2);
            let mut held: Vec<Reservation<Test>> = Vec::new();
            let mut armed: Vec<(Object, u64)> = Vec::new();
            let mut closed = false;
            let mut id = 0;
            for step in 0..steps {
                let (before, _, _) = counts(&port);
                if step == close_at {
                    // The close takes the index and every binding armed, each one's storage
                    // handed back; nothing is queued, since every packet is drained as it comes.
                    assert_eq!(refund_all(port.close(&Host)).len(), armed.len());
                    armed.clear();
                    closed = true;
                    closes += 1;
                    assert_eq!(counts(&port).0, 0, "bound {bound}: the close left an index");
                }
                match next(6) {
                    // Reserve, sometimes several in a row. A closed port makes no room, and is
                    // never full.
                    0 | 1 => {
                        for _ in 0..=next(3) {
                            id += 1;
                            match try_reserve(&port, &account, id) {
                                Ok(r) => held.push(r),
                                Err(e) => {
                                    assert!(!closed, "bound {bound}: a closed port was full");
                                    assert_eq!(e, Unreserved::Full);
                                }
                            }
                        }
                    }
                    // Bind a reservation held: armed while the port is open, refused once closed.
                    2 => {
                        if let Some(r) = held.pop() {
                            let object = Object::new();
                            let key = u64::from(id);
                            let bound_now = Source::bind(
                                &object.source,
                                &Host,
                                r,
                                Request {
                                    owner: ME,
                                    key,
                                    mask: READABLE,
                                },
                                || true,
                                || object.signals(),
                            );
                            match bound_now {
                                Ok(how) => {
                                    assert!(!closed, "bound {bound}: a closed port took a binding");
                                    assert_eq!(how, Bound::Armed);
                                    armed.push((object, key));
                                }
                                Err((why, r)) => {
                                    assert!(closed && matches!(why, Refused::PortClosed));
                                    drop(r.into_payload());
                                    account.charged.fetch_sub(1, Relaxed);
                                }
                            }
                        }
                    }
                    // Fire a binding armed.
                    3 => {
                        if !armed.is_empty() {
                            let (object, _) = armed.swap_remove(next(armed.len()));
                            assert_eq!(object.raise(READABLE), 1);
                            drain(&port);
                        }
                    }
                    // Cancel a binding armed, explicitly or as its handle closes.
                    4 => {
                        if !armed.is_empty() {
                            let (object, key) = armed.swap_remove(next(armed.len()));
                            if next(2) == 0 {
                                assert_eq!(refund_all(object.cancel(&port, key)).len(), 1);
                            } else {
                                assert_eq!(object.close_handle(ME), 1);
                                drain(&port);
                            }
                        }
                    }
                    // Drop a reservation held.
                    _ => {
                        if !held.is_empty() {
                            let r = held.swap_remove(next(held.len()));
                            drop(r.into_payload());
                            account.charged.fetch_sub(1, Relaxed);
                        }
                    }
                }
                let (capacity, used, reserved) = counts(&port);
                assert!(
                    used + reserved <= bound,
                    "bound {bound}: {used} armed, {reserved} reserved"
                );
                assert!(
                    capacity <= most,
                    "bound {bound}: {capacity} slots, the rule says {most}"
                );
                if capacity > before {
                    assert!(
                        before <= old_most,
                        "bound {bound}: grew from {before} slots"
                    );
                }
            }
            for r in held {
                drop(r.into_payload());
                account.charged.fetch_sub(1, Relaxed);
            }
            for (object, _) in armed {
                object.raise(READABLE);
            }
            drain(&port);
            nothing_left(&port, &[], &account);
            // The index and the block a growth makes, or that block and the one it replaced:
            // never more than the figure counts (T1).
            assert!(
                port.most_index_slots() <= most + old_most,
                "bound {bound}: {} slots of index blocks at once, the figure counts {}",
                port.most_index_slots(),
                most + old_most
            );
        }
        assert!(closes > 0, "no port was closed");
    }

    /// Reservations from several threads at once never pass the bound between them. In each
    /// round eight threads each try once and hold what they get until all have tried, so exactly
    /// as many as the bound hold a place, whatever the order; a bound that missed reservations in
    /// flight would let all eight through, every round (KB-12). Real threads explore few orders:
    /// this is a smoke test, and the race itself is the Loom model's
    /// (`ports_two_reservations_race_for_a_bound_of_one`; the independent review of TASKS 57,
    /// finding T2).
    #[test]
    fn reservations_from_several_threads_never_pass_the_bound() {
        const BOUND: usize = 3;
        const THREADS: usize = 8;
        const ROUNDS: u32 = 200;
        let account = Account::new();
        let port = Arc::new(Port::<Test>::bounded(BOUND));
        let (tried, dropped) = (
            std::sync::Barrier::new(THREADS),
            std::sync::Barrier::new(THREADS),
        );
        let made = AtomicU32::new(0);
        // Rounds that did not make exactly `BOUND`: recorded, not asserted, so that no thread
        // stops at a barrier the others wait at.
        let wrong = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for t in 0..THREADS {
                let (port, account, tried, dropped, made, wrong) =
                    (&port, &account, &tried, &dropped, &made, &wrong);
                scope.spawn(move || {
                    for round in 0..ROUNDS {
                        let id = round * 100 + u32::try_from(t).unwrap();
                        let got = match try_reserve(port, account, id) {
                            Ok(r) => {
                                made.fetch_add(1, Relaxed);
                                Some(r)
                            }
                            Err(e) => {
                                assert_eq!(e, Unreserved::Full);
                                None
                            }
                        };
                        if tried.wait().is_leader() {
                            let n = made.swap(0, Relaxed);
                            if n as usize != BOUND {
                                wrong.lock().unwrap().push((round, n));
                            }
                        }
                        if let Some(r) = got {
                            drop(r.into_payload());
                            account.charged.fetch_sub(1, Relaxed);
                        }
                        dropped.wait();
                    }
                });
            }
        });
        let wrong = wrong.into_inner().unwrap();
        assert!(
            wrong.is_empty(),
            "{} of {ROUNDS} rounds made other than {BOUND} reservations, the first (round, made) \
             {:?}",
            wrong.len(),
            wrong.first()
        );
        let (most, old) = Port::<Test>::index_at_bound(BOUND);
        assert!(port.most_index_slots() <= most + old);
        assert_eq!(counts(&port).2, 0);
        nothing_left(&port, &[], &account);
    }

    /// A port full of bindings armed refuses a bind on an object whose signals already meet its
    /// mask, though that bind would queue its packet at once and arm nothing. Its reservation
    /// holds a place from before the signals are read: they are read under the source's lock, at
    /// the bind, so that no raise between a look and the bind is missed (ADR-0005 I1, as row K7
    /// reads it; the independent review of TASKS 57, finding T3).
    #[test]
    fn a_full_port_refuses_a_bind_that_would_queue_at_once() {
        let account = Account::new();
        let port = Arc::new(Port::<Test>::bounded(2));
        let armed = [Object::new(), Object::new()];
        for (key, object) in (1..).zip(&armed) {
            assert_eq!(
                object.bind(&port, &account, u32::try_from(key).unwrap(), key, READABLE),
                Bound::Armed
            );
        }
        let signalled = Object::new();
        signalled.signals.store(READABLE, Relaxed);
        assert_eq!(
            try_reserve(&port, &account, 3).err(),
            Some(Unreserved::Full)
        );
        assert_eq!(account.charged(), 2, "the refused charge came back");
        // Once one fires, the same bind queues at once and takes no slot.
        assert_eq!(armed[0].raise(READABLE), 1);
        assert_eq!(
            signalled.bind(&port, &account, 4, 4, READABLE),
            Bound::Queued
        );
        assert_eq!(counts(&port).1, 1, "a bind that queued took a slot");
        assert_eq!(armed[1].raise(READABLE), 1);
        assert_eq!(drain(&port).len(), 3);
        nothing_left(&port, &[], &account);
    }
}
