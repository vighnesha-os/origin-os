//! Step B2: what reaches the host. Field units in each kind of space, the PCI function of a
//! PCI_Config region, mutexes and the Global Lock, events, time, `Notify`, `Fatal`, the
//! initialization walk and `_REG`, each against the model host of `mod.rs`.

use super::encode::*;
use super::{Interp, Seen, interp, interp_with, load};
use crate::{
    ErrorKind, HostError, InitOutcome, IntegerWidth, Limit, Limits, PciAddress, Space, TableKey,
    Value,
};

fn m(path: &str, body: &[B]) -> B {
    method(path, 0, body)
}

fn loaded(body: &[B]) -> Interp {
    let mut i = interp(IntegerWidth::Bits64);
    load(&mut i, body).expect("the block loads");
    i
}

#[track_caller]
fn int(i: &mut Interp, path: &str) -> u64 {
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

/// The accesses since the last call, as (write, address, width, value).
fn taken(i: &mut Interp) -> Vec<(bool, u64, u8, u64)> {
    std::mem::take(&mut i.host_mut().seen)
        .into_iter()
        .map(|s| (s.write, s.address, s.width, s.value))
        .collect()
}

const PRESERVE: u8 = 0x00;
const ONES: u8 = 0x20;
const ZEROS: u8 = 0x40;
const ANY: u8 = 0;
const BYTE: u8 = 1;
const WORD: u8 = 2;
const DWORD: u8 = 3;

#[test]
fn a_byte_field_inside_a_byte_is_read_modified_and_written() {
    let mut i = loaded(&[
        op_region("IO", 1, word(0x80), byte(4)),
        field(
            "IO",
            BYTE | PRESERVE,
            &[
                Fe::Named("LOW", 8),
                Fe::Named("NIBA", 4),
                Fe::Named("NIBB", 4),
            ],
        ),
        m("SET", &[store(byte(0xA), nm("NIBA")), ret(nm("NIBB"))]),
        m("ALL", &[store(byte(0xC3), nm("LOW")), ret(nm("LOW"))]),
    ]);
    i.host_mut().memory.insert((1, None, 0x81), 0x50);
    assert_eq!(int(&mut i, "SET"), 5);
    assert_eq!(
        taken(&mut i),
        vec![
            (false, 0x81, 8, 0x50),
            (true, 0x81, 8, 0x5A),
            (false, 0x81, 8, 0x5A)
        ]
    );
    // A unit the field covers whole is written without a read.
    assert_eq!(int(&mut i, "ALL"), 0xC3);
    assert_eq!(
        taken(&mut i),
        vec![(true, 0x80, 8, 0xC3), (false, 0x80, 8, 0xC3)]
    );
}

#[test]
fn the_update_rule_fills_the_rest_of_a_unit() {
    let mut i = loaded(&[
        op_region("IO", 1, word(0x80), byte(4)),
        field("IO", BYTE | ONES, &[Fe::Reserved(2), Fe::Named("ONE", 3)]),
        field("IO", BYTE | ZEROS, &[Fe::Reserved(10), Fe::Named("ZER", 3)]),
        m("W1", &[store(zero(), nm("ONE")), ret(one())]),
        m("W0", &[store(byte(7), nm("ZER")), ret(one())]),
    ]);
    int(&mut i, "W1");
    assert_eq!(taken(&mut i), vec![(true, 0x80, 8, 0b1110_0011)]);
    int(&mut i, "W0");
    assert_eq!(taken(&mut i), vec![(true, 0x81, 8, 0b0001_1100)]);
}

#[test]
fn access_widths_and_any_access() {
    let mut i = loaded(&[
        op_region("MEM", 0, dword(0x1000), byte(16)),
        // A word field at bit 8 under WordAcc spans two words.
        field("MEM", WORD, &[Fe::Reserved(8), Fe::Named("WSPN", 16)]),
        // AnyAcc: bits 8 to 23 fit one aligned dword (G5).
        field("MEM", ANY, &[Fe::Reserved(8), Fe::Named("ANY1", 16)]),
        // DWordAcc on a 64-bit field: two dwords.
        field("MEM", DWORD, &[Fe::Reserved(64), Fe::Named("QW", 64)]),
        m("RW", &[ret(nm("WSPN"))]),
        m("RA", &[ret(nm("ANY1"))]),
        m(
            "RQ",
            &[store(qword(0x1122_3344_5566_7788), nm("QW")), ret(nm("QW"))],
        ),
    ]);
    for (k, b) in [(0x1001u64, 0x34u8), (0x1002, 0x12)] {
        i.host_mut().memory.insert((0, None, k), b);
    }
    assert_eq!(int(&mut i, "RW"), 0x1234);
    assert_eq!(
        taken(&mut i),
        vec![(false, 0x1000, 16, 0x3400), (false, 0x1002, 16, 0x12)]
    );
    assert_eq!(int(&mut i, "RA"), 0x1234);
    assert_eq!(taken(&mut i), vec![(false, 0x1000, 32, 0x12_3400)]);
    assert_eq!(int(&mut i, "RQ"), 0x1122_3344_5566_7788);
    let t = taken(&mut i);
    assert_eq!(
        t[..2],
        [
            (true, 0x1008, 32, 0x5566_7788),
            (true, 0x100C, 32, 0x1122_3344)
        ]
    );
}

#[test]
fn a_field_past_its_region_is_refused_and_so_is_a_refused_access() {
    let mut i = loaded(&[
        op_region("IO", 1, word(0x80), one()),
        field("IO", WORD, &[Fe::Named("WIDE", 16)]),
        op_region("OK", 1, word(0x90), one()),
        field("OK", BYTE, &[Fe::Named("B", 8)]),
        m("RW", &[ret(nm("WIDE"))]),
        m("RB", &[ret(nm("B"))]),
    ]);
    assert_eq!(fails(&mut i, "RW"), ErrorKind::Range);
    i.host_mut().refuse = true;
    assert_eq!(
        fails(&mut i, "RB"),
        ErrorKind::Region {
            space: 1,
            address: 0x90,
            cause: HostError::Refused
        }
    );
}

#[test]
fn byte_only_spaces_are_read_a_byte_at_a_time() {
    let mut i = loaded(&[
        op_region("EC", 3, zero(), byte(0x10)),
        field("EC", DWORD, &[Fe::Named("TEMP", 32)]),
        m("RT", &[ret(nm("TEMP"))]),
    ]);
    i.host_mut().memory.insert((3, None, 2), 0x40);
    assert_eq!(int(&mut i, "RT"), 0x40_0000);
    let t = taken(&mut i);
    assert_eq!(t.len(), 4);
    assert!(t.iter().all(|a| a.2 == 8));
}

#[test]
fn a_lock_field_takes_the_global_lock_around_its_access() {
    let mut i = loaded(&[
        op_region("MEM", 0, dword(0x2000), byte(4)),
        field("MEM", DWORD | 0x10, &[Fe::Named("LK", 32)]),
        m("R", &[ret(nm("LK"))]),
    ]);
    int(&mut i, "R");
    assert_eq!(i.host().global_taken, 1);
    assert_eq!(i.host().global_held, 0);
    // It waits for ever, cut to the time left: a timeout of that is the time limit (the review of
    // B1, F9).
    i.host_mut().global_times_out = true;
    assert_eq!(fails(&mut i, "R"), ErrorKind::Limit(Limit::Time));
}

#[test]
fn index_and_bank_fields_go_through_their_registers() {
    let mut i = loaded(&[
        op_region("IO", 1, word(0x70), byte(2)),
        field("IO", BYTE, &[Fe::Named("IDX", 8), Fe::Named("DAT", 8)]),
        index_field("IDX", "DAT", BYTE, &[Fe::Reserved(16), Fe::Named("CFG", 8)]),
        op_region("BK", 1, word(0x60), byte(4)),
        field("BK", BYTE, &[Fe::Named("SEL", 8)]),
        bank_field(
            "BK",
            "SEL",
            byte(3),
            BYTE,
            &[Fe::Reserved(16), Fe::Named("BNKD", 8)],
        ),
        m("WI", &[store(byte(0x99), nm("CFG")), ret(one())]),
        m("RB", &[ret(nm("BNKD"))]),
    ]);
    int(&mut i, "WI");
    assert_eq!(
        taken(&mut i),
        vec![(true, 0x70, 8, 2), (true, 0x71, 8, 0x99)]
    );
    int(&mut i, "RB");
    assert_eq!(taken(&mut i), vec![(true, 0x60, 8, 3), (false, 0x62, 8, 0)]);
}

#[test]
fn a_pci_config_region_names_its_function_through_the_bridges_above_it() {
    let mut i = loaded(&[scope(
        "\\_SB",
        &[device(
            "PCI0",
            &[
                name("_HID", dword(0x080A_D041)),
                name("_SEG", one()),
                name("_BBN", byte(0x10)),
                device(
                    "BR0",
                    &[
                        name("_ADR", dword(0x0001_0000)),
                        device(
                            "DEV",
                            &[
                                method("_ADR", 0, &[ret(dword(0x0002_0003))]),
                                op_region("CFG", 2, zero(), word(0x100)),
                                field("CFG", BYTE, &[Fe::Named("VID", 8)]),
                            ],
                        ),
                    ],
                ),
                device(
                    "TOP",
                    &[
                        name("_ADR", dword(0x001F_0000)),
                        op_region("CFG", 2, zero(), word(0x100)),
                        field("CFG", BYTE, &[Fe::Named("VID", 8)]),
                    ],
                ),
            ],
        )],
    )]);
    // The bridge at bus 0x10, device 1, function 0 says its secondary bus is 0x22.
    i.host_mut()
        .memory
        .insert((2, Some((1, 0x10, 1, 0)), 0x19), 0x22);
    i.evaluate("\\_SB.PCI0.BR0.DEV.VID", &[]).unwrap();
    let last = i.host().seen.last().cloned().unwrap();
    assert_eq!(
        last.pci,
        Some(PciAddress {
            segment: 1,
            bus: 0x22,
            device: 2,
            function: 3
        })
    );
    i.evaluate("\\_SB.PCI0.TOP.VID", &[]).unwrap();
    let last: Seen = i.host().seen.last().cloned().unwrap();
    assert_eq!(
        last.pci,
        Some(PciAddress {
            segment: 1,
            bus: 0x10,
            device: 0x1F,
            function: 0
        })
    );
}

#[test]
fn buffer_protocol_fields_make_one_transfer_with_their_connection() {
    let mut i = loaded(&[
        name("CON", buffer(byte(3), &[0x8E, 1, 2])),
        op_region("SMB", 4, zero(), word(0x100)),
        field(
            "SMB",
            0x05,
            &[
                Fe::Reserved(0x30 * 8),
                Fe::Access(0x05, 0x08),
                Fe::Named("CMD", 8),
            ],
        ),
        op_region("GSB", 9, zero(), word(0x100)),
        field(
            "GSB",
            0x05,
            &[
                Fe::ConnectName("CON"),
                Fe::Access(0x45, 16),
                Fe::Named("REG", 8),
            ],
        ),
        m(
            "WS",
            &[ret(store(buffer(byte(4), &[0, 2, 0x34, 0x12]), nm("CMD")))],
        ),
        m("RG", &[ret(nm("REG"))]),
    ]);
    i.host_mut().answer = vec![0, 2, 0xAA, 0xBB];
    assert_eq!(
        i.evaluate("WS", &[]).unwrap(),
        Some(Value::Buffer(vec![0, 2, 0xAA, 0xBB]))
    );
    let (seen, _) = i.host().transfers.last().cloned().unwrap();
    assert_eq!(
        (seen.space, seen.address, seen.write, seen.attribute),
        (4, 0x30, true, 0x08)
    );
    assert_eq!(seen.sent.len(), 34);
    assert_eq!(&seen.sent[..4], &[0, 2, 0x34, 0x12]);
    assert_eq!(
        i.evaluate("RG", &[]).unwrap(),
        Some(Value::Buffer(vec![0, 2, 0xAA, 0xBB]))
    );
    let (seen, _) = i.host().transfers.last().cloned().unwrap();
    assert_eq!(seen.connection, Some(vec![0x8E, 1, 2]));
    assert_eq!((seen.attribute, seen.length, seen.write), (0x0B, 16, false));
}

#[test]
fn a_data_table_region_reads_the_table_the_host_names() {
    let mut i = interp(IntegerWidth::Bits64);
    i.host_mut().tables.push((
        TableKey {
            signature: *b"OEMX",
            oem_id: *b"ORIGIN",
            oem_table_id: *b"AMLTEST\0",
        },
        0x7000,
        // A sound table of 64 bytes: the copy is checked before its address is taken (B2-14).
        table(b"OEMX", 2, &[vec![0; 28]]),
    ));
    load(
        &mut i,
        &[m(
            "RD",
            &[
                data_region("DTR", string("OEMX"), string("ORIGIN"), string("AMLTEST")),
                field("DTR", BYTE, &[Fe::Reserved(8 * 10), Fe::Named("B10", 8)]),
                ret(nm("B10")),
            ],
        )],
    )
    .unwrap();
    i.host_mut().memory.insert((0, None, 0x700A), 0x42);
    assert_eq!(int(&mut i, "RD"), 0x42);
}

/// The review of B2, B2-14 (built in B4): a copy that is not a sound table under the key gives
/// the region nothing, and nothing is read.
#[test]
fn a_data_table_region_is_refused_on_a_copy_that_fails_its_check() {
    let sound = table(b"OEMX", 2, &[vec![0; 28]]);
    let resum = |mut t: Vec<u8>| {
        t[9] = 0;
        let sum = t.iter().fold(0u8, |s, &b| s.wrapping_add(b));
        t[9] = 0u8.wrapping_sub(sum);
        t
    };
    let cases: [(&str, Vec<u8>); 5] = [
        ("a bad checksum", {
            let mut t = sound.clone();
            t[40] ^= 1;
            t
        }),
        ("a length not the copy's", {
            let mut t = sound.clone();
            t[4] = 63;
            resum(t)
        }),
        ("a short header", sound[..30].to_vec()),
        (
            "another signature",
            resum({
                let mut t = sound.clone();
                t[3] = b'Y';
                t
            }),
        ),
        (
            "other IDs",
            resum({
                let mut t = sound.clone();
                t[10] = b'X';
                t
            }),
        ),
    ];
    for (what, bytes) in cases {
        let mut i = interp(IntegerWidth::Bits64);
        i.host_mut().tables.push((
            TableKey {
                signature: *b"OEMX",
                oem_id: *b"ORIGIN",
                oem_table_id: *b"AMLTEST\0",
            },
            0x7000,
            bytes,
        ));
        load(
            &mut i,
            &[m(
                "RD",
                &[
                    data_region("DTR", string("OEMX"), string("ORIGIN"), string("AMLTEST")),
                    ret(one()),
                ],
            )],
        )
        .unwrap();
        assert_eq!(
            i.evaluate("RD", &[]).map_err(|e| e.kind),
            Err(ErrorKind::Host(HostError::Failed)),
            "{what}"
        );
        assert!(i.host().seen.is_empty(), "{what}");
    }
}

#[test]
fn strings_are_written_a_character_at_a_time_and_long_buffers_in_pieces() {
    let mut i = loaded(&[
        op_region("IO", 1, word(0x80), one()),
        field("IO", BYTE, &[Fe::Named("PORT", 8)]),
        m("WS", &[store(string("AB"), nm("PORT")), ret(one())]),
        m(
            "WB",
            &[store(buffer(byte(3), &[1, 2, 3]), nm("PORT")), ret(one())],
        ),
    ]);
    int(&mut i, "WS");
    assert_eq!(
        taken(&mut i),
        vec![(true, 0x80, 8, 0x41), (true, 0x80, 8, 0x42)]
    );
    int(&mut i, "WB");
    assert_eq!(
        taken(&mut i),
        vec![(true, 0x80, 8, 1), (true, 0x80, 8, 2), (true, 0x80, 8, 3)]
    );
}

#[test]
fn mutexes_nest_keep_their_sync_levels_and_are_never_left_held() {
    let mut i = loaded(&[
        mutex("LO", 2),
        mutex("HI", 9),
        m(
            "NEST",
            &[
                store(acquire(nm("LO"), 0xFFFF), local(0)),
                acquire(nm("LO"), 0xFFFF),
                release(nm("LO")),
                release(nm("LO")),
                ret(local(0)),
            ],
        ),
        m(
            "DOWN",
            &[acquire(nm("HI"), 0xFFFF), acquire(nm("LO"), 0xFFFF)],
        ),
        m(
            "ORDR",
            &[
                acquire(nm("LO"), 0xFFFF),
                acquire(nm("HI"), 0xFFFF),
                release(nm("LO")),
            ],
        ),
        m("KEEP", &[acquire(nm("LO"), 0xFFFF), ret(one())]),
        m(
            "GL",
            &[
                acquire(nm("\\_GL"), 0xFFFF),
                release(nm("\\_GL")),
                ret(one()),
            ],
        ),
        method("SER", 8 | (5 << 4), &[ret(call("INR", &[]))]),
        m(
            "INR",
            &[acquire(nm("LO"), 0xFFFF), release(nm("LO")), ret(one())],
        ),
        m("PASS", &[ret(call("TAKE", &[nm("LO")]))]),
        method(
            "TAKE",
            1,
            &[acquire(arg(0), 10), release(arg(0)), ret(byte(7))],
        ),
    ]);
    assert_eq!(int(&mut i, "NEST"), 0);
    assert_eq!(fails(&mut i, "DOWN"), ErrorKind::SyncLevel);
    assert_eq!(fails(&mut i, "ORDR"), ErrorKind::SyncLevel);
    assert_eq!(fails(&mut i, "KEEP"), ErrorKind::MutexHeld);
    // Released by the failed evaluations: taken again at once.
    assert_eq!(int(&mut i, "NEST"), 0);
    assert_eq!(int(&mut i, "GL"), 1);
    assert_eq!((i.host().global_taken, i.host().global_held), (1, 0));
    // A serialized method at sync level 5 calls one that takes a level-2 mutex: refused.
    assert_eq!(fails(&mut i, "SER"), ErrorKind::SyncLevel);
    assert_eq!(int(&mut i, "PASS"), 7);
}

#[test]
fn events_count_signals_and_a_wait_for_ever_is_refused() {
    let mut i = loaded(&[
        event("EV"),
        m(
            "TWO",
            &[
                signal(nm("EV")),
                signal(nm("EV")),
                store(wait(nm("EV"), zero()), local(0)),
                ret(add(local(0), wait(nm("EV"), zero()), none())),
            ],
        ),
        m("EMPT", &[ret(wait(nm("EV"), byte(5)))]),
        m("EVER", &[ret(wait(nm("EV"), word(0xFFFF)))]),
    ]);
    assert_eq!(int(&mut i, "TWO"), 0);
    let before = i.host().clock;
    assert_eq!(int(&mut i, "EMPT"), u64::MAX);
    assert!(i.host().clock >= before + 50_000);
    assert_eq!(fails(&mut i, "EVER"), ErrorKind::WouldBlock);
}

#[test]
fn sleep_is_charged_before_it_runs_and_stall_takes_a_byte() {
    let limits = Limits {
        time_100ns: 1_000_000,
        ..Limits::SERVICE
    };
    let mut i = interp_with(IntegerWidth::Bits64, limits);
    load(
        &mut i,
        &[
            m("NAP", &[sleep(byte(50)), ret(one())]),
            m("LONG", &[sleep(word(500)), ret(one())]),
            m("ST1", &[stall(byte(255)), ret(one())]),
            m("ST2", &[stall(word(256)), ret(one())]),
        ],
    )
    .unwrap();
    assert_eq!(int(&mut i, "NAP"), 1);
    let before = i.host().clock;
    assert_eq!(fails(&mut i, "LONG"), ErrorKind::Limit(Limit::Time));
    assert!(
        i.host().clock < before + 5_000_000,
        "refused before sleeping"
    );
    assert_eq!(int(&mut i, "ST1"), 1);
    assert_eq!(fails(&mut i, "ST2"), ErrorKind::Range);
}

#[test]
fn notify_goes_to_devices_processors_and_thermal_zones_only() {
    let mut i = loaded(&[
        scope(
            "\\_SB",
            &[device("DEV", &[]), processor("CPU0", 0, 0, 0, &[])],
        ),
        thermal_zone("\\_TZ.TZ0", &[]),
        name("NAM", one()),
        m(
            "N1",
            &[
                notify(nm("\\_SB.DEV"), byte(0x80)),
                notify(nm("\\_SB.CPU0"), byte(0x81)),
                notify(nm("\\_TZ.TZ0"), byte(0x82)),
                ret(one()),
            ],
        ),
        m("N2", &[notify(nm("NAM"), one())]),
        m("N3", &[ret(call("NARG", &[nm("\\_SB.DEV")]))]),
        method("NARG", 1, &[notify(arg(0), byte(0x83)), ret(one())]),
    ]);
    int(&mut i, "N1");
    let n: Vec<_> = i
        .host()
        .notified
        .iter()
        .map(|(p, v)| (p.clone(), *v))
        .collect();
    assert_eq!(
        n,
        vec![
            (vec![*b"_SB_", *b"DEV_"], 0x80),
            (vec![*b"_SB_", *b"CPU0"], 0x81),
            (vec![*b"_TZ_", *b"TZ0_"], 0x82)
        ]
    );
    assert_eq!(fails(&mut i, "N2"), ErrorKind::Type);
    int(&mut i, "N3");
    assert_eq!(i.host().notified.last().map(|x| x.1), Some(0x83));
}

#[test]
fn the_initialization_walk_follows_table_6_67() {
    let sta = |s: u64| name("_STA", super::encode::int(s));
    let mut i = loaded(&[
        scope(
            "\\_SB",
            &[
                m("_INI", &[store(one(), nm("\\SBI"))]),
                device(
                    "ON",
                    &[
                        sta(0x0F),
                        m("_INI", &[store(one(), nm("\\ONI"))]),
                        device("KID", &[m("_INI", &[store(one(), nm("\\KDI"))])]),
                    ],
                ),
                device(
                    "OFF",
                    &[
                        sta(0),
                        m("_INI", &[store(one(), nm("\\OFI"))]),
                        device("KID", &[m("_INI", &[store(one(), nm("\\OKI"))])]),
                    ],
                ),
                device(
                    "BRG",
                    &[
                        sta(0x08),
                        m("_INI", &[store(one(), nm("\\BGI"))]),
                        device("KID", &[m("_INI", &[store(one(), nm("\\BKI"))])]),
                    ],
                ),
                device(
                    "BAD",
                    &[
                        m("_STA", &[ret(divide(one(), zero(), none(), none()))]),
                        device("KID", &[m("_INI", &[store(one(), nm("\\XKI"))])]),
                    ],
                ),
                device(
                    "FIN",
                    &[
                        m("_INI", &[fatal(1, 2, one())]),
                        device("KID", &[m("_INI", &[store(one(), nm("\\FKI"))])]),
                    ],
                ),
            ],
        ),
        name("SBI", zero()),
        name("ONI", zero()),
        name("KDI", zero()),
        name("OFI", zero()),
        name("OKI", zero()),
        name("BGI", zero()),
        name("BKI", zero()),
        name("XKI", zero()),
        name("FKI", zero()),
    ]);
    let mut seen = Vec::new();
    i.initialize(&mut |n, o| seen.push((n, o))).unwrap();
    let ran = |i: &mut Interp, n: &str| int(i, n) == 1;
    assert!(ran(&mut i, "SBI") && ran(&mut i, "ONI") && ran(&mut i, "KDI"));
    assert!(!ran(&mut i, "OFI") && !ran(&mut i, "OKI"));
    assert!(!ran(&mut i, "BGI") && ran(&mut i, "BKI"));
    assert!(!ran(&mut i, "XKI") && !ran(&mut i, "FKI"));
    let of = |path: &str| {
        let n = i.lookup(path).unwrap().unwrap();
        seen.iter().find(|(m, _)| *m == n).map(|(_, o)| *o)
    };
    assert_eq!(of("\\_SB.OFF"), Some(InitOutcome::Absent { status: 0 }));
    assert_eq!(
        of("\\_SB.BRG"),
        Some(InitOutcome::ChildrenOnly { status: 8 })
    );
    assert!(
        matches!(of("\\_SB.BAD"), Some(InitOutcome::StaFailed(e)) if e.kind == ErrorKind::DivideByZero)
    );
    assert!(matches!(
        of("\\_SB.FIN"),
        Some(InitOutcome::IniFailed { .. })
    ));
    assert_eq!(
        of("\\_SB.ON.KID"),
        Some(InitOutcome::Initialized {
            status: 0x0F,
            ini: true
        })
    );
    assert_eq!(of("\\_SB.OFF.KID"), None);
}

#[test]
fn reg_runs_where_regions_of_the_space_are_and_not_for_memory_or_io() {
    let mut i = loaded(&[
        name("CNT", zero()),
        name("ARG", zero()),
        scope(
            "\\_SB",
            &[
                device(
                    "EC0",
                    &[
                        op_region("ECR", 3, zero(), byte(0x10)),
                        op_region("ECS", 3, byte(0x10), byte(0x10)),
                        method(
                            "_REG",
                            2,
                            &[
                                increment(nm("\\CNT")),
                                store(
                                    add(bin(0x79, arg(0), byte(8), none()), arg(1), none()),
                                    nm("\\ARG"),
                                ),
                            ],
                        ),
                    ],
                ),
                device(
                    "IO0",
                    &[
                        op_region("IOR", 1, zero(), one()),
                        method("_REG", 2, &[increment(nm("\\CNT"))]),
                    ],
                ),
            ],
        ),
    ]);
    let mut reports = 0;
    let ran = i
        .connect_space(Space::EMBEDDED_CONTROL, true, &mut |_, r| {
            assert!(r.is_ok());
            reports += 1;
        })
        .unwrap();
    assert_eq!((ran, reports), (1, 1));
    assert_eq!(int(&mut i, "CNT"), 1);
    assert_eq!(int(&mut i, "ARG"), (3 << 8) | 1);
    assert_eq!(
        i.connect_space(Space::SYSTEM_IO, true, &mut |_, _| {})
            .unwrap(),
        0
    );
    assert_eq!(int(&mut i, "CNT"), 1);
}

// --- The time left, and tracing (the review of B1, F9) -------------------------------------------

#[test]
fn waiting_calls_are_given_the_time_left_and_never_for_ever() {
    let mut i = loaded(&[
        op_region("MEM", 0, dword(0x2000), byte(4)),
        field("MEM", DWORD | 0x10, &[Fe::Named("LK", 32)]),
        m("R", &[ret(nm("LK"))]),
        m(
            "EVER",
            &[
                store(acquire(nm("\\_GL"), 0xFFFF), local(0)),
                release(nm("\\_GL")),
                ret(local(0)),
            ],
        ),
        m(
            "SOON",
            &[
                store(acquire(nm("\\_GL"), 5), local(0)),
                release(nm("\\_GL")),
                ret(local(0)),
            ],
        ),
        m("TRY", &[ret(acquire(nm("\\_GL"), 5))]),
        m(
            "LATE",
            &[
                sleep(word(29_990)),
                store(acquire(nm("\\_GL"), 100), local(0)),
                release(nm("\\_GL")),
                ret(local(0)),
            ],
        ),
    ]);
    // The service limits give 30 s, and the clock stands still but for `Sleep`.
    assert_eq!(int(&mut i, "R"), 0);
    assert_eq!(i.host().time_left, [300_000_000]);
    assert_eq!(int(&mut i, "EVER"), 0);
    assert_eq!(int(&mut i, "SOON"), 0);
    assert_eq!(int(&mut i, "LATE"), 0);
    // The Lock field's wait and `Acquire`'s "for ever" are cut to the 30 s left; 5 ms is asked
    // as it is; 100 ms is cut to the 10 ms left after the sleep.
    assert_eq!(i.host().lock_waits, [30_000, 30_000, 5, 10]);
    // A wait that was cut and times out ends the evaluation; one asked as it is reports `Ones`.
    i.host_mut().global_times_out = true;
    assert_eq!(fails(&mut i, "EVER"), ErrorKind::Limit(Limit::Time));
    assert_eq!(fails(&mut i, "LATE"), ErrorKind::Limit(Limit::Time));
    assert_eq!(fails(&mut i, "R"), ErrorKind::Limit(Limit::Time));
    assert_eq!(int(&mut i, "TRY"), u64::MAX);
    assert_eq!(i.host().global_held, 0);
}

#[test]
fn no_access_is_made_with_no_time_left() {
    let mut i = interp_with(
        IntegerWidth::Bits64,
        Limits {
            time_100ns: 10_000_000,
            ..Limits::SERVICE
        },
    );
    load(
        &mut i,
        &[
            op_region("IO", 1, word(0x80), byte(1)),
            field("IO", BYTE, &[Fe::Named("P80", 8)]),
            // The whole second slept, then a read: refused before the host is asked.
            m("ALL", &[sleep(word(1_000)), ret(nm("P80"))]),
            m("LESS", &[sleep(word(999)), ret(nm("P80"))]),
        ],
    )
    .expect("loads");
    assert_eq!(fails(&mut i, "ALL"), ErrorKind::Limit(Limit::Time));
    assert!(i.host().seen.is_empty());
    assert_eq!(int(&mut i, "LESS"), 0);
    // 1 ms left, in 100 ns units.
    assert_eq!(i.host().time_left, [10_000]);
}

#[test]
fn a_tracing_host_hears_loads_calls_accesses_and_limits() {
    use super::Event;
    let mut i = interp(IntegerWidth::Bits64);
    i.host_mut().events = Some(Vec::new());
    let mut nest = m("DEEP", &[ret(one())]);
    for _ in 0..40 {
        nest = device("A", &[nest]);
    }
    load(
        &mut i,
        &[
            op_region("IO", 1, word(0x80), byte(1)),
            field("IO", BYTE, &[Fe::Named("P80", 8)]),
            m("OUT", &[store(byte(0x42), nm("P80")), ret(call("IN", &[]))]),
            m("IN", &[ret(nm("P80"))]),
            m("LOOP", &[ret(call("LOOP", &[]))]),
            device("B", &[device("C", &[m("D", &[ret(one())])])]),
            nest,
        ],
    )
    .expect("loads");
    let take = |i: &mut Interp| i.host_mut().events.replace(Vec::new()).unwrap();
    assert_eq!(take(&mut i), [Event::Load(*b"SSDT"), Event::Loaded(Ok(0))]);
    assert_eq!(int(&mut i, "OUT"), 0x42);
    assert_eq!(
        take(&mut i),
        [
            Event::Enter(vec![*b"OUT_"]),
            Event::Write(0x80, 0x42, Ok(())),
            Event::Enter(vec![*b"IN__"]),
            Event::Read(0x80, Ok(0x42)),
            Event::Leave,
            Event::Leave,
        ]
    );
    assert_eq!(int(&mut i, "\\B.C.D"), 1);
    assert_eq!(
        take(&mut i),
        [
            Event::Enter(vec![*b"B___", *b"C___", *b"D___"]),
            Event::Leave
        ]
    );
    // A path deeper than 32 segments is given as none.
    let deep = (0..40).map(|_| "A").collect::<Vec<_>>().join(".");
    assert_eq!(int(&mut i, &format!("\\{deep}.DEEP")), 1);
    assert_eq!(take(&mut i), [Event::Enter(vec![]), Event::Leave]);
    // Each activation an error unwinds is left, then the limit is told.
    assert_eq!(fails(&mut i, "LOOP"), ErrorKind::Limit(Limit::Depth));
    let ev = take(&mut i);
    let enters = ev.iter().filter(|e| matches!(e, Event::Enter(_))).count();
    let leaves = ev.iter().filter(|e| **e == Event::Leave).count();
    assert_eq!((enters, leaves), (64, 64));
    assert_eq!(ev.last(), Some(&Event::Limit(Limit::Depth)));
    // A failed load is told.
    assert_eq!(
        load(&mut i, &[m("X", &[]), m("X", &[])]).unwrap_err().kind,
        ErrorKind::Duplicate
    );
    assert_eq!(
        take(&mut i),
        [
            Event::Load(*b"SSDT"),
            Event::Loaded(Err(ErrorKind::Duplicate))
        ]
    );
}

// --- A PCI_Config region's function, found by frames (the review of B2, B2-1) ------------------

/// The stack the tests below run on: far smaller than a test thread's 2 MiB. What they run must
/// not need more however deep the firmware's calls go.
const SMALL_STACK: usize = 192 << 10;

fn on_small_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(f)
        .expect("a thread")
        .join()
        .expect("the test ran to its end on the small stack");
}

