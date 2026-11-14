//! Count quotas (ADR-0005 P11), tested on the host: how many objects of a kind a process may
//! hold at once, set by its creator.
//!
//! - **A count** is charged before the thing it counts is made and refunded once that thing is
//!   gone. A charge at the quota is `limit` and takes nothing. Charges from several processors
//!   never pass the quota between them: the check and the increment are one atomic step.
//! - **A quota of 0 allows nothing.** A creator gives each kind to a process deliberately, so a
//!   process whose creator said nothing gets nothing ([`ObjectLimits::NONE`]).
//! - **Channels:** a channel is charged to the process that created it, once, until the channel
//!   is destroyed.
//! - **Events, ports and bindings** are counted apart, each against its own quota
//!   ([`ObjectCounts`]; ADR-0005 Amendment 7, F7). An event or a port is charged to the process
//!   that created it until the object is destroyed. A binding is charged to the process that
//!   bound it, from the bind until its one packet has left the port. Task S3-6's calls charge
//!   them; until they are built nothing does.
//!
//! The kernel-wide bound (ADR-0005 Amendment 8, G5; task S4-3) is here too:
//!
//! - **A process's limits** ([`Limits`]) are worth a figure in bytes ([`Figure`]): the most
//!   kernel memory and frames they let the process make the kernel hold, kind by kind, from what
//!   each kind costs as the kernel's build measures it ([`Costs`]). Every allocation is counted
//!   as the kernel heap rounds it ([`footprint`]), and a table that grows at the most it holds
//!   while it grows ([`growing`]).
//! - **A purse** ([`Purse`]) holds bytes that payments are taken from. The kernel's own, the
//!   root, is sized once at boot: what its free frames hold, less a reserve for its own use
//!   ([`reserve_for`]). A [`Payment`] is taken before a process is made and given back when it
//!   is dropped, once.
#![forbid(unsafe_code)]

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use origin_abi::Error;

use crate::heap::{MAX_BLOCK, MIN_BLOCK, PAGE};
use crate::mapping::Rate;

/// How many of one kind are held, against the most that may be.
#[derive(Debug)]
pub struct Count {
    held: AtomicUsize,
    max: usize,
}

impl Count {
    /// Nothing held, and at most `max` at once.
    pub const fn new(max: usize) -> Self {
        Self {
            held: AtomicUsize::new(0),
            max,
        }
    }

    /// One more, or [`Error::Limit`] at the quota, with nothing taken.
    pub fn charge(&self) -> Result<(), Error> {
        self.held
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                // The sum is taken only under the quota, so it cannot overflow. `then_some`
                // would add first: on a count wrapped to the top of its range, a build that
                // checks overflow would panic where the charge must be refused.
                if n < self.max { Some(n + 1) } else { None }
            })
            .map(|_| ())
            .map_err(|_| Error::Limit)
    }

    /// One that was charged here is gone.
    ///
    /// A refund of something never charged is a kernel bug. It fails closed: the count wraps
    /// to the top of its range, so every later charge is refused.
    pub fn refund(&self) {
        self.held.fetch_sub(1, Ordering::AcqRel);
    }

    /// How many are charged now.
    pub fn held(&self) -> usize {
        self.held.load(Ordering::Acquire)
    }
}

/// How many events, ports and bindings a process may hold at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectLimits {
    /// Events it created that still exist.
    pub events: usize,
    /// Ports it created that still exist.
    pub ports: usize,
    /// Bindings it made whose packet has not left its port.
    pub bindings: usize,
    /// The most bindings armed at once on any one port it creates, reservations in flight among
    /// them (ADR-0005 Amendment 10, I1): each port it creates is bounded by it, and its figure
    /// pays for each port's index at that bound. Zero: its ports accept no binding.
    pub bindings_per_port: usize,
}

impl ObjectLimits {
    /// No events, no ports, no bindings, and no binding on any port.
    pub const NONE: Self = Self {
        events: 0,
        ports: 0,
        bindings: 0,
        bindings_per_port: 0,
    };
}

/// A process's events, ports and bindings, each counted against its own quota.
#[derive(Debug)]
pub struct ObjectCounts {
    /// Events it created that still exist.
    pub events: Count,
    /// Ports it created that still exist.
    pub ports: Count,
    /// Bindings it made whose packet has not left its port.
    pub bindings: Count,
    /// The bound of each port it creates (I1).
    pub bindings_per_port: usize,
}

impl ObjectCounts {
    /// Nothing held, under `limits`.
    pub const fn new(limits: ObjectLimits) -> Self {
        Self {
            events: Count::new(limits.events),
            ports: Count::new(limits.ports),
            bindings: Count::new(limits.bindings),
            bindings_per_port: limits.bindings_per_port,
        }
    }
}

/// The most slots a port's index holds at a bound of `bound` bindings armed and reserved, and
/// the most its old block holds while it grows: the framework's rule
/// (`origin_sync::ports::Port::index_at_bound`, held to the real index by its host test), copied
/// here so that a figure can be made on the host; the kernel's build checks that the two agree.
/// `first` is the index's first size (`Port::FIRST_INDEX_SLOTS`).
#[must_use]
pub const fn index_at_bound(bound: usize, first: usize) -> (usize, usize) {
    if bound == 0 {
        return (0, 0);
    }
    let most = bound.saturating_mul(2).saturating_sub(1);
    (if most > first { most } else { first }, bound - 1)
}

/// What one port's index at its creator's bound makes the heap hold, its new block and its old
/// one together, each rounded as the heap rounds it (ADR-0005 I1). `None` on overflow.
#[must_use]
pub const fn port_index(bound: usize, slot: Element, first: usize) -> Option<u64> {
    let (most, old) = index_at_bound(bound, first);
    let (Some(new), Some(old)) = (most.checked_mul(slot.size), old.checked_mul(slot.size)) else {
        return None;
    };
    match (footprint(new, slot.align), footprint(old, slot.align)) {
        (Some(new), Some(old)) => new.checked_add(old),
        _ => None,
    }
}

/// What a process may commit and map, and how fast it may change its mappings (P11, C7;
/// `THREAT_MODEL_ADDRESS_SPACES.md` T15, T16). [`MemoryLimits::NONE`] allows nothing: a creator
/// gives memory to a process deliberately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryLimits {
    /// Bytes of memory objects it may have committed at once.
    pub committed: usize,
    /// Whole mappings its address space may hold.
    pub mappings: usize,
    /// Its budget of shootdowns and interrupts for unmap and protect.
    pub changes: Rate,
}

