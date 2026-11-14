//! The RAS feature tables: the RASF (ACPI 6.6 §5.2.20) and the RAS2 (§5.2.21).
//!
//! Each table names Platform Communications Channel subspaces; the commands themselves pass
//! through those channels' shared memory, which is ADR-0019's area H. [`RasfChannel`] and
//! [`Ras2Channel`] decode that memory's layout from bytes their caller supplies: nothing here
//! reads or writes a channel.

use crate::census::Tally;
use crate::error::{Error, ErrorKind, Signature};
use crate::header::{self, Table};
use crate::read::Bytes;
use crate::walk::{Shape, Walk, Width, body};

/// A parameter block's shape in a channel: a 16-bit type, a 16-bit version, a 16-bit length.
const BLOCK: Shape = Shape {
    kind_at: 0,
    kind: Width::U16,
    len_at: 4,
    len: Width::U16,
};

/// The RASF (§5.2.20): which PCC subspace carries its commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rasf {
    /// The channel identifier, 12 bytes as the table holds them; §5.2.20 does not say which of
    /// them carries the subspace's ID, so none is singled out.
    pub channel: [u8; 12],
}

impl Rasf {
    /// Checks the table.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let b = header::check_signature(bytes, b"RASF")?.reader();
        b.require(48)?;
        Ok(Self {
            channel: b.array(36)?,
        })
    }
}

/// A RAS feature's parameter block in a channel's shared memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParameterBlock<'a> {
    /// Patrol scrubbing (type 0): the fields common to the RASF's and the RAS2's layouts.
    PatrolScrub {
        /// The block's version.
        version: u16,
        /// The command the OS asks for (1 get, 2 start, 3 stop).
        command: u16,
        /// The range the OS asks to scrub: base and size.
        requested: (u64, u64),
        /// The range the platform will scrub: base and size.
        actual: (u64, u64),
        /// The scrubber already runs over the actual range.
        running: bool,
    },
    /// Logical-to-physical address translation (RAS2 type 1).
    La2Pa {
        /// The block's version.
        version: u16,
        /// The logical address to translate.
        logical: u64,
        /// The physical address the platform answers.
        physical: u64,
        /// 0: the translation succeeded.
        status: u32,
    },
    /// Address translation either way (RAS2 type 2).
    AddressTranslation {
        /// The block's version.
        version: u16,
        /// The physical address in or out.
        physical: u64,
        /// The status the platform answers.
        status: u32,
        /// The logical address's type (1: DDR4/DDR5, 0xFF: vendor).
        logical_type: u16,
        /// The logical address, as its length gives it.
        logical: &'a [u8],
    },
    /// Another type, kept as bytes.
    Other {
        /// Its type.
        kind: u16,
        /// Its bytes.
        bytes: &'a [u8],
    },
}

/// The parameter blocks after a channel's 48-byte header: each fits, and no type appears twice
/// (§5.2.20.3, §5.2.21.1).
fn blocks(b: &Bytes<'_>, count: u16, ras2: bool) -> Result<(), Error> {
    let run = b.rest(48)?;
    let mut seen = 0u32;
    let mut walked = 0u16;
    for raw in Walk::new(run, BLOCK).take(usize::from(count)) {
        let raw = raw?;
        if let Some(bit) = 1u32.checked_shl(u32::from(raw.kind)) {
            if seen & bit != 0 {
                return Err(Error::new(
                    b.table(),
                    raw.at,
                    ErrorKind::Inconsistent {
                        field: "a second parameter block of one type",
                    },
                ));
            }
            seen |= bit;
        }
        decode_block(&raw, b.table(), ras2)?;
        walked = walked.saturating_add(1);
    }
    if walked != count {
        return Err(b.error(
            42,
            ErrorKind::Offset {
                field: "Number of Parameter Blocks",
            },
        ));
    }
    Ok(())
}

