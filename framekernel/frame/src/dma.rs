//! Memory a device may read and write (CLAUDE.md §31 DMA mapping, §58; ADR-0007; ADR-0011 N6).
//!
//! A [`DmaFrame`] is a zeroed page from the kernel heap, kept for ever: a device may use
//! it until it is reset, and the kernel has no way yet to tell when one has stopped, so a
//! frame is never given back. Everything a driver does to one goes through the volatile
//! accessors here: the device can write the page at any moment, so no Rust reference ever
//! covers it, and a word is read or written with one aligned access of its own size.
//! Two drivers (or a driver and a device) racing on the same word is a logic error of theirs,
//! not a memory-safety one: this memory is never a Rust object.
//!
//! The address a device uses is an [`Iova`]; a [`DmaPage`] is a frame together with the
//! address the owning device was told to use for it.

use alloc::alloc::{Layout, alloc_zeroed};

use crate::addr::{Iova, PhysAddr};

/// The bytes of one DMA frame.
pub const FRAME_BYTES: usize = 4096;

/// A zeroed 4 KiB page, in the kernel's memory, that a device may use. Kept for ever.
#[derive(Clone, Copy, Debug)]
pub struct DmaFrame {
    /// Where the kernel reaches it, through the direct map.
    virt: usize,
    phys: PhysAddr,
}

impl DmaFrame {
    /// A new zeroed page, or `None` if the heap has no room.
    pub fn new() -> Option<Self> {
        let layout = Layout::from_size_align(FRAME_BYTES, FRAME_BYTES).ok()?;
        // SAFETY: `layout` has a non-zero size. The page is deliberately never freed: a device
        // may use it for as long as the kernel runs.
        let page = unsafe { alloc_zeroed(layout) };
        if page.is_null() {
            return None;
        }
        let virt = page.expose_provenance();
        let phys = crate::phys::to_phys(virt as u64);
        Some(Self {
            virt,
            phys: PhysAddr::new(phys),
        })
    }

    /// The page's physical address.
    pub const fn phys(self) -> PhysAddr {
        self.phys
    }

    /// Where a `T` at byte `offset` lives, if inside the page and aligned for `T`.
    fn field<T>(self, offset: usize) -> Option<*mut T> {
        let end = offset.checked_add(size_of::<T>())?;
        (end <= FRAME_BYTES
            && self
                .virt
                .wrapping_add(offset)
                .is_multiple_of(align_of::<T>()))
        .then(|| core::ptr::with_exposed_provenance_mut::<T>(self.virt.wrapping_add(offset)))
    }

    /// Reads the `T` at `offset` (zero if out of bounds or misaligned).
    fn load<T: Copy + Default>(self, offset: usize) -> T {
        self.field::<T>(offset).map_or_else(T::default, |p| {
            // SAFETY: inside this kept-for-ever page and aligned (`field`); a volatile load
            // forms no reference, and the device may be writing it concurrently.
            unsafe { p.read_volatile() }
        })
    }

    /// Writes the `T` at `offset` (nothing if out of bounds or misaligned).
    fn store<T: Copy>(self, offset: usize, value: T) {
        if let Some(p) = self.field::<T>(offset) {
            // SAFETY: as for `load`; a volatile store, and this page is the device's and the
            // driver's to agree about, not a Rust object.
            unsafe { p.write_volatile(value) };
        }
    }

    /// Reads a `u8` at `offset`.
    pub fn read8(self, offset: usize) -> u8 {
        self.load(offset)
    }

    /// Reads a `u16` at `offset`.
    pub fn read16(self, offset: usize) -> u16 {
        self.load(offset)
    }

    /// Reads a `u32` at `offset`.
    pub fn read32(self, offset: usize) -> u32 {
        self.load(offset)
    }

    /// Reads a `u64` at `offset`.
    pub fn read64(self, offset: usize) -> u64 {
        self.load(offset)
    }

    /// Writes a `u8` at `offset`.
    pub fn write8(self, offset: usize, value: u8) {
        self.store(offset, value);
    }

    /// Writes a `u16` at `offset`.
    pub fn write16(self, offset: usize, value: u16) {
        self.store(offset, value);
    }

    /// Writes a `u32` at `offset`.
    pub fn write32(self, offset: usize, value: u32) {
        self.store(offset, value);
    }

    /// Writes a `u64` at `offset`.
    pub fn write64(self, offset: usize, value: u64) {
        self.store(offset, value);
    }

    /// Copies `out.len()` bytes from `offset` into `out`, a byte at a time, with volatile
    /// reads; `false` if they are not all inside the page.
    pub fn copy_out(self, offset: usize, out: &mut [u8]) -> bool {
        if !fits(offset, out.len()) {
            return false;
        }
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = self.load(offset + i);
        }
        true
    }

    /// Copies `bytes` to `offset`, a byte at a time, with volatile writes; `false` if they
    /// are not all inside the page.
    pub fn copy_in(self, offset: usize, bytes: &[u8]) -> bool {
        if !fits(offset, bytes.len()) {
            return false;
        }
        for (i, &byte) in bytes.iter().enumerate() {
            self.store(offset + i, byte);
        }
        true
    }

    /// Copies `bytes` to the start of the page (as much as fits), with volatile writes.
    pub fn fill(self, bytes: &[u8]) {
        for (offset, &byte) in bytes.iter().enumerate().take(FRAME_BYTES) {
            self.store(offset, byte);
        }
    }

    /// The page's bytes, copied out once with volatile reads, for when no command that writes
    /// it is outstanding.
    pub fn to_bytes(self) -> [u8; FRAME_BYTES] {
        let mut out = [0u8; FRAME_BYTES];
        self.copy_out(0, &mut out);
        out
    }
}

/// Whether `[offset, offset + len)` is inside a page.
const fn fits(offset: usize, len: usize) -> bool {
    matches!(offset.checked_add(len), Some(end) if end <= FRAME_BYTES)
}

/// A DMA frame and the address the owning device reaches it at.
#[derive(Clone, Copy, Debug)]
pub struct DmaPage {
    frame: DmaFrame,
    bus: Iova,
}

impl DmaPage {
    /// A frame with its device address: the address the owning device was told to use for it.
    pub const fn new(frame: DmaFrame, bus: Iova) -> Self {
        Self { frame, bus }
    }

    /// The frame.
    pub const fn frame(self) -> DmaFrame {
        self.frame
    }

    /// The address the device uses.
    pub const fn bus(self) -> Iova {
        self.bus
    }
}

impl core::ops::Deref for DmaPage {
    type Target = DmaFrame;

    fn deref(&self) -> &DmaFrame {
        &self.frame
    }
}
