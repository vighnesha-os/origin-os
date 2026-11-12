//! `cargo xtask check`'s gate on `origin.core` as it was linked (ADR-0005 B2; ADR-0011 Amendment
//! 2, L4 to L7, and Amendment 3, L13; task S4-2).
//!
//! The kernel accepts each call number only from the address of that number's own `syscall`
//! instruction in `origin.core`. That closes row E8 of the system-call threat model only if the
//! image really is what the design says: for each method of `origin.kernel` one stub, and in it
//! one `syscall`, reached only past the load of that method's own number. This gate reads what
//! the linker wrote and holds it to that, whatever the compiler and the generator did.
//!
//! It takes three things, each made by a different tool, and they must agree:
//!
//! - **The definition**, `sdk/interfaces/origin.kernel.oidl`, read here with the `oidl`
//!   library's front end. What each stub must be is worked out here from each method's
//!   parameters ([`expected_stub`]), apart from the generator's own writer of stubs.
//! - **The linked image**, disassembled with the toolchain's own `llvm-objdump`, as the image
//!   gate reads the kernel.
//! - **The kernel's table**: the description `onbf-pack corelib` wrote, which the kernel
//!   compiles in, and the code file it includes.
//!
//! The rules:
//!
//! 1. The image's functions are exactly the definition's methods: `origin_kernel_<method>` for
//!    each, no other, none missing.
//! 2. Each stub is, instruction for instruction: the extension of each of its own parameters
//!    narrower than 64 bits, in register order (L13), with the fourth argument's move from RCX
//!    to R10 at its place; the load of its own number into EAX; `syscall`; and `ret`, or `ud2`
//!    for a method that never returns. Anything after that inside the function is the linker's
//!    padding. So a stub with a second `syscall`, another number, an extension that is missing
//!    or not its own, or any other instruction before its `syscall` is refused.
//! 3. The disassembler decoded every byte: a listing with bytes it could not decode proves
//!    nothing.
//! 4. The offset the kernel's table holds for each call is the offset of that call's `syscall`
//!    instruction in the image, and each export's offset is its stub's.
//! 5. The table's exports are the methods' symbols, each with the version its method arrived
//!    in (L5).
//! 6. In the code file the kernel includes, the two bytes of `syscall` appear once for each
//!    method, at the table's offsets, and nowhere else, at any alignment.
//!
//! What it does not show: that the processor runs these bytes as the disassembler reads them
//! (it is the same decoder the image gate trusts), or anything about a program that reaches
//! the stubs. The kernel's own check of each call is what enforces; this gate is why that check
//! means what it says.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use oidl::model::{Int, Method, Transport, Type};

use crate::{Result, llvm_tool, read_file, repo_root, ring3};

/// The definition the stubs are generated from.
const DEFINITION: &str = "sdk/interfaces/origin.kernel.oidl";
/// Where `version compatibility "..."` is read from.
const VERSIONS: &str = "origin-version.toml";
/// The library whose entry points `origin.core` exports (L5).
const LIBRARY: &str = "origin.kernel";
/// The two bytes of a `syscall` instruction.
const SYSCALL: [u8; 2] = [0x0F, 0x05];

/// What one stub must be, worked out from the definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stub {
    /// The C symbol: `origin_kernel_<method>`.
    pub(crate) symbol: String,
    /// The method's name: the kernel's name for its pin.
    pub(crate) method: String,
    /// Its call number.
    pub(crate) number: u32,
    /// The version of the library it arrived in.
    pub(crate) since: (u16, u16),
    /// Its instructions, as the disassembler spells them.
    pub(crate) instructions: Vec<String>,
}

/// The C calling convention's argument registers, in order, each by its 64-, 32-, 16- and 8-bit
/// name.
const ARGUMENTS: [(&str, &str, &str, &str); 6] = [
    ("rdi", "edi", "di", "dil"),
    ("rsi", "esi", "si", "sil"),
    ("rdx", "edx", "dx", "dl"),
    ("rcx", "ecx", "cx", "cl"),
    ("r8", "r8d", "r8w", "r8b"),
    ("r9", "r9d", "r9w", "r9b"),
];

