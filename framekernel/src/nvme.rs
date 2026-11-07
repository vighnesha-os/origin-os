//! NVM Express over PCIe (CLAUDE.md §33): controller registers, queue entries, queue
//! indexes and Identify data, as plain data tested on the host. The kernel's driver
//! (`src/storage.rs`) does the MMIO and DMA.
//!
//! References:
//! - NVM Express Base Specification 2.x: §3.1.4 (controller registers CAP, VS, CC, CSTS,
//!   AQA, ASQ, ACQ), §4.2 (submission queue entry), §4.6 (completion queue entry, the phase
//!   tag), §5.2 (Create I/O Completion Queue 05h, Create I/O Submission Queue 01h, Identify
//!   06h with CNS 00h namespace and 01h controller);
//! - NVM Command Set Specification 1.x: Flush 00h, Write 01h, Read 02h, and the Identify
//!   Namespace fields NSZE, NLBAF, FLBAS and LBAF;
//! - NVMe over PCIe Transport Specification 1.x: doorbell layout, PRP entries.
//!
//! Everything a controller returns is untrusted: Identify data is parsed with bounds
//! checks, and a completion is accepted only with the expected phase and command ID.

/// Controller register offsets in BAR 0.
pub mod register {
    pub const CAP: u64 = 0x00;
    pub const VS: u64 = 0x08;
    pub const INTMS: u64 = 0x0C;
    pub const CC: u64 = 0x14;
    pub const CSTS: u64 = 0x1C;
    pub const AQA: u64 = 0x24;
    pub const ASQ: u64 = 0x28;
    pub const ACQ: u64 = 0x30;
}

/// CSTS.RDY and CSTS.CFS (controller fatal status).
pub const CSTS_READY: u32 = 1 << 0;
pub const CSTS_FATAL: u32 = 1 << 1;
/// Bytes of a submission and of a completion queue entry (CC.IOSQES = 6, CC.IOCQES = 4).
pub const SQ_ENTRY: usize = 64;
pub const CQ_ENTRY: usize = 16;

/// Admin and NVM command opcodes.
pub mod opcode {
    pub const CREATE_IO_SQ: u8 = 0x01;
    pub const CREATE_IO_CQ: u8 = 0x05;
    pub const IDENTIFY: u8 = 0x06;
    pub const FLUSH: u8 = 0x00;
    pub const WRITE: u8 = 0x01;
    pub const READ: u8 = 0x02;
}

/// What CAP says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// Entries a queue may have (MQES + 1).
    pub max_entries: u32,
    /// Queues must be physically contiguous (CQR).
    pub contiguous: bool,
    /// Worst-case time for CSTS.RDY to change after CC.EN does (TO, in 500 ms units).
    pub timeout_ms: u64,
    /// Bytes between doorbells (4 << DSTRD).
    pub doorbell_stride: u64,
    /// The NVM command set is supported (CSS bit 0).
    pub nvm: bool,
    /// Smallest and largest memory page sizes, as shifts (12 + MPSMIN, 12 + MPSMAX).
    pub min_page_shift: u32,
    pub max_page_shift: u32,
}

impl Capabilities {
    pub const fn decode(cap: u64) -> Self {
        Self {
            max_entries: (cap & 0xFFFF) as u32 + 1,
            contiguous: cap & (1 << 16) != 0,
            timeout_ms: ((cap >> 24) & 0xFF) * 500,
            doorbell_stride: 4 << ((cap >> 32) & 0xF),
            nvm: (cap >> 37) & 1 != 0,
            min_page_shift: 12 + ((cap >> 48) & 0xF) as u32,
            max_page_shift: 12 + ((cap >> 52) & 0xF) as u32,
        }
    }
}

/// The version register as `(major, minor, tertiary)`.
pub const fn version(vs: u32) -> (u16, u8, u8) {
    ((vs >> 16) as u16, (vs >> 8) as u8, vs as u8)
}

/// The offset of queue `queue`'s submission tail (or, with `completion`, completion head)
/// doorbell.
pub const fn doorbell(queue: u16, completion: bool, stride: u64) -> u64 {
    0x1000 + (2 * queue as u64 + completion as u64) * stride
}

