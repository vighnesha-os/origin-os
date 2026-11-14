//! The randomized hostile-input test (the contract's §7): definition blocks made at random from
//! the grammar, the same blocks mutated byte by byte, and blocks written to reach each limit. No
//! third-party crate: the generator is SplitMix64, as the repository's other models use.
//!
//! Each case checks that the interpreter returns (it never panics or hangs), that its memory
//! never passes the limit, that a failed load leaves the namespace as it was, that an evaluation
//! leaves no dynamic name behind, and that the same seed gives the same results.
//!
//! `ORIGIN_AML_CASES` sets the number of cases of each kind (default 2,000) and `ORIGIN_AML_SEED`
//! the first seed, for longer soaks.

use super::encode::*;
use super::{TestHost, interp_with};
use crate::object::Object;
use crate::{DefinitionBlock, IntegerWidth, Interpreter, Limits, Node, Value};

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

fn limits() -> Limits {
    Limits {
        loop_iterations: 2_000,
        time_100ns: u64::MAX,
        call_depth: 16,
        memory_bytes: 2 << 20,
        steps: 20_000,
    }
}

const NAMES: &[&str] = &[
    "N0", "N1", "N2", "N3", "BF", "ST", "PK", "D0", "D1", "M0", "M1", "M2", "F0", "R0",
];
const METHODS: &[(&str, u8)] = &[("M0", 0), ("M1", 1), ("M2", 2), ("\\D0.M3", 1)];

struct Gen {
    rng: Rng,
    /// The arguments the method being made takes.
    argc: u8,
}

impl Gen {
    fn expr(&mut self, depth: u32) -> B {
        let r = &mut self.rng;
        if depth == 0 || r.chance(30) {
            return match r.below(9) {
                0 => int(r.next() >> r.below(64)),
                1 => ones(),
                2 => string(r.pick(&["", "a", "0x10", "12", "Windows 2022", "\\N0"])),
                3 => buffer(byte(r.below(6) as u8), &[1, 2, 3][..r.below(4) as usize]),
                4 => local(r.below(8) as u8),
                5 if self.argc > 0 => arg(r.below(u64::from(self.argc)) as u8),
                5 => local(r.below(8) as u8),
                6 => nm(r.pick(NAMES)),
                7 => revision(),
                _ => package(
                    r.below(4) as u8,
                    &[one(), string("p")][..r.below(3) as usize],
                ),
            };
        }
        let d = depth - 1;
        match self.rng.below(30) {
            0 => add(self.expr(d), self.expr(d), self.target()),
            1 => bin(
                0x72 + self.rng.below(14) as u8,
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
            _ => nm(self.rng.pick(NAMES)),
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
            8 => while_(
                logic(0x95, increment(local(7)), byte(self.rng.below(40) as u8)),
                &self.stmts(d),
            ),
            9 => {
                if self.rng.chance(50) {
                    brk()
                } else {
                    cont()
                }
            }
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
            14 => sleep(byte(self.rng.below(4) as u8)),
            15 => stall(self.expr(1)),
            16 => notify(nm(self.rng.pick(&["D0", "N0"])), self.expr(1)),
            _ => device(self.rng.pick(&["DD0", "DD1"]), &[name("X", one())]),
        }
    }

    /// A method body: most locals set first, then statements.
    fn body(&mut self, argc: u8) -> Vec<B> {
        self.argc = argc;
        let mut out = Vec::new();
        for k in 0..8u8 {
            if self.rng.chance(70) {
                let v = self.expr(0);
                out.push(store(v, local(k)));
            }
        }
        out.extend(self.stmts(3));
        if self.rng.chance(60) {
            out.push(ret(self.expr(2)));
        }
        self.argc = 0;
        out
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
            body.push(if_(self.expr(2), &[device("\\D1", &[])]));
        }
        body
    }
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

/// What a run of cases did, printed when `ORIGIN_AML_STATS` is set.
#[derive(Default)]
struct Stats {
    cases: u64,
    loaded: u64,
    evaluations: u64,
    values: u64,
    errors: std::collections::BTreeMap<String, u64>,
}

impl Stats {
    fn print(&self, what: &str) {
        if std::env::var_os("ORIGIN_AML_STATS").is_some() {
            eprintln!(
                "{what}: {} cases, {} loaded, {} evaluations, {} returned a value; errors: {:?}",
                self.cases, self.loaded, self.evaluations, self.values, self.errors
            );
        }
    }
}

thread_local! {
    static STATS: std::cell::RefCell<Stats> = std::cell::RefCell::new(Stats::default());
}

fn note(f: impl FnOnce(&mut Stats)) {
    STATS.with(|s| f(&mut s.borrow_mut()));
}

fn note_error(e: &crate::Error) {
    let key = match e.kind {
        crate::ErrorKind::Reserved { .. } => "Reserved".to_string(),
        k => format!("{k:?}"),
    };
    note(|s| *s.errors.entry(key).or_insert(0) += 1);
}

/// Runs one case: loads `aml`, then evaluates every method with random arguments. Returns a
/// digest of what happened, for the determinism check.
fn run(aml: &[u8], width: IntegerWidth, seed: u64) -> u64 {
    note(|s| s.cases += 1);
    let lim = limits();
    let mut i = interp_with(width, lim);
    i.host_mut().tick = 1;
    // A third of the cases run against a host that refuses every region, as the boot proof's.
    i.host_mut().refuse = seed.is_multiple_of(3);
    let before = snapshot(&i);
    let block = DefinitionBlock {
        signature: *b"SSDT",
        oem_id: *b"ORIGIN",
        oem_table_id: *b"RANDOM  ",
        aml,
    };
    let mut digest: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |x: u64| digest = (digest ^ x).wrapping_mul(0x100_0000_01b3);
    match i.load(block) {
        Ok(_) => {
            note(|s| s.loaded += 1);
            mix(1)
        }
        Err(e) => {
            note_error(&e);
            mix(2 + e.at.map_or(0, |a| u64::from(a.offset)));
            assert_eq!(
                snapshot(&i),
                before,
                "seed {seed:#x}: a failed load left names behind"
            );
        }
    }
    assert!(i.memory_peak() <= lim.memory_bytes, "seed {seed:#x}");
    assert_eq!(
        i.host().global_held,
        0,
        "seed {seed:#x}: the Global Lock was kept"
    );
    let after_load = i.node_count();
    let mut rng = Rng(seed ^ 0x5555);
    for (node, argc) in methods(&i) {
        let args = random_args(&mut rng, argc);
        let r = i.evaluate_node(node, &args);
        note(|s| s.evaluations += 1);
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
        assert!(i.memory_peak() <= lim.memory_bytes, "seed {seed:#x}");
    }
    digest
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
    };
    let width = if g.rng.chance(30) {
        IntegerWidth::Bits32
    } else {
        IntegerWidth::Bits64
    };
    (g.table().concat(), width)
}

