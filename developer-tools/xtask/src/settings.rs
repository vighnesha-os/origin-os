//! One boot's settings, as a typed value (TASKS item 61; ADR-0011 Amendment 4, T1's notes;
//! decision items 300 and 310 to 312).
//!
//! `boot-test`, `boot-console`, `run`, `boot-matrix` and `boot-matrix`'s scenario commands read
//! the person's environment once, at the command's entry, into a [`Settings`]
//! ([`Settings::from_env`]). A scenario command then changes that value in code for each machine
//! it boots, and everything below it takes the value: the machine's processors, the markers, the
//! ESP, QEMU's command line, the checker and its allowances (ADR-0013 Amendments 2 and 3), and
//! the whole-log checks. Nothing else in `xtask` reads these variables, and no child is given
//! one: before this task, `boot-matrix` and its scenario commands re-ran `xtask boot-test` with
//! each machine's settings in the child's environment, which decision items 300 and 310 to 312
//! allowed until now. A host test holds that only this file names the variables.

use std::collections::BTreeMap;
use std::env;
use std::time::Duration;

use crate::{
    Accel, DEFAULT_BOOT_TEST_TIMEOUT, Iommu, QEMU_NVME_PAIRS, Result, check_iommu, parse_iommu,
};

/// `tcg` (the default) or `kvm`: how QEMU runs the guest's instructions.
pub(crate) const ACCEL: &str = "ORIGIN_QEMU_ACCEL";
/// `intel` (the default), `amd`, or an `intel-iommu` `-device` value.
pub(crate) const IOMMU: &str = "ORIGIN_QEMU_IOMMU";
/// The plain machine's processors: a whole number, 1 or more (default 2).
pub(crate) const SMP: &str = "ORIGIN_QEMU_SMP";
/// `x2apic`, `sparse`, `many` or `listed`; unset or empty for the plain machine.
pub(crate) const TOPOLOGY: &str = "ORIGIN_QEMU_TOPOLOGY";
/// The QEMU CPU model (default `max`).
pub(crate) const CPU: &str = "ORIGIN_QEMU_CPU";
/// Extra `-machine` options, comma-separated.
pub(crate) const MACHINE: &str = "ORIGIN_QEMU_MACHINE";
/// The I/O queue pairs QEMU's NVMe controller allocates: 2 or more (default 64).
pub(crate) const NVME_PAIRS: &str = "ORIGIN_QEMU_NVME_PAIRS";
/// `on`: the NIC behind a PCIe root port.
pub(crate) const BRIDGE: &str = "ORIGIN_QEMU_BRIDGE";
/// `on`: firmware-reserved memory planted by the TEST ONLY application.
pub(crate) const RESERVED: &str = "ORIGIN_QEMU_RESERVED";
/// `on`: the ACPI machine's NFIT, HMAT and generic initiator.
pub(crate) const ACPI: &str = "ORIGIN_QEMU_ACPI";
/// Set (to anything): QEMU logs interrupts and resets.
pub(crate) const DEBUG: &str = "ORIGIN_QEMU_DEBUG";
/// A boot's timeout in whole seconds.
pub(crate) const TIMEOUT: &str = "ORIGIN_BOOT_TEST_TIMEOUT";
/// Comma-separated parts of the labels of the scenarios to run.
pub(crate) const ONLY: &str = "ORIGIN_BOOT_TEST_ONLY";

/// Every variable [`Settings::from_env`] reads.
pub(crate) const VARIABLES: [&str; 13] = [
    ACCEL, IOMMU, SMP, TOPOLOGY, CPU, MACHINE, NVME_PAIRS, BRIDGE, RESERVED, ACPI, DEBUG, TIMEOUT,
    ONLY,
];

