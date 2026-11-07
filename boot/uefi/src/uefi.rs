//! The subset of UEFI 2.11 that the loader uses, defined from the specification.
//!
//! References: UEFI Specification 2.11 (December 2024), §4.2 `EFI_TABLE_HEADER`,
//! §4.3 `EFI_SYSTEM_TABLE`, §12.4 `EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL`, Appendix D status codes.
//!
//! Fields the loader does not use yet are typed as opaque pointers rather than given
//! guessed signatures. A `BOOLEAN` crossing this boundary is a `u8` validated as 0 or 1
//! (CLAUDE.md §10).

use core::ffi::c_void;
use core::fmt;
use core::mem::size_of;

/// `EFI_HANDLE`.
pub type Handle = *mut c_void;

/// `EFI_STATUS`: a `UINTN` whose most significant bit marks an error (Appendix D).
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Status(pub usize);

impl Status {
    const ERROR_BIT: usize = 1 << (usize::BITS - 1);

    /// `EFI_SUCCESS`.
    pub const SUCCESS: Self = Self(0);
    /// `EFI_LOAD_ERROR`.
    pub const LOAD_ERROR: Self = Self(Self::ERROR_BIT | 1);
    /// `EFI_INVALID_PARAMETER`.
    pub const INVALID_PARAMETER: Self = Self(Self::ERROR_BIT | 2);
    /// `EFI_UNSUPPORTED`.
    pub const UNSUPPORTED: Self = Self(Self::ERROR_BIT | 3);
    /// `EFI_DEVICE_ERROR`.
    pub const DEVICE_ERROR: Self = Self(Self::ERROR_BIT | 7);
    /// `EFI_INCOMPATIBLE_VERSION`.
    pub const INCOMPATIBLE_VERSION: Self = Self(Self::ERROR_BIT | 25);
    /// `EFI_WARN_UNKNOWN_GLYPH`: a warning, not an error.
    pub const WARN_UNKNOWN_GLYPH: Self = Self(1);

    /// True if the error bit is set. Warnings are not errors.
    pub const fn is_error(self) -> bool {
        self.0 & Self::ERROR_BIT != 0
    }
}

impl fmt::Debug for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match *self {
            Self::SUCCESS => "EFI_SUCCESS",
            Self::LOAD_ERROR => "EFI_LOAD_ERROR",
            Self::INVALID_PARAMETER => "EFI_INVALID_PARAMETER",
            Self::UNSUPPORTED => "EFI_UNSUPPORTED",
            Self::DEVICE_ERROR => "EFI_DEVICE_ERROR",
            Self::INCOMPATIBLE_VERSION => "EFI_INCOMPATIBLE_VERSION",
            Self::WARN_UNKNOWN_GLYPH => "EFI_WARN_UNKNOWN_GLYPH",
            _ => return write!(f, "EFI_STATUS({:#x})", self.0),
        };
        f.write_str(name)
    }
}

/// `EFI_TABLE_HEADER` (§4.2).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TableHeader {
    /// Identifies the table type.
    pub signature: u64,
    /// UEFI revision this table conforms to; see [`Revision`].
    pub revision: u32,
    /// Size of the entire table, header included, in bytes.
    pub header_size: u32,
    /// CRC32 of the table with this field zeroed.
    pub crc32: u32,
    /// Must be zero.
    pub reserved: u32,
}

/// `EFI_SYSTEM_TABLE_SIGNATURE`: the ASCII bytes "IBI SYST" read as a little-endian `u64`.
pub const SYSTEM_TABLE_SIGNATURE: u64 = 0x5453_5953_2049_4249;

/// `EFI_SYSTEM_TABLE` (§4.3).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SystemTable {
    /// Table header; signature [`SYSTEM_TABLE_SIGNATURE`].
    pub hdr: TableHeader,
    /// NUL-terminated `CHAR16` vendor name.
    pub firmware_vendor: *const u16,
    /// Vendor-specific firmware revision.
    pub firmware_revision: u32,
    /// Handle of the active console input device.
    pub console_in_handle: Handle,
    /// `EFI_SIMPLE_TEXT_INPUT_PROTOCOL`; not used yet.
    pub con_in: *mut c_void,
    /// Handle of the active console output device.
    pub console_out_handle: Handle,
    /// The active console output protocol.
    pub con_out: *mut SimpleTextOutputProtocol,
    /// Handle of the active standard error device.
    pub standard_error_handle: Handle,
    /// The standard error output protocol.
    pub std_err: *mut SimpleTextOutputProtocol,
    /// `EFI_RUNTIME_SERVICES`; not used yet.
    pub runtime_services: *mut c_void,
    /// `EFI_BOOT_SERVICES`; not used yet.
    pub boot_services: *mut c_void,
    /// Number of entries in `configuration_table`.
    pub number_of_table_entries: usize,
    /// `EFI_CONFIGURATION_TABLE` array; not used yet.
    pub configuration_table: *mut c_void,
}

