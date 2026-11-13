//! The `origin.core` gate against listings, tables and code made by hand: what a good image
//! looks like, and each way it can be wrong.

use std::fmt::Write as _;
use std::path::PathBuf;

use super::*;

/// The real definition and versions: the gate's expectations come from them.
fn stubs() -> Vec<Stub> {
    let root = repo_root();
    let read = |file: &str| String::from_utf8(read_file(&root.join(file)).unwrap()).unwrap();
    expected_stubs(&read(DEFINITION), &read(VERSIONS)).unwrap()
}

/// One instruction's length in bytes, for laying a listing out: enough to keep addresses
/// apart, and right for the instructions the stubs use.
fn length(instruction: &str) -> u64 {
    match instruction {
        "syscall" | "ud2" => 2,
        "ret" | "int3" => 1,
        i if i.starts_with("mov eax, ") => 5,
        i if i.starts_with("movzx r") || i.starts_with("movsx") => 4,
        i if i.starts_with("movzx ") => 3,
        i if i.starts_with("mov r10") || i.starts_with("mov r8d") || i.starts_with("mov r9d") => 3,
        _ => 2,
    }
}

/// An image made of `stubs`, each padded with `int3` to four bytes, from `0x1000`: its listing
/// as the disassembler prints one, the kernel's table for it, and its code.
struct Image {
    functions: Vec<(String, Vec<String>)>,
    /// Stubs by symbol, for the table: the method's name and its version.
    methods: Vec<(String, String, (u16, u16))>,
}

impl Image {
    fn of(stubs: &[Stub]) -> Self {
        Self {
            functions: stubs
                .iter()
                .map(|s| (s.symbol.clone(), s.instructions.clone()))
                .collect(),
            methods: stubs
                .iter()
                .map(|s| (s.symbol.clone(), s.method.clone(), s.since))
                .collect(),
        }
    }

    /// `(listing, description, code)`.
    fn files(&self) -> (String, String, Vec<u8>) {
        let mut listing = String::from(
            "\nliborigin_core.so:\tfile format elf64-x86-64\n\nDisassembly of section .text:\n",
        );
        let mut exports = String::new();
        let mut pins = String::new();
        let mut code = Vec::new();
        let mut address = 0x1000u64;
        // The table names each stub once: its first function, and that function's first
        // `syscall`, as a description made from a real image would.
        let mut tabled: Vec<&str> = Vec::new();
        for (name, instructions) in &self.functions {
            let start = address;
            let first = !tabled.contains(&name.as_str());
            tabled.push(name);
            let mut pinned = false;
            let _ = write!(listing, "\n{address:016x} <{name}>:\n");
            let mut body: Vec<String> = instructions.clone();
            let size: u64 = body.iter().map(|i| length(i)).sum();
            for _ in 0..(4 - size % 4) % 4 {
                body.push("int3".to_string());
            }
            for instruction in &body {
                let spelled = instruction.replacen(' ', "\t", 1);
                let _ = writeln!(listing, "    {address:x}:      \t{spelled}");
                let at = address - 0x1000;
                match instruction.as_str() {
                    "syscall" => {
                        code.extend_from_slice(&SYSCALL);
                        if let Some((_, method, _)) =
                            self.methods.iter().find(|(symbol, ..)| symbol == name)
                            && first
                            && !pinned
                        {
                            let _ = writeln!(pins, "        {method}: {at:#x},");
                            pinned = true;
                        }
                    }
                    "int3" => code.push(0xCC),
                    other => code.extend(std::iter::repeat_n(0x90, length(other) as usize)),
                }
                address += length(instruction);
            }
            if let Some((_, _, since)) = self.methods.iter().find(|(symbol, ..)| symbol == name)
                && first
            {
                let _ = writeln!(
                    exports,
                    "        CoreExport {{ name: \"{name}\", offset: {:#x}, since: ({}, {}) }},",
                    start - 0x1000,
                    since.0,
                    since.1
                );
            }
        }
        let description = format!(
            "// @generated\npub(crate) static ORIGIN_CORE: CoreImage = CoreImage {{\n    span: 0x1000,\n    exports: &[\n{exports}    ],\n    pins: Pins {{\n{pins}    }},\n    release: [\n        0x00,\n    ],\n}};\n"
        );
        (listing, description, code)
    }

    fn problems(&self, stubs: &[Stub]) -> Vec<String> {
        let (listing, description, code) = self.files();
        problems(
            stubs,
            &read_listing(&listing).unwrap(),
            &read_table(&description).unwrap(),
            &code,
        )
    }

    fn function(&mut self, symbol: &str) -> &mut Vec<String> {
        &mut self
            .functions
            .iter_mut()
            .find(|(name, _)| name == symbol)
            .unwrap()
            .1
    }
}

#[track_caller]
fn refused(image: &Image, stubs: &[Stub], what: &str) {
    let found = image.problems(stubs);
    assert!(
        found.iter().any(|problem| problem.contains(what)),
        "no problem with `{what}` among: {found:#?}"
    );
}

