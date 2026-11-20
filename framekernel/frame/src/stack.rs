//! Kernel stacks and FPU areas (CLAUDE.md §25, §28; ADR-0011 N6; ADR-0004 Amendment 1, A2).
//!
//! # Where a stack lives
//!
//! Every [`GuardedStack`] is in the kernel stacks' region, in a slot of its own: [`SLOT`] bytes of
//! address space with the stack's pages mapped at the top and the rest left unmapped. Running off
//! the bottom of a stack faults on that unmapped space, which is no other stack's (a frame larger
//! than a page is probed a page at a time, so none can step over it), and which costs no frame:
//! nothing in the direct map is split or unmapped for it. The region's page tables are made at
//! boot (`early`), so mapping a stack only fills in entries. The slots are `origin_sync::slots`;
//! this module maps and unmaps them.
//!
//! The region's base is random (ADR-0004 Amendment 2): placed once, when the kernel builds its
//! page tables ([`draw_region`]), 2 MiB-aligned within PML4 slots 384–447, from the random value
//! the loader's generator handed over for it (A5.1, decision item 166). This module runs neither
//! RDSEED nor RDRAND: the kernel's generator is the only code that does (ADR-0016), and it cannot
//! start this early. When the loader handed over no value the base is the fixed one,
//! `layout::STACK_REGION`, and the boot says so. After boot the kernel prints neither the base
//! nor an address in the region (A6): [`Region`]'s `Debug` leaves the base out.
//!
//! # Giving a stack back
//!
//! Dropping a stack waits for nobody, so a stack may be dropped under any lock: it is kept mapped
//! for the next stack of its size (up to [`IDLE`] of them), which gets it zeroed, or it is
//! retired. A retired stack's pages leave the tables, and its frames and slot are reused, only in
//! [`reclaim`], once a TLB shootdown has reached every processor. The scheduler reclaims where it
//! reaps exited threads.
//!
//! A stack's drop decides from records whether a processor may still be using it
//! (`cpu::stack_in_use`):
//! - **The caller's own stack pointer.**
//! - **Each processor's kernel-stack record**: the stack an interrupt or a system call from ring 3
//!   would land on, which the trap entry names from the running context's home (`cpu::resume`,
//!   TASKS item 77), so it never names a stack the table of contexts does not cover.
//! - **Each processor's table of contexts** (`cpu::CONTEXTS`, `origin_sync::contexts`). Every
//!   thread published to a processor has its home there, an address in its own stack, from its
//!   publication ([`GuardedStack::publish_first_frame`]) until it has ended and its processor has
//!   left the stack, or until its spawner withdraws it, never having made it runnable
//!   ([`GuardedStack::withdraw_first_frame`], TASKS item 75). A suspended context's saved frame is
//!   there too, which also covers a stack it is suspended on that is not its own, such as a
//!   measured stack ([`with_measured_stack`]).
//!
//! No switch writes a home. So a thread's own stack stays covered while it runs, while it is
//! suspended, while it runs a measured closure, and in the windows of a switch and of its exit,
//! however a processor's reads of the records interleave with switches (TASKS item 70; the review
//! of TASKS item 68, T68-2). A stack in any record is leaked rather than freed, which is the safe
//! failure for a safe caller that drops one too early.
//!
//! **Stacks held by their user** are in no record: the interrupt stacks, the generator's stacks,
//! and a measured stack while its closure runs on it. That is safe because only their holder can
//! drop them, and none does while running on them (T68-3).
//!
//! # Measuring a closure's stack
//!
//! [`with_measured_stack`] runs a closure on a stack of its own, painted beforehand with
//! [`stackpaint::PAINT`], and reads back how deep the closure went: the lowest word that no
//! longer holds the paint (TASKS item 68). It is how the kernel's own build measures what a
//! stack must hold, where a sample taken at chosen points can only miss the deepest one.

use alloc::alloc::{Layout, alloc, dealloc};
use core::arch::asm;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use origin_paging::PAGE;
use origin_paging::layout::{self, STACK_REGION, STACK_REGION_SIZE};
pub use origin_sync::slots::State;
use origin_sync::slots::{Counts, Slots, Taken};

use crate::cpu::{self, CpuIndex, without_interrupts};
use crate::descriptors::{KERNEL_CODE, KERNEL_DATA, USER_CODE, USER_DATA};
use crate::memory::{self, OwnedFrame};
use crate::stackpaint::{self, PAINT};

