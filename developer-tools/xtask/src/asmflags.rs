//! `cargo xtask check`'s gate on the `preserves_flags` option of assembly blocks (CLAUDE.md §13,
//! decision item 110). It is part of the gate on `unsafe` (`unsafety::gate`).
//!
//! The option is a promise to the compiler: when the block ends, the status flags, the x87
//! status word and the exception flags of MXCSR are as the block found them. The compiler may
//! then keep a comparison's result in the flags across the block, so a block that breaks the
//! promise is undefined behaviour, whether or not today's compiler uses the freedom.
//!
//! The rule here: a block that declares the option may hold only instructions on
//! [`FLAG_FREE`]. Anything the gate cannot read fails it: a template that is not a plain string
//! literal, AT&T syntax, a label, a directive, or the option given anywhere but inside `asm!`,
//! `naked_asm!` or `global_asm!` (a macro that wraps one, say).
//!
//! The evidence for the list, and for what is left off it, is
//! `docs/research/2026-11-10-asm-flags.md`.

use std::fs;
use std::path::Path;

use crate::lex::{self, Kind, Token};
use crate::{Result, collect_files, repo_root};

/// The instructions a block that declares `preserves_flags` may hold, by mnemonic, in Intel
/// syntax. Each is here because both vendors' manuals say it changes no status flag: Intel's
/// SDM Volume 2 (325383-093US, September 2026) under "Flags Affected", and AMD's APM Volume 3
/// (24594 Rev 3.39) or Volume 4 (26568 Rev 3.27) under "rFLAGS Affected". None is an x87, MMX or
/// SSE instruction, so none writes the x87 status word or MXCSR. The pages are in the ledger's
/// section 6.
///
/// To add one, read both entries on the day and record them there first. "Everyone knows it
/// leaves the flags alone" is how the blocks this gate was written for came to be wrong.
const FLAG_FREE: &[&str] = &[
    // Writes IF (or VIF) and no other flag. The option covers the six status flags only.
    "cli", "cpuid", "in", "invlpg",
    // Not to or from a control or debug register: see `privileged_register`.
    "mov", "out", // `popfq` is another mnemonic, and is not here.
    "pop", "pushfq", "rdmsr", "rdtsc", "rdtscp", "wrmsr", "xchg", "xgetbv", "xsetbv",
];

/// The macros that hold assembly. Only `asm!` accepts the option; the other two are read the
/// same way, so the gate does not depend on the compiler refusing it there.
const MACROS: &[&str] = &["asm", "naked_asm", "global_asm"];
/// The option this gate is about.
const OPTION: &str = "preserves_flags";
/// What an operand of an assembly macro starts with, after an optional `name =`.
const OPERAND_HEADS: &[&str] = &[
    "in",
    "out",
    "inout",
    "lateout",
    "inlateout",
    "const",
    "sym",
    "label",
    "options",
    "clobber_abi",
];

/// Something wrong in one file, and the line it is on.
#[derive(Debug, PartialEq, Eq)]
struct Problem {
    line: usize,
    what: String,
}

/// What a scan read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Seen {
    /// Assembly blocks of any kind.
    blocks: usize,
    /// Those that declare the option.
    declaring: usize,
}

/// The gate: every Rust file of the repository, host tools included.
pub(crate) fn gate() -> Result {
    let root = repo_root();
    let (problems, seen, files) = scan(&root)?;
    if seen.blocks == 0 {
        // The kernel cannot exist without assembly: this scan looked in the wrong place.
        return Err(format!(
            "assembly gate: no assembly block in {files} Rust files: nothing was checked"
        ));
    }
    if !problems.is_empty() {
        return Err(format!(
            "`{OPTION}` on assembly that may not keep that promise (CLAUDE.md §13; docs/research/2026-11-10-asm-flags.md):\n  {}\nremove the option from the block: it is a hint to the compiler, and nothing depends on it",
            problems.join("\n  ")
        ));
    }
    println!(
        "xtask: assembly gate: {} of {} assembly blocks declare `{OPTION}`, each holding only instructions known to leave the flags alone ({files} Rust files read)",
        seen.declaring, seen.blocks
    );
    Ok(())
}

