//! Origin OS build orchestration (host only; CLAUDE.md §70, handoff task T4).
//!
//! Each Origin artifact is built for its own target, which Cargo alone cannot express on
//! stable Rust (per-package targets are unstable). This tool runs the right command for
//! each crate. It uses std only, no third-party crates. Run it through the alias in
//! `.cargo/config.toml`: `cargo xtask <command>`.
#![forbid(unsafe_code)]

mod asmflags;
mod disk;
mod documents;
mod image;
mod interfaces;
mod lex;
mod loom;
mod mnemonics;
mod reproducible;
mod requirements;
mod ring3;
mod screen;
mod serial;
mod targetcfg;
mod turns;
mod unsafety;

use std::borrow::Cow;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, ExitCode, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  check       rustfmt, clippy and unit tests, each crate on its own target(s);
              the UEFI release build; the CLAUDE.md §11 cfg lint
  build       build the loader (developer mode) and the KRN kernel image; print their paths
  boot-test   boot under QEMU + OVMF: a signed kernel must start and report with developer
              mode off, an unsigned one in developer mode, and a loader with developer mode
              off must refuse an unsigned image, a wrong key, a tampered statement, a bad
              signature and tampered content, each with its own message
  boot-matrix boot-test with each IOMMU (Intel VT-d, AMD-Vi) and 1, 2 and 4 processors,
              one after another, then boot-x2apic, boot-bridge, boot-legacy-free,
              boot-random and boot-console
  boot-bridge boot-test with the NIC behind a PCIe root port (both IOMMUs): bus mastering is
              cleared on the bridge too and opened again above the device its driver attached
  boot-legacy-free  the kernel boot on a machine with no 8254 and no 8042: the timers
              calibrate against the ACPI PM timer, and the proofs that need those devices
              say they cannot run
  boot-random the kernel boot on two processors that lack what the random-number generator is
              seeded from (ADR-0016): one with RDRAND and no RDSEED, which boots and says
              so; one with neither, which has no generator, launches nothing and still
              reaches its verdict
  boot-x2apic boot-test on a sparse topology: the boot processor at APIC ID 0 and processors
              at APIC IDs 256 and 412, above what 8-bit APIC IDs reach (the MADT lists
              150 possible processors); twice, on a CPU model with CPUID leaf 1Fh and on
              one without
  boot-console the kernel's console on machines without a 16550 at 0x3F8 (ADR-0010): a UART
              named by an ACPI SPCR table, a table that must be refused, no display, and no
              UART at all (with a framebuffer the only way to see the boot)
  run         boot under QEMU with the serial console on this terminal (Ctrl-A X quits)
  reproducible-gate only the gate on reproducible builds: the loader, each way it is built,
              and the kernel are built twice from nothing in two target directories, and the
              two files of each must be the same bytes (part of check; CLAUDE.md §75)
  loom        the Loom models of origin-sync (SpinLock and the wait-queue protocol) and their
              negative controls, each applied to a scratch copy of the source; also part of
              check (CLAUDE.md §73)
  unsafe-gate only the gate on `unsafe`, on the kernel's dependency direction, on the
              `preserves_flags` option of assembly blocks, and on RDRAND and RDSEED, which
              only the kernel's generator and the loader's may run (part of check; quick)
  cfg-lint    only the CLAUDE.md §11 lint: no Origin code outside developer-tools/ asks
              whether its target is Linux or Unix (part of check; quick)
  document-gate only the gate on documents and archives: none is tracked, or left in this
              branch's history, outside a short allowlist (part of check; quick)
  interface-gate only the gates on the generated interface code: fresh, and its ABI recorded
              (part of check; quick)
  requirements-gate only the gate on the requirements matrix under docs/requirements/: a row
              for every statement of CLAUDE.md and every decision of an accepted ADR, each
              true to the tree; prints the coverage figure (part of check; CLAUDE.md §1;
              quick)
  regenerate  rewrites the kernel's generated system-call crate from sdk/interfaces
  image-gate [kernel ELF]
              only the gate on the kernel's image (ADR-0017 Z10): an INT3 after every return
              and no jump table, in the code this build compiled. Builds the kernel first, or
              reads the ELF it is given (part of check)
  exclusive -- <command> [args...]
              runs any command in a turn on the machine, as check, the boots, run and loom
              take theirs: one at a time across every checkout of this repository on this
              machine. A waiter says once, and about once a minute, which run it waits for.
              For scripts that call cargo directly (per-commit verification, say)

environment:
  ORIGIN_XTASK_LOCK_HELD              set by a run that holds the turn, for its own child
                                      processes: a child that finds the turn held by that
                                      process runs inside it instead of waiting for it
  ORIGIN_LOOM_DEEP                    if set, the Loom models search to their deeper bounds
                                      (a long run; the default search is part of check)
  ORIGIN_OVMF_CODE, ORIGIN_OVMF_VARS  firmware code image and NVRAM template
                                      (default: the EDK2 files installed next to QEMU)
  ORIGIN_BOOT_TEST_TIMEOUT            boot-test timeout in seconds (default 120)
  ORIGIN_BOOT_TEST_ONLY               run only the boot-test or boot-console scenarios whose
                                      label contains one of these comma-separated strings
  ORIGIN_QEMU_SMP                     processors QEMU emulates, 1-64 (default 2)
  ORIGIN_QEMU_TOPOLOGY                x2apic: the boot-x2apic machine (ORIGIN_QEMU_SMP is
                                      then ignored)
  ORIGIN_QEMU_CPU                     the QEMU CPU model (default max), e.g. max,-avx2 or
                                      max,vendor=GenuineIntel (which exposes leaf 1Fh on
                                      the boot-x2apic machine)
  ORIGIN_QEMU_IOMMU                   the IOMMU QEMU emulates: intel (default) or amd; or
                                      an intel-iommu -device value, booted with the VT-d
                                      markers (e.g. intel-iommu,intremap=off for a
                                      negative control)
  ORIGIN_QEMU_MACHINE                 extra -machine options, e.g. pit=off,i8042=off for a
                                      machine with no 8254 and no 8042 (the boot-legacy-free
                                      machine)
  ORIGIN_QEMU_BRIDGE                  on: the NIC behind a PCIe root port (the boot-bridge
                                      machine)
  ORIGIN_QEMU_ACCEL                   tcg (default) or kvm: run on a real x86-64 processor (needs
                                      an x86-64 host with /dev/kvm); the devices and firmware
                                      stay QEMU's
  ORIGIN_QEMU_DEBUG                   if set, QEMU logs interrupts and resets to
                                      target/xtask/qemu-debug.log";

const UEFI_TARGET: &str = "x86_64-unknown-uefi";
const LOADER_PACKAGE: &str = "origin-boot-uefi";
const LOADER_BIN: &str = "origin-boot";
const VERSION_PACKAGE: &str = "origin-version";
const ONBF_FORMAT_PACKAGE: &str = "onbf-format";
const CRYPTO_PACKAGE: &str = "origin-crypto";
/// The random-number generator's policy (ADR-0016 R2): in the loader and the Framekernel.
const RANDOM_PACKAGE: &str = "origin-random";
const MERKLE_PACKAGE: &str = "onbf-merkle";
const KERNEL_TARGET: &str = "x86_64-unknown-none";
/// `no_std` libraries embedded in the boot loader and the Framekernel, and the generated crates of
/// the SDK's interfaces, which the Framekernel's boot proofs use and programs will. Each must be
/// clippy-clean on every target and free of panic paths (ONBF §12).
const BOOT_INFO_PACKAGE: &str = "origin-boot-info";
const KERNEL_PACKAGE: &str = "framekernel";
/// The Framekernel's privileged framework (ADR-0011 N6).
const FRAMEWORK_PACKAGE: &str = "origin-frame";
const PACK_PACKAGE: &str = "onbf-pack";
/// The host-only signing code (CLAUDE.md §59): never linked into the OS.
const SIGN_PACKAGE: &str = "onbf-sign";
/// The interface-language tool (docs/spec/OIDL.md): host only.
const OIDL_PACKAGE: &str = "oidl";
const FREESTANDING_LIBS: &[&str] = &[
    BOOT_INFO_PACKAGE,
    ONBF_FORMAT_PACKAGE,
    MERKLE_PACKAGE,
    CRYPTO_PACKAGE,
    RANDOM_PACKAGE,
    "origin-sync",
    "onbf-verify",
    "origin-paging",
    "origin-abi",
    "origin-syscalls",
    "origin-directory",
];
/// The services' `no_std` logic, which programs will run in ring 3 and the host tests until then.
const SERVICE_LIBS: &[&str] = &["origin-directory-service"];
const QEMU: &str = "qemu-system-x86_64";
/// Where the emulated RTC starts (UTC), so the boot test can check the wall clock the
/// kernel reads from it: BCD decoding, the FADT century register and the calendar.
const RTC_BASE: &str = "2030-06-15T12:00:00";
/// What the kernel must report, allowing up to ten minutes of boot.
const RTC_BASE_MARKER: &str = "2030-06-15T12:0";
const DEFAULT_BOOT_TEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Which loader build a scenario boots (ADR-0002 B8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Loader {
    /// `developer-mode`: accepts an unsigned image, says so, and verifies a signed one.
    Developer,
    /// `test-trust-store` only: developer mode is off, so only a signed kernel boots. This is
    /// what a release build does, but with the test key in place of Origin's own.
    Verifying,
}

impl Loader {
    /// The Cargo feature this build needs.
    fn feature(self) -> &'static str {
        match self {
            Self::Developer => "developer-mode",
            Self::Verifying => "test-trust-store",
        }
    }
}

/// One QEMU boot the test runs, and what its serial output must and must not contain.
struct Scenario {
    label: &'static str,
    /// The loader build it boots.
    loader: Loader,
    /// Lines required, in this order.
    markers: Vec<String>,
    /// Output that fails the scenario as soon as it appears.
    failures: Vec<&'static str>,
    /// A check of the whole boot once every marker has appeared.
    check: Option<fn(&Completed) -> Result>,
    /// Text QEMU itself must have printed on its stderr by then (what its device models
    /// report about the guest).
    stderr: Vec<&'static str>,
    /// Which serial ports and monitor QEMU has, and where the markers are read from.
    capture: Capture,
    /// Stages what the scenario needs under the work directory and gives QEMU's extra
    /// arguments for it.
    qemu_args: Option<QemuArgs>,
}

/// A function that stages a scenario's files and returns its extra QEMU arguments.
type QemuArgs = fn(&Path) -> Result<Vec<String>>;

/// How a scenario's QEMU is wired for output (the devices are `qemu_command`'s).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Capture {
    /// COM1 on stdout, no monitor: what most scenarios use.
    Com1,
    /// COM1 and the monitor multiplexed on stdio: the screen is dumped once the markers
    /// have appeared, and the scenario's check gets the picture.
    Com1AndScreen,
    /// The UART an SPCR table names on stdout, COM1 in a file for the check to read.
    SpcrUart,
    /// No UART: the monitor on stdio, the markers looked for in screen dumps.
    Screen,
}

/// What a scenario's boot left behind, for its check.
struct Completed {
    /// The serial output, cleaned (the markers were found in this).
    serial: String,
    /// The screen dump, for [`Capture::Com1AndScreen`].
    screen: Option<screen::Image>,
    /// What COM1 wrote to its file, for [`Capture::SpcrUart`].
    com1: Option<String>,
}

/// The good path: the loader verifies and enters the kernel, and the kernel reports on
/// COM1. The banners are built from the same generated constants as the binaries, so the
/// test also checks that the release identity reached both.
/// QEMU's machine option: `base` (what the accelerator needs), with what
/// `ORIGIN_QEMU_MACHINE` adds (for example `pit=off,i8042=off` for a machine with no 8254 or
/// 8042).
fn qemu_machine_options(base: &str) -> String {
    match env::var("ORIGIN_QEMU_MACHINE") {
        Ok(extra) if !extra.is_empty() => format!("{base},{extra}"),
        _ => base.to_string(),
    }
}

/// Whether `ORIGIN_QEMU_MACHINE` turns `device` off (`device=off`).
fn qemu_machine_lacks(device: &str) -> bool {
    let off = format!("{device}=off");
    env::var("ORIGIN_QEMU_MACHINE").is_ok_and(|extra| extra.split(',').any(|o| o == off))
}

/// Whether `ORIGIN_QEMU_CPU` takes `feature` away from the processor (`-feature` or
/// `feature=off`).
fn qemu_cpu_lacks(feature: &str) -> bool {
    let (minus, off) = (format!("-{feature}"), format!("{feature}=off"));
    qemu_cpu_model()
        .split(',')
        .any(|option| option == minus || option == off)
}

/// The QEMU CPU model: `ORIGIN_QEMU_CPU` (for example `max,-avx2` to boot a CPU below the
/// x86-64-v3 baseline), or `max`.
fn qemu_cpu_model() -> String {
    std::env::var("ORIGIN_QEMU_CPU")
        .ok()
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "max".into())
}

/// The IOMMU QEMU emulates (`ORIGIN_QEMU_IOMMU`): Intel VT-d or AMD-Vi, or a raw
/// `intel-iommu` device for negative controls.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Iommu {
    Intel,
    Amd,
    /// An `intel-iommu` `-device` value other than the default (for example
    /// `intel-iommu,intremap=off`), booted with the VT-d markers.
    Device(String),
}

impl Iommu {
    /// The QEMU options that add it.
    fn device_args(&self) -> Vec<String> {
        let args: &[&str] = match self {
            // Intel VT-d: DMA and interrupt remapping. `eim=on` gives IRTEs 32-bit x2APIC
            // destinations (`auto` resolves to off without KVM's in-kernel irqchip); TCG needs
            // no irqchip option. See docs/research/2026-11-08-vt-d-iommu.md.
            Self::Intel => &["-device", "intel-iommu,intremap=on,eim=on"],
            // AMD-Vi: interrupt remapping, `xtsup=on` for 128-bit IRTEs with 32-bit x2APIC
            // destinations, and `dma-remap=on`, without which QEMU 11.1 does not translate the
            // DMA of emulated devices at all (they bypass the IOMMU). Its PCI function is
            // pinned at 00:05.0: QEMU would otherwise take the first free slot, moving the NVMe
            // and the NIC. See docs/research/2026-11-08-amd-vi-iommu.md.
            Self::Amd => &[
                "-device",
                "AMDVI-PCI,id=iommupci0,addr=05.0",
                "-device",
                "amd-iommu,pci-id=iommupci0,intremap=on,xtsup=on,dma-remap=on",
            ],
            Self::Device(device) => return vec!["-device".into(), device.clone()],
        };
        args.iter().map(|a| (*a).to_string()).collect()
    }
}

/// `ORIGIN_QEMU_IOMMU`: `intel` (the default), `amd`, or an `intel-iommu` `-device` value;
/// anything else is refused by [`check_environment`].
fn qemu_iommu() -> Iommu {
    parse_iommu(env::var("ORIGIN_QEMU_IOMMU").ok().as_deref())
}

/// What an `ORIGIN_QEMU_IOMMU` value selects.
fn parse_iommu(value: Option<&str>) -> Iommu {
    match value {
        None | Some("" | "intel") => Iommu::Intel,
        Some("amd") => Iommu::Amd,
        Some(other) => Iommu::Device(other.into()),
    }
}

/// Refuses an `ORIGIN_QEMU_IOMMU` the boot test cannot check, rather than booting another
/// IOMMU: a raw `-device` value is booted with the VT-d markers, so it must be an
/// `intel-iommu`.
fn check_environment() -> Result {
    Accel::from_env()?;
    check_iommu(&qemu_iommu())
}

/// [`check_environment`]'s rule for the IOMMU.
fn check_iommu(iommu: &Iommu) -> Result {
    match iommu {
        Iommu::Device(device) if device != "intel-iommu" && !device.starts_with("intel-iommu,") => {
            Err(format!(
                "ORIGIN_QEMU_IOMMU must be intel, amd or an intel-iommu -device value, not {device:?}"
            ))
        }
        _ => Ok(()),
    }
}

/// Whether the NIC sits behind a PCIe root port (`ORIGIN_QEMU_BRIDGE=on`, `boot-bridge`).
fn qemu_bridge() -> bool {
    env::var("ORIGIN_QEMU_BRIDGE").as_deref() == Ok("on")
}

/// Processors QEMU emulates: `ORIGIN_QEMU_SMP` (1–64), or 2.
fn qemu_cpus() -> u32 {
    std::env::var("ORIGIN_QEMU_SMP")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| (1..=64).contains(n))
        .unwrap_or(2)
}

/// How QEMU runs the guest's instructions (`ORIGIN_QEMU_ACCEL`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Accel {
    /// QEMU's software CPU: the default, and all an Apple Silicon host has.
    Tcg,
    /// The host's real x86-64 processor, through KVM. The devices and the firmware stay QEMU's.
    Kvm,
}

impl Accel {
    /// The accelerator a value of `ORIGIN_QEMU_ACCEL` names: unset, empty or `tcg` is TCG.
    fn named(value: Option<&str>) -> Result<Accel> {
        match value.filter(|v| !v.is_empty()) {
            None | Some("tcg") => Ok(Accel::Tcg),
            Some("kvm") => Ok(Accel::Kvm),
            Some(other) => Err(format!(
                "ORIGIN_QEMU_ACCEL must be tcg or kvm, not {other:?}"
            )),
        }
    }

    fn from_env() -> Result<Accel> {
        Accel::named(env::var("ORIGIN_QEMU_ACCEL").ok().as_deref())
    }

    /// The `-machine` value. Interrupt remapping needs the I/O APIC in QEMU, not in the host
    /// kernel, hence the split irqchip.
    fn machine(self) -> &'static str {
        match self {
            Accel::Tcg => "q35",
            Accel::Kvm => "q35,accel=kvm,kernel-irqchip=split",
        }
    }

    /// The complete lines of a boot that failure messages are looked for in: under KVM,
    /// without the lines [`kvm_forgives`] describes.
    fn judged(self, complete: &str) -> Cow<'_, str> {
        match self {
            Accel::Tcg => Cow::Borrowed(complete),
            Accel::Kvm => Cow::Owned(
                complete
                    .lines()
                    .filter(|l| !kvm_forgives(l))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        }
    }

    /// The failure messages a screen is searched for. A screen wraps long lines, so under KVM
    /// the lines [`kvm_forgives`] describes cannot be told from the rest there, and the
    /// screen-only boot leaves those messages to the boots with a serial port.
    fn screen_failures(self, failures: &[&'static str]) -> Vec<&'static str> {
        failures
            .iter()
            .copied()
            .filter(|f| self == Accel::Tcg || !KVM_FORGIVEN_MESSAGES.contains(f))
            .collect()
    }
}

/// The processors QEMU presents, and what the kernel must find.
struct Machine {
    /// The `-smp` value.
    smp: String,
    /// Processors cold-plugged with `-device` (QEMU starts them with the machine).
    devices: Vec<String>,
    /// The APIC ID of each CPU index the kernel gives: the boot processor, then the MADT's
    /// enabled processors in table order.
    apic_ids: Vec<u32>,
    /// Processor structures the MADT lists (QEMU lists every possible processor).
    listed: u32,
    /// The largest APIC ID the MADT lists.
    highest: u32,
    /// How `-smp` lays out the topology, for the proof of it.
    geometry: Geometry,
}

/// How many of each topology domain `-smp` builds, per parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Geometry {
    packages: u32,
    dies: u32,
    modules: u32,
    cores: u32,
    threads: u32,
}

impl Geometry {
    /// The bits QEMU allots to `count` IDs: the smallest power of two that holds them.
    fn bits(count: u32) -> u32 {
        count.next_power_of_two().trailing_zeros()
    }

    /// The possible processors' x2APIC IDs, as QEMU numbers them from these fields.
    fn ids(&self) -> Vec<u32> {
        let (t, c, m, d) = (
            Self::bits(self.threads),
            Self::bits(self.cores),
            Self::bits(self.modules),
            Self::bits(self.dies),
        );
        let mut ids = Vec::new();
        for package in 0..self.packages {
            for die in 0..self.dies {
                for module in 0..self.modules {
                    for core in 0..self.cores {
                        for thread in 0..self.threads {
                            ids.push(
                                package << (t + c + m + d)
                                    | die << (t + c + m)
                                    | module << (t + c)
                                    | core << t
                                    | thread,
                            );
                        }
                    }
                }
            }
        }
        ids
    }

    /// Whether the machine has a level CPUID leaf 1Fh describes (QEMU exposes the leaf only
    /// for die or module levels).
    fn has_extended_levels(&self) -> bool {
        self.dies > 1 || self.modules > 1
    }
}

impl Machine {
    /// Processors online.
    fn cpus(&self) -> u32 {
        self.apic_ids.len() as u32
    }

    /// `CPU {index} (APIC ID {id})`, as the kernel names a processor.
    fn named(&self, index: u32) -> String {
        let id = self.apic_ids.get(index as usize).copied().unwrap_or(index);
        format!("CPU {index} (APIC ID {id})")
    }
}

/// The machine the boot uses: `-smp N` (QEMU numbers the processors 0 to N-1), or with
/// `ORIGIN_QEMU_TOPOLOGY=x2apic` a sparse one. There, 2 packages of 3 dies × 5 modules × 5
/// cores take 2 + 3 + 3 bits of the APIC ID below the package, so package 1 starts at APIC
/// ID 256; 150 processors are possible. Present are the boot processor, one cold-plugged at
/// package 1 (APIC ID 256, docs/research/2026-11-08-x2apic-above-255.md) and one at package 1,
/// die 2, module 3, core 4 (APIC ID 412): its die, module and core are all non-zero, so a
/// decoder that confuses them shows (docs/research/2026-11-08-cpu-topology-leaf-1fh.md).
fn machine() -> Machine {
    if std::env::var("ORIGIN_QEMU_TOPOLOGY").as_deref() == Ok("x2apic") {
        // The cold-plugged processors are of the boot processor's model: QEMU names their
        // type after the model (`max` -> `max-x86_64-cpu`) and applies `-cpu`'s features.
        let model = qemu_cpu_model();
        let base = model.split(',').next().unwrap_or("max");
        return Machine {
            smp: "1,maxcpus=150,sockets=2,dies=3,modules=5,cores=5,threads=1".into(),
            devices: vec![
                format!("{base}-x86_64-cpu,socket-id=1,die-id=0,module-id=0,core-id=0,thread-id=0"),
                format!("{base}-x86_64-cpu,socket-id=1,die-id=2,module-id=3,core-id=4,thread-id=0"),
            ],
            apic_ids: vec![0, 256, 412],
            listed: 150,
            highest: 420,
            geometry: Geometry {
                packages: 2,
                dies: 3,
                modules: 5,
                cores: 5,
                threads: 1,
            },
        };
    }
    let cpus = qemu_cpus();
    Machine {
        smp: cpus.to_string(),
        devices: Vec::new(),
        apic_ids: (0..cpus).collect(),
        listed: cpus,
        highest: cpus - 1,
        // `-smp N`: QEMU builds one package of N single-thread cores.
        geometry: Geometry {
            packages: 1,
            dies: 1,
            modules: 1,
            cores: cpus,
            threads: 1,
        },
    }
}

/// What the kernel must report about the machine's topology (CLAUDE.md §24, §27), computed
/// here from the `-smp` fields alone, independently of CPUID. `leaf_1f`: the CPU model reports
/// die and module levels in leaf 1Fh; otherwise leaf 0Bh shows the core field reaching all
/// the way to the package.
fn topology_markers(machine: &Machine, leaf_1f: bool) -> Vec<String> {
    let g = machine.geometry;
    let (t, c, m, d) = (
        Geometry::bits(g.threads),
        Geometry::bits(g.cores),
        Geometry::bits(g.modules),
        Geometry::bits(g.dies),
    );
    let package_at = t + c + m + d;
    // Under leaf 0Bh the die and module bits belong to the core field.
    let (source, core_bits, module_bits, die_bits) = if leaf_1f {
        ("leaf 1Fh", c, m, d)
    } else {
        ("leaf 0Bh", c + m + d, 0, 0)
    };
    let field = |id: u32, from: u32, bits: u32| (id >> from) & ((1u32 << bits) - 1);
    let distinct = |ids: &[u32], shift: u32| {
        let mut seen: Vec<u32> = ids.iter().map(|id| id >> shift).collect();
        seen.sort_unstable();
        seen.dedup();
        seen.len()
    };
    let mut markers = vec![format!(
        "origin-kernel: topology: layout from {source}: thread bits {t}, core bits {core_bits}, module bits {module_bits}, die bits {die_bits}, package from bit {package_at}"
    )];
    for (index, &id) in (0..).zip(&machine.apic_ids) {
        markers.push(format!(
            "origin-kernel: topology: {}: package {}, die {}, module {}, core {}, thread {}",
            machine.named(index),
            id >> package_at,
            field(id, t + core_bits + module_bits, die_bits),
            field(id, t + core_bits, module_bits),
            field(id, t, core_bits),
            field(id, 0, t)
        ));
    }
    let listed = g.ids();
    let online = machine.apic_ids.as_slice();
    markers.push(format!(
        "origin-kernel: topology: the MADT lists {} processors: {} packages, {} dies, {} modules, {} cores, {} threads",
        machine.listed,
        distinct(&listed, package_at),
        distinct(&listed, t + core_bits + module_bits),
        distinct(&listed, t + core_bits),
        distinct(&listed, t),
        listed.len()
    ));
    let cpus = machine.cpus();
    markers.push(format!(
        "origin-kernel: topology: core types: reported by 0 of {cpus} processors; hybrid false"
    ));
    markers.push(format!(
        "origin-kernel: topology: against the MADT: {cpus} processors checked, 0 recorded nothing, 0 have another APIC ID, 0 another layout, 0 an undecodable leaf, 0 no core type although hybrid, 0 a duplicate placement; consistent true"
    ));
    markers.push(format!(
        "origin-kernel: topology: {cpus} logical, {} cores, {} packages ({} threads/core, {} logical/package); consistent true",
        distinct(online, t),
        distinct(online, package_at),
        1u32 << t,
        1u32 << package_at
    ));
    markers
}

fn boot_scenario() -> Scenario {
    let machine = machine();
    let mut scenario = boot_scenario_markers(&machine);
    // Each AP's start-up line comes after the MADT line, in index order.
    if let Some(at) = scenario
        .markers
        .iter()
        .position(|m| m.starts_with("origin-kernel: ACPI MADT: "))
    {
        for index in (1..machine.cpus()).rev() {
            scenario.markers.insert(
                at + 1,
                format!("origin-kernel: {} online after ", machine.named(index)),
            );
        }
        // Before them, the park drill on CPU 1: started into a counting loop, then INIT;
        // its count must stand still (ADR-0008 C7).
        if machine.cpus() > 1 {
            scenario.markers.insert(
                at + 1,
                format!(
                    "origin-kernel: SMP: park drill: {} ran a counting loop from the trampoline (count ",
                    machine.named(1)
                ),
            );
            scenario.markers.insert(
                at + 2,
                "; 10 ms after INIT, its count moved 0 in 1000 us: parked true".into(),
            );
        }
    }
    if qemu_iommu() == Iommu::Amd {
        amd_iommu_markers(&mut scenario, &machine);
    }
    if Accel::from_env() == Ok(Accel::Kvm) {
        kvm_markers(&mut scenario, qemu_iommu() != Iommu::Amd);
    }
    if qemu_bridge() {
        bridge_markers(&mut scenario);
    }
    if qemu_machine_lacks("pit") {
        no_pit_markers(&mut scenario);
    }
    if qemu_machine_lacks("i8042") {
        no_i8042_markers(&mut scenario);
    }
    match (qemu_cpu_lacks("rdseed"), qemu_cpu_lacks("rdrand")) {
        (true, false) => rdrand_only_markers(&mut scenario),
        (true, true) => no_generator_markers(&mut scenario),
        _ => {}
    }
    scenario
}

/// The markers of a boot on a processor with RDRAND and no RDSEED (`-cpu max,-rdseed`): the
/// machine AMD's second workaround for AMD-SB-7055 produces. It boots; the root is counted on
/// RDRAND and says its strength is no more than the processor's own generator's, and a
/// request with prediction resistance is refused (ADR-0016 R3, R6, R8).
fn rdrand_only_markers(scenario: &mut Scenario) {
    swap_marker(
        scenario,
        "origin-boot: random: known-answer test passed; ",
        "origin-boot: random: known-answer test passed; RDSEED absent, RDRAND healthy".into(),
    );
    swap_marker(
        scenario,
        "origin-kernel: random: root: known-answer test passed; ",
        "origin-kernel: random: root: known-answer test passed; RDSEED absent, RDRAND healthy on the boot processor; seeded from RDRAND's 1024 start-up draws, with 4 more of its draws as the nonce; no healthy RDSEED, so its strength is no more than the processor's own generator's".into(),
    );
    swap_marker(
        scenario,
        "origin-kernel: random: processor 0: ",
        "origin-kernel: random: processor 0: RDSEED absent (0 draws passed), RDRAND healthy (2052 draws passed)".into(),
    );
    swap_marker(
        scenario,
        "origin-kernel: random: 16 of 16 values answered, all distinct true; 300 bytes in pieces true; with prediction resistance: ",
        "origin-kernel: random: 16 of 16 values answered, all distinct true; 300 bytes in pieces true; with prediction resistance: refused, as it must be with no healthy RDSEED here".into(),
    );
}

/// The lines of a boot on a processor with neither instruction (`-cpu max,-rdseed,-rdrand`),
/// in place of the whole list: there is no generator, so the kernel and its stacks' region are
/// placed at fixed addresses, every request is refused, no process is launched and no mapping
/// placed at a random address, the network asks for no lease, and the boot still reaches its
/// verdict, which names the generator (ADR-0016 R8).
fn no_generator_markers(scenario: &mut Scenario) {
    scenario.markers = vec![
        "origin-boot: UEFI 2.".into(),
        "origin-boot: random: known-answer test passed; RDSEED absent, RDRAND absent; no generator: the kernel is placed at a fixed address and is handed no seed".into(),
        "(no generator: fixed placement)".into(),
        "origin-boot: random: no seed for the kernel; the loader's generator is uninstantiated".into(),
        "origin-boot: exiting boot services; entering the Framekernel".into(),
        // No value from the loader, so the kernel stacks' region stays at its fixed base
        // (ADR-0004 Amendment 2, A5.1 and H3), and the kernel says so both times.
        STACK_FIXED_LINE.into(),
        "origin-kernel: random: the loader's value for the kernel stacks' region: none: the loader handed over no secret area".into(),
        "origin-kernel: random: no generator: known-answer test passed; on the boot processor RDSEED is absent and RDRAND is absent. Nothing will be launched and nothing placed at a random address (ADR-0016 R8)".into(),
        "origin-kernel: random: processor 0: RDSEED absent (0 draws passed), RDRAND absent (0 draws passed); its generator NOT seeded".into(),
        "origin-kernel: random: no generator, so no request is answered; consistent false: random failed".into(),
        "origin-kernel: start: randomness is withheld, so no process is launched and no mapping is placed at a random address: the process-start, Rust-program, service-directory and memory-object proofs did not run".into(),
        "origin-kernel: kernel stacks: the region's base is 2 MiB-aligned with the whole region in PML4 slots 384-447: true; the loader handed over no random value: true, and the region is at its fixed base: true".into(),
        // The network asks for no lease: a transaction ID must not be guessable.
        "; DHCP lease 0.0.0.0 (".into(),
        "; no random value for a transaction ID or port; consistent false: virtio-net failed".into(),
        "origin-kernel: boot verdict: degraded: ".into(),
        "; 4 did not run; failed: ".into(),
        "random-number generator (no randomness)".into(),
        HALT_LINE.into(),
    ];
    // What this machine is expected to say is not a failure here.
    scenario.failures.retain(|failure| {
        ![
            "origin-kernel: random: no generator",
            ": random failed",
            "origin-boot: random: no seed for the kernel",
            // The network driver asks for no lease without a random transaction ID.
            ": virtio-net failed",
        ]
        .contains(failure)
    });
    scenario.check = None;
}