/// `\_SB.PCI0` (a root bridge, bus 0) and below it `D1`, with a PCI_Config region and the field
/// `F1` in it; `adr` is `D1`'s `_ADR` method's body, `more` anything else at the root.
fn pci_device(adr: &[B], more: &[B]) -> Interp {
    let mut body = vec![scope(
        "\\_SB",
        &[device(
            "PCI0",
            &[
                name("_HID", dword(0x080A_D041)),
                name("_BBN", zero()),
                device(
                    "D1",
                    &[
                        method("_ADR", 0, adr),
                        op_region("R1", 2, zero(), byte(4)),
                        field("R1", BYTE, &[Fe::Named("F1", 8)]),
                    ],
                ),
            ],
        )],
    )];
    body.extend_from_slice(more);
    let mut i = super::interp_with(IntegerWidth::Bits64, Limits::BOOT_PROOF);
    load(&mut i, &body).expect("the block loads");
    i
}

#[test]
fn an_adr_that_reads_its_own_region_is_refused_not_recursed() {
    on_small_stack(|| {
        let mut i = pci_device(
            &[ret(nm("\\_SB.PCI0.D1.F1"))],
            &[method("RD", 0, &[ret(nm("\\_SB.PCI0.D1.F1"))])],
        );
        // From a method (the frames' path) and from the field's own name (the API's).
        assert_eq!(fails(&mut i, "RD"), ErrorKind::AddressCycle);
        assert_eq!(fails(&mut i, "\\_SB.PCI0.D1.F1"), ErrorKind::AddressCycle);
        assert!(i.host().seen.is_empty(), "nothing reached the host");
        assert_eq!(i.resolving, 0);
    });
    // The review of B2's Addendum 2, N2-1: an `_ADR` that is itself a field of the region it
    // names. The walk's own step is put back to find that region's function, and must meet the
    // walk already under way: `AddressCycle`, not a walk pushed again each round until a limit.
    on_small_stack(|| {
        let mut i = super::interp_with(IntegerWidth::Bits64, Limits::BOOT_PROOF);
        load(
            &mut i,
            &[scope(
                "\\_SB",
                &[device(
                    "PCI0",
                    &[
                        name("_HID", dword(0x080A_D041)),
                        name("_BBN", zero()),
                        device(
                            "D1",
                            &[
                                op_region("R1", 2, zero(), byte(8)),
                                field("R1", DWORD, &[Fe::Named("_ADR", 32), Fe::Named("F1", 8)]),
                            ],
                        ),
                    ],
                )],
            )],
        )
        .expect("the block loads");
        assert_eq!(fails(&mut i, "\\_SB.PCI0.D1.F1"), ErrorKind::AddressCycle);
        assert!(i.host().seen.is_empty(), "nothing reached the host");
        assert_eq!(i.resolving, 0);
    });
}

