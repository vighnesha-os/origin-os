//! `onbf-pack` command line (CLAUDE.md §70). Exit status: 0 on success, 1 on a conversion
//! error, 2 on a usage error. Nothing is written unless the whole conversion succeeds; the
//! output appears through a rename, so it is never partial.
//!
//! A thin client of the `onbf_pack` library (ADR-0011 Amendment 4, T1): it reads its arguments
//! and files, calls the library, and writes what the library returns. What it does of its own
//! is the host's part, which the library leaves to its caller: running `rustc -vV` when a
//! toolchain value is not given, reading the operating system's random generator, checking a
//! key file as it reads it (no symbolic link, a regular file, owner-only: no group or other
//! permission bit) and writing new key files (0600 and 0644, never over an existing path, all or
//! none), listing an output directory, and writing files atomically. It names no crate but
//! `onbf_pack` and `std` (`cargo xtask host-tools-gate`'s G3); that it holds no logic of its own
//! is review's to see.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{self, Command, ExitCode};

use onbf_pack::sign::{self, HostKey, SignOptions, Signer};
use onbf_pack::{KrnOptions, Random, RandomError, Toolchain, carried, corelib, krn};

const USAGE: &str = "\
usage: onbf-pack krn --input <kernel.elf> --output <kernel.onbf> [options]
       onbf-pack carried --input <program.elf> --name <name> --variable <VAR> --out <dir>
                         [--core-exports <list>]
       onbf-pack corelib --input <origin.core> --exports <list> --variable <VAR> --out <dir>
       onbf-pack sign --input <kernel.onbf> --output <signed.onbf> --key <file>
                      --signing-time <seconds>
       onbf-pack keygen --key <file> --public-ed25519 <file> --public-mldsa <file>
                        [--insecure-test-seed <text>]

krn converts the Framekernel's ELF64 x86-64 PIE (rust-lld output) into an unsigned KRN image
(docs/spec/ONBF.md §6.1.2, §6.5).

krn options:
  --cpu-features N     required CPU features beyond x86-64-v3, as a registry bitset
                       (decimal or 0x hex; bits 0-5 only; default 0)
  --flags STR          canonical compiler and linker flags; the manifest records their
                       SHA-256 (default: the empty string)
  --rustc-version V    rustc release        } each defaults to the matching line of
  --rustc-commit H     rustc commit hash    } `rustc -vV`, run in the current directory
  --llvm-version V     LLVM version         }

carried lays out a program the kernel carries in its image (ADR-0011 L1; ONBF §8.3's note):
`<name>.rs`, a static the kernel compiles in, and `<name>.<i>.bin` for each segment with bytes,
which the Rust file includes from the directory the environment variable <VAR> names when the
kernel is compiled. A file whose bytes are already there is left untouched, and an older
`<name>.*` file the description no longer has is removed.
  --core-exports LIST  the program is linked against `origin.core`, whose exports LIST names
                       (the generator's core/<library>.exports): each of its imports becomes
                       a slot the kernel's launch path binds (ADR-0011 L3). Without it the
                       program may import nothing.

corelib lays out `origin.core`, the library of system-call stubs the kernel carries and maps
into every process that is pinned per call (ADR-0011 L2 to L7): `origin_core.rs`, a static the
kernel compiles in, and `origin_core.0.bin`, its code. <list> is the generator's list of its
exports (`oidl generate` writes it as core/<library>.exports). The image is refused unless it
is the listed stubs and nothing else, with one `syscall` in each, no writable data and nothing
to relocate.

sign signs an unsigned KRN image in the role origin-kernel (ONBF §7.1-§7.2), with a key from
`keygen`, and checks the result with the boot loader's code.
  --signing-time N     the statement's claimed signing time, seconds since 1970-01-01 UTC
                       (decimal or 0x hex); informational only, and never taken from the clock

keygen creates a new key pair: the private key in a new file (mode 0600) and the raw public
keys in two new files. No existing file is ever overwritten.
  --insecure-test-seed TEXT
                       derive the keys from public text instead of the operating system's
                       random generator. The result is a TEST KEY: anyone can recompute it.

options:
  -h, --help           print this help";

/// How the run ended, with the exit status it maps to.
enum Failure {
    /// Exit 2.
    Usage(String),
    /// Exit 1.
    Conversion(String),
}

/// The options of one command, each given at most once.
#[derive(Default)]
struct Options(BTreeMap<String, OsString>);

impl Options {
    /// A required option.
    fn require(&mut self, name: &str) -> Result<OsString, Failure> {
        self.0
            .remove(name)
            .ok_or_else(|| Failure::Usage(format!("{name} is required")))
    }

    fn take(&mut self, name: &str) -> Option<OsString> {
        self.0.remove(name)
    }

    /// A UTF-8 option value.
    fn text(&mut self, name: &str) -> Result<Option<String>, Failure> {
        self.take(name)
            .map(|v| {
                v.into_string()
                    .map_err(|_| Failure::Usage(format!("the value of {name} is not UTF-8")))
            })
            .transpose()
    }

    /// A numeric option value.
    fn number(&mut self, name: &str) -> Result<Option<u64>, Failure> {
        self.text(name)?
            .map(|t| {
                parse_number(&t)
                    .ok_or_else(|| Failure::Usage(format!("{name}: `{t}` is not a number")))
            })
            .transpose()
    }
}

/// What each command accepts.
const KRN_OPTIONS: &[&str] = &[
    "--input",
    "--output",
    "--cpu-features",
    "--flags",
    "--rustc-version",
    "--rustc-commit",
    "--llvm-version",
];
const CARRIED_OPTIONS: &[&str] = &["--input", "--name", "--variable", "--out", "--core-exports"];
const CORELIB_OPTIONS: &[&str] = &["--input", "--exports", "--variable", "--out"];
const SIGN_OPTIONS: &[&str] = &["--input", "--output", "--key", "--signing-time"];
const KEYGEN_OPTIONS: &[&str] = &[
    "--key",
    "--public-ed25519",
    "--public-mldsa",
    "--insecure-test-seed",
];

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match run(&args) {
        Ok(None) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Some(summary)) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(Failure::Usage(message)) => {
            eprintln!("onbf-pack: {message}\n\n{USAGE}");
            ExitCode::from(2)
        }
        Err(Failure::Conversion(message)) => {
            eprintln!("onbf-pack: error: {message}");
            ExitCode::from(1)
        }
    }
}

