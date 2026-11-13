//! What an image is built from besides its own source, kept free of the build host (TASKS
//! item 44; CLAUDE.md §75, §62).
//!
//! Cargo gives each crate a `-C metadata` value, and the compiler orders an image's code by
//! it. For a unit built for the host, a build script or a procedural macro, that value takes
//! in the host the compiler reports; and each unit takes in the values of the units it
//! depends on (`docs/research/2026-11-11-toolchain-path-in-images.md` §12.6). So an image
//! with such a unit anywhere under it is other bytes on another kind of host.
//!
//! - [`graph_gate`]: no package an image is built from has a build script or is a procedural
//!   macro. Cargo's own account of each package's targets says which do (`cargo metadata`),
//!   and its dependency tree says what each image is built from (`cargo tree`).
//! - [`version_gate`] and [`write_version_file`]: the release's constants are a file in the
//!   tree, `runtime/version/src/generated.rs`, where a build script used to write them at
//!   build time. `cargo xtask version` writes it from `origin-version.toml`, and the gate
//!   fails when the two are out of step.
//!
//! - [`linker_text`] and [`check_linker_text`]: the linker writes a line about itself into
//!   `.comment` of every ELF, and its builds for two kinds of host word it differently (the
//!   ledger's §13). `.cargo/config.toml` fixes that line with `LLD_VERSION`. The
//!   reproducible-build gate reads `.comment` of every ELF it builds and fails if the line is
//!   not the fixed one; `cargo xtask linker-text-gate <ELF>...` does it for a file by hand.
//!
//! The kernel's link arguments, which its build script used to print, are in
//! `.cargo/config.toml`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{
    KERNEL_PACKAGE, KERNEL_TARGET, LOADER_PACKAGE, Result, UEFI_TARGET, cargo, json_strings,
    repo_root, ring3,
};

/// A target of a package that Cargo builds for the host, by the name `cargo metadata` gives
/// its kind, and what to call it.
const HOST_KINDS: &[(&str, &str)] = &[
    ("custom-build", "a build script"),
    ("proc-macro", "a procedural macro"),
];

/// One target built for the host: what it is, and its source file.
#[derive(Debug, PartialEq, Eq)]
struct HostUnit {
    what: &'static str,
    source: PathBuf,
}

/// Every target built for the host among the workspace's packages, from the compact JSON of
/// `cargo metadata --no-deps`. A target is one object with no object inside it, and holds its
/// `kind` and its `src_path`.
fn host_units(metadata: &str) -> Result<Vec<HostUnit>> {
    let mut units = Vec::new();
    for &(kind, what) in HOST_KINDS {
        let marker = format!("\"kind\":[\"{kind}\"]");
        for (at, _) in metadata.match_indices(&marker) {
            let malformed = || format!("cargo metadata: a {kind} target that cannot be read");
            let start = metadata[..at].rfind('{').ok_or_else(malformed)?;
            let end = at + metadata[at..].find('}').ok_or_else(malformed)?;
            let sources = json_strings(&metadata[start..end], "src_path").ok_or_else(malformed)?;
            let [source] = sources.as_slice() else {
                return Err(malformed());
            };
            units.push(HostUnit {
                what,
                source: PathBuf::from(source),
            });
        }
    }
    Ok(units)
}

/// The directory of each of the workspace's packages, from the same JSON.
fn package_directories(metadata: &str) -> Result<Vec<PathBuf>> {
    let manifests = json_strings(metadata, "manifest_path")
        .ok_or("cargo metadata: an escape in a manifest_path value")?;
    Ok(manifests
        .iter()
        .filter_map(|manifest| Path::new(manifest).parent().map(Path::to_path_buf))
        .collect())
}

/// The package a source file belongs to: the deepest package directory that holds it. One
/// package's directory can be inside another's, as the framework's is inside the kernel's.
fn owner<'a>(source: &Path, packages: &'a [PathBuf]) -> Option<&'a PathBuf> {
    packages
        .iter()
        .filter(|directory| source.starts_with(directory))
        .max_by_key(|directory| directory.components().count())
}

/// A package in a dependency tree: its name and its directory, from one line of
/// `cargo tree --prefix none`, which reads `name v0.0.0 (/its/directory)` with `(proc-macro)`
/// before the directory or `(*)` after it at times. A line with no directory is a package
/// from outside the workspace, and is an error here: this gate could not look at it.
fn tree_package(line: &str) -> Result<(String, PathBuf)> {
    let unreadable = || format!("cargo tree: a line that names no directory: `{line}`");
    let name = line.split_whitespace().next().ok_or_else(unreadable)?;
    let start = line.find(" (/").ok_or_else(unreadable)? + 2;
    let length = line[start..].find(')').ok_or_else(unreadable)?;
    Ok((
        name.to_string(),
        PathBuf::from(&line[start..start + length]),
    ))
}

/// What is wrong with one image's dependency tree: each package in it that has a target built
/// for the host. `tree` is the output of `cargo tree --prefix none` for the image's package.
fn graph_problems(
    image: &str,
    tree: &str,
    units: &[HostUnit],
    packages: &[PathBuf],
) -> Result<(Vec<String>, BTreeSet<String>)> {
    let mut problems = BTreeSet::new();
    let mut names = BTreeSet::new();
    for line in tree.lines().filter(|line| !line.trim().is_empty()) {
        let (name, directory) = tree_package(line)?;
        for unit in units {
            if owner(&unit.source, packages) == Some(&directory) {
                problems.insert(format!(
                    "{image} is built from `{name}`, which has {} ({})",
                    unit.what,
                    unit.source
                        .strip_prefix(repo_root())
                        .unwrap_or(&unit.source)
                        .display()
                ));
            }
        }
        names.insert(name);
    }
    Ok((problems.into_iter().collect(), names))
}

