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
  boot-matrix boot-test with 1, 2 and 4 processors, one after another
  boot-x2apic boot-test on a sparse topology: the boot processor at APIC ID 0 and a second
              processor at APIC ID 256, above what 8-bit APIC IDs reach (the MADT lists
              150 possible processors)
  run         boot under QEMU with the serial console on this terminal (Ctrl-A X quits)

environment:
  ORIGIN_OVMF_CODE, ORIGIN_OVMF_VARS  firmware code image and NVRAM template
                                      (default: the EDK2 files installed next to QEMU)
  ORIGIN_BOOT_TEST_TIMEOUT            boot-test timeout in seconds (default 120)
  ORIGIN_QEMU_SMP                     processors QEMU emulates, 1-64 (default 2)
  ORIGIN_QEMU_TOPOLOGY                x2apic: the boot-x2apic machine (ORIGIN_QEMU_SMP is
                                      then ignored)
  ORIGIN_QEMU_CPU                     the QEMU CPU model (default max), e.g. max,-avx2
  ORIGIN_QEMU_IOMMU                   the IOMMU device (default
                                      intel-iommu,intremap=on,eim=on), e.g.
                                      intel-iommu,intremap=off for a negative control
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

/// The IOMMU QEMU emulates: `ORIGIN_QEMU_IOMMU` (a `-device` value, for negative
/// controls), or Intel VT-d with interrupt remapping and 32-bit x2APIC destinations.
fn qemu_iommu() -> String {
    std::env::var("ORIGIN_QEMU_IOMMU")
        .ok()
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "intel-iommu,intremap=on,eim=on".into())
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
/// ID 256; 150 processors are possible, and only the boot processor and one cold-plugged
/// at package 1 (APIC ID 256) are present (docs/research/2026-11-08-x2apic-above-255.md).
fn machine() -> Machine {
    if std::env::var("ORIGIN_QEMU_TOPOLOGY").as_deref() == Ok("x2apic") {
        // The cold-plugged processor is of the boot processor's model: QEMU names its type
        // after the model (`max` -> `max-x86_64-cpu`) and applies `-cpu`'s features to it.
        let model = qemu_cpu_model();
        let base = model.split(',').next().unwrap_or("max");
        return Machine {
            smp: "1,maxcpus=150,sockets=2,dies=3,modules=5,cores=5,threads=1".into(),
            devices: vec![format!(
                "{base}-x86_64-cpu,socket-id=1,die-id=0,module-id=0,core-id=0,thread-id=0"
            )],
            apic_ids: vec![0, 256],
            listed: 150,
            highest: 420,
        };
    }
    let cpus = qemu_cpus();
    Machine {
        smp: cpus.to_string(),
        devices: Vec::new(),
        apic_ids: (0..cpus).collect(),
        listed: cpus,
        highest: cpus - 1,
    }
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
            // CPU topology from CPUID leaf 0Bh: the online processors decompose consistently.
            "origin-kernel: topology: ".into(),
            "; consistent true".into(),
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
            "origin-kernel: user mode: two compute-bound user threads at once finished in ".into(),
            "reports 0b110; kernel stack follows every switch".into(),
            "origin-kernel: user mode: syscall instruction: reported Some(77), exited with Some(0)".into(),
            "origin-kernel: user mode: write: hello from ring 3".into(),
            "origin-kernel: user mode: copy from user: valid buffer -> 17, kernel address -> EFAULT, unmapped user address -> EFAULT; kernel carries on".into(),
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
        ]
        .into_iter()
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
        check: None,
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

/// Runs `boot-test` in a child process for each processor count, so each gets its own
/// `ORIGIN_QEMU_SMP` (the environment of this process is never changed).
fn boot_matrix() -> Result {
    let exe = env::current_exe().map_err(|e| format!("cannot find this xtask: {e}"))?;
    for cpus in [1, 2, 4] {
        println!("xtask: boot-matrix: {cpus} processor(s)");
        let status = Command::new(&exe)
            .arg("boot-test")
            .env("ORIGIN_QEMU_SMP", cpus.to_string())
            .status()
            .map_err(|e| format!("cannot run boot-test: {e}"))?;
        if !status.success() {
            return Err(format!(
                "boot-test with {cpus} processor(s) failed ({status})"
            ));
        }
    }
    println!("xtask: boot-matrix passed (1, 2 and 4 processors)");
    Ok(())
}

/// Runs `boot-test` in a child process on the sparse machine (`ORIGIN_QEMU_TOPOLOGY=x2apic`,
/// see `machine`): a processor at APIC ID 256 must come online, run threads, and take the
/// NVMe interrupts routed to it through interrupt remapping.
fn boot_x2apic() -> Result {
    let exe = env::current_exe().map_err(|e| format!("cannot find this xtask: {e}"))?;
    println!("xtask: boot-x2apic: processors at APIC IDs 0 and 256 of 150 possible");
    let status = Command::new(&exe)
        .arg("boot-test")
        .env("ORIGIN_QEMU_TOPOLOGY", "x2apic")
        .status()
        .map_err(|e| format!("cannot run boot-test: {e}"))?;
    if !status.success() {
        return Err(format!(
            "boot-test with a processor at APIC ID 256 failed ({status})"
        ));
    }
    println!("xtask: boot-x2apic passed (a processor at APIC ID 256)");
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
        // Intel VT-d (CLAUDE.md §32, §58; ADR-0007): DMA and interrupt remapping. It comes
        // before the PCI devices it translates. `eim=on` gives IRTEs 32-bit x2APIC
        // destinations (`auto` resolves to off without KVM's in-kernel irqchip); TCG needs no
        // irqchip option. See docs/research/2026-11-08-vt-d-iommu.md.
        .args(["-device", &qemu_iommu()])
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
    let verdict = verdict.and_then(|()| scenario.check.map_or(Ok(()), |check| check(&text)));
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
