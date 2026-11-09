//! Kernel processes (M4; CLAUDE.md §28 user address spaces, §29 processes and isolation;
//! ADR-0005 P1, P11).
//!
//! A process owns its address space, its handle table, the user frames it maps and the
//! threads the kernel started in it:
//! - Its own PML4, whose lower half holds only its own pages and whose kernel half (entries
//!   256–511) is a copy of the kernel's (ADR-0004 H2), so the kernel is mapped the same in
//!   every address space.
//! - Its page tables come from the frame allocator. Their [`TableMemory`] answers only for
//!   frames it allocated itself, so the paging library can never be led into a frame the
//!   process does not own.
//! - Its handle table is the only way it names kernel objects (P1).
//!
//! When its last thread ends, by exit or by fault, its handles close at once and its peers
//! see that. Its address space goes last, with the final reference (`Drop`): the threads
//! hold references through the scheduler, so by then no processor runs in it, and each that
//! ran one has loaded another CR3, which flushes its translations (no PCIDs yet).
//!
//! The first slice has no process-creation or memory system calls: the kernel builds each
//! process and maps its pages before any of its threads runs, so the page tables do not
//! change while it lives.
#![forbid(unsafe_code)]

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

use framekernel::handles::HandleTable;
use framekernel::paging::{Access, MapError, Translation};
use framekernel::runqueue::QueueError;
use origin_abi::{Error, Handle};
use origin_frame::sync::SpinLock;
use origin_frame::userspace::{UserFrame, UserSpace};

use crate::cpu::{CpuIndex, without_interrupts};
use crate::ipc::{self, Object};
use crate::sched;

/// Values a process's report log keeps, for the boot proofs.
const REPORTS: usize = 128;

/// What a process may hold, set by its creator (P11). Objects are charged here even while
/// they are in flight in a message, so a process cannot escape its table quota by putting
/// channels in the kernel's queues. Queued messages are charged too, to the process that
/// sent them, by the memory they hold.
pub(crate) struct Account {
    channels: AtomicUsize,
    max_channels: usize,
    /// Bytes of kernel heap held by messages this process has queued and nobody has read.
    queued_bytes: AtomicUsize,
    max_queued_bytes: usize,
}

impl Account {
    /// One more channel, or [`Error::Limit`].
    pub(crate) fn charge_channel(&self) -> Result<(), Error> {
        self.channels
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < self.max_channels).then_some(n + 1)
            })
            .map(|_| ())
            .map_err(|_| Error::Limit)
    }

    /// A channel charged to this account is gone.
    pub(crate) fn refund_channel(&self) {
        self.channels.fetch_sub(1, Ordering::AcqRel);
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
}

/// A process's quotas: at most `handles` entries in its table, `channels` live channels and
/// `message_bytes` of kernel heap in messages it has queued.
#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) handles: usize,
    pub(crate) channels: usize,
    pub(crate) message_bytes: usize,
    /// May it use `DEBUG_REPORT` and `DEBUG_WRITE`? The ABI reserves them for developer
    /// builds: an ordinary process that calls them gets `NOT_SUPPORTED`, so it cannot hold
    /// the serial line (at 115200 baud, a line for many milliseconds) with interrupts off.
    pub(crate) debug_calls: bool,
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

/// One process.
pub(crate) struct Process {
    /// Its page tables and the user frames they map, freed with it.
    space: UserSpace,
    /// Its handles (P1).
    pub(crate) handles: SpinLock<HandleTable<Object>>,
    /// What it may hold (P11).
    pub(crate) account: Arc<Account>,
    /// The only code range that may issue system calls (P4, ONBF §6.1): `[start, end)`.
    syscall_origin: (u64, u64),
    /// May it use the developer-only calls?
    debug_calls: bool,
    /// Threads running in it.
    threads: AtomicU32,
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
    /// code may issue system calls only from `[syscall_origin.0, syscall_origin.1)`.
    pub(crate) fn new(limits: Limits, syscall_origin: (u64, u64)) -> Option<Self> {
        Some(Self {
            space: UserSpace::new()?,
            handles: SpinLock::new(HandleTable::new(limits.handles)),
            account: Arc::new(Account {
                channels: AtomicUsize::new(0),
                max_channels: limits.channels,
                queued_bytes: AtomicUsize::new(0),
                max_queued_bytes: limits.message_bytes,
            }),
            syscall_origin,
            debug_calls: limits.debug_calls,
            threads: AtomicU32::new(0),
            reports: SpinLock::new(ReportLog {
                values: [0; REPORTS],
                count: 0,
            }),
            ending: SpinLock::new(None),
            blocked: AtomicU64::new(0),
            waiting: AtomicU32::new(0),
        })
    }

    /// The PML4 (physical), the value for CR3.
    pub(crate) const fn root(&self) -> u64 {
        self.space.root()
    }

    /// This process's address space.
    pub(crate) const fn space(&self) -> &UserSpace {
        &self.space
    }

    /// Maps the user page `virt` to a fresh zeroed frame this process owns, and returns it.
    /// Only inside the user half (ADR-0004 H1). Before the process has threads.
    pub(crate) fn map_page(&mut self, virt: u64, access: Access) -> Result<UserFrame, MapError> {
        self.space.map_page(virt, access)
    }

    /// Copies `bytes` to `offset` bytes into the frame `frame` (from [`Process::map_page`]).
    pub(crate) fn write_frame(&self, frame: UserFrame, offset: usize, bytes: &[u8]) -> bool {
        self.space.write_frame(frame, offset, bytes)
    }

    /// The frame mapped at `virt`, if any.
    pub(crate) fn frame_of(&self, virt: u64) -> Option<UserFrame> {
        self.space.frame_of(virt)
    }

    /// The little-endian `u64` at `virt` in this process's memory, for the boot proofs.
    pub(crate) fn read_u64(&self, virt: u64) -> Option<u64> {
        self.space.read_u64(virt)
    }

    /// How this process's tables translate the user address `virt`.
    pub(crate) fn translate(&self, virt: u64) -> Option<Translation> {
        self.space.translate(virt)
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
    /// the stack pointer `rsp` and `rdi` as its RDI; every other register is zero.
    pub(crate) fn start_thread(
        process: &Arc<Self>,
        cpu: CpuIndex,
        rip: u64,
        rsp: u64,
        rdi: u64,
    ) -> Result<(), QueueError> {
        process.threads.fetch_add(1, Ordering::AcqRel);
        match sched::spawn_user(cpu, process.clone(), rip, rsp, rdi) {
            Ok(_) => Ok(()),
            Err(e) => {
                process.threads.fetch_sub(1, Ordering::AcqRel);
                Err(e)
            }
        }
    }

    /// The calling thread is ending, for the reason `ending`. If it was the last, every
    /// handle closes now, and peers see that.
    pub(crate) fn thread_ended(&self, ending: Ending) {
        if self.threads.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        // An empty table of limit 0 replaces this one, so nothing can be added afterwards
        // and the old table is dropped outside its lock.
        let table = without_interrupts(|| {
            core::mem::replace(&mut *self.handles.lock(), HandleTable::new(0))
        });
        drop(table);
        ipc::reap_closed();
        // Last: "ended" means its handles are closed and its peers can see that. (Ends that
        // were only reachable through an undelivered message another processor is reaping
        // this instant close a moment later: they are delayed, never lost.)
        without_interrupts(|| *self.ending.lock() = Some(ending));
    }
}