#[test]
fn what_each_stub_must_be_follows_from_its_parameters() {
    let stubs = stubs();
    let of = |method: &str| {
        stubs
            .iter()
            .find(|s| s.method == method)
            .unwrap_or_else(|| panic!("no method {method}"))
            .instructions
            .clone()
    };
    // No parameter narrower than 64 bits, and fewer than four registers: nothing before the load.
    assert_eq!(of("handle_close"), ["mov eax, 0x3", "syscall", "ret"]);
    // Never returns.
    assert_eq!(of("thread_exit"), ["mov eax, 0x0", "syscall", "ud2"]);
    // `rights`, 32 bits of flags, in the second register.
    assert_eq!(
        of("handle_duplicate"),
        ["mov esi, esi", "mov eax, 0x4", "syscall", "ret"]
    );
    // `signals` in the second register, and a result's pointer in the fourth.
    assert_eq!(
        of("wait_one"),
        [
            "mov esi, esi",
            "mov r10, rcx",
            "mov eax, 0x8",
            "syscall",
            "ret"
        ]
    );
    // `clear` and `set`, both 32 bits, in the second and third.
    assert_eq!(
        of("object_signal"),
        [
            "mov esi, esi",
            "mov edx, edx",
            "mov eax, 0xa",
            "syscall",
            "ret"
        ]
    );
    // A narrow value in the fourth register is extended on its way to R10.
    assert_eq!(
        of("memory_protect"),
        ["mov r10d, ecx", "mov eax, 0xe", "syscall", "ret"]
    );
    assert_eq!(
        of("object_wait_async"),
        ["mov r10d, ecx", "mov eax, 0x10", "syscall", "ret"]
    );
    // Six registers, none narrow: only the fourth moves.
    assert_eq!(
        of("channel_read"),
        ["mov r10, rcx", "mov eax, 0x7", "syscall", "ret"]
    );
    // Every stub has its own number, loaded just before its one `syscall`.
    for stub in &stubs {
        let at = stub.instructions.len() - 3;
        assert_eq!(
            stub.instructions[at],
            format!("mov eax, {:#x}", stub.number)
        );
        assert_eq!(stub.instructions[at + 1], "syscall");
        assert_eq!(stub.symbol, format!("origin_kernel_{}", stub.method));
    }
    assert_eq!(stubs.len(), 20);
    assert!(
        stubs
            .iter()
            .any(|s| s.method == "memory_map" && s.since == (0, 5))
    );
}

#[test]
fn every_narrow_width_and_sign_has_its_own_instruction_in_each_register() {
    // The definition has no signed or 8- or 16-bit parameter today, so the rule is held here
    // against a table written out by hand, as the generator's own test holds its writer.
    let table: [[&str; 6]; 6] = [
        [
            "movzx edi, dil",
            "movzx esi, sil",
            "movzx edx, dl",
            "movzx r10d, cl",
            "movzx r8d, r8b",
            "movzx r9d, r9b",
        ],
        [
            "movzx edi, di",
            "movzx esi, si",
            "movzx edx, dx",
            "movzx r10d, cx",
            "movzx r8d, r8w",
            "movzx r9d, r9w",
        ],
        [
            "mov edi, edi",
            "mov esi, esi",
            "mov edx, edx",
            "mov r10d, ecx",
            "mov r8d, r8d",
            "mov r9d, r9d",
        ],
        [
            "movsx rdi, dil",
            "movsx rsi, sil",
            "movsx rdx, dl",
            "movsx r10, cl",
            "movsx r8, r8b",
            "movsx r9, r9b",
        ],
        [
            "movsx rdi, di",
            "movsx rsi, si",
            "movsx rdx, dx",
            "movsx r10, cx",
            "movsx r8, r8w",
            "movsx r9, r9w",
        ],
        [
            "movsxd rdi, edi",
            "movsxd rsi, esi",
            "movsxd rdx, edx",
            "movsxd r10, ecx",
            "movsxd r8, r8d",
            "movsxd r9, r9d",
        ],
    ];
    let definition = |ty: &str| {
        format!(
            "/// Fixture.\nlibrary origin.kernel;\ntransport syscall;\nversion 1.0;\n\
             /// Statuses.\nenum status: i64 {{\n    /// Fine.\n    ok = 0;\n    /// Not fine.\n    invalid_args = -1;\n}}\n\
             /// Calls.\ninterface kernel {{\n    /// Six.\n    method six = 3 (\n        /// A.\n        a: {ty},\n        /// B.\n        b: {ty},\n        /// C.\n        c: {ty},\n        /// D.\n        d: {ty},\n        /// E.\n        e: {ty},\n        /// F.\n        f: {ty},\n    ) errors(invalid_args) requires none;\n}}\n"
        )
    };
    for (ty, row) in ["u8", "u16", "u32", "i8", "i16", "i32"].iter().zip(table) {
        let stubs = expected_stubs(&definition(ty), "").unwrap_or_else(|e| panic!("{ty}: {e}"));
        let mut wanted: Vec<&str> = row.to_vec();
        wanted.extend(["mov eax, 0x3", "syscall", "ret"]);
        assert_eq!(stubs[0].instructions, wanted, "six parameters of {ty}");
    }
    // A boolean is a byte, and a value that fills its register needs nothing but the move.
    let stubs = expected_stubs(&definition("bool"), "").unwrap();
    assert_eq!(stubs[0].instructions[..6], table[0]);
    let stubs = expected_stubs(&definition("u64"), "").unwrap();
    assert_eq!(
        stubs[0].instructions,
        ["mov r10, rcx", "mov eax, 0x3", "syscall", "ret"]
    );
}

#[test]
fn an_image_of_the_definitions_stubs_passes() {
    let stubs = stubs();
    assert_eq!(Image::of(&stubs).problems(&stubs), Vec::<String>::new());
    // In any order of functions: the kernel's table says where each is.
    let mut reversed = Image::of(&stubs);
    reversed.functions.reverse();
    assert_eq!(reversed.problems(&stubs), Vec::<String>::new());
}

