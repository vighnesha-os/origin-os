//! The userspace build of CLAUDE.md §11, `origin.core`, and the programs the kernel carries
//! (task S4-1, TASKS item 41, and task S4-2, TASKS item 47; ADR-0011 Amendment 2, L1 to L6).
//!
//! - [`generate`] writes the code for `origin.kernel` (`oidl generate`, full mode) under the
//!   target directory: the client and its imports for `runtime/kernel-client` to include, and
//!   the stubs, with the list of what they export, for `runtime/core`. Nothing generated is
//!   committed, so no committed crate holds a shim.
//! - Every ring-3 build names `--target x86_64-unknown-linux-gnu` and passes
//!   `.cargo/userspace.toml` with `--config`; nothing else reads that file.
//! - [`origin_core`] links `runtime/core` as the shared object `origin.core` and has
//!   `onbf-pack corelib` describe it for the kernel, which refuses an image that is anything
//!   but the listed stubs.
//! - [`programs`] does that, then links each carried program against `origin.core`, so that
//!   the program holds no stub and the linker names each of its imports; has `onbf-pack
//!   carried` describe it, imports and all; and returns the directory every kernel build names
//!   in [`CARRIED_ENV`].
//! - [`carried_files`] lists what one build wrote for `origin.core` and for each carried
//!   program, for the reproducible-build gate to read (TASKS item 32).
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
/// The variable `runtime/kernel-client` reads for the generated client's directory.
const CLIENT_ENV: &str = "ORIGIN_KERNEL_CLIENT";
/// The variable `runtime/core` reads for the generated stubs' directory.
const CORE_STUBS_ENV: &str = "ORIGIN_CORE_STUBS";
/// `origin.core`'s package, the name its linker output has, its `DT_SONAME`, and the name its
/// description has in the kernel.
pub(crate) const CORE_PACKAGE: &str = "origin-core";
const CORE_LINKED: &str = "liborigin_core.so";
pub(crate) const CORE_SONAME: &str = "origin.core";
pub(crate) const CORE_NAME: &str = "origin_core";
/// The stubs' file and the list of what they export, under the generated stubs' directory.
const CORE_STUBS_FILE: &str = "origin_kernel/kernel.rs";
const CORE_EXPORTS: &str = "origin_kernel.exports";
/// Where a program's link finds `origin.core`, as the linker is told: a path from the
/// repository's root, where Cargo runs the compiler of a workspace's package. The same text in
/// every build, in every target directory and on every machine: Cargo hashes a package's
/// compiler arguments into what it builds, so a path of one build's own would make that
/// build's programs its own too (CLAUDE.md §75). [`origin_core`] puts a copy of the library
/// there before any program is linked.
const CORE_LINK: &str = "target/xtask/core-link/liborigin_core.so";
/// The variable the kernel's `program.rs` reads for the carried programs' descriptions.
pub(crate) const CARRIED_ENV: &str = "ORIGIN_CARRIED_PROGRAMS";
/// The definition the stubs are generated from.
const KERNEL_DEFINITION: &str = "sdk/interfaces/origin.kernel.oidl";

/// Every ring-3 crate, for clippy.
pub(crate) const CRATES: &[&str] = &[
    CORE_PACKAGE,
    "origin-kernel-client",
    "origin-rt",
    "origin-rt-mem",
    "first-program",
    "pin-program",
];
/// The bodies of the runtime's `mem*` functions: a ring-3 crate the host tests too (S4-1's
/// review, finding F6).
pub(crate) const MEM_PACKAGE: &str = "origin-rt-mem";

/// A program the kernel carries: its package and binary, its name in the kernel, and its entry
/// if that is not the runtime's `_start`.
struct Program {
    package: &'static str,
    name: &'static str,
    entry: Option<&'static str>,
}

/// Every program the kernel carries. The second starts at an entry of its own, which reads
/// RFLAGS before the runtime's runs (ADR-0005 Amendment 9, H1).
const PROGRAMS: &[Program] = &[
    Program {
        package: "first-program",
        name: "first_program",
        entry: None,
    },
    Program {
        package: "pin-program",
        name: "pin_program",
        entry: Some("origin_pin_entry"),
    },
];

/// The package of each ring-3 image the kernel carries: `origin.core`, then each program.
pub(crate) fn carried_packages() -> impl Iterator<Item = &'static str> {
    core::iter::once(CORE_PACKAGE).chain(PROGRAMS.iter().map(|program| program.package))
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

