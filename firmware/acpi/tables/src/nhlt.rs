//! The Non HD Audio Link Table, signature `NHLT` (ACPI 6.6 §5.2.27): the audio endpoints on I2S,
//! PDM and similar links, with the stream formats and configuration their DSP firmware is given.
//!
//! Each nested size is checked against what holds it: the endpoints against the table, each
//! endpoint's device configuration, formats and secondary device information against the
//! endpoint, each format's configuration against its format. The configuration bytes themselves
//! are the DSP's and are handed on, not read.

use crate::census::Tally;
use crate::error::{Error, ErrorKind};
use crate::header::{self, Table};
use crate::read::{Bytes, Guid};

/// Bytes in an endpoint descriptor before its device configuration.
const ENDPOINT_FIXED: usize = 19;
/// Bytes in a `WAVEFORMATEXTENSIBLE`.
const WAVE_FORMAT: usize = 40;

/// The PCM subformat (§5.2.27.4).
pub const PCM: Guid = Guid::from_fields(
    0x0000_0001,
    0x0000,
    0x0010,
    [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
);

/// One stream format an endpoint supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format<'a> {
    /// Channels.
    pub channels: u16,
    /// Samples per second.
    pub sample_rate: u32,
    /// Bits each sample's container holds.
    pub bits_per_sample: u16,
    /// Bits of each sample that carry data.
    pub valid_bits: u16,
    /// The speaker positions.
    pub channel_mask: u32,
    /// The data's subtype ([`PCM`], say).
    pub subformat: Guid,
    /// The configuration the DSP firmware is given for this format.
    pub config: &'a [u8],
}

/// One audio endpoint.
#[derive(Clone, Copy, Debug)]
pub struct Endpoint<'a> {
    bytes: Bytes<'a>,
    /// The link: 2 PDM, 3 SSP (I2S); 0, 1, 4, 5 and 6 are named and unused.
    pub link_type: u8,
    /// Its instance on its link.
    pub instance: u8,
    /// Its vendor.
    pub vendor_id: u16,
    /// Its device.
    pub device_id: u16,
    /// Its revision.
    pub revision_id: u16,
    /// Its subsystem.
    pub subsystem_id: u32,
    /// Its kind, which its link type gives the meaning of.
    pub device_type: u8,
    /// 0 playback, 1 capture.
    pub direction: u8,
    /// The SSP port, or 0 for PDM.
    pub virtual_bus: u8,
    /// The device configuration's capabilities.
    pub device_config: &'a [u8],
    formats_at: usize,
    format_count: u8,
}

impl<'a> Endpoint<'a> {
    /// Its stream formats.
    pub fn formats(&self) -> impl Iterator<Item = Result<Format<'a>, Error>> + 'a {
        let b = self.bytes;
        let mut at = self.formats_at.saturating_add(1);
        (0..self.format_count).map(move |_| {
            let (format, next) = format(&b, at)?;
            at = next;
            Ok(format)
        })
    }
}

/// A configuration space (§5.2.27.2): a 32-bit size and that many bytes. Returns them and where
/// the space ends.
fn config<'a>(b: &Bytes<'a>, at: usize, field: &'static str) -> Result<(&'a [u8], usize), Error> {
    let size = usize::try_from(b.u32(at)?).unwrap_or(usize::MAX);
    let start = at.saturating_add(4);
    let bytes = b
        .get(start, size)
        .map_err(|_| b.error(at, ErrorKind::Offset { field }))?;
    Ok((bytes, start.saturating_add(size)))
}

/// The format at `at`: a `WAVEFORMATEXTENSIBLE` and its configuration. Refused: a format that is
/// not the extensible kind (tag 0xFFFE, extension 22 bytes), or whose sizes disagree as §5.2.27.4
/// forbids (a container not a whole number of bytes, smaller than the valid bits, or a block that
/// is not one container per channel).
fn format<'a>(b: &Bytes<'a>, at: usize) -> Result<(Format<'a>, usize), Error> {
    let w = b.sub(at, WAVE_FORMAT)?;
    let value = |off, field| Err(w.error(off, ErrorKind::Value { field }));
    if w.u16(0)? != 0xFFFE {
        return value(0, "Format Tag");
    }
    if w.u16(16)? != 22 {
        return value(16, "Size");
    }
    let channels = w.u16(2)?;
    let bits = w.u16(14)?;
    let valid = w.u16(18)?;
    if bits % 8 != 0 || valid > bits {
        return value(14, "Bits Per Sample");
    }
    if u32::from(w.u16(12)?) != u32::from(channels).saturating_mul(u32::from(bits / 8)) {
        return value(12, "Block Align");
    }
    let (config, end) = config(b, at.saturating_add(WAVE_FORMAT), "Config CapabilitiesSize")?;
    Ok((
        Format {
            channels,
            sample_rate: w.u32(4)?,
            bits_per_sample: bits,
            valid_bits: valid,
            channel_mask: w.u32(20)?,
            subformat: w.guid(24)?,
            config,
        },
        end,
    ))
}

