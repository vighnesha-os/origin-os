//! The balancer's plan (ADR-0020 S7, S8 and S10; TASKS item 98): for each processor, the groups it
//! searches, nearest first, with each group's move period and whether its processors share a
//! last-level cache; and the choice among the threads a pull may take.
//!
//! Pure: built once from the scheduling domains (`origin_frame::domains`), no `unsafe`, tested on
//! the host. `sched`'s idle pull and idle kick read it.
//!
//! # Levels
//!
//! A processor's levels are the groups of the domains' tree that hold it, narrowest first (its
//! core, its caches, its die, its package, its node), and then the whole machine, unless the
//! widest of them already is the machine. An idle processor looks for a thread to pull at the
//! first level that has one (S7.1), and a crowded one looks for an idle processor the same way
//! (S7.2). So a thread leaves its node only when nothing nearer was found, which is S10's NUMA
//! rule for the idle pull.
//!
//! # Periods (S10)
//!
//! S10: "a thread moves at most once per period of the widest level the move crosses", one tick
//! at the core and cache levels, 4 at the package, 16 at the node and the machine. A move made at
//! a level, between two processors that level's one group holds, crosses every narrower level:
//! it leaves a core to stay in a cache, a cache to stay in a package, a package to stay in a
//! node, a node to stay in the machine. So each kind of level has the period of the move that
//! leaves it ([`crossing_period`]), and a level's period is the longest among the levels narrower
//! than it; the narrowest level's is one tick. On a tree in the usual order this is S10's table.
//!
//! It also holds on a tree in another order, which the kind of a level alone would not: the tree
//! is ordered by refinement, and QEMU's machines put two nodes inside one package and one cache,
//! so a move inside a node there leaves only a core (one tick), and a move that leaves the node
//! waits the node's 16, though it stays in the cache.
//!
//! A level of single processors is not in the tree, but leaving one is still crossed: where every
//! processor is a node, a package or a cache of its own, every move starts from that period.
//!
//! # Sharing the last-level cache (S10)
//!
//! Only a processor that shares the source's last-level cache may pull a cache-hot thread. Two
//! processors share it when one group of the tree's last-level-cache level holds both, so a
//! level shares it when it is that level or a narrower one.
//!
//! Where no processor reports a cache at all, the package stands in (the coordinating session's
//! ruling Q8 on TASKS item 98's contract): met only on an emulated AMD guest, whose software
//! processor reports no cache leaf. Where caches are reported and no level of the tree is the
//! last-level cache's, each processor has its own, and nobody shares.
#![forbid(unsafe_code)]

use alloc::vec::Vec;

use origin_frame::domains::{Kind, Tree};

/// The cache-hot window H, in microseconds (ADR-0020 S10: "H starts at 1 ms", a hypothesis).
pub const HOT_MICROS: u64 = 1000;

/// The period of a move that leaves a group of the kind `kind`, in ticks of the thread's
/// processor (S10; the module documentation, "Periods"): leaving a core or an L2 for another in
/// the same cache is S10's "cache level", one tick; leaving a last-level cache or a die for
/// another in the same package is its "package", 4; leaving a package or a node is its "node" and
/// "machine", 16.
pub const fn crossing_period(kind: Kind) -> u64 {
    match kind {
        Kind::Core | Kind::Cluster => 1,
        Kind::LastCache | Kind::Die => 4,
        Kind::Package | Kind::Node => 16,
    }
}

/// One level of the domains' tree, as the plan reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shape {
    /// The kinds the level stands for, in [`Kind::ALL`]'s order.
    pub kinds: Vec<Kind>,
    /// Its groups, each the CPU indexes of its processors, ascending.
    pub groups: Vec<Vec<u32>>,
}

/// The tree's levels, narrowest first, as [`Shape`]s.
pub fn shapes(tree: &Tree) -> Vec<Shape> {
    tree.levels()
        .iter()
        .map(|level| Shape {
            kinds: Kind::ALL.into_iter().filter(|&k| level.is(k)).collect(),
            groups: level.groups(),
        })
        .collect()
}

/// One level of one processor's search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    /// Which of the plan's groups: an index into [`Plan::groups`].
    pub group: usize,
    /// The move period of a move made at this level, in ticks.
    pub period: u64,
    /// Whether every processor of the group shares this processor's last-level cache.
    pub shares_cache: bool,
}

/// Where sharing the last-level cache is read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheFrom {
    /// The tree's last-level-cache level.
    Reported,
    /// The package level: no processor reports a cache.
    Package,
    /// Nobody shares: caches are reported and each processor has its own, or no cache is reported
    /// and each package holds one processor.
    Nobody,
}