/// Every image, by what to call it, its package and the triple it is built for.
fn images() -> Vec<(String, &'static str, &'static str)> {
    let mut images = vec![
        ("the loader".to_string(), LOADER_PACKAGE, UEFI_TARGET),
        ("the kernel".to_string(), KERNEL_PACKAGE, KERNEL_TARGET),
    ];
    images.extend(ring3::carried_packages().map(|package| {
        (
            format!("the carried program `{package}`"),
            package,
            ring3::USERSPACE_TARGET,
        )
    }));
    images
}

fn output_of(command: &mut std::process::Command, what: &str) -> Result<String> {
    let output = command
        .output()
        .map_err(|e| format!("cannot run {what}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{what} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|_| format!("{what}: output that is not UTF-8"))
}

/// `cargo xtask check`'s gate: no package an image is built from has a build script or is a
/// procedural macro, with every feature of each on. Normal and build dependencies both count;
/// what only a package's own tests use does not reach an image.
pub(crate) fn graph_gate() -> Result {
    let metadata = output_of(
        cargo().args(["metadata", "--format-version", "1", "--no-deps"]),
        "cargo metadata",
    )?;
    let units = host_units(&metadata)?;
    let packages = package_directories(&metadata)?;
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    let images = images();
    for (image, package, target) in &images {
        let tree = output_of(
            cargo().args([
                "tree",
                "--package",
                package,
                "--prefix",
                "none",
                "--edges",
                "normal,build",
                "--target",
                target,
                "--all-features",
            ]),
            "cargo tree",
        )?;
        let (found, names) = graph_problems(image, &tree, &units, &packages)?;
        if !names.contains(*package) {
            return Err(format!(
                "cargo tree for {package} does not name {package}: the gate on the images' dependency graphs cannot read it"
            ));
        }
        problems.extend(found);
        seen.extend(names);
    }
    if problems.is_empty() {
        println!(
            "xtask: image-graph gate: none of the {} packages the {} images are built from has a build script or is a procedural macro (of {} in the workspace, {} have one)",
            seen.len(),
            images.len(),
            packages.len(),
            units.len()
        );
        Ok(())
    } else {
        Err(format!(
            "a package an image is built from has a target that Cargo builds for the host (TASKS item 44; CLAUDE.md §75). An image's bytes then follow the kind of host that builds it (`docs/research/2026-11-11-toolchain-path-in-images.md` §12.6). Give the image what the target supplied another way: a file in the tree with a gate, or `.cargo/config.toml`.\n  {}",
            problems.join("\n  ")
        ))
    }
}

/// The parser and the generator, which the version crate's own tests use too.
#[path = "../../../runtime/version/manifest.rs"]
mod manifest;

/// Where the linker's line about itself is fixed.
const CARGO_CONFIG: &str = ".cargo/config.toml";
/// The variable lld reads in place of its own line.
const LINKER_VARIABLE: &str = "LLD_VERSION";
/// How the linker's own line starts, in every build of it.
const LINKERS_OWN: &str = "Linker: LLD";
/// How the line the compiler writes beside it starts. It holds the compiler's version and
/// nothing of the host.
const COMPILERS_LINE: &str = "rustc version ";

/// The fixed line, from the text of [`CARGO_CONFIG`]: the value of [`LINKER_VARIABLE`] in its
/// `[env]` table, which must be forced, so that a value in the environment cannot take its
/// place.
fn linker_text_in(config: &str) -> Result<String> {
    let missing = || {
        format!(
            "{CARGO_CONFIG} has no `{LINKER_VARIABLE} = {{ value = \"...\", force = true }}` under `[env]`: the linker then writes its own line about itself into every ELF, which is other text on another kind of host (CLAUDE.md §75; `docs/research/2026-11-11-toolchain-path-in-images.md` §13)"
        )
    };
    let mut in_env = false;
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            in_env = line == "[env]";
            continue;
        }
        let Some(rest) = line.strip_prefix(LINKER_VARIABLE) else {
            continue;
        };
        if !in_env {
            continue;
        }
        let entry = rest
            .trim_start()
            .strip_prefix('=')
            .map(str::trim)
            .ok_or_else(missing)?;
        let value = entry
            .strip_prefix("{ value = \"")
            .and_then(|rest| rest.strip_suffix("\", force = true }"))
            .ok_or_else(|| {
                format!(
                    "{CARGO_CONFIG}: `{LINKER_VARIABLE}` must read `{{ value = \"...\", force = true }}`, so that a value in the environment cannot take its place; it reads `{entry}`"
                )
            })?;
        if value.is_empty() || value.contains(['"', '\\']) || value.starts_with(LINKERS_OWN) {
            return Err(format!(
                "{CARGO_CONFIG}: `{LINKER_VARIABLE}` is `{value}`: it must be a plain text of its own, not empty and not the linker's own line"
            ));
        }
        return Ok(value.to_string());
    }
    Err(missing())
}

/// The fixed line this repository's configuration gives the linker.
pub(crate) fn linker_text() -> Result<String> {
    linker_text_in(&read(CARGO_CONFIG)?)
}

/// A little-endian integer of `N` bytes at `at`.
fn field<const N: usize>(bytes: &[u8], at: usize) -> Option<[u8; N]> {
    bytes.get(at..at.checked_add(N)?)?.try_into().ok()
}

fn half(bytes: &[u8], at: usize) -> Option<usize> {
    field(bytes, at).map(|b| usize::from(u16::from_le_bytes(b)))
}

fn word(bytes: &[u8], at: usize) -> Option<usize> {
    field(bytes, at).map(u32::from_le_bytes)?.try_into().ok()
}

fn long(bytes: &[u8], at: usize) -> Option<usize> {
    field(bytes, at).map(u64::from_le_bytes)?.try_into().ok()
}

