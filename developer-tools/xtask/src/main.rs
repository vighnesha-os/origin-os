//! Origin OS build orchestration (host only; CLAUDE.md §70, handoff task T4).
//!
//! Each Origin artifact is built for its own target, which Cargo alone cannot express on
//! stable Rust (per-package targets are unstable). This tool runs the right command for
//! each crate. It uses std only, no third-party crates. Run it through the alias in
//! `.cargo/config.toml`: `cargo xtask <command>`.
#![forbid(unsafe_code)]

mod serial;

use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  check       rustfmt, clippy and unit tests, each crate on its own target(s);
              the UEFI release build; the CLAUDE.md §11 cfg lint
  build       build the loader (developer mode) and the KRN kernel image; print their paths
  boot-test   boot under QEMU + OVMF twice: the kernel must start and report; a tampered
              kernel image must be rejected by the loader
  run         boot under QEMU with the serial console on this terminal (Ctrl-A X quits)

environment:
  ORIGIN_OVMF_CODE, ORIGIN_OVMF_VARS  firmware code image and NVRAM template
                                      (default: the EDK2 files installed next to QEMU)
  ORIGIN_BOOT_TEST_TIMEOUT            boot-test timeout in seconds (default 120)
  ORIGIN_QEMU_DEBUG                   if set, QEMU logs interrupts and resets to
                                      target/xtask/qemu-debug.log";

const UEFI_TARGET: &str = "x86_64-unknown-uefi";
const LOADER_PACKAGE: &str = "origin-boot-uefi";
const LOADER_BIN: &str = "origin-boot";
const VERSION_PACKAGE: &str = "origin-version";
const ONBF_FORMAT_PACKAGE: &str = "onbf-format";
const CRYPTO_PACKAGE: &str = "origin-crypto";
const MERKLE_PACKAGE: &str = "onbf-merkle";
const KERNEL_TARGET: &str = "x86_64-unknown-none";
/// `no_std` libraries embedded in the boot loader and the Framekernel. Each must be
/// clippy-clean on every target and free of panic paths (ONBF §12).
const BOOT_INFO_PACKAGE: &str = "origin-boot-info";
const KERNEL_PACKAGE: &str = "framekernel";
const PACK_PACKAGE: &str = "onbf-pack";
const FREESTANDING_LIBS: &[&str] = &[
    BOOT_INFO_PACKAGE,
    ONBF_FORMAT_PACKAGE,
    MERKLE_PACKAGE,
    CRYPTO_PACKAGE,
];
const QEMU: &str = "qemu-system-x86_64";
const DEFAULT_BOOT_TEST_TIMEOUT: Duration = Duration::from_secs(120);

/// One QEMU boot the test runs, and what its serial output must and must not contain.
struct Scenario {
    label: &'static str,
    /// Lines required, in this order.
    markers: Vec<String>,
    /// Output that fails the scenario as soon as it appears.
    failures: Vec<&'static str>,
}

/// The good path: the loader verifies and enters the kernel, and the kernel reports on
/// COM1. The banners are built from the same generated constants as the binaries, so the
/// test also checks that the release identity reached both.
/// Processors QEMU emulates: `ORIGIN_QEMU_SMP` (1–64), or 2.
fn qemu_cpus() -> u32 {
    std::env::var("ORIGIN_QEMU_SMP")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| (1..=64).contains(n))
        .unwrap_or(2)
}

fn boot_scenario() -> Scenario {
    let mut scenario = boot_scenario_markers();
    // With more than one processor, the first AP's start-up line comes right after the
    // MADT line.
    if qemu_cpus() > 1
        && let Some(at) = scenario
            .markers
            .iter()
            .position(|m| m.starts_with("origin-kernel: ACPI MADT: "))
    {
        scenario.markers.insert(
            at + 1,
            "origin-kernel: CPU 1 (APIC ID 1) online after ".into(),
        );
    }
    scenario
}

