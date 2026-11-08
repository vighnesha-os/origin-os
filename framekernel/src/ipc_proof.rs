//! Boot proof of M4's first slice: real system calls between isolated processes (ADR-0005
//! P1–P5, P8, P9; CLAUDE.md §29, §30, §42).
//!
//! Everything here runs in ring 3, through the `syscall` instruction, in processes with
//! their own address spaces. The kernel only builds the processes, grants them their first
//! handles (the start record comes later) and reads back what they reported:
//! - **ping-pong:** two processes on two processors exchange messages over a channel,
//!   checking every value, blocking in `wait_one` and woken from the other processor.
//! - **hostile calls:** a process makes every bad call the task names and each must return
//!   the right status: bad handles, too few rights, another process's handle value, pointers
//!   into the kernel and into unmapped or read-only memory, a closed peer, a full queue, and
//!   the handle transfers that would break the object graph. Then it goes on working.
//! - **exit:** a process exits, or faults, holding a channel; its handles close, and the
//!   peer, blocked on another processor, wakes and sees `PEER_CLOSED`.
//! - **cancellation:** a thread blocked in `wait_one` on a handle another thread closes
//!   completes with `CANCELED`.
//! - **limits:** quotas stop a process, not the kernel.
//! - **hostile processor state:** GS and RFLAGS.AC set from ring 3 reach no kernel code.
//! - **cleanup:** when all of them have ended, the heap and the frame allocator are where
//!   they started, so nothing a process left behind leaked.
//!
//! The ring-3 programs are written in assembly and carried in the kernel's image as data,
//! like any other payload. The script interpreter runs tables of system calls the kernel
//! writes into each process's memory, so a hostile sequence is a table with the status each
//! call must return beside it.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::global_asm;
use core::fmt::Write;
use core::sync::atomic::Ordering;

use framekernel::paging::{Access, PAGE};
use origin_abi::limits::{DEADLINE_INFINITE, MAX_MESSAGE_BYTES};
use origin_abi::{Error, OK, read_sizes, rights, signals, status_name, syscall as sys};

use crate::console::Console;
use crate::cpu::{self, CpuIndex};
use crate::ipc::{self, Object};
use crate::process::{Ending, Limits, Process};
use crate::{memory, sched, time};

/// Where every proof process's code begins.
const CODE: u64 = 0x40_0000;
/// Its read/write scratch memory.
const DATA: u64 = 0x50_0000;
/// Pages of scratch memory.
const DATA_PAGES: u64 = 4;
/// The script, on read-only pages.
const SCRIPT: u64 = 0x54_0000;
/// Pages of script (64 calls per page).
const SCRIPT_PAGES: u64 = 4;
/// A second read-write region for the 64 KiB message test.
const BIG_OUT: u64 = 0x70_0000;
const BIG_IN: u64 = 0x80_0000;
/// Pages in each big buffer: 64 KiB of message and a page beyond it that stays unmapped,
/// so a miscounted copy faults instead of running on.
const BIG_PAGES: u64 = 16;
/// Stacks of threads 0 and 1: each one page, with no neighbour mapped below it.
const STACK: u64 = 0x60_0000;
const STACK_1: u64 = 0x60_2000;

/// In the scratch memory: each process's handles, one `u32` per slot.
const fn slot_addr(i: u64) -> u64 {
    DATA + 4 * i
}
/// The bytes every test message carries.
const MSG: u64 = DATA + 0x100;
/// Where messages are read to.
const RBUF: u64 = DATA + 0x140;
/// `wait_one`'s output.
const OBS: u64 = DATA + 0x240;
/// `channel_read`'s sizes structure (`size` set to the known 12 before the process starts).
const SIZES: u64 = DATA + 0x248;
/// A sizes structure too small for the kernel to use.
const BAD_SIZES: u64 = DATA + 0x260;
/// A handle list to send.
const LIST: u64 = DATA + 0x300;
/// Handles received.
const RECV: u64 = DATA + 0x400;

/// A test message.
const TEXT: &[u8; 16] = b"origin ipc proof";

