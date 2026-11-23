//! A per-CPU round-robin run queue of saved contexts (M3, first scheduler; CLAUDE.md §25
//! thread lifecycle and termination, §26: "begin with a correct per-CPU scheduler", "keep a
//! safe scheduler fallback").
//!
//! A context is the stack pointer at which an interrupted thread's full register frame is
//! saved: the timer interrupt (or a yield, sleep or exit request, which enter the kernel the
//! same way) saves the running thread's frame on its own stack, and the queue answers which
//! saved frame to resume. Slot 0 is the context that was running when the queue started
//! (the CPU's boot flow or idle loop); it can never sleep or exit, so there is always
//! something to run. Time is counted in this CPU's timer ticks. This is policy only: plain
//! integers, and whatever the caller keeps of each slot's thread record (`RunQueue<R>`), no
//! `unsafe`, tested on the host. Priorities, fairness accounting and load
//! balancing come later (an EEVDF-style design is to be evaluated, §26).
//!
//! Choosing the next context is stride scheduling (C. A. Waldspurger and W. E. Weihl,
//! "Stride Scheduling: Deterministic Proportional-Share Resource Management", MIT
//! LCS/TM-528, 1995), a deterministic proportional-share policy:
//! - every context has a weight (1–[`MAX_WEIGHT`], default [`DEFAULT_WEIGHT`]) and a pass
//!   value; each quantum it uses (a tick, or a yield) advances its pass by
//!   [`STRIDE_ONE`]` / weight`;
//! - the ready context with the lowest pass runs next, ties going round-robin from the
//!   current one, so equal weights are exactly round-robin and over any long stretch each
//!   context's share of the processor is its share of the weights;
//! - a context that becomes ready (new, woken, or due after a sleep) starts at no less than
//!   the queue's **floor**, the lowest pass among the ready ones: sleeping earns no credit, so
//!   it can neither starve the others nor be starved;
//! - slot 0 can be the processor's idle context ([`RunQueue::set_idle`]), which runs only
//!   when nothing else is ready, and never counts toward the floor;
//! - with nothing ready that counts, the floor holds its last value: it never falls (ADR-0020
//!   S6, the note of 2026-11-22). Every pass placed in the queue is at least the floor, so the
//!   floor only rises, and a thread that blocked is never left far above it.
//!
//! Blocking: a thread [`RunQueue::block`]s until another [`RunQueue::wake`]s it. A wake-up
//! names its thread by slot and the slot's generation (bumped by every [`RunQueue::push`]),
//! so a stale one never wakes a later thread in the same slot. A wake-up that arrives
//! before its thread has blocked (it registered with a wait queue and is about to block) is
//! kept, and its next `block` returns at once: no wake-up is lost.
//!
//! The kernel names a thread by its record instead (`origin_sync::threads`, ADR-0020 S1). It
//! finds the slot through the record's location, checks the record's generation under this
//! queue's lock, and then passes the slot's own generation, so here the slot's generation is not
//! a second check: the record's location, cleared in the same critical section as
//! [`RunQueue::exit`], is what keeps a stale name from a slot's next thread (the review of TASKS
//! item 92, R92-4). Whatever later frees or reuses a slot must clear or move that location in the
//! same critical section.
//!
//! An interrupt or IPI that may have woken a thread goes through
//! [`RunQueue::preempt_for_wake`]: it charges nothing, and switches only if a thread woken
//! since the last choice should run now.
//!
//! A thread moves between two processors' queues (ADR-0020 S6; TASKS item 96) by
//! [`RunQueue::depart`] from its source and [`RunQueue::arrive`] at its destination, under both
//! queues' locks. What passes is its saved frame, its weight, a pending wake-up and its record,
//! and its **lag**: how far its pass lies above its source's floor. It arrives at its
//! destination's floor plus that lag, so a move gives it no credit and takes none from it, as
//! "sleeping earns no credit" does. Only a ready thread that is not running moves: the source's
//! scheduler then cannot have named its frame (the review of TASKS item 95, R95-5).
//!
//! What balancing reads here (ADR-0020 S7 and S10; TASKS item 98), all of it under this queue's
//! lock. [`RunQueue::switch`] is not changed for it; a block, a sleep and a wake-up's preemption
//! each set one bit for the cache-hot rule on their way to it (`ran`):
//! - which threads balancing may take ([`RunQueue::set_balanced`]), a bit a slot, which a move
//!   carries;
//! - a count of what is runnable and which of those threads wait ([`RunQueue::census`]);
//! - when a thread arrived by a move, in this queue's ticks, for the move rate
//!   ([`RunQueue::settled`]);
//! - which waiting threads are not cache-hot ([`RunQueue::mark`], [`RunQueue::judge`],
//!   [`RunQueue::cold`]): judged by this queue's own processor from its own TSC, never by a
//!   reader on another;
//! - whether this queue's processor has been asked, in its present tick, to publish a waiting
//!   thread's frame ([`RunQueue::take_ask`]; ADR-0020 Amendment 1): at most one ask a tick.

/// Contexts one queue holds, its boot context included.
pub const CAPACITY: usize = 16;
/// The pass a weight-1 context advances per quantum.
pub const STRIDE_ONE: u64 = 1 << 20;
/// The weight of a context nobody chose one for; the largest weight.
pub const DEFAULT_WEIGHT: u32 = 8;
pub const MAX_WEIGHT: u32 = 1024;

/// `part` of `whole`, in parts per thousand (a `whole` of 0 counts as 1).
pub fn per_mille(part: u64, whole: u64) -> u64 {
    part.saturating_mul(1000) / whole.max(1)
}

/// Whether a measured `share` (per mille) is near the share `weight` is due of `total`
/// weights: within a third of that ideal, plus 15 per mille. This is the bound of the
/// weighted-share boot proof (`sched::prove_weights`).
pub fn share_near_weight(share: u64, weight: u32, total: u32) -> bool {
    let ideal = u64::from(weight) * 1000 / u64::from(total.max(1));
    share.abs_diff(ideal) <= ideal / 3 + 15
}

/// The bound of the wake-up ping-pong boot proof (`wait::prove`): the round trip between
/// two processors, each woken by the other, in microseconds. Woken only by the other's
/// timer tick (10 ms), a round trip would take milliseconds.
pub const ROUND_TRIP_LIMIT_MICROS: u64 = 2000;

/// A ping-pong run over the bound is measured again if the boot flow went without a turn
/// for at least this much of it, in parts per thousand: on a loaded host, a virtual CPU
/// descheduled. A tenth, as for the weighted-share proof. The boot flow shares its processor
/// with a thread under test, which can produce the same reading just by running, so this can
/// earn a repeat and nothing more.
pub const DISTURBED_PER_MILLE: u64 = 100;

/// What one run of the ping-pong boot proof calls for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundTrips {
    /// The mean round trip is under the bound.
    Pass,
    /// Over the bound, on a run that shows a disturbance: it says nothing, measure again.
    Repeat,
    /// Over the bound, on a run that shows none.
    Fail,
}

/// Judges a ping-pong run by its mean round trip (microseconds). A disturbance
/// (`away_per_mille`, see [`DISTURBED_PER_MILLE`]) can earn a run over the bound a repeat;
/// it can never pass one.
pub const fn judge_round_trips(mean_micros: u64, away_per_mille: u64) -> RoundTrips {
    if mean_micros < ROUND_TRIP_LIMIT_MICROS {
        RoundTrips::Pass
    } else if away_per_mille >= DISTURBED_PER_MILLE {
        RoundTrips::Repeat
    } else {
        RoundTrips::Fail
    }
}

/// The timed-wait boot proof's bounds (`wait::prove_timed`), in milliseconds: an unsignalled
/// wait of [`TIMED_SHORT_MS`] must time out no earlier than its deadline and within
/// [`TIMED_SHORT_LATE_MS`] of it; a wait of 2000 ms signalled early must return within
/// [`TIMED_LONG_LIMIT_MS`].
pub const TIMED_SHORT_MS: u64 = 50;
pub const TIMED_SHORT_LATE_MS: u64 = 30;
pub const TIMED_LONG_LIMIT_MS: u64 = 500;

/// Ticks a waiter's processor may lose over its wait before the wait's lateness says nothing
/// about the kernel: the weighted-share proof's threshold (`sched::TICKS_LOST_TO_A_STALL`). A
/// timed wait sleeps whole ticks of its own processor (`sched::block_until`), so a late wait is
/// a processor that lost ticks; losing three in a 50 ms wait needs its interrupts held off, or
/// its virtual processor away, for 30 ms.
pub const TIMED_TICKS_LOST: u64 = 3;

/// One timed waiter's run, as the waiter measured it on its own processor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimedWait {
    /// Milliseconds from its start to its return.
    pub waited_ms: u64,
    /// Its processor's timer ticks over the same time.
    pub ticks: u64,
}

impl TimedWait {
    /// The ticks its processor did not take over its wait, at `hz` ticks a second.
    pub const fn ticks_lost(self, hz: u64) -> u64 {
        (self.waited_ms * hz / 1000).saturating_sub(self.ticks)
    }
}

/// Judges one run of the timed-wait proof's timing half (TASKS item 66). Only a run inside the
/// bounds passes. A run that ended early fails. A run that is late passes nothing: it is
/// measured again only if each late waiter shows a disturbance it cannot have made itself (its
/// processor lost [`TIMED_TICKS_LOST`] ticks over its wait; or, for the signalled wait, whose
/// lateness the boot flow's signal also decides, the boot flow went without a turn for
/// [`DISTURBED_PER_MILLE`] of the run). Otherwise it fails at once.
pub const fn judge_timed_waits(
    short: TimedWait,
    long: TimedWait,
    away_per_mille: u64,
    hz: u64,
) -> RoundTrips {
    if short.waited_ms < TIMED_SHORT_MS {
        return RoundTrips::Fail;
    }
    let short_ok = short.waited_ms <= TIMED_SHORT_MS + TIMED_SHORT_LATE_MS;
    let long_ok = long.waited_ms < TIMED_LONG_LIMIT_MS;
    if short_ok && long_ok {
        return RoundTrips::Pass;
    }
    let short_excused = short_ok || short.ticks_lost(hz) >= TIMED_TICKS_LOST;
    let long_excused =
        long_ok || long.ticks_lost(hz) >= TIMED_TICKS_LOST || away_per_mille >= DISTURBED_PER_MILLE;
    if short_excused && long_excused {
        RoundTrips::Repeat
    } else {
        RoundTrips::Fail
    }
}

/// The context switches the preemption proof's rounds clause (ADR-0013) needs within today's
/// bound: 15, five rounds of a processor's three contexts, the clause since before TASKS item 66.
pub const PREEMPTION_ROUNDS: u64 = 15;

/// What a processor's own periodic timer did while the preemption proof watched it (TASKS item
/// 66, the review's T66-R1): every gap between its ticks on the TSC, the first from the moment
/// the watch was armed, kept as the longest; the time of the processor's [`PREEMPTION_ROUNDS`]th
/// context switch since then (the addendum review's T66-R15); and the timer's programming as the
/// processor read it back at its first tick. The proof's window is counted in these very ticks,
/// so their rate is checked here: the programming against what the boot processor's timed proof
/// ran with, the switches against the TSC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TickWatch {
    /// Whether ticks are being noted.
    pub armed: bool,
    /// The TSC when the watch was armed, and at the last tick noted.
    pub start: u64,
    pub last: u64,
    /// The programmed period, in TSC ticks.
    pub period: u64,
    /// Ticks noted, and so gaps measured.
    pub ticks: u64,
    /// The longest gap.
    pub longest: u64,
    /// The queue's count of context switches when the watch was armed.
    pub switches_from: u64,
    /// The TSC just after the [`PREEMPTION_ROUNDS`]th context switch since the watch was armed,
    /// 0 until then.
    pub at_switches: u64,
    /// The timer's programming as this processor read it back at its first tick, against the
    /// programming the tick proof timed.
    pub read_back: ReadBack,
}

/// A processor's timer as read back at its first watched tick, against the initial count, divide
/// configuration and LVT entry the boot processor's timed proof ran with
/// (`origin_frame::apic::TimerProgramming::first_difference` compares them). Plain values, so
/// that this file also builds in `origin-sync`'s Loom models.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadBack {
    /// Not read yet, or its registers could not be read.
    Unread,
    /// Programmed as timed.
    AsTimed,
    /// Programmed otherwise: the first register that differs, by name, with the value read and
    /// the value timed.
    Otherwise(&'static str, u64, u64),
}

impl TickWatch {
    /// A watch that notes nothing.
    pub const fn new() -> Self {
        Self {
            armed: false,
            start: 0,
            last: 0,
            period: 0,
            ticks: 0,
            longest: 0,
            switches_from: 0,
            at_switches: 0,
            read_back: ReadBack::Unread,
        }
    }

    /// A watch armed at TSC `now` for a period of `period` TSC ticks.
    pub const fn armed(now: u64, period: u64) -> Self {
        Self {
            armed: true,
            start: now,
            last: now,
            period,
            ..Self::new()
        }
    }

    /// Whether the next tick noted is the first: the one at which the programming is read.
    pub const fn first_tick(&self) -> bool {
        self.armed && self.ticks == 0
    }

    /// Notes the timer's programming as read back on its processor.
    pub fn note_read_back(&mut self, read_back: ReadBack) {
        if self.armed {
            self.read_back = read_back;
        }
    }

    /// Notes a tick at TSC `now`, if armed.
    pub fn note(&mut self, now: u64) {
        if !self.armed {
            return;
        }
        let gap = now.saturating_sub(self.last);
        self.last = now;
        self.ticks += 1;
        self.longest = self.longest.max(gap);
    }

    /// Notes, at TSC `now` just after a scheduling step, that the queue has made `switches`
    /// context switches in all: the first time [`PREEMPTION_ROUNDS`] of them have come since the
    /// watch was armed, `now` is their time. Called after every step, so it is never earlier than
    /// the switch it stamps.
    pub fn note_switches(&mut self, now: u64, switches: u64) {
        if self.armed
            && self.at_switches == 0
            && switches.wrapping_sub(self.switches_from) >= PREEMPTION_ROUNDS
        {
            self.at_switches = now;
        }
    }
}

impl Default for TickWatch {
    fn default() -> Self {
        Self::new()
    }
}

/// What one processor's ticks say about a run of the preemption proof (TASKS item 66, T66-R1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowVerdict {
    /// Its timer is programmed as the boot processor's timed proof ran it, and its
    /// [`PREEMPTION_ROUNDS`]th context switch came within today's bound.
    InBounds,
    /// Its rounds came after today's bound, and a gap of two periods or more between its ticks
    /// shows expiries merged while it was away: this run says nothing, and is measured again.
    Stalled,
    /// Its rounds came after today's bound with no such gap, or it switched fewer than
    /// [`PREEMPTION_ROUNDS`] times in the whole window: too slow, and it fails.
    Late,
    /// Its timer is programmed otherwise than the boot processor's timed proof ran it (another
    /// initial count, divisor or mode, or a masked entry), or could not be read back: a wrong
    /// rate, and it fails at once.
    WrongRate,
    /// It did not take the window's ticks within the budget: its tick does not come.
    Short,
}

/// Judges one processor's [`TickWatch`] over a run: it should have taken `wanted` ticks, with its
/// timer read back as programmed the way the boot processor's timed proof ran it
/// ([`ReadBack::AsTimed`]), and its [`PREEMPTION_ROUNDS`]th context switch within `bound` TSC
/// ticks of the start (today's bound since before TASKS item 66: 15 switches in 500 ms of TSC
/// time; the addendum review's T66-R15). Only [`WindowVerdict::InBounds`] passes.
///
/// The rate is checked by the programming, not by the gaps' spread: under QEMU's software
/// processor the ticks' delivery jitters, so that a third or more of the gaps of a correct
/// timer fall under 95 % of the period even on a quiet two-processor machine (TASKS item 66's
/// diagnostic boots), and a median of gaps cannot be a verdict there.
pub fn judge_window(watch: &TickWatch, wanted: u64, bound: u64) -> WindowVerdict {
    if matches!(watch.read_back, ReadBack::Otherwise(..)) {
        // Named first, whatever else the run shows: a timer left in one-shot mode ticks once.
        WindowVerdict::WrongRate
    } else if watch.ticks < wanted || watch.ticks < PREEMPTION_ROUNDS {
        WindowVerdict::Short
    } else if watch.read_back == ReadBack::Unread {
        WindowVerdict::WrongRate
    } else if watch.at_switches == 0 {
        // Fewer than its rounds in the whole window, whatever the gaps: no stall explains it.
        WindowVerdict::Late
    } else if watch.at_switches.saturating_sub(watch.start) <= bound {
        WindowVerdict::InBounds
    } else if watch.longest >= watch.period.saturating_mul(2) {
        WindowVerdict::Stalled
    } else {
        WindowVerdict::Late
    }
}

/// The preemption proof's budget for its runs, from the first run's start: 120 s of wall time,
/// the pauses between runs included (the coordinating session's ruling of 2026-11-20). On the
/// oversubscribed software-processor machines a stall comes in an episode of seconds, the longest
/// seen outlasting 8 runs, so the budget is time, not a count.
pub const WINDOW_RUNS_SECONDS: u64 = 120;
/// The most runs within that budget, which bounds the lines printed.
pub const WINDOW_RUNS_MOST: u32 = 32;
/// The pause before each further run, every processor idle, so that an episode can pass.
pub const WINDOW_PAUSE_SECONDS: u64 = 2;

