//! Origin OS build orchestration (host only; CLAUDE.md §70, handoff task T4).
//!
//! Each Origin artifact is built for its own target, which Cargo alone cannot express on
//! stable Rust (per-package targets are unstable). This tool runs the right command for
//! each crate. It uses std only, no third-party crates. Run it through the alias in
//! `.cargo/config.toml`: `cargo xtask <command>`.
#![forbid(unsafe_code)]

mod disk;
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
  boot-matrix boot-test with each IOMMU (Intel VT-d, AMD-Vi) and 1, 2 and 4 processors,
              one after another
  boot-x2apic boot-test on a sparse topology: the boot processor at APIC ID 0 and processors
              at APIC IDs 256 and 412, above what 8-bit APIC IDs reach (the MADT lists
              150 possible processors); twice, on a CPU model with CPUID leaf 1Fh and on
              one without
  run         boot under QEMU with the serial console on this terminal (Ctrl-A X quits)

environment:
  ORIGIN_OVMF_CODE, ORIGIN_OVMF_VARS  firmware code image and NVRAM template
                                      (default: the EDK2 files installed next to QEMU)
  ORIGIN_BOOT_TEST_TIMEOUT            boot-test timeout in seconds (default 120)
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

/// One QEMU boot the test runs, and what its serial output must and must not contain.
struct Scenario {
    label: &'static str,
    /// Lines required, in this order.
    markers: Vec<String>,
    /// Output that fails the scenario as soon as it appears.
    failures: Vec<&'static str>,
    /// A check of the whole serial log once every marker has appeared.
    check: Option<fn(&str) -> Result>,
    /// Text QEMU itself must have printed on its stderr by then (what its device models
    /// report about the guest).
    stderr: Vec<&'static str>,
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
    scenario
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
        markers: vec![
            format!("{PRODUCT} {VERSION} UEFI loader (build {BUILD})"),
            "origin-boot: UEFI 2.".into(),
            "origin-boot: read \\EFI\\ORIGIN\\KERNEL.ONBF (".into(),
            "origin-boot: developer mode: unsigned kernel images are accepted".into(),
            "origin-boot: kernel verified: ".into(),
            "origin-boot: kernel placed at ".into(),
            "origin-boot: KASLR: kernel at 0xffffffff".into(),
            "origin-boot: page tables built: ".into(),
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
            // M5 groundwork: PCIe through the MCFG, the ECAM window uncached, q35's
            // functions listed with their BARs sized and restored.
            "origin-kernel: PCIe: ECAM for segment 0, buses 0-255, at 0x".into(),
            ": uncached true, 2 MiB pages".into(),
            "origin-kernel: PCIe: 00:00.0 8086:29c0 class 06:00:00 host bridge".into(),
            "origin-kernel: PCIe: 00:01.0 1234:1111 class 03:00:00 VGA-compatible display controller; BAR0 mem32 prefetchable 16 MiB at ".into(),
            "origin-kernel: PCIe: 00:02.0 1b36:0010 class 01:08:02 NVMe controller; BAR0 mem64 16 KiB at ".into(),
            "origin-kernel: PCIe: 00:1f.0 8086:2918 class 06:01:00 ISA bridge".into(),
            "origin-kernel: PCIe: 00:1f.2 8086:2922 class 01:06:01 SATA controller (AHCI); BAR4 I/O 32 B at ".into(),
            "; BAR5 mem32 4 KiB at ".into(),
            "functions on 1 buses; every BAR sized with decoding off and restored: true".into(),
            // Intel VT-d (CLAUDE.md §32, §58; ADR-0007): the DMAR QEMU builds for q35 with
            // `intremap=on` (one unit at 0xfed90000 whose scope lists every PCI function and the
            // I/O APIC), the unit's capabilities, then queued invalidation, interrupt remapping
            // and translation on before any driver binds.
            "origin-kernel: IOMMU: DMAR: host address width 48 bits, interrupt remapping true".into(),
            "origin-kernel: IOMMU: unit 0 at 0xfed90000 (segment 0, 4 KiB of registers): 7 PCI scope(s), I/O APIC 0 (source ID 0xff00)".into(),
            "origin-kernel: IOMMU: unit 0: VT-d 1.0; CAP 0x".into(),
            "queued invalidation true, interrupt remapping true, extended interrupt mode true".into(),
            "origin-kernel: IOMMU: unit 0: queued invalidation on (256 descriptors); empty root table at 0x".into(),
            "interrupt remapping on (256 entries, 32-bit x2APIC destinations, compatibility format blocked); translation on (GSTS 0x".into(),
            "every device's DMA faults until its driver attaches it; 0 stale fault(s) cleared".into(),
            // M5: the NVMe driver against the test disk `disk` writes (CLAUDE.md §33).
            "origin-kernel: NVMe: 00:02.0 1b36:0010, NVMe 1.".into(),
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
        ],
        check: Some(|text| console_lines_intact(text, self::machine().cpus() as usize)),
        stderr: vec![],
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
        check: None,
        stderr: vec![],
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
        ["build"] => build_loader().and_then(|loader| {
            let kernel = pack_kernel(&build_kernel()?)?;
            println!("{}\n{}", loader.display(), kernel.display());
            Ok(())
        }),
        ["boot-test"] => boot_test(),
        ["boot-matrix"] => boot_matrix(),
        ["boot-x2apic"] => boot_x2apic(),
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

/// Runs `boot-test` in a child process for each IOMMU and processor count, so each gets its
/// own `ORIGIN_QEMU_IOMMU` and `ORIGIN_QEMU_SMP` (the environment of this process is never
/// changed).
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
    println!("xtask: boot-matrix passed (Intel VT-d and AMD-Vi, each with 1, 2 and 4 processors)");
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
        .args(["-device", "nvme,serial=ORIGIN0001,drive=nvme0"])
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
        .args([
            "-device",
            "virtio-net-pci,netdev=net0,mac=52:54:00:aa:bb:cc,disable-legacy=on,iommu_platform=on",
        ])
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

fn boot_test() -> Result {
    check_environment()?;
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
    let verdict = verdict
        .and_then(|()| scenario.check.map_or(Ok(()), |check| check(&text)))
        .and_then(
            |()| match scenario.stderr.iter().find(|m| !qemu_stderr.contains(**m)) {
                Some(m) => Err(format!("QEMU's stderr lacks {m:?}")),
                None => Ok(()),
            },
        );
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
    check_environment()?;
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