/// The plan: every group, once, and each processor's levels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// Every group some processor searches, each the CPU indexes of its processors, ascending.
    pub groups: Vec<Vec<u32>>,
    /// For each CPU index, its levels, narrowest first; empty for an index the tree does not hold.
    pub near: Vec<Vec<Step>>,
    /// Where sharing the last-level cache is read from.
    pub cache_from: CacheFrom,
}

impl Plan {
    /// Builds the plan for CPU indexes `0..count` from the tree's levels `shapes`, narrowest
    /// first, and the processors it covers, `cpus`. `caches_reported`: whether any processor
    /// reports a cache (its own report, or a usable PPTT's). `nodes`: how many NUMA nodes hold a
    /// processor, 0 where the firmware places none.
    pub fn build(
        shapes: &[Shape],
        cpus: &[u32],
        count: usize,
        caches_reported: bool,
        nodes: usize,
    ) -> Plan {
        let has = |kind: Kind| shapes.iter().position(|shape| shape.kinds.contains(&kind));
        let (cache_level, cache_from) = match (has(Kind::LastCache), has(Kind::Package)) {
            (Some(level), _) => (Some(level), CacheFrom::Reported),
            (None, Some(level)) if !caches_reported => (Some(level), CacheFrom::Package),
            (None, _) => (None, CacheFrom::Nobody),
        };
        let mut groups: Vec<Vec<u32>> = Vec::new();
        let mut near: Vec<Vec<Step>> = (0..count).map(|_| Vec::new()).collect();
        // What every move crosses, though the tree has no level for it: a kind whose every group
        // is one processor. A cache of its own for each (caches reported, and no level for the
        // last one), a package of its own, a node of its own.
        let many = cpus.len() > 1;
        let mut period = 1;
        if many && caches_reported && has(Kind::LastCache).is_none() {
            period = period.max(crossing_period(Kind::LastCache));
        }
        if many && has(Kind::Package).is_none() {
            period = period.max(crossing_period(Kind::Package));
        }
        if many && nodes == cpus.len() {
            period = period.max(crossing_period(Kind::Node));
        }
        for (level, shape) in shapes.iter().enumerate() {
            // A move made at this level crosses every narrower one; the next level's moves cross
            // this one too.
            let step_period = period;
            let leaving = shape.kinds.iter().map(|&kind| crossing_period(kind)).max();
            period = period.max(leaving.unwrap_or(1));
            let period = step_period;
            let shares_cache = cache_level.is_some_and(|cache| level <= cache);
            for members in &shape.groups {
                let group = groups.len();
                groups.push(members.clone());
                for &cpu in members {
                    if let Some(steps) = near.get_mut(cpu as usize) {
                        steps.push(Step {
                            group,
                            period,
                            shares_cache,
                        });
                    }
                }
            }
        }
        // The machine, unless the widest level already holds every processor in one group.
        let whole = shapes
            .last()
            .is_some_and(|shape| shape.groups.len() == 1 && shape.groups[0].len() == cpus.len());
        if !whole && cpus.len() > 1 {
            let mut all = cpus.to_vec();
            all.sort_unstable();
            let group = groups.len();
            groups.push(all);
            for &cpu in cpus {
                if let Some(steps) = near.get_mut(cpu as usize) {
                    steps.push(Step {
                        group,
                        period,
                        shares_cache: false,
                    });
                }
            }
        }
        Plan {
            groups,
            near,
            cache_from,
        }
    }

    /// Processor `cpu`'s step at its level `level`.
    pub fn step(&self, cpu: usize, level: usize) -> Option<Step> {
        self.near.get(cpu)?.get(level).copied()
    }

