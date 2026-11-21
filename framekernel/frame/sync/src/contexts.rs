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
//!   ended, while a withdrawal is under way, and from its switch-out until its processor publishes
//!   the frame it left (below).
//!
//! Each table also has a **leaving record**: the frame the processor's last switch-out left and
//! its slot, until the processor publishes it there.
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
//! word, or the leaving record until the frame is published, still covers a stack a context is
//! suspended on that is not its home: a measured stack (`origin_frame::stack::with_measured_stack`),
//! or the boot flow's stack.
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
//!   publication of the frame its switch-out of the running context (`current`) left; a context
//!   being withdrawn never ran, so it was never `current`, and no publication writes its slot
//!   (the review of TASKS items 75, 77 and 81, R75-3).
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
//! - [`Contexts::publish_leaving`]: this processor only, **with interrupts off**, as a
//!   retirement. `switch` calls it first; a stack's drop calls it beside its retirement. A
//!   publication interrupted between its two stores by a switch would then clear the record that
//!   switch wrote, and the frame it named would never be published: its context lost.
//! - [`Contexts::covers`]: any processor. It never waits and takes no lock. A scan of every
//!   processor's table goes through [`Moves::scan`], which may wait once for the move gate, with
//!   interrupts off, after its fast passes: so a stack may still be dropped under any lock but
//!   the move gate, and a mover holding the gate drops none.
//! - [`Moves::relocate`], or [`Moves::begin`] with [`Contexts::lend`], [`Contexts::adopt`] and
//!   [`Lent::restore`] or [`Lent::forget`]: a mover, on any processor, with both queue locks
//!   held and interrupts off.
//!
//! # A frame is published only after its processor has left the stack (ADR-0020 S5)
//!
//! A switch-out's frame lies on the stack the trap entry is still running on: after
//! [`Contexts::switch`] returns, the entry returns along that stack, and only then loads the
//! incoming frame's stack pointer (`origin_frame::cpu`, `exception_saved`: `call dispatch`, then
//! `mov rsp, rax`). Once another processor may resume a frame, that frame must not be resumable
//! before its processor has left the stack, or two processors would run on one stack. So
//! `switch` does not store the outgoing frame in its slot: it records it, with its slot, as
//! **leaving**, and the processor publishes it ([`Contexts::publish_leaving`]) at its next call
//! into the table, which runs on another stack: the top of its next `switch`, before `next` is
//! taken (so a switch straight back to that context finds it), or a stack's drop, which runs on
//! the processor's current context. Until it is published nobody can take the frame, this
//! processor included; this processor's own next switch publishes it before it takes anything.
//!
//! - **Publication** stores the frame in its slot (Release), then clears the record (Release).
//!   Only this processor writes the record, with interrupts off, and never from a handler that
//!   can interrupt a switch: an NMI, a machine check, a debug exception or a double fault.
//! - **A reader** ([`Contexts::covers`]) reads the record (Acquire) before the slots. One that
//!   finds the record clear has read the clear or a later value, so it sees the store into the
//!   slot that came before the clear, or a later value of that slot. One that finds a frame there
//!   covers the stack that frame is on. So no frame is missed between the two stores.
//! - **The cost:** one plain load at each call with nothing leaving; when a frame is published,
//!   two plain stores, on lines the processor owns; the switch-out writes two words of the record
//!   where it wrote one into the slot.
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
//! # Moves between processors (ADR-0020 S4)
//!
//! A context moves from one processor's table (the **source**) to another's (the
//! **destination**) by three operations, in this order, under the machine's move gate
//! ([`Moves`]), each of which takes a witness that the gate is held ([`Moving`]):
//!
//! 1. [`Contexts::lend`], on the source: takes the frame of a suspended context by the same
//!    compare-and-swap a switch or a withdrawal makes, so at most one of them takes it. The home
//!    stays, so the stack stays covered here.
//! 2. [`Contexts::adopt`], on the destination: claims a free slot with the home and stores the
//!    frame (Release), as a publication does. From here the destination may resume it. If the
//!    destination is full, [`Lent::restore`] puts the frame back in its source slot instead.
//! 3. [`Lent::forget`], on the source: clears the home (Release).
//!
//! [`Moves::relocate`] runs the three in that order, with the caller's own work between `adopt`
//! and `forget`. So the home is claimed in the destination before it is retired in the source:
//! claim before retire. That alone does not stop a reader from missing it: a scan of every
//! table that reads the destination before the claim and the source after the clear finds the
//! home in neither (ADR-0020 N4 (a)).
//!
//! **The move sequence closes that.** Two machine-wide counters, both raised by
//! read-modify-writes under the gate: **begun**, before `lend`; **finished**, with Release, after
//! `forget` or `restore`. A scan ([`Moves::scan`]) reads finished, then begun (Acquire), makes its
//! pass, and reads begun again. A pass that found what it asked about is believed. One that found
//! nothing is believed only if the first two readings are equal and the third equals them;
//! otherwise it tries again. Why that is enough, in outline (Loom decides it, in
//! `models::moves`):
//!
//! - A pass that misses a moving home read the source after the move's `forget`, or after a later
//!   write to that word: a claim of the freed slot is a compare-and-swap that reads the forget's
//!   store, so it is in that store's release sequence. Either way the pass's Acquire load
//!   synchronises with the forget, and the move's raise of begun, sequenced before it, happens
//!   before the scan's last reading of begun, which therefore counts it.
//! - If the scan's first reading of begun did not count that raise, the last reading differs from
//!   it. If it did, the reading of finished before it must have counted the same move's finish,
//!   or the two differ: and a reading that counts the finish (Release, after `adopt`) sees the
//!   destination's claim, so the pass would have found the home there, or wherever a later move
//!   took it, which raised begun again.
//! - Begun is raised before `lend` (not only before `adopt`; the coordinating session's ruling on
//!   Q2 of TASKS item 95), so a pass that reads the source's frame word after `lend` has taken it
//!   also counts the move. That covers a suspended frame on a stack that is not its home, which
//!   is in no `frame` word between `lend` and `adopt`. ADR-0020 S2 keeps such contexts from
//!   moving, and the sequence covers them anyway.
//! - One counter whose parity says "a move is under way" is not enough: two moves begun at once
//!   leave it even. With the gate taken first, moves do not overlap, and the two counters say
//!   whether one began or finished during the pass.
//!
//! **A scan always ends.** After [`FAST_TRIES`] passes that each overlapped a move, the scan makes
//! one last pass under the gate, with interrupts off ([`Quiet`]): no move is then under way, and
//! every move that finished before it released the gate is visible to it. So a scan ends after
//! at most `FAST_TRIES + 1` passes and one wait for the gate. The gate is a ticket lock, which
//! serves its waiters in the order they came, and each processor holds at most one ticket at a
//! time, with interrupts off: so that wait is at most one critical section for each other
//! processor waiting ahead of it, whatever the rate of moves. (A test-and-set lock would let a
//! waiter lose to every newcomer in turn; ADR-0020 S4's "one move's critical section" holds only
//! when no other processor waits for the gate; the review of TASKS item 95, R95-2.) Loom cannot
//! show the wait ends: an execution that spins past its bound is not explored further. The lock
//! order is always queue locks, then the gate: a mover holds both queue locks first, and a
//! scanner may hold one (a stack dropped under it).
//!
//! **Begun is Relaxed.** Nothing reads it to learn what came before it: the scan's argument above
//! rests on happens-before carried by the forget's Release store (or the claims in its release
//! sequence) and by finished's Release, with begun sequenced before both. Finished is raised with
//! Release, and every reading is Acquire, which on x86-64 is a plain load. Each counter's raise is
//! one `lock xadd` per move.
//!
//! **What a move does not touch.** `current` and `ended` are this processor's; a lent context is
//! neither (it is suspended), and the leaving record names only a frame not yet in its slot,
//! which `lend` refuses ([`LendRefused::Leaving`]). The boot or idle flow never moves
//! ([`LendRefused::BootFlow`]; ADR-0020 S2, point 4). [`Contexts::withdraw`] keeps its meaning:
//! only a spawner calls it, for a context it never queued, which was never moved.
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
//! **Moves** (ADR-0020 S4, N13). A context being moved holds a slot in both tables from its
//! `adopt` to its `forget`. The mover adopts only after it has found a free slot in the
//! destination's run queue, under that queue's lock, and holds both queue locks until the source's
//! `forget`, so neither table holds more than its queue's contexts, one ended, and spawns in
//! flight. A destination that is full all the same refuses the adoption ([`AdoptRefused::Full`]).
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
    /// The frame of the context this processor last switched out, while it is not yet
    /// published in its slot, or 0 (ADR-0020 S5; the module documentation, "A frame is
    /// published only after its processor has left the stack"). Written only by this
    /// processor, with interrupts off; read by any processor.
    leaving: prim::AtomicU64,
    /// The slot that frame belongs to. Written and read only by this processor, with
    /// interrupts off.
    leaving_slot: prim::AtomicUsize,
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
            leaving: prim::AtomicU64::new(0),
            leaving_slot: prim::AtomicUsize::new(NONE),
        }
    }

    /// A table with no context.
    #[cfg(all(test, loom))]
    pub fn new() -> Self {
        Self {
            slots: core::array::from_fn(|_| Context::new()),
            current: prim::AtomicUsize::new(NONE),
            ended: prim::AtomicUsize::new(NONE),
            leaving: prim::AtomicU64::new(0),
            leaving_slot: prim::AtomicUsize::new(NONE),
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
    /// First it publishes the frame its last switch-out left ([`Self::publish_leaving`]), and
    /// retires a context that ended earlier ([`Self::retire_ended`]). Unless `next` is `here`,
    /// `next` must be a frame saved in this table. It is taken, so it cannot be resumed twice, and
    /// its context becomes the running one. The context that was running keeps its home and
    /// leaves `here` as its frame, which is published only at this processor's next call into the
    /// table (ADR-0020 S5), or, if `exiting`, is recorded as ended and saves nothing. The boot or
    /// idle flow's first switch-out claims a slot for it. No `home` is written. The answer carries
    /// the incoming context's home, read from the slot whose frame was taken, which the trap
    /// entry names as the stack ring 3 lands on (TASKS item 77); with nothing switched it carries
    /// none.
    ///
    /// `running_on(address)` says whether this processor runs on the stack holding `address`.
    pub fn switch(
        &self,
        here: u64,
        next: u64,
        exiting: bool,
        running_on: impl Fn(u64) -> bool,
    ) -> Result<Switched, Refused> {
        // Before `next` is taken: a context switched out by the last switch, and switched back
        // to now, is in its slot again to be taken.
        self.publish_leaving();
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
            Some(_) => self.record_leaving(outgoing, here),
            // The boot or idle flow, switched out for the first time; it never ends.
            None if exiting => {}
            None => {
                // Its slot is claimed with no frame, which it leaves like any other.
                let Some(slot) = self.slots.iter().position(|slot| slot.claim(BOOT_FLOW, 0)) else {
                    return Err(Refused::Full);
                };
                self.record_leaving(slot, here);
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

    /// Records `frame` as the frame the context in slot `slot` leaves: published in that slot
    /// only at this processor's next call into the table ([`Self::publish_leaving`]). This
    /// processor only, interrupts off, in its switch, which has just published the previous one.
    fn record_leaving(&self, slot: usize, frame: u64) {
        self.leaving_slot.store(slot, Relaxed);
        self.leaving.store(frame, Release);
    }

    /// Publishes the frame this processor's last switch-out left, if it has not been: the frame
    /// is stored in its context's slot (Release), where it may be resumed, and only then is the
    /// leaving record cleared (Release), so a reader that finds the record clear finds the frame
    /// in the slot (ADR-0020 S5). This processor only, with interrupts off, at a call into the
    /// table that runs after the switch-out has left the stack the frame is on: the top of the
    /// next [`Self::switch`], or a caller such as a stack's drop. Never from a handler that can
    /// interrupt a switch (an NMI, a machine check, a debug exception or a double fault). With
    /// nothing leaving it costs one plain load.
    pub fn publish_leaving(&self) {
        // Only this processor writes `leaving`: a plain load sees its own last write.
        let frame = self.leaving.load(Relaxed);
        if frame == 0 {
            return;
        }
        if let Some(slot) = self.slots.get(self.leaving_slot.load(Relaxed)) {
            slot.frame.store(frame, Release);
        }
        self.leaving.store(0, Release);
    }

    /// Whether any context's home or saved frame lies in `[low, top)`: a stack a context of this
    /// processor may still use. Any processor may ask. It never waits.
    ///
    /// The leaving record is read before the slots: a frame published between the two reads is
    /// in the slot by the time the slot is read, since the reader that finds the record clear
    /// sees the store into the slot that came before the clear (ADR-0020 S5).
    pub fn covers(&self, low: u64, top: u64) -> bool {
        let inside = |at: u64| at >= low && at < top;
        if inside(self.leaving.load(Acquire)) {
            return true;
        }
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

/// Why [`Contexts::lend`] refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LendRefused {
    /// The frame is the one this table's processor last switched out, not yet published: it
    /// may still be returning along the frame's stack (ADR-0020 S5). Try later.
    Leaving,
    /// The frame is the boot or idle flow's, which never moves (ADR-0020 S2, point 4).
    BootFlow,
    /// No slot holds the frame suspended: it is 0 or [`BOOT_FLOW`], was never saved here, was
    /// taken already (resumed, withdrawn or lent), or has not been switched out yet.
    NotHere,
}

/// Why [`Contexts::adopt`] refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdoptRefused {
    /// The destination table has no free slot.
    Full,
    /// The destination is the table the frame was lent from.
    SameTable,
}

/// Why [`Moves::relocate`] moved nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveRefused {
    /// The source would not lend the frame; nothing was taken.
    Lend(LendRefused),
    /// The destination would not adopt it; the frame is back in its source slot, resumable
    /// there as before.
    Adopt(AdoptRefused),
    /// The destination would not adopt it, and the frame could not be put back: its slot's
    /// frame word was no longer 0. Nothing in the protocol writes it while the frame is lent,
    /// so this is an inconsistency, and the context cannot be resumed (ADR-0020 S20).
    NotRestored,
}

/// How many passes [`Moves::scan`] makes with no lock before it makes one under the move gate
/// (ADR-0020 S4: at most four tries on the fast path, then one under the gate).
#[cfg(not(all(test, loom)))]
pub const FAST_TRIES: u32 = 4;
/// The Loom models' count: two, so that a model can make both fast tries miss with three moves
/// (TASKS item 95, the coordinating session's ruling on Q12). Nothing in the code depends on the
/// count; the host test `a_scan_makes_its_fast_passes_then_one_under_the_gate` shows the
/// kernel's four and one.
#[cfg(all(test, loom))]
pub const FAST_TRIES: u32 = 2;

/// Runs a closure with interrupts off on the calling processor. [`Moves::scan`] takes the move
/// gate inside it: a mover holds the gate with interrupts off, and may be an interrupt handler
/// of the scanner's own processor, so a scanner holding the gate with interrupts on could be
/// interrupted by a mover that then spins on it for ever. The kernel implements it with
/// `cpu::without_interrupts`.
pub trait Quiet {
    /// Runs `f` with interrupts off, and restores their state after.
    fn quietly<R>(&self, f: impl FnOnce() -> R) -> R;
}

/// What [`Moves::scan`] answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scanned {
    /// Whether the pass found what it looked for.
    pub covered: bool,
    /// How many passes it made: at most [`FAST_TRIES`] with no lock, then one under the gate.
    pub passes: u32,
}

impl Scanned {
    /// Whether the last pass was made under the move gate.
    pub fn gated(&self) -> bool {
        self.passes > FAST_TRIES
    }
}

/// The move gate: a ticket lock, so that whoever waits for it is served in the order it came
/// (the review of TASKS item 95, R95-2). A test-and-set lock lets a waiter lose to every newcomer
/// in turn, so a scan's last pass could wait as long as moves keep coming. Each processor holds at
/// most one ticket at a time, with interrupts off, so a waiter waits for at most one critical
/// section per other processor, whatever the rate of moves.
struct Gate {
    /// The next ticket to hand out.
    next: prim::AtomicU32,
    /// The ticket allowed in.
    serving: prim::AtomicU32,
}

/// Holding the gate: dropping it lets the next ticket in (Release).
struct GateGuard<'g> {
    gate: &'g Gate,
    ticket: u32,
}

