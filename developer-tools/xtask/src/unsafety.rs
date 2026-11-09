//! `cargo xtask check`'s gate on `unsafe` in the kernel (ADR-0011 N6 and N13): only the
//! framework crate may use it, and every other kernel crate builds on the framework, never
//! the reverse.
//!
//! - The kernel is the crates under [`KERNEL_ROOT`] and the leaf crates it links
//!   ([`KERNEL_LEAVES`]). Anywhere in them but under [`FRAMEWORK`], a file with an `unsafe`
//!   token (or a `global_asm!`, which needs no `unsafe` keyword) fails the gate.
//! - The kernel's library crate root carries `#![forbid(unsafe_code)]`, which the compiler then
//!   enforces for every module above the framework. (The kernel image's link root holds `_start`,
//!   so it lives under [`FRAMEWORK`].)
//! - Dependencies run one way, per [`LAYERS`]: the framework depends only on the leaf crates
//!   named there, and no crate below it depends on the framework or on what is above it.
//!
//! A "site" here is one `unsafe` token (or `global_asm!`) outside comments, strings and
//! character literals, whatever it heads: a block, an `unsafe fn`, an `unsafe impl`, an
//! `unsafe extern` or an attribute such as `#[unsafe(no_mangle)]`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::{Result, cargo, collect_files, repo_root};

/// Under this directory is the privileged framework, the one place `unsafe` may appear: the
/// crate `origin-frame`, and its lock package, `origin-sync`, kept as a package of its own
/// because the Loom models build it alone.
pub(crate) const FRAMEWORK: &str = "framekernel/frame";
/// The directory holding the rest of the kernel.
const KERNEL_ROOT: &str = "framekernel";
/// Leaf crates outside [`KERNEL_ROOT`] that the kernel links (the rest of the workspace is
/// the UEFI loader and host tools, N12's listed exceptions).
const KERNEL_LEAVES: &[&str] = &["boot/info", "runtime/abi", "runtime/version"];
/// The kernel library's crate root, whose `forbid(unsafe_code)` covers every module above the
/// framework.
const KERNEL_LIB_ROOT: &str = "framekernel/src/lib.rs";

/// What each kernel crate may depend on, directly or not (normal edges, packages in this
/// workspace). Every package under `framekernel/` must be listed: a crate of unknown place
/// in the layering fails the gate. `origin-frame` is the framework: the crates it may use
/// are the leaves below it, and everything that is the kernel proper sits above it.
const LAYERS: &[(&str, &[&str])] = &[
    ("origin-version", &[]),
    ("origin-boot-info", &["origin-version"]),
    ("origin-abi", &["origin-version"]),
    ("origin-sync", &[]),
    ("origin-syscalls", &[]),
    ("origin-paging", &["origin-boot-info", "origin-version"]),
    (
        "origin-frame",
        &[
            "origin-abi",
            "origin-boot-info",
            "origin-paging",
            "origin-sync",
            "origin-version",
        ],
    ),
    (
        "framekernel",
        &[
            "origin-abi",
            "origin-boot-info",
            "origin-frame",
            "origin-paging",
            "origin-sync",
            "origin-syscalls",
            "origin-version",
        ],
    ),
];

/// The kernel crates that sit directly above the framework: they must depend on it.
const ABOVE_THE_FRAMEWORK: &[&str] = &["framekernel"];
/// Only the framework may use the lock package directly: everything else takes it through
/// `origin_frame::sync`.
const FRAMEWORK_PACKAGE: &str = "origin-frame";
const LOCK_PACKAGE: &str = "origin-sync";
/// What the kernel library's crate root must carry.
const FORBID: &str = "#![forbid(unsafe_code)]";

/// The gate: see the module documentation.
pub(crate) fn gate() -> Result {
    let root = repo_root();
    let (outside, framework_sites, scanned) = scan(&root)?;
    let problems = site_problems(&outside, framework_sites);
    if !problems.is_empty() {
        return Err(format!(
            "`unsafe` outside the framework (ADR-0011 N6, N13):\n  {}",
            problems.join("\n  ")
        ));
    }
    let text = read(&root.join(KERNEL_LIB_ROOT))?;
    if !text.contains(FORBID) {
        return Err(format!(
            "{KERNEL_LIB_ROOT} no longer carries `{FORBID}` (ADR-0011 N6)"
        ));
    }
    let problems = dependency_problems(&root)?;
    if !problems.is_empty() {
        return Err(format!(
            "a kernel dependency runs the wrong way (ADR-0011 N6, N13):\n  {}",
            problems.join("\n  ")
        ));
    }
    println!(
        "xtask: unsafe gate: no `unsafe` outside the framework ({framework_sites} sites in it, {scanned} kernel source files scanned)"
    );
    Ok(())
}

