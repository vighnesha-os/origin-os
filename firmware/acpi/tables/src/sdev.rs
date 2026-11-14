//! The Secure Devices table, signature `SDEV` (ACPI 6.6 §5.2.28): devices a secure OS partition
//! should keep, and which of them it may hand to the OS. A hint, enforced by nothing here.

use crate::census::Tally;
use crate::error::{Error, ErrorKind, Signature};
use crate::header::{self, Table};
use crate::read::Bytes;
use crate::walk::{Raw, Shape, Walk, body};

/// One secure device structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecureDevice<'a> {
    /// A device in the namespace (type 0).
    Namespace {
        /// The secure OS may hand it to the OS.
        allow_handoff: bool,
        /// Its full namespace path, without its terminating NUL.
        path: &'a [u8],
        /// The vendor's data.
        vendor: &'a [u8],
        /// Its secure access components, if its flags say there are any.
        components: Option<&'a [u8]>,
    },
    /// A PCIe endpoint (type 1).
    Pcie {
        /// The secure OS may hand it to the OS.
        allow_handoff: bool,
        /// Its segment.
        segment: u16,
        /// The bus its path starts on.
        start_bus: u16,
        /// Its path: a device and a function byte for each bridge down to it.
        path: &'a [u8],
        /// The vendor's data.
        vendor: &'a [u8],
    },
    /// A type ACPI 6.6 reserves.
    Reserved(Raw<'a>),
}

/// A secure access component (§5.2.28.1, Tables 5.173 and 5.174).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Component<'a> {
    /// Identifies the device (type 0).
    Identification {
        /// Its hardware ID, a PNP or ACPI ID without the NUL.
        hardware_id: &'a [u8],
        /// Its subsystem ID, if any.
        subsystem_id: Option<&'a [u8]>,
        /// Its hardware revision, if given.
        revision: Option<u16>,
        /// Its PCI-style class code (base, sub, interface), if given.
        class: Option<[u8; 3]>,
    },
    /// Memory the device uses (type 1).
    Memory {
        /// Its base.
        base: u64,
        /// Its length.
        length: u64,
    },
    /// A type ACPI 6.6 reserves.
    Reserved(Raw<'a>),
}

/// The bytes at an offset and length a structure gives, which must lie inside it.
fn span<'a>(b: &Bytes<'a>, at: usize, field: &'static str) -> Result<&'a [u8], Error> {
    let offset = usize::from(b.u16(at)?);
    let length = usize::from(b.u16(at.saturating_add(2))?);
    b.get(offset, length)
        .map_err(|_| b.error(at, ErrorKind::Offset { field }))
}

/// A NUL-terminated string at an offset and length that count the NUL; returned without it.
/// Refused: one that is empty or does not end in its NUL.
fn string<'a>(b: &Bytes<'a>, at: usize, field: &'static str) -> Result<&'a [u8], Error> {
    match span(b, at, field)?.split_last() {
        Some((0, text)) if !text.is_empty() && !text.contains(&0) => Ok(text),
        _ => Err(b.error(at, ErrorKind::Value { field })),
    }
}

/// Whether `id` is a PNP ID (`AAA####`) or an ACPI ID (`NNNN####`), as §5.2.28.1 requires.
fn is_hardware_id(id: &[u8]) -> bool {
    let hex = |c: &u8| c.is_ascii_hexdigit() && !c.is_ascii_lowercase();
    match id {
        [a, b, c, rest @ ..] if rest.len() == 4 => {
            [a, b, c].iter().all(|c| c.is_ascii_uppercase()) && rest.iter().all(hex)
        }
        [a, b, c, d, rest @ ..] if rest.len() == 4 => {
            [a, b, c, d]
                .iter()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                && rest.iter().all(hex)
        }
        _ => false,
    }
}

fn component<'a>(raw: &Raw<'a>, signature: Signature) -> Result<Component<'a>, Error> {
    Ok(match raw.kind {
        0 => {
            let b = body(raw, signature, 19)?;
            let hardware_id = string(&b, 4, "Hardware Identifier")?;
            if !is_hardware_id(hardware_id) {
                return Err(b.error(
                    4,
                    ErrorKind::Value {
                        field: "Hardware Identifier",
                    },
                ));
            }
            let subsystem_id = if b.u16(8)? == 0 {
                None
            } else {
                let id = string(&b, 8, "Subsystem Identifier")?;
                if !is_hardware_id(id) {
                    return Err(b.error(
                        8,
                        ErrorKind::Value {
                            field: "Subsystem Identifier",
                        },
                    ));
                }
                Some(id)
            };
            Component::Identification {
                hardware_id,
                subsystem_id,
                revision: (b.u8(14)? != 0).then_some(b.u16(12)?),
                class: (b.u8(15)? != 0).then_some(b.array(16)?),
            }
        }
        1 => {
            let b = body(raw, signature, 24)?;
            Component::Memory {
                base: b.u64(8)?,
                length: b.u64(16)?,
            }
        }
        _ => Component::Reserved(*raw),
    })
}

