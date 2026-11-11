//! What happens between steps: the loops over tries and draws, and the order of a request's
//! steps (ADR-0016 R4 to R8, R10; TASKS 18b S1 to S6).
//!
//! The caller, the Framekernel or the loader, implements [`Platform`]: it runs a step on the
//! processor the caller is on, with interrupts off, on a stack it overwrites afterwards. The
//! functions here decide which step comes next and how often an instruction is tried. They
//! see only what a step reports: counts and outcomes, never a draw, a seed or a state.
//!
//! A thread may move to another processor between two steps. Each step works on the
//! processor it runs on, so a request's steps may be spread over several processors'
//! generators, which is harmless (R7).

use crate::health::{Condition, STARTUP_DRAWS};
use crate::node::{
    Allow, Fresh, Instruction, Maint, Node, PIECE, Piece, Purpose, RESEED_DRAWS, Source, Try,
};
use crate::root::{Answer, Root};
use crate::seed::NONCE_DRAWS;
use crate::selftest;
use crate::wipe::wipe;

/// What a step is given.
pub struct Ctx<'a> {
    /// The generator of the processor the step runs on.
    pub node: &'a mut Node,
    /// That processor's instructions.
    pub source: &'a mut (dyn Source + 'a),
    /// That processor's seed area while it starts ([`crate::seed::AREA_LEN`] bytes), and
    /// nothing afterwards.
    pub area: &'a mut [u8],
    /// That processor's index.
    pub index: u32,
    /// The monotonic clock, in nanoseconds.
    pub now: u64,
    /// The time-stamp counter.
    pub tsc: u64,
}

impl core::fmt::Debug for Ctx<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Ctx { .. }")
    }
}

/// Why a step did not run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Busy {
    /// A step is already running on this processor: the caller interrupted one.
    Node,
    /// Another processor holds the root's lock.
    Root,
}

/// What the generator's caller provides.
pub trait Platform {
    /// Runs `step` once, as one step on the processor the caller is on now.
    ///
    /// # Errors
    ///
    /// [`Busy::Node`] if a step is already running there; `step` is then not run.
    fn step(&mut self, step: &mut dyn FnMut(&mut Ctx<'_>)) -> Result<(), Busy>;

    /// The same, holding the root's lock for the length of the step.
    ///
    /// # Errors
    ///
    /// [`Busy::Root`] if another processor holds the lock, and [`Busy::Node`] as for
    /// [`Self::step`]; `step` is then not run.
    fn root_step(&mut self, step: &mut dyn FnMut(&mut Ctx<'_>, &mut Root)) -> Result<(), Busy>;

    /// Waits a moment between two tries of an instruction, or for the root's lock: the
    /// processor's PAUSE (R4 item 1).
    fn pause(&mut self);

    /// Whether the caller came with interrupts off. It then never waits in a pause for an
    /// instruction: each draw gets one try (TASKS 18b S6).
    fn quick(&self) -> bool;
}

/// Why a request was not answered (R11). On either, the caller's buffer holds no output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// There is no generator: no instruction passed its start-up test on the boot processor, or
    /// the known-answer test failed (R8).
    NoGenerator,
    /// Not possible now: fresh entropy could not be had for a request with prediction
    /// resistance, the root has stopped, or a step is already running on this processor.
    NotNow,
}

/// How a root's start went.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RootStart {
    /// Whether the known-answer self-test passed.
    pub self_test: bool,
    /// Where RDSEED stands on the processor that started the root.
    pub rdseed: Condition,
    /// Where RDRAND stands there.
    pub rdrand: Condition,
    /// Whether there is a root.
    pub instantiated: bool,
}

impl core::fmt::Debug for RootStart {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("RootStart { .. }")
    }
}

/// How a processor's start went.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ProcessorStart {
    /// Where RDSEED stands on it.
    pub rdseed: Condition,
    /// Where RDRAND stands on it.
    pub rdrand: Condition,
    /// How many of its instructions gave 1,024 draws of their own to its first seed.
    pub own_regions: u8,
    /// Whether it has a generator.
    pub generator: bool,
}

impl core::fmt::Debug for ProcessorStart {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ProcessorStart { .. }")
    }
}