/// `EFI_TEXT_STRING`: writes a NUL-terminated `CHAR16` string to the device.
pub type TextString =
    unsafe extern "efiapi" fn(this: *mut SimpleTextOutputProtocol, string: *const u16) -> Status;

/// `EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL` (§12.4).
#[repr(C)]
#[derive(Debug)]
pub struct SimpleTextOutputProtocol {
    /// `Reset`; not used yet.
    pub reset: *const c_void,
    /// `OutputString`. `None` if the firmware left it null.
    pub output_string: Option<TextString>,
    /// `TestString`; not used yet.
    pub test_string: *const c_void,
    /// `QueryMode`; not used yet.
    pub query_mode: *const c_void,
    /// `SetMode`; not used yet.
    pub set_mode: *const c_void,
    /// `SetAttribute`; not used yet.
    pub set_attribute: *const c_void,
    /// `ClearScreen`; not used yet.
    pub clear_screen: *const c_void,
    /// `SetCursorPosition`; not used yet.
    pub set_cursor_position: *const c_void,
    /// `EnableCursor`; not used yet.
    pub enable_cursor: *const c_void,
    /// `SIMPLE_TEXT_OUTPUT_MODE`; not used yet.
    pub mode: *mut c_void,
}

// The x86-64 layouts below follow from §4.2, §4.3 and §12.4 with 8-byte pointers and
// `UINTN`. They are checked at compile time so that a wrong field order cannot link.
#[cfg(target_pointer_width = "64")]
const _: () = {
    use core::mem::offset_of;
    assert!(size_of::<TableHeader>() == 24);
    assert!(offset_of!(SystemTable, firmware_vendor) == 24);
    assert!(offset_of!(SystemTable, firmware_revision) == 32);
    assert!(offset_of!(SystemTable, con_out) == 64);
    assert!(offset_of!(SystemTable, boot_services) == 96);
    assert!(offset_of!(SystemTable, configuration_table) == 112);
    assert!(size_of::<SystemTable>() == 120);
    assert!(offset_of!(SimpleTextOutputProtocol, output_string) == 8);
    assert!(offset_of!(SimpleTextOutputProtocol, mode) == 72);
    assert!(size_of::<Option<TextString>>() == 8);
};

/// A UEFI revision as stored in `EFI_TABLE_HEADER.Revision`: major in the upper 16 bits,
/// minor in the lower 16 bits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Revision(pub u32);

impl Revision {
    /// The oldest system table revision the loader accepts.
    pub const MINIMUM: Self = Self::new(2, 0);

    /// Builds a revision from its two halves, e.g. `new(2, 110)` for UEFI 2.11.
    pub const fn new(major: u16, minor: u16) -> Self {
        Self(((major as u32) << 16) | minor as u32)
    }

    /// The major revision.
    pub const fn major(self) -> u16 {
        (self.0 >> 16) as u16
    }

    /// The minor revision, e.g. 110 for UEFI 2.11 and 31 for UEFI 2.3.1.
    pub const fn minor(self) -> u16 {
        (self.0 & 0xFFFF) as u16
    }
}

/// Displays the revision the way §4.2 specifies: major, then the upper decimal digits of
/// the minor revision, then its last decimal digit only when it is non-zero. So 2.110 is
/// "2.11", 2.31 is "2.3.1" and 2.70 is "2.7".
impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (upper, lower) = (self.minor() / 10, self.minor() % 10);
        write!(f, "{}.{}", self.major(), upper)?;
        if lower != 0 {
            write!(f, ".{lower}")?;
        }
        Ok(())
    }
}

/// Why the loader rejected the system table pointer it was given.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SystemTableError {
    /// The pointer was null.
    Null,
    /// The pointer was not aligned for `EFI_SYSTEM_TABLE`.
    Misaligned,
    /// The header signature was not [`SYSTEM_TABLE_SIGNATURE`].
    BadSignature(u64),
    /// The revision is older than [`Revision::MINIMUM`].
    UnsupportedRevision(Revision),
    /// `HeaderSize` is too small to contain every field the loader reads.
    TooSmall(u32),
}

impl SystemTableError {
    /// The status `efi_main` returns for this error.
    pub const fn status(self) -> Status {
        match self {
            Self::Null | Self::Misaligned | Self::BadSignature(_) | Self::TooSmall(_) => {
                Status::INVALID_PARAMETER
            }
            Self::UnsupportedRevision(_) => Status::INCOMPATIBLE_VERSION,
        }
    }
}