#[test]
fn a_second_syscall_for_one_number_is_refused() {
    let stubs = stubs();
    let mut image = Image::of(&stubs);
    image
        .function("origin_kernel_handle_close")
        .insert(2, "syscall".to_string());
    refused(
        &image,
        &stubs,
        "`origin_kernel_handle_close` is not its stub",
    );
    refused(&image, &stubs, "21 `syscall` instructions for 20 methods");
}

#[test]
fn a_stub_whose_number_is_not_its_own_is_refused() {
    let stubs = stubs();
    let mut image = Image::of(&stubs);
    // `handle_close` loading `channel_write`'s number: the E8 attack, linked in.
    image.function("origin_kernel_handle_close")[0] = "mov eax, 0x6".to_string();
    refused(
        &image,
        &stubs,
        "`origin_kernel_handle_close` is not its stub",
    );
    // The number moved into RAX from a register a caller sets.
    let mut image = Image::of(&stubs);
    image.function("origin_kernel_handle_close")[0] = "mov eax, esi".to_string();
    refused(
        &image,
        &stubs,
        "`origin_kernel_handle_close` is not its stub",
    );
}

#[test]
fn anything_before_the_syscall_but_the_stubs_own_extensions_is_refused() {
    let stubs = stubs();
    // An instruction that is no extension and no move of the fourth argument.
    let mut image = Image::of(&stubs);
    image
        .function("origin_kernel_handle_close")
        .insert(0, "add rdi, 0x8".to_string());
    refused(
        &image,
        &stubs,
        "`origin_kernel_handle_close` is not its stub",
    );
    // An extension of a register the method does not take narrow.
    let mut image = Image::of(&stubs);
    image
        .function("origin_kernel_handle_close")
        .insert(0, "mov edi, edi".to_string());
    refused(
        &image,
        &stubs,
        "`origin_kernel_handle_close` is not its stub",
    );
    // An extension that is missing (L13): `rights` passed on as the caller left its register.
    let mut image = Image::of(&stubs);
    image.function("origin_kernel_handle_duplicate").remove(0);
    refused(
        &image,
        &stubs,
        "`origin_kernel_handle_duplicate` is not its stub",
    );
    // The wrong one: the sign where the value is unsigned.
    let mut image = Image::of(&stubs);
    image.function("origin_kernel_handle_duplicate")[0] = "movsxd rsi, esi".to_string();
    refused(
        &image,
        &stubs,
        "`origin_kernel_handle_duplicate` is not its stub",
    );
    // The fourth argument left in RCX, which `syscall` overwrites.
    let mut image = Image::of(&stubs);
    image.function("origin_kernel_channel_read").remove(0);
    refused(
        &image,
        &stubs,
        "`origin_kernel_channel_read` is not its stub",
    );
    // Something after the return that is not padding.
    let mut image = Image::of(&stubs);
    image
        .function("origin_kernel_handle_close")
        .push("jmp rax".to_string());
    refused(
        &image,
        &stubs,
        "`origin_kernel_handle_close` is not its stub",
    );
}

#[test]
fn a_syscall_at_an_offset_the_kernels_table_does_not_hold_is_refused() {
    let stubs = stubs();
    let image = Image::of(&stubs);
    let (listing, description, code) = image.files();
    let functions = read_listing(&listing).unwrap();
    let good = read_table(&description).unwrap();
    assert!(problems(&stubs, &functions, &good, &code).is_empty());
    // One pin off by a byte, and one that is another call's.
    let mut table = good.clone();
    *table.pins.get_mut("channel_write").unwrap() += 1;
    let found = problems(&stubs, &functions, &table, &code);
    assert!(
        found
            .iter()
            .any(|p| p.contains("the kernel accepts call 6 (`channel_write`)")),
        "{found:#?}"
    );
    let mut table = good.clone();
    let other = table.pins["channel_read"];
    *table.pins.get_mut("channel_write").unwrap() = other;
    let found = problems(&stubs, &functions, &table, &code);
    assert!(
        found.iter().any(|p| p.contains("(`channel_write`)")),
        "{found:#?}"
    );
    assert!(
        found.iter().any(|p| p.contains("must be the same places")),
        "{found:#?}"
    );
    // A pin left out, and an export put elsewhere.
    let mut table = good.clone();
    table.pins.remove("clock_read");
    let found = problems(&stubs, &functions, &table, &code);
    assert!(
        found.iter().any(|p| p.contains("from offset none")),
        "{found:#?}"
    );
    let mut table = good.clone();
    table.exports.get_mut("origin_kernel_clock_read").unwrap().0 += 4;
    let found = problems(&stubs, &functions, &table, &code);
    assert!(
        found
            .iter()
            .any(|p| p.contains("which is not where it was linked")),
        "{found:#?}"
    );
    // A version that is not the method's.
    let mut table = good.clone();
    table.exports.get_mut("origin_kernel_memory_map").unwrap().1 = (0, 0);
    let found = problems(&stubs, &functions, &table, &code);
    assert!(
        found.iter().any(|p| p.contains("arrived in 0.0")),
        "{found:#?}"
    );
    // A pin and an export of no method.
    let mut table = good;
    table.pins.insert("helper".to_string(), 0);
    table
        .exports
        .insert("origin_kernel_helper".to_string(), (0, (0, 0)));
    let found = problems(&stubs, &functions, &table, &code);
    assert!(
        found.iter().any(|p| p.contains("pins `helper`")),
        "{found:#?}"
    );
    assert!(
        found
            .iter()
            .any(|p| p.contains("exports `origin_kernel_helper`")),
        "{found:#?}"
    );
}