/// The contents of the section named `wanted` in a 64-bit little-endian ELF, or `None` if it
/// has no such section. An error is a file that is not such an ELF, or whose section table
/// points outside it.
fn elf_section<'a>(elf: &'a [u8], wanted: &str) -> Result<Option<&'a [u8]>> {
    let malformed =
        |what: &str| format!("not a 64-bit little-endian ELF this gate can read: {what}");
    if elf.get(..6) != Some(b"\x7fELF\x02\x01".as_slice()) {
        return Err(malformed("its first six bytes"));
    }
    let table = long(elf, 0x28).ok_or_else(|| malformed("e_shoff"))?;
    let entry = half(elf, 0x3a).ok_or_else(|| malformed("e_shentsize"))?;
    let count = half(elf, 0x3c).ok_or_else(|| malformed("e_shnum"))?;
    let names_index = half(elf, 0x3e).ok_or_else(|| malformed("e_shstrndx"))?;
    if entry < 0x40 || table == 0 {
        return Err(malformed("it has no section table"));
    }
    // One section's file offset and size, and the offset of its name in the names' section.
    let section = |index: usize| -> Option<(usize, usize, usize)> {
        let at = table.checked_add(index.checked_mul(entry)?)?;
        Some((long(elf, at + 0x18)?, long(elf, at + 0x20)?, word(elf, at)?))
    };
    let (names_at, names_size, _) =
        section(names_index).ok_or_else(|| malformed("the section of section names"))?;
    let names = names_at
        .checked_add(names_size)
        .and_then(|end| elf.get(names_at..end))
        .ok_or_else(|| malformed("the section of section names"))?;
    for index in 0..count {
        let (at, size, name) = section(index).ok_or_else(|| malformed("a section header"))?;
        let name = names
            .get(name..)
            .and_then(|rest| rest.split(|&byte| byte == 0).next())
            .ok_or_else(|| malformed("a section's name"))?;
        if name == wanted.as_bytes() {
            return at
                .checked_add(size)
                .and_then(|end| elf.get(at..end))
                .map(Some)
                .ok_or_else(|| malformed("a section's contents"));
        }
    }
    Ok(None)
}

/// What is wrong with the lines in `.comment` of `elf`, a file a linker wrote, given the
/// fixed line `text`: the linker's line must be `text` and nothing else, on any machine. The
/// compiler's own line beside it is expected.
fn linker_text_problem(elf: &[u8], text: &str) -> Result<Option<String>> {
    let Some(comment) = elf_section(elf, ".comment")? else {
        return Ok(Some(format!(
            "has no `.comment` section, where the line `{text}` should be"
        )));
    };
    let lines: Vec<String> = comment
        .split(|&byte| byte == 0)
        .filter(|line| !line.is_empty())
        .map(|line| String::from_utf8_lossy(line).into_owned())
        .collect();
    let others: Vec<&String> = lines
        .iter()
        .filter(|line| !line.starts_with(COMPILERS_LINE) && line.as_str() != text)
        .collect();
    if let Some(other) = others.first() {
        let what = if other.starts_with(LINKERS_OWN) {
            "the linker's own line about itself"
        } else {
            "a line this gate does not know"
        };
        return Ok(Some(format!(
            "holds {what} in `.comment`: `{other}`. The line there must be `{text}`, which {CARGO_CONFIG} gives the linker in `{LINKER_VARIABLE}`. If the linker has stopped reading that variable, its own line is back, and it differs from one kind of host to another"
        )));
    }
    if !lines.iter().any(|line| line == text) {
        return Ok(Some(format!(
            "does not hold the line `{text}` in `.comment`; it holds {lines:?}"
        )));
    }
    Ok(None)
}

/// Fails unless `.comment` of `elf`, which the messages call `name`, holds the fixed line and
/// not the linker's own.
pub(crate) fn check_linker_text(name: &str, elf: &[u8], text: &str) -> Result {
    match linker_text_problem(elf, text).map_err(|e| format!("{name}: {e}"))? {
        Some(problem) => Err(format!(
            "{name} {problem} (CLAUDE.md §75; `docs/research/2026-11-11-toolchain-path-in-images.md` §13)"
        )),
        None => Ok(()),
    }
}

/// Builds, then makes sure each of `outputs`, the ELF files the build links, was linked with
/// the fixed line; one that was not is removed and the build run once more.
///
/// Cargo does not link a file again when `[env]` changes: nothing it tracks for that file
/// changed. So a target directory that held a kernel or a program from before the entry, or
/// from before its text last changed, keeps that file, with the linker's own line or the old
/// one in it. A kernel built on top would carry a program's header from before, and would
/// not be the kernel the reproducible-build gate builds from nothing. Removing the file makes
/// Cargo link it again. Returns the files it had to link again.
pub(crate) fn build_with_the_fixed_line(
    outputs: &[PathBuf],
    mut build: impl FnMut() -> Result,
) -> Result<Vec<PathBuf>> {
    let text = linker_text()?;
    relink_stale(
        outputs,
        &text,
        &mut build,
        &|path| fs::read(path).map_err(|e| format!("{}: {e}", path.display())),
        &remove_linked,
    )
}

/// The target directory and the target that `path` was built into: Cargo writes a linked file
/// to `<target directory>/<target>/release/<file>`. `None` for a path of any other shape, and
/// for a target that no ELF image is built for.
fn built_into(path: &Path) -> Option<(&Path, &str)> {
    let release = path.parent()?;
    if release.file_name()? != "release" {
        return None;
    }
    let target = release.parent()?;
    let name = target.file_name()?.to_str()?;
    [KERNEL_TARGET, ring3::USERSPACE_TARGET]
        .contains(&name)
        .then_some((target.parent()?, name))
}

/// The packages whose linked files Cargo writes for `target`: the kernel for the kernel's
/// target, and each ring-3 image the kernel carries for the userspace's.
fn linked_packages(target: &str) -> Vec<&'static str> {
    if target == KERNEL_TARGET {
        vec![KERNEL_PACKAGE]
    } else {
        ring3::carried_packages().collect()
    }
}

