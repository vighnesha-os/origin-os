//! The runtime's entry: `_start`, the start record, and the call to the program's `main`
//! (ADR-0005 P6, C4). One of the two files of this crate that may hold `unsafe`.

use core::arch::naked_asm;

use origin_abi::start::{self, StartRecord};
use origin_kernel_stubs::origin_kernel::kernel::client;

/// The exit code of a program whose start record did not decode. The kernel writes every record
/// itself, so a correct kernel never gives one.
pub const BAD_RECORD_EXIT: u64 = u64::MAX - 1;

/// The exit code of a thread that somehow began to unwind: see [`rust_eh_personality`].
pub const UNWIND_EXIT: u64 = u64::MAX - 2;

/// What a program's `main` is given: its start record (P6), decoded and checked.
#[derive(Clone, Copy, Debug)]
pub struct Start {
    /// The record's address: what RDI held when the first thread started.
    pub address: u64,
    /// The record. The kernel maps it read-only for the life of the process (C4).
    pub record: StartRecord<'static>,
}

unsafe extern "Rust" {
    /// The program's `main`, named by [`crate::main!`].
    safe fn origin_rt_program_main(start: &Start) -> u64;
}

/// Where the kernel starts the first thread: RDI holds the start record's address, RSP the top
/// of its stack, and every other register is zero (P6). The psABI says RSP is 16-byte aligned
/// here; the stack is aligned again rather than trusted, RBP is cleared so a backtrace ends
/// here, and [`run`] is called with RDI untouched, as its first argument. `run` never returns;
/// `ud2` stops the thread if it ever did.
#[unsafe(naked)]
#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    naked_asm!(
        "xor ebp, ebp",
        "and rsp, -16",
        "call {run}",
        "ud2",
        run = sym run,
    )
}

/// The address `_start` runs at: the program's base plus its entry's offset.
#[must_use]
pub fn entry_address() -> u64 {
    (_start as extern "C" fn() -> !) as usize as u64
}

/// Named by the unwind tables of the prebuilt `core` for the userspace triple, which is compiled
/// to unwind; nothing calls it here. A program is built with `panic=abort` (ONBF §6.1.1), no
/// unwinder is linked, and the panic handler ends the thread, so no unwinding ever starts. Were
/// it ever called, the thread ends with [`BAD_RECORD_EXIT`]'s neighbour, [`UNWIND_EXIT`].
#[unsafe(no_mangle)]
extern "C" fn rust_eh_personality() -> ! {
    client::thread_exit(UNWIND_EXIT)
}

/// Decodes the record at `address`, runs the program and ends the thread with its exit code.
extern "C" fn run(address: u64) -> ! {
    let code = match record_at(address) {
        Some(record) => origin_rt_program_main(&Start { address, record }),
        None => BAD_RECORD_EXIT,
    };
    client::thread_exit(code)
}

/// The start record at `address`, decoded and checked, or `None`.
fn record_at(address: u64) -> Option<StartRecord<'static>> {
    let at = usize::try_from(address).ok()?;
    // The kernel places a record at the start of a page (C4).
    if at == 0 || at % 4096 != 0 {
        return None;
    }
    let pointer = core::ptr::with_exposed_provenance::<u8>(at);
    // SAFETY: the kernel maps the start record read-only at the address it passes in RDI, at
    // least `start::HEADER` bytes, and never unmaps or changes it while the process lives (C4):
    // the bytes are valid, initialised and never written for `'static`.
    let header = unsafe { core::slice::from_raw_parts(pointer, start::HEADER) };
    let size = header
        .get(start::SIZE_AT..start::SIZE_AT + 4)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_le_bytes)?;
    let size = usize::try_from(size).ok()?;
    if !(start::HEADER..=start::MAX_SIZE).contains(&size) {
        return None;
    }
    // SAFETY: as above, for the record's whole size, which its header states and the kernel
    // mapped (C4); `size` is at most 64 KiB.
    let bytes = unsafe { core::slice::from_raw_parts(pointer, size) };
    start::decode(bytes).ok()
}

/// Names the program's `main`, a `fn(&Start) -> u64` whose result is the thread's exit code.
/// Use it once, in the program's crate root:
///
/// ```ignore
/// origin_rt::main!(main);
/// fn main(start: &origin_rt::Start) -> u64 { 0 }
/// ```
///
/// It gives `main` the one name the runtime's entry calls. That needs `#[unsafe(no_mangle)]`,
/// which the expansion allows here so that the program's own files hold no `unsafe`.
#[macro_export]
macro_rules! main {
    ($main:path) => {
        /// The program's `main`, under the name `origin_rt`'s entry calls.
        #[allow(unsafe_code)]
        #[unsafe(no_mangle)]
        fn origin_rt_program_main(start: &$crate::Start) -> u64 {
            $main(start)
        }
    };
}
