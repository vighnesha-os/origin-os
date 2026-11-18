//! Step B4: `Load` and `LoadTable` (ACPI 6.6 §19.6.77 and §19.6.78; B4's contract, §5 and §7).
//! Each source and each outcome: a buffer, a SystemMemory region and a field unit loading; each
//! check that fails giving Zero with no name left; a limit inside a loaded table ending the
//! evaluation with the table rolled back; `LoadTable` found and not found, under a root path, with
//! its parameter; and a load nested inside methods and other loads on a fixed native stack.

use super::encode::*;
use super::{Event, Fault, Interp, interp, interp_with, load, plant};
use crate::{ErrorKind, IntegerWidth, Limit, Limits, TableKey, Value};

fn m(path: &str, body: &[B]) -> B {
    method(path, 0, body)
}

/// A buffer object holding `bytes`.
fn buffer_of(bytes: &[u8]) -> B {
    buffer(dword(bytes.len() as u32), bytes)
}

/// An SSDT of revision 2 whose term list is `body`.
fn ssdt(body: &[B]) -> Vec<u8> {
    table(b"SSDT", 2, body)
}

/// The table `TB` holds: two names and a method, all under the prefix `QA`.
fn qa() -> Vec<u8> {
    ssdt(&[
        name("QA0", int(5)),
        name("QA1", string("a")),
        m("QAM", &[ret(int(7))]),
    ])
}

fn loaded(width: IntegerWidth, body: &[B]) -> Interp {
    let mut i = interp(width);
    load(&mut i, body).expect("the block loads");
    i
}

#[track_caller]
fn get(i: &mut Interp, path: &str) -> u64 {
    match i.evaluate(path, &[]) {
        Ok(Some(Value::Integer(v))) => v,
        other => panic!("{path}: expected an integer, got {other:?}"),
    }
}

#[track_caller]
fn fails(i: &mut Interp, path: &str) -> ErrorKind {
    match i.evaluate(path, &[]) {
        Err(e) => e.kind,
        other => panic!("{path}: expected an error, got {other:?}"),
    }
}

fn exists(i: &Interp, path: &str) -> bool {
    i.lookup(path).unwrap().is_some()
}

/// Every node whose last segment begins with `prefix`.
fn named(i: &Interp, prefix: &[u8]) -> usize {
    let mut n = 0;
    let mut stack = vec![i.root()];
    while let Some(node) = stack.pop() {
        if i.name_of(node).unwrap().starts_with(prefix) {
            n += 1;
        }
        let mut c = i.first_child(node);
        while let Some(k) = c {
            stack.push(k);
            c = i.next_sibling(k);
        }
    }
    n
}

/// The table under `key` in the test host: an address and these bytes.
fn host_table(i: &mut Interp, key: (&[u8; 4], &[u8; 6], &[u8; 8]), bytes: Vec<u8>) {
    i.host_mut().tables.push((
        TableKey {
            signature: *key.0,
            oem_id: *key.1,
            oem_table_id: *key.2,
        },
        0x8000,
        bytes,
    ));
}

/// `LoadTable` with these six operands, its value returned.
fn load_table_of(sig: &str, oem: &str, id: &str, root: &str, param: &str, data: B) -> B {
    ret(load_table([
        string(sig),
        string(oem),
        string(id),
        string(root),
        string(param),
        data,
    ]))
}

// --- `Load` from each source -----------------------------------------------------------------

#[test]
fn a_buffer_loads_its_names_stay_and_the_target_holds_true() {
    let mut i = loaded(
        IntegerWidth::Bits64,
        &[
            name("TB", buffer_of(&qa())),
            name("RES", int(3)),
            m("LD", &[ret(load_op("TB", nm("RES")))]),
        ],
    );
    let tables = i.table_count();
    assert_eq!(get(&mut i, "LD"), u64::MAX, "True is Ones (Q4)");
    assert_eq!(get(&mut i, "RES"), u64::MAX, "the target holds True");
    assert_eq!(get(&mut i, "\\QA0"), 5);
    assert_eq!(get(&mut i, "\\QAM"), 7);
    assert_eq!(i.table_count(), tables + 1, "a table of its own");
    let t = i.table(tables as u32).unwrap();
    assert_eq!((t.signature, t.failed), (*b"SSDT", false));
    // Its names belong to it: they are loaded, and its method runs.
    let qa0 = i.lookup("\\QA0").unwrap().unwrap();
    assert!(i.is_loaded(qa0).unwrap());
}

