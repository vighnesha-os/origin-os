//! The Firmware ACPI Control Structure, signature `FACS` (ACPI 6.6 §5.2.10). It has no system
//! description table header and no checksum: its signature, its length and its fields are all the
//! check there is. The global lock (§5.2.10.1) is located here and taken by no one in this crate.

use crate::error::{Error, ErrorKind, Signature};
use crate::read::Bytes;

/// The shortest FACS.
pub const MIN_LEN: usize = 64;

/// The FACS's firmware flags (§5.2.10, Table 5.14). Bits 2 to 31 are reserved and ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FacsFlags(pub u32);

impl FacsFlags {
    /// The firmware supports S4BIOS (bit 0).
    #[must_use]
    pub const fn s4bios(self) -> bool {
        self.0 & 1 != 0
    }
    /// The firmware can enter the waking vector in long mode (bit 1).
    #[must_use]
    pub const fn wake_64bit_supported(self) -> bool {
        self.0 & 2 != 0
    }
}

/// The global lock's two bits (§5.2.10.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlobalLock {
    /// Someone waits for the lock (bit 0).
    pub pending: bool,
    /// The lock is held (bit 1).
    pub owned: bool,
}

/// The checked FACS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facs {
    /// Its length.
    pub length: u32,
    /// The firmware's signature of this boot's hardware: it changes when any table does.
    pub hardware_signature: u32,
    /// The 32-bit waking vector.
    pub firmware_waking_vector: u32,
    /// The global lock as it was when read.
    pub global_lock: GlobalLock,
    /// The firmware's flags.
    pub flags: FacsFlags,
    /// The 64-bit waking vector.
    pub x_firmware_waking_vector: u64,
    /// The structure's version.
    pub version: u8,
    /// The flags the OS sets for the firmware (bit 0: wake in long mode).
    pub ospm_flags: u32,
}

impl Facs {
    /// The offset of the global lock's dword, for the code that will one day take it.
    pub const GLOBAL_LOCK_AT: usize = 16;

    /// Checks a FACS: its signature, a length of at least 64 that is exactly the bytes given.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let signature = Signature::new(b"FACS");
        let b = Bytes::new(bytes, signature);
        let found = Signature(b.array(0)?);
        if found != signature {
            return Err(b.error(0, ErrorKind::Signature { found }));
        }
        let length = b.u32(4)?;
        let declared = usize::try_from(length).unwrap_or(usize::MAX);
        if declared < MIN_LEN || declared != bytes.len() {
            return Err(b.error(
                4,
                ErrorKind::Length {
                    declared: u64::from(length),
                    available: bytes.len(),
                },
            ));
        }
        let lock = b.u32(Self::GLOBAL_LOCK_AT)?;
        Ok(Self {
            length,
            hardware_signature: b.u32(8)?,
            firmware_waking_vector: b.u32(12)?,
            global_lock: GlobalLock {
                pending: lock & 1 != 0,
                owned: lock & 2 != 0,
            },
            flags: FacsFlags(b.u32(20)?),
            x_firmware_waking_vector: b.u64(24)?,
            version: b.u8(32)?,
            ospm_flags: b.u32(36)?,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn facs() -> Vec<u8> {
        let mut f = vec![0u8; 64];
        f[..4].copy_from_slice(b"FACS");
        f[4..8].copy_from_slice(&64u32.to_le_bytes());
        f[8..12].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        f[20..24].copy_from_slice(&2u32.to_le_bytes());
        f[32] = 3;
        f
    }

    #[test]
    fn decodes_every_field() {
        let mut f = facs();
        f[16] = 0b11;
        let parsed = Facs::parse(&f).unwrap();
        assert_eq!(parsed.hardware_signature, 0x1234_5678);
        assert!(parsed.flags.wake_64bit_supported() && !parsed.flags.s4bios());
        assert_eq!(
            parsed.global_lock,
            GlobalLock {
                pending: true,
                owned: true
            }
        );
        assert_eq!(parsed.version, 3);
        let mut long = facs();
        long.resize(128, 0);
        long[4..8].copy_from_slice(&128u32.to_le_bytes());
        assert_eq!(Facs::parse(&long).unwrap().length, 128);
    }

    #[test]
    fn signature_and_length_are_refused_by_name() {
        let mut f = facs();
        f[0] = b'X';
        assert!(matches!(
            Facs::parse(&f).unwrap_err().kind,
            ErrorKind::Signature { .. }
        ));
        let mut f = facs();
        f[4] = 63;
        assert!(matches!(
            Facs::parse(&f[..63]).unwrap_err().kind,
            ErrorKind::Length { .. }
        ));
        assert!(matches!(
            Facs::parse(&facs()[..63]).unwrap_err().kind,
            ErrorKind::Length { .. }
        ));
        assert!(Facs::parse(&[]).is_err());
        assert!(Facs::parse(b"FACS").is_err());
    }
}
