//! The signature block (ONBF §7.1) written from the fuzzer's choices, and its parser and
//! verifiers run.
//!
//! A fuzzed signature never verifies, so what is reached is everything before the arithmetic
//! succeeds: the block's framing, each entry's OCBOR-1 map and statement, the key and digest
//! checks of `onbf-verify`, and the signature verifiers' decoding of what they are given.
//! The verifiers are also called directly on each entry's signature bytes, with the test
//! kernel key, because `verify_entry` stops at the first signature that fails and so never
//! hands a fuzzed ML-DSA signature to its verifier.

use arbitrary::{Result, Unstructured};
use onbf_format::signature::{Entry, SignatureBlock, Suite};
use onbf_verify::{key_id, verify_entry, verify_krn_block};
use origin_boot_uefi::trust::STORE;
use origin_crypto::{
    MLDSA65_PUBLIC_KEY_BYTES, MLDSA65_SIGNATURE_BYTES, MLDSA87_PUBLIC_KEY_BYTES,
    MLDSA87_SIGNATURE_BYTES, ed25519_verify, mldsa65_verify, mldsa87_verify,
};

use crate::ocbor::{bytes, head, item, text, uint};

/// The block's magic (§7.1).
const MAGIC: &[u8; 8] = b"ONBFSIG1";
/// The kernel role and its suite (§7.2): what the test key in the loader's store signs as.
const ROLE_KERNEL: u64 = 4;

/// Writes a signature block whose statements carry `content_digest` and `profile`, so that
/// the checks of `onbf-verify` before the signature arithmetic are reached. With probability
/// set by `u`, the framing or one field is broken.
pub fn block(u: &mut Unstructured<'_>, content_digest: &[u8; 64], profile: u64) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    if u.ratio(63, 64)? {
        out.extend_from_slice(MAGIC);
    } else {
        let m: [u8; 8] = u.arbitrary()?;
        out.extend_from_slice(&m);
    }
    let version: u32 = if u.ratio(63, 64)? { 1 } else { u.arbitrary()? };
    out.extend_from_slice(&version.to_le_bytes());
    let entries: u32 = u.int_in_range(1..=3)?;
    let declared: u32 = if u.ratio(31, 32)? {
        entries
    } else {
        u.arbitrary()?
    };
    out.extend_from_slice(&declared.to_le_bytes());
    for index in 0..entries {
        let body = entry(u, content_digest, profile, index)?;
        let size = u32::try_from(body.len()).unwrap_or(u32::MAX);
        let size = if u.ratio(31, 32)? {
            size
        } else {
            u.arbitrary()?
        };
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&body);
    }
    if u.ratio(1, 32)? {
        let n = u.int_in_range(1..=16)?;
        out.extend_from_slice(u.bytes(n.min(u.len()))?);
    }
    Ok(out)
}

