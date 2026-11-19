//! Step B5: the load policy (G1, G3; B4's Q3 and Q7) and the binding of names in load-time
//! packages (G1), with the reports each gives (the coordinating session's rulings of 2026-11-19 on
//! TASKS 64's ledger; B5's contract, accepted the same day). Every test asserts its reports
//! exactly.

use super::encode::*;
use super::{Fault, Interp, Told, eval, int, interp, interp_with, load, plant, told};
use crate::{ErrorKind, IntegerWidth, Limits, TableKey, Value};

fn fresh() -> Interp {
    interp(IntegerWidth::Bits64)
}

fn exists(i: &Interp, p: &str) -> bool {
    i.lookup(p).ok().flatten().is_some()
}

/// Where statement `k` of `body` begins in its term list.
fn at(body: &[B], k: usize) -> u32 {
    body[..k].iter().map(Vec::len).sum::<usize>() as u32
}

/// The report of statement `k` of `body`, in table `table`, abandoned with `kind`.
fn abandoned(body: &[B], k: usize, table: u32, kind: ErrorKind) -> Told {
    Told::Abandoned {
        kind,
        table,
        statement: at(body, k),
        resumed: at(body, k + 1),
    }
}

fn ssdt(body: &[B]) -> Vec<u8> {
    table(b"SSDT", 2, body)
}

fn buffer_of(bytes: &[u8]) -> B {
    buffer(dword(bytes.len() as u32), bytes)
}

/// Nothing is left on the engine's stacks.
fn at_rest(i: &Interp) {
    assert!(i.frames.is_empty() && i.calls.is_empty() && i.owners.is_empty());
    assert!(i.held.is_empty());
    assert_eq!(i.loading, 0);
}

// --- The load policy ---------------------------------------------------------------------------

#[test]
fn unresolved_name_abandons_its_statement() {
    let body = [
        name("X", zero()),
        store(nm("UNDF"), nm("X")),
        name("AFT", one()),
    ];
    let mut i = fresh();
    assert_eq!(load(&mut i, &body), Ok(0));
    assert_eq!(
        told(&mut i),
        vec![abandoned(&body, 1, 0, ErrorKind::NotFound)]
    );
    assert_eq!(int(&mut i, "X"), 0);
    assert!(exists(&i, "\\AFT"));
    let t = i.table(0).unwrap();
    assert!(!t.failed);
    assert_eq!(t.abandoned, 1);
    at_rest(&i);
}

#[test]
fn forward_call_bytes_after_the_name_load_as_statements() {
    // `FOO (One, 2)` with `FOO` defined further on: at table level the name resolves to nothing,
    // so it is a plain name taking no arguments, abandoned alone; the bytes meant as its
    // arguments then load as statements (G1). In a method the same call takes its arguments.
    let body = [
        name("R0", zero()),
        call("FOO", &[one(), byte(2)]),
        method("FOO", 2, &[store(arg(0), nm("\\R0"))]),
        name("AFT", one()),
    ];
    let mut i = fresh();
    assert_eq!(load(&mut i, &body), Ok(0));
    let start = at(&body, 1);
    assert_eq!(
        told(&mut i),
        vec![Told::Abandoned {
            kind: ErrorKind::NotFound,
            table: 0,
            statement: start,
            resumed: start + 4,
        }]
    );
    assert_eq!(int(&mut i, "R0"), 0, "not called at load time");
    assert!(exists(&i, "\\AFT"));
    i.evaluate("\\FOO", &[Value::Integer(5), Value::Integer(6)])
        .unwrap();
    assert_eq!(int(&mut i, "R0"), 5);
    at_rest(&i);
}

