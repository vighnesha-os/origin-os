//! Origin OS build orchestration (host only; CLAUDE.md §70, handoff task T4).
//!
//! Each Origin artifact is built for its own target, which Cargo alone cannot express on
//! stable Rust (per-package targets are unstable). This tool runs the right command for
//! each crate. It uses std only, no third-party crates. Run it through the alias in
//! `.cargo/config.toml`: `cargo xtask <command>`.
#![forbid(unsafe_code)]

mod disk;
mod loom;
mod screen;
mod serial;

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
              one after another, then boot-x2apic, boot-bridge and boot-console
  boot-bridge boot-test with the NIC behind a PCIe root port (both IOMMUs): bus mastering is
              cleared on the bridge too and opened again above the device its driver attached
  boot-x2apic boot-test on a sparse topology: the boot processor at APIC ID 0 and processors
              at APIC IDs 256 and 412, above what 8-bit APIC IDs reach (the MADT lists
              150 possible processors); twice, on a CPU model with CPUID leaf 1Fh and on
              one without
  boot-console the kernel's console on machines without a 16550 at 0x3F8 (ADR-0010): a UART
              named by an ACPI SPCR table, a table that must be refused, no display, and no
              UART at all (with a framebuffer the only way to see the boot)
  run         boot under QEMU with the serial console on this terminal (Ctrl-A X quits)
  loom        the Loom models of origin-sync (SpinLock and the wait-queue protocol) and their
              negative controls, each applied to a scratch copy of the source; also part of
              check (CLAUDE.md §73)

