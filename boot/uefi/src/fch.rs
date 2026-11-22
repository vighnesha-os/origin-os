//! Bench builds only (TASKS item 121; decision item 345, ADR-0002 Amendment 4): a reset that runs
//! no firmware code, with the FCH's own record of the last reset as its control.
//!
//! On rental E the boot ended at the mark before `ExitBootServices`, with no reset, which reads
//! two ways: the call never returns, or it returned and both the console's path and the
//! firmware's reset failed after it. A reset through port `0xCF9` as the very first thing after
//! the call returns, or else a triple fault, needs neither, so a second run in the boot proves the
//! call returned.
//!
//! Before the call the bench loader reads the FCH's power-management registers, and writes none:
//! - only on an AMD processor, at the address AMD's documents give ([`PM_BLOCK`]), where the
//!   firmware's DSDT describes a system-memory region holding the registers read
//!   ([`scan_dsdt`]) and the firmware's page tables map them to themselves ([`walk`]), all checked
//!   by [`gate`];
//! - their raw values, printed, with what each bit says ([`Registers`]);
//! - a value that contradicts the documents' layout ([`Layout`]) ends the matter: no control, and
//!   the run takes Amendment 3's path after the call.
//!
//! The control ([`Branch`]): if the last reset had another documented cause, the run is the boot's
//! first. The loader writes `0x06` to `0xCF9` and makes no call, and the FCH's record of that reset
//! marks the next run, which makes the call and resets after it. A `0x06` to `0xCF9` may also be
//! someone else's (the firmware's own reset, for one), so a run of the second kind is evidence only
//! after a control run in the same boot (TASKS item 121's review, R121-1). A register that names
//! no reset cause at all holds no record: the run makes the call and resets after it, with no
//! control in that boot (the coordinating session's ruling on R121-2).
//!
//! After the call, the reset follows only a call that returned `EFI_SUCCESS`, the one thing before
//! it a compare of the status in RAX. After an error the loader's retry runs, with the same check
//! after it, and after a second error its normal path (the coordinating session's ruling on
//! R121-3).
//!
//! The registers come from AMD's FCH documents for related parts, PPR 55901 (family 19h model
//! 11h) and PPR 57238 (family 1Ah models 00h to 0Fh), which agree at every offset used here. PPR
//! 56713, this processor's own, has no FCH chapter. Read on 2026-11-21, paraphrased in
//! `docs/research/2026-11-21-rental-e-findings.md`.
//!
//! QEMU has no AMD FCH: its DSDT describes no region at [`PM_BLOCK`], so a QEMU boot reads
//! nothing. [`SWITCH_PATH`], which only QEMU's ESP may hold, gives values in place of the reads
//! ([`Pretend`]).
//!
//! This module exists only under `cfg(origin_bench)`, and in the host tests; no image a normal
//! build makes holds it (`cargo xtask check`, F1 and F2).

use core::fmt;

use origin_acpi_tables::fadt::{Block, Fadt};
use origin_acpi_tables::gas::AddressSpace;
use origin_acpi_tables::header::HEADER_LEN;
use origin_boot_info::{MemoryRegion, RegionKind};

use crate::bench::{self, NoReset, PREFIX};

/// The FCH's power-management block: its ACPI MMIO base `0xFED80000` plus `0x300` (PPR 55901,
/// the FCH's ACPI MMIO map and the PM block's description).
pub const PM_BLOCK: u64 = 0xFED8_0300;
/// `PMx00`, DECODEEN, 32 bits: bit 1 (`cf9ioen`) has port `0xCF9` decoded.
pub const DECODE_EN: u64 = 0x00;
/// `PMx08`, PCICONTROL, 32 bits: bit 20 (`shutdownoption`) makes a shutdown a PCI reset, where 0
/// sends an INIT upstream.
pub const PCI_CONTROL: u64 = 0x08;
/// `PMx60`, ACPIPM1EVTBLK, 16 bits: the PM1 event block's I/O base in bits 15:2.
pub const PM1_EVT_BLK: u64 = 0x60;
/// `PMx74`, ACPICONFIG, 32 bits: bit 17 (`en_shutdown_msg`) lets a shutdown reach the FCH.
pub const ACPI_CONFIG: u64 = 0x74;
/// `PMxC0`, S5_RESET_STATUS, 32 bits: the cause of the last reset, one bit set by each reset.
pub const RESET_STATUS: u64 = 0xC0;
/// `PMxD2`, PMIODEBUG, 8 bits: bit 6 (`cf9rstdisable`) turns the FCH's own reset at `0xCF9` off,
/// so that the firmware can trap the write (Amendment 4's addition, item 348).
pub const PMIO_DEBUG: u64 = 0xD2;
/// The bytes of the block the reads touch: from `PMx00` to `PMxD2` inclusive.
pub const SPAN: u64 = PMIO_DEBUG + 1;

/// `PMx00` bit 1.
pub const CF9_DECODED: u32 = 1 << 1;
/// `PMx08` bit 20.
pub const SHUTDOWN_PCI_RESET: u32 = 1 << 20;
/// `PMx74` bit 17.
pub const SHUTDOWN_MESSAGE: u32 = 1 << 17;
/// `PMxD2` bit 6 (`cf9rstdisable`): the FCH's own reset at `0xCF9` turned off, so that the
/// firmware can trap the write (PPR 55901, volume 5, p. 330). On rental F it was set, and `0xCF9`
/// still reset the machine through the firmware (Amendment 4's addition, item 348).
pub const CF9_RESET_STOPPED: u8 = 1 << 6;
/// `PMxC0` bit 19: the last reset was a `0x06` written to `0xCF9`.
pub const LAST_RESET_CF9_WARM: u32 = 1 << 19;
/// `PMxC0` bit 20: a `0x0E` written to `0xCF9`. Only software clears it, so a later reset leaves
/// it set beside its own bit.
pub const LAST_RESET_CF9_COLD: u32 = 1 << 20;
/// `PMx60`'s address bits.
pub const PM1_ADDRESS_MASK: u16 = 0xFFFC;
/// The reset causes of `PMxC0` that each new reset clears before it sets its own: bits 16, 18,
/// 19 and 21 to 30 but 20 (which only software clears) and 28 (which AMD's documents do not name).
/// Two of them set at once contradict the documents (TASKS item 121's review, R121-10).
pub const RESET_CAUSES: u32 = (1 << 16)
    | (1 << 18)
    | (1 << 19)
    | (1 << 21)
    | (1 << 22)
    | (1 << 23)
    | (1 << 24)
    | (1 << 25)
    | (1 << 26)
    | (1 << 27)
    | (1 << 29)
    | (1 << 30);
/// Every reset cause AMD's documents name in `PMxC0`: [`RESET_CAUSES`] and bit 20. Bits 0 to 9
/// name why the machine last shut down, not why it reset. With none of these set, the register
/// holds no record of the last reset (the coordinating session's ruling on R121-2).
pub const RESET_RECORD: u32 = RESET_CAUSES | LAST_RESET_CF9_COLD;

/// `PMxC0` bit 23: the last reset was a processor shutdown, the triple fault's record. AMD's
/// documents record it only with `PMx08` bit 20 and `PMx74` bit 17 set (PPR 55901, volume 5,
/// p. 326). The minimal mode's control writes it (ADR-0002 Amendment 5).
pub const LAST_RESET_SHUTDOWN: u32 = 1 << 23;

/// The FCH's SMI block: its ACPI MMIO base `0xFED80000` plus `0x200` (PPR 55901, volume 5, the SMI
/// registers' `SMI=FED8_0200h`).
pub const SMI_BLOCK: u64 = 0xFED8_0200;
/// `SMIxC0`, SMICONTROL8, 32 bits: bits 27:26 route a write of bit 2 to `0xCF9` to nothing, an
/// SMI, an NMI or IRQ13 (PPR 55901, volume 5, p. 119; Amendment 4's addition, item 348).
pub const SMI_CONTROL8: u64 = 0xC0;
/// The address of [`SMI_CONTROL8`].
pub const SMI_CF9_CONTROL: u64 = SMI_BLOCK + SMI_CONTROL8;
/// The bytes the `SMIxC0` read touches.
pub const SMI_SPAN: u64 = 4;
/// `SMIxC0`'s field for a write to `0xCF9`, after a shift right by [`SMI_CF9_SHIFT`].
pub const SMI_CF9_MASK: u32 = 0b11;
/// Where that field starts.
pub const SMI_CF9_SHIFT: u32 = 26;

/// The FCH's reset control port.
pub const RESET_PORT: u16 = 0xCF9;
/// Its warm-reset command (bits 3:1 = 011).
pub const WARM_RESET: u8 = 0x06;
/// How long the loader waits for a reset through `0xCF9` before it goes on.
pub const WAIT_MS: u64 = 100;
/// The wait under `slow`, so that a QEMU scenario tells from QEMU's exit time which method reset
/// the machine.
pub const SLOW_WAIT_MS: u64 = 5_000;

/// The QEMU scenarios' switch: values in place of the reads. A rented machine's ESP never holds it
/// (the rental tool refuses it).
pub const SWITCH_PATH: &str = "\\EFI\\ORIGIN\\BENCH-FCH";

/// The longest DSDT the loader scans: the rented board's is 471,613 bytes.
pub const MAX_DSDT_LENGTH: u32 = 4 << 20;
/// The bytes read at once while the DSDT is scanned.
pub const CHUNK: usize = 4096;
/// The bytes read at each `OperationRegion` found: its opcode, a name of at most four segments,
/// the space, and two constants of at most nine bytes.
pub const WINDOW: usize = 48;

/// The cause bits of `PMxC0` AMD's documents name, with what each says.
const CAUSES: [(u32, &str); 18] = [
    (0, "a thermal trip"),
    (1, "the power button held for 4 s"),
    (2, "the SHUTDOWN# pin"),
    (4, "an ASF power-off"),
    (9, "an internal thermal trip"),
    (16, "the reset pin"),
    (18, "0x04 to 0xCF9"),
    (19, "0x06 to 0xCF9"),
    (20, "0x0E to 0xCF9"),
    (21, "a sleep state"),
    (22, "KB_RST"),
    (23, "a shutdown"),
    (24, "the fail-boot timer"),
    (25, "the FCH's watchdog"),
    (26, "an ASF remote reset"),
    (27, "a sync flood"),
    (29, "the MP1 watchdog"),
    (30, "an SDP parity error"),
];

/// Where the DSDT's region holding the registers is: its base and length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    /// The region's base address.
    pub base: u64,
    /// Its length in bytes.
    pub length: u64,
}

/// Why the DSDT shows no region holding the registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoRegion {
    /// The FADT names no DSDT.
    NoDsdt,
    /// The DSDT's bytes, from this address, are not in memory the firmware's map describes.
    Unreadable(u64),
    /// The table there is not a DSDT, or is shorter than its header.
    NotDsdt,
    /// It declares more than [`MAX_DSDT_LENGTH`] bytes.
    TooLong(u32),
    /// No `OperationRegion` in system memory with constant bounds holds `[PM_BLOCK, PM_BLOCK +
    /// SPAN)`.
    NotDescribed,
}

/// What the table walk found for the FCH: the DSDT's region, and the FADT's PM1a event block when
/// it is a system I/O port, for [`Layout`]'s check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Located {
    /// The DSDT's region holding the registers.
    pub region: Result<Region, NoRegion>,
    /// The DSDT's region holding `SMIxC0` (Amendment 4's addition, item 348).
    pub smi_region: Result<Region, NoRegion>,
    /// The FADT's PM1a event block, if it is in system I/O.
    pub pm1a_event: Option<u16>,
}

