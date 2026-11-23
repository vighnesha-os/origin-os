//! `cargo xtask check`'s gate on the boot proofs that compare the whole machine's counts (TASKS
//! item 67). The heap and the free frames move on their own while a proof runs: an exited thread
//! is reaped on its own processor when that processor gets to it, a stack goes into the cache of
//! stacks kept for reuse or is retired, and a retired stack's frames come back only when some
//! processor reclaims it, one shootdown each. On the 65-processor machine that made the IPC proof
//! end with 60 more free frames than it began. So a kernel function that reads `memory::stats`
//! calls `sched::settle` before its first read and again after it, or this gate lists it, with
//! the reason it needs neither.
//!
//! It reads tokens (`lex`), so a name in a comment or a string does not count. What it cannot see:
//! - a count read under another name (a wrapper of `memory::stats`, or `memory` imported under
//!   another name);
//! - counts other than `memory::stats`, such as `stack::counts` or the kernel-wide bound's root,
//!   compared without a settle;
//! - whether the settle's answer is used, or whether the second settle comes after the proof's
//!   last thread has ended (an order only the run shows);
//! - code outside `framekernel/src`, and a function nested inside another (it counts as part of
//!   the outer one).

use crate::lex::{self, Kind, Token};
use crate::per_cpu::{in_ranges, test_ranges};
use crate::{Result, collect_files, repo_root};

/// The kernel crate's sources: where the boot proofs are.
const KERNEL: &str = "framekernel/src";

/// The file that defines `settle`, where it is called without its module's name.
const SCHED: &str = "framekernel/src/sched.rs";

/// Functions that read `memory::stats` and need no settle: file, function, reason.
const EXEMPT: &[(&str, &str, &str)] = &[
    (
        "framekernel/src/kernel.rs",
        "prove_heap",
        "runs before any other processor starts and before the first thread, so nothing else moves the counts; `sched::settle` reaps through the CPU-local area, which is not set up yet",
    ),
    (
        "framekernel/src/kernel.rs",
        "install_origin_core",
        "prints the fall in free frames as a figure and judges nothing",
    ),
    (
        "framekernel/src/kernel.rs",
        "size_root",
        "sizes the kernel-wide bound's root from the free frames, once, and compares nothing",
    ),
    (
        "framekernel/src/kernel.rs",
        "prove_sysfs",
        "prints the free memory into the /sys tree and compares nothing",
    ),
    (
        "framekernel/src/sched.rs",
        "prove_spawn_refused",
        "compares with `<=`: background work only gives heap back, which the check allows",
    ),
    (
        "framekernel/src/program_proof.rs",
        "refused_whole",
        "runs inside `program_proof::prove`, between its settles, before its first launch and with no thread of its own",
    ),
    (
        "framekernel/src/ipc_proof/kernel_bound.rs",
        "snapshot",
        "reads the counts for clauses that call `sched::settle` just before it",
    ),
    (
        "framekernel/src/migration_proof.rs",
        "counts",
        "reads the counts for each migration proof's bracket: in `prove_integrity` and `prove_delivery`, `sched::settle` is called just before the first read, and `bracket_after` calls it again before the second; a new caller of `counts` must settle first (TASKS item 96)",
    ),
    (
        "framekernel/src/aml_proof.rs",
        "prove",
        "runs on the loader's stack before the first thread exists, the other processors started and idle: nothing reaps, retires a stack or allocates while it runs, so the heap moves with the proof alone; it compares the heap's bytes only, not the free frames (TASKS item 63, step B3, and the coordinating session's ruling on its heap check)",
    ),
];

/// Runs the gate on the checkout.
pub(crate) fn gate() -> Result {
    let root = repo_root();
    let mut files = Vec::new();
    collect_files(
        &root.join(KERNEL),
        &|_| false,
        &|name| name.ends_with(".rs"),
        &mut files,
    )
    .map_err(|e| format!("reading {KERNEL}: {e}"))?;
    let mut sources = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&path).map_err(|e| format!("reading {rel}: {e}"))?;
        sources.push((rel, text));
    }
    let (problems, readers) = check(&sources);
    if problems.is_empty() {
        println!(
            "xtask: settle gate: {readers} kernel functions read the machine's counts (`memory::stats`): {} settle before their first read and after it, {} are listed with their reasons (TASKS item 67)",
            readers - EXEMPT.len(),
            EXEMPT.len()
        );
        return Ok(());
    }
    Err(format!(
        "the settle gate (TASKS item 67; developer-tools/xtask/src/settle.rs):\n  {}",
        problems.join("\n  ")
    ))
}