/// The address space each stack has: the stack at its top, unmapped space below.
pub const SLOT: u64 = layout::STACK_SLOT;
/// Stacks the region holds at boot, its floor: 256 MiB of slots.
const SLOTS: usize = (STACK_REGION_SIZE / SLOT) as usize;
/// The most processors the region's reserved address space has room for (ADR-0008 Amendment 1,
/// D3): 262,144.
pub const MAX_PROCESSORS: usize = layout::STACK_REGION_MAX_PROCESSORS as usize;
/// Freed stacks kept mapped for the next stack of their size.
pub const IDLE: usize = 16;
/// The largest stack: half its slot, so that at least as much unmapped space lies below it.
pub const MAX_STACK: usize = (SLOT / 2) as usize;
/// The pages of the largest stack.
const MAX_PAGES: usize = MAX_STACK / PAGE as usize;
const _: () = assert!(
    SLOT * SLOTS as u64 == STACK_REGION_SIZE,
    "the slots fill the region the kernel made tables for"
);
const _: () = assert!(
    MAX_PAGES as u64 <= origin_sync::shootdown::MAX_LISTED_PAGES,
    "a stack's shootdown names its pages"
);

/// The region's slots: the floor's, and those [`grow`] adds.
static REGION: Slots<SLOTS, IDLE> = Slots::new();
/// The region's size in bytes: its floor until [`grow`] adds to it, once, before any
/// application processor starts. Every address check reads it ([`region_size`]).
static REGION_SIZE: AtomicU64 = AtomicU64::new(STACK_REGION_SIZE);

/// The region's size in bytes, its floor until it grows.
pub fn region_size() -> u64 {
    REGION_SIZE.load(Ordering::Acquire)
}

/// The region's size for `processors` indexed processors (`layout::stack_region_size_for`).
fn size_for(processors: usize) -> u64 {
    layout::stack_region_size_for(processors as u64)
}

/// The processors a region of `size` bytes has room for (`layout::stack_region_room`).
fn room_in(size: u64) -> usize {
    layout::stack_region_room(size) as usize
}

/// What [`grow`] reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Growth {
    /// How many processors the region has room for: the floor's 64 if the tables or the slots
    /// could not be had, and at most [`MAX_PROCESSORS`].
    pub room: usize,
    /// Whether it stayed short for want of memory: the run of frames for its page tables or the
    /// storage for its slots (review finding F12 of 2026-11-14).
    pub short_of_memory: bool,
}

/// Grows the region for `processors` indexed processors (ADR-0008 Amendment 1, D3), once, on the
/// boot processor after the MADT is read, before the per-CPU tables are sized for its room and
/// before any application processor starts: the page tables for what lies past the floor, from
/// one run of frames taken now, then the slots.
pub(crate) fn grow(processors: usize) -> Growth {
    let size = size_for(processors);
    let floor = room_in(STACK_REGION_SIZE);
    let short = |short_of_memory| Growth {
        room: floor,
        short_of_memory,
    };
    if size <= STACK_REGION_SIZE || region_size() != STACK_REGION_SIZE {
        return Growth {
            room: room_in(region_size()).max(floor),
            short_of_memory: false,
        };
    }
    let base = region().base;
    match crate::vm::grow_stack_region(base, STACK_REGION_SIZE, size) {
        Ok(()) => {}
        Err(origin_paging::MapError::OutOfMemory) => return short(true),
        Err(_) => return short(false),
    }
    match without_interrupts(|| REGION.grow((size / SLOT) as usize)) {
        Ok(slots) if slots as u64 * SLOT == size => {
            REGION_SIZE.store(size, Ordering::Release);
            Growth {
                room: room_in(size),
                short_of_memory: false,
            }
        }
        Err(origin_sync::slots::Grow::NoMemory) => short(true),
        _ => short(false),
    }
}

/// Where the kernel stacks' region is (ADR-0004 Amendment 2).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// The region's base: 2 MiB-aligned, with the whole region in PML4 slots 384–447. Never
    /// printed after boot (A6).
    pub base: u64,
    /// Whether the loader's random value placed the base (A5.1). `false`: the loader handed
    /// over none, and the region is at its fixed base.
    pub drawn: bool,
}

/// Shows whether the base was drawn, never the base (A6).
impl core::fmt::Debug for Region {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Region")
            .field("drawn", &self.drawn)
            .finish_non_exhaustive()
    }
}

/// The region's base once `early` has placed it, 0 before; written once ([`place`]). A word of
/// its own under a name of its own, so that the boot tests can read it through QEMU's monitor
/// and check that boots place the region apart (ADR-0004 Amendment 2, A6.4), while the kernel
/// prints nothing of it.
static STACK_REGION_BASE: AtomicU64 = AtomicU64::new(0);
/// Whether the loader's value placed it. Written before the base is published.
static STACK_REGION_DRAWN: AtomicBool = AtomicBool::new(false);

/// Draws the region's base (ADR-0004 Amendment 2, A4, A5.1 and A5.2): from the random value the
/// loader handed over for it, which this takes, once (`random::take_stack_value`), or the fixed
/// base when the loader handed over none. No instruction is run here. For `early`, which makes
/// the region's tables there and then records it ([`place`]).
pub(crate) fn draw_region() -> Region {
    match crate::random::take_stack_value() {
        Some(random) => Region {
            base: layout::stack_region_base(random),
            drawn: true,
        },
        None => Region {
            base: STACK_REGION,
            drawn: false,
        },
    }
}