/// How often a processor that finds the root's lock taken pauses and tries again before it
/// gives up. The lock is held for one step.
const ROOT_WAITS: u32 = 1 << 16;
/// The most draws one request makes towards maintenance (TASKS 18b S2).
const REQUEST_DRAWS: u8 = 16;
/// The most steps one request's maintenance takes: every draw using all of RDSEED's tries,
/// and as many steps again for what follows a draw.
const MAINTAIN_STEPS: u32 = 2 * (REQUEST_DRAWS as u32) * 100 + 64;
/// How many draws that end "not now" a processor's start makes again before it gives the
/// instruction up for now. A start has nothing else to do, and the instructions are most
/// likely to be dry just then, when every processor has drawn its 1,024.
const START_PATIENCE: u32 = 8;
/// The most steps one piece with prediction resistance takes before it is refused.
const FRESH_STEPS: u32 = 4 * (RESEED_DRAWS as u32) * 100 + 64;

/// Runs `step` as one step and returns what it returned.
fn in_step<T>(
    platform: &mut dyn Platform,
    mut step: impl FnMut(&mut Ctx<'_>) -> T,
) -> Result<T, Error> {
    let mut result = None;
    platform
        .step(&mut |ctx| result = Some(step(ctx)))
        .map_err(|_| Error::NotNow)?;
    result.ok_or(Error::NotNow)
}

/// Runs `step` as one step under the root's lock, waiting for the lock between tries with
/// interrupts as the caller had them (TASKS 18b S4).
fn in_root_step<T>(
    platform: &mut dyn Platform,
    mut step: impl FnMut(&mut Ctx<'_>, &mut Root) -> T,
) -> Result<T, Error> {
    let mut waits = 0u32;
    loop {
        let mut result = None;
        match platform.root_step(&mut |ctx, root| result = Some(step(ctx, root))) {
            Ok(()) => return result.ok_or(Error::NotNow),
            Err(Busy::Root) if waits < ROOT_WAITS => {
                waits = waits.saturating_add(1);
                platform.pause();
            }
            Err(_) => return Err(Error::NotNow),
        }
    }
}

/// The tries of the draw that is being made.
struct Tries {
    /// One try for each draw (S6).
    quick: bool,
    /// The processor and instruction of the draw in hand, if one is.
    of: Option<(u32, Instruction)>,
    used: u32,
    set_aside: bool,
}

impl Tries {
    const fn new(quick: bool) -> Self {
        Self {
            quick,
            of: None,
            used: 0,
            set_aside: false,
        }
    }

    /// Notes one try's outcome. `Some` when the draw has used all its tries without a value:
    /// it holds whether any try gave zero or all ones.
    fn note(&mut self, on: u32, which: Instruction, outcome: Try) -> Option<bool> {
        match self.of {
            Some((before, same)) if same == which => {
                if before != on {
                    // The thread moved in the middle of a draw. The tries it has used still
                    // count, so that moving cannot buy more of them. What another processor's
                    // instruction returned says nothing about this one's.
                    self.of = Some((on, which));
                    self.set_aside = false;
                }
            }
            // A new draw: the count starts again.
            _ => {
                self.of = Some((on, which));
                self.used = 0;
                self.set_aside = false;
            }
        }
        match outcome {
            Try::Stored | Try::Completed | Try::Failed | Try::Unwanted => {
                self.of = None;
                None
            }
            Try::NotReady | Try::SetAside => {
                self.used = self.used.saturating_add(1);
                self.set_aside |= outcome == Try::SetAside;
                let budget = if self.quick { 1 } else { which.tries() };
                if self.used >= budget {
                    self.of = None;
                    Some(self.set_aside)
                } else {
                    None
                }
            }
        }
    }
}

/// How a collection into the seed area ended.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Collected {
    /// The region holds the draws wanted.
    Done,
    /// The instruction failed a test, or was not ready for a whole draw's tries. The region
    /// is empty.
    Without,
}

/// Collects `want` draws of `which` into `region` of the seed area: as a start-up test
/// (`testing`), or as further draws of an instruction that is healthy already.
fn collect(
    platform: &mut dyn Platform,
    which: Instruction,
    region: u8,
    want: u16,
    testing: bool,
) -> Result<Collected, Error> {
    if testing {
        in_step(platform, |ctx| ctx.node.begin_test(which))?;
    }
    let mut tries = Tries::new(false);
    let mut patience = START_PATIENCE;
    // Every draw may use all its tries, the start's patience adds its own, and one step more
    // ends it.
    let steps = u32::from(want)
        .saturating_add(START_PATIENCE)
        .saturating_mul(which.tries())
        .saturating_add(64);
    for _ in 0..steps {
        let (on, outcome) = in_step(platform, |ctx| {
            let outcome = ctx
                .node
                .collect_try(ctx.source, which, ctx.area, region, want, ctx.now);
            (ctx.index, outcome)
        })?;
        match outcome {
            // The draw arrived: the next one has all its tries.
            Try::Stored | Try::Completed => {
                tries.note(on, which, outcome);
            }
            // Nothing more is wanted: the region is full, or the instruction is not one that
            // may be drawn from.
            Try::Unwanted => {
                let have = in_step(platform, |ctx| ctx.node.region_draws(region))?;
                return Ok(if have >= want {
                    Collected::Done
                } else {
                    Collected::Without
                });
            }
            Try::Failed => return Ok(Collected::Without),
            Try::NotReady | Try::SetAside => {
                if let Some(set_aside) = tries.note(on, which, outcome) {
                    if !set_aside && patience != 0 {
                        // "Not now" is not a failure (R4 item 1), and a start can wait: the
                        // draw is made again.
                        patience = patience.saturating_sub(1);
                        in_step(platform, |ctx| ctx.node.waited_at_start(which))?;
                        platform.pause();
                        continue;
                    }
                    in_step(platform, |ctx| {
                        let failed = ctx.node.gave_up(which, set_aside, ctx.now);
                        if !failed && testing {
                            ctx.node.abandon_test(which);
                        }
                        ctx.node.empty_region(ctx.area, region);
                    })?;
                    return Ok(Collected::Without);
                }
                platform.pause();
            }
        }
    }
    in_step(platform, |ctx| ctx.node.empty_region(ctx.area, region))?;
    Ok(Collected::Without)
}

/// Starts a root on the processor the caller is on: the known-answer self-test, a start-up
/// test of each instruction the processor reports, and the root's first seed from their
/// draws (R4, R5, R8). The boot processor does this in the kernel; the loader does it for its
/// own generator (R10).
///
/// `extra` says the caller put [`crate::seed::EXTRA_LEN`] bytes into
/// [`crate::seed::extra_slot`] of the seed area: the loader's seed in the kernel, the
/// firmware's bytes in the loader. They are mixed in and never counted (R3).
///
/// With no healthy instruction, or a failed self-test, there is no root, and the report says
/// which.
pub fn start_root(platform: &mut dyn Platform, extra: bool) -> RootStart {
    start_root_after(platform, extra, selftest::run())
}

/// [`start_root`], given how the self-test went.
pub(crate) fn start_root_after(
    platform: &mut dyn Platform,
    extra: bool,
    self_test: bool,
) -> RootStart {
    let mut start = RootStart {
        self_test,
        rdseed: Condition::Absent,
        rdrand: Condition::Absent,
        instantiated: false,
    };
    let started = in_step(platform, |ctx| {
        ctx.node.start(ctx.source, ctx.index);
    });
    if started.is_ok() && self_test {
        start.instantiated = seed_root(platform, extra).unwrap_or(false);
    }
    let instantiated = start.instantiated;
    if let Ok((rdseed, rdrand)) = in_step(platform, |ctx| {
        // Whatever happened, nothing of a start-up test stays in the area. With no root, the
        // extra bytes the caller put in its head were not used and go as well.
        ctx.node.empty_region(ctx.area, 0);
        ctx.node.empty_region(ctx.area, 1);
        if !instantiated {
            wipe(ctx.area);
        }
        (
            ctx.node.condition(Instruction::Rdseed),
            ctx.node.condition(Instruction::Rdrand),
        )
    }) {
        start.rdseed = rdseed;
        start.rdrand = rdrand;
    }
    start
}

fn seed_root(platform: &mut dyn Platform, extra: bool) -> Result<bool, Error> {
    let mut regions = 0u8;
    let mut healthy = None;
    for which in Instruction::BOTH {
        let reported = in_step(platform, |ctx| {
            ctx.node.condition(which) != Condition::Absent
        })?;
        if reported && collect(platform, which, regions, STARTUP_DRAWS, true)? == Collected::Done {
            regions = regions.saturating_add(1);
            healthy = healthy.or(Some(which));
        }
    }
    let Some(healthy) = healthy else {
        return Ok(false);
    };
    if regions == 1 && collect(platform, healthy, 1, NONCE_DRAWS, false)? != Collected::Done {
        return Ok(false);
    }
    in_root_step(platform, |ctx, root| {
        root.instantiate(ctx.node, ctx.area, ctx.now, ctx.tsc, extra)
    })
}

/// Starts the generator of the processor the caller is on (R5): its instructions' start-up
/// tests, unless they have been run (the boot processor ran them for the root, and draws
/// again here), then a first seed from the root and from its own draws.
///
/// A processor none of whose instructions is healthy still gets a generator, seeded by the
/// root. With no root there is none.
pub fn start_processor(platform: &mut dyn Platform) -> ProcessorStart {
    let mut start = ProcessorStart {
        rdseed: Condition::Absent,
        rdrand: Condition::Absent,
        own_regions: 0,
        generator: false,
    };
    if let Ok((regions, generator)) = seed_processor(platform) {
        start.own_regions = regions;
        start.generator = generator;
    }
    if let Ok((rdseed, rdrand)) = in_step(platform, |ctx| {
        ctx.node.empty_region(ctx.area, 0);
        ctx.node.empty_region(ctx.area, 1);
        (
            ctx.node.condition(Instruction::Rdseed),
            ctx.node.condition(Instruction::Rdrand),
        )
    }) {
        start.rdseed = rdseed;
        start.rdrand = rdrand;
    }
    start
}

fn seed_processor(platform: &mut dyn Platform) -> Result<(u8, bool), Error> {
    in_step(platform, |ctx| {
        ctx.node.start(ctx.source, ctx.index);
    })?;
    let mut regions = 0u8;
    for which in Instruction::BOTH {
        let testing = match in_step(platform, |ctx| ctx.node.condition(which))? {
            Condition::Untested => true,
            Condition::Healthy => false,
            Condition::Absent | Condition::Testing | Condition::Unhealthy => continue,
        };
        if collect(platform, which, regions, STARTUP_DRAWS, testing)? == Collected::Done {
            regions = regions.saturating_add(1);
        }
    }
    // The root is told how this processor's instructions stand, and is reseeded first if it
    // is due and this processor can do it (R6).
    let due = in_root_step(platform, |ctx, root| {
        root.account(ctx.node);
        root.due(ctx.now) && !root.stopped()
    })?;
    if due {
        let reseeded = fill_store(platform).unwrap_or(false)
            && in_root_step(platform, |ctx, root| {
                root.reseed_from(ctx.node, ctx.now, ctx.tsc)
            }) == Ok(Answer::Done);
        // Draws that did not reach the root are not kept for later.
        in_step(platform, |ctx| {
            ctx.node.drop_root_draws();
            if reseeded {
                ctx.node.looked_at_root();
            }
        })?;
    }
    let answer = in_root_step(platform, |ctx, root| root.seed_processor(ctx.node))?;
    if answer != Answer::Done {
        return Ok((0, false));
    }
    let generator = in_step(platform, |ctx| {
        ctx.node.instantiate_from_root(ctx.area, ctx.now, ctx.tsc)
    })?;
    Ok((regions, generator))
}

/// Gathers a reseed's draws into this processor's store. `false` if they cannot be had now.
fn fill_store(platform: &mut dyn Platform) -> Result<bool, Error> {
    let mut tries = Tries::new(platform.quick());
    let mut allow = Allow::ALL;
    // At most a reseed's draws are made here, wherever the thread goes meanwhile (S2).
    let mut drawn = 0u8;
    for _ in 0..MAINTAIN_STEPS {
        if drawn >= RESEED_DRAWS {
            return in_step(platform, |ctx| ctx.node.store_full());
        }
        match in_step(platform, |ctx| {
            ctx.node.gather_for_root(ctx.source, ctx.now, allow)
        })? {
            Maint::Idle => return Ok(true),
            Maint::Tried {
                on, which, outcome, ..
            } => {
                if matches!(outcome, Try::Stored | Try::Completed) {
                    drawn = drawn.saturating_add(1);
                }
                if let Some(set_aside) = tries.note(on, which, outcome) {
                    in_step(platform, |ctx| {
                        ctx.node.gave_up(which, set_aside, ctx.now);
                    })?;
                    allow.stop(which);
                } else if matches!(outcome, Try::NotReady | Try::SetAside) {
                    platform.pause();
                }
            }
            Maint::Delayed
            | Maint::Reseeded
            | Maint::NeedRoot
            | Maint::TellRoot
            | Maint::LookAtRoot => {
                return Ok(false);
            }
        }
    }
    Ok(false)
}

/// What is due before a request on this processor: draws towards a start-up test that is
/// being run again, and a reseed (S1 to S3). Bounded: at most [`REQUEST_DRAWS`] draws.
///
/// `first` is what the step that found the processor due did already.
fn maintain(platform: &mut dyn Platform, first: Maint) -> Result<(), Error> {
    let mut allow = Allow::ALL;
    let mut tries = Tries::new(platform.quick());
    let mut draws = 0u8;
    let (mut retested_rdseed, mut retested_rdrand) = (0u8, 0u8);
    let mut first = Some(first);
    for _ in 0..MAINTAIN_STEPS {
        let did = match first.take() {
            Some(did) => did,
            None => in_step(platform, |ctx| {
                ctx.node.maintain(ctx.source, ctx.now, ctx.tsc, allow)
            })?,
        };
        let Maint::Tried {
            on,
            which,
            purpose,
            outcome,
        } = did
        else {
            match did {
                Maint::NeedRoot => {
                    let answer = in_root_step(platform, |ctx, root| {
                        root.account(ctx.node);
                        root.reseed_processor(ctx.node, ctx.now, ctx.tsc)
                    })?;
                    // The root has nothing to give now: it is left alone for a while. (If the
                    // thread has moved to a processor that needs nothing, nothing was done.)
                    if !matches!(answer, Answer::Done | Answer::NotGathered) {
                        in_step(platform, |ctx| ctx.node.root_refused(ctx.now))?;
                    }
                }
                // R6: the root is reseeded on the same schedule. A processor that has
                // reseeded itself looks at the root and, if it is due, reseeds it too, as
                // long as the request's draws allow it. Otherwise it looks at its next
                // request.
                Maint::Reseeded | Maint::LookAtRoot
                    if draws <= REQUEST_DRAWS.saturating_sub(RESEED_DRAWS) =>
                {
                    look_at_root(platform)?;
                }
                Maint::TellRoot => {
                    in_root_step(platform, |ctx, root| root.account(ctx.node))?;
                    continue;
                }
                _ => {}
            }
            return Ok(());
        };
        let stop_retest = |allow: &mut Allow| match which {
            Instruction::Rdseed => allow.retest_rdseed = false,
            Instruction::Rdrand => allow.retest_rdrand = false,
        };
        match outcome {
            Try::Stored | Try::Completed => {
                draws = draws.saturating_add(1);
                if purpose == Purpose::Retest {
                    let retested = match which {
                        Instruction::Rdseed => &mut retested_rdseed,
                        Instruction::Rdrand => &mut retested_rdrand,
                    };
                    *retested = retested.saturating_add(1);
                    if *retested >= RESEED_DRAWS {
                        stop_retest(&mut allow);
                    }
                }
                if draws >= REQUEST_DRAWS {
                    // Nothing more is drawn in this request. A store that is full is still
                    // used.
                    allow = Allow {
                        retest_rdseed: false,
                        retest_rdrand: false,
                        gather: false,
                        ..allow
                    };
                }
            }
            Try::Failed | Try::Unwanted => {
                if purpose == Purpose::Retest {
                    stop_retest(&mut allow);
                }
            }
            Try::NotReady | Try::SetAside => {}
        }
        if let Some(set_aside) = tries.note(on, which, outcome) {
            in_step(platform, |ctx| {
                ctx.node.gave_up(which, set_aside, ctx.now);
            })?;
            // The request does not try this instruction again, wherever it is by then.
            allow.stop(which);
        } else if matches!(outcome, Try::NotReady | Try::SetAside) {
            platform.pause();
        }
    }
    Ok(())
}

/// Runs `step` as one step under the root's lock if the lock is free, and does not wait for
/// it: the root's own reseed never makes a request wait for a lock another processor holds.
fn try_root_step<T>(
    platform: &mut dyn Platform,
    mut step: impl FnMut(&mut Ctx<'_>, &mut Root) -> T,
) -> Option<T> {
    let mut result = None;
    platform
        .root_step(&mut |ctx, root| result = Some(step(ctx, root)))
        .ok()?;
    result
}

/// Looks whether the root is due and, if it is, reseeds it from eight draws of this processor
/// (R6): the root keeps the processors' schedule, 2^20 requests or 60 seconds, as long as some
/// processor with a healthy instruction reseeds itself.
///
/// It never waits for the root's lock. If the lock is taken, or the draws cannot be had now,
/// the processor leaves the root alone for [`crate::REST_NANOS`] and looks again at a later
/// request; the draws it made are not kept.
fn look_at_root(platform: &mut dyn Platform) -> Result<(), Error> {
    let due = try_root_step(platform, |ctx, root| {
        root.account(ctx.node);
        let due = root.due(ctx.now) && !root.stopped();
        if !due {
            ctx.node.looked_at_root();
        }
        due
    });
    if due == Some(false) {
        return Ok(());
    }
    let reseeded = due == Some(true)
        && fill_store(platform).unwrap_or(false)
        && try_root_step(platform, |ctx, root| {
            let answer = root.reseed_from(ctx.node, ctx.now, ctx.tsc);
            if answer == Answer::Done {
                ctx.node.looked_at_root();
            }
            answer
        }) == Some(Answer::Done);
    in_step(platform, |ctx| {
        // Draws that did not reach the root are not kept for later.
        ctx.node.drop_root_draws();
        if !reseeded {
            ctx.node.root_refused(ctx.now);
        }
    })
}

/// One piece of a request, with the maintenance that is due before it.
fn piece(platform: &mut dyn Platform, out: &mut [u8], maintained: &mut bool) -> Result<(), Error> {
    for _ in 0..2 {
        let allow = if *maintained { None } else { Some(Allow::ALL) };
        match in_step(platform, |ctx| {
            ctx.node.piece(out, ctx.source, ctx.now, ctx.tsc, allow)
        })? {
            Piece::Done => return Ok(()),
            Piece::NoGenerator => return Err(Error::NoGenerator),
            Piece::NotNow => return Err(Error::NotNow),
            Piece::Due(first) => {
                // A reseed that cannot be made now is delayed, and the request is answered
                // from the state there is (T5).
                let _ = maintain(platform, first);
                *maintained = true;
            }
        }
    }
    Err(Error::NotNow)
}

/// Fills `out` from the generator of the processor the caller is on (R11). A request longer
/// than [`PIECE`] bytes is answered in pieces, one step each (R7).
///
/// # Errors
///
/// [`Error::NoGenerator`] or [`Error::NotNow`]; `out` then holds zeros.
pub fn fill(platform: &mut dyn Platform, out: &mut [u8]) -> Result<(), Error> {
    let mut maintained = false;
    let mut result = Ok(());
    if out.is_empty() {
        // A request for no bytes is still a request: it is refused where there is no generator.
        result = piece(platform, out, &mut maintained);
    }
    for part in out.chunks_mut(PIECE) {
        result = piece(platform, part, &mut maintained);
        if result.is_err() {
            break;
        }
    }
    if result.is_err() {
        wipe(out);
    }
    result
}

/// 64 random bits from the generator of the processor the caller is on (R11).
///
/// # Errors
///
/// As [`fill`]. There is no value that stands for "none".
pub fn next_u64(platform: &mut dyn Platform) -> Result<u64, Error> {
    let mut bytes = [0u8; 8];
    let filled = fill(platform, &mut bytes);
    let value = u64::from_le_bytes(bytes);
    // The value is the caller's from here on; this copy of it does not stay behind.
    wipe(&mut bytes);
    filled.map(|()| value)
}

/// One piece with prediction resistance: eight RDSEED draws of the processor the caller is
/// on, then the root reseeded from them and asked (R6; SP 800-90A §9.3.1).
///
/// Every one of the eight draws is made after the request began. The store is overwritten when
/// the request begins on a processor, and again if the request ends without an answer on the
/// processor it last gathered on, so a draw that was in memory before the request is never
/// the root's fresh entropy. What a request left on a processor it moved away from stays
/// until that store is next used, and whatever uses it next begins by overwriting it or, for
/// the root's ordinary reseed, uses it up.
fn fresh_piece(platform: &mut dyn Platform, out: &mut [u8]) -> Result<(), Error> {
    // The processor this request is gathering on, and the number its store was given.
    let mut mine: Option<(u32, u32)> = None;
    let answered = fresh_gather(platform, out, &mut mine);
    if answered.is_err()
        && let Some((cpu, epoch)) = mine
    {
        let _ = in_step(platform, |ctx| {
            if ctx.index == cpu {
                ctx.node.end_fresh(epoch);
            }
        });
    }
    answered
}

fn fresh_gather(
    platform: &mut dyn Platform,
    out: &mut [u8],
    mine: &mut Option<(u32, u32)>,
) -> Result<(), Error> {
    let mut tries = Tries::new(platform.quick());
    for _ in 0..FRESH_STEPS {
        let begun = *mine;
        let (on, generator, epoch, did) = in_step(platform, |ctx| {
            if !ctx.node.has_generator() {
                return (ctx.index, false, 0, Fresh::Unavailable);
            }
            // A request that has moved to another processor begins there afresh.
            let here = begun.and_then(|(cpu, epoch)| (cpu == ctx.index).then_some(epoch));
            let (epoch, did) = ctx.node.fresh_try(ctx.source, ctx.now, here);
            (ctx.index, true, epoch, did)
        })?;
        if !generator {
            return Err(Error::NoGenerator);
        }
        let outcome = match did {
            // RDSEED is not healthy here, or is resting: no fresh entropy now.
            Fresh::Unavailable => return Err(Error::NotNow),
            Fresh::Ready => {
                *mine = Some((on, epoch));
                let answer = in_root_step(platform, |ctx, root| {
                    if ctx.index != on {
                        // The thread moved after its last draw: gather where it is now.
                        return Answer::NotGathered;
                    }
                    root.account(ctx.node);
                    root.fresh_piece(ctx.node, out, ctx.now, ctx.tsc, epoch)
                })?;
                match answer {
                    Answer::Done => return Ok(()),
                    Answer::NotGathered => continue,
                    Answer::NoGenerator => return Err(Error::NoGenerator),
                    Answer::Stopped | Answer::NotNow => return Err(Error::NotNow),
                }
            }
            Fresh::Tried(outcome) => {
                *mine = Some((on, epoch));
                outcome
            }
        };
        match outcome {
            // The draw arrived: the next one has all its tries.
            Try::Stored | Try::Completed | Try::Unwanted => {
                tries.note(on, Instruction::Rdseed, outcome);
            }
            Try::Failed => return Err(Error::NotNow),
            Try::NotReady | Try::SetAside => {
                if let Some(set_aside) = tries.note(on, Instruction::Rdseed, outcome) {
                    in_step(platform, |ctx| {
                        ctx.node.gave_up(Instruction::Rdseed, set_aside, ctx.now);
                    })?;
                    return Err(Error::NotNow);
                }
                platform.pause();
            }
        }
    }
    Err(Error::NotNow)
}

/// Fills `out` with prediction resistance (R11): every piece is generated by the root
/// straight after a reseed from fresh RDSEED draws. For a value that must not depend on any
/// state that existed before the request, such as a long-lived key.
///
/// # Errors
///
/// [`Error::NotNow`] where the processor the caller is on has no healthy RDSEED, or RDSEED
/// did not deliver; [`Error::NoGenerator`]. `out` then holds zeros.
pub fn fill_fresh(platform: &mut dyn Platform, out: &mut [u8]) -> Result<(), Error> {
    let mut result = Ok(());
    if out.is_empty() {
        result = fresh_piece(platform, out);
    }
    for part in out.chunks_mut(PIECE) {
        result = fresh_piece(platform, part);
        if result.is_err() {
            break;
        }
    }
    if result.is_err() {
        wipe(out);
    }
    result
}

/// Fills `out` from the root itself: for a root with no processors' generators under it,
/// which is the loader's (R10).
///
/// # Errors
///
/// As [`fill`].
pub fn fill_from_root(platform: &mut dyn Platform, out: &mut [u8]) -> Result<(), Error> {
    let mut result = Ok(());
    for part in out.chunks_mut(PIECE) {
        result = match in_root_step(platform, |_, root| root.piece(part)) {
            Ok(Answer::Done) => Ok(()),
            Ok(Answer::NoGenerator) => Err(Error::NoGenerator),
            Ok(_) | Err(_) => Err(Error::NotNow),
        };
        if result.is_err() {
            break;
        }
    }
    if result.is_err() {
        wipe(out);
    }
    result
}
