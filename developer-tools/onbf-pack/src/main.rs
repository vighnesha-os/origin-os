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

use onbf_pack::sign::{self, KeyPaths, OpensslSigner, SignOptions, Signer};
use onbf_pack::{KrnOptions, Toolchain, krn};

const USAGE: &str = "\
usage: onbf-pack krn --input <kernel.elf> --output <kernel.onbf> [options]
       onbf-pack sign --input <kernel.onbf> --output <signed.onbf> --ed25519-key <key.pem>
                      --mldsa-key <key.pem> --signing-time <seconds> [--openssl <path>]
       onbf-pack keygen --ed25519-key <key.pem> --mldsa-key <key.pem>
                        --public-ed25519 <file> --public-mldsa <file>
                        [--insecure-test-seed <text>] [--openssl <path>]

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

sign signs an unsigned KRN image in the role origin-kernel, with Ed25519 and ML-DSA-87
private keys in PEM files (ONBF §7.1-§7.2), and checks the result with the boot loader's code.
OpenSSL 3.5 or later does the signing; this program never reads the private keys.
  --signing-time N     the statement's claimed signing time, seconds since 1970-01-01 UTC
                       (decimal or 0x hex); informational only, and never taken from the clock
  --openssl PATH       the OpenSSL program (default: `openssl` on PATH)

keygen creates a new key pair: the private keys in new PEM files (mode 0600) and the raw
public keys in two new files. No existing file is ever overwritten.
  --insecure-test-seed TEXT
                       derive the keys from public text instead of OpenSSL's random
                       generator. The result is a TEST KEY: anyone can recompute it.
  --openssl PATH       the OpenSSL program (default: `openssl` on PATH)

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

    /// The OpenSSL program to run.
    fn openssl(&mut self) -> PathBuf {
        self.take("--openssl")
            .map_or_else(|| PathBuf::from("openssl"), PathBuf::from)
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
const SIGN_OPTIONS: &[&str] = &[
    "--input",
    "--output",
    "--ed25519-key",
    "--mldsa-key",
    "--signing-time",
    "--openssl",
];
const KEYGEN_OPTIONS: &[&str] = &[
    "--ed25519-key",
    "--mldsa-key",
    "--public-ed25519",
    "--public-mldsa",
    "--insecure-test-seed",
    "--openssl",
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
        Some(command @ ("krn" | "sign" | "keygen")) => command,
        Some(other) => return Err(usage(format!("unknown command `{other}`"))),
        None => return Err(usage("no command given".into())),
    };
    let known = match command {
        "krn" => KRN_OPTIONS,
        "sign" => SIGN_OPTIONS,
        _ => KEYGEN_OPTIONS,
    };
    let Some(options) = parse_options(rest, known)? else {
        return Ok(None);
    };
    match command {
        "krn" => pack_krn(options),
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

fn sign_krn(mut options: Options) -> Result<String, Failure> {
    let input = PathBuf::from(options.require("--input")?);
    let output = PathBuf::from(options.require("--output")?);
    let ed25519_key = PathBuf::from(options.require("--ed25519-key")?);
    let mldsa_key = PathBuf::from(options.require("--mldsa-key")?);
    let claimed_signing_time = options
        .number("--signing-time")?
        .ok_or_else(|| Failure::Usage("--signing-time is required".into()))?;
    let openssl = options.openssl();
    let image = read_input(&input)?;
    let signer = OpensslSigner::new(&openssl, &ed25519_key, &mldsa_key).map_err(conversion)?;
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
        "onbf-pack: signed KRN {} in the role origin-kernel, key ID {}: {} bytes",
        output.display(),
        sign::hex(&onbf_verify::key_id(&onbf_verify::PublicKey {
            ed25519: &signer.public_keys().ed25519,
            mldsa: &signer.public_keys().mldsa87,
        })),
        signed.len()
    ))
}

fn keygen(mut options: Options) -> Result<String, Failure> {
    let ed25519_key = PathBuf::from(options.require("--ed25519-key")?);
    let mldsa_key = PathBuf::from(options.require("--mldsa-key")?);
    let public_ed25519 = PathBuf::from(options.require("--public-ed25519")?);
    let public_mldsa = PathBuf::from(options.require("--public-mldsa")?);
    let seed = options.text("--insecure-test-seed")?;
    let openssl = options.openssl();
    let keys = sign::generate_keys(
        &openssl,
        &KeyPaths {
            ed25519_key: &ed25519_key,
            mldsa87_key: &mldsa_key,
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