#[test]
fn operand_failure_skips_the_whole_statement() {
    // `Store (UNDF, M)`, `M` a method that would mark `MARK`: the statement is skipped whole, so
    // `M` is never called (Q1: Origin's choice).
    let body = [
        name("MARK", zero()),
        method(
            "M",
            0,
            &[store(one(), nm("\\MARK")), ret(ref_of(nm("\\MARK")))],
        ),
        store(nm("UNDF"), nm("M")),
        name("AFT", one()),
    ];
    let mut i = fresh();
    assert_eq!(load(&mut i, &body), Ok(0));
    assert_eq!(
        told(&mut i),
        vec![abandoned(&body, 2, 0, ErrorKind::NotFound)]
    );
    assert_eq!(int(&mut i, "MARK"), 0, "the skip ran nothing");
    assert!(exists(&i, "\\AFT"));
}

#[test]
fn failed_scope_opener_skips_its_body() {
    let body = [
        scope("UNDF", &[name("IN1", one())]),
        device("D", &[name("C1", one())]),
        device("D", &[name("C2", one())]),
        name("AFT", one()),
    ];
    let mut i = fresh();
    assert_eq!(load(&mut i, &body), Ok(0));
    assert_eq!(
        told(&mut i),
        vec![
            abandoned(&body, 0, 0, ErrorKind::NotFound),
            abandoned(&body, 2, 0, ErrorKind::Duplicate),
        ]
    );
    assert!(!exists(&i, "\\IN1"));
    assert!(exists(&i, "\\D.C1"));
    assert!(!exists(&i, "\\D.C2"));
    assert!(exists(&i, "\\AFT"));
    assert_eq!(i.table(0).unwrap().abandoned, 2);
}

#[test]
fn failed_predicate_skips_if_and_else() {
    let body = [
        name("A", zero()),
        if_else(
            nm("UNDF"),
            &[store(one(), nm("A"))],
            &[store(byte(2), nm("A"))],
        ),
        name("AFT", one()),
    ];
    let mut i = fresh();
    assert_eq!(load(&mut i, &body), Ok(0));
    assert_eq!(
        told(&mut i),
        vec![abandoned(&body, 1, 0, ErrorKind::NotFound)]
    );
    assert_eq!(int(&mut i, "A"), 0, "neither body ran");
    assert!(exists(&i, "\\AFT"));
}

#[test]
fn failed_predicate_skips_the_while_on_its_third_pass() {
    // The predicate reads `PKG`'s element `N`: on the third pass `N` is 2, past the package, and
    // the predicate fails (`Range`). The whole `While` is the statement abandoned.
    let body = [
        name("PKG", package(2, &[one(), one()])),
        name("N", zero()),
        name("RUNS", zero()),
        while_(
            logic(0x95, deref_of(index(nm("PKG"), nm("N"), zero())), byte(10)),
            &[increment(nm("N")), increment(nm("RUNS"))],
        ),
        name("AFT", one()),
    ];
    let mut i = fresh();
    assert_eq!(load(&mut i, &body), Ok(0));
    let t = told(&mut i);
    assert_eq!(t.len(), 1);
    assert!(
        matches!(t[0], Told::Abandoned { statement, resumed, .. }
            if statement == at(&body, 3) && resumed == at(&body, 4)),
        "{t:?}"
    );
    assert_eq!(int(&mut i, "RUNS"), 2);
    assert!(exists(&i, "\\AFT"));
    at_rest(&i);
}

#[test]
fn statement_in_a_body_fails_alone() {
    let inner = [
        name("C1", one()),
        store(nm("UNDF"), nm("C1")),
        name("C2", one()),
    ];
    let body = [device("D", &inner)];
    let mut i = fresh();
    assert_eq!(load(&mut i, &body), Ok(0));
    let t = told(&mut i);
    assert!(
        matches!(t.as_slice(), [Told::Abandoned { kind: ErrorKind::NotFound, resumed, statement, .. }]
            if resumed - statement == inner[1].len() as u32),
        "{t:?}"
    );
    assert!(exists(&i, "\\D.C1"));
    assert!(exists(&i, "\\D.C2"));
}

