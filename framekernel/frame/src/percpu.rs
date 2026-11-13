//! Per-processor tables, sized when the processors are discovered (ADR-0008 Amendment 1, D1;
//! CLAUDE.md §24 per-CPU state).
//!
//! Every table of per-processor state is a [`PerCpu`], indexed by `CpuIndex` (ADR-0008 C2). It
//! is made in two phases, so that the kernel compiles in no processor count:
//!
//! - **Before the heap exists**, the table holds one slot, the boot processor's (index 0), in
//!   the kernel's image. It is usable from the first instruction and never moves: one processor is
//!   the only bound before the heap.
//! - **After the MADT is read and before the first application processor starts**, the boot
//!   processor sizes it once ([`PerCpu::size`]): one slice on the heap for indexes 1 and up,
//!   published once and never resized, moved or freed.
//!
//! Every table of a crate is sized in that crate's one sizing function (`size_per_cpu`), called
//! once, and `cargo xtask check` refuses a `PerCpu` static that function does not name.
//!
//! A table that cannot get its memory is sized for as many indexes as the heap allows, and says
//! how many ([`Sizing`]); the processors past the fewest any table holds stay in firmware
//! (ADR-0008 Amendment 1, D3).
//!
//! The slice is published through [`SetOnce`], which a reader reads without a lock: this module
//! needs no `unsafe`.
#![forbid(unsafe_code)]

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::once::SetOnce;

/// A table with one `T` for each processor index: the boot processor's in the kernel's image,
/// the others' on the heap once sized.
pub struct PerCpu<T: 'static> {
    /// Index 0's element, the boot processor's.
    boot: T,
    /// A new element, as `boot` was before the boot processor used it: what sizing gives every
    /// other index.
    fresh: fn() -> T,
    /// Indexes 1 and up, once sized: a slice that is never freed, moved or resized.
    rest: SetOnce<&'static [T]>,
    /// Whether [`PerCpu::size`] has run.
    sized: AtomicBool,
}

impl<T> core::fmt::Debug for PerCpu<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PerCpu")
            .field("indexes", &self.indexes())
            .finish_non_exhaustive()
    }
}

/// Why [`PerCpu::size`] did nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeError {
    /// The table was sized already: it is sized once.
    Already,
}

impl<T> PerCpu<T> {
    /// A table holding the boot processor's element, `boot`, alone; [`PerCpu::size`] gives each
    /// other index `fresh()`, which makes what `boot` is.
    pub const fn new(boot: T, fresh: fn() -> T) -> Self {
        Self {
            boot,
            fresh,
            rest: SetOnce::new(),
            sized: AtomicBool::new(false),
        }
    }

    /// Processor `index`'s element, or `None` past the indexes the table holds.
    #[inline]
    pub fn get(&self, index: usize) -> Option<&T> {
        match index.checked_sub(1) {
            None => Some(&self.boot),
            Some(past) => self.rest.get()?.get(past),
        }
    }

    /// The indexes the table holds: 1 until it is sized, never 0.
    #[inline]
    pub fn indexes(&self) -> usize {
        1 + self.rest.get().map_or(0, |rest| rest.len())
    }

    /// Whether [`PerCpu::size`] has run.
    pub fn is_sized(&self) -> bool {
        self.sized.load(Ordering::Acquire)
    }

    /// Every element, index 0 first.
    pub fn iter(&self) -> impl Iterator<Item = &T> + '_ {
        (0..self.indexes()).filter_map(|index| self.get(index))
    }

    /// The bytes one more index costs this table.
    pub const fn element_bytes() -> usize {
        core::mem::size_of::<T>()
    }

    /// Sizes the table for `count` indexes, once, each index past 0 given a fresh element. If the
    /// heap cannot hold that many, the table holds as many as it can, and at least the boot
    /// processor's. Returns how many indexes it holds.
    ///
    /// # Errors
    ///
    /// [`SizeError::Already`] if the table was sized before; nothing changes.
    pub fn size(&self, count: usize) -> Result<usize, SizeError> {
        let fresh = self.fresh;
        self.size_with(count, |_| fresh())
    }

    /// [`PerCpu::size`], index `i` (1 and up) given `make(i)`.
    ///
    /// # Errors
    ///
    /// As [`PerCpu::size`].
    pub fn size_with(
        &self,
        count: usize,
        mut make: impl FnMut(usize) -> T,
    ) -> Result<usize, SizeError> {
        if self.sized.swap(true, Ordering::AcqRel) {
            return Err(SizeError::Already);
        }
        let wanted = count.saturating_sub(1);
        if wanted == 0 {
            return Ok(1);
        }
        let held = largest_fitting(wanted, |n| Vec::<T>::new().try_reserve_exact(n).is_ok());
        let mut rest = Vec::new();
        if rest.try_reserve_exact(held).is_err() {
            return Ok(1);
        }
        for index in 1..=held {
            rest.push(make(index));
        }
        let rest: &'static [T] = rest.leak();
        let held = rest.len() + 1;
        // Only this call, which `sized` admits once, sets it.
        let _ = self.rest.set(rest);
        Ok(held)
    }
}

