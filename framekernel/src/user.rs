//! First user mode (M4 groundwork; CLAUDE.md §29 isolation and crash isolation, §58
//! kernel/user isolation): ring-3 code that can only reach the kernel through the one DPL-3
//! gate, and whose faults end its thread while the kernel carries on.
//!
//! Three tiny user programs run, one after another, as threads on the boot processor, from
//! a user code page (read/execute) with a user stack page (read/write, never executable),
//! both at the top of the lower half:
//! - **good:** reports 42 with system call 1, then exits with code 0 (system call 0);
//! - **reader:** reports 7, then reads a kernel address: a #PF with the user bit set;
//! - **privileged:** reports 9, then executes `hlt`: a #GP.
//!
//! Each thread sets TSS.RSP0 to its own kernel stack and enters user mode with every
//! general-purpose register zeroed. The threads run one at a time because RSP0 is set by
//! the thread itself; concurrent user threads need RSP0 switched on every context switch
//! (M4). User mode keeps the kernel's GS base (`swapgs` comes with M4's system-call path).

use alloc::alloc::{Layout, alloc_zeroed, dealloc};
use core::fmt::Write;
use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering};

use framekernel::paging::Access;

use crate::console::Console;
use crate::cpu;
use crate::sched;
use crate::vm;

/// User virtual address of the code page.
const CODE: u64 = 0x0000_7FFF_FFF0_0000;
/// User virtual address of the stack page.
const STACK: u64 = 0x0000_7FFF_FFF1_0000;

/// The value the last report system call carried, plus one (0: none).
static REPORTED: AtomicU64 = AtomicU64::new(0);
/// The exit code of the last exit system call, plus one (0: none).
static EXIT_CODE: AtomicU64 = AtomicU64::new(0);
/// The last user fault: vector + 1 (0: none), error code, CR2.
static FAULT: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
/// User threads that have ended, by exit or by fault.
static ENDED: AtomicU64 = AtomicU64::new(0);
/// Bit `v` is set when a report system call carried `v` (v < 64).
static REPORT_MASK: AtomicU64 = AtomicU64::new(0);

pub(crate) fn record_report(value: u64) {
    REPORTED.store(value + 1, Ordering::Relaxed);
    if value < 64 {
        REPORT_MASK.fetch_or(1 << value, Ordering::Relaxed);
    }
}

pub(crate) fn record_exit(code: u64) {
    EXIT_CODE.store(code + 1, Ordering::Relaxed);
    ENDED.fetch_add(1, Ordering::Release);
}

pub(crate) fn record_fault(vector: u64, error: u64, address: u64) {
    FAULT[0].store(vector + 1, Ordering::Relaxed);
    FAULT[1].store(error, Ordering::Relaxed);
    FAULT[2].store(address, Ordering::Relaxed);
    ENDED.fetch_add(1, Ordering::Release);
}

/// `mov edi, value; mov eax, 1; int 0x80`: report `value`.
fn report(value: u8) -> [u8; 12] {
    [0xBF, value, 0, 0, 0, 0xB8, 1, 0, 0, 0, 0xCD, 0x80]
}

/// The three programs, at offsets 0x000, 0x100 and 0x200 of the code page.
fn programs(kernel_address: u64) -> [(usize, alloc::vec::Vec<u8>); 3] {
    let mut good = report(42).to_vec();
    // xor edi, edi; xor eax, eax; int 0x80 (exit 0); jmp $
    good.extend_from_slice(&[0x31, 0xFF, 0x31, 0xC0, 0xCD, 0x80, 0xEB, 0xFE]);
    let mut reader = report(7).to_vec();
    reader.extend_from_slice(&[0x48, 0xB8]); // movabs rax, kernel_address
    reader.extend_from_slice(&kernel_address.to_le_bytes());
    reader.extend_from_slice(&[0x48, 0x8B, 0x00, 0xEB, 0xFE]); // mov rax, [rax]; jmp $
    let mut privileged = report(9).to_vec();
    privileged.extend_from_slice(&[0xF4, 0xEB, 0xFE]); // hlt; jmp $
    [(0x000, good), (0x100, reader), (0x200, privileged)]
}

/// The same as the good program, but with the `syscall` instruction (at offset 0x400):
/// mov edi, 77; mov eax, 1; syscall; xor edi, edi; xor eax, eax; syscall; jmp $.
const SYSCALLER: [u8; 22] = [
    0xBF, 77, 0, 0, 0, 0xB8, 1, 0, 0, 0, 0x0F, 0x05, 0x31, 0xFF, 0x31, 0xC0, 0x0F, 0x05, 0xEB,
    0xFE, 0x90, 0x90,
];

