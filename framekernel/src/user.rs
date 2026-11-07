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
use framekernel::serial::Uart16550;

use crate::Com1;
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

pub(crate) fn record_report(value: u64) {
    REPORTED.store(value + 1, Ordering::Relaxed);
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

/// A kernel thread that becomes a user thread at `entry`.
extern "sysv64" fn user_thread(entry: u64) -> ! {
    match sched::current_stack_top() {
        Some(top) => {
            cpu::set_kernel_stack(top);
            // SAFETY: `entry` and the stack are in the user pages `prove` mapped; RSP0 is
            // this thread's own kernel stack, which it leaves for good here.
            unsafe { cpu::enter_user(entry, STACK + 4096, cpu::cpu_local_base()) }
        }
        None => sched::exit(),
    }
}

/// A program's name, the value it reports, and the fault (vector, error code) that must
/// end it, or `None` if it must exit with code 0.
type Case = (&'static str, u64, Option<(u64, u64)>);

/// Runs the three programs and checks each outcome. Returns whether all were right.
pub(crate) fn prove(com1: &mut Uart16550<Com1>, bsp: usize, tsc_hz: u64) -> bool {
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
    let mapped = vm::map_user(CODE, code.expose_provenance() as u64, Access::ReadExecute)
        .and_then(|()| vm::map_user(STACK, stack.expose_provenance() as u64, Access::ReadWrite));
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
