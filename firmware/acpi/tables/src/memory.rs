//! The memory tables of ACPI 6.6 §5.2.22: the Memory Power State Table (MPST) and the Platform
//! Memory Topology Table (PMTT, §5.2.22.12).
//!
//! The MPST's commands pass through a PCC subspace (ADR-0019's area H); [`MpstChannel`] decodes
//! that subspace's shared memory from bytes its caller supplies.

use crate::census::Tally;
use crate::error::{Error, ErrorKind, Signature};
use crate::header::{self, Table};
use crate::read::{Bytes, Guid};

/// Bytes in a power state characteristics structure (§5.2.22.6).
const CHARACTERISTICS: usize = 28;
/// Where the MPST's first memory power node is.
const NODES: usize = 44;

/// One memory power node structure (§5.2.22.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerNode<'a> {
    /// Its offset in the table.
    pub at: usize,
    /// The structure counts: if not, ACPI 6.6 tells the OS to ignore it.
    pub enabled: bool,
    /// The OS may put it in a low-power state.
    pub power_managed: bool,
    /// The node supports memory hot plug.
    pub hot_pluggable: bool,
    /// The node's ID; several structures share one when a node has several ranges.
    pub id: u16,
    /// The range's base.
    pub base: u64,
    /// The range's length.
    pub length: u64,
    /// The power states, each two bytes: the state's number and the index of its
    /// characteristics.
    pub states: &'a [u8],
    /// The physical components, two bytes each.
    pub components: &'a [u8],
}

impl PowerNode<'_> {
    /// The node's power states: each state's number (MPSx) and its characteristics' index.
    pub fn power_states(&self) -> impl Iterator<Item = (u8, u8)> + '_ {
        self.states
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&[state, index]| (state, index))
    }
}

/// One power state's characteristics (§5.2.22.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Characteristics {
    /// The structure's revision (bits 6 and 7 of its first byte).
    pub revision: u8,
    /// The memory's contents survive the state.
    pub content_preserved: bool,
    /// Bit 1 of the flags (autonomous entry); §5.2.22.6's text and the bit's name disagree on
    /// what a set bit means, so it is given as read.
    pub autonomous_entry_bit: bool,
    /// Bit 2 of the flags (autonomous exit), given as read for the same reason.
    pub autonomous_exit_bit: bool,
    /// Average power in MPS0, in mW.
    pub mps0_power_mw: u32,
    /// The power saved relative to MPS0, in percent.
    pub saving_percent: u32,
    /// The time back to MPS0, in nanoseconds.
    pub exit_latency_ns: u64,
}

/// The checked MPST.
#[derive(Clone, Copy, Debug)]
pub struct Mpst<'a> {
    table: Table<'a>,
    /// The PCC subspace its commands use.
    pub channel: u8,
    node_count: u16,
    characteristics_at: usize,
    characteristics_count: u16,
}

