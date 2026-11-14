//! The randomized hostile-input test (the contract's §7): definition blocks made at random from
//! the grammar, the same blocks mutated byte by byte, and blocks written to reach each limit. No
//! third-party crate: the generator is SplitMix64, as the repository's other models use.
//!
//! Each case checks that the interpreter returns (it never panics or hangs), that its memory
//! never passes the limit, that a failed load leaves the namespace as it was, that an evaluation
//! leaves no dynamic name behind, that no Global Lock is kept, and that the same seed gives the
//! same results. A third of the cases run against a host that refuses every region, and a third
//! under a time limit of one second with a clock that moves 5 ms each time it is read.
//!
//! The tests also require progress (the review of B1, F4): a share of the grammar's blocks must
//! load, a share of their evaluations must return a value or run past their first steps, and
//! several kinds of error must be seen, so that a regression refusing everything early cannot
//! pass. Each adversarial block has the outcome it was written for. The control test plants each
//! fault of [`super::Fault`] in turn and requires these tests to catch it.
//!
//! `ORIGIN_AML_CASES` sets the number of cases of each kind (default 2,000) and `ORIGIN_AML_SEED`
//! the first seed, for longer soaks; `ORIGIN_AML_STATS` prints what each kind did.

use super::encode::*;
use super::{Fault, TestHost, ends_within, interp_with, plant};
use crate::object::Object;
use crate::{DefinitionBlock, ErrorKind, IntegerWidth, Interpreter, Limit, Limits, Node, Value};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[self.below(v.len() as u64) as usize]
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

// --- The watchdog (after N-soak-1) -------------------------------------------------------------
//
// A soak that hangs must not hold the shared machine: the first randomized test to start starts
// a watchdog thread, and if the tests have not ended within `ORIGIN_AML_WATCHDOG_SECS` seconds
// (default 1,200, twenty minutes), it prints the case each kind was on, its seed and its index,
// and ends the process failing. The 2,000 cases of each kind `cargo test` runs take seconds.

const GRAMMAR: usize = 0;
const MUTATION: usize = 1;
const ADVERSARIAL: usize = 2;
const KINDS: [&str; 3] = ["grammar", "mutation", "adversarial"];

static ON_SEED: [std::sync::atomic::AtomicU64; 3] =
    [const { std::sync::atomic::AtomicU64::new(u64::MAX) }; 3];
static ON_INDEX: [std::sync::atomic::AtomicU64; 3] =
    [const { std::sync::atomic::AtomicU64::new(u64::MAX) }; 3];
static WATCHDOG: std::sync::Once = std::sync::Once::new();

