//! Typed addresses (ADR-0011 N6): a physical address, a device-visible address and a user
//! virtual address are different things, and a raw `u64` confuses them.
//!
//! These are plain numbers with a type: making one is safe, and proves nothing. What keeps
//! the kernel safe is that the framework's safe operations take them only as a reference to
//! something the framework already owns or has checked (a page it allocated, a range it
//! claimed), never as bare authority to reach memory.
#![forbid(unsafe_code)]

use core::fmt;

/// A physical address.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct PhysAddr(u64);

impl PhysAddr {
    /// The physical address `raw`.
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The number.
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// `self + offset`, if it fits.
    pub const fn checked_add(self, offset: u64) -> Option<Self> {
        match self.0.checked_add(offset) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }
}

impl fmt::LowerHex for PhysAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, f)
    }
}

impl fmt::Display for PhysAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

/// The address a device uses for DMA (a bus address): through the IOMMU, an IOVA in the
/// device's own domain; with no IOMMU, the page's physical address.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Iova(u64);

impl Iova {
    /// The address the device must use for `raw`.
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The number, for programming into the device.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::LowerHex for Iova {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_only_numbers_with_a_type() {
        let p = PhysAddr::new(0x1000);
        assert_eq!(p.raw(), 0x1000);
        assert_eq!(p.checked_add(0x20), Some(PhysAddr::new(0x1020)));
        assert_eq!(p.checked_add(u64::MAX), None);
        assert_eq!(format!("{p}"), "0x1000");
        assert_eq!(format!("{p:#x}"), "0x1000");
        let i = Iova::new(0x4000_0000);
        assert_eq!(i.raw(), 0x4000_0000);
    }
}
