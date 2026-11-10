//! The physical frame allocator (CLAUDE.md §28: physical page allocator), the second M2
//! step.
//!
//! One bit per 4 KiB frame, set while the frame is used. Only frames inside `Usable`
//! regions of the boot info ever start free; everything else (firmware, ACPI, MMIO, the
//! kernel image, its stack, the boot info, loader memory) stays used. The bitmap lives in
//! memory the caller provides, so this module needs no `unsafe` and is tested on the host.
#![forbid(unsafe_code)]

use origin_boot_info::{MemoryRegion, RegionKind};

/// Frame size.
pub const FRAME: u64 = 4096;
const BITS: u64 = 64;

/// Why the allocator refused something.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// The bitmap has fewer words than the usable memory needs.
    BitmapTooSmall {
        /// Words needed.
        needed: u64,
        /// Words provided.
        provided: u64,
    },
    /// An address is not 4 KiB aligned.
    Misaligned(u64),
    /// An address is beyond the memory the bitmap covers.
    OutOfRange(u64),
    /// The frame is already free.
    DoubleFree(u64),
    /// A run of zero frames was asked for.
    EmptyRun,
    /// The frame was never allocatable (outside every `Usable` region, frame 0, or
    /// reserved), so freeing it would hand out memory the allocator does not own.
    NotAllocatable(u64),
    /// More reserved ranges than [`MAX_RESERVED`].
    TooManyReserved,
}

/// Most reserved ranges [`FrameAllocator::new`] accepts.
pub const MAX_RESERVED: usize = 8;

/// End of the highest `Usable` region: the memory the bitmap must cover.
pub fn usable_end(regions: &[MemoryRegion]) -> u64 {
    regions
        .iter()
        .filter(|r| r.kind == RegionKind::Usable as u32)
        .map(|r| r.base.saturating_add(r.pages.saturating_mul(FRAME)))
        .max()
        .unwrap_or(0)
}

/// End of the highest region the allocator may ever own: `Usable` now, or
/// `LoaderReclaimable` once [`FrameAllocator::reclaim`] returns it. The bitmap covers this
/// much, so reclaiming needs no new bitmap.
pub fn covered_end(regions: &[MemoryRegion]) -> u64 {
    regions
        .iter()
        .filter(|r| {
            r.kind == RegionKind::Usable as u32 || r.kind == RegionKind::LoaderReclaimable as u32
        })
        .map(|r| r.base.saturating_add(r.pages.saturating_mul(FRAME)))
        .max()
        .unwrap_or(0)
}

/// Bitmap words needed to cover memory up to `end`.
pub const fn words_for(end: u64) -> u64 {
    end.div_ceil(FRAME).div_ceil(BITS)
}

/// A bitmap allocator of 4 KiB physical frames.
#[derive(Debug)]
pub struct FrameAllocator<'a> {
    bitmap: &'a mut [u64],
    regions: &'a [MemoryRegion],
    /// Region kinds, besides `Usable`, returned by [`Self::reclaim`] (bit = kind value).
    reclaimed: u32,
    reserved: [(u64, u64); MAX_RESERVED],
    frames: u64,
    next: u64,
    free: u64,
}

impl<'a> FrameAllocator<'a> {
    /// Builds the allocator over `bitmap`. Frames in `Usable` regions start free, except
    /// frame 0 (so no allocation is ever the null address) and the `reserved` ranges
    /// (`(base, pages)`, for example the bitmap's own frames).
    pub fn new(
        bitmap: &'a mut [u64],
        regions: &'a [MemoryRegion],
        reserved: &[(u64, u64)],
    ) -> Result<Self, FrameError> {
        let mut kept = [(0, 0); MAX_RESERVED];
        if reserved.len() > MAX_RESERVED {
            return Err(FrameError::TooManyReserved);
        }
        for (slot, range) in kept.iter_mut().zip(reserved) {
            *slot = *range;
        }
        let end = covered_end(regions);
        let needed = words_for(end);
        let provided = bitmap.len() as u64;
        if provided < needed {
            return Err(FrameError::BitmapTooSmall { needed, provided });
        }
        bitmap.fill(u64::MAX);
        let mut a = Self {
            bitmap,
            regions,
            reclaimed: 0,
            reserved: kept,
            frames: end / FRAME,
            next: 0,
            free: 0,
        };
        for r in regions
            .iter()
            .filter(|r| r.kind == RegionKind::Usable as u32)
        {
            let first = r.base / FRAME;
            for frame in first..first.saturating_add(r.pages) {
                a.set_free(frame, true);
            }
        }
        a.set_free(0, false);
        for &(base, pages) in reserved {
            let first = base / FRAME;
            for frame in first..first.saturating_add(pages) {
                a.set_free(frame, false);
            }
        }
        Ok(a)
    }

