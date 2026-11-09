//! `cargo xtask check`'s gates on the generated interface code (ADR-0011 N13,
//! `docs/spec/OIDL.md` §13):
//!
//! - **Fresh.** [`COMMITTED`] is byte for byte what the generator writes from [`DEFINITIONS`] and
//!   `origin-version.toml`: no file missing, none added, none changed.
//! - **Recorded.** [`LOCK`] holds the definitions' fingerprints: re-recording it into a scratch
//!   copy changes nothing. A definition's ABI therefore cannot change without the lock changing
//!   in the same commit, where a reviewer sees it, even at major 0, where `oidl` itself only
//!   warns (§9).
//!
//! `cargo xtask regenerate` rewrites [`COMMITTED`]. It never touches the lock: re-recording an ABI
//! is a decision, and the gate names the command for it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{OIDL_PACKAGE, Result, cargo, collect_files, read_file, repo_root, target_dir};

/// The definitions the kernel's system calls are generated from.
const DEFINITIONS: &[&str] = &["sdk/interfaces/origin.kernel.oidl"];
/// Where `version compatibility "..."` is read from.
const VERSIONS: &str = "origin-version.toml";
/// The generated source of the kernel-private crate: nothing here is written by hand.
const COMMITTED: &str = "framekernel/syscalls/src";
/// The recorded fingerprints.
const LOCK: &str = "sdk/interfaces/origin.kernel.lock";

/// A tree of files: relative path to contents.
type Tree = BTreeMap<String, Vec<u8>>;

/// Both gates: see the module documentation.
pub(crate) fn gate() -> Result {
    let scratch = scratch()?;
    let generated = generate(&scratch)?;
    let committed = read_tree(&repo_root().join(COMMITTED))?;
    let stale = differences(&committed, &generated);
    if !stale.is_empty() {
        return Err(format!(
            "{COMMITTED} is not what the generator writes from {} (ADR-0011 N13); run `cargo xtask regenerate`:\n  {}",
            DEFINITIONS.join(", "),
            stale.join("\n  ")
        ));
    }
    recorded(&scratch)?;
    println!(
        "xtask: interface gates: {COMMITTED} is the generator's output, byte for byte ({} files), and {LOCK} records it",
        generated.len()
    );
    Ok(())
}

/// Rewrites [`COMMITTED`] from the definitions, and removes what the generator no longer writes.
/// The generator runs first, into a scratch directory, because the workspace does not load
/// without the crate's `lib.rs`.
pub(crate) fn regenerate() -> Result {
    let generated = generate(&scratch()?)?;
    let target = repo_root().join(COMMITTED);
    for path in read_tree(&target)?
        .keys()
        .filter(|path| !generated.contains_key(*path))
    {
        let file = target.join(path);
        fs::remove_file(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    }
    for (path, text) in &generated {
        let file = target.join(path);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))?;
    }
    println!(
        "xtask: regenerated {COMMITTED} ({} files); if the ABI changed, the lock must be re-recorded too: {}",
        generated.len(),
        record_command()
    );
    Ok(())
}