/// Records where the region is, once its page tables are made: before then, and if this has run
/// already, nothing changes.
pub(crate) fn place(region: Region) -> bool {
    if STACK_REGION_BASE.load(Ordering::Relaxed) != 0 {
        return false;
    }
    STACK_REGION_DRAWN.store(region.drawn, Ordering::Relaxed);
    // Release: whoever reads the base reads whether it was drawn too.
    STACK_REGION_BASE
        .compare_exchange(0, region.base, Ordering::Release, Ordering::Relaxed)
        .is_ok()
}

/// Where the region is: at its fixed base until `early` has placed it, which it does before the
/// first stack is made.
pub fn region() -> Region {
    match STACK_REGION_BASE.load(Ordering::Acquire) {
        0 => Region {
            base: STACK_REGION,
            drawn: false,
        },
        base => Region {
            base,
            drawn: STACK_REGION_DRAWN.load(Ordering::Relaxed),
        },
    }
}

/// The first address of slot `slot`.
fn slot_base(slot: usize) -> u64 {
    region().base + slot as u64 * SLOT
}

/// The lowest of the top `pages` pages of slot `slot`: where a stack of that many pages starts.
fn stack_low(slot: usize, pages: u32) -> u64 {
    slot_base(slot) + SLOT - u64::from(pages) * PAGE
}

/// The slot holding `addr`, if `addr` is in the stack region.
fn slot_of(addr: u64) -> Option<usize> {
    let base = region().base;
    if addr < base || addr - base >= region_size() {
        return None;
    }
    Some(((addr - base) / SLOT) as usize)
}

/// One stack's slot, as [`GuardedStack::slot`] names it: kept after the stack is gone, to ask
/// about the slot later. It is not an address. A stack's [`GuardedStack::top`] is the base of the
/// next slot, so a question asked by address about "the slot of the top" asks about another
/// stack's slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackSlot(usize);

/// Whether `slot` is free: nothing is mapped there, and no processor holds a translation of what
/// was. For a proof that a freed stack's reclaim has completed, whichever processor did it.
pub fn slot_is_free(slot: StackSlot) -> bool {
    without_interrupts(|| REGION.state(slot.0)) == Some(State::Free)
}

/// The slot holding `addr`, if `addr` is in the stack region. A stack's last byte, its
/// [`GuardedStack::top`] less one, is in its slot; its top is in the next one.
pub fn slot_holding(addr: u64) -> Option<StackSlot> {
    slot_of(addr).map(StackSlot)
}

/// The top of the slot holding `addr`, if `addr` is in the stack region: past it lies the next
/// slot's unmapped space. For the backtrace (`cpu::capture_backtrace`).
pub fn slot_top(addr: u64) -> Option<u64> {
    slot_of(addr).map(|slot| slot_base(slot) + SLOT)
}

/// A kernel stack in the stack region, with unmapped space below it. Owned; given back on drop.
/// Its `Debug` names its slot and size, not its first frame, which is an address in the region
/// (ADR-0004 Amendment 2, A6; TASKS item 70, Q8).
pub struct GuardedStack {
    /// Its slot in the stack region.
    slot: usize,
    /// The usable bytes: whole pages, at the top of the slot.
    bytes: usize,
    /// The first frame built on this stack, not yet published to a processor.
    first_frame: Option<u64>,
    /// The processor and frame this stack's first frame was published to, for a withdrawal by
    /// its spawner ([`Self::withdraw_first_frame`]). Kept for the stack's life: once the
    /// scheduler holds the stack, nothing withdraws it.
    published: Option<(CpuIndex, u64)>,
}

impl core::fmt::Debug for GuardedStack {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GuardedStack")
            .field("slot", &self.slot)
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

impl GuardedStack {
    /// A zeroed stack of `bytes` usable bytes, rounded up to whole pages, or `None` if `bytes` is
    /// 0 or more than [`MAX_STACK`], every slot is taken, or there are no frames for it. Waits for
    /// no other processor.
    pub fn new(bytes: usize) -> Option<Self> {
        if bytes == 0 || bytes > MAX_STACK {
            return None;
        }
        let bytes = bytes.next_multiple_of(PAGE as usize);
        let pages = u32::try_from(bytes / PAGE as usize).ok()?;
        let Taken { slot, mapped } = without_interrupts(|| REGION.take(pages))?;
        if !mapped && !map_fresh(slot, pages) {
            return None;
        }
        let mut stack = Self {
            slot,
            bytes,
            first_frame: None,
            published: None,
        };
        if mapped {
            // A stack kept for reuse: as its last holder left it.
            stack.bytes_mut().fill(0);
        }
        Some(stack)
    }

    /// The page just below the stack: the first an overflow reaches, unmapped like the rest of
    /// the slot below it.
    pub fn guard_addr(&self) -> u64 {
        self.top() - self.bytes as u64 - PAGE
    }