impl Gate {
    #[cfg(not(all(test, loom)))]
    const fn new() -> Self {
        Self {
            next: prim::AtomicU32::new(0),
            serving: prim::AtomicU32::new(0),
        }
    }

    #[cfg(all(test, loom))]
    fn new() -> Self {
        Self {
            next: prim::AtomicU32::new(0),
            serving: prim::AtomicU32::new(0),
        }
    }

    /// Takes a ticket and waits for its turn. The Acquire load that sees the turn reads the last
    /// holder's Release store, so everything the holders before did is visible.
    fn lock(&self) -> GateGuard<'_> {
        let ticket = self.next.fetch_add(1, Relaxed);
        while self.serving.load(Acquire) != ticket {
            prim::spin_loop();
        }
        GateGuard { gate: self, ticket }
    }

    /// Whether nobody holds or waits for the gate: for the host tests.
    #[cfg(all(test, not(loom)))]
    fn is_free(&self) -> bool {
        self.next.load(Relaxed) == self.serving.load(Relaxed)
    }
}

impl Drop for GateGuard<'_> {
    fn drop(&mut self) {
        self.gate
            .serving
            .store(self.ticket.wrapping_add(1), Release);
    }
}

/// The machine's move sequence (ADR-0020 S4): the move gate and the two counters, **begun** and
/// **finished**. The module documentation, "Moves between processors", says what each is for.
pub struct Moves {
    /// Held by a move from before its `lend` to after its `forget`, with interrupts off, after
    /// both queue locks; whoever holds it takes no other lock and drops no stack. A scan's last
    /// pass holds it too.
    gate: Gate,
    /// Raised by each move, under the gate, before its `lend`.
    begun: prim::AtomicU64,
    /// Raised by each move (Release), under the gate, after its `forget` or `restore`.
    finished: prim::AtomicU64,
}

