//! Scheduling domains: the tree of processors that share a core, a cache, a die, a package or a
//! NUMA node (ADR-0020 S8; CLAUDE.md §24, §27; TASKS item 97).
//!
//! Built once at boot from what each processor reported about itself (its topology layout and
//! the caches it shares, `origin_frame::topology`), the SRAT's nodes and, where the firmware has
//! one, the PPTT's caches. Pure, no `unsafe`, tested on the host; the kernel gathers the reports
//! and prints the tree.
//!
//! # Claims, and when they disagree
//!
//! A processor claims, for each level, the processors it shares that level with: those whose APIC
//! IDs agree with its own above some number of low bits (its *shift*). That is an aligned block of
//! IDs. Processors may disagree: on a hybrid part, or when a report is wrong. The rule here (the
//! coordinating session's ruling Q2 of 2026-11-21) gives each processor the **largest aligned
//! block that holds it, lies inside its own claim, and lies inside the claim of every processor
//! in it**. Aligned blocks either nest or are disjoint, so these groups always partition the
//! processors; two processors share a group only when both claim it, which is the conservative
//! reading ADR-0020 S8 asks for ("not shared"); and every processor whose claim was cut is
//! counted. On a hybrid part an efficient-core module's four cores claim one block for their L2
//! and a performance core claims its own, so nothing is cut.
//!
//! The PPTT's caches are a second claim on the cache levels. The two are combined by their meet
//! (each group split wherever either claim splits it), and every processor whose group either
//! claim would have made larger is counted (ruling Q1: a reading of S8 with ACPI 6.6
//! §5.2.31.2, which gives the PPTT precedence for a cache's flagged properties, not for which
//! processors share it).
//!
//! # The tree
//!
//! The levels are ordered by refinement, narrowest first, not by a fixed list: a NUMA node may
//! sit inside a package or hold several. A level equal to the one below it is merged into it, its
//! names joined; a level of single processors is left out (S8: "a level whose groups each hold
//! one child is dropped"). The whole machine is the root; a level is kept where a kind's one group
//! holds every processor, since it says what they all share. If the levels do not nest, the
//! crossing is named and the tree is built from the processors' own core, die and package levels
//! alone (ruling Q3), or from as many of those as nest.
//!
//! Every list here is in the order of the online processors, ascending CPU indexes; the processors'
//! own CPU indexes are kept beside it, so a processor that did not start leaves a gap, not a
//! shifted name (the review of TASKS item 97, R97-1).
#![forbid(unsafe_code)]

use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use crate::once::SetOnce;
use crate::topology::{CpuTopology, Domain};

/// What a level of the tree stands for, narrowest first in the usual case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// An SMT core: its threads.
    Core,
    /// A shared level-2 cache: on Intel's efficient cores, a module.
    Cluster,
    /// The last-level cache: on AMD, a core complex.
    LastCache,
    /// A die.
    Die,
    /// A package.
    Package,
    /// A NUMA node: an SRAT proximity domain.
    Node,
}

impl Kind {
    /// Every kind, in this order.
    pub const ALL: [Kind; KINDS] = [
        Kind::Core,
        Kind::Cluster,
        Kind::LastCache,
        Kind::Die,
        Kind::Package,
        Kind::Node,
    ];

    /// Its name on the boot's lines.
    pub const fn name(self) -> &'static str {
        match self {
            Kind::Core => "core",
            Kind::Cluster => "L2",
            Kind::LastCache => "last-level cache",
            Kind::Die => "die",
            Kind::Package => "package",
            Kind::Node => "node",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }

    /// The kinds the processors' own layouts give, which the tree falls back to.
    const fn is_from_layout(self) -> bool {
        matches!(self, Kind::Core | Kind::Die | Kind::Package)
    }
}

/// How many kinds there are.
pub const KINDS: usize = 6;

/// A partition of processors `0..n`: for each, its group, named by the group's first processor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Partition {
    group: Vec<u32>,
}

impl Partition {
    /// The partition in which processors with equal keys share a group.
    pub fn from_keys<K: Ord + Copy>(keys: &[K]) -> Partition {
        let mut order: Vec<usize> = (0..keys.len()).collect();
        order.sort_by_key(|&i| (keys[i], i));
        let mut group = vec![0u32; keys.len()];
        let mut first = 0usize;
        for (at, &i) in order.iter().enumerate() {
            if at == 0 || keys[order[at - 1]] != keys[i] {
                first = i;
            }
            group[i] = first as u32;
        }
        Partition { group }
    }

    /// Every processor alone.
    pub fn discrete(n: usize) -> Partition {
        Partition {
            group: (0..n as u32).collect(),
        }
    }

    /// The processors it partitions.
    pub fn len(&self) -> usize {
        self.group.len()
    }

    /// Whether it partitions none.
    pub fn is_empty(&self) -> bool {
        self.group.is_empty()
    }

    /// Processor `i`'s group, named by its first processor.
    pub fn group_of(&self, i: usize) -> Option<u32> {
        self.group.get(i).copied()
    }

    /// How many groups.
    pub fn groups(&self) -> usize {
        self.group
            .iter()
            .enumerate()
            .filter(|&(i, &g)| g as usize == i)
            .count()
    }

    /// Each group's members, the groups in order of their first member.
    pub fn members(&self) -> Vec<Vec<u32>> {
        let mut out: Vec<Vec<u32>> = Vec::new();
        let mut slot = vec![usize::MAX; self.group.len()];
        for (i, &g) in self.group.iter().enumerate() {
            let g = g as usize;
            if slot[g] == usize::MAX {
                slot[g] = out.len();
                out.push(Vec::new());
            }
            out[slot[g]].push(i as u32);
        }
        out
    }