#[test]
fn true_is_ones_at_the_integer_width() {
    let mut i = loaded(
        IntegerWidth::Bits32,
        &[
            name("TB", buffer_of(&qa())),
            m("LD", &[ret(load_op("TB", none()))]),
        ],
    );
    assert_eq!(get(&mut i, "LD"), 0xFFFF_FFFF);
}

/// The SSDT in host memory at `at`, for a region to read.
fn in_memory(i: &mut Interp, at: u64, bytes: &[u8]) {
    for (k, &b) in bytes.iter().enumerate() {
        i.host_mut().memory.insert((0, None, at + k as u64), b);
    }
}

#[test]
fn a_system_memory_region_loads_read_only_within_its_span() {
    let t = qa();
    // The region is larger than the table: the block need only lie wholly inside it.
    let len = t.len() as u32 + 13;
    let mut i = loaded(
        IntegerWidth::Bits64,
        &[
            op_region("RG", 0, dword(0x1_0003), dword(len)),
            m("LD", &[ret(load_op("RG", none()))]),
        ],
    );
    in_memory(&mut i, 0x1_0003, &t);
    assert_eq!(get(&mut i, "LD"), u64::MAX);
    assert_eq!(get(&mut i, "\\QA0"), 5);
    let seen = std::mem::take(&mut i.host_mut().seen);
    assert!(!seen.is_empty());
    let mut covered = 0u64;
    for s in &seen {
        assert!(!s.write && s.space == 0, "{s:?}");
        // The host's width is in bits.
        let w = u64::from(s.width) / 8;
        assert!(s.address >= 0x1_0003, "{s:?}");
        assert!(s.address + w <= 0x1_0003 + u64::from(len), "{s:?}");
        assert!(s.address.is_multiple_of(w), "naturally aligned: {s:?}");
        covered += w;
    }
    assert_eq!(covered, u64::from(len), "the region read once, whole");
    // The widest aligned accesses from 0x1_0003: a byte, a double word, then quad words.
    assert_eq!(
        &seen.iter().map(|s| s.width).take(3).collect::<Vec<_>>(),
        &[8, 32, 64]
    );
}

#[test]
fn a_region_too_short_for_its_block_gives_zero_and_no_name() {
    let t = qa();
    let mut i = loaded(
        IntegerWidth::Bits64,
        &[
            op_region("RG", 0, dword(0x2_0000), dword(t.len() as u32 - 1)),
            m("LD", &[ret(load_op("RG", none()))]),
        ],
    );
    in_memory(&mut i, 0x2_0000, &t);
    assert_eq!(get(&mut i, "LD"), 0);
    assert_eq!(named(&i, b"QA"), 0);
}

#[test]
fn a_region_is_checked_whole_before_its_first_access() {
    // Larger than the memory limit allows: refused before a byte is read; a limit, so it ends
    // the evaluation.
    let mut i = loaded(
        IntegerWidth::Bits64,
        &[
            op_region("BIG", 0, dword(0x10_0000), dword(0x4000_0000)),
            m("LD", &[ret(load_op("BIG", none()))]),
        ],
    );
    assert_eq!(fails(&mut i, "LD"), ErrorKind::Limit(Limit::Memory));
    assert!(i.host().seen.is_empty(), "nothing read");
}

#[test]
fn a_region_outside_system_memory_is_a_type_error() {
    let mut i = loaded(
        IntegerWidth::Bits64,
        &[
            op_region("IO", 1, word(0x80), byte(64)),
            m("LD", &[ret(load_op("IO", none()))]),
        ],
    );
    assert_eq!(fails(&mut i, "LD"), ErrorKind::Type);
    assert!(i.host().seen.is_empty());
}