impl MemoryLimits {
    /// No memory objects, no mappings and no changes.
    pub const NONE: Self = Self {
        committed: 0,
        mappings: 0,
        changes: Rate {
            burst: 0,
            per_second: 1,
        },
    };
}

/// A process's quotas (P11), each kind given by its creator deliberately and none filled in for
/// it (ADR-0005 G6): at most `handles` entries in its table, `channels` live channels,
/// `message_bytes` of kernel heap in messages it has queued, its memory, its events, ports and
/// bindings, and `threads` threads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Entries in its handle table.
    pub handles: usize,
    /// Channels it created that still exist.
    pub channels: usize,
    /// Bytes of kernel heap held by the messages it has queued and nobody has read.
    pub message_bytes: usize,
    /// May it use `DEBUG_REPORT` and `DEBUG_WRITE`? The ABI reserves them for developer
    /// builds: an ordinary process that calls them gets `NOT_SUPPORTED`, so it cannot hold
    /// the serial line (at 115200 baud, a line for many milliseconds) with interrupts off.
    pub debug_calls: bool,
    /// Its memory objects and mappings.
    pub memory: MemoryLimits,
    /// Its events, ports and bindings. [`ObjectLimits::NONE`] allows none.
    pub objects: ObjectLimits,
    /// Threads started in it and not yet reaped, its first included (G5's "a kernel stack and
    /// an FPU area for each thread"). Only the kernel's launch path starts threads in step 4
    /// (G2); this is the count ADR-0005 G3's draft names, kept inside the kernel, with no call.
    pub threads: usize,
}

/// What the kernel heap sets aside for one allocation of `size` bytes aligned to `align`, as
/// `origin_frame::heap` rounds it: a block of the next power of two from 16 to 2,048 bytes, or a
/// run of whole 4 KiB frames for anything larger or aligned above 2,048 bytes. An allocation of
/// no bytes is never made (Rust's collections and `ObjectRef` make none), so it costs nothing.
/// `None` for an alignment that is not a power of two, or a size whose rounding overflows: the
/// heap would refuse such an allocation, and no figure is made from it.
#[must_use]
pub const fn footprint(size: usize, align: usize) -> Option<u64> {
    if size == 0 {
        return Some(0);
    }
    if !align.is_power_of_two() {
        return None;
    }
    let (size, align) = (size as u64, align as u64);
    let mut want = if size > align { size } else { align };
    if want < MIN_BLOCK {
        want = MIN_BLOCK;
    }
    match want.checked_next_power_of_two() {
        Some(block) if block <= MAX_BLOCK => Some(block),
        // The heap gives a run of frames for the size itself; its alignment is the run's start.
        _ => match size.div_ceil(PAGE).checked_mul(PAGE) {
            Some(bytes) => Some(bytes),
            None => None,
        },
    }
}

/// One element of a table that grows (a `Vec` or a `VecDeque`): its size and alignment, as
/// `size_of` and `align_of` give them in the kernel's build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Element {
    /// Its size in bytes.
    pub size: usize,
    /// Its alignment in bytes.
    pub align: usize,
}

/// The fewest elements Rust's collections give a table the first time it grows: 8 for one-byte
/// elements, 4 for elements up to 1 KiB, 1 for larger ones. A rule of the standard library, not
/// of the language; the host tests hold it against `Vec` as the pinned compiler builds it.
const fn first_capacity(size: usize) -> usize {
    if size == 1 {
        8
    } else if size <= 1024 {
        4
    } else {
        1
    }
}

/// The most a table of `element`s whose length never passes `most` makes the heap hold at once,
/// as it grows one element or several at a time (`try_reserve`, `try_reserve_exact`, `push`).
///
/// A table grows only when what it needs passes its capacity, and then to at least twice that
/// capacity or what it needs, whichever is more. So its capacity never passes the larger of
/// [`first_capacity`], `most`, and `2 * (most - 1)`. While it grows, the kernel's allocator
/// makes the new block before it frees the old one (`GlobalAlloc::realloc` as the kernel leaves
/// it), and the old one held at most `most - 1` elements: the two are counted together. `None`
/// if the arithmetic overflows.
#[must_use]
pub const fn growing(most: usize, element: Element) -> Option<u64> {
    if most == 0 || element.size == 0 {
        return Some(0);
    }
    let doubled = match (most - 1).checked_mul(2) {
        Some(n) => n,
        None => return None,
    };
    let mut capacity = first_capacity(element.size);
    if most > capacity {
        capacity = most;
    }
    if doubled > capacity {
        capacity = doubled;
    }
    let (Some(new), Some(old)) = (
        capacity.checked_mul(element.size),
        (most - 1).checked_mul(element.size),
    ) else {
        return None;
    };
    match (footprint(new, element.align), footprint(old, element.align)) {
        (Some(new), Some(old)) => new.checked_add(old),
        _ => None,
    }
}

/// What each kind of thing a process may hold costs the kernel, as the kernel's build measures
/// it (`process::costs`). Each fixed cost is a sum of [`footprint`]s, one for each allocation
/// the kernel makes for one of that kind; each table that grows is the element it keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Costs {
    /// Each process, whatever its limits: its own objects, and its address space's page tables
    /// up to the bound one space may have, with the list that holds them.
    pub process: u64,
    /// The tables a process's handles are kept in, each growing to its `handles` quota.
    pub handle_tables: [Element; 2],
    /// Each channel it creates.
    pub channel: u64,
    /// Each memory object, besides its pages. An object holds at least one page, so a process
    /// can have at most one for each page of its `committed` quota.
    pub memory_object: u64,
    /// Each committed page, besides its frame: what the object keeps for it.
    pub memory_page: u64,
    /// The table a process's mappings are recorded in, growing to its `mappings` quota.
    pub mapping_table: Element,
    /// The record an address space keeps for each page it maps, in each of its two tables of
    /// them (`mapped` and `spare`).
    pub page_record: Element,
    /// The most pages one address space can map: its tables' bound allows no more.
    pub user_pages: usize,
    /// Each event.
    pub event: u64,
    /// Each port, besides its index.
    pub port: u64,
    /// One slot of a port's index, and the index's first size: each port's index is counted at
    /// its creator's `bindings_per_port` ([`port_index`]; ADR-0005 I1).
    pub index_slot: Element,
    /// The index's first size (`Port::FIRST_INDEX_SLOTS`).
    pub index_first: usize,
    /// Each binding: its own two allocations. The slots of the port's index are the port's
    /// creator's to pay (I1), not the binder's.
    pub binding: u64,
    /// Each thread: its kernel stack, its FPU area and its kernel objects.
    pub thread: u64,
    /// Each thread's system call: the most one call can make the kernel hold at once that no
    /// quota has charged yet, or charges no longer ([`call_temporaries`]). A thread has one call
    /// in flight at most, and a call that is preempted keeps what it holds, so each thread is
    /// counted for the largest (the independent review of task S4-3, finding B1).
    pub call: u64,
    /// Each page a launch maps into a process on a frame of its own (its image, its stack, its
    /// start record): the frame, and the page's share of the space's two tables of page records.
    /// Not in a figure: the creator pays it besides (the coordinating session's ruling (d)).
    pub page: u64,
    /// Each page a launch maps on a frame every process shares (`origin.core`): its share of the
    /// space's two tables of page records.
    pub shared_page: u64,
}

