//! NVM Express over PCIe (CLAUDE.md §33): controller registers, queue entries, queue
//! indexes and Identify data, as plain data tested on the host, and the decisions the driver
//! takes from them. The kernel's driver (`src/storage.rs`) does the MMIO and DMA.
//!
//! References, the revisions CLAUDE.md §17 pins, read on 2026-11-14 (PDF pages; the reading
//! and what it found are in `docs/research/2026-11-14-nvme-2-4-check.md`):
//! - NVM Express Base Specification 2.4: §3.1.4 (the controller registers CAP, VS, INTMS, CC,
//!   CSTS, AQA, ASQ, ACQ and CRTO), §3.5.1 and §3.5.4 (initialization and its ready
//!   timeouts), §4.1 (the submission queue entry), §4.2 and §4.2.4 (the completion queue
//!   entry and its phase tag), §4.3.1 (PRP entries), §4.6.2 (the Namespace List), §5.2.14
//!   (Identify: CNS 00h a namespace, 01h the controller, 02h and 07h the active namespaces),
//!   §5.2.30.1.5 (Set Features, Number of Queues 07h), §5.3.1 and §5.3.2 (Create I/O
//!   Completion Queue 05h, Create I/O Submission Queue 01h), §7.2 (Flush 00h);
//! - NVM Command Set Specification 1.3: §3.3.4 (Read 02h), §3.3.6 (Write 01h), and the
//!   Identify Namespace fields NSZE, NLBAF, FLBAS, DPS, NULBAF and the LBA formats (§4.1.5.1,
//!   Figures 123 and 125; §5.6);
//! - NVMe over PCIe Transport Specification 1.4: §3.1.2 (register access, the doorbells),
//!   §3.2 (a completion queue's interrupt vector), §3.5 (interrupts).
//!
//! Everything a controller returns is untrusted: Identify data is parsed with bounds
//! checks, and a completion is accepted only with the expected phase, queue and command ID.

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
    /// Controller ready timeouts, from a controller that reports ready modes (CAP.CRMS).
    pub const CRTO: u64 = 0x68;
}

/// CC.EN.
pub const CC_ENABLE: u32 = 1 << 0;
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
    pub const SET_FEATURES: u8 = 0x09;
    pub const FLUSH: u8 = 0x00;
    pub const WRITE: u8 = 0x01;
    pub const READ: u8 = 0x02;
}

/// Identify's CNS values the driver sends (Base 2.4 Figure 336, p. 364).
pub mod cns {
    /// The Identify Namespace data structure of one NSID.
    pub const NAMESPACE: u8 = 0x00;
    /// The Identify Controller data structure.
    pub const CONTROLLER: u8 = 0x01;
    /// The active NSIDs, of every I/O command set (mandatory from 1.1).
    pub const ACTIVE_NAMESPACES: u8 = 0x02;
    /// The active NSIDs of the I/O command set CDW11.CSI names (from 2.0).
    pub const ACTIVE_NAMESPACES_OF_SET: u8 = 0x07;
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
    /// I/O command sets beside the NVM Command Set may be offered, with the Identify I/O
    /// Command Set data structure (CSS bit 6, IOCSS).
    pub io_command_sets: bool,
    /// Smallest and largest memory page sizes, as shifts (12 + MPSMIN, 12 + MPSMAX).
    pub min_page_shift: u32,
    pub max_page_shift: u32,
    /// Controller ready modes supported (CRMS): 0 from a controller older than 2.0, which has
    /// no CRTO register; otherwise bit 0 (ready with media) is set, and bit 1 if the
    /// controller can also be ready independent of media.
    pub ready_modes: u8,
}

impl Capabilities {
    pub const fn decode(cap: u64) -> Self {
        Self {
            max_entries: (cap & 0xFFFF) as u32 + 1,
            contiguous: cap & (1 << 16) != 0,
            timeout_ms: ((cap >> 24) & 0xFF) * 500,
            doorbell_stride: 4 << ((cap >> 32) & 0xF),
            nvm: (cap >> 37) & 1 != 0,
            io_command_sets: (cap >> 43) & 1 != 0,
            min_page_shift: 12 + ((cap >> 48) & 0xF) as u32,
            max_page_shift: 12 + ((cap >> 52) & 0xF) as u32,
            ready_modes: ((cap >> 59) & 0x3) as u8,
        }
    }
}

/// The version register as `(major, minor, tertiary)`.
pub const fn version(vs: u32) -> (u16, u8, u8) {
    ((vs >> 16) as u16, (vs >> 8) as u8, vs as u8)
}

/// How long the driver waits for CSTS.RDY to change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadyTimeouts {
    /// For RDY to become 1 after CC.EN becomes 1.
    pub enable_ms: u64,
    /// For RDY to become 0 after CC.EN becomes 0.
    pub disable_ms: u64,
}

/// The ready waits for a controller with capabilities `cap` and CRTO register `crto` (Base 2.4
/// Figure 36, CRMS p. 81 and TO p. 83; §3.5.4, pp. 137–138; Figure 57, p. 99). The driver
/// reads CRTO only when `cap.ready_modes` is not 0: from an older controller offset 68h is
/// reserved, and 0 is given here. With ready modes, the rise waits CRTO.CRWMT, in 500 ms
/// units, since the driver keeps CC.CRIME at 0 (ready with media); never less than CAP.TO,
/// which such a controller sets from CRWMT where CRWMT fits in it, and however much more the
/// controller states (at most FFFFh units, about 9.1 hours). Without ready modes, the rise
/// waits CAP.TO. The fall always waits CAP.TO. Each wait is at least one unit, 500 ms.
pub const fn ready_timeouts(cap: &Capabilities, crto: u32) -> ReadyTimeouts {
    const UNIT_MS: u64 = 500;
    let to = if cap.timeout_ms > UNIT_MS {
        cap.timeout_ms
    } else {
        UNIT_MS
    };
    let crwmt = (crto & 0xFFFF) as u64 * UNIT_MS;
    ReadyTimeouts {
        enable_ms: if cap.ready_modes != 0 && crwmt > to {
            crwmt
        } else {
            to
        },
        disable_ms: to,
    }
}

/// What the driver does first with a controller it finds, before its reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetStep {
    /// CC.EN is 0: wait for CSTS.RDY to be 0 (an earlier reset finishing).
    AwaitIdle,
    /// CC.EN is 1, and CSTS.RDY or CSTS.CFS is 1: clear EN now. With fatal status a reset is
    /// what the host should do (Base 2.4 §9.5, p. 852).
    Clear,
    /// CC.EN is 1 with RDY and CFS both 0: the controller is still becoming ready. Wait for RDY
    /// or CFS to become 1, then clear EN: clearing EN while RDY is 0 has undefined results
    /// (CC.EN, Base 2.4 Figure 41, p. 89).
    AwaitReady,
}

/// The step for a controller whose CC and CSTS read `cc` and `csts`.
pub const fn before_reset(cc: u32, csts: u32) -> ResetStep {
    if cc & CC_ENABLE == 0 {
        ResetStep::AwaitIdle
    } else if csts & (CSTS_READY | CSTS_FATAL) != 0 {
        ResetStep::Clear
    } else {
        ResetStep::AwaitReady
    }
}

