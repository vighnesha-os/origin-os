//! The limits of ADR-0019 A3 (the contract's §5): each reached by a block written to reach it,
//! each answered with its error, and nothing left behind.

use super::encode::*;
use super::{interp_with, load};
use crate::{ErrorKind, IntegerWidth, Limit, Limits, Value};

fn small() -> Limits {
    Limits {
        loop_iterations: 1_000,
        time_100ns: u64::MAX,
        call_depth: 16,
        memory_bytes: 1 << 20,
        steps: 1_000_000,
    }
}

#[track_caller]
fn limit_of(r: crate::Result<Option<Value>>) -> Limit {
    match r {
        Err(crate::Error {
            kind: ErrorKind::Limit(l),
            ..
        }) => l,
        other => panic!("expected a limit, got {other:?}"),
    }
}

#[test]
fn an_endless_loop_stops_at_the_loop_limit_and_leaves_no_names() {
    let mut i = interp_with(IntegerWidth::Bits64, small());
    load(
        &mut i,
        &[method(
            "SPIN",
            0,
            &[name("TMP", one()), while_(one(), &[noop()])],
        )],
    )
    .unwrap();
    let nodes = i.node_count();
    assert_eq!(limit_of(i.evaluate("SPIN", &[])), Limit::Loops);
    assert_eq!(i.node_count(), nodes, "the method's name was removed");
    // Again: the name is created afresh, so it was really gone.
    assert_eq!(limit_of(i.evaluate("SPIN", &[])), Limit::Loops);
}

#[test]
fn an_endless_loop_stops_at_the_time_limit() {
    let limits = Limits {
        loop_iterations: u64::MAX,
        time_100ns: 1_000_000,
        ..small()
    };
    let mut i = interp_with(IntegerWidth::Bits64, limits);
    i.host_mut().tick = 1_000;
    load(&mut i, &[method("SPIN", 0, &[while_(one(), &[noop()])])]).unwrap();
    assert_eq!(limit_of(i.evaluate("SPIN", &[])), Limit::Time);
}

#[test]
fn unbounded_recursion_stops_at_the_depth_limit() {
    let mut i = interp_with(IntegerWidth::Bits64, small());
    load(
        &mut i,
        &[
            method("PING", 0, &[ret(call("PONG", &[]))]),
            method("PONG", 0, &[ret(call("PING", &[]))]),
        ],
    )
    .unwrap();
    let nodes = i.node_count();
    assert_eq!(limit_of(i.evaluate("PING", &[])), Limit::Depth);
    assert_eq!(i.node_count(), nodes);
}

#[test]
fn doubling_a_buffer_stops_at_the_memory_limit_and_the_memory_comes_back() {
    let mut i = interp_with(IntegerWidth::Bits64, small());
    load(
        &mut i,
        &[method(
            "GROW",
            0,
            &[
                store(buffer(byte(64), &[]), local(0)),
                while_(one(), &[concat(local(0), local(0), local(0))]),
            ],
        )],
    )
    .unwrap();
    let before = i.memory_used();
    assert_eq!(limit_of(i.evaluate("GROW", &[])), Limit::Memory);
    assert!(i.memory_peak() <= small().memory_bytes);
    // Collected when due (the review of B2, B2-7): measured after that collection.
    i.settle_memory();
    let after = i.memory_used();
    // What stays is the arena's slot table, which keeps its size for reuse; the buffers are gone,
    // and a second run holds no more than the first.
    assert!(after < before + 16 * 1024, "{before} then {after}");
    assert_eq!(limit_of(i.evaluate("GROW", &[])), Limit::Memory);
    i.settle_memory();
    assert_eq!(i.memory_used(), after);
}

#[test]
fn huge_buffers_and_packages_are_refused_before_they_are_made() {
    let mut i = interp_with(IntegerWidth::Bits64, small());
    load(
        &mut i,
        &[
            method("BIGB", 0, &[ret(buffer(dword(0xFFFF_FFFF), &[]))]),
            method("BIGP", 0, &[ret(var_package(dword(0x0FFF_FFFF), &[]))]),
        ],
    )
    .unwrap();
    assert_eq!(limit_of(i.evaluate("BIGB", &[])), Limit::Memory);
    assert_eq!(limit_of(i.evaluate("BIGP", &[])), Limit::Memory);
    assert!(i.memory_peak() <= small().memory_bytes);
}

