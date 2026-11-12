//! `cargo xtask check`'s gate on `unsafe` in the kernel (ADR-0011 N6 and N13; CLAUDE.md §13,
//! decision item 111): only the files listed here may use it, all of them in the framework
//! crate, and every other kernel crate builds on the framework, never the reverse.
//!
//! - The kernel is the crates under [`KERNEL_ROOT`] and the leaf crates it links
//!   ([`KERNEL_LEAVES`]). Anywhere in them but under [`FRAMEWORK`], a file with an `unsafe`
//!   token (or a `global_asm!`, which needs no `unsafe` keyword) fails the gate.
//! - Under [`FRAMEWORK`], only the files on [`UNSAFE_FILES`] may hold one. Every other file
//!   there starts with `#![forbid(unsafe_code)]`, so the compiler refuses an `unsafe` added to it,
//!   and the gate refuses the attribute being taken away. A new file is one or the other.
//! - Each kernel crate outside the framework carries the same attribute at its library root, which
//!   the compiler then enforces for every module of the crate. (The kernel image's link root
//!   holds `_start`, so it lives under [`FRAMEWORK`].)
//! - Dependencies run one way, per [`LAYERS`]: the framework depends only on the leaf crates
//!   named there, and no crate below it depends on the framework or on what is above it.
//!
//! A "site" here is one `unsafe` token (or `global_asm!`) outside comments, strings and
//! character literals, whatever it heads: a block, an `unsafe fn`, an `unsafe impl`, an
//! `unsafe extern` or an attribute such as `#[unsafe(no_mangle)]`.
//!
//! Ring 3 has a list of its own ([`RING3_UNSAFE_FILES`], task S4-1): in the ring-3 crates
//! ([`RING3_CRATES`]) only the listed files may hold `unsafe`, each for a reason, and the crate
//! roots that can deny it do ([`RING3_ROOTS`]). `runtime/kernel-client` and `runtime/core`
//! commit no generated code: the shims they include (the client's imports in `abi.rs`, and
//! `origin.core`'s stubs, OIDL §11's two named shims) are the generator's output under the
//! target directory, written at each build.
//!
//! The gate ends with two on the assembly inside those sites: what a block promises the
//! compiler about the flags (`asmflags`, CLAUDE.md §13), and where the processor's two
//! random-number instructions may be run (`mnemonics`, CLAUDE.md §59; ADR-0016 R2).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::{Result, asmflags, cargo, collect_files, lex, mnemonics, repo_root};

/// Under this directory is the privileged framework, the one place `unsafe` may appear: the
/// crate `origin-frame`, and its lock package, `origin-sync`, kept as a package of its own
/// because the Loom models build it alone.
pub(crate) const FRAMEWORK: &str = "framekernel/frame";
/// The directory holding the rest of the kernel.
const KERNEL_ROOT: &str = "framekernel";
/// Leaf crates outside [`KERNEL_ROOT`] that the kernel links (the rest of the workspace is
/// the UEFI loader and host tools, N12's listed exceptions).
const KERNEL_LEAVES: &[&str] = &[
    "boot/info",
    "runtime/abi",
    "runtime/version",
    "sdk/directory",
    // The random-number generator's mechanism and policy (ADR-0016 R2): both forbid `unsafe`.
    "security/crypto",
    "security/random",
];
/// The kernel library's crate root, whose `forbid(unsafe_code)` covers every module above the
/// framework.
const KERNEL_LIB_ROOT: &str = "framekernel/src/lib.rs";

