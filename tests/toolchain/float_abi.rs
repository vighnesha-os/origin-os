//! Float-ABI probe (research experiment E2; CLAUDE.md §11).
//!
//! On `x86_64-unknown-none` this multiply becomes a call to the soft-float routine
//! `__muldf3`, which is why Origin userspace is not built for that target. On
//! `x86_64-unknown-linux-gnu` with `-C target-cpu=x86-64-v3` it is a single `vmulsd`.
#![no_std]

#[unsafe(no_mangle)]
pub extern "C" fn scale(x: f64) -> f64 {
    x * 2.5
}
