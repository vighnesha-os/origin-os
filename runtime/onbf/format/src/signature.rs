//! The ONBF signature block (ONBF §7.1–§7.2) with the owner's encodings S1–S4
//! (`docs/spec/ONBF-signature-encoding.md`, decided 2026-11-07).
//!
//! Parsing only: the block structure, every field's type and length, Appendix A's limits on
//! the block and the chain, and the cross-field rules (the statement's role equals the
//! entry's; the suite is fixed by the role; the ML-DSA signature length is fixed by the
//! suite; no entry twice). A statement's version must be SemVer 2.0 (S3); its `id` must be
//! non-empty printable ASCII, which is trivially NFC (errata O1). The
//! cryptographic checks
//! (the content digest, both signatures over [`Statement::bytes`] with the role's context
//! string, the certificate chain) belong to the verifier, which uses `origin-crypto`.
//!
//! ```text
//! SignatureBlock = "ONBFSIG1" ‖ le32 version = 1 ‖ le32 entry_count (1–8) ‖ Entry*
//! Entry          = le32 entry_size ‖ OCBOR-1 map with exactly keys 1–7
//! ```

use crate::header::MAX_SIGNATURE_BLOCK;
use crate::ocbor::{self, Cursor, Item, OcborError};

/// The block's first eight bytes.
pub const MAGIC: [u8; 8] = *b"ONBFSIG1";
/// The only block version.
pub const VERSION: u32 = 1;
/// Most entries a block may hold.
pub const MAX_ENTRIES: usize = 8;
/// Most certificates a chain may hold (Appendix A).
pub const MAX_CHAIN: u64 = 4;

/// Who signed (S1): integer roles, 0 and unknown values rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The software's publisher.
    Publisher = 1,
    /// A notary.
    Notary = 2,
    /// Origin's attestation of a third-party driver.
    DriverAttest = 3,
    /// Origin's kernel signing key.
    Kernel = 4,
    /// Origin's release signing key.
    Release = 5,
}

impl Role {
    /// The role for a raw value.
    pub const fn from_raw(raw: u64) -> Option<Self> {
        Some(match raw {
            1 => Self::Publisher,
            2 => Self::Notary,
            3 => Self::DriverAttest,
            4 => Self::Kernel,
            5 => Self::Release,
            _ => return None,
        })
    }

    /// The FIPS 204 context string ML-DSA signs with (§7.1). Built from this table, never
    /// from the file.
    pub const fn context(self) -> &'static [u8] {
        match self {
            Self::Publisher => b"origin/onbf/1/publisher",
            Self::Notary => b"origin/onbf/1/notary",
            Self::DriverAttest => b"origin/onbf/1/origin-driver-attest",
            Self::Kernel => b"origin/onbf/1/origin-kernel",
            Self::Release => b"origin/onbf/1/origin-release",
        }
    }

    /// The suite this role must use (§7.2): Origin's kernel and release keys use OSS-1R,
    /// everyone else OSS-1.
    pub const fn suite(self) -> Suite {
        match self {
            Self::Kernel | Self::Release => Suite::Oss1R,
            Self::Publisher | Self::Notary | Self::DriverAttest => Suite::Oss1,
        }
    }
}

/// The signature suite (S2): both signatures must verify.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Suite {
    /// Ed25519 + ML-DSA-65.
    Oss1 = 1,
    /// Ed25519 + ML-DSA-87.
    Oss1R = 2,
}

impl Suite {
    /// The suite for a raw value.
    pub const fn from_raw(raw: u64) -> Option<Self> {
        match raw {
            1 => Some(Self::Oss1),
            2 => Some(Self::Oss1R),
            _ => None,
        }
    }

    /// Length of the ML-DSA signature.
    pub const fn mldsa_signature_len(self) -> usize {
        match self {
            Self::Oss1 => 3309,
            Self::Oss1R => 4627,
        }
    }

    /// Length of the ML-DSA public key.
    pub const fn mldsa_public_key_len(self) -> usize {
        match self {
            Self::Oss1 => 1952,
            Self::Oss1R => 2592,
        }
    }
}