environment:
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
  ORIGIN_QEMU_BRIDGE                  on: the NIC behind a PCIe root port (the boot-bridge
                                      machine)
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
/// The host-only signing code (CLAUDE.md §59): never linked into the OS.
const SIGN_PACKAGE: &str = "onbf-sign";
const FREESTANDING_LIBS: &[&str] = &[
    BOOT_INFO_PACKAGE,
    ONBF_FORMAT_PACKAGE,
    MERKLE_PACKAGE,
    CRYPTO_PACKAGE,
    "origin-sync",
    "onbf-verify",
    "origin-paging",
    "origin-abi",
];
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
    if qemu_bridge() {
        bridge_markers(&mut scenario);
    }
    scenario
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
            "origin-boot: kernel placed at ".into(),
            // The firmware's framebuffer, for the kernel's console (ADR-0010).
            format!(
                "origin-boot: framebuffer: {QEMU_DISPLAY} at 0x"
            ),
            "origin-boot: KASLR: kernel at 0xffffffff".into(),
            "origin-boot: page tables built: ".into(),
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
            "origin-kernel: exception 3 (#BP breakpoint) at rip ".into(),
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
            "origin-kernel: NMI taken on its own stack (IST2): frame at ".into(),
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
            // M3: every processor QEMU emulates (`-smp`, see `machine`) comes online. The
            // MADT lists every possible processor; each enabled one gets a CPU index.
            format!(
                "origin-kernel: ACPI MADT: {} processors, {cpus} enabled (largest APIC ID listed {})",
                machine.listed, machine.highest
            ),
            format!(
                "origin-kernel: SMP: {cpus} of {cpus} enabled processors online; trampoline identity pages removed true"
            ),
            "origin-kernel: reclaimed ".into(),
        ]
        .into_iter()
        // CPU topology from CPUID (leaf 1Fh, 0Bh): each processor, the layout, the MADT.
        .chain(topology_markers(
            machine,
            machine.geometry.has_extended_levels()
                && qemu_cpu_model().contains("vendor=GenuineIntel"),
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
            // Hardware entropy from RDSEED/RDRAND: a batch of draws, all distinct and non-zero.
            "origin-kernel: entropy: ".into(),
            "; consistent true".into(),
            "origin-kernel: page protection: stack guard page at ".into(),
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
            // Every CPU printed its lines at once; `console_lines_intact` checks each one.
            format!(
                "origin-kernel: console: {cpus} processors printed {CONSOLE_LINES} lines each at once"
            ),
            // What drawing a line costs, measured (CLAUDE.md rule 21).
            "origin-kernel: console: framebuffer: ".into(),
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
        .chain([
            "origin-kernel: scheduler: exited threads' stacks reaped on their own CPUs: heap ".into(),
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
            "origin-kernel: M1 entry complete; halting".into(),
        ])
        .collect(),
        failures: boot_failures(),
        check: Some(|done| {
            console_lines_intact(&done.serial, self::machine().cpus() as usize)?;
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
        ": topology failed",
        ": CPU baseline failed",
        ": numa failed",
        ": slit failed",
        ": entropy failed",
        ": initramfs failed",
        ": virtio-net failed",
        "supervisor isolation from user pages incomplete",
        "origin-kernel: cannot map the user probe page",
        ": user isolation failed",
        ": NMI stack failed",
        ": higher half failed",
        ": process isolation failed",
        ": clock failed",
        ": HPET failed",
        ": I/O APIC failed",
        ": PM timer failed",
        ": ipc failed",
        ": console failed",
        ": TLB failed",
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

/// The screen after a whole boot: exactly what the serial log says it must show (the
/// console writes one line at a time to both).
fn check_screen(done: &Completed) -> Result {
    let image = done.screen.as_ref().ok_or("no screen dump was taken")?;
    let start = done
        .serial
        .find("origin-kernel: console: framebuffer")
        .ok_or("the serial log has no framebuffer console line")?;
    let text = done.serial.get(start..).ok_or("serial log out of bounds")?;
    // The markers can be seen a moment before the serial port's last newline arrives; the
    // console drew that whole last line before any of it left through the UART.
    let mut text = text.to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    screen::check_frame(image, &text)
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
fn ipc_markers(boot: String, far: String) -> Vec<String> {
    let mut markers = vec![
        format!(
            "origin-kernel: ipc: ping-pong: two ring-3 processes on {boot} and {far} exchanged 1000 messages each way, every one checked in ring 3"
        ),
        "origin-kernel: ipc: ping-pong round trip: ".into(),
    ];
    markers.extend([
        "origin-kernel: ipc: bad handles: handle 0 -> BAD_HANDLE, never issued -> BAD_HANDLE, bits above 32 -> BAD_HANDLE, wait on a bad handle -> BAD_HANDLE, closed -> BAD_HANDLE, stale, slot reused -> BAD_HANDLE, another process's -> BAD_HANDLE, wait on another process's -> BAD_HANDLE".to_string(),
        "origin-kernel: ipc: too few rights: write without WRITE -> ACCESS_DENIED, read without READ -> ACCESS_DENIED, wait without WAIT -> ACCESS_DENIED, duplicate without DUPLICATE -> ACCESS_DENIED, send without TRANSFER -> ACCESS_DENIED".into(),
        "origin-kernel: ipc: rights only shrink: adding WRITE -> INVALID_ARGS, an undefined right -> INVALID_ARGS, dropping DUPLICATE -> OK, the copy cannot write -> ACCESS_DENIED, the copy can read -> SHOULD_WAIT".into(),
        "origin-kernel: ipc: bad pointers: write from the kernel -> FAULT, from unmapped -> FAULT, across the end of mapped -> FAULT, from address 0 -> FAULT, wrapping past the top -> FAULT, from the top page -> FAULT, read into the kernel -> FAULT, into unmapped -> FAULT, into code -> FAULT, into read-only memory -> FAULT, sizes in the kernel -> FAULT, create out to the kernel -> FAULT, create out to read-only -> FAULT, duplicate out to the kernel -> FAULT, duplicate out to read-only -> FAULT, wait out to read-only -> FAULT, wait out to the kernel -> FAULT, handle list in the kernel -> FAULT, zero length names no memory -> OK, the empty message arrived -> 0x0".into(),
        "origin-kernel: ipc: sizes and numbers: one byte too many -> INVALID_ARGS, one handle too many -> INVALID_ARGS, no signals -> INVALID_ARGS, an undefined signal -> INVALID_ARGS, a sizes structure too small -> INVALID_ARGS, an unknown call number -> NOT_SUPPORTED, an object type not here yet -> NOT_SUPPORTED".into(),
        "origin-kernel: ipc: queues: empty -> SHOULD_WAIT, nothing by deadline 0 -> TIMED_OUT, written -> OK, readable -> OK, observed -> 0x3, 4-byte buffer -> BUFFER_TOO_SMALL, it needs -> 0x10, still there -> OK, received -> 0x10".into(),
        "origin-kernel: ipc: a full queue: the write after the last -> SHOULD_WAIT, not writable -> TIMED_OUT, writable again after a read -> OK".into(),
        "origin-kernel: ipc: a closed peer: sees PEER_CLOSED -> OK, observed -> 0x5, write -> PEER_CLOSED, the message sent before the close -> OK, read past the close -> PEER_CLOSED, READABLE will never rise -> TIMED_OUT".into(),
        "origin-kernel: ipc: handle transfer: send -> OK, the sender's handle is gone -> BAD_HANDLE, received -> OK, with one handle -> 0x1, the moved handle writes -> OK, and the other end reads -> OK, it kept its rights -> OK".into(),
        "origin-kernel: ipc: transfers that would break the graph: an end through its own channel -> INVALID_ARGS, the peer end through its own channel -> INVALID_ARGS, the same handle twice -> INVALID_ARGS, an end with another handle -> INVALID_ARGS, that end, alone again -> OK, into the first channel -> OK, a cycle through two queues -> INVALID_ARGS, a stale handle in the list -> BAD_HANDLE".into(),
        "origin-kernel: ipc: every hostile call returned its status and the process exited by itself (".into(),
        "; user-copy faults recovered: 0".into(),
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
        "origin-kernel: ipc: a full-size message: sixty-four KiB -> OK, received -> OK, bytes -> 0x10000".into(),
        "origin-kernel: ipc: 64 KiB through a channel and back, byte for byte: true".into(),
        "origin-kernel: ipc: registers: every register but RAX, RCX and R11 survives a system call: true".into(),
        "origin-kernel: ipc: a process's system calls are accepted only from its pinned code range: every call refused, and it ended by its own fault".into(),
        "origin-kernel: ipc: a program that loaded a null GS was interrupted and made system calls, and the kernel used its own GS (reports [1, 0])".into(),
        "origin-kernel: ipc: a program with RFLAGS.AC set took interrupts and made a system call: the flag reached kernel code 0 times, and stayed set in ring 3: true".into(),
        "origin-kernel: ipc: a process without the developer calls gets NOT_SUPPORTED: true".into(),
        "origin-kernel: ipc: sidt from ring 3 with UMIP on: refused, the process ended by vector 13".into(),
        "origin-kernel: ipc: a fault inside a user copy comes back as a failed copy, not a dead kernel: 1 fault recovered".into(),
        "origin-kernel: ipc: every process's memory, handles, channels and queued messages are gone: heap ".into(),
        "; threads run before their state existed: 0".into(),
    ]);
    markers
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
        ["check"] => check(),
        ["build"] => build_loader(Loader::Developer).and_then(|loader| {
            let kernel = pack_kernel(&build_kernel()?)?;
            println!("{}\n{}", loader.display(), kernel.display());
            Ok(())
        }),
        ["boot-test"] => boot_test(),
        ["boot-matrix"] => boot_matrix(),
        ["boot-x2apic"] => boot_x2apic(),
        ["boot-console"] => boot_console(),
        ["boot-bridge"] => boot_bridge(),
        ["run"] => run(),
        ["loom"] => loom::run(),
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

/// Runs `boot-test` in a child process for each IOMMU and processor count, so each gets its
/// own `ORIGIN_QEMU_IOMMU` and `ORIGIN_QEMU_SMP` (the environment of this process is never
/// changed), and then [`boot_x2apic`] (the owner's decision on ADR-0008, question 3).
fn boot_matrix() -> Result {
    let exe = env::current_exe().map_err(|e| format!("cannot find this xtask: {e}"))?;
    for iommu in ["intel", "amd"] {
        for cpus in [1, 2, 4] {
            println!("xtask: boot-matrix: {iommu} IOMMU, {cpus} processor(s)");
            let status = Command::new(&exe)
                .arg("boot-test")
                .env("ORIGIN_QEMU_IOMMU", iommu)
                .env("ORIGIN_QEMU_SMP", cpus.to_string())
                .status()
                .map_err(|e| format!("cannot run boot-test: {e}"))?;
            if !status.success() {
                return Err(format!(
                    "boot-test with the {iommu} IOMMU and {cpus} processor(s) failed ({status})"
                ));
            }
        }
    }
    boot_x2apic()?;
    boot_bridge()?;
    boot_console()?;
    println!(
        "xtask: boot-matrix passed (Intel VT-d and AMD-Vi, each with 1, 2 and 4 processors, the sparse machine, the bridge machine, and the console machines)"
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
        "xtask: boot-console passed ({} of {} scenarios: the UART the SPCR names, COM1 named, refused tables, a UART that takes no bytes, no display, no UART)",
        chosen.len(),
        scenarios.len()
    );
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
        let status = Command::new(&exe)
            .arg("boot-test")
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
        let status = Command::new(&exe)
            .arg("boot-test")
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
        &["test", "--package", "xtask"],
        &["test", "--package", LOADER_PACKAGE, "--lib"],
        &["test", "--package", VERSION_PACKAGE],
        &["test", "--package", PACK_PACKAGE],
        &["test", "--package", SIGN_PACKAGE],
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
    build_kernel()?;
    no_panic_check()?;
    dependency_check()?;
    linux_cfg_lint()?;
    manifest_version_lint()?;
    signing_stays_off_the_os_lint()?;
    signing_has_no_secret_branches()?;
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

/// Builds the EFI application with `loader`'s feature and returns the path of
/// `origin-boot.efi`.
fn build_loader(loader: Loader) -> Result<PathBuf> {
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
        loader.feature(),
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
    c.args(["-machine", "q35", "-cpu"])
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
            &scenario.failures,
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
        }
    };
    let elapsed = started.elapsed();

    // What the markers were found in, before any monitor output joins the stream.
    let text = serial::clean(&raw);
    let mut screen = None;
    if verdict.is_ok() && scenario.capture == Capture::Com1AndScreen {
        match stdin.as_mut() {
            Some(monitor) => match dump_screen(monitor, &screen_path, true) {
                Ok(image) => screen = Some(image),
                Err(e) => verdict = Err(format!("no screen dump: {e}")),
            },
            None => verdict = Err("QEMU stdin was not captured".into()),
        }
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
