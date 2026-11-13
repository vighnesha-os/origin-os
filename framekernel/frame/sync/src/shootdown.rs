//! TLB shootdown, the protocol alone (CLAUDE.md §28; Intel SDM Vol. 3A §4.10.5, "Propagation
//! of Paging-Structure Changes to Multiple Processors"; N14 step 3, S3-5a).
//!
//! When a mapping loses access, every processor that may cache the old translation must drop it
//! before the memory is reused. A processor cannot be reached synchronously: it is told by an
//! interrupt, and the one that asked waits until each has answered.
//!
//! This is the protocol, generic over what it asks of a processor ([`TlbHardware`]), so that the
//! Loom models (`models.rs`) run this code. `origin_frame::tlb` instantiates it for the real
//! machine.
//!
//! # How a request travels
//!
//! - Requests go one at a time, under `turn`. A processor waiting for its turn serves the request
//!   in progress meanwhile, so two initiators never wait on each other.
//! - Each request has a generation. A processor records the last generation it served, and the
//!   initiator waits until every target's record reaches its own: a target has dropped the
//!   translation by the time it answers.
//! - The caller changes the page tables first. The initiator's change must be visible to whatever
//!   it is racing with before it reads who must answer: that is the `SeqCst` fence in
//!   [`Shootdown::shoot`], which pairs with those in [`Shootdown::mark_online`] and
//!   [`Shootdown::switch_space`].
//!
//! # Who must answer
//!
//! A kernel mapping is in every address space, so [`Scope::Kernel`] asks every online processor.
//! A user mapping is only in its own space, and these processors use no PCIDs, so a CR3 load
//! drops every non-global translation of the space it leaves: [`Scope::Space`] asks only the
//! processors that have the space loaded or are in the middle of leaving it. A processor that
//! has not reached the space yet needs nothing: its page walks start after its fence in
//! `switch_space`, so if the initiator read that it had not arrived, those walks see the change.
//!
//! # Freeing what was mapped
//!
//! A frame that has left the page tables can still be reached through a TLB. So it is freed by
//! [`Shootdown::retire`], which runs the release only after every processor has answered, and
//! never at all if one does not: a frame nobody may reuse is leaked, for good.
//!
//! A change to a user address space is made in rounds, each shot down before what it took out is
//! given back ([`Confirmation::change`]). A stall does not stop such a change half made: the
//! rounds after it are still made, and one flush of the whole space at the end decides whether
//! what they took out is given back or leaked.
//!
//! # What the caller must uphold
//!
//! It must hold no lock that another processor may wait for with interrupts disabled: that
//! processor could then never take the interrupt. And what the shootdown is for must have left
//! the page tables before the call.
#![forbid(unsafe_code)]

use core::sync::atomic::Ordering;

use crate::{SpinLock, prim};

/// The size of the page a request names.
pub const PAGE: u64 = 4096;
/// The most pages one request names one by one. Past this a full flush is the cheaper way.
pub const MAX_LISTED_PAGES: u64 = 32;

const PAGE_MASK: u64 = PAGE - 1;
/// How [`Request::Everything`] travels: an address no page request can have, because its low
/// bits are above `MAX_LISTED_PAGES`.
const EVERYTHING: u64 = u64::MAX;
/// How [`Request::NonGlobal`] travels; see [`EVERYTHING`].
const NON_GLOBAL: u64 = u64::MAX - 1;

const _: () = assert!(MAX_LISTED_PAGES <= PAGE_MASK && MAX_LISTED_PAGES >= 1);

/// What a processor is asked to drop from its TLB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// The translations of `pages` consecutive 4 KiB pages from `base`, global ones included.
    /// `base` is page-aligned, and `pages` is at least 1 and at most [`MAX_LISTED_PAGES`].
    Pages {
        /// The first page.
        base: u64,
        /// How many.
        pages: u64,
    },
    /// Every translation that is not global: what a CR3 reload drops. Enough for a user mapping.
    NonGlobal,
    /// Every translation, global ones included: for a change to many kernel mappings at once.
    Everything,
}

impl Request {
    /// The page holding `addr`.
    #[must_use]
    pub const fn page(addr: u64) -> Self {
        Self::Pages {
            base: addr & !PAGE_MASK,
            pages: 1,
        }
    }

    /// The page holding `first`, the page holding `last` and everything between them, or a full
    /// flush of non-global translations if that is more than [`MAX_LISTED_PAGES`] pages or
    /// `last` comes before `first`.
    #[must_use]
    pub const fn covering(first: u64, last: u64) -> Self {
        let base = first & !PAGE_MASK;
        let end = last & !PAGE_MASK;
        if end < base {
            return Self::NonGlobal;
        }
        let pages = (end - base) / PAGE + 1;
        if pages > MAX_LISTED_PAGES {
            return Self::NonGlobal;
        }
        Self::Pages { base, pages }
    }

    /// The request as one word, so that it is published by one store. Anything `decode` could
    /// not give back the same is made a full flush, which drops at least as much.
    const fn encode(self) -> u64 {
        match self {
            Self::Pages { base, pages }
                if pages >= 1 && pages <= MAX_LISTED_PAGES && base & PAGE_MASK == 0 =>
            {
                base | (pages - 1)
            }
            Self::Pages { .. } | Self::Everything => EVERYTHING,
            Self::NonGlobal => NON_GLOBAL,
        }
    }

    /// The request [`encode`](Self::encode) made into `raw`.
    const fn decode(raw: u64) -> Self {
        match raw {
            EVERYTHING => Self::Everything,
            NON_GLOBAL => Self::NonGlobal,
            _ => Self::Pages {
                base: raw & !PAGE_MASK,
                pages: (raw & PAGE_MASK) + 1,
            },
        }
    }
}

/// What the protocol needs from a processor, in the calling processor's view.
pub trait TlbHardware {
    /// This processor's index into the protocol's per-processor tables.
    fn this_cpu(&self) -> usize;

    /// Drops what `request` names from this processor's TLB.
    fn invalidate(&self, request: Request);

    /// Interrupts processor `cpu`, whose handler calls [`Shootdown::service`].
    fn send_ipi(&self, cpu: usize);

    /// Runs `f` with interrupts disabled on this processor.
    fn without_interrupts<R>(&self, f: impl FnOnce() -> R) -> R;

    /// The root of the address space this processor runs on now (CR3).
    fn current_root(&self) -> u64;

    /// A clock for [`TlbHardware::patience`].
    fn now(&self) -> u64;

    /// How much of [`TlbHardware::now`] an initiator waits for one processor's answer.
    fn patience(&self) -> u64;
}

/// Which processors a request is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Every online processor: a kernel mapping changed.
    Kernel,
    /// The processors that have the address space with this root loaded: one of its user
    /// mappings changed.
    Space(u64),
}

/// A completed shootdown: every processor it was for has dropped the translations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Completed {
    /// How many other processors had to answer, each by an interrupt.
    pub processors: u32,
    /// How much of [`TlbHardware::now`] it took, waiting for the turn excluded.
    pub ticks: u64,
}

/// A processor did not answer in time: some translation may still be cached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stalled {
    /// The first processor that did not answer.
    pub cpu: usize,
}

/// Whether every change to one address space is known to have reached every processor
/// (review finding 3). After a stalled shootdown some processor may still hold a translation the
/// tables no longer give; a retry that finds nothing left to change would send nothing and report
/// success. So a stall is remembered, and the next change starts with a flush of the whole space,
/// and the mark goes only when such a flush completes. The caller serializes changes to the space.
#[derive(Debug)]
pub struct Confirmation {
    unconfirmed: prim::AtomicBool,
}