#[test]
fn a_field_unit_loads_through_the_field_machinery() {
    let t = qa();
    let bits = t.len() as u32 * 8;
    let mut i = loaded(
        IntegerWidth::Bits64,
        &[
            op_region("RG", 0, dword(0x3_0000), dword(t.len() as u32)),
            field("RG", 1, &[Fe::Named("TF", bits)]),
            m("LD", &[ret(load_op("TF", none()))]),
        ],
    );
    in_memory(&mut i, 0x3_0000, &t);
    assert_eq!(get(&mut i, "LD"), u64::MAX);
    assert_eq!(get(&mut i, "\\QAM"), 7);
}

#[test]
fn other_objects_are_type_errors_and_a_missing_one_not_found() {
    let mut i = loaded(
        IntegerWidth::Bits64,
        &[
            name("IN", int(1)),
            name("ST", string("SSDT")),
            m("L1", &[ret(load_op("IN", none()))]),
            m("L2", &[ret(load_op("ST", none()))]),
            m("L3", &[ret(load_op("\\NOPE", none()))]),
        ],
    );
    assert_eq!(fails(&mut i, "L1"), ErrorKind::Type);
    assert_eq!(fails(&mut i, "L2"), ErrorKind::Type);
    assert_eq!(fails(&mut i, "L3"), ErrorKind::NotFound);
}

// --- Each check that fails: Zero, and no name left ---------------------------------------------

/// Loads a buffer holding `bytes` from a method, and requires Zero, no `QA` name, and the failed
/// table's bytes given back.
#[track_caller]
fn gives_zero(bytes: Vec<u8>) {
    let mut i = loaded(
        IntegerWidth::Bits64,
        &[
            name("TB", buffer_of(&bytes)),
            name("RES", int(3)),
            m("LD", &[ret(load_op("TB", nm("RES")))]),
        ],
    );
    let (nodes, used) = (i.node_count(), i.memory_used());
    assert_eq!(get(&mut i, "LD"), 0);
    assert_eq!(get(&mut i, "RES"), 0, "the target holds Zero");
    assert_eq!(named(&i, b"QA"), 0);
    assert_eq!(i.node_count(), nodes);
    assert_eq!(i.memory_used(), used, "nothing kept");
}

#[test]
fn a_bad_checksum_gives_zero() {
    let mut t = qa();
    // `QA0`'s value, 5, made 4: the AML stays sound, and only the checksum is wrong.
    assert_eq!(&t[36..43], &[0x08, b'Q', b'A', b'0', b'_', 0x0A, 5]);
    t[42] ^= 1;
    gives_zero(t);
}

#[test]
fn a_length_past_its_source_gives_zero() {
    let mut t = qa();
    let more = (t.len() as u32 + 1).to_le_bytes();
    t[4..8].copy_from_slice(&more);
    // The checksum made right for the new length's bytes, so that only the length is wrong.
    t[9] = 0;
    let sum = t.iter().fold(0u8, |s, &b| s.wrapping_add(b));
    t[9] = 0u8.wrapping_sub(sum);
    gives_zero(t);
}

#[test]
fn a_short_header_gives_zero() {
    gives_zero(qa()[..20].to_vec());
}

#[test]
fn a_block_not_signed_ssdt_gives_zero() {
    // Q2: only an SSDT, though the checksum is right.
    for sig in [b"DSDT", b"PSDT", b"OEMX"] {
        gives_zero(table(sig, 2, &[name("QA0", one())]));
    }
}

#[test]
fn a_term_list_that_fails_gives_zero_and_takes_its_names_back() {
    gives_zero(ssdt(&[
        name("QA0", one()),
        device("QAD", &[name("QAX", one())]),
        store(nm("\\NOPE"), debug()),
        name("QA1", one()),
    ]));
}

#[test]
fn a_return_in_a_loaded_table_fails_its_load() {
    gives_zero(ssdt(&[name("QA0", one()), ret(one())]));
}