/// Finds the FADT as TASKS item 116's walk does ([`bench::with_fadt`]), then its DSDT's region
/// holding the registers. Every byte is read through `read`.
pub fn locate(
    rsdp: u64,
    read: &mut impl FnMut(u64, &mut [u8]) -> bool,
) -> Result<Located, NoReset> {
    bench::with_fadt(rsdp, read, |fadt, read| Located {
        region: fadt
            .dsdt_address()
            .map_or(Err(NoRegion::NoDsdt), |at| scan_dsdt(at, read)),
        smi_region: fadt.dsdt_address().map_or(Err(NoRegion::NoDsdt), |at| {
            scan_dsdt_for(at, read, SMI_CF9_CONTROL, SMI_SPAN)
        }),
        pm1a_event: pm1a_port(fadt),
    })
}

/// The FADT's PM1a event block as a port, if it is in system I/O.
fn pm1a_port(fadt: &Fadt<'_>) -> Option<u16> {
    let block = fadt.block(Block::Pm1aEvent)?;
    (block.gas.space == AddressSpace::SystemIo)
        .then(|| u16::try_from(block.gas.address).ok())
        .flatten()
}

/// Scans the DSDT at `at` for an `OperationRegion` that [`holds`] the power-management block's
/// registers ([`scan_dsdt_for`]).
pub fn scan_dsdt(
    at: u64,
    read: &mut dyn FnMut(u64, &mut [u8]) -> bool,
) -> Result<Region, NoRegion> {
    scan_dsdt_for(at, read, PM_BLOCK, SPAN)
}

/// Scans the DSDT at `at`, `CHUNK` bytes at a time, for an `OperationRegion` that holds the `len`
/// bytes from `start` ([`holds_range`]); each found is read again in a window of its own, so one
/// that crosses two chunks is read whole.
pub fn scan_dsdt_for(
    at: u64,
    read: &mut dyn FnMut(u64, &mut [u8]) -> bool,
    start_at: u64,
    len: u64,
) -> Result<Region, NoRegion> {
    let mut header = [0u8; HEADER_LEN];
    if !read(at, &mut header) {
        return Err(NoRegion::Unreadable(at));
    }
    if header.get(..4) != Some(&b"DSDT"[..]) {
        return Err(NoRegion::NotDsdt);
    }
    let declared = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    if declared > MAX_DSDT_LENGTH {
        return Err(NoRegion::TooLong(declared));
    }
    let length = declared as usize;
    if length < HEADER_LEN {
        return Err(NoRegion::NotDsdt);
    }
    let mut chunk = [0u8; CHUNK];
    let mut start = HEADER_LEN;
    while start < length {
        let n = (length - start).min(CHUNK);
        let here = at.wrapping_add(start as u64);
        let bytes = chunk.get_mut(..n).unwrap_or(&mut []);
        if !read(here, bytes) {
            return Err(NoRegion::Unreadable(here));
        }
        for (i, &b) in bytes.iter().enumerate() {
            let opens = b == 0x5B && bytes.get(i + 1).is_none_or(|&next| next == 0x80);
            if !opens {
                continue;
            }
            let from = start + i;
            let w = (length - from).min(WINDOW);
            let mut window = [0u8; WINDOW];
            let window = window.get_mut(..w).unwrap_or(&mut []);
            if !read(at.wrapping_add(from as u64), window) {
                return Err(NoRegion::Unreadable(at.wrapping_add(from as u64)));
            }
            if let Some(region) =
                operation_region(window).filter(|r| holds_range(*r, start_at, len))
            {
                return Ok(region);
            }
        }
        start += n;
    }
    Err(NoRegion::NotDescribed)
}

/// Whether `region` holds every byte the loader reads from the power-management block.
pub fn holds(region: Region) -> bool {
    holds_range(region, PM_BLOCK, SPAN)
}

/// Whether `region` holds the `len` bytes from `start`.
pub fn holds_range(region: Region, start: u64, len: u64) -> bool {
    region.base <= start
        && region
            .base
            .checked_add(region.length)
            .zip(start.checked_add(len))
            .is_some_and(|(end, wanted)| end >= wanted)
}

/// A system-memory `OperationRegion` whose offset and length are constants, at the start of
/// `bytes` (ACPI 6.6 §20.2.5.2, `DefOpRegion`: `ExtOpPrefix 0x80`, a name, the space as a byte,
/// the offset, the length). `None` for anything else: another space, a computed bound, a name
/// the loader does not take (more than four segments), or bytes that end early.
pub fn operation_region(bytes: &[u8]) -> Option<Region> {
    if bytes.get(..2)? != [0x5B, 0x80] {
        return None;
    }
    let after_name = name_end(bytes, 2)?;
    if *bytes.get(after_name)? != 0x00 {
        return None;
    }
    let (base, next) = constant(bytes, after_name + 1)?;
    let (length, _) = constant(bytes, next)?;
    Some(Region { base, length })
}

/// The index after the `NameString` at `i` (ACPI 6.6 §20.2.2): a root or any number of parent
/// prefixes, then a null name, one segment, two (`0x2E`), or up to four (`0x2F` and a count).
fn name_end(bytes: &[u8], mut i: usize) -> Option<usize> {
    if *bytes.get(i)? == b'\\' {
        i += 1;
    } else {
        while *bytes.get(i)? == b'^' {
            i += 1;
        }
    }
    let segments = match *bytes.get(i)? {
        0x00 => return Some(i + 1),
        0x2E => {
            i += 1;
            2
        }
        0x2F => {
            let count = usize::from(*bytes.get(i + 1)?);
            if !(1..=4).contains(&count) {
                return None;
            }
            i += 2;
            count
        }
        _ => 1,
    };
    for _ in 0..segments {
        let seg = bytes.get(i..i + 4)?;
        let lead = seg[0].is_ascii_uppercase() || seg[0] == b'_';
        let rest = seg[1..]
            .iter()
            .all(|&c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_');
        if !lead || !rest {
            return None;
        }
        i += 4;
    }
    Some(i)
}

/// The constant at `i` and the index after it (ACPI 6.6 §20.2.3): `ZeroOp`, `OneOp`, `OnesOp`,
/// or a byte, word, double word or quad word prefix and its value.
fn constant(bytes: &[u8], i: usize) -> Option<(u64, usize)> {
    let value = |n: usize| -> Option<(u64, usize)> {
        let raw = bytes.get(i + 1..i + 1 + n)?;
        let mut v = [0u8; 8];
        v[..n].copy_from_slice(raw);
        Some((u64::from_le_bytes(v), i + 1 + n))
    };
    match *bytes.get(i)? {
        0x00 => Some((0, i + 1)),
        0x01 => Some((1, i + 1)),
        0xFF => Some((u64::MAX, i + 1)),
        0x0A => value(1),
        0x0B => value(2),
        0x0C => value(4),
        0x0E => value(8),
        _ => None,
    }
}

/// What the firmware's page tables say of an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mapping {
    /// A present entry maps it, to this physical address.
    Mapped(u64),
    /// The entry at this level (4 for the PML4, down to 1) is not present.
    NotPresent(u8),
    /// CR4 says five-level paging, which this walk does not follow.
    FiveLevel,
    /// A table's entry, at this address, is not in memory the firmware's map describes.
    Unreadable(u64),
}

/// A page-table entry's address bits.
const ADDRESS: u64 = 0x000F_FFFF_FFFF_F000;
/// A 2 MiB leaf's frame bits (bit 12 is its PAT bit).
const FRAME_2M: u64 = 0x000F_FFFF_FFE0_0000;
/// A 1 GiB leaf's frame bits.
const FRAME_1G: u64 = 0x000F_FFFF_C000_0000;

/// Walks the four-level page tables at `cr3` for `va`, following 1 GiB and 2 MiB pages (Intel
/// SDM Volume 3A, §4.5; AMD64 APM Volume 2, §5.3), to the physical address the leaf gives. Every
/// entry is read through `read`.
pub fn walk(
    cr3: u64,
    la57: bool,
    va: u64,
    read: &mut dyn FnMut(u64, &mut [u8]) -> bool,
) -> Mapping {
    if la57 {
        return Mapping::FiveLevel;
    }
    let mut table = cr3 & ADDRESS;
    for (level, shift) in [(4u8, 39u32), (3, 30), (2, 21), (1, 12)] {
        let at = table + ((va >> shift) & 0x1FF) * 8;
        let mut entry = [0u8; 8];
        if !read(at, &mut entry) {
            return Mapping::Unreadable(at);
        }
        let entry = u64::from_le_bytes(entry);
        if entry & 1 == 0 {
            return Mapping::NotPresent(level);
        }
        let large = entry & (1 << 7) != 0;
        match level {
            1 => return Mapping::Mapped((entry & ADDRESS) | (va & 0xFFF)),
            2 if large => return Mapping::Mapped((entry & FRAME_2M) | (va & 0x1F_FFFF)),
            3 if large => return Mapping::Mapped((entry & FRAME_1G) | (va & 0x3FFF_FFFF)),
            _ => table = entry & ADDRESS,
        }
    }
    Mapping::NotPresent(1)
}

/// Whether CPUID leaf 0's vendor is AMD's: `AuthenticAMD` across EBX, EDX and ECX.
pub const fn is_amd_vendor(ebx: u32, ecx: u32, edx: u32) -> bool {
    ebx == 0x6874_7541 && edx == 0x6974_6E65 && ecx == 0x444D_4163
}

/// D1's checks, in order, before any register is read: the processor is AMD's; the walk found
/// the FADT; its DSDT describes a region holding the registers; and the firmware's page tables map
/// the first and the last byte read to themselves (`walk_at`). The region, or why not.
pub fn gate(
    amd: bool,
    located: Result<Located, NoReset>,
    walk_at: &mut dyn FnMut(u64) -> Mapping,
) -> Result<Region, NotRead> {
    gate_for(amd, located.map(|l| l.region), PM_BLOCK, SPAN, walk_at)
}

/// The same checks for `SMIxC0` (Amendment 4's addition, item 348): an AMD processor, the FADT,
/// a DSDT region holding its four bytes, and the firmware's page tables mapping them to
/// themselves.
pub fn gate_smi(
    amd: bool,
    located: Result<Located, NoReset>,
    walk_at: &mut dyn FnMut(u64) -> Mapping,
) -> Result<Region, NotRead> {
    gate_for(
        amd,
        located.map(|l| l.smi_region),
        SMI_CF9_CONTROL,
        SMI_SPAN,
        walk_at,
    )
}

/// [`gate`]'s checks for the `len` bytes from `start`, whose region the walk found as `region`.
fn gate_for(
    amd: bool,
    region: Result<Result<Region, NoRegion>, NoReset>,
    start: u64,
    len: u64,
    walk_at: &mut dyn FnMut(u64) -> Mapping,
) -> Result<Region, NotRead> {
    if !amd {
        return Err(NotRead::NotAmd);
    }
    let region = region
        .map_err(NotRead::NoFadt)?
        .map_err(NotRead::NoRegion)?;
    for at in [start, start + len - 1] {
        match walk_at(at) {
            Mapping::Mapped(phys) if phys == at => {}
            Mapping::Mapped(phys) => return Err(NotRead::Elsewhere { at, phys }),
            other => return Err(NotRead::NotMapped(other)),
        }
    }
    Ok(region)
}

/// What the firmware's memory map says of an address: printed, not required.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapSays {
    /// No region holds it.
    Absent,
    /// A region of this kind holds it (a [`RegionKind`] value, after the loader's translation).
    In(u32),
}