/// The components in a run of bytes; returns how many identify the device.
fn components(run: Bytes<'_>, signature: Signature) -> Result<usize, Error> {
    let mut identifications = 0usize;
    for raw in Walk::new(run, Shape::BYTE_PAD_WORD) {
        if let Component::Identification { .. } = component(&raw?, signature)? {
            identifications = identifications.saturating_add(1);
        }
    }
    Ok(identifications)
}

fn decode<'a>(raw: &Raw<'a>, signature: Signature) -> Result<SecureDevice<'a>, Error> {
    Ok(match raw.kind {
        0 => {
            let b = body(raw, signature, 12)?;
            let flags = b.u8(1)?;
            let path = string(&b, 4, "Device Identifier")?;
            let vendor = span(&b, 8, "Vendor specific data")?;
            let components = if flags & 2 != 0 {
                let b = body(raw, signature, 16)?;
                let offset = usize::from(b.u16(12)?);
                let length = usize::from(b.u16(14)?);
                let run = b.sub(offset, length).map_err(|_| {
                    b.error(
                        12,
                        ErrorKind::Offset {
                            field: "Secure Access Components",
                        },
                    )
                })?;
                // A secure device needs at least one identification component (§5.2.28.1).
                if components(run, signature)? == 0 {
                    return Err(b.error(
                        12,
                        ErrorKind::Value {
                            field: "Secure Access Components",
                        },
                    ));
                }
                Some(run.bytes())
            } else {
                None
            };
            SecureDevice::Namespace {
                allow_handoff: flags & 1 != 0,
                path,
                vendor,
                components,
            }
        }
        1 => {
            let b = body(raw, signature, 16)?;
            let path = span(&b, 8, "PCI Path")?;
            if path.is_empty() || path.len() % 2 != 0 {
                return Err(b.error(8, ErrorKind::Value { field: "PCI Path" }));
            }
            SecureDevice::Pcie {
                allow_handoff: b.u8(1)? & 1 != 0,
                segment: b.u16(4)?,
                start_bus: b.u16(6)?,
                path,
                vendor: span(&b, 12, "Vendor specific data")?,
            }
        }
        _ => SecureDevice::Reserved(*raw),
    })
}

/// The checked SDEV.
#[derive(Clone, Copy, Debug)]
pub struct Sdev<'a> {
    table: Table<'a>,
}

