//! The thermal LVT's sources on the host (TASKS item 106): CPUID's words, the register values the
//! kernel writes, and the notification's protocol against a model of the hardware's rule (SDM Vol.
//! 3, §17.6.6): the processor writes the table only while bit 26 is clear, sets it when it has,
//! and an acknowledgement clears only what it writes as 0 of the R/WC0 bits.
#![forbid(unsafe_code)]

use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::capacity::hfi_row;
use crate::testing::{Rng, cases};

/// CPUID.06H:EAX with the HFI, the package thermal registers and, if `td`, Thread Director.
const fn eax(td: bool) -> u32 {
    (1 << 19) | (1 << 6) | if td { 1 << 23 } else { 0 }
}
/// CPUID.01H:EDX with the thermal registers (bit 22).
const ACPI: u32 = 1 << 22;

#[test]
fn cpuid_words_decode() {
    let w = Words {
        eax6: eax(false) | (1 << 7) | (1 << 8) | (1 << 15),
        ecx6: 0,
        edx6: (7 << 16) | (1 << 8) | 0b11,
        edx1: ACPI,
        ebx_ext8: 0,
    };
    assert!(w.hfi() && w.package_thermal() && w.thermal_registers() && w.hwp());
    assert!(w.hwp_highest_interrupt() && !w.cppc());
    assert_eq!((w.pages(), w.row()), (2, 7));
    assert!(w.hfi_usable());
    let shape = w.shape().unwrap();
    assert!(w.row_fits(shape));
    // Without the thermal registers, the HFI cannot be used: its interrupt has no LVT.
    let no_lvt = Words { edx1: 0, ..w };
    assert!(no_lvt.hfi() && !no_lvt.hfi_usable());
    // Without the package registers either.
    let no_package = Words { eax6: 1 << 19, ..w };
    assert!(!no_package.hfi_usable());
    // A table that does not report performance is no table.
    let no_performance = Words { edx6: 0b10, ..w };
    assert!(!no_performance.hfi_usable());
    // HWP's interrupt needs both bit 8 and bit 15.
    assert!(
        !Words {
            eax6: eax(false) | (1 << 8),
            ..w
        }
        .hwp_highest_interrupt()
    );
    // AMD's CPPC: Fn8000_0008_EBX[27].
    assert!(
        Words {
            ebx_ext8: 1 << 27,
            ..Words::default()
        }
        .cppc()
    );
    assert_eq!(Words::default().pages(), 1);
}

/// A row past the table gives no value, and is never read as "avoid" (the contract's invariant
/// 2 and 4): one page holds rows 0 to 509 of the HFI's 8-byte rows, and row 510 starts past it.
#[test]
fn a_row_past_the_table_does_not_fit() {
    let row = |r: u32, pages: u32| Words {
        eax6: eax(false),
        ecx6: 0,
        edx6: (r << 16) | ((pages - 1) << 8) | 0b11,
        edx1: ACPI,
        ebx_ext8: 0,
    };
    let shape = row(0, 1).shape().unwrap();
    assert!(row(0, 1).row_fits(shape));
    // 8 + (509 + 1) * 8 + 2 = 4090 bytes: inside one page.
    assert!(row(509, 1).row_fits(shape));
    // 8 + (510 + 1) * 8 = 4096: past it.
    assert!(!row(510, 1).row_fits(shape));
    assert!(row(510, 2).row_fits(shape));
    assert!(!row(0xFFFF, 16).row_fits(shape));
}

/// Every processor of a package must report the same table: the signature differs where the HFI,
/// its size, its capabilities, its classes or the thermal LVT differ, and not where only the row
/// does.
#[test]
fn the_package_signature_holds_what_must_agree() {
    let base = Words {
        eax6: eax(true),
        ecx6: 4 << 8,
        edx6: (3 << 16) | (1 << 8) | 0b11,
        edx1: ACPI,
        ebx_ext8: 0,
    };
    let other_row = Words {
        edx6: (9 << 16) | (1 << 8) | 0b11,
        ..base
    };
    assert_eq!(base.package_signature(), other_row.package_signature());
    for differ in [
        Words { eax6: 0, ..base },
        Words {
            eax6: eax(false),
            ..base
        },
        Words {
            ecx6: 5 << 8,
            ..base
        },
        Words {
            edx6: (3 << 16) | 0b11,
            ..base
        },
        Words {
            edx6: (3 << 16) | (1 << 8) | 0b01,
            ..base
        },
        Words { edx1: 0, ..base },
    ] {
        assert_ne!(base.package_signature(), differ.package_signature());
    }
}