/// The firmware's memory map at `at`.
pub fn map_says(regions: &[MemoryRegion], at: u64) -> MapSays {
    regions
        .iter()
        .find(|r| r.base <= at && at - r.base < r.pages.saturating_mul(4096))
        .map_or(MapSays::Absent, |r| MapSays::In(r.kind))
}

/// The registers read, or given by [`SWITCH_PATH`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Registers {
    /// `PMx00`.
    pub decode_en: u32,
    /// `PMx08`.
    pub pci_control: u32,
    /// `PMx60`.
    pub pm1_evt_blk: u16,
    /// `PMx74`.
    pub acpi_config: u32,
    /// `PMxC0`.
    pub reset_status: u32,
    /// `PMxD2`.
    pub pmio_debug: u8,
}

/// The causes `status` names, as a list for a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Causes(pub u32);

impl fmt::Display for Causes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut any = false;
        for (bit, what) in CAUSES {
            if self.0 & (1 << bit) != 0 {
                f.write_str(if any { ", " } else { "" })?;
                write!(f, "{what} (bit {bit})")?;
                any = true;
            }
        }
        if !any {
            f.write_str("no cause AMD's documents name")?;
        }
        Ok(())
    }
}

/// Whether the registers fit the documents' layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// They fit; the PM1 event block matches the FADT's, or the FADT names none in I/O to check it
    /// against (`None`).
    Fits(Option<u16>),
    /// This register reads all ones: nothing answered there.
    AllOnes(&'static str),
    /// `PMxC0` names more than one of [`RESET_CAUSES`].
    SeveralCauses(u32),
    /// `PMx60`'s address is not the FADT's PM1a event block.
    Pm1Differs {
        /// `PMx60`'s address bits.
        block: u16,
        /// The FADT's port.
        fadt: u16,
    },
}

impl Layout {
    /// Checks `r` against the documents' layout and the FADT's PM1a event block.
    pub fn check(r: &Registers, pm1a_event: Option<u16>) -> Self {
        for (name, value) in [
            ("PMx00", r.decode_en),
            ("PMx08", r.pci_control),
            ("PMx74", r.acpi_config),
            ("PMxC0", r.reset_status),
        ] {
            if value == u32::MAX {
                return Self::AllOnes(name);
            }
        }
        if (r.reset_status & RESET_CAUSES).count_ones() > 1 {
            return Self::SeveralCauses(r.reset_status);
        }
        let block = r.pm1_evt_blk & PM1_ADDRESS_MASK;
        match pm1a_event {
            Some(fadt) if fadt & PM1_ADDRESS_MASK != block => Self::Pm1Differs { block, fadt },
            Some(fadt) => Self::Fits(Some(fadt)),
            None => Self::Fits(None),
        }
    }

    /// Whether the control may run.
    pub const fn fits(self) -> bool {
        matches!(self, Self::Fits(_))
    }
}

/// Which run this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Branch {
    /// The last reset was a `0x06` to `0xCF9`, as the control writes: make the call, and reset
    /// after it.
    Mine,
    /// `PMxC0` names no reset cause ([`RESET_RECORD`]): the firmware cleared it, or it keeps no
    /// record. Make the call, and reset after it; this boot has no control (the coordinating
    /// session's ruling on R121-2). Otherwise such a firmware would make every run a control, and
    /// the call would never be made.
    NoRecord,
    /// Another reset cause: the boot's first run, the control.
    First,
}

impl Branch {
    /// The branch `PMxC0` gives.
    pub const fn of(reset_status: u32) -> Self {
        Self::of_mine(reset_status, LAST_RESET_CF9_WARM)
    }

    /// The branch `PMxC0` gives when `mine` is the record of the mode's own reset: bit 19 in the
    /// full mode, bit 23 in the minimal mode (ADR-0002 Amendment 5).
    pub const fn of_mine(reset_status: u32, mine: u32) -> Self {
        if reset_status & mine != 0 {
            Self::Mine
        } else if reset_status & RESET_RECORD == 0 {
            Self::NoRecord
        } else {
            Self::First
        }
    }
}

/// What [`SWITCH_PATH`] asks for, which only QEMU's ESP may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pretend {
    /// Which values stand in for the reads.
    pub kind: Kind,
    /// `nocf9`: no `0x06` to `0xCF9`, before the call or after it.
    pub no_cf9: bool,
    /// `noshutdown`: no triple fault after the call.
    pub no_shutdown: bool,
    /// `slow`: waits of [`SLOW_WAIT_MS`].
    pub slow: bool,
    /// `smi=<hex>`: the value that stands in for `SMIxC0` (Amendment 4's addition, item 348);
    /// [`SMI_STAND_IN`] without it.
    pub smi: Option<u32>,
}

/// `SMIxC0`'s stand-in without `smi=`: a write to `0xCF9` raises an SMI (bits 27:26 = 01).
pub const SMI_STAND_IN: u32 = 1 << SMI_CF9_SHIFT;

/// The values [`Pretend`] gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `first`: a layout that fits, and a last reset through the reset pin.
    First,
    /// `mine`: a layout that fits, and a last reset through `0x06` to `0xCF9`.
    Mine,
    /// `norecord`: a layout that fits, and `PMxC0` at its documented reset value, which names no
    /// reset cause.
    NoRecord,
    /// `bad`: every register all ones.
    Bad,
}

/// `PMxC0`'s value after a power-on reset (AMD's FCH documents): bit 11 only, no reset cause.
pub const RESET_STATUS_AT_POWER_ON: u32 = 0x800;

impl Pretend {
    /// The words of [`SWITCH_PATH`]: exactly one of `first`, `mine`, `norecord` and `bad`, and
    /// any of `nocf9`, `noshutdown`, `slow` and `smi=<hex>`, separated by whitespace. `None` for
    /// anything else.
    pub fn from_contents(contents: &[u8]) -> Option<Self> {
        let mut kind = None;
        let (mut no_cf9, mut no_shutdown, mut slow) = (false, false, false);
        let mut smi = None;
        for word in contents
            .split(u8::is_ascii_whitespace)
            .filter(|w| !w.is_empty())
        {
            if let Some(hex) = word.strip_prefix(b"smi=") {
                if smi.is_some() {
                    return None;
                }
                smi = Some(parse_hex(hex)?);
                continue;
            }
            let k = match word {
                b"first" => Some(Kind::First),
                b"mine" => Some(Kind::Mine),
                b"norecord" => Some(Kind::NoRecord),
                b"bad" => Some(Kind::Bad),
                b"nocf9" if !no_cf9 => {
                    no_cf9 = true;
                    None
                }
                b"noshutdown" if !no_shutdown => {
                    no_shutdown = true;
                    None
                }
                b"slow" if !slow => {
                    slow = true;
                    None
                }
                _ => return None,
            };
            if let Some(k) = k {
                if kind.is_some() {
                    return None;
                }
                kind = Some(k);
            }
        }
        Some(Self {
            kind: kind?,
            no_cf9,
            no_shutdown,
            slow,
            smi,
        })
    }

    /// `SMIxC0`'s stand-in value.
    pub const fn smi_value(self) -> u32 {
        match self.smi {
            Some(v) => v,
            None => SMI_STAND_IN,
        }
    }

    /// [`Pretend::values`] for the minimal mode (ADR-0002 Amendment 5): `mine` names the minimal
    /// mode's own record, a shutdown (`PMxC0` bit 23), not a `0x06` to `0xCF9`; and `noshutdown`
    /// clears the shutdown message (`PMx74` bit 17), so the decision itself takes Amendment 3's
    /// order after the call, and says so.
    pub const fn values_minimal(self, pm1a_event: Option<u16>) -> Registers {
        let mut r = self.values(pm1a_event);
        if matches!(self.kind, Kind::Mine) {
            r.reset_status = LAST_RESET_SHUTDOWN;
        }
        if self.no_shutdown && !matches!(self.kind, Kind::Bad) {
            r.acpi_config &= !SHUTDOWN_MESSAGE;
        }
        r
    }

    /// The values that stand in for the reads: the documents' layout, with `0xCF9` decoded and its
    /// reset not stopped, both shutdown bits set, and `PMx60` the FADT's PM1a event block (or the
    /// documents' reset value with none); all ones for `bad`.
    pub const fn values(self, pm1a_event: Option<u16>) -> Registers {
        let reset_status = match self.kind {
            Kind::First => 1 << 16,
            Kind::Mine => LAST_RESET_CF9_WARM,
            Kind::NoRecord => RESET_STATUS_AT_POWER_ON,
            Kind::Bad => u32::MAX,
        };
        if matches!(self.kind, Kind::Bad) {
            return Registers {
                decode_en: u32::MAX,
                pci_control: u32::MAX,
                pm1_evt_blk: u16::MAX,
                acpi_config: u32::MAX,
                reset_status,
                pmio_debug: u8::MAX,
            };
        }
        Registers {
            decode_en: 0xE302_0B10 | CF9_DECODED,
            pci_control: SHUTDOWN_PCI_RESET,
            pm1_evt_blk: match pm1a_event {
                Some(port) => port,
                None => 0,
            },
            acpi_config: 0x1000_0140 | SHUTDOWN_MESSAGE,
            reset_status,
            pmio_debug: 0,
        }
    }

    /// The wait in milliseconds.
    pub const fn wait_ms(self) -> u64 {
        if self.slow { SLOW_WAIT_MS } else { WAIT_MS }
    }

    /// The loader's line about the switch: `found` is `None` with no [`SWITCH_PATH`], `Some(None)`
    /// for contents not understood.
    pub const fn said(found: Option<Option<Self>>) -> &'static str {
        match found {
            None => {
                "no BENCH-FCH: the FCH's registers are read where the firmware's tables allow it"
            }
            Some(None) => {
                "BENCH-FCH not understood: the FCH's registers are read where the firmware's tables allow it"
            }
            Some(Some(_)) => {
                "BENCH-FCH: the FCH's registers are not read; the switch's values stand in for them"
            }
        }
    }
}

/// A 32-bit value from hexadecimal ASCII, with or without `0x`; at most eight digits.
fn parse_hex(bytes: &[u8]) -> Option<u32> {
    let digits = bytes.strip_prefix(b"0x").unwrap_or(bytes);
    if digits.is_empty() || digits.len() > 8 {
        return None;
    }
    digits.iter().try_fold(0u32, |v, &b| {
        let d = (b as char).to_digit(16)?;
        Some((v << 4) | d)
    })
}

/// What a run does after `ExitBootServices` returns `EFI_SUCCESS`, in a run [`Branch::Mine`] or
/// [`Branch::NoRecord`] sends to the call, settled before the call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Free {
    /// Whether `0x06` goes to `0xCF9` first.
    pub cf9: bool,
    /// Whether the triple fault follows the wait.
    pub shutdown: bool,
    /// The wait, in time-stamp counter ticks.
    pub wait_ticks: u64,
}

/// Why the FCH's registers were not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotRead {
    /// The processor is not AMD's.
    NotAmd,
    /// The table walk found no FADT.
    NoFadt(NoReset),
    /// The DSDT shows no region holding the registers.
    NoRegion(NoRegion),
    /// The firmware's page tables do not map them.
    NotMapped(Mapping),
    /// The firmware's page tables map `at` to another physical address (TASKS item 121's review,
    /// R121-5).
    Elsewhere {
        /// The address checked.
        at: u64,
        /// Where the tables map it.
        phys: u64,
    },
}

