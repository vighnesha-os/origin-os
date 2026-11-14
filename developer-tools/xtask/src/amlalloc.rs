//! `cargo xtask check`'s allocation gate on the AML interpreter (TASKS item 63; the contract's §1,
//! accepted 2026-11-14; ADR-0019 A3).
//!
//! The interpreter runs definition blocks it cannot trust and must never end its host: not by a
//! panic, and not by an allocation that fails inside `alloc`, where a growing method's failure is
//! fatal. So every allocation that can grow goes through one file, `firmware/acpi/aml/src/mem.rs`,
//! which calls `try_reserve_exact` first and charges the bytes to the memory limit. This gate
//! refuses, in every other library file of the crate (its tests are exempt), the forms that
//! allocate or grow without that: `vec!`, `format!`, `Box::new`, `String::…`, `Vec::with_capacity`,
//! and the methods `push`, `push_str`, `extend`, `extend_from_slice`, `insert`, `resize`,
//! `append`, `reserve`, `reserve_exact`, `try_reserve`, `try_reserve_exact`, `to_vec`, `to_owned`,
//! `to_string`, `collect`, `clone`, `concat`, `join` and `repeat`.
//!
//! Why the no-panic check allows `alloc::raw_vec::handle_error` for this crate alone: `mem.rs`'s
//! growing methods run only after a `try_reserve_exact` that succeeded, so the growth they would
//! do, and the fatal path behind it, never happens; Rust 1.100.0's `Vec` has no stable method that
//! appends without that path in its code (`push_within_capacity` is unstable), so the symbol stays
//! in the object though it is unreachable.
//!
//! It reads tokens (`lex`), so a name in a comment or a string does not count. Every run also
//! checks a planted copy of a real file, and fails if the plant is not caught.

use std::collections::BTreeMap;

use crate::lex::{self, Kind, Token};
use crate::{Result, collect_files, repo_root};

/// The crate's library sources.
const SOURCES: &str = "firmware/acpi/aml/src";
/// The one file that may grow a collection.
const MEM: &str = "firmware/acpi/aml/src/mem.rs";
/// The tests, which run on the host only.
const TESTS: &str = "firmware/acpi/aml/src/tests";
/// The file a negative control is planted in, each run.
const PLANT_IN: &str = "firmware/acpi/aml/src/engine.rs";

/// Methods that allocate or grow.
const METHODS: &[&str] = &[
    "push",
    "push_str",
    "extend",
    "extend_from_slice",
    "insert",
    "resize",
    "append",
    "reserve",
    "reserve_exact",
    "try_reserve",
    "try_reserve_exact",
    "to_vec",
    "to_owned",
    "to_string",
    "collect",
    "clone",
    "concat",
    "join",
    "repeat",
];

/// Macros that allocate.
const MACROS: &[&str] = &["vec", "format"];

/// What `text` does that only `mem.rs` may: each finding with its line.
pub(crate) fn findings(text: &str) -> Vec<String> {
    let tokens = lex::tokens(text);
    let mut out = Vec::new();
    let at = |t: &Token<'_>| lex::line_of(text, t.start);
    for (i, t) in tokens.iter().enumerate() {
        let next = tokens.get(i + 1);
        let prev = i.checked_sub(1).and_then(|p| tokens.get(p));
        if t.kind != Kind::Ident {
            continue;
        }
        // `vec!` and `format!`.
        if MACROS.contains(&t.text) && next.is_some_and(|n| n.is_punct('!')) {
            out.push(format!("line {}: `{}!`", at(t), t.text));
            continue;
        }
        // `String::…` (`String::new`, `String::from` …). A bare `String` is a type, or a variant
        // of the crate's own enums, and allocates nothing by itself.
        if t.text == "String"
            && prev.is_none_or(|p| !p.is_punct(':'))
            && next.is_some_and(|n| n.is_punct(':'))
            && tokens.get(i + 2).is_some_and(|n| n.is_punct(':'))
        {
            out.push(format!("line {}: `String::`", at(t)));
            continue;
        }
        // `Box::new` and `Vec::with_capacity`.
        let path_call = |owner: &str, method: &str| {
            t.text == owner
                && tokens.get(i + 1).is_some_and(|c| c.is_punct(':'))
                && tokens.get(i + 2).is_some_and(|c| c.is_punct(':'))
                && tokens.get(i + 3).is_some_and(|m| m.is_ident(method))
        };
        if path_call("Box", "new") {
            out.push(format!("line {}: `Box::new`", at(t)));
            continue;
        }
        if path_call("Vec", "with_capacity") {
            out.push(format!("line {}: `Vec::with_capacity`", at(t)));
            continue;
        }
        // `.method(` and `.method::<…>(`.
        if METHODS.contains(&t.text)
            && prev.is_some_and(|p| p.is_punct('.'))
            && next.is_some_and(|n| n.is_punct('(') || n.is_punct(':'))
        {
            out.push(format!("line {}: `.{}(`", at(t), t.text));
        }
    }
    out
}