/// Every problem under `root`, as `path:line: what`, with what was read and in how many files.
fn scan(root: &Path) -> Result<(Vec<String>, Seen, usize)> {
    let mut files = Vec::new();
    collect_files(root, &|_| false, &|name| name.ends_with(".rs"), &mut files)
        .map_err(|e| format!("assembly gate: {e}"))?;
    let mut problems = Vec::new();
    let mut seen = Seen::default();
    for file in &files {
        let text = fs::read_to_string(file)
            .map_err(|e| format!("assembly gate: {}: {e}", file.display()))?;
        let (found, here) = problems_in(&text);
        seen.blocks += here.blocks;
        seen.declaring += here.declaring;
        let shown = file.strip_prefix(root).unwrap_or(file);
        problems.extend(
            found
                .into_iter()
                .map(|p| format!("{}:{}: {}", shown.display(), p.line, p.what)),
        );
    }
    Ok((problems, seen, files.len()))
}

/// What is wrong in one source file, and what was read there.
fn problems_in(source: &str) -> (Vec<Problem>, Seen) {
    let tokens = lex::tokens(source);
    let mut problems = Vec::new();
    let mut seen = Seen::default();
    let mut i = 0;
    while let Some(token) = tokens.get(i) {
        let opens = token.kind == Kind::Ident
            && MACROS.contains(&token.text)
            && tokens.get(i + 1).is_some_and(|t| t.is_punct('!'))
            && tokens.get(i + 2).is_some_and(|t| delimiter(t) == Some(1));
        if opens {
            let line = lex::line_of(source, token.start);
            let Some(close) = closing(&tokens, i + 2) else {
                problems.push(Problem {
                    line,
                    what: format!("`{}!` is never closed: the gate cannot read it", token.text),
                });
                break;
            };
            seen.blocks += 1;
            let body = tokens.get(i + 3..close).unwrap_or(&[]);
            if body.iter().any(|t| t.is_ident(OPTION)) {
                seen.declaring += 1;
                problems.extend(
                    block_problems(body)
                        .into_iter()
                        .map(|what| Problem { line, what }),
                );
            }
            i = close + 1;
        } else {
            if token.is_ident(OPTION) {
                problems.push(Problem {
                    line: lex::line_of(source, token.start),
                    what: format!(
                        "`{OPTION}` outside an `asm!` block: the gate cannot read the instructions it is about (a macro that wraps `asm!`?)"
                    ),
                });
            }
            i += 1;
        }
    }
    (problems, seen)
}

/// `Some(1)` for an opening bracket, `Some(-1)` for a closing one.
fn delimiter(token: &Token<'_>) -> Option<i32> {
    if token.kind != Kind::Punct {
        return None;
    }
    match token.text {
        "(" | "[" | "{" => Some(1),
        ")" | "]" | "}" => Some(-1),
        _ => None,
    }
}

/// The index of the bracket that closes the one at `open`.
fn closing(tokens: &[Token<'_>], open: usize) -> Option<usize> {
    let mut depth = 0;
    for (i, token) in tokens.iter().enumerate().skip(open) {
        depth += delimiter(token).unwrap_or(0);
        if depth == 0 {
            return Some(i);
        }
    }
    None
}

/// The arguments of a macro: its body split at the commas outside every bracket.
fn arguments<'t, 'a>(body: &'t [Token<'a>]) -> Vec<&'t [Token<'a>]> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, token) in body.iter().enumerate() {
        depth += delimiter(token).unwrap_or(0);
        if depth == 0 && token.is_punct(',') {
            out.push(body.get(start..i).unwrap_or(&[]));
            start = i + 1;
        }
    }
    out.push(body.get(start..).unwrap_or(&[]));
    out.retain(|argument| !argument.is_empty());
    out
}

