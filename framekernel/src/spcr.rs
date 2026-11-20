//! The ACPI Serial Port Console Redirection table (ADR-0010): where the firmware says its serial
//! console is, and how it is set. The parser, its checks and its soak live in `origin-spcr`,
//! which the UEFI loader builds too, so that both read the table by the same rules (ADR-0010
//! Amendment 3; decision item 317; TASKS item 84). This module re-exports it under the name the
//! kernel has always used.

pub use origin_spcr::*;
