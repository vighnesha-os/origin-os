//! Bench builds only (TASKS item 112): what the bench loader says about the hand-over, and where.
//!
//! Rentals B and C showed nothing between the loader's last line and the kernel's first. A bench
//! loader says more, around that gap:
//! - before `ExitBootServices`, on the firmware's console, the processor state the kernel
//!   inherits ([`Registers`], then [`Shadow`]), reading only the registers CPUID says exist
//!   ([`reads`]);
//! - after it, straight to the UART boot info names, which call succeeded ([`Exited`]), through
//!   `crate::uart::Uart`, which calls no firmware service (ADR-0002 B5). Every loader, bench or
//!   not, writes there why it halts when both calls fail (`crate::uart::Halting`, decision item
//!   342);
//! - for the QEMU scenarios, `ExitBootServices` given a stale map key once or twice
//!   ([`stale_calls`], [`key_for`]), so the retry, and the halt, run under real firmware code.
//!
//! The entry stub and the kernel write one byte after each step (`RS12345678`). This module exists
//! only under `cfg(origin_bench)`, and in the host tests; no image a normal build makes holds it
//! (`cargo xtask check`, F1 and F2).

use core::fmt;

use crate::uart::ExitCalls;

/// How every line the bench loader adds begins: the text F2 looks for in images.
pub const PREFIX: &str = "origin-boot: bench build: ";

/// The file on the loader's volume that asks for stale map keys, in QEMU scenarios only.
pub const STALE_KEY_PATH: &str = "\\EFI\\ORIGIN\\BENCH-STALE-KEY";

/// Which model-specific registers the state line may read. Intel's SDM Volume 4 (Table 2-2) and
/// AMD's APM Volume 2 (§7.8.3, §18.12) tie them to the same bits, so the decision does not depend
/// on the vendor: `IA32_PAT` (0x277) to CPUID 01H:EDX[16]; `S_CET` (0x6A2), `PL0_SSP` (0x6A4)
/// and the interrupt SSP table address (0x6A8) to CPUID 07H.0:ECX[7], CET_SS. Intel also defines
/// `S_CET` under CET_IBT alone, but not the other two, so all three wait for CET_SS. A register
/// read where it does not exist would take a #GP under the firmware's IDT before any breadcrumb.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reads {
    /// `IA32_PAT` exists.
    pub pat: bool,
    /// The three supervisor shadow-stack registers exist.
    pub shadow_stacks: bool,
}

/// [`Reads`] from CPUID: `max_basic_leaf` is leaf 0's EAX, `leaf1_edx` leaf 1's EDX, and
/// `leaf7_ecx` leaf 7 sub-leaf 0's ECX (ignored unless leaf 7 exists).
pub const fn reads(max_basic_leaf: u32, leaf1_edx: u32, leaf7_ecx: u32) -> Reads {
    Reads {
        pat: max_basic_leaf >= 1 && leaf1_edx & (1 << 16) != 0,
        shadow_stacks: max_basic_leaf >= 7 && leaf7_ecx & (1 << 7) != 0,
    }
}

/// The supervisor shadow-stack registers, where they exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShadowStacks {
    /// `S_CET`: bit 0 turns supervisor shadow stacks on.
    pub s_cet: u64,
    /// `PL0_SSP`.
    pub pl0_ssp: u64,
    /// The interrupt SSP table's address (`IA32_INTERRUPT_SSP_TABLE_ADDR`, AMD's `ISST_ADDR`).
    pub ssp_table: u64,
}

/// The processor state the kernel inherits from the firmware, which the entry stub changes only
/// in EFER.NXE, CR0.WP and CR3 (ADR-0002 B6): the registers every processor has, and PAT where it
/// exists. The state line's first part; [`Shadow`] ends it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Registers {
    /// CR0.
    pub cr0: u64,
    /// CR4.
    pub cr4: u64,
    /// EFER (0xC000_0080), which every x86-64 processor has.
    pub efer: u64,
    /// `IA32_PAT`, where [`Reads::pat`].
    pub pat: Option<u64>,
}

impl fmt::Display for Registers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "entry state: CR0 {:#x}, CR4 {:#x}, EFER {:#x}, ",
            self.cr0, self.cr4, self.efer
        )?;
        match self.pat {
            Some(pat) => write!(f, "PAT {pat:#x}; "),
            None => f.write_str("PAT not reported (CPUID 01H:EDX[16] = 0); "),
        }
    }
}