    /// The first address of the stack's slot: the bottom of the unmapped space below it.
    pub fn slot_base(&self) -> u64 {
        slot_base(self.slot)
    }

    /// The usable bytes.
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    /// The first address past the stack, 16-byte aligned: the top of its slot, which is the base
    /// of the next slot.
    pub fn top(&self) -> u64 {
        slot_base(self.slot) + SLOT
    }

    /// Its slot, to ask [`slot_is_free`] about once the stack is gone.
    pub const fn slot(&self) -> StackSlot {
        StackSlot(self.slot)
    }

    /// Writes a new thread's first frame on this stack, starting at `start` with every other
    /// register zero, and returns its address (the value the scheduler's run queue keeps). It
    /// is recorded as resumable only by [`publish_first_frame`](Self::publish_first_frame), before
    /// the scheduler makes the thread runnable, and a spawn refused after that withdraws it
    /// ([`withdraw_first_frame`](Self::withdraw_first_frame)), so a refused spawn leaves nothing
    /// behind.
    pub fn build_first_frame(&mut self, start: ThreadStart) -> Option<u64> {
        let base = self.bytes_mut().as_ptr().addr() as u64;
        let rsp = self.top() - 8;
        let frame_at = rsp - (FRAME_WORDS as u64) * 8;
        let mut frame = [0u64; FRAME_WORDS];
        frame[NEW_RFLAGS_INDEX] = NEW_RFLAGS;
        match start {
            ThreadStart::Kernel { entry, arg } => {
                frame[RDI_INDEX] = arg;
                frame[RIP_INDEX] = (entry as *const ()).addr() as u64;
                frame[CS_INDEX] = u64::from(KERNEL_CODE);
                frame[RSP_INDEX] = rsp;
                frame[SS_INDEX] = u64::from(KERNEL_DATA);
            }
            // The common return path enters ring 3 from this frame: `swapgs` when the
            // frame's CS is a user one, then IRETQ, which leaves the segment registers usable
            // at CPL 3.
            ThreadStart::User {
                rip,
                rsp: user_rsp,
                rdi,
            } => {
                frame[RDI_INDEX] = rdi;
                frame[RIP_INDEX] = rip;
                frame[CS_INDEX] = u64::from(USER_CODE);
                frame[RSP_INDEX] = user_rsp;
                frame[SS_INDEX] = u64::from(USER_DATA);
            }
        }
        // The thread starts with RSP = top - 8 holding a zero return address, as if its
        // entry had been called (System V: RSP + 8 is 16-byte aligned there); the word at
        // the top stays zero because the stack was zeroed when it was made.
        let offset = usize::try_from(frame_at - base).ok()?;
        let dest = self.bytes_mut();
        for (i, word) in frame.iter().enumerate() {
            let at = offset + i * 8;
            dest.get_mut(at..at + 8)?
                .copy_from_slice(&word.to_le_bytes());
        }
        self.first_frame = Some(frame_at);
        Some(frame_at)
    }

    /// Records the frame built by [`build_first_frame`](Self::build_first_frame) as resumable
    /// on processor `cpu`: the trap entry resumes only frames it has saved or a built first
    /// frame published here (and consumed by the first resume). The scheduler calls this before
    /// it makes the thread runnable, so that no run queue ever names a frame the processor's
    /// table does not hold (TASKS item 75). `false` if there is no built frame or the table is
    /// full.
    pub fn publish_first_frame(&mut self, cpu: CpuIndex) -> bool {
        let Some(frame) = self.first_frame.take() else {
            return false;
        };
        let published = cpu::publish_context(cpu, frame);
        if published {
            self.published = Some((cpu, frame));
        }
        published
    }

    /// Withdraws this stack's published first frame from its processor's table, for a spawn
    /// refused after the publication, before the thread was made runnable (TASKS item 75): the
    /// stack is then in no record, and its drop gives it back. Only the spawner calls it, before it
    /// hands the stack to the scheduler. `false` if nothing was published, or the table no longer
    /// holds the frame as never resumed (`Contexts::withdraw`); the home, if any, then stays, and
    /// the drop leaks the stack rather than free one a processor may run on.
    pub fn withdraw_first_frame(&mut self) -> bool {
        self.published
            .take()
            .is_some_and(|(cpu, frame)| cpu::withdraw_context(cpu, frame))
    }