impl Confirmation {
    /// Nothing unconfirmed.
    #[cfg(not(all(test, loom)))]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            unconfirmed: prim::AtomicBool::new(false),
        }
    }

    /// Nothing unconfirmed. Loom's atomics cannot be built in a `const`.
    #[cfg(all(test, loom))]
    pub fn new() -> Self {
        Self {
            unconfirmed: prim::AtomicBool::new(false),
        }
    }

    /// Whether a stalled shootdown has not been made good yet.
    pub fn is_unconfirmed(&self) -> bool {
        self.unconfirmed.load(Ordering::Relaxed)
    }

    /// Before a change: if a shootdown stalled, runs `flush` (every translation of the space,
    /// on every processor in it), and clears the mark only if that completes.
    pub fn settle(
        &self,
        flush: impl FnOnce() -> Result<Completed, Stalled>,
    ) -> Result<Option<Completed>, Stalled> {
        if !self.is_unconfirmed() {
            return Ok(None);
        }
        self.flush(flush).map(Some)
    }

    /// Runs `flush`, and clears the mark only if it completes.
    fn flush(
        &self,
        flush: impl FnOnce() -> Result<Completed, Stalled>,
    ) -> Result<Completed, Stalled> {
        let done = self.record(flush())?;
        self.unconfirmed.store(false, Ordering::Relaxed);
        Ok(done)
    }

    /// A shootdown's outcome: a stall is remembered.
    pub fn record(&self, done: Result<Completed, Stalled>) -> Result<Completed, Stalled> {
        if done.is_err() {
            self.unconfirmed.store(true, Ordering::Relaxed);
        }
        done
    }

    /// Makes the change `rounds` describes to the space this is for, round by round, with
    /// `shoot` as each round's shootdown (S3-5b review, finding A). The caller serializes the
    /// changes to the space, as for [`settle`](Self::settle).
    ///
    /// A stall never leaves the change half made: every round is still made, so the tables end
    /// as the change asks. Until the first stall, each round is shot down and then what it took
    /// out is given back. After it, the rounds keep what they take without a shootdown of their
    /// own, and one flush of the whole space ends the change. If that flush completes,
    /// everything kept is given back, the space is confirmed again and the change succeeds. If
    /// it stalls too, what is kept is lost for good, the space stays unconfirmed and the change
    /// returns [`Stalled`]. A stall that an earlier change left is met the same way: the flush
    /// that makes it good comes first, and if that stalls, the change goes on as after a stall
    /// of its own.
    ///
    /// # Errors
    ///
    /// - [`Stalled`] (as `R::Error`) only if the flush at the end stalled: the change is made,
    ///   and what it took out after the first stall is lost.
    /// - A refusal from [`Rounds::prepare`], before anything changed.
    /// - A refusal from [`Rounds::round`]: the rounds before it are made, and what they kept is
    ///   given back or lost as above.
    pub fn change<R: Rounds>(
        &self,
        rounds: &mut R,
        mut shoot: impl FnMut(Request) -> Result<Completed, Stalled>,
    ) -> Result<Cost, R::Error> {
        let mut cost = Cost::default();
        // A space's own translations are never global, so this flush drops every one of them.
        let mut stalled = match self.settle(|| shoot(Request::NonGlobal)) {
            Ok(flushed) => {
                if let Some(done) = flushed {
                    cost.add(done);
                    rounds.flushed();
                }
                false
            }
            Err(_) => true,
        };
        rounds.prepare()?;
        let refused = loop {
            match rounds.round() {
                Ok(Some(request)) if !stalled => match self.record(shoot(request)) {
                    Ok(done) => {
                        cost.add(done);
                        rounds.shot();
                    }
                    // Kept, with what every later round takes, until the flush at the end.
                    Err(_) => stalled = true,
                },
                Ok(Some(_)) => {}
                Ok(None) => break None,
                Err(e) => break Some(e),
            }
        };
        if stalled {
            match self.flush(|| shoot(Request::NonGlobal)) {
                Ok(done) => {
                    cost.add(done);
                    rounds.flushed();
                }
                Err(stall) => {
                    rounds.lost();
                    return Err(refused.unwrap_or_else(|| stall.into()));
                }
            }
        }
        refused.map_or(Ok(cost), Err)
    }
}

impl Default for Confirmation {
    fn default() -> Self {
        Self::new()
    }
}

/// What the completed shootdowns of a change cost: how many there were, and the interrupts they
/// sent to other processors between them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cost {
    /// The shootdowns that completed.
    pub shootdowns: u32,
    /// The interrupts those sent.
    pub interrupts: u32,
}

impl Cost {
    fn add(&mut self, done: Completed) {
        self.shootdowns = self.shootdowns.saturating_add(1);
        self.interrupts = self.interrupts.saturating_add(done.processors);
    }
}

/// A change to one address space, made in rounds by [`Confirmation::change`]. Each round takes
/// something out of the space's page tables, or out of what they allow, and keeps it until no
/// processor can still use it through a translation it cached.
pub trait Rounds {
    /// Why the change could not be made, [`Stalled`] among the reasons.
    type Error: From<Stalled>;

    /// Runs once before the first round, after the flush that makes an earlier stall good has
    /// completed or stalled.
    ///
    /// # Errors
    ///
    /// A refusal of the whole change, which must have changed nothing.
    fn prepare(&mut self) -> Result<(), Self::Error>;

    /// Makes the next round of the change in the tables and keeps what it took out. Returns the
    /// translations that may still lead to it, or `None` once the change is complete.
    ///
    /// # Errors
    ///
    /// A round that could not be made, which must have kept nothing.
    fn round(&mut self) -> Result<Option<Request>, Self::Error>;

    /// The shootdown of the round just made has completed: give back what that round took out.
    fn shot(&mut self);

    /// A flush of every translation of the space has completed: give back everything still
    /// kept, by this change or by an earlier one.
    fn flushed(&mut self);

    /// What the change still keeps may stay in some processor's TLB for good: never give it
    /// back.
    fn lost(&mut self);
}

/// One processor's part in the protocol, on a cache line of its own (ADR-0008 Amendment 1, D1):
/// the processor writes everything here but `online`, and no other processor's writes share the
/// line.
#[repr(align(64))]
#[derive(Debug)]
pub struct Record {
    /// The last generation this processor served.
    served: prim::AtomicU64,
    /// Whether it takes requests.
    online: prim::AtomicBool,
    /// Its current address-space root (0 before its first switch).
    loaded: prim::AtomicU64,
    /// The root it is switching away from, while its switch is in flight (else 0).
    leaving: prim::AtomicU64,
    /// The root it is visiting, without leaving `loaded` (else 0): see [`Shootdown::visit`].
    visiting: prim::AtomicU64,
}

impl Record {
    /// Not online, nothing served and no space loaded.
    #[cfg(not(all(test, loom)))]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            served: prim::AtomicU64::new(0),
            online: prim::AtomicBool::new(false),
            loaded: prim::AtomicU64::new(0),
            leaving: prim::AtomicU64::new(0),
            visiting: prim::AtomicU64::new(0),
        }
    }

    /// Not online, nothing served and no space loaded. Loom's atomics cannot be built in a
    /// `const`.
    #[cfg(all(test, loom))]
    pub fn new() -> Self {
        Self {
            served: prim::AtomicU64::new(0),
            online: prim::AtomicBool::new(false),
            loaded: prim::AtomicU64::new(0),
            leaving: prim::AtomicU64::new(0),
            visiting: prim::AtomicU64::new(0),
        }
    }
}

impl Default for Record {
    fn default() -> Self {
        Self::new()
    }
}

/// The initiator's mark for one processor: whether the request in progress asked it. Only the
/// processor holding the turn reads or writes a mark, and it clears every mark it set before it
/// gives the turn up. The turn is the lock that orders one holder's marks before the next's, so a
/// mark is a plain atomic even under Loom: it is no point the models need to interleave, and the
/// turn itself is modelled.
#[derive(Debug)]
pub struct Asked(core::sync::atomic::AtomicBool);