impl fmt::Display for NotRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NotRead::NotAmd => f.write_str("the processor is not AMD's"),
            NotRead::NoFadt(why) => write!(f, "no FADT: {why}"),
            NotRead::NoRegion(NoRegion::NoDsdt) => f.write_str("the FADT names no DSDT"),
            NotRead::NoRegion(NoRegion::Unreadable(at)) => write!(
                f,
                "the DSDT's bytes at {at:#x} are not in memory the firmware's map describes"
            ),
            NotRead::NoRegion(NoRegion::NotDsdt) => f.write_str("the FADT's DSDT is not a DSDT"),
            NotRead::NoRegion(NoRegion::TooLong(n)) => write!(
                f,
                "the DSDT declares {n} bytes, more than the loader scans ({MAX_DSDT_LENGTH})"
            ),
            NotRead::NoRegion(NoRegion::NotDescribed) => f.write_str(
                "the DSDT describes no system-memory region with constant bounds that holds it",
            ),
            NotRead::NotMapped(Mapping::NotPresent(level)) => write!(
                f,
                "the firmware's page tables do not map it (no entry at level {level})"
            ),
            NotRead::NotMapped(Mapping::FiveLevel) => {
                f.write_str("five-level paging, which the loader does not walk")
            }
            NotRead::NotMapped(Mapping::Unreadable(at)) => write!(
                f,
                "a page-table entry at {at:#x} is not in memory the firmware's map describes"
            ),
            NotRead::NotMapped(Mapping::Mapped(phys)) => write!(f, "mapped to {phys:#x}"),
            NotRead::Elsewhere { at, phys } => write!(
                f,
                "the firmware's page tables map {at:#x} to {phys:#x}, not to itself"
            ),
        }
    }
}

/// The loader's line about where the registers are, and whether they were read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhereLine {
    /// They were read: the DSDT's region, and what the firmware's memory map says.
    Read(Region, MapSays),
    /// They were not.
    NotRead(NotRead),
    /// The switch's values stand in.
    Switch,
    /// The minimal mode's stand-in table's values stand in (ADR-0002 Amendment 5; QEMU only).
    StandIn,
}

impl fmt::Display for WhereLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{PREFIX}the FCH's power-management block at {PM_BLOCK:#x} (AMD's documents; TASKS item 121): "
        )?;
        match self {
            Self::Read(region, says) => {
                write!(
                    f,
                    "an AMD processor; the DSDT describes a system-memory region at {:#x}, {} bytes, that holds it; the firmware's page tables map it to itself; the firmware's memory map: ",
                    region.base, region.length
                )?;
                match says {
                    MapSays::Absent => f.write_str("not described"),
                    MapSays::In(kind) => match RegionKind::from_raw(*kind) {
                        Some(k) => write!(f, "{k:?}"),
                        None => write!(f, "kind {kind}"),
                    },
                }
            }
            Self::NotRead(why) => write!(f, "not read, {why}"),
            Self::Switch => f.write_str("not read; BENCH-FCH's values stand in"),
            Self::StandIn => f.write_str("not read; the stand-in table's values stand in"),
        }
    }
}

/// The loader's line with the registers' raw values and what each bit says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegistersLine {
    /// The values.
    pub registers: Registers,
    /// Where they came from.
    pub source: Source,
}

/// Where the registers' values came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Read from the FCH.
    Read,
    /// Given by `BENCH-FCH`, under QEMU.
    Switch,
    /// Given by the minimal mode's stand-in table, under QEMU.
    Table,
}

impl fmt::Display for RegistersLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let r = self.registers;
        let yes = |b: bool| if b { "yes" } else { "no" };
        write!(
            f,
            "{PREFIX}the FCH's registers ({}): PMx00 {:#010x}, PMx08 {:#010x}, PMx60 {:#06x}, PMx74 {:#010x}, PMxC0 {:#010x}, PMxD2 {:#04x}; 0xCF9 decoded (PMx00 bit 1): {}; the FCH's own reset at 0xCF9 off, for the firmware to trap (PMxD2 bit 6): {}; the shutdown message (PMx74 bit 17): {}; a shutdown as a PCI reset (PMx08 bit 20): {}; the last reset: {}",
            match self.source {
                Source::Read => "read",
                Source::Switch => "BENCH-FCH's",
                Source::Table => "the stand-in table's",
            },
            r.decode_en,
            r.pci_control,
            r.pm1_evt_blk,
            r.acpi_config,
            r.reset_status,
            r.pmio_debug,
            yes(r.decode_en & CF9_DECODED != 0),
            yes(r.pmio_debug & CF9_RESET_STOPPED != 0),
            yes(r.acpi_config & SHUTDOWN_MESSAGE != 0),
            yes(r.pci_control & SHUTDOWN_PCI_RESET != 0),
            Causes(r.reset_status),
        )
    }
}

/// What the run does, and why: the loader's last FCH line before the control or the call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Nothing read: no control, and Amendment 3's path after the call.
    NotRead,
    /// The layout contradicts the documents: no control, and Amendment 3's path.
    Contradicts(Layout),
    /// The control, before the call.
    Control(u32),
    /// The call, then, once a call returns `EFI_SUCCESS`, the reset with no firmware code; with
    /// the triple fault only when the shutdown message is on (`PMx74` bit 17; TASKS item 121's
    /// review, R121-4).
    AfterCall {
        /// `PMxC0`, which names a `0x06` to `0xCF9` ([`Branch::Mine`]) or no reset cause
        /// ([`Branch::NoRecord`]).
        status: u32,
        /// Whether the triple fault follows the wait.
        shutdown: bool,
    },
}

impl Decision {
    /// What a run with `registers`, read or given, does: D3's check, then D4's branch, with the
    /// triple fault only where the shutdown message reaches the FCH (`PMx74` bit 17): a shutdown
    /// nothing acts on would hang where Amendment 3's order could still reset the machine (TASKS
    /// item 121's review, R121-4).
    pub fn of(registers: &Registers, pm1a_event: Option<u16>) -> Self {
        let layout = Layout::check(registers, pm1a_event);
        if !layout.fits() {
            return Self::Contradicts(layout);
        }
        match Branch::of(registers.reset_status) {
            Branch::First => Self::Control(registers.reset_status),
            Branch::Mine | Branch::NoRecord => Self::AfterCall {
                status: registers.reset_status,
                shutdown: registers.acpi_config & SHUTDOWN_MESSAGE != 0,
            },
        }
    }
}

/// The loader's decision line, with the switch's flags if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecisionLine {
    /// What the run does.
    pub decision: Decision,
    /// The switch, if it gave the values.
    pub pretend: Option<Pretend>,
}

/// The tail of a line where the call is made and Amendment 3's order follows it.
const AMENDMENT_3: &str = "ExitBootServices is called, and Amendment 3's path follows it";

impl fmt::Display for DecisionLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(PREFIX)?;
        match self.decision {
            Decision::NotRead => write!(f, "no FCH control: {AMENDMENT_3}"),
            Decision::Contradicts(layout) => {
                f.write_str("the FCH's registers contradict AMD's documents (")?;
                match layout {
                    Layout::AllOnes(name) => write!(f, "{name} reads all ones")?,
                    Layout::SeveralCauses(status) => {
                        write!(f, "PMxC0 {status:#010x} names more than one reset cause")?
                    }
                    Layout::Pm1Differs { block, fadt } => write!(
                        f,
                        "PMx60 names the PM1 event block at {block:#x}, the FADT at {fadt:#x}"
                    )?,
                    Layout::Fits(_) => f.write_str("they fit")?,
                }
                write!(f, "): no control; {AMENDMENT_3}")
            }
            Decision::Control(status) => write!(
                f,
                "the last reset was not a {WARM_RESET:#04x} to {RESET_PORT:#x} ({}): the boot's first run, the control: {WARM_RESET:#04x} to {RESET_PORT:#x} now, and ExitBootServices is not called",
                Causes(status)
            ),
            Decision::AfterCall { status, shutdown } => {
                if Branch::of(status) == Branch::Mine {
                    write!(
                        f,
                        "the last reset was a {WARM_RESET:#04x} to {RESET_PORT:#x} (PMxC0 bit 19), as the control writes"
                    )?;
                } else {
                    write!(
                        f,
                        "PMxC0 {status:#010x} names no reset cause: no record of the last reset, and no control in this boot"
                    )?;
                }
                write!(
                    f,
                    ": ExitBootServices is called; when a call returns EFI_SUCCESS, {WARM_RESET:#04x} to {RESET_PORT:#x} is the first thing after it, then "
                )?;
                f.write_str(if shutdown {
                    "a triple fault"
                } else {
                    "no triple fault, since the shutdown message is off (PMx74 bit 17), but Amendment 3's order"
                })
            }
        }?;
        if let Some(p) = self.pretend {
            if p.no_cf9 {
                f.write_str(" [BENCH-FCH nocf9: no write to 0xCF9]")?;
            }
            if p.no_shutdown {
                f.write_str(" [BENCH-FCH noshutdown: no triple fault]")?;
            }
            if p.slow {
                write!(f, " [BENCH-FCH slow: waits of {SLOW_WAIT_MS} ms]")?;
            }
        }
        Ok(())
    }
}

/// The control's line when the machine still runs after its wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlFailed(pub u64);

impl fmt::Display for ControlFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{PREFIX}{RESET_PORT:#x} did not reset the machine within {} ms: {AMENDMENT_3}",
            self.0
        )
    }
}

/// `SMIxC0` as a run saw it (Amendment 4's addition, item 348): read, given by a stand-in, or
/// not read and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Smi {
    /// Read behind [`gate_smi`].
    Read(u32),
    /// Given by `BENCH-FCH` or the minimal mode's stand-in table, under QEMU.
    Given(u32),
    /// Not read.
    NotRead(NotRead),
}

/// The loader's line about `SMIxC0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SmiLine(pub Smi);

impl fmt::Display for SmiLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{PREFIX}the FCH's SMI control for a write to 0xCF9 (SMIxC0 at {SMI_CF9_CONTROL:#x}, bits 27:26; ADR-0002 Amendment 4's addition): "
        )?;
        let (value, whose) = match self.0 {
            Smi::Read(v) => (v, "read"),
            Smi::Given(v) => (v, "a stand-in's"),
            Smi::NotRead(why) => return write!(f, "not read, {why}"),
        };
        let route = match (value >> SMI_CF9_SHIFT) & SMI_CF9_MASK {
            0 => "nothing: the FCH raises no event",
            1 => "an SMI, so the firmware's SMM code handles it",
            2 => "an NMI",
            _ => "IRQ13",
        };
        write!(
            f,
            "{value:#010x} ({whose}): a write of bit 2 to 0xCF9 raises {route}"
        )
    }
}

/// The stand-in table's signature: an ACPI table only `xtask` plants under QEMU, which no ACPI 6.6
/// signature (§5.2.6) takes (ADR-0002 Amendment 5; the coordinating session's ruling Q4).
pub const STAND_IN_SIGNATURE: [u8; 4] = *b"OXFC";
/// The longest stand-in table body read.
pub const STAND_IN_MAX: usize = 64;

/// The minimal mode's stand-in values under QEMU, which has no FCH: [`Pretend`]'s words, with
/// `mine` naming a shutdown (bit 23), and three more the minimal mode's QEMU scenarios need,
/// since it reads no switch file: `stale1` or `stale2` (that many calls are given a stale key),
/// and `noreset` (Amendment 3's order calls no `ResetSystem`, so the FADT's step is reached).
/// Compiled only into a minimal-mode build and the host tests, so no other build holds it (TASKS
/// item 128's review, R128-3; `bench-build` refuses a full bench loader holding the table's
/// signature).
#[cfg(any(test, origin_bench_minimal))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StandIn {
    /// The values.
    pub pretend: Pretend,
    /// Calls given a stale key: 0, 1 or 2.
    pub stale: u8,
    /// `noreset`: no `ResetSystem` in Amendment 3's order.
    pub no_reset_system: bool,
}