    /// Processor `cpu`'s groups, narrowest first: what `origin_sync::balance`'s searches take.
    pub fn levels(&self, cpu: usize) -> Vec<&[u32]> {
        self.near
            .get(cpu)
            .map(|steps| {
                steps
                    .iter()
                    .filter_map(|step| self.groups.get(step.group).map(Vec::as_slice))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A thread a pull may take: its slot in the source's queue, its pass there, and whether the
/// source last judged it not cache-hot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub slot: usize,
    pub pass: u64,
    pub cold: bool,
}

/// Of `candidates`, the thread a pull takes (S7.1: "preferring threads that are not cache-hot"):
/// one that is not cache-hot before one that is, then the lowest pass, which has waited longest
/// for its share, then the lowest slot. A cache-hot thread only if `hot_allowed`: the puller
/// shares the source's last-level cache (S10).
pub fn pick(candidates: impl Iterator<Item = Candidate>, hot_allowed: bool) -> Option<Candidate> {
    candidates
        .filter(|candidate| candidate.cold || hot_allowed)
        .min_by_key(|candidate| (!candidate.cold, candidate.pass, candidate.slot))
}

/// A queue's load as it is published (S9): its runnable contexts' weights, times 1024 over the
/// processor's capacity, so a slower processor counts as more loaded for the same threads. A
/// capacity of 0 is read as 1: such a processor is the most loaded there is.
pub fn scaled(weight: u32, capacity: u16) -> u32 {
    let scaled = u64::from(weight) * 1024 / u64::from(capacity.max(1));
    u32::try_from(scaled).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(kinds: &[Kind], groups: &[&[u32]]) -> Shape {
        Shape {
            kinds: kinds.to_vec(),
            groups: groups.iter().map(|g| g.to_vec()).collect(),
        }
    }

    fn all(n: u32) -> Vec<u32> {
        (0..n).collect()
    }

    /// The SMT machine of `boot-domains`: `core, L2 4x2; node 2x4; last-level cache, package 1x8`.
    #[test]
    fn the_smt_machine_s_levels_periods_and_cache() {
        let shapes = [
            shape(
                &[Kind::Core, Kind::Cluster],
                &[&[0, 1], &[2, 3], &[4, 5], &[6, 7]],
            ),
            shape(&[Kind::Node], &[&[0, 1, 2, 3], &[4, 5, 6, 7]]),
            shape(
                &[Kind::LastCache, Kind::Package],
                &[&[0, 1, 2, 3, 4, 5, 6, 7]],
            ),
        ];
        let plan = Plan::build(&shapes, &all(8), 8, true, 2);
        assert_eq!(plan.cache_from, CacheFrom::Reported);
        // Processor 5: its core, its node, the cache that is the machine. No machine level is
        // added: the widest level holds every processor already.
        let levels = plan.levels(5);
        assert_eq!(
            levels,
            [&[4u32, 5][..], &[4, 5, 6, 7], &[0, 1, 2, 3, 4, 5, 6, 7]]
        );
        let steps: Vec<(u64, bool)> = (0..3)
            .map(|level| plan.step(5, level).unwrap())
            .map(|step| (step.period, step.shares_cache))
            .collect();
        // A node inside one last-level cache shares it. A move between a core's two threads, or
        // inside a node, leaves at most a core: one tick. A move that leaves the node waits 16,
        // though it stays in the cache.
        assert_eq!(steps, [(1, true), (1, true), (16, true)]);
        assert_eq!(plan.step(5, 3), None);
        assert_eq!(plan.groups.len(), 7);
    }

    /// The two-package machine: `last-level cache, package, node 2x4`, then the machine.
    #[test]
    fn two_packages_share_no_cache_and_the_machine_is_added() {
        let shapes = [shape(
            &[Kind::LastCache, Kind::Package, Kind::Node],
            &[&[0, 1, 2, 3], &[4, 5, 6, 7]],
        )];
        let plan = Plan::build(&shapes, &all(8), 8, true, 2);
        assert_eq!(
            plan.levels(6),
            [&[4u32, 5, 6, 7][..], &[0, 1, 2, 3, 4, 5, 6, 7]]
        );
        // Inside the package, cache and node: one tick. Leaving all three at once: the longest.
        assert_eq!(
            plan.step(6, 0),
            Some(Step {
                group: 1,
                period: 1,
                shares_cache: true
            })
        );
        assert_eq!(
            plan.step(6, 1),
            Some(Step {
                group: 2,
                period: 16,
                shares_cache: false
            })
        );
    }

    /// No processor reports a cache (an emulated AMD guest): the package stands in.
    #[test]
    fn with_no_cache_reported_the_package_stands_in() {
        let shapes = [
            shape(&[Kind::Node], &[&[0, 1], &[2, 3]]),
            shape(&[Kind::Package], &[&[0, 1, 2, 3]]),
        ];
        let periods = |plan: &Plan| -> Vec<(u64, bool)> {
            (0..2)
                .map(|l| plan.step(0, l).unwrap())
                .map(|s| (s.period, s.shares_cache))
                .collect()
        };
        let plan = Plan::build(&shapes, &all(4), 4, false, 2);
        assert_eq!(plan.cache_from, CacheFrom::Package);
        assert_eq!(periods(&plan), [(1, true), (16, true)]);
        // With caches reported and no last-level-cache level, each processor has its own: nobody
        // shares, and every move leaves a cache.
        let plan = Plan::build(&shapes, &all(4), 4, true, 2);
        assert_eq!(plan.cache_from, CacheFrom::Nobody);
        assert_eq!(periods(&plan), [(4, false), (16, false)]);
    }

    /// A flat machine whose tree has no level at all (every package one processor, no SRAT), a
    /// tree that does not hold an index, and one processor.
    #[test]
    fn a_flat_machine_has_only_the_machine_level() {
        let plan = Plan::build(&[], &[0, 1, 3], 4, false, 0);
        assert_eq!(plan.cache_from, CacheFrom::Nobody);
        assert_eq!(plan.levels(1), [&[0u32, 1, 3][..]]);
        // Every processor is a package of its own, so every move leaves one.
        assert_eq!(
            plan.step(3, 0),
            Some(Step {
                group: 0,
                period: 16,
                shares_cache: false
            })
        );
        // Index 2 did not start: it searches nothing, and nobody's level names it.
        assert!(plan.levels(2).is_empty());
        assert!(plan.levels(9).is_empty());
        // One processor has nobody to balance with.
        let plan = Plan::build(&[], &[0], 1, false, 0);
        assert!(plan.levels(0).is_empty() && plan.groups.is_empty());
    }

    /// QEMU's two-processor machine: one package and one cache, each processor a node of its
    /// own. The tree has no node level, and every move still leaves a node.
    #[test]
    fn a_level_of_single_processors_is_still_crossed() {
        let shapes = [shape(&[Kind::LastCache, Kind::Package], &[&[0, 1]])];
        let step = |nodes| Plan::build(&shapes, &all(2), 2, true, nodes).step(1, 0);
        assert_eq!(
            step(2),
            Some(Step {
                group: 0,
                period: 16,
                shares_cache: true
            })
        );
        // With one node, or none placed, the move leaves only a core.
        assert_eq!(step(1).map(|s| s.period), Some(1));
        assert_eq!(step(0).map(|s| s.period), Some(1));
    }

    /// On a tree in the usual order, the periods are S10's table: one tick at the core and cache
    /// levels, 4 at the package, 16 at the node and the machine.
    #[test]
    fn periods_follow_s10_on_a_tree_in_the_usual_order() {
        let cpus = all(16);
        let halves = |size: u32| -> Vec<Vec<u32>> {
            (0..16 / size)
                .map(|g| (g * size..(g + 1) * size).collect())
                .collect()
        };
        let level = |kind, size| Shape {
            kinds: vec![kind],
            groups: halves(size),
        };
        let shapes = [
            level(Kind::Core, 2),
            level(Kind::LastCache, 4),
            level(Kind::Package, 8),
            level(Kind::Node, 8),
        ];
        // Two packages, each a node: the tree would merge them; kept apart here, one above the
        // other, the periods are the same.
        let plan = Plan::build(&shapes, &cpus, 16, true, 2);
        let periods: Vec<u64> = (0..5).map(|l| plan.step(0, l).unwrap().period).collect();
        assert_eq!(periods, [1, 1, 4, 16, 16]);
        let shares: Vec<bool> = (0..5)
            .map(|l| plan.step(0, l).unwrap().shares_cache)
            .collect();
        assert_eq!(shares, [true, true, false, false, false]);
    }

    /// S7.1 and S10: not cache-hot first, then the lowest pass; a cache-hot thread only for a
    /// puller that shares the cache.
    #[test]
    fn a_pull_prefers_a_thread_that_is_not_cache_hot_and_then_the_lowest_pass() {
        let c = |slot, pass, cold| Candidate { slot, pass, cold };
        let some = [
            c(3, 50, false),
            c(5, 90, true),
            c(7, 70, true),
            c(2, 70, true),
        ];
        assert_eq!(pick(some.into_iter(), true), Some(c(2, 70, true)));
        assert_eq!(pick(some.into_iter(), false), Some(c(2, 70, true)));
        // Only cache-hot threads: taken by a sharer, the lowest pass first, and by nobody else.
        let hot = [c(4, 80, false), c(1, 20, false), c(6, 20, false)];
        assert_eq!(pick(hot.into_iter(), true), Some(c(1, 20, false)));
        assert_eq!(pick(hot.into_iter(), false), None);
        assert_eq!(pick(core::iter::empty(), true), None);
    }

    /// S9: load is weight over capacity, and capacity 0 is the heaviest there is.
    #[test]
    fn load_is_scaled_by_capacity() {
        assert_eq!(scaled(16, 1024), 16);
        assert_eq!(scaled(16, 512), 32);
        assert_eq!(scaled(16, 341), 48);
        assert_eq!(scaled(16, 0), 16 * 1024);
        assert_eq!(scaled(0, 1024), 0);
        assert_eq!(scaled(u32::MAX, 1), u32::MAX);
        // A slower processor with the same threads is the more loaded.
        assert!(scaled(24, 600) > scaled(24, 1024));
    }
}
