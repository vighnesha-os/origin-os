//! CPU topology from CPUID (CLAUDE.md §24 CPU topology and SMT awareness, §27
//! topology-aware scheduling). Decodes the extended-topology leaf (0Bh) into the bit
//! widths that split an x2APIC ID into SMT thread, core and package, and groups a set of
//! processors into cores and packages. Pure, no `unsafe`, tested on the host; the kernel
//! runs CPUID and passes the sub-leaves in.
//!
//! Reference: Intel SDM Vol. 2A, CPUID leaf 0Bh. Each sub-leaf describes one level: ECX
//! bits 15:8 are the level type (1 = SMT, 2 = Core), EAX bits 4:0 are the number of bits
//! to shift an x2APIC ID right to reach the next level up, and EDX is the current
//! processor's x2APIC ID.

/// How an x2APIC ID splits into thread/core/package, as bit widths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Topology {
    /// Low bits identifying the SMT thread within a core.
    pub smt_width: u32,
    /// Low bits identifying the logical processor within a package (threads and cores).
    pub core_width: u32,
}

impl Topology {
    /// A flat topology: every processor is its own core in its own package.
    pub const FLAT: Topology = Topology {
        smt_width: 0,
        core_width: 0,
    };

    /// Decodes leaf 0Bh sub-leaves `[eax, ebx, ecx, edx]`, in order from sub-leaf 0. A
    /// sub-leaf whose level type is 0 ends the list. Returns [`Topology::FLAT`] if the leaf
    /// is absent (no sub-leaf describes a level).
    pub fn decode(subleaves: &[[u32; 4]]) -> Topology {
        let mut smt_width = 0;
        let mut core_width = 0;
        for sub in subleaves {
            let level_type = (sub[2] >> 8) & 0xFF;
            let shift = sub[0] & 0x1F;
            match level_type {
                1 => smt_width = shift,  // SMT level: shift to the core level
                2 => core_width = shift, // Core level: shift to the package level
                _ => break,              // 0 = invalid: end of the list
            }
        }
        // The core level's shift is cumulative from bit 0, so it includes the SMT bits.
        Topology {
            smt_width,
            core_width: core_width.max(smt_width),
        }
    }

    /// Threads per core.
    pub fn threads_per_core(&self) -> u32 {
        1 << self.smt_width
    }

    /// Logical processors per package (threads times cores).
    pub fn logical_per_package(&self) -> u32 {
        1 << self.core_width
    }

    /// Splits an x2APIC ID into `(package, core, thread)`, where `core` is the core index
    /// within its package and `thread` the SMT thread within its core.
    pub fn split(&self, apic_id: u32) -> (u32, u32, u32) {
        let thread = apic_id & (self.threads_per_core() - 1);
        let core_bits = self.core_width - self.smt_width;
        let core = (apic_id >> self.smt_width) & ((1 << core_bits) - 1);
        let package = apic_id >> self.core_width;
        (package, core, thread)
    }

    /// Whether two processors are SMT siblings (same package and core).
    pub fn are_siblings(&self, a: u32, b: u32) -> bool {
        let (pa, ca, _) = self.split(a);
        let (pb, cb, _) = self.split(b);
        (pa, ca) == (pb, cb)
    }
}

/// Counts of a set of processors: distinct packages, distinct cores, and total threads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub packages: usize,
    pub cores: usize,
    pub threads: usize,
}