#[cfg(any(test, origin_bench_minimal))]
impl StandIn {
    /// The stand-in table's body; `None` for words not understood, and for `nocf9` and `slow`,
    /// which mean nothing in a mode with no `0xCF9` write and no wait of `Pretend`'s (R128-15).
    pub fn from_table(body: &[u8]) -> Option<Self> {
        let mut stale = 0u8;
        let mut no_reset_system = false;
        let mut rest: [u8; STAND_IN_MAX] = [b' '; STAND_IN_MAX];
        let mut n = 0;
        for word in body
            .split(u8::is_ascii_whitespace)
            .filter(|w| !w.is_empty())
        {
            match word {
                b"stale1" if stale == 0 => stale = 1,
                b"stale2" if stale == 0 => stale = 2,
                b"noreset" if !no_reset_system => no_reset_system = true,
                b"stale1" | b"stale2" | b"noreset" | b"nocf9" | b"slow" => return None,
                other => {
                    let end = n + other.len() + 1;
                    if end > STAND_IN_MAX {
                        return None;
                    }
                    rest[n..n + other.len()].copy_from_slice(other);
                    n = end;
                }
            }
        }
        Some(Self {
            pretend: Pretend::from_contents(&rest[..n])?,
            stale,
            no_reset_system,
        })
    }
}

/// The body of the first table the XSDT lists with [`STAND_IN_SIGNATURE`], at most
/// [`STAND_IN_MAX`] bytes, read through `read` as the FADT's walk reads (`bench::with_fadt`'s
/// RSDP and XSDT checks). `None` if there is none, or it cannot be read whole. Only a
/// minimal-mode build looks (ADR-0002 Amendment 5, step 3; the coordinating session's ruling Q4).
#[cfg(any(test, origin_bench_minimal))]
pub fn find_stand_in(
    rsdp: u64,
    read: &mut impl FnMut(u64, &mut [u8]) -> bool,
) -> Option<([u8; STAND_IN_MAX], usize)> {
    if rsdp == 0 {
        return None;
    }
    let mut bytes = [0u8; crate::spcr::RSDP_LENGTH];
    if !read(rsdp, &mut bytes) {
        return None;
    }
    let xsdt = crate::spcr::parse_rsdp(&bytes).ok()?;
    let entries = crate::spcr::check_xsdt(xsdt, read).ok()?;
    for i in 0..entries {
        let mut entry = [0u8; 8];
        if !read(
            xsdt.wrapping_add(HEADER_LEN as u64 + 8 * u64::from(i)),
            &mut entry,
        ) {
            continue;
        }
        let table = u64::from_le_bytes(entry);
        let mut header = [0u8; HEADER_LEN];
        if table == 0 || !read(table, &mut header) || header[..4] != STAND_IN_SIGNATURE {
            continue;
        }
        let declared = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let n = declared.checked_sub(HEADER_LEN)?;
        if n > STAND_IN_MAX {
            return None;
        }
        let mut body = [0u8; STAND_IN_MAX];
        if !read(table.wrapping_add(HEADER_LEN as u64), &mut body[..n]) {
            return None;
        }
        return Some((body, n));
    }
    None
}

/// The minimal mode's line about the stand-in table: `found` is `None` with no table, `Some(None)`
/// for words not understood.
#[cfg(any(test, origin_bench_minimal))]
pub const fn stand_in_said(found: Option<Option<StandIn>>) -> &'static str {
    match found {
        None => {
            "origin-boot: bench build: minimal: no stand-in table: the FCH's registers are read where the firmware's tables allow it"
        }
        Some(None) => {
            "origin-boot: bench build: minimal: the stand-in table (OXFC) is not understood: the FCH's registers are read where the firmware's tables allow it"
        }
        Some(Some(_)) => {
            "origin-boot: bench build: minimal: the stand-in table (OXFC): the FCH's registers are not read; its values stand in for them (QEMU only)"
        }
    }
}

/// Why a minimal-mode run makes the call (ADR-0002 Amendment 5, step 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallWhy {
    /// `PMxC0` names a shutdown (bit 23): the control's record, or a call's.
    Mine(u32),
    /// `PMxC0` names no reset cause: no record, and no control in this boot.
    NoRecord(u32),
    /// Another reset cause, but a shutdown bit is clear, so a triple fault could not reset: no
    /// control.
    BitsClear(u32),
    /// The layout contradicts AMD's documents: no control.
    Contradicts(Layout),
    /// The registers were not read: no control.
    NotRead,
}

/// What a minimal-mode run does (ADR-0002 Amendment 5, step 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MinimalDecision {
    /// The boot's first run, the control: a triple fault before the call. `PMxC0` named this
    /// other cause.
    Control(u32),
    /// The call; after `EFI_SUCCESS`, the triple fault where `shutdown` (both shutdown bits read
    /// before the call), else Amendment 3's order (refinement 3, which holds after the call too:
    /// the coordinating session's ruling Q3).
    Call {
        /// Why the call is made.
        why: CallWhy,
        /// Whether the triple fault follows a successful call.
        shutdown: bool,
    },
}

impl MinimalDecision {
    /// What a minimal-mode run with `registers` (read or given; `None` when not read) does.
    pub fn of(registers: Option<&Registers>, pm1a_event: Option<u16>) -> Self {
        let Some(r) = registers else {
            return Self::Call {
                why: CallWhy::NotRead,
                shutdown: false,
            };
        };
        let layout = Layout::check(r, pm1a_event);
        if !layout.fits() {
            return Self::Call {
                why: CallWhy::Contradicts(layout),
                shutdown: false,
            };
        }
        let shutdown =
            r.acpi_config & SHUTDOWN_MESSAGE != 0 && r.pci_control & SHUTDOWN_PCI_RESET != 0;
        match Branch::of_mine(r.reset_status, LAST_RESET_SHUTDOWN) {
            Branch::Mine => Self::Call {
                why: CallWhy::Mine(r.reset_status),
                shutdown,
            },
            Branch::NoRecord => Self::Call {
                why: CallWhy::NoRecord(r.reset_status),
                shutdown,
            },
            Branch::First if shutdown => Self::Control(r.reset_status),
            Branch::First => Self::Call {
                why: CallWhy::BitsClear(r.reset_status),
                shutdown: false,
            },
        }
    }
}

/// The minimal mode's decision line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MinimalDecisionLine(pub MinimalDecision);

