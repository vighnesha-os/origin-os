//! x86 port I/O (Intel SDM Vol. 2, IN and OUT): the raw instructions.
//!
//! These are `unsafe fn`s with a contract: which port is written decides what the machine
//! does next. Safe callers will reach ports through claimed port ranges, not through these.

use core::arch::asm;

/// Writes `value` to I/O port `port`.
///
/// # Safety
///
/// CPL 0, and writing `value` to `port` must not have an effect on the machine that the
/// caller has not reasoned about (a port reset, a bus-master enable, ...).
pub unsafe fn outb(port: u16, value: u8) {
    // SAFETY: the caller's contract; OUT touches no memory.
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
    }
}

/// Reads a byte from I/O port `port`.
///
/// # Safety
///
/// CPL 0, and reading `port` must have no side effect the caller has not reasoned about
/// (reading a device's data port consumes the byte).
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: the caller's contract; IN touches no memory.
    unsafe {
        asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Reads a 32-bit value from I/O port `port` (the ACPI PM timer, for example).
///
/// # Safety
///
/// As for [`inb`].
pub unsafe fn ind(port: u16) -> u32 {
    let value: u32;
    // SAFETY: the caller's contract; IN touches no memory.
    unsafe {
        asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack, preserves_flags));
    }
    value
}
