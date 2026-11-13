//! The bodies of the ring-3 runtime's `memcpy`, `memmove`, `memset`, `memcmp` and `bcmp`
//! (`runtime/rt`, task S4-1), kept apart from the runtime so the host can test them (S4-1's
//! review, finding F6): the runtime cannot be built for the host, where `std` has a panic
//! handler of its own. The runtime's `mem*` functions call these, and nothing else.
//!
//! On x86-64 a forward copy is one `rep movsb` and a fill one `rep stosb`. A backward copy, for
//! a move whose destination starts inside its source, sets the direction flag, runs `rep movsb`
//! from the last byte down and clears the flag again, so it is clear on return, as the psABI
//! requires. Comparisons are a byte loop. The crate is `no_builtins`, so the compiler cannot turn
//! any of these into a call to the function it implements.
//!
//! How fast the string instructions run is the processor's, and nothing here measures it.
//! Processors that report ERMS or FSRM (CPUID leaf 7, sub-leaf 0: EBX bit 9 and EDX bit 4) run
//! `rep movsb` and `rep stosb` fast for long and for short lengths; neither feature is part of
//! the x86-64-v3 baseline, so a baseline processor without them runs these correctly and maybe
//! slowly. The backward copy, with the direction flag set, gets no fast-string handling.
//!
//! On any other architecture, which only a host test runs on (the development Mac), each string
//! instruction is replaced by a loop that does what the instruction does, byte by byte, in the
//! same direction. There the tests check what is Origin's own: which way a move copies, and where
//! a backward copy starts. On an x86-64 host (CI) they check the instructions themselves.
#![no_std]
// These are the bodies of `memcpy`, `memmove`, `memset` and `memcmp`: the compiler must not
// compile any of them into a call to itself.
#![no_builtins]

#[cfg(test)]
extern crate std;

/// Copies `n` bytes from `src` to `dest`, from the first byte up.
///
/// # Safety
///
/// `src` must be valid for reads and `dest` for writes of `n` bytes, and `dest` must not start
/// inside `src`'s bytes after the first of them: a copy up never reads a byte it has written
/// then.
#[inline]
pub unsafe fn copy_up(dest: *mut u8, src: *const u8, n: usize) {
    // SAFETY: the caller's promise.
    unsafe { string::move_up(dest, src, n) }
}

/// Copies `n` bytes from `src` to `dest`, which may overlap: up from the first byte unless
/// `dest` starts inside `src`'s bytes after the first, and then down from the last.
///
/// # Safety
///
/// `src` must be valid for reads and `dest` for writes of `n` bytes.
#[inline]
pub unsafe fn copy(dest: *mut u8, src: *const u8, n: usize) {
    let (to, from) = (dest.addr(), src.addr());
    if to <= from || to - from >= n {
        // SAFETY: the caller's promise, and `dest` does not start inside `src`'s bytes after the
        // first of them.
        unsafe { copy_up(dest, src, n) };
        return;
    }
    // Here `n > to - from > 0`, so the last byte of each is `n - 1` on: a copy down reads each
    // byte of `src` before the copy writes over it.
    let last = n - 1;
    // SAFETY: the caller's promise for `n` bytes, from the last byte of each down.
    unsafe { string::move_down(dest.wrapping_add(last), src.wrapping_add(last), n) }
}

/// Writes `byte` to the `n` bytes at `dest`.
///
/// # Safety
///
/// `dest` must be valid for writes of `n` bytes.
#[inline]
pub unsafe fn fill(dest: *mut u8, byte: u8, n: usize) {
    // SAFETY: the caller's promise.
    unsafe { string::store_up(dest, byte, n) }
}