    /// The stack's memory: the running thread's stack, or one being built. Only the framework
    /// hands it out, so nothing else runs on these bytes while the holder writes them.
    fn bytes_mut(&mut self) -> &mut [u8] {
        let start =
            core::ptr::with_exposed_provenance_mut::<u8>((self.top() - self.bytes as u64) as usize);
        // SAFETY: `[top - bytes, top)` is this stack's pages, mapped read/write in the stack region
        // for as long as the stack is held (`new` mapped them, or took them mapped, and only `Drop`
        // gives them back), and `&mut self` means nothing else uses them: the thread that will run
        // on the stack has not been started (or has ended).
        unsafe { core::slice::from_raw_parts_mut(start, self.bytes) }
    }
}

impl Drop for GuardedStack {
    fn drop(&mut self) {
        let top = self.top();
        // Leak rather than give back memory a processor could still run on (see the module docs).
        // This and `slot_in_use` must decide through the same `in_use`: the boot proofs ask
        // `slot_in_use` (TASKS items 68 and 70), and the kernel stacks proof's W3 shows that this
        // drop gives back a stack the records no longer hold.
        if in_use(top - self.bytes as u64, top) {
            without_interrupts(|| REGION.leak(self.slot));
            return;
        }
        // Kept mapped or retired: either way nothing is unmapped here, so this waits for nobody.
        without_interrupts(|| REGION.give_back(self.slot, true));
    }
}

/// Maps `pages` fresh zeroed frames at the top of slot `slot`, which [`Slots::take`] has just
/// handed out with nothing mapped. On failure the slot goes back: abandoned if nothing was mapped
/// in it, retired for a reclaim if something was. Returns whether every page is mapped.
fn map_fresh(slot: usize, pages: u32) -> bool {
    let mut frames: [Option<OwnedFrame>; MAX_PAGES] = [const { None }; MAX_PAGES];
    let mut mapped = 0;
    if let Some(wanted) = frames.get_mut(..pages as usize) {
        for frame in wanted.iter_mut() {
            *frame = memory::zeroed_frame();
        }
        if wanted.iter().all(Option::is_some) {
            mapped = crate::vm::map_stack_pages(stack_low(slot, pages), wanted);
        }
        if mapped == wanted.len() {
            return true;
        }
    }
    // What was not mapped goes back now; what was is the slot's, for its reclaim.
    for frame in frames.into_iter().flatten() {
        let _ = memory::free_frame(frame);
    }
    without_interrupts(|| {
        if mapped == 0 {
            REGION.abandon(slot);
        } else {
            REGION.give_back(slot, false);
        }
    });
    false
}

/// What [`reclaim`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reclaimed {
    /// Retired stacks whose pages left the tables, each with a shootdown, and whose frames and
    /// slots are free again.
    pub stacks: u32,
    /// The interrupts those shootdowns sent to other processors.
    pub interrupts: u32,
    /// Retired stacks whose shootdown stalled: their slots and frames are never reused.
    pub lost: u32,
}

/// Gives back what every retired stack holds: its pages leave the kernel's tables, a shootdown
/// drops their translations from every processor, and only then are its frames and its slot
/// free. It waits for every processor, so never call it under a lock another processor may wait
/// for with interrupts off: the scheduler calls it where it reaps exited threads. With no stack
/// retired it takes no lock.
pub fn reclaim() -> Reclaimed {
    let mut done = Reclaimed::default();
    loop {
        let outcome = without_interrupts(|| {
            REGION.reclaim(
                |slot, pages| {
                    let mut frames: [Option<OwnedFrame>; MAX_PAGES] = [const { None }; MAX_PAGES];
                    if let Some(taken) = frames.get_mut(..pages as usize) {
                        crate::vm::unmap_stack_pages(stack_low(slot, pages), taken);
                    }
                    frames
                },
                |slot, pages| {
                    crate::tlb::shootdown_kernel_pages(stack_low(slot, pages), u64::from(pages))
                },
                |frames| {
                    for frame in frames.into_iter().flatten() {
                        let _ = memory::free_frame(frame);
                    }
                },
            )
        });
        match outcome {
            None => return done,
            Some(Ok(completed)) => {
                done.stacks += 1;
                done.interrupts += completed.processors;
            }
            Some(Err(_)) => done.lost += 1,
        }
    }
}

/// Retires every freed stack that is kept mapped for reuse, so that the next [`reclaim`] gives
/// back what they hold; returns how many. For a check that nothing is left over.
pub fn trim() -> usize {
    without_interrupts(|| REGION.trim())
}

/// How many stacks are held, kept for reuse, retired or lost, and the pages they map.
pub fn counts() -> Counts {
    without_interrupts(|| REGION.counts())
}

/// Whether a processor may be using the stack `[low, top)`: whether one of the records of the
/// module docs holds it (`cpu::stack_in_use`). Only a stack in none of them is given back.
fn in_use(low: u64, top: u64) -> bool {
    cpu::stack_in_use(low, top, top)
}

/// What a stack's drop would decide, from the records, about the stack in slot `slot`: whether
/// one of them holds it (see the module docs). For proofs that a thread's own stack stays held
/// while the thread is switched out on a measured stack (TASKS item 68, A1), and in the windows of
/// its switch-out and its exit (TASKS item 70). It is not a test of
/// whether a processor runs on the slot: a stack held by its user, such as a measured stack while
/// its closure runs, is in no record and answers `false` (T68-3). It names no address.
pub fn slot_in_use(slot: StackSlot) -> bool {
    let low = slot_base(slot.0);
    in_use(low, low + SLOT)
}

