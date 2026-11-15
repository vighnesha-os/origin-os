//! The Virtual I/O Translation table, signature `VIOT` (ACPI 6.6 §5.2.33): para-virtual IOMMUs
//! (virtio-iommu) a hypervisor gives its guest, and the endpoints each translates for.

use crate::census::Tally;
use crate::error::{Error, ErrorKind, Signature};
use crate::header::{self, Table};
use crate::walk::{Index, Raw, Shape, Walk, body};

/// The most nodes a VIOT may hold here, which bounds the work of checking where each endpoint's
/// output goes.
pub const MAX_NODES: usize = 4096;

/// One VIOT node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViotNode<'a> {
    /// A range of PCI functions (type 1).
    PciRange {
        /// The first function's endpoint ID.
        endpoint_start: u32,
        /// The first and last segments.
        segments: (u16, u16),
        /// The first and last bus, device and function numbers.
        bdfs: (u16, u16),
        /// The offset of the IOMMU node that translates for them.
        output: u16,
    },
    /// One memory-mapped endpoint (type 2).
    MmioEndpoint {
        /// Its endpoint ID.
        endpoint: u32,
        /// Its base address.
        base: u64,
        /// The offset of the IOMMU node that translates for it.
        output: u16,
    },
    /// A virtio-iommu that is a PCI function (type 3).
    VirtioPciIommu {
        /// Its segment.
        segment: u16,
        /// Its bus, device and function.
        bdf: u16,
    },
    /// A virtio-iommu at a memory-mapped address (type 4).
    VirtioMmioIommu {
        /// Its base address.
        base: u64,
    },
    /// A type ACPI 6.6 reserves (0, which is never to be used, and 5 and above).
    Reserved(Raw<'a>),
}

fn decode<'a>(raw: &Raw<'a>, signature: Signature) -> Result<ViotNode<'a>, Error> {
    Ok(match raw.kind {
        1 => {
            let b = body(raw, signature, 24)?;
            let segments = (b.u16(8)?, b.u16(10)?);
            let bdfs = (b.u16(12)?, b.u16(14)?);
            if segments.1 < segments.0 || bdfs.1 < bdfs.0 {
                return Err(b.error(8, ErrorKind::Value { field: "PCI range" }));
            }
            ViotNode::PciRange {
                endpoint_start: b.u32(4)?,
                segments,
                bdfs,
                output: b.u16(16)?,
            }
        }
        2 => {
            let b = body(raw, signature, 24)?;
            ViotNode::MmioEndpoint {
                endpoint: b.u32(4)?,
                base: b.u64(8)?,
                output: b.u16(16)?,
            }
        }
        3 => {
            let b = body(raw, signature, 16)?;
            ViotNode::VirtioPciIommu {
                segment: b.u16(4)?,
                bdf: b.u16(6)?,
            }
        }
        4 => ViotNode::VirtioMmioIommu {
            base: body(raw, signature, 16)?.u64(8)?,
        },
        _ => ViotNode::Reserved(*raw),
    })
}

/// The checked VIOT.
#[derive(Clone, Debug)]
pub struct Viot<'a> {
    table: Table<'a>,
    index: Index<'a>,
    offset: usize,
    count: u16,
}

