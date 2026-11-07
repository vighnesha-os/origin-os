//! Process address spaces (M4, first step; CLAUDE.md §28 user address spaces, §29 process
//! isolation): each process has its own PML4, whose lower half holds only its own pages and
//! whose kernel half (entries 256–511) is a copy of the kernel's (ADR-0004 H2), so the
//! kernel is mapped the same in every address space.
//!
//! A process's page tables come from the frame allocator. Its [`TableMemory`] answers only
//! for frames it allocated itself, so the paging library can never be led into a frame the
//! process does not own (the kernel-half tables it shares are never walked through it: it
//! maps only lower-half addresses).

use framekernel::paging::{Access, AddressSpace, MapError, PAGE, Table, TableMemory};

use crate::{memory, phys, vm};

/// Page-table frames one process may own (enough for a few scattered megabytes; larger
/// processes come with a growable table list).
const MAX_TABLES: usize = 32;

/// The page-table frames of one process.
struct ProcessTables {
    frames: [u64; MAX_TABLES],
    count: usize,
}

impl TableMemory for ProcessTables {
    fn allocate_table(&mut self) -> Option<u64> {
        if self.count >= MAX_TABLES {
            return None;
        }
        let frame = memory::allocate_frame()?;
        let p = core::ptr::with_exposed_provenance_mut::<u8>(phys::to_virt(frame) as usize);
        // SAFETY: the allocator just handed out this frame, which only this process now
        // owns; the direct map makes it writable at `p`.
        unsafe { p.write_bytes(0, PAGE as usize) };
        let slot = self.frames.get_mut(self.count)?;
        *slot = frame;
        self.count += 1;
        Some(frame)
    }

    fn table(&self, addr: u64) -> Option<&Table> {
        if !self.frames.get(..self.count)?.contains(&addr) {
            return None;
        }
        let p = core::ptr::with_exposed_provenance::<Table>(phys::to_virt(addr) as usize);
        // SAFETY: `addr` is a zeroed frame this process allocated for its tables (checked
        // just above), live until `Process::destroy`, reachable through the direct map,
        // and only ever accessed as atomics; `Table` is valid for any bits.
        Some(unsafe { &*p })
    }
}

/// One process's address space.
pub(crate) struct Process {
    space: AddressSpace,
    tables: ProcessTables,
}

impl Process {
    /// A new address space: an empty lower half and the kernel's half.
    pub(crate) fn new() -> Option<Self> {
        let mut tables = ProcessTables {
            frames: [0; MAX_TABLES],
            count: 0,
        };
        let mut space = AddressSpace::new(&mut tables).ok()?;
        let half = vm::kernel_half()?;
        space.set_upper_half(&tables, &half).ok()?;
        Some(Self { space, tables })
    }

    /// The PML4 (physical), the value for CR3.
    pub(crate) const fn root(&self) -> u64 {
        self.space.root()
    }

    /// Maps the user page `virt` to frame `phys` with `access`.
    pub(crate) fn map(&mut self, virt: u64, phys: u64, access: Access) -> Result<(), MapError> {
        self.space.map_user(&mut self.tables, virt, phys, access)
    }

    /// Whether `virt` is mapped in this address space.
    pub(crate) fn is_mapped(&self, virt: u64) -> bool {
        self.space.translate(&self.tables, virt).is_some()
    }

    /// Frees the page-table frames (not the mapped frames, which the caller owns).
    ///
    /// The caller must make sure no processor will use this address space again: every
    /// thread in it has exited, and each processor that ran one has since loaded another
    /// CR3 (a CR3 load flushes the old translations; the kernel uses no PCIDs yet).
    pub(crate) fn destroy(self) {
        for &frame in self.tables.frames.iter().take(self.tables.count) {
            let _ = memory::free_frame(frame);
        }
    }
}