/// The files that may hold `unsafe`, and what each needs it for: the kernel's trusted base, file
/// by file (ADR-0011 N6, N12). All are under [`FRAMEWORK`].
///
/// A file comes onto this list with a reason a reviewer accepts, and leaves it when its last
/// `unsafe` goes: the gate fails on a listed file that holds none.
const UNSAFE_FILES: &[(&str, &str)] = &[
    (
        "framekernel/frame/entry/main.rs",
        "the kernel image's link root: `_start`, the symbol the boot loader jumps to",
    ),
    (
        "framekernel/frame/src/cpu.rs",
        "the processor: descriptor tables, control registers, MSRs, trap entry, FPU state, probes",
    ),
    (
        "framekernel/frame/src/dma.rs",
        "pages a device reads and writes: allocation, and volatile access to memory a device shares",
    ),
    (
        "framekernel/frame/src/early.rs",
        "early boot: the loader's boot information, read through the pointer it hands over",
    ),
    (
        "framekernel/frame/src/iommu.rs",
        "Intel VT-d: remapping structures the hardware reads, and their cache-line flushes",
    ),
    (
        "framekernel/frame/src/iommu/amd.rs",
        "AMD-Vi: remapping structures the hardware reads",
    ),
    (
        "framekernel/frame/src/irq.rs",
        "device interrupt handlers kept in atomics: one function pointer made from its stored bits",
    ),
    (
        "framekernel/frame/src/memory.rs",
        "the kernel's `GlobalAlloc`, and heap addresses turned into pointers",
    ),
    (
        "framekernel/frame/src/mmio.rs",
        "device registers: volatile access to mapped device memory",
    ),
    (
        "framekernel/frame/src/objref.rs",
        "a reference-counted object with a fallible constructor: raw allocation, `Send` and `Sync`",
    ),
    (
        "framekernel/frame/src/once.rs",
        "a value set once and read without a lock: `UnsafeCell`, `Sync`",
    ),
    (
        "framekernel/frame/src/port.rs",
        "x86 port I/O: `in` and `out`",
    ),
    (
        "framekernel/frame/src/proof_programs.rs",
        "the ring-3 proof programs: assembly carried as data, and the linker's symbols around it",
    ),
    (
        "framekernel/frame/src/random.rs",
        "the random-number generator: RDSEED and RDRAND, its own stack for each step, volatile overwrites",
    ),
    (
        "framekernel/frame/src/smp.rs",
        "application-processor start-up: the real-mode trampoline, its data page, the AP entry",
    ),
    (
        "framekernel/frame/src/stack.rs",
        "kernel stacks and FPU areas: raw allocation, guard pages, first frames",
    ),
    (
        "framekernel/frame/src/timer.rs",
        "the local APIC and the timers: MSRs, ports, interrupt-controller registers",
    ),
    (
        "framekernel/frame/src/tlb.rs",
        "TLB shootdown on the machine: IPIs, global pages",
    ),
    (
        "framekernel/frame/src/userspace.rs",
        "user address spaces: page-table frames reached through the direct map, user copies",
    ),
    (
        "framekernel/frame/src/vm.rs",
        "the kernel's own address space: installing and changing live page tables",
    ),
    (
        "framekernel/frame/sync/src/lib.rs",
        "the locks: `UnsafeCell`, `Send` and `Sync` for the guards",
    ),
    (
        "framekernel/frame/sync/src/models.rs",
        "the Loom models (tests only): a frame written through Loom's cell",
    ),
    (
        "framekernel/frame/sync/tests/ports_allocation.rs",
        "a host test's own global allocator, which counts and refuses on demand",
    ),
    (
        "framekernel/frame/tests/objref_out_of_memory.rs",
        "a host test's own global allocator, which refuses on demand",
    ),
];

/// The crates that run in ring 3 (CLAUDE.md §11; tasks S4-1 and S4-2): `origin.core`, the
/// runtime and the bodies of its `mem*` functions, the generated client, and the kernel's Rust
/// proof programs.
const RING3_CRATES: &[&str] = &[
    "runtime/core",
    "runtime/kernel-client",
    "runtime/rt",
    "runtime/rt-mem",
    "tests/kernel/first-program",
    "tests/kernel/pin-program",
];

/// The ring-3 files that may hold `unsafe`, and what each needs it for. A file comes onto this list
/// with a reason a reviewer accepts, and leaves it when its last `unsafe` goes.
const RING3_UNSAFE_FILES: &[(&str, &str)] = &[
    (
        "runtime/rt/src/entry.rs",
        "the runtime's entry: `_start`, the start record read where RDI points, and the program's `main` by its one name",
    ),
    (
        "runtime/rt/src/mem.rs",
        "`memcpy`, `memmove`, `memset`, `memcmp` and `bcmp`, which the compiler calls and no C library supplies",
    ),
    (
        "runtime/rt-mem/src/lib.rs",
        "the bodies of those five functions: `rep movsb`, `rep stosb` and the byte reads of a comparison, on raw pointers",
    ),
    (
        "runtime/rt-mem/src/tests.rs",
        "the host tests of those bodies, which are unsafe to call, against `core::ptr::copy`, and the direction flag read after a copy down",
    ),
    (
        "tests/kernel/first-program/src/faults.rs",
        "the proof's deliberate faults: a write to its own code, a jump into its own data and a write to its relocated read-only data",
    ),
    (
        "tests/kernel/pin-program/src/calls.rs",
        "each entry point of `origin.core` declared with six whole registers, so that the pin proof's scripts can pass a stub any register values, hostile ones among them",
    ),
    (
        "tests/kernel/pin-program/src/raw.rs",
        "the pin proof's script and scratch memory at the kernel's fixed addresses; its entry, which reads RFLAGS first; and the two calls the pin must stop: its one `syscall` of its own, and a jump into a stub",
    ),
];

/// The ring-3 crate roots, each of which denies `unsafe`; only the modules of listed files allow
/// it. (`runtime/core` and `runtime/kernel-client` cannot: each includes generated shims, the
/// stubs and the client's imports, which are not committed and so are not read here. Their own
/// committed files hold no `unsafe`, which this gate checks like any other file's.)
const RING3_ROOTS: &[&str] = &[
    "runtime/rt/src/lib.rs",
    "tests/kernel/first-program/src/main.rs",
    "tests/kernel/pin-program/src/main.rs",
];
/// What each of [`RING3_ROOTS`] carries.
const DENY: &str = "#![deny(unsafe_code)]";