/// One page's share of a table of `element`s that grows a page at a time: the most the table
/// holds while it grows, [`growing`], is never more than eight elements' bytes a page for
/// elements of 16 bytes to 1 KiB, up to the most pages a space can map (the host test
/// `a_page_s_share_of_a_record_table_is_eight_elements`).
#[must_use]
pub const fn eight_a_page(element: Element) -> Option<u64> {
    (element.size as u64).checked_mul(8)
}

/// What the largest system call can make the kernel hold at once, beside what any quota charges
/// while it holds it: a message's bytes, up to `max_bytes`, in one exact allocation, and beside
/// them one list of up to `max_handles` of the larger of `lists`' elements. Two calls do this
/// (`ipc::call_temporaries` names them): `channel_write`, between the copy of its bytes and the
/// charge to the sender's `message_bytes`, which D5's order puts after the handles are checked;
/// and `channel_read`, between the refund of the message's charge and the copy out. `None` on
/// overflow.
#[must_use]
pub const fn call_temporaries(
    max_bytes: usize,
    max_handles: usize,
    lists: &[Element],
) -> Option<u64> {
    let Some(bytes) = footprint(max_bytes, 1) else {
        return None;
    };
    let mut largest = 0;
    let mut i = 0;
    while i < lists.len() {
        let element = lists[i];
        let Some(size) = max_handles.checked_mul(element.size) else {
            return None;
        };
        let Some(list) = footprint(size, element.align) else {
            return None;
        };
        if list > largest {
            largest = list;
        }
        i += 1;
    }
    bytes.checked_add(largest)
}

/// Whether a process may start one more thread: not once it has ended, and not while it has as
/// many threads started and not yet reaped as its limits give (G5's "a kernel stack and an FPU
/// area for each thread"). The count is of threads not yet *reaped*, not of threads running: a
/// thread that has ended still holds its stack and FPU area until its processor reaps it.
#[must_use]
pub const fn may_start(ended: bool, unreaped: usize, threads: usize) -> bool {
    !ended && unreaped < threads
}

/// A set of limits' figure, kind by kind, in bytes (G5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Figure {
    /// The process itself.
    pub process: u64,
    /// Its handle table at its quota.
    pub handles: u64,
    /// Its channels.
    pub channels: u64,
    /// Its queued messages: charged as the heap rounds them already (`ipc::message_cost`).
    pub messages: u64,
    /// Its committed memory: the frames, the objects and their lists.
    pub committed: u64,
    /// Its mapping records.
    pub mappings: u64,
    /// Its events.
    pub events: u64,
    /// Its ports, each with its index at its `bindings_per_port` (I1).
    pub ports: u64,
    /// Its bindings.
    pub bindings: u64,
    /// Its threads.
    pub threads: u64,
}

impl Figure {
    /// Every kind's part added up, or `None` if the sum overflows.
    #[must_use]
    pub const fn total(&self) -> Option<u64> {
        let parts = [
            self.process,
            self.handles,
            self.channels,
            self.messages,
            self.committed,
            self.mappings,
            self.events,
            self.ports,
            self.bindings,
            self.threads,
        ];
        let mut sum: u64 = 0;
        let mut i = 0;
        while i < parts.len() {
            sum = match sum.checked_add(parts[i]) {
                Some(s) => s,
                None => return None,
            };
            i += 1;
        }
        Some(sum)
    }
}

/// `count` of a thing that costs `each`, or `None` on overflow.
const fn times(count: usize, each: u64) -> Option<u64> {
    (count as u64).checked_mul(each)
}

/// What `mappings` whole mappings let a process make its address space record: the table of
/// its mappings, and, once it may map anything at all, the space's two tables of page records
/// at the most pages the space can map. A single mapping of a shared object can cover that many
/// pages, so the count of mappings does not bound them; the space's tables do. A process given
/// no mappings maps nothing itself: the pages its launcher maps (its image, its stack and its
/// start record) are the launcher's own, and not in its figure.
const fn mapping_records(mappings: usize, costs: &Costs) -> Option<u64> {
    let Some(table) = growing(mappings, costs.mapping_table) else {
        return None;
    };
    if mappings == 0 {
        return Some(table);
    }
    match growing(costs.user_pages, costs.page_record) {
        Some(records) => match records.checked_mul(2) {
            Some(both) => table.checked_add(both),
            None => None,
        },
        None => None,
    }
}

impl Limits {
    /// What these limits let a process make the kernel hold, kind by kind, at `costs`; `None` if
    /// any part overflows, and then no process is made with them.
    #[must_use]
    pub const fn parts(&self, costs: &Costs) -> Option<Figure> {
        let [slots, free] = costs.handle_tables;
        let pages = self.memory.committed.div_ceil(PAGE as usize);
        let (
            Some(slot_table),
            Some(free_table),
            Some(channels),
            Some(objects),
            Some(page_entries),
            Some(mappings),
            Some(events),
            Some(ports),
            Some(bindings),
            Some(threads),
        ) = (
            growing(self.handles, slots),
            growing(self.handles, free),
            times(self.channels, costs.channel),
            times(pages, costs.memory_object),
            times(pages, costs.memory_page),
            mapping_records(self.memory.mappings, costs),
            times(self.objects.events, costs.event),
            // No port, no index to count: a bound with no port to bound costs nothing, however
            // large (the independent review of TASKS 57, finding T12).
            if self.objects.ports == 0 {
                Some(0)
            } else {
                match port_index(
                    self.objects.bindings_per_port,
                    costs.index_slot,
                    costs.index_first,
                ) {
                    Some(index) => match costs.port.checked_add(index) {
                        Some(each) => times(self.objects.ports, each),
                        None => None,
                    },
                    None => None,
                }
            },
            times(self.objects.bindings, costs.binding),
            match costs.thread.checked_add(costs.call) {
                Some(each) => times(self.threads, each),
                None => None,
            },
        )
        else {
            return None;
        };
        // The frames of the committed bytes, a page for each page begun.
        let Some(frames) = (pages as u64).checked_mul(PAGE) else {
            return None;
        };
        let (Some(handles), Some(committed)) = (
            slot_table.checked_add(free_table),
            match frames.checked_add(objects) {
                Some(n) => n.checked_add(page_entries),
                None => None,
            },
        ) else {
            return None;
        };
        Some(Figure {
            process: costs.process,
            handles,
            channels,
            messages: self.message_bytes as u64,
            committed,
            mappings,
            events,
            ports,
            bindings,
            threads,
        })
    }

