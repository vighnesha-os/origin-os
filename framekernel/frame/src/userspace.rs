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
//! An unmap edits the tables under `state` a round at a time, and shoots each round down on the
//! processors that have this space loaded before it releases that round's frames: a frame
//! leaves its old owner only once no TLB can lead to it (`Confirmation::change`). If a
//! processor does not answer, the unmap is still made to its end. One flush of the whole space
//! then decides whether the frames taken out since are released or leaked, and the caller is
//! told if they were leaked (S3-5b's review, finding A). A protect goes the same way.
//!
//! A map adds a translation that was not present, which no TLB can hold: it needs no shootdown.
//! A protect does, also when it only adds access: a processor that still caches the old
//! permissions would take a page fault for an access now allowed, and a fault ends a user thread.
//! Every change that needs one reports what it cost ([`Change`]), so the layer above can
//! charge for it or limit it (a program that maps and unmaps in a loop interrupts every other
//! processor its threads run on, each time).

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use origin_abi::Error;
use origin_paging::{Access, AddressSpace, MapError, PAGE, Table, TableMemory, Translation};
use origin_sync::shootdown::{Confirmation, Cost, Request, Rounds, Stalled};
use origin_sync::{SpinGuard, SpinLock};

use crate::addr::PhysAddr;
use crate::aliases::{Aliases, Mode};
use crate::memory::{self, OwnedFrame};
use crate::objref::ObjectRef;
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

/// A frame that can be mapped into several address spaces at once. Each mapping holds a
/// reference, given up only after its own shootdown, and the frame goes back to the allocator
/// with the last reference: so no processor can reach it by then, through any space.
///
/// What is in it is shared with ring 3, so it is reached only by volatile copies, never as a
/// Rust reference.
#[derive(Clone, Debug)]
pub struct SharedFrame {
    inner: ObjectRef<SharedInner>,
}

#[derive(Debug)]
struct SharedInner {
    phys: PhysAddr,
    /// `Some` until it is dropped: the frame moves out to be freed.
    frame: Option<OwnedFrame>,
    /// Its writable and executable mappings, in every space: never both (review finding 5).
    aliases: Aliases,
}

impl Drop for SharedInner {
    fn drop(&mut self) {
        if let Some(frame) = self.frame.take() {
            let _ = memory::free_frame(frame);
        }
    }
}

impl SharedFrame {
    /// A fresh frame, zeroed, or `None` without memory.
    pub fn new() -> Option<Self> {
        Self::adopt(memory::zeroed_frame()?)
    }

    /// The frame at `phys`, if it is free, zeroed: for a proof that needs to see one particular
    /// frame reused. Any free frame is as good as another to the rest of the kernel.
    pub fn claim(phys: PhysAddr) -> Option<Self> {
        let frame = memory::claim_frame(phys)?;
        memory::zero_frame(&frame);
        Self::adopt(frame)
    }

    fn adopt(frame: OwnedFrame) -> Option<Self> {
        let phys = frame.phys();
        // On failure the value comes back and is dropped, which frees the frame.
        ObjectRef::try_new(SharedInner {
            phys,
            frame: Some(frame),
            aliases: Aliases::new(),
        })
        .ok()
        .map(|inner| Self { inner })
    }

    /// The frame's physical address.
    #[must_use]
    pub fn phys(&self) -> PhysAddr {
        self.inner.phys
    }

    /// How many mappings, in every space, make this frame writable, and how many executable:
    /// one of the two is always 0 (review finding 5).
    #[must_use]
    pub fn mappings(&self) -> (u32, u32) {
        self.inner.aliases.counts()
    }

    /// Writes `value` at `offset`, once, as a volatile store. `false` unless that is an
    /// 8-aligned offset inside the frame.
    #[must_use]
    pub fn write_u64(&self, offset: usize, value: u64) -> bool {
        let Some(p) = self.word(offset) else {
            return false;
        };
        // SAFETY: `word` checked the offset, and this reference keeps the frame allocated. A
        // volatile store: the frame may be mapped into ring 3, so a racing access touches only
        // user memory.
        unsafe { p.write_volatile(value) };
        true
    }

    /// The word at `offset`, read once, as a volatile load. `None` unless that is an
    /// 8-aligned offset inside the frame.
    #[must_use]
    pub fn read_u64(&self, offset: usize) -> Option<u64> {
        let p = self.word(offset)?;
        // SAFETY: as in `write_u64`; the value is plain data and is not trusted.
        Some(unsafe { p.read_volatile() })
    }

