//! The UEFI console as a `core::fmt::Write` sink (UEFI 2.11 §12.4).

use core::fmt;

use crate::text;
use crate::uefi::{SimpleTextOutputProtocol, Status, TextString};

/// Writes text through `EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL.OutputString`.
///
/// Usable only while boot services are active. The loader must stop using every `ConOut`,
/// including the panic handler's, before it calls `ExitBootServices` (M1 step 2).
#[derive(Debug)]
pub struct ConOut {
    protocol: *mut SimpleTextOutputProtocol,
    output_string: TextString,
    last_error: Option<Status>,
}

impl ConOut {
    /// Wraps a console output protocol. Returns `None` if `protocol` is null or misaligned,
    /// or if its `OutputString` member is null.
    ///
    /// # Safety
    ///
    /// `protocol` must be null or point to a live `EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL`
    /// instance from a validated system table, and the returned value must only be used
    /// on the boot processor while boot services are active.
    pub unsafe fn new(protocol: *mut SimpleTextOutputProtocol) -> Option<Self> {
        if protocol.is_null() || !protocol.is_aligned() {
            return None;
        }
        // SAFETY: `protocol` is non-null and aligned, and the caller guarantees it points
        // to a live protocol instance. Only the `OutputString` field is read.
        let output_string = unsafe { (*protocol).output_string }?;
        Some(Self {
            protocol,
            output_string,
            last_error: None,
        })
    }

    /// The firmware status of the last failed `OutputString` call, if any.
    pub fn last_error(&self) -> Option<Status> {
        self.last_error
    }

    /// Writes already-encoded `CHAR16` units, such as the firmware vendor string.
    pub fn write_char16(&mut self, units: &[u16]) -> fmt::Result {
        text::encode_units(units, |chunk| self.output(chunk))
    }

    /// Passes one NUL-terminated chunk to `OutputString`.
    fn output(&mut self, chunk: &[u16]) -> fmt::Result {
        if chunk.last() != Some(&0) {
            return Err(fmt::Error);
        }
        // SAFETY: `self.protocol` and `self.output_string` came from a live protocol
        // instance (see `new`), and `chunk` is NUL-terminated and outlives the call.
        let status = unsafe { (self.output_string)(self.protocol, chunk.as_ptr()) };
        // Warnings such as EFI_WARN_UNKNOWN_GLYPH still printed the rest of the string.
        if status.is_error() {
            self.last_error = Some(status);
            return Err(fmt::Error);
        }
        Ok(())
    }
}

impl fmt::Write for ConOut {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        text::encode_str(s, |chunk| self.output(chunk))
    }
}