/// How wide a register argument's value is, and whether it is signed: `None` for one that fills
/// its register (a 64-bit integer, a handle, a pointer or a count).
fn narrow(int: Int) -> Option<(u64, bool)> {
    (int.size() < 8).then(|| (int.size() * 8, int.signed()))
}

/// The width and sign of each register argument of `method`'s C entry point, in the order of
/// `docs/spec/OIDL.md` §10: parameters, then results; a vector or a string as a pointer and a
/// count; a structure, an array and every result as a pointer.
fn registers(method: &Method) -> Vec<Option<(u64, bool)>> {
    let mut out = Vec::new();
    for param in &method.params {
        match &param.ty {
            Type::Bool => out.push(Some((8, false))),
            Type::Int(int) => out.push(narrow(*int)),
            Type::Enum(e) => out.push(narrow(e.repr)),
            Type::Bits(b) => out.push(narrow(b.repr)),
            Type::Handle(_) | Type::Struct(_) | Type::Array(..) => out.push(None),
            Type::Vector(..) | Type::String(_) => out.extend([None, None]),
        }
    }
    for result in &method.returns {
        match &result.ty {
            Type::Vector(..) | Type::String(_) => out.extend([None, None]),
            _ => out.push(None),
        }
    }
    out
}

/// What `method`'s stub must be (rule 2), as `llvm-objdump` prints Intel syntax: a mnemonic,
/// one space, the operands, and numbers in hexadecimal.
pub(crate) fn expected_stub(method: &Method) -> Vec<String> {
    let mut out = Vec::new();
    for (index, (register, value)) in ARGUMENTS.iter().zip(registers(method)).enumerate() {
        let &(whole, double, word, byte) = register;
        // The fourth argument arrives in RCX and leaves in R10 (ADR-0005 P4).
        let fourth = index == 3;
        let (to_whole, to_double) = if fourth {
            ("r10", "r10d")
        } else {
            (whole, double)
        };
        match value {
            None if fourth => out.push(format!("mov {to_whole}, {whole}")),
            None => {}
            Some((32, false)) => out.push(format!("mov {to_double}, {double}")),
            Some((16, false)) => out.push(format!("movzx {to_double}, {word}")),
            Some((8, false)) => out.push(format!("movzx {to_double}, {byte}")),
            Some((32, true)) => out.push(format!("movsxd {to_whole}, {double}")),
            Some((16, true)) => out.push(format!("movsx {to_whole}, {word}")),
            Some((8, true)) => out.push(format!("movsx {to_whole}, {byte}")),
            // No integer type has another width.
            Some((bits, _)) => out.push(format!("<a parameter of {bits} bits>")),
        }
    }
    out.push(format!("mov eax, {:#x}", method.ordinal));
    out.push("syscall".to_string());
    out.push(if method.never { "ud2" } else { "ret" }.to_string());
    out
}

/// Every stub the definition asks for, in number order.
pub(crate) fn expected_stubs(definition: &str, versions: &str) -> Result<Vec<Stub>> {
    let inputs = [oidl::Input {
        file: DEFINITION.to_string(),
        text: definition.as_bytes().to_vec(),
    }];
    let compilation = oidl::compile(&inputs, Some(versions))
        .map_err(|problem| format!("{DEFINITION} does not compile: {problem}"))?;
    let library = compilation
        .libraries
        .iter()
        .find(|l| l.name == LIBRARY && l.transport == Transport::Syscall)
        .ok_or_else(|| format!("{DEFINITION} does not define the syscall library {LIBRARY}"))?;
    let prefix = library.name.replace('.', "_");
    let stubs: Vec<Stub> = library
        .interfaces
        .iter()
        .flat_map(|interface| &interface.methods)
        .map(|method| Stub {
            symbol: format!("{prefix}_{}", method.name),
            method: method.name.clone(),
            number: method.ordinal,
            since: (method.since.major, method.since.minor),
            instructions: expected_stub(method),
        })
        .collect();
    if stubs.is_empty() {
        return Err(format!("{DEFINITION} defines no method of {LIBRARY}"));
    }
    Ok(stubs)
}

/// One function of a listing: its name, and each instruction with its address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Function {
    pub(crate) name: String,
    pub(crate) address: u64,
    pub(crate) instructions: Vec<(u64, String)>,
}

