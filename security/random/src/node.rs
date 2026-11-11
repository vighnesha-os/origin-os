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
    /// A test failed: the instruction is unhealthy and what it had given is thrown away.
    Failed,
    /// Nothing wanted the try, so the instruction was not run.
    Unwanted,
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

/// One instruction's tests and its schedule.
struct Line {
    health: Health,
    /// Not tried before this time.
    rest_until: u64,
    /// No new start-up test before this time.
    retest_after: u64,
    /// What the root has been told about its health.
    told_healthy: bool,
}

impl Line {
    const fn new() -> Self {
        Self {
            health: Health::absent(),
            rest_until: 0,
            retest_after: 0,
            told_healthy: false,
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
    /// What the root handed over.
    pub(crate) from_root: [u8; FROM_ROOT_LEN],
    /// How much of it is there.
    pub(crate) from_root_len: u8,
    /// Draws collected into each region of the seed area.
    pub(crate) region0: u16,
    pub(crate) region1: u16,
    /// When the generator was last seeded.
    pub(crate) seeded_at: u64,
    /// The root is not asked for a reseed before this time.
    root_rest_until: u64,
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
            from_root: [0; FROM_ROOT_LEN],
            from_root_len: 0,
            region0: 0,
            region1: 0,
            seeded_at: 0,
            root_rest_until: 0,
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
                || now.saturating_sub(self.seeded_at) >= RESEED_NANOS)
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

    /// Throws away what `which` has given and not yet been used for (R4 item 8).
    fn drop_draws(&mut self, which: Instruction) {
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
    fn draw(
        &mut self,
        source: &mut dyn Source,
        which: Instruction,
        now: u64,
    ) -> Result<(u64, bool), Try> {
        let before = self.line(which).health.condition();
        if !matches!(before, Condition::Testing | Condition::Healthy) {
            return Err(Try::Unwanted);
        }
        let Some(value) = source.try_once(which) else {
            return Err(Try::NotReady);
        };
        match self.line_mut(which).health.offer(value) {
            Offer::Accepted => {
                let completed =
                    matches!(before, Condition::Testing) && self.line(which).health.is_healthy();
                Ok((value, completed))
            }
            Offer::SetAside => Err(Try::SetAside),
            Offer::Failed(_) => {
                self.failed(which, now);
                Err(Try::Failed)
            }
        }
    }

    /// What follows any failure of `which`: its draws go, and it is not tested again for one
    /// reseed period.
    fn failed(&mut self, which: Instruction, now: u64) {
        self.drop_draws(which);
        self.line_mut(which).retest_after = now.saturating_add(RESEED_NANOS);
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
        let (value, completed) = match self.draw(source, which, now) {
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
    }

    /// Records that a draw of `which` used all its tries without a value.
    ///
    /// If any try gave zero or all ones, the instruction is stuck: that is a failure (R4 item
    /// 2; a mixture of "not ready" and stuck values is held to be one too, since a working
    /// instruction gives such a value about once in 2^63 tries). Otherwise it was "not now":
    /// the instruction rests for [`REST_NANOS`] (S3). Returns whether it was a failure.
    pub fn gave_up(&mut self, which: Instruction, saw_set_aside: bool, now: u64) -> bool {
        if saw_set_aside {
            if matches!(self.line_mut(which).health.stuck(), Offer::Failed(_)) {
                self.failed(which, now);
            }
            return true;
        }
        self.line_mut(which).rest_until = now.saturating_add(REST_NANOS);
        match which {
            Instruction::Rdseed => {
                self.counts.not_now_rdseed = self.counts.not_now_rdseed.saturating_add(1);
            }
            Instruction::Rdrand => {
                self.counts.not_now_rdrand = self.counts.not_now_rdrand.saturating_add(1);
            }
        }
        false
    }

    /// Gives up a start-up test that could not be finished while the processor starts: the
    /// instruction is unhealthy, and a new test may begin when a reseed next falls due.
    pub fn abandon_test(&mut self, which: Instruction) {
        self.line_mut(which).health.abandon_test();
        self.drop_draws(which);
        self.line_mut(which).retest_after = 0;
    }

    /// One try of `which` for the store. Draws of the other instruction that are in the way
    /// are moved aside (RDSEED's, which then go into the reseed as additional input) or dropped.
    fn gather_try(&mut self, source: &mut dyn Source, which: Instruction, now: u64) -> Try {
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
        let (value, completed) = match self.draw(source, which, now) {
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
    fn gather_next(&mut self, source: &mut dyn Source, now: u64) -> Option<(Instruction, Try)> {
        let rdseed = self.rdseed.health.is_healthy() && now >= self.rdseed.rest_until;
        let rdrand = self.rdrand.health.is_healthy() && now >= self.rdrand.rest_until;
        let which = if rdseed && (self.have == 0 || self.from == Instruction::Rdseed) {
            Instruction::Rdseed
        } else if rdrand {
            Instruction::Rdrand
        } else {
            return None;
        };
        Some((which, self.gather_try(source, which, now)))
    }

    /// Whether the store holds a whole reseed's draws.
    pub const fn store_full(&self) -> bool {
        self.have >= RESEED_DRAWS
    }

    /// One try towards a full store, whether or not a reseed is due: for the root's own reseed
    /// (R6), which the processor that next uses the root makes.
    pub fn gather_for_root(&mut self, source: &mut dyn Source, now: u64) -> Maint {
        if self.store_full() {
            return Maint::Idle;
        }
        match self.gather_next(source, now) {
            Some((which, outcome)) => Maint::Tried {
                on: self.index,
                which,
                purpose: Purpose::Reseed,
                outcome,
            },
            None => Maint::Delayed,
        }
    }

    /// One try of RDSEED towards a full store of RDSEED draws, for a request with prediction
    /// resistance (R6). `None` where RDSEED is not healthy or is resting.
    pub fn fresh_try(&mut self, source: &mut dyn Source, now: u64) -> Option<Try> {
        if !self.rdseed.health.is_healthy() || now < self.rdseed.rest_until {
            return None;
        }
        if self.store_full() && self.from == Instruction::Rdseed {
            return Some(Try::Unwanted);
        }
        Some(self.gather_try(source, Instruction::Rdseed, now))
    }

    /// Whether the store holds what a request with prediction resistance needs.
    pub(crate) const fn store_is_fresh(&self) -> bool {
        self.store_full() && matches!(self.from, Instruction::Rdseed)
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

    /// Records that the root could not be asked for a reseed: it is left alone for
    /// [`REST_NANOS`], and the generator goes on with the seed it has (R6).
    pub fn root_refused(&mut self, now: u64) {
        self.root_rest_until = now.saturating_add(REST_NANOS);
    }

    /// The next piece of maintenance, as one step: one try towards a start-up test that is
    /// being run again, or one try for a reseed that is due, or the reseed itself.
    ///
    /// A start-up test of an unhealthy instruction begins when a reseed falls due, at most once
    /// in a reseed period, and goes on over later requests until it passes or fails (S2).
    pub fn maintain(&mut self, source: &mut dyn Source, now: u64, tsc: u64, allow: Allow) -> Maint {
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
                && now >= line.retest_after
            {
                self.line_mut(which).health.begin_test();
                self.counts.retests = self.counts.retests.saturating_add(1);
                testing = true;
            }
            if testing && now >= self.line(which).rest_until {
                let outcome = match self.draw(source, which, now) {
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
            return Maint::Idle;
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
        if let Some((which, outcome)) = self.gather_next(source, now) {
            return Maint::Tried {
                on: self.index,
                which,
                purpose: Purpose::Reseed,
                outcome,
            };
        }
        if !self.rdseed.health.is_healthy() && !self.rdrand.health.is_healthy() {
            if now >= self.root_rest_until {
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
    ) -> Piece {
        if !self.drbg.is_instantiated() {
            return Piece::NoGenerator;
        }
        if out.len() > PIECE {
            return Piece::NotNow;
        }
        if let Some(allow) = maintain
            && (self.due(now) || self.retesting() || self.owes_root())
        {
            return Piece::Due(self.maintain(source, now, tsc, allow));
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
        self.region0 = 0;
        self.region1 = 0;
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