impl Asked {
    /// Not asked.
    #[must_use]
    pub const fn new() -> Self {
        Self(core::sync::atomic::AtomicBool::new(false))
    }
}

impl Default for Asked {
    fn default() -> Self {
        Self::new()
    }
}

/// Where a [`Shootdown`] keeps what it holds for each processor: storage for a number of
/// processors fixed once, before any processor but the first is online (ADR-0008 Amendment 1,
/// D1). The kernel's is sized when the processors are discovered; the tests' and the models'
/// are vectors.
pub trait Records {
    /// How many processors there is storage for: indexes `0..count()`.
    fn count(&self) -> usize;

    /// Processor `cpu`'s record: `Some` for every `cpu` below [`Records::count`].
    fn record(&self, cpu: usize) -> Option<&Record>;

    /// Processor `cpu`'s mark: `Some` for every `cpu` below [`Records::count`].
    fn asked(&self, cpu: usize) -> Option<&Asked>;
}

/// The shootdown state of a machine, its per-processor part in `S`.
pub struct Shootdown<S> {
    /// Held by the processor whose request is in progress.
    turn: SpinLock<()>,
    /// The request in progress ([`Request::encode`]).
    request: prim::AtomicU64,
    /// The latest request's generation (the first is 1).
    generation: prim::AtomicU64,
    /// Each processor's [`Record`] and [`Asked`] mark.
    records: S,
}

impl<S> core::fmt::Debug for Shootdown<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Shootdown").finish_non_exhaustive()
    }
}

impl<S> Shootdown<S> {
    /// No processor online, and no request made, with each processor's state in `records`.
    #[cfg(not(all(test, loom)))]
    #[must_use]
    pub const fn new(records: S) -> Self {
        Self {
            turn: SpinLock::new(()),
            request: prim::AtomicU64::new(0),
            generation: prim::AtomicU64::new(0),
            records,
        }
    }

    /// No processor online, and no request made, with each processor's state in `records`.
    /// Loom's atomics cannot be built in a `const`.
    #[cfg(all(test, loom))]
    pub fn new(records: S) -> Self {
        Self {
            turn: SpinLock::new(()),
            request: prim::AtomicU64::new(0),
            generation: prim::AtomicU64::new(0),
            records,
        }
    }

    /// Where each processor's state is.
    pub const fn records(&self) -> &S {
        &self.records
    }
}

impl<S: Records> Shootdown<S> {
    /// The last generation processor `id` served.
    fn served(&self, id: usize) -> Option<&prim::AtomicU64> {
        self.records.record(id).map(|r| &r.served)
    }

    /// Whether processor `id` takes requests.
    fn online(&self, id: usize) -> Option<&prim::AtomicBool> {
        self.records.record(id).map(|r| &r.online)
    }

    /// Processor `id`'s current root.
    fn loaded(&self, id: usize) -> Option<&prim::AtomicU64> {
        self.records.record(id).map(|r| &r.loaded)
    }

    /// The root processor `id` is leaving.
    fn leaving(&self, id: usize) -> Option<&prim::AtomicU64> {
        self.records.record(id).map(|r| &r.leaving)
    }

    /// The root processor `id` is visiting.
    fn visiting(&self, id: usize) -> Option<&prim::AtomicU64> {
        self.records.record(id).map(|r| &r.visiting)
    }

    /// Makes this processor a target of later requests, then flushes its whole TLB, so it holds
    /// no translation older than the first request it is sent. In that order: a request whose
    /// initiator does not yet see this processor online was made before the flush.
    pub fn mark_online(&self, hardware: &impl TlbHardware) {
        if let Some(online) = self.online(hardware.this_cpu()) {
            online.store(true, Ordering::Release);
        }
        prim::fence(Ordering::SeqCst);
        hardware.invalidate(Request::Everything);
    }

    /// Moves this processor to the address space `to`, by running `load` (the CR3 write), and
    /// keeps the record of it; nothing happens if `to` is what it runs on already. Both spaces
    /// count as loaded while the switch is in flight: the old one until the processor has really
    /// left it, the new one from before it arrives.
    ///
    /// All of it with interrupts off, the processor and the root it leaves read inside: an
    /// interrupt between the record and the CR3 write could switch this processor itself (the
    /// scheduler does, on a timer tick), and the write resumed after it would leave the record
    /// naming a space the processor no longer runs on, so that a shootdown passed it over and a
    /// space could be freed under it (review finding 1).
    ///
    /// The fence is what lets an initiator skip a processor that has not reached the space it
    /// is changing: this processor's page walks start after the fence, so if the initiator
    /// read the old `loaded`, its own fence came first, and the walks see its change.
    pub fn switch_space(&self, hardware: &impl TlbHardware, to: u64, load: impl FnOnce()) {
        hardware.without_interrupts(|| self.switch_here(hardware, to, load));
    }

    /// [`switch_space`](Self::switch_space)'s work, with interrupts already off.
    fn switch_here(&self, hardware: &impl TlbHardware, to: u64, load: impl FnOnce()) {
        let me = hardware.this_cpu();
        let from = hardware.current_root();
        if from == to {
            return;
        }
        if let Some(leaving) = self.leaving(me) {
            leaving.store(from, Ordering::Release);
        }
        if let Some(loaded) = self.loaded(me) {
            loaded.store(to, Ordering::Release);
        }
        prim::fence(Ordering::SeqCst);
        load();
        if let Some(leaving) = self.leaving(me) {
            leaving.store(0, Ordering::Release);
        }
    }

    /// Runs `f` on this processor with the address space `visiting` loaded, then puts back the
    /// space it was on, with interrupts off throughout: nothing can switch it meanwhile. `load`
    /// is the CR3 write, run once each way. For a visit that must not outlast the call, such as
    /// a proof's read through another space's tables (review finding 2).
    ///
    /// The visit is recorded in `visiting` alone, from before its CR3 write until after the one
    /// that ends it, and `loaded` keeps naming the space the processor returns to: each space
    /// is in one field for as long as the processor can reach it, whatever order a reader reads
    /// the fields in. A shootdown for the visited space waits for the visit to end, since the
    /// processor answers only with interrupts on again.
    pub fn visit<R>(
        &self,
        hardware: &impl TlbHardware,
        visiting: u64,
        load: impl Fn(u64),
        f: impl FnOnce() -> R,
    ) -> R {
        hardware.without_interrupts(|| {
            let home = hardware.current_root();
            if home == visiting {
                return f();
            }
            let slot = self.visiting(hardware.this_cpu());
            if let Some(slot) = slot {
                slot.store(visiting, Ordering::Release);
            }
            prim::fence(Ordering::SeqCst);
            load(visiting);
            let result = f();
            load(home);
            if let Some(slot) = slot {
                slot.store(0, Ordering::Release);
            }
            result
        })
    }

    /// Whether processor `id` has `root` loaded, is in the middle of leaving it, or is visiting
    /// it. `loaded` is read before `leaving`, the reverse of the order [`switch_space`] writes
    /// them, so a processor in the middle of a switch counts as holding both.
    ///
    /// [`switch_space`]: Self::switch_space
    fn holds(&self, id: usize, root: u64) -> bool {
        self.loaded(id)
            .is_some_and(|loaded| loaded.load(Ordering::Acquire) == root)
            || self
                .leaving(id)
                .is_some_and(|leaving| leaving.load(Ordering::Acquire) == root)
            || self
                .visiting(id)
                .is_some_and(|visiting| visiting.load(Ordering::Acquire) == root)
    }

    /// How many processors have the address space `root` loaded.
    pub fn processors_holding(&self, root: u64) -> u32 {
        (0..self.records.count())
            .filter(|&id| self.holds(id, root))
            .count() as u32
    }