/// The offset of queue `queue`'s submission tail (or, with `completion`, completion head)
/// doorbell.
pub const fn doorbell(queue: u16, completion: bool, stride: u64) -> u64 {
    0x1000 + (2 * queue as u64 + completion as u64) * stride
}

/// Whether the doorbells of queues up to `queue` lie inside a `bar_size`-byte register
/// window. CAP's stride is the device's word: at its largest (128 KiB) a few queues' doorbells
/// are already megabytes past a normal BAR.
pub const fn doorbells_fit(queue: u16, stride: u64, bar_size: u64) -> bool {
    doorbell(queue, true, stride) + 4 <= bar_size
}

/// CC's configuration with the controller still disabled (EN 0): the NVM Command Set (CSS
/// 000b), round-robin arbitration (AMS 000b), memory pages of 2^`page_shift` bytes (MPS),
/// 64-byte submission and 16-byte completion entries (IOSQES 6, IOCQES 4), no shutdown and
/// CRIME 0. The driver writes it before it sets EN (Base 2.4 §3.5.1 steps 3 to 5,
/// pp. 131–132).
pub const fn cc_config(page_shift: u32) -> u32 {
    ((page_shift - 12) << 7) | (6 << 16) | (4 << 20)
}

/// CC to enable the controller: [`cc_config`] with EN set.
pub const fn cc_enable(page_shift: u32) -> u32 {
    cc_config(page_shift) | CC_ENABLE
}

/// The most entries the driver gives an I/O queue: 64 submission entries fill one 4 KiB page.
pub const IO_QUEUE_ENTRIES: u16 = 64;

/// The entries of each I/O queue for a controller with capabilities `cap`: 64, or as many as
/// it allows if fewer (MQES + 1), or `None` if that is below 2, the smallest valid queue. MQES
/// applies to the I/O queues on PCIe; AQA sizes the admin queues (Base 2.4 Figure 36, p. 84;
/// Figure 44, p. 92).
pub const fn io_queue_entries(cap: &Capabilities) -> Option<u16> {
    if cap.max_entries < 2 {
        None
    } else if cap.max_entries < IO_QUEUE_ENTRIES as u32 {
        Some(cap.max_entries as u16)
    } else {
        Some(IO_QUEUE_ENTRIES)
    }
}

/// The command identifier after `cid`. FFFFh is never given: the Error Information log page
/// uses it for an error tied to no command (Base 2.4 Figure 92, p. 165).
pub const fn next_cid(cid: u16) -> u16 {
    match cid.wrapping_add(1) {
        0xFFFF => 0,
        n => n,
    }
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

    /// Identify, 4 KiB into `prp1`: CNS 01h (the controller, `nsid` 0), 00h (namespace
    /// `nsid`), or 02h and 07h (the active NSIDs above `nsid`). CDW11 is 0, which for CNS 07h
    /// names CSI 0, the NVM Command Set (Base 2.4 Figure 334, p. 363).
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

    /// Set Features, Number of Queues (feature 07h): asks for `submission` and
    /// `completion` I/O queues (1–65535 each). The specification allows this only before
    /// any I/O queue is created; the completion says how many the controller allocated
    /// ([`allocated_queues`]).
    pub const fn set_number_of_queues(cid: u16, submission: u16, completion: u16) -> Self {
        Self {
            opcode: opcode::SET_FEATURES,
            cid,
            nsid: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 0x07,
            cdw11: (((completion - 1) as u32) << 16) | (submission - 1) as u32,
            cdw12: 0,
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

/// The I/O queues a Number of Queues completion allocated: `(submission, completion)`.
pub const fn allocated_queues(result: u32) -> (u32, u32) {
    ((result & 0xFFFF) + 1, (result >> 16) + 1)
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

    /// Whether this is the status `other` names, by its type and code. DNR says only whether
    /// a retry could succeed, and no status code requires it (Base 2.4 Figure 101, p. 171).
    pub const fn is(&self, other: &Status) -> bool {
        self.kind == other.kind && self.code == other.code
    }
}

/// The driver's own deadline for a Flush, for an I/O command of any other kind, and for an
/// admin command, in milliseconds. Base 2.4 sets no command timeout for a controller on PCIe
/// (§3.9, p. 155): these are the driver's values (TASKS item 59, finding P1), to be revisited
/// when Abort or a controller reset is built. A command past its deadline may still be live,
/// so its queue is dead from then on; the deadlines are long so that a slow flush, on a
/// loaded host or a drive with a large volatile cache, does not end a queue.
pub const FLUSH_DEADLINE_MS: u64 = 30_000;
pub const IO_DEADLINE_MS: u64 = 10_000;
pub const ADMIN_DEADLINE_MS: u64 = 2_000;

/// The deadline of a command with `opcode`, on an admin queue or an I/O queue. Opcodes are
/// per command set: admin 00h is Delete I/O Submission Queue, not Flush.
pub const fn deadline_ms(admin: bool, opcode: u8) -> u64 {
    if admin {
        ADMIN_DEADLINE_MS
    } else if opcode == opcode::FLUSH {
        FLUSH_DEADLINE_MS
    } else {
        IO_DEADLINE_MS
    }
}

/// Why a command did not end in success, for the line that reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// It completed with this status, not success.
    Status(Status),
    /// No completion came within this many milliseconds, its deadline. The command may still
    /// be live, so its queue is dead from then on.
    NoCompletion { ms: u64 },
    /// The next completion on its queue answered command `cid` of submission queue `sq_id`
    /// instead: the controller's error. This command may still be live, so its queue is dead
    /// from then on.
    WrongCommand { cid: u16, sq_id: u16 },
    /// Refused before it reached the device: another command was outstanding on its queue.
    Busy,
    /// Refused before it reached the device: its queue had lost an earlier command.
    Dead,
    /// Refused before it reached the device: its queue had no free slot.
    Full,
    /// Refused before it reached the device: its buffer would need a PRP list.
    NeedsPrpList,
    /// Refused: no controller or queue for it.
    NoQueue,
}

impl Fault {
    /// Whether the command may still be live in the device, so that its buffer must never be
    /// used again.
    pub const fn may_be_live(&self) -> bool {
        matches!(self, Self::NoCompletion { .. } | Self::WrongCommand { .. })
    }
}

impl core::fmt::Display for Fault {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::Status(s) => write!(
                f,
                "completed with status type {} code {:#04x}{}",
                s.kind,
                s.code,
                if s.final_ { " (do not retry)" } else { "" }
            ),
            Self::NoCompletion { ms } => write!(f, "no completion within {ms} ms"),
            Self::WrongCommand { cid, sq_id } => write!(
                f,
                "a completion for command {cid} of submission queue {sq_id} came instead"
            ),
            Self::Busy => f.write_str("refused: another command was outstanding on its queue"),
            Self::Dead => f.write_str("refused: its queue had lost an earlier command"),
            Self::Full => f.write_str("refused: its queue was full"),
            Self::NeedsPrpList => f.write_str("refused: its buffer would need a PRP list"),
            Self::NoQueue => f.write_str("refused: no queue for it"),
        }
    }
}