#[test]
fn an_adr_thirty_calls_deep_runs_on_the_frames() {
    on_small_stack(|| {
        // `_ADR` calls L00, which calls L01, … L29, which gives device 3, function 0.
        let mut more = vec![];
        for k in 0..30 {
            let body = if k == 29 {
                vec![ret(dword(0x0003_0000))]
            } else {
                vec![ret(call(&format!("\\L{:02}", k + 1), &[]))]
            };
            more.push(method(&format!("\\L{k:02}"), 0, &body));
        }
        more.push(method("RD", 0, &[ret(nm("\\_SB.PCI0.D1.F1"))]));
        let mut i = pci_device(&[ret(call("\\L00", &[]))], &more);
        i.host_mut().memory.insert((2, Some((0, 0, 3, 0)), 0), 0x86);
        assert_eq!(int(&mut i, "RD"), 0x86);
        let last = i.host().seen.last().cloned().unwrap();
        assert_eq!(
            last.pci,
            Some(PciAddress {
                segment: 0,
                bus: 0,
                device: 3,
                function: 0
            })
        );
    });
}

#[test]
fn the_pci_walk_follows_devices_not_the_scopes_around_a_region() {
    // The review of B2, B2-2. `\_SB.PCI0` (a root bridge, bus 0) › `RP01` (a bridge at device
    // 0x1C, its secondary bus 5) › `PXSX` (device 0), with PCI_Config regions declared in a method
    // of `PXSX`, in a power resource of `RP01` and in a thermal zone inside `PXSX`; and `NADR`, a
    // device with no `_ADR`.
    let pxsx = device(
        "PXSX",
        &[
            name("_ADR", zero()),
            method(
                "WAKE",
                0,
                &[
                    op_region("PCFG", 2, byte(0x44), byte(4)),
                    field("PCFG", DWORD, &[Fe::Named("PMEC", 32)]),
                    store(word(0x8000), nm("PMEC")),
                ],
            ),
            thermal_zone(
                "TZ1",
                &[
                    op_region("TCFG", 2, byte(0x60), byte(4)),
                    field("TCFG", DWORD, &[Fe::Named("TMP", 32)]),
                    method("RD", 0, &[ret(nm("TMP"))]),
                ],
            ),
        ],
    );
    let rp01 = device(
        "RP01",
        &[
            name("_ADR", dword(0x001C_0000)),
            power_resource(
                "PXP",
                0,
                0,
                &[
                    op_region("PCFG", 2, byte(0x50), byte(4)),
                    field("PCFG", DWORD, &[Fe::Named("PWR", 32)]),
                    method("_ON", 0, &[store(one(), nm("PWR"))]),
                ],
            ),
            pxsx,
        ],
    );
    let nadr = device(
        "NADR",
        &[
            op_region("NCFG", 2, zero(), byte(4)),
            field("NCFG", BYTE, &[Fe::Named("NV", 8)]),
            method("RD", 0, &[ret(nm("NV"))]),
        ],
    );
    let mut i = loaded(&[scope(
        "\\_SB",
        &[device(
            "PCI0",
            &[
                name("_HID", dword(0x080A_D041)),
                name("_BBN", zero()),
                rp01,
                nadr,
            ],
        )],
    )]);
    i.host_mut()
        .memory
        .insert((2, Some((0, 0, 0x1C, 0)), 0x19), 5);
    let at = |segment, bus, device, function| {
        Some(PciAddress {
            segment,
            bus,
            device,
            function,
        })
    };
    // A method of PXSX: PXSX's function, below RP01. The bridge read is RP01's alone.
    i.evaluate("\\_SB.PCI0.RP01.PXSX.WAKE", &[]).unwrap();
    // The bridge's bus asked for by its own request (B2-3), never as a region access.
    assert_eq!(i.host().bridges, [at(0, 0, 0x1C, 0).unwrap()]);
    assert!(i.host().seen.iter().all(|s| s.address != 0x19));
    let seen = std::mem::take(&mut i.host_mut().seen);
    let last = seen.last().unwrap();
    assert_eq!(
        (last.write, last.address, last.pci),
        (true, 0x44, at(0, 5, 0, 0))
    );
    // A power resource of RP01: RP01's function, on the root bridge's bus.
    i.evaluate("\\_SB.PCI0.RP01.PXP._ON", &[]).unwrap();
    let last = i.host().seen.last().cloned().unwrap();
    assert_eq!(
        (last.write, last.address, last.pci),
        (true, 0x50, at(0, 0, 0x1C, 0))
    );
    // A thermal zone inside PXSX: PXSX's function.
    int(&mut i, "\\_SB.PCI0.RP01.PXSX.TZ1.RD");
    let last = i.host().seen.last().cloned().unwrap();
    assert_eq!(
        (last.write, last.address, last.pci),
        (false, 0x60, at(0, 5, 0, 0))
    );
    // A device with no `_ADR`: refused, never function 0, and nothing sent.
    let before = i.host().seen.len();
    assert_eq!(fails(&mut i, "\\_SB.PCI0.NADR.RD"), ErrorKind::NotFound);
    assert_eq!(i.host().seen.len(), before);
}