/// Reads a listing made by `llvm-objdump -d -z --no-show-raw-insn --x86-asm-syntax=intel`: every
/// function of every section, in the order listed. A line that is neither a header, a function's
/// label, an instruction nor blank is an error: the gate judges only what it has read whole.
pub(crate) fn read_listing(listing: &str) -> Result<Vec<Function>> {
    let mut functions: Vec<Function> = Vec::new();
    for line in listing.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("Disassembly of section ")
            || trimmed.contains("file format ")
        {
            continue;
        }
        let hex = |text: &str| u64::from_str_radix(text.trim(), 16).ok();
        // `0000000000001000 <name>:`
        if let Some((address, name)) = trimmed
            .strip_suffix(">:")
            .and_then(|rest| rest.split_once(" <"))
            && let Some(address) = hex(address)
        {
            functions.push(Function {
                name: name.to_string(),
                address,
                instructions: Vec::new(),
            });
            continue;
        }
        // `    1005:      	syscall`
        if let Some((address, instruction)) = trimmed.split_once(':')
            && let Some(address) = hex(address)
            && let Some(function) = functions.last_mut()
        {
            let spelled = instruction.split_whitespace().collect::<Vec<_>>().join(" ");
            if !spelled.is_empty() {
                function.instructions.push((address, spelled));
                continue;
            }
        }
        return Err(format!(
            "a line of the listing was not understood: `{line}`"
        ));
    }
    Ok(functions)
}

/// The kernel's table, as `onbf-pack corelib` wrote it for the kernel to compile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Table {
    /// Each export's name, with its offset and its version.
    pub(crate) exports: BTreeMap<String, (u64, (u16, u16))>,
    /// Each method's name, with the offset of its `syscall`.
    pub(crate) pins: BTreeMap<String, u64>,
}

/// Reads the description `onbf-pack corelib` writes: its lines of `CoreExport { name: "…",
/// offset: 0x…, since: (a, b) },` and, between `pins: Pins {` and its closing brace, its lines of
/// `<method>: 0x…,`. A line of either part that does not read is an error.
pub(crate) fn read_table(description: &str) -> Result<Table> {
    let mut table = Table::default();
    let mut in_pins = false;
    let hex = |text: &str| {
        text.trim()
            .strip_prefix("0x")
            .and_then(|digits| u64::from_str_radix(digits, 16).ok())
    };
    for line in description.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("CoreExport {") {
            let fields = rest
                .strip_suffix("},")
                .and_then(|fields| fields.trim().strip_prefix("name: \""))
                .and_then(|fields| fields.split_once("\", offset: "))
                .and_then(|(name, fields)| Some((name, fields.split_once(", since: (")?)))
                .and_then(|(name, (offset, since))| {
                    let (major, minor) = since.trim().strip_suffix(')')?.split_once(", ")?;
                    Some((
                        name,
                        hex(offset)?,
                        (major.parse().ok()?, minor.parse().ok()?),
                    ))
                });
            let Some((name, offset, since)) = fields else {
                return Err(format!(
                    "an export of the description does not read: `{line}`"
                ));
            };
            if table
                .exports
                .insert(name.to_string(), (offset, since))
                .is_some()
            {
                return Err(format!("the description exports `{name}` twice"));
            }
        } else if trimmed == "pins: Pins {" {
            in_pins = true;
        } else if in_pins && trimmed == "}," {
            in_pins = false;
        } else if in_pins {
            let pin = trimmed
                .strip_suffix(',')
                .and_then(|pin| pin.split_once(": "))
                .and_then(|(method, offset)| Some((method, hex(offset)?)));
            let Some((method, offset)) = pin else {
                return Err(format!("a pin of the description does not read: `{line}`"));
            };
            if table.pins.insert(method.to_string(), offset).is_some() {
                return Err(format!("the description pins `{method}` twice"));
            }
        }
    }
    if table.exports.is_empty() || table.pins.is_empty() {
        return Err("the description holds no export or no pin: nothing was read".to_string());
    }
    Ok(table)
}

