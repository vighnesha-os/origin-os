//! `cargo xtask check`'s gate on the processor's two random-number instructions (CLAUDE.md
//! §59, decision item 113; ADR-0016 R2 and R3).
//!
//! §59: the kernel has one random-number generator, and nothing else draws from the
//! processor's instructions directly. This gate is what makes that checkable:
//!
//! - **`rdrand` and `rdseed` are run in two files only**, [`ALLOWED`]: the Framekernel's
//!   generator and the loader's. An assembly block anywhere else that names either fails the
//!   gate, and so does the compiler's intrinsic for either, anywhere at all.
//! - **Only the 64-bit forms are used** (R3; AMD-SB-7055 is about the shorter ones). In the
//!   two files, each of the instructions takes one operand, which is a format placeholder with
//!   no size modifier, or with `:r`, or a 64-bit register by name. The placeholder's register
//!   then has the width of the Rust value bound to it, which the compiler checks against the
//!   `u64` the functions return.
//! - **Each allowed file runs both.** A file that no longer does comes off the list, so the
//!   list cannot go stale.
//!
//! What it does not see: an instruction given as bytes (`.byte 0x48, 0x0f, 0xc7, ...`), and a
//! template the gate cannot read, which it refuses in the two files and cannot find elsewhere.
//! The second gate on these blocks is `asmflags`: both instructions write the carry flag, so
//! neither may sit in a block that declares `preserves_flags`.

use std::fs;

use crate::lex::{self, Kind, Token};
use crate::{Result, collect_files, repo_root};

/// The files that may run the instructions, and why.
const ALLOWED: &[(&str, &str)] = &[
    (
        "boot/uefi/src/random.rs",
        "the loader's generator: its slides and the seed it hands the kernel (ADR-0016 R10)",
    ),
    (
        "framekernel/frame/src/random.rs",
        "the Framekernel's generator: the only way the kernel gets a random value (R2)",
    ),
];

/// The two instructions.
const MNEMONICS: &[&str] = &["rdrand", "rdseed"];
/// The compiler's intrinsics for them (`core::arch::x86_64`), which need no assembly block.
const INTRINSICS: &[&str] = &[
    "_rdrand16_step",
    "_rdrand32_step",
    "_rdrand64_step",
    "_rdseed16_step",
    "_rdseed32_step",
    "_rdseed64_step",
];
/// The macros that hold assembly.
const MACROS: &[&str] = &["asm", "naked_asm", "global_asm"];
/// The general registers' 64-bit names.
const REGISTERS_64: &[&str] = &[
    "rax", "rbx", "rcx", "rdx", "rsi", "rdi", "rbp", "rsp", "r8", "r9", "r10", "r11", "r12", "r13",
    "r14", "r15",
];

/// One use of an instruction, or something the gate cannot accept, with its line.
#[derive(Debug, PartialEq, Eq)]
struct Found {
    line: usize,
    /// The instruction, if this is a use the gate could read: `rdrand` or `rdseed`.
    mnemonic: Option<&'static str>,
    /// What is wrong with it, wherever it is; `None` for a use that is in order.
    problem: Option<String>,
}

/// The gate: every Rust file of the repository.
pub(crate) fn gate() -> Result {
    let root = repo_root();
    let mut files = Vec::new();
    collect_files(&root, &|_| false, &|name| name.ends_with(".rs"), &mut files)
        .map_err(|e| format!("random-instruction gate: {e}"))?;
    let mut sources = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file)
            .map_err(|e| format!("random-instruction gate: {}: {e}", file.display()))?;
        let shown = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        sources.push((shown, text));
    }
    let (problems, uses) = judge(&sources, ALLOWED);
    if !problems.is_empty() {
        return Err(format!(
            "RDRAND and RDSEED are for the kernel's generator and the loader's alone, in their 64-bit forms (CLAUDE.md §59; ADR-0016 R2, R3; the list is ALLOWED in developer-tools/xtask/src/mnemonics.rs):\n  {}",
            problems.join("\n  ")
        ));
    }
    println!(
        "xtask: random-instruction gate: RDRAND and RDSEED are run {uses} times, in their 64-bit forms, in the {} files listed and nowhere else ({} Rust files read)",
        ALLOWED.len(),
        files.len()
    );
    Ok(())
}

