//! Origin OS build orchestration (host only; CLAUDE.md §70, handoff task T4).
//!
//! Each Origin artifact is built for its own target, which Cargo alone cannot express on
//! stable Rust (per-package targets are unstable). This tool runs the right command for
//! each crate. It uses std only, no third-party crates. Run it through the alias in
//! `.cargo/config.toml`: `cargo xtask <command>`.
#![forbid(unsafe_code)]

mod amlalloc;
mod asmflags;
mod bench;
mod corestubs;
mod costs;
mod disk;
mod documents;
mod expected_warning;
mod host_tools;
mod image;
mod image_inputs;
mod interfaces;
mod lex;
mod listed_names;
mod loom;
mod mnemonics;
mod per_cpu;
mod progress;
mod provenance;
mod reproducible;
mod requirements;
mod ring3;
mod roadmap;
mod screen;
mod serial;
mod settings;
mod settle;
mod stack_depth;
mod targetcfg;
mod toolchain;
mod turns;
mod unsafety;

use std::borrow::Cow;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, ExitCode, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use settings::{Memory, Settings, Shape, Topology};

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
              boot-random, boot-identity, boot-console, boot-large, boot-rented and
              boot-bench
  boot-bridge boot-test with the NIC behind a PCIe root port (both IOMMUs): bus mastering is
              cleared on the bridge too and opened again above the device its driver attached
  boot-reserved  the kernel boot with firmware-reserved memory (RMRRs, IVMDs) a TEST ONLY UEFI
              application plants in QEMU's tables (both IOMMUs): the NVMe controller's domain
              identity-maps what the kernel may trust, and the rest is refused by name
  boot-large  the kernel boot on a machine with more than 4 GiB of RAM (both IOMMUs; 8 GiB in
              CI): the frame allocator holds all but 1% of it (TASKS item 83)
  boot-rented the kernel boot on rental B's board's shapes (both IOMMUs, the large machine's
              RAM): a TEST ONLY UEFI application puts the RSDP and XSDT in ACPI NVS, names the
              FACS by the FADT's 32-bit field alone, adds an FPDT whose tables are in reserved
              memory and reserves the top of the RAM; the SPCR names COM2 with COM1 present,
              and the kernel must move its console there and end with its verdict
  boot-legacy-free  the kernel boot on a machine with no 8254 and no 8042: the timers
              calibrate against the ACPI PM timer, and the proofs that need those devices
              say they cannot run
  boot-acpi   the kernel boot on a machine whose QEMU builds the ACPI tables the usual one
              lacks: an NFIT for an NVDIMM, an HMAT and an SRAT with a generic initiator, each
              checked by the kernel's census of its tables (TASKS item 62)
  boot-random the kernel boot on two processors that lack what the random-number generator is
              seeded from (ADR-0016): one with RDRAND and no RDSEED, which boots and says
              so; one with neither, which has no generator, launches nothing and still
              reaches its verdict
  boot-identity the kernel boot on two processors QEMU is told to report (ADR-0009 A2): a
              Panther Lake, in the declared scope, and rental A's Zen 4 EPYC, outside it;
              each must print its own CPU identity line and still reach its verdict
  boot-x2apic boot-test on a sparse topology: the boot processor at APIC ID 0 and processors
              at APIC IDs 256 and 412, above what 8-bit APIC IDs reach (the MADT lists
              150 possible processors); twice, on a CPU model with CPUID leaf 1Fh and on
              one without
  boot-sparse boot-test on 67 processors in two packages, three at APIC IDs 256, 300 and
              412 (ADR-0008 D7, decision item 294; part of boot-matrix)
  boot-many   boot-test on 288 processors in two packages, APIC IDs 0-143 and 256-399,
              under QEMU's software processor: measured and recorded, outside the gate
  boot-listed boot-test with 2 processors enabled of the 4096 the MADT lists
              (ADR-0008 Amendment 1, D7)
  boot-domains boot-test on ADR-0020 S23's two machines: one package of four two-thread
              cores in two NUMA nodes, and two packages of four cores, a node each, with a
              PPTT; each prints its scheduling domains (TASKS item 97; part of boot-matrix)
  boot-console the kernel's console on machines without a 16550 at 0x3F8 (ADR-0010): a UART
              named by an ACPI SPCR table, a table that must be refused, no display, and no
              UART at all (with a framebuffer the only way to see the boot)
  bench-build the bench build (TASKS item 112): the developer-mode loader and the kernel with
              breadcrumbs on the SPCR's UART, into target/bench; prints each image's size and
              SHA-256 (target/bench/xtask/BOOTX64.EFI and KERNEL.ONBF), for a rental. Never
              signed, never shipped. With --minimal, also the loader's minimal-program mode
              (ADR-0002 Amendment 5; TASKS item 128), staged with the bench kernel in
              target/bench-minimal/xtask, and its machine code checked: no 0xCF9 anywhere,
              and only the status compare and branches between the call and the triple fault.
              With --bisect, the loader's bisection mode (ADR-0002 Amendment 6; TASKS item
              131) in target/bench-bisect/xtask, with the same checks and one more: the call's
              target is not loaded through the live boot services table. With --kernel, the
              loader's kernel-entry mode (ADR-0002 Amendment 7; TASKS item 133) in
              target/bench-kernel/xtask: the full bench loader's work, no FCH decision and no
              reset, and the bench kernel entered after a successful call; no 0xCF9 anywhere.
              Every bench loader is checked to allocate none of B3's old OS-defined types
  boot-bench  Clippy over the bench code, bench-build, and four boots with the SPCR naming
              COM2: ExitBootServices given the firmware's key, a stale key once (the retry),
              twice (the halt, which says why and then nothing), and the bench kernel under the
              normal loader (no breadcrumbs); part of boot-matrix
  bench-gate  only the gate on which files may name the bench build's flag, or the stack
              measurement's (part of check)
  stack-depth how deep the loader's stack goes before ExitBootServices, under QEMU (TASKS
              item 127): the 128 KiB UEFI promises painted at entry and scanned after each of
              six phases, in a measurement build of the normal and the bench loader into
              target/stack-measure (never signed, never rented), three scenarios three boots
              each, and the longest chain of the loader's own frames its disassembly allows.
              On demand: not part of check or boot-matrix
  run         boot under QEMU with the serial console on this terminal (Ctrl-A X quits)
  reproducible-gate only the gate on reproducible builds: the loader, each way it is built,
              and the kernel are built twice from nothing in two target directories; the
              two files of each must be the same bytes, and hold no path of this machine
              (part of check; CLAUDE.md §75)
  reproducible-compare <directory>
              compares a directory of the files that gate keeps, CI's artifact unpacked or
              another checkout's target/xtask/reproducible/files, with this checkout's own,
              file by file; the directory is read as untrusted data. Builds nothing; run
              reproducible-gate first
  loom        the Loom models of origin-sync (SpinLock and the wait-queue protocol) and their
              negative controls, each applied to a scratch copy of the source; also part of
              check (CLAUDE.md §73)
  unsafe-gate only the gate on `unsafe`, on the kernel's dependency direction, on the
              `preserves_flags` option of assembly blocks, and on RDRAND and RDSEED, which
              only the kernel's generator and the loader's may run (part of check; quick)
  names-gate  only the gate on two names only listed files may use: the proof-process
              constructors in the kernel (ADR-0011 L8) and the call numbers in ring 3
              (CLAUDE.md §42); part of check
  aml-alloc-gate only the gate on the AML interpreter's allocations: only its mem.rs may
              allocate or grow a collection, and a planted push is caught (part of check;
              quick)
  settle-gate only the gate on boot proofs that compare the machine's counts: each
              settles first (TASKS item 67; part of check; quick)
  percpu-gate only the gate on per-processor state (ADR-0008 Amendment 1, D1 and D5):
              every PerCpu table sized by its crate's size_per_cpu, and no MAX_CPUS or
              static array sized by a processor count (part of check; quick)
  cfg-lint    only the CLAUDE.md §11 lint: no Origin code outside developer-tools/ asks
              whether its target is Linux or Unix (part of check; quick)
  document-gate only the gate on documents and archives: none is tracked, or left in this
              branch's history, outside a short allowlist (part of check; quick)
  interface-gate only the gates on the generated interface code: fresh, and its ABI recorded
              (part of check; quick)
  host-tools-gate only the gate on the host tools (ADR-0011 Amendment 4, T1): xtask starts no
              first-party program, by name, by `cargo run` or by a Cargo alias that runs
              one, but those decision items 300 and 310 allow (itself for boot-matrix's
              scenarios, each also run alone, and the command a person gives exclusive),
              and a test starting the program it tests; the libraries of
              onbf-pack, onbf-sign and oidl reach nothing of the host by the forms it
              reads and depend only on the crates it lists; their command lines name no
              crate but their library and std; with planted negative controls (part of
              check; quick)
  requirements-gate only the gate on the requirements matrix under docs/requirements/: a row
              for every statement of CLAUDE.md and every decision of an accepted ADR, each
              true to the tree; prints the coverage figure (part of check; CLAUDE.md §1;
              quick)
  provenance-gate only the gate on the provenance table in
              docs/engineering/SOURCE_PROVENANCE.md: one row for each crate, each of §6's
              fields filled, its dependencies named and its licence stated (part of check;
              CLAUDE.md §6; quick)
  roadmap-gate only the gate on docs/ROADMAP.md: every counted build row of the
              requirements matrix placed in one milestone or listed as in none, and
              every row it names in the matrix (part of check; CLAUDE.md §82; quick)
  progress    writes docs/PROGRESS.md from the requirements matrix, CLAUDE.md's and the
              ADRs' headings and docs/ROADMAP.md: counts of rows by state, overall, by
              milestone, by feature group, by ADR and for the standing rules. Run it when
              a row's state changes (TASKS item 43; quick)
  progress-gate only the gate on docs/PROGRESS.md: the committed page is the one the
              tree generates, byte for byte (part of check; quick)
  cost-gate   only the gate on the cost ledger in docs/engineering/PERFORMANCE_BUDGETS.md:
              each row's decision and figure, the list of ADR-0001 to ADR-0017, and a
              Performance cost statement in every ADR or amendment accepted after
              ADR-0018 (part of check; ADR-0018 V8; quick)
  regenerate rewrites the kernel's generated system-call crate from sdk/interfaces
  version     writes runtime/version/src/generated.rs, the release's constants, from
              origin-version.toml (CLAUDE.md §62)
  version-gate only the gate on that file: it is what origin-version.toml generates (part of
              check; quick)
  image-graph-gate only the gate on what the images are built from: no package under the
              loader, the kernel or a carried program has a build script or is a
              procedural macro (part of check; quick)
  linker-text-gate <ELF>...
              only the check the reproducible-build gate makes of each ELF it builds, on
              files named here: `.comment` holds the line .cargo/config.toml fixes for the
              linker, and not the linker's own, which differs between kinds of host
  core-gate   only the gates on `origin.core` as it was linked and on the programs the kernel
              carries: the image's functions are the stubs of origin.kernel's methods and
              nothing else; each is the extension of its own narrow parameters, the load of
              its own number and one `syscall`; the offsets the kernel accepts each call
              from are that image's; and no carried program holds a `syscall` instruction,
              but a proof program its stated one (part of check; ADR-0005 B2, ADR-0011 L6,
              L8 and L13)
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
  The ORIGIN_OVMF_, ORIGIN_BOOT_ and ORIGIN_QEMU_ variables below are read once, when a boot
  command (boot-test, boot-console, run, boot-matrix or one of its scenario commands) starts.
  An empty value is an unset one, and a value a variable does not take is refused. boot-matrix
  and its scenario commands set each machine's own over them in this process, and give none of
  them to another (TASKS item 61); they set each machine's topology, bridge, reserved memory
  and ACPI tables themselves, so ORIGIN_QEMU_TOPOLOGY, _BRIDGE, _RESERVED and _ACPI are refused
  there (boot-test, boot-console and run take them).
  ORIGIN_OVMF_CODE, ORIGIN_OVMF_VARS  firmware code image and NVRAM template, both or neither
                                      (default: the EDK2 files installed next to QEMU)
  ORIGIN_BOOT_MATRIX_ANY_COUNT        on or off: in CI (GITHUB_ACTIONS=true), whether
                                      boot-matrix runs the 3- and 65-processor machines, the
                                      67-processor sparse machine, the discovery machine,
                                      the SMT machine and the two-package machine;
                                      elsewhere they always run
  ORIGIN_BOOT_TEST_TIMEOUT            boot-test timeout in seconds (default 120)
  ORIGIN_BOOT_TEST_ONLY               run only the boot-test or boot-console scenarios whose
                                      label contains one of these comma-separated strings
  ORIGIN_QEMU_SMP                     processors QEMU emulates, 1 or more (q35 takes up
                                      to 4096; default 2)
  ORIGIN_QEMU_NVME_PAIRS              the I/O queue pairs QEMU's NVMe controller allocates
                                      (default 64; at least 2): 2 is the control for a
                                      controller with no pair past the driver's two
  ORIGIN_QEMU_TOPOLOGY                x2apic: the boot-x2apic machine; sparse: 67
                                      processors, 64 in package 0 and three at APIC IDs 256,
                                      300 and 412; many: two packages of 144 cores, 288
                                      processors with APIC IDs 0-143 and 256-399; listed: 2
                                      processors of 4096 the MADT lists; smt: one package
                                      of four two-thread cores in two nodes; packages: two
                                      packages of four cores, a node each (ORIGIN_QEMU_SMP is
                                      refused with any of them; ADR-0008 Amendment 1, D7;
                                      ADR-0020 S23); unset: the plain machine of
                                      ORIGIN_QEMU_SMP processors
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
                                      machine); off or unset: not
  ORIGIN_QEMU_RESERVED                on: a TEST ONLY UEFI application plants firmware-reserved
                                      memory (RMRRs or IVMDs) in QEMU's tables before the
                                      loader starts (the boot-reserved machine); off or unset: not
  ORIGIN_QEMU_MEMORY                  the machine's RAM: 512M (default), an even number of MiB
                                      above it, or 1G to 64G (the boot-large machine); above
                                      512M the kernel's memory figures are checked against it
  ORIGIN_QEMU_SHAPE                   on: a TEST ONLY UEFI application shapes the tables as
                                      rental B's board has them (the boot-rented machine); for
                                      boot-test's kernel boot, also an SPCR naming a second UART
                                      at 0x2F8, which carries the markers; not with
                                      ORIGIN_QEMU_RESERVED, nor on a processor without RDSEED and
                                      RDRAND; off or unset: not
  ORIGIN_QEMU_ACPI                    on: QEMU builds an NFIT, an HMAT and an SRAT with a generic
                                      initiator (the boot-acpi machine; needs
                                      ORIGIN_QEMU_MACHINE=nvdimm=on,hmat=on); off or unset: not
  ORIGIN_QEMU_ACCEL                   tcg (default) or kvm: run on a real x86-64 processor (needs
                                      an x86-64 host with /dev/kvm); the devices and firmware
                                      stay QEMU's
  ORIGIN_QEMU_DEBUG                   on if set and not empty: QEMU logs interrupts and resets
                                      to target/xtask/qemu-debug.log
  RUSTFLAGS, CARGO_ENCODED_RUSTFLAGS, CARGO_TARGET_<TRIPLE>_RUSTFLAGS for the kernel's, the
  loader's and the ring-3 programs' triples; RUSTC, RUSTC_WRAPPER, RUSTC_WORKSPACE_WRAPPER
  and their CARGO_BUILD_ forms; CARGO_TARGET_<TRIPLE>_LINKER for the kernel's and the
  loader's triples
                                      refused if set, even empty: they replace or add to the
                                      repository's compiler flags, or put another compiler or
                                      linker in the pinned one's place, so an image would not
                                      be the one the repository describes. Only `exclusive`
                                      runs with them, and what it runs may not";

const UEFI_TARGET: &str = "x86_64-unknown-uefi";
const LOADER_PACKAGE: &str = "origin-boot-uefi";
const LOADER_BIN: &str = "origin-boot";
/// TEST ONLY: the UEFI application that plants firmware-reserved memory in QEMU's ACPI tables
/// before the loader starts (`ORIGIN_QEMU_RESERVED`; TASKS item 40).
const RESERVED_PACKAGE: &str = "origin-test-reserved-memory";
const RESERVED_BIN: &str = "origin-test-reserved";
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
    // ACPI 6.6's tables (ADR-0019 A6, area A): the kernel's census of them.
    "origin-acpi-tables",
    // The AML interpreter (ADR-0019 A3), which the kernel's boot proof runs (TASKS item 63, step
    // B3). Its no-panic check is its own (`no_panic_check`), with `alloc`.
    AML_PACKAGE,
    // Microsoft's SPCR, which the loader and the kernel both parse (TASKS item 84).
    "origin-spcr",
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
/// The AML interpreter (ADR-0019 A3; TASKS item 63): `no_std` with `alloc`, in the kernel since
/// its boot proof (step B3), and the ACPI service's logic when that exists.
const AML_PACKAGE: &str = "origin-aml";
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
    /// The bench build (TASKS item 112): `developer-mode`, and the bench flag only
    /// `bench::build_loader` gives, into `target/bench`.
    Bench,
    /// TASKS item 127's stack measurement of the developer-mode loader, into
    /// `target/stack-measure/normal` (`stack_depth::build_loader`).
    MeasureNormal,
    /// The same of the bench loader, into `target/stack-measure/bench`.
    MeasureBench,
    /// TASKS item 128: the bench loader's minimal-program mode (ADR-0002 Amendment 5), into
    /// `target/bench-minimal` (`bench::build_minimal_loader`).
    BenchMinimal,
    /// TASKS item 131: the bench loader's bisection mode (ADR-0002 Amendment 6), into
    /// `target/bench-bisect` (`bench::build_bisect_loader`).
    BenchBisect,
    /// TASKS item 133: the bench loader's kernel-entry mode (ADR-0002 Amendment 7), into
    /// `target/bench-kernel` (`bench::build_kernel_loader`).
    BenchKernel,
}

impl Loader {
    /// The Cargo feature this build needs.
    fn feature(self) -> &'static str {
        match self {
            Self::Developer
            | Self::Bench
            | Self::BenchMinimal
            | Self::BenchBisect
            | Self::BenchKernel
            | Self::MeasureNormal
            | Self::MeasureBench => "developer-mode",
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
    check: Option<Check>,
    /// Text QEMU itself must have printed on its stderr by then (what its device models
    /// report about the guest).
    stderr: Vec<&'static str>,
    /// Which serial ports and monitor QEMU has, and where the markers are read from.
    capture: Capture,
    /// Stages what the scenario needs under the work directory and gives QEMU's extra
    /// arguments for it.
    qemu_args: Option<QemuArgs>,
    /// How long the output is read on once every marker has appeared, so the check sees
    /// whatever followed: a halt that must say nothing more (TASKS item 112).
    quiet: Option<Duration>,
}

/// A function that stages a scenario's files and returns its extra QEMU arguments.
type QemuArgs = fn(&Path) -> Result<Vec<String>>;

/// A check of a whole boot, given the settings it booted with.
type Check = fn(&Completed, &Settings) -> Result;

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
    /// Whether QEMU exited by itself, with a success status, before the test stopped it. Under
    /// `-no-reboot` that is the guest resetting the machine (TASKS item 116); a triple fault is
    /// a reset to QEMU too, which the scenarios' lines tell apart.
    exited: bool,
    /// When QEMU exited by itself: the time from the last byte it sent to its output closing, to
    /// within the reader's latency. TASKS item 121's scenarios tell a reset through `0xCF9` (at
    /// once) from the triple fault (after the loader's wait) by it.
    quiet_before_exit: Option<Duration>,
}

/// The IOMMU QEMU emulates (`ORIGIN_QEMU_IOMMU`, [`Settings::iommu`]): Intel VT-d or AMD-Vi,
/// or a raw `intel-iommu` device for negative controls.
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

/// The QEMU arguments of the `boot-acpi` machine beyond the usual ones (TASKS item 62; ADR-0019
/// A7): `nvdimm=on,hmat=on` go in through `ORIGIN_QEMU_MACHINE`; here, an NVDIMM in node 1 (the
/// NFIT), a third node with no memory and no processor holding the NVMe controller as a generic
/// initiator (SRAT type 5), every distance to it, and latencies, bandwidths and a memory-side
/// cache for every processor's node and every node with memory (the HMAT). It adds no PCI
/// function, so the usual proofs' counts hold. Checked against QEMU 11.1.3 with the guest paused
/// before it was booted: QEMU refuses an `hmat-lb` initiator with no processor, and an
/// `initiator` property naming the generic initiator's own node.
fn acpi_machine_args(cpus: u32) -> Vec<String> {
    let mut args: Vec<String> = [
        "-object",
        "memory-backend-ram,id=onvdimm,size=128M",
        "-device",
        "nvdimm,id=onvdimm0,memdev=onvdimm,node=1",
        "-numa",
        "node,nodeid=2",
        "-numa",
        "dist,src=0,dst=2,val=31",
        "-numa",
        "dist,src=1,dst=2,val=31",
        "-object",
        "acpi-generic-initiator,id=ogi0,pci-dev=onvme,node=2",
    ]
    .iter()
    .map(|a| (*a).to_string())
    .collect();
    // The HMAT's initiators are the nodes with a processor (QEMU refuses one without).
    let initiators: &[u32] = if cpus >= 2 { &[0, 1] } else { &[0] };
    for &initiator in initiators {
        for target in [0u32, 1] {
            let near = initiator == target;
            let (latency, bandwidth) = if near { (10, "10G") } else { (20, "5G") };
            args.push("-numa".into());
            args.push(format!(
                "hmat-lb,initiator={initiator},target={target},hierarchy=memory,data-type=access-latency,latency={latency}"
            ));
            args.push("-numa".into());
            args.push(format!(
                "hmat-lb,initiator={initiator},target={target},hierarchy=memory,data-type=access-bandwidth,bandwidth={bandwidth}"
            ));
        }
    }
    args.push("-numa".into());
    args.push(
        "hmat-cache,node-id=0,size=8M,level=1,associativity=direct,policy=write-back,line=64"
            .into(),
    );
    args
}

/// A `-numa node` value for node `id` with its 256M backend. On the ACPI machine the node also
/// names its processor, if it has one, and the initiator nearest its memory, which the HMAT
/// needs: QEMU spreads processors over the nodes itself only after it has read the HMAT's
/// options, which refuse an initiator with no processor yet.
fn numa_node(id: u32, machine: &Machine, acpi: bool) -> String {
    let cpus = machine.cpus();
    let node = format!("node,nodeid={id},memdev=onuma{id}");
    if let Some(ranges) = machine.node_cpus
        && let Some(range) = ranges.get(id as usize)
    {
        return format!("{node},cpus={range}");
    }
    if !acpi {
        return node;
    }
    if id < cpus {
        format!("{node},cpus={id},initiator={id}")
    } else {
        format!("{node},initiator=0")
    }
}

/// The shapes `boot-rented` gives the firmware's tables, as rental B's board has them (TASKS item
/// 83; `docs/research/2026-11-20-rental-b-findings.md`): the RSDP and XSDT in ACPI NVS, the FACS
/// named by the FADT's 32-bit field alone, an FPDT whose FBPT and S3PT are in reserved memory,
/// and 32 MiB of reserved pages at the top of the RAM. The board's device and reserved ranges
/// above its RAM, up to just below 1 TiB, need the PI specification's DXE services, which no one
/// here has read: not shown (the coordinating session's ruling Q3).
const RENTED_SHAPE: origin_test_reserved_memory::Shape = origin_test_reserved_memory::Shape {
    acpi_nvs: true,
    facs_32: true,
    fpdt: true,
    top_reserved_pages: 8192,
    rsdp_in_ram: false,
    spcr_in_ram: false,
};

/// The device the planted ranges name and no driver attaches: 00:1f.6, absent on QEMU's q35.
const ABSENT_DEVICE: u16 = 0x1F << 3 | 6;

/// The regions planted for the `boot-reserved` machine (TASKS item 40; research ledger
/// `docs/research/2026-11-12-reserved-memory-identity-maps.md` §6), for the IOMMU QEMU
/// emulates. Under VT-d: an RMRR of two reserved pages for the NVMe controller (00:02.0), and
/// two the kernel must refuse, for a device that does not exist: one in RAM the kernel will
/// own, one in the interrupt range. Under AMD-Vi, IVMDs for the controller: one read and
/// write, one read only, and one never to be mapped; one for every device, as an exclusion
/// range; and one in RAM, refused.
fn reserved_regions(amd: bool) -> Vec<origin_test_reserved_memory::Region> {
    use origin_test_reserved_memory::{Placement, Region, Table};
    const NVME: u16 = 2 << 3;
    let rmrr = |placement, device, pages| Region {
        table: Table::Dmar,
        ivmd_kind: 0,
        flags: 0,
        placement,
        device,
        last_device: 0,
        pages,
    };
    let ivmd = |kind, flags, placement, device, last_device| Region {
        table: Table::Ivrs,
        ivmd_kind: kind,
        flags,
        placement,
        device,
        last_device,
        pages: 1,
    };
    if amd {
        vec![
            ivmd(0x21, 0b0111, Placement::Reserved, NVME, 0),
            ivmd(0x21, 0b0011, Placement::Reserved, NVME, 0),
            ivmd(0x20, 0b1000, Placement::Reserved, 0, 0),
            ivmd(0x22, 0b0000, Placement::Reserved, NVME, 3 << 3),
            ivmd(0x21, 0b0111, Placement::LoaderData, ABSENT_DEVICE, 0),
            // A writable range whose page lacks the test fill: the proof must not write it (M2).
            ivmd(0x21, 0b0111, Placement::ReservedOther, NVME, 0),
            // A writable range over the IVRS copy itself, which the kernel holds (M1).
            ivmd(0x21, 0b0111, Placement::Table, ABSENT_DEVICE, 0),
            // Never mapped for any device of any IOMMU (ADR-0007 Amendment 2, R6).
            ivmd(0x20, 0b0000, Placement::Reserved, 0, 0),
        ]
    } else {
        vec![
            rmrr(Placement::Reserved, NVME, 2),
            rmrr(Placement::LoaderData, ABSENT_DEVICE, 1),
            rmrr(Placement::Fixed(0xFEE0_0000), ABSENT_DEVICE, 1),
            // A writable range whose page lacks the test fill: the proof must not write it (M2).
            rmrr(Placement::ReservedOther, NVME, 1),
            // A writable range over the DMAR copy itself, which the kernel holds (M1).
            rmrr(Placement::Table, ABSENT_DEVICE, 1),
        ]
    }
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

/// What the per-processor NVMe proof must print on this configuration, for a controller that
/// allocates `pairs` I/O queue pairs: every processor given a queue proven, or, where the
/// controller gives none past the driver's two, "not proven". The kernel judges on the controller's own numbers and prints each (review finding F5 of 2026-11-14):
/// QEMU 11.1's controller allocates its `max_ioqpairs` whatever is asked.
fn nvme_per_cpu_markers_for(machine: &Machine, cpus: u32, pairs: u32) -> Vec<String> {
    let queued = nvme_queued_for(cpus, pairs);
    let numbers = format!(
        "the controller allocated {pairs} submission and {pairs} completion queues, has {QEMU_NVME_VECTORS} MSI-X vectors, and BAR 0 holds the doorbells of {QEMU_NVME_BAR_QUEUES} I/O queues; the driver keeps 2 of each"
    );
    if queued == 0 {
        return vec![format!(
            "origin-kernel: NVMe: per-CPU queues: not proven: {numbers}"
        )];
    }
    std::iter::once(format!(
        "origin-kernel: NVMe: per-CPU queues: {queued} of {cpus} processors have an I/O queue pair and MSI-X vector of their own, {} have none ({numbers}): {queued} expected, {queued} filled",
        cpus - queued
    ))
    .chain(queue_order(machine).into_iter().take(queued as usize).map(|index| {
        format!(
            "origin-kernel: NVMe: {}: 32 reads through its own queue pair, 256 of 256 blocks match, its write read back true, ",
            machine.named(index)
        )
    }))
    .chain([format!(
        "origin-kernel: NVMe: per-CPU queues: {queued} threads at once, one per CPU, each with its own queue pair and MSI-X vector: {} reads, {1} of {1} blocks match",
        32 * queued,
        256 * queued
    )])
    .collect()
}

/// I/O queue pairs QEMU 11.1's NVMe controller allocates (its `max_ioqpairs`), and its MSI-X
/// vectors (`msix_qsize`): the driver keeps two pairs and two vectors, and every other
/// processor past what is left has no queue of its own (ADR-0008 Amendment 1, D6).
const QEMU_NVME_PAIRS: u32 = 64;
const QEMU_NVME_VECTORS: u32 = 65;
/// The highest I/O queue id whose doorbells QEMU 11.1's controller's BAR 0 holds: 16 KiB with
/// doorbell stride 4, so queue 1535's completion doorbell is the last dword (`nvme::doorbell_queues`).
const QEMU_NVME_BAR_QUEUES: u32 = 1535;

/// The CPU indexes in the order the kernel gives them NVMe queues of their own: every processor
/// whose APIC ID is above 255 first, then the rest in table order (`storage::queue_order`;
/// ADR-0008 Amendment 1, D6; review finding F1 of 2026-11-14). The first [`nvme_queued`] of
/// them get one, MSI-X entry `2 + k` for the `k`th.
fn queue_order(machine: &Machine) -> Vec<u32> {
    let wide = |index: &u32| {
        machine
            .apic_ids
            .get(*index as usize)
            .is_some_and(|&id| id > 0xFF)
    };
    let all = 0..machine.cpus();
    all.clone()
        .filter(wide)
        .chain(all.filter(|index| !wide(index)))
        .collect()
}

/// The processors that get an NVMe queue of their own on a controller that allocates `pairs` I/O
/// queue pairs (QEMU's default, or `ORIGIN_QEMU_NVME_PAIRS`, at least 2, which limits its
/// `max_ioqpairs` for the control that shows a controller with none past the driver's two: the
/// coordinating session's ruling of 2026-11-14): the first this many.
fn nvme_queued_for(cpus: u32, pairs: u32) -> u32 {
    cpus.min(pairs.saturating_sub(2)).min(QEMU_NVME_VECTORS - 2)
}

/// The per-CPU tables the kernel sizes at discovery, the framework's and the kernel crate's
/// (ADR-0008 Amendment 1, D1; `per_cpu` checks that each is sized).
const PER_CPU_TABLES: u32 = 30;

/// The cores of each package of the 288-processor run (`ORIGIN_QEMU_TOPOLOGY=many`, outside the
/// gate since decision item 294).
const MANY_CORES: u32 = 144;
/// The cores of each package of the gate's large sparse machine (`ORIGIN_QEMU_TOPOLOGY=sparse`,
/// decision item 294), and the processors QEMU starts in package 0.
const SPARSE_CORES: u32 = 157;
const SPARSE_PRESENT: u32 = 64;
/// The processors the discovery machine's MADT lists (`ORIGIN_QEMU_TOPOLOGY=listed`): the most
/// q35 takes, as QEMU 11.1.3 says on 2026-11-13.
const LISTED: u32 = 4096;

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
    /// The NUMA node of each CPU index: QEMU's own placement, a package's index modulo the two
    /// nodes (measured with QMP's `query-cpus-fast` on QEMU 11.1.3, 2026-11-21: every processor
    /// of a one-socket machine in node 0), unless `node_cpus` names them.
    nodes: Vec<u32>,
    /// The `cpus=` of each of the two `-numa node`s, where the machine places its processors
    /// itself (TASKS item 97, ruling Q5: the SMT and two-package machines).
    node_cpus: Option<[&'static str; 2]>,
    /// A PPTT the machine's firmware is given (`-acpitable`), and how it groups the L3.
    pptt: Option<Pptt>,
}

/// A PPTT the machine's firmware is given (TASKS item 97; the coordinating session's ruling Q6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pptt {
    /// Which processors its L3 is shared by.
    l3: L3,
    /// Whether its L3s carry valid Cache IDs (control C3 takes them away).
    cache_ids: bool,
}