/// Replaces the required marker `old` with `new`, or records that it was not found, which
/// fails the boot.
fn swap_marker(scenario: &mut Scenario, old: &str, new: String) {
    match scenario.markers.iter().position(|m| m == old) {
        Some(i) => scenario.markers[i] = new,
        None => scenario
            .markers
            .push(format!("xtask: the marker {old:?} is missing")),
    }
}

/// A machine with no 8254 (`pit=off`, ADR-0013 F3): the kernel calibrates against the ACPI PM
/// timer, and has no PIT interrupts to route through the I/O APIC, then or under remapping.
fn no_pit_markers(scenario: &mut Scenario) {
    swap_marker(
        scenario,
        "origin-kernel: APIC timer calibrated against the PIT: ",
        "origin-kernel: APIC timer calibrated against the ACPI PM timer: ".into(),
    );
    swap_marker(
        scenario,
        "origin-kernel: I/O APIC: 20 PIT interrupts routed through the I/O APIC in ",
        "origin-kernel: I/O APIC: routing not proven: the 8254 is not counting".into(),
    );
    let first = "origin-kernel: I/O APIC: remapped: the PIT's IRQ0 (GSI 2, edge-triggered, active-high) through IRTE ";
    let start = scenario.markers.iter().position(|m| m == first);
    let end = start.and_then(|s| {
        scenario.markers[s..]
            .iter()
            .position(|m| m == " released: true")
            .map(|n| s + n)
    });
    match (start, end) {
        (Some(s), Some(e)) => {
            scenario.markers.splice(
                s..=e,
                [
                    "origin-kernel: I/O APIC: remapped: not proven: the 8254 is not counting"
                        .into(),
                ],
            );
        }
        _ => scenario
            .markers
            .push(format!("xtask: the marker {first:?} is missing")),
    }
}

/// A machine with no 8042 (`i8042=off`, ADR-0013 F2): the FADT says so, and that passes.
fn no_i8042_markers(scenario: &mut Scenario) {
    swap_marker(
        scenario,
        "self-test 0x55 (want 0x55), keyboard port test 0x00 (want 0x00)",
        "origin-kernel: i8042: the FADT says this platform has none".into(),
    );
}

/// The markers of a boot under KVM: what the VT-d pin proof can show there, and without two
/// things a forgiven line leaves out. The kernel prints [`EVEN_SHARES_LINE`] only when every
/// processor's workers got even shares ([`is_uneven_fair_share`]), and the TX proof's line
/// ends `consistent true` only when its ring filled ([`is_unfilled_tx_ring`]). Each
/// processor's own line and the TX line itself are still required.
fn kvm_markers(scenario: &mut Scenario, vt_d: bool) {
    if vt_d {
        kvm_pin_markers(scenario);
    }
    scenario
        .markers
        .retain(|m| !m.starts_with(EVEN_SHARES_LINE));
    let tx = scenario
        .markers
        .iter()
        .position(|m| m == "origin-kernel: virtio-net TX: ");
    match tx {
        Some(at)
            if scenario.markers.get(at + 1).map(String::as_str) == Some("; consistent true") =>
        {
            scenario.markers.remove(at + 1);
        }
        _ => scenario
            .markers
            .push("xtask: the virtio-net TX markers are not as expected".to_string()),
    }
}

/// What the VT-d pin proof can show under KVM. Its last two steps take a remapped pin's IRTE
/// away and then make it check the wrong source ID, and expect the interrupts to stop. With
/// KVM's split irqchip QEMU keeps the route it already has when the IRTE goes away, so they
/// keep arriving, no fault is recorded and the kernel rightly reports a failure. TCG proves
/// those two steps; here the rest of each line must still hold, the restored IRTE carrying
/// interrupts again. Every other marker and failure message stays in force.
fn kvm_pin_markers(scenario: &mut Scenario) {
    let first = " not present: 0 interrupts arrived in 100 ms; fault recorded: source ID 0xffff, interrupt index ";
    let after = "origin-kernel: I/O APIC: remapped: the entry masked, the PIT stopped and IRTE ";
    let start = scenario.markers.iter().position(|m| m == first);
    let end = start.and_then(|s| {
        scenario.markers[s..]
            .iter()
            .position(|m| m == after)
            .map(|n| s + n)
    });
    match (start, end) {
        (Some(s), Some(e)) => {
            let restored = "; fault recorded: none; IRTE restored, ";
            let failed = " again: true; consistent false: I/O APIC failed";
            scenario.markers.splice(
                s..e,
                [
                    " not present: ",
                    restored,
                    failed,
                    "origin-kernel: I/O APIC: IRTE ",
                    " checking source ID 0xff01 instead of 0xff00: ",
                    restored,
                    failed,
                ]
                .map(String::from),
            );
        }
        _ => scenario
            .markers
            .push(format!("xtask: the VT-d marker {first:?} is missing")),
    }
}

/// Whether a boot's processor says it is Intel's: a `vendor=` in the model decides. Without
/// one, TCG's `max` is AMD's, and under KVM the processor reports the host's vendor.
fn guest_is_intel(model: &str, accel: Accel, host_is_intel: bool) -> bool {
    if model.contains("vendor=") {
        return model.contains("vendor=GenuineIntel");
    }
    accel == Accel::Kvm && host_is_intel
}

/// Whether this host's processor is Intel's (read only for KVM, which needs a Linux host).
fn host_is_intel() -> bool {
    fs::read_to_string("/proc/cpuinfo").is_ok_and(|info| info.contains("GenuineIntel"))
}

/// True for a line reporting a failure under KVM that says nothing about the kernel. Every
/// other failure fails the boot.
fn kvm_forgives(line: &str) -> bool {
    is_pin_limit(line)
        || is_small_tsc_step(line)
        || is_unfilled_tx_ring(line)
        || is_uneven_fair_share(line)
}

/// The failure messages of the lines [`kvm_forgives`] lets pass.
const KVM_FORGIVEN_MESSAGES: [&str; 4] = [
    IOAPIC_FAILED,
    CLOCK_FAILED,
    ": virtio-net failed",
    ": scheduler failed",
];

/// The virtio-net TX proof's line when the only thing missing is a ring that filled. The
/// proof queues one frame more than the ring holds, without a kick, expecting the ring to
/// fill. QEMU's device runs in its own thread and, after an earlier kick, can go on taking
/// frames as they appear, so on a fast enough processor the batch drains as it is queued.
/// Everything else the proof asks for must still be there.
fn is_unfilled_tx_ring(line: &str) -> bool {
    if !line.starts_with("origin-kernel: virtio-net TX: ")
        || !line.contains("; the ring was full 0 times and senders slept ")
        || !line.ends_with("; consistent false: virtio-net failed")
    {
        return false;
    }
    let number = |word| number_before(line, word);
    match (
        number(" frames through "),
        number(" descriptors ("),
        number(" completions reclaimed"),
        number(" of "),
        number(" free at the end"),
        number(" times, woken by"),
        number(" (of "),
        number("); consistent"),
    ) {
        (
            Some(sent),
            Some(descriptors),
            Some(reclaimed),
            Some(free),
            Some(of),
            Some(slept),
            Some(woken),
            Some(interrupts),
        ) => {
            sent > descriptors
                && reclaimed == sent
                && free == descriptors
                && of == descriptors
                && slept > 0
                && woken > 0
                && interrupts > 0
        }
        _ => false,
    }
}

/// How unequal two workers' counts [`is_uneven_fair_share`] lets pass: the larger under this
/// many times the smaller.
const KVM_FAIR_SHARE_FACTOR: u64 = 4;

/// The scheduler proof's line for a processor whose two workers both ran, for enough rounds,
/// but got unequal amounts done. The proof counts loop iterations, not processor time, and a
/// runner's virtual processors share physical cores: a Xeon 8370C showed 2.1 to 2.5 times.
/// TCG holds the proof to a factor of two.
fn is_uneven_fair_share(line: &str) -> bool {
    if !line.starts_with("origin-kernel: scheduler: CPU ")
        || !line.ends_with(" context switches: scheduler failed")
    {
        return false;
    }
    let Some((_, counts)) = line.split_once(" counted ") else {
        return false;
    };
    match (
        number_before(counts, " and "),
        number_before(counts, ", "),
        number_before(counts, " context switches"),
    ) {
        (Some(first), Some(second), Some(switches)) => {
            let (low, high) = (first.min(second), first.max(second));
            low > 0 && high < low.saturating_mul(KVM_FAIR_SHARE_FACTOR) && switches >= 15
        }
        _ => false,
    }
}

/// The two lines of [`kvm_pin_markers`], exactly as KVM leaves them: no fault was recorded and
/// the interrupts came back.
fn is_pin_limit(line: &str) -> bool {
    line.starts_with("origin-kernel: I/O APIC: IRTE ")
        && (line.contains(" not present: ") || line.contains(" checking source ID "))
        && line.contains("; fault recorded: none; IRTE restored, ")
        && line.ends_with(" again: true; consistent false: I/O APIC failed")
}

/// The largest step back in the TSC [`is_small_tsc_step`] lets pass: under half a microsecond
/// at the rates these machines run.
const KVM_TSC_STEP_LIMIT: u64 = 1000;

/// The TSC proof's line when all it saw was a rare, tiny step back. On a runner the guest's
/// processors are virtual processors of a virtual machine, which the hypervisors below can
/// move between physical processors whose counters agree only to within nanoseconds: an AMD
/// EPYC 9V45 runner showed one step of 16 ticks in 261,083 handoffs. A steady offset between
/// two processors would step back on nearly every handoff one way, and a large step is not
/// this, so neither is forgiven. TCG, whose processors share one clock, holds the proof to
/// zero.
fn is_small_tsc_step(line: &str) -> bool {
    if !line.starts_with("origin-kernel: clock: TSC consistent across ") {
        return false;
    }
    let tail = " ticks): clock failed";
    match (
        number_before(line, " handoffs between"),
        number_before(line, " backwards (worst "),
        number_before(line, tail),
    ) {
        (Some(handoffs), Some(backwards), Some(worst)) => {
            line.ends_with(tail)
                && backwards >= 1
                && backwards.saturating_mul(1000) <= handoffs
                && worst < KVM_TSC_STEP_LIMIT
        }
        _ => false,
    }
}

/// The decimal number written immediately before the first `word` in `text`.
fn number_before(text: &str, word: &str) -> Option<u64> {
    let head = text.get(..text.find(word)?)?;
    let digits = head.len() - head.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    head.get(head.len() - digits..)?.parse().ok()
}

/// The `boot-bridge` machine (`ORIGIN_QEMU_BRIDGE=on`): the NIC at 01:00.0 behind a PCIe root
/// port at 00:04.0, one more function than the default machine.
fn bridge_markers(scenario: &mut Scenario) {
    let amd = qemu_iommu() == Iommu::Amd;
    let count = if amd { 9 } else { 8 };
    let swaps: [(&str, String); 6] = [
        (
            "origin-kernel: PCIe: bus mastering: the firmware left it on for 7 of 7 functions (",
            "origin-kernel: PCIe: bus mastering: the firmware left it on for 8 of 8 functions (".into(),
        ),
        (
            "origin-kernel: PCIe: bus mastering: 1 of 7 functions have a PCI Express capability; Transactions Pending clear on 1 of them after waiting ",
            "origin-kernel: PCIe: bus mastering: 3 of 8 functions have a PCI Express capability; Transactions Pending clear on 3 of them after waiting ".into(),
        ),
        (
            "functions on 1 buses; every BAR sized with decoding off and restored: true",
            "functions on 2 buses; every BAR sized with decoding off and restored: true".into(),
        ),
        (
            "origin-kernel: PCIe: bus mastering: off on all 7 functions before any driver attached: true",
            "origin-kernel: PCIe: bus mastering: off on all 8 functions before any driver attached: true".into(),
        ),
        (
            "origin-kernel: PCIe: bus mastering: on for exactly the 2 attached function(s) (00:02.0 domain 1, 00:03.0 domain 2) and the 0 bridge(s) above them, off on the other 5; IOMMU translating true; consistent true",
            "origin-kernel: PCIe: bus mastering: on for exactly the 2 attached function(s) (00:02.0 domain 1, 01:00.0 domain 2) and the 1 bridge(s) above them, off on the other 5; IOMMU translating true; consistent true".into(),
        ),
        (" 00:03.0 domain 2 (", " 01:00.0 domain 2 (".into()),
    ];
    let mut swap = |old: &str, new: String| match scenario.markers.iter().position(|m| m == old) {
        Some(i) => scenario.markers[i] = new,
        None => scenario
            .markers
            .push(format!("xtask: the marker {old:?} is missing")),
    };
    for (old, new) in swaps {
        swap(old, new);
    }
    // The AMD-Vi device table reaches the highest DeviceID, now on bus 1.
    if amd {
        swap(
            "origin-kernel: IOMMU: unit 0 at 0xfed80000 (segment 0, IVHD type 0x11 of 2, IOMMU function 00:05.0, capability at 0x40): 9 device entries up to DeviceID 0x00fb, I/O APIC 0 (DeviceID 0x00a0)",
            "origin-kernel: IOMMU: unit 0 at 0xfed80000 (segment 0, IVHD type 0x11 of 2, IOMMU function 00:05.0, capability at 0x40): 10 device entries up to DeviceID 0x01ff, I/O APIC 0 (DeviceID 0x00a0)".into(),
        );
        swap(
            "origin-kernel: IOMMU: unit 0: device table of 256 entries at 0x",
            "origin-kernel: IOMMU: unit 0: device table of 512 entries at 0x".into(),
        );
    }
    for m in &mut scenario.markers {
        if m.starts_with("bus/pci/count=") {
            *m = format!(
                "bus/pci/count={count}, block/nvme0/size_mib=16; read back pci count Some({count})"
            );
        }
    }
}

/// What the kernel reports about its processors' x2APIC IDs: for each one above 255, its
/// interrupts reach it through an IRTE holding its full ID (NVMe MSI-X entry `2 + index`)
/// and wake its threads; with none, that there is nothing to prove.
fn x2apic_markers(machine: &Machine) -> Vec<String> {
    let wide: Vec<(u32, u32)> = (0..machine.cpus())
        .zip(machine.apic_ids.iter().copied())
        .filter(|&(_, id)| id > 0xFF)
        .collect();
    if wide.is_empty() {
        let largest = machine.apic_ids.iter().copied().max().unwrap_or(0);
        return vec![format!(
            "origin-kernel: x2APIC: no online processor has an APIC ID above 255 (the largest is {largest})"
        )];
    }
    wide.into_iter()
        .flat_map(|(index, id)| {
            [
                format!(
                    "origin-kernel: x2APIC: {} is above 255, which a compatibility-format MSI address cannot carry (refused true); its NVMe MSI-X entry {} holds only remappable handle ",
                    machine.named(index),
                    2 + index
                ),
                format!(
                    ", whose IRTE sends vector 0x40 to destination {id} after checking source ID 0x0010: true; "
                ),
                "; consistent true".into(),
            ]
        })
        .collect()
}

/// The AMD-Vi boot (`ORIGIN_QEMU_IOMMU=amd`): the IOMMU's own PCI function, its bring-up from
/// the IVRS, and the AMD-Vi wording of the IOMMU proofs replace the VT-d markers; every
/// other marker is the same. Two lines pin what QEMU 11.1's `amd-iommu` does not do (research
/// ledger `docs/research/2026-11-08-amd-vi-iommu.md` §2.5): its events carry address 0, and
/// it logs no interrupt-remapping fault, which it reports on its stderr instead.
fn amd_iommu_markers(scenario: &mut Scenario, machine: &Machine) {
    let cpus = machine.cpus();
    let swaps: [(String, Vec<String>); 13] = [
        (
            "origin-kernel: PCIe: 00:02.0 1b36:0010 class 01:08:02 NVMe controller; BAR0 mem64 16 KiB at ".into(),
            vec![
                "origin-kernel: PCIe: 00:02.0 1b36:0010 class 01:08:02 NVMe controller; BAR0 mem64 16 KiB at ".into(),
                "origin-kernel: PCIe: 00:05.0 1022:1419 class 08:06:00 IOMMU".into(),
            ],
        ),
        (
            "origin-kernel: IOMMU: DMAR: host address width 48 bits, interrupt remapping true".into(),
            vec!["origin-kernel: IOMMU: IVRS revision 1, IVinfo 0x2801 (EFR support true, 40-bit physical and 0-bit virtual addresses, pre-boot DMA protection false); 1 IOMMU(s) in 2 IVHD(s), 0 memory definition(s)".into()],
        ),
        (
            "origin-kernel: IOMMU: unit 0 at 0xfed90000 (segment 0, 4 KiB of registers): 7 PCI scope(s), I/O APIC 0 (source ID 0xff00)".into(),
            vec!["origin-kernel: IOMMU: unit 0 at 0xfed80000 (segment 0, IVHD type 0x11 of 2, IOMMU function 00:05.0, capability at 0x40): 9 device entries up to DeviceID 0x00fb, I/O APIC 0 (DeviceID 0x00a0)".into()],
        ),
        (
            "origin-kernel: IOMMU: unit 0: VT-d 1.0; CAP 0x".into(),
            vec!["origin-kernel: IOMMU: unit 0: AMD-Vi capability revision 0, IOTLB true, not-present entries cached true; EFR 0x29d7 (register 0x29d7, IVRS image 0x29d7): x2APIC true, guest APIC true, INVALIDATE_IOMMU_ALL true, 6-level host tables, performance counters false; EFR2 0x0: 512 interrupts per function".into()],
        ),
        (
            "queued invalidation true, interrupt remapping true, extended interrupt mode true".into(),
            vec![],
        ),
        (
            "origin-kernel: IOMMU: unit 0: queued invalidation on (256 descriptors); empty root table at 0x".into(),
            vec!["origin-kernel: IOMMU: unit 0: device table of 256 entries at 0x".into()],
        ),
        (
            "interrupt remapping on (256 entries, 32-bit x2APIC destinations, compatibility format blocked); translation on (GSTS 0x".into(),
            vec!["every entry blocking DMA and fixed interrupts; command buffer and event log on (256 entries each); interrupt remapping per device (128-bit entries, 32-bit x2APIC destinations); translation on (control 0x4000000021007, status 0x18): every device's DMA is blocked until its driver attaches it; caches invalidated (INVALIDATE_IOMMU_ALL, then every device table entry and interrupt table); 0 stale event(s) cleared".into()],
        ),
        (
            "every device's DMA faults until its driver attaches it; 0 stale fault(s) cleared".into(),
            vec![],
        ),
        (
            "and the page kept its canary true; fault recorded: source ID 0x0010, write at 0x".into(),
            vec!["unmapped (the translation cache invalidated page-selectively and the completion awaited), the same read completed with status type 0 code 0x0, and the page kept its canary true; fault recorded: DeviceID 0x0010, access at 0x0, domain 0, event 0x2 (I/O page fault: page not present); consistent true".into()],
        ),
        (
            "reason 0x5 (write to a page not mapped writable); consistent true".into(),
            vec![],
        ),
        (
            format!(
                "origin-kernel: IOMMU: interrupt remapping: {} of {} MSI-X entries (NVMe {}, virtio-net 2) hold only a remappable handle with data 0",
                3 + cpus,
                3 + cpus,
                1 + cpus
            ),
            vec![format!(
                "origin-kernel: IOMMU: interrupt remapping: {} of {} MSI-X entries (NVMe {}, virtio-net 2) hold only an index into their device's own interrupt remapping table, whose IRTE holds the vector and the x2APIC destination",
                3 + cpus,
                3 + cpus,
                1 + cpus
            )],
        ),
        (
            "left not present: a read on I/O queue 2 completed true, 0 interrupts arrived; fault recorded: source ID 0x0010, interrupt index ".into(),
            vec!["left not present: a read on I/O queue 2 completed true, 0 interrupts arrived; fault recorded: none; vector 1 restored; consistent true".into()],
        ),
        (
            "reason 0x22 (interrupt table entry not present); vector 1 restored; consistent true".into(),
            vec![],
        ),
    ];
    for (old, new) in swaps {
        match scenario.markers.iter().position(|m| *m == old) {
            Some(at) => {
                scenario.markers.splice(at..=at, new);
            }
            // Unmatchable on purpose, so a renamed VT-d marker fails the AMD boot loudly.
            None => scenario
                .markers
                .push(format!("xtask: the VT-d marker {old:?} is missing")),
        }
    }
    // The last proof line and the PCI count are shared with other markers' text, so they are
    // swapped in place.
    for m in &mut scenario.markers {
        if m == "no other function has a context entry, so its DMA faults; IOVAs from 0x40000000; faults recorded before the proofs: 0; consistent true"
        {
            *m = "every other function's device table entry blocks its DMA and fixed interrupts; IOVAs from 0x40000000; faults recorded before the proofs: 0; consistent true".into();
        }
        if m == "bus/pci/count=7, block/nvme0/size_mib=16; read back pci count Some(7)" {
            *m = "bus/pci/count=8, block/nvme0/size_mib=16; read back pci count Some(8)".into();
        }
    }
    // The I/O APIC's pins are routed through IRTEs under VT-d only: under AMD-Vi the kernel
    // reports that they stay masked, in one line, where the VT-d proof's lines were.
    let first = "origin-kernel: I/O APIC: remapped: the PIT's IRQ0 (GSI 2, edge-triggered, active-high) through IRTE ";
    let start = scenario.markers.iter().position(|m| m == first);
    let end = start.and_then(|s| {
        scenario.markers[s..]
            .iter()
            .position(|m| m == " released: true")
            .map(|n| s + n)
    });
    match (start, end) {
        (Some(s), Some(e)) => {
            scenario.markers.splice(
                s..=e,
                ["origin-kernel: I/O APIC: remapped: not under AMD-Vi yet: the I/O APIC's device table entry blocks its fixed interrupts until it has an interrupt table, so its pins stay masked".into()],
            );
        }
        _ => scenario
            .markers
            .push(format!("xtask: the VT-d marker {first:?} is missing")),
    }
    // QEMU reports the probe interrupt it blocked (its IRTE has RemapEn clear) on its stderr.
    scenario
        .stderr
        .push("failed to remap interrupt from devid 0x10");
}

