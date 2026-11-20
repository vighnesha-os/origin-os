//! The typed settings (TASKS item 61, and the coordinating session's rulings of 2026-11-19): each
//! variable read as `boot-test` read it, empty as unset, a value it does not take refused, every
//! machine `boot-matrix` and its scenario commands boot given the settings that the environment
//! `boot-matrix` gave its child before the task would give, and no other file naming a variable.

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
    assert_eq!(settings.topology, Topology::Plain(2));
    assert_eq!(settings.cpu, "max");
    assert_eq!(settings.nvme_pairs, 64);
    assert_eq!(settings.boot_timeout(), Duration::from_secs(120));
    assert_eq!(settings.machine_options("q35"), "q35");
    assert_eq!(settings.ovmf, None);
    assert!(settings.runs_any_count_machines());
    // Empty is unset, for every variable (the ruling on question 2): an empty timeout is the
    // default, where it was refused before TASKS item 61, and an empty debug log is off, where it
    // was on.
    let empty: Vec<(&str, &str)> = VARIABLES.iter().map(|&v| (v, "")).collect();
    assert_eq!(read(&empty).unwrap(), Settings::default());
    let timeout = read(&[(TIMEOUT, "")]).unwrap();
    assert_eq!(
        (timeout.timeout, timeout.boot_timeout()),
        (None, Duration::from_secs(120))
    );
    assert!(!read(&[(DEBUG, "")]).unwrap().debug);
    assert!(read(&[(DEBUG, "1")]).unwrap().debug);
}