// The script interpreter and the hand-written programs. Each is copied from here, as data,
// to a process's code pages at `CODE`: they use only their own stack and fixed user
// addresses, and the kernel executes none of this.
global_asm!(
    r#"
    .pushsection .rodata.ipc_programs,"a",@progbits

    // The interpreter: RDI points at a script, a table of 64-byte steps, each
    // number, flags, a0, a1, a2, a3, a4, a5. Bit i of `flags` set means argument i is
    // the address of a u32 to load (so a handle a call wrote can be used by a later call).
    // After a real call its status is reported, in order.
    .globl ipc_script_start
    .globl ipc_script_end
ipc_script_start:
    mov rbx, rdi
.Lscript_next:
    mov rax, qword ptr [rbx]
    test rax, rax
    js .Lscript_pseudo
    mov r15, qword ptr [rbx + 8]
    mov rdi, qword ptr [rbx + 16]
    test r15b, 1
    jz .Larg1
    mov edi, dword ptr [rdi]
.Larg1:
    mov rsi, qword ptr [rbx + 24]
    test r15b, 2
    jz .Larg2
    mov esi, dword ptr [rsi]
.Larg2:
    mov rdx, qword ptr [rbx + 32]
    test r15b, 4
    jz .Larg3
    mov edx, dword ptr [rdx]
.Larg3:
    mov r10, qword ptr [rbx + 40]
    test r15b, 8
    jz .Larg4
    mov r10d, dword ptr [r10]
.Larg4:
    mov r8, qword ptr [rbx + 48]
    test r15b, 16
    jz .Larg5
    mov r8d, dword ptr [r8]
.Larg5:
    mov r9, qword ptr [rbx + 56]
    test r15b, 32
    jz .Lscript_call
    mov r9d, dword ptr [r9]
.Lscript_call:
    syscall
    mov rdi, rax
    mov eax, {DEBUG_REPORT}
    syscall
.Lscript_step:
    add rbx, 64
    jmp .Lscript_next
.Lscript_pseudo:
    cmp rax, -2
    je .Lscript_mov32
    cmp rax, -3
    je .Lscript_report32
    cmp rax, -4
    je .Lscript_fault
    // -1: the end of the script.
    xor edi, edi
.Lscript_exit:
    mov eax, {THREAD_EXIT}
    syscall
    jmp .Lscript_exit
.Lscript_mov32:
    mov rdi, qword ptr [rbx + 16]
    mov edi, dword ptr [rdi]
    mov rsi, qword ptr [rbx + 24]
    mov dword ptr [rsi], edi
    jmp .Lscript_step
.Lscript_report32:
    mov rdi, qword ptr [rbx + 16]
    mov edi, dword ptr [rdi]
    mov eax, {DEBUG_REPORT}
    syscall
    jmp .Lscript_step
.Lscript_fault:
    ud2
ipc_script_end:

    // Ping: the channel handle at DATA, the round count at DATA + 8. Each round writes the
    // round number, waits, reads the reply and checks it is one more.
    .globl ipc_ping_start
    .globl ipc_ping_end
ipc_ping_start:
    mov r12, qword ptr [{DATA}]
    mov r13, qword ptr [{DATA} + 8]
    xor ebx, ebx
.Lping_loop:
    mov qword ptr [{DATA} + 0x100], rbx
    mov rdi, r12
    mov esi, {DATA} + 0x100
    mov edx, 8
    xor r10d, r10d
    xor r8d, r8d
    mov eax, {CHANNEL_WRITE}
    syscall
    test rax, rax
    jnz .Lping_fail
    mov rdi, r12
    mov esi, {READABLE}
    mov rdx, -1
    mov r10d, {DATA} + 0x240
    mov eax, {WAIT_ONE}
    syscall
    test rax, rax
    jnz .Lping_fail
    mov rdi, r12
    mov esi, {DATA} + 0x140
    mov edx, 16
    xor r10d, r10d
    xor r8d, r8d
    mov r9d, {DATA} + 0x248
    mov eax, {CHANNEL_READ}
    syscall
    test rax, rax
    jnz .Lping_fail
    lea rax, [rbx + 1]
    cmp qword ptr [{DATA} + 0x140], rax
    jne .Lping_wrong
    cmp dword ptr [{DATA} + 0x24C], 8
    jne .Lping_wrong
    inc rbx
    cmp rbx, r13
    jne .Lping_loop
    mov rdi, rbx
    mov eax, {DEBUG_REPORT}
    syscall
    xor edi, edi
.Lping_exit:
    mov eax, {THREAD_EXIT}
    syscall
    jmp .Lping_exit
.Lping_wrong:
    mov rdi, -1
    jmp .Lping_report
.Lping_fail:
    mov rdi, rax
.Lping_report:
    mov eax, {DEBUG_REPORT}
    syscall
    mov edi, 1
    jmp .Lping_exit
ipc_ping_end:

    // Pong: waits for each round number, checks it, and answers one more.
    .globl ipc_pong_start
    .globl ipc_pong_end
ipc_pong_start:
    mov r12, qword ptr [{DATA}]
    mov r13, qword ptr [{DATA} + 8]
    xor ebx, ebx
.Lpong_loop:
    mov rdi, r12
    mov esi, {READABLE}
    mov rdx, -1
    mov r10d, {DATA} + 0x240
    mov eax, {WAIT_ONE}
    syscall
    test rax, rax
    jnz .Lpong_fail
    mov rdi, r12
    mov esi, {DATA} + 0x140
    mov edx, 16
    xor r10d, r10d
    xor r8d, r8d
    mov r9d, {DATA} + 0x248
    mov eax, {CHANNEL_READ}
    syscall
    test rax, rax
    jnz .Lpong_fail
    mov rax, qword ptr [{DATA} + 0x140]
    cmp rax, rbx
    jne .Lpong_wrong
    inc rax
    mov qword ptr [{DATA} + 0x100], rax
    mov rdi, r12
    mov esi, {DATA} + 0x100
    mov edx, 8
    xor r10d, r10d
    xor r8d, r8d
    mov eax, {CHANNEL_WRITE}
    syscall
    test rax, rax
    jnz .Lpong_fail
    inc rbx
    cmp rbx, r13
    jne .Lpong_loop
    mov rdi, rbx
    mov eax, {DEBUG_REPORT}
    syscall
    xor edi, edi
.Lpong_exit:
    mov eax, {THREAD_EXIT}
    syscall
    jmp .Lpong_exit
.Lpong_wrong:
    mov rdi, -1
    jmp .Lpong_report
.Lpong_fail:
    mov rdi, rax
.Lpong_report:
    mov eax, {DEBUG_REPORT}
    syscall
    mov edi, 1
    jmp .Lpong_exit
ipc_pong_end:

    // The register convention (P4): every register but RAX, RCX and R11 survives a call.
    .globl ipc_regs_start
    .globl ipc_regs_end
ipc_regs_start:
    mov rbx, 0x1111111111111111
    mov rbp, 0x2222222222222222
    mov rdi, 0x3333333333333333
    mov rsi, 0x4444444444444444
    mov rdx, 0x5555555555555555
    mov r8, 0x6666666666666666
    mov r9, 0x7777777777777777
    mov r10, 0x8888888888888888
    mov r12, 0x9999999999999999
    mov r13, 0xAAAAAAAAAAAAAAAA
    mov r14, 0xBBBBBBBBBBBBBBBB
    mov r15, 0xCCCCCCCCCCCCCCCC
    mov eax, {UNKNOWN_CALL}
    syscall
    cmp rax, {NOT_SUPPORTED}
    jne .Lregs_wrong
    mov rcx, 0x1111111111111111
    cmp rbx, rcx
    jne .Lregs_wrong
    mov rcx, 0x2222222222222222
    cmp rbp, rcx
    jne .Lregs_wrong
    mov rcx, 0x3333333333333333
    cmp rdi, rcx
    jne .Lregs_wrong
    mov rcx, 0x4444444444444444
    cmp rsi, rcx
    jne .Lregs_wrong
    mov rcx, 0x5555555555555555
    cmp rdx, rcx
    jne .Lregs_wrong
    mov rcx, 0x6666666666666666
    cmp r8, rcx
    jne .Lregs_wrong
    mov rcx, 0x7777777777777777
    cmp r9, rcx
    jne .Lregs_wrong
    mov rcx, 0x8888888888888888
    cmp r10, rcx
    jne .Lregs_wrong
    mov rcx, 0x9999999999999999
    cmp r12, rcx
    jne .Lregs_wrong
    mov rcx, 0xAAAAAAAAAAAAAAAA
    cmp r13, rcx
    jne .Lregs_wrong
    mov rcx, 0xBBBBBBBBBBBBBBBB
    cmp r14, rcx
    jne .Lregs_wrong
    mov rcx, 0xCCCCCCCCCCCCCCCC
    cmp r15, rcx
    jne .Lregs_wrong
    cmp rsp, {STACK_TOP}
    jne .Lregs_wrong
    xor edi, edi
    jmp .Lregs_report
.Lregs_wrong:
    mov edi, 1
.Lregs_report:
    mov eax, {DEBUG_REPORT}
    syscall
    xor edi, edi
.Lregs_exit:
    mov eax, {THREAD_EXIT}
    syscall
    jmp .Lregs_exit
ipc_regs_end:

    // A program that loads a null selector into GS, makes a call and runs through timer
    // interrupts, then reports the selector is still what it loaded.
    .globl ipc_gs_start
    .globl ipc_gs_end
ipc_gs_start:
    xor eax, eax
    mov gs, ax
    mov edi, 1
    mov eax, {DEBUG_REPORT}
    syscall
    mov ecx, {SPIN}
.Lgs_spin:
    dec ecx
    jnz .Lgs_spin
    mov eax, gs
    movzx edi, ax
    mov eax, {DEBUG_REPORT}
    syscall
    xor edi, edi
.Lgs_exit:
    mov eax, {THREAD_EXIT}
    syscall
    jmp .Lgs_exit
ipc_gs_end:

    // A program that sets RFLAGS.AC and runs through timer interrupts and a system call
    // with it set.
    .globl ipc_ac_start
    .globl ipc_ac_end
ipc_ac_start:
    pushfq
    or dword ptr [rsp], 0x40000
    popfq
    mov ecx, {SPIN}
.Lac_spin:
    dec ecx
    jnz .Lac_spin
    pushfq
    pop rdi
    mov eax, {DEBUG_REPORT}
    syscall
    xor edi, edi
.Lac_exit:
    mov eax, {THREAD_EXIT}
    syscall
    jmp .Lac_exit
ipc_ac_end:

    // Without the developer calls: both must come back NOT_SUPPORTED, and it exits with 0
    // if the first did.
    .globl ipc_nodebug_start
    .globl ipc_nodebug_end
ipc_nodebug_start:
    xor edi, edi
    xor esi, esi
    mov eax, {DEBUG_WRITE}
    syscall
    cmp rax, {NOT_SUPPORTED}
    setne al
    movzx edi, al
    mov eax, {DEBUG_REPORT}
    syscall
.Lnodebug_exit:
    mov eax, {THREAD_EXIT}
    syscall
    jmp .Lnodebug_exit
ipc_nodebug_end:

    // `sidt` from ring 3: with UMIP on it is a #GP, and the kernel's IDT address stays secret.
    .globl ipc_sidt_start
    .globl ipc_sidt_end
ipc_sidt_start:
    sidt [{DATA} + 0x100]
    mov edi, 1
    mov eax, {DEBUG_REPORT}
    syscall
    xor edi, edi
.Lsidt_exit:
    mov eax, {THREAD_EXIT}
    syscall
    jmp .Lsidt_exit
ipc_sidt_end:

    .popsection
"#,
    DATA = const DATA,
    DEBUG_REPORT = const sys::DEBUG_REPORT,
    THREAD_EXIT = const sys::THREAD_EXIT,
    DEBUG_WRITE = const sys::DEBUG_WRITE,
    CHANNEL_WRITE = const sys::CHANNEL_WRITE,
    CHANNEL_READ = const sys::CHANNEL_READ,
    WAIT_ONE = const sys::WAIT_ONE,
    READABLE = const signals::READABLE,
    UNKNOWN_CALL = const 999u64,
    NOT_SUPPORTED = const Error::NotSupported.raw(),
    STACK_TOP = const STACK + PAGE,
    SPIN = const 30_000_000u32,
);