    /// These limits' figure: [`Limits::parts`] added up, or `None` on overflow.
    #[must_use]
    pub const fn figure(&self, costs: &Costs) -> Option<u64> {
        match self.parts(costs) {
            Some(parts) => parts.total(),
            None => None,
        }
    }
}

/// The reserve the kernel keeps for its own use when it sizes its root (G5): the larger of
/// 64 MiB and an eighth of the free memory. A hypothesis, like a budget (CLAUDE.md §53, decision
/// item 115; budget B19, whose model lists what it takes): the kernel's own use after the root
/// is sized, its drivers, its caches and its own threads; and what the root does not count, as
/// row O11 of the system-call threat model lists it, the slab table's slack, the frames a stalled
/// shootdown keeps and the kernel stacks kept or retired after their threads are reaped among
/// it. None of it is measured yet.
#[must_use]
pub const fn reserve_for(free: u64) -> u64 {
    const FLOOR: u64 = 64 << 20;
    let eighth = free / 8;
    if eighth > FLOOR { eighth } else { FLOOR }
}

/// Bytes that payments are taken from: the kernel's root (G5).
///
/// It holds nothing until it is sized, once, so a payment asked for before then is refused. A
/// payment and its check are one atomic step, so payments from several processors never take
/// the balance below zero; a [`Payment`] gives back what it took once, when it is dropped, so the
/// balance never passes the size.
#[derive(Debug)]
pub struct Purse {
    sized: AtomicBool,
    size: AtomicU64,
    left: AtomicU64,
    /// The least that was ever left: what the boot's busiest moment had paid out.
    lowest: AtomicU64,
}

impl Purse {
    /// A purse that holds nothing and is not sized.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sized: AtomicBool::new(false),
            size: AtomicU64::new(0),
            left: AtomicU64::new(0),
            lowest: AtomicU64::new(0),
        }
    }

    /// Gives the purse `bytes`, once: `false` if it was sized already, and then nothing changes.
    pub fn size_once(&self, bytes: u64) -> bool {
        if self.sized.swap(true, Ordering::AcqRel) {
            return false;
        }
        self.size.store(bytes, Ordering::Release);
        self.lowest.store(bytes, Ordering::Release);
        // Last: until now every payment found nothing left and was refused.
        self.left.store(bytes, Ordering::Release);
        true
    }

    /// Takes `bytes` out of the purse, or [`Error::Limit`] if fewer are left, with nothing taken.
    pub fn pay(&'static self, bytes: u64) -> Result<Payment, Error> {
        let before = self
            .left
            .try_update(Ordering::AcqRel, Ordering::Acquire, |left| {
                left.checked_sub(bytes)
            })
            .map_err(|_| Error::Limit)?;
        self.lowest.fetch_min(before - bytes, Ordering::AcqRel);
        Ok(Payment {
            purse: self,
            bytes: AtomicU64::new(bytes),
        })
    }

    /// What it was sized with; 0 before then.
    pub fn size(&self) -> u64 {
        self.size.load(Ordering::Acquire)
    }

    /// What is left in it now.
    pub fn left(&self) -> u64 {
        self.left.load(Ordering::Acquire)
    }

    /// The least that was ever left in it since it was sized.
    pub fn lowest(&self) -> u64 {
        self.lowest.load(Ordering::Acquire)
    }
}

impl Default for Purse {
    fn default() -> Self {
        Self::new()
    }
}

/// Bytes taken out of a [`Purse`]: given back, exactly, when this is dropped. Not `Clone` and
/// not `Copy`, so it is given back once. It can take more from its purse ([`Payment::add`]) and
/// give part back early ([`Payment::give_back`]); what it holds is always what its purse is
/// short by on its account.
#[derive(Debug)]
#[must_use = "a payment dropped at once gives its bytes straight back"]
pub struct Payment {
    purse: &'static Purse,
    bytes: AtomicU64,
}

impl Payment {
    /// The bytes it holds now.
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Acquire)
    }

    /// Takes `bytes` more out of the same purse, or [`Error::Limit`] if fewer are left there,
    /// with nothing taken.
    pub fn add(&self, bytes: u64) -> Result<(), Error> {
        let more = self.purse.pay(bytes)?;
        // The new payment's bytes move into this one, which gives them back when it is dropped.
        let moved = more.bytes.swap(0, Ordering::AcqRel);
        self.bytes.fetch_add(moved, Ordering::AcqRel);
        Ok(())
    }

    /// Gives back up to `bytes` of what it holds now, and returns how many it gave back.
    pub fn give_back(&self, bytes: u64) -> u64 {
        let given = match self
            .bytes
            .try_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                Some(held - held.min(bytes))
            }) {
            Ok(held) | Err(held) => held.min(bytes),
        };
        self.purse.left.fetch_add(given, Ordering::AcqRel);
        given
    }
}

