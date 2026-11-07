//! The NVMe driver (M5; CLAUDE.md §33: controller discovery, initialization, admin and I/O
//! queues, namespace discovery, DMA, flushes), the boot proof of its block I/O, and the
//! disk's GUID Partition Table, both copies checked (CLAUDE.md §39).
//!
//! A first-party kernel-mode driver (ADR-0001 D2). It binds to the first NVMe controller
//! (class 01:08:02) that `pcie` listed:
//! 1. memory decoding and bus mastering on; BAR 0 mapped uncached;
//! 2. the controller reset: CC.EN cleared and CSTS.RDY awaited within CAP.TO;
//! 3. admin queues of 32 entries; CC.EN set (4 KiB pages, 64/16-byte entries); RDY awaited,
//!    with the fatal-status bit checked;
//! 4. Identify Controller and Identify Namespace 1;
//! 5. I/O queue pair 1 (64 entries), polled;
//! 6. after the polled proofs, MSI-X: vector 1 is sent to one processor as
//!    `cpu::DEVICE_VECTOR` (every other entry masked), and I/O queue pair 2 completes to
//!    it. A thread there submits under the driver lock and then blocks on a wait queue
//!    until the interrupt wakes it; its wait condition is the completion queue itself.
//!
//! DMA memory is zeroed heap pages: the direct map makes them physically contiguous and
//! gives their physical addresses (`phys::to_phys`), and x86 DMA is cache-coherent. The
//! driver never keeps a reference into memory the device may write: queue entries are
//! written and read with volatile accesses, and a data buffer is read only after its command
//! completed. A fence orders memory writes before every doorbell. There is no IOMMU yet,
//! so the controller can reach all of memory (CLAUDE.md §32: a later step).

use alloc::alloc::{Layout, alloc_zeroed};
use core::fmt::Write;
use core::ptr;
use core::sync::atomic::{Ordering, compiler_fence, fence};

use core::sync::atomic::AtomicU64;
use framekernel::gpt;
use framekernel::nvme::{self, Capabilities, Command, Completion, NamespaceInfo, Queues, register};
use framekernel::pci::{self, BarKind};
use origin_sync::SpinLock;

use crate::sched::MAX_CPUS;
use crate::wait::WaitQueue;

use crate::console::Console;

/// Entries in the admin queues and the I/O queues.
const ADMIN_ENTRIES: u16 = 32;
const IO_ENTRIES: u16 = 64;
/// The memory page size the driver configures.
const PAGE: u64 = 4096;
/// How long one command may take.
const COMMAND_TIMEOUT_NS: u64 = 2_000_000_000;
/// The namespace the proof uses.
const NSID: u32 = 1;
/// Command register bits: memory space decoding, bus mastering.
const MEMORY_AND_BUS_MASTER: u16 = (1 << 1) | (1 << 2);

/// A zeroed 4 KiB DMA page, kept for ever (the controller may use it at any time).
#[derive(Clone, Copy)]
struct DmaPage {
    virt: u64,
    phys: u64,
}

