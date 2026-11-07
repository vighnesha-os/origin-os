//! The KRN manifest (`docs/spec/ONBF-manifest-keys.md`, owner decisions K1–K4).
//!
//! Per ONBF-R1, decode a manifest only after its section's SHA-256 has been checked
//! ([`crate::Section::verify_sha256`]) and the image's content digest and signature have
//! verified.

use crate::header::Profile;
use crate::image::Image;
use crate::ocbor::{self, Cursor, Item, OcborError};
use crate::section::{Flags, Kind};

/// A manifest field: a top-level key, and a sub-key inside a nested map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldPath {
    /// Top-level key.
    pub key: u64,
    /// Key inside a nested map (`toolchain`, `cet`), if any.
    pub sub: Option<u64>,
}

const fn top(key: u64) -> FieldPath {
    FieldPath { key, sub: None }
}
const fn nested(key: u64, sub: u64) -> FieldPath {
    FieldPath {
        key,
        sub: Some(sub),
    }
}

/// Why a KRN manifest was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManifestError {
    /// The bytes are not valid OCBOR-1.
    Ocbor(OcborError),
    /// The top-level item is not a map.
    NotAMap,
    /// `schema` (key 0) is not 1.
    UnsupportedSchema(u64),
    /// A key below 1000 that the registry does not define for KRN.
    UnknownKey(FieldPath),
    /// A required key is absent.
    MissingKey(FieldPath),
    /// A field has the wrong CBOR type.
    WrongType(FieldPath),
    /// A field has the right type but an invalid value (length, range or characters).
    InvalidValue(FieldPath),
    /// `cpu_features` sets a reserved bit (6–63).
    ReservedFeatureBits(u64),
    /// `cet.ibt` is `true`; IBT stays off until stable `rustc` can emit ENDBR64.
    IbtNotSupported,
    /// The image is not a KRN image.
    NotKrn,
    /// `entry` is not inside the content of a readable, executable SEGMENT.
    EntryNotExecutable(u64),
}

impl From<OcborError> for ManifestError {
    fn from(e: OcborError) -> Self {
        Self::Ocbor(e)
    }
}

/// Required CPU features beyond `x86-64-v3` (key 3; registry bits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuFeatures(pub u64);

impl CpuFeatures {
    /// AVX-512 Foundation.
    pub const AVX512F: u64 = 1 << 0;
    /// AVX10, version 2 or later.
    pub const AVX10_2: u64 = 1 << 1;
    /// Advanced Performance Extensions.
    pub const APX: u64 = 1 << 2;
    /// SHA-NI.
    pub const SHA: u64 = 1 << 3;
    /// Galois Field New Instructions.
    pub const GFNI: u64 = 1 << 4;
    /// Vector AES.
    pub const VAES: u64 = 1 << 5;
    /// Every bit the registry defines.
    pub const KNOWN: u64 = 0x3F;
}

/// CET settings (key 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cet {
    /// Shadow stack.
    pub shstk: bool,
    /// Indirect-branch tracking; always `false` in v1.
    pub ibt: bool,
}

/// Build provenance (key 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Toolchain<'a> {
    /// `rustc` release, such as `1.99.1`.
    pub rustc: &'a str,
    /// `rustc` commit hash: 40 lowercase hex digits.
    pub rustc_commit: &'a str,
    /// LLVM version.
    pub llvm: &'a str,
    /// SHA-256 of the canonical compiler and linker flags.
    pub flags_digest: [u8; 32],
}

/// A decoded KRN manifest. Every field is required.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KrnManifest<'a> {
    /// SHA-256 of all SEGMENT contents in table order (key 1).
    pub build_id: [u8; 32],
    /// Build provenance (key 2).
    pub toolchain: Toolchain<'a>,
    /// Required CPU features (key 3).
    pub cpu_features: CpuFeatures,
    /// CET settings (key 4).
    pub cet: Cet,
    /// Entry point RVA (key 5).
    pub entry: u64,
    /// Boot-info ABI `(major, minor)` (key 24).
    pub boot_info_abi: (u16, u16),
}

const KEY_SCHEMA: u64 = 0;
const KEY_BUILD_ID: u64 = 1;
const KEY_TOOLCHAIN: u64 = 2;
const KEY_CPU_FEATURES: u64 = 3;
const KEY_CET: u64 = 4;
const KEY_ENTRY: u64 = 5;
const KEY_BOOT_INFO_ABI: u64 = 24;
/// Keys at or above this are optional extensions (§5).
const FIRST_EXTENSION: u64 = 1000;