/// The arguments of a `cargo clean` of `packages`, as built for `target` into `directory`.
/// An error if no package is named.
///
/// **The packages are what make it safe**, so no call can be made without one. Without
/// `--package`, `cargo clean --release --target <target>` does not look at the target at all:
/// it removes the host's own `release` directory, and without `--release` the whole target
/// directory (measured with Cargo 1.99.1 and 1.100.0 on 2026-11-13, after a first form of
/// this did exactly that and left the stale file where it was).
fn clean_arguments(
    directory: &Path,
    target: &str,
    packages: &[&str],
) -> Result<Vec<std::ffi::OsString>> {
    if packages.is_empty() {
        return Err(format!(
            "no package is named for a `cargo clean` of {target} in {}: without `--package`, Cargo would remove the host's own `release` directory and leave what was meant",
            directory.display()
        ));
    }
    if directory.as_os_str().is_empty() {
        return Err(format!(
            "no target directory is named for a `cargo clean` of {packages:?} for {target}: an empty `--target-dir` is not the directory the file was built into"
        ));
    }
    let mut arguments: Vec<std::ffi::OsString> =
        ["clean", "--quiet", "--release", "--target", target]
            .into_iter()
            .map(Into::into)
            .collect();
    arguments.push("--target-dir".into());
    arguments.push(directory.into());
    for package in packages {
        arguments.push("--package".into());
        arguments.push(package.into());
    }
    Ok(arguments)
}

/// Makes Cargo link `path` again, by having it remove what it built of the packages that
/// link a file for that file's target.
///
/// Removing the file alone does not do it: Cargo keeps what the linker wrote among its
/// intermediate files and gives the old file its name back without linking. Where those files
/// are and what they are called is no interface: Cargo 1.100.0 moved them, and a shared
/// library's copy never had the hash in its name that a program's has. So nothing here names
/// one, and `cargo clean --package` is Cargo's own way. It costs one more compilation of those
/// packages, on the rare run that finds a file from before the fixed line.
fn remove_linked(path: &Path) -> Result {
    let (directory, target) = built_into(path).ok_or_else(|| {
        format!(
            "{} is not `<target directory>/<target>/release/<file>` for a target an image is linked for, so what to have Cargo build again is not known",
            path.display()
        )
    })?;
    let packages = linked_packages(target);
    let arguments = clean_arguments(directory, target, &packages)?;
    crate::run_step(crate::cargo().args(arguments))?;
    if path.exists() {
        return Err(still_there(path, target, &packages));
    }
    Ok(())
}

/// What to say when `cargo clean` of `packages` left `path` where it was: none of them links
/// that file. Said instead of the later "removed and linked again, and still does", which would
/// be untrue of a file that was never removed (TASKS item 52's review, finding 4).
fn still_there(path: &Path, target: &str, packages: &[&str]) -> String {
    format!(
        "{} is still there after `cargo clean` of {} for {target}: none of those packages links it, so it cannot be linked again this way. Name the package that links it among those `linked_packages` gives for {target} (TASKS item 50)",
        path.display(),
        packages.join(", ")
    )
}

/// [`build_with_the_fixed_line`] over the three things it does to a file, so that a test can
/// stand in for them.
fn relink_stale(
    outputs: &[PathBuf],
    text: &str,
    build: &mut dyn FnMut() -> Result,
    read: &dyn Fn(&Path) -> Result<Vec<u8>>,
    remove: &dyn Fn(&Path) -> Result,
) -> Result<Vec<PathBuf>> {
    build()?;
    let stale = |path: &PathBuf| -> Result<Option<String>> {
        linker_text_problem(&read(path)?, text).map_err(|e| format!("{}: {e}", path.display()))
    };
    let mut again = Vec::new();
    for path in outputs {
        if stale(path)?.is_some() {
            remove(path)?;
            again.push(path.clone());
        }
    }
    if again.is_empty() {
        return Ok(again);
    }
    build()?;
    for path in &again {
        if let Some(problem) = stale(path)? {
            return Err(format!(
                "{} {problem}. It was removed and linked again, and still does (CLAUDE.md §75)",
                path.display()
            ));
        }
        println!(
            "xtask: note: {} had been linked before {CARGO_CONFIG} fixed the linker's line, or with another text; Cargo does not link it again for that, so it was removed and linked again",
            path.display()
        );
    }
    Ok(again)
}

/// Where in `file` the linker's own line about itself starts, if anywhere. No file an image
/// is made of may hold it, an ELF or not.
pub(crate) fn linkers_own_line_in(file: &[u8]) -> Option<usize> {
    let needle = LINKERS_OWN.as_bytes();
    file.windows(needle.len())
        .position(|window| window == needle)
}

/// `cargo xtask linker-text-gate <ELF>...`: the same check on files named by hand.
pub(crate) fn linker_text_gate(files: &[&str]) -> Result {
    if files.is_empty() {
        return Err("linker-text-gate: name at least one ELF file".into());
    }
    let text = linker_text()?;
    for file in files {
        let bytes = fs::read(file).map_err(|e| format!("{file}: {e}"))?;
        check_linker_text(file, &bytes, &text)?;
        println!(
            "xtask: linker-text gate: {file}: `.comment` holds `{text}` and not the linker's own line"
        );
    }
    Ok(())
}

/// The one place a version is written by hand (CLAUDE.md §62).
const VERSIONS: &str = "origin-version.toml";
/// The constants generated from it, which `origin-version` includes.
const GENERATED: &str = "runtime/version/src/generated.rs";

/// The text of [`GENERATED`] for `versions`, the text of [`VERSIONS`].
fn generated(versions: &str) -> Result<String> {
    manifest::parse(versions)
        .map(|parsed| manifest::render(&parsed))
        .map_err(|e| format!("{VERSIONS}: {e}"))
}

/// What is wrong with `committed`, the text of [`GENERATED`], given the text of [`VERSIONS`].
fn version_problem(versions: &str, committed: &str) -> Result<Option<String>> {
    let wanted = generated(versions)?;
    if committed == wanted {
        return Ok(None);
    }
    let line = wanted
        .lines()
        .zip(committed.lines())
        .position(|(wanted, committed)| wanted != committed)
        .unwrap_or_else(|| wanted.lines().count().min(committed.lines().count()));
    let shown = |text: &str| text.lines().nth(line).unwrap_or("(nothing)").to_string();
    Ok(Some(format!(
        "{GENERATED} is not what the generator writes from {VERSIONS} (CLAUDE.md §62); run `cargo xtask version`. The first line that differs is line {}:\n  generated: {}\n  committed: {}",
        line + 1,
        shown(&wanted),
        shown(committed)
    )))
}

