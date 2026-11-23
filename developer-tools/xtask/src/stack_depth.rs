//! TASKS item 127: `cargo xtask stack-depth`, how deep Origin's loader goes on the firmware's stack
//! before `ExitBootServices`, under QEMU, against the 128 KiB UEFI promises an image (UEFI 2.11
//! §2.3.4).
//!
//! Two figures, the coordinating session's rulings Q1 to Q5:
//! - **The paint** (the loader's `stackdepth` module): the measurement build paints the promised
//!   stack at entry, and at the end of each of six phases says how deep the stack was written
//!   since the last paint: Origin's frames, the firmware's (OVMF's here) inside every service the
//!   loader called, and the firmware's timer interrupt. A calibration on a known 64 KiB array
//!   comes first, the positive control.
//! - **The static chain** ([`chain`]): from the measurement build's link map and its disassembly,
//!   the longest chain of the loader's own frames from `efi_main` over direct calls, with indirect
//!   calls (into the firmware, or through a function pointer) as leaves. The paint's figure is then
//!   split into Origin's part and the firmware's.
//!
//! The measurement build is the developer-mode loader, normal or bench, with one more flag,
//! `--cfg origin_stack_measure` ([`CFG`]), into `target/stack-measure` ([`target`]). Its lines
//! all say "stack measure" ([`MARKER`]): TASKS item 112's F2 refuses that text in every normal and
//! bench image, and the rental tool in any image, and F1 holds the flag's name to [`FILES`]. No
//! signing path takes a file from [`target`] (`bench::refuse`). It runs on demand only: neither
//! `check` nor `boot-matrix` runs it (ruling Q5).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Instant;

use crate::bench::{self, Insn};
use crate::settings::Settings;
use crate::{
    Capture, Completed, Loader, Result, Scenario, qemu_boot_failures, read_file, run_scenario,
    toolchain,
};

/// The text every measurement image holds and no other image may (F2).
pub(crate) const MARKER: &[u8] = b"stack measure";

/// The flag's name, which only [`FILES`] may hold (F1).
pub(crate) const CFG: &str = "origin_stack_measure";

/// The files that may name the measurement flag, each for a reason (F1, as for the bench flag).
pub(crate) const FILES: &[(&str, &str)] = &[
    ("Cargo.toml", "the flag's check-cfg declaration"),
    ("boot/uefi/src/lib.rs", "the measurement's module"),
    (
        "boot/uefi/src/stackdepth.rs",
        "the measurement's rules and lines",
    ),
    (
        "boot/uefi/src/main.rs",
        "the paint, the scans and the hooks",
    ),
    (
        "developer-tools/xtask/src/toolchain.rs",
        "the one place the flag is given",
    ),
    ("developer-tools/xtask/src/stack_depth.rs", "this command"),
];

/// What UEFI promises an image while boot services run (UEFI 2.11 §2.3.4).
const PROMISED: u64 = 128 * 1024;

/// How many times each scenario boots (ruling Q4).
const BOOTS: usize = 3;

/// The processor QEMU reports for the bench loader as a rental runs it: rental A's EPYC 4244P, so
/// that the loader takes the AMD branch and scans the DSDT, as on the rented board.
const RENTED_CPU: &str = crate::CPU_IDENTITIES[1].0;

/// Every line's start, and the six phases' and the summary's.
const LINE: &str = "origin-boot: stack measure: ";
const CALIBRATION: &str = "origin-boot: stack measure: calibration: ";
const SUMMARY: &str = "origin-boot: stack measure: deepest before the call: ";
/// What a calibration that holds ends with.
const CALIBRATION_HOLDS: &str = ", within the 512 allowed";

/// Where measurement builds go, apart from every other build.
pub(crate) fn target() -> PathBuf {
    crate::target_dir().join("stack-measure")
}

/// Whether `path` is under [`target`].
pub(crate) fn is_measure(path: &Path) -> bool {
    let root = target();
    path.starts_with(&root)
        || fs::canonicalize(path)
            .ok()
            .zip(fs::canonicalize(&root).ok())
            .is_some_and(|(path, root)| path.starts_with(root))
}

/// The target directory of the normal or the bench measurement build.
fn flavour(bench: bool) -> PathBuf {
    target().join(if bench { "bench" } else { "normal" })
}

/// Where the linker writes the measurement build's map.
fn map_path(bench: bool) -> PathBuf {
    flavour(bench).join("origin-boot.map")
}

/// The measurement loader: the developer-mode loader with the measurement flag, and with `bench`
/// the bench flag too, into its own directory under [`target`]. Fails unless it holds
/// [`MARKER`], and the bench build's text exactly when it is the bench one.
pub(crate) fn build_loader(bench: bool) -> Result<PathBuf> {
    let dir = flavour(bench);
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let efi = crate::build_loader_with(
        Some(Loader::Developer.feature()),
        Some(&dir),
        &toolchain::stack_measure_config(bench, &map_path(bench))?,
    )?;
    let bytes = read_file(&efi)?;
    if !bench::holds(&bytes, MARKER) {
        return Err(format!(
            "{} is a measurement image without the text \"stack measure\": F2's search in the other images could not see its code",
            efi.display()
        ));
    }
    if bench::holds(&bytes, bench::MARKER) != bench {
        return Err(format!(
            "{}: the {} measurement image {} the bench build's text",
            efi.display(),
            if bench { "bench" } else { "normal" },
            if bench { "lacks" } else { "holds" }
        ));
    }
    // ADR-0002 Amendment 7 (TASKS item 133's ruling Q4; its review, R133-5): a measurement
    // loader is a loader too.
    bench::check_no_os_types(&efi)?;
    Ok(efi)
}