#[test]
fn a_table_loaded_twice_fails_at_its_first_duplicate() {
    // Q7.
    let mut i = loaded(
        IntegerWidth::Bits64,
        &[
            name("TB", buffer_of(&qa())),
            m("LD", &[ret(load_op("TB", none()))]),
        ],
    );
    assert_eq!(get(&mut i, "LD"), u64::MAX);
    let nodes = i.node_count();
    assert_eq!(get(&mut i, "LD"), 0);
    assert_eq!(i.node_count(), nodes);
    assert_eq!(named(&i, b"QA"), 3, "the first load's names stay");
    let last = (i.table_count() - 1) as u32;
    assert!(i.table(last).unwrap().failed);
    assert!(!i.table(last - 1).unwrap().failed);
}

#[test]
fn a_completed_load_stays_when_its_evaluation_fails_later() {
    // Q8: AML has no transactions.
    let mut i = loaded(
        IntegerWidth::Bits64,
        &[
            name("TB", buffer_of(&qa())),
            m("LD", &[load_op("TB", none()), store(nm("\\NOPE"), debug())]),
        ],
    );
    assert_eq!(fails(&mut i, "LD"), ErrorKind::NotFound);
    assert_eq!(get(&mut i, "\\QA0"), 5);
}

#[test]
fn a_limit_inside_a_loaded_table_ends_the_evaluation_and_rolls_it_back() {
    let limits = Limits {
        loop_iterations: 100,
        ..Limits::SERVICE
    };
    let mut i = interp_with(IntegerWidth::Bits64, limits);
    let spin = ssdt(&[name("QA0", one()), while_(one(), &[noop()])]);
    load(
        &mut i,
        &[
            name("TB", buffer_of(&spin)),
            m("LD", &[ret(load_op("TB", none()))]),
        ],
    )
    .unwrap();
    let (nodes, used) = (i.node_count(), i.memory_used());
    assert_eq!(fails(&mut i, "LD"), ErrorKind::Limit(Limit::Loops));
    assert_eq!(i.node_count(), nodes);
    assert_eq!(i.memory_used(), used);
    let last = (i.table_count() - 1) as u32;
    assert!(i.table(last).unwrap().failed);
}

#[test]
fn a_table_that_loads_itself_is_bounded_by_the_call_depth() {
    // Its first term loads it again: each load is a level of the call depth.
    let mut i = interp(IntegerWidth::Bits64);
    let me = ssdt(&[load_op("\\TB", none()), name("QA0", one())]);
    load(
        &mut i,
        &[
            name("TB", buffer_of(&me)),
            m("LD", &[ret(load_op("TB", none()))]),
        ],
    )
    .unwrap();
    let nodes = i.node_count();
    assert_eq!(fails(&mut i, "LD"), ErrorKind::Limit(Limit::Depth));
    assert_eq!(i.node_count(), nodes);
    assert_eq!(named(&i, b"QA"), 0);
}

#[test]
fn a_load_that_fails_inside_a_load_leaves_the_outer_one_whole() {
    let inner = ssdt(&[name("QB0", one()), store(nm("\\NOPE"), debug())]);
    let outer = ssdt(&[
        name("QA0", one()),
        name("RIN", int(3)),
        load_op("\\TI", nm("RIN")),
        name("QA1", one()),
    ]);
    let mut i = loaded(
        IntegerWidth::Bits64,
        &[
            name("TI", buffer_of(&inner)),
            name("TO", buffer_of(&outer)),
            m("LD", &[ret(load_op("TO", none()))]),
        ],
    );
    assert_eq!(get(&mut i, "LD"), u64::MAX);
    assert_eq!(get(&mut i, "\\RIN"), 0, "the inner load gave Zero");
    assert_eq!(named(&i, b"QB"), 0);
    assert_eq!(named(&i, b"QA"), 2);
}