// SAFETY: these symbols are defined by the `global_asm!` above, in the kernel's read-only
// image; only their addresses are used.
unsafe extern "C" {
    static ipc_script_start: u8;
    static ipc_script_end: u8;
    static ipc_ping_start: u8;
    static ipc_ping_end: u8;
    static ipc_pong_start: u8;
    static ipc_pong_end: u8;
    static ipc_regs_start: u8;
    static ipc_regs_end: u8;
    static ipc_gs_start: u8;
    static ipc_gs_end: u8;
    static ipc_ac_start: u8;
    static ipc_ac_end: u8;
    static ipc_nodebug_start: u8;
    static ipc_nodebug_end: u8;
    static ipc_sidt_start: u8;
    static ipc_sidt_end: u8;
}

/// The bytes of one program in the image.
fn program(start: *const u8, end: *const u8) -> &'static [u8] {
    // SAFETY: `start..end` delimit a program in the kernel's read-only image, which is
    // mapped and unchanging for as long as the kernel runs; `end` is not below `start`.
    unsafe { core::slice::from_raw_parts(start, end.addr().saturating_sub(start.addr())) }
}

fn script_program() -> &'static [u8] {
    program(&raw const ipc_script_start, &raw const ipc_script_end)
}

// ------------------------------------------------------------------ scripted calls

/// An argument of a scripted call.
#[derive(Clone, Copy)]
enum Arg {
    /// This value.
    Imm(u64),
    /// The `u32` at this address, loaded by the interpreter when the call runs.
    Handle(u64),
}
use Arg::{Handle as Hd, Imm};

/// What a step must do.
#[derive(Clone, Copy)]
enum Want {
    /// Return this status.
    Status(i64),
    /// Report this value (a `report32` step).
    Value(u64),
    /// Nothing is reported.
    Nothing,
}

/// One entry of a script.
#[derive(Clone, Copy)]
struct Step {
    /// A label to print, empty for a setup step.
    name: &'static str,
    number: u64,
    args: [Arg; 6],
    want: Want,
}

/// The status of every step is printed by name, setup steps only when they go wrong.
fn step(name: &'static str, number: u64, args: &[Arg], want: i64) -> Step {
    let mut full = [Imm(0); 6];
    for (slot, arg) in full.iter_mut().zip(args) {
        *slot = *arg;
    }
    Step {
        name,
        number,
        args: full,
        want: Want::Status(want),
    }
}

/// Handle slot `i`'s value, as a call argument.
const fn h(i: u64) -> Arg {
    Hd(slot_addr(i))
}

fn create(first: u64) -> Step {
    step("", sys::CHANNEL_CREATE, &[Imm(slot_addr(first))], OK)
}

fn close(name: &'static str, x: Arg, want: i64) -> Step {
    step(name, sys::HANDLE_CLOSE, &[x], want)
}

fn dup(name: &'static str, x: Arg, r: u32, out: u64, want: i64) -> Step {
    step(
        name,
        sys::HANDLE_DUPLICATE,
        &[x, Imm(u64::from(r)), Imm(slot_addr(out))],
        want,
    )
}

fn write(name: &'static str, x: Arg, bytes: u64, len: u64, want: i64) -> Step {
    step(
        name,
        sys::CHANNEL_WRITE,
        &[x, Imm(bytes), Imm(len), Imm(0), Imm(0)],
        want,
    )
}

fn send(name: &'static str, x: Arg, count: u64, want: i64) -> Step {
    step(
        name,
        sys::CHANNEL_WRITE,
        &[x, Imm(MSG), Imm(4), Imm(LIST), Imm(count)],
        want,
    )
}

fn read(name: &'static str, x: Arg, bytes: u64, cap: u64, want: i64) -> Step {
    step(
        name,
        sys::CHANNEL_READ,
        &[x, Imm(bytes), Imm(cap), Imm(0), Imm(0), Imm(SIZES)],
        want,
    )
}

fn read_handles(name: &'static str, x: Arg, want: i64) -> Step {
    step(
        name,
        sys::CHANNEL_READ,
        &[x, Imm(RBUF), Imm(16), Imm(RECV), Imm(4), Imm(SIZES)],
        want,
    )
}

fn wait(name: &'static str, x: Arg, mask: u32, deadline: u64, want: i64) -> Step {
    step(
        name,
        sys::WAIT_ONE,
        &[x, Imm(u64::from(mask)), Imm(deadline), Imm(OBS)],
        want,
    )
}

/// The `u32` at `addr` must be `value` (reported to the kernel, which compares).
const fn value_at(name: &'static str, addr: u64, value: u64) -> Step {
    Step {
        name,
        number: PSEUDO_REPORT32,
        args: [Imm(addr), Imm(0), Imm(0), Imm(0), Imm(0), Imm(0)],
        want: Want::Value(value),
    }
}

/// Copies the `u32` at `from` to `to`, in ring 3, reporting nothing.
const fn copy32(from: u64, to: u64) -> Step {
    Step {
        name: "",
        number: PSEUDO_MOV32,
        args: [Imm(from), Imm(to), Imm(0), Imm(0), Imm(0), Imm(0)],
        want: Want::Nothing,
    }
}

const PSEUDO_MOV32: u64 = -2i64 as u64;
const PSEUDO_REPORT32: u64 = -3i64 as u64;
const PSEUDO_FAULT: u64 = -4i64 as u64;

/// Executes `ud2`: a #UD ends the thread.
const fn fault() -> Step {
    Step {
        name: "",
        number: PSEUDO_FAULT,
        args: [Imm(0); 6],
        want: Want::Nothing,
    }
}

/// A named group of steps, printed on one line.
struct Group {
    title: &'static str,
    steps: Vec<Step>,
}

// ------------------------------------------------------------------ building processes

/// A process being put together.
struct Image {
    process: Process,
    /// Every page mapped so far: its user address and frame.
    pages: Vec<(u64, u64)>,
}

impl Image {
    /// A process with `program` at [`CODE`], its data pages, and one stack page; system
    /// calls accepted from its code only.
    fn new(limits: Limits, code: &[u8]) -> Option<Self> {
        let code_pages = code.len().div_ceil(PAGE as usize).max(1) as u64;
        Self::with_origin(limits, code, (CODE, CODE + code_pages * PAGE))
    }

    /// As [`Image::new`], but system calls are accepted only from `origin`.
    fn with_origin(limits: Limits, code: &[u8], origin: (u64, u64)) -> Option<Self> {
        let code_pages = code.len().div_ceil(PAGE as usize).max(1) as u64;
        let mut image = Self {
            process: Process::new(limits, origin)?,
            pages: Vec::new(),
        };
        image.map(CODE, code_pages, Access::ReadExecute)?;
        image.write(CODE, code)?;
        image.map(DATA, DATA_PAGES, Access::ReadWrite)?;
        image.map(STACK, 1, Access::ReadWrite)?;
        Some(image)
    }

    fn map(&mut self, virt: u64, pages: u64, access: Access) -> Option<()> {
        for k in 0..pages {
            let at = virt + k * PAGE;
            let frame = self.process.map_page(at, access).ok()?;
            self.pages.push((at, frame));
        }
        Some(())
    }