/// Clippy over the measurement flag's code, which no other Clippy run sees: both loaders.
fn clippy() -> Result {
    for bench in [false, true] {
        let mut run = crate::cargo();
        run.args(["clippy", "--package", crate::LOADER_PACKAGE, "--target"])
            .arg(crate::UEFI_TARGET)
            .args(["--features", Loader::Developer.feature(), "--target-dir"])
            .arg(flavour(bench))
            .args(toolchain::stack_measure_config(bench, &map_path(bench))?)
            .args(["--", "-D", "warnings"]);
        crate::run_step(&mut run)?;
    }
    Ok(())
}

// ------------------------------------------------------------------ the paint's lines

/// A depth as a phase's line says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Depth {
    /// The phase stayed above the paint's end, no deeper than this.
    Above(u64),
    /// The lowest changed word was this many bytes below the entry RSP.
    Bytes(u64),
    /// The bottom of the promise was written: this many bytes or more.
    AtLeast(u64),
}

impl Depth {
    const fn bytes(self) -> u64 {
        match self {
            Self::Above(b) | Self::Bytes(b) | Self::AtLeast(b) => b,
        }
    }

    /// The depth in the text after a line's `: `, as the loader's `stackdepth` writes it.
    fn parse(text: &str) -> Option<Self> {
        let number = |s: &str| {
            s.split(|c: char| !c.is_ascii_digit())
                .find(|w| !w.is_empty())?
                .parse::<u64>()
                .ok()
        };
        if let Some(rest) = text.strip_prefix("no deeper than ") {
            number(rest).map(Self::Above)
        } else if text.contains(" bytes or more") {
            number(text).map(Self::AtLeast)
        } else if text.contains(" bytes of the ") {
            number(text).map(Self::Bytes)
        } else {
            None
        }
    }
}

impl std::fmt::Display for Depth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Above(b) => write!(f, "<= {b}"),
            Self::Bytes(b) => write!(f, "{b}"),
            Self::AtLeast(b) => write!(f, ">= {b}"),
        }
    }
}

/// What one boot's lines said.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Boot {
    /// The scenario's label.
    label: &'static str,
    /// How far below the calibration array its scan found the deepest change.
    calibration_below: u64,
    /// The six phases' depths, P1 first.
    phases: [Depth; 6],
}

/// The boots measured so far in this run, for the table at its end.
static BOOTS_SEEN: Mutex<Vec<Boot>> = Mutex::new(Vec::new());

/// The first line of `text` that starts with `start`, after it.
fn line_after<'a>(text: &'a str, start: &str) -> Option<&'a str> {
    text.lines()
        .map(|l| l.trim_end_matches('\r'))
        .find_map(|l| l.find(start).map(|at| &l[at + start.len()..]))
}

/// One boot's lines, read from `texts` (COM1's file and the SPCR's UART: under OVMF the
/// firmware's console reaches both, and the last phase's line goes to the SPCR's UART only).
fn read_boot(label: &'static str, texts: &[&str]) -> Result<Boot> {
    let find = |start: &str| texts.iter().find_map(|t| line_after(t, start));
    let calibration = find(CALIBRATION).ok_or(
        "no calibration line: the paint did not run, or the entry RSP could not hold the promise",
    )?;
    if !calibration.ends_with(CALIBRATION_HOLDS) {
        return Err(format!(
            "the calibration, the measurement's positive control, does not hold (TASKS item 127): {CALIBRATION}{calibration}"
        ));
    }
    let calibration_below = calibration
        .rsplit_once(": ")
        .and_then(|(_, tail)| tail.split(' ').next()?.parse::<u64>().ok())
        .ok_or_else(|| format!("the calibration's line cannot be read: {calibration}"))?;
    let mut phases = [Depth::Above(0); 6];
    for (i, phase) in phases.iter_mut().enumerate() {
        let start = format!("{LINE}P{} ", i + 1);
        let rest = find(&start).ok_or_else(|| format!("no line for phase P{}", i + 1))?;
        let text = rest
            .split_once(": ")
            .map(|(_, t)| t)
            .ok_or_else(|| format!("P{}'s line cannot be read: {rest}", i + 1))?;
        *phase = Depth::parse(text)
            .ok_or_else(|| format!("P{}'s depth cannot be read: {text}", i + 1))?;
    }
    find(SUMMARY).ok_or("no summary line")?;
    Ok(Boot {
        label,
        calibration_below,
        phases,
    })
}

/// A scenario's check: reads the boot's lines and keeps them for the table.
fn check(done: &Completed, _settings: &Settings, label: &'static str) -> Result {
    let com1 = done.com1.as_deref().unwrap_or_default();
    let boot = read_boot(label, &[com1, &done.serial])?;
    BOOTS_SEEN
        .lock()
        .map_err(|_| "the boots' record was poisoned".to_string())?
        .push(boot);
    Ok(())
}

fn check_normal(done: &Completed, settings: &Settings) -> Result {
    check(done, settings, NORMAL)
}

fn check_rental(done: &Completed, settings: &Settings) -> Result {
    check(done, settings, RENTAL)
}

fn check_fch(done: &Completed, settings: &Settings) -> Result {
    check(done, settings, FCH)
}

