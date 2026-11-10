//! x86-64 4-level paging (Intel SDM Vol. 3A §4.5 and Tables 4-14 to 4-20; AMD64 APM Vol. 2
//! §5.3): page-table entries and an address-space builder (CLAUDE.md §28: virtual memory,
//! page tables). Shared by the UEFI loader, which builds the kernel's first higher-half
//! tables (ADR-0004 H4), and the Framekernel.
//!
//! The walk reaches table frames through [`TableMemory`], a safe interface that returns
//! `None` for any address it does not hold, so this module needs no `unsafe` and is tested
//! on the host. [`TablePool`] implements it over a slice of tables: the kernel builds that
//! slice once from frames it reserved, and the host tests from a `Vec`. Entries are
//! [`AtomicU64`]: once a table is live the processor sets accessed and dirty bits in it
//! concurrently, so every access is atomic.
//!
//! Leaves are 4 KiB or 2 MiB pages. A walk never splits a large page; meeting one where a
//! table is needed is an error. Permissions make W^X structural: [`Access`] has no variant
//! that is both writable and executable, and every leaf that is not executable sets NX.
//! Intermediate entries are permissive (present and writable, and user when a user leaf
//! lies below), so the leaf alone decides access.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod layout;

use core::sync::atomic::{AtomicU64, Ordering};

/// Size of a 4 KiB page.
pub const PAGE: u64 = 4096;
/// Size of a 2 MiB page.
pub const LARGE_PAGE: u64 = PAGE << 9;
/// Entries per table.
pub const ENTRIES: usize = 512;

/// One page table: 512 entries in one 4 KiB frame.
pub type Table = [AtomicU64; ENTRIES];

const PRESENT: u64 = 1;
const WRITABLE: u64 = 1 << 1;
const USER: u64 = 1 << 2;
const LARGE: u64 = 1 << 7;
const GLOBAL: u64 = 1 << 8;
/// Set by the processor in any entry a walk has used; software neither needs nor keeps it.
const ACCESSED: u64 = 1 << 5;
/// Page-level write-through and cache-disable: with PAT bit 0, they select PAT entry
/// `PCD << 1 | PWT` (Intel SDM Vol. 3A §11.12.3).
const PWT: u64 = 1 << 3;
const PCD: u64 = 1 << 4;
/// The PAT bit of a 4 KiB leaf, and of a 2 MiB leaf (where bit 7 means "large").
const PAT_SMALL: u64 = 1 << 7;
const PAT_LARGE: u64 = 1 << 12;
const NO_EXECUTE: u64 = 1 << 63;
/// Physical-address bits 51:12 of an entry.
const ADDRESS: u64 = 0x000F_FFFF_FFFF_F000;
/// Physical addresses must stay below 2^52, the architectural MAXPHYADDR limit.
const PHYSICAL_LIMIT: u64 = 1 << 52;

/// What a mapping allows. There is deliberately no writable and executable variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Read only, not executable.
    Read,
    /// Read and write, not executable.
    ReadWrite,
    /// Read and execute, not writable.
    ReadExecute,
}

/// How a mapping is cached, as a PAT entry. The kernel programs the PAT so entry 0 is
/// write-back and entry 3 uncached (the architectural reset values) on every processor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cache {
    /// PAT entry 0: write-back, for RAM.
    WriteBack,
    /// PAT entry 3 (PCD and PWT): uncached, for device registers (MMIO).
    Uncached,
}

/// A leaf mapping's attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attributes {
    /// Permissions.
    pub access: Access,
    /// Accessible at CPL 3.
    pub user: bool,
    /// Kept in the TLB across CR3 loads (kernel mappings every address space shares).
    pub global: bool,
    /// Memory type.
    pub cache: Cache,
}

impl Attributes {
    /// A supervisor-only, non-global mapping.
    pub const fn kernel(access: Access) -> Self {
        Self {
            access,
            user: false,
            global: false,
            cache: Cache::WriteBack,
        }
    }

    /// A supervisor-only global mapping: for the kernel half, which every address space
    /// shares, so its translations may outlive a CR3 load (ADR-0004 H5). Never for the
    /// lower half, whose translations belong to one process.
    pub const fn kernel_global(access: Access) -> Self {
        Self {
            access,
            user: false,
            global: true,
            cache: Cache::WriteBack,
        }
    }

    /// Device registers: supervisor-only, read/write, never executable, uncached and
    /// global (the MMIO window is in the kernel half).
    pub const fn mmio() -> Self {
        Self {
            access: Access::ReadWrite,
            user: false,
            global: true,
            cache: Cache::Uncached,
        }
    }

    const fn bits(self) -> u64 {
        let access = access_bits(self.access);
        let user = if self.user { USER } else { 0 };
        let global = if self.global { GLOBAL } else { 0 };
        let cache = match self.cache {
            Cache::WriteBack => 0,
            Cache::Uncached => PCD | PWT,
        };
        PRESENT | access | user | global | cache
    }
}

/// The leaf bits a permission sets, out of [`WRITABLE`] and [`NO_EXECUTE`]: W^X lives here.
const fn access_bits(access: Access) -> u64 {
    match access {
        Access::Read => NO_EXECUTE,
        Access::ReadWrite => WRITABLE | NO_EXECUTE,
        Access::ReadExecute => 0,
    }
}

/// A leaf, as the processor would see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Translation {
    /// Physical address the virtual address translates to.
    pub phys: u64,
    /// Size of the leaf page: [`PAGE`], [`LARGE_PAGE`] or 1 GiB.
    pub size: u64,
    /// Writes are allowed.
    pub writable: bool,
    /// Instruction fetches are allowed.
    pub executable: bool,
    /// CPL 3 may access it.
    pub user: bool,
    /// Global: kept in the TLB across CR3 loads while CR4.PGE is set.
    pub global: bool,
    /// PCD and PWT both set (PAT entry 3, uncached, with the kernel's PAT).
    pub uncached: bool,
}

/// Present leaves of an address space, by half and by the global bit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeafCensus {
    /// Kernel-half leaves (PML4 entries 256–511) that are global, and that are not.
    pub upper_global: u64,
    pub upper_local: u64,
    /// Lower-half leaves that are global, and that are not.
    pub lower_global: u64,
    pub lower_local: u64,
}

/// Present leaves in part of an address space, by the size of the page each maps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeafSizes {
    /// 4 KiB leaves.
    pub small: u64,
    /// 2 MiB leaves.
    pub large: u64,
    /// 1 GiB leaves.
    pub huge: u64,
}

/// Why a mapping change was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// The address is not aligned to the page size.
    Misaligned(u64),
    /// The virtual address is not canonical (bits 63:47 are not all equal).
    NonCanonical(u64),
    /// The physical range reaches 2^52 or beyond.
    PhysicalRange(u64),
    /// Something is already mapped there (a leaf, or a table where a large page was asked).
    AlreadyMapped(u64),
    /// Nothing is mapped there.
    NotMapped(u64),
    /// A large page lies where a table is needed.
    LargePage(u64),
    /// The table memory is exhausted.
    OutOfMemory,
    /// The range wraps around the address space.
    Overflow,
    /// An entry points at a table the memory does not hold: the tables are corrupt.
    UnknownTable(u64),
}

/// Access to page-table frames by physical address.
pub trait TableMemory {
    /// A zeroed, 4 KiB-aligned frame for a new table, or `None` when memory is exhausted.
    fn allocate_table(&mut self) -> Option<u64>;

    /// The table at `addr`, or `None` if `addr` is not a table this memory allocated.
    fn table(&self, addr: u64) -> Option<&Table>;
}

/// Tables carved from one contiguous run of frames whose first frame is at physical `base`.
#[derive(Debug)]
pub struct TablePool<'a> {
    tables: &'a [Table],
    base: u64,
    used: usize,
}

impl<'a> TablePool<'a> {
    /// A pool over `tables`, the frames at `base`, `base + PAGE`, and so on.
    pub const fn new(tables: &'a [Table], base: u64) -> Self {
        Self {
            tables,
            base,
            used: 0,
        }
    }