/// Parses a decimal or `0x`-prefixed hexadecimal number.
fn parse_number(text: &str) -> Option<u64> {
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

/// Parses `--name value` pairs against `known`. Returns `None` for `-h` or `--help`.
fn parse_options(
    mut rest: core::slice::Iter<'_, OsString>,
    known: &[&str],
) -> Result<Option<Options>, Failure> {
    let usage = |m: String| Failure::Usage(m);
    let mut options = Options::default();
    while let Some(arg) = rest.next() {
        let name = arg
            .to_str()
            .ok_or_else(|| usage(format!("unexpected argument {arg:?}")))?;
        if matches!(name, "-h" | "--help") {
            return Ok(None);
        }
        if !known.contains(&name) {
            return Err(usage(format!("unknown option `{name}`")));
        }
        let value = rest
            .next()
            .ok_or_else(|| usage(format!("{name} needs a value")))?;
        if options.0.insert(name.to_string(), value.clone()).is_some() {
            return Err(usage(format!("{name} given twice")));
        }
    }
    Ok(Some(options))
}

/// Runs `rustc -vV` in the current directory, so that rustup picks the toolchain
/// `rust-toolchain.toml` pins, and reads its text with the library.
fn rustc_toolchain() -> Result<Toolchain, Failure> {
    let out = Command::new("rustc").arg("-vV").output().map_err(|e| {
        conversion(format!(
            "cannot run `rustc -vV` ({e}); pass --rustc-version, --rustc-commit and --llvm-version"
        ))
    })?;
    if !out.status.success() {
        return Err(conversion(format!("`rustc -vV` failed ({})", out.status)));
    }
    Toolchain::from_rustc_verbose_version(&String::from_utf8_lossy(&out.stdout)).map_err(conversion)
}

/// The toolchain fields: explicit values first, the rest from `rustc -vV`.
fn toolchain(options: &mut Options) -> Result<Toolchain, Failure> {
    let given = (
        options.text("--rustc-version")?,
        options.text("--rustc-commit")?,
        options.text("--llvm-version")?,
    );
    if let (Some(rustc), Some(rustc_commit), Some(llvm)) = given {
        return Ok(Toolchain {
            rustc,
            rustc_commit,
            llvm,
        });
    }
    let detected = rustc_toolchain()?;
    Ok(Toolchain {
        rustc: given.0.unwrap_or(detected.rustc),
        rustc_commit: given.1.unwrap_or(detected.rustc_commit),
        llvm: given.2.unwrap_or(detected.llvm),
    })
}

/// Writes `bytes` to a temporary file next to `path`, syncs it, and renames it into place. The
/// temporary file is made new (`create_new`): a stale one of this name is removed first, and a
/// link planted at its name is removed, never followed (the review of 2026-11-14, finding F9).
///
/// `xtask` has the same function (`developer-tools/xtask/src/main.rs`, `write_atomically`), and
/// the same `OsRandom`, `read_listing` and `write_description` (`src/main.rs`, `src/ring3.rs`):
/// the host's part, which `onbf_pack` leaves to each caller. Keep the copies in step (finding
/// F10).
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "the output has no file name")
    })?;
    let mut temp_name = OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".onbf-pack-{}.tmp", process::id()));
    let temp = path.with_file_name(temp_name);
    match fs::remove_file(&temp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let result = File::create_new(&temp)
        .and_then(|mut f| {
            f.write_all(bytes)?;
            f.sync_all()
        })
        .and_then(|()| fs::rename(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn run(args: &[OsString]) -> Result<Option<String>, Failure> {
    let usage = |m: String| Failure::Usage(m);
    let mut rest = args.iter();
    let command = match rest.next().and_then(|a| a.to_str()) {
        Some("-h" | "--help") => return Ok(None),
        Some(command @ ("krn" | "carried" | "corelib" | "sign" | "keygen")) => command,
        Some(other) => return Err(usage(format!("unknown command `{other}`"))),
        None => return Err(usage("no command given".into())),
    };
    let known = match command {
        "krn" => KRN_OPTIONS,
        "carried" => CARRIED_OPTIONS,
        "corelib" => CORELIB_OPTIONS,
        "sign" => SIGN_OPTIONS,
        _ => KEYGEN_OPTIONS,
    };
    let Some(options) = parse_options(rest, known)? else {
        return Ok(None);
    };
    match command {
        "krn" => pack_krn(options),
        "carried" => describe_carried(options),
        "corelib" => describe_corelib(options),
        "sign" => sign_krn(options),
        _ => keygen(options),
    }
    .map(Some)
}

fn conversion(message: impl std::fmt::Display) -> Failure {
    Failure::Conversion(message.to_string())
}

fn read_input(path: &Path) -> Result<Vec<u8>, Failure> {
    fs::read(path).map_err(|e| conversion(format!("cannot read {}: {e}", path.display())))
}

fn write_output(path: &Path, bytes: &[u8]) -> Result<(), Failure> {
    write_atomically(path, bytes)
        .map_err(|e| conversion(format!("cannot write {}: {e}", path.display())))
}

fn pack_krn(mut options: Options) -> Result<String, Failure> {
    let input = PathBuf::from(options.require("--input")?);
    let output = PathBuf::from(options.require("--output")?);
    let elf = read_input(&input)?;
    let cpu_features = options.number("--cpu-features")?.unwrap_or(0);
    let flags = options.text("--flags")?.unwrap_or_default();
    let packed = krn::pack(
        &elf,
        &KrnOptions {
            cpu_features,
            flags,
            toolchain: toolchain(&mut options)?,
        },
    )
    .map_err(conversion)?;
    write_output(&output, &packed.bytes)?;
    Ok(format!(
        "onbf-pack: wrote KRN {}: {} sections, {} segments, {} relocations, {} bytes",
        output.display(),
        packed.sections,
        packed.segments,
        packed.relocations,
        packed.bytes.len()
    ))
}

fn describe_carried(mut options: Options) -> Result<String, Failure> {
    let input = PathBuf::from(options.require("--input")?);
    let out = PathBuf::from(options.require("--out")?);
    let name = options
        .text("--name")?
        .ok_or_else(|| Failure::Usage("--name is required".into()))?;
    let variable = options
        .text("--variable")?
        .ok_or_else(|| Failure::Usage("--variable is required".into()))?;
    let exports = options.take("--core-exports").map(PathBuf::from);
    let elf = read_input(&input)?;
    let listing = match &exports {
        None => None,
        Some(exports) => Some(read_listing(exports)?),
    };
    let carried = carried::describe_program(&elf, listing.as_ref()).map_err(conversion)?;
    let files = carried.rust(&name, &variable).map_err(conversion)?;
    let written = write_description(&out, &name, &carried::generated_line(&name), &files)?;
    Ok(format!(
        "onbf-pack: described {name} in {}: {} segments, {} relocations, {} imports, {} files ({written} written)",
        out.display(),
        carried.segments.len(),
        carried.relocations.len(),
        carried.imports.len(),
        files.len()
    ))
}

/// The generator's list of `origin.core`'s exports, read from `path`.
fn read_listing(path: &Path) -> Result<corelib::Listing, Failure> {
    let list = String::from_utf8(read_input(path)?)
        .map_err(|_| conversion(format!("{} is not UTF-8", path.display())))?;
    corelib::Listing::parse(&list).map_err(|e| conversion(format!("{}: {e}", path.display())))
}

/// Writes `files`, a description of `name`, into `out`, and removes what an earlier
/// description of it left that this one no longer has. The library decides both from what is
/// read here of the directory (`carried::update`): only files this tool wrote for `name`, in a
/// directory whose `<name>.rs` begins with `first_line`, may go (S4-1's review, finding F9),
/// and a file whose bytes are already there is left untouched. Returns how many files were
/// written.
fn write_description(
    out: &Path,
    name: &str,
    first_line: &str,
    files: &[carried::File],
) -> Result<usize, Failure> {
    fs::create_dir_all(out)
        .map_err(|e| conversion(format!("cannot create {}: {e}", out.display())))?;
    // Only the files the description owns are read (`carried::owned`); one that cannot be read
    // is an error before anything is written (the review of 2026-11-14, finding F7).
    let mut present = BTreeMap::new();
    let entries =
        fs::read_dir(out).map_err(|e| conversion(format!("cannot read {}: {e}", out.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| conversion(format!("{}: {e}", out.display())))?;
        // A name that is not UTF-8 is not one this tool writes.
        let Ok(file) = entry.file_name().into_string() else {
            continue;
        };
        if !carried::owned(name, files, &file) {
            continue;
        }
        let path = entry.path();
        let bytes = fs::read(&path)
            .map_err(|e| conversion(format!("cannot read {}: {e}", path.display())))?;
        present.insert(file, bytes);
    }
    let update = carried::update(name, first_line, files, &present)
        .map_err(|e| conversion(format!("{}: {e}", out.display())))?;
    for (file, bytes) in &update.write {
        write_output(&out.join(file), bytes)?;
    }
    for file in &update.remove {
        fs::remove_file(out.join(file))
            .map_err(|e| conversion(format!("cannot remove {file}: {e}")))?;
    }
    Ok(update.write.len())
}

fn describe_corelib(mut options: Options) -> Result<String, Failure> {
    let input = PathBuf::from(options.require("--input")?);
    let exports = PathBuf::from(options.require("--exports")?);
    let out = PathBuf::from(options.require("--out")?);
    let variable = options
        .text("--variable")?
        .ok_or_else(|| Failure::Usage("--variable is required".into()))?;
    let listing = read_listing(&exports)?;
    let elf = read_input(&input)?;
    let core = corelib::describe(&elf, &listing).map_err(conversion)?;
    let files = core.rust(&variable).map_err(conversion)?;
    let written = write_description(&out, corelib::NAME, &corelib::generated_line(), &files)?;
    Ok(format!(
        "onbf-pack: described {} in {}: {} bytes of code in {} pages, {} exports, one `syscall` each, {} files ({written} written)",
        corelib::LIBRARY,
        out.display(),
        core.code.len(),
        core.span / 4096,
        core.exports.len(),
        files.len()
    ))
}

/// The operating system's random number generator: what the library signs and makes keys
/// with, from its caller.
struct OsRandom;

impl Random for OsRandom {
    fn fill(&mut self, out: &mut [u8]) -> Result<(), RandomError> {
        File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(out))
            .map_err(|e| RandomError(format!("the system random number generator failed: {e}")))
    }
}

/// `O_NOFOLLOW` of the build host, which `std` does not name: opening a symbolic link fails.
/// There is no value for any other host, so a build there stops here rather than following
/// links.
#[cfg(target_os = "macos")]
const O_NOFOLLOW: i32 = 0x0100;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const O_NOFOLLOW: i32 = 0o400_000;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const O_NOFOLLOW: i32 = 0o100_000;

/// Overwrites `bytes` with zeros, best effort: the compiler is not promised to keep the writes
/// (the library's `KeyFile` does the same).
fn wipe(bytes: &mut [u8]) {
    bytes.fill(0);
    std::hint::black_box(&*bytes);
}

/// Loads the key in the key file at `path`, refusing a symbolic link, anything but a regular
/// file, and one that anyone but its owner can read or write: like an SSH private key, it must
/// be owner-only, with no group or other permission bit (0600 as `keygen` writes it; 0400 and
/// 0700 pass too, and the message says what is checked, with 0600 as its example, since the
/// review of 2026-11-14, finding F13). Opened once and checked on that handle, so that what is
/// read is exactly what was checked; the library then refuses a damaged or foreign file.
fn load_key(path: &Path) -> Result<HostKey, Failure> {
    let cannot_read = |e| conversion(format!("cannot read {}: {e}", path.display()));
    let refused = |why: &str| conversion(format!("{}: {why}", path.display()));
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(path)
        .map_err(cannot_read)?;
    let metadata = file.metadata().map_err(cannot_read)?;
    if !metadata.is_file() {
        return Err(refused("not a regular file"));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(refused(
            "a key file must be readable or writable by its owner only (for example 0600)",
        ));
    }
    let mut bytes = Vec::new();
    let read = file.read_to_end(&mut bytes).map_err(cannot_read);
    let key = read.and_then(|_| {
        HostKey::from_key_file(&bytes, Box::new(OsRandom)).map_err(|e| refused(&e.to_string()))
    });
    wipe(&mut bytes);
    key
}

/// Writes a new file with `mode`, refusing to touch an existing path. If writing fails, the
/// partial file is removed.
fn write_new(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)?;
    let result = file.write_all(bytes).and_then(|()| file.sync_all());
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

/// Writes every file or none: a later failure removes the files already created. An existing
/// path, in particular a private key given by mistake as a public-key path, is refused.
fn write_all_new(files: &[(&Path, &[u8], u32)]) -> Result<(), Failure> {
    for (done, &(path, bytes, mode)) in files.iter().enumerate() {
        if let Err(e) = write_new(path, bytes, mode) {
            for &(created, _, _) in files.iter().take(done) {
                let _ = fs::remove_file(created);
            }
            return Err(conversion(if e.kind() == io::ErrorKind::AlreadyExists {
                format!("{} already exists", path.display())
            } else {
                format!("{}: {e}", path.display())
            }));
        }
    }
    Ok(())
}

fn sign_krn(mut options: Options) -> Result<String, Failure> {
    let input = PathBuf::from(options.require("--input")?);
    let output = PathBuf::from(options.require("--output")?);
    let key = PathBuf::from(options.require("--key")?);
    let claimed_signing_time = options
        .number("--signing-time")?
        .ok_or_else(|| Failure::Usage("--signing-time is required".into()))?;
    // Signing replaces the output through a rename, which would destroy the key.
    if output.exists() && fs::canonicalize(&output).ok() == fs::canonicalize(&key).ok() {
        return Err(Failure::Conversion(format!(
            "--output {} is the key file",
            output.display()
        )));
    }
    let image = read_input(&input)?;
    let signer = load_key(&key)?;
    let signed = sign::sign_krn(
        &image,
        &signer,
        &SignOptions {
            claimed_signing_time,
        },
    )
    .map_err(conversion)?;
    write_output(&output, &signed)?;
    Ok(format!(
        "onbf-pack: signed KRN {} in the role origin-kernel{}, key ID {}: {} bytes",
        output.display(),
        if signer.is_test_key() {
            " with a TEST KEY"
        } else {
            ""
        },
        sign::hex(&signer.public_keys().key_id()),
        signed.len()
    ))
}

fn keygen(mut options: Options) -> Result<String, Failure> {
    let key = PathBuf::from(options.require("--key")?);
    let public_ed25519 = PathBuf::from(options.require("--public-ed25519")?);
    let public_mldsa = PathBuf::from(options.require("--public-mldsa")?);
    let seed = options.text("--insecure-test-seed")?;
    let new = match &seed {
        Some(label) => sign::new_test_key(label),
        None => sign::new_key(&mut OsRandom).map_err(conversion)?,
    };
    write_all_new(&[
        (&key, new.key_file.as_bytes(), 0o600),
        (&public_ed25519, &new.public.ed25519, 0o644),
        (&public_mldsa, &new.public.mldsa87, 0o644),
    ])?;
    Ok(format!(
        "onbf-pack: wrote key pair{}: key ID {}",
        if seed.is_some() { " (TEST KEY)" } else { "" },
        sign::hex(&new.public.key_id())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The temporary file `write_atomically` writes is made new: a link planted at its name is
    /// removed, never followed, and a stale file there is replaced (the review of 2026-11-14,
    /// finding F9). `xtask` has the same test.
    #[test]
    fn writing_atomically_follows_no_link_at_the_temporary_name() {
        let dir = std::env::temp_dir().join(format!("onbf-pack-atomic-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let outside = dir.join("outside");
        fs::write(&outside, b"outside").unwrap();
        let path = dir.join("out.bin");
        let temporary = dir.join(format!(".out.bin.onbf-pack-{}.tmp", process::id()));
        std::os::unix::fs::symlink(&outside, &temporary).unwrap();
        write_atomically(&path, b"image").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"image");
        assert_eq!(fs::read(&outside).unwrap(), b"outside");
        assert!(fs::symlink_metadata(&temporary).is_err());
        fs::write(&temporary, b"stale").unwrap();
        write_atomically(&path, b"again").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"again");
        assert!(fs::symlink_metadata(&temporary).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
