//! Step B5: the load policy (G1, G3; B4's Q3 and Q7) and the binding of names in load-time
//! packages (G1), with the reports each gives (the coordinating session's rulings of 2026-11-19 on
//! TASKS 64's ledger; B5's contract, accepted the same day). Every test asserts its reports
//! exactly.

use super::encode::*;
use super::{Fault, Interp, Told, eval, int, interp, interp_with, load, plant, told};
use crate::{ErrorKind, IntegerWidth, Limits, TableKey, Value};

/// A planted fault, or none, and the tests that must fail with it (or pass without one).
type Control = (Option<Fault>, &'static [fn()]);

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

#[test]
fn a_boot_tables_local_abandons_its_statement() {
    // A boot table's table level has no method, so no locals or arguments: the statement naming
    // one is refused with `OutsideMethod`, as at a loaded table's table level (B4-13), and only
    // it is abandoned (step B5, Q2's line).
    let body = [
        store(byte(1), local(0)),
        store(arg(0), debug()),
        name("AFT", one()),
    ];
    let mut i = fresh();
    assert_eq!(load(&mut i, &body), Ok(0));
    assert_eq!(
        told(&mut i),
        vec![
            abandoned(&body, 0, 0, ErrorKind::OutsideMethod),
            abandoned(&body, 1, 0, ErrorKind::OutsideMethod),
        ]
    );
    assert!(exists(&i, "\\AFT"));
}

#[test]
fn a_skip_whose_work_list_reaches_the_memory_limit_fails_the_table_whole() {
    // The skip's work list is a vector charged to the memory limit and given back after each skip
    // (B5's contract, corrected; the coordinating session's ruling on the hand-over's point 4). A
    // statement nested deep enough that its list cannot be held fails the table whole, as a limit,
    // and leaves nothing held. `Store (Add (UNDF, Add (UNDF, … , Zero), Zero), X)`: the decoder
    // fails at the first `UNDF`, two levels down; the skip steps over all 20,000 levels, a level
    // of the work list each. A level is 24 bytes, so the list needs about 480 KiB: a 256 KiB limit
    // cannot hold it, where 1 MiB, tried first, held it whole and the table loaded.
    let mut e = add(nm("UNDF"), zero(), zero());
    for _ in 0..20_000 {
        e = add(nm("UNDF"), e, zero());
    }
    let body = [name("X", zero()), store(e, nm("X")), name("AFT", one())];
    let mut i = interp_with(
        IntegerWidth::Bits64,
        Limits {
            memory_bytes: 1 << 18,
            ..Limits::SERVICE
        },
    );
    i.settle_memory();
    let before = i.memory_used();
    assert_eq!(
        load(&mut i, &body).map_err(|e| e.kind),
        Err(ErrorKind::Limit(crate::Limit::Memory))
    );
    assert!(!exists(&i, "\\X") && !exists(&i, "\\AFT"));
    assert!(i.table(0).unwrap().failed);
    assert_eq!(told(&mut i), vec![], "the skip failed before its report");
    at_rest(&i);
    i.settle_memory();
    assert!(
        i.memory_used() < before + 1024,
        "{} bytes held after, {before} before",
        i.memory_used()
    );
    // With room, the same table loads and the statement is abandoned.
    let mut j = fresh();
    assert_eq!(load(&mut j, &body), Ok(0));
    assert!(matches!(
        told(&mut j).as_slice(),
        [Told::Abandoned {
            kind: ErrorKind::NotFound,
            ..
        }]
    ));
    assert!(exists(&j, "\\AFT"));
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
    let tests: [Control; 5] = [
        (
            Some(Fault::AllowDuplicate),
            &[duplicate_keeps_the_first_object],
        ),
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
                &[
                    stall(byte(100)),
                    stall(byte(101)),
                    stall(byte(255)),
                    ret(one()),
                ],
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
    let tests: [Control; 4] = [
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
        (
            Some(Fault::RefuseHighBytes),
            &[high_bytes_accepted_and_reported],
        ),
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

// --- Conversions and stores: G4, G11, G15 (step B5-4) ------------------------------------------

/// The value a method returning `v` gives, at `width`.
fn value_of(width: IntegerWidth, v: B) -> Value {
    let mut i = interp(width);
    load(&mut i, &[method("M", 0, &[ret(v)])]).unwrap();
    eval(&mut i, "M").unwrap().unwrap()
}

fn text(s: &str) -> Value {
    Value::String(s.as_bytes().to_vec())
}

#[test]
fn g4_texts() {
    use IntegerWidth::{Bits32, Bits64};
    let buf = || buffer(byte(3), &[0x01, 0xAB, 0x00]);
    let empty = || buffer(zero(), &[]);
    for (width, v, want) in [
        (
            Bits64,
            concat(string(""), byte(0x1A), none()),
            "000000000000001A",
        ),
        (Bits32, concat(string(""), byte(0x1A), none()), "0000001A"),
        (Bits64, un(0x98, byte(0x1A), none()), "0x1A"),
        (Bits64, un(0x98, zero(), none()), "0x0"),
        (
            Bits64,
            un(0x98, qword(u64::MAX), none()),
            "0xFFFFFFFFFFFFFFFF",
        ),
        (Bits64, un(0x97, word(1234), none()), "1234"),
        (Bits64, un(0x97, zero(), none()), "0"),
        (Bits64, concat(string(""), buf(), none()), "0x01 0xAB 0x00"),
        (Bits64, un(0x98, buf(), none()), "0x01,0xAB,0x00"),
        (Bits64, un(0x97, buf(), none()), "1,171,0"),
        (Bits64, un(0x98, empty(), none()), ""),
        (Bits64, un(0x97, empty(), none()), ""),
        (Bits64, concat(string(""), empty(), none()), ""),
    ] {
        assert_eq!(value_of(width, v), text(want), "{want}");
    }
}

#[test]
fn string_to_integer_is_total() {
    use IntegerWidth::{Bits32, Bits64};
    // Implicit (here through `Add`): hexadecimal, a `0x` tolerated.
    for (w, s, want) in [
        (Bits64, "1A", 0x1A),
        (Bits64, "  \t1a", 0x1A),
        (Bits64, "0x1A", 0x1A),
        (Bits64, "001A", 0x1A),
        (Bits64, "1AZ9", 0x1A),
        (Bits64, "", 0),
        (Bits64, "zz", 0),
        (Bits64, "12345678901234567", 0x1234_5678_9012_3456),
        (Bits32, "123456789", 0x1234_5678),
        (Bits64, "0000000000000000001", 1),
    ] {
        assert_eq!(
            value_of(w, add(string(s), zero(), none())),
            Value::Integer(want),
            "{s:?}"
        );
    }
}

#[test]
fn to_integer_decimal_and_hex() {
    use IntegerWidth::{Bits32, Bits64};
    for (w, s, want) in [
        (Bits64, "  1234", 1234),
        (Bits64, "0x1A", 26),
        (Bits64, "0X1f", 31),
        (Bits64, "12AB", 12),
        (Bits64, "", 0),
        (Bits64, "99999999999999999999999", 9_999_999_999_999_999_999),
        (Bits32, "9999999999", 999_999_999),
    ] {
        assert_eq!(
            value_of(w, un(0x99, string(s), none())),
            Value::Integer(want),
            "{s:?}"
        );
    }
    // A string becomes a buffer with its NUL, an empty one as one zero byte.
    assert_eq!(
        value_of(Bits64, un(0x96, string("AB"), none())),
        Value::Buffer(vec![0x41, 0x42, 0])
    );
    assert_eq!(
        value_of(Bits64, un(0x96, string(""), none())),
        Value::Buffer(vec![0])
    );
}

#[test]
fn zero_length_buffer_takes_source_length() {
    // G11: a zero-length named buffer takes the source's length; any other keeps its own; a named
    // string takes the new string's.
    let mut i = fresh();
    load(
        &mut i,
        &[
            name("B0", buffer(zero(), &[])),
            name("B2", buffer(byte(2), &[9, 9])),
            name("S", string("abcdef")),
            method(
                "M",
                0,
                &[
                    store(buffer(byte(3), &[1, 2, 3]), nm("B0")),
                    store(buffer(byte(3), &[1, 2, 3]), nm("B2")),
                    store(string("xy"), nm("S")),
                ],
            ),
            method("N", 0, &[store(buffer(byte(1), &[7]), nm("B2"))]),
        ],
    )
    .unwrap();
    eval(&mut i, "M").unwrap();
    assert_eq!(eval(&mut i, "B0"), Ok(Some(Value::Buffer(vec![1, 2, 3]))));
    assert_eq!(eval(&mut i, "B2"), Ok(Some(Value::Buffer(vec![1, 2]))));
    assert_eq!(eval(&mut i, "S"), Ok(Some(text("xy"))));
    eval(&mut i, "N").unwrap();
    assert_eq!(eval(&mut i, "B2"), Ok(Some(Value::Buffer(vec![7, 0]))));
    assert_eq!(told(&mut i), vec![]);
}

#[test]
fn strings_go_to_a_field_as_a_buffer_of_their_bytes() {
    // G15's string half, as the coordinating session ruled it again on 2026-11-19 (step B5's
    // point 1): a string is a buffer of its bytes, without its NUL. As long as the field or
    // shorter, one write, zero-extended; longer, field-sized pieces, lowest first, the last
    // zero-extended (ACPI 6.6 Table 19.7, the Buffer to Field Unit row), the host told once. The
    // field keeps the last piece, where ACPICA's one write, cut, keeps the first.
    let mut i = fresh();
    load(
        &mut i,
        &[
            op_region("RM", 0, word(0x1000), byte(4)),
            field("RM", 3, &[Fe::Named("F32", 32)]),
            method("W", 1, &[store(arg(0), nm("F32")), ret(nm("F32"))]),
        ],
    )
    .unwrap();
    for (s, writes, pieces) in [
        ("", vec![0u64], None),
        ("AB", vec![0x4241], None),
        ("ABCD", vec![0x4443_4241], None),
        ("ABCDEF", vec![0x4443_4241, 0x4645], Some(2)),
        ("ABCDEFGH", vec![0x4443_4241, 0x4847_4645], Some(2)),
        ("ABCDEFGHI", vec![0x4443_4241, 0x4847_4645, 0x49], Some(3)),
    ] {
        i.host_mut().seen.clear();
        let got = i
            .evaluate("\\W", &[Value::String(s.as_bytes().to_vec())])
            .unwrap();
        let last = *writes.last().unwrap();
        assert_eq!(got, Some(Value::Integer(last)), "{s:?}");
        let made: Vec<u64> = i
            .host()
            .seen
            .iter()
            .filter(|x| x.write)
            .map(|x| x.value)
            .collect();
        assert_eq!(made, writes, "{s:?}");
        let want: Vec<Told> = pieces
            .map(|n| Told::Pieces(vec![*b"F32_"], n))
            .into_iter()
            .collect();
        assert_eq!(told(&mut i), want, "{s:?}");
    }
}

#[test]
fn ec_pieces_are_one_transfer_each() {
    // Step B5's point 1 with G5: a string longer than an Embedded Controller field over 8 bits
    // goes in field-sized pieces, each one transfer.
    let mut i = fresh();
    load(
        &mut i,
        &[
            op_region("EC0", 3, zero(), byte(4)),
            field("EC0", 0, &[Fe::Named("E16", 16)]),
            method("W", 0, &[store(string("ABCDE"), nm("E16"))]),
        ],
    )
    .unwrap();
    eval(&mut i, "W").unwrap();
    let sent: Vec<(u64, bool, Vec<u8>)> = core::mem::take(&mut i.host_mut().transfers)
        .into_iter()
        .map(|t| (t.0.address, t.0.write, t.0.sent))
        .collect();
    assert_eq!(
        sent,
        vec![
            (0, true, vec![0x41, 0x42]),
            (0, true, vec![0x43, 0x44]),
            (0, true, vec![0x45, 0x00]),
        ]
    );
    assert_eq!(told(&mut i), vec![Told::Pieces(vec![*b"E16_"], 3)]);
}

#[test]
fn a_long_buffer_into_a_one_bit_field_ends_at_a_limit() {
    // Each piece is a region access counted as work (step B5's point 1, its bound): a buffer of
    // 256 KiB, as large as a 1 MiB memory limit lets the store hold it and its copy, into a field
    // of one bit would be 2,097,152 writes. The step limit ends the store, every write made before
    // it counted; the host was told the store's pieces first.
    let mut i = interp_with(
        IntegerWidth::Bits64,
        Limits {
            steps: 50_000,
            memory_bytes: 1 << 20,
            ..Limits::SERVICE
        },
    );
    load(
        &mut i,
        &[
            op_region("RM", 0, word(0x1000), byte(1)),
            field("RM", 1, &[Fe::Named("BIT", 1)]),
            method("W", 0, &[store(buffer(dword(0x4_0000), &[]), nm("BIT"))]),
        ],
    )
    .unwrap();
    i.host_mut().seen.clear();
    assert_eq!(
        eval(&mut i, "W").map_err(|e| e.kind),
        Err(ErrorKind::Limit(crate::Limit::Steps))
    );
    let writes = u64::try_from(i.host().seen.iter().filter(|s| s.write).count()).unwrap();
    assert!(writes > 1_000, "{writes} writes before the limit");
    // Every write made was counted, and the count passed the limit (13,914 writes for 50,002
    // units when written: a piece costs more than its write).
    assert!(
        writes <= i.run.steps && i.run.steps > 50_000,
        "{writes} writes, {} steps",
        i.run.steps
    );
    assert_eq!(told(&mut i), vec![Told::Pieces(vec![*b"BIT_"], 2_097_152)]);
    at_rest(&i);
}

#[test]
fn planted_faults_are_caught_by_the_piece_tests() {
    let tests: [Control; 4] = [
        (
            None,
            &[
                strings_go_to_a_field_as_a_buffer_of_their_bytes,
                ec_pieces_are_one_transfer_each,
                a_long_buffer_into_a_one_bit_field_ends_at_a_limit,
            ],
        ),
        (
            Some(Fault::StringCut),
            &[
                strings_go_to_a_field_as_a_buffer_of_their_bytes,
                ec_pieces_are_one_transfer_each,
            ],
        ),
        (
            Some(Fault::SilentPieces),
            &[
                strings_go_to_a_field_as_a_buffer_of_their_bytes,
                ec_pieces_are_one_transfer_each,
                a_long_buffer_into_a_one_bit_field_ends_at_a_limit,
            ],
        ),
        (
            Some(Fault::StringByCharacter),
            &[
                strings_go_to_a_field_as_a_buffer_of_their_bytes,
                ec_pieces_are_one_transfer_each,
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
}

#[test]
fn planted_faults_are_caught_by_the_conversion_tests() {
    let tests: [Control; 3] = [
        (
            None,
            &[
                g4_texts,
                string_to_integer_is_total,
                to_integer_decimal_and_hex,
                zero_length_buffer_takes_source_length,
            ],
        ),
        (Some(Fault::OldTexts), &[g4_texts]),
        (
            Some(Fault::KeepZeroLength),
            &[zero_length_buffer_takes_source_length],
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
}

// --- Access widths and Embedded Controller fields: G5 (step B5-5) -------------------------------

#[test]
fn anyacc_is_byte_access() {
    let mut i = fresh();
    load(
        &mut i,
        &[
            op_region("RM", 0, word(0x2000), byte(8)),
            field("RM", 0, &[Fe::Named("W16", 16), Fe::Named("D32", 32)]),
        ],
    )
    .unwrap();
    i.host_mut().seen.clear();
    assert_eq!(eval(&mut i, "D32"), Ok(Some(Value::Integer(0))));
    let seen = &i.host().seen;
    assert_eq!(seen.len(), 4, "{seen:?}");
    assert!(seen.iter().all(|s| s.width == 8), "{seen:?}");
}

#[test]
fn field_at_region_end_reads_within_it() {
    // `M652`'s shape (TASKS 72's finding 2): any width with the `Lock` rule, 4 bytes ending at
    // the region's end and not 4-byte aligned. Before step B5 an 8-byte unit passed the end.
    let mut i = fresh();
    load(
        &mut i,
        &[
            op_region("RM", 0, word(0x3000), byte(7)),
            field("RM", 0x10, &[Fe::Reserved(24), Fe::Named("LAST", 32)]),
        ],
    )
    .unwrap();
    i.host_mut().seen.clear();
    assert_eq!(eval(&mut i, "LAST"), Ok(Some(Value::Integer(0))));
    let seen = &i.host().seen;
    assert_eq!(seen.len(), 4, "{seen:?}");
    assert!(
        seen.iter()
            .all(|s| s.width == 8 && (0x3003..0x3007).contains(&s.address)),
        "{seen:?}"
    );
    assert_eq!((i.host().global_taken, i.host().global_held), (1, 0));
}

#[test]
fn ec_wide_field_is_one_transfer() {
    let mut i = fresh();
    load(
        &mut i,
        &[
            op_region("EC0", 3, zero(), byte(32)),
            field(
                "EC0",
                0,
                &[
                    Fe::Named("W16", 16),
                    Fe::Named("D32", 32),
                    Fe::Reserved(3),
                    Fe::Named("X13", 13),
                    Fe::Named("B136", 136),
                ],
            ),
            field("EC0", 0x40, &[Fe::Reserved(3), Fe::Named("Z13", 13)]),
            field("EC0", 0, &[Fe::Reserved(8 * 30), Fe::Named("BY", 8)]),
            method("WD", 0, &[store(dword(0x1122_3344), nm("D32"))]),
            method("WX", 0, &[store(zero(), nm("X13"))]),
            method("WZ", 0, &[store(word(0x1FFF), nm("Z13"))]),
        ],
    )
    .unwrap();
    let take = |i: &mut Interp| core::mem::take(&mut i.host_mut().transfers);
    // A read of 16 bits: one transfer of 2 bytes at the field's first byte.
    i.host_mut().answer = vec![0x34, 0x12];
    assert_eq!(eval(&mut i, "W16"), Ok(Some(Value::Integer(0x1234))));
    let t = take(&mut i);
    assert_eq!(t.len(), 1);
    assert_eq!(
        (
            t[0].0.space,
            t[0].0.address,
            t[0].0.write,
            t[0].0.bit_length,
            t[0].0.sent.len()
        ),
        (3, 0, false, 16, 2)
    );
    // A write of whole bytes: one transfer, no read.
    i.host_mut().answer = vec![];
    eval(&mut i, "WD").unwrap();
    let t = take(&mut i);
    assert_eq!(t.len(), 1);
    assert_eq!(
        (t[0].0.address, t[0].0.write, t[0].0.sent.clone()),
        (2, true, vec![0x44, 0x33, 0x22, 0x11])
    );
    // 13 bits from bit 3 of byte 6, Preserve: a read, then the write with the other bits kept.
    i.host_mut().answer = vec![0xFF, 0xFF];
    eval(&mut i, "WX").unwrap();
    let t = take(&mut i);
    assert_eq!(t.len(), 2);
    assert_eq!((t[0].0.address, t[0].0.write), (6, false));
    assert_eq!(
        (t[1].0.address, t[1].0.write, t[1].0.sent.clone()),
        (6, true, vec![0x07, 0x00])
    );
    // WriteAsZeros: no read.
    i.host_mut().answer = vec![];
    eval(&mut i, "WZ").unwrap();
    let t = take(&mut i);
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].0.sent, vec![0xF8, 0xFF]);
    // 136 bits: one transfer of 17 bytes.
    eval(&mut i, "B136").unwrap();
    let t = take(&mut i);
    assert_eq!(t.len(), 1);
    assert_eq!((t[0].0.address, t[0].0.sent.len()), (8, 17));
    // A byte field stays a byte read.
    i.host_mut().seen.clear();
    eval(&mut i, "BY").unwrap();
    assert!(take(&mut i).is_empty());
    assert_eq!(i.host().seen.len(), 1);
}

#[test]
fn ec_field_over_255_bytes_refused() {
    let mut i = fresh();
    load(
        &mut i,
        &[
            op_region("EC1", 3, zero(), word(300)),
            field("EC1", 0, &[Fe::Named("HUGE", 256 * 8)]),
        ],
    )
    .unwrap();
    assert_eq!(
        eval(&mut i, "HUGE").map_err(|e| e.kind),
        Err(ErrorKind::Range)
    );
    assert!(i.host().transfers.is_empty());
}

#[test]
fn planted_faults_are_caught_by_the_width_tests() {
    let tests: [Control; 3] = [
        (
            None,
            &[
                anyacc_is_byte_access,
                field_at_region_end_reads_within_it,
                ec_wide_field_is_one_transfer,
                ec_field_over_255_bytes_refused,
            ],
        ),
        (
            Some(Fault::WidestAnyAcc),
            &[field_at_region_end_reads_within_it],
        ),
        (Some(Fault::EcBytewise), &[ec_wide_field_is_one_transfer]),
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

// --- `LoadTable`'s and `DataTableRegion`'s IDs: G18 (step B5-6) ----------------------------------

/// An interpreter whose host lists `OEM1`, a table with the encoder's IDs (`ORIGIN`,
/// `AMLTEST `), under `listed`, and a method `LT` loading by `sig`, `oem` and `id`.
fn with_oem1(listed: TableKey, sig: &str, oem: &str, id: &str) -> Interp {
    let t = table(b"OEM1", 2, &[name("QY", one())]);
    let mut i = fresh();
    i.host_mut().tables.push((listed, 0x2000, t));
    load(
        &mut i,
        &[method(
            "LT",
            0,
            &[ret(load_table([
                string(sig),
                string(oem),
                string(id),
                string(""),
                string(""),
                zero(),
            ]))],
        )],
    )
    .unwrap();
    i
}

const OEM1: TableKey = TableKey {
    signature: *b"OEM1",
    oem_id: *b"ORIGIN",
    oem_table_id: *b"AMLTEST ",
};

#[test]
fn ids_compare_exactly_with_zero_padding() {
    // The table's OEM table ID is padded with a space: a key without it does not match it.
    let mut i = with_oem1(OEM1, "OEM1", "ORIGIN", "AMLTEST");
    assert_eq!(int(&mut i, "LT"), 0);
    let mut i = with_oem1(OEM1, "OEM1", "ORIGIN", "AMLTEST ");
    assert_eq!(int(&mut i, "LT"), u64::MAX);
    assert!(exists(&i, "\\QY"));
}

#[test]
fn a_hosts_copy_is_checked_with_the_same_rule() {
    // A host that lists the table under the key AML gives ("AMLTEST" and NUL): the library's check
    // of the copy still refuses it, its ID being "AMLTEST " (G18).
    let loose = TableKey {
        oem_table_id: *b"AMLTEST\0",
        ..OEM1
    };
    let mut i = with_oem1(loose, "OEM1", "ORIGIN", "AMLTEST");
    assert_eq!(int(&mut i, "LT"), 0);
    assert!(!exists(&i, "\\QY"));
}

#[test]
fn empty_id_matches_any() {
    let mut i = with_oem1(OEM1, "OEM1", "", "");
    assert_eq!(int(&mut i, "LT"), u64::MAX);
    let mut i = with_oem1(OEM1, "OEM1", "ORIGIN", "");
    assert_eq!(int(&mut i, "LT"), u64::MAX);
}

#[test]
fn signature_checked_and_cut_to_four() {
    let mut i = with_oem1(OEM1, "OEM1XYZ", "ORIGIN", "AMLTEST ");
    assert_eq!(int(&mut i, "LT"), u64::MAX, "only four compared");
    for bad in ["OEM", "oem1", ""] {
        let mut i = with_oem1(OEM1, bad, "ORIGIN", "AMLTEST ");
        assert_eq!(
            eval(&mut i, "LT").map_err(|e| e.kind),
            Err(ErrorKind::Name),
            "{bad:?}"
        );
    }
    let mut i = with_oem1(OEM1, "OEM1", "ORIGINAL", "AMLTEST ");
    assert_eq!(
        eval(&mut i, "LT").map_err(|e| e.kind),
        Err(ErrorKind::Range)
    );
}

#[test]
fn datatableregion_same_rule() {
    let t = table(b"OEM1", 2, &[]);
    for (id, made) in [("", true), ("AMLTEST ", true), ("AMLTEST", false)] {
        let mut i = fresh();
        i.host_mut().tables.push((OEM1, 0x2000, t.clone()));
        let body = [
            data_region("DR", string("OEM1"), string("ORIGIN"), string(id)),
            name("AFT", one()),
        ];
        load(&mut i, &body).unwrap();
        assert_eq!(exists(&i, "\\DR"), made, "{id:?}");
        assert_eq!(told(&mut i).len(), usize::from(!made), "{id:?}");
        assert!(exists(&i, "\\AFT"));
    }
}

#[test]
fn planted_faults_are_caught_by_the_id_tests() {
    let tests: [Control; 2] = [
        (
            None,
            &[
                ids_compare_exactly_with_zero_padding,
                a_hosts_copy_is_checked_with_the_same_rule,
                empty_id_matches_any,
                signature_checked_and_cut_to_four,
                datatableregion_same_rule,
            ],
        ),
        (
            Some(Fault::OldIdMatch),
            &[a_hosts_copy_is_checked_with_the_same_rule],
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
}