#[test]
fn each_variable_is_read_as_boot_test_read_it() {
    let s = read(&[
        (ACCEL, "kvm"),
        (IOMMU, "amd"),
        (SMP, "65"),
        (CPU, "max,-rdseed"),
        (MACHINE, "pit=off,i8042=off"),
        (NVME_PAIRS, "2"),
        (BRIDGE, "on"),
        (RESERVED, "on"),
        (ACPI, "on"),
        (MEMORY, "8G"),
        (SHAPE, "on"),
        (DEBUG, "1"),
        (TIMEOUT, "600"),
        (ONLY, "kernel boot,wrong key"),
        (OVMF_CODE, "/firmware/code.fd"),
        (OVMF_VARS, "/firmware/vars.fd"),
        (ANY_COUNT, "on"),
        (GITHUB_ACTIONS, "true"),
    ])
    .unwrap();
    assert_eq!(
        s,
        Settings {
            accel: Accel::Kvm,
            iommu: Iommu::Amd,
            topology: Topology::Plain(65),
            cpu: "max,-rdseed".into(),
            machine: "pit=off,i8042=off".into(),
            nvme_pairs: 2,
            bridge: true,
            reserved: true,
            acpi: true,
            memory: Memory::gib(8),
            shape: true,
            debug: true,
            timeout: Some(Duration::from_secs(600)),
            only: "kernel boot,wrong key".into(),
            ovmf: Some(("/firmware/code.fd".into(), "/firmware/vars.fd".into())),
            any_count: true,
            github_actions: true,
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
        ("sparse", Topology::Sparse),
        ("many", Topology::Many),
        ("listed", Topology::Listed),
    ] {
        assert_eq!(read(&[(TOPOLOGY, name)]).unwrap().topology, topology);
    }
    // An empty topology is the plain machine, with or without a count (question 1).
    assert_eq!(
        read(&[(TOPOLOGY, ""), (SMP, "65")]).unwrap().topology,
        Topology::Plain(65)
    );
    for off in ["", "off"] {
        assert!(!read(&[(BRIDGE, off)]).unwrap().bridge);
        assert!(!read(&[(ANY_COUNT, off)]).unwrap().any_count);
    }
    assert_eq!(
        read(&[(TIMEOUT, "0")]).unwrap().timeout,
        Some(Duration::ZERO)
    );
}

/// `ORIGIN_QEMU_MEMORY` (TASKS item 83): 512 MiB unset; an even number of MiB from 512, or 1 to
/// 64 GiB; anything else refused. QEMU takes it as written, and each NUMA node half of it.
#[test]
fn the_memory_is_read_and_split_between_the_nodes() {
    assert_eq!(read(&[]).unwrap().memory, Memory::default());
    for (value, mib, qemu, node) in [
        ("512M", 512, "512M", "256M"),
        ("1024M", 1024, "1G", "512M"),
        ("1536M", 1536, "1536M", "768M"),
        ("1G", 1024, "1G", "512M"),
        ("8G", 8192, "8G", "4G"),
        ("64G", 65536, "64G", "32G"),
    ] {
        let memory = read(&[(MEMORY, value)]).unwrap().memory;
        assert_eq!(
            (memory.mib(), memory.qemu(), memory.node()),
            (mib, qemu.to_string(), node.to_string()),
            "{value}"
        );
    }
    for bad in [
        "256M", "513M", "0G", "65G", "8", "8g", "8GiB", "G", "-8G", "8.5G",
    ] {
        let e = read(&[(MEMORY, bad)]).unwrap_err();
        assert!(e.starts_with("ORIGIN_QEMU_MEMORY must be"), "{bad}: {e}");
    }
    assert!(read(&[(SHAPE, "on")]).unwrap().shape);
    assert!(read(&[(SHAPE, "ON")]).is_err());
}

/// The coordinating session's ruling Q1 on TASKS item 83: the large machines' full size on a
/// developer's machine, and 8 GiB in CI, whatever the any-count switch says.
#[test]
fn ci_boots_the_large_machines_at_8_gib() {
    assert_eq!(
        read(&[]).unwrap().large_memory(),
        Memory::gib(FULL_MEMORY_GIB)
    );
    let ci = read(&[(GITHUB_ACTIONS, "true")]).unwrap();
    assert_eq!(ci.large_memory(), Memory::gib(8));
    let ci_all = read(&[(GITHUB_ACTIONS, "true"), (ANY_COUNT, "on")]).unwrap();
    assert_eq!(ci_all.large_memory(), Memory::gib(8));
    assert!(
        read(&[]).unwrap().large_memory().mib() > 4096,
        "more than 4 GiB"
    );
}

/// What `ORIGIN_QEMU_ACCEL` and `ORIGIN_QEMU_IOMMU` select (these tests were `main.rs`'s before
/// TASKS item 61 moved the parsers here): a preset, or an `intel-iommu` device passed through
/// (the CPU-index work's negative control X2 boots `intel-iommu,intremap=off`); any other device
/// is refused rather than booted against the VT-d markers.
#[test]
fn the_accelerator_and_the_iommu_take_their_presets() {
    for (value, expected) in [("tcg", Accel::Tcg), ("kvm", Accel::Kvm)] {
        assert_eq!(read(&[(ACCEL, value)]).unwrap().accel, expected);
    }
    for (value, expected) in [
        ("intel", Iommu::Intel),
        ("amd", Iommu::Amd),
        ("intel-iommu", Iommu::Device("intel-iommu".into())),
        (
            "intel-iommu,intremap=off",
            Iommu::Device("intel-iommu,intremap=off".into()),
        ),
    ] {
        assert_eq!(read(&[(IOMMU, value)]).unwrap().iommu, expected);
    }
}

/// Which machines `boot-matrix` runs: all of them on a developer's machine; in CI only with
/// `ORIGIN_BOOT_MATRIX_ANY_COUNT=on` (ADR-0008 Amendment 1, D7). Only `true` is the runner's.
#[test]
fn ci_runs_the_any_count_machines_only_when_asked() {
    let runs = |vars: &[(&str, &str)]| read(vars).unwrap().runs_any_count_machines();
    assert!(runs(&[]));
    assert!(runs(&[(ANY_COUNT, "off")]));
    assert!(!runs(&[(GITHUB_ACTIONS, "true")]));
    assert!(!runs(&[(GITHUB_ACTIONS, "true"), (ANY_COUNT, "off")]));
    assert!(runs(&[(GITHUB_ACTIONS, "true"), (ANY_COUNT, "on")]));
    assert!(runs(&[(GITHUB_ACTIONS, "false")]));
}

/// The rulings on questions 1 and 2: a value a variable does not take is refused before anything
/// boots, with the variable named and the values it takes, never booted as a default.
#[test]
fn a_value_a_variable_does_not_take_is_refused() {
    for (vars, says) in [
        (
            &[(ACCEL, "hvf")][..],
            "ORIGIN_QEMU_ACCEL must be tcg or kvm",
        ),
        (
            &[(IOMMU, "virtio-iommu-pci")],
            "ORIGIN_QEMU_IOMMU must be intel, amd or an intel-iommu",
        ),
        (&[(IOMMU, "intel-iommu-x")], "ORIGIN_QEMU_IOMMU must be"),
        (
            &[(SMP, "abc")],
            "ORIGIN_QEMU_SMP must be a whole number of processors, 1 or more",
        ),
        (&[(SMP, "0")], "ORIGIN_QEMU_SMP must be"),
        (&[(SMP, "-1")], "ORIGIN_QEMU_SMP must be"),
        (
            &[(TOPOLOGY, "sparce")],
            "ORIGIN_QEMU_TOPOLOGY must be x2apic, sparse, many or listed",
        ),
        (&[(TOPOLOGY, "X2APIC")], "ORIGIN_QEMU_TOPOLOGY must be"),
        (
            &[(TOPOLOGY, "x2apic"), (SMP, "3")],
            "ORIGIN_QEMU_SMP=3 is refused with ORIGIN_QEMU_TOPOLOGY=x2apic",
        ),
        (&[(TOPOLOGY, "listed"), (SMP, "2")], "is refused with"),
        // An unknown name is refused for its own sake, whatever the count.
        (&[(TOPOLOGY, "sparce"), (SMP, "2")], "must be x2apic"),
        (
            &[(NVME_PAIRS, "1")],
            "ORIGIN_QEMU_NVME_PAIRS must be a whole number of I/O queue pairs, 2 or more",
        ),
        (&[(NVME_PAIRS, "lots")], "ORIGIN_QEMU_NVME_PAIRS must be"),
        (&[(BRIDGE, "yes")], "ORIGIN_QEMU_BRIDGE must be on or off"),
        (
            &[(RESERVED, "ON")],
            "ORIGIN_QEMU_RESERVED must be on or off",
        ),
        (&[(ACPI, "1")], "ORIGIN_QEMU_ACPI must be on or off"),
        (
            &[(ANY_COUNT, "true")],
            "ORIGIN_BOOT_MATRIX_ANY_COUNT must be on or off",
        ),
        (
            &[(TIMEOUT, "2m")],
            "ORIGIN_BOOT_TEST_TIMEOUT must be a whole number of seconds",
        ),
        (
            &[(OVMF_CODE, "/firmware/code.fd")],
            "set both ORIGIN_OVMF_CODE and ORIGIN_OVMF_VARS, or neither",
        ),
        (&[(OVMF_VARS, "/firmware/vars.fd")], "set both"),
    ] {
        let e = read(vars).unwrap_err();
        assert!(e.contains(says), "{vars:?}: {e}");
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
        // The child read `ORIGIN_QEMU_SMP` only on the plain machine ("then ignored"), which
        // `from_vars` now refuses beside a named one.
        if vars.get(TOPOLOGY).is_some_and(|t| !t.is_empty()) {
            vars.remove(SMP);
        }
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
        (DEBUG, "1"),
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

/// The ruling R5: `boot-matrix` and its ten scenario commands set each machine's topology,
/// bridge, reserved memory and ACPI tables themselves, so a person's are refused at their entry,
/// named; `boot-test`, `boot-console` and `run` take them. Every other setting carries over.
#[test]
fn the_commands_that_set_their_machines_refuse_the_person_s() {
    assert_eq!(
        crate::SETS_ITS_MACHINES,
        [
            "boot-matrix",
            "boot-x2apic",
            "boot-bridge",
            "boot-reserved",
            "boot-legacy-free",
            "boot-acpi",
            "boot-random",
            "boot-identity",
            "boot-sparse",
            "boot-many",
            "boot-listed",
            "boot-large",
            "boot-rented",
        ]
    );
    for command in ["boot-test", "boot-console", "run"] {
        assert!(!crate::SETS_ITS_MACHINES.contains(&command), "{command}");
    }
    for (vars, says) in [
        (
            &[(TOPOLOGY, "sparse")][..],
            "ORIGIN_QEMU_TOPOLOGY=sparse is refused here",
        ),
        (
            &[(TOPOLOGY, "x2apic")],
            "ORIGIN_QEMU_TOPOLOGY=x2apic is refused here",
        ),
        (&[(TOPOLOGY, "many")], "ORIGIN_QEMU_TOPOLOGY=many"),
        (&[(TOPOLOGY, "listed")], "ORIGIN_QEMU_TOPOLOGY=listed"),
        (&[(BRIDGE, "on")], "ORIGIN_QEMU_BRIDGE=on is refused here"),
        (
            &[(RESERVED, "on")],
            "ORIGIN_QEMU_RESERVED=on is refused here",
        ),
        (&[(ACPI, "on")], "ORIGIN_QEMU_ACPI=on is refused here"),
        (&[(SHAPE, "on")], "ORIGIN_QEMU_SHAPE=on is refused here"),
        (&[(MEMORY, "8G")], "ORIGIN_QEMU_MEMORY=8G is refused here"),
        (
            &[(MEMORY, "1024M")],
            "ORIGIN_QEMU_MEMORY=1G is refused here",
        ),
        (
            &[(TOPOLOGY, "sparse"), (BRIDGE, "on")],
            "ORIGIN_QEMU_TOPOLOGY=sparse and ORIGIN_QEMU_BRIDGE=on is refused here",
        ),
    ] {
        let e = read(vars)
            .unwrap()
            .refuse_machine("boot-matrix")
            .unwrap_err();
        assert!(e.starts_with("boot-matrix sets each machine's"), "{e}");
        assert!(e.contains(says), "{vars:?}: {e}");
        assert!(e.contains("boot-test, boot-console and run take it"), "{e}");
    }
    // What carries over to each machine is not refused, nor a switch set off.
    for vars in [
        &[][..],
        &[(SMP, "3"), (CPU, "max,-avx2"), (MACHINE, "hpet=off")],
        &[(NVME_PAIRS, "2"), (IOMMU, "amd"), (ACCEL, "kvm")],
        &[(TIMEOUT, "600"), (ONLY, "kernel boot"), (DEBUG, "1")],
        &[
            (BRIDGE, "off"),
            (RESERVED, "off"),
            (ACPI, "off"),
            (SHAPE, "off"),
            (MEMORY, "512M"),
            (TOPOLOGY, ""),
        ],
    ] {
        assert_eq!(read(vars).unwrap().refuse_machine("boot-x2apic"), Ok(()));
    }
}

/// R5 through the commands' own entry (`entry_settings`, which `in_turn_with_settings` calls
/// before the turn), one command of each kind: `boot-matrix`, each scenario command, and the
/// three commands that take the person's machine.
#[test]
fn each_kind_of_command_takes_or_refuses_the_person_s_machine_at_entry() {
    let machine = [
        (TOPOLOGY, "sparse"),
        (BRIDGE, "on"),
        (RESERVED, "on"),
        (ACPI, "on"),
    ];
    for command in crate::SETS_ITS_MACHINES {
        for (name, value) in machine {
            let e = crate::entry_settings(command, read(&[(name, value)]).unwrap()).unwrap_err();
            assert!(
                e.starts_with(&format!("{command} sets each machine's"))
                    && e.contains(&format!("{name}={value} is refused here")),
                "{command}: {e}"
            );
        }
        // What carries over is taken.
        let carried = read(&[(SMP, "3"), (MACHINE, "hpet=off"), (BRIDGE, "off")]).unwrap();
        assert_eq!(crate::entry_settings(command, carried.clone()), Ok(carried));
    }
    let all = read(&machine).unwrap();
    for command in ["boot-test", "boot-console", "run"] {
        assert_eq!(
            crate::entry_settings(command, all.clone()),
            Ok(all.clone()),
            "{command}"
        );
    }
    assert!(
        (all.topology, all.bridge, all.reserved, all.acpi) == (Topology::Sparse, true, true, true)
    );
}

/// The scenario filter's refusal names its variable, from this module.
#[test]
fn a_filter_that_matches_nothing_is_named() {
    assert_eq!(
        no_scenario_matches("nope", 11),
        "ORIGIN_BOOT_TEST_ONLY=\"nope\" matches none of the 11 scenarios"
    );
}

/// The prefixes the tripwire refuses outside this module.
const PREFIXES: [&str; 3] = ["ORIGIN_QEMU_", "ORIGIN_BOOT_", "ORIGIN_OVMF_"];

/// The string literals of `text` that begin with one of [`PREFIXES`], read as written and, where
/// the lexer decodes them, decoded.
fn named(text: &str) -> Vec<String> {
    lex::tokens(text)
        .iter()
        .filter(|t| matches!(t.kind, Kind::Str { .. }))
        .filter_map(|t| {
            let decoded = match t.kind {
                Kind::Str { raw: false } => lex::unescape(t.text),
                _ => None,
            };
            [Some(t.text.to_string()), decoded]
                .into_iter()
                .flatten()
                .find(|v| PREFIXES.iter().any(|p| v.starts_with(p)))
        })
        .collect()
}

/// The ruling on question 4: a tripwire, in `cargo test --package xtask`. No `.rs` file of
/// `xtask` but this module and its tests holds, test modules included, a string literal that
/// begins `ORIGIN_QEMU_`, `ORIGIN_BOOT_` or `ORIGIN_OVMF_`: so `format!("ORIGIN_QEMU_{…}")` and
/// `concat!("ORIGIN_QEMU_", …)` are caught as well as a name. The names are private to this
/// module, which builds no command, so none can be passed out to a child. What it does not
/// catch (the module documentation): a name assembled from pieces split before its prefix ends,
/// a prefix written with escapes, a name reached through another crate.
#[test]
fn only_this_module_names_the_variables() {
    let dir = repo_root().join("developer-tools").join("xtask");
    let mut files = Vec::new();
    crate::collect_files(&dir, &|_| false, &|name| name.ends_with(".rs"), &mut files).unwrap();
    assert!(files.len() > 20, "{files:?}");
    let mut read = 0;
    for file in files {
        let relative = file.strip_prefix(&dir).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        if relative == std::path::Path::new("src/settings.rs") {
            // The names are private here, and nothing here starts a program.
            let tokens = lex::tokens(&text);
            assert!(!tokens.iter().any(|t| t.is_ident("Command")));
            for (i, t) in tokens.iter().enumerate() {
                if t.is_ident("const") {
                    let before = i.checked_sub(1).and_then(|b| tokens.get(b));
                    assert!(
                        !before.is_some_and(|b| b.is_ident("pub") || b.is_punct(')')),
                        "a public constant in settings.rs"
                    );
                }
            }
            assert!(!named(&text).is_empty());
            continue;
        }
        if relative == std::path::Path::new("src/settings/tests.rs") {
            continue;
        }
        assert_eq!(named(&text), Vec::<String>::new(), "{}", relative.display());
        read += 1;
    }
    assert!(read > 20);
    // The planted controls: each is caught.
    for (planted, caught) in [
        (
            "fn x() -> Option<String> { env::var(\"ORIGIN_QEMU_SMP\").ok() }",
            "ORIGIN_QEMU_SMP",
        ),
        (
            "fn x(c: &mut Command) { c.env(\"ORIGIN_QEMU_SMP\", \"65\"); }",
            "ORIGIN_QEMU_SMP",
        ),
        (
            "fn x(n: &str) -> String { format!(\"ORIGIN_QEMU_{n}\") }",
            "ORIGIN_QEMU_{n}",
        ),
        (
            "fn x() -> &'static str { concat!(\"ORIGIN_OVMF_\", \"CODE\") }",
            "ORIGIN_OVMF_",
        ),
        (
            "#[cfg(test)]\nmod t { const N: &str = \"ORIGIN_BOOT_TEST_ONLY\"; }",
            "ORIGIN_BOOT_TEST_ONLY",
        ),
        (
            "fn x() -> Option<String> { std::env::var(r#\"ORIGIN_BOOT_MATRIX_ANY_COUNT\"#).ok() }",
            "ORIGIN_BOOT_MATRIX_ANY_COUNT",
        ),
    ] {
        assert_eq!(named(planted), [caught], "{planted}");
    }
    // A name inside a longer text (the usage, a message) is not caught; one split before its
    // prefix ends is not either: a tripwire, not a proof.
    assert!(named("const U: &str = \"the filter (ORIGIN_BOOT_TEST_ONLY)\";").is_empty());
    assert!(named("const N: &str = concat!(\"ORIGIN_QE\", \"MU_SMP\");").is_empty());
}
