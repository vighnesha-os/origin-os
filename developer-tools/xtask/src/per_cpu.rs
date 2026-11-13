//! `cargo xtask check`'s gate on per-processor state (ADR-0008 Amendment 1, D1 and D5; TASKS
//! item 58). The kernel compiles in no processor count: every table of per-processor state is a
//! `PerCpu`, sized when the processors are discovered, in its crate's one sizing function.
//!
//! - **D1: every table is sized.** In each crate that holds `PerCpu` statics, the function
//!   `size_per_cpu` in that crate's `percpu.rs` must name each of them, as `crate::<module>::<NAME>`,
//!   once, and name nothing else. A table it leaves out would hold the boot processor's slot
//!   alone, and every other processor would find nothing there.
//! - **D5: no count comes back.** No kernel source names `MAX_CPUS`, and no `static` in one is an
//!   array whose length names a processor count (an identifier containing `CPU` or `PROCESSOR`).
//!   Host tests and Loom models keep small fixed machines of their own: `tests.rs` files, the
//!   Loom models (`models.rs`) and `#[cfg(test)]` modules are not read.
//!
//! It reads tokens (`lex`), so a name in a comment or a string does not count. It says nothing
//! about what the kernel does at run time: the boot proves that every table holds every index
//! (the "CPU indexes" line).

use std::collections::BTreeMap;

use crate::lex::{self, Kind, Token};
use crate::{Result, collect_files, repo_root};

/// The kernel's sources: where D5 holds.
const KERNEL: &str = "framekernel";

/// The crates whose `PerCpu` statics a sizing function must name: each crate's source directory
/// and the file holding its `size_per_cpu`.
const CRATES: &[(&str, &str)] = &[
    ("framekernel/frame/src", "framekernel/frame/src/percpu.rs"),
    ("framekernel/src", "framekernel/src/percpu.rs"),
];

/// The sizing function's name in each crate.
const SIZING: &str = "size_per_cpu";

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
    let problems = check(&sources);
    if problems.is_empty() {
        let tables: usize = CRATES
            .iter()
            .map(|&(dir, _)| {
                sources
                    .iter()
                    .filter(|(rel, _)| rel.starts_with(dir) && is_read(rel))
                    .map(|(rel, src)| per_cpu_statics(rel, src).len())
                    .sum::<usize>()
            })
            .sum();
        println!(
            "xtask: per-CPU gate: {tables} PerCpu tables in {} crates, each sized by its crate's `{SIZING}`; no `MAX_CPUS` and no static array sized by a processor count in {} kernel sources",
            CRATES.len(),
            sources.iter().filter(|(rel, _)| is_read(rel)).count()
        );
        return Ok(());
    }
    Err(format!(
        "the per-CPU gate (ADR-0008 Amendment 1, D1 and D5; developer-tools/xtask/src/per_cpu.rs):\n  {}",
        problems.join("\n  ")
    ))
}

/// Whether the gate reads `rel`: not a test file or the Loom models, which keep fixed machines.
fn is_read(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    rel.ends_with(".rs") && name != "tests.rs" && name != "models.rs" && !rel.contains("/tests/")
}

/// Every problem the gate finds in `sources` (repository-relative path, text).
pub(crate) fn check(sources: &[(String, String)]) -> Vec<String> {
    let mut problems = Vec::new();
    let read: Vec<&(String, String)> = sources.iter().filter(|(rel, _)| is_read(rel)).collect();
    // D5.
    for (rel, src) in &read {
        problems.extend(count_problems(rel, src));
    }
    // D1: each crate's statics against its sizing function.
    let mut tables: BTreeMap<&str, Vec<(String, String, usize)>> = BTreeMap::new();
    for (rel, src) in &read {
        let Some(&(dir, _)) = CRATES
            .iter()
            .find(|(dir, _)| rel.starts_with(&format!("{dir}/")))
        else {
            for (name, line) in per_cpu_statics(rel, src) {
                problems.push(format!(
                    "{rel}:{line}: `{name}` is a PerCpu table in a crate with no `{SIZING}` this gate knows"
                ));
            }
            continue;
        };
        for (name, line) in per_cpu_statics(rel, src) {
            let path = module_path(dir, rel).map_or(name.clone(), |m| format!("{m}::{name}"));
            tables
                .entry(dir)
                .or_default()
                .push((path, rel.to_string(), line));
        }
    }
    for &(dir, sizing_file) in CRATES {
        let found = tables.remove(dir).unwrap_or_default();
        let Some((_, src)) = sources.iter().find(|(rel, _)| rel == sizing_file) else {
            if !found.is_empty() {
                problems.push(format!(
                    "{sizing_file}: missing, and its crate has {} PerCpu tables",
                    found.len()
                ));
            }
            continue;
        };
        let Some(named) = sized_names(src) else {
            problems.push(format!("{sizing_file}: no `fn {SIZING}`"));
            continue;
        };
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for name in &named {
            *seen.entry(name.as_str()).or_default() += 1;
        }
        for (path, rel, line) in &found {
            match seen.get(path.as_str()) {
                None => problems.push(format!(
                    "{rel}:{line}: `{path}` is a PerCpu table that `{SIZING}` in {sizing_file} does not size"
                )),
                Some(&n) if n > 1 => problems.push(format!(
                    "{sizing_file}: `{SIZING}` sizes `{path}` {n} times; a table is sized once"
                )),
                Some(_) => {}
            }
        }
        for name in seen.keys() {
            if !found.iter().any(|(path, _, _)| path == name) {
                problems.push(format!(
                    "{sizing_file}: `{SIZING}` names `crate::{name}`, which is no PerCpu static of its crate"
                ));
            }
        }
    }
    problems
}

