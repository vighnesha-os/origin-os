//! End to end: compile a tiny `no_std`/`no_main` program for `x86_64-unknown-none` with the
//! pinned `rustc` and `rust-lld`, using the Framekernel's link flags, pack it, verify the
//! KRN image independently, and simulate the loader's relocation pass. Also runs the
//! `onbf-pack` binary.
//!
//! The program has a table of 70 pointers to one static and one pointer to another, so
//! the PIE carries RELATIVE relocations: in `DT_RELA` by default, or in `DT_RELR` with
//! `-z pack-relative-relocs`. A 6000-byte read-only array makes the R segment larger than
//! 4 KiB, so that segment gets a MERKLE section, and `POINTERS`, immutable but relocated, puts
//! data in `.data.rel.ro`, which the linker's `PT_GNU_RELRO` covers.

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
static POINTERS: [&u8; 2] = [&ONE, &TWO];

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
    // SAFETY: POINTERS is a valid, aligned static.
    core::hint::black_box(unsafe { core::ptr::read_volatile(&POINTERS) });
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
    "-zrelro",
    "-znow",
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

/// The release `rust-toolchain.toml` pins, read from the file. The pin is written there and in
/// the three places `verify.sh` holds to it (CLAUDE.md §8), and not a fifth time here.
fn pinned_channel() -> String {
    let file = manifest_dir().join("../../rust-toolchain.toml");
    let text = fs::read_to_string(&file).expect("rust-toolchain.toml is readable");
    text.lines()
        .find_map(|line| line.strip_prefix("channel = \"")?.strip_suffix('"'))
        .expect("rust-toolchain.toml names a channel")
        .to_string()
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
        .find(|s| s.entry.flags.has(Flags::WRITE) && !s.entry.flags.has(Flags::RELRO))
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
        toolchain.rustc,
        pinned_channel(),
        "the compiler rust-toolchain.toml pins built the probe"
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

/// CLAUDE.md §14, ADR-0001 default 9, on the real linker's output: `PT_GNU_RELRO` becomes a
/// RELRO segment of whole pages, the data after it is a plain RW segment, and the relocated
/// pointers of `.data.rel.ro` sit in the RELRO one. What the loader does with it is in
/// `boot/uefi/src/place.rs`.
#[test]
fn the_linkers_relro_becomes_a_relro_segment() {
    let dir = TempDir::new("relro-segment");
    let elf = compile(&dir, "kernel", KERNEL_LINK_FLAGS);
    let packed = krn::pack(&elf, &options(toolchain_of(&pinned_rustc()))).unwrap();
    let (segments, slots) = check_independently(&packed.bytes, "relro");

    let (r, w, x, relro) = (Flags::READ, Flags::WRITE, Flags::EXECUTE, Flags::RELRO);
    let permissions: Vec<u32> = segments
        .iter()
        .map(|s| s.entry.flags.0 & (r | w | x | relro))
        .collect();
    assert_eq!(permissions, [r, r | x, r | w | relro, r | w]);
    let (start, size) = (segments[2].entry.mem_offset, segments[2].entry.mem_size);
    assert_eq!(size % 4096, 0);
    assert_eq!(segments[3].entry.mem_offset, start + size);

    let in_relro: Vec<u64> = slots
        .iter()
        .copied()
        .filter(|s| (start..start + size).contains(s))
        .collect();
    assert!(in_relro.len() >= 2, "{} slots in RELRO", in_relro.len());
    let base = 0xFFFF_FFFF_8460_0000;
    let memory = load(&segments, &slots, base);
    for slot in in_relro {
        let at = slot as usize;
        let value = u64::from_le_bytes(memory[at..at + 8].try_into().unwrap());
        assert!(
            value >= base,
            "slot {slot:#x} was not relocated: {value:#x}"
        );
    }
}

#[test]
fn link_flags_that_the_contract_needs() {
    // On rust-lld 23.1.1, this flag changes the output of this program; without it the image
    // is rejected. The others match lld's or rustc's defaults here.
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
    // RELRO is what the linker writes: `-znorelro` turns it off, and the packer invents nothing.
    let mut norelro = KERNEL_LINK_FLAGS.to_vec();
    norelro.push("-znorelro");
    let elf = compile(&dir, "norelro", &norelro);
    let bytes = krn::pack(&elf, &options(toolchain.clone())).unwrap().bytes;
    let verified = krn::verify(&bytes).unwrap();
    assert!(
        verified
            .segments
            .iter()
            .all(|s| s.flags & Flags::RELRO == 0)
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
    assert!(stdout.contains(" sections, 4 segments, "), "{stdout}");
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

// ------------------------------------------------- the binary's key files (ADR-0011 Am. 4, T1)

/// Runs the binary with `args` and returns its exit code, standard output and standard error.
fn run(args: &[&std::ffi::OsStr]) -> (Option<i32>, String, String) {
    let out = onbf_pack().args(args).output().unwrap();
    (
        out.status.code(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

/// `keygen` into `dir`, under the names `<name>.key`, `<name>.ed25519` and `<name>.mldsa87`.
fn keygen(dir: &Path, name: &str, seed: Option<&str>) -> (Option<i32>, String, String) {
    let paths = [
        dir.join(format!("{name}.key")),
        dir.join(format!("{name}.ed25519")),
        dir.join(format!("{name}.mldsa87")),
    ];
    keygen_at(&paths, seed)
}

fn keygen_at(paths: &[PathBuf; 3], seed: Option<&str>) -> (Option<i32>, String, String) {
    let mut args: Vec<&std::ffi::OsStr> = vec![
        "keygen".as_ref(),
        "--key".as_ref(),
        paths[0].as_os_str(),
        "--public-ed25519".as_ref(),
        paths[1].as_os_str(),
        "--public-mldsa".as_ref(),
        paths[2].as_os_str(),
    ];
    if let Some(seed) = seed {
        args.extend([
            std::ffi::OsStr::new("--insecure-test-seed"),
            std::ffi::OsStr::new(seed),
        ]);
    }
    run(&args)
}

/// `sign` of `input` with the key file `key`, into `output`.
fn sign_with(input: &Path, key: &Path, output: &Path) -> (Option<i32>, String, String) {
    run(&[
        "sign".as_ref(),
        "--input".as_ref(),
        input.as_os_str(),
        "--output".as_ref(),
        output.as_os_str(),
        "--key".as_ref(),
        key.as_os_str(),
        "--signing-time".as_ref(),
        "1794000000".as_ref(),
    ])
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// The key files are the command line's (ADR-0011 Amendment 4, T1): it writes them, mode 0600
/// for the private key and 0644 for the public ones, never over an existing path and all or
/// none; it reads a key file only if it is no symbolic link, a regular file and owner-only (no
/// group or other permission bit); and
/// what it signs with such a key verifies under the public keys it wrote. These are the file
/// tests that `HostKey::load` and `generate_keys` had in the library before T1
/// (`src/tests/sign.rs`), run through the binary.
#[test]
fn command_line_keys() {
    use onbf_format::signature::Role;
    use onbf_verify::{PublicKey, TrustedKey, key_id, verify_krn_block};
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new("cli-keys");
    let elf = compile(&dir, "kernel", KERNEL_LINK_FLAGS);
    let unsigned = dir.0.join("kernel.onbf");
    fs::write(
        &unsigned,
        krn::pack(&elf, &options(toolchain_of(&pinned_rustc())))
            .unwrap()
            .bytes,
    )
    .unwrap();

    // A fresh key from the system's generator: the modes, and an end-to-end signature that
    // verifies under the public keys the command wrote.
    let (code, stdout, stderr) = keygen(&dir.0, "fresh", None);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.starts_with("onbf-pack: wrote key pair: key ID "),
        "{stdout}"
    );
    let key = dir.0.join("fresh.key");
    assert_eq!(fs::read(&key).unwrap().len(), 104);
    assert_eq!(&fs::read(&key).unwrap()[..8], b"ONBFKEY1");
    assert_eq!(mode(&key), 0o600);
    assert_eq!(mode(&dir.0.join("fresh.ed25519")), 0o644);
    assert_eq!(mode(&dir.0.join("fresh.mldsa87")), 0o644);
    let ed25519: [u8; 32] = fs::read(dir.0.join("fresh.ed25519"))
        .unwrap()
        .try_into()
        .unwrap();
    let mldsa = fs::read(dir.0.join("fresh.mldsa87")).unwrap();
    let public = PublicKey {
        ed25519: &ed25519,
        mldsa: &mldsa,
    };
    let id: String = key_id(&public).iter().map(|b| format!("{b:02x}")).collect();
    assert!(stdout.ends_with(&format!("key ID {id}\n")), "{stdout}");
    let signed = dir.0.join("signed.onbf");
    let (code, stdout, stderr) = sign_with(&unsigned, &key, &signed);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.starts_with("onbf-pack: signed KRN ")
            && stdout.contains(" in the role origin-kernel, key ID ")
            && !stdout.contains("TEST KEY"),
        "{stdout}"
    );
    let bytes = fs::read(&signed).unwrap();
    let image = Image::parse(&bytes, ParseOptions::default()).unwrap();
    let store = [TrustedKey {
        role: Role::Kernel,
        key: public,
    }];
    assert_eq!(
        verify_krn_block(
            image.signature_block().unwrap(),
            &store,
            &image.header().content_digest
        ),
        Ok(key_id(&store[0].key))
    );

    // A test seed: the key file says so, and its public halves are the ones committed in
    // `boot/uefi/trust` (from `test_keys_come_from_their_seed_only`).
    let label = "TEST-ONLY origin-kernel key 1: the test trust store holds it";
    let (code, stdout, stderr) = keygen(&dir.0, "trusted", Some(label));
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.starts_with("onbf-pack: wrote key pair (TEST KEY): key ID "),
        "{stdout}"
    );
    let committed = manifest_dir().join("../../boot/uefi/trust");
    for (written, name) in [
        ("trusted.ed25519", "TEST-ONLY-origin-kernel.ed25519"),
        ("trusted.mldsa87", "TEST-ONLY-origin-kernel.mldsa87"),
    ] {
        assert_eq!(
            fs::read(dir.0.join(written)).unwrap(),
            fs::read(committed.join(name)).unwrap()
        );
    }
    let test_key = dir.0.join("trusted.key");
    assert_eq!(&fs::read(&test_key).unwrap()[..8], b"ONBFTKEY");
    assert_eq!(mode(&test_key), 0o600);
    let (code, stdout, stderr) = sign_with(&unsigned, &test_key, &dir.0.join("test.onbf"));
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains(" with a TEST KEY, key ID "), "{stdout}");

    // keygen never overwrites anything (from `keygen_refuses_to_overwrite_anything`): an
    // existing file in each of the three positions is left alone, and nothing new is left.
    let existing = fs::read(&key).unwrap();
    for position in 0..3 {
        let mut paths = [
            dir.0.join("new.key"),
            dir.0.join("new.ed25519"),
            dir.0.join("new.mldsa87"),
        ];
        paths[position] = key.clone();
        let (code, _, stderr) = keygen_at(&paths, None);
        assert_eq!(code, Some(1), "position {position}");
        assert!(
            stderr.contains(&format!("{} already exists", key.display())),
            "{stderr}"
        );
        assert_eq!(fs::read(&key).unwrap(), existing);
        for path in &paths {
            assert!(*path == key || !path.exists(), "{}", path.display());
        }
    }
    // The same new path twice: the second write is refused and the first removed, so the
    // private key can never be replaced by a public key.
    let same = dir.0.join("same.key");
    let (code, _, stderr) = keygen_at(
        &[same.clone(), same.clone(), dir.0.join("same.mldsa87")],
        None,
    );
    assert_eq!(code, Some(1));
    assert!(stderr.contains("already exists"), "{stderr}");
    assert!(!same.exists() && !dir.0.join("same.mldsa87").exists());

    // The key file's refusals as a file (from `a_damaged_or_foreign_key_file_is_refused` and
    // `a_symbolic_link_to_a_key_file_is_refused`). Nothing is written for any of them.
    let out = dir.0.join("refused.onbf");
    let refused = |key: &Path, why: &str| {
        let (code, stdout, stderr) = sign_with(&unsigned, key, &out);
        assert_eq!(code, Some(1), "{why}: {stdout}");
        assert!(stderr.contains(why), "{why}: {stderr}");
        assert!(!out.exists(), "{why}");
    };
    // Missing.
    refused(&dir.0.join("missing.key"), "cannot read");
    // A symbolic link, even to a key file that is accepted itself: opened with O_NOFOLLOW.
    let link = dir.0.join("link.key");
    std::os::unix::fs::symlink(&key, &link).unwrap();
    refused(&link, "cannot read");
    // Not a regular file.
    refused(&dir.0, "not a regular file");
    // Readable by others.
    let open = dir.0.join("open.key");
    fs::copy(&key, &open).unwrap();
    fs::set_permissions(&open, fs::Permissions::from_mode(0o644)).unwrap();
    refused(
        &open,
        "a key file must be readable or writable by its owner only (for example 0600)",
    );
    fs::set_permissions(&open, fs::Permissions::from_mode(0o640)).unwrap();
    refused(
        &open,
        "a key file must be readable or writable by its owner only (for example 0600)",
    );
    // Its bytes, refused by the library, named with the file.
    let damaged = dir.0.join("damaged.key");
    let mut bytes = existing.clone();
    bytes[30] ^= 1;
    fs::write(&damaged, &bytes).unwrap();
    fs::set_permissions(&damaged, fs::Permissions::from_mode(0o600)).unwrap();
    refused(
        &damaged,
        &format!(
            "{}: damaged: its keys do not match its key ID",
            damaged.display()
        ),
    );
    // The output may not be the key file: signing replaces the output through a rename.
    let (code, _, stderr) = sign_with(&unsigned, &key, &key);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("is the key file"), "{stderr}");
    assert_eq!(fs::read(&key).unwrap(), existing);
    // The key file itself still loads.
    let (code, _, stderr) = sign_with(&unsigned, &key, &out);
    assert_eq!(code, Some(0), "{stderr}");
}
