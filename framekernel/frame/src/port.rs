//! x86 port I/O (ADR-0011 N6): claimed ranges of I/O ports, so safe code reaches only the ports
//! it owns.
//!
//! A [`PortRange`] is made by [`PortRange::claim`]: a range can be claimed once, and a range
//! that includes a system-owned port is refused ([`RESERVED`]), because those ports can
//! reprogram interrupts, DMA or the machine's reset line. The system ports that are used
//! (the PIT and speaker gate, the PICs, CMOS) are reached through the framework's own typed
//! interfaces, never through a claim.
//!
//! The raw instructions, [`inb`], [`outb`] and [`ind`], are `unsafe fn`s for the framework's
//! own use (a wrong one can reset the machine, for instance).

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

/// Each word is 64 ports' worth of "claimed" bits.
static CLAIMED: [AtomicU64; 1024] = [const { AtomicU64::new(0) }; 1024];

/// Ports no one may claim: the legacy DMA controllers (they can write memory), the PICs and
/// PIT (the framework's interrupt and timer set-up owns them), port 0x61 (the PIT gate and
/// speaker), CMOS (`Cmos`), the DMA page registers, and PCI configuration and reset.
const RESERVED: &[(u16, u16)] = &[
    (0x000, 0x020), // DMA controller 1
    (0x020, 0x002), // PIC 1
    (0x040, 0x004), // PIT
    (0x061, 0x001), // PIT gate, speaker
    (0x070, 0x002), // CMOS index and data
    (0x080, 0x020), // DMA page registers
    (0x0A0, 0x002), // PIC 2
    (0x0C0, 0x020), // DMA controller 2
    (0xCF8, 0x008), // PCI configuration, reset control
];

/// A claimed range of I/O ports. Reads and writes outside it do nothing (a read returns 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortRange {
    base: u16,
    len: u16,
}

impl PortRange {
    /// Claims `[base, base + len)` for the caller, for good. `None` if it is empty, wraps, is
    /// already claimed, or includes a system-owned port.
    pub fn claim(base: u16, len: u16) -> Option<Self> {
        if len == 0 {
            return None;
        }
        let end = u32::from(base) + u32::from(len);
        if end > 0x1_0000 {
            return None;
        }
        // Refuse the system's ports.
        for &(start, count) in RESERVED {
            let reserved_end = u32::from(start) + u32::from(count);
            if u32::from(base) < reserved_end && end > u32::from(start) {
                return None;
            }
        }
        // Claim every port, or none: a racing claim that overlaps is undone.
        for port in u32::from(base)..end {
            let (word, bit) = (port as usize / 64, 1u64 << (port % 64));
            let owned = CLAIMED
                .get(word)
                .map(|slot| slot.fetch_or(bit, Ordering::AcqRel) & bit == 0);
            if owned != Some(true) {
                for undo in u32::from(base)..port {
                    let (w, b) = (undo as usize / 64, 1u64 << (undo % 64));
                    if let Some(slot) = CLAIMED.get(w) {
                        slot.fetch_and(!b, Ordering::AcqRel);
                    }
                }
                return None;
            }
        }
        Some(Self { base, len })
    }

    /// The first port.
    pub const fn base(self) -> u16 {
        self.base
    }

    fn port(self, offset: u16) -> Option<u16> {
        (offset < self.len).then(|| self.base.wrapping_add(offset))
    }

    /// Reads the byte port at `offset`.
    pub fn read8(self, offset: u16) -> u8 {
        self.port(offset).map_or(0, |port| {
            // SAFETY: CPL 0 (the kernel), and this port belongs to the claimer: a claim excludes
            // the system's ports, whose accesses could reprogram the machine.
            unsafe { inb(port) }
        })
    }

    /// Writes the byte port at `offset`.
    pub fn write8(self, offset: u16, value: u8) {
        if let Some(port) = self.port(offset) {
            // SAFETY: as for `read8`; a claimed port can reach only its own device.
            unsafe { outb(port, value) };
        }
    }

    /// Reads a 32-bit value at `offset` (the ACPI PM timer, for example).
    pub fn read32(self, offset: u16) -> u32 {
        if self.port(offset).is_none() || self.port(offset.wrapping_add(3)).is_none() {
            return 0;
        }
        // SAFETY: as for `read8`; all four ports are this range's (checked).
        unsafe { ind(self.base.wrapping_add(offset)) }
    }
}

/// The CMOS/RTC registers (ports 0x70 and 0x71), which the RTC driver reads: one owner.
#[derive(Debug)]
pub struct Cmos(());

static CMOS_TAKEN: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

impl Cmos {
    /// The CMOS registers, or `None` if they were taken already.
    pub fn take() -> Option<Self> {
        (!CMOS_TAKEN.swap(true, Ordering::AcqRel)).then_some(Self(()))
    }

    /// Reads CMOS register `index` (NMI stays enabled: bit 7 of the index port is clear).
    pub fn read(&mut self, index: u8) -> u8 {
        // SAFETY: CPL 0; port 0x70 selects a CMOS register and 0x71 reads it, which affects
        // only the CMOS, and `Cmos` is the one handle to these ports.
        unsafe { outb(0x70, index & 0x7F) };
        // SAFETY: as above.
        unsafe { inb(0x71) }
    }
}

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