/// Iterations of the compute-bound programs: long enough to be preempted many times.
const SPIN: u32 = 40_000_000;

/// A compute-bound program at `offset` 0x300 + 0x40·k: counts `SPIN` down, checks that
/// RSP is still its own stack top (else reports 63), reports `value`, and exits.
fn spinner(value: u8, stack_top: u64) -> alloc::vec::Vec<u8> {
    let mut p = alloc::vec![0xB9]; // mov ecx, SPIN
    p.extend_from_slice(&SPIN.to_le_bytes());
    p.extend_from_slice(&[0xFF, 0xC9, 0x75, 0xFC]); // 1: dec ecx; jnz 1b
    p.extend_from_slice(&[0x48, 0xB8]); // movabs rax, stack_top
    p.extend_from_slice(&stack_top.to_le_bytes());
    p.extend_from_slice(&[0x48, 0x39, 0xC4]); // cmp rsp, rax
    p.extend_from_slice(&[0xBF, value, 0, 0, 0]); // mov edi, value
    p.extend_from_slice(&[0x74, 0x05]); // je +5 (skip the next mov)
    p.extend_from_slice(&[0xBF, 63, 0, 0, 0]); // mov edi, 63
    p.extend_from_slice(&[0xB8, 1, 0, 0, 0, 0xCD, 0x80]); // report
    p.extend_from_slice(&[0x31, 0xFF, 0x31, 0xC0, 0xCD, 0x80, 0xEB, 0xFE]); // exit 0
    p
}

/// A kernel thread that becomes a user thread at `entry`, with the stack top `STACK +
/// 4096` (or, for the spinners, their own half of the stack page).
extern "sysv64" fn user_thread(entry: u64) -> ! {
    let stack_top = match entry.checked_sub(CODE + 0x300) {
        Some(k) => STACK + 2048 * (k / 0x40 + 1),
        None => STACK + 4096,
    };
    match sched::current_stack_top() {
        Some(top) => {
            cpu::set_kernel_stack(top);
            // SAFETY: `entry` and the stack are in the user pages `prove` mapped; RSP0 is
            // this thread's own kernel stack, which it leaves for good here (the scheduler
            // keeps RSP0 on the running thread's stack from now on).
            unsafe { cpu::enter_user(entry, stack_top, cpu::cpu_local_base()) }
        }
        None => sched::exit(),
    }
}

/// The `syscall` instruction reaches the same system calls as `int 0x80`.
fn by_syscall(com1: &mut Console, bsp: crate::cpu::CpuIndex, tsc_hz: u64) -> bool {
    REPORTED.store(0, Ordering::Relaxed);
    EXIT_CODE.store(0, Ordering::Relaxed);
    let ended = ENDED.load(Ordering::Acquire);
    if sched::spawn(bsp, user_thread, CODE + 0x400).is_err() {
        return false;
    }
    sched::yield_until(|| ENDED.load(Ordering::Acquire) > ended, tsc_hz, 2);
    let reported = REPORTED.load(Ordering::Relaxed).checked_sub(1);
    let exited = EXIT_CODE.load(Ordering::Relaxed).checked_sub(1);
    let right = reported == Some(77) && exited == Some(0);
    let _ = writeln!(
        com1,
        "origin-kernel: user mode: syscall instruction: reported {reported:?}, exited with {exited:?}{}",
        if right { "" } else { ": user isolation failed" }
    );
    right
}

/// The text the writer program passes to system call 2, at offset 0x700 of the code page.
const GREETING: &[u8] = b"hello from ring 3";

/// `movabs rdi, buf; mov esi, len; mov eax, 2; int 0x80` (write), then report RAX (the
/// result) and exit.
fn writer(buf: u64, len: u8) -> alloc::vec::Vec<u8> {
    let mut p = alloc::vec![0x48, 0xBF];
    p.extend_from_slice(&buf.to_le_bytes());
    p.extend_from_slice(&[0xBE, len, 0, 0, 0, 0xB8, 2, 0, 0, 0, 0xCD, 0x80]);
    p.extend_from_slice(&[0x48, 0x89, 0xC7]); // mov rdi, rax
    p.extend_from_slice(&[0xB8, 1, 0, 0, 0, 0xCD, 0x80]); // report
    p.extend_from_slice(&[0x31, 0xFF, 0x31, 0xC0, 0xCD, 0x80, 0xEB, 0xFE]); // exit 0
    p
}