/// The acknowledgement clears bit 26 alone: every log bit written as 1, every read-only bit as
/// read, every reserved bit 0 (control C6 breaks this).
#[test]
fn the_acknowledgement_clears_bit_26_alone() {
    let mut rng = Rng::new(0x0106_ACC0);
    for case in 0..cases(10_000) {
        let status = rng.edge_u64();
        let ack = acknowledgement(status);
        assert_eq!(ack & STATUS_CHANGED, 0, "case {case}: bit 26 written as 0");
        assert_eq!(
            ack & STATUS_LOGS,
            STATUS_LOGS,
            "case {case}: logs written as 1"
        );
        assert_eq!(
            ack & STATUS_READ_ONLY,
            status & STATUS_READ_ONLY,
            "case {case}: read-only bits as read"
        );
        assert_eq!(
            ack & !(STATUS_LOGS | STATUS_READ_ONLY),
            0,
            "case {case}: reserved bits 0"
        );
    }
    assert_eq!(
        STATUS_LOGS,
        (1 << 1) | (1 << 3) | (1 << 5) | (1 << 7) | (1 << 9) | (1 << 11)
    );
    // HWP's: bit 3 cleared, bits 0, 2, 4 and 5 written as 1, bit 1 and the rest 0.
    assert_eq!(hwp_acknowledgement() & HWP_HIGHEST_CHANGED, 0);
    assert_eq!(hwp_acknowledgement(), 0b11_0101);
    // The pointer carries the valid bit; the heat enables are bits 0 and 1.
    assert_eq!(feedback_pointer(0x1234_5000), 0x1234_5001);
    assert_eq!(THERM_INTERRUPT_ENABLES, 0b11);
    assert!(hot(1) && !hot(2));
}

/// The hardware, as the SDM describes it, with what the protocol must never do counted.
struct Model {
    table: Vec<u8>,
    status: u64,
    /// The table the processor wants to write next.
    pending: Option<Vec<u8>>,
    /// Bytes copied while bit 26 was clear: a read the hardware could be writing under.
    reads_while_clear: u32,
    /// A write to a reserved bit, which the processor refuses.
    refused: bool,
    fault_status: bool,
    fault_ack: bool,
}

impl Model {
    fn new(table: Vec<u8>) -> Self {
        Self {
            table,
            status: 0,
            pending: None,
            reads_while_clear: 0,
            refused: false,
            fault_status: false,
            fault_ack: false,
        }
    }

    /// The processor writes its pending table, if bit 26 lets it.
    fn hardware(&mut self) {
        if self.status & STATUS_CHANGED == 0
            && let Some(t) = self.pending.take()
        {
            self.table = t;
            self.status |= STATUS_CHANGED;
        }
    }
}

impl PackageHardware for Model {
    fn status(&mut self) -> Option<u64> {
        (!self.fault_status).then_some(self.status)
    }

    fn acknowledge(&mut self, status: u64) -> bool {
        let value = acknowledgement(status);
        if self.fault_ack {
            return false;
        }
        if value & !(STATUS_READ_ONLY | STATUS_LOGS | STATUS_CHANGED) != 0 {
            self.refused = true;
            return false;
        }
        // R/WC0: a 0 clears, a 1 leaves.
        let rwc0 = STATUS_LOGS | STATUS_CHANGED;
        self.status &= !(rwc0 & !value);
        // The processor may write again at once.
        self.hardware();
        true
    }

    fn copy(&mut self, at: usize, out: &mut [u8]) -> bool {
        if self.status & STATUS_CHANGED == 0 {
            self.reads_while_clear += 1;
        }
        match self.table.get(at..at + out.len()) {
            Some(bytes) => {
                out.copy_from_slice(bytes);
                true
            }
            None => false,
        }
    }
}

/// What the operating system holds of each processor's row: never set, or the row (or `None`
/// where it lies past the table).
type View = Vec<Option<Option<HfiRow>>>;

