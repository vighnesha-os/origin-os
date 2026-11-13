//! Kernel processes (M4; CLAUDE.md §28 user address spaces, §29 processes and isolation;
//! ADR-0005 P1, P11).
//!
//! A process owns its address space, its handle table, the user frames it maps and the
//! threads the kernel started in it:
//! - Its own PML4, whose lower half holds only its own pages and whose kernel half (entries
//!   256–511) points at the kernel's tables, allocated once at boot (ADR-0004 H2): so the
//!   kernel is mapped the same in every address space, whenever the mapping was made.
//! - Its page tables come from the frame allocator. Their [`TableMemory`] answers only for
//!   frames it allocated itself, so the paging library can never be led into a frame the
//!   process does not own.
//! - Its handle table is the only way it names kernel objects (P1).
//!
//! A process is an object a handle can name, and it may outlive what it ran:
//! - When its last thread ends, by exit or by fault, its handles close at once and its peers
//!   see that.
//! - Its address space goes when the last of its threads has been reaped, whoever still holds
//!   a handle to the process: each thread holds a reference to the space through the scheduler
//!   and gives it up when it is reaped, so by then no processor runs in it, and each that ran
//!   one has loaded another CR3, which flushes its translations (no PCIDs yet).
//! - Only then is `TERMINATED` raised (P11's order: handles, memory, signal): whoever sees it
//!   knows the process's handles have closed and that neither it nor any of its threads holds
//!   the address space any more. A thread is counted before it takes its reference, so one being
//!   started cannot keep the space past `TERMINATED`. A kernel helper that took a reference of
//!   its own through [`Process::memory`] (`root`, `write_frame`, `frame_of`, `translate`, and the
//!   boot proof that reads a process's memory after it has ended) keeps the space until it lets
//!   go. The scheduler reaps a thread once its processor has left it: on a processor kept busy
//!   by a program that never enters the kernel, that waits until something gives the processor
//!   up, as the reaping of any thread's stack does.
//! - A thread is an object too ([`ThreadObject`]): `TERMINATED` on a thread is raised as soon
//!   as it has ended, without waiting to be reaped.
//!
//! Every object here is allocated fallibly ([`ObjectRef::try_new`]): a process or thread that
//! cannot be created is an error for its creator, never a stopped kernel.
//!
//! Only the kernel creates processes and maps their code, data and stacks, before any of their
//! threads runs. After that a space changes through the memory calls (`memory_object`): a
//! process's whole mappings of memory objects are recorded here, and the memory it commits is
//! charged to its account (P11, C7).
//!
//! Every process is paid for before it is made (ADR-0005 G5; task S4-3). Its limits are worth a
//! figure, the most kernel memory and frames they let it make the kernel hold ([`costs`] measures
//! each kind in this build), and the kernel's root ([`ROOT`], sized once at boot by
//! [`size_root`]) pays it before anything of the process exists. A process the root cannot pay
//! for is refused with `LIMIT`, with nothing made. The payment is held by the process's account
//! and given back when the account goes: after the process, its threads, its address space and
//! every object charged to it are gone. Its threads are counted against its limits too: a
//! thread started and not yet reaped holds a kernel stack and an FPU area, which the figure pays
//! for one each.
#![forbid(unsafe_code)]

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::channel::Identity;
use crate::handles::HandleTable;
use crate::mapping::{Budget, Mappings};
use crate::memory_object::MemoryObject;
use crate::paging::{Access, MapError, Translation};
pub(crate) use crate::quota::{Limits, MemoryLimits, ObjectLimits};
use crate::quota::{
    Costs, Count, Element, ObjectCounts, Payment, Purse, eight_a_page, footprint, growing,
};
use crate::runqueue::QueueError;
use origin_abi::{Error, Handle, signals};
use origin_frame::objref::ObjectRef;
use origin_frame::sync::SpinLock;
use origin_frame::sync::ports::Source;
use origin_frame::userspace::{ChangeError, UserFrame, UserSpace};

use crate::cpu::{CpuIndex, without_interrupts};
use crate::ipc::{self, Object};
use crate::port::Ports;
use crate::sched;
use crate::wait::{KernelThreads, WaitQueue};

/// Values a process's report log keeps, for the boot proofs.
const REPORTS: usize = 128;

/// What a process may hold, set by its creator (P11). Objects are charged here even while
/// they are in flight in a message, so a process cannot escape its table quota by putting
/// channels in the kernel's queues. Queued messages are charged too, to the process that
/// sent them, by the memory they hold.
pub(crate) struct Account {
    /// Channels this process created that still exist.
    channels: Count,
    /// Bytes of kernel heap held by messages this process has queued and nobody has read.
    queued_bytes: AtomicUsize,
    max_queued_bytes: usize,
    /// Bytes of memory objects this process created that still exist (C7): charged when one is
    /// made, refunded when its last handle and mapping are gone.
    committed: AtomicUsize,
    max_committed: usize,
    /// The events and ports this process created that still exist, and the bindings it made
    /// whose packet has not left its port.
    objects: ObjectCounts,
    /// What the root paid for this process (G5): its figure, and each page its launch mapped for
    /// it (the coordinating session's ruling (d)). Given back when the account is dropped, which
    /// is after the process and everything charged here are gone.
    paid: Payment,
    /// Its limits' figure, the first part of `paid`.
    figure: u64,
    /// Bytes paid ahead for pages its launch has not mapped yet (`launch::load_program`); what
    /// is left once its start record is written goes back ([`Process::settle_launch`]).
    ahead: AtomicU64,
    /// Pages its launch mapped, each paid for.
    launch_pages: AtomicU64,
}