#[test]
fn method_error_abandons_its_calling_statement() {
    let body = [
        method(
            "BAD",
            0,
            &[name("TMP", one()), store(nm("\\UNDF"), local(0))],
        ),
        call("BAD", &[]),
        name("AFT", one()),
    ];
    let mut i = fresh();
    assert_eq!(load(&mut i, &body), Ok(0));
    assert_eq!(
        told(&mut i),
        vec![abandoned(&body, 1, 0, ErrorKind::NotFound)]
    );
    assert!(!exists(&i, "\\BAD.TMP"), "the method's names went");
    assert!(exists(&i, "\\AFT"));
    at_rest(&i);
}

#[test]
fn duplicate_keeps_the_first_object() {
    let mut i = fresh();
    assert_eq!(load(&mut i, &[name("X", byte(1))]), Ok(0));
    let second = [name("X", byte(2)), name("Y", one())];
    assert_eq!(load(&mut i, &second), Ok(1));
    assert_eq!(
        told(&mut i),
        vec![abandoned(&second, 0, 1, ErrorKind::Duplicate)]
    );
    assert_eq!(int(&mut i, "X"), 1);
    assert!(exists(&i, "\\Y"));
    assert!(!i.table(0).unwrap().failed && !i.table(1).unwrap().failed);
}

#[test]
fn scope_over_an_integer_stays_refused() {
    let body = [
        name("V", one()),
        scope("V", &[name("IN", one())]),
        name("AFT", one()),
    ];
    let mut i = fresh();
    assert_eq!(load(&mut i, &body), Ok(0));
    assert_eq!(
        told(&mut i),
        vec![abandoned(&body, 1, 0, ErrorKind::WrongObject)]
    );
    assert!(!exists(&i, "\\V.IN"));
}

#[test]
fn structural_failure_still_rolls_back() {
    // A reserved opcode, a package length past its container, and a limit: the table fails whole,
    // as before step B5, and nothing is reported.
    for (body, kind) in [
        (
            vec![name("A", one()), vec![0x02]],
            ErrorKind::Reserved {
                first: 0x02,
                second: None,
            },
        ),
        (
            vec![name("A", one()), vec![0x10, 0x20, b'X', b'_', b'_', b'_']],
            ErrorKind::PackageLength,
        ),
    ] {
        let mut i = fresh();
        assert_eq!(load(&mut i, &body).map_err(|e| e.kind), Err(kind));
        assert!(!exists(&i, "\\A"));
        assert!(i.table(0).unwrap().failed);
        assert_eq!(told(&mut i), vec![]);
        at_rest(&i);
    }
}

#[test]
fn abandon_and_skip_are_bounded() {
    // 10,000 failing statements: under the service's limits they load, each reported up to the
    // bound and the rest counted; under a step limit they meet it cleanly, the table rolled back.
    let mut body = vec![name("X", zero())];
    body.extend((0..10_000).map(|_| store(nm("UNDF"), nm("X"))));
    body.push(name("AFT", one()));
    let mut i = fresh();
    assert_eq!(load(&mut i, &body), Ok(0));
    let t = told(&mut i);
    assert_eq!(t.len(), 65);
    assert_eq!(t[64], Told::Suppressed(10_000 - 64));
    assert_eq!(i.table(0).unwrap().abandoned, 10_000);
    assert!(exists(&i, "\\AFT"));
    let mut i = interp_with(
        IntegerWidth::Bits64,
        Limits {
            steps: 20_000,
            ..Limits::SERVICE
        },
    );
    let e = load(&mut i, &body).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Limit(crate::Limit::Steps));
    assert!(!exists(&i, "\\X"));
    at_rest(&i);
}