    /// Marks `frame` free or used, keeping the free count exact.
    fn set_free(&mut self, frame: u64, free: bool) {
        if frame >= self.frames {
            return;
        }
        let (word, bit) = ((frame / BITS) as usize, frame % BITS);
        if let Some(w) = self.bitmap.get_mut(word) {
            let was_free = *w & (1 << bit) == 0;
            if free && !was_free {
                *w &= !(1 << bit);
                self.free += 1;
            } else if !free && was_free {
                *w |= 1 << bit;
                self.free -= 1;
            }
        }
    }

    /// Free frames.
    pub const fn free_frames(&self) -> u64 {
        self.free
    }

    /// Allocates one frame and returns its physical address, or `None` when none is free.
    /// Next-fit: the search continues after the last allocation.
    pub fn allocate(&mut self) -> Option<u64> {
        if self.free == 0 {
            return None;
        }
        let words = self.frames.div_ceil(BITS);
        let start = self.next / BITS;
        for i in 0..words {
            let index = (start + i) % words;
            let Some(&word) = self.bitmap.get(index as usize) else {
                continue;
            };
            if word == u64::MAX {
                continue;
            }
            let frame = index * BITS + u64::from((!word).trailing_zeros());
            if frame >= self.frames {
                continue;
            }
            self.set_free(frame, false);
            self.next = frame + 1;
            return Some(frame * FRAME);
        }
        None
    }

    /// Whether `frame` is one this allocator may ever hand out: inside a `Usable` region,
    /// not frame 0 and not reserved.
    fn allocatable(&self, frame: u64) -> bool {
        let addr = frame.saturating_mul(FRAME);
        let inside = |base: u64, pages: u64| {
            addr >= base && addr < base.saturating_add(pages.saturating_mul(FRAME))
        };
        let owned = |kind: u32| {
            kind == RegionKind::Usable as u32 || (kind < 32 && self.reclaimed & (1 << kind) != 0)
        };
        frame != 0
            && self
                .regions
                .iter()
                .any(|r| owned(r.kind) && inside(r.base, r.pages))
            && !self
                .reserved
                .iter()
                .any(|&(base, pages)| inside(base, pages))
    }

    /// Takes over every region of `kind` (in practice `LoaderReclaimable`, once nothing the
    /// loader or the firmware left there is in use): its frames become free, except frame 0
    /// and the reserved ranges. Returns how many frames were added.
    pub fn reclaim(&mut self, kind: RegionKind) -> u64 {
        let raw = kind as u32;
        if raw == RegionKind::Usable as u32 || raw >= 32 {
            return 0;
        }
        self.reclaimed |= 1 << raw;
        let regions = self.regions;
        let mut added = 0;
        for r in regions.iter().filter(|r| r.kind == raw) {
            let first = r.base / FRAME;
            for frame in first..first.saturating_add(r.pages) {
                if self.allocatable(frame) && !self.is_free(frame) {
                    self.set_free(frame, true);
                    added += 1;
                }
            }
        }
        added
    }

    /// Whether `frame` is free.
    fn is_free(&self, frame: u64) -> bool {
        frame < self.frames
            && self
                .bitmap
                .get((frame / BITS) as usize)
                .is_some_and(|w| w & (1 << (frame % BITS)) == 0)
    }