impl Account {
    /// Its limits' figure, as the root paid it (G5).
    pub(crate) const fn figure(&self) -> u64 {
        self.figure
    }

    /// What the root paid for the pages its launch mapped, and how many they are.
    pub(crate) fn launch_paid(&self) -> (u64, u64) {
        let bytes = self.paid.bytes() - self.figure - self.ahead.load(Ordering::Acquire);
        (bytes, self.launch_pages.load(Ordering::Acquire))
    }

    /// Pays `bytes` for a page the launch maps: out of what was paid ahead if that covers it,
    /// otherwise out of the root now. `true` if it came from what was paid ahead.
    fn pay_page(&self, bytes: u64) -> Result<bool, Error> {
        let ahead = self
            .ahead
            .try_update(Ordering::AcqRel, Ordering::Acquire, |left| {
                left.checked_sub(bytes)
            })
            .is_ok();
        if !ahead {
            self.paid.add(bytes)?;
        }
        self.launch_pages.fetch_add(1, Ordering::AcqRel);
        Ok(ahead)
    }

    /// A page paid for with [`Account::pay_page`] was not mapped after all: `bytes` go back where
    /// they came from.
    fn unpay_page(&self, bytes: u64, ahead: bool) {
        self.launch_pages.fetch_sub(1, Ordering::AcqRel);
        if ahead {
            self.ahead.fetch_add(bytes, Ordering::AcqRel);
        } else {
            self.paid.give_back(bytes);
        }
    }

    /// One more channel, or [`Error::Limit`].
    pub(crate) fn charge_channel(&self) -> Result<(), Error> {
        self.channels.charge()
    }

    /// A channel charged to this account is gone.
    pub(crate) fn refund_channel(&self) {
        self.channels.refund();
    }

    /// `bytes` more of queued-message memory, or [`Error::Limit`].
    pub(crate) fn charge_queued(&self, bytes: usize) -> Result<(), Error> {
        self.queued_bytes
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(bytes)
                    .filter(|&total| total <= self.max_queued_bytes)
            })
            .map(|_| ())
            .map_err(|_| Error::Limit)
    }

    /// Queued-message memory charged to this account has gone: its message was read, or the
    /// end it was queued for closed.
    pub(crate) fn refund_queued(&self, bytes: usize) {
        self.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
    }

    /// Bytes of queued-message memory charged to this account now.
    pub(crate) fn queued_bytes(&self) -> usize {
        self.queued_bytes.load(Ordering::Acquire)
    }

    /// `bytes` more of committed memory, or [`Error::Limit`] past the quota (C7).
    pub(crate) fn charge_committed(&self, bytes: usize) -> Result<(), Error> {
        self.committed
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(bytes)
                    .filter(|&total| total <= self.max_committed)
            })
            .map(|_| ())
            .map_err(|_| Error::Limit)
    }

    /// A memory object charged to this account is gone, and its `bytes` with it.
    pub(crate) fn refund_committed(&self, bytes: usize) {
        self.committed.fetch_sub(bytes, Ordering::AcqRel);
    }

    /// Bytes of committed memory charged to this account now.
    pub(crate) fn committed(&self) -> usize {
        self.committed.load(Ordering::Acquire)
    }

    /// Its counts of events, ports and bindings, each against its own quota (F7).
    pub(crate) fn objects(&self) -> &ObjectCounts {
        &self.objects
    }
}

/// What ended the process's last thread, for the boot proofs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ending {
    /// `exit(code)`.
    Exited(u64),
    /// A fault: its vector, its error code and, for a page fault, CR2 (0 for any other
    /// vector, where CR2 holds some earlier page fault's address).
    Faulted {
        vector: u64,
        error: u64,
        address: u64,
    },
    /// A system call the per-call pin refused (ADR-0005 B2 and Amendment 8, G1): the thread
    /// was stopped as a fault stops one, and no status went back. The record is the number
    /// that was in RAX and the address of the `syscall` instruction that was run. It becomes
    /// the event on the process's exception channel once such channels exist (P10).
    Refused {
        /// The call number asked for.
        number: u64,
        /// Where the `syscall` instruction was.
        address: u64,
    },
}

pub(crate) use crate::pin::CallOrigin;

/// The values a process reported through `DEBUG_REPORT`, oldest first.
struct ReportLog {
    values: [u64; REPORTS],
    count: usize,
}

/// The signal an object raises when it has ended, who waits for it, and the bindings armed on
/// it (P9, P11, C9).
pub(crate) struct Termination {
    raised: AtomicBool,
    waiters: WaitQueue,
    source: Source<Ports>,
}