fn check_signed(done: &Completed, settings: &Settings) -> Result {
    check(done, settings, SIGNED)
}

const NORMAL: &str = "the normal loader";
const RENTAL: &str = "the bench loader as a rental runs it (rental A's EPYC reported)";
const FCH: &str = "the bench loader with BENCH-FCH's values (a run that makes the call)";
const SIGNED: &str =
    "the normal loader with a signed kernel (Ed25519 and ML-DSA-87 verified, as on a real boot)";

/// Which kernel a scenario boots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kernel {
    /// The normal kernel, unsigned: developer mode accepts it and verifies what it can, as on
    /// every rental.
    Unsigned,
    /// The normal kernel signed with the test key: developer mode verifies both signatures.
    Signed,
    /// The bench kernel, never signed.
    Bench,
}

/// The scenarios: ruling Q4's three, each run as the rentals run (an unsigned kernel), and the
/// normal loader with a signed kernel, the path a real boot takes, which verifies the signatures.
fn scenarios() -> [(Scenario, Kernel); 4] {
    let scenario = |label: &'static str,
                    loader: Loader,
                    qemu_args: crate::QemuArgs,
                    check: crate::Check| Scenario {
        quiet: None,
        label,
        loader,
        markers: vec![format!("{LINE}P6 "), SUMMARY.to_string()],
        failures: qemu_boot_failures(),
        check: Some(check),
        stderr: vec![],
        capture: Capture::SpcrUart,
        qemu_args: Some(qemu_args),
    };
    [
        (
            scenario(NORMAL, Loader::MeasureNormal, bench::spcr, check_normal),
            Kernel::Unsigned,
        ),
        (
            scenario(RENTAL, Loader::MeasureBench, bench::spcr, check_rental),
            Kernel::Bench,
        ),
        (
            scenario(FCH, Loader::MeasureBench, bench::fch_mine, check_fch),
            Kernel::Bench,
        ),
        (
            scenario(SIGNED, Loader::MeasureNormal, bench::spcr, check_signed),
            Kernel::Signed,
        ),
    ]
}

// ------------------------------------------------------------------ the static chain

/// One function of the measurement loader, from its link map.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Function {
    name: String,
    start: u64,
    /// The bytes its prologue puts on the stack: the return address, its pushes, and what it
    /// subtracts from RSP, probed or not.
    frame: u64,
    /// The functions it calls directly, and those it jumps to as a tail call.
    calls: Vec<usize>,
    tails: Vec<usize>,
    /// Whether it makes an indirect call: into the firmware, or through a function pointer.
    indirect: bool,
}

/// The longest chains the disassembly allows from `efi_main`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Chain {
    /// `efi_main`'s own frame, above the paint's top.
    entry_frame: u64,
    /// The longest chain of frames over direct calls, `efi_main` included, and its functions.
    longest: u64,
    path: Vec<(String, u64)>,
    /// The deepest point at which a function on a chain makes an indirect call, and its chain.
    to_indirect: u64,
    indirect_path: Vec<(String, u64)>,
    /// Calls that would close a cycle, left out (recursion).
    cycles: Vec<String>,
}

/// The symbols of a `lld-link /MAP` file: each public's name and its address (`Rva+Base`).
fn map_symbols(map: &str) -> Vec<(u64, String)> {
    let mut out: Vec<(u64, String)> = map
        .lines()
        .filter_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            let (section, name, address) = (words.first()?, words.get(1)?, words.get(2)?);
            let (s, o) = section.split_once(':')?;
            let hex = |w: &str, n: usize| w.len() == n && w.bytes().all(|b| b.is_ascii_hexdigit());
            if hex(s, 4) && hex(o, 8) && hex(address, 16) {
                u64::from_str_radix(address, 16)
                    .ok()
                    .map(|a| (a, (*name).to_string()))
            } else {
                None
            }
        })
        .collect();
    out.sort();
    out.dedup_by_key(|(a, _)| *a);
    out
}

/// A Rust symbol's path: a v0 symbol's (`_R`) through [`V0`], or a legacy one's (`_ZN`)
/// segments joined by `::` with the hash dropped; any other name, or a v0 symbol whose grammar
/// [`V0`] does not cover, as it is.
fn demangle(name: &str) -> String {
    if let Some(v0) = name.strip_prefix("_R").or_else(|| name.strip_prefix("__R")) {
        let mut p = V0 {
            s: v0.as_bytes(),
            at: 0,
        };
        return p
            .path()
            .map_or_else(|| name.to_string(), |parts| parts.join("::"));
    }
    let Some(mut rest) = name
        .strip_prefix("_ZN")
        .or_else(|| name.strip_prefix("__ZN"))
    else {
        return name.to_string();
    };
    let mut parts = Vec::new();
    while let Some(digits) = rest.find(|c: char| !c.is_ascii_digit()).filter(|&n| n > 0) {
        let Ok(len) = rest[..digits].parse::<usize>() else {
            break;
        };
        let Some(part) = rest.get(digits..digits + len) else {
            break;
        };
        parts.push(part);
        rest = &rest[digits + len..];
    }
    if parts
        .last()
        .is_some_and(|p| p.len() == 17 && p.starts_with('h'))
    {
        parts.pop();
    }
    if parts.is_empty() {
        return name.to_string();
    }
    // A segment that starts with an escape carries a `_` before it.
    parts
        .iter()
        .map(|p| {
            p.strip_prefix('_')
                .filter(|q| q.starts_with('$'))
                .unwrap_or(p)
        })
        .collect::<Vec<_>>()
        .join("::")
        .replace("$LT$", "<")
        .replace("$GT$", ">")
        .replace("$u20$", " ")
        .replace("$RF$", "&")
        .replace("$C$", ",")
        .replace("..", "::")
}