/// The state line's last part: the shadow-stack registers, where [`Reads::shadow_stacks`]. The
/// loader prints [`Registers`] before it reads these, so a fault here leaves those on the console.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shadow(pub Option<ShadowStacks>);

impl fmt::Display for Shadow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(s) => write!(
                f,
                "shadow stacks: S_CET {:#x}, PL0_SSP {:#x}, SSP table {:#x} (CPUID 07H.0:ECX[7] = 1)",
                s.s_cet, s.pl0_ssp, s.ssp_table
            ),
            None => f.write_str("shadow stacks: not reported (CPUID 07H.0:ECX[7] = 0)"),
        }
    }
}

/// The bench loader's line on the SPCR's UART once `ExitBootServices` succeeded: which call
/// succeeded, and whether a stale key was asked for ([`stale_calls`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exited {
    /// The calls made.
    pub calls: ExitCalls,
    /// Calls given a stale key: 0, 1 or 2.
    pub stale: u8,
}

impl fmt::Display for Exited {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(PREFIX)?;
        match (self.calls.first, self.calls.retry) {
            (Some(_), None) => f.write_str("boot services exited on the first call")?,
            (Some(first), Some(_)) => write!(
                f,
                "boot services exited on the retry; the first call returned {first:?}"
            )?,
            (None, _) => f.write_str("boot services exited, no call recorded")?,
        }
        match self.stale {
            0 => Ok(()),
            1 => f.write_str(" [the first call was given a stale key: BENCH-STALE-KEY]"),
            _ => f.write_str(" [both calls were given a stale key: BENCH-STALE-KEY]"),
        }
    }
}

/// How many `ExitBootServices` calls [`STALE_KEY_PATH`]'s contents ask to be given a stale key:
/// `1` or `2`, with any whitespace after it. Anything else asks for none.
pub fn stale_calls(contents: &[u8]) -> Option<u8> {
    match contents.trim_ascii() {
        b"1" => Some(1),
        b"2" => Some(2),
        _ => None,
    }
}