/// Which processors a PPTT's L3 is shared by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum L3 {
    /// A package's, as CPUID says on every machine here (`SMP_CACHE`).
    Package,
    /// A NUMA node's: what control C2 plants on the SMT machine, where CPUID says a package's.
    /// Built only by the control's one-line edit and by the host test of the table.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "control C2's table: built by its edit and its host test"
        )
    )]
    Node,
}

/// Processor hierarchy node flags and cache attributes of ACPI 6.6 §5.2.31 (Tables 5.190 to
/// 5.192): a physical package, a valid processor ID, a thread, a leaf; every cache property
/// valid, and the Cache ID too; unified, data and instruction caches.
const PPTT_PACKAGE: u32 = 1;
const PPTT_ID_VALID: u32 = 2;
const PPTT_THREAD: u32 = 4;
const PPTT_LEAF: u32 = 8;
const PPTT_PROPERTIES: u32 = 0x7F;
const PPTT_CACHE_ID: u32 = 0x80;
const PPTT_UNIFIED: u8 = 0b1000;
const PPTT_DATA: u8 = 0b0000;
const PPTT_INSTRUCTION: u8 = 0b0100;

/// `machine`'s PPTT, revision 3, built here from ACPI 6.6 §5.2.31's layouts and independently of
/// the kernel's reading of it. A node for each package (and, for [`L3::Node`], one for each NUMA
/// node in it, which holds the L3), one for each core, and a leaf for each processor, named by
/// its CPU index, which is QEMU's ACPI processor ID in its MADT (measured: TASKS item 97's
/// records). One L1 instruction, one L1 data and one L2 structure are listed under every core,
/// which ACPI reads as one cache of each per core ("for compactness"); each L3 is a structure of
/// its own.
fn pptt_table(machine: &Machine, plan: Pptt) -> Vec<u8> {
    fn add(table: &mut Vec<u8>, structure: Vec<u8>) -> u32 {
        let at = table.len() as u32;
        table.extend(structure);
        at
    }
    fn cache(flags: u32, attributes: u8, next: u32, id: u32) -> Vec<u8> {
        let mut c = vec![1, 28, 0, 0];
        c.extend(flags.to_le_bytes());
        c.extend(next.to_le_bytes());
        c.extend((1u32 << 20).to_le_bytes());
        c.extend(1024u32.to_le_bytes());
        c.extend([16, attributes, 64, 0]);
        c.extend(id.to_le_bytes());
        c
    }
    fn node(flags: u32, parent: u32, uid: u32, resources: &[u32]) -> Vec<u8> {
        let mut n = vec![0, (20 + 4 * resources.len()) as u8, 0, 0];
        n.extend(flags.to_le_bytes());
        n.extend(parent.to_le_bytes());
        n.extend(uid.to_le_bytes());
        n.extend((resources.len() as u32).to_le_bytes());
        for r in resources {
            n.extend(r.to_le_bytes());
        }
        n
    }
    let mut t = vec![0u8; 36];
    t[..4].copy_from_slice(b"PPTT");
    t[8] = 3;
    t[10..16].copy_from_slice(b"ORIGIN");
    t[16..24].copy_from_slice(b"ORIGPPTT");
    t[24..28].copy_from_slice(&1u32.to_le_bytes());
    t[28..32].copy_from_slice(b"ORIG");
    t[32..36].copy_from_slice(&1u32.to_le_bytes());
    let l2 = add(&mut t, cache(PPTT_PROPERTIES, PPTT_UNIFIED, 0, 0));
    let l1i = add(&mut t, cache(PPTT_PROPERTIES, PPTT_INSTRUCTION, l2, 0));
    let l1d = add(&mut t, cache(PPTT_PROPERTIES, PPTT_DATA, l2, 0));
    let g = machine.geometry;
    let (threads, package_at) = (Geometry::bits(g.threads), g.package_at());
    let mut next_id = 1;
    let mut l3 = |t: &mut Vec<u8>| {
        let (flags, id) = if plan.cache_ids {
            (PPTT_PROPERTIES | PPTT_CACHE_ID, next_id)
        } else {
            (PPTT_PROPERTIES, 0)
        };
        next_id += 1;
        add(t, cache(flags, PPTT_UNIFIED, 0, id))
    };
    for package in 0..g.packages {
        let in_package: Vec<(u32, u32)> = (0..machine.cpus())
            .map(|index| (index, machine.apic_ids[index as usize]))
            .filter(|&(_, id)| id >> package_at == package)
            .collect();
        let package_node = match plan.l3 {
            L3::Package => {
                let cache = l3(&mut t);
                add(&mut t, node(PPTT_PACKAGE, 0, package, &[cache]))
            }
            L3::Node => add(&mut t, node(PPTT_PACKAGE, 0, package, &[])),
        };
        // The node each processor's L3 hangs from: the package, or its NUMA node's group.
        let mut groups: Vec<(u32, u32)> = Vec::new();
        let mut parent_of = |t: &mut Vec<u8>, index: u32| -> u32 {
            if plan.l3 == L3::Package {
                return package_node;
            }
            let numa = machine.nodes[index as usize];
            if let Some(&(_, at)) = groups.iter().find(|&&(n, _)| n == numa) {
                return at;
            }
            let cache = l3(t);
            let at = add(t, node(0, package_node, 0, &[cache]));
            groups.push((numa, at));
            at
        };
        let mut cores: Vec<(u32, u32)> = Vec::new();
        for &(index, id) in &in_package {
            let core_key = id >> threads;
            let parent = parent_of(&mut t, index);
            if g.threads == 1 {
                add(
                    &mut t,
                    node(PPTT_ID_VALID | PPTT_LEAF, parent, index, &[l1i, l1d]),
                );
                continue;
            }
            let core = match cores.iter().find(|&&(k, _)| k == core_key) {
                Some(&(_, at)) => at,
                None => {
                    let at = add(&mut t, node(0, parent, 0, &[l1i, l1d]));
                    cores.push((core_key, at));
                    at
                }
            };
            add(
                &mut t,
                node(PPTT_ID_VALID | PPTT_THREAD | PPTT_LEAF, core, index, &[]),
            );
        }
    }
    let len = t.len() as u32;
    t[4..8].copy_from_slice(&len.to_le_bytes());
    let sum = t.iter().fold(0u8, |s, &b| s.wrapping_add(b));
    t[9] = 0u8.wrapping_sub(sum);
    t
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

    /// The bits of an APIC ID below the package's: the package's index is the ID shifted by
    /// this many.
    fn package_at(&self) -> u32 {
        Self::bits(self.threads)
            + Self::bits(self.cores)
            + Self::bits(self.modules)
            + Self::bits(self.dies)
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

/// The machine the boot uses ([`machine_shape`]), with each CPU index's NUMA node: QEMU places a
/// processor in its package's index modulo the two nodes unless the machine names them; on the
/// ACPI machine each `-numa node` names its processor ([`numa_node`]).
fn machine(settings: &Settings) -> Machine {
    let mut machine = machine_shape(settings);
    if machine.nodes.is_empty() {
        let package_at = machine.geometry.package_at();
        machine.nodes = machine
            .apic_ids
            .iter()
            .enumerate()
            .map(|(index, &id)| {
                if settings.acpi && index < 2 {
                    index as u32
                } else {
                    (id >> package_at) % 2
                }
            })
            .collect();
    }
    machine
}

/// The machine the boot uses: `-smp N` (QEMU numbers the processors 0 to N-1), or with
/// `ORIGIN_QEMU_TOPOLOGY=x2apic` a sparse one. There, 2 packages of 3 dies × 5 modules × 5
/// cores take 2 + 3 + 3 bits of the APIC ID below the package, so package 1 starts at APIC
/// ID 256; 150 processors are possible. Present are the boot processor, one cold-plugged at
/// package 1 (APIC ID 256, docs/research/2026-11-08-x2apic-above-255.md) and one at package 1,
/// die 2, module 3, core 4 (APIC ID 412): its die, module and core are all non-zero, so a
/// decoder that confuses them shows (docs/research/2026-11-08-cpu-topology-leaf-1fh.md).
fn machine_shape(settings: &Settings) -> Machine {
    let cpus = match settings.topology {
        Topology::Plain(cpus) => cpus,
        Topology::X2apic => {
            // The cold-plugged processors are of the boot processor's model: QEMU names their
            // type after the model (`max` -> `max-x86_64-cpu`) and applies `-cpu`'s features.
            let base = settings.cpu.split(',').next().unwrap_or("max");
            return Machine {
                nodes: Vec::new(),
                node_cpus: None,
                pptt: None,
                smp: "1,maxcpus=150,sockets=2,dies=3,modules=5,cores=5,threads=1".into(),
                devices: vec![
                    format!(
                        "{base}-x86_64-cpu,socket-id=1,die-id=0,module-id=0,core-id=0,thread-id=0"
                    ),
                    format!(
                        "{base}-x86_64-cpu,socket-id=1,die-id=2,module-id=3,core-id=4,thread-id=0"
                    ),
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
        Topology::Sparse => {
            // ADR-0008 Amendment 1, D7 as decision item 294 has it: the gate's large machine. Two
            // packages of 157 single-thread cores (8 bits of core ID, so package 1 starts at APIC ID
            // 256), 314 possible; the 64 processors of package 0 that QEMU starts, and three
            // cold-plugged in package 1 at cores 0, 44 and 156: APIC IDs 256, 300 and 412. 67
            // processors, three above 255, none a power of two.
            let base = settings.cpu.split(',').next().unwrap_or("max");
            let geometry = Geometry {
                packages: 2,
                dies: 1,
                modules: 1,
                cores: SPARSE_CORES,
                threads: 1,
            };
            let plugged = [0, 44, 156];
            let mut apic_ids: Vec<u32> = (0..SPARSE_PRESENT).collect();
            apic_ids.extend(plugged.iter().map(|core| 256 + core));
            return Machine {
                nodes: Vec::new(),
                node_cpus: None,
                pptt: None,
                smp: format!(
                    "{SPARSE_PRESENT},maxcpus={},sockets=2,cores={SPARSE_CORES},threads=1",
                    2 * SPARSE_CORES
                ),
                devices: plugged
                    .iter()
                    .map(|core| format!("{base}-x86_64-cpu,socket-id=1,core-id={core},thread-id=0"))
                    .collect(),
                listed: 2 * SPARSE_CORES,
                highest: 256 + SPARSE_CORES - 1,
                apic_ids,
                geometry,
            };
        }
        Topology::Many => {
            // ADR-0008 Amendment 1, D7: the large sparse machine. 144 cores take 8 bits, so package
            // 1 starts at APIC ID 256: 288 processors, half of them above 255, none a power of two.
            let geometry = Geometry {
                packages: 2,
                dies: 1,
                modules: 1,
                cores: MANY_CORES,
                threads: 1,
            };
            let apic_ids = geometry.ids();
            return Machine {
                nodes: Vec::new(),
                node_cpus: None,
                pptt: None,
                smp: format!("{0},sockets=2,cores={MANY_CORES},threads=1", 2 * MANY_CORES),
                devices: Vec::new(),
                listed: apic_ids.len() as u32,
                highest: apic_ids.iter().copied().max().unwrap_or(0),
                apic_ids,
                geometry,
            };
        }
        Topology::Smt | Topology::Packages => {
            // ADR-0020 S23 (TASKS item 97): eight processors, two NUMA nodes each. The SMT
            // machine is one package of four two-thread cores, its nodes two cores each, siblings
            // never split (ruling Q5); the two-package machine is two packages of four
            // single-thread cores, a node each, with a PPTT that agrees with CPUID (ruling Q6).
            let smt = settings.topology == Topology::Smt;
            let geometry = Geometry {
                packages: if smt { 1 } else { 2 },
                dies: 1,
                modules: 1,
                cores: 4,
                threads: if smt { 2 } else { 1 },
            };
            let apic_ids = geometry.ids();
            return Machine {
                smp: if smt {
                    "8,sockets=1,cores=4,threads=2".into()
                } else {
                    "8,sockets=2,cores=4,threads=1".into()
                },
                devices: Vec::new(),
                listed: 8,
                highest: 7,
                nodes: (0..8).map(|i| i / 4).collect(),
                node_cpus: Some(["0-3", "4-7"]),
                pptt: (!smt).then_some(Pptt {
                    l3: L3::Package,
                    cache_ids: true,
                }),
                apic_ids,
                geometry,
            };
        }
        Topology::Listed => {
            // ADR-0008 Amendment 1, D7: the discovery machine. The MADT lists every processor q35
            // allows, 4096, in one package; two are present and enabled.
            return Machine {
                nodes: Vec::new(),
                node_cpus: None,
                pptt: None,
                smp: format!("2,maxcpus={LISTED},sockets=1,cores={LISTED},threads=1"),
                devices: Vec::new(),
                apic_ids: vec![0, 1],
                listed: LISTED,
                highest: LISTED - 1,
                geometry: Geometry {
                    packages: 1,
                    dies: 1,
                    modules: 1,
                    cores: LISTED,
                    threads: 1,
                },
            };
        }
    };
    Machine {
        nodes: Vec::new(),
        node_cpus: None,
        pptt: None,
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

/// The cache leaf the guest reports its caches' sharing in, as `cargo xtask` sets the machine up
/// (TASKS item 97): leaf 04H on an Intel guest; Fn8000_001D on an AMD guest under KVM whose host
/// has TopologyExtensions (QEMU's software processor refuses it, measured with QEMU 11.1.3 on
/// 2026-11-21); otherwise none, an AMD guest's leaf 04H being zero.
fn cache_leaf(settings: &Settings) -> Option<&'static str> {
    if guest_is_intel(&settings.cpu, settings.accel, host_is_intel()) {
        Some("leaf 04H")
    } else if settings.accel == Accel::Kvm
        && !settings.cpu_lacks("topoext")
        && host_has_flag("topoext")
    {
        Some("leaf 8000001DH")
    } else {
        None
    }
}

/// Whether this host's processor lists `flag` (read only for KVM, which needs a Linux host).
fn host_has_flag(flag: &str) -> bool {
    fs::read_to_string("/proc/cpuinfo").is_ok_and(|info| {
        info.lines()
            .filter(|l| l.starts_with("flags"))
            .any(|l| l.split_whitespace().any(|f| f == flag))
    })
}

/// The scheduling domains the kernel must report (ADR-0020 S8, S23; TASKS item 97), computed here
/// from the `-smp` fields, the NUMA placement and the caches `SMP_CACHE` pins, never from the
/// kernel's view: each level's groups, narrowest first, a level equal to the one below merged and
/// named after both, a level of single processors left out. `cache_leaf` is the leaf the guest
/// reports its caches in, if any: the L2 is then a core's and the last level a package's.
/// `leaf_1f` as [`topology_markers`]: only leaf 1Fh enumerates dies.
fn domain_tree(machine: &Machine, cache_leaf: bool, leaf_1f: bool) -> String {
    let g = machine.geometry;
    let threads = Geometry::bits(g.threads);
    let package_at = g.package_at();
    let die_at = package_at - Geometry::bits(g.dies);
    let ids = &machine.apic_ids;
    let n = ids.len();
    let keyed = |key: &dyn Fn(usize) -> u64| -> Vec<usize> {
        (0..n)
            .map(|i| (0..n).find(|&j| key(j) == key(i)).unwrap_or(i))
            .collect()
    };
    let mut levels: Vec<(Vec<&str>, Vec<usize>)> = Vec::new();
    let mut add = |name: &'static str, groups: Vec<usize>| {
        levels.push((vec![name], groups));
    };
    add("core", keyed(&|i| u64::from(ids[i] >> threads)));
    // The caches: CPUID's (`SMP_CACHE`: the L2 a core's, the L3 a package's) where the guest
    // reports them, the planted PPTT's (its L2 a core's, its L3 a package's or a node's), and
    // where both speak, their meet, as the kernel combines them (the review's R97-2).
    let core_key = |i: usize| u64::from(ids[i] >> threads);
    let pptt_l3 = |i: usize| match machine.pptt.map(|p| p.l3) {
        Some(L3::Node) => u64::from(machine.nodes[i]),
        _ => u64::from(ids[i] >> package_at),
    };
    let both = |cpuid: &dyn Fn(usize) -> u64, pptt: &dyn Fn(usize) -> u64| -> Vec<usize> {
        match (cache_leaf, machine.pptt.is_some()) {
            (true, true) => keyed(&|i| cpuid(i) << 32 | pptt(i)),
            (true, false) => keyed(cpuid),
            (false, _) => keyed(pptt),
        }
    };
    if cache_leaf || machine.pptt.is_some() {
        add("L2", both(&core_key, &core_key));
        add(
            "last-level cache",
            both(&|i| u64::from(ids[i] >> package_at), &pptt_l3),
        );
    }
    if leaf_1f && g.dies > 1 {
        add("die", keyed(&|i| u64::from(ids[i] >> die_at)));
    }
    add("package", keyed(&|i| u64::from(ids[i] >> package_at)));
    add("node", keyed(&|i| u64::from(machine.nodes[i])));
    let count = |groups: &[usize]| (0..n).filter(|&i| groups[i] == i).count();
    // Narrowest first; a stable sort keeps the names in the kernel's order within one size.
    levels.sort_by_key(|(_, groups)| std::cmp::Reverse(count(groups)));
    let mut merged: Vec<(Vec<&str>, Vec<usize>)> = Vec::new();
    for (names, groups) in levels {
        match merged.last_mut() {
            Some((top, same)) if *same == groups => top.extend(names),
            _ => merged.push((names, groups)),
        }
    }
    let order = ["core", "L2", "last-level cache", "die", "package", "node"];
    let lines: Vec<String> = merged
        .into_iter()
        .filter(|(_, groups)| n == 1 || count(groups) != n)
        .map(|(mut names, groups)| {
            names.sort_by_key(|name| order.iter().position(|o| o == name));
            let mut members: Vec<Vec<usize>> = Vec::new();
            for i in 0..n {
                match members.iter_mut().find(|m| groups[m[0]] == groups[i]) {
                    Some(m) => m.push(i),
                    None => members.push(vec![i]),
                }
            }
            let size = members[0].len();
            let shape = if members.iter().all(|m| m.len() == size) {
                format!("{}x{size}", members.len())
            } else {
                format!("{} groups", members.len())
            };
            let ranges: Vec<String> = members.iter().map(|m| index_ranges(m)).collect();
            format!("{} {shape} [{}]", names.join(", "), ranges.join("|"))
        })
        .collect();
    format!(
        "origin-kernel: domains: {n} processors: {}",
        lines.join("; ")
    )
}

/// `0-3,8-11`: ascending indexes as ranges.
fn index_ranges(sorted: &[usize]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        while i + 1 < sorted.len() && sorted[i + 1] == sorted[i] + 1 {
            i += 1;
        }
        out.push(if sorted[i] == start {
            start.to_string()
        } else {
            format!("{start}-{}", sorted[i])
        });
        i += 1;
    }
    out.join(",")
}

/// The scheduling-domains proof's lines (TASKS item 97): the tree and where its claims came from,
/// exactly under QEMU's software processor and only their starts under KVM; that nothing
/// disagreed; and every processor at capacity 1024 under QEMU's software processor (TASKS item 106:
/// no source reports every processor there). Under KVM the host's processor decides the
/// guest's cache leaf, and how QEMU encodes `SMP_CACHE` there, or passes the host's own leaves
/// with `-cpu host`, has not been measured: until a CI run shows the trees, the tree line is held
/// to its start there (the review's R97-16), and a difference shows in CI's log, not as a failure.
fn domain_markers(machine: &Machine, settings: &Settings, leaf_1f: bool) -> Vec<String> {
    let n = machine.cpus();
    let leaf = cache_leaf(settings);
    let tree = domain_tree(machine, leaf.is_some(), leaf_1f);
    let host = settings.cpu.split(',').next() == Some("host");
    let mut nodes: Vec<u32> = machine.nodes.clone();
    nodes.sort_unstable();
    nodes.dedup();
    // Two nodes at distance 21 (`-numa dist`), the ACPI machine's third holding no processor.
    let order: Vec<String> = nodes
        .iter()
        .map(|&from| {
            let others: Vec<String> = nodes
                .iter()
                .filter(|&&to| to != from)
                .map(u32::to_string)
                .collect();
            format!("{from} [{}]", others.join(","))
        })
        .collect();
    let pptt = match machine.pptt {
        None => "PPTT none".to_string(),
        Some(plan) => {
            let l3s = match plan.l3 {
                L3::Package => machine.geometry.packages,
                L3::Node => nodes.len() as u32,
            };
            format!(
                "PPTT {} caches, {} with IDs",
                3 + l3s,
                if plan.cache_ids { l3s } else { 0 }
            )
        }
    };
    let from = format!(
        "origin-kernel: domains: from: caches by {} on {n} of {n}; {pptt}; nodes by the SRAT, nearest first: {}",
        leaf.unwrap_or("no cache leaf"),
        order.join(", ")
    );
    vec![
        // R97-16 (the coordinating session's ruling of 2026-11-21): under KVM the tree line is held
        // to its start; no KVM tree has been measured yet. The findings and capacity lines below
        // stay exact under KVM, and every line is exact under QEMU's software processor.
        if host || settings.accel == Accel::Kvm {
            format!("origin-kernel: domains: {n} processors: ")
        } else {
            tree
        },
        if settings.accel == Accel::Tcg {
            from
        } else {
            "origin-kernel: domains: from: caches by ".into()
        },
        "origin-kernel: domains: 0 claims cut, 0 differing from the PPTT, 0 naming another cache level than CPUID, 0 in no node, 0 recorded nothing; levels crossing: none".into(),
        // TASKS item 106: under QEMU's software processor no source reports every processor, so
        // every capacity is 1024 (source 5). Under KVM the host's leaf 06H decides, which no CI run
        // has shown yet: held to its start there, as the tree line is.
        if settings.accel == Accel::Tcg {
            format!(
                "origin-kernel: domains: capacity 1024 on {n} of {n} processors at boot (ADR-0020 S9 source 5: no source reports every processor); consistent true"
            )
        } else {
            "origin-kernel: domains: capacity ".into()
        },
    ]
}

/// The hardware capacity proof's lines (TASKS item 106). Exact under QEMU 11.1.3's software
/// processor, which reports the thermal registers (CPUID.01H:EDX\[22\]) and a thermal LVT entry,
/// masked, but no thermal sensor, HFI, HWP or AMD CPPC, so no source wants the LVT and only the
/// absence paths run; its tables have no `_CPC`. The round's vector is taken on every processor,
/// and the MSR probe recovers the #GP of a reserved x2APIC address. Under KVM the host's processor
/// decides what CPUID reports, which no CI run has shown yet: the lines are held to their starts,
/// the probe's too, and the round's processors exactly.
fn capacity_markers(machine: &Machine, settings: &Settings) -> Vec<String> {
    let n = machine.cpus();
    let probe = "origin-kernel: capacity: MSR probe: RDMSR of the reserved x2APIC address 0x80e raised #GP and was recovered";
    if settings.accel == Accel::Kvm {
        return vec![
            "origin-kernel: capacity: HFI on ".into(),
            "origin-kernel: capacity: thermal LVT free on ".into(),
            "origin-kernel: capacity: HWP on ".into(),
            "origin-kernel: capacity: _CPC not read in the kernel (ADR-0019 A2); in QEMU's tables on 0 of ".into(),
            // Whether KVM faults on 80EH has not been seen: "not run" there is no failure (the
            // coordinating session's ruling of 2026-11-22), so only the line's start is held.
            "origin-kernel: capacity: MSR probe: ".into(),
            "origin-kernel: capacity: the round waited ".into(),
            format!(
                "origin-kernel: capacity: thermal vector 0x44 sent to {n} of {n} processors, taken by {n}, "
            ),
        ];
    }
    let packages = machine.geometry.packages;
    vec![
        format!(
            "origin-kernel: capacity: HFI on 0 of {n} processors, 0 reading a row of a table in use (0 past their table); packages: {packages}, taken 0, HFI enabled 0, written 0, enabled by the firmware 0, disagreeing 0, an LVT the firmware's 0"
        ),
        format!(
            "origin-kernel: capacity: thermal LVT free on {n} of {n}, the firmware's on 0, taken on 0 (0 with other enables the firmware set; CPUID.01H:EDX[22] on {n}, with a sensor on 0); heat: not supported: no thermal sensor; AMD heat not supported at M3"
        ),
        format!(
            "origin-kernel: capacity: HWP on 0 of {n}, on from the firmware on 0, Highest-change interrupt on 0; AMD CPPC on 0 of {n}, on from the firmware on 0; neither is enabled here (power management's, M10)"
        ),
        "origin-kernel: capacity: _CPC not read in the kernel (ADR-0019 A2); in QEMU's tables on 0 of ".into(),
        probe.into(),
        "origin-kernel: capacity: the round waited ".into(),
        format!(
            "origin-kernel: capacity: thermal vector 0x44 sent to {n} of {n} processors, taken by {n}, 0 with a source, 0 quiet; 0 packages asked to enable the HFI, 0 answered; 0 table updates; processors with a register that faulted 0, with an enable not read back 0; source 5, capacity 1024 on {n} of {n}; consistent true"
        ),
    ]
}

/// The kernel boot on the machine `settings` describe, with its markers for that machine.
fn boot_scenario(settings: &Settings) -> Scenario {
    let machine = machine(settings);
    let mut scenario = boot_scenario_markers(&machine, settings);
    // Each AP's start-up line comes after the MADT line and the per-CPU line that follows it
    // (ADR-0008 Amendment 1, D3), in index order.
    if let Some(at) = scenario
        .markers
        .iter()
        .position(|m| m.starts_with("; kernel stacks' region: room for "))
        .or_else(|| {
            scenario
                .markers
                .iter()
                .position(|m| m.starts_with("origin-kernel: ACPI MADT: "))
        })
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
    if settings.iommu == Iommu::Amd {
        amd_iommu_markers(&mut scenario, &machine, settings);
    }
    if settings.accel == Accel::Kvm {
        kvm_markers(&mut scenario, settings.iommu != Iommu::Amd);
    } else if oversubscribed(settings) {
        // The processors share the host's cores, so the even-shares line may not come, and an
        // uneven share whose workers both ran is let pass (`oversubscribed_fair_share`).
        scenario
            .markers
            .retain(|m| !m.starts_with(EVEN_SHARES_LINE));
    }
    if settings.bridge {
        bridge_markers(&mut scenario, settings);
    }
    if settings.reserved {
        reserved_markers(&mut scenario, settings.iommu == Iommu::Amd);
    }
    if settings.shape.is_on() {
        rented_scenario(&mut scenario, settings.shape);
    }
    if settings.machine_lacks("pit") {
        no_pit_markers(&mut scenario);
    }
    if settings.machine_lacks("i8042") {
        no_i8042_markers(&mut scenario);
    }
    match (settings.cpu_lacks("rdseed"), settings.cpu_lacks("rdrand")) {
        (true, false) => rdrand_only_markers(&mut scenario),
        (true, true) => no_generator_markers(&mut scenario),
        _ => {}
    }
    cpu_identity_marker(&mut scenario, settings);
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
        // The root is sized whatever the generator (ADR-0005 G5).
        "origin-kernel: kernel bound: the root holds ".into(),
        " KiB kept for the kernel's own use; a second sizing is refused and changes nothing: true".into(),
        "origin-kernel: start: randomness is withheld, so no process is launched and no mapping is placed at a random address: the process-start, Rust-program, origin.core, entry-flags, service-directory and memory-object proofs did not run".into(),
        "origin-kernel: kernel stacks: the region's base is 2 MiB-aligned with the whole region in PML4 slots 384-447: true; the loader handed over no random value: true, and the region is at its fixed base: true".into(),
    ];
    // The bound's proof refuses a launch before any place is drawn, so it runs here too; only
    // the run of the program to its end is left out (ADR-0005 G5).
    scenario.markers.extend(kernel_bound_markers(false));
    scenario.markers.extend([
        // The network asks for no lease: a transaction ID must not be guessable.
        "; DHCP lease 0.0.0.0 (".into(),
        "; no random value for a transaction ID or port; consistent false: virtio-net failed"
            .into(),
        "origin-kernel: boot verdict: degraded: ".into(),
        "; 6 did not run; failed: ".into(),
        "random-number generator (no randomness)".into(),
        HALT_LINE.into(),
    ]);
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

/// Whether this boot is one of the oversubscribed machines, named, whose scheduler proof's fair
/// share is not judged ([`oversubscribed_fair_share`]): under QEMU's software processor,
/// `boot-matrix`'s 65-processor machine (`ORIGIN_QEMU_SMP=65`), the 67-processor sparse machine
/// (`ORIGIN_QEMU_TOPOLOGY=sparse`) and the 288-processor run (`many`). Each guest processor is a
/// host thread, so on those the two workers of one guest processor get the shares the host gives
/// QEMU's threads. The machines are named, never found from the host's threads (review finding
/// F9 of 2026-11-14; decision item 308, ADR-0013 Amendment 2): every other machine, those of 1
/// to 4 processors and `boot-x2apic` among them, is held to a fair share on every host. Since
/// TASKS item 61 the machine is the boot's typed settings, not the environment.
fn oversubscribed(settings: &Settings) -> bool {
    let host = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    oversubscribed_for(settings.accel, settings.topology, host)
}

/// [`oversubscribed`] for an accelerator, a topology (the plain machine's with its processor
/// count) and the host's threads. Decision item 308 keeps the host's threads out of the answer:
/// they are taken only so that the host tests can show that they change nothing.
fn oversubscribed_for(accel: Accel, topology: Topology, _host_threads: usize) -> bool {
    accel == Accel::Tcg
        && matches!(
            topology,
            Topology::Sparse | Topology::Many | Topology::Plain(65)
        )
}

/// The two workers' counts on a scheduler proof's line that an oversubscribed boot lets pass
/// (the coordinating session's ruling of 2026-11-14, TASKS item 58; ADR-0013): the workers both
/// ran, for enough rounds (at least 15 switches), whatever their shares. On such a boot the share
/// measures how the host schedules QEMU's threads, not the kernel, so fairness is not shown
/// there; preemption, which the two workers running and switching show, is still held.
fn oversubscribed_fair_share(line: &str) -> Option<(u64, u64)> {
    if !line.starts_with("origin-kernel: scheduler: CPU ")
        || !line.ends_with(" context switches: scheduler failed")
    {
        return None;
    }
    let (_, counts) = line.split_once(" counted ")?;
    let first = number_before(counts, " and ")?;
    let second = number_before(counts, ", ")?;
    let switches = number_before(counts, " context switches")?;
    (first > 0 && second > 0 && switches >= 15).then_some((first, second))
}

/// Whether the checker lets `line` pass on a boot under `accel`: KVM's allowances, and on an
/// oversubscribed boot an uneven fair share whose workers both ran.
fn checker_forgives(line: &str, accel: Accel, oversubscribed: bool) -> bool {
    (accel == Accel::Kvm && kvm_forgives(line))
        || (oversubscribed && oversubscribed_fair_share(line).is_some())
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

/// What the kernel says when no firmware-reserved range is identity-mapped for the NVMe
/// controller: QEMU's own tables name none.
const NO_RESERVED_LINE: &str = "origin-kernel: IOMMU: reserved memory: none is identity-mapped for the NVMe controller (0 range(s) named by the firmware, 0 refused), so the proofs through it did not run";

/// The `boot-reserved` machine (`ORIGIN_QEMU_RESERVED=on`; TASKS item 40): the TEST ONLY
/// application plants [`reserved_regions`] before the loader starts, says where, and the kernel
/// reports each range it was given, with its verdict, and proves each page identity-mapped for
/// the NVMe controller. Which page is where depends on the firmware's allocations, so the
/// proof's lines are counted by [`check_reserved_boot`], not ordered here.
fn reserved_markers(scenario: &mut Scenario, amd: bool) {
    let fixture: Vec<String> = if amd {
        vec![
            "origin-test-reserved: TEST ONLY: planting reserved memory in the firmware's tables",
            "origin-test-reserved: IVMD type 0x21 flags 0b0111 0x",
            "(reserved pages) for DeviceID 0x0010",
            "origin-test-reserved: IVMD type 0x21 flags 0b0011 0x",
            "(reserved pages) for DeviceID 0x0010",
            "origin-test-reserved: IVMD type 0x20 flags 0b1000 0x",
            "(reserved pages) for DeviceID 0x0000",
            "origin-test-reserved: IVMD type 0x22 flags 0b0000 0x",
            "(reserved pages) for DeviceID 0x0010",
            "origin-test-reserved: IVMD type 0x21 flags 0b0111 0x",
            "(loader-data pages) for DeviceID 0x00fe",
            "origin-test-reserved: IVMD type 0x21 flags 0b0111 0x",
            "(reserved pages without the test fill) for DeviceID 0x0010",
            "origin-test-reserved: IVMD type 0x21 flags 0b0111 0x",
            "(the table's own pages) for DeviceID 0x00fe",
            "origin-test-reserved: IVMD type 0x20 flags 0b0000 0x",
            "(reserved pages) for DeviceID 0x0000",
            "origin-test-reserved: the XSDT now names IVRS at 0x",
            "origin-test-reserved: starting the loader",
        ]
    } else {
        vec![
            "origin-test-reserved: TEST ONLY: planting reserved memory in the firmware's tables",
            "origin-test-reserved: RMRR 0x",
            "(reserved pages) for 00:02.0",
            "origin-test-reserved: RMRR 0x",
            "(loader-data pages) for 00:1f.6",
            "origin-test-reserved: RMRR 0xfee00000-0xfee00fff (a fixed address) for 00:1f.6",
            "origin-test-reserved: RMRR 0x",
            "(reserved pages without the test fill) for 00:02.0",
            "origin-test-reserved: RMRR 0x",
            "(the table's own pages) for 00:1f.6",
            "origin-test-reserved: the XSDT now names DMAR at 0x",
            "origin-test-reserved: starting the loader",
        ]
    }
    .into_iter()
    .map(String::from)
    .collect();
    scenario.markers.splice(0..0, fixture);
    let mapped = "in the domain of each device it names that a driver attaches";
    let refused_ram = "): refused (the memory map does not call 0x";
    let refused_lent =
        "): refused (it is writable and overlaps the firmware table the kernel holds at 0x";
    let (after, kernel): (&str, Vec<String>) = if amd {
        (
            "origin-kernel: IOMMU: IVRS revision 1, IVinfo 0x2801 (EFR support true, 40-bit physical and 0-bit virtual addresses, pre-boot DMA protection false); 1 IOMMU(s) in 2 IVHD(s), 8 memory definition(s)",
            vec![
                "origin-kernel: IOMMU: reserved memory 0x".into(),
                ", flags 0b0111; for DeviceID 0x0010 (00:02.0) in segment 0): identity-mapped, read and write, ".into(),
                "origin-kernel: IOMMU: reserved memory 0x".into(),
                ", flags 0b0011; for DeviceID 0x0010 (00:02.0) in segment 0): identity-mapped, read only, ".into(),
                "origin-kernel: IOMMU: reserved memory 0x".into(),
                " (IVMD type 0x20 at offset 0x".into(),
                format!(", flags 0b1000; every device of the IOMMU at 0xfed80000): identity-mapped, read and write, {mapped}"),
                "origin-kernel: IOMMU: reserved memory 0x".into(),
                ", flags 0b0000; for DeviceIDs 0x0010-0x0018 in segment 0): never mapped for the devices it names".into(),
                "origin-kernel: IOMMU: reserved memory 0x".into(),
                format!(", flags 0b0111; for DeviceID 0x00fe (00:1f.6) in segment 0{refused_ram}"),
                " reserved or EFI runtime memory), so the devices it names cannot be attached".into(),
                "origin-kernel: IOMMU: reserved memory 0x".into(),
                ", flags 0b0111; for DeviceID 0x0010 (00:02.0) in segment 0): identity-mapped, read and write, ".into(),
                "origin-kernel: IOMMU: reserved memory 0x".into(),
                format!(", flags 0b0111; for DeviceID 0x00fe (00:1f.6) in segment 0{refused_lent}"),
                "origin-kernel: IOMMU: reserved memory 0x".into(),
                " (IVMD type 0x20 at offset 0x".into(),
                ", flags 0b0000; every device in the system, on every IOMMU): never mapped for the devices it names".into(),
            ],
        )
    } else {
        (
            "every device's DMA faults until its driver attaches it; 0 stale fault(s) cleared",
            vec![
                "origin-kernel: IOMMU: reserved memory 0x".into(),
                format!("; for 00:02.0): identity-mapped, read and write, {mapped}"),
                "origin-kernel: IOMMU: reserved memory 0x".into(),
                format!("; for 00:1f.6{refused_ram}"),
                " reserved or EFI runtime memory), so the devices it names cannot be attached".into(),
                "origin-kernel: IOMMU: reserved memory 0xfee00000-0xfee00fff (RMRR at offset 0x".into(),
                "; for 00:1f.6): refused (it overlaps the interrupt range), so the devices it names cannot be attached".into(),
                "origin-kernel: IOMMU: reserved memory 0x".into(),
                format!("; for 00:02.0): identity-mapped, read and write, {mapped}"),
                "origin-kernel: IOMMU: reserved memory 0x".into(),
                format!("; for 00:1f.6{refused_lent}"),
            ],
        )
    };
    if amd {
        let old = "origin-kernel: IOMMU: IVRS revision 1, IVinfo 0x2801 (EFR support true, 40-bit physical and 0-bit virtual addresses, pre-boot DMA protection false); 1 IOMMU(s) in 2 IVHD(s), 0 memory definition(s)";
        match scenario.markers.iter().position(|m| m == old) {
            Some(i) => scenario.markers[i] = after.into(),
            None => scenario
                .markers
                .push(format!("xtask: the marker {old:?} is missing")),
        }
    }
    match scenario.markers.iter().position(|m| m == after) {
        Some(i) => {
            scenario.markers.splice(i + 1..i + 1, kernel);
        }
        None => scenario
            .markers
            .push(format!("xtask: the marker {after:?} is missing")),
    }
    match scenario.markers.iter().position(|m| m == NO_RESERVED_LINE) {
        Some(i) => {
            scenario.markers[i] = "origin-kernel: IOMMU: reserved memory proof: page 0x".into()
        }
        None => scenario
            .markers
            .push(format!("xtask: the marker {NO_RESERVED_LINE:?} is missing")),
    }
    scenario.check = Some(check_reserved_boot);
}

/// The `boot-reserved` machine's whole-log check, after the kernel boot's: every range the
/// application planted reached the kernel at the same addresses (the kernel read the table
/// the firmware gave), and the proof's lines are the ones [`reserved_regions`] calls for.
fn check_reserved_boot(done: &Completed, settings: &Settings) -> Result {
    check_kernel_boot(done, settings)?;
    let amd = settings.iommu == Iommu::Amd;
    let planted: Vec<&str> = done
        .serial
        .lines()
        .filter_map(|l| l.strip_prefix("origin-test-reserved: "))
        .filter_map(|l| {
            l.split_whitespace()
                .find(|t| t.starts_with("0x") && t.contains('-'))
        })
        .collect();
    let want = reserved_regions(amd).len();
    if planted.len() != want {
        return Err(format!(
            "the application reported {} planted ranges, not {want}",
            planted.len()
        ));
    }
    for span in &planted {
        let line = format!("origin-kernel: IOMMU: reserved memory {span} (");
        if !done.serial.contains(&line) {
            return Err(format!(
                "the kernel did not report the planted range {span}"
            ));
        }
    }
    // The range over the table copy is refused for the copy the kernel holds, at its address
    // (M1): the one the application put in the XSDT.
    let copy = done
        .serial
        .lines()
        .find_map(|l| l.strip_prefix("origin-test-reserved: the XSDT now names "))
        .and_then(|l| l.split_whitespace().nth(2))
        .ok_or("the application did not say where its table copy is")?;
    let held = format!("overlaps the firmware table the kernel holds at {copy})");
    if !done.serial.contains(&held) {
        return Err(format!(
            "the kernel did not refuse the range over the table copy at {copy} as a table it holds"
        ));
    }
    let proofs: Vec<&str> = done
        .serial
        .lines()
        .filter(|l| l.contains("origin-kernel: IOMMU: reserved memory proof: page 0x"))
        .collect();
    let count = |access: &str, write: &str, fill: &str, others: &str| {
        proofs
            .iter()
            .filter(|l| {
                l.contains(&format!("({access}, as the firmware's bits give: true) in the NVMe controller's domain maps to itself: true; the controller's write of LBA 2048 into it {write}: true; its read of it to LBA 24576 carried what the page holds: true; {fill}; other domains: {others}; consistent true"))
            })
            .count()
    };
    let landed = "landed there";
    let filled = "the page held the test step's fill";
    let alone = "00:03.0 does not map it at any address";
    // The page without the test fill: written by nothing, and left as it was (M2).
    let left = |count: &dyn Fn(&str, &str, &str, &str) -> usize| {
        count(
            "read and write",
            "was not run, as the page does not hold the test step's fill",
            "the page is unchanged: true",
            alone,
        )
    };
    let (expected, found) = if amd {
        (
            [1, 1, 1, 1],
            [
                count("read and write", landed, filled, alone),
                count("read only", "was blocked and recorded", filled, alone),
                count(
                    "read and write",
                    landed,
                    filled,
                    "00:03.0 maps it too, as the firmware names it",
                ),
                left(&count),
            ],
        )
    } else {
        (
            [2, 0, 0, 1],
            [
                count("read and write", landed, filled, alone),
                0,
                0,
                left(&count),
            ],
        )
    };
    if found != expected || proofs.len() != expected.iter().sum::<usize>() {
        return Err(format!(
            "the reserved-memory proof's lines: {found:?} of the kinds expected {expected:?}, {} in all",
            proofs.len()
        ));
    }
    Ok(())
}

/// The `boot-bridge` machine (`ORIGIN_QEMU_BRIDGE=on`): the NIC at 01:00.0 behind a PCIe root
/// port at 00:04.0, one more function than the default machine.
fn bridge_markers(scenario: &mut Scenario, settings: &Settings) {
    let amd = settings.iommu == Iommu::Amd;
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

/// What the kernel reports about its processors' x2APIC IDs: for each one above 255 with an
/// NVMe queue of its own, its interrupts reach it through an IRTE holding its full ID (MSI-X
/// entry `2 + k`, `k` its place in [`queue_order`], which puts them first: review finding F1 of
/// 2026-11-14) and wake its threads; with none above 255, that there is nothing to prove.
fn x2apic_markers(machine: &Machine, settings: &Settings) -> Vec<String> {
    let queued = nvme_queued_for(machine.cpus(), settings.nvme_pairs);
    let all_wide = machine.apic_ids.iter().filter(|&&id| id > 0xFF).count();
    let wide: Vec<(u32, u32, u32)> = queue_order(machine)
        .into_iter()
        .take(queued as usize)
        .zip(0..)
        .filter_map(|(index, k)| {
            let id = machine.apic_ids.get(index as usize).copied()?;
            (id > 0xFF).then_some((k, index, id))
        })
        .collect();
    if wide.is_empty() && all_wide > 0 {
        // ADR-0008 Amendment 1, D6: none of them has an NVMe queue of its own.
        return vec![format!(
            "origin-kernel: x2APIC: {all_wide} online processors have an APIC ID above 255 and no NVMe queue of their own (ADR-0008 Amendment 1, D6): nothing to prove through NVMe"
        )];
    }
    if wide.is_empty() {
        let largest = machine.apic_ids.iter().copied().max().unwrap_or(0);
        return vec![format!(
            "origin-kernel: x2APIC: no online processor has an APIC ID above 255 (the largest is {largest})"
        )];
    }
    wide.into_iter()
        .flat_map(|(k, index, id)| {
            [
                format!(
                    "origin-kernel: x2APIC: {} is above 255, which a compatibility-format MSI address cannot carry (refused true); its NVMe MSI-X entry {} holds only remappable handle ",
                    machine.named(index),
                    2 + k
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
fn amd_iommu_markers(scenario: &mut Scenario, machine: &Machine, settings: &Settings) {
    let cpus = machine.cpus();
    let queued = nvme_queued_for(cpus, settings.nvme_pairs);
    let swaps: [(String, Vec<String>); 14] = [
        // The ACPI census names the IOMMU's table (TASKS item 62).
        (
            "origin-kernel: acpi: DMAR rev ".into(),
            vec!["origin-kernel: acpi: IVRS rev ".into()],
        ),
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
                3 + queued,
                3 + queued,
                1 + queued
            ),
            vec![format!(
                "origin-kernel: IOMMU: interrupt remapping: {} of {} MSI-X entries (NVMe {}, virtio-net 2) hold only an index into their device's own interrupt remapping table, whose IRTE holds the vector and the x2APIC destination",
                3 + queued,
                3 + queued,
                1 + queued
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

fn boot_scenario_markers(machine: &Machine, settings: &Settings) -> Scenario {
    use origin_version::{BUILD, PRODUCT, VERSION};
    let cpus = machine.cpus();
    let queued = nvme_queued_for(cpus, settings.nvme_pairs);
    // The processor the interrupt-driven NVMe and virtio-net threads run on: the first
    // one after the boot processor, if any.
    let target = machine.named(cpus.min(2) - 1);
    Scenario {
        quiet: None,
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
            // The SPCR's UART for the kernel's first line (ADR-0010 Amendment 3; TASKS item 84):
            // QEMU's tables have no SPCR.
            LOADER_NO_SPCR.into(),
            // What the walk cost (cost ledger row CL46).
            "origin-boot: console: the walk read ".into(),
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
            KERNEL_NO_LOADER_UART.into(),
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
            // ADR-0009 A2: the core generation and the declared scope; what it says depends
            // on the processor, so only its start is fixed (`boot-identity` fixes all of it).
            "origin-kernel: CPU identity: ".into(),
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
            // ADR-0008 Amendment 1, D3: every per-CPU table holds every index, sized at
            // discovery, the stacks' region has room, and nothing held a processor back.
            format!(
                "origin-kernel: CPU indexes: {cpus} given at discovery ({cpus} to start, 0 reserved for online-capable processors); {PER_CPU_TABLES} per-CPU tables hold {cpus} each, "
            ),
            format!(
                "; kernel stacks' region: room for {} processors, at most 262144 in its reserved address space; x2APIC IDs are 32-bit, 0 with the broadcast ID refused; held back: none",
                cpus.max(64)
            ),
            format!(
                "origin-kernel: SMP: {cpus} of {cpus} enabled processors online; trampoline identity pages removed true"
            ),
            // D8: one at a time, and the total printed.
            format!(
                "origin-kernel: SMP: application processors started one at a time (ADR-0008 C7, Amendment 1 D8): {} in ",
                cpus - 1
            ),
            "; 0 never sent INIT for want of memory".into(),
            // Every processor's interrupt stacks end at a guard page, and each processor set
            // its own control state (CLAUDE.md §24, §58).
            format!(
                "origin-kernel: page protection: interrupt-stack guard pages: {0} of {0} faulted (4 per processor)",
                4 * cpus
            ),
            format!(
                "origin-kernel: CPU state: machine checks raise #MC, x87 errors raise #MF, ring 3 has no RDPMC, FSGSBASE or SYSENTER, no debug breakpoint armed: {cpus} of {cpus} processors"
            ),
            // ADR-0020 S6 (TASKS item 94): every processor saves its FPU state as the boot
            // processor does, and so takes part in moves. The XCR0, the sizes and the area
            // depend on the processor model, so only the line's frame is fixed.
            "origin-kernel: FPU format: the boot processor's XCR0 ".into(),
            format!("; {cpus} of {cpus} processors alike, every one taking part in moves"),
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
                    &settings.cpu,
                    settings.accel,
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
        ])
        // The scheduling domains (ADR-0020 S8; TASKS item 97).
        .chain(domain_markers(
            machine,
            settings,
            machine.geometry.has_extended_levels()
                && guest_is_intel(&settings.cpu, settings.accel, host_is_intel()),
        ))
        .chain(acpi_markers(settings.acpi))
        .chain(measured_stack_markers())
        .chain(aml_markers(settings.acpi))
        .chain([
            // The random-number generator (ADR-0016): every processor has one, requests are
            // answered with values that differ, and its stack holds only zeros after a request.
            "origin-kernel: random: processor 0: ".into(),
            "; its generator seeded".into(),
            "origin-kernel: random: 16 of 16 values answered, all distinct true; 300 bytes in pieces true; with prediction resistance: ".into(),
            "origin-kernel: random: cost: a 64-byte request ".into(),
            "; only zeros on it after a request: true".into(),
            "origin-kernel: random: every processor has a generator true; consistent true".into(),
            // `origin.core` into its one set of frames, before any process exists (task S4-2;
            // ADR-0011 L2).
            "origin-kernel: origin.core: ".into(),
            " bytes of code, the stubs of 20 calls and nothing else, written once into 1 frames and read back as written, the rest of each page the linker's int3; every process pinned per call maps them read-execute (free frames fell by ".into(),
            // The kernel's root, sized once before the first process (ADR-0005 G5; task S4-3).
            "origin-kernel: kernel bound: the root holds ".into(),
            " KiB of free frames less a reserve of ".into(),
            " KiB kept for the kernel's own use; a second sizing is refused and changes nothing: true".into(),
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
            // A thread is created whole or not at all, and a refused one queues nothing (TASKS
            // item 75).
            "origin-kernel: scheduler: spawns refused for want of an FPU area (Err(Full)), at their publication (Err(Full)) and on a full queue (Err(Full)) give back".into(),
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
        .chain(core_pin_markers())
        .chain(directory_markers())
        .chain(memory_markers())
        .chain(events_ports_markers())
        .chain(stack_markers(machine))
        .chain([
            "origin-kernel: scheduler: exited threads' stacks reaped on their own CPUs: heap ".into(),
            // The framework's measured stack in a thread that yields on it (TASKS item 68), after
            // the comparison above (T68-5): the boot flow ran meanwhile, the thread's own stack
            // stayed in use (ruling A1), and the reap gave it back rather than leaking it (T68-1).
            "origin-kernel: measured stack: in a thread, 32 levels and a yield at the deepest: ".into(),
            "; switched away and back true after ".into(),
            " yields; its own stack in use meanwhile true; value right true; ended true; its own stack given back after the reap true (".into(),
            "); passed".into(),
        ])
        .chain(kernel_bound_markers(true))
        .chain([
            // ADR-0020 S7 (TASKS item 98): balancing was set up, on every machine. Without this
            // line no processor pulls, and with two or more processors the pulled phases below
            // fail (the review of TASKS item 98, R98-3).
            "origin-kernel: balancing: idle pull and idle kick set up: ".into(),
            // ADR-0020 S6, S20 and S23 (TASKS item 96): the two migration proofs, on every
            // machine; on one with fewer than two processors taking part, each says so and passes.
            // A failure ends its line ": scheduler failed", a boot failure below. Each has a second
            // phase, in which the idle processors pull its threads (TASKS item 98), with a line of
            // its own after the first phase's.
            "origin-kernel: scheduler: thread migration: integrity: ".into(),
            "origin-kernel: scheduler: thread migration: integrity, pulled: ".into(),
            "origin-kernel: scheduler: thread migration: delivery: ".into(),
            "origin-kernel: scheduler: thread migration: delivery, pulled: ".into(),
        ])
        .chain([
            // ADR-0017 Z3: the scheduler noted each change of process, with one barrier for
            // each where the processor has IBPB and none where it has not.
            "origin-kernel: ibpb: ".into(),
            format!(
                " changes of process on {cpus} processor{}, ",
                if cpus == 1 { "" } else { "s" }
            ),
        ])
        .chain(capacity_markers(machine, settings))
        .chain([
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
        // ADR-0008 Amendment 1, D6: a queue of its own while the controller has one left. The
        // proof is demanded on every configuration that gives one; only a controller limited to
        // the driver's two pairs (`ORIGIN_QEMU_NVME_PAIRS=2`) is expected to say "not proven".
        .chain(nvme_per_cpu_markers_for(machine, cpus, settings.nvme_pairs))
        // Processors above APIC ID 255: NVMe interrupts reach them live through IRTEs.
        .chain(x2apic_markers(machine, settings))
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
                3 + queued,
                3 + queued,
                1 + queued
            ),
            "APIC ID 300 accepted (its IRTE reads back destination 300), which compatibility format refuses: true; consistent true".into(),
            "origin-kernel: IOMMU: blocked interrupt: NVMe MSI-X vector 1 pointed at IRTE ".into(),
            "left not present: a read on I/O queue 2 completed true, 0 interrupts arrived; fault recorded: source ID 0x0010, interrupt index ".into(),
            "reason 0x22 (interrupt table entry not present); vector 1 restored; consistent true".into(),
            "origin-kernel: IOMMU: domains: 00:02.0 domain 1 (".into(),
            " 00:03.0 domain 2 (".into(),
            "no other function has a context entry, so its DMA faults; IOVAs from 0x40000000; faults recorded before the proofs: 0; consistent true".into(),
            // Firmware-reserved memory (ADR-0007 questions 4 and 8): QEMU's tables name none,
            // so nothing is proved here; the boot-reserved machine plants some.
            NO_RESERVED_LINE.into(),
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
        failures: qemu_boot_failures(),
        check: Some(check_kernel_boot),
        stderr: vec![],
        capture: Capture::Com1AndScreen,
        qemu_args: None,
    }
}

/// The census of the firmware's ACPI tables (TASKS item 62; ADR-0019 A6, area A), as QEMU 11.1.3
/// and OVMF give them on every machine here, in the XSDT's order: each table QEMU builds, named
/// and checked (or, for another specification's, its header checked), the FADT's line, the DSDT
/// and FACS it points to, and the census's verdict. OVMF's BGRT is not required: OVMF builds it
/// only when it has drawn its logo. The ACPI machine (`boot-acpi`) adds its SRAT's generic
/// initiator, its HMAT, its NVDIMM's SSDT and its NFIT, where its first boot (2026-11-14) showed
/// QEMU lists them. The markers follow what the machines showed; they are not assumed.
fn acpi_markers(acpi: bool) -> Vec<String> {
    let checked = "checked";
    let other = "another specification's: header checked";
    let mut markers = Vec::new();
    let mut table = |signature: &str, outcomes: &[&str]| {
        markers.push(format!("origin-kernel: acpi: {signature} rev "));
        markers.extend(outcomes.iter().map(|o| (*o).to_string()));
    };
    table("RSDT", &["checked: "]);
    table("XSDT", &["checked: "]);
    table("FACP", &[checked]);
    table("APIC", &["checked: "]);
    table("HPET", &[other]);
    // On the ACPI machine, its generic initiator (type 5) among the SRAT's structures.
    table(
        "SRAT",
        if acpi {
            &["checked: ", "5x1"]
        } else {
            &["checked: "]
        },
    );
    table("SLIT", &[checked]);
    if acpi {
        // QEMU's HMAT: a memory domain, two matrices (latency and bandwidth) and a cache.
        table("HMAT", &["checked: ", "0x", "1x2", "2x1"]);
    }
    table("MCFG", &[other]);
    // The AMD-Vi boot swaps it for the IVRS ([`amd_iommu_markers`]).
    table("DMAR", &[other]);
    if acpi {
        // The NVDIMM's SSDT, and the NFIT: its SPA range, region mapping and control region.
        table("SSDT", &[checked]);
        table("NFIT", &["checked: ", "0x1", "1x1", "4x1"]);
    }
    table("WAET", &[other]);
    markers.extend(
        [
            "origin-kernel: acpi: FADT: ",
            "origin-kernel: acpi: DSDT rev ",
            "checked",
            "origin-kernel: acpi: FACS version ",
            "checked",
            "origin-kernel: acpi: the RSDT's and XSDT's OEM table IDs are the FADT's: true",
            " tables: ",
            // Every QEMU machine here gives tables that break no rule the census reports.
            " refused, 0 reported, ",
            "; consistent true",
        ]
        .map(String::from),
    );
    markers
}

/// The boot proof of the framework's measured stack at boot (TASKS item 68), before the AML proof:
/// a known recursion's depth at 8 and 64 levels, measured and sampled; the kernel prints "passed"
/// only when the two are equal to the byte at both.
fn measured_stack_markers() -> Vec<String> {
    [
        "origin-kernel: measured stack: at boot, on 32768 bytes: 8 levels: ",
        " sampled, value right true; 64 levels: ",
        " sampled, value right true; ",
        " bytes a level; an empty closure ",
        " us; passed",
    ]
    .map(String::from)
    .into()
}

/// The boot proof of the AML interpreter (TASKS item 63, step B3; ADR-0019 A3), as QEMU 11.1.3's
/// tables give it on every machine here: the DSDT loaded (and on the ACPI machine, its NVDIMM's
/// SSDT after it), every named result as expected, every device that failed refused in the ways
/// the proof allows, the HPET's, the processors' and the links' refusals there, the host having
/// answered nothing but refusals and given the Global Lock back, and the heap back.
fn aml_markers(acpi: bool) -> Vec<String> {
    let mut markers: Vec<String> = vec![
        "origin-kernel: aml: DSDT rev ".into(),
        ": loaded as table 0".into(),
    ];
    if acpi {
        markers.push("origin-kernel: aml: SSDT rev ".into());
        markers.push(": loaded as table 1".into());
    }
    markers.extend(
        [
            "origin-kernel: aml: named results: ",
            "; all true",
            "origin-kernel: aml: walk: ",
            " 0 failed otherwise; HPET's _STA refused in SystemMemory true; ",
            " processors, each _STA refused in SystemIO true; ",
            " links (LNKx), each _STA refused in PCI_Config true",
            // TASKS item 106: QEMU's processors have no `_CPC` (M2).
            "origin-kernel: aml: walk: _CPC on 0 of ",
            " processors: 0 with an Integer Highest, 0 with a register (not read: ADR-0019 A2), 0 refused, 0 not read otherwise",
            "origin-kernel: aml: the host: ",
            "; answered otherwise 0; Global Lock taken ",
            "origin-kernel: aml: ",
            " bytes of AML loaded in ",
            "; named results true; heap back true; passed",
        ]
        .map(String::from),
    );
    markers
}

/// The kernel boot's whole-log checks: every line whole, and the screen.
fn check_kernel_boot(done: &Completed, settings: &Settings) -> Result {
    console_lines_intact(&done.serial, machine(settings).cpus() as usize)?;
    long_lines_whole(&done.serial)?;
    unwaited_lines_whole(&done.serial)?;
    check_memory(&done.serial, settings)?;
    check_screen(done)
}

/// The rented board's machine (`boot-rented`, TASKS item 83): the kernel boot's checks but the
/// screen's (this machine's monitor is not on stdio), the RAM, COM1 stopping at the console's
/// move, and on COM1, where the firmware's console is, the application's shaping and the
/// loader's lines in their order.
fn check_rented_boot(done: &Completed, settings: &Settings) -> Result {
    console_lines_intact(&done.serial, machine(settings).cpus() as usize)?;
    long_lines_whole(&done.serial)?;
    unwaited_lines_whole(&done.serial)?;
    check_memory(&done.serial, settings)?;
    check_spcr_console(done, settings)?;
    check_firmware_console(done, settings)
}

/// On COM1, where the firmware's console is, the application's shaping and the loader's lines
/// in their order ([`rented_firmware_markers`]).
fn check_firmware_console(done: &Completed, settings: &Settings) -> Result {
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    let firmware = rented_firmware_markers(settings);
    let markers: Vec<&str> = firmware.iter().map(String::as_str).collect();
    match serial::first_missing(&com1, &markers) {
        None => Ok(()),
        Some(missing) => Err(format!(
            "COM1, the firmware's console, lacks {missing:?} in its order (the application's shaping and the loader's lines)"
        )),
    }
}

/// What a machine larger than 512 MiB gives the kernel (TASKS item 83; the review's T83-1 to
/// T83-3), from four of the boot's own figures, each held against the RAM QEMU was given (`-m`)
/// less the pages `boot-rented`'s application reserves at the top ([`RENTED_SHAPE`]):
///
/// - the boot info's usable memory: at most that, and at least that less
///   [`FIRMWARE_ALLOWANCE_MIB`], which OVMF keeps whatever the size (59 to 61 MiB at 8 and
///   64 GiB);
/// - the frame allocator's free memory: at most the usable memory, and at least that less
///   [`ALLOCATOR_ALLOWANCE_MIB`], its bitmap and page-table pool (5 MiB at 64 GiB);
/// - the kernel's direct map: at least the usable memory, which it must cover, and at most the
///   RAM less the reserved top, which it must not map;
/// - the NUMA ranges' total, from the SRAT: within the firmware's allowance of the whole RAM.
///
/// A 512 MiB machine is not held to it: every machine before TASKS item 83 had that size.
fn check_memory(serial: &str, settings: &Settings) -> Result {
    if settings.memory == Memory::default() {
        return Ok(());
    }
    memory_figures(
        serial,
        settings.memory.mib().into(),
        reserved_top_mib(settings),
    )
}

/// MiB the firmware may keep of the RAM it was given ([`check_memory`]).
const FIRMWARE_ALLOWANCE_MIB: u64 = 128;
/// MiB the frame allocator may keep of the usable memory for its bitmap and page-table pool.
const ALLOCATOR_ALLOWANCE_MIB: u64 = 16;

/// MiB `boot-rented`'s application reserves at the top of the RAM, 0 on other machines.
fn reserved_top_mib(settings: &Settings) -> u64 {
    if matches!(settings.shape, Shape::Rented | Shape::KernelRefused) {
        (u64::from(RENTED_SHAPE.top_reserved_pages) * 4096) >> 20
    } else {
        0
    }
}

/// [`check_memory`]'s rules over `serial`, for `ram` MiB of which `reserved` are reserved at the top.
fn memory_figures(serial: &str, ram: u64, reserved: u64) -> Result {
    let figure = |prefix: &str, before: &str, after: &str| -> Result<u64> {
        let line = serial
            .lines()
            .find(|l| l.starts_with(prefix))
            .ok_or_else(|| format!("no line starting {prefix:?}"))?;
        line.split_once(before)
            .and_then(|(head, _)| head.rsplit(|c: char| !c.is_ascii_digit()).next())
            .filter(|n| !n.is_empty())
            .and_then(|n| n.parse::<u64>().ok())
            .filter(|_| line.contains(after))
            .ok_or_else(|| format!("{line:?} does not give its figure"))
    };
    let usable = figure("origin-kernel: boot info ", " MiB usable", " MiB usable")?;
    let free = figure(
        "origin-kernel: frame allocator: ",
        " MiB)",
        " free frames (",
    )?;
    let direct = figure(
        "origin-kernel: page tables loaded: ",
        " MiB direct map",
        " MiB direct map",
    )?;
    let numa = figure("origin-kernel: numa: ", " MiB; ", " memory ranges, ")?;
    let room = ram.saturating_sub(reserved);
    let mut wrong = Vec::new();
    if usable > room || usable + FIRMWARE_ALLOWANCE_MIB < room {
        wrong.push(format!(
            "the boot info's {usable} MiB usable is not within {FIRMWARE_ALLOWANCE_MIB} MiB below the {room} MiB the firmware can give"
        ));
    }
    if free > usable || free + ALLOCATOR_ALLOWANCE_MIB < usable {
        wrong.push(format!(
            "the frame allocator's {free} MiB free is not within {ALLOCATOR_ALLOWANCE_MIB} MiB below the {usable} MiB usable"
        ));
    }
    if direct < usable || direct > room {
        wrong.push(format!(
            "the {direct} MiB direct map does not cover the {usable} MiB usable within the {room} MiB the firmware can give"
        ));
    }
    if numa > ram || numa + FIRMWARE_ALLOWANCE_MIB < ram {
        wrong.push(format!(
            "the NUMA ranges' {numa} MiB is not within {FIRMWARE_ALLOWANCE_MIB} MiB of the machine's {ram} MiB"
        ));
    }
    if wrong.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the machine's {ram} MiB of RAM ({reserved} reserved at the top): {}",
            wrong.join("; ")
        ))
    }
}

/// The firmware console's lines on the rented board's machine, all on COM1: the application's
/// shaping (TASKS item 83), then the loader's lines up to its hand-over, as the kernel boot's
/// own markers have them. A hand-over line the kernel boot lacks makes an impossible marker, so
/// the boot fails rather than check less (the review's T83-8).
fn rented_firmware_markers(settings: &Settings) -> Vec<String> {
    let shaping = if settings.shape == Shape::SpcrRefused {
        [
            "origin-test-reserved: TEST ONLY: shaping the firmware's tables with the SPCR in RAM the kernel owns",
            "origin-test-reserved: the XSDT now names SPCR at 0x",
            " (loader data, RAM the kernel owns) in place of the firmware's at 0x",
            "origin-test-reserved: starting the loader",
        ]
        .map(String::from)
        .to_vec()
    } else {
        rented_shaping_markers(settings)
    };
    let mut markers = shaping;
    let plain = boot_scenario(&Settings {
        shape: Shape::Off,
        ..settings.clone()
    });
    match plain.markers.iter().position(|m| m == HANDOVER_LINE) {
        Some(at) => markers.extend(plain.markers.into_iter().take(at + 1)),
        None => markers.push(format!("xtask: the marker {HANDOVER_LINE:?} is missing")),
    }
    // This machine's SPCR names COM2, and the loader reads it under boot services, whatever the
    // kernel can read later (TASKS item 84).
    match markers.iter().position(|m| m == LOADER_NO_SPCR) {
        Some(at) => markers[at] = LOADER_NAMES_COM2.to_string(),
        None => markers.push(format!("xtask: the marker {LOADER_NO_SPCR:?} is missing")),
    }
    markers
}

/// The application's lines when it shapes the tables as rental B's board has them.
fn rented_shaping_markers(settings: &Settings) -> Vec<String> {
    [
        "origin-test-reserved: TEST ONLY: shaping the firmware's tables as rental B's board has them",
        "origin-test-reserved: reserved 0x",
        " (8192 pages) at the top of the RAM",
        "origin-test-reserved: the firmware's FADT names the FACS by FIRMWARE_CTRL 0x",
        "origin-test-reserved: the XSDT now names FACP at 0x",
        "by FIRMWARE_CTRL alone",
        "origin-test-reserved: FPDT at 0x",
        "in reserved pages",
        "origin-test-reserved: the XSDT is copied to 0x",
        " (ACPI NVS), ",
        "origin-test-reserved: the RSDT is copied to 0x",
        " (ACPI NVS), ",
        "origin-test-reserved: the RSDP is copied to 0x",
        if settings.shape == Shape::KernelRefused {
            " (loader data, RAM the kernel owns), and the configuration table names it"
        } else {
            " (ACPI NVS), and the configuration table names it"
        },
        "origin-test-reserved: starting the loader",
    ]
    .map(String::from)
    .to_vec()
}

/// The loader's last line, after which only the kernel prints.
const HANDOVER_LINE: &str = "origin-boot: exiting boot services; entering the Framekernel";

/// The kernel's line when it cannot read the SPCR the loader read: the RSDP's copy is in RAM it
/// owns (`boot-rented`'s machine with the kernel's reading refused; TASKS item 84).
const REFUSED_STAYS: &str = "origin-kernel: console: no usable ACPI SPCR table (cannot map an ACPI table); the UART stays at I/O port 0x3f8, and the loader's at I/O port 0x2f8";

/// `boot-rented`'s machine with the kernel's reading refused (TASKS item 84; rental B's case):
/// the loader reads the SPCR under boot services and names COM2; the kernel, which can read no
/// ACPI table from an RSDP in RAM it owns, refuses the move, says so on COM1 and COM2, and keeps
/// both for the rest of the boot (the coordinating session's ruling Q1). Without the ACPI tables
/// most of the boot's proofs cannot run, so this machine is held to the console alone: its
/// first lines, the refusal, a verdict and the halt, all on COM2 and on COM1, and the loader's
/// line naming COM2 on COM1 ([`check_refused_boot`]).
fn refused_scenario(scenario: &mut Scenario) {
    scenario.markers = vec![
        format!("origin-kernel: console: framebuffer {QEMU_DISPLAY} at 0x"),
        KERNEL_WRITES_COM2.to_string(),
        REFUSED_STAYS.to_string(),
        BOOT_VERDICT.to_string(),
        HALT_LINE.to_string(),
    ];
    // Every marker that fails a kernel boot, but the ones that need the ACPI tables, which the
    // kernel cannot read on this machine by design (TASKS item 84's review, T84-3).
    scenario.failures = qemu_boot_failures()
        .into_iter()
        .filter(|f| !NEEDS_THE_TABLES.contains(f))
        .collect();
    // QEMU's own lines that the boot's proofs cause: none run here that need the ACPI tables.
    scenario.stderr = vec![];
    scenario.check = Some(check_refused_boot);
}

/// The failure markers [`refused_scenario`]'s boot prints because the kernel can read no ACPI
/// table: processor discovery, the topology, the tables' census, the AML interpreter, PCIe and
/// the IOMMU need them; and the console's two section proofs count the interrupt they make
/// pending through the I/O APIC, which only the MADT names (the screen's proof, which does not
/// touch a UART, fails the same way). The boot goes on to its verdict and halt.
const NEEDS_THE_TABLES: &[&str] = &[
    "origin-kernel: ACPI processor discovery failed",
    ": topology failed",
    ": acpi tables failed",
    "; aml failed",
    ": console failed",
    ": PCIe failed",
    ": IOMMU failed",
];

/// The proofs [`refused_scenario`]'s verdict may name as failed: those that need the ACPI tables,
/// and the speculation controls, which fail under QEMU on every machine (decision item 154).
const REFUSED_VERDICT_FAILED: &[&str] = &[
    "bus-master quiesce",
    "IOMMU bring-up",
    "application processors",
    "speculation controls",
    "CPU topology",
    "ACPI tables",
    "AML interpreter",
    "PCIe enumeration",
    "console UART sections",
    "console screen sections",
];

/// How the kernel's verdict line starts.
const BOOT_VERDICT: &str = "origin-kernel: boot verdict: ";

/// [`refused_scenario`]'s check: the UART the loader named carries the kernel's lines from the
/// first, as the boot's markers say; COM1, the firmware's console, carries the application's and
/// the loader's lines, and the kernel's to the end, the refusal and the verdict among them; and
/// neither UART was sent anything again.
fn check_refused_boot(done: &Completed, settings: &Settings) -> Result {
    check_both_uarts_kept(done, settings)?;
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    if let Some(missing) = serial::first_missing(&com1, &[REFUSED_STAYS, BOOT_VERDICT, HALT_LINE]) {
        return Err(format!(
            "COM1 lacks {missing:?}: the console must keep it beside the loader's UART when the move is refused"
        ));
    }
    let unexpected = verdict_failed_beyond(&done.serial, REFUSED_VERDICT_FAILED)?;
    if !unexpected.is_empty() {
        return Err(format!(
            "the verdict names proofs that do not need the ACPI tables as failed: {unexpected:?}"
        ));
    }
    Ok(())
}

/// A boot whose move was refused (ruling Q1): the UART the loader named carries the kernel's lines
/// from the first; COM1, the firmware's console, carries the application's and the loader's lines,
/// the loader's naming the same port as the kernel's line on that UART (T84-14); both UARTs carry
/// the same kernel lines, each once, from the first to the halt (T84-4); and neither was sent
/// anything again.
fn check_both_uarts_kept(done: &Completed, settings: &Settings) -> Result {
    check_first_line_on_the_uart(&done.serial)?;
    check_firmware_console(done, settings)?;
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    check_same_port_named(&com1, &done.serial)?;
    same_kernel_lines(&com1, &done.serial, None)?;
    if done.serial.contains(REPLAYED) || com1.contains(REPLAYED) {
        return Err(format!(
            "a UART was sent the earlier lines again ({REPLAYED:?}), though the console never moved"
        ));
    }
    Ok(())
}

/// The machine with the SPCR alone in RAM the kernel owns (T84-3): the boot's line checks on both
/// UARTs, its memory figures, [`check_both_uarts_kept`], the refused move keeping both UARTs, and
/// a verdict whose failed proofs one unreadable XSDT entry explains.
fn check_spcr_refused_boot(done: &Completed, settings: &Settings) -> Result {
    let cpus = machine(settings).cpus() as usize;
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    for (name, text) in [("the SPCR's UART", done.serial.as_str()), ("COM1", &com1)] {
        console_lines_intact(text, cpus).map_err(|e| format!("{name}: {e}"))?;
        long_lines_whole(text).map_err(|e| format!("{name}: {e}"))?;
        unwaited_lines_whole(text).map_err(|e| format!("{name}: {e}"))?;
    }
    check_memory(&done.serial, settings)?;
    check_both_uarts_kept(done, settings)?;
    let refused = done
        .serial
        .lines()
        .find(|l| l.starts_with(SPCR_REFUSED_PREFIX))
        .ok_or("no line about the refused SPCR")?;
    if !refused.ends_with(STAYS_WITH_THE_LOADERS) {
        return Err(format!(
            "the refused move does not keep both UARTs: {refused:?}"
        ));
    }
    let unexpected = verdict_failed_beyond(&done.serial, SPCR_REFUSED_VERDICT_FAILED)?;
    if !unexpected.is_empty() {
        return Err(format!(
            "the verdict names proofs as failed that one unreadable XSDT entry does not explain: {unexpected:?}"
        ));
    }
    Ok(())
}

/// The machine with the SPCR alone in RAM the kernel owns (TASKS item 84's review, T84-3): both
/// UARTs are written to for the whole boot, and every other table is read, so the console's
/// proofs run as on any machine. Its markers are the console's: every line of the kernel boot's
/// that starts `origin-kernel: console:`, in order, with the loader's UART and the refused move in
/// place of "none" and "no SPCR", then a verdict and the halt.
fn spcr_refused_scenario(scenario: &mut Scenario) {
    let console = "origin-kernel: console: ";
    let mut markers = Vec::new();
    for marker in scenario.markers.drain(..) {
        if marker == KERNEL_NO_LOADER_UART {
            markers.push(KERNEL_WRITES_COM2.to_string());
        } else if marker.starts_with("origin-kernel: console: no usable ACPI SPCR table") {
            markers.push(SPCR_REFUSED_PREFIX.to_string());
        } else if marker.starts_with(console) {
            markers.push(marker);
        }
    }
    markers.push(BOOT_VERDICT.to_string());
    markers.push(HALT_LINE.to_string());
    scenario.markers = markers;
    // Every marker that fails a kernel boot, but the four that one unreadable XSDT entry fails,
    // by the locator's rule that it may have been the table asked for (TASKS item 71). The
    // console's proofs are held, with both UARTs live (T84-3).
    scenario.failures = qemu_boot_failures()
        .into_iter()
        .filter(|f| !NEEDS_EVERY_ENTRY.contains(f))
        .collect();
    // The IOMMU's own QEMU lines: the IOMMU is not brought up without its tables.
    scenario.stderr = vec![];
    scenario.check = Some(check_spcr_refused_boot);
}

/// The failure markers [`spcr_refused_scenario`]'s boot prints because one XSDT entry, the SPCR's
/// copy, cannot be read: the tables' census, the AML interpreter, PCIe's reserved ranges and the
/// IOMMU each look for a table the entry might have been (TASKS item 71's locator), and refuse.
const NEEDS_EVERY_ENTRY: &[&str] = &[
    ": acpi tables failed",
    "; aml failed",
    ": PCIe failed",
    ": IOMMU failed",
];

/// The proofs [`spcr_refused_scenario`]'s verdict may name as failed: those, and the speculation
/// controls, which fail under QEMU on every machine (decision item 154).
const SPCR_REFUSED_VERDICT_FAILED: &[&str] = &[
    "IOMMU bring-up",
    "speculation controls",
    "ACPI tables",
    "AML interpreter",
    "PCIe enumeration",
];

/// The proofs `serial`'s verdict names as failed that are not in `allowed`.
fn verdict_failed_beyond<'a>(serial: &'a str, allowed: &[&str]) -> Result<Vec<&'a str>> {
    let verdict = serial
        .lines()
        .find(|l| l.starts_with(BOOT_VERDICT))
        .ok_or("no verdict on the SPCR's UART")?;
    Ok(verdict
        .split_once("failed: ")
        .map(|(_, list)| list.split(", ").collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|proof| proof.split(" (").next().unwrap_or(proof))
        .filter(|proof| !allowed.contains(proof))
        .collect())
}

/// How the line about a refused SPCR starts, whatever the reason (T84-3).
const SPCR_REFUSED_PREFIX: &str = "origin-kernel: console: no usable ACPI SPCR table (";
/// How a refused move ends its line when the loader's UART is kept with COM1 (ruling Q1).
const STAYS_WITH_THE_LOADERS: &str =
    "; the UART stays at I/O port 0x3f8, and the loader's at I/O port 0x2f8";

/// The kernel lines of `com1` and of `named` (the SPCR's UART) are the same, each once and in
/// order (T84-4): all of them when `upto` is `None`, or COM1's (which end with `upto`) against
/// `named`'s up to and including `upto`. A queue's notice that it lost lines is the only line
/// either may have alone, and none is expected.
fn same_kernel_lines(com1: &str, named: &str, upto: Option<&str>) -> Result {
    let named = serial::clean(named.as_bytes());
    let lost = "origin-kernel: console: some lines were lost here";
    let kernel = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|l| l.starts_with("origin-kernel:") && !l.starts_with(lost))
            .map(str::to_string)
            .collect()
    };
    let ours = kernel(com1);
    let mut theirs = kernel(&named);
    if let Some(end) = upto {
        let at = theirs
            .iter()
            .position(|l| l == end)
            .ok_or_else(|| format!("the SPCR's UART lacks {end:?}"))?;
        theirs.truncate(at + 1);
    }
    if ours == theirs {
        return Ok(());
    }
    let first = ours
        .iter()
        .zip(&theirs)
        .position(|(a, b)| a != b)
        .unwrap_or(ours.len().min(theirs.len()));
    Err(format!(
        "COM1 and the SPCR's UART do not carry the same kernel lines: {} and {} lines, first differing at line {}: {:?} against {:?}",
        ours.len(),
        theirs.len(),
        first + 1,
        ours.get(first),
        theirs.get(first)
    ))
}

/// The port the loader's line on COM1 names is the one the kernel's line on the SPCR's UART says
/// the loader named (T84-14): read from each line, not from xtask's constants.
fn check_same_port_named(com1: &str, named: &str) -> Result {
    let port_after = |text: &str, prefix: &str| -> Option<String> {
        let line = text.lines().find(|l| l.starts_with(prefix))?;
        let rest = line.split_once("I/O port ")?.1;
        Some(rest.split([' ', ':', ',', ')']).next()?.to_string())
    };
    let loader = port_after(
        com1,
        "origin-boot: console: the SPCR names a UART at I/O port ",
    );
    let kernel = port_after(
        named,
        "origin-kernel: console: the loader named the SPCR's UART at I/O port ",
    );
    match (loader, kernel) {
        (Some(a), Some(b)) if a == b => Ok(()),
        (a, b) => Err(format!(
            "the loader's line on COM1 names port {a:?}, and the kernel's line on the SPCR's UART says it named {b:?}"
        )),
    }
}

/// The kernel boot made the rented board's (TASKS item 83): the console's UART is the one the
/// SPCR names at `0x2F8`, with COM1 present and kept in a file, so the markers are read from
/// that UART, and they start with the kernel's lines that the move sends it again; the
/// firmware's console, the application's and the loader's lines, stays on COM1
/// ([`check_rented_boot`]). The move comes after the kernel's page-table proofs, and the census
/// names the FPDT's tables and what it never lends.
fn rented_scenario(scenario: &mut Scenario, shape: Shape) {
    scenario.capture = Capture::SpcrUart;
    scenario.qemu_args = Some(|work| spcr_args(work, &spcr_table(0, gas_io(SPCR_PORT), 7), &[]));
    if shape == Shape::KernelRefused {
        refused_scenario(scenario);
        return;
    }
    if shape == Shape::SpcrRefused {
        spcr_refused_scenario(scenario);
        return;
    }
    scenario.check = Some(if shape == Shape::SpcrRefused {
        check_spcr_refused_boot
    } else {
        check_rented_boot
    });
    // Each edit below needs its anchor: one that is missing makes an impossible marker, so the
    // boot fails rather than check less (the review's T83-8).
    let missing = |scenario: &mut Scenario, anchor: &str| {
        scenario
            .markers
            .push(format!("xtask: the marker {anchor:?} is missing"));
    };
    match scenario.markers.iter().position(|m| m == HANDOVER_LINE) {
        Some(at) => {
            scenario.markers.drain(..=at);
        }
        None => missing(scenario, HANDOVER_LINE),
    }
    // The usual machine has no SPCR; this one's names COM2, which the loader names and the
    // kernel writes to from its first line. The console moves there for good (below), or, with
    // the SPCR alone in RAM the kernel owns, the kernel refuses it and keeps both UARTs.
    let no_spcr = "origin-kernel: console: no usable ACPI SPCR table";
    match scenario.markers.iter().position(|m| m.starts_with(no_spcr)) {
        Some(at) if shape == Shape::SpcrRefused => {
            scenario.markers[at] = SPCR_REFUSED_PREFIX.to_string();
        }
        Some(at) => {
            scenario.markers.remove(at);
        }
        None => missing(scenario, no_spcr),
    }
    match scenario
        .markers
        .iter()
        .position(|m| m == KERNEL_NO_LOADER_UART)
    {
        Some(at) => scenario.markers[at] = KERNEL_WRITES_COM2.to_string(),
        None => missing(scenario, KERNEL_NO_LOADER_UART),
    }
    if shape == Shape::SpcrRefused {
        return;
    }
    let insert_before = |scenario: &mut Scenario, anchor: &str, items: &[&str]| match scenario
        .markers
        .iter()
        .position(|m| m.starts_with(anchor))
    {
        Some(at) => {
            scenario
                .markers
                .splice(at..at, items.iter().map(|m| (*m).to_string()));
        }
        None => missing(scenario, anchor),
    };
    insert_before(
        scenario,
        "origin-kernel: allocated, wrote and freed 3 frames at ",
        &[TAKEN_OVER_LINE, TAKEN_BYTES],
    );
    insert_before(
        scenario,
        "origin-kernel: acpi: FADT: ",
        &["origin-kernel: acpi: FPDT rev 1"],
    );
    insert_before(
        scenario,
        " tables: ",
        &[
            // The FPDT's tables, read after the FADT's, DSDT's and FACS's lines.
            "origin-kernel: acpi: FBPT, 56 bytes: checked: 1 records",
            "origin-kernel: acpi: S3PT, 32 bytes: checked: 1 records",
            "origin-kernel: acpi: never lent, as the firmware changes them after boot: FACS 0x",
            ", FBPT 0x",
            ", S3PT 0x",
            "; 3 kept",
        ],
    );
}

/// What fails a kernel boot under QEMU alone, and never a physical machine's (the review of TASKS 63's
/// B3 addendum, B3-18): on QEMU's tables the AML proof must run, so its line saying the tables are
/// not QEMU's ends the boot at once, not at the boot test's timeout on a missing marker. On a
/// physical machine that line is the right result, so a checker of a physical boot takes
/// [`boot_failures`] alone, never this list.
const QEMU_ONLY_FAILURES: &[&str] = &["are not QEMU's: the proof runs only"];

/// [`boot_failures`] and [`QEMU_ONLY_FAILURES`]: what xtask's QEMU scenarios fail on.
fn qemu_boot_failures() -> Vec<&'static str> {
    let mut failures = boot_failures();
    failures.extend_from_slice(QEMU_ONLY_FAILURES);
    failures
}

/// Output that fails any kernel boot the moment it appears, under QEMU or on a machine.
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
        // ADR-0020 S20 (TASKS item 96): moves withdrawn after an inconsistency, during the proofs
        // or after them. The kernel goes on, and the scenario fails, under QEMU or on a machine.
        "origin-kernel: scheduler: pinned mode:",
        ": topology failed",
        ": CPU baseline failed",
        ": numa failed",
        ": slit failed",
        ": acpi tables failed",
        "; aml failed",
        "measured stack failed",
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
        ": origin.core failed",
        "origin.core could not be written",
        ": entry flags failed",
        ": directory failed",
        ": memory failed",
        ": events and ports failed",
        ": user clock failed",
        ": kernel bound failed",
        ": console failed",
        ": TLB failed",
        ": address spaces failed",
        ": wait queues failed",
        // The timing half of the wait-queue proofs (Wait timing, a Report row; TASKS item 66).
        ": wait timing failed",
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
        // TASKS item 106: a register CPUID claims that faulted, an enable that did not read back, a
        // round not taken everywhere.
        ": hardware capacity failed",
    ]
}

/// The kernel's console, as QEMU's firmware sets it up: the mode, and the size of the first
/// console line it leads to.
const QEMU_DISPLAY: &str = "1280x800";
/// What the kernel says about the SPCR when the firmware has none (`console.rs`).
const NO_SPCR: &str =
    "no usable ACPI SPCR table (table not listed in the XSDT); the UART stays at I/O port 0x3f8";
/// What the loader says when the firmware has no SPCR (`boot/uefi/src/spcr.rs`; TASKS item 84).
const LOADER_NO_SPCR: &str = "origin-boot: console: no SPCR (the XSDT lists none); the kernel's console starts on COM1 and the screen";
/// What the kernel says when the loader named no UART (`console.rs`; TASKS item 84).
const KERNEL_NO_LOADER_UART: &str = "origin-kernel: console: the loader named no UART from the SPCR; COM1 and the screen until the SPCR is read";
/// Every scenario here boots the whole kernel, so it ends with this line.
const HALT_LINE: &str = "origin-kernel: M1 entry complete; halting";

/// How the line starts that the scheduler proof prints when every worker's share was even.
const EVEN_SHARES_LINE: &str = "origin-kernel: preemptive multitasking: ";

/// The stacks proof's last line, as far as its live count (`stacks::prove`), and the parts the
/// checker requires after it: nothing kept for reuse, retired or lost, and every count as before.
const STACKS_GIVEN_BACK: &str =
    "origin-kernel: kernel stacks: every stack the proof made is given back: Counts { live: ";
const STACKS_NONE_LOST: &str = ", idle: 0, retired: 0, lost: 0, pages: ";
const STACKS_AS_BEFORE: &str = " }, as before: true";

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
        quiet: None,
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
        quiet: None,
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
    // TASKS item 70: the switch windows, asked about from the boot processor while the first
    // processor after it holds the thread; not asked with one processor.
    let windows = if cpus > 1 {
        vec![
            format!(
                "origin-kernel: kernel stacks: a thread's own stack, asked about from {} while {reader} switched it out (its switch decided, the thread's frame not yet saved): in use, and covered by the table of contexts: true (held ",
                machine.named(0)
            ),
            " us); while it ended: the same: true (held ".into(),
            " us); given back by its processor's reap, not leaked: true (".into(),
        ]
    } else {
        vec![
            "origin-kernel: kernel stacks: a thread's own stack in its switch windows: not asked, with one processor".into(),
        ]
    };
    let mut markers = vec![
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
    ];
    markers.extend(windows);
    markers.extend([
        "origin-kernel: kernel stacks: 64 stacks of 16384 bytes made and freed: all made true; making one took ".into(),
        " (QEMU, no budget)".into(),
        STACKS_GIVEN_BACK.into(),
        STACKS_NONE_LOST.into(),
        STACKS_AS_BEFORE.into(),
    ]);
    markers
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
        "origin-kernel: rust program: first_program ran in ring 3 from the kernel's image at random bases, 6 times: each found its start record in RDI and read its header and 3 entries as written: true; ran at its base plus its entry: true; its tables mapped each page of its span as described, and its stack writable and never executable between two unmapped pages: true; every relocated slot held its value plus the base: true; read its data through relocated pointers: true; the runtime's mem functions copy, move, fill and compare: true; wrote on BOOTSTRAP, named as the writer: true; ended with its exit code: true".into(),
        "origin-kernel: rust program: two launches differ in base and stack: true; a write to its code faulted there: true; a jump into its data faulted there as a fetch: true; a write to its relocated read-only data faulted there: true; a panic ended it with the runtime's exit code: true; a launch refused before anything was made took nothing: true; a launch refused part-way through mapping, at the bound on its page tables, gave back every frame, table and heap byte it took: true".into(),
        "origin-kernel: rust program: loading and starting it took ".into(),
        "origin-kernel: rust program: every launched process and all they held are gone: heap ".into(),
    ]
}

/// `origin.core` and the per-call pin, and the flags at a thread's entry (task S4-2; ADR-0005 B2,
/// G1, H1; ADR-0011 L2, L3, L6 to L8, L13; `ipc_proof::prove_core_pin` and `prove_entry_flags`):
/// the kernel writes `origin.core` into its frames once, and the second program it carries, a
/// script interpreter in Rust, makes every call through it and is stopped where it must be.
fn core_pin_markers() -> Vec<String> {
    vec![
        "origin-kernel: core: the binding: before its thread starts, each of pin_program's ".into(),
        " import slots holds the address of its export in its own origin.core, on a page it cannot write: true; and each of first_program's ".into(),
        "origin-kernel: core: a Rust program made every one of origin.kernel's 20 calls through origin.core: 20 calls of its script, each reported through debug_report, each with the status it must give, then thread_exit: true".into(),
        "origin-kernel: core: the hostile scripts of \"IPC and handles\", made through the stubs, gave call for call the statuses they give directly, and each process exited by itself: ".into(),
        " IPC calls (the one call number no method has is left out: no stub can ask it): true; 49 memory calls: true".into(),
        "origin-kernel: core: one call through origin.core, as it is pinned: ".into(),
        "origin-kernel: core: the per-call pin: a syscall in the program's own code stopped the thread, its ending naming call 1 and the address the program reported for that instruction: true; a jump to debug_report's syscall with channel_create's number stopped it, naming call 5 and the stub's address: true; a number no stub has, 9999, stopped it there too: true; and debug_report's own number with bit 32 set, 0x100000001, there too: true".into(),
        "origin-kernel: core: the kernel's proof processes keep their range: a call from outside it was refused with a status and the process went on to its own fault: true; a call number no method has answered NOT_SUPPORTED: true".into(),
        "origin-kernel: core: origin.core in a process: a write to its page faulted there: true; memory_unmap and memory_protect of its range, a fixed memory_map over it, and a fixed memory_map of two pages from one page below it each answered INVALID_ARGS, and calls through it went on: true; its 1 pages were still mapped read-execute on the same frames afterwards: true; three processes, two of the pin program and one of the first, hold it on one set of frames, executable in ".into(),
        " spaces and writable in none: true, each at a base of its own: true; the kernel's own write path into a process's pages refused a page of it: true".into(),
        "origin-kernel: core: narrow parameters: through its stub, a call whose 32-bit parameter has bits set above it, in the second, third and fourth registers, is the same call with them clear, 4 calls: true; the same registers in a direct call by a proof process are each INVALID_ARGS: true".into(),
        "origin-kernel: core: every process it launched and all they held are gone: heap ".into(),
        "; origin.core's own frames stay, mapped by no process: true, and still hold the image word for word as it was written at boot: true".into(),
        "origin-kernel: entry flags: a program's first instruction pushed RFLAGS, ".into(),
        // Not the register's value: ADR-0005 H1 promises eight bits of it and no more, and the
        // kernel's proof judges those (the review of 2026-11-13, finding K11).
        ": the direction flag and the six status flags clear and interrupts on: true".into(),
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

/// The kernel-wide bound's proof (ADR-0005 G5; task S4-3; `ipc_proof::prove_kernel_bound`).
/// `generator`: whether the boot has a random-number generator, which the run of the program
/// to its end needs to place it.
fn kernel_bound_markers(generator: bool) -> Vec<String> {
    let mut markers: Vec<String> = vec![
        "origin-kernel: kernel bound: costs as this build measures them, in bytes: a process ".into(),
        "origin-kernel: kernel bound: after every earlier proof the root is back at its size, ".into(),
        " KiB: true; it cannot be sized again: true".into(),
        "origin-kernel: kernel bound: with all of the root but ".into(),
        " bytes held by a process that never runs, one byte short of what a launch of the first program pays, ".into(),
        " bytes: true; the launch is refused with LIMIT: true; nothing is left of it, the root, the heap in use, the free frames, the next process ID and the kernel stacks as they were: true".into(),
        "origin-kernel: kernel bound: with the holder gone the root has room again: true; a process with the program's limits takes their figure, ".into(),
        " bytes, to the byte and gives it back when dropped: true; the program launched and run to its end pays ".into(),
    ];
    markers.push(if generator {
        " bytes, each page it maps paid for, until it is gone, then gives everything back: true".into()
    } else {
        " bytes, each page it maps paid for, until it is gone, then gives everything back: not run, no generator".into()
    });
    markers.extend([
        "origin-kernel: kernel bound: a process whose channel outlives it keeps its ".into(),
        " bytes out of the root after the process object is dropped: true; they come back when the channel goes: true".into(),
        "origin-kernel: kernel bound: a process given no threads cannot start one: true; nothing is left of the refusal: true".into(),
        "origin-kernel: kernel bound: a process given one thread starts it: true; while it waits a second start is refused: true; nothing is left of the refusal: true; the first ends when let go: true".into(),
        "origin-kernel: kernel bound: a memory object another process made, mapped in one whose last thread has ended and is not yet reaped, keeps its maker's ".into(),
        " bytes out while the space that maps it stands: true; they come back when the last reap frees that space, before TERMINATED: true".into(),
        "origin-kernel: kernel bound: a port whose creator was given two bindings at once takes two from another process: true; a third is refused with LIMIT: true; nothing is left of it, the binder's charge and the port's armed count with the rest: true; once one fires it takes another: true; a port whose creator was given none refuses the first: true; the first creator paid beyond the second its port's index at two, to the byte: true".into(),
        "origin-kernel: kernel bound: after this proof the root is back at its size: true; the most of it the proofs before this one had paid out at once was ".into(),
    ]);
    markers
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
        quiet: None,
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
    // Nothing is built with compiler flags from the environment (`toolchain`). `exclusive`
    // builds nothing itself, and what it runs makes this check for itself.
    if let Some(command) = args.first()
        && command != "exclusive"
        && let Err(e) = toolchain::refuse_environment_flags()
    {
        eprintln!("xtask: error: {e}");
        return ExitCode::FAILURE;
    }
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["check"] => in_turn("check", check),
        ["unsafe-gate"] => unsafety::gate(),
        ["names-gate"] => listed_names::gate(),
        ["aml-alloc-gate"] => amlalloc::gate(),
        ["percpu-gate"] => per_cpu::gate(),
        ["settle-gate"] => settle::gate(),
        ["document-gate"] => documents::gate(),
        ["cfg-lint"] => linux_cfg_lint(),
        ["interface-gate"] => interfaces::gate(),
        ["host-tools-gate"] => host_tools::gate(),
        ["requirements-gate"] => requirements::gate(),
        ["provenance-gate"] => provenance::gate(),
        ["roadmap-gate"] => roadmap::gate(),
        ["progress"] => progress::write(),
        ["progress-gate"] => progress::gate(),
        ["cost-gate"] => costs::gate(),
        ["regenerate"] => interfaces::regenerate(),
        ["version"] => image_inputs::write_version_file(),
        ["version-gate"] => image_inputs::version_gate(),
        ["image-graph-gate"] => image_inputs::graph_gate(),
        ["linker-text-gate", files @ ..] => image_inputs::linker_text_gate(files),
        ["reproducible-compare", directory] => reproducible::compare(Path::new(directory)),
        ["image-gate"] => in_turn("image-gate", image::build_and_gate),
        ["core-gate"] => in_turn("core-gate", corestubs::gate),
        ["image-gate", elf] => image::gate(Path::new(elf)),
        ["build"] => in_turn("build", || {
            build_loader(Loader::Developer).and_then(|loader| {
                let kernel = pack_kernel(&build_kernel()?)?;
                println!("{}\n{}", loader.display(), kernel.display());
                Ok(())
            })
        }),
        ["boot-test"] => in_turn_with_settings("boot-test", boot_test),
        ["boot-matrix"] => in_turn_with_settings("boot-matrix", boot_matrix),
        ["boot-x2apic"] => in_turn_with_settings("boot-x2apic", boot_x2apic),
        ["boot-sparse"] => in_turn_with_settings("boot-sparse", boot_sparse),
        ["boot-many"] => in_turn_with_settings("boot-many", boot_many),
        ["boot-listed"] => in_turn_with_settings("boot-listed", boot_listed),
        ["boot-domains"] => in_turn_with_settings("boot-domains", boot_domains),
        ["boot-console"] => in_turn_with_settings("boot-console", boot_console),
        ["boot-bench"] => in_turn_with_settings("boot-bench", bench::boot),
        ["bench-build"] => in_turn("bench-build", || bench::build().map(|_| ())),
        ["bench-build", "--minimal"] => in_turn("bench-build --minimal", || {
            bench::build_minimal().map(|_| ())
        }),
        ["bench-build", "--bisect"] => {
            in_turn("bench-build --bisect", || bench::build_bisect().map(|_| ()))
        }
        ["bench-build", "--kernel"] => {
            in_turn("bench-build --kernel", || bench::build_kernel().map(|_| ()))
        }
        ["bench-gate"] => bench::gate(),
        ["stack-depth"] => in_turn_with_settings("stack-depth", stack_depth::run),
        ["boot-bridge"] => in_turn_with_settings("boot-bridge", boot_bridge),
        ["boot-reserved"] => in_turn_with_settings("boot-reserved", boot_reserved),
        ["boot-large"] => in_turn_with_settings("boot-large", boot_large),
        ["boot-rented"] => in_turn_with_settings("boot-rented", boot_rented),
        ["boot-legacy-free"] => in_turn_with_settings("boot-legacy-free", boot_legacy_free),
        ["boot-acpi"] => in_turn_with_settings("boot-acpi", boot_acpi),
        ["boot-random"] => in_turn_with_settings("boot-random", boot_random),
        ["boot-identity"] => in_turn_with_settings("boot-identity", boot_identity),
        ["run"] => in_turn_with_settings("run", run),
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

/// [`in_turn`] for a command that boots: the person's settings are read from the environment
/// once, here, and refused before the turn is taken if a value is not one they take; `run` gets
/// them as a typed value (TASKS item 61). For a command that sets each machine itself
/// ([`SETS_ITS_MACHINES`]), a person's topology, bridge, reserved-memory or ACPI machine is
/// refused too ([`Settings::refuse_machine`]).
fn in_turn_with_settings(command: &str, run: impl FnOnce(&Settings) -> Result) -> Result {
    let settings = entry_settings(command, Settings::from_env()?)?;
    in_turn(command, || run(&settings))
}

/// The settings boot command `command` runs with: the person's, refused if `command` sets each
/// machine itself and they name one ([`SETS_ITS_MACHINES`], [`Settings::refuse_machine`]).
fn entry_settings(command: &str, settings: Settings) -> Result<Settings> {
    if SETS_ITS_MACHINES.contains(&command) {
        settings.refuse_machine(command)?;
    }
    Ok(settings)
}

/// `boot-matrix` and its thirteen scenario commands: each sets every machine's topology, bridge,
/// reserved memory, ACPI tables, RAM and table shapes itself (the coordinating session's ruling R5
/// of 2026-11-19; TASKS item 83). The list is of names, so a new scenario command of
/// `boot-matrix`'s belongs here, by hand; the host test
/// `each_kind_of_command_takes_or_refuses_the_person_s_machine_at_entry` holds these fourteen.
const SETS_ITS_MACHINES: [&str; 14] = [
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
    "boot-domains",
    "boot-large",
    "boot-rented",
];

/// One machine `boot-matrix` or one of its scenario commands boots `boot-test` on: the line said
/// before it, the machine's settings, and the words its failure begins with. Each command's
/// machines are made from the person's settings by a function of its own, which starts nothing,
/// so the host tests compare each machine with the environment `boot-matrix` gave its child
/// before TASKS item 61.
#[derive(Debug)]
struct MachineRun {
    said: String,
    settings: Settings,
    failed: String,
}

/// Runs `boot-test` in this process on each machine of `runs`, in order, and stops at the first
/// that fails. Nothing here starts `xtask` again (ADR-0011 Amendment 4, T1; TASKS item 61).
fn boot_machines(runs: Vec<MachineRun>) -> Result {
    for run in runs {
        boot_machine(&run)?;
    }
    Ok(())
}

/// [`boot_machines`] for one machine.
fn boot_machine(run: &MachineRun) -> Result {
    println!("{}", run.said);
    boot_test(&run.settings).map_err(|e| format!("{} failed: {e}", run.failed))
}

/// The two IOMMUs the machines boot under, with the names `boot-matrix` prints for them.
fn iommus() -> [(&'static str, Iommu); 2] {
    [("intel", Iommu::Intel), ("amd", Iommu::Amd)]
}

/// The label of `boot-test`'s kernel boot, the only scenario most machines run.
const KERNEL_BOOT: &str = "kernel boot";

/// Runs `boot-test` in this process for each IOMMU and processor count, each with its own
/// settings made from the person's (`base`, which is never changed), and then [`boot_x2apic`] and
/// the other scenario commands (the owner's decision on ADR-0008, question 3).
fn boot_matrix(base: &Settings) -> Result {
    let bases = stack_bases_file();
    if let Some(dir) = bases.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::write(&bases, "").map_err(|e| format!("{}: {e}", bases.display()))?;
    // ADR-0008 Amendment 1, D7: 3 and 65 processors too, and after the others the large sparse
    // machine and the discovery machine, unless held out of CI
    // (`Settings::runs_any_count_machines`).
    let any_count = base.runs_any_count_machines();
    let counts: &[u32] = if any_count {
        &[1, 2, 3, 4, 65]
    } else {
        &[1, 2, 4]
    };
    for run in matrix_runs(base, counts) {
        boot_machine(&run)?;
        check_stack_bases_so_far(&bases)?;
    }
    // ADR-0020 S23's machines (TASKS item 97), held out of CI as D7's are until a CI run with
    // them on shows their proofs pass under KVM at two guest processors a host processor (the
    // coordinating session's ruling Q9 of 2026-11-21).
    if any_count {
        for run in domain_runs(base) {
            boot_machine(&run)?;
            check_stack_bases_so_far(&bases)?;
        }
    }
    boot_x2apic(base)?;
    boot_bridge(base)?;
    boot_reserved(base)?;
    boot_legacy_free(base)?;
    boot_acpi(base)?;
    boot_random(base)?;
    boot_identity(base)?;
    boot_console(base)?;
    // TASKS item 83: more than 4 GiB of RAM, and rental B's board's table shapes, at their full
    // size on a developer's machine and 8 GiB in CI (`Settings::large_memory`).
    boot_large(base)?;
    boot_rented(base)?;
    bench::boot(base)?;
    // ADR-0008 D7 as decision item 294 has it: the large sparse machine of 67 processors, and
    // the discovery machine. The 288-processor run (`boot-many`) is outside the gate: on the
    // development Mac QEMU disturbs the guest's timing at 288 processors before any AP starts.
    if any_count {
        boot_sparse(base)?;
        boot_listed(base)?;
    } else {
        println!(
            "xtask: boot-matrix: the machines of ADR-0008 Amendment 1, D7 (3 and 65 processors, the 67-processor sparse machine, the discovery machine) and of ADR-0020 S23 (the SMT machine, the two-package machine) are not run in CI until each one's time has been measured and accepted (set ORIGIN_BOOT_MATRIX_ANY_COUNT=on to run them)"
        );
    }
    // ADR-0004 Amendment 2, A6.4: the boots placed the kernel stacks' region apart.
    let recorded = fs::read_to_string(&bases).map_err(|e| format!("{}: {e}", bases.display()))?;
    let boots = distinct_stack_bases(&recorded)?;
    println!(
        "xtask: boot-matrix: the kernel stacks' region was at {boots} distinct bases in {boots} kernel boots, read through QEMU's monitor; none is printed"
    );
    println!(
        "xtask: boot-matrix passed (Intel VT-d and AMD-Vi, each with {} processors, the sparse machine, the bridge machine, the reserved-memory machines, the legacy-free machine, the ACPI machine, the two machines without the random-number instructions, the two CPU identities, the console machines, the {} machines with and without rental B's board's shapes{})",
        if any_count {
            "1, 2, 3, 4 and 65"
        } else {
            "1, 2 and 4"
        },
        base.large_memory().qemu(),
        if any_count {
            ", the 67-processor sparse machine, the discovery machine, the SMT machine, the two-package machine"
        } else {
            ""
        }
    );
    Ok(())
}

/// `boot-matrix`'s first machines: each IOMMU with each of `counts` processors, each with its
/// timeout ([`timeout_for`]).
fn matrix_runs(base: &Settings, counts: &[u32]) -> Vec<MachineRun> {
    let mut runs = Vec::new();
    for (name, iommu) in iommus() {
        for &cpus in counts {
            runs.push(MachineRun {
                said: format!("xtask: boot-matrix: {name} IOMMU, {cpus} processor(s)"),
                settings: Settings {
                    iommu: iommu.clone(),
                    topology: Topology::Plain(cpus),
                    timeout: Some(timeout_for(base, cpus)),
                    ..base.clone()
                },
                failed: format!("boot-test with the {name} IOMMU and {cpus} processor(s)"),
            });
        }
    }
    runs
}

/// A boot's timeout for `cpus` processors: the default, and two seconds more for each processor
/// past four. A boot under QEMU's software processor starts each AP one at a time and runs every
/// per-processor proof on each (ADR-0008 Amendment 1, D8), so it grows with the count. A timeout
/// the person set (`ORIGIN_BOOT_TEST_TIMEOUT`, in `base`) is kept.
fn timeout_for(base: &Settings, cpus: u32) -> Duration {
    base.timeout.unwrap_or_else(|| {
        DEFAULT_BOOT_TEST_TIMEOUT + Duration::from_secs(2 * u64::from(cpus.saturating_sub(4)))
    })
}

/// `boot-test` on the gate's large sparse machine (ADR-0008 D7 as decision item 294 has it): 67
/// processors in two packages, three at APIC IDs 256, 300 and 412, Intel VT-d with interrupt
/// remapping, under QEMU's software processor only (a KVM runner has 4 host processors).
fn boot_sparse(base: &Settings) -> Result {
    if base.accel == Accel::Kvm {
        println!(
            "xtask: boot-sparse: not run under KVM: the 67-processor sparse machine runs under QEMU's software processor only (ADR-0008 D7)"
        );
        return Ok(());
    }
    let cpus = SPARSE_PRESENT + 3;
    boot_machines(sparse_runs(base))?;
    println!("xtask: boot-sparse passed ({cpus} processors online, three above APIC ID 255)");
    Ok(())
}

/// [`boot_sparse`]'s machine.
fn sparse_runs(base: &Settings) -> Vec<MachineRun> {
    let cpus = SPARSE_PRESENT + 3;
    vec![MachineRun {
        said: format!(
            "xtask: boot-sparse: {cpus} processors in two packages, APIC IDs 0-{} and 256, 300 and 412 of {} possible",
            SPARSE_PRESENT - 1,
            2 * SPARSE_CORES
        ),
        settings: Settings {
            topology: Topology::Sparse,
            iommu: Iommu::Intel,
            timeout: Some(timeout_for(base, cpus)),
            ..base.clone()
        },
        failed: "boot-test on the 67-processor sparse machine".into(),
    }]
}

/// `boot-test` on 288 processors in two packages of 144 cores, APIC IDs 0-143 and 256-399,
/// Intel VT-d with interrupt remapping, under QEMU's software processor only: a run measured and
/// recorded outside the gate (decision item 294), since QEMU disturbs the guest's timing at this
/// count on the development Mac.
fn boot_many(base: &Settings) -> Result {
    if base.accel == Accel::Kvm {
        println!(
            "xtask: boot-many: not run under KVM: the large sparse machine runs under QEMU's software processor only (ADR-0008 Amendment 1, D7)"
        );
        return Ok(());
    }
    let cpus = 2 * MANY_CORES;
    boot_machines(many_runs(base))?;
    println!("xtask: boot-many passed ({cpus} processors online)");
    Ok(())
}

/// [`boot_many`]'s machine.
fn many_runs(base: &Settings) -> Vec<MachineRun> {
    let cpus = 2 * MANY_CORES;
    vec![MachineRun {
        said: format!(
            "xtask: boot-many: {cpus} processors in two packages, APIC IDs 0-{} and 256-{}",
            MANY_CORES - 1,
            255 + MANY_CORES
        ),
        settings: Settings {
            topology: Topology::Many,
            iommu: Iommu::Intel,
            timeout: Some(timeout_for(base, cpus)),
            ..base.clone()
        },
        failed: "boot-test on the large sparse machine".into(),
    }]
}

/// `boot-test` on ADR-0020 S23's two machines (TASKS item 97): the SMT machine under Intel VT-d
/// and the two-package machine under AMD-Vi (the coordinating session's ruling Q8), the kernel
/// boot alone on each.
fn boot_domains(base: &Settings) -> Result {
    boot_machines(domain_runs(base))?;
    println!(
        "xtask: boot-domains passed (the SMT machine under VT-d, the two-package machine under AMD-Vi)"
    );
    Ok(())
}

/// [`boot_domains`]'s machines. Under QEMU's software processor each is an Intel guest, whose leaf
/// 04H reports which processors share its caches (ruling Q4 (a)); under KVM the guest is the
/// host's vendor.
fn domain_runs(base: &Settings) -> Vec<MachineRun> {
    let cpu = if base.accel == Accel::Tcg && !base.cpu.contains("vendor=") {
        format!("{},vendor=GenuineIntel", base.cpu)
    } else {
        base.cpu.clone()
    };
    [
        (
            Topology::Smt,
            Iommu::Intel,
            "the SMT machine, one package of four two-thread cores in two NUMA nodes",
            "intel",
        ),
        (
            Topology::Packages,
            Iommu::Amd,
            "the two-package machine, four cores and a NUMA node a package, with a PPTT",
            "amd",
        ),
    ]
    .into_iter()
    .map(|(topology, iommu, what, name)| MachineRun {
        said: format!("xtask: boot-domains: {what}, the {name} IOMMU"),
        settings: Settings {
            topology,
            iommu,
            cpu: cpu.clone(),
            only: KERNEL_BOOT.into(),
            timeout: Some(timeout_for(base, 8)),
            ..base.clone()
        },
        failed: format!("boot-test on {what}"),
    })
    .collect()
}

/// `boot-test` on the discovery machine (ADR-0008 Amendment 1, D7): the MADT lists the 4096
/// processors q35 allows, two of them enabled.
fn boot_listed(base: &Settings) -> Result {
    boot_machines(listed_runs(base))?;
    println!("xtask: boot-listed passed (2 of {LISTED} listed processors online)");
    Ok(())
}

/// [`boot_listed`]'s machine.
fn listed_runs(base: &Settings) -> Vec<MachineRun> {
    vec![MachineRun {
        said: format!("xtask: boot-listed: 2 processors enabled of {LISTED} the MADT lists"),
        settings: Settings {
            topology: Topology::Listed,
            ..base.clone()
        },
        failed: "boot-test on the discovery machine".into(),
    }]
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
    check: Option<Check>,
) -> Scenario {
    Scenario {
        quiet: None,
        label,
        loader: Loader::Verifying,
        markers,
        failures: qemu_boot_failures(),
        check,
        stderr: vec![],
        capture,
        qemu_args,
    }
}

/// The UART the SPCR names, which the loader named in the boot info (ADR-0010 Amendment 3;
/// TASKS item 84): the kernel writes to it from its first line, beside COM1, and at the move
/// COM1 stops and nothing is sent again; what follows the move reaches it and only it. COM1's
/// log is checked to hold the loader's line naming it, and to stop at the line announcing the
/// move; this UART's, to start with the kernel's first line ([`check_spcr_console`]).
fn spcr_uart_scenario() -> Scenario {
    use origin_version::{BUILD, PRODUCT, VERSION};
    console_scenario(
        "console: UART named by the SPCR",
        Capture::SpcrUart,
        console_markers(&[
            // Everything before the move, written here as it was printed: the first kernel
            // lines of the boot.
            "origin-kernel: console: framebuffer ",
            KERNEL_WRITES_COM2,
            &format!("origin-kernel: {PRODUCT} {VERSION} Framekernel (build {BUILD})"),
            "origin-kernel: GDT, TSS and IDT loaded (32 exception vectors)",
            "origin-kernel: page tables loaded: ",
            TAKEN_OVER_LINE,
            TAKEN_BYTES,
            // After the move: only this UART has these.
            "origin-kernel: allocated, wrote and freed 3 frames at ",
            "origin-kernel: TLB: ",
            "origin-kernel: GPT: partition 2",
            "origin-kernel: sysfs: built /sys with 4 facts",
        ]),
        Some(|work| spcr_args(work, &spcr_table(0, gas_io(SPCR_PORT), 7), &[])),
        Some(check_spcr_console),
    )
}

/// The loader's line, on its console (COM1 under OVMF), naming the SPCR's UART at `0x2F8`
/// (TASKS item 84).
const LOADER_NAMES_COM2: &str = "origin-boot: console: the SPCR names a UART at I/O port 0x2f8 (interface 0x0, 115200 baud): the kernel writes to it from its first line";
/// The kernel's line about the UART the loader named, on COM1, that UART and the screen.
const KERNEL_WRITES_COM2: &str = "origin-kernel: console: the loader named the SPCR's UART at I/O port 0x2f8 (interface 0x0): written to from the first line, beside COM1, as the firmware set it";
/// The line announcing the move to the UART the loader named, the last one COM1 may ever carry.
const TAKEN_OVER_LINE: &str = "origin-kernel: console: the ACPI SPCR (revision 2, interface 0x0) names a UART at I/O port 0x2f8: the console has written there since its first line, so nothing is sent again; COM1 stops";
/// After [`TAKEN_OVER_LINE`], on that UART alone: what writing there from the first line cost
/// (cost ledger row CL46).
const TAKEN_BYTES: &str =
    " bytes of text went to that UART before the move, beside COM1 (cost ledger row CL46)";
/// The line announcing a move to a UART the loader did not name, the last COM1 carries.
const MOVED_LINE: &str = "origin-kernel: console: the ACPI SPCR (revision 2, interface 0x0) names a UART at I/O port 0x2f8: moving the console there";
/// The bytes of [`long_spcr_table`]: more than the loader reads (1,024).
const LONG_SPCR: u32 = 1100;

/// An SPCR naming COM2 at 115,200 baud, padded to [`LONG_SPCR`] bytes: the loader reads no more
/// than 1 KiB of one and names no UART; the kernel reads it whole and moves to it (TASKS item 84's
/// review, T84-5).
fn long_spcr_table() -> Vec<u8> {
    let mut table = spcr_table(0, gas_io(SPCR_PORT), 7);
    table.resize(LONG_SPCR as usize, 0);
    table[4..8].copy_from_slice(&LONG_SPCR.to_le_bytes());
    table[9] = 0;
    let sum = table.iter().fold(0u8, |s, &b| s.wrapping_add(b));
    table[9] = 0u8.wrapping_sub(sum);
    table
}

/// E4's move, as before TASKS item 84, for an SPCR the loader cannot read whole (T84-5): COM1
/// stops at the move's line and holds the loader's refusal; the SPCR's UART is sent the earlier
/// lines again, every one of COM1's kernel lines, each once and in order, up to that line.
fn check_moved_with_a_replay(done: &Completed, _settings: &Settings) -> Result {
    check_com1_stopped_at(done, MOVED_LINE)?;
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    let refused = format!(
        "origin-boot: console: the SPCR was refused (it declares {LONG_SPCR} bytes, more than the loader reads (1024)); the kernel's console starts on COM1 and the screen"
    );
    if !com1.contains(&refused) {
        return Err(format!("COM1, the loader's console, lacks {refused:?}"));
    }
    same_kernel_lines(&com1, &done.serial, Some(MOVED_LINE))
}

/// The move that sends the earlier lines again: a UART the loader did not name.
const REPLAYED: &str = " bytes of earlier output sent to it again";

/// The SPCR's UART as TASKS item 84 makes it ([`spcr_uart_scenario`], and `boot-rented`'s
/// machine): the loader names it on COM1, its own console; the kernel's first line, the
/// framebuffer's, is the first on that UART, and nothing is sent there again; COM1 stops at the
/// move ([`check_com1_stopped_at_the_move`]).
fn check_spcr_console(done: &Completed, settings: &Settings) -> Result {
    check_com1_stopped_at_the_move(done, settings)?;
    let com1 = serial::clean(done.com1.as_deref().unwrap_or("").as_bytes());
    if !com1.contains(LOADER_NAMES_COM2) {
        return Err(format!(
            "COM1, the loader's console, lacks its line naming the SPCR's UART: {LOADER_NAMES_COM2:?}"
        ));
    }
    check_first_line_on_the_uart(&done.serial)?;
    check_same_port_named(&com1, &done.serial)?;
    // Everything COM1 carried, the SPCR's UART carried too, each line once, up to the take-over
    // (T84-4): a replay without its sentence, or a line lost or doubled there, fails.
    same_kernel_lines(&com1, &done.serial, Some(TAKEN_OVER_LINE))?;
    if done.serial.contains(REPLAYED) {
        return Err(format!(
            "the SPCR's UART was sent the earlier lines again ({REPLAYED:?}), though it has had them from the first line"
        ));
    }
    Ok(())
}

/// The UART the loader named carries the kernel's lines from the first: the framebuffer's, then
/// the line saying the loader named it (TASKS item 84).
fn check_first_line_on_the_uart(serial: &str) -> Result {
    let text = serial::clean(serial.as_bytes());
    let mut kernel_lines = text.lines().filter(|l| l.starts_with("origin-kernel:"));
    let first = kernel_lines.next();
    let second = kernel_lines.next();
    if !first.is_some_and(|l| l.starts_with("origin-kernel: console: framebuffer "))
        || second != Some(KERNEL_WRITES_COM2)
    {
        return Err(format!(
            "the SPCR's UART does not carry the kernel's lines from the first: it starts with {first:?} and {second:?}"
        ));
    }
    Ok(())
}

/// Once the firmware has named a better UART, the fallback is not used: COM1 holds the move
/// line as its last kernel line, and has never heard of anything after it.
fn check_com1_stopped_at_the_move(done: &Completed, _settings: &Settings) -> Result {
    check_com1_stopped_at(done, TAKEN_OVER_LINE)
}

/// COM1's last kernel line is `moved`, the line announcing a move away from it.
fn check_com1_stopped_at(done: &Completed, moved: &str) -> Result {
    let com1 = done.com1.as_deref().ok_or("no COM1 log")?;
    let text = serial::clean(com1.as_bytes());
    let kernel_lines: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("origin-kernel:"))
        .collect();
    if kernel_lines.last().copied() != Some(moved) {
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
        // E4's move and replay (T84-5): an SPCR the loader cannot read whole names no UART in
        // boot info, so the kernel moves to it, as every SPCR was moved to before TASKS item 84.
        console_scenario(
            "console: SPCR longer than the loader reads, moved to with a replay",
            Capture::SpcrUart,
            console_markers(&[
                // Everything before the move, sent again after it.
                "origin-kernel: console: framebuffer ",
                KERNEL_NO_LOADER_UART,
                MOVED_LINE,
                "origin-kernel: console: the UART is now at I/O port 0x2f8, as the firmware set it; ",
                REPLAYED,
                "origin-kernel: allocated, wrote and freed 3 frames at ",
            ]),
            Some(|work| spcr_args(work, &long_spcr_table(), &[])),
            Some(check_moved_with_a_replay),
        ),
        console_scenario(
            "console: SPCR naming COM1 itself",
            Capture::Com1,
            console_markers(&[
                "origin-boot: console: the SPCR names a UART at I/O port 0x3f8 (interface 0x0, 115200 baud): the kernel writes to it from its first line",
                "origin-kernel: console: the loader named the SPCR's UART at I/O port 0x3f8: COM1, written to from the first line",
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
                "origin-boot: console: the SPCR names a UART at I/O port 0x2e8 (interface 0x0, 115200 baud): the kernel writes to it from its first line",
                // Given up on after one bounded wait, during the kernel's first line.
                "origin-kernel: console: the loader's UART stopped taking bytes; serial output there off",
                // Said as it is by then, not as it was attached (T84-9).
                "origin-kernel: console: the loader named the SPCR's UART at I/O port 0x2e8 (interface 0x0): written to from the first line, and it stopped taking bytes there; COM1 and the screen until the SPCR is read",
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
                "origin-boot: console: the SPCR was refused (Interface(3)); the kernel's console starts on COM1 and the screen",
                KERNEL_NO_LOADER_UART,
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
                "origin-boot: console: the SPCR names a UART at I/O port 0x2e8 (interface 0x0, 115200 baud): the kernel writes to it from its first line",
                "origin-kernel: console: the loader named the SPCR's UART at I/O port 0x2e8, which cannot be used (nothing answers at that address); COM1 and the screen until the SPCR is read",
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
                "origin-boot: console: the SPCR names a UART in memory space at 0x1000000: the kernel moves to it once its page tables are live; the kernel's console starts on COM1 and the screen",
                KERNEL_NO_LOADER_UART,
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

/// The console scenarios (`boot-console`), each a whole boot on the machine `settings` describe.
/// Its filter (`ORIGIN_BOOT_TEST_ONLY`) selects by label, as for `boot-test`.
fn boot_console(settings: &Settings) -> Result {
    let krn_path = pack_kernel(&build_kernel()?)?;
    let (trusted, _) = test_keys()?;
    let signed = sign_kernel(&krn_path, &trusted, "KERNEL.signed.ONBF")?;
    let scenarios: Vec<(Scenario, Vec<u8>)> = console_scenarios()
        .into_iter()
        .map(|scenario| (scenario, signed.clone()))
        .collect();
    let chosen = select_scenarios(&scenarios, &settings.only)?;
    for (scenario, kernel) in &chosen {
        run_scenario(scenario, kernel, settings)?;
    }
    println!(
        "xtask: boot-console passed ({} of {} scenarios: the UART the SPCR names, one moved to with a replay, COM1 named, refused tables, a UART that takes no bytes, one that stops taking them, no display, no UART)",
        chosen.len(),
        scenarios.len()
    );
    Ok(())
}

/// Runs `boot-test`'s kernel boot in this process under each IOMMU with firmware-reserved
/// memory planted in QEMU's tables by the TEST ONLY application (`ORIGIN_QEMU_RESERVED=on`;
/// TASKS item 40; ADR-0007 questions 4 and 8): the kernel identity-maps what it may trust for
/// the NVMe controller, refuses the rest by name, and proves the maps by DMA.
fn boot_reserved(base: &Settings) -> Result {
    boot_machines(reserved_runs(base))?;
    println!("xtask: boot-reserved passed (both IOMMUs, firmware-reserved memory planted)");
    Ok(())
}

/// [`boot_reserved`]'s machines.
fn reserved_runs(base: &Settings) -> Vec<MachineRun> {
    iommus()
        .into_iter()
        .map(|(name, iommu)| MachineRun {
            said: format!(
                "xtask: boot-reserved: the {name} IOMMU, firmware-reserved memory planted"
            ),
            settings: Settings {
                iommu,
                reserved: true,
                only: KERNEL_BOOT.into(),
                ..base.clone()
            },
            failed: format!("boot-test with reserved memory planted and the {name} IOMMU"),
        })
        .collect()
}

/// Runs the kernel boot under each IOMMU on a machine with more than 4 GiB of RAM
/// ([`Settings::large_memory`]: 8 GiB in CI; TASKS item 83): the frame allocator, the direct map
/// and the NUMA proofs over memory no other machine has, each checked ([`check_memory`]).
fn boot_large(base: &Settings) -> Result {
    boot_machines(large_runs(base))?;
    println!(
        "xtask: boot-large passed (both IOMMUs, {} of RAM)",
        base.large_memory().qemu()
    );
    Ok(())
}

/// [`boot_large`]'s machines.
fn large_runs(base: &Settings) -> Vec<MachineRun> {
    let memory = base.large_memory();
    iommus()
        .into_iter()
        .map(|(name, iommu)| MachineRun {
            said: format!(
                "xtask: boot-large: the {name} IOMMU, {} of RAM",
                memory.qemu()
            ),
            settings: Settings {
                iommu,
                memory,
                only: KERNEL_BOOT.into(),
                ..base.clone()
            },
            failed: format!(
                "boot-test with {} of RAM and the {name} IOMMU",
                memory.qemu()
            ),
        })
        .collect()
}

/// Runs the kernel boot under each IOMMU on rental B's board's shapes (TASKS item 83;
/// `docs/research/2026-11-20-rental-b-findings.md`): the large machine's RAM, the SPCR naming COM2
/// at `0x2F8` with COM1 present, and the firmware's tables shaped by the TEST ONLY application
/// ([`RENTED_SHAPE`]). The kernel must move its console to COM2 and end with its verdict there; a
/// boot that fails says COM1's last kernel line, which the rented board's console could not show.
fn boot_rented(base: &Settings) -> Result {
    boot_machines(rented_runs(base))?;
    println!(
        "xtask: boot-rented passed (both IOMMUs, {} of RAM, rental B's board's table shapes; and, under the AMD IOMMU, the SPCR alone refused by the kernel and its reading of every table refused)",
        base.large_memory().qemu()
    );
    Ok(())
}

/// [`boot_rented`]'s machines.
fn rented_runs(base: &Settings) -> Vec<MachineRun> {
    let memory = base.large_memory();
    let mut runs: Vec<MachineRun> = iommus()
        .into_iter()
        .map(|(name, iommu)| MachineRun {
            said: format!(
                "xtask: boot-rented: the {name} IOMMU, {} of RAM, rental B's board's table shapes",
                memory.qemu()
            ),
            settings: Settings {
                iommu,
                memory,
                shape: Shape::Rented,
                only: KERNEL_BOOT.into(),
                ..base.clone()
            },
            failed: format!(
                "boot-test on rental B's board's shapes with {} of RAM and the {name} IOMMU",
                memory.qemu()
            ),
        })
        .collect();
    // TASKS item 84's review, T84-3: the SPCR alone in RAM the kernel owns, so both UARTs are
    // written to for the whole boot, which every check of a kernel boot holds.
    runs.push(MachineRun {
        said: format!(
            "xtask: boot-rented: the AMD IOMMU, {} of RAM, the SPCR alone in RAM the kernel owns, both UARTs kept",
            memory.qemu()
        ),
        settings: Settings {
            iommu: Iommu::Amd,
            memory,
            shape: Shape::SpcrRefused,
            only: KERNEL_BOOT.into(),
            ..base.clone()
        },
        failed: format!(
            "boot-test with the SPCR alone in RAM the kernel owns, {} of RAM and the AMD IOMMU",
            memory.qemu()
        ),
    });
    // TASKS item 84: rental B's case, the loader reading the SPCR that the kernel cannot, under
    // the board's IOMMU alone.
    runs.push(MachineRun {
        said: format!(
            "xtask: boot-rented: the AMD IOMMU, {} of RAM, rental B's board's table shapes, the kernel's reading of the SPCR refused",
            memory.qemu()
        ),
        settings: Settings {
            iommu: Iommu::Amd,
            memory,
            shape: Shape::KernelRefused,
            only: KERNEL_BOOT.into(),
            ..base.clone()
        },
        failed: format!(
            "boot-test on rental B's board's shapes with the kernel's reading of the SPCR refused, {} of RAM and the AMD IOMMU",
            memory.qemu()
        ),
    });
    runs
}

/// Runs the kernel boot on the ACPI machine (`ORIGIN_QEMU_ACPI=on`; TASKS item 62; ADR-0019 A6
/// area A, A7): two processors, and the tables QEMU builds that the usual machine lacks, an NFIT
/// for an NVDIMM, an HMAT, and an SRAT with a generic initiator, each checked by the kernel's
/// census of its ACPI tables.
fn boot_acpi(base: &Settings) -> Result {
    boot_machines(acpi_runs(base))?;
    println!("xtask: boot-acpi passed (QEMU's NFIT, HMAT and generic initiator, each checked)");
    Ok(())
}

/// [`boot_acpi`]'s machine.
fn acpi_runs(base: &Settings) -> Vec<MachineRun> {
    vec![MachineRun {
        said: "xtask: boot-acpi: QEMU's NFIT, HMAT and generic initiator".into(),
        settings: Settings {
            acpi: true,
            machine: "nvdimm=on,hmat=on".into(),
            topology: Topology::Plain(2),
            only: KERNEL_BOOT.into(),
            ..base.clone()
        },
        failed: "boot-test on the ACPI machine".into(),
    }]
}

/// Runs the kernel boot on a machine with no 8254 and no 8042 (ADR-0013 F2, F3), the
/// legacy-free shape of the machines Origin targets.
fn boot_legacy_free(base: &Settings) -> Result {
    boot_machines(legacy_free_runs(base))?;
    println!("xtask: boot-legacy-free passed (no 8254, no 8042)");
    Ok(())
}

/// [`boot_legacy_free`]'s machine.
fn legacy_free_runs(base: &Settings) -> Vec<MachineRun> {
    vec![MachineRun {
        said: "xtask: boot-legacy-free: no 8254 and no 8042".into(),
        settings: Settings {
            machine: "pit=off,i8042=off".into(),
            only: KERNEL_BOOT.into(),
            ..base.clone()
        },
        failed: "boot-test with no 8254 and no 8042".into(),
    }]
}

/// Runs the kernel boot on two processors that lack the instructions the random-number
/// generator is seeded from (ADR-0016 R3, R8): one with RDRAND and no RDSEED, which boots and
/// says what its generator's strength rests on; and one with neither, which has no generator,
/// launches nothing, and still reaches its verdict.
fn boot_random(base: &Settings) -> Result {
    boot_machines(random_runs(base))?;
    println!("xtask: boot-random passed (RDRAND alone; neither instruction)");
    Ok(())
}

/// [`boot_random`]'s machines.
fn random_runs(base: &Settings) -> Vec<MachineRun> {
    [
        ("max,-rdseed", "RDRAND and no RDSEED"),
        ("max,-rdseed,-rdrand", "neither RDSEED nor RDRAND"),
    ]
    .into_iter()
    .map(|(model, what)| MachineRun {
        said: format!("xtask: boot-random: a processor with {what} (-cpu {model})"),
        settings: Settings {
            cpu: model.into(),
            only: KERNEL_BOOT.into(),
            ..base.clone()
        },
        failed: format!("boot-test on a processor with {what}"),
    })
    .collect()
}

/// The processors `boot-identity` has QEMU report (ADR-0009 A2; TASKS item 16): the `-cpu`
/// value, what it is, and the whole line the kernel must print for it. The lines are written
/// here from the vendors' documents (`docs/research/2026-11-12-cpu-identification-table.md`),
/// not taken from the kernel's table, so the boot checks the table.
const CPU_IDENTITIES: [(&str, &str, &str); 2] = [
    (
        // Intel's microcode notes: PTL-H, 06-CC-02.
        "max,vendor=GenuineIntel,family=6,model=204,stepping=2",
        "a Panther Lake, in the declared scope",
        "origin-kernel: CPU identity: GenuineIntel family 06h (6) model CCh stepping 2 (leaf 1 EAX 000C06C2h): Panther Lake; in the declared scope: a 2026 Intel generation, listed by its family-6 model (ADR-0009 A2); reported under a hypervisor, which can report any identity; no erratum listed for it",
    ),
    (
        // Rental A's AMD EPYC 4244P (docs/research/2026-11-12-rental-a-findings.md): Zen 4.
        "max,vendor=AuthenticAMD,family=25,model=97,stepping=2",
        "rental A's EPYC 4244P, outside it",
        "origin-kernel: CPU identity: AuthenticAMD family 19h (25) model 61h stepping 2 (leaf 1 EAX 00A60F12h): older than Zen 5; outside the declared scope, a bench machine: AMD family below 1Ah, where Zen 5 begins (ADR-0009 A2); reported under a hypervisor, which can report any identity; no erratum listed for it",
    ),
];

/// Where `ORIGIN_QEMU_CPU` is one of [`CPU_IDENTITIES`], the boot must print that identity's
/// whole line in place of the line's start. The rest of the boot is held to every other
/// marker, its verdict and the halt line among them: the kernel never refuses a machine for
/// its identity (A2).
fn cpu_identity_marker(scenario: &mut Scenario, settings: &Settings) {
    if let Some((_, _, line)) = CPU_IDENTITIES
        .iter()
        .find(|(cpu, _, _)| *cpu == settings.cpu)
    {
        swap_marker(
            scenario,
            "origin-kernel: CPU identity: ",
            (*line).to_string(),
        );
    }
}

/// Runs the kernel boot on the two processors of [`CPU_IDENTITIES`], each set with QEMU's
/// `family=` and `model=` (ADR-0009 A2: "QEMU can be told to report any family and model, so CI
/// can boot an in-scope and an out-of-scope identity"). Under KVM a guest's vendor is not
/// changed: those boots run on QEMU's software processor only, and the KVM jobs' own boots
/// print the real processor's line.
fn boot_identity(base: &Settings) -> Result {
    if base.accel == Accel::Kvm {
        println!(
            "xtask: boot-identity: not run under KVM, where the guest's vendor stays the host's; QEMU's software processor runs it"
        );
        return Ok(());
    }
    boot_machines(identity_runs(base))?;
    println!(
        "xtask: boot-identity passed (a Panther Lake in the declared scope; a Zen 4 EPYC outside it)"
    );
    Ok(())
}

/// [`boot_identity`]'s machines.
fn identity_runs(base: &Settings) -> Vec<MachineRun> {
    CPU_IDENTITIES
        .iter()
        .map(|(model, what, _)| MachineRun {
            said: format!("xtask: boot-identity: {what} (-cpu {model})"),
            settings: Settings {
                cpu: (*model).into(),
                only: KERNEL_BOOT.into(),
                ..base.clone()
            },
            failed: format!("boot-test as {what}"),
        })
        .collect()
}

/// Runs `boot-test` in this process with the NIC behind a PCIe root port
/// (`ORIGIN_QEMU_BRIDGE=on`), under each IOMMU: bus mastering must be cleared on the bridge at
/// the quiesce and opened again above the device its driver attached (ADR-0007 I12). QEMU
/// itself ignores a bridge's Bus Master Enable, so this proves the kernel's path logic and the
/// read-backs, not the bridge's effect on traffic (the research ledger says so).
fn boot_bridge(base: &Settings) -> Result {
    boot_machines(bridge_runs(base))?;
    println!("xtask: boot-bridge passed (both IOMMUs, the NIC behind a PCIe root port)");
    Ok(())
}

/// [`boot_bridge`]'s machines.
fn bridge_runs(base: &Settings) -> Vec<MachineRun> {
    iommus()
        .into_iter()
        .map(|(name, iommu)| MachineRun {
            said: format!("xtask: boot-bridge: the {name} IOMMU, the NIC behind a PCIe root port"),
            settings: Settings {
                iommu,
                bridge: true,
                ..base.clone()
            },
            failed: format!("boot-test with the NIC behind a bridge and the {name} IOMMU"),
        })
        .collect()
}

/// Runs `boot-test` in this process on the sparse machine (`ORIGIN_QEMU_TOPOLOGY=x2apic`, see
/// `machine`): processors at APIC IDs 256 and 412 must come online, run threads, and take the
/// NVMe interrupts routed to them through interrupt remapping.
///
/// Twice, because the two models tell the kernel different things about one machine. On an
/// Intel-vendor model QEMU raises the CPUID maximum to 1Fh, so the proof sees dies and modules.
/// On `max` (AMD vendor), the maximum stays 0Dh and only leaf 0Bh remains, so the die and
/// module fields read as part of the core
/// (docs/research/2026-11-08-cpu-topology-leaf-1fh.md).
fn boot_x2apic(base: &Settings) -> Result {
    boot_machines(x2apic_runs(base))?;
    println!(
        "xtask: boot-x2apic passed (processors at APIC IDs 256 and 412, under leaves 1Fh and 0Bh)"
    );
    Ok(())
}

/// [`boot_x2apic`]'s machines.
fn x2apic_runs(base: &Settings) -> Vec<MachineRun> {
    [("max,vendor=GenuineIntel", "leaf 1Fh"), ("max", "leaf 0Bh")]
        .into_iter()
        .map(|(model, leaf)| MachineRun {
            said: format!(
                "xtask: boot-x2apic: processors at APIC IDs 0, 256 and 412 of 150 possible, -cpu {model} ({leaf})"
            ),
            settings: Settings {
                topology: Topology::X2apic,
                cpu: model.into(),
                ..base.clone()
            },
            failed: format!("boot-test on the sparse machine with -cpu {model}"),
        })
        .collect()
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

/// `cargo`, as the toolchain that runs this xtask (the one rust-toolchain.toml pins).
fn cargo() -> Command {
    let mut c = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    c.current_dir(repo_root());
    c
}

/// Runs `cmd` and fails unless it exits successfully. Its standard output is inherited. Its
/// error output passes through line by line, except the one compiler warning `cargo xtask`
/// expects, which is collapsed to a single note (`expected_warning.rs`, ADR-0017 Z10).
fn run_step(cmd: &mut Command) -> Result {
    let shown = std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    println!("xtask: $ {shown}");
    // Piped, cargo would drop its colours; keep them where a person is watching.
    if env::var_os("CARGO_TERM_COLOR").is_none() && std::io::stderr().is_terminal() {
        cmd.env("CARGO_TERM_COLOR", "always");
    }
    let mut child = cmd
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run `{shown}`: {e}"))?;
    let mut filter = expected_warning::Filter::default();
    if let Some(stderr) = child.stderr.take() {
        let mut reader = BufReader::new(stderr);
        let mut bytes = Vec::new();
        loop {
            bytes.clear();
            let read = reader
                .read_until(b'\n', &mut bytes)
                .map_err(|e| format!("reading the output of `{shown}`: {e}"))?;
            if read == 0 {
                break;
            }
            let text = String::from_utf8_lossy(&bytes);
            let line = text.strip_suffix('\n').unwrap_or(&text);
            let line = line.strip_suffix('\r').unwrap_or(line);
            for out in filter.line(line) {
                eprintln!("{out}");
            }
        }
    }
    let (rest, dropped) = filter.finish();
    for out in rest {
        eprintln!("{out}");
    }
    let status = child
        .wait()
        .map_err(|e| format!("cannot run `{shown}`: {e}"))?;
    if dropped != expected_warning::Dropped::default() {
        println!("{}", expected_warning::note(&dropped));
    }
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{shown}` failed ({status})"))
    }
}

// ------------------------------------------------------------------ check

fn check() -> Result {
    // The compiler first, by the commit it reports, before any host gate runs. Cargo's
    // `rust-version` is a floor: it refuses an older compiler and lets a newer one through, and
    // until now the commit was first asked for at the first image build (TASKS item 52's
    // review, finding 10).
    let compiler = toolchain::toolchain()?;
    println!(
        "xtask: check: rustc reports the pinned commit {}",
        compiler.commit
    );
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
        // TEST ONLY: the application that plants firmware-reserved memory, on the host (its
        // library and tests) and on its real target.
        &[
            "clippy",
            "--package",
            RESERVED_PACKAGE,
            "--lib",
            "--tests",
            "--",
            "-D",
            "warnings",
        ],
        &[
            "clippy",
            "--package",
            RESERVED_PACKAGE,
            "--target",
            UEFI_TARGET,
            "--",
            "-D",
            "warnings",
        ],
        &["test", "--package", "xtask"],
        &["test", "--package", LOADER_PACKAGE, "--lib"],
        &["test", "--package", RESERVED_PACKAGE, "--lib"],
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
    // The bodies of the ring-3 runtime's `mem*` functions on the host, with their tests against
    // `core` (S4-1's review, finding F6). `ring3::clippy` lints them for the userspace triple.
    run_step(cargo().args([
        "clippy",
        "--package",
        ring3::MEM_PACKAGE,
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ]))?;
    run_step(cargo().args(["test", "--package", ring3::MEM_PACKAGE]))?;
    // The ring-3 crates on their own target (task S4-1), then the kernel, which carries the
    // programs they build.
    ring3::clippy()?;
    run_step(ring3::with_programs(
        cargo().args([
            "clippy",
            "--package",
            KERNEL_PACKAGE,
            "--target",
            KERNEL_TARGET,
            "--",
            "-D",
            "warnings",
        ]),
        None,
    )?)?;
    run_step(cargo().args(["test", "--package", KERNEL_PACKAGE, "--lib"]))?;
    // The AML interpreter's boot proof on a 32 KiB stack: release only, the stack's proxy (TASKS
    // item 63, step B3, ruling Q3 (a)).
    run_step(cargo().args([
        "test",
        "--release",
        "--package",
        KERNEL_PACKAGE,
        "--lib",
        "aml_proof::",
    ]))?;
    // The same for the interpreter's nested loads: a load 1,200 calls deep on a 32 KiB thread,
    // release only (TASKS item 63, step B4; the review of B4, B4-6).
    run_step(cargo().args([
        "test",
        "--release",
        "--package",
        AML_PACKAGE,
        "--lib",
        "tests::load::nested_loads",
    ]))?;
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
    // `origin.core` as that build linked it, against the definition and the kernel's table;
    // and no `syscall` in a program the kernel carries.
    corestubs::gate()?;
    no_panic_check()?;
    dependency_check()?;
    linux_cfg_lint()?;
    documents::gate()?;
    requirements::gate()?;
    provenance::gate()?;
    roadmap::gate()?;
    progress::gate()?;
    costs::gate()?;
    unsafety::gate()?;
    // Which files may name the bench build's flag (TASKS item 112, F1).
    bench::gate()?;
    // Who may make a proof process, and which ring-3 files may name the call numbers.
    listed_names::gate()?;
    // Only the AML interpreter's `mem.rs` may allocate (TASKS item 63).
    amlalloc::gate()?;
    // Every per-processor table sized at discovery; no processor count compiled in.
    per_cpu::gate()?;
    // Every boot proof that compares the machine's counts settles it first (TASKS item 67).
    settle::gate()?;
    interfaces::gate()?;
    // The host tools are libraries first: xtask starts none of them, and they reach nothing of
    // the host (ADR-0011 Amendment 4, T1).
    host_tools::gate()?;
    image_inputs::version_gate()?;
    image_inputs::graph_gate()?;
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
    // The compiler is told where to write the assembly. Where Cargo keeps a crate's
    // intermediate files is no interface, and Cargo 1.100.0 moved them.
    let assembly = target_dir()
        .join("xtask")
        .join("signing")
        .join("onbf_sign.s");
    if let Some(dir) = assembly.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut emit = std::ffi::OsString::from("--emit=asm=");
    emit.push(&assembly);
    let build = || {
        run_step(
            cargo()
                .args([
                    "rustc",
                    "--quiet",
                    "--release",
                    "--package",
                    SIGN_PACKAGE,
                    "--lib",
                    "--",
                ])
                .arg(&emit)
                .args([
                    "-C",
                    "codegen-units=1",
                    "-C",
                    "debuginfo=0",
                    "-C",
                    "symbol-mangling-version=v0",
                ]),
        )
    };
    build()?;
    if !assembly.is_file() {
        // An earlier run built the crate with these arguments, so Cargo did not run the
        // compiler again, and the file that run wrote is gone. `cargo clean --package` is
        // Cargo's own way to make a crate build again.
        run_step(cargo().args(["clean", "--quiet", "--release", "--package", SIGN_PACKAGE]))?;
        build()?;
    }
    let text = fs::read_to_string(&assembly).map_err(|e| format!("{}: {e}", assembly.display()))?;
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

/// What the AML interpreter's object (`origin-aml`, TASKS item 63) may reference beyond
/// [`NO_PANIC_ALLOWED`], matched by length-prefixed parts of the mangled name, for this crate
/// alone:
/// - the allocator's entry points (`__rust_alloc`, `__rust_dealloc`, `__rust_realloc`,
///   `__rust_alloc_zeroed` and the shim marker), and not its error handler;
/// - `alloc::raw_vec::handle_error`, which is unreachable: every growth in the crate follows a
///   successful `try_reserve_exact` (`amlalloc.rs` says why it is still in the object);
/// - `core::fmt`'s builders and integer formatting, which `Debug` impls call and which return an
///   error rather than panic.
const NO_PANIC_AML_PARTS: &[&str] = &[
    "12___rust_alloc",
    "14___rust_dealloc",
    "14___rust_realloc",
    "19___rust_alloc_zeroed",
    "35___rust_no_alloc_shim_is_unstable_v2",
    "5alloc7raw_vec12handle_error",
    "4core3fmt8builders",
    "9Formatter12debug_struct",
    "4core3fmt3num",
];

/// The one library whose symbols `origin-aml`'s object may reference (TASKS item 63, B4's
/// contract, Q11): `origin-acpi-tables`, which checks every table `Load`, `LoadTable` and
/// `DataTableRegion` read. Two conditions make the allowance sound, and the check holds both:
/// - only the symbols that library's own object defines are allowed, not those of any other
///   freestanding library, nor anything that library itself references;
/// - that library passes its own no-panic check, in the same run and before this allowance is
///   used, so that what it defines is free of panic paths.
const NO_PANIC_AML_LIBRARY: &str = "origin-acpi-tables";

/// What `origin-aml`'s object must define: the concrete instantiation and the engine's loop.
const NO_PANIC_AML_REQUIRED: &[&str] = &["14no_panic_probe", "10run_frames"];

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
    // The AML interpreter is checked below, against an allowance of its own.
    for &package in FREESTANDING_LIBS.iter().filter(|&&p| p != AML_PACKAGE) {
        objects.push((package, build_kernel_object(package, &out_dir)?));
    }
    let mut defined = std::collections::HashSet::new();
    let mut referenced = Vec::new();
    // What `origin-acpi-tables`' own object defines: all `origin-aml` may use of it (Q11).
    let mut aml_library = std::collections::HashSet::new();
    for (package, obj) in &objects {
        let own = nm_symbols(&nm, obj, "--defined-only")?;
        let undefined = nm_symbols(&nm, obj, "-u")?;
        require_symbols(package, obj, own.len() + undefined.len())?;
        if *package == NO_PANIC_AML_LIBRARY {
            aml_library.extend(own.iter().cloned());
        }
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
    // The library `origin-aml` may use passed above, or this run has already failed.
    if !referenced.iter().any(|(p, _)| **p == NO_PANIC_AML_LIBRARY) {
        return Err(format!(
            "no-panic check: {NO_PANIC_AML_LIBRARY} was not checked, so {AML_PACKAGE} may not use it"
        ));
    }
    // The AML interpreter, with `alloc` (ADR-0019 A3): its own allowance, nothing else. Its
    // interpreter is generic over its host, so the object holds the engine only through
    // `no_panic_probe`, a concrete instantiation; the check requires it and the engine's loop in
    // the object, so that losing them fails here (the review of TASKS 63's B1, F3).
    let obj = build_kernel_object(AML_PACKAGE, &out_dir)?;
    let undefined = nm_symbols(&nm, &obj, "-u")?;
    let own = nm_symbols(&nm, &obj, "--defined-only")?;
    require_symbols(AML_PACKAGE, &obj, own.len() + undefined.len())?;
    for needed in NO_PANIC_AML_REQUIRED {
        if !own.iter().any(|s| s.contains(needed)) {
            return Err(format!(
                "{AML_PACKAGE}: the release object defines no symbol containing `{needed}`: the no-panic check would not see the interpreter"
            ));
        }
    }
    let bad: Vec<&str> = undefined
        .iter()
        .map(String::as_str)
        .filter(|s| {
            !NO_PANIC_ALLOWED.contains(s)
                && !NO_PANIC_ALLOWED_PARTS.iter().any(|p| s.contains(p))
                && !NO_PANIC_AML_PARTS.iter().any(|p| s.contains(p))
                && !aml_library.contains(*s)
        })
        .collect();
    let from_library = undefined
        .iter()
        .filter(|s| aml_library.contains(*s))
        .count();
    if !bad.is_empty() {
        return Err(format!(
            "{AML_PACKAGE}: the release object for {KERNEL_TARGET} references symbols outside its no-panic allowance:\n  {}",
            bad.join("\n  ")
        ));
    }
    println!(
        "xtask: no-panic check: {AML_PACKAGE} ({KERNEL_TARGET}, release, the interpreter instantiated by `no_panic_probe`: {} symbols of its own) references {} external symbols, all within its allowance (allocator entry points, an unreachable `handle_error`, `core::fmt`, and {from_library} that {NO_PANIC_AML_LIBRARY}'s own object defines, which passed its check above)",
        own.len(),
        undefined.len()
    );
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
        // The same flags as the kernel's own build, so that Cargo does not compile the
        // libraries they share again each time one follows the other.
        run_step(
            cargo()
                .args([
                    "rustc",
                    "--quiet",
                    "--package",
                    package,
                    "--lib",
                    "--release",
                    "--target",
                    KERNEL_TARGET,
                ])
                .args(toolchain::image_config()?)
                .args(["--", &emit, "-C", "codegen-units=1"]),
        )?;
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
    let toolchain = toolchain::toolchain()?;
    let path = toolchain
        .sysroot
        .join("lib/rustlib")
        .join(&toolchain.host)
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
    match loader {
        Loader::Bench => bench::build_loader(),
        Loader::BenchMinimal => bench::build_minimal_loader(),
        Loader::BenchBisect => bench::build_bisect_loader(),
        Loader::BenchKernel => bench::build_kernel_loader(),
        Loader::MeasureNormal => stack_depth::build_loader(false),
        Loader::MeasureBench => stack_depth::build_loader(true),
        _ => build_loader_in(Some(loader.feature()), None),
    }
}

/// Builds the TEST ONLY application that plants firmware-reserved memory, and returns the path
/// of its `.efi`. It takes the loader's flags: it builds the loader's library too, and with other
/// flags each build would rebuild that library over the other's.
fn build_reserved_fixture() -> Result<PathBuf> {
    let mut build = cargo();
    build.args([
        "build",
        "--package",
        RESERVED_PACKAGE,
        "--bin",
        RESERVED_BIN,
        "--target",
        UEFI_TARGET,
        "--release",
    ]);
    build.args(toolchain::image_config()?);
    run_step(&mut build)?;
    let efi = target_dir()
        .join(UEFI_TARGET)
        .join("release")
        .join(format!("{RESERVED_BIN}.efi"));
    if efi.is_file() {
        Ok(efi)
    } else {
        Err(format!(
            "cargo reported success but {} does not exist",
            efi.display()
        ))
    }
}

/// Builds the EFI application with `feature`, or with none, into `target`, or into Cargo's own
/// target directory, and returns the path of `origin-boot.efi`. With no feature the trust
/// store is empty: the loader a release builds, until Origin has keys of its own.
fn build_loader_in(feature: Option<&str>, target: Option<&Path>) -> Result<PathBuf> {
    build_loader_with(feature, target, &toolchain::image_config()?)
}

/// [`build_loader_in`] with the `--config` arguments `config` in place of
/// [`toolchain::image_config`]'s (the bench build's, TASKS item 112).
fn build_loader_with(
    feature: Option<&str>,
    target: Option<&Path>,
    config: &[String],
) -> Result<PathBuf> {
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
    build.args(config);
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
    build_kernel_with(target, &toolchain::image_config()?)
}

/// [`build_kernel_in`] with the `--config` arguments `config` in place of
/// [`toolchain::image_config`]'s (the bench build's, TASKS item 112).
fn build_kernel_with(target: Option<&Path>, config: &[String]) -> Result<PathBuf> {
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
    build.args(config);
    let elf = target
        .map_or_else(target_dir, Path::to_path_buf)
        .join(KERNEL_TARGET)
        .join("release")
        .join(KERNEL_PACKAGE);
    // The programs it carries in its image (task S4-1), built into the same directory.
    let build = ring3::with_programs(&mut build, target)?;
    // A kernel linked before `.cargo/config.toml` fixed the linker's line is linked again:
    // Cargo does not do that by itself (TASKS item 44).
    let output = image_inputs::Linked {
        path: elf.clone(),
        package: KERNEL_PACKAGE,
    };
    image_inputs::build_with_the_fixed_line(std::slice::from_ref(&output), || run_step(build))?;
    if elf.is_file() {
        Ok(elf)
    } else {
        Err(format!(
            "cargo reported success but {} does not exist",
            elf.display()
        ))
    }
}

/// Converts the kernel ELF into the KRN image with `onbf-pack`'s library, which verifies its
/// output before returning it.
fn pack_kernel(elf: &Path) -> Result<PathBuf> {
    pack_kernel_to(elf, &target_dir().join("xtask").join(KERNEL_IMAGE))
}

/// The file name of the converted kernel, under a target directory's `xtask/`.
const KERNEL_IMAGE: &str = "KERNEL.ONBF";

/// [`pack_kernel`] into `out`: the one function that makes the image the loader starts, for
/// the boots and for the reproducible-build gate alike. It calls `onbf_pack::krn::pack` in
/// process, with what `onbf-pack krn --input <elf> --output <out>` used: no CPU features beyond
/// the baseline, no flags, and the toolchain `rustc -vV` reports from the repository's root
/// (ADR-0011 Amendment 4, T1). The image is written through a rename, so it is never partial.
fn pack_kernel_to(elf: &Path, out: &Path) -> Result<PathBuf> {
    // A bench build's kernel is packed only for a bench boot (TASKS item 112).
    if !bench::is_bench(out) {
        bench::refuse(elf, "packing a kernel image outside target/bench")?;
    }
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let packed = onbf_pack::krn::pack(
        &read_file(elf)?,
        &onbf_pack::KrnOptions {
            cpu_features: 0,
            flags: String::new(),
            toolchain: toolchain::toolchain()?.krn.clone(),
        },
    )
    .map_err(|e| format!("onbf-pack: {}: {e}", elf.display()))?;
    write_atomically(out, &packed.bytes)?;
    println!(
        "onbf-pack: wrote KRN {}: {} sections, {} segments, {} relocations, {} bytes",
        out.display(),
        packed.sections,
        packed.segments,
        packed.relocations,
        packed.bytes.len()
    );
    Ok(out.to_path_buf())
}

/// Writes `bytes` to a temporary file beside `path`, syncs it and renames it into place, so that
/// `path` never holds a partial file. The temporary file is made new (`create_new`): a stale one
/// of this name is removed first, and a link planted at its name is removed, never followed (the
/// review of 2026-11-14, finding F9).
///
/// `onbf-pack`'s command line has the same function (`developer-tools/onbf-pack/src/main.rs`,
/// `write_atomically`), as it has `OsRandom`: keep the two in step (finding F10).
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> Result {
    let name = path
        .file_name()
        .ok_or_else(|| format!("{}: no file name", path.display()))?;
    let mut temporary = std::ffi::OsString::from(".");
    temporary.push(name);
    temporary.push(format!(".xtask-{}.tmp", std::process::id()));
    let temporary = path.with_file_name(temporary);
    match fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", temporary.display())),
    }
    let written = fs::File::create_new(&temporary)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&temporary, path));
    if let Err(e) = written {
        let _ = fs::remove_file(&temporary);
        return Err(format!("{}: {e}", path.display()));
    }
    Ok(())
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

/// The text the test keys are derived from (`onbf_pack::HostKey::from_test_seed`, the key
/// `onbf-pack keygen --insecure-test-seed` derives). This is the only "private" input they have,
/// and it is public, so they are test keys by construction and must never sign anything real
/// (CLAUDE.md §59).
const TEST_KEY_TRUSTED: &str = "TEST-ONLY origin-kernel key 1: the test trust store holds it";
const TEST_KEY_UNTRUSTED: &str = "TEST-ONLY origin-kernel key 2: no trust store holds it";
/// The test statements' claimed signing time. Informational (ONBF §7.6), and fixed: a build
/// reads no clock (ONBF-R9).
const TEST_SIGNING_TIME: &str = "1794000000";

/// The operating system's random number generator, which `onbf_pack` signs with: ML-DSA's
/// hedge comes from the caller (ADR-0011 Amendment 4, T1). A failure stops the signing.
/// `onbf-pack`'s command line has the same `OsRandom`: keep the two in step (the review of
/// 2026-11-14, finding F10).
struct OsRandom;

impl onbf_pack::Random for OsRandom {
    fn fill(&mut self, out: &mut [u8]) -> std::result::Result<(), onbf_pack::RandomError> {
        fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(out))
            .map_err(|e| {
                onbf_pack::RandomError(format!("the system random number generator failed: {e}"))
            })
    }
}

/// `path`'s contents, or an error naming it.
fn read_file(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The two test keys, derived in memory from their public text (`onbf_pack::sign`, as
/// `onbf-pack keygen --insecure-test-seed` derives them), and a check that the trusted one is
/// the public key the loader has compiled in: a stale copy in `boot/uefi/trust` would otherwise
/// show up as a boot failure with no explanation. No key file is written.
fn test_keys() -> Result<(onbf_pack::HostKey, onbf_pack::HostKey)> {
    use onbf_pack::Signer as _;
    let trusted = onbf_pack::HostKey::from_test_seed(TEST_KEY_TRUSTED, Box::new(OsRandom));
    let untrusted = onbf_pack::HostKey::from_test_seed(TEST_KEY_UNTRUSTED, Box::new(OsRandom));
    let committed = repo_root().join("boot").join("uefi").join("trust");
    let public = trusted.public_keys();
    for (name, derived) in [
        ("TEST-ONLY-origin-kernel.ed25519", &public.ed25519[..]),
        ("TEST-ONLY-origin-kernel.mldsa87", &public.mldsa87[..]),
    ] {
        if read_file(&committed.join(name))? != derived {
            return Err(format!(
                "boot/uefi/trust/{name} is not the public half of the test key derived from \
                 {TEST_KEY_TRUSTED:?}; regenerate it with `onbf-pack keygen`, as \
                 boot/uefi/trust/README.md describes"
            ));
        }
    }
    Ok((trusted, untrusted))
}

/// Signs the unsigned KRN image `input` with `key` in process (`onbf_pack::sign::sign_krn`,
/// which checks the result with the loader's code), writes it to `target/xtask/<output>`, and
/// returns the signed bytes.
fn sign_kernel(input: &Path, key: &onbf_pack::HostKey, output: &str) -> Result<Vec<u8>> {
    use onbf_pack::Signer as _;
    // A bench build's kernel is never signed (TASKS item 112; ADR-0002 B8).
    bench::refuse(input, "signing")?;
    let out = target_dir().join("xtask").join(output);
    let signing_time = TEST_SIGNING_TIME
        .parse()
        .map_err(|_| format!("the test signing time {TEST_SIGNING_TIME:?} is not a number"))?;
    let bytes = read_file(input)?;
    // ... nor a copy of one: its bytes say "bench build" (TASKS item 112's review, R112-5).
    if bench::holds(&bytes, bench::MARKER) {
        return Err(format!(
            "signing: {} holds the text \"bench build\": a bench build's kernel is never signed (TASKS item 112)",
            input.display()
        ));
    }
    let signed = onbf_pack::sign::sign_krn(
        &bytes,
        key,
        &onbf_pack::SignOptions {
            claimed_signing_time: signing_time,
        },
    )
    .map_err(|e| format!("onbf-pack: signing {}: {e}", input.display()))?;
    write_atomically(&out, &signed)?;
    println!(
        "onbf-pack: signed KRN {} in the role origin-kernel{}, key ID {}: {} bytes",
        out.display(),
        if key.is_test_key() {
            " with a TEST KEY"
        } else {
            ""
        },
        onbf_pack::sign::hex(&key.public_keys().key_id()),
        signed.len()
    );
    Ok(signed)
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

/// The firmware a boot uses: the two files `settings` name (`ORIGIN_OVMF_CODE` and
/// `ORIGIN_OVMF_VARS`, read with the other settings), or EDK2's beside QEMU.
fn find_firmware(settings: &Settings) -> Result<Firmware> {
    let (code, vars) = match &settings.ovmf {
        Some((code, vars)) => (code.clone(), vars.clone()),
        None => {
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
/// and a fresh NVRAM copy for one boot on the machine `settings` describe.
fn stage(kernel: &[u8], loader: Loader, settings: &Settings) -> Result<Staged> {
    let efi = build_loader(loader)?;
    let firmware = find_firmware(settings)?;
    let work = target_dir().join("xtask");
    let esp = work.join("esp");
    if esp.exists() {
        fs::remove_dir_all(&esp).map_err(|e| format!("{}: {e}", esp.display()))?;
    }
    // The removable-media default boot path for x64 (UEFI 2.11 §3.5.1.1).
    let boot_dir = esp.join("EFI").join("BOOT");
    fs::create_dir_all(&boot_dir).map_err(|e| format!("{}: {e}", boot_dir.display()))?;
    // Where the loader looks for the Framekernel (ADR-0002 B1).
    let origin_dir = esp.join("EFI").join("ORIGIN");
    fs::create_dir_all(&origin_dir).map_err(|e| format!("{}: {e}", origin_dir.display()))?;
    if settings.reserved || settings.shape.is_on() {
        // TEST ONLY (TASKS items 40 and 83): the application that plants firmware-reserved
        // memory, and shapes the tables as rental B's board has them, starts first, and starts
        // the unchanged loader from \EFI\ORIGIN\LOADER.EFI.
        let fixture = build_reserved_fixture()?;
        fs::copy(&fixture, boot_dir.join("BOOTX64.EFI"))
            .map_err(|e| format!("copy {}: {e}", fixture.display()))?;
        fs::copy(&efi, origin_dir.join("LOADER.EFI"))
            .map_err(|e| format!("copy {}: {e}", efi.display()))?;
        let regions = if settings.reserved {
            reserved_regions(settings.iommu == Iommu::Amd)
        } else {
            Vec::new()
        };
        let shape = match settings.shape {
            Shape::Off => origin_test_reserved_memory::Shape::default(),
            Shape::Rented => RENTED_SHAPE,
            Shape::KernelRefused => origin_test_reserved_memory::Shape {
                rsdp_in_ram: true,
                ..RENTED_SHAPE
            },
            Shape::SpcrRefused => origin_test_reserved_memory::Shape {
                spcr_in_ram: true,
                ..origin_test_reserved_memory::Shape::default()
            },
        };
        let mut config = vec![0u8; 1024];
        let len = origin_test_reserved_memory::encode(&regions, shape, &mut config)
            .ok_or("too many reserved regions")?;
        fs::write(origin_dir.join("RESERVED.CFG"), &config[..len])
            .map_err(|e| format!("write RESERVED.CFG: {e}"))?;
    } else {
        fs::copy(&efi, boot_dir.join("BOOTX64.EFI"))
            .map_err(|e| format!("copy {}: {e}", efi.display()))?;
    }
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

/// Which processors share each cache, pinned on every machine (TASKS item 97; the coordinating
/// session's ruling Q4 (b)): L1 data and instructions and L2 per core, L3 per package. The
/// scheduling-domains proof's expected tree then follows from `cargo xtask`'s own options, under
/// QEMU's software processor and KVM alike, wherever the guest reports its caches at all (leaf 04H
/// on an Intel guest, Fn8000_001D on an AMD guest with TopologyExtensions, which TCG does not
/// emulate). QEMU 11.1.3 accepted it on every machine of `boot-matrix` (checked 2026-11-21).
const SMP_CACHE: &str = "smp-cache.0.cache=l1d,smp-cache.0.topology=core,smp-cache.1.cache=l1i,smp-cache.1.topology=core,smp-cache.2.cache=l2,smp-cache.2.topology=core,smp-cache.3.cache=l3,smp-cache.3.topology=socket";

/// The machine every boot uses: q35 (the only machine type the Secure Boot firmware
/// supports), x86-64 with every feature TCG can emulate, and no network or display.
fn qemu_command(s: &Staged, settings: &Settings) -> Result<Command> {
    let machine = machine(settings);
    let mut c = Command::new(QEMU);
    c.arg("-machine")
        .arg(settings.machine_options(&format!("{},{SMP_CACHE}", settings.accel.machine())))
        .arg("-cpu")
        .arg(&settings.cpu)
        .arg("-m")
        // The ACPI machine's NVDIMM sits in a slot above the RAM. The RAM is 512 MiB unless
        // `boot-large` or `boot-rented` sets more (TASKS item 83).
        .arg(if settings.acpi {
            format!(
                "{},slots=2,maxmem={}M",
                settings.memory.qemu(),
                settings.memory.mib() + 512
            )
        } else {
            settings.memory.qemu()
        })
        .arg("-smp")
        .arg(&machine.smp);
    for cpu in &machine.devices {
        c.args(["-device", cpu]);
    }
    if let Some(plan) = machine.pptt {
        let path = target_dir().join("xtask").join("pptt.bin");
        fs::write(&path, pptt_table(&machine, plan))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        c.args(["-acpitable", &qemu_path_arg("file=", &path)?]);
    }
    c
        // Two NUMA nodes so the ACPI SRAT is populated (CLAUDE.md §24, §28). The two halves'
        // backends sum to -m, and QEMU auto-distributes the vCPUs across the nodes, so this
        // is valid for any -smp (verified at 1/2/4/8, and for the sparse x2APIC machine,
        // whose packages go to one node each).
        .arg("-object")
        .arg(format!(
            "memory-backend-ram,id=onuma0,size={}",
            settings.memory.node()
        ))
        .arg("-object")
        .arg(format!(
            "memory-backend-ram,id=onuma1,size={}",
            settings.memory.node()
        ))
        // With the ACPI machine's HMAT, each node names the initiator nearest its memory.
        .args(["-numa", &numa_node(0, &machine, settings.acpi)])
        .args(["-numa", &numa_node(1, &machine, settings.acpi)])
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
        .args(settings.iommu.device_args())
        // The NVMe controller and its test disk (CLAUDE.md §33).
        .arg("-drive")
        .arg(qemu_path_arg("if=none,id=nvme0,format=raw,file=", &s.nvme)?)
        .args(["-device", &{
            let mut device = "nvme,serial=ORIGIN0001,drive=nvme0".to_string();
            if settings.nvme_pairs != QEMU_NVME_PAIRS {
                device.push_str(&format!(",max_ioqpairs={}", settings.nvme_pairs));
            }
            if settings.acpi {
                // Named, so the ACPI machine's generic initiator can name it.
                device.push_str(",id=onvme");
            }
            device
        }]);
    if settings.acpi {
        c.args(acpi_machine_args(machine.cpus()));
    }
    // A PCIe root port for the NIC (threat T4): clearing bus mastering stops its forwarding too,
    // and the driver must open it again above the device it attached (`boot-bridge`).
    let nic_bus = if settings.bridge {
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
    if settings.debug {
        let log = target_dir().join("xtask").join("qemu-debug.log");
        c.arg("-d")
            .arg("int,cpu_reset,guest_errors")
            .arg("-D")
            .arg(log);
    }
    Ok(c)
}

/// Every boot `boot-test` runs on the machine `settings` describe, with the kernel image each
/// one boots.
fn boot_test_scenarios(unsigned: &Path, settings: &Settings) -> Result<Vec<(Scenario, Vec<u8>)>> {
    let krn = read_file(unsigned)?;
    let (trusted, untrusted) = test_keys()?;
    let signed = sign_kernel(unsigned, &trusted, "KERNEL.signed.ONBF")?;
    let wrong_key = sign_kernel(unsigned, &untrusted, "KERNEL.wrong-key.ONBF")?;
    let (statement_digest, ed25519, mldsa) = signature_fields(&signed)?;
    use Loader::{Developer, Verifying};
    let refuse = refusal_scenario;
    Ok(vec![
        // The real boot: developer mode off, a signed kernel, every kernel proof.
        (boot_scenario(settings), signed.clone()),
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

/// `boot-test` on the machine `settings` describe: every scenario, or those its filter names.
/// `boot-matrix` and its scenario commands call it in process for each of their machines (TASKS
/// item 61).
fn boot_test(settings: &Settings) -> Result {
    let krn_path = pack_kernel(&build_kernel()?)?;
    let scenarios = boot_test_scenarios(&krn_path, settings)?;
    let chosen = select_scenarios(&scenarios, &settings.only)?;
    for (scenario, kernel) in &chosen {
        run_scenario(scenario, kernel, settings)?;
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
        return Err(settings::no_scenario_matches(only, scenarios.len()));
    }
    Ok(chosen)
}

/// Where a boot whose console's UART is not COM1 stopped, as COM1 alone saw it (TASKS item 83):
/// the kernel prints there until the console moves, so on a boot that never moved, or stopped
/// before, COM1's last kernel line is the last thing the kernel said. Empty for a boot that
/// kept no COM1 file.
fn com1_last_kernel_line(com1: Option<&str>) -> String {
    let Some(com1) = com1 else {
        return String::new();
    };
    let text = serial::clean(com1.as_bytes());
    let last_of = |prefix: &str| text.lines().rev().find(|l| l.starts_with(prefix));
    let last = match last_of("origin-kernel: ") {
        Some(line) => format!("; COM1's last kernel line: {line:?}"),
        // No kernel line: what the application and the loader said last, which the firmware's
        // later text would hide (the review's T83-7).
        None => {
            let mut said = String::from("; COM1 has no kernel line");
            for (whose, prefix) in [
                ("the application's", "origin-test-reserved: "),
                ("the loader's", "origin-boot: "),
            ] {
                if let Some(line) = last_of(prefix) {
                    said.push_str(&format!("; {whose} last line: {line:?}"));
                }
            }
            match text.lines().rev().find(|l| !l.trim().is_empty()) {
                Some(line) => said.push_str(&format!("; its last line: {line:?}")),
                None => said = "; COM1 has nothing".into(),
            }
            said
        }
    };
    // What the console decided about the firmware's UART, which says why a boot stayed on COM1.
    match text.lines().rev().find(|l| {
        l.starts_with("origin-kernel: console: ")
            && (l.contains("SPCR") || l.contains("the UART at"))
    }) {
        Some(line) if !last.contains(line) => format!("{last}; its console's UART: {line:?}"),
        _ => last,
    }
}

/// Where a scenario's serial log is written.
fn scenario_log(scenario: &Scenario) -> PathBuf {
    target_dir().join("xtask").join(format!(
        "boot-test-{}.log",
        scenario
            .label
            .replace(|c: char| !c.is_ascii_alphanumeric(), "-")
    ))
}

/// Boots `kernel` once on the machine `settings` describe and checks its output against
/// `scenario`.
fn run_scenario(scenario: &Scenario, kernel: &[u8], settings: &Settings) -> Result {
    let staged = stage(kernel, scenario.loader, settings)?;
    let timeout = settings.boot_timeout();
    let accel = settings.accel;
    let work = target_dir().join("xtask");
    let mut cmd = qemu_command(&staged, settings)?;
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
    // Whether QEMU closed its output: it exited by itself (TASKS item 116).
    let mut closed = false;
    // When the last byte came, and when QEMU's output closed (TASKS item 121): a scenario tells
    // from the time between them which reset ended the boot.
    let mut last_byte: Option<Instant> = None;
    let mut closed_at: Option<Instant> = None;
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
        let oversubscribed = oversubscribed(settings);
        // The verdict on the output so far: a failure in a complete line, or every marker in
        // order, or `None` while neither.
        let judge = |raw: &[u8]| -> Option<Result> {
            let text = serial::clean(raw);
            // Failures are matched in complete lines only, so a failing line reaches the log
            // in full (its details follow the marker) before QEMU is stopped.
            let complete = text
                .get(..text.rfind('\n').map_or(0, |i| i + 1))
                .unwrap_or("");
            let judged = if oversubscribed {
                Cow::Owned(
                    complete
                        .lines()
                        .filter(|l| !checker_forgives(l, accel, true))
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            } else {
                accel.judged(complete)
            };
            if let Some(m) = scenario.failures.iter().find(|m| judged.contains(**m)) {
                return Some(Err(format!("the serial output contains {m:?}")));
            }
            serial::contains_in_order(&text, &markers).then_some(Ok(()))
        };
        loop {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(chunk) => {
                    raw.extend_from_slice(&chunk);
                    last_byte = Some(Instant::now());
                    // Judged only once a line has ended: QEMU hands the serial port's output
                    // over a few bytes at a time, and judging the whole log for each chunk is
                    // quadratic in its length. A 65-processor log took 48 minutes to judge
                    // that way, its kernel 65 s to print it (ADR-0008 Amendment 1, D7). The
                    // output is judged once more at the end, whatever its last line.
                    if !chunk.contains(&b'\n') {
                        continue;
                    }
                    if let Some(verdict) = judge(&raw) {
                        break verdict;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(verdict) = judge(&raw) {
                        break verdict;
                    }
                    let text = serial::clean(&raw);
                    let missing = serial::first_missing(&text, &markers).unwrap_or("(none)");
                    break Err(format!(
                        "first missing marker {missing:?}; no complete loader output within {}s",
                        timeout.as_secs()
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    closed = true;
                    closed_at = Some(Instant::now());
                    if let Some(verdict) = judge(&raw) {
                        break verdict;
                    }
                    break Err("QEMU exited before the loader output was complete".into());
                }
            }
        }
    };
    // A scenario that must say nothing more is read on for its quiet time (TASKS item 112).
    if verdict.is_ok()
        && let Some(quiet) = scenario.quiet
    {
        let until = Instant::now() + quiet;
        loop {
            match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Ok(chunk) => {
                    raw.extend_from_slice(&chunk);
                    last_byte = Some(Instant::now());
                }
                Err(RecvTimeoutError::Timeout) => break,
                // QEMU exited, so the quiet time is over: a reset under `-no-reboot`.
                Err(RecvTimeoutError::Disconnected) => {
                    closed = true;
                    closed_at = Some(Instant::now());
                    break;
                }
            }
        }
    }
    let elapsed = started.elapsed();

    // What the markers were found in, before any monitor output joins the stream.
    let text = serial::clean(&raw);
    // Every fair share the checker let pass on an oversubscribed boot, said so.
    if scenario.capture != Capture::Screen && oversubscribed(settings) {
        let host = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        for line in text.lines() {
            if let Some((first, second)) = oversubscribed_fair_share(line) {
                let cpu = line
                    .strip_prefix("origin-kernel: scheduler: ")
                    .and_then(|rest| rest.split_once(':'))
                    .map_or("a processor", |(cpu, _)| cpu);
                println!(
                    "xtask: {}: forgave fairness on an oversubscribed machine, named (ADR-0013; {} guest processors on {host} host threads): {cpu}: the workers counted {first} and {second}; both ran",
                    scenario.label,
                    machine(settings).cpus()
                );
            }
        }
    }
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

    // Whether QEMU had exited by itself, with a success status, before it is stopped (TASKS item
    // 116): under `-no-reboot`, a guest's reset. Closed output alone is not enough (a reader's
    // error closes it too), so the process's own status is waited for, a few seconds at most.
    let exited = closed && {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) if Instant::now() < until => thread::sleep(Duration::from_millis(100)),
                _ => break false,
            }
        }
    };
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
        exited,
        quiet_before_exit: match (last_byte, closed_at) {
            (Some(byte), Some(closed)) if exited => Some(closed.saturating_duration_since(byte)),
            _ => None,
        },
    };
    let verdict = verdict
        .and_then(|()| match stack_window_lines(&done.serial).first() {
            Some(line) => Err(format!(
                "the serial log names an address in the kernel stacks' window, which no boot may \
                 print (ADR-0004 Amendment 2, A6.3): {line:?}"
            )),
            None => Ok(()),
        })
        .and_then(|()| {
            scenario
                .check
                .map_or(Ok(()), |check| check(&done, settings))
        })
        .and_then(
            |()| match scenario.stderr.iter().find(|m| !qemu_stderr.contains(**m)) {
                Some(m) => Err(format!("QEMU's stderr lacks {m:?}")),
                None => Ok(()),
            },
        );
    let log = scenario_log(scenario);
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
                "boot-test ({}) failed: {e} (QEMU: {exit:?}){}; serial log: {}{}",
                scenario.label,
                com1_last_kernel_line(done.com1.as_deref()),
                log.display(),
                if done.com1.is_some() {
                    format!("; COM1's log: {}", com1_log.display())
                } else {
                    String::new()
                }
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

/// `run`: the developer-mode kernel under QEMU, interactively, on the machine `settings`
/// describe.
fn run(settings: &Settings) -> Result {
    let krn_path = pack_kernel(&build_kernel()?)?;
    let krn = fs::read(&krn_path).map_err(|e| format!("{}: {e}", krn_path.display()))?;
    let staged = stage(&krn, Loader::Developer, settings)?;
    let mut cmd = qemu_command(&staged, settings)?;
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
mod rented_scenario_tests {
    use super::*;

    /// The rented board's scenario keeps what it must hold (the review's T83-8): on COM2 the move
    /// and the census's FPDT, FBPT, S3PT and never-lent lines, and none of the firmware console's
    /// lines; on COM1 the application's shaping and the loader's lines to its hand-over; and no
    /// anchor it edits is missing.
    #[test]
    fn the_rented_scenario_holds_its_markers() {
        let settings = Settings {
            shape: Shape::Rented,
            memory: Memory::gib(64),
            ..Settings::default()
        };
        let scenario = boot_scenario(&settings);
        assert_eq!(scenario.capture, Capture::SpcrUart);
        let has = |m: &str| scenario.markers.iter().any(|x| x == m);
        assert!(!has(KERNEL_NO_LOADER_UART));
        assert!(!scenario.markers.iter().any(|m| m.contains(REPLAYED)));
        for required in [
            KERNEL_WRITES_COM2,
            TAKEN_OVER_LINE,
            "origin-kernel: acpi: FPDT rev 1",
            "origin-kernel: acpi: FBPT, 56 bytes: checked: 1 records",
            "origin-kernel: acpi: S3PT, 32 bytes: checked: 1 records",
            "origin-kernel: acpi: never lent, as the firmware changes them after boot: FACS 0x",
            "; 3 kept",
        ] {
            assert!(has(required), "{required}");
        }
        assert!(
            !scenario
                .markers
                .iter()
                .any(|m| m.starts_with("xtask: the marker "))
        );
        assert!(
            !scenario
                .markers
                .iter()
                .any(|m| m.starts_with("origin-boot: "))
        );
        assert!(!has(HANDOVER_LINE));
        let at = |m: &str| scenario.markers.iter().position(|x| x == m).unwrap();
        assert!(at(KERNEL_WRITES_COM2) < at(TAKEN_OVER_LINE));
        assert!(at(TAKEN_OVER_LINE) < at("origin-kernel: acpi: FPDT rev 1"));
        assert!(at("origin-kernel: acpi: FPDT rev 1") < at("origin-kernel: acpi: FADT: "));
        assert!(at("origin-kernel: acpi: FADT: ") < at("; 3 kept"));
        let com1 = rented_firmware_markers(&settings);
        assert!(com1.iter().any(|m| m == HANDOVER_LINE));
        assert!(
            com1.iter()
                .any(|m| m == "origin-test-reserved: starting the loader")
        );
        assert!(com1.iter().any(|m| m == LOADER_NAMES_COM2));
        assert!(!com1.iter().any(|m| m == LOADER_NO_SPCR));
        assert!(!com1.iter().any(|m| m.starts_with("xtask: the marker ")));
    }

    /// TASKS item 84's machine with the kernel's reading refused: held to the console alone, the
    /// UART the loader named carrying the refusal and the verdict, and COM1 the loader's line and
    /// the RSDP in loader data.
    #[test]
    fn the_refused_machine_holds_the_console() {
        let settings = Settings {
            shape: Shape::KernelRefused,
            memory: Memory::gib(64),
            ..Settings::default()
        };
        let scenario = boot_scenario(&settings);
        assert_eq!(scenario.capture, Capture::SpcrUart);
        assert_eq!(scenario.markers[1], KERNEL_WRITES_COM2);
        assert_eq!(scenario.markers[2], REFUSED_STAYS);
        assert_eq!(scenario.markers.last().map(String::as_str), Some(HALT_LINE));
        assert!(scenario.stderr.is_empty());
        // Every kernel boot's failure but the ones that need the ACPI tables (T84-3).
        for kept in [
            "(#PF page fault)",
            ": scheduler failed",
            "origin-kernel: boot refused",
        ] {
            assert!(scenario.failures.contains(&kept), "{kept}");
        }
        for dropped in NEEDS_THE_TABLES {
            assert!(!scenario.failures.contains(dropped), "{dropped}");
        }
        assert_eq!(
            scenario.failures.len() + NEEDS_THE_TABLES.len(),
            qemu_boot_failures().len()
        );
        let com1 = rented_firmware_markers(&settings);
        assert!(com1.iter().any(|m| m == LOADER_NAMES_COM2));
        assert!(
            com1.iter()
                .any(|m| m.contains("(loader data, RAM the kernel owns)"))
        );
        let runs = rented_runs(&settings);
        assert_eq!(runs.len(), 4);
        assert_eq!(runs[2].settings.shape, Shape::SpcrRefused);
        assert_eq!(runs[3].settings.shape, Shape::KernelRefused);
        assert!(runs[2..].iter().all(|r| r.settings.iommu == Iommu::Amd));
    }

    /// The machine with the SPCR alone in RAM (T84-3): the kernel boot's console lines, with the
    /// loader's UART line in place of "none" and the refusal in place of "no SPCR", and no move;
    /// every failure but the four one unreadable entry causes, `: console failed` among those
    /// held; COM1 has the application's SPCR line and the loader's naming of COM2.
    #[test]
    fn the_spcr_refused_machine_holds_its_console_proofs() {
        let settings = Settings {
            shape: Shape::SpcrRefused,
            memory: Memory::gib(64),
            ..Settings::default()
        };
        let scenario = boot_scenario(&settings);
        assert_eq!(scenario.capture, Capture::SpcrUart);
        // Every failure but the four one unreadable entry causes: the console's proofs are held.
        assert!(scenario.failures.contains(&": console failed"));
        assert_eq!(
            scenario.failures.len() + NEEDS_EVERY_ENTRY.len(),
            qemu_boot_failures().len()
        );
        // The console's lines of the kernel boot, and nothing else of the kernel's.
        assert!(
            scenario
                .markers
                .iter()
                .all(|m| m.starts_with("origin-kernel: console: ")
                    || m == BOOT_VERDICT
                    || m == HALT_LINE)
        );
        assert!(scenario.markers.iter().any(|m| {
            m.ends_with(" lines from a writer that cannot wait filled the UART's queue")
                || m.starts_with("origin-kernel: console: a line took ")
        }));
        let has = |m: &str| scenario.markers.iter().any(|x| x == m);
        assert!(has(KERNEL_WRITES_COM2));
        assert!(has(SPCR_REFUSED_PREFIX));
        assert!(has(HALT_LINE));
        assert!(!has(KERNEL_NO_LOADER_UART));
        assert!(!has(TAKEN_OVER_LINE));
        assert!(
            !scenario
                .markers
                .iter()
                .any(|m| m.starts_with("xtask: the marker "))
        );
        assert_eq!(reserved_top_mib(&settings), 0);
        let com1 = rented_firmware_markers(&settings);
        assert!(com1.iter().any(|m| m == LOADER_NAMES_COM2));
        assert!(
            com1.iter()
                .any(|m| m == "origin-test-reserved: the XSDT now names SPCR at 0x")
        );
        assert!(!com1.iter().any(|m| m.contains("reserved 0x")));
    }

    /// T84-4 and T84-14's checks fail on the faults they name.
    #[test]
    fn the_two_uarts_are_compared_line_by_line() {
        let com1 = "origin-kernel: a\norigin-kernel: b\norigin-boot: x\n";
        assert_eq!(
            same_kernel_lines(com1, "origin-kernel: a\norigin-kernel: b\n", None),
            Ok(())
        );
        // A line doubled, lost, or out of order on the SPCR's UART.
        for named in [
            "origin-kernel: a\norigin-kernel: a\norigin-kernel: b\n",
            "origin-kernel: b\n",
            "origin-kernel: b\norigin-kernel: a\n",
        ] {
            assert!(same_kernel_lines(com1, named, None).is_err(), "{named:?}");
        }
        // Up to the take-over line, then whatever follows on that UART alone.
        let named = "origin-kernel: a\norigin-kernel: b\norigin-kernel: after\n";
        assert_eq!(
            same_kernel_lines(com1, named, Some("origin-kernel: b")),
            Ok(())
        );
        assert!(same_kernel_lines(com1, named, Some("origin-kernel: c")).is_err());
        // The ports read from the two lines.
        let loader = "origin-boot: console: the SPCR names a UART at I/O port 0x2f8 (interface 0x0, 115200 baud): …";
        let kernel = "origin-kernel: console: the loader named the SPCR's UART at I/O port 0x2f8 (interface 0x0): …";
        assert_eq!(check_same_port_named(loader, kernel), Ok(()));
        assert!(check_same_port_named(loader, &kernel.replace("0x2f8", "0x3e8")).is_err());
        assert!(check_same_port_named("", kernel).is_err());
    }
}

#[cfg(test)]
mod memory_figure_tests {
    use super::memory_figures;

    /// The four lines, as the kernel prints them (TASKS item 83's boots of 2026-11-20).
    fn boot(usable: u64, free: u64, direct: u64, numa: u64) -> String {
        format!(
            "origin-kernel: boot info 0.7, 44 memory regions, {usable} MiB usable\n\
             origin-kernel: frame allocator: 16761756 free frames ({free} MiB), bitmap of 528 pages at 0x100000\n\
             origin-kernel: page tables loaded: 467 tables, {direct} MiB direct map, 502 image pages, 5067 4 KiB and 32755 2 MiB leaves; global pages true\n\
             origin-kernel: numa: 2 nodes, 2 cpus, 4 memory ranges, {numa} MiB; consistent true\n"
        )
    }

    /// The figures the machines showed pass, and the 8 GiB rented machine the review's T83-1
    /// predicted would fail the old 1% rule, too.
    #[test]
    fn the_machines_figures_pass() {
        assert_eq!(
            memory_figures(&boot(65480, 65475, 65527, 65535), 65536, 0),
            Ok(())
        );
        assert_eq!(
            memory_figures(&boot(65448, 65443, 65495, 65535), 65536, 32),
            Ok(())
        );
        assert_eq!(
            memory_figures(&boot(8137, 8133, 8183, 8191), 8192, 0),
            Ok(())
        );
        assert_eq!(
            memory_figures(&boot(8105, 8101, 8151, 8191), 8192, 32),
            Ok(())
        );
        assert_eq!(
            memory_figures(&boot(4041, 4037, 4087, 4095), 4096, 0),
            Ok(())
        );
    }

    #[test]
    fn each_figure_out_of_its_bounds_fails() {
        for (serial, says) in [
            // RB-3's figure: memory above 4 GiB missing from the allocator.
            (
                boot(65480, 1989, 65527, 65535),
                "the frame allocator's 1989 MiB free",
            ),
            // Reserved memory at the top taken as usable.
            (
                boot(65512, 65507, 65527, 65535),
                "the boot info's 65512 MiB usable",
            ),
            // A direct map that stops short of the usable memory, or maps the reserved top.
            (boot(65448, 65443, 4096, 65535), "the 4096 MiB direct map"),
            (boot(65448, 65443, 65527, 65535), "the 65527 MiB direct map"),
            // NUMA ranges that lose a node.
            (
                boot(65448, 65443, 65495, 32767),
                "the NUMA ranges' 32767 MiB",
            ),
        ] {
            let e = memory_figures(&serial, 65536, 32).unwrap_err();
            assert!(e.contains(says), "{says}: {e}");
        }
        let e = memory_figures("origin-kernel: boot info 0.7\n", 8192, 0).unwrap_err();
        assert!(e.contains("does not give its figure"), "{e}");
        let e = memory_figures("", 8192, 0).unwrap_err();
        assert!(e.starts_with("no line starting"), "{e}");
    }
}

#[cfg(test)]
mod com1_report_tests {
    use super::com1_last_kernel_line;

    /// TASKS item 83: a failed boot names COM1's last kernel line, and why its console stayed
    /// there, when COM1 is kept in a file.
    #[test]
    fn a_failure_names_com1_s_last_kernel_line_and_its_uart() {
        assert_eq!(com1_last_kernel_line(None), "");
        assert_eq!(com1_last_kernel_line(Some("")), "; COM1 has nothing");
        assert_eq!(
            com1_last_kernel_line(Some("BdsDxe: loading\r\norigin-boot: exiting\n")),
            "; COM1 has no kernel line; the loader's last line: \"origin-boot: exiting\"; its last line: \"origin-boot: exiting\""
        );
        // The application failed, and the firmware went on to other boot options.
        assert_eq!(
            com1_last_kernel_line(Some(
                "origin-test-reserved: AllocatePages failed: EFI_OUT_OF_RESOURCES\nBdsDxe: loading Boot0003\n"
            )),
            "; COM1 has no kernel line; the application's last line: \"origin-test-reserved: AllocatePages failed: EFI_OUT_OF_RESOURCES\"; its last line: \"BdsDxe: loading Boot0003\""
        );
        let stayed = "origin-kernel: a\norigin-kernel: console: no usable ACPI SPCR table (x); the UART stays at I/O port 0x3f8\norigin-kernel: M1 entry complete; halting\n";
        let said = com1_last_kernel_line(Some(stayed));
        assert!(
            said.starts_with(
                "; COM1's last kernel line: \"origin-kernel: M1 entry complete; halting\""
            ),
            "{said}"
        );
        assert!(
            said.ends_with("its console's UART: \"origin-kernel: console: no usable ACPI SPCR table (x); the UART stays at I/O port 0x3f8\""),
            "{said}"
        );
        let moved = "origin-kernel: console: the ACPI SPCR (revision 2, interface 0x0) names a UART at I/O port 0x2f8: moving the console there\n";
        assert_eq!(
            com1_last_kernel_line(Some(moved)),
            format!("; COM1's last kernel line: {:?}", moved.trim_end())
        );
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

    /// `body` as an executable shell script in a new directory under `target/xtask/`, run once
    /// before it is handed back, so that the test's own run of it cannot fail with "Text file
    /// busy" (ETXTBSY).
    ///
    /// Why (PR 41's CI, 2026-11-19): while `fs::write` holds the new file open for writing,
    /// another test thread may fork a child (the turn lock's tests do), and the child holds a
    /// copy of that descriptor until it execs; Linux refuses to exec a file open for writing
    /// anywhere. Writing under another name and renaming does not help, since the inherited
    /// descriptor is the cause. Once one exec of the file has succeeded, no process holds it for
    /// writing (this one closed it, and every child that inherited it has exec'd or exited), and
    /// nothing can open it so again, so the test's later runs, through `nm_symbols`, are safe.
    /// Only here, in the tests: `nm_symbols` and every other path that runs a program report
    /// ETXTBSY as the error it is.
    fn script(name: &str, body: &str) -> PathBuf {
        let dir = target_dir().join("xtask").join("gate-tests").join(name);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nm");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        retry_while_busy(|| Command::new(&path).output())
            .unwrap_or_else(|e| panic!("cannot run {}: {e}", path.display()));
        path
    }

    /// ETXTBSY, "Text file busy": the same number on Linux and on macOS.
    const ETXTBSY: i32 = 26;
    /// How many more times [`retry_while_busy`] tries, and how long it waits before each: one
    /// second in all, far longer than a forked child takes to reach its exec.
    const BUSY_RETRIES: u32 = 20;
    const BUSY_WAIT: Duration = Duration::from_millis(50);

    /// `run`, and again while it fails with ETXTBSY alone, up to [`BUSY_RETRIES`] more times;
    /// any other result, or the last ETXTBSY, is returned as it is.
    fn retry_while_busy<T>(mut run: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
        let mut retries = 0;
        loop {
            match run() {
                Err(e) if e.raw_os_error() == Some(ETXTBSY) && retries < BUSY_RETRIES => {
                    retries += 1;
                    thread::sleep(BUSY_WAIT);
                }
                other => return other,
            }
        }
    }

    /// The retry, with ETXTBSY simulated: it tries again on ETXTBSY alone, gives up after
    /// [`BUSY_RETRIES`] more tries with the error, and returns any other error at once.
    #[test]
    fn a_busy_text_file_is_tried_again_and_nothing_else_is() {
        let busy = || std::io::Error::from_raw_os_error(ETXTBSY);
        let mut calls = 0;
        let result = retry_while_busy(|| {
            calls += 1;
            if calls < 3 { Err(busy()) } else { Ok(calls) }
        });
        assert_eq!(result.unwrap(), 3);

        let mut calls = 0;
        let result: std::io::Result<()> = retry_while_busy(|| {
            calls += 1;
            Err(busy())
        });
        assert_eq!(result.unwrap_err().raw_os_error(), Some(ETXTBSY));
        assert_eq!(calls, BUSY_RETRIES + 1);

        // Another error, ENOENT, is not tried again.
        let mut calls = 0;
        let result: std::io::Result<()> = retry_while_busy(|| {
            calls += 1;
            Err(std::io::Error::from_raw_os_error(2))
        });
        assert_eq!(result.unwrap_err().raw_os_error(), Some(2));
        assert_eq!(calls, 1);
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
mod nvme_marker_tests {
    use super::{
        Geometry, Machine, QEMU_NVME_PAIRS, Settings, nvme_per_cpu_markers_for, queue_order,
        x2apic_markers,
    };

    /// Review finding F1 of 2026-11-14: on the gate's large sparse machine the three processors
    /// above APIC ID 255 get the first queues, so interrupts through remapping to them are
    /// proven there, never "nothing to prove".
    #[test]
    fn the_processors_above_255_get_the_first_queues() {
        let mut m = dense(67);
        m.apic_ids = (0..64).chain([256, 300, 412]).collect();
        assert_eq!(queue_order(&m)[..4], [64, 65, 66, 0]);
        let x2apic = x2apic_markers(&m, &Settings::default());
        assert!(x2apic.iter().all(|l| !l.contains("nothing to prove")));
        for (k, (index, id)) in [(64, 256), (65, 300), (66, 412)].into_iter().enumerate() {
            assert!(
                x2apic.iter().any(|l| l
                    .contains(&format!("CPU {index} (APIC ID {id}) is above 255"))
                    && l.contains(&format!("MSI-X entry {} holds", 2 + k))),
                "CPU {index}"
            );
        }
        let nvme = nvme_per_cpu_markers_for(&m, 67, QEMU_NVME_PAIRS);
        assert!(nvme[1].starts_with("origin-kernel: NVMe: CPU 64 (APIC ID 256): "));
        assert!(nvme[4].starts_with("origin-kernel: NVMe: CPU 0 (APIC ID 0): "));
        // A machine with no processor above 255 keeps table order.
        assert_eq!(queue_order(&dense(3)), [0, 1, 2]);
    }

    fn dense(cpus: u32) -> Machine {
        Machine {
            smp: cpus.to_string(),
            devices: Vec::new(),
            nodes: Vec::new(),
            node_cpus: None,
            pptt: None,
            apic_ids: (0..cpus).collect(),
            listed: cpus,
            highest: cpus - 1,
            geometry: Geometry {
                packages: 1,
                dies: 1,
                modules: 1,
                cores: cpus,
                threads: 1,
            },
        }
    }

    /// The coordinating session's ruling of 2026-11-14: on the gate's own configurations, which
    /// give queues, the proven lines are demanded and "not proven" is never enough; only a
    /// controller limited to the driver's two pairs is expected to say "not proven".
    #[test]
    fn the_gate_demands_the_proof_wherever_the_controller_gives_queues() {
        for cpus in [1, 2, 3, 4, 65, 67] {
            let m = dense(cpus);
            let markers = nvme_per_cpu_markers_for(&m, cpus, QEMU_NVME_PAIRS);
            assert!(
                markers.iter().all(|l| !l.contains("not proven")),
                "{cpus} processors"
            );
            assert!(
                markers
                    .last()
                    .is_some_and(|l| l.contains("threads at once, one per CPU")),
                "{cpus} processors: the summary is demanded"
            );
        }
        let limited = nvme_per_cpu_markers_for(&dense(2), 2, 2);
        assert_eq!(
            limited,
            [
                "origin-kernel: NVMe: per-CPU queues: not proven: the controller allocated 2 submission and 2 completion queues, has 65 MSI-X vectors, and BAR 0 holds the doorbells of 1535 I/O queues; the driver keeps 2 of each"
            ]
        );
        // Three pairs give one processor a queue: proven again.
        let three = nvme_per_cpu_markers_for(&dense(2), 2, 3);
        assert!(
            three
                .first()
                .is_some_and(|l| l.contains("1 of 2 processors"))
        );
    }
}

#[cfg(test)]
mod oversubscribed_tests {
    use super::{
        Accel, Settings, Topology, checker_forgives, oversubscribed, oversubscribed_fair_share,
        oversubscribed_for,
    };

    const UNEVEN: &str = "origin-kernel: scheduler: CPU 0: workers 0 and 1 counted 720000 and 7273000, 27 context switches: scheduler failed";

    /// Review finding F9 of 2026-11-14 and decision item 308 (ADR-0013 Amendment 2): the
    /// allowance is keyed to the machines, by name, never to the host's threads.
    #[test]
    fn only_the_named_large_software_processor_machines_are_oversubscribed() {
        for host in [1, 2, 4, 10, 64, 256] {
            assert!(
                oversubscribed_for(Accel::Tcg, Topology::Plain(65), host),
                "{host}"
            );
            assert!(oversubscribed_for(Accel::Tcg, Topology::Sparse, host));
            assert!(oversubscribed_for(Accel::Tcg, Topology::Many, host));
            for smp in [1, 2, 3, 4, 11, 64, 66] {
                assert!(
                    !oversubscribed_for(Accel::Tcg, Topology::Plain(smp), host),
                    "{smp} processors on {host} host threads"
                );
            }
            // The named machines fix their own processors (a count set with one is refused
            // since TASKS item 61); an empty `ORIGIN_QEMU_TOPOLOGY` is the plain machine.
            for other in [Topology::X2apic, Topology::Listed] {
                assert!(!oversubscribed_for(Accel::Tcg, other, host), "{other:?}");
            }
            assert!(
                !oversubscribed_for(Accel::Kvm, Topology::Plain(65), host),
                "KVM keeps its own allowance"
            );
            assert!(!oversubscribed_for(Accel::Kvm, Topology::Sparse, host));
        }
    }

    /// TASKS item 61: the boot's own typed settings name the machine, as the environment did.
    /// The settings `boot-matrix` makes for its 65-processor machines, and those of
    /// `boot-sparse` and `boot-many`, are oversubscribed under QEMU's software processor; no
    /// other machine it boots is, and none under KVM.
    #[test]
    fn the_typed_settings_name_the_same_machines() {
        let tcg = Settings::default();
        let kvm = Settings {
            accel: Accel::Kvm,
            ..Settings::default()
        };
        for base in [&tcg, &kvm] {
            let named = |runs: Vec<super::MachineRun>| -> Vec<bool> {
                runs.iter().map(|r| oversubscribed(&r.settings)).collect()
            };
            let on_tcg = base.accel == Accel::Tcg;
            assert_eq!(
                named(super::matrix_runs(base, &[1, 2, 3, 4, 65])),
                [false, false, false, false, on_tcg].repeat(2)
            );
            assert_eq!(named(super::sparse_runs(base)), [on_tcg]);
            assert_eq!(named(super::many_runs(base)), [on_tcg]);
            for runs in [
                super::x2apic_runs(base),
                super::bridge_runs(base),
                super::reserved_runs(base),
                super::legacy_free_runs(base),
                super::acpi_runs(base),
                super::random_runs(base),
                super::identity_runs(base),
                super::listed_runs(base),
            ] {
                assert!(named(runs).iter().all(|o| !o));
            }
        }
    }

    /// Decision item 316: ADR-0013 Amendment 3 ended when TASKS item 73 landed, so the named
    /// machines' kernel boot requires the stacks proof's last line to count no stack lost, as
    /// every other machine's does, under QEMU's software processor and under KVM: every machine
    /// `boot-matrix` and the scenario commands make (the independent review of TASKS item 73
    /// asked for all of them, not the named ones alone).
    #[test]
    fn every_machine_requires_no_stack_lost() {
        let kvm = Settings {
            accel: Accel::Kvm,
            ..Settings::default()
        };
        for base in [&Settings::default(), &kvm] {
            let runs = [
                super::matrix_runs(base, &[1, 2, 3, 4, 65]),
                super::sparse_runs(base),
                super::many_runs(base),
                super::x2apic_runs(base),
                super::bridge_runs(base),
                super::reserved_runs(base),
                super::legacy_free_runs(base),
                super::acpi_runs(base),
                super::random_runs(base),
                super::identity_runs(base),
                super::listed_runs(base),
            ];
            // Every machine whose kernel boot runs the stacks proof; the one without the
            // random-number instructions withholds it, and its line, and no named machine may.
            let mut judged = 0;
            for run in runs.into_iter().flatten() {
                let markers = super::boot_scenario(&run.settings).markers;
                let Some(at) = markers.iter().position(|m| m == super::STACKS_GIVEN_BACK) else {
                    assert!(!super::oversubscribed(&run.settings), "{:?}", run.settings);
                    continue;
                };
                assert_eq!(
                    markers.get(at + 1..at + 3),
                    Some(
                        [super::STACKS_NONE_LOST, super::STACKS_AS_BEFORE]
                            .map(String::from)
                            .as_slice()
                    ),
                    "{:?}",
                    run.settings
                );
                judged += 1;
            }
            // The ten machines of `boot-matrix` at least.
            assert!(judged >= 10, "{judged}");
        }
    }

    /// The coordinating session's two cases for decision item 308: a 4-processor machine on a
    /// 2-thread host is held to a fair share, which the host-keyed rule before review finding F9
    /// would have waived; each named large machine on a 64-thread host is not.
    #[test]
    fn the_host_changes_nothing_either_way() {
        let small = oversubscribed_for(Accel::Tcg, Topology::Plain(4), 2);
        assert!(!small);
        assert!(!checker_forgives(UNEVEN, Accel::Tcg, small));
        for topology in [Topology::Plain(65), Topology::Sparse, Topology::Many] {
            let large = oversubscribed_for(Accel::Tcg, topology, 64);
            assert!(large, "{topology:?}");
            assert!(checker_forgives(UNEVEN, Accel::Tcg, large), "{topology:?}");
        }
    }

    /// The coordinating session's ruling of 2026-11-14: an uneven share passes on an
    /// oversubscribed software-processor boot when both workers ran and the rounds hold, and
    /// nowhere else.
    #[test]
    fn an_uneven_share_passes_only_on_an_oversubscribed_boot_with_both_workers_running() {
        assert_eq!(
            oversubscribed_fair_share(UNEVEN),
            Some((720_000, 7_273_000))
        );
        assert!(
            checker_forgives(UNEVEN, Accel::Tcg, true),
            "oversubscribed: passes"
        );
        assert!(
            !checker_forgives(UNEVEN, Accel::Tcg, false),
            "the same share on a 4-processor machine fails"
        );
        assert!(
            !checker_forgives(UNEVEN, Accel::Kvm, false),
            "KVM's factor of four still holds"
        );
        let idle = UNEVEN.replace("counted 720000 and", "counted 0 and");
        assert!(
            !checker_forgives(&idle, Accel::Tcg, true),
            "a worker never ran: fails"
        );
        let few = UNEVEN.replace("27 context switches", "12 context switches");
        assert!(
            !checker_forgives(&few, Accel::Tcg, true),
            "too few rounds: fails"
        );
        let other = "origin-kernel: scheduler: workers did not exit: scheduler failed";
        assert!(
            !checker_forgives(other, Accel::Tcg, true),
            "no other scheduler line"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The temporary file `write_atomically` writes is made new: a link planted at its name is
    /// removed, never followed, and a stale file there is replaced (the review of 2026-11-14,
    /// finding F9). `onbf-pack`'s command line has the same test.
    #[test]
    fn writing_atomically_follows_no_link_at_the_temporary_name() {
        let dir = target_dir()
            .join("xtask")
            .join("gate-tests")
            .join("write-atomically");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let outside = dir.join("outside");
        fs::write(&outside, b"outside").unwrap();
        let path = dir.join("out.bin");
        let temporary = dir.join(format!(".out.bin.xtask-{}.tmp", std::process::id()));
        std::os::unix::fs::symlink(&outside, &temporary).unwrap();
        write_atomically(&path, b"image").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"image");
        assert_eq!(fs::read(&outside).unwrap(), b"outside");
        assert!(fs::symlink_metadata(&temporary).is_err());
        fs::write(&temporary, b"stale").unwrap();
        write_atomically(&path, b"again").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"again");
        assert!(fs::symlink_metadata(&temporary).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    /// TASKS item 40's review, M2: the kernel writes into a planted reserved page only where it
    /// finds the fill the TEST ONLY application puts there, so the two copies must agree.
    #[test]
    fn the_fixtures_fill_is_the_one_the_kernel_recognises() {
        for page in [0x1000, 0x1FB3_1000, 0x7A00_0000, 0xFF_FFFF_F000] {
            let planted: Vec<u8> = (0..framekernel::testfill::PAGE as u64)
                .map(|o| origin_test_reserved_memory::fill_byte(page, o))
                .collect();
            assert!(
                framekernel::testfill::holds_fill(page, &planted),
                "{page:#x}"
            );
            // What the application puts in its pages without the fill is not the fill.
            let other: Vec<u8> = (0..framekernel::testfill::PAGE as u64)
                .map(|o| origin_test_reserved_memory::fill_byte(page + 0x10_0000, o))
                .collect();
            assert!(
                !framekernel::testfill::holds_fill(page, &other),
                "{page:#x}"
            );
        }
    }

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
        let settings = Settings::default();
        let m = machine(&settings);
        let mut s = boot_scenario_markers(&m, &settings);
        amd_iommu_markers(&mut s, &m, &settings);
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

    /// The QEMU options each IOMMU adds (what `ORIGIN_QEMU_IOMMU` selects is read in
    /// `settings.rs`, and tested there).
    #[test]
    fn each_iommu_adds_its_devices() {
        assert_eq!(
            Iommu::Intel.device_args(),
            ["-device", "intel-iommu,intremap=on,eim=on"]
        );
        let off = Iommu::Device("intel-iommu,intremap=off".into());
        assert_eq!(off.device_args(), ["-device", "intel-iommu,intremap=off"]);
    }

    /// The scheduling-domains lines expected of each kind of machine (TASKS item 97), from the
    /// machine alone: the trees the kernel's host tests build from the same shapes.
    #[test]
    fn the_domain_trees_follow_the_machines() {
        let tree = |topology: Topology, acpi: bool, caches: bool| {
            let settings = Settings {
                topology,
                acpi,
                ..Settings::default()
            };
            domain_tree(&machine(&settings), caches, false)
        };
        assert_eq!(
            tree(Topology::Smt, false, true),
            "origin-kernel: domains: 8 processors: core, L2 4x2 [0-1|2-3|4-5|6-7]; node 2x4 [0-3|4-7]; last-level cache, package 1x8 [0-7]"
        );
        assert_eq!(
            tree(Topology::Smt, false, false),
            "origin-kernel: domains: 8 processors: core 4x2 [0-1|2-3|4-5|6-7]; node 2x4 [0-3|4-7]; package 1x8 [0-7]"
        );
        assert_eq!(
            tree(Topology::Packages, false, true),
            "origin-kernel: domains: 8 processors: last-level cache, package, node 2x4 [0-3|4-7]"
        );
        // With no cache leaf (an AMD guest under QEMU's software processor) its PPTT alone gives
        // the caches (the review's R97-2).
        assert_eq!(
            tree(Topology::Packages, false, false),
            "origin-kernel: domains: 8 processors: last-level cache, package, node 2x4 [0-3|4-7]"
        );
        // Control C2's PPTT on the SMT machine: an L3 a node, met with CPUID's L3 a package.
        let mut c2 = machine(&Settings {
            topology: Topology::Smt,
            ..Settings::default()
        });
        c2.pptt = Some(Pptt {
            l3: L3::Node,
            cache_ids: true,
        });
        assert_eq!(
            domain_tree(&c2, true, false),
            "origin-kernel: domains: 8 processors: core, L2 4x2 [0-1|2-3|4-5|6-7]; last-level cache, node 2x4 [0-3|4-7]; package 1x8 [0-7]"
        );
        assert_eq!(
            tree(Topology::Plain(1), false, false),
            "origin-kernel: domains: 1 processors: core, package, node 1x1 [0]"
        );
        assert_eq!(
            tree(Topology::Plain(4), false, true),
            "origin-kernel: domains: 4 processors: last-level cache, package, node 1x4 [0-3]"
        );
        // The ACPI machine names each processor's node: a node each, so no node level.
        assert_eq!(
            tree(Topology::Plain(2), true, false),
            "origin-kernel: domains: 2 processors: package 1x2 [0-1]"
        );
        assert_eq!(
            tree(Topology::X2apic, false, false),
            "origin-kernel: domains: 3 processors: package, node 2 groups [0|1-2]"
        );
        assert_eq!(
            tree(Topology::Sparse, false, false),
            "origin-kernel: domains: 67 processors: package, node 2 groups [0-63|64-66]"
        );
        assert_eq!(
            tree(Topology::Many, false, false),
            "origin-kernel: domains: 288 processors: package, node 2x144 [0-143|144-287]"
        );
        assert_eq!(
            tree(Topology::Listed, false, false),
            "origin-kernel: domains: 2 processors: package, node 1x2 [0-1]"
        );
        // Leaf 1Fh shows the x2APIC machine's dies, one processor each: no die level.
        let x2apic = machine(&Settings {
            topology: Topology::X2apic,
            ..Settings::default()
        });
        assert_eq!(
            domain_tree(&x2apic, true, true),
            "origin-kernel: domains: 3 processors: last-level cache, package, node 2 groups [0|1-2]"
        );
        assert_eq!(index_ranges(&[0, 1, 2, 5, 7, 8]), "0-2,5,7-8");
    }

    /// The two-package machine's PPTT, as `cargo xtask` plants it: a valid table whose leaves are
    /// the CPU indexes, with one L3 a package (ruling Q6), and control C2's on the SMT machine, an
    /// L3 a node.
    #[test]
    fn the_planted_pptts_are_well_formed() {
        let packages = machine(&Settings {
            topology: Topology::Packages,
            ..Settings::default()
        });
        let plan = packages
            .pptt
            .expect("the two-package machine carries a PPTT");
        let table = pptt_table(&packages, plan);
        assert_eq!(&table[..4], b"PPTT");
        assert_eq!(
            u32::from_le_bytes(table[4..8].try_into().unwrap()) as usize,
            table.len()
        );
        assert_eq!(table.iter().fold(0u8, |s, &b| s.wrapping_add(b)), 0);
        // The kernel's own reading of it: the leaves are the CPU indexes, the L2 a core's and the
        // L3 a package's, two L3s with IDs among five caches.
        let madt: Vec<(u32, bool)> = (0..8).map(|uid| (uid, true)).collect();
        let asked: Vec<u32> = (0..8).collect();
        let read = framekernel::acpi::pptt_caches(&table, &madt, &asked)
            .expect("the kernel reads it")
            .expect("it has caches");
        assert_eq!((read.caches, read.with_id), (5, 2));
        let first = |v: &[Option<u64>]| -> Vec<usize> {
            v.iter()
                .map(|x| v.iter().position(|y| y == x).unwrap())
                .collect()
        };
        // The last levels, each the third level from the processor's own first cache.
        let last = |v: &[Option<(u8, u64)>]| -> Vec<usize> {
            let instances: Vec<Option<u64>> = v
                .iter()
                .map(|l| {
                    assert_eq!(l.map(|(level, _)| level), Some(3));
                    l.map(|(_, instance)| instance)
                })
                .collect();
            first(&instances)
        };
        assert_eq!(first(&read.second_level), [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(last(&read.last_level), [0, 0, 0, 0, 4, 4, 4, 4]);
        // Control C3's: the same table without IDs.
        let no_ids = pptt_table(
            &packages,
            Pptt {
                cache_ids: false,
                ..plan
            },
        );
        let read = framekernel::acpi::pptt_caches(&no_ids, &madt, &asked)
            .unwrap()
            .unwrap();
        assert_eq!((read.caches, read.with_id), (5, 0));
        assert_eq!(last(&read.last_level), [0, 0, 0, 0, 4, 4, 4, 4]);
        // Control C2's on the SMT machine: an L3 a node, the threads of a core sharing its L2.
        let smt = machine(&Settings {
            topology: Topology::Smt,
            ..Settings::default()
        });
        let c2 = pptt_table(
            &smt,
            Pptt {
                l3: L3::Node,
                cache_ids: true,
            },
        );
        let read = framekernel::acpi::pptt_caches(&c2, &madt, &asked)
            .unwrap()
            .unwrap();
        assert_eq!(first(&read.second_level), [0, 0, 2, 2, 4, 4, 6, 6]);
        assert_eq!(last(&read.last_level), [0, 0, 0, 0, 4, 4, 4, 4]);
    }

    /// The expected topology lines, computed from the `-smp` fields alone: what QEMU's
    /// machine must decode to under each leaf.
    #[test]
    fn the_topology_markers_follow_the_machine() {
        let sparse = Machine {
            smp: String::new(),
            devices: Vec::new(),
            nodes: Vec::new(),
            node_cpus: None,
            pptt: None,
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
            nodes: Vec::new(),
            node_cpus: None,
            pptt: None,
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

    /// Each accelerator's `-machine` value (which `ORIGIN_QEMU_ACCEL` selects is read in
    /// `settings.rs`, and tested there).
    #[test]
    fn each_accelerator_has_its_machine() {
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

    /// B3-18: the not-QEMU line fails the QEMU scenarios, and a physical boot's list stays
    /// without it. The phrase is the AML proof's own (`aml_proof.rs`).
    #[test]
    fn the_not_qemu_line_fails_qemu_boots_only() {
        let line = "origin-kernel: aml: the DSDT's OEM ID \"OEMID \" and OEM table ID \"TABLEID \" are not QEMU's: the proof runs only on QEMU's own tables (ADR-0019 A3) and ran no AML; passed";
        assert!(qemu_boot_failures().iter().any(|f| line.contains(f)));
        assert!(!boot_failures().iter().any(|f| line.contains(f)));
        let proof = fs::read_to_string(repo_root().join("framekernel/src/aml_proof.rs")).unwrap();
        for phrase in QEMU_ONLY_FAILURES {
            assert!(
                proof.contains(phrase),
                "{phrase}: not the proof's own words"
            );
        }
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
        let settings = Settings::default();
        let m = machine(&settings);
        let mut s = boot_scenario_markers(&m, &settings);
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
        let settings = Settings::default();
        let m = machine(&settings);
        let mut s = boot_scenario_markers(&m, &settings);
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
        let mut other = boot_scenario_markers(&m, &settings);
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
        let settings = Settings::default();
        let m = machine(&settings);
        let plain = boot_scenario_markers(&m, &settings);
        assert!(
            plain
                .markers
                .iter()
                .any(|m| m.starts_with(EVEN_SHARES_LINE))
        );
        let per_cpu = "origin-kernel: scheduler: CPU 0: workers 0 and 1 counted ";
        assert!(plain.markers.iter().any(|m| m == per_cpu));

        for vt_d in [true, false] {
            let mut s = boot_scenario_markers(&m, &settings);
            if !vt_d {
                amd_iommu_markers(&mut s, &m, &settings);
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
        let settings = Settings::default();
        let m = machine(&settings);
        let mut s = boot_scenario_markers(&m, &settings);
        amd_iommu_markers(&mut s, &m, &settings);
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
