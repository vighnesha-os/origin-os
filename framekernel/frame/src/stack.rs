//! Kernel stacks and FPU areas (CLAUDE.md §25, §28; ADR-0011 N6; ADR-0004 Amendment 1, A2).
//!
//! # Where a stack lives
//!
//! Every [`GuardedStack`] is in the kernel stacks' region (`origin_paging::layout::STACK_REGION`,
//! PML4 slot 384), in a slot of its own: [`SLOT`] bytes of address space with the stack's pages
//! mapped at the top and the rest left unmapped. Running off the bottom of a stack faults on that
//! unmapped space, which is no other stack's (a frame larger than a page is probed a page at a
//! time, so none can step over it), and which costs no frame: nothing in the direct map is split
//! or unmapped for it. The region's page tables are made at boot (`early`), so mapping a stack
//! only fills in entries. The slots are `origin_sync::slots`; this module maps and unmaps them.
//!
//! # Giving a stack back
//!
//! Dropping a stack waits for nobody, so a stack may be dropped under any lock: it is kept mapped
//! for the next stack of its size (up to [`IDLE`] of them), which gets it zeroed, or it is
//! retired. A retired stack's pages leave the tables, and its frames and slot are reused, only in
//! [`reclaim`], once a TLB shootdown has reached every processor. The scheduler reclaims where it
//! reaps exited threads.
//!
//! A stack still in use is never given back: the framework knows which stacks are in use because
//! every kernel stack a processor runs on has its top in some processor's TSS
//! (`cpu::set_kernel_stack`) and every suspended thread has a saved frame on its stack that the
//! trap entry recorded (`cpu::record_saved_frame`). A stack in either is leaked rather than freed,
//! which is the safe failure for a safe caller that drops one too early.

use alloc::alloc::{Layout, alloc, dealloc};

use origin_paging::PAGE;
use origin_paging::layout::{STACK_REGION, STACK_REGION_SIZE};
use origin_sync::slots::{Counts, Slots, Taken};

use crate::cpu::{self, CpuIndex, without_interrupts};
use crate::descriptors::{KERNEL_CODE, KERNEL_DATA, USER_CODE, USER_DATA};
use crate::memory::{self, OwnedFrame};

/// The address space each stack has: the stack at its top, unmapped space below.
pub const SLOT: u64 = 128 * 1024;
/// Stacks the region holds at once: 32 for each processor the kernel indexes.
const SLOTS: usize = 32 * crate::limits::MAX_CPUS;
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

/// The region's slots.
static REGION: Slots<SLOTS, IDLE> = Slots::new();

/// The first address of slot `slot`.
const fn slot_base(slot: usize) -> u64 {
    STACK_REGION + slot as u64 * SLOT
}

/// The lowest of the top `pages` pages of slot `slot`: where a stack of that many pages starts.
const fn stack_low(slot: usize, pages: u32) -> u64 {
    slot_base(slot) + SLOT - pages as u64 * PAGE
}

/// The top of the slot holding `addr`, if `addr` is in the stack region: past it lies the next
/// slot's unmapped space. For the backtrace (`cpu::capture_backtrace`).
pub const fn slot_top(addr: u64) -> Option<u64> {
    if addr < STACK_REGION || addr >= STACK_REGION + STACK_REGION_SIZE {
        return None;
    }
    Some(addr - (addr - STACK_REGION) % SLOT + SLOT)
}

/// A kernel stack in the stack region, with unmapped space below it. Owned; given back on drop.
#[derive(Debug)]
pub struct GuardedStack {
    /// Its slot in the stack region.
    slot: usize,
    /// The usable bytes: whole pages, at the top of the slot.
    bytes: usize,
    /// The first frame built on this stack, not yet published to a processor.
    first_frame: Option<u64>,
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
        };
        if mapped {
            // A stack kept for reuse: as its last holder left it.
            stack.bytes_mut().fill(0);
        }
        Some(stack)
    }

    /// The page just below the stack: the first an overflow reaches, unmapped like the rest of
    /// the slot below it.
    pub const fn guard_addr(&self) -> u64 {
        self.top() - self.bytes as u64 - PAGE
    }

    /// The first address of the stack's slot: the bottom of the unmapped space below it.
    pub const fn slot_base(&self) -> u64 {
        slot_base(self.slot)
    }

    /// The usable bytes.
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    /// The first address past the stack, 16-byte aligned: the top of its slot.
    pub const fn top(&self) -> u64 {
        slot_base(self.slot) + SLOT
    }

    /// Writes a new thread's first frame on this stack, starting at `start` with every other
    /// register zero, and returns its address (the value the scheduler's run queue keeps). It
    /// is recorded as resumable only by [`publish_first_frame`](Self::publish_first_frame), once
    /// the scheduler has accepted the thread, so a refused spawn leaves nothing behind.
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
    /// frame published here (and consumed by the first resume). The scheduler calls this once
    /// it has accepted the thread and still holds it unrunnable (under the run queue's lock).
    pub fn publish_first_frame(&mut self, cpu: CpuIndex) -> bool {
        self.first_frame
            .take()
            .is_some_and(|frame| cpu::record_saved_frame(cpu, frame))
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
        if cpu::stack_in_use(top - self.bytes as u64, top, top) {
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
    pub const fn addr(&self) -> u64 {
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