    /// Whether the frame at `addr` is allocatable and free right now.
    pub fn frame_is_free(&self, addr: u64) -> bool {
        addr.is_multiple_of(FRAME) && self.allocatable(addr / FRAME) && self.is_free(addr / FRAME)
    }

    /// Allocates exactly the frame at `addr`, if it is free: for a proof that needs one
    /// particular frame back. `false` otherwise, and nothing changes.
    pub fn claim(&mut self, addr: u64) -> bool {
        if !self.frame_is_free(addr) {
            return false;
        }
        self.set_free(addr / FRAME, false);
        true
    }

    /// Allocates `count` physically contiguous frames whose first frame number is a
    /// multiple of `align` (in frames; 0 or 1 means any), and returns the address of the
    /// first. First fit, scanning from frame 1; for the rare large allocations only.
    pub fn allocate_run(&mut self, count: u64, align: u64) -> Option<u64> {
        if count == 0 || count > self.free {
            return None;
        }
        let align = align.max(1);
        let mut start = align;
        while start
            .checked_add(count)
            .is_some_and(|end| end <= self.frames)
        {
            match (start..start + count).find(|&f| !self.is_free(f)) {
                None => {
                    for frame in start..start + count {
                        self.set_free(frame, false);
                    }
                    return Some(start * FRAME);
                }
                // Restart after the used frame, rounded up to the alignment.
                Some(used) => start = (used + 1).div_ceil(align) * align,
            }
        }
        None
    }

    /// Returns a run from [`Self::allocate_run`]. Every frame is checked before any is
    /// freed, so a bad call changes nothing.
    pub fn free_run(&mut self, addr: u64, count: u64) -> Result<(), FrameError> {
        if count == 0 {
            return Err(FrameError::EmptyRun);
        }
        if !addr.is_multiple_of(FRAME) {
            return Err(FrameError::Misaligned(addr));
        }
        let first = addr / FRAME;
        let end = first
            .checked_add(count)
            .ok_or(FrameError::OutOfRange(addr))?;
        if first == 0 || end > self.frames {
            return Err(FrameError::OutOfRange(addr));
        }
        if let Some(frame) = (first..end).find(|&f| !self.allocatable(f)) {
            return Err(FrameError::NotAllocatable(frame * FRAME));
        }
        if let Some(frame) = (first..end).find(|&f| self.is_free(f)) {
            return Err(FrameError::DoubleFree(frame * FRAME));
        }
        for frame in first..end {
            self.set_free(frame, true);
        }
        Ok(())
    }