/// Checks a system table header: signature, minimum revision, and a size that covers
/// every field of [`SystemTable`]. A larger size is accepted, since later revisions may
/// append fields.
pub fn check_system_table_header(hdr: &TableHeader) -> Result<Revision, SystemTableError> {
    if hdr.signature != SYSTEM_TABLE_SIGNATURE {
        return Err(SystemTableError::BadSignature(hdr.signature));
    }
    let revision = Revision(hdr.revision);
    if revision < Revision::MINIMUM {
        return Err(SystemTableError::UnsupportedRevision(revision));
    }
    // `as usize` cannot truncate: usize is at least 32 bits on every Origin target.
    if (hdr.header_size as usize) < size_of::<SystemTable>() {
        return Err(SystemTableError::TooSmall(hdr.header_size));
    }
    Ok(revision)
}

/// Validates the system table pointer passed to `efi_main` and returns a copy of the table.
///
/// The copy is taken once, so later firmware calls that update the live table cannot
/// change values the loader has already validated.
///
/// # Safety
///
/// `ptr` must be null or point to memory readable for at least `size_of::<TableHeader>()`
/// bytes and, when that header is a valid system table header, for `HeaderSize` bytes.
/// UEFI 2.11 §4.1 guarantees this for the pointer the firmware passes to an image entry
/// point while boot services are active.
pub unsafe fn read_system_table(ptr: *const SystemTable) -> Result<SystemTable, SystemTableError> {
    if ptr.is_null() {
        return Err(SystemTableError::Null);
    }
    if !ptr.is_aligned() {
        return Err(SystemTableError::Misaligned);
    }
    // SAFETY: `ptr` is non-null and aligned (checked above; `TableHeader` needs no stricter
    // alignment than `SystemTable`), and the caller guarantees the header is readable.
    let hdr = unsafe { ptr.cast::<TableHeader>().read() };
    check_system_table_header(&hdr)?;
    // SAFETY: the header check proved `HeaderSize >= size_of::<SystemTable>()`, and the
    // caller guarantees `HeaderSize` bytes are readable. Every field is an integer or a raw
    // pointer, so any bit pattern is a valid value.
    Ok(unsafe { ptr.read() })
}

