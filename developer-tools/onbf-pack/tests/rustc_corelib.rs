//! End to end for `origin.core` (task S4-2; ADR-0011 Amendment 2, L2 to L6): compile two naked
//! stubs for the userspace triple with the pinned `rustc`, link them with `rust-lld` as a shared
//! object named `origin.core`, and describe it with `onbf-pack corelib`. The description holds
//! the stubs and nothing else, with one `syscall` each. Then the negative controls: a library
//! with a second `syscall` in one stub, one with a function nothing lists, and one with writable
//! data are each refused. And a program linked against the library reads as one that imports
//! from it, with a slot for each call in its read-only-after-relocation data and no `syscall`
//! of its own.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use onbf_pack::corelib::{CoreError, LIBRARY, Listing, describe};
use onbf_pack::elf::{Linking, parse_linked};

/// Two stubs in the generator's form: the extension of a narrow parameter, the fourth
/// argument's move, the load of the number, one `syscall`, and the return.
const STUBS: &str = r#"
#![no_std]

#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn origin_kernel_first(a: u32, b: u64, c: u64, d: u64) -> i64 {
    core::arch::naked_asm!("mov edi, edi", "mov r10, rcx", "mov eax, 7", "syscall", "ret")
}

#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn origin_kernel_last(code: u64) -> ! {
    core::arch::naked_asm!("mov eax, 0", "syscall", "ud2")
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
"#;

/// What makes a library something else than its stubs, each appended to [`STUBS`].
const SECOND_SYSCALL: &str = r#"
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn origin_kernel_twice(code: u64) -> i64 {
    core::arch::naked_asm!("mov eax, 9", "syscall", "syscall", "ret")
}
"#;
const UNLISTED: &str = r#"
#[unsafe(no_mangle)]
pub extern "C" fn origin_kernel_helper(a: u64) -> u64 {
    a.wrapping_mul(3)
}
"#;
const WRITABLE: &str = r#"
#[unsafe(no_mangle)]
pub static mut ORIGIN_KERNEL_STATE: u64 = 7;
"#;

const PROGRAM: &str = r#"
#![no_std]
#![no_main]

unsafe extern "C" {
    fn origin_kernel_first(a: u32, b: u64, c: u64, d: u64) -> i64;
    fn origin_kernel_last(code: u64) -> !;
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let status = unsafe { origin_kernel_first(1, 2, 3, 4) };
    unsafe { origin_kernel_last(status as u64) }
}
"#;

const RELEASE: &str =
    "release sha256:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn listing(entries: &[&str]) -> Listing {
    let mut text = format!("# oidl-core-exports 1 origin.kernel\n{RELEASE}\n");
    for entry in entries {
        text.push_str(entry);
        text.push('\n');
    }
    Listing::parse(&text).expect("the list reads")
}

fn stubs_listing() -> Listing {
    listing(&[
        "0 origin_kernel_last last 0.0",
        "7 origin_kernel_first first 0.3",
    ])
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

/// The `-C` flags of `.cargo/userspace.toml`, in order: every quoted string after a `"-C",`.
fn userspace_flags() -> Vec<String> {
    let text = fs::read_to_string(workspace_root().join(".cargo/userspace.toml")).unwrap();
    let flags: Vec<String> = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("\"-C\", \""))
        .map(|rest| rest.trim_end_matches(['"', ',']).to_string())
        .collect();
    assert!(flags.iter().any(|f| f == "link-arg=-zrelro"), "{flags:?}");
    flags
}

