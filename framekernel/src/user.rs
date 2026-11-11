//! First user mode (M4 groundwork; CLAUDE.md §29 isolation and crash isolation, §58
//! kernel/user isolation): ring-3 programs that reach the kernel only through the `syscall`
//! instruction (ADR-0005 P4), and whose faults end them while the kernel carries on.
//!
//! Every program here is a process with its own address space (`process`): a code page
//! (read/execute) and a stack page (read/write, never executable) at the same user addresses
//! in each, one thread, which enters ring 3 with every general-purpose register zeroed:
//! - **good:** reports 42 with `DEBUG_REPORT`, then exits with code 0 (`THREAD_EXIT`);
//! - **reader:** reports 7, then reads a kernel address: a #PF with the user bit set;
//! - **privileged:** reports 9, then executes `hlt`: a #GP;
//! - **`int 0x80`:** reports 77, then tries the old gate, which is gone: a #GP.
#![forbid(unsafe_code)]

use alloc::vec::Vec;
use core::fmt::Write;
use core::ptr;

use crate::paging::{Access, PAGE};
use origin_abi::{Error, status_name};

use crate::console::Console;
use crate::cpu::CpuIndex;
use crate::process::{BOOT_PROOF_PRINCIPAL, Ending, Limits, MemoryLimits, ObjectLimits, Process};
use crate::sched;
use crate::vm;
use origin_frame::objref::ObjectRef;

/// User virtual address of each program's code page.
const CODE: u64 = 0x40_0000;
/// User virtual address of each program's stack page.
const STACK: u64 = 0x60_0000;
/// Where the isolation proof's data page is (the same in each process).
const DATA: u64 = 0x50_0000;
/// What a one-thread test program may hold.
const LIMITS: Limits = Limits {
    handles: 16,
    channels: 4,
    message_bytes: 1 << 20,
    debug_calls: true,
    memory: MemoryLimits::NONE,
    objects: ObjectLimits::NONE,
};

/// A kernel object for the reader to touch.
static TARGET: u64 = 0;

/// `mov edi, value; mov eax, DEBUG_REPORT; syscall`.
fn report(value: u8) -> [u8; 12] {
    [0xBF, value, 0, 0, 0, 0xB8, 1, 0, 0, 0, 0x0F, 0x05]
}

/// `xor edi, edi; xor eax, eax; syscall; jmp $`: exit 0.
const EXIT: [u8; 8] = [0x31, 0xFF, 0x31, 0xC0, 0x0F, 0x05, 0xEB, 0xFE];

/// Iterations of the compute-bound programs: long enough to be preempted many times.
const SPIN: u32 = 40_000_000;

/// A compute-bound program: counts `SPIN` down, checks that RSP is still its own stack top
/// (else reports 63), reports `value`, and exits.
fn spinner(value: u8, stack_top: u64) -> Vec<u8> {
    let mut p = alloc::vec![0xB9]; // mov ecx, SPIN
    p.extend_from_slice(&SPIN.to_le_bytes());
    p.extend_from_slice(&[0xFF, 0xC9, 0x75, 0xFC]); // 1: dec ecx; jnz 1b
    p.extend_from_slice(&[0x48, 0xB8]); // movabs rax, stack_top
    p.extend_from_slice(&stack_top.to_le_bytes());
    p.extend_from_slice(&[0x48, 0x39, 0xC4]); // cmp rsp, rax
    p.extend_from_slice(&[0xBF, value, 0, 0, 0]); // mov edi, value
    p.extend_from_slice(&[0x74, 0x05]); // je +5 (skip the next mov)
    p.extend_from_slice(&[0xBF, 63, 0, 0, 0]); // mov edi, 63
    p.extend_from_slice(&[0xB8, 1, 0, 0, 0, 0x0F, 0x05]); // report
    p.extend_from_slice(&EXIT);
    p
}