impl core::fmt::Debug for Moves {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Moves")
    }
}

impl Moves {
    /// No move made yet.
    #[cfg(not(all(test, loom)))]
    pub const fn new() -> Self {
        Self {
            gate: Gate::new(),
            begun: prim::AtomicU64::new(0),
            finished: prim::AtomicU64::new(0),
        }
    }

    /// No move made yet.
    #[cfg(all(test, loom))]
    pub fn new() -> Self {
        Self {
            gate: Gate::new(),
            begun: prim::AtomicU64::new(0),
            finished: prim::AtomicU64::new(0),
        }
    }

    /// Begins a move: takes the gate, then raises begun. The caller already holds the source's
    /// and the destination's queue locks, with interrupts off, and takes no other lock, and drops
    /// no stack, until the move ends, when the [`Moving`] is dropped: a stack's drop scans, and a
    /// scan's last pass waits for the gate, which this move holds.
    pub fn begin(&self) -> Moving<'_> {
        Moving {
            moves: self,
            _gate: self.enter(),
        }
    }

    /// Moves the context suspended at `frame` from `source` to `destination`, in the order
    /// ADR-0020 S4 sets: under the gate, `lend` from the source, `adopt` into the destination (or
    /// `restore` and give up), `between` with the context's home, then `forget` in the source.
    /// So the home is in the source's table until the destination's holds it. `between` is where
    /// the caller passes the thread's run-queue entry and record (ADR-0020 S6); it runs under the
    /// gate, so it takes no lock.
    ///
    /// The caller holds both queue locks, with interrupts off, and has checked that the
    /// destination's run queue has a free slot (N13).
    pub fn relocate<R>(
        &self,
        source: &Contexts,
        destination: &Contexts,
        frame: u64,
        between: impl FnOnce(u64) -> R,
    ) -> Result<(u64, R), MoveRefused> {
        let moving = self.begin();
        let lent = source.lend(&moving, frame).map_err(MoveRefused::Lend)?;
        if let Err(refused) = destination.adopt(&moving, &lent) {
            return if lent.restore() {
                Err(MoveRefused::Adopt(refused))
            } else {
                Err(MoveRefused::NotRestored)
            };
        }
        let home = lent.home();
        let value = between(home);
        lent.forget();
        drop(moving);
        Ok((home, value))
    }

    /// Scans every table with `pass`, which answers whether some table covers what the caller
    /// asks about, and does not let a move hide a home from it (ADR-0020 S4).
    ///
    /// A pass that finds it is believed at once: what it read was there. One that finds nothing
    /// is believed only if no move began or finished across it: finished, then begun, read before
    /// the pass, equal, and begun read again after it unchanged. Otherwise the scan tries again,
    /// at most [`FAST_TRIES`] times, and then makes one last pass under the move gate, inside
    /// `quiet`, with no move under way. So it ends after at most `FAST_TRIES + 1` passes and one
    /// wait for the gate, during which at most one critical section per other processor runs
    /// before it, whatever the rate of moves (the gate serves its waiters in order).
    ///
    /// The caller may hold a queue lock, which a mover takes before the gate: the order is
    /// always queue locks, then the gate. It must not hold the gate itself: a mover that dropped a
    /// stack would wait here for its own gate.
    pub fn scan(&self, pass: impl Fn() -> bool, quiet: &impl Quiet) -> Scanned {
        let mut passes = 0;
        while passes < FAST_TRIES {
            passes += 1;
            let readings = self.readings();
            if pass() {
                return Scanned {
                    covered: true,
                    passes,
                };
            }
            if self.settled(readings) {
                return Scanned {
                    covered: false,
                    passes,
                };
            }
        }
        let covered = quiet.quietly(|| {
            let _gate = self.gate.lock();
            pass()
        });
        Scanned {
            covered,
            passes: FAST_TRIES + 1,
        }
    }

    // The move sequence: every raise and every read of the two counters is in the four functions
    // below (the module documentation, "Moves between processors", says why each ordering is
    // enough).

    /// A move's start: the gate, then begun.
    fn enter(&self) -> GateGuard<'_> {
        let gate = self.gate.lock();
        let _ = self.begun.fetch_add(1, Relaxed);
        gate
    }

    /// A move's end, before the gate is released: finished, with Release.
    fn raise_finished(&self) {
        let _ = self.finished.fetch_add(1, Release);
    }

    /// What a scan reads before its pass: finished, then begun.
    fn readings(&self) -> (u64, u64) {
        (self.finished.load(Acquire), self.begun.load(Acquire))
    }

    /// Whether a pass that found nothing can be believed: no move was under way when it began
    /// (finished and begun equal), and none began during it (begun unchanged).
    fn settled(&self, (finished, begun): (u64, u64)) -> bool {
        finished == begun && self.begun.load(Acquire) == begun
    }
}

