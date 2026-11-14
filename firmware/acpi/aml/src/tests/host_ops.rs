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
        vec![0; 64],
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
    i.initialize(&mut |n, o| seen.push((n, o)));
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