fn uint(c: &mut Cursor<'_>, path: FieldPath) -> Result<u64, ManifestError> {
    match c.next_item()? {
        Item::Unsigned(v) => Ok(v),
        other => {
            c.skip_contents(other)?;
            Err(ManifestError::WrongType(path))
        }
    }
}

fn digest32(c: &mut Cursor<'_>, path: FieldPath) -> Result<[u8; 32], ManifestError> {
    match c.next_item()? {
        Item::Bytes(b) => b.try_into().map_err(|_| ManifestError::InvalidValue(path)),
        _ => Err(ManifestError::WrongType(path)),
    }
}

/// A text field restricted to printable ASCII, which is trivially NFC (§5).
fn ascii_text<'a>(c: &mut Cursor<'a>, path: FieldPath) -> Result<&'a str, ManifestError> {
    match c.next_item()? {
        Item::Text(t) if !t.is_empty() && t.bytes().all(|b| (0x20..=0x7E).contains(&b)) => Ok(t),
        Item::Text(_) => Err(ManifestError::InvalidValue(path)),
        _ => Err(ManifestError::WrongType(path)),
    }
}

fn map_len(c: &mut Cursor<'_>, path: FieldPath) -> Result<u64, ManifestError> {
    match c.next_item()? {
        Item::Map(n) => Ok(n),
        other => {
            c.skip_contents(other)?;
            Err(ManifestError::WrongType(path))
        }
    }
}

/// Reads the key of the next map entry; validation guarantees it is an unsigned integer.
fn key(c: &mut Cursor<'_>) -> Result<u64, ManifestError> {
    match c.next_item()? {
        Item::Unsigned(k) => Ok(k),
        _ => Err(ManifestError::NotAMap),
    }
}

/// Skips one value (an extension's value, for example).
fn skip_value(c: &mut Cursor<'_>) -> Result<(), ManifestError> {
    let item = c.next_item()?;
    c.skip_contents(item)?;
    Ok(())
}

fn decode_toolchain<'a>(c: &mut Cursor<'a>) -> Result<Toolchain<'a>, ManifestError> {
    let n = map_len(c, top(KEY_TOOLCHAIN))?;
    let (mut rustc, mut commit, mut llvm, mut flags) = (None, None, None, None);
    for _ in 0..n {
        let sub = key(c)?;
        let path = nested(KEY_TOOLCHAIN, sub);
        match sub {
            0 => rustc = Some(ascii_text(c, path)?),
            1 => {
                let t = ascii_text(c, path)?;
                let hex = t.len() == 40
                    && t.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
                if !hex {
                    return Err(ManifestError::InvalidValue(path));
                }
                commit = Some(t);
            }
            2 => llvm = Some(ascii_text(c, path)?),
            3 => flags = Some(digest32(c, path)?),
            s if s >= FIRST_EXTENSION => skip_value(c)?,
            _ => return Err(ManifestError::UnknownKey(path)),
        }
    }
    let missing = |sub| ManifestError::MissingKey(nested(KEY_TOOLCHAIN, sub));
    Ok(Toolchain {
        rustc: rustc.ok_or(missing(0))?,
        rustc_commit: commit.ok_or(missing(1))?,
        llvm: llvm.ok_or(missing(2))?,
        flags_digest: flags.ok_or(missing(3))?,
    })
}

fn bool_field(c: &mut Cursor<'_>, path: FieldPath) -> Result<bool, ManifestError> {
    match c.next_item()? {
        Item::Bool(b) => Ok(b),
        other => {
            c.skip_contents(other)?;
            Err(ManifestError::WrongType(path))
        }
    }
}

fn decode_cet(c: &mut Cursor<'_>) -> Result<Cet, ManifestError> {
    let n = map_len(c, top(KEY_CET))?;
    let (mut shstk, mut ibt) = (None, None);
    for _ in 0..n {
        let sub = key(c)?;
        let path = nested(KEY_CET, sub);
        match sub {
            0 => shstk = Some(bool_field(c, path)?),
            1 => ibt = Some(bool_field(c, path)?),
            s if s >= FIRST_EXTENSION => skip_value(c)?,
            _ => return Err(ManifestError::UnknownKey(path)),
        }
    }
    let cet = Cet {
        shstk: shstk.ok_or(ManifestError::MissingKey(nested(KEY_CET, 0)))?,
        ibt: ibt.ok_or(ManifestError::MissingKey(nested(KEY_CET, 1)))?,
    };
    if cet.ibt {
        return Err(ManifestError::IbtNotSupported);
    }
    Ok(cet)
}

