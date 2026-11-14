//! Dense CPU indexes for the processors the ACPI MADT lists (CLAUDE.md §24 per-CPU state
//! and CPU topology; ADR-0008 and its Amendment 1; research ledgers
//! `docs/research/2026-11-08-x2apic-above-255.md` and
//! `docs/research/2026-11-13-any-processor-count.md`).
//!
//! The kernel keeps its per-CPU state (CPU-local areas, run queues, shootdown records,
//! per-CPU device queues) in tables indexed by a dense index (`origin_frame::percpu`). Indexing
//! them by APIC ID would tie their size to the largest ID, and x2APIC IDs are 32-bit and sparse:
//! firmware builds an ID from topology fields (package, die, module, core, thread), each rounded
//! up to a power of two, so IDs above 255 appear on machines with only a few hundred processors,
//! and gaps are normal. QEMU 11.1, for one, gives the second package of a
//! `sockets=2,dies=3,modules=5,cores=5` machine the IDs 256–420. So each processor gets a dense
//! index at discovery instead:
//! - index 0 is the boot processor, wherever (and whether) the MADT lists it;
//! - every other processor the MADT marks enabled gets the next index, in table order;
//! - then every processor the MADT marks Online Capable gets the next index, in table order,
//!   after every enabled one: its index is reserved for when it is brought online (Amendment 1,
//!   D2). An index, once given, never changes;
//! - a processor listed twice (as a type 0 and a type 9 structure) is indexed once;
//! - a processor that is neither enabled nor online-capable gets no index (ACPI: ignore it);
//! - one with the broadcast ID gets none either, and is counted, so the kernel can say it
//!   refused it.
//!
//! No count is compiled in (Amendment 1, D3 and D5): every processor the rules index gets an
//! index here. Whether the kernel then has the memory and the address space for each is decided
//! when the tables are sized (`smp`), and reported there.
//!
//! The APIC ID stays the processor's address: IPIs and interrupt destinations use
//! [`CpuMap::apic_id`]. Safe code, host-tested.
#![forbid(unsafe_code)]

use alloc::vec::Vec;

use crate::acpi::Processor;

pub use crate::limits::BROADCAST;

/// The processors the kernel indexes, and what the MADT listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpuMap {
    /// Each index's APIC ID: the boot processor, the enabled processors, then the
    /// online-capable ones.
    apic_ids: Vec<u32>,
    /// Indexes given to enabled processors, the boot processor included (at least 1): indexes
    /// `0..started()` are the processors to start, the rest are reserved.
    started: usize,
    /// Processor structures given (each processor once, as `madt_processors` lists them).
    listed: usize,
    /// Of those, enabled: usable now (MADT flags bit 0).
    enabled: usize,
    /// Of those, online-capable: may be enabled later (flags bit 1, MADT revision 5 on).
    online_capable: usize,
    /// Enabled processors refused because they carry the broadcast ID.
    invalid: usize,
    /// Whether the MADT lists the boot processor as enabled.
    boot_listed: bool,
    /// The largest APIC ID listed, enabled or not.
    highest: Option<u32>,
}

impl CpuMap {
    /// Indexes the boot processor, whose x2APIC ID is `boot`, as 0, then each enabled
    /// processor in `processors` (a MADT's, as [`crate::acpi::madt_processors`] lists them)
    /// in order, then each online-capable one in order.
    pub fn new(boot: u32, processors: &[Processor]) -> Self {
        let mut map = Self {
            apic_ids: Vec::with_capacity(1),
            started: 1,
            listed: 0,
            enabled: 0,
            online_capable: 0,
            invalid: 0,
            boot_listed: false,
            highest: None,
        };
        map.apic_ids.push(boot);
        // The IDs indexed so far, sorted: a processor listed twice is found in log time.
        let mut seen = alloc::vec![boot];
        for p in processors {
            map.listed += 1;
            map.highest = Some(map.highest.map_or(p.apic_id, |h| h.max(p.apic_id)));
            if p.enabled {
                if p.apic_id == boot {
                    // The boot processor already has index 0; count it once.
                    if !map.boot_listed {
                        map.boot_listed = true;
                        map.enabled += 1;
                    }
                    continue;
                }
                if seen.binary_search(&p.apic_id).is_ok() {
                    continue; // the same processor again
                }
                map.enabled += 1;
                if !map.index(p.apic_id, &mut seen) {
                    map.invalid += 1;
                }
            }
        }
        map.started = map.apic_ids.len();
        for p in processors.iter().filter(|p| !p.enabled && p.online_capable) {
            if p.apic_id == boot || seen.binary_search(&p.apic_id).is_ok() {
                continue;
            }
            if map.index(p.apic_id, &mut seen) {
                map.online_capable += 1;
            }
        }
        map
    }

    /// Gives `apic_id` the next index, unless it is the broadcast ID; returns whether it did.
    fn index(&mut self, apic_id: u32, seen: &mut Vec<u32>) -> bool {
        if apic_id == BROADCAST {
            return false;
        }
        if let Err(at) = seen.binary_search(&apic_id) {
            seen.insert(at, apic_id);
        }
        self.apic_ids.push(apic_id);
        true
    }