#[test]
fn a_load_is_traced() {
    let mut i = loaded(
        IntegerWidth::Bits64,
        &[
            name("TB", buffer_of(&qa())),
            m("LD", &[load_op("TB", none()), ret(load_op("TB", none()))]),
        ],
    );
    i.host_mut().events = Some(Vec::new());
    let first = i.table_count() as u32;
    assert_eq!(get(&mut i, "LD"), 0);
    let events = i.host_mut().events.take().unwrap();
    let loads: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e, Event::Load(_) | Event::Loaded(_)))
        .collect();
    assert_eq!(
        loads,
        vec![
            &Event::Load(*b"SSDT"),
            &Event::Loaded(Ok(first)),
            &Event::Load(*b"SSDT"),
            &Event::Loaded(Err(ErrorKind::Duplicate)),
        ]
    );
}

#[test]
fn a_limit_inside_nested_loads_is_traced_for_each_table() {
    // A tracing host is told of the limit, then of each table being loaded, innermost first, as
    // it is told of a failed load anywhere: no `Load` without its `Loaded`.
    let limits = Limits {
        loop_iterations: 100,
        ..Limits::SERVICE
    };
    let spin = ssdt(&[name("QB0", one()), while_(one(), &[noop()])]);
    let outer = ssdt(&[name("QA0", one()), load_op("\\TI", none())]);
    let body = [
        name("TI", buffer_of(&spin)),
        name("TO", buffer_of(&outer)),
        m("LD", &[ret(load_op("TO", none()))]),
    ];
    let loads = |events: Vec<Event>| -> Vec<Event> {
        events
            .into_iter()
            .filter(|e| matches!(e, Event::Load(_) | Event::Loaded(_) | Event::Limit(_)))
            .collect()
    };
    let failed = Event::Loaded(Err(ErrorKind::Limit(Limit::Loops)));
    // In an evaluation.
    let mut i = interp_with(IntegerWidth::Bits64, limits);
    load(&mut i, &body).unwrap();
    i.host_mut().events = Some(Vec::new());
    assert_eq!(fails(&mut i, "LD"), ErrorKind::Limit(Limit::Loops));
    assert_eq!(
        loads(i.host_mut().events.take().unwrap()),
        vec![
            Event::Load(*b"SSDT"),
            Event::Load(*b"SSDT"),
            Event::Limit(Limit::Loops),
            failed.clone(),
            failed.clone(),
        ]
    );
    assert_eq!(named(&i, b"Q"), 0);
    // In a block's own load: its table is told last.
    let mut j = interp_with(IntegerWidth::Bits64, limits);
    j.host_mut().events = Some(Vec::new());
    let mut top = body.to_vec();
    top.push(load_op("\\TO", none()));
    assert_eq!(
        load(&mut j, &top).map_err(|e| e.kind),
        Err(ErrorKind::Limit(Limit::Loops))
    );
    assert_eq!(
        loads(j.host_mut().events.take().unwrap()),
        vec![
            Event::Load(*b"SSDT"),
            Event::Load(*b"SSDT"),
            Event::Load(*b"SSDT"),
            Event::Limit(Limit::Loops),
            failed.clone(),
            failed.clone(),
            failed,
        ]
    );
}

