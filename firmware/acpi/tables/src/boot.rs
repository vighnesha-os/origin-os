//! What the firmware leaves about the boot: the Boot Graphics Resource Table (BGRT, ACPI 6.6
//! §5.2.23) and the Firmware Performance Data Table (FPDT, §5.2.24) with the tables it points to.
//!
//! The BGRT's image is never read here: it is in memory the firmware gave back at
//! `ExitBootServices`. The FPDT's pointed-to tables (FBPT, S3PT, MBPT) have no system description
//! table header and no checksum; [`PerformanceTable::parse`] checks them from bytes the caller
//! read, through memory the firmware owns.

use crate::census::Tally;
use crate::error::{Error, ErrorKind, Signature};
use crate::header::{self, Table};
use crate::read::{Bytes, Guid};
use crate::walk::{Raw, Shape, Walk, Width, body};

/// A performance record's shape: a 16-bit type, then a one-byte length, then a revision.
const RECORD: Shape = Shape {
    kind_at: 0,
    kind: Width::U16,
    len_at: 2,
    len: Width::U8,
};

/// The image type the BGRT names (§5.2.23.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageType {
    /// A bitmap (0).
    Bitmap,
    /// A value ACPI 6.6 reserves.
    Reserved(u8),
}

/// The BGRT, decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bgrt {
    /// Its version; §5.2.23.1 gives 1.
    pub version: u16,
    /// The image is on the screen now.
    pub displayed: bool,
    /// The image's rotation clockwise from its usual orientation, in degrees: 0, 90, 180 or 270.
    pub orientation: u16,
    /// What the image is.
    pub image_type: ImageType,
    /// Where the firmware's copy of the image is.
    pub image_address: u64,
    /// Its left edge on the screen.
    pub x: u32,
    /// Its top edge on the screen.
    pub y: u32,
}

impl Bgrt {
    /// Checks the table and reads its fields.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let b = header::check_signature(bytes, b"BGRT")?.reader();
        b.require(56)?;
        let status = b.u8(38)?;
        let image_type = match b.u8(39)? {
            0 => ImageType::Bitmap,
            other => ImageType::Reserved(other),
        };
        Ok(Self {
            version: b.u16(36)?,
            displayed: status & 1 != 0,
            orientation: match (status >> 1) & 0b11 {
                0 => 0,
                1 => 90,
                2 => 180,
                _ => 270,
            },
            image_type,
            image_address: b.u64(40)?,
            x: b.u32(48)?,
            y: b.u32(52)?,
        })
    }
}

/// A record of the FPDT itself (§5.2.24.3 to §5.2.24.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FpdtRecord<'a> {
    /// Where the host firmware's boot performance table is (type 0).
    Fbpt(u64),
    /// Where the S3 performance table is (type 1).
    S3pt(u64),
    /// Where a microcontroller's boot performance table is (type 2).
    Mbpt(u64),
    /// The offset between one timestamp domain and the first (type 3).
    TimestampDelta {
        /// The domain.
        domain: u64,
        /// Its offset, as the table holds it (§5.2.24.7 does not say whether it is signed).
        delta: u64,
    },
    /// Another type, walked by its length (types 4 and above are reserved or vendors').
    Other(Raw<'a>),
}

/// The checked FPDT.
#[derive(Clone, Copy, Debug)]
pub struct Fpdt<'a> {
    table: Table<'a>,
}