/// Services `hw` for `members`, into `view`; returns what it found and whether `after` ran.
fn serve(
    hw: &mut Model,
    shape: HfiShape,
    members: &[(usize, u16)],
    view: &mut View,
) -> (Serviced, bool) {
    let mut ran = false;
    let out = service(
        hw,
        shape,
        members.iter().copied(),
        |cpu, row| view[cpu] = Some(row),
        |_| ran = true,
    );
    (out, ran)
}

/// What the table says of each member now.
fn rows_now(table: &[u8], shape: HfiShape, members: &[(usize, u16)], n: usize) -> View {
    let mut v = vec![None; n];
    for &(cpu, row) in members {
        v[cpu] = Some(hfi_row(table, shape, row));
    }
    v
}

/// A table of `shape` with `rows` rows of random cells, and random flags.
fn random_table(rng: &mut Rng, shape: HfiShape, rows: usize) -> Vec<u8> {
    let len = shape.row_offset(rows as u16).unwrap_or(0);
    let mut t: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
    // A table the processor wrote has a timestamp.
    t[0] |= 1;
    t
}

/// A package not yet written gives no value: the table is all zero and bit 26 clear, so nothing
/// is read, and no row reads as "avoid" (R97-14).
#[test]
fn an_unwritten_table_gives_no_value() {
    let shape = HfiShape::from_cpuid(eax(false), 0, 0b11).unwrap();
    let mut hw = Model::new(vec![0; 64]);
    let mut view: View = vec![None; 2];
    let (out, ran) = serve(&mut hw, shape, &[(0, 0), (1, 1)], &mut view);
    assert_eq!((out, ran), (Serviced::Quiet, false));
    assert_eq!(view, vec![None, None]);
    assert_eq!(hw.reads_while_clear, 0);
}

/// Two processors of one package take the same notification: the first reads and acknowledges,
/// the second finds bit 26 clear; an update that lands between them is the second's.
#[test]
fn two_processors_take_one_notification() {
    let shape = HfiShape::from_cpuid(eax(false), 0, 0b11).unwrap();
    let members = [(0, 0), (1, 1)];
    let mut table = vec![0u8; 32];
    table[0] = 1;
    table[16] = 200;
    table[17] = 100;
    table[24] = 0;
    table[25] = 0;
    let mut hw = Model::new(vec![0; 32]);
    hw.pending = Some(table.clone());
    hw.hardware();
    let mut view: View = vec![None; 2];
    let (first, ran) = serve(&mut hw, shape, &members, &mut view);
    assert!(matches!(first, Serviced::Updated(_)) && ran);
    assert_eq!(view, rows_now(&table, shape, &members, 2));
    assert!(
        view[1].unwrap().unwrap().avoid(),
        "an all-zero row is avoid"
    );
    assert_eq!(hw.status & STATUS_CHANGED, 0);
    let (second, ran) = serve(&mut hw, shape, &members, &mut view);
    assert_eq!((second, ran), (Serviced::Quiet, false));
    // An update between the two handlers is read by the second.
    let mut newer = table.clone();
    newer[24] = 50;
    newer[25] = 255;
    hw.pending = Some(newer.clone());
    hw.hardware();
    let (third, _) = serve(&mut hw, shape, &members, &mut view);
    assert!(matches!(third, Serviced::Updated(_)));
    assert!(view[1].unwrap().unwrap().consolidate());
    assert_eq!(hw.reads_while_clear, 0);
    assert!(!hw.refused);
}

/// A faulted status read, or a faulted acknowledgement, is reported as such; a row past the table
/// is `None`, never a value.
#[test]
fn faults_and_rows_past_the_table() {
    let shape = HfiShape::from_cpuid(eax(false), 0, 0b11).unwrap();
    let mut written = vec![0; 24];
    written[0] = 1;
    let mut hw = Model::new(written);
    hw.status = STATUS_CHANGED;
    hw.fault_status = true;
    let mut view: View = vec![None; 3];
    assert_eq!(
        serve(&mut hw, shape, &[(0, 0)], &mut view),
        (Serviced::Faulted, false)
    );
    hw.fault_status = false;
    hw.fault_ack = true;
    let (out, ran) = serve(&mut hw, shape, &[(0, 0), (1, 5)], &mut view);
    assert!(matches!(out, Serviced::Unacknowledged(_)) && ran);
    assert_eq!(view[1], Some(None), "row 5 lies past a 24-byte table");
    assert!(view[0].unwrap().is_some());
    // A table too short for its flags.
    let mut short = Model::new(vec![1; 8]);
    short.status = STATUS_CHANGED;
    assert_eq!(
        serve(&mut short, shape, &[(0, 0)], &mut view),
        (Serviced::Faulted, false)
    );
}

