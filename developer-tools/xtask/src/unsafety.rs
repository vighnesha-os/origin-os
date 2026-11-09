//! `cargo xtask check`'s gate on `unsafe` in the kernel (ADR-0011 N6 and N13): only the
//! framework crate may use it, and every other kernel crate builds on the framework, never
//! the reverse.
//!
//! - The kernel is the crates under [`KERNEL_ROOT`] and the leaf crates it links
//!   ([`KERNEL_LEAVES`]). Anywhere in them but under [`FRAMEWORK`], a file with an `unsafe`
//!   token (or a `global_asm!`, which needs no `unsafe` keyword) fails the gate, unless the
//!   file is on the allowlist with exactly that count: [`ALLOWLIST`] names the files not
//!   yet migrated. A file that reaches zero must leave the list; a count that drops must
//!   be lowered in the list in the same change.
//! - The list can only shrink. That is checked against its own git history: no later
//!   revision (nor the working tree) may name a file an earlier one did not, or give a
//!   file a higher count. So raising an entry needs an edit of this rule, not of the list.
//! - A module that has left the list must say so itself: it carries
//!   `#![forbid(unsafe_code)]`, which the compiler then enforces, or sits in a crate
//!   whose root does. The kernel's own crate root (`main.rs`, the link root) is the
//!   exception and is checked for the absence of the token instead.
//! - Dependencies run one way, per [`LAYERS`]: the framework depends only on the leaf crates
//!   named there, and no crate below it depends on the framework or on what is above it.
//!
//! A "site" here is one `unsafe` token (or `global_asm!`) outside comments, strings and
//! character literals, whatever it heads: a block, an `unsafe fn`, an `unsafe impl`, an
//! `unsafe extern` or an attribute such as `#[unsafe(no_mangle)]`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
/// The kernel binary's crate root: the link root, which the framework's entry macro will
/// expand into. It is held to no `unsafe` token instead of `forbid(unsafe_code)`.
const KERNEL_BIN_ROOT: &str = "framekernel/src/main.rs";
/// The kernel library's crate root, whose `forbid(unsafe_code)` covers its modules.
const KERNEL_LIB_ROOT: &str = "framekernel/src/lib.rs";
/// The modules not yet migrated, with the number of `unsafe` sites each still has.
pub(crate) const ALLOWLIST: &str = "developer-tools/xtask/unsafe-allowlist.txt";