/// CC to enable the controller: NVM command set, round-robin arbitration, 4 KiB pages
/// (`page_shift` 12), 64-byte submission and 16-byte completion entries.
pub const fn cc_enable(page_shift: u32) -> u32 {
    1 | ((page_shift - 12) << 7) | (6 << 16) | (4 << 20)
}

/// AQA for admin queues of `entries` entries each.
pub const fn aqa(entries: u16) -> u32 {
    let n = (entries - 1) as u32;
    (n << 16) | n
}

/// One submission queue entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Command {
    pub opcode: u8,
    pub cid: u16,
    pub nsid: u32,
    pub prp1: u64,
    pub prp2: u64,
    pub cdw10: u32,
    pub cdw11: u32,
    pub cdw12: u32,
}

impl Command {
    /// The entry's 16 dwords (PRPs, not SGLs: PSDT 0; no fused operation).
    pub const fn to_words(&self) -> [u32; 16] {
        let mut w = [0u32; 16];
        w[0] = self.opcode as u32 | ((self.cid as u32) << 16);
        w[1] = self.nsid;
        w[6] = self.prp1 as u32;
        w[7] = (self.prp1 >> 32) as u32;
        w[8] = self.prp2 as u32;
        w[9] = (self.prp2 >> 32) as u32;
        w[10] = self.cdw10;
        w[11] = self.cdw11;
        w[12] = self.cdw12;
        w
    }

    /// Identify: CNS 01h (controller, `nsid` 0) or 00h (namespace `nsid`), 4 KiB into
    /// `prp1`.
    pub const fn identify(cid: u16, cns: u8, nsid: u32, prp1: u64) -> Self {
        Self {
            opcode: opcode::IDENTIFY,
            cid,
            nsid,
            prp1,
            prp2: 0,
            cdw10: cns as u32,
            cdw11: 0,
            cdw12: 0,
        }
    }

    /// Create I/O Completion Queue `qid` of `entries` at `prp1`: physically contiguous,
    /// interrupts off (the driver polls).
    pub const fn create_io_cq(cid: u16, qid: u16, entries: u16, prp1: u64) -> Self {
        Self {
            opcode: opcode::CREATE_IO_CQ,
            cid,
            nsid: 0,
            prp1,
            prp2: 0,
            cdw10: ((entries as u32 - 1) << 16) | qid as u32,
            cdw11: 1,
            cdw12: 0,
        }
    }

    /// As [`Command::create_io_cq`], with interrupts on: completions raise interrupt
    /// `vector` (the MSI-X table entry).
    pub const fn create_io_cq_with_interrupts(
        cid: u16,
        qid: u16,
        entries: u16,
        vector: u16,
        prp1: u64,
    ) -> Self {
        let mut c = Self::create_io_cq(cid, qid, entries, prp1);
        c.cdw11 = ((vector as u32) << 16) | 0b11;
        c
    }

    /// Create I/O Submission Queue `qid` of `entries` at `prp1`, completing to queue `cqid`,
    /// physically contiguous.
    pub const fn create_io_sq(cid: u16, qid: u16, entries: u16, cqid: u16, prp1: u64) -> Self {
        Self {
            opcode: opcode::CREATE_IO_SQ,
            cid,
            nsid: 0,
            prp1,
            prp2: 0,
            cdw10: ((entries as u32 - 1) << 16) | qid as u32,
            cdw11: ((cqid as u32) << 16) | 1,
            cdw12: 0,
        }
    }

    /// Read or Write `blocks` (1–65536) logical blocks at `lba` of namespace `nsid`.
    pub const fn transfer(
        write: bool,
        cid: u16,
        nsid: u32,
        lba: u64,
        blocks: u32,
        prps: (u64, u64),
    ) -> Self {
        Self {
            opcode: if write { opcode::WRITE } else { opcode::READ },
            cid,
            nsid,
            prp1: prps.0,
            prp2: prps.1,
            cdw10: lba as u32,
            cdw11: (lba >> 32) as u32,
            cdw12: (blocks - 1) & 0xFFFF,
        }
    }

    /// Flush namespace `nsid`'s volatile write cache to non-volatile media.
    pub const fn flush(cid: u16, nsid: u32) -> Self {
        Self {
            opcode: opcode::FLUSH,
            cid,
            nsid,
            prp1: 0,
            prp2: 0,
            cdw10: 0,
            cdw11: 0,
            cdw12: 0,
        }
    }
}

