//! CPU cache hierarchy from CPUID leaf 04h (CLAUDE.md §24 shared caches, §27 cache-aware
//! scheduling). Decodes the deterministic-cache-parameters sub-leaves into each cache's
//! level, type, size and how many logical processors share it. Pure, no `unsafe`, tested
//! on the host; the kernel runs CPUID and passes the sub-leaves in.
//!
//! Reference: Intel SDM Vol. 2A, CPUID leaf 04h. Per sub-leaf: EAX bits 4:0 are the cache
//! type (0 ends the list, 1 data, 2 instruction, 3 unified), bits 7:5 the level, bits
//! 25:14 (plus one) the logical processors sharing it; EBX bits 11:0 the line size minus
//! one, 21:12 the partitions minus one, 31:22 the ways minus one; ECX plus one the number
//! of sets. Size = ways × partitions × line size × sets.

/// What a cache holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheType {
    Data,
    Instruction,
    Unified,
}

/// One cache level the CPU reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cache {
    pub level: u8,
    pub kind: CacheType,
    /// Total size in bytes.
    pub size: u64,
    pub line_size: u32,
    pub ways: u32,
    /// Logical processors sharing this cache.
    pub shared_by: u32,
}

/// Decodes one leaf-04h sub-leaf `[eax, ebx, ecx, edx]`, or `None` if its type field is 0
/// (no cache: the end of the list), an undefined type, or a geometry whose byte size does
/// not fit in 64 bits (a hypervisor can report anything here; no real cache needs 2^64
/// bytes, and the end of the list is the safe reading of one that claims to).
pub fn decode(sub: [u32; 4]) -> Option<Cache> {
    let kind = match sub[0] & 0x1F {
        1 => CacheType::Data,
        2 => CacheType::Instruction,
        3 => CacheType::Unified,
        _ => return None,
    };
    let level = ((sub[0] >> 5) & 0x7) as u8;
    let shared_by = ((sub[0] >> 14) & 0xFFF) + 1;
    let line_size = (sub[1] & 0xFFF) + 1;
    let partitions = ((sub[1] >> 12) & 0x3FF) + 1;
    let ways = ((sub[1] >> 22) & 0x3FF) + 1;
    let sets = u64::from(sub[2]) + 1;
    let size = u64::from(ways)
        .checked_mul(u64::from(partitions))?
        .checked_mul(u64::from(line_size))?
        .checked_mul(sets)?;
    Some(Cache {
        level,
        kind,
        size,
        line_size,
        ways,
        shared_by,
    })
}