impl<'a> Fpdt<'a> {
    /// Checks the table. Refused: a record shorter than its type's fields; a second pointer of one
    /// kind (§5.2.24.4 to §5.2.24.6 allow one each); and no pointer to the host firmware's table,
    /// which §5.2.24.4 requires in every FPDT.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = header::check_signature(bytes, b"FPDT")?;
        let fpdt = Self { table };
        let mut seen = [false; 3];
        for record in fpdt.walk() {
            let (at, record) = record?;
            let slot = match record {
                FpdtRecord::Fbpt(_) => 0,
                FpdtRecord::S3pt(_) => 1,
                FpdtRecord::Mbpt(_) => 2,
                _ => continue,
            };
            if let Some(seen) = seen.get_mut(slot) {
                if *seen {
                    return Err(Error::new(
                        table.signature(),
                        at,
                        ErrorKind::Inconsistent {
                            field: "a second pointer record of one type",
                        },
                    ));
                }
                *seen = true;
            }
        }
        if !seen[0] {
            return Err(Error::new(
                table.signature(),
                bytes.len(),
                ErrorKind::Value {
                    field: "no FBPT pointer record",
                },
            ));
        }
        Ok(fpdt)
    }

    /// The records, each with its offset in the table.
    pub fn walk(&self) -> impl Iterator<Item = Result<(usize, FpdtRecord<'a>), Error>> + 'a {
        let signature = self.table.signature();
        self.table
            .reader()
            .rest(header::HEADER_LEN)
            .ok()
            .into_iter()
            .flat_map(|run| Walk::new(run, RECORD))
            .map(move |raw| {
                let raw = raw?;
                let pointer = |raw: &Raw<'a>| body(raw, signature, 16).and_then(|b| b.u64(8));
                let record = match raw.kind {
                    0 => FpdtRecord::Fbpt(pointer(&raw)?),
                    1 => FpdtRecord::S3pt(pointer(&raw)?),
                    2 => FpdtRecord::Mbpt(pointer(&raw)?),
                    3 => {
                        let b = body(&raw, signature, 24)?;
                        FpdtRecord::TimestampDelta {
                            domain: b.u64(8)?,
                            delta: b.u64(16)?,
                        }
                    }
                    _ => FpdtRecord::Other(raw),
                };
                Ok((raw.at, record))
            })
    }

    pub(crate) fn census(&self, tally: &mut Tally<'_, 'a>) -> Result<(), Error> {
        for raw in Walk::new(self.table.reader().rest(header::HEADER_LEN)?, RECORD) {
            let raw = raw?;
            tally.structure(raw.kind, raw.at, raw.bytes);
        }
        Ok(())
    }
}

/// Which of the FPDT's pointed-to tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerformanceKind {
    /// The host firmware's boot performance table, `FBPT` (§5.2.24.8).
    Fbpt,
    /// The S3 performance table, `S3PT` (§5.2.24.10).
    S3pt,
    /// A microcontroller's boot performance table, `MBPT` (§5.2.24.11).
    Mbpt,
}

impl PerformanceKind {
    /// Its signature.
    #[must_use]
    pub const fn signature(self) -> &'static [u8; 4] {
        match self {
            Self::Fbpt => b"FBPT",
            Self::S3pt => b"S3PT",
            Self::Mbpt => b"MBPT",
        }
    }

    /// Its header's length.
    #[must_use]
    pub const fn header_len(self) -> usize {
        match self {
            Self::Fbpt | Self::S3pt => 8,
            Self::Mbpt => 24,
        }
    }
}

/// A performance event record (§5.2.24.9 to §5.2.24.12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventRecord<'a> {
    /// The last S3 resume (type 0).
    S3Resume {
        /// Resumes since the last full boot.
        count: u32,
        /// The last resume's time, in nanoseconds.
        full: u64,
        /// The mean over every resume.
        average: u64,
    },
    /// The last S3 suspend (type 1).
    S3Suspend {
        /// When the OS wrote `SLP_TYP`.
        start: u64,
        /// When the firmware entered S3.
        end: u64,
    },
    /// The host firmware's boot (type 2), each time in nanoseconds since reset.
    Boot {
        /// Code began running on a processor.
        reset_end: u64,
        /// Just before the OS loader was loaded.
        load_image_start: u64,
        /// Just before it was started.
        start_image_start: u64,
        /// The loader called `ExitBootServices`.
        exit_boot_services_entry: u64,
        /// `ExitBootServices` returned.
        exit_boot_services_exit: u64,
    },
    /// A string event (type 4).
    String {
        /// The controller's ID.
        controller: u64,
        /// Its timestamp domain.
        domain: u64,
        /// When.
        timestamp: u64,
        /// The module that logged it.
        guid: Guid,
        /// The text, up to its first NUL.
        name: &'a [u8],
    },
    /// Another type, walked by its length (type 3, whose layout §5.2.24 does not give, among them).
    Other(Raw<'a>),
}