#[test]
fn protocol_and_pin_fields_past_their_region_are_refused() {
    // The review of B2, B2-3: an SMBus region's length is its command space (§5.5.2.4.7); a field
    // whose first byte is past it is refused before anything reaches the host.
    let mut i = loaded(&[
        name("CON", buffer(byte(3), &[0x8C, 1, 2])),
        op_region("SMB", 4, zero(), byte(0x10)),
        field(
            "SMB",
            0x05,
            &[
                Fe::Reserved(0x30 * 8),
                Fe::Access(0x05, 0x08),
                Fe::Named("CMD", 8),
            ],
        ),
        op_region("GIO", 8, zero(), byte(1)),
        field(
            "GIO",
            BYTE,
            &[
                Fe::ConnectName("CON"),
                Fe::Reserved(16),
                Fe::Named("PIN", 1),
            ],
        ),
        m("RS", &[ret(nm("CMD"))]),
        m("RG", &[ret(nm("PIN"))]),
    ]);
    assert_eq!(fails(&mut i, "RS"), ErrorKind::Range);
    assert_eq!(fails(&mut i, "RG"), ErrorKind::Range);
    assert!(i.host().transfers.is_empty());
}

#[test]
fn gpio_fields_give_the_host_their_pins() {
    // The review of B2, B2-4: two fields after one `Connection` (§5.5.2.4.6.1) are the
    // connection's first pin and its next two; both start in byte 0 of the region, so only the
    // bit offset and length tell them apart. A second `Connection` starts the count again.
    let mut i = loaded(&[
        name("CON", buffer(byte(3), &[0x8C, 1, 2])),
        name("CO2", buffer(byte(3), &[0x8C, 3, 4])),
        op_region("GIO", 8, zero(), byte(4)),
        field(
            "GIO",
            BYTE,
            &[
                Fe::ConnectName("CON"),
                Fe::Named("A", 1),
                Fe::Named("B", 2),
                Fe::ConnectName("CO2"),
                Fe::Named("C", 4),
            ],
        ),
        m("WA", &[store(one(), nm("A"))]),
        m("WB", &[store(byte(3), nm("B"))]),
        m("RC", &[ret(nm("C"))]),
    ]);
    i.evaluate("WA", &[]).unwrap();
    i.evaluate("WB", &[]).unwrap();
    i.evaluate("RC", &[]).unwrap();
    let got: Vec<_> = i
        .host()
        .transfers
        .iter()
        .map(|(s, _)| {
            (
                s.address,
                s.bit_offset,
                s.bit_length,
                s.write,
                s.connection.clone(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (0, 0, 1, true, Some(vec![0x8C, 1, 2])),
            (0, 1, 2, true, Some(vec![0x8C, 1, 2])),
            (0, 0, 4, false, Some(vec![0x8C, 3, 4])),
        ]
    );
}

#[test]
fn the_global_lock_is_asked_for_once_however_often_the_evaluation_takes_it() {
    // The review of B2, B2-5. The model host's lock is not recursive: a second acquisition while
    // it is held times out, as a literal FACS lock would.
    let mut i = loaded(&[
        op_region("MEM", 0, dword(0x2000), byte(8)),
        field(
            "MEM",
            BYTE | 0x10,
            &[Fe::Named("LK", 8), Fe::Named("IDX", 8), Fe::Named("DAT", 8)],
        ),
        // An index field with the Lock rule over index and data registers that have it too.
        index_field("IDX", "DAT", BYTE | 0x10, &[Fe::Named("REG", 8)]),
        m(
            "BOTH",
            &[
                store(acquire(nm("\\_GL"), 0xFFFF), local(0)),
                store(nm("LK"), local(1)),
                release(nm("\\_GL")),
                ret(local(0)),
            ],
        ),
        m("NEST", &[ret(nm("REG"))]),
    ]);
    assert_eq!(int(&mut i, "BOTH"), 0);
    assert_eq!(int(&mut i, "NEST"), 0);
    // Once for each evaluation, and given back each time.
    assert_eq!((i.host().global_taken, i.host().global_held), (2, 0));
    assert_eq!(i.global_depth, 0);
}

#[test]
fn a_walk_cut_off_by_an_error_is_counted_out() {
    // The count of `Resolve` frames follows their removal by an error, so that a later walk for
    // a region still meets the check for a walk already under way.
    on_small_stack(|| {
        let mut i = pci_device(
            &[ret(nm("\\_SB.PCI0.D1.F1"))],
            &[method("RD", 0, &[ret(nm("\\_SB.PCI0.D1.F1"))])],
        );
        for _ in 0..3 {
            assert_eq!(fails(&mut i, "RD"), ErrorKind::AddressCycle);
            assert_eq!(i.resolving, 0);
        }
    });
}

#[test]
fn a_mutex_a_method_run_by_export_keeps_is_released_at_the_end() {
    // The review of B2, B2-6. `PK` returns a package naming a field of `D1`, whose region's
    // function is found only when the package is exported, by running `_ADR`, which takes `MX`
    // (sync level 5) and keeps it.
    let mut i = pci_device(
        &[acquire(nm("\\MX"), 0xFFFF), ret(dword(0x0001_0000))],
        &[
            mutex("MX", 5),
            mutex("LO", 1),
            method("PK", 0, &[ret(package(1, &[nm("\\_SB.PCI0.D1.F1")]))]),
            method(
                "LOW",
                0,
                &[acquire(nm("\\LO"), 0xFFFF), release(nm("\\LO")), ret(one())],
            ),
        ],
    );
    // Reported, and released: a mutex of a lower level can be taken afterwards. Since B2-10 the
    // `_ADR` is stopped at its own return, so the walk fails and the function stays unknown: the
    // next export fails the same way, and again leaves nothing held.
    for _ in 0..2 {
        assert_eq!(fails(&mut i, "PK"), ErrorKind::MutexHeld);
        assert!(i.held.is_empty());
        assert_eq!(int(&mut i, "LOW"), 1);
    }
}

// --- The initialization walk as a whole (the review of B2, B2-7) --------------------------------

#[test]
fn the_initialization_walk_is_bounded_as_a_whole() {
    // 200 devices whose `_STA` each sleeps 10 ms: each alone is well within the boot proof's one
    // second, the walk is not. It stops near one second and says so.
    let devices: Vec<B> = (0..200)
        .map(|k| {
            device(
                &format!("D{k:03}"),
                &[method("_STA", 0, &[sleep(byte(10)), ret(byte(0x0F))])],
            )
        })
        .collect();
    let mut i = super::interp_with(IntegerWidth::Bits64, Limits::BOOT_PROOF);
    load(&mut i, &[scope("\\_SB", &devices)]).unwrap();
    let mut reported = 0usize;
    let e = i
        .initialize(&mut |_, _| reported += 1)
        .expect_err("the walk is cut off");
    assert_eq!(e.kind, ErrorKind::Limit(Limit::Time));
    assert!((95..=101).contains(&reported), "{reported} devices reached");
}

#[test]
fn a_wide_namespace_is_walked_without_a_collection_per_device() {
    // 20,000 devices in 100 scopes, each `_STA` a plain name: each is an evaluation that
    // allocates nothing, and must not cost a pass over the whole heap.
    let scopes: Vec<B> = (0..100)
        .map(|s| {
            let devices: Vec<B> = (0..200)
                .map(|k| device(&format!("D{k:03}"), &[name("_STA", byte(0x0F))]))
                .collect();
            device(&format!("\\S{s:03}"), &devices)
        })
        .collect();
    let mut i = super::interp(IntegerWidth::Bits64);
    load(&mut i, &scopes).unwrap();
    let start = std::time::Instant::now();
    let mut reported = 0usize;
    i.initialize(&mut |_, _| reported += 1).unwrap();
    let took = start.elapsed();
    assert_eq!(reported, 20_100);
    // About a millisecond (release build); with a collection and a recount after each
    // evaluation, as before, 5.4 s. The bound is far from both.
    assert!(took.as_secs() < 2, "{took:?}");
    eprintln!("20,100 devices walked in {took:?}");
}

#[test]
fn a_field_straddling_its_region_is_refused_before_any_access() {
    // The review of B2, B2-8: a 16-bit field from byte 3 of a 4-byte region, in byte units: the
    // second unit is outside, so neither is written.
    let mut i = loaded(&[
        op_region("IO", 1, word(0x80), byte(4)),
        field("IO", BYTE, &[Fe::Reserved(24), Fe::Named("ST", 16)]),
        m("W", &[store(word(0xBEEF), nm("ST"))]),
    ]);
    assert_eq!(fails(&mut i, "W"), ErrorKind::Range);
    assert!(i.host().seen.is_empty(), "{:?}", i.host().seen);
}

#[test]
fn an_adr_outside_a_device_and_function_is_refused() {
    // The review of B2, B2-9: device 0 to 31, function 0 to 7; `0xFFFF` ("all functions") and a
    // device of 32 name no one function.
    for adr in [0x0001_FFFF_u32, 0x0020_0000, 0x0000_0008] {
        let mut i = pci_device(
            &[ret(dword(adr))],
            &[method("RD", 0, &[ret(nm("\\_SB.PCI0.D1.F1"))])],
        );
        assert_eq!(fails(&mut i, "RD"), ErrorKind::Range, "{adr:#x}");
        assert!(i.host().seen.is_empty());
    }
    let mut i = pci_device(
        &[ret(dword(0x001F_0007))],
        &[method("RD", 0, &[ret(nm("\\_SB.PCI0.D1.F1"))])],
    );
    assert_eq!(int(&mut i, "RD"), 0);
    let last = i.host().seen.last().cloned().unwrap();
    assert_eq!(
        last.pci,
        Some(PciAddress {
            segment: 0,
            bus: 0,
            device: 31,
            function: 7
        })
    );
}

#[test]
fn a_method_that_returns_holding_a_mutex_is_stopped_at_its_return() {
    // The review of B2, B2-10: G2's strict choice applies at the method's return, so its caller
    // does not go on using what the mutex guards.
    let mut i = loaded(&[
        mutex("MX", 3),
        name("FLAG", zero()),
        m("INNR", &[acquire(nm("MX"), 0xFFFF), ret(one())]),
        m(
            "OUTR",
            &[store(call("INNR", &[]), local(0)), store(one(), nm("FLAG"))],
        ),
        m(
            "NEST",
            &[acquire(nm("MX"), 0xFFFF), release(nm("MX")), ret(one())],
        ),
    ]);
    assert_eq!(fails(&mut i, "OUTR"), ErrorKind::MutexHeld);
    assert_eq!(int(&mut i, "FLAG"), 0, "the caller went on");
    assert!(i.held.is_empty());
    assert_eq!(int(&mut i, "NEST"), 1);
}

#[test]
fn a_reserved_region_space_is_refused() {
    // The review of B2, B2-11: Table 5.221 reserves spaces 0x0C to 0x7E; an OEM space (0x80 on)
    // is the host's to judge.
    for (space, refused) in [(0x0Cu8, true), (0x40, true), (0x7E, true), (0x80, false)] {
        let mut i = loaded(&[
            op_region("RS", space, zero(), byte(4)),
            field("RS", BYTE, &[Fe::Named("V", 8)]),
            m("RD", &[ret(nm("V"))]),
        ]);
        if refused {
            assert_eq!(fails(&mut i, "RD"), ErrorKind::Range, "{space:#x}");
            assert!(i.host().seen.is_empty());
        } else {
            assert_eq!(int(&mut i, "RD"), 0);
            assert_eq!(i.host().seen.len(), 1);
        }
    }
}

#[test]
fn divide_finds_both_targets_functions_before_its_first_store() {
    // B2-1, follow-up: `Divide` stores twice. Its remainder goes to a port, its quotient to a
    // field whose PCI function is not yet known: the step is put back for the second before the
    // first is done, so the port is written once.
    let mut i = pci_device(
        &[ret(dword(0x0001_0000))],
        &[
            op_region("IO", 1, word(0x80), byte(1)),
            field("IO", BYTE, &[Fe::Named("P80", 8)]),
            method(
                "DV",
                0,
                &[divide(
                    byte(10),
                    byte(3),
                    nm("\\P80"),
                    nm("\\_SB.PCI0.D1.F1"),
                )],
            ),
        ],
    );
    i.evaluate("DV", &[]).unwrap();
    let writes: Vec<_> = i
        .host()
        .seen
        .iter()
        .filter(|s| s.write)
        .map(|s| (s.space, s.address, s.value))
        .collect();
    assert_eq!(writes, [(1, 0x80, 1), (2, 0, 3)]);
}

// --- Fields of no bits and buffers of no bytes, every way (N-soak-1, the audit of the loops) ----

#[test]
fn fields_of_no_bits_and_buffers_of_no_bytes_end_however_they_are_used() {
    // After N-soak-1: a field of no bits, in each kind of field and space, and a buffer of no
    // bytes, used in each way the grammar allows, must end within the limits: each method on a
    // thread of its own, failing if it has not ended in 10 s.
    let fields = vec![
        op_region("IO", 1, word(0x80), byte(8)),
        field(
            "IO",
            BYTE,
            &[
                Fe::Named("ZW", 0),
                Fe::Named("IDX", 8),
                Fe::Named("DAT", 8),
                Fe::Named("BNK", 8),
            ],
        ),
        field("IO", BYTE | 0x10, &[Fe::Named("ZL", 0)]),
        index_field(
            "IDX",
            "DAT",
            BYTE,
            &[Fe::Named("ZI", 0), Fe::Named("I1", 8)],
        ),
        bank_field("IO", "BNK", byte(1), BYTE, &[Fe::Named("ZK", 0)]),
        name("CON", buffer(byte(3), &[0x8C, 1, 2])),
        op_region("SMB", 4, zero(), byte(0x10)),
        field("SMB", 0x05, &[Fe::Access(0x05, 0x08), Fe::Named("ZS", 0)]),
        op_region("GIO", 8, zero(), byte(4)),
        field("GIO", BYTE, &[Fe::ConnectName("CON"), Fe::Named("ZG", 0)]),
        name("E", buffer(zero(), &[])),
    ];
    let big = || buffer(byte(64), &[1, 2, 3]);
    let mut methods: Vec<(&str, Vec<B>)> = vec![];
    for f in ["ZW", "ZL", "ZI", "ZK", "ZS", "ZG"] {
        methods.extend([
            ("RD", vec![ret(nm(f))]),
            ("SI", vec![store(word(0x1234), nm(f))]),
            ("SB", vec![store(big(), nm(f))]),
            ("SS", vec![store(string("abcdefgh"), nm(f))]),
            ("SE", vec![store(buffer(zero(), &[]), nm(f))]),
            ("SP", vec![store(package(1, &[one()]), nm(f))]),
            ("CI", vec![copy_object(one(), nm(f))]),
            ("CB", vec![copy_object(big(), nm(f))]),
            ("IN", vec![increment(nm(f)), decrement(nm(f))]),
            ("AD", vec![add(one(), byte(2), nm(f))]),
            ("DV", vec![divide(byte(7), byte(2), nm(f), nm(f))]),
            ("CT", vec![concat(string("ab"), string("cd"), nm(f))]),
            ("TH", vec![un(0x98, nm(f), nm(f))]),
            ("MD", vec![ret(mid(nm(f), zero(), byte(4), none()))]),
            (
                "RF",
                vec![store(ref_of(nm(f)), local(0)), ret(deref_of(local(0)))],
            ),
            ("CR", vec![ret(cond_ref_of(nm(f), local(0)))]),
            ("SZ", vec![ret(size_of(nm(f)))]),
            ("OT", vec![ret(object_type(nm(f)))]),
            ("IX", vec![ret(index(nm(f), zero(), none()))]),
            ("CF", vec![create_byte_field(nm(f), zero(), "X")]),
            (
                "WL",
                vec![
                    store(zero(), local(0)),
                    while_(
                        logic(0x95, increment(local(0)), byte(200)),
                        &[store(big(), nm(f)), store(string("abc"), nm(f))],
                    ),
                ],
            ),
            ("PK", vec![ret(package(1, &[nm(f)]))]),
            ("NT", vec![notify(nm(f), zero())]),
        ]);
    }
    // A buffer of no bytes, every way a buffer field can be made over it, and as an operand.
    methods.extend([
        ("F0", vec![create_field(big(), zero(), zero(), "X")]),
        ("F1", vec![create_field(nm("E"), zero(), zero(), "X")]),
        ("F2", vec![create_bit_field(nm("E"), zero(), "X")]),
        ("F3", vec![create_byte_field(nm("E"), zero(), "X")]),
        ("F4", vec![create_word_field(nm("E"), zero(), "X")]),
        ("F5", vec![create_dword_field(nm("E"), zero(), "X")]),
        ("F6", vec![create_qword_field(nm("E"), zero(), "X")]),
        ("E1", vec![ret(concat(nm("E"), nm("E"), none()))]),
        ("E2", vec![ret(mid(nm("E"), zero(), byte(10), none()))]),
        ("E3", vec![ret(un(0x98, nm("E"), none()))]),
        ("E4", vec![ret(index(nm("E"), zero(), none()))]),
        ("E5", vec![ret(size_of(nm("E")))]),
        ("E6", vec![store(nm("E"), nm("E")), ret(nm("E"))]),
    ]);
    let limits = Limits {
        loop_iterations: 2_000,
        steps: 20_000,
        ..Limits::BOOT_PROOF
    };
    let mut outcomes = std::collections::BTreeMap::new();
    for (k, (what, body)) in methods.into_iter().enumerate() {
        let mut block = fields.clone();
        block.push(method("M0", 0, &body));
        let (tx, rx) = std::sync::mpsc::channel();
        super::ends_within(10, &format!("case {k} ({what})"), move || {
            let mut i = super::interp_with(IntegerWidth::Bits64, limits);
            load(&mut i, &block).expect("the block loads");
            let r = i.evaluate("M0", &[]);
            // The charge that passes the limit is counted whole before it is refused: a count
            // past the limit is always the step limit's end.
            if i.run.steps > limits.steps {
                assert!(
                    matches!(&r, Err(e) if e.kind == ErrorKind::Limit(Limit::Steps)),
                    "case {k}: {} steps and {r:?}",
                    i.run.steps
                );
            }
            let _ = tx.send(match r {
                Ok(_) => "Ok".to_string(),
                Err(e) => format!("{:?}", e.kind),
            });
        });
        *outcomes.entry(rx.recv().unwrap()).or_insert(0) += 1;
    }
    eprintln!("fields of no bits, buffers of no bytes: {outcomes:?}");
}

#[test]
fn match_lets_through_what_is_not_a_conversion() {
    // The review of B2's Addendum 2, N2-2: an element's read that needs a PCI function, meets a
    // limit or is refused is not "does not match".
    let mt = |name: &str| {
        method(
            "MT",
            0,
            &[ret(match_(
                package(1, &[nm(name)]),
                1,
                byte(0x86),
                0,
                zero(),
                zero(),
            ))],
        )
    };
    // A field whose PCI function is not yet known: found first, then read once, and matched.
    let mut i = pci_device(&[ret(dword(0x0001_0000))], &[mt("\\_SB.PCI0.D1.F1")]);
    i.host_mut().memory.insert((2, Some((0, 0, 1, 0)), 0), 0x86);
    assert_eq!(int(&mut i, "MT"), 0);
    assert_eq!(i.host().seen.iter().filter(|s| !s.write).count(), 1);
    let io = |len: u32, bits: u32| {
        vec![
            op_region("IO", 1, word(0x80), dword(len)),
            field("IO", BYTE, &[Fe::Named("P80", bits)]),
        ]
    };
    let with = |limits: Limits, body: Vec<B>| {
        let mut i = super::interp_with(IntegerWidth::Bits64, limits);
        load(&mut i, &body).expect("the block loads");
        i
    };
    // No time left when the element is read.
    let mut body = io(1, 8);
    body.push(method(
        "MT",
        0,
        &[
            sleep(word(1_000)),
            ret(match_(
                package(1, &[nm("P80")]),
                1,
                zero(),
                0,
                zero(),
                zero(),
            )),
        ],
    ));
    let mut i = with(
        Limits {
            time_100ns: 10_000_000,
            ..Limits::SERVICE
        },
        body,
    );
    assert_eq!(fails(&mut i, "MT"), ErrorKind::Limit(Limit::Time));
    // The element's bytes do not fit the memory limit.
    let mut body = io(0x2_0000, 0x10_0000);
    body.push(mt("P80"));
    let mut i = with(
        Limits {
            memory_bytes: 64 << 10,
            ..Limits::SERVICE
        },
        body,
    );
    assert_eq!(fails(&mut i, "MT"), ErrorKind::Limit(Limit::Memory));
    // The element's units pass the step limit.
    let mut body = io(1024, 8 * 1024);
    body.push(mt("P80"));
    let mut i = with(
        Limits {
            steps: 500,
            ..Limits::SERVICE
        },
        body,
    );
    assert_eq!(fails(&mut i, "MT"), ErrorKind::Limit(Limit::Steps));
    // The host refuses the element's read.
    let mut body = io(1, 8);
    body.push(mt("P80"));
    let mut i = with(Limits::SERVICE, body);
    i.host_mut().refuse = true;
    assert!(matches!(fails(&mut i, "MT"), ErrorKind::Region { .. }));
}

#[test]
fn a_pci_region_with_no_root_bridge_above_is_refused() {
    // The review of B2's Addendum 2, N2-3: `\_SB.PCI0` names itself no root bridge (neither
    // PNP0A03 nor PNP0A08), so the function of a region below it cannot be named: refused, and
    // nothing sent, never taken to be on segment 0, bus 0.
    let mut i = loaded(&[scope(
        "\\_SB",
        &[device(
            "PCI0",
            &[
                name("_HID", string("ACPI0001")),
                name("_BBN", zero()),
                device(
                    "D1",
                    &[
                        name("_ADR", dword(0x0003_0000)),
                        op_region("R1", 2, zero(), byte(4)),
                        field("R1", BYTE, &[Fe::Named("F1", 8)]),
                        method("RD", 0, &[ret(nm("F1"))]),
                    ],
                ),
            ],
        )],
    )]);
    assert_eq!(fails(&mut i, "\\_SB.PCI0.D1.RD"), ErrorKind::NotFound);
    assert!(i.host().seen.is_empty());
    assert!(i.host().bridges.is_empty());
}
