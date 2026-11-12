//! The userspace build of CLAUDE.md §11, and the programs the kernel carries (task S4-1, TASKS
//! item 41; ADR-0011 Amendment 2, L1).
//!
//! - [`generate_stubs`] writes today's code for `origin.kernel` (`oidl generate`, full mode) under
//!   the target directory, for `runtime/kernel-stubs` to include: nothing generated is
//!   committed, so no committed crate holds a shim.
//! - Every ring-3 build names `--target x86_64-unknown-linux-gnu` and passes
//!   `.cargo/userspace.toml` with `--config`; nothing else reads that file.
//! - [`programs`] builds each carried program, has `onbf-pack carried` describe it, and returns
//!   the directory every kernel build names in [`CARRIED_ENV`].
//! - [`clippy`] runs clippy over the ring-3 crates on their own target.
//!
//! Generated files are rewritten only when their bytes change, so a build that changed nothing
//! leaves the stubs crate and the kernel as they were.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{OIDL_PACKAGE, PACK_PACKAGE, Result, cargo, read_file, run_step, target_dir};

/// The triple Origin's userspace uses as a code generator only (§11).
pub(crate) const USERSPACE_TARGET: &str = "x86_64-unknown-linux-gnu";
/// The userspace flags, passed with `--config` to the ring-3 builds alone.
pub(crate) const USERSPACE_CONFIG: &str = ".cargo/userspace.toml";
/// The variable `runtime/kernel-stubs` reads for the generated code's directory.
const STUBS_ENV: &str = "ORIGIN_KERNEL_STUBS";
/// The variable the kernel's `program.rs` reads for the carried programs' descriptions.
pub(crate) const CARRIED_ENV: &str = "ORIGIN_CARRIED_PROGRAMS";
/// The definition the stubs are generated from.
const KERNEL_DEFINITION: &str = "sdk/interfaces/origin.kernel.oidl";

/// Every ring-3 crate, for clippy.
pub(crate) const CRATES: &[&str] = &["origin-kernel-stubs", "origin-rt", "first-program"];

/// A program the kernel carries: its package and binary, and its name in the kernel.
struct Program {
    package: &'static str,
    name: &'static str,
}

/// Every program the kernel carries.
const PROGRAMS: &[Program] = &[Program {
    package: "first-program",
    name: "first_program",
}];

/// `ring3` under the target directory.
fn ring3_dir() -> PathBuf {
    target_dir().join("xtask").join("ring3")
}

