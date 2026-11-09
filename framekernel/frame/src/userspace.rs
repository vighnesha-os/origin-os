//! A user address space (CLAUDE.md §28, §29; ADR-0004 H2; ADR-0005 P1, P11; ADR-0011 N6): the
//! page tables of a process and the frames they map, owned by one object and reached only
//! through checked operations.
//!
//! A process cannot name the kernel's memory and the kernel never trusts a user address: every
//! copy to or from one goes through [`UserSpace::copy_in`] or [`UserSpace::copy_out`], which
//! check the range against this space's own tables and recover from a fault at the copy
//! instruction. Dropping a space frees its tables and frames, but only if no processor
//! still has it loaded (`tlb::root_loaded`): a safe caller that drops one too early leaks the
//! memory instead of leaving a processor running on freed page tables.
//!
//! # A space that changes while it runs
//!
//! Every operation takes `&self`: the space's own threads may be running on other processors.
//!
//! - `state` holds the page tables and the records of what is mapped. It is taken briefly, with
//!   interrupts off, and never held across a TLB shootdown, so that it stays safe to spin on.
//! - `editor` makes changes to the space one at a time: taken around every change and held for
//!   as long as the TLBs need to catch up with it. Whoever waits for it serves shootdown requests
//!   in its loop, so waiting for it can never hold up the shootdown its holder is waiting for.
//!
//! The page-table memory of a space is freed only with the space, and no page is freed while
//! `state` could still lead to it, so a walk of the tables never reaches freed memory.
//!
//! An unmap edits the tables under `state`, then shoots down the processors that have this
//! space loaded, then releases the frames: a frame leaves its old owner only once no TLB can
//! lead to it (`Shootdown::retire`). If a processor does not answer, the frames are leaked, and
//! the caller is told.
//!
//! A map adds a translation that was not present, which no TLB can hold: it needs no shootdown.
//! Every other change does, and each reports what it cost ([`Change`]), so the layer above can
//! charge for it or limit it (a program that maps and unmaps in a loop interrupts every other
//! processor its threads run on, each time).

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use origin_abi::Error;
use origin_paging::{Access, AddressSpace, MapError, PAGE, Table, TableMemory, Translation};
use origin_sync::shootdown::{Completed, Request, Stalled};
use origin_sync::{SpinGuard, SpinLock};

use crate::addr::PhysAddr;
use crate::memory::{self, OwnedFrame};
use crate::usercopy::{self, UserAccess, UserRange};

/// Page-table frames one space may own (enough for a few scattered megabytes).
const MAX_TABLES: usize = 32;

/// How many spaces have given their memory back, for the proofs of when that happens.
static SPACES_FREED: AtomicU64 = AtomicU64::new(0);

/// How many address spaces have given their tables and frames back since the kernel started.
pub fn spaces_freed() -> u64 {
    SPACES_FREED.load(Ordering::Relaxed)
}

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

/// A user page frame this space maps, named by where it is mapped and what it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserFrame {
    virt: u64,
    phys: PhysAddr,
}

impl UserFrame {
    /// The frame's physical address, for reports.
    pub const fn phys(self) -> PhysAddr {
        self.phys
    }
}

/// The most pages one round of a change handles: how long it holds `state` with interrupts off,
/// and how many pages a shootdown covers at most.
const CHUNK: usize = 128;

/// One mapped page, and the frame it maps.
#[derive(Debug)]
struct Mapping {
    virt: u64,
    frame: OwnedFrame,
}

/// What a change to a space cost, for whoever pays for it (N14 step 3, S3-5b): each shootdown
/// interrupted `interrupts` processors, between them, while they ran.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Change {
    /// The pages it removed or changed.
    pub pages: u64,
    /// The TLB shootdowns it took (one per [`CHUNK`] pages at most).
    pub shootdowns: u32,
    /// The interrupts those sent to other processors.
    pub interrupts: u32,
}

impl Change {
    fn add(&mut self, done: Completed) {
        self.shootdowns += 1;
        self.interrupts += done.processors;
    }
}

/// Why a change to a space failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeError {
    /// The tables refused it, or it asked for addresses this space cannot map.
    Map(MapError),
    /// A processor did not answer within the shootdown's patience, so it may still hold a
    /// translation. The change is made; frames already taken out are leaked, not reused.
    Stalled {
        /// The processor.
        cpu: usize,
    },
}

impl From<MapError> for ChangeError {
    fn from(e: MapError) -> Self {
        Self::Map(e)
    }
}

