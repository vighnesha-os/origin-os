//! The fill the TEST ONLY firmware step (`tests/emulator/reserved-memory`, TASKS item 40) puts in
//! each reserved page it plants: the reserved-memory boot proof writes into a firmware-reserved
//! page only if the page holds this fill. A real machine's firmware never fills a page so, so on
//! one the proof never writes memory the firmware keeps for its own use (TASKS item 40's
//! review, M2). The fixture keeps its own copy, which `xtask`'s tests hold to this one.

/// The bytes of a 4 KiB page.
pub const PAGE: usize = 4096;

/// The first 16 bytes of every filled page.
pub const MAGIC: [u8; 16] = *b"ORIGIN TEST FILL";

/// The byte the fill puts at `offset` (below [`PAGE`]) of the page at physical address `page`
/// (4 KiB aligned): [`MAGIC`], then the page's own address, then bytes that are never zero and
/// differ from page to page, so a copy that lands on another page is seen.
pub fn fill_byte(page: u64, offset: usize) -> u8 {
    match offset {
        0..16 => MAGIC[offset],
        16..24 => page.to_le_bytes()[offset - 16],
        _ => {
            let v = (page >> 12)
                .wrapping_mul(0x9E37)
                .wrapping_add(offset as u64)
                % 255;
            v as u8 + 1
        }
    }
}

/// Whether `bytes`, read from the page at physical address `page`, is exactly the fill.
pub fn holds_fill(page: u64, bytes: &[u8]) -> bool {
    bytes.len() == PAGE
        && bytes
            .iter()
            .enumerate()
            .all(|(i, &b)| b == fill_byte(page, i))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(page: u64) -> [u8; PAGE] {
        core::array::from_fn(|i| fill_byte(page, i))
    }

    #[test]
    fn a_filled_page_is_recognised_and_only_at_its_own_address() {
        let page = 0x1fb3_1000;
        assert!(holds_fill(page, &filled(page)));
        // Another page's fill, a short read, and one changed byte anywhere are not it.
        assert!(!holds_fill(page, &filled(page + 0x1000)));
        assert!(!holds_fill(page, &filled(page)[..PAGE - 1]));
        for at in [0, 15, 16, 23, 24, 2048, PAGE - 1] {
            let mut b = filled(page);
            b[at] ^= 1;
            assert!(!holds_fill(page, &b), "byte {at}");
        }
        // Zeros, and a page of one repeated byte, are never the fill.
        assert!(!holds_fill(page, &[0; PAGE]));
        assert!(!holds_fill(page, &[0xFF; PAGE]));
    }

    #[test]
    fn the_fill_after_its_header_is_never_zero_and_differs_between_pages() {
        for page in [0, 0x1000, 0x7A00_0000, 0xFF_FFFF_F000] {
            for offset in 24..PAGE {
                assert_ne!(fill_byte(page, offset), 0);
            }
            assert_ne!(filled(page)[24..], filled(page + 0x1000)[24..]);
        }
        assert_eq!(&filled(0x1234_5000)[..16], &MAGIC);
        assert_eq!(filled(0x1234_5000)[16..24], 0x1234_5000u64.to_le_bytes());
    }
}
