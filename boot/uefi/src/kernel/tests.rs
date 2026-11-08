//! The loader's signature step, on a minimal image around the fixture block
//! `runtime/onbf/verify/tools/make_fixture.py` signed with the independent Python references.
//! The boot tests prove the whole path; here each refusal is pinned, with its own message.

use onbf_format::signature::{Role, SignatureBlock};
use onbf_format::{Flags, Image, ParseOptions, Profile};
use onbf_verify::{KrnBlockError, PublicKey, TrustedKey, VerifyError};
use origin_crypto::Sha256;

use super::{KernelError, check_sections, check_signature, verify};
use crate::trust;

struct Fixture {
    digest: [u8; 64],
    ed: [u8; 32],
    ml: Vec<u8>,
    block: Vec<u8>,
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn fixture() -> Fixture {
    let text = include_str!("../../../../runtime/onbf/verify/tests/vectors/kernel-block.txt");
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{name} ")))
            .unwrap()
            .to_string()
    };
    Fixture {
        digest: unhex(&field("digest")).try_into().unwrap(),
        ed: unhex(&field("ed25519_pk")).try_into().unwrap(),
        ml: unhex(&field("mldsa87_pk")),
        block: unhex(&field("block")),
    }
}

/// A one-section KRN image: `content_digest` carries `digest` (the fixture's, which the
/// signature covers) and the signature block, if any, follows the manifest.
fn image_bytes(digest: &[u8; 64], block: Option<&[u8]>) -> Vec<u8> {
    let manifest = [0xA5u8; 8];
    let table_end = 128 + 128;
    let manifest_at = 4096usize; // the canonical offset (errata C1)
    let block_at = (manifest_at + manifest.len()).next_multiple_of(8); // errata C3
    let mut out = vec![0u8; manifest_at + manifest.len()];
    out[..8].copy_from_slice(&onbf_format::MAGIC);
    out[8..10].copy_from_slice(&1u16.to_le_bytes());
    out[12..14].copy_from_slice(&(Profile::Krn as u16).to_le_bytes());
    out[14..16].copy_from_slice(&128u16.to_le_bytes());
    out[20..24].copy_from_slice(&1u32.to_le_bytes());
    out[24..32].copy_from_slice(&128u64.to_le_bytes());
    out[48..112].copy_from_slice(digest);
    out[112..114].copy_from_slice(&1u16.to_le_bytes());
    // One MANIFEST section entry (errata C7, C12, G5).
    out[128..132].copy_from_slice(&1u32.to_le_bytes());
    out[132..136].copy_from_slice(&Flags::REQUIRED.to_le_bytes());
    out[136..144].copy_from_slice(&(manifest_at as u64).to_le_bytes());
    out[144..152].copy_from_slice(&(manifest.len() as u64).to_le_bytes());
    out[152..160].copy_from_slice(&(manifest.len() as u64).to_le_bytes());
    out[168..176].copy_from_slice(&(manifest.len() as u64).to_le_bytes());
    out[180..184].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    out[192..224].copy_from_slice(&Sha256::digest(&manifest));
    assert!(table_end <= manifest_at);
    out[manifest_at..manifest_at + manifest.len()].copy_from_slice(&manifest);
    if let Some(block) = block {
        out.resize(block_at, 0);
        out.extend_from_slice(block);
        out[32..40].copy_from_slice(&(block_at as u64).to_le_bytes());
        out[40..48].copy_from_slice(&(block.len() as u64).to_le_bytes());
    }
    out
}

fn store(f: &Fixture) -> [TrustedKey<'_>; 1] {
    [TrustedKey {
        role: Role::Kernel,
        key: PublicKey {
            ed25519: &f.ed,
            mldsa: &f.ml,
        },
    }]
}

