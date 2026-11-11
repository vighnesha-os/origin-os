//! [`ObjectRef::try_new`] without memory hands the value back instead of stopping: a kernel object
//! that cannot be allocated is an error for whoever asked for it (CLAUDE.md §13, S3-1).
//!
//! Its own test binary, for its own global allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, Ordering};

use origin_frame::objref::ObjectRef;

/// While set, every allocation fails.
static REFUSE: AtomicBool = AtomicBool::new(false);

/// The system allocator, unless [`REFUSE`] is set.
struct Refusing;

// SAFETY: every call is forwarded to `System`, which upholds the contract, except that an
// allocation may return null, which the contract allows.
unsafe impl GlobalAlloc for Refusing {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if REFUSE.load(Ordering::SeqCst) {
            return core::ptr::null_mut();
        }
        // SAFETY: the caller's layout is passed on unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System.alloc` with this layout, through `alloc` above.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Refusing = Refusing;

/// The only test in this binary: the flag would fail anything running beside it.
#[test]
fn an_object_that_cannot_be_allocated_is_handed_back() {
    REFUSE.store(true, Ordering::SeqCst);
    let refused = ObjectRef::try_new([7u8; 64]);
    REFUSE.store(false, Ordering::SeqCst);
    assert_eq!(refused.err(), Some([7u8; 64]));
    // The same call succeeds once there is memory, so the refusal was the allocator's.
    assert_eq!(*ObjectRef::try_new(5u32).unwrap(), 5);
}