#[test]
fn the_skip_asks_the_host_nothing_and_reaches_no_fatal() {
    // A statement abandoned at its first operand, whose rest holds a `Fatal`, a `Stall`, a
    // `Sleep`, a `Notify`, a region field's read and a method call: none of them happens.
    let body = [
        name("MARK", zero()),
        op_region("RG", 1, word(0x80), byte(1)),
        field("RG", 1, &[Fe::Named("F8", 8)]),
        device("DEV", &[]),
        method("M", 0, &[store(one(), nm("\\MARK")), ret(one())]),
        store(
            add(
                nm("UNDF"),
                add(nm("F8"), add(call("M", &[]), zero(), zero()), zero()),
                zero(),
            ),
            nm("MARK"),
        ),
        if_(
            nm("UNDF"),
            &[fatal(1, 2, one()), stall(byte(5)), sleep(one())],
        ),
        if_(nm("UNDF"), &[notify(nm("DEV"), one())]),
        name("AFT", one()),
    ];
    let mut i = fresh();
    assert_eq!(load(&mut i, &body), Ok(0));
    assert_eq!(told(&mut i).len(), 3);
    assert_eq!(int(&mut i, "MARK"), 0);
    let h = i.host();
    assert!(h.seen.is_empty(), "no region access: {:?}", h.seen);
    assert!(h.fatals.is_empty() && h.notified.is_empty());
    assert!(exists(&i, "\\AFT"));
}

#[test]
fn already_loaded_is_refused() {
    // A boot load of an identical block returns `Duplicate`; `Load` of the same bytes twice, and
    // `LoadTable` of the same key twice, give Zero the second time. Nothing is created, and each
    // refusal is reported.
    let mut i = fresh();
    let body = [name("X", one())];
    assert_eq!(load(&mut i, &body), Ok(0));
    assert_eq!(
        load(&mut i, &body).map_err(|e| e.kind),
        Err(ErrorKind::Duplicate)
    );
    assert_eq!(told(&mut i), vec![Told::AlreadyLoaded(0)]);
    assert_eq!(i.table_count(), 1);

    // `Load` takes an SSDT (B4's Q2); `LoadTable` of an SSDT key gives Zero without asking the
    // host (Q6), so its table is signed `OEM1`.
    let t = ssdt(&[name("QX", one())]);
    let u = table(b"OEM1", 2, &[name("QZ", one())]);
    let mut i = fresh();
    i.host_mut().tables.push((
        TableKey {
            signature: *b"OEM1",
            oem_id: *b"ORIGIN",
            oem_table_id: *b"AMLTEST ",
        },
        0x1000,
        u,
    ));
    load(
        &mut i,
        &[
            name("TM", buffer_of(&t)),
            method("LD", 0, &[ret(load_op("TM", zero()))]),
            method(
                "LT",
                0,
                &[ret(load_table([
                    string("OEM1"),
                    string("ORIGIN"),
                    string("AMLTEST "),
                    string(""),
                    string(""),
                    zero(),
                ]))],
            ),
        ],
    )
    .unwrap();
    let _ = told(&mut i);
    assert_eq!(int(&mut i, "LD"), u64::MAX);
    let tables = i.table_count();
    assert_eq!(int(&mut i, "LD"), 0);
    assert_eq!(told(&mut i), vec![Told::AlreadyLoaded(1)]);
    assert_eq!(i.table_count(), tables);
    assert_eq!(int(&mut i, "LT"), u64::MAX);
    assert_eq!(int(&mut i, "LT"), 0, "the same table again, by key");
    assert_eq!(told(&mut i), vec![Told::AlreadyLoaded(2)]);
    assert_eq!(i.table_count(), tables + 1);
}

#[test]
fn loaded_table_with_abandoned_statements_gives_true() {
    let inner = [
        name("Q1", one()),
        store(nm("UNDF"), nm("Q1")),
        name("Q2", one()),
    ];
    let t = ssdt(&inner);
    let mut i = fresh();
    load(
        &mut i,
        &[
            name("TM", buffer_of(&t)),
            method("LD", 0, &[ret(load_op("TM", zero()))]),
        ],
    )
    .unwrap();
    assert_eq!(int(&mut i, "LD"), u64::MAX);
    assert_eq!(
        told(&mut i),
        vec![abandoned(&inner, 1, 1, ErrorKind::NotFound)]
    );
    assert!(exists(&i, "\\Q1") && exists(&i, "\\Q2"));
    assert_eq!(i.table(1).unwrap().abandoned, 1);
    at_rest(&i);
}