impl Termination {
    const fn new() -> Self {
        Self {
            raised: AtomicBool::new(false),
            waiters: WaitQueue::new(),
            source: Source::new(),
        }
    }

    /// The object's signals: `TERMINATED` once it has ended, and nothing before.
    pub(crate) fn signals(&self) -> u32 {
        if self.raised.load(Ordering::Acquire) {
            signals::TERMINATED
        } else {
            0
        }
    }

    /// The threads waiting on the object.
    pub(crate) const fn waiters(&self) -> &WaitQueue {
        &self.waiters
    }

    /// The bindings armed on the object.
    pub(crate) const fn source(&self) -> &Source<Ports> {
        &self.source
    }

    /// Whether the object has ended.
    pub(crate) fn raised(&self) -> bool {
        self.raised.load(Ordering::Acquire)
    }

    /// Raises the signal: the state first, then the wake-ups and the deliveries, as every waker
    /// does (`wait`, and the ports' protocol).
    fn raise(&self) {
        self.raised.store(true, Ordering::Release);
        ipc::note_cross_cpu_wakes(self.waiters.wake_all());
        self.source.deliver(&KernelThreads, || self.signals());
    }
}

/// A thread, as a handle names it (ADR-0005 P3). It owns nothing the thread ran on: a handle to
/// a thread that has ended keeps only this.
pub(crate) struct ThreadObject {
    termination: Termination,
}

impl ThreadObject {
    /// A thread not yet started: the launch path names it in a start record before it runs.
    pub(crate) const fn new() -> Self {
        Self {
            termination: Termination::new(),
        }
    }

    /// Its `TERMINATED` signal.
    pub(crate) const fn termination(&self) -> &Termination {
        &self.termination
    }
}

/// What a running user thread stands on, held by the scheduler from the thread's start until
/// it is reaped: dropping the last of these is what frees the address space.
pub(crate) struct UserContext {
    /// The thread's process.
    pub(crate) process: ObjectRef<Process>,
    /// The address space it runs in.
    pub(crate) space: ObjectRef<UserSpace>,
    /// The thread, as handles name it.
    pub(crate) thread: ObjectRef<ThreadObject>,
}

/// Where a process's threads are, under one lock so that "the last one has ended" and "no more
/// may start" are one decision.
struct Lifecycle {
    /// Threads started that have not ended.
    running: u32,
    /// Threads started that have not been reaped.
    unreaped: u32,
    /// What ended the most recent thread, once one has.
    latest: Option<Ending>,
    /// Its last thread has ended: no thread may start in it again.
    ended: bool,
}

/// The principal the boot proofs give the processes they make. The kernel gives principals
/// no meaning (ADR-0005 C1): this one only marks a process as a boot proof's.
pub(crate) const BOOT_PROOF_PRINCIPAL: u64 = 0xB007_0000_0000_0000;

/// The next process ID: 0 is the kernel, and an ID is never used twice (ADR-0005 C1).
static NEXT_PROCESS_ID: AtomicU64 = AtomicU64::new(1);

/// The ID the next process made will have: for the bound's proof that a refused launch drew none.
pub(crate) fn next_id() -> u64 {
    NEXT_PROCESS_ID.load(Ordering::Relaxed)
}

/// The kernel's root (ADR-0005 G5): what every process is paid out of while the kernel is the
/// only creator (step 4). It holds nothing until [`size_root`] sizes it, so a process made
/// before then is refused.
pub(crate) static ROOT: Purse = Purse::new();

/// Sizes [`ROOT`] once, from `free` bytes of free frames: what they hold, less the reserve the
/// kernel keeps for its own use ([`crate::quota::reserve_for`]). Returns the root and the
/// reserve, or `None` if the root was sized already, which leaves it as it was.
pub(crate) fn size_root(free: u64) -> Option<(u64, u64)> {
    let reserve = crate::quota::reserve_for(free);
    let root = free.saturating_sub(reserve);
    ROOT.size_once(root).then_some((root, reserve))
}

/// One allocation of a size and an alignment, as the heap rounds it.
fn block((size, align): (usize, usize)) -> Option<u64> {
    footprint(size, align)
}

/// One element of a growing table.
const fn element((size, align): (usize, usize)) -> Element {
    Element { size, align }
}