/// The PRP entries for a transfer of `len` bytes at physical `phys` with `page`-byte
/// memory pages: PRP2 is the second page if the transfer spans two pages, 0 if one. Longer
/// transfers need a PRP list, which this driver does not build: `None`.
pub fn prps(phys: u64, len: u64, page: u64) -> Option<(u64, u64)> {
    if len == 0 || !phys.is_multiple_of(4) || !page.is_power_of_two() {
        return None;
    }
    let first_end = (phys | (page - 1)).checked_add(1)?;
    let end = phys.checked_add(len)?;
    if end <= first_end {
        Some((phys, 0))
    } else if end - first_end <= page {
        Some((phys, first_end))
    } else {
        None
    }
}

/// A completion's status field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    /// Status code (SC) and status code type (SCT).
    pub code: u8,
    pub kind: u8,
    /// Do not retry (DNR).
    pub final_: bool,
}

impl Status {
    pub const fn ok(&self) -> bool {
        self.code == 0 && self.kind == 0
    }
}

/// Generic command status (SCT 0) 80h: LBA out of range.
pub const LBA_OUT_OF_RANGE: Status = Status {
    code: 0x80,
    kind: 0,
    final_: true,
};

/// One completion queue entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Completion {
    /// Command-specific dword 0.
    pub result: u32,
    /// How far the controller has consumed the submission queue.
    pub sq_head: u16,
    pub sq_id: u16,
    pub cid: u16,
    pub phase: bool,
    pub status: Status,
}

impl Completion {
    pub const fn decode(w: [u32; 4]) -> Self {
        let sf = w[3] >> 17;
        Self {
            result: w[0],
            sq_head: w[2] as u16,
            sq_id: (w[2] >> 16) as u16,
            cid: w[3] as u16,
            phase: (w[3] >> 16) & 1 != 0,
            status: Status {
                code: sf as u8,
                kind: ((sf >> 8) & 0x7) as u8,
                final_: (sf >> 14) & 1 != 0,
            },
        }
    }
}

/// The host's indexes for one submission/completion queue pair: where the next command
/// goes, where the next completion appears, and the phase that marks it new.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Queues {
    entries: u16,
    sq_tail: u16,
    /// The submission head the controller last reported.
    sq_head: u16,
    cq_head: u16,
    phase: bool,
}

impl Queues {
    /// Queues of `entries` entries (at least 2), freshly created: the controller writes
    /// phase 1 on its first pass through the zeroed completion queue.
    pub const fn new(entries: u16) -> Self {
        Self {
            entries,
            sq_tail: 0,
            sq_head: 0,
            cq_head: 0,
            phase: true,
        }
    }

    /// The submission slot for the next command, or `None` if the queue is full (one slot
    /// stays empty, so a full queue is told apart from an empty one).
    pub fn submit(&mut self) -> Option<u16> {
        let next = (self.sq_tail + 1) % self.entries;
        if next == self.sq_head {
            return None;
        }
        let slot = self.sq_tail;
        self.sq_tail = next;
        Some(slot)
    }

    /// The value for the submission tail doorbell.
    pub const fn sq_tail(&self) -> u16 {
        self.sq_tail
    }

    /// The completion slot to look at next.
    pub const fn cq_head(&self) -> u16 {
        self.cq_head
    }

    /// Takes `c`, read from slot [`Queues::cq_head`], if it is new (its phase matches):
    /// advances the head, flipping the expected phase at each wrap. Returns whether it was
    /// new; the caller then writes [`Queues::cq_head`] to the completion head doorbell.
    pub fn complete(&mut self, c: &Completion) -> bool {
        if c.phase != self.phase {
            return false;
        }
        self.cq_head = (self.cq_head + 1) % self.entries;
        if self.cq_head == 0 {
            self.phase = !self.phase;
        }
        if c.sq_head < self.entries {
            self.sq_head = c.sq_head;
        }
        true
    }
}