/// Whether the preemption proof runs its window again after run `runs` (from 1), whose verdict
/// was `verdict`, its correctness clauses `correct`, ending `elapsed` TSC ticks after the first
/// run's start: only a correct run its processors' ticks showed stalled, and only while the next
/// run can start within [`WINDOW_RUNS_SECONDS`], the pause before it counted, and
/// [`WINDOW_RUNS_MOST`] runs have not been made.
pub const fn window_again(
    verdict: WindowVerdict,
    correct: bool,
    runs: u32,
    elapsed: u64,
    tsc_hz: u64,
) -> bool {
    correct
        && matches!(verdict, WindowVerdict::Stalled)
        && runs < WINDOW_RUNS_MOST
        && elapsed.saturating_add(WINDOW_PAUSE_SECONDS.saturating_mul(tsc_hz))
            < WINDOW_RUNS_SECONDS.saturating_mul(tsc_hz)
}

/// Why a request was refused.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum QueueError {
    /// The queue holds [`CAPACITY`] contexts.
    Full,
    /// A saved context's stack pointer is not 8-byte aligned or is null.
    BadContext(u64),
    /// The boot context (slot 0) may not sleep or exit.
    BootContext,
}

/// Names the refusal, never `BadContext`'s value: it is a frame's address, in the kernel
/// stacks' region, which no line prints (ADR-0004 Amendment 2, A6 and A6.3; the review of TASKS
/// items 76 and 80, T76-80-3). `core` only: `origin-sync`'s Loom models compile this file too.
impl core::fmt::Debug for QueueError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Full => "Full",
            Self::BadContext(_) => "BadContext(..)",
            Self::BootContext => "BootContext",
        })
    }
}

/// What a slot holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Free,
    Ready,
    /// Asleep until this tick.
    Sleeping(u64),
    /// Waiting for a [`RunQueue::wake`], or until this tick if there is one.
    Blocked(Option<u64>),
}

/// One CPU's run queue. `R` is what each slot holds of its thread's record (ADR-0020 S1, TASKS
/// item 92): its number, in the tests and the Loom models; in the kernel, the number and the
/// record itself, resolved once when the thread is placed in the queue, so that a switch reaches
/// the thread's state without a lookup (the coordinating session's second ruling on TASKS item
/// 92's Q6). The queue only stores it and hands it back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunQueue<R = u32> {
    saved: [u64; CAPACITY],
    state: [State; CAPACITY],
    /// Bumped each time a slot gets a new thread.
    generation: [u32; CAPACITY],
    /// A wake-up that arrived while the slot's thread was not blocked.
    wake_pending: [bool; CAPACITY],
    /// The record each slot's thread has (ADR-0020 S1, TASKS item 92), so that the processor
    /// finds the thread's state, and a wake-up its name, under this queue's lock. `None` for slot
    /// 0 and for a free slot.
    record: [Option<R>; CAPACITY],
    /// The slots a wake-up made ready since this queue last chose what to run.
    woken: [bool; CAPACITY],
    /// Each slot's weight and pass (stride scheduling).
    weight: [u32; CAPACITY],
    pass: [u64; CAPACITY],
    /// Slot 0 is the idle context: it runs only when nothing else is ready.
    idle: bool,
    /// The floor while no context that counts toward it is ready: its last value
    /// ([`RunQueue::floor`]; ADR-0020 S6, the note of 2026-11-22).
    kept_floor: u64,
    current: usize,
    ticks: u64,
    switches: u64,
    /// What this processor's own timer does while the preemption proof watches it.
    watch: TickWatch,
    /// The slots whose thread balancing may take (ADR-0020 S7; TASKS item 98), a bit a slot.
    balanced: u16,
    /// For each slot, [`RunQueue::ticks`] when its thread arrived by a move; `None` for a thread
    /// that was pushed here, and for a free slot.
    arrived: [Option<u64>; CAPACITY],
    /// The mark ([`RunQueue::mark`]): this processor's own TSC at its last tick; the slots that
    /// were ready and not running then, or whose thread was pushed since; and every slot's pass
    /// then, or at its push.
    mark_tsc: u64,
    mark_waiting: u16,
    mark_pass: [u64; CAPACITY],
    /// The slots known to have run since the mark though their pass does not show it: the running
    /// one at a block, a sleep or a wake-up's preemption. A tick and a yield charge the pass.
    ran: u16,
    /// The waiting threads last judged not cache-hot ([`RunQueue::judge`]).
    cold: u16,
    /// The balancing interrupts this queue's processor sent, counted apart from the framework's
    /// rate ([`RunQueue::note_sent`]): in all, in the tick `sent_counted_at`, and the most in one
    /// tick.
    sent: u64,
    sent_counted_at: Option<u64>,
    sent_in_tick: u32,
    sent_most: u32,
    /// [`RunQueue::ticks`] when this queue's processor was last asked to publish
    /// ([`RunQueue::take_ask`]; ADR-0020 Amendment 1).
    asked_at: Option<u64>,
    /// The asks sent to this queue's processor, counted apart from `asked_at`
    /// ([`RunQueue::note_ask`]): in all, in the tick `asks_counted_at`, and the most in one tick.
    asks: u64,
    asks_counted_at: Option<u64>,
    asks_in_tick: u32,
    asks_most: u32,
}

// A mask of slots is 16 bits.
const _: () = assert!(CAPACITY <= u16::BITS as usize);

/// The bit of `slot` in a mask of slots; no bit for a slot the queue does not have.
const fn bit(slot: usize) -> u16 {
    if slot < CAPACITY { 1 << slot } else { 0 }
}

/// What a queue holds that could run, as [`RunQueue::census`] counts it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Census {
    /// The contexts that are running or ready, the idle flow aside.
    pub runnable: u32,
    /// The sum of their weights.
    pub weight: u32,
    /// The threads balancing may take that are ready and not running, a bit a slot.
    pub waiting: u16,
}

impl Default for RunQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// A thread taken out of its source queue by [`RunQueue::depart`], to be given to its
/// destination's [`RunQueue::arrive`] (ADR-0020 S6; TASKS item 96). It is in no queue until it
/// arrives. Its `Debug` writes its name alone: its frame is an address in the kernel stacks'
/// region, which nothing prints (ADR-0004 Amendment 2, A6).
/// One slot, as [`RunQueue::describe`] reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotState {
    pub state: &'static str,
    pub pending: bool,
    pub woken: bool,
    pub pass: u64,
    pub floor: u64,
    pub current: usize,
}

#[must_use = "a departed thread is in no queue until it arrives"]
pub struct Departed<R> {
    frame: u64,
    weight: u32,
    wake_pending: bool,
    lag: u64,
    record: Option<R>,
    balanced: bool,
}

impl<R> core::fmt::Debug for Departed<R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Departed")
    }
}

impl<R: Copy> Departed<R> {
    /// Whether it carries a wake-up that came before it blocked (N1): its next block on its
    /// destination returns at once.
    pub const fn wake_pending(&self) -> bool {
        self.wake_pending
    }

    /// How far its pass lay above its source's floor when it departed.
    pub const fn lag(&self) -> u64 {
        self.lag
    }

    /// The record its slot held.
    pub const fn record(&self) -> Option<R> {
        self.record
    }

    /// Whether balancing may take it ([`RunQueue::set_balanced`]).
    pub const fn balanced(&self) -> bool {
        self.balanced
    }
}

impl RunQueue {
    /// A queue holding only the running (boot) context, its slots holding record numbers.
    pub const fn new() -> Self {
        Self::empty()
    }
}

impl<R: Copy> RunQueue<R> {
    /// A queue holding only the running (boot) context.
    pub const fn empty() -> Self {
        let mut state = [State::Free; CAPACITY];
        state[0] = State::Ready;
        Self {
            saved: [0; CAPACITY],
            state,
            generation: [0; CAPACITY],
            wake_pending: [false; CAPACITY],
            record: [None; CAPACITY],
            woken: [false; CAPACITY],
            weight: [DEFAULT_WEIGHT; CAPACITY],
            pass: [0; CAPACITY],
            idle: false,
            kept_floor: 0,
            current: 0,
            ticks: 0,
            switches: 0,
            watch: TickWatch::new(),
            balanced: 0,
            arrived: [None; CAPACITY],
            mark_tsc: 0,
            mark_waiting: 0,
            mark_pass: [0; CAPACITY],
            ran: 0,
            cold: 0,
            sent: 0,
            sent_counted_at: None,
            sent_in_tick: 0,
            sent_most: 0,
            asked_at: None,
            asks: 0,
            asks_counted_at: None,
            asks_in_tick: 0,
            asks_most: 0,
        }
    }

    /// Starts watching this processor's ticks at TSC `now`, against a period of `period`
    /// TSC ticks ([`TickWatch`]).
    pub fn arm_watch(&mut self, now: u64, period: u64) {
        self.watch = TickWatch {
            switches_from: self.switches,
            ..TickWatch::armed(now, period)
        };
    }

    /// Notes, at TSC `now` just after a scheduling step, the switches made so far, if the ticks
    /// are being watched ([`TickWatch::note_switches`]).
    pub fn note_switches(&mut self, now: u64) {
        self.watch.note_switches(now, self.switches);
    }

    /// Whether the ticks are being watched: the tick handler then reads the TSC.
    pub const fn watching(&self) -> bool {
        self.watch.armed
    }

    /// Notes a tick taken at TSC `now`, if the ticks are being watched; at the first, also the
    /// timer's programming as `read_back` reads it back on this processor.
    pub fn note_tick(&mut self, now: u64, read_back: impl FnOnce() -> ReadBack) {
        if self.watch.first_tick() {
            self.watch.note_read_back(read_back());
        }
        self.watch.note(now);
    }

    /// What the watch has seen; it goes on until armed again or [`RunQueue::disarm_watch`].
    pub const fn watch(&self) -> TickWatch {
        self.watch
    }

    /// Stops watching the ticks, keeping what was seen.
    pub fn disarm_watch(&mut self) {
        self.watch.armed = false;
    }

    /// Adds a thread whose initial frame is saved at `context`, with no record; returns its slot.
    pub fn push(&mut self, context: u64) -> Result<usize, QueueError> {
        self.push_weighted(context, DEFAULT_WEIGHT, None)
    }

    /// As [`RunQueue::push`], with `weight` (clamped to 1–[`MAX_WEIGHT`]) and the thread's record
    /// (`R`), which [`RunQueue::record`] answers until the thread exits.
    pub fn push_weighted(
        &mut self,
        context: u64,
        weight: u32,
        record: Option<R>,
    ) -> Result<usize, QueueError> {
        if context == 0 || !context.is_multiple_of(8) {
            return Err(QueueError::BadContext(context));
        }
        let slot = (1..CAPACITY)
            .find(|&i| self.state.get(i) == Some(&State::Free))
            .ok_or(QueueError::Full)?;
        // The floor before this slot joins: its pass is a previous thread's.
        let floor = self.floor();
        if let (Some(saved), Some(state)) = (self.saved.get_mut(slot), self.state.get_mut(slot)) {
            *saved = context;
            *state = State::Ready;
        }
        if let (Some(g), Some(w)) = (
            self.generation.get_mut(slot),
            self.wake_pending.get_mut(slot),
        ) {
            *g = g.wrapping_add(1);
            *w = false;
        }
        if let (Some(w), Some(p)) = (self.weight.get_mut(slot), self.pass.get_mut(slot)) {
            *w = weight.clamp(1, MAX_WEIGHT);
            *p = floor;
        }
        if let Some(r) = self.record.get_mut(slot) {
            *r = record;
        }
        // The slot's new thread is as its spawner or its mover says, not as the last one was.
        self.balanced &= !bit(slot);
        self.cold &= !bit(slot);
        if let Some(a) = self.arrived.get_mut(slot) {
            *a = None;
        }
        // A thread pushed here has run nowhere yet: it has waited since before the mark, as far
        // as any cache knows ([`RunQueue::judge`]). An arrival is not so ([`RunQueue::arrive`]).
        self.mark_waiting |= bit(slot);
        self.ran &= !bit(slot);
        if let Some(m) = self.mark_pass.get_mut(slot) {
            *m = floor;
        }
        Ok(slot)
    }

    /// Makes slot 0 the idle context (it runs only when nothing else is ready), or a
    /// context like any other.
    pub fn set_idle(&mut self, idle: bool) {
        if !self.idle && idle {
            // Slot 0 stops counting toward the floor: the floor keeps the value it has now.
            self.kept_floor = self.kept_floor.max(self.floor());
        }
        if self.idle && !idle {
            // The idle context was charged only for the quanta nothing else wanted: it
            // joins the others at the floor, without credit.
            let floor = self.floor();
            self.admit(0, floor);
        }
        self.idle = idle;
    }

    /// The weight of `slot`.
    pub fn weight(&self, slot: usize) -> u32 {
        self.weight.get(slot).copied().unwrap_or(DEFAULT_WEIGHT)
    }

    /// Changes the weight of `slot` (clamped to 1–[`MAX_WEIGHT`]); it applies from the
    /// slot's next quantum.
    pub fn set_weight(&mut self, slot: usize, weight: u32) {
        if let Some(w) = self.weight.get_mut(slot) {
            *w = weight.clamp(1, MAX_WEIGHT);
        }
    }

    /// The pass `slot` advances per quantum.
    fn stride(&self, slot: usize) -> u64 {
        STRIDE_ONE / u64::from(self.weight(slot))
    }

    /// The floor: the lowest pass among the ready contexts that count toward it (threads, and
    /// slot 0 unless it is the idle flow), or, with none of them ready, the floor's last value
    /// (`kept_floor`): where a context that becomes ready starts at the earliest. It never falls
    /// (ADR-0020 S6, the note of 2026-11-22). Before that reading, a queue with nothing ready took
    /// the running one's pass, the idle flow's, which is charged only for the quanta nothing else
    /// wanted: a thread that blocked there was then left far above the next newcomer, and a move
    /// carried the difference as lag (TASKS item 129).
    ///
    /// Out of line, as it was before that reading (TASKS item 96's recount): inlined, it made
    /// `KernelQueues::wake_slot` too large to inline, a GOT call on every wake-up; and inlining the
    /// wake-up's queue calls instead doubled the wait-queue ping-pong under QEMU's software
    /// processor (turn 27). `switch` reads [`RunQueue::lowest_ready`] inline, so its path keeps
    /// the inlined search.
    #[inline(never)]
    fn floor(&self) -> u64 {
        self.lowest_ready().unwrap_or(self.kept_floor)
    }

    /// The lowest pass among the ready contexts that count toward the floor, if one is ready.
    fn lowest_ready(&self) -> Option<u64> {
        (0..CAPACITY)
            .filter(|&i| !(self.idle && i == 0) && self.state.get(i) == Some(&State::Ready))
            .filter_map(|i| self.pass.get(i).copied())
            .min()
    }

    /// Raises `slot`'s pass to at least `floor`: it is becoming ready.
    fn admit(&mut self, slot: usize, floor: u64) {
        if let Some(p) = self.pass.get_mut(slot) {
            *p = (*p).max(floor);
        }
    }

    /// The generation of the thread in `slot` (0 for slot 0, the boot context).
    pub fn generation(&self, slot: usize) -> u32 {
        self.generation.get(slot).copied().unwrap_or(0)
    }

    /// The record of the thread in `slot`, as [`RunQueue::push_weighted`] was given it: `None`
    /// for slot 0, for a free slot (also the moment its thread has exited), and for a thread
    /// pushed without one.
    pub fn record(&self, slot: usize) -> Option<R> {
        match self.state.get(slot) {
            Some(State::Free) | None => None,
            Some(_) => self.record.get(slot).copied().flatten(),
        }
    }

    /// The record `slot` holds, read without its state, for a wake-up's check that the slot holds
    /// its thread (ADR-0020 S20, trigger 3), where the state is read after it.
    ///
    /// - **A free slot's record is `None`** by construction: a push or an arrival fills the record
    ///   with the slot, an exit and a departure clear it as they free the slot, and a refused push
    ///   or arrival changes neither.
    /// - **Slot 0 never holds a record.** It is the processor's boot or idle flow, which has none
    ///   (ADR-0020 S2, point 4). Nothing places a record there:
    ///   - [`RunQueue::empty`] makes it `None`;
    ///   - a push and an arrival take only slots 1 and up;
    ///   - a departure refuses slot 0, and so do an exit, a sleep and a block (`BootContext`).
    ///
    ///   So slot 0 is answered without a read, and the check and [`RunQueue::wake`]'s own refusal
    ///   of slot 0 come to one compare of the slot on a wake-up's path.
    ///
    /// Both facts are held by the randomized test against a model after every step, and by a debug
    /// assertion here.
    #[inline]
    pub fn held(&self, slot: usize) -> Option<R> {
        if slot == 0 {
            debug_assert!(
                self.record.first().copied().flatten().is_none(),
                "slot 0 holds a record"
            );
            return None;
        }
        let held = self.record.get(slot).copied().flatten();
        debug_assert!(
            held.is_none() || self.state.get(slot) != Some(&State::Free),
            "a free slot kept a record"
        );
        held
    }

