//! The typed settings (TASKS item 61): each variable read as `boot-test` read it, a value it does
//! not take refused, and every machine `boot-matrix` and its scenario commands boot given the
//! settings that the environment `boot-matrix` gave its child before the task would give.

use super::*;
use crate::lex::{self, Kind};
use crate::{CPU_IDENTITIES, MachineRun, repo_root};

/// The settings `vars` give.
fn read(vars: &[(&str, &str)]) -> Result<Settings> {
    Settings::from_vars(&vars.iter().map(|&(k, v)| (k, v.to_string())).collect())
}

#[test]
fn an_empty_environment_gives_the_defaults() {
    let settings = read(&[]).unwrap();
    assert_eq!(settings, Settings::default());
    assert_eq!(settings.accel, Accel::Tcg);
    assert_eq!(settings.iommu, Iommu::Intel);
    assert_eq!((settings.smp, settings.topology), (2, Topology::Plain));
    assert_eq!(settings.cpu, "max");
    assert_eq!(settings.nvme_pairs, 64);
    assert_eq!(settings.boot_timeout(), Duration::from_secs(120));
    assert_eq!(settings.machine_options("q35"), "q35");
    // Empty is unset for every variable but the debug log, whose presence is all that counts.
    let empty: Vec<(&str, &str)> = VARIABLES
        .iter()
        .filter(|&&v| v != DEBUG && v != TIMEOUT)
        .map(|&v| (v, ""))
        .collect();
    assert_eq!(read(&empty).unwrap(), Settings::default());
    assert!(read(&[(DEBUG, "")]).unwrap().debug);
}

#[test]
fn each_variable_is_read_as_boot_test_read_it() {
    let s = read(&[
        (ACCEL, "kvm"),
        (IOMMU, "amd"),
        (SMP, "65"),
        (TOPOLOGY, "sparse"),
        (CPU, "max,-rdseed"),
        (MACHINE, "pit=off,i8042=off"),
        (NVME_PAIRS, "2"),
        (BRIDGE, "on"),
        (RESERVED, "on"),
        (ACPI, "on"),
        (DEBUG, "1"),
        (TIMEOUT, "600"),
        (ONLY, "kernel boot,wrong key"),
    ])
    .unwrap();
    assert_eq!(
        s,
        Settings {
            accel: Accel::Kvm,
            iommu: Iommu::Amd,
            smp: 65,
            topology: Topology::Sparse,
            cpu: "max,-rdseed".into(),
            machine: "pit=off,i8042=off".into(),
            nvme_pairs: 2,
            bridge: true,
            reserved: true,
            acpi: true,
            debug: true,
            timeout: Some(Duration::from_secs(600)),
            only: "kernel boot,wrong key".into(),
        }
    );
    assert_eq!(s.boot_timeout(), Duration::from_secs(600));
    assert_eq!(
        s.machine_options("q35,accel=kvm,kernel-irqchip=split"),
        "q35,accel=kvm,kernel-irqchip=split,pit=off,i8042=off"
    );
    assert!(s.machine_lacks("pit") && s.machine_lacks("i8042") && !s.machine_lacks("hpet"));
    assert!(s.cpu_lacks("rdseed") && !s.cpu_lacks("rdrand"));
    assert!(
        read(&[(CPU, "max,rdrand=off")])
            .unwrap()
            .cpu_lacks("rdrand")
    );
    for (name, topology) in [
        ("x2apic", Topology::X2apic),
        ("many", Topology::Many),
        ("listed", Topology::Listed),
    ] {
        assert_eq!(read(&[(TOPOLOGY, name)]).unwrap().topology, topology);
    }
    for off in ["", "off"] {
        assert!(!read(&[(BRIDGE, off)]).unwrap().bridge);
    }
    assert_eq!(
        read(&[(IOMMU, "intel-iommu,intremap=off")]).unwrap().iommu,
        Iommu::Device("intel-iommu,intremap=off".into())
    );
    assert_eq!(
        read(&[(TIMEOUT, "0")]).unwrap().timeout,
        Some(Duration::ZERO)
    );
}

/// Questions 1 and 2 of TASKS item 61's contract: a value a variable does not take is refused
/// before anything boots, never booted as a default.
#[test]
fn a_value_a_variable_does_not_take_is_refused() {
    for (name, value, says) in [
        (ACCEL, "hvf", "must be tcg or kvm"),
        (
            IOMMU,
            "virtio-iommu-pci",
            "must be intel, amd or an intel-iommu",
        ),
        (SMP, "abc", "1 or more"),
        (SMP, "0", "1 or more"),
        (SMP, "-1", "1 or more"),
        (TOPOLOGY, "sparce", "must be x2apic, sparse, many or listed"),
        (TOPOLOGY, "X2APIC", "must be x2apic, sparse, many or listed"),
        (NVME_PAIRS, "1", "2 or more"),
        (NVME_PAIRS, "lots", "2 or more"),
        (BRIDGE, "yes", "must be on or off"),
        (RESERVED, "ON", "must be on or off"),
        (ACPI, "1", "must be on or off"),
        (TIMEOUT, "", "whole seconds"),
        (TIMEOUT, "2m", "whole seconds"),
    ] {
        let e = read(&[(name, value)]).unwrap_err();
        assert!(
            e.contains(name) && e.contains(says),
            "{name}={value:?}: {e}"
        );
    }
}