    /// Where the kernel reaches the word at `offset`, if that is an 8-aligned offset inside
    /// the frame.
    fn word(&self, offset: usize) -> Option<*mut u64> {
        let inside = offset
            .checked_add(8)
            .is_some_and(|end| end <= PAGE as usize);
        if !inside || !offset.is_multiple_of(8) {
            return None;
        }
        let base = direct_map(self.phys());
        Some(base.wrapping_add(offset).cast())
    }
}

/// What a mapping owns of the frame it maps.
#[derive(Debug)]
enum Frame {
    /// A frame only this space has: freed when it is unmapped.
    Private(OwnedFrame),
    /// A reference to a frame other spaces may map too, counted in its [`Aliases`] with `mode`.
    /// `pending` is a mode this mapping had until a change of permissions, still counted until
    /// the shootdown of that change has completed.
    Shared {
        frame: SharedFrame,
        mode: Mode,
        pending: Mode,
    },
}

impl Frame {
    /// The frame's physical address.
    fn phys(&self) -> PhysAddr {
        match self {
            Self::Private(frame) => frame.phys(),
            Self::Shared { frame, .. } => frame.phys(),
        }
    }

    /// Gives back a mode left from a change of permissions, once no processor can use it.
    fn give_back_pending(&mut self) {
        if let Self::Shared { frame, pending, .. } = self {
            frame.inner.aliases.give_back(*pending);
            *pending = Mode::Plain;
        }
    }

    /// Gives up this mapping's hold on the frame: only with no processor able to reach it
    /// through this mapping.
    fn release(mut self) {
        self.give_back_pending();
        match self {
            Self::Private(frame) => {
                let _ = memory::free_frame(frame);
            }
            Self::Shared { frame, mode, .. } => {
                frame.inner.aliases.give_back(mode);
                drop(frame);
            }
        }
    }
}

/// One mapped page, and the frame it maps.
#[derive(Debug)]
struct Mapping {
    virt: u64,
    frame: Frame,
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
    /// A change of `pages` pages that cost `cost`.
    const fn of(pages: u64, cost: Cost) -> Self {
        Self {
            pages,
            shootdowns: cost.shootdowns,
            interrupts: cost.interrupts,
        }
    }
}