/// One entry's OCBOR-1 map (§7.1): seven fields in key order, one of them perhaps replaced
/// by an arbitrary item. Entry `index` takes a role of its own (kernel, then release, then
/// publisher), since a block may not hold two entries of one role and one key.
fn entry(
    u: &mut Unstructured<'_>,
    content_digest: &[u8; 64],
    profile: u64,
    index: u32,
) -> Result<Vec<u8>> {
    let role: u64 = if u.ratio(31, 32)? {
        [ROLE_KERNEL, 5, 1][usize::try_from(index).unwrap_or(0).min(2)]
    } else {
        u.int_in_range(0..=6)?
    };
    // Kernel and release keys use Ed25519 + ML-DSA-87 (suite 2), the others ML-DSA-65.
    let suite: u64 = if u.ratio(31, 32)? {
        if role == 4 || role == 5 { 2 } else { 1 }
    } else {
        u.int_in_range(0..=3)?
    };
    let spoil: u32 = if u.ratio(1, 16)? {
        u.int_in_range(1..=7)?
    } else {
        0
    };
    let mut out = Vec::new();
    head(&mut out, 5, 7);
    for key in 1..=7u32 {
        uint(&mut out, u64::from(key));
        if key == spoil {
            item(u, &mut out, 1)?;
            continue;
        }
        match key {
            1 => uint(&mut out, role),
            2 => uint(&mut out, suite),
            3 => {
                let id = match STORE.first() {
                    Some(trusted) if u.ratio(15, 16)? => key_id(&trusted.key),
                    _ => u.arbitrary()?,
                };
                bytes(&mut out, &id);
            }
            4 => {
                // The chain of Origin Key Certificates: maps, at most four (§7.3, Appendix A).
                // The parser only skips a certificate's contents, so they are mostly valid
                // OCBOR-1 bytes, and now and then an arbitrary item.
                let n = u.int_in_range(0..=2)?;
                head(&mut out, 4, n);
                for _ in 0..n {
                    head(&mut out, 5, 1);
                    uint(&mut out, 1);
                    if u.ratio(15, 16)? {
                        let k: [u8; 32] = u.arbitrary()?;
                        bytes(&mut out, &k);
                    } else {
                        item(u, &mut out, 3)?;
                    }
                }
            }
            5 => statement(u, &mut out, role, content_digest, profile)?,
            6 => {
                let sig = signature_bytes(u, 64)?;
                bytes(&mut out, &sig);
            }
            _ => {
                let len = if suite == 2 {
                    MLDSA87_SIGNATURE_BYTES
                } else {
                    MLDSA65_SIGNATURE_BYTES
                };
                let sig = signature_bytes(u, len)?;
                bytes(&mut out, &sig);
            }
        }
    }
    Ok(out)
}

/// The signed statement (§7.1), a map of seven fields.
fn statement(
    u: &mut Unstructured<'_>,
    out: &mut Vec<u8>,
    role: u64,
    content_digest: &[u8; 64],
    profile: u64,
) -> Result<()> {
    head(out, 5, 7);
    uint(out, 0);
    uint(out, if u.ratio(63, 64)? { 1 } else { u.arbitrary()? });
    uint(out, 1);
    uint(
        out,
        if u.ratio(15, 16)? {
            profile
        } else {
            u.arbitrary()?
        },
    );
    uint(out, 2);
    if u.ratio(15, 16)? {
        bytes(out, content_digest);
    } else {
        let d: [u8; 64] = u.arbitrary()?;
        bytes(out, &d);
    }
    uint(out, 3);
    if u.ratio(31, 32)? {
        text(out, "origin-kernel");
    } else {
        let t: &str = u.arbitrary()?;
        text(out, t);
    }
    uint(out, 4);
    // Mostly a version SemVer 2.0.0 allows, now and then one it does not. Each field is broken
    // rarely, since a block holds up to three entries and one broken field refuses it.
    let version = if u.ratio(31, 32)? {
        *u.choose(&["0.1.0", "1.0.0-rc.1+build.7", "2.10.300-alpha.beta"])?
    } else {
        *u.choose(&["01.0.0", "1.0", "1.0.0-", "1.0.0+", "1.0.0-01"])?
    };
    text(out, version);
    uint(out, 5);
    uint(
        out,
        if u.ratio(31, 32)? {
            role
        } else {
            u.int_in_range(0..=6)?
        },
    );
    uint(out, 6);
    uint(out, u.arbitrary()?);
    Ok(())
}

/// `len` signature bytes: the fuzzer's, then zeros.
fn signature_bytes(u: &mut Unstructured<'_>, len: usize) -> Result<Vec<u8>> {
    let mut sig = vec![0u8; len];
    let n = len.min(u.len());
    sig[..n].copy_from_slice(u.bytes(n)?);
    Ok(sig)
}