/// A limit larger than the heap can give (the coordinating session's ruling on step B3's limit,
/// 2026-11-14): an allocation the allocator itself refuses, not the budget, ends the evaluation
/// with the memory limit, leaves no count, peak or work behind, and the interpreter goes on. The
/// buffer asked for is nearly 8 EiB, which no address space here can hold, so the host's
/// allocator returns null as the kernel heap does when it has no frames.
#[test]
fn an_allocation_the_heap_refuses_is_the_memory_limit_and_leaves_nothing_behind() {
    let limits = Limits {
        memory_bytes: usize::MAX,
        ..small()
    };
    let mut i = interp_with(IntegerWidth::Bits64, limits);
    load(
        &mut i,
        &[
            method("HUGE", 0, &[ret(buffer(qword(0x7FFF_FFFF_FFFF_0000), &[]))]),
            method("SMAL", 0, &[ret(buffer(byte(4), &[1, 2, 3, 4]))]),
        ],
    )
    .unwrap();
    assert_eq!(
        i.evaluate("SMAL", &[]).unwrap(),
        Some(Value::Buffer(vec![1, 2, 3, 4]))
    );
    let peak = i.memory_peak();
    assert_eq!(limit_of(i.evaluate("HUGE", &[])), Limit::Memory);
    // The evaluation's own frames may raise the peak a little; the refused bytes may not.
    assert!(
        i.memory_peak() < peak + 4096,
        "the refused bytes were never held: peak {}",
        i.memory_peak()
    );
    let peak = i.memory_peak();
    // The step limit is small's: had the refused bytes been counted as work, this would stop.
    assert_eq!(
        i.evaluate("SMAL", &[]).unwrap(),
        Some(Value::Buffer(vec![1, 2, 3, 4]))
    );
    assert!(i.memory_peak() < peak + 4096);
}

/// N-soak-2 (the soak of 2026-11-15, grammar seed `0x6388aba2`, timed): a tracing host made the
/// interpreter count more work, a unit for each level of the path that each method entered walked
/// for its trace, so the clock was read at other times and `Timer` answered otherwise (1,250,000
/// against 1,300,000). A host that traces must change no count: here a method 21 levels deep, run
/// ten times, counts the same steps either way. Also: `steps_done` counts every evaluation.
#[test]
fn a_tracing_host_changes_no_count() {
    let mut body = vec![method("MTHD", 0, &[ret(one())])];
    for k in (0..20).rev() {
        body = vec![device(&format!("D{k:03}"), &body)];
    }
    let path: String = (0..20).map(|k| format!(".D{k:03}")).collect::<String>() + ".MTHD";
    let path = format!("\\{}", &path[1..]);
    let steps = |traced: bool| {
        let mut i = interp_with(IntegerWidth::Bits64, small());
        if traced {
            i.host_mut().events = Some(Vec::new());
        }
        load(&mut i, &body).unwrap();
        let before = i.steps_done();
        assert!(before > 0, "the load's steps are counted");
        for _ in 0..10 {
            assert_eq!(i.evaluate(&path, &[]).unwrap(), Some(Value::Integer(1)));
        }
        i.steps_done() - before
    };
    let plain = steps(false);
    assert!(plain >= 10, "each evaluation's steps are counted: {plain}");
    assert_eq!(steps(true), plain, "tracing changed the count");
}

#[test]
fn the_step_limit_stops_straight_line_code() {
    let limits = Limits {
        steps: 50,
        ..small()
    };
    let mut i = interp_with(IntegerWidth::Bits64, limits);
    let body: Vec<B> = (0..100).map(|_| noop()).collect();
    load(&mut i, &[method("LONG", 0, &body)]).unwrap();
    assert_eq!(limit_of(i.evaluate("LONG", &[])), Limit::Steps);
}

#[test]
fn operands_nested_a_hundred_thousand_deep_neither_recurse_nor_panic() {
    // Add (Add (Add (… One …))): each level is a frame on the interpreter's own heap stack.
    let depth = 100_000;
    let mut expr = one();
    for _ in 0..depth {
        let mut next = vec![0x72];
        next.extend(expr);
        next.extend(one());
        next.push(0x00);
        expr = next;
    }
    let limits = Limits {
        memory_bytes: 64 << 20,
        steps: 10_000_000,
        ..small()
    };
    let mut i = interp_with(IntegerWidth::Bits64, limits);
    load(&mut i, &[method("DEEP", 0, &[ret(expr)])]).unwrap();
    assert_eq!(
        i.evaluate("DEEP", &[]).unwrap(),
        Some(Value::Integer(depth + 1))
    );
    // The same nesting under a small memory limit is refused, not crashed.
    let mut j = interp_with(IntegerWidth::Bits64, small());
    let mut expr = one();
    for _ in 0..depth {
        let mut next = vec![0x72];
        next.extend(expr);
        next.extend(one());
        next.push(0x00);
        expr = next;
    }
    load(&mut j, &[method("DEEP", 0, &[ret(expr)])]).unwrap();
    assert_eq!(limit_of(j.evaluate("DEEP", &[])), Limit::Memory);
}

