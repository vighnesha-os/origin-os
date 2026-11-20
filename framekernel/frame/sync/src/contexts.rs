//! One processor's contexts: which kernel stacks hold a thread that may still run there, and
//! which frames the trap entry may resume (TASKS item 70; ADR-0011 N6).
//!
//! The framework gives a kernel stack back only if nothing may still use it (`origin_frame::
//! stack`, "Giving a stack back"). Each processor keeps a [`Contexts`] table, which
//! `origin_frame::cpu` instantiates. One slot of it is one context: a thread published to that
//! processor, or the processor's boot or idle flow. Each slot has two words:
//!
//! - **`home`**: an address inside the context's own stack (its first frame's), from the moment
//!   that frame is published ([`Contexts::publish`]) until the context has ended and its processor
//!   has left the stack ([`Contexts::retire_ended`]), or until its publisher withdraws it, never
//!   having made it runnable ([`Contexts::withdraw`]). For the boot or idle flow, which has no
//!   stack of its own in the region, it is [`BOOT_FLOW`], an address no stack contains. 0 is a
//!   free slot.
//! - **`frame`**: the frame the trap entry may resume, while the context is suspended (a published
//!   context waits for its first run suspended at its first frame), and 0 while it runs, after it
//!   ended, and while a withdrawal is under way.
//!
//! # What covers a stack, and why no switch uncovers one
//!
//! A switch ([`Contexts::switch`]) writes only `frame` words and the processor's `current` slot:
//! it never writes a `home`. So a context's own stack is covered by its `home` for its whole life,
//! whether it runs, is suspended, or is switched in or out. A reader on any processor
//! ([`Contexts::covers`]) can therefore read the words in any order, interleaved with any number
//! of switches, and still find every live context's stack. Two separate records, one for a
//! running thread and one for a suspended one, would not allow that. A switch-out hands a stack
//! from the first to the second and a switch-in hands one back, so a reader that reads the two in
//! a fixed order misses one direction. This was T68-2, in the review of TASKS item 68. The `frame`
//! word still covers a stack a context is suspended on that is not its home: a measured stack
//! (`origin_frame::stack::with_measured_stack`), or the boot flow's stack.
//!
//! # The orderings
//!
//! - A slot is claimed by a compare-and-swap of `home` from 0 (AcqRel), and its `frame` is
//!   stored with Release. Readers load both with Acquire.
//! - A `frame` word becomes 0 in two ways, each a compare-and-swap of the word from the frame
//!   it holds: this processor's `switch` takes it to resume it, or the context's publisher, on
//!   any processor, takes its first frame to withdraw a context it never made runnable
//!   ([`Contexts::withdraw`]). Both are compare-and-swaps of the same word, so at most one of them
//!   takes a given frame: a context withdrawn is never resumed, and one resumed is never
//!   withdrawn. Nothing else writes a nonzero frame to 0. A frame is written into a slot whose
//!   `frame` is 0 only by a claimer that has just taken it while free, or by this processor's
//!   switch-out of the context it runs (`current`); a context being withdrawn never ran, so it
//!   was never `current`, and no switch-out writes its slot (the review of TASKS items 75, 77
//!   and 81, R75-3).
//! - A home leaves the table by a Release store of 0, in one of two ways:
//!   - **retired** on the context's own processor, only after that processor has left the stack
//!     ([`Contexts::retire_ended`]);
//!   - **withdrawn** by its publisher, from any processor, only after the publisher's
//!     compare-and-swap took the first frame, so that no processor has run or ever will run on
//!     the stack as this context.
//!
//!   A reader that does not find the home has read that store or a later value. So everything
//!   the processor did on the stack, or the publisher did before withdrawing, happens before
//!   whatever the reader does next, such as giving the stack back. A claimer that takes the slot
//!   afterwards reads the store with its compare-and-swap, so the withdrawn context's words are
//!   behind it and cannot overwrite the claim.
//! - A home is claimed before anything can run on its stack: only a published frame is ever
//!   resumed. The publisher also holds the stack exclusively when it publishes, and until it
//!   withdraws, so any later holder's drop happens after the claim, and after the withdrawal.
//!
//! **Between a withdrawal's two stores** the slot has a home and a `frame` of 0, as a running
//! context's slot has. Nothing infers "running" from that pair: which context runs is `current`,
//! and which ended is `ended`, both slot indexes written only by this processor's `switch` (and
//! `ended` cleared by its retirement), and a withdrawn context is never either, since it never
//! ran. Meanwhile [`Contexts::covers`] still finds the home, so the stack stays covered until the
//! publisher's own drop; no claim can take the slot, whose home is not 0; and no `switch` can
//! match it, since a `switch` never looks for a frame of 0.
//!
//! On x86-64, Acquire loads and Release stores are plain moves, and the compare-and-swap is the
//! `lock cmpxchg` the table needed before. The Loom models (`models.rs`, "contexts") check these
//! orderings in the memory model Rust's atomics follow, with negative controls that break each.
//!
//! # Who calls what
//!
//! - [`Contexts::publish`]: any processor, for a thread it is placing on this one.
//! - [`Contexts::withdraw`]: the publisher, on any processor, for a context it published and
//!   never made runnable (a spawn that its run queue then refused; TASKS item 75).
//! - [`Contexts::switch`] and [`Contexts::retire_ended`]: this processor only, and **with
//!   interrupts off**. `switch` runs from the trap entry; a stack's drop, which may run with
//!   interrupts on, wraps its retirement in `cpu::without_interrupts` (the review of TASKS item
//!   70, T70-1, answering the coordinating session's ruling (c)). The condition matters: a
//!   retirement interrupted between its take of `ended` and its clear of the home leaves the
//!   ended context in no record that will retire it, so a reap in that gap finds its stack still
//!   covered and leaks it (safe, but lost), and the table could then hold more ended contexts
//!   than its capacity counts. The host test
//!   `a_retirement_interrupted_between_its_take_and_its_clear_leaves_the_stack_covered` shows
//!   that gap on this code.
//! - `ended` is taken by a swap, and the exit's record is written by a swap too. On one processor
//!   with interrupts off nothing interleaves with either, and C++20 would let the record be a
//!   plain Release store: a read-modify-write reads the last value in the modification order
//!   ([atomics.order]), and a value sequenced after a store reads that store or a later one
//!   ([intro.races]), so a plain store cannot be missed by the swaps after it. Loom 0.7.2 found an
//!   execution of model L5 that breaks both rules, a false alarm (the review of TASKS item 70,
//!   T70-3, which reproduced it and showed no C++20 execution matches it). The swap is kept for
//!   Loom's sake, at one `xchg` per thread that ends. A Loom failure on a location written by both
//!   plain stores and read-modify-writes gets that C++20 check before the code changes for it.
//! - A retirement with nothing ended costs one plain load (T70-2), so a tick that switches
//!   nothing executes no locked instruction.
//! - [`Contexts::covers`]: any processor. It never waits and takes no lock, so a stack may still
//!   be dropped under any lock.
//!
//! # The stack a processor names for ring 3 (TASKS item 77)
//!
//! A switch answers the incoming context's home with the frame to resume ([`Switched`]), and the
//! trap entry names the stack that home is in as the one an interrupt or a system call from ring
//! 3 lands on (`origin_frame::cpu`, the processor's kernel-stack record and TSS.RSP0). For the
//! boot or idle flow the home is [`BOOT_FLOW`], in no stack, which never runs in ring 3. So the
//! stack named is always the running context's own, whatever the scheduler above answered: the
//! table, not the scheduler, says whose frame was resumed. [`Contexts::running_home`] reads the
//! same home back for the entry's check that an entry from ring 3 landed on it.
//!
//! # A context never moves between processors
//!
//! A frame is resumed only by the processor whose table holds it, and the frames a context saves
//! later are recorded in that same table, so a context stays on the processor it was published
//! to. Nothing moves a thread today. A future move (CLAUDE.md §26, load balancing) must claim the
//! context's home in the new processor's table before it retires the home in the old one, so that
//! the stack is never uncovered, and it must be shown under a Loom model of its own. A move that
//! retires first, or that moves only the frame, reopens the gap this table closes.
//!
//! # Capacity
//!
//! [`SLOTS`] is 32. A processor holds at most one context per run-queue slot, the boot or idle
//! flow's included: the kernel's run queue has 16 (`runqueue::CAPACITY`, which the kernel checks
//! against [`SLOTS`] at compile time). It also holds at most one context that has ended and is not
//! yet retired, since each `switch` retires the previous one before an exit records another, and
//! every retirement runs with interrupts off, so none is caught half-done (T70-1). That
//! is 17 of 32. If a table were ever full, [`Contexts::publish`] returns `false`, and the kernel
//! refuses the spawn. A boot flow's first switch-out that found no slot would be refused by
//! [`Contexts::switch`] with [`Refused::Full`], and the trap entry halts.
//!
//! **Spawns in flight** (TASKS item 75; the review of TASKS items 75, 77 and 81, R75-2). A spawn
//! publishes before it queues, so the table also holds, for a moment, every spawn to this
//! processor between its publication and its push or withdrawal: one per spawner in that
//! stretch, which runs with interrupts on and may be preempted there. They share the 15 slots to
//! spare. More at once would have a publication refused, which only fails that spawn, and would
//! halt a processor whose boot or idle flow had not yet made its first switch-out. Today's kernel
//! reaches neither: every processor's boot or idle flow first switches out in the boot's first
//! scheduler proofs, to threads the boot flow spawns one at a time, before any thread can spawn
//! another. Claiming the boot or idle flow's slot when the table is made, rather than at its first
//! switch-out, would remove the halt; that is a follow-up.
#![forbid(unsafe_code)]

