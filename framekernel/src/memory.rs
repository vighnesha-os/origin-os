//! The kernel's memory owner: the frame allocator and the heap behind one lock, and the
//! `GlobalAlloc` that makes Rust's `alloc` crate usable in the kernel (CLAUDE.md §13, §28).
//!
//! The bookkeeping is the library's `frames` and `heap`, both safe code tested on the host.
//! This module only turns heap addresses into pointers. That is sound because the kernel's
//! direct map covers every `Usable` frame read/write (identity), and the heap hands each
//! byte to one allocation at a time.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

use framekernel::frames::{self, FrameAllocator};
use framekernel::heap::{Heap, HeapStats, PageSource, Slab};

use origin_sync::SpinLock;

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

/// Takes ownership of the frame allocator and starts the heap. Call once, after the kernel's
/// own page tables are live (the direct map makes heap addresses usable as pointers).
pub(crate) fn init(frames: FrameAllocator<'static>) {
    // SAFETY: `init` runs once, on the boot processor, before anything allocates, so this is
    // the only reference ever made to `SLAB_TABLE` (non-null and aligned: it is a static);
    // it then lives inside `MEMORY`.
    let Some(table) = (unsafe { (&raw mut SLAB_TABLE).as_mut() }) else {
        return;
    };
    *MEMORY.lock() = Some(Memory {
        frames,
        heap: Heap::new(table),
    });
}

/// Heap totals and free frames, or `None` before [`init`].
pub(crate) fn stats() -> Option<(HeapStats, u64)> {
    MEMORY
        .lock()
        .as_ref()
        .map(|m| (m.heap.stats(), m.frames.free_frames()))
}

/// Returns the loader's and the firmware's boot-time memory (`LoaderReclaimable`) to the
/// frame allocator. Returns the frames added and the free frames after, or `None` before
/// [`init`].
///
/// Call only when nothing there is in use: after the kernel's own page tables replaced the
/// firmware's, and after every enabled processor has left firmware code (INIT).
pub(crate) fn reclaim_loader_memory() -> Option<(u64, u64)> {
    let mut guard = MEMORY.lock();
    let m = guard.as_mut()?;
    let added = m
        .frames
        .reclaim(origin_boot_info::RegionKind::LoaderReclaimable);
    Some((added, m.frames.free_frames()))
}

/// The kernel's global allocator.
struct KernelAllocator;

// SAFETY: `alloc` returns null or the address of `layout.size()` bytes aligned to
// `layout.align()` that no other live allocation overlaps (the heap's contract, tested in the
// library); the direct map makes those bytes readable and writable. `dealloc` returns them.
// Interrupt handlers never allocate, so the spin lock cannot deadlock against one.
unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut guard = MEMORY.lock();
        let Some(m) = guard.as_mut() else {
            return ptr::null_mut();
        };
        let (size, align) = (layout.size() as u64, layout.align() as u64);
        match m.heap.allocate(&mut Frames(&mut m.frames), size, align) {
            Some(addr) => ptr::with_exposed_provenance_mut(addr as usize),
            None => ptr::null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let mut guard = MEMORY.lock();
        if let Some(m) = guard.as_mut() {
            let (size, align) = (layout.size() as u64, layout.align() as u64);
            let addr = ptr.expose_provenance() as u64;
            if let Err(e) = m.heap.free(&mut Frames(&mut m.frames), addr, size, align) {
                drop(guard);
                // A bad free means memory safety is already broken somewhere: stop.
                panic!("heap free of {addr:#x} ({size} bytes) refused: {e:?}");
            }
        }
    }
}

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator;

/// Frame size, for callers that report in bytes.
pub(crate) const FRAME: u64 = frames::FRAME;