    /// Whether `slot`'s thread holds a wake-up that came before it blocked, which its next
    /// [`RunQueue::block`] consumes. Read only: a thread that leaves this queue for another
    /// carries it ([`RunQueue::depart`], ADR-0020 S6).
    pub fn wake_pending(&self, slot: usize) -> bool {
        self.wake_pending.get(slot).copied().unwrap_or(false)
    }

    /// Contexts in the queue (ready or asleep), the boot context included.
    pub fn live(&self) -> usize {
        self.state.iter().filter(|s| **s != State::Free).count()
    }

    /// Context switches made so far.
    pub const fn switches(&self) -> u64 {
        self.switches
    }

    /// Timer ticks this queue has seen.
    pub const fn ticks(&self) -> u64 {
        self.ticks
    }

    /// The slot running now.
    pub const fn current(&self) -> usize {
        self.current
    }

    /// Saves the running context (charging it one quantum if `charge`), makes due sleepers
    /// ready, and returns the next context: the ready one with the lowest pass, ties going
    /// round-robin from the current one (possibly the same context).
    fn switch(&mut self, interrupted: u64, charge: bool) -> u64 {
        if let Some(slot) = self.saved.get_mut(self.current) {
            *slot = interrupted;
        }
        if charge {
            let stride = self.stride(self.current);
            if let Some(p) = self.pass.get_mut(self.current) {
                *p = p.saturating_add(stride);
            }
        }
        let now = self.ticks;
        let floor = match self.lowest_ready() {
            Some(lowest) => lowest,
            None => {
                // Nothing that counts is ready: the context that ran was the last of them to be
                // (it blocked, slept or exited), unless it is the idle flow, and the floor's last
                // value was its pass (ADR-0020 S6, the note of 2026-11-22).
                if !(self.idle && self.current == 0)
                    && let Some(&pass) = self.pass.get(self.current)
                {
                    self.kept_floor = self.kept_floor.max(pass);
                }
                self.kept_floor
            }
        };
        for i in 0..CAPACITY {
            let due = match self.state.get(i) {
                Some(&State::Sleeping(until) | &State::Blocked(Some(until))) => until <= now,
                _ => false,
            };
            if due {
                if let Some(s) = self.state.get_mut(i) {
                    *s = State::Ready;
                }
                self.admit(i, floor);
            }
        }
        let ready = |i: usize| self.state.get(i) == Some(&State::Ready);
        let others = (1..CAPACITY).any(ready);
        let next = self
            .lowest_pass(|i| ready(i) && !(self.idle && i == 0 && others))
            .unwrap_or(0);
        // Whatever was woken has had its say in this choice.
        self.woken = [false; CAPACITY];
        if next != self.current {
            self.switches += 1;
        }
        self.current = next;
        self.saved.get(next).copied().unwrap_or(interrupted)
    }

    /// Of the contexts `eligible` admits, the one with the lowest pass, ties going
    /// round-robin from the current one (so the current one wins only if alone at the lowest).
    fn lowest_pass(&self, eligible: impl Fn(usize) -> bool) -> Option<usize> {
        let mut found = None;
        let mut best = u64::MAX;
        for i in (1..=CAPACITY).map(|k| (self.current + k) % CAPACITY) {
            let pass = self.pass.get(i).copied().unwrap_or(u64::MAX);
            if eligible(i) && pass < best {
                best = pass;
                found = Some(i);
            }
        }
        found
    }

    /// An interrupt or IPI that may have woken a thread arrived while the context whose
    /// frame is at `interrupted` ran. That context is charged nothing. If a thread woken
    /// since the last choice should run now, it runs: the woken one with the lowest pass, if
    /// the running context is the idle one or its pass is no lower than that thread's (the
    /// woken thread winning a tie). A ready context that was not woken is not considered: it
    /// waits for the next tick. Otherwise `interrupted` is returned and nothing here changes.
    pub fn preempt_for_wake(&mut self, interrupted: u64) -> u64 {
        // The running context ran, and a switch away from it here charges it nothing
        // ([`RunQueue::judge`]).
        self.ran |= bit(self.current);
        let woken = core::mem::replace(&mut self.woken, [false; CAPACITY]);
        let pass = |slot: usize| self.pass.get(slot).copied().unwrap_or(u64::MAX);
        let running = pass(self.current);
        let idle = self.idle && self.current == 0;
        let next = self
            .lowest_pass(|i| {
                woken.get(i) == Some(&true) && self.state.get(i) == Some(&State::Ready)
            })
            .filter(|&slot| idle || pass(slot) <= running);
        let Some(next) = next else {
            return interrupted;
        };
        if let Some(slot) = self.saved.get_mut(self.current) {
            *slot = interrupted;
        }
        self.switches += 1;
        self.current = next;
        self.saved.get(next).copied().unwrap_or(interrupted)
    }

    /// The timer interrupted the running context, whose frame is at `interrupted`: counts
    /// the tick, wakes sleepers that are due, and returns the frame to resume.
    pub fn tick(&mut self, interrupted: u64) -> u64 {
        self.ticks += 1;
        self.switch(interrupted, true)
    }

    /// The running context gives up the processor.
    pub fn yield_now(&mut self, interrupted: u64) -> u64 {
        // A yield gives up the rest of the quantum: charged like a tick, so equal-weight
        // contexts alternate.
        self.switch(interrupted, true)
    }

    /// The running context sleeps for `ticks` timer ticks (at least one).
    pub fn sleep(&mut self, interrupted: u64, ticks: u64) -> Result<u64, QueueError> {
        if self.current == 0 {
            return Err(QueueError::BootContext);
        }
        let until = self.ticks.saturating_add(ticks.max(1));
        if let Some(s) = self.state.get_mut(self.current) {
            *s = State::Sleeping(until);
        }
        self.ran |= bit(self.current);
        Ok(self.switch(interrupted, false))
    }

    /// The running context blocks until [`RunQueue::wake`]. If a wake-up is already
    /// pending, it is consumed and the running context goes on (`interrupted` is returned).
    pub fn block(&mut self, interrupted: u64) -> Result<u64, QueueError> {
        self.block_for(interrupted, None)
    }

    /// As [`RunQueue::block`], but the context also becomes ready after `ticks` timer ticks
    /// (at least one) if no wake-up came first; `None` waits for ever. The caller tells the
    /// two apart by checking what it waited for.
    pub fn block_for(&mut self, interrupted: u64, ticks: Option<u64>) -> Result<u64, QueueError> {
        if self.current == 0 {
            return Err(QueueError::BootContext);
        }
        if let Some(pending) = self.wake_pending.get_mut(self.current)
            && *pending
        {
            *pending = false;
            return Ok(interrupted);
        }
        let until = ticks.map(|t| self.ticks.saturating_add(t.max(1)));
        if let Some(s) = self.state.get_mut(self.current) {
            *s = State::Blocked(until);
        }
        self.ran |= bit(self.current);
        Ok(self.switch(interrupted, false))
    }

    /// Wakes the thread in `slot` if it is still the one `generation` names. A blocked
    /// thread becomes ready, and `true` is returned: the processor running this queue
    /// should reschedule. Any other live thread keeps the wake-up for its next
    /// [`RunQueue::block`]. A stale or free slot is left alone.
    pub fn wake(&mut self, slot: usize, generation: u32) -> bool {
        if slot == 0 || self.generation.get(slot) != Some(&generation) {
            return false;
        }
        match self.state.get(slot) {
            Some(State::Blocked(_)) => {
                let floor = self.floor();
                if let Some(s) = self.state.get_mut(slot) {
                    *s = State::Ready;
                }
                self.admit(slot, floor);
                if let Some(w) = self.woken.get_mut(slot) {
                    *w = true;
                }
                true
            }
            Some(State::Ready | State::Sleeping(_)) => {
                if let Some(p) = self.wake_pending.get_mut(slot) {
                    *p = true;
                }
                false
            }
            _ => false,
        }
    }

    /// The running context ends: its slot is freed (its stack is the caller's to reclaim)
    /// and the next ready context is returned.
    pub fn exit(&mut self, interrupted: u64) -> Result<u64, QueueError> {
        if self.current == 0 {
            return Err(QueueError::BootContext);
        }
        if let Some(s) = self.state.get_mut(self.current) {
            *s = State::Free;
        }
        if let Some(r) = self.record.get_mut(self.current) {
            *r = None;
        }
        self.balanced &= !bit(self.current);
        if let Some(a) = self.arrived.get_mut(self.current) {
            *a = None;
        }
        Ok(self.switch(interrupted, false))
    }

    /// The saved frame of the thread in `slot`, if that thread may move: it is ready and not the
    /// running one, and not slot 0 (ADR-0020 S6, R95-5). A mover reads it under this queue's
    /// lock, to lend that frame from the processor's table of contexts.
    pub fn frame(&self, slot: usize) -> Option<u64> {
        if slot == 0 || slot == self.current || self.state.get(slot) != Some(&State::Ready) {
            return None;
        }
        self.saved.get(slot).copied()
    }

    /// Whether a thread could arrive here: a slot is free (N13). A mover asks under this queue's
    /// lock, before it takes anything, so that [`RunQueue::arrive`] cannot then be refused.
    /// What a slot holds, for a proof's line when it fails (TASKS item 96, the migration proofs'
    /// state lines): its state, whether it holds a pending wake-up, whether it is marked woken, its
    /// pass, and this queue's floor and current slot. Read only; no path but a failing proof's calls
    /// it.
    pub fn describe(&self, slot: usize) -> SlotState {
        SlotState {
            state: match self.state.get(slot) {
                Some(State::Free) | None => "free",
                Some(State::Ready) => "ready",
                Some(State::Sleeping(_)) => "sleeping",
                Some(State::Blocked(_)) => "blocked",
            },
            pending: self.wake_pending.get(slot).copied().unwrap_or(false),
            woken: self.woken.get(slot).copied().unwrap_or(false),
            pass: self.pass.get(slot).copied().unwrap_or(0),
            floor: self.floor(),
            current: self.current,
        }
    }

    /// How many slots are ready, the current slot, whether slot 0 is the idle flow, and how many
    /// contexts the queue holds, for a failing proof's line ([`RunQueue::describe`]).
    pub fn describe_queue(&self) -> (usize, usize, bool, usize) {
        let ready = self
            .state
            .iter()
            .filter(|state| matches!(state, State::Ready))
            .count();
        (ready, self.current, self.idle, self.live())
    }

    pub fn has_room(&self) -> bool {
        (1..CAPACITY).any(|i| self.state.get(i) == Some(&State::Free))
    }

    /// Takes the thread in `slot` out of this queue, for another's [`RunQueue::arrive`] (ADR-0020
    /// S6; TASKS item 96). Only a thread [`RunQueue::frame`] answers for: ready, not running, not
    /// slot 0. `None`, and nothing changed, for any other: the running one, a sleeper, a blocked
    /// thread, a free slot, slot 0.
    ///
    /// Its lag is its pass less this queue's floor, the lowest pass among the ready contexts, its
    /// own included: at least 0. Its slot is freed, with no record, its pending wake-up and its
    /// woken mark cleared, so [`RunQueue::preempt_for_wake`] never names it. The caller moves the
    /// record's location in the same critical section (the module documentation).
    pub fn depart(&mut self, slot: usize) -> Option<Departed<R>> {
        let frame = self.frame(slot)?;
        let floor = self.floor();
        // If it was the last ready context that counts, the floor keeps the value it has now.
        self.kept_floor = self.kept_floor.max(floor);
        let departed = Departed {
            frame,
            weight: self.weight(slot),
            wake_pending: self.wake_pending(slot),
            lag: self.pass.get(slot).copied()?.saturating_sub(floor),
            record: self.record(slot),
            balanced: self.is_balanced(slot),
        };
        self.balanced &= !bit(slot);
        self.cold &= !bit(slot);
        self.mark_waiting &= !bit(slot);
        if let Some(a) = self.arrived.get_mut(slot) {
            *a = None;
        }
        if let Some(s) = self.state.get_mut(slot) {
            *s = State::Free;
        }
        if let Some(r) = self.record.get_mut(slot) {
            *r = None;
        }
        if let Some(w) = self.woken.get_mut(slot) {
            *w = false;
        }
        if let Some(p) = self.wake_pending.get_mut(slot) {
            *p = false;
        }
        Some(departed)
    }

    /// Places a thread that departed from another queue in the lowest free slot here, ready
    /// (ADR-0020 S6; TASKS item 96), and returns the slot. As a push, it gets a new generation and
    /// keeps its weight and record; it also keeps its pending wake-up, which its next block
    /// consumes (N1). Its pass is this queue's floor, read before it joins, plus its lag: no
    /// credit, no loss. It is marked woken, so a reschedule IPI to this processor runs it now if
    /// [`RunQueue::preempt_for_wake`] would run a thread woken here (the coordinating session's
    /// ruling Q11).
    ///
    /// [`QueueError::Full`] if no slot is free, and the thread is dropped with its `Departed`: a
    /// mover asks [`RunQueue::has_room`] under this queue's lock first, so it never meets that.
    pub fn arrive(&mut self, departed: Departed<R>) -> Result<usize, QueueError> {
        let Departed {
            frame,
            weight,
            wake_pending,
            lag,
            record,
            balanced,
        } = departed;
        let slot = self.push_weighted(frame, weight, record)?;
        if balanced {
            self.balanced |= bit(slot);
        }
        // It ran on the processor it left, perhaps a moment ago: it has not waited since the mark.
        self.mark_waiting &= !bit(slot);
        // Its stay here starts now, in this queue's ticks (ADR-0020 S10's move rate).
        if let Some(a) = self.arrived.get_mut(slot) {
            *a = Some(self.ticks);
        }
        if let Some(p) = self.pass.get_mut(slot) {
            *p = p.saturating_add(lag);
        }
        if let Some(p) = self.wake_pending.get_mut(slot) {
            *p = wake_pending;
        }
        if let Some(w) = self.woken.get_mut(slot) {
            *w = true;
        }
        Ok(slot)
    }

    /// Declares the thread in `slot` one that balancing may take (ADR-0020 S7; TASKS item 98): its
    /// spawner, under this queue's lock, after its push. A move carries the declaration, and the
    /// slot's next thread starts without it. Nothing for slot 0 or a free slot.
    pub fn set_balanced(&mut self, slot: usize) {
        if slot != 0 && self.state.get(slot).is_some_and(|s| *s != State::Free) {
            self.balanced |= bit(slot);
        }
    }

    /// Whether balancing may take the thread in `slot`.
    pub const fn is_balanced(&self, slot: usize) -> bool {
        self.balanced & bit(slot) != 0
    }

    /// Whether any thread here is one balancing may take: what an upcall asks before it looks
    /// any further.
    pub const fn any_balanced(&self) -> bool {
        self.balanced != 0
    }

    /// Whether this processor runs a context that counts: a thread, or slot 0 where it is the
    /// boot flow. `false` while its idle flow runs.
    pub const fn busy(&self) -> bool {
        !(self.idle && self.current == 0)
    }

    /// What could run here: the contexts that are running or ready, the idle flow aside, their
    /// weights' sum, and the threads balancing may take that are ready and not running. One scan
    /// of the slots.
    pub fn census(&self) -> Census {
        let mut census = Census::default();
        for slot in 0..CAPACITY {
            if self.state.get(slot) != Some(&State::Ready) || (self.idle && slot == 0) {
                continue;
            }
            census.runnable += 1;
            census.weight = census.weight.saturating_add(self.weight(slot));
            if slot != self.current {
                census.waiting |= self.balanced & bit(slot);
            }
        }
        census
    }

    /// The pass of `slot`: among threads a pull may take, the lowest has waited longest for its
    /// share.
    pub fn pass_of(&self, slot: usize) -> u64 {
        self.pass.get(slot).copied().unwrap_or(u64::MAX)
    }

    /// The ticks of this queue since the thread in `slot` arrived by a move; `None` for a thread
    /// that was pushed here.
    pub fn since_arrival(&self, slot: usize) -> Option<u64> {
        let arrived = self.arrived.get(slot).copied().flatten()?;
        Some(self.ticks.saturating_sub(arrived))
    }

    /// Whether the thread in `slot` has been here long enough for a move whose period is `period`
    /// ticks (ADR-0020 S10: "a thread moves at most once per period of the widest level the move
    /// crosses"): it never moved here, or more than `period` ticks of this queue have passed since
    /// it did. More than, so that its two moves are at least `period` whole ticks apart, however
    /// the ticks fall around them.
    pub fn settled(&self, slot: usize, period: u64) -> bool {
        self.since_arrival(slot).is_none_or(|ticks| ticks > period)
    }

    /// Whether the thread in `slot` has stayed off this processor since the mark: it was ready
    /// and not running at the mark, or was pushed since; it is ready and not running now; and
    /// nothing shows it ran in between. Every way a context leaves the processor shows: a tick or
    /// a yield charges its pass, and a block, a sleep or a wake-up's preemption notes it in `ran`.
    fn waited(&self, slot: usize) -> bool {
        let pass = self.pass.get(slot).copied();
        self.mark_waiting & bit(slot) != 0
            && self.ran & bit(slot) == 0
            && slot != self.current
            && self.state.get(slot) == Some(&State::Ready)
            && pass == self.mark_pass.get(slot).copied()
            // A pass at its largest cannot show a charge.
            && pass != Some(u64::MAX)
    }