fn boot_scenario_markers() -> Scenario {
    use origin_version::{BUILD, PRODUCT, VERSION};
    let cpus = qemu_cpus();
    Scenario {
        label: "kernel boot",
        markers: vec![
            format!("{PRODUCT} {VERSION} UEFI loader (build {BUILD})"),
            "origin-boot: UEFI 2.".into(),
            "origin-boot: read \\EFI\\ORIGIN\\KERNEL.ONBF (".into(),
            "origin-boot: developer mode: unsigned kernel images are accepted".into(),
            "origin-boot: kernel verified: ".into(),
            "origin-boot: kernel placed at ".into(),
            "origin-boot: exiting boot services; entering the Framekernel".into(),
            format!("origin-kernel: {PRODUCT} {VERSION} Framekernel (build {BUILD})"),
            format!(
                "origin-kernel: boot info {}.{}, ",
                origin_version::BOOT_INFO_MAJOR,
                origin_version::BOOT_INFO_MINOR
            ),
            "origin-kernel: loaded at ".into(),
            // Boot info 0.2: the verified segment table, in RVA order (R, RX, RW).
            "origin-kernel: kernel segment 0: R at ".into(),
            "origin-kernel: kernel segment 1: RX at ".into(),
            "origin-kernel: kernel segment 2: RW at ".into(),
            "origin-kernel: GDT, TSS and IDT loaded (32 exception vectors)".into(),
            "origin-kernel: exception 3 (#BP breakpoint) at rip ".into(),
            "origin-kernel: breakpoint handled; resuming".into(),
            "origin-kernel: frame allocator: ".into(),
            "origin-kernel: page tables loaded: ".into(),
            "origin-kernel: page protection: write to kernel code at ".into(),
            "faulted (#PF error 0x3)".into(),
            "origin-kernel: page protection: write to read-only kernel data at ".into(),
            "origin-kernel: page protection: instruction fetch from writable data at ".into(),
            "faulted (#PF error 0x11)".into(),
            "origin-kernel: page protection: read of page 0 at 0x0 faulted (#PF error 0x0)".into(),
            "origin-kernel: page protection: read of the boot stack's guard page at ".into(),
            // After the switch, so this also proves the direct map.
            "origin-kernel: allocated, wrote and freed 3 frames at ".into(),
            "origin-kernel: heap: Vec of 100000 u64, 2000 boxes, B-tree of 1000 keys, a string: "
                .into(),
            "origin-kernel: heap: everything freed; ".into(),
            "origin-kernel: page protection: SMEP: supervisor fetch from a user page at 0x7fffffffe000 faulted (#PF error 0x11)".into(),
            "origin-kernel: page protection: SMAP: supervisor read of a user page at 0x7fffffffe000 faulted (#PF error 0x1)".into(),
            "origin-kernel: x2APIC enabled: APIC ID 0, ".into(),
            "origin-kernel: APIC timer calibrated against the PIT: ".into(),
            "origin-kernel: APIC timer: 50 interrupts at 100 Hz in ".into(),
            // M3: every processor QEMU emulates (`-smp`, see `qemu_cpus`) comes online.
            format!("origin-kernel: ACPI MADT: {cpus} processors, {cpus} enabled"),
            format!("origin-kernel: SMP: {cpus} of {cpus} enabled processors online"),
            "origin-kernel: reclaimed ".into(),
            "origin-kernel: page protection: stack guard page at ".into(),
            // M3: two non-yielding threads per processor, all preempted and all running.
            "origin-kernel: scheduler: CPU 0: workers 0 and 1 counted ".into(),
            format!(
                "origin-kernel: preemptive multitasking: {} non-yielding threads on {cpus} CPUs all ran",
                2 * cpus
            ),
            "origin-kernel: scheduler: yield alternated ".into(),
            "origin-kernel: scheduler: exited threads' stacks reaped on their own CPUs: heap ".into(),
            "origin-kernel: M1 entry complete; halting".into(),
        ],
        failures: vec![
            "origin-boot: panic:",
            "origin-kernel: panic:",
            "origin-boot: kernel rejected",
            "origin-kernel: invalid boot info",
            "origin-kernel: invalid image",
            "(#DF double fault)",
            "(#GP general protection)",
            "(#PF page fault)",
            "(#UD invalid opcode)",
            "origin-kernel: no usable region can hold",
            "origin-kernel: frame allocator exhausted",
            "did not keep its contents",
            "could not be freed",
            "origin-kernel: page tables not built",
            "origin-kernel: page protection violated",
            "origin-kernel: the processor has no NX bit",
            "origin-kernel: no code or read-only segment to probe",
            "RWX at",
            "origin-kernel: the processor has no x2APIC",
            "origin-kernel: APIC timer calibration failed",
            ": APIC timer is wrong",
            "is unusable",
            "(reserved) at rip",
            "origin-kernel: heap contents are wrong",
            "origin-kernel: heap leaked",
            "origin-kernel: ACPI processor discovery failed",
            "origin-kernel: AP start-up not possible",
            ": AP start-up failed",
            ": scheduler failed",
            "supervisor isolation from user pages incomplete",
            "origin-kernel: cannot map the user probe page",
        ],
    }
}