impl From<Stalled> for ChangeError {
    fn from(stalled: Stalled) -> Self {
        Self::Stalled { cpu: stalled.cpu }
    }
}

/// What `state` holds.
struct State {
    space: AddressSpace,
    tables: Tables,
    /// What this space maps, by ascending address: each entry is a present user leaf, and each
    /// present user leaf has an entry.
    mapped: Vec<Mapping>,
    /// Empty, and with room for everything in `mapped`: what an unmap moves its records into,
    /// so that giving memory back never needs memory.
    spare: Vec<Mapping>,
}

impl State {
    /// A new, empty space with the kernel half `half`, or `None` without memory.
    fn new(half: &[u64; 256]) -> Option<Self> {
        let mut tables = Tables { frames: Vec::new() };
        let mut space = AddressSpace::new(&mut tables).ok()?;
        space.set_upper_half(&tables, half).ok()?;
        Some(Self {
            space,
            tables,
            mapped: Vec::new(),
            spare: Vec::new(),
        })
    }

    /// Where `virt`'s entry is, or would go, in `mapped`.
    fn position(&self, virt: u64) -> usize {
        self.mapped.partition_point(|m| m.virt < virt)
    }

    /// The record of the page at `virt`.
    fn mapping(&self, virt: u64) -> Option<&Mapping> {
        self.mapped
            .get(self.position(virt))
            .filter(|m| m.virt == virt)
    }

    /// Maps the user page `virt` to `frame` with `access`, or gives the frame back with the
    /// reason. Nothing changes on failure.
    fn map(
        &mut self,
        virt: u64,
        frame: OwnedFrame,
        access: Access,
    ) -> Result<(), (MapError, OwnedFrame)> {
        let needed = self.mapped.len() + 1;
        if self.mapped.try_reserve(1).is_err() || self.spare.try_reserve(needed).is_err() {
            return Err((MapError::OutOfMemory, frame));
        }
        let at = self.position(virt);
        if let Err(e) = self
            .space
            .map_user(&mut self.tables, virt, frame.phys().raw(), access)
        {
            return Err((e, frame));
        }
        self.mapped.insert(at, Mapping { virt, frame });
        Ok(())
    }

    /// Takes the next [`CHUNK`] pages of `[start, end)` out of the tables and out of the records:
    /// what it took, the translations that can still lead to it and where the next round
    /// starts, or `None` if there is nothing left to take. The frames stay allocated, and the
    /// caller gives them back only after a shootdown (`Shootdown::retire`).
    fn retire(&mut self, start: u64, end: u64) -> Result<Option<Retired>, MapError> {
        let lo = self.position(start);
        let hi = self.position(end).min(lo + CHUNK);
        if lo == hi {
            return Ok(None);
        }
        let count = hi - lo;
        if self.spare.capacity() < count {
            // Only after a stalled shootdown has lost the spare vector: an unmap normally has
            // the room it needs from the map that made the record.
            self.spare
                .try_reserve(count)
                .map_err(|_| MapError::OutOfMemory)?;
        }
        let mut cleared = lo;
        let (mut first, mut last) = (u64::MAX, 0);
        for mapping in self.mapped.get(lo..hi).unwrap_or(&[]) {
            if self.space.unmap(&mut self.tables, mapping.virt).is_err() {
                break;
            }
            first = first.min(mapping.virt);
            last = last.max(mapping.virt);
            cleared += 1;
        }
        if cleared == lo {
            return Err(MapError::UnknownTable(start));
        }
        let mut retired = core::mem::take(&mut self.spare);
        retired.extend(self.mapped.drain(lo..cleared));
        Ok(Some(Retired {
            mappings: retired,
            request: Request::covering(first, last),
            next: last + PAGE,
        }))
    }
}

/// What one round of an unmap took out of the tables (`State::retire`).
struct Retired {
    mappings: Vec<Mapping>,
    /// The translations that can still lead to it.
    request: Request,
    /// Where the next round starts.
    next: u64,
}

/// One user address space: its own lower half and the kernel's half.
pub struct UserSpace {
    /// The PML4's physical address, which never changes.
    root: u64,
    /// Changes to the space go one at a time: see the module documentation.
    editor: SpinLock<()>,
    state: SpinLock<State>,
}

