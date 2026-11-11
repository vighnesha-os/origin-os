//! A randomized model of the policy (CLAUDE.md §72, "model-based testing"; TASKS 18b).
//!
//! The other tests here script one situation each. Two independent reviews of the policy, on
//! 2026-11-11, found defects that none of them could show: each needed requests whose steps
//! interleave, a thread that changes processor between two steps, or an instruction that goes
//! dry in the middle of a gathering. This file makes such orders by the thousand, from fixed
//! seeds, and holds every seed the policy makes against rules that must hold whatever the
//! order. It began as the machine the second review ran and the checks the coordinating
//! session added to it; `docs/engineering/SOURCE_PROVENANCE.md` says what was changed.
//!
//! **The machine.** [`Machine`] is a [`Platform`] like `world`'s: up to four processors, each
//! reporting RDSEED, RDRAND, both or neither, a root, and a generator on every processor. It
//! differs in what happens around a step:
//!
//! - *Weather* ([`Weather`]). Once the machine has started, an instruction may be not ready,
//!   go dry for up to 399 tries, give zero or all ones with success reported, or give its last
//!   value again.
//! - *One thread of control* ([`Chaos`]). After a step another request may run from beginning
//!   to end, up to three deep: an interrupt, or another thread on that processor. The thread
//!   may be on another processor afterwards. The root's lock may be found taken. The clock
//!   moves at every step and now and then steps back.
//! - *Several threads* ([`Thread`]). Each thread makes its requests in turns of one step, and
//!   the machine's random numbers say whose turn is next: the steps of requests that are in
//!   flight together interleave one by one, which nesting cannot do.
//!
//! **The log.** Apart from the zeros, all-ones and repeats that the weather has it give,
//! every value an instruction gives is new. It is logged with its logical time (the tries
//! made so far), its processor, its instruction, the request that was running, and the number
//! the processor's store was lent under when that step ended. The log keeps a draw for as
//! long as a store holds it, so it stays small however long the run is.
//!
//! **The rules.** A step that made a reseed shows in the counters. The processor's store *as
//! it was before that step* is what the reseed consumed, and it is held against these:
//!
//! - Any seed: eight draws; each one a draw the store was keeping; none in two seeds; none
//!   from before the last failure of its instruction; all of the processor the step ran on;
//!   the eight that are counted, of the instruction the store names; any set aside, RDSEED's.
//!   The store is empty afterwards: no draw, and no byte of one.
//! - A processor's own reseed: the store is not lent, and every draw was made in a step that
//!   left it not lent.
//! - The root's reseed: the store is lent, and every draw was made under that same number, by
//!   the request that makes the reseed, after that request began.
//! - A piece with prediction resistance: the same, inside a request with prediction
//!   resistance, and from RDSEED draws only.
//!
//! And these, at other moments:
//!
//! - After every step: an instruction's failure or "not now" is counted only by a step that
//!   tried that instruction.
//! - At every try: a plain request tries instructions on one processor only.
//! - When a request ends: a plain request was answered, and made at most 16 draws (S2). An
//!   answer holds no 16 bytes that are still as the caller left them, or zero, and its first
//!   eight bytes are no earlier answer's. A refused request's buffer is all zeros.
//! - With instructions that always deliver, no lock taken and no move: after a plain request
//!   on a processor with a healthy instruction, the root's seed is younger than a reseed
//!   period and 5 ms.
//! - In dry weather, at length: at least one reseed of the root is made from RDRAND, with the
//!   RDSEED draws gathered before as additional input (S3).
//! - Every configuration makes seeds, and does what it is there for: requests nest, threads
//!   move. A run in which nothing happens keeps every rule.
//!
//! **What it does not show.**
//!
//! - It runs the policy crate on a scripted platform, on the build host. The Framekernel's
//!   wiring (interrupts off, the generator's stack and its overwrite, the root's lock), the
//!   loader's generator and a real processor's instructions are not in it.
//! - A step is one indivisible thing here, as the platform's contract has it. A request made
//!   inside a running step, which the policy refuses, is not made; nor is anything that could
//!   interrupt a step.
//! - The machine starts in calm weather: start-up tests and first seeds are `start`'s tests'.
//!   No processor goes offline.
//! - It is randomized, not exhaustive. The seeds are fixed, so every run makes the same
//!   orders, and a run that finds nothing says nothing about the orders it did not make.
//! - It follows draws into seeds. It does not judge the output, beyond an answer coming
//!   twice. Of what stays in memory it sees a store after a reseed and a refused buffer.
//! - The threads take turns: no two steps run at once, so it finds no data race. The policy
//!   shares nothing between processors except through the platform.
//!
//! **Running it.** Every configuration is two tests. The short one runs with the crate's
//! others. The one whose name ends in `at_length` is marked `#[ignore]`: it is the run that
//! was made on 2026-11-11, seed for seed.
//!
//! ```text
//! cargo test --package origin-random                        the short runs
//! cargo test --package origin-random -- --ignored           the runs at length
//! cargo test --package origin-random model -- --nocapture   what each configuration did
//! ```
//!
//! A failure names the configuration, the machine's seed, the request during which the first
//! violation was seen, and the violations' texts. The same seed makes the same run, and a
//! short run is the beginning of the run at length.
//!
//! **Negative controls.** `tools/plant_faults.py` puts defects back one at a time; a test
//! here fails for many of them.

use std::collections::{HashMap, HashSet};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;

use super::world::splitmix;
use crate::{
    Busy, Ctx, Error, Instruction, Node, Platform, Root, RootCounts, Source, fill, fill_fresh,
    seed, start_processor, start_root,
};

use Instruction::{Rdrand, Rdseed};

/// The seed of the sequence the instructions' values come from.
const VALUES: u64 = 0xF00D_F00D;
/// The draws that make one reseed (R6). This and the next two are the rules' figures as the
/// model states them, not the crate's constants: a constant that is changed breaks a rule.
const SEED_DRAWS: u8 = 8;
/// The most draws a plain request makes (TASKS 18b S2).
const REQUEST_DRAWS: u32 = 16;
/// A reseed period, in nanoseconds (R6).
const PERIOD: u64 = 60_000_000_000;
/// What a request's buffer holds before the request is made.
const UNWRITTEN: u8 = 0xEE;
/// The most violations a machine keeps.
const KEPT: usize = 40;

/// The model's random numbers: [`splitmix`]'s sequence for a seed, in order.
struct Rng {
    seed: u64,
    taken: u64,
}

impl Rng {
    const fn new(seed: u64) -> Self {
        Self { seed, taken: 0 }
    }

    fn number(&mut self) -> u64 {
        let number = splitmix(self.seed, self.taken);
        self.taken += 1;
        number
    }