/// Why a change to a space failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeError {
    /// The tables refused it, or it asked for addresses this space cannot map.
    Map(MapError),
    /// A processor did not answer within the shootdown's patience, and the flush of the whole
    /// space that ends such a change did not complete either, so a processor may still hold a
    /// translation. The change is made all the same: every page it names has its final entry
    /// in the tables. Nothing it took out after the first stall is reused: an unmap's frames
    /// are leaked, and a protect's old modes stay counted until a later flush of the space
    /// completes. The space stays unconfirmed, so the next change starts with that flush
    /// (S3-5b's review, finding A).
    Stalled {
        /// The processor.
        cpu: usize,
    },
    /// A shared frame would be writable through one mapping and executable through another
    /// (review finding 5). Nothing was changed.
    WriteAndExecute,
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
    fn map(&mut self, virt: u64, frame: Frame, access: Access) -> Result<(), (MapError, Frame)> {
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

    /// Makes ready to map `count` pages from `base` to `end`: refuses if one of them is mapped,
    /// and makes the room for their records and every page table they need, so that
    /// [`map_run`](Self::map_run) cannot fail for want of either. On a failure, what it made
    /// stays, unused: room in two vectors, and empty tables this space frees with the rest.
    fn prepare_run(&mut self, base: u64, end: u64, count: usize) -> Result<(), MapError> {
        if let Some(mapped) = self
            .mapped
            .get(self.position(base))
            .filter(|m| m.virt < end)
        {
            return Err(MapError::AlreadyMapped(mapped.virt));
        }
        let records = self
            .mapped
            .len()
            .checked_add(count)
            .ok_or(MapError::OutOfMemory)?;
        if self.mapped.try_reserve(count).is_err() || self.spare.try_reserve(records).is_err() {
            return Err(MapError::OutOfMemory);
        }
        self.space
            .prepare_user(&mut self.tables, base, count as u64)
            .map(drop)
    }

    /// Maps `frames`, one page each from `virt`, with `access`, after
    /// [`prepare_run`](Self::prepare_run) made ready for them: each frame is counted in its
    /// aliases with `mode` already. Only corrupt tables can make an entry fail, and then the
    /// pages before it stay mapped, recorded, and the failure says how many those are.
    fn map_run(
        &mut self,
        virt: u64,
        frames: &[SharedFrame],
        mode: Mode,
        access: Access,
    ) -> Result<(), (usize, MapError)> {
        let at = self.position(virt);
        let before = self.mapped.len();
        let mut failed = None;
        for (k, frame) in (0..).zip(frames) {
            let page = virt + k * PAGE;
            if let Err(e) = self
                .space
                .map_user(&mut self.tables, page, frame.phys().raw(), access)
            {
                failed = Some(e);
                break;
            }
            // No allocation: `prepare_run` made the room.
            self.mapped.push(Mapping {
                virt: page,
                frame: Frame::Shared {
                    frame: frame.clone(),
                    mode,
                    pending: Mode::Plain,
                },
            });
        }
        let mapped = self.mapped.len() - before;
        // The new records go where `virt` belongs: every record from `at` on lies above the run.
        if let Some(above) = self.mapped.get_mut(at..) {
            above.rotate_right(mapped);
        }
        failed.map_or(Ok(()), |e| Err((mapped, e)))
    }

    /// Counts the mode `access` gives the shared frames among the next [`CHUNK`] mapped pages of
    /// `[start, end)`: where it is the end of the range, `Ok(None)`; where a frame would be
    /// writable through one mapping and executable through another, the page it stopped at.
    /// Pages before that one were counted, and `unclaim_modes` gives them back.
    fn claim_modes(&mut self, start: u64, end: u64, access: Access) -> Result<Option<u64>, u64> {
        let lo = self.position(start);
        let hi = self.position(end).min(lo + CHUNK);
        let new = Mode::of(access);
        let mut next = None;
        for mapping in self.mapped.get(lo..hi).unwrap_or(&[]) {
            next = Some(mapping.virt + PAGE);
            let Frame::Shared { frame, mode, .. } = &mapping.frame else {
                continue;
            };
            let flip = matches!(
                (*mode, new),
                (Mode::Write, Mode::Execute) | (Mode::Execute, Mode::Write)
            );
            if *mode != new && (flip || !frame.inner.aliases.take(new)) {
                return Err(mapping.virt);
            }
        }
        Ok(next)
    }

    /// Gives back what `claim_modes` counted for `access` in `[start, stop)`.
    fn unclaim_modes(&mut self, start: u64, stop: u64, access: Access) {
        let (lo, hi) = (self.position(start), self.position(stop));
        let new = Mode::of(access);
        for mapping in self.mapped.get(lo..hi).unwrap_or(&[]) {
            if let Frame::Shared { frame, mode, .. } = &mapping.frame
                && *mode != new
            {
                frame.inner.aliases.give_back(new);
            }
        }
    }

    /// Gives the next [`CHUNK`] mapped pages of `[start, end)` the permissions `access`, or says
    /// there are none left. A shared frame's old mode stays counted, as `pending`, until the
    /// caller has shot the old permissions down (`give_back_pending`).
    fn protect(&mut self, start: u64, end: u64, access: Access) -> Option<Reprotected> {
        let lo = self.position(start);
        let hi = self.position(end).min(lo + CHUNK);
        let new = Mode::of(access);
        let round = self.mapped.get_mut(lo..hi)?;
        let (first, last) = (round.first()?.virt, round.last()?.virt);
        let mut changed = 0;
        for mapping in round {
            if self.space.protect(&mut self.tables, mapping.virt, access) == Ok(true) {
                changed += 1;
                if let Frame::Shared {
                    frame,
                    mode,
                    pending,
                } = &mut mapping.frame
                    && *mode != new
                {
                    match (*pending, *mode) {
                        (Mode::Plain, old) => *pending = old,
                        // A mode an earlier change left pending (its shootdown stalled, and no
                        // flush has completed since) stays, and keeps the frame's kind counted:
                        // the old mode's own count can go.
                        (kept, old) if kept == old => frame.inner.aliases.give_back(old),
                        // The old mode is read-only, which is not counted. The other kind
                        // cannot be counted beside the pending one (`claim_modes`).
                        _ => {}
                    }
                    *mode = new;
                }
            }
        }
        Some(Reprotected {
            changed,
            request: Request::covering(first, last),
            next: last + PAGE,
        })
    }

    /// Gives back the modes left pending in `[start, stop)`: their old permissions have been
    /// shot down.
    fn give_back_pending(&mut self, start: u64, stop: u64) {
        let (lo, hi) = (self.position(start), self.position(stop));
        for mapping in self.mapped.get_mut(lo..hi).unwrap_or(&mut []) {
            mapping.frame.give_back_pending();
        }
    }

    /// Takes the next [`CHUNK`] pages of `[start, end)` out of the tables and moves their
    /// records into `into`: the translations that can still lead to them, how many there were
    /// and where the next round starts, or `None` if there is nothing left to take. The frames
    /// stay allocated, and the caller gives them back only once no processor can reach them
    /// (`Confirmation::change`).
    fn retire(
        &mut self,
        start: u64,
        end: u64,
        into: &mut Vec<Mapping>,
    ) -> Result<Option<Retired>, MapError> {
        let lo = self.position(start);
        let hi = self.position(end).min(lo + CHUNK);
        if lo == hi {
            return Ok(None);
        }
        // No allocation: `into` is the spare vector, which every map leaves with room for all
        // the records.
        into.try_reserve(hi - lo)
            .map_err(|_| MapError::OutOfMemory)?;
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
        into.extend(self.mapped.drain(lo..cleared));
        Ok(Some(Retired {
            pages: (cleared - lo) as u64,
            request: Request::covering(first, last),
            next: last + PAGE,
        }))
    }
}

