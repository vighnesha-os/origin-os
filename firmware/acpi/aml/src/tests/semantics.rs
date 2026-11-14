//! What each opcode does, checked against ACPI 6.6's text, one case at a time; and the coverage
//! test: across the cases, every opcode of Table 20.2 is emitted by the encoder and the block it
//! is in has its result checked.

use super::encode::*;
use super::{Interp, eval, int, interp, load};
use crate::{ErrorKind, Feature, IntegerWidth, Value};

/// A method `path` with no arguments whose body is `body`.
fn m(path: &str, body: &[B]) -> B {
    method(path, 0, body)
}

/// A method returning `v`.
fn r(path: &str, v: B) -> B {
    m(path, &[ret(v)])
}

fn buf(v: &[u8]) -> Value {
    Value::Buffer(v.to_vec())
}

fn s(v: &str) -> Value {
    Value::String(v.as_bytes().to_vec())
}

#[track_caller]
fn val(i: &mut Interp, path: &str) -> Value {
    match eval(i, path) {
        Ok(Some(v)) => v,
        other => panic!("{path}: expected a value, got {other:?}"),
    }
}

#[track_caller]
fn fails(i: &mut Interp, path: &str, kind: ErrorKind) {
    match eval(i, path) {
        Err(e) => assert_eq!(e.kind, kind, "{path}"),
        other => panic!("{path}: expected {kind:?}, got {other:?}"),
    }
}

/// One case: a block and what must hold after it is loaded.
struct Case {
    name: &'static str,
    width: IntegerWidth,
    body: Vec<B>,
    check: fn(&mut Interp),
}

fn case(name: &'static str, body: Vec<B>, check: fn(&mut Interp)) -> Case {
    Case {
        name,
        width: IntegerWidth::Bits64,
        body,
        check,
    }
}

fn case32(name: &'static str, body: Vec<B>, check: fn(&mut Interp)) -> Case {
    Case {
        name,
        width: IntegerWidth::Bits32,
        body,
        check,
    }
}

