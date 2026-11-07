//! End to end: compile a tiny `no_std`/`no_main` program for `x86_64-unknown-none` with the
//! pinned `rustc` and `rust-lld`, using the Framekernel's link flags, pack it, verify the
//! KRN image independently, and simulate the loader's relocation pass. Also runs the
//! `onbf-pack` binary.
//!
//! The program has a table of 70 pointers to one static and one pointer to another, so
//! the PIE carries RELATIVE relocations: in `DT_RELA` by default, or in `DT_RELR` with
//! `-z pack-relative-relocs`. A 6000-byte read-only array makes the R segment larger than
//! 4 KiB, so that segment gets a MERKLE section.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use onbf_format::{Flags, Image, Kind, KrnManifest, NO_MERKLE, ParseOptions, Section};
use onbf_merkle::Shape;
use onbf_pack::elf::ElfError;
use onbf_pack::{Error, KrnOptions, Toolchain, krn, relr};
use origin_crypto::{Sha256, Sha512};

const PROGRAM: &str = r#"
#![no_std]
#![no_main]

use core::panic::PanicInfo;

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

static ONE: u8 = 1;
static TWO: u8 = 2;
static BIG: [u8; 6000] = [0x5A; 6000];

#[repr(C)]
pub struct Table {
    magic: u64,
    entries: [&'static u8; 70],
    other: &'static u8,
}

#[unsafe(no_mangle)]
pub static mut TABLE: Table = Table {
    magic: 0x4F52_4947_494E_5442,
    entries: [&ONE; 70],
    other: &TWO,
};

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // SAFETY: single-threaded; a volatile read keeps TABLE and its relocations alive.
    let table = unsafe { core::ptr::read_volatile(&raw const TABLE) };
    core::hint::black_box(table);
    // SAFETY: BIG is a valid, aligned static.
    core::hint::black_box(unsafe { core::ptr::read_volatile(&BIG[5999]) });
    loop {
        core::hint::spin_loop();
    }
}
"#;

const MAGIC: u64 = 0x4F52_4947_494E_5442;

/// The Framekernel's link flags, as the coordinator reported them (2026-11-07).
const KERNEL_LINK_FLAGS: &[&str] = &[
    "--entry=_start",
    "--no-dynamic-linker",
    "-ztext",
    "-zseparate-loadable-segments",
    "-zmax-page-size=4096",
    "-zcommon-page-size=4096",
    "-znorelro",
    "-znoexecstack",
];

/// A scratch directory under `env::temp_dir()`, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("onbf-pack-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The pinned toolchain's own `rustc`. `rustc --print sysroot` runs in the package
/// directory, so rustup resolves `rust-toolchain.toml`; the compiler itself then runs
/// directly, from the scratch directory.
fn pinned_rustc() -> PathBuf {
    let out = Command::new("rustc")
        .args(["--print", "sysroot"])
        .current_dir(manifest_dir())
        .output()
        .expect("rustc runs");
    assert!(out.status.success());
    let sysroot = String::from_utf8(out.stdout).unwrap();
    Path::new(sysroot.trim()).join("bin").join("rustc")
}

fn toolchain_of(rustc: &Path) -> Toolchain {
    let out = Command::new(rustc).arg("-vV").output().unwrap();
    Toolchain::from_rustc_verbose_version(&String::from_utf8(out.stdout).unwrap()).unwrap()
}