/// The module path of `rel` within the crate whose sources are in `dir`: `None` for the crate
/// root.
fn module_path(dir: &str, rel: &str) -> Option<String> {
    let inner = rel.strip_prefix(dir)?.trim_start_matches('/');
    let inner = inner.strip_suffix(".rs")?;
    let inner = inner.strip_suffix("/mod").unwrap_or(inner);
    if inner == "lib" || inner == "main" {
        return None;
    }
    Some(inner.replace('/', "::"))
}

/// Token ranges `[start, end)` that only tests compile: an item after `#[cfg(test)]` or a
/// `#[cfg(all(test, …))]` (not `not(test)`), through its closing brace.
fn test_ranges(tokens: &[Token<'_>]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let is_cfg = tokens.get(i).is_some_and(|t| t.is_punct('#'))
            && tokens.get(i + 1).is_some_and(|t| t.is_punct('['))
            && tokens.get(i + 2).is_some_and(|t| t.is_ident("cfg"));
        if !is_cfg {
            i += 1;
            continue;
        }
        let Some(close) = lex::closing(tokens, i + 1) else {
            break;
        };
        let args = &tokens[i + 2..close];
        let tests_only = args.iter().any(|t| t.is_ident("test"))
            && !args.iter().any(|t| t.is_ident("not"))
            && !args.iter().any(|t| t.is_ident("any"));
        if tests_only {
            // The item: up to its first `{` (its body) or `;`.
            let mut j = close + 1;
            while j < tokens.len() && !tokens[j].is_punct('{') && !tokens[j].is_punct(';') {
                j += 1;
            }
            let end = if tokens.get(j).is_some_and(|t| t.is_punct('{')) {
                lex::closing(tokens, j).map_or(tokens.len(), |c| c + 1)
            } else {
                j + 1
            };
            out.push((i, end));
            i = end;
        } else {
            i = close + 1;
        }
    }
    out
}

/// Whether token `i` lies in one of `ranges`.
fn in_ranges(ranges: &[(usize, usize)], i: usize) -> bool {
    ranges.iter().any(|&(a, b)| (a..b).contains(&i))
}

/// The `static` items of `src` outside test code: each one's name, the tokens of its type, and
/// its line.
fn statics<'a>(src: &'a str, tokens: &'a [Token<'a>]) -> Vec<(String, &'a [Token<'a>], usize)> {
    let tests = test_ranges(tokens);
    let mut out = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        if !t.is_ident("static") || in_ranges(&tests, i) {
            continue;
        }
        let mut j = i + 1;
        if tokens.get(j).is_some_and(|t| t.is_ident("mut")) {
            j += 1;
        }
        let Some(name) = tokens.get(j).filter(|t| t.kind == Kind::Ident) else {
            continue;
        };
        if !tokens.get(j + 1).is_some_and(|t| t.is_punct(':')) {
            continue;
        }
        let start = j + 2;
        let mut end = start;
        let mut depth = 0i32;
        while let Some(t) = tokens.get(end) {
            if depth == 0 && (t.is_punct('=') || t.is_punct(';')) {
                break;
            }
            depth += lex::bracket(t).unwrap_or(0);
            if t.is_punct('<') {
                depth += 1;
            } else if t.is_punct('>') {
                depth -= 1;
            }
            end += 1;
        }
        out.push((
            name.text.to_string(),
            &tokens[start..end],
            lex::line_of(src, t.start),
        ));
    }
    out
}

