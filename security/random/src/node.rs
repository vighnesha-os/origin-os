//! One processor's generator, its two instructions' tests and its store: every operation here
//! is meant to run as one step (ADR-0016 R4 to R7; TASKS 18b S1 to S3).
//!
//! A step is the unit the caller runs with interrupts off, on a stack it overwrites afterwards.
//! Nothing here loops over tries or draws: one call tries one instruction once, or makes one
//! reseed, or answers one piece of a request. The loops are in `drive`, between steps, and see
//! only what a step reports, which is never a draw.

use origin_crypto::HmacDrbgSha512;

use crate::health::{Condition, Fault, Health, Offer, STARTUP_DRAWS};
use crate::seed;
use crate::wipe::{copy, wipe};

/// One of the processor's two instructions, always in its 64-bit form (R3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Instruction {
    /// RDSEED: what a seed is counted on.
    Rdseed,
    /// RDRAND: always mixed in, and counted on only where RDSEED is absent or unhealthy.
    Rdrand,
}

impl Instruction {
    /// Both, RDSEED first.
    pub const BOTH: [Self; 2] = [Self::Rdseed, Self::Rdrand];

    /// How many times one draw tries the instruction before it is "not now" (R4 item 1: 10 is
    /// Intel's figure for RDRAND).
    pub const fn tries(self) -> u32 {
        match self {
            Self::Rdseed => 100,
            Self::Rdrand => 10,
        }
    }

    /// Its mnemonic, for reports.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rdseed => "RDSEED",
            Self::Rdrand => "RDRAND",
        }
    }
}

/// The processor's two instructions, as the caller reaches them.
pub trait Source {
    /// Whether this processor reports the instruction (CPUID).
    fn reports(&self, which: Instruction) -> bool;
    /// One try: the value, if the instruction reported success.
    fn try_once(&mut self, which: Instruction) -> Option<u64>;
}

/// Draws that make one reseed: 512 bits for the 256 the standard asks (R6).
pub const RESEED_DRAWS: u8 = 8;
/// A generator is due a reseed after this many requests (R6; a first value).
pub const RESEED_REQUESTS: u64 = 1 << 20;
/// Or after this long, in nanoseconds: 60 seconds (R6; a first value).
pub const RESEED_NANOS: u64 = 60_000_000_000;
/// How long an instruction that was "not now" is left alone, in nanoseconds: one second
/// (TASKS 18b S3).
pub const REST_NANOS: u64 = 1_000_000_000;
/// The most one step generates (R7).
pub const PIECE: usize = 128;

/// The bytes of one reseed's draws.
const ENTROPY_LEN: usize = RESEED_DRAWS as usize * 8;
/// A reseed's additional input: the time-stamp counter and the processor's index, then any
/// RDSEED draws that were set aside when RDRAND took over.
const ASIDE_LEN: usize = 16 + ENTROPY_LEN;
/// What a root hands a processor's generator: an entropy input and a nonce (R5).
const FROM_ROOT_LEN: usize = 80;
const FROM_ROOT_ENTROPY: usize = 64;

/// What one try did. It says nothing about the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Try {
    /// A value passed the tests and went where it was wanted.
    Stored,
    /// The same, and it was the last draw of a start-up test: the instruction is healthy.
    Completed,
    /// The instruction was not ready.
    NotReady,
    /// The instruction returned zero or all ones, which is not a value.
    SetAside,
    /// This try was the last the draw had, and none of them gave a stuck value: the draw is
    /// "not now" (R4 item 1), and the instruction rests on this processor.
    NotNow,
    /// A test failed, or the draw used all its tries and some of them gave zero or all ones:
    /// the instruction is unhealthy and what it had given is thrown away.
    Failed,
    /// Nothing wanted the try, so the instruction was not run.
    Unwanted,
}

/// What one step of a request with prediction resistance did.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Fresh {
    /// RDSEED is not healthy on this processor, or is resting: no fresh entropy now.
    Unavailable,
    /// The store holds the eight draws, all made since the request began here.
    Ready,
    /// One try was made.
    Tried(Try),
}

impl core::fmt::Debug for Fresh {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "Unavailable",
            Self::Ready => "Ready",
            Self::Tried(_) => "Tried",
        })
    }
}

/// What a try in [`Node::maintain`] was for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// A start-up test that is being run again.
    Retest,
    /// A reseed that is due.
    Reseed,
}

/// What one call of [`Node::maintain`] did.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Maint {
    /// Nothing is due, or nothing more is allowed in this request.
    Idle,
    /// One try was made.
    Tried {
        /// The processor it was made on.
        on: u32,
        /// The instruction.
        which: Instruction,
        /// What for.
        purpose: Purpose,
        /// What it did.
        outcome: Try,
    },
    /// The store was full and the generator was reseeded from it.
    Reseeded,
    /// A reseed is due and this processor has no healthy instruction: ask the root.
    NeedRoot,
    /// An instruction's health has changed since the root was last told: tell it.
    TellRoot,
    /// Nothing is due here, and this processor reseeded itself without having looked whether
    /// the root is due as well: look now.
    LookAtRoot,
    /// A reseed is due and cannot be made now: the instruction it needs is resting.
    Delayed,
}