    /// Serves the request in progress, if this processor has not yet. Called from the
    /// shootdown interrupt, and by an initiator waiting for its turn.
    pub fn service(&self, hardware: &impl TlbHardware) {
        // With interrupts off, so that the record below only ever goes forward: a request's
        // interrupt arriving between the comparison and the store would otherwise be overwritten.
        hardware.without_interrupts(|| {
            let Some(served) = self.served(hardware.this_cpu()) else {
                return;
            };
            // Acquire: the request read below is the one this generation published (its
            // initiator stored it before the generation, and the next initiator cannot start
            // until every target, this processor included, has served this one).
            let generation = self.generation.load(Ordering::Acquire);
            if served.load(Ordering::Relaxed) < generation {
                hardware.invalidate(Request::decode(self.request.load(Ordering::Relaxed)));
                // Release: the initiator that sees this may free what the translation led to.
                served.store(generation, Ordering::Release);
            }
        });
    }

    /// Whether processor `id` must answer a request for `scope` made now.
    fn must_answer(&self, id: usize, scope: Scope) -> bool {
        self.online(id)
            .is_some_and(|online| online.load(Ordering::Acquire))
            && match scope {
                Scope::Kernel => true,
                Scope::Space(root) => self.holds(id, root),
            }
    }

    /// Drops what `request` names from this processor's TLB and from every other processor
    /// `scope` covers, and waits until each has.
    ///
    /// The caller changes the page tables first. On [`Stalled`] a processor may still hold the
    /// translation: what it maps must then never be reused.
    pub fn shoot(
        &self,
        hardware: &impl TlbHardware,
        request: Request,
        scope: Scope,
    ) -> Result<Completed, Stalled> {
        hardware.without_interrupts(|| {
            let turn = loop {
                if let Some(turn) = self.turn.try_lock() {
                    break turn;
                }
                self.service(hardware);
                prim::spin_loop();
            };
            let start = hardware.now();
            hardware.invalidate(request);
            let me = hardware.this_cpu();
            self.request.store(request.encode(), Ordering::Relaxed);
            let generation = self.generation.fetch_add(1, Ordering::Release) + 1;
            // The caller's page-table change and this request must be visible before anyone is
            // ruled out: a processor not yet online flushes after this, and one not yet in the
            // space walks its tables after this (`mark_online`, `switch_space`).
            prim::fence(Ordering::SeqCst);
            let count = self.records.count();
            let mut processors = 0;
            // A processor that must answer and has no mark could not be waited for: a stall,
            // never a skip.
            let mut stalled = None;
            for id in 0..count {
                if id != me && self.must_answer(id, scope) {
                    let Some(asked) = self.records.asked(id) else {
                        stalled.get_or_insert(Stalled { cpu: id });
                        continue;
                    };
                    asked.0.store(true, Ordering::Relaxed);
                    processors += 1;
                    hardware.send_ipi(id);
                }
            }
            // Every mark set above is cleared here, the processor that stalled and those after it
            // included, so the next initiator starts with none.
            let patience = hardware.patience();
            for target in 0..count {
                let Some(asked) = self.records.asked(target) else {
                    continue;
                };
                if !asked.0.load(Ordering::Relaxed) {
                    continue;
                }
                asked.0.store(false, Ordering::Relaxed);
                let Some(served) = self.served(target) else {
                    stalled.get_or_insert(Stalled { cpu: target });
                    continue;
                };
                if stalled.is_some() {
                    continue;
                }
                while served.load(Ordering::Acquire) < generation {
                    if hardware.now().wrapping_sub(start) >= patience
                        // Looked at once more: this processor may have been the one away.
                        && served.load(Ordering::Acquire) < generation
                    {
                        stalled = Some(Stalled { cpu: target });
                        break;
                    }
                    prim::spin_loop();
                }
            }
            drop(turn);
            match stalled {
                Some(stall) => Err(stall),
                None => Ok(Completed {
                    processors,
                    ticks: hardware.now().wrapping_sub(start),
                }),
            }
        })
    }
}

impl<S: Records> Shootdown<S> {
    /// The deferred free: `value` is what the caller has just taken out of the page tables
    /// (`request` and `scope` say which translations can still lead to it), and `release` gets it
    /// back only once no processor can reach it any more.
    ///
    /// On [`Stalled`] a processor may still hold a translation: `release` does not run, and
    /// `value` is forgotten. What it owns stays allocated for good, which is the only way to be
    /// sure it is not reused.
    pub fn retire<T>(
        &self,
        hardware: &impl TlbHardware,
        value: T,
        request: Request,
        scope: Scope,
        release: impl FnOnce(T),
    ) -> Result<Completed, Stalled> {
        match self.shoot(hardware, request, scope) {
            Ok(done) => {
                release(value);
                Ok(done)
            }
            Err(stalled) => {
                core::mem::forget(value);
                Err(stalled)
            }
        }
    }
}

/// Records in vectors, for the host tests and the Loom models: a machine whose number of
/// processors is known only at run time.
#[cfg(test)]
pub(crate) struct Held {
    records: alloc::vec::Vec<Record>,
    asked: alloc::vec::Vec<Asked>,
}

#[cfg(test)]
impl Held {
    /// Storage for `count` processors, none online.
    pub(crate) fn new(count: usize) -> Self {
        Self::with_marks(count, count)
    }

    /// Storage for `count` processors with marks for only the first `marks`: storage that breaks
    /// [`Records`]' contract, to show that a processor with no mark is a stall, never a skip.
    pub(crate) fn with_marks(count: usize, marks: usize) -> Self {
        Self {
            records: (0..count).map(|_| Record::new()).collect(),
            asked: (0..marks).map(|_| Asked::new()).collect(),
        }
    }
}

#[cfg(test)]
impl Records for Held {
    fn count(&self) -> usize {
        self.records.len()
    }

    fn record(&self, cpu: usize) -> Option<&Record> {
        self.records.get(cpu)
    }

    fn asked(&self, cpu: usize) -> Option<&Asked> {
        self.asked.get(cpu)
    }
}

