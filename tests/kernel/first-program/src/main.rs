//! The first Rust program in ring 3 (task S4-1; ADR-0011 Amendment 2, L1 and L8): a boot proof
//! that the kernel carries in its image, launches at a random base and judges
//! (`framekernel/src/program_proof.rs`).
//!
//! It links today's generated stubs for `origin.kernel` into itself and runs under ADR-0005 A6.
//! Its first argument says what to do (ADR-0005 E2: the arguments are bytes for the program
//! alone). In every mode it first reports, with `debug_report`, what it found: the start
//! record's address, where its entry runs, and its record's header and entries ([`report`]).
//! Then:
//!
//! - `run`: reads data only through relocated pointers, checks the runtime's `mem*` functions,
//!   writes a message on its BOOTSTRAP channel, and ends with [`RAN`];
//! - `write-code`: writes over its own code, which must fault;
//! - `jump-data`: jumps into its own writable data, which must fault;
//! - `write-relro`: writes over a relocated pointer in its RELRO page, which must fault;
//! - `panic`: panics, which the runtime turns into [`origin_rt::PANIC_EXIT`].
//!
//! The values it reports, and their order, are a contract with the proof, which keeps its own
//! copy of every constant below.
#![no_std]
#![no_main]
#![deny(unsafe_code)]

#[allow(unsafe_code)]
mod faults;
mod relocated;

use core::hint::black_box;

use origin_abi::start::role;
use origin_kernel_stubs::oidl_rt::Handle;
use origin_kernel_stubs::origin_kernel::kernel::client;
use origin_rt::Start;

origin_rt::main!(main);

/// The exit code of a `run` that did everything it was asked.
const RAN: u64 = 0x0052_5553_5433;
/// The exit code of an argument it does not know.
const UNKNOWN_MODE: u64 = 0x0042_4144;
/// What it writes on BOOTSTRAP in `run`.
const GREETING: &[u8] = b"first program: Rust in ring 3";
/// Reported just before each deliberate fault, then the address it is about to fault at.
const ABOUT_TO_WRITE_CODE: u64 = 0xC0DE;
/// Reported just before the jump into data.
const ABOUT_TO_JUMP_INTO_DATA: u64 = 0xDA7A;
/// Reported just before the write into RELRO.
const ABOUT_TO_WRITE_RELRO: u64 = 0x2E10;
/// Reported just before the panic.
const ABOUT_TO_PANIC: u64 = 0x9A1C;

fn main(start: &Start) -> u64 {
    report_start(start);
    let mode = start.record.args.iter().next().unwrap_or(&[]);
    match mode {
        b"run" => run(start),
        b"write-code" => {
            report(ABOUT_TO_WRITE_CODE);
            faults::write_code(report)
        }
        b"jump-data" => {
            report(ABOUT_TO_JUMP_INTO_DATA);
            faults::jump_data(report)
        }
        b"write-relro" => {
            report(ABOUT_TO_WRITE_RELRO);
            faults::write_relro(report, relocated::words_address())
        }
        b"panic" => {
            report(ABOUT_TO_PANIC);
            // Not foldable: the compiler cannot see that the value is `None`.
            black_box(None::<u64>).expect("asked to panic")
        }
        _ => UNKNOWN_MODE,
    }
}

/// Reports one value. A program the kernel launched as a boot proof may (its limits allow the
/// developer calls); a failure is ignored, since the proof then finds a value missing.
fn report(value: u64) {
    let _ = client::debug_report(value);
}

/// Reports what the record says, in the order the proof reads it: RDI, the entry's address, the
/// ABI (`major << 32 | minor << 16 | oldest minor`), the number of entries, then for each entry
/// `role << 32 | object type` and `rights | handle-is-not-zero << 32`, then the number of
/// arguments.
fn report_start(start: &Start) {
    report(start.address);
    report(origin_rt::entry_address());
    let record = &start.record;
    let (major, minor) = record.abi;
    report(u64::from(major) << 32 | u64::from(minor) << 16 | u64::from(record.abi_min_minor));
    report(record.entries().count() as u64);
    for entry in record.entries() {
        report(u64::from(entry.role) << 32 | u64::from(entry.object_type));
        report(u64::from(entry.rights) | u64::from(entry.handle != 0) << 32);
    }
    report(record.args.count() as u64);
}

/// `run`: reports the values it read through relocated pointers and the `mem*` checks, writes
/// [`GREETING`] on BOOTSTRAP and reports how that went (0 for success), then ends with [`RAN`].
fn run(start: &Start) -> u64 {
    report(u64::from(relocated::words_match()));
    report(relocated::steps());
    report(relocated::mem_checks());
    let written = start.record.find(role::BOOTSTRAP).map_or(2, |entry| {
        let end = Handle::from_abi(entry.handle);
        u64::from(client::channel_write(end, GREETING, &[]).is_err())
    });
    report(written);
    RAN
}
