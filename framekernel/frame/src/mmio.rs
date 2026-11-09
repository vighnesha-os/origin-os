//! Device registers (ADR-0011 N6; CLAUDE.md §31, §32 BAR mapping): a window of uncached
//! device memory, reached only through bounds-checked volatile accesses.
//!
//! An [`MmioRegion`] is made only by [`crate::vm::map_mmio`], so it always covers registers that
//! were mapped uncached, never RAM. Safe code gets no raw address out of it: every access goes
//! through the methods below, which refuse an offset outside the region or misaligned for the
//! access (a read returns 0, a write does nothing, the same as a device that is not there).
//! The accesses are volatile and go to device registers, not Rust objects, so two regions over
//! the same registers, or a device changing them, is the hardware's business and no data race.
//! A region can be shared between processors (`Copy`): the device defines what its registers
//! do when written from two places at once; drivers serialize that themselves.

use crate::addr::PhysAddr;

/// A window of device registers.
#[derive(Clone, Copy, Debug)]
pub struct MmioRegion {
    /// The window's virtual address (never 0).
    base: usize,
    len: usize,
    phys: PhysAddr,
}

impl MmioRegion {
    /// A region over the `len` bytes of already-mapped device memory at `base`.
    ///
    /// # Safety
    ///
    /// `[base, base + len)` must be mapped uncached and read/write, stay mapped for ever,
    /// be device registers that are not RAM, and be the registers at `phys`.
    #[cfg_attr(not(any(test, target_os = "none")), allow(dead_code))]
    pub(crate) unsafe fn from_mapped(base: usize, len: usize, phys: PhysAddr) -> Self {
        Self { base, len, phys }
    }

    /// The window's length in bytes.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// The window's virtual address, for reports and for `vm::translate`. A number: it gives
    /// no way to touch the registers except through this region.
    pub const fn virt(&self) -> u64 {
        self.base as u64
    }

    /// Whether the window is empty.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The physical address of the window's first byte.
    pub const fn phys(&self) -> PhysAddr {
        self.phys
    }

    /// The window of `len` bytes at `offset`, if it is inside this one.
    pub fn slice(&self, offset: usize, len: usize) -> Option<Self> {
        if !fits(offset, len, self.len) {
            return None;
        }
        Some(Self {
            base: self.base.wrapping_add(offset),
            len,
            phys: self.phys.checked_add(offset as u64)?,
        })
    }

    /// Where a `T` at `offset` lives, if it is inside the window and aligned.
    fn field<T>(&self, offset: usize) -> Option<*mut T> {
        fits(offset, size_of::<T>(), self.len)
            .then(|| self.base.wrapping_add(offset))
            .filter(|addr| addr.is_multiple_of(align_of::<T>()))
            .map(core::ptr::with_exposed_provenance_mut)
    }

    /// Reads the `u8` register at `offset` (0 if it is outside the window).
    pub fn read8(&self, offset: usize) -> u8 {
        self.field::<u8>(offset).map_or(0, |p| {
            // SAFETY: inside this mapped, uncached window (`field`); a volatile load of a
            // device register, with no reference formed.
            unsafe { p.read_volatile() }
        })
    }

    /// Reads the `u16` register at `offset` (0 if outside the window or misaligned).
    pub fn read16(&self, offset: usize) -> u16 {
        self.field::<u16>(offset).map_or(0, |p| {
            // SAFETY: as for `read8`, 2 bytes, aligned (`field`).
            unsafe { p.read_volatile() }
        })
    }

    /// Reads the `u32` register at `offset` (0 if outside the window or misaligned).
    pub fn read32(&self, offset: usize) -> u32 {
        self.field::<u32>(offset).map_or(0, |p| {
            // SAFETY: as for `read8`, 4 bytes, aligned (`field`).
            unsafe { p.read_volatile() }
        })
    }

    /// Reads the `u64` register at `offset` (0 if outside the window or misaligned).
    pub fn read64(&self, offset: usize) -> u64 {
        self.field::<u64>(offset).map_or(0, |p| {
            // SAFETY: as for `read8`, 8 bytes, aligned (`field`).
            unsafe { p.read_volatile() }
        })
    }