/// A process running `program` at [`CODE`] with one stack page, not started yet. System
/// calls are accepted only from its code page.
fn build(program: &[u8]) -> Option<Process> {
    let mut p = Process::new(LIMITS, (CODE, CODE + PAGE), BOOT_PROOF_PRINCIPAL)?;
    let frame = p.map_page(CODE, Access::ReadExecute).ok()?;
    if !p.write_frame(frame, 0, program) {
        return None;
    }
    p.map_page(STACK, Access::ReadWrite).ok()?;
    Some(p)
}

/// Starts `program` on `cpu` and returns its process.
fn start(program: &[u8], cpu: CpuIndex) -> Option<ObjectRef<Process>> {
    let process = ObjectRef::try_new(build(program)?).ok()?;
    Process::start_thread(&process, cpu, CODE, STACK + PAGE, 0).ok()?;
    Some(process)
}

/// Waits for `process`'s last thread to end.
fn wait_ended(process: &Process, tsc_hz: u64, seconds: u64) {
    sched::yield_until(|| process.ending().is_some(), tsc_hz, seconds);
}

/// What the program's last `DEBUG_REPORT` carried.
fn last_report(process: &Process) -> Option<u64> {
    process.reports().last().copied()
}

/// `int 0x80` is no longer a way in: no gate answers it. A program that tries it faults
/// (#GP, error code `(0x80 << 3) | 2`: an IDT selector), and the kernel carries on.
fn no_interrupt_gate(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let mut program = report(77).to_vec();
    program.extend_from_slice(&[0xCD, 0x80, 0xEB, 0xFE]); // int 0x80; jmp $
    let Some(process) = start(&program, bsp) else {
        return false;
    };
    wait_ended(&process, tsc_hz, 2);
    let reported = last_report(&process);
    let ending = process.ending();
    let right = reported == Some(77)
        && matches!(
            ending,
            Some(Ending::Faulted {
                vector: 13,
                error: 0x402,
                ..
            })
        );
    let outcome = match ending {
        Some(Ending::Faulted { vector, error, .. }) => {
            alloc::format!("ended by vector {vector} (error {error:#x})")
        }
        other => alloc::format!("ended by {other:?}"),
    };
    let _ = writeln!(
        com1,
        "origin-kernel: user mode: int 0x80 is not a way in: reported {reported:?}, {outcome}; kernel carries on{}",
        if right { "" } else { ": user isolation failed" }
    );
    right
}

/// The text the writer program passes to `DEBUG_WRITE`, at offset 0x200 of its code page.
const GREETING: &[u8] = b"hello from ring 3";

/// `movabs rdi, buf; mov esi, len; mov eax, DEBUG_WRITE; syscall`, then reports RAX (the
/// status) and exits.
fn writer(buf: u64, len: u8) -> Vec<u8> {
    let mut p = alloc::vec![0x48, 0xBF];
    p.extend_from_slice(&buf.to_le_bytes());
    p.extend_from_slice(&[0xBE, len, 0, 0, 0, 0xB8, 2, 0, 0, 0, 0x0F, 0x05]);
    p.extend_from_slice(&[0x48, 0x89, 0xC7]); // mov rdi, rax
    p.extend_from_slice(&[0xB8, 1, 0, 0, 0, 0x0F, 0x05]); // report
    p.extend_from_slice(&EXIT);
    p
}

/// `DEBUG_WRITE` copies from the caller's memory: a valid buffer is written to the console,
/// a kernel address is refused by the range check, and an unmapped user address by the walk
/// of the caller's own tables; the program gets `FAULT` and the kernel carries on.
fn copies(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let kernel_address = ptr::from_ref(&TARGET).addr() as u64;
    let cases = [
        ("valid buffer", CODE + 0x200, GREETING.len() as u8, 0),
        ("kernel address", kernel_address, 8, Error::Fault.raw()),
        (
            "unmapped user address",
            0x0000_7000_0000_0000,
            8,
            Error::Fault.raw(),
        ),
    ];
    let mut ok = true;
    let mut results = [0i64; 3];
    for (i, (_, buf, len, want)) in cases.into_iter().enumerate() {
        let mut program = writer(buf, len);
        program.resize(0x200, 0x90);
        program.extend_from_slice(GREETING);
        let Some(process) = start(&program, bsp) else {
            return false;
        };
        wait_ended(&process, tsc_hz, 2);
        let status = last_report(&process).map_or(i64::MAX, |v| v as i64);
        if let Some(r) = results.get_mut(i) {
            *r = status;
        }
        ok &= status == want && process.ending() == Some(Ending::Exited(0));
    }
    let _ = writeln!(
        com1,
        "origin-kernel: user mode: copy from user: valid buffer -> {}, kernel address -> {}, unmapped user address -> {}; kernel carries on{}",
        status_name(results[0]),
        status_name(results[1]),
        status_name(results[2]),
        if ok { "" } else { ": user isolation failed" }
    );
    ok
}

