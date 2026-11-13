//! `memcpy`, `memmove`, `memset`, `memcmp` and `bcmp`: the compiler calls them for copies,
//! fills and comparisons, and on the userspace triple they would come from a C library, which
//! Origin does not have (CLAUDE.md §11). One of the two files of this crate that may hold
//! `unsafe`.
//!
//! Each is its C name around a body in `origin-rt-mem` (`runtime/rt-mem`), which the host tests
//! against `core`'s own copy, fill and comparison (S4-1's review, finding F6): a copy up is one
//! `rep movsb`, a copy down `std; rep movsb; cld`, a fill one `rep stosb` and a comparison a byte
//! loop. Both crates are `no_builtins`, so the compiler cannot turn any of these into a call to
//! itself. The direction flag is clear on entry and on return, as the psABI requires.

use core::ffi::c_void;

/// Copies `n` bytes from `src` to `dest`, which must not overlap, and returns `dest`.
///
/// # Safety
///
/// `src` must be valid for reads and `dest` for writes of `n` bytes, and the two must not
/// overlap.
#[unsafe(no_mangle)]
unsafe extern "C" fn memcpy(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    // SAFETY: the caller's promise, which is more than `copy_up` asks: no overlap at all.
    unsafe { origin_rt_mem::copy_up(dest.cast(), src.cast(), n) };
    dest
}

/// Copies `n` bytes from `src` to `dest`, which may overlap, and returns `dest`.
///
/// # Safety
///
/// `src` must be valid for reads and `dest` for writes of `n` bytes.
#[unsafe(no_mangle)]
unsafe extern "C" fn memmove(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    // SAFETY: the caller's promise is `copy`'s.
    unsafe { origin_rt_mem::copy(dest.cast(), src.cast(), n) };
    dest
}

/// Writes the low byte of `c` to `n` bytes at `dest`, and returns `dest`.
///
/// # Safety
///
/// `dest` must be valid for writes of `n` bytes.
#[unsafe(no_mangle)]
unsafe extern "C" fn memset(dest: *mut c_void, c: i32, n: usize) -> *mut c_void {
    // SAFETY: the caller's promise is `fill`'s. C converts `c` to `unsigned char`: its low byte.
    unsafe { origin_rt_mem::fill(dest.cast(), c as u8, n) };
    dest
}

/// Compares `n` bytes at `a` and `b` as unsigned bytes: negative, zero or positive as the first
/// byte that differs is smaller in `a`, absent, or larger in `a`.
///
/// # Safety
///
/// `a` and `b` must each be valid for reads of `n` bytes.
#[unsafe(no_mangle)]
unsafe extern "C" fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> i32 {
    // SAFETY: the caller's promise is `compare`'s.
    unsafe { origin_rt_mem::compare(a.cast(), b.cast(), n) }
}

/// Zero when the `n` bytes at `a` and `b` are equal, and not zero when they differ.
///
/// # Safety
///
/// `a` and `b` must each be valid for reads of `n` bytes.
#[unsafe(no_mangle)]
unsafe extern "C" fn bcmp(a: *const c_void, b: *const c_void, n: usize) -> i32 {
    // SAFETY: the caller's promise is `compare`'s.
    unsafe { origin_rt_mem::compare(a.cast(), b.cast(), n) }
}