impl core::fmt::Debug for Maint {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Idle => "Idle",
            Self::Tried { .. } => "Tried",
            Self::Reseeded => "Reseeded",
            Self::NeedRoot => "NeedRoot",
            Self::TellRoot => "TellRoot",
            Self::LookAtRoot => "LookAtRoot",
            Self::Delayed => "Delayed",
        })
    }
}

/// What a request may still do towards maintenance (TASKS 18b S2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Allow {
    /// Draws towards a start-up test of RDSEED that is being run again.
    pub retest_rdseed: bool,
    /// The same for RDRAND.
    pub retest_rdrand: bool,
    /// Draws for a reseed that is due. A store that is already full is used whatever this
    /// says.
    pub gather: bool,
}

impl Allow {
    /// Everything.
    pub const ALL: Self = Self {
        retest_rdseed: true,
        retest_rdrand: true,
        gather: true,
    };

    const fn retest(self, which: Instruction) -> bool {
        match which {
            Instruction::Rdseed => self.retest_rdseed,
            Instruction::Rdrand => self.retest_rdrand,
        }
    }
}

/// What became of one piece of a request.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Piece {
    /// The bytes were written.
    Done,
    /// Nothing was written: a reseed or a test is due first. The step did the first piece of
    /// that maintenance instead, and this is what it did.
    Due(Maint),
    /// Nothing was written: this processor has no generator.
    NoGenerator,
    /// Nothing was written: the generator has reached the standard's limit of requests without
    /// a reseed.
    NotNow,
}

impl core::fmt::Debug for Piece {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Done => "Done",
            Self::Due(_) => "Due",
            Self::NoGenerator => "NoGenerator",
            Self::NotNow => "NotNow",
        })
    }
}

/// Counts for reports. None is secret.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Reseeds from RDSEED.
    pub reseeds_rdseed: u32,
    /// Reseeds from RDRAND.
    pub reseeds_rdrand: u32,
    /// Of those, the ones made from RDRAND because a healthy RDSEED was not ready (S3).
    pub reseeds_dry: u32,
    /// Reseeds from the root's output.
    pub reseeds_root: u32,
    /// Draws of RDSEED that ended "not now".
    pub not_now_rdseed: u32,
    /// Draws of RDRAND that ended "not now".
    pub not_now_rdrand: u32,
    /// Start-up tests begun again after a failure.
    pub retests: u32,
}

impl core::fmt::Debug for Counts {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Counts { .. }")
    }
}

/// Whether `period` has passed since `since`. A clock that reads earlier than `since` has
/// stepped back: the difference then wraps to a number larger than any period, so what was
/// being waited for is held to have passed, and a wait never outlasts its period because of
/// the clock.
pub(crate) const fn passed(now: u64, since: u64, period: u64) -> bool {
    now.wrapping_sub(since) >= period
}

/// One instruction's tests and its schedule.
struct Line {
    health: Health,
    /// When it began to rest after a "not now", if it is resting.
    rest_since: Option<u64>,
    /// When it last failed, if a new start-up test has to wait a reseed period for that.
    failed_at: Option<u64>,
    /// What the root has been told about its health.
    told_healthy: bool,
    /// Tries of the draw in hand, on this processor, that gave no value. A draw's tries are
    /// counted here and nowhere else, so what follows the last of them falls on the processor
    /// that made them, wherever the thread that asked is by then.
    dry: u32,
    /// Whether any of those tries gave zero or all ones.
    stuck: bool,
}

impl Line {
    const fn new() -> Self {
        Self {
            health: Health::absent(),
            rest_since: None,
            failed_at: None,
            told_healthy: false,
            dry: 0,
            stuck: false,
        }
    }

    /// Whether the instruction is resting after a "not now" (S3).
    const fn resting(&self, now: u64) -> bool {
        match self.rest_since {
            Some(since) => !passed(now, since, REST_NANOS),
            None => false,
        }
    }

    /// Whether a new start-up test may begin: a reseed period after the last failure.
    const fn may_retest(&self, now: u64) -> bool {
        match self.failed_at {
            Some(at) => passed(now, at, RESEED_NANOS),
            None => true,
        }
    }
}

