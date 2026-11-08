//! Device DMA memory (CLAUDE.md §31 DMA mapping, §58 DMA protection; ADR-0007): pages a
//! device may read and write, and the address the device must use for each.
//!
//! With the IOMMU translating, a driver first attaches its device ([`DeviceDma::attach`]),
//! which gives the device a domain of its own; every page then comes mapped there, and the
//! address the device uses (the bus address) is an IOVA, never the page's physical address.
//! Without an IOMMU — or for a virtio device that did not offer `VIRTIO_F_ACCESS_PLATFORM`,
//! which by virtio 1.1 §6 uses physical addresses — the bus address is the physical address.

use alloc::alloc::{Layout, alloc_zeroed, dealloc};

use framekernel::pci::Address;

/// How one device reaches memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeviceDma {
    /// Through its own IOMMU domain: bus addresses are IOVAs that domain maps.
    Translated(crate::iommu::Domain),
    /// Directly: bus addresses are physical addresses.
    Direct,
}

/// A zeroed 4 KiB page a device may read and write, kept for ever (the device may use it
/// until it is reset): where the kernel reaches it, and where the device does.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DmaPage {
    pub(crate) virt: u64,
    pub(crate) bus: u64,
}

impl DeviceDma {
    /// The DMA path for the PCI function at `at`: a domain of its own when the IOMMU
    /// translates (from then on, it reaches only pages mapped for it), else direct.
    pub(crate) fn attach(at: Address) -> Result<Self, &'static str> {
        if crate::iommu::translating() {
            crate::iommu::attach(at).map(Self::Translated)
        } else {
            Ok(Self::Direct)
        }
    }

    /// Turns bus mastering on for the PCI function at `at`, which this path was attached for
    /// (`attach`). Returns its command register. While the IOMMU translates, a function
    /// without a domain of its own is refused: it would master the bus unconfined.
    pub(crate) fn enable_bus_master(&self, at: Address) -> Result<u16, &'static str> {
        match *self {
            Self::Translated(d) if crate::iommu::attached(d, at) => {}
            Self::Translated(_) => return Err("this domain does not belong to the device"),
            Self::Direct if crate::iommu::translating() => {
                return Err("the IOMMU translates and the device has no domain of its own");
            }
            Self::Direct => {}
        }
        crate::pcie::set_bus_master(at)
    }

    /// A new zeroed page the device may read and write, mapped for it.
    pub(crate) fn page(&self) -> Option<DmaPage> {
        let layout = Layout::from_size_align(4096, 4096).ok()?;
        // SAFETY: `layout` has a non-zero size.
        let p = unsafe { alloc_zeroed(layout) };
        if p.is_null() {
            return None;
        }
        let virt = p.expose_provenance() as u64;
        let phys = crate::phys::to_phys(virt);
        let bus = match *self {
            Self::Translated(d) => crate::iommu::map(d, phys).ok(),
            Self::Direct => Some(phys),
        };
        if bus.is_none() {
            // SAFETY: `p` came from `alloc_zeroed` with this layout just above, and no device
            // was given it (the mapping failed).
            unsafe { dealloc(p, layout) };
        }
        Some(DmaPage { virt, bus: bus? })
    }
}
