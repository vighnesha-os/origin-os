//! Bench builds only (TASKS item 112): the kernel's breadcrumbs. One byte goes to the UART the
//! bench loader handed over after each step from `_start` to the kernel's first line, so a boot
//! that stops before that line shows the step it stopped after: `1` to `8`, then CR LF.
//!
//! The bench entry stub hands the port over in RSI (`origin_boot_info::bench`), and `_start`
//! leaves RSI for `kernel_main`, which passes it to [`start`] first. A register without the magic,
//! which is what a normal loader leaves, means no breadcrumbs, and the boot goes on unchanged.
//! Nothing here formats, calls through a trait object or reads a pointer stored in data, so the
//! breadcrumbs work before the kernel has confirmed its relocations (step 4).
//!
//! This module exists only under `cfg(origin_bench)`; no image a normal build makes holds it
//! (`cargo xtask check`, F1 and F2).
#![forbid(unsafe_code)]

use core::fmt;
use core::sync::atomic::{AtomicU16, Ordering};

/// The breadcrumbs' port, or 0: none.
static PORT: AtomicU16 = AtomicU16::new(0);

/// Takes the register the bench entry stub handed over. A port is kept only if the register
/// carries the magic and none of the UART's eight registers is a system port.
pub fn start(register: u64) {
    if let Some(port) = origin_boot_info::bench::port(register)
        && port.checked_add(7).is_some()
        && !crate::port::reserved(port, 8)
    {
        PORT.store(port, Ordering::Relaxed);
    }
}

/// The breadcrumbs' port, if one was handed over.
pub fn port() -> Option<u16> {
    match PORT.load(Ordering::Relaxed) {
        0 => None,
        port => Some(port),
    }
}

/// Writes `bytes` to the breadcrumbs' port, if there is one; nothing otherwise.
pub fn crumb(bytes: &[u8]) {
    if let Some(port) = port() {
        crate::port::bench_write(port, bytes);
    }
}

/// What the bench kernel says, once its console runs, about where its breadcrumbs went.
#[derive(Clone, Copy, Debug)]
pub struct Said;

impl fmt::Display for Said {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match port() {
            Some(port) => write!(f, "breadcrumbs went to I/O port {port:#x}"),
            None => f.write_str("no breadcrumbs: the loader handed over no port"),
        }
    }
}