    /// Processors indexed, the boot processor and the reserved ones included: indexes
    /// `0..indexed()` are in use, and every per-CPU table is sized for them.
    pub fn indexed(&self) -> usize {
        self.apic_ids.len()
    }

    /// Indexes given to the boot processor and the enabled processors: indexes `0..started()`
    /// are the processors to start.
    pub const fn started(&self) -> usize {
        self.started
    }

    /// Indexes reserved for online-capable processors: `started()..indexed()`.
    pub fn reserved(&self) -> usize {
        self.indexed() - self.started
    }

    /// The APIC ID of processor `index`.
    pub fn apic_id(&self, index: usize) -> Option<u32> {
        self.apic_ids.get(index).copied()
    }

    /// The index of the processor whose APIC ID is `apic_id`.
    pub fn index_of(&self, apic_id: u32) -> Option<usize> {
        self.apic_ids.iter().position(|&id| id == apic_id)
    }

    /// Every `(index, APIC ID)`, the boot processor first, the reserved ones last.
    pub fn iter(&self) -> impl Iterator<Item = (usize, u32)> + '_ {
        self.apic_ids.iter().copied().enumerate()
    }

    /// Processor structures the MADT listed, enabled or not.
    pub const fn listed(&self) -> usize {
        self.listed
    }

    /// Of those, the processors enabled, the boot processor included when listed.
    pub const fn enabled(&self) -> usize {
        self.enabled
    }

    /// Of those, the online-capable processors given an index.
    pub const fn online_capable(&self) -> usize {
        self.online_capable
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
    /// all if it has the memory for them.
    pub const fn covers_enabled(&self) -> bool {
        self.invalid == 0
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

    /// ADR-0008 Amendment 1, D3 and D7: no count is compiled in. At 1, 3, 64, 65, 300, 1,000
    /// and more than 4,096 processors, with sparse IDs above 255, every enabled one is indexed
    /// in table order.
    #[test]
    fn every_enabled_processor_is_indexed_whatever_the_count() {
        for count in [1u32, 3, 64, 65, 300, 1000, 4097, 9000] {
            // Two packages, the second from APIC ID 0x10000: IDs above 255, with gaps.
            let ids: Vec<u32> = (0..count)
                .map(|i| if i % 2 == 0 { i * 3 } else { 0x1_0000 + i * 5 })
                .collect();
            let processors: Vec<_> = ids.iter().map(|&id| cpu(id, true)).collect();
            let map = CpuMap::new(0, &processors);
            assert_eq!(map.indexed(), count as usize, "{count}");
            assert_eq!(map.started(), count as usize, "{count}");
            assert_eq!(map.reserved(), 0);
            assert!(map.covers_enabled());
            let got: Vec<u32> = map.iter().map(|(_, id)| id).collect();
            assert_eq!(
                got, ids,
                "{count}: table order, the boot processor (APIC ID 0) first"
            );
            let last = *ids.last().unwrap();
            assert_eq!(map.index_of(last), Some(count as usize - 1));
            assert_eq!(map.apic_id(count as usize), None);
        }
    }

    /// ADR-0008 Amendment 1, D2: online-capable processors get the indexes after every enabled
    /// one, in table order; processors neither enabled nor online-capable get none.
    #[test]
    fn online_capable_processors_are_indexed_after_every_enabled_one() {
        let capable = |id| Processor {
            apic_id: id,
            uid: id,
            enabled: false,
            online_capable: true,
        };
        let map = CpuMap::new(
            0,
            &[
                cpu(0, true),
                capable(8),
                cpu(1, true),
                cpu(9, false),
                capable(300),
                cpu(256, true),
                capable(1), // already enabled: indexed once
                capable(0), // the boot processor
                capable(BROADCAST),
            ],
        );
        let pairs: Vec<_> = map.iter().collect();
        assert_eq!(pairs, [(0, 0), (1, 1), (2, 256), (3, 8), (4, 300)]);
        assert_eq!((map.started(), map.reserved(), map.indexed()), (3, 2, 5));
        assert_eq!((map.enabled(), map.online_capable()), (3, 2));
        assert_eq!(map.index_of(9), None, "neither enabled nor online-capable");
        assert_eq!(
            map.invalid(),
            0,
            "an online-capable broadcast ID is no enabled processor"
        );
        assert!(map.covers_enabled());
    }

    /// ADR-0008 Amendment 1, D7: the discovery machine lists every processor q35 allows, 4,096,
    /// with two enabled.
    #[test]
    fn a_madt_of_4096_possible_processors_with_2_enabled() {
        let processors: Vec<_> = (0..4096u32)
            .map(|id| cpu(id, id == 0 || id == 4095))
            .collect();
        let map = CpuMap::new(0, &processors);
        assert_eq!((map.listed(), map.enabled(), map.indexed()), (4096, 2, 2));
        assert_eq!(map.apic_id(1), Some(4095));
        assert_eq!(map.highest(), Some(4095));
        assert!(map.covers_enabled());
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
            let n = rng.below(300);
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
            assert!(map.indexed() >= 1, "case {case}");
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
            assert_eq!(ids.get(1..), Some(&wanted[..]), "case {case}");
            assert_eq!(map.started(), wanted.len() + 1, "case {case}");
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
