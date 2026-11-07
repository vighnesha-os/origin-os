//! The text the Framekernel prints on COM1.
//!
//! `cargo xtask boot-test` matches these lines, so they are written in one place and pinned
//! by unit tests. Every line starts with [`PREFIX`].

use core::fmt::{self, Write};

use origin_boot_info::BootInfo;

use crate::boot::{InvalidBootInfo, MemorySummary};

/// Start of every line the kernel prints.
pub const PREFIX: &str = "origin-kernel: ";
/// The last line of a successful M1 boot.
pub const HALT_LINE: &str = "origin-kernel: M1 entry complete; halting";
/// Start of the line printed when the boot info is rejected.
pub const INVALID_BOOT_INFO: &str = "origin-kernel: invalid boot info: ";
/// Start of the line the panic handler prints.
pub const PANIC: &str = "origin-kernel: panic: ";
/// Printed, without `core::fmt`, when the loader did not apply the image's relocations.
/// Formatting needs relocated pointers (vtables and string tables), so this text is passed
/// to `write_str` as it is.
pub const RELOCATIONS_NOT_APPLIED: &str = "origin-kernel: invalid image: relocations not applied\n";

/// Prints the banner and the boot-info summary. `_start` prints [`HALT_LINE`] last, after
/// the CPU tables are loaded and exceptions are proven ([`write_halt`]).
///
/// The release identity is generated from `origin-version.toml` (CLAUDE.md §62).
pub fn write_report<W: Write>(out: &mut W, info: &BootInfo, memory: &MemorySummary) -> fmt::Result {
    writeln!(
        out,
        "{PREFIX}{} {} Framekernel (build {})",
        origin_version::PRODUCT,
        origin_version::VERSION,
        origin_version::BUILD
    )?;
    writeln!(
        out,
        "{PREFIX}boot info {}.{}, {} memory regions, {} MiB usable",
        info.version_major,
        info.version_minor,
        info.region_count,
        memory.usable_mib()
    )?;
    writeln!(
        out,
        "{PREFIX}loaded at {:#x}, span {:#x} bytes",
        info.kernel_base, info.kernel_span
    )?;
    if info.acpi_rsdp == 0 {
        writeln!(out, "{PREFIX}no ACPI RSDP")?;
    } else {
        writeln!(out, "{PREFIX}ACPI RSDP at {:#x}", info.acpi_rsdp)?;
    }
    Ok(())
}

/// Printed once the GDT, TSS and IDT are loaded (the first M2 step).
pub const CPU_TABLES_LINE: &str = "origin-kernel: GDT, TSS and IDT loaded (32 exception vectors)";

/// Prints [`CPU_TABLES_LINE`].
pub fn write_cpu_tables<W: Write>(out: &mut W) -> fmt::Result {
    writeln!(out, "{CPU_TABLES_LINE}")
}

/// Prints [`HALT_LINE`], the last line of a successful boot.
pub fn write_halt<W: Write>(out: &mut W) -> fmt::Result {
    writeln!(out, "{HALT_LINE}")
}

/// Prints why the boot info was rejected.
pub fn write_invalid<W: Write>(out: &mut W, err: InvalidBootInfo) -> fmt::Result {
    writeln!(out, "{INVALID_BOOT_INFO}{err:?}")
}

/// Prints a panic report.
pub fn write_panic<W: Write, D: fmt::Display>(out: &mut W, info: D) -> fmt::Result {
    writeln!(out, "{PANIC}{info}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use origin_boot_info::{BootInfoError, MAGIC, VERSION};

    fn info(acpi_rsdp: u64) -> BootInfo {
        BootInfo {
            magic: MAGIC,
            version_major: VERSION.0,
            version_minor: VERSION.1,
            size: 96,
            regions: 0x8000,
            region_count: 42,
            kernel_base: 0x7e5_0000,
            kernel_span: 0x1_3000,
            acpi_rsdp,
            uefi_system_table: 0,
            stack_base: 0x40_0000,
            stack_size: 0x1_0000,
            kernel_segments: 0x9000,
            kernel_segment_count: 3,
        }
    }

    #[test]
    fn report_lines_are_exact() {
        let mut out = String::new();
        let memory = MemorySummary {
            usable_pages: 256 * 2014 + 255,
        };
        write_report(&mut out, &info(0x7fb7_e014), &memory).unwrap();
        let expected = format!(
            "origin-kernel: {} {} Framekernel (build {})\n\
             origin-kernel: boot info {}.{}, 42 memory regions, 2014 MiB usable\n\
             origin-kernel: loaded at 0x7e50000, span 0x13000 bytes\n\
             origin-kernel: ACPI RSDP at 0x7fb7e014\n",
            origin_version::PRODUCT,
            origin_version::VERSION,
            origin_version::BUILD,
            VERSION.0,
            VERSION.1
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn report_says_when_there_is_no_rsdp() {
        let mut out = String::new();
        write_report(&mut out, &info(0), &MemorySummary { usable_pages: 0 }).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[3], "origin-kernel: no ACPI RSDP");
        let mut tail = String::new();
        write_cpu_tables(&mut tail).unwrap();
        write_halt(&mut tail).unwrap();
        assert_eq!(tail, format!("{CPU_TABLES_LINE}\n{HALT_LINE}\n"));
        assert!(lines.iter().all(|l| l.starts_with(PREFIX)));
    }

    #[test]
    fn failure_lines_are_exact() {
        let mut out = String::new();
        write_invalid(&mut out, InvalidBootInfo::Header(BootInfoError::BadMagic)).unwrap();
        write_invalid(&mut out, InvalidBootInfo::Null).unwrap();
        write_panic(&mut out, "boom").unwrap();
        assert_eq!(
            out,
            "origin-kernel: invalid boot info: Header(BadMagic)\n\
             origin-kernel: invalid boot info: Null\n\
             origin-kernel: panic: boom\n"
        );
        assert!(RELOCATIONS_NOT_APPLIED.starts_with(PREFIX));
        assert!(RELOCATIONS_NOT_APPLIED.ends_with('\n'));
    }
}