/// Every allocation a nested load makes refused in turn: the memory limit swept, 16 bytes at a
/// time, from what the block holds once loaded to past what the evaluation needs, with the load
/// made from 0 to 11 calls deep so that the frame stack grows at each of its pushes. Each time the
/// evaluation either loads the table or ends at `Limit(Memory)`; a tracing host hears a `Loaded`
/// for every `Load`; and no name of a failed table is left.
#[test]
fn every_memory_refusal_in_a_load_leaves_the_trace_balanced_and_no_name() {
    let body = [
        name("TB", buffer_of(&qa())),
        method(
            "REC",
            1,
            &[
                if_(
                    logic(0x94, arg(0), zero()),
                    &[ret(call("REC", &[bin(0x74, arg(0), one(), none())]))],
                ),
                ret(load_op("TB", none())),
            ],
        ),
    ];
    let run = |memory_bytes: usize, depth: u64| {
        let limits = Limits {
            memory_bytes,
            ..Limits::SERVICE
        };
        let mut i = interp_with(IntegerWidth::Bits64, limits);
        load(&mut i, &body).ok()?;
        let held = i.memory_used();
        i.host_mut().events = Some(Vec::new());
        let r = i
            .evaluate("REC", &[Value::Integer(depth)])
            .map_err(|e| e.kind);
        let events = i.host_mut().events.take().unwrap();
        let count = |f: fn(&Event) -> bool| events.iter().filter(|e| f(e)).count();
        let (loads, ended) = (
            count(|e| matches!(e, Event::Load(_))),
            count(|e| matches!(e, Event::Loaded(_))),
        );
        Some((r, loads, ended, named(&i, b"QA"), held, i.memory_peak()))
    };
    let mut refused = 0;
    for depth in 0..12 {
        let (r, _, _, _, held, peak) = run(1 << 20, depth).unwrap();
        assert_eq!(r, Ok(Some(Value::Integer(u64::MAX))));
        for limit in (held..peak + 64).step_by(16) {
            let Some((r, loads, ended, names, _, _)) = run(limit, depth) else {
                continue;
            };
            assert_eq!(loads, ended, "depth {depth}, limit {limit}: {r:?}");
            match r {
                Ok(Some(Value::Integer(u64::MAX))) => assert_eq!(names, 3),
                Err(ErrorKind::Limit(Limit::Memory)) => {
                    assert_eq!(names, 0, "depth {depth}, limit {limit}");
                    refused += 1;
                }
                other => panic!("depth {depth}, limit {limit}: {other:?}"),
            }
        }
    }
    assert!(refused > 0);
}

// --- `LoadTable` -------------------------------------------------------------------------------

const KEY: (&[u8; 4], &[u8; 6], &[u8; 8]) = (b"OEMT", b"ORIGIN", b"AMLTEST\0");

/// The table the host lists under [`KEY`]: names under the prefix `QF`. The encoder pads its
/// table ID with a space, as firmware does; the key from AML strings is padded with NUL (G18).
fn qf() -> Vec<u8> {
    table(b"OEMT", 2, &[name("QF0", int(9)), name("QF1", int(1))])
}

#[test]
fn load_table_loads_what_the_host_lists() {
    let mut i = loaded(
        IntegerWidth::Bits64,
        &[m(
            "LT",
            &[load_table_of("OEMT", "ORIGIN", "AMLTEST", "", "", zero())],
        )],
    );
    host_table(&mut i, KEY, qf());
    assert_eq!(get(&mut i, "LT"), u64::MAX);
    assert_eq!(get(&mut i, "\\QF0"), 9);
}

#[test]
fn load_table_of_a_table_not_listed_gives_zero() {
    let mut i = loaded(
        IntegerWidth::Bits64,
        &[m(
            "LT",
            &[load_table_of("OEMT", "ORIGIN", "OTHER", "", "", zero())],
        )],
    );
    host_table(&mut i, KEY, qf());
    assert_eq!(get(&mut i, "LT"), 0);
    assert_eq!(named(&i, b"QF"), 0);
}

#[test]
fn load_table_loads_under_its_root_path_and_stores_its_parameter() {
    let mut i = loaded(
        IntegerWidth::Bits64,
        &[
            device("DV", &[]),
            name("PRM", int(0)),
            m(
                "L1",
                &[load_table_of(
                    "OEMT",
                    "ORIGIN",
                    "AMLTEST",
                    "\\DV",
                    "QF1",
                    int(42),
                )],
            ),
        ],
    );
    host_table(&mut i, KEY, qf());
    assert_eq!(get(&mut i, "L1"), u64::MAX);
    assert_eq!(get(&mut i, "\\DV.QF0"), 9);
    assert!(!exists(&i, "\\QF0"));
    // `QF1` is taken under the root path, `\DV`.
    assert_eq!(get(&mut i, "\\DV.QF1"), 42);
}

#[test]
fn load_table_takes_a_parameter_path_from_the_root_as_written() {
    let mut i = loaded(
        IntegerWidth::Bits64,
        &[
            device("DV", &[]),
            name("PRM", int(0)),
            m(
                "L1",
                &[load_table_of(
                    "OEMT",
                    "ORIGIN",
                    "AMLTEST",
                    "DV",
                    "\\PRM",
                    int(42),
                )],
            ),
        ],
    );
    host_table(&mut i, KEY, qf());
    assert_eq!(get(&mut i, "L1"), u64::MAX);
    assert_eq!(get(&mut i, "\\PRM"), 42);
}