/// Identify Controller (CNS 01h) fields the driver reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControllerInfo {
    pub vendor: u16,
    pub serial: [u8; 20],
    pub model: [u8; 40],
    pub firmware: [u8; 8],
    /// Maximum transfer, as a power of two of the minimum page size (0: no limit).
    pub mdts: u8,
    pub namespaces: u32,
}

/// Parses Identify Controller data.
pub fn identify_controller(data: &[u8; 4096]) -> ControllerInfo {
    let take = |at: usize| data.get(at).copied().unwrap_or(0);
    let mut serial = [0u8; 20];
    let mut model = [0u8; 40];
    let mut firmware = [0u8; 8];
    serial.copy_from_slice(data.get(4..24).unwrap_or(&[0; 20]));
    model.copy_from_slice(data.get(24..64).unwrap_or(&[0; 40]));
    firmware.copy_from_slice(data.get(64..72).unwrap_or(&[0; 8]));
    ControllerInfo {
        vendor: u16::from_le_bytes([take(0), take(1)]),
        serial,
        model,
        firmware,
        mdts: take(77),
        namespaces: u32::from_le_bytes([take(516), take(517), take(518), take(519)]),
    }
}

/// An Identify text field (ASCII, space-padded) as text: printable ASCII only, trailing
/// spaces removed; anything else is cut at the first unprintable byte.
pub fn text(field: &[u8]) -> &str {
    let printable = field
        .iter()
        .position(|&b| !(0x20..0x7F).contains(&b))
        .unwrap_or(field.len());
    let field = field.get(..printable).unwrap_or(&[]);
    let end = field.iter().rposition(|&b| b != b' ').map_or(0, |i| i + 1);
    core::str::from_utf8(field.get(..end).unwrap_or(&[])).unwrap_or("")
}

/// Identify Namespace (CNS 00h) fields the driver uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NamespaceInfo {
    /// Size in logical blocks (NSZE).
    pub blocks: u64,
    /// Bytes per logical block (2^LBADS of the format FLBAS selects).
    pub block_size: u32,
    /// Metadata bytes per block in that format.
    pub metadata: u16,
}