/// A reader of the part of Rust's v0 mangling a report needs: crate roots (`C`), nested paths
/// (`N`), inherent and trait impls (`M`, `X`) and back-references (`B`, shown as `…`). Generic
/// arguments, punycode and the rest give `None`, and the name is shown mangled.
struct V0<'a> {
    s: &'a [u8],
    at: usize,
}

impl V0<'_> {
    fn next(&mut self) -> Option<u8> {
        let c = *self.s.get(self.at)?;
        self.at += 1;
        Some(c)
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.at).copied()
    }

    /// A base-62 number up to its `_`.
    fn base62(&mut self) -> Option<()> {
        loop {
            match self.next()? {
                b'_' => return Some(()),
                c if c.is_ascii_alphanumeric() => {}
                _ => return None,
            }
        }
    }

    /// An optional disambiguator, `s` and a base-62 number.
    fn disambiguator(&mut self) -> Option<()> {
        if self.peek() == Some(b's') {
            self.at += 1;
            self.base62()?;
        }
        Some(())
    }

    /// An identifier: its disambiguator, its length, an `_` if the bytes start with a digit or
    /// `_`, and the bytes.
    fn ident(&mut self) -> Option<String> {
        self.disambiguator()?;
        let start = self.at;
        while self.peek()?.is_ascii_digit() {
            self.at += 1;
        }
        let len: usize = std::str::from_utf8(&self.s[start..self.at])
            .ok()?
            .parse()
            .ok()?;
        if self.peek() == Some(b'_') {
            self.at += 1;
        }
        let bytes = self.s.get(self.at..self.at + len)?;
        self.at += len;
        String::from_utf8(bytes.to_vec()).ok()
    }

    /// A path, as its segments.
    fn path(&mut self) -> Option<Vec<String>> {
        match self.next()? {
            b'C' => Some(vec![self.ident()?]),
            b'N' => {
                let namespace = self.next()?;
                let mut parts = self.path()?;
                let ident = self.ident()?;
                parts.push(match namespace {
                    b'C' => "{closure}".to_string(),
                    _ if ident.is_empty() => format!("{{{}}}", namespace as char),
                    _ => ident,
                });
                Some(parts)
            }
            b'M' => {
                self.disambiguator()?;
                let mut parts = self.path()?;
                let ty = self.path()?;
                parts.push(format!("<{}>", ty.join("::")));
                Some(parts)
            }
            b'X' => {
                self.disambiguator()?;
                let _impl_parent = self.path()?;
                let ty = self.path()?;
                let tr = self.path()?;
                Some(vec![format!("<{} as {}>", ty.join("::"), tr.join("::"))])
            }
            b'B' => {
                self.base62()?;
                Some(vec!["…".to_string()])
            }
            _ => None,
        }
    }
}

/// The immediate a `sub rsp, …` or `mov eax, …` holds.
fn immediate(operand: &str) -> Option<u64> {
    let operand = operand.trim();
    operand.strip_prefix("0x").map_or_else(
        || operand.parse().ok(),
        |hex| u64::from_str_radix(hex, 16).ok(),
    )
}

/// The bytes a function's prologue, `insns` from its start, puts on the stack below its return
/// address: its pushes before the first other instruction, and a `sub rsp, imm`, or a probed
/// `mov eax, imm` … `sub rsp, rax`, within its first sixteen instructions.
fn prologue(insns: &[Insn]) -> u64 {
    let pushes = insns
        .iter()
        .take_while(|i| i.text.starts_with("push "))
        .count() as u64;
    let mut eax = None;
    let mut sub = 0;
    for insn in insns.iter().take(16) {
        if let Some(op) = insn.text.strip_prefix("mov eax, ") {
            eax = immediate(op);
        } else if insn.text == "sub rsp, rax" {
            sub = eax.unwrap_or(0);
            break;
        } else if let Some(op) = insn.text.strip_prefix("sub rsp, ") {
            sub = immediate(op).unwrap_or(0);
            break;
        } else if insn.text.starts_with("ret") {
            break;
        }
    }
    pushes * 8 + sub
}

/// The target of a direct `call` or `jmp`, if `operands` is one.
fn direct_target(operands: &str) -> Option<u64> {
    let hex = operands.split_whitespace().next()?.strip_prefix("0x")?;
    u64::from_str_radix(hex, 16).ok()
}

/// Whether the function named `mangled` is the measurement's own (the loader's `measure` module
/// and the `stackdepth` rules), which the normal and bench loaders do not hold: its frames, the
/// calibration's 64 KiB among them, are left out of the chain.
fn is_measurement(mangled: &str) -> bool {
    mangled.contains("11origin_boot7measure") || mangled.contains("16origin_boot_uefi10stackdepth")
}

/// Whether the function named `mangled` is a stack probe: it walks down a frame its caller is
/// about to take, and puts nothing of its own below it, so its frame counts as none.
fn is_probe(mangled: &str) -> bool {
    mangled.contains("probestack") || mangled.contains("chkstk")
}

