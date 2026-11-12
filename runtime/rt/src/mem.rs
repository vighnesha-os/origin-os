//! `memcpy`, `memmove`, `memset`, `memcmp` and `bcmp`: the compiler calls them for copies,
//! fills and comparisons, and on the userspace triple they would come from a C library, which
//! Origin does not have (CLAUDE.md §11). One of the two files of this crate that may hold
//! `unsafe`.
//!
//! Copies and fills are one `rep movsb` or `rep stosb`, which processors of the x86-64-v3
//! baseline run fast for every length. Comparisons are a byte loop; the crate is `no_builtins`,
//! so the compiler cannot turn any of these into a call to itself. The direction flag is clear
//! on entry and on return, as the psABI requires: `memmove`'s backward copy sets it and clears
//! it again.

use core::arch::asm;
use core::ffi::c_void;

/// Copies `n` bytes from `src` to `dest`, which must not overlap, and returns `dest`.
///
/// # Safety
///
/// `src` must be valid for reads and `dest` for writes of `n` bytes, and the two must not
/// overlap.
#[unsafe(no_mangle)]
unsafe extern "C" fn memcpy(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    // SAFETY: the caller gives `n` readable bytes at `src` and `n` writable bytes at `dest`;
    // `rep movsb` copies exactly `n` bytes forward, and the direction flag is clear (psABI).
    unsafe {
        asm!(
            "rep movsb",
            inout("rcx") n => _,
            inout("rdi") dest => _,
            inout("rsi") src => _,
            options(nostack),
        );
    }
    dest
}

/// Copies `n` bytes from `src` to `dest`, which may overlap, and returns `dest`.
///
/// # Safety
///
/// `src` must be valid for reads and `dest` for writes of `n` bytes.
#[unsafe(no_mangle)]
unsafe extern "C" fn memmove(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    let (to, from) = (dest as usize, src as usize);
    // A forward copy is safe unless `dest` starts inside `src`'s bytes.
    if to <= from || to - from >= n {
        // SAFETY: as `memcpy`'s: a forward copy never reads a byte it has already written
        // when `dest` does not start inside `[src, src + n)`.
        unsafe { memcpy(dest, src, n) };
        return dest;
    }
    if n == 0 {
        return dest;
    }
    // SAFETY: `n > 0` and `dest` starts inside `src`'s bytes, so copy from the last byte down:
    // RDI and RSI point at the last byte of each, `std` makes `rep movsb` count down, and `cld`
    // leaves the direction flag clear again (psABI). Each byte is read before it is written.
    unsafe {
        asm!(
            "std",
            "rep movsb",
            "cld",
            inout("rcx") n => _,
            inout("rdi") dest.cast::<u8>().wrapping_add(n - 1) => _,
            inout("rsi") src.cast::<u8>().wrapping_add(n - 1) => _,
            options(nostack),
        );
    }
    dest
}

/// Writes the low byte of `c` to `n` bytes at `dest`, and returns `dest`.
///
/// # Safety
///
/// `dest` must be valid for writes of `n` bytes.
#[unsafe(no_mangle)]
unsafe extern "C" fn memset(dest: *mut c_void, c: i32, n: usize) -> *mut c_void {
    // SAFETY: the caller gives `n` writable bytes at `dest`; `rep stosb` writes AL to exactly
    // `n` bytes forward, and the direction flag is clear (psABI).
    unsafe {
        asm!(
            "rep stosb",
            inout("rcx") n => _,
            inout("rdi") dest => _,
            in("al") c as u8,
            options(nostack),
        );
    }
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
    let (a, b) = (a.cast::<u8>(), b.cast::<u8>());
    let mut i = 0;
    while i < n {
        // SAFETY: `i < n`, and the caller gives `n` readable bytes at `a`.
        let x = unsafe { a.wrapping_add(i).read() };
        // SAFETY: `i < n`, and the caller gives `n` readable bytes at `b`.
        let y = unsafe { b.wrapping_add(i).read() };
        if x != y {
            return i32::from(x) - i32::from(y);
        }
        i += 1;
    }
    0
}

/// Zero when the `n` bytes at `a` and `b` are equal, and not zero when they differ.
///
/// # Safety
///
/// `a` and `b` must each be valid for reads of `n` bytes.
#[unsafe(no_mangle)]
unsafe extern "C" fn bcmp(a: *const c_void, b: *const c_void, n: usize) -> i32 {
    // SAFETY: the caller's promise is `memcmp`'s.
    unsafe { memcmp(a, b, n) }
}
