//! How a crash report prints an address (ADR-0004 Amendment 2, A6.2): `origin_paging::layout`
//! tells it ([`Told`]: an offset from the base of the sliding window it is in, or the address
//! itself), and [`Shown`] prints that, never a base, in every mode. Host-tested here;
//! `phys::told` tells the kernel's addresses against its live bases. The printing is here, not in
//! `origin-paging`, whose no-panic check forbids formatting (ONBF §12).
#![forbid(unsafe_code)]

use origin_paging::layout::Told;

/// A told address, printed: `image +0x1f20`, `stack region -0x10`, `direct map +0x7e50000`, or
/// the address itself, `0x400000`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown(pub Told);

impl core::fmt::Display for Shown {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (window, offset) = match self.0 {
            Told::Image(offset) => ("image", offset),
            Told::StackRegion(offset) => ("stack region", offset),
            Told::DirectMap(offset) => ("direct map", offset),
            Told::Address(addr) => return write!(f, "{addr:#x}"),
        };
        let sign = if offset.below { '-' } else { '+' };
        write!(f, "{window} {sign}{:#x}", offset.by)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use origin_paging::layout::{Bases, DIRECT_MAP_WINDOW, KERNEL_WINDOW, stack_region_base, tell};

    /// As printed: an offset and its window's name, above or below the base, and never a base;
    /// an address in no sliding window as itself.
    #[test]
    fn a_told_address_prints_as_an_offset_and_never_a_base() {
        let bases = Bases {
            image: KERNEL_WINDOW + 0x60_0000,
            direct_map: DIRECT_MAP_WINDOW + (7 << 30),
            stack_region: stack_region_base(0x1234_5678),
        };
        let shown = |addr: u64| Shown(tell(addr, &bases)).to_string();
        assert_eq!(shown(bases.image + 0x1f20), "image +0x1f20");
        assert_eq!(shown(KERNEL_WINDOW), "image -0x600000");
        assert_eq!(
            shown(bases.stack_region + 0x1_fff8),
            "stack region +0x1fff8"
        );
        assert_eq!(shown(bases.stack_region - 0x10), "stack region -0x10");
        assert_eq!(
            shown(bases.direct_map + 0x7e5_0000),
            "direct map +0x7e50000"
        );
        assert_eq!(shown(0x40_0000), "0x400000");
        for addr in [bases.image + 0x1f20, bases.stack_region, bases.direct_map] {
            let text = shown(addr);
            for base in [bases.image, bases.stack_region, bases.direct_map] {
                assert!(!text.contains(&alloc::format!("{base:x}")), "{text}");
            }
        }
    }
}
