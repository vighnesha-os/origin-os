//! `cargo xtask check`'s allocation gate on the AML interpreter (TASKS item 63; the contract's §1,
//! accepted 2026-11-14; ADR-0019 A3).
//!
//! The interpreter runs definition blocks it cannot trust and must never end its host: not by a
//! panic, and not by an allocation that fails inside `alloc`, where a growing method's failure is
//! fatal. So every allocation that can grow goes through one file, `firmware/acpi/aml/src/mem.rs`,
//! which calls `try_reserve_exact` first and charges the bytes to the memory limit. This gate
//! refuses, in every other library file of the crate (its tests are exempt): `vec!` and
//! `format!`; `String::…`; every use of `Box`, `Rc`, `Arc`, the other collections and
//! `FromIterator`; `Vec::from` and `String::from`; `.into(`, whose target the
//! lexer cannot see; and every growing method ([`GROWERS`]) wherever it is named, as a method or
//! by path, except `mem.rs`'s own `mem::push` and kin. The review of B1 (F5) found the first
//! version caught methods only after a `.` and named few path forms.
//!
//! Why the no-panic check allows `alloc::raw_vec::handle_error` for this crate alone: `mem.rs`'s
//! growing methods run only after a `try_reserve_exact` that succeeded, so the growth they would
//! do, and the fatal path behind it, never happens; Rust 1.100.0's `Vec` has no stable method that
//! appends without that path in its code (`push_within_capacity` is unstable), so the symbol stays
//! in the object though it is unreachable.
//!
//! **The kernel's boot proof** (`framekernel/src/aml_proof.rs`, TASKS item 63's step B3) is held to
//! the same rules, for the same reason: while the interpreter may hold up to its memory limit of
//! the kernel heap, nothing the proof grows may fail inside `alloc` (the coordinating session's
//! ruling on B3's limit, condition 2; the review of B3, B3-6). Its one file that may grow a
//! collection is `aml_proof/fallible.rs`, whose `fallible::push` and kin are its `mem::push`; its
//! tests are exempt.
//!
//! It reads tokens (`lex`), so a name in a comment or a string does not count. Every run also
//! checks a planted copy of a real file, in each of the two, and fails if the plant is not caught.

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
/// The kernel's boot proof of the interpreter, held to the same rules (B3-6).
const PROOF: &str = "framekernel/src/aml_proof.rs";
/// The proof's one file that may grow a collection, as `mem.rs` is the library's.
const PROOF_GROWS: &str = "framekernel/src/aml_proof/fallible.rs";

/// Names of methods and functions that allocate or grow a collection. Refused wherever they are
/// named, as a method (`.push(`) or by path (`Vec::push(&mut v, …)`), unless the path is `mem.rs`'s
/// own (`mem::push`). The review of B1, F5.
const GROWERS: &[&str] = &[
    "push",
    "push_str",
    "push_back",
    "push_front",
    "push_within_capacity",
    "extend",
    "extend_from_slice",
    "extend_from_within",
    "insert",
    "insert_str",
    "resize",
    "resize_with",
    "append",
    "split_off",
    "reserve",
    "reserve_exact",
    "try_reserve",
    "try_reserve_exact",
    "shrink_to",
    "shrink_to_fit",
    "to_vec",
    "to_owned",
    "to_string",
    "into_boxed_slice",
    "collect",
    "from_iter",
    "clone",
    "clone_from",
    "concat",
    "join",
    "repeat",
    "with_capacity",
    // The review of B1's addendum (A3): `splice` grows; the stable sorts allocate a scratch
    // buffer; on a slice the ASCII case conversions return a `Vec`; `alloc::fmt::format` is
    // `format!` by name; `into_owned` makes a `Vec` of a borrowed `Cow`.
    "splice",
    "sort",
    "sort_by",
    "sort_by_key",
    "sort_by_cached_key",
    "to_ascii_uppercase",
    "to_ascii_lowercase",
    "format",
    "into_owned",
];

/// Types whose every use allocates, or that the crate has no business with: refused wherever
/// named. (`Vec` itself is allowed: making an empty one allocates nothing.)
const TYPES: &[&str] = &[
    "Box",
    "Rc",
    "Arc",
    "VecDeque",
    "BTreeMap",
    "BTreeSet",
    "BinaryHeap",
    "LinkedList",
    "FromIterator",
];

