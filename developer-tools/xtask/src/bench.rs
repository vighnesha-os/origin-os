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

/// The stand-in table's signature (ADR-0002 Amendment 5; ruling Q4): the minimal-mode loader holds
/// it, and no other image may, so no other build can take the table (R128-3).
const STAND_IN_SIGNATURE: &[u8] = b"OXFC";

/// The bisection mode's flag (ADR-0002 Amendment 6; TASKS item 131), given with [`CFG`]; only
/// [`BISECT_FILES`] may hold it (F1, the coordinating session's ruling Q1).
const BISECT_CFG: &str = "origin_bench_bisect";

/// The text every bisection-mode image holds, beside [`MARKER`], and no other image may.
pub(crate) const BISECT_MARKER: &[u8] = b"bench build: bisect";

/// The files that may name the bisection mode's flag. Each also names the bench flag, which its
/// name begins with, so each is on [`FILES`] too.
const BISECT_FILES: &[(&str, &str)] = &[
    ("Cargo.toml", "the flag's check-cfg declaration"),
    (
        "boot/uefi/src/fch.rs",
        "the bisection mode's BENCH-FCH words, compiled only in that mode",
    ),
    (
        "boot/uefi/src/main.rs",
        "the bisection mode's branches and module (TASKS item 131)",
    ),
    (
        "developer-tools/xtask/src/toolchain.rs",
        "the one place the flag is given",
    ),
    ("developer-tools/xtask/src/bench.rs", "this gate"),
];

/// The kernel-entry mode's flag (ADR-0002 Amendment 7; TASKS item 133), given with [`CFG`]; only
/// [`KERNEL_FILES`] may hold it (F1; the coordinating session's ruling Q5).
const KERNEL_CFG: &str = "origin_bench_kernel";

/// The text every kernel-entry image holds, beside [`MARKER`], and no other image may.
pub(crate) const KERNEL_MARKER: &[u8] = b"bench build: kernel";

/// The files that may name the kernel-entry mode's flag. Each also names the bench flag, which its
/// name begins with, so each is on [`FILES`] too.
const KERNEL_FILES: &[(&str, &str)] = &[
    ("Cargo.toml", "the flag's check-cfg declaration"),
    (
        "boot/uefi/src/main.rs",
        "the kernel-entry mode's branches and module (TASKS item 133)",
    ),
    (
        "developer-tools/xtask/src/toolchain.rs",
        "the one place the flag is given",
    ),
    ("developer-tools/xtask/src/bench.rs", "this gate"),
];

