//! The NVMe driver (M5; CLAUDE.md §33: controller discovery, initialization, admin and I/O
//! queues, namespace discovery, DMA, flushes) and the boot proof of its block I/O.
//!
//! A first-party kernel-mode driver (ADR-0001 D2), polled for now: no interrupts until
//! MSI-X routing exists. It binds to the first NVMe controller (class 01:08:02) that
//! `pcie` listed:
//! 1. memory decoding and bus mastering on; BAR 0 mapped uncached;
//! 2. the controller reset: CC.EN cleared and CSTS.RDY awaited within CAP.TO;
//! 3. admin queues of 32 entries; CC.EN set (4 KiB pages, 64/16-byte entries); RDY awaited,
//!    with the fatal-status bit checked;
//! 4. Identify Controller and Identify Namespace 1;
//! 5. one I/O queue pair of 64 entries.
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

use framekernel::nvme::{self, Capabilities, Command, Completion, NamespaceInfo, Queues, register};
use framekernel::pci::BarKind;

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

/// A bound controller.
struct Nvme {
    regs: u64,
    cap: Capabilities,
    admin: QueuePair,
    io: Option<QueuePair>,
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

    /// Submits `make(cid)` to the admin queue (or the I/O queue) and polls for its
    /// completion.
    fn run(&mut self, io: bool, make: impl FnOnce(u16) -> Command) -> Result<Completion, Failure> {
        let stride = self.cap.doorbell_stride;
        let regs = self.regs;
        let qp = if io {
            self.io.as_mut().ok_or(Failure::Full)?
        } else {
            &mut self.admin
        };
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
        let deadline = crate::time::monotonic_nanos() + COMMAND_TIMEOUT_NS;
        loop {
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
            if qp.state.complete(&c) {
                doorbell(nvme::doorbell(qp.qid, true, stride), qp.state.cq_head());
                return if c.cid == cid {
                    Ok(c)
                } else {
                    Err(Failure::WrongCommand)
                };
            }
            if crate::time::monotonic_nanos() > deadline {
                return Err(Failure::Timeout);
            }
            core::hint::spin_loop();
        }
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

/// Binds the first NVMe controller and proves block I/O on namespace 1. Returns whether
/// every step and check passed (no controller at all is reported, and passes).
pub(crate) fn init(com1: &mut Console) -> bool {
    let Some(dev) = crate::pcie::find(0x01, 0x08, 0x02) else {
        let _ = writeln!(com1, "origin-kernel: NVMe: no controller");
        return true;
    };
    match bind(com1, &dev) {
        Ok(ok) => ok,
        Err(why) => {
            let _ = writeln!(com1, "origin-kernel: NVMe: {why}: NVMe failed");
            false
        }
    }
}

fn bind(com1: &mut Console, dev: &crate::pcie::Device) -> Result<bool, &'static str> {
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
        .run(false, |cid| Command::identify(cid, 1, 0, data.phys))
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
        .run(false, |cid| Command::identify(cid, 0, NSID, data.phys))
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
    let io = new_queue(1, IO_ENTRIES).ok_or("no memory for the I/O queues")?;
    let (cq_phys, sq_phys) = (io.cq.phys, io.sq.phys);
    let created = c
        .run(false, |cid| {
            Command::create_io_cq(cid, 1, IO_ENTRIES, cq_phys)
        })
        .map_err(failed)?;
    if !created.status.ok() {
        return Err("Create I/O Completion Queue failed");
    }
    let created = c
        .run(false, |cid| {
            Command::create_io_sq(cid, 1, IO_ENTRIES, 1, sq_phys)
        })
        .map_err(failed)?;
    if !created.status.ok() {
        return Err("Create I/O Submission Queue failed");
    }
    c.io = Some(io);
    let ok = prove_io(com1, &mut c, &data, namespace).map_err(failed)?;
    Ok(ok)
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
        c.run(true, |cid| {
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
    let wrote = c.run(true, |cid| {
        Command::transfer(true, cid, NSID, scratch, 1, prps)
    })?;
    let flushed = c.run(true, |cid| Command::flush(cid, NSID))?;
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