/// System call 2 copies from user memory: a valid buffer is written to the console, a
/// kernel address is refused by the range check, and an unmapped user address faults
/// inside the copy and is refused too; the kernel carries on.
fn copies(com1: &mut Console, bsp: crate::cpu::CpuIndex, tsc_hz: u64) -> bool {
    let cases = [
        ("valid buffer", 0x500u64, GREETING.len() as u64),
        ("kernel address", 0x580, cpu::EFAULT),
        ("unmapped user address", 0x600, cpu::EFAULT),
    ];
    let mut ok = true;
    let mut results = [0u64; 3];
    for (i, (_, offset, want)) in cases.iter().enumerate() {
        REPORTED.store(0, Ordering::Relaxed);
        EXIT_CODE.store(0, Ordering::Relaxed);
        let ended = ENDED.load(Ordering::Acquire);
        if sched::spawn(bsp, user_thread, CODE + offset).is_err() {
            return false;
        }
        sched::yield_until(|| ENDED.load(Ordering::Acquire) > ended, tsc_hz, 2);
        let got = REPORTED.load(Ordering::Relaxed).wrapping_sub(1);
        let exited = EXIT_CODE.load(Ordering::Relaxed).checked_sub(1);
        if let Some(r) = results.get_mut(i) {
            *r = got;
        }
        ok &= got == *want && exited == Some(0);
    }
    let show = |v: u64| {
        if v == cpu::EFAULT {
            alloc::string::String::from("EFAULT")
        } else {
            alloc::format!("{v}")
        }
    };
    let _ = writeln!(
        com1,
        "origin-kernel: user mode: copy from user: valid buffer -> {}, kernel address -> {}, unmapped user address -> {}; kernel carries on{}",
        show(results[0]),
        show(results[1]),
        show(results[2]),
        if ok { "" } else { ": user isolation failed" }
    );
    ok
}

/// Two compute-bound user threads at once, preempted by the timer: both must finish with
/// the right report, which needs TSS.RSP0 to follow every context switch (otherwise one
/// thread's interrupt frames would land on the other's kernel stack).
fn concurrent(com1: &mut Console, bsp: crate::cpu::CpuIndex, tsc_hz: u64) -> bool {
    REPORT_MASK.store(0, Ordering::Relaxed);
    let ended = ENDED.load(Ordering::Acquire);
    let started = crate::cpu::rdtsc();
    for k in 0..2 {
        if sched::spawn(bsp, user_thread, CODE + 0x300 + 0x40 * k).is_err() {
            return false;
        }
    }
    sched::yield_until(|| ENDED.load(Ordering::Acquire) >= ended + 2, tsc_hz, 10);
    let ms = crate::cpu::rdtsc().wrapping_sub(started) / (tsc_hz / 1000).max(1);
    let mask = REPORT_MASK.load(Ordering::Relaxed);
    let right = mask == 0b110 && ENDED.load(Ordering::Acquire) == ended + 2;
    let _ = writeln!(
        com1,
        "origin-kernel: user mode: two compute-bound user threads at once finished in {ms} ms, reports {mask:#b}; kernel stack follows every switch{}",
        if right { "" } else { ": user isolation failed" }
    );
    right
}

