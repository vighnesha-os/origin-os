//! What the processors publish to each other for load balancing (ADR-0020 S7; TASKS item 98): each
//! processor's own element, two masks of hints, and the two searches that read them.
//!
//! Balancing in version 1 is by pulls: a processor about to idle takes a thread from a processor
//! that has one waiting (the idle pull), and a processor that has one waiting tells an idle one
//! (the idle kick). Neither may wait for the other, so each reads what the other published,
//! with no lock, and then checks everything again under the run queues' locks. A stale read only
//! picks a worse candidate.
//!
//! # The element
//!
//! One [`Element`] for each processor, on a cache line of its own:
//!
//! - **`idle`**: written only by its own processor, with interrupts off. Its idle flow sets it
//!   before it looks for a thread to pull and halts ([`idle_then_choose`]); the processor clears
//!   it when it starts a thread ([`Element::leave_idle`]).
//! - **`load`**: written by whoever holds this processor's run-queue lock, at a census of the
//!   queue ([`Element::publish`]): how many contexts are runnable, how many threads balancing may
//!   take are ready and not running, and the weights' sum scaled by the processor's capacity.
//!
//! # The hints
//!
//! Two [`Hints`] ([`Masks`]), one bit a processor: who may be idle, and who may have a thread waiting
//! (crowded). They let a search skip every processor that has nothing to say with a few loads.
//! They are written lazily: a bit is set only if it is clear, and cleared only by its processor's
//! tick, when it is wrong ([`Element::settle`]). So a processor that idles and wakes many times a
//! tick writes a line the others share at most twice a tick. A set bit promises nothing: a search
//! confirms it in the element.
//!
//! # Neither misses the other
//!
//! A processor that goes idle while another becomes crowded must not halt beside a thread that
//! waits. Each side **publishes its own state first and reads the other's after**:
//!
//! - the idle side stores `idle` and then reads the crowded processors' `load`
//!   ([`idle_then_choose`]);
//! - the crowded side stores its `load` and then reads the idle processors' `idle`
//!   ([`publish_then_find_idle`]).
//!
//! Between its store and its reads each side makes a sequentially consistent fence, as the TLB
//! shootdown's two sides do (`shootdown.rs`). The two fences have an order. The side whose fence
//! comes second reads after it, and what the other side stored before the first fence is then
//! visible to it: at least one side sees the other. The hints do not weaken this: each side sets
//! its own hint before its fence too, if the bit was clear, and a bit that was set already is
//! there to be read. On x86-64 the fence is one `mfence`, on the idle flow's way to its halt and
//! on a crowded processor's way to a kick, never on a switch.
//!
//! The Loom models ("balance" in `models.rs`) check it in the memory model Rust's atomics follow,
//! with fresh hints and with stale ones, and with a control for each of the two fences and one
//! for the order of the idle side's two steps. Loom orders a fence as the language does; a
//! sequentially consistent store or load it treats as no stronger than acquire and release, which
//! is why the fence is written out.
//!
//! # Who calls what
//!
//! - [`idle_then_choose`]: a processor's idle flow, interrupts off, before it halts.
//! - [`Element::leave_idle`], [`Element::settle`]: the processor itself, in its scheduler's
//!   upcalls, interrupts off.
//! - [`Element::publish`], [`publish_then_find_idle`]: a holder of that processor's run-queue
//!   lock, interrupts off.
//! - [`Element::load`], [`Element::is_idle`], [`Hints::test`]: any processor.
#![forbid(unsafe_code)]

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release, SeqCst};

use crate::prim;

/// What a processor's run queue held at its last census, as it publishes it.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub struct Load {
    /// The contexts that are running or ready, the idle flow aside.
    pub runnable: u8,
    /// The threads balancing may take that are ready and not running.
    pub waiting: u8,
    /// The runnable contexts' weights, times 1024 over the processor's capacity (ADR-0020 S9).
    pub load: u32,
}

// Only its name: the kernel's no-panic check refuses formatting beyond `write_str` in this crate's
// release object (`exits.rs` says the same of its types).
#[cfg(not(test))]
impl core::fmt::Debug for Load {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Load")
    }
}

impl Load {
    /// One word: the load in bits 0 to 31, the runnable count in bits 32 to 39, the waiting count
    /// in bits 40 to 47.
    const fn pack(self) -> u64 {
        (self.load as u64) | ((self.runnable as u64) << 32) | ((self.waiting as u64) << 40)
    }

