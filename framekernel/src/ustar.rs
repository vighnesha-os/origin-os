//! A read-only ustar (POSIX tar) archive reader (CLAUDE.md §21 initial system deployment,
//! §46 package parsing). ustar is the natural format for an initial RAM filesystem and for
//! the file payload of a package: a sequence of 512-byte header+data records, ending in two
//! zero blocks. This reader validates the header checksum and the `ustar` magic, parses the
//! octal fields safely, and yields each entry's name, size, type and a borrowed data slice.
//! Every field is bounds-checked and the octal parser rejects non-octal, so a malformed or
//! hostile archive is a typed error, never a panic (untrusted input, §34). Pure, no
//! `unsafe`; `alloc` for the owned names.

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

/// The archive block size.
pub const BLOCK: usize = 512;

// Header field offsets (POSIX ustar).
const OFF_NAME: usize = 0;
const OFF_SIZE: usize = 124;
const OFF_CHKSUM: usize = 148;
const OFF_TYPEFLAG: usize = 156;
const OFF_MAGIC: usize = 257;
const OFF_PREFIX: usize = 345;

/// A tar type flag (the common ones; others are exposed as their raw byte).
pub mod typeflag {
    pub const REGULAR: u8 = b'0';
    pub const REGULAR_ALT: u8 = 0; // historically a regular file too
    pub const DIRECTORY: u8 = b'5';
    pub const SYMLINK: u8 = b'2';
    pub const HARDLINK: u8 = b'1';
}

/// Why an archive was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UstarError {
    /// A header was truncated (fewer than 512 bytes remain).
    Truncated,
    /// The `ustar` magic is missing.
    BadMagic,
    /// The stored header checksum does not match the computed one.
    BadChecksum,
    /// An octal field held a non-octal byte, or a size/offset overflowed.
    BadField,
    /// A file's data runs past the end of the archive.
    BadSize,
    /// A name was not valid UTF-8.
    BadName,
}

/// One archive entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    /// The full path (`prefix/name` when the prefix field is used).
    pub name: String,
    pub size: usize,
    pub typeflag: u8,
    /// The file's bytes (empty for a directory).
    pub data: &'a [u8],
}

impl Entry<'_> {
    pub fn is_file(&self) -> bool {
        self.typeflag == typeflag::REGULAR || self.typeflag == typeflag::REGULAR_ALT
    }
    pub fn is_dir(&self) -> bool {
        self.typeflag == typeflag::DIRECTORY
    }
}

/// Parses a ustar octal field: optional leading spaces, then octal digits, terminated by a
/// space or NUL. An empty field is 0. A non-octal byte is rejected.
fn parse_octal(field: &[u8]) -> Option<u64> {
    let mut v = 0u64;
    let mut started = false;
    for &b in field {
        match b {
            b'0'..=b'7' => {
                v = v.checked_mul(8)?.checked_add(u64::from(b - b'0'))?;
                started = true;
            }
            b' ' if !started => {} // leading padding
            b' ' | 0 => break,     // terminator
            _ => return None,
        }
    }
    Some(v)
}

/// The standard tar header checksum: the sum of every header byte with the 8-byte checksum
/// field counted as spaces.
fn header_checksum(header: &[u8]) -> u32 {
    header
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (OFF_CHKSUM..OFF_CHKSUM + 8).contains(&i) {
                u32::from(b' ')
            } else {
                u32::from(b)
            }
        })
        .sum()
}

/// Reads a NUL-terminated (or field-length) string from a header field.
fn field_str(field: &[u8]) -> Option<&str> {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    core::str::from_utf8(&field[..end]).ok()
}

