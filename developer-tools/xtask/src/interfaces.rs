//! `cargo xtask check`'s gates on the generated interface code (ADR-0011 N13,
//! `docs/spec/OIDL.md` §13), for each crate in [`CRATES`]:
//!
//! - **Fresh.** The crate's committed source is byte for byte what the generator writes from its
//!   definitions and `origin-version.toml`: no file missing, none added, none changed.
//! - **Recorded.** Its lock holds its library's fingerprints: re-recording it, in memory,
//!   changes nothing. A definition's ABI therefore cannot change without the lock changing
//!   in the same commit, where a reviewer sees it, even at major 0, where `oidl` itself only
//!   warns (§9).
//!
//! `cargo xtask regenerate` rewrites every crate's source. It never touches a lock: re-recording an
//! ABI is a decision, and the gate names the command for it.
//!
//! The generator runs in process, through the `oidl` library, as the ring-3 build's does
//! (ADR-0011 Amendment 4, T1): `xtask` starts no first-party program. Each definition is named
//! by its path from the repository's root, as the command line was given it, so whatever the
//! generator writes of its source's name is the same.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use oidl::codegen::{self, Output};
use oidl::model::Compilation;

use crate::{OIDL_PACKAGE, Result, collect_files, read_file, repo_root};

/// Where `version compatibility "..."` is read from.
const VERSIONS: &str = "origin-version.toml";

/// A crate generated from OIDL definitions, and the lock that records its library.
struct Generated {
    /// The library the crate is for and the lock records.
    library: &'static str,
    /// The definitions given to the generator: the library's, and every library it uses.
    definitions: &'static [&'static str],
    /// What kind of crate (`docs/spec/OIDL.md` §11).
    mode: Mode,
    /// Its source: nothing here is written by hand.
    committed: &'static str,
    /// The library's recorded fingerprints.
    lock: &'static str,
}

/// What kind of crate the generator writes (`docs/spec/OIDL.md` §11): `oidl generate`'s
/// `--server-only` or `--library <name>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// The server side alone.
    ServerOnly,
    /// One `channel` library's bindings.
    Library(&'static str),
}

impl Mode {
    fn output(self) -> Output {
        match self {
            Self::ServerOnly => Output::ServerOnly,
            Self::Library(name) => Output::Library(name.to_string()),
        }
    }
}

/// Every generated crate.
const CRATES: &[Generated] = &[
    // The kernel's system calls: the server side alone, kernel-private.
    Generated {
        library: "origin.kernel",
        definitions: &["sdk/interfaces/origin.kernel.oidl"],
        mode: Mode::ServerOnly,
        committed: "framekernel/syscalls/src",
        lock: "sdk/interfaces/origin.kernel.lock",
    },
    // The service directory (ADR-0005 C12): its server and client, and origin.kernel's types.
    Generated {
        library: "origin.directory",
        definitions: &[
            "sdk/interfaces/origin.kernel.oidl",
            "sdk/interfaces/origin.directory.oidl",
        ],
        mode: Mode::Library("origin.directory"),
        committed: "sdk/directory/src",
        lock: "sdk/interfaces/origin.directory.lock",
    },
];

/// A tree of files: relative path to contents.
type Tree = BTreeMap<String, Vec<u8>>;

/// Both gates on every crate: see the module documentation.
pub(crate) fn gate() -> Result {
    for krate in CRATES {
        let generated = generate(krate)?;
        let committed = read_tree(&repo_root().join(krate.committed))?;
        let stale = differences(&committed, &generated);
        if !stale.is_empty() {
            return Err(format!(
                "{} is not what the generator writes from {} (ADR-0011 N13); run `cargo xtask regenerate`:\n  {}",
                krate.committed,
                krate.definitions.join(", "),
                stale.join("\n  ")
            ));
        }
        recorded(krate)?;
        println!(
            "xtask: interface gates: {} is the generator's output, byte for byte ({} files), and {} records {}",
            krate.committed,
            generated.len(),
            krate.lock,
            krate.library
        );
    }
    Ok(())
}