impl Default for Moves {
    fn default() -> Self {
        Self::new()
    }
}

/// A move under way: holding it is holding the move gate ([`Moves::begin`]). Dropping it raises
/// finished (Release), then releases the gate, also when a panic unwinds through it. On the
/// kernel a panic under the gate halts the machine anyway (its panic policy), so this matters to
/// the host tests and the models, not to recovery.
#[must_use = "a move holds the gate until this is dropped"]
pub struct Moving<'m> {
    moves: &'m Moves,
    /// Dropped after `Drop::drop` has raised finished.
    _gate: GateGuard<'m>,
}

impl Drop for Moving<'_> {
    fn drop(&mut self) {
        self.moves.raise_finished();
    }
}

impl core::fmt::Debug for Moving<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Moving")
    }
}

/// A frame taken from its slot by [`Contexts::lend`], under a [`Moving`] it borrows, so the move
/// cannot end before it is settled: [`Lent::restore`] puts it back, or [`Lent::forget`] clears
/// its home once the destination has adopted it. Its `Debug` writes its name alone: its words
/// are addresses in the kernel stacks' region.
#[must_use = "a lent frame is in no slot until it is restored, or adopted and its home forgotten"]
pub struct Lent<'t> {
    table: &'t Contexts,
    slot: usize,
    home: u64,
    frame: u64,
}