/// What is wrong with `origin.core`: with its `functions` as linked, the kernel's `table` and
/// the `code` the kernel includes, against the `stubs` the definition asks for. Empty when
/// every rule of the module documentation holds.
pub(crate) fn problems(
    stubs: &[Stub],
    functions: &[Function],
    table: &Table,
    code: &[u8],
) -> Vec<String> {
    let mut problems = Vec::new();
    // Rule 3: every byte was decoded.
    for function in functions {
        for (address, instruction) in &function.instructions {
            if instruction.contains("(bad)") || instruction.contains("<unknown>") {
                problems.push(format!(
                    "{}: the disassembler could not decode the bytes at {address:#x}: the listing proves nothing",
                    function.name
                ));
            }
        }
    }
    // Rule 1: the functions are the methods.
    for function in functions {
        if !stubs.iter().any(|stub| stub.symbol == function.name) {
            problems.push(format!(
                "the image holds `{}`, which is no method of {LIBRARY}: `origin.core` holds the generated stubs and nothing else (ADR-0011 L4)",
                function.name
            ));
        }
    }
    // The image's base: where the code's first byte is. Every stub must give the same one.
    let mut base: Option<u64> = None;
    for stub in stubs {
        let mut found = functions.iter().filter(|f| f.name == stub.symbol);
        let (Some(function), None) = (found.next(), found.next()) else {
            problems.push(format!(
                "the image does not hold exactly one `{}` (ADR-0011 L5)",
                stub.symbol
            ));
            continue;
        };
        // Rule 2: the stub, then nothing but padding.
        let body: Vec<&str> = function
            .instructions
            .iter()
            .map(|(_, instruction)| instruction.as_str())
            .collect();
        let padding_from = body
            .iter()
            .rposition(|instruction| *instruction != "int3")
            .map_or(0, |last| last + 1);
        let (stub_part, _padding) = body.split_at(padding_from);
        if stub_part != stub.instructions {
            problems.push(format!(
                "`{}` is not its stub (ADR-0005 B2; ADR-0011 L6, L13):\n      linked:   {}\n      expected: {}",
                stub.symbol,
                stub_part.join("; "),
                stub.instructions.join("; ")
            ));
            continue;
        }
        // Rules 4 and 5: the kernel's table is this image's.
        let Some(&(export, since)) = table.exports.get(&stub.symbol) else {
            problems.push(format!(
                "the kernel's table does not export `{}` (ADR-0011 L5)",
                stub.symbol
            ));
            continue;
        };
        if since != stub.since {
            problems.push(format!(
                "the kernel's table says `{}` arrived in {}.{}; the definition says {}.{} (ADR-0011 L5)",
                stub.symbol, since.0, since.1, stub.since.0, stub.since.1
            ));
        }
        let Some(stub_base) = function.address.checked_sub(export) else {
            problems.push(format!(
                "the kernel's table puts `{}` at {export:#x}, past where it was linked ({:#x})",
                stub.symbol, function.address
            ));
            continue;
        };
        if *base.get_or_insert(stub_base) != stub_base || stub_base % 4096 != 0 {
            problems.push(format!(
                "the kernel's table puts `{}` at {export:#x}, which is not where it was linked ({:#x}) from the image's base",
                stub.symbol, function.address
            ));
            continue;
        }
        let at = function
            .instructions
            .iter()
            .find(|(_, instruction)| instruction == "syscall")
            .map(|(address, _)| address - stub_base);
        let pin = table.pins.get(&stub.method).copied();
        if pin.is_none() || pin != at {
            problems.push(format!(
                "the kernel accepts call {} (`{}`) from offset {}, and its `syscall` was linked at {} (ADR-0011 L7)",
                stub.number,
                stub.method,
                pin.map_or("none".to_string(), |pin| format!("{pin:#x}")),
                at.map_or("none".to_string(), |at| format!("{at:#x}")),
            ));
        }
    }
    // One `syscall` a method, in the whole listing.
    let listed: usize = functions
        .iter()
        .flat_map(|f| &f.instructions)
        .filter(|(_, instruction)| instruction == "syscall")
        .count();
    if listed != stubs.len() {
        problems.push(format!(
            "the image holds {listed} `syscall` instructions for {} methods: exactly one each (ADR-0005 B2)",
            stubs.len()
        ));
    }
    // Rule 5, the other way: the table names nothing the definition does not.
    for name in table.exports.keys() {
        if !stubs.iter().any(|stub| stub.symbol == *name) {
            problems.push(format!(
                "the kernel's table exports `{name}`, which is no method of {LIBRARY} (ADR-0011 L5)"
            ));
        }
    }
    for method in table.pins.keys() {
        if !stubs.iter().any(|stub| stub.method == *method) {
            problems.push(format!(
                "the kernel's table pins `{method}`, which is no method of {LIBRARY}"
            ));
        }
    }
    // Rule 6: in the bytes the kernel carries, the pair is at the table's offsets and nowhere
    // else, at any alignment.
    let pairs: Vec<u64> = code
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| **pair == SYSCALL)
        .map(|(at, _)| at as u64)
        .collect();
    let mut pinned: Vec<u64> = table.pins.values().copied().collect();
    pinned.sort_unstable();
    if pairs != pinned {
        problems.push(format!(
            "the code the kernel carries holds the two bytes of `syscall` at {pairs:x?}, and the kernel's table pins {pinned:x?}: they must be the same places (ADR-0011 L6, L7)"
        ));
    }
    problems
}