    /// The threads balancing may take that have waited since the mark ([`RunQueue::waited`]).
    fn waited_since_mark(&self) -> u16 {
        (1..CAPACITY)
            .filter(|&slot| self.is_balanced(slot) && self.waited(slot))
            .fold(0, |waited, slot| waited | bit(slot))
    }

    /// Judges which waiting threads are not cache-hot, at `now` of this processor's own TSC
    /// (ADR-0020 S10's window `h`): if the mark is `h` or more behind, every thread balancing may
    /// take that has waited since the mark; and, whatever the mark's age, those judged so before
    /// that have not run since. Its own processor only: the mark's TSC is this processor's, and no
    /// other's reading may be compared with it (N10).
    ///
    /// At a tick it is called before the queue's step, so that a thread that waited the whole of
    /// the last tick is named, and [`RunQueue::mark`] after the step. It never names a thread
    /// that ran within `h`: a thread named was ready and not running from the mark to `now`, or
    /// has run nowhere at all. It leaves out threads that have in fact been off the processor
    /// longer: one that was blocked at the mark, and one that arrived by a move since it.
    pub fn judge(&mut self, now: u64, h: u64) {
        let mut cold = self.cold();
        if now.saturating_sub(self.mark_tsc) >= h {
            cold |= self.waited_since_mark();
        }
        self.cold = cold;
    }

    /// Sets the mark: this processor's own TSC, `now`; the contexts that are ready and not
    /// running; and every slot's pass. Its own processor only, at its tick, after the queue's
    /// step. The threads judged not cache-hot stay so while they still wait.
    pub fn mark(&mut self, now: u64) {
        let cold = self.cold();
        self.mark_tsc = now;
        self.mark_pass = self.pass;
        self.ran = 0;
        self.mark_waiting = (0..CAPACITY)
            .filter(|&slot| slot != self.current && self.state.get(slot) == Some(&State::Ready))
            .fold(0, |waiting, slot| waiting | bit(slot));
        self.cold = cold & self.mark_waiting;
    }

    /// Whether this queue's processor may be asked now to publish a waiting thread's frame
    /// (ADR-0020 Amendment 1: "a processor takes at most one ask a tick, however many processors
    /// are idle"): `true` for the first asker in each tick of this queue, which then sends the ask.
    /// The mark is the tick's number, so the next tick ends it and the processor has nothing to
    /// clear. An asker holds this queue's lock, as it does to find the thread it asks for.
    pub fn take_ask(&mut self) -> bool {
        if self.asked_at == Some(self.ticks) {
            return false;
        }
        self.asked_at = Some(self.ticks);
        true
    }

    /// Counts one ask sent to this queue's processor, apart from [`RunQueue::take_ask`]'s mark, so
    /// that the count would show a second ask in a tick if the mark ever failed to stop it.
    pub fn note_ask(&mut self) {
        if self.asks_counted_at != Some(self.ticks) {
            self.asks_counted_at = Some(self.ticks);
            self.asks_in_tick = 0;
        }
        self.asks_in_tick = self.asks_in_tick.saturating_add(1);
        self.asks_most = self.asks_most.max(self.asks_in_tick);
        self.asks = self.asks.saturating_add(1);
    }

    /// The asks sent to this queue's processor in all, and the most in one of its ticks.
    pub const fn asks(&self) -> (u64, u32) {
        (self.asks, self.asks_most)
    }

    /// The waiting threads that are not cache-hot, a bit a slot, as this queue's processor last
    /// judged them ([`RunQueue::judge`]) and that have not run since: for any holder of this
    /// queue's lock, a puller on another processor among them. A thread judged off the processor
    /// for the window has only been off it longer.
    pub fn cold(&self) -> u16 {
        self.cold & self.waited_since_mark()
    }

    /// Counts one balancing interrupt this queue's processor sent (a kick or an ask), against
    /// this queue's own ticks and apart from the framework's rate (`origin_sync::balance::Rate`),
    /// so that the count would show a second one in a tick if the rate ever failed to stop it.
    pub fn note_sent(&mut self) {
        if self.sent_counted_at != Some(self.ticks) {
            self.sent_counted_at = Some(self.ticks);
            self.sent_in_tick = 0;
        }
        self.sent_in_tick = self.sent_in_tick.saturating_add(1);
        self.sent_most = self.sent_most.max(self.sent_in_tick);
        self.sent = self.sent.saturating_add(1);
    }

