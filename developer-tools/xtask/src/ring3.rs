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
//! - [`carried_files`] lists what one build wrote for each carried program, for the
//!   reproducible-build gate to read (TASKS item 32).
//!
//! Each of these takes the target directory to build into, or `None` for Cargo's own: the gate
//! builds the programs from nothing in two directories of its own, and each directory's kernel
//! carries that directory's programs.
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
pub(crate) const CRATES: &[&str] = &[
    "origin-kernel-stubs",
    "origin-rt",
    "origin-rt-mem",
    "first-program",
];
/// The bodies of the runtime's `mem*` functions: a ring-3 crate the host tests too (S4-1's
/// review, finding F6).
pub(crate) const MEM_PACKAGE: &str = "origin-rt-mem";

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

/// The package of each program the kernel carries.
pub(crate) fn carried_packages() -> impl Iterator<Item = &'static str> {
    PROGRAMS.iter().map(|program| program.package)
}

/// `target`, or Cargo's own target directory.
fn base_dir(target: Option<&Path>) -> PathBuf {
    target.map_or_else(target_dir, Path::to_path_buf)
}

/// `ring3` under the target directory.
fn ring3_dir(target: Option<&Path>) -> PathBuf {
    base_dir(target).join("xtask").join("ring3")
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
pub(crate) fn generate_stubs(target: Option<&Path>) -> Result<PathBuf> {
    let staging = ring3_dir(target).join("oidl-staging");
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
    let stubs = ring3_dir(target).join("oidl");
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

/// `cargo <subcommand>` for the ring-3 crates: the userspace triple, its configuration file, the
/// remap that keeps the toolchain's path out of what it builds (`toolchain`, TASKS item 32),
/// and the generated stubs' directory.
fn userspace(subcommand: &str, stubs: &Path, target: Option<&Path>) -> Result<Command> {
    let mut command = cargo();
    command
        .arg(subcommand)
        .args(["--target", USERSPACE_TARGET, "--config", USERSPACE_CONFIG])
        .args(crate::toolchain::userspace_config()?)
        .env(STUBS_ENV, stubs);
    if let Some(target) = target {
        command.arg("--target-dir").arg(target);
    }
    Ok(command)
}

/// Builds every carried program for ring 3 and describes each for the kernel. Returns the
/// directory the kernel's builds name in [`CARRIED_ENV`].
pub(crate) fn programs(target: Option<&Path>) -> Result<PathBuf> {
    let stubs = generate_stubs(target)?;
    let mut build = userspace("build", &stubs, target)?;
    build.arg("--release");
    for program in PROGRAMS {
        build.args(["--package", program.package]);
    }
    // Each is an ELF the kernel takes a segment of with its header in it, so each must have
    // been linked with the linker's fixed line: one from before it is linked again.
    let linked_files: Vec<PathBuf> = PROGRAMS.iter().map(|p| linked(target, p)).collect();
    crate::image_inputs::build_with_the_fixed_line(&linked_files, || run_step(&mut build))?;
    let out = ring3_dir(target).join("carried");
    for program in PROGRAMS {
        let elf = linked(target, program);
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
pub(crate) fn with_programs<'a>(
    command: &'a mut Command,
    target: Option<&Path>,
) -> Result<&'a mut Command> {
    let carried = programs(target)?;
    Ok(command.env(CARRIED_ENV, carried))
}

/// Where the linker writes `program`.
fn linked(target: Option<&Path>, program: &Program) -> PathBuf {
    base_dir(target)
        .join(USERSPACE_TARGET)
        .join("release")
        .join(program.package)
}

/// What a file of a carried program is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CarriedKind {
    /// The linker's output, which `onbf-pack carried` reads.
    Linked,
    /// A segment's bytes as `onbf-pack carried` wrote them: the kernel includes the file, so
    /// its image holds these bytes as they are.
    Segment,
    /// The description `onbf-pack carried` wrote, which the kernel compiles as source.
    Description,
}

/// A file of a carried program, as one build wrote it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CarriedFile {
    /// The program's name in the kernel.
    pub(crate) program: &'static str,
    pub(crate) kind: CarriedKind,
    /// The file's own name.
    pub(crate) file: String,
    pub(crate) path: PathBuf,
}