impl Drop for Payment {
    fn drop(&mut self) {
        let held = self.bytes.swap(0, Ordering::AcqRel);
        self.purse.left.fetch_add(held, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Charges `count` until it refuses, and returns how many it took.
    fn fill(count: &Count) -> usize {
        let mut taken = 0;
        while count.charge().is_ok() {
            taken += 1;
        }
        taken
    }

    #[test]
    fn a_count_stops_at_its_quota_and_a_refused_charge_takes_nothing() {
        let count = Count::new(2);
        assert_eq!(count.charge(), Ok(()));
        assert_eq!(count.charge(), Ok(()));
        assert_eq!(count.held(), 2);
        assert_eq!(count.charge(), Err(Error::Limit));
        assert_eq!(count.charge(), Err(Error::Limit));
        assert_eq!(count.held(), 2);
    }

    #[test]
    fn a_refund_makes_room_for_exactly_one_more() {
        let count = Count::new(3);
        assert_eq!(fill(&count), 3);
        count.refund();
        assert_eq!(count.held(), 2);
        assert_eq!(count.charge(), Ok(()));
        assert_eq!(count.charge(), Err(Error::Limit));
        for _ in 0..3 {
            count.refund();
        }
        assert_eq!(count.held(), 0);
        assert_eq!(fill(&count), 3);
    }

    #[test]
    fn a_quota_of_zero_allows_nothing() {
        let count = Count::new(0);
        assert_eq!(count.charge(), Err(Error::Limit));
        assert_eq!(count.held(), 0);

        let none = ObjectCounts::new(ObjectLimits::NONE);
        assert_eq!(none.events.charge(), Err(Error::Limit));
        assert_eq!(none.ports.charge(), Err(Error::Limit));
        assert_eq!(none.bindings.charge(), Err(Error::Limit));
    }

    #[test]
    fn the_largest_quota_does_not_overflow() {
        let count = Count::new(usize::MAX);
        assert_eq!(count.charge(), Ok(()));
        assert_eq!(count.held(), 1);
    }

    #[test]
    fn a_refund_too_many_refuses_every_later_charge() {
        let count = Count::new(4);
        count.refund();
        assert_eq!(count.charge(), Err(Error::Limit));
        // Even under the largest quota there is.
        let widest = Count::new(usize::MAX);
        widest.refund();
        assert_eq!(widest.charge(), Err(Error::Limit));
    }

    #[test]
    fn each_kind_is_counted_against_its_own_quota() {
        let counts = ObjectCounts::new(ObjectLimits {
            events: 1,
            ports: 2,
            bindings: 3,
            bindings_per_port: 3,
        });
        assert_eq!(fill(&counts.events), 1);
        assert_eq!(fill(&counts.ports), 2);
        assert_eq!(fill(&counts.bindings), 3);

        // Room made in one kind is room in that kind only.
        counts.bindings.refund();
        assert_eq!(counts.events.charge(), Err(Error::Limit));
        assert_eq!(counts.ports.charge(), Err(Error::Limit));
        assert_eq!(counts.bindings.charge(), Ok(()));
        assert_eq!(
            (
                counts.events.held(),
                counts.ports.held(),
                counts.bindings.held()
            ),
            (1, 2, 3)
        );
    }

    /// Several threads charge and refund one count at once. None of them may ever see more
    /// held than the quota, and what they hold at the end is what they kept.
    #[test]
    fn charges_from_several_threads_never_pass_the_quota() {
        const THREADS: usize = 8;
        const ROUNDS: usize = 20_000;
        const QUOTA: usize = 3;

        let count = Count::new(QUOTA);
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                scope.spawn(|| {
                    for _ in 0..ROUNDS {
                        if count.charge().is_ok() {
                            assert!(count.held() <= QUOTA);
                            count.refund();
                        }
                    }
                });
            }
        });
        assert_eq!(count.held(), 0);