    /// Writes `bytes` into the process's memory at `virt`.
    fn write(&self, virt: u64, bytes: &[u8]) -> Option<()> {
        let mut at = virt;
        let mut rest = bytes;
        while !rest.is_empty() {
            let page = at & !(PAGE - 1);
            let offset = (at - page) as usize;
            let chunk = rest.len().min(PAGE as usize - offset);
            let (head, tail) = rest.split_at_checked(chunk)?;
            let &(_, frame) = self.pages.iter().find(|(v, _)| *v == page)?;
            if !self.process.write_frame(frame, offset, head) {
                return None;
            }
            at += chunk as u64;
            rest = tail;
        }
        Some(())
    }

    fn write_u64(&self, virt: u64, value: u64) -> Option<()> {
        self.write(virt, &value.to_le_bytes())
    }

    /// The scratch memory every scripted process starts with.
    fn scratch(&self) -> Option<()> {
        self.write(MSG, TEXT)?;
        self.write(SIZES, &read_sizes::SIZE.to_le_bytes())?;
        self.write(BAD_SIZES, &8u32.to_le_bytes())
    }

    /// Maps the script's pages (read-only) and writes `steps` into them.
    fn script(&mut self, steps: &[Step]) -> Option<()> {
        self.map(SCRIPT, SCRIPT_PAGES, Access::Read)?;
        self.write_script(SCRIPT, steps)
    }

    fn write_script(&self, at: u64, steps: &[Step]) -> Option<()> {
        let mut encoded = Vec::with_capacity((steps.len() + 1) * 64);
        for s in steps {
            let mut flags = 0u64;
            let mut args = [0u64; 6];
            for (i, (value, arg)) in args.iter_mut().zip(s.args).enumerate() {
                match arg {
                    Imm(v) => *value = v,
                    Hd(addr) => {
                        *value = addr;
                        flags |= 1 << i;
                    }
                }
            }
            encoded.extend_from_slice(&s.number.to_le_bytes());
            encoded.extend_from_slice(&flags.to_le_bytes());
            for a in args {
                encoded.extend_from_slice(&a.to_le_bytes());
            }
        }
        encoded.extend_from_slice(&u64::MAX.to_le_bytes());
        encoded.extend_from_slice(&[0; 56]);
        self.write(at, &encoded)
    }

    fn finish(self) -> Arc<Process> {
        Arc::new(self.process)
    }
}

/// What a test process may hold.
const LIMITS: Limits = Limits {
    handles: 64,
    channels: 32,
    debug_calls: true,
};

/// Waits for `process`'s last thread to end.
fn await_end(process: &Process, tsc_hz: u64, seconds: u64) -> bool {
    sched::yield_until(|| process.ending().is_some(), tsc_hz, seconds);
    process.ending().is_some()
}

/// Whether `reports` are what `steps` must produce, printing each group on its own line.
/// Setup steps are checked too, and named only when one goes wrong.
fn check_groups(com1: &mut Console, groups: &[Group], reports: &[u64]) -> bool {
    let mut at = 0;
    let mut ok = true;
    for group in groups {
        let mut text = alloc::string::String::new();
        let mut group_ok = true;
        for s in &group.steps {
            let got = match s.want {
                Want::Nothing => continue,
                _ => reports.get(at).copied(),
            };
            at += 1;
            let shown = match (s.want, got) {
                (Want::Status(_), Some(v)) => alloc::string::String::from(status_name(v as i64)),
                (Want::Value(_), Some(v)) => alloc::format!("{v:#x}"),
                (_, None) => alloc::string::String::from("nothing"),
                (Want::Nothing, _) => continue,
            };
            let right = match s.want {
                Want::Status(w) => got == Some(w as u64),
                Want::Value(w) => got == Some(w),
                Want::Nothing => true,
            };
            group_ok &= right;
            if !s.name.is_empty() {
                let _ = write!(
                    text,
                    "{}{} -> {}",
                    if text.is_empty() { "" } else { ", " },
                    s.name,
                    shown
                );
            } else if !right {
                let _ = writeln!(
                    com1,
                    "origin-kernel: ipc: {}: a setup step {} returned {shown}: ipc failed",
                    group.title, s.number as i64
                );
            }
        }
        ok &= group_ok;
        let _ = writeln!(
            com1,
            "origin-kernel: ipc: {}: {text}{}",
            group.title,
            if group_ok { "" } else { ": ipc failed" }
        );
    }
    ok
}

// ------------------------------------------------------------------ the proofs

/// Round trips of the ping-pong.
const ROUNDS: u64 = 1000;

/// Two processes exchange [`ROUNDS`] messages over a channel, each on its own processor,
/// each blocking in `wait_one` for the other. Every message's content is checked in ring 3.
fn ping_pong(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let ping_code = program(&raw const ipc_ping_start, &raw const ipc_ping_end);
    let pong_code = program(&raw const ipc_pong_start, &raw const ipc_pong_end);
    let (Some(ping_image), Some(pong_image)) =
        (Image::new(LIMITS, ping_code), Image::new(LIMITS, pong_code))
    else {
        return false;
    };
    let (Some(ping), Some(pong)) = (
        build_pinger(ping_image, ROUNDS),
        build_pinger(pong_image, ROUNDS),
    ) else {
        return false;
    };
    let Ok((a, b)) = ipc::create_channel(&ping.account) else {
        return false;
    };
    let granted = ping
        .grant(Object::Channel(a), rights::CHANNEL)
        .and_then(|ha| {
            pong.grant(Object::Channel(b), rights::CHANNEL)
                .map(|hb| (ha, hb))
        });
    let Ok((ha, hb)) = granted else {
        return false;
    };
    // The kernel told each process its handle through its data page.
    let told = write_param(&ping, 0, u64::from(ha)) && write_param(&pong, 0, u64::from(hb));
    let wakes_before = ipc::cross_cpu_wakes();
    let start = cpu::rdtsc();
    if !told
        || Process::start_thread(&pong, far, CODE, STACK + PAGE, 0).is_err()
        || Process::start_thread(&ping, bsp, CODE, STACK + PAGE, 0).is_err()
    {
        return false;
    }
    let ended = await_end(&ping, tsc_hz, 20) && await_end(&pong, tsc_hz, 5);
    let cycles = cpu::rdtsc().wrapping_sub(start);
    let micros = cycles / (tsc_hz / 1_000_000).max(1);
    let (ping_reports, pong_reports) = (ping.reports(), pong.reports());
    let wakes = ipc::cross_cpu_wakes().wrapping_sub(wakes_before);
    let blocked = (ping.blocked(), pong.blocked());
    let right = ended
        && ping.ending() == Some(Ending::Exited(0))
        && pong.ending() == Some(Ending::Exited(0))
        && ping_reports == [ROUNDS]
        && pong_reports == [ROUNDS]
        && blocked.0 + blocked.1 > 0
        && (far == bsp || wakes > 0);
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: ping-pong: two ring-3 processes on {} and {} exchanged {} messages each way, every one checked in ring 3; the processes blocked in wait_one {} and {} times, and {} wake-ups crossed processors{}",
        bsp.named(),
        far.named(),
        ROUNDS,
        blocked.0,
        blocked.1,
        wakes,
        if right { "" } else { ": ipc failed" }
    );
    if right {
        let _ = writeln!(
            com1,
            "origin-kernel: ipc: ping-pong round trip: {} ns between two processors",
            micros * 1000 / ROUNDS.max(1)
        );
    }
    right
}

/// The process of `image`, its round count written (handles are written after the grants).
fn build_pinger(image: Image, rounds: u64) -> Option<Arc<Process>> {
    image.write_u64(DATA + 8, rounds)?;
    image.write(SIZES, &read_sizes::SIZE.to_le_bytes())?;
    Some(image.finish())
}