/// What each kind a process's limits name costs the kernel, as this build measures it (G5):
/// each allocation the kernel makes for one of a kind, rounded as the heap rounds it, and each
/// table that grows by its element. `None` if an allocation could not be made at all (which
/// no type here has) or the arithmetic overflows.
///
/// - **A process:** the process, its account and its address space's object; its page tables
///   at the bound one space may have (`MAX_TABLES`, the PML4 among them), and the list that
///   holds them. A process holds them whatever its limits.
/// - **Handles:** the handle table's two lists, at the `handles` quota (`HandleTable`).
/// - **A channel:** `ipc::channel_cost`.
/// - **Committed memory:** a frame for each page, and for each page the block `SharedFrame`
///   makes and its entry in the object's list of frames. That list is one allocation of exactly
///   the object's pages, which the heap rounds to at most twice its bytes; and an object has at
///   least one page, so a process has at most one object for each page of its quota.
/// - **Mappings:** the table of whole mappings, and the address space's two tables of page
///   records at the most pages a space can map (`quota::Limits::parts`).
/// - **An event, a port:** the object.
/// - **A binding:** its storage and its ticket, and the slots of a port's index it can make the
///   port keep. A port's index grows as a `Vec` does, from four slots: while it grows it holds
///   its new block and its old one, at most six slots' bytes a binding once rounded, and eight
///   when it holds one. Counted as eight (row K7).
/// - **A thread:** its kernel stack, its FPU area at this processor's size (64-byte aligned,
///   `FpuArea`), its thread object and the context the scheduler holds for it.
/// - **A page a launch maps** (not in a figure; the creator pays it besides, the coordinating
///   session's ruling (d)): its frame, if it is the process's own, and its share of the space's
///   two tables of page records, eight records' bytes in each (`quota::eight_a_page`).
pub(crate) fn costs() -> Option<Costs> {
    use crate::memory::OwnedFrame;
    use origin_frame::sync::ports::{Port, Reservation};
    use origin_frame::userspace::{
        MAX_TABLES, MAX_USER_PAGES, PAGE_RECORD, SHARED_FRAME, SharedFrame,
    };

    const PAGE: u64 = crate::paging::PAGE;
    let tables = (MAX_TABLES as u64).checked_mul(PAGE)?;
    let table_list = growing(
        MAX_TABLES,
        element((
            core::mem::size_of::<OwnedFrame>(),
            core::mem::align_of::<OwnedFrame>(),
        )),
    )?;
    let process = block(ObjectRef::<Process>::ALLOCATION)?
        .checked_add(block(ObjectRef::<Account>::ALLOCATION)?)?
        .checked_add(block(ObjectRef::<UserSpace>::ALLOCATION)?)?
        .checked_add(tables)?
        .checked_add(table_list)?;
    let [slots, free] = HandleTable::<Object>::ELEMENTS;
    let frame_entry = core::mem::size_of::<SharedFrame>() as u64;
    let memory_page = block(SHARED_FRAME)?.checked_add(frame_entry.checked_mul(2)?)?;
    let [store, ticket] = Reservation::<Ports>::ALLOCATIONS;
    let (slot, _) = Port::<Ports>::INDEX_SLOT;
    let binding = block(store)?
        .checked_add(block(ticket)?)?
        .checked_add((slot as u64).checked_mul(8)?)?;
    // A page's share of the space's two tables of page records, each growing a page at a time.
    let records = eight_a_page(element(PAGE_RECORD))?.checked_mul(2)?;
    let fpu = crate::cpu::fpu_area_size().max(crate::features::FXSAVE_SIZE);
    let thread = (sched::THREAD_STACK as u64)
        .checked_add(footprint(fpu, 64)?)?
        .checked_add(block(ObjectRef::<ThreadObject>::ALLOCATION)?)?
        .checked_add(block(ObjectRef::<UserContext>::ALLOCATION)?)?;
    Some(Costs {
        process,
        handle_tables: [element(slots), element(free)],
        channel: ipc::channel_cost()?,
        memory_object: block(ObjectRef::<MemoryObject>::ALLOCATION)?,
        memory_page,
        mapping_table: element(Mappings::<ObjectRef<MemoryObject>>::RECORD),
        page_record: element(PAGE_RECORD),
        // One more than a space can map: a single page's map reserves room for one record before
        // its tables refuse it (`UserSpace::map_page`), so a table can grow for that one.
        user_pages: MAX_USER_PAGES + 1,
        event: block(ObjectRef::<crate::event::Event>::ALLOCATION)?,
        port: block(ObjectRef::<crate::port::KernelPort>::ALLOCATION)?,
        binding,
        thread,
        page: PAGE.checked_add(records)?,
        shared_page: records,
    })
}

// What `costs` assumes of this build, each held by a host test in `quota` for the values here:
// a port's index grows from the same first capacity a `Vec` of its slots does, and its slot is
// no smaller than the heap's smallest block, so eight slots' bytes cover a binding's share; and a
// memory object's list of frames holds 8-byte entries, so twice its entries' bytes cover it.
const _: () = {
    let (slot, _) = origin_frame::sync::ports::Port::<Ports>::INDEX_SLOT;
    assert!(origin_frame::sync::ports::Port::<Ports>::FIRST_INDEX_SLOTS == 4);
    assert!(slot >= 16 && slot <= 1024);
    assert!(core::mem::size_of::<origin_frame::userspace::SharedFrame>() == 8);
    // A page record is in the range that host test holds eight elements a page for.
    let (record, align) = origin_frame::userspace::PAGE_RECORD;
    assert!(record >= 16 && record <= 1024 && record % 8 == 0 && align <= 8);
};

