//! One boot's settings, as a typed value (TASKS item 61; ADR-0011 Amendment 4, T1's notes;
//! decision items 300 and 310 to 312; the coordinating session's rulings of 2026-11-19).
//!
//! `boot-test`, `boot-console`, `run`, `boot-matrix` and `boot-matrix`'s scenario commands read
//! the person's environment once, at the command's entry, into a [`Settings`]
//! ([`Settings::from_env`]). A scenario command then changes that value in code for each machine
//! it boots, and everything below it takes the value: the machine's processors, the markers, the
//! firmware and the ESP, QEMU's command line, the checker and its allowances (ADR-0013
//! Amendments 2 and 3), the whole-log checks, and which machines `boot-matrix` runs. Before this
//! task, `boot-matrix` and its scenario commands re-ran `xtask boot-test` with each machine's
//! settings in the child's environment, which decision items 300 and 310 to 312 allowed until
//! now.
//!
//! **Only this module names the variables.** Their names are private here, and this module
//! builds no command, so none can be handed to a child. A host test in `settings/tests.rs`
//! refuses, in every other `.rs` file of `xtask`, test modules included, a string literal that
//! begins `ORIGIN_QEMU_`, `ORIGIN_BOOT_` or `ORIGIN_OVMF_`. It is a tripwire, not a proof: it
//! reads tokens, so a name assembled from pieces split before its prefix ends
//! (`concat!("ORIGIN_QE", "MU_SMP")`), a prefix written with escapes (`"\x4FRIGIN_QEMU_"`), or a
//! name reached through another crate is not seen. `GITHUB_ACTIONS` is the runner's, not
//! Origin's; it is read here too, and the tripwire does not cover it.
//!
//! **Each value is checked as it is read** (the rulings on the contract's questions 1 and 2): an
//! empty value is an unset one, for every variable; a value a variable does not take is refused,
//! with the variable named and the values it takes, before anything is built or booted. And
//! `boot-matrix` and its scenario commands, which set each machine themselves, refuse a person's
//! topology, bridge, reserved-memory and ACPI machine, RAM and table shapes
//! ([`Settings::refuse_machine`], ruling R5; TASKS item 83).

use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::time::Duration;

use crate::{Accel, DEFAULT_BOOT_TEST_TIMEOUT, Iommu, QEMU_NVME_PAIRS, Result};

/// `tcg` (the default) or `kvm`: how QEMU runs the guest's instructions.
const ACCEL: &str = "ORIGIN_QEMU_ACCEL";
/// `intel` (the default), `amd`, or an `intel-iommu` `-device` value.
const IOMMU: &str = "ORIGIN_QEMU_IOMMU";
/// The plain machine's processors: a whole number, 1 or more (default 2). Refused with a
/// [`TOPOLOGY`], whose machine fixes its own.
const SMP: &str = "ORIGIN_QEMU_SMP";
/// `x2apic`, `sparse`, `many` or `listed`; unset for the plain machine.
const TOPOLOGY: &str = "ORIGIN_QEMU_TOPOLOGY";
/// The QEMU CPU model (default `max`).
const CPU: &str = "ORIGIN_QEMU_CPU";
/// Extra `-machine` options, comma-separated.
const MACHINE: &str = "ORIGIN_QEMU_MACHINE";
/// The I/O queue pairs QEMU's NVMe controller allocates: a whole number, 2 or more (default 64).
const NVME_PAIRS: &str = "ORIGIN_QEMU_NVME_PAIRS";
/// `on` or `off`: the NIC behind a PCIe root port.
const BRIDGE: &str = "ORIGIN_QEMU_BRIDGE";
/// `on` or `off`: firmware-reserved memory planted by the TEST ONLY application.
const RESERVED: &str = "ORIGIN_QEMU_RESERVED";
/// `on` or `off`: the ACPI machine's NFIT, HMAT and generic initiator.
const ACPI: &str = "ORIGIN_QEMU_ACPI";
/// The machine's RAM: `512M` (the default), a larger even number of MiB (`1024M`), or a whole
/// number of GiB from 1 to 64 (`8G`). TASKS item 83.
const MEMORY: &str = "ORIGIN_QEMU_MEMORY";
/// `on`, `refused` or `off`: the firmware tables shaped as rental B's board has them, by the
/// TEST ONLY application, with the console's UART at `0x2F8` (TASKS item 83); `refused` also puts
/// the RSDP's copy in RAM the kernel owns, so the loader reads the SPCR and the kernel's own
/// reading is refused (TASKS item 84). See [`Shape`].
const SHAPE: &str = "ORIGIN_QEMU_SHAPE";
/// On if set and not empty: QEMU logs interrupts and resets.
const DEBUG: &str = "ORIGIN_QEMU_DEBUG";
/// A boot's timeout in whole seconds.
const TIMEOUT: &str = "ORIGIN_BOOT_TEST_TIMEOUT";
/// Comma-separated parts of the labels of the scenarios to run.
const ONLY: &str = "ORIGIN_BOOT_TEST_ONLY";
/// `on` or `off`: whether `boot-matrix` runs ADR-0008 Amendment 1, D7's machines in CI.
const ANY_COUNT: &str = "ORIGIN_BOOT_MATRIX_ANY_COUNT";
/// The firmware's code image, with [`OVMF_VARS`]; neither for EDK2's beside QEMU.
const OVMF_CODE: &str = "ORIGIN_OVMF_CODE";
/// The firmware's NVRAM template, with [`OVMF_CODE`].
const OVMF_VARS: &str = "ORIGIN_OVMF_VARS";
/// `true` on a GitHub Actions runner (the runner's own variable).
const GITHUB_ACTIONS: &str = "GITHUB_ACTIONS";