/// Writes `value` to the `u64` at `offset` of the process's data page, before it starts.
fn write_param(process: &Process, offset: usize, value: u64) -> bool {
    let frame = process.translate(DATA).map(|t| t.phys);
    frame.is_some_and(|f| process.write_frame(f & !(PAGE - 1), offset, &value.to_le_bytes()))
}

/// The hostile script. `kernel` is an address of kernel data; `foreign` a handle value that
/// is live in another process's table, and not in this one's.
fn hostile_groups(kernel: u64, foreign: u64) -> Vec<Group> {
    const UNMAPPED: u64 = 0x0000_2000_0000_0000;
    const TOP_PAGE: u64 = 0x0000_7FFF_FFFF_F000;
    let fault = Error::Fault.raw();
    let bad = Error::BadHandle.raw();
    let denied = Error::AccessDenied.raw();
    let invalid = Error::InvalidArgs.raw();
    let wait_status = Error::ShouldWait.raw();
    let timed_out = Error::TimedOut.raw();
    let peer_closed = Error::PeerClosed.raw();
    let full = rights::CHANNEL;
    let (a, b) = (h(4), h(5));
    let data_end = DATA + DATA_PAGES * PAGE;
    alloc::vec![
        Group {
            title: "bad handles",
            steps: alloc::vec![
                close("handle 0", Imm(0), bad),
                write("never issued", Imm(0x7FFF_0042), MSG, 8, bad),
                read("bits above 32", Imm(0x1_0001_0001), RBUF, 16, bad),
                wait(
                    "wait on a bad handle",
                    Imm(0x7FFF_0042),
                    signals::READABLE,
                    0,
                    bad
                ),
                create(0),
                close("", h(0), OK),
                write("closed", h(0), MSG, 8, bad),
                create(2),
                write("stale, slot reused", h(0), MSG, 8, bad),
                write("another process's", Imm(foreign), MSG, 8, bad),
                wait(
                    "wait on another process's",
                    Imm(foreign),
                    signals::READABLE,
                    0,
                    bad
                ),
            ],
        },
        Group {
            title: "too few rights",
            steps: alloc::vec![
                create(4),
                dup("", a, rights::READ | rights::WAIT, 6, OK),
                dup("", a, rights::WRITE, 7, OK),
                dup("", a, rights::READ | rights::WRITE | rights::WAIT, 8, OK),
                write("write without WRITE", h(6), MSG, 8, denied),
                read("read without READ", h(7), RBUF, 16, denied),
                wait("wait without WAIT", h(7), signals::READABLE, 0, denied),
                dup("duplicate without DUPLICATE", h(8), rights::READ, 9, denied),
                copy32(slot_addr(6), LIST),
                send("send without TRANSFER", a, 1, denied),
            ],
        },
        Group {
            title: "rights only shrink",
            steps: alloc::vec![
                dup(
                    "",
                    a,
                    rights::READ | rights::WAIT | rights::DUPLICATE,
                    10,
                    OK
                ),
                dup(
                    "adding WRITE",
                    h(10),
                    rights::READ | rights::WRITE,
                    11,
                    invalid
                ),
                dup("an undefined right", a, 1 << 30, 11, invalid),
                dup(
                    "dropping DUPLICATE",
                    h(10),
                    rights::READ | rights::WAIT,
                    11,
                    OK
                ),
                write("the copy cannot write", h(11), MSG, 8, denied),
                read("the copy can read", h(11), RBUF, 16, wait_status),
            ],
        },
        Group {
            title: "bad pointers",
            steps: alloc::vec![
                write("write from the kernel", a, kernel, 8, fault),
                write("from unmapped", a, UNMAPPED, 8, fault),
                write("across the end of mapped", a, data_end - 4, 8, fault),
                write("from address 0", a, 0, 8, fault),
                write("wrapping past the top", a, u64::MAX - 7, 16, fault),
                write("from the top page", a, TOP_PAGE, 8, fault),
                read("read into the kernel", b, kernel, 16, fault),
                read("into unmapped", b, UNMAPPED, 16, fault),
                read("into code", b, CODE, 16, fault),
                read("into read-only memory", b, SCRIPT, 16, fault),
                step(
                    "sizes in the kernel",
                    sys::CHANNEL_READ,
                    &[b, Imm(RBUF), Imm(16), Imm(0), Imm(0), Imm(kernel)],
                    fault,
                ),
                step(
                    "create out to the kernel",
                    sys::CHANNEL_CREATE,
                    &[Imm(kernel)],
                    fault
                ),
                step(
                    "create out to read-only",
                    sys::CHANNEL_CREATE,
                    &[Imm(SCRIPT)],
                    fault
                ),
                step(
                    "duplicate out to the kernel",
                    sys::HANDLE_DUPLICATE,
                    &[a, Imm(rights::READ.into()), Imm(kernel)],
                    fault
                ),
                step(
                    "duplicate out to read-only",
                    sys::HANDLE_DUPLICATE,
                    &[a, Imm(rights::READ.into()), Imm(CODE)],
                    fault
                ),
                step(
                    "wait out to read-only",
                    sys::WAIT_ONE,
                    &[a, Imm(signals::WRITABLE.into()), Imm(0), Imm(SCRIPT)],
                    fault
                ),
                step(
                    "wait out to the kernel",
                    sys::WAIT_ONE,
                    &[a, Imm(signals::WRITABLE.into()), Imm(0), Imm(kernel)],
                    fault
                ),
                step(
                    "handle list in the kernel",
                    sys::CHANNEL_WRITE,
                    &[a, Imm(MSG), Imm(4), Imm(kernel), Imm(1)],
                    fault
                ),
                step(
                    "zero length names no memory",
                    sys::CHANNEL_WRITE,
                    &[a, Imm(UNMAPPED), Imm(0), Imm(UNMAPPED), Imm(0)],
                    OK
                ),
                read("", b, RBUF, 16, OK),
                value_at(
                    "the empty message arrived",
                    SIZES + read_sizes::BYTES_AT as u64,
                    0
                ),
            ],
        },
        Group {
            title: "sizes and numbers",
            steps: alloc::vec![
                write(
                    "one byte too many",
                    a,
                    MSG,
                    MAX_MESSAGE_BYTES as u64 + 1,
                    invalid
                ),
                step(
                    "one handle too many",
                    sys::CHANNEL_WRITE,
                    &[a, Imm(MSG), Imm(4), Imm(LIST), Imm(65)],
                    invalid
                ),
                wait("no signals", a, 0, 0, invalid),
                wait("an undefined signal", a, 1 << 31, 0, invalid),
                step(
                    "a sizes structure too small",
                    sys::CHANNEL_READ,
                    &[b, Imm(RBUF), Imm(16), Imm(0), Imm(0), Imm(BAD_SIZES)],
                    invalid
                ),
                step(
                    "an unknown call number",
                    999,
                    &[],
                    Error::NotSupported.raw()
                ),
                step(
                    "an object type not here yet",
                    sys::EVENT_CREATE,
                    &[Imm(slot_addr(9))],
                    Error::NotSupported.raw()
                ),
            ],
        },
        Group {
            title: "queues",
            steps: alloc::vec![
                create(12),
                read("empty", h(13), RBUF, 16, wait_status),
                wait(
                    "nothing by deadline 0",
                    h(13),
                    signals::READABLE,
                    0,
                    timed_out
                ),
                write("written", h(12), MSG, 16, OK),
                wait("readable", h(13), signals::READABLE, 0, OK),
                value_at(
                    "observed",
                    OBS,
                    u64::from(signals::READABLE | signals::WRITABLE)
                ),
                read("4-byte buffer", h(13), RBUF, 4, Error::BufferTooSmall.raw()),
                value_at("it needs", SIZES + read_sizes::BYTES_AT as u64, 16),
                read("still there", h(13), RBUF, 16, OK),
                value_at("received", SIZES + read_sizes::BYTES_AT as u64, 16),
            ],
        },
        Group {
            title: "a full queue",
            steps: full_queue_steps(timed_out, wait_status),
        },
        Group {
            title: "a closed peer",
            steps: alloc::vec![
                create(14),
                write("", h(14), MSG, 16, OK),
                close("", h(14), OK),
                wait("sees PEER_CLOSED", h(15), signals::PEER_CLOSED, 0, OK),
                value_at(
                    "observed",
                    OBS,
                    u64::from(signals::READABLE | signals::PEER_CLOSED)
                ),
                write("write", h(15), MSG, 8, peer_closed),
                read("the message sent before the close", h(15), RBUF, 16, OK),
                read("read past the close", h(15), RBUF, 16, peer_closed),
                wait(
                    "READABLE will never rise",
                    h(15),
                    signals::READABLE,
                    0,
                    timed_out
                ),
            ],
        },
        Group {
            title: "handle transfer",
            steps: alloc::vec![
                create(16),
                create(18),
                copy32(slot_addr(18), LIST),
                send("send", h(16), 1, OK),
                write("the sender's handle is gone", h(18), MSG, 4, bad),
                read_handles("received", h(17), OK),
                value_at("with one handle", SIZES + read_sizes::HANDLES_AT as u64, 1),
                write("the moved handle writes", Hd(RECV), MSG, 4, OK),
                read("and the other end reads", h(19), RBUF, 16, OK),
                dup("it kept its rights", Hd(RECV), full, 20, OK),
                close("", h(20), OK),
            ],
        },
        Group {
            title: "transfers that would break the graph",
            steps: alloc::vec![
                copy32(slot_addr(16), LIST),
                send("an end through its own channel", h(16), 1, invalid),
                copy32(slot_addr(17), LIST),
                send("the peer end through its own channel", h(16), 1, invalid),
                create(22),
                create(24),
                copy32(slot_addr(19), LIST),
                copy32(slot_addr(19), LIST + 4),
                send("the same handle twice", h(16), 2, invalid),
                dup("", h(19), full, 21, OK),
                copy32(slot_addr(19), LIST),
                send("an end with another handle", h(16), 1, invalid),
                close("", h(21), OK),
                send("that end, alone again", h(16), 1, OK),
                read_handles("", h(17), OK),
                copy32(slot_addr(24), LIST),
                send("into the first channel", h(23), 1, OK),
                copy32(slot_addr(22), LIST),
                send("a cycle through two queues", h(25), 1, invalid),
                copy32(slot_addr(0), LIST),
                send("a stale handle in the list", h(16), 1, bad),
            ],
        },
    ]
}