/// One process.
pub(crate) struct Process {
    /// Unique for the boot and never reused (ADR-0005 C1). Grants nothing.
    id: u64,
    /// Who it acts for (ADR-0005 C1): opaque to the kernel, given by its creator and never
    /// changed. A plain field: written before the process is shared, never after.
    principal: u64,
    /// The creator's reference to its address space, given up when its last thread ends.
    space: SpinLock<Option<ObjectRef<UserSpace>>>,
    /// Its handles (P1).
    pub(crate) handles: SpinLock<HandleTable<Object>>,
    /// What it may hold (P11).
    pub(crate) account: ObjectRef<Account>,
    /// Its whole mappings of memory objects (C7), each holding its object.
    pub(crate) mappings: SpinLock<Mappings<ObjectRef<MemoryObject>>>,
    /// Its budget for the unmaps and protects it asks for, in any process (T15, T16).
    pub(crate) changes: SpinLock<Budget>,
    /// Where its system calls may come from (P4, B2, ONBF §6.1). A plain field: written before
    /// the process is shared, never after.
    origin: CallOrigin,
    /// May it use the developer-only calls?
    debug_calls: bool,
    /// The most threads it may have started and not yet reaped (G5): each holds a kernel stack
    /// and an FPU area its figure paid for. A plain field: written before the process is shared.
    threads: usize,
    /// Its threads.
    lifecycle: SpinLock<Lifecycle>,
    /// `TERMINATED`, raised once everything it held is gone (P11).
    termination: Termination,
    /// Developer-only output of `DEBUG_REPORT`.
    reports: SpinLock<ReportLog>,
    /// Its last thread's ending, once it has ended.
    ending: SpinLock<Option<Ending>>,
    /// How often a thread of this process blocked in `wait_one`.
    blocked: AtomicU64,
    /// Its threads currently in `wait_one`, for the boot proofs' ordering.
    pub(crate) waiting: AtomicU32,
}

impl Process {
    /// A new **proof** process with an empty lower half and the kernel's half, under `limits`,
    /// whose code may issue system calls only from `[syscall_origin.0, syscall_origin.1)`
    /// (ADR-0005 A6, which the kernel's own proof processes keep: ADR-0011 L8), acting for
    /// `principal` (C1). Only kernel code creates processes yet, so the creator names the
    /// principal.
    ///
    /// Only the boot proofs may make a process this way: every program is launched pinned per
    /// call ([`pinned_child_of`](Self::pinned_child_of)). `cargo xtask check` holds that,
    /// naming the files that may call this and [`proof_child_of`](Self::proof_child_of), each
    /// with its reason (the independent review of 2026-11-13, finding K5).
    pub(crate) fn proof(
        limits: Limits,
        syscall_origin: (u64, u64),
        principal: u64,
    ) -> Option<Self> {
        let (start, end) = syscall_origin;
        Self::with_origin(limits, CallOrigin::Range { start, end }, principal, 0).ok()
    }

    /// A new process whose system calls come from `origin`.
    ///
    /// Paid for first (G5): `LIMIT` if the root cannot pay what `limits` are worth and `ahead`
    /// bytes besides, for the pages its launch will map (the coordinating session's ruling (d)),
    /// or if that overflows; and then no ID is drawn and nothing is allocated. `NO_MEMORY`
    /// without the memory for its objects, with the payment given back.
    fn with_origin(
        limits: Limits,
        origin: CallOrigin,
        principal: u64,
        ahead: u64,
    ) -> Result<Self, Error> {
        let figure = costs()
            .and_then(|costs| limits.figure(&costs))
            .ok_or(Error::Limit)?;
        let paid = ROOT.pay(figure.checked_add(ahead).ok_or(Error::Limit)?)?;
        // On any failure below, `paid` is dropped where it then is, and the root is whole again.
        let space = UserSpace::new().ok_or(Error::NoMemory)?;
        let space = ObjectRef::try_new(space).map_err(|_| Error::NoMemory)?;
        let account = ObjectRef::try_new(Account {
            channels: Count::new(limits.channels),
            queued_bytes: AtomicUsize::new(0),
            max_queued_bytes: limits.message_bytes,
            committed: AtomicUsize::new(0),
            max_committed: limits.memory.committed,
            objects: ObjectCounts::new(limits.objects),
            paid,
            figure,
            ahead: AtomicU64::new(ahead),
            launch_pages: AtomicU64::new(0),
        })
        .map_err(|_| Error::NoMemory)?;
        Ok(Self {
            id: NEXT_PROCESS_ID.fetch_add(1, Ordering::Relaxed),
            principal,
            space: SpinLock::new(Some(space)),
            handles: SpinLock::new(HandleTable::new(limits.handles)),
            account,
            mappings: SpinLock::new(Mappings::new(limits.memory.mappings)),
            changes: SpinLock::new(Budget::new(limits.memory.changes)),
            origin,
            debug_calls: limits.debug_calls,
            threads: limits.threads,
            lifecycle: SpinLock::new(Lifecycle {
                running: 0,
                unreaped: 0,
                latest: None,
                ended: false,
            }),
            termination: Termination::new(),
            reports: SpinLock::new(ReportLog {
                values: [0; REPORTS],
                count: 0,
            }),
            ending: SpinLock::new(None),
            blocked: AtomicU64::new(0),
            waiting: AtomicU32::new(0),
        })
    }