fn read(relative: &str) -> Result<String> {
    let path = repo_root().join(relative);
    fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
}

/// `cargo xtask check`'s gate: the committed constants are the ones [`VERSIONS`] generates.
pub(crate) fn version_gate() -> Result {
    match version_problem(&read(VERSIONS)?, &read(GENERATED)?)? {
        Some(problem) => Err(problem),
        None => {
            println!(
                "xtask: version gate: {GENERATED} is what the generator writes from {VERSIONS}"
            );
            Ok(())
        }
    }
}

/// `cargo xtask version`: writes [`GENERATED`] from [`VERSIONS`]. The file is left alone when
/// it is already right, so nothing is rebuilt for it.
pub(crate) fn write_version_file() -> Result {
    let wanted = generated(&read(VERSIONS)?)?;
    let path = repo_root().join(GENERATED);
    if fs::read_to_string(&path).is_ok_and(|committed| committed == wanted) {
        println!("xtask: version: {GENERATED} is already what {VERSIONS} generates");
        return Ok(());
    }
    fs::write(&path, &wanted).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("xtask: version: wrote {GENERATED} from {VERSIONS}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERSIONS_TEXT: &str = "[release]\nversion = \"1.2.3-rc.1\"\nbuild = 42\nsecurity-version = 7\n\n[compatibility]\nonbf-format = \"1.0\"\nboot-info = \"0.6\"\nuser-abi = \"0.7\"\n";

    #[test]
    fn the_generated_text_says_what_wrote_it_and_holds_every_constant() {
        let text = generated(VERSIONS_TEXT).expect("a valid file");
        assert_eq!(
            text.lines().next(),
            Some("// Generated by `cargo xtask version` from origin-version.toml. Do not edit.")
        );
        for line in [
            "pub const VERSION: &str = \"1.2.3-rc.1\";",
            "pub const VERSION_MAJOR: u64 = 1;",
            "pub const VERSION_MINOR: u64 = 2;",
            "pub const VERSION_PATCH: u64 = 3;",
            "pub const VERSION_PRE: &str = \"rc.1\";",
            "pub const BUILD: u64 = 42;",
            "pub const SECURITY_VERSION: u32 = 7;",
            "pub const ONBF_FORMAT_MAJOR: u16 = 1;",
            "pub const ONBF_FORMAT_MINOR: u16 = 0;",
            "pub const BOOT_INFO_MAJOR: u16 = 0;",
            "pub const BOOT_INFO_MINOR: u16 = 6;",
            "pub const USER_ABI_MAJOR: u16 = 0;",
            "pub const USER_ABI_MINOR: u16 = 7;",
        ] {
            assert_eq!(text.lines().filter(|l| *l == line).count(), 1, "{line}");
        }
        assert_eq!(text.matches("pub const ").count(), 13);
        // The same file gives the same text: nothing of the host or the time is in it.
        assert_eq!(generated(VERSIONS_TEXT).expect("a valid file"), text);
    }

    #[test]
    fn a_generated_file_out_of_step_is_refused_with_the_command_that_rewrites_it() {
        let committed = generated(VERSIONS_TEXT).expect("a valid file");
        assert_eq!(version_problem(VERSIONS_TEXT, &committed), Ok(None));
        // The version file moved on and the generated file was not rewritten.
        let moved = VERSIONS_TEXT.replace("boot-info = \"0.6\"", "boot-info = \"0.7\"");
        let problem = version_problem(&moved, &committed)
            .expect("a valid file")
            .expect("out of step");
        assert!(problem.contains("run `cargo xtask version`"), "{problem}");
        assert!(
            problem.contains("generated: pub const BOOT_INFO_MINOR: u16 = 7;"),
            "{problem}"
        );
        assert!(
            problem.contains("committed: pub const BOOT_INFO_MINOR: u16 = 6;"),
            "{problem}"
        );
        // An edit by hand, a line lost, and a line added are each out of step.
        let by_hand = committed.replace("BUILD: u64 = 42", "BUILD: u64 = 43");
        assert!(
            version_problem(VERSIONS_TEXT, &by_hand)
                .expect("valid")
                .is_some()
        );
        let shorter = committed.lines().take(5).collect::<Vec<_>>().join("\n");
        let problem = version_problem(VERSIONS_TEXT, &shorter)
            .expect("valid")
            .expect("a line lost");
        assert!(problem.contains("committed: (nothing)"), "{problem}");
        let longer = format!("{committed}pub const EXTRA: u8 = 0;\n");
        let problem = version_problem(VERSIONS_TEXT, &longer)
            .expect("valid")
            .expect("a line added");
        assert!(problem.contains("generated: (nothing)"), "{problem}");
        // A version file the parser refuses is an error, not a pass.
        let error = version_problem("[release]\nversion = 1\n", &committed).expect_err("invalid");
        assert!(error.starts_with("origin-version.toml: "), "{error}");
    }

    const FIXED: &str = "Linker: a fixed line";

    /// A 64-bit little-endian ELF with nothing in it but `.comment` and the section of
    /// section names, laid out as a linker lays one out: the contents, then the table.
    fn elf_with_comment(comment: &[u8]) -> Vec<u8> {
        let names = b"\0.comment\0.shstrtab\0";
        let mut elf = vec![0u8; 0x40];
        elf[..6].copy_from_slice(b"\x7fELF\x02\x01");
        let comment_at = elf.len();
        elf.extend_from_slice(comment);
        let names_at = elf.len();
        elf.extend_from_slice(names);
        while !elf.len().is_multiple_of(8) {
            elf.push(0);
        }
        let table = elf.len();
        let mut header = |name: u32, at: usize, size: usize| {
            let mut entry = [0u8; 0x40];
            entry[..4].copy_from_slice(&name.to_le_bytes());
            entry[0x18..0x20].copy_from_slice(&(at as u64).to_le_bytes());
            entry[0x20..0x28].copy_from_slice(&(size as u64).to_le_bytes());
            elf.extend_from_slice(&entry);
        };
        header(0, 0, 0);
        header(1, comment_at, comment.len());
        header(10, names_at, names.len());
        elf[0x28..0x30].copy_from_slice(&(table as u64).to_le_bytes());
        elf[0x3a..0x3c].copy_from_slice(&0x40u16.to_le_bytes());
        elf[0x3c..0x3e].copy_from_slice(&3u16.to_le_bytes());
        elf[0x3e..0x40].copy_from_slice(&2u16.to_le_bytes());
        elf
    }

    #[test]
    fn the_fixed_line_is_read_from_the_configuration_and_must_be_forced() {
        let config = "[alias]\nxtask = \"run\"\n\n[env]\nLLD_VERSION = { value = \"Linker: a fixed line\", force = true }\n";
        assert_eq!(linker_text_in(config), Ok(FIXED.to_string()));
        // Taken out, or under another table, it is not there.
        for absent in [
            "[alias]\nxtask = \"run\"\n",
            "[env]\nOTHER = \"x\"\n",
            "[target.x]\nLLD_VERSION = { value = \"Linker: a fixed line\", force = true }\n",
            "[env]\n# LLD_VERSION = { value = \"Linker: a fixed line\", force = true }\n",
        ] {
            let error = linker_text_in(absent).expect_err("no entry");
            assert!(error.contains("has no `LLD_VERSION"), "{error}");
        }
        // Not forced, a value in the environment would take its place.
        for unforced in [
            "[env]\nLLD_VERSION = \"Linker: a fixed line\"\n",
            "[env]\nLLD_VERSION = { value = \"Linker: a fixed line\" }\n",
            "[env]\nLLD_VERSION = { value = \"Linker: a fixed line\", force = false }\n",
        ] {
            let error = linker_text_in(unforced).expect_err("not forced");
            assert!(error.contains("force = true"), "{error}");
        }
        // The linker's own line, or nothing, is no fixed line.
        for bad in [
            "[env]\nLLD_VERSION = { value = \"\", force = true }\n",
            "[env]\nLLD_VERSION = { value = \"Linker: LLD 23.1.1 (somewhere abc)\", force = true }\n",
        ] {
            let error = linker_text_in(bad).expect_err("not a text of its own");
            assert!(error.contains("a plain text of its own"), "{error}");
        }
    }

    #[test]
    fn this_repositorys_fixed_line_names_no_version_and_no_host() {
        let text = linker_text().expect("the entry is in .cargo/config.toml");
        assert!(text.starts_with("Linker: "), "{text}");
        // No number a toolchain upgrade would make false.
        assert!(!text.contains(|c: char| c.is_ascii_digit()), "{text}");
        for host in ["darwin", "linux", "apple", "/Users", "/home", "checkout"] {
            assert!(!text.contains(host), "{text}");
        }
        assert!(linkers_own_line_in(text.as_bytes()).is_none(), "{text}");
    }

    #[test]
    fn an_elfs_comment_must_hold_the_fixed_line_and_not_the_linkers_own() {
        let rustc = b"rustc version 1.99.1 (58cf8f9c3 2026-10-13)\0";
        let good = elf_with_comment(&[b"\0", rustc.as_slice(), FIXED.as_bytes(), b"\0"].concat());
        assert_eq!(linker_text_problem(&good, FIXED), Ok(None));
        assert_eq!(check_linker_text("the kernel ELF", &good, FIXED), Ok(()));
        // The two lines the pinned linker's builds write when nothing fixes them: the Mac's
        // and CI's (the ledger's §13.3).
        for own in [
            "Linker: LLD 23.1.1 (https://github.com/rust-lang/llvm-project.git 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145)",
            "Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145)",
        ] {
            let elf = elf_with_comment(&[own.as_bytes(), b"\0\0", rustc.as_slice()].concat());
            let error = check_linker_text("the kernel ELF", &elf, FIXED).expect_err("its own line");
            assert!(
                error.starts_with("the kernel ELF holds the linker's own line about itself"),
                "{error}"
            );
            assert!(error.contains(own), "{error}");
            assert!(linkers_own_line_in(&elf).is_some());
        }
        // Both lines there: the linker's own is still refused.
        let both =
            elf_with_comment(&[FIXED.as_bytes(), b"\0Linker: LLD 99\0", rustc.as_slice()].concat());
        assert!(linker_text_problem(&both, FIXED).expect("an ELF").is_some());
        // Another fixed line than the configuration's, a line from elsewhere, the compiler's
        // line alone, and no `.comment` at all.
        let other = elf_with_comment(&[b"Linker: another fixed line\0", rustc.as_slice()].concat());
        let problem = linker_text_problem(&other, FIXED)
            .expect("an ELF")
            .expect("refused");
        assert!(
            problem.contains("a line this gate does not know"),
            "{problem}"
        );
        let alone = elf_with_comment(rustc);
        let problem = linker_text_problem(&alone, FIXED)
            .expect("an ELF")
            .expect("refused");
        assert!(problem.contains("does not hold the line"), "{problem}");
        let mut none = elf_with_comment(rustc);
        let names = none
            .windows(8)
            .position(|w| w == b".comment")
            .expect("its name");
        none[names + 1] = b'x';
        let problem = linker_text_problem(&none, FIXED)
            .expect("an ELF")
            .expect("refused");
        assert!(problem.contains("has no `.comment` section"), "{problem}");
        assert!(linkers_own_line_in(&good).is_none());
    }

    #[test]
    fn a_file_linked_before_the_fixed_line_is_removed_and_linked_again_once() {
        use std::cell::{Cell, RefCell};
        use std::collections::BTreeMap;
        let rustc = b"rustc version 1\0".as_slice();
        let fresh = elf_with_comment(&[rustc, FIXED.as_bytes(), b"\0"].concat());
        let old = elf_with_comment(&[b"Linker: LLD 23.1.1 (somewhere abc)\0", rustc].concat());
        let kernel = PathBuf::from("/t/kernel");
        let program = PathBuf::from("/t/program");
        // A directory, and a build that links whatever is missing and leaves the rest alone,
        // as Cargo does. `honoured` is whether the linker writes the fixed line.
        let run = |start: &[(&PathBuf, &Vec<u8>)], honoured: bool| {
            let files: RefCell<BTreeMap<PathBuf, Vec<u8>>> = RefCell::new(
                start
                    .iter()
                    .map(|(path, bytes)| ((*path).clone(), (*bytes).clone()))
                    .collect(),
            );
            let builds = Cell::new(0);
            let outputs = [kernel.clone(), program.clone()];
            let result = relink_stale(
                &outputs,
                FIXED,
                &mut || {
                    builds.set(builds.get() + 1);
                    for path in &outputs {
                        files
                            .borrow_mut()
                            .entry(path.clone())
                            .or_insert_with(|| if honoured { fresh.clone() } else { old.clone() });
                    }
                    Ok(())
                },
                &|path| {
                    files
                        .borrow()
                        .get(path)
                        .cloned()
                        .ok_or_else(|| "missing".to_string())
                },
                &|path| {
                    files
                        .borrow_mut()
                        .remove(path)
                        .map(|_| ())
                        .ok_or_else(|| "missing".to_string())
                },
            );
            (result, builds.get(), files.into_inner())
        };
        // Nothing there: one build, nothing linked again.
        let (result, builds, _) = run(&[], true);
        assert_eq!((result, builds), (Ok(Vec::new()), 1));
        // Both from before the entry: both removed, one more build, both right afterwards.
        let (result, builds, files) = run(&[(&kernel, &old), (&program, &old)], true);
        assert_eq!(result, Ok(vec![kernel.clone(), program.clone()]));
        assert_eq!(builds, 2);
        assert!(files.values().all(|bytes| bytes == &fresh));
        // The program alone from before, the case that booted a kernel one byte off the
        // gate's: it alone is linked again.
        let (result, builds, _) = run(&[(&kernel, &fresh), (&program, &old)], true);
        assert_eq!((result, builds), (Ok(vec![program.clone()]), 2));
        // Where a stale file was built: its target directory and its target. Cargo is then
        // asked to build again the packages that link a file for that target (below). A path
        // of another shape, or of a target no ELF image is linked for, names neither.
        assert_eq!(
            built_into(Path::new("/t/x86_64-unknown-none/release/framekernel")),
            Some((Path::new("/t"), "x86_64-unknown-none"))
        );
        assert_eq!(
            built_into(Path::new(
                "target/xtask/reproducible/a/x86_64-unknown-linux-gnu/release/liborigin_core.so"
            )),
            Some((
                Path::new("target/xtask/reproducible/a"),
                "x86_64-unknown-linux-gnu"
            ))
        );
        for other in [
            "/t/x86_64-unknown-none/debug/framekernel",
            "/t/x86_64-unknown-uefi/release/origin-boot.efi",
            "/t/release/framekernel",
            "release/framekernel",
            "framekernel",
            "",
        ] {
            assert_eq!(built_into(Path::new(other)), None, "{other}");
        }
        // The clean names the packages that link a file for that target, always: without one,
        // `cargo clean` removes the host's own directory and leaves the stale file. An empty
        // list is refused, so no such command can be put together.
        let refused = clean_arguments(Path::new("/t"), KERNEL_TARGET, &[])
            .expect_err("a clean with no package");
        assert!(refused.contains("no package is named"), "{refused}");
        // A relative path with no directory in front gives an empty target directory, and an
        // empty `--target-dir` is refused too.
        let (directory, target) =
            built_into(Path::new("x86_64-unknown-none/release/framekernel")).expect("its shape");
        assert_eq!((directory, target), (Path::new(""), KERNEL_TARGET));
        let refused = clean_arguments(directory, target, &[KERNEL_PACKAGE])
            .expect_err("a clean with no target directory");
        assert!(
            refused.contains("no target directory is named"),
            "{refused}"
        );
        // A file the clean did not remove is said to be still there, with the packages that
        // were cleaned, not said to have been removed.
        let message = still_there(
            Path::new("/t/x86_64-unknown-linux-gnu/release/liborigin_core.so"),
            ring3::USERSPACE_TARGET,
            &["first-program"],
        );
        assert!(
            message
                .contains("liborigin_core.so is still there after `cargo clean` of first-program"),
            "{message}"
        );
        let words = |target: &str, packages: &[&str]| -> Vec<String> {
            clean_arguments(Path::new("/t"), target, packages)
                .expect("packages were named")
                .iter()
                .map(|word| word.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            words(KERNEL_TARGET, &linked_packages(KERNEL_TARGET)),
            [
                "clean",
                "--quiet",
                "--release",
                "--target",
                "x86_64-unknown-none",
                "--target-dir",
                "/t",
                "--package",
                "framekernel"
            ]
        );
        // For the userspace's target: every ring-3 image the kernel carries, each after its
        // own `--package`.
        let carried: Vec<&str> = ring3::carried_packages().collect();
        assert!(carried.contains(&"first-program"));
        assert_eq!(linked_packages(ring3::USERSPACE_TARGET), carried);
        let userspace = words(ring3::USERSPACE_TARGET, &carried);
        assert_eq!(userspace[4], "x86_64-unknown-linux-gnu");
        assert_eq!(userspace.len(), 7 + 2 * carried.len());
        for (pair, package) in userspace[7..].chunks(2).zip(&carried) {
            assert_eq!(pair, ["--package", package]);
        }
        // A linker that does not write the fixed line: an error after one more build, never a
        // third.
        let (result, builds, _) = run(&[(&kernel, &old)], false);
        let error = result.expect_err("still the linker's own line");
        assert!(
            error.contains("removed and linked again, and still does"),
            "{error}"
        );
        assert_eq!(builds, 2);
    }

    #[test]
    fn a_file_that_is_not_an_elf_or_is_cut_short_is_an_error_not_a_pass() {
        let good = elf_with_comment(b"rustc version 1\0Linker: a fixed line\0");
        assert_eq!(linker_text_problem(&good, FIXED), Ok(None));
        for bad in [
            b"MZ\x90\0".to_vec(),
            Vec::new(),
            good[..0x30].to_vec(),
            good[..good.len() - 0x41].to_vec(),
        ] {
            let error = linker_text_problem(&bad, FIXED).expect_err("unreadable");
            assert!(
                error.starts_with("not a 64-bit little-endian ELF"),
                "{error}"
            );
        }
        // A section table that points outside the file.
        let mut outside = good.clone();
        outside[0x28..0x30].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(linker_text_problem(&outside, FIXED).is_err());
        let error = check_linker_text("a file", &[], FIXED).expect_err("unreadable");
        assert!(error.starts_with("a file: not a 64-bit"), "{error}");
    }

    /// Two packages, one inside the other's directory, as `cargo metadata` writes them.
    const METADATA: &str = concat!(
        r#"{"packages":[{"name":"kernel","targets":["#,
        r#"{"kind":["lib"],"crate_types":["lib"],"name":"kernel","src_path":"/w/kernel/src/lib.rs","edition":"2024"},"#,
        r#"{"kind":["custom-build"],"crate_types":["bin"],"name":"build-script-build","src_path":"/w/kernel/build.rs","edition":"2024"}],"#,
        r#""manifest_path":"/w/kernel/Cargo.toml"},{"name":"frame","targets":["#,
        r#"{"kind":["lib"],"crate_types":["lib"],"name":"frame","src_path":"/w/kernel/frame/src/lib.rs","edition":"2024"}],"#,
        r#""manifest_path":"/w/kernel/frame/Cargo.toml"},{"name":"derive","targets":["#,
        r#"{"kind":["proc-macro"],"crate_types":["proc-macro"],"name":"derive","src_path":"/w/derive/src/lib.rs","edition":"2024"}],"#,
        r#""manifest_path":"/w/derive/Cargo.toml"}]}"#
    );

    #[test]
    fn the_targets_built_for_the_host_are_read_from_cargos_own_account() {
        let units = host_units(METADATA).expect("readable");
        assert_eq!(
            units,
            [
                HostUnit {
                    what: "a build script",
                    source: "/w/kernel/build.rs".into()
                },
                HostUnit {
                    what: "a procedural macro",
                    source: "/w/derive/src/lib.rs".into()
                },
            ]
        );
        let packages = package_directories(METADATA).expect("readable");
        assert_eq!(
            packages,
            ["/w/kernel", "/w/kernel/frame", "/w/derive"].map(PathBuf::from)
        );
        // The kernel's script is the kernel's, though the framework's directory is inside
        // the kernel's; a file of the framework is the framework's.
        assert_eq!(
            owner(Path::new("/w/kernel/build.rs"), &packages),
            Some(&PathBuf::from("/w/kernel"))
        );
        assert_eq!(
            owner(Path::new("/w/kernel/frame/src/lib.rs"), &packages),
            Some(&PathBuf::from("/w/kernel/frame"))
        );
        assert_eq!(owner(Path::new("/elsewhere/build.rs"), &packages), None);
        // A target whose source cannot be read is an error, not a target passed over.
        let broken = METADATA.replace(r#""src_path":"/w/kernel/build.rs","#, "");
        assert!(host_units(&broken).is_err());
    }

    #[test]
    fn a_package_with_a_host_target_in_an_images_tree_is_refused_by_name() {
        let units = host_units(METADATA).expect("readable");
        let packages = package_directories(METADATA).expect("readable");
        // The kernel's own script, and a macro two levels down, shown twice by the tree.
        let tree = "kernel v0.0.0 (/w/kernel)\nframe v0.0.0 (/w/kernel/frame)\nderive v0.0.0 (proc-macro) (/w/derive)\nframe v0.0.0 (/w/kernel/frame) (*)\nderive v0.0.0 (proc-macro) (/w/derive) (*)\n";
        let (problems, names) =
            graph_problems("the kernel", tree, &units, &packages).expect("readable");
        assert_eq!(
            problems,
            [
                "the kernel is built from `derive`, which has a procedural macro (/w/derive/src/lib.rs)",
                "the kernel is built from `kernel`, which has a build script (/w/kernel/build.rs)",
            ]
        );
        assert_eq!(names.len(), 3);
        // The framework alone: its directory is inside the kernel's, and the kernel's script
        // is not held against it.
        let (problems, names) = graph_problems(
            "the carried program `p`",
            "frame v0.0.0 (/w/kernel/frame)\n",
            &units,
            &packages,
        )
        .expect("readable");
        assert!(problems.is_empty(), "{problems:?}");
        assert!(names.contains("frame"));
        // A package from outside the workspace has no directory on its line.
        let error = graph_problems("the kernel", "serde v1.0.0\n", &units, &packages)
            .expect_err("no directory");
        assert!(error.contains("names no directory"), "{error}");
    }

    #[test]
    fn every_image_is_looked_at() {
        let images = images();
        let packages: Vec<&str> = images.iter().map(|(_, package, _)| *package).collect();
        assert!(packages.contains(&LOADER_PACKAGE) && packages.contains(&KERNEL_PACKAGE));
        for package in ring3::carried_packages() {
            assert!(packages.contains(&package), "{package}");
        }
        assert_eq!(images.len(), 2 + ring3::carried_packages().count());
    }

    /// The gate itself, on this repository's real `cargo metadata` and `cargo tree`.
    #[test]
    fn no_image_of_this_workspace_is_built_from_a_host_target() {
        assert_eq!(graph_gate(), Ok(()));
    }

    #[test]
    fn this_repositorys_generated_file_is_in_step() {
        let versions = read(VERSIONS).expect("the version file");
        let committed = read(GENERATED).expect("the generated file");
        assert_eq!(version_problem(&versions, &committed), Ok(None));
    }
}