    const fn unpack(word: u64) -> Self {
        // Each `as` keeps the bits `pack` put there.
        Self {
            load: word as u32,
            runnable: (word >> 32) as u8,
            waiting: (word >> 40) as u8,
        }
    }

    /// Whether a thread could be taken from here and leave the processor something to run: two
    /// or more runnable contexts, one of them a thread balancing may take that waits (ADR-0020
    /// S7.1's "surplus of at least one thread").
    pub const fn has_surplus(self) -> bool {
        self.runnable >= 2 && self.waiting >= 1
    }
}

/// One processor's published state (the module documentation, "The element").
#[repr(align(64))]
pub struct Element {
    idle: prim::AtomicBool,
    load: prim::AtomicU64,
}

impl core::fmt::Debug for Element {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Element")
    }
}

impl Element {
    /// Not idle, nothing runnable.
    #[cfg(not(all(test, loom)))]
    pub const fn new() -> Self {
        Self {
            idle: prim::AtomicBool::new(false),
            load: prim::AtomicU64::new(0),
        }
    }

    /// Not idle, nothing runnable.
    #[cfg(all(test, loom))]
    pub fn new() -> Self {
        Self {
            idle: prim::AtomicBool::new(false),
            load: prim::AtomicU64::new(0),
        }
    }

    /// Whether the processor's idle flow has published that it is idle, and the processor has
    /// not started a thread since.
    pub fn is_idle(&self) -> bool {
        self.idle.load(Acquire)
    }

    /// The processor starts a thread: it is not idle. Its own processor only. With nothing to
    /// clear it costs one plain load; the store is a plain move too, since a reader that still
    /// sees `idle` only sends a kick the processor does not need.
    #[inline]
    pub fn leave_idle(&self) {
        if self.idle.load(Relaxed) {
            self.idle.store(false, Release);
        }
    }

    /// What the processor's queue held at its last census.
    pub fn load(&self) -> Load {
        Load::unpack(self.load.load(Acquire))
    }

    /// Publishes a census of processor `cpu`'s queue, and marks it crowded if a thread waits. A
    /// holder of that queue's lock only.
    pub fn publish(&self, cpu: usize, load: Load, crowded: &Hints) {
        self.load.store(load.pack(), Release);
        if load.waiting != 0 {
            crowded.set(cpu);
        }
    }

    /// Clears the hints of processor `cpu` that are wrong: `crowded` if its last census found no
    /// thread waiting, `idle` if it is not idle. Its own processor only, at its tick, holding its
    /// queue's lock: at most one write of each mask a tick.
    pub fn settle(&self, cpu: usize, masks: &Masks) {
        if !self.idle.load(Relaxed) {
            masks.idle.clear(cpu);
        }
        if Load::unpack(self.load.load(Relaxed)).waiting == 0 {
            masks.crowded.clear(cpu);
        }
    }
}

#[cfg(not(all(test, loom)))]
impl Default for Element {
    fn default() -> Self {
        Self::new()
    }
}

/// A mask of hints, one bit a processor (the module documentation, "The hints").
pub struct Hints {
    words: Box<[prim::AtomicU64]>,
}

impl core::fmt::Debug for Hints {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Hints")
    }
}

impl Hints {
    /// A mask for `processors` processors, every bit clear; `None` if the memory cannot be had.
    pub fn new(processors: usize) -> Option<Self> {
        let count = processors.div_ceil(64).max(1);
        let mut words = Vec::new();
        words.try_reserve_exact(count).ok()?;
        for _ in 0..count {
            words.push(prim::AtomicU64::new(0));
        }
        Some(Self {
            words: words.into_boxed_slice(),
        })
    }

    /// The word and the bit of processor `cpu`, if the mask holds it.
    fn at(&self, cpu: usize) -> Option<(&prim::AtomicU64, u64)> {
        Some((self.words.get(cpu / 64)?, 1 << (cpu % 64)))
    }

    /// Whether processor `cpu`'s bit is set.
    pub fn test(&self, cpu: usize) -> bool {
        self.at(cpu)
            .is_some_and(|(word, bit)| word.load(Acquire) & bit != 0)
    }

    /// Whether any bit is set: with none, a search has nothing to read.
    pub fn any(&self) -> bool {
        self.words.iter().any(|word| word.load(Acquire) != 0)
    }

    /// Sets processor `cpu`'s bit, writing only if it is clear.
    pub fn set(&self, cpu: usize) {
        if let Some((word, bit)) = self.at(cpu)
            && word.load(Acquire) & bit == 0
        {
            word.fetch_or(bit, AcqRel);
        }
    }

