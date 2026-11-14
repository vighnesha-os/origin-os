//! `onbf-pack` command line (CLAUDE.md §70). Exit status: 0 on success, 1 on a conversion
//! error, 2 on a usage error. Nothing is written unless the whole conversion succeeds; the
//! output appears through a rename, so it is never partial.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{self, ExitCode};

use onbf_pack::sign::{self, HostKey, KeyPaths, SignOptions, Signer};
use onbf_pack::{KrnOptions, Toolchain, carried, corelib, krn};

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
    let detected = Toolchain::from_rustc().map_err(|e| Failure::Conversion(e.to_string()))?;
    Ok(Toolchain {
        rustc: given.0.unwrap_or(detected.rustc),
        rustc_commit: given.1.unwrap_or(detected.rustc_commit),
        llvm: given.2.unwrap_or(detected.llvm),
    })
}

/// Writes `bytes` to a temporary file next to `path`, syncs it, and renames it into place.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "the output has no file name")
    })?;
    let mut temp_name = OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".onbf-pack-{}.tmp", process::id()));
    let temp = path.with_file_name(temp_name);
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
    let carried = match &exports {
        None => carried::describe(&elf),
        Some(exports) => {
            let list = String::from_utf8(read_input(exports)?)
                .map_err(|_| conversion(format!("{} is not UTF-8", exports.display())))?;
            let listing = corelib::Listing::parse(&list)
                .map_err(|e| conversion(format!("{}: {e}", exports.display())))?;
            carried::describe_importing(&elf, &listing)
        }
    }
    .map_err(conversion)?;
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

/// Writes `files`, a description of `name`, into `out`, and removes what an earlier
/// description of it left that this one no longer has. What may go is decided before anything
/// is written: only files this tool wrote for `name`, in a directory whose `<name>.rs` begins
/// with `first_line` (`carried::stale`; S4-1's review, finding F9). A file whose bytes are
/// already there is left untouched. Returns how many files were written.
fn write_description(
    out: &Path,
    name: &str,
    first_line: &str,
    files: &[carried::File],
) -> Result<usize, Failure> {
    fs::create_dir_all(out)
        .map_err(|e| conversion(format!("cannot create {}: {e}", out.display())))?;
    let mut present = Vec::new();
    let entries =
        fs::read_dir(out).map_err(|e| conversion(format!("cannot read {}: {e}", out.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| conversion(format!("{}: {e}", out.display())))?;
        // A name that is not UTF-8 is not one this tool writes.
        if let Ok(file) = entry.file_name().into_string() {
            present.push(file);
        }
    }
    let description = fs::read(out.join(format!("{name}.rs"))).ok();
    let stale = carried::stale(name, first_line, files, &present, description.as_deref())
        .map_err(|e| conversion(format!("{}: {e}", out.display())))?;
    let mut written = 0;
    for (file, bytes) in files {
        let path = out.join(file);
        if fs::read(&path).is_ok_and(|old| old == *bytes) {
            continue;
        }
        write_output(&path, bytes)?;
        written += 1;
    }
    for file in &stale {
        fs::remove_file(out.join(file))
            .map_err(|e| conversion(format!("cannot remove {file}: {e}")))?;
    }
    Ok(written)
}

fn describe_corelib(mut options: Options) -> Result<String, Failure> {
    let input = PathBuf::from(options.require("--input")?);
    let exports = PathBuf::from(options.require("--exports")?);
    let out = PathBuf::from(options.require("--out")?);
    let variable = options
        .text("--variable")?
        .ok_or_else(|| Failure::Usage("--variable is required".into()))?;
    let list = String::from_utf8(read_input(&exports)?)
        .map_err(|_| conversion(format!("{} is not UTF-8", exports.display())))?;
    let listing = corelib::Listing::parse(&list)
        .map_err(|e| conversion(format!("{}: {e}", exports.display())))?;
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
    let signer = HostKey::load(&key).map_err(conversion)?;
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
        sign::hex(&onbf_verify::key_id(&onbf_verify::PublicKey {
            ed25519: &signer.public_keys().ed25519,
            mldsa: &signer.public_keys().mldsa87,
        })),
        signed.len()
    ))
}

fn keygen(mut options: Options) -> Result<String, Failure> {
    let key = PathBuf::from(options.require("--key")?);
    let public_ed25519 = PathBuf::from(options.require("--public-ed25519")?);
    let public_mldsa = PathBuf::from(options.require("--public-mldsa")?);
    let seed = options.text("--insecure-test-seed")?;
    let keys = sign::generate_keys(
        &KeyPaths {
            key: &key,
            public_ed25519: &public_ed25519,
            public_mldsa87: &public_mldsa,
        },
        seed.as_deref(),
    )
    .map_err(conversion)?;
    Ok(format!(
        "onbf-pack: wrote key pair{}: key ID {}",
        if seed.is_some() { " (TEST KEY)" } else { "" },
        sign::hex(&onbf_verify::key_id(&onbf_verify::PublicKey {
            ed25519: &keys.ed25519,
            mldsa: &keys.mldsa87,
        }))
    ))
}