impl<'a> Sdev<'a> {
    /// Checks the table. Refused: an offset and length that leave their structure; a string that
    /// is empty or not NUL-terminated within its length; a hardware or subsystem ID that is
    /// neither a PNP ID nor an ACPI ID; a namespace device with secure access components but none
    /// that identifies it; a PCI path that is empty or not whole device-and-function pairs.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = header::check_signature(bytes, b"SDEV")?;
        let sdev = Self { table };
        for device in sdev.devices() {
            device?;
        }
        Ok(sdev)
    }

    /// Every secure device structure, in order.
    pub fn devices(&self) -> impl Iterator<Item = Result<SecureDevice<'a>, Error>> + 'a {
        let signature = self.table.signature();
        self.table
            .reader()
            .rest(header::HEADER_LEN)
            .ok()
            .into_iter()
            .flat_map(|run| Walk::new(run, Shape::BYTE_PAD_WORD))
            .map(move |raw| decode(&raw?, signature))
    }

    pub(crate) fn census(&self, tally: &mut Tally<'_, 'a>) -> Result<(), Error> {
        for raw in Walk::new(
            self.table.reader().rest(header::HEADER_LEN)?,
            Shape::BYTE_PAD_WORD,
        ) {
            let raw = raw?;
            tally.structure(raw.kind, raw.at, raw.bytes);
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testing::table;

    /// Valid tables, for the hostile-table tests.
    pub(crate) fn samples() -> Vec<Vec<u8>> {
        let comps = identification(b"INTC1043");
        vec![table(
            b"SDEV",
            1,
            &[namespace(b"\\_SB.I2C0", &comps), pcie(&[0x12, 0, 2, 1])].concat(),
        )]
    }

    fn identification(hid: &[u8]) -> Vec<u8> {
        let mut c = vec![0, 0, 0, 0];
        c.extend_from_slice(&19u16.to_le_bytes());
        c.extend_from_slice(&u16::try_from(hid.len() + 1).unwrap().to_le_bytes());
        c.extend_from_slice(&[0, 0, 0, 0, 0x01, 0x00, 1, 1, 0x0C, 0x03, 0x30]);
        c.extend_from_slice(hid);
        c.push(0);
        let len = u16::try_from(c.len()).unwrap();
        c[2..4].copy_from_slice(&len.to_le_bytes());
        c
    }

    fn namespace(path: &[u8], components: &[u8]) -> Vec<u8> {
        let mut s = vec![0, if components.is_empty() { 1 } else { 3 }, 0, 0];
        let path_at = 16u16;
        let vendor_at = path_at + u16::try_from(path.len() + 1).unwrap();
        let comp_at = vendor_at + 2;
        s.extend_from_slice(&path_at.to_le_bytes());
        s.extend_from_slice(&u16::try_from(path.len() + 1).unwrap().to_le_bytes());
        s.extend_from_slice(&vendor_at.to_le_bytes());
        s.extend_from_slice(&2u16.to_le_bytes());
        s.extend_from_slice(&comp_at.to_le_bytes());
        s.extend_from_slice(&u16::try_from(components.len()).unwrap().to_le_bytes());
        s.extend_from_slice(path);
        s.push(0);
        s.extend_from_slice(&[0xAA, 0xBB]);
        s.extend_from_slice(components);
        let len = u16::try_from(s.len()).unwrap();
        s[2..4].copy_from_slice(&len.to_le_bytes());
        s
    }

    fn pcie(path: &[u8]) -> Vec<u8> {
        let mut s = vec![1, 1, 0, 0, 0, 0, 0, 0, 16, 0];
        s.extend_from_slice(&u16::try_from(path.len()).unwrap().to_le_bytes());
        s.extend_from_slice(&u16::try_from(16 + path.len()).unwrap().to_le_bytes());
        s.extend_from_slice(&4u16.to_le_bytes());
        s.extend_from_slice(path);
        s.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        let len = u16::try_from(s.len()).unwrap();
        s[2..4].copy_from_slice(&len.to_le_bytes());
        s
    }

    #[test]
    fn decodes_both_kinds_of_device() {
        let mut memory = vec![1, 0, 24, 0, 0, 0, 0, 0];
        memory.extend_from_slice(&0xFED0_0000u64.to_le_bytes());
        memory.extend_from_slice(&0x1000u64.to_le_bytes());
        let comps = [identification(b"INTC1043"), memory].concat();
        let t = table(
            b"SDEV",
            1,
            &[namespace(b"\\_SB.I2C0", &comps), pcie(&[0x12, 0, 2, 1])].concat(),
        );
        let s = Sdev::parse(&t).unwrap_or_else(|e| panic!("{e}"));
        let all: Vec<_> = s.devices().map(Result::unwrap).collect();
        assert!(matches!(
            all[0],
            SecureDevice::Namespace {
                path: b"\\_SB.I2C0",
                allow_handoff: true,
                components: Some(_),
                ..
            }
        ));
        assert!(matches!(
            all[1],
            SecureDevice::Pcie {
                path: [0x12, 0, 2, 1],
                ..
            }
        ));
        let SecureDevice::Namespace {
            components: Some(c),
            ..
        } = all[0]
        else {
            panic!()
        };
        let first = Walk::new(Bytes::new(c, Signature::new(b"SDEV")), Shape::BYTE_PAD_WORD)
            .next()
            .unwrap()
            .unwrap();
        assert!(matches!(
            component(&first, Signature::new(b"SDEV")).unwrap(),
            Component::Identification {
                hardware_id: b"INTC1043",
                revision: Some(1),
                class: Some([0x0C, 0x03, 0x30]),
                ..
            }
        ));
    }

    #[test]
    fn ids_and_spans_are_checked() {
        assert!(
            is_hardware_id(b"PNP0C09")
                && is_hardware_id(b"INTC1043")
                && is_hardware_id(b"ACPI000E")
        );
        assert!(
            !is_hardware_id(b"pnp0c09")
                && !is_hardware_id(b"PNP0C0")
                && !is_hardware_id(b"INTC10g3")
        );
        let kind = |body: Vec<u8>| Sdev::parse(&table(b"SDEV", 1, &body)).unwrap_err().kind;
        assert_eq!(
            kind(namespace(b"\\X", &identification(b"bad"))),
            ErrorKind::Value {
                field: "Hardware Identifier"
            }
        );
        assert_eq!(
            kind(namespace(b"\\X", &[1, 0, 24, 0])),
            ErrorKind::Truncated { need: 81 }
        );
        let mut mem_only = vec![1, 0, 24, 0];
        mem_only.resize(24, 0);
        assert_eq!(
            kind(namespace(b"\\X", &mem_only)),
            ErrorKind::Value {
                field: "Secure Access Components"
            }
        );
        let mut no_nul = namespace(b"\\X", &[]);
        no_nul[18] = b'Y';
        assert_eq!(
            kind(no_nul),
            ErrorKind::Value {
                field: "Device Identifier"
            }
        );
        let mut outside = namespace(b"\\X", &[]);
        outside[4] = 200;
        assert!(matches!(kind(outside), ErrorKind::Offset { .. }));
        assert_eq!(
            kind(pcie(&[1, 2, 3])),
            ErrorKind::Value { field: "PCI Path" }
        );
        assert_eq!(kind(pcie(&[])), ErrorKind::Value { field: "PCI Path" });
    }
}