impl core::fmt::Debug for Lent<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Lent")
    }
}

impl Lent<'_> {
    /// The lent context's home.
    pub fn home(&self) -> u64 {
        self.home
    }

    /// The lent frame.
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// Puts the frame back in its slot, after a refused `adopt`: a compare-and-swap of the slot's
    /// frame word from 0, so it is resumable at its source again. `false` if the word was not 0,
    /// which nothing in the protocol does while the frame is lent.
    pub fn restore(self) -> bool {
        self.table.slots.get(self.slot).is_some_and(|slot| {
            slot.frame
                .compare_exchange(0, self.frame, AcqRel, Acquire)
                .is_ok()
        })
    }

    /// Clears the home in the source, once the destination has adopted the context: a Release
    /// store of 0, so a reader that no longer finds it there has read that store or a later
    /// value, and so sees the move's raise of begun (the module documentation, "Moves between
    /// processors"). The slot is free for a new claim.
    pub fn forget(self) {
        if let Some(slot) = self.table.slots.get(self.slot) {
            slot.home.store(0, Release);
        }
    }
}

impl Contexts {
    /// Takes the frame `frame` of a suspended context of this table for a move, under the move
    /// gate (`moving`): the same compare-and-swap of the frame word from the frame to 0 that a
    /// switch or a withdrawal makes, so at most one of them takes it (ADR-0020 S4). The home
    /// stays, so the stack stays covered here until the move's [`Lent::forget`].
    ///
    /// The leaving record is read first: a frame not yet published is refused as
    /// [`LendRefused::Leaving`], and one published meanwhile is in its slot by the time the
    /// slots are read (the publication stores the slot before it clears the record).
    pub fn lend<'t>(
        &'t self,
        _moving: &'t Moving<'_>,
        frame: u64,
    ) -> Result<Lent<'t>, LendRefused> {
        if frame <= BOOT_FLOW {
            return Err(LendRefused::NotHere);
        }
        if self.leaving.load(Acquire) == frame {
            return Err(LendRefused::Leaving);
        }
        for (slot, context) in self.slots.iter().enumerate() {
            if context.frame.load(Relaxed) != frame {
                continue;
            }
            // The boot or idle flow's slot keeps `BOOT_FLOW` for the table's life.
            if context.home.load(Acquire) == BOOT_FLOW {
                return Err(LendRefused::BootFlow);
            }
            // The take. While the frame word is not 0 no home is cleared: a retirement clears
            // only an ended context's, a withdrawal takes the frame first, and a forget follows
            // a lend; so the home read after it is this context's.
            if context
                .frame
                .compare_exchange(frame, 0, AcqRel, Acquire)
                .is_ok()
            {
                return Ok(Lent {
                    table: self,
                    slot,
                    home: context.home.load(Acquire),
                    frame,
                });
            }
        }
        Err(LendRefused::NotHere)
    }

    /// Records a lent context in this table, under the move gate: claims a free slot with its
    /// home (a compare-and-swap from 0) and stores its frame (Release), as a publication does
    /// with a home that need not equal the frame. Before the source forgets the home, so the
    /// stack is never in neither table. The context is resumable here from the store on.
    pub fn adopt(&self, _moving: &Moving<'_>, lent: &Lent<'_>) -> Result<(), AdoptRefused> {
        if core::ptr::eq(self, lent.table) {
            return Err(AdoptRefused::SameTable);
        }
        if self
            .slots
            .iter()
            .any(|slot| slot.claim(lent.home, lent.frame))
        {
            Ok(())
        } else {
            Err(AdoptRefused::Full)
        }
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

    /// The frames in the table's slots, in slot order: what a switch or a lend could take.
    fn resumable(table: &Contexts) -> Vec<u64> {
        table
            .slots
            .iter()
            .map(|slot| slot.frame.load(Relaxed))
            .filter(|&frame| frame != 0)
            .collect()
    }

    /// ADR-0020 S5: a switch-out leaves its frame in the leaving record, in no slot, and covered;
    /// the next call into the table publishes it in its context's slot.
    #[test]
    fn a_switched_out_frame_is_leaving_until_the_next_call() {
        let table = Contexts::new();
        assert!(table.publish(first_frame(1)));
        assert!(table.publish(first_frame(2)));
        let boot = stack(9).1 - 0x40;
        assert_eq!(
            switch_to(&table, boot, first_frame(1), false, not_on_it),
            Ok(first_frame(1))
        );
        // The boot flow's first switch-out: its slot is claimed with no frame, the frame leaves.
        assert_eq!(table.leaving.load(Relaxed), boot);
        assert_eq!(resumable(&table), [first_frame(2)]);
        assert!(covered(&table, 9), "covered by the leaving record");
        // Context 1 is switched out on a measured stack: the next switch publishes the boot
        // flow's frame, and context 1's frame leaves.
        let on_measured = stack(7).1 - 0x100;
        assert_eq!(
            switch_to(&table, on_measured, first_frame(2), false, not_on_it),
            Ok(first_frame(2))
        );
        assert_eq!(table.leaving.load(Relaxed), on_measured);
        assert_eq!(resumable(&table), [boot], "published at the switch's top");
        assert!(
            covered(&table, 7),
            "the measured stack, by the leaving record"
        );
        table.publish_leaving();
        assert_eq!(table.leaving.load(Relaxed), 0);
        assert!(resumable(&table).contains(&on_measured));
        assert!(covered(&table, 7), "the measured stack, by its slot");
        // Nothing left: a second publication changes nothing.
        let before = resumable(&table);
        table.publish_leaving();
        assert_eq!(resumable(&table), before);
    }

    /// The coordinating session's condition on Q3 (TASKS item 95): a switch whose `next` is the
    /// context that is leaving publishes it first and then takes it, so it is neither lost nor
    /// taken twice.
    #[test]
    fn a_switch_back_to_the_leaving_context_publishes_it_then_takes_it() {
        let table = Contexts::new();
        assert!(table.publish(first_frame(1)));
        assert!(table.publish(first_frame(2)));
        assert_eq!(
            switch_to(&table, 0x9000, first_frame(1), false, not_on_it),
            Ok(first_frame(1))
        );
        let at_1 = first_frame(1) - 0x200;
        assert_eq!(
            switch_to(&table, at_1, first_frame(2), false, not_on_it),
            Ok(first_frame(2))
        );
        assert_eq!(table.leaving.load(Relaxed), at_1);
        // Straight back to context 1, before anything else called into the table.
        let at_2 = first_frame(2) - 0x200;
        assert_eq!(switch_to(&table, at_2, at_1, false, not_on_it), Ok(at_1));
        assert_eq!(table.running_home(), Some(first_frame(1)));
        assert_eq!(table.leaving.load(Relaxed), at_2, "context 2 leaves now");
        assert!(!resumable(&table).contains(&at_1), "taken once");
        // A second resume of the same frame is refused; context 2's frame is not lost.
        assert_eq!(
            switch_to(&table, at_1 - 0x100, at_1, false, not_on_it),
            Err(Refused::NeverSaved)
        );
        assert_eq!(
            switch_to(&table, at_1 - 0x100, at_2, false, not_on_it),
            Ok(at_2)
        );
    }

    /// A tick that switches nothing (`next == here`) still publishes what the last switch left:
    /// a switched-out context becomes resumable elsewhere within one tick (ADR-0020 S5).
    #[test]
    fn a_switch_that_changes_nothing_still_publishes() {
        let table = Contexts::new();
        assert!(table.publish(first_frame(1)));
        assert!(table.publish(first_frame(2)));
        assert_eq!(
            switch_to(&table, 0x9000, first_frame(1), false, not_on_it),
            Ok(first_frame(1))
        );
        let at_1 = first_frame(1) - 0x200;
        assert_eq!(
            switch_to(&table, at_1, first_frame(2), false, not_on_it),
            Ok(first_frame(2))
        );
        let at_2 = first_frame(2) - 0x200;
        assert_eq!(switch_to(&table, at_2, at_2, false, not_on_it), Ok(at_2));
        assert_eq!(table.leaving.load(Relaxed), 0);
        assert!(resumable(&table).contains(&at_1));
    }

    /// An exit leaves nothing to publish: its frame is never saved, as before ADR-0020 S5.
    #[test]
    fn an_exit_leaves_no_frame() {
        let table = Contexts::new();
        assert!(table.publish(first_frame(1)));
        assert!(table.publish(first_frame(2)));
        assert_eq!(
            switch_to(&table, 0x9000, first_frame(1), false, not_on_it),
            Ok(first_frame(1))
        );
        // The boot flow's frame leaves; context 1's exit publishes it and leaves nothing itself.
        let at_exit = first_frame(1) - 0x300;
        assert_eq!(
            switch_to(&table, at_exit, first_frame(2), true, not_on_it),
            Ok(first_frame(2))
        );
        assert_eq!(table.leaving.load(Relaxed), 0);
        assert_eq!(resumable(&table), [0x9000]);
    }

    /// A host stands in for a processor with interrupts off: nothing interrupts it.
    struct Plain;

    impl Quiet for Plain {
        fn quietly<R>(&self, f: impl FnOnce() -> R) -> R {
            f()
        }
    }

    /// Two tables, each running its boot flow; `source` also has context `n` published and
    /// never run. Returns the boot flows' frames.
    fn two_processors(source: &Contexts, destination: &Contexts, n: u64) -> (u64, u64) {
        assert!(source.publish(first_frame(n)));
        assert!(source.publish(first_frame(20)));
        assert!(destination.publish(first_frame(21)));
        // Each boot flow switches to a thread of its own and back, so it has a slot and its
        // frame is published.
        let (boot_s, boot_d) = (stack(30).1 - 0x40, stack(31).1 - 0x40);
        for (table, boot, own) in [(source, boot_s, 20), (destination, boot_d, 21)] {
            assert_eq!(
                switch_to(table, boot, first_frame(own), false, not_on_it),
                Ok(first_frame(own))
            );
            assert_eq!(
                switch_to(table, first_frame(own) - 0x100, boot, false, not_on_it),
                Ok(boot)
            );
            table.publish_leaving();
        }
        (boot_s, boot_d)
    }

    /// ADR-0020 S4: a move takes the frame from the source, records it in the destination, and
    /// clears the home in the source, in that order; the destination resumes it, the source no
    /// longer can, and the counters say one move began and finished.
    #[test]
    fn a_move_lends_adopts_and_forgets() {
        let (source, destination, moves) = (Contexts::new(), Contexts::new(), Moves::new());
        let (boot_s, boot_d) = two_processors(&source, &destination, 1);
        let seen = core::cell::Cell::new(None);
        let moved = moves.relocate(&source, &destination, first_frame(1), |home| {
            // Between `adopt` and `forget`: both tables cover the stack.
            seen.set(Some((covered(&source, 1), covered(&destination, 1))));
            home
        });
        assert_eq!(moved, Ok((first_frame(1), first_frame(1))));
        assert_eq!(seen.get(), Some((true, true)));
        assert!(!covered(&source, 1), "forgotten in the source");
        assert!(covered(&destination, 1));
        assert_eq!(moves.readings(), (1, 1));
        assert_eq!(
            switch_to(&source, boot_s - 0x100, first_frame(1), false, not_on_it),
            Err(Refused::NeverSaved),
            "the source cannot resume it"
        );
        let switched = destination
            .switch(boot_d - 0x100, first_frame(1), false, not_on_it)
            .unwrap();
        assert_eq!(
            (switched.frame, switched.home),
            (first_frame(1), Some(first_frame(1)))
        );
        // The source's freed slot takes a new context.
        assert!(source.publish(first_frame(2)));
    }

    /// A context that has run and was switched out moves with its saved frame, which is not its
    /// home: the destination resumes the saved frame and names the home's stack.
    #[test]
    fn a_context_that_ran_moves_with_its_saved_frame() {
        let (source, destination, moves) = (Contexts::new(), Contexts::new(), Moves::new());
        let (boot_s, boot_d) = two_processors(&source, &destination, 1);
        assert_eq!(
            switch_to(&source, boot_s - 0x100, first_frame(1), false, not_on_it),
            Ok(first_frame(1))
        );
        let at_1 = first_frame(1) - 0x200;
        assert_eq!(
            switch_to(&source, at_1, boot_s - 0x100, false, not_on_it),
            Ok(boot_s - 0x100)
        );
        // Leaving: not yet movable.
        assert_eq!(
            moves.relocate(&source, &destination, at_1, |_| ()),
            Err(MoveRefused::Lend(LendRefused::Leaving))
        );
        assert_eq!(moves.readings(), (1, 1), "a refused lend is an empty move");
        source.publish_leaving();
        assert_eq!(
            moves.relocate(&source, &destination, at_1, |home| home),
            Ok((first_frame(1), first_frame(1)))
        );
        let switched = destination
            .switch(boot_d - 0x100, at_1, false, not_on_it)
            .unwrap();
        assert_eq!(
            (switched.frame, switched.home),
            (at_1, Some(first_frame(1)))
        );
        assert_eq!(destination.running_home(), Some(first_frame(1)));
    }

    /// Every refusal of `lend`: a leaving frame, the boot or idle flow's, and frames no slot holds
    /// suspended (0, `BOOT_FLOW`, unknown, running, ended, already lent).
    #[test]
    fn a_lend_refuses_what_it_may_not_take() {
        let (source, destination, moves) = (Contexts::new(), Contexts::new(), Moves::new());
        let (boot_s, _) = two_processors(&source, &destination, 1);
        assert!(source.publish(first_frame(2)));
        assert!(source.publish(first_frame(3)));
        let moving = moves.begin();
        for frame in [0, BOOT_FLOW, first_frame(9)] {
            assert_eq!(
                source.lend(&moving, frame).map(|l| l.frame()),
                Err(LendRefused::NotHere)
            );
        }
        // One lent: a second lend of the same frame finds nothing.
        let lent = source.lend(&moving, first_frame(2)).unwrap();
        assert_eq!(
            source.lend(&moving, first_frame(2)).map(|l| l.frame()),
            Err(LendRefused::NotHere)
        );
        assert!(lent.restore());
        drop(moving);
        // Context 1 runs, context 3 ends: neither is suspended.
        assert_eq!(
            switch_to(&source, boot_s - 0x100, first_frame(1), false, not_on_it),
            Ok(first_frame(1))
        );
        let moving = moves.begin();
        assert_eq!(
            source.lend(&moving, first_frame(1)).map(|l| l.frame()),
            Err(LendRefused::NotHere),
            "running"
        );
        assert_eq!(
            source.lend(&moving, boot_s - 0x100).map(|l| l.frame()),
            Err(LendRefused::Leaving),
            "the boot flow's frame, leaving"
        );
        source.publish_leaving();
        assert_eq!(
            source.lend(&moving, boot_s - 0x100).map(|l| l.frame()),
            Err(LendRefused::BootFlow),
            "the boot flow's frame, published"
        );
        drop(moving);
        assert_eq!(
            switch_to(
                &source,
                first_frame(1) - 8,
                first_frame(3),
                false,
                not_on_it
            ),
            Ok(first_frame(3))
        );
        assert_eq!(
            switch_to(&source, first_frame(3) - 8, first_frame(2), true, not_on_it),
            Ok(first_frame(2))
        );
        let moving = moves.begin();
        assert_eq!(
            source.lend(&moving, first_frame(3) - 8).map(|l| l.frame()),
            Err(LendRefused::NotHere),
            "an exit saves no frame"
        );
        drop(moving);
        assert_eq!(moves.readings(), (3, 3));
    }

    /// N13: a full destination refuses the adoption, and the frame goes back to its source,
    /// which resumes it; a table never adopts what it lent.
    #[test]
    fn a_full_destination_refuses_and_the_source_gets_its_frame_back() {
        let (source, destination, moves) = (Contexts::new(), Contexts::new(), Moves::new());
        let (boot_s, _) = two_processors(&source, &destination, 1);
        while destination.publish(first_frame(40)) {}
        assert_eq!(
            moves.relocate(&source, &destination, first_frame(1), |_| ()),
            Err(MoveRefused::Adopt(AdoptRefused::Full))
        );
        assert_eq!(
            moves.relocate(&source, &source, first_frame(1), |_| ()),
            Err(MoveRefused::Adopt(AdoptRefused::SameTable))
        );
        assert!(covered(&source, 1));
        assert_eq!(moves.readings(), (2, 2));
        assert_eq!(
            switch_to(&source, boot_s - 0x100, first_frame(1), false, not_on_it),
            Ok(first_frame(1)),
            "restored, so the source resumes it"
        );
    }

    /// The witnesses by hand: `restore` puts a lent frame back; `forget` after `adopt` clears the
    /// source's home; a `restore` that finds the frame word taken reports it.
    #[test]
    fn restore_and_forget_settle_a_lent_frame() {
        let (source, destination, moves) = (Contexts::new(), Contexts::new(), Moves::new());
        two_processors(&source, &destination, 1);
        let moving = moves.begin();
        let lent = source.lend(&moving, first_frame(1)).unwrap();
        assert_eq!(
            (lent.home(), lent.frame()),
            (first_frame(1), first_frame(1))
        );
        assert!(covered(&source, 1), "the home stays while lent");
        assert!(!resumable(&source).contains(&first_frame(1)));
        assert!(lent.restore());
        assert!(resumable(&source).contains(&first_frame(1)));
        let lent = source.lend(&moving, first_frame(1)).unwrap();
        assert_eq!(destination.adopt(&moving, &lent), Ok(()));
        lent.forget();
        assert!(!covered(&source, 1));
        assert!(resumable(&destination).contains(&first_frame(1)));
        // A lent frame whose slot was written meanwhile, which nothing in the protocol does.
        let lent = destination.lend(&moving, first_frame(1)).unwrap();
        destination.slots[lent.slot].frame.store(0x77, Release);
        assert!(!lent.restore());
        drop(moving);
        assert_eq!(moves.readings(), (1, 1));
    }

    /// The coordinating session's ruling on Q1: dropping a `Moving`, also in a panic, raises
    /// finished and then frees the gate.
    #[test]
    fn a_move_that_panics_still_finishes_and_frees_the_gate() {
        let moves = Moves::new();
        let unwound = std::panic::catch_unwind(core::panic::AssertUnwindSafe(|| {
            let _moving = moves.begin();
            panic!("a panic under the gate");
        }));
        assert!(unwound.is_err());
        assert_eq!(moves.readings(), (1, 1));
        assert!(moves.gate.is_free(), "the gate is free");
    }

    /// ADR-0020 S4, the bound: a scan makes at most `FAST_TRIES` passes with no lock, then one
    /// under the gate. Context 1 is moved from table 1 to table 0 inside each of the first
    /// `misses` passes, between its reads of the two tables, and back up at the start of the
    /// next, so each of those passes misses it; the next pass finds it.
    fn scan_with_misses(misses: u32) -> Scanned {
        let tables = [Contexts::new(), Contexts::new()];
        let moves = Moves::new();
        two_processors(&tables[1], &tables[0], 1);
        let passes = core::cell::Cell::new(0u32);
        let scanned = moves.scan(
            || {
                passes.set(passes.get() + 1);
                // Never in the gated pass, which holds the gate a move would wait for.
                let moving = passes.get() <= misses.min(FAST_TRIES);
                if moving && covered(&tables[0], 1) {
                    assert!(
                        moves
                            .relocate(&tables[0], &tables[1], first_frame(1), |_| ())
                            .is_ok()
                    );
                }
                let in_0 = covered(&tables[0], 1);
                if moving {
                    assert!(
                        moves
                            .relocate(&tables[1], &tables[0], first_frame(1), |_| ())
                            .is_ok()
                    );
                }
                in_0 || covered(&tables[1], 1)
            },
            &Plain,
        );
        assert_eq!(scanned.passes, passes.get());
        scanned
    }

    #[test]
    fn a_scan_makes_its_fast_passes_then_one_under_the_gate() {
        assert_eq!(FAST_TRIES, 4);
        let scanned = scan_with_misses(FAST_TRIES);
        assert_eq!(
            scanned,
            Scanned {
                covered: true,
                passes: 5
            }
        );
        assert!(scanned.gated());
        // Fewer misses: found on the next fast pass.
        for misses in 0..FAST_TRIES {
            let scanned = scan_with_misses(misses);
            assert_eq!(
                scanned,
                Scanned {
                    covered: true,
                    passes: misses + 1
                }
            );
            assert!(!scanned.gated());
        }
    }

    /// A stack no table covers: believed after one pass with no move under way, and after the
    /// gated pass when every fast pass overlaps a move of another context.
    #[test]
    fn a_scan_believes_nothing_found_only_with_no_move_across_it() {
        let tables = [Contexts::new(), Contexts::new()];
        let moves = Moves::new();
        two_processors(&tables[1], &tables[0], 1);
        let asked = |_: ()| tables.iter().any(|t| covered(t, 5));
        assert_eq!(
            moves.scan(|| asked(()), &Plain),
            Scanned {
                covered: false,
                passes: 1
            }
        );
        let passes = core::cell::Cell::new(0u32);
        let scanned = moves.scan(
            || {
                passes.set(passes.get() + 1);
                if passes.get() <= FAST_TRIES {
                    let (from, to) = if covered(&tables[1], 1) {
                        (1, 0)
                    } else {
                        (0, 1)
                    };
                    assert!(
                        moves
                            .relocate(&tables[from], &tables[to], first_frame(1), |_| ())
                            .is_ok()
                    );
                }
                asked(())
            },
            &Plain,
        );
        assert_eq!(
            scanned,
            Scanned {
                covered: false,
                passes: FAST_TRIES + 1
            }
        );
        // A move under way when the pass began (finished behind begun) is not believed either,
        // nor one that began during it (begun changed).
        let readings = moves.readings();
        assert!(moves.settled(readings));
        let moving = moves.begin();
        assert!(!moves.settled(readings), "begun changed");
        assert!(!moves.settled(moves.readings()), "a move under way");
        drop(moving);
        assert!(moves.settled(moves.readings()));
    }

    #[test]
    fn the_move_types_name_no_address() {
        let (source, destination, moves) = (Contexts::new(), Contexts::new(), Moves::new());
        two_processors(&source, &destination, 1);
        assert_eq!(format!("{moves:?}"), "Moves");
        let moving = moves.begin();
        assert_eq!(format!("{moving:?}"), "Moving");
        let lent = source.lend(&moving, first_frame(1)).unwrap();
        assert_eq!(format!("{lent:?}"), "Lent");
        assert!(lent.restore());
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