/// One processor's generator and everything that feeds it.
///
/// It is neither `Clone` nor `Copy`, and its `Debug` prints nothing of it.
pub struct Node {
    pub(crate) drbg: HmacDrbgSha512,
    rdseed: Line,
    rdrand: Line,
    pub(crate) index: u32,
    started: bool,
    /// Draws gathered for the next reseed.
    pub(crate) entropy: [u8; ENTROPY_LEN],
    /// How many of them there are.
    pub(crate) have: u8,
    /// The instruction they came from.
    pub(crate) from: Instruction,
    /// A reseed's additional input (see [`ASIDE_LEN`]).
    pub(crate) aside: [u8; ASIDE_LEN],
    /// RDSEED draws set aside in it.
    pub(crate) aside_have: u8,
    /// The request the store is lent to, if it is: one that gathers for the root, with or
    /// without prediction resistance. Only the request that holds this number uses the draws,
    /// and this generator's own reseed never takes them over. The number holds the
    /// processor's index, so no other processor ever gives the same one.
    pub(crate) lent: Option<u64>,
    /// How often the store has been lent.
    lends: u32,
    /// What the root handed over.
    pub(crate) from_root: [u8; FROM_ROOT_LEN],
    /// How much of it is there.
    pub(crate) from_root_len: u8,
    /// Draws collected into each region of the seed area, and the instruction they are of.
    pub(crate) region0: u16,
    pub(crate) region1: u16,
    region0_of: Option<Instruction>,
    region1_of: Option<Instruction>,
    /// When the generator was last seeded.
    pub(crate) seeded_at: u64,
    /// When the root last could not be asked for a reseed, or reached for its own, if it is
    /// being left alone.
    root_rest_since: Option<u64>,
    /// This processor has reseeded itself and has not yet looked whether the root is due (R6).
    root_look: bool,
    /// When the root was last seeded, as this processor found it at its last look. The
    /// processor looks again when a reseed period has passed since then, so the root's
    /// schedule does not hang on when this processor reseeds itself.
    root_seen: Option<u64>,
    counts: Counts,
}

impl Node {
    /// A processor that has not started: no generator, no instruction.
    pub const fn new() -> Self {
        Self {
            drbg: HmacDrbgSha512::new(),
            rdseed: Line::new(),
            rdrand: Line::new(),
            index: 0,
            started: false,
            entropy: [0; ENTROPY_LEN],
            have: 0,
            from: Instruction::Rdseed,
            aside: [0; ASIDE_LEN],
            aside_have: 0,
            lent: None,
            lends: 0,
            from_root: [0; FROM_ROOT_LEN],
            from_root_len: 0,
            region0: 0,
            region1: 0,
            region0_of: None,
            region1_of: None,
            seeded_at: 0,
            root_rest_since: None,
            root_look: false,
            root_seen: None,
            counts: Counts {
                reseeds_rdseed: 0,
                reseeds_rdrand: 0,
                reseeds_dry: 0,
                reseeds_root: 0,
                not_now_rdseed: 0,
                not_now_rdrand: 0,
                retests: 0,
            },
        }
    }

    const fn line(&self, which: Instruction) -> &Line {
        match which {
            Instruction::Rdseed => &self.rdseed,
            Instruction::Rdrand => &self.rdrand,
        }
    }

    const fn line_mut(&mut self, which: Instruction) -> &mut Line {
        match which {
            Instruction::Rdseed => &mut self.rdseed,
            Instruction::Rdrand => &mut self.rdrand,
        }
    }

    /// Records which instructions the processor reports, and its index. Once: a second call
    /// changes nothing and returns `false`.
    pub fn start(&mut self, source: &dyn Source, index: u32) -> bool {
        if self.started {
            return false;
        }
        self.started = true;
        self.index = index;
        for which in Instruction::BOTH {
            if source.reports(which) {
                self.line_mut(which).health = Health::untested();
            }
        }
        true
    }

    /// The processor's index.
    pub const fn index(&self) -> u32 {
        self.index
    }

    /// Whether the processor has a generator.
    pub const fn has_generator(&self) -> bool {
        self.drbg.is_instantiated()
    }

    /// Where an instruction stands.
    pub const fn condition(&self, which: Instruction) -> Condition {
        self.line(which).health.condition()
    }

    /// Whether an instruction's draws may be used.
    pub const fn is_healthy(&self, which: Instruction) -> bool {
        self.line(which).health.is_healthy()
    }

    /// An instruction's last failure.
    pub const fn last_fault(&self, which: Instruction) -> Option<Fault> {
        self.line(which).health.last_fault()
    }

    /// Draws of an instruction that have passed its tests since the processor started.
    pub const fn accepted(&self, which: Instruction) -> u64 {
        self.line(which).health.accepted()
    }

    /// Values of an instruction set aside as zero or all ones.
    pub const fn set_aside(&self, which: Instruction) -> u32 {
        self.line(which).health.set_aside()
    }

    /// Failures of an instruction's tests.
    pub const fn faults(&self, which: Instruction) -> u32 {
        self.line(which).health.faults()
    }

    /// The counts.
    pub const fn counts(&self) -> Counts {
        self.counts
    }

    /// Requests answered since the generator was last seeded.
    pub const fn requests_since_seed(&self) -> u64 {
        self.drbg.requests_since_seed()
    }

    /// Whether the generator is due a reseed (R6).
    pub const fn due(&self, now: u64) -> bool {
        self.drbg.is_instantiated()
            && (self.drbg.requests_since_seed() >= RESEED_REQUESTS
                || passed(now, self.seeded_at, RESEED_NANOS))
    }

    /// Whether a start-up test is being run again.
    const fn retesting(&self) -> bool {
        matches!(self.rdseed.health.condition(), Condition::Testing)
            || matches!(self.rdrand.health.condition(), Condition::Testing)
    }

    /// Whether an instruction's health differs from what the root was last told.
    const fn owes_root(&self) -> bool {
        self.rdseed.health.is_healthy() != self.rdseed.told_healthy
            || self.rdrand.health.is_healthy() != self.rdrand.told_healthy
    }