/// The `PerCpu` statics of `src`, outside test code: each one's name and line.
fn per_cpu_statics(_rel: &str, src: &str) -> Vec<(String, usize)> {
    let tokens = lex::tokens(src);
    statics(src, &tokens)
        .into_iter()
        .filter(|(_, ty, _)| {
            // The type's own name: the last identifier before its first `<`.
            let head = ty
                .iter()
                .take_while(|t| !t.is_punct('<'))
                .collect::<Vec<_>>();
            head.iter()
                .rev()
                .find(|t| t.kind == Kind::Ident)
                .is_some_and(|t| t.text == "PerCpu")
        })
        .map(|(name, _, line)| (name, line))
        .collect()
}

/// The `crate::…` paths `size_per_cpu` names in `src`, without `crate::`, or `None` if it has
/// no such function.
fn sized_names(src: &str) -> Option<Vec<String>> {
    let tokens = lex::tokens(src);
    let at = tokens
        .windows(2)
        .position(|w| w[0].is_ident("fn") && w[1].is_ident(SIZING))?;
    let open = (at..tokens.len()).find(|&i| tokens[i].is_punct('{'))?;
    let close = lex::closing(&tokens, open)?;
    let body = &tokens[open..close];
    let mut out = Vec::new();
    let mut i = 0;
    while i < body.len() {
        if !body[i].is_ident("crate") {
            i += 1;
            continue;
        }
        let mut parts = Vec::new();
        let mut j = i + 1;
        while body.get(j).is_some_and(|t| t.is_punct(':'))
            && body.get(j + 1).is_some_and(|t| t.is_punct(':'))
            && body.get(j + 2).is_some_and(|t| t.kind == Kind::Ident)
        {
            parts.push(body[j + 2].text);
            j += 3;
        }
        if !parts.is_empty() {
            out.push(parts.join("::"));
        }
        i = j.max(i + 1);
    }
    Some(out)
}