impl<'a> Viot<'a> {
    /// Checks the table. Refused: a node offset inside the fixed fields or not 8-byte aligned
    /// (§5.2.33.1); fewer nodes than it counts; a node not 8-byte aligned; a PCI range whose end
    /// is below its start; an endpoint whose output names no IOMMU node (type 3 or 4); more than
    /// [`MAX_NODES`].
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = header::check_signature(bytes, b"VIOT")?;
        let b = table.reader();
        b.require(48)?;
        let count = b.u16(36)?;
        let offset = usize::from(b.u16(38)?);
        if count != 0 && (offset < 48 || offset & 7 != 0) {
            return Err(b.error(
                38,
                ErrorKind::Offset {
                    field: "Node Offset",
                },
            ));
        }
        let all = b
            .rest(if count == 0 { b.len() } else { offset })
            .map_err(|_| {
                b.error(
                    38,
                    ErrorKind::Offset {
                        field: "Node Offset",
                    },
                )
            })?;
        // Only the counted nodes are indexed: bytes after them (padding, say) are not nodes, and
        // the census hands them on as bytes (the review's F12). The walk to find their end takes
        // at most `count` steps.
        let mut end = 0usize;
        for raw in Walk::new(all, Shape::BYTE_PAD_WORD).take(usize::from(count)) {
            end = end.saturating_add(raw?.bytes.len());
        }
        let run = all.sub(0, end)?;
        let index = Index::build(
            run,
            Shape::BYTE_PAD_WORD,
            MAX_NODES,
            "more nodes than the VIOT's checks take",
        )?;
        let viot = Self {
            table,
            index,
            offset,
            count,
        };
        let mut seen = 0u16;
        for node in viot.nodes() {
            let (at, node) = node?;
            if at & 7 != 0 {
                return Err(b.error(
                    at,
                    ErrorKind::Value {
                        field: "a node not 8-byte aligned",
                    },
                ));
            }
            let output = match node {
                ViotNode::PciRange { output, .. } | ViotNode::MmioEndpoint { output, .. } => {
                    Some(output)
                }
                _ => None,
            };
            if let Some(output) = output {
                let target = viot
                    .index
                    .at(usize::from(output))
                    .map(|raw| decode(&raw, table.signature()));
                if !matches!(
                    target,
                    Some(Ok(
                        ViotNode::VirtioPciIommu { .. } | ViotNode::VirtioMmioIommu { .. }
                    ))
                ) {
                    return Err(b.error(
                        at.saturating_add(16),
                        ErrorKind::Reference {
                            field: "Output Node",
                        },
                    ));
                }
            }
            seen = seen.saturating_add(1);
        }
        if seen != count {
            return Err(b.error(
                36,
                ErrorKind::Offset {
                    field: "Node Count",
                },
            ));
        }
        Ok(viot)
    }

    /// The nodes, as many as the table counts, each with its offset.
    pub fn nodes(&self) -> impl Iterator<Item = Result<(usize, ViotNode<'a>), Error>> + 'a {
        let signature = self.table.signature();
        let run = self.table.reader().rest(self.offset).ok();
        run.into_iter()
            .flat_map(|run| Walk::new(run, Shape::BYTE_PAD_WORD))
            .take(usize::from(self.count))
            .map(move |raw| {
                let raw = raw?;
                Ok((raw.at, decode(&raw, signature)?))
            })
    }

    pub(crate) fn census(&self, tally: &mut Tally<'_, 'a>) -> Result<(), Error> {
        let b = self.table.reader();
        let mut end = self.offset;
        for node in self.nodes() {
            let (at, _) = node?;
            let len = usize::from(b.u16(at.saturating_add(2))?);
            tally.structure(u16::from(b.u8(at)?), at, b.get(at, len)?);
            end = at.saturating_add(len);
        }
        if self.count != 0
            && let Ok(rest) = b.rest(end)
            && !rest.bytes().is_empty()
        {
            tally.bytes(end, rest.bytes());
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::census;
    use crate::testing::table;

    /// Valid tables, for the hostile-table tests.
    pub(crate) fn samples() -> Vec<Vec<u8>> {
        let mut vmmio = vec![4, 0, 16, 0, 0, 0, 0, 0];
        vmmio.extend_from_slice(&0xFEC0_0000u64.to_le_bytes());
        vec![viot(48, 3, &[pci_iommu(), pci_range(48, 0, 0xFF), vmmio])]
    }

    fn pci_range(output: u16, start: u16, end: u16) -> Vec<u8> {
        let mut n = vec![1, 0, 24, 0];
        n.extend_from_slice(&8u32.to_le_bytes());
        n.extend_from_slice(&[0, 0, 0, 0]);
        n.extend_from_slice(&start.to_le_bytes());
        n.extend_from_slice(&end.to_le_bytes());
        n.extend_from_slice(&output.to_le_bytes());
        n.extend_from_slice(&[0; 6]);
        n
    }

    fn viot(offset: u16, count: u16, nodes: &[Vec<u8>]) -> Vec<u8> {
        let mut body = count.to_le_bytes().to_vec();
        body.extend_from_slice(&offset.to_le_bytes());
        body.extend_from_slice(&[0; 8]);
        for n in nodes {
            body.extend_from_slice(n);
        }
        table(b"VIOT", 1, &body)
    }

    fn pci_iommu() -> Vec<u8> {
        let mut n = vec![3, 0, 16, 0, 0, 0, 0x10, 0];
        n.extend_from_slice(&[0; 8]);
        n
    }

    #[test]
    fn decodes_and_links_nodes() {
        let mut mmio = vec![2, 0, 24, 0];
        mmio.extend_from_slice(&1u32.to_le_bytes());
        mmio.extend_from_slice(&0xFEB0_0000u64.to_le_bytes());
        mmio.extend_from_slice(&112u16.to_le_bytes());
        mmio.extend_from_slice(&[0; 6]);
        let mut vmmio = vec![4, 0, 16, 0, 0, 0, 0, 0];
        vmmio.extend_from_slice(&0xFEC0_0000u64.to_le_bytes());
        let t = viot(48, 4, &[pci_iommu(), pci_range(48, 0, 0xFF), mmio, vmmio]);
        let v = Viot::parse(&t).unwrap_or_else(|e| panic!("{e:?}"));
        let all: Vec<_> = v.nodes().map(|n| n.unwrap().1).collect();
        assert_eq!(
            all[0],
            ViotNode::VirtioPciIommu {
                segment: 0,
                bdf: 0x10
            }
        );
        assert!(matches!(
            all[1],
            ViotNode::PciRange {
                output: 48,
                bdfs: (0, 0xFF),
                ..
            }
        ));
        assert!(matches!(
            all[3],
            ViotNode::VirtioMmioIommu { base: 0xFEC0_0000 }
        ));
        assert!(
            matches!(census::check(&t).unwrap().class, census::Class::Checked(s) if s.structures() == 4)
        );
        assert!(Viot::parse(&viot(0, 0, &[])).is_ok());
        // The review's F12: bytes after the counted nodes are not walked as nodes.
        let mut padded = viot(48, 1, &[pci_iommu()]);
        padded.extend_from_slice(&[0; 8]);
        crate::testing::seal(&mut padded);
        let v = Viot::parse(&padded).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(v.nodes().count(), 1);
        assert!(
            matches!(census::check(&padded).unwrap().class, census::Class::Checked(s) if s.structures() == 1)
        );
    }

    #[test]
    fn offsets_and_outputs_are_checked() {
        let kind = |t: Vec<u8>| Viot::parse(&t).unwrap_err().kind;
        assert_eq!(
            kind(viot(52, 1, &[vec![0; 4], pci_iommu()])),
            ErrorKind::Offset {
                field: "Node Offset"
            }
        );
        assert_eq!(
            kind(viot(40, 1, &[pci_iommu()])),
            ErrorKind::Offset {
                field: "Node Offset"
            }
        );
        assert_eq!(
            kind(viot(48, 2, &[pci_iommu()])),
            ErrorKind::Offset {
                field: "Node Count"
            }
        );
        assert_eq!(
            kind(viot(48, 1, &[pci_range(48, 0, 1)])),
            ErrorKind::Reference {
                field: "Output Node"
            }
        );
        assert_eq!(
            kind(viot(48, 2, &[pci_iommu(), pci_range(50, 0, 1)])),
            ErrorKind::Reference {
                field: "Output Node"
            }
        );
        assert_eq!(
            kind(viot(48, 2, &[pci_iommu(), pci_range(48, 2, 1)])),
            ErrorKind::Value { field: "PCI range" }
        );
        let mut odd = pci_iommu();
        odd[2] = 20;
        odd.extend_from_slice(&[0; 4]);
        assert_eq!(
            kind(viot(48, 2, &[odd, pci_iommu()])),
            ErrorKind::Value {
                field: "a node not 8-byte aligned"
            }
        );
    }
}
