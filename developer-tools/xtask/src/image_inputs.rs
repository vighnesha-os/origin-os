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
