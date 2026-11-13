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
#![forbid(unsafe_code)]

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::channel::Identity;
use crate::handles::HandleTable;
use crate::mapping::{Budget, Mappings, Rate};
use crate::memory_object::MemoryObject;
use crate::paging::{Access, MapError, Translation};
pub(crate) use crate::quota::ObjectLimits;
use crate::quota::{Count, ObjectCounts};
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
}

impl Account {
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

/// What a process may commit and map, and how fast it may change its mappings (P11, C7;
/// `THREAT_MODEL_ADDRESS_SPACES.md` T15, T16). [`MemoryLimits::NONE`] allows nothing: a creator
/// gives memory to a process deliberately.
#[derive(Clone, Copy)]
pub(crate) struct MemoryLimits {
    /// Bytes of memory objects it may have committed at once.
    pub(crate) committed: usize,
    /// Whole mappings its address space may hold.
    pub(crate) mappings: usize,
    /// Its budget of shootdowns and interrupts for unmap and protect.
    pub(crate) changes: Rate,
}

impl MemoryLimits {
    /// No memory objects, no mappings and no changes.
    pub(crate) const NONE: Self = Self {
        committed: 0,
        mappings: 0,
        changes: Rate {
            burst: 0,
            per_second: 1,
        },
    };
}

/// A process's quotas: at most `handles` entries in its table, `channels` live channels,
/// `message_bytes` of kernel heap in messages it has queued, its memory, and its events, ports
/// and bindings.
#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) handles: usize,
    pub(crate) channels: usize,
    pub(crate) message_bytes: usize,
    /// May it use `DEBUG_REPORT` and `DEBUG_WRITE`? The ABI reserves them for developer
    /// builds: an ordinary process that calls them gets `NOT_SUPPORTED`, so it cannot hold
    /// the serial line (at 115200 baud, a line for many milliseconds) with interrupts off.
    pub(crate) debug_calls: bool,
    /// Its memory objects and mappings.
    pub(crate) memory: MemoryLimits,
    /// Its events, ports and bindings. [`ObjectLimits::NONE`] allows none.
    pub(crate) objects: ObjectLimits,
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
}

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
    /// The only code range that may issue system calls (P4, ONBF §6.1): `[start, end)`.
    syscall_origin: (u64, u64),
    /// May it use the developer-only calls?
    debug_calls: bool,
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
    /// A new process with an empty lower half and the kernel's half, under `limits`, whose
    /// code may issue system calls only from `[syscall_origin.0, syscall_origin.1)`, acting
    /// for `principal` (C1). Only kernel code creates processes yet, so the creator names the
    /// principal; a launch path will pass its own by default.
    pub(crate) fn new(limits: Limits, syscall_origin: (u64, u64), principal: u64) -> Option<Self> {
        Some(Self {
            id: NEXT_PROCESS_ID.fetch_add(1, Ordering::Relaxed),
            principal,
            space: SpinLock::new(Some(ObjectRef::try_new(UserSpace::new()?).ok()?)),
            handles: SpinLock::new(HandleTable::new(limits.handles)),
            account: ObjectRef::try_new(Account {
                channels: Count::new(limits.channels),
                queued_bytes: AtomicUsize::new(0),
                max_queued_bytes: limits.message_bytes,
                committed: AtomicUsize::new(0),
                max_committed: limits.memory.committed,
                objects: ObjectCounts::new(limits.objects),
            })
            .ok()?,
            mappings: SpinLock::new(Mappings::new(limits.memory.mappings)),
            changes: SpinLock::new(Budget::new(limits.memory.changes)),
            syscall_origin,
            debug_calls: limits.debug_calls,
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

    /// A new process that `creator` starts: it acts for the creator's principal, and there is no
    /// way to give it another (ADR-0005 C1; no capability for one exists). The launch path's way
    /// in; only kernel code that creates a process with no creator names a principal.
    pub(crate) fn child_of(
        creator: &Self,
        limits: Limits,
        syscall_origin: (u64, u64),
    ) -> Option<Self> {
        Self::new(limits, syscall_origin, creator.principal)
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
    /// page needs one more page table than the space may have (`UserSpace::map_page`).
    pub(crate) fn map_page(&mut self, virt: u64, access: Access) -> Result<UserFrame, ChangeError> {
        without_interrupts(|| {
            let mut space = self.space.lock();
            let unique = space.as_mut().and_then(ObjectRef::get_mut);
            unique.map_or(Err(ChangeError::Map(MapError::OutOfMemory)), |space| {
                space.map_page(virt, access)
            })
        })
    }

    /// Copies `bytes` to `offset` bytes into the frame `frame` (from [`Process::map_page`]).
    pub(crate) fn write_frame(&self, frame: UserFrame, offset: usize, bytes: &[u8]) -> bool {
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

    /// Whether the system call whose return address is `rip` came from the code allowed to
    /// make one (P4): the `syscall` instruction, two bytes long, lies inside the range.
    pub(crate) fn may_call_from(&self, rip: u64) -> bool {
        let (start, end) = self.syscall_origin;
        rip.checked_sub(2)
            .is_some_and(|at| at >= start && rip <= end)
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
        without_interrupts(|| {
            let mut life = process.lifecycle.lock();
            if life.ended {
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