/// Writes `bytes` to `path` unless it already holds exactly them.
fn write_if_changed(path: &Path, bytes: &[u8]) -> Result {
    if fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// The files under `dir`, as paths relative to it.
fn files_under(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    crate::collect_files(dir, &|_| false, &|_| true, &mut found)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(found
        .into_iter()
        .filter_map(|p| p.strip_prefix(dir).ok().map(Path::to_path_buf))
        .collect())
}

/// Generates `origin.kernel`'s code into a scratch directory, copies each file that changed
/// into place, and writes the `modules.rs` that `runtime/kernel-stubs` includes. Returns the
/// directory `ORIGIN_KERNEL_STUBS` names.
pub(crate) fn generate_stubs() -> Result<PathBuf> {
    let staging = ring3_dir().join("oidl-staging");
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    }
    let mut oidl = cargo();
    oidl.args([
        "run",
        "--quiet",
        "--package",
        OIDL_PACKAGE,
        "--",
        "generate",
    ])
    .arg("--out")
    .arg(&staging)
    .args(["--versions", "origin-version.toml", KERNEL_DEFINITION]);
    run_step(&mut oidl)?;
    let generated = staging.join("rust");
    let stubs = ring3_dir().join("oidl");
    let wanted = files_under(&generated)?;
    if wanted.is_empty() {
        return Err(format!(
            "oidl wrote no Rust under {}: nothing to build",
            generated.display()
        ));
    }
    for relative in &wanted {
        write_if_changed(
            &stubs.join(relative),
            &read_file(&generated.join(relative))?,
        )?;
    }
    if stubs.exists() {
        for relative in files_under(&stubs)? {
            if relative != Path::new("modules.rs") && !wanted.contains(&relative) {
                let gone = stubs.join(&relative);
                fs::remove_file(&gone).map_err(|e| format!("{}: {e}", gone.display()))?;
            }
        }
    }
    let path = |relative: &str| -> Result<String> {
        let file = stubs.join(relative);
        let text = file
            .to_str()
            .ok_or_else(|| format!("{} is not UTF-8", file.display()))?;
        if text.contains('"') || text.contains('\\') {
            return Err(format!(
                "{text}: a path the generated `#[path]` cannot quote"
            ));
        }
        Ok(text.to_string())
    };
    // Clippy finds two things in today's generated client and stubs, which only the generator
    // can change (task S4-2 changes what it writes): a `u64::from` of a `u64` in every stub
    // (`useless_conversion`) and one `Ok(x?)` in the client (`needless_question_mark`). Both
    // are allowed on these modules alone, and nothing else is.
    let modules = format!(
        "// Written by `cargo xtask` (developer-tools/xtask/src/ring3.rs). Do not edit.\n\
         #[rustfmt::skip]\n#[path = \"{}\"]\npub mod oidl_rt;\n\
         #[rustfmt::skip]\n#[allow(clippy::useless_conversion, clippy::needless_question_mark)]\n\
         #[path = \"{}\"]\npub mod origin_kernel;\n",
        path("oidl_rt.rs")?,
        path("origin_kernel/mod.rs")?,
    );
    write_if_changed(&stubs.join("modules.rs"), modules.as_bytes())?;
    Ok(stubs)
}

/// `cargo <subcommand>` for the ring-3 crates: the userspace triple, its configuration file, and
/// the generated stubs' directory.
fn userspace(subcommand: &str, stubs: &Path) -> Command {
    let mut command = cargo();
    command
        .arg(subcommand)
        .args(["--target", USERSPACE_TARGET, "--config", USERSPACE_CONFIG])
        .env(STUBS_ENV, stubs);
    command
}

/// Builds every carried program for ring 3 and describes each for the kernel. Returns the
/// directory the kernel's builds name in [`CARRIED_ENV`].
pub(crate) fn programs() -> Result<PathBuf> {
    let stubs = generate_stubs()?;
    let mut build = userspace("build", &stubs);
    build.arg("--release");
    for program in PROGRAMS {
        build.args(["--package", program.package]);
    }
    run_step(&mut build)?;
    let out = ring3_dir().join("carried");
    for program in PROGRAMS {
        let elf = target_dir()
            .join(USERSPACE_TARGET)
            .join("release")
            .join(program.package);
        if !elf.is_file() {
            return Err(format!(
                "cargo reported success but {} does not exist",
                elf.display()
            ));
        }
        let mut describe = cargo();
        describe
            .args([
                "run",
                "--quiet",
                "--release",
                "--package",
                PACK_PACKAGE,
                "--",
            ])
            .args(["carried", "--name", program.name, "--variable", CARRIED_ENV])
            .arg("--input")
            .arg(&elf)
            .arg("--out")
            .arg(&out);
        run_step(&mut describe)?;
    }
    Ok(out)
}

/// Gives `command`, a build or clippy of the kernel for its own target, the carried programs:
/// builds and describes them first.
pub(crate) fn with_programs(command: &mut Command) -> Result<&mut Command> {
    let carried = programs()?;
    Ok(command.env(CARRIED_ENV, carried))
}

/// Clippy over every ring-3 crate on the userspace triple, warnings denied.
pub(crate) fn clippy() -> Result {
    let stubs = generate_stubs()?;
    for package in CRATES {
        let mut clippy = userspace("clippy", &stubs);
        clippy.args(["--package", package, "--", "-D", "warnings"]);
        run_step(&mut clippy)?;
    }
    Ok(())
}