    /// Draws collected into a region of the seed area.
    pub const fn region_draws(&self, region: u8) -> u16 {
        self.collected(region)
    }

    const fn collected(&self, region: u8) -> u16 {
        if region == 0 {
            self.region0
        } else {
            self.region1
        }
    }

    const fn set_collected(&mut self, region: u8, draws: u16) {
        if region == 0 {
            self.region0 = draws;
        } else {
            self.region1 = draws;
        }
    }

    const fn region_of(&self, region: u8) -> Option<Instruction> {
        if region == 0 {
            self.region0_of
        } else {
            self.region1_of
        }
    }

    const fn set_region_of(&mut self, region: u8, which: Option<Instruction>) {
        if region == 0 {
            self.region0_of = which;
        } else {
            self.region1_of = which;
        }
    }

    /// Forgets what the regions held, once a first seed has taken them or they were wiped.
    pub(crate) const fn forget_regions(&mut self) {
        self.region0 = 0;
        self.region1 = 0;
        self.region0_of = None;
        self.region1_of = None;
    }

    /// Empties each region whose instruction has failed since its draws were collected: those
    /// draws are "in hand" and go with the others (R4 item 8). If that leaves region 1 alone
    /// full, its draws move to region 0, so that a first seed's own draws stay one slice.
    pub(crate) fn settle_regions(&mut self, area: &mut [u8]) {
        for region in [0u8, 1] {
            if let Some(which) = self.region_of(region)
                && !self.is_healthy(which)
            {
                self.empty_region(area, region);
            }
        }
        if self.region0 == 0 && self.region1 == STARTUP_DRAWS {
            let middle = seed::HEAD.saturating_add(seed::REGION);
            if let Some((low, high)) = area.split_at_mut_checked(middle)
                && let (Some(to), Some(from)) =
                    (low.get_mut(seed::HEAD..), high.get(..seed::REGION))
            {
                copy(to, from);
                self.region0 = self.region1;
                self.region0_of = self.region1_of;
            }
            self.empty_region(area, 1);
        }
    }

    /// Throws away what `which` has given and not yet been used for (R4 item 8).
    pub(crate) fn drop_draws(&mut self, which: Instruction) {
        if self.from == which {
            wipe(&mut self.entropy);
            self.have = 0;
        }
        if which == Instruction::Rdseed {
            wipe(&mut self.aside);
            self.aside_have = 0;
        }
    }

    /// One try of `which`, through its tests. `Ok` holds the value and whether it completed a
    /// start-up test.
    ///
    /// A try that gives no value is counted here, on this processor. The last try a draw has
    /// (100 for RDSEED, 10 for RDRAND, one for a caller that came with interrupts off) ends
    /// the draw in the same step: as a failure if any of its tries gave zero or all ones (R4
    /// item 2; a mixture of "not ready" and stuck values is held to be one too, since a
    /// working instruction gives such a value about once in 2^63 tries), and otherwise as
    /// "not now", after which the instruction rests for [`REST_NANOS`] if `rest` says so (S3).
    fn draw(
        &mut self,
        source: &mut dyn Source,
        which: Instruction,
        now: u64,
        quick: bool,
        rest: bool,
    ) -> Result<(u64, bool), Try> {
        let before = self.line(which).health.condition();
        if !matches!(before, Condition::Testing | Condition::Healthy) {
            return Err(Try::Unwanted);
        }
        let Some(value) = source.try_once(which) else {
            return Err(self.no_value(which, false, now, quick, rest));
        };
        match self.line_mut(which).health.offer(value) {
            Offer::Accepted => {
                let line = self.line_mut(which);
                line.dry = 0;
                line.stuck = false;
                let completed =
                    matches!(before, Condition::Testing) && self.line(which).health.is_healthy();
                Ok((value, completed))
            }
            Offer::SetAside => Err(self.no_value(which, true, now, quick, rest)),
            Offer::Failed(_) => {
                self.failed(which, now);
                Err(Try::Failed)
            }
        }
    }

    /// Counts a try of `which` that gave no value, and ends the draw if it was its last.
    fn no_value(
        &mut self,
        which: Instruction,
        stuck: bool,
        now: u64,
        quick: bool,
        rest: bool,
    ) -> Try {
        let budget = if quick { 1 } else { which.tries() };
        let line = self.line_mut(which);
        line.dry = line.dry.saturating_add(1);
        line.stuck |= stuck;
        if line.dry < budget {
            return if stuck { Try::SetAside } else { Try::NotReady };
        }
        let saw_stuck = line.stuck;
        line.dry = 0;
        line.stuck = false;
        if saw_stuck {
            if matches!(self.line_mut(which).health.stuck(), Offer::Failed(_)) {
                self.failed(which, now);
            }
            return Try::Failed;
        }
        if rest {
            self.line_mut(which).rest_since = Some(now);
        }
        self.count_not_now(which);
        Try::NotNow
    }

    /// What follows any failure of `which`: its draws go, and it is not tested again for one
    /// reseed period.
    fn failed(&mut self, which: Instruction, now: u64) {
        self.drop_draws(which);
        let line = self.line_mut(which);
        line.failed_at = Some(now);
        line.dry = 0;
        line.stuck = false;
    }