use core::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};

use crate::prim;

/// The contexts one processor's table holds; see the module documentation's "Capacity".
#[cfg(not(all(test, loom)))]
pub const SLOTS: usize = 32;
/// The Loom models' table: four slots, enough for the contexts each model makes. Nothing in the
/// code depends on the count, and four keeps the search exhaustive.
#[cfg(all(test, loom))]
pub const SLOTS: usize = 4;

/// The `home` of a processor's boot or idle flow: an address no kernel stack contains, since
/// the stack region is in the kernel's half of the address space.
pub const BOOT_FLOW: u64 = 1;

/// No slot: no context recorded as running (`current`) or as ended (`ended`).
const NONE: usize = usize::MAX;

/// Why [`Contexts::switch`] or [`Contexts::retire_ended`] refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The scheduler named a frame this processor never saved, or 0. The trap entry halts
    /// rather than load it.
    NeverSaved,
    /// The boot or idle flow's first switch-out found no free slot for its frame.
    Full,
    /// A context's home would have been retired while this processor still ran on its stack.
    StillOnIt,
}

/// One context: its own stack (`home`) and the frame to resume it from (`frame`).
struct Context {
    home: prim::AtomicU64,
    frame: prim::AtomicU64,
}

impl Context {
    #[cfg(not(all(test, loom)))]
    const fn new() -> Self {
        Self {
            home: prim::AtomicU64::new(0),
            frame: prim::AtomicU64::new(0),
        }
    }

