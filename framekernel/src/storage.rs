//! The NVMe driver (M5; CLAUDE.md §33: controller discovery, initialization, admin and I/O
//! queues, namespace discovery, DMA, flushes), the boot proof of its block I/O, and the
//! disk's GUID Partition Table, both copies checked (CLAUDE.md §39).
//!
//! A first-party kernel-mode driver (ADR-0001 D2). It binds to the first NVMe controller
//! (class 01:08:02) that `pcie` listed:
//! 1. memory decoding on; BAR 0 mapped uncached; the controller attached to a domain;
//! 2. the controller reset: CC.EN cleared and CSTS.RDY awaited within CAP.TO; only then is
//!    bus mastering turned on (ADR-0007 I12: never before it is attached, and a controller
//!    the firmware left running cannot act on the firmware's queues in between);
//! 3. admin queues of 32 entries; CC.EN set (4 KiB pages, 64/16-byte entries); RDY awaited,
//!    with the fatal-status bit checked;
//! 4. Identify Controller and Identify Namespace 1;
//! 5. I/O queue pair 1 (64 entries), polled;
//! 6. after the polled proofs, MSI-X: vector 1 is sent to one processor as
//!    `cpu::DEVICE_VECTOR` (every other entry masked), and I/O queue pair 2 completes to
//!    it. A thread there submits under the driver lock and then blocks on a wait queue
//!    until the interrupt wakes it; its wait condition is the completion queue itself.
//!    Then every processor gets a queue pair and vector of its own. A message's destination
//!    is the processor's x2APIC ID (which may exceed 255: ADR-0008); per-CPU state here is
//!    indexed by its CPU index.
//!
//! DMA memory is zeroed heap pages (`dma::DeviceDma`). With the IOMMU translating, the
//! controller is attached to a domain of its own before its first DMA, every page is mapped
//! there, and the driver programs IOVAs (ADR-0007): the controller reaches those pages and
//! nothing else. x86 DMA is cache-coherent. The driver never keeps a reference into memory
//! the device may write: queue entries are written and read with volatile accesses, and a
//! data buffer is read only after its command completed. A fence orders memory writes before
//! every doorbell. MSI-X entries are programmed through `iommu::msi_message`, so with
//! interrupt remapping on they hold only remappable-format handles.
//!
//! A queue runs one command at a time. A command that never completes kills its queue (there
//! is no abort or controller reset yet): every later command on it fails at once, its late
//! completion is never taken, and the block layer keeps the command's page out of use.
//!
//! The boot proofs write only to the disk the boot test builds, named positively by
//! `framekernel::blockcache::test_disk` (CLAUDE.md P0). Any other disk gets no write and no
//! flush during boot: the proofs say so once, and that is not a failure.

use alloc::vec::Vec;
use core::fmt::Write;
use core::ptr;
use core::sync::atomic::{Ordering, compiler_fence, fence};

use core::sync::atomic::AtomicU64;
use framekernel::blockcache::{QueueGate, Transfer, test_disk};
use framekernel::gpt;
use framekernel::nvme::{self, Capabilities, Command, Completion, NamespaceInfo, Queues, register};
use framekernel::pci::{self, BarKind};
use framekernel::vtd;
use origin_sync::SpinLock;

use crate::cpu::{CpuIndex, MAX_CPUS};
use crate::dma::DeviceDma;
use crate::wait::WaitQueue;

use crate::console::Console;

pub(crate) use framekernel::blockcache::test_disk::pattern;

/// Entries in the admin queues and the I/O queues.
const ADMIN_ENTRIES: u16 = 32;
const IO_ENTRIES: u16 = 64;
/// The memory page size the driver configures.
pub(crate) const PAGE: u64 = 4096;
/// How long one command may take.
const COMMAND_TIMEOUT_NS: u64 = 2_000_000_000;
/// The namespace the proof uses.
const NSID: u32 = 1;

/// A zeroed 4 KiB DMA page, kept for ever (the controller may use it at any time): where
/// the kernel reaches it (`virt`) and the address the controller uses (`bus`: an IOVA in its
/// domain, or the physical address without an IOMMU).
#[derive(Clone, Copy)]
struct DmaPage {
    virt: u64,
    bus: u64,
}

impl DmaPage {
    fn new(dma: DeviceDma) -> Option<Self> {
        let p = dma.page()?;
        Some(Self {
            virt: p.virt,
            bus: p.bus,
        })
    }

    fn write32(&self, offset: usize, value: u32) {
        if offset + 4 <= PAGE as usize {
            let p = ptr::with_exposed_provenance_mut::<u32>(self.virt as usize + offset);
            // SAFETY: inside this live page, 4-byte aligned (callers use multiples of 4).
            unsafe { p.write_volatile(value) };
        }
    }

    fn read32(&self, offset: usize) -> u32 {
        if offset + 4 > PAGE as usize {
            return 0;
        }
        let p = ptr::with_exposed_provenance::<u32>(self.virt as usize + offset);
        // SAFETY: inside this live page, 4-byte aligned; the device may be writing it, so
        // the read is volatile and no reference is made.
        unsafe { p.read_volatile() }
    }

    fn fill(&self, bytes: &[u8]) {
        for (i, &b) in bytes.iter().enumerate().take(PAGE as usize) {
            let p = ptr::with_exposed_provenance_mut::<u8>(self.virt as usize + i);
            // SAFETY: inside this live page, which is kept for ever; the write is volatile
            // and makes no reference, so even a command that timed out and may still reach
            // the page cannot make it unsound.
            unsafe { p.write_volatile(b) };
        }
    }

    /// The page's bytes, once no command that writes it is outstanding.
    fn bytes(&self) -> &[u8] {
        // SAFETY: the page is live for ever and 4 KiB long; callers read it only after the
        // command that filled it completed, so the device no longer writes it.
        unsafe {
            core::slice::from_raw_parts(
                ptr::with_exposed_provenance(self.virt as usize),
                PAGE as usize,
            )
        }
    }
}

/// One submission and completion queue pair.
struct QueuePair {
    qid: u16,
    sq: DmaPage,
    cq: DmaPage,
    state: Queues,
    next_cid: u16,
    /// Whether it can take a command: one at a time, and a dead queue never again.
    gate: QueueGate,
}

/// Why a command failed.
#[derive(Clone, Copy, Debug)]
enum Failure {
    Full,
    /// No completion in time: the command may still be live, so its queue is dead now.
    Timeout,
    /// A completion for another command, so this one may still be live: dead, too.
    WrongCommand,
    /// The queue is dead: refused without touching the device.
    Dead,
    /// Another command is outstanding on the queue: refused without touching the device.
    Busy,
}

/// Which queue pair a command goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Q {
    Admin,
    /// I/O queue 1: polled.
    Polled,
    /// I/O queue 2: completions raise an MSI-X interrupt.
    Interrupt,
}

/// A bound controller.
struct Nvme {
    regs: u64,
    /// How the controller reaches memory (its IOMMU domain, or directly).
    dma: DeviceDma,
    cap: Capabilities,
    admin: QueuePair,
    io: Option<QueuePair>,
    irq: Option<QueuePair>,
}

impl Nvme {
    fn read32(&self, offset: u64) -> u32 {
        let p = ptr::with_exposed_provenance::<u32>((self.regs + offset) as usize);
        // SAFETY: BAR 0 is mapped uncached at `regs`; `offset` is a register or doorbell
        // inside it (the callers' constants and `doorbell`, for queues that exist), 4-byte
        // aligned. A register read has no effect on memory.
        unsafe { p.read_volatile() }
    }

    fn write32(&self, offset: u64, value: u32) {
        let p = ptr::with_exposed_provenance_mut::<u32>((self.regs + offset) as usize);
        // SAFETY: as for `read32`. The writes are the controller's configuration, queue
        // bases (DMA pages this driver owns for ever) and doorbells.
        unsafe { p.write_volatile(value) };
    }

    /// A 64-bit register as two 32-bit reads, low first (allowed for every register).
    fn read64(&self, offset: u64) -> u64 {
        u64::from(self.read32(offset)) | (u64::from(self.read32(offset + 4)) << 32)
    }

    fn write64(&self, offset: u64, value: u64) {
        self.write32(offset, value as u32);
        self.write32(offset + 4, (value >> 32) as u32);
    }

    /// Waits for CSTS.RDY to become `ready`, up to CAP.TO; fails on controller fatal status.
    fn wait_ready(&self, ready: bool) -> Result<(), &'static str> {
        let deadline = crate::time::monotonic_nanos() + self.cap.timeout_ms.max(500) * 1_000_000;
        loop {
            let csts = self.read32(register::CSTS);
            if csts & nvme::CSTS_FATAL != 0 && ready {
                return Err("controller fatal status");
            }
            if (csts & nvme::CSTS_READY != 0) == ready {
                return Ok(());
            }
            if crate::time::monotonic_nanos() > deadline {
                return Err("CSTS.RDY did not change within CAP.TO");
            }
            core::hint::spin_loop();
        }
    }

    fn queue(&mut self, q: Q) -> Option<&mut QueuePair> {
        match q {
            Q::Admin => Some(&mut self.admin),
            Q::Polled => self.io.as_mut(),
            Q::Interrupt => self.irq.as_mut(),
        }
    }

    /// Submits `make(cid)` to queue `q` and polls for its completion. A caller holds no lock
    /// (the wait lets interrupts in) and has the controller to itself; with the driver
    /// shared, use [`command`].
    fn run(&mut self, q: Q, make: impl FnOnce(u16) -> Command) -> Result<Completion, Failure> {
        let cid = self.submit(q, make)?;
        let deadline = crate::time::monotonic_nanos() + COMMAND_TIMEOUT_NS;
        loop {
            // The clock first: whatever completed by this moment is found below, however
            // long this flow was away from the processor.
            let late = crate::time::monotonic_nanos() > deadline;
            if let Some(done) = self.poll(q, cid) {
                return done;
            }
            if late {
                self.kill(q);
                return Err(Failure::Timeout);
            }
            between_polls();
        }
    }

    /// Queue `q` lost a command: it is dead from now on.
    fn kill(&mut self, q: Q) {
        if let Some(qp) = self.queue(q) {
            qp.gate.kill();
        }
    }

    /// Writes `make(cid)` into queue `q` and rings its doorbell; returns the command ID.
    fn submit(&mut self, q: Q, make: impl FnOnce(u16) -> Command) -> Result<u16, Failure> {
        let (regs, stride) = (self.regs, self.cap.doorbell_stride);
        submit_on(self.queue(q).ok_or(Failure::Full)?, regs, stride, make)
    }

    /// Takes queue `q`'s next completion if it is there: the result for command `cid`, or
    /// `None` if nothing new has arrived.
    fn poll(&mut self, q: Q, cid: u16) -> Option<Result<Completion, Failure>> {
        let (regs, stride) = (self.regs, self.cap.doorbell_stride);
        poll_on(self.queue(q)?, regs, stride, cid)
    }
}

