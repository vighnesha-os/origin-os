//! TASKS item 112: the bench build of the loader and the kernel, with breadcrumbs on the SPCR's
//! UART, and what keeps it out of every other image.
//!
//! A bench build is the same source with one more compiler flag, `--cfg origin_bench`, given
//! for both freestanding targets ([`toolchain::bench_config`]), into a target directory of its own
//! ([`target`]). Only [`build`] (`cargo xtask bench-build`) and [`boot`] (`cargo xtask
//! boot-bench`) give it. The loader is the developer-mode loader with the TEST trust store, so it
//! says it must never ship, and the kernel is never signed (ADR-0002 B8): a release loader refuses
//! it. Three checks hold the rest:
//! - F1, [`gate`] (part of `check`): the flag's name appears only in [`FILES`];
//! - F2, `reproducible::gate`: no image a normal build makes holds [`MARKER`], and [`build`]
//!   requires it in both bench images, so the search is shown to see it;
//! - [`refuse`]: no signing path takes a file from [`target`], and a bench kernel is packed only
//!   into it.
//!
//! [`boot`] boots the bench pair under QEMU sixteen ways, the SPCR naming COM2 and OVMF's console
//! on COM1. With `BENCH-AFTER` asking for the kernel (TASKS item 112's path): `ExitBootServices`
//! given the firmware's key, a stale key once (the retry path, under real firmware code), a stale
//! key twice (the halt, which must say why and then nothing), and the bench kernel under the
//! normal developer-mode loader (no magic in RSI: no breadcrumbs, and the boot otherwise
//! unchanged). Without it, as a rental runs (TASKS item 116): the reset through `ResetSystem`,
//! the reset through the FADT's register with `ResetSystem` not called, and a UART disturbed as
//! the call returns and written back. With `BENCH-FCH` (TASKS item 121), the nine of the FCH's
//! reset with no firmware code: the control before the call; `0xCF9` after it, the triple fault,
//! and the fall-through to TASKS item 116's path; no record of the last reset; the reset only
//! after the retry succeeds, and the normal path after both calls fail; a contradicting layout;
//! and a control that does not reset. QEMU runs with `-no-reboot`, so a reset is QEMU exiting.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::settings::Settings;
use crate::{
    Capture, Completed, KERNEL_WRITES_COM2, LOADER_NAMES_COM2, Loader, QemuArgs, Result, SPCR_PORT,
    Scenario, TAKEN_BYTES, TAKEN_OVER_LINE, cargo, check_spcr_console, console_markers,
    qemu_boot_failures, read_file, repo_root, run_scenario, run_step, serial, toolchain,
};

/// The text every bench image holds and no other image may (F2): the start of the bench lines'
/// second word pair, `origin-boot: bench build:` and `origin-kernel: bench build:`.
pub(crate) const MARKER: &[u8] = b"bench build";

/// The flag's name, which only [`FILES`] may hold (F1).
const CFG: &str = "origin_bench";

/// The minimal mode's flag (ADR-0002 Amendment 5; TASKS item 128), given with [`CFG`]; only
/// [`MINIMAL_FILES`] may hold it (F1, the coordinating session's ruling Q2).
const MINIMAL_CFG: &str = "origin_bench_minimal";

/// The text every minimal-mode image holds, beside [`MARKER`], and no full bench image may.
pub(crate) const MINIMAL_MARKER: &[u8] = b"bench build: minimal";

/// The files that may name the minimal mode's flag. Each also names the bench flag, which its name
/// begins with, so each is on [`FILES`] too.
const MINIMAL_FILES: &[(&str, &str)] = &[
    ("Cargo.toml", "the flag's check-cfg declaration"),
    (
        "boot/uefi/src/main.rs",
        "the minimal mode's entry and module (TASKS item 128)",
    ),
    (
        "developer-tools/xtask/src/toolchain.rs",
        "the one place the flag is given",
    ),
    ("developer-tools/xtask/src/bench.rs", "this gate"),
];

/// The files that may name the bench flag, each for a reason (F1). A file leaves the list when its
/// last mention goes: the gate fails on a listed file that holds none.
const FILES: &[(&str, &str)] = &[
    ("Cargo.toml", "the flag's check-cfg declaration"),
    ("boot/info/src/lib.rs", "the bench hand-over's module"),
    ("boot/info/src/bench.rs", "the bench hand-over"),
    (
        "boot/uefi/src/bench.rs",
        "the bench loader's lines and switch",
    ),
    (
        "boot/uefi/src/fch.rs",
        "the bench loader's FCH reads and reset (TASKS item 121)",
    ),
    ("framekernel/frame/src/bench.rs", "the breadcrumbs"),
    ("boot/uefi/src/lib.rs", "the bench loader's module"),
    (
        "boot/uefi/src/main.rs",
        "the bench loader's lines, entry stub and stale-key hook",
    ),
    ("framekernel/frame/src/lib.rs", "the breadcrumbs' module"),
    (
        "framekernel/frame/src/port.rs",
        "the breadcrumbs' port write",
    ),
    ("framekernel/src/lib.rs", "the `crumb!` macro"),
    (
        "framekernel/src/kernel.rs",
        "`kernel_main`'s bench argument and line",
    ),
    (
        "developer-tools/xtask/src/toolchain.rs",
        "the one place the flag is given",
    ),
    ("developer-tools/xtask/src/bench.rs", "this gate"),
];

/// The flags F1 holds to their listed files: the bench build's, and TASKS item 127's stack
/// measurement, whose rule is the same (the coordinating session's ruling on TASKS item 127, Q1).
const FLAGS: [(&str, &[(&str, &str)]); 3] = [
    (CFG, FILES),
    (MINIMAL_CFG, MINIMAL_FILES),
    (crate::stack_depth::CFG, crate::stack_depth::FILES),
];

/// The extensions of the files F1 reads: code, build configuration and CI.
const SCANNED: &[&str] = &["rs", "toml", "yml", "yaml", "sh", "py", "json"];

/// Where bench builds go, apart from every other build.
pub(crate) fn target() -> PathBuf {
    crate::target_dir().join("bench")
}

/// Where minimal-mode builds go (TASKS item 128), apart from every other build.
pub(crate) fn minimal_target() -> PathBuf {
    crate::target_dir().join("bench-minimal")
}

/// F1: the bench flag's name appears only in [`FILES`], and in each of them; and the
/// measurement flag's only in its own list ([`FLAGS`]).
pub(crate) fn gate() -> Result {
    let root = repo_root();
    let mut files = Vec::new();
    let want = |name: &str| {
        Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| SCANNED.contains(&e))
    };
    let skip = |path: &Path| path.starts_with(root.join("docs"));
    crate::collect_files(&root, &skip, &want, &mut files)
        .map_err(|e| format!("bench gate: {e}"))?;
    // `collect_files` skips folders whose names start with a dot: CI's and Cargo's
    // configuration are read too (TASKS item 112's review, R112-4).
    for dotted in [".github", ".cargo"] {
        let dir = root.join(dotted);
        if dir.is_dir() {
            crate::collect_files(&dir, &skip, &|_| true, &mut files)
                .map_err(|e| format!("bench gate: {e}"))?;
        }
    }
    let texts: Vec<(String, String)> = files
        .iter()
        .filter_map(|path| {
            let text = fs::read_to_string(path).ok()?;
            let rel = path
                .strip_prefix(&root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            Some((rel, text))
        })
        .collect();
    let mut counts = Vec::new();
    for (flag, listed) in FLAGS {
        let found: Vec<String> = texts
            .iter()
            .filter(|(_, text)| text.contains(flag))
            .map(|(rel, _)| rel.clone())
            .collect();
        check_found(flag, listed, &found)?;
        counts.push(found.len());
    }
    println!(
        "xtask: bench gate (TASKS item 112, F1): the bench flag is named in {} listed files and nowhere else, the minimal mode's flag (TASKS item 128) in {}, and the stack measurement's flag (TASKS item 127) in {}",
        counts[0], counts[1], counts[2]
    );
    Ok(())
}