    /// The balancing interrupts this queue's processor sent in all, and the most in one of its
    /// ticks.
    pub const fn sent(&self) -> (u64, u32) {
        (self.sent, self.sent_most)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // R3's judgement of the timed waits, at its edges and over a grid: deleted with the window's
    // first tests in `4c856ce3` and restored from `c0f0d02a` unchanged (the addendum review's
    // T66-R12).
    /// A wait of `waited_ms` on a processor that took `ticks` ticks over it.
    const fn wait(waited_ms: u64, ticks: u64) -> TimedWait {
        TimedWait { waited_ms, ticks }
    }

    const HZ: u64 = 100;

    #[test]
    fn a_timed_wait_in_bounds_passes_and_its_edges_hold() {
        let long = wait(18, 1);
        assert_eq!(
            judge_timed_waits(wait(59, 5), long, 0, HZ),
            RoundTrips::Pass
        );
        assert_eq!(
            judge_timed_waits(wait(50, 5), long, 0, HZ),
            RoundTrips::Pass
        );
        assert_eq!(
            judge_timed_waits(wait(80, 8), long, 0, HZ),
            RoundTrips::Pass
        );
        assert_eq!(
            judge_timed_waits(wait(60, 6), wait(499, 49), 0, HZ),
            RoundTrips::Pass
        );
        // Early is a fault a stall cannot make: it fails whatever the ticks.
        assert_eq!(
            judge_timed_waits(wait(49, 0), long, 1000, HZ),
            RoundTrips::Fail
        );
    }

    #[test]
    fn a_late_wait_with_its_ticks_there_fails_at_once() {
        // 81 ms with 8 ticks taken: none lost, so the kernel overslept.
        assert_eq!(
            judge_timed_waits(wait(81, 8), wait(18, 1), 0, HZ),
            RoundTrips::Fail
        );
        assert_eq!(
            judge_timed_waits(wait(122, 11), wait(18, 1), 0, HZ),
            RoundTrips::Fail
        );
        // Two ticks lost is not enough to excuse it.
        assert_eq!(
            judge_timed_waits(wait(100, 8), wait(18, 1), 0, HZ),
            RoundTrips::Fail
        );
        // The boot flow's absence excuses only the signalled wait, not the unsignalled one.
        assert_eq!(
            judge_timed_waits(wait(122, 11), wait(18, 1), 1000, HZ),
            RoundTrips::Fail
        );
        // The signalled wait late with its ticks there and the boot flow present fails.
        assert_eq!(
            judge_timed_waits(wait(59, 5), wait(600, 60), 0, HZ),
            RoundTrips::Fail
        );
    }

    #[test]
    fn a_late_wait_whose_processor_lost_ticks_is_measured_again() {
        // The 122 ms of 2026-11-14 as a stall: 5 ticks in 122 ms, 7 lost.
        assert_eq!(wait(122, 5).ticks_lost(HZ), 7);
        assert_eq!(
            judge_timed_waits(wait(122, 5), wait(18, 1), 0, HZ),
            RoundTrips::Repeat
        );
        assert_eq!(
            judge_timed_waits(wait(81, 5), wait(18, 1), 0, HZ),
            RoundTrips::Repeat
        );
        // The signalled wait late: its processor's lost ticks, or the boot flow away.
        assert_eq!(
            judge_timed_waits(wait(59, 5), wait(600, 20), 0, HZ),
            RoundTrips::Repeat
        );
        assert_eq!(
            judge_timed_waits(wait(59, 5), wait(600, 60), DISTURBED_PER_MILLE, HZ),
            RoundTrips::Repeat
        );
        // Both late, both excused.
        assert_eq!(
            judge_timed_waits(wait(150, 5), wait(700, 10), 0, HZ),
            RoundTrips::Repeat
        );
        // Both late, one excused: fails.
        assert_eq!(
            judge_timed_waits(wait(150, 5), wait(700, 70), 0, HZ),
            RoundTrips::Fail
        );
    }

    #[test]
    fn over_a_grid_only_a_wait_in_bounds_passes() {
        for short in (0..300).step_by(7) {
            for short_ticks in 0..30 {
                for long in [5, 20, 499, 500, 900] {
                    for long_ticks in [0, 1, 40, 90] {
                        for away in [0, 99, 100, 900] {
                            let verdict = judge_timed_waits(
                                wait(short, short_ticks),
                                wait(long, long_ticks),
                                away,
                                HZ,
                            );
                            let in_bounds = (50..=80).contains(&short) && long < 500;
                            assert_eq!(
                                verdict == RoundTrips::Pass,
                                in_bounds,
                                "{short} ms/{short_ticks}, {long} ms/{long_ticks}, {away}"
                            );
                            if short < 50 {
                                assert_eq!(verdict, RoundTrips::Fail);
                            }
                        }
                    }
                }
            }
        }
    }

    /// A watch over ticks at the given gaps, from TSC 0, with a 10 ms period on a 1 GHz TSC,
    /// its timer read back as `read_back` at the first tick.
    fn watched_as(gaps: &[u64], read_back: ReadBack) -> TickWatch {
        let mut w = TickWatch::armed(0, 10_000_000);
        let mut at = 0;
        for &gap in gaps {
            at += gap;
            if w.first_tick() {
                w.note_read_back(read_back);
            }
            w.note(at);
            // A switch at every tick, as the proof's two workers and their processor's own
            // context make.
            w.note_switches(at, w.ticks);
        }
        w
    }

    fn watched(gaps: &[u64]) -> TickWatch {
        watched_as(gaps, ReadBack::AsTimed)
    }

    const P: u64 = 10_000_000;
    const HALF_SECOND: u64 = 500_000_000;

    #[test]
    fn a_window_at_the_programmed_rate_is_in_bounds() {
        let w = watched(&[P; 50]);
        assert_eq!((w.ticks, w.longest, w.at_switches), (50, P, 15 * P));
        assert_eq!(judge_window(&w, 50, HALF_SECOND), WindowVerdict::InBounds);
        // Jitter in the ticks' delivery changes nothing: a third of the gaps short, a third long.
        let jittery: alloc::vec::Vec<u64> = (0..50)
            .map(|i| [P * 7 / 10, P, P * 13 / 10][i % 3])
            .collect();
        assert_eq!(
            judge_window(&watched(&jittery), 50, HALF_SECOND),
            WindowVerdict::InBounds
        );
        // A watch that is not armed notes nothing, and reads nothing.
        let mut idle = TickWatch::new();
        idle.note(5);
        idle.note_read_back(ReadBack::AsTimed);
        assert_eq!((idle.ticks, idle.read_back), (0, ReadBack::Unread));
    }

    #[test]
    fn a_timer_programmed_otherwise_fails_at_once() {
        // The review's T66-R1: any register read back otherwise than timed (which ones differ is
        // `origin_frame::apic`'s tests'): divide-by-128 for 16, another count, a timer left in
        // one-shot mode or masked.
        let wrongs = [
            ReadBack::Otherwise("divide configuration", 0b1010, 0b0011),
            ReadBack::Otherwise("initial count", 5_000_000, 625_000),
            ReadBack::Otherwise("initial count", 0, 625_000),
            ReadBack::Otherwise("LVT entry", 0x40, (1 << 17) | 0x40),
            ReadBack::Otherwise("LVT entry", (1 << 17) | (1 << 16) | 0x40, (1 << 17) | 0x40),
        ];
        for wrong in wrongs {
            let w = watched_as(&[P; 50], wrong);
            assert_eq!(judge_window(&w, 50, HALF_SECOND), WindowVerdict::WrongRate);
        }
        // A misprogrammed timer is named even when it also stops ticking, as one in one-shot mode
        // does after its first.
        let once = watched_as(
            &[P],
            ReadBack::Otherwise("LVT entry", 0x40, (1 << 17) | 0x40),
        );
        assert_eq!(
            judge_window(&once, 50, HALF_SECOND),
            WindowVerdict::WrongRate
        );
        // Not read back at all.
        let w = watched_as(&[P; 50], ReadBack::Unread);
        assert_eq!(judge_window(&w, 50, HALF_SECOND), WindowVerdict::WrongRate);
        // Read at the first tick only: a later change of the register is not this check's.
        let mut w = TickWatch::armed(0, P);
        w.note_read_back(ReadBack::AsTimed);
        w.note(P);
        assert!(!w.first_tick());
    }

    #[test]
    fn the_rounds_clause_stamps_the_fifteenth_switch_not_the_fifteenth_tick() {
        // 50 ticks `gap` apart, armed with the programmed 10 ms period, switching at every
        // `every`th tick.
        let watched_switching = |every: u64, gap: u64| {
            let mut w = TickWatch::armed(0, P);
            for k in 1..=50 {
                if w.first_tick() {
                    w.note_read_back(ReadBack::AsTimed);
                }
                w.note(k * gap);
                w.note_switches(k * gap, k / every);
            }
            w
        };
        // Switching at every other tick: the 15th switch at 300 ms, in bounds.
        let half = watched_switching(2, P);
        assert_eq!(half.at_switches, 30 * P);
        assert_eq!(
            judge_window(&half, 50, HALF_SECOND),
            WindowVerdict::InBounds
        );
        // At every third tick of a 12 ms tick: its 15th tick at 180 ms met the old stamp, its
        // 15th switch at 540 ms does not, and no gap of two periods excuses it: late.
        let third = watched_switching(3, 12 * P / 10);
        assert_eq!(third.at_switches, 540_000_000);
        assert_eq!(judge_window(&third, 50, HALF_SECOND), WindowVerdict::Late);
        // Fewer than 15 switches in the whole window fails, whatever else.
        let few = watched_switching(5, P);
        assert_eq!(few.at_switches, 0);
        assert_eq!(judge_window(&few, 50, HALF_SECOND), WindowVerdict::Late);
        // The addendum review's T66-R15: a 40 Hz tick that reads back as timed, switching at
        // every other tick, whose 15th tick at 375 ms met the old stamp. Its 15th switch is at
        // 750 ms, with gaps of 2.5 periods: never a pass, and stalled in every run, so it fails at
        // the proof's last run.
        let slow = watched_switching(2, 25 * P / 10);
        assert_eq!(slow.at_switches, 750_000_000);
        assert_eq!(judge_window(&slow, 50, HALF_SECOND), WindowVerdict::Stalled);
        // The switches counted are those since the watch was armed.
        let mut q = RunQueue::new();
        q.arm_watch(0, P);
        assert_eq!(q.watch().switches_from, q.switches());
    }

    /// The verdict and the runs a proof makes over `runs`, each a verdict, its correctness and how
    /// long it took with the pause after it, on a 1 GHz TSC: as `sched::prove` loops.
    fn budgeted(runs: &[(WindowVerdict, bool, u64)]) -> (WindowVerdict, u32, u64) {
        const HZ_TSC: u64 = 1_000_000_000;
        let mut elapsed = 0;
        let mut made = 0;
        for &(verdict, correct, seconds) in runs {
            made += 1;
            elapsed += seconds * HZ_TSC;
            if !window_again(verdict, correct, made, elapsed, HZ_TSC) {
                return (verdict, made, elapsed / HZ_TSC);
            }
            elapsed += WINDOW_PAUSE_SECONDS * HZ_TSC;
        }
        (WindowVerdict::Stalled, made, elapsed / HZ_TSC)
    }

    #[test]
    fn the_window_repeats_within_its_time_budget_and_its_cap() {
        use WindowVerdict::{InBounds, Late, Stalled};
        // A pass on a later run counts: two stalled runs, then one in bounds.
        assert_eq!(
            budgeted(&[(Stalled, true, 3), (Stalled, true, 3), (InBounds, true, 1)]),
            (InBounds, 3, 11)
        );
        // Stalled in every run of 3 s: with the 2 s pauses, 5 s a run, the 24th ends at 118 s
        // and the 25th could not start within 120 s.
        let stalled = [(Stalled, true, 3); 40];
        assert_eq!(budgeted(&stalled), (Stalled, 24, 118));
        // Short runs stop at the cap of 32.
        let quick = [(Stalled, true, 1); 40];
        assert_eq!(budgeted(&quick).1, WINDOW_RUNS_MOST);
        // The budget's edge: a run ending 118 s in, plus the 2 s pause, is not under 120 s.
        const HZ_TSC: u64 = 1_000_000_000;
        assert!(window_again(Stalled, true, 1, 117 * HZ_TSC, HZ_TSC));
        assert!(!window_again(Stalled, true, 1, 118 * HZ_TSC, HZ_TSC));
        assert!(!window_again(Stalled, true, WINDOW_RUNS_MOST, 0, HZ_TSC));
        // Only a correct stalled run is repeated: a failure of correctness, a late run without
        // a stall, or a pass ends the runs at once.
        assert!(!window_again(Stalled, false, 1, 0, HZ_TSC));
        assert!(!window_again(Late, true, 1, 0, HZ_TSC));
        assert!(!window_again(InBounds, true, 1, 0, HZ_TSC));
        assert_eq!(
            budgeted(&[(Stalled, true, 3), (Stalled, false, 3)]),
            (Stalled, 2, 8)
        );
    }

    #[test]
    fn a_stall_before_the_rounds_earns_a_repeat_and_never_a_pass() {
        // 400 ms away after the 5th tick: the 15th comes at 540 ms, past today's bound.
        let mut gaps = [P; 50];
        gaps[5] = 400_000_000;
        let w = watched(&gaps);
        assert_eq!(judge_window(&w, 50, HALF_SECOND), WindowVerdict::Stalled);
        // The same stall after the 15th tick changes nothing: today's bound was met.
        let mut later = [P; 50];
        later[20] = 400_000_000;
        assert_eq!(
            judge_window(&watched(&later), 50, HALF_SECOND),
            WindowVerdict::InBounds
        );
        // Late with no gap of two periods: no stall to blame, so it fails.
        let late = watched(&[P * 19 / 10; 50]);
        assert_eq!(judge_window(&late, 50, P * 15), WindowVerdict::Late);
        // Slow enough for every gap to be two periods: stalled in every run, so it fails at the
        // proof's last run (the kernel's budget of runs), never passing.
        let slow = watched(&[8 * P; 50]);
        assert_eq!(judge_window(&slow, 50, HALF_SECOND), WindowVerdict::Stalled);
    }

    #[test]
    fn a_processor_short_of_its_ticks_fails() {
        assert_eq!(
            judge_window(&watched(&[P; 49]), 50, HALF_SECOND),
            WindowVerdict::Short
        );
        assert_eq!(
            judge_window(&watched(&[]), 50, HALF_SECOND),
            WindowVerdict::Short
        );
    }

    #[test]
    fn a_lone_context_keeps_running() {
        let mut q = RunQueue::new();
        assert_eq!(q.tick(0x1000), 0x1000);
        assert_eq!(q.yield_now(0x2000), 0x2000);
        assert_eq!(q.switches(), 0);
        assert_eq!(q.live(), 1);
        assert_eq!(q.sleep(0x2000, 3), Err(QueueError::BootContext));
        assert_eq!(q.exit(0x2000), Err(QueueError::BootContext));
    }

    #[test]
    fn round_robin_resumes_each_saved_frame_in_turn() {
        let mut q = RunQueue::new();
        assert_eq!(q.push(0xA000), Ok(1));
        assert_eq!(q.push(0xB000), Ok(2));
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(q.tick(0xA100), 0xB000);
        assert_eq!(q.tick(0xB200), 0x1000);
        assert_eq!(q.yield_now(0x1100), 0xA100);
        assert_eq!(q.tick(0xA300), 0xB200);
        assert_eq!(q.switches(), 5);
    }

    #[test]
    fn every_context_gets_an_equal_share() {
        let mut q = RunQueue::new();
        for i in 1..CAPACITY as u64 {
            q.push(i * 0x1000).unwrap();
        }
        assert_eq!(q.push(0x9_0000), Err(QueueError::Full));
        let mut runs = [0u32; CAPACITY];
        let mut running = 0u64;
        for _ in 0..CAPACITY * 100 {
            let next = q.tick(running);
            runs[(next / 0x1000) as usize] += 1;
            running = next;
        }
        assert!(runs.iter().all(|&r| r == 100), "{runs:?}");
    }

    #[test]
    fn sleepers_wake_after_their_ticks_and_not_before() {
        let mut q = RunQueue::new();
        q.push(0xA000).unwrap();
        assert_eq!(q.tick(0x1000), 0xA000); // tick 1: A runs
        assert_eq!(q.sleep(0xA100, 3), Ok(0x1000)); // A sleeps until tick 4
        assert_eq!(q.live(), 2);
        assert_eq!(q.tick(0x1100), 0x1100); // tick 2: only the boot context is ready
        assert_eq!(q.tick(0x1200), 0x1200); // tick 3
        assert_eq!(q.tick(0x1300), 0xA100, "tick 4: A is due again");
    }

    #[test]
    fn exit_frees_the_slot_for_reuse() {
        let mut q = RunQueue::new();
        q.push(0xA000).unwrap();
        q.push(0xB000).unwrap();
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(q.exit(0xA100), Ok(0xB000));
        assert_eq!(q.live(), 2);
        assert_eq!(q.push(0xC000), Ok(1), "A's slot is reused");
        // The new thread starts level with the others: equal shares from here on.
        let runs = run(&mut q, 30);
        assert_eq!((runs[0], runs[1], runs[2]), (10, 10, 10), "{runs:?}");
    }

    #[test]
    fn a_blocked_thread_runs_only_after_its_wake() {
        let mut q = RunQueue::new();
        let slot = q.push(0xA000).unwrap();
        let generation = q.generation(slot);
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(
            q.block(0xA100),
            Ok(0x1000),
            "A blocks; the boot context runs"
        );
        for t in 0..5 {
            assert_eq!(
                q.tick(0x1100 + t),
                0x1100 + t,
                "ticks do not wake a blocked thread"
            );
        }
        assert_eq!(q.live(), 2);
        assert!(q.wake(slot, generation), "a blocked thread: reschedule");
        assert_eq!(q.tick(0x1200), 0xA100, "A resumes where it blocked");
        assert!(!q.wake(slot, generation), "running: only a pending wake-up");
        assert_eq!(
            q.block(0xA200),
            Ok(0xA200),
            "the pending wake-up is consumed at once"
        );
        assert_eq!(q.block(0xA300), Ok(0x1200), "the next block really blocks");
    }

    #[test]
    fn a_timed_block_ends_at_its_deadline_or_at_a_wake_up() {
        let mut q = RunQueue::new();
        let slot = q.push(0xA000).unwrap();
        let generation = q.generation(slot);
        assert_eq!(q.tick(0x1000), 0xA000); // tick 1
        assert_eq!(
            q.block_for(0xA100, Some(3)),
            Ok(0x1000),
            "blocked until tick 4"
        );
        assert_eq!(q.tick(0x1100), 0x1100); // tick 2
        assert_eq!(q.tick(0x1200), 0x1200); // tick 3
        assert_eq!(q.tick(0x1300), 0xA100, "tick 4: the deadline passed");
        assert!(
            !q.wake(slot, generation),
            "running again: a late wake-up is only kept"
        );
        assert_eq!(
            q.block_for(0xA200, Some(100)),
            Ok(0xA200),
            "and consumed by the next block"
        );
        assert_eq!(q.block_for(0xA300, Some(100)), Ok(0x1300));
        assert!(q.wake(slot, generation), "a wake-up before the deadline");
        assert_eq!(q.tick(0x1400), 0xA300);
        assert_eq!(
            q.block_for(0xA400, Some(0)),
            Ok(0x1400),
            "zero ticks still waits one"
        );
        assert_eq!(q.tick(0x1500), 0xA400);
    }

    #[test]
    fn a_stale_wake_never_reaches_a_later_thread_in_the_slot() {
        let mut q = RunQueue::new();
        let slot = q.push(0xA000).unwrap();
        let old = q.generation(slot);
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(q.exit(0xA100), Ok(0x1000));
        assert!(!q.wake(slot, old), "free slot");
        assert_eq!(q.push(0xB000), Ok(slot));
        let new = q.generation(slot);
        assert_ne!(new, old);
        assert_eq!(q.tick(0x1100), 0xB000);
        assert!(!q.wake(slot, old), "the old thread's wake-up");
        assert_eq!(
            q.block(0xB100),
            Ok(0x1100),
            "and so no pending wake-up either"
        );
        assert!(q.wake(slot, new));
        assert!(!q.wake(0, 0), "the boot context is never blocked");
        assert_eq!(q.block(0x1200), Err(QueueError::BootContext));
    }

    /// Runs `ticks` ticks and counts how often each slot ran.
    fn run(q: &mut RunQueue, ticks: usize) -> [u32; CAPACITY] {
        let mut runs = [0u32; CAPACITY];
        let mut running = q.saved[q.current()];
        for _ in 0..ticks {
            running = q.tick(running);
            runs[q.current()] += 1;
        }
        runs
    }

    #[test]
    fn shares_follow_the_weights_exactly() {
        let mut q = RunQueue::new();
        q.set_idle(true);
        for (i, w) in [1u32, 4, 16].into_iter().enumerate() {
            q.push_weighted(0x1000 * (i as u64 + 1), w, None).unwrap();
        }
        // 21 quanta make one full cycle of strides 16 : 4 : 1.
        let runs = run(&mut q, 2100);
        assert_eq!(
            (runs[0], runs[1], runs[2], runs[3]),
            (0, 100, 400, 1600),
            "{runs:?}"
        );
        // Changing a weight changes the shares from then on.
        // Weights 16 : 4 : 16 make a 36-quantum cycle.
        q.set_weight(1, 16);
        let runs = run(&mut q, 3600);
        let (a, b, c) = (runs[1], runs[2], runs[3]);
        assert!(
            a.abs_diff(1600) <= 2 && b.abs_diff(400) <= 2 && c.abs_diff(1600) <= 2,
            "{runs:?}"
        );
    }

    #[test]
    fn the_idle_context_runs_only_when_nothing_else_is_ready() {
        let mut q = RunQueue::new();
        q.set_idle(true);
        let a = q.push(0xA000).unwrap();
        assert_eq!(run(&mut q, 50)[0], 0, "never while A is ready");
        let frame = q.saved[a];
        assert_eq!(
            q.sleep(frame, 10).map(|_| q.current()),
            Ok(0),
            "A sleeps: idle runs"
        );
        let runs = run(&mut q, 20);
        assert_eq!(
            (runs[0], runs[a]),
            (9, 11),
            "idle until A is due, then A only"
        );
        // Not idle: slot 0 takes its turn like any context.
        q.set_idle(false);
        let runs = run(&mut q, 20);
        assert_eq!((runs[0], runs[a]), (10, 10));
    }

    #[test]
    fn sleeping_earns_no_credit_and_new_threads_start_level() {
        let mut q = RunQueue::new();
        q.set_idle(true);
        let a = q.push(0xA000).unwrap();
        let b = q.push(0xB000).unwrap();
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(
            q.sleep(0xA100, 100),
            Ok(0xB000),
            "A sleeps until tick 101; B runs alone"
        );
        let runs = run(&mut q, 99);
        assert_eq!(runs[b], 99);
        // A is back: it does not get 100 quanta in a row to catch up; the two alternate.
        let runs = run(&mut q, 40);
        assert_eq!((runs[a], runs[b]), (20, 20), "{runs:?}");
        // A thread added now starts level with them: a third each.
        let c = q.push(0xC000).unwrap();
        let runs = run(&mut q, 300);
        assert_eq!((runs[a], runs[b], runs[c]), (100, 100, 100), "{runs:?}");
        // Weights are clamped.
        q.set_weight(c, 0);
        assert_eq!(q.weight(c), 1);
        q.set_weight(c, u32::MAX);
        assert_eq!(q.weight(c), MAX_WEIGHT);
    }

    #[test]
    fn the_share_bound_is_the_one_the_boot_proof_always_used() {
        assert_eq!(per_mille(1, 21), 47);
        assert_eq!(per_mille(4, 21), 190);
        assert_eq!(per_mille(16, 21), 761);
        assert_eq!(per_mille(0, 0), 0, "nothing of nothing is nothing");
        // Ideal 47: 17 to 77 per mille; 190: 112 to 268; 761: 493 to 1029.
        for (weight, low, high) in [(1, 17, 77), (4, 112, 268), (16, 493, 1029)] {
            assert!(share_near_weight(low, weight, 21), "{weight}: {low}");
            assert!(share_near_weight(high, weight, 21), "{weight}: {high}");
            assert!(!share_near_weight(low - 1, weight, 21), "{weight}: below");
            assert!(!share_near_weight(high + 1, weight, 21), "{weight}: above");
        }
    }

    #[test]
    fn a_disturbance_earns_a_repeat_and_never_a_pass() {
        assert_eq!(ROUND_TRIP_LIMIT_MICROS, 2000);
        // Under the bound passes, disturbed or not.
        assert_eq!(judge_round_trips(1999, 0), RoundTrips::Pass);
        assert_eq!(judge_round_trips(1999, 1000), RoundTrips::Pass);
        // At or over it, a run that shows no disturbance fails at once...
        assert_eq!(judge_round_trips(2000, 0), RoundTrips::Fail);
        assert_eq!(
            judge_round_trips(2000, DISTURBED_PER_MILLE - 1),
            RoundTrips::Fail
        );
        assert_eq!(judge_round_trips(50_000, 0), RoundTrips::Fail);
        // ... and one that shows a disturbance is measured again, however disturbed.
        assert_eq!(
            judge_round_trips(2000, DISTURBED_PER_MILLE),
            RoundTrips::Repeat
        );
        assert_eq!(judge_round_trips(50_000, 1000), RoundTrips::Repeat);
        // Over any mix of inputs, only a mean under the bound passes.
        for mean in [0, 1, 1999, 2000, 2001, 1_000_000, u64::MAX] {
            for away in [0, 99, 100, 101, 1000, u64::MAX] {
                assert_eq!(
                    judge_round_trips(mean, away) == RoundTrips::Pass,
                    mean < 2000
                );
            }
        }
        // The tenth of a run away that earns a repeat.
        assert_eq!(DISTURBED_PER_MILLE, 100);
        assert_eq!(per_mille(99, 1000), 99);
        assert_eq!(per_mille(100, 1000), DISTURBED_PER_MILLE);
    }

    #[test]
    fn an_interrupt_that_woke_nothing_charges_nothing_and_resumes_the_same_context() {
        let mut q = RunQueue::new();
        q.push(0xA000).unwrap();
        q.push(0xB000).unwrap();
        assert_eq!(q.tick(0x1000), 0xA000);
        let before = (q.switches(), q.pass);
        for _ in 0..1000 {
            assert_eq!(
                q.preempt_for_wake(0xA100),
                0xA100,
                "the same thread goes on"
            );
        }
        assert_eq!((q.switches(), q.pass), before, "no switch, nothing charged");
    }

    #[test]
    fn interrupts_change_neither_the_shares_nor_the_number_of_switches() {
        let mut quiet = RunQueue::new();
        let mut busy = RunQueue::new();
        for q in [&mut quiet, &mut busy] {
            q.set_idle(true);
            for (i, w) in [1u32, 4, 16].into_iter().enumerate() {
                q.push_weighted(0x1000 * (i as u64 + 1), w, None).unwrap();
            }
        }
        // 21 quanta make one full cycle of strides 16 : 4 : 1. On `busy`, the running thread
        // also takes ten interrupts every quantum, none of them waking anything.
        let mut runs = [0u32; CAPACITY];
        let mut running = busy.saved[busy.current()];
        for _ in 0..2100 {
            quiet.tick(quiet.saved[quiet.current()]);
            running = busy.tick(running);
            for _ in 0..10 {
                assert_eq!(busy.preempt_for_wake(running), running);
            }
            runs[busy.current()] += 1;
        }
        assert_eq!((runs[0], runs[1], runs[2], runs[3]), (0, 100, 400, 1600));
        assert_eq!(busy.switches(), quiet.switches());
        assert_eq!(busy.pass, quiet.pass);
    }

    #[test]
    fn a_woken_thread_that_should_run_now_takes_over_uncharged() {
        let mut q = RunQueue::new();
        q.set_idle(true);
        let a = q.push(0xA000).unwrap();
        q.push(0xB000).unwrap();
        let generation = q.generation(a);
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(q.block(0xA100), Ok(0xB000), "A blocks; B runs");
        let before = q.pass;
        assert!(q.wake(a, generation));
        assert_eq!(q.preempt_for_wake(0xB100), 0xA100, "A is level: it runs");
        assert_eq!(q.current(), a);
        assert_eq!(q.pass, before, "and neither of them is charged for it");
        // B's interrupted frame was saved: it is B's turn again after A blocks.
        assert_eq!(q.block(0xA200), Ok(0xB100));
    }

    #[test]
    fn a_woken_thread_behind_the_running_context_waits_for_its_turn() {
        let mut q = RunQueue::new();
        q.set_idle(true);
        let a = q.push(0xA000).unwrap();
        q.push(0xB000).unwrap();
        let generation = q.generation(a);
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(q.block(0xA100), Ok(0xB000));
        q.pass[a] = 5 * STRIDE_ONE; // A has used far more than B
        let switches = q.switches();
        assert!(q.wake(a, generation));
        assert_eq!(
            q.preempt_for_wake(0xB100),
            0xB100,
            "not ahead of B: B goes on"
        );
        assert_eq!(q.switches(), switches);
        // It is ready all the same: B runs until A's pass is the lowest.
        let mut running = 0xB100;
        for _ in 0..50 {
            running = q.tick(running);
            if q.current() == a {
                break;
            }
        }
        assert_eq!(q.current(), a, "A gets its turn at a tick");
    }

    #[test]
    fn the_idle_context_gives_way_to_any_woken_thread_whatever_its_pass() {
        let mut q = RunQueue::new();
        q.set_idle(true);
        let a = q.push(0xA000).unwrap();
        let generation = q.generation(a);
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(q.block(0xA100), Ok(0x1000), "A blocks; only idle is left");
        q.pass[a] = 1 << 40;
        q.pass[0] = 0;
        assert!(q.wake(a, generation));
        assert_eq!(q.preempt_for_wake(0x1100), 0xA100);
        assert_eq!(q.current(), a);
    }

    #[test]
    fn the_woken_thread_wins_a_tie_ahead_of_another_ready_one() {
        let mut q = RunQueue::new();
        q.set_idle(true);
        let woken = q.push(0xA000).unwrap();
        let running = q.push(0xB000).unwrap();
        let other = q.push(0xC000).unwrap();
        let generation = q.generation(woken);
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(q.block(0xA100), Ok(0xB000));
        assert_eq!(q.current(), running);
        assert_eq!(q.pass[running], q.pass[other]);
        assert!(q.wake(woken, generation));
        assert_eq!(q.pass[woken], q.pass[running]);
        // `other` comes first round-robin from `running`, but only `woken` was woken.
        assert_eq!(q.preempt_for_wake(0xB100), 0xA100);
        assert_eq!(q.current(), woken);
    }

    #[test]
    fn a_wake_up_a_later_choice_has_seen_does_not_preempt_again() {
        let mut q = RunQueue::new();
        q.set_idle(true);
        let a = q.push(0xA000).unwrap();
        q.push(0xB000).unwrap();
        let generation = q.generation(a);
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(q.block(0xA100), Ok(0xB000));
        assert!(q.wake(a, generation));
        // The next timer tick chooses before the IPI arrives; the IPI then finds nothing.
        q.tick(0xB100);
        let current = q.current();
        let switches = q.switches();
        assert_eq!(q.preempt_for_wake(0xF000), 0xF000);
        assert_eq!((q.current(), q.switches()), (current, switches));
    }

    #[test]
    fn bad_contexts_are_refused() {
        let mut q = RunQueue::new();
        assert_eq!(q.push(0), Err(QueueError::BadContext(0)));
        assert_eq!(q.push(0x1004), Err(QueueError::BadContext(0x1004)));
        assert_eq!(q.live(), 1);
    }

    /// A refusal's `Debug` names it and never prints the frame `BadContext` carries, in any
    /// base: the scheduler's proof prints a `Result<usize, QueueError>` (T76-80-3).
    #[test]
    fn a_refusal_s_debug_prints_no_frame_address() {
        let frame = 0xffff_c000_0001_fff4_u64;
        let refused: Result<usize, QueueError> = Err(QueueError::BadContext(frame));
        let text = alloc::format!("{refused:?}");
        assert_eq!(text, "Err(BadContext(..))");
        for shown in [
            alloc::format!("{frame:x}"),
            alloc::format!("{frame:X}"),
            alloc::format!("{frame}"),
        ] {
            assert!(!text.contains(shown.as_str()), "{text}");
        }
        assert_eq!(alloc::format!("{:?}", QueueError::Full), "Full");
        assert_eq!(
            alloc::format!("{:?}", QueueError::BootContext),
            "BootContext"
        );
    }

    /// What a failing proof's line reads of a slot and a queue (TASKS item 96's state lines).
    #[test]
    fn a_slot_and_a_queue_are_described_as_they_are() {
        let mut q = RunQueue::new();
        let a = q.push_weighted(0x1000, DEFAULT_WEIGHT, Some(7)).unwrap();
        let b = q.push(0x2000).unwrap();
        assert_eq!(q.describe(a).state, "ready");
        assert_eq!(q.describe(5).state, "free");
        assert_eq!(q.describe(CAPACITY).state, "free", "past the queue");
        assert_eq!(q.describe(a).current, 0);
        assert_eq!(q.tick(0x9000), 0x1000);
        assert!(q.block(0x1000).is_ok());
        let blocked = q.describe(a);
        assert_eq!((blocked.state, blocked.pending), ("blocked", false));
        let (ready, current, _, live) = q.describe_queue();
        assert_eq!((ready, live), (2, 3), "slot 0 and b ready, a blocked");
        assert!(current == 0 || current == b, "one of the ready two runs");
    }

    /// `held` hands back the very record a slot was given, not one equal to it, so the kernel's
    /// record check, which compares the record's address (`sched::KernelQueues::holds`), tells
    /// apart two records that compare equal in value (the review of TASKS item 96, R96-11).
    #[test]
    fn held_hands_back_the_record_itself() {
        static MINE: u32 = 7;
        static OTHER: u32 = 7;
        let mut q: RunQueue<&'static u32> = RunQueue::empty();
        let mine = q
            .push_weighted(0x1000, DEFAULT_WEIGHT, Some(&MINE))
            .unwrap();
        let other = q
            .push_weighted(0x2000, DEFAULT_WEIGHT, Some(&OTHER))
            .unwrap();
        let is = |slot: usize, record: &'static u32| {
            q.held(slot).is_some_and(|held| core::ptr::eq(held, record))
        };
        assert!(
            is(mine, &MINE) && is(other, &OTHER),
            "each slot's own record"
        );
        assert!(
            !is(mine, &OTHER) && !is(other, &MINE),
            "equal in value, not the same"
        );
        assert!(
            !is(0, &MINE) && !is(5, &MINE),
            "slot 0 and a free slot hold none"
        );
    }