/// A command's step in a proof and why it did not succeed: printed as "`step`: `fault`".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepFault {
    pub step: &'static str,
    pub fault: Fault,
}

impl core::fmt::Display for StepFault {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}: {}", self.step, self.fault)
    }
}

/// Generic command status (SCT 0) 80h: LBA out of range (NVM Command Set 1.3 Figure 18,
/// p. 25). Compare with [`Status::is`].
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

    /// Whether this completion answers command `cid` of submission queue `sqid`. With a
    /// completion queue for each submission queue, any other is the controller's error (Base
    /// 2.4 Figure 98, p. 170).
    pub const fn answers(&self, sqid: u16, cid: u16) -> bool {
        self.sq_id == sqid && self.cid == cid
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

/// Identify Controller (CNS 01h) fields the driver reports or checks (Base 2.4 Figure 338).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControllerInfo {
    pub vendor: u16,
    pub serial: [u8; 20],
    pub model: [u8; 40],
    pub firmware: [u8; 8],
    /// Maximum transfer, as a power of two of the minimum page size (0: no limit).
    pub mdts: u8,
    /// CNTRLTYPE (byte 111): 1h an I/O controller; see [`io_controller`].
    pub controller_type: u8,
    /// NN: the largest valid NSID.
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
        controller_type: take(111),
        namespaces: u32::from_le_bytes([take(516), take(517), take(518), take(519)]),
    }
}

/// Whether a controller of type `kind` (CNTRLTYPE) that reports version `version` is an I/O
/// controller, the only type with I/O queues. 1h is. 0h means "not reported", which a
/// controller older than 1.4 may give and a later one may not (Base 2.4 Figure 338, byte 111,
/// p. 375).
pub const fn io_controller(kind: u8, version: (u16, u8, u8)) -> bool {
    kind == 1 || (kind == 0 && (version.0 < 1 || (version.0 == 1 && version.1 < 4)))
}

/// CNTRLTYPE's meaning, for a refusal.
pub const fn controller_type_name(kind: u8) -> &'static str {
    match kind {
        0 => "not reported",
        1 => "an I/O controller",
        2 => "a discovery controller",
        3 => "an administrative controller",
        _ => "a reserved type",
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

/// The Identify that lists the controller's active NVM namespaces. CNS 07h with CSI 0 lists
/// only namespaces of the NVM Command Set, the one set the driver enables (CC.CSS 000b): it is
/// sent to a controller that follows 2.0 or later or offers I/O command sets (IOCSS). CNS 02h,
/// mandatory from 1.1, lists every active namespace, and is sent to an older controller, which
/// knows no other set (Base 2.4 §3.5.1 step 8, p. 132; Figure 336, p. 364; §5.2.14.2.2 and
/// §5.2.14.2.7, pp. 413 and 416). A namespace of another set, such as a zoned one, is so never
/// listed from a controller that has one.
pub const fn active_list_cns(cap: &Capabilities, version: (u16, u8, u8)) -> u8 {
    if version.0 >= 2 || cap.io_command_sets {
        cns::ACTIVE_NAMESPACES_OF_SET
    } else {
        cns::ACTIVE_NAMESPACES
    }
}

/// The active NSIDs of a Namespace List, from [`active_namespaces`].
#[derive(Clone, Debug)]
pub struct ActiveNamespaces<'a> {
    list: &'a [u8; 4096],
    at: usize,
    last: u32,
    max: u32,
    done: bool,
}

/// Walks a Namespace List (Base 2.4 §4.6.2, p. 198) that Identify CNS 02h or 07h returned for
/// NSID 0: its NSIDs in increasing order, ending at the first 0h (unused entries are 0h) or
/// after 1,024. Each is `Ok`. An entry that is not a valid NSID above the one before it is the
/// controller's error: one above `max` (NN, the largest valid NSID), FFFFFFFEh or FFFFFFFFh
/// (never NSIDs), or not above the one before. It is given as `Err` and ends the walk.
pub fn active_namespaces(list: &[u8; 4096], max: u32) -> ActiveNamespaces<'_> {
    ActiveNamespaces {
        list,
        at: 0,
        last: 0,
        max,
        done: false,
    }
}

impl Iterator for ActiveNamespaces<'_> {
    type Item = Result<u32, u32>;

    fn next(&mut self) -> Option<Result<u32, u32>> {
        if self.done {
            return None;
        }
        let Some(&[a, b, c, d]) = self.list.get(self.at..self.at + 4) else {
            self.done = true;
            return None;
        };
        self.at += 4;
        let nsid = u32::from_le_bytes([a, b, c, d]);
        if nsid == 0 {
            self.done = true;
            return None;
        }
        if nsid <= self.last || nsid > self.max || nsid >= 0xFFFF_FFFE {
            self.done = true;
            return Some(Err(nsid));
        }
        self.last = nsid;
        Some(Ok(nsid))
    }
}

/// Identify Namespace (CNS 00h) fields the driver uses, of a namespace it can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NamespaceInfo {
    /// Size in logical blocks (NSZE).
    pub blocks: u64,
    /// Bytes per logical block (2^LBADS of the format FLBAS selects).
    pub block_size: u32,
}

impl NamespaceInfo {
    /// Whether each logical block fits in one `page`-byte memory page. Longer blocks would
    /// need a PRP list per block, which this driver does not build.
    pub const fn block_fits_page(&self, page: u64) -> bool {
        self.block_size as u64 <= page
    }
}

/// Why Identify Namespace data describes a namespace the driver cannot use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamespaceRefusal {
    /// Its size is 0: an inactive NSID gives a zeroed structure.
    Inactive,
    /// The format it is formatted with lies beyond the formats it lists.
    FormatIndex { index: u16, formats: u16 },
    /// Protection information of this type (DPS.PIT; 4 to 7 are reserved) is on.
    ProtectionInformation(u8),
    /// Its format carries this many metadata bytes with each block, extended or in a separate
    /// buffer: the driver sets no metadata pointer and sizes its transfers by data alone.
    Metadata(u16),
    /// Its format's blocks are 2^n bytes, outside 512 B to 64 KiB (n = 0: the format is not
    /// available).
    BlockSize(u8),
    /// Its size in bytes does not fit in 64 bits.
    TooLarge,
}

impl core::fmt::Display for NamespaceRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::Inactive => f.write_str("it is inactive (size 0)"),
            Self::FormatIndex { index, formats } => write!(
                f,
                "it is formatted with LBA format {index}, beyond the {formats} it lists"
            ),
            Self::ProtectionInformation(kind @ 1..=3) => write!(
                f,
                "protection information type {kind} is on, which this driver does not support"
            ),
            Self::ProtectionInformation(kind) => write!(
                f,
                "protection information of a reserved type ({kind}) is on, which this driver does not support"
            ),
            Self::Metadata(bytes) => write!(
                f,
                "its LBA format carries {bytes} metadata bytes with each block, which this driver does not support"
            ),
            Self::BlockSize(0) => f.write_str("its LBA format is not available (LBADS 0)"),
            Self::BlockSize(n) => {
                write!(f, "its blocks of 2^{n} bytes are outside 512 B to 64 KiB")
            }
            Self::TooLarge => f.write_str("its size in bytes does not fit in 64 bits"),
        }
    }
}