/// A checked FBPT, S3PT or MBPT.
#[derive(Clone, Copy, Debug)]
pub struct PerformanceTable<'a> {
    bytes: Bytes<'a>,
    /// Which table.
    pub kind: PerformanceKind,
}

impl<'a> PerformanceTable<'a> {
    /// Checks a pointed-to table: its signature, a length that is exactly the bytes given, and its
    /// records. The length is the space the firmware set aside, so records end at the first byte
    /// from which the rest is zero. Refused besides: an FBPT without exactly one boot record
    /// (§5.2.24.8); an S3PT without exactly one resume record, or with more than one suspend record
    /// (§5.2.24.10).
    pub fn parse(bytes: &'a [u8], kind: PerformanceKind) -> Result<Self, Error> {
        let signature = Signature::new(kind.signature());
        let b = Bytes::new(bytes, signature);
        let found = Signature(b.array(0)?);
        if found != signature {
            return Err(b.error(0, ErrorKind::Signature { found }));
        }
        let length = b.u32(4)?;
        if usize::try_from(length).ok() != Some(bytes.len()) || bytes.len() < kind.header_len() {
            return Err(b.error(
                4,
                ErrorKind::Length {
                    declared: u64::from(length),
                    available: bytes.len(),
                },
            ));
        }
        let table = Self { bytes: b, kind };
        let mut counts = [0u32; 3];
        for record in table.records() {
            if let Some(slot) = match record? {
                EventRecord::S3Resume { .. } => counts.get_mut(0),
                EventRecord::S3Suspend { .. } => counts.get_mut(1),
                EventRecord::Boot { .. } => counts.get_mut(2),
                _ => None,
            } {
                *slot = slot.saturating_add(1);
            }
        }
        let [resume, suspend, boot] = counts;
        let refuse = |field| Err(b.error(kind.header_len(), ErrorKind::Value { field }));
        match kind {
            PerformanceKind::Fbpt if boot != 1 => refuse("the boot performance record"),
            PerformanceKind::S3pt if resume != 1 => refuse("the S3 resume record"),
            PerformanceKind::S3pt if suspend > 1 => refuse("the S3 suspend record"),
            _ => Ok(table),
        }
    }