fn decode_block<'a>(
    raw: &crate::Raw<'a>,
    table: Signature,
    ras2: bool,
) -> Result<ParameterBlock<'a>, Error> {
    Ok(match (raw.kind, ras2) {
        (0, _) => {
            let b = body(raw, table, if ras2 { 52 } else { 43 })?;
            ParameterBlock::PatrolScrub {
                version: b.u16(2)?,
                command: b.u16(6)?,
                requested: (b.u64(8)?, b.u64(16)?),
                actual: (b.u64(24)?, b.u64(32)?),
                running: b.u8(40)? & 1 != 0,
            }
        }
        (1, true) => {
            let b = body(raw, table, 36)?;
            ParameterBlock::La2Pa {
                version: b.u16(2)?,
                logical: b.u64(16)?,
                physical: b.u64(24)?,
                status: b.u32(32)?,
            }
        }
        (2, true) => {
            let b = body(raw, table, 28)?;
            let len = usize::from(b.u16(26)?);
            let logical = b.get(28, len).map_err(|_| {
                b.error(
                    26,
                    ErrorKind::Offset {
                        field: "Logical Address Length",
                    },
                )
            })?;
            ParameterBlock::AddressTranslation {
                version: b.u16(2)?,
                physical: b.u64(8)?,
                status: b.u32(16)?,
                logical_type: b.u16(24)?,
                logical,
            }
        }
        (kind, _) => ParameterBlock::Other {
            kind,
            bytes: raw.bytes,
        },
    })
}

/// A RASF channel's shared memory (§5.2.20.3), decoded from bytes the caller read.
#[derive(Clone, Copy, Debug)]
pub struct RasfChannel<'a> {
    bytes: Bytes<'a>,
    /// The command the OS writes (1: execute).
    pub command: u16,
    /// The PCC status.
    pub status: u16,
    /// The minor (low byte) and major (high byte) version.
    pub version: u16,
    /// The features the platform supports.
    pub capabilities: u128,
    /// The features the OS commands.
    pub set_capabilities: u128,
    /// How many parameter blocks follow.
    pub block_count: u16,
    /// The command's result.
    pub set_status: u32,
}

/// A channel's 48-byte header, read in common for the RASF and the RAS2.
fn channel_header<'a>(
    bytes: &'a [u8],
    table: Signature,
    signature: u32,
) -> Result<(Bytes<'a>, [u16; 4], [u128; 2], u32), Error> {
    let b = Bytes::new(bytes, table);
    b.require(48)?;
    if b.u32(0)? != signature {
        return Err(b.error(0, ErrorKind::Value { field: "Signature" }));
    }
    let caps = [
        u128::from_le_bytes(b.array(10)?),
        u128::from_le_bytes(b.array(26)?),
    ];
    Ok((
        b,
        [b.u16(4)?, b.u16(6)?, b.u16(8)?, b.u16(42)?],
        caps,
        b.u32(44)?,
    ))
}

impl<'a> RasfChannel<'a> {
    /// The value a RASF channel's signature holds: `RASF` as a little-endian number (§5.2.20.3).
    pub const SIGNATURE: u32 = 0x5241_5346;

    /// Decodes a RASF channel's shared memory: its signature, its header, and every parameter
    /// block, none twice.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let (
            b,
            [command, status, version, block_count],
            [capabilities, set_capabilities],
            set_status,
        ) = channel_header(bytes, Signature::new(b"RASF"), Self::SIGNATURE)?;
        blocks(&b, block_count, false)?;
        Ok(Self {
            bytes: b,
            command,
            status,
            version,
            capabilities,
            set_capabilities,
            block_count,
            set_status,
        })
    }

    /// The parameter blocks, in order.
    pub fn blocks(&self) -> impl Iterator<Item = Result<ParameterBlock<'a>, Error>> + 'a {
        block_iter(self.bytes, self.block_count, false)
    }
}

fn block_iter<'a>(
    b: Bytes<'a>,
    count: u16,
    ras2: bool,
) -> impl Iterator<Item = Result<ParameterBlock<'a>, Error>> + 'a {
    let table = b.table();
    b.rest(48)
        .ok()
        .into_iter()
        .flat_map(move |run| Walk::new(run, BLOCK).take(usize::from(count)))
        .map(move |raw| raw.and_then(|raw| decode_block(&raw, table, ras2)))
}

/// One of the RAS2's PCC descriptors (§5.2.21, Table 5.92).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ras2Descriptor {
    /// The PCC subspace's index.
    pub pcc_id: u8,
    /// The kind of component it serves: 0 memory; 1 to 0x7F reserved; 0x80 and above the vendor's.
    pub feature_type: u8,
    /// Which component: for memory, a proximity domain of the SRAT.
    pub instance: u32,
}

