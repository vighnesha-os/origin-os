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
#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use oidl::corpus;
use oidl::fingerprint::{self, LockFinding};
use oidl::model::Compilation;
use oidl::{Input, codegen, compile};

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
    let Some(name) = &options.library else {
        return Ok(compilation.clone());
    };
    let mut only = compilation.clone();
    only.libraries.retain(|library| &library.name == name);
    if only.libraries.is_empty() {
        return Err(format!("{name} is not among the definitions given"));
    }
    Ok(only)
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
    let mut count = 0usize;
    let one_crate = if options.server_only {
        Some(codegen::generate_server(&compilation))
    } else if let Some(name) = &options.library {
        Some(codegen::generate_library(&compilation, name)?)
    } else {
        None
    };
    if let Some(files) = one_crate {
        for file in files {
            write_file(out, &file.path, &file.text)?;
            count = count.saturating_add(1);
        }
        println!("oidl: {count} files written to {}", out.display());
        return Ok(());
    }
    for file in codegen::generate(&compilation) {
        write_file(out, &file.path, &file.text)?;
        count = count.saturating_add(1);
    }
    for library in &compilation.libraries {
        write_file(
            out,
            &format!("abi/{}.fingerprint", library.name),
            &fingerprint::fingerprint_file(library),
        )?;
        let text = corpus::corpus_for(library);
        write_file(out, &format!("corpus/{}.corpus", library.name), &text)?;
        count = count.saturating_add(2);
    }
    println!("oidl: {count} files written to {}", out.display());
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
        eprintln!("oidl: error: {}", describe_finding(finding));
    }
    for finding in &report.warnings {
        eprintln!(
            "oidl: warning (major 0 promises nothing): {}",
            describe_finding(finding)
        );
    }
    for finding in &report.unrecorded {
        eprintln!("oidl: not recorded yet: {}", describe_finding(finding));
    }
    if options.update {
        if !report.errors.is_empty() {
            return Err(
                "refusing to update a lock whose recorded fingerprints changed".to_string(),
            );
        }
        fs::write(lock_path, fingerprint::render_lock(&compilation))
            .map_err(|e| format!("{}: {e}", lock_path.display()))?;
        return Ok(true);
    }
    Ok(report.errors.is_empty())
}

fn describe_finding(finding: &LockFinding) -> String {
    match finding {
        LockFinding::Changed { library, version } => {
            format!("{library} {version}: the recorded fingerprint changed")
        }
        LockFinding::Gone { library, version } => {
            format!("{library} {version}: a recorded version is gone")
        }
        LockFinding::Unrecorded { library, version } => {
            format!("{library} {version}: no recorded fingerprint")
        }
        LockFinding::Malformed { line } => format!("the lock file's line {line} is malformed"),
    }
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