    /// Whether every group of this one lies inside a group of `coarser`.
    pub fn refines(&self, coarser: &Partition) -> bool {
        self.group.len() == coarser.group.len()
            && self
                .group
                .iter()
                .zip(&coarser.group)
                .all(|(&own, &theirs)| coarser.group[own as usize] == theirs)
    }

    /// The meet: processors share a group when they share one in both.
    pub fn meet(&self, other: &Partition) -> Partition {
        let keys: Vec<(u32, u32)> = self
            .group
            .iter()
            .zip(&other.group)
            .map(|(&a, &b)| (a, b))
            .collect();
        Partition::from_keys(&keys)
    }

    /// The size of each processor's group.
    fn sizes(&self) -> Vec<u32> {
        let mut count = vec![0u32; self.group.len()];
        for &g in &self.group {
            count[g as usize] += 1;
        }
        self.group.iter().map(|&g| count[g as usize]).collect()
    }
}

/// The processors as one level of the tree prints them: `4x2 [0-1|2-3|4-5|6-7]`, or
/// `2 groups [0-63|64-66]` where the groups differ in size.
impl fmt::Display for Partition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let members = self.members();
        let size = members.first().map_or(0, Vec::len);
        if members.iter().all(|m| m.len() == size) {
            write!(f, "{}x{size} [", members.len())?;
        } else {
            write!(f, "{} groups [", members.len())?;
        }
        for (g, group) in members.iter().enumerate() {
            if g > 0 {
                f.write_str("|")?;
            }
            write_ranges(f, group)?;
        }
        f.write_str("]")
    }
}

/// `0-3,8-11`: ascending indexes as ranges.
fn write_ranges(f: &mut fmt::Formatter<'_>, sorted: &[u32]) -> fmt::Result {
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        if start != sorted[0] {
            f.write_str(",")?;
        }
        if start == end {
            write!(f, "{start}")?;
        } else {
            write!(f, "{start}-{end}")?;
        }
        i += 1;
    }
    Ok(())
}

/// The aligned-block rule (the module comment): each processor's group from its APIC ID and its
/// claim, the low bits its group may differ in (`None`: only itself). Returns the partition and
/// how many processors' claims were cut: those whose group holds fewer of the processors than
/// their claim covers (R97-10). Two processors with one APIC ID never share a group by that alone:
/// at width 0 each is its own (R97-9).
pub fn aligned(claims: &[(u32, Option<u8>)]) -> (Partition, usize) {
    let n = claims.len();
    let claim = |i: usize| u32::from(claims[i].1.unwrap_or(0)).min(32);
    let key = |i: usize, s: u32| u64::from(claims[i].0) >> s;
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (claims[i].0, i));
    let top = (0..n).map(claim).max().unwrap_or(0);
    // For each width `s`, a processor whose block of width `s` holds only processors claiming at
    // least `s` may have that block. The property only weakens as `s` grows (a smaller block's
    // members are a subset), so the last `s` that holds is the largest.
    let mut best = vec![0u32; n];
    for s in 0..=top {
        let mut i = 0;
        while i < n {
            let k = key(order[i], s);
            let mut j = i;
            let mut least = u32::MAX;
            while j < n && key(order[j], s) == k {
                least = least.min(claim(order[j]));
                j += 1;
            }
            if least >= s {
                for &p in &order[i..j] {
                    best[p] = s;
                }
            }
            i = j;
        }
    }
    let keys: Vec<(u32, u64, usize)> = (0..n)
        .map(|i| (best[i], key(i, best[i]), if best[i] == 0 { i } else { 0 }))
        .collect();
    let partition = Partition::from_keys(&keys);
    // How many processors each claim covers: the run of sorted IDs inside its block.
    let ids: Vec<u64> = order.iter().map(|&i| u64::from(claims[i].0)).collect();
    let covered = |i: usize| {
        let s = claim(i);
        let block = u64::from(claims[i].0) >> s;
        let first = ids.partition_point(|&id| id >> s < block);
        let past = ids.partition_point(|&id| id >> s <= block);
        past - first
    };
    let sizes = partition.sizes();
    let cut = (0..n).filter(|&i| (sizes[i] as usize) < covered(i)).count();
    (partition, cut)
}

/// One processor's reports, in CPU-index order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Processor {
    /// Its CPU index.
    pub cpu: u32,
    /// Its x2APIC ID, as the MADT lists it.
    pub apic_id: u32,
    /// What it recorded about itself, if it did.
    pub topology: Option<CpuTopology>,
    /// Its SRAT proximity domain, if the SRAT places it.
    pub node: Option<u32>,
}

/// The PPTT's claims on the cache levels: for each processor, in CPU-index order, the cache
/// instance it reaches at that level (equal instances are one cache), or `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PpttClaims {
    /// The level-2 cache that holds data.
    pub second_level: Vec<Option<u64>>,
    /// The highest-level cache that holds data: its level, counted from the processor's own
    /// first cache, and its instance.
    pub last_level: Vec<Option<(u8, u64)>>,
}

/// What the tree is built from.
#[derive(Clone, Copy, Debug)]
pub struct Input<'a> {
    /// Every online processor, in CPU-index order.
    pub processors: &'a [Processor],
    /// Whether the firmware has an SRAT: if not, there is no node level and nobody is unplaced.
    pub srat: bool,
    /// The PPTT's claims, where the firmware has a usable PPTT with caches.
    pub pptt: Option<&'a PpttClaims>,
}

/// One level of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Level {
    kinds: [bool; KINDS],
    partition: Partition,
    /// The CPU index of each processor the partition numbers.
    cpus: Vec<u32>,
}