/// Where one build's generated code for `origin.kernel` is.
pub(crate) struct Generated {
    /// The client's tree: `oidl_rt`, the library's types, its client and its imports, with the
    /// `modules.rs` that `runtime/kernel-client` includes. What [`CLIENT_ENV`] names.
    pub(crate) client: PathBuf,
    /// The stubs' tree: one file of naked functions for each interface, and the list of what
    /// they export. What [`CORE_STUBS_ENV`] names.
    pub(crate) core: PathBuf,
}

impl Generated {
    /// The generator's list of `origin.core`'s exports.
    pub(crate) fn exports(&self) -> PathBuf {
        self.core.join(CORE_EXPORTS)
    }

    /// The file of `origin.kernel`'s stubs.
    pub(crate) fn stubs(&self) -> PathBuf {
        self.core.join(CORE_STUBS_FILE)
    }
}

/// Copies every file under `from` into `to` that is not already there with the same bytes, and
/// removes every file under `to` that `from` does not hold, but for those named in `keep`.
fn mirror(from: &Path, to: &Path, keep: &[&str]) -> Result {
    let wanted = files_under(from)?;
    if wanted.is_empty() {
        return Err(format!(
            "oidl wrote nothing under {}: nothing to build",
            from.display()
        ));
    }
    for relative in &wanted {
        write_if_changed(&to.join(relative), &read_file(&from.join(relative))?)?;
    }
    for relative in files_under(to)? {
        if !keep.iter().any(|kept| relative == Path::new(kept)) && !wanted.contains(&relative) {
            let gone = to.join(&relative);
            fs::remove_file(&gone).map_err(|e| format!("{}: {e}", gone.display()))?;
        }
    }
    Ok(())
}