/// Every variable [`Settings::from_env`] reads.
const VARIABLES: [&str; 19] = [
    ACCEL,
    IOMMU,
    SMP,
    TOPOLOGY,
    CPU,
    MACHINE,
    NVME_PAIRS,
    BRIDGE,
    RESERVED,
    ACPI,
    MEMORY,
    SHAPE,
    DEBUG,
    TIMEOUT,
    ONLY,
    ANY_COUNT,
    OVMF_CODE,
    OVMF_VARS,
    GITHUB_ACTIONS,
];

/// The processors' layout QEMU builds ([`crate::machine`] describes each).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Topology {
    /// `-smp N`: one package of N single-thread cores.
    Plain(u32),
    /// `boot-x2apic`'s machine: processors at APIC IDs 0, 256 and 412 of 150.
    X2apic,
    /// The gate's large sparse machine: 67 processors in two packages (decision item 294).
    Sparse,
    /// 288 processors in two packages, outside the gate.
    Many,
    /// The discovery machine: 2 processors of the 4096 the MADT lists.
    Listed,
}

/// A machine's RAM, in MiB (TASKS item 83): at least 512, and even, so its two NUMA nodes each
/// hold a whole number of MiB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Memory(u32);

/// 512 MiB: every machine's before TASKS item 83.
impl Default for Memory {
    fn default() -> Memory {
        Memory(512)
    }
}

impl Memory {
    /// `gib` GiB.
    pub(crate) fn gib(gib: u32) -> Memory {
        Memory(gib * 1024)
    }

    /// Its size in MiB.
    pub(crate) fn mib(self) -> u32 {
        self.0
    }

    /// The size QEMU's `-m` takes: whole GiB as `G`, else MiB as `M`.
    pub(crate) fn qemu(self) -> String {
        Memory::size(self.0)
    }

    /// One of the two NUMA nodes' memory backends, half of it, as QEMU takes a size.
    pub(crate) fn node(self) -> String {
        Memory::size(self.0 / 2)
    }

    fn size(mib: u32) -> String {
        if mib.is_multiple_of(1024) {
            format!("{}G", mib / 1024)
        } else {
            format!("{mib}M")
        }
    }
}