impl fmt::Display for MinimalDecisionLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(PREFIX)?;
        f.write_str("minimal: ")?;
        let shutdown = match self.0 {
            MinimalDecision::Control(status) => {
                return write!(
                    f,
                    "the last reset was not a shutdown ({}): the boot's first run, the control: a triple fault now, and ExitBootServices is not called",
                    Causes(status)
                );
            }
            MinimalDecision::Call { why, shutdown } => {
                match why {
                    CallWhy::Mine(_) => f.write_str(
                        "the last reset was a shutdown (PMxC0 bit 23), as the control's triple fault writes",
                    )?,
                    CallWhy::NoRecord(status) => write!(
                        f,
                        "PMxC0 {status:#010x} names no reset cause: no record of the last reset, and no control in this boot"
                    )?,
                    CallWhy::BitsClear(status) => write!(
                        f,
                        "the last reset was {}, but a shutdown cannot reset this system (PMx74 bit 17 and PMx08 bit 20 are not both set): no control in this boot",
                        Causes(status)
                    )?,
                    CallWhy::Contradicts(_) => f.write_str(
                        "the FCH's registers contradict AMD's documents: no control in this boot",
                    )?,
                    CallWhy::NotRead => f.write_str(
                        "the FCH's registers were not read: no control in this boot",
                    )?,
                }
                shutdown
            }
        };
        f.write_str(": ExitBootServices is called through the live boot services table; after EFI_SUCCESS, ")?;
        f.write_str(if shutdown {
            "a triple fault and nothing before it"
        } else {
            "Amendment 3's order, since no shutdown is known to reset this system"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::format;
    use std::vec::Vec;

    /// `DefOpRegion` with `name` (already encoded), space `space`, and the two constants encoded.
    fn region(name: &[u8], space: u8, base: &[u8], length: &[u8]) -> Vec<u8> {
        let mut v = vec![0x5B, 0x80];
        v.extend_from_slice(name);
        v.push(space);
        v.extend_from_slice(base);
        v.extend_from_slice(length);
        v
    }

    const PMRG: &[u8] = b"PMRG";
    const DWORD_BLOCK: [u8; 5] = [0x0C, 0x00, 0x03, 0xD8, 0xFE];
    const WORD_256: [u8; 3] = [0x0B, 0x00, 0x01];

    #[test]
    fn the_rented_boards_region_is_read_and_holds_the_registers() {
        // As the rented DSDT has it: OperationRegion (PMRG, SystemMemory, 0xFED80300, 0x100).
        let bytes = region(PMRG, 0x00, &DWORD_BLOCK, &WORD_256);
        let r = operation_region(&bytes).unwrap();
        assert_eq!(
            r,
            Region {
                base: 0xFED8_0300,
                length: 0x100
            }
        );
        assert!(holds(r));
        // And its 4 KiB region at the ACPI MMIO base.
        let bytes = region(
            b"GSMM",
            0x00,
            &[0x0C, 0x00, 0x00, 0xD8, 0xFE],
            &[0x0B, 0x00, 0x10],
        );
        assert!(holds(operation_region(&bytes).unwrap()));
    }

    #[test]
    fn a_region_that_does_not_hold_every_register_read_is_not_taken() {
        // Too short: it ends before PMxD2.
        let short = region(PMRG, 0x00, &DWORD_BLOCK, &[0x0A, 0xD2]);
        assert!(!holds(operation_region(&short).unwrap()));
        // Starts after the block.
        let late = region(PMRG, 0x00, &[0x0C, 0x04, 0x03, 0xD8, 0xFE], &WORD_256);
        assert!(!holds(operation_region(&late).unwrap()));
        // A length that wraps.
        let wraps = Region {
            base: 0xFED8_0300,
            length: u64::MAX,
        };
        assert!(!holds(wraps));
        // Exactly the span.
        assert!(holds(Region {
            base: PM_BLOCK,
            length: SPAN
        }));
        assert!(!holds(Region {
            base: PM_BLOCK,
            length: SPAN - 1
        }));
    }

    #[test]
    fn only_system_memory_with_constant_bounds_counts() {
        // SystemIO (1) and a computed offset (a name where the constant should be).
        assert_eq!(
            operation_region(&region(PMRG, 0x01, &DWORD_BLOCK, &WORD_256)),
            None
        );
        assert_eq!(
            operation_region(&region(PMRG, 0x00, b"BASE", &WORD_256)),
            None
        );
        // Every constant form.
        for (enc, value) in [
            (&[0x00][..], 0u64),
            (&[0x01][..], 1),
            (&[0xFF][..], u64::MAX),
            (&[0x0A, 0x12][..], 0x12),
            (&[0x0B, 0x34, 0x12][..], 0x1234),
            (&[0x0C, 0x78, 0x56, 0x34, 0x12][..], 0x1234_5678),
            (
                &[0x0E, 1, 0, 0, 0, 0, 0, 0, 0x80][..],
                0x8000_0000_0000_0001,
            ),
        ] {
            let r = operation_region(&region(PMRG, 0x00, enc, &WORD_256)).unwrap();
            assert_eq!(r.base, value);
        }
    }

    #[test]
    fn names_of_every_form_are_stepped_over_and_bad_ones_refused() {
        for name in [
            &b"\\PMRG"[..],
            b"^^PMRG",
            b"\x2EAAAABBBB",
            b"\x2F\x03AAAABBBBCCCC",
            b"\x00",
            b"_PR_",
        ] {
            let r = operation_region(&region(name, 0x00, &DWORD_BLOCK, &WORD_256));
            assert_eq!(
                r,
                Some(Region {
                    base: PM_BLOCK,
                    length: 0x100
                }),
                "{name:?}"
            );
        }
        for name in [
            &b"pmrg"[..],
            b"1MRG",
            b"\x2F\x05AAAABBBBCCCCDDDDEEEE",
            b"\x2F\x00",
            b"PM-G",
        ] {
            assert_eq!(
                operation_region(&region(name, 0x00, &DWORD_BLOCK, &WORD_256)),
                None
            );
        }
        // Bytes that end early.
        let whole = region(PMRG, 0x00, &DWORD_BLOCK, &WORD_256);
        for n in 0..whole.len() {
            assert_eq!(operation_region(&whole[..n]), None, "{n}");
        }
    }

    /// A DSDT whose body is `body`, at `at`, read through a closure that refuses outside it.
    fn dsdt(body: &[u8]) -> Vec<u8> {
        let mut t = vec![0u8; HEADER_LEN];
        t[..4].copy_from_slice(b"DSDT");
        let len = (HEADER_LEN + body.len()) as u32;
        t[4..8].copy_from_slice(&len.to_le_bytes());
        t.extend_from_slice(body);
        t
    }

    fn reader(at: u64, bytes: Vec<u8>) -> impl FnMut(u64, &mut [u8]) -> bool {
        move |addr, out| {
            let Some(off) = addr.checked_sub(at) else {
                return false;
            };
            let off = off as usize;
            match bytes.get(off..off + out.len()) {
                Some(src) => {
                    out.copy_from_slice(src);
                    true
                }
                None => false,
            }
        }
    }

    #[test]
    fn the_dsdt_is_scanned_across_chunks_and_bounded() {
        let at = 0x8E3F_5000;
        let pm = region(PMRG, 0x00, &DWORD_BLOCK, &WORD_256);
        // The region anywhere, across a chunk boundary included, and after decoys.
        for pad in [
            0usize,
            1,
            CHUNK - HEADER_LEN - 1,
            CHUNK - HEADER_LEN - 3,
            3 * CHUNK,
        ] {
            let mut body = vec![0x10u8; pad];
            // A decoy: an I/O region at the same address, and a lone 0x5B.
            body.extend(region(b"DECO", 0x01, &DWORD_BLOCK, &WORD_256));
            body.push(0x5B);
            body.extend(&pm);
            body.extend([0u8; 7]);
            let mut read = reader(at, dsdt(&body));
            assert_eq!(
                scan_dsdt(at, &mut read),
                Ok(Region {
                    base: PM_BLOCK,
                    length: 0x100
                }),
                "{pad}"
            );
        }
        // Not described.
        let mut read = reader(at, dsdt(&[0x10; 100]));
        assert_eq!(scan_dsdt(at, &mut read), Err(NoRegion::NotDescribed));
        // The region at the very end, its window cut by the table's end: still read whole.
        let mut read = reader(at, dsdt(&pm));
        assert!(scan_dsdt(at, &mut read).is_ok());
        // A table that declares more than it has, one too long, one not a DSDT.
        let mut t = dsdt(&pm);
        t[4..8].copy_from_slice(&5000u32.to_le_bytes());
        let mut read = reader(at, t);
        assert!(matches!(
            scan_dsdt(at, &mut read),
            Err(NoRegion::Unreadable(_))
        ));
        let mut t = dsdt(&pm);
        t[4..8].copy_from_slice(&(MAX_DSDT_LENGTH + 1).to_le_bytes());
        let mut read = reader(at, t);
        assert_eq!(
            scan_dsdt(at, &mut read),
            Err(NoRegion::TooLong(MAX_DSDT_LENGTH + 1))
        );
        let mut t = dsdt(&pm);
        t[..4].copy_from_slice(b"SSDT");
        let mut read = reader(at, t);
        assert_eq!(scan_dsdt(at, &mut read), Err(NoRegion::NotDsdt));
        let mut t = dsdt(&[]);
        t[4..8].copy_from_slice(&10u32.to_le_bytes());
        let mut read = reader(at, t);
        assert_eq!(scan_dsdt(at, &mut read), Err(NoRegion::NotDsdt));
        let mut read = reader(at, Vec::new());
        assert_eq!(scan_dsdt(at, &mut read), Err(NoRegion::Unreadable(at)));
    }

    /// Page tables in a vector, as `(address, entry)` pairs, read through a closure.
    fn tables(entries: &[(u64, u64)]) -> impl FnMut(u64, &mut [u8]) -> bool + '_ {
        move |addr, out| match entries.iter().find(|(a, _)| *a == addr) {
            Some((_, e)) if out.len() == 8 => {
                out.copy_from_slice(&e.to_le_bytes());
                true
            }
            _ => false,
        }
    }

    #[test]
    fn the_walk_follows_each_page_size_and_stops_at_what_is_absent() {
        let va = PM_BLOCK;
        let (l4, l3, l2, l1) = (
            (va >> 39) & 0x1FF,
            (va >> 30) & 0x1FF,
            (va >> 21) & 0x1FF,
            (va >> 12) & 0x1FF,
        );
        let pml4 = 0x1000;
        let pdpt = 0x2000;
        let pd = 0x3000;
        let pt = 0x4000;
        // 4 KiB pages.
        let four = [
            (pml4 + l4 * 8, pdpt | 3),
            (pdpt + l3 * 8, pd | 3),
            (pd + l2 * 8, pt | 3),
            (pt + l1 * 8, 0xFED8_0000 | 3),
        ];
        assert_eq!(
            walk(pml4 | 0x18, false, va, &mut tables(&four)),
            Mapping::Mapped(va)
        );
        // 2 MiB and 1 GiB pages.
        let two = [
            (pml4 + l4 * 8, pdpt | 3),
            (pdpt + l3 * 8, pd | 3),
            (pd + l2 * 8, 0xFEC0_0000 | 0x83),
        ];
        assert_eq!(
            walk(pml4, false, va, &mut tables(&two)),
            Mapping::Mapped(va)
        );
        // A 2 MiB leaf's bit 12 is its PAT bit, not an address bit.
        let pat = [
            (pml4 + l4 * 8, pdpt | 3),
            (pdpt + l3 * 8, pd | 3),
            (pd + l2 * 8, 0xFEC0_1000 | 0x83),
        ];
        assert_eq!(
            walk(pml4, false, va, &mut tables(&pat)),
            Mapping::Mapped(va)
        );
        let one = [
            (pml4 + l4 * 8, pdpt | 3),
            (pdpt + l3 * 8, 0xC000_0000 | 0x83),
        ];
        assert_eq!(
            walk(pml4, false, va, &mut tables(&one)),
            Mapping::Mapped(va)
        );
        // A leaf elsewhere.
        let mut away = four;
        away[3].1 = 0x1234_5000 | 3;
        assert_eq!(
            walk(pml4, false, va, &mut tables(&away)),
            Mapping::Mapped(0x1234_5300)
        );
        // Not present at each level.
        for level in 1..=4u8 {
            let mut e = four;
            e[usize::from(4 - level)].1 &= !1;
            assert_eq!(
                walk(pml4, false, va, &mut tables(&e)),
                Mapping::NotPresent(level)
            );
        }
        // A PS bit at the PML4 is not a large page: the walk goes on through it.
        let mut e = four;
        e[0].1 |= 0x80;
        assert_eq!(walk(pml4, false, va, &mut tables(&e)), Mapping::Mapped(va));
        // Five levels, and an entry outside described memory.
        assert_eq!(walk(pml4, true, va, &mut tables(&four)), Mapping::FiveLevel);
        assert_eq!(
            walk(0x9000, false, va, &mut tables(&four)),
            Mapping::Unreadable(0x9000 + l4 * 8)
        );
    }

    #[test]
    fn the_vendor_is_amds_only_for_authenticamd() {
        assert!(is_amd_vendor(0x6874_7541, 0x444D_4163, 0x6974_6E65));
        // GenuineIntel, and AMD's words in the wrong registers.
        assert!(!is_amd_vendor(0x756E_6547, 0x6C65_746E, 0x4965_6E69));
        assert!(!is_amd_vendor(0x6874_7541, 0x6974_6E65, 0x444D_4163));
    }

    #[test]
    fn the_gate_checks_vendor_region_and_identity_in_order() {
        let region = Region {
            base: PM_BLOCK,
            length: 0x100,
        };
        let found = Ok(Located {
            region: Ok(region),
            smi_region: Err(NoRegion::NotDescribed),
            pm1a_event: Some(0x800),
        });
        let identity = &mut |at: u64| Mapping::Mapped(at);
        assert_eq!(gate(true, found, identity), Ok(region));
        assert_eq!(gate(false, found, identity), Err(NotRead::NotAmd));
        assert_eq!(
            gate(true, Err(NoReset::NoMap), identity),
            Err(NotRead::NoFadt(NoReset::NoMap))
        );
        let none = Ok(Located {
            region: Err(NoRegion::NotDescribed),
            smi_region: Err(NoRegion::NotDescribed),
            pm1a_event: None,
        });
        assert_eq!(
            gate(true, none, identity),
            Err(NotRead::NoRegion(NoRegion::NotDescribed))
        );
        assert_eq!(
            gate(true, found, &mut |_| Mapping::NotPresent(2)),
            Err(NotRead::NotMapped(Mapping::NotPresent(2)))
        );
        assert_eq!(
            gate(true, found, &mut |at| Mapping::Mapped(at + 0x1000)),
            Err(NotRead::Elsewhere {
                at: PM_BLOCK,
                phys: PM_BLOCK + 0x1000
            })
        );
        // The last byte read is checked as well as the first.
        let last = PM_BLOCK + SPAN - 1;
        let mut only_first = |at: u64| {
            if at == last {
                Mapping::NotPresent(1)
            } else {
                Mapping::Mapped(at)
            }
        };
        assert_eq!(
            gate(true, found, &mut only_first),
            Err(NotRead::NotMapped(Mapping::NotPresent(1)))
        );
    }

    fn fitting(reset_status: u32) -> Registers {
        Registers {
            decode_en: 0xE302_0B12,
            pci_control: 0,
            pm1_evt_blk: 0x0800,
            acpi_config: 0x1000_0140,
            reset_status,
            pmio_debug: 0,
        }
    }

    #[test]
    fn the_layout_is_checked_against_all_ones_and_the_fadts_pm1_block() {
        let r = fitting(1 << 16);
        assert_eq!(Layout::check(&r, Some(0x800)), Layout::Fits(Some(0x800)));
        // PMx60's low two bits are reserved.
        let low = Registers {
            pm1_evt_blk: 0x0803,
            ..r
        };
        assert_eq!(Layout::check(&low, Some(0x800)), Layout::Fits(Some(0x800)));
        assert_eq!(Layout::check(&r, None), Layout::Fits(None));
        assert_eq!(
            Layout::check(&r, Some(0x600)),
            Layout::Pm1Differs {
                block: 0x800,
                fadt: 0x600
            }
        );
        for (name, r) in [
            (
                "PMx00",
                Registers {
                    decode_en: u32::MAX,
                    ..r
                },
            ),
            (
                "PMx08",
                Registers {
                    pci_control: u32::MAX,
                    ..r
                },
            ),
            (
                "PMx74",
                Registers {
                    acpi_config: u32::MAX,
                    ..r
                },
            ),
            (
                "PMxC0",
                Registers {
                    reset_status: u32::MAX,
                    ..r
                },
            ),
        ] {
            assert_eq!(Layout::check(&r, Some(0x800)), Layout::AllOnes(name));
            assert!(!Layout::check(&r, Some(0x800)).fits());
        }
        // Two reset causes at once contradict the documents; a shut-down cause or bit 20 beside
        // one does not.
        let two = Registers {
            reset_status: (1 << 16) | (1 << 19),
            ..r
        };
        assert_eq!(
            Layout::check(&two, Some(0x800)),
            Layout::SeveralCauses((1 << 16) | (1 << 19))
        );
        for fine in [(1 << 19) | (1 << 20), (1 << 19) | 1, (1 << 16) | 0x800] {
            let r = Registers {
                reset_status: fine,
                ..r
            };
            assert!(Layout::check(&r, Some(0x800)).fits(), "{fine:#x}");
        }
        // QEMU's unassigned memory, were it read: zeros contradict the FADT's block.
        let zeros = Registers {
            decode_en: 0,
            pci_control: 0,
            pm1_evt_blk: 0,
            acpi_config: 0,
            reset_status: 0,
            pmio_debug: 0,
        };
        assert!(!Layout::check(&zeros, Some(0x600)).fits());
    }

    #[test]
    fn bit_19_marks_a_cf9_reset_another_cause_the_control_and_none_no_record() {
        assert_eq!(Branch::of(LAST_RESET_CF9_WARM), Branch::Mine);
        assert_eq!(Branch::of(LAST_RESET_CF9_WARM | 0x800), Branch::Mine);
        // Bit 20 stays set from an older 0x0E, beside the new reset's own bit.
        assert_eq!(
            Branch::of(LAST_RESET_CF9_WARM | LAST_RESET_CF9_COLD),
            Branch::Mine
        );
        for other in [1 << 16, 1 << 18, 1 << 20, 1 << 23, 1 << 25, 1 << 30] {
            assert_eq!(Branch::of(other), Branch::First, "{other:#x}");
        }
        // No reset cause: zero, the power-on value, a shutdown's causes only, and bits 28 and 31,
        // which AMD's documents do not name as reset causes (R121-2's ruling).
        for none in [
            0,
            RESET_STATUS_AT_POWER_ON,
            1,
            (1 << 4) | (1 << 9) | 0x800,
            1 << 28,
            1 << 31,
        ] {
            assert_eq!(Branch::of(none), Branch::NoRecord, "{none:#x}");
        }
        // Every bit of RESET_RECORD is one the cause table names.
        for bit in 0..32 {
            if RESET_RECORD & (1 << bit) != 0 {
                assert!(CAUSES.iter().any(|&(b, _)| b == bit), "bit {bit}");
            }
        }
    }

    #[test]
    fn the_decision_follows_the_layout_the_branch_and_the_shutdown_message() {
        assert_eq!(
            Decision::of(&fitting(1 << 16), Some(0x800)),
            Decision::Control(1 << 16)
        );
        let mine = fitting(LAST_RESET_CF9_WARM);
        assert_eq!(
            Decision::of(&mine, Some(0x800)),
            Decision::AfterCall {
                status: LAST_RESET_CF9_WARM,
                shutdown: false
            }
        );
        let on = Registers {
            acpi_config: mine.acpi_config | SHUTDOWN_MESSAGE,
            ..mine
        };
        assert_eq!(
            Decision::of(&on, Some(0x800)),
            Decision::AfterCall {
                status: LAST_RESET_CF9_WARM,
                shutdown: true
            }
        );
        // No record: the call, not the control (R121-2's ruling).
        assert_eq!(
            Decision::of(&fitting(0), Some(0x800)),
            Decision::AfterCall {
                status: 0,
                shutdown: false
            }
        );
        assert_eq!(
            Decision::of(&mine, Some(0x600)),
            Decision::Contradicts(Layout::Pm1Differs {
                block: 0x800,
                fadt: 0x600
            })
        );
        // The switch's values: the control, the reset after the call with the triple fault, and
        // a contradiction.
        for (words, want) in [
            (&b"first"[..], Decision::Control(1 << 16)),
            (
                b"mine",
                Decision::AfterCall {
                    status: LAST_RESET_CF9_WARM,
                    shutdown: true,
                },
            ),
            (
                b"norecord",
                Decision::AfterCall {
                    status: RESET_STATUS_AT_POWER_ON,
                    shutdown: true,
                },
            ),
            (b"bad", Decision::Contradicts(Layout::AllOnes("PMx00"))),
        ] {
            let p = Pretend::from_contents(words).unwrap();
            assert_eq!(Decision::of(&p.values(Some(0x600)), Some(0x600)), want);
        }
    }

    #[test]
    fn the_causes_are_named() {
        assert_eq!(
            format!("{}", Causes(0x800)),
            "no cause AMD's documents name"
        );
        assert_eq!(format!("{}", Causes(1 << 16)), "the reset pin (bit 16)");
        assert_eq!(
            format!("{}", Causes((1 << 19) | (1 << 23))),
            "0x06 to 0xCF9 (bit 19), a shutdown (bit 23)"
        );
    }

    #[test]
    fn the_switch_takes_one_kind_and_any_flags() {
        let p = Pretend::from_contents(b"mine nocf9 slow\n").unwrap();
        assert_eq!(
            (p.kind, p.no_cf9, p.no_shutdown, p.slow),
            (Kind::Mine, true, false, true)
        );
        assert_eq!(p.wait_ms(), SLOW_WAIT_MS);
        assert_eq!(Pretend::from_contents(b"first").unwrap().wait_ms(), WAIT_MS);
        for bad in [
            &b""[..],
            b"nocf9",
            b"mine first",
            b"mine mine",
            b"mine nocf9 nocf9",
            b"MINE",
            b"mine x",
            b"norecord mine",
        ] {
            assert_eq!(Pretend::from_contents(bad), None, "{bad:?}");
        }
        // Its values: a fitting layout, the branch the kind names.
        let first = Pretend::from_contents(b"first")
            .unwrap()
            .values(Some(0x600));
        assert!(Layout::check(&first, Some(0x600)).fits());
        assert_eq!(Branch::of(first.reset_status), Branch::First);
        let mine = Pretend::from_contents(b"mine").unwrap().values(Some(0x600));
        assert!(Layout::check(&mine, Some(0x600)).fits());
        assert_eq!(Branch::of(mine.reset_status), Branch::Mine);
        let none = Pretend::from_contents(b"norecord")
            .unwrap()
            .values(Some(0x600));
        assert!(Layout::check(&none, Some(0x600)).fits());
        assert_eq!(Branch::of(none.reset_status), Branch::NoRecord);
        let bad = Pretend::from_contents(b"bad").unwrap().values(Some(0x600));
        assert_eq!(Layout::check(&bad, Some(0x600)), Layout::AllOnes("PMx00"));
    }

    #[test]
    fn the_lines() {
        let line = format!(
            "{}",
            WhereLine::NotRead(NotRead::NoRegion(NoRegion::NotDescribed))
        );
        assert_eq!(
            line,
            "origin-boot: bench build: the FCH's power-management block at 0xfed80300 (AMD's documents; TASKS item 121): not read, the DSDT describes no system-memory region with constant bounds that holds it"
        );
        let line = format!(
            "{}",
            WhereLine::Read(
                Region {
                    base: PM_BLOCK,
                    length: 0x100
                },
                MapSays::Absent
            )
        );
        assert!(line.ends_with("the DSDT describes a system-memory region at 0xfed80300, 256 bytes, that holds it; the firmware's page tables map it to itself; the firmware's memory map: not described"));
        let line = format!(
            "{}",
            RegistersLine {
                registers: fitting(1 << 16),
                source: Source::Read
            }
        );
        assert_eq!(
            line,
            "origin-boot: bench build: the FCH's registers (read): PMx00 0xe3020b12, PMx08 0x00000000, PMx60 0x0800, PMx74 0x10000140, PMxC0 0x00010000, PMxD2 0x00; 0xCF9 decoded (PMx00 bit 1): yes; the FCH's own reset at 0xCF9 off, for the firmware to trap (PMxD2 bit 6): no; the shutdown message (PMx74 bit 17): no; a shutdown as a PCI reset (PMx08 bit 20): no; the last reset: the reset pin (bit 16)"
        );
        let line = format!(
            "{}",
            DecisionLine {
                decision: Decision::Control(1 << 16),
                pretend: None
            }
        );
        assert_eq!(
            line,
            "origin-boot: bench build: the last reset was not a 0x06 to 0xcf9 (the reset pin (bit 16)): the boot's first run, the control: 0x06 to 0xcf9 now, and ExitBootServices is not called"
        );
        let p = Pretend::from_contents(b"mine nocf9 noshutdown slow").unwrap();
        let line = format!(
            "{}",
            DecisionLine {
                decision: Decision::AfterCall {
                    status: LAST_RESET_CF9_WARM,
                    shutdown: true
                },
                pretend: Some(p)
            }
        );
        assert!(line.starts_with("origin-boot: bench build: the last reset was a 0x06 to 0xcf9 (PMxC0 bit 19), as the control writes: ExitBootServices is called; when a call returns EFI_SUCCESS, 0x06 to 0xcf9 is the first thing after it, then a triple fault"));
        let none = format!(
            "{}",
            DecisionLine {
                decision: Decision::AfterCall {
                    status: RESET_STATUS_AT_POWER_ON,
                    shutdown: true
                },
                pretend: None
            }
        );
        assert_eq!(
            none,
            "origin-boot: bench build: PMxC0 0x00000800 names no reset cause: no record of the last reset, and no control in this boot: ExitBootServices is called; when a call returns EFI_SUCCESS, 0x06 to 0xcf9 is the first thing after it, then a triple fault"
        );
        let off = format!(
            "{}",
            DecisionLine {
                decision: Decision::AfterCall {
                    status: LAST_RESET_CF9_WARM,
                    shutdown: false
                },
                pretend: None
            }
        );
        assert!(off.ends_with("then no triple fault, since the shutdown message is off (PMx74 bit 17), but Amendment 3's order"));
        assert!(line.ends_with(" [BENCH-FCH nocf9: no write to 0xCF9] [BENCH-FCH noshutdown: no triple fault] [BENCH-FCH slow: waits of 5000 ms]"));
        let line = format!(
            "{}",
            DecisionLine {
                decision: Decision::Contradicts(Layout::AllOnes("PMx00")),
                pretend: None
            }
        );
        assert_eq!(
            line,
            "origin-boot: bench build: the FCH's registers contradict AMD's documents (PMx00 reads all ones): no control; ExitBootServices is called, and Amendment 3's path follows it"
        );
        assert_eq!(
            format!("{}", ControlFailed(100)),
            "origin-boot: bench build: 0xcf9 did not reset the machine within 100 ms: ExitBootServices is called, and Amendment 3's path follows it"
        );
    }

    // TASKS item 128 (ADR-0002 Amendment 4's addition, item 348, and Amendment 5, item 347).

    #[test]
    fn smi_control_is_found_in_the_boards_4_kib_region_and_gated_like_the_block() {
        // Rental F's board: GSMM, 4 KiB at 0xFED80000, holds SMIxC0; PMRG (0xFED80300, 256
        // bytes) holds the PM block but not SMIxC0.
        let gsmm = Region {
            base: 0xFED8_0000,
            length: 0x1000,
        };
        let pmrg = Region {
            base: PM_BLOCK,
            length: 0x100,
        };
        assert!(holds_range(gsmm, SMI_CF9_CONTROL, SMI_SPAN));
        assert!(!holds_range(pmrg, SMI_CF9_CONTROL, SMI_SPAN));
        assert!(holds(pmrg) && holds(gsmm));
        assert!(!holds_range(
            Region {
                base: u64::MAX - 1,
                length: 2
            },
            u64::MAX - 1,
            4
        ));
        let located = Ok(Located {
            region: Ok(pmrg),
            smi_region: Ok(gsmm),
            pm1a_event: Some(0x800),
        });
        let identity = &mut |at: u64| Mapping::Mapped(at);
        assert_eq!(gate_smi(true, located, identity), Ok(gsmm));
        assert_eq!(gate_smi(false, located, identity), Err(NotRead::NotAmd));
        let mut seen = Vec::new();
        let _ = gate_smi(true, located, &mut |at| {
            seen.push(at);
            Mapping::Mapped(at)
        });
        assert_eq!(seen, [SMI_CF9_CONTROL, SMI_CF9_CONTROL + 3]);
        let without = Ok(Located {
            region: Ok(pmrg),
            smi_region: Err(NoRegion::NotDescribed),
            pm1a_event: None,
        });
        assert_eq!(
            gate_smi(true, without, identity),
            Err(NotRead::NoRegion(NoRegion::NotDescribed))
        );
        // The PM block's gate is unchanged by it.
        assert_eq!(gate(true, without, identity), Ok(pmrg));
    }

    #[test]
    fn the_dsdt_scan_finds_each_range_in_its_own_region() {
        let mut body = region(
            b"PMRG",
            0x00,
            &[0x0C, 0x00, 0x03, 0xD8, 0xFE],
            &[0x0B, 0x00, 0x01],
        );
        body.extend(region(
            b"GSMM",
            0x00,
            &[0x0C, 0x00, 0x00, 0xD8, 0xFE],
            &[0x0B, 0x00, 0x10],
        ));
        let at = 0x7000_0000;
        let mut read = reader(at, dsdt(&body));
        assert_eq!(
            scan_dsdt(at, &mut read),
            Ok(Region {
                base: PM_BLOCK,
                length: 0x100
            })
        );
        assert_eq!(
            scan_dsdt_for(at, &mut read, SMI_CF9_CONTROL, SMI_SPAN),
            Ok(Region {
                base: 0xFED8_0000,
                length: 0x1000
            })
        );
    }

    #[test]
    fn the_smi_line_names_each_route() {
        let line = |v| SmiLine(Smi::Read(v)).to_string();
        assert!(line(0).ends_with(
            "0x00000000 (read): a write of bit 2 to 0xCF9 raises nothing: the FCH raises no event"
        ));
        assert!(line(1 << 26).ends_with("raises an SMI, so the firmware's SMM code handles it"));
        assert!(line(2 << 26).ends_with("raises an NMI"));
        assert!(line(3 << 26).ends_with("raises IRQ13"));
        assert!(
            SmiLine(Smi::Given(SMI_STAND_IN))
                .to_string()
                .contains("0x04000000 (a stand-in's): a write of bit 2 to 0xCF9 raises an SMI")
        );
        assert!(
            SmiLine(Smi::NotRead(NotRead::NotAmd))
                .to_string()
                .ends_with("not read, the processor is not AMD's")
        );
        assert!(line(0).starts_with(
            "origin-boot: bench build: the FCH's SMI control for a write to 0xCF9 (SMIxC0 at 0xfed802c0, bits 27:26"
        ));
    }

    #[test]
    fn the_switch_takes_smi_once_in_hex() {
        let p = Pretend::from_contents(b"first smi=0x0c000000").unwrap();
        assert_eq!(p.smi, Some(0x0C00_0000));
        assert_eq!(p.smi_value(), 0x0C00_0000);
        assert_eq!(
            Pretend::from_contents(b"mine").unwrap().smi_value(),
            SMI_STAND_IN
        );
        assert_eq!(
            Pretend::from_contents(b"mine smi=ffffffff").unwrap().smi,
            Some(u32::MAX)
        );
        for bad in [
            &b"mine smi="[..],
            b"mine smi=0x123456789",
            b"mine smi=0xzz",
            b"mine smi=1 smi=2",
        ] {
            assert_eq!(Pretend::from_contents(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_registers_line_says_the_fchs_own_reset_is_off_not_that_it_stopped() {
        let mut r = fitting(LAST_RESET_CF9_WARM);
        r.pmio_debug = CF9_RESET_STOPPED;
        let line = RegistersLine {
            registers: r,
            source: Source::Read,
        }
        .to_string();
        assert!(
            line.contains(
                "the FCH's own reset at 0xCF9 off, for the firmware to trap (PMxD2 bit 6): yes"
            ),
            "{line}"
        );
        assert!(!line.contains("its reset stopped"), "{line}");
    }

    #[test]
    fn the_stand_in_table_takes_whole_bodies_only() {
        // That no other build takes the table is a property of the build, not of this function:
        // the stand-in code is compiled only under `origin_bench_minimal` (and here), and
        // `bench-build` refuses a full bench loader holding its signature (R128-3).
        for body in [&b"mine nocf9"[..], b"first slow"] {
            assert_eq!(StandIn::from_table(body), None, "{body:?}");
        }
        for body in [
            &b"first"[..],
            b"mine",
            b"mine stale1 noreset",
            b"norecord smi=0",
        ] {
            assert!(StandIn::from_table(body).is_some(), "{body:?}");
        }
    }

    #[test]
    fn the_stand_in_table_takes_the_switchs_words_and_its_own() {
        let s = StandIn::from_table(b"mine stale1").unwrap();
        assert_eq!(s.pretend.kind, Kind::Mine);
        assert_eq!((s.stale, s.no_reset_system), (1, false));
        let s = StandIn::from_table(b"norecord noshutdown noreset stale2 smi=0x08000000").unwrap();
        assert_eq!(s.pretend.kind, Kind::NoRecord);
        assert!(s.pretend.no_shutdown);
        assert_eq!(s.pretend.smi, Some(0x0800_0000));
        assert_eq!((s.stale, s.no_reset_system), (2, true));
        for bad in [
            &b""[..],
            b"stale1",
            b"mine stale1 stale2",
            b"mine noreset noreset",
            b"mine first",
            b"mine what",
        ] {
            assert_eq!(StandIn::from_table(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn mine_is_a_shutdown_in_the_minimal_mode_and_a_cf9_reset_in_the_full_one() {
        let p = Pretend::from_contents(b"mine").unwrap();
        assert_eq!(p.values(Some(0x800)).reset_status, LAST_RESET_CF9_WARM);
        assert_eq!(
            p.values_minimal(Some(0x800)).reset_status,
            LAST_RESET_SHUTDOWN
        );
        let first = Pretend::from_contents(b"first").unwrap();
        assert_eq!(first.values_minimal(Some(0x800)), first.values(Some(0x800)));
        let quiet = Pretend::from_contents(b"norecord noshutdown").unwrap();
        assert_eq!(
            quiet.values_minimal(Some(0x800)).acpi_config & SHUTDOWN_MESSAGE,
            0
        );
        assert_eq!(
            MinimalDecision::of(Some(&quiet.values_minimal(Some(0x800))), Some(0x800)),
            MinimalDecision::Call {
                why: CallWhy::NoRecord(RESET_STATUS_AT_POWER_ON),
                shutdown: false
            }
        );
        assert_eq!(
            Branch::of_mine(LAST_RESET_SHUTDOWN, LAST_RESET_SHUTDOWN),
            Branch::Mine
        );
        assert_eq!(Branch::of(LAST_RESET_SHUTDOWN), Branch::First);
        assert_eq!(
            Branch::of_mine(LAST_RESET_CF9_WARM, LAST_RESET_SHUTDOWN),
            Branch::First
        );
    }

    #[test]
    fn the_minimal_decision_follows_amendment_5_step_4_and_ruling_q3() {
        let both = |status| {
            let mut r = fitting(status);
            r.acpi_config |= SHUTDOWN_MESSAGE;
            r.pci_control |= SHUTDOWN_PCI_RESET;
            r
        };
        let clear = |status, bits: (bool, bool)| {
            let mut r = both(status);
            if !bits.0 {
                r.acpi_config &= !SHUTDOWN_MESSAGE;
            }
            if !bits.1 {
                r.pci_control &= !SHUTDOWN_PCI_RESET;
            }
            r
        };
        let of = |r: &Registers| MinimalDecision::of(Some(r), Some(0x800));
        // A shutdown: the call, and the triple fault after it.
        assert_eq!(
            of(&both(LAST_RESET_SHUTDOWN)),
            MinimalDecision::Call {
                why: CallWhy::Mine(LAST_RESET_SHUTDOWN),
                shutdown: true
            }
        );
        // Another cause with both bits: the control.
        assert_eq!(of(&both(1 << 21)), MinimalDecision::Control(1 << 21));
        assert_eq!(
            of(&both(LAST_RESET_CF9_WARM)),
            MinimalDecision::Control(LAST_RESET_CF9_WARM)
        );
        // Another cause with either bit clear: the call, no control, Amendment 3's order after.
        for bits in [(false, true), (true, false), (false, false)] {
            assert_eq!(
                of(&clear(1 << 21, bits)),
                MinimalDecision::Call {
                    why: CallWhy::BitsClear(1 << 21),
                    shutdown: false
                },
                "{bits:?}"
            );
        }
        // No record: the call; the triple fault only with both bits (ruling Q3).
        assert_eq!(
            of(&both(RESET_STATUS_AT_POWER_ON)),
            MinimalDecision::Call {
                why: CallWhy::NoRecord(RESET_STATUS_AT_POWER_ON),
                shutdown: true
            }
        );
        assert_eq!(
            of(&clear(RESET_STATUS_AT_POWER_ON, (true, false))),
            MinimalDecision::Call {
                why: CallWhy::NoRecord(RESET_STATUS_AT_POWER_ON),
                shutdown: false
            }
        );
        // A contradicting layout, or nothing read: the call, Amendment 3's order after.
        let bad = Pretend::from_contents(b"bad").unwrap().values(Some(0x800));
        assert!(matches!(
            of(&bad),
            MinimalDecision::Call {
                why: CallWhy::Contradicts(_),
                shutdown: false
            }
        ));
        assert_eq!(
            MinimalDecision::of(None, Some(0x800)),
            MinimalDecision::Call {
                why: CallWhy::NotRead,
                shutdown: false
            }
        );
    }

    #[test]
    fn the_minimal_lines() {
        let line = |d| MinimalDecisionLine(d).to_string();
        assert_eq!(
            line(MinimalDecision::Control(1 << 21)),
            "origin-boot: bench build: minimal: the last reset was not a shutdown (a sleep state (bit 21)): the boot's first run, the control: a triple fault now, and ExitBootServices is not called"
        );
        assert_eq!(
            line(MinimalDecision::Call {
                why: CallWhy::Mine(LAST_RESET_SHUTDOWN),
                shutdown: true
            }),
            "origin-boot: bench build: minimal: the last reset was a shutdown (PMxC0 bit 23), as the control's triple fault writes: ExitBootServices is called through the live boot services table; after EFI_SUCCESS, a triple fault and nothing before it"
        );
        assert!(line(MinimalDecision::Call {
            why: CallWhy::NotRead,
            shutdown: false
        })
        .ends_with("after EFI_SUCCESS, Amendment 3's order, since no shutdown is known to reset this system"));
    }
}