/// The files that may name the minimal mode's flag. Each also names the bench flag, which its name
/// begins with, so each is on [`FILES`] too.
const MINIMAL_FILES: &[(&str, &str)] = &[
    ("Cargo.toml", "the flag's check-cfg declaration"),
    (
        "boot/uefi/src/fch.rs",
        "the stand-in table's code, compiled only in the minimal mode (R128-3)",
    ),
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
const FLAGS: [(&str, &[(&str, &str)]); 5] = [
    (CFG, FILES),
    (MINIMAL_CFG, MINIMAL_FILES),
    (BISECT_CFG, BISECT_FILES),
    (KERNEL_CFG, KERNEL_FILES),
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

/// Where bisection-mode builds go (TASKS item 131), apart from every other build.
pub(crate) fn bisect_target() -> PathBuf {
    crate::target_dir().join("bench-bisect")
}

/// Where kernel-entry builds go (TASKS item 133), apart from every other build.
pub(crate) fn kernel_target() -> PathBuf {
    crate::target_dir().join("bench-kernel")
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
        "xtask: bench gate (TASKS item 112, F1): the bench flag is named in {} listed files and nowhere else, the minimal mode's flag (TASKS item 128) in {}, the bisection mode's (TASKS item 131) in {}, the kernel-entry mode's (TASKS item 133) in {}, and the stack measurement's flag (TASKS item 127) in {}",
        counts[0], counts[1], counts[2], counts[3], counts[4]
    );
    Ok(())
}

/// The heart of [`gate`]: `found` are the files that name `flag`, which only `files` may.
fn check_found(flag: &str, files: &[(&str, &str)], found: &[String]) -> Result {
    let which = match flag {
        CFG => "the bench flag",
        MINIMAL_CFG => "the minimal mode's flag",
        BISECT_CFG => "the bisection mode's flag",
        KERNEL_CFG => "the kernel-entry mode's flag",
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

/// Whether `path` is under [`target`], [`minimal_target`], [`bisect_target`] or
/// [`kernel_target`].
pub(crate) fn is_bench(path: &Path) -> bool {
    [target(), minimal_target(), bisect_target(), kernel_target()]
        .iter()
        .any(|root| {
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

/// The bisection-mode bench loader (ADR-0002 Amendment 6): the developer-mode loader with the
/// bench flag and the bisection mode's, into [`bisect_target`].
pub(crate) fn build_bisect_loader() -> Result<PathBuf> {
    crate::build_loader_with(
        Some(Loader::Developer.feature()),
        Some(&bisect_target()),
        &toolchain::bench_bisect_config()?,
    )
}

/// The kernel-entry bench loader (ADR-0002 Amendment 7; TASKS item 133): the developer-mode loader
/// with the bench flag and the kernel-entry mode's, into [`kernel_target`].
pub(crate) fn build_kernel_loader() -> Result<PathBuf> {
    crate::build_loader_with(
        Some(Loader::Developer.feature()),
        Some(&kernel_target()),
        &toolchain::bench_kernel_config()?,
    )
}

/// `cargo xtask bench-build --minimal` (TASKS item 128): the full bench images first, then the
/// minimal-mode loader, staged with the bench kernel (which it never reads; the rental tool needs
/// one) as `target/bench-minimal/xtask/BOOTX64.EFI` and `KERNEL.ONBF` for a rental's
/// `--image-dir`. Fails unless the loader holds [`MARKER`], [`MINIMAL_MARKER`] and the stand-in
/// table's signature (the positive control of `build`'s refusal of it, R128-3), writes `0xCF9`
/// nowhere, and holds the triple fault right after the call ([`check_minimal_code`]).
pub(crate) fn build_minimal() -> Result<(PathBuf, PathBuf)> {
    let (full_loader, bench_kernel) = build()?;
    let efi = build_minimal_loader()?;
    let out = minimal_target().join("xtask");
    fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let loader = out.join("BOOTX64.EFI");
    let kernel = out.join(crate::KERNEL_IMAGE);
    fs::copy(&efi, &loader).map_err(|e| format!("copy {}: {e}", efi.display()))?;
    fs::copy(&bench_kernel, &kernel)
        .map_err(|e| format!("copy {}: {e}", bench_kernel.display()))?;
    let bytes = read_file(&loader)?;
    if !holds(&bytes, MARKER)
        || !holds(&bytes, MINIMAL_MARKER)
        || !holds(&bytes, STAND_IN_SIGNATURE)
    {
        return Err(format!(
            "{} is a minimal-mode image without the texts \"bench build\", \"bench build: minimal\" and \"OXFC\"",
            loader.display()
        ));
    }
    if holds(&bytes, BISECT_MARKER) || holds(&bytes, KERNEL_MARKER) {
        return Err(format!(
            "{} holds the text \"bench build: bisect\" or \"bench build: kernel\": another mode's code reached the minimal-mode image (TASKS items 131 and 133)",
            loader.display()
        ));
    }
    check_minimal_code(&loader, &full_loader)?;
    check_no_os_types(&loader)?;
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

/// `cargo xtask bench-build --bisect` (TASKS item 131): the full bench images first, the
/// minimal-mode loader (the live-table check's control), then the bisection-mode loader, staged
/// with the bench kernel as `target/bench-bisect/xtask/BOOTX64.EFI` and `KERNEL.ONBF` for a
/// rental's `--image-dir`. Fails unless the loader holds [`MARKER`] and [`BISECT_MARKER`], holds
/// neither the minimal mode's text nor the stand-in table's signature, and passes
/// [`check_bisect_code`].
pub(crate) fn build_bisect() -> Result<(PathBuf, PathBuf)> {
    let (full_loader, bench_kernel) = build()?;
    let minimal_loader = build_minimal_loader()?;
    let efi = build_bisect_loader()?;
    let out = bisect_target().join("xtask");
    fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let loader = out.join("BOOTX64.EFI");
    let kernel = out.join(crate::KERNEL_IMAGE);
    fs::copy(&efi, &loader).map_err(|e| format!("copy {}: {e}", efi.display()))?;
    fs::copy(&bench_kernel, &kernel)
        .map_err(|e| format!("copy {}: {e}", bench_kernel.display()))?;
    let bytes = read_file(&loader)?;
    if !holds(&bytes, MARKER) || !holds(&bytes, BISECT_MARKER) {
        return Err(format!(
            "{} is a bisection-mode image without the texts \"bench build\" and \"bench build: bisect\"",
            loader.display()
        ));
    }
    if holds(&bytes, MINIMAL_MARKER) || holds(&bytes, STAND_IN_SIGNATURE) {
        return Err(format!(
            "{} holds the text \"bench build: minimal\" or the stand-in table's signature \"OXFC\": the minimal mode's code reached the bisection-mode image (TASKS item 131)",
            loader.display()
        ));
    }
    if holds(&bytes, KERNEL_MARKER) {
        return Err(format!(
            "{} holds the text \"bench build: kernel\": the kernel-entry mode's code reached the bisection-mode image (TASKS item 133)",
            loader.display()
        ));
    }
    check_bisect_code(&loader, &full_loader, &minimal_loader)?;
    check_no_os_types(&loader)?;
    for path in [&loader, &kernel] {
        let bytes = read_file(path)?;
        println!(
            "xtask: bench-build --bisect: {}: {} bytes, SHA-256 {}",
            path.display(),
            bytes.len(),
            crate::reproducible::hex(&origin_crypto::Sha256::digest(&bytes), "")
        );
    }
    Ok((loader, kernel))
}

/// `cargo xtask bench-build --kernel` (TASKS item 133): the full bench images first (the
/// machine-code check's control), then the kernel-entry loader, staged with the bench kernel it
/// enters as `target/bench-kernel/xtask/BOOTX64.EFI` and `KERNEL.ONBF` for a rental's
/// `--image-dir`. Fails unless the loader holds [`MARKER`] and [`KERNEL_MARKER`], holds no other
/// mode's text, puts `0xCF9` in no port register (no control and no reset of Amendment 4's is
/// compiled in), and allocates none of B3's old OS-defined types.
pub(crate) fn build_kernel() -> Result<(PathBuf, PathBuf)> {
    let (full_loader, bench_kernel) = build()?;
    let efi = build_kernel_loader()?;
    let out = kernel_target().join("xtask");
    fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let loader = out.join("BOOTX64.EFI");
    let kernel = out.join(crate::KERNEL_IMAGE);
    fs::copy(&efi, &loader).map_err(|e| format!("copy {}: {e}", efi.display()))?;
    fs::copy(&bench_kernel, &kernel)
        .map_err(|e| format!("copy {}: {e}", bench_kernel.display()))?;
    let bytes = read_file(&loader)?;
    if !holds(&bytes, MARKER) || !holds(&bytes, KERNEL_MARKER) {
        return Err(format!(
            "{} is a kernel-entry image without the texts \"bench build\" and \"bench build: kernel\"",
            loader.display()
        ));
    }
    if holds(&bytes, MINIMAL_MARKER)
        || holds(&bytes, STAND_IN_SIGNATURE)
        || holds(&bytes, BISECT_MARKER)
    {
        return Err(format!(
            "{} holds another mode's text (\"bench build: minimal\", \"OXFC\" or \"bench build: bisect\"): its code reached the kernel-entry image (TASKS item 133)",
            loader.display()
        ));
    }
    check_kernel_code(&loader, &full_loader)?;
    check_no_os_types(&loader)?;
    for path in [&loader, &kernel] {
        let bytes = read_file(path)?;
        println!(
            "xtask: bench-build --kernel: {}: {} bytes, SHA-256 {}",
            path.display(),
            bytes.len(),
            crate::reproducible::hex(&origin_crypto::Sha256::digest(&bytes), "")
        );
    }
    Ok((loader, kernel))
}

/// The kernel-entry loader's machine code (TASKS item 133's ruling Q5): no `0xCF9` in a port
/// register anywhere, since the mode has no control and no reset of Amendment 4's. The full bench
/// loader, which writes `0xCF9`, is the check's control.
fn check_kernel_code(loader: &Path, full_loader: &Path) -> Result {
    let full = instructions(&disassemble(full_loader)?);
    if !full.iter().any(|i| is_cf9_load(&i.text)) {
        return Err(format!(
            "{}: the full bench loader puts no 0xCF9 in DX, so it cannot serve as the kernel-entry check's control",
            full_loader.display()
        ));
    }
    let insns = instructions(&disassemble(loader)?);
    if insns.iter().any(|i| is_cf9_load(&i.text)) {
        return Err(format!(
            "{}: the kernel-entry loader puts 0xCF9 in DX: Amendment 4's control or reset reached a mode that has neither (TASKS item 133)",
            loader.display()
        ));
    }
    Ok(())
}

/// B3's old OS-defined types, which no loader allocates since ADR-0002 Amendment 7.
const OLD_OS_TYPES: [&str; 2] = ["0x80000002", "0x80000003"];

/// The instructions of `insns` with one of [`OLD_OS_TYPES`] as an operand, matched as a whole
/// immediate (TASKS item 133's ruling Q4; its review, R133-5). Before Amendment 7 the loaders held
/// them in two shapes: `mov edx, 0x80000003` and `mov edx, 0x80000002`, `AllocatePages`'
/// `MemoryType` for the kernel stack and the boot info, and `cmp eax, …`, the translation of a
/// descriptor's type. Since Amendment 7 a loader holds neither constant at all, so any instruction
/// with either is one too many: kept in another register, stored, or compared.
pub(crate) fn os_type_allocations(insns: &[Insn]) -> Vec<usize> {
    (0..insns.len())
        .filter(|&i| {
            insns[i].text.split_once(' ').is_some_and(|(_, operands)| {
                operands
                    .split([',', ' ', '[', ']', '+'])
                    .any(|word| OLD_OS_TYPES.contains(&word))
            })
        })
        .collect()
}

/// The full bench loader's two allocations at `32b6e043`, before ADR-0002 Amendment 7, as
/// `llvm-objdump` printed them: the kernel stack (`0x80000003`, 17 pages) and the boot info
/// (`0x80000002`), each `AllocatePages(AllocateAnyPages, type, pages, &address)`; then the
/// translation's compares of a descriptor's type. [`check_no_os_types`] runs its matcher on this
/// first, so every gate shows the check can see the old loaders' instructions (R133-5).
const OLD_ALLOCATIONS: &str = "14000a010:\tlea\tr9, [rbp + 0x760]\n14000a017:\tmov\tr8d, 0x11\n14000a01d:\txor\tecx, ecx\n14000a01f:\tmov\tedx, 0x80000003\n14000a024:\tcall\tqword ptr [rbp + 0x868]\n14000a13a:\tlea\tr9, [rbp + 0x760]\n14000a141:\txor\tecx, ecx\n14000a143:\tmov\tedx, 0x80000002\n14000a148:\tmov\tr8, qword ptr [rbp + 0x700]\n14000a14f:\tcall\tqword ptr [rbp + 0x868]\n140011c24:\tcmp\teax, 0x80000002\n140011c2b:\tcmp\teax, 0x80000003\n";

/// Fails if the loader image at `image` allocates one of B3's old OS-defined types
/// ([`os_type_allocations`]): ADR-0002 Amendment 7, so that a later change cannot bring them back
/// unseen, since the translated map names the regions by address either way.
pub(crate) fn check_no_os_types(image: &Path) -> Result {
    // The control first: the batch's own loader's instructions, which the check must find.
    if os_type_allocations(&instructions(OLD_ALLOCATIONS)).len() != 4 {
        return Err(
            "the check for B3's old OS-defined types does not find them in the loader they were taken from (OLD_ALLOCATIONS), so its verdict on any image cannot be trusted (TASKS item 133, R133-5)"
                .into(),
        );
    }
    let insns = instructions(&disassemble(image)?);
    let found = os_type_allocations(&insns);
    if found.is_empty() {
        return Ok(());
    }
    let at: Vec<String> = found
        .iter()
        .map(|&i| format!("{:#x}: {}", insns[i].at, insns[i].text))
        .collect();
    Err(format!(
        "{}: the loader asks AllocatePages for one of B3's old OS-defined types, which ADR-0002 Amendment 7 replaced by EfiLoaderData: {}",
        image.display(),
        at.join("; ")
    ))
}

/// The bisection-mode loader's machine code (ADR-0002 Amendment 6; TASKS item 131): no `0xCF9` in
/// a port register anywhere (step 5), a path from an indirect call to the triple fault with
/// nothing on it but reads into registers, compares and branches (step 5, as Amendment 5's step
/// 6), and that call's target not loaded through the live boot services table (step 4, "through
/// the copied table"; the coordinating session's ruling Q9). Each check has its controls: the full
/// bench loader writes `0xCF9`, has no triple fault right after a call, and makes its call through
/// its copy; the minimal-mode loader makes its call through the live table.
fn check_bisect_code(loader: &Path, full_loader: &Path, minimal_loader: &Path) -> Result {
    let full = instructions(&disassemble(full_loader)?);
    let minimal = instructions(&disassemble(minimal_loader)?);
    let full_calls = reset_calls(&full);
    let minimal_calls = triple_fault_calls(&minimal);
    let one =
        |calls: &[usize], insns: &[Insn]| calls.len() == 1 && takes_exit_arguments(insns, calls[0]);
    if !full.iter().any(|i| is_cf9_load(&i.text))
        || !triple_fault_calls(&full).is_empty()
        || !one(&full_calls, &full)
        || full_calls.iter().any(|&i| through_live_table(&full, i))
        || !live_exit_loads(&full).is_empty()
        || !one(&minimal_calls, &minimal)
        || !minimal_calls
            .iter()
            .all(|&i| through_live_table(&minimal, i))
        || live_exit_loads(&minimal).is_empty()
    {
        return Err(format!(
            "{} and {}: the full bench loader and the minimal-mode loader do not serve as the machine-code checks' controls (the full one: 0xCF9 in DX, no triple fault right after a call, one ExitBootServices-like call, through its copy, and no read of ExitBootServices from the live table; the minimal one: one such call, through the live table), so their verdict on the bisection-mode loader cannot be trusted",
            full_loader.display(),
            minimal_loader.display()
        ));
    }
    let insns = instructions(&disassemble(loader)?);
    if insns.iter().any(|i| is_cf9_load(&i.text)) {
        return Err(format!(
            "{}: the bisection-mode loader puts 0xCF9 in DX: it would write port 0xCF9, which ADR-0002 Amendment 6 forbids in this mode",
            loader.display()
        ));
    }
    let calls = triple_fault_calls(&insns);
    if calls.is_empty() {
        return Err(format!(
            "{}: no path from an indirect call to the triple fault holds only reads into registers, compares and branches: the triple fault is not the first thing after ExitBootServices (ADR-0002 Amendment 6, step 5)",
            loader.display()
        ));
    }
    // R131-3: the call found must be ExitBootServices itself, and the only one, so that no other
    // firmware call (a `ResetSystem`, say) can stand between the call and the triple fault and be
    // taken for it.
    if calls.len() != 1 || !takes_exit_arguments(&insns, calls[0]) {
        return Err(format!(
            "{}: {} indirect calls reach the triple fault, and ExitBootServices' must be the only one, with its image handle and map key from memory or registers, not constants: another call stands before the triple fault (ADR-0002 Amendment 6, step 5)",
            loader.display(),
            calls.len()
        ));
    }
    if calls.iter().any(|&i| through_live_table(&insns, i)) || !live_exit_loads(&insns).is_empty() {
        return Err(format!(
            "{}: ExitBootServices is read from the live boot services table, for the call or anywhere else in the image, not taken from the loader's copy (ADR-0002 Amendment 6, step 4)",
            loader.display()
        ));
    }
    Ok(())
}

/// Every `mov <r>, qword ptr [<b> + 0xe8]` of `insns` whose `<b>` was itself loaded from
/// `[<s> + 0x60]`: `ExitBootServices` read through the live boot services table, wherever the
/// pointer then goes (TASKS item 131's review, R131-2). A spill of the table's address between the
/// two loads would hide one; the controls in [`check_bisect_code`] show the shape the loaders
/// compile to.
fn live_exit_loads(insns: &[Insn]) -> Vec<usize> {
    (0..insns.len())
        .filter(|&k| {
            qword_load(&insns[k].text).is_some_and(|(_, base, disp)| {
                disp == EXIT_BOOT_SERVICES_AT
                    && last_load(insns, k, base).is_some_and(|(_, _, d)| d == BOOT_SERVICES_AT)
            })
        })
        .collect()
}

/// Whether `mnemonic` only reads its first operand and writes none.
fn reads_only(mnemonic: &str) -> bool {
    matches!(mnemonic, "test" | "cmp" | "push" | "nop" | "bt")
        || mnemonic.starts_with('j')
        || mnemonic.starts_with("call")
}

/// Whether the indirect call at `insns[i]` takes arguments as `ExitBootServices(ImageHandle,
/// MapKey)` does: RCX and RDX last written from memory or another register, not set to a
/// constant (TASKS item 131's review, R131-3). `ResetSystem(EfiResetCold, EFI_SUCCESS, 0, NULL)`,
/// the firmware call most likely to be put before the triple fault, takes constants in both.
fn takes_exit_arguments(insns: &[Insn], i: usize) -> bool {
    let constant = |reg64: &str, reg32: &str| {
        let from = i.saturating_sub(LOAD_WINDOW);
        let last = (from..i).rev().find_map(|k| {
            let text = insns.get(k)?.text.as_str();
            let (mnemonic, operands) = text.split_once(' ')?;
            let dest = operands.split(", ").next()?;
            (!reads_only(mnemonic) && (dest == reg64 || dest == reg32))
                .then_some((mnemonic, operands))
        });
        match last {
            // Not written in the window: whatever the caller left, not a constant set here.
            None => false,
            Some(("xor", operands)) => {
                let mut ops = operands.split(", ");
                ops.next() == ops.next()
            }
            Some(("mov", operands)) => operands
                .split(", ")
                .nth(1)
                .is_some_and(|source| source.starts_with("0x") || source.parse::<i64>().is_ok()),
            Some(_) => false,
        }
    };
    insns.get(i).is_some_and(|c| c.text.starts_with("call "))
        && !constant("rcx", "ecx")
        && !constant("rdx", "edx")
}

/// Where the system table holds its boot services table's address (UEFI 2.11 §4.3: the header's
/// 24 bytes, then nine members of 8 bytes, the 32-bit revision padded to 8), and where that table
/// holds `ExitBootServices` (§4.4: the header's 24 bytes, then 26 pointers before it).
const BOOT_SERVICES_AT: &str = "0x60";
const EXIT_BOOT_SERVICES_AT: &str = "0xe8";

/// How far back from a call [`through_live_table`] looks for the loads its target came from.
const LOAD_WINDOW: usize = 64;

/// The register and displacement a `mov <reg>, qword ptr [<base> + <disp>]` or `[<base>]` loads
/// from, with `text`'s destination register.
fn qword_load(text: &str) -> Option<(&str, &str, &str)> {
    let operands = text.strip_prefix("mov ")?;
    let (dest, source) = operands.split_once(", ")?;
    let inner = source.strip_prefix("qword ptr [")?.strip_suffix(']')?;
    let (base, disp) = inner.split_once(" + ").unwrap_or((inner, "0x0"));
    Some((dest, base, disp))
}

/// The last instruction before `before` within [`LOAD_WINDOW`] that writes `reg`, if it is a
/// qword load: its index, base and displacement.
fn last_load<'a>(insns: &'a [Insn], before: usize, reg: &str) -> Option<(usize, &'a str, &'a str)> {
    let from = before.saturating_sub(LOAD_WINDOW);
    (from..before).rev().find_map(|k| {
        let text = insns.get(k)?.text.as_str();
        let (mnemonic, operands) = text.split_once(' ')?;
        let writes = !reads_only(mnemonic) && operands.split(", ").next() == Some(reg);
        if !writes {
            return None;
        }
        Some(
            qword_load(text)
                .map(|(_, base, disp)| (k, base, disp))
                .unwrap_or((k, "", "")),
        )
    })
}

/// Whether the indirect call at `insns[i]` takes its target through the live boot services table
/// (the coordinating session's ruling Q9 of TASKS item 131): its target loaded from
/// `[<b> + 0xe8]`, `ExitBootServices`' place, where `<b>` was itself loaded from `[<s> + 0x60]`,
/// the system table's boot services member. Each register's last write is found by searching back
/// from the call, which a branch could mislead; the controls in [`check_bisect_code`] show it reads
/// both loaders' calls as they are.
fn through_live_table(insns: &[Insn], i: usize) -> bool {
    let Some(operand) = insns.get(i).and_then(|c| c.text.strip_prefix("call ")) else {
        return false;
    };
    let (at, base, disp) = if let Some(inner) = operand
        .strip_prefix("qword ptr [")
        .and_then(|o| o.strip_suffix(']'))
    {
        let (base, disp) = inner.split_once(" + ").unwrap_or((inner, "0x0"));
        (i, base, disp)
    } else {
        match last_load(insns, i, operand) {
            Some(found) => found,
            None => return false,
        }
    };
    disp == EXIT_BOOT_SERVICES_AT
        && last_load(insns, at, base).is_some_and(|(_, _, d)| d == BOOT_SERVICES_AT)
}

/// Whether `text` puts `0xCF9` in DX.
fn is_cf9_load(text: &str) -> bool {
    matches!(text, "mov dx, 0xcf9" | "mov edx, 0xcf9")
}

/// The minimal-mode loader's machine code, as the pinned toolchain's `llvm-objdump` prints it
/// (TASKS item 128): no `0xCF9` in a port register anywhere ("there is no `0xCF9` write anywhere in
/// this mode"), and a path from an indirect call to the triple fault with nothing on it but reads
/// into registers, compares and branches (Amendment 5, step 6: "nothing comes between them").
fn check_minimal_code(loader: &Path, full_loader: &Path) -> Result {
    // The checks' controls, on the full bench loader (R128-8, R128-9): it writes `0xCF9`, so
    // `writes_cf9` sees that form; and its triple fault comes after `out`, `cli` and a wait, so
    // `triple_fault_after_call` must not find it.
    let full = disassemble(full_loader)?;
    if !writes_cf9(&full) || triple_fault_after_call(&full) {
        return Err(format!(
            "{}: the full bench loader does not serve as the machine-code checks' control (0xCF9 in DX: {}; a triple fault right after a call: {}), so their verdict on the minimal loader cannot be trusted",
            full_loader.display(),
            writes_cf9(&full),
            triple_fault_after_call(&full)
        ));
    }
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
    // TASKS item 131's review, R131-3: that call must be ExitBootServices itself, through the live
    // table, and the only one.
    let insns = instructions(&listing);
    let calls = triple_fault_calls(&insns);
    if calls.len() != 1
        || !takes_exit_arguments(&insns, calls[0])
        || !through_live_table(&insns, calls[0])
    {
        return Err(format!(
            "{}: {} indirect calls reach the triple fault, and ExitBootServices' through the live table must be the only one: another call stands before the triple fault (ADR-0002 Amendment 5, step 6)",
            loader.display(),
            calls.len()
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
    instructions(listing).iter().any(|i| is_cf9_load(&i.text))
}

/// Whether `listing` holds an indirect call and, within [`AFTER_CALL_WINDOW`] instructions along
/// some path from it, the triple fault's first instruction (`push 0x0`, `push 0x0`,
/// `lidt [rsp]`), every instruction before it on the path one [`step`] allows.
fn triple_fault_after_call(listing: &str) -> bool {
    !triple_fault_calls(&instructions(listing)).is_empty()
}

/// The indirect calls of `insns` from which [`triple_fault_after_call`]'s path reaches the triple
/// fault.
fn triple_fault_calls(insns: &[Insn]) -> Vec<usize> {
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
    (0..insns.len())
        .filter(|&i| {
            insns[i]
                .text
                .strip_prefix("call ")
                .is_some_and(|op| !op.starts_with("0x"))
                && reaches(i + 1)
        })
        .collect()
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
        if holds(&bytes, MINIMAL_MARKER) || holds(&bytes, STAND_IN_SIGNATURE) {
            return Err(format!(
                "{} holds the text \"bench build: minimal\" or the stand-in table's signature \"OXFC\": the minimal mode's code reached the full bench image (TASKS item 128, ruling Q4 and R128-3)",
                path.display()
            ));
        }
        if holds(&bytes, BISECT_MARKER) {
            return Err(format!(
                "{} holds the text \"bench build: bisect\": the bisection mode's code reached the full bench image (TASKS item 131)",
                path.display()
            ));
        }
        if holds(&bytes, KERNEL_MARKER) {
            return Err(format!(
                "{} holds the text \"bench build: kernel\": the kernel-entry mode's code reached the full bench image (TASKS item 133)",
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
    // ADR-0002 Amendment 7 (TASKS item 133's ruling Q4): the full bench loader, like every other.
    check_no_os_types(&loader)?;
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
            is_general_register(dest).then_some(Step::Next)
        }
        "cmp" | "test" | "nop" => Some(Step::Next),
        "jmp" => target().map(Step::Jump),
        m if m.starts_with('j') => target().map(Step::Branch),
        _ => None,
    }
}

/// Whether `name` is a general-purpose register other than the stack pointer, so a `mov` into it
/// only reads (TASKS item 128's review, R128-8): not memory, and not a control, debug or segment
/// register, nor RSP.
fn is_general_register(name: &str) -> bool {
    const NAMES: &[&str] = &[
        "rax", "rbx", "rcx", "rdx", "rsi", "rdi", "rbp", "eax", "ebx", "ecx", "edx", "esi", "edi",
        "ebp", "ax", "bx", "cx", "dx", "si", "di", "bp", "al", "bl", "cl", "dl", "ah", "bh", "ch",
        "dh", "sil", "dil", "bpl",
    ];
    NAMES.contains(&name)
        || name
            .strip_prefix('r')
            .map(|rest| rest.trim_end_matches(['d', 'w', 'b']))
            .and_then(|n| n.parse::<u8>().ok())
            .is_some_and(|n| (8..=15).contains(&n))
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
    !reset_calls(&instructions(listing)).is_empty()
}

/// The indirect calls of `insns` from which [`reset_after_call`]'s path reaches the reset.
fn reset_calls(insns: &[Insn]) -> Vec<usize> {
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
    (0..insns.len())
        .filter(|&i| insns[i].text.starts_with("call qword ptr [") && reaches(i + 1))
        .collect()
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
    // TASKS item 131: the bisection mode's loader, likewise.
    let mut bisect = cargo();
    bisect
        .args(["clippy", "--package", crate::LOADER_PACKAGE, "--target"])
        .arg(crate::UEFI_TARGET)
        .args(["--features", Loader::Developer.feature(), "--target-dir"])
        .arg(bisect_target())
        .args(toolchain::bench_bisect_config()?)
        .args(["--", "-D", "warnings"]);
    run_step(&mut bisect)?;
    // TASKS item 133: the kernel-entry mode's loader, likewise.
    let mut kernel_entry = cargo();
    kernel_entry
        .args(["clippy", "--package", crate::LOADER_PACKAGE, "--target"])
        .arg(crate::UEFI_TARGET)
        .args(["--features", Loader::Developer.feature(), "--target-dir"])
        .arg(kernel_target())
        .args(toolchain::bench_kernel_config()?)
        .args(["--", "-D", "warnings"]);
    run_step(&mut kernel_entry)?;
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
// TASKS item 131 (ADR-0002 Amendment 6): the bisection mode's lines.
const BIS_START: &str =
    "origin-boot: bench build: bisect: the bisection mode (ADR-0002 Amendment 6)";
const BIS_AFTER_IGNORED: &str = "origin-boot: bench build: bisect: BENCH-AFTER's value is ignored: this mode never enters the kernel, and after a successful call it triple-faults";
const BIS_CONTROL: &str = "origin-boot: bench build: bisect: the last reset was not a shutdown (the reset pin (bit 16)): the boot's first run, the control: a triple fault now, and ExitBootServices is not called";
const BIS_MINE: &str = "origin-boot: bench build: bisect: the last reset was a shutdown (PMxC0 bit 23), as the control's triple fault writes: ExitBootServices is called through the loader's copy of the boot services table; after EFI_SUCCESS, a triple fault and nothing before it";
const BIS_NO_RECORD: &str = "origin-boot: bench build: bisect: PMxC0 0x00000800 names no reset cause: no record of the last reset, and no control in this boot: ExitBootServices is called through the loader's copy of the boot services table; after EFI_SUCCESS, a triple fault and nothing before it";
const BIS_NO_RECORD_BITS_CLEAR: &str = "origin-boot: bench build: bisect: PMxC0 0x00000800 names no reset cause: no record of the last reset, and no control in this boot: ExitBootServices is called through the loader's copy of the boot services table; after EFI_SUCCESS, a triple fault and nothing before it, though PMx74 bit 17 and PMx08 bit 20 are not both set, so a shutdown may not reset this system (decision item 349)";
const BIS_LAST: &str = "origin-boot: bench build: bisect: ExitBootServices next, through the loader's copy of the boot services table";
const BIS_NOT_READ: &str = "origin-boot: bench build: bisect: the FCH's registers were not read: no control in this boot: ExitBootServices is called through the loader's copy of the boot services table; after EFI_SUCCESS, a triple fault and nothing before it, though PMx74 bit 17 and PMx08 bit 20 are not both set, so a shutdown may not reset this system (decision item 349)";
const BIS_BITS_CLEAR: &str = "origin-boot: bench build: bisect: the last reset was the reset pin (bit 16), but a shutdown cannot reset this system (PMx74 bit 17 and PMx08 bit 20 are not both set): no control in this boot: ExitBootServices is called through the loader's copy of the boot services table; after EFI_SUCCESS, a triple fault and nothing before it, though PMx74 bit 17 and PMx08 bit 20 are not both set, so a shutdown may not reset this system (decision item 349)";
const BIS_NO_RESET: &str =
    "origin-boot: bench build: BENCH-FCH's noreset: Amendment 3's order calls no ResetSystem";
const BIS_NO_OS_TYPE: &str = "origin-boot: bench build: bisect: the memory map the page tables were built from holds no region of an OS-defined type (0x80000000 and above), in ";
const BIS_CF9_SKIPPED: &str = "origin-boot: bench build: ResetSystem was not called (BENCH-FCH's noreset), and the FADT gives no usable reset register (RESET_REG is I/O port 0xcf9, which the bisection mode never writes (ADR-0002 Amendment 6)): halting";
// TASKS item 133: the kernel-entry mode's three lines.
const KER_START: &str = "origin-boot: bench build: kernel: the kernel-entry mode (TASKS item 133, ADR-0002 Amendment 7)";
const KER_AFTER_IGNORED: &str = "origin-boot: bench build: kernel: BENCH-AFTER's value is ignored: this mode enters the kernel after a successful call, and halts after two failed calls";
const KER_NO_AFTER: &str = "origin-boot: bench build: kernel: no BENCH-AFTER; this mode enters the kernel after a successful call, and halts after two failed calls";
const KER_FCH: &str = "origin-boot: bench build: kernel: the FCH's record decides nothing in this mode: no control, and no reset after the call";
/// The full loader's lines before the call that the bisection mode keeps (Amendment 6, step 1).
const FULL_WORK: [&str; 4] = [
    "origin-boot: read \\EFI\\ORIGIN\\KERNEL.ONBF",
    "origin-boot: kernel verified",
    "origin-boot: random: ",
    "origin-boot: page tables built",
];
const MIN_STAND_IN: &str = "origin-boot: bench build: minimal: the stand-in table (OXFC): the FCH's registers are not read; its values stand in for them (QEMU only)";
const MIN_CONTROL: &str = "origin-boot: bench build: minimal: the last reset was not a shutdown (the reset pin (bit 16)): the boot's first run, the control: a triple fault now, and ExitBootServices is not called";
const MIN_MINE: &str = "origin-boot: bench build: minimal: the last reset was a shutdown (PMxC0 bit 23), as the control's triple fault writes: ExitBootServices is called through the live boot services table; after EFI_SUCCESS, a triple fault and nothing before it";
const MIN_NO_RECORD: &str = "origin-boot: bench build: minimal: PMxC0 0x00000800 names no reset cause: no record of the last reset, and no control in this boot: ExitBootServices is called through the live boot services table; after EFI_SUCCESS, a triple fault and nothing before it";
const MIN_NO_RECORD_BITS_CLEAR: &str = "origin-boot: bench build: minimal: PMxC0 0x00000800 names no reset cause: no record of the last reset, and no control in this boot: ExitBootServices is called through the live boot services table; after EFI_SUCCESS, a triple fault and nothing before it, though PMx74 bit 17 and PMx08 bit 20 are not both set, so a shutdown may not reset this system (decision item 349)";
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

fn minimal_bits_clear(work: &Path) -> Result<Vec<String>> {
    stand_in(work, b"norecord noshutdown")
}

fn minimal_stale_twice(work: &Path) -> Result<Vec<String>> {
    stand_in(work, b"mine stale2 noreset")
}

/// TASKS item 131: `BENCH-FCH` with `words`, as the bisection mode reads it.
fn bisect_first(work: &Path) -> Result<Vec<String>> {
    fch(work, b"first\n")
}

fn bisect_mine(work: &Path) -> Result<Vec<String>> {
    fch(work, b"mine\n")
}

fn bisect_no_record(work: &Path) -> Result<Vec<String>> {
    fch(work, b"norecord\n")
}

fn bisect_bits_clear(work: &Path) -> Result<Vec<String>> {
    fch(work, b"norecord noshutdown\n")
}

fn bisect_stale_once(work: &Path) -> Result<Vec<String>> {
    switch(work, b"1\n")?;
    fch(work, b"mine\n")
}

fn bisect_stale_twice(work: &Path) -> Result<Vec<String>> {
    switch(work, b"2\n")?;
    fch(work, b"mine noreset\n")
}

/// No `BENCH-FCH`: the rental's path, where QEMU's processor has no FCH, so its registers are not
/// read and the call is made with no control (R131-5).
fn bisect_no_switch(work: &Path) -> Result<Vec<String>> {
    spcr(work)
}

/// `first noshutdown`: another cause, but the shutdown message off, so the control is withheld
/// (R131-5).
fn bisect_first_noshutdown(work: &Path) -> Result<Vec<String>> {
    fch(work, b"first noshutdown\n")
}

/// TASKS item 133: a stale key once, with no `BENCH-AFTER`: the kernel-entry mode enters the
/// kernel after the retry.
fn kernel_stale_once(work: &Path) -> Result<Vec<String>> {
    switch(work, b"1\n")?;
    spcr(work)
}

/// TASKS item 133: a stale key twice, with no `BENCH-AFTER`: Amendment 2's line and the halt.
fn kernel_stale_twice(work: &Path) -> Result<Vec<String>> {
    switch(work, b"2\n")?;
    spcr(work)
}

/// `BENCH-AFTER kernel`, which the bisection mode reads and ignores (ruling Q3).
fn bisect_after_kernel(work: &Path) -> Result<Vec<String>> {
    after(work, b"kernel\n")?;
    fch(work, b"mine\n")
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

/// The thirty-six scenarios of [`boot`]: TASKS item 112's four, TASKS item 116's three, TASKS
/// item 121's nine, TASKS item 128's six of the minimal mode, TASKS item 131's nine of the
/// bisection mode, and TASKS item 133's five of the kernel-entry mode.
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
            "bench: minimal, a stale key twice, Amendment 2's line, then the FADT's register skipped",
            Loader::BenchMinimal,
            lines(&[
                MIN_MINE,
                MIN_LAST,
                MARK_FIRST,
                MARK_RETRY,
                HALTING,
                MIN_HALT_SOON,
                MIN_CF9_SKIPPED,
            ]),
            minimal_stale_twice,
            check_minimal_both_failed,
            Some(QUIET),
        ),
        scenario(
            "bench: minimal, the shutdown bits clear, the triple fault anyway",
            Loader::BenchMinimal,
            lines(&[MIN_NO_RECORD_BITS_CLEAR, MIN_LAST, MARK_FIRST]),
            minimal_bits_clear,
            check_minimal_bits_clear,
            Some(RESET_WITHIN),
        ),
        // TASKS item 131 (ADR-0002 Amendment 6): the bisection mode.
        scenario(
            "bench: bisect, the control, a triple fault before the call",
            Loader::BenchBisect,
            lines(&[BIS_START, BIS_CONTROL]),
            bisect_first,
            check_bisect_control,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: bisect, a returning call through the copy, the triple fault at once",
            Loader::BenchBisect,
            lines(&[BIS_MINE, BIS_LAST, MARK_FIRST]),
            bisect_mine,
            check_bisect_mine,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: bisect, no record, no control, the triple fault at once",
            Loader::BenchBisect,
            lines(&[BIS_NO_RECORD, BIS_LAST, MARK_FIRST]),
            bisect_no_record,
            check_bisect_no_record,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: bisect, a stale key once, the triple fault only after the retry succeeds",
            Loader::BenchBisect,
            lines(&[BIS_MINE, BIS_LAST, MARK_FIRST, MARK_RETRY]),
            bisect_stale_once,
            check_bisect_retry,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: bisect, a stale key twice, Amendment 2's line, then the FADT's register skipped",
            Loader::BenchBisect,
            lines(&[
                BIS_MINE,
                BIS_LAST,
                MARK_FIRST,
                MARK_RETRY,
                HALTING,
                MIN_HALT_SOON,
                BIS_CF9_SKIPPED,
            ]),
            bisect_stale_twice,
            check_bisect_both_failed,
            Some(QUIET),
        ),
        scenario(
            "bench: bisect, the shutdown bits clear, the triple fault anyway",
            Loader::BenchBisect,
            lines(&[BIS_NO_RECORD_BITS_CLEAR, BIS_LAST, MARK_FIRST]),
            bisect_bits_clear,
            check_bisect_bits_clear,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: bisect, no BENCH-FCH, the rental's path, no control, the triple fault at once",
            Loader::BenchBisect,
            lines(&[BIS_START, NO_FCH, BIS_NOT_READ, BIS_LAST, MARK_FIRST]),
            bisect_no_switch,
            check_bisect_no_switch,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: bisect, FCH first noshutdown, the control withheld, the triple fault at once",
            Loader::BenchBisect,
            lines(&[BIS_BITS_CLEAR, BIS_LAST, MARK_FIRST]),
            bisect_first_noshutdown,
            check_bisect_first_noshutdown,
            Some(RESET_WITHIN),
        ),
        scenario(
            "bench: bisect, BENCH-AFTER kernel ignored, the triple fault at once",
            Loader::BenchBisect,
            lines(&[
                AFTER_KERNEL,
                BIS_AFTER_IGNORED,
                BIS_MINE,
                BIS_LAST,
                MARK_FIRST,
            ]),
            bisect_after_kernel,
            check_bisect_after_kernel,
            Some(RESET_WITHIN),
        ),
        // TASKS item 133: the kernel-entry mode, as rental I would run it and against the switches
        // that would make the full mode reset.
        scenario(
            "bench: kernel, no switch, the bench kernel entered with its breadcrumbs",
            Loader::BenchKernel,
            with_crumbs(FIRST_CALL),
            spcr,
            check_kernel_first_call,
            None,
        ),
        scenario(
            "bench: kernel, a stale key once, the bench kernel entered after the retry",
            Loader::BenchKernel,
            with_crumbs(RETRY),
            kernel_stale_once,
            check_kernel_retry,
            None,
        ),
        scenario(
            "bench: kernel, a stale key twice, Amendment 2's line and the halt, no reset",
            Loader::BenchKernel,
            lines(&[HALTING_START]),
            kernel_stale_twice,
            check_kernel_halt,
            Some(QUIET),
        ),
        scenario(
            "bench: kernel, BENCH-AFTER fadt ignored, the bench kernel entered",
            Loader::BenchKernel,
            with_crumbs(FIRST_CALL),
            spcr_fadt,
            check_kernel_after_ignored,
            None,
        ),
        scenario(
            "bench: kernel, BENCH-FCH first, no control, the bench kernel entered",
            Loader::BenchKernel,
            with_crumbs(FIRST_CALL),
            fch_first,
            check_kernel_fch_first,
            None,
        ),
    ]
}

/// The loader's console (COM1) holds `switch_line`, `after_line`, the state line,
/// `LOADER_NAMES_COM2`, and TASKS item 116's lines about the reset and the UART, all before its
/// last line; and the line straight to the UART before the call is on the SPCR's UART only, before
/// the loader's last line there.
fn check_loader_console(done: &Completed, switch_line: &str, after_line: &str) -> Result {
    check_loader_console_in(done, switch_line, after_line, false)
}

/// TASKS item 133: [`check_loader_console`] for the kernel-entry mode, whose console also holds its
/// three lines, and no FCH decision.
fn check_kernel_console(done: &Completed, switch_line: &str, after_line: &str) -> Result {
    check_loader_console_in(done, switch_line, after_line, true)
}

/// [`check_loader_console`], for the full mode or (`kernel`) the kernel-entry mode.
fn check_loader_console_in(
    done: &Completed,
    switch_line: &str,
    after_line: &str,
    kernel: bool,
) -> Result {
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    let at = |needle: &str| com1.find(needle);
    let last = at(LAST_LOADER_LINE).ok_or("COM1 lacks the loader's last line")?;
    let own: &[&str] = if kernel { &[KER_START] } else { &[] };
    // R133-4: the kernel-entry mode writes its own line about BENCH-AFTER, never the full mode's,
    // which would say the kernel is not entered.
    if kernel && com1.contains("the kernel is not entered") {
        return Err(
            "the kernel-entry mode's console says the kernel is not entered (TASKS item 133, R133-4)"
                .into(),
        );
    }
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
    ]
    .iter()
    .chain(own.iter())
    {
        match at(needle) {
            Some(i) if i < last => {}
            _ => {
                return Err(format!(
                    "COM1, the bench loader's console, lacks {needle:?} before its last line"
                ));
            }
        }
    }
    check_fch_lines_in(&com1, last, kernel)?;
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

/// TASKS item 133: the kernel-entry mode with no switch, as a rental runs it: the bench kernel
/// entered after the first call, with its breadcrumbs.
fn check_kernel_first_call(done: &Completed, settings: &Settings) -> Result {
    check_kernel_console(done, NO_SWITCH, KER_NO_AFTER)?;
    check_crumbs(done, settings, FIRST_CALL, &[MARK_FIRST], AFTER_FIRST)
}

/// TASKS item 133: the retry, then the bench kernel.
fn check_kernel_retry(done: &Completed, settings: &Settings) -> Result {
    check_kernel_console(done, SWITCH_ONCE, KER_NO_AFTER)?;
    check_crumbs(
        done,
        settings,
        RETRY,
        &[MARK_FIRST, MARK_RETRY],
        AFTER_RETRY,
    )
}

/// TASKS item 133: `BENCH-AFTER fadt`, which would make the full mode reset through the FADT's
/// register, ignored: the bench kernel entered.
fn check_kernel_after_ignored(done: &Completed, settings: &Settings) -> Result {
    check_kernel_console(done, NO_SWITCH, KER_AFTER_IGNORED)?;
    check_crumbs(done, settings, FIRST_CALL, &[MARK_FIRST], AFTER_FIRST)
}

/// TASKS item 133: `BENCH-FCH first`, which would make the full mode's boot a control, read for the
/// FCH's lines only: no control, and the bench kernel entered.
fn check_kernel_fch_first(done: &Completed, settings: &Settings) -> Result {
    check_kernel_console(done, NO_SWITCH, KER_NO_AFTER)?;
    // R133-7: the switch read, as the full mode reads it, so its values are on the console.
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    for needle in [FCH_SWITCH, FCH_REGISTERS, SMI_GIVEN] {
        if !com1.contains(needle) {
            return Err(format!(
                "COM1, the kernel-entry loader's console, lacks {needle:?}: BENCH-FCH was not read as the full mode reads it"
            ));
        }
    }
    check_crumbs(done, settings, FIRST_CALL, &[MARK_FIRST], AFTER_FIRST)
}

/// Both calls failed: on the SPCR's UART the marks, the halting line and the line after the call
/// follow the loader's last line and, [`QUIET`] later, nothing follows them; QEMU did not exit;
/// and no kernel line was written anywhere.
fn check_halt(done: &Completed, _settings: &Settings) -> Result {
    check_halt_in(done, AFTER_KERNEL, false)
}

/// TASKS item 133: [`check_halt`] for the kernel-entry mode, which halts after both calls fail
/// whatever `BENCH-AFTER` says, with no reset.
fn check_kernel_halt(done: &Completed, _settings: &Settings) -> Result {
    check_halt_in(done, KER_NO_AFTER, true)
}

/// [`check_halt`], with `after_line` and the full mode's or (`kernel`) the kernel-entry mode's
/// console.
fn check_halt_in(done: &Completed, after_line: &str, kernel: bool) -> Result {
    check_loader_console_in(done, SWITCH_TWICE, after_line, kernel)?;
    let after = after_last_line(done)?;
    // The kernel-entry mode writes no line after the call once both have failed, and writes no
    // UART register back (ruling Q5; R133-3): the marks and Amendment 2's line, then nothing.
    let whole = if kernel {
        after == [MARK_FIRST, MARK_RETRY, HALTING]
    } else {
        after.len() == 4
            && after[..3] == [MARK_FIRST, MARK_RETRY, HALTING]
            && is_after_call(&after[3], AFTER_BOTH_FAILED)
    };
    if !whole {
        return Err(format!(
            "after both calls failed, the SPCR's UART should hold the marks, the halting line{} after the loader's last line, and nothing more for {} s; it holds {after:?}",
            if kernel {
                ""
            } else {
                " and the line after the call"
            },
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
///
/// In the kernel-entry mode (`kernel`, TASKS item 133) the FCH's lines end in its own line, and
/// hold no decision of the full mode's.
fn check_fch_lines_in(com1: &str, before: usize, kernel: bool) -> Result {
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
        if kernel {
            &[FCH_REGISTERS, KER_FCH]
        } else {
            &[FCH_REGISTERS]
        }
    } else {
        if !FCH_QEMU_NOT_READ.contains(&tail) {
            return Err(format!(
                "under QEMU, with no BENCH-FCH, the FCH's block is not read: {where_line:?}"
            ));
        }
        if kernel {
            &[NO_FCH, KER_FCH]
        } else {
            &[NO_FCH, FCH_NO_CONTROL]
        }
    };
    if kernel && (com1.contains(FCH_NO_CONTROL) || com1.contains("the control:")) {
        return Err(
            "the kernel-entry mode printed a decision of the full mode's on the FCH's record (TASKS item 133)"
                .into(),
        );
    }
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

/// Both calls given a stale key (R128-10): `X`, `Y`, Amendment 2's line, then Amendment 3's order
/// with no `ResetSystem` (`noreset`), whose FADT step the mode skips, since the register is port
/// `0xCF9` under QEMU (the coordinating session's addition to ruling Q3, kept for the error path
/// by item 349): the halt, and QEMU runs on.
fn check_minimal_both_failed(done: &Completed, _settings: &Settings) -> Result {
    check_minimal_console(done, MIN_MINE)?;
    let after = after_minimal_last(done)?;
    let want = [
        MARK_FIRST,
        MARK_RETRY,
        HALTING,
        MIN_HALT_SOON,
        MIN_CF9_SKIPPED,
    ];
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

/// The shutdown bits read clear (owner item 349, "Forced fault anyway"): the line says a shutdown
/// may not reset this system, no control is made, and a successful call is still followed by the
/// triple fault at once, with no firmware call.
fn check_minimal_bits_clear(done: &Completed, _settings: &Settings) -> Result {
    check_minimal_call(done, MIN_NO_RECORD_BITS_CLEAR, &[MARK_FIRST])
}

/// The bisection mode's console (COM1): its own lines, `BENCH-FCH`'s, the SMI line given, the
/// table's line, the no-OS-type line and `decision`; the full loader's work before the call
/// (Amendment 6, step 1); and not the full loader's last line, since the kernel is never entered.
fn check_bisect_console(done: &Completed, decision: &str) -> Result {
    check_bisect_console_with(done, decision, true)
}

/// [`check_bisect_console`], with `BENCH-FCH` (`switch`) or without it, the rental's path, where
/// QEMU's processor has no FCH: then the lines say no switch and the registers not read.
fn check_bisect_console_with(done: &Completed, decision: &str, switch: bool) -> Result {
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    let fch: &[&str] = if switch {
        &[FCH_SWITCH, FCH_REGISTERS, SMI_GIVEN]
    } else {
        &[NO_FCH, SMI_LINE]
    };
    if !switch && com1.contains(FCH_REGISTERS) {
        return Err("with no BENCH-FCH, the bisection mode printed BENCH-FCH's registers".into());
    }
    for needle in [
        BIS_START,
        BIS_AFTER_IGNORED,
        TABLE_LINE,
        BIS_NO_OS_TYPE,
        STATE,
        RESET_PLAN,
        decision,
    ]
    .iter()
    .chain(fch.iter())
    .chain(FULL_WORK.iter())
    {
        if !com1.contains(needle) {
            return Err(format!(
                "COM1, the bisection-mode loader's console, lacks {needle:?}"
            ));
        }
    }
    // R131-4: the no-OS-type line, found above, must have counted at least one descriptor. Checked
    // after the line itself, so a run whose line names an OS-defined type is refused for that.
    let counted = no_os_type_count(&com1);
    if !counted.is_some_and(|n| n > 0) {
        return Err(format!(
            "the bisection mode's no-OS-type line counted no descriptor: {counted:?}"
        ));
    }
    if com1.contains(LAST_LOADER_LINE) {
        return Err(format!(
            "the bisection-mode loader wrote {LAST_LOADER_LINE:?}: it never enters the kernel"
        ));
    }
    if done.serial.contains("origin-kernel:") || com1.contains("origin-kernel:") {
        return Err("a kernel line was written: the bisection mode never enters the kernel".into());
    }
    Ok(())
}

/// How many descriptors the bisection mode's no-OS-type line says it counted, or `None` with no
/// such line or no number after it (TASKS item 131's review, R131-4).
fn no_os_type_count(com1: &str) -> Option<u32> {
    let at = com1.find(BIS_NO_OS_TYPE)? + BIS_NO_OS_TYPE.len();
    com1.get(at..)?.split_whitespace().next()?.parse().ok()
}

/// The lines on the SPCR's UART after the bisection mode's last line, empty ones left out.
fn after_bisect_last(done: &Completed) -> Result<Vec<String>> {
    let uart = serial::clean(done.serial.as_bytes());
    let lines: Vec<&str> = uart.lines().filter(|l| !l.is_empty()).collect();
    let last = lines
        .iter()
        .position(|l| *l == BIS_LAST)
        .ok_or("the SPCR's UART lacks the bisection mode's last line")?;
    Ok(lines[last + 1..].iter().map(ToString::to_string).collect())
}

/// The bisection mode's control: its line is the loader's last, and QEMU exits; no line straight
/// to the UART before the call, no mark and no last line.
fn check_bisect_control(done: &Completed, _settings: &Settings) -> Result {
    check_bisect_console(done, BIS_CONTROL)?;
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    let last = com1.lines().rev().find(|l| l.starts_with("origin-boot:"));
    if last != Some(BIS_CONTROL) {
        return Err(format!(
            "the loader's last line should be the control's: {last:?}"
        ));
    }
    let uart = serial::clean(done.serial.as_bytes());
    if uart.contains(BIS_LAST)
        || uart.contains(BEFORE_CALL)
        || uart.lines().any(|l| l == MARK_FIRST)
    {
        return Err("the line before the call, the mode's last line or its mark was written: the call was made".into());
    }
    if !done.exited {
        return Err(
            "QEMU did not exit after the control: the triple fault did not reset the machine"
                .into(),
        );
    }
    Ok(())
}

/// `decision` on the console, the line straight to the UART before the mode's last line, then
/// `marks` and nothing more; and QEMU exits within [`CF9_AT_ONCE`] of the last mark: the triple
/// fault, the first thing after the call.
fn check_bisect_call(done: &Completed, decision: &str, marks: &[&str]) -> Result {
    check_bisect_call_with(done, decision, marks, true)
}

/// [`check_bisect_call`], with `BENCH-FCH` (`switch`) or without it.
fn check_bisect_call_with(
    done: &Completed,
    decision: &str,
    marks: &[&str],
    switch: bool,
) -> Result {
    check_bisect_console_with(done, decision, switch)?;
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    if !com1.contains(UNDER_FIRMWARE) || !com1.contains(BIS_LAST) {
        return Err(format!(
            "COM1 lacks {UNDER_FIRMWARE:?} or the bisection mode's last line"
        ));
    }
    let uart = serial::clean(done.serial.as_bytes());
    match (uart.find(BEFORE_CALL), uart.find(BIS_LAST)) {
        (Some(before), Some(last)) if before < last => {}
        _ => {
            return Err(format!(
                "the SPCR's UART lacks {BEFORE_CALL:?} before the bisection mode's last line"
            ));
        }
    }
    let after = after_bisect_last(done)?;
    if after.iter().map(String::as_str).ne(marks.iter().copied()) {
        return Err(format!(
            "after the bisection mode's last line the SPCR's UART should hold {marks:?}; it holds {after:?}"
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

fn check_bisect_mine(done: &Completed, _settings: &Settings) -> Result {
    check_bisect_call(done, BIS_MINE, &[MARK_FIRST])
}

fn check_bisect_no_record(done: &Completed, _settings: &Settings) -> Result {
    check_bisect_call(done, BIS_NO_RECORD, &[MARK_FIRST])
}

fn check_bisect_retry(done: &Completed, _settings: &Settings) -> Result {
    check_bisect_call(done, BIS_MINE, &[MARK_FIRST, MARK_RETRY])
}

fn check_bisect_bits_clear(done: &Completed, _settings: &Settings) -> Result {
    check_bisect_call(done, BIS_NO_RECORD_BITS_CLEAR, &[MARK_FIRST])
}

/// R131-5: the rental's path, with no `BENCH-FCH`.
fn check_bisect_no_switch(done: &Completed, _settings: &Settings) -> Result {
    check_bisect_call_with(done, BIS_NOT_READ, &[MARK_FIRST], false)
}

/// R131-5: the control withheld, as the shutdown message is off; the call, and the triple fault
/// after it (QEMU resets on one whatever the FCH's bits).
fn check_bisect_first_noshutdown(done: &Completed, _settings: &Settings) -> Result {
    check_bisect_call(done, BIS_BITS_CLEAR, &[MARK_FIRST])?;
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    if com1.contains(BIS_CONTROL) || com1.contains("the boot's first run, the control") {
        return Err("the control ran though the shutdown message is off".into());
    }
    Ok(())
}

/// `BENCH-AFTER kernel` read and said, its value ignored: the triple fault after the call, and
/// no kernel line (ruling Q3).
fn check_bisect_after_kernel(done: &Completed, _settings: &Settings) -> Result {
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    if !com1.contains(AFTER_KERNEL) {
        return Err(format!("COM1 lacks {AFTER_KERNEL:?}"));
    }
    check_bisect_call(done, BIS_MINE, &[MARK_FIRST])
}

/// Both calls given a stale key: `X`, `Y`, Amendment 2's line, then Amendment 3's order with no
/// `ResetSystem` (`BENCH-FCH`'s `noreset`), whose FADT step the mode skips at port `0xCF9`
/// (Amendment 6, step 5; ruling Q5): the halt, with no UART register read or write-back after the
/// call, and QEMU runs on.
fn check_bisect_both_failed(done: &Completed, _settings: &Settings) -> Result {
    check_bisect_console(done, BIS_MINE)?;
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    if !com1.contains(BIS_NO_RESET) {
        return Err(format!(
            "COM1 lacks {BIS_NO_RESET:?}: noreset was not taken"
        ));
    }
    let after = after_bisect_last(done)?;
    let want = [
        MARK_FIRST,
        MARK_RETRY,
        HALTING,
        MIN_HALT_SOON,
        BIS_CF9_SKIPPED,
    ];
    if after.iter().map(String::as_str).ne(want) {
        return Err(format!(
            "after the bisection mode's last line the SPCR's UART should hold {want:?}; it holds {after:?}"
        ));
    }
    if done.exited {
        return Err("QEMU exited: something reset the machine, though the mode skips the FADT's write to 0xCF9 and halts".into());
    }
    Ok(())
}

/// A bench kernel under the normal loader: no breadcrumb anywhere, the line saying so, and the
/// boot otherwise as `boot-console`'s SPCR scenario requires.
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

/// `cargo xtask boot-bench`: Clippy over the bench code, the bench pair and the minimal-mode loader
/// built with their machine-code checks, and the scenarios booted (part of `boot-matrix`).
pub(crate) fn boot(settings: &Settings) -> Result {
    let started = Instant::now();
    clippy()?;
    let (loader, kernel) = build()?;
    check_reset_after_call(&loader)?;
    // TASKS item 128: the minimal-mode loader, with its machine-code checks.
    build_minimal()?;
    // TASKS item 131: the bisection-mode loader, with its machine-code checks.
    build_bisect()?;
    // TASKS item 133: the kernel-entry loader, with its machine-code checks.
    build_kernel()?;
    let kernel = read_file(&kernel)?;
    let all: Vec<(Scenario, Vec<u8>)> = scenarios()
        .into_iter()
        .map(|scenario| (scenario, kernel.clone()))
        .collect();
    // `ORIGIN_BOOT_TEST_ONLY` picks scenarios by label, as for `boot-console`.
    let chosen = crate::select_scenarios(&all, &settings.only)?;
    for (scenario, kernel) in &chosen {
        run_scenario(scenario, kernel, settings, None)?;
    }
    println!(
        "xtask: boot-bench passed ({} of {} scenarios: the firmware's key, a stale key once, twice, the bench kernel under the normal loader, the reset through ResetSystem, through the FADT's register, a disturbed UART written back, and the FCH's nine: the control, 0xCF9 after the call, the triple fault, the fall-through, no record, 0xCF9 only after the retry succeeds, the normal path after both calls fail, a contradicting layout, and a control that does not reset; and the minimal mode's six: the control, a returning call, no record, the retry, both calls failing with the FADT's write to 0xCF9 skipped, and the triple fault with the shutdown bits clear; and the bisection mode's nine: the control, a returning call through the copy, no record, the retry, both calls failing with the FADT's write to 0xCF9 skipped, the shutdown bits clear, no BENCH-FCH, the control withheld, and BENCH-AFTER kernel ignored; and the kernel-entry mode's five: the bench kernel entered with no switch, after the retry, with BENCH-AFTER fadt ignored and with BENCH-FCH's first ignored, and the halt after both calls fail) in {} s",
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
        let (flag, files) = FLAGS[4];
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
        // TASKS item 131 (R131-11): nor a bisection-mode build's.
        let bisect = bisect_target().join("xtask").join("BOOTX64.EFI");
        assert!(refuse(&bisect, "signing").is_err());
        // TASKS item 133: nor a kernel-entry build's.
        let kernel_entry = kernel_target().join("xtask").join("BOOTX64.EFI");
        assert!(refuse(&kernel_entry, "signing").is_err());
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
        // A write to a control register or the stack pointer is no read (R128-8).
        for write in [
            "mov\tcr3, rax",
            "mov\trsp, rbp",
            "mov\tdr7, rax",
            "mov\tss, ax",
        ] {
            let listing = good.replace(
                "140003e5c:\tpush",
                &format!("140003e5b:\t{write}\n140003e5c:\tpush"),
            );
            assert!(!triple_fault_after_call(&listing), "{write}");
        }
        assert!(
            is_general_register("r12d") && is_general_register("rax") && is_general_register("dil")
        );
        assert!(
            !is_general_register("rsp")
                && !is_general_register("r16")
                && !is_general_register("cr0")
        );
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
        more.push("boot/uefi/src/kernel.rs".into());
        let err = check_found(flag, files, &more).unwrap_err();
        assert!(
            err.contains("boot/uefi/src/kernel.rs names the minimal mode's flag"),
            "{err}"
        );
        // Each file naming the minimal flag names the bench flag too, so the bench list holds it.
        for (path, _) in MINIMAL_FILES {
            assert!(FILES.iter().any(|(p, _)| p == path), "{path}");
        }
    }

    #[test]
    fn f1_holds_the_bisection_flag_to_its_own_list() {
        let (flag, files) = FLAGS[2];
        assert_eq!(flag, BISECT_CFG);
        let listed: Vec<String> = files.iter().map(|(p, _)| (*p).to_string()).collect();
        assert_eq!(check_found(flag, files, &listed), Ok(()));
        let mut more = listed.clone();
        more.push("boot/uefi/src/kernel.rs".into());
        let err = check_found(flag, files, &more).unwrap_err();
        assert!(
            err.contains("boot/uefi/src/kernel.rs names the bisection mode's flag"),
            "{err}"
        );
        // Each file naming the bisection flag names the bench flag too, so the bench list holds it.
        for (path, _) in BISECT_FILES {
            assert!(FILES.iter().any(|(p, _)| p == path), "{path}");
        }
    }

    #[test]
    fn f1_holds_the_kernel_entry_flag_to_its_own_list() {
        let (flag, files) = FLAGS[3];
        assert_eq!(flag, KERNEL_CFG);
        let listed: Vec<String> = files.iter().map(|(p, _)| (*p).to_string()).collect();
        assert_eq!(check_found(flag, files, &listed), Ok(()));
        let mut more = listed.clone();
        more.push("boot/uefi/src/bench.rs".into());
        let err = check_found(flag, files, &more).unwrap_err();
        assert!(
            err.contains("boot/uefi/src/bench.rs names the kernel-entry mode's flag"),
            "{err}"
        );
        for (path, _) in KERNEL_FILES {
            assert!(FILES.iter().any(|(p, _)| p == path), "{path}");
        }
    }

    #[test]
    fn an_allocation_of_an_old_os_defined_type_is_found() {
        let insns = instructions(OLD_ALLOCATIONS);
        let found = os_type_allocations(&insns);
        assert_eq!(
            found.iter().map(|&i| insns[i].at).collect::<Vec<_>>(),
            [0x1_4000_a01f, 0x1_4000_a143, 0x1_4001_1c24, 0x1_4001_1c2b]
        );
        // As Amendment 7 compiles them, `EfiLoaderData` (2), and no translation of the old types:
        // nothing found.
        let now: String = OLD_ALLOCATIONS
            .replace("mov\tedx, 0x80000003", "mov\tedx, 0x2")
            .replace("mov\tedx, 0x80000002", "mov\tedx, 0x2")
            .lines()
            .filter(|l| !l.contains("cmp"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(
            os_type_allocations(&instructions(&now)),
            Vec::<usize>::new()
        );
        // R133-5's other shapes: another register, a store, an address; and longer numbers that
        // only begin with the type (an EFI status, say), which are not it.
        for (shape, found) in [
            ("mov\tebx, 0x80000003", 1),
            ("mov\tdword ptr [rbp - 0x20], 0x80000002", 1),
            ("lea\trdx, [rbx + 0x80000003]", 1),
            ("mov\trax, 0x8000000200000000", 0),
            ("mov\trax, 0x800000030", 0),
        ] {
            let listing = format!("140001000:\t{shape}\n");
            assert_eq!(
                os_type_allocations(&instructions(&listing)).len(),
                found,
                "{shape}"
            );
        }
    }

    /// TASKS item 133's ruling Q4, the positive control on a real image: the batch's loader before
    /// Amendment 7, whose path `ORIGIN_OS_TYPES_CONTROL` names, is refused. Run by hand in the
    /// task's turn: `ORIGIN_OS_TYPES_CONTROL=<loader> cargo test -p xtask -- --ignored
    /// the_batch_s_loader_is_refused_for_its_old_types`.
    #[test]
    #[ignore = "needs a loader built before ADR-0002 Amendment 7, named by ORIGIN_OS_TYPES_CONTROL"]
    fn the_batch_s_loader_is_refused_for_its_old_types() {
        let path = std::env::var("ORIGIN_OS_TYPES_CONTROL").expect("ORIGIN_OS_TYPES_CONTROL");
        let err = check_no_os_types(Path::new(&path)).unwrap_err();
        assert!(
            err.contains("0x80000003") && err.contains("0x80000002"),
            "{err}"
        );
    }

    /// TASKS item 131, ruling Q9: the minimal-mode loader's call, as `llvm-objdump` printed it at
    /// `32b6e043`: the boot services table's address read from the system table's member at
    /// `0x60`, then `ExitBootServices` from `0xe8`.
    const LIVE_CALL: &str = "140001759:\tmov\trcx, qword ptr [rdi + 0x18]\n14000175d:\tmov\trcx, qword ptr [rcx]\n140001760:\tmov\trcx, qword ptr [rcx + 0x60]\n140001764:\ttest\trcx, rcx\n140001776:\tmov\tr8, qword ptr [rcx]\n140001779:\tmov\trdx, qword ptr [rcx + 0x10]\n1400017b6:\tmov\tr8, qword ptr [rcx + 0xe8]\n1400017bd:\ttest\tr8, r8\n1400017c2:\tmov\trax, qword ptr [rdi + 0x20]\n1400017c6:\tmov\trcx, qword ptr [rax]\n1400017c9:\tmov\trdx, rsi\n1400017cc:\tcall\tr8\n";
    /// The full bench loader's call at `32b6e043`: its target from a variable holding the
    /// pointer read from the loader's copy.
    const COPY_CALL: &str = "140003f03:\tmov\trax, qword ptr [rsi + 0x28]\n140003f07:\tmov\trcx, qword ptr [rsi + 0x30]\n140003f0b:\tmov\trcx, qword ptr [rcx]\n140003f0e:\tcall\tqword ptr [rax]\n";

    #[test]
    fn a_call_through_the_live_table_is_told_from_one_through_the_copy() {
        let call_at = |insns: &[Insn]| {
            insns
                .iter()
                .position(|i| i.text.starts_with("call "))
                .unwrap()
        };
        let live = instructions(LIVE_CALL);
        assert!(through_live_table(&live, call_at(&live)));
        let copy = instructions(COPY_CALL);
        assert!(!through_live_table(&copy, call_at(&copy)));
        // The memory-operand form of the live call.
        let direct = instructions(
            &LIVE_CALL
                .replace(
                    "1400017cc:\tcall\tr8",
                    "1400017cc:\tcall\tqword ptr [rcx + 0xe8]",
                )
                .replace("1400017c6:\tmov\trcx, qword ptr [rax]\n", ""),
        );
        assert!(through_live_table(&direct, call_at(&direct)));
        // ExitBootServices' offset from a table whose address did not come from the system
        // table's member: the loader's own copy, on the stack or elsewhere.
        let stack =
            instructions("140001000:\tmov\tr8, qword ptr [rsp + 0xe8]\n140001008:\tcall\tr8\n");
        assert!(!through_live_table(&stack, call_at(&stack)));
        let other = instructions(&LIVE_CALL.replace(
            "140001760:\tmov\trcx, qword ptr [rcx + 0x60]",
            "140001760:\tmov\trcx, qword ptr [rcx + 0x58]",
        ));
        assert!(!through_live_table(&other, call_at(&other)));
        // A later write to the call's register is the one that counts.
        let rewritten = instructions(&LIVE_CALL.replace(
            "1400017c9:\tmov\trdx, rsi",
            "1400017c9:\tmov\tr8, qword ptr [rbp - 0x40]",
        ));
        assert!(!through_live_table(&rewritten, call_at(&rewritten)));
        assert!(!through_live_table(&[], 0));
        // R131-2: a live read of ExitBootServices anywhere, spilled before its call, is found.
        let spilled = instructions(&format!(
            "{}140001800:\tmov\tqword ptr [rsp + 0x40], r8\n140001900:\tmov\trax, qword ptr [rsp + 0x40]\n140001904:\tcall\tqword ptr [rax]\n",
            LIVE_CALL.replace("1400017cc:\tcall\tr8\n", "")
        ));
        assert!(!through_live_table(&spilled, call_at(&spilled)));
        assert_eq!(live_exit_loads(&spilled).len(), 1);
        assert_eq!(live_exit_loads(&copy), Vec::<usize>::new());
        // R131-3: ExitBootServices' arguments come from memory or registers; ResetSystem's first
        // two are constants, so a ResetSystem put before the triple fault is not taken for it.
        assert!(takes_exit_arguments(&live, call_at(&live)));
        assert!(takes_exit_arguments(&copy, call_at(&copy)));
        let reset = instructions(
            "140002000:\tmov\trax, qword ptr [rsi + 0x10]\n140002004:\txor\tecx, ecx\n140002006:\txor\tedx, edx\n140002008:\txor\tr8d, r8d\n14000200b:\txor\tr9d, r9d\n14000200e:\tcall\tqword ptr [rax]\n",
        );
        assert!(!takes_exit_arguments(&reset, call_at(&reset)));
        let one_constant = instructions(
            "140002000:\tmov\trax, qword ptr [rsi + 0x10]\n140002004:\tmov\trcx, qword ptr [rsi + 0x8]\n140002006:\tmov\tedx, 0x0\n14000200e:\tcall\tqword ptr [rax]\n",
        );
        assert!(!takes_exit_arguments(&one_constant, call_at(&one_constant)));
        // Instructions that only read the register do not stand for its load.
        let read = instructions(
            &LIVE_CALL.replace("1400017c9:\tmov\trdx, rsi", "1400017c9:\tcmp\tr8, rdx"),
        );
        assert!(through_live_table(&read, call_at(&read)));
    }

    #[test]
    fn the_no_os_type_lines_count_is_read() {
        let line = |rest: &str| format!("x\n{BIS_NO_OS_TYPE}{rest}\ny\n");
        assert_eq!(no_os_type_count(&line("135 descriptors")), Some(135));
        assert_eq!(no_os_type_count(&line("0 descriptors")), Some(0));
        assert_eq!(no_os_type_count(&line("")), None);
        // The line a planted OS-defined type gives (BC-1) is not the no-OS-type line.
        assert_eq!(
            no_os_type_count(
                "origin-boot: bench build: bisect: the memory map the page tables were built from holds 1 region(s) of an OS-defined type (0x80000000 and above), the first 0x80000003, in 135 descriptors"
            ),
            None
        );
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