/// D5's problems in one file: `MAX_CPUS` named, or a static array sized by a processor count.
fn count_problems(rel: &str, src: &str) -> Vec<String> {
    let tokens = lex::tokens(src);
    let tests = test_ranges(&tokens);
    let mut out = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        if t.is_ident("MAX_CPUS") && !in_ranges(&tests, i) {
            out.push(format!(
                "{rel}:{}: names `MAX_CPUS`: no processor count is compiled in (D5)",
                lex::line_of(src, t.start)
            ));
        }
    }
    for (name, ty, line) in statics(src, &tokens) {
        // Every array length in the type: what follows a `;` up to the bracket that closes it.
        let mut depth_after_semicolon = None;
        let mut depth = 0i32;
        for t in ty {
            if let Some(d) = lex::bracket(t) {
                depth += d;
                if depth_after_semicolon.is_some_and(|at| depth < at) {
                    depth_after_semicolon = None;
                }
                continue;
            }
            if t.is_punct(';') {
                depth_after_semicolon = Some(depth);
                continue;
            }
            if depth_after_semicolon.is_some() && t.kind == Kind::Ident {
                let upper = t.text.to_ascii_uppercase();
                if upper.contains("CPU") || upper.contains("PROCESSOR") {
                    out.push(format!(
                        "{rel}:{line}: `{name}` is a static array sized by `{}`, a processor count: make it a PerCpu table (D5)",
                        t.text
                    ));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(rel: &str, src: &str) -> (String, String) {
        (rel.to_string(), src.to_string())
    }

    const SIZING_FRAME: &str = "pub(crate) fn size_per_cpu(sizing: &mut Sizing) {\n    sizing.table(&crate::cpu::CPU_LOCAL);\n    sizing.table(&crate::tlb::RECORDS);\n}\n";

    fn tree(extra: &[(String, String)]) -> Vec<(String, String)> {
        let mut out = vec![
            file("framekernel/frame/src/percpu.rs", SIZING_FRAME),
            file(
                "framekernel/frame/src/cpu.rs",
                "pub(crate) static CPU_LOCAL: PerCpu<CpuLocal> = PerCpu::new(CpuLocal::new(), CpuLocal::new);\nstatic OTHER: AtomicU64 = AtomicU64::new(0);\n",
            ),
            file(
                "framekernel/frame/src/tlb.rs",
                "pub(crate) static RECORDS: crate::percpu::PerCpu<Record> =\n    PerCpu::new(Record::new(), Record::new);\n",
            ),
            file(
                "framekernel/src/percpu.rs",
                "pub(crate) fn size_per_cpu(sizing: &mut Sizing) {\n    sizing.table(&crate::sched::QUEUES);\n}\n",
            ),
            file(
                "framekernel/src/sched.rs",
                "static QUEUES: origin_frame::percpu::PerCpu<SpinLock<RunQueue>> = x;\n#[cfg(test)]\nmod tests {\n    static T: PerCpu<u8> = PerCpu::new(0, || 0);\n    static A: [u8; MAX_CPUS] = [0; MAX_CPUS];\n}\n",
            ),
        ];
        out.extend_from_slice(extra);
        out
    }

    #[test]
    fn a_tree_whose_tables_are_all_sized_passes() {
        assert_eq!(check(&tree(&[])), Vec::<String>::new());
    }

    #[test]
    fn a_table_left_out_of_the_sizing_function_fails() {
        let problems = check(&tree(&[file(
            "framekernel/frame/src/smp.rs",
            "pub(crate) static TOPOLOGY: PerCpu<SpinLock<Option<CpuTopology>>> =\n    PerCpu::new(SpinLock::new(None), || SpinLock::new(None));\n",
        )]));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("smp::TOPOLOGY") && problems[0].contains("does not size"));
    }

    #[test]
    fn a_stale_or_doubled_name_in_the_sizing_function_fails() {
        let mut t = tree(&[]);
        t[0].1 = SIZING_FRAME.replace(
            "    sizing.table(&crate::tlb::RECORDS);\n",
            "    sizing.table(&crate::tlb::RECORDS);\n    sizing.table(&crate::tlb::RECORDS);\n    sizing.table(&crate::gone::TABLE);\n",
        );
        let problems = check(&t);
        assert!(
            problems.iter().any(|p| p.contains("2 times")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("gone::TABLE")),
            "{problems:?}"
        );
    }

    #[test]
    fn max_cpus_and_arrays_sized_by_a_processor_count_fail_outside_tests() {
        let problems = check(&tree(&[
            file(
                "framekernel/frame/src/limits.rs",
                "pub const MAX_CPUS: usize = 64;\n// MAX_CPUS in a comment is fine\n",
            ),
            file(
                "framekernel/frame/sync/src/thing.rs",
                "static A: [AtomicU64; CPU_COUNT] = x;\nstatic B: [[u8; 4]; crate::NPROCESSORS] = x;\nstatic C: [u8; 16] = [0; 16];\n",
            ),
            file(
                "framekernel/frame/sync/src/models.rs",
                "static M: [u8; SHOOT_CPUS] = x;\n",
            ),
            file(
                "framekernel/src/pin/tests.rs",
                "static T: [u8; MAX_CPUS] = x;\n",
            ),
        ]));
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems[0].contains("limits.rs:1") && problems[0].contains("MAX_CPUS"));
        assert!(
            problems
                .iter()
                .any(|p| p.contains("`A`") && p.contains("CPU_COUNT"))
        );
        assert!(
            problems
                .iter()
                .any(|p| p.contains("`B`") && p.contains("NPROCESSORS"))
        );
    }

    #[test]
    fn a_table_in_a_crate_without_a_sizing_function_fails() {
        let problems = check(&tree(&[file(
            "framekernel/frame/sync/src/x.rs",
            "static T: PerCpu<u8> = PerCpu::new(0, || 0);\n",
        )]));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("no `size_per_cpu`"));
    }

    #[test]
    fn module_paths_follow_the_files() {
        let dir = "framekernel/frame/src";
        assert_eq!(
            module_path(dir, "framekernel/frame/src/cpu.rs").as_deref(),
            Some("cpu")
        );
        assert_eq!(
            module_path(dir, "framekernel/frame/src/iommu/amd.rs").as_deref(),
            Some("iommu::amd")
        );
        assert_eq!(
            module_path(dir, "framekernel/frame/src/a/mod.rs").as_deref(),
            Some("a")
        );
        assert_eq!(module_path(dir, "framekernel/frame/src/lib.rs"), None);
    }

    /// The checkout itself passes: what `cargo xtask check` runs.
    #[test]
    fn the_checkout_passes() {
        gate().unwrap();
    }
}
