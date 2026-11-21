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
//! [`boot`] boots the bench pair under QEMU four ways, the SPCR naming COM2 and OVMF's console on
//! COM1: `ExitBootServices` given the firmware's key, a stale key once (the retry path, under
//! real firmware code), a stale key twice (the halt, which must say why and then nothing), and
//! the bench kernel under the normal developer-mode loader (no magic in RSI: no breadcrumbs, and
//! the boot otherwise unchanged).

use std::fs;
use std::path::{Path, PathBuf};
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

/// The extensions of the files F1 reads: code, build configuration and CI.
const SCANNED: &[&str] = &["rs", "toml", "yml", "yaml", "sh", "py", "json"];

/// Where bench builds go, apart from every other build.
pub(crate) fn target() -> PathBuf {
    crate::target_dir().join("bench")
}

/// F1: the bench flag's name appears only in [`FILES`], and in each of them.
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
    let found = files
        .iter()
        .filter_map(|path| {
            let text = fs::read_to_string(path).ok()?;
            let rel = path
                .strip_prefix(&root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            text.contains(CFG).then_some(rel)
        })
        .collect::<Vec<_>>();
    check_found(&found)?;
    println!(
        "xtask: bench gate (TASKS item 112, F1): the bench flag is named in {} listed files and nowhere else",
        found.len()
    );
    Ok(())
}

/// The heart of [`gate`]: `found` are the files that name the flag.
fn check_found(found: &[String]) -> Result {
    let listed: Vec<&str> = FILES.iter().map(|(path, _)| *path).collect();
    let mut problems: Vec<String> = found
        .iter()
        .filter(|path| !listed.contains(&path.as_str()))
        .map(|path| {
            format!("{path} names the bench flag `{CFG}`, and only listed files may (TASKS item 112, F1)")
        })
        .collect();
    problems.extend(
        listed
            .iter()
            .filter(|path| !found.iter().any(|f| f == *path))
            .map(|path| {
                format!(
                    "{path} is on the bench gate's list and no longer names `{CFG}`: take it off"
                )
            }),
    );
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// Whether `path` is under [`target`].
pub(crate) fn is_bench(path: &Path) -> bool {
    let root = target();
    path.starts_with(&root)
        || fs::canonicalize(path)
            .ok()
            .zip(fs::canonicalize(&root).ok())
            .is_some_and(|(path, root)| path.starts_with(root))
}

/// Fails if `path` is a bench build's file: no signing path takes one (TASKS item 112, ruling Q1).
pub(crate) fn refuse(path: &Path, what: &str) -> Result {
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
        println!(
            "xtask: bench-build: {}: {} bytes, SHA-256 {}",
            path.display(),
            bytes.len(),
            crate::reproducible::hex(&origin_crypto::Sha256::digest(&bytes), "")
        );
    }
    Ok((loader, kernel))
}

/// Whether `bytes` holds `needle`.
pub(crate) fn holds(bytes: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && bytes.windows(needle.len()).any(|w| w == needle)
}

