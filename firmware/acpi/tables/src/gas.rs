//! The generic address structure (ACPI 6.6 §5.2.3.2): where a register is, in which address
//! space, and how wide an access to it is.

use crate::error::{Error, ErrorKind};
use crate::read::Bytes;

/// Bytes in a generic address structure.
pub const GAS_LEN: usize = 12;

/// The address space a register is in (the structure's first byte).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressSpace {
    /// System memory (0).
    SystemMemory,
    /// System I/O ports (1).
    SystemIo,
    /// PCI configuration space (2): the address holds the device, function and offset.
    PciConfig,
    /// The embedded controller's space (3).
    EmbeddedController,
    /// SMBus (4).
    SmBus,
    /// The CMOS RAM behind the real-time clock (5).
    SystemCmos,
    /// A PCI device's base address register (6).
    PciBarTarget,
    /// IPMI (7).
    Ipmi,
    /// General-purpose I/O (8).
    GeneralPurposeIo,
    /// A generic serial bus (9).
    GenericSerialBus,
    /// A Platform Communications Channel (10).
    PlatformCommunicationsChannel,
    /// The Platform Runtime Mechanism (11).
    PlatformRuntimeMechanism,
    /// Functional fixed hardware (0x7F): a register reached by the processor vendor's own means.
    FunctionalFixedHardware,
    /// A value ACPI 6.6 reserves (12 to 0x7E).
    Reserved(u8),
    /// A value for the OEM's own use (0x80 to 0xFF).
    Oem(u8),
}

impl AddressSpace {
    /// The space a byte names.
    #[must_use]
    pub const fn from_id(id: u8) -> Self {
        match id {
            0 => Self::SystemMemory,
            1 => Self::SystemIo,
            2 => Self::PciConfig,
            3 => Self::EmbeddedController,
            4 => Self::SmBus,
            5 => Self::SystemCmos,
            6 => Self::PciBarTarget,
            7 => Self::Ipmi,
            8 => Self::GeneralPurposeIo,
            9 => Self::GenericSerialBus,
            10 => Self::PlatformCommunicationsChannel,
            11 => Self::PlatformRuntimeMechanism,
            0x7F => Self::FunctionalFixedHardware,
            0x80..=0xFF => Self::Oem(id),
            _ => Self::Reserved(id),
        }
    }
}

/// How wide one access to the register is (the structure's fourth byte).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessSize {
    /// Not given (0): ACPI 1.0's legacy reason; the register's width decides.
    Undefined,
    /// One byte (1).
    Byte,
    /// Two bytes (2).
    Word,
    /// Four bytes (3).
    Dword,
    /// Eight bytes (4).
    Qword,
    /// A value ACPI 6.6 reserves (5 and above).
    Reserved(u8),
}

impl AccessSize {
    /// The size a byte names.
    #[must_use]
    pub const fn from_id(id: u8) -> Self {
        match id {
            0 => Self::Undefined,
            1 => Self::Byte,
            2 => Self::Word,
            3 => Self::Dword,
            4 => Self::Qword,
            _ => Self::Reserved(id),
        }
    }

    /// The access's width in bits, if one is given.
    #[must_use]
    pub const fn bits(self) -> Option<u16> {
        match self {
            Self::Byte => Some(8),
            Self::Word => Some(16),
            Self::Dword => Some(32),
            Self::Qword => Some(64),
            Self::Undefined | Self::Reserved(_) => None,
        }
    }
}

/// A generic address structure, decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gas {
    /// The address space.
    pub space: AddressSpace,
    /// The register's width in bits.
    pub bit_width: u8,
    /// The register's first bit at `address`.
    pub bit_offset: u8,
    /// The access width.
    pub access: AccessSize,
    /// The register's address in its space.
    pub address: u64,
}

impl Gas {
    /// Reads the 12 bytes at `at`.
    pub(crate) fn read(b: &Bytes<'_>, at: usize) -> Result<Self, Error> {
        let raw = b.array::<GAS_LEN>(at)?;
        let [
            space,
            bit_width,
            bit_offset,
            access,
            a0,
            a1,
            a2,
            a3,
            a4,
            a5,
            a6,
            a7,
        ] = raw;
        Ok(Self {
            space: AddressSpace::from_id(space),
            bit_width,
            bit_offset,
            access: AccessSize::from_id(access),
            address: u64::from_le_bytes([a0, a1, a2, a3, a4, a5, a6, a7]),
        })
    }

    /// Whether the structure names no register: an address of zero.
    #[must_use]
    pub const fn is_null(&self) -> bool {
        self.address == 0
    }

    /// Checks a register the operating system will access: an address space and an access size
    /// that are not reserved. Which spaces, widths and offsets a given register may have is its
    /// table's rule, checked there (a register block's width is the whole block's, not one
    /// access's).
    ///
    /// Refused here, as reserved values an OS would act on (crate documentation, "Refusals"): a
    /// reserved address space and a reserved access size.
    pub(crate) fn check_register(
        &self,
        b: &Bytes<'_>,
        at: usize,
        field: &'static str,
    ) -> Result<(), Error> {
        let refuse = || b.error(at, ErrorKind::Value { field });
        if matches!(self.space, AddressSpace::Reserved(_)) {
            return Err(refuse());
        }
        if matches!(self.access, AccessSize::Reserved(_)) {
            return Err(refuse());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Signature;
    use crate::testing::gas;

    #[test]
    fn decodes_every_field() {
        let raw = gas(1, 32, 0, 3, 0x608);
        let g = Gas::read(&Bytes::new(&raw, Signature::new(b"TEST")), 0).unwrap();
        assert_eq!(
            g,
            Gas {
                space: AddressSpace::SystemIo,
                bit_width: 32,
                bit_offset: 0,
                access: AccessSize::Dword,
                address: 0x608
            }
        );
        assert_eq!(
            AddressSpace::from_id(0x7F),
            AddressSpace::FunctionalFixedHardware
        );
        assert_eq!(AddressSpace::from_id(0x0C), AddressSpace::Reserved(0x0C));
        assert_eq!(AddressSpace::from_id(0x7E), AddressSpace::Reserved(0x7E));
        assert_eq!(AddressSpace::from_id(0x80), AddressSpace::Oem(0x80));
        assert_eq!(AccessSize::from_id(5), AccessSize::Reserved(5));
        assert_eq!(AccessSize::Qword.bits(), Some(64));
    }

    #[test]
    fn a_register_with_reserved_values_is_refused() {
        let b = |raw: [u8; 12]| {
            let bytes = Bytes::new(&raw, Signature::new(b"TEST"));
            Gas::read(&bytes, 0)
                .unwrap()
                .check_register(&bytes, 0, "REG")
        };
        assert!(b(gas(0, 64, 0, 4, 0x1000)).is_ok());
        assert!(b(gas(0x0C, 8, 0, 1, 1)).is_err());
        assert!(b(gas(1, 8, 0, 5, 1)).is_err());
        assert!(
            b(gas(1, 128, 0, 0, 0x620)).is_ok(),
            "a GPE block is wider than one access"
        );
        let short = [0u8; 11];
        assert!(Gas::read(&Bytes::new(&short, Signature::new(b"TEST")), 0).is_err());
    }
}