/// Files under [`FRAMEWORK`] that hold no `unsafe` and still cannot carry the attribute: it would
/// reach the listed modules declared beneath them, and the crate would not compile. The compiler
/// cannot hold these to the rule, so the gate counts their sites itself.
const ABOVE_UNSAFE_FILES: &[&str] = &["framekernel/frame/src/lib.rs"];

/// What each kernel crate may depend on, directly or not (normal edges, packages in this
/// workspace). Every package under `framekernel/` must be listed: a crate of unknown place
/// in the layering fails the gate. `origin-frame` is the framework: the crates it may use
/// are the leaves below it, and everything that is the kernel proper sits above it.
const LAYERS: &[(&str, &[&str])] = &[
    ("origin-version", &[]),
    ("origin-boot-info", &["origin-version"]),
    ("origin-abi", &["origin-version"]),
    ("origin-sync", &[]),
    ("origin-crypto", &[]),
    ("origin-random", &["origin-crypto"]),
    ("origin-syscalls", &[]),
    // The service directory's generated client and stub, which a boot proof uses.
    ("origin-directory", &[]),
    ("origin-paging", &["origin-boot-info", "origin-version"]),
    (
        "origin-frame",
        &[
            "origin-abi",
            "origin-boot-info",
            "origin-crypto",
            "origin-paging",
            "origin-random",
            "origin-sync",
            "origin-version",
        ],
    ),
    (
        "framekernel",
        &[
            "origin-abi",
            "origin-boot-info",
            "origin-crypto",
            "origin-frame",
            "origin-paging",
            "origin-directory",
            "origin-random",
            "origin-sync",
            "origin-syscalls",
            "origin-version",
        ],
    ),
];

/// The kernel crates that sit directly above the framework: they must depend on it.
const ABOVE_THE_FRAMEWORK: &[&str] = &["framekernel"];
/// Only the framework may use the lock package directly: everything else takes it through
/// `origin_frame::sync`.
const FRAMEWORK_PACKAGE: &str = "origin-frame";
const LOCK_PACKAGE: &str = "origin-sync";
/// What every kernel file off [`UNSAFE_FILES`] carries, itself or through its crate's root.
const FORBID: &str = "#![forbid(unsafe_code)]";

/// The gate: see the module documentation.
pub(crate) fn gate() -> Result {
    let root = repo_root();
    let scanned = scan(&root)?;
    let problems = site_problems(&scanned.outside, scanned.framework_sites);
    if !problems.is_empty() {
        return Err(format!(
            "`unsafe` outside the framework (ADR-0011 N6, N13):\n  {}",
            problems.join("\n  ")
        ));
    }
    let problems = framework_problems(&scanned.framework);
    if !problems.is_empty() {
        return Err(format!(
            "`unsafe` in the framework is for the files the gate lists, and every other file there forbids it (CLAUDE.md §13, decision item 111; the list is UNSAFE_FILES in developer-tools/xtask/src/unsafety.rs):\n  {}",
            problems.join("\n  ")
        ));
    }
    let problems = root_problems(&root)?;
    if !problems.is_empty() {
        return Err(format!(
            "a kernel crate outside the framework must forbid `unsafe` at its root (ADR-0011 N6):\n  {}",
            problems.join("\n  ")
        ));
    }
    let problems = dependency_problems(&root)?;
    if !problems.is_empty() {
        return Err(format!(
            "a kernel dependency runs the wrong way (ADR-0011 N6, N13):\n  {}",
            problems.join("\n  ")
        ));
    }
    let ring3 = read_ring3(&root)?;
    let problems = ring3_problems(&ring3);
    if !problems.is_empty() {
        return Err(format!(
            "`unsafe` in ring 3 is for the files the gate lists (RING3_UNSAFE_FILES in developer-tools/xtask/src/unsafety.rs; task S4-1):\n  {}",
            problems.join("\n  ")
        ));
    }
    let ring3_sites: usize = ring3.values().map(|text| count_sites(text)).sum();
    println!(
        "xtask: unsafe gate: in ring 3, {ring3_sites} sites in the {} files listed and none in the other {} files of {} crates, whose {} roots deny it",
        RING3_UNSAFE_FILES.len(),
        ring3.len().saturating_sub(RING3_UNSAFE_FILES.len()),
        RING3_CRATES.len(),
        RING3_ROOTS.len()
    );
    let forbidding = scanned.framework.values().filter(|f| f.forbids).count();
    println!(
        "xtask: unsafe gate: no `unsafe` outside the framework; in it, {} sites in the {} files listed, and none in its other {}, of which {forbidding} start with `{FORBID}` and {} cannot, being above listed modules ({} kernel source files scanned)",
        scanned.framework_sites,
        UNSAFE_FILES.len(),
        scanned.framework.len().saturating_sub(UNSAFE_FILES.len()),
        ABOVE_UNSAFE_FILES.len(),
        scanned.files
    );
    // What the assembly inside those sites promises the compiler about the flags, and where
    // RDRAND and RDSEED may be run.
    asmflags::gate()?;
    mnemonics::gate()
}