/// The signed statement (S3), entry key 5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Statement<'a> {
    /// The exact encoded statement: what both signatures cover.
    pub bytes: &'a [u8],
    /// Key 1: the profile; the verifier checks it against the header.
    pub profile: u64,
    /// Key 2: the 64-byte content digest; the verifier checks it against the image.
    pub content_digest: &'a [u8],
    /// Key 3: the artifact's ID, non-empty printable ASCII (errata O1).
    pub id: &'a str,
    /// Key 4: the artifact's version, SemVer 2.0.
    pub version: &'a str,
    /// Key 5: the role, equal to the entry's.
    pub role: Role,
    /// Key 6: informational only (§7.6).
    pub claimed_signing_time: u64,
}

/// One signature entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    /// Key 1.
    pub role: Role,
    /// Key 2.
    pub suite: Suite,
    /// Key 3: SHA-256 of the signer's canonical public-key encoding, 32 bytes.
    pub key_id: &'a [u8],
    /// Key 4: the encoded certificate array, leaf first, at most [`MAX_CHAIN`] long (parsed by
    /// the chain verifier).
    pub chain: &'a [u8],
    /// Number of certificates in [`Self::chain`].
    pub chain_len: u64,
    /// Key 5.
    pub statement: Statement<'a>,
    /// Key 6: 64 bytes.
    pub sig_ed25519: &'a [u8],
    /// Key 7: [`Suite::mldsa_signature_len`] bytes.
    pub sig_mldsa: &'a [u8],
}

/// Why a signature block was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignatureError {
    /// The block is larger than 1 MiB (Appendix A).
    TooLarge,
    /// The block does not start with [`MAGIC`].
    Magic,
    /// A version other than [`VERSION`].
    Version(u32),
    /// An entry count outside 1–8.
    EntryCount(u32),
    /// The block ends inside a header or an entry.
    Truncated,
    /// Bytes follow the last entry.
    TrailingData,
    /// Entry `entry` is not valid OCBOR-1.
    Ocbor {
        /// The entry's index.
        entry: u8,
        /// What the OCBOR-1 validator found.
        error: OcborError,
    },
    /// Entry `entry` is not a map with exactly keys 1–7 (key 8 is reserved in v1), or its
    /// statement is not a map with exactly keys 0–6.
    Shape {
        /// The entry's index.
        entry: u8,
    },
    /// The field at `key` of entry `entry` has the wrong type, length or value (statement
    /// fields are reported with key 50 + their statement key).
    Field {
        /// The entry's index.
        entry: u8,
        /// The field.
        key: u64,
    },
    /// The suite is not the one the role requires.
    RoleSuite {
        /// The entry's index.
        entry: u8,
    },
    /// The statement's role differs from the entry's.
    RoleMismatch {
        /// The entry's index.
        entry: u8,
    },
    /// The same role and key appear twice.
    Duplicate {
        /// The entry's index.
        entry: u8,
    },
}

/// A parsed signature block.
#[derive(Clone, Copy, Debug)]
pub struct SignatureBlock<'a> {
    entries: [Option<Entry<'a>>; MAX_ENTRIES],
    count: usize,
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    let mut v = [0u8; 4];
    v.copy_from_slice(s);
    Some(u32::from_le_bytes(v))
}