/// The machines a command boots, each with the environment `boot-matrix` set for its child
/// before TASKS item 61, read from `developer-tools/xtask/src/main.rs` at `b983bf6c`; and the
/// line said before each and the start of its failure, as the parent printed them.
type Old<'a> = Vec<(Vec<(&'a str, String)>, String, String)>;

/// Whether each machine of `new` has the settings `from_vars` reads from the person's
/// environment `person` with the child's variables of `old` set over it, and says and fails as
/// before.
fn same_as_before(person: &[(&str, &str)], new: &[MachineRun], old: &Old<'_>) {
    assert_eq!(new.len(), old.len());
    for (run, (child, said, failed)) in new.iter().zip(old) {
        let mut vars: BTreeMap<&str, String> =
            person.iter().map(|&(k, v)| (k, v.to_string())).collect();
        vars.extend(child.iter().cloned());
        assert_eq!(run.settings, Settings::from_vars(&vars).unwrap(), "{said}");
        assert_eq!(&run.said, said);
        assert_eq!(&run.failed, failed);
    }
}

/// The child's timeout `boot-matrix` set for `cpus` processors: the person's, or 120 seconds and
/// two more for each processor past four.
fn old_timeout(person: &[(&str, &str)], cpus: u32) -> String {
    person.iter().find(|(k, _)| *k == TIMEOUT).map_or_else(
        || (120 + 2 * u64::from(cpus.saturating_sub(4))).to_string(),
        |(_, v)| (*v).to_string(),
    )
}

/// The environments of a person the comparison runs under: none; CI's real-CPU job; a person's
/// processor, timeout and filter; and the other variables, which each machine inherits unless it
/// sets its own.
const PEOPLE: [&[(&str, &str)]; 4] = [
    &[],
    &[(ACCEL, "kvm"), (TIMEOUT, "600")],
    &[(CPU, "max,-avx2"), (TIMEOUT, "300"), (ONLY, "kernel boot")],
    &[
        (SMP, "3"),
        (IOMMU, "amd"),
        (MACHINE, "hpet=off"),
        (NVME_PAIRS, "2"),
        (DEBUG, ""),
        (BRIDGE, "on"),
    ],
];