    /// Writes the `u8` register at `offset` (nothing if outside the window).
    pub fn write8(&self, offset: usize, value: u8) {
        if let Some(p) = self.field::<u8>(offset) {
            // SAFETY: as for `read8`; a volatile store to a device register.
            unsafe { p.write_volatile(value) };
        }
    }

    /// Writes the `u16` register at `offset` (nothing if outside the window or misaligned).
    pub fn write16(&self, offset: usize, value: u16) {
        if let Some(p) = self.field::<u16>(offset) {
            // SAFETY: as for `write8`, 2 bytes, aligned (`field`).
            unsafe { p.write_volatile(value) };
        }
    }

    /// Writes the `u32` register at `offset` (nothing if outside the window or misaligned).
    pub fn write32(&self, offset: usize, value: u32) {
        if let Some(p) = self.field::<u32>(offset) {
            // SAFETY: as for `write8`, 4 bytes, aligned (`field`).
            unsafe { p.write_volatile(value) };
        }
    }

    /// Writes the `u64` register at `offset` (nothing if outside the window or misaligned).
    pub fn write64(&self, offset: usize, value: u64) {
        if let Some(p) = self.field::<u64>(offset) {
            // SAFETY: as for `write8`, 8 bytes, aligned (`field`).
            unsafe { p.write_volatile(value) };
        }
    }
}

/// Whether `[offset, offset + size)` is inside `[0, len)` without overflow.
const fn fits(offset: usize, size: usize, len: usize) -> bool {
    matches!(offset.checked_add(size), Some(end) if end <= len)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A region over a live, ordinary buffer: the accessors are bounds checks and volatile
    /// loads and stores, which behave the same on memory, whatever the address means.
    fn over(buffer: &mut [u64; 4]) -> MmioRegion {
        // SAFETY: `buffer` stays alive for the test, and is written only through the region.
        unsafe {
            MmioRegion::from_mapped(
                buffer.as_mut_ptr().expose_provenance(),
                32,
                PhysAddr::new(0x1000),
            )
        }
    }

    #[test]
    fn reads_and_writes_inside_the_window() {
        let mut buffer = [0u64; 4];
        let region = over(&mut buffer);
        region.write32(4, 0xAABB_CCDD);
        assert_eq!(region.read32(4), 0xAABB_CCDD);
        region.write64(8, 0x1122_3344_5566_7788);
        assert_eq!(region.read64(8), 0x1122_3344_5566_7788);
        assert_eq!(region.read8(8), 0x88);
        assert_eq!(region.read16(8), 0x7788);
        region.write8(31, 0x5A);
        assert_eq!(region.read8(31), 0x5A);
        assert_eq!(buffer[1], 0x1122_3344_5566_7788);
        assert_eq!(buffer[0], 0xAABB_CCDD_0000_0000);
    }

    #[test]
    fn refuses_what_is_outside_or_misaligned() {
        let mut buffer = [0u64; 4];
        let region = over(&mut buffer);
        // Outside: reads as zero, a write does nothing.
        assert_eq!(region.read32(32), 0);
        assert_eq!(region.read64(28), 0);
        region.write32(32, 0xFFFF_FFFF);
        region.write64(usize::MAX - 7, u64::MAX);
        // Misaligned.
        region.write32(2, 0xFFFF_FFFF);
        assert_eq!(region.read16(1), 0);
        assert_eq!(buffer, [0; 4]);
    }

    #[test]
    fn a_slice_is_a_window_inside_the_window() {
        let mut buffer = [0u64; 4];
        let region = over(&mut buffer);
        let tail = region.slice(16, 16).expect("inside");
        assert_eq!(tail.len(), 16);
        assert_eq!(tail.phys(), PhysAddr::new(0x1010));
        tail.write32(0, 7);
        assert_eq!(region.read32(16), 7);
        assert!(region.slice(16, 17).is_none());
        assert!(region.slice(40, 1).is_none());
        assert!(region.slice(usize::MAX, 2).is_none());
        assert_eq!(region.slice(32, 0).map(|r| r.len()), Some(0));
    }
}
