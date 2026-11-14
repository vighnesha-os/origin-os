//! QEMU's own tables, when a directory of them is given (the coordinating session's ruling of
//! 2026-11-14: they are QEMU's output and stay out of the repository). `ORIGIN_AML_TABLES` names
//! a directory searched for files whose first four bytes are `DSDT` or `SSDT`; each DSDT is loaded
//! with the SSDTs beside it, and the named results the boot proof will check (step B3) are checked
//! here. Without the variable the test does nothing and says so.

use std::path::Path;

use super::{TestHost, tables_standin};
use crate::{Config, Interpreter, Limits, Value};

fn tables_in(dir: &Path) -> Vec<Vec<u8>> {
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
                out.push(bytes);
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
    let dsdts: Vec<&Vec<u8>> = tables.iter().filter(|t| t.starts_with(b"DSDT")).collect();
    assert!(!dsdts.is_empty(), "no DSDT under {dir:?}");
    for dsdt in dsdts {
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
        eprintln!(
            "QEMU DSDT ({} bytes, revision {}): loaded in {:?}; {} nodes, {} objects, {} bytes held",
            dsdt.len(),
            dsdt[8],
            loaded,
            i.node_count(),
            i.object_count(),
            i.memory_used()
        );
    }
}