    /// Tables handed out so far.
    pub const fn used(&self) -> usize {
        self.used
    }

    /// Tables still free.
    pub const fn remaining(&self) -> usize {
        self.tables.len() - self.used
    }
}

impl TableMemory for TablePool<'_> {
    fn allocate_table(&mut self) -> Option<u64> {
        let table = self.tables.get(self.used)?;
        for entry in table {
            entry.store(0, Ordering::Relaxed);
        }
        let addr = self.base + self.used as u64 * PAGE;
        self.used += 1;
        Some(addr)
    }

    fn table(&self, addr: u64) -> Option<&Table> {
        let offset = addr.checked_sub(self.base)?;
        if !offset.is_multiple_of(PAGE) {
            return None;
        }
        let i = usize::try_from(offset / PAGE).ok()?;
        if i >= self.used {
            return None;
        }
        self.tables.get(i)
    }
}

/// Index of `virt` in the table at `level` (0: page table, 3: PML4).
const fn index(virt: u64, level: u32) -> usize {
    ((virt >> (12 + 9 * level)) & 0x1FF) as usize
}

/// True if bits 63:47 of `virt` are all equal (48-bit, 4-level paging).
pub const fn is_canonical(virt: u64) -> bool {
    let top = (virt as i64) >> 47;
    top == 0 || top == -1
}

/// Whether two copies of PML4 entries 256–511 name the same tables with the same permissions.
/// The processor sets the accessed bit in each copy on its own, so that bit does not count.
pub fn same_kernel_half(a: &[u64; ENTRIES / 2], b: &[u64; ENTRIES / 2]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x ^ y) & !ACCESSED == 0)
}

/// Whether every one of these PML4 entries points at a table (ADR-0004 H2).
pub fn kernel_half_is_complete(entries: &[u64; ENTRIES / 2]) -> bool {
    entries.iter().all(|e| e & PRESENT != 0)
}

/// The entry for `virt` at `level` in the table at `table`.
fn slot(mem: &impl TableMemory, table: u64, virt: u64, level: u32) -> Result<&AtomicU64, MapError> {
    mem.table(table)
        .and_then(|t| t.get(index(virt, level)))
        .ok_or(MapError::UnknownTable(table))
}

/// Adds the leaves under the entry `value` at `level` (3: a PML4 entry) to `census`.
fn census_below(
    mem: &impl TableMemory,
    value: u64,
    level: u32,
    upper: bool,
    census: &mut LeafCensus,
) {
    if value & PRESENT == 0 {
        return;
    }
    let leaf = level == 0 || (level <= 2 && value & LARGE != 0);
    if leaf {
        let count = match (upper, value & GLOBAL != 0) {
            (true, true) => &mut census.upper_global,
            (true, false) => &mut census.upper_local,
            (false, true) => &mut census.lower_global,
            (false, false) => &mut census.lower_local,
        };
        *count = count.saturating_add(1);
        return;
    }
    let (Some(table), Some(below)) = (mem.table(value & ADDRESS), level.checked_sub(1)) else {
        return;
    };
    for entry in table {
        census_below(mem, entry.load(Ordering::Relaxed), below, upper, census);
    }
}

/// Adds to `sizes` the present leaves under the table at `table`, which is at `level` (3: the
/// PML4) and maps from `base`, whose pages overlap `[first, last]`.
fn sizes_below(
    mem: &impl TableMemory,
    table: u64,
    level: u32,
    base: u64,
    (first, last): (u64, u64),
    sizes: &mut LeafSizes,
) {
    let Some(entries) = mem.table(table) else {
        return;
    };
    let span = PAGE << (9 * level);
    for (i, entry) in entries.iter().enumerate() {
        let mut at = base + i as u64 * span;
        if level == 3 && i >= ENTRIES / 2 {
            // The kernel half: bits 63:48 copy bit 47.
            at |= 0xFFFF_0000_0000_0000;
        }
        // The entry's last byte: no overflow, since `at` is at most 2^64 - `span`.
        if at + (span - 1) < first || at > last {
            continue;
        }
        let value = entry.load(Ordering::Relaxed);
        if value & PRESENT == 0 {
            continue;
        }
        if level == 0 || (level <= 2 && value & LARGE != 0) {
            let count = match level {
                0 => &mut sizes.small,
                1 => &mut sizes.large,
                _ => &mut sizes.huge,
            };
            *count = count.saturating_add(1);
        } else if let Some(below) = level.checked_sub(1) {
            sizes_below(mem, value & ADDRESS, below, at, (first, last), sizes);
        }
    }
}

/// One 4-level address space, identified by the physical address of its PML4. Tables are
/// never freed when they become empty.
#[derive(Debug)]
pub struct AddressSpace {
    root: u64,
}

impl AddressSpace {
    /// An empty address space with a fresh PML4.
    pub fn new(mem: &mut impl TableMemory) -> Result<Self, MapError> {
        mem.allocate_table()
            .map(|root| Self { root })
            .ok_or(MapError::OutOfMemory)
    }

    /// Physical address of the PML4, the value for CR3.
    pub const fn root(&self) -> u64 {
        self.root
    }

    /// The raw PML4 entries 256–511: the kernel half (ADR-0004 H2).
    pub fn upper_half(&self, mem: &impl TableMemory) -> Option<[u64; ENTRIES / 2]> {
        let table = mem.table(self.root)?;
        let mut out = [0u64; ENTRIES / 2];
        for (o, e) in out.iter_mut().zip(table.iter().skip(ENTRIES / 2)) {
            *o = e.load(Ordering::Relaxed);
        }
        Some(out)
    }