/// The loader's functions, from its link map's symbols and its disassembly. The measurement's own
/// functions keep their place, so no instruction is given to another function, but have no frame
/// and no calls.
fn functions(symbols: &[(u64, String)], insns: &[Insn]) -> Vec<Function> {
    let starts: Vec<u64> = symbols.iter().map(|(a, _)| *a).collect();
    let index_of = |at: u64| match starts.binary_search(&at) {
        Ok(i) => Some(i),
        Err(0) => None,
        Err(i) => Some(i - 1),
    };
    let mut by_function: Vec<Vec<&Insn>> = vec![Vec::new(); symbols.len()];
    for insn in insns {
        if let Some(i) = index_of(insn.at) {
            by_function[i].push(insn);
        }
    }
    symbols
        .iter()
        .enumerate()
        .map(|(i, (start, name))| {
            let body: Vec<Insn> = by_function[i]
                .iter()
                .map(|insn| Insn {
                    at: insn.at,
                    text: insn.text.clone(),
                })
                .collect();
            let mut calls = Vec::new();
            let mut tails = Vec::new();
            let mut indirect = false;
            for insn in &body {
                let (mnemonic, operands) = insn.text.split_once(' ').unwrap_or((&insn.text, ""));
                match (mnemonic, direct_target(operands)) {
                    ("call", Some(to)) => calls.extend(index_of(to).filter(|&j| j != i)),
                    ("call", None) => indirect = true,
                    ("jmp", Some(to)) if starts.binary_search(&to).is_ok() => {
                        tails.extend(index_of(to).filter(|&j| j != i));
                    }
                    _ => {}
                }
            }
            calls.sort_unstable();
            calls.dedup();
            tails.sort_unstable();
            tails.dedup();
            let measurement = is_measurement(name);
            Function {
                name: demangle(name),
                start: *start,
                frame: if measurement || is_probe(name) {
                    0
                } else {
                    8 + prologue(&body)
                },
                calls: if measurement { Vec::new() } else { calls },
                tails: if measurement { Vec::new() } else { tails },
                indirect: indirect && !measurement,
            }
        })
        .collect()
}

/// The longest chains from the function named `entry` ([`Chain`]). A call that would close a
/// cycle is left out and named.
fn chain(functions: &[Function], entry: &str) -> Option<Chain> {
    /// What lies below one function: the longest chain, and the deepest indirect call (none if no
    /// chain below it makes one), each with the next function on its chain.
    #[derive(Clone, Copy)]
    struct Below {
        longest: u64,
        longest_next: Option<usize>,
        indirect: Option<u64>,
        indirect_next: Option<usize>,
    }
    struct Walk<'a> {
        functions: &'a [Function],
        /// What lies below each function visited.
        done: HashMap<usize, Below>,
        on_path: HashSet<usize>,
        cycles: Vec<String>,
    }
    impl Walk<'_> {
        fn visit(&mut self, f: usize) -> Below {
            if let Some(&known) = self.done.get(&f) {
                return known;
            }
            self.on_path.insert(f);
            let function = &self.functions[f];
            let frame = function.frame;
            // Through a call, the callee's frame is below this one's; through a tail jump, it
            // takes this one's place.
            let mut below = Below {
                longest: frame,
                longest_next: None,
                indirect: function.indirect.then_some(frame),
                indirect_next: None,
            };
            let edges: Vec<(usize, bool)> = function
                .calls
                .iter()
                .map(|&c| (c, true))
                .chain(function.tails.iter().map(|&t| (t, false)))
                .collect();
            for (callee, is_call) in edges {
                if self.on_path.contains(&callee) {
                    self.cycles.push(format!(
                        "{} -> {}",
                        self.functions[f].name, self.functions[callee].name
                    ));
                    continue;
                }
                let callee_below = self.visit(callee);
                let base = if is_call { frame } else { 0 };
                if base + callee_below.longest > below.longest {
                    below.longest = base + callee_below.longest;
                    below.longest_next = Some(callee);
                }
                if let Some(d) = callee_below.indirect
                    && below.indirect.is_none_or(|known| base + d > known)
                {
                    below.indirect = Some(base + d);
                    below.indirect_next = Some(callee);
                }
            }
            self.on_path.remove(&f);
            self.done.insert(f, below);
            below
        }

        fn path(&self, from: usize, indirect: bool) -> Vec<(String, u64)> {
            let mut out = Vec::new();
            let mut at = Some(from);
            while let Some(f) = at {
                out.push((self.functions[f].name.clone(), self.functions[f].frame));
                let below = self.done[&f];
                at = if indirect {
                    below.indirect_next
                } else {
                    below.longest_next
                };
                if out.len() > self.functions.len() {
                    break;
                }
            }
            out
        }
    }
    let start = functions.iter().position(|f| f.name == entry)?;
    let mut walk = Walk {
        functions,
        done: HashMap::new(),
        on_path: HashSet::new(),
        cycles: Vec::new(),
    };
    let below = walk.visit(start);
    let mut cycles = walk.cycles.clone();
    cycles.sort();
    cycles.dedup();
    Some(Chain {
        entry_frame: functions[start].frame,
        longest: below.longest,
        path: walk.path(start, false),
        to_indirect: below.indirect.unwrap_or(0),
        indirect_path: walk.path(start, true),
        cycles,
    })
}

