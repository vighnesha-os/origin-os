//! The kernel's memory owner: the frame allocator and the heap behind one lock, and the
//! `GlobalAlloc` that makes Rust's `alloc` crate usable in the kernel (CLAUDE.md §13, §28).
//!
//! The bookkeeping is the library's `frames` and `heap`, both safe code tested on the host.
//! This module only turns heap addresses into pointers. That is sound because the kernel's
//! direct map covers every `Usable` frame read/write (identity), and the heap hands each
//! byte to one allocation at a time.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

use crate::frames::{self, FrameAllocator};
use crate::heap::{Heap, HeapStats, PageSource, Slab};

use origin_sync::SpinLock;

use crate::addr::PhysAddr;
use crate::cpu::without_interrupts;

/// Capacity of the slab table: 1024 slabs of 4 KiB, 4 MiB of small blocks.
const SLABS: usize = 1024;

/// The slab table, in the kernel's RW segment (48 KiB of zero-filled bss).
static mut SLAB_TABLE: [Slab; SLABS] = [Slab::ZERO; SLABS];

struct Memory {
    frames: FrameAllocator<'static>,
    heap: Heap<'static>,
}

/// `None` until [`init`].
static MEMORY: SpinLock<Option<Memory>> = SpinLock::new(None);
/// Whether [`init`] has run: the slab table is handed out once.
static MEMORY_STARTED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

struct Frames<'a>(&'a mut FrameAllocator<'static>);

impl PageSource for Frames<'_> {
    fn allocate_pages(&mut self, count: u64, align: u64) -> Option<u64> {
        if count == 1 && align <= 1 {
            self.0.allocate()
        } else {
            self.0.allocate_run(count, align)
        }
    }

    fn free_pages(&mut self, addr: u64, count: u64) -> bool {
        if count == 1 {
            self.0.free(addr).is_ok()
        } else {
            self.0.free_run(addr, count).is_ok()
        }
    }
}

/// Takes ownership of the frame allocator and starts the heap, once (a later call does
/// nothing). The first call is the boot flow's, after the kernel's own page tables are live
/// (the direct map makes heap addresses usable as pointers).
pub fn init(frames: FrameAllocator<'static>) {
    if MEMORY_STARTED.swap(true, core::sync::atomic::Ordering::AcqRel) {
        return;
    }
    // SAFETY: `init` passes the check above once, so this is the only reference ever made to
    // `SLAB_TABLE` (non-null and aligned: it is a static); it then lives inside `MEMORY`.
    let Some(table) = (unsafe { (&raw mut SLAB_TABLE).as_mut() }) else {
        return;
    };
    without_interrupts(|| {
        *MEMORY.lock() = Some(Memory {
            frames,
            heap: Heap::new(table),
        });
    });
}

/// Heap totals and free frames, or `None` before [`init`].
pub fn stats() -> Option<(HeapStats, u64)> {
    without_interrupts(|| {
        MEMORY
            .lock()
            .as_ref()
            .map(|m| (m.heap.stats(), m.frames.free_frames()))
    })
}

/// Returns the loader's and the firmware's boot-time memory (`LoaderReclaimable`) to the
/// frame allocator. Returns the frames added and the free frames after, or `None` before
/// [`init`].
///
///
/// # Safety
///
/// Only once nothing there is in use: after the kernel's own page tables replaced the
/// firmware's, and after every enabled processor has left firmware code (INIT).
pub unsafe fn reclaim_loader_memory() -> Option<(u64, u64)> {
    without_interrupts(|| {
        let mut guard = MEMORY.lock();
        let m = guard.as_mut()?;
        let added = m
            .frames
            .reclaim(origin_boot_info::RegionKind::LoaderReclaimable);
        Some((added, m.frames.free_frames()))
    })
}

/// One 4 KiB physical frame handed out by the allocator: its holder owns it until it gives it
/// back with [`free_frame`]. Not `Copy` and not `Clone`: a frame has one owner, so a free cannot
/// be doubled and a freed frame cannot still be named by anyone.
#[derive(Debug)]
pub struct OwnedFrame {
    phys: PhysAddr,
}

impl OwnedFrame {
    /// The frame's physical address.
    pub const fn phys(&self) -> PhysAddr {
        self.phys
    }

    /// Where the kernel reaches it, through the direct map.
    pub(crate) fn virt(&self) -> usize {
        crate::phys::to_virt(self.phys.raw()) as usize
    }
}

/// One free 4 KiB frame, or `None`.
pub fn allocate_frame() -> Option<OwnedFrame> {
    without_interrupts(|| MEMORY.lock().as_mut()?.frames.allocate()).map(|addr| OwnedFrame {
        phys: PhysAddr::new(addr),
    })
}

/// Gives back a frame from [`allocate_frame`]; `false` if the allocator refused it.
pub fn free_frame(frame: OwnedFrame) -> bool {
    without_interrupts(|| {
        MEMORY
            .lock()
            .as_mut()
            .is_some_and(|m| m.frames.free(frame.phys.raw()).is_ok())
    })
}

/// The kernel's global allocator.
struct KernelAllocator;

// SAFETY: `alloc` returns null or the address of `layout.size()` bytes aligned to
// `layout.align()` that no other live allocation overlaps (the heap's contract, tested in the
// library); the direct map makes those bytes readable and writable. `dealloc` returns them.
// Interrupt handlers never allocate, and the lock is held with interrupts disabled, so it
// can deadlock neither against a handler nor against a preempted holder on this CPU.
unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let (size, align) = (layout.size() as u64, layout.align() as u64);
        let addr = without_interrupts(|| {
            let mut guard = MEMORY.lock();
            let m = guard.as_mut()?;
            m.heap.allocate(&mut Frames(&mut m.frames), size, align)
        });
        match addr {
            Some(addr) => ptr::with_exposed_provenance_mut(crate::phys::to_virt(addr) as usize),
            None => ptr::null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let (size, align) = (layout.size() as u64, layout.align() as u64);
        let addr = crate::phys::to_phys(ptr.expose_provenance() as u64);
        let result = without_interrupts(|| {
            let mut guard = MEMORY.lock();
            match guard.as_mut() {
                Some(m) => m.heap.free(&mut Frames(&mut m.frames), addr, size, align),
                None => Ok(()),
            }
        });
        if let Err(e) = result {
            // A bad free means memory safety is already broken somewhere: stop.
            panic!("heap free of {addr:#x} ({size} bytes) refused: {e:?}");
        }
    }
}

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator;

/// Frame size, for callers that report in bytes.
pub const FRAME: u64 = frames::FRAME;