impl Level {
    /// Whether it stands for `kind` (several, where their groups are the same).
    pub fn is(&self, kind: Kind) -> bool {
        self.kinds[kind.index()]
    }

    /// Its groups, over the processors' positions in the input.
    pub fn partition(&self) -> &Partition {
        &self.partition
    }

    /// The CPU indexes of the processors in each group, the groups in order of their first.
    pub fn groups(&self) -> Vec<Vec<u32>> {
        self.partition
            .members()
            .into_iter()
            .map(|m| {
                m.into_iter()
                    .map(|i| self.cpus.get(i as usize).copied().unwrap_or(u32::MAX))
                    .collect()
            })
            .collect()
    }
}

/// `core 4x2 [0-1|2-3|4-5|6-7]`, or with merged names `package, last-level cache 2x4 [0-3|4-7]`.
impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for kind in Kind::ALL.iter().filter(|&&k| self.is(k)) {
            if !first {
                f.write_str(", ")?;
            }
            f.write_str(kind.name())?;
            first = false;
        }
        let groups = self.groups();
        let size = groups.first().map_or(0, Vec::len);
        if groups.iter().all(|g| g.len() == size) {
            write!(f, " {}x{size} [", groups.len())?;
        } else {
            write!(f, " {} groups [", groups.len())?;
        }
        for (g, group) in groups.iter().enumerate() {
            if g > 0 {
                f.write_str("|")?;
            }
            write_ranges(f, group)?;
        }
        f.write_str("]")
    }
}

/// What the tree's build found wrong. Nothing here stops the build: each is the conservative
/// reading, counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Findings {
    /// Processors that recorded nothing about themselves.
    pub unrecorded: usize,
    /// For each kind, processors whose own claim was cut because another's claimed less.
    pub cut: [usize; KINDS],
    /// For each cache kind, processors whose group CPUID or the PPTT would have made larger.
    pub pptt_differs: [usize; KINDS],
    /// For each cache kind, processors whose PPTT and CPUID name different caches at that level:
    /// one names a level-2 cache and the other none, or their last levels differ in number. The
    /// PPTT's claims on that kind are then not used (R97-3).
    pub pptt_levels: [usize; KINDS],
    /// Processors the SRAT does not place, while it exists.
    pub unplaced: usize,
    /// Two levels that did not nest, the first such pair: the tree then fell back.
    pub crossing: Option<(Kind, Kind)>,
}

impl Findings {
    /// Processors whose claim was cut, over every kind.
    pub fn cuts(&self) -> usize {
        self.cut.iter().sum()
    }

    /// Processors whose CPUID and PPTT groups differed, over every kind.
    pub fn differences(&self) -> usize {
        self.pptt_differs.iter().sum()
    }

    /// Processors whose CPUID and PPTT name different caches, over every kind.
    pub fn level_mismatches(&self) -> usize {
        self.pptt_levels.iter().sum()
    }

    /// Whether everything agreed.
    pub fn agree(&self) -> bool {
        self.unrecorded == 0
            && self.cuts() == 0
            && self.differences() == 0
            && self.level_mismatches() == 0
            && self.unplaced == 0
            && self.crossing.is_none()
    }
}

/// The scheduling domains: the levels, narrowest first, without the processors themselves. The
/// whole machine is the root: it is a level only where a kind's one group holds every processor
/// (a package, a node, a cache), which says what the machine's processors share.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tree {
    cpus: Vec<u32>,
    levels: Vec<Level>,
    findings: Findings,
}

impl Tree {
    /// Builds the tree (the module comment).
    pub fn build(input: &Input<'_>) -> Tree {
        let n = input.processors.len();
        let cpus: Vec<u32> = input.processors.iter().map(|p| p.cpu).collect();
        let mut findings = Findings {
            unrecorded: input
                .processors
                .iter()
                .filter(|p| p.topology.is_none())
                .count(),
            ..Findings::default()
        };
        let mut candidates: Vec<(Kind, Partition)> = Vec::new();
        for kind in Kind::ALL {
            if let Some(partition) = level(input, kind, &mut findings) {
                candidates.push((kind, partition));
            }
        }
        let levels = match chain(&candidates, &cpus) {
            Ok(levels) => levels,
            Err(crossing) => {
                findings.crossing = Some(crossing);
                let own: Vec<(Kind, Partition)> = candidates
                    .into_iter()
                    .filter(|(kind, _)| kind.is_from_layout())
                    .collect();
                longest_chain(&own, &cpus)
            }
        };
        Tree {
            cpus,
            // A level of single processors is the processors themselves; on one processor the
            // only level is both.
            levels: levels
                .into_iter()
                .filter(|l| n == 1 || l.partition.groups() != n)
                .collect(),
            findings,
        }
    }

    /// The processors it covers.
    pub fn processors(&self) -> usize {
        self.cpus.len()
    }

    /// Their CPU indexes, in the input's order: position `i` of every partition is `cpus()[i]`.
    pub fn cpus(&self) -> &[u32] {
        &self.cpus
    }

    /// Its levels, narrowest first.
    pub fn levels(&self) -> &[Level] {
        &self.levels
    }

    /// What its build found wrong.
    pub fn findings(&self) -> &Findings {
        &self.findings
    }
}