/// One boot's settings: what QEMU emulates, how it runs, which firmware it boots, which of
/// `boot-test`'s scenarios run with what timeout, and which machines `boot-matrix` runs.
/// [`Default`] is what an empty environment gives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    /// How QEMU runs the guest's instructions ([`ACCEL`]).
    pub(crate) accel: Accel,
    /// The IOMMU QEMU emulates ([`IOMMU`]).
    pub(crate) iommu: Iommu,
    /// The processors' layout, with the plain machine's count ([`TOPOLOGY`], [`SMP`]).
    pub(crate) topology: Topology,
    /// The QEMU CPU model ([`CPU`]).
    pub(crate) cpu: String,
    /// Extra `-machine` options, comma-separated, or empty ([`MACHINE`]).
    pub(crate) machine: String,
    /// The I/O queue pairs QEMU's NVMe controller allocates ([`NVME_PAIRS`]).
    pub(crate) nvme_pairs: u32,
    /// The NIC behind a PCIe root port ([`BRIDGE`], `boot-bridge`).
    pub(crate) bridge: bool,
    /// Firmware-reserved memory planted before the loader starts ([`RESERVED`],
    /// `boot-reserved`).
    pub(crate) reserved: bool,
    /// The ACPI machine's tables ([`ACPI`], `boot-acpi`).
    pub(crate) acpi: bool,
    /// The machine's RAM ([`MEMORY`]; `boot-large`, `boot-rented`).
    pub(crate) memory: Memory,
    /// The firmware tables shaped as rental B's board has them ([`SHAPE`], `boot-rented`).
    pub(crate) shape: Shape,
    /// QEMU's log of interrupts and resets ([`DEBUG`]).
    pub(crate) debug: bool,
    /// A boot's timeout set by the person or by `boot-matrix` ([`TIMEOUT`]); `None` for
    /// [`DEFAULT_BOOT_TEST_TIMEOUT`].
    pub(crate) timeout: Option<Duration>,
    /// The scenarios to run, by parts of their labels, or empty for all ([`ONLY`]).
    pub(crate) only: String,
    /// The firmware's code image and NVRAM template ([`OVMF_CODE`], [`OVMF_VARS`]), or `None`
    /// for EDK2's beside QEMU.
    pub(crate) ovmf: Option<(PathBuf, PathBuf)>,
    /// `boot-matrix` runs ADR-0008 Amendment 1, D7's machines in CI too ([`ANY_COUNT`]).
    pub(crate) any_count: bool,
    /// The run is a GitHub Actions job's ([`GITHUB_ACTIONS`] is `true`).
    pub(crate) github_actions: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            accel: Accel::Tcg,
            iommu: Iommu::Intel,
            topology: Topology::Plain(2),
            cpu: "max".into(),
            machine: String::new(),
            nvme_pairs: QEMU_NVME_PAIRS,
            bridge: false,
            reserved: false,
            acpi: false,
            memory: Memory::default(),
            shape: Shape::Off,
            debug: false,
            timeout: None,
            only: String::new(),
            ovmf: None,
            any_count: false,
            github_actions: false,
        }
    }
}

impl Settings {
    /// The settings the person's environment gives, read once at a command's entry. A value a
    /// variable does not take is refused, before anything is built or booted.
    pub(crate) fn from_env() -> Result<Settings> {
        let mut vars = BTreeMap::new();
        for name in VARIABLES {
            match env::var(name) {
                Ok(value) => {
                    vars.insert(name, value);
                }
                Err(env::VarError::NotPresent) => {}
                // Only whether it is set counts, and a value that is not UTF-8 is not empty.
                Err(env::VarError::NotUnicode(_)) if name == DEBUG => {
                    vars.insert(name, "(not UTF-8)".into());
                }
                Err(env::VarError::NotUnicode(_)) => {
                    return Err(format!("{name} is not UTF-8"));
                }
            }
        }
        Settings::from_vars(&vars)
    }

