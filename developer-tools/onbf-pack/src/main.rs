//! `onbf-pack` command line (CLAUDE.md §70). Exit status: 0 on success, 1 on a conversion
//! error, 2 on a usage error. Nothing is written unless the whole conversion succeeds; the
//! output appears through a rename, so it is never partial.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{self, ExitCode};

use onbf_pack::{KrnOptions, Toolchain, krn};

const USAGE: &str = "\
usage: onbf-pack krn --input <kernel.elf> --output <kernel.onbf> [options]

Converts the Framekernel's ELF64 x86-64 PIE (rust-lld output) into an unsigned KRN image
(docs/spec/ONBF.md §6.1.2, §6.5).

options:
  --cpu-features N     required CPU features beyond x86-64-v3, as a registry bitset
                       (decimal or 0x hex; bits 0-5 only; default 0)
  --flags STR          canonical compiler and linker flags; the manifest records their
                       SHA-256 (default: the empty string)
  --rustc-version V    rustc release        } each defaults to the matching line of
  --rustc-commit H     rustc commit hash    } `rustc -vV`, run in the current directory
  --llvm-version V     LLVM version         }
  -h, --help           print this help";

/// How the run ended, with the exit status it maps to.
enum Failure {
    /// Exit 2.
    Usage(String),
    /// Exit 1.
    Conversion(String),
}

#[derive(Default)]
struct Args {
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    cpu_features: Option<u64>,
    flags: Option<String>,
    rustc: Option<String>,
    rustc_commit: Option<String>,
    llvm: Option<String>,
}

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

fn parse_args(args: &[OsString]) -> Result<Option<Args>, Failure> {
    let usage = |m: String| Failure::Usage(m);
    let mut rest = args.iter();
    match rest.next().and_then(|a| a.to_str()) {
        Some("krn") => {}
        Some("-h" | "--help") => return Ok(None),
        Some(other) => return Err(usage(format!("unknown command `{other}`"))),
        None => return Err(usage("no command given".into())),
    }
    let mut parsed = Args::default();
    while let Some(arg) = rest.next() {
        let name = arg
            .to_str()
            .ok_or_else(|| usage(format!("unexpected argument {arg:?}")))?;
        if matches!(name, "-h" | "--help") {
            return Ok(None);
        }
        let value = rest
            .next()
            .ok_or_else(|| usage(format!("{name} needs a value")))?;
        let text = || {
            value
                .to_str()
                .map(str::to_string)
                .ok_or_else(|| usage(format!("the value of {name} is not UTF-8")))
        };
        let fresh = match name {
            "--input" => parsed.input.replace(PathBuf::from(value)).is_none(),
            "--output" => parsed.output.replace(PathBuf::from(value)).is_none(),
            "--cpu-features" => {
                let t = text()?;
                let n = parse_number(&t)
                    .ok_or_else(|| usage(format!("--cpu-features: `{t}` is not a number")))?;
                parsed.cpu_features.replace(n).is_none()
            }
            "--flags" => parsed.flags.replace(text()?).is_none(),
            "--rustc-version" => parsed.rustc.replace(text()?).is_none(),
            "--rustc-commit" => parsed.rustc_commit.replace(text()?).is_none(),
            "--llvm-version" => parsed.llvm.replace(text()?).is_none(),
            other => return Err(usage(format!("unknown option `{other}`"))),
        };
        if !fresh {
            return Err(usage(format!("{name} given twice")));
        }
    }
    if parsed.input.is_none() || parsed.output.is_none() {
        return Err(usage("--input and --output are required".into()));
    }
    Ok(Some(parsed))
}

/// The toolchain fields: explicit values first, the rest from `rustc -vV`.
fn toolchain(args: &mut Args) -> Result<Toolchain, Failure> {
    let given = (
        args.rustc.take(),
        args.rustc_commit.take(),
        args.llvm.take(),
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
    let Some(mut args) = parse_args(args)? else {
        return Ok(None);
    };
    let input = args.input.take().unwrap_or_default();
    let output = args.output.take().unwrap_or_default();
    let elf = fs::read(&input)
        .map_err(|e| Failure::Conversion(format!("cannot read {}: {e}", input.display())))?;
    let options = KrnOptions {
        cpu_features: args.cpu_features.unwrap_or(0),
        flags: args.flags.take().unwrap_or_default(),
        toolchain: toolchain(&mut args)?,
    };
    let packed = krn::pack(&elf, &options).map_err(|e| Failure::Conversion(e.to_string()))?;
    write_atomically(&output, &packed.bytes)
        .map_err(|e| Failure::Conversion(format!("cannot write {}: {e}", output.display())))?;
    Ok(Some(format!(
        "onbf-pack: wrote KRN {}: {} sections, {} segments, {} relocations, {} bytes",
        output.display(),
        packed.sections,
        packed.segments,
        packed.relocations,
        packed.bytes.len()
    )))
}