/// Parses Identify Namespace data, or `None` for an inactive namespace (size 0), a format
/// index beyond the formats listed, or a block size outside 512 B–64 KiB.
pub fn identify_namespace(data: &[u8; 4096]) -> Option<NamespaceInfo> {
    let mut size = [0u8; 8];
    size.copy_from_slice(data.get(0..8)?);
    let blocks = u64::from_le_bytes(size);
    let formats = usize::from(*data.get(25)?) + 1;
    let flbas = *data.get(26)?;
    let index = usize::from(flbas & 0xF) | (usize::from((flbas >> 5) & 0x3) << 4);
    if blocks == 0 || index >= formats || index >= 64 {
        return None;
    }
    let at = 128 + 4 * index;
    let mut f = [0u8; 4];
    f.copy_from_slice(data.get(at..at + 4)?);
    let lbaf = u32::from_le_bytes(f);
    let lbads = (lbaf >> 16) & 0xFF;
    if !(9..=16).contains(&lbads) {
        return None;
    }
    Some(NamespaceInfo {
        blocks,
        block_size: 1 << lbads,
        metadata: lbaf as u16,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_of_qemus_controller() {
        // QEMU 11's nvme: MQES 7FFh, CQR, TO 0Fh (7.5 s), DSTRD 0, CSS NVM and others,
        // MPSMIN 0, MPSMAX 4.
        let cap = 0x7FF | (1 << 16) | (0x0F << 24) | (0xC1u64 << 37) | (4u64 << 52);
        let c = Capabilities::decode(cap);
        assert_eq!(
            c,
            Capabilities {
                max_entries: 2048,
                contiguous: true,
                timeout_ms: 7500,
                doorbell_stride: 4,
                nvm: true,
                min_page_shift: 12,
                max_page_shift: 16,
            }
        );
        assert_eq!(Capabilities::decode(3u64 << 32).doorbell_stride, 32);
        assert_eq!(version(0x0001_0400), (1, 4, 0));
        assert_eq!(version(0x0002_0100), (2, 1, 0));
    }

    #[test]
    fn doorbells_and_configuration() {
        assert_eq!(doorbell(0, false, 4), 0x1000);
        assert_eq!(doorbell(0, true, 4), 0x1004);
        assert_eq!(doorbell(1, false, 4), 0x1008);
        assert_eq!(doorbell(1, true, 32), 0x1000 + 3 * 32);
        assert_eq!(cc_enable(12), 0x0046_0001);
        assert_eq!(aqa(32), 0x001F_001F);
    }

    #[test]
    fn command_entries() {
        let id = Command::identify(7, 1, 0, 0x1234_5000).to_words();
        assert_eq!(id[0], 0x0007_0006);
        assert_eq!((id[1], id[6], id[7], id[10]), (0, 0x1234_5000, 0, 1));
        let cq = Command::create_io_cq(1, 1, 64, 0x9_0000_0000).to_words();
        assert_eq!(
            (cq[0] & 0xFF, cq[6], cq[7], cq[10], cq[11]),
            (5, 0, 9, 0x003F_0001, 1)
        );
        let irq = Command::create_io_cq_with_interrupts(1, 2, 64, 1, 0x9000).to_words();
        assert_eq!(
            (irq[10], irq[11]),
            (0x003F_0002, 0x0001_0003),
            "PC, IEN, IV 1"
        );
        let sq = Command::create_io_sq(2, 1, 64, 1, 0x8000).to_words();
        assert_eq!(
            (sq[0] & 0xFF, sq[10], sq[11]),
            (1, 0x003F_0001, 0x0001_0001)
        );
        let rd = Command::transfer(false, 9, 1, 0x1_0000_0800, 8, (0x5000, 0)).to_words();
        assert_eq!(rd[0], 0x0009_0002);
        assert_eq!((rd[1], rd[10], rd[11], rd[12]), (1, 0x800, 1, 7));
        let wr = Command::transfer(true, 3, 1, 5, 1, (0x5000, 0)).to_words();
        assert_eq!((wr[0] & 0xFF, wr[12]), (1, 0));
        assert_eq!(Command::flush(4, 1).to_words()[0], 0x0004_0000);
    }

    #[test]
    fn prp_entries() {
        assert_eq!(prps(0x5000, 4096, 4096), Some((0x5000, 0)));
        assert_eq!(prps(0x5000, 512, 4096), Some((0x5000, 0)));
        assert_eq!(
            prps(0x5E00, 1024, 4096),
            Some((0x5E00, 0x6000)),
            "crosses one boundary"
        );
        assert_eq!(prps(0x5000, 8192, 4096), Some((0x5000, 0x6000)));
        assert_eq!(prps(0x5000, 8193, 4096), None, "needs a PRP list");
        assert_eq!(prps(0x5002, 16, 4096), None, "not dword aligned");
        assert_eq!(prps(0x5000, 0, 4096), None);
        assert_eq!(prps(u64::MAX - 3, 16, 4096), None);
    }

    #[test]
    fn completions_and_phases() {
        let c = Completion::decode([0xAB, 0, 0x0001_0005, 0x0001_0009]);
        assert_eq!(
            (c.result, c.sq_head, c.sq_id, c.cid, c.phase),
            (0xAB, 5, 1, 9, true)
        );
        assert!(c.status.ok());
        // Status: SCT 0, SC 80h (LBA out of range), DNR.
        let sf: u32 = 0x80 | (1 << 14);
        let e = Completion::decode([0, 0, 0, (sf << 17) | (1 << 16) | 3]);
        assert_eq!(e.status, LBA_OUT_OF_RANGE);
        assert!(!e.status.ok());
        let mut q = Queues::new(4);
        let new = |phase, sq_head| Completion::decode([0, 0, sq_head, u32::from(phase) << 16]);
        // Three submissions fill a 4-entry queue.
        assert_eq!(
            (q.submit(), q.submit(), q.submit(), q.submit()),
            (Some(0), Some(1), Some(2), None)
        );
        assert_eq!(q.sq_tail(), 3);
        assert!(
            !q.complete(&new(false, 1)),
            "an old entry (phase 0) is not new"
        );
        for i in 0..3 {
            assert!(q.complete(&new(true, i + 1)), "completion {i}");
        }
        assert_eq!(q.cq_head(), 3);
        assert_eq!(
            q.submit(),
            Some(3),
            "room again once the controller consumed them"
        );
        assert!(q.complete(&new(true, 0)));
        assert_eq!(q.cq_head(), 0, "wrapped");
        assert!(
            !q.complete(&new(true, 0)),
            "after the wrap, new entries carry phase 0"
        );
        assert!(q.complete(&new(false, 0)));
    }

    fn identify_data() -> [u8; 4096] {
        let mut d = [0u8; 4096];
        d[0..2].copy_from_slice(&0x1B36u16.to_le_bytes());
        d[4..24].copy_from_slice(b"ORIGIN0001          ");
        d[24..64].copy_from_slice(b"QEMU NVMe Ctrl                          ");
        d[64..72].copy_from_slice(b"11.1.3  ");
        d[77] = 7;
        d[516..520].copy_from_slice(&256u32.to_le_bytes());
        d
    }

    #[test]
    fn identify_controller_fields() {
        let c = identify_controller(&identify_data());
        assert_eq!(c.vendor, 0x1B36);
        assert_eq!(text(&c.serial), "ORIGIN0001");
        assert_eq!(text(&c.model), "QEMU NVMe Ctrl");
        assert_eq!(text(&c.firmware), "11.1.3");
        assert_eq!((c.mdts, c.namespaces), (7, 256));
        assert_eq!(text(b"AB\x00CD  "), "AB", "cut at an unprintable byte");
        assert_eq!(text(b"    "), "");
    }

    /// Random Identify data never panics the parsers; an accepted namespace has a block
    /// size from 512 B to 64 KiB and at least one block; text fields are printable.
    #[test]
    fn mutations_of_identify_data_never_panic() {
        use crate::testing::{Rng, mutate};
        let mut rng = Rng::new(0x4E56_4D45);
        let mut accepted = 0;
        for case in 0..crate::testing::cases(3000) {
            let mut d = identify_data().to_vec();
            // Namespace fields too: two formats, the first of 512-byte blocks.
            d[0..8].copy_from_slice(&32768u64.to_le_bytes());
            d[25] = 1;
            d[26] = 0;
            d[128..132].copy_from_slice(&(9u32 << 16).to_le_bytes());
            mutate(&mut d, &mut rng);
            d.resize(4096, 0);
            let page: [u8; 4096] = d.try_into().unwrap();
            let c = identify_controller(&page);
            for field in [&c.serial[..], &c.model[..], &c.firmware[..]] {
                assert!(
                    text(field).bytes().all(|b| (0x20..0x7F).contains(&b)),
                    "case {case}"
                );
            }
            if let Some(ns) = identify_namespace(&page) {
                assert!(
                    ns.blocks > 0 && ns.block_size.is_power_of_two(),
                    "case {case}"
                );
                assert!((512..=65536).contains(&ns.block_size), "case {case}");
                accepted += 1;
            }
            // Any completion dwords decode.
            let _ = Completion::decode([
                rng.next() as u32,
                rng.next() as u32,
                rng.next() as u32,
                rng.next() as u32,
            ]);
        }
        assert!(accepted > 1000, "{accepted}");
    }

    #[test]
    fn identify_namespace_fields() {
        let mut d = [0u8; 4096];
        d[0..8].copy_from_slice(&32768u64.to_le_bytes());
        d[25] = 1; // two formats
        d[26] = 1; // format 1 in use
        d[128..132].copy_from_slice(&(9u32 << 16).to_le_bytes());
        d[132..136].copy_from_slice(&((12u32 << 16) | 8).to_le_bytes());
        assert_eq!(
            identify_namespace(&d),
            Some(NamespaceInfo {
                blocks: 32768,
                block_size: 4096,
                metadata: 8
            })
        );
        d[26] = 0;
        assert_eq!(identify_namespace(&d).map(|n| n.block_size), Some(512));
        d[26] = 2;
        assert_eq!(identify_namespace(&d), None, "beyond the formats listed");
        d[26] = 0;
        d[128..132].copy_from_slice(&(8u32 << 16).to_le_bytes());
        assert_eq!(identify_namespace(&d), None, "256-byte blocks");
        d[128..132].copy_from_slice(&(9u32 << 16).to_le_bytes());
        d[0..8].copy_from_slice(&0u64.to_le_bytes());
        assert_eq!(identify_namespace(&d), None, "inactive");
    }
}