fn watchdog_secs() -> u64 {
    std::env::var("ORIGIN_AML_WATCHDOG_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_200)
}

/// Notes the case a kind is starting, and starts the watchdog if it is not running.
fn watch(kind: usize, seed: u64, index: u64) {
    use std::sync::atomic::Ordering::Relaxed;
    ON_SEED[kind].store(seed, Relaxed);
    ON_INDEX[kind].store(index, Relaxed);
    WATCHDOG.call_once(|| {
        let bound = std::time::Duration::from_secs(watchdog_secs());
        let started = std::time::Instant::now();
        std::thread::spawn(move || {
            while started.elapsed() < bound {
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            // Straight to the process's stderr: a thread a test spawns inherits the harness's
            // capture of `eprintln!`, which is never shown when the process exits.
            use std::io::Write;
            let mut out = std::io::stderr().lock();
            let _ = writeln!(
                out,
                "\nrandomized-test watchdog: still running after {} s; the cases each kind was on:",
                bound.as_secs()
            );
            for (k, what) in KINDS.iter().enumerate() {
                let (seed, index) = (ON_SEED[k].load(Relaxed), ON_INDEX[k].load(Relaxed));
                let _ = if index == u64::MAX {
                    writeln!(out, "  {what}: not started")
                } else {
                    writeln!(out, "  {what}: seed {seed:#x}, index {index}")
                };
            }
            let _ = out.flush();
            std::process::exit(101);
        });
    });
}

/// How one case runs.
#[derive(Clone, Copy, Debug)]
struct Setup {
    /// The host refuses every region access, as the kernel's boot proof's.
    refuse: bool,
    /// One second of time, the clock moving 5 ms a read; otherwise no time limit and a clock
    /// moving 100 ns a read.
    timed: bool,
    /// Ten million steps instead of 20,000, so that another limit is reached first.
    roomy: bool,
    /// The host traces: what happens must not change (the review of B1, F9).
    traced: bool,
}

impl Setup {
    const PLAIN: Self = Self {
        refuse: false,
        timed: false,
        roomy: false,
        traced: false,
    };
    const TIMED: Self = Self {
        timed: true,
        ..Self::PLAIN
    };
    const ROOMY: Self = Self {
        roomy: true,
        ..Self::PLAIN
    };

    /// A third of the seeds refuse regions, another third are timed.
    fn of(seed: u64) -> Self {
        Self {
            refuse: seed.is_multiple_of(3),
            timed: seed % 3 == 1,
            ..Self::PLAIN
        }
    }

    fn limits(self) -> Limits {
        Limits {
            loop_iterations: 2_000,
            time_100ns: if self.timed { 10_000_000 } else { u64::MAX },
            call_depth: 16,
            memory_bytes: 2 << 20,
            steps: if self.roomy { 10_000_000 } else { 20_000 },
        }
    }
}

/// Names an expression reads: data, a device, a method taking no argument, a field, a name
/// methods make, and one that never exists.
const NAMES: &[&str] = &[
    "N0", "N1", "N2", "BF", "PK", "BD", "D0", "D1", "M0", "F0", "T0", "N3",
];
/// Names a store writes.
const PLACES: &[&str] = &["N0", "N1", "BF", "PK", "BD", "F0", "LK", "N3"];
const METHODS: &[(&str, u8)] = &[("M0", 0), ("M1", 1), ("M2", 2), ("\\D0.M3", 1)];

struct Gen {
    rng: Rng,
    /// The arguments the method being made takes.
    argc: u8,
    /// The `While`s around the statement being made.
    loops: u32,
}

impl Gen {
    fn expr(&mut self, depth: u32) -> B {
        let r = &mut self.rng;
        if depth == 0 || r.chance(30) {
            return match r.below(9) {
                // Small integers half the time, so that indexes, lengths and times are often in
                // range.
                0 if r.chance(50) => int(r.below(8)),
                0 => int(r.next() >> r.below(64)),
                1 => ones(),
                2 => string(r.pick(&["", "a", "0x10", "12", "Windows 2022", "\\N0"])),
                3 => buffer(byte(r.below(6) as u8), &[1, 2, 3][..r.below(4) as usize]),
                4 => local(r.below(8) as u8),
                5 if self.argc > 0 => arg(r.below(u64::from(self.argc)) as u8),
                5 => local(r.below(8) as u8),
                6 => nm(r.pick(NAMES)),
                7 => revision(),
                _ => small_package(r),
            };
        }
        let d = depth - 1;
        match self.rng.below(30) {
            0 => add(self.expr(d), self.expr(d), self.target()),
            // Add, Concatenate, Subtract, Multiply, the shifts, And, NAnd, Or, NOr, XOr, Mod.
            1 => bin(
                *self.rng.pick(&[
                    0x72, 0x73, 0x74, 0x77, 0x79, 0x7A, 0x7B, 0x7C, 0x7D, 0x7E, 0x7F, 0x85,
                ]),
                self.expr(d),
                self.expr(d),
                self.target(),
            ),
            2 => divide(self.expr(d), self.expr(d), self.target(), self.target()),
            3 => un(
                *self.rng.pick(&[0x80, 0x81, 0x82, 0x96, 0x97, 0x98, 0x99]),
                self.expr(d),
                self.target(),
            ),
            4 => logic(
                *self.rng.pick(&[0x90, 0x91, 0x93, 0x94, 0x95]),
                self.expr(d),
                self.expr(d),
            ),
            5 => lnot(self.expr(d)),
            6 => concat(self.expr(d), self.expr(d), self.target()),
            7 => index(self.expr(d), self.expr(d), self.target()),
            8 => deref_of(self.expr(d)),
            9 => ref_of(self.place()),
            10 => size_of(self.place()),
            11 => object_type(self.place()),
            12 => mid(self.expr(d), self.expr(d), self.expr(d), self.target()),
            13 => to_string(self.expr(d), self.expr(d), self.target()),
            14 => match_(
                self.expr(d),
                self.rng.below(7) as u8,
                self.expr(d),
                self.rng.below(6) as u8,
                self.expr(d),
                self.expr(d),
            ),
            15 => store(self.expr(d), self.place()),
            16 => copy_object(self.expr(d), self.place()),
            17 => cond_ref_of(self.place(), self.target()),
            18 => {
                let (n, argc) = *self.rng.pick(METHODS);
                let args: Vec<B> = (0..argc).map(|_| self.expr(d)).collect();
                call(n, &args)
            }
            19 => increment(self.place()),
            20 => un_ext(*self.rng.pick(&[0x28, 0x29]), self.expr(d), self.target()),
            21 => buffer(self.expr(d), &[9, 8]),
            22 => var_package(self.expr(d), &[one()]),
            // Step B2: what reaches the host.
            24 => acquire(
                nm(self.rng.pick(&["MX", "\\_GL"])),
                *self.rng.pick(&[0, 5, 0xFFFF]),
            ),
            25 => wait(nm("EV"), self.expr(d)),
            26 => timer(),
            27 => nm(self.rng.pick(&["F0", "F1", "IX", "LK"])),
            28 => store(self.expr(d), nm(self.rng.pick(&["F0", "F1", "IX", "LK"]))),
            _ => bin(0x84, self.expr(d), self.expr(d), self.target()),
        }
    }

    fn place(&mut self) -> B {
        match self.rng.below(4) {
            0 => local(self.rng.below(8) as u8),
            1 if self.argc > 0 => arg(self.rng.below(u64::from(self.argc)) as u8),
            1 => local(self.rng.below(8) as u8),
            2 => debug(),
            _ => nm(self.rng.pick(PLACES)),
        }
    }

    fn target(&mut self) -> B {
        if self.rng.chance(50) {
            none()
        } else {
            self.place()
        }
    }

    fn stmts(&mut self, depth: u32) -> Vec<B> {
        let n = self.rng.below(5);
        (0..n).map(|_| self.stmt(depth)).collect()
    }

    fn stmt(&mut self, depth: u32) -> B {
        let d = depth.saturating_sub(1);
        match self.rng.below(if depth == 0 { 6 } else { 18 }) {
            0 => store(self.expr(2), self.place()),
            1 => ret(self.expr(2)),
            2 => noop(),
            3 => increment(local(self.rng.below(8) as u8)),
            4 => self.expr(2),
            5 => name(self.rng.pick(&["T0", "T1"]), int(self.rng.below(9))),
            6 => if_(self.expr(2), &self.stmts(d)),
            7 => if_else(self.expr(2), &self.stmts(d), &self.stmts(d)),
            8 => {
                self.loops += 1;
                let body = self.stmts(d);
                self.loops -= 1;
                while_(
                    logic(0x95, increment(local(7)), byte(self.rng.below(40) as u8)),
                    &body,
                )
            }
            // Inside a `While`, or one time in ten outside one (`NotInLoop`).
            9 if self.loops > 0 || self.rng.chance(10) => {
                if self.rng.chance(50) {
                    brk()
                } else {
                    cont()
                }
            }
            9 => noop(),
            10 => create_field(
                nm("BF"),
                self.expr(1),
                self.expr(1),
                self.rng.pick(&["C0", "C1"]),
            ),
            12 => release(nm(self.rng.pick(&["MX", "\\_GL"]))),
            13 => {
                if self.rng.chance(50) {
                    signal(nm("EV"))
                } else {
                    reset(nm("EV"))
                }
            }
            // 1.5 s passes a timed case's limit: refused before it sleeps.
            14 => sleep(word(*self.rng.pick(&[0, 1, 3, 400, 1500]))),
            15 => stall(self.expr(1)),
            16 => notify(nm(self.rng.pick(&["D0", "N0"])), self.expr(1)),
            _ => device(self.rng.pick(&["DD0", "DD1"]), &[name("X", one())]),
        }
    }

    /// A method body: most locals set first, then statements, then most often a value returned.
    fn body(&mut self, argc: u8) -> Vec<B> {
        self.argc = argc;
        let mut out = Vec::new();
        for k in 0..8u8 {
            if self.rng.chance(90) {
                let v = self.datum();
                out.push(store(v, local(k)));
            }
        }
        out.extend(self.stmts(3));
        if self.rng.chance(90) {
            out.push(ret(self.expr(2)));
        }
        self.argc = 0;
        out
    }

    /// A value that reads nothing: what a method's prefix sets its locals to.
    fn datum(&mut self) -> B {
        let r = &mut self.rng;
        match r.below(5) {
            0 | 1 => int(r.next() >> r.below(64)),
            2 => string(r.pick(&["", "a", "0x10", "12", "\\N0"])),
            3 => buffer(byte(r.below(6) as u8), &[1, 2, 3][..r.below(4) as usize]),
            _ => small_package(r),
        }
    }

    /// A predicate load-time code can evaluate: names and constants only.
    fn load_expr(&mut self) -> B {
        match self.rng.below(4) {
            0 => one(),
            1 => logic(0x93, nm("N0"), nm("\\N2")),
            2 => lnot(revision()),
            _ => logic(0x95, revision(), int(self.rng.below(4))),
        }
    }

    fn table(&mut self) -> Vec<B> {
        let m3 = self.body(1);
        let mut body = vec![
            name("N0", int(self.rng.next())),
            name("N1", string("n1")),
            name("BF", buffer(byte(16), &[1, 2, 3, 4])),
            name(
                "PK",
                package(4, &[one(), string("x"), nm("\\N0"), nm("\\D0")]),
            ),
            device(
                "D0",
                &[name("_HID", dword(0x0A0C_D041)), method("M3", 1, &m3)],
            ),
            op_region("R0", 1, word(0x80), byte(8)),
            field("R0", 1, &[Fe::Named("F0", 8), Fe::Named("F1", 8)]),
            field("R0", 0x13, &[Fe::Reserved(32), Fe::Named("LK", 32)]),
            index_field("F0", "F1", 1, &[Fe::Reserved(8), Fe::Named("IX", 12)]),
            mutex("MX", self.rng.below(16) as u8),
            event("EV"),
            create_dword_field(nm("BF"), zero(), "BD"),
            alias("\\N0", "\\N2"),
        ];
        for (n, argc) in METHODS.iter().take(3) {
            let flags = *argc | if self.rng.chance(20) { 8 } else { 0 };
            let b = self.body(*argc);
            body.push(method(n, flags, &b));
        }
        if self.rng.chance(30) {
            // Load-time code. One in ten takes any expression, which often fails, so that a load
            // fails after it made names and must remove them.
            let pred = if self.rng.chance(10) {
                self.expr(2)
            } else {
                self.load_expr()
            };
            body.push(if_(pred, &[device("\\D1", &[])]));
        }
        body
    }
}

/// A package of up to two elements, its count the same or one more (more elements than the
/// count is refused, §19.6.102, and the mutated blocks reach that).
fn small_package(r: &mut Rng) -> B {
    let n = r.below(3) as usize;
    package((n as u64 + r.below(2)) as u8, &[one(), string("p")][..n])
}

/// The methods a namespace holds, with their argument counts.
fn methods(i: &Interpreter<TestHost>) -> Vec<(Node, u8)> {
    let mut out = Vec::new();
    let mut stack = vec![i.root()];
    while let Some(n) = stack.pop() {
        if let Ok(obj) = i.ns.obj(n.0)
            && let Ok(Object::Method(m)) = i.heap.get(obj)
        {
            out.push((n, m.argc()));
        }
        let mut c = i.first_child(n);
        while let Some(k) = c {
            stack.push(k);
            c = i.next_sibling(k);
        }
    }
    out
}

/// Every node's path: the namespace's fingerprint.
fn snapshot(i: &Interpreter<TestHost>) -> Vec<Vec<[u8; 4]>> {
    let mut out = Vec::new();
    let mut stack = vec![i.root()];
    while let Some(n) = stack.pop() {
        out.push(i.path_of(n).unwrap());
        let mut c = i.first_child(n);
        while let Some(k) = c {
            stack.push(k);
            c = i.next_sibling(k);
        }
    }
    out.sort();
    out
}

fn random_args(rng: &mut Rng, argc: u8) -> Vec<Value> {
    (0..argc)
        .map(|_| match rng.below(4) {
            0 => Value::Integer(rng.next()),
            1 => Value::String(b"arg".to_vec()),
            2 => Value::Buffer(vec![1, 2, 3]),
            _ => Value::Package(vec![Value::Integer(1)]),
        })
        .collect()
}

/// What a run of cases did: printed when `ORIGIN_AML_STATS` is set, and held to floors.
#[derive(Default)]
struct Stats {
    cases: u64,
    loaded: u64,
    evaluations: u64,
    values: u64,
    /// Evaluations that ran at least [`DEEP`] operations.
    deep: u64,
    errors: std::collections::BTreeMap<String, u64>,
}

/// The operations an evaluation must complete (opcodes, names, locals, constants) to count as
/// having got past its first statements: a method's prefix setting its locals is at most 24.
const DEEP: u64 = 32;

impl Stats {
    fn print(&self, what: &str) {
        if std::env::var_os("ORIGIN_AML_STATS").is_some() {
            eprintln!(
                "{what}: {} cases, {} loaded ({}%), {} evaluations, {} returned a value ({}%), \
                 {} ran {DEEP} operations or more ({}%); {} kinds of error: {:?}",
                self.cases,
                self.loaded,
                percent(self.loaded, self.cases),
                self.evaluations,
                self.values,
                percent(self.values, self.evaluations),
                self.deep,
                percent(self.deep, self.evaluations),
                self.errors.len(),
                self.errors
            );
        }
    }

    /// Requires progress, when there were cases enough for shares to mean something.
    fn require(&self, what: &str, floors: Floors) {
        let Floors {
            loaded,
            values,
            deep,
            kinds,
            seen,
        } = floors;
        if self.cases < 500 {
            return;
        }
        let shares = (
            percent(self.loaded, self.cases),
            percent(self.values, self.evaluations),
            percent(self.deep, self.evaluations),
        );
        assert!(
            shares.0 >= loaded && shares.1 >= values && shares.2 >= deep,
            "{what}: loaded {}%, returned a value {}%, ran {DEEP} operations {}%: below the floors \
             {loaded}%, {values}%, {deep}%",
            shares.0,
            shares.1,
            shares.2
        );
        assert!(
            self.errors.len() >= kinds,
            "{what}: {} kinds of error, fewer than {kinds}: {:?}",
            self.errors.len(),
            self.errors
        );
        for e in seen {
            assert!(
                self.errors.contains_key(*e),
                "{what}: no {e} among {:?}",
                self.errors
            );
        }
    }
}

/// The progress a kind of case must show: per cent of cases loaded, per cent of evaluations that
/// returned a value, per cent that ran [`DEEP`] operations, and kinds of error seen. Each is set below
/// what the default seeds give (recorded in the provenance entry), with room for other seeds.
#[derive(Clone, Copy)]
struct Floors {
    loaded: u64,
    values: u64,
    deep: u64,
    kinds: usize,
    /// Errors that must be among them.
    seen: &'static [&'static str],
}

/// The default seeds give 98%, 22%, 31% and 19 kinds, with every limit but `Loops` reached.
const GRAMMAR_FLOORS: Floors = Floors {
    loaded: 90,
    values: 15,
    deep: 20,
    kinds: 14,
    seen: &[
        "Limit(Time)",
        "Limit(Memory)",
        "Limit(Depth)",
        "Region(Refused)",
    ],
};

/// The default seeds give 13%, 21%, 28% and 21 kinds.
const MUTATION_FLOORS: Floors = Floors {
    loaded: 8,
    values: 12,
    deep: 18,
    kinds: 15,
    seen: &["Grammar", "PackageLength", "Truncated", "Reserved"],
};

fn percent(n: u64, of: u64) -> u64 {
    (n * 100).checked_div(of).unwrap_or(0)
}

thread_local! {
    static STATS: std::cell::RefCell<Stats> = std::cell::RefCell::new(Stats::default());
}

fn note(f: impl FnOnce(&mut Stats)) {
    STATS.with(|s| f(&mut s.borrow_mut()));
}

fn take_stats() -> Stats {
    STATS.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

fn note_error(e: &crate::Error) {
    let key = match e.kind {
        ErrorKind::Reserved { .. } => "Reserved".to_string(),
        ErrorKind::Region { cause, .. } => format!("Region({cause:?})"),
        k => format!("{k:?}"),
    };
    note(|s| *s.errors.entry(key).or_insert(0) += 1);
}

/// What one case did.
#[derive(Debug, PartialEq)]
struct Ran {
    /// A digest of every outcome, for the determinism check.
    digest: u64,
    load: Result<(), ErrorKind>,
    /// Each method's evaluation, in the order `methods` gives.
    evals: Vec<Result<Option<Value>, ErrorKind>>,
}

/// Runs one case: loads `aml`, then evaluates every method with random arguments, checking the
/// invariants after each step.
fn run(aml: &[u8], width: IntegerWidth, seed: u64, setup: Setup) -> Ran {
    note(|s| s.cases += 1);
    let lim = setup.limits();
    let mut i = interp_with(width, lim);
    i.host_mut().tick = if setup.timed { 50_000 } else { 1 };
    i.host_mut().refuse = setup.refuse;
    if setup.traced {
        i.host_mut().events = Some(Vec::new());
    }
    let before = snapshot(&i);
    let block = DefinitionBlock {
        signature: *b"SSDT",
        oem_id: *b"ORIGIN",
        oem_table_id: *b"RANDOM  ",
        aml,
    };
    let mut digest: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |x: u64| digest = (digest ^ x).wrapping_mul(0x100_0000_01b3);
    let load = match i.load(block) {
        Ok(_) => {
            note(|s| s.loaded += 1);
            mix(1);
            Ok(())
        }
        Err(e) => {
            note_error(&e);
            mix(2 + e.at.map_or(0, |a| u64::from(a.offset)));
            assert_eq!(
                snapshot(&i),
                before,
                "seed {seed:#x}: a failed load left names behind"
            );
            Err(e.kind)
        }
    };
    assert!(
        i.memory_peak() <= lim.memory_bytes,
        "seed {seed:#x}: memory passed the limit"
    );
    assert_eq!(
        i.host().global_held,
        0,
        "seed {seed:#x}: the Global Lock was kept"
    );
    let after_load = i.node_count();
    let mut rng = Rng(seed ^ 0x5555);
    let mut evals = Vec::new();
    for (node, argc) in methods(&i) {
        let args = random_args(&mut rng, argc);
        super::take_ops();
        let r = i.evaluate_node(node, &args);
        let ops = super::take_ops();
        note(|s| {
            s.evaluations += 1;
            if ops >= DEEP {
                s.deep += 1;
            }
        });
        match &r {
            Ok(Some(_)) => note(|s| s.values += 1),
            Err(e) => note_error(e),
            Ok(None) => {}
        }
        mix(match &r {
            Ok(Some(_)) => 3,
            Ok(None) => 4,
            Err(e) => 5 + e.at.map_or(0, |a| u64::from(a.offset)),
        });
        assert_eq!(
            i.node_count(),
            after_load,
            "seed {seed:#x}: an evaluation left names behind"
        );
        assert!(
            i.memory_peak() <= lim.memory_bytes,
            "seed {seed:#x}: memory passed the limit"
        );
        assert_eq!(
            i.host().global_held,
            0,
            "seed {seed:#x}: the Global Lock was kept"
        );
        evals.push(r.map_err(|e| e.kind));
    }
    if let Some(events) = &i.host().events {
        let enters = events
            .iter()
            .filter(|e| matches!(e, super::Event::Enter(_)))
            .count();
        let leaves = events.iter().filter(|e| **e == super::Event::Leave).count();
        assert_eq!(
            enters, leaves,
            "seed {seed:#x}: a method entered and never left"
        );
    }
    Ran {
        digest,
        load,
        evals,
    }
}

fn cases() -> u64 {
    std::env::var("ORIGIN_AML_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2_000)
}

fn first_seed() -> u64 {
    std::env::var("ORIGIN_AML_SEED")
        .ok()
        .and_then(|v| u64::from_str_radix(v.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x0A11_0063)
}

fn generated(seed: u64) -> (Vec<u8>, IntegerWidth) {
    let mut g = Gen {
        rng: Rng(seed),
        argc: 0,
        loops: 0,
    };
    let width = if g.rng.chance(30) {
        IntegerWidth::Bits32
    } else {
        IntegerWidth::Bits64
    };
    (g.table().concat(), width)
}

/// `n` grammar cases from the seed `start`; every 97th is run again with a tracing host, and
/// must do the same: the same seed gives the same results, and tracing changes nothing.
fn grammar_cases(start: u64, n: u64) {
    for k in 0..n {
        let seed = start.wrapping_add(k);
        watch(GRAMMAR, seed, k);
        let (aml, width) = generated(seed);
        let setup = Setup::of(seed);
        let a = run(&aml, width, seed, setup);
        if k % 97 == 0 {
            let traced = Setup {
                traced: true,
                ..setup
            };
            assert_eq!(
                a,
                run(&aml, width, seed, traced),
                "seed {seed:#x}: not the same again, traced"
            );
        }
    }
}

#[test]
fn random_blocks_from_the_grammar() {
    grammar_cases(first_seed(), cases());
    let s = take_stats();
    s.print("grammar");
    s.require("grammar", GRAMMAR_FLOORS);
}

fn mutate(rng: &mut Rng, aml: &mut Vec<u8>) {
    for _ in 0..=rng.below(4) {
        if aml.is_empty() {
            aml.push(rng.next() as u8);
            continue;
        }
        let at = rng.below(aml.len() as u64) as usize;
        match rng.below(6) {
            0 => aml[at] ^= 1 << rng.below(8),
            1 => aml[at] = rng.next() as u8,
            2 => aml.insert(at, rng.next() as u8),
            3 => {
                aml.remove(at);
            }
            4 => aml.truncate(at),
            _ => {
                // Splice a run from elsewhere in the block.
                let from = rng.below(aml.len() as u64) as usize;
                let len = (rng.below(16) as usize).min(aml.len() - from);
                let run: Vec<u8> = aml[from..from + len].to_vec();
                aml.splice(at..at, run);
            }
        }
    }
}

/// `n` mutated grammar cases from the seed `start`.
fn mutation_cases(start: u64, n: u64) {
    for k in 0..n {
        let seed = start.wrapping_add(k);
        watch(MUTATION, seed, k);
        let (mut aml, width) = generated(seed);
        let mut rng = Rng(seed);
        mutate(&mut rng, &mut aml);
        run(&aml, width, seed, Setup::of(seed));
    }
}

#[test]
fn mutated_blocks() {
    mutation_cases(first_seed() ^ 0xFFFF_0000, cases());
    let s = take_stats();
    s.print("mutation");
    s.require("mutation", MUTATION_FLOORS);
}

/// What an adversarial block must do: fail to load with an error, or have its first method
/// end so.
#[derive(Clone, Debug)]
enum Expect {
    Load(ErrorKind),
    Eval(Result<Option<Value>, ErrorKind>),
}

impl Expect {
    fn is(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Load(a), Self::Load(b)) => a == b,
            (Self::Eval(a), Self::Eval(b)) => a == b,
            _ => false,
        }
    }
}

/// The blocks written to reach each limit, each with the setup it runs under and the outcome it
/// was written for; both integer widths.
fn adversarial_cases() {
    let mut deep_pkg = package(1, &[one()]);
    for _ in 0..5_000 {
        deep_pkg = package(1, &[deep_pkg]);
    }
    let many: Vec<B> = (0..20_000)
        .map(|k| device(&format!("\\Z{:03X}", k % 4096), &[]))
        .collect();
    let endless = vec![method("M0", 0, &[while_(one(), &[noop()])])];
    let limit = |l| Expect::Eval(Err(ErrorKind::Limit(l)));
    let doubling = vec![method(
        "M0",
        0,
        &[
            store(string("ab"), local(0)),
            while_(one(), &[concat(local(0), local(0), local(0))]),
        ],
    )];
    let cases: Vec<(Vec<B>, Setup, Expect)> = vec![
        (endless.clone(), Setup::PLAIN, limit(Limit::Loops)),
        // Each pass reads the clock, 5 ms a read: a second is 200 passes, before 2,000 loops.
        (endless, Setup::TIMED, limit(Limit::Time)),
        (
            vec![method("M0", 0, &[ret(call("M0", &[]))])],
            Setup::PLAIN,
            limit(Limit::Depth),
        ),
        // Each byte allocated is work (the review of B1, F1): a megabyte is 16,384 steps.
        (doubling.clone(), Setup::PLAIN, limit(Limit::Steps)),
        (doubling, Setup::ROOMY, limit(Limit::Memory)),
        (
            vec![method("M0", 0, &[ret(buffer(dword(u32::MAX), &[]))])],
            Setup::PLAIN,
            limit(Limit::Memory),
        ),
        (
            vec![method("M0", 0, &[ret(var_package(qword(u64::MAX), &[]))])],
            Setup::PLAIN,
            limit(Limit::Memory),
        ),
        // Operands nested 5,000 deep: the engine does not recurse, so the frames' work ends it,
        // or with room for that work, their memory.
        (
            vec![name("P", deep_pkg.clone())],
            Setup::PLAIN,
            Expect::Load(ErrorKind::Limit(Limit::Steps)),
        ),
        (
            vec![name("P", deep_pkg)],
            Setup::ROOMY,
            Expect::Load(ErrorKind::Limit(Limit::Memory)),
        ),
        // 4,096 devices in one scope, each name compared with those before it: the comparisons'
        // work ends it, or with room for that work the 4,097th collides, and all must go.
        (
            many.clone(),
            Setup::PLAIN,
            Expect::Load(ErrorKind::Limit(Limit::Steps)),
        ),
        (many, Setup::ROOMY, Expect::Load(ErrorKind::Duplicate)),
        // A package length that claims past the end of the block.
        (
            vec![vec![0x10, 0xFF, 0xFF, 0xFF, 0x0F]],
            Setup::PLAIN,
            Expect::Load(ErrorKind::PackageLength),
        ),
        // A package length in four bytes where one would do: legal.
        (
            vec![pkg_min(&[0x14], cat2(nm("M0"), vec![0]), 4)],
            Setup::PLAIN,
            Expect::Eval(Ok(None)),
        ),
        (
            vec![method("M0", 0, &[while_(one(), &[name("X", one())])])],
            Setup::PLAIN,
            Expect::Eval(Err(ErrorKind::Duplicate)),
        ),
        // A name a method makes goes when it returns.
        (
            vec![method("M0", 0, &[name("X", one()), ret(nm("X"))])],
            Setup::PLAIN,
            Expect::Eval(Ok(Some(Value::Integer(1)))),
        ),
        // A local holding a reference to itself: what is returned is that reference, to a local
        // gone once the method returns.
        (
            vec![method(
                "M0",
                0,
                &[
                    store(ref_of(local(0)), local(0)),
                    ret(deref_of(deref_of(local(0)))),
                ],
            )],
            Setup::PLAIN,
            Expect::Eval(Err(ErrorKind::Stale)),
        ),
        (
            vec![
                name("PK", package(1, &[zero()])),
                method(
                    "M0",
                    0,
                    &[
                        store(nm("PK"), index(nm("PK"), zero(), none())),
                        ret(nm("PK")),
                    ],
                ),
            ],
            Setup::PLAIN,
            Expect::Eval(Ok(Some(Value::Package(vec![Value::Package(vec![
                Value::Integer(0),
            ])])))),
        ),
        (
            vec![method(
                "M0",
                0,
                &[
                    store(
                        ref_of(local(0)),
                        index(var_package(byte(1), &[]), zero(), local(1)),
                    ),
                    ret(local(1)),
                ],
            )],
            Setup::PLAIN,
            Expect::Eval(Err(ErrorKind::Stale)),
        ),
        // A buffer field may not end past its buffer, though it is never read.
        (
            vec![method(
                "M0",
                0,
                &[
                    create_byte_field(buffer(byte(4), &[]), byte(4), "X"),
                    ret(one()),
                ],
            )],
            Setup::PLAIN,
            Expect::Eval(Err(ErrorKind::Range)),
        ),
        // A field of no bits written with a buffer and a string larger than it: nothing to write.
        // Found by the soak of 2026-11-14 (mutation seed 0x9cc4f803), where the piece-by-piece
        // write advanced by the field's width, 0, for ever.
        (
            vec![
                op_region("ZR", 1, word(0x80), byte(1)),
                field("ZR", 1, &[Fe::Named("ZW", 0), Fe::Named("ZB", 8)]),
                method(
                    "M0",
                    0,
                    &[
                        store(buffer(byte(4), &[1, 2, 3, 4]), nm("ZW")),
                        store(string("abcd"), nm("ZW")),
                    ],
                ),
            ],
            Setup::PLAIN,
            Expect::Eval(Ok(None)),
        ),
        // An evaluation that ends holding the Global Lock fails, and the lock goes back.
        (
            vec![method("M0", 0, &[acquire(nm("\\_GL"), 0xFFFF), ret(one())])],
            Setup::PLAIN,
            Expect::Eval(Err(ErrorKind::MutexHeld)),
        ),
    ];
    for (k, (body, setup, expect)) in cases.iter().enumerate() {
        watch(ADVERSARIAL, k as u64, k as u64);
        let aml = body.concat();
        for width in [IntegerWidth::Bits64, IntegerWidth::Bits32] {
            let r = run(&aml, width, k as u64, *setup);
            let got = match &r.load {
                Err(e) => Expect::Load(*e),
                Ok(()) => Expect::Eval(r.evals.first().cloned().unwrap_or(Ok(None))),
            };
            assert!(
                got.is(expect),
                "adversarial block {k}, {width:?}: {got:?}, written for {expect:?}"
            );
        }
    }
}

#[test]
fn adversarial_blocks() {
    adversarial_cases();
    take_stats().print("adversarial");
}

fn cat2(a: B, b: B) -> B {
    let mut v = a;
    v.extend(b);
    v
}

/// The planted faults (the contract's §7; the review of B1, F4): with each, the tests above must
/// fail, at the check named; without any, they must pass.
#[test]
fn planted_faults_are_caught() {
    fn outcome(f: Option<Fault>) -> Option<String> {
        plant(f);
        let r = std::panic::catch_unwind(|| {
            grammar_cases(first_seed(), 300);
            mutation_cases(first_seed() ^ 0xFFFF_0000, 300);
            adversarial_cases();
        });
        plant(None);
        take_stats();
        r.err().map(|e| {
            e.downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_string()))
                .unwrap_or_default()
        })
    }
    assert_eq!(outcome(None), None, "the controls' cases fail unplanted");
    let faults = [
        (Fault::SkipRollback, "a failed load left names behind"),
        (Fault::KeepDynamicNames, "an evaluation left names behind"),
        (Fault::NoLoopLimit, "Limit(Loops)"),
        (Fault::NoTimeLimit, "Limit(Time)"),
        (Fault::LooseMemoryLimit, "memory passed the limit"),
        (Fault::WeakFieldBound, "Err(Range)"),
        (Fault::KeepGlobalLock, "the Global Lock was kept"),
    ];
    for (f, by) in faults {
        let message = outcome(Some(f));
        assert!(
            message.as_deref().is_some_and(|m| m.contains(by)),
            "{f:?} planted: expected a failure naming {by:?}, got {message:?}"
        );
    }
}