/// Sixteen writes fill a queue; the seventeenth is refused until a read makes room.
fn full_queue_steps(timed_out: i64, wait_status: i64) -> Vec<Step> {
    let mut steps = alloc::vec![create(26)];
    for _ in 0..framekernel::channel::QUEUE_DEPTH {
        steps.push(write("", h(26), MSG, 16, OK));
    }
    steps.push(write(
        "the write after the last",
        h(26),
        MSG,
        16,
        wait_status,
    ));
    steps.push(wait("not writable", h(26), signals::WRITABLE, 0, timed_out));
    steps.push(read("", h(27), RBUF, 16, OK));
    steps.push(wait(
        "writable again after a read",
        h(26),
        signals::WRITABLE,
        0,
        OK,
    ));
    steps
}

/// A process makes every bad call, each checked against the status it must return; the
/// kernel is undisturbed if the rest of the boot runs.
fn hostile(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    // Another process with a live handle whose value is not live in the hostile process:
    // slot 40, which the hostile process's table never reaches.
    let Some(other) = Process::new(LIMITS, (CODE, CODE + PAGE)) else {
        return false;
    };
    let Ok((end, peer)) = ipc::create_channel(&other.account) else {
        return false;
    };
    drop(peer);
    let mut foreign = 0;
    for _ in 0..=40 {
        match other.grant(Object::Channel(end.clone()), rights::CHANNEL) {
            Ok(value) => foreign = value,
            Err(_) => return false,
        }
    }
    drop(end);

    let kernel = core::ptr::from_ref(&ROUNDS_MARKER).addr() as u64;
    let groups = hostile_groups(kernel, u64::from(foreign));
    let steps: Vec<Step> = groups
        .iter()
        .flat_map(|g| g.steps.iter().copied())
        .collect();
    let Some(mut image) = Image::new(LIMITS, script_program()) else {
        return false;
    };
    if image.scratch().is_none() || image.script(&steps).is_none() {
        return false;
    }
    let process = image.finish();
    if Process::start_thread(&process, bsp, CODE, STACK + PAGE, SCRIPT).is_err() {
        return false;
    }
    let ended = await_end(&process, tsc_hz, 10);
    let reports = process.reports();
    let groups_ok = check_groups(com1, &groups, &reports);
    let exit_ok = process.ending() == Some(Ending::Exited(0));
    // The process lived through all of it and ended by its own exit; the other process's
    // handle was never reachable from it.
    let right = ended && groups_ok && exit_ok && foreign & 0xFFFF == 41;
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: every hostile call returned its status and the process exited by itself ({} calls); user-copy faults recovered: {}{}",
        reports.len(),
        cpu::user_copy_faults(),
        if right { "" } else { ": ipc failed" }
    );
    // It never ran: only its handle table mattered. It is dropped with its handles.
    drop(other);
    right
}

/// What the "kernel pointer" argument points at.
static ROUNDS_MARKER: u64 = 0;

/// A peer blocked in `wait_one` on another processor sees `PEER_CLOSED` when the process at
/// the far end ends, by exiting or by faulting, without ever closing its handle.
fn exit_closes(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let mut ok = true;
    for (label, exit_step) in [("exits", None), ("faults", Some(fault()))] {
        let (Some(mut waiter), Some(mut ender)) = (
            Image::new(LIMITS, script_program()),
            Image::new(LIMITS, script_program()),
        ) else {
            return false;
        };
        let Ok((end_a, end_b)) = ipc::create_channel(&ender.process.account) else {
            return false;
        };
        let (Ok(_), Ok(hb)) = (
            ender.process.grant(Object::Channel(end_a), rights::CHANNEL),
            waiter
                .process
                .grant(Object::Channel(end_b), rights::CHANNEL),
        ) else {
            return false;
        };
        let held = Imm(u64::from(hb));
        let waiter_steps = alloc::vec![
            wait(
                "woken",
                held,
                signals::PEER_CLOSED | signals::READABLE,
                DEADLINE_INFINITE,
                OK,
            ),
            value_at("observed", OBS, u64::from(signals::PEER_CLOSED)),
            write("write", held, MSG, 8, Error::PeerClosed.raw()),
            read("read", held, RBUF, 16, Error::PeerClosed.raw()),
        ];
        let ender_steps = alloc::vec![];
        let ender_steps = match exit_step {
            None => ender_steps,
            Some(step) => alloc::vec![step],
        };
        if waiter.scratch().is_none()
            || waiter.script(&waiter_steps).is_none()
            || ender.scratch().is_none()
            || ender.script(&ender_steps).is_none()
        {
            return false;
        }
        let (waiter, ender) = (waiter.finish(), ender.finish());
        let wakes_before = ipc::cross_cpu_wakes();
        if Process::start_thread(&waiter, far, CODE, STACK + PAGE, SCRIPT).is_err() {
            return false;
        }
        // The peer must be in `wait_one` before the other process ends. It is then blocked,
        // or about to be, and the close wakes it either way (no lost wake-up, `wait`).
        sched::yield_until(|| waiter.waiting.load(Ordering::Acquire) > 0, tsc_hz, 5);
        if Process::start_thread(&ender, bsp, CODE, STACK + PAGE, SCRIPT).is_err() {
            return false;
        }
        let ended = await_end(&ender, tsc_hz, 10) && await_end(&waiter, tsc_hz, 10);
        let groups = [Group {
            title: "the peer of a process that",
            steps: waiter_steps,
        }];
        let right_reports = check_groups(com1, &groups, &waiter.reports());
        let wakes = ipc::cross_cpu_wakes().wrapping_sub(wakes_before);
        let ender_ok = matches!(
            (exit_step.is_some(), ender.ending()),
            (false, Some(Ending::Exited(0))) | (true, Some(Ending::Faulted { vector: 6, .. }))
        );
        let handles_left = ender.handle_count();
        let right =
            ended && right_reports && ender_ok && handles_left == 0 && (far == bsp || wakes > 0);
        ok &= right;
        let _ = writeln!(
            com1,
            "origin-kernel: ipc: when a process {label}: its handles close ({handles_left} left), and its peer on {}, blocked {} times, woke and saw PEER_CLOSED; {} wake-ups crossed processors{}",
            far.named(),
            waiter.blocked(),
            wakes,
            if right { "" } else { ": ipc failed" }
        );
    }
    ok
}