/// Generates `origin.kernel`'s code into a scratch directory, copies each file that changed
/// into place, and writes the `modules.rs` that `runtime/kernel-client` includes. Returns where
/// the client's tree and the stubs' tree are.
pub(crate) fn generate(target: Option<&Path>) -> Result<Generated> {
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
    let generated = Generated {
        client: ring3_dir(target).join("oidl"),
        core: ring3_dir(target).join("oidl-core"),
    };
    mirror(&staging.join("rust"), &generated.client, &["modules.rs"])?;
    mirror(&staging.join("core"), &generated.core, &[])?;
    for needed in [generated.exports(), generated.stubs()] {
        if !needed.is_file() {
            return Err(format!(
                "oidl did not write {}: `origin.core` cannot be built",
                needed.display()
            ));
        }
    }
    let quoted = |file: PathBuf| -> Result<String> {
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
    // Clippy finds one thing in the generated client, which only the generator can change: an
    // `Ok(x?)` (`needless_question_mark`). It is allowed on that module alone.
    //
    // The client's tree and nothing of the stubs': a program holds no `syscall`, and reaches
    // the kernel through the imports the launch path binds to `origin.core` (ADR-0011 L3).
    let modules = format!(
        "// Written by `cargo xtask` (developer-tools/xtask/src/ring3.rs). Do not edit.\n\
         #[rustfmt::skip]\n#[path = \"{}\"]\npub mod oidl_rt;\n\
         #[rustfmt::skip]\n#[allow(clippy::needless_question_mark)]\n\
         #[path = \"{}\"]\npub mod origin_kernel;\n",
        quoted(generated.client.join("oidl_rt.rs"))?,
        quoted(generated.client.join("origin_kernel/mod.rs"))?,
    );
    write_if_changed(&generated.client.join("modules.rs"), modules.as_bytes())?;
    Ok(generated)
}

/// `cargo <subcommand>` for the ring-3 crates: the userspace triple, its configuration file, the
/// remap that keeps the toolchain's path out of what it builds (`toolchain`, TASKS item 32),
/// and the generated code's two directories.
fn userspace(subcommand: &str, generated: &Generated, target: Option<&Path>) -> Result<Command> {
    let mut command = cargo();
    command
        .arg(subcommand)
        .args(["--target", USERSPACE_TARGET, "--config", USERSPACE_CONFIG])
        .args(crate::toolchain::userspace_config()?)
        .env(CLIENT_ENV, &generated.client)
        .env(CORE_STUBS_ENV, &generated.core);
    if let Some(target) = target {
        command.arg("--target-dir").arg(target);
    }
    Ok(command)
}

/// Each program the kernel carries: its package, its name in the kernel, and where the linker
/// writes it. `origin.core` is not one of them.
pub(crate) fn program_images(target: Option<&Path>) -> Vec<(&'static str, &'static str, PathBuf)> {
    PROGRAMS
        .iter()
        .map(|program| (program.package, program.name, linked(target, program)))
        .collect()
}

/// Where the descriptions of one build go: what the kernel's builds name in [`CARRIED_ENV`].
pub(crate) fn carried_dir(target: Option<&Path>) -> PathBuf {
    ring3_dir(target).join("carried")
}

/// Where the linker writes `origin.core`.
pub(crate) fn core_linked(target: Option<&Path>) -> PathBuf {
    base_dir(target)
        .join(USERSPACE_TARGET)
        .join("release")
        .join(CORE_LINKED)
}

/// Links `runtime/core` as the shared object `origin.core` and describes it for the kernel
/// (ADR-0011 Amendment 2, L2 to L7): `onbf-pack corelib` refuses an image that is anything but
/// the generated stubs, each with one `syscall`, with no writable data and nothing to relocate.
///
/// Its name is given to the linker for this one image: `cargo rustc` passes what follows `--`
/// to the compiler of this package alone. It has no entry, and `.cargo/userspace.toml` names
/// none.
fn origin_core(generated: &Generated, target: Option<&Path>) -> Result {
    let mut build = userspace("rustc", generated, target)?;
    build
        .args(["--release", "--package", CORE_PACKAGE, "--", "-C"])
        .arg(format!("link-arg=--soname={CORE_SONAME}"));
    let linked = core_linked(target);
    // An ELF the kernel takes its code from: it must have been linked with the linker's fixed
    // line, as each program must (TASKS item 44).
    crate::image_inputs::build_with_the_fixed_line(core::slice::from_ref(&linked), || {
        run_step(&mut build)
    })?;
    if !linked.is_file() {
        return Err(format!(
            "cargo reported success but {} does not exist",
            linked.display()
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
        .args(["corelib", "--variable", CARRIED_ENV])
        .arg("--input")
        .arg(&linked)
        .arg("--exports")
        .arg(generated.exports())
        .arg("--out")
        .arg(carried_dir(target));
    run_step(&mut describe)?;
    // Where each program's link looks for it: see [`CORE_LINK`].
    write_if_changed(&crate::repo_root().join(CORE_LINK), &read_file(&linked)?)
}

/// Builds `origin.core` and every carried program for ring 3 and describes each for the kernel.
/// Returns the directory the kernel's builds name in [`CARRIED_ENV`].
pub(crate) fn programs(target: Option<&Path>) -> Result<PathBuf> {
    let generated = generate(target)?;
    origin_core(&generated, target)?;
    // Each program is linked against `origin.core`: the library is one more input of its link,
    // given to the compiler of that package alone (`cargo rustc` passes what follows `--` to
    // it), with the program's own entry where it has one.
    let mut builds = Vec::new();
    for program in PROGRAMS {
        let mut build = userspace("rustc", &generated, target)?;
        build
            .args(["--release", "--package", program.package, "--", "-C"])
            .arg(format!("link-arg={CORE_LINK}"));
        if let Some(entry) = program.entry {
            build.arg("-C").arg(format!("link-arg=--entry={entry}"));
        }
        builds.push(build);
    }
    // Each is an ELF the kernel takes a segment of with its header in it, so each must have
    // been linked with the linker's fixed line: one from before it is linked again.
    let linked_files: Vec<PathBuf> = PROGRAMS.iter().map(|p| linked(target, p)).collect();
    crate::image_inputs::build_with_the_fixed_line(&linked_files, || {
        builds.iter_mut().try_for_each(run_step)
    })?;
    let out = carried_dir(target);
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
            .arg("--core-exports")
            .arg(generated.exports())
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

/// Every file of `origin.core` and of every carried program built into `target`: the linker's
/// output, then what `onbf-pack` wrote for it, by name. [`programs`] must have run for
/// `target`. An image with no segment file is an error: there would be nothing of it to read.
/// `origin.core`'s one segment file is its code, which the kernel's image holds as each
/// program's segments are held.
pub(crate) fn carried_files(target: Option<&Path>) -> Result<Vec<CarriedFile>> {
    let carried = carried_dir(target);
    let mut files = Vec::new();
    let images = core::iter::once((CORE_NAME, CORE_LINKED, core_linked(target))).chain(
        PROGRAMS
            .iter()
            .map(|program| (program.name, program.package, linked(target, program))),
    );
    for (name, linked_name, path) in images {
        files.push(CarriedFile {
            program: name,
            kind: CarriedKind::Linked,
            file: linked_name.to_string(),
            path,
        });
        let described = described_files(&carried, name)?;
        if !described
            .iter()
            .any(|(_, kind)| *kind == CarriedKind::Segment)
        {
            return Err(format!(
                "{} holds no segment of `{name}`: nothing of it to read",
                carried.display(),
            ));
        }
        for (file, kind) in described {
            files.push(CarriedFile {
                program: name,
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
    let generated = generate(None)?;
    for package in CRATES {
        let mut clippy = userspace("clippy", &generated, None)?;
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