/// The block the mutation test made from seed `0x9cc4f803`, the case on which the soak of
/// 2026-11-14 (seed `0x63400000`, case 391,171) hung: its bytes as they were, so that the case
/// stays the same whatever the generator becomes. It stores a buffer to a field of no bits.
const SOAK_HANG_0X9CC4F803: [&str; 14] = [
    "084e305f5f0e70ca05ef307e9223084e315f5f0d6e31000842465f5f11070a100102030408504b5f5f121004",
    "010d78005c4e305f5f5c44305f5f5b824a0a44305f5f085f4849440c41d00c0a1449094d335f5f0170120301",
    "0160700e34a6ee2d188a1d00617011040a0501627011030a0063700eb2d19f39cced3c00647011050a020102",
    "65700d610066700cb52e4a04670854305f5f0a02a24b049575670a24a02f1111930e2e1d4ca7720000001103",
    "0a020908a35b217042445f5f46305f5f864e305f5f5c2e44305f5f4d335f5f68a1099f0854305f5f0a035b22",
    "0bdc055b220b0000a4685b8052305f5f010b80000a085b811052305f5f0146305f5f0046315f5f085b810d52",
    "305f5f1300204c4b5f5f205b861146305f5f46315f5f01000849585f5f0c5b014d585f5f035b0245565f5f8a",
    "42465f5f0042445f5f065c4e305f5f5c4e325f5f1449054d305f5f007011030a0160700edda1d67a567d0200",
    "617012020162700a0f6370120602010d700065700e15354bdad9490e00667011060a0301020367a3a4809eff",
    "0d57696e646f77732032303232005b3046305f5f5b31144a044d315f5f017012020160700e68577f08060000",
    "00617011040a0401637011030a0464700e528bc067c7000000657012020166700d5c4e300067a4939446305f",
    "5f5b309d12030101681444064d325f5f0a700ea1b9d33ca28c640061700e67ad99d95dcae90362700cc1d4c0",
    "04637011030a0464700e6586e35209000000657011050a00010266700efc817604b1d11e006775655b244556",
    "5f5fa470120602010d7000697561a411067066690908",
];

#[test]
fn the_soaks_hanging_case_ends() {
    // N-soak-1 of the review of B2's Resolution.
    let hex: String = SOAK_HANG_0X9CC4F803.concat();
    let aml: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|k| u8::from_str_radix(&hex[k..k + 2], 16).unwrap())
        .collect();
    assert_eq!(aml.len(), 594);
    let seed = 0x9cc4_f803;
    ends_within(10, "seed 0x9cc4f803", move || {
        run(&aml, IntegerWidth::Bits32, seed, Setup::of(seed));
    });
}
