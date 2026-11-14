//! QEMU's own tables, when a directory of them is given (the coordinating session's ruling of
//! 2026-11-14: they are QEMU's output and stay out of the repository). `ORIGIN_AML_TABLES` names
//! a directory searched for files whose first four bytes are `DSDT` or `SSDT`; each DSDT is loaded,
//! then the SSDTs in its own directory, and the named results the boot proof will check (step B3)
//! are checked. Without the variable the test does nothing and says so.
//!
//! This is the only check of the parser against tables it did not make (the review of B1, F10 and
//! F15), and `check` does not run it: it is run by hand, and its runs are recorded in the
//! provenance entry. The five dumps of QEMU 11.1.3 read so far hold no SSDT. Step B3's boot proof
//! is to dump the tables it reads and run this test on them.

use std::path::{Path, PathBuf};

use super::{TestHost, tables_standin};
use crate::{Config, ErrorKind, InitOutcome, Interpreter, Limits, Value};

/// Every DSDT and SSDT under `dir`, with the directory it is in.
fn tables_in(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(bytes) = std::fs::read(&p)
                && (bytes.starts_with(b"DSDT") || bytes.starts_with(b"SSDT"))
            {
                out.push((d.clone(), bytes));
            }
        }
    }
    out
}

#[test]
fn qemu_tables_load_and_answer() {
    let Some(dir) = std::env::var_os("ORIGIN_AML_TABLES") else {
        eprintln!("qemu_tables_load_and_answer: ORIGIN_AML_TABLES is not set; nothing checked");
        return;
    };
    let tables = tables_in(Path::new(&dir));
    let dsdts: Vec<&(PathBuf, Vec<u8>)> =
        tables.iter().filter(|t| t.1.starts_with(b"DSDT")).collect();
    assert!(!dsdts.is_empty(), "no DSDT under {dir:?}");
    for (here, dsdt) in dsdts {
        let (block, width) = tables_standin::block(dsdt).expect("QEMU's DSDT is well-formed");
        let start = std::time::Instant::now();
        let mut i = Interpreter::new(
            TestHost::default(),
            Config {
                limits: Limits::BOOT_PROOF,
            },
            width,
        )
        .unwrap();
        i.load(block).expect("QEMU's DSDT loads");
        let mut ssdts = 0usize;
        for (_, ssdt) in tables
            .iter()
            .filter(|(d, t)| d == here && t.starts_with(b"SSDT"))
        {
            let (block, _) = tables_standin::block(ssdt).expect("QEMU's SSDT is well-formed");
            i.load(block).expect("QEMU's SSDT loads");
            ssdts += 1;
        }
        let loaded = start.elapsed();
        assert_eq!(
            i.evaluate("\\_S5_", &[]).unwrap(),
            Some(Value::Package(vec![Value::Integer(0); 4]))
        );
        assert_eq!(i.evaluate("\\PICF", &[]).unwrap(), Some(Value::Integer(0)));
        assert_eq!(i.evaluate("\\_PIC", &[Value::Integer(1)]).unwrap(), None);
        assert_eq!(i.evaluate("\\PICF", &[]).unwrap(), Some(Value::Integer(1)));
        // EISAID ("PNP0A08").
        assert_eq!(
            i.evaluate("\\_SB.PCI0._HID", &[]).unwrap(),
            Some(Value::Integer(0x080A_D041))
        );
        match i.evaluate("\\_SB.PCI0._CRS", &[]).unwrap() {
            Some(Value::Buffer(b)) => assert_eq!(b[b.len() - 2], 0x79, "an end tag"),
            other => panic!("_CRS: {other:?}"),
        }
        // The walk the boot proof will make (step B3), with every region access refused.
        i.host_mut().refuse = true;
        let mut outcomes = Vec::new();
        let walk = std::time::Instant::now();
        i.initialize(&mut |n, o| outcomes.push((n, o))).unwrap();
        let walked = walk.elapsed();
        let mut refused_by_space = [0usize; 3];
        let (mut initialized, mut other) = (0usize, 0usize);
        for (n, o) in &outcomes {
            match o {
                InitOutcome::StaFailed(e) => match e.kind {
                    ErrorKind::Region {
                        space,
                        cause: crate::HostError::Refused,
                        ..
                    } if space < 3 => refused_by_space[usize::from(space)] += 1,
                    k => panic!("{:?}: _STA failed with {k:?}", i.path_of(*n)),
                },
                InitOutcome::Initialized { .. } => initialized += 1,
                _ => other += 1,
            }
        }
        let hpet = i.lookup("\\_SB.HPET").unwrap().unwrap();
        assert!(outcomes.iter().any(|(n, o)| *n == hpet
            && matches!(o, InitOutcome::StaFailed(e) if matches!(e.kind, ErrorKind::Region { space: 0, .. }))));
        let c000 = i.lookup("\\_SB.CPUS.C000").unwrap().unwrap();
        assert!(outcomes.iter().any(|(n, o)| *n == c000
            && matches!(o, InitOutcome::StaFailed(e) if matches!(e.kind, ErrorKind::Region { space: 1, .. }))));
        let lnka = i.lookup("\\_SB.LNKA").unwrap().unwrap();
        assert!(outcomes.iter().any(|(n, o)| *n == lnka
            && matches!(o, InitOutcome::StaFailed(e) if matches!(e.kind, ErrorKind::Region { space: 2, .. }))));
        assert_eq!(i.host().global_held, 0, "the Global Lock was given back");
        eprintln!(
            "QEMU DSDT ({} bytes, revision {}) and {} SSDTs: loaded in {:?}; {} nodes, {} objects, {} bytes held; \
             walk in {:?}: {} devices, {} initialized, _STA refused in SystemMemory {}, SystemIO {}, \
             PCI_Config {}, {} other",
            dsdt.len(),
            dsdt[8],
            ssdts,
            loaded,
            i.node_count(),
            i.object_count(),
            i.memory_used(),
            walked,
            outcomes.len(),
            initialized,
            refused_by_space[0],
            refused_by_space[1],
            refused_by_space[2],
            other
        );
    }
}