/// Compiles [`PROGRAM`] with `link_flags` and returns the ELF bytes.
fn compile(dir: &TempDir, name: &str, link_flags: &[&str]) -> Vec<u8> {
    let source = dir.0.join("probe.rs");
    fs::write(&source, PROGRAM).unwrap();
    let output = dir.0.join(format!("{name}.elf"));
    let mut cmd = Command::new(pinned_rustc());
    cmd.current_dir(&dir.0)
        .args([
            "--edition",
            "2024",
            "--crate-name",
            "probe",
            "--crate-type",
            "bin",
        ])
        .args(["--target", "x86_64-unknown-none"])
        .args([
            "-C",
            "opt-level=2",
            "-C",
            "panic=abort",
            "-C",
            "codegen-units=1",
        ])
        .args(["-C", "force-frame-pointers=yes"]);
    for flag in link_flags {
        cmd.arg("-C").arg(format!("link-arg={flag}"));
    }
    let out = cmd.arg("-o").arg(&output).arg(&source).output().unwrap();
    assert!(
        out.status.success(),
        "rustc failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    fs::read(&output).unwrap()
}

fn options(toolchain: Toolchain) -> KrnOptions {
    KrnOptions {
        cpu_features: 0,
        flags: KERNEL_LINK_FLAGS.join(" "),
        toolchain,
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Manifest => "MANIFEST",
        Kind::Segment => "SEGMENT",
        Kind::Relocs => "RELOCS",
        Kind::Merkle => "MERKLE",
        _ => "other",
    }
}

/// Verifies `bytes` with `onbf-format`, `onbf-merkle` and the RELR decoder, prints the
/// layout, and returns the segments and the relocation slots.
fn check_independently<'a>(bytes: &'a [u8], label: &str) -> (Vec<Section<'a>>, Vec<u64>) {
    let options = ParseOptions {
        developer_mode: true,
        ..ParseOptions::default()
    };
    let image = Image::parse(bytes, options).unwrap();
    image
        .verify_content_digest(|parts| {
            let mut h = Sha512::new();
            for p in parts {
                h.update(p);
            }
            h.finalize()
        })
        .unwrap();
    let m = image.manifest().unwrap();
    m.verify_sha256(Sha256::digest).unwrap();
    let manifest = KrnManifest::decode(m.stored).unwrap();
    manifest.check_against(&image).unwrap();

    eprintln!("{label}: {} bytes", bytes.len());
    eprintln!(
        "  idx kind      file_offset  stored_size  mem_offset  mem_size  flags  integrity merkle"
    );
    for s in image.sections() {
        let e = &s.entry;
        let merkle = if e.merkle_index == NO_MERKLE {
            "-".to_string()
        } else {
            e.merkle_index.to_string()
        };
        eprintln!(
            "  {:>3} {:<9} {:>#11x} {:>#12x} {:>#11x} {:>#9x} {:>#6x}  {:>9?} {merkle}",
            s.index,
            kind_name(e.kind()),
            e.file_offset,
            e.stored_size,
            e.mem_offset,
            e.mem_size,
            e.flags.0,
            e.integrity
        );
        assert!(e.flags.has(Flags::REQUIRED));
    }

    let segments: Vec<Section<'_>> = image
        .sections()
        .filter(|s| s.entry.kind() == Kind::Segment)
        .collect();
    let mut build_id = Sha256::new();
    for s in &segments {
        assert_eq!(onbf_merkle::root(s.stored).unwrap(), s.entry.digest);
        let shape = Shape::new(s.entry.content_size).unwrap();
        if shape.hash_blocks() > 0 {
            let mut blocks = vec![0u8; shape.hash_blocks_len() as usize];
            onbf_merkle::build(s.stored, &mut blocks).unwrap();
            let merkle = image.section(s.entry.merkle_index as usize).unwrap();
            assert_eq!(merkle.stored, &blocks[..]);
        } else {
            assert_eq!(s.entry.merkle_index, NO_MERKLE);
        }
        build_id.update(s.stored);
    }
    assert_eq!(build_id.finalize(), manifest.build_id);

    let relocs = image
        .sections()
        .find(|s| s.entry.kind() == Kind::Relocs)
        .expect("the program has relocations");
    relocs.verify_sha256(Sha256::digest).unwrap();
    let words = relr::words_from_bytes(relocs.stored).unwrap();
    let slots = relr::decode(&words).unwrap();
    eprintln!("  RELOCS: {} slots in {} words", slots.len(), words.len());
    (segments, slots)
}