    /// A new proof process that `creator` starts: it acts for the creator's principal, and
    /// there is no way to give it another (ADR-0005 C1; no capability for one exists). Its
    /// system calls come from the range its creator names (A6).
    pub(crate) fn proof_child_of(
        creator: &Self,
        limits: Limits,
        syscall_origin: (u64, u64),
    ) -> Option<Self> {
        Self::proof(limits, syscall_origin, creator.principal)
    }

    /// A new process that `creator` starts, pinned per call (ADR-0005 B2; ADR-0011 L8): the
    /// launch path's way in for every program that is not a proof process. It acts for the
    /// creator's principal, as [`proof_child_of`](Self::proof_child_of) says for a proof process. Until `origin.core` is mapped
    /// into it ([`map_core`](Self::map_core)) none of its calls would be accepted, and it has
    /// no thread to make one.
    ///
    /// The root pays its figure and `ahead` bytes besides, for the pages its launch will map
    /// (G5; the coordinating session's ruling (d)). `LIMIT` if it cannot, and then nothing is
    /// made; `NO_MEMORY` without the memory for its objects.
    pub(crate) fn pinned_child_of(creator: &Self, limits: Limits, ahead: u64) -> Result<Self, Error> {
        Self::with_origin(
            limits,
            CallOrigin::Core { base: None },
            creator.principal,
            ahead,
        )
    }

    /// Gives back to the root what was paid ahead for pages its launch did not map: once its
    /// start record is written, nothing more is mapped for it (`launch::start`).
    pub(crate) fn settle_launch(&self) {
        let left = self.account.ahead.swap(0, Ordering::AcqRel);
        self.account.paid.give_back(left);
    }

    /// This process's ID.
    pub(crate) const fn id(&self) -> u64 {
        self.id
    }

    /// Who this process is, as a message it writes carries it (ADR-0005 B1, C1).
    pub(crate) const fn identity(&self) -> Identity {
        Identity {
            process: self.id,
            principal: self.principal,
        }
    }

    /// Its `TERMINATED` signal.
    pub(crate) const fn termination(&self) -> &Termination {
        &self.termination
    }

    /// A reference to its address space, while it has one: for its creator, and for a boot
    /// proof that inspects a process's memory after it has ended.
    pub(crate) fn memory(&self) -> Option<ObjectRef<UserSpace>> {
        without_interrupts(|| self.space.lock().clone())
    }

    /// The PML4 (physical), the value for CR3; 0 once the address space is gone.
    pub(crate) fn root(&self) -> u64 {
        self.memory().map_or(0, |space| space.root())
    }

    /// Maps the user page `virt` to a fresh zeroed frame this process owns, and returns it.
    /// Only inside the user half (ADR-0004 H1). Before the process has threads, while its
    /// creator holds the only reference to the address space. [`ChangeError::Limit`] when the
    /// page needs one more page table than the space may have (`UserSpace::map_page`), and when
    /// the root cannot pay for the page.
    ///
    /// The page is the creator's spending for its child, paid out of the root (G5; the
    /// coordinating session's ruling (d)): out of what its launch paid ahead, or out of the root
    /// now. A page that is not mapped after all is not paid for.
    pub(crate) fn map_page(&mut self, virt: u64, access: Access) -> Result<UserFrame, ChangeError> {
        let bytes = costs().ok_or(ChangeError::Limit)?.page;
        let ahead = self
            .account
            .pay_page(bytes)
            .map_err(|_| ChangeError::Limit)?;
        let mapped = without_interrupts(|| {
            let mut space = self.space.lock();
            let unique = space.as_mut().and_then(ObjectRef::get_mut);
            unique.map_or(Err(ChangeError::Map(MapError::OutOfMemory)), |space| {
                space.map_page(virt, access)
            })
        });
        if mapped.is_err() {
            self.account.unpay_page(bytes, ahead);
        }
        mapped
    }

    /// Copies `bytes` to `offset` bytes into the frame `frame` (from [`Process::map_page`]).
    ///
    /// Never into a frame of `origin.core`: those are written once, at boot, by
    /// `core_image::install` alone, and every pinned process maps the same ones. `false` then,
    /// as for a frame this space does not map (the independent review of 2026-11-13, finding
    /// K1). The framework's own `UserSpace::write_frame` does not refuse it; a rule there is
    /// TASKS item 51. Until then this is the kernel's only way into a process's pages: the names
    /// gate in `cargo xtask check` lets `write_frame` be named in this file, for this function,
    /// and in the address-space proofs alone, which hold a bare space and no process.
    pub(crate) fn write_page(&self, frame: UserFrame, offset: usize, bytes: &[u8]) -> bool {
        if crate::core_image::is_core_frame(frame.phys()) {
            return false;
        }
        self.memory()
            .is_some_and(|space| space.write_frame(frame, offset, bytes))
    }

    /// The frame mapped at `virt`, if any.
    pub(crate) fn frame_of(&self, virt: u64) -> Option<UserFrame> {
        self.memory()?.frame_of(virt)
    }

