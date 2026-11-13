//! The program of two boot proofs (task S4-2): "origin.core and the call pin" and "the flags at
//! a thread's entry" (`framekernel/src/ipc_proof/core_pin.rs`). The kernel carries it in its
//! image and launches it pinned per call, like any program that is not one of its assembly
//! proof programs (ADR-0011 Amendment 2, L8).
//!
//! It is a script interpreter, in Rust, for the scripts the kernel's proofs already write for
//! their assembly interpreter (`origin_frame::proof_programs`): a table of 64-byte steps at
//! [`raw::SCRIPT`], each a call number, a word of flags and six arguments. Where the assembly
//! interpreter runs `syscall` itself, this one makes each call through the import of that
//! number ([`calls`]), which the launch path bound to `origin.core`'s stub: so the same script
//! shows that every call, and every hostile call, gives through `origin.core` what it gives
//! directly. It holds no stub. What it does hold on purpose, in [`raw`], is what the pin must
//! stop: one `syscall` instruction of its own, and a jump into the middle of a stub.
//!
//! Its first argument says what to do (ADR-0005 E2: the arguments are bytes for the program
//! alone):
//!
//! - `flags`: reports RFLAGS as its own entry found them, before any other instruction of the
//!   thread ran (ADR-0005 Amendment 9, H1), and ends with [`FLAGS_REPORTED`];
//! - `script`: runs the script, reporting each call's status in order, as the assembly
//!   interpreter does, and ends with 0 at the script's end. A second argument of eight bytes
//!   is an address: the program first stores its own process handle there, for a script that
//!   names it.
//!
//! The pseudo-steps are the assembly interpreter's, with three more for this proof:
//!
//! | Number | Step |
//! |---|---|
//! | -1 | the end: exit with 0 |
//! | -2 | copy the `u32` at the first argument to the second |
//! | -3 | report the `u32` at the first argument |
//! | -4 | fault (`ud2`) |
//! | -5 | copy the `u64` at the first argument to the second |
//! | -6 | report the address of the `syscall` instruction in this program's own code, then make call number (first argument) with one argument (second) by it |
//! | -7 | jump to the address in the first argument with the second in RAX and the third in RDI |
//! | -8 | make call number (second argument) the first argument's number of times, with the next three as its arguments, and report the nanoseconds that took |
//!
//! Steps -6 and -7 must not return: the kernel stops the thread (ADR-0005 G1). If one does, its
//! status is reported and the program ends with [`NOT_STOPPED`], which fails the proof.
#![no_std]
#![no_main]
#![deny(unsafe_code)]

#[allow(unsafe_code)]
mod calls;
#[allow(unsafe_code)]
mod raw;

use origin_abi::start::role;
use origin_kernel_client::origin_kernel::kernel::client;
use origin_rt::Start;

origin_rt::main!(main);

/// The exit code of `flags`.
const FLAGS_REPORTED: u64 = 0x0046_4C41_4753;
/// The exit code after a forbidden call that came back.
const NOT_STOPPED: u64 = 0x0053_544F_5050;
/// The exit code of an argument it does not know.
const UNKNOWN_MODE: u64 = 0x0042_4144;
/// The exit code of a step it does not know, or a call number it has no import for.
const BAD_STEP: u64 = 0x0042_5354;
/// Bytes in a step.
const STEP: u64 = 64;

fn main(start: &Start) -> u64 {
    let mut args = start.record.args.iter();
    match args.next().unwrap_or(&[]) {
        b"flags" => {
            report(raw::entry_flags());
            FLAGS_REPORTED
        }
        b"script" => {
            let own = start.record.find(role::SELF_PROCESS);
            let slot = args
                .next()
                .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok());
            if let (Some(entry), Some(slot)) = (own, slot) {
                raw::write_u64(u64::from_le_bytes(slot), entry.handle);
            }
            interpret(raw::SCRIPT)
        }
        _ => UNKNOWN_MODE,
    }
}

/// Reports one value: the proof reads the log. A failure is ignored, since the proof then finds
/// a value missing.
fn report(value: u64) {
    let _ = client::debug_report(value);
}

/// Runs the script at `at` to its end, and returns the exit code.
fn interpret(mut at: u64) -> u64 {
    loop {
        let [number, flags, arguments @ ..] = raw::step(at);
        let argument = |i: usize| arguments.get(i).copied().unwrap_or(0);
        match number as i64 {
            -1 => return 0,
            -2 => raw::write_u32(argument(1), raw::read_u32(argument(0))),
            -3 => report(u64::from(raw::read_u32(argument(0)))),
            -4 => raw::fault(),
            -5 => raw::write_u64(argument(1), raw::read_u64(argument(0))),
            -6 => {
                // Where the instruction is, first: the proof compares the thread's ending with
                // it.
                report(raw::own_syscall_address());
                report(raw::own_syscall(argument(0), argument(1)) as u64);
                return NOT_STOPPED;
            }
            -7 => {
                report(raw::jump_into(argument(0), argument(1), argument(2)) as u64);
                return NOT_STOPPED;
            }
            -8 => {
                let call = [argument(2), argument(3), argument(4), 0, 0, 0];
                let Some(nanos) = timed(argument(0), argument(1), call) else {
                    return BAD_STEP;
                };
                report(nanos);
            }
            n if n >= 0 => {
                // An argument whose flag is set is the address of a handle to load.
                let mut loaded = arguments;
                for (i, value) in loaded.iter_mut().enumerate() {
                    if flags & (1 << i) != 0 {
                        *value = raw::read_u64(*value);
                    }
                }
                let Some(status) = calls::call(number, loaded) else {
                    return BAD_STEP;
                };
                report(status as u64);
            }
            _ => return BAD_STEP,
        }
        at += STEP;
    }
}

/// Makes call `number` with `arguments`, `times` times, through its import, and returns the
/// nanoseconds between a reading of the clock before and one after. `None` if the number has no
/// import or the clock cannot be read.
fn timed(times: u64, number: u64, arguments: [u64; 6]) -> Option<u64> {
    let before = client::clock_read().ok()?;
    for _ in 0..times {
        calls::call(number, arguments)?;
    }
    let after = client::clock_read().ok()?;
    Some(after.wrapping_sub(before))
}
