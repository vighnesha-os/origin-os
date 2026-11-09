//! Device DMA memory (CLAUDE.md §31 DMA mapping, §58 DMA protection; ADR-0007): pages a
//! device may read and write, and the address the device must use for each.
//!
//! With the IOMMU translating, a driver first attaches its device ([`DeviceDma::attach`]),
//! which gives the device a domain of its own; every page then comes mapped there, and the
//! address the device uses (the bus address) is an IOVA, never the page's physical address.
//! Without an IOMMU — or for a virtio device that did not offer `VIRTIO_F_ACCESS_PLATFORM`,
//! which by virtio 1.1 §6 uses physical addresses — the bus address is the physical address.
#![forbid(unsafe_code)]

use crate::pci::{self, Address, DmaPath};
use origin_frame::addr::Iova;
use origin_frame::dma::{DmaFrame, DmaPage};

/// How one device reaches memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeviceDma {
    /// Through its own IOMMU domain: bus addresses are IOVAs that domain maps.
    Translated(crate::iommu::Domain),
    /// Directly: bus addresses are physical addresses.
    Direct,
}

impl DeviceDma {
    /// Whether a driver may bind at all now, and how its device may reach memory: refused if
    /// the quiesce failed or an IOMMU is present but did not come up (`pci::dma_path`).
    fn path() -> Result<DmaPath, &'static str> {
        pci::dma_path(crate::pcie::quiesced(), crate::iommu::status())
    }

    /// The DMA path for the PCI function at `at`: a domain of its own when the IOMMU
    /// translates (from then on, it reaches only pages mapped for it); direct only on a
    /// platform with no IOMMU at all. Refused if the quiesce failed or the IOMMU did not
    /// come up.
    pub(crate) fn attach(at: Address) -> Result<Self, &'static str> {
        match Self::path()? {
            DmaPath::Domain => crate::iommu::attach(at).map(Self::Translated),
            DmaPath::Direct => Ok(Self::Direct),
        }
    }

    /// Turns bus mastering on for the PCI function at `at`, which this path was attached for
    /// (`attach`). Returns its command register. While the IOMMU translates, a function
    /// without a domain of its own is refused: it would master the bus unconfined.
    ///
    /// A device that was never attached (a virtio device without `VIRTIO_F_ACCESS_PLATFORM`
    /// is handed [`Self::Direct`]) gets the same check as [`attach`](Self::attach): direct
    /// is refused unless the platform has no IOMMU at all.
    pub(crate) fn enable_bus_master(&self, at: Address) -> Result<u16, &'static str> {
        match (*self, Self::path()?) {
            (Self::Translated(d), DmaPath::Domain) if crate::iommu::attached(d, at) => {}
            (Self::Translated(_), _) => return Err("this domain does not belong to the device"),
            (Self::Direct, DmaPath::Domain) => {
                return Err("the IOMMU translates and the device has no domain of its own");
            }
            (Self::Direct, DmaPath::Direct) => {}
        }
        crate::pcie::set_bus_master(at)
    }

    /// A new zeroed page the device may read and write, mapped for it. The page is the
    /// framework's and is never freed: on a failed map it stays allocated, because
    /// `iommu::map` installs the page-table entry before its invalidation, which can fail,
    /// so a page whose map failed may still be reachable by the device.
    pub(crate) fn page(&self) -> Option<DmaPage> {
        let frame = DmaFrame::new()?;
        let bus = match *self {
            Self::Translated(d) => crate::iommu::map(d, frame).ok()?,
            Self::Direct => Iova::new(frame.phys().raw()),
        };
        Some(DmaPage::new(frame, bus))
    }
}