#[test]
fn a_function_that_is_no_stub_and_a_stub_that_is_missing_are_refused() {
    let stubs = stubs();
    let mut more = Image::of(&stubs);
    more.functions
        .push(("origin_core_helper".to_string(), vec!["ret".to_string()]));
    refused(&more, &stubs, "the image holds `origin_core_helper`");
    let mut fewer = Image::of(&stubs);
    fewer
        .functions
        .retain(|(name, _)| name != "origin_kernel_port_wait");
    refused(
        &fewer,
        &stubs,
        "does not hold exactly one `origin_kernel_port_wait`",
    );
    let mut twice = Image::of(&stubs);
    let again = twice.functions[3].clone();
    twice.functions.push(again);
    refused(&twice, &stubs, "does not hold exactly one");
}

#[test]
fn the_two_bytes_of_syscall_anywhere_else_in_the_carried_code_are_refused() {
    let stubs = stubs();
    let (listing, description, mut code) = Image::of(&stubs).files();
    let functions = read_listing(&listing).unwrap();
    let table = read_table(&description).unwrap();
    // Inside another instruction's bytes, where the disassembler shows no `syscall`.
    code[1..3].copy_from_slice(&SYSCALL);
    let found = problems(&stubs, &functions, &table, &code);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(found[0].contains("must be the same places"), "{found:#?}");
    // And one that is missing from the bytes.
    let (_, _, mut code) = Image::of(&stubs).files();
    let at = table.pins["wait_one"] as usize;
    code[at] = 0x90;
    let found = problems(&stubs, &functions, &table, &code);
    assert!(found[0].contains("must be the same places"), "{found:#?}");
}

#[test]
fn bytes_the_disassembler_could_not_decode_are_refused() {
    let stubs = stubs();
    let mut image = Image::of(&stubs);
    image
        .function("origin_kernel_handle_close")
        .push("(bad)".to_string());
    refused(&image, &stubs, "could not decode");
}

#[test]
fn a_listing_or_a_table_that_does_not_read_is_an_error() {
    assert!(read_listing("0000000000001000 <f>:\n    1000:\tret\n  what is this\n").is_err());
    // An instruction before any function.
    assert!(read_listing("    1000:\tret\n").is_err());
    let functions = read_listing(
        "\nx.so:\tfile format elf64-x86-64\n\nDisassembly of section .text:\n\n0000000000001000 <f>:\n    1000:      \tmov\teax, 0x5\n    1005:      \tsyscall\n",
    )
    .unwrap();
    assert_eq!(
        functions,
        [Function {
            name: "f".to_string(),
            address: 0x1000,
            instructions: vec![
                (0x1000, "mov eax, 0x5".to_string()),
                (0x1005, "syscall".to_string())
            ],
        }]
    );
    assert!(read_table("nothing here").is_err());
    assert!(read_table("    pins: Pins {\n        a: 5,\n    },\n").is_err());
    assert!(
        read_table("        CoreExport { name: \"a\", offset: 0x0 },\n    pins: Pins {\n        a: 0x5,\n    },\n")
            .is_err()
    );
    let table = read_table(
        "        CoreExport { name: \"origin_kernel_a\", offset: 0x10, since: (0, 5) },\n    pins: Pins {\n        a: 0x15,\n    },\n    release: [\n        0x00, 0x11,\n    ],\n",
    )
    .unwrap();
    assert_eq!(table.exports["origin_kernel_a"], (0x10, (0, 5)));
    assert_eq!(table.pins["a"], 0x15);
}

// ------------------------------------------------------------------ the carried programs

/// A program's code, as the disassembler lists it: `(function, instructions)`.
fn program(functions: &[(&str, &[&str])]) -> Vec<Function> {
    let mut address = 0x1000;
    functions
        .iter()
        .map(|(name, instructions)| {
            let start = address;
            let instructions = instructions
                .iter()
                .map(|instruction| {
                    address += 4;
                    (address - 4, (*instruction).to_string())
                })
                .collect();
            Function {
                name: (*name).to_string(),
                address: start,
                instructions,
            }
        })
        .collect()
}

#[test]
fn a_program_with_no_syscall_passes_and_one_with_any_is_refused() {
    let clean = program(&[
        ("_start", &["xor ebp, ebp", "call 0x1010 <run>", "ud2"]),
        ("run", &["call qword ptr [rip + 0x2f00]", "ret"]),
    ]);
    assert_eq!(
        program_problems("first-program", &clean, None),
        Vec::<String>::new()
    );
    // A stub compiled into it: today's `asm!` form, inlined, or the stubs' file linked in.
    let inlined = program(&[
        ("_start", &["xor ebp, ebp", "call 0x1010 <run>", "ud2"]),
        ("run", &["mov eax, 0x3", "syscall", "ret"]),
    ]);
    let found = program_problems("first-program", &inlined, None);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(
        found[0].contains("holds 1 `syscall` instructions") && found[0].contains("in `run`"),
        "{found:#?}"
    );
}

