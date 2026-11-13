//! The proof's deliberate faults: a write to the program's own code, a jump into its own
//! writable data, and a write to its relocated read-only data. The only file of this program
//! that may hold `unsafe` (`cargo xtask check`'s gate lists it), because each is meant to fault:
//! the launch path must have mapped code read-execute, data writable but never executable, and
//! RELRO read-only once relocated (ONBF §6.1, CLAUDE.md §14). Each reports the address it is
//! about to fault at, then faults there; none returns.

/// Bytes in writable data that would return at once if they ran: `ret`.
static mut DATA: [u8; 16] = [0xC3; 16];

/// Writes over the first byte of this function's own code.
pub(crate) fn write_code(report: fn(u64)) -> ! {
    let target = write_code as fn(fn(u64)) -> ! as usize;
    report(target as u64);
    let code = core::ptr::with_exposed_provenance_mut::<u8>(target);
    // SAFETY: none is claimed. This write is meant to fault: the page is mapped read-execute,
    // so the processor raises a page fault and the kernel ends the thread before anything is
    // written. Were it ever written, the program would be ending anyway.
    unsafe { code.write_volatile(0xCC) };
    loop {
        core::hint::spin_loop();
    }
}

/// Calls the bytes of [`DATA`] as code.
pub(crate) fn jump_data(report: fn(u64)) -> ! {
    let target = (&raw const DATA).cast::<u8>();
    report(target as usize as u64);
    // SAFETY: none is claimed. This call is meant to fault: `DATA` is in a page mapped writable
    // and never executable, so fetching its first byte raises a page fault and the kernel ends
    // the thread. Were it ever run, its byte is `ret`, which returns here.
    let code = unsafe { core::mem::transmute::<*const u8, extern "C" fn()>(target) };
    code();
    loop {
        core::hint::spin_loop();
    }
}

/// Writes over the relocated pointer at `target`, in a RELRO page.
pub(crate) fn write_relro(report: fn(u64), target: u64) -> ! {
    report(target);
    let pointer = core::ptr::with_exposed_provenance_mut::<u64>(target as usize);
    // SAFETY: none is claimed. This write is meant to fault: the launch path maps RELRO pages
    // read-only to the program from the start, so the processor raises a page fault and the
    // kernel ends the thread before anything is written.
    unsafe { pointer.write_volatile(0) };
    loop {
        core::hint::spin_loop();
    }
}