/// What one file of the framework holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FrameworkFile {
    /// Its `unsafe` sites.
    sites: usize,
    /// Whether it starts with the attribute.
    forbids: bool,
}

/// What [`scan`] read.
#[derive(Debug)]
struct Scanned {
    /// Files outside the framework that hold `unsafe`, by repository path, with their sites.
    outside: BTreeMap<String, usize>,
    /// Every file of the framework, by repository path.
    framework: BTreeMap<String, FrameworkFile>,
    /// The framework's sites in all.
    framework_sites: usize,
    /// How many files were read.
    files: usize,
}

/// Reads every kernel source file.
fn scan(root: &Path) -> Result<Scanned> {
    let mut files = Vec::new();
    let framework = root.join(FRAMEWORK);
    for dir in std::iter::once(KERNEL_ROOT).chain(KERNEL_LEAVES.iter().copied()) {
        collect_files(
            &root.join(dir),
            &|_| false,
            &|name| name.ends_with(".rs"),
            &mut files,
        )
        .map_err(|e| format!("unsafe gate: {dir}: {e}"))?;
    }
    if files.is_empty() {
        return Err(format!(
            "unsafe gate: no Rust files under {KERNEL_ROOT}: nothing was checked"
        ));
    }
    let mut scanned = Scanned {
        outside: BTreeMap::new(),
        framework: BTreeMap::new(),
        framework_sites: 0,
        files: files.len(),
    };
    for file in &files {
        let text = fs::read_to_string(file)
            .map_err(|e| format!("unsafe gate: {}: {e}", file.display()))?;
        let sites = count_sites(&text);
        let relative = file
            .strip_prefix(root)
            .map_err(|e| format!("unsafe gate: {}: {e}", file.display()))?
            .to_string_lossy()
            .into_owned();
        if file.starts_with(&framework) {
            scanned.framework_sites += sites;
            let forbids = carries_forbid(&text);
            scanned
                .framework
                .insert(relative, FrameworkFile { sites, forbids });
        } else if sites > 0 {
            scanned.outside.insert(relative, sites);
        }
    }
    Ok(scanned)
}

/// Whether `source` starts with `#![forbid(unsafe_code)]`: among the inner attributes that open
/// the file, before any item. The text in a comment or a string is not the attribute, and neither
/// is one inside a module or a function further down, which covers only that.
fn carries_forbid(source: &str) -> bool {
    let tokens = lex::tokens(source);
    let mut i = 0;
    while tokens.get(i).is_some_and(|t| t.is_punct('#'))
        && tokens.get(i + 1).is_some_and(|t| t.is_punct('!'))
        && tokens.get(i + 2).is_some_and(|t| t.is_punct('['))
    {
        let Some(close) = lex::closing(&tokens, i + 2) else {
            return false;
        };
        if let Some([forbid, open, lint, shut]) = tokens.get(i + 3..close)
            && forbid.is_ident("forbid")
            && open.is_punct('(')
            && lint.is_ident("unsafe_code")
            && shut.is_punct(')')
        {
            return true;
        }
        i = close + 1;
    }
    false
}

/// What is wrong among the framework's files: `unsafe` in a file off the list, a file off the
/// list that does not forbid it, or a list that names a file with none.
fn framework_problems(files: &BTreeMap<String, FrameworkFile>) -> Vec<String> {
    let mut problems = Vec::new();
    for &(listed, _) in UNSAFE_FILES {
        match files.get(listed) {
            None => problems.push(format!(
                "{listed} is on the list and does not exist: take it off"
            )),
            Some(file) if file.sites == 0 => problems.push(format!(
                "{listed} is on the list and holds no `unsafe`: take it off, and start it with `{FORBID}`"
            )),
            Some(_) => {}
        }
    }
    for &above in ABOVE_UNSAFE_FILES {
        if !files.contains_key(above) {
            problems.push(format!(
                "{above} is named as a file above the listed ones and does not exist"
            ));
        }
    }
    for (path, file) in files {
        if UNSAFE_FILES.iter().any(|&(listed, _)| listed == path) {
            continue;
        }
        if file.sites > 0 {
            problems.push(format!(
                "{path}: {} `unsafe` sites in a file that is not on the list",
                file.sites
            ));
        }
        if !file.forbids && !ABOVE_UNSAFE_FILES.contains(&path.as_str()) {
            problems.push(format!(
                "{path} does not start with `{FORBID}`, and is not on the list"
            ));
        }
    }
    problems
}