#[test]
fn a_proof_program_is_excepted_for_exactly_its_own_syscall() {
    let allowed = Some((1, "own_syscall"));
    let as_meant = program(&[
        (
            "_ZN11pin_program3raw11own_syscall17h0123456789abcdefE",
            &["mov rax, rdi", "mov rdi, rsi", "syscall", "ret"],
        ),
        ("run", &["call qword ptr [rip + 0x2f00]", "ret"]),
    ]);
    assert_eq!(
        program_problems("pin-program", &as_meant, allowed),
        Vec::<String>::new()
    );
    // One more, anywhere: a stub inlined into the proof program is still caught.
    let more = program(&[
        (
            "_ZN11pin_program3raw11own_syscall17h0123456789abcdefE",
            &["mov rax, rdi", "syscall", "ret"],
        ),
        ("run", &["mov eax, 0x3", "syscall", "ret"]),
    ]);
    let found = program_problems("pin-program", &more, allowed);
    assert!(
        found[0].contains("holds 2 `syscall` instructions"),
        "{found:#?}"
    );
    // The right number in the wrong function.
    let elsewhere = program(&[("run", &["mov eax, 0x3", "syscall", "ret"])]);
    assert_eq!(
        program_problems("pin-program", &elsewhere, allowed).len(),
        1
    );
    // None at all: the proof's forbidden call has gone, and the list must change with it.
    let none = program(&[("run", &["ret"])]);
    assert_eq!(program_problems("pin-program", &none, allowed).len(), 1);
    // The same code in a program that is not excepted is refused.
    assert_eq!(program_problems("first-program", &as_meant, None).len(), 1);
}

#[test]
fn bytes_of_a_program_that_did_not_decode_fail_the_gate() {
    let lost = program(&[("run", &["mov eax, 0x3", "(bad)", "ret"])]);
    let found = program_problems("first-program", &lost, None);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(found[0].contains("could not decode"), "{found:#?}");
}

#[test]
fn the_code_a_description_names_is_its_read_execute_segments_files() {
    let description = "pub(crate) static P: Carried = Carried {\n    segments: &[\n        Segment {\n            rva: 0x0,\n            size: 0x1000,\n            access: SegmentAccess::Read,\n            bytes: include_bytes!(concat!(env!(\"ORIGIN_CARRIED_PROGRAMS\"), \"/p.0.bin\")),\n        },\n        Segment {\n            rva: 0x1000,\n            size: 0x3000,\n            access: SegmentAccess::ReadExecute,\n            bytes: include_bytes!(concat!(env!(\"ORIGIN_CARRIED_PROGRAMS\"), \"/p.1.bin\")),\n        },\n        Segment {\n            rva: 0x4000,\n            size: 0x1000,\n            access: SegmentAccess::ReadWrite,\n            bytes: &[],\n        },\n    ],\n};\n";
    assert_eq!(code_files(description), [(0x1000, "p.1.bin".to_string())]);
    assert!(code_files("nothing").is_empty());
}

/// One function at 0x1000 whose instructions start at the given offsets.
fn listed_at(instructions: &[(u64, &str)]) -> Vec<Function> {
    vec![Function {
        name: "run".to_string(),
        address: 0x1000,
        instructions: instructions
            .iter()
            .map(|(offset, text)| (0x1000 + offset, (*text).to_string()))
            .collect(),
    }]
}

#[test]
fn each_pair_of_syscalls_bytes_is_placed_against_the_listing() {
    // mov eax, 3 (5 bytes); syscall (2); ret (1).
    let code = vec![(0x1000, vec![0xB8, 3, 0, 0, 0, 0x0F, 0x05, 0xC3])];
    let listing = listed_at(&[(0, "mov eax, 0x3"), (5, "syscall"), (7, "ret")]);
    assert_eq!(pair_problems("p", &listing, &code), (Vec::new(), 0));

    // The pair as the immediate of a `mov`: inside a listed instruction, counted, not refused.
    let code = vec![(0x1000, vec![0xB8, 0x0F, 0x05, 0, 0, 0xC3])];
    let listing = listed_at(&[(0, "mov eax, 0x50f"), (5, "ret")]);
    assert_eq!(pair_problems("p", &listing, &code), (Vec::new(), 1));

    // The pair across two instructions: the last byte of one and the first of the next.
    let code = vec![(0x1000, vec![0xB0, 0x0F, 0x05, 1, 0, 0, 0, 0xC3])];
    let listing = listed_at(&[(0, "mov al, 0xf"), (2, "add eax, 0x1"), (7, "ret")]);
    assert_eq!(pair_problems("p", &listing, &code), (Vec::new(), 1));
}

#[test]
fn a_pair_the_listing_does_not_cover_fails_the_gate() {
    // After the last listed instruction: nothing shows what those bytes are.
    let code = vec![(0x1000, vec![0xC3, 0xCC, 0xCC, 0x0F, 0x05])];
    let listing = listed_at(&[(0, "ret")]);
    let (problems, inside) = pair_problems("p", &listing, &code);
    assert_eq!(inside, 0);
    assert!(
        problems.len() == 1 && problems[0].contains("listed no instruction over"),
        "{problems:#?}"
    );
    // Before the first.
    let code = vec![(0x1000, vec![0x0F, 0x05, 0xC3])];
    let listing = listed_at(&[(2, "ret")]);
    assert_eq!(pair_problems("p", &listing, &code).0.len(), 1);
    // Between two listed instructions too far apart for one instruction to reach.
    let mut bytes = vec![0xC3];
    bytes.extend([0xCC; 20]);
    bytes.extend([0x0F, 0x05, 0xC3]);
    let listing = listed_at(&[(0, "ret"), (23, "ret")]);
    assert_eq!(pair_problems("p", &listing, &[(0x1000, bytes)]).0.len(), 1);
    // In a second segment the listing says nothing of.
    let code = vec![(0x1000, vec![0xC3]), (0x9000, vec![0x90, 0x0F, 0x05, 0xC3])];
    let listing = listed_at(&[(0, "ret")]);
    assert_eq!(pair_problems("p", &listing, &code).0.len(), 1);
}

