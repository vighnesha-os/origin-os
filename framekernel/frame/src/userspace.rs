//! A user address space (CLAUDE.md §28, §29; ADR-0004 H2; ADR-0005 P1, P11; ADR-0011 N6): the
//! page tables of a process and the frames they map, owned by one object and reached only
//! through checked operations.
//!
//! A process cannot name the kernel's memory and the kernel never trusts a user address: every
//! copy to or from one goes through [`UserSpace::copy_in`] or [`UserSpace::copy_out`], which
//! check the range against this space's own tables and recover from a fault at the copy
//! instruction. Dropping a space frees its tables and frames, but only if no processor
//! still has it loaded (`cpu::root_loaded`): a safe caller that drops one too early leaks the
//! memory instead of leaving a processor running on freed page tables.

use alloc::vec::Vec;

use origin_abi::Error;
use origin_paging::{Access, AddressSpace, MapError, PAGE, Table, TableMemory, Translation};

use crate::addr::PhysAddr;
use crate::memory::{self, OwnedFrame};
use crate::usercopy::{self, UserAccess, UserRange};

/// Page-table frames one space may own (enough for a few scattered megabytes).
const MAX_TABLES: usize = 32;

/// The page-table frames of one space.
struct Tables {
    frames: Vec<OwnedFrame>,
}

impl TableMemory for Tables {
    fn allocate_table(&mut self) -> Option<u64> {
        if self.frames.len() >= MAX_TABLES {
            return None;
        }
        self.frames.try_reserve(1).ok()?;
        let frame = memory::allocate_frame()?;
        let p = core::ptr::with_exposed_provenance_mut::<u8>(frame.virt());
        // SAFETY: the allocator just handed out this frame, which only this space now owns;
        // the direct map makes it writable at `p`.
        unsafe { p.write_bytes(0, PAGE as usize) };
        let addr = frame.phys().raw();
        self.frames.push(frame);
        Some(addr)
    }

    fn table(&self, addr: u64) -> Option<&Table> {
        let frame = self.frames.iter().find(|f| f.phys().raw() == addr)?;
        let p = core::ptr::with_exposed_provenance::<Table>(frame.virt());
        // SAFETY: `addr` is a zeroed frame this space allocated for its tables (found just
        // above, so it is still owned), live until this space is dropped, reachable through the
        // direct map, and only ever accessed as atomics; `Table` is valid for any bits.
        Some(unsafe { &*p })
    }
}

/// A user page frame this space owns, named by its physical address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserFrame(PhysAddr);

impl UserFrame {
    /// The frame's physical address, for reports.
    pub const fn phys(self) -> PhysAddr {
        self.0
    }
}

/// One user address space: its own lower half and the kernel's half.
pub struct UserSpace {
    space: AddressSpace,
    tables: Tables,
    /// The user frames this space maps, freed with it.
    pages: Vec<OwnedFrame>,
}

impl core::fmt::Debug for UserSpace {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UserSpace")
            .field("root", &self.space.root())
            .finish_non_exhaustive()
    }
}

impl UserSpace {
    /// A new, empty space with the kernel's half mapped, or `None` without memory.
    pub fn new() -> Option<Self> {
        let mut tables = Tables { frames: Vec::new() };
        let mut space = AddressSpace::new(&mut tables).ok()?;
        let half = crate::vm::kernel_half()?;
        space.set_upper_half(&tables, &half).ok()?;
        Some(Self {
            space,
            tables,
            pages: Vec::new(),
        })
    }

    /// The physical address of this space's top-level table (the value for CR3), for reports
    /// and proofs; the scheduler loads the space through [`activate`](Self::activate).
    pub const fn root(&self) -> u64 {
        self.space.root()
    }

    /// Makes this space the current one on the calling processor. Safe: the space is complete
    /// (it maps the kernel's half, built by [`new`](Self::new)), and it stays alive for this
    /// borrow; the scheduler holds a reference per thread that runs in it, and a dropped
    /// space's memory is not freed while a processor still has it loaded.
    pub fn activate(&self) {
        let root = self.space.root();
        if crate::cpu::read_cr3() != root {
            // SAFETY: `root` is the PML4 of this complete address space, whose kernel half is the
            // kernel's (built by `new`), so the code, stacks and data running now stay mapped.
            unsafe { crate::cpu::write_cr3(root) };
        }
    }

    /// The frame mapped at user address `virt` in this space, if any.
    pub fn frame_of(&self, virt: u64) -> Option<UserFrame> {
        let leaf = self.translate(virt)?;
        let page = leaf.phys & !(PAGE - 1);
        self.pages
            .iter()
            .find(|f| f.phys().raw() == page)
            .map(|f| UserFrame(f.phys()))
    }

    /// Maps the user page `virt` to a fresh zeroed frame this space owns, with `access`. Only
    /// inside the user half. `&mut self`: nothing runs in the space while it changes (a
    /// process's creator holds it uniquely before any thread starts).
    pub fn map_page(&mut self, virt: u64, access: Access) -> Result<UserFrame, MapError> {
        if !usercopy::is_user_address(virt) || usercopy::USER_END - virt < PAGE {
            return Err(MapError::NonCanonical(virt));
        }
        self.pages
            .try_reserve(1)
            .map_err(|_| MapError::OutOfMemory)?;
        let frame = memory::allocate_frame().ok_or(MapError::OutOfMemory)?;
        let p = core::ptr::with_exposed_provenance_mut::<u8>(frame.virt());
        // SAFETY: the allocator just handed out this frame, which only this space now owns;
        // the direct map makes it writable at `p`, and it spans `PAGE` bytes.
        unsafe { p.write_bytes(0, PAGE as usize) };
        let phys = frame.phys();
        if let Err(e) = self
            .space
            .map_user(&mut self.tables, virt, phys.raw(), access)
        {
            let _ = memory::free_frame(frame);
            return Err(e);
        }
        self.pages.push(frame);
        Ok(UserFrame(phys))
    }