/// A thread blocked in `wait_one` completes with `CANCELED` when another thread of its
/// process closes the handle it waits on.
fn cancel(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let Some(mut image) = Image::new(LIMITS, script_program()) else {
        return false;
    };
    // The second thread's stack.
    if image.map(STACK_1, 1, Access::ReadWrite).is_none() {
        return false;
    }
    let Ok((a, b)) = ipc::create_channel(&image.process.account) else {
        return false;
    };
    // The peer end stays open in the same process: only the close can end the wait.
    let (Ok(ha), Ok(_)) = (
        image.process.grant(Object::Channel(a), rights::CHANNEL),
        image.process.grant(Object::Channel(b), rights::CHANNEL),
    ) else {
        return false;
    };
    let waited = Imm(u64::from(ha));
    let first = alloc::vec![wait(
        "the waiter",
        waited,
        signals::READABLE | signals::PEER_CLOSED,
        DEADLINE_INFINITE,
        Error::Canceled.raw(),
    )];
    let second = alloc::vec![close("", waited, OK)];
    if image.scratch().is_none() || image.script(&first).is_none() {
        return false;
    }
    let second_script = SCRIPT + SCRIPT_PAGES * PAGE / 2;
    if image.write_script(second_script, &second).is_none() {
        return false;
    }
    let process = image.finish();
    if Process::start_thread(&process, bsp, CODE, STACK + PAGE, SCRIPT).is_err() {
        return false;
    }
    // The closing thread starts only once the waiter is in `wait_one`, past its lookup of
    // the handle: whichever comes first, its block or the close, the wait ends `CANCELED`.
    sched::yield_until(|| process.waiting.load(Ordering::Acquire) > 0, tsc_hz, 5);
    let in_wait = process.waiting.load(Ordering::Acquire) > 0;
    if Process::start_thread(&process, far, CODE, STACK_1 + PAGE, second_script).is_err() {
        return false;
    }
    let ended = await_end(&process, tsc_hz, 10);
    let reports = process.reports();
    // Both threads' statuses are in the one log; the waiter's must be `CANCELED`.
    let canceled = reports.contains(&(Error::Canceled.raw() as u64));
    let right = ended && in_wait && canceled && process.ending() == Some(Ending::Exited(0));
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: a thread in wait_one on a handle another thread closes completes with {}{}",
        if canceled {
            "CANCELED"
        } else {
            "something else"
        },
        if right { "" } else { ": ipc failed" }
    );
    right
}

/// Quotas stop a process, not the kernel: it can make channels until its channel quota,
/// handles until its table is full, and carry on.
fn quotas(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let limits = Limits {
        handles: 12,
        channels: 4,
        debug_calls: true,
    };
    let limit = Error::Limit.raw();
    let steps = alloc::vec![
        create(0),
        create(2),
        create(4),
        create(6),
        step(
            "a fifth channel",
            sys::CHANNEL_CREATE,
            &[Imm(slot_addr(8))],
            limit
        ),
        dup("", h(0), rights::READ, 8, OK),
        dup("", h(0), rights::READ, 9, OK),
        dup("", h(0), rights::READ, 10, OK),
        dup("", h(0), rights::READ, 11, OK),
        dup("a thirteenth handle", h(0), rights::READ, 12, limit),
        close("", h(11), OK),
        dup("one more after a close", h(0), rights::READ, 12, OK),
        write("still working", h(2), MSG, 8, OK),
    ];
    let group = [Group {
        title: "quotas",
        steps,
    }];
    let Some(mut image) = Image::new(limits, script_program()) else {
        return false;
    };
    if image.scratch().is_none() || image.script(&group[0].steps).is_none() {
        return false;
    }
    let process = image.finish();
    if Process::start_thread(&process, bsp, CODE, STACK + PAGE, SCRIPT).is_err() {
        return false;
    }
    let ended = await_end(&process, tsc_hz, 10);
    let groups_ok = check_groups(com1, &group, &process.reports());
    ended && groups_ok && process.ending() == Some(Ending::Exited(0))
}

/// A 64 KiB message, the largest the ABI allows, arrives whole.
fn big_message(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let steps = alloc::vec![
        create(0),
        write(
            "sixty-four KiB",
            h(0),
            BIG_OUT,
            MAX_MESSAGE_BYTES as u64,
            OK
        ),
        read("received", h(1), BIG_IN, MAX_MESSAGE_BYTES as u64, OK),
        value_at(
            "bytes",
            SIZES + read_sizes::BYTES_AT as u64,
            MAX_MESSAGE_BYTES as u64
        ),
    ];
    let Some(mut image) = Image::new(LIMITS, script_program()) else {
        return false;
    };
    if image.map(BIG_OUT, BIG_PAGES, Access::ReadWrite).is_none()
        || image.map(BIG_IN, BIG_PAGES, Access::ReadWrite).is_none()
        || image.scratch().is_none()
        || image.script(&steps).is_none()
    {
        return false;
    }
    let pattern = |k: u64| (k.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 13) ^ k;
    for k in 0..(MAX_MESSAGE_BYTES as u64 / 8) {
        if image.write_u64(BIG_OUT + 8 * k, pattern(k)).is_none() {
            return false;
        }
    }
    let process = image.finish();
    if Process::start_thread(&process, bsp, CODE, STACK + PAGE, SCRIPT).is_err() {
        return false;
    }
    let ended = await_end(&process, tsc_hz, 10);
    let group = [Group {
        title: "a full-size message",
        steps,
    }];
    let groups_ok = check_groups(com1, &group, &process.reports());
    let mut intact = ended;
    if ended {
        for k in 0..(MAX_MESSAGE_BYTES as u64 / 8) {
            intact &= process.read_u64(BIG_IN + 8 * k) == Some(pattern(k));
        }
    }
    let right = ended && groups_ok && intact && process.ending() == Some(Ending::Exited(0));
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: 64 KiB through a channel and back, byte for byte: {intact}{}",
        if right { "" } else { ": ipc failed" }
    );
    right
}