/// The tampered path: one byte of a SEGMENT changed on disk. The loader must refuse it, and
/// the kernel must never run.
fn tamper_scenario() -> Scenario {
    Scenario {
        label: "tampered kernel",
        markers: vec![
            "origin-boot: read \\EFI\\ORIGIN\\KERNEL.ONBF (".into(),
            "origin-boot: kernel rejected: section ".into(),
            "does not match its digest".into(),
        ],
        failures: vec![
            "origin-kernel:",
            "origin-boot: panic:",
            "origin-boot: kernel verified",
        ],
    }
}

type Result<T = ()> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["check"] => check(),
        ["build"] => build_loader().and_then(|loader| {
            let kernel = pack_kernel(&build_kernel()?)?;
            println!("{}\n{}", loader.display(), kernel.display());
            Ok(())
        }),
        ["boot-test"] => boot_test(),
        ["run"] => run(),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: error: {e}");
            ExitCode::FAILURE
        }
    }
}

// ------------------------------------------------------------------ paths and commands

/// The repository root: two levels above this crate's manifest directory.
fn repo_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .map_or_else(|| manifest.to_path_buf(), Path::to_path_buf)
}

fn target_dir() -> PathBuf {
    env::var_os("CARGO_TARGET_DIR").map_or_else(|| repo_root().join("target"), PathBuf::from)
}

/// `cargo`, as the toolchain that runs this xtask (rust-toolchain.toml pins 1.99.1).
fn cargo() -> Command {
    let mut c = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    c.current_dir(repo_root());
    c
}

/// Runs `cmd` with inherited stdio and fails unless it exits successfully.
fn run_step(cmd: &mut Command) -> Result {
    let shown = std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    println!("xtask: $ {shown}");
    let status = cmd
        .status()
        .map_err(|e| format!("cannot run `{shown}`: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{shown}` failed ({status})"))
    }
}

// ------------------------------------------------------------------ check