/// A streaming reader over a ustar archive's bytes.
#[derive(Clone, Debug)]
pub struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    /// Reads the next entry, `Ok(None)` at the end-of-archive marker (a zero block) or when
    /// the bytes run out cleanly, or an error on a malformed header.
    pub fn next_entry(&mut self) -> Result<Option<Entry<'a>>, UstarError> {
        let header = match self.bytes.get(self.offset..self.offset + BLOCK) {
            Some(h) => h,
            None => {
                // No full block left: clean end only if nothing remains.
                return if self.offset >= self.bytes.len() {
                    Ok(None)
                } else {
                    Err(UstarError::Truncated)
                };
            }
        };
        // Two zero blocks mark the end; one all-zero block is enough to stop.
        if header.iter().all(|&b| b == 0) {
            return Ok(None);
        }
        if header.get(OFF_MAGIC..OFF_MAGIC + 5) != Some(b"ustar") {
            return Err(UstarError::BadMagic);
        }
        let stored =
            parse_octal(&header[OFF_CHKSUM..OFF_CHKSUM + 8]).ok_or(UstarError::BadField)?;
        if u64::from(header_checksum(header)) != stored {
            return Err(UstarError::BadChecksum);
        }

        let size = usize::try_from(
            parse_octal(&header[OFF_SIZE..OFF_SIZE + 12]).ok_or(UstarError::BadField)?,
        )
        .map_err(|_| UstarError::BadField)?;
        let typeflag = header[OFF_TYPEFLAG];

        let name = field_str(&header[OFF_NAME..OFF_NAME + 100]).ok_or(UstarError::BadName)?;
        let prefix = field_str(&header[OFF_PREFIX..OFF_PREFIX + 155]).ok_or(UstarError::BadName)?;
        let full = if prefix.is_empty() {
            String::from(name)
        } else {
            let mut s = String::from(prefix);
            s.push('/');
            s.push_str(name);
            s
        };

        let data_start = self.offset + BLOCK;
        let data_end = data_start.checked_add(size).ok_or(UstarError::BadField)?;
        let data = self
            .bytes
            .get(data_start..data_end)
            .ok_or(UstarError::BadSize)?;

        // Advance past the data, rounded up to the next block boundary.
        let padded = size
            .checked_next_multiple_of(BLOCK)
            .ok_or(UstarError::BadField)?;
        self.offset = data_start.checked_add(padded).ok_or(UstarError::BadField)?;

        Ok(Some(Entry {
            name: full,
            size,
            typeflag,
            data,
        }))
    }
}