/// The static chain of the measurement loader at `efi`, whose link map is `map`.
fn static_chain(efi: &Path, map: &Path) -> Result<Chain> {
    let map_text = fs::read_to_string(map).map_err(|e| format!("{}: {e}", map.display()))?;
    let symbols = map_symbols(&map_text);
    let objdump = crate::llvm_tool("llvm-objdump")?;
    let out = Command::new(&objdump)
        .args(["-d", "--no-show-raw-insn", "--x86-asm-syntax=intel"])
        .arg(efi)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", objdump.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{} failed on {}: {}",
            objdump.display(),
            efi.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let insns = bench::instructions(&String::from_utf8_lossy(&out.stdout));
    let functions = functions(&symbols, &insns);
    chain(&functions, "efi_main").ok_or_else(|| {
        format!(
            "{}: no `efi_main` among its {} symbols: the link map was not read",
            map.display(),
            symbols.len()
        )
    })
}

// ------------------------------------------------------------------ the command

/// `cargo xtask stack-depth`: Clippy over the measurement code, both measurement loaders and
/// both kernels built, the static chains, then each scenario booted [`BOOTS`] times, and the
/// figures against [`PROMISED`].
pub(crate) fn run(settings: &Settings) -> Result {
    let started = Instant::now();
    clippy()?;
    let unsigned = crate::pack_kernel(&crate::build_kernel()?)?;
    let normal_kernel = read_file(&unsigned)?;
    let (trusted, _) = crate::test_keys()?;
    let signed_kernel = crate::sign_kernel(&unsigned, &trusted, "KERNEL.signed.ONBF")?;
    let (_, bench_kernel) = bench::build()?;
    let bench_kernel = read_file(&bench_kernel)?;
    let mut chains = Vec::new();
    for bench in [false, true] {
        let efi = build_loader(bench)?;
        chains.push((bench, static_chain(&efi, &map_path(bench))?));
    }
    BOOTS_SEEN
        .lock()
        .map_err(|_| "the boots' record was poisoned".to_string())?
        .clear();
    let mut rented = settings.clone();
    rented.cpu = RENTED_CPU.to_string();
    for (scenario, which) in scenarios() {
        let kernel = match which {
            Kernel::Unsigned => &normal_kernel,
            Kernel::Signed => &signed_kernel,
            Kernel::Bench => &bench_kernel,
        };
        let settings = if scenario.label == RENTAL {
            &rented
        } else {
            settings
        };
        for _ in 0..BOOTS {
            run_scenario(&scenario, kernel, settings)?;
        }
    }
    let boots = BOOTS_SEEN
        .lock()
        .map_err(|_| "the boots' record was poisoned".to_string())?
        .clone();
    for line in report(&boots, &chains) {
        println!("{line}");
    }
    println!(
        "xtask: stack-depth passed ({} boots, {} s)",
        boots.len(),
        started.elapsed().as_secs()
    );
    Ok(())
}