/// Every kernel source file's `unsafe` count: those outside the framework by repository
/// path, the framework's total, and how many files were read.
fn scan(root: &Path) -> Result<(BTreeMap<String, usize>, usize, usize)> {
    let mut files = Vec::new();
    let framework = root.join(FRAMEWORK);
    for dir in std::iter::once(KERNEL_ROOT).chain(KERNEL_LEAVES.iter().copied()) {
        collect_files(
            &root.join(dir),
            &|_| false,
            &|name| name.ends_with(".rs"),
            &mut files,
        )
        .map_err(|e| format!("unsafe gate: {dir}: {e}"))?;
    }
    if files.is_empty() {
        return Err(format!(
            "unsafe gate: no Rust files under {KERNEL_ROOT}: nothing was checked"
        ));
    }
    let mut outside = BTreeMap::new();
    let mut framework_sites = 0;
    for file in &files {
        let text = fs::read_to_string(file)
            .map_err(|e| format!("unsafe gate: {}: {e}", file.display()))?;
        let count = count_sites(&text);
        if file.starts_with(&framework) {
            framework_sites += count;
        } else if count > 0 {
            let relative = file
                .strip_prefix(root)
                .map_err(|e| format!("unsafe gate: {}: {e}", file.display()))?;
            outside.insert(relative.to_string_lossy().into_owned(), count);
        }
    }
    Ok((outside, framework_sites, files.len()))
}