    /// Each slot answers the record its thread was pushed with (ADR-0020 S1, TASKS item 92);
    /// slot 0, a free slot and a thread pushed without one answer none.
    #[test]
    fn a_slot_answers_its_thread_s_record_until_the_thread_exits() {
        let mut q = RunQueue::new();
        assert_eq!(q.record(0), None, "the boot or idle flow has no record");
        let a = q.push_weighted(0x1000, DEFAULT_WEIGHT, Some(7)).unwrap();
        let b = q.push(0x2000).unwrap();
        assert_eq!((q.record(a), q.record(b)), (Some(7), None));
        assert_eq!(q.record(5), None, "a free slot");
        assert_eq!((q.held(0), q.held(a), q.held(5)), (None, Some(7), None));
        assert_eq!(q.record(CAPACITY), None, "no such slot");
        // `a` runs and exits: from the critical section that freed its slot on, nothing finds
        // its record through the slot.
        assert_eq!(q.tick(0x9000), 0x1000);
        assert_eq!(q.current(), a);
        q.exit(0x1000).unwrap();
        assert_eq!(q.record(a), None);
        // Read without the state, it answers the same: an exit clears it (TASKS item 96).
        assert_eq!(q.held(a), None);
        // The slot's next thread has its own.
        let c = q.push_weighted(0x3000, DEFAULT_WEIGHT, Some(40)).unwrap();
        assert_eq!(c, a, "the lowest free slot is reused");
        assert_eq!(q.record(c), Some(40));
        assert_eq!(q.generation(c), 2);
    }

    /// The pending wake-up of a thread not yet blocked can be read, and reading it changes
    /// nothing: its next block consumes it as before.
    #[test]
    fn a_pending_wake_up_can_be_read_without_consuming_it() {
        let mut q = RunQueue::new();
        let slot = q.push(0x1000).unwrap();
        assert!(!q.wake_pending(slot));
        assert_eq!(q.tick(0x9000), 0x1000);
        assert!(
            !q.wake(slot, q.generation(slot)),
            "ready: the wake-up is kept"
        );
        assert!(q.wake_pending(slot));
        assert!(q.wake_pending(slot), "a read consumes nothing");
        assert_eq!(q.block(0x1000), Ok(0x1000), "the block returns at once");
        assert!(!q.wake_pending(slot));
        assert!(!q.wake_pending(0) && !q.wake_pending(CAPACITY));
    }

    /// A queue whose slot 1 holds a thread that is ready and not running, after `ticks` of its
    /// own and the boot context's: with equal weights its pass is then level with or above the
    /// floor. Returns the queue and the thread's frame.
    fn one_ready(record: u32) -> (RunQueue, u64) {
        let mut q = RunQueue::new();
        assert_eq!(q.push_weighted(0xA000, DEFAULT_WEIGHT, Some(record)), Ok(1));
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(q.tick(0xA100), 0x1000, "slot 1 is ready and not running");
        (q, 0xA100)
    }

    /// Only a ready thread that is not running moves (ADR-0020 S6, R95-5): `frame` and `depart`
    /// answer nothing, and change nothing, for slot 0, the running slot, a sleeper, a blocked
    /// thread, a free slot and a slot past the end.
    #[test]
    fn depart_takes_only_a_ready_thread_that_is_not_running() {
        let mut q = RunQueue::new();
        let sleeper = q.push(0xA000).unwrap();
        let blocked = q.push(0xB000).unwrap();
        let running = q.push(0xC000).unwrap();
        assert_eq!(q.tick(0x1000), 0xA000);
        assert_eq!(q.sleep(0xA100, 5), Ok(0xB000));
        assert_eq!(q.block(0xB100), Ok(0xC000));
        assert_eq!(q.current(), running);
        let before = q;
        for slot in [0, running, sleeper, blocked, 9, CAPACITY] {
            assert_eq!(q.frame(slot), None, "slot {slot}");
            assert!(q.depart(slot).is_none(), "slot {slot}");
        }
        assert_eq!(q, before, "nothing changed");
        // The boot context, ready and not running, is still slot 0: it never moves.
        assert_eq!(q.frame(0), None);
    }

    /// A move keeps the lag, at 0 and above, and the arrival is never below the destination's
    /// floor: no credit, no loss (ADR-0020 S6).
    #[test]
    fn a_move_keeps_the_lag_and_arrives_at_the_destination_s_floor() {
        // Lag 0: slot 1 is level with the boot context.
        let (mut source, frame) = one_ready(7);
        assert_eq!(source.pass[1], source.pass[0]);
        assert_eq!(source.frame(1), Some(frame));
        let departed = source.depart(1).unwrap();
        assert_eq!((departed.lag(), departed.record()), (0, Some(7)));
        assert_eq!(source.live(), 1);
        assert_eq!((source.record(1), source.held(1)), (None, None));
        let mut destination = RunQueue::new();
        for _ in 0..40 {
            assert_eq!(destination.tick(0x2000), 0x2000);
        }
        let floor = destination.floor();
        assert!(floor > source.pass[0], "the destination's floor is higher");
        let slot = destination.arrive(departed).unwrap();
        assert_eq!(destination.pass[slot], floor, "at the floor, with lag 0");
        assert_eq!(destination.record(slot), Some(7));
        assert_eq!(destination.generation(slot), 1);
        assert_eq!(destination.saved[slot], frame);

        // Lag above 0: the boot context's stride is tiny, so after one quantum each, slot 1's
        // pass lies above it by the difference of the two strides.
        let mut source = RunQueue::new();
        source.set_weight(0, MAX_WEIGHT);
        source
            .push_weighted(0xA000, DEFAULT_WEIGHT, Some(8))
            .unwrap();
        assert_eq!(source.tick(0x1000), 0xA000);
        assert_eq!(source.tick(0xA100), 0x1000);
        assert_eq!(source.floor(), source.pass[0]);
        let lag = STRIDE_ONE / u64::from(DEFAULT_WEIGHT) - STRIDE_ONE / u64::from(MAX_WEIGHT);
        assert_eq!(source.pass[1] - source.floor(), lag);
        let departed = source.depart(1).unwrap();
        assert_eq!(departed.lag(), lag);
        let mut destination = RunQueue::new();
        destination.push(0xC000).unwrap();
        let floor = destination.floor();
        let slot = destination.arrive(departed).unwrap();
        assert_eq!(destination.pass[slot], floor + lag);
        assert!(destination.pass[slot] >= destination.floor());
    }

    /// The case of TASKS item 129, in small (ADR-0020 S6, the note of 2026-11-22). On a queue
    /// whose slot 0 is the idle flow, T runs alone for 1000 quanta and blocks, and nothing that
    /// counts is ready: the floor holds T's pass, never falling to the idle flow's, which was
    /// charged one quantum. A newcomer joins at it, T wakes beside it, and T's move carries no
    /// lag: at its destination the boot flow runs at most one quantum before it. Under the old
    /// reading the newcomer joined at the idle flow's pass, T left with 999 strides of lag, and
    /// the boot flow ran 999 quanta first (TASKS item 96's control C96-7).
    #[test]
    fn the_floor_holds_under_a_blocked_thread_so_a_newcomer_leaves_it_no_lag() {
        let stride = STRIDE_ONE / u64::from(DEFAULT_WEIGHT);
        let mut ap: RunQueue<u32> = RunQueue::empty();
        ap.set_idle(true);
        let t = ap.push_weighted(0x1000, DEFAULT_WEIGHT, Some(1)).unwrap();
        assert_eq!(ap.tick(0x8000), 0x1000, "the tick runs T");
        for _ in 0..1000 {
            assert_eq!(ap.yield_now(0x1000), 0x1000, "T alone keeps the processor");
        }
        let passed = ap.pass[t];
        assert_eq!(passed, 1000 * stride);
        assert_eq!(ap.pass[0], stride, "the idle flow was charged one quantum");
        assert!(ap.block(0x1000).is_ok());
        assert_eq!(ap.floor(), passed, "the floor holds T's pass");
        let u = ap.push_weighted(0x2000, DEFAULT_WEIGHT, Some(2)).unwrap();
        assert_eq!(ap.pass[u], passed, "the newcomer joins at the floor");
        assert!(ap.wake(t, ap.generation(t)));
        let departed = ap.depart(t).unwrap();
        let lag = departed.lag();
        assert!(lag <= stride, "lag {lag}");
        let mut boot: RunQueue<u32> = RunQueue::empty();
        for _ in 0..10 {
            let _ = boot.yield_now(0x9000);
        }
        let floor = boot.floor();
        let slot = boot.arrive(departed).unwrap();
        assert_eq!(boot.pass[slot], floor + lag);
        let mut yields = 0;
        while boot.current() == 0 && yields < 10_000 {
            let _ = boot.yield_now(0x9000);
            yields += 1;
        }
        assert!(yields <= 1, "the boot flow ran {yields} quanta first");
    }

    /// The control of the test above: with no newcomer, T is its queue's floor when it wakes,
    /// and its move carries no lag, under either reading.
    #[test]
    fn without_a_newcomer_a_woken_thread_s_move_carries_no_lag() {
        let mut ap: RunQueue<u32> = RunQueue::empty();
        ap.set_idle(true);
        let t = ap.push_weighted(0x1000, DEFAULT_WEIGHT, Some(1)).unwrap();
        assert_eq!(ap.tick(0x8000), 0x1000);
        for _ in 0..1000 {
            let _ = ap.yield_now(0x1000);
        }
        assert!(ap.block(0x1000).is_ok());
        assert!(ap.wake(t, ap.generation(t)));
        assert_eq!(ap.depart(t).unwrap().lag(), 0, "alone, T is the floor");
    }

    /// A pending wake-up travels with its thread and is consumed by its next block there (N1);
    /// the source keeps none.
    #[test]
    fn a_pending_wake_up_is_carried_and_consumed_by_the_next_block() {
        let (mut source, frame) = one_ready(3);
        assert!(!source.wake(1, source.generation(1)), "ready: kept");
        assert!(source.wake_pending(1));
        let departed = source.depart(1).unwrap();
        assert!(departed.wake_pending());
        assert!(!source.wake_pending(1), "the source keeps none");
        let mut destination = RunQueue::new();
        let slot = destination.arrive(departed).unwrap();
        assert!(destination.wake_pending(slot));
        assert_eq!(destination.tick(0x2000), frame, "it runs there");
        assert_eq!(
            destination.block(frame - 0x100),
            Ok(frame - 0x100),
            "consumed: no block"
        );
        assert!(!destination.wake_pending(slot));
        assert_eq!(
            destination.block(frame - 0x200),
            Ok(0x2000),
            "the next block blocks"
        );
    }

    /// A departed slot is no longer woken, so the source's `preempt_for_wake` never names it; an
    /// arrival is woken, so the destination's does (the coordinating session's ruling Q11).
    #[test]
    fn a_departure_clears_its_woken_mark_and_an_arrival_sets_one() {
        let mut source = RunQueue::new();
        let slot = source.push(0xA000).unwrap();
        assert_eq!(source.tick(0x1000), 0xA000);
        assert_eq!(source.block(0xA100), Ok(0x1000));
        assert!(source.wake(slot, source.generation(slot)), "woken");
        assert!(source.woken[slot]);
        let departed = source.depart(slot).unwrap();
        assert!(!source.woken[slot]);
        assert_eq!(
            source.preempt_for_wake(0x1100),
            0x1100,
            "nothing to run here"
        );
        let mut destination = RunQueue::new();
        destination.set_idle(true);
        let arrived = destination.arrive(departed).unwrap();
        assert!(destination.woken[arrived]);
        assert_eq!(
            destination.preempt_for_wake(0x2000),
            0xA100,
            "an idle destination runs the arrival at the IPI"
        );
    }

    /// `arrive` is refused by a full queue, which `has_room` says beforehand.
    #[test]
    fn a_full_queue_has_no_room_and_refuses_an_arrival() {
        let (mut source, _) = one_ready(1);
        let mut destination = RunQueue::new();
        for i in 1..CAPACITY as u64 {
            assert!(destination.has_room());
            destination.push(i * 0x1000).unwrap();
        }
        assert!(!destination.has_room());
        let departed = source.depart(1).unwrap();
        assert_eq!(destination.arrive(departed), Err(QueueError::Full));
    }

