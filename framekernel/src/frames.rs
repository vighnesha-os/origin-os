//! The physical frame allocator (CLAUDE.md §28: physical page allocator), the second M2
//! step.
//!
//! One bit per 4 KiB frame, set while the frame is used. Only frames inside `Usable`
//! regions of the boot info ever start free; everything else (firmware, ACPI, MMIO, the
//! kernel image, its stack, the boot info, loader memory) stays used. The bitmap lives in
//! memory the caller provides, so this module needs no `unsafe` and is tested on the host.

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
}

/// End of the highest `Usable` region: the memory the bitmap must cover.
pub fn usable_end(regions: &[MemoryRegion]) -> u64 {
    regions
        .iter()
        .filter(|r| r.kind == RegionKind::Usable as u32)
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
        regions: &[MemoryRegion],
        reserved: &[(u64, u64)],
    ) -> Result<Self, FrameError> {
        let end = usable_end(regions);
        let needed = words_for(end);
        let provided = bitmap.len() as u64;
        if provided < needed {
            return Err(FrameError::BitmapTooSmall { needed, provided });
        }
        bitmap.fill(u64::MAX);
        let mut a = Self {
            bitmap,
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

    /// Returns the frame at `addr` to the allocator.
    pub fn free(&mut self, addr: u64) -> Result<(), FrameError> {
        if !addr.is_multiple_of(FRAME) {
            return Err(FrameError::Misaligned(addr));
        }
        let frame = addr / FRAME;
        if frame >= self.frames || frame == 0 {
            return Err(FrameError::OutOfRange(addr));
        }
        let used = self
            .bitmap
            .get((frame / BITS) as usize)
            .is_some_and(|w| w & (1 << (frame % BITS)) != 0);
        if !used {
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
        assert_eq!(a.free_frames(), before - 1);
        // Freeing the other allocation restores the original count exactly.
        assert_eq!(a.free(y), Ok(()));
        assert_eq!(a.free_frames(), before);
    }
}