/// The figures, as lines: per scenario, each phase's range over its boots; per loader, its
/// static chain.
fn report(boots: &[Boot], chains: &[(bool, Chain)]) -> Vec<String> {
    let mut lines = vec![format!(
        "xtask: stack-depth: UEFI promises {PROMISED} bytes (UEFI 2.11 §2.3.4); depths are bytes below efi_main's RSP after its prologue"
    )];
    let mut labels: Vec<&str> = boots.iter().map(|b| b.label).collect();
    labels.dedup();
    for label in labels {
        let these: Vec<&Boot> = boots.iter().filter(|b| b.label == label).collect();
        let range = |of: &dyn Fn(&Boot) -> u64| {
            let lo = these.iter().map(|b| of(b)).min().unwrap_or(0);
            let hi = these.iter().map(|b| of(b)).max().unwrap_or(0);
            if lo == hi {
                format!("{lo}")
            } else {
                format!("{lo}-{hi}")
            }
        };
        let mut line = format!(
            "xtask: stack-depth: {label}, {} boots: calibration {} bytes below its array",
            these.len(),
            range(&|b: &Boot| b.calibration_below)
        );
        for p in 0..6 {
            let kinds: HashSet<&str> = these
                .iter()
                .map(|b| match b.phases[p] {
                    Depth::Above(_) => "<=",
                    Depth::Bytes(_) => "",
                    Depth::AtLeast(_) => ">=",
                })
                .collect();
            let mark = if kinds.len() == 1 {
                kinds.into_iter().next().unwrap_or("")
            } else {
                "~"
            };
            line.push_str(&format!(
                "; P{} {mark}{}",
                p + 1,
                range(&|b: &Boot| b.phases[p].bytes())
            ));
        }
        let deepest = these
            .iter()
            .map(|b| b.phases.iter().map(|d| d.bytes()).max().unwrap_or(0))
            .max()
            .unwrap_or(0);
        line.push_str(&format!(
            "; deepest {deepest} ({:.1}% of the promise)",
            deepest as f64 * 100.0 / PROMISED as f64
        ));
        lines.push(line);
    }
    for (bench, chain) in chains {
        let top: Vec<String> = chain
            .path
            .iter()
            .filter(|(_, frame)| *frame >= 512)
            .map(|(name, frame)| format!("{name} {frame}"))
            .collect();
        lines.push(format!(
            "xtask: stack-depth: static, the {} measurement loader: efi_main's own frame {} bytes (above the paint's top); the longest chain of the loader's own frames as their prologues reserve them, over direct calls and with the measurement's own functions left out, {} bytes in {} functions (those of 512 bytes or more: {}); the deepest of its indirect calls (into the firmware, or through a function pointer) {} bytes down, through {} functions; {}",
            if *bench { "bench" } else { "normal" },
            chain.entry_frame,
            chain.longest,
            chain.path.len(),
            if top.is_empty() { "none".to_string() } else { top.join(", ") },
            chain.to_indirect,
            chain.indirect_path.len(),
            if chain.cycles.is_empty() {
                "no recursion over direct calls".to_string()
            } else {
                format!("calls left out as recursion: {}", chain.cycles.join("; "))
            }
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insn(at: u64, text: &str) -> Insn {
        Insn {
            at,
            text: text.to_string(),
        }
    }

    #[test]
    fn the_depths_are_read_as_the_loader_writes_them() {
        assert_eq!(
            Depth::parse("no deeper than 712 bytes (the paint's end)"),
            Some(Depth::Above(712))
        );
        assert_eq!(
            Depth::parse("23456 bytes of the 131072 UEFI promises"),
            Some(Depth::Bytes(23456))
        );
        assert_eq!(
            Depth::parse(
                "131072 bytes or more: the bottom of the 131072 UEFI promises was written"
            ),
            Some(Depth::AtLeast(131_072))
        );
        assert_eq!(Depth::parse("something else"), None);
    }

    #[test]
    fn a_boot_is_read_from_both_uarts_and_refused_without_its_calibration() {
        let com1 = "origin-boot: stack measure: calibration: a 65536-byte array whose lowest word is 66000 bytes below the entry RSP; the scan found 66016 bytes of the 131072 UEFI promises: 16 bytes below the array, within the 512 allowed\r\n\
origin-boot: stack measure: P1 reading the kernel: 3000 bytes of the 131072 UEFI promises\n\
origin-boot: stack measure: P2 verifying it: 30000 bytes of the 131072 UEFI promises\n\
origin-boot: stack measure: P3 placing it and starting the generator: no deeper than 900 bytes (the paint's end)\n\
origin-boot: stack measure: P4 the allocations and the framebuffer: 4000 bytes of the 131072 UEFI promises\n\
origin-boot: stack measure: P5 the page tables and the table walks: 9000 bytes of the 131072 UEFI promises\n";
        let com2 = "X\norigin-boot: stack measure: P6 the last lines, the last map and the mark, to the call: 5000 bytes of the 131072 UEFI promises\norigin-boot: stack measure: deepest before the call: P2, 30000 bytes of the 131072 UEFI promises\n";
        let boot = read_boot(NORMAL, &[com1, com2]).expect("a boot");
        assert_eq!(boot.calibration_below, 16);
        assert_eq!(
            boot.phases,
            [
                Depth::Bytes(3000),
                Depth::Bytes(30000),
                Depth::Above(900),
                Depth::Bytes(4000),
                Depth::Bytes(9000),
                Depth::Bytes(5000)
            ]
        );
        // SD-4: a calibration that does not hold fails the boot.
        let wrong = com1.replace(
            ": 16 bytes below the array, within the 512 allowed",
            ": WRONG",
        );
        let err = read_boot(NORMAL, &[&wrong, com2]).unwrap_err();
        assert!(err.contains("positive control"), "{err}");
        // SD-1: no paint, no calibration line.
        let err = read_boot(NORMAL, &[&com1.replace("calibration", "x"), com2]).unwrap_err();
        assert!(err.contains("no calibration line"), "{err}");
        // A phase missing.
        let err = read_boot(
            NORMAL,
            &[
                com1,
                "origin-boot: stack measure: deepest before the call: P2",
            ],
        )
        .unwrap_err();
        assert!(err.contains("P6"), "{err}");
    }

    #[test]
    fn the_link_map_gives_publics_by_value() {
        let map = " origin-boot\n\n Preferred load address is 0000000140000000\n\n  Address         Publics by Value              Rva+Base               Lib:Object\n\n 0000:00000000       ___safe_se_handler_count   0000000000000000     <absolute>\n 0001:00000000       efi_main                   0000000140001000 f   origin_boot.o\n 0001:00000400       _ZN16origin_boot_uefi6kernel6verify17h0123456789abcdefE 0000000140001400 f   lib.o\n 0001:00000400       alias                      0000000140001400 f   lib.o\n";
        let symbols = map_symbols(map);
        assert_eq!(
            symbols,
            vec![
                (0, "___safe_se_handler_count".to_string()),
                (0x1_4000_1000, "efi_main".to_string()),
                (
                    0x1_4000_1400,
                    "_ZN16origin_boot_uefi6kernel6verify17h0123456789abcdefE".to_string()
                ),
            ]
        );
        assert_eq!(demangle(&symbols[2].1), "origin_boot_uefi::kernel::verify");
        assert_eq!(
            demangle("_ZN4core3fmt5write17h0123456789abcdefE"),
            "core::fmt::write"
        );
        assert_eq!(
            demangle(
                "_ZN50_$LT$T$u20$as$u20$core..convert..Into$LT$U$GT$$GT$4into17h0123456789abcdefE"
            ),
            "<T as core::convert::Into<U>>::into"
        );
        assert_eq!(demangle("efi_main"), "efi_main");
        // v0, as the pinned toolchain mangles (TASKS item 127's first run's map).
        assert_eq!(
            demangle("_RNvNtCs4M7Mb8By6Ar_13origin_crypto5mldsa14mldsa87_verify"),
            "origin_crypto::mldsa::mldsa87_verify"
        );
        assert_eq!(
            demangle("_RNvMs0_NtCs8DCFvQcYz5U_11onbf_format9signatureNtB5_14SignatureBlock5parse"),
            "onbf_format::signature::<…::SignatureBlock>::parse"
        );
        assert_eq!(
            demangle("_RNvCs5WqGp7zLRnA_7___rustc17___rust_probestack"),
            "__rustc::__rust_probestack"
        );
        // Generic arguments are not read: the name stays mangled.
        let generic = "_RINvNtCsf2a0OcjqUyK_16origin_boot_uefi4spcr4walkNCNvCs1crPuf7B2wz_11origin_boot4bootsd_0EBO_";
        assert_eq!(demangle(generic), generic);
        assert!(is_measurement(
            "_RNvNtCs1crPuf7B2wz_11origin_boot7measure9calibrate"
        ));
        assert!(is_measurement(
            "_RNvNtCsf2a0OcjqUyK_16origin_boot_uefi10stackdepth4scan"
        ));
        assert!(!is_measurement(
            "_RNvCs1crPuf7B2wz_11origin_boot16find_framebuffer"
        ));
        assert!(is_probe("_RNvCs5WqGp7zLRnA_7___rustc17___rust_probestack"));
    }

    #[test]
    fn a_prologue_counts_pushes_and_the_frame_probed_or_not() {
        let plain = [
            insn(0, "push rbp"),
            insn(1, "push rbx"),
            insn(2, "sub rsp, 0x28"),
            insn(3, "mov rbx, rcx"),
        ];
        assert_eq!(prologue(&plain), 16 + 0x28);
        let probed = [
            insn(0, "push rsi"),
            insn(1, "mov eax, 0x33b8"),
            insn(2, "call 0x140020000 <.text+0x1f000>"),
            insn(3, "sub rsp, rax"),
        ];
        assert_eq!(prologue(&probed), 8 + 0x33b8);
        assert_eq!(prologue(&[insn(0, "ret")]), 0);
        assert_eq!(prologue(&[insn(0, "sub rsp, 40")]), 40);
    }

    /// efi_main (frame 8+0x20) calls boot (8+0x1000), which calls verify (8+0x3000, probed) and,
    /// through a pointer, the firmware; verify calls a leaf (8) and jumps to a tail (8+0x10); the
    /// leaf calls boot again (recursion).
    fn program() -> Vec<Function> {
        let symbols = vec![
            (0x100, "efi_main".to_string()),
            (0x200, "boot".to_string()),
            (0x300, "verify".to_string()),
            (0x400, "leaf".to_string()),
            (0x500, "tail".to_string()),
            (0x600, "__chkstk".to_string()),
        ];
        let insns = [
            insn(0x100, "sub rsp, 0x20"),
            insn(0x104, "call 0x200 <.text>"),
            insn(0x200, "sub rsp, 0x1000"),
            insn(0x204, "call 0x300 <.text>"),
            insn(0x208, "call qword ptr [rax + 0x8]"),
            insn(0x300, "mov eax, 0x3000"),
            insn(0x302, "call 0x600 <.text>"),
            insn(0x304, "sub rsp, rax"),
            insn(0x306, "call 0x400 <.text>"),
            insn(0x308, "jmp 0x500 <.text>"),
            insn(0x400, "call 0x200 <.text>"),
            insn(0x402, "ret"),
            insn(0x500, "sub rsp, 0x10"),
            insn(0x504, "ret"),
            insn(0x600, "ret"),
        ];
        functions(&symbols, &insns)
    }

    #[test]
    fn the_longest_chain_follows_direct_calls_and_tail_jumps() {
        let f = program();
        assert_eq!(f[1].frame, 8 + 0x1000);
        assert!(f[1].indirect);
        let c = chain(&f, "efi_main").expect("a chain");
        assert_eq!(c.entry_frame, 8 + 0x20);
        // efi_main + boot + verify + the deeper of leaf (8) and tail in verify's place (8+0x10).
        assert_eq!(c.longest, (8 + 0x20) + (8 + 0x1000) + (8 + 0x3000) + 8);
        let names: Vec<&str> = c.path.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["efi_main", "boot", "verify", "leaf"]);
        // The firmware is called from boot.
        assert_eq!(c.to_indirect, (8 + 0x20) + (8 + 0x1000));
        assert_eq!(c.cycles, ["leaf -> boot"]);
        assert!(chain(&f, "nothing").is_none());
    }

    #[test]
    fn the_report_gives_ranges_and_the_share_of_the_promise() {
        let boot = |p2| Boot {
            label: NORMAL,
            calibration_below: 16,
            phases: [
                Depth::Bytes(3000),
                Depth::Bytes(p2),
                Depth::Above(900),
                Depth::Bytes(4000),
                Depth::Bytes(9000),
                Depth::Bytes(5000),
            ],
        };
        let c = chain(&program(), "efi_main").expect("a chain");
        let lines = report(&[boot(30000), boot(32768)], &[(false, c)]);
        assert!(
            lines[1].contains("2 boots: calibration 16 bytes"),
            "{}",
            lines[1]
        );
        assert!(
            lines[1].contains("P2 30000-32768; P3 <=900"),
            "{}",
            lines[1]
        );
        assert!(
            lines[1].ends_with("deepest 32768 (25.0% of the promise)"),
            "{}",
            lines[1]
        );
        assert!(
            lines[2].contains("calls left out as recursion: leaf -> boot"),
            "{}",
            lines[2]
        );
    }
}