fn boot_scenario_markers(machine: &Machine) -> Scenario {
    use origin_version::{BUILD, PRODUCT, VERSION};
    let cpus = machine.cpus();
    // The processor the interrupt-driven NVMe and virtio-net threads run on: the first
    // one after the boot processor, if any.
    let target = machine.named(cpus.min(2) - 1);
    Scenario {
        label: "kernel boot",
        // Developer mode off: the signed kernel of `boot_test`, against the test key.
        loader: Loader::Verifying,
        markers: vec![
            format!("{PRODUCT} {VERSION} UEFI loader (build {BUILD})"),
            "origin-boot: UEFI 2.".into(),
            "origin-boot: read \\EFI\\ORIGIN\\KERNEL.ONBF (".into(),
            "origin-boot: TEST trust store: this loader trusts a development key and must never ship".into(),
            "origin-boot: kernel signature verified: origin-kernel, Ed25519 and ML-DSA-87 both good, key ID ".into(),
            "origin-boot: kernel verified: ".into(),
            // The loader's own random-number generator (ADR-0016 R10): the self-test passed
            // and an instruction passed its start-up test.
            "origin-boot: random: known-answer test passed; ".into(),
            "; the loader's generator is seeded".into(),
            "origin-boot: kernel placed at ".into(),
            // The firmware's framebuffer, for the kernel's console (ADR-0010).
            format!(
                "origin-boot: framebuffer: {QEMU_DISPLAY} at 0x"
            ),
            // Two slides, each from its own request.
            "origin-boot: KASLR: kernel at 0xffffffff".into(),
            "(two requests to the loader's generator)".into(),
            "origin-boot: page tables built: ".into(),
            // The kernel's seed is handed over and the loader's generator is gone.
            "origin-boot: random: 64 bytes for the kernel's generator and 8 for its stacks' region are in the boot info; the loader's generator is uninstantiated, and its state, seed area and stack are overwritten".into(),
            "origin-boot: exiting boot services; entering the Framekernel".into(),
            // Attached before anything else is printed, so everything below is also on the
            // screen; `check_screen` holds the kernel to that.
            format!("origin-kernel: console: framebuffer {QEMU_DISPLAY} at 0x"),
            format!("origin-kernel: {PRODUCT} {VERSION} Framekernel (build {BUILD})"),
            format!(
                "origin-kernel: boot info {}.{}, ",
                origin_version::BOOT_INFO_MAJOR,
                origin_version::BOOT_INFO_MINOR
            ),
            "origin-kernel: loaded at ".into(),
            // Boot info 0.2: the verified segment table, in RVA order (R, RX, the RELRO part
            // of the data, which the loader hands over without write access, then RW).
            "origin-kernel: kernel segment 0: R at ".into(),
            "origin-kernel: kernel segment 1: RX at ".into(),
            "origin-kernel: kernel segment 2: R at ".into(),
            "origin-kernel: kernel segment 3: RW at ".into(),
            "origin-kernel: GDT, TSS and IDT loaded (32 exception vectors)".into(),
            // An image offset, never the image's base (ADR-0004 Amendment 2, A6.2).
            "origin-kernel: exception 3 (#BP breakpoint) at rip image +0x".into(),
            "origin-kernel: breakpoint handled; resuming".into(),
            // CPU features: QEMU's `max` CPU has every x86-64-v3 feature, so the kernel must
            // find the baseline met (CLAUDE.md §14); below it, the kernel refuses to boot.
            "origin-kernel: CPU: x86-64 level 3 (baseline v3 true)".into(),
            // A frame-pointer backtrace of a known call chain (CLAUDE.md §69).
            "origin-kernel: backtrace: 4 frames from a known call chain, all in the kernel image true".into(),
            "origin-kernel: frame allocator: ".into(),
            "origin-kernel: page tables loaded: ".into(),
            "origin-kernel: higher half: kernel image identity address mapped false, direct-map alias mapped false".into(),
            "origin-kernel: page protection: write to kernel code at ".into(),
            "faulted (#PF error 0x3)".into(),
            "origin-kernel: page protection: write to read-only kernel data at ".into(),
            "origin-kernel: page protection: instruction fetch from writable data at ".into(),
            "faulted (#PF error 0x11)".into(),
            "origin-kernel: page protection: read of page 0 at 0x0 faulted (#PF error 0x0)".into(),
            "origin-kernel: page protection: read of the boot stack's guard page at ".into(),
            // QEMU's firmware publishes no SPCR, so the console keeps COM1 (ADR-0010).
            format!("origin-kernel: console: {NO_SPCR}"),
            // After the switch, so this also proves the direct map.
            "origin-kernel: allocated, wrote and freed 3 frames at ".into(),
            // Threat T4 (ADR-0007 I12): as soon as the kernel can reach configuration space,
            // bus mastering is cleared on every function and read back. OVMF leaves it on for
            // all of them (QEMU resets them all clear).
            "origin-kernel: PCIe: ECAM for segment 0, buses 0-255, at 0x".into(),
            ": uncached true, 2 MiB pages".into(),
            "origin-kernel: PCIe: bus mastering: the firmware left it on for 7 of 7 functions (".into(),
            "; every cleared function reads back clear: true".into(),
            "origin-kernel: heap: Vec of 100000 u64, 2000 boxes, B-tree of 1000 keys, a string: "
                .into(),
            "origin-kernel: heap: everything freed; ".into(),
            "origin-kernel: page protection: SMEP: supervisor fetch from a user page at 0x7fffffffe000 faulted (#PF error 0x11)".into(),
            "origin-kernel: page protection: SMAP: supervisor read of a user page at 0x7fffffffe000 faulted (#PF error 0x1)".into(),
            "origin-kernel: x2APIC enabled: APIC ID 0, ".into(),
            "origin-kernel: APIC timer calibrated against the PIT: ".into(),
            "origin-kernel: APIC timer: 50 interrupts at 100 Hz in ".into(),
            // The wait for Transactions Pending, once the TSC is calibrated: none is pending.
            "origin-kernel: PCIe: bus mastering: 1 of 7 functions have a PCI Express capability; Transactions Pending clear on 1 of them after waiting ".into(),
            "origin-kernel: NMI taken on its own stack (IST2): frame at image +0x".into(),
            // The clocks (CLAUDE.md §68): the TSC rate proven against one RTC second, and
            // the wall clock read from the RTC, which `qemu_command` starts at RTC_BASE.
            "origin-kernel: clock: TSC at ".into(),
            "origin-kernel: clock: one RTC second lasted ".into(),
            format!("origin-kernel: wall clock: {RTC_BASE_MARKER}"),
            // The HPET: discovered via ACPI, its counter rate cross-checked against the TSC.
            "origin-kernel: HPET: at 0xfed00000, 100 MHz counter".into(),
            // The ACPI PM timer: a second independent time source cross-checked vs the TSC.
            "origin-kernel: PM timer: 24-bit at port 0x608, 3.579545 MHz;".into(),
            // I/O APIC: the PIT's IRQ0 routed through it (IRQ0 -> GSI 2 via an override).
            "origin-kernel: I/O APIC: id 0, 24 entries at 0xfec00000 (GSI base 0); routing the PIT's IRQ0 via GSI 2 to vector 0x41 on CPU 0".into(),
            "origin-kernel: I/O APIC: 20 PIT interrupts routed through the I/O APIC in ".into(),
            // Intel VT-d (CLAUDE.md §32, §58; ADR-0007): before the application processors
            // start (T4), the DMAR QEMU builds for q35 with `intremap=on` (one unit at 0xfed90000
            // whose scope lists every PCI function and the I/O APIC), the unit's capabilities,
            // then queued invalidation, interrupt remapping and translation on, so every DMA
            // faults until a driver attaches its device.
            "origin-kernel: IOMMU: DMAR: host address width 48 bits, interrupt remapping true".into(),
            "origin-kernel: IOMMU: unit 0 at 0xfed90000 (segment 0, 4 KiB of registers): 7 PCI scope(s), I/O APIC 0 (source ID 0xff00)".into(),
            "origin-kernel: IOMMU: unit 0: VT-d 1.0; CAP 0x".into(),
            "queued invalidation true, interrupt remapping true, extended interrupt mode true".into(),
            "origin-kernel: IOMMU: unit 0: queued invalidation on (256 descriptors); empty root table at 0x".into(),
            "interrupt remapping on (256 entries, 32-bit x2APIC destinations, compatibility format blocked); translation on (GSTS 0x".into(),
            "every device's DMA faults until its driver attaches it; 0 stale fault(s) cleared".into(),
            "origin-kernel: IOMMU: translating ".into(),
            // The random-number generator's root and the boot processor's own generator
            // (ADR-0016 R5, R13), before another processor starts: the self-test passed and
            // an instruction passed its start-up test.
            // The loader's value for the kernel stacks' region (decision item 166): taken once,
            // by the code that places that region while the kernel builds its page tables
            // (`stack::draw_region`), so a take at this point is refused.
            "origin-kernel: random: the loader's value for the kernel stacks' region: taken once before this point, and a take here was refused".into(),
            "origin-kernel: random: root: known-answer test passed; ".into(),
            "; the loader's seed mixed in and overwritten where it lay; the boot processor's generator seeded".into(),
            // M3: every processor QEMU emulates (`-smp`, see `machine`) comes online. The
            // MADT lists every possible processor; each enabled one gets a CPU index.
            format!(
                "origin-kernel: ACPI MADT: {} processors, {cpus} enabled (largest APIC ID listed {})",
                machine.listed, machine.highest
            ),
            format!(
                "origin-kernel: SMP: {cpus} of {cpus} enabled processors online; trampoline identity pages removed true"
            ),
            // Every processor's interrupt stacks end at a guard page, and each processor set
            // its own control state (CLAUDE.md §24, §58).
            format!(
                "origin-kernel: page protection: interrupt-stack guard pages: {0} of {0} faulted (4 per processor)",
                4 * cpus
            ),
            format!(
                "origin-kernel: CPU state: machine checks raise #MC, x87 errors raise #MF, ring 3 has no RDPMC, FSGSBASE or SYSENTER, no debug breakpoint armed: {cpus} of {cpus} processors"
            ),
            // ADR-0017: what the processors reported and set as they started (which controls
            // depends on the machine, so only the line's start is fixed), and the lookup
            // that every index and address from ring 3 goes through, in both its forms.
            if cpus == 1 {
                "origin-kernel: speculation: 1 processor: on: ".into()
            } else {
                format!("origin-kernel: speculation: {cpus} processors alike: on: ")
            },
            "; lookups: ".into(),
            "; not offered: ".into(),
            "origin-kernel: nospec: both forms keep an index and an address in range: yes; in use: ".into(),
            " ns masked and ".into(),
            "origin-kernel: reclaimed ".into(),
            // A boot step out of turn, or run twice, does nothing (ADR-0011 N6).
            "origin-kernel: boot steps: out of turn refused true; a second run refused true".into(),
        ]
        .into_iter()
        // CPU topology from CPUID (leaf 1Fh, 0Bh): each processor, the layout, the MADT.
        .chain(topology_markers(
            machine,
            machine.geometry.has_extended_levels()
                && guest_is_intel(
                    &qemu_cpu_model(),
                    Accel::from_env().unwrap_or(Accel::Tcg),
                    host_is_intel(),
                ),
        ))
        .chain([
            // NUMA from the ACPI SRAT (QEMU is booted with two -numa nodes): every online
            // processor is placed in a node and memory is present.
            "origin-kernel: numa: ".into(),
            "; consistent true".into(),
            // Inter-node distances from the ACPI SLIT (QEMU -numa dist): diagonal 10, every
            // off-diagonal >= 10.
            "origin-kernel: slit: ".into(),
            "; consistent true".into(),
            // The random-number generator (ADR-0016): every processor has one, requests are
            // answered with values that differ, and its stack holds only zeros after a request.
            "origin-kernel: random: processor 0: ".into(),
            "; its generator seeded".into(),
            "origin-kernel: random: 16 of 16 values answered, all distinct true; 300 bytes in pieces true; with prediction resistance: ".into(),
            "origin-kernel: random: cost: a 64-byte request ".into(),
            "; only zeros on it after a request: true".into(),
            "origin-kernel: random: every processor has a generator true; consistent true".into(),
            // The guard is in the kernel stacks' region, whose addresses the boot never prints
            // (ADR-0004 Amendment 2, A6.3).
            "origin-kernel: page protection: the first thread's stack guard page faulted (#PF error 0x0)".into(),
            // M3: two non-yielding threads per processor, all preempted and all running.
            "origin-kernel: scheduler: CPU 0: workers 0 and 1 counted ".into(),
            format!(
                "origin-kernel: preemptive multitasking: {} non-yielding threads on {cpus} CPUs all ran",
                2 * cpus
            ),
            // Stride scheduling: shares follow the weights 1 : 4 : 16 on one CPU.
            "origin-kernel: scheduler: weights 1, 4 and 16 on CPU ".into(),
            " per mille (ideal 47, 190 and 761)".into(),
            // FPU/SIMD state is saved and restored across context switches.
            "origin-kernel: scheduler: 3 SIMD threads kept their XMM state across ".into(),
            " corruptions".into(),
            "origin-kernel: scheduler: yield alternated ".into(),
            // A thread is created whole or not at all.
            "origin-kernel: scheduler: spawns refused for want of an FPU area (Err(Full)) and on a full queue (Err(Full)) give back".into(),
            // M4 groundwork: ring 3, the one DPL-3 gate, and crash isolation.
            "origin-kernel: user mode: good program: reported Some(42), exited with Some(0); kernel carries on".into(),
            "origin-kernel: user mode: kernel read: reported Some(7), ended by vector 14 (error 0x5".into(),
            "origin-kernel: user mode: privileged instruction: reported Some(9), ended by vector 13 (error 0x0); kernel carries on".into(),
            "origin-kernel: user mode: two compute-bound user processes at once finished in ".into(),
            "reports 0b110; kernel stack follows every switch".into(),
            "origin-kernel: user mode: int 0x80 is not a way in: reported Some(77), ended by vector 13 (error 0x402); kernel carries on".into(),
            "origin-kernel: user mode: write: hello from ring 3".into(),
            "origin-kernel: user mode: copy from user: valid buffer -> OK, kernel address -> FAULT, unmapped user address -> FAULT; kernel carries on".into(),
            "origin-kernel: processes: PML4 ".into(),
            "at once and read back 0x400800 from 0x500000 (own values only: 0x400800); not mapped in the kernel's tables: true".into(),
            format!("origin-kernel: clock: TSC consistent across {cpus} processors: "),
            // ADR-0012: no caller is ever given less than before, on any processor; and what a
            // read costs, measured (CLAUDE.md rule 21).
            format!("origin-kernel: clock: monotonic across {cpus} processors: "),
            "origin-kernel: clock: reading it costs ".into(),
            // Every CPU printed its lines at once; `console_lines_intact` checks each one.
            format!(
                "origin-kernel: console: {cpus} processors printed {CONSOLE_LINES} lines each at once"
            ),
            // A thread's line, drawn a section at a time with interrupts on between them.
            "origin-kernel: console: a line drawn by a thread: ".into(),
            "origin-kernel: console: a thread's line took ".into(),
            // Lines from a writer that cannot wait fill the UART's queue, and the next writer
            // waits for room: `unwaited_lines_whole` reads each of them back.
            "origin-kernel: console: a line that had to wait for room: ".into(),
            " lines from a writer that cannot wait filled the UART's queue".into(),
            // ADR-0004 H5: global kernel pages on every CPU, nothing global in the lower
            // half; and TLB shootdowns: every other CPU's next read of each unmapped page
            // faults.
            format!(
                "origin-kernel: TLB: global pages on {cpus} of {cpus} processors; kernel's tables: Some(LeafCensus {{ upper_global: "
            ),
            ", upper_local: 0, lower_global: 0, lower_local: 0 })".into(),
            format!(
                "origin-kernel: TLB: {} other processors cached 3 pages; after one single-page shootdown (",
                cpus - 1
            ),
            format!(
                "{} of their {} reads faulted and 0 saw a stale translation",
                3 * (cpus - 1),
                3 * (cpus - 1)
            ),
            // Kernel wait queues: an in-order producer/consumer transfer between CPUs, and
            // a cross-CPU ping-pong whose wake-ups go by IPI.
            "origin-kernel: wait queues: 10000 of 10000 items from CPU ".into(),
            ", in order true, in ".into(),
            "origin-kernel: wait queues: ping-pong between CPU 0 and CPU ".into(),
            ": 1000 round trips, ".into(),
            // Timed waits: one times out at its deadline, one is signalled first.
            "origin-kernel: wait queues: an unsignalled 50 ms timed wait timed out true after ".into(),
            "ms; a 2000 ms timed wait signalled early returned in ".into(),
            " ms, timed out false".into(),
        ])
        // M4's first slice (ADR-0005 P1–P5, P8, P9): system calls between ring-3 processes.
        .chain(ipc_markers(machine.named(0), target.clone()))
        .chain(object_markers(&target))
        .chain(address_space_markers(cpus))
        .chain(identity_markers())
        .chain(launch_markers())
        .chain(rust_program_markers())
        .chain(directory_markers())
        .chain(memory_markers())
        .chain(events_ports_markers())
        .chain(stack_markers(machine))
        .chain([
            "origin-kernel: scheduler: exited threads' stacks reaped on their own CPUs: heap ".into(),
            // ADR-0017 Z3: the scheduler noted each change of process, with one barrier for
            // each where the processor has IBPB and none where it has not.
            "origin-kernel: ibpb: ".into(),
            format!(
                " changes of process on {cpus} processor{}, ",
                if cpus == 1 { "" } else { "s" }
            ),
            // M5 groundwork: q35's functions listed with their BARs sized and restored.
            "origin-kernel: PCIe: 00:00.0 8086:29c0 class 06:00:00 host bridge".into(),
            "origin-kernel: PCIe: 00:01.0 1234:1111 class 03:00:00 VGA-compatible display controller; BAR0 mem32 prefetchable 16 MiB at ".into(),
            "origin-kernel: PCIe: 00:02.0 1b36:0010 class 01:08:02 NVMe controller; BAR0 mem64 16 KiB at ".into(),
            "origin-kernel: PCIe: 00:1f.0 8086:2918 class 06:01:00 ISA bridge".into(),
            "origin-kernel: PCIe: 00:1f.2 8086:2922 class 01:06:01 SATA controller (AHCI); BAR4 I/O 32 B at ".into(),
            "; BAR5 mem32 4 KiB at ".into(),
            "functions on 1 buses; every BAR sized with decoding off and restored: true".into(),
            // T4: re-read before any driver binds; nothing turned bus mastering back on.
            "origin-kernel: PCIe: bus mastering: off on all 7 functions before any driver attached: true".into(),
            // M5: the NVMe driver against the test disk `disk` writes (CLAUDE.md §33).
            "origin-kernel: NVMe: 00:02.0 1b36:0010, NVMe 1.".into(),
            // T4: only after the controller has a domain and has been reset.
            "origin-kernel: NVMe: bus mastering on after the controller was attached and reset: command register 0x0007".into(),
            "origin-kernel: NVMe: controller ready: model \"QEMU NVMe Ctrl\", serial \"ORIGIN0001\"".into(),
            format!(
                "origin-kernel: NVMe: namespace 1: {} blocks of {} B ({} MiB)",
                disk::BLOCKS,
                disk::BLOCK,
                (disk::BLOCKS * disk::BLOCK) >> 20
            ),
            format!(
                "origin-kernel: NVMe: asked for {} I/O queue pairs, allocated ",
                2 + cpus
            ),
            "origin-kernel: NVMe: through I/O queue 1: LBA 0 has the MBR signature true, LBA 1 the GPT signature true, and 8 of 8 blocks from LBA 2048 match the boot test's pattern".into(),
            format!(
                "origin-kernel: NVMe: wrote LBA {}, flushed and read it back: match true; a read at LBA {} (past the end) failed with status type 0 code 0x80 (LBA out of range: true)",
                disk::BLOCKS - 64,
                disk::BLOCKS
            ),
            // The disk's GPT, both copies checked, and its partitions as `disk` wrote them.
            format!(
                "origin-kernel: GPT: primary copy valid, backup copy at LBA {} valid, the two agree true",
                disk::BLOCKS - 1
            ),
            format!(
                "origin-kernel: GPT: disk {}, usable blocks 34-{}, 128 entries of 128 B, 2 in use",
                disk::DISK_GUID,
                disk::BLOCKS - 34
            ),
            format!(
                "origin-kernel: GPT: partition 1: \"{}\", type {}, blocks {}-{} (",
                disk::PARTITIONS[0].0,
                disk::PARTITIONS[0].1,
                disk::PARTITIONS[0].2,
                disk::PARTITIONS[0].3
            ),
            format!(
                "origin-kernel: GPT: partition 2: \"{}\", type {}, blocks {}-{} (",
                disk::PARTITIONS[1].0,
                disk::PARTITIONS[1].1,
                disk::PARTITIONS[1].2,
                disk::PARTITIONS[1].3
            ),
            // Interrupt-driven completions: MSI-X vector 1 to another CPU (q35's NVMe has
            // 65 vectors, the table in BAR 0), a thread there blocking for each one.
            format!(
                "origin-kernel: NVMe: MSI-X: 65 vectors, table in BAR0 at offset 0x2000; I/O queue 2 completes to vector 1, sent to {target} as IDT vector 0x40"
            ),
            format!(
                "origin-kernel: NVMe: interrupt-driven: 64 reads of 8 blocks by a thread on {target}, each blocking until its completion interrupt: "
            ),
            format!("(the last on {target}), 512 of 512 blocks match"),
            // Per-CPU queues: every CPU at once through its own queue pair and vector, the
            // vector's destination its APIC ID.
        ])
        .chain((0..cpus).map(|index| {
            format!(
                "origin-kernel: NVMe: {}: 32 reads through its own queue pair, 256 of 256 blocks match, its write read back true, ",
                machine.named(index)
            )
        }))
        .chain([format!(
            "origin-kernel: NVMe: per-CPU queues: {cpus} threads at once, one per CPU, each with its own queue pair and MSI-X vector: {} reads, {} of {} blocks match",
            32 * cpus,
            256 * cpus,
            256 * cpus
        )])
        // Processors above APIC ID 255: NVMe interrupts reach them live through IRTEs.
        .chain(x2apic_markers(machine))
        .chain([
            // The block cache over NVMe: cold misses fetch, re-reads hit, eviction and
            // write-back work (CLAUDE.md §39).
            "origin-kernel: block cache: 4 pages (512 B blocks, 8 per page): 32 of 32 blocks matched the pattern; 4 device reads cold, re-reading 4 resident pages did 0 more (32 hits, 4 misses)".into(),
            "origin-kernel: block cache: a 5th page evicted the LRU clean page (one read, no write-back: true); wrote LBA 10240, flushed (1 device writes), evicted and read back changed: true".into(),
            // The virtio-net NIC brought up (features, MAC, DRIVER_OK, MSI-X); a receive thread
            // woken only by the NIC's interrupt resolves the gateway by ARP and takes a DHCP
            // lease (CLAUDE.md §31, §54).
            // T4: only after the device has its domain.
            "origin-kernel: virtio-net: bus mastering on after the device got its DMA path: command register 0x0007".into(),
            "origin-kernel: virtio-net: 52:54:00:aa:bb:cc, features_ok true".into(),
            format!("; receive thread on {target}: slept "),
            format!(", the last on {target}); consistent true"),
            // Its transmit ring: more frames than descriptors, every one completed and its
            // descriptor reclaimed and reused; the TX-completion interrupt fired.
            "origin-kernel: virtio-net TX: ".into(),
            "; consistent true".into(),
            // Traffic on the lease: the gateway answers every echo request sent from the
            // leased address to the MAC ARP resolved.
            "origin-kernel: virtio-net ping: 10.0.2.15 -> 10.0.2.2 (52:55:0a:00:02:02): 4 of 4 echo replies".into(),
            "; consistent true".into(),
            // A file over the lease: the kernel's initramfs image fetched by TFTP from the
            // gateway (4608 bytes = nine full blocks and an empty tenth), identical to the
            // embedded copy, through recycled receive buffers.
            "origin-kernel: virtio-net TFTP: initramfs.tar from 10.0.2.2: 4608 bytes in 10 blocks (the last empty), identical to the embedded image".into(),
            "; consistent true".into(),
            // The fetched file — not the embedded copy — unpacked into a fresh VFS and read back.
            "origin-kernel: virtio-net initramfs: the fetched image extracted into a fresh VFS: 4 entries; /etc/hostname true, implicit /boot dir true; consistent true".into(),
            // The IOMMU's proofs through the NVMe controller (both drivers above already ran
            // through their own domains): a DMA through an IOVA lands in the physical page it
            // maps; after an unmap the same DMA is blocked and recorded; every MSI-X entry is a
            // handle to an IRTE, and an APIC ID above 255 is accepted; an interrupt through a
            // not-present IRTE is blocked and recorded; and nothing else faulted.
            "origin-kernel: IOMMU: translated DMA: the NVMe controller (00:02.0, source ID 0x0010) read LBA 2048 into IOVA 0x".into(),
            "(not the same address): 8 of 8 blocks landed in that physical page; consistent true".into(),
            "origin-kernel: IOMMU: blocked DMA: IOVA 0x".into(),
            "and the page kept its canary true; fault recorded: source ID 0x0010, write at 0x".into(),
            "reason 0x5 (write to a page not mapped writable); consistent true".into(),
            format!(
                "origin-kernel: IOMMU: interrupt remapping: {} of {} MSI-X entries (NVMe {}, virtio-net 2) hold only a remappable handle with data 0",
                3 + cpus,
                3 + cpus,
                1 + cpus
            ),
            "APIC ID 300 accepted (its IRTE reads back destination 300), which compatibility format refuses: true; consistent true".into(),
            "origin-kernel: IOMMU: blocked interrupt: NVMe MSI-X vector 1 pointed at IRTE ".into(),
            "left not present: a read on I/O queue 2 completed true, 0 interrupts arrived; fault recorded: source ID 0x0010, interrupt index ".into(),
            "reason 0x22 (interrupt table entry not present); vector 1 restored; consistent true".into(),
            "origin-kernel: IOMMU: domains: 00:02.0 domain 1 (".into(),
            " 00:03.0 domain 2 (".into(),
            "no other function has a context entry, so its DMA faults; IOVAs from 0x40000000; faults recorded before the proofs: 0; consistent true".into(),
            // The PIT's IRQ0 again, now through interrupt remapping (CLAUDE.md §24, §32;
            // ADR-0007 I8): the I/O APIC's entry in remappable format names an IRTE that
            // sends vector 0x41 to the target CPU (another than the boot processor when there
            // is one), edge-triggered like the pin, checking the I/O APIC's source ID from the
            // DMAR; the interrupts arrive there and nowhere else. With the IRTE not present,
            // none arrive and the unit records reason 0x22. QEMU's I/O APIC sends its messages
            // without a requester ID, so the fault carries 0xffff and the source-ID check
            // cannot be shown under QEMU: the kernel says so, and the IRTE is restored.
            "origin-kernel: I/O APIC: remapped: the PIT's IRQ0 (GSI 2, edge-triggered, active-high) through IRTE ".into(),
            format!(
                "the redirection entry reads back in remappable format with that index, vector 0x41 and mask clear: true; the IRTE reads back vector 0x41 to {target}, edge-triggered like the pin, checking source ID 0xff00 (I/O APIC 0's, from the DMAR): true; the two agree: true"
            ),
            "origin-kernel: I/O APIC: remapped: ".into(),
            format!(" PIT interrupts arrived on {target} through IRTE "),
            ", 0 on other CPUs; fault recorded: none; consistent true".into(),
            "origin-kernel: I/O APIC: IRTE ".into(),
            " not present: 0 interrupts arrived in 100 ms; fault recorded: source ID 0xffff, interrupt index ".into(),
            format!(
                "reason 0x22 (interrupt table entry not present), from a message without a requester ID (the DMAR names 0xff00); IRTE restored, 5 interrupts arrived on {target} again: true; consistent true"
            ),
            "origin-kernel: I/O APIC: IRTE ".into(),
            " checking source ID 0xff01 instead of 0xff00: ".into(),
            "fault recorded: none; the source-ID check cannot be shown on this platform".into(),
            "; consistent true".into(),
            "origin-kernel: I/O APIC: remapped: the entry masked, the PIT stopped and IRTE ".into(),
            " released: true".into(),
            // T4: with every driver bound, bus mastering is on exactly for the attached
            // functions and the bridges above them.
            "origin-kernel: PCIe: bus mastering: on for exactly the 2 attached function(s) (00:02.0 domain 1, 00:03.0 domain 2) and the 0 bridge(s) above them, off on the other 5; IOMMU translating true; consistent true".into(),
            // The in-memory VFS (CLAUDE.md §39): a file built, read back, listed and removed.
            "origin-kernel: vfs: built /etc/origin/hostname (read back true), listed 1 entry, removed all back to 1 node".into(),
            // §21: the embedded ustar initramfs extracts into a VFS (contents and an
            // implicit parent directory checked).
            "origin-kernel: initramfs: 4 entries into the VFS; /etc/hostname true, implicit /boot dir true; consistent true".into(),
            // The software rasterizer: the kernel's scene CRC matches the host-proven one.
            "origin-kernel: raster: 160x48 scene rendered, CRC 0x912df877 (expected 0x912df877)".into(),
            // The i8042 PS/2 controller answers its self-tests (CLAUDE.md §56).
            "self-test 0x55 (want 0x55), keyboard port test 0x00 (want 0x00)".into(),
            // The network packet layer: an ICMP echo built, parsed and replied via loopback.
            "origin-kernel: net: 65-byte Ethernet/IPv4/ICMP echo built, parsed and replied; a UDP datagram round-tripped true; payload and checksums intact true".into(),
            // TCP: a full loopback connection establishes, carries data and closes.
            "origin-kernel: tcp: loopback connection established, 30-byte payload delivered, and closed: true".into(),
            // The monotonic timer queue: three deadlines fire in order (one cancelled).
            "origin-kernel: timers: 3 deadlines (1 cancelled) fired in order [10, 20, 30], worst lateness ".into(),
            // Integration: a /sys tree built in the VFS from live discovered facts.
            "origin-kernel: sysfs: built /sys with 4 facts (4 subtrees): ".into(),
            "bus/pci/count=7, block/nvme0/size_mib=16; read back pci count Some(7)".into(),
            // A line sent to the UART in sections (`console::prove_uart_sections`, near the
            // end so its lines are on the screen for the screen check): on the UART before
            // its writer goes on; an interrupt taken before it was all sent, whose handler's
            // line came after it on the wire; a breakpoint in the middle of another; every
            // section one FIFO load at most; and what a section costs, measured (rule 21).
            "origin-kernel: console: a line sent on the UART a section at a time: ".into(),
            "origin-kernel: console: a line sent in sections while an interrupt waits: ".into(),
            "origin-kernel: console: this line comes from an interrupt handler".into(),
            "origin-kernel: console: a line sent in sections with a breakpoint in the middle: ".into(),
            "origin-kernel: exception 3 (#BP breakpoint) at rip".into(),
            "origin-kernel: breakpoint handled; resuming".into(),
            "origin-kernel: console: a line took ".into(),
            "origin-kernel: console: UART: ".into(),
            // A line drawn in sections (`console::prove_sections`, last so its lines are
            // on the screen for the screen check): an interrupt taken before it was finished,
            // whose handler's line queued behind it; a breakpoint in the middle of another;
            // every section at most 1 KiB of pixels; and what drawing a line costs, measured
            // (CLAUDE.md rule 21).
            "origin-kernel: console: a line drawn in sections while an interrupt waits: ".into(),
            "origin-kernel: console: this line comes from an interrupt handler".into(),
            "origin-kernel: console: a line drawn in sections with a breakpoint in the middle: ".into(),
            "origin-kernel: exception 3 (#BP breakpoint) at rip".into(),
            "origin-kernel: breakpoint handled; resuming".into(),
            "origin-kernel: console: a line took ".into(),
            "origin-kernel: console: framebuffer: ".into(),
            // ADR-0013: every proof was recorded. Whether it passed is the failure list's
            // job, line by line: a proof a machine forgives can fail here too.
            "origin-kernel: boot verdict: ".into(),
            format!(
                " of {} proofs passed",
                framekernel::bootproof::Proof::ALL.len()
            ),
            "origin-kernel: M1 entry complete; halting".into(),
        ])
        .collect(),
        failures: boot_failures(),
        check: Some(|done| {
            console_lines_intact(&done.serial, self::machine().cpus() as usize)?;
            long_lines_whole(&done.serial)?;
            unwaited_lines_whole(&done.serial)?;
            check_screen(done)
        }),
        stderr: vec![],
        capture: Capture::Com1AndScreen,
        qemu_args: None,
    }
}

/// Output that fails any kernel boot the moment it appears.
fn boot_failures() -> Vec<&'static str> {
    vec![
        "origin-boot: panic:",
        "origin-kernel: panic:",
        "origin-boot: kernel rejected",
        // The loader's generator: a failed self-test, no generator, or a stack that ran over.
        "origin-boot: random: known-answer test FAILED",
        "origin-boot: random: no seed for the kernel",
        "ran over its stack",
        "kernel stacks' region: WRONG",
        "origin-kernel: invalid boot info",
        "origin-kernel: invalid image",
        // ADR-0013: a proof whose failure stops the boot said so.
        "origin-kernel: boot refused",
        "(#DF double fault)",
        "(#GP general protection)",
        "(#PF page fault)",
        "(#UD invalid opcode)",
        "origin-kernel: machine check:",
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
        ": boot steps failed",
        ": scheduler failed",
        ": topology failed",
        ": CPU baseline failed",
        ": numa failed",
        ": slit failed",
        ": random failed",
        "origin-kernel: random: no generator",
        "with prediction resistance: WRONG",
        ": initramfs failed",
        ": virtio-net failed",
        "supervisor isolation from user pages incomplete",
        "origin-kernel: cannot map the user probe page",
        ": user isolation failed",
        ": NMI stack failed",
        ": higher half failed",
        ": process isolation failed",
        CLOCK_FAILED,
        ": HPET failed",
        IOAPIC_FAILED,
        ": PM timer failed",
        ": ipc failed",
        ": start failed",
        ": rust program failed",
        ": directory failed",
        ": memory failed",
        ": events and ports failed",
        ": user clock failed",
        ": console failed",
        ": TLB failed",
        ": address spaces failed",
        ": wait queues failed",
        ": PCIe failed",
        // No BAR on the test machines is refused, and the exhaustive quiesce sees every
        // function before enumeration does.
        "refused and treated as unimplemented",
        "was not seen by the quiesce",
        ": NVMe failed",
        ": block cache failed",
        ": backtrace failed",
        ": vfs failed",
        ": raster failed",
        ": i8042 failed",
        ": net failed",
        ": tcp failed",
        ": timers failed",
        ": sysfs failed",
        ": GPT failed",
        ": IOMMU failed",
        // ADR-0017: a control that did not read back, a processor that left no record, a
        // lookup out of range, an IBPB miscounted. A control the machine lacks is not here.
        // (The topology proof's healthy line says "0 recorded nothing": not that phrase.)
        "a control register did not read back as written",
        "left no record of its controls",
        ": nospec failed",
        ": ibpb failed",
    ]
}

/// The kernel's console, as QEMU's firmware sets it up: the mode, and the size of the first
/// console line it leads to.
const QEMU_DISPLAY: &str = "1280x800";
/// What the kernel says about the SPCR when the firmware has none (`console.rs`).
const NO_SPCR: &str =
    "no usable ACPI SPCR table (table not listed in the XSDT); the UART stays at I/O port 0x3f8";
/// Every scenario here boots the whole kernel, so it ends with this line.
const HALT_LINE: &str = "origin-kernel: M1 entry complete; halting";

/// How the line starts that the scheduler proof prints when every worker's share was even.
const EVEN_SHARES_LINE: &str = "origin-kernel: preemptive multitasking: ";
/// How every I/O APIC proof ends a line that failed.
const IOAPIC_FAILED: &str = ": I/O APIC failed";
/// How every clock proof ends a line that failed.
const CLOCK_FAILED: &str = ": clock failed";

/// The lines of `serial` that name an address in the window the kernel stacks' region slides
/// in, PML4 slots 384–447: a boot log must have none, during boot or after (ADR-0004 Amendment 2,
/// A6.3), or it gives the region's base away.
fn stack_window_lines(serial: &str) -> Vec<&str> {
    use framekernel::paging::layout::{STACK_WINDOW, STACK_WINDOW_SIZE};
    serial
        .lines()
        .filter(|line| {
            line.match_indices("0x").any(|(at, _)| {
                let digits = line.get(at + 2..).unwrap_or("");
                let end = digits
                    .find(|c: char| !c.is_ascii_hexdigit())
                    .unwrap_or(digits.len());
                u64::from_str_radix(&digits[..end], 16)
                    .is_ok_and(|v| v >= STACK_WINDOW && v - STACK_WINDOW < STACK_WINDOW_SIZE)
            })
        })
        .collect()
}

/// The start of the kernel stacks proof's line about the region's base: a boot that printed it
/// placed the region, so its base can be read (A6.4).
const STACK_PROOF_LINE: &str = "origin-kernel: kernel stacks: the region's base is 2 MiB-aligned";

/// The kernel's line, as it builds its tables, for a region left at its fixed base: the loader
/// handed over no random value for it (ADR-0004 Amendment 2, A5.1 and H3).
const STACK_FIXED_LINE: &str = "origin-kernel: kernel stacks: the loader handed over no random value: the region is at its fixed base";

/// Where each full kernel boot records the base it placed the kernel stacks' region at, read
/// through QEMU's monitor (ADR-0004 Amendment 2, A6.4). `boot-matrix` empties it and then checks
/// that its boots placed the region apart. The kernel prints nothing of a base; this file is the
/// host's, and nothing prints it.
fn stack_bases_file() -> PathBuf {
    target_dir().join("xtask").join("stack-region-bases")
}

/// The kernel's physical base, from its boot line `loaded at … (physical 0x…)`.
fn kernel_physical_base(serial: &str) -> Option<u64> {
    let line = serial
        .lines()
        .find(|l| l.starts_with("origin-kernel: loaded at "))?;
    let from = line.find("(physical 0x")? + "(physical 0x".len();
    let digits = line.get(from..)?;
    let end = digits.find(|c: char| !c.is_ascii_hexdigit())?;
    u64::from_str_radix(digits.get(..end)?, 16).ok()
}

