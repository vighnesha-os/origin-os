//! `cargo xtask check`'s gate on two names that only listed files may use, each with its reason
//! (task S4-2; the independent review of 2026-11-13, findings K5 and K12). It is held like the
//! `unsafe` list (`unsafety`): a name in a file off its list fails, and so does a listed file that
//! no longer uses it, so that the list stays what the tree does.
//!
//! - **The proof-process constructors** (ADR-0011 Amendment 2, L8). `Process::proof` and
//!   `Process::proof_child_of` make a process whose system calls are judged by a range its
//!   creator names (ADR-0005 A6), not pinned per call. L8 keeps that for the kernel's own boot
//!   proofs; every program is launched pinned (`launch::load_program`). Without a gate the rule
//!   would rest on which constructor a later task picks.
//! - **The private call numbers in ring 3** (CLAUDE.md §42; ADR-0011 L3 and N13). A program
//!   reaches the kernel through `origin.core`'s imports and names no call number. `origin_abi`
//!   has a public `syscall` module of the numbers, for the kernel and for proofs. A ring-3 file
//!   may name it only if it is listed here.
//!
//! It reads tokens (`lex`), so a name in a comment or a string does not count. It is the
//! source-level form of two rules whose run-time form is the per-call pin; it shows nothing
//! about what a program does once built.

use std::collections::BTreeMap;
use std::path::Path;

use crate::lex::{self, Kind, Token};
use crate::{Result, collect_files, repo_root};

/// Where the proof-process constructors may be named: the kernel's own source.
const KERNEL: &str = "framekernel/src";

/// The files that may name `Process::proof` or `Process::proof_child_of`, and why.
const PROOF_PROCESS_FILES: &[(&str, &str)] = &[
    (
        "framekernel/src/process.rs",
        "defines them, and `proof_child_of` is `proof` with its creator's principal",
    ),
    (
        "framekernel/src/aspace.rs",
        "the address-space proofs run a hand-made program in a process of their own",
    ),
    (
        "framekernel/src/user.rs",
        "the ring-3 isolation proof runs hand-made instructions",
    ),
    (
        "framekernel/src/ipc_proof.rs",
        "the IPC, objects, launch and identity proofs run the assembly script interpreter, and make creators that never run",
    ),
    (
        "framekernel/src/ipc_proof/memory_objects.rs",
        "the memory-object proofs run the assembly script interpreter",
    ),
    (
        "framekernel/src/ipc_proof/directory.rs",
        "the directory proof's launcher, directory, service and client are assembly proof programs",
    ),
    (
        "framekernel/src/ipc_proof/core_pin.rs",
        "the creator of the pinned processes, which never runs, and the proof process that keeps its range (L8)",
    ),
    (
        "framekernel/src/program_proof.rs",
        "the creator of the first program's launches, which never runs",
    ),
];

/// The ring-3 files that may name `origin_abi::syscall`, and why.
const CALL_NUMBER_FILES: &[(&str, &str)] = &[(
    "tests/kernel/pin-program/src/calls.rs",
    "the pin proof's interpreter maps a script's call number to the import of that call; the numbers are this program's on purpose, and the proof's scripts are written in them",
)];

/// Whether `tokens` name one of the proof-process constructors: `Process::` or `Self::`
/// followed by `proof` or `proof_child_of`, or a definition `fn proof` / `fn proof_child_of`.
fn names_proof_process(tokens: &[Token<'_>]) -> bool {
    let is = |t: Option<&Token<'_>>, word: &str| {
        t.is_some_and(|t| t.kind == Kind::Ident && t.text == word)
    };
    let colon = |t: Option<&Token<'_>>| t.is_some_and(|t| t.kind == Kind::Punct && t.text == ":");
    tokens.iter().enumerate().any(|(i, token)| {
        let constructor = |t: Option<&Token<'_>>| is(t, "proof") || is(t, "proof_child_of");
        let path = (token.is_ident("Process") || token.is_ident("Self"))
            && colon(tokens.get(i + 1))
            && colon(tokens.get(i + 2))
            && constructor(tokens.get(i + 3));
        let definition = token.is_ident("fn") && constructor(tokens.get(i + 1));
        path || definition
    })
}

