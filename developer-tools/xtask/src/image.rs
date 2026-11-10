//! `cargo xtask check`'s gate on the kernel's image (ADR-0017 Z10).
//!
//! Two of the kernel's compiler options exist to keep two shapes of code out of its image:
//!
//! - **A return with anything but an INT3 after it.** A processor may run the bytes after a
//!   RET before it knows where the return goes, and AMD asks for an INT3 there on every one of
//!   its processors. `-C target-feature=+harden-sls-ret` makes the compiler write one, and the
//!   framework's hand-written RET and IRETQ carry their own.
//! - **A jump table:** an indirect jump whose target is read from a table at an index.
//!   `-C jump-tables=no` makes a `match` compile to comparisons.
//!
//! The second option is one the stable compiler supports. The first is a code-generator
//! feature it passes on with a warning that it is unknown and unstable, so only the image
//! says whether it still works. This gate disassembles the kernel that `cargo xtask check`
//! has just built, with the toolchain's own `llvm-objdump`, and reads every instruction.
//!
//! **What it cannot hold.** The toolchain ships `core`, `alloc` and `compiler_builtins`
//! already compiled for the kernel's target, without these options, and stable Rust cannot
//! rebuild them (`-Z build-std` is the nightly compiler's; CLAUDE.md §12). Their functions
//! that are not generic arrive in the image as they were built. The gate counts what it
//! finds in them and prints the count on every run. It fails only for code this build
//! compiled. See [`prebuilt`] for how it tells the two apart. The owner accepted that gap by
//! name (ADR-0017 Amendment 1, decision item 152), and it is looked at again at every
//! toolchain upgrade (CLAUDE.md §15): when stable Rust can rebuild them, the exception goes.
//!
//! **One more thing it reads** is the trap entry itself. ADR-0017 Z2 has the entry zero every
//! general-purpose register once a program's values are saved, so that a wrongly guessed path
//! in the kernel has none of them to work with. No boot proof can see that from inside, so the
//! gate reads the entry stub and fails if one of those instructions has gone ([`ENTRY`]).
//!
//! It reads a listing, so it sees what a linear disassembly sees. It is a check that two
//! options and one stub still do their work, not a proof about every byte of the image.

use std::path::Path;
use std::process::Command;

use crate::{Result, build_kernel, llvm_tool};

/// The crates the toolchain ships already compiled.
const PREBUILT_CRATES: &[&str] = &["core", "alloc", "compiler_builtins"];

/// The functions of `compiler_builtins` whose names are not mangled, as found in the kernel
/// today. One that is not listed is held to the rule, so a new one fails the gate until it is
/// added here on purpose.
const PREBUILT_SYMBOLS: &[&str] = &["memcpy", "memmove", "memset", "memcmp", "__udivti3"];

/// The two parts of the processor start-up trampoline that are 16-bit and 32-bit code
/// (`framekernel/frame/src/smp.rs`). Read as 64-bit code they are nonsense, and they never
/// run where the image holds them: each processor runs a copy once, before it has a
/// scheduler. The trampoline's 64-bit part is read like any other function.
const NOT_LONG_MODE: &[&str] = &["origin_ap_trampoline_start", "origin_ap_trampoline_32"];

/// How far back from an indirect jump the load of a table entry is looked for.
const WINDOW: usize = 8;

/// The trap entry: every exception, interrupt and system call saves its registers here and
/// calls the kernel's dispatcher (`framekernel/frame/src/cpu.rs`).
const ENTRY: &str = "origin_frame::cpu::exception_saved";

/// What the entry must run before that call, once the registers are saved (ADR-0017 Z2):
/// each general-purpose register zeroed, and RBP by a conditional move, which no processor
/// guesses, for an entry from ring 3. RDI is not here: it carries the frame to the dispatcher.
const ENTRY_CLEARS: &[&str] = &[
    "xor eax, eax",
    "xor ebx, ebx",
    "xor ecx, ecx",
    "xor edx, edx",
    "xor esi, esi",
    "xor r8d, r8d",
    "xor r9d, r9d",
    "xor r10d, r10d",
    "xor r11d, r11d",
    "xor r12d, r12d",
    "xor r13d, r13d",
    "xor r14d, r14d",
    "xor r15d, r15d",
    "cmovne rbp, rax",
];