impl<'a> SignatureBlock<'a> {
    /// Parses a whole signature block.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, SignatureError> {
        if u64::try_from(bytes.len()).map_or(true, |n| n > MAX_SIGNATURE_BLOCK) {
            return Err(SignatureError::TooLarge);
        }
        if bytes.get(..8) != Some(&MAGIC[..]) {
            return Err(SignatureError::Magic);
        }
        let version = le32(bytes, 8).ok_or(SignatureError::Truncated)?;
        if version != VERSION {
            return Err(SignatureError::Version(version));
        }
        let count = le32(bytes, 12).ok_or(SignatureError::Truncated)?;
        let n = usize::try_from(count).unwrap_or(usize::MAX);
        if n == 0 || n > MAX_ENTRIES {
            return Err(SignatureError::EntryCount(count));
        }
        let mut block = Self {
            entries: [None; MAX_ENTRIES],
            count: 0,
        };
        let mut pos = 16usize;
        for index in 0..n {
            let entry = u8::try_from(index).unwrap_or(u8::MAX);
            let size = le32(bytes, pos).ok_or(SignatureError::Truncated)?;
            let start = pos.checked_add(4).ok_or(SignatureError::Truncated)?;
            let end = start
                .checked_add(usize::try_from(size).unwrap_or(usize::MAX))
                .ok_or(SignatureError::Truncated)?;
            let body = bytes.get(start..end).ok_or(SignatureError::Truncated)?;
            let parsed = parse_entry(body, entry)?;
            let duplicate = block
                .entries
                .iter()
                .flatten()
                .any(|e| e.role == parsed.role && e.key_id == parsed.key_id);
            if duplicate {
                return Err(SignatureError::Duplicate { entry });
            }
            if let Some(slot) = block.entries.get_mut(index) {
                *slot = Some(parsed);
            }
            block.count = index.saturating_add(1);
            pos = end;
        }
        if pos != bytes.len() {
            return Err(SignatureError::TrailingData);
        }
        Ok(block)
    }

    /// The entries, in block order.
    pub fn entries(&self) -> impl Iterator<Item = &Entry<'a>> {
        self.entries.iter().take(self.count).flatten()
    }
}

/// True if `text` is printable ASCII (0x20–0x7E). That is trivially NFC, so this parser
/// needs no Unicode normalization tables (errata O1, the same rule as R4).
fn is_printable_ascii(text: &str) -> bool {
    text.bytes().all(|b| (0x20..=0x7E).contains(&b))
}

/// A SemVer numeric identifier: `0`, or digits with no leading zero.
fn is_number(field: &[u8]) -> bool {
    match field.split_first() {
        None => false,
        Some((&b'0', rest)) => rest.is_empty(),
        Some(_) => field.iter().all(u8::is_ascii_digit),
    }
}

/// A SemVer identifier: one or more of `[0-9A-Za-z-]`.
fn is_identifier(field: &[u8]) -> bool {
    !field.is_empty()
        && field
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b == b'-')
}

/// True if `version` follows SemVer 2.0.0 (the grammar at semver.org), which S3 requires of
/// the statement's version. Syntax only: numbers are not converted, so their size is not
/// limited.
fn is_semver(version: &str) -> bool {
    let is_dot = |b: &u8| *b == b'.';
    let mut parts = version.as_bytes().splitn(2, |&b| b == b'+');
    let (main, build) = (parts.next().unwrap_or(&[]), parts.next());
    let mut parts = main.splitn(2, |&b| b == b'-');
    let (core, pre) = (parts.next().unwrap_or(&[]), parts.next());
    let mut numbers = core.split(is_dot);
    let core_ok = (numbers.next().is_some_and(is_number))
        && (numbers.next().is_some_and(is_number))
        && (numbers.next().is_some_and(is_number))
        && numbers.next().is_none();
    core_ok
        && pre.is_none_or(|pre| {
            pre.split(is_dot).all(|id| {
                is_identifier(id) && (is_number(id) || !id.iter().all(u8::is_ascii_digit))
            })
        })
        && build.is_none_or(|build| build.split(is_dot).all(is_identifier))
}

/// Reads the next item, which must be the map key `key`.
fn expect_key(c: &mut Cursor<'_>, key: u64, entry: u8) -> Result<(), SignatureError> {
    match c.next_item() {
        Ok(Item::Unsigned(k)) if k == key => Ok(()),
        _ => Err(SignatureError::Shape { entry }),
    }
}