/// Decodes leaf-04h sub-leaves in order into `out`, stopping at the first type-0 sub-leaf
/// or when `out` is full; returns how many caches were decoded.
pub fn decode_all(subleaves: &[[u32; 4]], out: &mut [Cache]) -> usize {
    let mut n = 0;
    for &sub in subleaves {
        let Some(cache) = decode(sub) else { break };
        let Some(slot) = out.get_mut(n) else { break };
        *slot = cache;
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A leaf-04h sub-leaf from the fields (the inverse of `decode`).
    fn sub(
        kind: u32,
        level: u32,
        shared: u32,
        line: u32,
        parts: u32,
        ways: u32,
        sets: u32,
    ) -> [u32; 4] {
        let eax = kind | (level << 5) | ((shared - 1) << 14);
        let ebx = (line - 1) | ((parts - 1) << 12) | ((ways - 1) << 22);
        [eax, ebx, sets - 1, 0]
    }

    #[test]
    fn decodes_a_typical_hierarchy() {
        // L1d 32 KiB (8-way, 64-byte lines, 64 sets), per-core (shared by 2 threads).
        let l1d = decode(sub(1, 1, 2, 64, 1, 8, 64)).unwrap();
        assert_eq!(l1d.kind, CacheType::Data);
        assert_eq!(
            (l1d.level, l1d.size, l1d.line_size, l1d.ways, l1d.shared_by),
            (1, 32 * 1024, 64, 8, 2)
        );
        // L2 256 KiB unified.
        let l2 = decode(sub(3, 2, 2, 64, 1, 8, 512)).unwrap();
        assert_eq!(
            (l2.level, l2.kind, l2.size),
            (2, CacheType::Unified, 256 * 1024)
        );
        // L3 8 MiB shared by 16 logical processors.
        let l3 = decode(sub(3, 3, 16, 64, 1, 16, 8192)).unwrap();
        assert_eq!((l3.level, l3.size, l3.shared_by), (3, 8 * 1024 * 1024, 16));
        // An instruction cache, and the end of the list.
        assert_eq!(
            decode(sub(2, 1, 2, 64, 1, 8, 64)).unwrap().kind,
            CacheType::Instruction
        );
        assert_eq!(decode([0, 0, 0, 0]), None, "type 0 ends the list");
    }

    /// Random and boundary CPUID words: a sub-leaf with the largest set count, or a geometry
    /// too large for 64 bits, is refused rather than wrapped (CLAUDE.md §72). The first
    /// version added one to a `u32` (`ECX = FFFFFFFFh`) and multiplied in `u64` without a
    /// check.
    #[test]
    fn any_cpuid_words_decode_without_overflow() {
        use crate::testing::Rng;
        let mut rng = Rng::new(0x4341_4348);
        let mut decoded = 0u32;
        for case in 0..crate::testing::cases(30_000) {
            let words = [
                rng.edge_u32(),
                rng.edge_u32(),
                rng.edge_u32(),
                rng.edge_u32(),
            ];
            let expected_size = u128::from(((words[1] >> 22) & 0x3FF) + 1)
                * u128::from(((words[1] >> 12) & 0x3FF) + 1)
                * u128::from((words[1] & 0xFFF) + 1)
                * (u128::from(words[2]) + 1);
            match decode(words) {
                Some(c) => {
                    decoded += 1;
                    assert!(
                        (1..=3).contains(&(words[0] & 0x1F)),
                        "case {case}: {words:x?}"
                    );
                    assert_eq!(u128::from(c.size), expected_size, "case {case}: {words:x?}");
                    assert!(c.level < 8 && c.line_size >= 1, "case {case}");
                    assert!((1..=4096).contains(&c.line_size), "case {case}");
                    assert!((1..=1024).contains(&c.ways), "case {case}");
                    assert!((1..=4096).contains(&c.shared_by), "case {case}");
                }
                None => assert!(
                    !(1..=3).contains(&(words[0] & 0x1F)) || expected_size > u128::from(u64::MAX),
                    "case {case}: {words:x?}"
                ),
            }
            let subs = [words, [rng.edge_u32(), 0, 0, 0], [3, 0, 0, 0]];
            let mut out = [Cache {
                level: 0,
                kind: CacheType::Data,
                size: 0,
                line_size: 0,
                ways: 0,
                shared_by: 0,
            }; 2];
            assert!(decode_all(&subs, &mut out) <= 2, "case {case}");
        }
        assert!(decoded > 5000, "{decoded}");
        // The largest geometry the fields can express is exactly 2^64 bytes.
        let huge = [1, 0xFFFF_FFFF, 0xFFFF_FFFF, 0];
        assert_eq!(decode(huge), None);
    }

    #[test]
    fn decode_all_stops_at_the_terminator() {
        let subs = [
            sub(1, 1, 1, 64, 1, 8, 64),
            sub(2, 1, 1, 64, 1, 8, 64),
            sub(3, 2, 2, 64, 1, 4, 1024),
            [0, 0, 0, 0],                  // terminator
            sub(3, 3, 8, 64, 1, 16, 8192), // never reached
        ];
        let mut out = [Cache {
            level: 0,
            kind: CacheType::Data,
            size: 0,
            line_size: 0,
            ways: 0,
            shared_by: 0,
        }; 8];
        let n = decode_all(&subs, &mut out);
        assert_eq!(n, 3);
        assert_eq!(out[0].size, 32 * 1024);
        assert_eq!(out[2].level, 2);
        // The size is always the product of the geometry.
        for c in &out[..n] {
            assert_eq!(
                c.size,
                u64::from(c.ways)
                    * u64::from(c.line_size)
                    * (c.size / (u64::from(c.ways) * u64::from(c.line_size)))
            );
        }
    }
}