    /// How this process's tables translate the user address `virt`.
    pub(crate) fn translate(&self, virt: u64) -> Option<Translation> {
        self.memory()?.translate(virt)
    }

    /// Whether `virt` is mapped in this process's address space.
    pub(crate) fn is_mapped(&self, virt: u64) -> bool {
        self.translate(virt).is_some()
    }

    /// Maps `origin.core` into this process at `base`, read-execute, on the one set of frames
    /// every process shares, and records `base` as where its calls must come from (ADR-0011
    /// Amendment 2, L2 and L7). Once: only a process made by
    /// [`pinned_child_of`](Self::pinned_child_of) that has no `origin.core` yet, and only
    /// before the process is shared, while its creator holds it alone.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGS` for a proof process, for a second mapping, and for a place that is not
    /// free pages of the user half; `NO_MEMORY` if `origin.core` is not installed or the tables
    /// cannot be made; `LIMIT` at the bound on a space's page tables. Nothing is mapped and no
    /// base is recorded on any of them.
    ///
    /// Its pages' records are paid for as [`map_page`](Self::map_page) pays for a page, at
    /// [`Costs::shared_page`] each, since their frames are shared: `LIMIT` if the root cannot.
    pub(crate) fn map_core(&mut self, base: u64) -> Result<(), Error> {
        let mapped = self.origin.mapped_at(base).ok_or(Error::InvalidArgs)?;
        let space = self.memory().ok_or(Error::InvalidArgs)?;
        let each = costs().ok_or(Error::Limit)?.shared_page;
        let pages = crate::core_image::image().span / crate::paging::PAGE;
        let mut paid = Vec::new();
        paid.try_reserve_exact(pages as usize)
            .map_err(|_| Error::NoMemory)?;
        let give_back = |paid: &[bool]| {
            for &ahead in paid {
                self.account.unpay_page(each, ahead);
            }
        };
        for _ in 0..pages {
            match self.account.pay_page(each) {
                Ok(ahead) => paid.push(ahead),
                Err(e) => {
                    give_back(&paid);
                    return Err(e);
                }
            }
        }
        if let Err(e) = crate::core_image::map_into(&space, base) {
            give_back(&paid);
            return Err(match e {
                ChangeError::Limit => Error::Limit,
                ChangeError::Map(MapError::OutOfMemory) => Error::NoMemory,
                _ => Error::InvalidArgs,
            });
        }
        self.origin = mapped;
        Ok(())
    }

    /// Where this process's system calls may come from.
    pub(crate) const fn call_origin(&self) -> CallOrigin {
        self.origin
    }

    /// Whether this process may use the developer-only calls.
    pub(crate) const fn debug_calls(&self) -> bool {
        self.debug_calls
    }

    /// Puts `object` in this process's table with `rights`: what the creator grants.
    pub(crate) fn grant(&self, object: Object, rights: u32) -> Result<Handle, Error> {
        without_interrupts(|| self.handles.lock().insert(object, rights)).map_err(|(e, _)| e)
    }

    /// Handles it holds.
    pub(crate) fn handle_count(&self) -> usize {
        without_interrupts(|| self.handles.lock().len())
    }

    /// Records `value` from `DEBUG_REPORT`.
    pub(crate) fn report(&self, value: u64) {
        without_interrupts(|| {
            let mut log = self.reports.lock();
            let at = log.count;
            if let Some(slot) = log.values.get_mut(at) {
                *slot = value;
                log.count = at + 1;
            }
        });
    }

    /// Everything `DEBUG_REPORT` recorded, oldest first.
    pub(crate) fn reports(&self) -> Vec<u64> {
        without_interrupts(|| {
            let log = self.reports.lock();
            log.values.get(..log.count).unwrap_or(&[]).to_vec()
        })
    }

    /// How this process's last thread ended, once it has.
    pub(crate) fn ending(&self) -> Option<Ending> {
        without_interrupts(|| *self.ending.lock())
    }

    /// Adds to the count of `wait_one`s that blocked.
    pub(crate) fn count_blocked(&self, times: u64) {
        self.blocked.fetch_add(times, Ordering::Relaxed);
    }

    /// How often this process's threads blocked in `wait_one`.
    pub(crate) fn blocked(&self) -> u64 {
        self.blocked.load(Ordering::Relaxed)
    }

    /// Starts a thread of this process on processor `cpu`, at the user address `rip` with
    /// the stack pointer `rsp` and `rdi` as its RDI; every other register is zero. Gives back
    /// the thread, as handles name it. Refused once the process has ended.
    pub(crate) fn start_thread(
        process: &ObjectRef<Self>,
        cpu: CpuIndex,
        rip: u64,
        rsp: u64,
        rdi: u64,
    ) -> Result<ObjectRef<ThreadObject>, QueueError> {
        let thread = ObjectRef::try_new(ThreadObject::new()).map_err(|_| QueueError::Full)?;
        Self::start_thread_as(process, cpu, rip, rsp, rdi, thread)
    }