/// What the listing holds.
#[derive(Debug, Default, PartialEq, Eq)]
struct Findings {
    /// Returns in code this build compiled.
    returns: usize,
    /// Indirect jumps in code this build compiled.
    indirect_jumps: usize,
    /// What breaks the rule in code this build compiled, one line each.
    problems: Vec<String>,
    /// Returns in the prebuilt libraries.
    prebuilt_returns: usize,
    /// Those of them with something other than an INT3 after them.
    prebuilt_bare: usize,
    /// Jump tables in the prebuilt libraries.
    prebuilt_tables: usize,
    /// Whether the listing holds the trap entry ([`ENTRY`]).
    entry_seen: bool,
}

/// Whether a function came from the toolchain's prebuilt libraries, by its demangled name.
///
/// A name is a set of paths, and each path starts with a crate: `<alloc::string::String as
/// core::fmt::Write>::write_str` starts two, in `alloc` and `core`. A function is prebuilt if
/// every path in its name starts in one of [`PREBUILT_CRATES`]. A name with no path at all is
/// prebuilt if it is a method of a primitive type (`<u128>::_fmt_inner`), which only the core
/// libraries can define, or one of [`PREBUILT_SYMBOLS`].
///
/// Every function defined in an Origin crate names that crate, so none is taken for prebuilt.
/// The reverse can happen: a generic function of `core` that an Origin crate instantiates with
/// `core`'s own types (`<&u8 as core::fmt::Debug>::fmt`) names no Origin crate and is counted
/// with the libraries, though this build compiled it. That hides nothing: if the option
/// stopped working, the functions that do name an Origin crate would fail the gate.
fn prebuilt(name: &str) -> bool {
    let crates = leading_crates(name);
    if crates.is_empty() {
        return name.starts_with('<') || PREBUILT_SYMBOLS.contains(&name);
    }
    crates.iter().all(|c| PREBUILT_CRATES.contains(c))
}

/// The first component of every path in a demangled name: each identifier that is followed
/// by `::` and does not itself follow one.
fn leading_crates(name: &str) -> Vec<&str> {
    let bytes = name.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !word(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && word(bytes[i]) {
            i += 1;
        }
        let leads = start < 2 || &bytes[start - 2..start] != b"::";
        let followed = bytes.len() >= i + 2 && &bytes[i..i + 2] == b"::";
        if leads && followed && !bytes[start].is_ascii_digit() {
            found.push(&name[start..i]);
        }
    }
    found
}

/// An instruction's mnemonic, past any prefix the disassembler prints as a word of its own.
fn mnemonic(instruction: &str) -> &str {
    instruction
        .split(' ')
        .find(|word| {
            !matches!(
                *word,
                "rep" | "repe" | "repz" | "repne" | "repnz" | "lock" | "bnd" | "notrack" | "data16"
            )
        })
        .unwrap_or("")
}

/// Whether an instruction returns: near and far returns, interrupt returns and FRED's two
/// event returns, in the spellings the disassembler uses.
fn is_return(instruction: &str) -> bool {
    matches!(
        mnemonic(instruction),
        "ret"
            | "retq"
            | "retl"
            | "retw"
            | "retf"
            | "retfq"
            | "retfl"
            | "retfw"
            | "lret"
            | "lretq"
            | "lretl"
            | "lretw"
            | "iret"
            | "iretq"
            | "iretd"
            | "iretl"
            | "iretw"
            | "erets"
            | "eretu"
    )
}