impl<'a> Mpst<'a> {
    /// Checks the table. Refused: a node whose length does not hold its two lists, or that runs
    /// past the table; a range that wraps past the top of the address space; characteristics that
    /// do not fit; a characteristics structure of a format other than 1 (§5.2.22.6: the only one
    /// whose layout is known, so its stride cannot be trusted otherwise); and a power state whose
    /// index names no characteristics.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = header::check_signature(bytes, b"MPST")?;
        let b = table.reader();
        b.require(NODES)?;
        let node_count = b.u16(40)?;
        let mut at = NODES;
        for _ in 0..node_count {
            node(&b, at)?;
            at = at.saturating_add(
                usize::try_from(b.u32(at.saturating_add(4))?).unwrap_or(usize::MAX),
            );
        }
        let characteristics_count = b.u16(at)?;
        let characteristics_at = at.saturating_add(4);
        b.array_of(
            characteristics_at,
            usize::from(characteristics_count),
            CHARACTERISTICS,
            "Memory Power State Characteristics Count",
        )?;
        let mpst = Self {
            table,
            channel: b.u8(36)?,
            node_count,
            characteristics_at,
            characteristics_count,
        };
        for i in 0..characteristics_count {
            mpst.characteristics(i)?;
        }
        for node in mpst.nodes() {
            let node = node?;
            for (i, (_, index)) in node.power_states().enumerate() {
                if u16::from(index) >= characteristics_count {
                    let at = node
                        .at
                        .saturating_add(33)
                        .saturating_add(i.saturating_mul(2));
                    return Err(b.error(
                        at,
                        ErrorKind::Reference {
                            field: "Power State Information Index",
                        },
                    ));
                }
            }
        }
        Ok(mpst)
    }

    /// The memory power nodes, in order.
    pub fn nodes(&self) -> impl Iterator<Item = Result<PowerNode<'a>, Error>> + 'a {
        let b = self.table.reader();
        let mut at = NODES;
        (0..self.node_count).map(move |_| {
            let n = node(&b, at)?;
            at = at.saturating_add(
                usize::try_from(b.u32(at.saturating_add(4))?).unwrap_or(usize::MAX),
            );
            Ok(n)
        })
    }

    /// The `i`th characteristics structure.
    pub fn characteristics(&self, i: u16) -> Result<Characteristics, Error> {
        let b = self.table.reader();
        let at = usize::from(i)
            .saturating_mul(CHARACTERISTICS)
            .saturating_add(self.characteristics_at);
        let c = b.sub(at, CHARACTERISTICS)?;
        let id = c.u8(0)?;
        if id & 0x3F != 1 {
            return Err(c.error(
                0,
                ErrorKind::Value {
                    field: "Power State Structure ID",
                },
            ));
        }
        let flags = c.u8(1)?;
        Ok(Characteristics {
            revision: id >> 6,
            content_preserved: flags & 1 != 0,
            autonomous_entry_bit: flags & 2 != 0,
            autonomous_exit_bit: flags & 4 != 0,
            mps0_power_mw: c.u32(4)?,
            saving_percent: c.u32(8)?,
            exit_latency_ns: c.u64(12)?,
        })
    }

    pub(crate) fn census(&self, tally: &mut Tally<'_, 'a>) -> Result<(), Error> {
        let b = self.table.reader();
        for node in self.nodes() {
            let node = node?;
            let len = usize::try_from(b.u32(node.at.saturating_add(4))?).unwrap_or(usize::MAX);
            tally.structure(0, node.at, b.get(node.at, len)?);
        }
        for i in 0..self.characteristics_count {
            let at = usize::from(i)
                .saturating_mul(CHARACTERISTICS)
                .saturating_add(self.characteristics_at);
            tally.structure(1, at, b.get(at, CHARACTERISTICS)?);
        }
        Ok(())
    }
}

/// The node at `at`: its length holds its fixed part and its two lists, and fits.
fn node<'a>(b: &Bytes<'a>, at: usize) -> Result<PowerNode<'a>, Error> {
    let length = usize::try_from(b.u32(at.saturating_add(4))?).unwrap_or(usize::MAX);
    let n = b
        .sub(at, length)
        .map_err(|_| b.error(at.saturating_add(4), ErrorKind::Offset { field: "Length" }))?;
    let structure = |_| b.error(at, ErrorKind::Structure { kind: 0, length });
    n.require(32).map_err(structure)?;
    let states = usize::try_from(n.u32(24)?).unwrap_or(usize::MAX);
    let components = usize::try_from(n.u32(28)?).unwrap_or(usize::MAX);
    let states_bytes = n
        .array_of(32, states, 2, "Number of Power States")
        .map_err(structure)?;
    let components_at = 32usize.saturating_add(states_bytes.len());
    let components_bytes = n
        .array_of(
            components_at,
            components,
            2,
            "Number of Physical Components",
        )
        .map_err(structure)?;
    let base = n.u64(8)?;
    let range = n.u64(16)?;
    if base.checked_add(range).is_none() {
        return Err(n.error(
            16,
            ErrorKind::Value {
                field: "Length Low",
            },
        ));
    }
    let flag = n.u8(0)?;
    Ok(PowerNode {
        at,
        enabled: flag & 1 != 0,
        power_managed: flag & 2 != 0,
        hot_pluggable: flag & 4 != 0,
        id: n.u16(2)?,
        base,
        length: range,
        states: states_bytes.bytes(),
        components: components_bytes.bytes(),
    })
}