impl core::fmt::Debug for UserSpace {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UserSpace")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

/// The end of the `pages` pages from `virt`, if they are whole pages of the user half.
fn user_range_end(virt: u64, pages: u64) -> Result<u64, MapError> {
    if !virt.is_multiple_of(PAGE) {
        return Err(MapError::Misaligned(virt));
    }
    if !usercopy::is_user_address(virt) {
        return Err(MapError::NonCanonical(virt));
    }
    pages
        .checked_mul(PAGE)
        .and_then(|bytes| virt.checked_add(bytes))
        .filter(|&end| end <= usercopy::USER_END)
        .ok_or(MapError::Overflow)
}

/// A fresh zeroed frame, or `None`.
fn zeroed_frame() -> Option<OwnedFrame> {
    let frame = memory::allocate_frame()?;
    let p = core::ptr::with_exposed_provenance_mut::<u8>(frame.virt());
    // SAFETY: the allocator just handed out this frame, which only this function holds; the
    // direct map makes it writable at `p`, and it spans `PAGE` bytes.
    unsafe { p.write_bytes(0, PAGE as usize) };
    Some(frame)
}

impl UserSpace {
    /// A new, empty space with the kernel's half mapped, or `None` without memory.
    pub fn new() -> Option<Self> {
        Self::from_state(State::new(crate::vm::kernel_half()?)?)
    }

    /// A space around tables that have just been built.
    fn from_state(state: State) -> Option<Self> {
        Some(Self {
            root: state.space.root(),
            editor: SpinLock::new(()),
            state: SpinLock::new(state),
        })
    }

    /// Runs `f` on this space's tables and records, with interrupts off.
    fn with_state<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        crate::cpu::without_interrupts(|| f(&mut self.state.lock()))
    }