    #[cfg(all(test, loom))]
    fn new() -> Self {
        Self {
            home: prim::AtomicU64::new(0),
            frame: prim::AtomicU64::new(0),
        }
    }

    /// Claims this slot, if free, for a context whose home is `home` and whose first frame to
    /// resume is `frame`.
    fn claim(&self, home: u64, frame: u64) -> bool {
        if self
            .home
            .compare_exchange(0, home, AcqRel, Acquire)
            .is_err()
        {
            return false;
        }
        self.frame.store(frame, Release);
        true
    }
}

/// One processor's contexts; see the module documentation. Its `Debug` writes its name alone:
/// its words are addresses in the kernel stacks' region, which nothing prints (ADR-0004
/// Amendment 2, A6), and this crate's release object may format nothing more (ONBF §12, the
/// no-panic check).
pub struct Contexts {
    slots: [Context; SLOTS],
    /// The slot of the context this processor runs, or `NONE` before its boot or idle flow's
    /// first switch-out. Written only by this processor, with interrupts off.
    current: prim::AtomicUsize,
    /// The slot of a context that ended and whose home is not yet retired, or `NONE`.
    ended: prim::AtomicUsize,
}

impl core::fmt::Debug for Contexts {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Contexts")
    }
}

/// What [`Contexts::switch`] answers: the frame to resume, and the home of the context that now
/// runs if the switch changed it (TASKS item 77). Its `Debug` writes its name alone, as
/// [`Contexts`]' does: its words are addresses in the kernel stacks' region.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Switched {
    /// The frame the trap entry resumes: `next`, which the table held, or `here` if nothing
    /// switched.
    pub frame: u64,
    /// The incoming context's home, which its stack holds ([`BOOT_FLOW`] for the boot or idle
    /// flow), or `None` if nothing switched and the running context stays.
    pub home: Option<u64>,
}

impl core::fmt::Debug for Switched {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Switched")
    }
}

impl Contexts {
    /// A table with no context.
    #[cfg(not(all(test, loom)))]
    pub const fn new() -> Self {
        Self {
            slots: [const { Context::new() }; SLOTS],
            current: prim::AtomicUsize::new(NONE),
            ended: prim::AtomicUsize::new(NONE),
        }
    }

    /// A table with no context.
    #[cfg(all(test, loom))]
    pub fn new() -> Self {
        Self {
            slots: core::array::from_fn(|_| Context::new()),
            current: prim::AtomicUsize::new(NONE),
            ended: prim::AtomicUsize::new(NONE),
        }
    }

    /// Records a new context whose first frame, at `frame`, is on its own stack: that frame
    /// becomes its home and the frame to resume. Any processor may call it, for a thread it places
    /// on this one, before the thread can be picked. `false` if `frame` cannot be one (0 or
    /// [`BOOT_FLOW`]) or the table is full.
    pub fn publish(&self, frame: u64) -> bool {
        frame > BOOT_FLOW && self.slots.iter().any(|slot| slot.claim(frame, frame))
    }