#[test]
fn a_parameter_that_names_nothing_is_an_error_and_the_table_stays() {
    let mut i = loaded(
        IntegerWidth::Bits64,
        &[m(
            "L1",
            &[load_table_of(
                "OEMT",
                "ORIGIN",
                "AMLTEST",
                "",
                "NONE",
                int(1),
            )],
        )],
    );
    host_table(&mut i, KEY, qf());
    assert_eq!(fails(&mut i, "L1"), ErrorKind::NotFound);
    assert_eq!(get(&mut i, "\\QF0"), 9);
}

#[test]
fn a_root_path_that_names_nothing_is_an_error() {
    let mut i = loaded(
        IntegerWidth::Bits64,
        &[m(
            "L1",
            &[load_table_of(
                "OEMT",
                "ORIGIN",
                "AMLTEST",
                "\\NODV",
                "",
                zero(),
            )],
        )],
    );
    host_table(&mut i, KEY, qf());
    assert_eq!(fails(&mut i, "L1"), ErrorKind::NotFound);
    assert_eq!(named(&i, b"QF"), 0);
}

#[test]
fn load_table_of_a_dsdt_or_ssdt_key_gives_zero() {
    // Q6: even when the host would answer.
    for sig in ["DSDT", "SSDT"] {
        let body = [m(
            "LT",
            &[load_table_of(sig, "ORIGIN", "AMLTEST", "", "", zero())],
        )];
        let mut i = loaded(IntegerWidth::Bits64, &body);
        let s: [u8; 4] = sig.as_bytes().try_into().unwrap();
        host_table(
            &mut i,
            (&s, KEY.1, KEY.2),
            table(&s, 2, &[name("QF0", one())]),
        );
        assert_eq!(get(&mut i, "LT"), 0, "{sig}");
        assert_eq!(named(&i, b"QF"), 0, "{sig}");
    }
}

#[test]
fn keys_too_long_are_errors() {
    let mut i = loaded(
        IntegerWidth::Bits64,
        &[
            m("K1", &[load_table_of("OEMTX", "", "", "", "", zero())]),
            m(
                "K2",
                &[load_table_of("OEMT", "ORIGIN7", "", "", "", zero())],
            ),
            m(
                "K3",
                &[load_table_of("OEMT", "", "AMLTEST99", "", "", zero())],
            ),
        ],
    );
    for k in ["K1", "K2", "K3"] {
        assert_eq!(fails(&mut i, k), ErrorKind::Range, "{k}");
    }
}

#[test]
fn a_host_copy_that_fails_its_check_gives_zero() {
    let cases: [(&str, Vec<u8>); 3] = [
        ("a bad checksum", {
            let mut t = qf();
            // `QF0`'s value only: the AML stays sound.
            assert_eq!(t[42], 9);
            t[42] ^= 1;
            t
        }),
        (
            "another signature",
            table(b"OEMU", 2, &[name("QF0", one())]),
        ),
        ("other IDs", {
            let mut t = qf();
            t[16..24].copy_from_slice(b"AMLTESTX");
            t[9] = 0;
            let sum = t.iter().fold(0u8, |s, &b| s.wrapping_add(b));
            t[9] = 0u8.wrapping_sub(sum);
            t
        }),
    ];
    for (what, bytes) in cases {
        let mut i = loaded(
            IntegerWidth::Bits64,
            &[m(
                "LT",
                &[load_table_of("OEMT", "ORIGIN", "AMLTEST", "", "", zero())],
            )],
        );
        host_table(&mut i, KEY, bytes);
        assert_eq!(get(&mut i, "LT"), 0, "{what}");
        assert_eq!(named(&i, b"QF"), 0, "{what}");
    }
}

// --- Nesting on a fixed native stack ---------------------------------------------------------