#[test]
fn a_package_nested_past_64_levels_is_not_handed_out() {
    let mut p = package(1, &[one()]);
    for _ in 0..80 {
        p = package(1, &[p]);
    }
    let mut i = interp_with(IntegerWidth::Bits64, small());
    load(&mut i, &[name("NEST", p)]).unwrap();
    assert_eq!(limit_of(i.evaluate("NEST", &[])), Limit::Nesting);
}

#[test]
fn a_name_made_twice_in_a_loop_is_refused_and_removed() {
    let mut i = interp_with(IntegerWidth::Bits64, small());
    load(
        &mut i,
        &[method("TWIC", 0, &[while_(one(), &[name("X", one())])])],
    )
    .unwrap();
    let nodes = i.node_count();
    let e = i.evaluate("TWIC", &[]).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Duplicate);
    assert_eq!(i.node_count(), nodes);
}

#[test]
fn garbage_is_collected_before_it_fills_the_limit_while_live_data_is_large() {
    // 600 KiB of a 1 MiB limit is live; each iteration leaves about 16 KiB behind.
    let mut i = interp_with(IntegerWidth::Bits64, small());
    load(
        &mut i,
        &[
            name("BIG", buffer(dword(600 * 1024), &[])),
            method(
                "CHRN",
                0,
                &[
                    store(zero(), local(1)),
                    store(buffer(word(4096), &[]), local(2)),
                    while_(
                        logic(0x95, local(1), word(900)),
                        &[concat(local(2), local(2), local(0)), increment(local(1))],
                    ),
                    ret(local(1)),
                ],
            ),
        ],
    )
    .unwrap();
    assert_eq!(i.evaluate("CHRN", &[]).unwrap(), Some(Value::Integer(900)));
    assert!(i.memory_peak() <= small().memory_bytes);
}

#[test]
fn a_memory_limit_hit_while_copying_leaves_the_count_where_it_was() {
    // A named package of 2,000 strings, copied until the limit is hit inside the copy.
    let strings: Vec<B> = (0..200)
        .map(|_| string("0123456789abcdef0123456789abcdef"))
        .collect();
    let mut i = interp_with(IntegerWidth::Bits64, small());
    load(
        &mut i,
        &[
            name("PKG", package(200, &strings)),
            method(
                "KEEP",
                0,
                &[
                    store(var_package(word(4000), &[]), local(0)),
                    store(zero(), local(7)),
                    while_(
                        one(),
                        &[
                            store(nm("PKG"), index(local(0), local(7), none())),
                            increment(local(7)),
                        ],
                    ),
                ],
            ),
        ],
    )
    .unwrap();
    // The first run grows the arena's slot table, which keeps its size for reuse; every later
    // run must end where the first did.
    assert_eq!(limit_of(i.evaluate("KEEP", &[])), Limit::Memory);
    i.settle_memory();
    let base = i.memory_used();
    let _ = super::take_drift();
    for _ in 0..30 {
        // Collected and counted afresh when due (the review of B2, B2-7): each run is measured
        // after that collection, which must find the count where the first run's was.
        assert_eq!(limit_of(i.evaluate("KEEP", &[])), Limit::Memory);
        i.settle_memory();
        assert_eq!(i.memory_used(), base);
    }
    let (over, under) = super::take_drift();
    assert_eq!(under, 0);
    eprintln!(
        "memory-limit copies: {over} bytes over-counted in 30 runs, given back by the recount"
    );
}

/// The review of B1's F1 case: one `Field` of 300,000 names in one scope, then a term that fails.
fn wide_field() -> Vec<B> {
    const LEAD: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    const REST: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut list = Vec::with_capacity(300_000);
    let mut k = 0usize;
    'all: for &a in LEAD {
        for &b in REST {
            for &c in REST {
                for &d in REST {
                    list.push(Fe::Seg([a, b, c, d], 1));
                    k += 1;
                    if k == 300_000 {
                        break 'all;
                    }
                }
            }
        }
    }
    vec![
        op_region("WIDE", 0, zero(), dword(300_000 / 8 + 1)),
        field("WIDE", 1, &list),
        scope("\\MISS", &[]),
    ]
}