#[test]
fn a_listing_and_carried_code_that_are_not_one_program_fail_the_gate() {
    // The listing has a `syscall` where the carried bytes have none.
    let code = vec![(0x1000, vec![0xB8, 3, 0, 0, 0, 0x90, 0x90, 0xC3])];
    let listing = listed_at(&[(0, "mov eax, 0x3"), (5, "syscall"), (7, "ret")]);
    let (problems, _) = pair_problems("p", &listing, &code);
    assert!(
        problems.len() == 1 && problems[0].contains("no such bytes there"),
        "{problems:#?}"
    );
    // The carried bytes have the pair where the listing has another instruction.
    let code = vec![(0x1000, vec![0x0F, 0x05, 0xC3])];
    let listing = listed_at(&[(0, "nop"), (2, "ret")]);
    let (problems, _) = pair_problems("p", &listing, &code);
    assert!(
        problems.len() == 1 && problems[0].contains("has another instruction"),
        "{problems:#?}"
    );
}

#[test]
fn every_excepted_proof_program_is_one_the_kernel_carries() {
    let carried: Vec<&str> = ring3::program_images(None)
        .iter()
        .map(|(package, ..)| *package)
        .collect();
    for (package, count, holder) in PROOF_PROGRAMS {
        assert!(
            carried.contains(package),
            "{package} is excepted and not carried"
        );
        assert!(*count > 0 && !holder.is_empty());
    }
    // And the first program is not excepted: it must hold none.
    assert!(carried.contains(&"first-program"));
    assert!(
        !PROOF_PROGRAMS
            .iter()
            .any(|(package, ..)| *package == "first-program")
    );
}

// ------------------------------------------------------------------ a really linked image

/// Where one really linked `origin.core` and its description are.
struct Linked {
    dir: PathBuf,
    image: PathBuf,
    description: PathBuf,
    code: PathBuf,
}

impl Drop for Linked {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// The `-C` flags of `.cargo/userspace.toml`, in order: every quoted string after a `"-C",`.
fn userspace_flags() -> Vec<String> {
    let text =
        String::from_utf8(read_file(&repo_root().join(".cargo/userspace.toml")).unwrap()).unwrap();
    let flags: Vec<String> = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("\"-C\", \""))
        .map(|rest| rest.trim_end_matches(['"', ',']).to_string())
        .collect();
    assert!(flags.iter().any(|f| f == "panic=abort"), "{flags:?}");
    flags
}