/// The value of the first symbol whose name contains `part` in the ELF64 image `elf`. Only the
/// section headers, the symbol tables and their string tables are read; any other shape gives
/// `None`. The kernel is linked at 0, so a symbol's value is its RVA (`onbf-pack`).
fn elf_symbol(elf: &[u8], part: &str) -> Option<u64> {
    let bytes = |at: u64, n: usize| elf.get(usize::try_from(at).ok()?..)?.get(..n);
    let u16_at = |at: u64| bytes(at, 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |at: u64| bytes(at, 4)?.try_into().ok().map(u32::from_le_bytes);
    let u64_at = |at: u64| bytes(at, 8)?.try_into().ok().map(u64::from_le_bytes);
    if elf.get(..5)? != b"\x7fELF\x02" {
        return None;
    }
    let (table, entry, count) = (u64_at(0x28)?, u64::from(u16_at(0x3A)?), u16_at(0x3C)?);
    let section = |i: u64| table.checked_add(i.checked_mul(entry)?);
    (0..u64::from(count)).find_map(|i| {
        let header = section(i)?;
        if u32_at(header + 4)? != 2 {
            return None;
        }
        let (symbols, size) = (u64_at(header + 24)?, u64_at(header + 32)?);
        let strings = u64_at(section(u64::from(u32_at(header + 40)?))? + 24)?;
        (0..size / 24).find_map(|k| {
            let symbol = symbols.checked_add(k * 24)?;
            let name = elf.get(usize::try_from(strings + u64::from(u32_at(symbol)?)).ok()?..)?;
            let name = name.get(..name.iter().position(|&b| b == 0)?)?;
            std::str::from_utf8(name)
                .ok()?
                .contains(part)
                .then(|| u64_at(symbol + 8))?
        })
    })
}

/// Reads the base this boot placed the kernel stacks' region at, out of the halted kernel's
/// memory through QEMU's monitor, which the scenario has switched to (A6.4). The word is the
/// framework's `STACK_REGION_BASE`: the kernel's physical base, from its boot line, plus the
/// symbol's RVA in the kernel ELF. `rx` brings QEMU's output. Only a base a draw can give is
/// accepted: 2 MiB-aligned, the whole region in its window, and not the fixed base.
///
/// The one exception is a boot whose kernel said, in [`STACK_FIXED_LINE`], that the loader
/// handed over no random value: its word must be the fixed base and nothing else, and the
/// answer is `None`, since a fixed base is not one to compare across boots. Whether a machine
/// may say that line is its marker list's to decide, not this function's.
fn read_stack_base(
    monitor: &mut ChildStdin,
    rx: &mpsc::Receiver<Vec<u8>>,
    serial: &str,
) -> Result<Option<u64>> {
    use framekernel::paging::layout::{STACK_REGION, is_stack_region_base};
    let path = target_dir()
        .join(KERNEL_TARGET)
        .join("release")
        .join(KERNEL_PACKAGE);
    let elf = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let rva = elf_symbol(&elf, "STACK_REGION_BASE")
        .ok_or("the kernel ELF has no STACK_REGION_BASE symbol")?;
    let physical =
        kernel_physical_base(serial).ok_or("the kernel's boot line gives no physical base")?;
    let addr = physical
        .checked_add(rva)
        .ok_or("the word's physical address overflows")?;
    monitor
        .write_all(format!("xp /1gx {addr:#x}\n").as_bytes())
        .and_then(|()| monitor.flush())
        .map_err(|e| format!("cannot write to the monitor: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut out = Vec::new();
    loop {
        let chunk = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| format!("the monitor did not answer xp at {addr:#x}"))?;
        out.extend_from_slice(&chunk);
        let text = String::from_utf8_lossy(&out);
        // Split at newlines only: a whole answer line keeps the carriage return QEMU ends it with.
        let Some(digits) = text.split('\n').find_map(|line| xp_answer(line, addr)) else {
            continue;
        };
        // Only a whole number: the line goes on past its last digit.
        let Some(end) = digits.find(|c: char| !c.is_ascii_hexdigit()) else {
            continue;
        };
        let base = u64::from_str_radix(&digits[..end], 16)
            .map_err(|e| format!("the monitor's answer is not a number: {e}"))?;
        return stack_base_read(
            serial.contains(STACK_FIXED_LINE),
            base,
            STACK_REGION,
            is_stack_region_base(base),
        );
    }
}

/// What the word read as the kernel stacks' region's base means: `said_fixed` is whether the
/// kernel said the region is at its fixed base, `fixed` is that base, and `placeable` is whether
/// `base` is 2 MiB-aligned with the whole region in its window. A drawn base is `Some`; the fixed
/// base of a kernel that said so is `None`; anything else is an error.
fn stack_base_read(
    said_fixed: bool,
    base: u64,
    fixed: u64,
    placeable: bool,
) -> Result<Option<u64>> {
    match (said_fixed, base == fixed) {
        (true, true) => Ok(None),
        (true, false) => Err(
            "the kernel said the region is at its fixed base, and the word read is not that base"
                .into(),
        ),
        (false, false) if placeable => Ok(Some(base)),
        (false, _) => Err("the word read is not a base a draw can give".into()),
    }
}

/// The digits of the word `xp` printed for `addr`, from its answer line `<addr>: 0x<word>`. QEMU
/// pads the address to a width of its own choosing (8 digits below 4 GiB with QEMU 11.1), and its
/// line editor leaves escape codes before it, so the address is matched by value: the last run of
/// hexadecimal digits before the colon.
fn xp_answer(line: &str, addr: u64) -> Option<&str> {
    let (left, right) = line.split_once(": 0x")?;
    let digits = left.rsplit(|c: char| !c.is_ascii_hexdigit()).next()?;
    (u64::from_str_radix(digits, 16).ok()? == addr).then_some(right)
}

/// Appends this boot's base to the file `boot-matrix` compares (A6.4).
fn record_stack_base(label: &str, base: u64) -> Result {
    let path = stack_bases_file();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| writeln!(file, "{label}\t{base:#x}"))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// How many boots `recorded` (the lines of [`stack_bases_file`]) holds, if each placed the kernel
/// stacks' region at a base no other did (A6.4).
fn stack_bases_apart(recorded: &str) -> Result<usize> {
    let bases: Vec<&str> = recorded
        .lines()
        .filter_map(|line| line.split('\t').nth(1))
        .collect();
    let distinct: std::collections::HashSet<&str> = bases.iter().copied().collect();
    if distinct.len() != bases.len() {
        return Err(format!(
            "the kernel stacks' region was at {} distinct bases in {} boots: boots placed it alike",
            distinct.len(),
            bases.len()
        ));
    }
    Ok(bases.len())
}

/// As [`stack_bases_apart`], and there are at least two boots to compare: the end of
/// `boot-matrix`.
fn distinct_stack_bases(recorded: &str) -> Result<usize> {
    let boots = stack_bases_apart(recorded)?;
    if boots < 2 {
        return Err(format!(
            "{boots} boot(s) recorded where they placed the kernel stacks' region: nothing to compare"
        ));
    }
    Ok(boots)
}

/// Fails `boot-matrix` as soon as two of its boots so far placed the kernel stacks' region alike.
fn check_stack_bases_so_far(path: &Path) -> Result {
    let recorded = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    stack_bases_apart(&recorded).map(drop)
}

/// The screen after a whole boot: exactly what the serial log says it must show (the
/// console writes one line at a time to both).
fn check_screen(done: &Completed) -> Result {
    let image = done.screen.as_ref().ok_or("no screen dump was taken")?;
    check_screen_text(image, &done.serial)
}

/// The check itself: `image` against everything the log says the console printed.
fn check_screen_text(image: &screen::Image, serial: &str) -> Result {
    let start = serial
        .find("origin-kernel: console: framebuffer")
        .ok_or("the serial log has no framebuffer console line")?;
    let text = serial.get(start..).ok_or("serial log out of bounds")?;
    // The markers can be seen a moment before the serial port's last newline arrives; the
    // whole last line is on the screen by then or very soon after ([`settled_screen`]).
    let mut text = text.to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    screen::check_frame(image, &text)
}

/// How long [`settled_screen`] gives the screen to catch up with the log.
const SCREEN_SETTLE: Duration = Duration::from_secs(5);

/// The screen dump to check, once it has caught up with the log. The console sends each line
/// to the UART before it draws it (ADR-0010 Amendment 1), so a dump taken the moment the markers appear can be a line or so behind
/// the log: this takes dumps until one matches or the time is up. Whichever comes first, the
/// last one is returned for [`check_screen`] to judge, so a frame that never matches fails
/// the same way as before, with the difference named.
fn settled_screen(monitor: &mut ChildStdin, path: &Path, serial: &str) -> Result<screen::Image> {
    let mut image = dump_screen(monitor, path, true)?;
    let deadline = Instant::now() + SCREEN_SETTLE;
    while check_screen_text(&image, serial).is_err() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(200));
        image = dump_screen(monitor, path, false)?;
    }
    Ok(image)
}

/// Developer mode: an unsigned kernel still boots (ADR-0002 B8), and the loader says so.
fn developer_scenario() -> Scenario {
    use origin_version::{BUILD, PRODUCT, VERSION};
    Scenario {
        label: "developer mode unsigned kernel",
        loader: Loader::Developer,
        markers: vec![
            "origin-boot: read \\EFI\\ORIGIN\\KERNEL.ONBF (".into(),
            "origin-boot: developer mode: unsigned kernel images are accepted".into(),
            "origin-boot: kernel verified: ".into(),
            format!("origin-kernel: {PRODUCT} {VERSION} Framekernel (build {BUILD})"),
        ],
        failures: vec!["origin-boot: kernel rejected", "origin-boot: panic:"],
        check: None,
        stderr: vec![],
        capture: Capture::Com1,
        qemu_args: None,
    }
}

/// A scenario `loader` must refuse with `message` on the console, without ever entering the
/// kernel. `details` are further lines that must follow.
fn refusal_scenario(
    label: &'static str,
    loader: Loader,
    message: &str,
    details: &[&str],
) -> Scenario {
    let mut markers = vec![
        "origin-boot: read \\EFI\\ORIGIN\\KERNEL.ONBF (".to_string(),
        format!("origin-boot: kernel rejected: {message}"),
    ];
    markers.extend(details.iter().map(|d| (*d).to_string()));
    Scenario {
        label,
        loader,
        markers,
        failures: vec![
            "origin-kernel:",
            "origin-boot: panic:",
            "origin-boot: kernel verified",
        ],
        check: None,
        stderr: vec![],
        capture: Capture::Com1,
        qemu_args: None,
    }
}

/// What the kernel's first system-call slice must report, in order: a line per group of
/// hostile calls, each naming the status its call returned.
/// N14 step 3, S3-5a: address spaces (ADR-0004 H2), each boot proof's own lines.
fn address_space_markers(cpus: u32) -> Vec<String> {
    let (far, one) = if cpus > 1 {
        ("another processor", 1)
    } else {
        ("this processor", 0)
    };
    let cost = format!("Ok(Change {{ pages: 1, shootdowns: 1, interrupts: {one} }})");
    vec![
        "origin-kernel: address spaces: the kernel half is the same tables in every space: true; a kernel mapping made at 0xfffff00000000000, in a top-level slot of its own, after a space was created is visible from that space: true; from a space lacking the slot, the same read faults: true; the probe page is not global: true".into(),
        format!(
            "origin-kernel: address spaces: a page unmapped under a running thread on {far}: frame free when the unmap returned: true; reused with a secret: true; the thread faulted at 0x500000 (#PF error 0x4): true; it read the secret: false; {cost}"
        ),
        "origin-kernel: address spaces: control: a thread whose page is still mapped reads the same secret: true".into(),
        format!(
            "origin-kernel: address spaces: a page made read-only under a running writer on {far}: the writer faulted at 0x500000 (#PF error 0x7): true; {cost}"
        ),
        format!(
            "origin-kernel: address spaces: a page made writable under a running reader on {far}: it wrote without a fault: true; {cost}"
        ),
        "origin-kernel: address spaces: a page mapped under a running thread: it read 0x1234 and exited".into(),
        format!(
            "origin-kernel: address spaces: 5 pages unmapped at once under a running thread on {far}: it faulted at its page among them: true; Ok(Change {{ pages: 5, shootdowns: 1, interrupts: {one} }})"
        ),
        format!(
            "origin-kernel: address spaces: 200 pages unmapped at once under a running thread on {far}: it faulted at its page among them: true; Ok(Change {{ pages: 200, shootdowns: 2, interrupts: {} }})",
            2 * one
        ),
        "origin-kernel: address spaces: a frame shared by two spaces: unmapped from the first, its thread faulted: true, the other went on: true, the frame still in use: true; unmapped from the second, its thread faulted: true, the frame free when the unmap returned: true".into(),
        "origin-kernel: address spaces: W^X across a shared frame's mappings: executable in another space while writable in one, refused: true; at another address in the same space, refused: true; read-only beside it: true; executable once nothing writes it: true; writable again while executable elsewhere, refused: true; from executable to writable in one step, refused: true".into(),
        "origin-kernel: address spaces: a run of shared frames is mapped whole or not at all: over a page already mapped, refused with none of it mapped: true; with a frame executable in another space, refused with none of it mapped and no count taken: true; into 2 MiB that needs a page table once the space has made all it may, refused as a limit with none of it mapped: true; mapped whole: true".into(),
        format!(
            "origin-kernel: address spaces: 64 maps and unmaps of a page under a thread on {far}: {} interrupts, 0 on processors not in the space; ",
            64 * one
        ),
        " us each (QEMU, no budget)".into(),
        "origin-kernel: address spaces: every process and frame of these proofs is gone: ".into(),
    ]
}

/// ADR-0004 Amendment 1, A2: the kernel stacks' region, each of its boot proof's lines.
fn stack_markers(machine: &Machine) -> Vec<String> {
    let cpus = machine.cpus();
    let interrupt_stacks = 4 * (cpus - 1);
    // The processor that caches the freed stack: the first after the boot processor, if any.
    let reader = machine.named(cpus.min(2) - 1);
    vec![
        // ADR-0004 Amendment 2: every machine here has RDSEED or RDRAND, so the loader hands
        // over a random value and the region's base is drawn from it (A5.1). The line names
        // neither the base nor the value (A6). The machine with neither instruction has its
        // own list (`no_generator_markers`).
        "origin-kernel: kernel stacks: the region's base is 2 MiB-aligned with the whole region in PML4 slots 384-447: true; drawn from the loader's random value, taken once: true, so not its fixed base: true".into(),
        format!(
            "origin-kernel: kernel stacks: stacks of 4, 16 and 64 KiB each in a slot of its own in the region, every page read/write, not executable and global: true; the page below each, the bottom of its slot and the bottom of the next faulted: 9 of 9; the application processors' interrupt stacks, as their TSSs name them, in the region with the page below each faulting: {interrupt_stacks} of {interrupt_stacks}"
        ),
        "origin-kernel: kernel stacks: the direct map has every 2 MiB page it was built with: true (".into(),
        format!(
            "origin-kernel: kernel stacks: a stack's 4 pages read on {reader}, then the stack freed and reclaimed: 4 of 4 reads there faulted and 0 saw the old pages; its slot, the one its last byte was in: true, free again: true"
        ),
        "origin-kernel: kernel stacks: 64 stacks of 16384 bytes made and freed: all made true; making one took ".into(),
        " (QEMU, no budget)".into(),
        "origin-kernel: kernel stacks: every stack the proof made is given back: Counts { live: ".into(),
        ", idle: 0, retired: 0, lost: 0, pages: ".into(),
        " }, as before: true".into(),
    ]
}

fn ipc_markers(boot: String, far: String) -> Vec<String> {
    let mut markers = vec![
        format!(
            "origin-kernel: ipc: ping-pong: two ring-3 processes on {boot} and {far} exchanged 1000 messages each way, every one checked in ring 3"
        ),
        "origin-kernel: ipc: ping-pong round trip: ".into(),
    ];
    markers.extend([
        "origin-kernel: ipc: bad handles: handle 0 -> BAD_HANDLE, a live handle -> OK, its low 32 bits alone -> BAD_HANDLE, its slot at a generation never issued -> BAD_HANDLE, never issued -> BAD_HANDLE, bits above 32 -> BAD_HANDLE, wait on a bad handle -> BAD_HANDLE, closed -> BAD_HANDLE, stale, slot reused -> BAD_HANDLE, another process's -> BAD_HANDLE, wait on another process's -> BAD_HANDLE".to_string(),
        "origin-kernel: ipc: too few rights: write without WRITE -> ACCESS_DENIED, read without READ -> ACCESS_DENIED, wait without WAIT -> ACCESS_DENIED, duplicate without DUPLICATE -> ACCESS_DENIED, send without TRANSFER -> ACCESS_DENIED".into(),
        "origin-kernel: ipc: rights only shrink: adding WRITE -> INVALID_ARGS, an undefined right -> INVALID_ARGS, dropping DUPLICATE -> OK, the copy cannot write -> ACCESS_DENIED, the copy can read -> SHOULD_WAIT".into(),
        "origin-kernel: ipc: bad pointers: write from the kernel -> FAULT, from unmapped -> FAULT, across the end of mapped -> FAULT, from address 0 -> FAULT, wrapping past the top -> FAULT, from the top page -> FAULT, read into the kernel -> FAULT, into unmapped -> FAULT, into code -> FAULT, into read-only memory -> FAULT, sizes in the kernel -> FAULT, a handle record across the end of mapped -> FAULT, create out to the kernel -> FAULT, create out to read-only -> FAULT, duplicate out to the kernel -> FAULT, duplicate out to read-only -> FAULT, wait out to read-only -> FAULT, wait out to the kernel -> FAULT, handle list in the kernel -> FAULT, zero length names no memory -> OK, the empty message arrived -> 0x0".into(),
        "origin-kernel: ipc: sizes and numbers: one byte too many -> INVALID_ARGS, one handle too many -> INVALID_ARGS, no signals -> INVALID_ARGS, an undefined signal -> INVALID_ARGS, a sizes structure too small -> INVALID_ARGS, an unknown call number -> NOT_SUPPORTED, an event with no quota for one -> LIMIT".into(),
        "origin-kernel: ipc: queues: empty -> SHOULD_WAIT, nothing by deadline 0 -> TIMED_OUT, written -> OK, readable -> OK, observed -> 0x3, 4-byte buffer -> BUFFER_TOO_SMALL, it needs -> 0x10, still there -> OK, received -> 0x10".into(),
        "origin-kernel: ipc: a full queue: the write after the last -> SHOULD_WAIT, not writable -> TIMED_OUT, writable again after a read -> OK".into(),
        "origin-kernel: ipc: a closed peer: sees PEER_CLOSED -> OK, observed -> 0x5, write -> PEER_CLOSED, the message sent before the close -> OK, read past the close -> PEER_CLOSED, READABLE will never rise -> TIMED_OUT".into(),
        "origin-kernel: ipc: handle transfer: send -> OK, the sender's handle is gone -> BAD_HANDLE, received -> OK, with one handle -> 0x1, its record names a channel -> 0x4, with the rights it was sent with -> 0x73, the moved handle writes -> OK, and the other end reads -> OK, it kept its rights -> OK".into(),
        "origin-kernel: ipc: transfers that would break the graph: an end through its own channel -> INVALID_ARGS, the peer end through its own channel -> INVALID_ARGS, the same handle twice -> INVALID_ARGS, an end with another handle -> INVALID_ARGS, that end, alone again -> OK, into the first channel -> OK, a cycle through two queues -> INVALID_ARGS, a stale handle in the list -> BAD_HANDLE".into(),
        "origin-kernel: ipc: every hostile call returned its status and the process exited by itself (".into(),
        "; user-copy faults recovered: 0".into(),
        // N14 step 3, S3-5b: the memory calls' corpus, in a process of its own.
        "origin-kernel: ipc: memory objects: a request in the kernel -> FAULT, a request of the wrong size -> INVALID_ARGS, no bytes -> INVALID_ARGS, not whole pages -> INVALID_ARGS, past the quota -> LIMIT, its handle into the kernel -> FAULT, one page -> OK, wait on it -> ACCESS_DENIED, give it EXECUTE -> INVALID_ARGS".into(),
        "origin-kernel: ipc: memory maps: a bad process handle -> BAD_HANDLE, a channel for the process -> WRONG_TYPE, a process handle without WRITE -> ACCESS_DENIED, a channel for the object -> WRONG_TYPE, an object handle without MAP -> ACCESS_DENIED, a request in the kernel -> FAULT, the address into the kernel -> FAULT, a request of the wrong size -> INVALID_ARGS, an undefined access -> INVALID_ARGS, an undefined flag -> INVALID_ARGS, write without read -> INVALID_ARGS, write and execute -> INVALID_ARGS, execute -> ACCESS_DENIED, write without WRITE -> ACCESS_DENIED, a misaligned offset -> INVALID_ARGS, no length -> INVALID_ARGS, past the object -> INVALID_ARGS, in the kernel half -> INVALID_ARGS, at address 0 -> INVALID_ARGS, at the top page -> INVALID_ARGS, an address without FIXED -> INVALID_ARGS, mapped -> OK, over it -> INVALID_ARGS".into(),
        "origin-kernel: ipc: memory unmap and protect: unmap without WRITE -> ACCESS_DENIED, unmap where nothing is -> INVALID_ARGS, unmap none of it -> INVALID_ARGS, unmap more than it -> INVALID_ARGS, unmap not whole pages -> INVALID_ARGS, protect to nothing -> INVALID_ARGS, protect with an undefined bit -> INVALID_ARGS, protect to execute -> ACCESS_DENIED, protect where nothing is -> INVALID_ARGS, protect to read-only -> OK, protect back to writable -> ACCESS_DENIED, unmap -> OK, unmap again -> INVALID_ARGS".into(),
        "origin-kernel: ipc: every hostile memory call returned its status and the process exited by itself (49 calls)".into(),
    ]);
    for how in ["exits", "faults"] {
        markers.extend([
            "origin-kernel: ipc: the peer of a process that: woken -> OK, observed -> 0x4, write -> PEER_CLOSED, read -> PEER_CLOSED".to_string(),
            format!("origin-kernel: ipc: when a process {how}: its handles close (0 left), and its peer on {far}, blocked "),
            "woke and saw PEER_CLOSED".into(),
        ]);
    }
    markers.extend([
        "origin-kernel: ipc: a thread in wait_one on a handle another thread closes completes with CANCELED".to_string(),
        "origin-kernel: ipc: quotas: a fifth channel -> LIMIT, a thirteenth handle -> LIMIT, one more after a close -> OK, still working -> OK".into(),
        "origin-kernel: ipc: a writer over its byte quota: the first -> OK, the second -> OK, the third is over the quota -> LIMIT, the peer answers -> OK, the answer -> OK, written after the peer read -> OK".into(),
        "origin-kernel: ipc: its peer, unaffected: the first -> OK, the second -> OK, its answer -> OK".into(),
        "origin-kernel: ipc: queued messages are charged to their sender and refunded on read and close: 0 and 0 bytes still charged".into(),
        "origin-kernel: ipc: transfers into nested ends: nested to the limit -> OK, a link deeper -> LIMIT".into(),
        "origin-kernel: ipc: a full-size message: sixty-four KiB -> OK, received -> OK, bytes -> 0x10000".into(),
        "origin-kernel: ipc: 64 KiB through a channel and back, byte for byte: true".into(),
        "origin-kernel: ipc: registers: every register but RAX, RCX and R11 survives a system call: true".into(),
        "origin-kernel: ipc: a process's system calls are accepted only from its pinned code range: every call refused, and it ended by its own fault".into(),
        "origin-kernel: ipc: a program that loaded a null GS was interrupted and made system calls, and the kernel used its own GS (reports [1, 0])".into(),
        "origin-kernel: ipc: a program with RFLAGS.AC set took interrupts and made a system call: the flag reached kernel code 0 times, and stayed set in ring 3: true".into(),
        "origin-kernel: ipc: a process without the developer calls gets NOT_SUPPORTED: true".into(),
        "origin-kernel: ipc: sidt from ring 3 with UMIP on: refused, the process ended by vector 13".into(),
        "origin-kernel: ipc: a program that single-stepped itself ended by #DB, taken on its own interrupt stack: true".into(),
        "origin-kernel: ipc: a fault inside a user copy comes back as a failed copy, not a dead kernel: 1 fault recovered".into(),
        "origin-kernel: ipc: every process's memory, handles, channels and queued messages are gone: heap ".into(),
        "; threads run before their state existed: 0".into(),
        "origin-kernel: ipc: a status its call does not declare reached the system-call boundary 0 times".into(),
    ]);
    markers
}

/// The proofs of process and thread objects (ADR-0005 P3, P9, P11; `ipc_proof::prove_objects`).
fn object_markers(far: &str) -> Vec<String> {
    let mut markers = vec![
        "origin-kernel: ipc: a process's memory goes when its last thread is reaped, with the process object still held: 0 spaces freed while a thread ran, 1 when TERMINATED was raised".to_string(),
        "origin-kernel: ipc: object types: a process is no channel -> WRONG_TYPE, a thread is no channel -> WRONG_TYPE, the type comes before the rights -> WRONG_TYPE, a live process -> TIMED_OUT, raises nothing -> 0x0, a live thread -> TIMED_OUT, raises nothing either -> 0x0, waiting needs WAIT -> ACCESS_DENIED, a duplicate waits -> TIMED_OUT, and cannot duplicate -> ACCESS_DENIED, a process handle travels -> OK, the sender's is gone -> BAD_HANDLE, the received one waits -> TIMED_OUT, a thread handle closes -> OK".into(),
    ];
    for how in ["exits", "faults"] {
        markers.extend([
            "origin-kernel: ipc: the waiter on a process that: woken -> OK, observed -> 0x10, and it stays raised -> OK".to_string(),
            format!("origin-kernel: ipc: when a process {how}: TERMINATED reached the thread waiting on it on {far}, blocked "),
        ]);
    }
    markers.extend([
        "origin-kernel: ipc: a thread waiting for another thread: a thread still running -> TIMED_OUT, joined -> OK, observed -> 0x10, and it stays raised -> OK".to_string(),
        "origin-kernel: ipc: a thread in wait_one on a process handle another thread closes completes with CANCELED".into(),
        "origin-kernel: ipc: a thread in wait_one on a process handle another thread sends away completes with CANCELED".into(),
        "origin-kernel: ipc: process IDs: 16 processes created and dropped in turn, each ID above the last and none of them 0: true".into(),
        "origin-kernel: ipc: every process and thread object and all they held are gone: heap ".into(),
    ]);
    markers
}

/// The proof of a message's writer (ADR-0005 B1, C1, C2; `ipc_proof::prove_identity`).
fn identity_markers() -> Vec<String> {
    vec![
        "origin-kernel: ipc: a message's writer: the message written before its end moved to another process names its writer, process and principal: true; the one written after names the new writer: true; a read too small for it names the writer too: true; none names the reader: true".into(),
        "origin-kernel: ipc: a reader that gives read_sizes' first size, 12: its read -> OK, its counts -> 7 bytes, and nothing written past them: true".into(),
        "origin-kernel: ipc: the identity proof's processes and all they held are gone: heap ".into(),
    ]
}

/// The process start proof (ADR-0005 P6, C1, C4-C6; `ipc_proof::prove_launch`).
fn launch_markers() -> Vec<String> {
    vec![
        "origin-kernel: start: two children launched from their creator found their start records in RDI, read-only at random addresses: each record, read back in ring 3, has its size, version, 4 entries with C6's rights, 2 arguments and 1 environment string: true; each start handle works: true; each child's message names it and its creator's principal: true; writing its record faulted there: true".into(),
        "origin-kernel: start: the two records are at different addresses: true; a record over 64 KiB is refused: true; a launch refused after its child held a handle gives everything back: true".into(),
        "origin-kernel: start: every launched process and all they held are gone: heap ".into(),
    ]
}

/// The first Rust program in ring 3 (task S4-1; ADR-0011 L1, L8; `program_proof::prove`): the
/// kernel carries it in its image, and launches it six times at random bases.
fn rust_program_markers() -> Vec<String> {
    vec![
        "origin-kernel: rust program: first_program ran in ring 3 from the kernel's image at random bases, 6 times: each found its start record in RDI and read its header and 3 entries as written: true; ran at its base plus its entry: true; every relocated slot held its value plus the base: true; read its data through relocated pointers: true; the runtime's mem functions copy, move, fill and compare: true; wrote on BOOTSTRAP, named as the writer: true; ended with its exit code: true".into(),
        "origin-kernel: rust program: two launches differ in base and stack: true; a write to its code faulted there: true; a jump into its data faulted there as a fetch: true; a write to its relocated read-only data faulted there: true; a panic ended it with the runtime's exit code: true; a launch refused after its image was mapped: true".into(),
        "origin-kernel: rust program: loading and starting it took ".into(),
        "origin-kernel: rust program: every launched process and all they held are gone: heap ".into(),
    ]
}

/// The service directory proof (ADR-0005 C11, C12; ADR-0011 N9; `ipc_proof::prove_directory`).
fn directory_markers() -> Vec<String> {
    vec![
        "origin-kernel: directory: a launcher started a directory, a service and a client; the client's connect, and the request it wrote on its own end before any reply, reached the service through the directory: true; the service read that request stamped with the client's process and principal, not the directory's: true; the client read the service's reply stamped with the service's process: true".into(),
        "origin-kernel: directory: the client's replies, read in ring 3 and decoded by the generated client: ok for the interface and major the directory holds: true; not_found (-14) for a major it does not hold: true; that connection's end reports peer_closed: true; the service answered ok for the end passed to it: true".into(),
        "origin-kernel: directory: the directory, the service, the client and all they held are gone: heap ".into(),
    ]
}