/// Rewrites every crate's source from its definitions, and removes what the generator no longer
/// writes. The whole crate is generated before anything is written, because the workspace does
/// not load without each crate's `lib.rs`.
pub(crate) fn regenerate() -> Result {
    for krate in CRATES {
        let generated = generate(krate)?;
        let target = repo_root().join(krate.committed);
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
            "xtask: regenerated {} ({} files); if the ABI changed, the lock must be re-recorded too: {}",
            krate.committed,
            generated.len(),
            record_command(krate)
        );
    }
    Ok(())
}

/// The definitions, each read from the repository's root and named by the path given, compiled
/// with the `oidl` library against `origin-version.toml`: what `oidl ... --versions
/// origin-version.toml <definitions>` compiled, run from the root.
pub(crate) fn compile(definitions: &[&str]) -> Result<Compilation> {
    let root = repo_root();
    let mut inputs = Vec::new();
    for definition in definitions {
        inputs.push(oidl::Input {
            file: (*definition).to_string(),
            text: read_file(&root.join(definition))?,
        });
    }
    let versions = String::from_utf8(read_file(&root.join(VERSIONS))?)
        .map_err(|_| format!("{VERSIONS}: not UTF-8"))?;
    oidl::compile(&inputs, Some(&versions)).map_err(|e| format!("oidl: {e}"))
}

/// `krate` as the generator writes it: the `oidl` library in process, what `oidl generate
/// <mode> --versions origin-version.toml <definitions>` wrote.
fn generate(krate: &Generated) -> Result<Tree> {
    let compilation = compile(krate.definitions)?;
    let tree: Tree = codegen::generate_output(&compilation, &krate.mode.output())
        .map_err(|e| format!("oidl generate: {e}"))?
        .into_iter()
        .map(|file| (file.path, file.text.into_bytes()))
        .collect();
    if tree.is_empty() {
        return Err(format!(
            "oidl generated nothing for {}: nothing was checked",
            krate.committed
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

/// How a developer re-records `krate`'s lock.
fn record_command(krate: &Generated) -> String {
    format!(
        "cargo run --package {OIDL_PACKAGE} -- fingerprint --library {} --versions {VERSIONS} --lock {} --update {}",
        krate.library,
        krate.lock,
        krate.definitions.join(" ")
    )
}

/// Fails unless re-recording `krate`'s lock leaves it as it is: the lock `oidl fingerprint
/// --library <library> --update` would write (`oidl::fingerprint::updated_lock`) is its text,
/// byte for byte.
fn recorded(krate: &Generated) -> Result {
    let lock = repo_root().join(krate.lock);
    let recorded =
        String::from_utf8(read_file(&lock)?).map_err(|_| format!("{}: not UTF-8", krate.lock))?;
    let library = compile(krate.definitions)?.only(krate.library)?;
    let refused = |why: String| {
        format!(
            "the ABI of {} is not the one {} records (ADR-0011 N13): {why}. If the change is meant, record it in the same commit, citing the decision: {}",
            krate.library,
            krate.lock,
            record_command(krate)
        )
    };
    match oidl::fingerprint::updated_lock(&library, &recorded) {
        Err(report) => Err(refused(
            report
                .errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        )),
        Ok(text) if text != recorded => {
            Err(refused("re-recording it changes the lock".to_string()))
        }
        Ok(_) => Ok(()),
    }
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

    /// The real tree: each crate's paths name what is there, no crate holds a shim, and the
    /// kernel's crate holds no client either.
    #[test]
    fn the_paths_name_the_real_tree() {
        let root = repo_root();
        for krate in CRATES {
            let committed = read_tree(&root.join(krate.committed)).expect("the committed tree");
            assert!(committed.contains_key("lib.rs"), "{:?}", committed.keys());
            assert!(
                committed.keys().any(|path| path.ends_with("/server.rs")),
                "{:?}",
                committed.keys()
            );
            let server_only = krate.mode == Mode::ServerOnly;
            for shim in ["abi.rs", "core_stubs.rs", "client.rs"] {
                if shim == "client.rs" && !server_only {
                    continue;
                }
                assert!(
                    !committed.keys().any(|path| path.ends_with(shim)),
                    "{shim} must not be generated into {}",
                    krate.committed
                );
            }
            assert!(root.join(krate.lock).is_file(), "{}", krate.lock);
            for definition in krate.definitions {
                assert!(root.join(definition).is_file(), "{definition}");
            }
        }
    }
}