/// Every file of every carried program built into `target`: the linker's output, then what
/// `onbf-pack carried` wrote for it, by name. [`programs`] must have run for `target`. A
/// program with no segment file is an error: there would be nothing of it to read.
pub(crate) fn carried_files(target: Option<&Path>) -> Result<Vec<CarriedFile>> {
    let carried = ring3_dir(target).join("carried");
    let mut files = Vec::new();
    for program in PROGRAMS {
        let path = linked(target, program);
        files.push(CarriedFile {
            program: program.name,
            kind: CarriedKind::Linked,
            file: program.package.to_string(),
            path,
        });
        let described = described_files(&carried, program.name)?;
        if !described
            .iter()
            .any(|(_, kind)| *kind == CarriedKind::Segment)
        {
            return Err(format!(
                "{} holds no segment of `{}`: nothing of the program to read",
                carried.display(),
                program.name
            ));
        }
        for (file, kind) in described {
            files.push(CarriedFile {
                program: program.name,
                kind,
                path: carried.join(&file),
                file,
            });
        }
    }
    Ok(files)
}

/// The files in `carried` that `onbf-pack carried` wrote for the program `name`, which are
/// the ones named `<name>.<something>`, in the order of their names. A `.bin` is a segment.
fn described_files(carried: &Path, name: &str) -> Result<Vec<(String, CarriedKind)>> {
    let prefix = format!("{name}.");
    let mut found = Vec::new();
    let entries = fs::read_dir(carried).map_err(|e| format!("{}: {e}", carried.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", carried.display()))?;
        let file = entry.file_name().to_string_lossy().into_owned();
        if file.starts_with(&prefix) {
            let kind = if file.ends_with(".bin") {
                CarriedKind::Segment
            } else {
                CarriedKind::Description
            };
            found.push((file, kind));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(found)
}

/// Clippy over every ring-3 crate on the userspace triple, warnings denied.
pub(crate) fn clippy() -> Result {
    let stubs = generate_stubs(None)?;
    for package in CRATES {
        let mut clippy = userspace("clippy", &stubs, None)?;
        clippy.args(["--package", package, "--", "-D", "warnings"]);
        run_step(&mut clippy)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_programs_files_are_the_ones_named_for_it_and_a_bin_is_a_segment() {
        let dir = target_dir()
            .join("xtask")
            .join("ring3-tests")
            .join("described");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a scratch directory");
        for name in [
            "first_program.rs",
            "first_program.10.bin",
            "first_program.0.bin",
            "first_program_two.0.bin",
            "other.0.bin",
            "first_program",
        ] {
            fs::write(dir.join(name), b"x").expect("a scratch file");
        }
        assert_eq!(
            described_files(&dir, "first_program"),
            Ok(vec![
                ("first_program.0.bin".to_string(), CarriedKind::Segment),
                ("first_program.10.bin".to_string(), CarriedKind::Segment),
                ("first_program.rs".to_string(), CarriedKind::Description),
            ])
        );
        assert_eq!(described_files(&dir, "absent"), Ok(Vec::new()));
        // A directory that is not there is an error, not an empty list.
        assert!(described_files(&dir.join("missing"), "first_program").is_err());
        fs::remove_dir_all(&dir).expect("the scratch directory goes");
    }

    #[test]
    fn a_target_directory_holds_its_own_stubs_programs_and_linker_output() {
        let target = Path::new("/somewhere/else");
        assert_eq!(
            ring3_dir(Some(target)),
            Path::new("/somewhere/else/xtask/ring3")
        );
        assert_eq!(ring3_dir(None), target_dir().join("xtask").join("ring3"));
        for program in PROGRAMS {
            assert_eq!(
                linked(Some(target), program),
                target
                    .join(USERSPACE_TARGET)
                    .join("release")
                    .join(program.package)
            );
        }
    }
}