    /// Begins a start-up test of `which`, if the processor reports it and none has passed.
    pub fn begin_test(&mut self, which: Instruction) {
        if matches!(
            self.line(which).health.condition(),
            Condition::Untested | Condition::Unhealthy
        ) {
            self.line_mut(which).health.begin_test();
        }
    }

    /// One try of `which` towards the `want` draws of `region` in the seed area: a start-up
    /// test's draws, or a healthy instruction's further ones. A failure empties the region.
    pub fn collect_try(
        &mut self,
        source: &mut dyn Source,
        which: Instruction,
        area: &mut [u8],
        region: u8,
        want: u16,
        now: u64,
    ) -> Try {
        let have = self.collected(region);
        if have >= want || area.len() < seed::AREA_LEN {
            return Try::Unwanted;
        }
        // A start has nothing else to do and makes a "not now" draw again, so nothing rests.
        let (value, completed) = match self.draw(source, which, now, false, false) {
            Ok(drawn) => drawn,
            Err(outcome) => {
                if outcome == Try::Failed {
                    self.empty_region(area, region);
                }
                return outcome;
            }
        };
        let Some(slot) = seed::draw_slot(area, region, have) else {
            return Try::Unwanted;
        };
        copy(slot, &value.to_le_bytes());
        self.set_collected(region, have.saturating_add(1));
        self.set_region_of(region, Some(which));
        if completed {
            Try::Completed
        } else {
            Try::Stored
        }
    }

    /// Empties a region of the seed area.
    pub fn empty_region(&mut self, area: &mut [u8], region: u8) {
        if let Some(bytes) = seed::region(area, region) {
            wipe(bytes);
        }
        self.set_collected(region, 0);
        self.set_region_of(region, None);
    }

    /// Counts a draw of `which` that ended "not now".
    fn count_not_now(&mut self, which: Instruction) {
        match which {
            Instruction::Rdseed => {
                self.counts.not_now_rdseed = self.counts.not_now_rdseed.saturating_add(1);
            }
            Instruction::Rdrand => {
                self.counts.not_now_rdrand = self.counts.not_now_rdrand.saturating_add(1);
            }
        }
    }

    /// Rests `which` for [`REST_NANOS`]: for a start that has waited out as many "not now"
    /// draws as it will.
    pub fn rest(&mut self, which: Instruction, now: u64) {
        self.line_mut(which).rest_since = Some(now);
    }

    /// Gives up a start-up test that could not be finished while the processor starts: the
    /// instruction is unhealthy, and a new test may begin when a reseed next falls due.
    pub fn abandon_test(&mut self, which: Instruction) {
        self.line_mut(which).health.abandon_test();
        self.drop_draws(which);
        self.line_mut(which).failed_at = None;
    }

    /// One try of `which` for the store. Draws of the other instruction that are in the way
    /// are moved aside (RDSEED's, which then go into the reseed as additional input) or dropped.
    fn gather_try(
        &mut self,
        source: &mut dyn Source,
        which: Instruction,
        now: u64,
        quick: bool,
    ) -> Try {
        if self.have >= RESEED_DRAWS && self.from == which {
            return Try::Unwanted;
        }
        if self.have != 0 && self.from != which {
            if which == Instruction::Rdrand {
                let len = usize::from(self.have).saturating_mul(8);
                if let (Some(to), Some(from)) = (self.aside.get_mut(16..), self.entropy.get(..len))
                {
                    copy(to, from);
                    self.aside_have = self.have;
                }
            }
            wipe(&mut self.entropy);
            self.have = 0;
        }
        self.from = which;
        let (value, completed) = match self.draw(source, which, now, quick, true) {
            Ok(drawn) => drawn,
            Err(outcome) => return outcome,
        };
        let at = usize::from(self.have).saturating_mul(8);
        let Some(slot) = self.entropy.get_mut(at..at.saturating_add(8)) else {
            return Try::Unwanted;
        };
        copy(slot, &value.to_le_bytes());
        self.have = self.have.saturating_add(1);
        if completed {
            Try::Completed
        } else {
            Try::Stored
        }
    }

    /// One try for a reseed, from RDSEED where it is healthy and rested and from RDRAND where
    /// it is not (S3), or `None` if neither can be tried now.
    fn gather_next(
        &mut self,
        source: &mut dyn Source,
        now: u64,
        quick: bool,
    ) -> Option<(Instruction, Try)> {
        let rdseed = self.rdseed.health.is_healthy() && !self.rdseed.resting(now);
        let rdrand = self.rdrand.health.is_healthy() && !self.rdrand.resting(now);
        let which = if rdseed && (self.have == 0 || self.from == Instruction::Rdseed) {
            Instruction::Rdseed
        } else if rdrand {
            Instruction::Rdrand
        } else {
            return None;
        };
        Some((which, self.gather_try(source, which, now, quick)))
    }

    /// Whether the store holds a whole reseed's draws.
    pub const fn store_full(&self) -> bool {
        self.have >= RESEED_DRAWS
    }

