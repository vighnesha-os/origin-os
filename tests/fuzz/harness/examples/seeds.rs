//! Writes each fuzz target's first seeds (TASKS item 144; the coordinating session's ruling
//! Q9) into `<directory>/<target>/`: `cargo xtask fuzz seeds` runs it.
//!
//! The seeds come from the harness's own writers, driven by a fixed generator, so the same
//! files come out on every run. For a target that reads raw bytes, a seed is a writer's output
//! that its parser accepts. For a target that writes its own input (`*_built`), a seed is the
//! generator's bytes the writer read, whole (`arbitrary` reads some lengths from the end), when
//! what it wrote was accepted. Each target gets at most [`PER_TARGET`] seeds, and none larger
//! than [`LARGEST`] bytes.

use arbitrary::Unstructured;
use onbf_format::signature::SignatureBlock;
use onbf_format::{Image, KrnManifest, ParseOptions};
use origin_aml::{Config, DefinitionBlock, IntegerWidth, Interpreter, Limits};
use origin_boot_uefi::{kernel, trust::STORE};
use origin_fuzz_harness::aml::{FuzzHost, RAW_PREFIX};
use origin_fuzz_harness::{aml, fixed_bytes, ocbor, onbf, signature};
use std::path::{Path, PathBuf};

/// Seeds per target.
const PER_TARGET: usize = 8;
/// The largest seed, in bytes.
const LARGEST: usize = 48 * 1024;
/// Generator starts tried per target.
const TRIES: u64 = 2000;
const DEVELOPER: ParseOptions = ParseOptions {
    nested_in_verified_parent: false,
    developer_mode: true,
};

/// The seeds of one target, gathered until there are enough.
struct Seeds {
    dir: PathBuf,
    written: usize,
}

impl Seeds {
    fn new(root: &Path, target: &str) -> Self {
        let dir = root.join(target);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).expect("the old seeds can be removed");
        }
        std::fs::create_dir_all(&dir).expect("the seed directory can be made");
        Self { dir, written: 0 }
    }

    fn full(&self) -> bool {
        self.written >= PER_TARGET
    }

    fn keep(&mut self, bytes: &[u8]) {
        if self.full() || bytes.is_empty() || bytes.len() > LARGEST {
            return;
        }
        let path = self.dir.join(format!("seed-{:02}", self.written));
        std::fs::write(&path, bytes).expect("a seed can be written");
        self.written += 1;
    }
}

fn aml_loads(aml: &[u8], answers: &[u8]) -> bool {
    let config = Config {
        limits: Limits::BOOT_PROOF,
    };
    let Ok(mut i) = Interpreter::new(FuzzHost::new(answers), config, IntegerWidth::Bits64) else {
        return false;
    };
    i.load(DefinitionBlock {
        signature: *b"DSDT",
        oem_id: *b"ORIGIN",
        oem_table_id: *b"FUZZ    ",
        aml,
    })
    .is_ok()
}

fn main() {
    let root = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: seeds <directory for the seeds>"),
    );
    let mut image = Seeds::new(&root, "onbf_image");
    let mut image_built = Seeds::new(&root, "onbf_image_built");
    let mut loader = Seeds::new(&root, "loader_place");
    let mut doc = Seeds::new(&root, "ocbor");
    let mut doc_built = Seeds::new(&root, "ocbor_built");
    let mut sig = Seeds::new(&root, "signature");
    let mut sig_built = Seeds::new(&root, "signature_built");
    let mut aml_raw = Seeds::new(&root, "aml");
    let mut aml_built = Seeds::new(&root, "aml_built");

    for seed in 0..TRIES {
        let data = fixed_bytes(seed, 8192);
        let mut u = Unstructured::new(&data);
        if let Ok(bytes) = onbf::image(&mut u) {
            if Image::parse(&bytes, DEVELOPER).is_ok() {
                image.keep(&bytes);
            }
            if kernel::verify(&bytes, true, STORE).is_ok() {
                loader.keep(&bytes);
                image_built.keep(&data);
            }
        }

        let data = fixed_bytes(seed, 2048);
        let mut u = Unstructured::new(&data);
        if let Ok(fields) = ocbor::KrnFields::arbitrary_from(&mut u)
            && let Ok(m) = ocbor::krn_manifest(&mut u, &fields)
            && KrnManifest::decode(&m).is_ok()
        {
            doc.keep(&m);
            // `ocbor_built` takes the manifest branch when its first choice says so: one byte
            // that `ratio(1, 2)` reads as true, then the bytes the manifest writer read.
            let mut input = vec![0u8];
            input.extend_from_slice(&data);
            doc_built.keep(&input);
        }

        let data = fixed_bytes(seed, 16384);
        let mut u = Unstructured::new(&data);
        if let Ok(block) = signature::block(&mut u, &signature::RAW_DIGEST, 6)
            && SignatureBlock::parse(&block).is_ok()
        {
            sig.keep(&block);
        }
        let mut u = Unstructured::new(&data);
        let built = (|| {
            let digest: [u8; 64] = u.arbitrary()?;
            let profile = if u.ratio(7, 8)? { 6 } else { u.arbitrary()? };
            signature::block(&mut u, &digest, profile)
        })();
        if built.is_ok_and(|b| SignatureBlock::parse(&b).is_ok()) {
            sig_built.keep(&data);
        }

        let data = fixed_bytes(seed, 4096);
        let mut u = Unstructured::new(&data);
        if let Ok(block) = aml::definition_block(&mut u)
            && aml_loads(&block, &data[..RAW_PREFIX])
        {
            let mut input = data[..RAW_PREFIX].to_vec();
            input.extend_from_slice(&block);
            aml_raw.keep(&input);
        }
        let mut u = Unstructured::new(&data);
        let built = (|| {
            let n = u.int_in_range(0..=512usize)?;
            let answers = u.bytes(n.min(u.len()))?.to_vec();
            Ok::<_, arbitrary::Error>((aml::definition_block(&mut u)?, answers))
        })();
        if built.is_ok_and(|(block, answers)| aml_loads(&block, &answers)) {
            aml_built.keep(&data);
        }
    }

    for s in [
        &image,
        &image_built,
        &loader,
        &doc,
        &doc_built,
        &sig,
        &sig_built,
        &aml_raw,
        &aml_built,
    ] {
        println!("{}: {} seeds", s.dir.display(), s.written);
    }
}