fn corpus() -> Vec<Case> {
    vec![
        case(
            "constants",
            vec![
                r("C0", zero()),
                r("C1", one()),
                r("C2", ones()),
                r("C3", byte(0xAB)),
                r("C4", word(0xABCD)),
                r("C5", dword(0xDEAD_BEEF)),
                r("C6", qword(0x0123_4567_89AB_CDEF)),
                r("C7", revision()),
                r("C8", string("text")),
            ],
            |i| {
                assert_eq!(int(i, "C0"), 0);
                assert_eq!(int(i, "C1"), 1);
                assert_eq!(int(i, "C2"), u64::MAX);
                assert_eq!(int(i, "C3"), 0xAB);
                assert_eq!(int(i, "C4"), 0xABCD);
                assert_eq!(int(i, "C5"), 0xDEAD_BEEF);
                assert_eq!(int(i, "C6"), 0x0123_4567_89AB_CDEF);
                assert_eq!(int(i, "C7"), crate::INTERPRETER_REVISION);
                assert_eq!(val(i, "C8"), s("text"));
            },
        ),
        case32(
            "32-bit integers",
            vec![
                r("C2", ones()),
                r("C6", qword(0x0123_4567_89AB_CDEF)),
                r("A0", add(dword(0xFFFF_FFFF), one(), none())),
                r("T0", un(0x96, one(), none())),
                r("T1", un(0x98, byte(0x1F), none())),
                r("N0", un(0x80, zero(), none())),
                r("L0", logic(0x93, one(), one())),
            ],
            |i| {
                assert_eq!(int(i, "C2"), 0xFFFF_FFFF);
                assert_eq!(int(i, "C6"), 0x89AB_CDEF);
                assert_eq!(int(i, "A0"), 0);
                assert_eq!(val(i, "T0"), buf(&[1, 0, 0, 0]));
                assert_eq!(val(i, "T1"), s("0000001F"));
                assert_eq!(int(i, "N0"), 0xFFFF_FFFF);
                assert_eq!(int(i, "L0"), 0xFFFF_FFFF);
            },
        ),
        case(
            "buffers and packages",
            vec![
                name("B0", buffer(byte(4), &[1, 2])),
                name("B1", buffer(one(), &[1, 2, 3])),
                name("B2", buffer(zero(), &[])),
                name(
                    "P0",
                    package(3, &[one(), string("x"), package(1, &[byte(2)])]),
                ),
                name("P1", package(3, &[one()])),
                name(
                    "P2",
                    var_package(add(one(), one(), none()), &[byte(5), byte(6)]),
                ),
                name("P3", package(2, &[nm("\\IN1"), nm("\\DV1")])),
                name("IN1", byte(7)),
                device("DV1", &[]),
            ],
            |i| {
                assert_eq!(val(i, "B0"), buf(&[1, 2, 0, 0]));
                assert_eq!(val(i, "B1"), buf(&[1, 2, 3]));
                assert_eq!(val(i, "B2"), buf(&[]));
                assert_eq!(
                    val(i, "P0"),
                    Value::Package(vec![
                        Value::Integer(1),
                        s("x"),
                        Value::Package(vec![Value::Integer(2)])
                    ])
                );
                assert_eq!(
                    val(i, "P1"),
                    Value::Package(vec![
                        Value::Integer(1),
                        Value::Uninitialized,
                        Value::Uninitialized
                    ])
                );
                assert_eq!(
                    val(i, "P2"),
                    Value::Package(vec![Value::Integer(5), Value::Integer(6)])
                );
                let dv1 = i.lookup("\\DV1").unwrap().unwrap();
                assert_eq!(
                    val(i, "P3"),
                    Value::Package(vec![Value::Integer(7), Value::Reference(dv1)])
                );
            },
        ),
        case(
            "integer arithmetic",
            vec![
                r("A0", add(byte(2), byte(3), none())),
                r("A1", bin(0x74, byte(2), byte(3), none())),
                r("A2", bin(0x77, byte(6), byte(7), none())),
                m(
                    "A3",
                    &[
                        divide(byte(17), byte(5), local(0), local(1)),
                        ret(add(
                            local(0),
                            bin(0x77, local(1), byte(100), none()),
                            none(),
                        )),
                    ],
                ),
                r("A4", bin(0x85, byte(17), byte(5), none())),
                r("A5", bin(0x79, one(), byte(4), none())),
                r("A6", bin(0x7A, byte(0x80), byte(3), none())),
                r("A7", bin(0x7B, byte(0x0F), byte(0x3C), none())),
                r("A8", bin(0x7C, ones(), ones(), none())),
                r("A9", bin(0x7D, byte(0x0F), byte(0x30), none())),
                r("AA", bin(0x7E, zero(), zero(), none())),
                r("AB", bin(0x7F, byte(0x0F), byte(0x3C), none())),
                r("AC", un(0x80, zero(), none())),
                r("AD", un(0x81, byte(0x10), none())),
                r("AE", un(0x82, byte(0x10), none())),
                r("AF", un(0x81, zero(), none())),
                m(
                    "AG",
                    &[store(byte(9), local(2)), increment(local(2)), ret(local(2))],
                ),
                m(
                    "AH",
                    &[store(zero(), local(3)), decrement(local(3)), ret(local(3))],
                ),
                r("AI", bin(0x79, one(), byte(64), none())),
                r("AJ", add(ones(), one(), none())),
                r("AK", un_ext(0x28, word(0x1234), none())),
                r("AL", un_ext(0x29, word(1234), none())),
                r("D0", divide(one(), zero(), none(), none())),
                r("D1", bin(0x85, one(), zero(), none())),
                r("D2", un_ext(0x28, byte(0x1A), none())),
            ],
            |i| {
                assert_eq!(int(i, "A0"), 5);
                assert_eq!(int(i, "A1"), u64::MAX);
                assert_eq!(int(i, "A2"), 42);
                assert_eq!(int(i, "A3"), 2 + 300);
                assert_eq!(int(i, "A4"), 2);
                assert_eq!(int(i, "A5"), 16);
                assert_eq!(int(i, "A6"), 0x10);
                assert_eq!(int(i, "A7"), 0x0C);
                assert_eq!(int(i, "A8"), 0);
                assert_eq!(int(i, "A9"), 0x3F);
                assert_eq!(int(i, "AA"), u64::MAX);
                assert_eq!(int(i, "AB"), 0x33);
                assert_eq!(int(i, "AC"), u64::MAX);
                assert_eq!(int(i, "AD"), 5);
                assert_eq!(int(i, "AE"), 5);
                assert_eq!(int(i, "AF"), 0);
                assert_eq!(int(i, "AG"), 10);
                assert_eq!(int(i, "AH"), u64::MAX);
                assert_eq!(int(i, "AI"), 0);
                assert_eq!(int(i, "AJ"), 0);
                assert_eq!(int(i, "AK"), 1234);
                assert_eq!(int(i, "AL"), 0x1234);
                fails(i, "D0", ErrorKind::DivideByZero);
                fails(i, "D1", ErrorKind::DivideByZero);
                fails(i, "D2", ErrorKind::Type);
            },
        ),
        case(
            "logic and comparisons",
            vec![
                r("L0", logic(0x90, one(), byte(2))),
                r("L1", logic(0x90, one(), zero())),
                r("L2", logic(0x91, zero(), one())),
                r("L3", lnot(zero())),
                r("L4", logic(0x93, string("ab"), string("ab"))),
                r("L5", logic(0x94, string("b"), string("ab"))),
                r("L6", logic(0x95, string("ab"), string("abc"))),
                r("L7", lnot_pair(0x93, byte(1), byte(2))),
                r("L8", lnot_pair(0x94, byte(2), byte(2))),
                r("L9", lnot_pair(0x95, byte(1), byte(2))),
                r(
                    "LA",
                    logic(0x93, buffer(byte(2), &[1, 2]), buffer(byte(2), &[1, 2])),
                ),
                r(
                    "LB",
                    logic(0x95, buffer(byte(1), &[1]), buffer(byte(2), &[1, 0])),
                ),
                r("LC", logic(0x93, byte(0x10), string("10"))),
            ],
            |i| {
                assert_eq!(int(i, "L0"), u64::MAX);
                assert_eq!(int(i, "L1"), 0);
                assert_eq!(int(i, "L2"), u64::MAX);
                assert_eq!(int(i, "L3"), u64::MAX);
                assert_eq!(int(i, "L4"), u64::MAX);
                assert_eq!(int(i, "L5"), u64::MAX);
                assert_eq!(int(i, "L6"), u64::MAX);
                assert_eq!(int(i, "L7"), u64::MAX);
                assert_eq!(int(i, "L8"), u64::MAX);
                assert_eq!(int(i, "L9"), 0);
                assert_eq!(int(i, "LA"), u64::MAX);
                assert_eq!(int(i, "LB"), u64::MAX);
                assert_eq!(int(i, "LC"), u64::MAX);
            },
        ),
        case(
            "explicit conversions",
            vec![
                r("T0", un(0x96, one(), none())),
                r("T1", un(0x96, string("ab"), none())),
                r("T2", un(0x98, byte(0x1F), none())),
                r("T3", un(0x98, buffer(byte(2), &[1, 0xAB]), none())),
                r("T4", un(0x97, byte(123), none())),
                r("T5", un(0x97, buffer(byte(2), &[1, 2]), none())),
                r("T6", un(0x99, string("0x1F"), none())),
                r("T7", un(0x99, string("123"), none())),
                r("T8", un(0x99, string("12z"), none())),
                r("T9", to_string(buffer(byte(4), b"ab\0c"), ones(), none())),
                r("TA", to_string(buffer(byte(2), b"ab"), one(), none())),
                r("TB", un(0x99, buffer(byte(2), &[0x34, 0x12]), none())),
                r("TC", un(0x96, string(""), none())),
            ],
            |i| {
                assert_eq!(val(i, "T0"), buf(&[1, 0, 0, 0, 0, 0, 0, 0]));
                assert_eq!(val(i, "T1"), buf(b"ab\0"));
                assert_eq!(val(i, "T2"), s("000000000000001F"));
                assert_eq!(val(i, "T3"), s("01,AB"));
                assert_eq!(val(i, "T4"), s("123"));
                assert_eq!(val(i, "T5"), s("1,2"));
                assert_eq!(int(i, "T6"), 0x1F);
                assert_eq!(int(i, "T7"), 123);
                fails(i, "T8", ErrorKind::Type);
                assert_eq!(val(i, "T9"), s("ab"));
                assert_eq!(val(i, "TA"), s("a"));
                assert_eq!(int(i, "TB"), 0x1234);
                assert_eq!(val(i, "TC"), buf(&[]));
            },
        ),
        case(
            "implicit conversions and stores",
            vec![
                name("IN0", zero()),
                name("ST0", string("")),
                name("BF0", buffer(byte(2), &[])),
                name("PK0", package(1, &[one()])),
                r("I0", add(string("10"), one(), none())),
                m("I1", &[store(word(0x1234), nm("ST0")), ret(nm("ST0"))]),
                m("I2", &[store(string("1"), nm("BF0")), ret(nm("BF0"))]),
                m(
                    "I3",
                    &[
                        store(buffer(byte(3), &[1, 2, 3]), nm("IN0")),
                        ret(nm("IN0")),
                    ],
                ),
                m("I4", &[store(string("x"), nm("IN0")), ret(nm("IN0"))]),
                m(
                    "I5",
                    &[
                        copy_object(string("x"), nm("IN0")),
                        ret(object_type(nm("IN0"))),
                    ],
                ),
                m("I6", &[store(one(), nm("PK0"))]),
                m(
                    "I7",
                    &[
                        store(package(2, &[byte(8), byte(9)]), nm("PK0")),
                        ret(nm("PK0")),
                    ],
                ),
                m(
                    "I8",
                    &[store(buffer(byte(1), &[0x41]), nm("ST0")), ret(nm("ST0"))],
                ),
                r("I9", add(buffer(zero(), &[]), one(), none())),
                r("IA", add(string(""), one(), none())),
            ],
            |i| {
                assert_eq!(int(i, "I0"), 0x11);
                assert_eq!(val(i, "I1"), s("0000000000001234"));
                assert_eq!(val(i, "I2"), buf(&[0x31, 0]));
                assert_eq!(int(i, "I3"), 0x03_0201);
                assert_eq!(int(i, "I4"), 0);
                assert_eq!(int(i, "I5"), 2);
                fails(i, "I6", ErrorKind::Type);
                assert_eq!(
                    val(i, "I7"),
                    Value::Package(vec![Value::Integer(8), Value::Integer(9)])
                );
                assert_eq!(val(i, "I8"), s("41"));
                fails(i, "I9", ErrorKind::Type);
                fails(i, "IA", ErrorKind::Type);
            },
        ),
        case(
            "concatenation",
            vec![
                device("DVX", &[]),
                r("K0", concat(one(), byte(2), none())),
                r("K1", concat(string("ab"), string("cd"), none())),
                r("K2", concat(string("a"), one(), none())),
                r(
                    "K3",
                    concat(buffer(byte(1), &[1]), buffer(byte(1), &[2]), none()),
                ),
                r("K4", concat(buffer(byte(1), &[1]), string("a"), none())),
                r("K5", concat(string("x"), nm("DVX"), none())),
                r("K6", concat(nm("DVX"), string("y"), none())),
                r(
                    "K7",
                    bin(
                        0x84,
                        buffer(byte(5), &[0x22, 0x01, 0x00, 0x79, 0x00]),
                        buffer(byte(10), &[0x47, 1, 2, 3, 4, 5, 6, 7, 0x79, 0x00]),
                        none(),
                    ),
                ),
                r(
                    "K8",
                    bin(0x84, buffer(one(), &[0x22]), buffer(zero(), &[]), none()),
                ),
                r(
                    "K9",
                    bin(0x84, buffer(zero(), &[]), buffer(zero(), &[]), none()),
                ),
            ],
            |i| {
                let mut k0 = vec![1, 0, 0, 0, 0, 0, 0, 0, 2];
                k0.extend([0; 7]);
                assert_eq!(val(i, "K0"), buf(&k0));
                assert_eq!(val(i, "K1"), s("abcd"));
                assert_eq!(val(i, "K2"), s("a0000000000000001"));
                assert_eq!(val(i, "K3"), buf(&[1, 2]));
                assert_eq!(val(i, "K4"), buf(&[1, b'a', 0]));
                assert_eq!(val(i, "K5"), s("x[Device]"));
                assert_eq!(val(i, "K6"), s("[Device]y"));
                let mut k7 = vec![0x22, 0x01, 0x00, 0x47, 1, 2, 3, 4, 5, 6, 7, 0x79];
                let sum = k7.iter().fold(0u8, |a, &b| a.wrapping_add(b));
                k7.push(0u8.wrapping_sub(sum));
                assert_eq!(val(i, "K7"), buf(&k7));
                fails(i, "K8", ErrorKind::Range);
                assert_eq!(val(i, "K9"), buf(&[0x79, 0x87]));
            },
        ),
        case(
            "mid and match",
            vec![
                name("PM0", package(4, &[byte(1), byte(5), byte(10), byte(15)])),
                r("M0", mid(string("abcdef"), byte(2), byte(3), none())),
                r(
                    "M1",
                    mid(buffer(byte(3), &[1, 2, 3]), one(), byte(10), none()),
                ),
                r("M2", mid(string("abc"), byte(5), one(), none())),
                r("M3", match_(nm("PM0"), 5, byte(4), 3, byte(11), zero())),
                r("M4", match_(nm("PM0"), 1, byte(99), 0, zero(), zero())),
                r("M5", match_(nm("PM0"), 2, byte(10), 4, byte(10), zero())),
                r("M6", match_(nm("PM0"), 0, zero(), 0, zero(), byte(3))),
            ],
            |i| {
                assert_eq!(val(i, "M0"), s("cde"));
                assert_eq!(val(i, "M1"), buf(&[2, 3]));
                assert_eq!(val(i, "M2"), s(""));
                assert_eq!(int(i, "M3"), 1);
                assert_eq!(int(i, "M4"), u64::MAX);
                assert_eq!(int(i, "M5"), 2);
                assert_eq!(int(i, "M6"), 3);
            },
        ),
        case(
            "references",
            vec![
                name("PR0", package(3, &[byte(1), byte(2), byte(3)])),
                name("BR0", buffer(byte(4), &[1, 2, 3, 4])),
                name("SR0", string("ABCD")),
                name("IR0", byte(5)),
                m("R0", &[ret(deref_of(index(nm("PR0"), one(), none())))]),
                m(
                    "R1",
                    &[
                        store(dword(0x1234_5678), index(nm("BR0"), byte(2), none())),
                        ret(nm("BR0")),
                    ],
                ),
                m(
                    "R2",
                    &[
                        store(string("H"), index(nm("SR0"), one(), none())),
                        ret(nm("SR0")),
                    ],
                ),
                m(
                    "R3",
                    &[store(ref_of(nm("IR0")), local(0)), ret(deref_of(local(0)))],
                ),
                method("SET", 1, &[store(byte(9), arg(0))]),
                m("R4", &[call("SET", &[ref_of(nm("IR0"))]), ret(nm("IR0"))]),
                m("R5", &[ret(cond_ref_of(nm("\\MISS"), local(0)))]),
                m(
                    "R6",
                    &[
                        if_(
                            cond_ref_of(nm("\\IR0"), local(1)),
                            &[ret(deref_of(local(1)))],
                        ),
                        ret(zero()),
                    ],
                ),
                m("R7", &[ret(deref_of(string("\\IR0")))]),
                m(
                    "R8",
                    &[
                        store(byte(7), index(nm("PR0"), zero(), local(2))),
                        ret(deref_of(local(2))),
                    ],
                ),
                m("R9", &[ret(deref_of(index(nm("BR0"), byte(9), none())))]),
                m(
                    "RA",
                    &[store(byte(1), deref_of(ref_of(nm("IR0")))), ret(nm("IR0"))],
                ),
            ],
            |i| {
                assert_eq!(int(i, "R0"), 2);
                assert_eq!(val(i, "R1"), buf(&[1, 2, 0x78, 4]));
                assert_eq!(val(i, "R2"), s("AHCD"));
                assert_eq!(int(i, "R3"), 5);
                assert_eq!(int(i, "R4"), 9);
                assert_eq!(int(i, "R5"), 0);
                assert_eq!(int(i, "R6"), 9);
                assert_eq!(int(i, "R7"), 9);
                assert_eq!(int(i, "R8"), 7);
                fails(i, "R9", ErrorKind::Range);
                assert_eq!(int(i, "RA"), 1);
            },
        ),
        case(
            "sizes and types",
            vec![
                name("ZB", buffer(byte(3), &[])),
                name("ZS", string("abcd")),
                name("ZP", package(5, &[])),
                name("ZI", one()),
                device("ZD", &[]),
                mutex("ZM", 3),
                event("ZE"),
                op_region("ZR", 1, word(0x400), byte(8)),
                field("ZR", 1, &[Fe::Named("ZF0", 8)]),
                power_resource("ZW", 0, 0, &[]),
                processor("ZC", 1, 0x410, 6, &[]),
                thermal_zone("ZT", &[]),
                create_byte_field(nm("ZB"), zero(), "ZBF"),
                m("ZM0", &[]),
                r("S0", size_of(nm("ZB"))),
                r("S1", size_of(nm("ZS"))),
                r("S2", size_of(nm("ZP"))),
                m(
                    "S3",
                    &[store(ref_of(nm("ZS")), local(0)), ret(size_of(local(0)))],
                ),
                r("S4", size_of(nm("ZI"))),
                r("Y0", object_type(nm("ZI"))),
                r("Y1", object_type(nm("ZS"))),
                r("Y2", object_type(nm("ZB"))),
                r("Y3", object_type(nm("ZP"))),
                r("Y4", object_type(nm("ZF0"))),
                r("Y5", object_type(nm("ZD"))),
                r("Y6", object_type(nm("ZE"))),
                r("Y7", object_type(nm("ZM0"))),
                r("Y8", object_type(nm("ZM"))),
                r("Y9", object_type(nm("ZR"))),
                r("YA", object_type(nm("ZW"))),
                r("YB", object_type(nm("ZC"))),
                r("YC", object_type(nm("ZT"))),
                r("YD", object_type(nm("ZBF"))),
                r("YE", object_type(debug())),
                r("YF", object_type(local(5))),
                r("YG", object_type(nm("\\_SB"))),
                m(
                    "YH",
                    &[
                        store(ref_of(nm("ZD")), local(0)),
                        ret(object_type(local(0))),
                    ],
                ),
            ],
            |i| {
                assert_eq!(int(i, "S0"), 3);
                assert_eq!(int(i, "S1"), 4);
                assert_eq!(int(i, "S2"), 5);
                assert_eq!(int(i, "S3"), 4);
                fails(i, "S4", ErrorKind::Type);
                let expect = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 16, 0, 0, 6];
                let names = [
                    "Y0", "Y1", "Y2", "Y3", "Y4", "Y5", "Y6", "Y7", "Y8", "Y9", "YA", "YB", "YC",
                    "YD", "YE", "YF", "YG", "YH",
                ];
                for (n, e) in names.iter().zip(expect) {
                    assert_eq!(int(i, n), e, "{n}");
                }
            },
        ),
        case(
            "control flow",
            vec![
                method(
                    "F0",
                    1,
                    &[if_else(
                        logic(0x95, arg(0), byte(10)),
                        &[ret(byte(1))],
                        &[if_else(
                            logic(0x95, arg(0), byte(20)),
                            &[ret(byte(2))],
                            &[ret(byte(3))],
                        )],
                    )],
                ),
                m(
                    "F1",
                    &[
                        store(zero(), local(0)),
                        store(zero(), local(1)),
                        while_(
                            logic(0x95, local(0), byte(10)),
                            &[
                                increment(local(0)),
                                if_(
                                    logic(0x93, bin(0x85, local(0), byte(2), none()), zero()),
                                    &[cont()],
                                ),
                                if_(logic(0x93, local(0), byte(9)), &[brk()]),
                                add(local(1), local(0), local(1)),
                            ],
                        ),
                        ret(local(1)),
                    ],
                ),
                m("F2", &[while_(one(), &[while_(one(), &[ret(byte(42))])])]),
                m("F3", &[noop(), breakpoint(), ret(byte(4))]),
                m("F4", &[brk()]),
                m("F5", &[cont()]),
                m("F6", &[if_(zero(), &[ret(one())]), ret(byte(6))]),
            ],
            |i| {
                assert_eq!(
                    i.evaluate("F0", &[Value::Integer(5)]).unwrap(),
                    Some(Value::Integer(1))
                );
                assert_eq!(
                    i.evaluate("F0", &[Value::Integer(15)]).unwrap(),
                    Some(Value::Integer(2))
                );
                assert_eq!(
                    i.evaluate("F0", &[Value::Integer(25)]).unwrap(),
                    Some(Value::Integer(3))
                );
                assert_eq!(int(i, "F1"), 1 + 3 + 5 + 7);
                assert_eq!(int(i, "F2"), 42);
                assert_eq!(int(i, "F3"), 4);
                fails(i, "F4", ErrorKind::NotInLoop);
                fails(i, "F5", ErrorKind::NotInLoop);
                assert_eq!(int(i, "F6"), 6);
            },
        ),
        case(
            "methods, arguments and locals",
            vec![
                method(
                    "SUM7",
                    7,
                    &[
                        store(arg(0), local(0)),
                        add(local(0), arg(1), local(1)),
                        add(local(1), arg(2), local(2)),
                        add(local(2), arg(3), local(3)),
                        add(local(3), arg(4), local(4)),
                        add(local(4), arg(5), local(5)),
                        add(local(5), arg(6), local(6)),
                        store(local(6), local(7)),
                        ret(local(7)),
                    ],
                ),
                m(
                    "CS7",
                    &[ret(call(
                        "SUM7",
                        &[one(), byte(2), byte(3), byte(4), byte(5), byte(6), byte(7)],
                    ))],
                ),
                method(
                    "FACT",
                    1,
                    &[
                        if_(logic(0x95, arg(0), byte(2)), &[ret(one())]),
                        ret(bin(
                            0x77,
                            arg(0),
                            call("FACT", &[bin(0x74, arg(0), one(), none())]),
                            none(),
                        )),
                    ],
                ),
                m("CF", &[ret(call("FACT", &[byte(10)]))]),
                m("NRET", &[store(one(), local(0))]),
                m("USE", &[ret(add(call("NRET", &[]), one(), none()))]),
                m("STMT", &[call("NRET", &[]), ret(byte(3))]),
                name("NB", buffer(byte(2), &[1, 2])),
                m("GETB", &[ret(nm("NB"))]),
                m(
                    "COPY",
                    &[
                        store(call("GETB", &[]), local(0)),
                        store(byte(9), index(local(0), zero(), none())),
                        ret(nm("NB")),
                    ],
                ),
                method("SETA", 1, &[store(byte(5), arg(0)), ret(arg(0))]),
                m(
                    "CA",
                    &[
                        store(one(), local(0)),
                        call("SETA", &[local(0)]),
                        ret(local(0)),
                    ],
                ),
                method("IDXA", 1, &[store(byte(7), index(arg(0), zero(), none()))]),
                m("CI", &[call("IDXA", &[nm("NB")]), ret(nm("NB"))]),
            ],
            |i| {
                assert_eq!(int(i, "CS7"), 28);
                assert_eq!(int(i, "CF"), 3_628_800);
                fails(i, "USE", ErrorKind::NoValue);
                assert_eq!(int(i, "STMT"), 3);
                assert_eq!(val(i, "COPY"), buf(&[1, 2]));
                assert_eq!(int(i, "CA"), 1);
                assert_eq!(val(i, "CI"), buf(&[7, 2]));
                assert!(eval(i, "NRET").unwrap().is_none());
                assert!(i.evaluate("FACT", &[]).is_err());
            },
        ),
        case(
            "dynamic names and aliases",
            vec![
                name("IA0", byte(3)),
                alias("\\IA0", "\\AL0"),
                scope("\\", &[device("XYZ", &[])]),
                scope(
                    "\\XYZ",
                    &[
                        name("BAR", byte(5)),
                        m("FOO", &[name("BAR", byte(7)), ret(nm("BAR"))]),
                        m("FOOG", &[ret(nm("BAR"))]),
                        m(
                            "FOOC",
                            &[name("\\XYZ.FOOB", byte(3)), ret(nm("\\XYZ.FOOB"))],
                        ),
                        m("UP", &[ret(nm("^BAR"))]),
                        m("SRCH", &[ret(nm("IA0"))]),
                    ],
                ),
                m("AS0", &[store(byte(8), nm("AL0")), ret(nm("IA0"))]),
                r("AR0", nm("AL0")),
                r("MS", nm("\\XYZ.BAR")),
                name("PKA", package(1, &[nm("\\AL0")])),
            ],
            |i| {
                assert_eq!(int(i, "\\XYZ.FOO"), 7);
                assert_eq!(int(i, "\\XYZ.FOO"), 7);
                assert!(i.lookup("\\XYZ.FOO.BAR").unwrap().is_none());
                assert_eq!(int(i, "\\XYZ.FOOG"), 5);
                assert_eq!(int(i, "\\XYZ.FOOC"), 3);
                assert!(i.lookup("\\XYZ.FOOB").unwrap().is_none());
                assert_eq!(int(i, "\\XYZ.UP"), 5);
                assert_eq!(int(i, "\\XYZ.SRCH"), 3);
                assert_eq!(int(i, "AR0"), 3);
                assert_eq!(int(i, "AS0"), 8);
                assert_eq!(int(i, "MS"), 5);
                assert_eq!(val(i, "PKA"), Value::Package(vec![Value::Integer(8)]));
            },
        ),
        case(
            "buffer fields",
            vec![
                name(
                    "FB",
                    buffer(
                        byte(16),
                        &[0xFF, 0x00, 0x34, 0x12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                    ),
                ),
                create_bit_field(nm("FB"), byte(8), "BIT0"),
                create_byte_field(nm("FB"), zero(), "BYT0"),
                create_word_field(nm("FB"), byte(2), "WRD0"),
                create_dword_field(nm("FB"), byte(4), "DWD0"),
                create_qword_field(nm("FB"), byte(8), "QWD0"),
                create_field(nm("FB"), byte(4), byte(8), "ODD0"),
                create_field(nm("FB"), zero(), byte(72), "BIG0"),
                r("G0", nm("BYT0")),
                r("G1", nm("WRD0")),
                r("G2", nm("BIT0")),
                r("G3", nm("ODD0")),
                m("G4", &[store(one(), nm("BIT0")), ret(nm("WRD0"))]),
                m("G5", &[store(byte(0xAB), nm("ODD0")), ret(nm("BYT0"))]),
                m("G6", &[store(qword(u64::MAX), nm("QWD0")), ret(nm("DWD0"))]),
                r("G7", nm("BIG0")),
                m("G8", &[create_dword_field(nm("FB"), byte(14), "OUT0")]),
                m("G9", &[create_field(nm("FB"), zero(), zero(), "NUL0")]),
            ],
            |i| {
                assert_eq!(int(i, "G0"), 0xFF);
                assert_eq!(int(i, "G1"), 0x1234);
                assert_eq!(int(i, "G2"), 0);
                assert_eq!(int(i, "G3"), 0x0F);
                assert_eq!(int(i, "G4"), 0x1234);
                assert_eq!(int(i, "G5"), 0xBF);
                assert_eq!(int(i, "G6"), 0);
                match val(i, "G7") {
                    Value::Buffer(b) => assert_eq!(b.len(), 9),
                    other => panic!("G7 {other:?}"),
                }
                fails(i, "G8", ErrorKind::Range);
                fails(i, "G9", ErrorKind::Range);
            },
        ),
        case(
            "regions and fields, read and written through the host",
            vec![
                op_region("OR0", 1, word(0x80), byte(4)),
                field(
                    "OR0",
                    0x01,
                    &[
                        Fe::Named("IDX0", 8),
                        Fe::Named("DAT0", 8),
                        Fe::Reserved(8),
                        Fe::Access(0x03, 0),
                        Fe::Named("BNK0", 8),
                    ],
                ),
                index_field(
                    "IDX0",
                    "DAT0",
                    0x01,
                    &[Fe::Named("IF0", 8), Fe::Named("IF1", 1)],
                ),
                bank_field(
                    "OR0",
                    "BNK0",
                    one(),
                    0x01,
                    &[Fe::Reserved(16), Fe::Named("BF0", 8)],
                ),
                op_region("OR1", 9, zero(), byte(16)),
                field(
                    "OR1",
                    0x05,
                    &[
                        Fe::ConnectBuffer(vec![0x8E, 0x02, 0x00]),
                        Fe::ExtAccess(0x05, 0x0B, 4),
                        Fe::Named("SB0", 32),
                        Fe::ConnectName("CRS0"),
                        Fe::Named("SB1", 8),
                    ],
                ),
                name("CRS0", buffer(byte(2), &[0x79, 0])),
                m("RD0", &[store(byte(0x5A), nm("DAT0")), ret(nm("DAT0"))]),
                m("WR0", &[store(byte(0x3C), nm("IF0")), ret(nm("IF0"))]),
                m("WB0", &[store(byte(0x77), nm("BF0")), ret(nm("BF0"))]),
                r("SB", store(buffer(byte(2), &[1, 2]), nm("SB0"))),
                r("FT", object_type(nm("BF0"))),
                r("FT2", object_type(nm("SB1"))),
            ],
            |i| {
                assert_eq!(int(i, "RD0"), 0x5A);
                assert_eq!(int(i, "WR0"), 0x3C);
                assert_eq!(int(i, "WB0"), 0x77);
                i.host_mut().answer = vec![0, 2, 0xAA, 0xBB];
                assert_eq!(val(i, "SB"), buf(&[0, 2, 0xAA, 0xBB]));
                assert_eq!(int(i, "FT"), 5);
                assert_eq!(int(i, "FT2"), 5);
            },
        ),
        case(
            "what reaches the host",
            vec![
                mutex("MX0", 0),
                event("EV0"),
                device("DN0", &[]),
                external("\\EXT0", 8, 2),
                m(
                    "H0",
                    &[
                        store(acquire(nm("MX0"), 0xFFFF), local(0)),
                        release(nm("MX0")),
                        ret(local(0)),
                    ],
                ),
                m("H1", &[release(nm("MX0"))]),
                m("H2", &[signal(nm("EV0")), ret(wait(nm("EV0"), zero()))]),
                m("H3", &[ret(wait(nm("EV0"), one()))]),
                m(
                    "H4",
                    &[
                        signal(nm("EV0")),
                        reset(nm("EV0")),
                        ret(wait(nm("EV0"), one())),
                    ],
                ),
                m("H5", &[sleep(byte(2)), ret(one())]),
                m("H6", &[stall(byte(255)), ret(one())]),
                m("H7", &[notify(nm("DN0"), byte(0x80)), ret(one())]),
                m("H8", &[fatal(1, 2, byte(3))]),
                r("H9", timer()),
                m("HA", &[load_op("MX0", none())]),
                m(
                    "HB",
                    &[load_table([
                        string("OEM1"),
                        string(""),
                        string(""),
                        string(""),
                        string(""),
                        zero(),
                    ])],
                ),
                m(
                    "HC",
                    &[data_region("DRG0", string("DSDT"), string(""), string(""))],
                ),
                m("HD", &[store(string("hello"), debug()), ret(one())]),
                m("HE", &[breakpoint(), ret(one())]),
            ],
            |i| {
                use Feature as F;
                assert_eq!(int(i, "H0"), 0);
                fails(i, "H1", ErrorKind::NotOwned);
                assert_eq!(int(i, "H2"), 0);
                assert_eq!(int(i, "H3"), u64::MAX);
                assert_eq!(int(i, "H4"), u64::MAX);
                let before = i.host().clock;
                assert_eq!(int(i, "H5"), 1);
                assert!(i.host().clock >= before + 20_000);
                assert_eq!(int(i, "H6"), 1);
                assert_eq!(int(i, "H7"), 1);
                assert_eq!(i.host().notified.last().map(|n| n.1), Some(0x80));
                fails(
                    i,
                    "H8",
                    ErrorKind::Fatal {
                        kind: 1,
                        code: 2,
                        arg: 3,
                    },
                );
                assert_eq!(i.host().fatals, vec![(1, 2, 3)]);
                let t = int(i, "H9");
                assert!(t > 0 && t <= i.host().clock);
                fails(i, "HA", ErrorKind::NotSupported(F::Load));
                fails(i, "HB", ErrorKind::NotSupported(F::LoadTable));
                fails(i, "HC", ErrorKind::NotFound);
                assert_eq!(int(i, "HD"), 1);
                assert_eq!(
                    i.host().debug.last().map(Vec::as_slice),
                    Some(&b"hello"[..])
                );
                assert_eq!(int(i, "HE"), 1);
                assert_eq!(i.host().breakpoints, 1);
                assert!(i.lookup("\\EXT0").unwrap().is_none());
            },
        ),
        case(
            "predefined objects",
            vec![
                r("O0", call("\\_OSI", &[string("Windows 2022")])),
                r("O1", call("\\_OSI", &[string("Linux")])),
                r("O2", call("\\_OSI", &[string("Module Device")])),
                m("O3", &[ret(cond_ref_of(nm("\\_OSI"), none()))]),
                r("O4", nm("\\_OS")),
                r("O5", nm("\\_REV")),
                r("O6", object_type(nm("\\_GL"))),
            ],
            |i| {
                assert_eq!(int(i, "O0"), u64::MAX);
                assert_eq!(int(i, "O1"), 0);
                assert_eq!(int(i, "O2"), 0);
                assert_eq!(int(i, "O3"), u64::MAX);
                assert_eq!(val(i, "O4"), s("Origin OS"));
                assert_eq!(int(i, "O5"), 2);
                assert_eq!(int(i, "O6"), 9);
            },
        ),
        case(
            "load-time code and multi-segment names",
            vec![
                name("CFG1", one()),
                if_else(
                    logic(0x93, nm("CFG1"), one()),
                    &[device("\\_SB.YES", &[])],
                    &[device("\\_SB.NO", &[])],
                ),
                scope(
                    "\\_SB",
                    &[device(
                        "D1",
                        &[device("D2", &[device("D3", &[name("V", byte(4))])])],
                    )],
                ),
                r("MN0", nm("\\_SB.D1.D2.D3.V")),
                r("MN1", nm("\\_SB.D1")),
            ],
            |i| {
                assert!(i.lookup("\\_SB.YES").unwrap().is_some());
                assert!(i.lookup("\\_SB.NO").unwrap().is_none());
                assert_eq!(int(i, "MN0"), 4);
                let d1 = i.lookup("\\_SB.D1").unwrap().unwrap();
                assert_eq!(val(i, "MN1"), Value::Reference(d1));
            },
        ),
    ]
}