/// The processors' layout QEMU builds ([`crate::machine`] describes each).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Topology {
    /// `-smp N`: one package of N single-thread cores.
    Plain,
    /// `boot-x2apic`'s machine: processors at APIC IDs 0, 256 and 412 of 150.
    X2apic,
    /// The gate's large sparse machine: 67 processors in two packages (decision item 294).
    Sparse,
    /// 288 processors in two packages, outside the gate.
    Many,
    /// The discovery machine: 2 processors of the 4096 the MADT lists.
    Listed,
}

impl Topology {
    /// The topology a value of [`TOPOLOGY`] names: unset or empty is the plain machine, and a
    /// name it does not know is refused, never booted as the plain machine.
    fn named(value: Option<&str>) -> Result<Topology> {
        match value.unwrap_or_default() {
            "" => Ok(Topology::Plain),
            "x2apic" => Ok(Topology::X2apic),
            "sparse" => Ok(Topology::Sparse),
            "many" => Ok(Topology::Many),
            "listed" => Ok(Topology::Listed),
            other => Err(format!(
                "{TOPOLOGY} must be x2apic, sparse, many or listed, or unset for the plain machine, not {other:?}"
            )),
        }
    }
}

/// One boot's settings: what QEMU emulates, how it runs, and which of `boot-test`'s scenarios
/// run with what timeout. [`Default`] is what an empty environment gives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    /// How QEMU runs the guest's instructions ([`ACCEL`]).
    pub(crate) accel: Accel,
    /// The IOMMU QEMU emulates ([`IOMMU`]).
    pub(crate) iommu: Iommu,
    /// The plain machine's processors ([`SMP`]); the other topologies fix their own.
    pub(crate) smp: u32,
    /// The processors' layout ([`TOPOLOGY`]).
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
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            accel: Accel::Tcg,
            iommu: Iommu::Intel,
            smp: 2,
            topology: Topology::Plain,
            cpu: "max".into(),
            machine: String::new(),
            nvme_pairs: QEMU_NVME_PAIRS,
            bridge: false,
            reserved: false,
            acpi: false,
            debug: false,
            timeout: None,
            only: String::new(),
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
                // Only whether it is set counts.
                Err(env::VarError::NotUnicode(_)) if name == DEBUG => {
                    vars.insert(name, String::new());
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
        let get = |name: &str| vars.get(name).map(String::as_str);
        // Unset or empty is the default.
        let set = |name: &str| get(name).filter(|v| !v.is_empty());
        let iommu = parse_iommu(get(IOMMU));
        check_iommu(&iommu)?;
        let defaults = Settings::default();
        Ok(Settings {
            accel: Accel::named(get(ACCEL))?,
            iommu,
            smp: match set(SMP) {
                None => defaults.smp,
                Some(v) => at_least(SMP, v, 1, "processors")?,
            },
            topology: Topology::named(get(TOPOLOGY))?,
            cpu: set(CPU).map_or(defaults.cpu, str::to_string),
            machine: get(MACHINE).unwrap_or_default().to_string(),
            nvme_pairs: match set(NVME_PAIRS) {
                None => defaults.nvme_pairs,
                Some(v) => at_least(NVME_PAIRS, v, 2, "I/O queue pairs")?,
            },
            bridge: switch(BRIDGE, get(BRIDGE))?,
            reserved: switch(RESERVED, get(RESERVED))?,
            acpi: switch(ACPI, get(ACPI))?,
            debug: get(DEBUG).is_some(),
            timeout: get(TIMEOUT)
                .map(|v| {
                    v.parse::<u64>()
                        .map(Duration::from_secs)
                        .map_err(|_| format!("{TIMEOUT} must be whole seconds, got {v:?}"))
                })
                .transpose()?,
            only: get(ONLY).unwrap_or_default().to_string(),
        })
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

/// A switch: `on`, or `off`, empty or unset.
fn switch(name: &str, value: Option<&str>) -> Result<bool> {
    match value.unwrap_or_default() {
        "" | "off" => Ok(false),
        "on" => Ok(true),
        other => Err(format!("{name} must be on or off, or unset, not {other:?}")),
    }
}

#[cfg(test)]
mod tests;