/// The checked RAS2 (§5.2.21).
#[derive(Clone, Copy, Debug)]
pub struct Ras2<'a> {
    table: Table<'a>,
    count: u16,
}

impl<'a> Ras2<'a> {
    /// Checks the table: its descriptors, as many as it says, fit.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = header::check_signature(bytes, b"RAS2")?;
        let b = table.reader();
        b.require(40)?;
        let count = b.u16(38)?;
        b.array_of(40, usize::from(count), 8, "Number of PCC descriptors")?;
        Ok(Self { table, count })
    }

    /// Its descriptors.
    pub fn descriptors(&self) -> impl Iterator<Item = Ras2Descriptor> + 'a {
        self.table
            .bytes()
            .get(40..)
            .unwrap_or(&[])
            .as_chunks::<8>()
            .0
            .iter()
            .take(usize::from(self.count))
            .map(
                |&[pcc_id, _, _, feature_type, i0, i1, i2, i3]| Ras2Descriptor {
                    pcc_id,
                    feature_type,
                    instance: u32::from_le_bytes([i0, i1, i2, i3]),
                },
            )
    }

    pub(crate) fn census(&self, tally: &mut Tally<'_, 'a>) -> Result<(), Error> {
        let b = self.table.reader();
        for (i, d) in self.descriptors().enumerate() {
            let at = i.saturating_mul(8).saturating_add(40);
            tally.structure(u16::from(d.feature_type), at, b.get(at, 8)?);
        }
        Ok(())
    }
}

/// A RAS2 channel's shared memory (§5.2.21.1), decoded from bytes the caller read.
#[derive(Clone, Copy, Debug)]
pub struct Ras2Channel<'a> {
    bytes: Bytes<'a>,
    /// The command the OS writes (1: execute).
    pub command: u16,
    /// The PCC status.
    pub status: u16,
    /// The features the platform supports for this instance.
    pub features: u128,
    /// The features the OS commands.
    pub set_capabilities: u128,
    /// How many parameter blocks follow.
    pub block_count: u16,
    /// The command's result.
    pub set_status: u32,
}

impl<'a> Ras2Channel<'a> {
    /// Decodes the shared memory of PCC subspace `subspace`. Refused: a signature other than
    /// `0x50434300` with the subspace's ID in its low byte, a version other than 0 (the only
    /// layout §5.2.21.1 defines), and two parameter blocks of one type.
    pub fn parse(bytes: &'a [u8], subspace: u8) -> Result<Self, Error> {
        let signature = 0x5043_4300 | u32::from(subspace);
        let (b, [command, status, version, block_count], [features, set_capabilities], set_status) =
            channel_header(bytes, Signature::new(b"RAS2"), signature)?;
        if version != 0 {
            return Err(b.error(8, ErrorKind::Value { field: "Version" }));
        }
        blocks(&b, block_count, true)?;
        Ok(Self {
            bytes: b,
            command,
            status,
            features,
            set_capabilities,
            block_count,
            set_status,
        })
    }