/// What an instruction jumps through, if it is an indirect jump: a register's name, or the
/// memory operand's text between its brackets.
enum Through<'a> {
    Register(&'a str),
    Memory(&'a str),
}

fn indirect_jump(instruction: &str) -> Option<Through<'_>> {
    if mnemonic(instruction) != "jmp" {
        return None;
    }
    let operand = instruction.split_once("jmp ")?.1.trim();
    if let Some(open) = operand.find('[') {
        let close = operand.rfind(']')?;
        return Some(Through::Memory(operand.get(open + 1..close)?));
    }
    // A direct jump's operand is an address (`0x224e2 <name+0x52>`); a register's is a word.
    let register = !operand.is_empty()
        && operand.starts_with(|c: char| c.is_ascii_lowercase())
        && operand.chars().all(|c| c.is_ascii_alphanumeric());
    register.then_some(Through::Register(operand))
}

/// The load of a 32-bit table entry: `movsxd X, dword ptr [B + 4*I]`, as `(X, B)`.
fn table_load(instruction: &str) -> Option<(&str, &str)> {
    let rest = instruction.strip_prefix("movsxd ")?;
    let (loaded, memory) = rest.split_once(", dword ptr [")?;
    let memory = memory.strip_suffix(']')?;
    let (base, scaled) = memory.split_once(" + ")?;
    scaled.starts_with("4*").then_some((loaded, base))
}

/// The two registers of `add D, S`.
fn register_add(instruction: &str) -> Option<(&str, &str)> {
    let (to, from) = instruction.strip_prefix("add ")?.split_once(", ")?;
    let register = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric());
    (register(to) && register(from) && from.starts_with(|c: char| c.is_ascii_lowercase()))
        .then_some((to, from))
}

/// Whether a jump through `register` ends the sequence a compiler writes for a jump table in
/// position-independent code: a 32-bit entry loaded at four times an index, an address added
/// to it, and a jump to the sum. `before` is the function's instructions before the jump.
/// Returns the load.
///
/// Any sum that takes the entry in counts, whatever is added to it: a jump the rule calls a
/// table by mistake is looked at by a person, and one it missed is not.
fn ends_table_sequence<'a>(register: &str, before: &[&'a str]) -> Option<&'a str> {
    let window = &before[before.len().saturating_sub(WINDOW)..];
    let mut load = None;
    let mut found = None;
    for &instruction in window {
        if let Some((loaded, _base)) = table_load(instruction) {
            load = Some((loaded, instruction));
            found = None;
        } else if let (Some((loaded, text)), Some((to, from))) = (load, register_add(instruction))
            && to == register
            && (to == loaded || from == loaded)
        {
            found = Some(text);
        }
    }
    found
}

/// One line of the listing.
enum Line<'a> {
    /// `Disassembly of section .text:`
    Section,
    /// `0000000000017000 <name>:`
    Function(&'a str),
    /// `17000: xor ebp, ebp`, as the address and the instruction with single spaces.
    Instruction(&'a str, String),
    Other,
}

fn classify(line: &str) -> Line<'_> {
    let line = line.trim_end();
    if line.starts_with("Disassembly of section") {
        return Line::Section;
    }
    let hex = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit());
    // A function's name holds `::`, so its line is recognised before any split at a colon.
    if let Some((digits, name)) = line.split_once(" <")
        && hex(digits)
        && let Some(name) = name.strip_suffix(">:")
    {
        return Line::Function(name);
    }
    let Some((address, rest)) = line.split_once(':') else {
        return Line::Other;
    };
    let address = address.trim_start();
    if !hex(address) {
        return Line::Other;
    }
    // The disassembler's own remark (`# 0x3df0`) is not part of the instruction.
    let text = rest.split('#').next().unwrap_or("");
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        Line::Other
    } else {
        Line::Instruction(address, text)
    }
}

/// A return that is waiting for the instruction after it.
struct Pending {
    prebuilt: bool,
    place: String,
}