/// Clippy over the bench flag's code, which no other Clippy run sees: the loader on its target,
/// and the kernel with the framework and boot info on theirs, into [`target`].
fn clippy() -> Result {
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

/// The SPCR naming COM2, as `boot-console`'s scenario gives it.
fn spcr(work: &Path) -> Result<Vec<String>> {
    crate::spcr_args(
        work,
        &crate::spcr_table(0, crate::gas_io(SPCR_PORT), 7),
        &[],
    )
}

/// Writes `BENCH-STALE-KEY` with `contents` on the ESP `stage` has just laid out.
fn switch(work: &Path, contents: &[u8]) -> Result {
    let path = work
        .join("esp")
        .join("EFI")
        .join("ORIGIN")
        .join("BENCH-STALE-KEY");
    fs::write(&path, contents).map_err(|e| format!("{}: {e}", path.display()))
}

fn stale_once(work: &Path) -> Result<Vec<String>> {
    switch(work, b"1\n")?;
    spcr(work)
}

fn stale_twice(work: &Path) -> Result<Vec<String>> {
    switch(work, b"2\n")?;
    spcr(work)
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

/// The four scenarios of [`boot`].
fn scenarios() -> Vec<Scenario> {
    let with_crumbs = |first: &str| {
        let mut m = vec![first.to_string(), CRUMBS.to_string()];
        m.extend(kernel_markers(SAID_PORT));
        m
    };
    vec![
        scenario(
            "bench: the firmware's key",
            Loader::Bench,
            with_crumbs(FIRST_CALL),
            spcr,
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
            vec![HALTING_START.to_string()],
            stale_twice,
            check_halt,
            Some(QUIET),
        ),
        scenario(
            "bench: the bench kernel under the normal loader",
            Loader::Developer,
            kernel_markers(SAID_NONE),
            spcr,
            check_normal_loader,
            None,
        ),
    ]
}

/// The loader's console (COM1) holds `switch_line`, the state line and `LOADER_NAMES_COM2`, all
/// before its last line.
fn check_loader_console(done: &Completed, switch_line: &str) -> Result {
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    let at = |needle: &str| com1.find(needle);
    let last = at(LAST_LOADER_LINE).ok_or("COM1 lacks the loader's last line")?;
    for needle in [switch_line, STATE, LOADER_NAMES_COM2] {
        match at(needle) {
            Some(i) if i < last => {}
            _ => {
                return Err(format!(
                    "COM1, the bench loader's console, lacks {needle:?} before its last line"
                ));
            }
        }
    }
    let state = com1
        .lines()
        .find(|l| l.starts_with(STATE))
        .unwrap_or_default();
    if !(state.contains(", EFER 0x") && state.contains("; shadow stacks: ")) {
        return Err(format!("the state line is not whole: {state:?}"));
    }
    Ok(())
}

/// On the SPCR's UART, `first` is followed by the breadcrumbs on a line of their own and then by
/// the kernel's first line, with no kernel line before it; and `boot-console`'s SPCR checks hold.
/// (OVMF writes its console, and so the loader's earlier lines, to every serial port: COM2 holds
/// those first.)
fn check_crumbs(done: &Completed, settings: &Settings, first: &str) -> Result {
    let uart = serial::clean(done.serial.as_bytes());
    let lines: Vec<&str> = uart.lines().filter(|l| !l.is_empty()).collect();
    let at = lines
        .iter()
        .position(|l| *l == first)
        .ok_or_else(|| format!("the SPCR's UART lacks the bench loader's line {first:?}"))?;
    let next = lines.get(at + 1..(at + 3).min(lines.len())).unwrap_or(&[]);
    if next.first() != Some(&CRUMBS)
        || !next
            .get(1)
            .is_some_and(|l| l.starts_with("origin-kernel: console: framebuffer "))
    {
        return Err(format!(
            "on the SPCR's UART the loader's line {first:?} should be followed by {CRUMBS:?} on its own, then the kernel's first line; it is followed by {next:?}"
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
    check_loader_console(done, NO_SWITCH)?;
    check_crumbs(done, settings, FIRST_CALL)
}

fn check_retry(done: &Completed, settings: &Settings) -> Result {
    check_loader_console(done, SWITCH_ONCE)?;
    check_crumbs(done, settings, RETRY)
}

/// Both calls failed: on the SPCR's UART the halting line follows the loader's last line and,
/// [`QUIET`] later, nothing follows it; and no kernel line was written anywhere.
fn check_halt(done: &Completed, _settings: &Settings) -> Result {
    check_loader_console(done, SWITCH_TWICE)?;
    let uart = serial::clean(done.serial.as_bytes());
    let lines: Vec<&str> = uart.lines().filter(|l| !l.is_empty()).collect();
    let last = lines
        .iter()
        .position(|l| *l == LAST_LOADER_LINE)
        .ok_or("the SPCR's UART lacks the loader's last line")?;
    let after = &lines[last + 1..];
    if after != [HALTING] {
        return Err(format!(
            "after both calls failed, the SPCR's UART should hold the halting line after the loader's last line and nothing more for {} s; it holds {after:?}",
            QUIET.as_secs()
        ));
    }
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    if uart.contains("origin-kernel:") || com1.contains("origin-kernel:") {
        return Err("a kernel line was written, though the loader halted".into());
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
    }
    check_spcr_console(done, settings)
}

/// Whether `line` is made of breadcrumbs only.
fn is_crumb_line(line: &str) -> bool {
    !line.is_empty() && line.bytes().all(|b| CRUMBS.as_bytes().contains(&b))
}

/// `cargo xtask boot-bench`: Clippy over the bench code, the bench pair built, and the four
/// scenarios booted (part of `boot-matrix`).
pub(crate) fn boot(settings: &Settings) -> Result {
    let started = Instant::now();
    clippy()?;
    let (_, kernel) = build()?;
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
        "xtask: boot-bench passed ({} of {} scenarios: the firmware's key, a stale key once, twice, and the bench kernel under the normal loader) in {} s",
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
    fn f1_takes_the_listed_files_and_refuses_others() {
        let listed: Vec<String> = FILES.iter().map(|(p, _)| (*p).to_string()).collect();
        assert_eq!(check_found(&listed), Ok(()));
        let mut more = listed.clone();
        more.push("framekernel/src/console.rs".into());
        let err = check_found(&more).unwrap_err();
        assert!(
            err.contains("framekernel/src/console.rs names the bench flag"),
            "{err}"
        );
        let fewer = &listed[1..];
        let err = check_found(fewer).unwrap_err();
        assert!(
            err.contains("Cargo.toml is on the bench gate's list"),
            "{err}"
        );
    }

    #[test]
    fn no_signing_path_takes_a_bench_file() {
        let bench_kernel = target().join("xtask").join("KERNEL.ONBF");
        assert!(refuse(&bench_kernel, "signing").is_err());
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
}