    /// The parameter blocks, in order.
    pub fn blocks(&self) -> impl Iterator<Item = Result<ParameterBlock<'a>, Error>> + 'a {
        block_iter(self.bytes, self.block_count, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::table;

    #[test]
    fn rasf_names_its_channel() {
        let mut body = [0u8; 12];
        body[0] = 7;
        let r = Rasf::parse(&table(b"RASF", 1, &body)).unwrap();
        assert_eq!(r.channel[0], 7);
        assert!(Rasf::parse(&table(b"RASF", 1, &body[..11])).is_err());
    }

    fn ras2(descriptors: &[[u8; 8]], count: u16) -> Vec<u8> {
        let mut body = vec![0, 0];
        body.extend_from_slice(&count.to_le_bytes());
        for d in descriptors {
            body.extend_from_slice(d);
        }
        table(b"RAS2", 1, &body)
    }

    #[test]
    fn ras2_lists_its_descriptors() {
        let t = ras2(&[[3, 0, 0, 0, 1, 0, 0, 0], [4, 0, 0, 0x80, 2, 0, 0, 0]], 2);
        let r = Ras2::parse(&t).unwrap();
        let all: Vec<_> = r.descriptors().collect();
        assert_eq!(
            all[1],
            Ras2Descriptor {
                pcc_id: 4,
                feature_type: 0x80,
                instance: 2
            }
        );
        assert!(matches!(
            Ras2::parse(&ras2(&[[0; 8]], 2)).unwrap_err().kind,
            ErrorKind::Offset { .. }
        ));
    }

    fn channel(signature: u32, version: u16, blocks: &[Vec<u8>]) -> Vec<u8> {
        let mut c = vec![0u8; 48];
        c[..4].copy_from_slice(&signature.to_le_bytes());
        c[4..6].copy_from_slice(&1u16.to_le_bytes());
        c[8..10].copy_from_slice(&version.to_le_bytes());
        c[10] = 0b11;
        c[42..44].copy_from_slice(&u16::try_from(blocks.len()).unwrap().to_le_bytes());
        for b in blocks {
            c.extend_from_slice(b);
        }
        c
    }

    fn block(kind: u16, len: u16) -> Vec<u8> {
        let mut b = vec![0u8; usize::from(len)];
        b[..2].copy_from_slice(&kind.to_le_bytes());
        b[4..6].copy_from_slice(&len.to_le_bytes());
        b
    }

    #[test]
    fn channels_decode_their_blocks() {
        let mut scrub = block(0, 43);
        scrub[8..16].copy_from_slice(&0x1000u64.to_le_bytes());
        scrub[40] = 1;
        let c = channel(RasfChannel::SIGNATURE, 0x0100, &[scrub.clone()]);
        let rasf = RasfChannel::parse(&c).unwrap();
        assert_eq!(rasf.capabilities, 0b11);
        assert!(matches!(
            rasf.blocks().next(),
            Some(Ok(ParameterBlock::PatrolScrub {
                requested: (0x1000, 0),
                running: true,
                ..
            }))
        ));
        let mut at = block(2, 34);
        at[26..28].copy_from_slice(&6u16.to_le_bytes());
        let c = channel(0x5043_4305, 0, &[block(0, 56), block(1, 36), at]);
        let ras2 = Ras2Channel::parse(&c, 5).unwrap();
        let all: Vec<_> = ras2.blocks().map(Result::unwrap).collect();
        assert!(
            matches!(all[2], ParameterBlock::AddressTranslation { logical, .. } if logical.len() == 6)
        );
    }

    #[test]
    fn channels_refuse_what_they_must() {
        let field = |r: Result<RasfChannel<'_>, Error>| r.unwrap_err().kind;
        assert_eq!(
            field(RasfChannel::parse(&channel(0, 0, &[]))),
            ErrorKind::Value { field: "Signature" }
        );
        let twice = channel(RasfChannel::SIGNATURE, 0, &[block(0, 43), block(0, 43)]);
        assert!(matches!(
            field(RasfChannel::parse(&twice)),
            ErrorKind::Inconsistent { .. }
        ));
        let short = channel(RasfChannel::SIGNATURE, 0, &[block(0, 42)]);
        let mut missing = channel(RasfChannel::SIGNATURE, 0, &[block(0, 43)]);
        missing[42] = 2;
        assert!(matches!(
            field(RasfChannel::parse(&missing)),
            ErrorKind::Offset { .. }
        ));
        assert!(matches!(
            field(RasfChannel::parse(&short)),
            ErrorKind::Structure { .. }
        ));
        assert!(matches!(
            Ras2Channel::parse(&channel(0x5043_4305, 0, &[]), 6)
                .unwrap_err()
                .kind,
            ErrorKind::Value { field: "Signature" }
        ));
        assert!(matches!(
            Ras2Channel::parse(&channel(0x5043_4305, 1, &[]), 5)
                .unwrap_err()
                .kind,
            ErrorKind::Value { field: "Version" }
        ));
        let mut long = block(2, 34);
        long[26..28].copy_from_slice(&7u16.to_le_bytes());
        assert!(matches!(
            Ras2Channel::parse(&channel(0x5043_4305, 0, &[long]), 5)
                .unwrap_err()
                .kind,
            ErrorKind::Offset { .. }
        ));
        assert!(RasfChannel::parse(&[0; 47]).is_err());
    }
}