    /// A number below `n`.
    fn below(&mut self, n: u64) -> u64 {
        self.number() % n
    }

    /// An index into something of `len` entries.
    fn index(&mut self, len: usize) -> usize {
        self.below(len as u64) as usize
    }

    /// True `per_mille` times in a thousand.
    fn chance(&mut self, per_mille: u64) -> bool {
        self.below(1000) < per_mille
    }
}

/// Where an instruction's figures are kept in a pair.
const fn slot(which: Instruction) -> usize {
    match which {
        Rdseed => 0,
        Rdrand => 1,
    }
}

/// What a request asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// [`fill`].
    Plain,
    /// [`fill_fresh`]: with prediction resistance.
    Fresh,
}

impl Kind {
    const fn name(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Fresh => "fresh",
        }
    }
}

/// A request in flight.
#[derive(Debug)]
struct Frame {
    kind: Kind,
    /// Its number: no two requests of a machine have the same.
    id: u64,
    /// The logical time at which it began.
    began: u64,
    /// Values the instructions have given it.
    draws: u32,
    /// The processor its first try was made on.
    tried_on: Option<usize>,
}

/// What a reseed is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seed {
    /// A processor's generator, from its own store.
    Own,
    /// The root, on its schedule.
    Root,
    /// The root, for a piece with prediction resistance.
    Fresh,
}

impl Seed {
    const fn name(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::Root => "root",
            Self::Fresh => "fresh",
        }
    }
}

/// What the log keeps of one value an instruction gave.
#[derive(Clone, Copy)]
struct Drawn {
    /// The logical time: the tries the machine had made when this one gave the value.
    time: u64,
    cpu: usize,
    which: Instruction,
    /// The request that was running, if one was.
    request: Option<u64>,
    /// What the processor's store was lent under when the step that made the draw ended.
    lent: Option<u64>,
}

/// What a machine did, in counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Tally {
    plain_answered: u64,
    plain_refused: u64,
    fresh_answered: u64,
    fresh_refused: u64,
    /// Reseeds of a processor's generator from its own store.
    own_reseeds: u64,
    /// Reseeds of the root on its schedule.
    root_reseeds: u64,
    /// Of those, the ones made from RDRAND with RDSEED draws set aside.
    root_fallbacks: u64,
    /// Pieces with prediction resistance.
    fresh_pieces: u64,
    /// Failures of an instruction's tests.
    failures: u64,
    /// Requests that ran between two steps of another.
    nested: u64,
    /// Times a thread was on another processor after a step.
    moves: u64,
    /// The most draws one plain request made.
    most_plain_draws: u32,
    /// Draws in seeds that were held against the rules.
    draws_checked: u64,
    /// Of those, the draws of the root's scheduled reseeds.
    root_draws_checked: u64,
}

impl Tally {
    fn add(&mut self, other: &Self) {
        self.plain_answered += other.plain_answered;
        self.plain_refused += other.plain_refused;
        self.fresh_answered += other.fresh_answered;
        self.fresh_refused += other.fresh_refused;
        self.own_reseeds += other.own_reseeds;
        self.root_reseeds += other.root_reseeds;
        self.root_fallbacks += other.root_fallbacks;
        self.fresh_pieces += other.fresh_pieces;
        self.failures += other.failures;
        self.nested += other.nested;
        self.moves += other.moves;
        self.most_plain_draws = self.most_plain_draws.max(other.most_plain_draws);
        self.draws_checked += other.draws_checked;
        self.root_draws_checked += other.root_draws_checked;
    }
}

/// Everything the model remembers of a machine's run.
struct Log {
    /// Tries made so far: the logical time.
    time: u64,
    /// Values given so far.
    given: u64,
    /// The values that are in a processor's store now. A draw that no store kept, and one
    /// that a store no longer holds, cannot be taken by any reseed: the log drops it, and
    /// stays small however long the run is.
    drawn: HashMap<u64, Drawn>,
    /// The values that went into a seed, and what the seed was of.
    used: HashMap<u64, Seed>,
    /// The logical time of the last failure of each processor's instructions; 0 for none.
    failed_at: Vec<[u64; 2]>,
    /// The requests in flight on the thread of control that is running, innermost last.
    frames: Vec<Frame>,
    /// Requests begun so far.
    requests: u64,
    /// The values the step that is running has been given.
    step_values: Vec<u64>,
    /// Its tries of each instruction.
    step_tries: [u32; 2],
    violations: Vec<String>,
    /// The request during which the first violation was seen.
    first_violation: Option<u64>,
    tally: Tally,
}

impl Log {
    fn violation(&mut self, text: String) {
        self.first_violation.get_or_insert(self.requests);
        if self.violations.len() < KEPT {
            self.violations.push(text);
        }
    }

    /// A new request in flight.
    fn open(&mut self, kind: Kind) -> Frame {
        self.requests += 1;
        Frame {
            kind,
            id: self.requests,
            began: self.time,
            draws: 0,
            tried_on: None,
        }
    }
}

/// How the instructions misbehave once the machine has started. Each figure is a chance at
/// one try. RDSEED is the one that runs dry: for RDRAND the first two are a tenth of what
/// they say.
#[derive(Clone, Copy, Debug)]
struct Weather {
    /// In a thousand: the instruction goes dry, for up to 399 tries.
    dry: u64,
    /// In a thousand: it is not ready, this once.
    not_ready: u64,
    /// In a hundred thousand: it gives zero or all ones and reports success.
    stuck: u64,
    /// In a hundred thousand: it gives the value it gave last.
    repeat: u64,
}

/// Something of everything, now and then.
const ROUGH: Weather = Weather {
    dry: 2,
    not_ready: 20,
    stuck: 30,
    repeat: 6,
};
/// The same, with RDSEED dry fifteen times as often.
const DRY: Weather = Weather { dry: 30, ..ROUGH };
/// Not ready now and then, and nothing else.
const FAIR: Weather = Weather {
    dry: 0,
    not_ready: 2,
    stuck: 0,
    repeat: 0,
};
/// Instructions that never fail.
const CLEAR: Weather = Weather {
    not_ready: 0,
    ..FAIR
};

/// One instruction of one processor, as the model runs it.
struct Line {
    reported: bool,
    /// The value it gave last; zero before the first.
    last: u64,
    /// Tries for which it stays dry.
    dry: u64,
}

/// One processor's instructions, for the length of one step.
struct Lines<'a> {
    cpu: usize,
    lines: &'a mut [Line; 2],
    weather: Option<Weather>,
    rng: &'a mut Rng,
    log: &'a mut Log,
}