    /// Withdraws a context published at `frame` and never resumed: its slot is free again, and
    /// its stack no longer covered. Only its publisher calls it, from any processor, for a context
    /// it never made runnable (TASKS item 75).
    ///
    /// The first frame is taken by a compare-and-swap first, the same take a `switch` makes to
    /// resume it, so that at most one of the two succeeds; only then is the home cleared, with
    /// Release (the module documentation, "The orderings"). `false` if no slot holds `frame` as
    /// both its home and its frame to resume: a frame never published, one already withdrawn, or
    /// one a `switch` took first. Any home then stays, and the stack it covers is leaked rather
    /// than freed under a context that may run on it. It is for a context never made runnable,
    /// the only kind its publisher withdraws. A context that ran is not taken by mistake either:
    /// its saved frames are never at its first frame's address. In 64-bit mode the processor
    /// aligns the stack to 16 bytes before it pushes an interrupt's frame (Intel SDM Vol. 3A,
    /// order 325384-093US, §7.14.2), and the kernel builds a system call's frame from the stack's
    /// top likewise, while a first frame is built 8 bytes below such a place
    /// (`origin_frame::stack::GuardedStack::build_first_frame`; the review of TASKS items 75, 77
    /// and 81).
    pub fn withdraw(&self, frame: u64) -> bool {
        if frame <= BOOT_FLOW {
            return false;
        }
        let Some(slot) = self.slots.iter().find(|slot| {
            slot.home.load(Acquire) == frame
                && slot
                    .frame
                    .compare_exchange(frame, 0, AcqRel, Acquire)
                    .is_ok()
        }) else {
            return false;
        };
        slot.home.store(0, Release);
        true
    }

    /// What the trap entry resumes after a scheduler upcall, which the scheduler answered with
    /// `next` for the context interrupted at `here`. This processor only, interrupts off.
    ///
    /// First it retires a context that ended earlier ([`Self::retire_ended`]). Unless `next`
    /// is `here`, `next` must be a frame saved in this table. It is taken, so it cannot be resumed
    /// twice, and its context becomes the running one. The context that was running keeps its
    /// home and saves `here` as its frame, or, if `exiting`, is recorded as ended and saves
    /// nothing. The boot or idle flow's first switch-out claims a slot for it. No `home` is
    /// written. The answer carries the incoming context's home, read from the slot whose frame
    /// was taken, which the trap entry names as the stack ring 3 lands on (TASKS item 77); with
    /// nothing switched it carries none.
    ///
    /// `running_on(address)` says whether this processor runs on the stack holding `address`.
    pub fn switch(
        &self,
        here: u64,
        next: u64,
        exiting: bool,
        running_on: impl Fn(u64) -> bool,
    ) -> Result<Switched, Refused> {
        self.retire_ended(running_on)?;
        if next == here {
            return Ok(Switched {
                frame: here,
                home: None,
            });
        }
        if next <= BOOT_FLOW {
            return Err(Refused::NeverSaved);
        }
        // The take is a compare-and-swap: between the load and the swap a publisher on another
        // processor may withdraw the same frame (a context it never made runnable, so the
        // scheduler had no business naming it), which takes the word by the same swap. Then this
        // one fails and the search goes on, to `NeverSaved` if nothing else holds `next`. A frame
        // read equal to `next` is otherwise still `next` at the swap: only this processor's
        // switches and a withdrawal clear one, and a publisher writes one only into a slot it has
        // just claimed.
        let incoming = self
            .slots
            .iter()
            .position(|slot| {
                slot.frame.load(Relaxed) == next
                    && slot
                        .frame
                        .compare_exchange(next, 0, AcqRel, Acquire)
                        .is_ok()
            })
            .ok_or(Refused::NeverSaved)?;
        let outgoing = self.current.load(Relaxed);
        match self.slots.get(outgoing) {
            Some(_) if exiting => {
                let _ = self.ended.swap(outgoing, AcqRel);
            }
            Some(slot) => slot.frame.store(here, Release),
            // The boot or idle flow, switched out for the first time; it never ends.
            None if exiting => {}
            None => {
                if !self.slots.iter().any(|slot| slot.claim(BOOT_FLOW, here)) {
                    return Err(Refused::Full);
                }
            }
        }
        self.current.store(incoming, Relaxed);
        // The slot whose frame this switch took: its home is the incoming context's (its claim,
        // AcqRel, came before the Release store of the frame the take read with Acquire).
        let home = self.slots.get(incoming).map(|slot| slot.home.load(Acquire));
        Ok(Switched { frame: next, home })
    }

    /// The home of the context this processor runs: what [`Self::switch`] last answered, read
    /// back from the slot `current` names, or `None` before the boot or idle flow's first
    /// switch-out. This processor only, with interrupts off, as the trap entry runs: `current` is
    /// written only by its switches, and the running context's home only by its publication. The
    /// trap entry's check that an entry from ring 3 landed on the running context's own stack
    /// reads it, and never the record it wrote from the switch's answer (TASKS item 77).
    pub fn running_home(&self) -> Option<u64> {
        self.slots
            .get(self.current.load(Relaxed))
            .map(|slot| slot.home.load(Acquire))
    }