        // With nothing refunded, the threads get the quota between them and no more.
        let kept = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                scope.spawn(|| {
                    for _ in 0..ROUNDS {
                        if count.charge().is_ok() {
                            kept.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
        });
        assert_eq!(kept.load(Ordering::Relaxed), QUOTA);
        assert_eq!(count.held(), QUOTA);
    }

    // ---------------------------------------------------------- the kernel-wide bound (G5)

    use crate::heap::{Heap, PageSource, Slab};

    /// Frames for a heap on the host: addresses only, handed out upwards and never reused, so
    /// the heap's bookkeeping is all that is measured.
    struct Frames {
        next: u64,
    }

    impl PageSource for Frames {
        fn allocate_pages(&mut self, count: u64, align: u64) -> Option<u64> {
            let step = align.max(1) * PAGE;
            let at = self.next.next_multiple_of(step);
            self.next = at + count * PAGE;
            Some(at)
        }

        fn free_pages(&mut self, _addr: u64, _count: u64) -> bool {
            true
        }
    }

    /// What the real heap counts in use for one allocation of `size` bytes aligned to `align`.
    fn measured(size: u64, align: u64) -> Option<u64> {
        let mut table = [Slab::ZERO; 4];
        let mut heap = Heap::new(&mut table);
        let mut frames = Frames { next: 1 << 30 };
        heap.allocate(&mut frames, size, align)?;
        Some(heap.stats().bytes_in_use)
    }

    #[test]
    fn the_footprint_is_what_the_heap_counts_in_use() {
        let aligns = [1u64, 2, 4, 8, 16, 32, 64, 128, 256, 1024, 2048, 4096, 8192];
        let mut sizes: Vec<u64> = (1..=5000).collect();
        sizes.extend((5000..70_000).step_by(97));
        sizes.extend([1 << 16, (1 << 16) + 1, 1 << 20, (1 << 20) + 4095]);
        for &align in &aligns {
            for &size in &sizes {
                assert_eq!(
                    footprint(size as usize, align as usize),
                    measured(size, align),
                    "size {size} aligned to {align}"
                );
            }
        }
        // No allocation is made for nothing, and an alignment that is no power of two makes none.
        assert_eq!(footprint(0, 8), Some(0));
        assert_eq!(footprint(8, 3), None);
        assert_eq!(footprint(usize::MAX, 8), None);
    }

    /// Grows a `Vec` of `value`s to `most`, `step` at a time (the last step shorter), and returns
    /// the most its two blocks held at once while it grew: its new capacity's bytes and the old
    /// one's, as the kernel's `realloc` holds both.
    fn grown<T: Copy>(value: T, most: usize, step: usize) -> u64 {
        let (size, align) = (core::mem::size_of::<T>(), core::mem::align_of::<T>());
        let block = |capacity: usize| footprint(capacity * size, align).unwrap();
        let mut worst = 0;
        let mut table: Vec<T> = Vec::new();
        while table.len() < most {
            let before = table.capacity();
            let want = step.min(most - table.len());
            table.try_reserve(want).unwrap();
            if table.capacity() != before {
                worst = worst.max(block(table.capacity()) + block(before));
            }
            for _ in 0..want {
                table.push(value);
            }
        }
        worst
    }

    #[test]
    fn a_growing_table_never_holds_more_than_its_bound() {
        for most in 1..=700 {
            for step in [1, 2, 3, 7, 64] {
                let one = Element { size: 1, align: 1 };
                let eight = Element { size: 8, align: 8 };
                let forty = Element { size: 40, align: 8 };
                let big = Element {
                    size: 1032,
                    align: 8,
                };
                assert!(
                    grown(0u8, most, step) <= growing(most, one).unwrap(),
                    "{most} {step}"
                );
                assert!(
                    grown(0u64, most, step) <= growing(most, eight).unwrap(),
                    "{most} {step}"
                );
                assert!(
                    grown([0u64; 5], most, step) <= growing(most, forty).unwrap(),
                    "{most} {step}"
                );
                assert!(
                    grown([0u64; 129], most, step) <= growing(most, big).unwrap(),
                    "{most} {step}"
                );
            }
        }
        // The first growth gives the standard library's first capacity, which the bound assumes.
        for (size, first) in [(1usize, 8usize), (8, 4), (1024, 4), (1032, 1)] {
            assert_eq!(first_capacity(size), first);
        }
        let mut bytes: Vec<u8> = Vec::new();
        bytes.try_reserve(1).unwrap();
        assert_eq!(bytes.capacity(), 8);
        let mut words: Vec<u64> = Vec::new();
        words.try_reserve(1).unwrap();
        assert_eq!(words.capacity(), 4);
        let mut large: Vec<[u64; 129]> = Vec::new();
        large.try_reserve(1).unwrap();
        assert_eq!(large.capacity(), 1);
        // Nothing for nothing; overflow makes no figure.
        assert_eq!(growing(0, Element { size: 8, align: 8 }), Some(0));
        assert_eq!(growing(usize::MAX, Element { size: 8, align: 8 }), None);
    }

    /// Costs with a distinct value in every field, so each kind's part can be told apart.
    const COSTS: Costs = Costs {
        process: 1_000_003,
        handle_tables: [
            Element { size: 40, align: 8 },
            Element { size: 4, align: 4 },
        ],
        channel: 3_584,
        memory_object: 64,
        memory_page: 80,
        mapping_table: Element { size: 32, align: 8 },
        page_record: Element { size: 24, align: 8 },
        user_pages: 29 * 512,
        event: 512,
        port: 768,
        index_slot: Element { size: 32, align: 8 },
        index_first: 4,
        binding: 192,
        thread: 17_920,
        call: 66_560,
        page: 4096 + 512,
        shared_page: 512,
    };

    /// Limits that give nothing at all.
    const NOTHING: Limits = Limits {
        handles: 0,
        channels: 0,
        message_bytes: 0,
        debug_calls: false,
        memory: MemoryLimits::NONE,
        objects: ObjectLimits::NONE,
        threads: 0,
    };

    #[test]
    fn a_figure_is_its_kinds_added_kind_by_kind() {
        // Given nothing, a process costs only itself.
        let none = NOTHING.parts(&COSTS).unwrap();
        assert_eq!(
            none,
            Figure {
                process: COSTS.process,
                ..Figure::default()
            }
        );
        assert_eq!(NOTHING.figure(&COSTS), Some(COSTS.process));

        // Each kind alone adds its own part, and only that part.
        let alone = |limits: Limits| limits.figure(&COSTS).unwrap() - COSTS.process;
        let [slots, free] = COSTS.handle_tables;
        assert_eq!(
            alone(Limits {
                handles: 64,
                ..NOTHING
            }),
            growing(64, slots).unwrap() + growing(64, free).unwrap()
        );
        assert_eq!(
            alone(Limits {
                channels: 32,
                ..NOTHING
            }),
            32 * COSTS.channel
        );
        assert_eq!(
            alone(Limits {
                message_bytes: 1 << 20,
                ..NOTHING
            }),
            1 << 20
        );
        // Committed bytes: a frame for each page begun, an object for each page at most, and what
        // an object keeps for each page.
        let committed = |bytes: usize| Limits {
            memory: MemoryLimits {
                committed: bytes,
                ..MemoryLimits::NONE
            },
            ..NOTHING
        };
        assert_eq!(
            alone(committed(PAGE as usize)),
            PAGE + COSTS.memory_object + COSTS.memory_page
        );
        assert_eq!(
            alone(committed(10 * PAGE as usize + 1)),
            11 * (PAGE + COSTS.memory_object + COSTS.memory_page)
        );
        // One mapping lets a process map up to what its space's tables allow, in two tables.
        let mappings = |n: usize| Limits {
            memory: MemoryLimits {
                mappings: n,
                ..MemoryLimits::NONE
            },
            ..NOTHING
        };
        let records = 2 * growing(COSTS.user_pages, COSTS.page_record).unwrap();
        assert_eq!(
            alone(mappings(1)),
            growing(1, COSTS.mapping_table).unwrap() + records
        );
        assert_eq!(
            alone(mappings(9)),
            growing(9, COSTS.mapping_table).unwrap() + records
        );
        // The change rate holds no memory.
        let rate = Limits {
            memory: MemoryLimits {
                changes: Rate {
                    burst: 1_000,
                    per_second: 1_000_000,
                },
                ..MemoryLimits::NONE
            },
            ..NOTHING
        };
        assert_eq!(alone(rate), 0);
        let objects = |events, ports, bindings, bindings_per_port| Limits {
            objects: ObjectLimits {
                events,
                ports,
                bindings,
                bindings_per_port,
            },
            ..NOTHING
        };
        let index = |bound| port_index(bound, COSTS.index_slot, COSTS.index_first).unwrap();
        assert_eq!(alone(objects(3, 0, 0, 0)), 3 * COSTS.event);
        // A port whose bound is zero has no index; one with a bound pays for its index at it.
        assert_eq!(alone(objects(0, 2, 0, 0)), 2 * COSTS.port);
        assert_eq!(alone(objects(0, 2, 0, 3)), 2 * (COSTS.port + index(3)));
        // The bound costs nothing without a port to bound, however large (T12).
        assert_eq!(alone(objects(0, 0, 0, 1_000)), 0);
        assert_eq!(alone(objects(0, 0, 0, usize::MAX)), 0);
        assert!(objects(1, 1, 0, usize::MAX).figure(&COSTS).is_none());
        // A binding pays its own two allocations, and no slot of the index.
        assert_eq!(alone(objects(0, 0, 5, 0)), 5 * COSTS.binding);
        assert_eq!(
            alone(Limits {
                threads: 4,
                ..NOTHING
            }),
            4 * (COSTS.thread + COSTS.call)
        );
        assert_eq!(
            alone(Limits {
                debug_calls: true,
                ..NOTHING
            }),
            0
        );

        // Everything at once is the sum of the parts.
        let all = Limits {
            handles: 64,
            channels: 32,
            message_bytes: 1 << 20,
            debug_calls: true,
            memory: MemoryLimits {
                committed: 16 * PAGE as usize,
                mappings: 3,
                changes: Rate {
                    burst: 64,
                    per_second: 1_000,
                },
            },
            objects: ObjectLimits {
                events: 3,
                ports: 2,
                bindings: 4,
                bindings_per_port: 2,
            },
            threads: 2,
        };
        let parts = all.parts(&COSTS).unwrap();
        assert_eq!(
            parts.total(),
            Some(
                COSTS.process
                    + alone(Limits {
                        handles: 64,
                        ..NOTHING
                    })
                    + 32 * COSTS.channel
                    + (1 << 20)
                    + alone(committed(16 * PAGE as usize))
                    + alone(mappings(3))
                    + alone(objects(3, 2, 4, 2))
                    + 2 * (COSTS.thread + COSTS.call)
            )
        );
        assert_eq!(all.figure(&COSTS), parts.total());
    }

    #[test]
    fn limits_whose_figure_overflows_have_none() {
        assert_eq!(
            Limits {
                handles: usize::MAX,
                ..NOTHING
            }
            .figure(&COSTS),
            None
        );
        assert_eq!(
            Limits {
                channels: usize::MAX,
                ..NOTHING
            }
            .figure(&COSTS),
            None
        );
        assert_eq!(
            Limits {
                threads: usize::MAX,
                ..NOTHING
            }
            .figure(&COSTS),
            None
        );
        let huge = |committed| Limits {
            memory: MemoryLimits {
                committed,
                ..MemoryLimits::NONE
            },
            ..NOTHING
        };
        assert_eq!(huge(usize::MAX).figure(&COSTS), None);
        // Parts that each fit but whose sum does not.
        let wide = Limits {
            message_bytes: usize::MAX,
            channels: 1,
            ..NOTHING
        };
        assert_eq!(wide.figure(&COSTS), None);
        // A cost so large that one of a kind overflows.
        let dear = Costs {
            event: u64::MAX,
            ..COSTS
        };
        assert_eq!(
            Limits {
                objects: ObjectLimits {
                    events: 2,
                    ports: 0,
                    bindings: 0,
                    bindings_per_port: 0,
                },
                ..NOTHING
            }
            .figure(&dear),
            None
        );
    }

    /// A purse of its own for one test.
    fn purse() -> &'static Purse {
        Box::leak(Box::new(Purse::new()))
    }