/// Whether `tokens` name `origin_abi::syscall`: as a path, or among the items of a `use
/// origin_abi::{…}` group at any depth.
fn names_call_numbers(tokens: &[Token<'_>]) -> bool {
    let colon = |t: Option<&Token<'_>>| t.is_some_and(|t| t.kind == Kind::Punct && t.text == ":");
    tokens.iter().enumerate().any(|(i, token)| {
        if !token.is_ident("origin_abi") || !colon(tokens.get(i + 1)) || !colon(tokens.get(i + 2)) {
            return false;
        }
        match tokens.get(i + 3) {
            Some(next) if next.is_ident("syscall") => true,
            Some(next) if next.kind == Kind::Punct && next.text == "{" => {
                let close = lex::closing(tokens, i + 3).unwrap_or(tokens.len());
                tokens
                    .get(i + 4..close)
                    .is_some_and(|group| group.iter().any(|t| t.is_ident("syscall")))
            }
            _ => false,
        }
    })
}

/// What is wrong with `files` (repository path to text) against `listed`: a file that names
/// what `names` finds and is not listed, and a listed file that does not name it or is gone.
pub(crate) fn problems(
    what: &str,
    files: &BTreeMap<String, String>,
    listed: &[(&str, &str)],
    names: fn(&[Token<'_>]) -> bool,
) -> Vec<String> {
    let mut problems = Vec::new();
    for (path, text) in files {
        let named = names(&lex::tokens(text));
        let on_list = listed.iter().any(|(file, _)| file == path);
        if named && !on_list {
            problems.push(format!("{path} names {what} and is not on the list"));
        }
        if on_list && !named {
            problems.push(format!(
                "{path} is on the list for {what} and no longer names it: take it off"
            ));
        }
    }
    for (file, _) in listed {
        if !files.contains_key(*file) {
            problems.push(format!(
                "{file} is on the list for {what} and is not in the tree"
            ));
        }
    }
    problems
}

/// Every Rust file under each of `dirs`, by repository path, with its text.
fn read_rust(root: &Path, dirs: &[&str]) -> Result<BTreeMap<String, String>> {
    let mut paths = Vec::new();
    for dir in dirs {
        collect_files(
            &root.join(dir),
            &|_| false,
            &|name| name.ends_with(".rs"),
            &mut paths,
        )
        .map_err(|e| format!("names gate: {dir}: {e}"))?;
    }
    let mut files = BTreeMap::new();
    for path in paths {
        let shown = path
            .strip_prefix(root)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        files.insert(shown, text);
    }
    Ok(files)
}

/// `cargo xtask names-gate`, and a step of `check`.
pub(crate) fn gate() -> Result {
    let root = repo_root();
    let kernel = read_rust(&root, &[KERNEL])?;
    let ring3 = read_rust(&root, crate::unsafety::RING3_CRATES)?;
    let mut found = problems(
        "`Process::proof` or `Process::proof_child_of`, which make a process judged by a range and not pinned per call (ADR-0011 L8)",
        &kernel,
        PROOF_PROCESS_FILES,
        names_proof_process,
    );
    found.extend(problems(
        "`origin_abi::syscall`, the private call numbers (CLAUDE.md §42; ADR-0011 L3)",
        &ring3,
        CALL_NUMBER_FILES,
        names_call_numbers,
    ));
    if !found.is_empty() {
        return Err(format!(
            "names gate (developer-tools/xtask/src/listed_names.rs) ({} problems):\n  {}",
            found.len(),
            found.join("\n  ")
        ));
    }
    println!(
        "xtask: names gate: the proof-process constructors are named in {} kernel files, each listed with its reason, and in none of the other {}; the call numbers in {} ring-3 file, listed, and in none of the other {}",
        PROOF_PROCESS_FILES.len(),
        kernel.len() - PROOF_PROCESS_FILES.len(),
        CALL_NUMBER_FILES.len(),
        ring3.len() - CALL_NUMBER_FILES.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests;