/// The MPST channel's shared memory (§5.2.22.1, Table 5.106), decoded from bytes the caller read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MpstChannel {
    /// The PCC command (3: execute).
    pub command: u16,
    /// The PCC status.
    pub status: u16,
    /// The memory power command: 1 get state, 2 set state, 3 average power, 4 energy.
    pub power_command: u32,
    /// The result in bits 0 to 3, and in bit 4 whether memory was busy.
    pub power_status: u32,
    /// The power state got or to set.
    pub power_state: u32,
    /// The node the command is for.
    pub node: u32,
    /// The energy the node used, or `None` if not implemented (all ones).
    pub energy: Option<u64>,
    /// The node's expected average power, or `None` if not implemented.
    pub expected_power: Option<u64>,
}

impl MpstChannel {
    /// Decodes the shared memory of PCC subspace `subspace`: its signature must be `0x50434300`
    /// with the subspace's ID in its low byte.
    pub fn parse(bytes: &[u8], subspace: u8) -> Result<Self, Error> {
        let b = Bytes::new(bytes, Signature::new(b"MPST"));
        b.require(40)?;
        if b.u32(0)? != 0x5043_4300 | u32::from(subspace) {
            return Err(b.error(0, ErrorKind::Value { field: "Signature" }));
        }
        let implemented = |v: u64| (v != u64::MAX).then_some(v);
        Ok(Self {
            command: b.u16(4)?,
            status: b.u16(6)?,
            power_command: b.u32(8)?,
            power_status: b.u32(12)?,
            power_state: b.u32(16)?,
            node: b.u32(20)?,
            energy: implemented(b.u64(24)?),
            expected_power: implemented(b.u64(32)?),
        })
    }
}

/// The deepest nesting of memory devices the PMTT walk follows. ACPI 6.6 sets no limit; the
/// topology it describes (socket, controller, DIMM, and a vendor's level) is four deep.
pub const PMTT_MAX_DEPTH: usize = 8;

/// A memory device in the PMTT (§5.2.22.12, Tables 5.115 to 5.119).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryDevice<'a> {
    /// A socket (type 0).
    Socket {
        /// Its ID.
        id: u16,
    },
    /// A memory controller (type 1).
    Controller {
        /// Its ID within its parent.
        id: u16,
    },
    /// A DIMM (type 2).
    Dimm {
        /// Its SMBIOS type 17 handle, or `None` (0xFFFFFFFF).
        smbios_handle: Option<u32>,
    },
    /// A vendor's device (type 0xFF).
    Vendor {
        /// The vendor's type.
        uuid: Guid,
        /// The vendor's data.
        data: &'a [u8],
    },
    /// A type ACPI 6.6 reserves (3 to 0xFE).
    Reserved(u8),
}

/// One device met by the PMTT walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceNode<'a> {
    /// Its offset in the table.
    pub at: usize,
    /// How deep it is: 0 for a top-level device.
    pub depth: usize,
    /// Its flags: bit 0 top-level aggregator, bit 1 physical, bits 2 and 3 volatility.
    pub flags: u16,
    /// What it is.
    pub device: MemoryDevice<'a>,
    /// Its own bytes, its children not included.
    pub bytes: &'a [u8],
}

/// The checked PMTT.
#[derive(Clone, Copy, Debug)]
pub struct Pmtt<'a> {
    table: Table<'a>,
}

