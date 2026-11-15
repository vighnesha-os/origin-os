//! `oidl`: checks OIDL definitions and generates code from them (`docs/spec/OIDL.md`).
//!
//! ```text
//! oidl check [--versions FILE] FILE...
//! oidl generate --out DIR [--server-only | --library NAME] [--versions FILE] FILE...
//! oidl fingerprint [--library NAME] [--versions FILE] [--lock FILE [--update]] FILE...
//! ```
//!
//! Every `use`d library must be named on the command line: nothing is searched for, so an
//! output depends on exactly its inputs (CLAUDE.md §75). `--library` picks one of them: for
//! `generate`, the channel library whose bindings crate is written; for `fingerprint`, the one
//! library printed, and checked against or recorded in the lock.
//!
//! A thin client of the `oidl` library (ADR-0011 Amendment 4, T1): it reads its arguments and
//! files, calls the library, and writes and prints what the library returns. It decides nothing
//! the library does not: what each mode writes is `codegen::generate_output`, the narrowing to
//! one library `Compilation::only`, and the lock an update writes `fingerprint::updated_lock`.
#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use oidl::codegen::{self, Output};
use oidl::fingerprint;
use oidl::model::Compilation;
use oidl::{Input, compile};

const USAGE: &str = "usage:
  oidl check [--versions FILE] FILE...
  oidl generate --out DIR [--server-only | --library NAME] [--versions FILE] FILE...
  oidl fingerprint [--library NAME] [--versions FILE] [--lock FILE [--update]] FILE...";

#[derive(Default)]
struct Options {
    versions: Option<PathBuf>,
    out: Option<PathBuf>,
    lock: Option<PathBuf>,
    update: bool,
    server_only: bool,
    library: Option<String>,
    files: Vec<PathBuf>,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<(String, Options), String> {
    let mut args = args.peekable();
    let command = args.next().ok_or(USAGE)?;
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .ok_or_else(|| format!("{name} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--versions" => options.versions = Some(PathBuf::from(value("--versions")?)),
            "--out" => options.out = Some(PathBuf::from(value("--out")?)),
            "--lock" => options.lock = Some(PathBuf::from(value("--lock")?)),
            "--update" => options.update = true,
            "--server-only" => options.server_only = true,
            "--library" => options.library = Some(value("--library")?),
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other}\n{USAGE}"));
            }
            other => options.files.push(PathBuf::from(other)),
        }
    }
    if options.server_only && options.library.is_some() {
        return Err(format!(
            "--server-only and --library are different crates: give one\n{USAGE}"
        ));
    }
    Ok((command, options))
}

/// The compilation narrowed to the library `--library` names, for printing and the lock: the
/// others were given only so that it compiles.
fn selected(compilation: &Compilation, options: &Options) -> Result<Compilation, String> {
    match &options.library {
        None => Ok(compilation.clone()),
        Some(name) => compilation.only(name),
    }
}

fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn load(options: &Options) -> Result<Compilation, String> {
    let mut inputs = Vec::new();
    for path in &options.files {
        inputs.push(Input {
            file: path.display().to_string(),
            text: read_file(path)?,
        });
    }
    if inputs.is_empty() {
        return Err(format!("no definition files given\n{USAGE}"));
    }
    let versions = match &options.versions {
        Some(path) => Some(
            String::from_utf8(read_file(path)?)
                .map_err(|_| format!("{}: not UTF-8", path.display()))?,
        ),
        None => None,
    };
    compile(&inputs, versions.as_deref()).map_err(|e| e.to_string())
}

fn write_file(out: &Path, relative: &str, text: &str) -> Result<(), String> {
    let path = out.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn generate(options: &Options) -> Result<(), String> {
    let compilation = load(options)?;
    let out = options.out.as_ref().ok_or("generate needs --out DIR")?;
    let output = if options.server_only {
        Output::ServerOnly
    } else if let Some(name) = &options.library {
        Output::Library(name.clone())
    } else {
        Output::Everything
    };
    let files = codegen::generate_output(&compilation, &output)?;
    for file in &files {
        write_file(out, &file.path, &file.text)?;
    }
    println!("oidl: {} files written to {}", files.len(), out.display());
    Ok(())
}

fn fingerprint_command(options: &Options) -> Result<bool, String> {
    let compilation = selected(&load(options)?, options)?;
    for library in &compilation.libraries {
        print!("{}", fingerprint::fingerprint_file(library));
    }
    let Some(lock_path) = &options.lock else {
        return Ok(true);
    };
    let recorded = if lock_path.exists() {
        String::from_utf8(read_file(lock_path)?)
            .map_err(|_| format!("{}: not UTF-8", lock_path.display()))?
    } else if options.update {
        String::new()
    } else {
        return Err(format!(
            "{}: no such lock file (pass --update to create it)",
            lock_path.display()
        ));
    };
    let report = fingerprint::check_lock(&compilation, &recorded);
    for finding in &report.errors {
        eprintln!("oidl: error: {finding}");
    }
    for finding in &report.warnings {
        eprintln!("oidl: warning (major 0 promises nothing): {finding}");
    }
    for finding in &report.unrecorded {
        eprintln!("oidl: not recorded yet: {finding}");
    }
    if options.update {
        let lock = fingerprint::updated_lock(&compilation, &recorded).map_err(|_| {
            "refusing to update a lock whose recorded fingerprints changed".to_string()
        })?;
        fs::write(lock_path, lock).map_err(|e| format!("{}: {e}", lock_path.display()))?;
        return Ok(true);
    }
    Ok(report.errors.is_empty())
}

fn run() -> Result<bool, String> {
    let (command, options) = parse_args(std::env::args().skip(1))?;
    match command.as_str() {
        "check" => {
            let compilation = load(&options)?;
            println!("oidl: {} libraries valid", compilation.libraries.len());
            Ok(true)
        }
        "generate" => {
            generate(&options)?;
            Ok(true)
        }
        "fingerprint" => fingerprint_command(&options),
        other => Err(format!("unknown command {other}\n{USAGE}")),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(message) => {
            eprintln!("oidl: {message}");
            ExitCode::FAILURE
        }
    }
}
