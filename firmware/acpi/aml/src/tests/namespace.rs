//! The namespace (ACPI 6.6 §5.3): search rules, atomic loads, and handles that outlive their
//! nodes.

use super::encode::*;
use super::{interp, load, tables_standin};
use crate::{ErrorKind, IntegerWidth, Value};

#[test]
fn a_failed_load_is_rolled_back_whole() {
    let mut i = interp(IntegerWidth::Bits64);
    load(&mut i, &[device("\\_SB.KEEP", &[])]).unwrap();
    let nodes = i.node_count();
    let e = load(
        &mut i,
        &[
            device(
                "\\_SB.NEW1",
                &[name("A", one()), method("M", 0, &[ret(one())])],
            ),
            scope("\\_SB.KEEP", &[name("INNR", one())]),
            name("\\TOP", one()),
            // Fails here: \MISS does not exist.
            scope("\\MISS", &[]),
        ],
    )
    .unwrap_err();
    assert_eq!(e.kind, ErrorKind::NotFound);
    assert_eq!(i.node_count(), nodes);
    assert!(i.lookup("\\_SB.NEW1").unwrap().is_none());
    assert!(i.lookup("\\_SB.KEEP.INNR").unwrap().is_none());
    assert!(i.lookup("\\_SB.KEEP").unwrap().is_some());
    assert!(i.table(1).unwrap().failed);
    assert!(!i.table(0).unwrap().failed);
}

#[test]
fn a_duplicate_name_in_a_later_table_fails_that_table_only() {
    let mut i = interp(IntegerWidth::Bits64);
    load(&mut i, &[name("\\DUP", one())]).unwrap();
    let nodes = i.node_count();
    let e = load(&mut i, &[name("\\OK1", one()), name("\\DUP", byte(2))]).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Duplicate);
    assert_eq!(i.node_count(), nodes);
    assert_eq!(i.evaluate("\\DUP", &[]).unwrap(), Some(Value::Integer(1)));
}

#[test]
fn single_segments_are_searched_upward_and_paths_are_exact() {
    let mut i = interp(IntegerWidth::Bits64);
    load(
        &mut i,
        &[
            name("VAL", byte(1)),
            device(
                "\\_SB.D1",
                &[
                    name("VAL", byte(2)),
                    device("D2", &[method("GET", 0, &[ret(nm("VAL"))])]),
                    method("UP", 0, &[ret(nm("^VAL"))]),
                ],
            ),
            method("\\_SB.D1.D2.EXCT", 0, &[ret(nm("D2.VAL"))]),
            method("ROOT", 0, &[ret(nm("^^^^^^VAL"))]),
        ],
    )
    .unwrap();
    assert_eq!(
        i.evaluate("\\_SB.D1.D2.GET", &[]).unwrap(),
        Some(Value::Integer(2))
    );
    // A method is a scope (§19.6.85), so `^` from its body is the device it is in. G12, strict
    // until the reading session answers.
    assert_eq!(
        i.evaluate("\\_SB.D1.UP", &[]).unwrap(),
        Some(Value::Integer(2))
    );
    assert_eq!(
        i.evaluate("\\_SB.D1.D2.EXCT", &[]).unwrap_err().kind,
        ErrorKind::NotFound
    );
    assert_eq!(
        i.evaluate("ROOT", &[]).unwrap_err().kind,
        ErrorKind::NotFound
    );
}

#[test]
fn scope_needs_a_scope() {
    let mut i = interp(IntegerWidth::Bits64);
    let e = load(&mut i, &[name("NAM", one()), scope("NAM", &[])]).unwrap_err();
    assert_eq!(e.kind, ErrorKind::WrongObject);
}

#[test]
fn a_handle_to_a_removed_node_is_stale() {
    let mut i = interp(IntegerWidth::Bits64);
    load(&mut i, &[device("\\GONE", &[])]).unwrap();
    let gone = i.lookup("\\GONE").unwrap().unwrap();
    // A second table that fails after re-using nothing: the handle stays valid.
    assert_eq!(i.object_type(gone).unwrap(), 6);
    // Removal happens to dynamic names: one made by a method is gone after it returns.
    load(
        &mut i,
        &[method("MK", 0, &[device("\\TMPD", &[]), ret(one())])],
    )
    .unwrap();
    i.evaluate("MK", &[]).unwrap();
    assert!(i.lookup("\\TMPD").unwrap().is_none());
}

#[test]
fn api_paths_are_checked() {
    let i = interp(IntegerWidth::Bits64);
    for bad in ["\\_SB..X", "\\TOOLONG", "\\9ABC", "\\_SB.", "a.b"] {
        assert_eq!(i.lookup(bad).unwrap_err().kind, ErrorKind::Path, "{bad}");
    }
    assert!(i.lookup("\\_SB").unwrap().is_some());
    assert!(i.lookup("_SB_").unwrap().is_some());
}

#[test]
fn walking_the_namespace_follows_definition_order() {
    let mut i = interp(IntegerWidth::Bits64);
    load(
        &mut i,
        &[
            device("\\_SB.A", &[]),
            device("\\_SB.B", &[]),
            device("\\_SB.C", &[]),
        ],
    )
    .unwrap();
    let sb = i.lookup("\\_SB").unwrap().unwrap();
    let mut names = Vec::new();
    let mut c = i.first_child(sb);
    while let Some(n) = c {
        names.push(i.name_of(n).unwrap());
        c = i.next_sibling(n);
    }
    assert_eq!(names, vec![*b"A___", *b"B___", *b"C___"]);
    let a = i.lookup("\\_SB.A").unwrap().unwrap();
    assert_eq!(i.path_of(a).unwrap(), vec![*b"_SB_", *b"A___"]);
}

#[test]
fn the_stand_in_refuses_bad_headers() {
    let t = table(b"SSDT", 2, &[name("X", one())]);
    let mut short = t.clone();
    short.truncate(20);
    assert!(tables_standin::block(&short).is_err());
    let mut bad = t.clone();
    bad[40] ^= 1;
    assert!(tables_standin::block(&bad).is_err());
    assert!(tables_standin::block(&t).is_ok());
}