/// Writes `make(cid)` into `qp` and rings its doorbell (the controller's registers at
/// `regs`, doorbells `stride` bytes apart); returns the command ID.
fn submit_on(
    qp: &mut QueuePair,
    regs: u64,
    stride: u64,
    make: impl FnOnce(u16) -> Command,
) -> Result<u16, Failure> {
    {
        qp.gate.claim().map_err(|gate| {
            if gate.is_dead() {
                Failure::Dead
            } else {
                Failure::Busy
            }
        })?;
        let cid = qp.next_cid;
        qp.next_cid = qp.next_cid.wrapping_add(1);
        let Some(slot) = qp.state.submit() else {
            qp.gate.release();
            return Err(Failure::Full);
        };
        for (i, word) in make(cid).to_words().iter().enumerate() {
            qp.sq
                .write32(usize::from(slot) * nvme::SQ_ENTRY + 4 * i, *word);
        }
        // The entry (and any data buffer) must be visible before the controller fetches it.
        fence(Ordering::SeqCst);
        let doorbell = |offset: u64, value: u16| {
            let p = ptr::with_exposed_provenance_mut::<u32>((regs + offset) as usize);
            // SAFETY: as in `write32`: a doorbell of a queue that exists.
            unsafe { p.write_volatile(u32::from(value)) };
        };
        doorbell(nvme::doorbell(qp.qid, false, stride), qp.state.sq_tail());
        Ok(cid)
    }
}

/// Takes `qp`'s next completion if it is there: the result for command `cid`, or `None`
/// if nothing new has arrived.
fn poll_on(
    qp: &mut QueuePair,
    regs: u64,
    stride: u64,
    cid: u16,
) -> Option<Result<Completion, Failure>> {
    {
        // The queue died with this command outstanding, so it may still be live.
        if qp.gate.is_dead() {
            return Some(Err(Failure::Timeout));
        }
        let at = usize::from(qp.state.cq_head()) * nvme::CQ_ENTRY;
        // The phase is in the last dword: read it first, then the rest.
        let last = qp.cq.read32(at + 12);
        compiler_fence(Ordering::Acquire);
        let words = [
            qp.cq.read32(at),
            qp.cq.read32(at + 4),
            qp.cq.read32(at + 8),
            last,
        ];
        let c = Completion::decode(words);
        if !qp.state.complete(&c) {
            return None;
        }
        let p = ptr::with_exposed_provenance_mut::<u32>(
            (regs + nvme::doorbell(qp.qid, true, stride)) as usize,
        );
        // SAFETY: as in `write32`: the completion head doorbell of a queue that exists.
        unsafe { p.write_volatile(u32::from(qp.state.cq_head())) };
        Some(if c.cid == cid {
            qp.gate.release();
            Ok(c)
        } else {
            qp.gate.kill();
            Err(Failure::WrongCommand)
        })
    }
}

/// Between two looks at a completion queue, with no lock held: lets a pending interrupt in,
/// on whichever flow this is. A kernel thread runs with interrupts on anyway; the boot flow
/// runs with them off and takes them only through windows like this one.
fn between_polls() {
    crate::cpu::without_interrupts(crate::cpu::interrupt_window);
    core::hint::spin_loop();
}

fn new_queue(dma: DeviceDma, qid: u16, entries: u16) -> Option<QueuePair> {
    Some(QueuePair {
        qid,
        sq: DmaPage::new(dma)?,
        cq: DmaPage::new(dma)?,
        state: Queues::new(entries),
        next_cid: 0,
        gate: QueueGate::Idle,
    })
}

/// The bound controller, once the polled proofs are done, with a data page for the
/// interrupt-driven proof's thread. Locked with interrupts disabled and never across a wait:
/// [`command`] submits under the lock, waits with it released and takes it again only to
/// look for the completion; set-up that needs the driver across several commands takes it
/// out of the lock first ([`with_driver_out`]).
struct Driver {
    nvme: Nvme,
    dev: crate::pcie::Device,
    page: DmaPage,
    /// Namespace 1.
    namespace: NamespaceInfo,
    /// Namespace 1 is the boot test's disk: the only one the boot proofs may write to.
    test_disk: bool,
    /// The MSI-X table's virtual address and the capability, once set up.
    msix: Option<(u64, pci::Msix)>,
}

static DRIVER: SpinLock<Option<Driver>> = SpinLock::new(None);
/// Per processor (by CPU index): woken by the completion interrupts routed there.
static IO_WAIT: [WaitQueue; MAX_CPUS] = [const { WaitQueue::new() }; MAX_CPUS];
/// Per processor: completion interrupts taken there.
static CPU_INTERRUPTS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
/// Per processor: completion interrupts taken there that found a thread waiting on its
/// queue and woke it.
static CPU_WAKES: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
/// Per processor: how often threads there blocked waiting for a completion.
static CPU_BLOCKS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static INTERRUPTS: AtomicU64 = AtomicU64::new(0);
/// The CPU index the last completion interrupt arrived on, plus one (0: none yet).
static INTERRUPT_CPU: AtomicU64 = AtomicU64::new(0);

/// The completion interrupt (`cpu::DEVICE_VECTOR`): counts it on the processor that took it
/// and wakes the waiter there, which then reads the completion itself.
pub(crate) fn on_interrupt() {
    let me = crate::cpu::this_cpu().get();
    INTERRUPTS.fetch_add(1, Ordering::Relaxed);
    INTERRUPT_CPU.store((me as u64).saturating_add(1), Ordering::Relaxed);
    if let Some(n) = CPU_INTERRUPTS.get(me) {
        n.fetch_add(1, Ordering::Relaxed);
    }
    if let Some(w) = IO_WAIT.get(me)
        && w.wake_one()
        && let Some(n) = CPU_WAKES.get(me)
    {
        n.fetch_add(1, Ordering::Relaxed);
    }
}

/// Counts `blocks` waits for a completion by a thread on this processor.
fn count_blocks(blocks: u64) {
    if let Some(n) = CPU_BLOCKS.get(crate::cpu::this_cpu().get()) {
        n.fetch_add(blocks, Ordering::Relaxed);
    }
}

/// Binds the first NVMe controller and proves block I/O on namespace 1, polled and then
/// interrupt-driven. Returns whether every step and check passed (no controller at all is
/// reported, and passes, and so does a disk that is not the boot test's, on which only the
/// read-only proofs that do not need its contents run).
pub(crate) fn init(com1: &mut Console, cpus: &[CpuIndex], tsc_hz: u64) -> bool {
    let Some(dev) = crate::pcie::find(0x01, 0x08, 0x02) else {
        let _ = writeln!(com1, "origin-kernel: NVMe: no controller");
        return true;
    };
    let (polled, on_test_disk) = match bind(com1, &dev, cpus.len()) {
        Ok((nvme, ok, namespace, test_disk)) => {
            let Some(page) = DmaPage::new(nvme.dma) else {
                // Nothing will drive the controller: it must not keep bus mastering (I12).
                let _ = crate::pcie::clear_bus_master(dev.function.at);
                return false;
            };
            crate::cpu::without_interrupts(|| {
                *DRIVER.lock() = Some(Driver {
                    nvme,
                    dev,
                    page,
                    namespace,
                    test_disk,
                    msix: None,
                });
            });
            (ok, test_disk)
        }
        Err(why) => {
            // Failed after bus mastering went on, or before: either way nobody drives it (I12).
            let _ = crate::pcie::clear_bus_master(dev.function.at);
            let _ = writeln!(com1, "origin-kernel: NVMe: {why}: NVMe failed");
            return false;
        }
    };
    // Every proof below reads the boot test's pattern, and two of them write.
    if !on_test_disk {
        return polled;
    }
    let interrupts = polled && prove_interrupts(com1, cpus, tsc_hz);
    let per_cpu = interrupts && prove_per_cpu(com1, cpus, tsc_hz);
    let x2apic = per_cpu && prove_x2apic(com1, cpus);
    x2apic && crate::block::prove(com1)
}

/// Device block size and block count of namespace 1, or `None` if no controller is bound.
pub(crate) fn geometry() -> Option<(u32, u64)> {
    crate::cpu::without_interrupts(|| {
        let g = DRIVER.lock();
        let d = g.as_ref()?;
        Some((d.namespace.block_size, d.namespace.blocks))
    })
}

/// Whether namespace 1 is the boot test's disk, the only one the boot proofs may write to.
pub(crate) fn on_test_disk() -> bool {
    crate::cpu::without_interrupts(|| DRIVER.lock().as_ref().is_some_and(|d| d.test_disk))
}

/// Allocates a zeroed 4 KiB DMA page for the block layer, mapped for the bound
/// controller: its virtual address and the address the controller uses. Never freed.
pub(crate) fn dma_page() -> Option<(u64, u64)> {
    let dma = crate::cpu::without_interrupts(|| DRIVER.lock().as_ref().map(|d| d.nvme.dma))?;
    DmaPage::new(dma).map(|p| (p.virt, p.bus))
}