    /// Retires the home of a context that ended, if any: this processor only, after it has left
    /// that context's stack. That holds at every call: the exit's own `switch` records the
    /// context as ended, and the trap entry then loads the next frame with nothing in between
    /// that calls this, so every later call, from [`Self::switch`] or from a caller with
    /// interrupts off on this processor, runs on another context's stack. `running_on` checks it
    /// all the same.
    ///
    /// Call it with interrupts off (the module documentation, "Who calls what"). A refused
    /// retirement puts its record back only if no newer exit has recorded itself meanwhile
    /// (T70-6); its callers halt on the refusal in any case.
    pub fn retire_ended(&self, running_on: impl Fn(u64) -> bool) -> Result<(), Refused> {
        // Only this processor writes `ended`, so a plain load sees its own last write: with
        // nothing ended, nothing is written and no locked instruction runs (T70-2).
        if self.ended.load(Relaxed) == NONE {
            return Ok(());
        }
        let ended = self.ended.swap(NONE, AcqRel);
        let Some(slot) = self.slots.get(ended) else {
            return Ok(());
        };
        if running_on(slot.home.load(Relaxed)) {
            let _ = self.ended.compare_exchange(NONE, ended, AcqRel, Acquire);
            return Err(Refused::StillOnIt);
        }
        slot.home.store(0, Release);
        Ok(())
    }

    /// Whether any context's home or saved frame lies in `[low, top)`: a stack a context of this
    /// processor may still use. Any processor may ask. It never waits.
    pub fn covers(&self, low: u64, top: u64) -> bool {
        let inside = |at: u64| at >= low && at < top;
        self.slots
            .iter()
            .any(|slot| inside(slot.home.load(Acquire)) || inside(slot.frame.load(Acquire)))
    }
}

impl Default for Contexts {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    /// Stacks of 16 KiB at `0x1_0000` apart, each with its first frame near its top.
    fn stack(n: u64) -> (u64, u64) {
        let low = 0x10_0000 + n * 0x1_0000;
        (low, low + 0x4000)
    }

    fn first_frame(n: u64) -> u64 {
        stack(n).1 - 0xb0
    }

    fn not_on_it(_: u64) -> bool {
        false
    }

    /// The frame a switch resumes, which is what these tests compare.
    fn switch_to(
        table: &Contexts,
        here: u64,
        next: u64,
        exiting: bool,
        running_on: impl Fn(u64) -> bool,
    ) -> Result<u64, Refused> {
        table
            .switch(here, next, exiting, running_on)
            .map(|switched| switched.frame)
    }

    fn covered(table: &Contexts, n: u64) -> bool {
        let (low, top) = stack(n);
        table.covers(low, top)
    }

    #[test]
    fn a_published_context_is_covered_while_it_runs_and_after() {
        let table = Contexts::new();
        assert!(table.publish(first_frame(1)));
        assert!(covered(&table, 1));
        // The boot flow switches to it: its frame is taken, its home stays.
        let boot = 0x9000;
        assert_eq!(
            switch_to(&table, boot, first_frame(1), false, not_on_it),
            Ok(first_frame(1))
        );
        assert!(covered(&table, 1), "running, by its home");
        // It is switched out at a frame lower on its stack, and the boot flow resumed.
        let here = first_frame(1) - 0x200;
        assert_eq!(switch_to(&table, here, boot, false, not_on_it), Ok(boot));
        assert!(covered(&table, 1), "suspended");
        // Resumed again: the frame it saved is the one the scheduler names.
        assert_eq!(switch_to(&table, boot, here, false, not_on_it), Ok(here));
        assert!(covered(&table, 1));
    }

    #[test]
    fn only_saved_frames_are_resumed_and_never_twice() {
        let table = Contexts::new();
        assert!(table.publish(first_frame(1)));
        assert_eq!(
            switch_to(&table, 0x9000, 0, false, not_on_it),
            Err(Refused::NeverSaved),
            "0 matches no slot, free ones included"
        );
        assert_eq!(
            switch_to(&table, 0x9000, BOOT_FLOW, false, not_on_it),
            Err(Refused::NeverSaved)
        );
        assert_eq!(
            switch_to(&table, 0x9000, first_frame(2), false, not_on_it),
            Err(Refused::NeverSaved)
        );
        assert_eq!(
            switch_to(&table, 0x9000, first_frame(1), false, not_on_it),
            Ok(first_frame(1))
        );
        // Taken: a second resume of the same frame is refused.
        assert_eq!(
            switch_to(
                &table,
                first_frame(1) - 0x100,
                first_frame(1),
                false,
                not_on_it
            ),
            Err(Refused::NeverSaved)
        );
        // The same frame as `here` is no switch.
        assert_eq!(
            switch_to(&table, 0x7000, 0x7000, false, not_on_it),
            Ok(0x7000)
        );
    }