/// Whether the gate reads `rel`: not a test file.
fn is_read(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    rel.ends_with(".rs") && name != "tests.rs" && !rel.contains("/tests/")
}

/// Every problem in `sources` (repository-relative path, text), and how many functions read the
/// counts.
pub(crate) fn check(sources: &[(String, String)]) -> (Vec<String>, usize) {
    let mut problems = Vec::new();
    let mut readers = 0;
    let mut exempt_seen = vec![false; EXEMPT.len()];
    for (rel, src) in sources.iter().filter(|(rel, _)| is_read(rel)) {
        let tokens = lex::tokens(src);
        let tests = test_ranges(&tokens);
        for (name, line, body) in functions(&tokens, src, &tests) {
            let reads = body_reads(&tokens, body);
            let Some(&first) = reads.first() else {
                continue;
            };
            readers += 1;
            if let Some(at) = EXEMPT
                .iter()
                .position(|&(file, function, _)| file == rel && function == name)
            {
                exempt_seen[at] = true;
                continue;
            }
            let settles = body_settles(&tokens, body, rel == SCHED);
            let before = settles.iter().any(|&at| at < first);
            let after = settles.iter().any(|&at| at > first);
            if !before || !after {
                problems.push(format!(
                    "{rel}:{line}: `{name}` reads `memory::stats` (line {}) without calling `sched::settle` {}: settle before the first read and again before the compare, or list it here with its reason",
                    lex::line_of(src, tokens[first].start),
                    match (before, after) {
                        (false, false) => "at all",
                        (false, true) => "before its first read",
                        _ => "after its first read",
                    }
                ));
            }
        }
    }
    for (seen, (file, function, _)) in exempt_seen.iter().zip(EXEMPT) {
        if !seen && sources.iter().any(|(rel, _)| rel == file) {
            problems.push(format!(
                "developer-tools/xtask/src/settle.rs: `{function}` in {file} is listed, but no such function there reads `memory::stats`: take it off the list"
            ));
        }
    }
    (problems, readers)
}

/// Each `fn` item of `src` outside test code with a body: its name, its line, and its body's
/// token range `[open, close]`.
fn functions<'a>(
    tokens: &[Token<'a>],
    src: &str,
    tests: &[(usize, usize)],
) -> Vec<(&'a str, usize, (usize, usize))> {
    let mut out = Vec::new();
    for i in 0..tokens.len() {
        if !tokens[i].is_ident("fn") || in_ranges(tests, i) {
            continue;
        }
        let Some(name) = tokens.get(i + 1).filter(|t| t.kind == Kind::Ident) else {
            continue;
        };
        // The signature ends at its body's `{`, or at `;` for a declaration with no body.
        let mut j = i + 2;
        let mut depth = 0i32;
        while let Some(t) = tokens.get(j) {
            if depth == 0 && (t.is_punct('{') || t.is_punct(';')) {
                break;
            }
            depth += lex::bracket(t).unwrap_or(0);
            j += 1;
        }
        if !tokens.get(j).is_some_and(|t| t.is_punct('{')) {
            continue;
        }
        let Some(close) = lex::closing(tokens, j) else {
            continue;
        };
        out.push((name.text, lex::line_of(src, tokens[i].start), (j, close)));
    }
    out
}