#[test]
fn random_blocks_from_the_grammar() {
    let start = first_seed();
    for k in 0..cases() {
        let seed = start.wrapping_add(k);
        let (aml, width) = generated(seed);
        let a = run(&aml, width, seed);
        if k % 97 == 0 {
            assert_eq!(
                a,
                run(&aml, width, seed),
                "seed {seed:#x}: not deterministic"
            );
        }
    }
    STATS.with(|s| s.borrow().print("grammar"));
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

#[test]
fn mutated_blocks() {
    let start = first_seed() ^ 0xFFFF_0000;
    for k in 0..cases() {
        let seed = start.wrapping_add(k);
        let (mut aml, width) = generated(seed);
        let mut rng = Rng(seed);
        mutate(&mut rng, &mut aml);
        run(&aml, width, seed);
    }
    STATS.with(|s| s.borrow().print("mutation"));
}

#[test]
fn adversarial_blocks() {
    let mut deep_pkg = package(1, &[one()]);
    for _ in 0..5_000 {
        deep_pkg = package(1, &[deep_pkg]);
    }
    let many: Vec<B> = (0..20_000)
        .map(|k| device(&format!("\\Z{:03X}", k % 4096), &[]))
        .collect();
    let blocks: Vec<Vec<B>> = vec![
        vec![method("M0", 0, &[while_(one(), &[noop()])])],
        vec![method("M0", 0, &[ret(call("M0", &[]))])],
        vec![method(
            "M0",
            0,
            &[
                store(string("ab"), local(0)),
                while_(one(), &[concat(local(0), local(0), local(0))]),
            ],
        )],
        vec![method("M0", 0, &[ret(buffer(dword(u32::MAX), &[]))])],
        vec![method("M0", 0, &[ret(var_package(qword(u64::MAX), &[]))])],
        vec![name("P", deep_pkg)],
        many,
        // A package length that claims past the end of the block.
        vec![vec![0x10, 0xFF, 0xFF, 0xFF, 0x0F]],
        vec![pkg_min(&[0x14], cat2(nm("M0"), vec![0]), 4)],
        vec![method("M0", 0, &[while_(one(), &[name("X", one())])])],
        vec![method(
            "M0",
            0,
            &[
                store(ref_of(local(0)), local(0)),
                ret(deref_of(deref_of(local(0)))),
            ],
        )],
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
    ];
    for (k, body) in blocks.iter().enumerate() {
        let aml = body.concat();
        run(&aml, IntegerWidth::Bits64, k as u64);
        run(&aml, IntegerWidth::Bits32, k as u64);
    }
    STATS.with(|s| s.borrow().print("adversarial"));
}

fn cat2(a: B, b: B) -> B {
    let mut v = a;
    v.extend(b);
    v
}