    #[test]
    fn an_ended_context_stays_covered_until_its_processor_retires_it() {
        let table = Contexts::new();
        assert!(table.publish(first_frame(1)));
        assert!(table.publish(first_frame(2)));
        let boot = 0x9000;
        assert_eq!(
            switch_to(&table, boot, first_frame(1), false, not_on_it),
            Ok(first_frame(1))
        );
        // Context 1 ends, and the processor switches to context 2.
        let at_exit = first_frame(1) - 0x300;
        assert_eq!(
            switch_to(&table, at_exit, first_frame(2), true, not_on_it),
            Ok(first_frame(2))
        );
        assert!(covered(&table, 1), "ended, not yet retired");
        assert!(
            !table.covers(at_exit, at_exit + 1),
            "its exit frame is never saved"
        );
        // Retired by a call on another stack: the home goes.
        assert_eq!(table.retire_ended(not_on_it), Ok(()));
        assert!(!covered(&table, 1));
        assert!(covered(&table, 2), "the running one keeps its home");
        // Nothing is left to retire.
        assert_eq!(table.retire_ended(|_| true), Ok(()));
    }

    #[test]
    fn a_retirement_on_the_ended_stack_is_refused_and_kept() {
        let table = Contexts::new();
        assert!(table.publish(first_frame(1)));
        assert!(table.publish(first_frame(2)));
        assert_eq!(
            switch_to(&table, 0x9000, first_frame(1), false, not_on_it),
            Ok(first_frame(1))
        );
        assert_eq!(
            switch_to(&table, first_frame(1) - 8, first_frame(2), true, not_on_it),
            Ok(first_frame(2))
        );
        let (low, top) = stack(1);
        let on_it = |at: u64| at >= low && at < top;
        assert_eq!(table.retire_ended(on_it), Err(Refused::StillOnIt));
        assert!(covered(&table, 1), "still covered");
        // The next switch, made from another stack, retires it.
        assert_eq!(
            switch_to(
                &table,
                first_frame(2) - 8,
                first_frame(2) - 8,
                false,
                not_on_it
            ),
            Ok(first_frame(2) - 8)
        );
        assert!(!covered(&table, 1));
    }

    #[test]
    fn the_next_switch_retires_an_ended_context_before_recording_another() {
        let table = Contexts::new();
        for n in 1..=3 {
            assert!(table.publish(first_frame(n)));
        }
        assert_eq!(
            switch_to(&table, 0x9000, first_frame(1), false, not_on_it),
            Ok(first_frame(1))
        );
        assert_eq!(
            switch_to(&table, 0x100, first_frame(2), true, not_on_it),
            Ok(first_frame(2))
        );
        assert_eq!(
            switch_to(&table, 0x200, first_frame(3), true, not_on_it),
            Ok(first_frame(3))
        );
        assert!(!covered(&table, 1), "retired by the second exit's switch");
        assert!(covered(&table, 2), "ended, waiting for the next");
        assert_eq!(table.retire_ended(not_on_it), Ok(()));
        assert!(!covered(&table, 2));
    }

    #[test]
    fn the_boot_flow_claims_a_slot_that_covers_no_stack() {
        let table = Contexts::new();
        assert!(table.publish(first_frame(1)));
        let boot = stack(5).1 - 0x40;
        assert_eq!(
            switch_to(&table, boot, first_frame(1), false, not_on_it),
            Ok(first_frame(1))
        );
        // Suspended, its frame covers the stack it is on; its home covers nothing.
        assert!(covered(&table, 5));
        assert_eq!(
            switch_to(&table, first_frame(1) - 8, boot, false, not_on_it),
            Ok(boot)
        );
        assert!(
            !covered(&table, 5),
            "running: no stack of its own in the table"
        );
        assert!(table.covers(0, 2), "its home is BOOT_FLOW, 1");
    }

    #[test]
    fn a_frame_on_another_stack_covers_that_stack_while_suspended() {
        // A thread switched out on a measured stack: its saved frame is there, its home on its own.
        let table = Contexts::new();
        assert!(table.publish(first_frame(1)));
        assert!(table.publish(first_frame(2)));
        assert_eq!(
            switch_to(&table, 0x9000, first_frame(1), false, not_on_it),
            Ok(first_frame(1))
        );
        let on_measured = stack(7).1 - 0x100;
        assert_eq!(
            switch_to(&table, on_measured, first_frame(2), false, not_on_it),
            Ok(first_frame(2))
        );
        assert!(covered(&table, 7), "the measured stack, by the frame");
        assert!(covered(&table, 1), "its own stack, by its home");
        assert_eq!(
            switch_to(&table, first_frame(2) - 8, on_measured, false, not_on_it),
            Ok(on_measured)
        );
        assert!(!covered(&table, 7), "running again: the frame is taken");
        assert!(covered(&table, 1));
    }