    /// Installs `entries` as PML4 entries 256–511, so this address space shares the kernel
    /// half of another (whose tables this one never walks or edits: every address this
    /// space maps for itself is in the lower half).
    pub fn set_upper_half(
        &mut self,
        mem: &impl TableMemory,
        entries: &[u64; ENTRIES / 2],
    ) -> Result<(), MapError> {
        let table = mem
            .table(self.root)
            .ok_or(MapError::UnknownTable(self.root))?;
        for (e, v) in table.iter().skip(ENTRIES / 2).zip(entries) {
            e.store(*v, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Gives every PML4 entry 256–511 that has none a table of its own (ADR-0004 H2), and
    /// returns how many it made. Afterwards no mapping of the kernel half ever writes the
    /// PML4, so a copy of these entries stays right for as long as the kernel runs: a mapping
    /// made later is found through tables that every copy already points at.
    ///
    /// Only for the kernel's own address space, once, before any copy is taken. On
    /// [`MapError::OutOfMemory`] the entries already made stay, and a later call makes the rest.
    pub fn reserve_upper_half(&mut self, mem: &mut impl TableMemory) -> Result<u32, MapError> {
        let mut made = 0;
        for index in ENTRIES / 2..ENTRIES {
            let entry = mem
                .table(self.root)
                .and_then(|t| t.get(index))
                .ok_or(MapError::UnknownTable(self.root))?;
            if entry.load(Ordering::Relaxed) & PRESENT != 0 {
                continue;
            }
            let table = mem.allocate_table().ok_or(MapError::OutOfMemory)?;
            mem.table(self.root)
                .and_then(|t| t.get(index))
                .ok_or(MapError::UnknownTable(self.root))?
                .store(table | PRESENT | WRITABLE, Ordering::Relaxed);
            made += 1;
        }
        Ok(made)
    }

    /// Makes every table that 4 KiB leaves in `[start, start + bytes)` would need, down to the
    /// page tables, maps nothing, and returns how many tables it made. For a region whose leaves
    /// come and go while the kernel runs (ADR-0004 Amendment 1, A2: the kernel stacks): once this
    /// has run, a mapping there never needs a table, so it cannot fail for want of one, and its
    /// tables are counted at boot.
    ///
    /// `start` and `bytes` must be 2 MiB aligned, and the range in one half. On
    /// [`MapError::OutOfMemory`] the tables already made stay, and a later call makes the rest.
    pub fn reserve_tables(
        &mut self,
        mem: &mut impl TableMemory,
        start: u64,
        bytes: u64,
    ) -> Result<u32, MapError> {
        if !start.is_multiple_of(LARGE_PAGE) {
            return Err(MapError::Misaligned(start));
        }
        if !bytes.is_multiple_of(LARGE_PAGE) {
            return Err(MapError::Misaligned(bytes));
        }
        let end = start.checked_add(bytes).ok_or(MapError::Overflow)?;
        if let Some(last) = end.checked_sub(1).filter(|_| bytes > 0)
            && (!is_canonical(start) || !is_canonical(last) || (start >> 47) != (last >> 47))
        {
            return Err(MapError::NonCanonical(start));
        }
        self.make_tables(mem, start, end, false)
    }

    /// Makes every table that the 4 KiB **user** pages `[virt, virt + pages * PAGE)` would
    /// need, down to the page tables and with the links open to user mode as
    /// [`map_user`](Self::map_user) makes them; maps nothing, and returns how many tables it
    /// made. Once it has run, `map_user` of any page of the range needs no table, so it cannot
    /// fail for want of one: a run of pages can then be mapped all or nothing (S3-5b's review,
    /// finding B).
    ///
    /// `virt` must be page-aligned and the range in the lower half. On
    /// [`MapError::OutOfMemory`] the tables already made stay, and a later call makes the rest.
    pub fn prepare_user(
        &mut self,
        mem: &mut impl TableMemory,
        virt: u64,
        pages: u64,
    ) -> Result<u32, MapError> {
        if !virt.is_multiple_of(PAGE) {
            return Err(MapError::Misaligned(virt));
        }
        let end = pages
            .checked_mul(PAGE)
            .and_then(|bytes| virt.checked_add(bytes))
            .ok_or(MapError::Overflow)?;
        if end > 1 << 47 {
            return Err(MapError::NonCanonical(virt.max(1 << 47)));
        }
        self.make_tables(mem, virt - virt % LARGE_PAGE, end, true)
    }

    /// Makes the tables that 4 KiB leaves from `start`, 2 MiB aligned, up to `end` need, their
    /// links open to user mode if `user`, and returns how many it made.
    fn make_tables(
        &mut self,
        mem: &mut impl TableMemory,
        start: u64,
        end: u64,
        user: bool,
    ) -> Result<u32, MapError> {
        let mut made = 0u32;
        let mut at = start;
        while at < end {
            let mut table = self.root;
            for level in (1..=3).rev() {
                let present = slot(mem, table, at, level)?.load(Ordering::Relaxed) & PRESENT != 0;
                table = Self::next_table(mem, table, at, level, user)?;
                if !present {
                    made = made.saturating_add(1);
                }
            }
            at += LARGE_PAGE;
        }
        Ok(made)
    }

    /// Maps the 4 KiB **user** page at `virt` (lower half only) to `phys`.
    pub fn map_user(
        &mut self,
        mem: &mut impl TableMemory,
        virt: u64,
        phys: u64,
        access: Access,
    ) -> Result<(), MapError> {
        if virt >= 1 << 47 {
            return Err(MapError::NonCanonical(virt));
        }
        let attributes = Attributes {
            access,
            user: true,
            global: false,
            cache: Cache::WriteBack,
        };
        self.map(mem, virt, phys, attributes)
    }

    /// The table below the entry for `virt` at `level` of `table`, created if absent.
    fn next_table(
        mem: &mut impl TableMemory,
        table: u64,
        virt: u64,
        level: u32,
        user: bool,
    ) -> Result<u64, MapError> {
        let entry = slot(mem, table, virt, level)?;
        let value = entry.load(Ordering::Relaxed);
        if value & PRESENT != 0 {
            if value & LARGE != 0 {
                return Err(MapError::LargePage(virt));
            }
            if user && value & USER == 0 {
                entry.fetch_or(USER, Ordering::Relaxed);
            }
            return Ok(value & ADDRESS);
        }
        let new = mem.allocate_table().ok_or(MapError::OutOfMemory)?;
        let link = new | PRESENT | WRITABLE | if user { USER } else { 0 };
        slot(mem, table, virt, level)?.store(link, Ordering::Relaxed);
        Ok(new)
    }

    /// Maps the page of `PAGE << (9 * level)` bytes at `virt` to `phys`.
    fn map_at(
        &mut self,
        mem: &mut impl TableMemory,
        virt: u64,
        phys: u64,
        attributes: Attributes,
        level: u32,
    ) -> Result<(), MapError> {
        let size = PAGE << (9 * level);
        if !virt.is_multiple_of(size) {
            return Err(MapError::Misaligned(virt));
        }
        if !phys.is_multiple_of(size) {
            return Err(MapError::Misaligned(phys));
        }
        if !is_canonical(virt) {
            return Err(MapError::NonCanonical(virt));
        }
        if phys
            .checked_add(size)
            .is_none_or(|end| end > PHYSICAL_LIMIT)
        {
            return Err(MapError::PhysicalRange(phys));
        }
        let mut table = self.root;
        for l in (level + 1..=3).rev() {
            table = Self::next_table(mem, table, virt, l, attributes.user)?;
        }
        let leaf = slot(mem, table, virt, level)?;
        if leaf.load(Ordering::Relaxed) & PRESENT != 0 {
            return Err(MapError::AlreadyMapped(virt));
        }
        let large = if level > 0 { LARGE } else { 0 };
        leaf.store(phys | attributes.bits() | large, Ordering::Relaxed);
        Ok(())
    }

    /// Maps the 4 KiB page at `virt` to `phys`.
    pub fn map(
        &mut self,
        mem: &mut impl TableMemory,
        virt: u64,
        phys: u64,
        attributes: Attributes,
    ) -> Result<(), MapError> {
        self.map_at(mem, virt, phys, attributes, 0)
    }

    /// Maps the 2 MiB page at `virt` to `phys`.
    pub fn map_large(
        &mut self,
        mem: &mut impl TableMemory,
        virt: u64,
        phys: u64,
        attributes: Attributes,
    ) -> Result<(), MapError> {
        self.map_at(mem, virt, phys, attributes, 1)
    }

    /// Maps `bytes` (a multiple of 4 KiB) at `virt` to `phys`, using a 2 MiB page wherever
    /// both addresses are 2 MiB aligned and 2 MiB remain, and 4 KiB pages elsewhere.
    ///
    /// Returns the number of 4 KiB and of 2 MiB pages mapped. On error, every leaf this call
    /// mapped is removed again, so the range is as it was and a later call can map it; only
    /// tables created along the way stay, empty, for that later call to use. The caller must
    /// invalidate the TLB for what this removed if these tables are live.
    pub fn map_range(
        &mut self,
        mem: &mut impl TableMemory,
        virt: u64,
        phys: u64,
        bytes: u64,
        attributes: Attributes,
    ) -> Result<(u64, u64), MapError> {
        if !bytes.is_multiple_of(PAGE) {
            return Err(MapError::Misaligned(bytes));
        }
        if virt.checked_add(bytes).is_none() || phys.checked_add(bytes).is_none() {
            return Err(MapError::Overflow);
        }
        // Whether the leaf at offset `at` is a 2 MiB one, for mapping it and for undoing it.
        let is_large = |at: u64| {
            (virt + at).is_multiple_of(LARGE_PAGE)
                && (phys + at).is_multiple_of(LARGE_PAGE)
                && bytes - at >= LARGE_PAGE
        };
        let (mut small, mut large) = (0, 0);
        let mut done = 0;
        while done < bytes {
            let (v, p) = (virt + done, phys + done);
            let mapped = if is_large(done) {
                self.map_large(mem, v, p, attributes)
            } else {
                self.map(mem, v, p, attributes)
            };
            if let Err(error) = mapped {
                let mut undone = 0;
                while undone < done {
                    // Each of these is a leaf this call just mapped: nothing else can have
                    // removed it under the caller's lock.
                    let _ = self.unmap(mem, virt + undone);
                    undone += if is_large(undone) { LARGE_PAGE } else { PAGE };
                }
                return Err(error);
            }
            if is_large(done) {
                large += 1;
                done += LARGE_PAGE;
            } else {
                small += 1;
                done += PAGE;
            }
        }
        Ok((small, large))
    }

    /// The leaf entry covering `virt`, with its level, or `None` if unmapped.
    fn leaf<'m>(&self, mem: &'m impl TableMemory, virt: u64) -> Option<(&'m AtomicU64, u32)> {
        if !is_canonical(virt) {
            return None;
        }
        let mut table = self.root;
        for level in (0..=3).rev() {
            let entry = slot(mem, table, virt, level).ok()?;
            let value = entry.load(Ordering::Relaxed);
            if value & PRESENT == 0 {
                return None;
            }
            if level == 0 || (level <= 2 && value & LARGE != 0) {
                return Some((entry, level));
            }
            table = value & ADDRESS;
        }
        None
    }

    /// How the processor would translate `virt` with these tables.
    pub fn translate(&self, mem: &impl TableMemory, virt: u64) -> Option<Translation> {
        let (entry, level) = self.leaf(mem, virt)?;
        let value = entry.load(Ordering::Relaxed);
        let size = PAGE << (9 * level);
        Some(Translation {
            phys: (value & ADDRESS & !(size - 1)) | (virt & (size - 1)),
            size,
            writable: value & WRITABLE != 0,
            executable: value & NO_EXECUTE == 0,
            user: value & USER != 0,
            global: value & GLOBAL != 0,
            uncached: value & (PCD | PWT) == PCD | PWT,
        })
    }

    /// Counts every present leaf, by half and by the global bit: each half's leaves must be
    /// all global (the kernel half) or all non-global (the lower half).
    pub fn leaf_census(&self, mem: &impl TableMemory) -> LeafCensus {
        let mut census = LeafCensus::default();
        let Some(pml4) = mem.table(self.root) else {
            return census;
        };
        for (i, entry) in pml4.iter().enumerate() {
            let upper = i >= ENTRIES / 2;
            census_below(mem, entry.load(Ordering::Relaxed), 3, upper, &mut census);
        }
        census
    }

    /// Counts the present leaves whose pages overlap `[start, end)`, by size: the direct map's
    /// page sizes, which nothing may split after boot (ADR-0004 Amendment 1, A2).
    pub fn leaf_sizes(&self, mem: &impl TableMemory, start: u64, end: u64) -> LeafSizes {
        let mut sizes = LeafSizes::default();
        if let Some(last) = end.checked_sub(1).filter(|&last| last >= start) {
            sizes_below(mem, self.root, 3, 0, (start, last), &mut sizes);
        }
        sizes
    }

    /// Splits the 2 MiB leaf covering `virt` into 512 4 KiB leaves with the same physical
    /// pages and permissions, so one of them can then be changed or removed. Does nothing
    /// if `virt` is already mapped by a 4 KiB leaf. The caller must flush the TLB on every
    /// processor that may cache the old 2 MiB translation (the permissions are unchanged,
    /// so a stale entry only keeps the old granularity).
    pub fn split_large(&mut self, mem: &mut impl TableMemory, virt: u64) -> Result<(), MapError> {
        let (entry, level) = self.leaf(mem, virt).ok_or(MapError::NotMapped(virt))?;
        match level {
            0 => return Ok(()),
            1 => {}
            _ => return Err(MapError::LargePage(virt)),
        }
        let value = entry.load(Ordering::Relaxed);
        let base = value & ADDRESS & !(LARGE_PAGE - 1);
        // Everything but the address, the large bit and the large page's PAT bit, which a
        // 4 KiB leaf holds at bit 7 instead: the pieces keep the memory type too.
        let pat = if value & PAT_LARGE != 0 { PAT_SMALL } else { 0 };
        let flags = value & (PRESENT | WRITABLE | USER | GLOBAL | NO_EXECUTE | PWT | PCD) | pat;
        let table = mem.allocate_table().ok_or(MapError::OutOfMemory)?;
        let entries = mem.table(table).ok_or(MapError::UnknownTable(table))?;
        for (i, e) in entries.iter().enumerate() {
            e.store((base + i as u64 * PAGE) | flags, Ordering::Relaxed);
        }
        // Re-find the slot: `allocate_table` needed `mem` mutably.
        let (entry, _) = self.leaf(mem, virt).ok_or(MapError::NotMapped(virt))?;
        let link = table | PRESENT | WRITABLE | (flags & USER);
        entry.store(link, Ordering::Release);
        Ok(())
    }

    /// Changes what the 4 KiB leaf at `virt` allows to `access`, and returns whether that
    /// changed anything. Nothing else in the entry changes, and it is written in one atomic
    /// step, so an accessed or dirty bit the processor sets at the same moment is kept. A page
    /// covered by a 2 MiB leaf is refused: it would have to be split first. The caller must
    /// invalidate the TLB on every processor that may cache the old permissions, if these
    /// tables are live.
    pub fn protect(
        &mut self,
        mem: &mut impl TableMemory,
        virt: u64,
        access: Access,
    ) -> Result<bool, MapError> {
        if !virt.is_multiple_of(PAGE) {
            return Err(MapError::Misaligned(virt));
        }
        let (entry, level) = self.leaf(mem, virt).ok_or(MapError::NotMapped(virt))?;
        if level != 0 {
            return Err(MapError::LargePage(virt));
        }
        let bits = access_bits(access);
        let mut changed = false;
        let _ = entry.try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            let next = (value & !(WRITABLE | NO_EXECUTE)) | bits;
            changed = next != value;
            Some(next)
        });
        Ok(changed)
    }