fn check() -> Result {
    let steps: &[&[&str]] = &[
        &["fmt", "--all", "--", "--check"],
        &[
            "clippy",
            "--package",
            "xtask",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        // The loader library on the host, as its unit tests see it.
        &[
            "clippy",
            "--package",
            LOADER_PACKAGE,
            "--lib",
            "--tests",
            "--",
            "-D",
            "warnings",
        ],
        // The library and the EFI binary on the real target.
        &[
            "clippy",
            "--package",
            LOADER_PACKAGE,
            "--target",
            UEFI_TARGET,
            "--",
            "-D",
            "warnings",
        ],
        // The generated release identity: library, tests and build script.
        &[
            "clippy",
            "--package",
            VERSION_PACKAGE,
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        // The KRN packer (a host tool): library, CLI and integration tests.
        &[
            "clippy",
            "--package",
            PACK_PACKAGE,
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        &["test", "--package", "xtask"],
        &["test", "--package", LOADER_PACKAGE, "--lib"],
        &["test", "--package", VERSION_PACKAGE],
        &["test", "--package", PACK_PACKAGE],
    ];
    for step in steps {
        run_step(cargo().args(*step))?;
    }
    // Libraries that run inside the boot loader and the Framekernel: clippy on the host
    // (with tests) and on both freestanding targets, then their tests. Their no-panic lints
    // are active in every non-test build.
    for &package in FREESTANDING_LIBS {
        run_step(cargo().args([
            "clippy",
            "--package",
            package,
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ]))?;
        for target in [UEFI_TARGET, KERNEL_TARGET] {
            run_step(cargo().args([
                "clippy",
                "--package",
                package,
                "--lib",
                "--target",
                target,
                "--",
                "-D",
                "warnings",
            ]))?;
        }
        run_step(cargo().args(["test", "--package", package]))?;
    }
    // The Framekernel: its library on the host (with tests), the binary on its target.
    run_step(cargo().args([
        "clippy",
        "--package",
        KERNEL_PACKAGE,
        "--lib",
        "--tests",
        "--",
        "-D",
        "warnings",
    ]))?;
    run_step(cargo().args([
        "clippy",
        "--package",
        KERNEL_PACKAGE,
        "--target",
        KERNEL_TARGET,
        "--",
        "-D",
        "warnings",
    ]))?;
    run_step(cargo().args(["test", "--package", KERNEL_PACKAGE, "--lib"]))?;
    // The loader in developer mode too (the boot test's variant).
    run_step(cargo().args([
        "clippy",
        "--package",
        LOADER_PACKAGE,
        "--target",
        UEFI_TARGET,
        "--features",
        "developer-mode",
        "--",
        "-D",
        "warnings",
    ]))?;
    build_loader()?;
    build_kernel()?;
    no_panic_check()?;
    linux_cfg_lint()?;
    manifest_version_lint()?;
    println!("xtask: check passed");
    Ok(())
}

/// External symbols a freestanding library's release object may reference. Anything else,
/// above all a `core::panicking`, bounds-check or `copy_from_slice` length routine, is a
/// panic path, which ONBF §12 forbids. Lints alone cannot show this; the emitted object can.
const NO_PANIC_ALLOWED: &[&str] = &["memcpy", "memmove", "memset", "memcmp"];
/// Allowed by substring of the mangled name. Each is a total `core` function that returns
/// rather than panics:
/// - `core::fmt::Formatter::write_str`, used by `Debug` impls;
/// - `core::str::from_utf8`, used by OCBOR-1 text validation. It returns `Err` for any
///   invalid input, is RFC 3629-strict, and under `forbid(unsafe_code)` it is the only way
///   to turn checked bytes into `&str`.
const NO_PANIC_ALLOWED_PARTS: &[&str] = &[
    "4core3fmtNtB5_9Formatter9write_str",
    "4core3str8converts9from_utf8",
];

/// Builds each freestanding library for the kernel target in release mode, as one object,
/// and fails if it references any symbol outside the allowlist. A symbol defined by another
/// freestanding library is allowed, because that library passes this same check.
fn no_panic_check() -> Result {
    let nm = llvm_tool("llvm-nm")?;
    let out_dir = target_dir().join("xtask").join("no-panic");
    fs::create_dir_all(&out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    let mut objects = Vec::new();
    for &package in FREESTANDING_LIBS {
        objects.push((package, build_kernel_object(package, &out_dir)?));
    }
    let symbols = |obj: &Path, flag: &str| -> Result<Vec<String>> {
        let out = Command::new(&nm)
            .arg(flag)
            .arg(obj)
            .output()
            .map_err(|e| format!("cannot run {}: {e}", nm.display()))?;
        // Lines are "[address] TYPE name"; the name is the last field.
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split_whitespace().last().map(str::to_string))
            .collect())
    };
    let mut defined = std::collections::HashSet::new();
    for (_, obj) in &objects {
        defined.extend(symbols(obj, "--defined-only")?);
    }
    for (package, obj) in &objects {
        let undefined = symbols(obj, "-u")?;
        let bad: Vec<&String> = undefined
            .iter()
            .filter(|s| {
                !NO_PANIC_ALLOWED.contains(&s.as_str())
                    && !NO_PANIC_ALLOWED_PARTS.iter().any(|p| s.contains(p))
                    && !defined.contains(*s)
            })
            .collect();
        if !bad.is_empty() {
            let list: Vec<&str> = bad.iter().map(|s| s.as_str()).collect();
            return Err(format!(
                "{package}: the release object for {KERNEL_TARGET} references symbols outside the \
                 no-panic allowlist (ONBF §12):\n  {}",
                list.join("\n  ")
            ));
        }
        println!(
            "xtask: no-panic check: {package} ({KERNEL_TARGET}, release) references {} external symbols, all allowed",
            undefined.len()
        );
    }
    Ok(())
}

/// Builds `package` for the kernel target in release mode as a single object file.
fn build_kernel_object(package: &str, out_dir: &Path) -> Result<PathBuf> {
    {
        let obj = out_dir.join(format!("{package}.o"));
        let _ = fs::remove_file(&obj);
        // Clean only this package for this target and profile, so rustc really runs and
        // writes a fresh object (Cargo would otherwise consider it up to date).
        run_step(cargo().args([
            "clean",
            "--quiet",
            "--package",
            package,
            "--release",
            "--target",
            KERNEL_TARGET,
        ]))?;
        let emit = format!("--emit=obj={}", obj.display());
        run_step(cargo().args([
            "rustc",
            "--quiet",
            "--package",
            package,
            "--lib",
            "--release",
            "--target",
            KERNEL_TARGET,
            "--",
            &emit,
            "-C",
            "codegen-units=1",
        ]))?;
        if !obj.is_file() {
            return Err(format!(
                "no-panic check: rustc did not write {}",
                obj.display()
            ));
        }
        Ok(obj)
    }
}

/// A tool from the pinned toolchain's `llvm-tools` component.
fn llvm_tool(name: &str) -> Result<PathBuf> {
    let rustc = |args: &[&str]| -> Result<String> {
        let out = Command::new("rustc")
            .args(args)
            .current_dir(repo_root())
            .output()
            .map_err(|e| format!("cannot run rustc: {e}"))?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let sysroot = rustc(&["--print", "sysroot"])?.trim().to_string();
    let host = rustc(&["-vV"])?
        .lines()
        .find_map(|l| l.strip_prefix("host: ").map(str::to_string))
        .ok_or("cannot read the host triple from `rustc -vV`")?;
    let path = Path::new(&sysroot)
        .join("lib/rustlib")
        .join(host)
        .join("bin")
        .join(name);
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "{} not found; is the llvm-tools component installed?",
            path.display()
        ))
    }
}