/// Reads a listing made by `llvm-objdump -d -C -z --no-show-raw-insn --x86-asm-syntax=intel`.
fn read(listing: &str) -> Findings {
    // The instructions are kept as owned strings so the window can borrow them.
    let lines: Vec<Line<'_>> = listing.lines().map(classify).collect();
    let mut findings = Findings::default();
    let mut function = "";
    let mut library = false;
    let mut skipped = false;
    let mut before: Vec<&str> = Vec::new();
    let mut pending: Option<Pending> = None;
    // The trap entry: which of its clearing instructions have been seen, until its call.
    let mut cleared = [false; ENTRY_CLEARS.len()];
    let mut in_entry = false;

    let settle = |pending: &mut Option<Pending>, next: Option<&str>, findings: &mut Findings| {
        let Some(waiting) = pending.take() else {
            return;
        };
        if next.is_some_and(|text| mnemonic(text) == "int3") {
            return;
        }
        if waiting.prebuilt {
            findings.prebuilt_bare += 1;
        } else {
            let after = match next {
                Some(text) => format!("is followed by `{text}`"),
                None => "has nothing after it".to_string(),
            };
            findings.problems.push(format!(
                "{}: the return {after}, not by INT3",
                waiting.place
            ));
        }
    };

    for line in &lines {
        match line {
            Line::Section => {
                settle(&mut pending, None, &mut findings);
                function = "";
                library = false;
                skipped = false;
                before.clear();
            }
            Line::Function(name) => {
                function = name;
                library = prebuilt(name);
                skipped = NOT_LONG_MODE.contains(name);
                before.clear();
                in_entry = *name == ENTRY;
                findings.entry_seen |= in_entry;
            }
            Line::Instruction(address, text) => {
                let text: &str = text;
                if skipped {
                    // What follows a return is its own business; what these bytes are is not.
                    settle(&mut pending, None, &mut findings);
                    continue;
                }
                settle(&mut pending, Some(text), &mut findings);
                let place = format!("{address} in {function}");
                if in_entry {
                    if let Some(at) = ENTRY_CLEARS.iter().position(|clear| *clear == text) {
                        cleared[at] = true;
                    } else if mnemonic(text) == "call" {
                        // What is not cleared by now reaches the dispatcher.
                        in_entry = false;
                        for (clear, _) in ENTRY_CLEARS.iter().zip(cleared).filter(|(_, c)| !c) {
                            findings.problems.push(format!(
                                "{place}: the trap entry calls the dispatcher without `{clear}` \
                                 before it: a program's value stays in a register (ADR-0017 Z2)"
                            ));
                        }
                    }
                }
                if text.starts_with('<') {
                    // `<unknown>`: bytes that are not an instruction, where one was expected.
                    if !library {
                        findings.problems.push(format!(
                            "{place}: bytes the disassembler cannot read as an instruction"
                        ));
                    }
                } else if is_return(text) {
                    if library {
                        findings.prebuilt_returns += 1;
                    } else {
                        findings.returns += 1;
                    }
                    pending = Some(Pending {
                        prebuilt: library,
                        place: format!("{place} (`{text}`)"),
                    });
                } else if let Some(through) = indirect_jump(text) {
                    let table = match through {
                        Through::Register(register) => ends_table_sequence(register, &before)
                            .map(|load| format!("after `{load}`")),
                        // A vtable or the global offset table is read at a fixed offset. An
                        // index scaled by the entry size is a table.
                        Through::Memory(operand) => operand
                            .contains('*')
                            .then(|| "at a scaled index".to_string()),
                    };
                    match (table, library) {
                        (Some(_), true) => findings.prebuilt_tables += 1,
                        (Some(how), false) => findings
                            .problems
                            .push(format!("{place}: `{text}` {how} is a jump through a table")),
                        (None, true) => {}
                        (None, false) => findings.indirect_jumps += 1,
                    }
                }
                before.push(text);
            }
            Line::Other => {}
        }
    }
    settle(&mut pending, None, &mut findings);
    findings
}