/// The library root of each kernel crate outside the framework, which must carry the attribute:
/// `src/lib.rs` beside each manifest.
fn root_problems(root: &Path) -> Result<Vec<String>> {
    let framework = root.join(FRAMEWORK);
    let mut manifests = Vec::new();
    for dir in std::iter::once(KERNEL_ROOT).chain(KERNEL_LEAVES.iter().copied()) {
        collect_files(
            &root.join(dir),
            &|_| false,
            &|name| name == "Cargo.toml",
            &mut manifests,
        )
        .map_err(|e| format!("unsafe gate: {dir}: {e}"))?;
    }
    let mut problems = Vec::new();
    let mut checked = 0;
    for manifest in manifests.iter().filter(|m| !m.starts_with(&framework)) {
        let lib = manifest.with_file_name("src").join("lib.rs");
        let shown = lib.strip_prefix(root).unwrap_or(&lib).display().to_string();
        match fs::read_to_string(&lib) {
            Ok(text) if carries_forbid(&text) => checked += 1,
            Ok(_) => problems.push(format!("{shown} does not start with `{FORBID}`")),
            Err(e) => problems.push(format!(
                "{shown}: {e} (the gate looks for a kernel crate's library root there)"
            )),
        }
    }
    if checked == 0 && problems.is_empty() {
        problems.push(format!(
            "no kernel crate found outside {FRAMEWORK}: the gate's roots are wrong"
        ));
    }
    if !root.join(KERNEL_LIB_ROOT).is_file() {
        problems.push(format!("{KERNEL_LIB_ROOT} does not exist"));
    }
    Ok(problems)
}

/// Counts `unsafe` tokens and `global_asm!` invocations in `source`, outside comments, strings
/// and character literals. Raw identifiers (`r#unsafe`) are not the keyword and do not count.
pub(crate) fn count_sites(source: &str) -> usize {
    let tokens = lex::tokens(source);
    tokens
        .iter()
        .enumerate()
        .filter(|&(i, token)| {
            token.is_ident("unsafe")
                || (token.is_ident("global_asm")
                    && tokens.get(i + 1).is_some_and(|next| next.is_punct('!')))
        })
        .count()
}

/// What is wrong with the `unsafe` the scan found: any of it outside the framework, or none
/// of it inside.
fn site_problems(outside: &BTreeMap<String, usize>, framework_sites: usize) -> Vec<String> {
    let mut problems = Vec::new();
    if framework_sites == 0 {
        // The gate would otherwise read as passing if it were looking in the wrong place.
        problems.push(format!(
            "the framework ({FRAMEWORK}) has no `unsafe` at all: the gate's roots are wrong"
        ));
    }
    for (path, count) in outside {
        problems.push(format!(
            "{path}: {count} `unsafe` sites (move them into {FRAMEWORK})"
        ));
    }
    problems
}

/// Every Rust file of the ring-3 crates, by repository path, with its text.
fn read_ring3(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut paths = Vec::new();
    for dir in RING3_CRATES {
        collect_files(
            &root.join(dir),
            &|_| false,
            &|name| name.ends_with(".rs"),
            &mut paths,
        )
        .map_err(|e| format!("unsafe gate: {dir}: {e}"))?;
    }
    let mut files = BTreeMap::new();
    for path in paths {
        let shown = path
            .strip_prefix(root)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        files.insert(shown, read(&path)?);
    }
    Ok(files)
}

/// What is wrong with `files`, the ring-3 crates' Rust files: `unsafe` in a file off the list, a
/// listed file with none or gone, or a root that does not deny it.
fn ring3_problems(files: &BTreeMap<String, String>) -> Vec<String> {
    let mut problems = Vec::new();
    if files.is_empty() {
        problems.push("no ring-3 Rust files found: the gate's roots are wrong".to_string());
    }
    for (path, text) in files {
        let sites = count_sites(text);
        let listed = RING3_UNSAFE_FILES.iter().any(|(p, _)| p == path);
        if sites != 0 && !listed {
            problems.push(format!(
                "{path}: {sites} `unsafe` sites, and it is not on the list"
            ));
        }
    }
    for (path, _) in RING3_UNSAFE_FILES {
        match files.get(*path) {
            None => problems.push(format!("{path}: listed, and not found")),
            Some(text) if count_sites(text) == 0 => {
                problems.push(format!(
                    "{path}: listed, and holds no `unsafe`: take it off"
                ));
            }
            Some(_) => {}
        }
    }
    for path in RING3_ROOTS {
        let denies = files
            .get(*path)
            .is_some_and(|text| text.lines().any(|line| line.trim() == DENY));
        if !denies {
            problems.push(format!("{path}: a ring-3 crate root without `{DENY}`"));
        }
    }
    problems
}

fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Each package named by a `Cargo.toml` under [`KERNEL_ROOT`].
fn kernel_packages(root: &Path) -> Result<BTreeSet<String>> {
    let mut manifests = Vec::new();
    collect_files(
        &root.join(KERNEL_ROOT),
        &|_| false,
        &|name| name == "Cargo.toml",
        &mut manifests,
    )
    .map_err(|e| format!("unsafe gate: {KERNEL_ROOT}: {e}"))?;
    let mut names = BTreeSet::new();
    for manifest in &manifests {
        let text = read(manifest)?;
        let name = text
            .lines()
            .find_map(|line| line.strip_prefix("name = "))
            .map(|value| value.trim_matches('"').to_string())
            .ok_or_else(|| format!("{}: no package name", manifest.display()))?;
        names.insert(name);
    }
    Ok(names)
}

/// The dependency direction, from `cargo tree` (normal edges, every target and feature).
fn dependency_problems(root: &Path) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    let layered: BTreeSet<&str> = LAYERS.iter().map(|(name, _)| *name).collect();
    for name in kernel_packages(root)? {
        if !layered.contains(name.as_str()) {
            problems.push(format!(
                "{name} has no entry in xtask's LAYERS table: say where it sits"
            ));
        }
    }
    for &(package, allowed) in LAYERS {
        let tree = dependencies_of(package)?;
        for dependency in tree.iter().filter(|d| !allowed.contains(&d.as_str())) {
            problems.push(format!(
                "{package} depends on {dependency}, which its layer does not allow"
            ));
        }
        if ABOVE_THE_FRAMEWORK.contains(&package) && !tree.contains(FRAMEWORK_PACKAGE) {
            problems.push(format!("{package} does not depend on {FRAMEWORK_PACKAGE}"));
        }
        // The lock package is the framework's: everyone else uses `origin_frame::sync`.
        if package != FRAMEWORK_PACKAGE && direct_dependencies_of(package)?.contains(LOCK_PACKAGE) {
            problems.push(format!(
                "{package} names {LOCK_PACKAGE} directly: use `origin_frame::sync`"
            ));
        }
    }
    Ok(problems)
}

/// Every package `package` depends on, directly or not.
fn dependencies_of(package: &str) -> Result<BTreeSet<String>> {
    tree_lines(package, None)
}

/// The packages `package` names in its own manifest.
fn direct_dependencies_of(package: &str) -> Result<BTreeSet<String>> {
    tree_lines(package, Some("1"))
}