fn parse_entry(body: &[u8], entry: u8) -> Result<Entry<'_>, SignatureError> {
    ocbor::validate(body).map_err(|error| SignatureError::Ocbor { entry, error })?;
    let field = |key| SignatureError::Field { entry, key };
    let mut c = Cursor::new(body);
    if c.next_item() != Ok(Item::Map(7)) {
        return Err(SignatureError::Shape { entry });
    }
    expect_key(&mut c, 1, entry)?;
    let role = match c.next_item() {
        Ok(Item::Unsigned(v)) => Role::from_raw(v).ok_or(field(1))?,
        _ => return Err(field(1)),
    };
    expect_key(&mut c, 2, entry)?;
    let suite = match c.next_item() {
        Ok(Item::Unsigned(v)) => Suite::from_raw(v).ok_or(field(2))?,
        _ => return Err(field(2)),
    };
    if suite != role.suite() {
        return Err(SignatureError::RoleSuite { entry });
    }
    expect_key(&mut c, 3, entry)?;
    let key_id = match c.next_item() {
        Ok(Item::Bytes(b)) if b.len() == 32 => b,
        _ => return Err(field(3)),
    };
    expect_key(&mut c, 4, entry)?;
    let chain_start = c.position();
    let chain_len = match c.next_item() {
        Ok(Item::Array(n)) if n <= MAX_CHAIN => n,
        _ => return Err(field(4)),
    };
    for _ in 0..chain_len {
        match c.next_item() {
            Ok(cert @ Item::Map(_)) => c.skip_contents(cert).map_err(|_| field(4))?,
            _ => return Err(field(4)),
        }
    }
    let chain = body.get(chain_start..c.position()).ok_or(field(4))?;
    expect_key(&mut c, 5, entry)?;
    let statement = parse_statement(body, &mut c, entry)?;
    if statement.role != role {
        return Err(SignatureError::RoleMismatch { entry });
    }
    expect_key(&mut c, 6, entry)?;
    let sig_ed25519 = match c.next_item() {
        Ok(Item::Bytes(b)) if b.len() == 64 => b,
        _ => return Err(field(6)),
    };
    expect_key(&mut c, 7, entry)?;
    let sig_mldsa = match c.next_item() {
        Ok(Item::Bytes(b)) if b.len() == suite.mldsa_signature_len() => b,
        _ => return Err(field(7)),
    };
    Ok(Entry {
        role,
        suite,
        key_id,
        chain,
        chain_len,
        statement,
        sig_ed25519,
        sig_mldsa,
    })
}

fn parse_statement<'a>(
    body: &'a [u8],
    c: &mut Cursor<'a>,
    entry: u8,
) -> Result<Statement<'a>, SignatureError> {
    let field = |key: u64| SignatureError::Field {
        entry,
        key: key.saturating_add(50),
    };
    let start = c.position();
    if c.next_item() != Ok(Item::Map(7)) {
        return Err(SignatureError::Shape { entry });
    }
    let uint = |c: &mut Cursor<'a>, key| match c.next_item() {
        Ok(Item::Unsigned(v)) => Ok(v),
        _ => Err(field(key)),
    };
    let text = |c: &mut Cursor<'a>, key| match c.next_item() {
        Ok(Item::Text(t)) if !t.is_empty() => Ok(t),
        _ => Err(field(key)),
    };
    expect_key(c, 0, entry)?;
    if uint(c, 0)? != 1 {
        return Err(field(0));
    }
    expect_key(c, 1, entry)?;
    let profile = uint(c, 1)?;
    expect_key(c, 2, entry)?;
    let content_digest = match c.next_item() {
        Ok(Item::Bytes(b)) if b.len() == 64 => b,
        _ => return Err(field(2)),
    };
    expect_key(c, 3, entry)?;
    let id = text(c, 3)?;
    if !is_printable_ascii(id) {
        return Err(field(3));
    }
    expect_key(c, 4, entry)?;
    let version = text(c, 4)?;
    if !is_semver(version) {
        return Err(field(4));
    }
    expect_key(c, 5, entry)?;
    let role = Role::from_raw(uint(c, 5)?).ok_or(field(5))?;
    expect_key(c, 6, entry)?;
    let claimed_signing_time = uint(c, 6)?;
    let bytes = body.get(start..c.position()).ok_or(field(0))?;
    Ok(Statement {
        bytes,
        profile,
        content_digest,
        id,
        version,
        role,
        claimed_signing_time,
    })
}