/// Where the body `[open, close]` names `memory::stats`.
fn body_reads(tokens: &[Token<'_>], (open, close): (usize, usize)) -> Vec<usize> {
    (open..close)
        .filter(|&i| {
            tokens[i].is_ident("stats")
                && i >= 3
                && tokens[i - 1].is_punct(':')
                && tokens[i - 2].is_punct(':')
                && tokens[i - 3].is_ident("memory")
        })
        .collect()
}

/// Where the body `[open, close]` calls `sched::settle(`, or `settle(` with no path inside the
/// file that defines it.
fn body_settles(tokens: &[Token<'_>], (open, close): (usize, usize), in_sched: bool) -> Vec<usize> {
    (open..close)
        .filter(|&i| {
            if !tokens[i].is_ident("settle") || !tokens.get(i + 1).is_some_and(|t| t.is_punct('('))
            {
                return false;
            }
            let pathed = i >= 3
                && tokens[i - 1].is_punct(':')
                && tokens[i - 2].is_punct(':')
                && tokens[i - 3].is_ident("sched");
            let bare = in_sched
                && !tokens[i - 1].is_punct(':')
                && !tokens[i - 1].is_ident("fn")
                && !tokens[i - 1].is_punct('.');
            pathed || bare
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(rel: &str, src: &str) -> (String, String) {
        (rel.to_string(), src.to_string())
    }

    const SETTLED: &str = "pub(crate) fn prove(tsc_hz: u64) -> bool {\n    let settled_before = sched::settle(tsc_hz);\n    let before = memory::stats();\n    work();\n    let same = || memory::stats() == before;\n    let settled_after = sched::settle(tsc_hz);\n    sched::yield_until(same, tsc_hz, 2);\n    same()\n}\n";

    /// Every listed exemption as a function that reads the counts, so a fixture is whole.
    fn exempt_fixtures() -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for (path, function, _) in EXEMPT {
            let text = format!("fn {function}() {{\n    let _ = memory::stats();\n}}\n");
            match out.iter_mut().find(|(rel, _)| rel == path) {
                Some((_, src)) => src.push_str(&text),
                None => out.push(file(path, &text)),
            }
        }
        out
    }

    fn tree(extra: &[(String, String)]) -> Vec<(String, String)> {
        let mut out = exempt_fixtures();
        out.push(file("framekernel/src/ipc_proof/settled.rs", SETTLED));
        out.extend_from_slice(extra);
        out
    }

    #[test]
    fn a_proof_that_settles_at_both_ends_passes() {
        let (problems, readers) = check(&tree(&[]));
        assert_eq!(problems, Vec::<String>::new());
        assert_eq!(readers, EXEMPT.len() + 1);
    }

    #[test]
    fn a_proof_with_no_settle_fails() {
        let src = SETTLED
            .replace("    let settled_before = sched::settle(tsc_hz);\n", "")
            .replace("    let settled_after = sched::settle(tsc_hz);\n", "");
        let (problems, _) = check(&tree(&[file("framekernel/src/aspace.rs", &src)]));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("aspace.rs:1") && problems[0].contains("at all"));
    }

    #[test]
    fn a_settle_on_one_side_only_fails() {
        let only_after = SETTLED.replace("    let settled_before = sched::settle(tsc_hz);\n", "");
        let only_before = SETTLED.replace("    let settled_after = sched::settle(tsc_hz);\n", "");
        let (problems, _) = check(&tree(&[
            file("framekernel/src/a.rs", &only_after),
            file("framekernel/src/b.rs", &only_before),
        ]));
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(
            problems
                .iter()
                .any(|p| p.contains("a.rs") && p.contains("before its first read"))
        );
        assert!(
            problems
                .iter()
                .any(|p| p.contains("b.rs") && p.contains("after its first read"))
        );
    }

    /// A call of something else named `settle` (another module's, or a method) does not count;
    /// in `sched.rs` a bare `settle(` does.
    #[test]
    fn only_sched_settle_counts() {
        let other = SETTLED.replace("sched::settle(", "stacks::settle(");
        let method = SETTLED.replace("sched::settle(", "self.settle(");
        let in_sched = SETTLED.replace("sched::settle(", "settle(");
        let (problems, _) = check(&tree(&[
            file("framekernel/src/a.rs", &other),
            file("framekernel/src/b.rs", &method),
            file("framekernel/src/sched.rs", &in_sched),
        ]));
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems.iter().all(|p| !p.contains("sched.rs")));
    }

    #[test]
    fn a_listed_function_passes_and_a_stale_listing_fails() {
        let mut t = tree(&[]);
        let (path, function, _) = EXEMPT[0];
        let at = t.iter().position(|(rel, _)| rel == path).unwrap();
        t[at].1 = t[at].1.replace(
            &format!("fn {function}() {{\n    let _ = memory::stats();\n}}\n"),
            "",
        );
        let (problems, _) = check(&t);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains(function) && problems[0].contains("take it off the list"));
    }

    #[test]
    fn test_code_and_comments_are_not_read() {
        let src = "// memory::stats() in a comment\n#[cfg(test)]\nmod tests {\n    fn t() { let _ = memory::stats(); }\n}\nfn s() -> &'static str { \"memory::stats()\" }\n";
        let (problems, readers) = check(&tree(&[file("framekernel/src/x.rs", src)]));
        assert_eq!(problems, Vec::<String>::new());
        assert_eq!(readers, EXEMPT.len() + 1);
    }

    /// The checkout itself passes: what `cargo xtask check` runs.
    #[test]
    fn the_checkout_passes() {
        gate().unwrap();
    }
}
