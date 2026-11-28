//! The writers' own tests: what each writes reaches acceptance often enough that its target
//! gets past the first checks (the contract's "its seeds reach acceptance"), the sealer
//! restores what a flipped byte broke, and every exerciser runs clean over the writers' output.
//! They run under plain `cargo test`, without libFuzzer, and `cargo xtask fuzz` runs them
//! before it fuzzes.

use arbitrary::Unstructured;
use onbf_format::signature::SignatureBlock;
use onbf_format::{Image, Kind, KrnManifest, ParseOptions, ocbor};
use origin_aml::{Config, DefinitionBlock, IntegerWidth, Interpreter, Limits};
use origin_boot_uefi::{kernel, trust::STORE};

use crate::aml::FuzzHost;
use crate::fixed_bytes;

/// Inputs per test.
const RUNS: u64 = 300;
const DEVELOPER: ParseOptions = ParseOptions {
    nested_in_verified_parent: false,
    developer_mode: true,
};

#[test]
fn overflow_checks_are_on_in_this_build() {
    crate::require_overflow_checks();
}

#[test]
fn written_images_parse_and_verify() {
    let (mut parsed, mut verified) = (0u64, 0u64);
    for seed in 0..RUNS {
        let data = fixed_bytes(seed, 8192);
        let Ok(image) = crate::onbf::image(&mut Unstructured::new(&data)) else {
            continue;
        };
        parsed += u64::from(Image::parse(&image, DEVELOPER).is_ok());
        verified += u64::from(kernel::verify(&image, true, STORE).is_ok());
        crate::onbf::exercise(&image);
        crate::onbf::loader(&image);
    }
    println!(
        "ONBF writer: of {RUNS} images, {parsed} parsed and {verified} verified by the loader"
    );
    assert!(
        parsed * 2 >= RUNS,
        "the ONBF writer's images are mostly refused: {parsed} of {RUNS} parsed"
    );
    assert!(
        verified * 5 >= RUNS,
        "the loader verifies too few written images: {verified} of {RUNS}"
    );
}

#[test]
fn sealing_restores_what_a_flipped_byte_broke() {
    let mut checked = 0;
    for seed in 0..RUNS {
        let data = fixed_bytes(seed, 8192);
        let Ok(mut image) = crate::onbf::image(&mut Unstructured::new(&data)) else {
            continue;
        };
        // An unsigned image the loader accepts, and a section in it whose bytes mean nothing
        // to the loader but their digest: hash blocks, unwind tables, a debug link, exports.
        // (A flipped byte in the manifest or a segment is resealed too, but changes what the
        // loader reads, so the image may rightly stay refused.)
        let unsigned = image.get(32..40) == Some(&[0u8; 8][..]);
        let opaque = Image::parse(&image, DEVELOPER).ok().and_then(|p| {
            p.sections()
                .find(|s| {
                    matches!(
                        s.entry.kind(),
                        Kind::Merkle | Kind::Unwind | Kind::DebugLink | Kind::Exports
                    ) && !s.stored.is_empty()
                })
                .and_then(|s| usize::try_from(s.entry.file_offset).ok())
        });
        let Some(at) = opaque else { continue };
        if !unsigned || kernel::verify(&image, true, STORE).is_err() {
            continue;
        }
        image[at] ^= 0x55;
        assert!(
            kernel::verify(&image, true, STORE).is_err(),
            "a flipped content byte went unnoticed (seed {seed})"
        );
        crate::onbf::seal(&mut image);
        assert!(
            kernel::verify(&image, true, STORE).is_ok(),
            "sealing did not restore the digests a flipped byte broke (seed {seed})"
        );
        checked += 1;
    }
    println!("ONBF sealer: {checked} images flipped and resealed");
    assert!(checked >= 10, "too few images to test sealing: {checked}");
}

#[test]
fn written_documents_validate_and_manifests_decode() {
    let (mut valid, mut decoded) = (0u64, 0u64);
    for seed in 0..RUNS {
        let data = fixed_bytes(seed, 2048);
        let mut u = Unstructured::new(&data);
        let mut doc = Vec::new();
        if crate::ocbor::item(&mut u, &mut doc, 0).is_ok() {
            valid += u64::from(ocbor::validate(&doc).is_ok());
            crate::ocbor::exercise(&doc);
        }
        let mut u = Unstructured::new(&data);
        if let Ok(fields) = crate::ocbor::KrnFields::arbitrary_from(&mut u)
            && let Ok(m) = crate::ocbor::krn_manifest(&mut u, &fields)
        {
            decoded += u64::from(KrnManifest::decode(&m).is_ok());
            crate::ocbor::exercise(&m);
        }
    }
    println!("OCBOR-1 writer: of {RUNS}, {valid} items validated and {decoded} manifests decoded");
    // The writer breaks about a third of its choices on purpose, and nesting compounds it.
    assert!(
        valid * 3 >= RUNS,
        "the OCBOR-1 writer's items are mostly refused: {valid} of {RUNS}"
    );
    assert!(
        decoded * 3 >= RUNS,
        "too few written manifests decode: {decoded} of {RUNS}"
    );
}

#[test]
fn written_signature_blocks_parse() {
    let mut parsed = 0u64;
    for seed in 0..RUNS {
        let data = fixed_bytes(seed, 16384);
        let mut u = Unstructured::new(&data);
        let Ok(block) = crate::signature::block(&mut u, &crate::signature::RAW_DIGEST, 6) else {
            continue;
        };
        parsed += u64::from(SignatureBlock::parse(&block).is_ok());
        crate::signature::raw(&block);
    }
    println!("Signature writer: of {RUNS} blocks, {parsed} parsed");
    assert!(
        parsed * 4 >= RUNS,
        "the signature writer's blocks are mostly refused: {parsed} of {RUNS}"
    );
}

#[test]
fn written_aml_loads() {
    let mut loaded = 0u64;
    for seed in 0..RUNS {
        let data = fixed_bytes(seed, 4096);
        let Ok(aml) = crate::aml::definition_block(&mut Unstructured::new(&data)) else {
            continue;
        };
        let config = Config {
            limits: Limits::BOOT_PROOF,
        };
        let mut i = Interpreter::new(FuzzHost::new(&data[..64]), config, IntegerWidth::Bits64)
            .expect("the interpreter starts");
        let block = DefinitionBlock {
            signature: *b"DSDT",
            oem_id: *b"ORIGIN",
            oem_table_id: *b"FUZZ    ",
            aml: &aml,
        };
        loaded += u64::from(i.load(block).is_ok());
        crate::aml::exercise(&aml, &data[..64], IntegerWidth::Bits64);
    }
    println!("AML writer: of {RUNS} blocks, {loaded} loaded");
    assert!(
        loaded * 4 >= RUNS,
        "the AML writer's blocks are mostly refused: {loaded} of {RUNS}"
    );
}