/// The pinned toolchain's `rustc` (see `rustc_kernel.rs`).
fn pinned_rustc() -> PathBuf {
    let out = Command::new("rustc")
        .args(["--print", "sysroot"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(out.status.success());
    Path::new(String::from_utf8(out.stdout).unwrap().trim())
        .join("bin")
        .join("rustc")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("onbf-pack-corelib-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Compiles `source` in `dir` as `kind` with the userspace flags and `extra`, and returns the
/// path of what the linker wrote.
fn compile(dir: &Path, kind: &str, name: &str, source: &str, extra: &[String]) -> PathBuf {
    let file = dir.join(format!("{name}.rs"));
    fs::write(&file, source).unwrap();
    let output = dir.join(name);
    let mut cmd = Command::new(pinned_rustc());
    cmd.current_dir(dir)
        .args([
            "--edition",
            "2024",
            "--crate-type",
            kind,
            "--crate-name",
            name,
        ])
        .args(["--target", "x86_64-unknown-linux-gnu", "-C", "opt-level=2"]);
    for flag in userspace_flags().iter().chain(extra) {
        cmd.arg("-C").arg(flag);
    }
    let out = cmd.arg("-o").arg(&output).arg(&file).output().unwrap();
    assert!(
        out.status.success(),
        "rustc failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    output
}

/// Links `source` as the shared object `origin.core`, as `cargo xtask` links `runtime/core`.
fn library(dir: &Path, source: &str) -> PathBuf {
    compile(
        dir,
        "cdylib",
        "origin_core",
        source,
        &[format!("link-arg=--soname={LIBRARY}")],
    )
}

#[test]
fn a_library_of_naked_stubs_is_described_as_the_stubs_and_nothing_else() {
    let dir = scratch("good");
    let elf = fs::read(library(&dir, STUBS)).unwrap();
    let core = describe(&elf, &stubs_listing()).expect("origin.core converts");
    assert_eq!(
        core,
        describe(&elf, &stubs_listing()).unwrap(),
        "the same twice"
    );
    assert_eq!(core.span, 4096);
    // By name: `first` then `last`. In the code, each is where its symbol says.
    let names: Vec<&str> = core.exports.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(names, ["origin_kernel_first", "origin_kernel_last"]);
    assert_eq!(core.exports[0].since, (0, 3));
    let at = |offset: u64, len: usize| &core.code[offset as usize..][..len];
    // `mov edi, edi`, `mov r10, rcx`, `mov eax, 7`, `syscall`, `ret`.
    assert_eq!(
        at(core.exports[0].offset, 13),
        [
            0x89, 0xFF, 0x49, 0x89, 0xCA, 0xB8, 7, 0, 0, 0, 0x0F, 0x05, 0xC3
        ]
    );
    // `mov eax, 0`, `syscall`, `ud2`.
    assert_eq!(
        at(core.exports[1].offset, 9),
        [0xB8, 0, 0, 0, 0, 0x0F, 0x05, 0x0F, 0x0B]
    );
    // The pins, by number: each is its stub's one `syscall`.
    let pins: Vec<(&str, u64, u64)> = core
        .pins
        .iter()
        .map(|p| (p.method.as_str(), p.number, p.offset))
        .collect();
    assert_eq!(
        pins,
        [
            ("last", 0, core.exports[1].offset + 5),
            ("first", 7, core.exports[0].offset + 10)
        ]
    );
    // Nothing but the two stubs and the linker's padding: 22 bytes of instructions.
    let padding = core.code.iter().filter(|&&b| b == 0xCC).count();
    assert_eq!(core.code.len() - padding, 22, "{:02x?}", core.code);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_library_that_is_more_than_its_listed_stubs_is_refused() {
    let dir = scratch("more");
    // A second `syscall` in one stub.
    let twice = fs::read(library(&dir, &format!("{STUBS}{SECOND_SYSCALL}"))).unwrap();
    let listed = listing(&[
        "0 origin_kernel_last last 0.0",
        "7 origin_kernel_first first 0.3",
        "9 origin_kernel_twice twice 0.0",
    ]);
    assert_eq!(
        describe(&twice, &listed),
        Err(CoreError::Syscalls {
            name: "origin_kernel_twice".to_string(),
            found: 2
        })
    );
    // A function the generator's list does not name.
    let helper = fs::read(library(&dir, &format!("{STUBS}{UNLISTED}"))).unwrap();
    assert_eq!(
        describe(&helper, &stubs_listing()),
        Err(CoreError::Unlisted("origin_kernel_helper".to_string()))
    );
    // Writable data: its segment holds bytes that are not the linker's dynamic table.
    let writable = fs::read(library(&dir, &format!("{STUBS}{WRITABLE}"))).unwrap();
    assert!(
        matches!(
            describe(&writable, &stubs_listing()),
            Err(CoreError::WritableData(_))
        ),
        "{:?}",
        describe(&writable, &stubs_listing())
    );
    // A listed stub the library lacks.
    let good = fs::read(library(&dir, STUBS)).unwrap();
    assert_eq!(
        describe(&good, &listed),
        Err(CoreError::Missing("origin_kernel_twice".to_string()))
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_program_linked_against_it_imports_each_call_through_a_relro_slot() {
    let dir = scratch("program");
    let core = library(&dir, STUBS);
    let program = compile(
        &dir,
        "bin",
        "program",
        PROGRAM,
        &[format!("link-arg={}", core.display())],
    );
    let elf = fs::read(program).unwrap();
    let list = stubs_listing();
    let symbols = list.symbols();
    let parsed = parse_linked(
        &elf,
        Linking::Importing {
            library: LIBRARY,
            exports: &symbols,
        },
    )
    .expect("the program reads as one that imports from origin.core");
    let mut imported: Vec<&str> = parsed.imports.iter().map(|i| i.symbol.as_str()).collect();
    imported.sort_unstable();
    assert_eq!(imported, ["origin_kernel_first", "origin_kernel_last"]);
    for import in &parsed.imports {
        let segment = parsed
            .segments
            .iter()
            .find(|s| import.slot >= s.vaddr && import.slot < s.vaddr + s.mapped_size)
            .expect("the slot is mapped");
        assert!(segment.relro, "slot {:#x} is not in RELRO", import.slot);
    }
    // The program's own code holds no `syscall`: the stubs are the library's.
    for segment in &parsed.segments {
        if segment.permissions == onbf_pack::elf::Permissions::ReadExecute {
            let code = &elf[segment.file_offset as usize..][..segment.file_size as usize];
            assert!(
                !code.windows(2).any(|pair| pair == [0x0F, 0x05]),
                "{code:02x?}"
            );
        }
    }
    // Laid out for the kernel: each import is a slot and an export by its place in name order,
    // the slot holds zero, and the description carries the list's fingerprint.
    let carried = onbf_pack::carried::describe_importing(&elf, &list).expect("it converts");
    let mut laid: Vec<(usize, &str)> = carried
        .imports
        .iter()
        .map(|i| (i.export, i.symbol.as_str()))
        .collect();
    laid.sort_unstable();
    assert_eq!(
        laid,
        [(0, "origin_kernel_first"), (1, "origin_kernel_last")]
    );
    assert_eq!(carried.core, Some(list.release));
    for import in &carried.imports {
        let segment = carried
            .segments
            .iter()
            .find(|s| import.slot >= s.rva && import.slot < s.rva + s.size)
            .unwrap();
        assert_eq!(
            segment.access,
            onbf_pack::carried::Access::ReadAfterRelocation
        );
        let at = (import.slot - segment.rva) as usize;
        assert_eq!(segment.bytes[at..at + 8], [0; 8]);
        assert!(!carried.relocations.contains(&import.slot));
    }
    // Read as a static image, as before this task, it is refused for its library.
    assert_eq!(
        onbf_pack::elf::parse(&elf),
        Err(onbf_pack::elf::ElfError::Needed)
    );
    let _ = fs::remove_dir_all(&dir);
}