    /// Clears processor `cpu`'s bit, writing only if it is set.
    pub fn clear(&self, cpu: usize) {
        if let Some((word, bit)) = self.at(cpu)
            && word.load(Acquire) & bit != 0
        {
            word.fetch_and(!bit, AcqRel);
        }
    }
}

/// The machine's two masks of hints: who may be idle, and who may have a thread waiting.
pub struct Masks {
    /// The processors whose idle flow may be halted.
    pub idle: Hints,
    /// The processors that may have a thread balancing may take, ready and not running.
    pub crowded: Hints,
}

impl core::fmt::Debug for Masks {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Masks")
    }
}

impl Masks {
    /// Both masks for `processors` processors, every bit clear; `None` if the memory cannot be
    /// had.
    pub fn new(processors: usize) -> Option<Self> {
        Some(Self {
            idle: Hints::new(processors)?,
            crowded: Hints::new(processors)?,
        })
    }
}

/// A processor to pull from, as [`choose`] found it.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub struct Victim {
    /// Its CPU index.
    pub cpu: u32,
    /// The level it was found at, 0 the narrowest: an index into the caller's levels.
    pub level: usize,
}

#[cfg(not(test))]
impl core::fmt::Debug for Victim {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Victim")
    }
}

/// The processor an idle processor `me` should pull from (ADR-0020 S7.1): at the narrowest level
/// that has one, the processor with the highest load among those with a surplus
/// ([`Load::has_surplus`]).
///
/// `levels` are `me`'s groups, narrowest first, each the CPU indexes of a group that holds `me`
/// (its core, its cache, its package, its node, the machine), in ascending order. A wider level
/// repeats the narrower one's processors; they were looked at already and are looked at again,
/// which changes no answer. `allowed(cpu, level)` is the caller's own refusals (a processor that
/// takes no part in moves, one it has already tried in this pass).
///
/// A tie goes to the first processor after `me` in index order, going round, so that idle
/// processors with different indexes do not all pick one victim.
pub fn choose<'a>(
    me: u32,
    crowded: &Hints,
    levels: &[&[u32]],
    element: impl Fn(u32) -> Option<&'a Element>,
    allowed: impl Fn(u32, usize) -> bool,
) -> Option<Victim> {
    if !crowded.any() {
        return None;
    }
    for (level, members) in levels.iter().enumerate() {
        let mut best: Option<(u32, u32, u32)> = None;
        for &cpu in members.iter() {
            if cpu == me || !crowded.test(cpu as usize) || !allowed(cpu, level) {
                continue;
            }
            let Some(load) = element(cpu).map(Element::load) else {
                continue;
            };
            if !load.has_surplus() {
                continue;
            }
            // How far after `me` it is, going round: the tie's order.
            let after = cpu.wrapping_sub(me).wrapping_sub(1);
            let better = best.is_none_or(|(_, most, nearest)| {
                load.load > most || (load.load == most && after < nearest)
            });
            if better {
                best = Some((cpu, load.load, after));
            }
        }
        if let Some((cpu, _, _)) = best {
            return Some(Victim { cpu, level });
        }
    }
    None
}

/// The idle flow's step before it halts (the module documentation, "Neither misses the other"):
/// publishes that processor `me` is idle, **then** chooses a processor to pull from ([`choose`]).
/// `None`: nothing to pull, and from here on a processor that becomes crowded finds `me` idle and
/// kicks it.
pub fn idle_then_choose<'a>(
    me: u32,
    own: &Element,
    masks: &Masks,
    levels: &[&[u32]],
    element: impl Fn(u32) -> Option<&'a Element>,
    allowed: impl Fn(u32, usize) -> bool,
) -> Option<Victim> {
    own.idle.store(true, Release);
    masks.idle.set(me as usize);
    prim::fence(SeqCst);
    choose(me, &masks.crowded, levels, element, allowed)
}

/// The nearest idle processor to `me` (ADR-0020 S7.2): the first, narrowest level first, whose
/// `idle` hint is set and whose element confirms it, and that `allowed(cpu, level)` admits.
pub fn nearest_idle<'a>(
    me: u32,
    idle: &Hints,
    levels: &[&[u32]],
    element: impl Fn(u32) -> Option<&'a Element>,
    allowed: impl Fn(u32, usize) -> bool,
) -> Option<u32> {
    if !idle.any() {
        return None;
    }
    levels.iter().enumerate().find_map(|(level, members)| {
        members.iter().copied().find(|&cpu| {
            cpu != me
                && idle.test(cpu as usize)
                && element(cpu).is_some_and(Element::is_idle)
                && allowed(cpu, level)
        })
    })
}