/// The gate's verdict on what a listing holds: the lines to print, or why it fails.
fn verdict(findings: &Findings) -> Result<Vec<String>> {
    if !findings.problems.is_empty() {
        return Err(format!(
            "the kernel's image breaks ADR-0017 Z10 ({} places; are `-C jump-tables=no` and \
             `-C target-feature=+harden-sls-ret` still in .cargo/config.toml, and does the \
             compiler still honour them?):\n  {}",
            findings.problems.len(),
            findings.problems.join("\n  ")
        ));
    }
    if findings.returns == 0 {
        return Err(
            "image gate: no return was found in the kernel's own code, so nothing was checked \
             (has the disassembler's output changed?)"
                .to_string(),
        );
    }
    if !findings.entry_seen {
        return Err(format!(
            "image gate: the trap entry `{ENTRY}` is not in the image under that name, so its \
             clearing of the registers was not checked (ADR-0017 Z2)"
        ));
    }
    let mut lines = vec![format!(
        "xtask: image gate: {} returns in the code this build compiled, an INT3 after each; \
         no jump table among its {} indirect jumps (ADR-0017 Z10); the trap entry zeroes the \
         {} registers it must before it calls the dispatcher (Z2)",
        findings.returns,
        findings.indirect_jumps,
        ENTRY_CLEARS.len()
    )];
    lines.push(format!(
        "xtask: image gate: NOT covered: the toolchain's prebuilt core, alloc and \
         compiler_builtins hold {} returns, {} without an INT3 after them, and {} jump tables; \
         stable Rust cannot rebuild them (CLAUDE.md §12)",
        findings.prebuilt_returns, findings.prebuilt_bare, findings.prebuilt_tables
    ));
    Ok(lines)
}

/// Disassembles `elf` and holds it to the rule.
pub(crate) fn gate(elf: &Path) -> Result {
    let objdump = llvm_tool("llvm-objdump")?;
    let out = Command::new(&objdump)
        .args([
            "-d",
            "-C",
            "-z",
            "--no-show-raw-insn",
            "--x86-asm-syntax=intel",
        ])
        .arg(elf)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", objdump.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{} failed on {}: {}",
            objdump.display(),
            elf.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let listing = String::from_utf8_lossy(&out.stdout);
    for line in verdict(&read(&listing))? {
        println!("{line}");
    }
    Ok(())
}

