//! The panic handler: a panicking program's thread ends, with [`PANIC_EXIT`] (ONBF §6.1.1:
//! userspace Rust is built with `panic=abort` until the Origin `std` port exists).

use core::panic::PanicInfo;

use origin_kernel_stubs::origin_kernel::kernel::client;

/// The exit code of a thread that panicked. The message is not written anywhere: a program has
/// no console, and the location would carry its source paths.
pub const PANIC_EXIT: u64 = u64::MAX;

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    client::thread_exit(PANIC_EXIT)
}
