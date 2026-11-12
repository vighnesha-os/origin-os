//! What the proof needs that safe Rust cannot say: one of the two files of this program that may
//! hold `unsafe` (`cargo xtask check`'s gate lists it).
//!
//! - The script and the scratch memory the kernel maps at fixed addresses before the program
//!   starts, read and written a word at a time.
//! - The program's own entry, which reads RFLAGS before any other instruction of the thread
//!   runs (ADR-0005 Amendment 9, H1), then goes on to the runtime's.
//! - The two calls the per-call pin must stop (ADR-0005 B2, G1): a `syscall` instruction in
//!   this program's own code, and a jump into the middle of a stub of `origin.core` with
//!   another call's number in RAX. The first is the one `syscall` this program holds, and the
//!   gate on carried programs counts it.
//! - A fault, for the script's `-4`.

use core::arch::{asm, naked_asm};

/// Where the kernel puts the script, read-only to the program: the address its assembly
/// interpreter's scripts go at (`origin_frame::proof_programs::SCRIPT`).
pub(crate) const SCRIPT: u64 = 0x54_0000;

/// RFLAGS as [`origin_pin_entry`] found them. Written once, by that entry, before anything else
/// of the thread runs; read by [`entry_flags`].
static mut ENTRY_FLAGS: u64 = 0;

/// Where the kernel starts this program's first thread, in place of the runtime's `_start`
/// (`cargo xtask` gives the linker this entry for this program alone). Its first instruction
/// pushes RFLAGS, so what it keeps is what the kernel started the thread with: no instruction
/// of the thread has run before it. `pushfq`, `pop` and `mov` change no flag, and RDI, the start
/// record's address, is left as it was. Then the runtime's entry takes over; RAX, which it
/// does not read, no longer holds zero.
#[unsafe(naked)]
#[unsafe(no_mangle)]
extern "C" fn origin_pin_entry() -> ! {
    naked_asm!(
        "pushfq",
        "pop rax",
        "mov qword ptr [rip + {flags}], rax",
        "jmp {start}",
        flags = sym ENTRY_FLAGS,
        start = sym _start,
    )
}

unsafe extern "C" {
    /// The runtime's entry (`origin-rt`).
    fn _start() -> !;
}

/// RFLAGS as the thread's first instruction found them.
pub(crate) fn entry_flags() -> u64 {
    // SAFETY: `ENTRY_FLAGS` is written once, by the entry, before any Rust code of this
    // single-threaded program runs, and never again: this read races with nothing.
    unsafe { (&raw const ENTRY_FLAGS).read() }
}

/// Makes call `number` with `argument` by a `syscall` instruction in this program's own code:
/// the one `syscall` this program holds, which no stub of `origin.core` is. The kernel must
/// stop the thread here (ADR-0005 G1). If it does not, this returns the status.
#[unsafe(naked)]
pub(crate) extern "C" fn own_syscall(number: u64, argument: u64) -> i64 {
    naked_asm!("mov rax, rdi", "mov rdi, rsi", "syscall", "ret")
}

/// Jumps to `target` with `number` in RAX and `argument` in RDI. With `target` the `syscall`
/// instruction of one call's stub and `number` another call's, this is the attack the per-call
/// pin exists to stop (row E8 of the system-call threat model): the stub's own load of its
/// number is skipped. The stub's return, if the kernel ever let the call through, comes back to
/// this function's caller with the status.
#[unsafe(naked)]
pub(crate) extern "C" fn jump_into(target: u64, number: u64, argument: u64) -> i64 {
    naked_asm!("mov rax, rsi", "mov r11, rdi", "mov rdi, rdx", "jmp r11",)
}

/// The eight words of the step at `at`: its number, its flags and its six arguments.
pub(crate) fn step(at: u64) -> [u64; 8] {
    let mut words = [0u64; 8];
    for (word, offset) in words.iter_mut().zip((0u64..).step_by(8)) {
        *word = read_u64(at.wrapping_add(offset));
    }
    words
}

/// The `u64` at `at`.
pub(crate) fn read_u64(at: u64) -> u64 {
    let pointer = core::ptr::with_exposed_provenance::<u64>(at as usize);
    // SAFETY: none is claimed beyond the proof's own arrangement. The kernel maps the script
    // and the scratch memory at the addresses its steps name, and writes them before this
    // program starts; an address it did not map faults, which ends the thread and fails the
    // proof. An unaligned read, since a step may name any address.
    unsafe { pointer.read_unaligned() }
}

/// The `u32` at `at`.
pub(crate) fn read_u32(at: u64) -> u32 {
    let pointer = core::ptr::with_exposed_provenance::<u32>(at as usize);
    // SAFETY: as in `read_u64`.
    unsafe { pointer.read_unaligned() }
}

/// Stores `value` at `at`.
pub(crate) fn write_u64(at: u64, value: u64) {
    let pointer = core::ptr::with_exposed_provenance_mut::<u64>(at as usize);
    // SAFETY: as in `read_u64`, for the scratch memory, which is writable. A step may also
    // name an address that must not be writable, a page of `origin.core` among them: the
    // write then faults, which is what that step is there to show.
    unsafe { pointer.write_unaligned(value) }
}

/// Stores `value` at `at`.
pub(crate) fn write_u32(at: u64, value: u32) {
    let pointer = core::ptr::with_exposed_provenance_mut::<u32>(at as usize);
    // SAFETY: as in `write_u64`.
    unsafe { pointer.write_unaligned(value) }
}

/// Executes `ud2`: an invalid-opcode fault ends the thread.
pub(crate) fn fault() -> ! {
    // SAFETY: none is claimed. `ud2` is meant to fault, and the kernel ends the thread.
    unsafe { asm!("ud2", options(noreturn, nomem, nostack)) }
}