    /// Lends the store to a request that gathers for the root: whatever it holds is
    /// overwritten, and it gets a number that no other request and no other processor has.
    fn begin_lend(&mut self) -> u64 {
        self.clear_store();
        self.lends = self.lends.wrapping_add(1);
        let number = (u64::from(self.index) << 32) | u64::from(self.lends);
        self.lent = Some(number);
        number
    }

    /// Whether the store is lent to the request that holds `number`.
    pub(crate) fn lent_to(&self, number: u64) -> bool {
        self.lent == Some(number)
    }

    /// Whether the store is lent to that request and holds a whole reseed's draws.
    pub fn lent_full(&self, number: u64) -> bool {
        self.lent_to(number) && self.store_full()
    }

    /// Ends a lending: the store is overwritten, unless it has been cleared or lent again
    /// since, in which case it is no longer that request's and is left alone.
    pub fn end_lend(&mut self, number: u64) {
        if self.lent_to(number) {
            self.clear_store();
        }
    }

    /// Begins a gathering for the root's own reseed (R6), whether or not a reseed is due
    /// here, and returns the number the store is lent under. Draws another request left in a
    /// lent store are overwritten: a gathering never goes on from them. `None` if the store
    /// holds draws of this generator's own reseed, which comes first: they are neither handed
    /// to the root nor thrown away for it.
    pub fn begin_root_gather(&mut self) -> Option<u64> {
        if self.lent.is_none() && (self.have != 0 || self.aside_have != 0) {
            return None;
        }
        Some(self.begin_lend())
    }

    /// One try towards a full store for the root, by the request that began the gathering
    /// under `number`. [`Maint::Idle`] when the store is full; [`Maint::Delayed`] if no
    /// instruction can be tried now, or the store is no longer lent to that request: it has
    /// been lent again, or the thread is on another processor.
    pub fn gather_for_root(
        &mut self,
        source: &mut dyn Source,
        now: u64,
        number: u64,
        quick: bool,
    ) -> Maint {
        if !self.lent_to(number) {
            return Maint::Delayed;
        }
        if self.store_full() {
            return Maint::Idle;
        }
        match self.gather_next(source, now, quick) {
            Some((which, outcome)) => Maint::Tried {
                on: self.index,
                which,
                purpose: Purpose::Reseed,
                outcome,
            },
            None => Maint::Delayed,
        }
    }

    /// One try of RDSEED towards the eight draws of a request with prediction resistance
    /// (R6). `mine` is the number the request holds, if it has begun.
    ///
    /// Returns the number the request holds now, and what happened. A request that has not
    /// begun here, or whose store has been lent again or cleared since, begins (again) in this
    /// step: whatever the store holds is overwritten, so every draw the request uses is made
    /// after it began, on the processor that answers it.
    pub fn fresh_try(
        &mut self,
        source: &mut dyn Source,
        now: u64,
        mine: Option<u64>,
        quick: bool,
    ) -> (u64, Fresh) {
        if !self.rdseed.health.is_healthy() || self.rdseed.resting(now) {
            return (mine.unwrap_or(0), Fresh::Unavailable);
        }
        let number = match mine {
            Some(number) if self.lent_to(number) => number,
            _ => self.begin_lend(),
        };
        if self.store_is_fresh() {
            return (number, Fresh::Ready);
        }
        (
            number,
            Fresh::Tried(self.gather_try(source, Instruction::Rdseed, now, quick)),
        )
    }

    /// Whether the store holds what a request with prediction resistance needs: eight RDSEED
    /// draws.
    pub(crate) const fn store_is_fresh(&self) -> bool {
        self.store_full() && matches!(self.from, Instruction::Rdseed)
    }

    /// Whether this processor has to ask the root for its reseed: one is due and it has no
    /// healthy instruction of its own (R6).
    pub(crate) const fn wants_root(&self, now: u64) -> bool {
        self.due(now) && !self.rdseed.health.is_healthy() && !self.rdrand.health.is_healthy()
    }

    /// Whether this processor should look whether the root is due, and may now. It should
    /// when it has reseeded itself since it last looked, and when a reseed period has passed
    /// since the root's last seed as it last saw it. It may when it has an instruction to
    /// reseed the root from and the root is not being left alone.
    pub const fn wants_root_look(&self, now: u64) -> bool {
        let should = self.root_look
            || match self.root_seen {
                Some(seeded) => passed(now, seeded, RESEED_NANOS),
                None => false,
            };
        should
            && (self.rdseed.health.is_healthy() || self.rdrand.health.is_healthy())
            && self.root_rested(now)
    }

    /// Records a look at the root: it was last seeded at `seeded`, which is now if this
    /// processor has just reseeded it.
    pub const fn saw_root(&mut self, seeded: u64) {
        self.root_look = false;
        self.root_seen = Some(seeded);
    }

    /// Whether the root may be asked again after it could not be (S3's second).
    const fn root_rested(&self, now: u64) -> bool {
        match self.root_rest_since {
            Some(since) => passed(now, since, REST_NANOS),
            None => true,
        }
    }