/// P4's register convention, the `syscall`-origin rule, and the two hostile processor-state
/// tests.
fn abi_and_entry(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let mut ok = true;

    // Every register but RAX, RCX and R11 survives a call.
    let code = program(&raw const ipc_regs_start, &raw const ipc_regs_end);
    let Some(image) = Image::new(LIMITS, code) else {
        return false;
    };
    let regs = image.finish();
    if Process::start_thread(&regs, bsp, CODE, STACK + PAGE, 0).is_err() {
        return false;
    }
    let ended = await_end(&regs, tsc_hz, 5);
    let right = ended && regs.reports() == [0] && regs.ending() == Some(Ending::Exited(0));
    ok &= right;
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: registers: every register but RAX, RCX and R11 survives a system call: {right}{}",
        if right { "" } else { ": ipc failed" }
    );

    // System calls only from the code range the creator named (P4, ONBF §6.1): this process
    // is told its origin is the page below its code, so none of its calls is accepted, not
    // even `exit`. It ends by the deliberate fault at the end of its script.
    let Some(mut image) = Image::with_origin(LIMITS, script_program(), (CODE - PAGE, CODE)) else {
        return false;
    };
    let script = alloc::vec![step("", sys::DEBUG_REPORT, &[Imm(1)], OK), fault()];
    if image.scratch().is_none() || image.script(&script).is_none() {
        return false;
    }
    let pinned = image.finish();
    if Process::start_thread(&pinned, bsp, CODE, STACK + PAGE, SCRIPT).is_err() {
        return false;
    }
    let ended = await_end(&pinned, tsc_hz, 5);
    let refused = pinned.reports().is_empty();
    let right =
        ended && refused && matches!(pinned.ending(), Some(Ending::Faulted { vector: 6, .. }));
    ok &= right;
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: a process's system calls are accepted only from its pinned code range: {}, and it ended by its own fault{}",
        if refused {
            "every call refused"
        } else {
            "a call got through"
        },
        if right { "" } else { ": ipc failed" }
    );

    // A program that changes GS cannot touch the kernel: a timer interrupt or system call
    // from it must find the kernel's GS.
    let gs = program(&raw const ipc_gs_start, &raw const ipc_gs_end);
    let Some(image) = Image::new(LIMITS, gs) else {
        return false;
    };
    let process = image.finish();
    if Process::start_thread(&process, bsp, CODE, STACK + PAGE, 0).is_err() {
        return false;
    }
    let ended = await_end(&process, tsc_hz, 10);
    let right = ended && process.reports() == [1, 0] && process.ending() == Some(Ending::Exited(0));
    ok &= right;
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: a program that loaded a null GS was interrupted and made system calls, and the kernel used its own GS (reports {:?}){}",
        process.reports(),
        if right { "" } else { ": ipc failed" }
    );

    // A program that sets RFLAGS.AC cannot switch SMAP off in the kernel.
    let ac = program(&raw const ipc_ac_start, &raw const ipc_ac_end);
    let Some(image) = Image::new(LIMITS, ac) else {
        return false;
    };
    let process = image.finish();
    let leaks_before = cpu::ac_leaks();
    if Process::start_thread(&process, bsp, CODE, STACK + PAGE, 0).is_err() {
        return false;
    }
    let ended = await_end(&process, tsc_hz, 10);
    let leaks = cpu::ac_leaks().wrapping_sub(leaks_before);
    let reported = process.reports();
    let flag_was_set = reported.first().is_some_and(|f| f & (1 << 18) != 0);
    let right = ended && flag_was_set && leaks == 0 && process.ending() == Some(Ending::Exited(0));
    ok &= right;
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: a program with RFLAGS.AC set took interrupts and made a system call: the flag reached kernel code {leaks} times, and stayed set in ring 3: {flag_was_set}{}",
        if right { "" } else { ": ipc failed" }
    );

    // A process without the developer calls cannot use them (the ABI reserves them for
    // developer builds): it cannot hold the console for the whole machine.
    let nodebug = program(&raw const ipc_nodebug_start, &raw const ipc_nodebug_end);
    let limits = Limits {
        debug_calls: false,
        ..LIMITS
    };
    let Some(image) = Image::new(limits, nodebug) else {
        return false;
    };
    let process = image.finish();
    if Process::start_thread(&process, bsp, CODE, STACK + PAGE, 0).is_err() {
        return false;
    }
    let ended = await_end(&process, tsc_hz, 5);
    let right =
        ended && process.ending() == Some(Ending::Exited(0)) && process.reports().is_empty();
    ok &= right;
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: a process without the developer calls gets NOT_SUPPORTED: {right}{}",
        if right { "" } else { ": ipc failed" }
    );

    // With UMIP on, `sidt` from ring 3 faults: the kernel's descriptor-table addresses, which
    // reveal the KASLR slide, stay out of reach of programs.
    let sidt = program(&raw const ipc_sidt_start, &raw const ipc_sidt_end);
    let Some(image) = Image::new(LIMITS, sidt) else {
        return false;
    };
    let process = image.finish();
    if Process::start_thread(&process, bsp, CODE, STACK + PAGE, 0).is_err() {
        return false;
    }
    let ended = await_end(&process, tsc_hz, 5);
    let umip = cpu::umip_on();
    let refused = matches!(process.ending(), Some(Ending::Faulted { vector: 13, .. }));
    let right = ended && (!umip || refused);
    ok &= right;
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: sidt from ring 3 with UMIP {}: {}{}",
        if umip { "on" } else { "unavailable" },
        if refused {
            "refused, the process ended by vector 13"
        } else {
            "completed"
        },
        if right { "" } else { ": ipc failed" }
    );
    ok
}

/// A page fault during a user copy comes back as a failed copy, not a dead kernel. The
/// system-call path checks every pointer first, so nothing else ever exercises this; it
/// copies here without a check, on purpose, from a user address no address space maps.
fn copy_fault_recovered(com1: &mut Console) -> bool {
    const NOWHERE: u64 = 0x0000_2000_0000_0000;
    let mut buffer = [0u8; 16];
    let destination = buffer.as_mut_ptr().expose_provenance() as u64;
    let before = cpu::user_copy_faults();
    // SAFETY: `buffer` is a live 16-byte kernel buffer; `NOWHERE` is a user address that is
    // not mapped in the kernel's tables, the ones in force here, so the first access
    // faults and the fault is recovered.
    let completed = unsafe { cpu::copy_user(destination, NOWHERE, 16) };
    let recovered = cpu::user_copy_faults().wrapping_sub(before);
    let right = !completed && recovered == 1 && buffer == [0; 16];
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: a fault inside a user copy comes back as a failed copy, not a dead kernel: {recovered} fault recovered{}",
        if right { "" } else { ": ipc failed" }
    );
    right
}

/// Runs every proof and checks nothing leaked. Called from the boot flow with every
/// scheduler running.
pub(crate) fn prove(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let before = memory::stats();
    let started = time::monotonic_nanos();
    let mut ok = true;
    ok &= ping_pong(com1, cpus, bsp, tsc_hz);
    ok &= hostile(com1, bsp, tsc_hz);
    ok &= exit_closes(com1, cpus, bsp, tsc_hz);
    ok &= cancel(com1, cpus, bsp, tsc_hz);
    ok &= quotas(com1, bsp, tsc_hz);
    ok &= big_message(com1, bsp, tsc_hz);
    ok &= abi_and_entry(com1, bsp, tsc_hz);
    ok &= copy_fault_recovered(com1);
    // Every process has exited and been dropped here. Once each processor has reaped what
    // its own threads left, the heap and the frame count are back where they started.
    let same = || {
        memory::stats().map(|(h, f)| (h.bytes_in_use, f))
            == before.map(|(h, f)| (h.bytes_in_use, f))
    };
    ipc::reap_closed();
    sched::yield_until(same, tsc_hz, 2);
    let back = same();
    let elapsed = time::monotonic_nanos().wrapping_sub(started) / 1_000_000;
    let (heap_now, frames_now) = memory::stats().map_or((0, 0), |(h, f)| (h.bytes_in_use, f));
    let (heap_before, frames_before) = before.map_or((0, 0), |(h, f)| (h.bytes_in_use, f));
    // Every thread's state was complete before it could be picked (`sched::spawn_with`).
    let unready = sched::unready_switches();
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: every process's memory, handles, channels and queued messages are gone: heap {heap_now} bytes (was {heap_before}), {frames_now} free frames (was {frames_before}), {elapsed} ms for the whole proof; threads run before their state existed: {unready}{}",
        if back && unready == 0 {
            ""
        } else {
            ": ipc failed"
        }
    );
    ok && back && unready == 0
}