/// Reads or writes `blocks` device blocks at `lba` between the disk and the DMA page the
/// controller reaches at `bus`, through the polled I/O queue. For the block layer, driven
/// from the boot flow. A [`Transfer::Stuck`] means the command may still be live: the page
/// must not be reused.
pub(crate) fn transfer(write: bool, lba: u64, bus: u64, blocks: u32) -> Transfer {
    match transfer_status(write, lba, bus, blocks) {
        Ok(status) if status.ok() => Transfer::Done,
        Ok(_) | Err(Failure::Full | Failure::Dead | Failure::Busy) => Transfer::Failed,
        Err(Failure::Timeout | Failure::WrongCommand) => Transfer::Stuck,
    }
}

/// As [`transfer`], returning the completion's status, or why there is none.
fn transfer_status(write: bool, lba: u64, bus: u64, blocks: u32) -> Result<nvme::Status, Failure> {
    let (block_size, _) = geometry().ok_or(Failure::Full)?;
    let prps = nvme::prps(bus, u64::from(blocks) * u64::from(block_size), PAGE).unwrap_or((bus, 0));
    command(Q::Polled, |cid| {
        Command::transfer(write, cid, NSID, lba, blocks, prps)
    })
    .map(|c| c.status)
}

/// Flushes namespace 1's write cache to media.
pub(crate) fn flush_device() -> bool {
    command(Q::Polled, |cid| Command::flush(cid, NSID)).is_ok_and(|c| c.status.ok())
}

/// Runs one command on queue `q` of the bound controller: submitted under the driver lock,
/// then waited for with the lock released (taken again only to look for the completion) and
/// interrupts let in. Queues are shared, so a caller that finds another command outstanding
/// waits its turn the same way, for as long as one command may take.
fn command(q: Q, make: impl Fn(u16) -> Command) -> Result<Completion, Failure> {
    let turn = crate::time::monotonic_nanos() + COMMAND_TIMEOUT_NS;
    let cid = loop {
        let submitted = crate::cpu::without_interrupts(|| match DRIVER.lock().as_mut() {
            Some(d) => d.nvme.submit(q, &make),
            None => Err(Failure::Full),
        });
        match submitted {
            Ok(cid) => break cid,
            Err(Failure::Busy) if crate::time::monotonic_nanos() < turn => between_polls(),
            Err(failure) => return Err(failure),
        }
    };
    let deadline = crate::time::monotonic_nanos() + COMMAND_TIMEOUT_NS;
    loop {
        // The clock first, as in `Nvme::run`.
        let late = crate::time::monotonic_nanos() > deadline;
        let done = crate::cpu::without_interrupts(|| {
            let mut g = DRIVER.lock();
            let Some(d) = g.as_mut() else {
                return Some(Err(Failure::Timeout));
            };
            let done = d.nvme.poll(q, cid);
            if done.is_none() && late {
                d.nvme.kill(q);
                return Some(Err(Failure::Timeout));
            }
            done
        });
        if let Some(done) = done {
            return done;
        }
        between_polls();
    }
}

/// Runs boot-flow set-up on the bound driver with no lock held: the driver is taken out of
/// [`DRIVER`] for the duration, so that its commands wait with interrupts let in. Nothing
/// else uses the driver meanwhile: the threads that do are spawned after the set-up.
fn with_driver_out<T>(f: impl FnOnce(&mut Driver) -> T) -> Option<T> {
    let mut driver = crate::cpu::without_interrupts(|| DRIVER.lock().take())?;
    let result = f(&mut driver);
    crate::cpu::without_interrupts(|| *DRIVER.lock() = Some(driver));
    Some(result)
}

/// Reads `out.len()` bytes at `offset` of the DMA page mapped at `virt` (from
/// [`dma_page`]) into `out`. The block layer's slots are such pages.
pub(crate) fn dma_read(virt: u64, offset: usize, out: &mut [u8]) {
    if offset + out.len() > PAGE as usize {
        return;
    }
    for (i, b) in out.iter_mut().enumerate() {
        let p = ptr::with_exposed_provenance::<u8>(virt as usize + offset + i);
        // SAFETY: inside a live 4 KiB DMA page (bounds checked); no command is outstanding
        // on it while the block layer copies, so the device is not writing it.
        *b = unsafe { p.read_volatile() };
    }
}

/// Writes `bytes` at `offset` of the DMA page mapped at `virt`.
pub(crate) fn dma_write(virt: u64, offset: usize, bytes: &[u8]) {
    if offset + bytes.len() > PAGE as usize {
        return;
    }
    for (i, &b) in bytes.iter().enumerate() {
        let p = ptr::with_exposed_provenance_mut::<u8>(virt as usize + offset + i);
        // SAFETY: as in `dma_read`; the page is the block layer's, not in device use here.
        unsafe { p.write_volatile(b) };
    }
}

fn bind(
    com1: &mut Console,
    dev: &crate::pcie::Device,
    cpus: usize,
) -> Result<(Nvme, bool, NamespaceInfo, bool), &'static str> {
    let f = dev.function;
    let bar = dev.bars[0].ok_or("no BAR 0")?;
    if bar.kind == BarKind::Io || bar.size < 0x2000 {
        return Err("BAR 0 is not a register window");
    }
    crate::pcie::enable_memory(f.at).ok_or("memory decoding did not turn on")?;
    let regs = crate::vm::map_mmio(bar.address, bar.size).map_err(|_| "cannot map BAR 0")?;
    // Its own IOMMU domain before its first DMA page: from here on it reaches only the pages
    // mapped for it. Bus mastering stays off until the controller is reset.
    let dma = DeviceDma::attach(f.at)?;
    let mut c = Nvme {
        regs,
        dma,
        cap: Capabilities::decode(0),
        admin: new_queue(dma, 0, ADMIN_ENTRIES).ok_or("no memory for the admin queues")?,
        io: None,
        irq: None,
    };
    c.cap = Capabilities::decode(c.read64(register::CAP));
    // CAP is the device's word. Before the first command and its first doorbell: every
    // doorbell the driver will ring lies inside BAR 0 (the admin queues', queue pairs 1
    // (polled), 2 (one interrupt) and one per processor).
    let wanted = u16::try_from(2 + cpus).map_err(|_| "too many processors")?;
    if !nvme::doorbells_fit(wanted, c.cap.doorbell_stride, bar.size) {
        return Err("the queues' doorbells lie outside BAR 0");
    }
    let (major, minor, tertiary) = nvme::version(c.read32(register::VS));
    let _ = writeln!(
        com1,
        "origin-kernel: NVMe: {} {:04x}:{:04x}, NVMe {major}.{minor}.{tertiary}; BAR0 {} KiB at {:#x} mapped uncached; up to {} entries per queue, doorbell stride {}, ready timeout {} ms",
        f.at,
        f.vendor,
        f.device,
        bar.size >> 10,
        bar.address,
        c.cap.max_entries,
        c.cap.doorbell_stride,
        c.cap.timeout_ms
    );
    if !c.cap.nvm || c.cap.min_page_shift > 12 || c.cap.max_page_shift < 12 {
        return Err("no NVM command set or no 4 KiB pages");
    }
    if c.cap.max_entries < u32::from(IO_ENTRIES) {
        return Err("queues too small");
    }
    // Reset, then bus mastering, then the admin queues, then enable.
    let cc = c.read32(register::CC);
    if cc & 1 != 0 {
        c.write32(register::CC, cc & !1);
    }
    c.wait_ready(false)?;
    let command = dma.enable_bus_master(f.at)?;
    let _ = writeln!(
        com1,
        "origin-kernel: NVMe: bus mastering on after the controller was attached and reset: command register {command:#06x}"
    );
    c.write32(register::AQA, nvme::aqa(ADMIN_ENTRIES));
    c.write64(register::ASQ, c.admin.sq.bus);
    c.write64(register::ACQ, c.admin.cq.bus);
    c.write32(register::CC, nvme::cc_enable(12));
    c.wait_ready(true)?;
    // The driver polls: mask the controller's pin interrupt.
    c.write32(register::INTMS, u32::MAX);

    let data = DmaPage::new(dma).ok_or("no memory for a data page")?;
    let failed = |e: Failure| -> &'static str {
        match e {
            Failure::Full => "queue full",
            Failure::Timeout => "a command timed out",
            Failure::WrongCommand => "a completion for another command",
            Failure::Dead => "a command on a queue that lost an earlier one",
            Failure::Busy => "a command on a queue with one outstanding",
        }
    };
    let id = c
        .run(Q::Admin, |cid| Command::identify(cid, 1, 0, data.bus))
        .map_err(failed)?;
    if !id.status.ok() {
        return Err("Identify Controller failed");
    }
    let mut page = [0u8; 4096];
    page.copy_from_slice(data.bytes());
    let info = nvme::identify_controller(&page);
    let _ = writeln!(
        com1,
        "origin-kernel: NVMe: controller ready: model \"{}\", serial \"{}\", firmware \"{}\", {} namespaces, MDTS {}",
        nvme::text(&info.model),
        nvme::text(&info.serial),
        nvme::text(&info.firmware),
        info.namespaces,
        info.mdts
    );
    let ns = c
        .run(Q::Admin, |cid| Command::identify(cid, 0, NSID, data.bus))
        .map_err(failed)?;
    page.copy_from_slice(data.bytes());
    let Some(namespace) = ns
        .status
        .ok()
        .then(|| nvme::identify_namespace(&page))
        .flatten()
    else {
        return Err("namespace 1 is not usable");
    };
    if !namespace.block_fits_page(PAGE) {
        // A longer block needs a PRP list per block, and PRP1 alone would overrun the page.
        return Err("namespace 1's blocks are larger than a page");
    }
    let _ = writeln!(
        com1,
        "origin-kernel: NVMe: namespace {NSID}: {} blocks of {} B ({} MiB)",
        namespace.blocks,
        namespace.block_size,
        (namespace.blocks * u64::from(namespace.block_size)) >> 20
    );
    let granted = c
        .run(Q::Admin, |cid| {
            Command::set_number_of_queues(cid, wanted, wanted)
        })
        .map_err(failed)?;
    let (sqs, cqs) = nvme::allocated_queues(granted.result);
    if !granted.status.ok() || sqs < u32::from(wanted) || cqs < u32::from(wanted) {
        return Err("the controller allocated too few I/O queues");
    }
    let _ = writeln!(
        com1,
        "origin-kernel: NVMe: asked for {wanted} I/O queue pairs, allocated {sqs} submission and {cqs} completion queues"
    );
    let io = new_queue(dma, 1, IO_ENTRIES).ok_or("no memory for the I/O queues")?;
    let (cq_bus, sq_bus) = (io.cq.bus, io.sq.bus);
    let created = c
        .run(Q::Admin, |cid| {
            Command::create_io_cq(cid, 1, IO_ENTRIES, cq_bus)
        })
        .map_err(failed)?;
    if !created.status.ok() {
        return Err("Create I/O Completion Queue failed");
    }
    let created = c
        .run(Q::Admin, |cid| {
            Command::create_io_sq(cid, 1, IO_ENTRIES, 1, sq_bus)
        })
        .map_err(failed)?;
    if !created.status.ok() {
        return Err("Create I/O Submission Queue failed");
    }
    c.io = Some(io);
    let on_test_disk = is_test_disk(&mut c, &data, namespace).map_err(failed)?;
    let ok = prove_io(com1, &mut c, &data, namespace, on_test_disk).map_err(failed)?;
    let gpt = prove_gpt(com1, &mut c, &data, namespace, on_test_disk).map_err(failed)?;
    Ok((c, ok && gpt, namespace, on_test_disk))
}