impl<'a> Pmtt<'a> {
    /// Checks the table. Refused: revision 1, deprecated since ACPI 6.4 and of a layout §5.2.22.12
    /// no longer gives; a device whose length does not hold its type's fields; children nested
    /// deeper than [`PMTT_MAX_DEPTH`]; a top-level aggregator (flag bit 0) anywhere but the top
    /// level, which §5.2.22.12 forbids; and bytes after the last top-level device's tree, which
    /// the count of top-level devices does not account for.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = header::check_signature(bytes, b"PMTT")?;
        if table.revision() < 2 {
            return Err(Error::new(
                table.signature(),
                8,
                ErrorKind::Revision(table.revision()),
            ));
        }
        table.reader().require(40)?;
        let pmtt = Self { table };
        let end = pmtt.walk(&mut |_| {})?;
        if end != bytes.len() {
            return Err(Error::new(
                table.signature(),
                end,
                ErrorKind::Offset {
                    field: "Number of Memory Devices",
                },
            ));
        }
        Ok(pmtt)
    }

    /// Visits every device, parents before their children, and returns where the last one's tree
    /// ends.
    pub fn walk(&self, visit: &mut dyn FnMut(DeviceNode<'a>)) -> Result<usize, Error> {
        let b = self.table.reader();
        let count = b.u32(36)?;
        let mut at = 40;
        for _ in 0..count {
            at = device(&b, at, 0, visit)?;
        }
        Ok(at)
    }

    pub(crate) fn census(&self, tally: &mut Tally<'_, 'a>) -> Result<(), Error> {
        self.walk(&mut |node| {
            let kind = match node.device {
                MemoryDevice::Socket { .. } => 0,
                MemoryDevice::Controller { .. } => 1,
                MemoryDevice::Dimm { .. } => 2,
                MemoryDevice::Vendor { .. } => 0xFF,
                MemoryDevice::Reserved(kind) => u16::from(kind),
            };
            tally.structure(kind, node.at, node.bytes);
        })?;
        Ok(())
    }
}