#[test]
fn every_machine_has_the_settings_its_child_s_environment_gave() {
    let v = |pairs: &[(&'static str, &str)]| -> Vec<(&'static str, String)> {
        pairs.iter().map(|&(k, v)| (k, v.to_string())).collect()
    };
    for person in PEOPLE {
        let base = read(person).unwrap();
        let counts = [1, 2, 3, 4, 65];
        let mut old: Old<'_> = Vec::new();
        for iommu in ["intel", "amd"] {
            for cpus in counts {
                old.push((
                    vec![
                        (IOMMU, iommu.into()),
                        (SMP, cpus.to_string()),
                        (TIMEOUT, old_timeout(person, cpus)),
                    ],
                    format!("xtask: boot-matrix: {iommu} IOMMU, {cpus} processor(s)"),
                    format!("boot-test with the {iommu} IOMMU and {cpus} processor(s)"),
                ));
            }
        }
        same_as_before(person, &crate::matrix_runs(&base, &counts), &old);

        let old = [("max,vendor=GenuineIntel", "leaf 1Fh"), ("max", "leaf 0Bh")]
            .map(|(model, leaf)| {
                (
                    v(&[(TOPOLOGY, "x2apic"), (CPU, model)]),
                    format!("xtask: boot-x2apic: processors at APIC IDs 0, 256 and 412 of 150 possible, -cpu {model} ({leaf})"),
                    format!("boot-test on the sparse machine with -cpu {model}"),
                )
            })
            .to_vec();
        same_as_before(person, &crate::x2apic_runs(&base), &old);

        let old = ["intel", "amd"]
            .map(|iommu| {
                (
                    v(&[(IOMMU, iommu), (BRIDGE, "on")]),
                    format!(
                        "xtask: boot-bridge: the {iommu} IOMMU, the NIC behind a PCIe root port"
                    ),
                    format!("boot-test with the NIC behind a bridge and the {iommu} IOMMU"),
                )
            })
            .to_vec();
        same_as_before(person, &crate::bridge_runs(&base), &old);

        let old = ["intel", "amd"]
            .map(|iommu| {
                (
                    v(&[(IOMMU, iommu), (RESERVED, "on"), (ONLY, "kernel boot")]),
                    format!(
                        "xtask: boot-reserved: the {iommu} IOMMU, firmware-reserved memory planted"
                    ),
                    format!("boot-test with reserved memory planted and the {iommu} IOMMU"),
                )
            })
            .to_vec();
        same_as_before(person, &crate::reserved_runs(&base), &old);

        let old = vec![(
            v(&[(MACHINE, "pit=off,i8042=off"), (ONLY, "kernel boot")]),
            "xtask: boot-legacy-free: no 8254 and no 8042".to_string(),
            "boot-test with no 8254 and no 8042".to_string(),
        )];
        same_as_before(person, &crate::legacy_free_runs(&base), &old);

        let old = vec![(
            v(&[
                (ACPI, "on"),
                (MACHINE, "nvdimm=on,hmat=on"),
                (SMP, "2"),
                (ONLY, "kernel boot"),
            ]),
            "xtask: boot-acpi: QEMU's NFIT, HMAT and generic initiator".to_string(),
            "boot-test on the ACPI machine".to_string(),
        )];
        same_as_before(person, &crate::acpi_runs(&base), &old);

        let old = [
            ("max,-rdseed", "RDRAND and no RDSEED"),
            ("max,-rdseed,-rdrand", "neither RDSEED nor RDRAND"),
        ]
        .map(|(model, what)| {
            (
                v(&[(CPU, model), (ONLY, "kernel boot")]),
                format!("xtask: boot-random: a processor with {what} (-cpu {model})"),
                format!("boot-test on a processor with {what}"),
            )
        })
        .to_vec();
        same_as_before(person, &crate::random_runs(&base), &old);

        let old = CPU_IDENTITIES
            .map(|(model, what, _)| {
                (
                    v(&[(CPU, model), (ONLY, "kernel boot")]),
                    format!("xtask: boot-identity: {what} (-cpu {model})"),
                    format!("boot-test as {what}"),
                )
            })
            .to_vec();
        same_as_before(person, &crate::identity_runs(&base), &old);

        let old = vec![(
            vec![
                (TOPOLOGY, "sparse".to_string()),
                (IOMMU, "intel".to_string()),
                (TIMEOUT, old_timeout(person, 67)),
            ],
            "xtask: boot-sparse: 67 processors in two packages, APIC IDs 0-63 and 256, 300 and 412 of 314 possible".to_string(),
            "boot-test on the 67-processor sparse machine".to_string(),
        )];
        same_as_before(person, &crate::sparse_runs(&base), &old);

        let old = vec![(
            vec![
                (TOPOLOGY, "many".to_string()),
                (IOMMU, "intel".to_string()),
                (TIMEOUT, old_timeout(person, 288)),
            ],
            "xtask: boot-many: 288 processors in two packages, APIC IDs 0-143 and 256-399"
                .to_string(),
            "boot-test on the large sparse machine".to_string(),
        )];
        same_as_before(person, &crate::many_runs(&base), &old);

        let old = vec![(
            v(&[(TOPOLOGY, "listed")]),
            "xtask: boot-listed: 2 processors enabled of 4096 the MADT lists".to_string(),
            "boot-test on the discovery machine".to_string(),
        )];
        same_as_before(person, &crate::listed_runs(&base), &old);
    }
}

/// Question 4 of TASKS item 61's contract: no file of `xtask` but this module holds, outside an
/// item under `#[cfg(test)]`, a string literal that is the name of a variable the settings read,
/// so none is read anywhere else, and no child is given one. A planted `.env(…)` in another file
/// is caught. A test may name one as an example (Loom's test of the variables it passes on does).
#[test]
fn only_this_module_names_the_variables() {
    let named = |text: &str| -> Vec<String> {
        let tokens = lex::tokens(text);
        let tests = crate::host_tools::test_items(&tokens);
        tokens
            .iter()
            .enumerate()
            .filter(|(i, _)| !tests.iter().any(|(start, end)| start <= i && i <= end))
            .filter_map(|(_, t)| match t.kind {
                Kind::Str { raw: true } => Some(t.text.to_string()),
                Kind::Str { raw: false } => lex::unescape(t.text),
                _ => None,
            })
            .filter(|value| VARIABLES.contains(&value.as_str()))
            .collect()
    };
    let dir = repo_root().join("developer-tools").join("xtask");
    let mut files = Vec::new();
    crate::collect_files(&dir, &|_| false, &|name| name.ends_with(".rs"), &mut files).unwrap();
    assert!(files.len() > 20, "{files:?}");
    for file in files {
        let relative = file.strip_prefix(&dir).unwrap();
        if relative.starts_with("src/settings.rs") || relative.starts_with("src/settings") {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(named(&text), Vec::<String>::new(), "{}", relative.display());
    }
    let planted = "fn x(c: &mut Command) { c.env(\"ORIGIN_QEMU_SMP\", \"65\"); }";
    assert_eq!(named(planted), ["ORIGIN_QEMU_SMP"]);
    // The names inside a longer text (the usage, a message) are not a variable read, and a
    // test's example is not either.
    assert!(named("const U: &str = \"ORIGIN_QEMU_SMP processors\";").is_empty());
    assert!(named("#[cfg(test)]\nmod t { const N: &str = \"ORIGIN_QEMU_SMP\"; }").is_empty());
}