/// Whether namespace 1 is the disk the boot test builds, the only one the boot proofs may
/// write to (CLAUDE.md P0). Reads only, and nothing at all on a disk of any other geometry.
fn is_test_disk(c: &mut Nvme, data: &DmaPage, ns: NamespaceInfo) -> Result<bool, Failure> {
    if !test_disk::geometry_matches(ns.blocks, ns.block_size) {
        return Ok(false);
    }
    let mut header = [0u8; test_disk::BLOCK_SIZE as usize];
    let mut area = alloc::vec![0u8; test_disk::PATTERN_BYTES];
    Ok(read_blocks(c, data, ns, 1, &mut header)?
        && read_blocks(c, data, ns, test_disk::PATTERN_FIRST, &mut area)?
        && test_disk::identifies(ns.blocks, ns.block_size, &header, &area))
}

/// Reads the interrupt-driven proof makes, of 8 blocks each.
const IRQ_READS: u64 = 64;
/// What the interrupt-driven thread found: reads done, blocks that matched, and the TSC
/// cycles they took (each plus one once set; 0: not finished).
static IRQ_RESULT: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// Programs MSI-X for I/O queue 2 (vector 1, sent to processor `target` at
/// `cpu::DEVICE_VECTOR`, every other entry masked), turns MSI-X on, and creates the queue
/// pair with interrupts. Returns the MSI-X capability.
fn setup_interrupts(d: &mut Driver, target: u32) -> Result<pci::Msix, &'static str> {
    let msix = crate::pcie::msix(d.dev.function.at).ok_or("no MSI-X capability")?;
    if msix.entries < 2 {
        return Err("fewer than two MSI-X vectors");
    }
    let bar = d
        .dev
        .bars
        .get(usize::from(msix.table_bar))
        .copied()
        .flatten()
        .ok_or("the MSI-X table's BAR is not implemented")?;
    let table_len = u64::from(msix.entries) * pci::MSIX_ENTRY;
    let table_address = msix
        .table_address(&bar)
        .ok_or("the MSI-X table lies outside its memory BAR")?;
    let table = if msix.table_bar == 0 {
        d.nvme.regs + u64::from(msix.table_offset)
    } else {
        crate::vm::map_mmio(table_address, table_len).map_err(|_| "cannot map the MSI-X table")?
    };
    let message = crate::iommu::msi_message(d.dev.function.at, target, crate::cpu::DEVICE_VECTOR)?;
    let entry = |i: u64, word: u64, value: u32| {
        let p = ptr::with_exposed_provenance_mut::<u32>(
            (table + i * pci::MSIX_ENTRY + 4 * word) as usize,
        );
        // SAFETY: inside the MSI-X table (checked against its BAR above), mapped uncached;
        // the entries only say where the device's interrupt messages go.
        unsafe { p.write_volatile(value) };
    };
    for i in 0..u64::from(msix.entries) {
        entry(i, 3, pci::MSIX_MASKED);
    }
    entry(1, 0, message.address as u32);
    entry(1, 1, (message.address >> 32) as u32);
    entry(1, 2, message.data);
    entry(1, 3, 0);
    let control =
        crate::pcie::enable_msix(d.dev.function.at, &msix).ok_or("no configuration access")?;
    if control & pci::MSIX_ENABLE == 0 || control & pci::MSIX_FUNCTION_MASK != 0 {
        return Err("MSI-X did not turn on");
    }
    let queue = new_queue(d.nvme.dma, 2, IO_ENTRIES).ok_or("no memory for I/O queue 2")?;
    let (cq, sq) = (queue.cq.bus, queue.sq.bus);
    let failed = |_| "creating I/O queue 2 failed";
    let made = d
        .nvme
        .run(Q::Admin, |cid| {
            Command::create_io_cq_with_interrupts(cid, 2, IO_ENTRIES, 1, cq)
        })
        .map_err(failed)?;
    if !made.status.ok() {
        return Err("Create I/O Completion Queue 2 failed");
    }
    let made = d
        .nvme
        .run(Q::Admin, |cid| {
            Command::create_io_sq(cid, 2, IO_ENTRIES, 2, sq)
        })
        .map_err(failed)?;
    if !made.status.ok() {
        return Err("Create I/O Submission Queue 2 failed");
    }
    d.nvme.irq = Some(queue);
    d.msix = Some((table, msix));
    Ok(msix)
}

/// The interrupt-driven proof's thread: [`IRQ_READS`] reads through I/O queue 2, each
/// submitted under the driver lock and then waited for by blocking until the completion
/// interrupt wakes it. The wait's condition is the completion queue itself, so a
/// completion that lands before the thread blocks is never missed.
extern "sysv64" fn io_thread(_: u64) -> ! {
    let start = crate::cpu::rdtsc();
    let (mut reads, mut matched) = (0u64, 0u64);
    let mut copy = [0u8; 4096];
    let mut expected = [0u8; 512];
    for i in 0..IRQ_READS {
        let lba = 2048 + (i % 8) * 8;
        let cid = crate::cpu::without_interrupts(|| {
            let mut g = DRIVER.lock();
            let d = g.as_mut()?;
            let prps = (d.page.bus, 0);
            d.nvme
                .submit(Q::Interrupt, |cid| {
                    Command::transfer(false, cid, NSID, lba, 8, prps)
                })
                .ok()
        });
        let Some(cid) = cid else {
            break;
        };
        let done = core::cell::Cell::new(None);
        let deadline = crate::time::monotonic_nanos() + COMMAND_TIMEOUT_NS;
        let Some(wait) = IO_WAIT.get(crate::cpu::this_cpu().get()) else {
            break;
        };
        let waited = wait.wait_until_deadline(
            || {
                let r = DRIVER
                    .lock()
                    .as_mut()
                    .and_then(|d| d.nvme.poll(Q::Interrupt, cid));
                let ready = r.is_some();
                if ready {
                    done.set(r);
                }
                ready
            },
            deadline,
        );
        count_blocks(*waited.as_ref().unwrap_or(&0));
        let Some(Ok(c)) = done.get() else {
            // No completion for it, so it may still be live: the queue, and the page with it,
            // is never used again.
            crate::cpu::without_interrupts(|| {
                if let Some(d) = DRIVER.lock().as_mut() {
                    d.nvme.kill(Q::Interrupt);
                }
            });
            break;
        };
        if waited.is_err() || !c.status.ok() {
            break;
        }
        crate::cpu::without_interrupts(|| {
            if let Some(d) = DRIVER.lock().as_ref() {
                copy.copy_from_slice(d.page.bytes());
            }
        });
        for k in 0..8u64 {
            pattern(lba + k, &mut expected);
            let at = (k * 512) as usize;
            if copy.get(at..at + 512) == Some(&expected[..]) {
                matched += 1;
            }
        }
        reads += 1;
    }
    let cycles = crate::cpu::rdtsc().wrapping_sub(start);
    for (slot, v) in IRQ_RESULT.iter().zip([reads, matched, cycles]) {
        slot.store(v + 1, Ordering::Release);
    }
    crate::sched::exit()
}