/// The device at `at` and its children; returns where its tree ends.
fn device<'a>(
    b: &Bytes<'a>,
    at: usize,
    depth: usize,
    visit: &mut dyn FnMut(DeviceNode<'a>),
) -> Result<usize, Error> {
    if depth >= PMTT_MAX_DEPTH {
        return Err(b.error(
            at,
            ErrorKind::Value {
                field: "memory devices nested too deep",
            },
        ));
    }
    let kind = b.u8(at)?;
    let length = usize::from(b.u16(at.saturating_add(2))?);
    let min = match kind {
        0..=2 => 16,
        0xFF => 28,
        _ => 12,
    };
    if length < min {
        return Err(b.error(
            at,
            ErrorKind::Structure {
                kind: u16::from(kind),
                length,
            },
        ));
    }
    let d = b.sub(at, length)?;
    let flags = d.u16(4)?;
    if depth > 0 && flags & 1 != 0 {
        return Err(d.error(
            4,
            ErrorKind::Inconsistent {
                field: "a top-level device nested in another",
            },
        ));
    }
    let decoded = match kind {
        0 => MemoryDevice::Socket { id: d.u16(12)? },
        1 => MemoryDevice::Controller { id: d.u16(12)? },
        2 => {
            let handle = d.u32(12)?;
            MemoryDevice::Dimm {
                smbios_handle: (handle != u32::MAX).then_some(handle),
            }
        }
        0xFF => MemoryDevice::Vendor {
            uuid: d.guid(12)?,
            data: d.rest(28)?.bytes(),
        },
        other => MemoryDevice::Reserved(other),
    };
    visit(DeviceNode {
        at,
        depth,
        flags,
        device: decoded,
        bytes: d.bytes(),
    });
    let children = d.u32(8)?;
    let mut next = at.saturating_add(length);
    for _ in 0..children {
        next = device(b, next, depth.saturating_add(1), visit)?;
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::census;
    use crate::testing::table;

    fn node_bytes(id: u16, states: &[(u8, u8)], components: &[u16]) -> Vec<u8> {
        let len = 32 + 2 * states.len() + 2 * components.len();
        let mut n = vec![0b011, 0];
        n.extend_from_slice(&id.to_le_bytes());
        n.extend_from_slice(&u32::try_from(len).unwrap().to_le_bytes());
        n.extend_from_slice(&0x1_0000_0000u64.to_le_bytes());
        n.extend_from_slice(&0x4000_0000u64.to_le_bytes());
        n.extend_from_slice(&u32::try_from(states.len()).unwrap().to_le_bytes());
        n.extend_from_slice(&u32::try_from(components.len()).unwrap().to_le_bytes());
        for &(s, i) in states {
            n.extend_from_slice(&[s, i]);
        }
        for c in components {
            n.extend_from_slice(&c.to_le_bytes());
        }
        n
    }

    fn characteristics(id: u8) -> Vec<u8> {
        let mut c = vec![id, 1, 0, 0];
        c.extend_from_slice(&900u32.to_le_bytes());
        c.extend_from_slice(&40u32.to_le_bytes());
        c.extend_from_slice(&5000u64.to_le_bytes());
        c.extend_from_slice(&[0; 8]);
        c
    }

    fn mpst(nodes: &[Vec<u8>], chars: &[Vec<u8>]) -> Vec<u8> {
        let mut body = vec![4, 0, 0, 0];
        body.extend_from_slice(&u16::try_from(nodes.len()).unwrap().to_le_bytes());
        body.extend_from_slice(&[0, 0]);
        for n in nodes {
            body.extend_from_slice(n);
        }
        body.extend_from_slice(&u16::try_from(chars.len()).unwrap().to_le_bytes());
        body.extend_from_slice(&[0, 0]);
        for c in chars {
            body.extend_from_slice(c);
        }
        table(b"MPST", 1, &body)
    }

    #[test]
    fn mpst_decodes_nodes_and_characteristics() {
        let t = mpst(
            &[
                node_bytes(1, &[(0, 0), (1, 1)], &[7]),
                node_bytes(1, &[(0, 0)], &[]),
            ],
            &[characteristics(0x41), characteristics(0x41)],
        );
        let m = Mpst::parse(&t).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(m.channel, 4);
        let nodes: Vec<_> = m.nodes().map(Result::unwrap).collect();
        assert_eq!(nodes.len(), 2);
        assert_eq!(
            nodes[0].power_states().collect::<Vec<_>>(),
            [(0, 0), (1, 1)]
        );
        assert_eq!(nodes[0].components, &[7, 0]);
        assert!(nodes[0].enabled && nodes[0].power_managed && !nodes[0].hot_pluggable);
        let c = m.characteristics(1).unwrap();
        assert_eq!(
            (c.revision, c.exit_latency_ns, c.content_preserved),
            (1, 5000, true)
        );
        let s = census::check(&t).unwrap();
        assert!(matches!(s.class, census::Class::Checked(s) if s.structures() == 4));
    }

    #[test]
    fn mpst_refuses_by_name() {
        let kind = |t: Vec<u8>| Mpst::parse(&t).unwrap_err().kind;
        assert_eq!(
            kind(mpst(
                &[node_bytes(1, &[(1, 2)], &[])],
                &[characteristics(0x41)]
            )),
            ErrorKind::Reference {
                field: "Power State Information Index"
            }
        );
        assert_eq!(
            kind(mpst(
                &[node_bytes(1, &[(1, 0)], &[])],
                &[characteristics(0x42)]
            )),
            ErrorKind::Value {
                field: "Power State Structure ID"
            }
        );
        let mut short = node_bytes(1, &[(1, 0)], &[]);
        short[4] = 33;
        assert!(matches!(
            kind(mpst(&[short], &[characteristics(0x41)])),
            ErrorKind::Structure { .. }
        ));
        let mut long = node_bytes(1, &[], &[]);
        long[4] = 0xFF;
        assert!(matches!(kind(mpst(&[long], &[])), ErrorKind::Offset { .. }));
        let mut wraps = node_bytes(1, &[], &[]);
        wraps[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(matches!(kind(mpst(&[wraps], &[])), ErrorKind::Value { .. }));
        let mut t = mpst(&[], &[characteristics(0x41)]);
        t.truncate(t.len() - 1);
        crate::testing::seal(&mut t);
        assert!(matches!(
            Mpst::parse(&t).unwrap_err().kind,
            ErrorKind::Offset { .. }
        ));
    }

    #[test]
    fn mpst_channel_decodes() {
        let mut c = vec![0u8; 40];
        c[..4].copy_from_slice(&0x5043_4304u32.to_le_bytes());
        c[24..32].copy_from_slice(&u64::MAX.to_le_bytes());
        let ch = MpstChannel::parse(&c, 4).unwrap();
        assert_eq!((ch.energy, ch.expected_power), (None, Some(0)));
        assert!(MpstChannel::parse(&c, 5).is_err());
        assert!(MpstChannel::parse(&c[..39], 4).is_err());
    }

    fn dev(kind: u8, flags: u16, children: u32, extra: &[u8]) -> Vec<u8> {
        let len = 12 + extra.len();
        let mut d = vec![kind, 0];
        d.extend_from_slice(&u16::try_from(len).unwrap().to_le_bytes());
        d.extend_from_slice(&flags.to_le_bytes());
        d.extend_from_slice(&[0, 0]);
        d.extend_from_slice(&children.to_le_bytes());
        d.extend_from_slice(extra);
        d
    }

    fn pmtt(top: u32, devices: &[Vec<u8>]) -> Vec<u8> {
        let mut body = top.to_le_bytes().to_vec();
        for d in devices {
            body.extend_from_slice(d);
        }
        table(b"PMTT", 2, &body)
    }

    #[test]
    fn pmtt_walks_its_tree() {
        let t = pmtt(
            1,
            &[
                dev(0, 0b11, 1, &[1, 0, 0, 0]),
                dev(1, 0b10, 2, &[0, 0, 0, 0]),
                dev(2, 0b10, 0, &[0x10, 0, 0, 0]),
                dev(0xFF, 0, 0, &[0xAB; 20]),
            ],
        );
        let p = Pmtt::parse(&t).unwrap_or_else(|e| panic!("{e}"));
        let mut seen = Vec::new();
        p.walk(&mut |n| seen.push((n.depth, n.device))).unwrap();
        assert_eq!(seen.len(), 4);
        assert_eq!(seen[0], (0, MemoryDevice::Socket { id: 1 }));
        assert_eq!(
            seen[2],
            (
                2,
                MemoryDevice::Dimm {
                    smbios_handle: Some(0x10)
                }
            )
        );
        assert!(matches!(seen[3], (2, MemoryDevice::Vendor { data, .. }) if data == [0xAB; 4]));
    }

    #[test]
    fn pmtt_refuses_by_name() {
        let kind = |t: Vec<u8>| Pmtt::parse(&t).unwrap_err().kind;
        assert_eq!(kind(table(b"PMTT", 1, &[0; 4])), ErrorKind::Revision(1));
        assert!(matches!(
            kind(pmtt(1, &[dev(0, 1, 0, &[0; 3])])),
            ErrorKind::Structure { .. }
        ));
        assert!(matches!(
            kind(pmtt(1, &[dev(0, 1, 1, &[0; 4]), dev(1, 1, 0, &[0; 4])])),
            ErrorKind::Inconsistent { .. }
        ));
        assert!(matches!(
            kind(pmtt(2, &[dev(0, 1, 0, &[0; 4])])),
            ErrorKind::Truncated { .. }
        ));
        assert!(matches!(
            kind(pmtt(1, &[dev(0, 1, 0, &[0; 4]), dev(0, 1, 0, &[0; 4])])),
            ErrorKind::Offset { .. }
        ));
        // Nested past the limit: each device has one child.
        let mut deep = Vec::new();
        for i in 0..=PMTT_MAX_DEPTH {
            deep.push(dev(0xFE, u16::from(i == 0), 1, &[0; 4]));
        }
        assert!(matches!(
            kind(pmtt(1, &deep)),
            ErrorKind::Value {
                field: "memory devices nested too deep"
            }
        ));
    }
}