    /// Capacity (the coordinating session's ruling (a) on TASKS item 70): 16 run-queue slots,
    /// the boot flow's among them, and one context ended and not yet retired fit, with room to
    /// spare; a full table refuses a publication.
    #[test]
    fn a_full_run_queue_and_an_ended_context_fit() {
        const CAPACITY: u64 = 16;
        let table = Contexts::new();
        // Fifteen threads, plus the boot flow: the run queue's sixteen slots.
        for n in 1..CAPACITY {
            assert!(table.publish(first_frame(n)));
        }
        let boot = 0x9000;
        assert_eq!(
            switch_to(&table, boot, first_frame(1), false, not_on_it),
            Ok(first_frame(1))
        );
        // Thread 1 ends; its queue slot takes a new thread before the home is retired.
        assert_eq!(
            switch_to(&table, 0x100, first_frame(2), true, not_on_it),
            Ok(first_frame(2))
        );
        assert!(table.publish(first_frame(CAPACITY)));
        let taken = table
            .slots
            .iter()
            .filter(|s| s.home.load(Relaxed) != 0)
            .count();
        assert_eq!(taken, CAPACITY as usize + 1);
        assert!(taken <= SLOTS);
        // What is left can be published, and no more.
        for n in 0..(SLOTS - taken) as u64 {
            assert!(table.publish(first_frame(100 + n)));
        }
        assert!(!table.publish(first_frame(200)), "a full table refuses");
    }

    #[test]
    fn a_boot_flow_with_no_free_slot_is_refused() {
        let table = Contexts::new();
        for n in 0..SLOTS as u64 {
            assert!(table.publish(first_frame(n)));
        }
        assert_eq!(
            switch_to(&table, 0x9000, first_frame(0), false, not_on_it),
            Err(Refused::Full)
        );
    }

    #[test]
    fn publication_refuses_what_cannot_be_a_frame() {
        let table = Contexts::new();
        assert!(!table.publish(0));
        assert!(!table.publish(BOOT_FLOW));
    }

    /// TASKS item 75: a context published and never resumed is withdrawn whole, its slot free
    /// again for another publication; a second withdrawal finds nothing.
    #[test]
    fn a_context_never_resumed_is_withdrawn_and_its_slot_reused() {
        let table = Contexts::new();
        for n in 0..SLOTS as u64 {
            assert!(table.publish(first_frame(n)));
        }
        assert!(!table.publish(first_frame(100)), "full");
        assert!(table.withdraw(first_frame(3)));
        assert!(!covered(&table, 3), "its stack is no longer covered");
        assert_eq!(
            switch_to(&table, 0x9000, first_frame(3), false, not_on_it),
            Err(Refused::NeverSaved),
            "a withdrawn frame is never resumed"
        );
        assert!(!table.withdraw(first_frame(3)), "withdrawn once");
        assert!(table.publish(first_frame(100)), "the slot is free again");
        assert!(covered(&table, 100));
        assert!(covered(&table, 4), "the others keep their homes");
    }

    /// TASKS item 75: a context the processor resumed is never withdrawn, and keeps its home.
    #[test]
    fn a_resumed_context_is_not_withdrawn() {
        let table = Contexts::new();
        assert!(table.publish(first_frame(1)));
        assert!(table.publish(first_frame(2)));
        assert_eq!(
            switch_to(&table, 0x9000, first_frame(1), false, not_on_it),
            Ok(first_frame(1))
        );
        assert!(!table.withdraw(first_frame(1)), "running");
        assert!(covered(&table, 1), "its home stays");
        // Suspended at a frame of its own, it is still not the context published at that frame.
        let here = first_frame(1) - 0x200;
        assert_eq!(
            switch_to(&table, here, first_frame(2), false, not_on_it),
            Ok(first_frame(2))
        );
        assert!(!table.withdraw(first_frame(1)), "suspended");
        assert!(!table.withdraw(here), "a saved frame is no home");
        assert!(covered(&table, 1));
        assert_eq!(
            switch_to(&table, first_frame(2) - 8, here, false, not_on_it),
            Ok(here)
        );
    }

    /// TASKS item 75: nothing that cannot be a published frame is withdrawn.
    #[test]
    fn a_withdrawal_refuses_what_was_never_published() {
        let table = Contexts::new();
        assert!(!table.withdraw(0));
        assert!(!table.withdraw(BOOT_FLOW));
        assert!(!table.withdraw(first_frame(1)));
        assert!(table.publish(first_frame(1)));
        let boot = stack(5).1 - 0x40;
        assert_eq!(
            switch_to(&table, boot, first_frame(1), false, not_on_it),
            Ok(first_frame(1))
        );
        // The boot flow's slot: its home is BOOT_FLOW, its frame `boot`.
        assert!(!table.withdraw(boot));
        assert!(covered(&table, 5), "the boot flow's frame stays");
    }