/// Generates the stubs of the real definition, lets `plant` change the file of stubs, compiles
/// and links `runtime/core` over them with the pinned compiler, its linker and the userspace
/// flags, and describes the image as the build does (`onbf-pack corelib`). So the files the
/// gate then reads are a compiler's and a linker's, not a listing laid out by hand.
///
/// `None` if the description refuses the image: then the build stops before the gate.
fn really_linked(name: &str, plant: impl FnOnce(&str) -> String) -> Option<Linked> {
    let root = repo_root();
    let read = |path: &str| String::from_utf8(read_file(&root.join(path)).unwrap()).unwrap();
    let inputs = [oidl::Input {
        file: DEFINITION.to_string(),
        text: read(DEFINITION).into_bytes(),
    }];
    let compilation = oidl::compile(&inputs, Some(&read(VERSIONS))).expect("it compiles");
    let dir = crate::target_dir()
        .join("xtask")
        .join("corestubs-tests")
        .join(format!("{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let stubs_file = "core/origin_kernel/kernel.rs";
    let mut exports = None;
    let mut plant = Some(plant);
    for file in oidl::codegen::generate(&compilation) {
        let Some(relative) = file.path.strip_prefix("core/") else {
            continue;
        };
        let text = match plant.take_if(|_| file.path == stubs_file) {
            Some(plant) => plant(&file.text),
            None => file.text,
        };
        if relative.ends_with(".exports") {
            exports = Some(text.clone());
        }
        let to = dir.join("stubs").join(relative);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::write(&to, text).unwrap();
    }
    assert!(plant.is_none(), "the generator wrote no {stubs_file}");
    let image = dir.join("liborigin_core.so");
    let toolchain = crate::toolchain::toolchain().expect("the pinned toolchain");
    let mut rustc = Command::new(toolchain.sysroot.join("bin").join("rustc"));
    rustc
        .current_dir(&dir)
        .env("ORIGIN_CORE_STUBS", dir.join("stubs"))
        .args(["--edition", "2024", "--crate-type", "cdylib"])
        .args([
            "--crate-name",
            "origin_core",
            "--target",
            "x86_64-unknown-linux-gnu",
        ])
        .args(["-C", "opt-level=3"]);
    for flag in userspace_flags() {
        rustc.arg("-C").arg(flag);
    }
    let out = rustc
        .args(["-C", "link-arg=--soname=origin.core", "-o"])
        .arg(&image)
        .arg(root.join("runtime/core/src/lib.rs"))
        .output()
        .expect("rustc runs");
    assert!(
        out.status.success(),
        "rustc failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listing = onbf_pack::corelib::Listing::parse(&exports.expect("a list of exports"))
        .expect("the list reads");
    let described = onbf_pack::corelib::describe(&fs::read(&image).unwrap(), &listing).ok()?;
    let files = described
        .rust("ORIGIN_CARRIED_PROGRAMS")
        .expect("a description");
    let carried = dir.join("carried");
    fs::create_dir_all(&carried).unwrap();
    for (file, bytes) in &files {
        fs::write(carried.join(file), bytes).unwrap();
    }
    Some(Linked {
        image,
        description: carried.join(format!("{}.rs", ring3::CORE_NAME)),
        code: carried.join(format!("{}.0.bin", ring3::CORE_NAME)),
        dir,
    })
}

/// The gate's verdict on a really linked image.
fn gate_on(linked: &Linked) -> Result {
    gate_files(&linked.image, &linked.description, &linked.code)
}

/// Replaces `old`, which must be there, with `new`, the first time it appears.
#[track_caller]
fn planted(stubs: &str, old: &str, new: &str) -> String {
    assert!(stubs.contains(old), "the generated stubs have no `{old}`");
    stubs.replacen(old, new, 1)
}

/// The gate, on files a compiler and a linker made (the review of 2026-11-13, finding F5): it
/// passes the image as generated and refuses each planted fault that the description lets
/// through. Until this test the gate had been seen to refuse only listings laid out by hand in
/// the form its author expected of the disassembler; a new version of that tool is held to the
/// same refusals here.
#[test]
fn the_gate_refuses_a_really_linked_image_with_a_planted_fault() {
    // As generated: the gate passes it.
    let good = really_linked("good", str::to_string).expect("the description takes it");
    assert_eq!(gate_on(&good), Ok(()));

    // `handle_close` (call 3) loads `channel_create`'s number.
    let number = really_linked("number", |stubs| {
        planted(stubs, "\"mov eax, 3\",", "\"mov eax, 5\",")
    })
    .expect("one `syscall` a stub still: the description takes it");
    let refused = gate_on(&number).expect_err("another call's number");
    assert!(
        refused.contains("`origin_kernel_handle_close` is not its stub"),
        "{refused}"
    );

    // `handle_duplicate` without the extension of its `rights`.
    let extension = really_linked("extension", |stubs| planted(stubs, "\"mov esi, esi\",", ""))
        .expect("the description does not read instructions");
    let refused = gate_on(&extension).expect_err("a missing extension");
    assert!(
        refused.contains("`origin_kernel_handle_duplicate` is not its stub"),
        "{refused}"
    );

    // A byte that is no instruction in 64-bit mode, inside `handle_close`.
    let byte = really_linked("byte", |stubs| {
        planted(
            stubs,
            "\"mov eax, 3\",",
            "\".byte 0x06\",\n        \"mov eax, 3\",",
        )
    })
    .expect("no second `syscall`: the description takes it");
    let refused = gate_on(&byte).expect_err("a byte the disassembler cannot decode");
    // The pinned disassembler prints `<unknown>` for it: both rules speak.
    assert!(
        refused.contains("origin_kernel_handle_close: the disassembler could not decode the bytes")
            && refused.contains("linked:   <unknown>; mov eax, 0x3; syscall; ret"),
        "{refused}"
    );

    // A second `syscall` in one stub never reaches the gate: the description refuses it.
    assert!(
        really_linked("second", |stubs| {
            planted(
                stubs,
                "\"mov eax, 3\",",
                "\"syscall\",\n        \"mov eax, 3\",",
            )
        })
        .is_none()
    );
}

// ------------------------------------------------------------------ enumerations (item 277)

/// The gate's own rule for an enumeration, on the real definition with two enumerations and
/// two calls added to it: one over a signed narrow integer with a member below zero, one over
/// an unsigned one. Each is extended as its underlying integer (ADR-0011 Amendment 3, L13, as
/// decision item 277 words it): the sign for the first, zero for the second. `origin.kernel`
/// passes no enumeration by register, so without this the rule would be stated and not shown.
#[test]
fn the_gate_expects_an_enumeration_extended_as_its_underlying_integer() {
    let root = repo_root();
    let read = |path: &str| String::from_utf8(read_file(&root.join(path)).unwrap()).unwrap();
    let definition = read(DEFINITION);
    // The two enumerations go before the interface's own doc comment, and the two calls
    // before its closing brace.
    let lines: Vec<&str> = definition.lines().collect();
    let interface = lines
        .iter()
        .position(|line| line.starts_with("interface kernel {"))
        .expect("the interface");
    let doc = (0..interface)
        .rev()
        .find(|&i| !lines[i].starts_with("///") && !lines[i].starts_with('@'))
        .map_or(0, |i| i + 1);
    let close = lines
        .iter()
        .rposition(|line| *line == "}")
        .expect("its end");
    let mut text = lines[..doc].join("\n");
    text.push_str(
        "\n/// A signed choice.\nenum test_sign: i8 {\n    /// Below zero.\n    minus = -1;\n    /// None.\n    none = 0;\n}\n\n\
         /// An unsigned choice.\nenum test_plain: u16 {\n    /// One.\n    one = 1;\n}\n\n",
    );
    text.push_str(&lines[doc..close].join("\n"));
    text.push_str(
        "\n\n    /// A signed enumeration in the first and the fourth register.\n\
         \x20   method test_signed = 900 (\n        /// One.\n        a: test_sign,\n        /// Two.\n        b: u64,\n\
         \x20       /// Three.\n        c: u64,\n        /// Four.\n        d: test_sign,\n    ) errors(invalid_args) requires none;\n\n\
         \x20   /// An unsigned enumeration in the second and the third register.\n\
         \x20   method test_unsigned = 901 (\n        /// One.\n        a: u64,\n        /// Two.\n        b: test_plain,\n\
         \x20       /// Three.\n        c: test_plain,\n    ) errors(invalid_args) requires none;\n}\n",
    );
    let stubs = expected_stubs(&text, &read(VERSIONS)).expect("the definition still compiles");
    let of = |method: &str| {
        stubs
            .iter()
            .find(|s| s.method == method)
            .unwrap_or_else(|| panic!("no method {method}"))
            .instructions
            .clone()
    };
    assert_eq!(
        of("test_signed"),
        [
            "movsx rdi, dil",
            "movsx r10, cl",
            "mov eax, 0x384",
            "syscall",
            "ret"
        ]
    );
    assert_eq!(
        of("test_unsigned"),
        [
            "movzx esi, si",
            "movzx edx, dx",
            "mov eax, 0x385",
            "syscall",
            "ret"
        ]
    );
    // Every stub of the definition as it is stays what it was.
    let unchanged = expected_stubs(&definition, &read(VERSIONS)).expect("it compiles");
    for stub in &unchanged {
        assert_eq!(of(&stub.method), stub.instructions, "{}", stub.method);
    }
}

// ------------------------------------------------------------------ prefixed `syscall` (K9)

/// A program whose three functions each hold a `syscall` with a prefix byte before its two
/// bytes: an operand-size prefix, a repeat prefix and a REX byte. Each runs as a system call.
const PREFIXED: &str = r#"
#![no_std]
#![no_main]

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[unsafe(naked)]
extern "C" fn operand_size() {
    core::arch::naked_asm!(".byte 0x66, 0x0f, 0x05", "ret")
}

#[unsafe(naked)]
extern "C" fn repeat() {
    core::arch::naked_asm!(".byte 0xf3, 0x0f, 0x05", "ret")
}

#[unsafe(naked)]
extern "C" fn rex() {
    core::arch::naked_asm!(".byte 0x48, 0x0f, 0x05", "ret")
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    for f in [operand_size as extern "C" fn(), repeat, rex] {
        core::hint::black_box(f)();
    }
    loop {}
}
"#;

/// The carried-program gate, on a program the pinned compiler and linker made with three
/// prefixed `syscall`s (the review of 2026-11-13, finding K9): each is counted as a `syscall`
/// of the program and refused. The pinned `llvm-objdump` lists each as a plain `syscall` at the
/// prefix's address, one byte before the pair; a spelling with the prefix's name is held by
/// [`a_syscall_spelled_with_its_prefix_is_counted`].
#[test]
fn a_prefixed_syscall_in_a_really_linked_program_is_refused() {
    let dir = crate::target_dir()
        .join("xtask")
        .join("corestubs-tests")
        .join(format!("prefixed-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("prefixed.rs");
    fs::write(&source, PREFIXED).unwrap();
    let elf = dir.join("prefixed");
    let toolchain = crate::toolchain::toolchain().expect("the pinned toolchain");
    let mut rustc = Command::new(toolchain.sysroot.join("bin").join("rustc"));
    rustc
        .current_dir(&dir)
        .args([
            "--edition",
            "2024",
            "--crate-type",
            "bin",
            "--crate-name",
            "prefixed",
        ])
        .args(["--target", "x86_64-unknown-linux-gnu", "-C", "opt-level=2"]);
    for flag in userspace_flags() {
        rustc.arg("-C").arg(flag);
    }
    let out = rustc
        .arg("-o")
        .arg(&elf)
        .arg(&source)
        .output()
        .expect("rustc runs");
    assert!(
        out.status.success(),
        "rustc failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listing = disassemble(&elf).unwrap();
    let functions = read_listing(&listing).unwrap();
    let spelled: Vec<String> = functions
        .iter()
        .filter(|f| {
            ["operand_size", "repeat", "rex"]
                .iter()
                .any(|n| f.name.contains(n))
        })
        .filter_map(|f| f.instructions.first().map(|(_, i)| i.clone()))
        .collect();
    let problems = program_problems("prefixed", &functions, None);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        spelled.len(),
        3,
        "the three functions as listed: {functions:#?}"
    );
    assert!(
        problems.len() == 1 && problems[0].contains("holds 3 `syscall` instructions"),
        "the pinned disassembler spells them {spelled:?}; the gate said {problems:#?}"
    );
}

#[test]
fn a_syscall_spelled_with_its_prefix_is_counted() {
    for spelled in [
        "data16 syscall",
        "rep syscall",
        "rex64 syscall",
        "rex.W syscall",
    ] {
        let listing = program(&[("run", &["mov eax, 0x3", spelled, "ret"])]);
        let found = program_problems("first-program", &listing, None);
        assert!(
            found.len() == 1 && found[0].contains("holds 1 `syscall` instructions"),
            "{spelled}: {found:#?}"
        );
    }
    // An instruction that only mentions the word elsewhere is not one.
    let listing = program(&[("run", &["call 0x1000 <syscall_helper>", "ret"])]);
    assert!(program_problems("first-program", &listing, None).is_empty());
}