    #[test]
    fn a_purse_not_sized_pays_nothing_and_is_sized_once() {
        let root = purse();
        assert_eq!(root.pay(1).err(), Some(Error::Limit));
        assert_eq!((root.size(), root.left()), (0, 0));
        assert!(root.size_once(4096));
        assert!(!root.size_once(1 << 30));
        assert_eq!(
            (root.size(), root.left(), root.lowest()),
            (4096, 4096, 4096)
        );
    }

    #[test]
    fn paying_and_repaying_are_to_the_unit() {
        let root = purse();
        assert!(root.size_once(10_000));
        let first = root.pay(3_333).unwrap();
        assert_eq!(first.bytes(), 3_333);
        assert_eq!(root.left(), 6_667);
        // Exactly what is left is paid, and leaves nothing.
        let rest = root.pay(6_667).unwrap();
        assert_eq!(root.left(), 0);
        // One byte more is refused, and takes nothing.
        assert_eq!(root.pay(1).err(), Some(Error::Limit));
        assert_eq!(root.left(), 0);
        drop(first);
        assert_eq!(root.left(), 3_333);
        // A payment over what is left is refused whole, not paid in part.
        assert_eq!(root.pay(3_334).err(), Some(Error::Limit));
        assert_eq!(root.left(), 3_333);
        drop(rest);
        assert_eq!(root.left(), 10_000);
        assert_eq!(root.lowest(), 0);
        // A payment of nothing changes nothing.
        let nothing = root.pay(0).unwrap();
        assert_eq!(root.left(), 10_000);
        drop(nothing);
        assert_eq!(root.left(), 10_000);
    }