fn sources() -> Result<BTreeMap<String, String>> {
    let root = repo_root();
    let mut paths = Vec::new();
    collect_files(
        &root.join(SOURCES),
        &|p| p.ends_with("tests"),
        &|name| name.ends_with(".rs"),
        &mut paths,
    )
    .map_err(|e| format!("allocation gate: {SOURCES}: {e}"))?;
    let mut files = BTreeMap::new();
    for path in paths {
        let shown = path
            .strip_prefix(&root)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        if shown.starts_with(TESTS) {
            continue;
        }
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        files.insert(shown, text);
    }
    Ok(files)
}

/// The gate.
pub(crate) fn gate() -> Result {
    let files = sources()?;
    if !files.contains_key(MEM) || !files.contains_key(PLANT_IN) {
        return Err(format!(
            "allocation gate: {MEM} or {PLANT_IN} is missing: the gate would check nothing"
        ));
    }
    // The negative control: a growth planted in a real file must be caught.
    let planted = format!(
        "{}\nfn planted(v: &mut alloc::vec::Vec<u8>) {{ v.push(1); }}\n",
        files.get(PLANT_IN).map_or("", String::as_str)
    );
    if findings(&planted).len()
        != findings(files.get(PLANT_IN).map_or("", String::as_str)).len() + 1
    {
        return Err(format!(
            "allocation gate: its negative control (a `push` planted in {PLANT_IN}) was not caught"
        ));
    }
    let mut problems = Vec::new();
    for (path, text) in &files {
        if path == MEM {
            continue;
        }
        for f in findings(text) {
            problems.push(format!("{path}: {f}"));
        }
    }
    if !problems.is_empty() {
        return Err(format!(
            "allocation gate (developer-tools/xtask/src/amlalloc.rs): only {MEM} may allocate or grow a collection ({} found):\n  {}",
            problems.len(),
            problems.join("\n  ")
        ));
    }
    println!(
        "xtask: allocation gate: {} library files of origin-aml allocate only through {MEM}; its planted `push` was caught",
        files.len() - 1
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::findings;

    #[test]
    fn each_refused_form_is_found_once_and_named() {
        for (src, what) in [
            ("fn f(v: &mut Vec<u8>) { v.push(1); }", "`.push(`"),
            (
                "fn f(v: &mut Vec<u8>) { v.extend_from_slice(&[1]); }",
                "`.extend_from_slice(`",
            ),
            ("fn f() -> Vec<u8> { vec![1] }", "`vec!`"),
            ("fn f() { let _ = format!(\"x\"); }", "`format!`"),
            ("fn f() -> Box<u8> { Box::new(1) }", "`Box::new`"),
            (
                "fn f() -> Vec<u8> { Vec::with_capacity(4) }",
                "`Vec::with_capacity`",
            ),
            ("fn f() -> String { String::new() }", "`String::`"),
            ("fn f(v: &[u8]) -> Vec<u8> { v.to_vec() }", "`.to_vec(`"),
            (
                "fn f(v: &[u8]) -> Vec<u8> { v.iter().copied().collect::<Vec<u8>>() }",
                "`.collect(`",
            ),
            ("fn f(v: &Vec<u8>) -> Vec<u8> { v.clone() }", "`.clone(`"),
            (
                "fn f(v: &mut Vec<u8>) { v.try_reserve(1).ok(); }",
                "`.try_reserve(`",
            ),
        ] {
            let found = findings(src);
            assert_eq!(found.len(), 1, "{src}: {found:?}");
            assert!(found[0].contains(what), "{src}: {found:?}");
        }
    }

    #[test]
    fn comments_strings_and_non_growing_methods_pass() {
        for src in [
            "// v.push(1) in a comment\nfn f() {}",
            "fn f() -> &'static str { \"v.push(1)\" }",
            "fn f(v: &mut Vec<u8>) { v.truncate(1); v.pop(); v.clear(); }",
            "fn f() -> Vec<u8> { Vec::new() }",
            "fn push(x: u8) -> u8 { x }",
            "enum V { String(u8) } fn f() -> V { V::String(1) }",
        ] {
            assert!(findings(src).is_empty(), "{src}: {:?}", findings(src));
        }
    }
}