/// Parses Identify Namespace data (NVM Command Set 1.3 Figures 123 and 125, §5.6) into a
/// namespace the driver can use, or says why it cannot: inactive (size 0); formatted with an
/// index beyond the formats listed; protection information on (DPS.PIT not 0); metadata in
/// the format in use (MS not 0); blocks outside 512 B to 64 KiB; or a size in bytes beyond 64
/// bits (NSZE is the device's word, and every use of it multiplies by the block size).
/// Protection information is looked at before metadata, which it needs, so a namespace with
/// both is refused for the first.
///
/// The formats listed are NLBAF + 1 (byte 25, zero-based) and NULBAF more (byte 82); FLBAS
/// gives the format in use, its bits 6:5 (FIDXU) only when more than 16 formats are listed.
/// The format table holds 64.
pub fn usable_namespace(data: &[u8; 4096]) -> Result<NamespaceInfo, NamespaceRefusal> {
    let byte = |at: usize| data.get(at).copied().unwrap_or(0);
    let mut size = [0u8; 8];
    size.copy_from_slice(data.get(0..8).unwrap_or(&[0; 8]));
    let blocks = u64::from_le_bytes(size);
    if blocks == 0 {
        return Err(NamespaceRefusal::Inactive);
    }
    let formats = u16::from(byte(25)) + 1 + u16::from(byte(82));
    let flbas = byte(26);
    let lower = u16::from(flbas & 0xF);
    let index = if formats > 16 {
        lower | (u16::from((flbas >> 5) & 0x3) << 4)
    } else {
        lower
    };
    if index >= formats || index >= 64 {
        return Err(NamespaceRefusal::FormatIndex { index, formats });
    }
    let protection = byte(29) & 0x7;
    if protection != 0 {
        return Err(NamespaceRefusal::ProtectionInformation(protection));
    }
    let at = 128 + 4 * usize::from(index);
    let lbaf = u32::from_le_bytes([byte(at), byte(at + 1), byte(at + 2), byte(at + 3)]);
    let metadata = lbaf as u16;
    if metadata != 0 {
        return Err(NamespaceRefusal::Metadata(metadata));
    }
    let lbads = (lbaf >> 16) as u8;
    if !(9..=16).contains(&lbads) {
        return Err(NamespaceRefusal::BlockSize(lbads));
    }
    let block_size: u32 = 1 << lbads;
    blocks
        .checked_mul(u64::from(block_size))
        .ok_or(NamespaceRefusal::TooLarge)?;
    Ok(NamespaceInfo { blocks, block_size })
}

