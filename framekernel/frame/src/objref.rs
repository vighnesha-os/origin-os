//! A reference-counted kernel object with a fallible constructor (`Arc::try_new` is unstable on
//! Rust 1.99.1): the kernel must report an allocation failure, not stop.

use alloc::alloc::{Layout, alloc, dealloc};
use core::ops::Deref;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering, fence};

/// A counted reference to a kernel object, created with [`ObjectRef::try_new`], which
/// reports an allocation failure instead of stopping the kernel the way `Arc::new` would
/// (`Arc::try_new` is unstable on Rust 1.99.1).
#[derive(Debug)]
pub struct ObjectRef<T> {
    inner: NonNull<Counted<T>>,
}

struct Counted<T> {
    count: AtomicUsize,
    value: T,
}

// SAFETY: an `ObjectRef` shares `T` between its holders exactly like `Arc<T>` does: `T: Send`
// because the last holder, on any thread, drops it, and `T: Sync` because all holders read it.
unsafe impl<T: Send + Sync> Send for ObjectRef<T> {}
// SAFETY: as above.
unsafe impl<T: Send + Sync> Sync for ObjectRef<T> {}

impl<T> ObjectRef<T> {
    /// A new object, or the value back if the kernel heap has no room.
    pub fn try_new(value: T) -> Result<Self, T> {
        let layout = Layout::new::<Counted<T>>();
        // SAFETY: the layout has a non-zero size: `Counted` holds an `AtomicUsize`.
        let raw = unsafe { alloc(layout) }.cast::<Counted<T>>();
        let Some(inner) = NonNull::new(raw) else {
            return Err(value);
        };
        // SAFETY: `inner` is a fresh allocation with the layout of `Counted<T>`, so writing
        // one there is in bounds, aligned, and overwrites nothing that needs dropping.
        unsafe {
            inner.write(Counted {
                count: AtomicUsize::new(1),
                value,
            })
        };
        Ok(Self { inner })
    }

    /// Whether two references name the same object.
    pub fn same(a: &Self, b: &Self) -> bool {
        a.inner == b.inner
    }
}

impl<T> Deref for ObjectRef<T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: `inner` points at a `Counted<T>` that `try_new` initialized, kept alive by
        // this reference's count: `Drop` frees it only when the count reaches zero.
        unsafe { &self.inner.as_ref().value }
    }
}

impl<T> Clone for ObjectRef<T> {
    fn clone(&self) -> Self {
        // SAFETY: as in `deref`.
        let counted = unsafe { self.inner.as_ref() };
        counted.count.fetch_add(1, Ordering::Relaxed);
        Self { inner: self.inner }
    }
}

impl<T> Drop for ObjectRef<T> {
    fn drop(&mut self) {
        // SAFETY: as in `deref`; this reference is still counted here.
        let counted = unsafe { self.inner.as_ref() };
        if counted.count.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }
        // Everything the other holders did with the object happens before it is destroyed.
        fence(Ordering::Acquire);
        // SAFETY: this was the last reference, so nothing else can reach the object.
        unsafe { self.inner.drop_in_place() };
        // SAFETY: allocated by `try_new` with this layout, destroyed just above, never freed
        // before: only the holder that takes the count to zero gets here.
        unsafe { dealloc(self.inner.as_ptr().cast(), Layout::new::<Counted<T>>()) };
    }
}