/// The endpoint at `at`; refused unless its parts fit inside its own length, and fill it to the
/// byte when there is no secondary device information.
fn endpoint<'a>(b: &Bytes<'a>, at: usize) -> Result<Endpoint<'a>, Error> {
    let length = usize::try_from(b.u32(at)?).unwrap_or(usize::MAX);
    let e = b.sub(at, length).map_err(|_| {
        b.error(
            at,
            ErrorKind::Offset {
                field: "Endpoint Length",
            },
        )
    })?;
    e.require(ENDPOINT_FIXED.saturating_add(4).saturating_add(1))?;
    let (device_config, formats_at) = config(&e, ENDPOINT_FIXED, "DeviceConfig CapabilitiesSize")?;
    let format_count = e.u8(formats_at)?;
    let mut next = formats_at.saturating_add(1);
    for _ in 0..format_count {
        next = format(&e, next)?.1;
    }
    if next != e.len() {
        // What is left is the obsolete secondary device information: a count and 18 bytes each.
        let count = usize::from(e.u8(next)?);
        let end = next
            .saturating_add(1)
            .saturating_add(count.saturating_mul(18));
        if end != e.len() {
            return Err(e.error(
                next,
                ErrorKind::Offset {
                    field: "Endpoint Length",
                },
            ));
        }
    }
    Ok(Endpoint {
        bytes: e,
        link_type: e.u8(4)?,
        instance: e.u8(5)?,
        vendor_id: e.u16(6)?,
        device_id: e.u16(8)?,
        revision_id: e.u16(10)?,
        subsystem_id: e.u32(12)?,
        device_type: e.u8(16)?,
        direction: e.u8(17)?,
        virtual_bus: e.u8(18)?,
        device_config,
        formats_at,
        format_count,
    })
}

/// The checked NHLT.
#[derive(Clone, Copy, Debug)]
pub struct Nhlt<'a> {
    table: Table<'a>,
    /// The configuration for the OS's offload driver after the endpoints, if any.
    pub oed_config: Option<&'a [u8]>,
}