impl Source for Lines<'_> {
    fn reports(&self, which: Instruction) -> bool {
        self.lines[slot(which)].reported
    }

    fn try_once(&mut self, which: Instruction) -> Option<u64> {
        let log = &mut *self.log;
        log.time += 1;
        log.step_tries[slot(which)] += 1;
        let cpu = self.cpu;
        let request = log
            .frames
            .last_mut()
            .map(|frame| (frame.kind, *frame.tried_on.get_or_insert(cpu)));
        if let Some((Kind::Plain, first)) = request
            && first != cpu
        {
            log.violation(format!(
                "plain: a try on processor {cpu} in a request whose tries began on {first}"
            ));
        }
        let line = &mut self.lines[slot(which)];
        if let Some(weather) = self.weather {
            if line.dry > 0 {
                line.dry -= 1;
                return None;
            }
            let scale = match which {
                Rdseed => 1,
                Rdrand => 10,
            };
            if self.rng.below(1000 * scale) < weather.dry {
                line.dry = self.rng.below(400);
                return None;
            }
            if self.rng.below(1000 * scale) < weather.not_ready {
                return None;
            }
            if self.rng.below(100_000) < weather.stuck {
                return Some(if self.rng.chance(500) { 0 } else { u64::MAX });
            }
            if self.rng.below(100_000) < weather.repeat && line.last != 0 {
                return Some(line.last);
            }
        }
        log.given += 1;
        let value = splitmix(VALUES, log.given);
        line.last = value;
        log.drawn.insert(
            value,
            Drawn {
                time: log.time,
                cpu,
                which,
                request: log.frames.last().map(|frame| frame.id),
                lent: None,
            },
        );
        log.step_values.push(value);
        if let Some(frame) = log.frames.last_mut() {
            frame.draws += 1;
        }
        Some(value)
    }
}

/// A processor's store and counts, as a step finds them or leaves them.
struct Snapshot {
    /// The draws in the store: first the `counted` ones that a reseed counts, then the
    /// `aside` RDSEED draws that were set aside when RDRAND took over. Zero after those.
    draws: [u64; 16],
    counted: u8,
    aside: u8,
    /// Whether every byte of the store is zero.
    blank: bool,
    /// The instruction the counted draws are of.
    from: Instruction,
    /// The number the store is lent under.
    lent: Option<u64>,
    own_reseeds: u32,
    failures: [u32; 2],
    not_now: [u32; 2],
}

impl Snapshot {
    fn of(node: &Node) -> Self {
        let counts = node.counts();
        // The store has room for eight draws and for eight set aside, after the 16 bytes
        // that a reseed stamps.
        let counted = node.have.min(8);
        let aside = node.aside_have.min(8);
        let mut draws = [0; 16];
        let mut held = 0;
        for bytes in &node.entropy.as_chunks::<8>().0[..usize::from(counted)] {
            draws[held] = u64::from_le_bytes(*bytes);
            held += 1;
        }
        for bytes in &node.aside[16..].as_chunks::<8>().0[..usize::from(aside)] {
            draws[held] = u64::from_le_bytes(*bytes);
            held += 1;
        }
        Self {
            draws,
            counted,
            aside,
            blank: node.entropy == [0; 64] && node.aside == [0; 80],
            from: node.from,
            lent: node.lent,
            own_reseeds: counts.reseeds_rdseed + counts.reseeds_rdrand,
            failures: [node.faults(Rdseed), node.faults(Rdrand)],
            not_now: [counts.not_now_rdseed, counts.not_now_rdrand],
        }
    }

    /// The draws the store holds, the counted ones first.
    fn held(&self) -> &[u64] {
        &self.draws[..usize::from(self.counted) + usize::from(self.aside)]
    }

    fn holds(&self, value: u64) -> bool {
        self.held().contains(&value)
    }

    /// Whether the store holds no draw and no byte of one.
    fn is_empty(&self) -> bool {
        self.counted == 0 && self.aside == 0 && self.blank
    }
}

/// What happens between the steps of a request on one thread of control.
#[derive(Clone, Copy, Debug)]
struct Chaos {
    /// In a thousand, after a step: another request runs from beginning to end.
    nest: u64,
    /// How deep requests nest.
    depth: u32,
    /// In a thousand, after a step: the thread is on a processor chosen anew.
    moves: u64,
    /// In a thousand, at a step under the root's lock: the lock is found taken.
    root_busy: u64,
    /// In a hundred thousand, after a step: the clock steps back by up to three seconds.
    clock_back: u64,
}

impl Chaos {
    /// Requests that nest and threads that move this often, with the lock taken and the
    /// clock stepping back now and then.
    const fn of(nest: u64, moves: u64) -> Self {
        Self {
            nest,
            depth: 3,
            moves,
            root_busy: 30,
            clock_back: 20,
        }
    }
}

/// The machine.
struct Machine {
    /// The seed of its random numbers: with its configuration, all that its run depends on.
    seed: u64,
    nodes: Vec<Node>,
    areas: Vec<Vec<u8>>,
    lines: Vec<[Line; 2]>,
    root: Root,
    /// The processor the thread of control is on.
    on: usize,
    now: u64,
    rng: Rng,
    log: Log,
    /// `None` while the machine starts: every try then gives a value.
    weather: Option<Weather>,
    chaos: Option<Chaos>,
    /// How deep the request that is running is nested.
    depth: u32,
    /// Whether each request in flight came with interrupts off, innermost last.
    quick: Vec<bool>,
    /// The first eight bytes of every answer.
    outputs: HashSet<u64>,
}

impl Machine {
    /// A machine that has not started. `reports` says of each processor whether it reports
    /// RDSEED and RDRAND.
    fn new(seed: u64, reports: &[(bool, bool)]) -> Self {
        let line = |reported| Line {
            reported,
            last: 0,
            dry: 0,
        };
        Self {
            seed,
            nodes: reports.iter().map(|_| Node::new()).collect(),
            areas: reports.iter().map(|_| vec![0; seed::AREA_LEN]).collect(),
            lines: reports
                .iter()
                .map(|&(rdseed, rdrand)| [line(rdseed), line(rdrand)])
                .collect(),
            root: Root::new(),
            on: 0,
            now: 1_000_000,
            rng: Rng::new(seed),
            log: Log {
                time: 0,
                given: 0,
                drawn: HashMap::new(),
                used: HashMap::new(),
                failed_at: vec![[0; 2]; reports.len()],
                frames: Vec::new(),
                requests: 0,
                step_values: Vec::new(),
                step_tries: [0; 2],
                violations: Vec::new(),
                first_violation: None,
                tally: Tally::default(),
            },
            weather: None,
            chaos: None,
            depth: 0,
            quick: Vec::new(),
            outputs: HashSet::new(),
        }
    }