    /// The settings `vars` give, by variable name: [`Settings::from_env`]'s rules.
    pub(crate) fn from_vars(vars: &BTreeMap<&str, String>) -> Result<Settings> {
        // Empty is unset, for every variable.
        let set = |name: &str| vars.get(name).map(String::as_str).filter(|v| !v.is_empty());
        let defaults = Settings::default();
        let settings = Settings {
            accel: accel(set(ACCEL))?,
            iommu: iommu(set(IOMMU))?,
            topology: topology(set(TOPOLOGY), set(SMP))?,
            cpu: set(CPU).map_or(defaults.cpu, str::to_string),
            machine: set(MACHINE).unwrap_or_default().to_string(),
            nvme_pairs: match set(NVME_PAIRS) {
                None => defaults.nvme_pairs,
                Some(v) => at_least(NVME_PAIRS, v, 2, "I/O queue pairs")?,
            },
            bridge: switch(BRIDGE, set(BRIDGE))?,
            reserved: switch(RESERVED, set(RESERVED))?,
            acpi: switch(ACPI, set(ACPI))?,
            memory: match set(MEMORY) {
                None => Memory::default(),
                Some(v) => memory(v)?,
            },
            shape: shape(set(SHAPE))?,
            debug: set(DEBUG).is_some(),
            timeout: set(TIMEOUT)
                .map(|v| {
                    v.parse::<u64>().map(Duration::from_secs).map_err(|_| {
                        format!("{TIMEOUT} must be a whole number of seconds, not {v:?}")
                    })
                })
                .transpose()?,
            only: set(ONLY).unwrap_or_default().to_string(),
            ovmf: match (set(OVMF_CODE), set(OVMF_VARS)) {
                (Some(code), Some(vars)) => Some((PathBuf::from(code), PathBuf::from(vars))),
                (None, None) => None,
                _ => {
                    return Err(format!(
                        "set both {OVMF_CODE} and {OVMF_VARS}, or neither for the EDK2 files beside QEMU"
                    ));
                }
            },
            any_count: switch(ANY_COUNT, set(ANY_COUNT))?,
            github_actions: set(GITHUB_ACTIONS) == Some("true"),
        };
        // The rented board's machine has a whole-boot check of its own, which would replace the
        // reserved-memory machine's and is dropped by the machine without a generator: those
        // combinations are refused, not booted with less checked (the review's T83-9).
        let shaped = settings.shape.name();
        if settings.shape.is_on() && settings.reserved {
            return Err(format!(
                "{SHAPE}={shaped} and {RESERVED}=on are not booted together: each machine's whole-boot check would replace the other's"
            ));
        }
        if settings.shape.is_on() && settings.cpu_lacks("rdseed") && settings.cpu_lacks("rdrand") {
            return Err(format!(
                "{SHAPE}={shaped} is not booted on a processor with neither RDSEED nor RDRAND: that machine's markers drop the rented board's whole-boot check"
            ));
        }
        Ok(settings)
    }

    /// Whether `boot-matrix` runs the machines of ADR-0008 Amendment 1, D7: always on a
    /// developer's machine; in CI (GitHub Actions) only with `ORIGIN_BOOT_MATRIX_ANY_COUNT=on`.
    /// D7's condition: none becomes required in CI until each one's boot has been measured on
    /// the development Mac, its share of the real-CPU job stated, and the coordinating session
    /// has accepted it.
    pub(crate) fn runs_any_count_machines(&self) -> bool {
        !self.github_actions || self.any_count
    }

    /// The size of `boot-large`'s and `boot-rented`'s machines (TASKS item 83; the coordinating
    /// session's ruling Q1 of 2026-11-20): [`FULL_MEMORY_GIB`] on a developer's machine, 8 GiB in CI
    /// (GitHub Actions), where the runners have far less, as the any-count machines are kept
    /// ([`Settings::runs_any_count_machines`]).
    pub(crate) fn large_memory(&self) -> Memory {
        if self.github_actions {
            Memory::gib(8)
        } else {
            Memory::gib(FULL_MEMORY_GIB)
        }
    }