fn tree_lines(package: &str, depth: Option<&str>) -> Result<BTreeSet<String>> {
    let mut command = cargo();
    command.args([
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
    ]);
    if let Some(depth) = depth {
        command.args(["--depth", depth]);
    }
    let output = command
        .output()
        .map_err(|e| format!("cannot run cargo tree: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo tree failed for {package}: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let tree = String::from_utf8_lossy(&output.stdout);
    let names: BTreeSet<String> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| *name != package)
        .map(str::to_string)
        .collect();
    // `cargo tree` prints the package itself first: an empty result means it saw nothing.
    if tree.trim().is_empty() {
        return Err(format!("cargo tree saw nothing for {package}"));
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring3_unsafe_is_only_in_the_listed_files() {
        let clean = || -> BTreeMap<String, String> {
            let mut files = BTreeMap::new();
            for (path, _) in RING3_UNSAFE_FILES {
                files.insert((*path).to_string(), "unsafe { x() }".to_string());
            }
            for path in RING3_ROOTS {
                files.insert((*path).to_string(), format!("//! a root\n{DENY}\n"));
            }
            files.insert(
                "runtime/rt/src/panic.rs".to_string(),
                "fn panic() {} // unsafe in a comment".to_string(),
            );
            files
        };
        assert_eq!(ring3_problems(&clean()), Vec::<String>::new());
        // An `unsafe` planted in a file off the list.
        let mut planted = clean();
        planted.insert(
            "runtime/rt/src/panic.rs".to_string(),
            "fn panic() { unsafe { y() } }".to_string(),
        );
        assert_eq!(
            ring3_problems(&planted),
            ["runtime/rt/src/panic.rs: 1 `unsafe` sites, and it is not on the list"]
        );
        // A root that no longer denies it.
        let mut open = clean();
        open.insert(RING3_ROOTS[1].to_string(), "//! a root\n".to_string());
        assert_eq!(ring3_problems(&open).len(), 1);
        // A listed file with none, and one gone.
        let mut emptied = clean();
        emptied.insert(RING3_UNSAFE_FILES[0].0.to_string(), "fn a() {}".to_string());
        emptied.remove(RING3_UNSAFE_FILES[1].0);
        assert_eq!(ring3_problems(&emptied).len(), 2);
    }

    #[test]
    fn the_ring3_gate_sees_the_real_tree() {
        let files = read_ring3(&repo_root()).expect("the ring-3 crates");
        assert_eq!(ring3_problems(&files), Vec::<String>::new());
        assert!(files.contains_key("runtime/rt/src/entry.rs"));
        assert!(
            files
                .keys()
                .all(|path| RING3_CRATES.iter().any(|c| path.starts_with(c)))
        );
    }

    #[test]
    fn counts_the_keyword_and_global_asm_in_code() {
        let source = r#"
            unsafe fn f() {}
            pub unsafe extern "C" fn g() {}
            #[unsafe(no_mangle)] extern "C" fn h() {}
            unsafe impl Send for S {}
            fn i() { unsafe { asm!("nop") } }
            global_asm!("nop");
        "#;
        assert_eq!(count_sites(source), 6);
    }

    #[test]
    fn counts_nothing_in_comments_strings_or_chars() {
        let source = r##"
            // an unsafe line comment
            //! unsafe in docs
            /// and unsafe in docs
            /* unsafe, /* nested unsafe */ still unsafe */
            let s = "unsafe in a string, with an escaped \" unsafe quote";
            let r = r#"unsafe raw"unsafe"#;
            let b = b"unsafe bytes";
            let br = br#"unsafe bytes"#;
            let c = c"unsafe";
            let ch = 'u';
            let q = '"';
            let e = '\'';
            let g = global_asm_like; // no bang
            fn r#unsafe() {}
        "##;
        assert_eq!(count_sites(source), 0, "{source}");
    }

    #[test]
    fn lifetimes_are_not_character_literals() {
        // A lifetime must not swallow the code after it.
        assert_eq!(count_sites("fn f<'a>(x: &'a u8) { unsafe {} }"), 1);
        assert_eq!(count_sites("let c = 'x'; unsafe {}"), 1);
    }

    #[test]
    fn a_word_containing_unsafe_is_not_the_keyword() {
        assert_eq!(count_sites("let unsafely = 1; let not_unsafe = 2;"), 0);
    }

    fn map(entries: &[(&str, usize)]) -> BTreeMap<String, usize> {
        entries.iter().map(|&(p, c)| (p.to_string(), c)).collect()
    }

    #[test]
    fn unsafe_outside_the_framework_fails_and_a_clean_tree_passes() {
        assert!(site_problems(&map(&[]), 5).is_empty());
        let found = site_problems(&map(&[("a.rs", 2)]), 5);
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("a.rs: 2"), "{found:?}");
        // A framework with nothing in it means the gate is looking in the wrong place.
        let empty = site_problems(&map(&[]), 0);
        assert!(empty[0].contains("gate's roots are wrong"), "{empty:?}");
    }

    fn framework(entries: &[(&str, usize, bool)]) -> BTreeMap<String, FrameworkFile> {
        // Every listed file, each holding one site, and the root above them, then `entries`.
        let mut files: BTreeMap<String, FrameworkFile> = UNSAFE_FILES
            .iter()
            .map(|&(path, _)| {
                let file = FrameworkFile {
                    sites: 1,
                    forbids: false,
                };
                (path.to_string(), file)
            })
            .collect();
        for &above in ABOVE_UNSAFE_FILES {
            let file = FrameworkFile {
                sites: 0,
                forbids: false,
            };
            files.insert(above.to_string(), file);
        }
        for &(path, sites, forbids) in entries {
            files.insert(path.to_string(), FrameworkFile { sites, forbids });
        }
        files
    }

    #[test]
    fn the_attribute_counts_only_where_it_opens_the_file() {
        for source in [
            "#![forbid(unsafe_code)]\nfn f() {}",
            "//! Documentation first.\n\n#![forbid(unsafe_code)]\n\nuse core::fmt;",
            "/* a comment */ #![no_std]\n#![allow(dead_code)]\n#! [ forbid ( unsafe_code ) ]\nfn f() {}",
        ] {
            assert!(carries_forbid(source), "{source}");
        }
        for source in [
            "",
            "fn f() {}",
            // In a comment, in documentation, in a string.
            "// #![forbid(unsafe_code)]\nfn f() {}",
            "//! `#![forbid(unsafe_code)]` is what the modules above carry.\nfn f() {}",
            "const TEXT: &str = \"#![forbid(unsafe_code)]\";",
            // After an item: an inner attribute of something smaller than the file.
            "mod inner {\n    #![forbid(unsafe_code)]\n}\nfn f() { unsafe {} }",
            "fn f() {\n    #![forbid(unsafe_code)]\n}",
            // Weaker lints, another lint, a condition, an outer attribute.
            "#![deny(unsafe_code)]",
            "#![warn(unsafe_code)]",
            "#![forbid(unsafe_op_in_unsafe_fn)]",
            "#![forbid(unsafe_code, dead_code)]",
            "#![cfg_attr(not(test), forbid(unsafe_code))]",
            "#[forbid(unsafe_code)]\nfn f() {}",
            "#![forbid(unsafe_code)",
        ] {
            assert!(!carries_forbid(source), "{source}");
        }
    }

    #[test]
    fn the_listed_files_and_forbidding_ones_pass() {
        let files = framework(&[
            ("framekernel/frame/src/apic.rs", 0, true),
            ("framekernel/frame/sync/src/wait.rs", 0, true),
        ]);
        assert_eq!(framework_problems(&files), [""; 0]);
    }

    #[test]
    fn unsafe_in_a_file_off_the_list_is_caught() {
        // With the attribute the compiler refuses it first; the gate does not depend on that.
        for forbids in [true, false] {
            let files = framework(&[("framekernel/frame/src/apic.rs", 2, forbids)]);
            let found = framework_problems(&files);
            assert!(
                found
                    .iter()
                    .any(|p| p
                        .contains("apic.rs: 2 `unsafe` sites in a file that is not on the list")),
                "{found:?}"
            );
        }
        // The crate root holds none, and nothing but this count says so.
        let files = framework(&[("framekernel/frame/src/lib.rs", 1, false)]);
        let found = framework_problems(&files);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("src/lib.rs: 1 `unsafe` sites"),
            "{found:?}"
        );
    }

    #[test]
    fn a_file_off_the_list_without_the_attribute_is_caught() {
        // A new file, or one whose attribute was deleted.
        let files = framework(&[("framekernel/frame/src/new_driver.rs", 0, false)]);
        let found = framework_problems(&files);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("new_driver.rs does not start with `#![forbid(unsafe_code)]`"),
            "{found:?}"
        );
        // The root above the listed files is the one file excused, and only from that.
        assert_eq!(framework_problems(&framework(&[])), [""; 0]);
    }

    #[test]
    fn a_listed_file_with_no_unsafe_or_that_is_gone_is_caught() {
        let files = framework(&[("framekernel/frame/src/port.rs", 0, false)]);
        let found = framework_problems(&files);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("port.rs is on the list and holds no `unsafe`"),
            "{found:?}"
        );
        let mut files = framework(&[]);
        files.remove("framekernel/frame/src/port.rs");
        files.remove("framekernel/frame/src/lib.rs");
        let found = framework_problems(&files);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].contains("port.rs is on the list and does not exist"));
        assert!(found[1].contains("lib.rs is named as a file above the listed ones"));
    }

    #[test]
    fn the_list_is_sorted_under_the_framework_and_has_no_duplicates() {
        assert!(
            UNSAFE_FILES.windows(2).all(|w| w[0].0 < w[1].0),
            "the list is not in order"
        );
        for &(path, reason) in UNSAFE_FILES {
            assert!(path.starts_with(&format!("{FRAMEWORK}/")), "{path}");
            assert!(!reason.is_empty(), "{path} has no reason");
            assert!(
                !ABOVE_UNSAFE_FILES.contains(&path),
                "{path} is on both lists"
            );
        }
    }

    /// The real tree, so the gate's roots are known to find the real files.
    #[test]
    fn the_gate_sees_the_real_tree() {
        let root = repo_root();
        let scanned = scan(&root).expect("the scan");
        assert!(scanned.framework_sites > 0, "the framework has no unsafe");
        assert!(scanned.files > 50, "only {} files scanned", scanned.files);
        // Nothing is left outside the framework, and the library's root says so.
        assert!(scanned.outside.is_empty(), "{:?}", scanned.outside);
        assert!(carries_forbid(
            &read(&root.join(KERNEL_LIB_ROOT)).expect("the root")
        ));
        // Inside it, the list is exact, and every other file forbids `unsafe`.
        assert_eq!(framework_problems(&scanned.framework), [""; 0]);
        assert!(scanned.framework.len() > UNSAFE_FILES.len() + 20);
        assert_eq!(root_problems(&root).expect("the roots"), [""; 0]);
        let names = kernel_packages(&root).expect("the packages");
        assert!(names.contains("origin-frame") && names.contains("framekernel"));
    }

    /// Violations planted in the text of real files: the attribute deleted from one that carries
    /// it, and an `unsafe` block added to it.
    #[test]
    fn a_real_file_changed_for_the_worse_is_caught() {
        let root = repo_root();
        let path = "framekernel/frame/src/apic.rs";
        let real = read(&root.join(path)).expect("the file");
        assert!(carries_forbid(&real) && count_sites(&real) == 0);
        let mut scanned = scan(&root).expect("the scan");

        let without = real.replace("#![forbid(unsafe_code)]\n", "");
        assert_ne!(without, real);
        let file = FrameworkFile {
            sites: count_sites(&without),
            forbids: carries_forbid(&without),
        };
        scanned.framework.insert(path.to_string(), file);
        let found = framework_problems(&scanned.framework);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("apic.rs does not start with"),
            "{found:?}"
        );

        let with_unsafe = format!(
            "{real}\nfn planted() {{ unsafe {{ core::hint::unreachable_unchecked() }} }}\n"
        );
        let file = FrameworkFile {
            sites: count_sites(&with_unsafe),
            forbids: carries_forbid(&with_unsafe),
        };
        scanned.framework.insert(path.to_string(), file);
        let found = framework_problems(&scanned.framework);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("apic.rs: 1 `unsafe` sites"), "{found:?}");
    }
}