    /// The records, in order. The length is the space the firmware set aside, so they end at the
    /// first record boundary from which every remaining byte is zero.
    pub fn records(&self) -> impl Iterator<Item = Result<EventRecord<'a>, Error>> + 'a {
        let signature = self.bytes.table();
        let run = self.bytes.rest(self.kind.header_len()).ok();
        let mut at = 0usize;
        let mut done = false;
        core::iter::from_fn(move || {
            let run = run?;
            let rest = run.bytes().get(at..)?;
            if done || rest.iter().all(|&b| b == 0) {
                return None;
            }
            let item = run.rest(at).and_then(|r| {
                Walk::new(r, RECORD)
                    .next()
                    .unwrap_or(Err(r.error(0, ErrorKind::Truncated { need: r.offset(4) })))
            });
            match &item {
                Ok(raw) => at = at.saturating_add(raw.bytes.len()),
                Err(_) => done = true,
            }
            Some(item)
        })
        .map(move |raw| {
            let raw = raw?;
            Ok(match raw.kind {
                0 => {
                    let b = body(&raw, signature, 24)?;
                    EventRecord::S3Resume {
                        count: b.u32(4)?,
                        full: b.u64(8)?,
                        average: b.u64(16)?,
                    }
                }
                1 => {
                    let b = body(&raw, signature, 20)?;
                    EventRecord::S3Suspend {
                        start: b.u64(4)?,
                        end: b.u64(12)?,
                    }
                }
                2 => {
                    let b = body(&raw, signature, 48)?;
                    EventRecord::Boot {
                        reset_end: b.u64(8)?,
                        load_image_start: b.u64(16)?,
                        start_image_start: b.u64(24)?,
                        exit_boot_services_entry: b.u64(32)?,
                        exit_boot_services_exit: b.u64(40)?,
                    }
                }
                4 => {
                    // §5.2.24.12 gives the record 60 bytes and its name 24 from offset 44: the
                    // name is read as far as the record goes.
                    let b = body(&raw, signature, 44)?;
                    let name = b.rest(44)?.bytes();
                    let name = name
                        .get(..name.iter().position(|&c| c == 0).unwrap_or(name.len()))
                        .unwrap_or(&[]);
                    EventRecord::String {
                        controller: b.u64(4)?,
                        domain: b.u64(12)?,
                        timestamp: b.u64(20)?,
                        guid: b.guid(28)?,
                        name,
                    }
                }
                _ => EventRecord::Other(raw),
            })
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testing::table;

    /// Valid tables, for the hostile-table tests.
    pub(crate) fn samples() -> Vec<Vec<u8>> {
        let mut bgrt = vec![1, 0, 0b101, 0];
        bgrt.extend_from_slice(&0x7E00_0000u64.to_le_bytes());
        bgrt.extend_from_slice(&[0; 8]);
        let mut delta = 3u16.to_le_bytes().to_vec();
        delta.extend_from_slice(&[24, 1, 0, 0, 0, 0]);
        delta.extend_from_slice(&[1; 16]);
        vec![
            table(b"BGRT", 1, &bgrt),
            table(
                b"FPDT",
                1,
                &[pointer(0, 0x7F00_0000), pointer(1, 0x7F01_0000), delta].concat(),
            ),
        ]
    }

    #[test]
    fn bgrt_decodes() {
        let mut body = vec![1, 0, 0b101, 0];
        body.extend_from_slice(&0x7E00_0000u64.to_le_bytes());
        body.extend_from_slice(&100u32.to_le_bytes());
        body.extend_from_slice(&200u32.to_le_bytes());
        let g = Bgrt::parse(&table(b"BGRT", 1, &body)).unwrap();
        assert_eq!(
            g,
            Bgrt {
                version: 1,
                displayed: true,
                orientation: 180,
                image_type: ImageType::Bitmap,
                image_address: 0x7E00_0000,
                x: 100,
                y: 200
            }
        );
        assert!(Bgrt::parse(&table(b"BGRT", 1, &body[..19])).is_err());
    }

    fn pointer(kind: u16, address: u64) -> Vec<u8> {
        let mut r = kind.to_le_bytes().to_vec();
        r.extend_from_slice(&[16, 1, 0, 0, 0, 0]);
        r.extend_from_slice(&address.to_le_bytes());
        r
    }

    #[test]
    fn fpdt_lists_its_pointers() {
        let mut delta = 3u16.to_le_bytes().to_vec();
        delta.extend_from_slice(&[24, 1, 0, 0, 0, 0]);
        delta.extend_from_slice(&[1; 16]);
        let body = [
            pointer(0, 0x7F00_0000),
            pointer(1, 0x7F01_0000),
            delta,
            vec![9, 0, 4, 1],
        ]
        .concat();
        let t = table(b"FPDT", 1, &body);
        let f = Fpdt::parse(&t).unwrap();
        let all: Vec<_> = f.walk().map(|r| r.unwrap().1).collect();
        assert_eq!(all[0], FpdtRecord::Fbpt(0x7F00_0000));
        assert_eq!(all[1], FpdtRecord::S3pt(0x7F01_0000));
        assert!(matches!(all[2], FpdtRecord::TimestampDelta { .. }));
        assert!(matches!(all[3], FpdtRecord::Other(Raw { kind: 9, .. })));
        let kind = |b: Vec<u8>| Fpdt::parse(&table(b"FPDT", 1, &b)).unwrap_err().kind;
        assert!(matches!(kind(pointer(1, 0)), ErrorKind::Value { .. }));
        assert!(matches!(
            kind([pointer(0, 1), pointer(0, 2)].concat()),
            ErrorKind::Inconsistent { .. }
        ));
        let mut short = pointer(0, 1);
        short[2] = 15;
        short.truncate(15);
        assert!(matches!(kind(short), ErrorKind::Structure { .. }));
        assert!(matches!(
            kind(vec![0, 0, 3, 1]),
            ErrorKind::Structure { .. }
        ));
    }

    fn fbpt(records: &[Vec<u8>], spare: usize) -> Vec<u8> {
        let mut t = b"FBPT".to_vec();
        let len = 8 + records.iter().map(Vec::len).sum::<usize>() + spare;
        t.extend_from_slice(&u32::try_from(len).unwrap().to_le_bytes());
        for r in records {
            t.extend_from_slice(r);
        }
        t.resize(len, 0);
        t
    }

    fn boot_record() -> Vec<u8> {
        let mut r = vec![2, 0, 48, 2, 0, 0, 0, 0];
        for v in 1..=5u64 {
            r.extend_from_slice(&(v * 1000).to_le_bytes());
        }
        r
    }

    #[test]
    fn performance_tables_hold_their_records() {
        let t = fbpt(&[boot_record()], 64);
        let p = PerformanceTable::parse(&t, PerformanceKind::Fbpt).unwrap();
        assert!(matches!(
            p.records().next(),
            Some(Ok(EventRecord::Boot {
                reset_end: 1000,
                ..
            }))
        ));
        let mut string = vec![4, 0, 60, 1];
        string.extend_from_slice(&[0; 40]);
        string.extend_from_slice(b"ready\0\0\0\0\0\0\0\0\0\0\0");
        let t = fbpt(&[boot_record(), string], 0);
        let names: Vec<_> = PerformanceTable::parse(&t, PerformanceKind::Fbpt)
            .unwrap()
            .records()
            .filter_map(|r| match r.unwrap() {
                EventRecord::String { name, .. } => Some(name),
                _ => None,
            })
            .collect();
        assert_eq!(names, [b"ready"]);
        let kind = |t: Vec<u8>, k| PerformanceTable::parse(&t, k).unwrap_err().kind;
        assert!(matches!(
            kind(fbpt(&[], 8), PerformanceKind::Fbpt),
            ErrorKind::Value { .. }
        ));
        assert!(matches!(
            kind(
                fbpt(&[boot_record(), boot_record()], 0),
                PerformanceKind::Fbpt
            ),
            ErrorKind::Value { .. }
        ));
        assert!(matches!(
            kind(fbpt(&[boot_record()], 0), PerformanceKind::S3pt),
            ErrorKind::Signature { .. }
        ));
        let mut long = fbpt(&[boot_record()], 0);
        long[4] = 99;
        assert!(matches!(
            kind(long, PerformanceKind::Fbpt),
            ErrorKind::Length { .. }
        ));
        let mut s3 = b"S3PT".to_vec();
        s3.extend_from_slice(&52u32.to_le_bytes());
        s3.extend_from_slice(&[0, 0, 24, 1]);
        s3.extend_from_slice(&[0; 20]);
        s3.extend_from_slice(&[1, 0, 20, 1]);
        s3.extend_from_slice(&[0; 16]);
        assert!(PerformanceTable::parse(&s3, PerformanceKind::S3pt).is_ok());
        let mut garbage = fbpt(&[boot_record()], 8);
        let end = garbage.len() - 1;
        garbage[end] = 1;
        assert!(PerformanceTable::parse(&garbage, PerformanceKind::Fbpt).is_err());
    }
}