/// CLAUDE.md §62: a version is written by hand only in origin-version.toml. No Cargo
/// manifest may declare a package version, including `version.workspace = true`.
fn manifest_version_lint() -> Result {
    let root = repo_root();
    let mut manifests = Vec::new();
    collect_files(
        &root,
        &|_| false,
        &|name| name == "Cargo.toml",
        &mut manifests,
    )
    .map_err(|e| format!("version lint: {e}"))?;
    let mut hits = Vec::new();
    for file in &manifests {
        let text = fs::read_to_string(file)
            .map_err(|e| format!("version lint: {}: {e}", file.display()))?;
        let mut section = String::new();
        for (n, line) in text.lines().enumerate() {
            let t = line.trim();
            if t.starts_with('[') {
                section = t.trim_matches(|c| c == '[' || c == ']').trim().to_string();
                continue;
            }
            let key = t.split(['=', '.']).next().unwrap_or("").trim();
            if (section == "package" || section == "workspace.package") && key == "version" {
                let shown = file.strip_prefix(&root).unwrap_or(file);
                hits.push(format!("{}:{}: {t}", shown.display(), n + 1));
            }
        }
    }
    println!(
        "xtask: §62 version lint scanned {} Cargo manifests",
        manifests.len()
    );
    if hits.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "crate versions must come from origin-version.toml, not Cargo manifests (CLAUDE.md §62):\n{}",
            hits.join("\n")
        ))
    }
}

/// CLAUDE.md §11: Origin code compiles for the `x86_64-unknown-linux-gnu` triple only as a
/// code generator, so it must never use `cfg(target_os = "linux")`. Host developer tools
/// (`developer-tools/`) run on Linux or macOS build hosts and are exempt.
fn linux_cfg_lint() -> Result {
    let root = repo_root();
    let mut files = Vec::new();
    let dev_tools = root.join("developer-tools");
    collect_files(
        &root,
        &|dir| dir == dev_tools,
        &|name| name.ends_with(".rs"),
        &mut files,
    )
    .map_err(|e| format!("cfg lint: {e}"))?;
    let mut hits = Vec::new();
    for file in &files {
        let text =
            fs::read_to_string(file).map_err(|e| format!("cfg lint: {}: {e}", file.display()))?;
        for (n, line) in text.lines().enumerate() {
            let squeezed: String = line.chars().filter(|c| !c.is_whitespace()).collect();
            if squeezed.contains("target_os=\"linux\"") {
                let shown = file.strip_prefix(&root).unwrap_or(file);
                hits.push(format!("{}:{}: {}", shown.display(), n + 1, line.trim()));
            }
        }
    }
    println!(
        "xtask: §11 cfg lint scanned {} Rust files outside developer-tools/",
        files.len()
    );
    if hits.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "target_os = \"linux\" is not allowed in Origin code (CLAUDE.md §11):\n{}",
            hits.join("\n")
        ))
    }
}