/// A program's name, the value it reports, and the fault (vector, error code) that must
/// end it, or `None` if it must exit with code 0.
type Case = (&'static str, u64, Option<(u64, u64)>);

/// Runs the three programs and checks each outcome. Returns whether all were right.
pub(crate) fn prove(com1: &mut Console, bsp: crate::cpu::CpuIndex, tsc_hz: u64) -> bool {
    let Ok(layout) = Layout::from_size_align(4096, 4096) else {
        return false;
    };
    // SAFETY: non-zero size.
    let code = unsafe { alloc_zeroed(layout) };
    // SAFETY: non-zero size.
    let stack = unsafe { alloc_zeroed(layout) };
    if code.is_null() || stack.is_null() {
        return false;
    }
    let kernel_address = ptr::from_ref(&REPORTED).addr() as u64;
    for (offset, bytes) in programs(kernel_address) {
        // SAFETY: each program is under 0x100 bytes and lands inside the fresh code frame.
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), code.wrapping_add(offset), bytes.len()) };
    }
    // Two spinners for the concurrent phase, each with its own half of the stack page.
    for (k, value) in [(0usize, 1u8), (1, 2)] {
        let top = STACK + 2048 * (k as u64 + 1);
        let bytes = spinner(value, top);
        // SAFETY: each spinner is under 0x40 bytes and lands inside the code frame.
        unsafe {
            ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                code.wrapping_add(0x300 + 0x40 * k),
                bytes.len(),
            );
        }
    }
    let writers = [
        (0x500usize, writer(CODE + 0x700, GREETING.len() as u8)),
        (0x580, writer(ptr::from_ref(&REPORTED).addr() as u64, 8)),
        (0x600, writer(0x0000_7000_0000_0000, 8)),
    ];
    for (offset, bytes) in &writers {
        // SAFETY: each writer is under 0x80 bytes and lands inside the code frame.
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), code.wrapping_add(*offset), bytes.len())
        };
    }
    // SAFETY: the greeting is 17 bytes at offset 0x700 of the code frame.
    unsafe {
        ptr::copy_nonoverlapping(GREETING.as_ptr(), code.wrapping_add(0x700), GREETING.len())
    };
    // SAFETY: the program is 22 bytes at offset 0x400 of the code frame.
    unsafe {
        ptr::copy_nonoverlapping(
            SYSCALLER.as_ptr(),
            code.wrapping_add(0x400),
            SYSCALLER.len(),
        )
    };
    let phys = |p: *mut u8| crate::phys::to_phys(p.expose_provenance() as u64);
    let mapped = vm::map_user(CODE, phys(code), Access::ReadExecute)
        .and_then(|()| vm::map_user(STACK, phys(stack), Access::ReadWrite));
    let mut ok = mapped.is_ok();
    if ok {
        let cases: [Case; 3] = [
            ("good program", 42, None),
            ("kernel read", 7, Some((14, 0x5))), // #PF: present, read, user
            ("privileged instruction", 9, Some((13, 0))), // #GP(0)
        ];
        for (i, (name, report, fault)) in cases.into_iter().enumerate() {
            REPORTED.store(0, Ordering::Relaxed);
            EXIT_CODE.store(0, Ordering::Relaxed);
            FAULT[0].store(0, Ordering::Relaxed);
            let ended = ENDED.load(Ordering::Acquire);
            let entry = CODE + 0x100 * i as u64;
            if sched::spawn(bsp, user_thread, entry).is_err() {
                ok = false;
                break;
            }
            sched::yield_until(|| ENDED.load(Ordering::Acquire) > ended, tsc_hz, 2);
            let got_report = REPORTED.load(Ordering::Relaxed).checked_sub(1);
            let got_fault = FAULT[0]
                .load(Ordering::Relaxed)
                .checked_sub(1)
                .map(|v| (v, FAULT[1].load(Ordering::Relaxed)));
            let exited = EXIT_CODE.load(Ordering::Relaxed).checked_sub(1);
            let right = got_report == Some(report)
                && got_fault == fault
                && (fault.is_some() || exited == Some(0));
            ok &= right;
            let outcome = match got_fault {
                Some((14, e)) => alloc::format!(
                    "ended by vector 14 (error {e:#x}, address {:#x})",
                    FAULT[2].load(Ordering::Relaxed)
                ),
                Some((v, e)) => alloc::format!("ended by vector {v} (error {e:#x})"),
                None => alloc::format!("exited with {exited:?}"),
            };
            let _ = writeln!(
                com1,
                "origin-kernel: user mode: {name}: reported {got_report:?}, {outcome}; kernel carries on{}",
                if right { "" } else { ": user isolation failed" }
            );
        }
    }
    if ok {
        ok &= concurrent(com1, bsp, tsc_hz);
    }
    if ok {
        ok &= by_syscall(com1, bsp, tsc_hz);
    }
    if ok {
        ok &= copies(com1, bsp, tsc_hz);
    }
    if ok {
        ok &= processes(com1, bsp, tsc_hz);
    }
    let _ = vm::unmap(CODE);
    let _ = vm::unmap(STACK);
    sched::yield_until(|| sched::reap() == 0, tsc_hz, 1);
    // SAFETY: both frames came from `alloc_zeroed(layout)` and are no longer user-mapped.
    unsafe {
        dealloc(code, layout);
    }
    // SAFETY: as above.
    unsafe {
        dealloc(stack, layout);
    }
    ok
}

/// Where every process's program, data and stack live (the same addresses in each).
const PROCESS_CODE: u64 = 0x40_0000;
const PROCESS_DATA: u64 = 0x50_0000;
const PROCESS_STACK: u64 = 0x60_0000;