/// The heart of [`gate`]: `found` are the files that name `flag`, which only `files` may.
fn check_found(flag: &str, files: &[(&str, &str)], found: &[String]) -> Result {
    let which = match flag {
        CFG => "the bench flag",
        MINIMAL_CFG => "the minimal mode's flag",
        _ => "the stack measurement's flag",
    };
    let listed: Vec<&str> = files.iter().map(|(path, _)| *path).collect();
    let mut problems: Vec<String> = found
        .iter()
        .filter(|path| !listed.contains(&path.as_str()))
        .map(|path| {
            format!("{path} names {which} `{flag}`, and only listed files may (TASKS item 112, F1)")
        })
        .collect();
    problems.extend(
        listed
            .iter()
            .filter(|path| !found.iter().any(|f| f == *path))
            .map(|path| {
                format!(
                    "{path} is on the bench gate's list and no longer names `{flag}`: take it off"
                )
            }),
    );
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// Whether `path` is under [`target`] or [`minimal_target`].
pub(crate) fn is_bench(path: &Path) -> bool {
    [target(), minimal_target()].iter().any(|root| {
        path.starts_with(root)
            || fs::canonicalize(path)
                .ok()
                .zip(fs::canonicalize(root).ok())
                .is_some_and(|(path, root)| path.starts_with(root))
    })
}

/// Fails if `path` is a bench build's file, or a stack measurement's (TASKS item 127): no signing
/// path takes one (TASKS item 112, ruling Q1).
pub(crate) fn refuse(path: &Path, what: &str) -> Result {
    if crate::stack_depth::is_measure(path) {
        return Err(format!(
            "{what}: {} is a stack measurement's file (target/stack-measure): a measurement image is never signed (TASKS item 127)",
            path.display()
        ));
    }
    if is_bench(path) {
        Err(format!(
            "{what}: {} is a bench build's file (target/bench): a bench image is never signed, and is packed only for a bench boot (TASKS item 112)",
            path.display()
        ))
    } else {
        Ok(())
    }
}

/// The bench loader: the developer-mode loader with the bench flag, into [`target`].
pub(crate) fn build_loader() -> Result<PathBuf> {
    crate::build_loader_with(
        Some(Loader::Developer.feature()),
        Some(&target()),
        &toolchain::bench_config()?,
    )
}

/// The minimal-mode bench loader (ADR-0002 Amendment 5): the developer-mode loader with the bench
/// flag and the minimal mode's, into [`minimal_target`].
pub(crate) fn build_minimal_loader() -> Result<PathBuf> {
    crate::build_loader_with(
        Some(Loader::Developer.feature()),
        Some(&minimal_target()),
        &toolchain::bench_minimal_config()?,
    )
}

/// `cargo xtask bench-build --minimal` (TASKS item 128): the full bench images first, then the
/// minimal-mode loader, staged with the bench kernel (which it checks is there but never reads) as
/// `target/bench-minimal/xtask/BOOTX64.EFI` and `KERNEL.ONBF` for a rental's `--image-dir`.
/// Fails unless the loader holds both [`MARKER`] and [`MINIMAL_MARKER`], writes `0xCF9` nowhere,
/// and holds the triple fault right after the call ([`check_minimal_code`]).
pub(crate) fn build_minimal() -> Result<(PathBuf, PathBuf)> {
    let (_, bench_kernel) = build()?;
    let efi = build_minimal_loader()?;
    let out = minimal_target().join("xtask");
    fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let loader = out.join("BOOTX64.EFI");
    let kernel = out.join(crate::KERNEL_IMAGE);
    fs::copy(&efi, &loader).map_err(|e| format!("copy {}: {e}", efi.display()))?;
    fs::copy(&bench_kernel, &kernel)
        .map_err(|e| format!("copy {}: {e}", bench_kernel.display()))?;
    let bytes = read_file(&loader)?;
    if !holds(&bytes, MARKER) || !holds(&bytes, MINIMAL_MARKER) {
        return Err(format!(
            "{} is a minimal-mode image without the texts \"bench build\" and \"bench build: minimal\"",
            loader.display()
        ));
    }
    check_minimal_code(&loader)?;
    for path in [&loader, &kernel] {
        let bytes = read_file(path)?;
        println!(
            "xtask: bench-build --minimal: {}: {} bytes, SHA-256 {}",
            path.display(),
            bytes.len(),
            crate::reproducible::hex(&origin_crypto::Sha256::digest(&bytes), "")
        );
    }
    Ok((loader, kernel))
}

/// The minimal-mode loader's machine code, as the pinned toolchain's `llvm-objdump` prints it
/// (TASKS item 128): no `0xCF9` in a port register anywhere ("there is no `0xCF9` write anywhere in
/// this mode"), and a path from an indirect call to the triple fault with nothing on it but reads
/// into registers, compares and branches (Amendment 5, step 6: "nothing comes between them").
fn check_minimal_code(loader: &Path) -> Result {
    let listing = disassemble(loader)?;
    if writes_cf9(&listing) {
        return Err(format!(
            "{}: the minimal-mode loader puts 0xCF9 in DX: it would write port 0xCF9, which ADR-0002 Amendment 5 forbids in this mode",
            loader.display()
        ));
    }
    if !triple_fault_after_call(&listing) {
        return Err(format!(
            "{}: no path from an indirect call to the triple fault holds only reads into registers, compares and branches: the triple fault is not the first thing after ExitBootServices (ADR-0002 Amendment 5, step 6)",
            loader.display()
        ));
    }
    Ok(())
}

/// `llvm-objdump`'s Intel listing of `image`.
fn disassemble(image: &Path) -> Result<String> {
    let objdump = crate::llvm_tool("llvm-objdump")?;
    let out = Command::new(&objdump)
        .args(["-d", "--no-show-raw-insn", "--x86-asm-syntax=intel"])
        .arg(image)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", objdump.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{} failed on {}: {}",
            objdump.display(),
            image.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether `listing` puts `0xCF9` in DX, the only way to address the port.
fn writes_cf9(listing: &str) -> bool {
    instructions(listing)
        .iter()
        .any(|i| matches!(i.text.as_str(), "mov dx, 0xcf9" | "mov edx, 0xcf9"))
}

/// Whether `listing` holds an indirect call and, within [`AFTER_CALL_WINDOW`] instructions along
/// some path from it, the triple fault's first instruction (`push 0x0`, `push 0x0`,
/// `lidt [rsp]`), every instruction before it on the path one [`step`] allows.
fn triple_fault_after_call(listing: &str) -> bool {
    let insns = instructions(listing);
    let mut index = HashMap::new();
    for (i, insn) in insns.iter().enumerate() {
        index.entry(insn.at).or_insert(i);
    }
    let is_triple_fault = |i: usize| {
        [i, i + 1, i + 2]
            .iter()
            .map(|&k| insns.get(k).map(|insn| insn.text.as_str()))
            .eq([Some("push 0x0"), Some("push 0x0"), Some("lidt [rsp]")])
    };
    let reaches = |from: usize| {
        let mut queue = VecDeque::from([(from, 0)]);
        let mut seen = HashSet::new();
        while let Some((i, n)) = queue.pop_front() {
            if n > AFTER_CALL_WINDOW || !seen.insert(i) {
                continue;
            }
            if is_triple_fault(i) {
                return true;
            }
            let Some(step) = insns.get(i).and_then(|insn| step(&insn.text)) else {
                continue;
            };
            let (fall, to) = match step {
                Step::Next => (true, None),
                Step::Branch(at) => (true, index.get(&at).copied()),
                Step::Jump(at) => (false, index.get(&at).copied()),
            };
            for j in fall.then_some(i + 1).into_iter().chain(to) {
                queue.push_back((j, n + 1));
            }
        }
        false
    };
    insns.iter().enumerate().any(|(i, insn)| {
        insn.text
            .strip_prefix("call ")
            .is_some_and(|op| !op.starts_with("0x"))
            && reaches(i + 1)
    })
}

/// `cargo xtask bench-build`: builds the bench loader and kernel into [`target`], copies them to
/// `target/bench/xtask/BOOTX64.EFI` and `KERNEL.ONBF` for a rental's `--image-dir`, and prints
/// each file's size and SHA-256. Fails unless both hold [`MARKER`] (F2's control).
pub(crate) fn build() -> Result<(PathBuf, PathBuf)> {
    let dir = target();
    let efi = build_loader()?;
    let elf = crate::build_kernel_with(Some(&dir), &toolchain::bench_config()?)?;
    let out = dir.join("xtask");
    let kernel = crate::pack_kernel_to(&elf, &out.join(crate::KERNEL_IMAGE))?;
    let loader = out.join("BOOTX64.EFI");
    fs::copy(&efi, &loader).map_err(|e| format!("copy {}: {e}", efi.display()))?;
    for path in [&loader, &kernel] {
        let bytes = read_file(path)?;
        if !holds(&bytes, MARKER) {
            return Err(format!(
                "{} is a bench image without the text \"bench build\": F2's search in the normal images could not see a bench build's code",
                path.display()
            ));
        }
        if holds(&bytes, crate::stack_depth::MARKER) {
            return Err(format!(
                "{} holds the text \"stack measure\": the stack measurement's code reached a bench image (TASKS item 127, F2)",
                path.display()
            ));
        }
        if holds(&bytes, MINIMAL_MARKER) {
            return Err(format!(
                "{} holds the text \"bench build: minimal\": the minimal mode's code reached the full bench image (TASKS item 128)",
                path.display()
            ));
        }
        println!(
            "xtask: bench-build: {}: {} bytes, SHA-256 {}",
            path.display(),
            bytes.len(),
            crate::reproducible::hex(&origin_crypto::Sha256::digest(&bytes), "")
        );
    }
    Ok((loader, kernel))
}

/// TASKS item 121's review, R121-8, and the coordinating session's ruling on R121-3: the bench
/// loader's machine code, as the pinned toolchain's `llvm-objdump` prints it, holds a path from an
/// indirect call to a write of `0x06` to port `0xCF9` with nothing on it but reads into registers,
/// compares and branches: the plan's checks and the status's. No store, port instruction, call or
/// other instruction stands between, so the reset is the first thing after `ExitBootServices`
/// returns. No scenario can see the order, since under QEMU the call returns.
fn check_reset_after_call(loader: &Path) -> Result {
    let objdump = crate::llvm_tool("llvm-objdump")?;
    let out = Command::new(&objdump)
        .args(["-d", "--no-show-raw-insn", "--x86-asm-syntax=intel"])
        .arg(loader)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", objdump.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{} failed on {}: {}",
            objdump.display(),
            loader.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    if reset_after_call(&String::from_utf8_lossy(&out.stdout)) {
        Ok(())
    } else {
        Err(format!(
            "{}: no path from an indirect call to a write of 0x06 to port 0xCF9 holds only reads into registers, compares and branches: the reset after ExitBootServices is not the first thing after it (TASKS item 121, R121-8 and R121-3)",
            loader.display()
        ))
    }
}

/// The instructions a path from the call to the reset's `out` holds at most.
const AFTER_CALL_WINDOW: usize = 64;

/// One instruction of `llvm-objdump`'s Intel listing: its address, and its text with single
/// spaces and no `#` comment.
pub(crate) struct Insn {
    pub(crate) at: u64,
    pub(crate) text: String,
}

/// The instructions of `listing`; label, section and file lines are left out.
pub(crate) fn instructions(listing: &str) -> Vec<Insn> {
    listing
        .lines()
        .filter_map(|line| {
            let (at, rest) = line.split_once(':')?;
            let at = u64::from_str_radix(at.trim(), 16).ok()?;
            let rest = rest.split('#').next().unwrap_or_default();
            let words: Vec<&str> = rest.split_whitespace().collect();
            (!words.is_empty()).then(|| Insn {
                at,
                text: words.join(" "),
            })
        })
        .collect()
}

/// Where a path goes from an instruction that may stand between the call and the reset.
enum Step {
    /// To the next instruction.
    Next,
    /// To the next instruction or to this address: a conditional branch.
    Branch(u64),
    /// To this address only.
    Jump(u64),
}

/// The step `text` makes, if it may stand between the call and the reset: a `mov`, `movzx`,
/// `movsx` or `movsxd` into a register, which at most reads memory; `cmp`, `test` and `nop`; and
/// branches and jumps to a fixed address. `None` for anything else: a store, a port
/// instruction, a call, an indirect jump, or any other instruction.
fn step(text: &str) -> Option<Step> {
    let (mnemonic, operands) = text.split_once(' ').unwrap_or((text, ""));
    let target = || {
        let hex = operands.split_whitespace().next()?.strip_prefix("0x")?;
        u64::from_str_radix(hex, 16).ok()
    };
    match mnemonic {
        "mov" | "movzx" | "movsx" | "movsxd" => {
            let dest = operands.split(", ").next().unwrap_or_default();
            (!dest.is_empty() && !dest.contains('[') && !dest.contains("ptr")).then_some(Step::Next)
        }
        "cmp" | "test" | "nop" => Some(Step::Next),
        "jmp" => target().map(Step::Jump),
        m if m.starts_with('j') => target().map(Step::Branch),
        _ => None,
    }
}

/// Whether AL holds `0x06` and DX `0xCF9` after `text`, given whether they did before: a write to
/// either that is not the reset's own value clears it.
fn reset_values(text: &str, al: bool, dx: bool) -> (bool, bool) {
    let Some(("mov" | "movzx" | "movsx" | "movsxd", operands)) = text.split_once(' ') else {
        return (al, dx);
    };
    let dest = operands.split(", ").next().unwrap_or_default();
    let al = match dest {
        "al" | "ah" | "ax" | "eax" | "rax" => matches!(text, "mov al, 0x6" | "mov eax, 0x6"),
        _ => al,
    };
    let dx = match dest {
        "dl" | "dh" | "dx" | "edx" | "rdx" => matches!(text, "mov dx, 0xcf9" | "mov edx, 0xcf9"),
        _ => dx,
    };
    (al, dx)
}

/// Whether `listing` (`llvm-objdump`'s Intel syntax) holds an indirect `call qword ptr [...]` and,
/// within [`AFTER_CALL_WINDOW`] instructions along some path from it (falling through, or taking a
/// branch or jump to a fixed address), `out dx, al` with AL `0x06` and DX `0xCF9`, every
/// instruction before it on the path one [`step`] allows.
fn reset_after_call(listing: &str) -> bool {
    let insns = instructions(listing);
    let mut index = HashMap::new();
    for (i, insn) in insns.iter().enumerate() {
        index.entry(insn.at).or_insert(i);
    }
    let reaches = |from: usize| {
        let mut queue = VecDeque::from([(from, false, false, 0)]);
        let mut seen = HashSet::new();
        while let Some((i, al, dx, n)) = queue.pop_front() {
            if n > AFTER_CALL_WINDOW || !seen.insert((i, al, dx)) {
                continue;
            }
            let Some(insn) = insns.get(i) else { continue };
            if insn.text == "out dx, al" {
                if al && dx {
                    return true;
                }
                continue;
            }
            let Some(step) = step(&insn.text) else {
                continue;
            };
            let (al, dx) = reset_values(&insn.text, al, dx);
            let (fall, to) = match step {
                Step::Next => (true, None),
                Step::Branch(at) => (true, index.get(&at).copied()),
                Step::Jump(at) => (false, index.get(&at).copied()),
            };
            for j in fall.then_some(i + 1).into_iter().chain(to) {
                queue.push_back((j, al, dx, n + 1));
            }
        }
        false
    };
    insns
        .iter()
        .enumerate()
        .any(|(i, insn)| insn.text.starts_with("call qword ptr [") && reaches(i + 1))
}

/// Whether `bytes` holds `needle`.
pub(crate) fn holds(bytes: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && bytes.windows(needle.len()).any(|w| w == needle)
}

/// Clippy over the bench flag's code, which no other Clippy run sees: the loader on its target,
/// and the kernel with the framework and boot info on theirs, into [`target`].
fn clippy() -> Result {
    // TASKS item 128: the minimal mode's loader, which no other Clippy run sees either.
    let mut minimal = cargo();
    minimal
        .args(["clippy", "--package", crate::LOADER_PACKAGE, "--target"])
        .arg(crate::UEFI_TARGET)
        .args(["--features", Loader::Developer.feature(), "--target-dir"])
        .arg(minimal_target())
        .args(toolchain::bench_minimal_config()?)
        .args(["--", "-D", "warnings"]);
    run_step(&mut minimal)?;
    let config = toolchain::bench_config()?;
    let dir = target();
    let mut loader = cargo();
    loader
        .args(["clippy", "--package", crate::LOADER_PACKAGE, "--target"])
        .arg(crate::UEFI_TARGET)
        .args(["--features", Loader::Developer.feature(), "--target-dir"])
        .arg(&dir)
        .args(&config)
        .args(["--", "-D", "warnings"]);
    run_step(&mut loader)?;
    let mut kernel = cargo();
    kernel
        .args([
            "clippy",
            "--package",
            crate::KERNEL_PACKAGE,
            "--package",
            "origin-frame",
            "--package",
            "origin-boot-info",
            "--target",
        ])
        .arg(crate::KERNEL_TARGET)
        .arg("--target-dir")
        .arg(&dir)
        .args(&config)
        .args(["--", "-D", "warnings"]);
    // The kernel includes the programs it carries, which xtask builds and names, as for its build.
    let kernel = crate::ring3::with_programs(&mut kernel, Some(&dir))?;
    run_step(kernel)
}

/// The bench loader's line on the SPCR's UART when the first call succeeds.
const FIRST_CALL: &str = "origin-boot: bench build: boot services exited on the first call";
/// ... when the retry succeeds after a stale key.
const RETRY: &str = "origin-boot: bench build: boot services exited on the retry; the first call returned EFI_INVALID_PARAMETER [the first call was given a stale key: BENCH-STALE-KEY]";
/// ... when both calls fail: every loader's line before it halts (decision item 342), how it
/// begins and the whole of it under OVMF with two stale keys. It is the normal build's line, so
/// it does not say "bench build".
const HALTING_START: &str =
    "origin-boot: ExitBootServices failed (first call EFI_INVALID_PARAMETER";
const HALTING: &str = "origin-boot: ExitBootServices failed (first call EFI_INVALID_PARAMETER, retry EFI_INVALID_PARAMETER): halting";
/// The bench loader's lines on its console (COM1 under OVMF) about the switch.
const NO_SWITCH: &str =
    "origin-boot: bench build: no BENCH-STALE-KEY: ExitBootServices gets the firmware's own key";
const SWITCH_ONCE: &str =
    "origin-boot: bench build: BENCH-STALE-KEY: the first ExitBootServices call gets a stale key";
const SWITCH_TWICE: &str =
    "origin-boot: bench build: BENCH-STALE-KEY: both ExitBootServices calls get a stale key";
/// How the bench loader's state line begins, on its console.
const STATE: &str = "origin-boot: bench build: entry state: CR0 ";
/// The loader's last line on its console.
const LAST_LOADER_LINE: &str = "origin-boot: exiting boot services; entering the Framekernel";
/// The breadcrumbs, on a line of their own: the stub's two, then the kernel's eight.
pub(crate) const CRUMBS: &str = "RS12345678";
/// The bench kernel's line about its breadcrumbs.
const SAID_PORT: &str = "origin-kernel: bench build: breadcrumbs went to I/O port 0x2f8";
const SAID_NONE: &str =
    "origin-kernel: bench build: no breadcrumbs: the loader handed over no port";
/// How long the halting scenario's UART must stay silent after its line.
const QUIET: Duration = Duration::from_secs(20);

// TASKS item 116's lines.
/// On the loader's console: no `BENCH-AFTER`, as on a rental; and each mode a QEMU scenario asks
/// for.
const NO_AFTER: &str = "origin-boot: bench build: no BENCH-AFTER: after ExitBootServices the loader resets the machine, and the kernel is not entered";
const AFTER_KERNEL: &str = "origin-boot: bench build: BENCH-AFTER kernel: after ExitBootServices the loader enters the kernel, or halts after a failed call";
const AFTER_FADT: &str = "origin-boot: bench build: BENCH-AFTER fadt: after ExitBootServices the loader resets the machine through the FADT's reset register, without calling ResetSystem";
const AFTER_DISTURB: &str = "origin-boot: bench build: BENCH-AFTER disturb: after ExitBootServices the loader turns the UART's loopback on and changes its divisor, then writes the firmware's values back, then resets the machine";
/// On the loader's console, before the call: how the reset will go, the FADT's register, and
/// the UART as the firmware left it.
const RESET_PLAN: &str = "origin-boot: bench build: ResetSystem is at 0x";
const RESET_REGISTER: &str = "origin-boot: bench build: the FADT's reset register: write ";
const UNDER_FIRMWARE: &str =
    "origin-boot: bench build: the UART at I/O port 0x2f8 under the firmware: ";
/// Straight to the SPCR's UART: before the call, the marks before each call, after it, and the
/// reset's lines.
const BEFORE_CALL: &str =
    "origin-boot: bench build: this line went straight to the UART, before ExitBootServices";
const MARK_FIRST: &str = "X";
const MARK_RETRY: &str = "Y";
const AFTER_CALL: &str = "origin-boot: bench build: after ExitBootServices (";
/// How the line after the call begins, for each way the calls ended in the scenarios.
const AFTER_FIRST: &str =
    "origin-boot: bench build: after ExitBootServices (first call EFI_SUCCESS) the UART read ";
const AFTER_RETRY: &str = "origin-boot: bench build: after ExitBootServices (first call EFI_INVALID_PARAMETER, retry EFI_SUCCESS) the UART read ";
const AFTER_BOTH_FAILED: &str = "origin-boot: bench build: after ExitBootServices (first call EFI_INVALID_PARAMETER, retry EFI_INVALID_PARAMETER) the UART read ";
const WRITTEN_BACK: &str = "; the firmware's values are written back";
const RESET_SOON: &str = "origin-boot: bench build: a cold reset through the firmware's ResetSystem in 5 s, and the FADT's reset register 2 s later if the machine still runs";
const FADT_SOON: &str = "origin-boot: bench build: a reset through the FADT's reset register in 7 s: ResetSystem is not called";
const FADT_WRITE: &str =
    "origin-boot: bench build: ResetSystem was not called (BENCH-AFTER fadt): writing ";
/// How long a reset scenario waits, after its last marker, for QEMU to exit: the loader's 5 s or
/// 7 s, and room for a slow host.
const RESET_WITHIN: Duration = Duration::from_secs(30);

// TASKS item 121: the FCH's lines on the loader's console, and how long QEMU took to exit.
const NO_FCH: &str = "origin-boot: bench build: no BENCH-FCH: the FCH's registers are read where the firmware's tables allow it";
const FCH_SWITCH: &str = "origin-boot: bench build: BENCH-FCH: the FCH's registers are not read; the switch's values stand in for them";
const FCH_WHERE: &str = "origin-boot: bench build: the FCH's power-management block at 0xfed80300 (AMD's documents; TASKS item 121): ";
/// QEMU's DSDT describes no region there, on an AMD processor; or the processor is not AMD's.
const FCH_QEMU_NOT_READ: [&str; 2] = [
    "not read, the DSDT describes no system-memory region with constant bounds that holds it",
    "not read, the processor is not AMD's",
];
const FCH_BY_SWITCH: &str = "not read; BENCH-FCH's values stand in";
const FCH_REGISTERS: &str = "origin-boot: bench build: the FCH's registers (BENCH-FCH's): ";
const FCH_NO_CONTROL: &str = "origin-boot: bench build: no FCH control: ExitBootServices is called, and Amendment 3's path follows it";
const FCH_CONTROL: &str = "origin-boot: bench build: the last reset was not a 0x06 to 0xcf9 (the reset pin (bit 16)): the boot's first run, the control: 0x06 to 0xcf9 now, and ExitBootServices is not called";
const FCH_AFTER_CALL: &str = "origin-boot: bench build: the last reset was a 0x06 to 0xcf9 (PMxC0 bit 19), as the control writes: ExitBootServices is called; when a call returns EFI_SUCCESS, 0x06 to 0xcf9 is the first thing after it, then a triple fault";
const FCH_NO_RECORD: &str = "origin-boot: bench build: PMxC0 0x00000800 names no reset cause: no record of the last reset, and no control in this boot: ExitBootServices is called; when a call returns EFI_SUCCESS, 0x06 to 0xcf9 is the first thing after it, then a triple fault";
const FCH_CONTRADICTS: &str = "origin-boot: bench build: the FCH's registers contradict AMD's documents (PMx00 reads all ones): no control; ExitBootServices is called, and Amendment 3's path follows it";
const FCH_CONTROL_FAILED: &str = "origin-boot: bench build: 0xcf9 did not reset the machine within 100 ms: ExitBootServices is called, and Amendment 3's path follows it";
/// A reset through `0xCF9` ends QEMU at once; the triple fault comes after `slow`'s 5 s wait.
const CF9_AT_ONCE: Duration = Duration::from_millis(2_500);

// TASKS item 128: the minimal mode's lines (ADR-0002 Amendment 5) and Amendment 4's addition's.
const MIN_START: &str =
    "origin-boot: bench build: minimal: the minimal-program mode (ADR-0002 Amendment 5)";
const MIN_STAND_IN: &str = "origin-boot: bench build: minimal: the stand-in table (OXFC): the FCH's registers are not read; its values stand in for them (QEMU only)";
const MIN_CONTROL: &str = "origin-boot: bench build: minimal: the last reset was not a shutdown (the reset pin (bit 16)): the boot's first run, the control: a triple fault now, and ExitBootServices is not called";
const MIN_MINE: &str = "origin-boot: bench build: minimal: the last reset was a shutdown (PMxC0 bit 23), as the control's triple fault records: ExitBootServices is called through the live boot services table; after EFI_SUCCESS, a triple fault and nothing before it";
const MIN_NO_RECORD: &str = "origin-boot: bench build: minimal: PMxC0 0x00000800 names no reset cause: no record of the last reset, and no control in this boot: ExitBootServices is called through the live boot services table; after EFI_SUCCESS, a triple fault and nothing before it";
const MIN_NO_RECORD_QUIET: &str = "origin-boot: bench build: minimal: PMxC0 0x00000800 names no reset cause: no record of the last reset, and no control in this boot: ExitBootServices is called through the live boot services table; after EFI_SUCCESS, Amendment 3's order, since no shutdown is known to reset this system";
const MIN_LAST: &str = "origin-boot: bench build: minimal: ExitBootServices next, through the live boot services table";
const MIN_HALT_SOON: &str = "origin-boot: bench build: no reset: ResetSystem is not called and the FADT gives no usable reset register; a halt in 7 s";
const MIN_CF9_SKIPPED: &str = "origin-boot: bench build: ResetSystem was not called (the stand-in table's noreset), and the FADT gives no usable reset register (RESET_REG is I/O port 0xcf9, which the minimal mode never writes (ADR-0002 Amendment 5)): halting";
const SMI_LINE: &str = "origin-boot: bench build: the FCH's SMI control for a write to 0xCF9 (SMIxC0 at 0xfed802c0, bits 27:26; ADR-0002 Amendment 4's addition): ";
const SMI_GIVEN: &str = "origin-boot: bench build: the FCH's SMI control for a write to 0xCF9 (SMIxC0 at 0xfed802c0, bits 27:26; ADR-0002 Amendment 4's addition): 0x04000000 (a stand-in's): a write of bit 2 to 0xCF9 raises an SMI, so the firmware's SMM code handles it";
const TABLE_LINE: &str = "origin-boot: bench build: the boot services table (ADR-0002 Amendment 4's addition): the system table names it at ";
const AFTER_THE_WAIT: Duration = Duration::from_secs(4);

/// The SPCR naming COM2, as `boot-console`'s scenario gives it.
pub(crate) fn spcr(work: &Path) -> Result<Vec<String>> {
    crate::spcr_args(
        work,
        &crate::spcr_table(0, crate::gas_io(SPCR_PORT), 7),
        &[],
    )
}

/// Writes the switch `name` with `contents` on the ESP `stage` has just laid out.
fn write_switch(work: &Path, name: &str, contents: &[u8]) -> Result {
    let path = work.join("esp").join("EFI").join("ORIGIN").join(name);
    fs::write(&path, contents).map_err(|e| format!("{}: {e}", path.display()))
}

/// `BENCH-STALE-KEY` with `contents`.
fn switch(work: &Path, contents: &[u8]) -> Result {
    write_switch(work, "BENCH-STALE-KEY", contents)
}

/// `BENCH-AFTER` with `mode` (TASKS item 116).
fn after(work: &Path, mode: &[u8]) -> Result {
    write_switch(work, "BENCH-AFTER", mode)
}

/// TASKS item 112's path: the kernel after a successful call.
fn spcr_kernel(work: &Path) -> Result<Vec<String>> {
    after(work, b"kernel\n")?;
    spcr(work)
}

fn stale_once(work: &Path) -> Result<Vec<String>> {
    switch(work, b"1\n")?;
    spcr_kernel(work)
}

fn stale_twice(work: &Path) -> Result<Vec<String>> {
    switch(work, b"2\n")?;
    spcr_kernel(work)
}

/// The FADT's register, with `ResetSystem` not called.
fn spcr_fadt(work: &Path) -> Result<Vec<String>> {
    after(work, b"fadt\n")?;
    spcr(work)
}

/// The UART disturbed as the call returns.
fn spcr_disturb(work: &Path) -> Result<Vec<String>> {
    after(work, b"disturb\n")?;
    spcr(work)
}

/// `BENCH-FCH` with `words` (TASKS item 121).
fn fch(work: &Path, words: &[u8]) -> Result<Vec<String>> {
    write_switch(work, "BENCH-FCH", words)?;
    spcr(work)
}

fn fch_first(work: &Path) -> Result<Vec<String>> {
    fch(work, b"first\n")
}

pub(crate) fn fch_mine(work: &Path) -> Result<Vec<String>> {
    fch(work, b"mine slow\n")
}

fn fch_mine_nocf9(work: &Path) -> Result<Vec<String>> {
    fch(work, b"mine nocf9 slow\n")
}

fn fch_mine_neither(work: &Path) -> Result<Vec<String>> {
    fch(work, b"mine nocf9 noshutdown slow\n")
}

fn fch_no_record(work: &Path) -> Result<Vec<String>> {
    fch(work, b"norecord slow\n")
}

/// `BENCH-FCH` with `mine`, and a stale key for the first call, or for both.
fn fch_mine_stale(work: &Path, calls: &[u8]) -> Result<Vec<String>> {
    switch(work, calls)?;
    fch_mine(work)
}

fn fch_mine_stale_once(work: &Path) -> Result<Vec<String>> {
    fch_mine_stale(work, b"1\n")
}

fn fch_mine_stale_twice(work: &Path) -> Result<Vec<String>> {
    fch_mine_stale(work, b"2\n")
}

fn fch_bad(work: &Path) -> Result<Vec<String>> {
    fch(work, b"bad\n")
}

fn fch_first_nocf9(work: &Path) -> Result<Vec<String>> {
    fch(work, b"first nocf9\n")
}

/// An ACPI table with the minimal mode's stand-in signature (TASKS item 128) and `words` as its
/// body: the stand-in values under QEMU, which has no FCH. Only `xtask` plants it.
fn stand_in_table(words: &[u8]) -> Vec<u8> {
    let len = 36 + words.len();
    let mut table = vec![0u8; len];
    table[..4].copy_from_slice(b"OXFC");
    table[4..8].copy_from_slice(&u32::try_from(len).unwrap_or(u32::MAX).to_le_bytes());
    table[8] = 1;
    table[10..16].copy_from_slice(b"ORIGIN");
    table[16..24].copy_from_slice(b"ORIGOXFC");
    table[24..28].copy_from_slice(&1u32.to_le_bytes());
    table[28..32].copy_from_slice(b"ORIG");
    table[32..36].copy_from_slice(&1u32.to_le_bytes());
    table[36..].copy_from_slice(words);
    let sum = table.iter().fold(0u8, |s, &b| s.wrapping_add(b));
    table[9] = 0u8.wrapping_sub(sum);
    table
}

/// The SPCR scenario's table, and the stand-in table with `words`.
fn stand_in(work: &Path, words: &[u8]) -> Result<Vec<String>> {
    let path = work.join("oxfc.bin");
    fs::write(&path, stand_in_table(words)).map_err(|e| format!("{}: {e}", path.display()))?;
    let table = crate::qemu_path_arg("file=", &path)?;
    crate::spcr_args(
        work,
        &crate::spcr_table(0, crate::gas_io(SPCR_PORT), 7),
        &["-acpitable", &table],
    )
}

fn minimal_first(work: &Path) -> Result<Vec<String>> {
    stand_in(work, b"first")
}

fn minimal_mine(work: &Path) -> Result<Vec<String>> {
    stand_in(work, b"mine")
}

fn minimal_no_record(work: &Path) -> Result<Vec<String>> {
    stand_in(work, b"norecord")
}

fn minimal_stale_once(work: &Path) -> Result<Vec<String>> {
    stand_in(work, b"mine stale1")
}

fn minimal_fadt_skipped(work: &Path) -> Result<Vec<String>> {
    stand_in(work, b"norecord noshutdown noreset")
}

/// The SPCR scenario with the stand-in table planted, which the full mode must ignore (the
/// coordinating session's ruling Q4).
fn spcr_stand_in_ignored(work: &Path) -> Result<Vec<String>> {
    stand_in(work, b"first")
}

/// The kernel's markers on the SPCR's UART, as `boot-console`'s SPCR scenario has them, with the
/// bench kernel's line after the line naming the UART.
fn kernel_markers(said: &str) -> Vec<String> {
    console_markers(&[
        "origin-kernel: console: framebuffer ",
        KERNEL_WRITES_COM2,
        said,
        TAKEN_OVER_LINE,
        TAKEN_BYTES,
    ])
}

fn scenario(
    label: &'static str,
    loader: Loader,
    markers: Vec<String>,
    qemu_args: QemuArgs,
    check: crate::Check,
    quiet: Option<Duration>,
) -> Scenario {
    Scenario {
        quiet,
        label,
        loader,
        markers,
        failures: qemu_boot_failures(),
        check: Some(check),
        stderr: vec![],
        capture: Capture::SpcrUart,
        qemu_args: Some(qemu_args),
    }
}

/// The sixteen scenarios of [`boot`]: TASKS item 112's four, TASKS item 116's three, and TASKS
/// item 121's nine.
fn scenarios() -> Vec<Scenario> {
    let with_crumbs = |first: &str| {
        let mut m = vec![
            first.to_string(),
            AFTER_CALL.to_string(),
            CRUMBS.to_string(),
        ];
        m.extend(kernel_markers(SAID_PORT));
        m
    };
    let lines = |lines: &[&str]| lines.iter().map(ToString::to_string).collect::<Vec<_>>();
    vec![
        scenario(
            "bench: the firmware's key",
            Loader::Bench,
            with_crumbs(FIRST_CALL),
            spcr_kernel,
            check_first_call,
            None,
        ),
        scenario(
            "bench: a stale key once, the retry",
            Loader::Bench,
            with_crumbs(RETRY),
            stale_once,
            check_retry,
            None,
        ),
        scenario(
            "bench: a stale key twice, the halt",
            Loader::Bench,
            lines(&[HALTING_START, AFTER_CALL]),
            stale_twice,
            check_halt,
            Some(QUIET),
        ),
        scenario(
            "bench: no switch, the reset through ResetSystem, the stand-in table ignored",
            Loader::Bench,
            lines(&[FIRST_CALL, AFTER_CALL, RESET_SOON]),
            spcr_stand_in_ignored,
            check_reset_system,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: ResetSystem not called, the reset through the FADT's register",
            Loader::Bench,
            lines(&[FIRST_CALL, AFTER_CALL, FADT_SOON, FADT_WRITE]),
            spcr_fadt,
            check_reset_fadt,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: a UART disturbed as the call returns, written back",
            Loader::Bench,
            lines(&[AFTER_CALL, RESET_SOON]),
            spcr_disturb,
            check_disturbed,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: the bench kernel under the normal loader",
            Loader::Developer,
            kernel_markers(SAID_NONE),
            spcr,
            check_normal_loader,
            None,
        ),
        scenario(
            "bench: FCH first, the control through 0xCF9 before the call",
            Loader::Bench,
            lines(&[FCH_REGISTERS, FCH_CONTROL]),
            fch_first,
            check_fch_control,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: FCH mine, 0xCF9 at once after the call",
            Loader::Bench,
            lines(&[FCH_AFTER_CALL, BEFORE_CALL, LAST_LOADER_LINE]),
            fch_mine,
            check_fch_cf9,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: FCH mine nocf9, the triple fault after the wait",
            Loader::Bench,
            lines(&[FCH_AFTER_CALL, BEFORE_CALL, LAST_LOADER_LINE]),
            fch_mine_nocf9,
            check_fch_triple_fault,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: FCH mine nocf9 noshutdown, Amendment 3's order after them",
            Loader::Bench,
            lines(&[FCH_AFTER_CALL, FIRST_CALL, AFTER_CALL, RESET_SOON]),
            fch_mine_neither,
            check_fch_fall_through,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: FCH norecord, no control, 0xCF9 at once after the call",
            Loader::Bench,
            lines(&[FCH_NO_RECORD, BEFORE_CALL, LAST_LOADER_LINE]),
            fch_no_record,
            check_fch_no_record,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: FCH mine with a stale key once, 0xCF9 only after the retry succeeds",
            Loader::Bench,
            lines(&[FCH_AFTER_CALL, BEFORE_CALL, LAST_LOADER_LINE]),
            fch_mine_stale_once,
            check_fch_after_retry,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: FCH mine with a stale key twice, the normal path after both calls fail",
            Loader::Bench,
            lines(&[FCH_AFTER_CALL, HALTING_START, AFTER_CALL, RESET_SOON]),
            fch_mine_stale_twice,
            check_fch_both_failed,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: FCH bad, a layout that contradicts AMD's documents",
            Loader::Bench,
            lines(&[FCH_CONTRADICTS, FIRST_CALL, AFTER_CALL, RESET_SOON]),
            fch_bad,
            check_fch_contradicts,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: FCH first nocf9, a control that does not reset",
            Loader::Bench,
            lines(&[
                FCH_CONTROL,
                FCH_CONTROL_FAILED,
                FIRST_CALL,
                AFTER_CALL,
                RESET_SOON,
            ]),
            fch_first_nocf9,
            check_fch_control_failed,
            Some(RESET_WITHIN),
        ),
        // TASKS item 128 (ADR-0002 Amendment 5): the minimal mode.
        scenario(
            "bench: minimal, the control, a triple fault before the call",
            Loader::BenchMinimal,
            lines(&[MIN_START, MIN_CONTROL]),
            minimal_first,
            check_minimal_control,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: minimal, a returning call, the triple fault at once",
            Loader::BenchMinimal,
            lines(&[MIN_MINE, MIN_LAST, MARK_FIRST]),
            minimal_mine,
            check_minimal_mine,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: minimal, no record, no control, the triple fault at once",
            Loader::BenchMinimal,
            lines(&[MIN_NO_RECORD, MIN_LAST, MARK_FIRST]),
            minimal_no_record,
            check_minimal_no_record,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: minimal, a stale key once, the triple fault only after the retry succeeds",
            Loader::BenchMinimal,
            lines(&[MIN_MINE, MIN_LAST, MARK_FIRST, MARK_RETRY]),
            minimal_stale_once,
            check_minimal_retry,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: minimal, Amendment 3's order, the FADT's register at 0xCF9 skipped",
            Loader::BenchMinimal,
            lines(&[
                MIN_NO_RECORD_QUIET,
                MIN_LAST,
                MIN_HALT_SOON,
                MIN_CF9_SKIPPED,
            ]),
            minimal_fadt_skipped,
            check_minimal_fadt_skipped,
            Some(QUIET),
        ),
    ]
}

/// The loader's console (COM1) holds `switch_line`, `after_line`, the state line,
/// `LOADER_NAMES_COM2`, and TASKS item 116's lines about the reset and the UART, all before its
/// last line; and the line straight to the UART before the call is on the SPCR's UART only, before
/// the loader's last line there.
fn check_loader_console(done: &Completed, switch_line: &str, after_line: &str) -> Result {
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    let at = |needle: &str| com1.find(needle);
    let last = at(LAST_LOADER_LINE).ok_or("COM1 lacks the loader's last line")?;
    for needle in [
        switch_line,
        after_line,
        STATE,
        LOADER_NAMES_COM2,
        RESET_PLAN,
        RESET_REGISTER,
        UNDER_FIRMWARE,
        SMI_LINE,
        TABLE_LINE,
    ] {
        match at(needle) {
            Some(i) if i < last => {}
            _ => {
                return Err(format!(
                    "COM1, the bench loader's console, lacks {needle:?} before its last line"
                ));
            }
        }
    }
    check_fch_lines(&com1, last)?;
    let state = com1
        .lines()
        .find(|l| l.starts_with(STATE))
        .unwrap_or_default();
    if !(state.contains(", EFER 0x") && state.contains("; shadow stacks: ")) {
        return Err(format!("the state line is not whole: {state:?}"));
    }
    if com1.contains(BEFORE_CALL) {
        return Err(format!(
            "COM1, the firmware's console, holds {BEFORE_CALL:?}, which goes straight to the SPCR's UART only"
        ));
    }
    let uart = serial::clean(done.serial.as_bytes());
    match (uart.find(BEFORE_CALL), uart.find(LAST_LOADER_LINE)) {
        (Some(before), Some(last)) if before < last => Ok(()),
        _ => Err(format!(
            "the SPCR's UART lacks {BEFORE_CALL:?} before the loader's last line"
        )),
    }
}

/// The lines on the SPCR's UART after the loader's last line, empty ones left out.
fn after_last_line(done: &Completed) -> Result<Vec<String>> {
    let uart = serial::clean(done.serial.as_bytes());
    let lines: Vec<&str> = uart.lines().filter(|l| !l.is_empty()).collect();
    let last = lines
        .iter()
        .position(|l| *l == LAST_LOADER_LINE)
        .ok_or("the SPCR's UART lacks the loader's last line")?;
    Ok(lines[last + 1..].iter().map(ToString::to_string).collect())
}

/// Whether `line` is the line after the call that begins `start`, with the firmware's values
/// written back.
fn is_after_call(line: &str, start: &str) -> bool {
    line.starts_with(start) && line.ends_with(WRITTEN_BACK)
}

/// The value after `name` in a register line: `"MCR 0x1b"` gives 0x1b.
fn register(line: &str, name: &str) -> Option<u32> {
    let rest = line.split(&format!("{name} 0x")).nth(1)?;
    let hex: String = rest.chars().take_while(char::is_ascii_hexdigit).collect();
    u32::from_str_radix(&hex, 16).ok()
}

/// After the loader's last line, on the SPCR's UART: exactly `want`, each a line that starts with
/// it; and QEMU exited by itself (`reset`) or did not.
fn check_after(done: &Completed, want: &[&str], reset: bool) -> Result {
    let after = after_last_line(done)?;
    let fits = after.len() == want.len()
        && after.iter().zip(want).all(|(line, w)| {
            if w.starts_with(AFTER_CALL) {
                is_after_call(line, w)
            } else {
                line.starts_with(w)
            }
        });
    if !fits {
        return Err(format!(
            "after the loader's last line the SPCR's UART should hold lines starting {want:?}; it holds {after:?}"
        ));
    }
    if done.exited != reset {
        return Err(if reset {
            format!(
                "QEMU did not exit within {} s of the reset line, with a success status: the machine was not reset",
                RESET_WITHIN.as_secs()
            )
        } else {
            "QEMU exited, though nothing should have reset the machine".into()
        });
    }
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    if reset && (done.serial.contains("origin-kernel:") || com1.contains("origin-kernel:")) {
        return Err("a kernel line was written, though the loader resets the machine".into());
    }
    Ok(())
}

/// On the SPCR's UART, `first` is followed by the breadcrumbs on a line of their own and then by
/// the kernel's first line, with no kernel line before it; and `boot-console`'s SPCR checks hold.
/// (OVMF writes its console, and so the loader's earlier lines, to every serial port: COM2 holds
/// those first.)
fn check_crumbs(
    done: &Completed,
    settings: &Settings,
    first: &str,
    marks: &[&str],
    after_call: &str,
) -> Result {
    let uart = serial::clean(done.serial.as_bytes());
    let lines: Vec<&str> = uart.lines().filter(|l| !l.is_empty()).collect();
    let at = lines
        .iter()
        .position(|l| *l == first)
        .ok_or_else(|| format!("the SPCR's UART lacks the bench loader's line {first:?}"))?;
    // TASKS item 116: the marks before each call, after the loader's last line.
    let last = lines.iter().position(|l| *l == LAST_LOADER_LINE);
    if last.and_then(|last| lines.get(last + 1..at)) != Some(marks) {
        return Err(format!(
            "on the SPCR's UART, between the loader's last line and {first:?}, the marks should be {marks:?}"
        ));
    }
    let next = lines.get(at + 1..(at + 4).min(lines.len())).unwrap_or(&[]);
    if !next.first().is_some_and(|l| is_after_call(l, after_call))
        || next.get(1) != Some(&CRUMBS)
        || !next
            .get(2)
            .is_some_and(|l| l.starts_with("origin-kernel: console: framebuffer "))
    {
        return Err(format!(
            "on the SPCR's UART the loader's line {first:?} should be followed by the line after the call, {CRUMBS:?} on its own, then the kernel's first line; it is followed by {next:?}"
        ));
    }
    if let Some(early) = lines[..at].iter().find(|l| l.starts_with("origin-kernel:")) {
        return Err(format!(
            "the SPCR's UART holds a kernel line before the loader's: {early:?}"
        ));
    }
    check_spcr_console(done, settings)
}

fn check_first_call(done: &Completed, settings: &Settings) -> Result {
    check_loader_console(done, NO_SWITCH, AFTER_KERNEL)?;
    check_crumbs(done, settings, FIRST_CALL, &[MARK_FIRST], AFTER_FIRST)
}

fn check_retry(done: &Completed, settings: &Settings) -> Result {
    check_loader_console(done, SWITCH_ONCE, AFTER_KERNEL)?;
    check_crumbs(
        done,
        settings,
        RETRY,
        &[MARK_FIRST, MARK_RETRY],
        AFTER_RETRY,
    )
}

/// Both calls failed: on the SPCR's UART the marks, the halting line and the line after the call
/// follow the loader's last line and, [`QUIET`] later, nothing follows them; QEMU did not exit;
/// and no kernel line was written anywhere.
fn check_halt(done: &Completed, _settings: &Settings) -> Result {
    check_loader_console(done, SWITCH_TWICE, AFTER_KERNEL)?;
    let after = after_last_line(done)?;
    if after.len() != 4
        || after[..3] != [MARK_FIRST, MARK_RETRY, HALTING]
        || !is_after_call(&after[3], AFTER_BOTH_FAILED)
    {
        return Err(format!(
            "after both calls failed, the SPCR's UART should hold the marks, the halting line and the line after the call after the loader's last line, and nothing more for {} s; it holds {after:?}",
            QUIET.as_secs()
        ));
    }
    if done.exited {
        return Err("QEMU exited, though the loader halts".into());
    }
    let uart = serial::clean(done.serial.as_bytes());
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    if uart.contains("origin-kernel:") || com1.contains("origin-kernel:") {
        return Err("a kernel line was written, though the loader halted".into());
    }
    Ok(())
}

/// No switch, as a rental runs: the mark, the line after the call, the reset line, and QEMU
/// exiting within [`RESET_WITHIN`], with nothing after the reset line: `ResetSystem` reset the
/// machine before the FADT's register was reached.
fn check_reset_system(done: &Completed, _settings: &Settings) -> Result {
    check_loader_console(done, NO_SWITCH, NO_AFTER)?;
    // The stand-in table is planted, and the full mode ignores it (ruling Q4): its FCH's lines are
    // those of a QEMU boot with no BENCH-FCH (`check_fch_lines`), and none names the table.
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    if com1.contains("stand-in table") || com1.contains("bench build: minimal") {
        return Err("the full bench loader took the stand-in table, which only the minimal mode reads (TASKS item 128, ruling Q4)".into());
    }
    let table = com1
        .lines()
        .find(|l| l.starts_with(TABLE_LINE))
        .unwrap_or_default();
    if !table.contains("; the same as the loader's copy") && !table.contains("; it differs from") {
        return Err(format!(
            "the boot services table's line does not say how the live table compares: {table:?}"
        ));
    }
    check_after(
        done,
        &[MARK_FIRST, FIRST_CALL, AFTER_FIRST, RESET_SOON],
        true,
    )
}

/// `ResetSystem` not called: the FADT's register written, and QEMU exiting.
fn check_reset_fadt(done: &Completed, _settings: &Settings) -> Result {
    check_loader_console(done, NO_SWITCH, AFTER_FADT)?;
    check_after(
        done,
        &[MARK_FIRST, FIRST_CALL, AFTER_FIRST, FADT_SOON, FADT_WRITE],
        true,
    )
}

/// The UART disturbed as the call returns: the line written as it then is never reaches the
/// line (loopback), the line after the call names the loopback and the changed divisor, so the
/// firmware's values were written back before it, and the reset follows.
fn check_disturbed(done: &Completed, _settings: &Settings) -> Result {
    check_loader_console(done, NO_SWITCH, AFTER_DISTURB)?;
    check_after(done, &[MARK_FIRST, AFTER_FIRST, RESET_SOON], true)?;
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    let under = com1
        .lines()
        .find(|l| l.starts_with(UNDER_FIRMWARE))
        .and_then(|l| register(l, "divisor"))
        .ok_or("the loader's console lacks the UART's divisor under the firmware")?;
    let after = after_last_line(done)?;
    let line = after.get(1).map_or("", String::as_str);
    let (mcr, divisor) = (register(line, "MCR"), register(line, "divisor"));
    if !mcr.is_some_and(|m| m & 0x10 != 0) || divisor == Some(under) || divisor.is_none() {
        return Err(format!(
            "the line after the call should name the loopback and a divisor other than the firmware's {under:#06x}: {line:?}"
        ));
    }
    Ok(())
}

/// TASKS item 121's lines on the loader's console, before `before` (a byte offset in `com1`): with
/// no `BENCH-FCH`, the line saying so, the FCH's block not read under QEMU (no region in its DSDT,
/// or not an AMD processor), and no control; with it, the switch's values and the registers'
/// line.
fn check_fch_lines(com1: &str, before: usize) -> Result {
    let line_at = |start: &str| {
        com1.find(start)
            .filter(|&i| i < before)
            .map(|i| com1[i..].lines().next().unwrap_or_default())
    };
    let where_line = line_at(FCH_WHERE)
        .ok_or_else(|| format!("COM1 lacks {FCH_WHERE:?} before the loader's last line"))?;
    let tail = &where_line[FCH_WHERE.len()..];
    let wanted: &[&str] = if line_at(FCH_SWITCH).is_some() {
        if tail != FCH_BY_SWITCH {
            return Err(format!(
                "with BENCH-FCH the FCH's line should say {FCH_BY_SWITCH:?}: {where_line:?}"
            ));
        }
        &[FCH_REGISTERS]
    } else {
        if !FCH_QEMU_NOT_READ.contains(&tail) {
            return Err(format!(
                "under QEMU, with no BENCH-FCH, the FCH's block is not read: {where_line:?}"
            ));
        }
        &[NO_FCH, FCH_NO_CONTROL]
    };
    for needle in wanted {
        if line_at(needle).is_none() {
            return Err(format!(
                "COM1 lacks {needle:?} before the loader's last line"
            ));
        }
    }
    Ok(())
}

/// The registers' line and `decision` on COM1, the loader's console; and QEMU exited by itself.
fn check_fch_decided(done: &Completed, decision: &str) -> Result {
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    for needle in [FCH_SWITCH, FCH_REGISTERS, SMI_GIVEN, decision] {
        if !com1.contains(needle) {
            return Err(format!(
                "COM1, the bench loader's console, lacks {needle:?}"
            ));
        }
    }
    if !done.exited {
        return Err(format!(
            "QEMU did not exit within {} s, with a success status: the machine was not reset",
            RESET_WITHIN.as_secs()
        ));
    }
    Ok(())
}

/// The boot's first run: the control line is the loader's last, and QEMU exits. The call is never
/// made: no line straight to the UART, no mark, no last line.
fn check_fch_control(done: &Completed, _settings: &Settings) -> Result {
    check_fch_decided(done, FCH_CONTROL)?;
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    let uart = serial::clean(done.serial.as_bytes());
    if com1.contains(FCH_CONTROL_FAILED) || com1.contains(LAST_LOADER_LINE) {
        return Err("the loader went on after the control: 0xCF9 did not reset the machine".into());
    }
    if uart.contains(BEFORE_CALL) || uart.lines().any(|l| l == MARK_FIRST) {
        return Err("the line before the call or its mark was written: the call was made".into());
    }
    let last = com1.lines().rev().find(|l| l.starts_with("origin-boot:"));
    if last != Some(FCH_CONTROL) {
        return Err(format!(
            "the loader's last line should be the control's: {last:?}"
        ));
    }
    Ok(())
}

/// A run after a `0x06` to `0xCF9`: the mark, then nothing, and QEMU exits at once: `0xCF9`,
/// before the triple fault's wait.
fn check_fch_cf9(done: &Completed, _settings: &Settings) -> Result {
    check_cf9_after(done, FCH_AFTER_CALL, &[MARK_FIRST])
}

/// `decision` on the loader's console; after its last line, on the SPCR's UART, `marks` and
/// nothing more; and QEMU exits within [`CF9_AT_ONCE`] of the last mark: `0xCF9`, before the
/// triple fault's wait.
fn check_cf9_after(done: &Completed, decision: &str, marks: &[&str]) -> Result {
    check_fch_decided(done, decision)?;
    check_after(done, marks, true)?;
    match done.quiet_before_exit {
        Some(quiet) if quiet < CF9_AT_ONCE => Ok(()),
        other => Err(format!(
            "QEMU should exit within {} ms of the last mark, by 0xCF9; it took {other:?}",
            CF9_AT_ONCE.as_millis()
        )),
    }
}

/// No record of the last reset: no control, the call, and `0xCF9` at once after it (the
/// coordinating session's ruling on R121-2).
fn check_fch_no_record(done: &Completed, _settings: &Settings) -> Result {
    check_cf9_after(done, FCH_NO_RECORD, &[MARK_FIRST])?;
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    if com1.contains(FCH_CONTROL_FAILED) || com1.contains("the boot's first run, the control") {
        return Err("a run with no record of the last reset ran the control".into());
    }
    Ok(())
}

/// The first call fails: no `0xCF9` after it, the retry's mark, and `0xCF9` at once after the
/// retry succeeds (the coordinating session's ruling on R121-3).
fn check_fch_after_retry(done: &Completed, _settings: &Settings) -> Result {
    check_cf9_after(done, FCH_AFTER_CALL, &[MARK_FIRST, MARK_RETRY])
}

/// Both calls fail: no `0xCF9` after either, then the loader's normal path: Amendment 2's line,
/// the line after the calls, and the reset through `ResetSystem`.
fn check_fch_both_failed(done: &Completed, _settings: &Settings) -> Result {
    check_fch_decided(done, FCH_AFTER_CALL)?;
    check_after(
        done,
        &[
            MARK_FIRST,
            MARK_RETRY,
            HALTING,
            AFTER_BOTH_FAILED,
            RESET_SOON,
        ],
        true,
    )
}

/// With no `0xCF9` write: the mark, then nothing, and QEMU exits only after the wait: the triple
/// fault.
fn check_fch_triple_fault(done: &Completed, _settings: &Settings) -> Result {
    check_fch_decided(done, FCH_AFTER_CALL)?;
    check_after(done, &[MARK_FIRST], true)?;
    match done.quiet_before_exit {
        Some(quiet) if quiet >= AFTER_THE_WAIT => Ok(()),
        other => Err(format!(
            "QEMU should exit {} s or more after the mark, by the triple fault after the wait; it took {other:?}",
            AFTER_THE_WAIT.as_secs()
        )),
    }
}

/// With neither: the mark, then TASKS item 116's path, and `ResetSystem` resets the machine.
fn check_fch_fall_through(done: &Completed, _settings: &Settings) -> Result {
    check_fch_decided(done, FCH_AFTER_CALL)?;
    check_after(
        done,
        &[MARK_FIRST, FIRST_CALL, AFTER_FIRST, RESET_SOON],
        true,
    )
}

/// A layout that contradicts the documents: no control, and TASKS item 116's path after the call.
fn check_fch_contradicts(done: &Completed, _settings: &Settings) -> Result {
    check_fch_decided(done, FCH_CONTRADICTS)?;
    check_after(
        done,
        &[MARK_FIRST, FIRST_CALL, AFTER_FIRST, RESET_SOON],
        true,
    )
}

/// A control whose `0xCF9` does not reset: its line, the line saying so, then the call and TASKS
/// item 116's path.
fn check_fch_control_failed(done: &Completed, _settings: &Settings) -> Result {
    check_fch_decided(done, FCH_CONTROL_FAILED)?;
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    match (com1.find(FCH_CONTROL), com1.find(FCH_CONTROL_FAILED)) {
        (Some(a), Some(b)) if a < b => {}
        _ => {
            return Err(
                "COM1 should hold the control's line, then the line saying it did not reset".into(),
            );
        }
    }
    check_after(
        done,
        &[MARK_FIRST, FIRST_CALL, AFTER_FIRST, RESET_SOON],
        true,
    )
}

/// A bench kernel under the normal loader: no breadcrumb anywhere, the line saying so, and the
/// boot otherwise as `boot-console`'s SPCR scenario requires.
/// The minimal mode's console (COM1): its own line, the stand-in table's, the SMI line given, and
/// `decision`; and none of the full path's lines: no switch file read, no kernel read or
/// verification, no generator, no page tables, no framebuffer, no table compare.
fn check_minimal_console(done: &Completed, decision: &str) -> Result {
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    for needle in [MIN_START, MIN_STAND_IN, SMI_GIVEN, decision] {
        if !com1.contains(needle) {
            return Err(format!(
                "COM1, the minimal-mode loader's console, lacks {needle:?}"
            ));
        }
    }
    for absent in [
        "BENCH-",
        "origin-boot: read \\EFI\\ORIGIN\\KERNEL.ONBF",
        "origin-boot: kernel verified",
        "origin-boot: random:",
        "origin-boot: page tables built",
        "origin-boot: framebuffer:",
        TABLE_LINE,
        LAST_LOADER_LINE,
    ] {
        if com1.contains(absent) {
            return Err(format!(
                "the minimal-mode loader wrote {absent:?}, which only the full path writes (ADR-0002 Amendment 5)"
            ));
        }
    }
    Ok(())
}

/// The lines on the SPCR's UART after the minimal mode's last line, empty ones left out.
fn after_minimal_last(done: &Completed) -> Result<Vec<String>> {
    let uart = serial::clean(done.serial.as_bytes());
    let lines: Vec<&str> = uart.lines().filter(|l| !l.is_empty()).collect();
    let last = lines
        .iter()
        .position(|l| *l == MIN_LAST)
        .ok_or("the SPCR's UART lacks the minimal mode's last line")?;
    Ok(lines[last + 1..].iter().map(ToString::to_string).collect())
}

/// The minimal mode's control: its line is the loader's last, and QEMU exits; no mark is written.
fn check_minimal_control(done: &Completed, _settings: &Settings) -> Result {
    check_minimal_console(done, MIN_CONTROL)?;
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    let last = com1.lines().rev().find(|l| l.starts_with("origin-boot:"));
    if last != Some(MIN_CONTROL) {
        return Err(format!(
            "the loader's last line should be the control's: {last:?}"
        ));
    }
    let uart = serial::clean(done.serial.as_bytes());
    if uart.contains(MIN_LAST) || uart.lines().any(|l| l == MARK_FIRST) {
        return Err(
            "the minimal mode's last line or its mark was written: the call was made".into(),
        );
    }
    if !done.exited {
        return Err(
            "QEMU did not exit after the control: the triple fault did not reset the machine"
                .into(),
        );
    }
    Ok(())
}

/// `decision` on the console; after the mode's last line, `marks` and nothing more; and QEMU exits
/// within [`CF9_AT_ONCE`] of the last mark: the triple fault, the first thing after the call.
fn check_minimal_call(done: &Completed, decision: &str, marks: &[&str]) -> Result {
    check_minimal_console(done, decision)?;
    let after = after_minimal_last(done)?;
    if after.iter().map(String::as_str).ne(marks.iter().copied()) {
        return Err(format!(
            "after the minimal mode's last line the SPCR's UART should hold {marks:?}; it holds {after:?}"
        ));
    }
    if !done.exited {
        return Err("QEMU did not exit after the call: no triple fault reset the machine".into());
    }
    match done.quiet_before_exit {
        Some(quiet) if quiet < CF9_AT_ONCE => Ok(()),
        other => Err(format!(
            "QEMU should exit within {} ms of the last mark, by the triple fault; it took {other:?}",
            CF9_AT_ONCE.as_millis()
        )),
    }
}

fn check_minimal_mine(done: &Completed, _settings: &Settings) -> Result {
    check_minimal_call(done, MIN_MINE, &[MARK_FIRST])
}

fn check_minimal_no_record(done: &Completed, _settings: &Settings) -> Result {
    check_minimal_call(done, MIN_NO_RECORD, &[MARK_FIRST])
}

fn check_minimal_retry(done: &Completed, _settings: &Settings) -> Result {
    check_minimal_call(done, MIN_MINE, &[MARK_FIRST, MARK_RETRY])
}

/// With the shutdown message off, a successful call is followed by Amendment 3's order with no
/// `ResetSystem` (`noreset`), whose FADT step the mode skips, since the register is port `0xCF9`
/// under QEMU (the coordinating session's addition to ruling Q3): the halt, and QEMU runs on.
fn check_minimal_fadt_skipped(done: &Completed, _settings: &Settings) -> Result {
    check_minimal_console(done, MIN_NO_RECORD_QUIET)?;
    let after = after_minimal_last(done)?;
    let want = [MARK_FIRST, MIN_HALT_SOON, MIN_CF9_SKIPPED];
    if after.iter().map(String::as_str).ne(want) {
        return Err(format!(
            "after the minimal mode's last line the SPCR's UART should hold {want:?}; it holds {after:?}"
        ));
    }
    if done.exited {
        return Err("QEMU exited: something reset the machine, though the mode skips the FADT's write to 0xCF9 and halts".into());
    }
    Ok(())
}

fn check_normal_loader(done: &Completed, settings: &Settings) -> Result {
    let uart = serial::clean(done.serial.as_bytes());
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    for (name, text) in [("the SPCR's UART", &uart), ("COM1", &com1)] {
        if let Some(line) = text.lines().find(|l| is_crumb_line(l)) {
            return Err(format!(
                "{name} holds breadcrumbs ({line:?}) though the normal loader handed over no port"
            ));
        }
        if text.contains("origin-boot: bench build:") {
            return Err(format!(
                "{name} holds a bench loader's line under the normal loader"
            ));
        }
        if text.lines().any(|l| l == MARK_FIRST || l == MARK_RETRY) {
            return Err(format!(
                "{name} holds a bench loader's mark before ExitBootServices under the normal loader"
            ));
        }
    }
    check_spcr_console(done, settings)
}

/// Whether `line` is made of breadcrumbs only.
fn is_crumb_line(line: &str) -> bool {
    !line.is_empty() && line.bytes().all(|b| CRUMBS.as_bytes().contains(&b))
}

/// `cargo xtask boot-bench`: Clippy over the bench code, the bench pair built, and the seven
/// scenarios booted (part of `boot-matrix`).
pub(crate) fn boot(settings: &Settings) -> Result {
    let started = Instant::now();
    clippy()?;
    let (loader, kernel) = build()?;
    check_reset_after_call(&loader)?;
    // TASKS item 128: the minimal-mode loader, with its machine-code checks.
    build_minimal()?;
    let kernel = read_file(&kernel)?;
    let all: Vec<(Scenario, Vec<u8>)> = scenarios()
        .into_iter()
        .map(|scenario| (scenario, kernel.clone()))
        .collect();
    // `ORIGIN_BOOT_TEST_ONLY` picks scenarios by label, as for `boot-console`.
    let chosen = crate::select_scenarios(&all, &settings.only)?;
    for (scenario, kernel) in &chosen {
        run_scenario(scenario, kernel, settings)?;
    }
    println!(
        "xtask: boot-bench passed ({} of {} scenarios: the firmware's key, a stale key once, twice, the bench kernel under the normal loader, the reset through ResetSystem, through the FADT's register, a disturbed UART written back, and the FCH's nine: the control, 0xCF9 after the call, the triple fault, the fall-through, no record, 0xCF9 only after the retry succeeds, the normal path after both calls fail, a contradicting layout, and a control that does not reset; and the minimal mode's five: the control, a returning call, no record, the retry, and Amendment 3's order with the FADT's write to 0xCF9 skipped) in {} s",
        chosen.len(),
        all.len(),
        started.elapsed().as_secs()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reset_after_the_call_is_found_only_with_reads_compares_and_branches_between() {
        let good = "140003e4e:\tcall\tqword ptr [rax]\n140003e50:\tmov\trcx, qword ptr [rsi + 0x18]\n140003e5d:\tje\t0x140003ebe <.text+0x2ebe>\n140003e6b:\tmov\tdx, 0xcf9\n140003e6f:\tmov\tal, 0x6\n140003e71:\tout\tdx, al\n";
        assert!(reset_after_call(good));
        // The two loads the other way round.
        assert!(reset_after_call(&good.replace(
            "mov\tdx, 0xcf9\n140003e6f:\tmov\tal, 0x6",
            "mov\tal, 0x6\n140003e6f:\tmov\tdx, 0xcf9"
        )));
        // A compare of the status and a branch on it (R121-3's ruling), and the listing's labels
        // and comments.
        assert!(reset_after_call(&format!(
            "0000000140003e00 <.text+0x2e00>:\n{}",
            good.replace(
                "je\t",
                "test\trax, rax\n140003e5a:\tjne\t0x140003ec0 <.text+0x2ec0> # status\n140003e5c:\tje\t"
            )
        )));
        // A port read, a direct call, a store, or any other instruction between.
        for between in [
            "in\tal, dx",
            "call\t0x140017630 <.text+0x16630>",
            "mov\tqword ptr [rsp + 0x8], rax",
            "push\trax",
            "xor\tecx, ecx",
            "jmp\trax",
        ] {
            let listing = good.replace("je\t", &format!("{between}\n140003e5c:\tje\t"));
            assert!(!reset_after_call(&listing), "{between}");
        }
        // No indirect call before it, or another value or port.
        assert!(!reset_after_call(
            &good.replace("call\tqword ptr [rax]", "nop")
        ));
        assert!(!reset_after_call(&good.replace("al, 0x6", "al, 0xe")));
        assert!(!reset_after_call(&good.replace("dx, 0xcf9", "dx, 0x2f8")));
        // DX set, then changed before the write.
        assert!(!reset_after_call(&good.replace(
            "mov\tal, 0x6\n",
            "mov\tal, 0x6\n140003e70:\tmov\tedx, 0x2f8\n"
        )));
        assert!(!reset_after_call(""));
        // The reset out of line: reached by the branch, not by falling through, which returns.
        let out_of_line = "140003e4e:\tcall\tqword ptr [rax]\n140003e50:\ttest\trax, rax\n140003e53:\tje\t0x140003f00 <.text+0x2f00>\n140003e55:\tret\n140003f00:\tmov\tdx, 0xcf9\n140003f04:\tmov\tal, 0x6\n140003f06:\tout\tdx, al\n";
        assert!(reset_after_call(out_of_line));
        // A jump to itself ends.
        assert!(!reset_after_call(
            "1000:\tcall\tqword ptr [rax]\n1002:\tjmp\t0x1002 <.text>\n"
        ));
    }

    #[test]
    fn f1_takes_the_listed_files_and_refuses_others() {
        let listed: Vec<String> = FILES.iter().map(|(p, _)| (*p).to_string()).collect();
        assert_eq!(check_found(CFG, FILES, &listed), Ok(()));
        let mut more = listed.clone();
        more.push("framekernel/src/console.rs".into());
        let err = check_found(CFG, FILES, &more).unwrap_err();
        assert!(
            err.contains("framekernel/src/console.rs names the bench flag"),
            "{err}"
        );
        let fewer = &listed[1..];
        let err = check_found(CFG, FILES, fewer).unwrap_err();
        assert!(
            err.contains("Cargo.toml is on the bench gate's list"),
            "{err}"
        );
        // TASKS item 127, SD-3: the measurement flag in a file not on its own list.
        let (flag, files) = FLAGS[2];
        let measure: Vec<String> = files.iter().map(|(p, _)| (*p).to_string()).collect();
        assert_eq!(check_found(flag, files, &measure), Ok(()));
        let mut more = measure.clone();
        more.push("boot/uefi/src/kernel.rs".into());
        let err = check_found(flag, files, &more).unwrap_err();
        // The flag's name is not written here: F1 would find it in this file.
        assert!(
            err.contains(&format!(
                "boot/uefi/src/kernel.rs names the stack measurement's flag `{flag}`"
            )),
            "{err}"
        );
        assert!(
            !FILES
                .iter()
                .any(|(p, _)| *p == "developer-tools/xtask/src/stack_depth.rs"),
            "each flag has its own list"
        );
    }

    #[test]
    fn no_signing_path_takes_a_bench_file() {
        let bench_kernel = target().join("xtask").join("KERNEL.ONBF");
        assert!(refuse(&bench_kernel, "signing").is_err());
        // TASKS item 128: nor a minimal-mode build's.
        let minimal = minimal_target().join("xtask").join("KERNEL.ONBF");
        assert!(refuse(&minimal, "signing").is_err());
        // TASKS item 127: nor a stack measurement's.
        let measured = crate::stack_depth::target()
            .join("normal")
            .join("origin-boot.efi");
        let err = refuse(&measured, "signing").unwrap_err();
        assert!(err.contains("stack measurement"), "{err}");
        assert!(
            refuse(
                &crate::target_dir().join("xtask").join("KERNEL.ONBF"),
                "signing"
            )
            .is_ok()
        );
        let (key, _) = crate::test_keys().unwrap();
        let err = crate::sign_kernel(&bench_kernel, &key, "KERNEL.signed.ONBF").unwrap_err();
        assert!(err.contains("a bench build's file"), "{err}");
        // A copy outside target/bench is refused by its bytes (R112-5).
        let copy = crate::target_dir()
            .join("xtask")
            .join("bench-copy-test.ONBF");
        fs::create_dir_all(copy.parent().unwrap()).unwrap();
        fs::write(
            &copy,
            b"ONBF... origin-kernel: bench build: breadcrumbs ...",
        )
        .unwrap();
        let err = crate::sign_kernel(&copy, &key, "KERNEL.signed.ONBF").unwrap_err();
        let _ = fs::remove_file(&copy);
        assert!(err.contains("holds the text"), "{err}");
        // A bench kernel's ELF packs only into the bench directory.
        let bench_elf = target()
            .join("x86_64-unknown-none")
            .join("release")
            .join("framekernel");
        let err = crate::pack_kernel_to(
            &bench_elf,
            &crate::target_dir().join("xtask").join("K.ONBF"),
        )
        .unwrap_err();
        assert!(err.contains("a bench build's file"), "{err}");
    }

    #[test]
    fn breadcrumb_lines() {
        assert!(is_crumb_line(CRUMBS));
        assert!(is_crumb_line("RS1234"));
        assert!(!is_crumb_line(""));
        assert!(!is_crumb_line("origin-kernel: x"));
        assert!(holds(b"origin-boot: bench build: x", MARKER));
        assert!(!holds(b"origin-boot: benchbuild", MARKER));
        assert!(HALTING.starts_with(HALTING_START));
    }

    // TASKS item 128.

    #[test]
    fn the_triple_fault_is_found_only_right_after_an_indirect_call() {
        let good = "140003e4e:\tcall\trax\n140003e50:\ttest\trax, rax\n140003e53:\tjne\t0x140003e70 <.text+0x2e70>\n140003e55:\tcmp\tbyte ptr [rsp + 0x7], 0x0\n140003e5a:\tje\t0x140003e70 <.text+0x2e70>\n140003e5c:\tpush\t0x0\n140003e5e:\tpush\t0x0\n140003e60:\tlidt\t[rsp]\n140003e64:\tint3\n";
        assert!(triple_fault_after_call(good));
        assert!(triple_fault_after_call(
            &good.replace("call\trax", "call\tqword ptr [rax + 0xe8]")
        ));
        // A direct call is not the firmware's.
        assert!(!triple_fault_after_call(
            &good.replace("call\trax", "call\t0x140001000 <.text>")
        ));
        // Anything else between: a store, a port, a call, another instruction.
        for between in [
            "mov\tqword ptr [rsp + 0x8], rax",
            "out\tdx, al",
            "call\t0x140017630 <.text+0x16630>",
            "cli",
            "xor\tecx, ecx",
        ] {
            let listing = good.replace(
                "140003e5c:\tpush",
                &format!("140003e5b:\t{between}\n140003e5c:\tpush"),
            );
            assert!(!triple_fault_after_call(&listing), "{between}");
        }
        // Not the triple fault's three instructions.
        assert!(!triple_fault_after_call(
            &good.replace("lidt\t[rsp]", "lgdt\t[rsp]")
        ));
        assert!(!triple_fault_after_call(""));
    }

    #[test]
    fn a_port_0xcf9_in_dx_is_found_anywhere() {
        assert!(writes_cf9(
            "1000:\tmov\tdx, 0xcf9\n1004:\tmov\tal, 0x6\n1006:\tout\tdx, al\n"
        ));
        assert!(writes_cf9("1000:\tmov\tedx, 0xcf9\n"));
        assert!(!writes_cf9("1000:\tmov\tdx, 0x2f8\n1004:\tout\tdx, al\n"));
        assert!(!writes_cf9(""));
    }

    #[test]
    fn f1_holds_the_minimal_flag_to_its_own_list() {
        let (flag, files) = FLAGS[1];
        assert_eq!(flag, MINIMAL_CFG);
        let listed: Vec<String> = files.iter().map(|(p, _)| (*p).to_string()).collect();
        assert_eq!(check_found(flag, files, &listed), Ok(()));
        let mut more = listed.clone();
        more.push("boot/uefi/src/fch.rs".into());
        let err = check_found(flag, files, &more).unwrap_err();
        assert!(
            err.contains("boot/uefi/src/fch.rs names the minimal mode's flag"),
            "{err}"
        );
        // Each file naming the minimal flag names the bench flag too, so the bench list holds it.
        for (path, _) in MINIMAL_FILES {
            assert!(FILES.iter().any(|(p, _)| p == path), "{path}");
        }
    }

    #[test]
    fn the_stand_in_table_checks_and_carries_its_words() {
        let table = stand_in_table(b"mine stale1");
        assert_eq!(&table[..4], b"OXFC");
        assert_eq!(
            u32::from_le_bytes([table[4], table[5], table[6], table[7]]) as usize,
            table.len()
        );
        assert_eq!(table.iter().fold(0u8, |s, &b| s.wrapping_add(b)), 0);
        assert_eq!(&table[36..], b"mine stale1");
    }
}