/// Whether some processor's table of contexts covers the stack in slot `slot`: a context's home
/// or saved frame there. Part of what [`slot_in_use`] asks, without the caller's stack pointer
/// and the kernel-stack records. For the kernel stacks proof's switch windows, where since TASKS
/// item 77 the held processor's kernel-stack record still names the held thread's stack, so
/// that what [`slot_in_use`] answers there would not show its home word (the review of TASKS
/// items 75, 77 and 81, R77-1). It names no address.
pub fn slot_in_contexts(slot: StackSlot) -> bool {
    let low = slot_base(slot.0);
    cpu::contexts_cover(low, low + SLOT)
}

/// The state of slot `slot` in the stack region's table, or `None` if there is no such slot. For
/// a proof that a stack was given back rather than leaked (`State::Lost`; TASKS item 68, T68-1).
/// It names no address.
pub fn slot_state(slot: StackSlot) -> Option<State> {
    without_interrupts(|| REGION.state(slot.0))
}

/// What [`with_measured_stack`] gives back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measured<R> {
    /// The closure's value.
    pub value: R,
    /// How deep the closure's stack was written, in bytes from its top (see
    /// [`with_measured_stack`]).
    pub depth: usize,
    /// The stack's size in bytes: what was asked for, rounded up to whole pages.
    pub size: usize,
}

/// Runs `f` on a stack of its own of `bytes` bytes (rounded up to whole pages), and returns its
/// value with how deep `f` went on that stack. `None` if no stack can be had (`bytes` is 0 or
/// more than [`MAX_STACK`], every slot is taken, or there are no frames), and then `f` has not
/// run.
///
/// The stack is a [`GuardedStack`], with the unmapped space below it that every kernel stack
/// has: an access past its lowest byte faults, and a push there is a double fault, which the
/// kernel reports before it halts. Every 8-byte word of it is painted with
/// [`stackpaint::PAINT`] before `f` runs, and afterwards the scan finds the lowest word that no
/// longer holds it ([`stackpaint::depth`]).
///
/// **What the depth is** (TASKS item 68, Q2): the bytes from the stack's top down to that word,
/// the 8-byte return address of the switch's own call included. It is exact for every word
/// written while `f` ran, and a lower bound on the deepest access: it misses a word that was
/// reserved and only read, or touched without its value changing, or written with exactly the
/// paint's value (which is why the paint is neither a canonical address nor a small integer).
/// Safe Rust reads no stack word it has not written, so a word reserved and only read does not
/// arise in safe code. The kernel's target uses no red zone, so nothing is written below the
/// stack pointer, and the deepest access is in the deepest frame. So the room the stack had
/// left, `size - depth`, is an upper bound, by at most the part of the deepest frame below its
/// lowest write. In this build that part is at most one page. As a frame's stack pointer moves
/// past each page but its last, the page is probed with a zero write (`subq $0x1000, %rsp;
/// movq $0x0, (%rsp)`, or the same in a loop), which the paint counts. The last page or less of a
/// frame is not probed, since a move of one page or less cannot pass the unmapped space below a
/// stack, and nothing is allocated on the stack dynamically. The probes are this build's, read
/// from the kernel image (TASKS item 68's result), and a toolchain upgrade reads them again (T68-8).
///
/// **Interrupts** (Q3): the caller's interrupt state is kept. An interrupt or exception taken
/// while `f` runs is pushed on this stack, since its gate names no IST and the privilege level
/// does not change (Intel SDM 325384-093, Vol. 3A §7.14.2), and its handlers' frames count in
/// the depth: a stack `f` runs on must hold them too. NMI, machine checks, double faults and
/// debug exceptions run on stacks of their own and do not count.
///
/// **Where it may be called** (Q4):
/// - in the boot flow on the boot processor, before any thread exists;
/// - in the kernel code of any thread, with interrupts on or off: the thread may be switched
///   out while `f` runs and resumed on this stack, on another processor if the scheduler ever
///   moves it (today's does not). The thread's own stack stays in use meanwhile, by its home in
///   its processor's table of contexts (the module docs; TASKS item 70, which replaced the table
///   of stacks left that TASKS item 68's ruling A1 added), so dropping it leaks it rather than
///   freeing it.
///
/// Not in an interrupt or exception handler or on an interrupt stack, not with a spin lock held
/// (it maps pages and takes the region's slot table, as [`GuardedStack::new`] does), and not
/// before the stack region and the frame allocator exist. Calls may nest. `f` must return: a
/// thread that ends inside it leaks this stack, which is safe. Its own stack is given back once
/// it is reaped, since the thread and its processor have left it. A backtrace captured inside `f` stops at this stack's top: the walk
/// reads no further than the top of the slot it starts in (`cpu::capture_backtrace`), so it never
/// reaches the caller's frames.
///
/// The stack's contents are not wiped when it is given back: like any kernel stack, it is zeroed
/// before its next holder gets it.
pub fn with_measured_stack<R>(bytes: usize, f: impl FnOnce() -> R) -> Option<Measured<R>> {
    let stack = GuardedStack::new(bytes)?;
    let size = stack.bytes();
    let words = size / 8;
    let low = ptr::with_exposed_provenance_mut::<u64>((stack.top() - size as u64) as usize);
    for word in 0..words {
        // SAFETY: `low` is the stack's lowest byte, page-aligned, and `word` is below `words`, so
        // the address is inside `[top - size, top)`: this stack's pages, mapped read/write for as
        // long as `stack` is held. Nothing runs on the stack yet: it was taken just now.
        unsafe { low.wrapping_add(word).write_volatile(PAINT) };
    }
    let mut f = Some(f);
    let mut value = None;
    let mut run = || {
        if let Some(f) = f.take() {
            value = Some(f());
        }
    };
    // SAFETY: `stack` is held by this call until after the scan below, and nothing runs on it (it
    // was taken and painted here, and this code is not on it). The stack this call leaves stays
    // in use while `run` runs: it is a thread's own stack, which its home keeps in use (the module
    // docs), or a stack held by its user (an outer measured stack, or a boot flow's stack, which
    // nothing gives back).
    unsafe { run_on(&stack, &mut run) };
    let depth = stackpaint::depth(words, |word| {
        // SAFETY: as for the paint: inside the stack, which is still held. `f` has returned and
        // the stack pointer is this call's again, so nothing runs on the stack any more; an
        // interrupt or exception taken on it has returned before `f` did.
        unsafe { low.wrapping_add(word).read_volatile() }
    });
    drop(stack);
    value.map(|value| Measured { value, depth, size })
}