    /// Copies `bytes` into the frame `frame` of this space, at `offset`, with volatile
    /// writes. `false` if `frame` is not one of its frames or the range is not inside it.
    pub fn write_frame(&self, frame: UserFrame, offset: usize, bytes: &[u8]) -> bool {
        let Some(owned) = self.pages.iter().find(|f| f.phys() == frame.0) else {
            return false;
        };
        if offset
            .checked_add(bytes.len())
            .is_none_or(|end| end > PAGE as usize)
        {
            return false;
        }
        let p = core::ptr::with_exposed_provenance_mut::<u8>(owned.virt());
        for (i, &byte) in bytes.iter().enumerate() {
            let at = p.wrapping_add(offset + i);
            // SAFETY: `frame` is one of this space's frames (found above, so owned and live
            // while `self` is borrowed), writable through the direct map, and `offset + i` is
            // inside the page (checked). A volatile byte store: the frame may be mapped into
            // ring 3, so a racing user access touches only user memory, never a Rust object.
            unsafe { at.write_volatile(byte) };
        }
        true
    }

    /// The little-endian `u64` at `virt` in this space, read once (`None` unless `virt` is
    /// 8-aligned and mapped).
    pub fn read_u64(&self, virt: u64) -> Option<u64> {
        if !virt.is_multiple_of(8) {
            return None;
        }
        let leaf = self.translate(virt)?;
        let p = core::ptr::with_exposed_provenance::<u64>(crate::phys::to_virt(leaf.phys) as usize);
        // SAFETY: `translate` found the page mapped in this space, so its frame belongs to this
        // space and stays allocated while `self` is borrowed; the frame offset of `virt` is
        // 8-aligned and inside the page. A volatile read of plain data; a user thread may be
        // writing the word, so no reference is formed and the value is not trusted.
        Some(unsafe { p.read_volatile() })
    }

    /// How this space's tables translate the user address `virt`.
    pub fn translate(&self, virt: u64) -> Option<Translation> {
        self.space.translate(&self.tables, virt)
    }

    /// Whether this space is the one loaded on the calling processor (its CR3 holds this root).
    fn loaded_here(&self) -> bool {
        crate::cpu::read_cr3() == self.space.root()
    }

    /// Copies into `dst` from the user range `[src, src + dst.len())` of this space, checked
    /// against its tables, with a fault at the copy instruction recovered as an error.
    /// `Err` if the space is not the one loaded on this processor.
    pub fn copy_in(&self, dst: &mut [u8], src: u64) -> Result<(), Error> {
        let range = self.checked(src, dst.len() as u64, UserAccess::Read)?;
        if range.is_empty() {
            return Ok(());
        }
        let target = dst.as_mut_ptr().expose_provenance() as u64;
        // SAFETY: `dst` is a live kernel buffer of `range.len()` bytes that this call owns;
        // `src` was checked against this space's own tables, and this space is the one loaded
        // here (`checked`); a fault during the copy is recovered at the copy instruction.
        if unsafe { crate::cpu::copy_user(target, src, range.len()) } {
            Ok(())
        } else {
            Err(Error::Fault)
        }
    }

    /// Copies `src` to the user range at `dst`, as [`copy_in`](Self::copy_in) the other way.
    pub fn copy_out(&self, dst: u64, src: &[u8]) -> Result<(), Error> {
        let range = self.checked(dst, src.len() as u64, UserAccess::Write)?;
        if range.is_empty() {
            return Ok(());
        }
        let source = src.as_ptr().expose_provenance() as u64;
        // SAFETY: as in `copy_in`: `src` is a live kernel buffer, and the destination was
        // checked to be present, writable user memory of this space, the one loaded here.
        if unsafe { crate::cpu::copy_user(dst, source, range.len()) } {
            Ok(())
        } else {
            Err(Error::Fault)
        }
    }

    /// Checks a user pointer and length against this space's tables (the system-call layer's
    /// validation before it consumes anything). `Err` if this space is not loaded on this
    /// processor: the copy must run on its own page tables.
    pub fn check_user(&self, addr: u64, len: u64, access: UserAccess) -> Result<(), Error> {
        self.checked(addr, len, access).map(drop)
    }

    fn checked(&self, addr: u64, len: u64, access: UserAccess) -> Result<UserRange, Error> {
        if !self.loaded_here() {
            return Err(Error::Fault);
        }
        let range = UserRange::new(addr, len)?;
        usercopy::check_mapped(&range, access, |page| self.translate(page))?;
        Ok(range)
    }
}

impl Drop for UserSpace {
    fn drop(&mut self) {
        // Leak (not free) a space a processor still has loaded (see the module docs).
        if crate::cpu::root_loaded(self.space.root()) {
            core::mem::forget(core::mem::take(&mut self.pages));
            core::mem::forget(core::mem::take(&mut self.tables.frames));
            return;
        }
        for frame in self.pages.drain(..) {
            let _ = memory::free_frame(frame);
        }
        for frame in self.tables.frames.drain(..) {
            let _ = memory::free_frame(frame);
        }
    }
}