/// [`usable_namespace`] without its reason: the driver's call until it names the reason.
pub fn identify_namespace(data: &[u8; 4096]) -> Option<NamespaceInfo> {
    usable_namespace(data).ok()
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
                io_command_sets: true,
                min_page_shift: 12,
                max_page_shift: 16,
                ready_modes: 0,
            }
        );
        assert_eq!(Capabilities::decode(3u64 << 32).doorbell_stride, 32);
        assert_eq!(version(0x0001_0400), (1, 4, 0));
        assert_eq!(version(0x0002_0100), (2, 1, 0));
        // CRMS is bits 60:59; IOCSS is CSS bit 6, CAP bit 43; NCSS alone is not IOCSS.
        assert_eq!(Capabilities::decode(1 << 59).ready_modes, 1);
        assert_eq!(Capabilities::decode(3 << 59).ready_modes, 3);
        assert_eq!(Capabilities::decode(1 << 61).ready_modes, 0);
        assert!(Capabilities::decode(1 << 43).io_command_sets);
        assert!(!Capabilities::decode(1 << 37).io_command_sets);
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

    /// L1: the configuration is written with EN 0, and the enabling write differs from it in EN
    /// alone (Base 2.4 §3.5.1 steps 3 to 5).
    #[test]
    fn cc_is_configured_before_it_is_enabled() {
        assert_eq!(cc_config(12) & CC_ENABLE, 0);
        assert_eq!(cc_config(12), 0x0046_0000);
        assert_eq!(cc_enable(12) ^ cc_config(12), CC_ENABLE);
        assert_eq!(cc_config(13), 0x0046_0080, "MPS 1: 8 KiB pages");
    }

    /// M4: the rise waits CRTO.CRWMT only from a controller with ready modes, never less than
    /// CAP.TO; the fall waits CAP.TO; neither waits less than 500 ms.
    #[test]
    fn ready_timeouts_follow_crto_only_with_ready_modes() {
        let cap = |to: u64, modes: u64| Capabilities::decode((to << 24) | (modes << 59));
        let t = |to, modes, crto| {
            let r = ready_timeouts(&cap(to, modes), crto);
            (r.enable_ms, r.disable_ms)
        };
        // No ready modes (older than 2.0): CAP.TO, whatever offset 68h held.
        assert_eq!(t(0x0F, 0, 0), (7500, 7500));
        assert_eq!(t(0x0F, 0, 0xFFFF), (7500, 7500));
        // Ready with media (01b) and both modes (11b): CRWMT where it is longer.
        for modes in [1, 3] {
            assert_eq!(t(0x0F, modes, 0x000E), (7500, 7500), "below CAP.TO");
            assert_eq!(t(0x0F, modes, 0x000F), (7500, 7500), "equal");
            assert_eq!(t(0xFF, modes, 0x0400), (512_000, 127_500), "above FFh");
            assert_eq!(
                t(0xFF, modes, 0xFFFF),
                (32_767_500, 127_500),
                "the most it can state"
            );
            // CRIMT (bits 31:16) does not apply: the driver keeps CRIME at 0.
            assert_eq!(t(0x0F, modes, 0xFFFF_0000), (7500, 7500));
        }
        // A TO of 0 still waits one unit.
        assert_eq!(t(0, 0, 0), (500, 500));
        assert_eq!(t(0, 1, 0), (500, 500));
        assert_eq!(t(0, 1, 3), (1500, 500));
    }

    /// M5: EN is cleared at once only from a ready or failed controller; one still becoming
    /// ready is waited for first.
    #[test]
    fn a_controller_still_becoming_ready_is_waited_for_before_its_reset() {
        for other in [0u32, 0xFFFF_FFF0, 0b1100] {
            for cc in [0u32, 1] {
                for csts in 0..4u32 {
                    let step = before_reset(cc | (other & !1), csts | (other & !3));
                    let expected = match (cc, csts & 1, csts >> 1) {
                        (0, _, _) => ResetStep::AwaitIdle,
                        (1, 0, 0) => ResetStep::AwaitReady,
                        _ => ResetStep::Clear,
                    };
                    assert_eq!(
                        step, expected,
                        "CC {cc}, CSTS {csts:#b}, other bits {other:#x}"
                    );
                }
            }
        }
    }

    /// L3: FFFFh is never a command identifier.
    #[test]
    fn command_identifiers_skip_ffff() {
        assert_eq!(next_cid(0), 1);
        assert_eq!(next_cid(0xFFFD), 0xFFFE);
        assert_eq!(next_cid(0xFFFE), 0, "FFFFh skipped");
        assert_eq!(next_cid(0xFFFF), 0);
        let mut cid = 0u16;
        for _ in 0..3 * 65536 {
            cid = next_cid(cid);
            assert_ne!(cid, 0xFFFF);
        }
    }

    /// M2: an I/O queue takes 64 entries, or what the controller allows down to 2.
    #[test]
    fn io_queues_take_what_the_controller_allows() {
        let entries = |mqes: u64| io_queue_entries(&Capabilities::decode(mqes));
        assert_eq!(entries(0), None, "MQES 0, one entry, is not valid");
        assert_eq!(entries(1), Some(2), "the smallest valid queue");
        assert_eq!(entries(2), Some(3));
        assert_eq!(entries(62), Some(63));
        assert_eq!(entries(63), Some(64));
        assert_eq!(entries(64), Some(64));
        assert_eq!(entries(0x7FF), Some(64));
        assert_eq!(entries(0xFFFF), Some(64), "MQES + 1 = 65,536");
        // 64 submission entries fill one page, and so do fewer.
        assert_eq!(usize::from(IO_QUEUE_ENTRIES) * SQ_ENTRY, 4096);
    }

    #[test]
    fn command_entries() {
        let id = Command::identify(7, 1, 0, 0x1234_5000).to_words();
        assert_eq!(id[0], 0x0007_0006);
        assert_eq!((id[1], id[6], id[7], id[10]), (0, 0x1234_5000, 0, 1));
        // M3: CNS 07h with CSI 0 in CDW11 31:24, from NSID 0.
        let list = Command::identify(8, cns::ACTIVE_NAMESPACES_OF_SET, 0, 0x6000).to_words();
        assert_eq!((list[1], list[10], list[11]), (0, 0x07, 0));
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
        let small = Command::create_io_sq(2, 1, 4, 1, 0x8000).to_words();
        assert_eq!(small[10], 0x0003_0001, "a queue of 4 entries");
        let rd = Command::transfer(false, 9, 1, 0x1_0000_0800, 8, (0x5000, 0)).to_words();
        assert_eq!(rd[0], 0x0009_0002);
        assert_eq!((rd[1], rd[10], rd[11], rd[12]), (1, 0x800, 1, 7));
        let wr = Command::transfer(true, 3, 1, 5, 1, (0x5000, 0)).to_words();
        assert_eq!((wr[0] & 0xFF, wr[12]), (1, 0));
        assert_eq!(Command::flush(4, 1).to_words()[0], 0x0004_0000);
        let q = Command::set_number_of_queues(5, 6, 6).to_words();
        assert_eq!((q[0], q[10], q[11]), (0x0005_0009, 7, 0x0005_0005));
        assert_eq!(allocated_queues(0x003F_003F), (64, 64));
        assert_eq!(allocated_queues(0x0001_0007), (8, 2));
        assert_eq!(allocated_queues(0), (1, 1), "one pair");
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
        // The smallest valid queue, two entries, takes one command at a time.
        let mut two = Queues::new(2);
        assert_eq!((two.submit(), two.submit()), (Some(0), None));
        assert!(two.complete(&new(true, 1)));
        assert_eq!(two.submit(), Some(1));
    }

    /// L5: LBA Out of Range is its type and code, with DNR or without.
    #[test]
    fn a_status_is_its_type_and_code() {
        let status = |sc: u32, sct: u32, dnr: u32| {
            let sf = sc | (sct << 8) | (dnr << 14);
            Completion::decode([0, 0, 0, sf << 17]).status
        };
        assert!(status(0x80, 0, 1).is(&LBA_OUT_OF_RANGE));
        assert!(status(0x80, 0, 0).is(&LBA_OUT_OF_RANGE), "without DNR");
        assert!(!status(0x80, 1, 1).is(&LBA_OUT_OF_RANGE), "another type");
        assert!(!status(0x81, 0, 1).is(&LBA_OUT_OF_RANGE), "another code");
        assert!(!status(0, 0, 0).is(&LBA_OUT_OF_RANGE));
    }

    /// P1: the driver's own deadlines, by queue and opcode.
    #[test]
    fn deadlines_by_command() {
        assert_eq!(deadline_ms(false, opcode::FLUSH), 30_000);
        assert_eq!(deadline_ms(false, opcode::READ), 10_000);
        assert_eq!(deadline_ms(false, opcode::WRITE), 10_000);
        assert_eq!(deadline_ms(true, 0x00), 2_000, "admin 00h is not Flush");
        assert_eq!(deadline_ms(true, opcode::IDENTIFY), 2_000);
        assert_eq!(deadline_ms(true, opcode::CREATE_IO_CQ), 2_000);
        const { assert!(FLUSH_DEADLINE_MS >= IO_DEADLINE_MS && IO_DEADLINE_MS >= ADMIN_DEADLINE_MS) };
    }

    /// P1: each way a command can fail names itself, and says whether the command may still
    /// be live.
    #[test]
    fn a_fault_names_itself() {
        let name = |f: Fault| alloc::format!("{f}");
        let lba = Completion::decode([0, 0, 0, (0x80 | 1 << 14) << 17]).status;
        assert_eq!(
            name(Fault::Status(lba)),
            "completed with status type 0 code 0x80 (do not retry)"
        );
        let retry = Completion::decode([0, 0, 0, (0x06 | 2 << 8) << 17]).status;
        assert_eq!(
            name(Fault::Status(retry)),
            "completed with status type 2 code 0x06"
        );
        assert_eq!(
            name(Fault::NoCompletion { ms: 30_000 }),
            "no completion within 30000 ms"
        );
        assert_eq!(
            name(Fault::WrongCommand { cid: 7, sq_id: 3 }),
            "a completion for command 7 of submission queue 3 came instead"
        );
        for (f, text) in [
            (
                Fault::Busy,
                "refused: another command was outstanding on its queue",
            ),
            (
                Fault::Dead,
                "refused: its queue had lost an earlier command",
            ),
            (Fault::Full, "refused: its queue was full"),
            (
                Fault::NeedsPrpList,
                "refused: its buffer would need a PRP list",
            ),
            (Fault::NoQueue, "refused: no queue for it"),
        ] {
            assert_eq!(name(f), text);
            assert!(!f.may_be_live(), "{text}");
        }
        assert!(!Fault::Status(lba).may_be_live());
        assert!(Fault::NoCompletion { ms: 1 }.may_be_live());
        assert!(Fault::WrongCommand { cid: 0, sq_id: 0 }.may_be_live());
        assert_eq!(
            alloc::format!(
                "{}",
                StepFault {
                    step: "its flush",
                    fault: Fault::NoCompletion { ms: 30_000 }
                }
            ),
            "its flush: no completion within 30000 ms"
        );
    }

    /// L10: a completion answers a command only on that command's own submission queue.
    #[test]
    fn a_completion_answers_its_own_queue_s_command() {
        let c = Completion::decode([0, 0, (3 << 16) | 4, (1 << 16) | 9]);
        assert!(c.answers(3, 9));
        assert!(!c.answers(2, 9), "another queue's completion");
        assert!(!c.answers(3, 8), "another command");
    }

    fn identify_data() -> [u8; 4096] {
        let mut d = [0u8; 4096];
        d[0..2].copy_from_slice(&0x1B36u16.to_le_bytes());
        d[4..24].copy_from_slice(b"ORIGIN0001          ");
        d[24..64].copy_from_slice(b"QEMU NVMe Ctrl                          ");
        d[64..72].copy_from_slice(b"11.1.3  ");
        d[77] = 7;
        d[111] = 1;
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
        assert_eq!((c.mdts, c.controller_type, c.namespaces), (7, 1, 256));
        assert_eq!(text(b"AB\x00CD  "), "AB", "cut at an unprintable byte");
        assert_eq!(text(b"    "), "");
    }

    /// L9: only an I/O controller has I/O queues; "not reported" is accepted only from a
    /// controller older than 1.4.
    #[test]
    fn only_an_io_controller_is_bound() {
        for (version, accepted) in [
            ((1, 3, 0), [true, true, false, false, false]),
            ((1, 4, 0), [false, true, false, false, false]),
            ((2, 0, 0), [false, true, false, false, false]),
            ((2, 4, 0), [false, true, false, false, false]),
        ] {
            for (kind, &ok) in accepted.iter().enumerate() {
                assert_eq!(
                    io_controller(kind as u8, version),
                    ok,
                    "type {kind}, version {version:?}"
                );
            }
            assert!(!io_controller(0xFF, version));
        }
        assert_eq!(controller_type_name(1), "an I/O controller");
        assert_eq!(controller_type_name(3), "an administrative controller");
        assert_eq!(controller_type_name(9), "a reserved type");
    }

    /// A Namespace List page holding `nsids`, the rest zero.
    fn namespace_list(nsids: &[u32]) -> [u8; 4096] {
        let mut d = [0u8; 4096];
        for (i, n) in nsids.iter().enumerate() {
            d[4 * i..4 * i + 4].copy_from_slice(&n.to_le_bytes());
        }
        d
    }

    /// M3: the active NSIDs are walked in order; an entry that is not a valid NSID above the
    /// one before ends the walk as the controller's error.
    #[test]
    fn active_namespace_lists() {
        let walk =
            |nsids: &[u32], max| active_namespaces(&namespace_list(nsids), max).collect::<Vec<_>>();
        assert_eq!(walk(&[1], 256), [Ok(1)]);
        assert_eq!(walk(&[2], 256), [Ok(2)], "no namespace 1");
        assert_eq!(walk(&[7, 9, 200], 256), [Ok(7), Ok(9), Ok(200)]);
        assert_eq!(walk(&[256], 256), [Ok(256)], "NN itself");
        assert_eq!(walk(&[], 256), [], "no active namespace");
        assert_eq!(walk(&[0, 5], 256), [], "the list ends at 0h");
        assert_eq!(walk(&[257], 256), [Err(257)], "above NN");
        assert_eq!(walk(&[1, 0xFFFF_FFFF], u32::MAX), [Ok(1), Err(0xFFFF_FFFF)]);
        assert_eq!(walk(&[0xFFFF_FFFE], u32::MAX), [Err(0xFFFF_FFFE)]);
        assert_eq!(walk(&[3, 2, 4], 256), [Ok(3), Err(2)], "not increasing");
        assert_eq!(walk(&[3, 3], 256), [Ok(3), Err(3)], "repeated");
        assert_eq!(walk(&[1], 0), [Err(1)], "NN 0: no valid NSID");
        let full: Vec<u32> = (1..=1024).collect();
        assert_eq!(
            walk(&full, 2048).len(),
            1024,
            "a full list ends after 1,024"
        );
        // The Identify that lists them.
        let cap = |iocss: bool| Capabilities::decode(u64::from(iocss) << 43 | 1 << 37);
        for (version, iocss, cns) in [
            ((1, 3, 0), false, cns::ACTIVE_NAMESPACES),
            ((1, 4, 0), false, cns::ACTIVE_NAMESPACES),
            ((1, 4, 0), true, cns::ACTIVE_NAMESPACES_OF_SET),
            ((2, 0, 0), false, cns::ACTIVE_NAMESPACES_OF_SET),
            ((2, 4, 0), false, cns::ACTIVE_NAMESPACES_OF_SET),
            ((2, 4, 0), true, cns::ACTIVE_NAMESPACES_OF_SET),
        ] {
            assert_eq!(
                active_list_cns(&cap(iocss), version),
                cns,
                "{version:?} IOCSS {iocss}"
            );
        }
    }

    /// Identify Namespace data: `blocks` blocks, the formats `formats` (each `(lbads, ms)`),
    /// formatted with `flbas`.
    fn namespace(blocks: u64, formats: &[(u32, u16)], flbas: u8) -> [u8; 4096] {
        let mut d = [0u8; 4096];
        d[0..8].copy_from_slice(&blocks.to_le_bytes());
        d[25] = (formats.len() - 1) as u8;
        d[26] = flbas;
        for (i, &(lbads, ms)) in formats.iter().enumerate() {
            let f = (lbads << 16) | u32::from(ms);
            d[128 + 4 * i..132 + 4 * i].copy_from_slice(&f.to_le_bytes());
        }
        d
    }

    /// H1: a namespace whose format in use carries metadata, or with protection information
    /// on, is refused by name; metadata in a format not in use is no matter.
    #[test]
    fn a_namespace_with_metadata_or_protection_information_is_refused() {
        let formats = [(9, 0), (9, 8), (12, 0), (12, 64)];
        assert_eq!(
            usable_namespace(&namespace(32768, &formats, 0)),
            Ok(NamespaceInfo {
                blocks: 32768,
                block_size: 512
            }),
            "metadata in formats not in use"
        );
        assert_eq!(
            usable_namespace(&namespace(32768, &formats, 2)).map(|n| n.block_size),
            Ok(4096)
        );
        // Format 1: 8 metadata bytes, in a separate buffer (FLBAS bit 4 clear) or extended.
        for flbas in [1, 1 | 1 << 4] {
            assert_eq!(
                usable_namespace(&namespace(32768, &formats, flbas)),
                Err(NamespaceRefusal::Metadata(8)),
                "FLBAS {flbas:#x}"
            );
        }
        assert_eq!(
            usable_namespace(&namespace(32768, &formats, 3 | 1 << 4)),
            Err(NamespaceRefusal::Metadata(64))
        );
        for pit in 1..=7u8 {
            let mut d = namespace(32768, &formats, 0);
            d[29] = pit;
            assert_eq!(
                usable_namespace(&d),
                Err(NamespaceRefusal::ProtectionInformation(pit)),
                "type {pit}"
            );
            // Both: named for the protection information.
            d[26] = 1;
            assert_eq!(
                usable_namespace(&d),
                Err(NamespaceRefusal::ProtectionInformation(pit))
            );
        }
        // PIP (DPS bit 3) without a type is not protection information on.
        let mut d = namespace(32768, &formats, 0);
        d[29] = 1 << 3;
        assert!(usable_namespace(&d).is_ok());
        // The names a refusal is printed with.
        let name = |r: NamespaceRefusal| alloc::format!("{r}");
        assert_eq!(
            name(NamespaceRefusal::Metadata(8)),
            "its LBA format carries 8 metadata bytes with each block, which this driver does not support"
        );
        assert_eq!(
            name(NamespaceRefusal::ProtectionInformation(1)),
            "protection information type 1 is on, which this driver does not support"
        );
        assert_eq!(
            name(NamespaceRefusal::ProtectionInformation(5)),
            "protection information of a reserved type (5) is on, which this driver does not support"
        );
        assert_eq!(identify_namespace(&namespace(8, &formats, 1)), None);
    }

    /// L4: the formats listed are NLBAF + 1 + NULBAF; FIDXU counts only above 16.
    #[test]
    fn format_indexes_count_unique_attribute_formats() {
        let plain = [(9, 0); 20];
        // One common format and two with unique attributes: index 2 is valid.
        let mut d = namespace(8, &plain[..3], 2);
        d[25] = 0;
        d[82] = 2;
        assert_eq!(usable_namespace(&d).map(|n| n.block_size), Ok(512));
        d[26] = 3;
        assert_eq!(
            usable_namespace(&d),
            Err(NamespaceRefusal::FormatIndex {
                index: 3,
                formats: 3
            })
        );
        // 16 or fewer formats: FIDXU is ignored, whatever the controller left in it.
        let mut d = namespace(8, &plain[..4], 1 | 0b11 << 5);
        assert_eq!(usable_namespace(&d).map(|n| n.block_size), Ok(512));
        d[82] = 12;
        assert_eq!(usable_namespace(&d).map(|n| n.block_size), Ok(512), "16");
        // 17 or more: FIDXU is the index's upper bits.
        d[82] = 13;
        assert_eq!(
            usable_namespace(&d),
            Err(NamespaceRefusal::FormatIndex {
                index: 1 | 0b11 << 4,
                formats: 17
            })
        );
        let mut d = namespace(8, &plain, 1 << 5);
        assert_eq!(
            usable_namespace(&d).map(|n| n.block_size),
            Ok(512),
            "index 16"
        );
        d[26] = 4 | 1 << 5;
        assert_eq!(
            usable_namespace(&d),
            Err(NamespaceRefusal::FormatIndex {
                index: 20,
                formats: 20
            })
        );
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
            // The model text lies over the namespace's DPS byte in this shared page.
            d[29] = 0;
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
            if let Ok(ns) = usable_namespace(&page) {
                assert!(
                    ns.blocks > 0 && ns.block_size.is_power_of_two(),
                    "case {case}"
                );
                assert!((512..=65536).contains(&ns.block_size), "case {case}");
                accepted += 1;
            }
            // Any page walks as a namespace list, in increasing order and within NN.
            let mut last = 0;
            for nsid in active_namespaces(&page, c.namespaces).flatten() {
                assert!(nsid > last && nsid <= c.namespaces, "case {case}");
                last = nsid;
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

    /// The fields both Identify parsers read are mutated directly (a uniform mutation of
    /// the 4 KiB page rarely lands on them), and anything accepted keeps its invariants: its
    /// format, worked out here from the rule, has no metadata, and no protection information
    /// is on.
    #[test]
    fn mutated_identify_fields_keep_their_invariants() {
        use crate::testing::{Rng, mutate_window};
        let mut rng = Rng::new(0x4944_4E54);
        let (mut namespaces, mut refused) = (0u32, 0u32);
        let mut reasons = [0u32; 6];
        for case in 0..crate::testing::cases(10_000) {
            let mut d = identify_data();
            d[0..8].copy_from_slice(&32768u64.to_le_bytes());
            d[25] = 3;
            d[26] = 1;
            d[29] = 0;
            d[128..132].copy_from_slice(&(9u32 << 16).to_le_bytes());
            d[132..136].copy_from_slice(&(12u32 << 16).to_le_bytes());
            d[136..140].copy_from_slice(&((12u32 << 16) | 8).to_le_bytes());
            d[140..144].copy_from_slice(&(16u32 << 16).to_le_bytes());
            match case % 5 {
                0 => mutate_window(&mut d, 0..32, &mut rng),
                1 => mutate_window(&mut d, 128..152, &mut rng),
                2 => mutate_window(&mut d, 0..84, &mut rng),
                3 => mutate_window(&mut d, 80..84, &mut rng),
                _ => mutate_window(&mut d, 516..520, &mut rng),
            }
            let c = identify_controller(&d);
            assert_eq!(c.vendor.to_le_bytes(), d[0..2], "case {case}");
            assert_eq!(c.namespaces.to_le_bytes(), d[516..520], "case {case}");
            for field in [&c.serial[..], &c.model[..], &c.firmware[..]] {
                let t = text(field);
                assert!(t.bytes().all(|b| (0x20..0x7F).contains(&b)), "case {case}");
                assert!(!t.ends_with(' '), "case {case}: {t:?}");
                assert!(field.starts_with(t.as_bytes()), "case {case}: {t:?}");
            }
            let ns = match usable_namespace(&d) {
                Ok(ns) => ns,
                Err(why) => {
                    refused += 1;
                    reasons[match why {
                        NamespaceRefusal::Inactive => 0,
                        NamespaceRefusal::FormatIndex { .. } => 1,
                        NamespaceRefusal::ProtectionInformation(_) => 2,
                        NamespaceRefusal::Metadata(_) => 3,
                        NamespaceRefusal::BlockSize(_) => 4,
                        NamespaceRefusal::TooLarge => 5,
                    }] += 1;
                    continue;
                }
            };
            namespaces += 1;
            let formats = usize::from(d[25]) + 1 + usize::from(d[82]);
            let flbas = d[26];
            let upper = if formats > 16 {
                usize::from((flbas >> 5) & 3) << 4
            } else {
                0
            };
            let index = usize::from(flbas & 0xF) | upper;
            assert!(index < formats && index < 64, "case {case}");
            let f = u32::from_le_bytes(d[128 + 4 * index..132 + 4 * index].try_into().unwrap());
            assert_eq!(f & 0xFFFF, 0, "case {case}: metadata accepted");
            assert_eq!(d[29] & 7, 0, "case {case}: protection information accepted");
            let lbads = (f >> 16) & 0xFF;
            assert!((9..=16).contains(&lbads), "case {case}");
            assert_eq!(ns.block_size, 1 << lbads, "case {case}");
            assert_eq!(ns.blocks.to_le_bytes(), d[0..8], "case {case}");
            assert!(ns.blocks > 0, "case {case}");
            assert!(
                ns.blocks.checked_mul(u64::from(ns.block_size)).is_some(),
                "case {case}"
            );
        }
        assert!(namespaces > 2000 && refused > 500, "{namespaces}/{refused}");
        // Each kind of refusal was reached.
        assert!(reasons.iter().all(|&n| n > 0), "{reasons:?}");
    }

    /// A namespace whose size in bytes needs more than 64 bits is refused: the driver and the
    /// block layer multiply NSZE by the block size.
    #[test]
    fn a_namespace_too_large_for_64_bits_is_refused() {
        let page = |blocks: u64, lbads: u32| namespace(blocks, &[(lbads, 0)], 0);
        // 2^55 blocks of 512 bytes is exactly 2^64 bytes: one too many.
        assert_eq!(
            usable_namespace(&page(1 << 55, 9)),
            Err(NamespaceRefusal::TooLarge)
        );
        assert_eq!(
            usable_namespace(&page((1 << 55) - 1, 9)).map(|n| n.blocks),
            Ok((1 << 55) - 1)
        );
        assert_eq!(
            usable_namespace(&page(u64::MAX, 16)),
            Err(NamespaceRefusal::TooLarge)
        );
        assert_eq!(
            usable_namespace(&page(1 << 48, 16)),
            Err(NamespaceRefusal::TooLarge)
        );
    }

    /// Controller registers and queue indexes hold together over any register or completion
    /// values, with the queue arithmetic in range after each step.
    #[test]
    fn hostile_registers_and_completions_never_break_the_queues() {
        use crate::testing::Rng;
        let mut rng = Rng::new(0x514E_564D);
        for case in 0..crate::testing::cases(20_000) {
            let v = rng.edge_u64();
            let cap = Capabilities::decode(v);
            assert!(
                (1..=65536).contains(&cap.max_entries),
                "case {case}: {v:#x}"
            );
            assert!(cap.doorbell_stride.is_power_of_two(), "case {case}: {v:#x}");
            assert!((4..=4 << 15).contains(&cap.doorbell_stride), "case {case}");
            assert!(cap.timeout_ms <= 0xFF * 500, "case {case}");
            assert!((12..=27).contains(&cap.min_page_shift), "case {case}");
            assert!((12..=27).contains(&cap.max_page_shift), "case {case}");
            assert!(cap.ready_modes <= 3, "case {case}");
            let r = ready_timeouts(&cap, rng.edge_u32());
            assert!(
                r.enable_ms >= r.disable_ms && r.disable_ms >= 500,
                "case {case}"
            );
            assert!(r.enable_ms <= 0xFFFF * 500, "case {case}");
            if let Some(n) = io_queue_entries(&cap) {
                assert!(
                    (2..=IO_QUEUE_ENTRIES).contains(&n) && u32::from(n) <= cap.max_entries,
                    "case {case}"
                );
            }
            let _ = before_reset(rng.edge_u32(), rng.edge_u32());
            let _ = version(rng.edge_u32());
            let (sq, cq) = allocated_queues(rng.edge_u32());
            assert!(
                (1..=65536).contains(&sq) && (1..=65536).contains(&cq),
                "case {case}"
            );
            let queue = rng.edge_u32() as u16;
            let at = doorbell(queue, rng.below(2) == 0, cap.doorbell_stride);
            assert!(at >= 0x1000, "case {case}");
            let bar = rng.edge_u64() >> 1;
            let fits = doorbells_fit(queue, cap.doorbell_stride, bar);
            assert_eq!(
                fits,
                doorbell(queue, true, cap.doorbell_stride) + 4 <= bar,
                "case {case}"
            );
            if let Some((p1, p2)) = prps(rng.edge_u64(), rng.edge_u64(), 1 << (12 + rng.below(4))) {
                assert!(p1.is_multiple_of(4), "case {case}");
                let _ = p2;
            }
        }
        for case in 0..crate::testing::cases(600) {
            let entries = match case % 3 {
                0 => 2,
                1 => 64,
                _ => u16::MAX,
            };
            let mut q = Queues::new(entries);
            for step in 0..200 {
                if rng.below(2) == 0
                    && let Some(slot) = q.submit()
                {
                    assert!(slot < entries, "case {case}.{step}: {slot}");
                }
                let c = Completion::decode([
                    rng.edge_u32(),
                    rng.edge_u32(),
                    rng.edge_u32(),
                    rng.edge_u32(),
                ]);
                let _ = c.status.ok();
                q.complete(&c);
                assert!(q.cq_head() < entries, "case {case}.{step}");
                assert!(q.sq_tail() < entries, "case {case}.{step}");
            }
        }
    }

    #[test]
    fn doorbells_must_fit_their_bar() {
        // QEMU's controller: stride 4, 16 KiB of registers.
        assert!(doorbells_fit(9, 4, 16 << 10));
        // A controller claiming the largest stride: the doorbells of queue 1 alone are
        // 256 KiB past the start of a 16 KiB BAR.
        assert!(!doorbells_fit(1, 4 << 15, 16 << 10));
        // Queue 1's completion doorbell is the dword at 100Ch.
        assert!(doorbells_fit(1, 4, 0x1010));
        assert!(!doorbells_fit(1, 4, 0x100F));
    }

    #[test]
    fn a_block_must_fit_one_page() {
        let ns = |block_size| NamespaceInfo {
            blocks: 8,
            block_size,
        };
        assert!(ns(512).block_fits_page(4096) && ns(4096).block_fits_page(4096));
        assert!(!ns(8192).block_fits_page(4096) && !ns(65536).block_fits_page(4096));
    }

    #[test]
    fn identify_namespace_fields() {
        let mut d = namespace(32768, &[(9, 0), (12, 0)], 1);
        assert_eq!(
            usable_namespace(&d),
            Ok(NamespaceInfo {
                blocks: 32768,
                block_size: 4096,
            })
        );
        d[26] = 0;
        assert_eq!(usable_namespace(&d).map(|n| n.block_size), Ok(512));
        d[26] = 2;
        assert_eq!(
            usable_namespace(&d),
            Err(NamespaceRefusal::FormatIndex {
                index: 2,
                formats: 2
            }),
            "beyond the formats listed"
        );
        d[26] = 0;
        d[128..132].copy_from_slice(&(8u32 << 16).to_le_bytes());
        assert_eq!(
            usable_namespace(&d),
            Err(NamespaceRefusal::BlockSize(8)),
            "256-byte blocks"
        );
        d[128..132].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            usable_namespace(&d),
            Err(NamespaceRefusal::BlockSize(0)),
            "a format not available"
        );
        d[128..132].copy_from_slice(&(9u32 << 16).to_le_bytes());
        d[0..8].copy_from_slice(&0u64.to_le_bytes());
        assert_eq!(
            usable_namespace(&d),
            Err(NamespaceRefusal::Inactive),
            "inactive"
        );
    }
}
