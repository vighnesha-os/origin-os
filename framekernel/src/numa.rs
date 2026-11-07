//! A NUMA locality map built from the ACPI SRAT and SLIT (CLAUDE.md §27 migration cost,
//! §28 NUMA policies, §66 NUMA locality). Pure, no `unsafe`, tested on the host; it turns
//! the parsed affinities and the distance matrix into the questions a frame allocator or a
//! scheduler asks — which node owns a processor or a physical address, and the nodes
//! nearest a given node in ascending distance. The kernel feeds it the already-parsed ACPI
//! data; this module decides nothing about the hardware, only about placement.

use crate::acpi::{CpuAffinity, MemoryAffinity, Slit};

/// The proximity domain (NUMA node) a processor belongs to: the first enabled CPU affinity
/// whose APIC ID matches, or `None` if the processor is not listed.
pub fn node_of_cpu(cpus: &[CpuAffinity], apic_id: u32) -> Option<u32> {
    cpus.iter()
        .find(|c| c.enabled && c.apic_id == apic_id)
        .map(|c| c.proximity_domain)
}

/// The proximity domain that owns physical address `addr`: the first enabled memory
/// affinity whose `[base, base + length)` range contains it, or `None` if no range does.
pub fn node_of_address(mems: &[MemoryAffinity], addr: u64) -> Option<u32> {
    mems.iter()
        .find(|m| m.enabled && addr >= m.base && (addr - m.base) < m.length)
        .map(|m| m.proximity_domain)
}

/// Fills `out` with the SLIT locality indices `0..n` ordered by ascending distance from
/// locality `from`, and returns how many were written (`min(localities, out.len())`). The
/// sort is stable, so equal-distance nodes keep ascending-index order. A well-formed SLIT
/// puts `from` first (its self-distance 10 is the minimum).
pub fn nearest_nodes(slit: &Slit, from: usize, out: &mut [usize]) -> usize {
    let n = slit.localities().min(out.len());
    for (i, slot) in out.iter_mut().enumerate().take(n) {
        *slot = i;
    }
    let dist = |to: usize| slit.distance(from, to).unwrap_or(u8::MAX);
    // Stable insertion sort by distance(from, ·).
    for i in 1..n {
        let mut j = i;
        while j > 0 && dist(out[j]) < dist(out[j - 1]) {
            out.swap(j, j - 1);
            j -= 1;
        }
    }
    n
}

/// Whether every locality's nearest node (by the SLIT) is itself — the basic sanity an
/// allocator relies on, that local memory is never further than remote memory. Returns
/// `false` if the SLIT has no localities.
pub fn self_is_nearest(slit: &Slit) -> bool {
    let n = slit.localities();
    if n == 0 {
        return false;
    }
    let mut order = [0usize; 256];
    (0..n).all(|from| {
        let k = nearest_nodes(slit, from, &mut order).min(1);
        k == 1 && order[0] == from
    })
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use crate::acpi::parse_slit;
    use alloc::vec::Vec;

    fn cpu(apic_id: u32, node: u32, enabled: bool) -> CpuAffinity {
        CpuAffinity {
            apic_id,
            proximity_domain: node,
            enabled,
        }
    }

    fn mem(node: u32, base: u64, length: u64) -> MemoryAffinity {
        MemoryAffinity {
            proximity_domain: node,
            base,
            length,
            enabled: true,
            hot_pluggable: false,
            non_volatile: false,
        }
    }

    /// A SLIT table body (no checksum needed — `parse_slit` assumes a checked table): a
    /// 44-byte header area, the u64 locality count at offset 36, then the matrix.
    fn slit_bytes(n: u64, matrix: &[u8]) -> Vec<u8> {
        let mut b = alloc::vec![0u8; 44];
        b[36..44].copy_from_slice(&n.to_le_bytes());
        b.extend_from_slice(matrix);
        b
    }

    #[test]
    fn cpu_and_address_lookup() {
        let cpus = [cpu(0, 0, true), cpu(1, 1, true), cpu(2, 1, false)];
        assert_eq!(node_of_cpu(&cpus, 0), Some(0));
        assert_eq!(node_of_cpu(&cpus, 1), Some(1));
        assert_eq!(
            node_of_cpu(&cpus, 2),
            None,
            "a disabled affinity is skipped"
        );
        assert_eq!(node_of_cpu(&cpus, 9), None, "an unlisted processor");

        let mems = [
            mem(0, 0x0000_0000, 0x1000_0000),   // node 0: [0, 256M)
            mem(1, 0x1_0000_0000, 0x1000_0000), // node 1: [4G, 4G+256M)
        ];
        assert_eq!(node_of_address(&mems, 0), Some(0));
        assert_eq!(node_of_address(&mems, 0x0FFF_FFFF), Some(0));
        assert_eq!(
            node_of_address(&mems, 0x1000_0000),
            None,
            "just past node 0"
        );
        assert_eq!(node_of_address(&mems, 0x1_0000_0001), Some(1));
        assert_eq!(node_of_address(&mems, 0x2_0000_0000), None, "unmapped");
    }

    #[test]
    fn nearest_nodes_orders_by_distance() {
        // Three localities; node 0 is closer to 1 (16) than to 2 (32).
        let m = [10, 16, 32, 16, 10, 20, 32, 20, 10];
        let bytes = slit_bytes(3, &m);
        let slit = parse_slit(&bytes).unwrap();

        let mut out = [0usize; 8];
        assert_eq!(nearest_nodes(&slit, 0, &mut out), 3);
        assert_eq!(&out[..3], &[0, 1, 2], "0, then 1 (16), then 2 (32)");
        assert_eq!(nearest_nodes(&slit, 2, &mut out), 3);
        assert_eq!(&out[..3], &[2, 1, 0], "2, then 1 (20), then 0 (32)");

        // Every locality's nearest is itself.
        assert!(self_is_nearest(&slit));

        // A short output buffer caps the count without panicking.
        let mut small = [0usize; 2];
        assert_eq!(nearest_nodes(&slit, 1, &mut small), 2);
    }

    #[test]
    fn equal_distances_keep_ascending_order() {
        // Node 0 is equidistant (20) from 1 and 2: a stable sort keeps 1 before 2.
        let m = [10, 20, 20, 20, 10, 99, 20, 99, 10];
        let bytes = slit_bytes(3, &m);
        let slit = parse_slit(&bytes).unwrap();
        let mut out = [0usize; 4];
        assert_eq!(nearest_nodes(&slit, 0, &mut out), 3);
        assert_eq!(&out[..3], &[0, 1, 2]);
    }

    #[test]
    fn a_broken_slit_is_not_self_nearest() {
        // Node 0 claims a remote node (1) is closer to it than itself: self-nearest fails.
        let m = [20, 10, 10, 10, 10, 10, 10, 10, 10];
        let bytes = slit_bytes(3, &m);
        let slit = parse_slit(&bytes).unwrap();
        assert!(!self_is_nearest(&slit));
    }
}