/// A load from a method 600 calls deep, whose table at load time calls a method that recurses 600
/// more and loads a second table there, on a thread of 32 KiB in release mode (as the boot
/// proof's stack test, B3; B4's contract) and 256 KiB in debug mode, whose frames are several
/// times a release build's. Were the engine to recurse in Rust for a call or a load, this depth
/// would overflow either.
#[test]
fn nested_loads_run_on_a_fixed_native_stack() {
    let stack = if cfg!(debug_assertions) {
        256 << 10
    } else {
        32 << 10
    };
    let handle = std::thread::Builder::new()
        .stack_size(stack)
        .spawn(|| {
            let run = |call_depth| {
                let limits = Limits {
                    call_depth,
                    memory_bytes: 64 << 20,
                    steps: 10_000_000,
                    ..Limits::SERVICE
                };
                let mut i = interp_with(IntegerWidth::Bits64, limits);
                let recurse = |name: &str, then: B| {
                    method(
                        name,
                        1,
                        &[
                            if_(
                                logic(0x94, arg(0), zero()),
                                &[ret(call(name, &[bin(0x74, arg(0), one(), none())]))],
                            ),
                            ret(then),
                        ],
                    )
                };
                let second = ssdt(&[name("QL2", int(2))]);
                let first = ssdt(&[
                    name("QL1", int(1)),
                    name("RL2", int(3)),
                    store(call("\\RE2", &[word(600)]), nm("RL2")),
                ]);
                load(
                    &mut i,
                    &[
                        name("TB1", buffer_of(&first)),
                        name("TB2", buffer_of(&second)),
                        recurse("RE1", load_op("TB1", none())),
                        recurse("RE2", load_op("\\TB2", none())),
                    ],
                )
                .unwrap();
                let r = i.evaluate("RE1", &[Value::Integer(600)]);
                let rl2 = i.evaluate("\\RL2", &[]);
                (
                    r.map_err(|e| e.kind),
                    rl2.map_err(|e| e.kind),
                    named(&i, b"QL"),
                )
            };
            (run(2_000), run(1_000))
        })
        .unwrap();
    let ((r, rl2, names), (control, _, left)) = handle.join().expect("no overflow");
    assert_eq!(r, Ok(Some(Value::Integer(u64::MAX))));
    assert_eq!(rl2, Ok(Some(Value::Integer(u64::MAX))), "the inner load");
    assert_eq!(names, 2);
    // The control: the nesting is as deep as written, past 1,000 levels, and the limit holds.
    assert_eq!(control, Err(ErrorKind::Limit(Limit::Depth)));
    assert_eq!(left, 0, "both tables rolled back");
}

// --- The planted faults ------------------------------------------------------------------------

/// A fault planted, or none, and the tests it must fail (or pass).
type Planted = (Option<Fault>, &'static [fn()]);

/// Each of B4's planted faults (its contract, §7) fails the tests written for it; with none, they
/// pass. The randomized test's control plants them as well (`random.rs`).
#[test]
fn planted_faults_are_caught_by_the_load_tests() {
    let tests: [Planted; 5] = [
        (
            None,
            &[
                a_bad_checksum_gives_zero,
                a_block_not_signed_ssdt_gives_zero,
                a_term_list_that_fails_gives_zero_and_takes_its_names_back,
                a_load_that_fails_inside_a_load_leaves_the_outer_one_whole,
                a_buffer_loads_its_names_stay_and_the_target_holds_true,
                a_table_loaded_twice_fails_at_its_first_duplicate,
            ],
        ),
        (
            Some(Fault::LoadUnchecked),
            &[
                a_bad_checksum_gives_zero,
                a_block_not_signed_ssdt_gives_zero,
            ],
        ),
        (
            Some(Fault::KeepFailedLoad),
            &[
                a_term_list_that_fails_gives_zero_and_takes_its_names_back,
                a_load_that_fails_inside_a_load_leaves_the_outer_one_whole,
            ],
        ),
        (
            Some(Fault::InvertLoadResult),
            &[a_buffer_loads_its_names_stay_and_the_target_holds_true],
        ),
        (
            Some(Fault::AllowDuplicate),
            &[a_table_loaded_twice_fails_at_its_first_duplicate],
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