/// Proves interrupt-driven completions: MSI-X set up for a second I/O queue, then a thread
/// on another processor reads through it, blocking for each completion.
fn prove_interrupts(com1: &mut Console, cpus: &[CpuIndex], tsc_hz: u64) -> bool {
    let bsp = crate::cpu::this_cpu();
    let target = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let setup = with_driver_out(|d| {
        let apic_id = target
            .apic_id()
            .ok_or("the target processor has no APIC ID")?;
        setup_interrupts(d, apic_id)
    })
    .unwrap_or(Err("no driver"));
    let msix = match setup {
        Ok(m) => m,
        Err(why) => {
            let _ = writeln!(com1, "origin-kernel: NVMe: MSI-X: {why}: NVMe failed");
            return false;
        }
    };
    let _ = writeln!(
        com1,
        "origin-kernel: NVMe: MSI-X: {} vectors, table in BAR{} at offset {:#x}; I/O queue 2 completes to vector 1, sent to {} as IDT vector {:#x}",
        msix.entries,
        msix.table_bar,
        msix.table_offset,
        target.named(),
        crate::cpu::DEVICE_VECTOR
    );
    if crate::sched::spawn(target, io_thread, 0).is_err() {
        let _ = writeln!(
            com1,
            "origin-kernel: NVMe: cannot spawn the I/O thread: NVMe failed"
        );
        return false;
    }
    let finished = || IRQ_RESULT.iter().all(|r| r.load(Ordering::Acquire) != 0);
    crate::sched::yield_until(finished, tsc_hz, 10);
    let get = |i: usize| {
        IRQ_RESULT
            .get(i)
            .map_or(0, |r| r.load(Ordering::Acquire).saturating_sub(1))
    };
    let (reads, matched, cycles) = (get(0), get(1), get(2));
    let interrupts = INTERRUPTS.load(Ordering::Relaxed);
    let on = INTERRUPT_CPU.load(Ordering::Relaxed).saturating_sub(1);
    let ok = finished()
        && reads == IRQ_READS
        && matched == 8 * IRQ_READS
        && interrupts >= IRQ_READS
        && on == target.get() as u64;
    let micros = cycles / reads.max(1) / (tsc_hz / 1_000_000).max(1);
    let _ = writeln!(
        com1,
        "origin-kernel: NVMe: interrupt-driven: {reads} reads of 8 blocks by a thread on {}, each blocking until its completion interrupt: {interrupts} interrupts ({}), {matched} of {} blocks match, {micros} us per read{}",
        target.named(),
        if interrupts == 0 {
            alloc::string::String::from("none arrived")
        } else {
            match usize::try_from(on).ok().and_then(CpuIndex::new) {
                Some(c) => alloc::format!("the last on {}", c.named()),
                None => alloc::string::String::from("the last on a processor without a CPU index"),
            }
        },
        8 * IRQ_READS,
        if ok { "" } else { ": NVMe failed" }
    );
    ok
}

/// Reads whole blocks from `lba` into `out`, a page at a time.
fn read_blocks(
    c: &mut Nvme,
    data: &DmaPage,
    ns: NamespaceInfo,
    lba: u64,
    out: &mut [u8],
) -> Result<bool, Failure> {
    let bs = u64::from(ns.block_size);
    let per_page = (PAGE / bs).max(1);
    let mut done = 0u64;
    for chunk in out.chunks_mut(PAGE as usize) {
        let blocks = (chunk.len() as u64).div_ceil(bs).min(per_page);
        let prps = nvme::prps(data.bus, blocks * bs, PAGE).unwrap_or((data.bus, 0));
        let at = lba + done;
        let r = c.run(Q::Polled, |cid| {
            Command::transfer(false, cid, NSID, at, blocks as u32, prps)
        })?;
        if !r.status.ok() {
            return Ok(false);
        }
        let n = chunk.len();
        chunk.copy_from_slice(data.bytes().get(..n).unwrap_or(&[]));
        done += blocks;
    }
    Ok(true)
}

/// A GPT copy's validated header, and how many of its entries are in use.
type GptCopy = Result<(gpt::Header, usize), gpt::GptError>;

/// One GPT copy: its header from block `lba` and its entry array, both validated.
fn read_gpt_copy(
    c: &mut Nvme,
    data: &DmaPage,
    ns: NamespaceInfo,
    lba: u64,
    parts: &mut [gpt::Partition],
) -> Result<GptCopy, Failure> {
    let mut block = alloc::vec![0u8; ns.block_size as usize];
    if !read_blocks(c, data, ns, lba, &mut block)? {
        return Ok(Err(gpt::GptError::Signature));
    }
    let header = match gpt::parse_header(&block, lba, ns.blocks) {
        Ok(h) => h,
        Err(e) => return Ok(Err(e)),
    };
    let bytes = header.array_blocks(u64::from(ns.block_size)) * u64::from(ns.block_size);
    let mut array = alloc::vec![0u8; bytes as usize];
    if !read_blocks(c, data, ns, header.entries_lba, &mut array)? {
        return Ok(Err(gpt::GptError::ArrayCrc));
    }
    Ok(gpt::parse_entries(&array, &header, parts).map(|n| (header, n)))
}

/// A copy's state for the report: `valid`, or why not.
struct State<'a>(&'a GptCopy);

impl core::fmt::Display for State<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Ok(_) => f.write_str("valid"),
            Err(e) => write!(f, "invalid ({e:?})"),
        }
    }
}

/// Reads and checks both GPT copies (UEFI 2.11 §5.3.2) and lists the partitions. Both must
/// be valid and agree; with only one, the disk is reported from it, and the proof fails. On
/// a disk that is not the boot test's, which need not have a GPT at all, this only reports.
fn prove_gpt(
    com1: &mut Console,
    c: &mut Nvme,
    data: &DmaPage,
    ns: NamespaceInfo,
    on_test_disk: bool,
) -> Result<bool, Failure> {
    let empty = gpt::Partition {
        index: 0,
        kind: gpt::Guid::default(),
        unique: gpt::Guid::default(),
        first: 0,
        last: 0,
        attributes: 0,
        name: [0; 36],
    };
    let mut parts = [empty; 16];
    let mut backup_parts = [empty; 16];
    let primary = read_gpt_copy(c, data, ns, 1, &mut parts)?;
    let backup_lba = match primary {
        Ok((h, _)) => h.alternate_lba,
        Err(_) => ns.blocks - 1,
    };
    let backup = read_gpt_copy(c, data, ns, backup_lba, &mut backup_parts)?;
    let agree = match (&primary, &backup) {
        (Ok((p, n)), Ok((b, m))) => p.agrees_with(b) && parts.get(..*n) == backup_parts.get(..*m),
        _ => false,
    };
    let passed = agree || !on_test_disk;
    let _ = writeln!(
        com1,
        "origin-kernel: GPT: primary copy {}, backup copy at LBA {backup_lba} {}, the two agree {agree}{}",
        State(&primary),
        State(&backup),
        if passed { "" } else { ": GPT failed" }
    );
    let (list, (h, n)) = match (&primary, &backup) {
        (Ok(p), _) => (&parts, p),
        (Err(_), Ok(b)) => (&backup_parts, b),
        _ => return Ok(passed),
    };
    let _ = writeln!(
        com1,
        "origin-kernel: GPT: disk {}, usable blocks {}-{}, {} entries of {} B, {n} in use",
        h.disk, h.first_usable, h.last_usable, h.entry_count, h.entry_size
    );
    for p in list.iter().take(*n) {
        let _ = writeln!(
            com1,
            "origin-kernel: GPT: partition {}: \"{}\", type {}, blocks {}-{} ({} MiB), unique {}",
            p.index + 1,
            p.name(),
            p.kind,
            p.first,
            p.last,
            (p.blocks() * u64::from(ns.block_size)) >> 20,
            p.unique
        );
    }
    Ok(passed)
}

/// Reads blocks the boot test wrote, writes and reads back one, and checks that a read past
/// the end fails. Only on the boot test's disk: on any other this says so, once, and passes
/// having issued no write and no flush (CLAUDE.md P0).
fn prove_io(
    com1: &mut Console,
    c: &mut Nvme,
    data: &DmaPage,
    ns: NamespaceInfo,
    on_test_disk: bool,
) -> Result<bool, Failure> {
    if !on_test_disk {
        let _ = writeln!(
            com1,
            "origin-kernel: NVMe: write proofs skipped: this is not the boot test's disk, so nothing is written to it or flushed during boot"
        );
        return Ok(true);
    }
    let bs = u64::from(ns.block_size);
    let read = |c: &mut Nvme, lba: u64, blocks: u32| {
        let prps = nvme::prps(data.bus, u64::from(blocks) * bs, PAGE).unwrap_or((data.bus, 0));
        c.run(Q::Polled, |cid| {
            Command::transfer(false, cid, NSID, lba, blocks, prps)
        })
    };
    let mbr = read(c, 0, 1)?;
    let signature = mbr.status.ok() && data.bytes().get(510..512) == Some(&[0x55, 0xAA][..]);
    let header = read(c, 1, 1)?;
    let efi = header.status.ok() && data.bytes().get(0..8) == Some(&b"EFI PART"[..]);
    let blocks = (PAGE / bs) as u32;
    let first = 2048;
    let bulk = read(c, first, blocks)?;
    let mut expected = [0u8; 512];
    let mut matching = 0;
    if bulk.status.ok() && bs == 512 {
        for i in 0..u64::from(blocks) {
            pattern(first + i, &mut expected);
            let at = (i * bs) as usize;
            if data.bytes().get(at..at + 512) == Some(&expected[..]) {
                matching += 1;
            }
        }
    }
    let reads_ok = signature && efi && matching == blocks;
    let _ = writeln!(
        com1,
        "origin-kernel: NVMe: through I/O queue 1: LBA 0 has the MBR signature {signature}, LBA 1 the GPT signature {efi}, and {matching} of {blocks} blocks from LBA {first} match the boot test's pattern{}",
        if reads_ok { "" } else { ": NVMe failed" }
    );
    // A write, a flush, and the block read back into the same (cleared) page.
    let scratch = ns.blocks.saturating_sub(64);
    let mut block = [0u8; 512];
    pattern(scratch ^ 0xA5A5, &mut block);
    data.fill(&block);
    let prps = (data.bus, 0);
    let wrote = c.run(Q::Polled, |cid| {
        Command::transfer(true, cid, NSID, scratch, 1, prps)
    })?;
    let flushed = c.run(Q::Polled, |cid| Command::flush(cid, NSID))?;
    data.fill(&[0u8; 512]);
    let back = read(c, scratch, 1)?;
    let round_trip = wrote.status.ok()
        && flushed.status.ok()
        && back.status.ok()
        && data.bytes().get(0..512) == Some(&block[..]);
    // A read past the end must fail with "LBA out of range", not succeed or hang.
    let past = read(c, ns.blocks, 1)?;
    let refused = past.status == nvme::LBA_OUT_OF_RANGE;
    let ok = round_trip && refused;
    let _ = writeln!(
        com1,
        "origin-kernel: NVMe: wrote LBA {scratch}, flushed and read it back: match {round_trip}; a read at LBA {} (past the end) failed with status type {} code {:#x} (LBA out of range: {refused}){}",
        ns.blocks,
        past.status.kind,
        past.status.code,
        if ok { "" } else { ": NVMe failed" }
    );
    Ok(reads_ok && ok)
}