    /// Several threads pay and repay from one purse at once. None ever sees the balance below
    /// zero or above the size, and once every payment is dropped the purse is whole again.
    #[test]
    fn payments_from_several_threads_never_take_the_balance_below_zero() {
        const THREADS: usize = 8;
        const ROUNDS: usize = 20_000;
        let root = purse();
        assert!(root.size_once(1_000));
        std::thread::scope(|scope| {
            for t in 0..THREADS {
                scope.spawn(move || {
                    for r in 0..ROUNDS {
                        let bytes = ((t * 131 + r * 7) % 400) as u64 + 1;
                        if let Ok(payment) = root.pay(bytes) {
                            assert!(root.left() <= 1_000 - bytes);
                            drop(payment);
                        }
                        assert!(root.left() <= 1_000);
                    }
                });
            }
        });
        assert_eq!(root.left(), 1_000);

        // With nothing repaid, the threads get the purse between them and no more.
        let kept = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                scope.spawn(|| {
                    for _ in 0..ROUNDS {
                        if let Ok(payment) = root.pay(7) {
                            kept.lock().unwrap().push(payment);
                        }
                    }
                });
            }
        });
        let kept = kept.into_inner().unwrap();
        assert_eq!(kept.len(), 1_000 / 7);
        assert_eq!(root.left(), 1_000 % 7);
        drop(kept);
        assert_eq!(root.left(), 1_000);
    }

    #[test]
    fn the_reserve_is_an_eighth_of_free_memory_and_never_under_64_mib() {
        assert_eq!(reserve_for(0), 64 << 20);
        assert_eq!(reserve_for(480 << 20), 64 << 20);
        assert_eq!(reserve_for(512 << 20), 64 << 20);
        assert_eq!(reserve_for(1 << 30), 128 << 20);
        assert_eq!(reserve_for(64 << 30), 8 << 30);
        assert_eq!(reserve_for(u64::MAX), u64::MAX / 8);
    }

    /// A memory object keeps its frames in one list of exactly its pages (`MemoryObject::create`,
    /// `try_reserve_exact`), each entry a pointer of 8 bytes. `process::costs` counts each page's
    /// entry twice over, so the heap's rounding of the whole list must never pass that.
    #[test]
    fn an_objects_list_of_frames_costs_at_most_twice_its_entries() {
        for pages in 1..=(1 << 20) {
            let list = footprint(8 * pages, 8).unwrap();
            assert!(list <= 16 * pages as u64, "{pages} pages: {list} bytes");
        }
    }

    #[test]
    fn a_payment_takes_more_and_gives_part_back_to_the_byte() {
        let root = purse();
        assert!(root.size_once(100_000));
        let paid = root.pay(10_000).unwrap();
        paid.add(4_096).unwrap();
        assert_eq!((paid.bytes(), root.left()), (14_096, 85_904));
        // More than is left is refused whole, and the payment is as it was.
        assert_eq!(paid.add(85_905), Err(Error::Limit));
        assert_eq!((paid.bytes(), root.left()), (14_096, 85_904));
        // Part given back early.
        assert_eq!(paid.give_back(96), 96);
        assert_eq!((paid.bytes(), root.left()), (14_000, 86_000));
        // Never more than it holds.
        assert_eq!(paid.give_back(1 << 40), 14_000);
        assert_eq!((paid.bytes(), root.left()), (0, 100_000));
        paid.add(1).unwrap();
        drop(paid);
        assert_eq!(root.left(), 100_000);
    }

    /// The coordinating session's ruling (c): a process's payment is held by its account and
    /// given back when the account goes, which is after every object charged to it, not when the
    /// process object goes. **A model**, on `std::sync::Arc`: the kernel's `Account`, `Process`
    /// and channel cannot be built on the host, so this shows the shape the kernel uses (each
    /// holds the account through a counted reference, and the payment is the account's field),
    /// not the kernel's own types. The boot proof's clause "the refund waits" shows the kernel's,
    /// for a channel (the independent review of task S4-3, finding B9).
    #[test]
    fn the_refund_waits_for_an_object_charged_to_the_account_that_outlives_the_process() {
        use std::sync::Arc;
        struct Account {
            _paid: Payment,
        }
        struct Process {
            _account: Arc<Account>,
        }
        struct Channel {
            _account: Arc<Account>,
        }
        let root = purse();
        assert!(root.size_once(1 << 20));
        let account = Arc::new(Account {
            _paid: root.pay(300_000).unwrap(),
        });
        let channel = Channel {
            _account: account.clone(),
        };
        let process = Process { _account: account };
        assert_eq!(root.left(), (1 << 20) - 300_000);
        drop(process);
        // The process object is gone; the channel it made is not, and nor is the payment.
        assert_eq!(root.left(), (1 << 20) - 300_000);
        drop(channel);
        assert_eq!(root.left(), 1 << 20);
    }

    #[test]
    fn a_page_s_share_of_a_record_table_is_eight_elements() {
        for size in (16..=1024).step_by(8) {
            let element = Element { size, align: 8 };
            let share = eight_a_page(element).unwrap();
            for pages in 1..=(29 * 512 + 1) {
                assert!(growing(pages, element).unwrap() <= pages as u64 * share);
            }
        }
    }

    /// The review's finding B1: a thread's part of a figure holds the largest call's temporaries,
    /// a message's bytes at the most a message may have and the larger list beside them, as the
    /// ABI's limits set them; each thread counts them once.
    #[test]
    fn a_thread_s_part_holds_the_largest_call_s_temporaries() {
        use origin_abi::limits::{MAX_MESSAGE_BYTES, MAX_MESSAGE_HANDLES};
        let gone = Element { size: 40, align: 8 };
        let received = Element { size: 16, align: 8 };
        let call =
            call_temporaries(MAX_MESSAGE_BYTES, MAX_MESSAGE_HANDLES, &[gone, received]).unwrap();
        let bytes = footprint(MAX_MESSAGE_BYTES, 1).unwrap();
        assert_eq!(bytes, 65_536);
        assert!(call >= bytes + footprint(MAX_MESSAGE_HANDLES * 40, 8).unwrap());
        assert!(call >= bytes + footprint(MAX_MESSAGE_HANDLES * 16, 8).unwrap());
        assert_eq!(
            call,
            bytes + footprint(MAX_MESSAGE_HANDLES * 40, 8).unwrap()
        );
        // No list at all still counts the bytes.
        assert_eq!(
            call_temporaries(MAX_MESSAGE_BYTES, MAX_MESSAGE_HANDLES, &[]),
            Some(bytes)
        );
        // In a figure: each thread its stack and objects, and one call's temporaries.
        let costs = Costs { call, ..COSTS };
        let one = Limits {
            threads: 1,
            ..NOTHING
        };
        assert_eq!(
            one.figure(&costs).unwrap() - costs.process,
            COSTS.thread + call
        );
        assert!(one.figure(&costs).unwrap() - costs.process >= COSTS.thread + bytes);
        assert_eq!(call_temporaries(usize::MAX, 1, &[]), None);
        assert_eq!(call_temporaries(1, usize::MAX, &[gone]), None);
    }

    /// The review's finding B4: the threads count refuses one more than the limit, counts threads
    /// not yet reaped, and nothing once the process has ended.
    #[test]
    fn a_start_past_the_threads_given_is_refused() {
        // A limit of one: the first, then no second while it is unreaped, then one again once
        // it is reaped (in a process that has not ended, which a step-4 program's never is).
        assert!(may_start(false, 0, 1));
        assert!(!may_start(false, 1, 1));
        assert!(may_start(false, 0, 1));
        // A limit of two.
        assert!(may_start(false, 1, 2));
        assert!(!may_start(false, 2, 2));
        // None given: none at all.
        assert!(!may_start(false, 0, 0));
        // Once ended, never, whatever is left.
        assert!(!may_start(true, 0, 1));
        assert!(!may_start(true, 0, usize::MAX));
    }

    /// ADR-0005 I1: a port's index at its creator's bound, as the framework's rule has it, rounded
    /// as the heap rounds it, its new block and its old one together.
    #[test]
    fn a_port_s_index_is_counted_at_its_creator_s_bound() {
        let slot = Element { size: 32, align: 8 };
        assert_eq!(index_at_bound(0, 4), (0, 0));
        assert_eq!(port_index(0, slot, 4), Some(0));
        // At a bound of one or two the index never grows past its first four slots.
        assert_eq!(index_at_bound(1, 4), (4, 0));
        assert_eq!(index_at_bound(2, 4), (4, 1));
        assert_eq!(port_index(1, slot, 4), Some(128));
        assert_eq!(port_index(2, slot, 4), Some(128 + 32));
        // Above that, twice the bound less one, and the old block under the bound.
        assert_eq!(index_at_bound(3, 4), (5, 2));
        assert_eq!(index_at_bound(100, 4), (199, 99));
        assert_eq!(
            port_index(100, slot, 4),
            Some(footprint(199 * 32, 8).unwrap() + footprint(99 * 32, 8).unwrap())
        );
        // Never less than the bound's own slots, so every slot armed or reserved is counted.
        for bound in 1..5_000 {
            let (most, old) = index_at_bound(bound, 4);
            assert!(most >= bound && old < bound);
        }
        assert_eq!(port_index(usize::MAX, slot, 4), None);
    }
}