/// A crowded processor's step (the module documentation, "Neither misses the other"): publishes
/// processor `me`'s census, **then** finds the nearest idle processor ([`nearest_idle`]) for the
/// caller to kick. A holder of `me`'s run-queue lock only.
pub fn publish_then_find_idle<'a>(
    me: u32,
    own: &Element,
    load: Load,
    masks: &Masks,
    levels: &[&[u32]],
    element: impl Fn(u32) -> Option<&'a Element>,
    allowed: impl Fn(u32, usize) -> bool,
) -> Option<u32> {
    own.publish(me as usize, load, &masks.crowded);
    prim::fence(SeqCst);
    nearest_idle(me, &masks.idle, levels, element, allowed)
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    fn loaded(elements: &[Element], crowded: &Hints, loads: &[(u8, u8, u32)]) {
        for (cpu, &(runnable, waiting, load)) in loads.iter().enumerate() {
            elements[cpu].publish(
                cpu,
                Load {
                    runnable,
                    waiting,
                    load,
                },
                crowded,
            );
        }
    }

    fn machine(n: usize) -> (Vec<Element>, Masks) {
        (
            (0..n).map(|_| Element::new()).collect(),
            Masks::new(n).unwrap(),
        )
    }

    #[test]
    fn a_load_packs_and_unpacks_whole() {
        for load in [
            Load::default(),
            Load {
                runnable: 16,
                waiting: 15,
                load: u32::MAX,
            },
            Load {
                runnable: 2,
                waiting: 1,
                load: 16 * 1024,
            },
        ] {
            assert_eq!(Load::unpack(load.pack()), load);
        }
        // One runnable context is never a surplus, and neither are two with none to take.
        let surplus = |runnable, waiting| {
            Load {
                runnable,
                waiting,
                load: 8,
            }
            .has_surplus()
        };
        assert!(surplus(2, 1) && surplus(16, 15));
        assert!(!surplus(1, 1) && !surplus(2, 0) && !surplus(0, 0));
    }

    #[test]
    fn hints_are_set_and_cleared_a_bit_a_processor_past_one_word() {
        let hints = Hints::new(130).unwrap();
        assert!(!hints.any());
        for cpu in [0, 63, 64, 129] {
            hints.set(cpu);
            assert!(hints.test(cpu));
        }
        assert!(hints.any() && !hints.test(1) && !hints.test(65));
        // Past the mask: no bit, and nothing written.
        hints.set(4096);
        assert!(!hints.test(4096));
        for cpu in [0, 63, 64, 129] {
            hints.clear(cpu);
        }
        assert!(!hints.any());
        // A mask for no processor still holds a word.
        assert!(!Hints::new(0).unwrap().any());
    }

    /// ADR-0020 S7.1: the narrowest level first; within it, the highest load with a surplus.
    #[test]
    fn the_victim_is_the_highest_load_at_the_narrowest_level_that_has_one() {
        let (elements, masks) = machine(8);
        let crowded = &masks.crowded;
        let element = |cpu: u32| elements.get(cpu as usize);
        let all: Vec<u32> = (0..8).collect();
        // Processor 1's core is {0, 1}, its cache {0..4}, the machine {0..8}.
        let levels: [&[u32]; 3] = [&[0, 1], &[0, 1, 2, 3], &all];
        let any = |_: u32, _: usize| true;
        assert_eq!(choose(1, crowded, &levels, element, any), None);
        // A heavier processor farther away loses to a lighter one nearer.
        loaded(
            &elements,
            crowded,
            &[
                (1, 0, 8),
                (0, 0, 0),
                (2, 1, 16),
                (3, 2, 24),
                (0, 0, 0),
                (9, 8, 72),
                (0, 0, 0),
                (0, 0, 0),
            ],
        );
        assert_eq!(
            choose(1, crowded, &levels, element, any),
            Some(Victim { cpu: 3, level: 1 })
        );
        // Its own core's sibling, once it has a surplus, wins over both.
        loaded(&elements, crowded, &[(2, 1, 16)]);
        assert_eq!(
            choose(1, crowded, &levels, element, any),
            Some(Victim { cpu: 0, level: 0 })
        );
        // What the caller refuses is passed over, at every level it refuses it.
        assert_eq!(
            choose(1, crowded, &levels, element, |cpu, _| cpu != 0 && cpu != 3),
            Some(Victim { cpu: 2, level: 1 })
        );
        assert_eq!(
            choose(1, crowded, &levels, element, |cpu, _| cpu == 5),
            Some(Victim { cpu: 5, level: 2 })
        );
        // Itself, never.
        loaded(&elements, crowded, &[(1, 0, 8), (9, 8, 72)]);
        assert_eq!(
            choose(1, crowded, &[&[0, 1]], element, any),
            None,
            "only itself is crowded there"
        );
    }

    /// A tie goes to the first processor after the chooser, going round.
    #[test]
    fn a_tie_goes_to_the_next_index_after_the_chooser() {
        let (elements, masks) = machine(6);
        let crowded = &masks.crowded;
        let element = |cpu: u32| elements.get(cpu as usize);
        let all: Vec<u32> = (0..6).collect();
        loaded(
            &elements,
            crowded,
            &[
                (0, 0, 0),
                (2, 1, 16),
                (0, 0, 0),
                (0, 0, 0),
                (2, 1, 16),
                (0, 0, 0),
            ],
        );
        let pick = |me| choose(me, crowded, &[&all], element, |_, _| true).map(|v| v.cpu);
        assert_eq!(pick(0), Some(1));
        assert_eq!(pick(2), Some(4));
        assert_eq!(pick(3), Some(4));
        assert_eq!(pick(5), Some(1), "going round");
    }

    /// A stale `crowded` hint, or one whose processor has no surplus any more, names no victim;
    /// a processor whose hint is clear is never read.
    #[test]
    fn a_hint_alone_names_no_victim() {
        let (elements, masks) = machine(4);
        let crowded = &masks.crowded;
        let all: Vec<u32> = (0..4).collect();
        loaded(&elements, crowded, &[(0, 0, 0), (3, 2, 24)]);
        // The puller found nothing and published what it saw: the hint stays, the word says no.
        loaded(&elements, crowded, &[(0, 0, 0), (3, 0, 24)]);
        assert!(crowded.test(1));
        let element = |cpu: u32| elements.get(cpu as usize);
        assert_eq!(choose(0, crowded, &[&all], element, |_, _| true), None);
        // Its tick clears the hint.
        elements[1].settle(1, &masks);
        assert!(!crowded.any());
        // With the hint clear, a surplus in the word is not looked at.
        elements[2].load.store(
            Load {
                runnable: 3,
                waiting: 2,
                load: 24,
            }
            .pack(),
            Release,
        );
        crowded.set(3);
        assert_eq!(choose(0, crowded, &[&all], element, |_, _| true), None);
    }

    /// ADR-0020 S7.2: the nearest idle processor, confirmed in its element.
    #[test]
    fn the_nearest_idle_processor_is_confirmed_in_its_element() {
        let (elements, masks) = machine(8);
        let (idle, crowded) = (&masks.idle, &masks.crowded);
        let element = |cpu: u32| elements.get(cpu as usize);
        let all: Vec<u32> = (0..8).collect();
        let levels: [&[u32]; 3] = [&[2, 3], &[0, 1, 2, 3], &all];
        let any = |_: u32, _: usize| true;
        assert_eq!(nearest_idle(2, idle, &levels, element, any), None);
        // Processors 6 and 1 go idle, each by its idle flow's step.
        for me in [6, 1] {
            let near: [&[u32]; 1] = [&all];
            let chosen = idle_then_choose(me, &elements[me as usize], &masks, &near, element, any);
            assert_eq!(chosen, None);
        }
        assert_eq!(nearest_idle(2, idle, &levels, element, any), Some(1));
        assert_eq!(
            nearest_idle(2, idle, &levels, element, |cpu, _| cpu != 1),
            Some(6)
        );
        // Processor 1 starts a thread: its hint is stale until its tick, and is not believed.
        elements[1].leave_idle();
        assert!(idle.test(1) && !elements[1].is_idle());
        assert_eq!(nearest_idle(2, idle, &levels, element, any), Some(6));
        elements[1].settle(1, &masks);
        assert!(!idle.test(1));
        // An idle processor's own tick leaves its hint.
        elements[6].settle(6, &masks);
        assert!(idle.test(6));
        // The crowded side's step publishes, then finds it.
        let found = publish_then_find_idle(
            2,
            &elements[2],
            Load {
                runnable: 2,
                waiting: 1,
                load: 16,
            },
            &masks,
            &levels,
            element,
            any,
        );
        assert_eq!(found, Some(6));
        assert!(crowded.test(2) && elements[2].load().has_surplus());
    }
}