fn run_corpus() {
    for c in corpus() {
        let mut i = interp(c.width);
        if let Err(e) = load(&mut i, &c.body) {
            panic!("{}: load failed: {e:?}", c.name);
        }
        (c.check)(&mut i);
    }
}

#[test]
fn every_case_holds() {
    run_corpus();
}

/// Every opcode, name prefix, local and argument of Table 20.2.
fn table_20_2() -> Vec<u16> {
    let mut all: Vec<u16> = vec![
        0x00, 0x01, 0x06, 0x08, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15,
        0x2E, 0x2F, 0x5C, 0x5E, 0x9C, 0x9D, 0x9E, 0x9F, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xCC,
        0xFF, 0x9293, 0x9294, 0x9295,
    ];
    all.extend(0x60..=0x6E);
    all.extend(0x70..=0x99);
    for x in [
        0x01, 0x02, 0x12, 0x13, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29,
        0x30, 0x31, 0x32, 0x33, 0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88,
    ] {
        all.push(0x5B00 | x);
    }
    all.sort_unstable();
    all
}

#[test]
fn every_opcode_is_emitted_and_checked() {
    let _ = take_emitted();
    run_corpus();
    let emitted = take_emitted();
    let missing: Vec<String> = table_20_2()
        .into_iter()
        .filter(|c| !emitted.contains(c))
        .map(|c| format!("{c:#x}"))
        .collect();
    assert!(missing.is_empty(), "opcodes no case emits: {missing:?}");
}

#[test]
fn a_stray_else_is_out_of_place() {
    let mut i = interp(IntegerWidth::Bits64);
    let e = load(&mut i, &[noop(), else_(&[noop()])]).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Grammar);
    assert_eq!(e.at.map(|a| a.offset), Some(1));
}

#[test]
fn reserved_opcodes_are_refused_with_their_place() {
    for bad in [
        &[0x02u8][..],
        &[0x07],
        &[0x16],
        &[0x5B, 0x00],
        &[0x5B, 0x2A],
        &[0x5B, 0x89],
        &[0xCD],
    ] {
        let mut i = interp(IntegerWidth::Bits64);
        let mut body = vec![noop()];
        body.push(bad.to_vec());
        let e = load(&mut i, &body).unwrap_err();
        assert!(
            matches!(e.kind, ErrorKind::Reserved { .. }),
            "{bad:x?}: {e:?}"
        );
        assert_eq!(e.at.map(|a| a.offset), Some(1), "{bad:x?}");
    }
}
