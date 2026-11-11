//! A reference-counted kernel object with a fallible constructor (`Arc::try_new` is unstable on
//! Rust 1.99.1): the kernel must report an allocation failure, not stop.

use alloc::alloc::{Layout, alloc, dealloc};
use core::ops::Deref;
use core::ptr::NonNull;

use origin_sync::refcount::RefCount;

/// A counted reference to a kernel object, created with [`ObjectRef::try_new`], which
/// reports an allocation failure instead of stopping the kernel the way `Arc::new` would
/// (`Arc::try_new` is unstable on Rust 1.99.1).
#[derive(Debug)]
pub struct ObjectRef<T> {
    inner: NonNull<Counted<T>>,
}

struct Counted<T> {
    count: RefCount,
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
                count: RefCount::one(),
                value,
            })
        };
        Ok(Self { inner })
    }

    /// Whether two references name the same object.
    pub fn same(a: &Self, b: &Self) -> bool {
        a.inner == b.inner
    }

    /// The object itself, if this is the only reference to it: a creator setting up what no
    /// one else can see yet.
    pub fn get_mut(this: &mut Self) -> Option<&mut T> {
        // SAFETY: as in `deref`; the count is read through a shared reference, so nothing here
        // aliases a `&mut` someone else may hold while the check is still undecided.
        let unique = unsafe { this.inner.as_ref() }.count.is_unique();
        if !unique {
            return None;
        }
        // SAFETY: the count is 1, and this reference is it. Another can only be made through a
        // `&self` of this one, which the `&mut` borrow excludes for as long as the result lives,
        // and the Acquire load above orders every earlier holder's accesses before this one.
        Some(unsafe { &mut this.inner.as_mut().value })
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
    /// One more reference.
    ///
    /// # Panics
    ///
    /// With more references than the count can hold (`origin_sync::refcount::MAX_REFERENCES`),
    /// as `Arc` aborts: in the kernel a panic halts and never unwinds, so the count can never
    /// be brought back down to zero while references remain.
    fn clone(&self) -> Self {
        // SAFETY: as in `deref`.
        let counted = unsafe { self.inner.as_ref() };
        if counted.count.retain().is_err() {
            panic!("origin-frame: an object has more references than its count can hold");
        }
        Self { inner: self.inner }
    }
}

impl<T> Drop for ObjectRef<T> {
    fn drop(&mut self) {
        // SAFETY: as in `deref`; this reference is still counted here.
        let counted = unsafe { self.inner.as_ref() };
        // Everything the other holders did with the object happens before it is destroyed:
        // the count's own ordering (`RefCount::release`).
        if !counted.count.release() {
            return;
        }
        // SAFETY: this was the last reference, so nothing else can reach the object.
        unsafe { self.inner.drop_in_place() };
        // SAFETY: allocated by `try_new` with this layout, destroyed just above, never freed
        // before: only the holder that takes the count to zero gets here.
        unsafe { dealloc(self.inner.as_ptr().cast(), Layout::new::<Counted<T>>()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_sole_reference_gets_the_object_itself() {
        let mut only = ObjectRef::try_new(1u32).unwrap();
        *ObjectRef::get_mut(&mut only).unwrap() = 2;
        assert_eq!(*only, 2);
        let mut second = only.clone();
        assert!(ObjectRef::get_mut(&mut only).is_none(), "two references");
        assert!(ObjectRef::get_mut(&mut second).is_none(), "two references");
        drop(second);
        assert!(ObjectRef::get_mut(&mut only).is_some(), "one again");
    }
}