/// One candidate level's partition, or `None` where nothing reports that level.
fn level(input: &Input<'_>, kind: Kind, findings: &mut Findings) -> Option<Partition> {
    let ps = input.processors;
    let claims = |shift: &dyn Fn(&CpuTopology) -> Option<u8>| -> Vec<(u32, Option<u8>)> {
        ps.iter()
            .map(|p| (p.apic_id, p.topology.as_ref().and_then(shift)))
            .collect()
    };
    let layout = |domain: Domain| move |t: &CpuTopology| Some(t.layout.shift(domain).min(32) as u8);
    let from_cpuid = match kind {
        Kind::Core => Some(claims(&layout(Domain::Core))),
        Kind::Die => Some(claims(&layout(Domain::Die))),
        Kind::Package => Some(claims(&layout(Domain::Package))),
        Kind::Cluster => Some(claims(&|t: &CpuTopology| {
            t.caches.second_level().map(|c| c.shift)
        })),
        Kind::LastCache => Some(claims(&|t: &CpuTopology| {
            t.caches.last_level().map(|c| c.shift)
        })),
        Kind::Node => None,
    };
    match kind {
        Kind::Node => {
            if !input.srat {
                return None;
            }
            findings.unplaced = ps.iter().filter(|p| p.node.is_none()).count();
            // An unplaced processor is a node of its own: nothing says it shares one.
            let keys: Vec<(bool, u32)> = ps
                .iter()
                .enumerate()
                .map(|(i, p)| p.node.map_or((true, i as u32), |node| (false, node)))
                .collect();
            Some(Partition::from_keys(&keys))
        }
        Kind::Core | Kind::Die | Kind::Package => {
            let claims = from_cpuid?;
            if ps.iter().all(|p| p.topology.is_none()) {
                return None;
            }
            // A die only where a processor's leaf enumerates one: otherwise its field is empty
            // and the die would only repeat the package under another name.
            if kind == Kind::Die
                && !ps
                    .iter()
                    .filter_map(|p| p.topology.as_ref())
                    .any(|t| t.layout.width(Domain::Die) > 0)
            {
                return None;
            }
            let (partition, cut) = aligned(&claims);
            findings.cut[kind.index()] = cut;
            Some(partition)
        }
        Kind::Cluster | Kind::LastCache => {
            let claims = from_cpuid?;
            let reported = claims.iter().any(|c| c.1.is_some());
            let cpuid = reported.then(|| {
                let (partition, cut) = aligned(&claims);
                findings.cut[kind.index()] = cut;
                partition
            });
            // The PPTT's claim, where it names a cache at this level for any processor: each
            // processor's cache there with its level. One it names none for is alone: nothing says
            // it shares that cache.
            let mut pptt: Option<Vec<Option<(u8, u64)>>> = input
                .pptt
                .map(|p| {
                    if kind == Kind::Cluster {
                        p.second_level.iter().map(|x| x.map(|i| (2, i))).collect()
                    } else {
                        p.last_level.clone()
                    }
                })
                .filter(|levels: &Vec<Option<(u8, u64)>>| levels.iter().any(Option::is_some));
            // Where both speak they must name the same cache: a level-2 cache for both, or last
            // levels of one number. Where they do not, the PPTT is not used for this kind, and
            // every such processor is counted (R97-3).
            if let (true, Some(levels)) = (reported, &pptt) {
                let cpuid_level = |i: usize| {
                    ps[i]
                        .topology
                        .as_ref()
                        .and_then(|t| {
                            if kind == Kind::Cluster {
                                t.caches.second_level()
                            } else {
                                t.caches.last_level()
                            }
                        })
                        .map(|c| c.level)
                };
                let mismatched = (0..ps.len())
                    .filter(|&i| levels.get(i).copied().flatten().map(|(l, _)| l) != cpuid_level(i))
                    .count();
                if mismatched > 0 {
                    findings.pptt_levels[kind.index()] = mismatched;
                    pptt = None;
                }
            }
            let pptt = pptt.map(|levels| {
                let keys: Vec<(bool, u64)> = (0..ps.len())
                    .map(|i| match levels.get(i).copied().flatten() {
                        Some((_, instance)) => (false, instance),
                        None => (true, i as u64),
                    })
                    .collect();
                Partition::from_keys(&keys)
            });
            match (cpuid, pptt) {
                (Some(cpuid), Some(pptt)) => {
                    let meet = cpuid.meet(&pptt);
                    let (a, b, m) = (cpuid.sizes(), pptt.sizes(), meet.sizes());
                    findings.pptt_differs[kind.index()] = (0..ps.len())
                        .filter(|&i| m[i] < a[i] || m[i] < b[i])
                        .count();
                    Some(meet)
                }
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            }
        }
    }
}

/// The candidates ordered into a chain, narrowest first, equal partitions merged; or the first
/// pair that crosses.
fn chain(candidates: &[(Kind, Partition)], cpus: &[u32]) -> Result<Vec<Level>, (Kind, Kind)> {
    let mut sorted: Vec<&(Kind, Partition)> = candidates.iter().collect();
    sorted.sort_by_key(|(kind, p)| (core::cmp::Reverse(p.groups()), *kind));
    let mut levels: Vec<Level> = Vec::new();
    let mut last_kind: Option<Kind> = None;
    for (kind, partition) in sorted {
        match levels.last_mut() {
            Some(top) if top.partition == *partition => top.kinds[kind.index()] = true,
            Some(top) if !top.partition.refines(partition) => {
                return Err((last_kind.unwrap_or(*kind), *kind));
            }
            _ => {
                let mut kinds = [false; KINDS];
                kinds[kind.index()] = true;
                levels.push(Level {
                    kinds,
                    partition: partition.clone(),
                    cpus: cpus.to_vec(),
                });
            }
        }
        last_kind = Some(*kind);
    }
    Ok(levels)
}