    /// Removes the leaf at `virt` (a 4 KiB or 2 MiB page base) and returns the physical
    /// address it mapped. The caller must invalidate the TLB entry (`invlpg`) if these
    /// tables are live.
    pub fn unmap(&mut self, mem: &mut impl TableMemory, virt: u64) -> Result<u64, MapError> {
        let (entry, level) = self.leaf(mem, virt).ok_or(MapError::NotMapped(virt))?;
        let size = PAGE << (9 * level);
        if !virt.is_multiple_of(size) {
            return Err(MapError::Misaligned(virt));
        }
        let value = entry.swap(0, Ordering::Relaxed);
        Ok(value & ADDRESS & !(size - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARENA_BASE: u64 = 0x100_0000;

    fn arena(tables: usize) -> Vec<Table> {
        (0..tables)
            .map(|_| core::array::from_fn(|_| AtomicU64::new(0xDEAD_BEEF)))
            .collect()
    }

    fn raw(mem: &TablePool<'_>, space: &AddressSpace, virt: u64, level: u32) -> u64 {
        let mut table = space.root();
        for l in (level + 1..=3).rev() {
            table = slot(mem, table, virt, l).unwrap().load(Ordering::Relaxed) & ADDRESS;
        }
        slot(mem, table, virt, level)
            .unwrap()
            .load(Ordering::Relaxed)
    }

    fn kernel(access: Access) -> Attributes {
        Attributes::kernel(access)
    }

    #[test]
    fn pool_zeroes_tables_and_holds_only_what_it_handed_out() {
        let tables = arena(3);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        assert_eq!(
            mem.table(ARENA_BASE).map(|_| ()),
            None,
            "nothing allocated yet"
        );
        let a = mem.allocate_table().unwrap();
        assert_eq!(a, ARENA_BASE);
        assert!(
            mem.table(a)
                .unwrap()
                .iter()
                .all(|e| e.load(Ordering::Relaxed) == 0)
        );
        assert_eq!(mem.table(a + 8).map(|_| ()), None, "misaligned");
        assert_eq!(mem.table(a + PAGE).map(|_| ()), None, "not yet allocated");
        assert_eq!(mem.table(a - PAGE).map(|_| ()), None, "below the pool");
        assert_eq!(mem.allocate_table(), Some(ARENA_BASE + PAGE));
        assert_eq!(mem.allocate_table(), Some(ARENA_BASE + 2 * PAGE));
        assert_eq!(mem.allocate_table(), None);
        assert_eq!((mem.used(), mem.remaining()), (3, 0));
    }

    #[test]
    fn mmio_mappings_are_uncached_and_keep_it_when_split() {
        let tables = arena(16);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        let high = 0xFFFF_E000_0000_0000;
        space
            .map_large(&mut mem, high, 0xB000_0000, Attributes::mmio())
            .unwrap();
        space
            .map(
                &mut mem,
                high + 2 * LARGE_PAGE,
                0xFEBF_0000,
                Attributes::mmio(),
            )
            .unwrap();
        for v in [high, high + 2 * LARGE_PAGE] {
            let t = space.translate(&mem, v).unwrap();
            assert!(
                t.uncached && t.writable && !t.executable && t.global && !t.user,
                "{t:?}"
            );
        }
        space
            .map(
                &mut mem,
                high + 3 * LARGE_PAGE,
                0x1000,
                Attributes::kernel_global(Access::Read),
            )
            .unwrap();
        assert!(
            !space
                .translate(&mem, high + 3 * LARGE_PAGE)
                .unwrap()
                .uncached
        );
        // Splitting keeps the memory type in every 4 KiB piece.
        space.split_large(&mut mem, high).unwrap();
        for i in [0, 1, 255, 511] {
            let t = space.translate(&mem, high + i * PAGE).unwrap();
            assert!(t.uncached && t.size == PAGE, "piece {i}: {t:?}");
            assert_eq!(t.phys, 0xB000_0000 + i * PAGE);
        }
    }

    #[test]
    fn the_census_counts_leaves_by_half_and_global_bit() {
        let tables = arena(16);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        assert_eq!(space.leaf_census(&mem), LeafCensus::default());
        let high = 0xFFFF_8000_0000_0000;
        space
            .map(
                &mut mem,
                high,
                0x1000,
                Attributes::kernel_global(Access::Read),
            )
            .unwrap();
        space
            .map_large(
                &mut mem,
                high + LARGE_PAGE,
                0x20_0000,
                Attributes::kernel_global(Access::ReadWrite),
            )
            .unwrap();
        space
            .map(
                &mut mem,
                high + 3 * LARGE_PAGE,
                0x3000,
                Attributes::kernel(Access::Read),
            )
            .unwrap();
        space
            .map_user(&mut mem, 0x40_0000, 0x4000, Access::ReadExecute)
            .unwrap();
        space
            .map(
                &mut mem,
                0x50_0000,
                0x5000,
                Attributes::kernel_global(Access::Read),
            )
            .unwrap();
        assert_eq!(
            space.leaf_census(&mem),
            LeafCensus {
                upper_global: 2,
                upper_local: 1,
                lower_global: 1,
                lower_local: 1,
            }
        );
        assert!(space.translate(&mem, high).unwrap().global);
        assert!(!space.translate(&mem, 0x40_0000).unwrap().global);
        // Splitting a global large page keeps every piece global.
        space.split_large(&mut mem, high + LARGE_PAGE).unwrap();
        let census = space.leaf_census(&mem);
        assert_eq!(census.upper_global, 1 + 512);
        assert!(
            space
                .translate(&mem, high + LARGE_PAGE + 5 * PAGE)
                .unwrap()
                .global
        );
    }

    #[test]
    fn leaf_bits_make_w_xor_x_structural() {
        let tables = arena(16);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        let cases = [
            (0x1000, Access::Read, false, false),
            (0x2000, Access::ReadWrite, true, false),
            (0x3000, Access::ReadExecute, false, true),
        ];
        for (virt, access, writable, executable) in cases {
            space
                .map(&mut mem, virt, virt + 0x40_0000, kernel(access))
                .unwrap();
            let t = space.translate(&mem, virt + 0x123).unwrap();
            assert_eq!(t.phys, virt + 0x40_0123);
            assert_eq!(t.size, PAGE);
            assert_eq!(
                (t.writable, t.executable, t.user),
                (writable, executable, false)
            );
            let entry = raw(&mem, &space, virt, 0);
            assert!(
                entry & WRITABLE == 0 || entry & NO_EXECUTE != 0,
                "W and X in {entry:#x}"
            );
        }
        // One PML4, PDPT, PD and PT cover all three pages.
        assert_eq!(mem.used(), 4);
    }

    #[test]
    fn rejects_bad_addresses() {
        let tables = arena(16);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        let rw = kernel(Access::ReadWrite);
        assert_eq!(
            space.map(&mut mem, 0x1001, 0, rw),
            Err(MapError::Misaligned(0x1001))
        );
        assert_eq!(
            space.map(&mut mem, 0x1000, 0x10, rw),
            Err(MapError::Misaligned(0x10))
        );
        assert_eq!(
            space.map_large(&mut mem, 0x1000, 0, rw),
            Err(MapError::Misaligned(0x1000))
        );
        let hole = 0x0000_8000_0000_0000;
        assert_eq!(
            space.map(&mut mem, hole, 0, rw),
            Err(MapError::NonCanonical(hole))
        );
        assert_eq!(
            space.map(&mut mem, 0x1000, PHYSICAL_LIMIT, rw),
            Err(MapError::PhysicalRange(PHYSICAL_LIMIT))
        );
        assert!(
            space
                .map(&mut mem, 0xFFFF_8000_0000_0000, 0x1000, rw)
                .is_ok()
        );
        assert_eq!(space.translate(&mem, hole), None);
    }

    #[test]
    fn refuses_overlaps_and_never_splits_large_pages() {
        let tables = arena(16);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        let rw = kernel(Access::ReadWrite);
        space.map(&mut mem, 0x1000, 0x1000, rw).unwrap();
        assert_eq!(
            space.map(&mut mem, 0x1000, 0x5000, rw),
            Err(MapError::AlreadyMapped(0x1000))
        );
        // A page table already sits under the 2 MiB slot at 0.
        assert_eq!(
            space.map_large(&mut mem, 0, 0, rw),
            Err(MapError::AlreadyMapped(0))
        );
        space
            .map_large(&mut mem, LARGE_PAGE, LARGE_PAGE, rw)
            .unwrap();
        let inside = LARGE_PAGE + 0x3000;
        assert_eq!(
            space.map(&mut mem, inside, 0x3000, rw),
            Err(MapError::LargePage(inside))
        );
        let t = space.translate(&mem, inside + 5).unwrap();
        assert_eq!((t.phys, t.size), (inside + 5, LARGE_PAGE));
        assert_eq!(raw(&mem, &space, LARGE_PAGE, 1) & LARGE, LARGE);
    }

    #[test]
    fn map_range_uses_large_pages_where_aligned() {
        let tables = arena(16);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        let start = LARGE_PAGE - PAGE;
        let bytes = PAGE + 2 * LARGE_PAGE + 2 * PAGE;
        let counts = space
            .map_range(&mut mem, start, start, bytes, kernel(Access::Read))
            .unwrap();
        assert_eq!(counts, (3, 2));
        for (virt, size) in [
            (start, PAGE),
            (LARGE_PAGE, LARGE_PAGE),
            (2 * LARGE_PAGE + 0x1234, LARGE_PAGE),
            (3 * LARGE_PAGE, PAGE),
            (3 * LARGE_PAGE + PAGE, PAGE),
        ] {
            let t = space.translate(&mem, virt).unwrap();
            assert_eq!((t.phys, t.size), (virt, size), "at {virt:#x}");
        }
        assert_eq!(space.translate(&mem, start + bytes), None);
        assert_eq!(space.translate(&mem, start - PAGE), None);
        // Physical addresses that are not 2 MiB aligned force 4 KiB pages.
        let counts = space
            .map_range(
                &mut mem,
                0x4000_0000,
                0x4000_1000,
                LARGE_PAGE,
                kernel(Access::Read),
            )
            .unwrap();
        assert_eq!(counts, (512, 0));
        assert_eq!(
            space.map_range(&mut mem, 0x1000, 0, 0x800, kernel(Access::Read)),
            Err(MapError::Misaligned(0x800))
        );
        assert_eq!(
            space.map_range(
                &mut mem,
                u64::MAX - 0xFFF,
                0,
                PAGE * 2,
                kernel(Access::Read)
            ),
            Err(MapError::Overflow)
        );
    }

    /// A range that fails partway leaves no leaf behind, keeps whatever it ran into, and can
    /// be mapped again afterwards: partial leaves would make every later mapping of the same
    /// addresses fail with `AlreadyMapped`.
    #[test]
    fn map_range_undoes_itself_when_it_fails() {
        let rw = kernel(Access::ReadWrite);

        // The pool runs out: the root, a PDPT and two page directories cover 2 GiB of large
        // pages; the third directory is not there.
        let tables = arena(4);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        let (gib, base) = (1u64 << 30, 0x10_0000_0000u64);
        assert_eq!(
            space.map_range(&mut mem, 0, base, 3 * gib, rw),
            Err(MapError::OutOfMemory)
        );
        let census = space.leaf_census(&mem);
        assert_eq!(census.lower_local, 0, "leaves left behind");
        assert_eq!(space.translate(&mem, 0), None);
        assert_eq!(space.translate(&mem, 2 * gib - PAGE), None);
        // The tables it made stay, empty, and the failed range can be retried at once.
        assert_eq!(mem.remaining(), 0);
        assert_eq!(
            space.map_range(&mut mem, 0, base, 2 * gib, rw),
            Ok((0, 1024))
        );

        // A leaf already there stops the range: what the range mapped before it goes, the
        // leaf itself stays, and pages after it were never touched.
        let tables = arena(16);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        let v = 4 * LARGE_PAGE;
        space.map(&mut mem, v + 2 * PAGE, 0x7000, rw).unwrap();
        assert_eq!(
            space.map_range(&mut mem, v, 0x20_0000, 4 * PAGE, rw),
            Err(MapError::AlreadyMapped(v + 2 * PAGE))
        );
        assert_eq!(space.translate(&mem, v), None);
        assert_eq!(space.translate(&mem, v + PAGE), None);
        assert_eq!(space.translate(&mem, v + 2 * PAGE).unwrap().phys, 0x7000);
        assert_eq!(space.translate(&mem, v + 3 * PAGE), None);
        assert_eq!(space.leaf_census(&mem).lower_local, 1);

        // Both kinds of leaf are undone: a 4 KiB head, a 2 MiB page, then a collision.
        let start = 8 * LARGE_PAGE - PAGE;
        space.map(&mut mem, 9 * LARGE_PAGE, 0x8000, rw).unwrap();
        let bytes = PAGE + LARGE_PAGE + PAGE;
        assert_eq!(
            space.map_range(&mut mem, start, start, bytes, rw),
            Err(MapError::AlreadyMapped(9 * LARGE_PAGE))
        );
        assert_eq!(space.translate(&mem, start), None);
        assert_eq!(space.translate(&mem, 8 * LARGE_PAGE), None);
        assert_eq!(space.translate(&mem, 9 * LARGE_PAGE).unwrap().phys, 0x8000);
        assert_eq!(space.leaf_census(&mem).lower_local, 2);

        // A failure partway through the physical range is undone the same way.
        assert_eq!(
            space.map_range(&mut mem, v + PAGE, (1 << 52) - PAGE, 2 * PAGE, rw),
            Err(MapError::PhysicalRange(1 << 52))
        );
        assert_eq!(space.translate(&mem, v + PAGE), None);
        assert_eq!(space.leaf_census(&mem).lower_local, 2);
    }

    #[test]
    fn user_leaves_open_their_path_and_kernel_leaves_do_not() {
        let tables = arena(16);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        space
            .map(&mut mem, 0x1000, 0x1000, kernel(Access::ReadWrite))
            .unwrap();
        for level in 1..=3 {
            assert_eq!(raw(&mem, &space, 0x1000, level) & USER, 0);
        }
        let user = Attributes {
            access: Access::Read,
            user: true,
            global: false,
            cache: Cache::WriteBack,
        };
        space.map(&mut mem, 0x2000, 0x2000, user).unwrap();
        for level in 1..=3 {
            assert_eq!(raw(&mem, &space, 0x2000, level) & USER, USER);
        }
        assert!(!space.translate(&mem, 0x1000).unwrap().user);
        assert!(space.translate(&mem, 0x2000).unwrap().user);
    }

    #[test]
    fn address_spaces_share_the_kernel_half_and_keep_their_lower_halves() {
        let tables = arena(16);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut kspace = AddressSpace::new(&mut mem).unwrap();
        let high = 0xFFFF_FFFF_8000_0000;
        kspace
            .map(&mut mem, high, 0x20_0000, kernel(Access::ReadExecute))
            .unwrap();
        let half = kspace.upper_half(&mem).unwrap();
        let mut a = AddressSpace::new(&mut mem).unwrap();
        let mut b = AddressSpace::new(&mut mem).unwrap();
        a.set_upper_half(&mem, &half).unwrap();
        b.set_upper_half(&mem, &half).unwrap();
        // The kernel's mapping is visible in both.
        for s in [&a, &b] {
            assert_eq!(s.translate(&mem, high).map(|t| t.phys), Some(0x20_0000));
        }
        // The same user address maps to different frames in each.
        a.map_user(&mut mem, 0x40_0000, 0x10_0000, Access::ReadWrite)
            .unwrap();
        b.map_user(&mut mem, 0x40_0000, 0x11_0000, Access::ReadWrite)
            .unwrap();
        let ta = a.translate(&mem, 0x40_0000).unwrap();
        let tb = b.translate(&mem, 0x40_0000).unwrap();
        assert_eq!((ta.phys, tb.phys), (0x10_0000, 0x11_0000));
        assert!(ta.user && tb.user);
        assert_eq!(
            kspace.translate(&mem, 0x40_0000),
            None,
            "not in the kernel's half"
        );
        assert_eq!(
            a.map_user(&mut mem, high, 0, Access::Read),
            Err(MapError::NonCanonical(high)),
            "user mappings stay in the lower half"
        );
    }

    /// ADR-0004 H2. The defect this closes: a kernel mapping made in a top-level slot of its
    /// own, after a copy of the kernel half was taken, is missing from that copy.
    #[test]
    fn a_mapping_in_a_fresh_slot_reaches_an_earlier_copy_only_if_the_half_was_reserved() {
        let fresh = 0xFFFF_F000_0000_0000;
        let build = |reserve: bool, tables: &Vec<Table>| {
            let mut mem = TablePool::new(tables, ARENA_BASE);
            let mut kernel = AddressSpace::new(&mut mem).unwrap();
            if reserve {
                assert_eq!(kernel.reserve_upper_half(&mut mem), Ok(256));
            }
            let half = kernel.upper_half(&mem).unwrap();
            let mut process = AddressSpace::new(&mut mem).unwrap();
            process.set_upper_half(&mem, &half).unwrap();
            kernel.map(&mut mem, fresh, 0x5000, kernel_page()).unwrap();
            (process.translate(&mem, fresh).map(|t| t.phys), {
                let now = kernel.upper_half(&mem).unwrap();
                same_kernel_half(&now, &half)
            })
        };
        let tables = arena(300);
        assert_eq!(build(false, &tables), (None, false), "today's defect");
        let tables = arena(300);
        assert_eq!(build(true, &tables), (Some(0x5000), true));
    }

    fn kernel_page() -> Attributes {
        Attributes::kernel_global(Access::Read)
    }

    #[test]
    fn reserving_the_half_keeps_what_is_there_and_makes_the_rest() {
        let tables = arena(300);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        let (a, b) = (0xFFFF_8000_0000_0000, 0xFFFF_FFFF_8000_0000);
        space.map(&mut mem, a, 0x1000, kernel_page()).unwrap();
        space.map(&mut mem, b, 0x2000, kernel_page()).unwrap();
        assert!(!kernel_half_is_complete(&space.upper_half(&mem).unwrap()));
        assert_eq!(space.reserve_upper_half(&mut mem), Ok(254));
        let half = space.upper_half(&mem).unwrap();
        assert!(kernel_half_is_complete(&half));
        assert_eq!(space.translate(&mem, a).map(|t| t.phys), Some(0x1000));
        assert_eq!(space.translate(&mem, b).map(|t| t.phys), Some(0x2000));
        // The new tables are writable, supervisor links: a leaf below one is what decides.
        for (i, entry) in half.iter().enumerate() {
            assert_eq!(entry & USER, 0, "slot {}", i + 256);
            assert_eq!(entry & WRITABLE, WRITABLE, "slot {}", i + 256);
        }
        // Doing it again makes nothing, and no slot is ever written again by a later mapping.
        assert_eq!(space.reserve_upper_half(&mut mem), Ok(0));
        for slot in 256..512u64 {
            let virt = 0xFFFF_0000_0000_0000 | (slot << 39);
            let _ = space.map(&mut mem, virt + 0x20_0000, 0x3000, kernel_page());
        }
        assert!(same_kernel_half(&space.upper_half(&mem).unwrap(), &half));
    }

    #[test]
    fn a_half_that_cannot_be_reserved_says_so_and_can_be_finished_later() {
        let tables = arena(5);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        assert_eq!(
            space.reserve_upper_half(&mut mem),
            Err(MapError::OutOfMemory)
        );
        assert!(!kernel_half_is_complete(&space.upper_half(&mem).unwrap()));
        assert_eq!(mem.remaining(), 0);
        // Nothing but the slots that fit were written.
        let half = space.upper_half(&mem).unwrap();
        assert_eq!(half.iter().filter(|e| **e & PRESENT != 0).count(), 4);
    }

    #[test]
    fn only_the_accessed_bit_may_differ_between_copies_of_the_half() {
        let mut a = [0u64; ENTRIES / 2];
        a[3] = 0x1000 | PRESENT | WRITABLE;
        let mut b = a;
        assert!(same_kernel_half(&a, &b));
        b[3] |= ACCESSED;
        assert!(same_kernel_half(&a, &b), "the processor sets this one");
        b[3] |= USER;
        assert!(!same_kernel_half(&a, &b));
        b[3] = 0x2000 | PRESENT | WRITABLE | ACCESSED;
        assert!(!same_kernel_half(&a, &b));
        b = a;
        b[200] = 1;
        assert!(!same_kernel_half(&a, &b));
    }

    #[test]
    fn protect_changes_only_the_permissions_and_keeps_w_xor_x() {
        let tables = arena(8);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        let (virt, phys) = (0x40_0000, 0x7000);
        space
            .map_user(&mut mem, virt, phys, Access::ReadWrite)
            .unwrap();
        // What the processor sets by itself must survive a change of permissions.
        let table = {
            let mut table = space.root();
            for level in [3, 2, 1] {
                table = slot(&mem, table, virt, level)
                    .unwrap()
                    .load(Ordering::Relaxed)
                    & ADDRESS;
            }
            table
        };
        slot(&mem, table, virt, 0)
            .unwrap()
            .fetch_or(ACCESSED | 1 << 6, Ordering::Relaxed);
        for (access, writable, executable) in [
            (Access::Read, false, false),
            (Access::ReadExecute, false, true),
            (Access::ReadWrite, true, false),
        ] {
            assert_eq!(
                space.protect(&mut mem, virt, access),
                Ok(true),
                "{access:?}"
            );
            let t = space.translate(&mem, virt).unwrap();
            assert_eq!(
                (t.phys, t.size, t.writable, t.executable, t.user),
                (phys, PAGE, writable, executable, true),
                "{access:?}"
            );
            let raw = raw(&mem, &space, virt, 0);
            assert!(raw & WRITABLE == 0 || raw & NO_EXECUTE != 0, "W and X");
            assert_eq!(raw & (ACCESSED | 1 << 6), ACCESSED | 1 << 6, "A and D kept");
        }
        // Asking for what is already there changes nothing: no TLB work to do.
        assert_eq!(space.protect(&mut mem, virt, Access::ReadWrite), Ok(false));
    }

    #[test]
    fn protect_refuses_what_it_cannot_do() {
        let tables = arena(8);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        assert_eq!(
            space.protect(&mut mem, 0x1000, Access::Read),
            Err(MapError::NotMapped(0x1000))
        );
        assert_eq!(
            space.protect(&mut mem, 0x1234, Access::Read),
            Err(MapError::Misaligned(0x1234))
        );
        space
            .map_large(&mut mem, LARGE_PAGE, LARGE_PAGE, kernel(Access::Read))
            .unwrap();
        assert_eq!(
            space.protect(&mut mem, LARGE_PAGE, Access::ReadWrite),
            Err(MapError::LargePage(LARGE_PAGE))
        );
    }

    #[test]
    fn unmap_returns_the_frame_and_exhaustion_is_reported() {
        let tables = arena(4);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        let rw = kernel(Access::ReadWrite);
        space.map(&mut mem, 0x7000, 0x9000, rw).unwrap();
        assert_eq!(space.unmap(&mut mem, 0x7000), Ok(0x9000));
        assert_eq!(space.translate(&mem, 0x7000), None);
        assert_eq!(
            space.unmap(&mut mem, 0x7000),
            Err(MapError::NotMapped(0x7000))
        );
        // A page under another PDPT entry needs two new tables; the pool has none left.
        assert_eq!(
            space.map(&mut mem, 0x4000_0000, 0, rw),
            Err(MapError::OutOfMemory)
        );
    }

    #[test]
    fn split_large_keeps_every_translation_and_allows_one_hole() {
        let tables = arena(8);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        let virt = 4 * LARGE_PAGE;
        let phys = 0x4060_0000;
        space
            .map_large(&mut mem, virt, phys, kernel(Access::ReadWrite))
            .unwrap();
        let before: Vec<_> = (0..512)
            .map(|i| space.translate(&mem, virt + i * PAGE + 0x10).unwrap())
            .collect();
        space.split_large(&mut mem, virt + 0x5000).unwrap();
        for (i, b) in before.iter().enumerate() {
            let t = space
                .translate(&mem, virt + i as u64 * PAGE + 0x10)
                .unwrap();
            assert_eq!(t.size, PAGE);
            assert_eq!(
                (t.phys, t.writable, t.executable, t.user),
                (b.phys, b.writable, b.executable, b.user)
            );
        }
        // Now one page can become a hole (a guard page) and its neighbours stay.
        assert_eq!(space.unmap(&mut mem, virt + 0x5000), Ok(phys + 0x5000));
        assert_eq!(space.translate(&mem, virt + 0x5000), None);
        assert!(space.translate(&mem, virt + 0x4000).is_some());
        assert!(space.translate(&mem, virt + 0x6000).is_some());
        // Splitting a 4 KiB page is a no-op; an unmapped address is an error.
        assert_eq!(space.split_large(&mut mem, virt + 0x4000), Ok(()));
        assert_eq!(
            space.split_large(&mut mem, 0x1000),
            Err(MapError::NotMapped(0x1000))
        );
    }

    #[test]
    fn a_corrupt_link_is_reported_not_followed() {
        let tables = arena(8);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        let rw = kernel(Access::ReadWrite);
        space.map(&mut mem, 0x1000, 0x1000, rw).unwrap();
        // Point the PML4's first entry at a frame outside the pool.
        let bogus = 0x7700_0000;
        slot(&mem, space.root(), 0, 3)
            .unwrap()
            .store(bogus | PRESENT | WRITABLE, Ordering::Relaxed);
        assert_eq!(space.translate(&mem, 0x1000), None);
        assert_eq!(
            space.map(&mut mem, 0x2000, 0x2000, rw),
            Err(MapError::UnknownTable(bogus))
        );
    }

    #[test]
    fn many_scattered_pages_translate_exactly() {
        // Each random page may need a new PDPT, PD and PT.
        let tables = arena(6100);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut pages = Vec::new();
        for _ in 0..2000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let virt = (state & 0x0000_7FFF_FFFF_F000) | 0x1000;
            let phys = (state >> 20) & 0x000F_FFFF_F000;
            match space.map(&mut mem, virt, phys, kernel(Access::Read)) {
                Ok(()) => pages.push((virt, phys)),
                Err(e) => assert_eq!(e, MapError::AlreadyMapped(virt)),
            }
        }
        assert!(pages.len() > 1990);
        for &(virt, phys) in &pages {
            assert_eq!(
                space.translate(&mem, virt + 7).map(|t| t.phys),
                Some(phys + 7)
            );
        }
    }

    /// A kernel-half address of the tests: the stack region's (ADR-0004 Amendment 1, A2).
    const REGION: u64 = 0xFFFF_C000_0000_0000;

    #[test]
    fn reserved_tables_let_every_page_of_the_range_be_mapped_without_one() {
        let tables = arena(16);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        // A PDPT, a page directory, and a page table for each of the two 2 MiB pieces.
        assert_eq!(
            space.reserve_tables(&mut mem, REGION, 2 * LARGE_PAGE),
            Ok(4)
        );
        assert_eq!(mem.used(), 5);
        assert_eq!(
            space.reserve_tables(&mut mem, REGION, 2 * LARGE_PAGE),
            Ok(0),
            "made once"
        );
        // Nothing is mapped, and mapping anywhere in the range takes no table.
        assert_eq!(space.leaf_census(&mem), LeafCensus::default());
        let rw = Attributes::kernel_global(Access::ReadWrite);
        for virt in [
            REGION,
            REGION + LARGE_PAGE - PAGE,
            REGION + 2 * LARGE_PAGE - PAGE,
        ] {
            space.map(&mut mem, virt, 0x5000, rw).unwrap();
        }
        assert_eq!(mem.used(), 5, "no table made by the mappings");
        // Past the range, a mapping makes its own page table.
        space
            .map(&mut mem, REGION + 2 * LARGE_PAGE, 0x5000, rw)
            .unwrap();
        assert_eq!(mem.used(), 6);
        // A later range in the same directory makes only its page tables.
        assert_eq!(
            space.reserve_tables(&mut mem, REGION + 4 * LARGE_PAGE, LARGE_PAGE),
            Ok(1)
        );
    }

    #[test]
    fn reserving_tables_refuses_what_it_cannot_honour() {
        let tables = arena(8);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        assert_eq!(
            space.reserve_tables(&mut mem, REGION + PAGE, LARGE_PAGE),
            Err(MapError::Misaligned(REGION + PAGE))
        );
        assert_eq!(
            space.reserve_tables(&mut mem, REGION, PAGE),
            Err(MapError::Misaligned(PAGE))
        );
        // Into the hole between the halves, or off the top of the address space.
        let below = (1 << 47) - LARGE_PAGE;
        assert_eq!(
            space.reserve_tables(&mut mem, below, 2 * LARGE_PAGE),
            Err(MapError::NonCanonical(below))
        );
        assert_eq!(
            space.reserve_tables(&mut mem, 0xFFFF_FFFF_FFE0_0000, 2 * LARGE_PAGE),
            Err(MapError::Overflow)
        );
        // A 2 MiB leaf where a page table would go.
        let rw = Attributes::kernel_global(Access::ReadWrite);
        space.map_large(&mut mem, REGION, 0x20_0000, rw).unwrap();
        assert_eq!(
            space.reserve_tables(&mut mem, REGION, LARGE_PAGE),
            Err(MapError::LargePage(REGION))
        );
        // Running out: what was made stays, and a later call with room makes the rest.
        let few = arena(2);
        let mut small = TablePool::new(&few, ARENA_BASE);
        let mut other = AddressSpace::new(&mut small).unwrap();
        assert_eq!(
            other.reserve_tables(&mut small, REGION, LARGE_PAGE),
            Err(MapError::OutOfMemory)
        );
        assert_eq!(small.used(), 2, "the PDPT stays");
    }

    #[test]
    fn prepared_user_pages_map_without_a_table_through_links_open_to_user_mode() {
        let tables = arena(16);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        // Three pages across a 2 MiB boundary: a PDPT, a page directory and two page tables.
        let virt = 0x40_0000 - PAGE;
        assert_eq!(space.prepare_user(&mut mem, virt, 3), Ok(4));
        assert_eq!(space.prepare_user(&mut mem, virt, 3), Ok(0), "made once");
        assert_eq!(space.prepare_user(&mut mem, virt, 0), Ok(0));
        let used = mem.used();
        for level in 1..=3 {
            assert_eq!(raw(&mem, &space, virt, level) & USER, USER);
            assert_eq!(raw(&mem, &space, virt + 2 * PAGE, level) & USER, USER);
        }
        assert!(space.translate(&mem, virt).is_none(), "nothing is mapped");
        for k in 0..3 {
            space
                .map_user(
                    &mut mem,
                    virt + k * PAGE,
                    0x5000 + k * PAGE,
                    Access::ReadWrite,
                )
                .unwrap();
        }
        assert_eq!(mem.used(), used, "no table made by the mappings");
        assert!(space.translate(&mem, virt + 2 * PAGE).unwrap().user);
    }

    #[test]
    fn preparing_user_pages_refuses_what_it_cannot_honour() {
        let tables = arena(8);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        assert_eq!(
            space.prepare_user(&mut mem, 0x40_0001, 1),
            Err(MapError::Misaligned(0x40_0001))
        );
        let top = (1 << 47) - PAGE;
        assert_eq!(space.prepare_user(&mut mem, top, 1), Ok(3));
        assert_eq!(
            space.prepare_user(&mut mem, top, 2),
            Err(MapError::NonCanonical(1 << 47))
        );
        assert_eq!(
            space.prepare_user(&mut mem, 0xFFFF_8000_0000_0000, 1),
            Err(MapError::NonCanonical(0xFFFF_8000_0000_0000))
        );
        assert_eq!(
            space.prepare_user(&mut mem, 0x1000, u64::MAX / PAGE),
            Err(MapError::Overflow)
        );
        // Running out: what was made stays, and a later call with room makes the rest.
        let few = arena(2);
        let mut small = TablePool::new(&few, ARENA_BASE);
        let mut other = AddressSpace::new(&mut small).unwrap();
        assert_eq!(
            other.prepare_user(&mut small, 0x40_0000, 1),
            Err(MapError::OutOfMemory)
        );
        assert_eq!(small.used(), 2, "the PDPT stays");
    }

    #[test]
    fn leaf_sizes_count_only_the_range_asked_for() {
        let tables = arena(16);
        let mut mem = TablePool::new(&tables, ARENA_BASE);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        let rw = Attributes::kernel_global(Access::ReadWrite);
        let direct = 0xFFFF_8040_0000_0000;
        // Two 2 MiB leaves and three 4 KiB ones in the direct map; one of each elsewhere.
        let (small, large) = space
            .map_range(&mut mem, direct, 0x20_0000, 2 * LARGE_PAGE + 3 * PAGE, rw)
            .unwrap();
        assert_eq!((small, large), (3, 2));
        space.map(&mut mem, REGION, 0x9000, rw).unwrap();
        space
            .map_large(&mut mem, REGION + LARGE_PAGE, 0x60_0000, rw)
            .unwrap();
        space
            .map(&mut mem, 0x40_0000, 0xA000, kernel(Access::Read))
            .unwrap();
        let window = (0xFFFF_8000_0000_0000, REGION);
        assert_eq!(
            space.leaf_sizes(&mem, window.0, window.1),
            LeafSizes {
                small: 3,
                large: 2,
                huge: 0
            }
        );
        // The whole address space has every leaf; an empty range has none.
        assert_eq!(
            space.leaf_sizes(&mem, 0, u64::MAX),
            LeafSizes {
                small: 5,
                large: 3,
                huge: 0
            }
        );
        assert_eq!(space.leaf_sizes(&mem, REGION, REGION), LeafSizes::default());
        // A leaf that only partly overlaps the range counts.
        assert_eq!(
            space
                .leaf_sizes(&mem, direct + LARGE_PAGE - 1, direct + LARGE_PAGE)
                .large,
            1
        );
        // Splitting a 2 MiB leaf turns it into 512 4 KiB ones, and the count shows it.
        space.split_large(&mut mem, direct).unwrap();
        assert_eq!(
            space.leaf_sizes(&mem, window.0, window.1),
            LeafSizes {
                small: 3 + 512,
                large: 1,
                huge: 0
            }
        );
    }
}
