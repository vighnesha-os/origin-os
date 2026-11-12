//! Each call of `origin.kernel` by its number, made through its import: one of the two files of
//! this program that may hold `unsafe` (`cargo xtask check`'s gate lists it).
//!
//! The proof's scripts are written in call numbers and raw register values, hostile ones among
//! them, which the typed client cannot say. So each entry point of `origin.core` is declared
//! here a second time, taking six whole registers. That is sound for these functions and no
//! others: each is a stub written in assembly that takes the C calling convention's argument
//! registers as they are, uses only as many as its method has, and extends for itself each one
//! its method takes narrower than 64 bits (ADR-0011 Amendment 3, L13; that extension is one of
//! the things the proof shows). A status comes back in the whole of RAX.
//!
//! The numbers are `origin-abi`'s names for them, not written out: they are private to the
//! release (CLAUDE.md §42), and this program is built with the kernel that carries it.

use origin_abi::syscall as sys;

unsafe extern "C" {
    fn origin_kernel_thread_exit(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
    fn origin_kernel_debug_report(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
    fn origin_kernel_debug_write(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
    fn origin_kernel_handle_close(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
    fn origin_kernel_handle_duplicate(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
    fn origin_kernel_channel_create(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
    fn origin_kernel_channel_write(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
    fn origin_kernel_channel_read(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
    fn origin_kernel_wait_one(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
    fn origin_kernel_event_create(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
    fn origin_kernel_object_signal(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
    fn origin_kernel_memory_object_create(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
    fn origin_kernel_memory_map(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
    fn origin_kernel_memory_unmap(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
    fn origin_kernel_memory_protect(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
    fn origin_kernel_port_create(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
    fn origin_kernel_object_wait_async(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
    fn origin_kernel_port_wait(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
    fn origin_kernel_port_cancel(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
    fn origin_kernel_clock_read(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64;
}

/// Calls the entry point `$entry` with the six registers `$registers`.
macro_rules! through {
    ($entry:ident, $registers:expr) => {{
        let [a, b, c, d, e, f] = $registers;
        // SAFETY: the callee is a stub of `origin.core`, which takes its argument registers as
        // they are and extends what it must (the module documentation). Whatever the values,
        // the call is a system call, and the kernel checks every pointer among them against
        // this process's own tables before it reads or writes anything: a hostile value earns
        // a status, and touches no memory of this program that the kernel was not told to
        // write.
        unsafe { $entry(a, b, c, d, e, f) }
    }};
}

/// Makes call `number` with the six registers `arguments`, through its import, and returns its
/// status. `None` for a number `origin.core` has no stub for: through `origin.core` no such
/// call can be made. A call that ends the thread does not return.
///
/// Each arm calls its import by name, so that each is a call through the import's own slot,
/// which the launch path bound. A table of the entry points' addresses would need a second
/// kind of slot, in data, which the build refuses (`onbf-pack carried` reads only the linker's
/// own import slots).
pub(crate) fn call(number: u64, arguments: [u64; 6]) -> Option<i64> {
    Some(match number {
        sys::THREAD_EXIT => through!(origin_kernel_thread_exit, arguments),
        sys::DEBUG_REPORT => through!(origin_kernel_debug_report, arguments),
        sys::DEBUG_WRITE => through!(origin_kernel_debug_write, arguments),
        sys::HANDLE_CLOSE => through!(origin_kernel_handle_close, arguments),
        sys::HANDLE_DUPLICATE => through!(origin_kernel_handle_duplicate, arguments),
        sys::CHANNEL_CREATE => through!(origin_kernel_channel_create, arguments),
        sys::CHANNEL_WRITE => through!(origin_kernel_channel_write, arguments),
        sys::CHANNEL_READ => through!(origin_kernel_channel_read, arguments),
        sys::WAIT_ONE => through!(origin_kernel_wait_one, arguments),
        sys::EVENT_CREATE => through!(origin_kernel_event_create, arguments),
        sys::OBJECT_SIGNAL => through!(origin_kernel_object_signal, arguments),
        sys::MEMORY_OBJECT_CREATE => through!(origin_kernel_memory_object_create, arguments),
        sys::MEMORY_MAP => through!(origin_kernel_memory_map, arguments),
        sys::MEMORY_UNMAP => through!(origin_kernel_memory_unmap, arguments),
        sys::MEMORY_PROTECT => through!(origin_kernel_memory_protect, arguments),
        sys::PORT_CREATE => through!(origin_kernel_port_create, arguments),
        sys::OBJECT_WAIT_ASYNC => through!(origin_kernel_object_wait_async, arguments),
        sys::PORT_WAIT => through!(origin_kernel_port_wait, arguments),
        sys::PORT_CANCEL => through!(origin_kernel_port_cancel, arguments),
        sys::CLOCK_READ => through!(origin_kernel_clock_read, arguments),
        _ => return None,
    })
}