/// The events-and-ports proof and the clock as ring 3 reads it (ADR-0005 C9, C10, C13,
/// Amendment 7; `ipc_proof::prove_events_ports`, `ipc_proof::prove_user_clock`).
fn events_ports_markers() -> Vec<String> {
    vec![
        "origin-kernel: events: an event starts with nothing raised; object_signal raises and clears SIGNALED, a signal in both masks ends set, and wait_one sees each state: true; a mask with another signal, a handle without signal and a bad pointer are refused: true".into(),
        "origin-kernel: ports: a binding on an event and on a channel end becomes one packet when its signal is raised, at once if it already is, twice if made twice, in the order queued: true; each packet is what was bound and seen: true".into(),
        "origin-kernel: ports: port_cancel removes an armed binding and a queued packet silently, then not_found: true; closing the handle a binding was made through queues one canceled packet, another duplicate's close cancels nothing, a move cancels, and a queued signal packet stays with nothing after it: true".into(),
        "origin-kernel: ports: hostile binds, waits and cancels are refused in D5's order, and a port cannot be waited on: true; events, ports and bindings stop at their quotas, and closing a port gives back its bindings' charges: true; every other call returned what it must, and the three processes ended: true".into(),
        "origin-kernel: ports: a process given no quota can make no event and no port: true".into(),
        "origin-kernel: ports: a thread in port_wait through a handle another thread closes ends canceled, while a duplicate keeps the port open and empty: true".into(),
        "origin-kernel: ports: a binder that exits while another process holds the port leaves its 2 bindings as canceled packets, still charged to it (2 after its exit, 0 once the holder read them): true".into(),
        "origin-kernel: ports: 30 round trips between ".into(),
        " us each (QEMU, through the script interpreter, no budget): true".into(),
        "origin-kernel: ports: a bind, a signal that delivers it, a take and a clear took ".into(),
        " ns together (QEMU, the kernel's own calls, 1000 times, no budget): true".into(),
        "origin-kernel: ports: every process, event, port, binding and packet of the proof is gone: heap ".into(),
        "origin-kernel: clock: 30 reads from ring 3 never went back: true; each lies between the kernel's readings before and after: true; a wait to a deadline on the same clock ended after it: true; a bad out pointer is fault: true; one read every ".into(),
        " ns (QEMU, through the script interpreter)".into(),
    ]
}

/// The memory-object proof (ADR-0005 P7, P11, C6-C8; `ipc_proof::prove_memory`).
fn memory_markers() -> Vec<String> {
    vec![
        "origin-kernel: memory: two processes share an object by passing its handle, and each reads what the other wrote: true; protect to read-only refuses an increase and makes a write fault at the mapping: true; unmap makes a read fault there: true".into(),
        "origin-kernel: memory: a fixed address over a mapping is refused, and the mapping keeps what was written to it: true; execute is refused: true; a handle without write maps nothing writable, and one without read maps nothing: true; two random placements are apart, page-aligned and in their region: true; closing the handle leaves the mapping working: true".into(),
        "origin-kernel: memory: the quota refuses an object past its bound and takes one again once an object is closed; a closed object still mapped keeps its charge; a third mapping past the limit of two is refused: true; once the process has ended its account owes nothing: true".into(),
        "origin-kernel: memory: mapping into another process needs write on its handle, and with it the two share the frame: true; unmapping there needs it too, and the other's next read faults: true".into(),
        "origin-kernel: memory: a hostile unmap loop is refused with should_wait once its budget is spent, changing nothing: true; it is served again once the budget refills: true".into(),
        "origin-kernel: memory: a map whose address cannot be written to its caller leaves no page and no record, though the caller's budget pays for nothing: true; what taking it back cost is charged, and the caller's next unmap must wait: true".into(),
        "origin-kernel: memory: a fixed run over a page of the program's own is refused whole, and the page stays: true; a mapping of an object's later pages maps those pages' frames: true; a run past the space's page tables is refused with limit and leaves no page and no record: true".into(),
        "origin-kernel: memory: an unmap and a protect of a mapping whose map has not returned yet are told to wait (should_wait): true; so are those of a mapping another change holds: true; the refusals changed nothing and spent nothing: true".into(),
        "origin-kernel: memory: in a space at its bound of page tables, a fixed run that needs one more table and lies over a mapping's page is invalid_args: true; so is one over a page of the program's own: true; a protect that asks for more than a mapping allows is told to wait while another call holds the mapping, and is denied once it is free: true".into(),
        "origin-kernel: memory: every other call of the proof's programs returned what its script says, and each program ended as it should: true".into(),
        "origin-kernel: memory: map, protect and unmap of 16 pages took ".into(),
        " us (QEMU; the kernel's own calls, in a space no processor holds)".into(),
        "origin-kernel: memory: unmapping a page of a shared object from a space a thread is reading it in took ".into(),
        " us (QEMU, no budget); the thread then faulted at the page: true; the object stays mapped in its creator's space: true".into(),
        "origin-kernel: memory: every process, object and mapping of the proof is gone: heap ".into(),
    ]
}

/// The tampered path: one byte of a SEGMENT changed on disk. The loader must refuse it, and
/// the kernel must never run. Unsigned, under the developer-mode loader: developer mode
/// still verifies what it can.
fn tamper_scenario() -> Scenario {
    Scenario {
        label: "tampered kernel",
        loader: Loader::Developer,
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
        check: None,
        stderr: vec![],
        capture: Capture::Com1,
        qemu_args: None,
    }
}

type Result<T = ()> = std::result::Result<T, String>;

/// Lines each CPU prints in the kernel's console proof, and the letters after each line's
/// prefix (`framekernel/src/console.rs`: `PROOF_LINES`, `PROOF_PAYLOAD`).
const CONSOLE_LINES: usize = 20;
const CONSOLE_PAYLOAD: usize = 60;

/// The kernel's console serializes whole lines across processors. In its proof every CPU
/// prints [`CONSOLE_LINES`] lines at once, each `origin-kernel: console: CPU {id} line {k}
/// of 20: ` followed by [`CONSOLE_PAYLOAD`] copies of the letter `a + id % 26`. Every one
/// must arrive whole and in order; a line that mixed two processors' bytes fails. No line
/// of the whole log may hold two `origin-kernel:` prefixes either.
fn console_lines_intact(text: &str, cpus: usize) -> Result {
    if let Some(line) = text
        .lines()
        .find(|l| l.matches("origin-kernel:").count() > 1)
    {
        return Err(format!("two kernel lines ran together: {line:?}"));
    }
    let mut seen = std::collections::BTreeMap::<u64, Vec<usize>>::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("origin-kernel: console: CPU ") else {
            continue;
        };
        let (id, k) =
            parse_console_line(rest).ok_or_else(|| format!("garbled console line: {line:?}"))?;
        seen.entry(id).or_default().push(k);
    }
    let complete: Vec<usize> = (0..CONSOLE_LINES).collect();
    if seen.len() != cpus || seen.values().any(|ks| *ks != complete) {
        return Err(format!(
            "console proof: expected lines 0..{CONSOLE_LINES} from each of {cpus} CPUs, got {seen:?}"
        ));
    }
    Ok(())
}

/// How many dots follow the prefix of each of the kernel's long proof lines
/// (`framekernel/src/console.rs`: `PROOF_FILL`).
const LONG_LINE_FILL: usize = 60;
/// The long lines the console's proofs print, by their prefixes. Interrupts and reports come
/// in the middle of them, and each is many sections long on both sinks.
const LONG_LINES: [&str; 7] = [
    "origin-kernel: console: a line drawn by a thread: ",
    "origin-kernel: console: a line that had to wait for room: ",
    "origin-kernel: console: a line sent on the UART a section at a time: ",
    "origin-kernel: console: a line sent in sections while an interrupt waits: ",
    "origin-kernel: console: a line sent in sections with a breakpoint in the middle: ",
    "origin-kernel: console: a line drawn in sections while an interrupt waits: ",
    "origin-kernel: console: a line drawn in sections with a breakpoint in the middle: ",
];

/// Every long line must have reached the UART once and whole: the dots, all of them and
/// nothing else, where a load that overran the UART's FIFO would lose some, and an interrupt's
/// or a report's line sent in the middle would show. (Such a line also has two `origin-kernel:`
/// prefixes on it, which [`console_lines_intact`] refuses.)
fn long_lines_whole(text: &str) -> Result {
    for prefix in LONG_LINES {
        let lines: Vec<&str> = text.lines().filter(|l| l.contains(prefix)).collect();
        let [line] = lines.as_slice() else {
            return Err(format!(
                "long proof line {prefix:?} appears {} times",
                lines.len()
            ));
        };
        let dots = line.strip_prefix(prefix);
        if dots.is_none_or(|d| d.len() != LONG_LINE_FILL || d.bytes().any(|b| b != b'.')) {
            return Err(format!("a long proof line is not whole: {line:?}"));
        }
    }
    Ok(())
}

/// The lines of a writer that cannot wait (`console::prove_room`), `0..n`, each with its dots:
/// the queue must lose none of them, send none twice and keep their order.
fn unwaited_lines_whole(text: &str) -> Result {
    const PREFIX: &str = "origin-kernel: console: a line that cannot wait: ";
    let mut next = 0;
    for line in text.lines().filter(|l| l.contains(PREFIX)) {
        let whole = line.strip_prefix(PREFIX).and_then(|rest| {
            let (k, dots) = rest.split_once(": ")?;
            (dots.len() == LONG_LINE_FILL && dots.bytes().all(|b| b == b'.'))
                .then(|| k.parse::<usize>().ok())?
        });
        if whole != Some(next) {
            return Err(format!("line {next} that could not wait: got {line:?}"));
        }
        next += 1;
    }
    if next == 0 {
        return Err("no lines from a writer that could not wait".into());
    }
    Ok(())
}

/// Parses `{id} line {k} of 20: {letters}`.
fn parse_console_line(rest: &str) -> Option<(u64, usize)> {
    let (id, rest) = rest.split_once(" line ")?;
    let (k, letters) = rest.split_once(&format!(" of {CONSOLE_LINES}: "))?;
    let id: u64 = id.parse().ok()?;
    let k: usize = k.parse().ok()?;
    let letter = char::from(b'a' + (id % 26) as u8);
    (letters.len() == CONSOLE_PAYLOAD && letters.chars().all(|c| c == letter)).then_some((id, k))
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["check"] => in_turn("check", check),
        ["unsafe-gate"] => unsafety::gate(),
        ["document-gate"] => documents::gate(),
        ["cfg-lint"] => linux_cfg_lint(),
        ["interface-gate"] => interfaces::gate(),
        ["requirements-gate"] => requirements::gate(),
        ["regenerate"] => interfaces::regenerate(),
        ["image-gate"] => in_turn("image-gate", image::build_and_gate),
        ["image-gate", elf] => image::gate(Path::new(elf)),
        ["build"] => in_turn("build", || {
            build_loader(Loader::Developer).and_then(|loader| {
                let kernel = pack_kernel(&build_kernel()?)?;
                println!("{}\n{}", loader.display(), kernel.display());
                Ok(())
            })
        }),
        ["boot-test"] => in_turn("boot-test", boot_test),
        ["boot-matrix"] => in_turn("boot-matrix", boot_matrix),
        ["boot-x2apic"] => in_turn("boot-x2apic", boot_x2apic),
        ["boot-console"] => in_turn("boot-console", boot_console),
        ["boot-bridge"] => in_turn("boot-bridge", boot_bridge),
        ["boot-legacy-free"] => in_turn("boot-legacy-free", boot_legacy_free),
        ["boot-random"] => in_turn("boot-random", boot_random),
        ["run"] => in_turn("run", run),
        ["loom"] => in_turn("loom", loom::run),
        ["reproducible-gate"] => in_turn("reproducible-gate", reproducible::gate),
        ["exclusive", "--", command @ ..] => {
            let command: Vec<String> = command.iter().map(|s| (*s).to_string()).collect();
            return match turns::exclusive(&command) {
                Ok(code) => ExitCode::from(code),
                Err(e) => {
                    eprintln!("xtask: error: {e}");
                    ExitCode::FAILURE
                }
            };
        }
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

/// Runs `run` in this process's turn on the machine: heavy runs go one at a time across every
/// checkout of the repository (`turns`).
fn in_turn(command: &str, run: impl FnOnce() -> Result) -> Result {
    let _turn = turns::take(command)?;
    run()
}

/// `xtask boot-test`, as a child process inside this run's turn on the machine.
fn boot_test_child(exe: &Path) -> Command {
    let mut command = Command::new(exe);
    command.arg("boot-test");
    turns::pass(&mut command);
    command
}

/// Runs `boot-test` in a child process for each IOMMU and processor count, so each gets its
/// own `ORIGIN_QEMU_IOMMU` and `ORIGIN_QEMU_SMP` (the environment of this process is never
/// changed), and then [`boot_x2apic`] (the owner's decision on ADR-0008, question 3).
fn boot_matrix() -> Result {
    let exe = env::current_exe().map_err(|e| format!("cannot find this xtask: {e}"))?;
    let bases = stack_bases_file();
    if let Some(dir) = bases.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::write(&bases, "").map_err(|e| format!("{}: {e}", bases.display()))?;
    for iommu in ["intel", "amd"] {
        for cpus in [1, 2, 4] {
            println!("xtask: boot-matrix: {iommu} IOMMU, {cpus} processor(s)");
            let status = boot_test_child(&exe)
                .env("ORIGIN_QEMU_IOMMU", iommu)
                .env("ORIGIN_QEMU_SMP", cpus.to_string())
                .status()
                .map_err(|e| format!("cannot run boot-test: {e}"))?;
            if !status.success() {
                return Err(format!(
                    "boot-test with the {iommu} IOMMU and {cpus} processor(s) failed ({status})"
                ));
            }
            check_stack_bases_so_far(&bases)?;
        }
    }
    boot_x2apic()?;
    boot_bridge()?;
    boot_legacy_free()?;
    boot_random()?;
    boot_console()?;
    // ADR-0004 Amendment 2, A6.4: the boots placed the kernel stacks' region apart.
    let recorded = fs::read_to_string(&bases).map_err(|e| format!("{}: {e}", bases.display()))?;
    let boots = distinct_stack_bases(&recorded)?;
    println!(
        "xtask: boot-matrix: the kernel stacks' region was at {boots} distinct bases in {boots} kernel boots, read through QEMU's monitor; none is printed"
    );
    println!(
        "xtask: boot-matrix passed (Intel VT-d and AMD-Vi, each with 1, 2 and 4 processors, the sparse machine, the bridge machine, the legacy-free machine, the two machines without the random-number instructions, and the console machines)"
    );
    Ok(())
}

/// The I/O port of the UART the SPCR proof's table names: COM2's, where there is no COM1.
const SPCR_PORT: u16 = 0x2F8;

/// A checksummed SPCR table, revision 2: `interface` type, the Generic Address Structure
/// (`[space, bit width, bit offset, access size, address...]`) at the UART, `baud` code 7 for
/// 115 200 and 1 stop bit (the only value the table defines). Built here, independently of
/// `framekernel::spcr`, so the boot proof checks the parser against the specification.
fn spcr_table(interface: u8, base_address: [u8; 12], baud: u8) -> Vec<u8> {
    let mut table = vec![0u8; 80];
    table[..4].copy_from_slice(b"SPCR");
    table[4..8].copy_from_slice(&80u32.to_le_bytes());
    table[8] = 2;
    table[10..16].copy_from_slice(b"ORIGIN");
    table[16..24].copy_from_slice(b"ORIGSPCR");
    table[24..28].copy_from_slice(&1u32.to_le_bytes());
    table[28..32].copy_from_slice(b"ORIG");
    table[32..36].copy_from_slice(&1u32.to_le_bytes());
    table[36] = interface;
    table[40..52].copy_from_slice(&base_address);
    table[58] = baud;
    table[60] = 1;
    table[64..68].copy_from_slice(&[0xFF; 4]);
    let sum = table.iter().fold(0u8, |s, &b| s.wrapping_add(b));
    table[9] = 0u8.wrapping_sub(sum);
    table
}

/// A Generic Address Structure in I/O space, as the SPCR's examples write COM1 and COM2.
fn gas_io(port: u16) -> [u8; 12] {
    let mut gas = [0u8; 12];
    gas[0] = 1;
    gas[1] = 8;
    gas[4..6].copy_from_slice(&port.to_le_bytes());
    gas
}

/// Writes `table` under the work directory and returns the QEMU arguments that add it.
fn spcr_args(work: &Path, table: &[u8], extra: &[&str]) -> Result<Vec<String>> {
    let path = work.join("spcr.bin");
    fs::write(&path, table).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut args = vec!["-acpitable".to_string(), qemu_path_arg("file=", &path)?];
    args.extend(extra.iter().map(|a| (*a).to_string()));
    Ok(args)
}

/// Text the scenarios in [`boot_console`] must see from the kernel at the end of a boot.
fn console_markers(specific: &[&str]) -> Vec<String> {
    let mut markers: Vec<String> = specific.iter().map(|m| (*m).to_string()).collect();
    markers.push(HALT_LINE.to_string());
    markers
}

/// A scenario for [`boot_console`]: the signed boot and every kernel failure message; the
/// markers come from the machine and what's special about it.
fn console_scenario(
    label: &'static str,
    capture: Capture,
    markers: Vec<String>,
    qemu_args: Option<QemuArgs>,
    check: Option<fn(&Completed) -> Result>,
) -> Scenario {
    Scenario {
        label,
        loader: Loader::Verifying,
        markers,
        failures: boot_failures(),
        check,
        stderr: vec![],
        capture,
        qemu_args,
    }
}

/// The UART the SPCR names: everything printed before the move is sent to it, from the first
/// line of the boot, and what follows the move reaches it and only it. COM1's log is checked
/// to stop at the line announcing the move.
fn spcr_uart_scenario() -> Scenario {
    use origin_version::{BUILD, PRODUCT, VERSION};
    console_scenario(
        "console: UART named by the SPCR",
        Capture::SpcrUart,
        console_markers(&[
            // Everything before the move, replayed: the first kernel lines of the boot.
            "origin-kernel: console: framebuffer ",
            &format!("origin-kernel: {PRODUCT} {VERSION} Framekernel (build {BUILD})"),
            "origin-kernel: GDT, TSS and IDT loaded (32 exception vectors)",
            "origin-kernel: page tables loaded: ",
            MOVED_LINE,
            "origin-kernel: console: the UART is now at I/O port 0x2f8, as the firmware set it; ",
            // After the move: only this UART has these.
            "origin-kernel: allocated, wrote and freed 3 frames at ",
            "origin-kernel: TLB: ",
            "origin-kernel: GPT: partition 2",
            "origin-kernel: sysfs: built /sys with 4 facts",
        ]),
        Some(|work| spcr_args(work, &spcr_table(0, gas_io(SPCR_PORT), 7), &[])),
        Some(check_com1_stopped_at_the_move),
    )
}

/// The line announcing the move, the last one COM1 may ever carry.
const MOVED_LINE: &str = "origin-kernel: console: the ACPI SPCR (revision 2, interface 0x0) names a UART at I/O port 0x2f8: moving the console there";

/// Once the firmware has named a better UART, the fallback is not used: COM1 holds the move
/// line as its last kernel line, and has never heard of anything after it.
fn check_com1_stopped_at_the_move(done: &Completed) -> Result {
    let com1 = done.com1.as_deref().ok_or("no COM1 log")?;
    let text = serial::clean(com1.as_bytes());
    let kernel_lines: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("origin-kernel:"))
        .collect();
    if kernel_lines.last().copied() != Some(MOVED_LINE) {
        return Err(format!(
            "COM1's last kernel line is {:?}; it should be the line about the move",
            kernel_lines.last()
        ));
    }
    if kernel_lines.len() < 10 {
        return Err(format!(
            "COM1 carried only {} kernel lines before the move",
            kernel_lines.len()
        ));
    }
    Ok(())
}

/// Every console scenario for `boot-console`: where the UART is not at 0x3F8, and what the
/// kernel does without one or without a display (ADR-0010: a missing or unusable console
/// never stops the boot).
fn console_scenarios() -> Vec<Scenario> {
    vec![
        spcr_uart_scenario(),
        console_scenario(
            "console: SPCR naming COM1 itself",
            Capture::Com1,
            console_markers(&[
                "origin-kernel: console: the ACPI SPCR (revision 2, interface 0x0) names a UART at I/O port 0x3f8: moving the console there",
                "origin-kernel: console: the UART is now at I/O port 0x3f8, as the firmware set it; 0 bytes of earlier output sent to it again, 0 did not fit",
                "origin-kernel: allocated, wrote and freed 3 frames at ",
            ]),
            Some(|work| spcr_args(work, &spcr_table(0, gas_io(0x3F8), 7), &[])),
            None,
        ),
        // A device that answers at the UART's status register but never reports room to
        // transmit: the debug console's read value, which QEMU makes readable, stands in
        // for a clock-gated UART (this is the status port of the block the table names).
        console_scenario(
            "console: SPCR naming a UART that takes no bytes",
            Capture::Com1,
            console_markers(&[
                "origin-kernel: console: the ACPI SPCR (revision 2, interface 0x0) names a UART at I/O port 0x2e8: moving the console there",
                "origin-kernel: console: the UART at I/O port 0x2e8 takes no bytes; the UART stays at I/O port 0x3f8",
                "origin-kernel: allocated, wrote and freed 3 frames at ",
            ]),
            Some(|work| {
                spcr_args(
                    work,
                    &spcr_table(0, gas_io(0x2E8), 7),
                    &[
                        "-chardev",
                        "null,id=wedged",
                        "-device",
                        "isa-debugcon,iobase=0x2ed,readback=0,chardev=wedged",
                    ],
                )
            }),
            None,
        ),
        console_scenario(
            "console: SPCR for an interface this kernel cannot drive",
            Capture::Com1,
            console_markers(&[
                "origin-kernel: console: the ACPI SPCR table was refused (Interface(3)); the UART stays at I/O port 0x3f8",
                "origin-kernel: allocated, wrote and freed 3 frames at ",
            ]),
            Some(|work| spcr_args(work, &spcr_table(3, gas_io(SPCR_PORT), 7), &[])),
            None,
        ),
        console_scenario(
            "console: SPCR naming a port with no UART",
            Capture::Com1,
            console_markers(&[
                "origin-kernel: console: the ACPI SPCR names a UART at I/O port 0x2e8 that cannot be used (nothing answers at that address); the UART stays at I/O port 0x3f8",
                "origin-kernel: allocated, wrote and freed 3 frames at ",
            ]),
            Some(|work| spcr_args(work, &spcr_table(0, gas_io(0x2E8), 7), &[])),
            None,
        ),
        console_scenario(
            "console: SPCR naming registers in RAM",
            Capture::Com1,
            console_markers(&[
                "origin-kernel: console: the ACPI SPCR names a UART at physical address 0x1000000 (registers 4 bytes apart, 4 bytes wide) that cannot be used (its registers are in RAM); the UART stays at I/O port 0x3f8",
                "origin-kernel: allocated, wrote and freed 3 frames at ",
            ]),
            Some(|work| {
                let mut base = [0u8; 12];
                base[1] = 32; // a 32-bit stride
                base[3] = 3; // dword accesses
                base[4..12].copy_from_slice(&0x0100_0000u64.to_le_bytes());
                spcr_args(work, &spcr_table(0x12, base, 7), &[])
            }),
            None,
        ),
        // COM1's status port answers and never reports room to transmit, and there is no UART
        // behind it: the kernel must give up after one bounded wait and say so on the screen,
        // the only place left to say it. The wait is the whole of the first line's, whoever
        // polls: the count is the limit, not the limit once for each line.
        console_scenario(
            "console: a UART that stops taking bytes",
            Capture::Screen,
            console_markers(&[&format!(
                "origin-kernel: console: the UART was given up on after {} polls; nothing more is sent to it, and the screen said so at once: true",
                framekernel::serial::TX_POLL_LIMIT
            )]),
            Some(|_| {
                Ok(vec![
                    "-chardev".into(),
                    "null,id=wedged".into(),
                    "-device".into(),
                    "isa-debugcon,iobase=0x3fd,readback=0,chardev=wedged".into(),
                ])
            }),
            None,
        ),
        console_scenario(
            "console: no display",
            Capture::Com1,
            console_markers(&[
                "origin-boot: no Graphics Output Protocol",
                "origin-kernel: console: the loader found no framebuffer; output goes to the UART only",
                &format!("origin-kernel: console: {NO_SPCR}"),
            ]),
            Some(|_| Ok(vec!["-vga".into(), "none".into()])),
            None,
        ),
        console_scenario(
            "console: no UART at all",
            Capture::Screen,
            // Read from the screen: the last screenful of lines, the only record of the boot.
            console_markers(&["origin-kernel: sysfs: built /sys with 4 facts"]),
            None,
            None,
        ),
    ]
}

/// The console scenarios (`boot-console`), each a whole boot. `ORIGIN_BOOT_TEST_ONLY` selects
/// by label, as for `boot-test`.
fn boot_console() -> Result {
    check_environment()?;
    let krn_path = pack_kernel(&build_kernel()?)?;
    let (trusted, _) = test_keys()?;
    let signed = sign_kernel(&krn_path, &trusted, "KERNEL.signed.ONBF")?;
    let scenarios: Vec<(Scenario, Vec<u8>)> = console_scenarios()
        .into_iter()
        .map(|scenario| (scenario, signed.clone()))
        .collect();
    let only = env::var("ORIGIN_BOOT_TEST_ONLY").unwrap_or_default();
    let chosen = select_scenarios(&scenarios, &only)?;
    for (scenario, kernel) in &chosen {
        run_scenario(scenario, kernel)?;
    }
    println!(
        "xtask: boot-console passed ({} of {} scenarios: the UART the SPCR names, COM1 named, refused tables, a UART that takes no bytes, one that stops taking them, no display, no UART)",
        chosen.len(),
        scenarios.len()
    );
    Ok(())
}

/// Runs the kernel boot on a machine with no 8254 and no 8042 (ADR-0013 F2, F3), the
/// legacy-free shape of the machines Origin targets.
fn boot_legacy_free() -> Result {
    let exe = env::current_exe().map_err(|e| format!("cannot find this xtask: {e}"))?;
    println!("xtask: boot-legacy-free: no 8254 and no 8042");
    let status = boot_test_child(&exe)
        .env("ORIGIN_QEMU_MACHINE", "pit=off,i8042=off")
        .env("ORIGIN_BOOT_TEST_ONLY", "kernel boot")
        .status()
        .map_err(|e| format!("cannot run boot-test: {e}"))?;
    if !status.success() {
        return Err(format!(
            "boot-test with no 8254 and no 8042 failed ({status})"
        ));
    }
    println!("xtask: boot-legacy-free passed (no 8254, no 8042)");
    Ok(())
}

/// Runs the kernel boot on two processors that lack the instructions the random-number
/// generator is seeded from (ADR-0016 R3, R8): one with RDRAND and no RDSEED, which boots and
/// says what its generator's strength rests on; and one with neither, which has no generator,
/// launches nothing, and still reaches its verdict.
fn boot_random() -> Result {
    let exe = env::current_exe().map_err(|e| format!("cannot find this xtask: {e}"))?;
    for (model, what) in [
        ("max,-rdseed", "RDRAND and no RDSEED"),
        ("max,-rdseed,-rdrand", "neither RDSEED nor RDRAND"),
    ] {
        println!("xtask: boot-random: a processor with {what} (-cpu {model})");
        let status = boot_test_child(&exe)
            .env("ORIGIN_QEMU_CPU", model)
            .env("ORIGIN_BOOT_TEST_ONLY", "kernel boot")
            .status()
            .map_err(|e| format!("cannot run boot-test: {e}"))?;
        if !status.success() {
            return Err(format!(
                "boot-test on a processor with {what} failed ({status})"
            ));
        }
    }
    println!("xtask: boot-random passed (RDRAND alone; neither instruction)");
    Ok(())
}

/// Runs `boot-test` in a child process with the NIC behind a PCIe root port
/// (`ORIGIN_QEMU_BRIDGE=on`), under each IOMMU: bus mastering must be cleared on the bridge at
/// the quiesce and opened again above the device its driver attached (ADR-0007 I12). QEMU
/// itself ignores a bridge's Bus Master Enable, so this proves the kernel's path logic and the
/// read-backs, not the bridge's effect on traffic (the research ledger says so).
fn boot_bridge() -> Result {
    let exe = env::current_exe().map_err(|e| format!("cannot find this xtask: {e}"))?;
    for iommu in ["intel", "amd"] {
        println!("xtask: boot-bridge: the {iommu} IOMMU, the NIC behind a PCIe root port");
        let status = boot_test_child(&exe)
            .env("ORIGIN_QEMU_IOMMU", iommu)
            .env("ORIGIN_QEMU_BRIDGE", "on")
            .status()
            .map_err(|e| format!("cannot run boot-test: {e}"))?;
        if !status.success() {
            return Err(format!(
                "boot-test with the NIC behind a bridge and the {iommu} IOMMU failed ({status})"
            ));
        }
    }
    println!("xtask: boot-bridge passed (both IOMMUs, the NIC behind a PCIe root port)");
    Ok(())
}