/// The protocol against the model, at random: hardware updates landing at any point between
/// handlers, shared rows and gaps, the HFI's table and Thread Director's. No row is read while bit
/// 26 is clear, each update read is the table as the hardware wrote it, and at the end no update
/// is lost (control C5, acknowledging before reading, fails this).
#[test]
fn no_update_is_lost_and_no_row_is_read_after_the_acknowledgement() {
    let mut rng = Rng::new(0x0106_9A0C);
    for case in 0..cases(2_000) {
        let td = rng.below(2) == 0;
        let classes = if td { 1 + rng.below(12) as u32 } else { 1 };
        let caps = [0b01, 0b11][rng.below(2)];
        let shape = HfiShape::from_cpuid(eax(td), classes << 8, caps).unwrap();
        let n = 1 + rng.below(12);
        let rows = 1 + rng.below(n + 2);
        // Rows may be shared, may leave gaps, and one may lie past the table.
        let members: Vec<(usize, u16)> = (0..n)
            .map(|cpu| (cpu, rng.below(rows + 1) as u16))
            .collect();
        let mut hw = Model::new(vec![0; shape.row_offset(rows as u16).unwrap()]);
        let mut view: View = vec![None; n];
        let mut seen = 0u32;
        for _ in 0..1 + rng.below(30) {
            if rng.below(2) == 0 {
                hw.pending = Some(random_table(&mut rng, shape, rows));
                hw.hardware();
            } else {
                let expected = rows_now(&hw.table, shape, &members, n);
                let written = hw.status & STATUS_CHANGED != 0;
                let (out, ran) = serve(&mut hw, shape, &members, &mut view);
                assert_eq!(ran, written, "case {case}");
                if written {
                    assert!(matches!(out, Serviced::Updated(_)), "case {case}");
                    assert_eq!(view, expected, "case {case}: the rows as written");
                    seen += 1;
                }
            }
        }
        // Drain: every update the hardware wrote is read.
        for _ in 0..3 {
            let _ = serve(&mut hw, shape, &members, &mut view);
        }
        assert_eq!(
            hw.status & STATUS_CHANGED,
            0,
            "case {case}: nothing left to read"
        );
        assert!(hw.pending.is_none(), "case {case}: no update held back");
        if seen > 0 || hw.table.iter().any(|&b| b != 0) {
            assert_eq!(
                view,
                rows_now(&hw.table, shape, &members, n),
                "case {case}: no update lost"
            );
        }
        assert_eq!(hw.reads_while_clear, 0, "case {case}");
        assert!(!hw.refused, "case {case}");
    }
}

/// Hostile tables and shapes never panic the service, and give a value only from bytes inside
/// the table.
#[test]
fn hostile_tables_never_panic_the_service() {
    let mut rng = Rng::new(0x0106_F022);
    for case in 0..cases(10_000) {
        let Some(shape) = HfiShape::from_cpuid(
            rng.edge_u32() | (1 << 19),
            rng.edge_u32(),
            rng.edge_u32() | 1,
        ) else {
            continue;
        };
        let table: Vec<u8> = (0..rng.below(400)).map(|_| rng.byte()).collect();
        let mut hw = Model::new(table.clone());
        hw.status = rng.edge_u64() | STATUS_CHANGED;
        let members: Vec<(usize, u16)> = (0..1 + rng.below(6))
            .map(|cpu| (cpu, rng.edge_u32() as u16))
            .collect();
        let mut view: View = vec![None; members.len()];
        let _ = serve(&mut hw, shape, &members, &mut view);
        for &(cpu, row) in &members {
            if let Some(Some(r)) = view[cpu] {
                assert_eq!(Some(r), hfi_row(&table, shape, row), "case {case}");
            }
        }
    }
}