/// The longest chain the candidates give, taken narrowest first: the fallback when the levels
/// cross. The processors' own core, die and package levels nest whenever their layouts agree;
/// where even they cross, the chain stops before the first that does.
fn longest_chain(candidates: &[(Kind, Partition)], cpus: &[u32]) -> Vec<Level> {
    for take in (0..=candidates.len()).rev() {
        if let Ok(levels) = chain(&candidates[..take], cpus) {
            return levels;
        }
    }
    Vec::new()
}

/// For each node that holds a processor, the others in ascending SLIT distance (ruling Q7), for
/// the idle pull's outward search (TASKS item 98). `distance(from, to)` is the SLIT's, `None`
/// where the SLIT does not reach: such a node comes last. Ties keep the nodes' order.
pub fn node_order(
    nodes: &[u32],
    distance: impl Fn(u32, u32) -> Option<u8>,
) -> Vec<(u32, Vec<u32>)> {
    let mut held: Vec<u32> = nodes.to_vec();
    held.sort_unstable();
    held.dedup();
    held.iter()
        .map(|&from| {
            let mut others: Vec<u32> = held.iter().copied().filter(|&to| to != from).collect();
            others.sort_by_key(|&to| (distance(from, to).map_or(u16::MAX, u16::from), to));
            (from, others)
        })
        .collect()
}

/// What the kernel keeps after the boot: the tree, each node's others by distance, and each
/// processor's capacity (ADR-0020 S9), all in CPU-index order. Items 98 and 100 read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Domains {
    /// The tree.
    pub tree: Tree,
    /// Each node's others, nearest first.
    pub node_order: Vec<(u32, Vec<u32>)>,
    /// Each processor's capacity, from 0 to [`crate::capacity::FULL`], in the order of
    /// `tree.cpus()`.
    pub capacity: Vec<u16>,
}