/// Two compute-bound user processes at once, preempted by the timer: both must finish with
/// the right report, which needs TSS.RSP0 to follow every context switch (otherwise one
/// thread's interrupt frames would land on the other's kernel stack).
fn concurrent(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let started = crate::cpu::rdtsc();
    let top = STACK + PAGE;
    let (Some(a), Some(b)) = (start(&spinner(1, top), bsp), start(&spinner(2, top), bsp)) else {
        return false;
    };
    sched::yield_until(|| a.ending().is_some() && b.ending().is_some(), tsc_hz, 10);
    let ms = crate::cpu::rdtsc().wrapping_sub(started) / (tsc_hz / 1000).max(1);
    let mask = [&a, &b]
        .iter()
        .filter_map(|p| last_report(p))
        .filter(|&v| v < 64)
        .fold(0u64, |m, v| m | (1 << v));
    let right = mask == 0b110
        && a.ending() == Some(Ending::Exited(0))
        && b.ending() == Some(Ending::Exited(0));
    let _ = writeln!(
        com1,
        "origin-kernel: user mode: two compute-bound user processes at once finished in {ms} ms, reports {mask:#b}; kernel stack follows every switch{}",
        if right { "" } else { ": user isolation failed" }
    );
    right
}

/// A program's name, the value it reports, and the fault (vector, error code) that must
/// end it, or `None` if it must exit with code 0.
type Case = (&'static str, u64, Option<(u64, u64)>);

/// Runs the programs and checks each outcome. Returns whether all were right.
pub(crate) fn prove(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let kernel_address = ptr::from_ref(&TARGET).addr() as u64;
    let mut reader = report(7).to_vec();
    reader.extend_from_slice(&[0x48, 0xB8]); // movabs rax, kernel_address
    reader.extend_from_slice(&kernel_address.to_le_bytes());
    reader.extend_from_slice(&[0x48, 0x8B, 0x00, 0xEB, 0xFE]); // mov rax, [rax]; jmp $
    let mut good = report(42).to_vec();
    good.extend_from_slice(&EXIT);
    let mut privileged = report(9).to_vec();
    privileged.extend_from_slice(&[0xF4, 0xEB, 0xFE]); // hlt; jmp $
    let programs: [(Case, &[u8]); 3] = [
        (("good program", 42, None), &good),
        (("kernel read", 7, Some((14, 0x5))), &reader), // #PF: present, read, user
        (("privileged instruction", 9, Some((13, 0))), &privileged), // #GP(0)
    ];
    let mut ok = true;
    for ((name, want_report, want_fault), program) in programs {
        let Some(process) = start(program, bsp) else {
            return false;
        };
        wait_ended(&process, tsc_hz, 2);
        let got_report = last_report(&process);
        let ending = process.ending();
        let (right, outcome) = match ending {
            Some(Ending::Faulted {
                vector,
                error,
                address,
            }) => (
                want_fault == Some((vector, error))
                    // Only a page fault's CR2 is reported: `privileged` (#GP) runs after
                    // `reader` (#PF on a kernel address), and must not be told that address.
                    && (vector == 14) == (address != 0),
                if vector == 14 {
                    alloc::format!("ended by vector 14 (error {error:#x}, address {address:#x})")
                } else {
                    alloc::format!("ended by vector {vector} (error {error:#x})")
                },
            ),
            Some(Ending::Exited(code)) => (
                want_fault.is_none() && code == 0,
                alloc::format!("exited with {:?}", Some(code)),
            ),
            None => (false, alloc::string::String::from("did not end")),
        };
        let right = right && got_report == Some(want_report);
        ok &= right;
        let _ = writeln!(
            com1,
            "origin-kernel: user mode: {name}: reported {got_report:?}, {outcome}; kernel carries on{}",
            if right { "" } else { ": user isolation failed" }
        );
    }
    if ok {
        ok &= concurrent(com1, bsp, tsc_hz);
    }
    if ok {
        ok &= no_interrupt_gate(com1, bsp, tsc_hz);
    }
    if ok {
        ok &= copies(com1, bsp, tsc_hz);
    }
    if ok {
        ok &= processes(com1, bsp, tsc_hz);
    }
    sched::yield_until(|| sched::reap() == 0, tsc_hz, 1);
    ok
}

/// Stores `value` at [`DATA`], spins long enough to be preempted (so the other process runs
/// and stores its own value at the same virtual address), reads the address back, reports
/// what it read, and exits.
fn isolation_program(value: u8) -> Vec<u8> {
    let data = (DATA as u32).to_le_bytes();
    let mut p = alloc::vec![0xB8, value, 0, 0, 0]; // mov eax, value
    p.extend_from_slice(&[0x48, 0x89, 0x04, 0x25]); // mov [data], rax
    p.extend_from_slice(&data);
    p.push(0xB9); // mov ecx, SPIN
    p.extend_from_slice(&SPIN.to_le_bytes());
    p.extend_from_slice(&[0xFF, 0xC9, 0x75, 0xFC]); // 1: dec ecx; jnz 1b
    p.extend_from_slice(&[0x48, 0x8B, 0x3C, 0x25]); // mov rdi, [data]
    p.extend_from_slice(&data);
    p.extend_from_slice(&[0xB8, 1, 0, 0, 0, 0x0F, 0x05]); // report
    p.extend_from_slice(&EXIT);
    p
}

/// Two processes run the same program at the same virtual addresses at the same time,
/// each in its own address space: each must read back only its own value, the kernel's
/// tables must not map their pages, and their PML4s must differ.
fn processes(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let mut procs = [None, None];
    for (k, value) in [(0usize, 11u8), (1, 22)] {
        let Some(mut p) = build(&isolation_program(value)) else {
            return false;
        };
        if p.map_page(DATA, Access::ReadWrite).is_err() {
            return false;
        }
        let Ok(shared) = ObjectRef::try_new(p) else {
            return false;
        };
        procs[k] = Some(shared);
    }
    let [Some(a), Some(b)] = procs else {
        return false;
    };
    let roots = [a.root(), b.root()];
    let in_kernel_space = vm::is_mapped(DATA);
    let in_each = a.is_mapped(DATA) && b.is_mapped(DATA);
    let top = STACK + PAGE;
    let started = Process::start_thread(&a, bsp, CODE, top, 0).is_ok()
        && Process::start_thread(&b, bsp, CODE, top, 0).is_ok();
    if started {
        sched::yield_until(|| a.ending().is_some() && b.ending().is_some(), tsc_hz, 10);
    }
    let mask = [&a, &b]
        .iter()
        .filter_map(|p| last_report(p))
        .filter(|&v| v < 64)
        .fold(0u64, |m, v| m | (1 << v));
    let right = started
        && mask == (1 << 11) | (1 << 22)
        && a.ending() == Some(Ending::Exited(0))
        && b.ending() == Some(Ending::Exited(0))
        && !in_kernel_space
        && in_each
        && roots[0] != roots[1];
    let _ = writeln!(
        com1,
        "origin-kernel: processes: PML4 {:#x} and {:#x} ran the same program at {CODE:#x} at once and read back {mask:#x} from {DATA:#x} (own values only: 0x400800); not mapped in the kernel's tables: {}{}",
        roots[0],
        roots[1],
        !in_kernel_space,
        if right {
            ""
        } else {
            ": process isolation failed"
        }
    );
    right
}