/// The largest `n` in `0..=wanted` for which `fits(n)` holds, given that `fits` holds for every
/// `n` below one it holds for, and for 0. `wanted` itself is tried first, so a heap with room for
/// it is asked once.
fn largest_fitting(wanted: usize, mut fits: impl FnMut(usize) -> bool) -> usize {
    if fits(wanted) {
        return wanted;
    }
    let (mut low, mut high) = (0, wanted);
    // Invariant: `fits(low)`, and not `fits(high)`.
    while high - low > 1 {
        let mid = low + (high - low) / 2;
        if fits(mid) {
            low = mid;
        } else {
            high = mid;
        }
    }
    low
}

/// What sizing a crate's tables reached: the indexes every table holds, and the bytes each index
/// costs across them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sizing {
    /// The indexes asked for.
    pub wanted: usize,
    /// The indexes every table sized so far holds: at most `wanted`, at least 1.
    pub reached: usize,
    /// The bytes one index costs across the tables sized so far.
    pub bytes_per_index: usize,
    /// Tables sized.
    pub tables: usize,
    /// Tables that were sized already when asked: a sizing function called twice.
    pub again: usize,
}

impl Sizing {
    /// Nothing sized yet, `wanted` indexes asked for.
    pub const fn new(wanted: usize) -> Self {
        Self {
            wanted,
            reached: if wanted == 0 { 1 } else { wanted },
            bytes_per_index: 0,
            tables: 0,
            again: 0,
        }
    }

    /// Sizes `table` for the indexes asked for.
    pub fn table<T>(&mut self, table: &PerCpu<T>) {
        match table.size(self.wanted) {
            Ok(held) => {
                self.reached = self.reached.min(held);
                self.bytes_per_index += PerCpu::<T>::element_bytes();
                self.tables += 1;
            }
            Err(SizeError::Already) => self.again += 1,
        }
    }
}