/// Owners whose `from` allocates: `Vec::from`, `String::from`.
const FROM_OWNERS: &[&str] = &["Vec", "String"];

/// Macros that allocate.
const MACROS: &[&str] = &["vec", "format"];

/// What `text` does that only `mem.rs` may: each finding with its line.
pub(crate) fn findings(text: &str) -> Vec<String> {
    findings_beside(text, "mem")
}

/// What `text` does that only the module `own` may: each finding with its line. A growing method
/// named by `own`'s path (`mem::push`, `fallible::push`) is that module's, and allowed.
fn findings_beside(text: &str, own: &str) -> Vec<String> {
    let tokens = lex::tokens(text);
    let mut out = Vec::new();
    let at = |t: &Token<'_>| lex::line_of(text, t.start);
    // Whether the tokens just before `i` are `owner ::`.
    let after_path = |i: usize, owner: &str| {
        i >= 3
            && tokens.get(i - 1).is_some_and(|c| c.is_punct(':'))
            && tokens.get(i - 2).is_some_and(|c| c.is_punct(':'))
            && tokens.get(i - 3).is_some_and(|o| o.is_ident(owner))
    };
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
        if TYPES.contains(&t.text) {
            out.push(format!("line {}: `{}`", at(t), t.text));
            continue;
        }
        // `Vec::from`, `String::from`.
        if t.text == "from" && FROM_OWNERS.iter().any(|o| after_path(i, o)) {
            out.push(format!("line {}: `::from` into a collection", at(t)));
            continue;
        }
        // `.into(`: the lexer cannot see the target type, so it is refused everywhere; the crate
        // converts with `From::from` by name instead.
        if t.text == "into"
            && prev.is_some_and(|p| p.is_punct('.'))
            && next.is_some_and(|n| n.is_punct('(') || n.is_punct(':'))
        {
            out.push(format!("line {}: `.into(`", at(t)));
            continue;
        }
        // A growing method, called or named by path, but `mem::push` and its kin. A definition
        // (`fn push`) is not a call.
        if GROWERS.contains(&t.text)
            && !after_path(i, own)
            && !prev.is_some_and(|p| p.is_ident("fn"))
            && next.is_some_and(|n| n.is_punct('(') || n.is_punct(':'))
        {
            out.push(format!("line {}: `{}`", at(t), t.text));
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
    proof_gate()
}

/// The same rules on the kernel's boot proof of the interpreter, whose one file that may grow a
/// collection is [`PROOF_GROWS`] (B3-6), with its own planted control.
fn proof_gate() -> Result {
    let root = repo_root();
    let read = |path: &str| {
        std::fs::read_to_string(root.join(path))
            .map_err(|e| format!("allocation gate: {path}: {e}"))
    };
    let text = read(PROOF)?;
    // The file that may grow must exist and do so, or the gate is looking in the wrong place.
    if !read(PROOF_GROWS)?.contains("try_reserve") {
        return Err(format!(
            "allocation gate: {PROOF_GROWS} reserves nothing: the gate's files are wrong"
        ));
    }
    let planted = format!("{text}\nfn planted(v: &mut alloc::vec::Vec<u8>) {{ v.push(1); }}\n");
    if findings_beside(&planted, "fallible").len() != findings_beside(&text, "fallible").len() + 1 {
        return Err(format!(
            "allocation gate: its negative control (a `push` planted in {PROOF}) was not caught"
        ));
    }
    let problems: Vec<String> = findings_beside(&text, "fallible")
        .into_iter()
        .map(|f| format!("{PROOF}: {f}"))
        .collect();
    if !problems.is_empty() {
        return Err(format!(
            "allocation gate (developer-tools/xtask/src/amlalloc.rs): the boot proof allocates only through {PROOF_GROWS} ({} found):\n  {}",
            problems.len(),
            problems.join("\n  ")
        ));
    }
    println!(
        "xtask: allocation gate: the kernel's boot proof of the interpreter ({PROOF}) allocates only through {PROOF_GROWS}; its planted `push` was caught"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::findings;

    #[test]
    fn each_refused_form_is_found_and_named() {
        for (src, what) in [
            ("fn f(v: &mut Vec<u8>) { v.push(1); }", "`push`"),
            (
                "fn f(v: &mut Vec<u8>) { v.extend_from_slice(&[1]); }",
                "`extend_from_slice`",
            ),
            ("fn f() -> Vec<u8> { vec![1] }", "`vec!`"),
            ("fn f() { let _ = format!(\"x\"); }", "`format!`"),
            (
                "fn f() -> Vec<u8> { Vec::with_capacity(4) }",
                "`with_capacity`",
            ),
            ("fn f() -> String { String::new() }", "`String::`"),
            ("fn f(v: &[u8]) -> Vec<u8> { v.to_vec() }", "`to_vec`"),
            (
                "fn f(v: &[u8]) -> Vec<u8> { v.iter().copied().collect::<Vec<u8>>() }",
                "`collect`",
            ),
            ("fn f(v: &Vec<u8>) -> Vec<u8> { v.clone() }", "`clone`"),
            (
                "fn f(v: &mut Vec<u8>) { v.try_reserve(1).ok(); }",
                "`try_reserve`",
            ),
            // The forms the review of B1 (F5) found the first gate let through.
            ("fn f(v: &mut Vec<u8>) { Vec::push(v, 1); }", "`push`"),
            (
                "fn f(a: [u8; 2]) -> Vec<u8> { Vec::from(a) }",
                "`::from` into a collection",
            ),
            ("fn f(a: &[u8]) -> Vec<u8> { a.into() }", "`.into(`"),
            (
                "fn f(a: &[u8]) -> Vec<u8> { FromIterator::from_iter(a.iter().copied()) }",
                "`FromIterator`",
            ),
            (
                "fn f(a: &[u8]) -> Vec<u8> { Vec::from_iter(a.iter().copied()) }",
                "`from_iter`",
            ),
            (
                "fn f(v: &mut Vec<u8>) -> Vec<u8> { v.split_off(1) }",
                "`split_off`",
            ),
            (
                "fn f(v: &mut Vec<u8>) { v.extend_from_within(..); }",
                "`extend_from_within`",
            ),
            (
                "fn f(v: &mut Vec<u8>) { v.shrink_to_fit(); }",
                "`shrink_to_fit`",
            ),
            ("fn f() -> u8 { *Box::<u8>::new(1) }", "`Box`"),
            ("fn f() { let _ = Rc::new(1); }", "`Rc`"),
            ("fn f() { let _ = Arc::new(1); }", "`Arc`"),
            ("fn f(q: &mut VecDeque<u8>) { q.len(); }", "`VecDeque`"),
            // The forms the addendum to that review (A3) found the second let through.
            (
                "fn f(v: &mut Vec<u8>) { v.splice(0..0, [1, 2]); }",
                "`splice`",
            ),
            ("fn f(v: &mut [u8]) { v.sort(); }", "`sort`"),
            (
                "fn f(v: &mut [u8]) { v.sort_by(|a, b| a.cmp(b)); }",
                "`sort_by`",
            ),
            (
                "fn f(v: &mut [u8]) { v.sort_by_key(|a| *a); }",
                "`sort_by_key`",
            ),
            (
                "fn f(v: &mut [u8]) { v.sort_by_cached_key(|a| *a); }",
                "`sort_by_cached_key`",
            ),
            (
                "fn f(v: &[u8]) -> Vec<u8> { v.to_ascii_uppercase() }",
                "`to_ascii_uppercase`",
            ),
            (
                "fn f(v: &[u8]) -> Vec<u8> { v.to_ascii_lowercase() }",
                "`to_ascii_lowercase`",
            ),
            (
                "fn f(a: core::fmt::Arguments<'_>) -> String { alloc::fmt::format(a) }",
                "`format`",
            ),
            (
                "fn f(c: Cow<'_, [u8]>) -> Vec<u8> { c.into_owned() }",
                "`into_owned`",
            ),
        ] {
            let found = findings(src);
            assert!(found.iter().any(|f| f.contains(what)), "{src}: {found:?}");
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
            "fn f(v: &mut Vec<u8>) { mem::push(v, 1, b).ok(); mem::reserve(v, 1, b).ok(); }",
            "fn f(x: u8) -> u32 { u32::from(x) }",
            "fn f(k: K) -> E { E::from(k) }",
            // Their relatives that allocate nothing.
            "fn f(v: &mut [u8]) { v.sort_unstable(); v.sort_unstable_by_key(|a| *a); }",
            "fn f(v: &mut [u8]) { v.make_ascii_uppercase(); v.make_ascii_lowercase(); }",
            "fn f(f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result { f.write_str(\"x\") }",
        ] {
            assert!(findings(src).is_empty(), "{src}: {:?}", findings(src));
        }
    }
}