/// Every problem in `sources` (path and text), given the files that may run the
/// instructions, and how many uses in order there are.
fn judge(sources: &[(String, String)], allowed: &[(&str, &str)]) -> (Vec<String>, usize) {
    let mut problems = Vec::new();
    let mut uses = 0;
    for &(path, _) in allowed {
        if !sources.iter().any(|(shown, _)| shown == path) {
            problems.push(format!(
                "{path}: on the list of files that may run the instructions, and there is no such file"
            ));
        }
    }
    for (path, text) in sources {
        let listed = allowed.iter().any(|&(file, _)| file == path);
        let found = uses_in(text);
        for use_ in &found {
            match (&use_.problem, use_.mnemonic) {
                (Some(problem), _) => {
                    problems.push(format!("{path}:{}: {problem}", use_.line));
                }
                (None, Some(mnemonic)) if !listed => problems.push(format!(
                    "{path}:{}: `{mnemonic}` outside the files that may run it: ask `origin_frame::random` for the value",
                    use_.line
                )),
                (None, _) => uses += 1,
            }
        }
        if listed {
            for mnemonic in MNEMONICS {
                if !found
                    .iter()
                    .any(|f| f.problem.is_none() && f.mnemonic == Some(*mnemonic))
                {
                    problems.push(format!(
                        "{path}: on the list of files that may run the instructions, and it does not run `{mnemonic}`: take it off the list, or put the instruction back"
                    ));
                }
            }
        }
    }
    (problems, uses)
}

/// Every use of the instructions in one source file, and every use the gate cannot accept.
fn uses_in(source: &str) -> Vec<Found> {
    let tokens = lex::tokens(source);
    let mut found = Vec::new();
    let mut i = 0;
    while let Some(token) = tokens.get(i) {
        let opens = token.kind == Kind::Ident
            && MACROS.contains(&token.text)
            && tokens.get(i + 1).is_some_and(|t| t.is_punct('!'))
            && tokens
                .get(i + 2)
                .is_some_and(|t| lex::bracket(t) == Some(1));
        if opens {
            let line = lex::line_of(source, token.start);
            let Some(close) = lex::closing(&tokens, i + 2) else {
                found.push(Found {
                    line,
                    mnemonic: None,
                    problem: Some(format!(
                        "`{}!` is never closed: the gate cannot read it",
                        token.text
                    )),
                });
                break;
            };
            let body = tokens.get(i + 3..close).unwrap_or(&[]);
            found.extend(block_uses(source, body));
            i = close + 1;
        } else {
            if token.kind == Kind::Ident
                && let Some(intrinsic) = INTRINSICS.iter().find(|name| token.is_ident(name))
            {
                found.push(Found {
                    line: lex::line_of(source, token.start),
                    mnemonic: None,
                    problem: Some(format!(
                        "`{intrinsic}`: the compiler's intrinsic runs the instruction outside the generator's own blocks"
                    )),
                });
            }
            i += 1;
        }
    }
    found
}

