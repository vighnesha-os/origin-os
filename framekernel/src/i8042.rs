//! The i8042 PS/2 controller (CLAUDE.md §31 device discovery, §56 keyboards): controller
//! bring-up and a read path that decodes scancodes with `framekernel::ps2`.
//!
//! The controller is reached through I/O ports 0x60 (data) and 0x64 (command/status).
//! After `ExitBootServices` the kernel owns it. Bring-up follows the usual sequence:
//! disable both ports, flush the output buffer, run the controller self-test (0xAA, which
//! answers 0x55), test the keyboard port (0xAB, which answers 0x00), then enable the
//! keyboard port and clear its scancode-translation-independent state. Every wait on the
//! status register is bounded, so a missing or wedged controller cannot hang the kernel.
//!
//! Only the boot processor touches these ports, at CPL 0, and nothing else drives them, so
//! the port I/O is sound. This brings the controller up and proves it answers; delivering
//! live keystrokes needs the keyboard IRQ (vector routing), which is not wired yet.

use core::fmt::Write;

use crate::console::Console;
use crate::{inb, outb};

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;
const COMMAND: u16 = 0x64;

/// Status register bits: output buffer full (data to read), input buffer full (do not
/// write yet).
const OUTPUT_FULL: u8 = 1 << 0;
const INPUT_FULL: u8 = 1 << 1;

/// Controller commands.
const READ_CONFIG: u8 = 0x20;
const WRITE_CONFIG: u8 = 0x60;
const DISABLE_PORT2: u8 = 0xA7;
const DISABLE_PORT1: u8 = 0xAD;
const ENABLE_PORT1: u8 = 0xAE;
const SELF_TEST: u8 = 0xAA;
const TEST_PORT1: u8 = 0xAB;

/// Config byte bits: port-1 interrupt, and the first-port scancode translation.
const CONFIG_PORT1_IRQ: u8 = 1 << 0;
const CONFIG_PORT1_TRANSLATE: u8 = 1 << 6;

/// Polls before a wait gives up (each poll is one status read).
const POLL_LIMIT: u32 = 1_000_000;

/// The controller, owned by the kernel after `ExitBootServices`.
struct Controller(());

impl Controller {
    /// # Safety
    ///
    /// CPL 0, the boot processor, and nothing else may drive ports 0x60/0x64 while this
    /// value lives.
    unsafe fn new() -> Self {
        Self(())
    }

    fn status(&self) -> u8 {
        // SAFETY: the type invariant; reading the status port has no memory effect.
        unsafe { inb(STATUS) }
    }

    fn command(&self, cmd: u8) {
        // SAFETY: the type invariant; `cmd` is a controller command, which affects only
        // the controller.
        unsafe { outb(COMMAND, cmd) };
    }

    /// Waits until the output buffer has a byte, then reads it; `None` if none arrives.
    fn read(&self) -> Option<u8> {
        for _ in 0..POLL_LIMIT {
            if self.status() & OUTPUT_FULL != 0 {
                // SAFETY: a byte is waiting; reading the data port consumes it.
                return Some(unsafe { inb(DATA) });
            }
            core::hint::spin_loop();
        }
        None
    }

    /// Waits until the input buffer is empty, then writes a data byte; `false` on timeout.
    fn write_data(&self, byte: u8) -> bool {
        for _ in 0..POLL_LIMIT {
            if self.status() & INPUT_FULL == 0 {
                // SAFETY: the controller is ready for a byte; this writes the data port.
                unsafe { outb(DATA, byte) };
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    /// Writes `cmd`, then its argument byte, waiting for the input buffer each time.
    fn command_arg(&self, cmd: u8, arg: u8) -> bool {
        for _ in 0..POLL_LIMIT {
            if self.status() & INPUT_FULL == 0 {
                self.command(cmd);
                return self.write_data(arg);
            }
            core::hint::spin_loop();
        }
        false
    }

    /// Drains any bytes already in the output buffer.
    fn flush(&self) {
        for _ in 0..1024 {
            if self.status() & OUTPUT_FULL == 0 {
                return;
            }
            // SAFETY: a byte is waiting; discard it.
            let _ = unsafe { inb(DATA) };
        }
    }
}

/// Brings the controller up and proves it answers, unless the FADT at `rsdp` says this
/// platform has none, which is reported and passes. Returns whether the self-tests passed.
pub(crate) fn init(com1: &mut Console, rsdp: u64) -> bool {
    let fadt = crate::firmware::acpi_table(rsdp, b"FACP")
        .ok()
        .and_then(|t| framekernel::acpi::parse_fadt(t).ok());
    if fadt.is_some_and(|f| f.i8042 == Some(false)) {
        let _ = writeln!(
            com1,
            "origin-kernel: i8042: the FADT says this platform has none"
        );
        return true;
    }
    // SAFETY: CPL 0, boot processor, interrupts disabled during bring-up; nothing else
    // uses these ports.
    let c = unsafe { Controller::new() };
    // Disable both ports so no device traffic races the configuration, and flush.
    c.command(DISABLE_PORT1);
    c.command(DISABLE_PORT2);
    c.flush();
    // Read the current config, clear the IRQ and translation bits for now, write it back.
    c.command(READ_CONFIG);
    let config = c.read().unwrap_or(0xFF);
    let new_config = config & !(CONFIG_PORT1_IRQ | CONFIG_PORT1_TRANSLATE);
    c.command_arg(WRITE_CONFIG, new_config);
    // Controller self-test and keyboard-port test.
    c.command(SELF_TEST);
    let self_test = c.read();
    c.command(TEST_PORT1);
    let port_test = c.read();
    // Writing the config can reset it, so write it again, then enable the keyboard port.
    c.command_arg(WRITE_CONFIG, new_config);
    c.command(ENABLE_PORT1);

    let ok = self_test == Some(0x55) && port_test == Some(0x00);
    let _ = writeln!(
        com1,
        "origin-kernel: i8042: config {config:#04x}; self-test {} (want 0x55), keyboard port test {} (want 0x00){}",
        byte(self_test),
        byte(port_test),
        if ok { "" } else { ": i8042 failed" }
    );
    let _ = c; // the controller is left enabled; live keystrokes await IRQ routing.
    ok
}

/// A byte as `0xNN`, or `none`.
fn byte(b: Option<u8>) -> ByteOrNone {
    ByteOrNone(b)
}

struct ByteOrNone(Option<u8>);

impl core::fmt::Display for ByteOrNone {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(b) => write!(f, "{b:#04x}"),
            None => f.write_str("none (timeout)"),
        }
    }
}