    /// Refuses the person's machine for `command`, one of `boot-matrix` and its scenario
    /// commands, which set each machine themselves (the coordinating session's ruling R5 of
    /// 2026-11-19). With the children of before TASKS item 61, `ORIGIN_QEMU_TOPOLOGY=sparse
    /// cargo xtask boot-matrix` booted the 67-processor machine under every "N processor(s)"
    /// label and passed. A named topology and the bridge, reserved-memory and ACPI machines are
    /// refused here; `boot-test`, `boot-console` and `run` take them. Every other setting
    /// carries over to each machine, as it did to each child.
    pub(crate) fn refuse_machine(&self, command: &str) -> Result {
        let mut set = Vec::new();
        if let Some(name) = self.topology.name() {
            set.push(format!("{TOPOLOGY}={name}"));
        }
        for (on, name) in [
            (self.bridge, BRIDGE),
            (self.reserved, RESERVED),
            (self.acpi, ACPI),
        ] {
            if on {
                set.push(format!("{name}=on"));
            }
        }
        if self.shape.is_on() {
            set.push(format!("{SHAPE}={}", self.shape.name()));
        }
        if self.memory != Memory::default() {
            set.push(format!("{MEMORY}={}", self.memory.qemu()));
        }
        if set.is_empty() {
            return Ok(());
        }
        Err(format!(
            "{command} sets each machine's topology, bridge, reserved memory, ACPI tables, RAM and table shapes itself, so {} is refused here; boot-test, boot-console and run take it",
            set.join(" and ")
        ))
    }

    /// A boot's timeout: the one set, or [`DEFAULT_BOOT_TEST_TIMEOUT`].
    pub(crate) fn boot_timeout(&self) -> Duration {
        self.timeout.unwrap_or(DEFAULT_BOOT_TEST_TIMEOUT)
    }

    /// QEMU's machine option: `base` (what the accelerator needs), with the extra options (for
    /// example `pit=off,i8042=off` for a machine with no 8254 or 8042).
    pub(crate) fn machine_options(&self, base: &str) -> String {
        if self.machine.is_empty() {
            base.to_string()
        } else {
            format!("{base},{}", self.machine)
        }
    }

    /// Whether the extra `-machine` options turn `device` off (`device=off`).
    pub(crate) fn machine_lacks(&self, device: &str) -> bool {
        let off = format!("{device}=off");
        self.machine.split(',').any(|o| o == off)
    }

    /// Whether the CPU model takes `feature` away from the processor (`-feature` or
    /// `feature=off`).
    pub(crate) fn cpu_lacks(&self, feature: &str) -> bool {
        let (minus, off) = (format!("-{feature}"), format!("{feature}=off"));
        self.cpu
            .split(',')
            .any(|option| option == minus || option == off)
    }
}

impl Topology {
    /// The name [`TOPOLOGY`] gives a named machine; `None` for the plain one.
    fn name(self) -> Option<&'static str> {
        match self {
            Topology::Plain(_) => None,
            Topology::X2apic => Some("x2apic"),
            Topology::Sparse => Some("sparse"),
            Topology::Many => Some("many"),
            Topology::Listed => Some("listed"),
        }
    }
}

/// Why a scenario filter (`ORIGIN_BOOT_TEST_ONLY`) that matches none of `scenarios` scenarios is
/// refused: a mistyped name must not report a passing run with nothing booted.
pub(crate) fn no_scenario_matches(only: &str, scenarios: usize) -> String {
    format!("{ONLY}={only:?} matches none of the {scenarios} scenarios")
}

/// The accelerator a set value of [`ACCEL`] names.
fn accel(value: Option<&str>) -> Result<Accel> {
    match value {
        None | Some("tcg") => Ok(Accel::Tcg),
        Some("kvm") => Ok(Accel::Kvm),
        Some(other) => Err(format!("{ACCEL} must be tcg or kvm, not {other:?}")),
    }
}

/// The IOMMU a set value of [`IOMMU`] names. A raw `-device` value is booted with the VT-d
/// markers, so it must be an `intel-iommu`: any other is refused rather than booted against
/// them.
fn iommu(value: Option<&str>) -> Result<Iommu> {
    match value {
        None | Some("intel") => Ok(Iommu::Intel),
        Some("amd") => Ok(Iommu::Amd),
        Some(device) if device == "intel-iommu" || device.starts_with("intel-iommu,") => {
            Ok(Iommu::Device(device.into()))
        }
        Some(other) => Err(format!(
            "{IOMMU} must be intel, amd or an intel-iommu -device value, not {other:?}"
        )),
    }
}