/// Disassembles `elf` as the gate reads it.
fn disassemble(elf: &Path) -> Result<String> {
    let objdump = llvm_tool("llvm-objdump")?;
    let out = Command::new(&objdump)
        .args(["-d", "-z", "--no-show-raw-insn", "--x86-asm-syntax=intel"])
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
    String::from_utf8(out.stdout).map_err(|_| {
        format!(
            "{}'s listing of {} is not UTF-8",
            objdump.display(),
            elf.display()
        )
    })
}

/// The gate, over the files of one build: `origin.core` as linked, and the description and the
/// code `onbf-pack corelib` wrote from it.
pub(crate) fn gate_files(linked: &Path, description: &Path, code: &Path) -> Result {
    let root = repo_root();
    let stubs = expected_stubs(
        &String::from_utf8_lossy(&read_file(&root.join(DEFINITION))?),
        &String::from_utf8_lossy(&read_file(&root.join(VERSIONS))?),
    )?;
    let functions = read_listing(&disassemble(linked)?)?;
    let table = read_table(&String::from_utf8_lossy(&read_file(description)?))?;
    let code = fs::read(code).map_err(|e| format!("{}: {e}", code.display()))?;
    let problems = problems(&stubs, &functions, &table, &code);
    if !problems.is_empty() {
        return Err(format!(
            "`origin.core` as linked ({}) is not the definition's stubs with the kernel's table ({} problems):\n  {}",
            linked.display(),
            problems.len(),
            problems.join("\n  ")
        ));
    }
    let extended: usize = stubs
        .iter()
        .map(|stub| {
            stub.instructions
                .iter()
                .filter(|i| i.starts_with("movzx ") || i.starts_with("movsx") || is_clearing(i))
                .count()
        })
        .sum();
    println!(
        "xtask: origin.core gate: the linked image is {} stubs and nothing else, one for each method of {LIBRARY}; each is the extension of its own narrow parameters ({extended} in all), the load of its own number and one `syscall`, at the offset the kernel's table holds; the code the kernel carries ({} bytes) holds the two bytes of `syscall` {} times, at those offsets and nowhere else",
        stubs.len(),
        code.len(),
        stubs.len()
    );
    Ok(())
}

/// Whether `instruction` clears the upper half of a register by writing its 32-bit name with
/// itself, or, for the fourth argument, with ECX: L13's extension of an unsigned 32-bit value.
fn is_clearing(instruction: &str) -> bool {
    instruction
        .strip_prefix("mov ")
        .and_then(|operands| operands.split_once(", "))
        .is_some_and(|(to, from)| {
            ARGUMENTS
                .iter()
                .any(|&(_, double, _, _)| double == from && (to == from || to == "r10d"))
        })
}

/// `cargo xtask core-gate`, and a step of `check`: builds `origin.core` as every build does and
/// holds what was linked to the rules.
pub(crate) fn gate() -> Result {
    let carried = ring3::programs(None)?;
    gate_files(
        &ring3::core_linked(None),
        &carried.join(format!("{}.rs", ring3::CORE_NAME)),
        &carried.join(format!("{}.0.bin", ring3::CORE_NAME)),
    )
}

#[cfg(test)]
mod tests;
