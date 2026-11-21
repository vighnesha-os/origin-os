//! Bench builds only (TASKS item 112): how a bench loader hands a bench kernel the I/O port its
//! breadcrumbs go to. This is not boot info. It travels in RSI, beside the boot info's address in
//! RDI, from the bench entry stub to `kernel_main`, and nothing but a bench build reads or writes
//! it: the module exists only under `cfg(origin_bench)`, and in the host tests.
//!
//! RSI holds `"ORIGIN"` in its high 48 bits and the port in its low 16. A normal loader's entry
//! stub leaves RSI holding the boot info's address, a direct-map address whose high bits never
//! spell `"ORIGIN"`, so a bench kernel started by a normal loader writes no breadcrumb and boots
//! as it would otherwise.

/// `"ORIGIN"` in ASCII: the high 48 bits of the register.
pub const MAGIC: u64 = 0x4F52_4947_494E;

/// The register value that hands `port` over, or 0 (no breadcrumbs) when `port` is 0.
pub const fn register(port: u16) -> u64 {
    if port == 0 {
        0
    } else {
        (MAGIC << 16) | port as u64
    }
}

/// The port `register` hands over: `None` unless its high 48 bits are [`MAGIC`] and its port
/// passes the rule [`crate::EarlyUart::check`] applies, `origin_spcr::port_block_allowed`, so no
/// register can send breadcrumbs to a port boot info could not name (TASKS item 112's review,
/// R112-3).
pub fn port(register: u64) -> Option<u16> {
    let port = (register & 0xFFFF) as u16;
    (register >> 16 == MAGIC && port != 0 && origin_spcr::port_block_allowed(port)).then_some(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_goes_over_and_comes_back() {
        for port in [0x2F8u16, 0x3F8, 0x3E8, 0x2E8, 0x1000, 0xFFF8] {
            assert_eq!(port_of(register(port)), Some(port));
        }
        // The magic with a port boot info could not name: the i8042's, the PIC's, one whose
        // eight registers would pass the top of the port space.
        for port in [0x60u16, 0x20, 0xFFF9] {
            assert_eq!(port_of(register(port)), None, "{port:#x}");
        }
        assert_eq!(register(0x2F8), 0x4F52_4947_494E_02F8);
    }

    fn port_of(value: u64) -> Option<u16> {
        port(value)
    }

    #[test]
    fn no_magic_no_breadcrumbs() {
        // What a normal loader's stub leaves in RSI: the boot info's address in the direct map
        // (rental C's three boots placed the direct map at these bases).
        for direct_map in [
            0xFFFF_ABB2_0000_0000u64,
            0xFFFF_B52B_8000_0000,
            0xFFFF_B4EC_C000_0000,
        ] {
            assert_eq!(port(direct_map + 0x7E00_1000), None);
        }
        // No port, no magic, a wrong magic, the magic with port 0, the magic shifted wrongly.
        assert_eq!(register(0), 0);
        for value in [
            0,
            0x2F8,
            0x4F52_4947_494F_02F8,
            0x4F52_4947_494E_0000,
            0x4F52_4947_494E,
            u64::MAX,
        ] {
            assert_eq!(port(value), None, "{value:#x}");
        }
    }
}