/// Stores `value` at `PROCESS_DATA`, spins long enough to be preempted (so the other
/// process runs and stores its own value at the same virtual address), reads the address
/// back, reports what it read, and exits.
fn isolation_program(value: u8) -> alloc::vec::Vec<u8> {
    let data = (PROCESS_DATA as u32).to_le_bytes();
    let mut p = alloc::vec![0xB8, value, 0, 0, 0]; // mov eax, value
    p.extend_from_slice(&[0x48, 0x89, 0x04, 0x25]); // mov [data], rax
    p.extend_from_slice(&data);
    p.push(0xB9); // mov ecx, SPIN
    p.extend_from_slice(&SPIN.to_le_bytes());
    p.extend_from_slice(&[0xFF, 0xC9, 0x75, 0xFC]); // 1: dec ecx; jnz 1b
    p.extend_from_slice(&[0x48, 0x8B, 0x3C, 0x25]); // mov rdi, [data]
    p.extend_from_slice(&data);
    p.extend_from_slice(&[0xB8, 1, 0, 0, 0, 0xCD, 0x80]); // report
    p.extend_from_slice(&[0x31, 0xFF, 0x31, 0xC0, 0xCD, 0x80, 0xEB, 0xFE]); // exit 0
    p
}

/// A thread of a process: the scheduler has loaded the process's CR3 before running it.
extern "sysv64" fn process_thread(entry: u64) -> ! {
    match sched::current_stack_top() {
        Some(top) => {
            cpu::set_kernel_stack(top);
            // SAFETY: `entry` and the stack are user pages of this thread's process, whose
            // address space is live (the scheduler loaded it); RSP0 is this thread's own
            // kernel stack.
            unsafe { cpu::enter_user(entry, PROCESS_STACK + 4096, cpu::cpu_local_base()) }
        }
        None => sched::exit(),
    }
}

/// Two processes run the same program at the same virtual addresses at the same time,
/// each in its own address space: each must read back only its own value, the kernel's
/// tables must not map their pages, and their PML4s must differ.
fn processes(com1: &mut Console, bsp: crate::cpu::CpuIndex, tsc_hz: u64) -> bool {
    let Ok(layout) = Layout::from_size_align(4096, 4096) else {
        return false;
    };
    let mut frames = [ptr::null_mut::<u8>(); 6];
    for f in &mut frames {
        // SAFETY: non-zero size.
        *f = unsafe { alloc_zeroed(layout) };
        if f.is_null() {
            return false;
        }
    }
    let phys = |p: *mut u8| crate::phys::to_phys(p.expose_provenance() as u64);
    let mut procs = [None, None];
    let mut ok = true;
    for (k, value) in [(0usize, 11u8), (1, 22)] {
        let (code, data, stack) = (frames[3 * k], frames[3 * k + 1], frames[3 * k + 2]);
        let bytes = isolation_program(value);
        // SAFETY: the program is under 64 bytes and lands inside the fresh code frame.
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), code, bytes.len()) };
        let Some(mut p) = crate::process::Process::new() else {
            return false;
        };
        ok &= p.map(PROCESS_CODE, phys(code), Access::ReadExecute).is_ok()
            && p.map(PROCESS_DATA, phys(data), Access::ReadWrite).is_ok()
            && p.map(PROCESS_STACK, phys(stack), Access::ReadWrite).is_ok();
        procs[k] = Some(p);
    }
    let roots: [u64; 2] = [
        procs[0].as_ref().map_or(0, |p| p.root()),
        procs[1].as_ref().map_or(0, |p| p.root()),
    ];
    let in_kernel_space = vm::is_mapped(PROCESS_DATA);
    let in_each = procs.iter().flatten().all(|p| p.is_mapped(PROCESS_DATA));
    REPORT_MASK.store(0, Ordering::Relaxed);
    let ended = ENDED.load(Ordering::Acquire);
    if ok {
        for root in roots {
            if sched::spawn_in(bsp, process_thread, PROCESS_CODE, root).is_err() {
                ok = false;
            }
        }
        sched::yield_until(|| ENDED.load(Ordering::Acquire) >= ended + 2, tsc_hz, 10);
    }
    let mask = REPORT_MASK.load(Ordering::Relaxed);
    let right = ok
        && mask == (1 << 11) | (1 << 22)
        && ENDED.load(Ordering::Acquire) == ended + 2
        && !in_kernel_space
        && in_each
        && roots[0] != roots[1];
    let _ = writeln!(
        com1,
        "origin-kernel: processes: PML4 {:#x} and {:#x} ran the same program at {PROCESS_CODE:#x} at once and read back {mask:#x} from {PROCESS_DATA:#x} (own values only: 0x400800); not mapped in the kernel's tables: {}{}",
        roots[0],
        roots[1],
        !in_kernel_space,
        if right {
            ""
        } else {
            ": process isolation failed"
        }
    );
    // Both threads have exited, and this boot flow runs on the kernel's CR3 again, so no
    // processor uses either address space any more.
    for p in procs.into_iter().flatten() {
        p.destroy();
    }
    for f in frames {
        // SAFETY: each frame came from `alloc_zeroed(layout)` and is no longer mapped.
        unsafe { dealloc(f, layout) };
    }
    right
}
