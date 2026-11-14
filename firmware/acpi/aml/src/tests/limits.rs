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
    let after = i.memory_used();
    // What stays is the arena's slot table, which keeps its size for reuse; the buffers are gone,
    // and a second run holds no more than the first.
    assert!(after < before + 16 * 1024, "{before} then {after}");
    assert_eq!(limit_of(i.evaluate("GROW", &[])), Limit::Memory);
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