/// What [`run_on`] calls on the other stack.
struct Call<'a> {
    run: &'a mut dyn FnMut(),
}

/// The first frame on the other stack.
extern "sysv64" fn enter(call: *mut Call<'_>) {
    // SAFETY: `run_on` passes a pointer to a `Call` that lives in its own frame for the whole of
    // this call, and nothing else refers to it meanwhile.
    let call = unsafe { &mut *call };
    (call.run)();
}

/// Runs `run` on `stack` and comes back.
///
/// # Safety
///
/// Nothing runs on `stack`, and nothing will but `run` and the interrupt and exception handlers
/// taken while it runs, until this returns: the caller holds `stack` and is not on it. The stack
/// the caller is leaving stays in use while `run` runs, as [`with_measured_stack`] says.
unsafe fn run_on(stack: &GuardedStack, run: &mut dyn FnMut()) {
    let mut call = Call { run };
    let top = stack.top();
    // SAFETY: `stack` is mapped, writable memory of `stack.bytes()` bytes ending at its top,
    // which is 16-byte aligned (the top of a slot), with unmapped space below it, and by the
    // contract above nothing else uses it. The block keeps this stack pointer in R12, which is
    // callee-saved, loads the other stack's top and calls `enter`, an ordinary System V function
    // that keeps R12, RBX, RBP, R13 to R15 and the direction flag as it found them; the call is
    // made with RSP 16-byte aligned, as the psABI asks. It then puts the stack pointer back, so it
    // ends where it began. An interrupt or exception taken while `enter` runs saves and restores
    // every register, R12 among them, whatever processor it resumes on. The flags are not kept,
    // and the block does not say they are (decision item 110). It cannot unwind: the kernel
    // aborts on a panic.
    unsafe {
        asm!(
            "mov r12, rsp",
            "mov rsp, {top}",
            "call {enter}",
            "mov rsp, r12",
            enter = sym enter,
            top = in(reg) top,
            inout("rdi") ptr::from_mut(&mut call) => _,
            out("r12") _,
            clobber_abi("sysv64"),
        );
    }
}

/// Words in a saved frame: 15 registers, vector, error code, RIP, CS, RFLAGS, RSP, SS.
const FRAME_WORDS: usize = 22;
const RDI_INDEX: usize = 9;
const RIP_INDEX: usize = 17;
const CS_INDEX: usize = 18;
const NEW_RFLAGS_INDEX: usize = 19;
const RSP_INDEX: usize = 20;
const SS_INDEX: usize = 21;
/// RFLAGS of a new thread: IF (interrupts on) and the always-one bit 1.
const NEW_RFLAGS: u64 = 0x202;

/// Where a new thread starts.
#[derive(Clone, Copy, Debug)]
pub enum ThreadStart {
    /// In kernel mode at `entry(arg)`.
    Kernel {
        entry: extern "sysv64" fn(u64) -> !,
        arg: u64,
    },
    /// In ring 3 at `rip`, with the stack pointer `rsp` and `rdi` in RDI.
    User { rip: u64, rsp: u64, rdi: u64 },
}