/// The framework's one sizing function (ADR-0008 Amendment 1, D1): every `PerCpu` table of this
/// crate, sized for the indexes `sizing` asks for. `smp::start` calls it once, after the MADT is
/// read and before any application processor is sent INIT. `cargo xtask check` refuses a
/// `PerCpu` static of this crate that it does not name.
#[cfg(target_os = "none")]
pub(crate) fn size_per_cpu(sizing: &mut Sizing) {
    sizing.table(&crate::cpu::APIC_IDS);
    sizing.table(&crate::cpu::CPU_LOCAL);
    sizing.table(&crate::cpu::CPU_STATE_HOLDS);
    sizing.table(&crate::cpu::SAVED_FRAMES);
    sizing.table(&crate::cpu::PAT_READY);
    sizing.table(&crate::random::SLOTS);
    sizing.table(&crate::smp::TOPOLOGY);
    sizing.table(&crate::speculation::RECORDS);
    sizing.table(&crate::speculation::LEDGERS);
    sizing.table(&crate::timer::X2APIC_READY);
    sizing.table(&crate::tlb::RECORDS);
    sizing.table(&crate::tlb::ASKED);
    sizing.table(&crate::tlb::GLOBAL_PAGES);
    sizing.table(&crate::tlb::INTERRUPTS);
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::AtomicU64;

    #[test]
    fn before_sizing_the_boot_slot_alone_and_it_never_moves() {
        let table = Box::leak(Box::new(PerCpu::new(AtomicU64::new(7), || {
            AtomicU64::new(0)
        })));
        assert_eq!(table.indexes(), 1);
        assert!(!table.is_sized());
        let boot = table.get(0).unwrap() as *const AtomicU64;
        assert!(table.get(1).is_none());
        assert!(table.get(usize::MAX).is_none());
        assert_eq!(table.size_with(4, |i| AtomicU64::new(i as u64 * 10)), Ok(4));
        assert_eq!(
            table.get(0).unwrap() as *const AtomicU64,
            boot,
            "never moved"
        );
        assert_eq!(table.get(0).unwrap().load(Ordering::Relaxed), 7);
        let got: Vec<_> = table.iter().map(|v| v.load(Ordering::Relaxed)).collect();
        assert_eq!(got, [7, 10, 20, 30]);
        assert!(table.get(4).is_none(), "past the length");
    }

    #[test]
    fn a_table_is_sized_once_with_fresh_elements() {
        let table = PerCpu::new(0u8, || 5);
        assert_eq!(table.size(3), Ok(3));
        assert_eq!(table.get(2), Some(&5), "a fresh element");
        assert_eq!(table.size(9), Err(SizeError::Already));
        assert_eq!(table.size_with(9, |_| 2), Err(SizeError::Already));
        assert_eq!(table.indexes(), 3);
        let single = PerCpu::new(0u8, || 0);
        assert_eq!(single.size(1), Ok(1));
        assert!(single.is_sized());
        assert_eq!(
            single.size(5),
            Err(SizeError::Already),
            "even one index is a sizing"
        );
        let none = PerCpu::new(0u8, || 0);
        assert_eq!(
            none.size(0),
            Ok(1),
            "the boot processor's slot is always there"
        );
    }

    /// ADR-0008 Amendment 1, D7: from one processor to more than 4,096.
    #[test]
    fn tables_hold_any_count() {
        for count in [1, 2, 3, 64, 65, 300, 1000, 4097, 70_000] {
            let table = PerCpu::new(0u32, || 0);
            assert_eq!(table.size_with(count, |i| i as u32), Ok(count), "{count}");
            assert_eq!(table.indexes(), count);
            assert_eq!(table.get(count - 1).copied(), Some(count as u32 - 1));
            assert!(table.get(count).is_none());
        }
    }

    #[test]
    fn the_largest_count_that_fits_is_found() {
        for (wanted, limit) in [(0, 0), (5, 9), (5, 5), (5, 4), (5, 0), (4096, 287), (1, 0)] {
            let mut asked = Vec::new();
            let got = largest_fitting(wanted, |n| {
                asked.push(n);
                n <= limit
            });
            assert_eq!(got, wanted.min(limit), "{wanted} wanted, room for {limit}");
            assert_eq!(
                asked.first(),
                Some(&wanted),
                "the whole count is tried first"
            );
            assert!(asked.len() <= 2 + usize::BITS as usize, "{asked:?}");
        }
    }

    #[test]
    fn sizing_reaches_the_fewest_any_table_holds_and_counts_every_table() {
        static A: PerCpu<u64> = PerCpu::new(0, || 1);
        static B: PerCpu<[u8; 3]> = PerCpu::new([0; 3], || [1; 3]);
        let mut sizing = Sizing::new(5);
        sizing.table(&A);
        sizing.table(&B);
        assert_eq!((A.get(4), B.get(4)), (Some(&1), Some(&[1; 3])));
        assert_eq!(
            sizing,
            Sizing {
                wanted: 5,
                reached: 5,
                bytes_per_index: 8 + 3,
                tables: 2,
                again: 0,
            }
        );
        sizing.table(&A);
        assert_eq!(
            (sizing.tables, sizing.again),
            (2, 1),
            "a second sizing is counted"
        );
    }

    #[test]
    fn lookups_from_many_threads_see_whole_elements() {
        static T: PerCpu<[AtomicU64; 4]> = PerCpu::new([const { AtomicU64::new(0) }; 4], || {
            [const { AtomicU64::new(0) }; 4]
        });
        std::thread::scope(|s| {
            let readers: Vec<_> = (0..4)
                .map(|_| {
                    s.spawn(|| {
                        loop {
                            if let Some(e) = T.get(63) {
                                return e.iter().map(|v| v.load(Ordering::Relaxed)).sum::<u64>();
                            }
                            std::hint::spin_loop();
                        }
                    })
                })
                .collect();
            assert_eq!(
                T.size_with(64, |i| core::array::from_fn(|_| AtomicU64::new(i as u64))),
                Ok(64)
            );
            for r in readers {
                assert_eq!(r.join().unwrap(), 4 * 63);
            }
        });
    }
}