/// What one round of a change of permissions did (`State::protect`).
struct Reprotected {
    /// The pages whose permissions it changed.
    changed: u64,
    /// The translations that may still hold the old permissions.
    request: Request,
    /// Where the next round starts.
    next: u64,
}

/// What one round of an unmap took out of the tables (`State::retire`).
struct Retired {
    /// The pages it took out.
    pages: u64,
    /// The translations that can still lead to them.
    request: Request,
    /// Where the next round starts.
    next: u64,
}

/// An unmap of `[cursor, end)`, as [`Confirmation::change`] makes it: each round takes up to
/// [`CHUNK`] mapped pages out of the tables, and their records, with their frames, wait in
/// `taken` until no processor can reach them.
struct Unmapping<'a> {
    space: &'a UserSpace,
    cursor: u64,
    end: u64,
    /// The space's spare vector while the unmap runs, with room for every record it can take.
    taken: Vec<Mapping>,
    /// The pages taken out so far.
    pages: u64,
}

impl Unmapping<'_> {
    /// Gives back every frame waiting: no processor can reach one through this space any more.
    fn release(&mut self) {
        for mapping in self.taken.drain(..) {
            mapping.frame.release();
        }
    }
}

impl Rounds for Unmapping<'_> {
    type Error = ChangeError;

    fn prepare(&mut self) -> Result<(), ChangeError> {
        Ok(())
    }

    fn round(&mut self) -> Result<Option<Request>, ChangeError> {
        let (cursor, end, taken) = (self.cursor, self.end, &mut self.taken);
        let Some(retired) = self.space.with_state(|s| s.retire(cursor, end, taken))? else {
            return Ok(None);
        };
        self.cursor = retired.next;
        self.pages += retired.pages;
        Ok(Some(retired.request))
    }

    fn shot(&mut self) {
        self.release();
    }

    fn flushed(&mut self) {
        self.release();
        // Every old permission is gone everywhere too, including any a stall left counted.
        self.space.with_state(|s| s.give_back_pending(0, u64::MAX));
    }

    fn lost(&mut self) {
        // A processor may still reach these frames: they stay allocated, for good.
        for mapping in self.taken.drain(..) {
            core::mem::forget(mapping);
        }
    }
}

/// A change of the permissions of `[start, end)` to `access`, as [`Confirmation::change`]
/// makes it: each round changes up to [`CHUNK`] mapped pages, and a shared frame's old mode
/// stays counted, as pending, until no processor can use the old permissions.
struct Reprotecting<'a> {
    space: &'a UserSpace,
    access: Access,
    start: u64,
    end: u64,
    /// Where the round just made began, and where the next one begins.
    round: u64,
    cursor: u64,
    /// The pages whose permissions changed so far.
    pages: u64,
}