/// Maps the segments at `base` and applies `*(u64 *)(base + rva) += base` at every slot,
/// as the boot loader will; returns the memory image.
fn load(segments: &[Section<'_>], slots: &[u64], base: u64) -> Vec<u8> {
    let span = segments
        .iter()
        .map(|s| s.entry.mem_offset + s.entry.mem_size)
        .max()
        .unwrap();
    let mut memory = vec![0u8; span as usize];
    for s in segments {
        let at = s.entry.mem_offset as usize;
        memory[at..at + s.stored.len()].copy_from_slice(s.stored);
    }
    for &slot in slots {
        let at = slot as usize;
        let value = u64::from_le_bytes(memory[at..at + 8].try_into().unwrap());
        memory[at..at + 8].copy_from_slice(&value.wrapping_add(base).to_le_bytes());
    }
    memory
}

/// Finds TABLE in the relocated image and checks that every pointer in it, minus the base,
/// points at the right static.
fn check_table(memory: &[u8], segments: &[Section<'_>], base: u64) {
    let word = |at: usize| u64::from_le_bytes(memory[at..at + 8].try_into().unwrap());
    let rw = segments
        .iter()
        .find(|s| s.entry.flags.has(Flags::WRITE))
        .unwrap();
    let start = rw.entry.mem_offset as usize;
    let table = (start..start + rw.stored.len())
        .step_by(8)
        .find(|&at| word(at) == MAGIC)
        .expect("TABLE is in the RW segment");
    let deref = |at: usize| memory[(word(at).wrapping_sub(base)) as usize];
    for i in 0..70 {
        assert_eq!(deref(table + 8 + 8 * i), 1, "entry {i}");
    }
    assert_eq!(deref(table + 8 + 8 * 70), 2, "other");
}

fn pack_compile_and_check(link_flags: &[&str], name: &str) -> (Vec<u8>, usize) {
    let dir = TempDir::new(name);
    let elf = compile(&dir, name, link_flags);
    let toolchain = toolchain_of(&pinned_rustc());
    assert_eq!(
        toolchain.rustc, "1.99.1",
        "the pinned compiler built the probe"
    );
    let packed = krn::pack(&elf, &options(toolchain.clone())).unwrap();
    // Determinism: a second run gives the same bytes.
    assert_eq!(
        krn::pack(&elf, &options(toolchain)).unwrap().bytes,
        packed.bytes
    );
    let (segments, slots) = check_independently(&packed.bytes, name);
    assert!(slots.len() >= 71, "{} slots", slots.len());
    // BIG makes the R segment longer than one page, so it has a MERKLE section.
    let big = segments
        .iter()
        .find(|s| s.entry.content_size > 4096)
        .unwrap();
    assert_ne!(big.entry.merkle_index, NO_MERKLE);
    assert!(
        big.stored
            .windows(6000)
            .any(|w| w.iter().all(|&b| b == 0x5A))
    );
    for base in [0x20_0000u64, 0xFFFF_FFFF_8000_0000] {
        let memory = load(&segments, &slots, base);
        check_table(&memory, &segments, base);
    }
    (packed.bytes, slots.len())
}

#[test]
fn rela_kernel_packs_and_relocates() {
    let (bytes, slots) = pack_compile_and_check(KERNEL_LINK_FLAGS, "rela");
    let v = krn::verify(&bytes).unwrap();
    assert_eq!(v.relocations.len(), slots);
    assert!(v.shadow_stack);
}

#[test]
fn relr_kernel_packs_and_relocates() {
    let mut flags = KERNEL_LINK_FLAGS.to_vec();
    flags.push("-zpack-relative-relocs");
    pack_compile_and_check(&flags, "relr");
}

#[test]
fn link_flags_that_the_contract_needs() {
    // On rust-lld 23.1.1, these two flags change the output of this program; without
    // them the image is rejected. The others match lld's or rustc's defaults here.
    let dir = TempDir::new("flags");
    let toolchain = toolchain_of(&pinned_rustc());
    let without = |dropped: &str| -> Vec<&str> {
        KERNEL_LINK_FLAGS
            .iter()
            .copied()
            .filter(|f| *f != dropped)
            .collect()
    };
    let elf = compile(
        &dir,
        "shared-pages",
        &without("-zseparate-loadable-segments"),
    );
    assert_eq!(
        krn::pack(&elf, &options(toolchain.clone())),
        Err(Error::Elf(ElfError::SegmentAlignment { header: 2 }))
    );
    let elf = compile(&dir, "relro", &without("-znorelro"));
    assert_eq!(
        krn::pack(&elf, &options(toolchain.clone())),
        Err(Error::Elf(ElfError::GnuRelro))
    );
    // rustc's default link line alone is rejected too.
    let elf = compile(&dir, "default", &[]);
    assert!(krn::pack(&elf, &options(toolchain)).is_err());
}

// ------------------------------------------------------------- the binary

fn onbf_pack() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_onbf-pack"));
    cmd.current_dir(manifest_dir());
    cmd
}

#[test]
fn command_line() {
    let dir = TempDir::new("cli");
    let elf = compile(&dir, "kernel", KERNEL_LINK_FLAGS);
    let input = dir.0.join("kernel.elf");
    let output = dir.0.join("kernel.onbf");
    let t = toolchain_of(&pinned_rustc());

    // Explicit toolchain: the output equals the library's.
    let out = onbf_pack()
        .args(["krn", "--input"])
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .args(["--flags", "f", "--cpu-features", "0x8"])
        .args([
            "--rustc-version",
            &t.rustc,
            "--rustc-commit",
            &t.rustc_commit,
        ])
        .args(["--llvm-version", &t.llvm])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1);
    assert!(stdout.starts_with("onbf-pack: wrote KRN "), "{stdout}");
    assert!(stdout.contains(" sections, 3 segments, "), "{stdout}");
    let expected = krn::pack(
        &elf,
        &KrnOptions {
            cpu_features: 0x8,
            flags: "f".into(),
            toolchain: t.clone(),
        },
    )
    .unwrap();
    assert_eq!(fs::read(&output).unwrap(), expected.bytes);
    assert!(stdout.ends_with(&format!(" {} bytes\n", expected.bytes.len())));

    // Default toolchain: `rustc -vV` from the package directory, the pinned one.
    fs::remove_file(&output).unwrap();
    let status = onbf_pack()
        .args(["krn", "--input"])
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .status()
        .unwrap();
    assert!(status.success());
    let v = krn::verify(&fs::read(&output).unwrap()).unwrap();
    assert_eq!(v.toolchain, t);
    assert_eq!(v.flags_digest, Sha256::digest(b""));

    // Help.
    assert_eq!(onbf_pack().arg("--help").status().unwrap().code(), Some(0));

    // Usage errors: exit 2, nothing written.
    let missing = dir.0.join("never.onbf");
    let input_s = input.to_str().unwrap();
    let missing_s = missing.to_str().unwrap();
    let usage_cases: &[&[&str]] = &[
        &[],
        &["pack"],
        &["krn", "--input", input_s],
        &[
            "krn", "--input", input_s, "--output", missing_s, "--bogus", "1",
        ],
        &[
            "krn", "--input", input_s, "--input", input_s, "--output", missing_s,
        ],
        &[
            "krn",
            "--input",
            input_s,
            "--output",
            missing_s,
            "--cpu-features",
            "zz",
        ],
        &["krn", "--input", input_s, "--output", missing_s, "--flags"],
    ];
    for args in usage_cases {
        let out = onbf_pack().args(*args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(!missing.exists());
    }

    // Conversion errors: exit 1 with the typed reason, nothing written.
    let not_elf = dir.0.join("not.elf");
    fs::write(&not_elf, b"#!/bin/sh\n").unwrap();
    let conversion_cases: [(&Path, &[&str], &str); 3] = [
        (&not_elf, &[], "not an ELF file"),
        (&input, &["--cpu-features", "0x40"], "not in the registry"),
        (&dir.0.join("absent.elf"), &[], "cannot read"),
    ];
    for (source, extra, reason) in conversion_cases {
        let out = onbf_pack()
            .args(["krn", "--input"])
            .arg(source)
            .arg("--output")
            .arg(&missing)
            .args(extra)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{reason}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(reason), "{stderr}");
        assert!(!missing.exists());
    }
    // No temporary file is left behind either.
    let leftovers: Vec<_> = fs::read_dir(&dir.0)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".onbf-pack-"))
        .collect();
    assert!(leftovers.is_empty());
}