    /// TASKS item 77: a switch answers the incoming context's home, on its first run (frame and
    /// home the same) and on a later one (a saved frame below it), the boot flow's `BOOT_FLOW`
    /// when it is switched back to, and none when nothing switched; `running_home` reads the
    /// same back.
    #[test]
    fn a_switch_answers_the_incoming_home() {
        let table = Contexts::new();
        assert_eq!(table.running_home(), None, "before the first switch-out");
        assert!(table.publish(first_frame(1)));
        assert!(table.publish(first_frame(2)));
        let boot = 0x9000;
        let first = table
            .switch(boot, first_frame(1), false, not_on_it)
            .unwrap();
        assert_eq!(
            (first.frame, first.home),
            (first_frame(1), Some(first_frame(1)))
        );
        assert_eq!(table.running_home(), Some(first_frame(1)));
        let at_1 = first_frame(1) - 0x200;
        let second = table
            .switch(at_1, first_frame(2), false, not_on_it)
            .unwrap();
        assert_eq!(second.home, Some(first_frame(2)));
        let at_2 = first_frame(2) - 0x200;
        let back = table.switch(at_2, at_1, false, not_on_it).unwrap();
        assert_eq!((back.frame, back.home), (at_1, Some(first_frame(1))));
        assert_eq!(table.running_home(), Some(first_frame(1)));
        let same = table.switch(0x7000, 0x7000, false, not_on_it).unwrap();
        assert_eq!((same.frame, same.home), (0x7000, None));
        let to_boot = table.switch(at_1 - 8, boot, false, not_on_it).unwrap();
        assert_eq!((to_boot.frame, to_boot.home), (boot, Some(BOOT_FLOW)));
        assert_eq!(table.running_home(), Some(BOOT_FLOW));
    }

    #[test]
    fn debug_names_no_address() {
        let table = Contexts::new();
        assert!(table.publish(0xffff_c000_0001_ff40));
        assert_eq!(format!("{table:?}"), "Contexts");
        let switched = Switched {
            frame: 0xffff_c000_0001_ff40,
            home: Some(0xffff_c000_0001_ff40),
        };
        assert_eq!(format!("{switched:?}"), "Switched");
    }

    /// The review of TASKS item 70, T70-1, its experiment (a): a retirement interrupted between
    /// its take of `ended` and its clear of the home. `running_on` runs in that gap and stands for
    /// the interrupt: a `switch` there finds nothing to retire, and a reaper there finds the ended
    /// stack still covered, so it would leak it. Safe, but lost: why every caller runs the
    /// retirement with interrupts off.
    #[test]
    fn a_retirement_interrupted_between_its_take_and_its_clear_leaves_the_stack_covered() {
        let table = Contexts::new();
        for n in 1..=3 {
            assert!(table.publish(first_frame(n)));
        }
        assert_eq!(
            switch_to(&table, 0x9000, first_frame(1), false, not_on_it),
            Ok(first_frame(1))
        );
        assert_eq!(
            switch_to(&table, 0x100, first_frame(2), true, not_on_it),
            Ok(first_frame(2))
        );
        let seen_in_the_gap = core::cell::Cell::new(None);
        let outcome = table.retire_ended(|_| {
            // The interrupt: a switch whose own retirement finds `ended` already taken.
            assert_eq!(
                switch_to(&table, first_frame(2) - 8, first_frame(3), false, not_on_it),
                Ok(first_frame(3))
            );
            // A reaper here would find stack 1, ended and no longer recorded as ended, in use.
            seen_in_the_gap.set(Some(covered(&table, 1)));
            false
        });
        assert_eq!(outcome, Ok(()));
        assert_eq!(
            seen_in_the_gap.get(),
            Some(true),
            "covered, so a drop would leak it"
        );
        assert!(
            !covered(&table, 1),
            "retired once the interrupted call finishes"
        );
    }

    /// T70-6: a refused retirement does not overwrite a newer exit's record.
    #[test]
    fn a_refused_retirement_does_not_overwrite_a_newer_exit() {
        let table = Contexts::new();
        for n in 1..=3 {
            assert!(table.publish(first_frame(n)));
        }
        assert_eq!(
            switch_to(&table, 0x9000, first_frame(1), false, not_on_it),
            Ok(first_frame(1))
        );
        assert_eq!(
            switch_to(&table, 0x100, first_frame(2), true, not_on_it),
            Ok(first_frame(2))
        );
        let outcome = table.retire_ended(|_| {
            // In the gap, context 2 ends too, and records itself.
            assert_eq!(
                switch_to(&table, 0x200, first_frame(3), true, not_on_it),
                Ok(first_frame(3))
            );
            true
        });
        assert_eq!(outcome, Err(Refused::StillOnIt));
        // The record left is context 2's, the newer: retiring it now clears its home.
        assert_eq!(table.retire_ended(not_on_it), Ok(()));
        assert!(
            !covered(&table, 2),
            "the newer exit's record survived the put-back"
        );
        assert!(
            covered(&table, 1),
            "the refused one stays covered: its caller halts"
        );
    }
}