#[test]
fn a_census_is_sound_only_without_a_failure() {
    let sound = Census {
        processors: 4,
        started: 4,
        ..Census::default()
    };
    assert!(sound.sound());
    for broken in [
        Census {
            faulted: 1,
            ..sound
        },
        Census {
            mismatched: 1,
            ..sound
        },
        Census {
            disagree: 1,
            ..sound
        },
        Census { failed: 1, ..sound },
        Census {
            no_memory: 1,
            ..sound
        },
        Census {
            unacknowledged: 1,
            ..sound
        },
        Census {
            rows_past: 1,
            ..sound
        },
        Census {
            no_topology: 1,
            ..sound
        },
        Census {
            started: 3,
            ..sound
        },
    ] {
        assert!(!broken.sound(), "{broken:?}");
    }
    // The firmware holding the LVT or the HFI is a finding, not a failure.
    assert!(
        Census {
            lvt_firmware: 4,
            firmware_hfi: 1,
            no_lvt: 1,
            ..sound
        }
        .sound()
    );
}

/// The handler's work on the 65-processor machine, modelled on the host (the coordinating
/// session's ruling Q14 of 2026-11-21; CL51): one package of 65 members, each reading its row; the
/// rescale over 65 processors (`choose_into`) and their publication in a live store; then the
/// acknowledgement. The MSR accesses are the model's, free here: on a processor each is one RDMSR
/// or WRMSR, which this does not measure. Run alone, on demand:
/// `cargo test -p origin-frame --lib hfi::tests::handler_model -- --ignored --nocapture`.
#[test]
#[ignore = "a timing model, run on demand for CL51"]
fn handler_model() {
    use crate::capacity::{Reports, Store, choose_into};
    const N: usize = 65;
    let shape = HfiShape::from_cpuid(eax(false), 0, 0b11).unwrap();
    let members: Vec<(usize, u16)> = (0..N).map(|cpu| (cpu, cpu as u16)).collect();
    let mut rng = Rng::new(0x0106_C151);
    let store = Store::new();
    assert!(store.start(&[crate::capacity::FULL; N]));
    // Shared between the service's two closures, as the kernel shares its records' atomics.
    let hfi = core::cell::RefCell::new(vec![None; N]);
    let mut out = vec![0u16; N];
    let mut samples = Vec::new();
    for _ in 0..2_000 {
        let mut hw = Model::new(vec![0; shape.row_offset(N as u16).unwrap()]);
        hw.pending = Some(random_table(&mut rng, shape, N));
        hw.hardware();
        let started = std::time::Instant::now();
        let served = service(
            &mut hw,
            shape,
            members.iter().copied(),
            |cpu, row| hfi.borrow_mut()[cpu] = row.map(|r| u32::from(r.performance)),
            |_| {
                let rows = hfi.borrow();
                let reports = Reports {
                    hfi: Some(&rows),
                    ..Reports::default()
                };
                let _ = choose_into(&reports, &mut out);
                for (cpu, &c) in out.iter().enumerate() {
                    store.publish(cpu, c, 0);
                }
            },
        );
        samples.push(started.elapsed().as_nanos());
        assert!(matches!(served, Serviced::Updated(_)));
    }
    samples.sort_unstable();
    let at = |q: usize| samples[(samples.len() - 1) * q / 100];
    std::println!(
        "hfi handler model, {N} processors, one package: p50 {} ns, p99 {} ns, max {} ns ({} runs)",
        at(50),
        at(99),
        samples[samples.len() - 1],
        samples.len()
    );
}