/// Parses `bytes` as a loader outside developer mode would, and runs the signature step.
fn signature_verdict(
    bytes: &[u8],
    store: &[TrustedKey<'_>],
) -> Result<Option<[u8; 32]>, KernelError> {
    let image = Image::parse(bytes, ParseOptions::default()).expect("the image parses");
    check_signature(&image, false, store)
}

#[test]
fn a_good_signature_names_its_signer() {
    let f = fixture();
    let bytes = image_bytes(&f.digest, Some(&f.block));
    let store = store(&f);
    let key = onbf_verify::key_id(&store[0].key);
    assert_eq!(signature_verdict(&bytes, &store), Ok(Some(key)));
}

/// Without a signature: refused outside developer mode with its own message, and accepted
/// in developer mode (which `verify` reaches only with a good content digest).
#[test]
fn an_unsigned_image_is_refused_outside_developer_mode() {
    let f = fixture();
    let bytes = image_bytes(&f.digest, None);
    assert_eq!(
        verify(&bytes, false, &store(&f)).map(|_| ()),
        Err(KernelError::Unsigned)
    );
    assert!(
        KernelError::Unsigned
            .to_string()
            .starts_with("unsigned image:")
    );
    // Developer mode gets past the parse (this hand-made image's digest is the fixture's,
    // so the content digest refuses it next).
    assert_eq!(
        verify(&bytes, true, &store(&f)).map(|_| ()),
        Err(KernelError::ContentDigest)
    );
    // An unsigned image has no signature to check, which is acceptable only in developer
    // mode: the step refuses it by itself, whatever the parser let through.
    let image = Image::parse(
        &bytes,
        ParseOptions {
            developer_mode: true,
            ..ParseOptions::default()
        },
    )
    .unwrap();
    assert_eq!(check_signature(&image, true, &store(&f)), Ok(None));
    assert_eq!(
        check_signature(&image, false, &store(&f)),
        Err(KernelError::Unsigned)
    );
}

/// Each refusal has its own message; `xtask` matches these on the serial console.
#[test]
fn each_refusal_says_what_it_is() {
    let f = fixture();
    let parsed = SignatureBlock::parse(&f.block).unwrap();
    let entry = parsed.entries().next().unwrap();
    let at = |part: &[u8]| part.as_ptr() as usize - f.block.as_ptr() as usize;

    let refuse = |block: &[u8], store: &[TrustedKey<'_>]| {
        let bytes = image_bytes(&f.digest, Some(block));
        signature_verdict(&bytes, store).unwrap_err()
    };
    let store = store(&f);

    // A wrong key: nothing in the store has the entry's key ID.
    assert_eq!(
        refuse(&f.block, &[]),
        KernelError::Signature(KrnBlockError::UntrustedKey)
    );
    assert_eq!(
        KernelError::Signature(KrnBlockError::UntrustedKey).to_string(),
        "signed by a key this loader does not trust"
    );

    // A tampered statement: its digest no longer names this image.
    let mut block = f.block.clone();
    block[at(entry.statement.content_digest)] ^= 1;
    let err = refuse(&block, &store);
    assert_eq!(
        err,
        KernelError::Signature(KrnBlockError::Entry(VerifyError::Digest))
    );
    assert!(
        err.to_string()
            .starts_with("the signed statement does not name this image")
    );

    // A tampered statement whose digest is intact: the signature no longer covers it.
    let mut block = f.block.clone();
    block[at(entry.statement.id.as_bytes())] ^= 1;
    let err = refuse(&block, &store);
    assert!(
        matches!(
            err,
            KernelError::Signature(KrnBlockError::Entry(VerifyError::Ed25519(_)))
        ),
        "{err:?}"
    );
    assert!(
        err.to_string()
            .contains("the signature or the statement was altered")
    );

    // A bad signature, each algorithm on its own: the other one is intact.
    let mut block = f.block.clone();
    block[at(entry.sig_ed25519) + 40] ^= 1;
    assert!(
        refuse(&block, &store)
            .to_string()
            .starts_with("the Ed25519 signature")
    );
    let mut block = f.block.clone();
    block[at(entry.sig_mldsa) + 200] ^= 1;
    assert!(
        refuse(&block, &store)
            .to_string()
            .starts_with("the ML-DSA signature")
    );

    // A block that does not parse.
    let mut block = f.block.clone();
    block[0] ^= 1;
    assert!(
        refuse(&block, &store)
            .to_string()
            .starts_with("malformed signature block")
    );
}

/// A store that is empty (the release build today) refuses a good signature.
#[test]
fn an_empty_trust_store_refuses_everything() {
    let f = fixture();
    let bytes = image_bytes(&f.digest, Some(&f.block));
    assert_eq!(
        signature_verdict(&bytes, &[]),
        Err(KernelError::Signature(KrnBlockError::UntrustedKey))
    );
    // This test build has no feature, so its store is that empty one.
    assert!(trust::STORE.is_empty());
    const { assert!(!trust::HOLDS_TEST_KEY) };
}

/// A signed image that is not a KRN image is never handed to the verifier.
#[test]
fn another_profile_is_refused_before_the_signature() {
    let f = fixture();
    let mut bytes = image_bytes(&f.digest, Some(&f.block));
    bytes[12..14].copy_from_slice(&(Profile::Lib as u16).to_le_bytes());
    assert_eq!(
        verify(&bytes, false, &store(&f)).map(|_| ()),
        Err(KernelError::NotKrn)
    );
}

/// An unsigned, developer-mode KRN container: an 8-byte MANIFEST, a 5,000-byte SEGMENT and that
/// segment's MERKLE section, each with its true digest. Returns the image and the file ranges of
/// the segment and of the MERKLE section. (The manifest is not an OCBOR document: this exercises
/// `check_sections` alone.)
fn container_with_merkle() -> (Vec<u8>, std::ops::Range<usize>, std::ops::Range<usize>) {
    let manifest = [0xA5u8; 8];
    let content: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
    let mut blocks = vec![0u8; 4096];
    let root = onbf_merkle::build(&content, &mut blocks).unwrap();
    // The canonical layout (errata C1): the table ends at 512.
    let (manifest_at, segment_at) = (4096usize, 8192usize);
    let merkle_at = (segment_at + content.len()).next_multiple_of(8);
    let mut out = vec![0u8; merkle_at + blocks.len()];
    out[..8].copy_from_slice(&onbf_format::MAGIC);
    out[8..10].copy_from_slice(&1u16.to_le_bytes());
    out[12..14].copy_from_slice(&(Profile::Krn as u16).to_le_bytes());
    out[14..16].copy_from_slice(&128u16.to_le_bytes());
    out[20..24].copy_from_slice(&3u32.to_le_bytes());
    out[24..32].copy_from_slice(&128u64.to_le_bytes());
    out[112..114].copy_from_slice(&1u16.to_le_bytes());
    let entry = |out: &mut [u8], i: usize, kind: u32, flags: u32, at: usize, len: usize| {
        let e = 128 + 128 * i;
        out[e..e + 4].copy_from_slice(&kind.to_le_bytes());
        out[e + 4..e + 8].copy_from_slice(&flags.to_le_bytes());
        out[e + 8..e + 16].copy_from_slice(&(at as u64).to_le_bytes());
        out[e + 16..e + 24].copy_from_slice(&(len as u64).to_le_bytes());
        out[e + 24..e + 32].copy_from_slice(&(len as u64).to_le_bytes());
        out[e + 40..e + 48].copy_from_slice(&(len as u64).to_le_bytes());
        out[e + 52..e + 56].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    };
    entry(
        &mut out,
        0,
        0x0001,
        Flags::REQUIRED,
        manifest_at,
        manifest.len(),
    );
    out[128 + 64..128 + 96].copy_from_slice(&Sha256::digest(&manifest));
    out[manifest_at..manifest_at + manifest.len()].copy_from_slice(&manifest);
    let code = Flags::REQUIRED | Flags::MAPPABLE | Flags::READ | Flags::EXECUTE;
    entry(&mut out, 1, 0x0010, code, segment_at, content.len());
    let s = 128 + 128;
    out[s + 40..s + 48].copy_from_slice(&8192u64.to_le_bytes());
    out[s + 49] = 1; // integrity: Merkle
    out[s + 52..s + 56].copy_from_slice(&2u32.to_le_bytes());
    out[s + 64..s + 96].copy_from_slice(&root);
    out[segment_at..segment_at + content.len()].copy_from_slice(&content);
    entry(
        &mut out,
        2,
        0x00F0,
        Flags::REQUIRED,
        merkle_at,
        blocks.len(),
    );
    out[128 + 256 + 49] = 2; // integrity: Merkle hash blocks
    out[merkle_at..merkle_at + blocks.len()].copy_from_slice(&blocks);
    (
        out,
        segment_at..segment_at + content.len(),
        merkle_at..merkle_at + blocks.len(),
    )
}

fn sections_verdict(bytes: &[u8]) -> Result<(), KernelError> {
    let options = ParseOptions {
        developer_mode: true,
        ..ParseOptions::default()
    };
    check_sections(&Image::parse(bytes, options).expect("the image parses"))
}

/// ONBF §4.5: every byte of every section is covered, so a changed MERKLE byte is refused even
/// though nothing in the boot path reads those bytes, and even with every digest intact.
#[test]
fn a_changed_merkle_byte_is_refused() {
    let (bytes, segment, merkle) = container_with_merkle();
    assert_eq!(sections_verdict(&bytes), Ok(()));
    assert_eq!(
        KernelError::MerkleBlocks(2).to_string(),
        "section 2 (MERKLE) is not the hash blocks of the section it serves"
    );
    for at in merkle {
        let mut t = bytes.clone();
        t[at] ^= 0x10;
        assert_eq!(
            sections_verdict(&t),
            Err(KernelError::MerkleBlocks(2)),
            "byte {at}"
        );
    }
    // A changed segment byte is still that section's own failure.
    let mut t = bytes.clone();
    t[segment.start + 7] ^= 1;
    assert_eq!(sections_verdict(&t), Err(KernelError::SectionDigest(1)));
}