impl DmaPage {
    fn new() -> Option<Self> {
        let layout = Layout::from_size_align(PAGE as usize, PAGE as usize).ok()?;
        // SAFETY: `layout` has a non-zero size. The page is never freed: the controller
        // may write it until it is reset.
        let p = unsafe { alloc_zeroed(layout) };
        if p.is_null() {
            return None;
        }
        let virt = p.expose_provenance() as u64;
        Some(Self {
            virt,
            phys: crate::phys::to_phys(virt),
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
            // SAFETY: inside this live page; the device is not using it (no command is
            // outstanding on it).
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
}

/// Why a command failed.
#[derive(Clone, Copy, Debug)]
enum Failure {
    Full,
    Timeout,
    WrongCommand,
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

    /// Submits `make(cid)` to queue `q` and polls for its completion.
    fn run(&mut self, q: Q, make: impl FnOnce(u16) -> Command) -> Result<Completion, Failure> {
        let cid = self.submit(q, make)?;
        let deadline = crate::time::monotonic_nanos() + COMMAND_TIMEOUT_NS;
        loop {
            if let Some(done) = self.poll(q, cid) {
                return done;
            }
            if crate::time::monotonic_nanos() > deadline {
                return Err(Failure::Timeout);
            }
            core::hint::spin_loop();
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
        let cid = qp.next_cid;
        qp.next_cid = qp.next_cid.wrapping_add(1);
        let slot = qp.state.submit().ok_or(Failure::Full)?;
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
            Ok(c)
        } else {
            Err(Failure::WrongCommand)
        })
    }
}

fn new_queue(qid: u16, entries: u16) -> Option<QueuePair> {
    Some(QueuePair {
        qid,
        sq: DmaPage::new()?,
        cq: DmaPage::new()?,
        state: Queues::new(entries),
        next_cid: 0,
    })
}

/// The bytes xtask writes in pattern block `lba`: `ORIGIN NVME LBA 00002048 ` repeated.
fn pattern(lba: u64, out: &mut [u8]) {
    let mut unit = *b"ORIGIN NVME LBA 00000000 ";
    let mut v = lba;
    for i in (16..24).rev() {
        if let Some(d) = unit.get_mut(i) {
            *d = b'0' + (v % 10) as u8;
        }
        v /= 10;
    }
    for (o, u) in out.iter_mut().zip(unit.iter().cycle()) {
        *o = *u;
    }
}

/// The bound controller, once the polled proofs are done, with a data page for the
/// interrupt-driven proof's thread. Locked with interrupts disabled, never across a wait.
struct Driver {
    nvme: Nvme,
    dev: crate::pcie::Device,
    page: DmaPage,
    /// The MSI-X table's virtual address and the capability, once set up.
    msix: Option<(u64, pci::Msix)>,
}

static DRIVER: SpinLock<Option<Driver>> = SpinLock::new(None);
/// Per processor: woken by the completion interrupts routed there.
static IO_WAIT: [WaitQueue; MAX_CPUS] = [const { WaitQueue::new() }; MAX_CPUS];
/// Per processor: completion interrupts taken there.
static CPU_INTERRUPTS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static INTERRUPTS: AtomicU64 = AtomicU64::new(0);
/// The processor the last completion interrupt arrived on, plus one (0: none yet).
static INTERRUPT_CPU: AtomicU64 = AtomicU64::new(0);

/// The completion interrupt (`cpu::DEVICE_VECTOR`): counts it and wakes the waiter, which
/// then reads the completion itself.
pub(crate) fn on_interrupt() {
    let me = crate::cpu::cpu_id();
    INTERRUPTS.fetch_add(1, Ordering::Relaxed);
    INTERRUPT_CPU.store(me + 1, Ordering::Relaxed);
    if let Some(n) = CPU_INTERRUPTS.get(me as usize) {
        n.fetch_add(1, Ordering::Relaxed);
    }
    if let Some(w) = IO_WAIT.get(me as usize) {
        w.wake_one();
    }
}

/// Binds the first NVMe controller and proves block I/O on namespace 1, polled and then
/// interrupt-driven. Returns whether every step and check passed (no controller at all is
/// reported, and passes).
pub(crate) fn init(com1: &mut Console, cpus: &[u32], tsc_hz: u64) -> bool {
    let Some(dev) = crate::pcie::find(0x01, 0x08, 0x02) else {
        let _ = writeln!(com1, "origin-kernel: NVMe: no controller");
        return true;
    };
    let polled = match bind(com1, &dev, cpus.len()) {
        Ok((nvme, ok)) => {
            let Some(page) = DmaPage::new() else {
                return false;
            };
            crate::cpu::without_interrupts(|| {
                *DRIVER.lock() = Some(Driver {
                    nvme,
                    dev,
                    page,
                    msix: None,
                });
            });
            ok
        }
        Err(why) => {
            let _ = writeln!(com1, "origin-kernel: NVMe: {why}: NVMe failed");
            return false;
        }
    };
    let interrupts = polled && prove_interrupts(com1, cpus, tsc_hz);
    interrupts && prove_per_cpu(com1, cpus, tsc_hz)
}

fn bind(
    com1: &mut Console,
    dev: &crate::pcie::Device,
    cpus: usize,
) -> Result<(Nvme, bool), &'static str> {
    let f = dev.function;
    let bar = dev.bars[0].ok_or("no BAR 0")?;
    if bar.kind == BarKind::Io || bar.size < 0x2000 {
        return Err("BAR 0 is not a register window");
    }
    let command =
        crate::pcie::enable(f.at, MEMORY_AND_BUS_MASTER).ok_or("no configuration access")?;
    if command & MEMORY_AND_BUS_MASTER != MEMORY_AND_BUS_MASTER {
        return Err("memory decoding and bus mastering did not turn on");
    }
    let regs = crate::vm::map_mmio(bar.address, bar.size).map_err(|_| "cannot map BAR 0")?;
    let mut c = Nvme {
        regs,
        cap: Capabilities::decode(0),
        admin: new_queue(0, ADMIN_ENTRIES).ok_or("no memory for the admin queues")?,
        io: None,
        irq: None,
    };
    c.cap = Capabilities::decode(c.read64(register::CAP));
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
    // Reset, then the admin queues, then enable.
    let cc = c.read32(register::CC);
    if cc & 1 != 0 {
        c.write32(register::CC, cc & !1);
    }
    c.wait_ready(false)?;
    c.write32(register::AQA, nvme::aqa(ADMIN_ENTRIES));
    c.write64(register::ASQ, c.admin.sq.phys);
    c.write64(register::ACQ, c.admin.cq.phys);
    c.write32(register::CC, nvme::cc_enable(12));
    c.wait_ready(true)?;
    // The driver polls: mask the controller's pin interrupt.
    c.write32(register::INTMS, u32::MAX);

    let data = DmaPage::new().ok_or("no memory for a data page")?;
    let failed = |e: Failure| -> &'static str {
        match e {
            Failure::Full => "queue full",
            Failure::Timeout => "a command timed out",
            Failure::WrongCommand => "a completion for another command",
        }
    };
    let id = c
        .run(Q::Admin, |cid| Command::identify(cid, 1, 0, data.phys))
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
        .run(Q::Admin, |cid| Command::identify(cid, 0, NSID, data.phys))
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
    let _ = writeln!(
        com1,
        "origin-kernel: NVMe: namespace {NSID}: {} blocks of {} B ({} MiB)",
        namespace.blocks,
        namespace.block_size,
        (namespace.blocks * u64::from(namespace.block_size)) >> 20
    );
    // Before any I/O queue exists: queue pairs 1 (polled), 2 (one interrupt) and one per
    // processor.
    let wanted = u16::try_from(2 + cpus).map_err(|_| "too many processors")?;
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
    let io = new_queue(1, IO_ENTRIES).ok_or("no memory for the I/O queues")?;
    let (cq_phys, sq_phys) = (io.cq.phys, io.sq.phys);
    let created = c
        .run(Q::Admin, |cid| {
            Command::create_io_cq(cid, 1, IO_ENTRIES, cq_phys)
        })
        .map_err(failed)?;
    if !created.status.ok() {
        return Err("Create I/O Completion Queue failed");
    }
    let created = c
        .run(Q::Admin, |cid| {
            Command::create_io_sq(cid, 1, IO_ENTRIES, 1, sq_phys)
        })
        .map_err(failed)?;
    if !created.status.ok() {
        return Err("Create I/O Submission Queue failed");
    }
    c.io = Some(io);
    let ok = prove_io(com1, &mut c, &data, namespace).map_err(failed)?;
    let gpt = prove_gpt(com1, &mut c, &data, namespace).map_err(failed)?;
    Ok((c, ok && gpt))
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
    if u64::from(msix.table_offset) + table_len > bar.size {
        return Err("the MSI-X table lies outside its BAR");
    }
    let table = if msix.table_bar == 0 {
        d.nvme.regs + u64::from(msix.table_offset)
    } else {
        crate::vm::map_mmio(bar.address + u64::from(msix.table_offset), table_len)
            .map_err(|_| "cannot map the MSI-X table")?
    };
    let address = pci::msi_address(target).ok_or("the target needs interrupt remapping")?;
    let data = pci::msi_data(crate::cpu::DEVICE_VECTOR).ok_or("bad vector")?;
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
    entry(1, 0, address as u32);
    entry(1, 1, (address >> 32) as u32);
    entry(1, 2, data);
    entry(1, 3, 0);
    let control =
        crate::pcie::enable_msix(d.dev.function.at, &msix).ok_or("no configuration access")?;
    if control & pci::MSIX_ENABLE == 0 || control & pci::MSIX_FUNCTION_MASK != 0 {
        return Err("MSI-X did not turn on");
    }
    let queue = new_queue(2, IO_ENTRIES).ok_or("no memory for I/O queue 2")?;
    let (cq, sq) = (queue.cq.phys, queue.sq.phys);
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
            let prps = (d.page.phys, 0);
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
        let Some(wait) = IO_WAIT.get(crate::cpu::cpu_id() as usize) else {
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
        let Some(Ok(c)) = done.get() else {
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
fn prove_interrupts(com1: &mut Console, cpus: &[u32], tsc_hz: u64) -> bool {
    let bsp = crate::cpu::cpu_id();
    let target = cpus
        .iter()
        .copied()
        .find(|&id| u64::from(id) != bsp)
        .unwrap_or(bsp as u32);
    let setup = crate::cpu::without_interrupts(|| {
        let mut g = DRIVER.lock();
        let d = g.as_mut().ok_or("no driver")?;
        setup_interrupts(d, target)
    });
    let msix = match setup {
        Ok(m) => m,
        Err(why) => {
            let _ = writeln!(com1, "origin-kernel: NVMe: MSI-X: {why}: NVMe failed");
            return false;
        }
    };
    let _ = writeln!(
        com1,
        "origin-kernel: NVMe: MSI-X: {} vectors, table in BAR{} at offset {:#x}; I/O queue 2 completes to vector 1, sent to CPU {target} as IDT vector {:#x}",
        msix.entries,
        msix.table_bar,
        msix.table_offset,
        crate::cpu::DEVICE_VECTOR
    );
    if crate::sched::spawn(target as usize, io_thread, 0).is_err() {
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
        && on == u64::from(target);
    let micros = cycles / reads.max(1) / (tsc_hz / 1_000_000).max(1);
    let _ = writeln!(
        com1,
        "origin-kernel: NVMe: interrupt-driven: {reads} reads of 8 blocks by a thread on CPU {target}, each blocking until its completion interrupt: {interrupts} interrupts ({}), {matched} of {} blocks match, {micros} us per read{}",
        if interrupts == 0 {
            alloc::string::String::from("none arrived")
        } else {
            alloc::format!("the last on CPU {on}")
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
        let prps = nvme::prps(data.phys, blocks * bs, PAGE).unwrap_or((data.phys, 0));
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
/// be valid and agree; with only one, the disk is reported from it, and the proof fails.
fn prove_gpt(
    com1: &mut Console,
    c: &mut Nvme,
    data: &DmaPage,
    ns: NamespaceInfo,
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
    let _ = writeln!(
        com1,
        "origin-kernel: GPT: primary copy {}, backup copy at LBA {backup_lba} {}, the two agree {agree}{}",
        State(&primary),
        State(&backup),
        if agree { "" } else { ": GPT failed" }
    );
    let (list, (h, n)) = match (&primary, &backup) {
        (Ok(p), _) => (&parts, p),
        (Err(_), Ok(b)) => (&backup_parts, b),
        _ => return Ok(false),
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
    Ok(agree)
}

/// Reads blocks the boot test wrote, writes and reads back one, and checks that a read past
/// the end fails.
fn prove_io(
    com1: &mut Console,
    c: &mut Nvme,
    data: &DmaPage,
    ns: NamespaceInfo,
) -> Result<bool, Failure> {
    let bs = u64::from(ns.block_size);
    let read = |c: &mut Nvme, lba: u64, blocks: u32| {
        let prps = nvme::prps(data.phys, u64::from(blocks) * bs, PAGE).unwrap_or((data.phys, 0));
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
    let prps = (data.phys, 0);
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

/// For each online processor `k` (by position in `cpus`): MSI-X entry `2 + k` sent to it,
/// and I/O queue pair `3 + k` completing to that entry.
fn setup_cpu_queues(d: &mut Driver, cpus: &[u32]) -> Result<(), &'static str> {
    let (table, msix) = d.msix.ok_or("MSI-X is not set up")?;
    if usize::from(msix.entries) < 2 + cpus.len() {
        return Err("too few MSI-X vectors for one per processor");
    }
    let data = pci::msi_data(crate::cpu::DEVICE_VECTOR).ok_or("bad vector")?;
    for (k, &cpu) in cpus.iter().enumerate() {
        let vector = 2 + k as u16;
        let qid = 3 + k as u16;
        let address = pci::msi_address(cpu).ok_or("a processor needs interrupt remapping")?;
        let entry = |word: u64, value: u32| {
            let at = table + u64::from(vector) * pci::MSIX_ENTRY + 4 * word;
            let p = ptr::with_exposed_provenance_mut::<u32>(at as usize);
            // SAFETY: inside the MSI-X table (`setup_interrupts` checked its bounds against
            // its BAR, and `vector` is below its entry count), mapped uncached.
            unsafe { p.write_volatile(value) };
        };
        entry(0, address as u32);
        entry(1, (address >> 32) as u32);
        entry(2, data);
        entry(3, 0);
        let queue = new_queue(qid, IO_ENTRIES).ok_or("no memory for a per-processor queue")?;
        let page = DmaPage::new().ok_or("no memory for a per-processor page")?;
        let (cq, sq) = (queue.cq.phys, queue.sq.phys);
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
        let slot = CPU_QUEUES
            .get(cpu as usize)
            .ok_or("processor ID out of range")?;
        *slot.lock() = Some(CpuQueue {
            qp: queue,
            page,
            regs: d.nvme.regs,
            stride: d.nvme.cap.doorbell_stride,
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
        let phys = q.page.phys;
        submit_on(&mut q.qp, q.regs, q.stride, |cid| make(cid, phys)).ok()
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
    let me = crate::cpu::cpu_id() as usize;
    let (mut reads, mut matched) = (0u64, 0u64);
    let mut page = [0u8; 4096];
    let mut expected = [0u8; 512];
    for i in 0..CPU_READS {
        let lba = 2048 + ((i + index) % 8) * 8;
        if !cpu_command(
            me,
            |cid, phys| Command::transfer(false, cid, NSID, lba, 8, (phys, 0)),
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
    // A block of its own: written from this processor's page, flushed, cleared, read back.
    let scratch = 20_000 + 8 * index;
    pattern(scratch ^ 0x5A5A, &mut expected);
    let filled = crate::cpu::without_interrupts(|| {
        let g = CPU_QUEUES.get(me).map(|s| s.lock());
        g.as_ref()
            .and_then(|g| g.as_ref())
            .map(|q| q.page.fill(&expected))
            .is_some()
    });
    let wrote = filled
        && cpu_command(
            me,
            |cid, phys| Command::transfer(true, cid, NSID, scratch, 1, (phys, 0)),
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
            |cid, phys| Command::transfer(false, cid, NSID, scratch, 1, (phys, 0)),
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
fn prove_per_cpu(com1: &mut Console, cpus: &[u32], tsc_hz: u64) -> bool {
    let setup = crate::cpu::without_interrupts(|| {
        let mut g = DRIVER.lock();
        let d = g.as_mut().ok_or("no driver")?;
        setup_cpu_queues(d, cpus)
    });
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
        if crate::sched::spawn(cpu as usize, cpu_io_thread, k as u64).is_err() {
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
                .get(c as usize)
                .is_some_and(|r| r.iter().all(|x| x.load(Ordering::Acquire) != 0))
        })
    };
    crate::sched::yield_until(finished, tsc_hz, 10);
    let micros = crate::cpu::rdtsc().wrapping_sub(start) / (tsc_hz / 1_000_000).max(1);
    let mut ok = finished();
    let (mut reads, mut matched) = (0, 0);
    for &cpu in cpus {
        let c = cpu as usize;
        let get = |i: usize| {
            CPU_RESULT
                .get(c)
                .map_or(0, |r| r[i].load(Ordering::Acquire).saturating_sub(1))
        };
        let interrupts = CPU_INTERRUPTS[c].load(Ordering::Relaxed) - before[c];
        let (r, m, back) = (get(0), get(1), get(2) == 1);
        let mine = r == CPU_READS && m == 8 * CPU_READS && back && interrupts >= CPU_READS + 3;
        ok &= mine;
        reads += r;
        matched += m;
        let _ = writeln!(
            com1,
            "origin-kernel: NVMe: CPU {cpu}: {r} reads through its own queue pair, {m} of {} blocks match, its write read back {back}, {interrupts} interrupts on CPU {cpu}{}",
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
