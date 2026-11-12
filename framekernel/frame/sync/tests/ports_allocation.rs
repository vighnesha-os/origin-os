//! What the ports' protocol asks of the heap (S3-6a; the kernel's first reliance): a reservation
//! is the only allocation, and without memory it is refused, not a stop. Every operation after it
//! only moves what it made, and frees.
//!
//! Its own test binary, for its own global allocator, which watches only the thread that asks it
//! to, so the test harness's own allocations do not count.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

use origin_sync::ports::{Bound, Kernel, NoMemory, Port, Refused, Request, Reservation, Source};
use origin_sync::wait::Scheduler;

std::thread_local! {
    /// While set, every allocation this thread asks for fails.
    static REFUSE: Cell<bool> = const { Cell::new(false) };
    /// While set, how many allocations this thread has asked for.
    static COUNTED: Cell<Option<u64>> = const { Cell::new(None) };
}

/// The system allocator, watched.
struct Watching;

// SAFETY: every call is forwarded to `System`, which upholds the contract, except that an
// allocation may return null, which the contract allows. The thread-locals it reads are
// initialized by constants and need no destructor, so reading them allocates nothing.
unsafe impl GlobalAlloc for Watching {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if REFUSE.with(Cell::get) {
            return core::ptr::null_mut();
        }
        COUNTED.with(|counted| {
            if let Some(count) = counted.get() {
                counted.set(Some(count + 1));
            }
        });
        // SAFETY: the caller's layout is passed on unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System.alloc` with this layout, through `alloc` above.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Watching = Watching;

/// A flow that cannot block: a wait polls.
struct Host;

impl Scheduler for Host {
    type Thread = u32;

    fn current_thread(&self) -> Option<u32> {
        None
    }

    fn without_interrupts<R>(&self, f: impl FnOnce() -> R) -> R {
        f()
    }

    fn block(&self) {}

    fn block_until(&self, _deadline: u64) {}

    fn yield_now(&self) {}

    fn wake(&self, _thread: u32) {}

    fn monotonic_nanos(&self) -> u64 {
        0
    }
}

struct Test;

impl Kernel for Test {
    type Scheduler = Host;
    type Payload = u32;
    type Owner = u32;
    type PortRef = Arc<Port<Test>>;
    type SourceRef = Arc<Source<Test>>;
}

/// `f`, and how many allocations this thread asked for while it ran.
fn counting<R>(f: impl FnOnce() -> R) -> (R, u64) {
    COUNTED.with(|counted| counted.set(Some(0)));
    let result = f();
    let count = COUNTED.with(|counted| counted.replace(None)).unwrap_or(0);
    (result, count)
}

const ASKED: u32 = 1;
const OTHER: u32 = 2;

#[test]
fn a_reservation_without_memory_is_refused() {
    let port = Arc::new(Port::<Test>::new());
    REFUSE.with(|refuse| refuse.set(true));
    let refused = Reservation::<Test>::try_new(&Host, &port, 7);
    REFUSE.with(|refuse| refuse.set(false));
    assert_eq!(refused.err(), Some(NoMemory));
    // The same call succeeds once there is memory, so the refusal was the allocator's.
    assert_eq!(
        Reservation::<Test>::try_new(&Host, &port, 7)
            .unwrap()
            .into_payload(),
        7
    );
}

#[test]
fn nothing_allocates_after_the_reservation() {
    let port = Arc::new(Port::<Test>::new());
    let source = Arc::new(Source::<Test>::new());
    let signals = AtomicU32::new(0);
    let reserved: Vec<Reservation<Test>> = (0..8)
        .map(|id| Reservation::<Test>::try_new(&Host, &port, id).unwrap())
        .collect();
    let bind = |storage, owner, key, mask| {
        Source::<Test>::bind(
            &source,
            &Host,
            storage,
            Request { owner, key, mask },
            || true,
            || signals.load(Relaxed),
        )
    };
    let ((), allocations) = counting(|| {
        let mut reserved = reserved.into_iter();
        let mut next = || reserved.next().unwrap();
        // Armed, fired, taken.
        assert!(matches!(bind(next(), 1, 1, ASKED), Ok(Bound::Armed)));
        signals.store(ASKED, Relaxed);
        assert_eq!(source.deliver(&Host, || signals.load(Relaxed)), 1);
        let (packet, storage) = port.take(&Host, 0, || true).unwrap();
        assert_eq!(packet.key, 1);
        assert_eq!(storage.into_payload(), 0);
        // Queued at once, then taken.
        assert!(matches!(bind(next(), 1, 2, ASKED), Ok(Bound::Queued)));
        drop(port.take(&Host, 0, || true).unwrap());
        // Cancelled implicitly, explicitly, and taken back by the close.
        assert!(matches!(bind(next(), 1, 3, OTHER), Ok(Bound::Armed)));
        assert_eq!(
            source.cancel_owners(&Host, |owner| *owner == 1, || signals.load(Relaxed)),
            1
        );
        assert!(matches!(bind(next(), 2, 4, OTHER), Ok(Bound::Armed)));
        assert_eq!(source.cancel(&Host, &port, 4).count(), 1);
        assert!(matches!(bind(next(), 2, 5, OTHER), Ok(Bound::Armed)));
        assert_eq!(
            port.close(&Host).count(),
            2,
            "the canceled packet and the armed binding"
        );
        // Refused after the close, and given up unused.
        assert!(matches!(
            bind(next(), 2, 6, OTHER),
            Err((Refused::PortClosed, _))
        ));
        drop(next());
        drop(next());
    });
    assert_eq!(
        allocations, 0,
        "an operation after the reservations allocated"
    );
}