    /// `Departed`'s `Debug` prints its name alone, never its frame (ADR-0004 Amendment 2, A6).
    #[test]
    fn a_departed_thread_s_debug_prints_no_frame() {
        let (mut source, frame) = one_ready(1);
        let departed = source.depart(1).unwrap();
        let text = alloc::format!("{departed:?}");
        assert_eq!(text, "Departed");
        assert!(!text.contains(alloc::format!("{frame:x}").as_str()));
        let mut destination = RunQueue::new();
        assert!(destination.arrive(departed).is_ok());
    }

    /// ADR-0020 S7 (TASKS item 98): a thread is one balancing may take only by its spawner's
    /// declaration; a move carries it; the slot's next thread starts without it.
    #[test]
    fn a_balanced_declaration_is_carried_by_a_move_and_not_left_to_the_slot_s_next_thread() {
        let (mut source, _) = one_ready(1);
        assert!(!source.any_balanced());
        source.set_balanced(0);
        source.set_balanced(7);
        assert!(!source.any_balanced(), "slot 0 and a free slot take none");
        source.set_balanced(1);
        assert!(source.is_balanced(1) && source.any_balanced());
        let departed = source.depart(1).unwrap();
        assert!(departed.balanced());
        assert!(!source.any_balanced(), "it left with its thread");
        // The slot's next thread, pushed, is not balanced.
        assert_eq!(source.push(0x5000), Ok(1));
        assert!(!source.is_balanced(1));
        let mut destination = RunQueue::new();
        assert_eq!(destination.push(0x6000), Ok(1));
        let slot = destination.arrive(departed).unwrap();
        assert!(destination.is_balanced(slot) && !destination.is_balanced(1));
        // An exit ends it with the thread.
        let mut q = RunQueue::new();
        let slot = q.push(0x7000).unwrap();
        q.set_balanced(slot);
        assert_eq!(q.tick(0x9000), 0x7000);
        assert!(q.exit(0x7100).is_ok());
        assert!(!q.any_balanced());
    }

    /// A census counts the contexts that are running or ready, the idle flow aside and the boot
    /// flow among them, and names the balanced threads that are ready and not running.
    #[test]
    fn a_census_counts_what_is_runnable_and_which_balanced_threads_wait() {
        let mut q = RunQueue::new();
        assert_eq!(
            q.census(),
            Census {
                runnable: 1,
                weight: DEFAULT_WEIGHT,
                waiting: 0
            },
            "the boot flow counts"
        );
        assert!(q.busy());
        q.set_idle(true);
        assert_eq!(q.census(), Census::default(), "the idle flow does not");
        assert!(!q.busy());
        let a = q.push_weighted(0x1000, 4, None).unwrap();
        let b = q.push_weighted(0x2000, 16, None).unwrap();
        let c = q.push_weighted(0x3000, 1, None).unwrap();
        q.set_balanced(a);
        q.set_balanced(b);
        assert_eq!(
            q.census(),
            Census {
                runnable: 3,
                weight: 21,
                waiting: bit(a) | bit(b)
            }
        );
        // The running one does not wait; a blocked one is not runnable.
        assert_eq!(q.tick(0x9000), 0x1000);
        assert!(q.busy());
        assert_eq!(q.census().waiting, bit(b));
        assert!(q.block(0x1100).is_ok());
        assert_eq!(q.current(), b);
        assert_eq!(
            q.census(),
            Census {
                runnable: 2,
                weight: 17,
                waiting: 0
            },
            "{c} is ready and not balanced"
        );
    }

    /// ADR-0020 S10's move rate: a thread that arrived by a move is settled for a level only once
    /// more than that level's period of this queue's ticks has passed; one pushed here always is.
    #[test]
    fn a_thread_is_settled_only_after_more_than_its_period_of_ticks() {
        let (mut source, _) = one_ready(1);
        assert_eq!(source.since_arrival(1), None);
        assert!(source.settled(1, 16), "it never moved");
        let mut q = RunQueue::new();
        q.set_idle(true);
        for _ in 0..5 {
            q.tick(0x9000);
        }
        let slot = q.arrive(source.depart(1).unwrap()).unwrap();
        assert_eq!(q.since_arrival(slot), Some(0));
        for ticks in 1..=5u64 {
            // The thread runs alone: each tick resumes it.
            q.tick(0x8000 + 8 * ticks);
            assert_eq!(q.since_arrival(slot), Some(ticks));
            assert_eq!(q.settled(slot, 1), ticks > 1, "after {ticks} ticks");
            assert_eq!(q.settled(slot, 4), ticks > 4, "after {ticks} ticks");
        }
        assert!(!q.settled(slot, 16));
    }

    /// ADR-0020 S10's cache-hot window, as built (TASKS item 98): a waiting thread is judged not
    /// cache-hot when it has stayed off the processor since the processor's mark and the mark is
    /// the window or more behind, on the processor's own TSC. A thread pushed here has run
    /// nowhere; one that ran since the mark, by a tick, a yield, a block or a preemption, is not
    /// named until it has waited through a mark; an arrival neither.
    #[test]
    fn a_thread_that_has_waited_since_the_mark_is_judged_cold() {
        const H: u64 = 1000;
        let mut q = RunQueue::new();
        q.set_idle(true);
        let a = q.push(0x1000).unwrap();
        let b = q.push(0x2000).unwrap();
        q.set_balanced(a);
        q.set_balanced(b);
        // As the tick's upcall does: the judgement before the step, the step, then the mark.
        let tick = |q: &mut RunQueue, now: u64, here: u64| {
            q.judge(now, H);
            let next = q.tick(here);
            q.mark(now);
            next
        };
        // Pushed threads have run nowhere: cold once the mark, here the boot's, is a window old.
        q.judge(999, H);
        assert_eq!(q.cold(), 0);
        q.judge(1000, H);
        assert_eq!(q.cold(), bit(a) | bit(b));
        // The first tick runs `a`: `b` still waits, and stays cold across the mark.
        assert_eq!(tick(&mut q, 10_000, 0x9000), 0x1000);
        assert_eq!(q.cold(), bit(b));
        // The next tick switches `a` out and runs `b`: `a` ran until now.
        assert_eq!(tick(&mut q, 20_000, 0x1100), 0x2000);
        assert_eq!(q.cold(), 0, "the only waiting thread has just run");
        q.judge(20_999, H);
        assert_eq!(q.cold(), 0, "within the window after the mark");
        q.judge(21_000, H);
        assert_eq!(q.cold(), bit(a), "off the processor for the window");
        // A yield runs `a`: it is no longer waiting. `b` yielded since the mark: it ran, and its
        // pass shows it.
        assert_eq!(q.yield_now(0x2100), 0x1100);
        assert_eq!(q.cold(), 0);
        q.judge(30_000, H);
        assert_eq!(q.cold(), 0, "`b` ran since the mark");
        // A thread that blocks and is woken inside one mark ran too, though nothing charged it.
        let (running, waiting) = (q.current(), if q.current() == a { b } else { a });
        assert!(q.block(0x1200).is_ok());
        assert_eq!(q.current(), waiting);
        assert!(q.wake(running, q.generation(running)));
        q.judge(39_000, H);
        assert_eq!(q.cold(), 0);
        // After a mark at which it waits, and a window, it is cold.
        tick(&mut q, 40_000, 0x2200);
        let waits = if q.current() == a { b } else { a };
        q.judge(41_000, H);
        assert_eq!(q.cold(), bit(waits));
        // A thread that is not balanced is never named, and an arrival not until it has waited
        // through a mark here: it may have run a moment ago on the processor it left.
        let c = q.push(0x3000).unwrap();
        let (mut other, _) = one_ready(7);
        other.set_balanced(1);
        let d = q.arrive(other.depart(1).unwrap()).unwrap();
        q.judge(50_000, H);
        assert_eq!(q.cold() & (bit(c) | bit(d)), 0);
        tick(&mut q, 60_000, 0x2300);
        q.judge(61_000, H);
        if q.current() != d {
            assert_ne!(
                q.cold() & bit(d),
                0,
                "the arrival, once it has waited through a mark"
            );
        }
        assert_eq!(q.cold() & bit(c), 0);
        // A TSC that reads behind the mark names nothing new.
        let before = q.cold();
        q.judge(59_000, H);
        assert_eq!(q.cold(), before);
    }

    /// The count of balancing interrupts a processor sent is kept against its queue's own ticks,
    /// apart from the framework's rate.
    #[test]
    fn a_queue_counts_the_balancing_interrupts_its_processor_sent_by_its_own_ticks() {
        let mut q = RunQueue::new();
        assert_eq!(q.sent(), (0, 0));
        q.note_sent();
        q.tick(0x9000);
        q.note_sent();
        assert_eq!(q.sent(), (2, 1));
        q.note_sent();
        assert_eq!(q.sent(), (3, 2), "two in one tick show");
        q.tick(0x9008);
        q.note_sent();
        assert_eq!(q.sent(), (4, 2));
    }

    /// ADR-0020 Amendment 1: a processor takes at most one ask in each of its ticks, however many
    /// ask; the next tick allows one again; and the count is kept apart from the mark.
    #[test]
    fn a_queue_takes_one_ask_a_tick() {
        let mut q = RunQueue::new();
        assert_eq!(q.asks(), (0, 0));
        assert!(q.take_ask());
        q.note_ask();
        assert!(
            !q.take_ask() && !q.take_ask(),
            "a second asker in the same tick"
        );
        assert_eq!(q.asks(), (1, 1));
        q.tick(0x9000);
        assert!(q.take_ask(), "the first after a tick");
        q.note_ask();
        assert!(!q.take_ask());
        q.tick(0x9008);
        q.tick(0x9010);
        assert_eq!(q.asks(), (2, 1));
        // Were the mark ignored, the count would say so.
        q.note_ask();
        q.note_ask();
        assert_eq!(q.asks(), (4, 2));
        q.tick(0x9018);
        q.note_ask();
        assert_eq!(q.asks(), (5, 2));
    }

    /// The balancing state against a brute-force model (TASKS item 98), over random sequences on
    /// two to three queues: which threads are balanced, wherever they moved; each one's ticks
    /// since it arrived; the census; the cache-hot rule's one promise, that no thread named cold
    /// has run within the window; and one ask a tick for each queue. The model keeps each thread's true last moment on a
    /// processor, on one clock for the whole machine, which the queues never see.
    mod balancing_against_a_model {
        use super::super::*;

        const H: u64 = 1000;

        /// xorshift64*, seeded.
        struct Rng(u64);

        impl Rng {
            fn next(&mut self) -> u64 {
                self.0 ^= self.0 >> 12;
                self.0 ^= self.0 << 25;
                self.0 ^= self.0 >> 27;
                self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
            }

            fn below(&mut self, n: usize) -> usize {
                usize::try_from(self.next() % u64::try_from(n).unwrap()).unwrap()
            }
        }

        /// What the model keeps of each thread, by its number.
        #[derive(Clone, Copy, Default)]
        struct Thread {
            balanced: bool,
            /// Its queue's ticks when it arrived by a move.
            arrived: Option<u64>,
            /// The last moment it was running.
            ran_until: Option<u64>,
        }

        /// The thread each queue runs, if it runs one.
        fn running(queues: &[RunQueue]) -> alloc::vec::Vec<Option<u32>> {
            queues.iter().map(|q| q.record(q.current())).collect()
        }

        /// One seed's run; returns how many times a thread was named cold, and the asks sent.
        fn run(seed: u64, steps: usize) -> (u64, u64) {
            let mut rng = Rng(seed | 1);
            let n = 2 + rng.below(2);
            let mut queues: alloc::vec::Vec<RunQueue> = (0..n)
                .map(|_| {
                    let mut q = RunQueue::new();
                    q.set_idle(rng.below(2) == 0);
                    q
                })
                .collect();
            let mut threads: alloc::vec::Vec<Thread> = alloc::vec![Thread::default()];
            let (mut now, mut named_cold) = (0u64, 0u64);
            // Each processor's TSC is its own: the machine's clock plus an offset no queue is
            // told. A judgement that compared two processors' readings would be off by it.
            let skew: alloc::vec::Vec<u64> = (0..n).map(|_| rng.next() % 1_000_000_000).collect();
            // The tick each queue was last asked in, and the asks sent in all.
            let mut asked: alloc::vec::Vec<Option<u64>> = alloc::vec![None; n];
            let mut asks = 0u64;
            for step in 0..steps {
                let at = alloc::format!("seed {seed:#x}, step {step}");
                now += 1 + rng.next() % 600;
                // Whatever ran since the last step ran until now.
                for id in running(&queues).into_iter().flatten() {
                    threads[id as usize].ran_until = Some(now);
                }
                let c = rng.below(n);
                let here = 0x10_0000 + u64::try_from(step).unwrap() * 0x40;
                let own = now + skew[c];
                let q = &mut queues[c];
                match rng.below(15) {
                    0 | 1 => {
                        let id = u32::try_from(threads.len()).unwrap();
                        let frame = 0x8000_0000 + u64::from(id) * 0x1000;
                        if let Ok(slot) = q.push_weighted(frame, DEFAULT_WEIGHT, Some(id)) {
                            let balanced = rng.below(3) != 0;
                            if balanced {
                                q.set_balanced(slot);
                            }
                            threads.push(Thread {
                                balanced,
                                ..Thread::default()
                            });
                        }
                    }
                    2 | 3 => {
                        // The kernel's tick on a queue with a balanced thread: the judgement,
                        // the step, the mark. With none it only steps, and the mark grows old.
                        if q.any_balanced() {
                            q.judge(own, H);
                            q.tick(here);
                            q.mark(own);
                        } else {
                            q.tick(here);
                        }
                    }
                    4 => {
                        q.yield_now(here);
                    }
                    5 => {
                        let _ = q.block(here);
                    }
                    // A sleeper and a timed block: each is made ready again by a later tick's
                    // step (the review of TASKS item 98, D98-9).
                    11 => {
                        let _ = q.sleep(here, 1 + rng.next() % 3);
                    }
                    12 => {
                        let _ = q.block_for(here, Some(1 + rng.next() % 3));
                    }
                    6 => {
                        let slot = 1 + rng.below(CAPACITY - 1);
                        let generation = q.generation(slot);
                        q.wake(slot, generation);
                    }
                    7 => {
                        q.preempt_for_wake(here);
                        q.judge(own, H);
                    }
                    8 => {
                        let _ = q.exit(here);
                    }
                    9 => q.judge(own, H),
                    10 => {
                        // An asker: the first in each of this queue's ticks sends, no other.
                        let first = asked[c] != Some(q.ticks());
                        assert_eq!(q.take_ask(), first, "{at}: queue {c}'s ask");
                        if first {
                            asked[c] = Some(q.ticks());
                            q.note_ask();
                            asks += 1;
                        }
                        assert!(q.asks().1 <= 1, "{at}: queue {c} took two asks in a tick");
                    }
                    _ => {
                        let to = rng.below(n);
                        let slot = 1 + rng.below(CAPACITY - 1);
                        if to != c && queues[to].has_room() && queues[c].frame(slot).is_some() {
                            let id = queues[c].record(slot).unwrap();
                            let departed = queues[c].depart(slot).unwrap();
                            assert_eq!(departed.balanced(), threads[id as usize].balanced, "{at}");
                            queues[to].arrive(departed).unwrap();
                            threads[id as usize].arrived = Some(queues[to].ticks());
                        }
                    }
                }
                // Whatever runs now is running.
                for id in running(&queues).into_iter().flatten() {
                    threads[id as usize].ran_until = Some(now);
                }
                for (k, q) in queues.iter().enumerate() {
                    let mut expected = Census::default();
                    for slot in 0..CAPACITY {
                        let ready = q.describe(slot).state == "ready";
                        let idle_flow = slot == 0 && q.describe_queue().2;
                        if ready && !idle_flow {
                            expected.runnable += 1;
                            expected.weight += q.weight(slot);
                        }
                        let Some(id) = q.record(slot) else {
                            assert!(!q.is_balanced(slot), "{at}: queue {k} slot {slot}");
                            assert_eq!(q.since_arrival(slot), None, "{at}: queue {k} slot {slot}");
                            continue;
                        };
                        let thread = threads[id as usize];
                        assert_eq!(q.is_balanced(slot), thread.balanced, "{at}: thread {id}");
                        let since = thread.arrived.map(|arrived| q.ticks() - arrived);
                        assert_eq!(q.since_arrival(slot), since, "{at}: thread {id}");
                        for period in [1, 4, 16] {
                            assert_eq!(
                                q.settled(slot, period),
                                since.is_none_or(|ticks| ticks > period),
                                "{at}: thread {id}, period {period}"
                            );
                        }
                        if thread.balanced && ready && slot != q.current() {
                            expected.waiting |= bit(slot);
                        }
                    }
                    assert_eq!(q.census(), expected, "{at}: queue {k}'s census");
                    let cold = q.cold();
                    assert_eq!(cold & !expected.waiting, 0, "{at}: cold, and not waiting");
                    for slot in (0..CAPACITY).filter(|&slot| cold & bit(slot) != 0) {
                        let id = q.record(slot).unwrap();
                        named_cold += 1;
                        // Never ran, or ran last a window or more ago: the rule's one promise.
                        if let Some(ran_until) = threads[id as usize].ran_until {
                            assert!(
                                now - ran_until >= H,
                                "{at}: thread {id} was named cold {} after it ran",
                                now - ran_until
                            );
                        }
                    }
                }
            }
            let counted: u64 = queues.iter().map(|q| q.asks().0).sum();
            assert_eq!(counted, asks, "seed {seed:#x}: the asks counted");
            (named_cold, asks)
        }