/// Runs `boot-test` in a child process on the sparse machine (`ORIGIN_QEMU_TOPOLOGY=x2apic`,
/// see `machine`): processors at APIC IDs 256 and 412 must come online, run threads, and
/// take the NVMe interrupts routed to them through interrupt remapping.
///
/// Twice, because the two models tell the kernel different things about one machine. On an
/// Intel-vendor model QEMU raises the CPUID maximum to 1Fh, so the proof sees dies and modules.
/// On `max` (AMD vendor), the maximum stays 0Dh and only leaf 0Bh remains, so the die and
/// module fields read as part of the core
/// (docs/research/2026-11-08-cpu-topology-leaf-1fh.md).
fn boot_x2apic() -> Result {
    let exe = env::current_exe().map_err(|e| format!("cannot find this xtask: {e}"))?;
    for (model, leaf) in [("max,vendor=GenuineIntel", "leaf 1Fh"), ("max", "leaf 0Bh")] {
        println!(
            "xtask: boot-x2apic: processors at APIC IDs 0, 256 and 412 of 150 possible, -cpu {model} ({leaf})"
        );
        let status = boot_test_child(&exe)
            .env("ORIGIN_QEMU_TOPOLOGY", "x2apic")
            .env("ORIGIN_QEMU_CPU", model)
            .status()
            .map_err(|e| format!("cannot run boot-test: {e}"))?;
        if !status.success() {
            return Err(format!(
                "boot-test on the sparse machine with -cpu {model} failed ({status})"
            ));
        }
    }
    println!(
        "xtask: boot-x2apic passed (processors at APIC IDs 256 and 412, under leaves 1Fh and 0Bh)"
    );
    Ok(())
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
        // The host-only signing code.
        &[
            "clippy",
            "--package",
            SIGN_PACKAGE,
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        // The interface-language tool (a host tool): parser, generators and their tests.
        &[
            "clippy",
            "--package",
            OIDL_PACKAGE,
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        &["test", "--package", "xtask"],
        &["test", "--package", LOADER_PACKAGE, "--lib"],
        &["test", "--package", VERSION_PACKAGE],
        &["test", "--package", PACK_PACKAGE],
        &["test", "--package", SIGN_PACKAGE],
        &["test", "--package", OIDL_PACKAGE],
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
    // The services' logic: `no_std`, tested on the host until programs run it in ring 3 (ADR-0011
    // N14 step 4). Clippy on the host (with tests) and, as a `no_std` build, for the kernel's
    // target, then their tests.
    for &package in SERVICE_LIBS {
        run_step(cargo().args([
            "clippy",
            "--package",
            package,
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ]))?;
        run_step(cargo().args([
            "clippy",
            "--package",
            package,
            "--lib",
            "--target",
            KERNEL_TARGET,
            "--",
            "-D",
            "warnings",
        ]))?;
        run_step(cargo().args(["test", "--package", package]))?;
    }
    // The framework: its logic on the host (with tests), and for the kernel's own target.
    run_step(cargo().args([
        "clippy",
        "--package",
        FRAMEWORK_PACKAGE,
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ]))?;
    run_step(cargo().args([
        "clippy",
        "--package",
        FRAMEWORK_PACKAGE,
        "--lib",
        "--target",
        KERNEL_TARGET,
        "--",
        "-D",
        "warnings",
    ]))?;
    run_step(cargo().args(["test", "--package", FRAMEWORK_PACKAGE]))?;
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
    // The ring-3 crates on their own target (task S4-1), then the kernel, which carries the
    // programs they build.
    ring3::clippy()?;
    run_step(ring3::with_programs(cargo().args([
        "clippy",
        "--package",
        KERNEL_PACKAGE,
        "--target",
        KERNEL_TARGET,
        "--",
        "-D",
        "warnings",
    ]))?)?;
    run_step(cargo().args(["test", "--package", KERNEL_PACKAGE, "--lib"]))?;
    // The loader in developer mode too (the boot test's variant).
    for loader in [Loader::Developer, Loader::Verifying] {
        run_step(cargo().args([
            "clippy",
            "--package",
            LOADER_PACKAGE,
            "--target",
            UEFI_TARGET,
            "--features",
            loader.feature(),
            "--",
            "-D",
            "warnings",
        ]))?;
        build_loader(loader)?;
    }
    image::gate(&build_kernel()?)?;
    no_panic_check()?;
    dependency_check()?;
    linux_cfg_lint()?;
    documents::gate()?;
    requirements::gate()?;
    unsafety::gate()?;
    interfaces::gate()?;
    manifest_version_lint()?;
    signing_stays_off_the_os_lint()?;
    signing_has_no_secret_branches()?;
    reproducible::gate()?;
    loom::run()?;
    println!("xtask: check passed");
    Ok(())
}

/// CLAUDE.md §59: every Ed25519 function that handles the secret scalar or nonce, as
/// `(name, end of its symbol, conditional branches)`. Each branch counted is a loop counter or
/// a test of a public value: none depends on a secret bit. The same on aarch64 and x86-64.
const ED25519_SIGNING_PATH: &[(&str, &str, usize)] = &[
    // One loop per 512-bit reduction: the signer does three, inlined.
    ("ed25519::sign", "7ed255194sign", 3),
    // Expands the seed and calls `base_multiple`.
    ("ed25519::public_key", "7ed2551910public_key", 0),
    // The secret scalar's loop: one counter, for all 256 bits, doubling and adding each one.
    ("ed25519::base_multiple", "7ed2551913base_multiple", 1),
    // The inversion, by the public exponent: its loop and the exponent's two zero bits.
    ("ed25519::Point::compress", "5Point8compress", 3),
];
/// The module part of every Ed25519 symbol in the signing crate (v0 mangling).
const ED25519_SYMBOL: &str = "9onbf_sign7ed25519";

/// Each function in compiler-written `assembly`: its label and the lines up to its
/// `.cfi_endproc`. A label heads a function only if `.cfi_startproc` comes right after it, so
/// local and data labels are not mistaken for one.
fn assembly_functions(assembly: &str) -> Vec<(&str, Vec<&str>)> {
    let mut functions = Vec::new();
    let mut lines = assembly.lines().peekable();
    while let Some(line) = lines.next() {
        let Some(label) = line.strip_suffix(':') else {
            continue;
        };
        if lines.peek().map(|l| l.trim()) != Some(".cfi_startproc") {
            continue;
        }
        let body = lines
            .by_ref()
            .take_while(|l| l.trim() != ".cfi_endproc")
            .collect();
        functions.push((label, body));
    }
    functions
}

/// Whether `line` is a conditional branch instruction on `arch`.
fn is_conditional_branch(line: &str, arch: &str) -> bool {
    let word = line.split_whitespace().next().unwrap_or("");
    match arch {
        "aarch64" => word.starts_with("b.") || ["cbz", "cbnz", "tbz", "tbnz"].contains(&word),
        "x86_64" => word.starts_with('j') && !word.starts_with("jmp"),
        _ => false,
    }
}

/// Counts the conditional branches of each function of the Ed25519 signing path in `assembly`
/// (from `arch`, the host) against [`ED25519_SIGNING_PATH`]. A listed function that is missing
/// fails, as does a count that differs, and an Ed25519 function outside the list, which would
/// be code the check never looks at (the reason this check walks the whole path).
fn check_signing_path(assembly: &str, arch: &str) -> Result<Vec<(&'static str, usize)>> {
    if !matches!(arch, "aarch64" | "x86_64") {
        return Err(format!(
            "§59: the branch check does not know the {arch} instruction set"
        ));
    }
    let functions = assembly_functions(assembly);
    let mut counted = Vec::new();
    for &(name, suffix, expected) in ED25519_SIGNING_PATH {
        let (_, body) = functions
            .iter()
            .find(|(label, _)| label.contains(ED25519_SYMBOL) && label.ends_with(suffix))
            .ok_or_else(|| format!("§59: {name} is not in the assembly"))?;
        let count = body
            .iter()
            .filter(|line| is_conditional_branch(line, arch))
            .count();
        if count != expected {
            return Err(format!(
                "§59: {name} has {count} conditional branches in its {arch} assembly, expected only its {expected} loop counters and public tests; check for a branch on the secret scalar or nonce"
            ));
        }
        counted.push((name, count));
    }
    for (label, _) in &functions {
        let listed = ED25519_SIGNING_PATH
            .iter()
            .any(|&(_, suffix, _)| label.ends_with(suffix));
        if label.contains(ED25519_SYMBOL) && !listed {
            return Err(format!(
                "§59: {label} is an Ed25519 function outside ED25519_SIGNING_PATH: classify it"
            ));
        }
    }
    Ok(counted)
}

/// CLAUDE.md §59: the Ed25519 signing path must compile with no conditional branch that
/// depends on the secret scalar or nonce (see [`ED25519_SIGNING_PATH`]). LLVM once turned the
/// reduction's masks into two data-dependent branches per bit, which `black_box` now prevents;
/// a toolchain or code change that brought them back would show up here. ML-DSA has no such
/// check: its rejection loops branch by design (`onbf-sign`'s side-channel assessment).
fn signing_has_no_secret_branches() -> Result {
    let deps = target_dir().join("release").join("deps");
    run_step(cargo().args([
        "rustc",
        "--quiet",
        "--release",
        "--package",
        SIGN_PACKAGE,
        "--lib",
        "--",
        "--emit=asm",
        "-C",
        "codegen-units=1",
        "-C",
        "debuginfo=0",
        "-C",
        "symbol-mangling-version=v0",
    ]))?;
    let mut assembly: Vec<_> = fs::read_dir(&deps)
        .map_err(|e| format!("{}: {e}", deps.display()))?
        .filter_map(std::result::Result::ok)
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with("onbf_sign-") && name.ends_with(".s")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    assembly.sort();
    let (_, path) = assembly
        .last()
        .ok_or("the signing crate's assembly was not written")?;
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let arch = std::env::consts::ARCH;
    for (name, count) in check_signing_path(&text, arch)? {
        println!(
            "xtask: §59 branch check: {name} has {count} conditional branches, its loop counters and public tests ({arch})"
        );
    }
    Ok(())
}

/// CLAUDE.md §59: Origin OS contains verification code only. The crate with the signing
/// algorithms must not be a dependency of anything that ships.
fn signing_stays_off_the_os_lint() -> Result {
    for package in [LOADER_PACKAGE, KERNEL_PACKAGE] {
        let output = cargo()
            .args([
                "tree",
                "--package",
                package,
                "--prefix",
                "none",
                "--edges",
                "normal",
                "--target",
                "all",
                "--all-features",
            ])
            .output()
            .map_err(|e| format!("cannot run cargo tree: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "cargo tree failed for {package}: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let tree = String::from_utf8_lossy(&output.stdout);
        if tree
            .lines()
            .any(|l| l.split_whitespace().next() == Some(SIGN_PACKAGE))
        {
            return Err(format!(
                "{package} depends on {SIGN_PACKAGE}: the OS must contain verification code only (CLAUDE.md §59)"
            ));
        }
        println!("xtask: §59 signing lint: {package} does not depend on {SIGN_PACKAGE}");
    }
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

/// Freestanding libraries whose object rightly holds no symbols at all: every item in them is
/// generic, so nothing is compiled until a user instantiates it, and the no-panic check has
/// nothing of theirs to read. Listed by name so that nothing else can pass having shown nothing.
const NO_PANIC_NO_SYMBOLS: &[&str] = &["origin-sync"];

/// What `nm` says about `obj` for `flag`. A failing `nm` is an error, not an empty list.
fn nm_symbols(nm: &Path, obj: &Path, flag: &str) -> Result<Vec<String>> {
    let out = Command::new(nm)
        .arg(flag)
        .arg(obj)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", nm.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{} {flag} {} failed ({}): {}",
            nm.display(),
            obj.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    // Lines are "[address] TYPE name"; the name is the last field.
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().last().map(str::to_string))
        .collect())
}

/// Fails if `package`'s object listed no symbols at all: a gate that saw nothing has shown
/// nothing, unless the library is one of [`NO_PANIC_NO_SYMBOLS`].
fn require_symbols(package: &str, obj: &Path, symbols: usize) -> Result {
    if symbols == 0 && !NO_PANIC_NO_SYMBOLS.contains(&package) {
        return Err(format!(
            "no-panic check: {} ({package}) has no symbols: nothing was checked",
            obj.display()
        ));
    }
    Ok(())
}

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
    let mut defined = std::collections::HashSet::new();
    let mut referenced = Vec::new();
    for (package, obj) in &objects {
        let own = nm_symbols(&nm, obj, "--defined-only")?;
        let undefined = nm_symbols(&nm, obj, "-u")?;
        require_symbols(package, obj, own.len() + undefined.len())?;
        if own.is_empty() && undefined.is_empty() {
            println!(
                "xtask: no-panic check: {package} ({KERNEL_TARGET}, release) is all generic: no symbols of its own to check"
            );
            continue;
        }
        defined.extend(own);
        referenced.push((package, undefined));
    }
    for (package, undefined) in &referenced {
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

/// Where every crate in [`APPROVED_THIRD_PARTY`] comes from.
const APPROVED_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// Every third-party crate the Loom workspace's lock file (`src/loom.lock`) may hold, as
/// `name version sha256-of-the-crate`, and no others may be anywhere else: the root `Cargo.lock`
/// holds none. All of them are `loom` and what it brings, used only by the host-only models
/// (`loom.rs`): approved by the owner with these versions, licences and sizes on 2026-11-08
/// (`SOURCE_PROVENANCE.md`, CLAUDE.md §5, §60). Changing this list is a new approval.
const APPROVED_THIRD_PARTY: &[&str] = &[
    "aho-corasick 1.1.5 c982642fa9e8606056828ee9a8505737230110bb1099153c79efe865c59d12ba",
    "cc 1.10.0 86e055fa6e3e40ad6187ba25b6855eeb635d5271f8e0f0e1e9a5826b8d75bea0",
    "cfg-if 1.0.5 4e7648175b45a9a48536d676f68d918270699102aa8dab5496df06904c914600",
    "find-msvc-tools 0.1.15 1c66915cc5391097de7a2b0b972e8505aa33c0df401fd3a6811dbdb32881ba48",
    "generator 0.8.11 1be81b31c9ce4caf7f3178eeb90662bb1864d3ad363ec6bdada3201adff19c37",
    "lazy_static 1.5.1 20870f649af7073d53e38067b2a84312175d56ea15217e1b15bc83506ec50afb",
    "libc 0.2.191 65cd87c9a6d60536580b316ad985726104730a5151c99845f2490428ef91240a",
    "log 0.4.34 f9f8bd3e56ce4dfc153cf470fffbfa98c7620958b312ca5c3a4b8d5181fd13c6",
    "loom 0.7.2 419e0dc8046cb947daa77eb95ae174acfbddb7673b4151f56d1eed8e93fbfaca",
    "matchers 0.2.0 d1525a2a28c7f4fa0fc98bb91ae755d1e2d1505079e05539e35bc876b5d65ae9",
    "memchr 2.8.3 cf8baf1c55e62ffcace7a9f06f4bd9cd3f0c4beb022d3b367256b91b87513d98",
    "nu-ansi-term 0.50.3 7957b9740744892f114936ab4a57b3f487491bbeafaf8083688b16841a4240e5",
    "once_cell 1.21.4 9f7c3e4beb33f85d45ae3e3a1792185706c8e16d043238c593331cc7cd313b50",
    "pin-project-lite 0.2.17 a89322df9ebe1c1578d689c92318e070967d1042b512afbe49518723f4e6d5cd",
    "regex-automata 0.4.18 ad8553b9b26413251cbf30e620595c7a41b3887f03da04579c0e6b0d6a06b4b2",
    "regex-syntax 0.8.11 d6f6ff9a378485b298a5286656da665ba74413d36db0979633275d2e708145d4",
    "rustversion 1.0.23 cf54715a573b99ac80df0bc206da022bcd442c974952c7b9720069370852e21f",
    "scoped-tls 1.0.1 e1cf6437eb19a8f4a6cc0f7dca544973b0b78843adbfeb3683d1a94a0024a294",
    "sharded-slab 0.1.7 f40ca3c46823713e0d4209592e8d6e826aa57e928f09752619fc696c499637f6",
    "shlex 2.0.1 f8fadd59c855ef2080decdef8ff161eb6661b86933c9d82e5ba29dc602a55aba",
    "smallvec 1.16.4 d0d5d4b1981b8bbbb7aed204cae5726d683d5d6ac7e74ec54122ea28f315e7c8",
    "thread_local 1.1.10 1ad99c4c6d32803332c548b1af0540b357b3f5fc0be8f6c6bfe8b2e6ae784070",
    "tracing 0.1.44 63e71662fa4b2a2c3a26f570f037eb95bb1f85397f3cd8076caed2f026a6d100",
    "tracing-core 0.1.36 db97caf9d906fbde555dd62fa95ddba9eecfd14cb388e4f491a66d74cd5fb79a",
    "tracing-log 0.2.0 ee855f1f400bd0e5c02d150ae5de3840039a3f54b025156404e34c23c03f47c3",
    "tracing-subscriber 0.3.23 cb7f578e5945fb242538965c2d0b04418d38ec25c79d160cd279bf0731c8d319",
    "valuable 0.1.1 ba73ea9cf16a25df0c8caa16c51acb937d5712a8429db78a3ee29d5dcacd3a65",
    "windows-link 0.2.1 f0805222e57f7521d6a62e36fa9163bc891acd422f971defe97d64e70d0a4fe5",
    "windows-result 0.4.1 7781fa89eaf60850ac3d2da7af8e5242a5ea78d1a11c49bf2910bb5a73853eb5",
    "windows-sys 0.61.2 ae137229bcbd6cdf0f7b80a31df61766145077ddf49416a728b02cb3921ff3fc",
];

/// One `[[package]]` of a `Cargo.lock`.
#[derive(Debug, PartialEq, Eq)]
struct LockedPackage {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
}

/// The packages of a `Cargo.lock`.
fn locked_packages(lock: &str) -> Vec<LockedPackage> {
    lock.split("[[package]]")
        .skip(1)
        .map(|package| {
            let field = |key: &str| {
                package.lines().find_map(|line| {
                    let value = line.strip_prefix(key)?.strip_prefix(" = \"")?;
                    value.strip_suffix('"').map(str::to_string)
                })
            };
            LockedPackage {
                name: field("name").unwrap_or_default(),
                version: field("version").unwrap_or_default(),
                source: field("source"),
                checksum: field("checksum"),
            }
        })
        .collect()
}

/// What is wrong with the root `Cargo.lock`: it must name the workspace's own packages and
/// nothing else, so that nothing but this repository is an input to building the kernel
/// (CLAUDE.md §5, §60, §75). A package with a source or a checksum came from somewhere else;
/// any table but `[[package]]` (`[[patch.unused]]`, say) is something else to review.
fn root_lock_problems(lock: &str) -> Vec<String> {
    let foreign = locked_packages(lock)
        .into_iter()
        .filter(|package| package.source.is_some() || package.checksum.is_some())
        .map(|package| {
            let from = package.source.unwrap_or_else(|| "a checksum".to_string());
            format!("{} {} ({from})", package.name, package.version)
        });
    let tables = lock
        .lines()
        .filter(|line| line.starts_with('[') && *line != "[[package]]")
        .map(|line| format!("the table {line}"));
    foreign.chain(tables).collect()
}

/// Each value of `"key":"..."` in the compact JSON of `cargo metadata`, or `None` if one holds
/// an escape: a value that cannot be compared must not be guessed at.
fn json_strings(json: &str, key: &str) -> Option<Vec<String>> {
    let opening = format!("\"{key}\":\"");
    json.match_indices(&opening)
        .map(|(at, _)| {
            let value = json.get(at + opening.len()..)?;
            let value = value.get(..value.find('"')?)?;
            (!value.contains('\\')).then(|| value.to_string())
        })
        .collect()
}

/// What is wrong with the workspace's manifests, from `cargo metadata --no-deps` (every
/// dependency of every member, for every target and feature): a dependency on a registry or git
/// crate, or a path dependency on anything but a member of the workspace. Such a path
/// dependency is third-party code too, and a `Cargo.lock` shows it without a `source`.
fn manifest_problems(metadata: &str) -> Result<Vec<String>> {
    let strings = |key: &str| {
        json_strings(metadata, key)
            .ok_or_else(|| format!("cargo metadata: an escape in a {key} value"))
    };
    let manifests = strings("manifest_path")?;
    let members: Vec<&Path> = manifests
        .iter()
        .filter_map(|manifest| Path::new(manifest).parent())
        .collect();
    let mut problems: Vec<String> = strings("source")?
        .into_iter()
        .map(|source| format!("a dependency from {source}"))
        .collect();
    problems.extend(
        strings("path")?
            .into_iter()
            .filter(|path| !members.contains(&Path::new(path)))
            .map(|path| format!("a path dependency outside the workspace: {path}")),
    );
    Ok(problems)
}

/// What is wrong with the Loom workspace's lock file: it must hold `origin-sync` and exactly the
/// approved crates, each from the approved source at its pinned checksum.
fn model_lock_problems(lock: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let mut approved_seen = Vec::new();
    for package in locked_packages(lock) {
        let shown = format!("{} {}", package.name, package.version);
        match (package.source.as_deref(), package.checksum.as_deref()) {
            (None, None) if package.name == "origin-sync" => {}
            (Some(APPROVED_SOURCE), Some(checksum)) => {
                let pinned = format!("{shown} {checksum}");
                match APPROVED_THIRD_PARTY.iter().find(|a| **a == pinned) {
                    Some(approved) => approved_seen.push(*approved),
                    None => {
                        problems.push(format!("{shown} at checksum {checksum} is not approved"))
                    }
                }
            }
            (source, checksum) => problems.push(format!(
                "{shown}: unexpected source {source:?} or checksum {checksum:?}"
            )),
        }
    }
    problems.extend(
        APPROVED_THIRD_PARTY
            .iter()
            .filter(|approved| !approved_seen.contains(*approved))
            .map(|approved| format!("{approved} is approved but missing")),
    );
    problems
}

/// Fails unless `lock`, the Loom workspace's lock file, is the approved one (CLAUDE.md §60, §75).
fn check_model_lock(lock: &str) -> Result {
    let problems = model_lock_problems(lock);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the Loom workspace's lock file (developer-tools/xtask/src/loom.lock) is not the \
             approved one (CLAUDE.md §5, §60):\n  {}",
            problems.join("\n  ")
        ))
    }
}

/// CLAUDE.md §5, §60 and §75: the root `Cargo.lock` holds this workspace's packages and nothing
/// else, no member reaches a crate outside the workspace, and the one third-party set there is
/// (the host-only Loom workspace's) is the approved one, by source and checksum.
fn dependency_check() -> Result {
    let path = repo_root().join("Cargo.lock");
    let lock = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let problems = root_lock_problems(&lock);
    if !problems.is_empty() {
        return Err(format!(
            "Cargo.lock must hold only this workspace's packages (CLAUDE.md §5, §60, §75):\n  {}",
            problems.join("\n  ")
        ));
    }
    let out = cargo()
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .output()
        .map_err(|e| format!("cannot run cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let problems = manifest_problems(&String::from_utf8_lossy(&out.stdout))?;
    if !problems.is_empty() {
        return Err(format!(
            "this workspace depends on crates outside it (CLAUDE.md §5, §60):\n  {}",
            problems.join("\n  ")
        ));
    }
    check_model_lock(loom::MODEL_LOCK)?;
    println!(
        "xtask: dependency check: Cargo.lock holds only this workspace's {} packages, no member \
         depends on anything outside it, and the Loom workspace's lock holds the {} approved \
         crates at their pinned sources and checksums",
        locked_packages(&lock).len(),
        APPROVED_THIRD_PARTY.len()
    );
    Ok(())
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
/// code generator, so it must never ask whether its target is Linux, or Unix, or which C
/// library it has: `target_os = "linux"`, `cfg(unix)`, `target_family` and `target_env`
/// (`targetcfg` has the rule). Host developer tools (`developer-tools/`) run on Linux or macOS
/// build hosts and are exempt.
fn linux_cfg_lint() -> Result {
    let root = repo_root();
    let dev_tools = root.join("developer-tools");
    let mut sources = Vec::new();
    collect_files(
        &root,
        &|dir| dir == dev_tools,
        &|name| name.ends_with(".rs"),
        &mut sources,
    )
    .map_err(|e| format!("cfg lint: {e}"))?;
    // The manifests, and Cargo's own configuration, which can set flags by `cfg` too.
    let mut manifests = Vec::new();
    collect_files(
        &root,
        &|dir| dir == dev_tools,
        &|name| name == "Cargo.toml",
        &mut manifests,
    )
    .map_err(|e| format!("cfg lint: {e}"))?;
    // `config.toml`, and the userspace flags xtask passes to the ring-3 builds (task S4-1).
    for name in ["config.toml", "userspace.toml"] {
        let cargo_config = root.join(".cargo").join(name);
        if cargo_config.is_file() {
            manifests.push(cargo_config);
        }
    }
    if sources.is_empty() || manifests.is_empty() {
        return Err(format!(
            "cfg lint: {} Rust files and {} manifests found outside developer-tools/: nothing was checked",
            sources.len(),
            manifests.len()
        ));
    }
    let mut hits = Vec::new();
    for (files, read) in [
        (
            &sources,
            targetcfg::rust_hits as fn(&str) -> Vec<targetcfg::Hit>,
        ),
        (&manifests, targetcfg::manifest_hits),
    ] {
        for file in files {
            let text = fs::read_to_string(file)
                .map_err(|e| format!("cfg lint: {}: {e}", file.display()))?;
            let shown = file.strip_prefix(&root).unwrap_or(file);
            for hit in read(&text) {
                hits.push(format!("{}:{}: {}", shown.display(), hit.line, hit.what));
            }
        }
    }
    println!(
        "xtask: §11 cfg lint scanned {} Rust files and {} manifests outside developer-tools/",
        sources.len(),
        manifests.len()
    );
    if hits.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Origin code is compiled for the Linux triple only as a code generator, and must not ask whether its target is Linux or Unix (CLAUDE.md §11):\n  {}\nuse `target_os = \"none\"` for the kernel, `target_os = \"uefi\"` for the loader, or no condition at all",
            hits.join("\n  ")
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

/// Builds the EFI application with `loader`'s feature and returns the path of
/// `origin-boot.efi`.
fn build_loader(loader: Loader) -> Result<PathBuf> {
    build_loader_in(Some(loader.feature()), None)
}

/// Builds the EFI application with `feature`, or with none, into `target`, or into Cargo's own
/// target directory, and returns the path of `origin-boot.efi`. With no feature the trust
/// store is empty: the loader a release builds, until Origin has keys of its own.
fn build_loader_in(feature: Option<&str>, target: Option<&Path>) -> Result<PathBuf> {
    let mut build = cargo();
    build.args([
        "build",
        "--package",
        LOADER_PACKAGE,
        "--bin",
        LOADER_BIN,
        "--target",
        UEFI_TARGET,
        "--release",
    ]);
    if let Some(feature) = feature {
        build.args(["--features", feature]);
    }
    if let Some(target) = target {
        build.arg("--target-dir").arg(target);
    }
    run_step(&mut build)?;
    let efi = target
        .map_or_else(target_dir, Path::to_path_buf)
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
    build_kernel_in(None)
}

/// Builds the Framekernel ELF into `target`, or into Cargo's own target directory, and returns
/// its path.
fn build_kernel_in(target: Option<&Path>) -> Result<PathBuf> {
    let mut build = cargo();
    build.args([
        "build",
        "--package",
        KERNEL_PACKAGE,
        "--bin",
        KERNEL_PACKAGE,
        "--target",
        KERNEL_TARGET,
        "--release",
    ]);
    if let Some(target) = target {
        build.arg("--target-dir").arg(target);
    }
    // The programs it carries in its image (task S4-1).
    run_step(ring3::with_programs(&mut build)?)?;
    let elf = target
        .map_or_else(target_dir, Path::to_path_buf)
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

// ------------------------------------------------------------------ signing, for the boot tests

/// The text the test keys are derived from (`onbf-pack keygen --insecure-test-seed`). This is
/// the only "private" input they have, and it is public, so they are test keys by
/// construction and must never sign anything real (CLAUDE.md §59).
const TEST_KEY_TRUSTED: &str = "TEST-ONLY origin-kernel key 1: the test trust store holds it";
const TEST_KEY_UNTRUSTED: &str = "TEST-ONLY origin-kernel key 2: no trust store holds it";
/// The test statements' claimed signing time. Informational (ONBF §7.6), and fixed: a build
/// reads no clock (ONBF-R9).
const TEST_SIGNING_TIME: &str = "1794000000";

/// A test key's files, under `target/xtask/test-keys/` (never in the repository).
struct TestKey {
    key: PathBuf,
    public_ed25519: PathBuf,
    public_mldsa: PathBuf,
}

/// Runs `onbf-pack` with these arguments.
fn onbf_pack(args: &[&std::ffi::OsStr]) -> Result {
    run_step(
        cargo()
            .args([
                "run",
                "--quiet",
                "--release",
                "--package",
                PACK_PACKAGE,
                "--",
            ])
            .args(args),
    )
}

/// Creates the test key derived from `seed` in a fresh directory `name`.
fn make_test_key(name: &str, seed: &str) -> Result<TestKey> {
    let dir = target_dir().join("xtask").join("test-keys").join(name);
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let key = TestKey {
        key: dir.join("origin-kernel.key"),
        public_ed25519: dir.join("ed25519.pub"),
        public_mldsa: dir.join("mldsa87.pub"),
    };
    onbf_pack(&[
        "keygen".as_ref(),
        "--insecure-test-seed".as_ref(),
        seed.as_ref(),
        "--key".as_ref(),
        key.key.as_os_str(),
        "--public-ed25519".as_ref(),
        key.public_ed25519.as_os_str(),
        "--public-mldsa".as_ref(),
        key.public_mldsa.as_os_str(),
    ])?;
    Ok(key)
}

/// `path`'s contents, or an error naming it.
fn read_file(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Derives the two test keys, and checks that the trusted one is the public key the loader
/// has compiled in: a stale copy in `boot/uefi/trust` would otherwise show up as a boot
/// failure with no explanation.
fn test_keys() -> Result<(TestKey, TestKey)> {
    let trusted = make_test_key("trusted", TEST_KEY_TRUSTED)?;
    let untrusted = make_test_key("untrusted", TEST_KEY_UNTRUSTED)?;
    let committed = repo_root().join("boot").join("uefi").join("trust");
    for (name, generated) in [
        ("TEST-ONLY-origin-kernel.ed25519", &trusted.public_ed25519),
        ("TEST-ONLY-origin-kernel.mldsa87", &trusted.public_mldsa),
    ] {
        if read_file(&committed.join(name))? != read_file(generated)? {
            return Err(format!(
                "boot/uefi/trust/{name} is not the public half of the test key derived from \
                 {TEST_KEY_TRUSTED:?}; regenerate it with `onbf-pack keygen`, as \
                 boot/uefi/trust/README.md describes"
            ));
        }
    }
    Ok((trusted, untrusted))
}

/// Signs the unsigned KRN image `input` with `key`, into `target/xtask/<output>`, and returns
/// the signed bytes.
fn sign_kernel(input: &Path, key: &TestKey, output: &str) -> Result<Vec<u8>> {
    let out = target_dir().join("xtask").join(output);
    onbf_pack(&[
        "sign".as_ref(),
        "--input".as_ref(),
        input.as_os_str(),
        "--output".as_ref(),
        out.as_os_str(),
        "--key".as_ref(),
        key.key.as_os_str(),
        "--signing-time".as_ref(),
        TEST_SIGNING_TIME.as_ref(),
    ])?;
    read_file(&out)
}

/// Where a part of a signed image lies in it.
fn offset_within(whole: &[u8], part: &[u8]) -> Result<usize> {
    let start = (part.as_ptr() as usize).wrapping_sub(whole.as_ptr() as usize);
    if start <= whole.len() && part.len() <= whole.len() - start {
        Ok(start)
    } else {
        Err("a signature field lies outside the image".to_string())
    }
}

/// A copy of `krn` with byte `at` flipped.
fn flipped(krn: &[u8], at: usize) -> Result<Vec<u8>> {
    let mut out = krn.to_vec();
    let byte = out.get_mut(at).ok_or("flip outside the image")?;
    *byte ^= 0x01;
    Ok(out)
}

/// The offsets of the signed statement's content digest, the Ed25519 signature and the
/// ML-DSA signature in the signed image `krn`.
fn signature_fields(krn: &[u8]) -> Result<(usize, usize, usize)> {
    let options = onbf_format::ParseOptions {
        developer_mode: true,
        ..Default::default()
    };
    let image = onbf_format::Image::parse(krn, options).map_err(|e| format!("KRN image: {e:?}"))?;
    let block = image.signature_block().ok_or("the KRN image is unsigned")?;
    let parsed = onbf_format::signature::SignatureBlock::parse(block)
        .map_err(|e| format!("signature block: {e:?}"))?;
    let entry = parsed.entries().next().ok_or("no signature entry")?;
    Ok((
        offset_within(krn, entry.statement.content_digest)?,
        offset_within(krn, entry.sig_ed25519)?,
        offset_within(krn, entry.sig_mldsa)?,
    ))
}

/// A forged image whose digests all agree: one byte of a SEGMENT changed, its Merkle root and
/// the content digest recomputed. Only the signed statement can tell it from the real one.
fn substituted(krn: &[u8]) -> Result<Vec<u8>> {
    let options = onbf_format::ParseOptions {
        developer_mode: true,
        ..Default::default()
    };
    let image = onbf_format::Image::parse(krn, options).map_err(|e| format!("KRN image: {e:?}"))?;
    let segment = image
        .sections()
        .find(|s| s.entry.kind() == onbf_format::Kind::Segment && s.entry.stored_size > 0)
        .ok_or("the KRN image has no segment with content")?;
    let start = usize::try_from(segment.entry.file_offset).map_err(|_| "segment offset")?;
    let len = usize::try_from(segment.entry.stored_size).map_err(|_| "segment size")?;
    let entry = 128 + usize::from(segment.index) * 128;
    let table_end = 128 + image.section_count() * 128;
    let mut out = krn.to_vec();
    let content = out
        .get_mut(start..start + len)
        .ok_or("segment outside the file")?;
    let middle = content.len() / 2;
    let byte = content.get_mut(middle).ok_or("empty segment")?;
    *byte ^= 0x01;
    let root = onbf_merkle::root(
        out.get(start..start + len)
            .ok_or("segment outside the file")?,
    )
    .map_err(|e| format!("Merkle root: {e:?}"))?;
    out.get_mut(entry + 64..entry + 96)
        .ok_or("section table outside the file")?
        .copy_from_slice(&root);
    let mut digest = origin_crypto::Sha512::new();
    for range in [0..32, 112..128, 128..table_end] {
        digest.update(out.get(range).ok_or("header outside the file")?);
    }
    out.get_mut(48..112)
        .ok_or("header outside the file")?
        .copy_from_slice(&digest.finalize());
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
    /// The NVMe test disk (`disk`).
    nvme: PathBuf,
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

/// Builds the `loader` variant and lays out a fresh ESP, holding `kernel` as the KRN image,
/// and a fresh NVRAM copy for one boot.
fn stage(kernel: &[u8], loader: Loader) -> Result<Staged> {
    let efi = build_loader(loader)?;
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
    // A fresh NVMe test disk for every boot, too.
    let nvme = work.join("nvme.img");
    disk::write_image(&nvme)?;
    Ok(Staged {
        firmware,
        esp,
        vars,
        nvme,
    })
}

/// A QEMU option value that ends in a path (a `-drive` or `-netdev` value). QEMU option
/// syntax escapes a literal comma as ",,".
fn qemu_path_arg(options: &str, path: &Path) -> Result<String> {
    let p = path
        .to_str()
        .ok_or_else(|| format!("path is not UTF-8: {}", path.display()))?;
    Ok(format!("{options}{}", p.replace(',', ",,")))
}

/// The machine every boot uses: q35 (the only machine type the Secure Boot firmware
/// supports), x86-64 with every feature TCG can emulate, and no network or display.
fn qemu_command(s: &Staged) -> Result<Command> {
    let machine = machine();
    let mut c = Command::new(QEMU);
    c.arg("-machine")
        .arg(qemu_machine_options(Accel::from_env()?.machine()))
        .arg("-cpu")
        .arg(qemu_cpu_model())
        .args(["-m", "512M", "-smp"])
        .arg(&machine.smp);
    for cpu in &machine.devices {
        c.args(["-device", cpu]);
    }
    c
        // Two NUMA nodes so the ACPI SRAT is populated (CLAUDE.md §24, §28). The 256M
        // backends sum to -m, and QEMU auto-distributes the vCPUs across the nodes, so this
        // is valid for any -smp (verified at 1/2/4/8, and for the sparse x2APIC machine,
        // whose packages go to one node each).
        .args(["-object", "memory-backend-ram,id=onuma0,size=256M"])
        .args(["-object", "memory-backend-ram,id=onuma1,size=256M"])
        .args(["-numa", "node,nodeid=0,memdev=onuma0"])
        .args(["-numa", "node,nodeid=1,memdev=onuma1"])
        // A cross-node distance so the ACPI SLIT is populated too (QEMU mirrors 1->0).
        .args(["-numa", "dist,src=0,dst=1,val=21"])
        .arg("-drive")
        .arg(qemu_path_arg(
            "if=pflash,format=raw,readonly=on,file=",
            &s.firmware.code,
        )?)
        .arg("-drive")
        .arg(qemu_path_arg("if=pflash,format=raw,file=", &s.vars)?)
        .arg("-drive")
        .arg(qemu_path_arg("format=raw,file=fat:rw:", &s.esp)?)
        // The IOMMU (CLAUDE.md §32, §58; ADR-0007), before the PCI devices it translates.
        .args(qemu_iommu().device_args())
        // The NVMe controller and its test disk (CLAUDE.md §33).
        .arg("-drive")
        .arg(qemu_path_arg("if=none,id=nvme0,format=raw,file=", &s.nvme)?)
        .args(["-device", "nvme,serial=ORIGIN0001,drive=nvme0"]);
    // A PCIe root port for the NIC (threat T4): clearing bus mastering stops its forwarding too,
    // and the driver must open it again above the device it attached (`boot-bridge`).
    let nic_bus = if qemu_bridge() {
        c.args([
            "-device",
            "pcie-root-port,id=rp0,chassis=1,slot=1,bus=pcie.0,addr=04.0",
        ]);
        ",bus=rp0"
    } else {
        ""
    };
    c
        // A modern virtio-net NIC with a fixed MAC, on QEMU user networking (CLAUDE.md §54).
        // Its built-in TFTP server serves framekernel/assets read-only, so the kernel can
        // fetch its own initramfs image over the network and compare it with the embedded
        // copy (no host network involved). `iommu_platform=on` puts its DMA behind the IOMMU
        // (without it QEMU's virtio devices bypass it) and offers VIRTIO_F_ACCESS_PLATFORM.
        .arg("-netdev")
        .arg(qemu_path_arg(
            "user,id=net0,tftp=",
            &repo_root().join("framekernel").join("assets"),
        )?)
        .arg("-device")
        .arg(format!(
            "virtio-net-pci,netdev=net0,mac=52:54:00:aa:bb:cc,disable-legacy=on,iommu_platform=on{nic_bus}"
        ))
        .args(["-display", "none", "-no-reboot"])
        .arg("-rtc")
        .arg(format!("base={RTC_BASE}"));
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

/// Every boot `boot-test` runs, with the kernel image each one boots.
fn boot_test_scenarios(unsigned: &Path) -> Result<Vec<(Scenario, Vec<u8>)>> {
    let krn = read_file(unsigned)?;
    let (trusted, untrusted) = test_keys()?;
    let signed = sign_kernel(unsigned, &trusted, "KERNEL.signed.ONBF")?;
    let wrong_key = sign_kernel(unsigned, &untrusted, "KERNEL.wrong-key.ONBF")?;
    let (statement_digest, ed25519, mldsa) = signature_fields(&signed)?;
    use Loader::{Developer, Verifying};
    let refuse = refusal_scenario;
    Ok(vec![
        // The real boot: developer mode off, a signed kernel, every kernel proof.
        (boot_scenario(), signed.clone()),
        // Developer mode still boots an unsigned kernel, and still verifies what it can.
        (developer_scenario(), krn.clone()),
        (tamper_scenario(), tampered(&krn)?),
        // Developer mode does not skip the signature check of a signed image (errata V5).
        (
            refuse(
                "bad signature in developer mode",
                Developer,
                "the ML-DSA signature does not verify over the statement",
                &[],
            ),
            flipped(&signed, mldsa)?,
        ),
        // What a loader with developer mode off must refuse, each with its own message.
        (
            refuse(
                "unsigned kernel",
                Verifying,
                "unsigned image: a signature by an origin-kernel key is required outside developer mode",
                &[],
            ),
            krn,
        ),
        (
            refuse(
                "wrong key",
                Verifying,
                "signed by a key this loader does not trust",
                &[],
            ),
            wrong_key,
        ),
        (
            refuse(
                "tampered statement",
                Verifying,
                "the signed statement does not name this image (its content digest differs)",
                &[],
            ),
            flipped(&signed, statement_digest)?,
        ),
        (
            refuse(
                "tampered Ed25519 signature",
                Verifying,
                "the Ed25519 signature does not verify over the statement",
                &[],
            ),
            flipped(&signed, ed25519)?,
        ),
        (
            refuse(
                "tampered ML-DSA signature",
                Verifying,
                "the ML-DSA signature does not verify over the statement",
                &[],
            ),
            flipped(&signed, mldsa)?,
        ),
        // Past the signature, each section's digest catches a changed byte...
        (
            refuse(
                "tampered content",
                Verifying,
                "section ",
                &["does not match its digest"],
            ),
            tampered(&signed)?,
        ),
        // ...and when an attacker makes those digests agree, only the signature can tell.
        (
            refuse(
                "substituted kernel",
                Verifying,
                "the signed statement does not name this image (its content digest differs)",
                &[],
            ),
            substituted(&signed)?,
        ),
    ])
}

fn boot_test() -> Result {
    check_environment()?;
    let krn_path = pack_kernel(&build_kernel()?)?;
    let scenarios = boot_test_scenarios(&krn_path)?;
    let only = env::var("ORIGIN_BOOT_TEST_ONLY").unwrap_or_default();
    let chosen = select_scenarios(&scenarios, &only)?;
    for (scenario, kernel) in &chosen {
        run_scenario(scenario, kernel)?;
    }
    println!(
        "xtask: boot-test passed ({} of {} scenarios)",
        chosen.len(),
        scenarios.len()
    );
    Ok(())
}

/// The scenarios whose label contains one of the comma-separated strings in `only`, or all of
/// them if it is empty. A filter that matches nothing is an error: a mistyped name must not
/// report a passing run with nothing booted.
fn select_scenarios<'a>(
    scenarios: &'a [(Scenario, Vec<u8>)],
    only: &str,
) -> Result<Vec<&'a (Scenario, Vec<u8>)>> {
    let chosen: Vec<_> = scenarios
        .iter()
        .filter(|(s, _)| only.is_empty() || only.split(',').any(|p| s.label.contains(p)))
        .collect();
    if chosen.is_empty() {
        return Err(format!(
            "ORIGIN_BOOT_TEST_ONLY={only:?} matches none of the {} scenarios",
            scenarios.len()
        ));
    }
    Ok(chosen)
}

/// Boots `kernel` once and checks its output against `scenario`.
fn run_scenario(scenario: &Scenario, kernel: &[u8]) -> Result {
    let staged = stage(kernel, scenario.loader)?;
    let timeout = boot_test_timeout()?;
    let accel = Accel::from_env()?;
    let work = target_dir().join("xtask");
    let mut cmd = qemu_command(&staged)?;
    if let Some(args) = scenario.qemu_args {
        cmd.args(args(&work)?);
    }
    let com1_log = work.join("com1.log");
    let _ = fs::remove_file(&com1_log);
    match scenario.capture {
        Capture::Com1 => {
            cmd.args(["-serial", "stdio", "-monitor", "none"]);
        }
        Capture::Com1AndScreen => {
            cmd.args(["-serial", "mon:stdio"]);
        }
        Capture::SpcrUart => {
            cmd.arg("-serial")
                .arg(qemu_path_arg("file:", &com1_log)?)
                .args(["-chardev", "stdio,id=spcruart", "-device"])
                .arg(format!(
                    "isa-serial,iobase={SPCR_PORT:#x},irq=3,chardev=spcruart"
                ))
                .args(["-monitor", "none"]);
        }
        Capture::Screen => {
            cmd.args(["-serial", "none", "-monitor", "stdio"]);
        }
    }
    let monitored = matches!(scenario.capture, Capture::Com1AndScreen | Capture::Screen);
    cmd.stdin(if monitored {
        Stdio::piped()
    } else {
        Stdio::null()
    })
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
    let mut stdin = child.stdin.take();

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
    let screen_path = work.join("screen.ppm");
    let mut verdict: Result = if scenario.capture == Capture::Screen {
        let monitor = stdin.as_mut().ok_or("QEMU stdin was not captured")?;
        wait_for_screen(
            monitor,
            &screen_path,
            &markers,
            &accel.screen_failures(&scenario.failures),
            deadline,
        )
    } else {
        loop {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(chunk) => {
                    raw.extend_from_slice(&chunk);
                    let text = serial::clean(&raw);
                    // Failures are matched in complete lines only, so a failing line reaches
                    // the log in full (its details follow the marker) before QEMU is stopped.
                    let complete = text
                        .get(..text.rfind('\n').map_or(0, |i| i + 1))
                        .unwrap_or("");
                    let judged = accel.judged(complete);
                    if let Some(m) = scenario.failures.iter().find(|m| judged.contains(**m)) {
                        break Err(format!("the serial output contains {m:?}"));
                    }
                    if serial::contains_in_order(&text, &markers) {
                        break Ok(());
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    let text = serial::clean(&raw);
                    let missing = serial::first_missing(&text, &markers).unwrap_or("(none)");
                    break Err(format!(
                        "first missing marker {missing:?}; no complete loader output within {}s",
                        timeout.as_secs()
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    break Err("QEMU exited before the loader output was complete".into());
                }
            }
        }
    };
    let elapsed = started.elapsed();

    // What the markers were found in, before any monitor output joins the stream.
    let text = serial::clean(&raw);
    let mut screen = None;
    if verdict.is_ok() && scenario.capture == Capture::Com1AndScreen {
        match stdin.as_mut() {
            Some(monitor) => match settled_screen(monitor, &screen_path, &text) {
                Ok(image) => screen = Some(image),
                Err(e) => verdict = Err(format!("no screen dump: {e}")),
            },
            None => verdict = Err("QEMU stdin was not captured".into()),
        }
    }
    // Where this boot placed the kernel stacks' region, read through the monitor, for
    // `boot-matrix` to compare across boots (ADR-0004 Amendment 2, A6.4). A fixed base, on the
    // machine whose loader had no random value to hand over, is checked and not recorded.
    if verdict.is_ok()
        && scenario.capture == Capture::Com1AndScreen
        && text.contains(STACK_PROOF_LINE)
        && let Some(monitor) = stdin.as_mut()
    {
        verdict = read_stack_base(monitor, &rx, &text)
            .and_then(|base| base.map_or(Ok(()), |base| record_stack_base(scenario.label, base)))
            .map_err(|e| format!("the kernel stacks' region's base: {e}"));
    }
    drop(stdin);

    // QEMU keeps running after the scenario ends (the kernel halts; a rejecting loader hands
    // back to the firmware), so the test stops it. (QEMU does not exit on SIGALRM.)
    let _ = child.kill();
    let exit = child.wait();
    let qemu_stderr = stderr_reader.join().unwrap_or_default();

    let com1 = if scenario.capture == Capture::SpcrUart {
        match fs::read(&com1_log) {
            Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
            Err(e) => {
                if verdict.is_ok() {
                    verdict = Err(format!("{}: {e}", com1_log.display()));
                }
                None
            }
        }
    } else {
        None
    };
    let done = Completed {
        serial: text.clone(),
        screen,
        com1,
    };
    let verdict = verdict
        .and_then(|()| match stack_window_lines(&done.serial).first() {
            Some(line) => Err(format!(
                "the serial log names an address in the kernel stacks' window, which no boot may \
                 print (ADR-0004 Amendment 2, A6.3): {line:?}"
            )),
            None => Ok(()),
        })
        .and_then(|()| scenario.check.map_or(Ok(()), |check| check(&done)))
        .and_then(
            |()| match scenario.stderr.iter().find(|m| !qemu_stderr.contains(**m)) {
                Some(m) => Err(format!("QEMU's stderr lacks {m:?}")),
                None => Ok(()),
            },
        );
    let log = work.join(format!(
        "boot-test-{}.log",
        scenario
            .label
            .replace(|c: char| !c.is_ascii_alphanumeric(), "-")
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

/// Asks QEMU's monitor for a screen dump and reads it. `switch`: the monitor shares stdio
/// with the serial port, so change over to it first (Ctrl-A, then C). QEMU writes the file
/// once, whole, so a complete PPM is a finished dump.
fn dump_screen(monitor: &mut ChildStdin, path: &Path, switch: bool) -> Result<screen::Image> {
    let _ = fs::remove_file(path);
    let shown = path
        .to_str()
        .ok_or_else(|| format!("path is not UTF-8: {}", path.display()))?;
    if switch {
        monitor
            .write_all(b"\x01c")
            .map_err(|e| format!("cannot reach the QEMU monitor: {e}"))?;
        thread::sleep(Duration::from_millis(250));
    }
    monitor
        .write_all(format!("screendump \"{shown}\"\n").as_bytes())
        .map_err(|e| format!("cannot reach the QEMU monitor: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(bytes) = fs::read(path)
            && let Ok(image) = screen::parse_ppm(&bytes)
        {
            return Ok(image);
        }
        if Instant::now() > deadline {
            return Err(format!("{} never became a screen dump", path.display()));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// [`Capture::Screen`]: the display is all there is, so a poll of screen dumps watches for
/// the markers, in order, in the rows on the screen, and for any failure message among the
/// rows visible when it looks.
fn wait_for_screen(
    monitor: &mut ChildStdin,
    path: &Path,
    markers: &[&str],
    failures: &[&'static str],
    deadline: Instant,
) -> Result {
    let mut rows = Vec::new();
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1500));
        let image = dump_screen(monitor, path, false)?;
        // Until the kernel draws, the screen holds no console rows.
        rows = screen::screen_text(&image).unwrap_or_default();
        let text = rows.join("\n");
        if let Some(m) = failures.iter().find(|m| text.contains(**m)) {
            return Err(format!("the screen shows {m:?}"));
        }
        if serial::contains_in_order(&text, markers) {
            return Ok(());
        }
    }
    Err(format!(
        "the screen never showed the markers; it showed:\n{}",
        rows.join("\n")
    ))
}

fn run() -> Result {
    check_environment()?;
    let krn_path = pack_kernel(&build_kernel()?)?;
    let krn = fs::read(&krn_path).map_err(|e| format!("{}: {e}", krn_path.display()))?;
    let staged = stage(&krn, Loader::Developer)?;
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

#[cfg(test)]
mod dependency_tests {
    use super::{
        APPROVED_THIRD_PARTY, LockedPackage, dependency_check, locked_packages, loom,
        manifest_problems, model_lock_problems, root_lock_problems,
    };

    const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";

    #[test]
    fn a_lock_file_is_read_package_by_package() {
        let lock = r#"version = 4

[[package]]
name = "origin-sync"
version = "0.0.0"
dependencies = [
 "loom",
]

[[package]]
name = "cfg-if"
version = "1.0.5"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "4e7648175b45a9a48536d676f68d918270699102aa8dab5496df06904c914600"
"#;
        assert_eq!(
            locked_packages(lock),
            vec![
                LockedPackage {
                    name: "origin-sync".to_string(),
                    version: "0.0.0".to_string(),
                    source: None,
                    checksum: None,
                },
                LockedPackage {
                    name: "cfg-if".to_string(),
                    version: "1.0.5".to_string(),
                    source: Some(REGISTRY.to_string()),
                    checksum: Some(
                        "4e7648175b45a9a48536d676f68d918270699102aa8dab5496df06904c914600"
                            .to_string()
                    ),
                },
            ]
        );
        assert!(locked_packages("version = 4\n").is_empty());
    }

    #[test]
    fn the_root_lock_may_hold_workspace_packages_only() {
        let own = "version = 4\n\n[[package]]\nname = \"origin-sync\"\nversion = \"0.0.0\"\n";
        assert!(root_lock_problems(own).is_empty());

        let registry = format!(
            "{own}\n[[package]]\nname = \"loom\"\nversion = \"0.7.2\"\nsource = \"{REGISTRY}\"\n"
        );
        let problems = root_lock_problems(&registry);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].starts_with("loom 0.7.2"), "{problems:?}");

        let checksum =
            format!("{own}\n[[package]]\nname = \"x\"\nversion = \"1.0.0\"\nchecksum = \"00\"\n");
        assert_eq!(root_lock_problems(&checksum).len(), 1);

        let patch = format!("{own}\n[[patch.unused]]\nname = \"x\"\nversion = \"1.0.0\"\n");
        assert_eq!(root_lock_problems(&patch), ["the table [[patch.unused]]"]);
    }

    #[test]
    fn a_path_dependency_must_be_a_member_of_the_workspace() {
        let member = r#""manifest_path":"/repo/framekernel/Cargo.toml""#;
        let sync = r#""manifest_path":"/repo/framekernel/sync/Cargo.toml""#;
        let inside = format!(
            r#"{{"packages":[{{"source":null,"dependencies":[{{"source":null,"path":"/repo/framekernel/sync"}}],{member}}},{{"source":null,"dependencies":[],{sync}}}]}}"#
        );
        assert_eq!(manifest_problems(&inside).unwrap(), Vec::<String>::new());

        for outside in ["/elsewhere/sync", "/repo/vendor/not-a-member", "/repo"] {
            let metadata = inside.replace("/repo/framekernel/sync\"}", &format!("{outside}\"}}"));
            let problems = manifest_problems(&metadata).unwrap();
            assert_eq!(
                problems,
                [format!(
                    "a path dependency outside the workspace: {outside}"
                )]
            );
        }

        // The manifest of a registry crate, however it is resolved.
        let registry = inside.replace(
            r#""source":null,"path":"/repo/framekernel/sync""#,
            r#""source":"registry+x""#,
        );
        assert_eq!(
            manifest_problems(&registry).unwrap(),
            ["a dependency from registry+x"]
        );

        assert!(manifest_problems(r#"{"path":"/repo/a\"b"}"#).is_err());
    }

    #[test]
    fn the_committed_model_lock_is_the_approved_one() {
        assert_eq!(model_lock_problems(loom::MODEL_LOCK), Vec::<String>::new());
    }

    #[test]
    fn any_change_to_the_model_lock_is_found() {
        let lock = loom::MODEL_LOCK;
        // A crate at another version, from another source, at another checksum, or new: each is
        // a change someone has to approve.
        let checksum = "419e0dc8046cb947daa77eb95ae174acfbddb7673b4151f56d1eed8e93fbfaca";
        assert!(lock.contains(checksum));
        for (from, to) in [
            (
                checksum,
                "0000000000000000000000000000000000000000000000000000000000000000",
            ),
            (
                "name = \"loom\"\nversion = \"0.7.2\"",
                "name = \"loom\"\nversion = \"0.7.1\"",
            ),
            (
                "name = \"loom\"\nversion = \"0.7.2\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"",
                "name = \"loom\"\nversion = \"0.7.2\"\nsource = \"git+https://example.invalid/loom#0\"",
            ),
        ] {
            assert!(lock.contains(from), "{from}");
            let changed = lock.replace(from, to);
            assert!(!model_lock_problems(&changed).is_empty(), "{to}");
        }
        // A crate the lock lost.
        let start = lock.find("[[package]]\nname = \"loom\"").unwrap();
        let end = lock[start + 1..].find("[[package]]").unwrap() + start + 1;
        let without = format!("{}{}", &lock[..start], &lock[end..]);
        let problems = model_lock_problems(&without);
        assert!(
            problems
                .iter()
                .any(|p| p.contains("loom 0.7.2") && p.contains("missing")),
            "{problems:?}"
        );
        // An unapproved crate, and a package of unknown origin with no source.
        for extra in [
            format!(
                "\n[[package]]\nname = \"new\"\nversion = \"1.0.0\"\nsource = \"{REGISTRY}\"\nchecksum = \"ab\"\n"
            ),
            "\n[[package]]\nname = \"elsewhere\"\nversion = \"1.0.0\"\n".to_string(),
        ] {
            assert_eq!(
                model_lock_problems(&format!("{lock}{extra}")).len(),
                1,
                "{extra}"
            );
        }
    }

    #[test]
    fn the_approved_list_is_sorted_and_has_no_duplicates() {
        assert!(APPROVED_THIRD_PARTY.windows(2).all(|w| w[0] < w[1]));
    }

    /// The gate itself, on this repository's real `Cargo.lock` and `cargo metadata`.
    #[test]
    fn this_workspace_passes_its_dependency_check() {
        dependency_check().unwrap();
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// `body` as an executable shell script in a new directory under `target/xtask/`.
    fn script(name: &str, body: &str) -> PathBuf {
        let dir = target_dir().join("xtask").join("gate-tests").join(name);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nm");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn a_failing_nm_is_an_error_not_an_empty_list() {
        let obj = Path::new("whatever.o");
        let nm = script("failing-nm", "echo boom >&2\nexit 3");
        let err = nm_symbols(&nm, obj, "-u").unwrap_err();
        assert!(err.contains("failed") && err.contains("boom"), "{err}");
        // A successful run reports its symbols, by the last field of each line.
        let nm = script("working-nm", "echo '  U memcpy'\necho '0000 T _start'");
        assert_eq!(
            nm_symbols(&nm, obj, "-u").unwrap(),
            ["memcpy".to_string(), "_start".to_string()]
        );
    }

    #[test]
    fn no_symbols_at_all_is_an_error_except_for_generic_only_libraries() {
        let obj = Path::new("some.o");
        assert_eq!(require_symbols("onbf-format", obj, 3), Ok(()));
        let err = require_symbols("onbf-format", obj, 0).unwrap_err();
        assert!(err.contains("nothing was checked"), "{err}");
        assert_eq!(require_symbols("origin-sync", obj, 0), Ok(()));
    }

    /// Function `name` with `branches` branch lines of the given mnemonic and some noise.
    fn function(label: &str, mnemonic: &str, branches: usize) -> String {
        let mut text = format!("{label}:\n\t.cfi_startproc\n\tret\n");
        for i in 0..branches {
            text += &format!(".LBB0_{i}:\n\t{mnemonic}\t.LBB0_{i}\n");
        }
        text += "\t.cfi_endproc\n";
        text
    }

    /// The listed functions with their expected branch counts, as the compiler writes them.
    fn signing_path(arch_mnemonic: &str) -> String {
        ED25519_SIGNING_PATH
            .iter()
            .map(|&(_, suffix, count)| {
                function(
                    &format!("_RNvNtCsAB_{ED25519_SYMBOL}{suffix}"),
                    arch_mnemonic,
                    count,
                )
            })
            .collect()
    }

    #[test]
    fn the_branch_check_counts_each_listed_function_on_both_architectures() {
        for (arch, mnemonic) in [("aarch64", "b.ne"), ("x86_64", "jne")] {
            let counted = check_signing_path(&signing_path(mnemonic), arch).unwrap();
            assert_eq!(counted.len(), ED25519_SIGNING_PATH.len(), "{arch}");
        }
        // aarch64's other conditional branches count, and neither `b` nor `jmp` do.
        assert!(is_conditional_branch("\tcbnz\tx0, .L1", "aarch64"));
        assert!(is_conditional_branch("\ttbz\tx0, #3, .L1", "aarch64"));
        assert!(!is_conditional_branch("\tb\t.L1", "aarch64"));
        assert!(!is_conditional_branch("\tjmp\t.L1", "x86_64"));
        assert!(!is_conditional_branch("\tjmpq\t*%rax", "x86_64"));
        assert!(is_conditional_branch("\tjae\t.L1", "x86_64"));
    }

    #[test]
    fn the_branch_check_refuses_what_it_cannot_vouch_for() {
        let good = signing_path("b.ne");
        // A listed function missing from the assembly.
        let without_loop = good.replacen("13base_multiple", "13renamed_elsewhere", 1);
        let err = check_signing_path(&without_loop, "aarch64").unwrap_err();
        assert!(err.contains("ed25519::base_multiple is not in"), "{err}");
        // One more branch inside the secret scalar loop.
        let more = format!(
            "{}{}",
            function(
                &format!("_RNvNtCsAB_{ED25519_SYMBOL}13base_multiple"),
                "b.ne",
                2
            ),
            good.replacen("13base_multiple", "x", 1)
        );
        let err = check_signing_path(&more, "aarch64").unwrap_err();
        assert!(
            err.contains("base_multiple has 2 conditional branches"),
            "{err}"
        );
        // A new function in the module that nobody has looked at.
        let extra = good
            + &function(
                &format!("_RNvNtCsAB_{ED25519_SYMBOL}9leak_a_bit"),
                "b.ne",
                5,
            );
        let err = check_signing_path(&extra, "aarch64").unwrap_err();
        assert!(err.contains("outside ED25519_SIGNING_PATH"), "{err}");
        // An architecture it knows nothing about.
        assert!(check_signing_path(&signing_path("b.ne"), "riscv64").is_err());
    }

    #[test]
    fn only_a_label_followed_by_cfi_startproc_is_a_function() {
        let asm = "\t.globl\tdata\ndata:\n\t.quad\t1\n_foo:\n\t.cfi_startproc\n.LBB0_1:\n\tb.ne\t.LBB0_1\n\tret\n\t.cfi_endproc\n";
        let functions = assembly_functions(asm);
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].0, "_foo");
        assert_eq!(functions[0].1.len(), 4, "everything up to .cfi_endproc");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0004 Amendment 2, A6.3: a boot log names no address in the stack window, PML4 slots
    /// 384–447, in any case and anywhere in a line; its edges and the windows beside it are fine.
    #[test]
    fn a_boot_log_naming_a_stack_window_address_is_caught() {
        let log = "\
origin-kernel: loaded at 0xffffffff80600000 (physical 0x7e50000), span 0x13000 bytes\n\
origin-kernel: direct map at 0xffff800000000000 and 0xffffbfffffffffff\n\
origin-kernel: page protection: stack guard page at 0xffffc00000001000 faulted\n\
origin-kernel: framebuffer at 0xffffe00000000000\n\
origin-kernel: the last page 0xFFFFDFFFFFFFF000, in capitals\n\
origin-kernel: two numbers, 0x1000 then 0xffffd8a4c0000000, in a line\n\
origin-kernel: a long run 0xffffc00000000000123 is no address\n";
        assert_eq!(
            stack_window_lines(log),
            [
                "origin-kernel: page protection: stack guard page at 0xffffc00000001000 faulted",
                "origin-kernel: the last page 0xFFFFDFFFFFFFF000, in capitals",
                "origin-kernel: two numbers, 0x1000 then 0xffffd8a4c0000000, in a line",
            ]
        );
        assert!(stack_window_lines("origin-kernel: 0x").is_empty());
    }

    #[test]
    fn the_kernels_physical_base_is_read_from_its_boot_line() {
        let line = "origin-kernel: loaded at 0xffffffff80600000 (physical 0x7e50000), span 0x13000 bytes; direct map at 0xffff800000000000 (KASLR)\n";
        assert_eq!(kernel_physical_base(line), Some(0x7e5_0000));
        assert_eq!(
            kernel_physical_base("origin-kernel: loaded at somewhere\n"),
            None
        );
    }

    /// A hand-made ELF64: a null section, a symbol table and its string table.
    fn elf_with(symbols: &[(&str, u64)]) -> Vec<u8> {
        let mut strings = vec![0u8];
        let mut table = vec![0u8; 24];
        for (name, value) in symbols {
            let at = u32::try_from(strings.len()).unwrap();
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
            let mut symbol = [0u8; 24];
            symbol[..4].copy_from_slice(&at.to_le_bytes());
            symbol[8..16].copy_from_slice(&value.to_le_bytes());
            table.extend_from_slice(&symbol);
        }
        let symbols_at = 64u64;
        let strings_at = symbols_at + table.len() as u64;
        let sections_at = strings_at + strings.len() as u64;
        let mut elf = vec![0u8; 64];
        elf[..5].copy_from_slice(b"\x7fELF\x02");
        elf[0x28..0x30].copy_from_slice(&sections_at.to_le_bytes());
        elf[0x3A..0x3C].copy_from_slice(&64u16.to_le_bytes());
        elf[0x3C..0x3E].copy_from_slice(&3u16.to_le_bytes());
        elf.extend_from_slice(&table);
        elf.extend_from_slice(&strings);
        let section = |kind: u32, offset: u64, size: u64, link: u32| {
            let mut header = [0u8; 64];
            header[4..8].copy_from_slice(&kind.to_le_bytes());
            header[24..32].copy_from_slice(&offset.to_le_bytes());
            header[32..40].copy_from_slice(&size.to_le_bytes());
            header[40..44].copy_from_slice(&link.to_le_bytes());
            header
        };
        elf.extend_from_slice(&[0u8; 64]);
        elf.extend_from_slice(&section(2, symbols_at, table.len() as u64, 2));
        elf.extend_from_slice(&section(3, strings_at, strings.len() as u64, 0));
        elf
    }

    #[test]
    fn a_kernel_symbol_is_found_by_part_of_its_name() {
        let elf = elf_with(&[
            ("_ZN4core3fmt5write17h0123E", 0x1000),
            (
                "_ZN12origin_frame5stack17STACK_REGION_BASE17h4567E",
                0x13_6008,
            ),
        ]);
        assert_eq!(elf_symbol(&elf, "STACK_REGION_BASE"), Some(0x13_6008));
        assert_eq!(elf_symbol(&elf, "NOT_THERE"), None);
        assert_eq!(
            elf_symbol(&elf[..100], "STACK_REGION_BASE"),
            None,
            "cut short"
        );
        assert_eq!(elf_symbol(b"not an ELF", "STACK_REGION_BASE"), None);
    }

    /// What QEMU 11.1's monitor sends back for `xp /1gx`: its line editor's redraw of the
    /// command, then the answer, its address padded to 8 digits.
    #[test]
    fn the_monitors_answer_is_matched_by_its_address() {
        let answer =
            "\u{1b}[Dxp /1gx 0x1dcffd30\u{1b}[K\r\n1dcffd30: 0xffffd8a4c0000000\r\n(qemu) ";
        let line = answer
            .split('\n')
            .find_map(|line| xp_answer(line, 0x1dcf_fd30));
        assert_eq!(line, Some("ffffd8a4c0000000\r"));
        assert_eq!(
            xp_answer("000000001dcffd30: 0x10\r", 0x1dcf_fd30),
            Some("10\r")
        );
        assert_eq!(
            xp_answer("1dcffd38: 0x10\r", 0x1dcf_fd30),
            None,
            "another word"
        );
        assert_eq!(
            xp_answer("xp /1gx 0x1dcffd30", 0x1dcf_fd30),
            None,
            "the command"
        );
    }

    #[test]
    fn boots_must_place_the_stack_region_apart() {
        let three = "kernel boot\t0xffffc00000200000\nkernel boot\t0xffffd00000400000\nx2APIC\t0xffffd8a4c0000000\n";
        assert_eq!(distinct_stack_bases(three), Ok(3));
        let alike = "kernel boot\t0xffffc00000200000\nkernel boot\t0xffffc00000200000\n";
        assert!(distinct_stack_bases(alike).is_err());
        assert!(distinct_stack_bases("kernel boot\t0xffffc00000200000\n").is_err());
        assert!(distinct_stack_bases("").is_err());
        // So far, during the matrix: one boot, or none, is no failure; two alike are.
        assert_eq!(
            stack_bases_apart("kernel boot\t0xffffc00000200000\n"),
            Ok(1)
        );
        assert_eq!(stack_bases_apart(""), Ok(0));
        assert!(stack_bases_apart(alike).is_err());
    }

    #[test]
    fn a_fixed_stack_base_is_accepted_only_from_a_kernel_that_said_so() {
        use framekernel::paging::layout::{STACK_REGION, is_stack_region_base, stack_region_base};
        let drawn = stack_region_base(0x1234_5678_9abc_def0);
        assert_ne!(drawn, STACK_REGION);
        let read = |said_fixed, base| {
            stack_base_read(said_fixed, base, STACK_REGION, is_stack_region_base(base))
        };
        // A drawn base is recorded; the fixed base of a kernel that drew is refused.
        assert_eq!(read(false, drawn), Ok(Some(drawn)));
        assert!(read(false, STACK_REGION).is_err());
        // A base no draw can give: not 2 MiB-aligned, or outside the window.
        assert!(read(false, drawn + 0x1000).is_err());
        assert!(read(false, 0).is_err());
        // A kernel that said its region is at the fixed base: that base, not recorded, and no
        // other.
        assert_eq!(read(true, STACK_REGION), Ok(None));
        assert!(read(true, drawn).is_err());
    }

    fn proof(cpus: u64) -> String {
        let mut text = String::from("origin-kernel: SMP: up\n");
        for k in 0..CONSOLE_LINES {
            for id in 0..cpus {
                let letter = char::from(b'a' + (id % 26) as u8);
                text += &format!(
                    "origin-kernel: console: CPU {id} line {k} of {CONSOLE_LINES}: {}\n",
                    letter.to_string().repeat(CONSOLE_PAYLOAD)
                );
            }
        }
        text
    }

    #[test]
    fn a_scenario_filter_that_matches_nothing_is_an_error() {
        let scenarios = vec![
            (
                refusal_scenario("wrong key", Loader::Verifying, "x", &[]),
                vec![],
            ),
            (
                refusal_scenario("unsigned kernel", Loader::Verifying, "x", &[]),
                vec![],
            ),
        ];
        let all = select_scenarios(&scenarios, "").unwrap();
        assert_eq!(all.len(), 2);
        let some = select_scenarios(&scenarios, "wrong,nope").unwrap();
        assert_eq!(some.len(), 1);
        assert!(some[0].0.label.contains("wrong"));
        assert!(select_scenarios(&scenarios, "nope").is_err());
    }

    /// Every VT-d marker the AMD-Vi boot swaps exists, so none is silently left in place.
    #[test]
    fn the_amd_markers_replace_every_vt_d_one() {
        let m = machine();
        let mut s = boot_scenario_markers(&m);
        amd_iommu_markers(&mut s, &m);
        assert!(
            !s.markers
                .iter()
                .any(|m| m.starts_with("xtask: the VT-d marker"))
        );
        for vtd in [
            "DMAR",
            "VT-d",
            "context entry",
            "remappable handle",
            "reason 0x",
            "pci count Some(7)",
        ] {
            assert!(!s.markers.iter().any(|m| m.contains(vtd)), "{vtd}");
        }
        assert!(
            s.markers
                .iter()
                .any(|m| m.contains("00:05.0 1022:1419 class 08:06:00 IOMMU"))
        );
        assert_eq!(s.stderr, ["failed to remap interrupt from devid 0x10"]);
        // The I/O APIC's VT-d proof gives way to one line saying its pins stay masked (the
        // proof that runs before the IOMMU is up keeps its lines).
        let remapped: Vec<&String> = s
            .markers
            .iter()
            .filter(|m| {
                m.starts_with("origin-kernel: I/O APIC: remapped")
                    || m.starts_with("origin-kernel: I/O APIC: IRTE")
            })
            .collect();
        assert_eq!(remapped.len(), 1);
        assert!(remapped[0].contains("not under AMD-Vi yet"));
    }

    /// `ORIGIN_QEMU_IOMMU` selects a preset, or passes an `intel-iommu` device through (the
    /// CPU-index work's negative control X2 boots `intel-iommu,intremap=off`); any other
    /// device is refused rather than booted against the VT-d markers.
    #[test]
    fn the_iommu_variable_takes_presets_and_intel_iommu_devices() {
        assert_eq!(parse_iommu(None), Iommu::Intel);
        assert_eq!(parse_iommu(Some("")), Iommu::Intel);
        assert_eq!(parse_iommu(Some("intel")), Iommu::Intel);
        assert_eq!(parse_iommu(Some("amd")), Iommu::Amd);
        assert_eq!(
            Iommu::Intel.device_args(),
            ["-device", "intel-iommu,intremap=on,eim=on"]
        );
        let off = parse_iommu(Some("intel-iommu,intremap=off"));
        assert_eq!(off, Iommu::Device("intel-iommu,intremap=off".into()));
        assert_eq!(off.device_args(), ["-device", "intel-iommu,intremap=off"]);
        assert_eq!(check_iommu(&off), Ok(()));
        assert_eq!(check_iommu(&Iommu::Amd), Ok(()));
        assert!(check_iommu(&parse_iommu(Some("virtio-iommu-pci"))).is_err());
        assert!(check_iommu(&parse_iommu(Some("intel-iommu-x"))).is_err());
    }

    /// The expected topology lines, computed from the `-smp` fields alone: what QEMU's
    /// machine must decode to under each leaf.
    #[test]
    fn the_topology_markers_follow_the_machine() {
        let sparse = Machine {
            smp: String::new(),
            devices: Vec::new(),
            apic_ids: vec![0, 256, 412],
            listed: 150,
            highest: 420,
            geometry: Geometry {
                packages: 2,
                dies: 3,
                modules: 5,
                cores: 5,
                threads: 1,
            },
        };
        assert_eq!(sparse.geometry.ids().len(), 150);
        assert_eq!(sparse.geometry.ids().iter().max(), Some(&420));
        let intel = topology_markers(&sparse, true);
        assert_eq!(
            intel[0],
            "origin-kernel: topology: layout from leaf 1Fh: thread bits 0, core bits 3, module bits 3, die bits 2, package from bit 8"
        );
        assert_eq!(
            intel[1],
            "origin-kernel: topology: CPU 0 (APIC ID 0): package 0, die 0, module 0, core 0, thread 0"
        );
        assert_eq!(
            intel[3],
            "origin-kernel: topology: CPU 2 (APIC ID 412): package 1, die 2, module 3, core 4, thread 0"
        );
        assert_eq!(
            intel[4],
            "origin-kernel: topology: the MADT lists 150 processors: 2 packages, 6 dies, 30 modules, 150 cores, 150 threads"
        );
        assert_eq!(
            intel[7],
            "origin-kernel: topology: 3 logical, 3 cores, 2 packages (1 threads/core, 256 logical/package); consistent true"
        );
        // Leaf 0Bh on the same machine: no dies or modules are visible.
        let amd = topology_markers(&sparse, false);
        assert_eq!(
            amd[0],
            "origin-kernel: topology: layout from leaf 0Bh: thread bits 0, core bits 8, module bits 0, die bits 0, package from bit 8"
        );
        assert_eq!(
            amd[3],
            "origin-kernel: topology: CPU 2 (APIC ID 412): package 1, die 0, module 0, core 156, thread 0"
        );
        assert_eq!(
            amd[4],
            "origin-kernel: topology: the MADT lists 150 processors: 2 packages, 2 dies, 2 modules, 150 cores, 150 threads"
        );
        // A plain -smp 4 machine: one package of 4 single-thread cores.
        let plain = Machine {
            smp: String::new(),
            devices: Vec::new(),
            apic_ids: vec![0, 1, 2, 3],
            listed: 4,
            highest: 3,
            geometry: Geometry {
                packages: 1,
                dies: 1,
                modules: 1,
                cores: 4,
                threads: 1,
            },
        };
        let m = topology_markers(&plain, false);
        assert_eq!(
            m[0],
            "origin-kernel: topology: layout from leaf 0Bh: thread bits 0, core bits 2, module bits 0, die bits 0, package from bit 2"
        );
        assert_eq!(
            m[4],
            "origin-kernel: topology: CPU 3 (APIC ID 3): package 0, die 0, module 0, core 3, thread 0"
        );
        assert_eq!(
            m[5],
            "origin-kernel: topology: the MADT lists 4 processors: 1 packages, 1 dies, 1 modules, 4 cores, 4 threads"
        );
        assert_eq!(
            m[8],
            "origin-kernel: topology: 4 logical, 4 cores, 1 packages (1 threads/core, 4 logical/package); consistent true"
        );
        assert!(!plain.geometry.has_extended_levels());
        assert!(sparse.geometry.has_extended_levels());
    }

    #[test]
    fn the_accelerator_is_tcg_unless_kvm_is_asked_for() {
        assert_eq!(Accel::named(None), Ok(Accel::Tcg));
        assert_eq!(Accel::named(Some("")), Ok(Accel::Tcg));
        assert_eq!(Accel::named(Some("tcg")), Ok(Accel::Tcg));
        assert_eq!(Accel::named(Some("kvm")), Ok(Accel::Kvm));
        assert!(Accel::named(Some("hvf")).is_err());
        assert_eq!(Accel::Tcg.machine(), "q35");
        assert_eq!(Accel::Kvm.machine(), "q35,accel=kvm,kernel-irqchip=split");
    }

    #[test]
    fn the_guest_is_intel_when_its_model_or_under_kvm_its_host_says_so() {
        // A vendor in the model decides, on any host.
        for host in [false, true] {
            for accel in [Accel::Tcg, Accel::Kvm] {
                assert!(guest_is_intel("max,vendor=GenuineIntel", accel, host));
                assert!(!guest_is_intel("max,vendor=AuthenticAMD", accel, host));
            }
        }
        // TCG's `max` is AMD's, whatever runs it.
        assert!(!guest_is_intel("max", Accel::Tcg, true));
        assert!(!guest_is_intel("max", Accel::Tcg, false));
        // Under KVM, the host's.
        assert!(guest_is_intel("max", Accel::Kvm, true));
        assert!(!guest_is_intel("max", Accel::Kvm, false));
        assert!(guest_is_intel("host", Accel::Kvm, true));
    }

    /// As the kernel printed them on a real AMD EPYC (Actions run 42547942738).
    const KVM_NOT_PRESENT: &str = "origin-kernel: I/O APIC: IRTE 5 not present: 10 interrupts arrived in 100 ms; fault recorded: none; IRTE restored, 5 interrupts arrived on CPU 1 (APIC ID 1) again: true; consistent false: I/O APIC failed";
    const KVM_WRONG_SOURCE: &str = "origin-kernel: I/O APIC: IRTE 5 checking source ID 0xff01 instead of 0xff00: 10 interrupts arrived in 100 ms; fault recorded: none; IRTE restored, 5 interrupts arrived on CPU 1 (APIC ID 1) again: true; consistent false: I/O APIC failed";

    #[test]
    fn kvm_forgives_only_the_two_lines_qemu_cannot_pass() {
        assert!(kvm_forgives(KVM_NOT_PRESENT));
        assert!(kvm_forgives(KVM_WRONG_SOURCE));
        for line in [
            // The interrupts did not come back.
            KVM_NOT_PRESENT.replace("again: true", "again: false"),
            KVM_WRONG_SOURCE.replace("again: true", "again: false"),
            // A fault was recorded, so QEMU did act on the entry: not this limit.
            KVM_NOT_PRESENT.replace("fault recorded: none", "fault recorded: source ID 0x0010"),
            // Every other I/O APIC proof.
            "origin-kernel: I/O APIC: remapped: the entry masked, the PIT stopped and IRTE 5 released: false; 0 fault(s) recorded after: I/O APIC failed".to_string(),
            "origin-kernel: I/O APIC: remapped: 0 PIT interrupts arrived on CPU 1 (APIC ID 1) through IRTE 5 in 200 ms, 0 on other CPUs; fault recorded: none; consistent false: I/O APIC failed".to_string(),
        ] {
            assert!(!kvm_forgives(&line), "{line}");
        }
    }

    /// As the kernel printed it on an AMD EPYC 9V45 (Actions run 42555218264).
    const KVM_TSC_STEP: &str = "origin-kernel: clock: TSC consistent across 2 processors: 261609 ordered readings, 261083 handoffs between processors, 1 backwards (worst 16 ticks): clock failed";

    #[test]
    fn kvm_forgives_only_a_rare_tiny_step_back_in_the_tsc() {
        assert!(kvm_forgives(KVM_TSC_STEP));
        for line in [
            // At the limit.
            KVM_TSC_STEP.replace("worst 16 ticks", "worst 1000 ticks"),
            // A steady offset: backwards on far more than one handoff in a thousand.
            KVM_TSC_STEP.replace("1 backwards", "262 backwards"),
            // It failed for another reason: too few handoffs, or a thread that never ended.
            KVM_TSC_STEP.replace("1 backwards (worst 16", "0 backwards (worst 0"),
            // Another clock proof.
            "origin-kernel: clock: TSC rate 0: clock failed".to_string(),
        ] {
            assert!(!kvm_forgives(&line), "{line}");
        }
        // The most the ratio allows passes.
        assert!(kvm_forgives(
            &KVM_TSC_STEP.replace("1 backwards", "261 backwards")
        ));
    }

    /// As the kernel printed it on an AMD EPYC 9V45 (Actions run 42561135158).
    const KVM_TX_UNFILLED: &str = "origin-kernel: virtio-net TX: 25 frames through 4 descriptors (buffers allocated once), 25 completions reclaimed, 4 of 4 free at the end; the ring was full 0 times and senders slept 2 times, woken by the TX-completion interrupt 2 (of 22); consistent false: virtio-net failed";

    #[test]
    fn kvm_forgives_only_a_tx_ring_that_never_filled() {
        assert!(kvm_forgives(KVM_TX_UNFILLED));
        for line in [
            // Frames were lost or still in flight.
            KVM_TX_UNFILLED.replace("25 completions", "24 completions"),
            KVM_TX_UNFILLED.replace("4 of 4 free", "3 of 4 free"),
            // No more frames than descriptors went through, so nothing was reused.
            KVM_TX_UNFILLED
                .replace("25 frames", "4 frames")
                .replace("25 completions", "4 completions"),
            // Nobody slept, or the interrupt never came.
            KVM_TX_UNFILLED.replace("slept 2 times", "slept 0 times"),
            KVM_TX_UNFILLED.replace("interrupt 2 (of 22)", "interrupt 0 (of 22)"),
            KVM_TX_UNFILLED.replace("interrupt 2 (of 22)", "interrupt 2 (of 0)"),
            // The ring did fill, so it failed for another reason.
            KVM_TX_UNFILLED.replace("full 0 times", "full 1 times"),
        ] {
            assert!(!kvm_forgives(&line), "{line}");
        }
    }

    /// As the kernel printed it on a Xeon 8370C (Actions run 42561135174).
    const KVM_UNEVEN_SHARE: &str = "origin-kernel: scheduler: CPU 0: workers 0 and 1 counted 1600936 and 653292, 51 context switches: scheduler failed";

    #[test]
    fn kvm_forgives_only_a_moderately_uneven_share() {
        assert!(kvm_forgives(KVM_UNEVEN_SHARE));
        for line in [
            // One worker hardly ran, or never did.
            KVM_UNEVEN_SHARE.replace("653292", "400234"),
            KVM_UNEVEN_SHARE.replace("653292", "0"),
            // Too few rounds to mean anything.
            KVM_UNEVEN_SHARE.replace("51 context", "14 context"),
            // Other scheduler failures.
            "origin-kernel: workers did not exit: scheduler failed".to_string(),
            "origin-kernel: cannot spawn on CPU 1: scheduler failed".to_string(),
        ] {
            assert!(!kvm_forgives(&line), "{line}");
        }
    }

    #[test]
    fn number_before_reads_the_digits_right_before_the_word() {
        assert_eq!(number_before("a 12 b", " b"), Some(12));
        assert_eq!(number_before("12 b", " b"), Some(12));
        assert_eq!(number_before("a b", " b"), None);
        assert_eq!(number_before("a 1x b", " b"), None);
        assert_eq!(number_before("a 12", " b"), None);
    }

    #[test]
    fn only_kvm_leaves_those_lines_out_of_the_failure_search() {
        let known = format!("a\n{KVM_NOT_PRESENT}\n{KVM_WRONG_SOURCE}\nb\n");
        assert!(Accel::Tcg.judged(&known).contains(IOAPIC_FAILED));
        assert!(!Accel::Kvm.judged(&known).contains(IOAPIC_FAILED));
        let another = format!("{known}origin-kernel: I/O APIC: x{IOAPIC_FAILED}\n");
        assert!(Accel::Kvm.judged(&another).contains(IOAPIC_FAILED));

        let failures = boot_failures();
        assert!(
            Accel::Tcg
                .screen_failures(&failures)
                .contains(&IOAPIC_FAILED)
        );
        let on_kvm = Accel::Kvm.screen_failures(&failures);
        assert!(!on_kvm.contains(&IOAPIC_FAILED));
        for message in KVM_FORGIVEN_MESSAGES {
            assert!(failures.contains(&message), "{message}");
            assert!(!on_kvm.contains(&message), "{message}");
        }
        assert_eq!(on_kvm.len(), failures.len() - KVM_FORGIVEN_MESSAGES.len());
    }

    /// The KVM markers replace those two steps, and only those: they match what KVM prints and
    /// not what TCG prints, so the boot that proves the steps cannot be mistaken for this one.
    #[test]
    fn the_kvm_markers_replace_the_two_pin_steps() {
        let m = machine();
        let mut s = boot_scenario_markers(&m);
        let (before, target) = (s.markers.clone(), m.named(m.cpus().min(2) - 1));
        kvm_pin_markers(&mut s);
        assert!(
            !s.markers
                .iter()
                .any(|m| m.starts_with("xtask: the VT-d marker"))
        );
        // Six out, seven in.
        assert_eq!(s.markers.len(), before.len() + 1);
        let at = s
            .markers
            .iter()
            .position(|m| m == " not present: ")
            .unwrap();
        assert_eq!(s.markers[..at], before[..at]);
        assert_eq!(s.markers[at + 7..], before[at + 6..]);

        let window: Vec<&str> = s.markers[at..at + 7].iter().map(String::as_str).collect();
        let on_kvm = format!("{KVM_NOT_PRESENT}\n{KVM_WRONG_SOURCE}\n");
        assert!(serial::contains_in_order(&on_kvm, &window));
        let on_tcg = format!(
            "origin-kernel: I/O APIC: IRTE 5 not present: 0 interrupts arrived in 100 ms; fault recorded: source ID 0xffff, interrupt index 5, reason 0x22 (interrupt table entry not present), from a message without a requester ID (the DMAR names 0xff00); IRTE restored, 5 interrupts arrived on {target} again: true; consistent true\n"
        );
        assert!(!serial::contains_in_order(&on_tcg, &window));
    }

    /// The TX line a forgiven boot prints must not eat the `consistent true` of a later line,
    /// so its own is gone and the rest of the list still matches that boot's order.
    #[test]
    fn kvm_does_not_ask_the_unfilled_ring_line_to_be_consistent() {
        let m = machine();
        let mut s = boot_scenario_markers(&m);
        kvm_markers(&mut s, true);
        let at = s
            .markers
            .iter()
            .position(|m| m == "origin-kernel: virtio-net TX: ")
            .unwrap();
        assert_ne!(s.markers[at + 1], "; consistent true");
        let boot = format!("{KVM_TX_UNFILLED}\norigin-kernel: virtio-net ping: x; consistent true");
        let window = [
            s.markers[at].as_str(),
            "origin-kernel: virtio-net ping: ",
            "; consistent true",
        ];
        assert!(serial::contains_in_order(&boot, &window));

        // A list that has changed shape is reported, not silently accepted.
        let mut other = boot_scenario_markers(&m);
        let tx = other
            .markers
            .iter()
            .position(|m| m == "origin-kernel: virtio-net TX: ")
            .unwrap();
        other.markers[tx + 1] = "something else".to_string();
        kvm_markers(&mut other, true);
        assert_eq!(
            other.markers.last().map(String::as_str),
            Some("xtask: the virtio-net TX markers are not as expected")
        );
    }

    /// Under KVM, and only there, the even-shares line is not required: the proof prints it
    /// only when no processor's shares were uneven.
    #[test]
    fn kvm_does_not_wait_for_the_even_shares_line() {
        let m = machine();
        let plain = boot_scenario_markers(&m);
        assert!(
            plain
                .markers
                .iter()
                .any(|m| m.starts_with(EVEN_SHARES_LINE))
        );
        let per_cpu = "origin-kernel: scheduler: CPU 0: workers 0 and 1 counted ";
        assert!(plain.markers.iter().any(|m| m == per_cpu));

        for vt_d in [true, false] {
            let mut s = boot_scenario_markers(&m);
            if !vt_d {
                amd_iommu_markers(&mut s, &m);
            }
            kvm_markers(&mut s, vt_d);
            assert!(!s.markers.iter().any(|m| m.starts_with(EVEN_SHARES_LINE)));
            assert!(s.markers.iter().any(|m| m == per_cpu));
            assert!(!s.markers.iter().any(|m| m.starts_with("xtask: ")));
        }
    }

    /// A marker list without the proof's lines is reported, not silently left alone.
    #[test]
    fn the_kvm_markers_say_so_when_the_vt_d_lines_are_gone() {
        let m = machine();
        let mut s = boot_scenario_markers(&m);
        amd_iommu_markers(&mut s, &m);
        kvm_pin_markers(&mut s);
        assert!(
            s.markers
                .last()
                .is_some_and(|m| m.starts_with("xtask: the VT-d marker"))
        );
    }

    /// The table the boot proof hands QEMU means what the proof claims, by the kernel's own
    /// parser and the ACPI checks every table goes through.
    #[test]
    fn the_spcr_table_for_the_proof_parses_as_the_uart_it_names() {
        use framekernel::spcr::{self, Access, UartAddress};
        let table = spcr_table(0, gas_io(SPCR_PORT), 7);
        assert_eq!(framekernel::acpi::check_table(&table, b"SPCR"), Ok(()));
        let parsed = spcr::parse(&table).unwrap();
        assert_eq!(parsed.address, UartAddress::Port(SPCR_PORT));
        assert_eq!(parsed.baud, Some(115_200));
        assert_eq!(
            parsed.line_setup(0),
            framekernel::serial::LineSetup::Divisor(1)
        );

        // The table `boot-console` uses for "registers in RAM".
        let mut base = [0u8; 12];
        base[1] = 32;
        base[3] = 3;
        base[4..12].copy_from_slice(&0x0100_0000u64.to_le_bytes());
        let table = spcr_table(0x12, base, 7);
        assert_eq!(framekernel::acpi::check_table(&table, b"SPCR"), Ok(()));
        assert_eq!(
            spcr::parse(&table).unwrap().address,
            UartAddress::Mmio {
                base: 0x0100_0000,
                stride: 4,
                access: Access::Dword
            }
        );
        // The refused one, by name.
        assert_eq!(
            spcr::parse(&spcr_table(3, gas_io(SPCR_PORT), 7)),
            Err(spcr::SpcrError::Interface(3))
        );
    }

    fn long_lines() -> String {
        LONG_LINES
            .iter()
            .map(|prefix| format!("{prefix}{}\n", ".".repeat(LONG_LINE_FILL)))
            .collect()
    }

    fn unwaited(count: usize) -> String {
        (0..count)
            .map(|k| {
                format!(
                    "origin-kernel: console: a line that cannot wait: {k}: {}\n",
                    ".".repeat(LONG_LINE_FILL)
                )
            })
            .collect()
    }

    #[test]
    fn unwaited_lines_pass_only_if_all_are_there_once_and_whole_in_order() {
        assert_eq!(unwaited_lines_whole(&unwaited(40)), Ok(()));
        assert!(unwaited_lines_whole("").is_err());
        let good = unwaited(4);
        let lines: Vec<&str> = good.lines().collect();
        // Lost, twice, out of order, cut short.
        let lost = [lines[0], lines[2], lines[3]].join("\n");
        assert!(unwaited_lines_whole(&lost).is_err());
        let twice = [lines[0], lines[1], lines[1], lines[2]].join("\n");
        assert!(unwaited_lines_whole(&twice).is_err());
        let swapped = [lines[1], lines[0], lines[2]].join("\n");
        assert!(unwaited_lines_whole(&swapped).is_err());
        let short = good.replacen(&".".repeat(LONG_LINE_FILL), ".", 1);
        assert!(unwaited_lines_whole(&short).is_err());
    }

    #[test]
    fn long_lines_pass_only_if_every_one_is_whole() {
        assert_eq!(long_lines_whole(&long_lines()), Ok(()));
        // One whole line is missing, or there twice.
        let good = long_lines();
        let first = good.lines().next().unwrap();
        assert!(long_lines_whole(good.strip_prefix(first).unwrap()).is_err());
        assert!(long_lines_whole(&format!("{first}\n{good}")).is_err());
        // A dot lost, as a burst that overran the FIFO would.
        let short = good.replacen(&format!(": {}", ".".repeat(LONG_LINE_FILL)), ": .", 1);
        assert!(long_lines_whole(&short).is_err());
        // A report in the middle of a line.
        let mixed = good.replacen(
            &format!(": {}", ".".repeat(LONG_LINE_FILL)),
            &format!(
                ": {}origin-kernel: exception 3\n{}",
                ".".repeat(10),
                ".".repeat(LONG_LINE_FILL - 10)
            ),
            1,
        );
        assert!(long_lines_whole(&mixed).is_err());
    }

    #[test]
    fn intact_console_lines_pass_from_every_cpu() {
        assert_eq!(console_lines_intact(&proof(2), 2), Ok(()));
        assert_eq!(console_lines_intact(&proof(4), 4), Ok(()));
        assert!(
            console_lines_intact(&proof(2), 4).is_err(),
            "two CPUs missing"
        );
    }

    #[test]
    fn mixed_or_missing_lines_fail() {
        let good = proof(2);
        // Two processors' bytes mixed inside a line.
        let mixed = good.replacen("aaaaab", "aabaab", 1).replacen(
            "line 3 of 20: aaaaa",
            "line 3 of 20: aabaa",
            1,
        );
        assert!(console_lines_intact(&mixed, 2).is_err());
        // One line lost.
        let lost = good.replacen("origin-kernel: console: CPU 1 line 7", "x", 1);
        assert!(console_lines_intact(&lost, 2).is_err());
        // Two lines run together.
        let joined = good.replacen(
            "\norigin-kernel: console: CPU 0 line 5",
            "origin-kernel: console: CPU 0 line 5",
            1,
        );
        assert!(console_lines_intact(&joined, 2).is_err());
        // A line too long by one letter.
        let long = good.replacen(
            &"b".repeat(CONSOLE_PAYLOAD),
            &"b".repeat(CONSOLE_PAYLOAD + 1),
            1,
        );
        assert!(console_lines_intact(&long, 2).is_err());
    }
}