#[test]
fn a_field_of_300000_names_ends_within_its_limits() {
    let body = wide_field();
    let t = table(b"SSDT", 2, &body);
    let (block, _) = super::block(&t).unwrap();
    // The service's limits, on the wall clock.
    for (what, limits) in [
        ("service limits", Limits::SERVICE),
        (
            "a 2 s time limit and no step limit",
            Limits {
                steps: u64::MAX,
                time_100ns: 20_000_000,
                ..Limits::SERVICE
            },
        ),
    ] {
        let mut i = interp_with(IntegerWidth::Bits64, limits);
        let nodes = i.node_count();
        let start = std::time::Instant::now();
        i.host_mut().real_clock = Some(start);
        let e = i.load(block).unwrap_err();
        let took = start.elapsed();
        assert!(
            matches!(e.kind, ErrorKind::Limit(Limit::Steps | Limit::Time)),
            "{what}: {e:?}"
        );
        // The limit is read each 1,024 units of work, and the rollback that follows is linear in
        // what was made: the load ends within a quarter more than its limit.
        assert!(
            u128::from(limits.time_100ns) * 125 >= took.as_nanos(),
            "{what}: {took:?} well past its time limit"
        );
        assert_eq!(i.node_count(), nodes, "{what}: rolled back");
        eprintln!("300,000-name field, {what}: {:?} after {took:?}", e.kind);
    }
    // A step limit: deterministic.
    let limits = Limits {
        steps: 1_000_000,
        ..Limits::SERVICE
    };
    let mut first = None;
    for _ in 0..2 {
        let mut i = interp_with(IntegerWidth::Bits64, limits);
        let e = i.load(block).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Limit(Limit::Steps));
        assert_eq!(*first.get_or_insert(e), e);
    }
}

#[test]
fn a_vector_near_the_limit_grows_once_to_what_is_left_then_is_refused() {
    // The review of B1, F1 (growth): never by one element, so a vector at the limit is not
    // copied whole for each element pushed.
    use crate::mem::{self, Budget};
    let mut budget = Budget::new(10_000);
    let mut v: Vec<u64> = Vec::new();
    let mut capacities = vec![];
    while mem::push(&mut v, 7, &mut budget).is_ok() {
        if capacities.last() != Some(&v.capacity()) {
            capacities.push(v.capacity());
        }
    }
    // 4, 8, … 1,024 by doubling, then once to the 1,250 that 10,000 bytes hold; then refused.
    assert_eq!(
        capacities,
        [4, 8, 16, 32, 64, 128, 256, 512, 1024, 1250],
        "the capacities a vector of u64 took under a 10,000-byte limit"
    );
    assert_eq!(v.len(), 1250);
    assert!(budget.used() <= 10_000);
}

#[test]
fn a_store_into_a_large_buffer_is_charged_for_every_byte_it_writes() {
    // The review of B1, A1: a 60 MiB named buffer, and straight-line stores of one byte into it,
    // each of which writes all 60 MiB. Each is charged before it runs, so the step limit ends it,
    // and with no step limit the clock does, a store's work after its time.
    let stores: Vec<B> = (0..3_000).map(|_| store(one(), nm("BIGB"))).collect();
    let body = [
        name("BIGB", buffer(dword(0x3C0_0000), &[])),
        method("M0", 0, &stores),
    ];
    for (what, limits, want) in [
        ("service limits", Limits::SERVICE, Limit::Steps),
        (
            "a 1 s time limit and no step limit",
            Limits {
                steps: u64::MAX,
                time_100ns: 10_000_000,
                ..Limits::SERVICE
            },
            Limit::Time,
        ),
    ] {
        let mut i = interp_with(IntegerWidth::Bits64, limits);
        load(&mut i, &body).unwrap();
        let start = std::time::Instant::now();
        i.host_mut().real_clock = Some(start);
        let e = i.evaluate("M0", &[]).unwrap_err();
        let took = start.elapsed();
        assert_eq!(e.kind, ErrorKind::Limit(want), "{what}");
        assert!(
            u128::from(limits.time_100ns) * 125 >= took.as_nanos(),
            "{what}: {took:?} well past its time limit"
        );
        eprintln!(
            "60 MiB buffer, 3,000 stores, {what}: {:?} after {took:?}",
            e.kind
        );
    }
}

#[test]
fn a_long_parent_prefix_is_charged_for_its_length() {
    // The review of B1, A1: `CondRefOf` of a name with 60,000 parent prefixes never fails, so an
    // endless loop of it parses 60,000 bytes a pass. Charged a unit a prefix, it meets a step
    // limit of ten million after some 160 passes, not the loop limit after a million.
    let far = format!("{}X", "^".repeat(60_000));
    let body = [method(
        "M0",
        0,
        &[while_(one(), &[cond_ref_of(nm(&far), local(0))])],
    )];
    let mut i = interp_with(
        IntegerWidth::Bits64,
        Limits {
            steps: 10_000_000,
            ..Limits::SERVICE
        },
    );
    load(&mut i, &body).unwrap();
    let start = std::time::Instant::now();
    let e = i.evaluate("M0", &[]).unwrap_err();
    let took = start.elapsed();
    assert_eq!(e.kind, ErrorKind::Limit(Limit::Steps));
    eprintln!(
        "60,000 parent prefixes in a loop: {:?} after {took:?}",
        e.kind
    );
}
