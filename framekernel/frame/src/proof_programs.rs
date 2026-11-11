//! The ring-3 programs of the kernel's IPC boot proof (`ipc_proof`, above the framework;
//! ADR-0005, ADR-0011 N6).
//!
//! They are hand-written assembly, carried in the kernel's read-only image as data: each is
//! copied to a process's code pages, and the kernel executes none of it. Assembling them takes
//! `global_asm!`, so they live here; all safe code can do with one is read its bytes.

use core::arch::global_asm;

use origin_abi::{Error, signals, syscall as sys};
use origin_paging::PAGE;

/// The read/write scratch memory of every proof process: these programs use it at fixed
/// addresses.
pub const DATA: u64 = 0x50_0000;
/// The stack of a proof process's thread 0: one page, with no neighbour mapped below it.
pub const STACK: u64 = 0x60_0000;
/// Where a scripted process's script is (read-only to it).
pub const SCRIPT: u64 = 0x54_0000;
/// Where a launched program keeps the RDI its first thread started with: its start record's
/// address (ADR-0005 P6).
pub const ENTRY_RDI: u64 = DATA + 0x600;

global_asm!(
    r#"
    .pushsection .rodata.ipc_programs,"a",@progbits

    // The interpreter: RDI points at a script, a table of 64-byte steps, each
    // number, flags, a0, a1, a2, a3, a4, a5. Bit i of `flags` set means argument i is
    // the address of a u32 to load (so a handle a call wrote can be used by a later call).
    // After a real call its status is reported, in order.
    .globl ipc_script_start
    .globl ipc_script_end
    .globl ipc_launched_start

    // A launched process starts here: RDI holds its start record's address (ADR-0005 P6). It
    // keeps that at ENTRY_RDI and runs the script at SCRIPT, falling into the interpreter.
ipc_launched_start:
    mov qword ptr [{ENTRY_RDI}], rdi
    mov edi, {SCRIPT}
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

    // Sets RFLAGS.TF: the next instruction traps to #DB, which the kernel takes on its own
    // interrupt stack and answers by ending the thread.
    .globl ipc_step_start
    .globl ipc_step_end
ipc_step_start:
    pushfq
    or dword ptr [rsp], 0x100
    popfq
    nop
    mov edi, 1
    mov eax, {DEBUG_REPORT}
    syscall
    xor edi, edi
.Lstep_exit:
    mov eax, {THREAD_EXIT}
    syscall
    jmp .Lstep_exit
ipc_step_end:

    .popsection
"#,
    DATA = const DATA,
    ENTRY_RDI = const ENTRY_RDI,
    SCRIPT = const SCRIPT,
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
    static ipc_launched_start: u8;
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
    static ipc_step_start: u8;
    static ipc_step_end: u8;
}

/// The bytes between two labels of the image.
fn between(start: *const u8, end: *const u8) -> &'static [u8] {
    // SAFETY: `start..end` delimit a program in the kernel's read-only image, which is
    // mapped and unchanging for as long as the kernel runs; `end` is not below `start`.
    unsafe { core::slice::from_raw_parts(start, end.addr().saturating_sub(start.addr())) }
}

/// The script interpreter: RDI names a table of calls, each reported in order.
pub fn script() -> &'static [u8] {
    between(&raw const ipc_script_start, &raw const ipc_script_end)
}

/// A launched program: keeps the start record's address it finds in RDI at [`ENTRY_RDI`], then
/// interprets the script at [`SCRIPT`].
pub fn launched() -> &'static [u8] {
    between(&raw const ipc_launched_start, &raw const ipc_script_end)
}

/// Writes each round number on a channel, then reads and checks the reply.
pub fn ping() -> &'static [u8] {
    between(&raw const ipc_ping_start, &raw const ipc_ping_end)
}

/// Waits for each round number, checks it, and answers one more.
pub fn pong() -> &'static [u8] {
    between(&raw const ipc_pong_start, &raw const ipc_pong_end)
}

/// Checks that every register but RAX, RCX and R11 survives a system call.
pub fn regs() -> &'static [u8] {
    between(&raw const ipc_regs_start, &raw const ipc_regs_end)
}

/// Loads a null GS and checks it survives a call and timer interrupts.
pub fn gs() -> &'static [u8] {
    between(&raw const ipc_gs_start, &raw const ipc_gs_end)
}

/// Sets RFLAGS.AC and runs through timer interrupts and a system call.
pub fn ac() -> &'static [u8] {
    between(&raw const ipc_ac_start, &raw const ipc_ac_end)
}

/// Makes the developer calls, which must come back as not supported.
pub fn nodebug() -> &'static [u8] {
    between(&raw const ipc_nodebug_start, &raw const ipc_nodebug_end)
}

/// Runs `sidt`, which must fault with UMIP on.
pub fn sidt() -> &'static [u8] {
    between(&raw const ipc_sidt_start, &raw const ipc_sidt_end)
}

/// Sets RFLAGS.TF, so that the next instruction traps to #DB.
pub fn step() -> &'static [u8] {
    between(&raw const ipc_step_start, &raw const ipc_step_end)
}