/// A bit 26 set over a table whose timestamp is still 0, as a bit left by a firmware's disable
/// would be: nothing is read, nothing reads as "avoid", and the bit is acknowledged so that the
/// processor can write (the review's R106-7).
#[test]
fn a_set_bit_over_an_unwritten_table_reads_nothing() {
    let shape = HfiShape::from_cpuid(eax(false), 0, 0b11).unwrap();
    let mut hw = Model::new(vec![0; 32]);
    hw.status = STATUS_CHANGED | (1 << 1);
    let mut view: View = vec![None; 2];
    let (out, ran) = serve(&mut hw, shape, &[(0, 0), (1, 1)], &mut view);
    assert_eq!((out, ran), (Serviced::Unwritten, false));
    assert_eq!(view, vec![None, None]);
    assert_eq!(hw.status & STATUS_CHANGED, 0, "acknowledged");
    assert_eq!(hw.status & (1 << 1), 1 << 1, "the log kept");
    // The processor's first write then follows, and is read.
    let mut table = vec![0u8; 32];
    table[0] = 9;
    table[16] = 100;
    hw.pending = Some(table.clone());
    hw.hardware();
    let (out, _) = serve(&mut hw, shape, &[(0, 0), (1, 1)], &mut view);
    assert!(matches!(out, Serviced::Updated(_)));
    assert_eq!(view, rows_now(&table, shape, &[(0, 0), (1, 1)], 2));
}

/// Idle is forced only with bit 1 in every class-0 capability's flag the table reports (SDM Vol.
/// 3, §17.6.7; the review's R106-11).
#[test]
fn force_idle_needs_every_capability() {
    let both = HfiShape::from_cpuid(eax(false), 0, 0b11).unwrap();
    let performance = HfiShape::from_cpuid(eax(false), 0, 0b01).unwrap();
    let flags = |p: bool, e: bool| HfiFlags {
        performance_force_idle: p,
        efficiency_force_idle: e,
        ..HfiFlags::default()
    };
    assert!(force_idle(flags(true, true), both));
    assert!(!force_idle(flags(true, false), both));
    assert!(!force_idle(flags(false, true), both));
    assert!(force_idle(flags(true, false), performance));
    assert!(!force_idle(flags(false, false), performance));
}

/// Heat needs a sensor beside the registers, and the LVT is wanted only by a source that would use
/// it: under QEMU 11.1.3's software processor, which reports CPUID.01H:EDX[22] and neither sensor,
/// nothing wants it.
#[test]
fn heat_needs_a_sensor_and_the_lvt_a_source() {
    let qemu = Words {
        edx1: ACPI,
        eax6: 1 << 2,
        ..Words::default()
    };
    assert!(qemu.thermal_registers() && !qemu.thermal_sensor() && !qemu.heat());
    assert!(!qemu.wants_lvt(false) && !qemu.wants_lvt(true));
    let tm1 = Words {
        edx1: ACPI | (1 << 29),
        ..Words::default()
    };
    let dts = Words {
        edx1: ACPI,
        eax6: 1,
        ..Words::default()
    };
    assert!(tm1.heat() && dts.heat() && tm1.wants_lvt(false) && dts.wants_lvt(false));
    // A sensor without the registers is no heat.
    assert!(
        !Words {
            edx1: 1 << 29,
            ..Words::default()
        }
        .heat()
    );
    // HWP's interrupt wants the LVT only where the firmware has HWP on.
    let hwp = Words {
        edx1: ACPI,
        eax6: (1 << 7) | (1 << 8) | (1 << 15),
        ..Words::default()
    };
    assert!(!hwp.wants_lvt(false) && hwp.wants_lvt(true));
    // The HFI wants it.
    let hfi = Words {
        edx1: ACPI,
        eax6: eax(false),
        edx6: 0b11,
        ..Words::default()
    };
    assert!(hfi.wants_lvt(false));
    // Without the registers nothing does.
    assert!(!Words { edx1: 0, ..hfi }.wants_lvt(true));
}

/// Heat's clause on the proof's line: under QEMU's software processor, which has the registers and
/// no sensor, it says heat is not supported and why, never a silent 0 (the coordinating session's
/// ruling of 2026-11-22).
#[test]
fn heat_without_a_sensor_says_it_is_not_supported() {
    let clause = |c: Census| alloc::format!("{}", HeatClause(&c));
    let qemu = Census {
        processors: 4,
        started: 4,
        thermal_registers: 4,
        ..Census::default()
    };
    assert_eq!(clause(qemu), "heat: not supported: no thermal sensor");
    assert_eq!(
        clause(Census {
            thermal_registers: 0,
            ..qemu
        }),
        "heat: not supported: no thermal registers"
    );
    assert_eq!(
        clause(Census {
            sensors: 4,
            heat: 3,
            hot: 1,
            ..qemu
        }),
        "heat on 3 of 4 with a sensor, 1 hot"
    );
}