/// Parses `block`, judges what the parser accepts, and runs every verifier over it:
/// `verify_entry` against each trusted key, `verify_krn_block` against the loader's store,
/// and the Ed25519 and ML-DSA verifiers directly on each entry's signatures.
///
/// # Panics
///
/// On a finding: an accepted block that breaks a rule of §7.1 its parser promises.
pub fn exercise(block: &[u8], content_digest: &[u8; 64], profile: u64) {
    if let Ok(parsed) = SignatureBlock::parse(block) {
        let mut entries = 0usize;
        for e in parsed.entries() {
            entries += 1;
            assert_eq!(
                e.key_id.len(),
                32,
                "ONBF §7.1: an accepted key_id is 32 bytes"
            );
            assert_eq!(
                e.sig_ed25519.len(),
                64,
                "ONBF §7.1: an accepted Ed25519 signature is 64 bytes"
            );
            assert_eq!(
                e.sig_mldsa.len(),
                e.suite.mldsa_signature_len(),
                "ONBF §7.1: an accepted ML-DSA signature has its suite's length"
            );
            assert_eq!(
                e.suite,
                e.role.suite(),
                "ONBF §7.2: an accepted entry's suite is its role's"
            );
            assert_eq!(
                e.statement.role, e.role,
                "ONBF §7.1: an accepted statement names its entry's role"
            );
            assert_eq!(
                e.statement.content_digest.len(),
                64,
                "ONBF §7.1: an accepted statement's digest is 64 bytes"
            );
            for trusted in STORE {
                let _ = verify_entry(e, &trusted.key, content_digest, profile);
            }
            verifiers(e);
        }
        assert!(
            (1..=8).contains(&entries),
            "ONBF §7.1: an accepted block holds 1 to 8 entries, not {entries}"
        );
    }
    let _ = verify_krn_block(block, STORE, content_digest);
}

/// The signature verifiers on an entry's signatures, with the test kernel key. ML-DSA-65 has
/// no trusted key in the loader's store, so the first 1,952 bytes of the ML-DSA-87 key stand in:
/// any bytes decode as a public key, and what is fuzzed is the signature's decoding.
fn verifiers(e: &Entry<'_>) {
    let Some(trusted) = STORE.first() else {
        return;
    };
    if let Ok(sig) = <&[u8; 64]>::try_from(e.sig_ed25519) {
        let _ = ed25519_verify(trusted.key.ed25519, e.statement.bytes, sig);
    }
    match e.suite {
        Suite::Oss1R => {
            if let (Ok(pk), Ok(sig)) = (
                <&[u8; MLDSA87_PUBLIC_KEY_BYTES]>::try_from(trusted.key.mldsa),
                <&[u8; MLDSA87_SIGNATURE_BYTES]>::try_from(e.sig_mldsa),
            ) {
                let _ = mldsa87_verify(pk, e.statement.bytes, e.role.context(), sig);
            }
        }
        Suite::Oss1 => {
            let pk = trusted
                .key
                .mldsa
                .get(..MLDSA65_PUBLIC_KEY_BYTES)
                .and_then(|k| <&[u8; MLDSA65_PUBLIC_KEY_BYTES]>::try_from(k).ok());
            if let (Some(pk), Ok(sig)) =
                (pk, <&[u8; MLDSA65_SIGNATURE_BYTES]>::try_from(e.sig_mldsa))
            {
                let _ = mldsa65_verify(pk, e.statement.bytes, e.role.context(), sig);
            }
        }
    }
}

/// The content digest the raw target's statements are checked against, and that the seeds'
/// statements carry, so that `verify_entry` gets past its digest check on them.
pub const RAW_DIGEST: [u8; 64] = [0x5A; 64];

/// The `signature` target: `data` as a whole block, against [`RAW_DIGEST`] and the KRN profile.
pub fn raw(data: &[u8]) {
    exercise(data, &RAW_DIGEST, 6);
}

/// The `signature_built` target: a block written from `data`'s choices, then its readers.
pub fn built(data: &[u8]) {
    let mut u = Unstructured::new(data);
    let written = (|| {
        let digest: [u8; 64] = u.arbitrary()?;
        let profile = if u.ratio(7, 8)? { 6 } else { u.arbitrary()? };
        Ok::<_, arbitrary::Error>((digest, profile, block(&mut u, &digest, profile)?))
    })();
    if let Ok((digest, profile, b)) = written {
        exercise(&b, &digest, profile);
    }
}
