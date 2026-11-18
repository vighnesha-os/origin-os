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
//! with the variable named and the values it takes, before anything is built or booted.

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
/// Set to anything: QEMU logs interrupts and resets.
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
const VARIABLES: [&str; 17] = [
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
        Ok(Settings {
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
        })
    }

    /// Whether `boot-matrix` runs the machines of ADR-0008 Amendment 1, D7: always on a
    /// developer's machine; in CI (GitHub Actions) only with `ORIGIN_BOOT_MATRIX_ANY_COUNT=on`.
    /// D7's condition: none becomes required in CI until each one's boot has been measured on
    /// the development Mac, its share of the real-CPU job stated, and the coordinating session
    /// has accepted it.
    pub(crate) fn runs_any_count_machines(&self) -> bool {
        !self.github_actions || self.any_count
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

/// A switch: `on`, or `off` (or unset).
fn switch(name: &str, value: Option<&str>) -> Result<bool> {
    match value {
        None | Some("off") => Ok(false),
        Some("on") => Ok(true),
        Some(other) => Err(format!("{name} must be on or off, not {other:?}")),
    }
}

#[cfg(test)]
mod tests;
