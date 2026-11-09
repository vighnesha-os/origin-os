//! Dense CPU indexes for the processors the ACPI MADT lists (CLAUDE.md §24 per-CPU state
//! and CPU topology; ADR-0008, Proposed; research ledger
//! `docs/research/2026-11-08-x2apic-above-255.md`).
//!
//! The kernel keeps its per-CPU state (CPU-local areas, run queues, shootdown records,
//! per-CPU device queues) in fixed arrays. Indexing them by APIC ID ties their size to the
//! largest ID, and x2APIC IDs are 32-bit and sparse: firmware builds an ID from topology
//! fields (package, die, module, core, thread), each rounded up to a power of two, so IDs
//! above 255 appear on machines with only a few hundred processors, and gaps are normal.
//! QEMU 11.1, for one, gives the second package of a `sockets=2,dies=3,modules=5,cores=5`
//! machine the IDs 256–420. So each processor gets a dense index at discovery instead:
//! - index 0 is the boot processor, wherever (and whether) the MADT lists it;
//! - every other processor the MADT marks enabled gets the next index, in table order;
//! - an enabled processor listed twice (as a type 0 and a type 9 structure) is indexed once;
//! - a processor that is not enabled gets no index (hot-plug is a later step);
//! - an enabled processor beyond [`MAX_CPUS`], or one with the broadcast ID, gets none
//!   either, and is counted, so the kernel can say it left that processor in firmware.
//!
//! The APIC ID stays the processor's address: IPIs and interrupt destinations use
//! [`CpuMap::apic_id`]. Safe code, host-tested.

use crate::acpi::Processor;

/// Processors the kernel indexes: every per-CPU array has this many slots.
pub const MAX_CPUS: usize = 64;

/// The x2APIC broadcast destination in physical mode (all ones): an IPI to it reaches every
/// processor, so it is never one processor's ID.
pub const BROADCAST: u32 = u32::MAX;

/// The processors the kernel indexes, and what the MADT listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpuMap {
    /// Each index's APIC ID; slots at and past `indexed` are unused.
    apic_ids: [u32; MAX_CPUS],
    /// Processors indexed, the boot processor included (at least 1).
    indexed: usize,
    /// Processor structures given (each processor once, as `madt_processors` lists them).
    listed: usize,
    /// Of those, enabled: usable now (MADT flags bit 0).
    enabled: usize,
    /// Enabled processors left without an index: beyond [`MAX_CPUS`].
    unindexed: usize,
    /// Enabled processors refused: they carry the broadcast ID.
    invalid: usize,
    /// Whether the MADT lists the boot processor as enabled.
    boot_listed: bool,
    /// The largest APIC ID listed, enabled or not.
    highest: Option<u32>,
}

impl CpuMap {
    /// Indexes the boot processor, whose x2APIC ID is `boot`, as 0, then each enabled
    /// processor in `processors` (a MADT's, as [`crate::acpi::madt_processors`] lists them)
    /// in order.
    pub fn new(boot: u32, processors: &[Processor]) -> Self {
        let mut map = Self {
            apic_ids: [BROADCAST; MAX_CPUS],
            indexed: 1,
            listed: 0,
            enabled: 0,
            unindexed: 0,
            invalid: 0,
            boot_listed: false,
            highest: None,
        };
        map.apic_ids[0] = boot;
        for p in processors {
            map.listed += 1;
            map.highest = Some(map.highest.map_or(p.apic_id, |h| h.max(p.apic_id)));
            if !p.enabled {
                continue;
            }
            if p.apic_id == boot {
                // The boot processor already has index 0; count it once.
                if !map.boot_listed {
                    map.boot_listed = true;
                    map.enabled += 1;
                }
                continue;
            }
            if map.index_of(p.apic_id).is_some() {
                continue; // the same processor again
            }
            map.enabled += 1;
            if p.apic_id == BROADCAST {
                map.invalid += 1;
                continue;
            }
            match map.apic_ids.get_mut(map.indexed) {
                Some(slot) => {
                    *slot = p.apic_id;
                    map.indexed += 1;
                }
                None => map.unindexed += 1,
            }
        }
        map
    }

    /// Processors indexed, the boot processor included: indexes `0..indexed()` are in use.
    pub const fn indexed(&self) -> usize {
        self.indexed
    }

    /// The APIC ID of processor `index`.
    pub fn apic_id(&self, index: usize) -> Option<u32> {
        self.apic_ids.get(..self.indexed)?.get(index).copied()
    }