    /// Writes the time-stamp counter and the index at the front of the additional input, and
    /// returns how much of it is in use.
    pub(crate) fn stamp(&mut self, tsc: u64) -> usize {
        if let Some(to) = self.aside.get_mut(..8) {
            copy(to, &tsc.to_le_bytes());
        }
        if let Some(to) = self.aside.get_mut(8..16) {
            copy(to, &u64::from(self.index).to_le_bytes());
        }
        16usize.saturating_add(usize::from(self.aside_have).saturating_mul(8))
    }

    /// Empties the store.
    pub(crate) fn clear_store(&mut self) {
        wipe(&mut self.entropy);
        wipe(&mut self.aside);
        self.have = 0;
        self.aside_have = 0;
        self.lent = None;
    }

    /// Reseeds the generator from a full store (R6).
    fn reseed_from_store(&mut self, now: u64, tsc: u64) -> bool {
        if !self.store_full() {
            return false;
        }
        let extra = self.stamp(tsc);
        let reseeded = match self.aside.get(..extra) {
            Some(additional) => self.drbg.reseed(&self.entropy, additional).is_ok(),
            None => false,
        };
        let from = self.from;
        self.clear_store();
        if reseeded {
            self.seeded_at = now;
            // The root keeps the same schedule: this processor looks at it next.
            self.root_look = true;
            match from {
                Instruction::Rdseed => {
                    self.counts.reseeds_rdseed = self.counts.reseeds_rdseed.saturating_add(1);
                }
                Instruction::Rdrand => {
                    self.counts.reseeds_rdrand = self.counts.reseeds_rdrand.saturating_add(1);
                    if self.rdseed.health.is_healthy() {
                        self.counts.reseeds_dry = self.counts.reseeds_dry.saturating_add(1);
                    }
                }
            }
        }
        reseeded
    }

    /// Reseeds the generator from the 64 bytes the root handed over.
    pub(crate) fn reseed_from_root(&mut self, now: u64, tsc: u64) -> bool {
        if usize::from(self.from_root_len) < FROM_ROOT_ENTROPY {
            return false;
        }
        let extra = self.stamp(tsc).min(16);
        let reseeded = match (
            self.from_root.get(..FROM_ROOT_ENTROPY),
            self.aside.get(..extra),
        ) {
            (Some(entropy), Some(additional)) => self.drbg.reseed(entropy, additional).is_ok(),
            _ => false,
        };
        wipe(&mut self.from_root);
        self.from_root_len = 0;
        if reseeded {
            self.seeded_at = now;
            self.counts.reseeds_root = self.counts.reseeds_root.saturating_add(1);
        }
        reseeded
    }

    /// Records that the root could not be asked for a reseed, or could not be reached or
    /// reseeded when this processor looked at it: it is left alone for [`REST_NANOS`], and the
    /// generator goes on with the seed it has (R6).
    pub fn root_refused(&mut self, now: u64) {
        self.root_rest_since = Some(now);
    }

    /// The next piece of maintenance, as one step: one try towards a start-up test that is
    /// being run again, or one try for a reseed that is due, or the reseed itself.
    ///
    /// A start-up test of an unhealthy instruction begins when a reseed falls due, at most once
    /// in a reseed period, and goes on over later requests until it passes or fails (S2).
    pub fn maintain(
        &mut self,
        source: &mut dyn Source,
        now: u64,
        tsc: u64,
        allow: Allow,
        quick: bool,
    ) -> Maint {
        if !self.drbg.is_instantiated() {
            return Maint::Idle;
        }
        if self.owes_root() {
            return Maint::TellRoot;
        }
        let due = self.due(now);
        for which in Instruction::BOTH {
            if !allow.retest(which) {
                continue;
            }
            let line = self.line(which);
            let mut testing = matches!(line.health.condition(), Condition::Testing);
            if !testing
                && due
                && matches!(line.health.condition(), Condition::Unhealthy)
                && line.may_retest(now)
            {
                self.line_mut(which).health.begin_test();
                self.counts.retests = self.counts.retests.saturating_add(1);
                testing = true;
            }
            if testing && !self.line(which).resting(now) {
                let outcome = match self.draw(source, which, now, quick, true) {
                    Ok((_, true)) => Try::Completed,
                    Ok((_, false)) => Try::Stored,
                    Err(outcome) => outcome,
                };
                return Maint::Tried {
                    on: self.index,
                    which,
                    purpose: Purpose::Retest,
                    outcome,
                };
            }
        }
        if !due {
            return if self.wants_root_look(now) {
                Maint::LookAtRoot
            } else {
                Maint::Idle
            };
        }
        if self.lent.is_some() {
            // Draws of a gathering for the root, or of a request with prediction resistance:
            // this generator's reseed does not take them over. Whoever gathered them begins
            // again, if it is still there.
            self.clear_store();
        }
        if self.store_full() {
            return if self.reseed_from_store(now, tsc) {
                Maint::Reseeded
            } else {
                Maint::Delayed
            };
        }
        if !allow.gather {
            return Maint::Idle;
        }
        if let Some((which, outcome)) = self.gather_next(source, now, quick) {
            return Maint::Tried {
                on: self.index,
                which,
                purpose: Purpose::Reseed,
                outcome,
            };
        }
        if !self.rdseed.health.is_healthy() && !self.rdrand.health.is_healthy() {
            if self.root_rested(now) {
                Maint::NeedRoot
            } else {
                Maint::Delayed
            }
        } else {
            Maint::Delayed
        }
    }