/// Groups `apic_ids` by the topology and checks consistency: returns the counts and
/// whether every `(package, core, thread)` triple is unique (no two processors collide).
/// `scratch` must be at least `apic_ids.len()` long; it holds the `(package, core,
/// thread)` triples so the host code needs no allocation.
pub fn analyze(
    topology: &Topology,
    apic_ids: &[u32],
    scratch: &mut [(u32, u32, u32)],
) -> (Counts, bool) {
    let n = apic_ids.len().min(scratch.len());
    for (slot, &id) in scratch.iter_mut().zip(apic_ids).take(n) {
        *slot = topology.split(id);
    }
    let triples = &scratch[..n];
    let mut unique = true;
    let mut packages = 0;
    let mut cores = 0;
    for (i, &t) in triples.iter().enumerate() {
        // Unique triples: no earlier processor shares this exact (pkg, core, thread).
        if triples[..i].contains(&t) {
            unique = false;
        }
        // Count a package/core the first time its representative appears.
        if !triples[..i].iter().any(|&(p, _, _)| p == t.0) {
            packages += 1;
        }
        if !triples[..i].iter().any(|&(p, c, _)| (p, c) == (t.0, t.1)) {
            cores += 1;
        }
    }
    (
        Counts {
            packages,
            cores,
            threads: n,
        },
        unique,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds leaf 0Bh sub-leaves for an SMT level of `smt_shift` and a core level of
    /// `core_shift`.
    fn subleaves(smt_shift: u32, core_shift: u32) -> [[u32; 4]; 3] {
        [
            [smt_shift, 0, 1 << 8, 0],  // level type 1 (SMT)
            [core_shift, 0, 2 << 8, 0], // level type 2 (Core)
            [0, 0, 0, 0],               // invalid: end
        ]
    }

    #[test]
    fn decodes_widths_and_splits_ids() {
        // 2 threads/core (smt_shift 1), 4 logical/package (core_shift 2) => 2 cores/pkg.
        let t = Topology::decode(&subleaves(1, 2));
        assert_eq!(
            t,
            Topology {
                smt_width: 1,
                core_width: 2
            }
        );
        assert_eq!(t.threads_per_core(), 2);
        assert_eq!(t.logical_per_package(), 4);
        // id = package<<2 | core<<1 | thread (1 thread bit, 1 core bit, the rest package).
        assert_eq!(t.split(0), (0, 0, 0));
        assert_eq!(t.split(1), (0, 0, 1), "the sibling thread");
        assert_eq!(t.split(2), (0, 1, 0), "core 1");
        assert_eq!(t.split(7), (1, 1, 1), "package 1: 4 | 2 | 1");
        assert!(t.are_siblings(0, 1));
        assert!(!t.are_siblings(0, 2), "different cores");
        assert!(!t.are_siblings(0, 4), "different packages");
    }

    /// Any sub-leaves decode, and the widths always split an APIC ID into fields that
    /// reassemble it (CLAUDE.md §72).
    #[test]
    fn any_subleaves_split_ids_consistently() {
        use crate::testing::Rng;
        let mut rng = Rng::new(0x544F_504F);
        for case in 0..crate::testing::cases(20_000) {
            let leaves: [[u32; 4]; 3] = core::array::from_fn(|_| {
                [
                    rng.edge_u32(),
                    rng.edge_u32(),
                    rng.edge_u32(),
                    rng.edge_u32(),
                ]
            });
            let t = Topology::decode(&leaves[..rng.below(4)]);
            assert!(
                t.smt_width <= t.core_width && t.core_width <= 31,
                "case {case}: {t:?}"
            );
            assert_eq!(t.threads_per_core(), 1 << t.smt_width, "case {case}");
            assert_eq!(t.logical_per_package(), 1 << t.core_width, "case {case}");
            let id = rng.edge_u32();
            let (package, core, thread) = t.split(id);
            let back = (u64::from(package) << t.core_width)
                | (u64::from(core) << t.smt_width)
                | u64::from(thread);
            assert_eq!(back, u64::from(id), "case {case}: {t:?}");
            assert!(u64::from(thread) < (1u64 << t.smt_width), "case {case}");
            assert!(t.are_siblings(id, id), "case {case}");

            let ids: [u32; 6] = core::array::from_fn(|_| rng.edge_u32());
            let mut scratch = [(0u32, 0u32, 0u32); 6];
            let (counts, unique) = analyze(&t, &ids, &mut scratch);
            assert_eq!(counts.threads, 6, "case {case}");
            assert!(
                counts.packages <= counts.cores && counts.cores <= 6,
                "case {case}"
            );
            let mut sorted = ids;
            sorted.sort_unstable();
            let distinct = sorted.windows(2).all(|w| w[0] != w[1]);
            assert_eq!(unique, distinct, "case {case}: {ids:x?}");
            let mut small = [(0u32, 0u32, 0u32); 2];
            assert_eq!(analyze(&t, &ids, &mut small).0.threads, 2, "case {case}");
        }
    }

    #[test]
    fn a_flat_topology_makes_each_processor_its_own_core() {
        let t = Topology::decode(&[]);
        assert_eq!(t, Topology::FLAT);
        assert_eq!(t.threads_per_core(), 1);
        assert_eq!(t.split(5), (5, 0, 0), "the whole id is the package");
        assert!(!t.are_siblings(0, 1));
    }

    #[test]
    fn analyze_counts_packages_cores_and_threads() {
        let t = Topology::decode(&subleaves(1, 2));
        // Two packages, each 2 cores x 2 threads: ids 0..4 (pkg 0) and 4..8 (pkg 1).
        let ids: [u32; 8] = [0, 1, 2, 3, 4, 5, 6, 7];
        let mut scratch = [(0, 0, 0); 8];
        let (counts, unique) = analyze(&t, &ids, &mut scratch);
        assert!(unique);
        assert_eq!(
            counts,
            Counts {
                packages: 2,
                cores: 4,
                threads: 8
            }
        );

        // A flat topology over 4 ids: 4 packages, 4 cores, 4 threads.
        let (flat, u) = analyze(&Topology::FLAT, &[0, 1, 2, 3], &mut scratch);
        assert!(u);
        assert_eq!(
            flat,
            Counts {
                packages: 4,
                cores: 4,
                threads: 4
            }
        );

        // Duplicate ids collide (not unique).
        let (_, u2) = analyze(&t, &[0, 0, 1], &mut scratch);
        assert!(!u2);
    }
}