    /// Takes this space's turn to change, serving TLB requests meanwhile (module documentation).
    fn edit(&self) -> SpinGuard<'_, ()> {
        loop {
            if let Some(turn) = self.editor.try_lock() {
                return turn;
            }
            crate::tlb::service();
            core::hint::spin_loop();
        }
    }

    /// As [`new`](Self::new), with the top-level entry of the kernel probe page's slot empty:
    /// what every space used to look like when a kernel mapping was made in a slot of its own
    /// after the space was created. For the in-boot control of that proof. Loading this space
    /// is harmless: nothing but a `KernelProbe` is ever mapped in that slot, so all the code,
    /// stacks and data the kernel touches stay mapped.
    pub fn without_probe_slot() -> Option<Self> {
        let mut half = *crate::vm::kernel_half()?;
        *half.get_mut(crate::vm::PROBE_SLOT)? = 0;
        Self::from_state(State::new(&half)?)
    }

    /// Whether this space's PML4 entries 256–511 are the ones the kernel fixed at boot
    /// (ADR-0004 H2).
    #[must_use]
    pub fn shares_kernel_half(&self) -> bool {
        let own = self.with_state(|s| s.space.upper_half(&s.tables));
        match (own, crate::vm::kernel_half()) {
            (Some(own), Some(fixed)) => origin_paging::same_kernel_half(&own, fixed),
            _ => false,
        }
    }

    /// The physical address of this space's top-level table (the value for CR3), for reports
    /// and proofs; the scheduler loads the space through [`activate`](Self::activate).
    pub const fn root(&self) -> u64 {
        self.root
    }

    /// Makes this space the current one on the calling processor. Safe: the space is complete
    /// (it maps the kernel's half, built by [`new`](Self::new)), and it stays alive for this
    /// borrow; the scheduler holds a reference per thread that runs in it, and a dropped
    /// space's memory is not freed while a processor still has it loaded.
    pub fn activate(&self) {
        if crate::cpu::read_cr3() != self.root {
            // SAFETY: `root` is the PML4 of this complete address space, whose kernel half is the
            // kernel's (built by `new`), so the code, stacks and data running now stay mapped.
            unsafe { crate::cpu::write_cr3(self.root) };
        }
    }

    /// The frame mapped at the user page holding `virt` in this space, if any.
    pub fn frame_of(&self, virt: u64) -> Option<UserFrame> {
        let page = virt & !(PAGE - 1);
        self.with_state(|s| {
            s.mapping(page).map(|m| UserFrame {
                virt: page,
                phys: m.frame.phys(),
            })
        })
    }

    /// Maps the user page `virt` to a fresh zeroed frame this space owns, with `access`. Only
    /// inside the user half.
    pub fn map_page(&self, virt: u64, access: Access) -> Result<UserFrame, MapError> {
        if !usercopy::is_user_address(virt) || usercopy::USER_END - virt < PAGE {
            return Err(MapError::NonCanonical(virt));
        }
        let frame = zeroed_frame().ok_or(MapError::OutOfMemory)?;
        let phys = frame.phys();
        let _turn = self.edit();
        match self.with_state(|s| s.map(virt, frame, access)) {
            Ok(()) => Ok(UserFrame { virt, phys }),
            Err((e, frame)) => {
                let _ = memory::free_frame(frame);
                Err(e)
            }
        }
    }

    /// Removes every mapping in the `pages` pages from `virt`, and gives back their frames.
    /// When it returns, no processor can reach them through this space, and a frame this
    /// space alone mapped is free. Pages not mapped are left alone. Safe with this space's
    /// threads running: one that touches a page after its translation is gone takes a fault.
    ///
    /// Must not be called holding a lock another processor may wait for with interrupts off.
    pub fn unmap(&self, virt: u64, pages: u64) -> Result<Change, ChangeError> {
        let end = user_range_end(virt, pages)?;
        let _turn = self.edit();
        let mut change = Change::default();
        let mut cursor = virt;
        while let Some(retired) = self.with_state(|s| s.retire(cursor, end))? {
            let Retired {
                mappings,
                request,
                next,
            } = retired;
            cursor = next;
            change.pages += mappings.len() as u64;
            let done =
                crate::tlb::retire_in_space(self.root, request, mappings, |mut mappings| {
                    for mapping in mappings.drain(..) {
                        let _ = memory::free_frame(mapping.frame);
                    }
                    self.with_state(|s| s.spare = mappings);
                })?;
            change.add(done);
        }
        Ok(change)
    }

    /// How many processors have this space loaded: what the next change would interrupt, so
    /// that a caller can price it before making it.
    pub fn processors_loaded(&self) -> u32 {
        crate::tlb::processors_holding(self.root)
    }

    /// Copies `bytes` into the frame `frame` of this space, at `offset`, with volatile
    /// writes. `false` if `frame` is no longer mapped there or the range is not inside it.
    pub fn write_frame(&self, frame: UserFrame, offset: usize, bytes: &[u8]) -> bool {
        if offset
            .checked_add(bytes.len())
            .is_none_or(|end| end > PAGE as usize)
        {
            return false;
        }
        self.with_state(|s| {
            let Some(mapping) = s
                .mapping(frame.virt)
                .filter(|m| m.frame.phys() == frame.phys)
            else {
                return false;
            };
            let p = core::ptr::with_exposed_provenance_mut::<u8>(mapping.frame.virt());
            for (i, &byte) in bytes.iter().enumerate() {
                let at = p.wrapping_add(offset + i);
                // SAFETY: the frame is one this space maps (found above) and `state` is held,
                // so it stays allocated; the direct map makes it writable, and `offset + i` is
                // inside the page (checked). A volatile byte store: the frame may be mapped
                // into ring 3, so a racing user access touches only user memory.
                unsafe { at.write_volatile(byte) };
            }
            true
        })
    }

    /// The little-endian `u64` at `virt` in this space, read once (`None` unless `virt` is
    /// 8-aligned and mapped).
    pub fn read_u64(&self, virt: u64) -> Option<u64> {
        if !virt.is_multiple_of(8) {
            return None;
        }
        self.with_state(|s| {
            let leaf = s.space.translate(&s.tables, virt)?;
            let p =
                core::ptr::with_exposed_provenance::<u64>(crate::phys::to_virt(leaf.phys) as usize);
            // SAFETY: `translate` found the page mapped in this space and `state` is held, so
            // nothing can unmap and free its frame during the read; the offset of `virt` is
            // 8-aligned and inside the page. A volatile read of plain data; a user thread may
            // be writing the word, so no reference is formed and the value is not trusted.
            Some(unsafe { p.read_volatile() })
        })
    }

    /// How this space's tables translate the user address `virt`.
    pub fn translate(&self, virt: u64) -> Option<Translation> {
        self.with_state(|s| s.space.translate(&s.tables, virt))
    }

    /// Whether this space is the one loaded on the calling processor (its CR3 holds this root).
    fn loaded_here(&self) -> bool {
        crate::cpu::read_cr3() == self.root
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
        let mut state = self.state.lock();
        // Leak (not free) a space a processor still has loaded (see the module docs).
        if crate::tlb::root_loaded(self.root) {
            core::mem::forget(core::mem::take(&mut state.mapped));
            core::mem::forget(core::mem::take(&mut state.tables.frames));
            return;
        }
        for mapping in state.mapped.drain(..) {
            let _ = memory::free_frame(mapping.frame);
        }
        for frame in state.tables.frames.drain(..) {
            let _ = memory::free_frame(frame);
        }
        SPACES_FREED.fetch_add(1, Ordering::Relaxed);
    }
}