fn decode_version_pair(c: &mut Cursor<'_>, path: FieldPath) -> Result<(u16, u16), ManifestError> {
    match c.next_item()? {
        Item::Array(2) => {}
        other => {
            c.skip_contents(other)?;
            return Err(ManifestError::WrongType(path));
        }
    }
    let major = uint(c, path)?;
    let minor = uint(c, path)?;
    let narrow = |v: u64| u16::try_from(v).map_err(|_| ManifestError::InvalidValue(path));
    Ok((narrow(major)?, narrow(minor)?))
}

impl<'a> KrnManifest<'a> {
    /// Validates `bytes` as OCBOR-1 and decodes the KRN schema.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, ManifestError> {
        ocbor::validate(bytes)?;
        let mut c = Cursor::new(bytes);
        let Item::Map(n) = c.next_item()? else {
            return Err(ManifestError::NotAMap);
        };
        let mut schema = None;
        let mut build_id = None;
        let mut toolchain = None;
        let mut cpu_features = None;
        let mut cet = None;
        let mut entry = None;
        let mut boot_info_abi = None;
        for _ in 0..n {
            let k = key(&mut c)?;
            let path = top(k);
            match k {
                KEY_SCHEMA => {
                    let v = uint(&mut c, path)?;
                    if v != 1 {
                        return Err(ManifestError::UnsupportedSchema(v));
                    }
                    schema = Some(v);
                }
                KEY_BUILD_ID => build_id = Some(digest32(&mut c, path)?),
                KEY_TOOLCHAIN => toolchain = Some(decode_toolchain(&mut c)?),
                KEY_CPU_FEATURES => {
                    let bits = uint(&mut c, path)?;
                    if bits & !CpuFeatures::KNOWN != 0 {
                        return Err(ManifestError::ReservedFeatureBits(
                            bits & !CpuFeatures::KNOWN,
                        ));
                    }
                    cpu_features = Some(CpuFeatures(bits));
                }
                KEY_CET => cet = Some(decode_cet(&mut c)?),
                KEY_ENTRY => entry = Some(uint(&mut c, path)?),
                KEY_BOOT_INFO_ABI => boot_info_abi = Some(decode_version_pair(&mut c, path)?),
                k if k >= FIRST_EXTENSION => skip_value(&mut c)?,
                _ => return Err(ManifestError::UnknownKey(path)),
            }
        }
        let missing = |k| ManifestError::MissingKey(top(k));
        schema.ok_or(missing(KEY_SCHEMA))?;
        Ok(Self {
            build_id: build_id.ok_or(missing(KEY_BUILD_ID))?,
            toolchain: toolchain.ok_or(missing(KEY_TOOLCHAIN))?,
            cpu_features: cpu_features.ok_or(missing(KEY_CPU_FEATURES))?,
            cet: cet.ok_or(missing(KEY_CET))?,
            entry: entry.ok_or(missing(KEY_ENTRY))?,
            boot_info_abi: boot_info_abi.ok_or(missing(KEY_BOOT_INFO_ABI))?,
        })
    }

    /// Checks the manifest against the image it came from: the image is KRN, and `entry`
    /// lies inside the content (not the zero-filled tail) of a SEGMENT that is readable and
    /// executable.
    pub fn check_against(&self, image: &Image<'_>) -> Result<(), ManifestError> {
        if image.header().profile != Profile::Krn {
            return Err(ManifestError::NotKrn);
        }
        let in_code = image.sections().any(|s| {
            let e = &s.entry;
            e.kind() == Kind::Segment
                && e.flags.has(Flags::READ | Flags::EXECUTE)
                && self.entry >= e.mem_offset
                && e.mem_offset
                    .checked_add(e.content_size)
                    .is_some_and(|end| self.entry < end)
        });
        if in_code {
            Ok(())
        } else {
            Err(ManifestError::EntryNotExecutable(self.entry))
        }
    }
}