// --- Names in load-time packages -------------------------------------------------------------

#[test]
fn forward_name_in_a_package_binds_after_all_tables() {
    let mut i = fresh();
    load(&mut i, &[name("PKG", package(1, &[nm("\\_SB.LNKB")]))]).unwrap();
    load(&mut i, &[scope("\\_SB", &[device("LNKB", &[])])]).unwrap();
    assert_eq!(i.bind_deferred(), Ok(0));
    assert_eq!(told(&mut i), vec![]);
    let lnkb = i.lookup("\\_SB.LNKB").unwrap().unwrap();
    assert_eq!(
        eval(&mut i, "PKG"),
        Ok(Some(Value::Package(vec![Value::Reference(lnkb)])))
    );
}

#[test]
fn unbound_element_is_empty_and_reported() {
    let body = [name("PKG", package(2, &[nm("\\NONE"), one()]))];
    let mut i = fresh();
    load(&mut i, &body).unwrap();
    // Before binding, the name is looked up when read, and names nothing.
    assert_eq!(
        eval(&mut i, "PKG").map_err(|e| e.kind),
        Err(ErrorKind::NotFound)
    );
    assert_eq!(i.bind_deferred(), Ok(1));
    // `Name` (1), its name (4), `Package` (1), its length (1), its count (1): the element at 8.
    assert_eq!(told(&mut i), vec![Told::Unbound(8)]);
    assert_eq!(
        eval(&mut i, "PKG"),
        Ok(Some(Value::Package(vec![
            Value::Uninitialized,
            Value::Integer(1)
        ])))
    );
    // Bound once: a second pass binds nothing and reports nothing.
    assert_eq!(i.bind_deferred(), Ok(0));
    assert_eq!(told(&mut i), vec![]);
}

#[test]
fn initialize_binds_first() {
    let mut i = fresh();
    load(&mut i, &[name("PKG", package(1, &[nm("\\NONE")]))]).unwrap();
    i.initialize(&mut |_, _| {}).unwrap();
    assert_eq!(told(&mut i), vec![Told::Unbound(8)]);
}

#[test]
fn binding_happens_once() {
    // `A` is found from `\_SB` by the upward search: `\A`. Bound then, it stays `\A` when a later
    // table makes a nearer `\_SB.A`.
    let mut i = fresh();
    load(
        &mut i,
        &[
            name("A", byte(1)),
            scope("\\_SB", &[name("PKG", package(1, &[nm("A")]))]),
        ],
    )
    .unwrap();
    assert_eq!(i.bind_deferred(), Ok(0));
    load(&mut i, &[scope("\\_SB", &[name("A", byte(2))])]).unwrap();
    assert_eq!(
        eval(&mut i, "\\_SB.PKG"),
        Ok(Some(Value::Package(vec![Value::Integer(1)])))
    );
}

#[test]
fn runtime_load_binds_its_own() {
    let t = ssdt(&[name("PK2", package(1, &[nm("\\NONE")]))]);
    let mut i = fresh();
    load(
        &mut i,
        &[
            name("TM", buffer_of(&t)),
            method("LD", 0, &[ret(load_op("TM", zero()))]),
        ],
    )
    .unwrap();
    assert_eq!(int(&mut i, "LD"), u64::MAX);
    assert_eq!(told(&mut i), vec![Told::Unbound(8)]);
    assert_eq!(
        eval(&mut i, "PK2"),
        Ok(Some(Value::Package(vec![Value::Uninitialized])))
    );
}