impl Rounds for Reprotecting<'_> {
    type Error = ChangeError;

    fn prepare(&mut self) -> Result<(), ChangeError> {
        self.space.claim_modes(self.start, self.end, self.access)
    }

    fn round(&mut self) -> Result<Option<Request>, ChangeError> {
        let (end, access) = (self.end, self.access);
        loop {
            let cursor = self.cursor;
            let Some(round) = self.space.with_state(|s| s.protect(cursor, end, access)) else {
                return Ok(None);
            };
            (self.round, self.cursor) = (cursor, round.next);
            self.pages += round.changed;
            // A round that changed nothing needs no shootdown: it left nothing pending.
            if round.changed != 0 {
                return Ok(Some(round.request));
            }
        }
    }

    fn shot(&mut self) {
        let (start, stop) = (self.round, self.cursor);
        self.space.with_state(|s| s.give_back_pending(start, stop));
    }

    fn flushed(&mut self) {
        self.space.with_state(|s| s.give_back_pending(0, u64::MAX));
    }

    fn lost(&mut self) {
        // The old modes stay counted, as pending, until a flush of the whole space completes.
    }
}

/// One user address space: its own lower half and the kernel's half.
pub struct UserSpace {
    /// The PML4's physical address, which never changes.
    root: u64,
    /// Changes to the space go one at a time: see the module documentation.
    editor: SpinLock<()>,
    state: SpinLock<State>,
    /// Whether a stalled shootdown of this space has not been made good yet (review finding
    /// 3). Changed only under `editor`.
    confirmation: Confirmation,
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

/// Where the kernel reaches the frame at `phys`: its address in the direct map.
fn direct_map(phys: PhysAddr) -> *mut u8 {
    core::ptr::with_exposed_provenance_mut(crate::phys::to_virt(phys.raw()) as usize)
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
            confirmation: Confirmation::new(),
        })
    }

    /// Runs `f` on this space's tables and records, with interrupts off.
    fn with_state<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        crate::cpu::without_interrupts(|| f(&mut self.state.lock()))
    }

    /// Takes this space's turn to change, serving TLB requests meanwhile (module documentation).
    /// System calls come here with interrupts on, and a waiter that the holder has been
    /// preempted under is preempted in turn; with interrupts off (the boot flow) nothing
    /// preempts the waiter, so it must not contend with a holder on its own processor.
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
    /// after the space was created. Only for `KernelProbe::read_without_its_slot`, the in-boot
    /// control of that proof, which drops it before it returns: a space that breaks H2 is never
    /// handed out (review housekeeping). Visiting it is harmless: nothing but a `KernelProbe` is
    /// ever mapped in that slot, so all the code, stacks and data the kernel touches stay mapped.
    pub(crate) fn without_probe_slot() -> Option<Self> {
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

    /// Makes this space the current one on the calling processor, for the thread the
    /// scheduler is about to resume: only at the scheduler's switch point (`trap::Switch`),
    /// so that nothing else can leave a processor on a space the scheduler did not choose
    /// (review finding 2). Safe: the space is complete (it maps the kernel's half, built by
    /// [`new`](Self::new)), and it stays alive for this borrow; the scheduler holds a reference
    /// per thread that runs in it, and a dropped space's memory is not freed while a processor
    /// still has it loaded.
    pub fn activate(&self, _switch: &crate::trap::Switch) {
        // SAFETY: `root` is the PML4 of this complete address space, whose kernel half is the
        // kernel's (built by `new`), so the code, stacks and data running now stay mapped.
        unsafe { crate::cpu::write_cr3(self.root) };
    }

    /// Reads a byte at `addr` through this space's own tables, on this processor, and says what
    /// happened (`cpu::probe_read`): for the boot proofs. The processor visits this space with
    /// interrupts off and goes back to the one it was on, so the scheduler's choice stands
    /// (review finding 2). The page's translation is dropped first, so the read walks these
    /// tables whatever an earlier read left in the TLB (finding 4).
    pub fn probe_read(&self, addr: u64) -> crate::cpu::Probe {
        // The space stays alive for this borrow, and is complete (`new`).
        crate::tlb::visit(self.root, || {
            crate::cpu::invlpg(addr);
            crate::cpu::probe_read(addr)
        })
    }

    /// The frame mapped at the user page holding `virt` in this space, if any.
    pub fn frame_of(&self, virt: u64) -> Option<UserFrame> {
        let page = virt & !(PAGE - 1);
        self.with_state(|s| {
            let phys = s.mapping(page)?.frame.phys();
            Some(UserFrame { virt: page, phys })
        })
    }

    /// Maps the user page `virt` to a fresh zeroed frame this space owns, with `access`. Only
    /// inside the user half.
    pub fn map_page(&self, virt: u64, access: Access) -> Result<UserFrame, MapError> {
        if !usercopy::is_user_address(virt) || usercopy::USER_END - virt < PAGE {
            return Err(MapError::NonCanonical(virt));
        }
        let frame = memory::zeroed_frame().ok_or(MapError::OutOfMemory)?;
        let phys = frame.phys();
        let _turn = self.edit();
        match self.with_state(|s| s.map(virt, Frame::Private(frame), access)) {
            Ok(()) => Ok(UserFrame { virt, phys }),
            Err((e, frame)) => {
                frame.release();
                Err(e)
            }
        }
    }

    /// Maps the user page `virt` to `frame`, with `access`, as this space's own mapping of a
    /// frame others may map too: [`map_shared_run`](Self::map_shared_run) with one frame.
    ///
    /// # Errors
    ///
    /// As `map_shared_run`'s; nothing is mapped on any of them.
    pub fn map_shared(
        &self,
        virt: u64,
        frame: &SharedFrame,
        access: Access,
    ) -> Result<(), ChangeError> {
        self.map_shared_run(virt, core::slice::from_ref(frame), access)
    }

    /// Maps `frames`, one page each from the user page `base`, all with `access`, as this
    /// space's own mappings of frames others may map too: every one of them, or none (S3-5b's
    /// review, finding B). Needs no shootdown: a translation that was not present is in no
    /// TLB, and after a failure no entry of the run has ever been present.
    ///
    /// Everything that can fail is done for the whole run before its first entry is written:
    /// the check that no page of it is mapped, the room for its records, its page tables, and
    /// each frame's count of writable or executable mappings. The entries are then written a
    /// round of [`CHUNK`] at a time.
    ///
    /// # Errors
    ///
    /// Each before any entry of the run is written. The page tables it needed stay made, and
    /// empty, for this space to use later.
    ///
    /// - [`ChangeError::Map`]: `base` is not a page of the user half, or the run does not fit
    ///   there; [`MapError::AlreadyMapped`] with the first page of the run that is mapped;
    ///   [`MapError::OutOfMemory`] without a frame for a table, or memory for the records.
    /// - [`ChangeError::WriteAndExecute`] if a frame would then be writable through one mapping
    ///   and executable through another, in any space (review finding 5).
    pub fn map_shared_run(
        &self,
        base: u64,
        frames: &[SharedFrame],
        access: Access,
    ) -> Result<(), ChangeError> {
        let end = user_range_end(base, frames.len() as u64)?;
        if frames.is_empty() {
            return Ok(());
        }
        let mode = Mode::of(access);
        let _turn = self.edit();
        self.with_state(|s| s.prepare_run(base, end, frames.len()))?;
        // Counted before any entry is written; the counts taken go back if one cannot be.
        for (counted, frame) in frames.iter().enumerate() {
            if !frame.inner.aliases.take(mode) {
                for frame in frames.iter().take(counted) {
                    frame.inner.aliases.give_back(mode);
                }
                return Err(ChangeError::WriteAndExecute);
            }
        }
        let mut virt = base;
        for round in frames.chunks(CHUNK) {
            if let Err((mapped, e)) = self.with_state(|s| s.map_run(virt, round, mode, access)) {
                // Only with corrupt tables. The pages this call mapped go again, shot down,
                // and the frames it did not reach give back their counts.
                let stopped = virt + mapped as u64 * PAGE;
                let _ = self.unmap_range(base, stopped);
                let reached = ((stopped - base) / PAGE) as usize;
                for frame in frames.iter().skip(reached) {
                    frame.inner.aliases.give_back(mode);
                }
                return Err(e.into());
            }
            virt += round.len() as u64 * PAGE;
        }
        Ok(())
    }

    /// Removes every mapping in the `pages` pages from `virt`, and gives back their frames.
    /// When it returns, no processor can reach them through this space, and a frame this
    /// space alone mapped is free. Pages not mapped are left alone. Safe with this space's
    /// threads running: one that touches a page after its translation is gone takes a fault.
    ///
    /// Must not be called holding a lock another processor may wait for with interrupts off.
    ///
    /// # Errors
    ///
    /// - [`ChangeError::Map`] for a range this space cannot have, before anything changes.
    /// - [`ChangeError::Stalled`]: a processor did not answer, and every page of the range is
    ///   unmapped all the same; the frames taken out after the stall are leaked.
    pub fn unmap(&self, virt: u64, pages: u64) -> Result<Change, ChangeError> {
        let end = user_range_end(virt, pages)?;
        let _turn = self.edit();
        self.unmap_range(virt, end)
    }

    /// [`unmap`](Self::unmap)'s change of `[virt, end)`, by a caller that has this space's turn
    /// to change (`editor`).
    fn unmap_range(&self, virt: u64, end: u64) -> Result<Change, ChangeError> {
        let taken = self.with_state(|s| core::mem::take(&mut s.spare));
        let mut unmapping = Unmapping {
            space: self,
            cursor: virt,
            end,
            taken,
            pages: 0,
        };
        let outcome = self.confirmation.change(&mut unmapping, |request| {
            crate::tlb::shootdown_space(self.root, request)
        });
        // Every record it took has been given back or lost by now: the vector is empty again.
        let Unmapping { taken, pages, .. } = unmapping;
        self.with_state(|s| s.spare = taken);
        Ok(Change::of(pages, outcome?))
    }

    /// Whether a change to this space is not known to have reached every processor: a
    /// shootdown stalled, and no flush has completed since (review finding 3).
    pub fn change_unconfirmed(&self) -> bool {
        self.confirmation.is_unconfirmed()
    }

    /// Gives every page mapped in the `pages` pages from `virt` the permissions `access`. When
    /// it returns, no processor can use the old ones. Pages not mapped are left alone. Safe
    /// with this space's threads running: one whose access the new permissions refuse takes a
    /// fault. Must not be called holding a lock as [`unmap`](Self::unmap) says.
    ///
    /// # Errors
    ///
    /// - [`ChangeError::Map`] for a range this space cannot have, before anything changes.
    /// - [`ChangeError::WriteAndExecute`], before anything changes, if a shared frame would then
    ///   be writable through one mapping and executable through another, in any space, or if it
    ///   would go from writable to executable or back in one step (review finding 5).
    /// - [`ChangeError::Stalled`]: a processor did not answer, and every page of the range has
    ///   its new permissions all the same; the old ones stay counted until a later flush.
    pub fn protect(&self, virt: u64, pages: u64, access: Access) -> Result<Change, ChangeError> {
        let end = user_range_end(virt, pages)?;
        let _turn = self.edit();
        let mut reprotecting = Reprotecting {
            space: self,
            access,
            start: virt,
            end,
            round: virt,
            cursor: virt,
            pages: 0,
        };
        let cost = self.confirmation.change(&mut reprotecting, |request| {
            crate::tlb::shootdown_space(self.root, request)
        })?;
        Ok(Change::of(reprotecting.pages, cost))
    }

    /// Counts the mode `access` gives every shared frame mapped in `[start, end)`, or counts
    /// none of them and refuses (review finding 5). Called under `editor`, so the records do not
    /// change between the rounds.
    fn claim_modes(&self, start: u64, end: u64, access: Access) -> Result<(), ChangeError> {
        let mut cursor = start;
        loop {
            match self.with_state(|s| s.claim_modes(cursor, end, access)) {
                Ok(Some(next)) => cursor = next,
                Ok(None) => return Ok(()),
                Err(stopped) => {
                    self.with_state(|s| s.unclaim_modes(start, stopped, access));
                    return Err(ChangeError::WriteAndExecute);
                }
            }
        }
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
            let Some(mapping) = s.mapping(frame.virt) else {
                return false;
            };
            if mapping.frame.phys() != frame.phys {
                return false;
            }
            let p = direct_map(frame.phys);
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
            mapping.frame.release();
        }
        for frame in state.tables.frames.drain(..) {
            let _ = memory::free_frame(frame);
        }
        SPACES_FREED.fetch_add(1, Ordering::Relaxed);
    }
}