/// A kernel page that can be unmapped and mapped back: what the TLB boot proof caches, unmaps
/// and probes on every processor. It is in the stack region, in a slot of its own like a
/// one-page stack, so taking it away splits nothing in the direct map. It is never a Rust object
/// anyone holds a reference to: the holder sees it only through [`crate::cpu::probe_read`], so
/// taking the mapping away breaks nothing. Dropping it gives the slot back.
#[derive(Debug)]
pub struct GuardablePage {
    /// Its slot: the page at the top, nothing below.
    slot: usize,
    /// The page has left the tables and every processor's TLB, and its frame is free.
    unmapped: bool,
}

impl GuardablePage {
    /// A new mapped page, or `None`. What it holds is never read: only probes touch it.
    pub fn new() -> Option<Self> {
        let Taken { slot, mapped } = without_interrupts(|| REGION.take(1))?;
        if !mapped && !map_fresh(slot, 1) {
            return None;
        }
        Some(Self {
            slot,
            unmapped: false,
        })
    }

    /// The page's address, for probes.
    pub fn addr(&self) -> u64 {
        stack_low(self.slot, 1)
    }

    /// Takes the page out of the tables, if it is still there, and its frame with it; returns
    /// the frame. The caller shoots down before freeing it.
    fn take_out(&self) -> Option<OwnedFrame> {
        if self.unmapped {
            return None;
        }
        let mut frame = [None];
        crate::vm::unmap_stack_pages(self.addr(), &mut frame);
        let [frame] = frame;
        frame
    }

    /// Unmaps it on every processor with a single-page shootdown. `false` if it was not mapped.
    pub fn unmap(&mut self) -> bool {
        let Some(frame) = self.take_out() else {
            return false;
        };
        crate::tlb::shootdown(self.addr());
        let _ = memory::free_frame(frame);
        self.unmapped = true;
        true
    }

    /// Unmaps several pages with one full flush on every processor. `false` if one of them was
    /// not mapped, or there are more than eight.
    pub fn unmap_all(pages: &mut [&mut Self]) -> bool {
        let mut frames: [Option<OwnedFrame>; 8] = [const { None }; 8];
        let Some(taken) = frames.get_mut(..pages.len()) else {
            return false;
        };
        for (frame, page) in taken.iter_mut().zip(pages.iter()) {
            *frame = page.take_out();
        }
        crate::tlb::shootdown_all();
        let all = taken.iter().all(Option::is_some);
        for frame in frames.into_iter().flatten() {
            let _ = memory::free_frame(frame);
        }
        for page in pages.iter_mut() {
            page.unmapped = true;
        }
        all
    }
}

impl Drop for GuardablePage {
    fn drop(&mut self) {
        without_interrupts(|| {
            if self.unmapped {
                // Nothing is mapped in the slot, and no processor holds a translation of it.
                REGION.abandon(self.slot);
            } else {
                REGION.give_back(self.slot, false);
            }
        });
    }
}

/// A processor's FPU/SIMD save area for one thread. Owned; freed on drop.
#[derive(Debug)]
pub struct FpuArea {
    /// The area's address, 64-byte aligned, never 0.
    base: usize,
    size: usize,
}

impl FpuArea {
    /// A clean area of the processor's current size: a restore from it yields the FPU's
    /// initial state. `None` if there is no memory.
    pub fn new() -> Option<Self> {
        let size = crate::cpu::fpu_area_size().max(crate::features::FXSAVE_SIZE);
        let layout = Layout::from_size_align(size, 64).ok()?;
        // SAFETY: `size` is non-zero (`FXSAVE_SIZE` is 512).
        let p = unsafe { alloc(layout) };
        if p.is_null() {
            return None;
        }
        // SAFETY: `p` is a fresh, writable buffer of `size` >= `fpu_area_size()` bytes.
        unsafe { crate::cpu::fpu_init_area(p.expose_provenance() as u64) };
        Some(Self {
            base: p.expose_provenance(),
            size,
        })
    }

    /// Saves this processor's FPU/SIMD state into the area.
    pub fn save(&mut self) {
        // SAFETY: SSE/XSAVE are on (`enable_simd` runs before any thread); the area was
        // allocated `fpu_area_size()` bytes, 64-aligned, and `&mut self` means it is not
        // being restored from or saved to concurrently.
        unsafe { crate::cpu::fpu_save(self.base as u64) };
    }

    /// Loads this processor's FPU/SIMD state from the area.
    pub fn restore(&self) {
        // SAFETY: as for `save`; the area only ever holds what `fpu_init_area` or `fpu_save`
        // wrote, so XRSTOR accepts it.
        unsafe { crate::cpu::fpu_restore(self.base as u64) };
    }
}

impl Drop for FpuArea {
    fn drop(&mut self) {
        if let Ok(layout) = Layout::from_size_align(self.size, 64) {
            // SAFETY: allocated by `new` with this layout, not used any more: `&mut self`.
            unsafe { dealloc(core::ptr::with_exposed_provenance_mut(self.base), layout) };
        }
    }
}