/// The topology set values of [`TOPOLOGY`] and [`SMP`] name. A named machine fixes its own
/// processors, so a count with it is refused rather than ignored; an unknown name is refused,
/// never booted as the plain machine (question 1 and its ruling).
fn topology(name: Option<&str>, smp: Option<&str>) -> Result<Topology> {
    let named = match name {
        None => {
            let cpus = smp.map_or(Ok(2), |v| at_least(SMP, v, 1, "processors"))?;
            return Ok(Topology::Plain(cpus));
        }
        Some("x2apic") => Topology::X2apic,
        Some("sparse") => Topology::Sparse,
        Some("many") => Topology::Many,
        Some("listed") => Topology::Listed,
        Some(other) => {
            return Err(format!(
                "{TOPOLOGY} must be x2apic, sparse, many or listed, or unset for the plain machine of {SMP} processors, not {other:?}"
            ));
        }
    };
    match smp {
        None => Ok(named),
        Some(count) => Err(format!(
            "{SMP}={count} is refused with {TOPOLOGY}={}: that machine fixes its own processors",
            name.unwrap_or_default()
        )),
    }
}

/// `value` of `name` as a whole number of at least `least` `what`, or why not.
fn at_least(name: &str, value: &str, least: u32, what: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .ok()
        .filter(|&n| n >= least)
        .ok_or_else(|| {
            format!("{name} must be a whole number of {what}, {least} or more, not {value:?}")
        })
}

/// The full size of `boot-large`'s and `boot-rented`'s machines on a developer's machine, in GiB
/// (TASKS item 83): settled by the trial the coordinating session's ruling asked for.
const FULL_MEMORY_GIB: u32 = 64;

/// The RAM a set value of [`MEMORY`] names.
fn memory(value: &str) -> Result<Memory> {
    // Digits only before the unit: no sign, no space, and a value ending in any other character,
    // multi-byte ones included, is refused, never split (the review's T83-10).
    let digits = |n: &str| {
        (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .then(|| n.parse::<u32>().ok())
            .flatten()
    };
    if let Some(n) = value.strip_suffix('G').and_then(digits)
        && (1..=64).contains(&n)
    {
        return Ok(Memory::gib(n));
    }
    if let Some(n) = value.strip_suffix('M').and_then(digits)
        && n >= 512
        && n.is_multiple_of(2)
        && n <= 64 * 1024
    {
        return Ok(Memory(n));
    }
    Err(format!(
        "{MEMORY} must be 512M or more MiB, even (1024M), or 1G to 64G, not {value:?}"
    ))
}

/// A switch: `on`, or `off` (or unset).
/// The firmware tables' shape on the rented board's machines ([`SHAPE`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Shape {
    /// Unshaped: every machine but `boot-rented`'s.
    #[default]
    Off,
    /// As rental B's board has them (TASKS item 83).
    Rented,
    /// As [`Shape::Rented`], with the RSDP's copy in RAM the kernel owns: the loader reads the
    /// SPCR under boot services, and the kernel's own reading is refused (TASKS item 84).
    KernelRefused,
}

impl Shape {
    /// Whether the tables are shaped at all.
    pub(crate) fn is_on(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// Its value of [`SHAPE`].
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Rented => "on",
            Self::KernelRefused => "refused",
        }
    }
}

fn shape(value: Option<&str>) -> Result<Shape> {
    match value {
        None | Some("off") => Ok(Shape::Off),
        Some("on") => Ok(Shape::Rented),
        Some("refused") => Ok(Shape::KernelRefused),
        Some(other) => Err(format!("{SHAPE} must be on, refused or off, not {other:?}")),
    }
}

fn switch(name: &str, value: Option<&str>) -> Result<bool> {
    match value {
        None | Some("off") => Ok(false),
        Some("on") => Ok(true),
        Some(other) => Err(format!("{name} must be on or off, not {other:?}")),
    }
}

#[cfg(test)]
mod tests;
