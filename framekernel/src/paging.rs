//! x86-64 4-level paging (Intel SDM Vol. 3A §4.5 and Tables 4-14 to 4-20; AMD64 APM Vol. 2
//! §5.3): page-table entries and an address-space builder, the third M2 step (CLAUDE.md
//! §28: virtual memory, page tables).
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

/// A leaf mapping's attributes. Caching is write-back (PAT index 0) for now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attributes {
    /// Permissions.
    pub access: Access,
    /// Accessible at CPL 3.
    pub user: bool,
    /// Kept in the TLB across CR3 loads (kernel mappings every address space shares).
    pub global: bool,
}

impl Attributes {
    /// A supervisor-only, non-global mapping.
    pub const fn kernel(access: Access) -> Self {
        Self {
            access,
            user: false,
            global: false,
        }
    }

    const fn bits(self) -> u64 {
        let access = match self.access {
            Access::Read => NO_EXECUTE,
            Access::ReadWrite => WRITABLE | NO_EXECUTE,
            Access::ReadExecute => 0,
        };
        let user = if self.user { USER } else { 0 };
        let global = if self.global { GLOBAL } else { 0 };
        PRESENT | access | user | global
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

/// The entry for `virt` at `level` in the table at `table`.
fn slot(mem: &impl TableMemory, table: u64, virt: u64, level: u32) -> Result<&AtomicU64, MapError> {
    mem.table(table)
        .and_then(|t| t.get(index(virt, level)))
        .ok_or(MapError::UnknownTable(table))
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
    /// Returns the number of 4 KiB and of 2 MiB pages mapped. On error, the pages mapped
    /// before the failure stay mapped.
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
        let (mut small, mut large) = (0, 0);
        let mut done = 0;
        while done < bytes {
            let (v, p) = (virt + done, phys + done);
            if v.is_multiple_of(LARGE_PAGE)
                && p.is_multiple_of(LARGE_PAGE)
                && bytes - done >= LARGE_PAGE
            {
                self.map_large(mem, v, p, attributes)?;
                large += 1;
                done += LARGE_PAGE;
            } else {
                self.map(mem, v, p, attributes)?;
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
        })
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
        let flags = value & (PRESENT | WRITABLE | USER | GLOBAL | NO_EXECUTE);
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
        };
        space.map(&mut mem, 0x2000, 0x2000, user).unwrap();
        for level in 1..=3 {
            assert_eq!(raw(&mem, &space, 0x2000, level) & USER, USER);
        }
        assert!(!space.translate(&mem, 0x1000).unwrap().user);
        assert!(space.translate(&mem, 0x2000).unwrap().user);
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
}