impl<'a> Nhlt<'a> {
    /// Checks the table: every endpoint, as many as it says, then either nothing or exactly one
    /// configuration space to the table's end.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = header::check_signature(bytes, b"NHLT")?;
        let b = table.reader();
        b.require(37)?;
        let mut at = 37usize;
        for _ in 0..b.u8(36)? {
            let e = endpoint(&b, at)?;
            for format in e.formats() {
                format?;
            }
            at = at.saturating_add(e.bytes.len());
        }
        let oed_config = if at == b.len() {
            None
        } else {
            let (config, end) = config(&b, at, "OED Config")?;
            if end != b.len() {
                return Err(b.error(
                    at,
                    ErrorKind::Offset {
                        field: "OED Config",
                    },
                ));
            }
            Some(config)
        };
        Ok(Self { table, oed_config })
    }

    /// The endpoints, in order.
    pub fn endpoints(&self) -> impl Iterator<Item = Result<Endpoint<'a>, Error>> + 'a {
        let b = self.table.reader();
        let count = b.u8(36).unwrap_or(0);
        let mut at = 37usize;
        (0..count).map(move |_| {
            let e = endpoint(&b, at)?;
            at = at.saturating_add(e.bytes.len());
            Ok(e)
        })
    }

    pub(crate) fn census(&self, tally: &mut Tally<'_, 'a>) -> Result<(), Error> {
        for e in self.endpoints() {
            let e = e?;
            tally.structure(u16::from(e.link_type), e.bytes.base(), e.bytes.bytes());
            tally.bytes(e.bytes.base(), e.device_config);
            for f in e.formats() {
                tally.bytes(e.bytes.base(), f?.config);
            }
        }
        if let Some(oed) = self.oed_config {
            tally.bytes(0, oed);
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
        vec![nhlt(
            &[
                endpoint_bytes(
                    &[wave(2, 32, 24, 8, &[1, 2, 3]), wave(4, 16, 16, 8, &[])],
                    None,
                ),
                endpoint_bytes(&[], Some(1)),
            ],
            Some(&[9, 9]),
        )]
    }

    fn wave(channels: u16, bits: u16, valid: u16, align: u16, config: &[u8]) -> Vec<u8> {
        let mut w = 0xFFFEu16.to_le_bytes().to_vec();
        w.extend_from_slice(&channels.to_le_bytes());
        w.extend_from_slice(&48_000u32.to_le_bytes());
        w.extend_from_slice(&(48_000u32 * u32::from(align)).to_le_bytes());
        w.extend_from_slice(&align.to_le_bytes());
        w.extend_from_slice(&bits.to_le_bytes());
        w.extend_from_slice(&22u16.to_le_bytes());
        w.extend_from_slice(&valid.to_le_bytes());
        w.extend_from_slice(&3u32.to_le_bytes());
        w.extend_from_slice(&PCM.0);
        w.extend_from_slice(&u32::try_from(config.len()).unwrap().to_le_bytes());
        w.extend_from_slice(config);
        w
    }

    fn endpoint_bytes(formats: &[Vec<u8>], devices: Option<u8>) -> Vec<u8> {
        let mut e = vec![0u8; 4];
        e.extend_from_slice(&[2, 0]);
        e.extend_from_slice(&0x8086u16.to_le_bytes());
        e.extend_from_slice(&0xAE20u16.to_le_bytes());
        e.extend_from_slice(&1u16.to_le_bytes());
        e.extend_from_slice(&0u32.to_le_bytes());
        e.extend_from_slice(&[0, 1, 0]);
        e.extend_from_slice(&3u32.to_le_bytes());
        e.extend_from_slice(&[0, 1, 0x0A]);
        e.push(u8::try_from(formats.len()).unwrap());
        for f in formats {
            e.extend_from_slice(f);
        }
        if let Some(n) = devices {
            e.push(n);
            e.extend_from_slice(&vec![0x41; 18 * usize::from(n)]);
        }
        let len = u32::try_from(e.len()).unwrap();
        e[..4].copy_from_slice(&len.to_le_bytes());
        e
    }

    fn nhlt(endpoints: &[Vec<u8>], oed: Option<&[u8]>) -> Vec<u8> {
        let mut body = vec![u8::try_from(endpoints.len()).unwrap()];
        for e in endpoints {
            body.extend_from_slice(e);
        }
        if let Some(c) = oed {
            body.extend_from_slice(&u32::try_from(c.len()).unwrap().to_le_bytes());
            body.extend_from_slice(c);
        }
        table(b"NHLT", 1, &body)
    }

    #[test]
    fn decodes_endpoints_and_formats() {
        let t = nhlt(
            &[
                endpoint_bytes(
                    &[wave(2, 32, 24, 8, &[1, 2, 3]), wave(4, 16, 16, 8, &[])],
                    None,
                ),
                endpoint_bytes(&[], Some(2)),
            ],
            Some(&[9, 9]),
        );
        let n = Nhlt::parse(&t).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(n.oed_config, Some(&[9, 9][..]));
        let eps: Vec<_> = n.endpoints().map(Result::unwrap).collect();
        assert_eq!(
            (eps[0].link_type, eps[0].device_id, eps[0].direction),
            (2, 0xAE20, 1)
        );
        assert_eq!(eps[0].device_config, &[0, 1, 0x0A]);
        let formats: Vec<_> = eps[0].formats().map(Result::unwrap).collect();
        assert_eq!(
            (
                formats[0].channels,
                formats[0].valid_bits,
                formats[0].config
            ),
            (2, 24, &[1, 2, 3][..])
        );
        assert_eq!(formats[1].subformat, PCM);
        assert!(
            matches!(census::check(&t).unwrap().class, census::Class::Checked(s) if s.structures() == 2)
        );
    }

    #[test]
    fn sizes_and_formats_are_checked() {
        let kind = |t: Vec<u8>| Nhlt::parse(&t).unwrap_err().kind;
        let v = |field| ErrorKind::Value { field };
        assert_eq!(
            kind(nhlt(
                &[endpoint_bytes(&[wave(2, 32, 24, 4, &[])], None)],
                None
            )),
            v("Block Align")
        );
        assert_eq!(
            kind(nhlt(
                &[endpoint_bytes(&[wave(2, 12, 12, 2, &[])], None)],
                None
            )),
            v("Bits Per Sample")
        );
        assert_eq!(
            kind(nhlt(
                &[endpoint_bytes(&[wave(2, 16, 24, 4, &[])], None)],
                None
            )),
            v("Bits Per Sample")
        );
        let mut tag = wave(2, 16, 16, 4, &[]);
        tag[0] = 1;
        assert_eq!(
            kind(nhlt(&[endpoint_bytes(&[tag], None)], None)),
            v("Format Tag")
        );
        let mut long = endpoint_bytes(&[], None);
        long[0] += 2;
        long.extend_from_slice(&[0, 0]);
        assert!(matches!(
            kind(nhlt(&[long], None)),
            ErrorKind::Offset { .. }
        ));
        let mut cfg = endpoint_bytes(&[], None);
        cfg[19] = 200;
        assert!(matches!(kind(nhlt(&[cfg], None)), ErrorKind::Offset { .. }));
        let mut t = nhlt(&[endpoint_bytes(&[], None)], Some(&[1]));
        t.push(0);
        crate::testing::seal(&mut t);
        assert!(matches!(
            kind(t),
            ErrorKind::Offset {
                field: "OED Config"
            }
        ));
        let mut two = nhlt(&[endpoint_bytes(&[], None)], None);
        two[36] = 2;
        crate::testing::seal(&mut two);
        assert!(Nhlt::parse(&two).is_err());
    }
}