        /// Fixed seeds, printed in every failure.
        #[test]
        fn the_balancing_state_agrees_with_the_model_over_random_sequences() {
            let (mut named_cold, mut asks) = (0, 0);
            for seed in 0..64u64 {
                let (cold, asked) = run(0xd1b5_4a32_d192_ed03u64.wrapping_mul(seed + 1), 3000);
                named_cold += cold;
                asks += asked;
            }
            assert!(
                named_cold > 1000,
                "the sequences named a thread cold {named_cold} times"
            );
            assert!(asks > 1000, "the sequences sent {asks} asks");
        }
    }

    /// The randomized test of moves against a brute-force model (TASKS item 96, the contract's
    /// §1.5): what a run queue should hold, written out slot by slot, with its floor, its lag and
    /// its next choice computed by brute force rather than by the queue's own code.
    mod against_a_model {
        use super::super::*;

        /// A model slot's state, as `State`.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        enum Is {
            Free,
            Ready,
            Sleeping(u64),
            Blocked(Option<u64>),
        }

        /// One slot of the model.
        #[derive(Clone, Copy, Debug)]
        struct Slot {
            is: Is,
            frame: u64,
            pass: u64,
            weight: u32,
            generation: u32,
            pending: bool,
            woken: bool,
            record: Option<u32>,
        }

        /// What departs, in the model: frame, weight, pending wake-up, lag and record.
        type Leaving = (u64, u32, bool, u64, Option<u32>);

        /// One queue of the model.
        struct Model {
            slots: [Slot; CAPACITY],
            current: usize,
            idle: bool,
            ticks: u64,
            /// The floor at the end of the last step ([`Model::settle`]): the value it holds while
            /// nothing that counts toward it is ready. Written after every step by brute force,
            /// not at the moments the queue's own code keeps it.
            last: u64,
        }

        impl Model {
            fn new(idle: bool) -> Self {
                let free = Slot {
                    is: Is::Free,
                    frame: 0,
                    pass: 0,
                    weight: DEFAULT_WEIGHT,
                    generation: 0,
                    pending: false,
                    woken: false,
                    record: None,
                };
                let mut slots = [free; CAPACITY];
                slots[0].is = Is::Ready;
                Self {
                    slots,
                    current: 0,
                    idle,
                    ticks: 0,
                    last: 0,
                }
            }

            /// The end of a step: the floor's value now is the one it holds next.
            fn settle(&mut self) {
                self.last = self.floor();
            }

            /// Whether slot `i` counts toward the floor: ready, and not the idle context.
            fn counts(&self, i: usize) -> bool {
                self.slots[i].is == Is::Ready && !(self.idle && i == 0)
            }

            /// The lowest pass among the ready contexts, the idle one aside, else the floor at
            /// the end of the last step (ADR-0020 S6, the note of 2026-11-22): every slot looked
            /// at.
            fn floor(&self) -> u64 {
                (0..CAPACITY)
                    .filter(|&i| self.counts(i))
                    .map(|i| self.slots[i].pass)
                    .min()
                    .unwrap_or(self.last)
            }

            /// Of the slots `eligible` admits, the lowest pass, and of those the first after the
            /// current one, going round: the least distance forward from it, counted for each.
            fn choose(&self, eligible: impl Fn(usize) -> bool) -> Option<usize> {
                let low = (0..CAPACITY)
                    .filter(|&i| eligible(i))
                    .map(|i| self.slots[i].pass)
                    .min()?;
                (0..CAPACITY)
                    .filter(|&i| eligible(i) && self.slots[i].pass == low)
                    .min_by_key(|&i| (i + CAPACITY - self.current - 1) % CAPACITY)
            }

            fn switch(&mut self, interrupted: u64, charge: bool) -> u64 {
                let cur = self.current;
                self.slots[cur].frame = interrupted;
                if charge {
                    let stride = STRIDE_ONE / u64::from(self.slots[cur].weight);
                    self.slots[cur].pass = self.slots[cur].pass.saturating_add(stride);
                }
                let (floor, now) = (self.floor(), self.ticks);
                for slot in &mut self.slots {
                    let due = match slot.is {
                        Is::Sleeping(until) | Is::Blocked(Some(until)) => until <= now,
                        _ => false,
                    };
                    if due {
                        slot.is = Is::Ready;
                        slot.pass = slot.pass.max(floor);
                    }
                }
                let others = (1..CAPACITY).any(|i| self.slots[i].is == Is::Ready);
                let next = self
                    .choose(|i| self.slots[i].is == Is::Ready && !(self.idle && i == 0 && others))
                    .unwrap_or(0);
                for slot in &mut self.slots {
                    slot.woken = false;
                }
                self.current = next;
                self.slots[next].frame
            }

            fn push(&mut self, frame: u64, record: u32) -> Option<usize> {
                let slot = (1..CAPACITY).find(|&i| self.slots[i].is == Is::Free)?;
                let floor = self.floor();
                let s = &mut self.slots[slot];
                *s = Slot {
                    is: Is::Ready,
                    frame,
                    pass: floor,
                    weight: DEFAULT_WEIGHT,
                    generation: s.generation.wrapping_add(1),
                    pending: false,
                    woken: s.woken,
                    record: Some(record),
                };
                Some(slot)
            }

            fn wake(&mut self, slot: usize, generation: u32) -> bool {
                if slot == 0 || self.slots[slot].generation != generation {
                    return false;
                }
                match self.slots[slot].is {
                    Is::Blocked(_) => {
                        let floor = self.floor();
                        let s = &mut self.slots[slot];
                        s.is = Is::Ready;
                        s.pass = s.pass.max(floor);
                        s.woken = true;
                        true
                    }
                    Is::Ready | Is::Sleeping(_) => {
                        self.slots[slot].pending = true;
                        false
                    }
                    Is::Free => false,
                }
            }

            fn preempt(&mut self, interrupted: u64) -> u64 {
                let woken: [bool; CAPACITY] = core::array::from_fn(|i| self.slots[i].woken);
                for slot in &mut self.slots {
                    slot.woken = false;
                }
                let running = self.slots[self.current].pass;
                let idle = self.idle && self.current == 0;
                let next = self
                    .choose(|i| woken[i] && self.slots[i].is == Is::Ready)
                    .filter(|&i| idle || self.slots[i].pass <= running);
                let Some(next) = next else {
                    return interrupted;
                };
                self.slots[self.current].frame = interrupted;
                self.current = next;
                self.slots[next].frame
            }

            /// What departs from `slot`: (frame, weight, pending, lag, record), if it may.
            fn depart(&mut self, slot: usize) -> Option<Leaving> {
                if slot == 0 || slot == self.current || self.slots[slot].is != Is::Ready {
                    return None;
                }
                let lag = self.slots[slot].pass - self.floor();
                let s = &mut self.slots[slot];
                let out = (s.frame, s.weight, s.pending, lag, s.record);
                s.is = Is::Free;
                s.record = None;
                s.pending = false;
                s.woken = false;
                Some(out)
            }

            fn arrive(&mut self, (frame, weight, pending, lag, record): Leaving) -> Option<usize> {
                let slot = (1..CAPACITY).find(|&i| self.slots[i].is == Is::Free)?;
                let floor = self.floor();
                let s = &mut self.slots[slot];
                *s = Slot {
                    is: Is::Ready,
                    frame,
                    pass: floor + lag,
                    weight,
                    generation: s.generation.wrapping_add(1),
                    pending,
                    woken: true,
                    record,
                };
                Some(slot)
            }
        }

        /// The queue's state, slot by slot, as the model's.
        fn same(q: &RunQueue, m: &Model, at: &str) {
            assert_eq!(q.current, m.current, "{at}: the running slot");
            assert_eq!(q.floor(), m.floor(), "{at}: the floor");
            // Slot 0, the boot or idle flow's, never holds a record (`RunQueue::held`).
            assert_eq!(q.record[0], None, "{at}: slot 0 holds a record");
            assert_eq!(q.ticks, m.ticks, "{at}: ticks");
            for (i, s) in m.slots.iter().enumerate() {
                let is = match q.state[i] {
                    State::Free => Is::Free,
                    State::Ready => Is::Ready,
                    State::Sleeping(until) => Is::Sleeping(until),
                    State::Blocked(until) => Is::Blocked(until),
                };
                assert_eq!(is, s.is, "{at}: slot {i}'s state");
                if s.is == Is::Free {
                    assert_eq!(q.record(i), None, "{at}: free slot {i}'s record");
                    // Read without the state too, as a wake-up's record check reads it (TASKS
                    // item 96): every path that frees a slot clears its record.
                    assert_eq!(q.held(i), None, "{at}: free slot {i} kept a record");
                    continue;
                }
                assert_eq!(
                    q.held(i),
                    s.record,
                    "{at}: slot {i}'s record, read without its state"
                );
                assert_eq!(q.pass[i], s.pass, "{at}: slot {i}'s pass");
                assert_eq!(q.weight[i], s.weight, "{at}: slot {i}'s weight");
                assert_eq!(
                    q.wake_pending[i], s.pending,
                    "{at}: slot {i}'s pending wake-up"
                );
                assert_eq!(q.woken[i], s.woken, "{at}: slot {i}'s woken mark");
                assert_eq!(q.record(i), s.record, "{at}: slot {i}'s record");
                assert_eq!(q.generation[i], s.generation, "{at}: slot {i}'s generation");
                if i != q.current {
                    assert_eq!(q.saved[i], s.frame, "{at}: slot {i}'s saved frame");
                }
            }
        }

        /// xorshift64*, seeded: the same sequence for the same seed on every host.
        struct Rng(u64);

        impl Rng {
            fn next(&mut self) -> u64 {
                self.0 ^= self.0 >> 12;
                self.0 ^= self.0 << 25;
                self.0 ^= self.0 >> 27;
                self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
            }

            fn below(&mut self, n: usize) -> usize {
                usize::try_from(self.next() % u64::try_from(n).unwrap()).unwrap()
            }
        }

        /// One seed's run: two to four queues, up to 15 threads in all, `steps` random operations
        /// (push, tick, yield, sleep, block, block with a timeout, wake, a stale wake, exit, the
        /// IPI's `preempt_for_wake`, and a move by `depart` and `arrive`), and after each the
        /// queues equal to the model, and every choice the model's. Two properties too (ADR-0020
        /// S6, the note of 2026-11-22; TASKS item 129): no queue's floor ever falls, and every
        /// thread that departs carries a lag of at most one of its own strides, all weights here
        /// being equal.
        fn run(seed: u64, steps: usize) -> u64 {
            let mut rng = Rng(seed | 1);
            let n = 2 + rng.below(3);
            let mut queues: alloc::vec::Vec<RunQueue> = alloc::vec::Vec::new();
            let mut models: alloc::vec::Vec<Model> = alloc::vec::Vec::new();
            for _ in 0..n {
                let idle = rng.below(2) == 0;
                let mut q = RunQueue::new();
                q.set_idle(idle);
                queues.push(q);
                models.push(Model::new(idle));
            }
            let (mut threads, mut moves) = (0u32, 0u64);
            let mut floors = alloc::vec![0u64; n];
            for step in 0..steps {
                let at = alloc::format!("seed {seed:#x}, step {step}");
                let c = rng.below(n);
                // A frame for whatever is interrupted now: 8-aligned, never 0, never repeated.
                let here = 0x10_0000 + u64::try_from(step).unwrap() * 0x40;
                // Threads in all the queues, the boot or idle contexts aside: at most 15.
                let live: usize = models
                    .iter()
                    .map(|m| m.slots.iter().skip(1).filter(|s| s.is != Is::Free).count())
                    .sum();
                let (q, m) = (&mut queues[c], &mut models[c]);
                match rng.below(12) {
                    0 if live < 15 => {
                        threads += 1;
                        let frame = 0x8000_0000 + u64::from(threads) * 0x1000;
                        let pushed = q.push_weighted(frame, DEFAULT_WEIGHT, Some(threads)).ok();
                        assert_eq!(pushed, m.push(frame, threads), "{at}: push");
                    }
                    1 | 2 => {
                        m.ticks += 1;
                        assert_eq!(q.tick(here), m.switch(here, true), "{at}: tick");
                    }
                    3 => assert_eq!(q.yield_now(here), m.switch(here, true), "{at}: yield"),
                    4 if m.current != 0 => {
                        let t = 1 + u64::try_from(rng.below(3)).unwrap();
                        m.slots[m.current].is = Is::Sleeping(m.ticks + t);
                        assert_eq!(q.sleep(here, t), Ok(m.switch(here, false)), "{at}: sleep");
                    }
                    5 | 6 if m.current != 0 => {
                        let ticks =
                            (rng.below(3) == 0).then(|| 1 + u64::try_from(rng.below(3)).unwrap());
                        let expected = if m.slots[m.current].pending {
                            m.slots[m.current].pending = false;
                            here
                        } else {
                            m.slots[m.current].is = Is::Blocked(ticks.map(|t| m.ticks + t));
                            m.switch(here, false)
                        };
                        assert_eq!(q.block_for(here, ticks), Ok(expected), "{at}: block");
                    }
                    7 | 8 => {
                        // A wake-up of a thread wherever it is, by its slot and generation; one
                        // time in four by a stale generation.
                        let found = (0..n)
                            .flat_map(|k| (1..CAPACITY).map(move |i| (k, i)))
                            .filter(|&(k, i)| models[k].slots[i].is != Is::Free)
                            .collect::<alloc::vec::Vec<_>>();
                        if !found.is_empty() {
                            let (k, i) = found[rng.below(found.len())];
                            let stale = rng.below(4) == 0;
                            let generation =
                                models[k].slots[i].generation.wrapping_sub(u32::from(stale));
                            assert_eq!(
                                queues[k].wake(i, generation),
                                models[k].wake(i, generation),
                                "{at}: wake"
                            );
                        }
                    }
                    9 if m.current != 0 => {
                        m.slots[m.current].is = Is::Free;
                        m.slots[m.current].record = None;
                        assert_eq!(q.exit(here), Ok(m.switch(here, false)), "{at}: exit");
                    }
                    10 => assert_eq!(q.preempt_for_wake(here), m.preempt(here), "{at}: preempt"),
                    11 => {
                        let to = rng.below(n);
                        // Half the time any slot, so that every refusal is tried; half the time
                        // one the model says may move, if there is one.
                        let movable = (1..CAPACITY)
                            .filter(|&i| {
                                i != models[c].current && models[c].slots[i].is == Is::Ready
                            })
                            .collect::<alloc::vec::Vec<_>>();
                        let slot = if rng.below(2) == 0 || movable.is_empty() {
                            1 + rng.below(CAPACITY - 1)
                        } else {
                            movable[rng.below(movable.len())]
                        };
                        if to != c {
                            let departed = queues[c].depart(slot);
                            let modelled = models[c].depart(slot);
                            assert_eq!(departed.is_some(), modelled.is_some(), "{at}: depart");
                            if let (Some(d), Some(md)) = (departed, modelled) {
                                assert_eq!(
                                    (d.wake_pending, d.lag, d.record),
                                    (md.2, md.3, md.4),
                                    "{at}: what departs"
                                );
                                assert!(
                                    d.lag <= STRIDE_ONE / u64::from(d.weight),
                                    "{at}: a lag of {} over one stride",
                                    d.lag
                                );
                                if queues[to].has_room() {
                                    moves += 1;
                                    let arrived = queues[to].arrive(d).ok();
                                    assert_eq!(arrived, models[to].arrive(md), "{at}: arrive");
                                    let slot = arrived.unwrap();
                                    assert!(
                                        queues[to].pass[slot] >= queues[to].floor(),
                                        "{at}: below the floor"
                                    );
                                } else {
                                    // A mover asks first and would not have taken it: put it back.
                                    let back = queues[c].arrive(d).ok();
                                    assert_eq!(back, models[c].arrive(md), "{at}: back");
                                }
                            }
                        }
                    }
                    _ => {}
                }
                for (k, (q, m)) in queues.iter().zip(models.iter_mut()).enumerate() {
                    m.settle();
                    same(q, m, &at);
                    let floor = q.floor();
                    assert!(
                        floor >= floors[k],
                        "{at}: queue {k}'s floor fell from {} to {floor}",
                        floors[k]
                    );
                    floors[k] = floor;
                }
            }
            moves
        }

        /// Fixed seeds, printed in every failure: run one again by its seed.
        #[test]
        fn moves_agree_with_the_model_over_random_sequences() {
            let mut moves = 0;
            for seed in 0..64u64 {
                moves += run(0x9e37_79b9_7f4a_7c15u64.wrapping_mul(seed + 1), 3000);
            }
            assert!(moves > 1000, "the sequences made {moves} moves");
        }
    }
}