/// The domains, set once at boot (ruling Q13: no per-processor table, so the count of per-CPU
/// tables is unchanged).
pub static DOMAINS: SetOnce<Domains> = SetOnce::new();

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Rng, cases};
    use crate::topology::{CacheLeaf, Caches};
    use std::string::ToString;

    /// Brute force: P's group is the largest aligned block inside every member's claim, and two
    /// processors with one APIC ID stand apart at width 0.
    fn model(claims: &[(u32, Option<u8>)]) -> Vec<(u32, u64, usize)> {
        let claim = |i: usize| u32::from(claims[i].1.unwrap_or(0)).min(32);
        (0..claims.len())
            .map(|p| {
                let mut s = claim(p);
                loop {
                    let block = u64::from(claims[p].0) >> s;
                    let ok = (0..claims.len())
                        .filter(|&q| u64::from(claims[q].0) >> s == block)
                        .all(|q| claim(q) >= s);
                    if ok {
                        return (s, block, if s == 0 { p } else { 0 });
                    }
                    s -= 1;
                }
            })
            .collect()
    }

    #[test]
    fn the_aligned_block_rule_matches_its_definition() {
        let mut rng = Rng::new(0x444F_4D41_494E);
        for case in 0..cases(3000) {
            let n = 1 + rng.below(24);
            let width = 1 + rng.below(7) as u32;
            let claims: Vec<(u32, Option<u8>)> = (0..n)
                .map(|_| {
                    let id = rng.next() as u32 & ((1 << width) - 1);
                    let shift = match rng.below(6) {
                        0 => None,
                        _ => Some(rng.below(width as usize + 2) as u8),
                    };
                    (id, shift)
                })
                .collect();
            let (partition, cut) = aligned(&claims);
            let model = model(&claims);
            for i in 0..n {
                for j in 0..n {
                    assert_eq!(
                        partition.group_of(i) == partition.group_of(j),
                        model[i] == model[j],
                        "case {case}: {claims:?}"
                    );
                    // Conservative: grouped only where both claims hold the other.
                    if partition.group_of(i) == partition.group_of(j) {
                        let holds = |a: usize, b: usize| {
                            let s = u32::from(claims[a].1.unwrap_or(0)).min(32);
                            u64::from(claims[a].0) >> s == u64::from(claims[b].0) >> s
                        };
                        assert!(holds(i, j) && holds(j, i), "case {case}");
                    }
                }
            }
            let expect_cut = (0..n)
                .filter(|&i| {
                    let s = u32::from(claims[i].1.unwrap_or(0)).min(32);
                    let covered = (0..n)
                        .filter(|&j| u64::from(claims[j].0) >> s == u64::from(claims[i].0) >> s)
                        .count();
                    (0..n).filter(|&j| model[j] == model[i]).count() < covered
                })
                .count();
            assert_eq!(cut, expect_cut, "case {case}");
        }
    }

    /// The SMT machine's L3 with CPU 3 claiming a block of 4 where the others claim 8 (control
    /// C1): the groups become 0-3 and 4-7, and seven claims are cut.
    #[test]
    fn one_narrower_claim_cuts_the_others_to_aligned_halves() {
        let claims: Vec<(u32, Option<u8>)> = (0..8)
            .map(|id| (id, Some(if id == 3 { 2 } else { 3 })))
            .collect();
        let (partition, cut) = aligned(&claims);
        assert_eq!(partition.to_string(), "2x4 [0-3|4-7]");
        assert_eq!(cut, 7);
    }

    /// Ruling Q2's case: a hybrid part, two efficient-core modules of four cores sharing an L2
    /// (APIC IDs 16 to 23, one thread each) beside four performance cores of two threads with
    /// their own L2 (IDs 0 to 7). Nothing is cut; a global narrowest shift would have split the
    /// modules.
    #[test]
    fn efficient_core_modules_keep_their_shared_l2() {
        let mut claims: Vec<(u32, Option<u8>)> = (0..8).map(|id| (id, Some(1))).collect();
        claims.extend((16..24).map(|id| (id, Some(2))));
        let (partition, cut) = aligned(&claims);
        assert_eq!(cut, 0);
        assert_eq!(
            partition.to_string(),
            "6 groups [0-1|2-3|4-5|6-7|8-11|12-15]"
        );
    }

    fn cpu(apic_id: u32, thread_bits: u32, l2: u32, l3: u32) -> CpuTopology {
        // A layout from leaf 0Bh: `thread_bits` of thread, 2 of core, the package above.
        let layout_words = |sub: u32| match sub {
            0 => [thread_bits, 1, 0x100, apic_id],
            1 => [thread_bits + 2, 4, 0x201, apic_id],
            _ => [0, 0, 2, apic_id],
        };
        let cache = |sub: u32| match sub {
            0 => [3 | 2 << 5 | (l2 - 1) << 14, 0, 0, 0],
            1 => [3 | 3 << 5 | (l3 - 1) << 14, 0, 0, 0],
            _ => [0; 4],
        };
        let mut own = CpuTopology::detect(|leaf, sub| match leaf {
            0 => [0x0B, 0, 0, 0],
            0x8000_0000 => [0x8000_0008, 0, 0, 0],
            0x0B => layout_words(sub),
            4 => cache(sub),
            _ => [0; 4],
        });
        own.caches = Caches::decode(CacheLeaf::Leaf04, cache);
        own
    }

    /// The SMT machine of ADR-0020 S23 (`-smp 8,sockets=1,cores=4,threads=2`, two nodes of two
    /// cores, L2 per core, L3 per socket): the node level sits inside the package.
    fn smt_machine(l3: impl Fn(u32) -> u32) -> Vec<Processor> {
        (0..8)
            .map(|id| Processor {
                cpu: id,
                apic_id: id,
                topology: Some(cpu(id, 1, 2, l3(id))),
                node: Some(id / 4),
            })
            .collect()
    }

    fn lines(tree: &Tree) -> Vec<String> {
        tree.levels().iter().map(ToString::to_string).collect()
    }

    #[test]
    fn the_smt_machine_has_cores_inside_nodes_inside_the_package() {
        let ps = smt_machine(|_| 8);
        let tree = Tree::build(&Input {
            processors: &ps,
            srat: true,
            pptt: None,
        });
        assert_eq!(
            lines(&tree),
            [
                "core, L2 4x2 [0-1|2-3|4-5|6-7]",
                "node 2x4 [0-3|4-7]",
                "last-level cache, package 1x8 [0-7]"
            ]
        );
        assert!(tree.findings().agree());
        let tree = Tree::build(&Input {
            processors: &ps,
            srat: false,
            pptt: None,
        });
        assert_eq!(
            lines(&tree),
            [
                "core, L2 4x2 [0-1|2-3|4-5|6-7]",
                "last-level cache, package 1x8 [0-7]"
            ]
        );
    }

    #[test]
    fn a_planted_narrow_shift_is_cut_and_counted() {
        let ps = smt_machine(|id| if id == 3 { 4 } else { 8 });
        let tree = Tree::build(&Input {
            processors: &ps,
            srat: true,
            pptt: None,
        });
        assert_eq!(
            lines(&tree),
            [
                "core, L2 4x2 [0-1|2-3|4-5|6-7]",
                "last-level cache, node 2x4 [0-3|4-7]",
                "package 1x8 [0-7]"
            ]
        );
        assert_eq!(tree.findings().cut[Kind::LastCache.index()], 7);
        assert!(!tree.findings().agree());
    }

    /// Control C2's shape: a PPTT that says the L3 is per node where CPUID says per socket. The
    /// meet is per node, and every processor is counted.
    #[test]
    fn a_pptt_that_disagrees_is_met_and_counted() {
        let ps = smt_machine(|_| 8);
        let pptt = PpttClaims {
            second_level: (0..8).map(|i| Some(100 + i / 2)).collect(),
            last_level: (0..8).map(|i| Some((3, 200 + i / 4))).collect(),
        };
        let tree = Tree::build(&Input {
            processors: &ps,
            srat: true,
            pptt: Some(&pptt),
        });
        assert_eq!(
            lines(&tree),
            [
                "core, L2 4x2 [0-1|2-3|4-5|6-7]",
                "last-level cache, node 2x4 [0-3|4-7]",
                "package 1x8 [0-7]"
            ]
        );
        assert_eq!(tree.findings().pptt_differs[Kind::LastCache.index()], 8);
        assert_eq!(tree.findings().pptt_differs[Kind::Cluster.index()], 0);
        // An agreeing PPTT changes nothing.
        let agreeing = PpttClaims {
            second_level: (0..8).map(|i| Some(100 + i / 2)).collect(),
            last_level: (0..8).map(|_| Some((3, 7))).collect(),
        };
        let tree = Tree::build(&Input {
            processors: &ps,
            srat: true,
            pptt: Some(&agreeing),
        });
        assert!(tree.findings().agree());
        assert_eq!(tree.levels().len(), 3);
    }

    /// With no cache leaf (QEMU's AMD-vendor software processor), the PPTT alone gives the cache
    /// levels.
    #[test]
    fn the_pptt_alone_where_cpuid_reports_no_cache() {
        let mut ps = smt_machine(|_| 8);
        for p in &mut ps {
            if let Some(t) = &mut p.topology {
                t.caches = Caches::NONE;
            }
        }
        let tree = Tree::build(&Input {
            processors: &ps,
            srat: false,
            pptt: None,
        });
        assert_eq!(
            lines(&tree),
            ["core 4x2 [0-1|2-3|4-5|6-7]", "package 1x8 [0-7]"]
        );
        let pptt = PpttClaims {
            second_level: vec![None; 8],
            last_level: (0..8).map(|i| Some((3, i / 4))).collect(),
        };
        let tree = Tree::build(&Input {
            processors: &ps,
            srat: false,
            pptt: Some(&pptt),
        });
        assert_eq!(
            lines(&tree),
            [
                "core 4x2 [0-1|2-3|4-5|6-7]",
                "last-level cache 2x4 [0-3|4-7]",
                "package 1x8 [0-7]"
            ]
        );
        assert!(tree.findings().agree());
    }

    /// The two-package machine (`-smp 8,sockets=2,cores=4,threads=1`, a node per package).
    #[test]
    fn the_two_package_machine_is_one_level() {
        let ps: Vec<Processor> = (0..8u32)
            .map(|i| {
                // Two cores' bits and the package above: package 1 starts at APIC ID 4.
                let mut own = cpu(i, 0, 1, 4);
                own.apic_id = i;
                Processor {
                    cpu: i,
                    apic_id: i,
                    topology: Some(own),
                    node: Some(i / 4),
                }
            })
            .collect();
        let tree = Tree::build(&Input {
            processors: &ps,
            srat: true,
            pptt: None,
        });
        assert_eq!(
            lines(&tree),
            ["last-level cache, package, node 2x4 [0-3|4-7]"]
        );
        assert!(tree.findings().agree());
    }

    /// Nodes that cross the last-level caches: the crossing is named and the tree falls back to
    /// core, die and package.
    #[test]
    fn levels_that_cross_fall_back_to_the_layout() {
        let mut ps = smt_machine(|_| 4);
        for (i, p) in ps.iter_mut().enumerate() {
            p.node = Some(if (2..6).contains(&i) { 1 } else { 0 });
        }
        let tree = Tree::build(&Input {
            processors: &ps,
            srat: true,
            pptt: None,
        });
        assert_eq!(
            tree.findings().crossing,
            Some((Kind::LastCache, Kind::Node))
        );
        assert_eq!(
            lines(&tree),
            ["core 4x2 [0-1|2-3|4-5|6-7]", "package 1x8 [0-7]"]
        );
    }

    #[test]
    fn unplaced_and_unrecorded_processors_are_counted_and_stand_alone() {
        let mut ps = smt_machine(|_| 8);
        ps[5].node = None;
        ps[6].topology = None;
        let tree = Tree::build(&Input {
            processors: &ps,
            srat: true,
            pptt: None,
        });
        let f = tree.findings();
        assert_eq!((f.unplaced, f.unrecorded), (1, 1));
        assert!(!f.agree());
        // CPU 6 claims nothing, so its core sibling 7 is cut from it, and CPU 5 is in no node,
        // which crosses its core with 4: the tree falls back.
        assert!(f.cut[Kind::Core.index()] > 0);
    }

    /// Random reports never give a tree that is not a chain of partitions of every processor,
    /// narrowest first, and nothing panics (CLAUDE.md §72).
    #[test]
    fn any_reports_give_a_chain_of_partitions() {
        let mut rng = Rng::new(0x5452_4545);
        for case in 0..cases(1500) {
            let n = 1 + rng.below(20);
            let ps: Vec<Processor> = (0..n as u32)
                .map(|index| {
                    let id = rng.below(64) as u32;
                    let topology = (rng.below(8) != 0).then(|| {
                        cpu(
                            id,
                            rng.below(3) as u32,
                            1 + rng.below(8) as u32,
                            1 + rng.below(64) as u32,
                        )
                    });
                    Processor {
                        cpu: index,
                        apic_id: id,
                        topology,
                        node: (rng.below(6) != 0).then(|| rng.below(3) as u32),
                    }
                })
                .collect();
            let pptt = (rng.below(3) == 0).then(|| PpttClaims {
                second_level: (0..n)
                    .map(|_| (rng.below(4) != 0).then(|| rng.below(4) as u64))
                    .collect(),
                last_level: (0..n)
                    .map(|_| {
                        (rng.below(4) != 0).then(|| (2 + rng.below(2) as u8, rng.below(3) as u64))
                    })
                    .collect(),
            });
            let tree = Tree::build(&Input {
                processors: &ps,
                srat: rng.below(2) == 0,
                pptt: pptt.as_ref(),
            });
            let levels = tree.levels();
            for l in levels {
                assert_eq!(l.partition().len(), n, "case {case}");
                assert!(n == 1 || l.partition().groups() < n, "case {case}");
                assert!(Kind::ALL.iter().any(|&k| l.is(k)), "case {case}");
                let _ = l.to_string();
            }
            for w in levels.windows(2) {
                assert!(w[0].partition().refines(w[1].partition()), "case {case}");
                assert_ne!(w[0].partition(), w[1].partition(), "case {case}");
            }
            // Invariant 1: two processors share a group only where every claim on each kind the
            // level names holds the other: CPUID's both ways, the PPTT's equal where it was used,
            // and the SRAT's node.
            let f = *tree.findings();
            let claim = |p: &Processor, kind: Kind| -> Option<u32> {
                let t = p.topology.as_ref()?;
                match kind {
                    Kind::Core => Some(t.layout.shift(Domain::Core)),
                    Kind::Die => Some(t.layout.shift(Domain::Die)),
                    Kind::Package => Some(t.layout.shift(Domain::Package)),
                    Kind::Cluster => t.caches.second_level().map(|c| u32::from(c.shift)),
                    Kind::LastCache => t.caches.last_level().map(|c| u32::from(c.shift)),
                    Kind::Node => None,
                }
            };
            let covers = |a: &Processor, b: &Processor, kind: Kind| {
                claim(a, kind)
                    .is_some_and(|s| u64::from(a.apic_id) >> s == u64::from(b.apic_id) >> s)
            };
            for l in levels {
                let members = l.partition().members();
                for kind in Kind::ALL.iter().copied().filter(|&k| l.is(k)) {
                    let cpuid_reports = ps.iter().any(|p| claim(p, kind).is_some());
                    let pptt_used = pptt.as_ref().filter(|_| f.pptt_levels[kind.index()] == 0);
                    for group in members.iter().filter(|g| g.len() > 1) {
                        for &i in group {
                            for &j in group {
                                let (a, b) = (&ps[i as usize], &ps[j as usize]);
                                if kind == Kind::Node {
                                    assert!(a.node.is_some() && a.node == b.node, "case {case}");
                                    continue;
                                }
                                if cpuid_reports {
                                    assert!(
                                        covers(a, b, kind) && covers(b, a, kind),
                                        "case {case}"
                                    );
                                }
                                if let Some(p) = pptt_used
                                    .filter(|_| matches!(kind, Kind::Cluster | Kind::LastCache))
                                {
                                    let at = |x: u32| match kind {
                                        Kind::Cluster => p.second_level[x as usize],
                                        _ => p.last_level[x as usize].map(|(_, inst)| inst),
                                    };
                                    let named = if kind == Kind::Cluster {
                                        p.second_level.iter().any(Option::is_some)
                                    } else {
                                        p.last_level.iter().any(Option::is_some)
                                    };
                                    if named {
                                        assert!(at(i).is_some() && at(i) == at(j), "case {case}");
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// A processor that did not start leaves a gap in the CPU indexes, and the tree names the
    /// others by their own (R97-1): the SMT machine without CPU 3.
    #[test]
    fn the_tree_names_processors_by_their_cpu_index() {
        let ps: Vec<Processor> = smt_machine(|_| 8)
            .into_iter()
            .filter(|p| p.cpu != 3)
            .collect();
        let tree = Tree::build(&Input {
            processors: &ps,
            srat: true,
            pptt: None,
        });
        assert_eq!(
            lines(&tree),
            [
                "core, L2 4 groups [0-1|2|4-5|6-7]",
                "node 2 groups [0-2|4-7]",
                "last-level cache, package 1x7 [0-2,4-7]"
            ]
        );
        assert_eq!(tree.cpus(), [0, 1, 2, 4, 5, 6, 7]);
        assert!(tree.findings().agree());
    }

    /// A PPTT whose last level is an L2 where CPUID's is an L3 names another cache, not another
    /// sharing: it is not used for that level, and each processor is counted (R97-3).
    #[test]
    fn a_pptt_describing_another_last_level_is_not_met() {
        let ps = smt_machine(|_| 8);
        let pptt = PpttClaims {
            second_level: (0..8).map(|i| Some(100 + i / 2)).collect(),
            last_level: (0..8).map(|i| Some((2, 100 + i / 2))).collect(),
        };
        let tree = Tree::build(&Input {
            processors: &ps,
            srat: true,
            pptt: Some(&pptt),
        });
        let f = tree.findings();
        assert_eq!(f.pptt_levels[Kind::LastCache.index()], 8);
        assert_eq!(f.pptt_differs[Kind::LastCache.index()], 0);
        assert!(!f.agree());
        // The last level is CPUID's alone: the package's.
        assert_eq!(lines(&tree)[2], "last-level cache, package 1x8 [0-7]");
    }

    /// R97-9 and R97-10: one APIC ID twice is two groups, not one; a claim wider than any
    /// processor it would add is not cut.
    #[test]
    fn duplicates_stand_apart_and_cuts_are_counted_by_membership() {
        let (partition, cut) = aligned(&[(5, None), (5, None)]);
        assert_eq!(partition.groups(), 2);
        assert_eq!(
            cut, 2,
            "each claims a block holding both, and has itself alone"
        );
        let (partition, cut) = aligned(&[(0, Some(2)), (1, Some(1))]);
        assert_eq!(partition.to_string(), "1x2 [0-1]");
        assert_eq!(cut, 0);
    }

    #[test]
    fn partitions_print_ranges_and_shapes() {
        assert_eq!(
            Partition::from_keys(&[0, 0, 1, 1, 1]).to_string(),
            "2 groups [0-1|2-4]"
        );
        assert_eq!(
            Partition::from_keys(&[0, 1, 0, 1]).to_string(),
            "2x2 [0,2|1,3]"
        );
        assert_eq!(Partition::discrete(3).to_string(), "3x1 [0|1|2]");
        assert!(Partition::discrete(3).refines(&Partition::from_keys(&[0, 0, 0])));
        assert!(!Partition::from_keys(&[0, 0, 1]).refines(&Partition::from_keys(&[0, 1, 1])));
    }

    #[test]
    fn nodes_are_ordered_by_distance() {
        let slit = |a: u32, b: u32| -> Option<u8> {
            let m = [[10, 21, 31], [21, 10, 15], [31, 15, 10]];
            m.get(a as usize)?.get(b as usize).copied()
        };
        assert_eq!(
            node_order(&[0, 1, 1, 2], slit),
            [(0, vec![1, 2]), (1, vec![2, 0]), (2, vec![1, 0])]
        );
        // A node beyond the SLIT comes last.
        assert_eq!(node_order(&[0, 7, 1], slit)[0], (0, vec![1, 7]));
    }
}