/// What is wrong with a block that declares the option, given its body.
fn block_problems(body: &[Token<'_>]) -> Vec<String> {
    let mut problems = Vec::new();
    let arguments = arguments(body);
    // The templates come first: each a string literal by itself.
    let templates = arguments
        .iter()
        .take_while(|a| matches!(a, [only] if matches!(only.kind, Kind::Str { .. })))
        .count();
    let (templates, operands) = arguments.split_at(templates);
    let mut options = Vec::new();
    for &operand in operands {
        // `name = in(reg) value`: the name is not what the operand is.
        let rest = match operand {
            [name, equals, rest @ ..]
                if matches!(name.kind, Kind::Ident | Kind::RawIdent) && equals.is_punct('=') =>
            {
                rest
            }
            whole => whole,
        };
        match rest.first() {
            Some(head) if head.is_ident("options") => {
                options.extend(
                    rest.iter()
                        .filter(|t| t.kind == Kind::Ident)
                        .map(|t| t.text),
                );
            }
            Some(head) if head.kind == Kind::Ident && OPERAND_HEADS.contains(&head.text) => {}
            _ => problems.push(format!(
                "`{OPTION}` on a block with an argument the gate cannot read, `{}`: a template must be a plain string literal",
                shown(operand)
            )),
        }
    }
    for syntax in ["att_syntax", "raw"] {
        if options.contains(&syntax) {
            problems.push(format!(
                "`{OPTION}` with `{syntax}`: the gate reads Intel-syntax format strings only"
            ));
        }
    }
    if templates.is_empty() {
        problems.push(format!(
            "`{OPTION}` on a block with no template the gate can read"
        ));
    }
    for template in templates {
        let Some(token) = template.first() else {
            continue;
        };
        let text = match token.kind {
            Kind::Str { raw: true } => Some(token.text.to_string()),
            _ => lex::unescape(token.text),
        };
        let Some(text) = text else {
            problems.push(format!(
                "`{OPTION}` on a template with an escape the gate cannot read: \"{}\"",
                token.text
            ));
            continue;
        };
        let text = without_placeholders(&text);
        for statement in text.split(['\n', ';']) {
            if let Some(why) = statement_problem(statement.trim()) {
                problems.push(format!(
                    "`{OPTION}` on a block that holds `{}`: {why}",
                    statement.trim()
                ));
            }
        }
    }
    problems
}

/// A template with each format placeholder (`{}`, `{0}`, `{name:r}`) replaced by `{}`, and the
/// doubled braces that stand for one brace left as one. A placeholder is always a general
/// register, a constant or a symbol, never an instruction or a control register, so its name
/// must not be read as either.
fn without_placeholders(template: &str) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                for inner in chars.by_ref() {
                    if inner == '}' {
                        break;
                    }
                }
                out.push_str("{}");
            }
            other => out.push(other),
        }
    }
    out
}

/// Why `statement` may not stand in a block that declares the option, or `None` if it may.
fn statement_problem(statement: &str) -> Option<String> {
    if statement.is_empty() {
        return None;
    }
    let end = statement
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(statement.len());
    let (mnemonic, operands) = statement.split_at(end);
    let starts_with_a_letter = mnemonic
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic());
    // After the mnemonic comes white space or nothing: `2:` and `.byte` are not instructions,
    // and neither is a mnemonic the gate would have to cut out of something longer.
    let ends_cleanly = operands.chars().next().is_none_or(char::is_whitespace);
    if !starts_with_a_letter || !ends_cleanly {
        return Some(
            "not an instruction the gate can read (a label, a directive, a prefix or a comment)"
                .to_string(),
        );
    }
    let mnemonic = mnemonic.to_ascii_lowercase();
    if !FLAG_FREE.contains(&mnemonic.as_str()) {
        return Some(format!(
            "`{mnemonic}` is not on the list of instructions known to leave the flags alone"
        ));
    }
    if mnemonic == "mov"
        && let Some(register) = privileged_register(operands)
    {
        return Some(format!(
            "a move to or from `{register}`: Intel's manual leaves all six status flags undefined after it"
        ));
    }
    None
}