    /// The index of the processor whose APIC ID is `apic_id`.
    pub fn index_of(&self, apic_id: u32) -> Option<usize> {
        self.apic_ids
            .get(..self.indexed)?
            .iter()
            .position(|&id| id == apic_id)
    }

    /// Every `(index, APIC ID)`, the boot processor first.
    pub fn iter(&self) -> impl Iterator<Item = (usize, u32)> + '_ {
        self.apic_ids.iter().copied().enumerate().take(self.indexed)
    }

    /// Processor structures the MADT listed, enabled or not.
    pub const fn listed(&self) -> usize {
        self.listed
    }

    /// Of those, the processors enabled, the boot processor included when listed.
    pub const fn enabled(&self) -> usize {
        self.enabled
    }

    /// Enabled processors left without an index because every slot was taken.
    pub const fn unindexed(&self) -> usize {
        self.unindexed
    }

    /// Enabled processors refused because they carry the broadcast ID.
    pub const fn invalid(&self) -> usize {
        self.invalid
    }

    /// Whether the MADT lists the boot processor as enabled.
    pub const fn boot_listed(&self) -> bool {
        self.boot_listed
    }

    /// The largest APIC ID the MADT listed, enabled or not.
    pub const fn highest(&self) -> Option<u32> {
        self.highest
    }

    /// Whether every processor the MADT enables has an index, so the kernel can start them
    /// all (none is left in firmware by its limits).
    pub const fn covers_enabled(&self) -> bool {
        self.unindexed == 0 && self.invalid == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Rng;

    fn cpu(apic_id: u32, enabled: bool) -> Processor {
        Processor {
            apic_id,
            uid: apic_id,
            enabled,
            online_capable: false,
        }
    }

    /// The processors QEMU 11.1 lists for `-smp 1,maxcpus=150,sockets=2,dies=3,modules=5,
    /// cores=5,threads=1` with one CPU cold-plugged at socket 1 (die, module, core and thread
    /// 0): every possible processor, in index order, the two present ones enabled. Core IDs
    /// take bits 0–2, module IDs bits 3–5, die IDs bits 6–7 and the package bit 8.
    fn qemu_sparse() -> Vec<Processor> {
        let mut out = Vec::new();
        for socket in 0..2u32 {
            for die in 0..3u32 {
                for module in 0..5u32 {
                    for core in 0..5u32 {
                        let id = socket << 8 | die << 6 | module << 3 | core;
                        out.push(cpu(id, id == 0 || id == 256));
                    }
                }
            }
        }
        out
    }

    #[test]
    fn the_boot_processor_is_index_0_then_enabled_processors_in_table_order() {
        let map = CpuMap::new(
            0,
            &[cpu(0, true), cpu(1, true), cpu(2, false), cpu(256, true)],
        );
        let pairs: Vec<_> = map.iter().collect();
        assert_eq!(pairs, [(0, 0), (1, 1), (2, 256)]);
        assert_eq!(map.indexed(), 3);
        assert_eq!((map.listed(), map.enabled()), (4, 3));
        assert!(map.boot_listed() && map.covers_enabled());
        assert_eq!(map.highest(), Some(256));
        // A disabled processor gets no index.
        assert_eq!(map.index_of(2), None);
        assert_eq!(map.apic_id(3), None);
    }

    #[test]
    fn the_boot_processor_is_index_0_wherever_the_madt_lists_it() {
        let map = CpuMap::new(5, &[cpu(1, true), cpu(3, true), cpu(5, true), cpu(7, true)]);
        let ids: Vec<_> = map.iter().map(|(_, id)| id).collect();
        assert_eq!(ids, [5, 1, 3, 7]);
        assert!(map.boot_listed());
        assert_eq!(map.enabled(), 4);
    }

    #[test]
    fn a_boot_processor_the_madt_omits_still_has_index_0() {
        let map = CpuMap::new(9, &[cpu(1, true), cpu(2, true)]);
        assert_eq!(map.apic_id(0), Some(9));
        assert_eq!(map.indexed(), 3);
        assert!(!map.boot_listed());
        assert_eq!(map.enabled(), 2);
    }

    #[test]
    fn sparse_ids_above_255_get_dense_indexes() {
        let processors = qemu_sparse();
        let map = CpuMap::new(0, &processors);
        assert_eq!(map.listed(), 150);
        assert_eq!(map.enabled(), 2);
        assert_eq!(map.indexed(), 2);
        assert_eq!(map.apic_id(1), Some(256));
        assert_eq!(map.index_of(256), Some(1));
        // Socket 1's last processor: die 2, module 4, core 4.
        assert_eq!(map.highest(), Some(420));
        assert!(map.covers_enabled() && map.boot_listed());
        // 75 of them need type 9 (x2APIC) structures: IDs of 255 and above.
        assert_eq!(processors.iter().filter(|p| p.apic_id >= 255).count(), 75);
    }

    #[test]
    fn processors_beyond_max_cpus_are_counted_not_indexed() {
        let processors: Vec<_> = (1..=70).map(|id| cpu(id * 3, true)).collect();
        let map = CpuMap::new(0, &processors);
        assert_eq!(map.indexed(), MAX_CPUS);
        assert_eq!(map.unindexed(), 70 - (MAX_CPUS - 1));
        assert!(!map.covers_enabled());
        // The first 63 in table order got indexes 1-63.
        assert_eq!(map.apic_id(1), Some(3));
        assert_eq!(map.apic_id(MAX_CPUS - 1), Some(3 * (MAX_CPUS as u32 - 1)));
        assert_eq!(map.index_of(3 * MAX_CPUS as u32), None);
    }

    #[test]
    fn a_processor_listed_twice_is_indexed_once() {
        let map = CpuMap::new(
            0,
            &[cpu(0, true), cpu(300, true), cpu(300, true), cpu(0, true)],
        );
        assert_eq!(map.indexed(), 2);
        assert_eq!(map.enabled(), 2);
    }

    #[test]
    fn the_broadcast_id_is_refused() {
        let map = CpuMap::new(0, &[cpu(BROADCAST, true), cpu(4, true)]);
        assert_eq!(map.invalid(), 1);
        assert_eq!(map.index_of(BROADCAST), None);
        assert_eq!(map.apic_id(1), Some(4));
        assert!(!map.covers_enabled());
    }

    /// Random MADTs (each processor once, IDs from a sparse 32-bit range, any mix of
    /// enabled): the indexes are dense, the boot processor is 0, the order is the table's,
    /// every lookup round-trips, and every enabled processor is accounted for.
    #[test]
    fn random_madts_keep_the_map_invariants() {
        let mut rng = Rng::new(0x0C0F_FEE5);
        for case in 0..crate::testing::cases(2000) {
            let n = rng.below(120);
            let mut processors: Vec<Processor> = Vec::new();
            while processors.len() < n {
                let id = match rng.below(4) {
                    0 => rng.below(256) as u32,
                    1 => 256 + rng.below(4096) as u32,
                    2 => rng.next() as u32,
                    _ => BROADCAST - rng.below(2) as u32,
                };
                if processors.iter().all(|p| p.apic_id != id) {
                    processors.push(cpu(id, rng.below(3) != 0));
                }
            }
            let boot = match processors.get(rng.below(n + 1)) {
                Some(p) if rng.below(4) != 0 => p.apic_id,
                _ => rng.next() as u32 & 0xFFFF,
            };
            let map = CpuMap::new(boot, &processors);
            assert_eq!(map.apic_id(0), Some(boot), "case {case}");
            assert!((1..=MAX_CPUS).contains(&map.indexed()), "case {case}");
            let ids: Vec<u32> = map.iter().map(|(_, id)| id).collect();
            for (i, &id) in ids.iter().enumerate() {
                assert_eq!(map.index_of(id), Some(i), "case {case}: lookups round-trip");
                assert!(i == 0 || id != BROADCAST, "case {case}");
            }
            // Indexes 1.. are the enabled, valid, non-boot processors in table order.
            let wanted: Vec<u32> = processors
                .iter()
                .filter(|p| p.enabled && p.apic_id != boot && p.apic_id != BROADCAST)
                .map(|p| p.apic_id)
                .collect();
            let taken = wanted.len().min(MAX_CPUS - 1);
            assert_eq!(ids.get(1..), wanted.get(..taken), "case {case}");
            assert_eq!(map.unindexed(), wanted.len() - taken, "case {case}");
            let enabled = processors.iter().filter(|p| p.enabled).count();
            assert_eq!(map.enabled(), enabled, "case {case}");
            assert_eq!(map.listed(), n, "case {case}");
            let refused = processors
                .iter()
                .filter(|p| p.enabled && p.apic_id == BROADCAST && boot != BROADCAST)
                .count();
            assert_eq!(map.invalid(), refused, "case {case}");
            assert_eq!(
                map.boot_listed(),
                processors.iter().any(|p| p.enabled && p.apic_id == boot),
                "case {case}"
            );
        }
    }
}