    /// A machine whose root and processors have started, in calm weather. The first processor
    /// starts the root.
    fn started(seed: u64, reports: &[(bool, bool)]) -> Self {
        let mut machine = Self::new(seed, reports);
        assert!(
            start_root(&mut machine, false).instantiated,
            "seed {seed}: the root did not start"
        );
        for cpu in 0..reports.len() {
            machine.on = cpu;
            assert!(
                start_processor(&mut machine).generator,
                "seed {seed}: processor {cpu} got no generator"
            );
        }
        machine
    }

    /// Runs one step on the processor the thread of control is on.
    fn run_step(&mut self, step: impl FnOnce(&mut Ctx<'_>, &mut Root)) {
        let on = self.on;
        let mut source = Lines {
            cpu: on,
            lines: &mut self.lines[on],
            weather: self.weather,
            rng: &mut self.rng,
            log: &mut self.log,
        };
        let mut ctx = Ctx {
            node: &mut self.nodes[on],
            source: &mut source,
            area: &mut self.areas[on],
            index: on as u32,
            now: self.now,
            tsc: 0x1234_5678_9ABC_DEF0 ^ self.now,
        };
        step(&mut ctx, &mut self.root);
    }

    /// Holds what a reseed took against the rules. `before` is processor `on` as the step
    /// that made the reseed found it: its store then is what the reseed consumed.
    fn consume(&mut self, on: usize, before: &Snapshot, seed: Seed) {
        let what = seed.name();
        let log = &mut self.log;
        match seed {
            Seed::Own => log.tally.own_reseeds += 1,
            Seed::Root => log.tally.root_reseeds += 1,
            Seed::Fresh => log.tally.fresh_pieces += 1,
        }
        if seed == Seed::Root && before.from == Rdrand && before.aside > 0 {
            log.tally.root_fallbacks += 1;
        }
        if before.counted != SEED_DRAWS {
            log.violation(format!("{what}: the store held {} draws", before.counted));
        }
        let request = log
            .frames
            .last()
            .map(|frame| (frame.id, frame.kind, frame.began));
        for (n, &value) in before.held().iter().enumerate() {
            let counted = n < usize::from(before.counted);
            let earlier = log.used.insert(value, seed);
            if let Some(earlier) = earlier {
                log.violation(format!(
                    "a draw went into two seeds: {}, then {what}",
                    earlier.name()
                ));
            }
            let Some(drawn) = log.drawn.get(&value).copied() else {
                if earlier.is_none() {
                    log.violation(format!(
                        "{what}: a value in the seed is no draw that the store was keeping"
                    ));
                }
                continue;
            };
            // The lending: a processor's own reseed takes nothing that was gathered for the
            // root, and the root takes only what was gathered under the number it is shown.
            let lent_right = match seed {
                Seed::Own => before.lent.is_none() && drawn.lent.is_none(),
                Seed::Root | Seed::Fresh => before.lent.is_some() && drawn.lent == before.lent,
            };
            log.tally.draws_checked += 1;
            if !lent_right {
                log.violation(format!(
                    "{what}: a draw made under {} is used from a store that is {}",
                    if drawn.lent.is_some() {
                        "a lending"
                    } else {
                        "no lending"
                    },
                    match (before.lent.is_some(), drawn.lent == before.lent) {
                        (false, _) => "not lent",
                        (true, false) => "lent under another number",
                        (true, true) => "lent under the same number",
                    },
                ));
            }
            if seed != Seed::Own && drawn.request != request.map(|(id, ..)| id) {
                log.violation(format!("{what}: a draw that another request made"));
            }
            if seed == Seed::Root
                && let Some((_, _, began)) = request
            {
                log.tally.root_draws_checked += 1;
                if drawn.time <= began {
                    log.violation(
                        "root: a draw made before the request that reseeds the root began"
                            .to_string(),
                    );
                }
            }
            if drawn.time <= log.failed_at[drawn.cpu][slot(drawn.which)] {
                log.violation(format!(
                    "{what}: a draw of an instruction that failed since"
                ));
            }
            if drawn.cpu != on {
                log.violation(format!(
                    "{what}: a draw of processor {} used on {on}",
                    drawn.cpu
                ));
            }
            if !counted {
                if drawn.which != Rdseed {
                    log.violation(format!("{what}: an RDRAND draw was set aside"));
                }
                continue;
            }
            if drawn.which != before.from {
                log.violation(format!("{what}: the store's instruction is not the draw's"));
            }
            if seed == Seed::Fresh {
                match request {
                    Some((_, Kind::Fresh, began)) => {
                        if drawn.time <= began {
                            log.violation(format!(
                                "fresh: a draw made at {} for a request that began at {began}",
                                drawn.time
                            ));
                        }
                    }
                    Some((_, Kind::Plain, _)) => {
                        log.violation("fresh: a piece inside a plain request".to_string());
                    }
                    None => log.violation("fresh: a piece outside any request".to_string()),
                }
                if drawn.which != Rdseed {
                    log.violation("fresh: an RDRAND draw".to_string());
                }
            }
        }
    }

    /// What follows every step. `before` is processor `on` as the step found it, and
    /// `root_before` the root's counts if the step ran under its lock.
    fn step_ended(&mut self, on: usize, before: &Snapshot, root_before: Option<RootCounts>) {
        let after = Snapshot::of(&self.nodes[on]);
        let log = &mut self.log;
        // A draw of this step that the store holds now learns what the store is lent under.
        // One that it does not hold was not kept.
        for value in log.step_values.drain(..) {
            if !after.holds(value) {
                log.drawn.remove(&value);
            } else if let Some(drawn) = log.drawn.get_mut(&value) {
                drawn.lent = after.lent;
            }
        }
        let tries = std::mem::take(&mut log.step_tries);
        for which in Instruction::BOTH {
            let n = slot(which);
            let failed = after.failures[n] > before.failures[n];
            if tries[n] == 0 && (failed || after.not_now[n] > before.not_now[n]) {
                log.violation(
                    "an instruction was failed or rested in a step that did not try it".to_string(),
                );
            }
            if failed {
                log.tally.failures += 1;
                log.failed_at[on][n] = log.time;
            }
        }
        // The counters say whether the step made a reseed, and of what.
        let own = (after.own_reseeds > before.own_reseeds).then_some(Seed::Own);
        let root = root_before.and_then(|was| {
            let root = self.root.counts();
            (root.reseeds > was.reseeds).then_some(if root.fresh > was.fresh {
                Seed::Fresh
            } else {
                Seed::Root
            })
        });
        for seed in [own, root].into_iter().flatten() {
            self.consume(on, before, seed);
            if !after.is_empty() {
                self.log.violation(format!(
                    "{}: the store was not emptied after the reseed took it",
                    seed.name()
                ));
            }
        }
        // What the store held and holds no more is nobody's from here on.
        if before.draws != after.draws {
            for value in before.held() {
                if !after.holds(*value) {
                    self.log.drawn.remove(value);
                }
            }
        }
        self.now += 200 + self.rng.below(2000);
        self.interleave();
    }

    /// What may happen between two steps on one thread of control: the clock steps back,
    /// another request runs from beginning to end, the thread moves.
    fn interleave(&mut self) {
        let Some(chaos) = self.chaos else {
            return;
        };
        if self.rng.below(100_000) < chaos.clock_back {
            self.now = self.now.saturating_sub(self.rng.below(3_000_000_000));
        }
        if self.depth < chaos.depth && self.rng.chance(chaos.nest) {
            self.log.tally.nested += 1;
            let outer = self.on;
            // Mostly on the same processor: an interrupt, or another thread there.
            if self.rng.chance(200) {
                self.on = self.rng.index(self.nodes.len());
            }
            let quick = self.rng.chance(300);
            self.request(quick);
            self.on = outer;
        }
        if self.rng.chance(chaos.moves) {
            self.log.tally.moves += 1;
            self.on = self.rng.index(self.nodes.len());
        }
    }

    /// One request on this thread of control, from beginning to end.
    fn request(&mut self, quick: bool) {
        self.depth += 1;
        self.quick.push(quick);
        let (kind, len) = shape(&mut self.rng, 300, 400);
        let frame = self.log.open(kind);
        self.log.frames.push(frame);
        let mut out = vec![UNWRITTEN; len];
        let result = match kind {
            Kind::Plain => fill(self, &mut out),
            Kind::Fresh => fill_fresh(self, &mut out),
        };
        let frame = self.log.frames.pop();
        self.quick.pop();
        self.depth -= 1;
        match frame {
            Some(frame) => self.judge(&frame, &out, result),
            None => self.log.violation("the model lost a request".to_string()),
        }
    }

    /// Holds the end of a request against the rules for what a caller is given.
    fn judge(&mut self, frame: &Frame, out: &[u8], result: Result<(), Error>) {
        let log = &mut self.log;
        let kind = frame.kind;
        let what = kind.name();
        let tally = &mut log.tally;
        match (kind, result.is_ok()) {
            (Kind::Plain, true) => tally.plain_answered += 1,
            (Kind::Plain, false) => tally.plain_refused += 1,
            (Kind::Fresh, true) => tally.fresh_answered += 1,
            (Kind::Fresh, false) => tally.fresh_refused += 1,
        }
        if result.is_ok() {
            let left = out
                .as_chunks::<16>()
                .0
                .iter()
                .any(|chunk| *chunk == [0; 16] || *chunk == [UNWRITTEN; 16]);
            if left {
                log.violation(format!(
                    "{what}: an answer of {} bytes with 16 that are no answer",
                    out.len()
                ));
            }
            if let Some(first) = out.first_chunk::<8>()
                && !self.outputs.insert(u64::from_le_bytes(*first))
            {
                log.violation(format!("{what}: an output came twice"));
            }
        } else if out.iter().any(|&byte| byte != 0) {
            log.violation(format!("{what}: a refused request left bytes"));
        }
        if kind == Kind::Plain {
            log.tally.most_plain_draws = log.tally.most_plain_draws.max(frame.draws);
            if frame.draws > REQUEST_DRAWS {
                log.violation(format!("plain: a request made {} draws", frame.draws));
            }
            // Every processor here has a generator and no step runs inside another, so
            // nothing refuses a plain request: a reseed that cannot be made is delayed (T5).
            if result.is_err() {
                log.violation("plain: a request was refused".to_string());
            }
        }
    }
}

impl Platform for Machine {
    fn step(&mut self, step: &mut dyn FnMut(&mut Ctx<'_>)) -> Result<(), Busy> {
        let on = self.on;
        let before = Snapshot::of(&self.nodes[on]);
        self.run_step(|ctx, _| step(ctx));
        self.step_ended(on, &before, None);
        Ok(())
    }

    fn root_step(&mut self, step: &mut dyn FnMut(&mut Ctx<'_>, &mut Root)) -> Result<(), Busy> {
        if let Some(chaos) = self.chaos
            && self.rng.chance(chaos.root_busy)
        {
            // Another processor holds the lock, and things go on meanwhile.
            self.now += 500;
            self.interleave();
            return Err(Busy::Root);
        }
        let on = self.on;
        let before = Snapshot::of(&self.nodes[on]);
        let root_before = self.root.counts();
        self.run_step(|ctx, root| step(ctx, root));
        self.step_ended(on, &before, Some(root_before));
        Ok(())
    }

    fn pause(&mut self) {
        self.now += 40;
    }

    fn quick(&self) -> bool {
        self.quick.last().copied().unwrap_or(false)
    }
}

/// What a request asks for: prediction resistance `fresh` times in a thousand, and 0, 8, 64
/// or 128 bytes or any number up to `longest`.
fn shape(rng: &mut Rng, fresh: u64, longest: u64) -> (Kind, usize) {
    let kind = if rng.chance(fresh) {
        Kind::Fresh
    } else {
        Kind::Plain
    };
    let len = match rng.below(5) {
        0 => 0,
        1 => 8,
        2 => 64,
        3 => 128,
        _ => 1 + rng.below(longest) as usize,
    };
    (kind, len)
}

/// Which instructions each of `cpus` processors reports. The first reports both. Each other
/// reports RDSEED alone, RDRAND alone or neither with one chance in `sides` each, and both
/// otherwise.
fn reports(rng: &mut Rng, cpus: usize, sides: u64) -> Vec<(bool, bool)> {
    let mut reports = vec![(true, true)];
    for _ in 1..cpus {
        reports.push(match rng.below(sides) {
            0 => (true, false),
            1 => (false, true),
            2 => (false, false),
            _ => (true, true),
        });
    }
    reports
}

/// How far the clock has moved when the next request is made. One request in `waits` comes a
/// second or two after the one before it, one in `waits` 20 to 70 seconds after, and the
/// others sooner than `mostly` nanoseconds. The fewer the waits, the oftener a reseed falls
/// due.
fn interval(rng: &mut Rng, waits: u64, mostly: u64) -> u64 {
    match rng.below(waits) {
        0 => 1_000_000_000 + rng.below(1_000_000_000),
        1 => 20_000_000_000 + rng.below(50_000_000_000),
        _ => rng.below(mostly),
    }
}

/// A configuration on one thread of control.
#[derive(Clone, Copy, Debug)]
struct Single {
    weather: Weather,
    chaos: Chaos,
}

impl Single {
    /// Requests that nest and threads that move this often.
    const fn of(weather: Weather, nest: u64, moves: u64) -> Self {
        Self {
            weather,
            chaos: Chaos::of(nest, moves),
        }
    }
}

/// One machine of four processors, and `requests` requests on one thread of control.
fn run(seed: u64, requests: u32, config: Single) -> Machine {
    let mut rng = Rng::new(seed ^ 0xABCD);
    let mut reports = reports(&mut rng, 4, 6);
    if rng.chance(250) {
        // The root is then counted on RDSEED with no RDRAND beside it.
        reports[0] = (true, false);
    }
    let mut machine = Machine::started(seed, &reports);
    machine.weather = Some(config.weather);
    machine.chaos = Some(config.chaos);
    for _ in 0..requests {
        machine.on = machine.rng.index(reports.len());
        machine.now += interval(&mut machine.rng, 20, 50_000_000);
        machine.request(false);
    }
    machine
}

/// The machine among threads, and whose turn it is.
struct Shared {
    machine: Machine,
    turn: usize,
    /// The threads that still have requests to make.
    alive: Vec<bool>,
}

impl Shared {
    /// Gives the turn to one of the threads that are left, the one that has it included.
    fn pass(&mut self) {
        let left = self.alive.iter().filter(|alive| **alive).count();
        if left == 0 {
            return;
        }
        let pick = self.machine.rng.index(left);
        if let Some(next) = (0..self.alive.len()).filter(|&id| self.alive[id]).nth(pick) {
            self.turn = next;
        }
    }
}

/// What the threads share: the machine behind a lock and, for each thread, the signal that
/// its turn has come.
struct Turns {
    shared: Mutex<Shared>,
    signals: Vec<Condvar>,
}

impl Turns {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        // A thread that panicked fails the test by itself. The others still have to end.
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One of several threads: a [`Platform`] whose every step waits for the thread's turn.
struct Thread<'a> {
    id: usize,
    turns: &'a Turns,
    /// The processor the thread is on.
    on: usize,
    /// The request it is making.
    frame: Option<Frame>,
    /// Whether that request came with interrupts off.
    quick: bool,
    /// In a thousand, after a step: the thread is on a processor chosen anew.
    moves: u64,
    /// In a thousand, at a step under the root's lock: the lock is found taken.
    root_busy: u64,
}

impl Thread<'_> {
    /// Runs `act` on the machine when it is this thread's turn, then gives the turn on. After
    /// its `last` turn the thread is given none.
    fn turn<T>(&mut self, last: bool, act: impl FnOnce(&mut Self, &mut Machine) -> T) -> T {
        let turns = self.turns;
        let mut shared = turns.lock();
        while shared.turn != self.id {
            shared = turns.signals[self.id]
                .wait(shared)
                .unwrap_or_else(PoisonError::into_inner);
        }
        let result = act(self, &mut shared.machine);
        if last {
            shared.alive[self.id] = false;
        }
        shared.pass();
        let next = shared.turn;
        drop(shared);
        if next != self.id {
            turns.signals[next].notify_one();
        }
        result
    }

    /// Runs one step of this thread's request on the machine. The thread may be on another
    /// processor afterwards.
    fn on_machine(
        &mut self,
        machine: &mut Machine,
        run: impl FnOnce(&mut Machine) -> Result<(), Busy>,
    ) -> Result<(), Busy> {
        machine.on = self.on;
        machine.log.frames.extend(self.frame.take());
        let done = run(machine);
        self.frame = machine.log.frames.pop();
        if machine.rng.chance(self.moves) {
            machine.log.tally.moves += 1;
            self.on = machine.rng.index(machine.nodes.len());
        }
        done
    }

    /// One request, from beginning to end: a turn to begin it, one for each step and each
    /// pause, and one to end it.
    fn request(&mut self) {
        let (kind, len) = self.turn(false, |thread, machine| {
            let (kind, len) = shape(&mut machine.rng, 350, 300);
            thread.quick = machine.rng.chance(200);
            thread.on = machine.rng.index(machine.nodes.len());
            machine.now += interval(&mut machine.rng, 40, 5_000_000);
            thread.frame = Some(machine.log.open(kind));
            (kind, len)
        });
        let mut out = vec![UNWRITTEN; len];
        let result = match kind {
            Kind::Plain => fill(self, &mut out),
            Kind::Fresh => fill_fresh(self, &mut out),
        };
        self.turn(false, |thread, machine| match thread.frame.take() {
            Some(frame) => machine.judge(&frame, &out, result),
            None => machine
                .log
                .violation("the model lost a request".to_string()),
        });
    }

    /// The thread's last act: a turn of its own, so that the order of the turns depends on
    /// the seed alone.
    fn leave(&mut self) {
        self.turn(true, |_, _| ());
    }
}

impl Platform for Thread<'_> {
    fn step(&mut self, step: &mut dyn FnMut(&mut Ctx<'_>)) -> Result<(), Busy> {
        self.turn(false, |thread, machine| {
            thread.on_machine(machine, |machine| machine.step(step))
        })
    }

    fn root_step(&mut self, step: &mut dyn FnMut(&mut Ctx<'_>, &mut Root)) -> Result<(), Busy> {
        self.turn(false, |thread, machine| {
            if machine.rng.chance(thread.root_busy) {
                machine.now += 500;
                return Err(Busy::Root);
            }
            thread.on_machine(machine, |machine| machine.root_step(step))
        })
    }

    fn pause(&mut self) {
        self.turn(false, |_, machine| machine.now += 40);
    }

    fn quick(&self) -> bool {
        self.quick
    }
}

/// Takes a thread out of the turns if it ends without leaving, which only a panic makes it
/// do: the others must not wait for a turn that is never given on.
struct Gone<'a> {
    id: usize,
    turns: &'a Turns,
}

impl Drop for Gone<'_> {
    fn drop(&mut self) {
        let mut shared = self.turns.lock();
        if !shared.alive[self.id] {
            return;
        }
        shared.alive[self.id] = false;
        if shared.turn == self.id {
            shared.pass();
            let next = shared.turn;
            drop(shared);
            self.turns.signals[next].notify_one();
        }
    }
}

/// A configuration with several threads.
#[derive(Clone, Copy, Debug)]
struct Threads {
    threads: usize,
    cpus: usize,
    /// Requests each thread makes.
    requests: u32,
    /// In a thousand, after a step: the thread is on a processor chosen anew.
    moves: u64,
    weather: Weather,
}

/// One machine, and several threads that make requests on it.
fn run_threads(seed: u64, config: Threads) -> Machine {
    let mut rng = Rng::new(seed ^ 0x7777);
    let reports = reports(&mut rng, config.cpus, 8);
    let mut machine = Machine::started(seed, &reports);
    machine.weather = Some(config.weather);
    let turns = Turns {
        shared: Mutex::new(Shared {
            machine,
            turn: 0,
            alive: vec![true; config.threads],
        }),
        signals: (0..config.threads).map(|_| Condvar::new()).collect(),
    };
    thread::scope(|scope| {
        for id in 0..config.threads {
            let turns = &turns;
            scope.spawn(move || {
                let _gone = Gone { id, turns };
                let mut thread = Thread {
                    id,
                    turns,
                    on: 0,
                    frame: None,
                    quick: false,
                    moves: config.moves,
                    root_busy: 20,
                };
                for _ in 0..config.requests {
                    thread.request();
                }
                thread.leave();
            });
        }
    });
    turns
        .shared
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
        .machine
}

/// Adds up what `machines` did, prints it, and fails if any of them holds a violation.
fn check(name: &str, machines: &[Machine]) -> Tally {
    let mut total = Tally::default();
    let mut given = 0;
    let mut violations = Vec::new();
    for machine in machines {
        total.add(&machine.log.tally);
        given += machine.log.given;
        let first = machine.log.first_violation.unwrap_or(0);
        for text in &machine.log.violations {
            violations.push(format!(
                "seed {}, first at request {first}: {text}",
                machine.seed
            ));
        }
    }
    println!(
        "{name}: {} machines. Plain requests: {} answered, {} refused. With prediction \
         resistance: {} answered, {} refused. Reseeds: {} of processors, {} of the root ({} from \
         RDRAND with RDSEED draws set aside), {} pieces with prediction resistance. {} failures \
         of an instruction, {} nested requests, {} moves. At most {} draws in a plain request. \
         {given} values given, {} of them in seeds and held against the rules, {} of those the \
         root's.",
        machines.len(),
        total.plain_answered,
        total.plain_refused,
        total.fresh_answered,
        total.fresh_refused,
        total.own_reseeds,
        total.root_reseeds,
        total.root_fallbacks,
        total.fresh_pieces,
        total.failures,
        total.nested,
        total.moves,
        total.most_plain_draws,
        total.draws_checked,
        total.root_draws_checked,
    );
    assert!(
        violations.is_empty(),
        "{name}: the rules were broken {} time(s):\n  {}",
        violations.len(),
        violations.join("\n  ")
    );
    // A run that makes no seed keeps every rule.
    assert!(total.draws_checked > 0, "{name}: no seed was made");
    total
}

/// `machines` machines on one thread of control, with seeds from `first` on.
fn single(name: &str, first: u64, machines: u64, requests: u32, config: Single) -> Tally {
    let machines: Vec<Machine> = (first..first + machines)
        .map(|seed| run(seed, requests, config))
        .collect();
    check(name, &machines)
}

/// `machines` machines among threads, with seeds from `first` on.
fn threaded(name: &str, first: u64, machines: u64, config: Threads) -> Tally {
    let machines: Vec<Machine> = (first..first + machines)
        .map(|seed| run_threads(seed, config))
        .collect();
    check(name, &machines)
}

// ---- One thread of control. ----

fn one_after_another(machines: u64, requests: u32) {
    let config = Single::of(ROUGH, 0, 0);
    single("one request after another", 0, machines, requests, config);
}

#[test]
fn requests_one_after_another_keep_the_rules() {
    one_after_another(6, 2000);
}

#[test]
#[ignore = "at length: run with -- --ignored"]
fn requests_one_after_another_keep_the_rules_at_length() {
    one_after_another(12, 4000);
}

fn nested(machines: u64, requests: u32) {
    let config = Single::of(ROUGH, 40, 0);
    let tally = single("nested requests", 100, machines, requests, config);
    assert!(tally.nested > 0, "no request ran inside another");
}

#[test]
fn nested_requests_keep_the_rules() {
    nested(6, 2000);
}

#[test]
#[ignore = "at length: run with -- --ignored"]
fn nested_requests_keep_the_rules_at_length() {
    nested(12, 4000);
}

/// Requests that nest and a thread that moves, in one weather: `nest` and `moves` in a
/// thousand steps.
fn nested_and_moving(
    name: &str,
    first: u64,
    (machines, requests): (u64, u32),
    weather: Weather,
    (nest, moves): (u64, u64),
) -> Tally {
    let config = Single::of(weather, nest, moves);
    let tally = single(name, first, machines, requests, config);
    assert!(tally.nested > 0, "no request ran inside another");
    assert!(tally.moves > 0, "no thread moved");
    tally
}

fn in_rough_weather(size: (u64, u32)) {
    let name = "nested requests on a thread that moves";
    nested_and_moving(name, 200, size, ROUGH, (40, 60));
}

#[test]
fn nested_requests_on_a_thread_that_moves_keep_the_rules() {
    in_rough_weather((6, 2000));
}

#[test]
#[ignore = "at length: run with -- --ignored"]
fn nested_requests_on_a_thread_that_moves_keep_the_rules_at_length() {
    in_rough_weather((16, 4000));
}

fn in_fair_weather(size: (u64, u32)) {
    let name = "nested requests on a thread that moves, fair weather";
    nested_and_moving(name, 300, size, FAIR, (40, 60));
}

#[test]
fn the_rules_are_kept_in_fair_weather() {
    in_fair_weather((6, 2000));
}

#[test]
#[ignore = "at length: run with -- --ignored"]
fn the_rules_are_kept_in_fair_weather_at_length() {
    in_fair_weather((12, 4000));
}

fn in_dry_weather(size: (u64, u32)) -> Tally {
    let name = "nested requests on a thread that moves, dry weather";
    nested_and_moving(name, 900, size, DRY, (40, 60))
}

#[test]
fn the_rules_are_kept_in_dry_weather() {
    in_dry_weather((6, 2000));
}

/// At this length the weather makes the root's reseed fall back to RDRAND, three times on the
/// code of 2026-11-11.
#[test]
#[ignore = "at length: run with -- --ignored"]
fn the_rules_are_kept_in_dry_weather_at_length() {
    let tally = in_dry_weather((16, 4000));
    assert!(
        tally.root_fallbacks > 0,
        "no reseed of the root fell back to RDRAND"
    );
}

fn crowded(size: (u64, u32)) {
    let name = "a request inside almost every other, on a thread that moves every few steps";
    nested_and_moving(name, 1000, size, ROUGH, (300, 150));
}

#[test]
fn requests_crowded_into_each_other_keep_the_rules() {
    crowded((6, 1500));
}

#[test]
#[ignore = "at length: run with -- --ignored"]
fn requests_crowded_into_each_other_keep_the_rules_at_length() {
    crowded((16, 4000));
}

/// With instructions that always deliver, no lock taken and no move, the root's seed is never
/// older than a reseed period after a request on a processor that could reseed it.
fn calm(machines: u64, requests: u32) {
    let mut worst = 0;
    let mut seen = 0u64;
    let mut made = Vec::new();
    for seed in 0..machines {
        let mut rng = Rng::new(seed ^ 0x5151);
        let reports = reports(&mut rng, 4, 6);
        let mut machine = Machine::started(seed, &reports);
        for _ in 0..requests {
            machine.on = machine.rng.index(reports.len());
            machine.now += interval(&mut machine.rng, 20, 50_000_000);
            let on = machine.on;
            let frame = machine.log.open(Kind::Plain);
            machine.log.frames.push(frame);
            let mut out = [UNWRITTEN; 32];
            let result = fill(&mut machine, &mut out);
            if let Some(frame) = machine.log.frames.pop() {
                machine.judge(&frame, &out, result);
            }
            if machine.nodes[on].is_healthy(Rdseed) || machine.nodes[on].is_healthy(Rdrand) {
                let age = machine.now.saturating_sub(machine.root.seeded_at());
                worst = worst.max(age);
                seen += 1;
                if age >= PERIOD + 5_000_000 {
                    machine.log.violation(format!(
                        "calm: the root's seed is {age} ns old after a request on processor \
                         {on}, which could reseed it"
                    ));
                }
            }
        }
        made.push(machine);
    }
    let tally = check("calm weather, one request after another", &made);
    println!(
        "calm weather: after {seen} requests on processors that could reseed the root, its \
         seed was at most {worst} ns old"
    );
    assert!(
        seen > 0,
        "no request was on a processor that could reseed the root"
    );
    assert!(tally.root_reseeds > 0, "the root was never reseeded");
}

#[test]
fn the_root_keeps_its_schedule_in_calm_weather() {
    calm(8, 2000);
}

#[test]
#[ignore = "at length: run with -- --ignored"]
fn the_root_keeps_its_schedule_in_calm_weather_at_length() {
    calm(40, 6000);
}

// ---- Several threads. ----

/// Threads on `cpus` processors in rough weather that stay where they are while a request
/// runs.
fn threads_that_stay(name: &str, first: u64, threads: usize, cpus: usize, size: (u64, u32)) {
    let config = Threads {
        threads,
        cpus,
        requests: size.1,
        moves: 0,
        weather: ROUGH,
    };
    threaded(name, first, size.0, config);
}

fn on_one_processor(size: (u64, u32)) {
    threads_that_stay("three threads on one processor", 400, 3, 1, size);
}

#[test]
fn threads_on_one_processor_keep_the_rules() {
    on_one_processor((4, 500));
}

#[test]
#[ignore = "at length: run with -- --ignored"]
fn threads_on_one_processor_keep_the_rules_at_length() {
    on_one_processor((10, 1500));
}

fn on_their_processors(size: (u64, u32)) {
    let name = "four threads on three processors, no move inside a request";
    threads_that_stay(name, 500, 4, 3, size);
}

#[test]
fn threads_that_stay_on_their_processors_keep_the_rules() {
    on_their_processors((4, 500));
}

#[test]
#[ignore = "at length: run with -- --ignored"]
fn threads_that_stay_on_their_processors_keep_the_rules_at_length() {
    on_their_processors((10, 1500));
}

/// Four threads that move among three processors, in one weather.
fn threads_that_move(name: &str, weather: Weather, first: u64, size: (u64, u32)) {
    let config = Threads {
        threads: 4,
        cpus: 3,
        requests: size.1,
        moves: 80,
        weather,
    };
    let tally = threaded(name, first, size.0, config);
    assert!(tally.moves > 0, "no thread moved");
}

fn moving_in_rough_weather(size: (u64, u32)) {
    let name = "four threads that move among three processors";
    threads_that_move(name, ROUGH, 600, size);
}

#[test]
fn threads_that_move_keep_the_rules() {
    moving_in_rough_weather((4, 500));
}

#[test]
#[ignore = "at length: run with -- --ignored"]
fn threads_that_move_keep_the_rules_at_length() {
    moving_in_rough_weather((12, 1500));
}

fn moving_in_fair_weather(size: (u64, u32)) {
    let name = "four threads that move among three processors, fair weather";
    threads_that_move(name, FAIR, 700, size);
}

#[test]
fn threads_that_move_keep_the_rules_in_fair_weather() {
    moving_in_fair_weather((4, 500));
}

#[test]
#[ignore = "at length: run with -- --ignored"]
fn threads_that_move_keep_the_rules_in_fair_weather_at_length() {
    moving_in_fair_weather((10, 1500));
}

fn moving_in_dry_weather(size: (u64, u32)) {
    let name = "four threads that move among three processors, dry weather";
    threads_that_move(name, DRY, 950, size);
}

#[test]
fn threads_that_move_keep_the_rules_in_dry_weather() {
    moving_in_dry_weather((4, 500));
}

#[test]
#[ignore = "at length: run with -- --ignored"]
fn threads_that_move_keep_the_rules_in_dry_weather_at_length() {
    moving_in_dry_weather((10, 1500));
}

/// Threads on one processor whose instructions never fail. One thread alone is always
/// answered. Two or three overwrite each other's gatherings for prediction resistance, and
/// most such requests are refused after thousands of draws: ADR-0016 records that, with its
/// bound, and no rule is broken by it. It makes this the configuration that takes longest.
fn in_clear_weather(threads: usize, size: (u64, u32)) -> Tally {
    let config = Threads {
        threads,
        cpus: 1,
        requests: size.1,
        moves: 0,
        weather: CLEAR,
    };
    let name = format!("{threads} thread(s) on one processor, instructions that never fail");
    threaded(&name, 800, size.0, config)
}

#[test]
fn threads_on_one_processor_keep_the_rules_when_no_instruction_fails() {
    let alone = in_clear_weather(1, (2, 400));
    assert_eq!(alone.fresh_refused, 0, "a request alone was refused");
    in_clear_weather(2, (2, 200));
    in_clear_weather(3, (2, 50));
}

#[test]
#[ignore = "at length: run with -- --ignored"]
fn threads_on_one_processor_keep_the_rules_when_no_instruction_fails_at_length() {
    let alone = in_clear_weather(1, (4, 800));
    assert_eq!(alone.fresh_refused, 0, "a request alone was refused");
    in_clear_weather(2, (4, 800));
    in_clear_weather(3, (4, 800));
}

/// A failure has to be found again: the same seed makes the same run, among threads too.
#[test]
fn the_same_seed_makes_the_same_run() {
    let config = Threads {
        threads: 4,
        cpus: 3,
        requests: 150,
        moves: 80,
        weather: ROUGH,
    };
    let runs = [run_threads(77, config), run_threads(77, config)];
    let [once, again] = runs.map(|machine| {
        let mut outputs: Vec<u64> = machine.outputs.into_iter().collect();
        outputs.sort_unstable();
        (
            machine.log.tally,
            machine.log.time,
            machine.log.given,
            machine.now,
            outputs,
        )
    });
    assert!(once.0.fresh_pieces > 0 && once.0.moves > 0);
    assert_eq!(once, again);
}