/// One processor's own I/O queue pair, with its controller's doorbell addresses and a data
/// page: only threads on that processor use it, so it never waits on another's lock.
struct CpuQueue {
    qp: QueuePair,
    page: DmaPage,
    regs: u64,
    stride: u64,
}

static CPU_QUEUES: [SpinLock<Option<CpuQueue>>; MAX_CPUS] =
    [const { SpinLock::new(None) }; MAX_CPUS];

/// Reads each processor's thread makes, of 8 blocks each.
const CPU_READS: u64 = 32;
/// Per processor: reads done, blocks matched, and whether its write read back (each plus
/// one once set).
static CPU_RESULT: [[AtomicU64; 3]; MAX_CPUS] =
    [const { [const { AtomicU64::new(0) }; 3] }; MAX_CPUS];

/// For each online processor `k` (by position in `cpus`): MSI-X entry `2 + k` sent to its
/// APIC ID, and I/O queue pair `3 + k` completing to that entry.
fn setup_cpu_queues(d: &mut Driver, cpus: &[CpuIndex]) -> Result<(), &'static str> {
    let (table, msix) = d.msix.ok_or("MSI-X is not set up")?;
    if usize::from(msix.entries) < 2 + cpus.len() {
        return Err("too few MSI-X vectors for one per processor");
    }
    for (k, &cpu) in cpus.iter().enumerate() {
        let vector = 2 + k as u16;
        let qid = 3 + k as u16;
        let apic_id = cpu.apic_id().ok_or("a processor has no APIC ID")?;
        let message =
            crate::iommu::msi_message(d.dev.function.at, apic_id, crate::cpu::DEVICE_VECTOR)?;
        let entry = |word: u64, value: u32| {
            let at = table + u64::from(vector) * pci::MSIX_ENTRY + 4 * word;
            let p = ptr::with_exposed_provenance_mut::<u32>(at as usize);
            // SAFETY: inside the MSI-X table (`setup_interrupts` checked its bounds against
            // its BAR, and `vector` is below its entry count), mapped uncached.
            unsafe { p.write_volatile(value) };
        };
        entry(0, message.address as u32);
        entry(1, (message.address >> 32) as u32);
        entry(2, message.data);
        entry(3, 0);
        let queue =
            new_queue(d.nvme.dma, qid, IO_ENTRIES).ok_or("no memory for a per-processor queue")?;
        let page = DmaPage::new(d.nvme.dma).ok_or("no memory for a per-processor page")?;
        let (cq, sq) = (queue.cq.bus, queue.sq.bus);
        let failed = |_| "creating a per-processor queue failed";
        let made = d
            .nvme
            .run(Q::Admin, |cid| {
                Command::create_io_cq_with_interrupts(cid, qid, IO_ENTRIES, vector, cq)
            })
            .map_err(failed)?;
        let made_sq = d
            .nvme
            .run(Q::Admin, |cid| {
                Command::create_io_sq(cid, qid, IO_ENTRIES, qid, sq)
            })
            .map_err(failed)?;
        if !made.status.ok() || !made_sq.status.ok() {
            return Err("a per-processor queue was refused");
        }
        let slot = CPU_QUEUES.get(cpu.get()).ok_or("CPU index out of range")?;
        crate::cpu::without_interrupts(|| {
            *slot.lock() = Some(CpuQueue {
                qp: queue,
                page,
                regs: d.nvme.regs,
                stride: d.nvme.cap.doorbell_stride,
            });
        });
    }
    Ok(())
}

/// One command on this processor's own queue: submitted under its lock, then waited for by
/// blocking until its interrupt (the wait's condition is the completion queue itself).
/// Returns the page's contents for a read.
fn cpu_command(me: usize, make: impl FnOnce(u16, u64) -> Command, out: &mut [u8; 4096]) -> bool {
    let (Some(slot), Some(wait)) = (CPU_QUEUES.get(me), IO_WAIT.get(me)) else {
        return false;
    };
    let cid = crate::cpu::without_interrupts(|| {
        let mut g = slot.lock();
        let q = g.as_mut()?;
        let bus = q.page.bus;
        submit_on(&mut q.qp, q.regs, q.stride, |cid| make(cid, bus)).ok()
    });
    let Some(cid) = cid else {
        return false;
    };
    let done = core::cell::Cell::new(None);
    let deadline = crate::time::monotonic_nanos() + COMMAND_TIMEOUT_NS;
    let waited = wait.wait_until_deadline(
        || {
            let r = slot
                .lock()
                .as_mut()
                .and_then(|q| poll_on(&mut q.qp, q.regs, q.stride, cid));
            let ready = r.is_some();
            if ready {
                done.set(r);
            }
            ready
        },
        deadline,
    );
    count_blocks(*waited.as_ref().unwrap_or(&0));
    if !matches!(done.get(), Some(Ok(_))) {
        // No completion for it, so it may still be live: the queue, and the page with it, is
        // never used again.
        crate::cpu::without_interrupts(|| {
            if let Some(q) = slot.lock().as_mut() {
                q.qp.gate.kill();
            }
        });
    }
    let ok = waited.is_ok() && matches!(done.get(), Some(Ok(c)) if c.status.ok());
    if ok {
        crate::cpu::without_interrupts(|| {
            if let Some(q) = slot.lock().as_ref() {
                out.copy_from_slice(q.page.bytes());
            }
        });
    }
    ok
}

/// A processor's thread: [`CPU_READS`] reads of the pattern blocks through its own queue,
/// then one block of its own written, flushed and read back.
extern "sysv64" fn cpu_io_thread(index: u64) -> ! {
    let me = crate::cpu::this_cpu().get();
    let (mut reads, mut matched) = (0u64, 0u64);
    let mut page = [0u8; 4096];
    let mut expected = [0u8; 512];
    for i in 0..CPU_READS {
        let lba = 2048 + ((i + index) % 8) * 8;
        if !cpu_command(
            me,
            |cid, bus| Command::transfer(false, cid, NSID, lba, 8, (bus, 0)),
            &mut page,
        ) {
            break;
        }
        for k in 0..8u64 {
            pattern(lba + k, &mut expected);
            let at = (k * 512) as usize;
            if page.get(at..at + 512) == Some(&expected[..]) {
                matched += 1;
            }
        }
        reads += 1;
    }
    // A block of its own, on the boot test's disk and no other: written from this processor's
    // page, flushed, cleared, read back.
    let scratch = 20_000 + 8 * index;
    pattern(scratch ^ 0x5A5A, &mut expected);
    let filled = on_test_disk()
        && crate::cpu::without_interrupts(|| {
            let g = CPU_QUEUES.get(me).map(|s| s.lock());
            g.as_ref()
                .and_then(|g| g.as_ref())
                .map(|q| q.page.fill(&expected))
                .is_some()
        });
    let wrote = filled
        && cpu_command(
            me,
            |cid, bus| Command::transfer(true, cid, NSID, scratch, 1, (bus, 0)),
            &mut page,
        )
        && cpu_command(me, |cid, _| Command::flush(cid, NSID), &mut page);
    crate::cpu::without_interrupts(|| {
        if let Some(q) = CPU_QUEUES
            .get(me)
            .and_then(|s| s.lock().as_ref().map(|q| q.page))
        {
            q.fill(&[0u8; 512]);
        }
    });
    let back = wrote
        && cpu_command(
            me,
            |cid, bus| Command::transfer(false, cid, NSID, scratch, 1, (bus, 0)),
            &mut page,
        )
        && page.get(..512) == Some(&expected[..]);
    if let Some(r) = CPU_RESULT.get(me) {
        for (slot, v) in r.iter().zip([reads, matched, u64::from(back)]) {
            slot.store(v + 1, Ordering::Release);
        }
    }
    crate::sched::exit()
}

/// Every online processor reads and writes through its own queue pair and vector at the
/// same time: every read must match, every write read back, and each processor's
/// interrupts must have arrived on that processor.
fn prove_per_cpu(com1: &mut Console, cpus: &[CpuIndex], tsc_hz: u64) -> bool {
    let setup = with_driver_out(|d| setup_cpu_queues(d, cpus)).unwrap_or(Err("no driver"));
    if let Err(why) = setup {
        let _ = writeln!(
            com1,
            "origin-kernel: NVMe: per-CPU queues: {why}: NVMe failed"
        );
        return false;
    }
    let before: [u64; MAX_CPUS] =
        core::array::from_fn(|i| CPU_INTERRUPTS[i].load(Ordering::Relaxed));
    let start = crate::cpu::rdtsc();
    for (k, &cpu) in cpus.iter().enumerate() {
        if crate::sched::spawn(cpu, cpu_io_thread, k as u64).is_err() {
            let _ = writeln!(
                com1,
                "origin-kernel: NVMe: per-CPU queues: cannot spawn: NVMe failed"
            );
            return false;
        }
    }
    let finished = || {
        cpus.iter().all(|&c| {
            CPU_RESULT
                .get(c.get())
                .is_some_and(|r| r.iter().all(|x| x.load(Ordering::Acquire) != 0))
        })
    };
    crate::sched::yield_until(finished, tsc_hz, 10);
    let micros = crate::cpu::rdtsc().wrapping_sub(start) / (tsc_hz / 1_000_000).max(1);
    let mut ok = finished();
    let (mut reads, mut matched) = (0, 0);
    for &cpu in cpus {
        let c = cpu.get();
        let get = |i: usize| {
            CPU_RESULT
                .get(c)
                .map_or(0, |r| r[i].load(Ordering::Acquire).saturating_sub(1))
        };
        let interrupts = CPU_INTERRUPTS
            .get(c)
            .map_or(0, |n| n.load(Ordering::Relaxed))
            - before.get(c).copied().unwrap_or(0);
        let (r, m, back) = (get(0), get(1), get(2) == 1);
        let mine = r == CPU_READS && m == 8 * CPU_READS && back && interrupts >= CPU_READS + 3;
        ok &= mine;
        reads += r;
        matched += m;
        let _ = writeln!(
            com1,
            "origin-kernel: NVMe: {}: {r} reads through its own queue pair, {m} of {} blocks match, its write read back {back}, {interrupts} interrupts on CPU {cpu}{}",
            cpu.named(),
            8 * CPU_READS,
            if mine { "" } else { ": NVMe failed" }
        );
    }
    let mib = (reads * 4096 * 1_000_000 / micros.max(1)) >> 20;
    let _ = writeln!(
        com1,
        "origin-kernel: NVMe: per-CPU queues: {} threads at once, one per CPU, each with its own queue pair and MSI-X vector: {reads} reads, {matched} of {} blocks match, in {} ms ({mib} MiB/s){}",
        cpus.len(),
        8 * reads,
        micros / 1000,
        if ok { "" } else { ": NVMe failed" }
    );
    ok
}