/// An empty scratch directory.
fn scratch() -> Result<PathBuf> {
    let dir = target_dir().join("xtask").join("interfaces");
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// `oidl`, built from this workspace.
fn oidl() -> Command {
    let mut command = cargo();
    command.args(["run", "--quiet", "--package", OIDL_PACKAGE, "--"]);
    command
}

/// Runs `command`, keeping its output for the error message.
fn run_quietly(mut command: Command, what: &str) -> Result {
    let out = command
        .output()
        .map_err(|e| format!("cannot run {what}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!(
        "{what} failed ({}):\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

/// The server side of the definitions, generated under `scratch`.
fn generate(scratch: &Path) -> Result<Tree> {
    let out = scratch.join("generated");
    let mut command = oidl();
    command
        .args(["generate", "--server-only", "--out"])
        .arg(&out)
        .args(["--versions", VERSIONS])
        .args(DEFINITIONS);
    run_quietly(command, "oidl generate")?;
    let tree = read_tree(&out)?;
    if tree.is_empty() {
        return Err(format!(
            "oidl wrote nothing under {}: nothing was checked",
            out.display()
        ));
    }
    Ok(tree)
}

/// The files under `dir`. Hidden files are not ours (an editor's or the file manager's).
fn read_tree(dir: &Path) -> Result<Tree> {
    let mut paths = Vec::new();
    collect_files(dir, &|_| false, &|name| !name.starts_with('.'), &mut paths)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut tree = Tree::new();
    for path in paths {
        let relative = path
            .strip_prefix(dir)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        tree.insert(
            relative.to_string_lossy().replace('\\', "/"),
            read_file(&path)?,
        );
    }
    Ok(tree)
}

/// Each way `committed` differs from `generated`, by path.
fn differences(committed: &Tree, generated: &Tree) -> Vec<String> {
    let mut found = Vec::new();
    for (path, text) in generated {
        match committed.get(path) {
            None => found.push(format!("{path}: missing")),
            Some(bytes) if bytes != text => found.push(format!(
                "{path}: differs from line {}",
                first_different_line(bytes, text)
            )),
            Some(_) => {}
        }
    }
    for path in committed
        .keys()
        .filter(|path| !generated.contains_key(*path))
    {
        found.push(format!("{path}: the generator does not write this file"));
    }
    found
}

/// The line, counted from 1, of the first byte where `a` and `b` differ.
fn first_different_line(a: &[u8], b: &[u8]) -> usize {
    let at = a
        .iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()));
    1 + a.iter().take(at).filter(|byte| **byte == b'\n').count()
}

/// How a developer re-records the lock.
fn record_command() -> String {
    format!(
        "cargo run --package {OIDL_PACKAGE} -- fingerprint --versions {VERSIONS} --lock {LOCK} --update {}",
        DEFINITIONS.join(" ")
    )
}

/// Fails unless re-recording [`LOCK`] leaves it as it is.
fn recorded(scratch: &Path) -> Result {
    let recorded = read_file(&repo_root().join(LOCK))?;
    let copy = scratch.join("recorded.lock");
    fs::write(&copy, &recorded).map_err(|e| format!("{}: {e}", copy.display()))?;
    let mut command = oidl();
    command
        .args(["fingerprint", "--versions", VERSIONS, "--lock"])
        .arg(&copy)
        .arg("--update")
        .args(DEFINITIONS);
    run_quietly(command, "oidl fingerprint")?;
    if read_file(&copy)? != recorded {
        return Err(format!(
            "the ABI in {} is not the one {LOCK} records (ADR-0011 N13). If the change is meant, record it in the same commit, citing the decision: {}",
            DEFINITIONS.join(", "),
            record_command()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[(&str, &str)]) -> Tree {
        files
            .iter()
            .map(|(path, text)| (path.to_string(), text.as_bytes().to_vec()))
            .collect()
    }

    #[test]
    fn identical_trees_have_no_differences() {
        let a = tree(&[("lib.rs", "one\n"), ("sub/mod.rs", "two\n")]);
        assert!(differences(&a, &a).is_empty());
    }

    #[test]
    fn a_missing_an_added_and_a_changed_file_are_each_named() {
        let generated = tree(&[
            ("gone.rs", "x\n"),
            ("same.rs", "y\n"),
            ("edited.rs", "a\nb\n"),
        ]);
        let committed = tree(&[
            ("same.rs", "y\n"),
            ("edited.rs", "a\nc\n"),
            ("mine.rs", "z\n"),
        ]);
        assert_eq!(
            differences(&committed, &generated),
            [
                "edited.rs: differs from line 2",
                "gone.rs: missing",
                "mine.rs: the generator does not write this file",
            ]
        );
    }

    #[test]
    fn a_file_that_only_got_longer_differs_where_it_ends() {
        assert_eq!(first_different_line(b"a\nb\n", b"a\nb\nc\n"), 3);
        assert_eq!(first_different_line(b"", b"a"), 1);
    }

    /// The real tree: the gate's paths name what is there, and the kernel crate holds no shims.
    #[test]
    fn the_paths_name_the_real_tree() {
        let root = repo_root();
        let committed = read_tree(&root.join(COMMITTED)).expect("the committed tree");
        assert!(committed.contains_key("lib.rs"), "{:?}", committed.keys());
        assert!(
            committed.keys().any(|path| path.ends_with("/server.rs")),
            "{:?}",
            committed.keys()
        );
        for shim in ["abi.rs", "core_stubs.rs", "client.rs"] {
            assert!(
                !committed.keys().any(|path| path.ends_with(shim)),
                "{shim} must not be generated into the kernel's crate"
            );
        }
        assert!(root.join(LOCK).is_file());
        for definition in DEFINITIONS {
            assert!(root.join(definition).is_file(), "{definition}");
        }
    }
}