// Not under Loom: its atomics exist only inside a model (`models.rs` has this protocol's models).
#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
    use std::thread;

    /// One processor of a test machine: `invalidated` records every request it handled.
    struct Cpu<'a> {
        id: usize,
        machine: &'a Machine,
    }

    #[derive(Default)]
    struct Machine {
        invalidated: Mutex<Vec<(usize, Request)>>,
        ipis: Mutex<Vec<(usize, usize)>>,
        clock: AtomicU64,
        ticking: AtomicBool,
        /// Each processor's CR3.
        cr3: [AtomicU64; 8],
    }

    impl TlbHardware for Cpu<'_> {
        fn this_cpu(&self) -> usize {
            self.id
        }

        fn invalidate(&self, request: Request) {
            self.machine
                .invalidated
                .lock()
                .unwrap()
                .push((self.id, request));
        }

        fn send_ipi(&self, cpu: usize) {
            self.machine.ipis.lock().unwrap().push((self.id, cpu));
        }

        fn without_interrupts<R>(&self, f: impl FnOnce() -> R) -> R {
            f()
        }

        fn now(&self) -> u64 {
            if self.machine.ticking.load(Ordering::Relaxed) {
                self.machine.clock.fetch_add(1, Ordering::Relaxed)
            } else {
                self.machine.clock.load(Ordering::Relaxed)
            }
        }

        fn patience(&self) -> u64 {
            1000
        }

        fn current_root(&self) -> u64 {
            self.machine.cr3[self.id].load(Ordering::Relaxed)
        }
    }

    fn cpu(id: usize, machine: &Machine) -> Cpu<'_> {
        Cpu { id, machine }
    }

    /// Switches `cpu` to `to` through `shootdown`, the CR3 write running `during` too.
    fn switch(shootdown: &Shootdown<Held>, cpu: &Cpu<'_>, to: u64, during: impl FnOnce()) {
        shootdown.switch_space(cpu, to, || {
            cpu.machine.cr3[cpu.id].store(to, Ordering::Relaxed);
            during();
        });
    }

    #[test]
    fn a_request_is_one_word_and_comes_back_the_same() {
        for request in [
            Request::Everything,
            Request::NonGlobal,
            Request::page(0x40_0000),
            Request::page(0xFFFF_FFFF_8000_0FFF),
            Request::Pages {
                base: 0x7FFF_FFFF_F000,
                pages: MAX_LISTED_PAGES,
            },
        ] {
            assert_eq!(Request::decode(request.encode()), request);
        }
        // The two flushes never collide with a page request, whatever its base.
        for pages in 1..=MAX_LISTED_PAGES {
            let raw = Request::Pages {
                base: !PAGE_MASK,
                pages,
            }
            .encode();
            assert_ne!(raw, EVERYTHING);
            assert_ne!(raw, NON_GLOBAL);
        }
    }

    #[test]
    fn a_request_that_cannot_be_named_flushes_everything() {
        for bad in [
            Request::Pages {
                base: 0x1000,
                pages: 0,
            },
            Request::Pages {
                base: 0x1000,
                pages: MAX_LISTED_PAGES + 1,
            },
            Request::Pages {
                base: 0x1234,
                pages: 1,
            },
        ] {
            assert_eq!(Request::decode(bad.encode()), Request::Everything);
        }
    }

    #[test]
    fn covering_names_the_span_and_gives_up_past_the_limit() {
        assert_eq!(Request::covering(0x1000, 0x1000), Request::page(0x1000));
        assert_eq!(
            Request::covering(0x1234, 0x3FFF),
            Request::Pages {
                base: 0x1000,
                pages: 3,
            }
        );
        let last = 0x1000 + (MAX_LISTED_PAGES - 1) * PAGE;
        assert_eq!(
            Request::covering(0x1000, last),
            Request::Pages {
                base: 0x1000,
                pages: MAX_LISTED_PAGES,
            }
        );
        assert_eq!(Request::covering(0x1000, last + PAGE), Request::NonGlobal);
        assert_eq!(Request::covering(0x3000, 0x1000), Request::NonGlobal);
        assert_eq!(
            Request::covering(0x7FFF_FFFF_F000, 0x7FFF_FFFF_FFFF),
            Request::page(0x7FFF_FFFF_F000)
        );
    }

    #[test]
    fn with_no_other_processor_it_is_local_and_needs_nobody() {
        let machine = Machine::default();
        let shootdown = Shootdown::new(Held::new(4));
        shootdown.mark_online(&cpu(0, &machine));
        let done = shootdown
            .shoot(&cpu(0, &machine), Request::page(0x1000), Scope::Kernel)
            .unwrap();
        assert_eq!(done.processors, 0);
        assert!(machine.ipis.lock().unwrap().is_empty());
        assert_eq!(
            *machine.invalidated.lock().unwrap(),
            [(0, Request::Everything), (0, Request::page(0x1000))]
        );
    }

    /// A processor marks itself a target before it flushes: otherwise a request made between
    /// the two would find it neither flushed nor listening.
    #[test]
    fn a_processor_comes_online_before_its_first_flush() {
        struct Watching<'a> {
            shootdown: &'a Shootdown<Held>,
            saw_online: &'a AtomicBool,
        }
        impl TlbHardware for Watching<'_> {
            fn this_cpu(&self) -> usize {
                1
            }
            fn invalidate(&self, _: Request) {
                let online = self.shootdown.must_answer(1, Scope::Kernel);
                self.saw_online.store(online, Ordering::Relaxed);
            }
            fn send_ipi(&self, _: usize) {}
            fn without_interrupts<R>(&self, f: impl FnOnce() -> R) -> R {
                f()
            }
            fn now(&self) -> u64 {
                0
            }
            fn patience(&self) -> u64 {
                1
            }
            fn current_root(&self) -> u64 {
                0
            }
        }
        let shootdown = Shootdown::new(Held::new(2));
        let saw_online = AtomicBool::new(false);
        shootdown.mark_online(&Watching {
            shootdown: &shootdown,
            saw_online: &saw_online,
        });
        assert!(saw_online.load(Ordering::Relaxed));
    }

    #[test]
    fn every_online_processor_answers_and_the_rest_are_left_alone() {
        let machine = Machine::default();
        let shootdown = Shootdown::new(Held::new(4));
        for id in [0, 1, 2] {
            shootdown.mark_online(&cpu(id, &machine));
        }
        let stop = AtomicBool::new(false);
        let done = thread::scope(|s| {
            for id in [1, 2] {
                let (shootdown, machine, stop) = (&shootdown, &machine, &stop);
                s.spawn(move || {
                    let me = cpu(id, machine);
                    while !stop.load(Ordering::Relaxed) {
                        shootdown.service(&me);
                        thread::yield_now();
                    }
                });
            }
            let done = shootdown.shoot(&cpu(0, &machine), Request::page(0x5000), Scope::Kernel);
            stop.store(true, Ordering::Relaxed);
            done
        })
        .unwrap();
        assert_eq!(done.processors, 2);
        let invalidated = machine.invalidated.lock().unwrap();
        for id in [0, 1, 2] {
            assert!(
                invalidated.contains(&(id, Request::page(0x5000))),
                "CPU {id}"
            );
        }
        assert!(
            !invalidated.iter().any(|(id, _)| *id == 3),
            "CPU 3 is offline"
        );
        assert_eq!(*machine.ipis.lock().unwrap(), [(0, 1), (0, 2)]);
    }

    #[test]
    fn a_processor_that_does_not_answer_is_named() {
        let machine = Machine::default();
        machine.ticking.store(true, Ordering::Relaxed);
        let shootdown = Shootdown::new(Held::new(3));
        shootdown.mark_online(&cpu(0, &machine));
        shootdown.mark_online(&cpu(2, &machine));
        let outcome = shootdown.shoot(&cpu(0, &machine), Request::NonGlobal, Scope::Kernel);
        assert_eq!(outcome, Err(Stalled { cpu: 2 }));
        // The turn is given back, so the next request can be made.
        let next = shootdown.shoot(&cpu(2, &machine), Request::Everything, Scope::Kernel);
        assert_eq!(next, Err(Stalled { cpu: 0 }));
    }

    /// Two processors shooting at each other each serve the other's request while they wait
    /// for their turn: neither stalls.
    #[test]
    fn initiators_never_wait_on_each_other() {
        const ROUNDS: u64 = 200;
        let machine = Machine::default();
        let shootdown = Shootdown::new(Held::new(3));
        for id in 0..3 {
            shootdown.mark_online(&cpu(id, &machine));
        }
        let finished = AtomicUsize::new(0);
        thread::scope(|s| {
            for id in 0..3 {
                let (shootdown, machine, finished) = (&shootdown, &machine, &finished);
                s.spawn(move || {
                    let me = cpu(id, machine);
                    for round in 0..ROUNDS {
                        // A test machine has no interrupts: each processor answers between
                        // its own requests, as a real one would in its handler.
                        shootdown.service(&me);
                        shootdown
                            .shoot(&me, Request::page(0x1000 * (round + 1)), Scope::Kernel)
                            .unwrap();
                    }
                    finished.fetch_add(1, Ordering::Relaxed);
                    while finished.load(Ordering::Relaxed) < 3 {
                        shootdown.service(&me);
                        thread::yield_now();
                    }
                });
            }
        });
        assert_eq!(finished.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn a_switch_counts_as_holding_both_spaces_until_it_is_done() {
        let machine = Machine::default();
        let shootdown = Shootdown::new(Held::new(3));
        let (one, two) = (cpu(1, &machine), cpu(2, &machine));
        assert_eq!(shootdown.processors_holding(7), 0);
        switch(&shootdown, &one, 7, || {
            assert_eq!(shootdown.processors_holding(7), 1, "arriving");
        });
        assert_eq!(shootdown.processors_holding(7), 1);
        switch(&shootdown, &one, 9, || {
            assert_eq!(shootdown.processors_holding(7), 1, "leaving");
            assert_eq!(shootdown.processors_holding(9), 1, "arriving");
        });
        assert_eq!(shootdown.processors_holding(7), 0, "gone");
        assert_eq!(shootdown.processors_holding(9), 1);
        // A second processor in the same space.
        switch(&shootdown, &two, 9, || ());
        assert_eq!(shootdown.processors_holding(9), 2);
        // An index past the table counts nothing, and still loads.
        let mut ran = false;
        switch(&shootdown, &cpu(5, &machine), 2, || ran = true);
        assert!(ran);
        assert_eq!(shootdown.processors_holding(2), 0);
    }

    /// A visit is recorded while it lasts, puts the processor back, and leaves `loaded` alone.
    #[test]
    fn a_visit_is_recorded_while_it_lasts_and_ends_where_it_began() {
        let machine = Machine::default();
        let shootdown = Shootdown::new(Held::new(2));
        let one = cpu(1, &machine);
        switch(&shootdown, &one, 7, || ());
        let load = |to: u64| machine.cr3[1].store(to, Ordering::Relaxed);
        let seen = shootdown.visit(&one, 9, load, || {
            (
                machine.cr3[1].load(Ordering::Relaxed),
                shootdown.processors_holding(9),
                shootdown.processors_holding(7),
            )
        });
        assert_eq!(seen, (9, 1, 1), "on the visited space, both recorded");
        assert_eq!(
            machine.cr3[1].load(Ordering::Relaxed),
            7,
            "back where it was"
        );
        assert_eq!(shootdown.processors_holding(9), 0);
        assert_eq!(shootdown.processors_holding(7), 1);
        // A visit to where it already is changes nothing.
        let loads = AtomicUsize::new(0);
        let mut f_ran = false;
        shootdown.visit(
            &one,
            7,
            |_| {
                loads.fetch_add(1, Ordering::Relaxed);
            },
            || f_ran = true,
        );
        assert!(f_ran);
        assert_eq!(loads.load(Ordering::Relaxed), 0);
    }

    /// A switch to the space the processor is on does nothing, not even the CR3 write (which
    /// would drop every non-global translation for nothing).
    #[test]
    fn a_switch_to_where_it_is_does_nothing() {
        let machine = Machine::default();
        let shootdown = Shootdown::new(Held::new(2));
        let one = cpu(1, &machine);
        switch(&shootdown, &one, 7, || ());
        let mut loaded_again = false;
        shootdown.switch_space(&one, 7, || loaded_again = true);
        assert!(!loaded_again);
        assert_eq!(shootdown.processors_holding(7), 1);
    }

    /// A processor that is not in the space is left alone: no interrupt, nothing invalidated,
    /// and nothing to wait for, even if it never answers anything.
    #[test]
    fn a_space_request_asks_only_the_processors_in_that_space() {
        const SPACE: u64 = 0x7000;
        let machine = Machine::default();
        let shootdown = Shootdown::new(Held::new(4));
        for id in 0..4 {
            shootdown.mark_online(&cpu(id, &machine));
        }
        machine.invalidated.lock().unwrap().clear();
        switch(&shootdown, &cpu(1, &machine), SPACE, || ());
        switch(&shootdown, &cpu(2, &machine), 0x9000, || ());
        let stop = AtomicBool::new(false);
        let done = thread::scope(|s| {
            let (shootdown, machine, stop) = (&shootdown, &machine, &stop);
            s.spawn(move || {
                let me = cpu(1, machine);
                while !stop.load(Ordering::Relaxed) {
                    shootdown.service(&me);
                    thread::yield_now();
                }
            });
            let done = shootdown.shoot(&cpu(0, machine), Request::NonGlobal, Scope::Space(SPACE));
            stop.store(true, Ordering::Relaxed);
            done
        })
        .unwrap();
        assert_eq!(done.processors, 1);
        assert_eq!(*machine.ipis.lock().unwrap(), [(0, 1)]);
        let invalidated = machine.invalidated.lock().unwrap();
        assert!(
            invalidated.contains(&(0, Request::NonGlobal)),
            "the initiator's own"
        );
        assert!(invalidated.contains(&(1, Request::NonGlobal)));
        assert!(
            !invalidated.iter().any(|(id, _)| *id >= 2),
            "{invalidated:?}"
        );
    }

    #[test]
    fn a_processor_leaving_the_space_must_still_answer() {
        const SPACE: u64 = 0x7000;
        let machine = Machine::default();
        machine.ticking.store(true, Ordering::Relaxed);
        let shootdown = Shootdown::new(Held::new(2));
        shootdown.mark_online(&cpu(0, &machine));
        shootdown.mark_online(&cpu(1, &machine));
        let one = cpu(1, &machine);
        switch(&shootdown, &one, SPACE, || ());
        switch(&shootdown, &one, 0x9000, || {
            // It has not loaded the other space yet: its TLB still holds this one's.
            assert_eq!(
                shootdown.shoot(&cpu(0, &machine), Request::NonGlobal, Scope::Space(SPACE)),
                Err(Stalled { cpu: 1 })
            );
        });
        // Gone, and nobody needs it any more.
        assert_eq!(
            shootdown
                .shoot(&cpu(0, &machine), Request::NonGlobal, Scope::Space(SPACE))
                .map(|done| done.processors),
            Ok(0)
        );
    }

    /// A processor that has the space loaded but has not come online takes no requests: it
    /// flushes when it does come online.
    #[test]
    fn a_space_request_never_interrupts_a_processor_that_is_not_online() {
        const SPACE: u64 = 0x7000;
        let machine = Machine::default();
        let shootdown = Shootdown::new(Held::new(2));
        shootdown.mark_online(&cpu(0, &machine));
        switch(&shootdown, &cpu(1, &machine), SPACE, || ());
        let done = shootdown
            .shoot(&cpu(0, &machine), Request::NonGlobal, Scope::Space(SPACE))
            .unwrap();
        assert_eq!(done.processors, 0);
        assert!(machine.ipis.lock().unwrap().is_empty());
    }

    /// What a retirement holds: counts its releases, and its destruction.
    struct Retired<'a> {
        dropped: &'a AtomicUsize,
    }

    impl Drop for Retired<'_> {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn a_retirement_releases_only_after_everyone_has_answered() {
        let machine = Machine::default();
        let shootdown = Shootdown::new(Held::new(2));
        shootdown.mark_online(&cpu(0, &machine));
        shootdown.mark_online(&cpu(1, &machine));
        let (released, dropped) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let stop = AtomicBool::new(false);
        thread::scope(|s| {
            let (shootdown, machine, stop) = (&shootdown, &machine, &stop);
            s.spawn(move || {
                let me = cpu(1, machine);
                while !stop.load(Ordering::Relaxed) {
                    shootdown.service(&me);
                    thread::yield_now();
                }
            });
            let done = shootdown
                .retire(
                    &cpu(0, machine),
                    Retired { dropped: &dropped },
                    Request::page(0x1000),
                    Scope::Kernel,
                    |retired| {
                        // By now the other processor has dropped the translation.
                        let invalidated = machine.invalidated.lock().unwrap();
                        assert!(invalidated.contains(&(1, Request::page(0x1000))));
                        released.fetch_add(1, Ordering::Relaxed);
                        drop(retired);
                    },
                )
                .unwrap();
            assert_eq!(done.processors, 1);
            stop.store(true, Ordering::Relaxed);
        });
        assert_eq!(released.load(Ordering::Relaxed), 1);
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }

    /// Nothing that may still be reached is released, or even dropped: it is leaked.
    #[test]
    fn a_retirement_that_stalls_releases_nothing_and_leaks_the_value() {
        let machine = Machine::default();
        machine.ticking.store(true, Ordering::Relaxed);
        let shootdown = Shootdown::new(Held::new(2));
        shootdown.mark_online(&cpu(0, &machine));
        shootdown.mark_online(&cpu(1, &machine));
        let (released, dropped) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let outcome = shootdown.retire(
            &cpu(0, &machine),
            Retired { dropped: &dropped },
            Request::Everything,
            Scope::Kernel,
            |_| {
                released.fetch_add(1, Ordering::Relaxed);
            },
        );
        assert_eq!(outcome, Err(Stalled { cpu: 1 }));
        assert_eq!(released.load(Ordering::Relaxed), 0);
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_stall_is_remembered_until_a_flush_completes() {
        let confirmation = Confirmation::new();
        let done = Completed {
            processors: 1,
            ticks: 5,
        };
        let stall = Stalled { cpu: 3 };
        let mut flushes = 0;
        // Nothing to settle: no flush.
        assert_eq!(confirmation.settle(|| unreachable!("no flush")), Ok(None));
        assert_eq!(confirmation.record(Ok(done)), Ok(done));
        assert!(!confirmation.is_unconfirmed());
        // A stall is remembered...
        assert_eq!(confirmation.record(Err(stall)), Err(stall));
        assert!(confirmation.is_unconfirmed());
        // ... through a flush that stalls too...
        assert_eq!(
            confirmation.settle(|| {
                flushes += 1;
                Err(stall)
            }),
            Err(stall)
        );
        assert!(confirmation.is_unconfirmed());
        // ... and goes with the first flush that completes, which the next change pays for.
        assert_eq!(
            confirmation.settle(|| {
                flushes += 1;
                Ok(done)
            }),
            Ok(Some(done))
        );
        assert!(!confirmation.is_unconfirmed());
        assert_eq!(flushes, 2);
        assert_eq!(confirmation.settle(|| unreachable!("settled")), Ok(None));
    }

    /// Why a [`Pages`] change failed.
    #[derive(Debug, PartialEq, Eq)]
    enum Refused {
        Prepare,
        Round(u64),
        Stalled(Stalled),
    }

    impl From<Stalled> for Refused {
        fn from(stalled: Stalled) -> Self {
            Self::Stalled(stalled)
        }
    }

    /// A change of `rounds` rounds of one page each, made as a space's unmap makes them: a
    /// round's page leaves the tables when the round is made, and is kept until given back.
    #[derive(Default)]
    struct Pages {
        rounds: u64,
        /// The round that fails, if one does, and whether `prepare` refuses.
        fails: Option<u64>,
        refuses: bool,
        /// The pages out of the tables: every round made so far.
        made: u64,
        kept: Vec<u64>,
        given_back: Vec<u64>,
        lost: Vec<u64>,
        /// Whole flushes completed, and the pages made when each completed.
        flushes: Vec<u64>,
    }

    impl Pages {
        fn of(rounds: u64) -> Self {
            Self {
                rounds,
                ..Self::default()
            }
        }
    }

    impl Rounds for Pages {
        type Error = Refused;

        fn prepare(&mut self) -> Result<(), Refused> {
            if self.refuses {
                return Err(Refused::Prepare);
            }
            Ok(())
        }

        fn round(&mut self) -> Result<Option<Request>, Refused> {
            if self.made == self.rounds {
                return Ok(None);
            }
            if self.fails == Some(self.made) {
                return Err(Refused::Round(self.made));
            }
            let page = self.made;
            self.made += 1;
            self.kept.push(page);
            Ok(Some(Request::page(page * PAGE)))
        }

        fn shot(&mut self) {
            assert_eq!(self.kept.len(), 1, "only the round just shot is kept");
            self.given_back.append(&mut self.kept);
        }

        fn flushed(&mut self) {
            self.flushes.push(self.made);
            self.given_back.append(&mut self.kept);
        }

        fn lost(&mut self) {
            self.lost.append(&mut self.kept);
        }
    }

    /// Runs `pages`' change on a space marked unconfirmed or not, with shootdowns that complete
    /// or stall as `script` says, in order. Returns the outcome, the requests and whether the
    /// space ends unconfirmed.
    fn change(
        pages: &mut Pages,
        unconfirmed: bool,
        script: &[bool],
    ) -> (Result<Cost, Refused>, Vec<Request>, bool) {
        let confirmation = Confirmation::new();
        if unconfirmed {
            let _ = confirmation.record(Err(Stalled { cpu: 1 }));
        }
        let mut asked = Vec::new();
        let outcome = confirmation.change(pages, |request| {
            let completes = script[asked.len()];
            asked.push(request);
            if completes {
                Ok(Completed {
                    processors: 2,
                    ticks: 1,
                })
            } else {
                Err(Stalled { cpu: 1 })
            }
        });
        assert_eq!(
            asked.len(),
            script.len(),
            "every scripted shootdown was asked for"
        );
        (outcome, asked, confirmation.is_unconfirmed())
    }

    const fn page(n: u64) -> Request {
        Request::page(n * PAGE)
    }

    const fn cost(shootdowns: u32) -> Cost {
        Cost {
            shootdowns,
            interrupts: 2 * shootdowns,
        }
    }

    #[test]
    fn without_a_stall_each_round_is_shot_down_then_given_back() {
        let mut pages = Pages::of(3);
        let (outcome, asked, unconfirmed) = change(&mut pages, false, &[true; 3]);
        assert_eq!(outcome, Ok(cost(3)));
        assert_eq!(asked, [page(0), page(1), page(2)]);
        assert_eq!(pages.given_back, [0, 1, 2]);
        assert!(pages.lost.is_empty() && pages.flushes.is_empty() && !unconfirmed);
    }

    /// Finding A, case b: a round in the middle stalls. The rounds after it are still made, and
    /// what they and the stalled round took out waits for one flush of the whole space.
    #[test]
    fn a_round_that_stalls_does_not_stop_the_change() {
        let mut pages = Pages::of(3);
        let (outcome, asked, unconfirmed) = change(&mut pages, false, &[true, false, true]);
        assert_eq!(outcome, Ok(cost(2)));
        assert_eq!(asked, [page(0), page(1), Request::NonGlobal]);
        assert_eq!(pages.made, 3, "every page left the tables");
        assert_eq!(pages.given_back, [0, 1, 2]);
        assert_eq!(
            pages.flushes,
            [3],
            "given back after the flush, with every round made"
        );
        assert!(pages.lost.is_empty() && !unconfirmed);
    }

    #[test]
    fn when_the_flush_at_the_end_stalls_too_what_was_kept_is_lost() {
        let mut pages = Pages::of(3);
        let (outcome, asked, unconfirmed) = change(&mut pages, false, &[true, false, false]);
        assert_eq!(outcome, Err(Refused::Stalled(Stalled { cpu: 1 })));
        assert_eq!(asked, [page(0), page(1), Request::NonGlobal]);
        assert_eq!(pages.made, 3, "every page left the tables");
        assert_eq!(
            pages.given_back,
            [0],
            "only what a completed shootdown covered"
        );
        assert_eq!(pages.lost, [1, 2]);
        assert!(unconfirmed, "the space stays unconfirmed");
    }

    #[test]
    fn a_stall_an_earlier_change_left_is_flushed_before_the_first_round() {
        let mut pages = Pages::of(2);
        let (outcome, asked, unconfirmed) = change(&mut pages, true, &[true; 3]);
        assert_eq!(outcome, Ok(cost(3)));
        assert_eq!(asked, [Request::NonGlobal, page(0), page(1)]);
        assert_eq!(
            pages.flushes,
            [0],
            "what earlier changes kept goes back first"
        );
        assert_eq!(pages.given_back, [0, 1]);
        assert!(!unconfirmed);
    }

    /// Finding A, case a: the flush that should make an earlier stall good stalls again. The
    /// change is still made, without a shootdown per round, and one flush at its end decides.
    #[test]
    fn a_first_flush_that_stalls_does_not_stop_the_change() {
        let mut pages = Pages::of(3);
        let (outcome, asked, unconfirmed) = change(&mut pages, true, &[false, true]);
        assert_eq!(outcome, Ok(cost(1)));
        assert_eq!(asked, [Request::NonGlobal, Request::NonGlobal]);
        assert_eq!(pages.made, 3, "every page left the tables");
        assert_eq!(pages.given_back, [0, 1, 2]);
        assert_eq!(pages.flushes, [3]);
        assert!(!unconfirmed);

        let mut pages = Pages::of(3);
        let (outcome, _, unconfirmed) = change(&mut pages, true, &[false, false]);
        assert_eq!(outcome, Err(Refused::Stalled(Stalled { cpu: 1 })));
        assert_eq!(pages.made, 3, "every page left the tables");
        assert!(pages.given_back.is_empty());
        assert_eq!(pages.lost, [0, 1, 2]);
        assert!(unconfirmed);
    }

    #[test]
    fn a_refusal_before_the_first_round_changes_nothing() {
        let mut pages = Pages {
            refuses: true,
            ..Pages::of(2)
        };
        let (outcome, asked, unconfirmed) = change(&mut pages, false, &[]);
        assert_eq!(outcome, Err(Refused::Prepare));
        assert!(asked.is_empty() && pages.made == 0 && !unconfirmed);
        // After a first flush that stalled: still nothing made, and the mark stays.
        let mut pages = Pages {
            refuses: true,
            ..Pages::of(2)
        };
        let (outcome, _, unconfirmed) = change(&mut pages, true, &[false]);
        assert_eq!(outcome, Err(Refused::Prepare));
        assert!(pages.made == 0 && unconfirmed);
    }

    #[test]
    fn a_round_refused_after_a_stall_still_settles_what_was_kept() {
        let mut pages = Pages {
            fails: Some(2),
            ..Pages::of(3)
        };
        let (outcome, asked, unconfirmed) = change(&mut pages, false, &[false, true]);
        assert_eq!(outcome, Err(Refused::Round(2)));
        assert_eq!(asked, [page(0), Request::NonGlobal]);
        assert_eq!(pages.given_back, [0, 1]);
        assert!(pages.lost.is_empty() && !unconfirmed);

        let mut pages = Pages {
            fails: Some(2),
            ..Pages::of(3)
        };
        let (outcome, _, unconfirmed) = change(&mut pages, false, &[false, false]);
        assert_eq!(
            outcome,
            Err(Refused::Round(2)),
            "the refusal says more than the stall"
        );
        assert_eq!(pages.lost, [0, 1]);
        assert!(unconfirmed);
    }

    /// Every processor `0..count` is online, and one thread answers for all of them but the
    /// initiator, 0, until `shoot` returns; the outcome and every mark afterwards.
    fn shoot_with_everyone_online(count: usize) -> (Result<Completed, Stalled>, Machine, bool) {
        let machine = Machine::default();
        let shootdown = Shootdown::new(Held::new(count));
        for id in 0..count {
            shootdown.mark_online(&cpu(id, &machine));
        }
        let stop = AtomicBool::new(false);
        let done = thread::scope(|s| {
            let (shootdown, machine, stop) = (&shootdown, &machine, &stop);
            s.spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    for id in 1..count {
                        shootdown.service(&cpu(id, machine));
                    }
                    thread::yield_now();
                }
            });
            let done = shootdown.shoot(&cpu(0, machine), Request::page(0x9000), Scope::Kernel);
            stop.store(true, Ordering::Relaxed);
            done
        });
        let marks_clear = (0..count).all(|id| {
            !shootdown
                .records()
                .asked(id)
                .unwrap()
                .0
                .load(Ordering::Relaxed)
        });
        (done, machine, marks_clear)
    }

    /// ADR-0008 Amendment 1, D1 and D7: the number of processors is the storage's, known only
    /// at run time, and every one of them is asked, interrupted once and waited for.
    #[test]
    fn every_processor_answers_whatever_the_count() {
        for count in [1, 3, 65, 300] {
            let count = std::hint::black_box(count);
            let (done, machine, marks_clear) = shoot_with_everyone_online(count);
            let done = done.unwrap();
            assert_eq!(done.processors as usize, count - 1, "{count} processors");
            let ipis = machine.ipis.lock().unwrap();
            assert_eq!(
                *ipis,
                (1..count).map(|id| (0, id)).collect::<Vec<_>>(),
                "{count} processors: one interrupt each, in index order"
            );
            let invalidated = machine.invalidated.lock().unwrap();
            for id in 0..count {
                assert!(
                    invalidated.contains(&(id, Request::page(0x9000))),
                    "{count} processors: CPU {id}"
                );
            }
            assert!(marks_clear, "{count} processors: a mark was left set");
        }
    }

    /// A stall clears every mark the request set, the stalled processor's and those after it,
    /// so the next request starts with none and asks again.
    #[test]
    fn a_stall_leaves_no_mark_behind() {
        let machine = Machine::default();
        machine.ticking.store(true, Ordering::Relaxed);
        let shootdown = Shootdown::new(Held::new(5));
        for id in [0, 2, 4] {
            shootdown.mark_online(&cpu(id, &machine));
        }
        let outcome = shootdown.shoot(&cpu(0, &machine), Request::NonGlobal, Scope::Kernel);
        assert_eq!(outcome, Err(Stalled { cpu: 2 }));
        for id in 0..5 {
            let asked = shootdown.records().asked(id).unwrap();
            assert!(!asked.0.load(Ordering::Relaxed), "CPU {id}'s mark");
        }
        let again = shootdown.shoot(&cpu(0, &machine), Request::NonGlobal, Scope::Kernel);
        assert_eq!(again, Err(Stalled { cpu: 2 }));
        assert_eq!(
            *machine.ipis.lock().unwrap(),
            [(0, 2), (0, 4), (0, 2), (0, 4)]
        );
    }

    /// A processor past the storage cannot come online and is never asked; a processor that
    /// must answer and has no mark makes the request a stall, never a skip.
    #[test]
    fn storage_bounds_who_is_asked_and_a_missing_mark_is_a_stall() {
        let machine = Machine::default();
        let shootdown = Shootdown::new(Held::new(2));
        shootdown.mark_online(&cpu(0, &machine));
        shootdown.mark_online(&cpu(2, &machine));
        assert!(!shootdown.must_answer(2, Scope::Kernel));
        let done = shootdown
            .shoot(&cpu(0, &machine), Request::page(0x1000), Scope::Kernel)
            .unwrap();
        assert_eq!(done.processors, 0);
        assert!(machine.ipis.lock().unwrap().is_empty());

        let machine = Machine::default();
        let shootdown = Shootdown::new(Held::with_marks(3, 2));
        for id in 0..3 {
            shootdown.mark_online(&cpu(id, &machine));
        }
        let outcome = shootdown.shoot(&cpu(0, &machine), Request::page(0x1000), Scope::Kernel);
        assert_eq!(outcome, Err(Stalled { cpu: 2 }));
    }

    #[test]
    fn it_fits_a_kernel_static() {
        /// Records for a fixed machine, made in a `const` as the kernel's boot slots are.
        struct Fixed([Record; 3], [Asked; 3]);
        impl Records for Fixed {
            fn count(&self) -> usize {
                self.0.len()
            }
            fn record(&self, cpu: usize) -> Option<&Record> {
                self.0.get(cpu)
            }
            fn asked(&self, cpu: usize) -> Option<&Asked> {
                self.1.get(cpu)
            }
        }
        static SHOOTDOWN: Shootdown<Fixed> = Shootdown::new(Fixed(
            [const { Record::new() }; 3],
            [const { Asked::new() }; 3],
        ));
        assert_eq!(SHOOTDOWN.generation.load(Ordering::Relaxed), 0);
        assert_eq!(SHOOTDOWN.records().count(), 3);
    }
}
