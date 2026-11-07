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
  build       build the UEFI loader for x86_64-unknown-uefi (release) and print its path
  boot-test   build, boot under QEMU + OVMF, and check the loader's serial output
  run         boot under QEMU with the serial console on this terminal (Ctrl-A X quits)

environment:
  ORIGIN_OVMF_CODE, ORIGIN_OVMF_VARS  firmware code image and NVRAM template
                                      (default: the EDK2 files installed next to QEMU)
  ORIGIN_BOOT_TEST_TIMEOUT            boot-test timeout in seconds (default 120)";

const UEFI_TARGET: &str = "x86_64-unknown-uefi";
const LOADER_PACKAGE: &str = "origin-boot-uefi";
const LOADER_BIN: &str = "origin-boot";
const VERSION_PACKAGE: &str = "origin-version";
const QEMU: &str = "qemu-system-x86_64";
const DEFAULT_BOOT_TEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Lines `boot-test` requires, in this order.
///
/// The banner is built from the same generated constants the loader uses, so the test
/// also checks that the release identity in origin-version.toml reached the binary.
/// The last two markers come from OVMF's boot manager, not from the loader. They show that
/// the firmware received `EFI_UNSUPPORTED` as the image's return value, which checks the
/// `efiapi` return path end to end.
fn boot_markers() -> Vec<String> {
    use origin_version::{BUILD, PRODUCT, VERSION};
    vec![
        format!("{PRODUCT} {VERSION} UEFI loader (build {BUILD})"),
        "origin-boot: UEFI 2.".into(),
        "origin-boot: firmware ".into(),
        "origin-boot: Framekernel loading is not implemented yet (M1 step 2); returning EFI_UNSUPPORTED".into(),
        "BdsDxe: failed to start Boot0001 ".into(),
        "): Unsupported".into(),
    ]
}

/// Output that fails `boot-test` as soon as it appears.
const FAILURE_MARKERS: &[&str] = &["origin-boot: panic:"];

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
        ["build"] => build_loader().map(|p| println!("{}", p.display())),
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
        &["test", "--package", "xtask"],
        &["test", "--package", LOADER_PACKAGE, "--lib"],
        &["test", "--package", VERSION_PACKAGE],
    ];
    for step in steps {
        run_step(cargo().args(*step))?;
    }
    build_loader()?;
    linux_cfg_lint()?;
    manifest_version_lint()?;
    println!("xtask: check passed");
    Ok(())
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

/// Builds the loader and lays out a fresh ESP and NVRAM copy for one boot.
fn stage() -> Result<Staged> {
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
    c.args(["-machine", "q35", "-cpu", "max", "-m", "512M", "-smp", "2"])
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
    let staged = stage()?;
    let timeout = boot_test_timeout()?;
    let mut cmd = qemu_command(&staged)?;
    cmd.args(["-serial", "stdio", "-monitor", "none"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    println!(
        "xtask: booting under {QEMU} + OVMF (timeout {}s)",
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

    let markers = boot_markers();
    let markers: Vec<&str> = markers.iter().map(String::as_str).collect();
    let started = Instant::now();
    let deadline = started + timeout;
    let mut raw = Vec::new();
    let verdict: Result = loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(chunk) => {
                raw.extend_from_slice(&chunk);
                let text = serial::clean(&raw);
                if let Some(m) = FAILURE_MARKERS.iter().find(|m| text.contains(**m)) {
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

    // After the loader returns, the firmware moves on to its next boot option and QEMU
    // keeps running, so the test stops it. (QEMU does not exit on SIGALRM.)
    let _ = child.kill();
    let exit = child.wait();
    let qemu_stderr = stderr_reader.join().unwrap_or_default();

    let text = serial::clean(&raw);
    let log = target_dir().join("xtask").join("boot-test-serial.log");
    fs::write(&log, &text).map_err(|e| format!("{}: {e}", log.display()))?;

    match verdict {
        Ok(()) => {
            for line in text
                .lines()
                .filter(|l| l.starts_with("Origin OS") || l.starts_with("origin-boot:"))
            {
                println!("  | {line}");
            }
            println!(
                "xtask: boot-test passed in {:.1}s; serial log: {}",
                elapsed.as_secs_f64(),
                log.display()
            );
            Ok(())
        }
        Err(e) => {
            eprintln!("--- serial output ---\n{text}\n--- {QEMU} stderr ---\n{qemu_stderr}");
            Err(format!(
                "boot-test failed: {e} (QEMU: {exit:?}); serial log: {}",
                log.display()
            ))
        }
    }
}

fn run() -> Result {
    let staged = stage()?;
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