/// The key call `call` (0 for the first, 1 for the retry) passes to `ExitBootServices`: the one
/// the firmware gave, or, for the first `stale` calls, that key plus one. A key that is not the
/// current one is refused with `EFI_INVALID_PARAMETER` (UEFI 2.11 §7.4).
pub const fn key_for(call: u8, stale: u8, key: usize) -> usize {
    if call < stale {
        key.wrapping_add(1)
    } else {
        key
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uefi::Status;

    /// CPUID values for the gating tests: leaf 0's EAX, leaf 1's EDX, leaf 7.0's ECX and EDX.
    struct Processor {
        name: &'static str,
        max_basic_leaf: u32,
        leaf1_edx: u32,
        leaf7_ecx: u32,
    }

    #[test]
    fn amd_registers_are_read_only_where_cpuid_says_they_exist() {
        for (p, want) in [
            // The rented board, AMD EPYC 4244P (Zen 4), from rental C's dump: PAT and CET_SS.
            (
                Processor {
                    name: "AMD Zen 4, rented",
                    max_basic_leaf: 0x10,
                    leaf1_edx: 0x178B_FBFF,
                    leaf7_ecx: 0x0040_5FDE,
                },
                Reads {
                    pat: true,
                    shadow_stacks: true,
                },
            ),
            // An AMD processor without shadow stacks (CET_SS clear), PAT present.
            (
                Processor {
                    name: "AMD, no CET_SS",
                    max_basic_leaf: 0x10,
                    leaf1_edx: 0x178B_FBFF,
                    leaf7_ecx: 0x0040_5F5E,
                },
                Reads {
                    pat: true,
                    shadow_stacks: false,
                },
            ),
            // A leaf-7 value with bit 7 set behind a maximum leaf below 7: not read.
            (
                Processor {
                    name: "AMD, leaf 7 absent",
                    max_basic_leaf: 6,
                    leaf1_edx: 0x078B_FBFF,
                    leaf7_ecx: 0x80,
                },
                Reads {
                    pat: true,
                    shadow_stacks: false,
                },
            ),
        ] {
            assert_eq!(
                reads(p.max_basic_leaf, p.leaf1_edx, p.leaf7_ecx),
                want,
                "{}",
                p.name
            );
        }
    }

    #[test]
    fn intel_registers_are_read_only_where_cpuid_says_they_exist() {
        for (p, want) in [
            // CET_SS set: all three shadow-stack registers exist (SDM Vol. 4, Table 2-2).
            (
                Processor {
                    name: "Intel, CET_SS",
                    max_basic_leaf: 0x23,
                    leaf1_edx: 0xBFEB_FBFF,
                    leaf7_ecx: 0x1840_07EC | 0x80,
                },
                Reads {
                    pat: true,
                    shadow_stacks: true,
                },
            ),
            // CET_IBT alone (leaf 7 EDX[20], not looked at): S_CET exists then on Intel, but
            // PL0_SSP and the SSP table do not, so none of the three is read.
            (
                Processor {
                    name: "Intel, CET_IBT without CET_SS",
                    max_basic_leaf: 0x23,
                    leaf1_edx: 0xBFEB_FBFF,
                    leaf7_ecx: 0x1840_076C,
                },
                Reads {
                    pat: true,
                    shadow_stacks: false,
                },
            ),
            // No PAT bit: IA32_PAT is not read.
            (
                Processor {
                    name: "Intel, no PAT",
                    max_basic_leaf: 0x0D,
                    leaf1_edx: 0xBFEB_FBFF & !(1 << 16),
                    leaf7_ecx: 0,
                },
                Reads {
                    pat: false,
                    shadow_stacks: false,
                },
            ),
        ] {
            assert_eq!(
                reads(p.max_basic_leaf, p.leaf1_edx, p.leaf7_ecx),
                want,
                "{}",
                p.name
            );
        }
    }

    #[test]
    fn the_state_line_names_what_it_did_not_read() {
        let line = |registers: Registers, shadow: Shadow| format!("{registers}{shadow}");
        let registers = Registers {
            cr0: 0x8000_0033,
            cr4: 0x0036_06F8,
            efer: 0xD01,
            pat: Some(0x0007_0406_0007_0406),
        };
        let none = ShadowStacks {
            s_cet: 0,
            pl0_ssp: 0,
            ssp_table: 0,
        };
        assert_eq!(
            line(registers, Shadow(Some(none))),
            "entry state: CR0 0x80000033, CR4 0x3606f8, EFER 0xd01, PAT 0x7040600070406; shadow stacks: S_CET 0x0, PL0_SSP 0x0, SSP table 0x0 (CPUID 07H.0:ECX[7] = 1)"
        );
        assert_eq!(
            line(
                Registers {
                    pat: None,
                    ..registers
                },
                Shadow(None)
            ),
            "entry state: CR0 0x80000033, CR4 0x3606f8, EFER 0xd01, PAT not reported (CPUID 01H:EDX[16] = 0); shadow stacks: not reported (CPUID 07H.0:ECX[7] = 0)"
        );
    }

    #[test]
    fn the_exit_lines() {
        let mut calls = ExitCalls::default();
        calls.record(Status::SUCCESS);
        assert_eq!(
            Exited { calls, stale: 0 }.to_string(),
            "origin-boot: bench build: boot services exited on the first call"
        );
        let mut calls = ExitCalls::default();
        calls.record(Status::INVALID_PARAMETER);
        calls.record(Status::SUCCESS);
        assert_eq!(
            Exited { calls, stale: 1 }.to_string(),
            "origin-boot: bench build: boot services exited on the retry; the first call returned EFI_INVALID_PARAMETER [the first call was given a stale key: BENCH-STALE-KEY]"
        );
    }

    #[test]
    fn the_stale_key_switch() {
        assert_eq!(stale_calls(b"1"), Some(1));
        assert_eq!(stale_calls(b"2\n"), Some(2));
        assert_eq!(stale_calls(b" 2 \r\n"), Some(2));
        for other in [&b""[..], b"0", b"3", b"12", b"x", b"1 2"] {
            assert_eq!(stale_calls(other), None, "{other:?}");
        }
        assert_eq!(key_for(0, 0, 7), 7);
        assert_eq!(key_for(0, 1, 7), 8);
        assert_eq!(key_for(1, 1, 7), 7);
        assert_eq!(key_for(1, 2, 7), 8);
        assert_eq!(key_for(0, 2, usize::MAX), 0);
        let mut calls = ExitCalls::default();
        assert_eq!(calls.made(), 0);
        calls.record(Status::INVALID_PARAMETER);
        assert_eq!(calls.made(), 1);
        calls.record(Status::SUCCESS);
        assert_eq!(calls.made(), 2);
    }
}