/// Collects files under `dir` whose name satisfies `want`, in sorted order. Hidden
/// directories, `target/` and any directory for which `skip` returns true are not entered.
fn collect_files(
    dir: &Path,
    skip: &dyn Fn(&Path) -> bool,
    want: &dyn Fn(&str) -> bool,
    out: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    let mut entries = fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type()?.is_dir() {
            if !(name.starts_with('.') || name == "target" || skip(&path)) {
                collect_files(&path, skip, want, out)?;
            }
        } else if want(&name) {
            out.push(path);
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ build

/// Builds the EFI application and returns the path of `origin-boot.efi`.
fn build_loader() -> Result<PathBuf> {
    run_step(cargo().args([
        "build",
        "--package",
        LOADER_PACKAGE,
        "--bin",
        LOADER_BIN,
        "--target",
        UEFI_TARGET,
        "--release",
        "--features",
        "developer-mode",
    ]))?;
    let efi = target_dir()
        .join(UEFI_TARGET)
        .join("release")
        .join(format!("{LOADER_BIN}.efi"));
    if efi.is_file() {
        Ok(efi)
    } else {
        Err(format!(
            "cargo reported success but {} does not exist",
            efi.display()
        ))
    }
}

/// Builds the Framekernel ELF for `x86_64-unknown-none` and returns its path.
fn build_kernel() -> Result<PathBuf> {
    run_step(cargo().args([
        "build",
        "--package",
        KERNEL_PACKAGE,
        "--bin",
        KERNEL_PACKAGE,
        "--target",
        KERNEL_TARGET,
        "--release",
    ]))?;
    let elf = target_dir()
        .join(KERNEL_TARGET)
        .join("release")
        .join(KERNEL_PACKAGE);
    if elf.is_file() {
        Ok(elf)
    } else {
        Err(format!(
            "cargo reported success but {} does not exist",
            elf.display()
        ))
    }
}

/// Converts the kernel ELF into the KRN image with `onbf-pack`, which verifies its output
/// before writing it.
fn pack_kernel(elf: &Path) -> Result<PathBuf> {
    let out = target_dir().join("xtask").join("KERNEL.ONBF");
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let (Some(input), Some(output)) = (elf.to_str(), out.to_str()) else {
        return Err("kernel paths are not UTF-8".into());
    };
    run_step(cargo().args([
        "run",
        "--quiet",
        "--release",
        "--package",
        PACK_PACKAGE,
        "--",
        "krn",
        "--input",
        input,
        "--output",
        output,
    ]))?;
    Ok(out)
}

/// A copy of `krn` with one byte flipped in the middle of the first SEGMENT's content.
fn tampered(krn: &[u8]) -> Result<Vec<u8>> {
    let options = onbf_format::ParseOptions {
        developer_mode: true,
        ..Default::default()
    };
    let image = onbf_format::Image::parse(krn, options).map_err(|e| format!("KRN image: {e:?}"))?;
    let segment = image
        .sections()
        .find(|s| s.entry.kind() == onbf_format::Kind::Segment && s.entry.stored_size > 0)
        .ok_or("the KRN image has no segment with content")?;
    let at = usize::try_from(segment.entry.file_offset + segment.entry.stored_size / 2)
        .map_err(|_| "segment offset does not fit in usize")?;
    let mut out = krn.to_vec();
    let byte = out.get_mut(at).ok_or("segment outside the file")?;
    *byte ^= 0x01;
    Ok(out)
}

// ------------------------------------------------------------------ QEMU

/// An EDK2 x86-64 firmware code image and the NVRAM template that belongs to it.
struct Firmware {
    code: PathBuf,
    vars_template: PathBuf,
}

/// Everything one QEMU boot needs, staged under `target/xtask/`.
struct Staged {
    firmware: Firmware,
    esp: PathBuf,
    vars: PathBuf,
}

fn find_firmware() -> Result<Firmware> {
    let (code, vars) = match (
        env::var_os("ORIGIN_OVMF_CODE"),
        env::var_os("ORIGIN_OVMF_VARS"),
    ) {
        (Some(code), Some(vars)) => (PathBuf::from(code), PathBuf::from(vars)),
        (None, None) => {
            // Homebrew (and a QEMU source install) keep EDK2 firmware in <prefix>/share/qemu,
            // beside bin/. The x86_64 descriptor there (firmware/60-edk2-x86_64.json) pairs
            // edk2-x86_64-code.fd with the edk2-i386-vars.fd NVRAM template.
            let qemu = find_on_path(QEMU).ok_or_else(|| format!("{QEMU} is not on PATH"))?;
            let real = fs::canonicalize(&qemu).map_err(|e| format!("{}: {e}", qemu.display()))?;
            let share = real
                .parent()
                .and_then(Path::parent)
                .map(|prefix| prefix.join("share").join("qemu"))
                .ok_or_else(|| format!("cannot derive the QEMU prefix from {}", real.display()))?;
            (
                share.join("edk2-x86_64-code.fd"),
                share.join("edk2-i386-vars.fd"),
            )
        }
        _ => return Err("set both ORIGIN_OVMF_CODE and ORIGIN_OVMF_VARS, or neither".into()),
    };
    for p in [&code, &vars] {
        if !p.is_file() {
            return Err(format!(
                "firmware file not found: {} (set ORIGIN_OVMF_CODE and ORIGIN_OVMF_VARS)",
                p.display()
            ));
        }
    }
    Ok(Firmware {
        code,
        vars_template: vars,
    })
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// Builds the loader and lays out a fresh ESP, holding `kernel` as the KRN image, and a
/// fresh NVRAM copy for one boot.
fn stage(kernel: &[u8]) -> Result<Staged> {
    let efi = build_loader()?;
    let firmware = find_firmware()?;
    let work = target_dir().join("xtask");
    let esp = work.join("esp");
    if esp.exists() {
        fs::remove_dir_all(&esp).map_err(|e| format!("{}: {e}", esp.display()))?;
    }
    // The removable-media default boot path for x64 (UEFI 2.11 §3.5.1.1).
    let boot_dir = esp.join("EFI").join("BOOT");
    fs::create_dir_all(&boot_dir).map_err(|e| format!("{}: {e}", boot_dir.display()))?;
    fs::copy(&efi, boot_dir.join("BOOTX64.EFI"))
        .map_err(|e| format!("copy {}: {e}", efi.display()))?;
    // Where the loader looks for the Framekernel (ADR-0002 B1).
    let origin_dir = esp.join("EFI").join("ORIGIN");
    fs::create_dir_all(&origin_dir).map_err(|e| format!("{}: {e}", origin_dir.display()))?;
    fs::write(origin_dir.join("KERNEL.ONBF"), kernel)
        .map_err(|e| format!("write KERNEL.ONBF: {e}"))?;
    // A fresh NVRAM copy for every boot, so no run inherits variables from an earlier one.
    let vars = work.join("ovmf-vars.fd");
    fs::copy(&firmware.vars_template, &vars)
        .map_err(|e| format!("copy {}: {e}", firmware.vars_template.display()))?;
    Ok(Staged {
        firmware,
        esp,
        vars,
    })
}

/// One `-drive` value. QEMU option syntax escapes a literal comma as ",,".
fn drive(options: &str, path: &Path) -> Result<String> {
    let p = path
        .to_str()
        .ok_or_else(|| format!("path is not UTF-8: {}", path.display()))?;
    Ok(format!("{options}{}", p.replace(',', ",,")))
}

/// The machine every boot uses: q35 (the only machine type the Secure Boot firmware
/// supports), x86-64 with every feature TCG can emulate, and no network or display.
fn qemu_command(s: &Staged) -> Result<Command> {
    let mut c = Command::new(QEMU);
    c.args(["-machine", "q35", "-cpu", "max", "-m", "512M", "-smp"])
        .arg(qemu_cpus().to_string())
        .arg("-drive")
        .arg(drive(
            "if=pflash,format=raw,readonly=on,file=",
            &s.firmware.code,
        )?)
        .arg("-drive")
        .arg(drive("if=pflash,format=raw,file=", &s.vars)?)
        .arg("-drive")
        .arg(drive("format=raw,file=fat:rw:", &s.esp)?)
        .args(["-net", "none", "-display", "none", "-no-reboot"]);
    // Opt-in diagnostics: every interrupt and CPU reset, for debugging a triple fault.
    if env::var_os("ORIGIN_QEMU_DEBUG").is_some() {
        let log = target_dir().join("xtask").join("qemu-debug.log");
        c.arg("-d")
            .arg("int,cpu_reset,guest_errors")
            .arg("-D")
            .arg(log);
    }
    Ok(c)
}

fn boot_test_timeout() -> Result<Duration> {
    match env::var("ORIGIN_BOOT_TEST_TIMEOUT") {
        Ok(v) => v
            .parse::<u64>()
            .map(Duration::from_secs)
            .map_err(|_| format!("ORIGIN_BOOT_TEST_TIMEOUT must be whole seconds, got {v:?}")),
        Err(_) => Ok(DEFAULT_BOOT_TEST_TIMEOUT),
    }
}

fn boot_test() -> Result {
    let krn_path = pack_kernel(&build_kernel()?)?;
    let krn = fs::read(&krn_path).map_err(|e| format!("{}: {e}", krn_path.display()))?;
    run_scenario(&boot_scenario(), &krn)?;
    run_scenario(&tamper_scenario(), &tampered(&krn)?)?;
    println!("xtask: boot-test passed (both scenarios)");
    Ok(())
}

/// Boots `kernel` once and checks the serial output against `scenario`.
fn run_scenario(scenario: &Scenario, kernel: &[u8]) -> Result {
    let staged = stage(kernel)?;
    let timeout = boot_test_timeout()?;
    let mut cmd = qemu_command(&staged)?;
    cmd.args(["-serial", "stdio", "-monitor", "none"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    println!(
        "xtask: {}: booting under {QEMU} + OVMF (timeout {}s)",
        scenario.label,
        timeout.as_secs()
    );
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {QEMU}: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("QEMU stdout was not captured")?;
    let mut stderr = child.stderr.take().ok_or("QEMU stderr was not captured")?;

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n @ 1..) = stdout.read(&mut buf) {
            if tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let stderr_reader = thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    let markers: Vec<&str> = scenario.markers.iter().map(String::as_str).collect();
    let started = Instant::now();
    let deadline = started + timeout;
    let mut raw = Vec::new();
    let verdict: Result = loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(chunk) => {
                raw.extend_from_slice(&chunk);
                let text = serial::clean(&raw);
                // Failures are matched in complete lines only, so a failing line reaches the
                // log in full (its details follow the marker) before QEMU is stopped.
                let complete = text
                    .get(..text.rfind('\n').map_or(0, |i| i + 1))
                    .unwrap_or("");
                if let Some(m) = scenario.failures.iter().find(|m| complete.contains(**m)) {
                    break Err(format!("the serial output contains {m:?}"));
                }
                if serial::contains_in_order(&text, &markers) {
                    break Ok(());
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                break Err(format!(
                    "no complete loader output within {}s",
                    timeout.as_secs()
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                break Err("QEMU exited before the loader output was complete".into());
            }
        }
    };
    let elapsed = started.elapsed();

    // QEMU keeps running after the scenario ends (the kernel halts; a rejecting loader hands
    // back to the firmware), so the test stops it. (QEMU does not exit on SIGALRM.)
    let _ = child.kill();
    let exit = child.wait();
    let qemu_stderr = stderr_reader.join().unwrap_or_default();

    let text = serial::clean(&raw);
    let log = target_dir().join("xtask").join(format!(
        "boot-test-{}.log",
        scenario.label.replace(' ', "-")
    ));
    fs::write(&log, &text).map_err(|e| format!("{}: {e}", log.display()))?;

    match verdict {
        Ok(()) => {
            for line in text.lines().filter(|l| {
                l.starts_with("Origin OS")
                    || l.starts_with("origin-boot:")
                    || l.starts_with("origin-kernel:")
            }) {
                println!("  | {line}");
            }
            println!(
                "xtask: {}: passed in {:.1}s; serial log: {}",
                scenario.label,
                elapsed.as_secs_f64(),
                log.display()
            );
            Ok(())
        }
        Err(e) => {
            eprintln!("--- serial output ---\n{text}\n--- {QEMU} stderr ---\n{qemu_stderr}");
            Err(format!(
                "boot-test ({}) failed: {e} (QEMU: {exit:?}); serial log: {}",
                scenario.label,
                log.display()
            ))
        }
    }
}

fn run() -> Result {
    let krn_path = pack_kernel(&build_kernel()?)?;
    let krn = fs::read(&krn_path).map_err(|e| format!("{}: {e}", krn_path.display()))?;
    let staged = stage(&krn)?;
    let mut cmd = qemu_command(&staged)?;
    cmd.args(["-serial", "mon:stdio"]);
    println!("xtask: starting {QEMU}; Ctrl-A X quits, Ctrl-A C switches to the QEMU monitor");
    let status = cmd
        .status()
        .map_err(|e| format!("cannot start {QEMU}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{QEMU} exited with {status}"))
    }
}