/// Copies the NUL-terminated `CHAR16` string at `s` into `buf`, stopping at the NUL or when
/// `buf` is full, and returns the number of units copied. A null `s` copies nothing.
///
/// The bound means a missing terminator can truncate the result but cannot make the loop
/// run past `buf.len()` units.
///
/// # Safety
///
/// `s` must be null or point to a `CHAR16` string readable up to its terminator or up to
/// `buf.len()` units, whichever comes first. It need not be aligned.
pub unsafe fn copy_char16_string(s: *const u16, buf: &mut [u16]) -> usize {
    if s.is_null() {
        return 0;
    }
    let mut n = 0;
    while n < buf.len() {
        // `wrapping_add` is safe to compute; the read below is what needs the contract.
        let p = s.wrapping_add(n);
        // SAFETY: `n < buf.len()` and no terminator has been seen yet, so the caller
        // guarantees unit `n` (at `p`) is readable. `read_unaligned` imposes no alignment.
        let unit = unsafe { p.read_unaligned() };
        if unit == 0 {
            break;
        }
        if let Some(slot) = buf.get_mut(n) {
            *slot = unit;
        }
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(signature: u64, revision: Revision, header_size: u32) -> TableHeader {
        TableHeader {
            signature,
            revision: revision.0,
            header_size,
            crc32: 0,
            reserved: 0,
        }
    }

    const FULL: u32 = size_of::<SystemTable>() as u32;

    #[test]
    fn signature_is_ibi_syst() {
        assert_eq!(&SYSTEM_TABLE_SIGNATURE.to_le_bytes(), b"IBI SYST");
    }

    #[test]
    fn revision_display_follows_section_4_2() {
        let cases = [
            (Revision::new(2, 110), "2.11"),
            (Revision::new(2, 100), "2.10"),
            (Revision::new(2, 70), "2.7"),
            (Revision::new(2, 31), "2.3.1"),
            (Revision::new(2, 0), "2.0"),
            (Revision::new(1, 2), "1.0.2"),
        ];
        for (rev, want) in cases {
            assert_eq!(format!("{rev}"), want, "{rev:?}");
        }
    }

    #[test]
    fn accepts_valid_header() {
        let rev = Revision::new(2, 110);
        assert_eq!(
            check_system_table_header(&header(SYSTEM_TABLE_SIGNATURE, rev, FULL)),
            Ok(rev)
        );
        // A larger table (fields appended by a later revision) is fine.
        assert!(check_system_table_header(&header(SYSTEM_TABLE_SIGNATURE, rev, FULL + 64)).is_ok());
    }

    #[test]
    fn rejects_bad_signature() {
        let h = header(0x1234, Revision::new(2, 110), FULL);
        assert_eq!(
            check_system_table_header(&h),
            Err(SystemTableError::BadSignature(0x1234))
        );
    }

    #[test]
    fn rejects_old_revision() {
        let old = Revision::new(1, 10);
        let h = header(SYSTEM_TABLE_SIGNATURE, old, FULL);
        assert_eq!(
            check_system_table_header(&h),
            Err(SystemTableError::UnsupportedRevision(old))
        );
        assert_eq!(
            SystemTableError::UnsupportedRevision(old).status(),
            Status::INCOMPATIBLE_VERSION
        );
    }

    #[test]
    fn rejects_short_table() {
        let h = header(SYSTEM_TABLE_SIGNATURE, Revision::new(2, 110), FULL - 8);
        assert_eq!(
            check_system_table_header(&h),
            Err(SystemTableError::TooSmall(FULL - 8))
        );
    }

    #[test]
    fn read_rejects_null_and_misaligned() {
        // SAFETY: a null pointer is explicitly allowed.
        let null = unsafe { read_system_table(core::ptr::null()) };
        assert_eq!(null.err(), Some(SystemTableError::Null));
        let backing = [0u64; 16];
        let misaligned = backing
            .as_ptr()
            .cast::<u8>()
            .wrapping_add(1)
            .cast::<SystemTable>();
        // SAFETY: the function rejects a misaligned pointer before reading through it.
        let r = unsafe { read_system_table(misaligned) };
        assert_eq!(r.err(), Some(SystemTableError::Misaligned));
    }

    #[test]
    fn read_copies_a_valid_table() {
        let vendor: [u16; 3] = [b'O' as u16, b'S' as u16, 0];
        let table = SystemTable {
            hdr: header(SYSTEM_TABLE_SIGNATURE, Revision::new(2, 70), FULL),
            firmware_vendor: vendor.as_ptr(),
            firmware_revision: 0x1_0000,
            console_in_handle: core::ptr::null_mut(),
            con_in: core::ptr::null_mut(),
            console_out_handle: core::ptr::null_mut(),
            con_out: core::ptr::null_mut(),
            standard_error_handle: core::ptr::null_mut(),
            std_err: core::ptr::null_mut(),
            runtime_services: core::ptr::null_mut(),
            boot_services: core::ptr::null_mut(),
            number_of_table_entries: 0,
            configuration_table: core::ptr::null_mut(),
        };
        // SAFETY: `table` is a live, aligned, fully initialized `SystemTable`.
        let copy = unsafe { read_system_table(&table) }.expect("valid table");
        assert_eq!(copy.firmware_revision, 0x1_0000);
        assert_eq!(copy.firmware_vendor, vendor.as_ptr());
    }

    #[test]
    fn copy_char16_stops_at_nul_and_at_capacity() {
        let s: [u16; 5] = [0x4F, 0x72, 0x69, 0, 0x67];
        let mut buf = [0u16; 8];
        // SAFETY: `s` is NUL-terminated within its bounds.
        let n = unsafe { copy_char16_string(s.as_ptr(), &mut buf) };
        assert_eq!(&buf[..n], &[0x4F, 0x72, 0x69]);

        let unterminated: [u16; 4] = [1, 2, 3, 4];
        let mut small = [0u16; 2];
        // SAFETY: the copy is bounded by `small.len()` = 2, within `unterminated`.
        let n = unsafe { copy_char16_string(unterminated.as_ptr(), &mut small) };
        assert_eq!(&small[..n], &[1, 2]);

        // SAFETY: a null pointer is explicitly allowed.
        let n = unsafe { copy_char16_string(core::ptr::null(), &mut buf) };
        assert_eq!(n, 0);
    }

    #[test]
    fn copy_char16_reads_unaligned_input() {
        // Bytes "A\0B\0\0\0" starting at an odd address.
        let bytes: [u8; 8] = [0xFF, b'A', 0, b'B', 0, 0, 0, 0xFF];
        let p = bytes.as_ptr().wrapping_add(1).cast::<u16>();
        let mut buf = [0u16; 4];
        // SAFETY: the string at `p` is NUL-terminated within `bytes`; unaligned is allowed.
        let n = unsafe { copy_char16_string(p, &mut buf) };
        assert_eq!(&buf[..n], &[u16::from(b'A'), u16::from(b'B')]);
    }

    #[test]
    fn status_error_bit() {
        assert!(!Status::SUCCESS.is_error());
        assert!(!Status::WARN_UNKNOWN_GLYPH.is_error());
        assert!(Status::UNSUPPORTED.is_error());
        assert_eq!(format!("{:?}", Status::UNSUPPORTED), "EFI_UNSUPPORTED");
        assert_eq!(format!("{:?}", Status(5)), "EFI_STATUS(0x5)");
    }
}