/// Compares the `n` bytes at `a` and `b` as unsigned bytes: the first byte that differs, in `a`
/// less the one in `b`, or zero when none does.
///
/// # Safety
///
/// `a` and `b` must each be valid for reads of `n` bytes.
#[inline]
#[must_use]
pub unsafe fn compare(a: *const u8, b: *const u8, n: usize) -> i32 {
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

/// The string instructions, each run with the direction flag clear on entry (psABI) and left
/// clear on return.
#[cfg(target_arch = "x86_64")]
mod string {
    use core::arch::asm;

    /// `rep movsb` with the direction flag clear: `n` bytes from `src` to `dest`, ascending.
    ///
    /// # Safety
    ///
    /// As [`super::copy_up`]'s.
    #[inline(always)]
    pub(super) unsafe fn move_up(dest: *mut u8, src: *const u8, n: usize) {
        // SAFETY: the caller gives `n` readable bytes at `src` and `n` writable bytes at `dest`;
        // `rep movsb` copies exactly `n` bytes up, as the direction flag is clear (psABI).
        unsafe {
            asm!(
                "rep movsb",
                inout("rcx") n => _,
                inout("rdi") dest => _,
                inout("rsi") src => _,
                options(nostack),
            );
        }
    }

    /// `std; rep movsb; cld`: `n` bytes, from the byte at `last_src` down, to the byte at
    /// `last_dest` down. The direction flag is clear again on return.
    ///
    /// # Safety
    ///
    /// The `n` bytes ending at `last_src` must be readable and the `n` ending at `last_dest`
    /// writable, and `last_dest` must not be below `last_src` where they overlap.
    #[inline(always)]
    pub(super) unsafe fn move_down(last_dest: *mut u8, last_src: *const u8, n: usize) {
        // SAFETY: the caller gives the `n` bytes ending at each; `std` makes `rep movsb` count
        // RDI and RSI down from there, and `cld` clears the flag again (psABI). Each byte is read
        // before it is written, as `last_dest` is not below `last_src`.
        unsafe {
            asm!(
                "std",
                "rep movsb",
                "cld",
                inout("rcx") n => _,
                inout("rdi") last_dest => _,
                inout("rsi") last_src => _,
                options(nostack),
            );
        }
    }

    /// `rep stosb` with the direction flag clear: `byte` to the `n` bytes at `dest`.
    ///
    /// # Safety
    ///
    /// As [`super::fill`]'s.
    #[inline(always)]
    pub(super) unsafe fn store_up(dest: *mut u8, byte: u8, n: usize) {
        // SAFETY: the caller gives `n` writable bytes at `dest`; `rep stosb` writes AL to
        // exactly `n` bytes up, as the direction flag is clear (psABI).
        unsafe {
            asm!(
                "rep stosb",
                inout("rcx") n => _,
                inout("rdi") dest => _,
                in("al") byte,
                options(nostack),
            );
        }
    }
}

/// What each string instruction does, byte by byte, in the same direction: only a host test on
/// another architecture runs this (the module documentation).
#[cfg(not(target_arch = "x86_64"))]
mod string {
    /// As the x86-64 `move_up`: byte `i` of `src` to byte `i` of `dest`, `i` going up.
    ///
    /// # Safety
    ///
    /// As [`super::copy_up`]'s.
    #[inline(always)]
    pub(super) unsafe fn move_up(dest: *mut u8, src: *const u8, n: usize) {
        let mut i = 0;
        while i < n {
            // SAFETY: `i < n`, and the caller gives `n` readable bytes at `src`.
            let byte = unsafe { src.wrapping_add(i).read() };
            // SAFETY: `i < n`, and the caller gives `n` writable bytes at `dest`.
            unsafe { dest.wrapping_add(i).write(byte) };
            i += 1;
        }
    }

    /// As the x86-64 `move_down`: the byte `i` below `last_src` to the byte `i` below
    /// `last_dest`, `i` going up.
    ///
    /// # Safety
    ///
    /// As the x86-64 `move_down`'s.
    #[inline(always)]
    pub(super) unsafe fn move_down(last_dest: *mut u8, last_src: *const u8, n: usize) {
        let mut i = 0;
        while i < n {
            // SAFETY: `i < n`, and the caller gives the `n` readable bytes ending at `last_src`.
            let byte = unsafe { last_src.wrapping_sub(i).read() };
            // SAFETY: `i < n`, and the caller gives the `n` writable bytes ending at `last_dest`.
            unsafe { last_dest.wrapping_sub(i).write(byte) };
            i += 1;
        }
    }

    /// As the x86-64 `store_up`.
    ///
    /// # Safety
    ///
    /// As [`super::fill`]'s.
    #[inline(always)]
    pub(super) unsafe fn store_up(dest: *mut u8, byte: u8, n: usize) {
        let mut i = 0;
        while i < n {
            // SAFETY: `i < n`, and the caller gives `n` writable bytes at `dest`.
            unsafe { dest.wrapping_add(i).write(byte) };
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests;