    /// As [`Process::start_thread`], as the thread `thread`, made before it runs: the launch
    /// path names it in the start record first.
    pub(crate) fn start_thread_as(
        process: &ObjectRef<Self>,
        cpu: CpuIndex,
        rip: u64,
        rsp: u64,
        rdi: u64,
        thread: ObjectRef<ThreadObject>,
    ) -> Result<ObjectRef<ThreadObject>, QueueError> {
        // Counted before it takes anything, and before it can run, so that its own end always
        // finds itself counted. While it is counted the process cannot end, and its last reaping
        // cannot raise `TERMINATED`, so the reference to the address space taken below is always
        // given up first: by the reaper, or here before `start_failed`.
        //
        // A start past the process's `threads` is refused here too, before anything is taken
        // (G5): its figure paid for that many kernel stacks and FPU areas, no more.
        without_interrupts(|| {
            let mut life = process.lifecycle.lock();
            if life.ended || life.unreaped as usize >= process.threads {
                return Err(QueueError::Full);
            }
            life.running += 1;
            life.unreaped += 1;
            Ok(())
        })?;
        let Some(user) = Self::context_for(process, thread) else {
            process.start_failed();
            return Err(QueueError::Full);
        };
        let thread = user.thread.clone();
        match sched::spawn_user(cpu, user, rip, rsp, rdi) {
            Ok(_) => Ok(thread),
            Err(e) => {
                // The scheduler has dropped the context, and this thread's reference with it.
                process.start_failed();
                Err(e)
            }
        }
    }

    /// What a new thread of `process` stands on: the process, a reference to its address space
    /// and the thread object. `None` without the memory for them, having dropped whatever it
    /// took.
    fn context_for(
        process: &ObjectRef<Self>,
        thread: ObjectRef<ThreadObject>,
    ) -> Option<ObjectRef<UserContext>> {
        let space = process.memory()?;
        ObjectRef::try_new(UserContext {
            process: process.clone(),
            space,
            thread,
        })
        .ok()
    }

    /// A counted thread never started. If every other thread ended in the meantime, this is
    /// what makes the process end: the thread that thought it was not the last did not.
    fn start_failed(&self) {
        let (finished, done) = without_interrupts(|| {
            let mut life = self.lifecycle.lock();
            life.running -= 1;
            life.unreaped -= 1;
            let finished = if life.running == 0 && !life.ended {
                life.latest
            } else {
                None
            };
            life.ended |= finished.is_some();
            (finished, life.ended && life.unreaped == 0)
        });
        if let Some(ending) = finished {
            self.close_out(ending);
        }
        if done {
            self.termination.raise();
        }
    }

    /// The calling thread, `thread`, is ending, for the reason `ending`. `TERMINATED` is raised
    /// on the thread now. If it was the process's last, every handle closes now, and peers see
    /// that.
    pub(crate) fn thread_ended(&self, thread: &ThreadObject, ending: Ending) {
        thread.termination.raise();
        let last = without_interrupts(|| {
            let mut life = self.lifecycle.lock();
            life.running -= 1;
            life.latest = Some(ending);
            let last = life.running == 0;
            life.ended |= last;
            last
        });
        if last {
            self.close_out(ending);
        }
    }

    /// The last thread has ended: its handles close, and the creator's reference to its
    /// address space goes, so that the threads' own are all that hold it up.
    fn close_out(&self, ending: Ending) {
        // An empty table of limit 0 replaces this one, so nothing can be added afterwards
        // and the old table is dropped outside its lock.
        let table = without_interrupts(|| {
            core::mem::replace(&mut *self.handles.lock(), HandleTable::new(0))
        });
        // The bindings it made are cancelled before its handles close (F11).
        ipc::close_table(table, self.id);
        ipc::reap_closed();
        // Its mappings' objects go with it, and no more can be made (P11): before it counts as
        // ended, so whoever sees the ending sees their charges back.
        let objects = without_interrupts(|| self.mappings.lock().close());
        drop(objects);
        // "Ended" means its handles are closed and its peers can see that. (Ends that were only
        // reachable through an undelivered message another processor is reaping this instant
        // close a moment later: they are delayed, never lost.)
        without_interrupts(|| *self.ending.lock() = Some(ending));
        let creators = without_interrupts(|| self.space.lock().take());
        drop(creators);
    }

    /// A launch that failed before the first thread ran gives back everything granted to this
    /// process: its table closes, and with it the process's handle to itself, which would
    /// otherwise keep the process alive for ever. Nothing can be added to the table afterwards.
    pub(crate) fn abandon(&self) {
        let table = without_interrupts(|| {
            core::mem::replace(&mut *self.handles.lock(), HandleTable::new(0))
        });
        // The bindings it made are cancelled before its handles close (F11).
        ipc::close_table(table, self.id);
        ipc::reap_closed();
    }

    /// A thread of this process has been reaped, and its reference to the address space is
    /// gone. After the last of them everything the process held has been released: `TERMINATED`.
    pub(crate) fn thread_reaped(&self) {
        let done = without_interrupts(|| {
            let mut life = self.lifecycle.lock();
            life.unreaped -= 1;
            life.ended && life.unreaped == 0
        });
        if done {
            self.termination.raise();
        }
    }
}