#[test]
fn method_packages_resolve_when_read() {
    let mut i = fresh();
    load(
        &mut i,
        &[method("MP", 0, &[ret(package(1, &[nm("\\LATE")]))])],
    )
    .unwrap();
    assert_eq!(i.bind_deferred(), Ok(0));
    load(&mut i, &[name("LATE", byte(7))]).unwrap();
    assert_eq!(
        eval(&mut i, "MP"),
        Ok(Some(Value::Package(vec![Value::Integer(7)])))
    );
    assert_eq!(told(&mut i), vec![]);
}

// --- The controls ----------------------------------------------------------------------------

#[test]
fn planted_faults_are_caught_by_the_tolerance_tests() {
    let tests: [(Option<Fault>, &[fn()]); 4] = [
        (
            None,
            &[
                unresolved_name_abandons_its_statement,
                forward_call_bytes_after_the_name_load_as_statements,
                operand_failure_skips_the_whole_statement,
                failed_scope_opener_skips_its_body,
                failed_predicate_skips_if_and_else,
                failed_predicate_skips_the_while_on_its_third_pass,
                statement_in_a_body_fails_alone,
                method_error_abandons_its_calling_statement,
                duplicate_keeps_the_first_object,
                scope_over_an_integer_stays_refused,
                abandon_and_skip_are_bounded,
                the_skip_asks_the_host_nothing_and_reaches_no_fatal,
                already_loaded_is_refused,
                loaded_table_with_abandoned_statements_gives_true,
                unbound_element_is_empty_and_reported,
                binding_happens_once,
                runtime_load_binds_its_own,
            ],
        ),
        (
            Some(Fault::NoAbandon),
            &[
                unresolved_name_abandons_its_statement,
                forward_call_bytes_after_the_name_load_as_statements,
                operand_failure_skips_the_whole_statement,
                failed_scope_opener_skips_its_body,
                failed_predicate_skips_if_and_else,
                failed_predicate_skips_the_while_on_its_third_pass,
                statement_in_a_body_fails_alone,
                method_error_abandons_its_calling_statement,
                duplicate_keeps_the_first_object,
                scope_over_an_integer_stays_refused,
                abandon_and_skip_are_bounded,
                the_skip_asks_the_host_nothing_and_reaches_no_fatal,
                loaded_table_with_abandoned_statements_gives_true,
            ],
        ),
        (Some(Fault::LoadTwice), &[already_loaded_is_refused]),
        (
            Some(Fault::NoBind),
            &[
                unbound_element_is_empty_and_reported,
                binding_happens_once,
                runtime_load_binds_its_own,
            ],
        ),
    ];
    for (f, cases) in tests {
        for case in cases {
            plant(f);
            let failed = std::panic::catch_unwind(case).is_err();
            plant(None);
            assert_eq!(failed, f.is_some(), "{f:?}: case {case:?}");
        }
    }
    // The report itself: with `NoReport` the first test sees none.
    plant(Some(Fault::NoReport));
    let failed = std::panic::catch_unwind(unresolved_name_abandons_its_statement).is_err();
    plant(None);
    assert!(failed, "NoReport");
}

// --- The decoder: G13, G14 and G7's report (step B5-3) -----------------------------------------

#[test]
fn high_bytes_accepted_and_reported() {
    let body = [name("S", string("Caf\u{e9}")), name("AFT", one())];
    let mut i = fresh();
    assert_eq!(load(&mut i, &body), Ok(0));
    // `Name` (1) and its name (4): the literal's 0x0D at 5.
    assert_eq!(told(&mut i), vec![Told::HighBytes(5)]);
    assert_eq!(
        eval(&mut i, "S"),
        Ok(Some(Value::String(vec![b'C', b'a', b'f', 0xC3, 0xA9])))
    );
    assert!(exists(&i, "\\AFT"));
}