/// The control, debug or test register among `operands`, if there is one: `cr0` to `cr15`,
/// `dr0` to `dr15` (which assemblers also spell `db0` to `db15`) and `tr0` to `tr7`.
fn privileged_register(operands: &str) -> Option<String> {
    operands
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .map(str::to_ascii_lowercase)
        .find(|word| {
            ["cr", "dr", "db", "tr"].iter().any(|prefix| {
                word.strip_prefix(prefix)
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
        })
}

/// A macro argument as text, for a message.
fn shown(tokens: &[Token<'_>]) -> String {
    tokens.iter().map(|t| t.text).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The problems in `source`, as text.
    fn found(source: &str) -> Vec<String> {
        problems_in(source).0.into_iter().map(|p| p.what).collect()
    }

    fn assert_clean(source: &str) {
        let problems = found(source);
        assert!(problems.is_empty(), "{source:?}: {problems:#?}");
    }

    fn assert_caught(source: &str, needle: &str) {
        let problems = found(source);
        assert!(
            problems.iter().any(|p| p.contains(needle)),
            "expected a problem naming {needle:?} in {source:?}, found {problems:?}"
        );
    }

    #[test]
    fn the_five_blocks_of_the_review_are_caught() {
        // `enable_smep_smap` as it was: `or` writes the flags.
        let source = r#"
            unsafe {
                asm!(
                    "mov {tmp}, cr4",
                    "or {tmp}, {bits}",
                    "mov cr4, {tmp}",
                    bits = in(reg) bits,
                    tmp = out(reg) _,
                    options(nomem, nostack, preserves_flags),
                );
            }
        "#;
        let problems = found(source);
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert_caught(source, "`or` is not on the list");
        assert_caught(source, "a move to or from `cr4`");
        // `enable_simd`'s: `and` too.
        assert_caught(
            r#"asm!("mov {t}, cr0", "and {t}, {clear}", "or {t}, {set}", "mov cr0, {t}", t = out(reg) _, options(preserves_flags));"#,
            "`and` is not on the list",
        );
        // The same block without the option says nothing the gate can fault.
        assert_clean(&source.replace(", preserves_flags", ""));
    }

    #[test]
    fn a_move_to_or_from_a_control_or_debug_register_is_caught() {
        for (template, register) in [
            ("mov {}, cr4", "cr4"),
            ("mov {}, cr0", "cr0"),
            ("mov {}, cr2", "cr2"),
            ("mov cr3, {}", "cr3"),
            ("mov rax, CR8", "cr8"),
            ("mov dr7, {}", "dr7"),
            ("mov {}, dr6", "dr6"),
            ("mov rax, db7", "db7"),
            ("MOV RAX, CR3", "cr3"),
        ] {
            let source = format!(
                r#"unsafe {{ asm!("{template}", out(reg) value, options(nomem, nostack, preserves_flags)) }};"#
            );
            assert_caught(&source, &format!("a move to or from `{register}`"));
        }
        // An ordinary move is not one, whatever its operands are called.
        for template in [
            "mov {}, rsp",
            "mov {}, gs:[0]",
            "mov {cr3}, rax",
            "mov rax, [crash]",
            "mov rax, [rip + cr3_shadow]",
            "mov eax, 0xdb7",
        ] {
            assert_clean(&format!(
                r#"asm!("{template}", out(reg) v, cr3 = out(reg) _, options(preserves_flags));"#
            ));
        }
    }

    #[test]
    fn the_listed_instructions_pass() {
        let source = r#"
            asm!("mov {save:r}, rbx", "cpuid", "xchg {save:r}, rbx", save = out(reg) ebx,
                inout("eax") leaf => eax, inout("ecx") 0u32 => ecx, out("edx") edx,
                options(nomem, nostack, preserves_flags));
            asm!("pushfq", "pop {}", "cli", out(reg) flags, options(preserves_flags));
            asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags));
            asm!("wrmsr", in("ecx") msr, in("eax") low, in("edx") high, options(nomem, nostack, preserves_flags));
            asm!("rdtsc", out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags));
            asm!("rdtscp", out("eax") low, out("edx") high, out("ecx") _, options(nomem, nostack, preserves_flags));
            asm!("invlpg [{}]", in(reg) addr, options(nostack, preserves_flags));
            asm!("xgetbv", in("ecx") reg, out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags));
            asm!("xsetbv", in("ecx") reg, in("eax") low, in("edx") high, options(nomem, nostack, preserves_flags));
            asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
            asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack, preserves_flags));
            core::arch::asm!("mov {}, rbp", out(reg) rbp, options(nomem, nostack, preserves_flags));
        "#;
        assert_clean(source);
        assert_eq!(
            problems_in(source).1,
            Seen {
                blocks: 12,
                declaring: 12
            }
        );
    }

    #[test]
    fn what_was_left_off_the_list_is_caught() {
        for (template, mnemonic) in [
            ("lfence", "lfence"),
            ("clflush [{}]", "clflush"),
            ("xsave [{a}]", "xsave"),
            ("fxsave [{a}]", "fxsave"),
            ("xrstor [{a}]", "xrstor"),
            ("fxrstor [{a}]", "fxrstor"),
            ("movq xmm0, {0}", "movq"),
            ("rdrand {v}", "rdrand"),
            ("sti", "sti"),
            ("popfq", "popfq"),
            ("add rax, 1", "add"),
            ("test {a}, {a}", "test"),
            ("cmp eax, 1", "cmp"),
            ("xor eax, eax", "xor"),
            ("syscall", "syscall"),
        ] {
            let source = format!(r#"asm!("{template}", options(preserves_flags));"#);
            assert_caught(&source, &format!("`{mnemonic}` is not on the list"));
        }
    }

    #[test]
    fn every_statement_of_every_template_is_read() {
        // Statements divided by a line break inside one literal, by `;`, and by a raw string.
        assert_caught(
            r#"asm!("cpuid\nor eax, 1", options(preserves_flags));"#,
            "`or` is not on the list",
        );
        assert_caught(
            r#"asm!("cpuid; and eax, 1", options(preserves_flags));"#,
            "`and` is not on the list",
        );
        assert_caught(
            "asm!(r\"cpuid\n sub eax, 1\", options(preserves_flags));",
            "`sub` is not on the list",
        );
        assert_caught(
            "asm!(\"cpuid \\\n      ; neg eax\", options(preserves_flags));",
            "`neg` is not on the list",
        );
        // The last template counts as much as the first.
        assert_caught(
            r#"asm!("cpuid", "cpuid", "cpuid", "inc eax", options(preserves_flags));"#,
            "`inc` is not on the list",
        );
        // An empty statement is none.
        assert_clean(r#"asm!("cpuid;", "", " ; cpuid ", options(preserves_flags));"#);
    }

    #[test]
    fn what_the_gate_cannot_read_fails() {
        assert_caught(
            r#"asm!(concat!("cp", "uid"), options(preserves_flags));"#,
            "an argument the gate cannot read",
        );
        assert_caught(
            r#"asm!(concat!("cp", "uid"), options(preserves_flags));"#,
            "no template the gate can read",
        );
        assert_caught(
            r#"macro_rules! m { ($t:literal) => { asm!($t, options(preserves_flags)) } }"#,
            "an argument the gate cannot read",
        );
        assert_caught(
            r#"asm!("cpuid", #[cfg(feature = "x")] "or eax, 1", options(preserves_flags));"#,
            "an argument the gate cannot read",
        );
        assert_caught(
            r#"asm!("mov %cr0, %rax", options(att_syntax, preserves_flags));"#,
            "`att_syntax`",
        );
        assert_caught(r#"asm!("cpuid", options(raw, preserves_flags));"#, "`raw`");
        assert_caught(
            r#"asm!("cpuid\q", options(preserves_flags));"#,
            "an escape the gate cannot read",
        );
        for (template, what) in [
            ("2:", "not an instruction"),
            ("2: cpuid", "not an instruction"),
            (".byte 0x0f, 0x22, 0xe0", "not an instruction"),
            ("// cpuid", "not an instruction"),
            ("{insn} rax", "not an instruction"),
            ("mov{}, cr4", "not an instruction"),
            ("lock xchg [{}], rax", "`lock` is not on the list"),
            ("rep movsb", "`rep` is not on the list"),
        ] {
            let source = format!(r#"asm!("{template}", insn = sym f, options(preserves_flags));"#);
            assert_caught(&source, what);
        }
        assert_caught(r#"asm!("cpuid", options(preserves_flags)"#, "never closed");
    }

    #[test]
    fn the_option_outside_an_assembly_macro_is_caught() {
        // A renamed macro, or one that passes its options through.
        assert_caught(
            r#"use core::arch::asm as a; fn f() { unsafe { a!("or eax, 1", options(preserves_flags)) } }"#,
            "outside an `asm!` block",
        );
        assert_caught(
            r#"quiet_asm!("or eax, 1"; preserves_flags);"#,
            "outside an `asm!` block",
        );
    }

    #[test]
    fn a_block_without_the_option_and_text_that_is_not_code_are_left_alone() {
        let source = r##"
            // asm!("or eax, 1", options(preserves_flags))
            /* asm!("or eax, 1", options(preserves_flags)) */
            let text = "asm!(\"or eax, 1\", options(preserves_flags))";
            let raw = r#"asm!("or eax, 1", options(preserves_flags))"#;
            unsafe { asm!("or eax, 1", "mov cr4, rax", options(nomem, nostack)) };
            naked_asm!("xor ebp, ebp", "and rsp, -16", "call {main}", "ud2", main = sym main);
            global_asm!(".globl f", "f:", "ret");
            fn asm() {}
            let asm = 1;
        "##;
        assert_clean(source);
        assert_eq!(
            problems_in(source).1,
            Seen {
                blocks: 3,
                declaring: 0
            }
        );
    }

    #[test]
    fn placeholders_and_doubled_braces_are_told_apart() {
        assert_eq!(without_placeholders("mov {t:r}, {0}"), "mov {}, {}");
        assert_eq!(without_placeholders("{{cr0}} {}"), "{cr0} {}");
        assert_eq!(without_placeholders("a }} b {"), "a } b {}");
    }

    #[test]
    fn the_list_is_sorted_lower_case_and_has_no_duplicates() {
        assert!(FLAG_FREE.windows(2).all(|w| w[0] < w[1]), "{FLAG_FREE:?}");
        assert!(
            FLAG_FREE
                .iter()
                .all(|m| m.bytes().all(|b| b.is_ascii_lowercase()))
        );
        // The instructions this gate exists to refuse.
        for writer in ["or", "and", "xrstor", "fxrstor", "popfq", "sti"] {
            assert!(!FLAG_FREE.contains(&writer), "{writer}");
        }
    }

    /// The real tree passes, and the gate's roots find the real assembly.
    #[test]
    fn the_real_tree_passes() {
        let (problems, seen, files) = scan(&repo_root()).expect("the scan");
        assert!(problems.is_empty(), "{problems:#?}");
        assert!(seen.blocks > 50, "only {} blocks read", seen.blocks);
        assert!(seen.declaring > 0, "no block declares the option");
        assert!(files > 100, "only {files} files read");
    }

    /// The defect the review found, put back into the real files: every block of the framework
    /// and of the loader that does not declare the option is made to, and the gate names each
    /// kind of instruction that makes that false.
    #[test]
    fn the_option_put_back_on_the_real_blocks_is_caught() {
        let root = repo_root();
        let mut files = Vec::new();
        collect_files(
            &root.join("framekernel/frame/src"),
            &|_| false,
            &|name| name.ends_with(".rs"),
            &mut files,
        )
        .expect("the framework's files");
        files.push(root.join("boot/uefi/src/main.rs"));
        let mut problems = Vec::new();
        let mut planted_in = 0;
        for file in &files {
            let real = fs::read_to_string(file).expect("a source file");
            let planted = real
                .replace("options(nostack)", "options(nostack, preserves_flags)")
                .replace(
                    "options(nomem, nostack)",
                    "options(nomem, nostack, preserves_flags)",
                );
            planted_in += usize::from(planted != real);
            problems.extend(problems_in(&planted).0.into_iter().map(|p| p.what));
        }
        assert!(
            planted_in >= 2,
            "the option was planted in {planted_in} files"
        );
        for needle in [
            "`or` is not on the list",
            "`and` is not on the list",
            "a move to or from `cr0`",
            "a move to or from `cr3`",
            "a move to or from `cr4`",
            "`xrstor` is not on the list",
            "`fxrstor` is not on the list",
        ] {
            assert!(
                problems.iter().any(|p| p.contains(needle)),
                "{needle:?} not among {problems:#?}"
            );
        }
    }
}
