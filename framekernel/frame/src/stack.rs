//! Kernel thread stacks and FPU areas (CLAUDE.md §25, §28; ADR-0011 N6).
//!
//! A [`GuardedStack`] is memory the framework owns: a guard page, unmapped so running off the
//! end of the stack faults, and the usable bytes above it. Dropping one gives the memory back,
//! except while a processor might still be running on it: the framework knows which stacks are
//! in use because every kernel stack a processor runs on has its top in some processor's TSS
//! (`cpu::set_kernel_stack`) and every suspended thread has a saved frame on its stack that the
//! trap entry recorded (`cpu::record_saved_frame`). A stack still in either is leaked rather than
//! freed, which is the safe failure for a safe caller that drops one too early.

use alloc::alloc::{Layout, alloc, alloc_zeroed, dealloc};

use crate::cpu::{self, CpuIndex};
use crate::descriptors::{KERNEL_CODE, KERNEL_DATA, USER_CODE, USER_DATA};

/// The guard page below every stack.
const GUARD: usize = 4096;

/// A kernel stack with an unmapped guard page below it. Owned; freed on drop.
#[derive(Debug)]
pub struct GuardedStack {
    /// The guard page's address (the allocation's start), never 0.
    guard: usize,
    /// The usable bytes above the guard page.
    bytes: usize,
    /// The first frame built on this stack, not yet published to a processor.
    first_frame: Option<u64>,
}

impl GuardedStack {
    /// A zeroed stack of `bytes` usable bytes, or `None` if there is no memory or the guard
    /// page could not be made.
    pub fn new(bytes: usize) -> Option<Self> {
        let layout = Layout::from_size_align(bytes.checked_add(GUARD)?, GUARD).ok()?;
        // SAFETY: `layout` has a non-zero size; the allocation is freed in `Drop`.
        let base = unsafe { alloc_zeroed(layout) };
        if base.is_null() {
            return None;
        }
        let guard = base.expose_provenance();
        if crate::vm::make_guard(guard as u64).is_err() {
            // SAFETY: `base` is the allocation made just above with this layout, nothing else
            // refers to it, and a failed `make_guard` left its first page mapped.
            unsafe { dealloc(base, layout) };
            return None;
        }
        Some(Self {
            guard,
            bytes,
            first_frame: None,
        })
    }

    /// The guard page's address.
    pub const fn guard_addr(&self) -> u64 {
        self.guard as u64
    }

    /// The usable bytes (above the guard page).
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    /// The first address past the stack, 16-byte aligned.
    pub const fn top(&self) -> u64 {
        ((self.guard + GUARD + self.bytes) & !0xF) as u64
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
        // the top stays zero because `alloc_zeroed` made the whole stack zero.
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
        let start = (self.guard + GUARD) as *mut u8;
        // SAFETY: `[guard + GUARD, guard + GUARD + bytes)` is mapped (only the guard page is
        // not) and owned by this stack, and `&mut self` means nothing else uses it: the
        // thread that will run on it has not been started (or has ended).
        unsafe { core::slice::from_raw_parts_mut(start, self.bytes) }
    }

    fn layout(&self) -> Option<Layout> {
        Layout::from_size_align(self.bytes.checked_add(GUARD)?, GUARD).ok()
    }
}

impl Drop for GuardedStack {
    fn drop(&mut self) {
        let start = self.guard as u64;
        let end = start.saturating_add((self.bytes + GUARD) as u64);
        let Some(layout) = self.layout() else {
            return;
        };
        // Leak rather than free memory a processor could still run on (see the module docs).
        if crate::cpu::stack_in_use(start, end, self.top()) {
            return;
        }
        // The guard must be mapped again before its frame returns to the heap.
        if crate::vm::unguard(start).is_err() {
            return; // leak rather than free memory that is not mapped
        }
        // SAFETY: `guard` is the start of the allocation made in `new` with this layout, the
        // guard page is mapped again, and nothing uses this memory any more (just checked).
        unsafe { dealloc(core::ptr::with_exposed_provenance_mut(self.guard), layout) };
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

/// A heap page that can be unmapped and mapped back: what the TLB boot proof caches, unmaps
/// and probes on every processor. It is never a Rust object anyone holds a reference to:
/// the holder sees it only through [`crate::cpu::probe_read`], so taking the mapping away
/// breaks nothing. Dropping it gives the page back (mapped first, if it isn't).
#[derive(Debug)]
pub struct GuardablePage {
    addr: u64,
    unmapped: bool,
}

impl GuardablePage {
    /// A new zeroed, mapped page, or `None`.
    pub fn new() -> Option<Self> {
        let layout = Layout::from_size_align(GUARD, GUARD).ok()?;
        // SAFETY: `layout` has a non-zero size; the allocation is freed in `Drop`.
        let page = unsafe { alloc_zeroed(layout) };
        if page.is_null() {
            return None;
        }
        Some(Self {
            addr: page.expose_provenance() as u64,
            unmapped: false,
        })
    }

    /// The page's address, for probes.
    pub const fn addr(&self) -> u64 {
        self.addr
    }

    /// Unmaps it on every processor with a single-page shootdown. `false` if that was refused.
    pub fn unmap(&mut self) -> bool {
        let ok = crate::vm::make_guard(self.addr).is_ok();
        self.unmapped |= ok;
        ok
    }

    /// Unmaps several pages with one full flush on every processor. `false` if that was
    /// refused.
    pub fn unmap_all(pages: &mut [&mut Self]) -> bool {
        let mut addrs = [0u64; 8];
        let Some(slots) = addrs.get_mut(..pages.len()) else {
            return false;
        };
        for (slot, page) in slots.iter_mut().zip(pages.iter()) {
            *slot = page.addr;
        }
        let ok = crate::vm::make_guards(slots).is_ok();
        for page in pages.iter_mut() {
            page.unmapped |= ok;
        }
        ok
    }
}

impl Drop for GuardablePage {
    fn drop(&mut self) {
        if self.unmapped && crate::vm::unguard(self.addr).is_err() {
            return; // leak rather than free memory that is not mapped
        }
        if let Ok(layout) = Layout::from_size_align(GUARD, GUARD) {
            // SAFETY: `addr` is the allocation made in `new` with this layout, mapped again
            // above if it was unmapped, and only this owner ever knew it.
            unsafe {
                dealloc(
                    core::ptr::with_exposed_provenance_mut(self.addr as usize),
                    layout,
                )
            };
        }
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