    /// Returns the frame at `addr` to the allocator.
    pub fn free(&mut self, addr: u64) -> Result<(), FrameError> {
        if !addr.is_multiple_of(FRAME) {
            return Err(FrameError::Misaligned(addr));
        }
        let frame = addr / FRAME;
        if frame >= self.frames || frame == 0 {
            return Err(FrameError::OutOfRange(addr));
        }
        if !self.allocatable(frame) {
            return Err(FrameError::NotAllocatable(addr));
        }
        if self.is_free(frame) {
            return Err(FrameError::DoubleFree(addr));
        }
        self.set_free(frame, true);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn a_frame_can_be_claimed_only_while_it_is_free() {
        let regions = [MemoryRegion {
            base: FRAME,
            pages: 8,
            kind: RegionKind::Usable as u32,
            reserved: 0,
        }];
        let mut bitmap = vec![0u64; 4];
        let mut a = FrameAllocator::new(&mut bitmap, &regions, &[]).unwrap();
        let before = a.free_frames();
        let frame = 4 * FRAME;
        assert!(a.frame_is_free(frame));
        assert!(a.claim(frame));
        assert!(!a.frame_is_free(frame), "taken");
        assert_eq!(a.free_frames(), before - 1);
        assert!(!a.claim(frame), "twice");
        assert_eq!(a.free_frames(), before - 1);
        a.free(frame).unwrap();
        assert!(a.claim(frame), "free again");
        // What the allocator does not own, or is not a frame, cannot be claimed.
        assert!(!a.claim(frame + 1));
        assert!(!a.claim(0));
        assert!(!a.claim(100 * FRAME));
        assert!(!a.frame_is_free(100 * FRAME));
    }

    fn region(base: u64, pages: u64, kind: RegionKind) -> MemoryRegion {
        MemoryRegion {
            base,
            pages,
            kind: kind as u32,
            reserved: 0,
        }
    }

    fn sample() -> Vec<MemoryRegion> {
        vec![
            region(0x0, 0xA0, RegionKind::Usable),        // low 640 KiB
            region(0xA_0000, 0x60, RegionKind::Reserved), // legacy hole
            region(0x10_0000, 0x100, RegionKind::Usable), // 1 MiB
            region(0x20_0000, 0x10, RegionKind::KernelImage),
            region(0x21_0000, 0x200, RegionKind::LoaderReclaimable),
            region(0x41_0000, 0x40, RegionKind::Usable),
            region(0xFEC0_0000, 1, RegionKind::Mmio), // beyond usable_end: ignored
        ]
    }

    fn is_usable(addr: u64, regions: &[MemoryRegion]) -> bool {
        regions.iter().any(|r| {
            r.kind == RegionKind::Usable as u32 && addr >= r.base && addr < r.base + r.pages * FRAME
        })
    }

    #[test]
    fn sizes_cover_usable_memory_only() {
        let regions = sample();
        assert_eq!(usable_end(&regions), 0x45_0000);
        assert_eq!(words_for(0x45_0000), (0x450u64).div_ceil(64));
        let mut small = vec![0u64; 2];
        assert!(matches!(
            FrameAllocator::new(&mut small, &regions, &[]),
            Err(FrameError::BitmapTooSmall { .. })
        ));
    }

    #[test]
    fn hands_out_every_usable_frame_once_and_nothing_else() {
        let regions = sample();
        let mut bitmap = vec![0u64; words_for(usable_end(&regions)) as usize];
        let reserved = [(0x10_0000, 2)]; // e.g. the bitmap's own frames
        let mut a = FrameAllocator::new(&mut bitmap, &regions, &reserved).unwrap();
        // 0xA0 + 0x100 + 0x40 usable frames, minus frame 0 and the 2 reserved frames.
        let expected = 0xA0 + 0x100 + 0x40 - 1 - 2;
        assert_eq!(a.free_frames(), expected);
        let mut seen = HashSet::new();
        while let Some(addr) = a.allocate() {
            assert!(is_usable(addr, &regions), "{addr:#x} is not usable");
            assert_ne!(addr, 0);
            assert!(
                !(0x10_0000..0x10_2000).contains(&addr),
                "{addr:#x} is reserved"
            );
            assert!(seen.insert(addr), "{addr:#x} handed out twice");
        }
        assert_eq!(seen.len() as u64, expected);
        assert_eq!(a.free_frames(), 0);
        assert_eq!(a.allocate(), None);
    }

    #[test]
    fn runs_are_contiguous_aligned_usable_and_returnable() {
        let regions = sample();
        let mut bitmap = vec![0u64; words_for(usable_end(&regions)) as usize];
        let mut a = FrameAllocator::new(&mut bitmap, &regions, &[]).unwrap();
        let before = a.free_frames();
        // 0x100 frames at 1 MiB: a run of 0x100 fits only there.
        let big = a.allocate_run(0x100, 1).unwrap();
        assert_eq!(big, 0x10_0000);
        assert_eq!(a.free_frames(), before - 0x100);
        assert_eq!(a.allocate_run(0x100, 1), None, "no second run that long");
        // Aligned to 16 frames (64 KiB), inside usable memory.
        let aligned = a.allocate_run(5, 16).unwrap();
        assert!(aligned.is_multiple_of(16 * FRAME));
        for f in 0..5 {
            assert!(is_usable(aligned + f * FRAME, &regions));
        }
        // Freeing checks everything first.
        assert_eq!(a.free_run(big, 0), Err(FrameError::EmptyRun));
        assert_eq!(a.free_run(big + 1, 1), Err(FrameError::Misaligned(big + 1)));
        assert_eq!(a.free_run(0, 1), Err(FrameError::OutOfRange(0)));
        // The run's last frame plus the kernel-image frame after it.
        let last = big + 0xFF * FRAME;
        assert_eq!(
            a.free_run(last, 2),
            Err(FrameError::NotAllocatable(last + FRAME))
        );
        // A run that reaches a frame that is free.
        a.free(aligned + 4 * FRAME).unwrap();
        assert_eq!(
            a.free_run(aligned, 5),
            Err(FrameError::DoubleFree(aligned + 4 * FRAME))
        );
        let again = a.allocate_run(1, 1).unwrap();
        assert_eq!(again, 0x1000, "first fit from frame 1");
        a.free(again).unwrap();
        // Take the freed frame back so the run below is whole again.
        let mut taken = Vec::new();
        while let Some(f) = a.allocate() {
            if f == aligned + 4 * FRAME {
                break;
            }
            taken.push(f);
        }
        for f in taken {
            a.free(f).unwrap();
        }
        assert_eq!(a.free_run(big, 0x100), Ok(()));
        assert_eq!(a.free_run(aligned, 5), Ok(()));
        assert_eq!(a.free_frames(), before);
        assert_eq!(a.free_run(big, 1), Err(FrameError::DoubleFree(big)));
        assert_eq!(a.allocate_run(0, 1), None);
        assert_eq!(a.allocate_run(before + 1, 1), None);
    }

    #[test]
    fn reclaimed_regions_become_allocatable_and_nothing_else_does() {
        let regions = sample();
        let mut bitmap = vec![0u64; words_for(covered_end(&regions)) as usize];
        let mut a = FrameAllocator::new(&mut bitmap, &regions, &[(0x21_0000, 1)]).unwrap();
        let before = a.free_frames();
        let loader = 0x21_1000; // inside the LoaderReclaimable region
        assert_eq!(a.free(loader), Err(FrameError::NotAllocatable(loader)));
        // 0x200 frames, minus the one reserved.
        assert_eq!(a.reclaim(RegionKind::LoaderReclaimable), 0x1FF);
        assert_eq!(a.free_frames(), before + 0x1FF);
        assert_eq!(a.reclaim(RegionKind::LoaderReclaimable), 0, "idempotent");
        assert_eq!(a.reclaim(RegionKind::Usable), 0);
        let mut seen = HashSet::new();
        while let Some(f) = a.allocate() {
            assert!(f != 0x21_0000, "the reserved frame stays reserved");
            assert!(!(0x20_0000..0x21_0000).contains(&f), "kernel image");
            seen.insert(f);
        }
        assert!(seen.contains(&loader));
        assert_eq!(a.free(loader), Ok(()), "reclaimed frames can be freed");
        assert_eq!(
            a.free(0x20_0000),
            Err(FrameError::NotAllocatable(0x20_0000))
        );
    }

    #[test]
    fn free_returns_frames_and_rejects_bad_addresses() {
        let regions = sample();
        let mut bitmap = vec![0u64; words_for(usable_end(&regions)) as usize];
        let mut a = FrameAllocator::new(&mut bitmap, &regions, &[]).unwrap();
        let before = a.free_frames();
        let x = a.allocate().unwrap();
        let y = a.allocate().unwrap();
        assert_ne!(x, y);
        assert_eq!(a.free_frames(), before - 2);
        assert_eq!(a.free(x), Ok(()));
        assert_eq!(a.free(x), Err(FrameError::DoubleFree(x)));
        assert_eq!(a.free(x + 1), Err(FrameError::Misaligned(x + 1)));
        assert_eq!(a.free(0), Err(FrameError::OutOfRange(0)));
        assert_eq!(
            a.free(0x1000_0000),
            Err(FrameError::OutOfRange(0x1000_0000))
        );
        // Reserved, kernel-image and legacy-hole frames were never allocatable.
        for addr in [0x20_0000, 0x21_0000, 0xA_0000] {
            assert_eq!(a.free(addr), Err(FrameError::NotAllocatable(addr)));
        }
        assert_eq!(a.free_frames(), before - 1);
        // Freeing the other allocation restores the original count exactly.
        assert_eq!(a.free(y), Ok(()));
        assert_eq!(a.free_frames(), before);
    }
}