/// What each kernel crate may depend on, directly or not (normal edges, packages in this
/// workspace). Every package under `framekernel/` must be listed: a crate of unknown place
/// in the layering fails the gate. `origin-frame` is the framework: the crates it may use
/// are the leaves below it, and everything that is the kernel proper sits above it.
const LAYERS: &[(&str, &[&str])] = &[
    ("origin-version", &[]),
    ("origin-boot-info", &["origin-version"]),
    ("origin-abi", &["origin-version"]),
    ("origin-sync", &[]),
    ("origin-paging", &["origin-boot-info", "origin-version"]),
    (
        "origin-frame",
        &[
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

/// The gate: see the module documentation.
pub(crate) fn gate() -> Result {
    let root = repo_root();
    let list = read_allowlist(&root)?;
    let (outside, framework_sites, scanned) = scan(&root)?;
    let problems = allowlist_problems(&outside, &list, framework_sites);
    if !problems.is_empty() {
        return Err(format!(
            "`unsafe` outside the framework (ADR-0011 N6, N13):\n  {}",
            problems.join("\n  ")
        ));
    }
    check_ratchet(&root, &list)?;
    let problems = module_forbid_problems(&root, &list)?;
    if !problems.is_empty() {
        return Err(format!(
            "a migrated module must carry `#![forbid(unsafe_code)]` (ADR-0011 N6):\n  {}",
            problems.join("\n  ")
        ));
    }
    let problems = dependency_problems(&root)?;
    if !problems.is_empty() {
        return Err(format!(
            "a kernel dependency runs the wrong way (ADR-0011 N6, N13):\n  {}",
            problems.join("\n  ")
        ));
    }
    let remaining: usize = outside.values().sum();
    println!(
        "xtask: unsafe gate: {remaining} `unsafe` sites remain outside the framework in {} allowlisted files ({framework_sites} in the framework, {scanned} kernel source files scanned)",
        outside.len()
    );
    Ok(())
}

/// `xtask unsafe-list`: prints the file's up-to-date body (one `COUNT PATH` line per file with
/// `unsafe` outside the framework), for updating [`ALLOWLIST`] after a migration.
pub(crate) fn print_list() -> Result {
    let root = repo_root();
    let (outside, _, _) = scan(&root)?;
    for (path, count) in &outside {
        println!("{count} {path}");
    }
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

/// The allowlist's entries, by path.
fn read_allowlist(root: &Path) -> Result<BTreeMap<String, usize>> {
    let path = root.join(ALLOWLIST);
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_allowlist(&text).map_err(|e| format!("{ALLOWLIST}: {e}"))
}

fn parse_allowlist(text: &str) -> Result<BTreeMap<String, usize>> {
    let mut entries = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        let (Some(count), Some(path), None) = (words.next(), words.next(), words.next()) else {
            return Err(format!("line {}: expected `COUNT PATH`", n + 1));
        };
        let count: usize = count
            .parse()
            .map_err(|_| format!("line {}: bad count {count}", n + 1))?;
        if entries.insert(path.to_string(), count).is_some() {
            return Err(format!("line {}: {path} is listed twice", n + 1));
        }
    }
    Ok(entries)
}

/// What is wrong with the allowlist against the source: files with `unsafe` that are not
/// listed, files listed with another count, and entries that have gone stale.
fn allowlist_problems(
    outside: &BTreeMap<String, usize>,
    list: &BTreeMap<String, usize>,
    framework_sites: usize,
) -> Vec<String> {
    let mut problems = Vec::new();
    if framework_sites == 0 {
        // The gate would otherwise read as passing if it were looking in the wrong place.
        problems.push(format!(
            "the framework ({FRAMEWORK}) has no `unsafe` at all: the gate's roots are wrong"
        ));
    }
    for (path, &count) in outside {
        match list.get(path) {
            None => problems.push(format!(
                "{path}: {count} `unsafe` sites, and it is not on the allowlist (move them into {FRAMEWORK})"
            )),
            Some(&listed) if listed != count => problems.push(format!(
                "{path}: the allowlist says {listed}, the file has {count}; a count may only go down, and the list must say so"
            )),
            Some(_) => {}
        }
    }
    for path in list.keys().filter(|path| !outside.contains_key(*path)) {
        problems.push(format!(
            "{path}: on the allowlist but has no `unsafe` left (or no longer exists): take it off the list and add `#![forbid(unsafe_code)]`"
        ));
    }
    problems
}

/// Whether `later` only shrinks `earlier`: nothing new, nothing bigger.
fn shrinks(earlier: &BTreeMap<String, usize>, later: &BTreeMap<String, usize>) -> Result {
    for (path, &count) in later {
        match earlier.get(path) {
            None => {
                return Err(format!(
                    "{path} was added to the allowlist, which can only shrink"
                ));
            }
            Some(&before) if count > before => {
                return Err(format!(
                    "{path} went from {before} to {count} on the allowlist, which can only shrink"
                ));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git {}: {e}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed ({}): {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Reads the allowlist's whole history and checks that it only ever shrank.
fn check_ratchet(root: &Path, current: &BTreeMap<String, usize>) -> Result {
    let log = git(root, &["log", "--format=%H", "--", ALLOWLIST])?;
    let revisions: Vec<&str> = log.lines().collect();
    // Oldest first.
    let mut previous: Option<(String, BTreeMap<String, usize>)> = None;
    for revision in revisions.iter().rev() {
        let spec = format!("{revision}:{ALLOWLIST}");
        let text = git(root, &["show", &spec])?;
        let entries =
            parse_allowlist(&text).map_err(|e| format!("{ALLOWLIST} at {revision}: {e}"))?;
        if let Some((before, earlier)) = &previous {
            shrinks(earlier, &entries).map_err(|e| {
                format!("{ALLOWLIST}: commit {revision} grew it (against {before}): {e}")
            })?;
        }
        previous = Some(((*revision).to_string(), entries));
    }
    // The working tree against the last commit: an uncommitted edit that grew the list.
    if let Some((revision, committed)) = previous {
        shrinks(&committed, current).map_err(|e| {
            format!("{ALLOWLIST}: the working tree grew it (against {revision}): {e}")
        })?;
    }
    Ok(())
}

/// Every module of the kernel binary and the library's crate root must forbid `unsafe_code`,
/// except the modules still on the allowlist (and the link root, held to no `unsafe` token).
fn module_forbid_problems(root: &Path, list: &BTreeMap<String, usize>) -> Result<Vec<String>> {
    const FORBID: &str = "#![forbid(unsafe_code)]";
    let mut problems = Vec::new();
    let text = read(&root.join(KERNEL_LIB_ROOT))?;
    if !text.contains(FORBID) {
        problems.push(format!("{KERNEL_LIB_ROOT} no longer forbids `unsafe_code`"));
    }
    let bin_root = root.join(KERNEL_BIN_ROOT);
    let root_text = read(&bin_root)?;
    if !list.contains_key(KERNEL_BIN_ROOT) && count_sites(&root_text) != 0 {
        problems.push(format!(
            "{KERNEL_BIN_ROOT}: not on the allowlist, so it must hold no `unsafe` token"
        ));
    }
    for file in bin_modules(&bin_root, &root_text)? {
        let relative = file
            .strip_prefix(root)
            .map_err(|e| format!("{}: {e}", file.display()))?
            .to_string_lossy()
            .into_owned();
        if list.contains_key(&relative) {
            continue;
        }
        if !read(&file)?.contains(FORBID) {
            problems.push(relative);
        }
    }
    Ok(problems)
}

fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The files of the modules declared by the kernel binary's crate root (a line `mod x;`,
/// `pub mod x;` or `pub(crate) mod x;`), and of the modules those declare in turn.
fn bin_modules(root_file: &Path, root_text: &str) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut pending: Vec<(PathBuf, String)> = vec![(root_file.to_path_buf(), root_text.into())];
    while let Some((file, text)) = pending.pop() {
        let dir = module_dir(&file);
        for name in declared_modules(&text) {
            let (flat, nested) = (
                dir.join(format!("{name}.rs")),
                dir.join(&name).join("mod.rs"),
            );
            let child = if flat.is_file() {
                flat
            } else if nested.is_file() {
                nested
            } else {
                return Err(format!(
                    "unsafe gate: module {name} of {} has no file",
                    file.display()
                ));
            };
            let child_text = read(&child)?;
            found.push(child.clone());
            pending.push((child, child_text));
        }
    }
    Ok(found)
}

/// Where the files of the modules declared in `file` live: beside it for `main.rs`,
/// `lib.rs` and `mod.rs`; otherwise in a directory named after it (`iommu.rs` → `iommu/`).
fn module_dir(file: &Path) -> PathBuf {
    let stem = file.file_stem().unwrap_or_default().to_string_lossy();
    let parent = file.parent().unwrap_or(Path::new("")).to_path_buf();
    if matches!(stem.as_ref(), "main" | "lib" | "mod") {
        parent
    } else {
        parent.join(stem.as_ref())
    }
}

/// The module names declared without a body in `text`: lines of the shape `mod x;` (with
/// an optional visibility in front), after comments and attributes are stripped by the
/// line's first token being the keyword.
fn declared_modules(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("//") {
            continue;
        }
        let rest = line
            .strip_prefix("pub(crate) mod ")
            .or_else(|| line.strip_prefix("pub mod "))
            .or_else(|| line.strip_prefix("mod "));
        if let Some(name) = rest.and_then(|r| r.strip_suffix(';'))
            && !name.is_empty()
            && name.bytes().all(is_identifier_byte)
        {
            names.push(name.to_string());
        }
    }
    names
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

    #[test]
    fn the_allowlist_parses() {
        let text = "# a comment\n\n3 a.rs\n12 b/c.rs\n";
        let list = parse_allowlist(text).unwrap();
        assert_eq!(list.get("a.rs"), Some(&3));
        assert_eq!(list.get("b/c.rs"), Some(&12));
        assert!(parse_allowlist("nope a.rs\n").is_err());
        assert!(parse_allowlist("1 a.rs\n1 a.rs\n").is_err());
        assert!(parse_allowlist("1\n").is_err());
    }

    fn map(entries: &[(&str, usize)]) -> BTreeMap<String, usize> {
        entries.iter().map(|&(p, c)| (p.to_string(), c)).collect()
    }

    #[test]
    fn the_allowlist_may_only_shrink() {
        let earlier = map(&[("a.rs", 3), ("b.rs", 2)]);
        // Lower counts and removals are fine.
        assert!(shrinks(&earlier, &map(&[("a.rs", 1)])).is_ok());
        assert!(shrinks(&earlier, &map(&[])).is_ok());
        assert!(shrinks(&earlier, &earlier).is_ok());
        // A new file or a higher count is not.
        assert!(shrinks(&earlier, &map(&[("c.rs", 1)])).is_err());
        assert!(shrinks(&earlier, &map(&[("a.rs", 4)])).is_err());
    }

    #[test]
    fn an_unsafe_file_off_the_list_fails_and_a_stale_entry_fails() {
        let outside = map(&[("a.rs", 2)]);
        assert!(allowlist_problems(&outside, &map(&[("a.rs", 2)]), 5).is_empty());
        // Not listed.
        let missing = allowlist_problems(&outside, &map(&[]), 5);
        assert_eq!(missing.len(), 1);
        assert!(missing[0].contains("not on the allowlist"), "{missing:?}");
        // Listed with the wrong count.
        let wrong = allowlist_problems(&outside, &map(&[("a.rs", 3)]), 5);
        assert!(wrong[0].contains("may only go down"), "{wrong:?}");
        // Listed but clean: the entry must go.
        let stale = allowlist_problems(&map(&[]), &map(&[("a.rs", 2)]), 5);
        assert!(stale[0].contains("take it off the list"), "{stale:?}");
        // A framework with nothing in it means the gate is looking in the wrong place.
        let empty = allowlist_problems(&outside, &map(&[("a.rs", 2)]), 0);
        assert!(empty[0].contains("gate's roots are wrong"), "{empty:?}");
    }

    #[test]
    fn module_declarations_are_found() {
        let text = "// mod not_this;\nmod first;\npub mod second;\npub(crate) mod third;\nmod with_body {}\nmod broken";
        assert_eq!(declared_modules(text), ["first", "second", "third"]);
    }

    #[test]
    fn module_files_are_found_where_rust_looks_for_them() {
        assert_eq!(
            module_dir(Path::new("framekernel/src/main.rs")),
            Path::new("framekernel/src")
        );
        assert_eq!(
            module_dir(Path::new("framekernel/src/iommu.rs")),
            Path::new("framekernel/src/iommu")
        );
    }

    /// The real tree, so the gate's roots are known to find the real files.
    #[test]
    fn the_gate_sees_the_real_tree() {
        let root = repo_root();
        let (_, framework_sites, scanned) = scan(&root).expect("the scan");
        assert!(framework_sites > 0, "the framework has no unsafe");
        assert!(scanned > 50, "only {scanned} files scanned");
        let list = read_allowlist(&root).expect("the allowlist");
        assert!(!list.is_empty());
        let names = kernel_packages(&root).expect("the packages");
        assert!(names.contains("origin-frame") && names.contains("framekernel"));
    }
}