/// The uses in one assembly block, given its body.
fn block_uses(source: &str, body: &[Token<'_>]) -> Vec<Found> {
    let mut found = Vec::new();
    for token in body {
        let Kind::Str { raw } = token.kind else {
            continue;
        };
        let line = lex::line_of(source, token.start);
        let text = if raw {
            Some(token.text.to_string())
        } else {
            lex::unescape(token.text)
        };
        let Some(text) = text else {
            // An escape the gate cannot read: refuse it only if the raw text names an
            // instruction, so an unrelated template is left to the other gates.
            let lower = token.text.to_ascii_lowercase();
            if let Some(mnemonic) = MNEMONICS.iter().find(|m| lower.contains(**m)) {
                found.push(Found {
                    line,
                    mnemonic: Some(mnemonic),
                    problem: Some(format!(
                        "a template that names `{mnemonic}` with an escape the gate cannot read"
                    )),
                });
            }
            continue;
        };
        for statement in text.split(['\n', ';']) {
            if let Some(use_) = statement_use(statement.trim(), line) {
                found.push(use_);
            }
        }
    }
    found
}

/// What one statement of a template is, if it names one of the instructions.
fn statement_use(statement: &str, line: usize) -> Option<Found> {
    let lower = statement.to_ascii_lowercase();
    let mnemonic = MNEMONICS.iter().find(|m| lower.contains(**m))?;
    let problem = |what: String| {
        Some(Found {
            line,
            mnemonic: Some(mnemonic),
            problem: Some(what),
        })
    };
    // The statement is the mnemonic, white space and one operand, and nothing else: no
    // prefix, no label, no AT&T suffix, no second instruction hiding beside it.
    let Some(operand) = lower
        .strip_prefix(*mnemonic)
        .filter(|rest| rest.starts_with(char::is_whitespace))
        .map(str::trim)
    else {
        return problem(format!(
            "`{statement}` names `{mnemonic}` in a form the gate cannot read: write `{mnemonic} {{operand}}`"
        ));
    };
    let wide = match operand.strip_prefix('{').and_then(|o| o.strip_suffix('}')) {
        // One placeholder, with no modifier or the 64-bit one. Anything else between the
        // braces is not one placeholder: a second operand, say.
        Some(inner) => {
            let (name, modifier) = inner.split_once(':').unwrap_or((inner, "r"));
            name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && modifier == "r"
        }
        None => REGISTERS_64.contains(&operand),
    };
    if !wide {
        return problem(format!(
            "`{statement}`: only the 64-bit form of `{mnemonic}` is used (ADR-0016 R3; AMD-SB-7055): the operand must be a 64-bit register"
        ));
    }
    Some(Found {
        line,
        mnemonic: Some(mnemonic),
        problem: None,
    })
}

/// Whether `path` is one of the files the gate lists, for the tests of other gates.
#[cfg(test)]
pub(crate) fn is_allowed(path: &std::path::Path) -> bool {
    ALLOWED
        .iter()
        .any(|&(file, _)| std::path::Path::new(file) == path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const LISTED: &[(&str, &str)] = &[("kernel/random.rs", "the generator")];

    /// A generator file that runs both instructions as the real ones do.
    const GENERATOR: &str = r#"
        fn rdseed64() -> Option<u64> {
            let (value, ok): (u64, u8);
            unsafe {
                asm!("rdseed {v}", "setc {ok}", v = out(reg) value, ok = out(reg_byte) ok,
                     options(nomem, nostack));
            }
            (ok == 1).then_some(value)
        }
        fn rdrand64() -> Option<u64> {
            let (value, ok): (u64, u8);
            unsafe {
                asm!("rdrand {v}", "setc {ok}", v = out(reg) value, ok = out(reg_byte) ok,
                     options(nomem, nostack));
            }
            (ok == 1).then_some(value)
        }
    "#;

    fn judged(files: &[(&str, &str)]) -> (Vec<String>, usize) {
        let sources: Vec<(String, String)> = files
            .iter()
            .map(|(path, text)| (path.to_string(), text.to_string()))
            .collect();
        judge(&sources, LISTED)
    }

    fn assert_refused(files: &[(&str, &str)], needle: &str) {
        let (problems, _) = judged(files);
        assert!(
            problems.iter().any(|p| p.contains(needle)),
            "expected a problem naming {needle:?}, found {problems:#?}"
        );
    }

    #[test]
    fn the_two_instructions_in_the_listed_file_pass() {
        let (problems, uses) = judged(&[
            ("kernel/random.rs", GENERATOR),
            ("kernel/other.rs", r#"fn f() { unsafe { asm!("cpuid") } }"#),
        ]);
        assert!(problems.is_empty(), "{problems:#?}");
        assert_eq!(uses, 2);
    }

    #[test]
    fn either_instruction_anywhere_else_is_refused() {
        for mnemonic in ["rdrand", "rdseed", "RDSEED", "RdRand"] {
            let other =
                format!(r#"fn f() {{ unsafe {{ asm!("{mnemonic} {{v}}", v = out(reg) x) }} }}"#);
            assert_refused(
                &[("kernel/random.rs", GENERATOR), ("kernel/cpu.rs", &other)],
                "kernel/cpu.rs:1: `",
            );
            assert_refused(
                &[("kernel/random.rs", GENERATOR), ("kernel/cpu.rs", &other)],
                "outside the files that may run it",
            );
        }
        // In `global_asm!` and `naked_asm!` too, and on a later line of a block.
        for source in [
            "global_asm!(\"f:\", \"rdrand rax\", \"ret\");",
            "fn f() { naked_asm!(\"rdseed rax\", \"ret\") }",
            "fn f() { unsafe { asm!(\"nop\\nrdrand rax\") } }",
            "fn f() { unsafe { asm!(\"nop; rdseed rcx\") } }",
            "fn f() { unsafe { asm!(r\"rdseed {v}\", v = out(reg) x) } }",
        ] {
            assert_refused(
                &[("kernel/random.rs", GENERATOR), ("drivers/nic.rs", source)],
                "outside the files that may run it",
            );
        }
    }

    #[test]
    fn a_word_in_a_comment_or_a_string_is_not_a_use() {
        let (problems, uses) = judged(&[
            ("kernel/random.rs", GENERATOR),
            (
                "kernel/doc.rs",
                r#"
                // RDSEED and rdrand are run in random.rs.
                /// `rdrand` again.
                const NAME: &str = "rdrand {v}";
                fn f() { let rdseed = true; let _ = format!("asm!(\"rdrand {{v}}\")"); }
                "#,
            ),
        ]);
        assert!(problems.is_empty(), "{problems:#?}");
        assert_eq!(uses, 2);
    }

    #[test]
    fn the_shorter_forms_are_refused_in_the_listed_file() {
        for (operand, binding) in [
            ("{v:e}", "v = out(reg) value"),
            ("{v:x}", "v = out(reg) value"),
            ("eax", "out(\"eax\") value"),
            ("ax", "out(\"ax\") value"),
            ("r8d", "out(\"r8d\") value"),
            ("r10w", "out(\"r10w\") value"),
        ] {
            let source = GENERATOR.replacen(
                "asm!(\"rdseed {v}\", \"setc {ok}\", v = out(reg) value,",
                &format!("asm!(\"rdseed {operand}\", \"setc {{ok}}\", {binding},"),
                1,
            );
            assert_ne!(source, GENERATOR, "{operand}");
            assert_refused(&[("kernel/random.rs", &source)], "only the 64-bit form");
        }
        // The 64-bit forms by name and by modifier pass.
        for operand in ["{v:r}", "rax", "r15", "{}", "{0}"] {
            let source = GENERATOR.replacen("\"rdseed {v}\"", &format!("\"rdseed {operand}\""), 1);
            let (problems, uses) = judged(&[("kernel/random.rs", &source)]);
            assert!(problems.is_empty(), "{operand}: {problems:#?}");
            assert_eq!(uses, 2);
        }
    }

    #[test]
    fn a_form_the_gate_cannot_read_is_refused() {
        for template in [
            "lock rdseed {v}",
            "rdseedq {v}",
            "rdseed {v}, {w}",
            "2: rdrand {v}",
            "rdrand",
            "rdseed [rax]",
            "rdseed rax, rbx",
        ] {
            let source = GENERATOR.replacen("\"rdseed {v}\"", &format!("\"{template}\""), 1);
            let (problems, _) = judged(&[("kernel/random.rs", &source)]);
            assert!(
                problems
                    .iter()
                    .any(|p| p.contains("cannot read") || p.contains("only the 64-bit form")),
                "{template}: {problems:#?}"
            );
        }
        // An unclosed block ends the reading of the file, and says so.
        assert_refused(
            &[(
                "kernel/random.rs",
                "fn f() { unsafe { asm!(\"rdrand {v}\", v = out(reg) x",
            )],
            "never closed",
        );
    }

    #[test]
    fn the_intrinsics_are_refused_everywhere() {
        for intrinsic in INTRINSICS {
            let source = format!(
                "fn f(v: &mut u64) -> i32 {{ unsafe {{ core::arch::x86_64::{intrinsic}(v) }} }}"
            );
            assert_refused(
                &[("kernel/random.rs", GENERATOR), ("kernel/keys.rs", &source)],
                "the compiler's intrinsic",
            );
            // In the listed file as well: the generator's blocks are the only way.
            let both = format!("{GENERATOR}\n{source}");
            assert_refused(&[("kernel/random.rs", &both)], "the compiler's intrinsic");
        }
    }

    #[test]
    fn a_listed_file_must_exist_and_run_both() {
        assert_refused(&[("kernel/cpu.rs", "fn f() {}")], "there is no such file");
        let only_rdseed = GENERATOR.replace("rdrand {v}", "nop");
        assert_refused(
            &[("kernel/random.rs", &only_rdseed)],
            "does not run `rdrand`",
        );
        let only_rdrand = GENERATOR.replace("rdseed {v}", "nop");
        assert_refused(
            &[("kernel/random.rs", &only_rdrand)],
            "does not run `rdseed`",
        );
        // A use that is refused does not count as running it.
        let short = GENERATOR.replace("rdseed {v}", "rdseed {v:e}");
        assert_refused(&[("kernel/random.rs", &short)], "does not run `rdseed`");
    }

    #[test]
    fn the_real_list_names_two_files() {
        assert_eq!(ALLOWED.len(), 2);
        assert!(is_allowed(Path::new("framekernel/frame/src/random.rs")));
        assert!(is_allowed(Path::new("boot/uefi/src/random.rs")));
        assert!(!is_allowed(Path::new("framekernel/frame/src/cpu.rs")));
        assert!(!is_allowed(Path::new("boot/uefi/src/main.rs")));
        assert!(!is_allowed(Path::new("framekernel/src/entropy.rs")));
    }
}