/// Builds the kernel and holds its image to the rule: `cargo xtask image-gate`.
pub(crate) fn build_and_gate() -> Result {
    gate(&build_kernel()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A listing in the disassembler's layout, from `(function, instructions)`.
    fn listing(functions: &[(&str, &[&str])]) -> String {
        let mut text =
            String::from("\nkernel:\tfile format elf64-x86-64\n\nDisassembly of section .text:\n");
        let mut address = 0x17000u64;
        for (name, instructions) in functions {
            text.push_str(&format!("\n{address:016x} <{name}>:\n"));
            for instruction in *instructions {
                let (mnemonic, operands) = instruction.split_once(' ').unwrap_or((instruction, ""));
                text.push_str(&format!("   {address:x}:      \t{mnemonic}\t{operands}\n"));
                address += 4;
            }
        }
        text
    }

    const OURS: &str = "<framekernel::handles::HandleTable<u8>>::get";
    const THEIRS: &str = "<core::fmt::Formatter>::pad";

    /// The trap entry as the kernel's image holds it, between the last push and the return.
    fn entry() -> Vec<&'static str> {
        let mut stub = vec!["push r14", "push r15"];
        stub.extend_from_slice(ENTRY_CLEARS);
        stub.extend_from_slice(&[
            "test byte ptr [rsp + 0x90], 0x3",
            "mov rdi, rsp",
            "cld",
            "call 0xb9340 <origin_frame::cpu::dispatch>",
            "mov rsp, rax",
            "pop r15",
            "iretq",
            "int3",
        ]);
        stub
    }

    #[test]
    fn a_hardened_function_passes_and_is_counted() {
        let found = read(&listing(&[
            (
                OURS,
                &["push rbp", "mov rbp, rsp", "pop rbp", "ret", "int3"],
            ),
            (ENTRY, &entry()),
        ]));
        assert_eq!(found.returns, 2);
        assert!(found.problems.is_empty(), "{:?}", found.problems);
        assert!(verdict(&found).is_ok());
    }

    /// The first negative control: the option's work undone at one return.
    #[test]
    fn a_return_followed_by_code_fails() {
        let found = read(&listing(&[(
            OURS,
            &[
                "cmp rdi, rsi",
                "jae 0x17010",
                "ret",
                "xor eax, eax",
                "ret",
                "int3",
            ],
        )]));
        assert_eq!(found.returns, 2);
        assert_eq!(found.problems.len(), 1, "{:?}", found.problems);
        assert!(found.problems[0].contains("is followed by `xor eax, eax`"));
        assert!(found.problems[0].contains(OURS));
        assert!(verdict(&found).is_err());
    }

    #[test]
    fn padding_between_functions_counts_but_the_next_function_does_not() {
        // The linker's padding after a function is INT3, and it is what follows the return.
        let padded = read(&listing(&[
            (OURS, &["ret", "int3", "int3"]),
            ("_start", &["ud2"]),
        ]));
        assert!(padded.problems.is_empty());
        // With no padding, the next function's first instruction follows it.
        let tight = read(&listing(&[
            ("origin_frame::cpu::probe_recover", &["ret"]),
            ("_start", &["xor ebp, ebp"]),
        ]));
        assert_eq!(tight.problems.len(), 1, "{:?}", tight.problems);
    }

    #[test]
    fn a_return_at_the_end_of_a_section_fails() {
        let found = read(&listing(&[("origin_frame::cpu::probe_recover", &["ret"])]));
        assert_eq!(found.problems.len(), 1);
        assert!(found.problems[0].contains("has nothing after it"));
    }

    #[test]
    fn an_interrupt_return_is_held_to_the_same_rule() {
        let bare = read(&listing(&[(
            "origin_frame::cpu::exception_saved",
            &["swapgs", "iretq", "nop"],
        )]));
        assert_eq!(bare.problems.len(), 1, "{:?}", bare.problems);
        let hardened = read(&listing(&[(
            "origin_frame::cpu::exception_saved",
            &["swapgs", "iretq", "int3"],
        )]));
        assert!(hardened.problems.is_empty());
        assert_eq!(hardened.returns, 1);
    }

    #[test]
    fn the_prebuilt_libraries_are_counted_and_do_not_fail() {
        let found = read(&listing(&[
            (ENTRY, &entry()),
            (THEIRS, &["test al, al", "ret", "mov eax, 1", "ret", "int3"]),
            ("memmove", &["ret", "mov rax, rdi"]),
        ]));
        assert!(found.problems.is_empty(), "{:?}", found.problems);
        assert_eq!((found.returns, found.prebuilt_returns), (1, 3));
        assert_eq!(found.prebuilt_bare, 2);
        let lines = verdict(&found).expect("only the libraries fall short");
        assert!(lines[1].contains("NOT covered"));
        assert!(lines[1].contains("3 returns, 2 without"));
    }

    /// The second negative control: the sequence the compiler writes without the option,
    /// copied from a kernel built that way.
    #[test]
    fn a_jump_table_fails() {
        let table: &[&str] = &[
            "movzx eax, al",
            "lea rcx, [rip - 0x1e6db]",
            "movsxd rax, dword ptr [rcx + 4*rax]",
            "add rax, rcx",
            "jmp rax",
            "int3",
        ];
        let found = read(&listing(&[(OURS, table), ("_start", &["ret", "int3"])]));
        assert_eq!(found.problems.len(), 1, "{:?}", found.problems);
        assert!(found.problems[0].contains("is a jump through a table"));
        // The same with the sum left in the table's register, and with the table's address
        // taken from a third register.
        for (add, jump) in [("add rcx, rax", "jmp rcx"), ("add rax, rdx", "jmp rax")] {
            let other_way: &[&str] = &[
                "movsxd rax, dword ptr [rcx + 4*rax]",
                add,
                "mov r14, qword ptr [rbp - 0x70]",
                jump,
            ];
            assert_eq!(
                read(&listing(&[(OURS, other_way)])).problems.len(),
                1,
                "{add}"
            );
        }
        // In a prebuilt library it is counted, and does not fail.
        let theirs = read(&listing(&[(THEIRS, table), (OURS, &["ret", "int3"])]));
        assert!(theirs.problems.is_empty());
        assert_eq!(theirs.prebuilt_tables, 1);
    }

    #[test]
    fn a_table_of_addresses_fails_too() {
        let found = read(&listing(&[(OURS, &["jmp qword ptr [8*rax + 0x3df0]"])]));
        assert_eq!(found.problems.len(), 1, "{:?}", found.problems);
        assert!(found.problems[0].contains("at a scaled index"));
    }

    #[test]
    fn a_tail_call_is_not_a_table() {
        let found = read(&listing(&[
            (
                "<framekernel::storage::Nvme>::write64",
                // A call through a pointer, as the last thing a function does.
                &[
                    "mov rax, qword ptr [rdi + 0x18]",
                    "add rsp, 8",
                    "pop rbp",
                    "jmp rax",
                ],
            ),
            // Through a vtable, and through the global offset table.
            (OURS, &["jmp qword ptr [rcx + 0x18]"]),
            (
                "framekernel::kernel::report",
                &["jmp qword ptr [rip + 0xb9389]"],
            ),
            // A load at four times an index that feeds no jump.
            (
                "framekernel::font::glyph",
                &[
                    "movsxd rax, dword ptr [rcx + 4*rax]",
                    "add rdx, rax",
                    "jmp r11",
                ],
            ),
            (
                "_start",
                &[
                    "jmp 0x58cf0 <framekernel::kernel::kernel_main>",
                    "ret",
                    "int3",
                ],
            ),
        ]));
        assert!(found.problems.is_empty(), "{:?}", found.problems);
        assert_eq!(found.indirect_jumps, 4);
    }

    #[test]
    fn the_window_ends_at_the_function() {
        // A load in one function and a jump in the next are not one sequence.
        let found = read(&listing(&[
            (
                OURS,
                &["movsxd rax, dword ptr [rcx + 4*rax]", "add rax, rcx", "ud2"],
            ),
            ("framekernel::kernel::report", &["jmp rax"]),
            ("_start", &["ret", "int3"]),
        ]));
        assert!(found.problems.is_empty(), "{:?}", found.problems);
    }

    #[test]
    fn the_trampoline_s_real_mode_part_is_not_read_and_its_long_mode_part_is() {
        let found = read(&listing(&[
            // Real-mode bytes read as 64-bit code: nonsense that happens to hold a return.
            (
                "origin_ap_trampoline_start",
                &["cli", "<unknown>", "ret", "lgdt [rsi]"],
            ),
            ("origin_ap_trampoline_32", &["ret", "<unknown>"]),
            ("origin_ap_trampoline_64", &["call rax", "ret", "hlt"]),
            ("_start", &["ret", "int3"]),
        ]));
        assert_eq!(found.problems.len(), 1, "{:?}", found.problems);
        assert!(found.problems[0].contains("origin_ap_trampoline_64"));
    }

    #[test]
    fn bytes_that_are_not_instructions_fail_in_our_code() {
        let found = read(&listing(&[(OURS, &["<unknown>", "ret", "int3"])]));
        assert_eq!(found.problems.len(), 1);
        assert!(found.problems[0].contains("cannot read"));
    }

    /// The third negative control: the entry with one register's clearing taken out, with
    /// the conditional move turned into something else, and with the clearing after the call.
    #[test]
    fn a_trap_entry_that_leaves_a_register_fails() {
        let whole = read(&listing(&[(ENTRY, &entry())]));
        assert!(
            whole.entry_seen && whole.problems.is_empty(),
            "{:?}",
            whole.problems
        );

        let without = |gone: &str| {
            let stub: Vec<&str> = entry().into_iter().filter(|text| *text != gone).collect();
            read(&listing(&[(ENTRY, &stub)])).problems
        };
        for gone in ["xor esi, esi", "xor r11d, r11d", "cmovne rbp, rax"] {
            let problems = without(gone);
            assert_eq!(problems.len(), 1, "{gone}: {problems:?}");
            assert!(problems[0].contains(gone) && problems[0].contains("ADR-0017 Z2"));
        }

        // Cleared too late: the dispatcher has run with the program's value by then.
        let mut late = entry();
        let at = late
            .iter()
            .position(|text| *text == "xor ebx, ebx")
            .unwrap();
        let moved = late.remove(at);
        late.insert(late.len() - 2, moved);
        let problems = read(&listing(&[(ENTRY, &late)])).problems;
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("xor ebx, ebx"));

        // The same instructions in another function are not the entry.
        let elsewhere = read(&listing(&[(OURS, &entry())]));
        assert!(!elsewhere.entry_seen);
        assert!(
            verdict(&elsewhere)
                .unwrap_err()
                .contains("is not in the image")
        );
    }

    #[test]
    fn a_listing_with_no_return_checks_nothing_and_says_so() {
        assert!(verdict(&read("")).is_err());
        assert!(verdict(&read(&listing(&[(THEIRS, &["ret", "int3"])]))).is_err());
        // A changed layout that hides every instruction ends the same way.
        let changed = listing(&[(OURS, &["ret", "int3"])]).replace(":      \t", " | ");
        assert!(verdict(&read(&changed)).is_err());
    }

    #[test]
    fn names_are_sorted_into_ours_and_the_toolchain_s() {
        for name in [
            "<core::fmt::Formatter>::pad_integral",
            "core::str::count::do_count_chars",
            "<alloc::string::String as core::fmt::Write>::write_str",
            "<u128>::_fmt_inner",
            "<[u8]>::fill",
            "<i32 as core::fmt::Display>::fmt",
            "<alloc::raw_vec::RawVecInner<_>>::reserve::do_reserve_and_handle::<alloc::alloc::Global>",
            "compiler_builtins::mem::impls::compare_bytes::{closure#3} (.llvm.2074388861103626067)",
            "memmove",
            "__udivti3",
        ] {
            assert!(prebuilt(name), "{name}");
        }
        for name in [
            "<framekernel::irq::MappedIoApic>::set_redirection",
            "origin_frame::cpu::jump",
            "core::ptr::drop_in_place::<framekernel::process::Process>",
            "<&origin_frame::vm::Error as core::fmt::Debug>::fmt",
            "<alloc::vec::Vec<framekernel::thread::Thread>>::push",
            "<[framekernel::thread::Thread]>::len",
            "<u8 as origin_frame::port::Width>::read",
            "_start",
            "origin_ap_trampoline_64",
            // An unmangled name nobody listed is ours until someone says otherwise.
            "__umodti3",
            "",
        ] {
            assert!(!prebuilt(name), "{name}");
        }
        assert_eq!(
            leading_crates("<alloc::string::String as core::fmt::Write>::write_str"),
            ["alloc", "core"]
        );
    }

    #[test]
    fn instructions_are_recognised_in_the_disassembler_s_spellings() {
        for text in [
            "ret", "retq", "iretq", "retfq", "rep ret", "bnd ret", "erets", "eretu",
        ] {
            assert!(is_return(text), "{text}");
        }
        for text in [
            "int3",
            "sysretq",
            "retaddr",
            "jmp rax",
            "call rax",
            "mov rax, rdi",
        ] {
            assert!(!is_return(text), "{text}");
        }
        assert!(matches!(
            indirect_jump("jmp rax"),
            Some(Through::Register("rax"))
        ));
        assert!(matches!(
            indirect_jump("notrack jmp r11"),
            Some(Through::Register("r11"))
        ));
        assert!(matches!(
            indirect_jump("jmp qword ptr [rcx + 0x18]"),
            Some(Through::Memory("rcx + 0x18"))
        ));
        assert!(
            indirect_jump("jmp 0x224e2 <<framekernel::dma::DeviceDma>::attach+0x47>").is_none()
        );
        assert!(indirect_jump("call rax").is_none());
        assert_eq!(
            table_load("movsxd rax, dword ptr [rcx + 4*rax]"),
            Some(("rax", "rcx"))
        );
        assert_eq!(table_load("movsxd rax, dword ptr [rcx + 8]"), None);
        assert_eq!(register_add("add rax, rcx"), Some(("rax", "rcx")));
        assert_eq!(register_add("add rax, 8"), None);
        assert_eq!(register_add("add rsp, 0x10"), None);
    }
}