/// Counts `unsafe` tokens and `global_asm!` invocations in `source`, outside comments, strings
/// and character literals. Raw identifiers (`r#unsafe`) are not the keyword and do not count.
pub(crate) fn count_sites(source: &str) -> usize {
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut count = 0;
    while let Some(&byte) = bytes.get(i) {
        match byte {
            b'/' if bytes.get(i + 1) == Some(&b'/') => i = line_end(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = block_comment_end(bytes, i + 2),
            b'"' => i = string_end(bytes, i + 1),
            b'\'' => i = char_or_lifetime_end(bytes, i),
            b'_' | b'a'..=b'z' | b'A'..=b'Z' => {
                let (word, end) = identifier(bytes, i);
                match word {
                    "r" | "br" | "cr" if raw_string_start(bytes, end).is_some() => {
                        i = raw_string_end(bytes, end).unwrap_or(bytes.len());
                    }
                    "b" | "c" if bytes.get(end) == Some(&b'"') => {
                        i = string_end(bytes, end + 1);
                    }
                    "b" if bytes.get(end) == Some(&b'\'') => {
                        i = char_or_lifetime_end(bytes, end);
                    }
                    "r" if bytes.get(end) == Some(&b'#') => {
                        // A raw identifier: `r#unsafe` names an ordinary identifier.
                        let (_, after) = identifier(bytes, end + 1);
                        i = after;
                    }
                    _ => {
                        if word == "unsafe"
                            || (word == "global_asm" && bytes.get(end) == Some(&b'!'))
                        {
                            count += 1;
                        }
                        i = end;
                    }
                }
            }
            _ => i += 1,
        }
    }
    count
}

fn is_identifier_byte(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
}

/// The identifier starting at `start` and the index just after it.
fn identifier(bytes: &[u8], start: usize) -> (&str, usize) {
    let mut end = start;
    while bytes.get(end).copied().is_some_and(is_identifier_byte) {
        end += 1;
    }
    // Only ASCII bytes were accepted, so this cannot fail.
    (core::str::from_utf8(&bytes[start..end]).unwrap_or(""), end)
}

fn line_end(bytes: &[u8], mut i: usize) -> usize {
    while bytes.get(i).is_some_and(|&b| b != b'\n') {
        i += 1;
    }
    i
}

/// The index after the nested block comment whose body starts at `i` (after `/*`).
fn block_comment_end(bytes: &[u8], mut i: usize) -> usize {
    let mut depth = 1;
    while depth > 0 && i < bytes.len() {
        if bytes.get(i) == Some(&b'/') && bytes.get(i + 1) == Some(&b'*') {
            depth += 1;
            i += 2;
        } else if bytes.get(i) == Some(&b'*') && bytes.get(i + 1) == Some(&b'/') {
            depth -= 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    i
}

/// The index after the string whose body starts at `i` (after its opening quote).
fn string_end(bytes: &[u8], mut i: usize) -> usize {
    while let Some(&byte) = bytes.get(i) {
        match byte {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    i
}

/// The hash count if `at` starts a raw string's delimiter (`#*"`).
fn raw_string_start(bytes: &[u8], at: usize) -> Option<usize> {
    let mut hashes = 0;
    while bytes.get(at + hashes) == Some(&b'#') {
        hashes += 1;
    }
    (bytes.get(at + hashes) == Some(&b'"')).then_some(hashes)
}

/// The index after the raw string whose delimiter starts at `at`.
fn raw_string_end(bytes: &[u8], at: usize) -> Option<usize> {
    let hashes = raw_string_start(bytes, at)?;
    let mut i = at + hashes + 1;
    while i < bytes.len() {
        if bytes.get(i) == Some(&b'"') && (0..hashes).all(|k| bytes.get(i + 1 + k) == Some(&b'#')) {
            return Some(i + 1 + hashes);
        }
        i += 1;
    }
    Some(bytes.len())
}

/// The index after a character literal, or after the `'` if this is a lifetime. A character
/// literal is either escaped (`'\…'`) or has a closing quote within a code point (`'x'`,
/// up to four bytes of UTF-8 between the quotes).
fn char_or_lifetime_end(bytes: &[u8], at: usize) -> usize {
    let start = at + 1;
    match bytes.get(start) {
        Some(b'\\') => {
            let mut i = start + 2;
            // Skip to the closing quote: an escape may be longer than one byte.
            while let Some(&byte) = bytes.get(i) {
                if byte == b'\'' {
                    return i + 1;
                }
                i += 1;
            }
            i
        }
        Some(_) => match (1..=4).find(|n| bytes.get(start + n) == Some(&b'\'')) {
            Some(n) => start + n + 1,
            None => start,
        },
        None => start,
    }
}

/// What is wrong with the `unsafe` the scan found: any of it outside the framework, or none
/// of it inside.
fn site_problems(outside: &BTreeMap<String, usize>, framework_sites: usize) -> Vec<String> {
    let mut problems = Vec::new();
    if framework_sites == 0 {
        // The gate would otherwise read as passing if it were looking in the wrong place.
        problems.push(format!(
            "the framework ({FRAMEWORK}) has no `unsafe` at all: the gate's roots are wrong"
        ));
    }
    for (path, count) in outside {
        problems.push(format!(
            "{path}: {count} `unsafe` sites (move them into {FRAMEWORK})"
        ));
    }
    problems
}

fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Each package named by a `Cargo.toml` under [`KERNEL_ROOT`].
fn kernel_packages(root: &Path) -> Result<BTreeSet<String>> {
    let mut manifests = Vec::new();
    collect_files(
        &root.join(KERNEL_ROOT),
        &|_| false,
        &|name| name == "Cargo.toml",
        &mut manifests,
    )
    .map_err(|e| format!("unsafe gate: {KERNEL_ROOT}: {e}"))?;
    let mut names = BTreeSet::new();
    for manifest in &manifests {
        let text = read(manifest)?;
        let name = text
            .lines()
            .find_map(|line| line.strip_prefix("name = "))
            .map(|value| value.trim_matches('"').to_string())
            .ok_or_else(|| format!("{}: no package name", manifest.display()))?;
        names.insert(name);
    }
    Ok(names)
}

/// The dependency direction, from `cargo tree` (normal edges, every target and feature).
fn dependency_problems(root: &Path) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    let layered: BTreeSet<&str> = LAYERS.iter().map(|(name, _)| *name).collect();
    for name in kernel_packages(root)? {
        if !layered.contains(name.as_str()) {
            problems.push(format!(
                "{name} has no entry in xtask's LAYERS table: say where it sits"
            ));
        }
    }
    for &(package, allowed) in LAYERS {
        let tree = dependencies_of(package)?;
        for dependency in tree.iter().filter(|d| !allowed.contains(&d.as_str())) {
            problems.push(format!(
                "{package} depends on {dependency}, which its layer does not allow"
            ));
        }
        if ABOVE_THE_FRAMEWORK.contains(&package) && !tree.contains(FRAMEWORK_PACKAGE) {
            problems.push(format!("{package} does not depend on {FRAMEWORK_PACKAGE}"));
        }
        // The lock package is the framework's: everyone else uses `origin_frame::sync`.
        if package != FRAMEWORK_PACKAGE && direct_dependencies_of(package)?.contains(LOCK_PACKAGE) {
            problems.push(format!(
                "{package} names {LOCK_PACKAGE} directly: use `origin_frame::sync`"
            ));
        }
    }
    Ok(problems)
}

/// Every package `package` depends on, directly or not.
fn dependencies_of(package: &str) -> Result<BTreeSet<String>> {
    tree_lines(package, None)
}

/// The packages `package` names in its own manifest.
fn direct_dependencies_of(package: &str) -> Result<BTreeSet<String>> {
    tree_lines(package, Some("1"))
}

fn tree_lines(package: &str, depth: Option<&str>) -> Result<BTreeSet<String>> {
    let mut command = cargo();
    command.args([
        "tree",
        "--package",
        package,
        "--prefix",
        "none",
        "--edges",
        "normal",
        "--target",
        "all",
        "--all-features",
    ]);
    if let Some(depth) = depth {
        command.args(["--depth", depth]);
    }
    let output = command
        .output()
        .map_err(|e| format!("cannot run cargo tree: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo tree failed for {package}: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let tree = String::from_utf8_lossy(&output.stdout);
    let names: BTreeSet<String> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| *name != package)
        .map(str::to_string)
        .collect();
    // `cargo tree` prints the package itself first: an empty result means it saw nothing.
    if tree.trim().is_empty() {
        return Err(format!("cargo tree saw nothing for {package}"));
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_the_keyword_and_global_asm_in_code() {
        let source = r#"
            unsafe fn f() {}
            pub unsafe extern "C" fn g() {}
            #[unsafe(no_mangle)] extern "C" fn h() {}
            unsafe impl Send for S {}
            fn i() { unsafe { asm!("nop") } }
            global_asm!("nop");
        "#;
        assert_eq!(count_sites(source), 6);
    }

    #[test]
    fn counts_nothing_in_comments_strings_or_chars() {
        let source = r##"
            // an unsafe line comment
            //! unsafe in docs
            /// and unsafe in docs
            /* unsafe, /* nested unsafe */ still unsafe */
            let s = "unsafe in a string, with an escaped \" unsafe quote";
            let r = r#"unsafe raw"unsafe"#;
            let b = b"unsafe bytes";
            let br = br#"unsafe bytes"#;
            let c = c"unsafe";
            let ch = 'u';
            let q = '"';
            let e = '\'';
            let g = global_asm_like; // no bang
            fn r#unsafe() {}
        "##;
        assert_eq!(count_sites(source), 0, "{source}");
    }

    #[test]
    fn lifetimes_are_not_character_literals() {
        // A lifetime must not swallow the code after it.
        assert_eq!(count_sites("fn f<'a>(x: &'a u8) { unsafe {} }"), 1);
        assert_eq!(count_sites("let c = 'x'; unsafe {}"), 1);
    }

    #[test]
    fn a_word_containing_unsafe_is_not_the_keyword() {
        assert_eq!(count_sites("let unsafely = 1; let not_unsafe = 2;"), 0);
    }

    fn map(entries: &[(&str, usize)]) -> BTreeMap<String, usize> {
        entries.iter().map(|&(p, c)| (p.to_string(), c)).collect()
    }

    #[test]
    fn unsafe_outside_the_framework_fails_and_a_clean_tree_passes() {
        assert!(site_problems(&map(&[]), 5).is_empty());
        let found = site_problems(&map(&[("a.rs", 2)]), 5);
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("a.rs: 2"), "{found:?}");
        // A framework with nothing in it means the gate is looking in the wrong place.
        let empty = site_problems(&map(&[]), 0);
        assert!(empty[0].contains("gate's roots are wrong"), "{empty:?}");
    }

    /// The real tree, so the gate's roots are known to find the real files.
    #[test]
    fn the_gate_sees_the_real_tree() {
        let root = repo_root();
        let (_, framework_sites, scanned) = scan(&root).expect("the scan");
        assert!(framework_sites > 0, "the framework has no unsafe");
        assert!(scanned > 50, "only {scanned} files scanned");
        // Nothing is left outside the framework, and the library's root says so.
        let (outside, _, _) = scan(&root).expect("the scan");
        assert!(outside.is_empty(), "{outside:?}");
        assert!(
            read(&root.join(KERNEL_LIB_ROOT))
                .expect("the root")
                .contains(FORBID)
        );
        let names = kernel_packages(&root).expect("the packages");
        assert!(names.contains("origin-frame") && names.contains("framekernel"));
    }
}