/// For every online processor whose x2APIC ID is above 255, proves that NVMe interrupts reach
/// it live through interrupt remapping (CLAUDE.md §24, §32; ADR-0007 I8, ADR-0008):
/// - a compatibility-format MSI address cannot carry its ID (8 bits), so the kernel's
///   compatibility path refuses it;
/// - its per-CPU MSI-X entry, programmed through `iommu::msi_message`, holds only a
///   remappable handle (data 0), and the IRTE behind it holds the vector, the full 32-bit
///   APIC ID and the check of the controller's source ID;
/// - the completions of its own queue pair arrived as interrupts on that processor (each
///   counted by the handler running there, which knows its CPU index from its CPU-local
///   area);
/// - every time a thread there blocked for a completion, an interrupt taken there woke it:
///   blocks and wake-ups are equal, and not zero. An interrupt sent anywhere else wakes
///   nobody, and the thread would sleep until its deadline instead.
///
/// With no such processor (APIC IDs 0-255) there is nothing beyond compatibility format to
/// prove; that is reported, and passes.
fn prove_x2apic(com1: &mut Console, cpus: &[CpuIndex]) -> bool {
    let wide: Vec<(usize, CpuIndex, u32)> = cpus
        .iter()
        .enumerate()
        .filter_map(|(k, &c)| c.apic_id().map(|id| (k, c, id)))
        .filter(|&(_, _, id)| id > 0xFF)
        .collect();
    if wide.is_empty() {
        let largest = cpus.iter().filter_map(|c| c.apic_id()).max().unwrap_or(0);
        let _ = writeln!(
            com1,
            "origin-kernel: x2APIC: no online processor has an APIC ID above 255 (the largest is {largest}): nothing beyond compatibility-format MSI to prove"
        );
        return true;
    }
    let driver = crate::cpu::without_interrupts(|| {
        DRIVER
            .lock()
            .as_ref()
            .map(|d| (d.dev.function.at, d.msix.map(|(table, _)| table)))
    });
    let Some((at, Some(table))) = driver else {
        let _ = writeln!(
            com1,
            "origin-kernel: x2APIC: NVMe MSI-X is not set up: NVMe failed"
        );
        return false;
    };
    let mut ok = true;
    for (k, cpu, apic_id) in wide {
        let entry = 2 + k as u64;
        let message = msix_message(table, entry);
        let refused = pci::msi_address(apic_id).is_none();
        let routed = routed_through_irte(at, message, crate::cpu::DEVICE_VECTOR, apic_id);
        let irte = vtd::interrupt_index(message);
        let c = cpu.get();
        let read =
            |counts: &[AtomicU64; MAX_CPUS]| counts.get(c).map_or(0, |n| n.load(Ordering::Relaxed));
        let (interrupts, blocks, wakes) =
            (read(&CPU_INTERRUPTS), read(&CPU_BLOCKS), read(&CPU_WAKES));
        let mine =
            refused && routed && interrupts >= CPU_READS + 3 && blocks > 0 && wakes == blocks;
        ok &= mine;
        let _ = writeln!(
            com1,
            "origin-kernel: x2APIC: {} is above 255, which a compatibility-format MSI address cannot carry (refused {refused}); its NVMe MSI-X entry {entry} holds only remappable handle {}, whose IRTE sends vector {:#x} to destination {apic_id} after checking source ID {:#06x}: {routed}; {interrupts} NVMe interrupts arrived there, and its threads blocked {blocks} times for a completion and were woken {wakes} times by an interrupt taken there; consistent {mine}{}",
            cpu.named(),
            irte.map_or(alloc::string::String::from("none"), |i| alloc::format!(
                "{i}"
            )),
            crate::cpu::DEVICE_VECTOR,
            crate::iommu::source_id(at),
            if mine { "" } else { ": NVMe failed" }
        );
    }
    ok
}

/// A recognizable fill for a page no DMA may reach.
const CANARY: u8 = 0xCA;
/// The block the IOMMU proofs read (a pattern block the boot test wrote).
const PROBE_LBA: u64 = 2048;

/// Reads word `word` of MSI-X table entry `entry` (the table mapped at `table`).
fn msix_word(table: u64, entry: u64, word: u64) -> u32 {
    let p =
        ptr::with_exposed_provenance::<u32>((table + entry * pci::MSIX_ENTRY + 4 * word) as usize);
    // SAFETY: the callers pass entries below the table's entry count; `setup_interrupts`
    // checked the table against its BAR, which is mapped uncached. Reading an entry has no
    // effect on the device.
    unsafe { p.read_volatile() }
}

/// Writes word `word` of MSI-X table entry `entry`.
fn msix_write(table: u64, entry: u64, word: u64, value: u32) {
    let p = ptr::with_exposed_provenance_mut::<u32>(
        (table + entry * pci::MSIX_ENTRY + 4 * word) as usize,
    );
    // SAFETY: as for `msix_word`; the entry only says where the device's message goes.
    unsafe { p.write_volatile(value) };
}

/// The message in MSI-X table entry `entry`.
fn msix_message(table: u64, entry: u64) -> vtd::MsiMessage {
    vtd::MsiMessage {
        address: u64::from(msix_word(table, entry, 0))
            | u64::from(msix_word(table, entry, 1)) << 32,
        data: msix_word(table, entry, 2),
    }
}

/// Points MSI-X entry `entry` at `m`: masked while it changes, then unmasked.
fn msix_point(table: u64, entry: u64, m: vtd::MsiMessage) {
    msix_write(table, entry, 3, pci::MSIX_MASKED);
    msix_write(table, entry, 0, m.address as u32);
    msix_write(table, entry, 1, (m.address >> 32) as u32);
    msix_write(table, entry, 2, m.data);
    msix_write(table, entry, 3, 0);
}

/// Whether `m` is a handle to an IRTE (VT-d: remappable format with data 0, on `at`'s unit;
/// AMD-Vi: an index into `at`'s own table) that sends `vector` to `apic` and that only `at`
/// can use.
pub(crate) fn routed_through_irte(
    at: pci::Address,
    m: vtd::MsiMessage,
    vector: u8,
    apic: u32,
) -> bool {
    crate::iommu::route(at, m)
        .is_some_and(|r| r.vector == vector && r.destination == apic && r.source_checked)
}

