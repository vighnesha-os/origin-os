//! End to end for a ring-3 program (task S4-1): compile a tiny `no_std` program for the
//! userspace triple with the pinned `rustc` and `rust-lld`, using exactly the flags of
//! `.cargo/userspace.toml`, describe it with `onbf-pack carried`, and relocate it at a base as the
//! kernel's launch path does. Then the negative control: the same program linked with a writable
//! code segment is refused.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use onbf_pack::carried::{Access, CarriedError, describe};
use onbf_pack::elf::ElfError;

const PROGRAM: &str = r#"
#![no_std]
#![no_main]

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}

static ONE: u64 = 0x4F4E_4500;
static TWO: u64 = 0x5457_4F00;
/// Two pointers: two relative relocations, in a RELRO page.
static POINTERS: [&u64; 2] = [&ONE, &TWO];

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let sum = core::hint::black_box(&POINTERS).iter().map(|p| **p).sum::<u64>();
    core::hint::black_box(sum);
    loop {}
}
"#;

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
    let mut flags = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("\"-C\", \"") {
            flags.push(rest.trim_end_matches(['"', ',']).to_string());
        }
    }
    assert!(
        flags.iter().any(|f| f == "target-cpu=x86-64-v3")
            && flags.iter().any(|f| f == "panic=abort")
            && flags.iter().any(|f| f == "link-arg=-ztext"),
        "{flags:?}"
    );
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

/// Compiles [`PROGRAM`] with the userspace flags and `extra` and returns its ELF.
fn compile(name: &str, extra: &[&str]) -> Vec<u8> {
    let dir = std::env::temp_dir().join(format!("onbf-pack-program-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("program.rs");
    fs::write(&source, PROGRAM).unwrap();
    let output = dir.join("program.elf");
    let mut cmd = Command::new(pinned_rustc());
    cmd.current_dir(&dir)
        .args([
            "--edition",
            "2024",
            "--crate-type",
            "bin",
            "--crate-name",
            "program",
        ])
        .args(["--target", "x86_64-unknown-linux-gnu", "-C", "opt-level=2"]);
    for flag in userspace_flags()
        .iter()
        .map(String::as_str)
        .chain(extra.iter().copied())
    {
        cmd.arg("-C").arg(flag);
    }
    let out = cmd.arg("-o").arg(&output).arg(&source).output().unwrap();
    assert!(
        out.status.success(),
        "rustc failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let elf = fs::read(&output).unwrap();
    let _ = fs::remove_dir_all(&dir);
    elf
}

#[test]
fn a_program_built_with_the_userspace_flags_is_described_and_relocates() {
    let elf = compile("good", &[]);
    let carried = describe(&elf).expect("the program converts");
    let code: Vec<_> = carried
        .segments
        .iter()
        .filter(|s| s.access == Access::ReadExecute)
        .collect();
    assert_eq!(code.len(), 1);
    assert!(carried.entry >= code[0].rva && carried.entry < code[0].rva + code[0].size);
    assert!(
        carried
            .segments
            .iter()
            .any(|s| s.access == Access::ReadAfterRelocation),
        "-z relro gives a RELRO segment"
    );
    assert!(carried.relocations.len() >= 2, "{:?}", carried.relocations);

    // Lay the program out at a base and add the base to every slot, as the launch path does;
    // then every relocated value lies inside the image.
    let base = 0x0000_0123_4560_0000u64;
    let mut memory = vec![0u8; usize::try_from(carried.span).unwrap()];
    for segment in &carried.segments {
        let at = usize::try_from(segment.rva).unwrap();
        memory[at..at + segment.bytes.len()].copy_from_slice(&segment.bytes);
    }
    for &slot in &carried.relocations {
        let at = usize::try_from(slot).unwrap();
        let value = u64::from_le_bytes(memory[at..at + 8].try_into().unwrap()) + base;
        assert!(
            value >= base && value < base + carried.span,
            "slot {slot:#x} holds {value:#x}, outside the image"
        );
        memory[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }

    // The same ELF gives the same description, twice.
    assert_eq!(
        describe(&elf).unwrap().rust("program", "PROGRAMS"),
        carried.rust("program", "PROGRAMS")
    );
}

#[test]
fn a_program_linked_with_a_writable_code_segment_is_refused() {
    // `-N` (`--omagic`): text and data in one segment, readable, writable and executable.
    let elf = compile("omagic", &["link-arg=-N"]);
    assert_eq!(
        describe(&elf),
        Err(CarriedError::Elf(ElfError::WriteAndExecute { header: 0 }))
    );
}