#[test]
fn unterminated_string_ends_at_its_range() {
    // The last thing in a device's body: a literal with no NUL before the body's end.
    let mut s = name("S", vec![0x0D]);
    s.extend(b"abc");
    let body = [device("D", &[s]), name("AFT", one())];
    let mut i = fresh();
    assert_eq!(load(&mut i, &body), Ok(0));
    // The device: `0x5B 0x82` (2), its length (1), its name (4); then `Name` (1), its name (4).
    assert_eq!(told(&mut i), vec![Told::Unterminated(12)]);
    assert_eq!(
        eval(&mut i, "D.S"),
        Ok(Some(Value::String(b"abc".to_vec())))
    );
    assert!(exists(&i, "\\AFT"));
}

#[test]
fn unterminated_string_in_a_package() {
    let mut lit = vec![0x0D];
    lit.extend(b"xy");
    let body = [name("P", package(2, &[lit])), name("AFT", one())];
    let mut i = fresh();
    assert_eq!(load(&mut i, &body), Ok(0));
    assert_eq!(told(&mut i), vec![Told::Unterminated(8)]);
    assert_eq!(
        eval(&mut i, "P"),
        Ok(Some(Value::Package(vec![
            Value::String(b"xy".to_vec()),
            Value::Uninitialized
        ])))
    );
}

#[test]
fn extra_elements_dropped_and_reported() {
    let body = [
        name("P", package(1, &[one(), byte(2), byte(3)])),
        name("V", var_package(one(), &[byte(4), byte(5)])),
    ];
    let mut i = fresh();
    assert_eq!(load(&mut i, &body), Ok(0));
    assert_eq!(
        told(&mut i),
        vec![
            Told::Extra { count: 1, found: 3 },
            Told::Extra { count: 1, found: 2 }
        ]
    );
    assert_eq!(
        eval(&mut i, "P"),
        Ok(Some(Value::Package(vec![Value::Integer(1)])))
    );
    assert_eq!(
        eval(&mut i, "V"),
        Ok(Some(Value::Package(vec![Value::Integer(4)])))
    );
}

#[test]
fn expression_element_still_refused() {
    // An element that is an expression: refused, as ACPICA does; `Grammar` fails the table whole.
    let body = [name("P", package(1, &[add(one(), one(), zero())]))];
    let mut i = fresh();
    assert_eq!(
        load(&mut i, &body).map_err(|e| e.kind),
        Err(ErrorKind::Grammar)
    );
    assert_eq!(told(&mut i), vec![]);
}

#[test]
fn long_stall_reported() {
    let mut i = fresh();
    load(
        &mut i,
        &[
            method(
                "ST",
                0,
                &[stall(byte(100)), stall(byte(101)), stall(byte(255)), ret(one())],
            ),
            method("BIG", 0, &[stall(word(256)), ret(one())]),
        ],
    )
    .unwrap();
    let _ = told(&mut i);
    assert_eq!(int(&mut i, "ST"), 1);
    assert_eq!(
        told(&mut i),
        vec![Told::LongStall(101), Told::LongStall(255)]
    );
    assert_eq!(
        eval(&mut i, "BIG").map_err(|e| e.kind),
        Err(ErrorKind::Range)
    );
}

#[test]
fn planted_faults_are_caught_by_the_decoder_tests() {
    let tests: [(Option<Fault>, &[fn()]); 4] = [
        (
            None,
            &[
                high_bytes_accepted_and_reported,
                unterminated_string_ends_at_its_range,
                unterminated_string_in_a_package,
                extra_elements_dropped_and_reported,
                expression_element_still_refused,
                long_stall_reported,
            ],
        ),
        (Some(Fault::RefuseHighBytes), &[high_bytes_accepted_and_reported]),
        (
            Some(Fault::RefuseExtraElements),
            &[extra_elements_dropped_and_reported],
        ),
        (Some(Fault::SilentStall), &[long_stall_reported]),
    ];
    for (f, cases) in tests {
        for case in cases {
            plant(f);
            let failed = std::panic::catch_unwind(case).is_err();
            plant(None);
            assert_eq!(failed, f.is_some(), "{f:?}: case {case:?}");
        }
    }
}