    /// One piece of a request: at most [`PIECE`] bytes.
    ///
    /// With `maintain`, a generator that is due a reseed, or has a test running, writes
    /// nothing: the step does the first piece of that maintenance ([`Self::maintain`]) and
    /// says what it did, so a request that finds a processor due always moves that processor
    /// forward, wherever the thread is afterwards. The caller carries the maintenance on and
    /// asks again without `maintain`; the piece is then answered from the state there is,
    /// which is what a delayed reseed means (T5).
    pub fn piece(
        &mut self,
        out: &mut [u8],
        source: &mut dyn Source,
        now: u64,
        tsc: u64,
        maintain: Option<Allow>,
        quick: bool,
    ) -> Piece {
        if !self.drbg.is_instantiated() {
            return Piece::NoGenerator;
        }
        if out.len() > PIECE {
            return Piece::NotNow;
        }
        if let Some(allow) = maintain
            && (self.due(now) || self.retesting() || self.owes_root() || self.wants_root_look(now))
        {
            return Piece::Due(self.maintain(source, now, tsc, allow, quick));
        }
        match self.drbg.generate(out, &[]) {
            Ok(()) => Piece::Done,
            Err(_) => Piece::NotNow,
        }
    }

    /// Gives the generator its first state from what the root handed over and the processor's
    /// own draws in the seed area (R5), and empties both.
    pub fn instantiate_from_root(&mut self, area: &mut [u8], now: u64, tsc: u64) -> bool {
        let instantiated = self.instantiate_inner(area, tsc);
        wipe(&mut self.from_root);
        self.from_root_len = 0;
        wipe(area);
        self.forget_regions();
        if instantiated {
            self.seeded_at = now;
        }
        instantiated
    }

    fn instantiate_inner(&mut self, area: &mut [u8], tsc: u64) -> bool {
        if self.drbg.is_instantiated()
            || usize::from(self.from_root_len) != FROM_ROOT_LEN
            || area.len() < seed::AREA_LEN
        {
            return false;
        }
        // Draws of an instruction that has failed since they were collected are not used.
        self.settle_regions(area);
        let Some(head) = seed::write_processor_head(area, self.index, tsc) else {
            return false;
        };
        // Each full region is one instruction's 1,024 draws; region 1 is never full alone.
        let regions = if self.region0 != STARTUP_DRAWS {
            0usize
        } else if self.region1 != STARTUP_DRAWS {
            1
        } else {
            2
        };
        let start = seed::HEAD.saturating_sub(head);
        let end = seed::HEAD.saturating_add(regions.saturating_mul(seed::REGION));
        match (
            self.from_root.get(..FROM_ROOT_ENTROPY),
            self.from_root.get(FROM_ROOT_ENTROPY..),
            area.get(start..end),
        ) {
            (Some(entropy), Some(nonce), Some(personalization)) => self
                .drbg
                .instantiate(entropy, nonce, personalization)
                .is_ok(),
            _ => false,
        }
    }

    /// The standard's uninstantiate function for this processor: the generator's state and the
    /// store are overwritten, and the processor has no generator. Its instructions' tests stay
    /// as they are.
    pub fn uninstantiate(&mut self) {
        self.drbg.uninstantiate();
        self.clear_store();
        wipe(&mut self.from_root);
        self.from_root_len = 0;
    }

    /// For the tests: puts `which` into a start-up test that has passed `passed` draws.
    #[cfg(test)]
    pub(crate) fn begin_retest_for_tests(&mut self, which: Instruction, passed: u16) {
        let line = self.line_mut(which);
        line.health = Health::untested();
        line.health.begin_test();
        for n in 0..u64::from(passed) {
            let value = crate::tests::world::splitmix(0x7E57, n);
            assert_eq!(line.health.offer(value), Offer::Accepted);
        }
    }

    /// For the tests: `which` gives one value twice and fails its repetition count test.
    #[cfg(test)]
    pub(crate) fn fail_for_tests(&mut self, which: Instruction, now: u64) {
        for _ in 0..2 {
            if let Offer::Failed(_) = self.line_mut(which).health.offer(0x1357_9BDF_0246_8ACE) {
                self.failed(which, now);
            }
        }
    }

    /// For the tests: says which instruction each region's draws are of.
    #[cfg(test)]
    pub(crate) const fn set_regions_for_tests(
        &mut self,
        region0: Option<Instruction>,
        region1: Option<Instruction>,
    ) {
        self.region0_of = region0;
        self.region1_of = region1;
    }

    pub(crate) const fn told_healthy(&self, which: Instruction) -> bool {
        self.line(which).told_healthy
    }

    pub(crate) const fn set_told_healthy(&mut self, which: Instruction, healthy: bool) {
        self.line_mut(which).told_healthy = healthy;
    }
}

impl Default for Node {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for Node {
    /// Prints nothing of the state, the store or the tests (CLAUDE.md §61).
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Node { .. }")
    }
}