/// Parses the whole archive into a vector of entries, erroring on the first malformed
/// header.
pub fn parse(bytes: &[u8]) -> Result<Vec<Entry<'_>>, UstarError> {
    let mut reader = Reader::new(bytes);
    let mut out = Vec::new();
    while let Some(entry) = reader.next_entry()? {
        out.push(entry);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a ustar archive for the tests.
    struct Builder {
        buf: Vec<u8>,
    }
    impl Builder {
        fn new() -> Self {
            Self { buf: Vec::new() }
        }

        fn entry(&mut self, prefix: &str, name: &str, typeflag: u8, data: &[u8]) {
            let mut h = [0u8; BLOCK];
            h[OFF_NAME..OFF_NAME + name.len()].copy_from_slice(name.as_bytes());
            h[OFF_PREFIX..OFF_PREFIX + prefix.len()].copy_from_slice(prefix.as_bytes());
            h[OFF_TYPEFLAG] = typeflag;
            h[OFF_MAGIC..OFF_MAGIC + 6].copy_from_slice(b"ustar\0");
            h[OFF_MAGIC + 6..OFF_MAGIC + 8].copy_from_slice(b"00");
            // Octal size, NUL-terminated in a 12-byte field.
            let size = alloc::format!("{:011o}", data.len());
            h[OFF_SIZE..OFF_SIZE + 11].copy_from_slice(size.as_bytes());
            // Some writers use mode/uid/gid/mtime; zeros are fine. Compute the checksum last.
            let sum = header_checksum(&h);
            let chk = alloc::format!("{sum:06o}\0 ");
            h[OFF_CHKSUM..OFF_CHKSUM + 8].copy_from_slice(chk.as_bytes());

            self.buf.extend_from_slice(&h);
            self.buf.extend_from_slice(data);
            let pad = (BLOCK - data.len() % BLOCK) % BLOCK;
            self.buf.extend_from_slice(&alloc::vec![0u8; pad]);
        }

        fn finish(mut self) -> Vec<u8> {
            self.buf.extend_from_slice(&[0u8; BLOCK * 2]); // end-of-archive
            self.buf
        }
    }

    #[test]
    fn round_trips_files_and_directories() {
        let mut b = Builder::new();
        b.entry("", "hello.txt", typeflag::REGULAR, b"Hello, tar!\n");
        b.entry("", "etc", typeflag::DIRECTORY, b"");
        // A file larger than one block, to exercise multi-block data and padding.
        let big: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();
        b.entry("etc", "origin.conf", typeflag::REGULAR, &big);
        let archive = b.finish();

        let entries = parse(&archive).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "hello.txt");
        assert!(entries[0].is_file());
        assert_eq!(entries[0].data, b"Hello, tar!\n");
        assert_eq!(entries[0].size, 12);
        assert!(entries[1].is_dir() && entries[1].name == "etc");
        assert_eq!(
            entries[2].name, "etc/origin.conf",
            "prefix joined with name"
        );
        assert_eq!(entries[2].size, 1000);
        assert_eq!(entries[2].data, &big[..]);
    }

    #[test]
    fn rejects_bad_checksum_magic_and_truncation() {
        let mut b = Builder::new();
        b.entry("", "f", typeflag::REGULAR, b"x");
        let good = b.finish();
        assert!(parse(&good).is_ok());

        // A flipped checksum byte.
        let mut bad = good.clone();
        bad[OFF_CHKSUM] ^= 0x01;
        assert_eq!(parse(&bad), Err(UstarError::BadChecksum));

        // Corrupt the magic (and fix nothing): BadMagic is reported before the checksum.
        let mut bad = good.clone();
        bad[OFF_MAGIC] = b'x';
        assert_eq!(parse(&bad), Err(UstarError::BadMagic));

        // A header cut short (fewer than 512 bytes of header present).
        assert_eq!(parse(&good[..BLOCK - 1]), Err(UstarError::Truncated));

        // A size that runs past the archive.
        let mut bad = good.clone();
        bad[OFF_SIZE..OFF_SIZE + 11].copy_from_slice(b"77777777777"); // a huge octal size
        let sum = header_checksum(&bad[..BLOCK]);
        let chk = alloc::format!("{sum:06o}\0 ");
        bad[OFF_CHKSUM..OFF_CHKSUM + 8].copy_from_slice(chk.as_bytes());
        assert_eq!(parse(&bad), Err(UstarError::BadSize));
    }

    #[test]
    fn stops_at_the_zero_block() {
        // One entry then the end marker, with trailing garbage the reader must not reach.
        let mut b = Builder::new();
        b.entry("", "a", typeflag::REGULAR, b"1");
        let mut archive = b.finish();
        archive.extend_from_slice(b"garbage that is past the end-of-archive marker and ignored");
        let entries = parse(&archive).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "a");
    }

    #[test]
    fn mutations_never_panic() {
        let mut b = Builder::new();
        b.entry("", "hello.txt", typeflag::REGULAR, b"Hello, tar!\n");
        b.entry(
            "usr/local",
            "bin.dat",
            typeflag::REGULAR,
            &alloc::vec![7u8; 700],
        );
        b.entry("", "d", typeflag::DIRECTORY, b"");
        let base = b.finish();

        let mut rng = crate::testing::Rng::new(0x5553_5441);
        for _ in 0..crate::testing::cases(20_000) {
            let mut data = base.clone();
            crate::testing::mutate(&mut data, &mut rng);
            if let Ok(entries) = parse(&data) {
                for e in entries {
                    let _ = (e.is_file(), e.is_dir(), e.data.len(), e.size);
                }
            }
        }
    }
}