/// The IOMMU proofs that need the bound controller (CLAUDE.md §32, §58; ADR-0007):
/// 1. **translated DMA**: a read into a fresh page lands in that physical page, although the
///    controller was given only its IOVA — never its physical address — and the domain's
///    tables translate the one to the other;
/// 2. **blocked DMA**: once that IOVA is unmapped (the translation cache invalidated and the
///    invalidation waited for), the same read leaves the page untouched, and the IOMMU
///    records the fault: the controller's source ID, the IOVA, a page not mapped (VT-d: a
///    write, reason 5; AMD-Vi: an I/O page fault with PR clear);
/// 3. **remapped interrupts**: every MSI-X entry the NVMe and virtio-net drivers programmed
///    holds only a handle to an IRTE (VT-d: remappable format with data 0; AMD-Vi: an index
///    into the device's own table), and each IRTE holds the vector and the destination and
///    can be used only by its device; and an APIC ID above 255, which compatibility format
///    cannot address, is accepted (its IRTE read back, then freed);
/// 4. **a blocked interrupt**: vector 1 pointed at a not-present IRTE, a completion on I/O
///    queue 2 raises no interrupt, and the IOMMU records the fault (VT-d: the source ID, the
///    index, reason 22h; AMD-Vi: the DeviceID, an interrupt I/O page fault with PR clear).
///    Vector 1 is restored afterwards.
///
/// Faults recorded before the proofs are reported; one from an attached device fails the
/// proof (a driver's DMA went where it had not mapped). Returns whether everything held;
/// with no IOMMU there is nothing to prove.
pub(crate) fn prove_iommu(com1: &mut Console, cpus: &[CpuIndex], tsc_hz: u64) -> bool {
    let driver = crate::cpu::without_interrupts(|| {
        DRIVER.lock().as_ref().map(|d| {
            (
                d.nvme.dma,
                d.dev.function.at,
                d.msix,
                d.page.bus,
                d.test_disk,
            )
        })
    });
    let on = crate::iommu::translating();
    let Some((dma, at, msix, irq_page, on_test_disk)) = driver else {
        let _ = writeln!(
            com1,
            "origin-kernel: IOMMU: no NVMe controller to prove DMA remapping with{}",
            if on { ": IOMMU failed" } else { "" }
        );
        return !on;
    };
    let DeviceDma::Translated(domain) = dma else {
        let _ = writeln!(
            com1,
            "origin-kernel: IOMMU: the NVMe controller's DMA is not translated{}",
            if on { ": IOMMU failed" } else { "" }
        );
        return !on;
    };
    // They read the boot test's pattern and use the queues only its disk gets (`init`).
    if !on_test_disk {
        let _ = writeln!(
            com1,
            "origin-kernel: IOMMU: the proofs through the NVMe controller skipped: this is not the boot test's disk"
        );
        return true;
    }
    let sid = crate::iommu::source_id(at);
    let (domains, _) = crate::iommu::domains();
    let (before, lost_before) = crate::iommu::take_faults();
    let attached_fault = before.iter().any(|f| {
        domains
            .iter()
            .any(|d| crate::iommu::source_id(d.at) == f.source_id)
    });
    let canary = [CANARY; PAGE as usize];

    // 1. Translated DMA: the controller writes through IOVA `page.bus` into physical `phys`.
    let Some(page) = DmaPage::new(dma) else {
        let _ = writeln!(
            com1,
            "origin-kernel: IOMMU: no memory for a probe page: IOMMU failed"
        );
        return false;
    };
    let phys = crate::phys::to_phys(page.virt);
    let walked = crate::iommu::translate(domain, page.bus);
    page.fill(&canary);
    let first = transfer_status(false, PROBE_LBA, page.bus, 8).ok();
    let mut expected = [0u8; 512];
    let mut landed = 0;
    // Only after a completion: a read that never completed may land at any time.
    for k in 0..first.map_or(0, |_| 8u64) {
        pattern(PROBE_LBA + k, &mut expected);
        let at = (k * 512) as usize;
        if page.bytes().get(at..at + 512) == Some(&expected[..]) {
            landed += 1;
        }
    }
    let translated = first.is_some_and(|s| s.ok())
        && landed == 8
        && page.bus != phys
        && walked == Some((phys, vtd::Access::READ_WRITE));
    let _ = writeln!(
        com1,
        "origin-kernel: IOMMU: translated DMA: the NVMe controller ({at}, source ID {sid:#06x}) read LBA {PROBE_LBA} into IOVA {:#x}, which its domain maps to physical {phys:#x} ({}): {landed} of 8 blocks landed in that physical page; consistent {translated}{}",
        page.bus,
        if page.bus == phys {
            "the same address"
        } else {
            "not the same address"
        },
        if translated { "" } else { ": IOMMU failed" }
    );

    // 2. Blocked DMA: unmapped, the same read must not reach the page, and must be recorded.
    page.fill(&canary);
    let unmapped = crate::iommu::unmap(domain, page.bus);
    let gone = crate::iommu::translate(domain, page.bus).is_none();
    let second = transfer_status(false, PROBE_LBA, page.bus, 8).ok();
    let intact = unmapped == Ok(phys) && page.bytes() == &canary[..];
    let (faults, lost) = crate::iommu::take_faults();
    let address_ok = |a: u64| a == page.bus || (a == 0 && !crate::iommu::fault_address_required());
    let mine = faults
        .iter()
        .find(|f| f.source_id == sid && !f.interrupt && address_ok(f.address))
        .copied();
    let recorded = mine.is_some_and(|f| {
        f.kind == crate::iommu::FaultKind::PageNotMapped && f.write != Some(false)
    });
    let blocked = unmapped == Ok(phys) && gone && intact && recorded;
    let status = match second {
        Some(s) => alloc::format!("completed with status type {} code {:#x}", s.kind, s.code),
        None => alloc::string::String::from("did not complete"),
    };
    let _ = writeln!(
        com1,
        "origin-kernel: IOMMU: blocked DMA: IOVA {:#x} unmapped ({}), the same read {status}, and the page kept its canary {intact}; fault recorded: {}{}; consistent {blocked}{}",
        page.bus,
        crate::iommu::unmap_words(),
        crate::iommu::Recorded(mine),
        if lost { " (a fault was lost)" } else { "" },
        if blocked { "" } else { ": IOMMU failed" }
    );

    // 3. Remapped interrupts: every programmed MSI-X entry is a handle to its own IRTE, whose
    //    destination is the target processor's APIC ID.
    let bsp = crate::cpu::this_cpu();
    let target = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let apic = |c: CpuIndex| c.apic_id().unwrap_or(framekernel::cpumap::BROADCAST);
    let mut routes: Vec<(pci::Address, vtd::MsiMessage, u8, u32)> = Vec::new();
    if let Some((table, capability)) = msix {
        routes.push((
            at,
            msix_message(table, 1),
            crate::cpu::DEVICE_VECTOR,
            apic(target),
        ));
        // Only the entries the table has: a table too short for one per processor
        // (`setup_cpu_queues` refused it) leaves `nvme_routes` short, and the proof fails.
        let per_cpu = usize::from(capability.entries).saturating_sub(2);
        for (k, &cpu) in cpus.iter().enumerate().take(per_cpu) {
            let entry = 2 + k as u64;
            routes.push((
                at,
                msix_message(table, entry),
                crate::cpu::DEVICE_VECTOR,
                apic(cpu),
            ));
        }
    }
    let nvme_routes = routes.len();
    routes.extend(crate::virtionet::msix_routes());
    let through = routes
        .iter()
        .filter(|&&(dev, m, vector, apic)| routed_through_irte(dev, m, vector, apic))
        .count();
    // An APIC ID above 255: compatibility format cannot carry it; an IRTE can.
    const WIDE: u32 = 300;
    let refused = pci::msi_address(WIDE).is_none();
    let wide = crate::iommu::msi_message(at, WIDE, crate::cpu::DEVICE_VECTOR);
    let wide_route = wide.ok().and_then(|m| crate::iommu::route(at, m));
    let wide_back = wide_route.map(|r| r.destination);
    let released = wide_route.is_some_and(|r| crate::iommu::release_interrupt(at, r.index).is_ok());
    let (_, irtes) = crate::iommu::domains();
    let remapped = crate::iommu::remapping()
        && nvme_routes == 1 + cpus.len()
        && routes.len() == nvme_routes + 2
        && through == routes.len()
        && refused
        && wide_back == Some(WIDE)
        && released;
    let _ = writeln!(
        com1,
        "origin-kernel: IOMMU: interrupt remapping: {through} of {} MSI-X entries (NVMe {nvme_routes}, virtio-net {}) {}; {irtes} IRTEs in use; APIC ID {WIDE} accepted (its IRTE reads back destination {}), which compatibility format refuses: {refused}; consistent {remapped}{}",
        routes.len(),
        routes.len() - nvme_routes,
        crate::iommu::routing_words(),
        wide_back.map_or(alloc::string::String::from("none"), |d| alloc::format!(
            "{d}"
        )),
        if remapped { "" } else { ": IOMMU failed" }
    );

    // 4. A blocked interrupt: vector 1 sent through a not-present IRTE.
    let blocked_irq = match (msix, crate::iommu::reserve_blocked_interrupt(at)) {
        (Some((table, _)), Ok((message, index))) => {
            let saved = msix_message(table, 1);
            msix_point(table, 1, message);
            let before = INTERRUPTS.load(Ordering::Relaxed);
            let completed = command(Q::Interrupt, |cid| {
                Command::transfer(false, cid, NSID, PROBE_LBA, 8, (irq_page, 0))
            })
            .ok();
            // Long enough for a message that was delivered to be taken.
            let start = crate::cpu::rdtsc();
            crate::sched::yield_until(
                || crate::cpu::rdtsc().wrapping_sub(start) > tsc_hz / 50,
                tsc_hz,
                1,
            );
            let arrived = INTERRUPTS.load(Ordering::Relaxed) - before;
            let (faults, lost) = crate::iommu::take_faults();
            let fault = faults
                .iter()
                .find(|f| {
                    f.source_id == sid
                        && f.interrupt
                        && f.interrupt_index.is_none_or(|i| i == index)
                })
                .copied();
            msix_point(table, 1, saved);
            let freed = crate::iommu::release_interrupt(at, index).is_ok();
            let recorded = match fault {
                Some(f) => f.kind == crate::iommu::FaultKind::InterruptBlocked,
                None => !crate::iommu::interrupt_record_required(),
            };
            let ok = completed.is_some_and(|c| c.status.ok()) && arrived == 0 && recorded && freed;
            let _ = writeln!(
                com1,
                "origin-kernel: IOMMU: blocked interrupt: NVMe MSI-X vector 1 pointed at IRTE {index}, left not present: a read on I/O queue 2 completed {}, {arrived} interrupts arrived; fault recorded: {}{}; vector 1 restored; consistent {ok}{}",
                completed.is_some_and(|c| c.status.ok()),
                crate::iommu::Recorded(fault),
                if lost { " (a fault was lost)" } else { "" },
                if ok { "" } else { ": IOMMU failed" }
            );
            ok
        }
        (_, Err(why)) => {
            let _ = writeln!(
                com1,
                "origin-kernel: IOMMU: blocked interrupt: {why}: IOMMU failed"
            );
            false
        }
        (None, Ok(_)) => {
            let _ = writeln!(
                com1,
                "origin-kernel: IOMMU: blocked interrupt: NVMe MSI-X is not set up: IOMMU failed"
            );
            false
        }
    };

    // The domains, and what was recorded before the proofs.
    let (domains, _) = crate::iommu::domains();
    let _ = write!(com1, "origin-kernel: IOMMU: domains:");
    for d in &domains {
        let _ = write!(
            com1,
            " {} domain {} ({} pages mapped, {} ever, {} table pages);",
            d.at, d.id, d.mapped, d.mapped_ever, d.tables
        );
    }
    let quiet = !attached_fault && !lost_before;
    let _ = writeln!(
        com1,
        " {}; IOVAs from {:#x}; faults recorded before the proofs: {}{}; consistent {quiet}{}",
        crate::iommu::default_deny_words(),
        vtd::IOVA_BASE,
        before.len(),
        if lost_before { " (some lost)" } else { "" },
        if quiet { "" } else { ": IOMMU failed" }
    );
    translated && blocked && remapped && blocked_irq && quiet
}
