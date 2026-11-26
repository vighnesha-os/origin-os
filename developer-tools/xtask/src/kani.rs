//! `cargo xtask kani` and `cargo xtask kani-gate`: Kani's proofs (CLAUDE.md §73; owner item 358,
//! TASKS item 143) and their negative controls.
//!
//! The proofs are `#[kani::proof]` harnesses in the proof files listed in [`PROOF_FILES`], each a
//! child module that its parent names on one line, `#[cfg(kani)] mod proofs;`. Only `cargo kani`
//! sets `cfg(kani)`, in its own build under `target/kani`, with the compiler Kani's bundle
//! brings (owner item 358). Nothing it builds ships, and no build of the kernel, the loader or
//! any image holds any of it.
//!
//! - **`cargo xtask kani`** runs every harness and every control. It refuses any Kani but
//!   [`VERSION`] with the bundle's compiler [`BUNDLE_RUSTC`]. Each harness must report
//!   `VERIFICATION:- SUCCESSFUL` with every cover property satisfied, and at least one, so that
//!   no assumption made it vacuous. A compiler warning in a proof file fails the run. The full
//!   run is the CI job `kani`'s (the coordinating session's ruling of 2026-11-26): it is not part
//!   of `cargo xtask check`.
//! - **`cargo xtask kani --harness <name>`** runs one harness and the controls that name it: what
//!   writing a harness needs on the development machine, in a turn.
//! - **Each control** applies one deliberate bug to a scratch copy of the tree under the target
//!   directory (the working tree is never touched), and requires each harness it names to fail
//!   with one of the checks it names. A harness that cannot fail proves nothing. A control whose
//!   text no longer matches the source exactly once stops the run.
//! - **`cargo xtask kani-gate`**, part of `check`, needs no Kani: the harnesses the proof files
//!   define and the controls name agree, each harness has a control, each control's text is in
//!   its file exactly once, the word `kani` appears outside xtask and the proof files only in
//!   the parents' `#[cfg(kani)]` before `mod proofs;`, each proof file forbids `unsafe`, and the
//!   CI job installs this version.

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::{Result, collect_files, repo_root, target_dir};

/// The Kani release the proofs are checked with, read on 2026-11-26 (owner item 358).
pub(crate) const VERSION: &str = "0.68.0";
/// The commit of the compiler Kani's bundle brings (`nightly-2026-08-21`).
const BUNDLE_RUSTC: &str = "8925ea358";
/// The script the CI job installs Kani with; it pins [`VERSION`] and the bundles' digests.
const INSTALL_SCRIPT: &str = ".github/ci/install-kani.sh";
/// The workflow that runs `cargo xtask kani`.
const WORKFLOW: &str = ".github/workflows/ci.yml";

/// One file of harnesses.
struct ProofFile {
    /// The package it belongs to.
    package: &'static str,
    /// The file, from the repository's root.
    file: &'static str,
    /// Its module path inside the package, as Kani names harnesses.
    module: &'static str,
    /// The file that declares it, with `#[cfg(kani)] mod proofs;`.
    parent: &'static str,
}

/// Every file of harnesses.
const PROOF_FILES: &[ProofFile] = &[
    ProofFile {
        package: "origin-frame",
        file: "framekernel/frame/src/frames/proofs.rs",
        module: "frames::proofs",
        parent: "framekernel/frame/src/frames.rs",
    },
    ProofFile {
        package: "origin-frame",
        file: "framekernel/frame/src/heap/proofs.rs",
        module: "heap::proofs",
        parent: "framekernel/frame/src/heap.rs",
    },
];

/// The line a parent declares its proof module with, after `#[cfg(kani)]`.
const MOD_LINE: &str = "mod proofs;";

/// One deliberate bug, and the harnesses that must report it.
struct Control {
    name: &'static str,
    /// The file it changes, from the repository's root.
    file: &'static str,
    /// The text to replace; it must occur exactly once.
    from: &'static str,
    to: &'static str,
    /// Each harness that must fail, as Kani names it.
    harnesses: &'static [&'static str],
    /// The failed checks any of which a harness may report.
    expect: &'static [&'static str],
}

const FRAMES: &str = "framekernel/frame/src/frames.rs";
const HEAP: &str = "framekernel/frame/src/heap.rs";

/// The controls. Each harness of [`PROOF_FILES`] is named by at least one.
const CONTROLS: &[Control] = &[
    Control {
        name: "frame-zero-left-free",
        file: FRAMES,
        from: "        a.set_free(0, false);\n",
        to: "",
        harnesses: &["frames::proofs::new_frees_exactly_the_owned_frames"],
        expect: &[
            "ownership: every free frame is owned",
            "new frees exactly the owned frames",
        ],
    },
    Control {
        name: "reserved-from-its-first-frame-only",
        file: FRAMES,
        from: "    base / FRAME..end.div_ceil(FRAME)\n",
        to: "    base / FRAME..base / FRAME + pages\n",
        harnesses: &["frames::proofs::new_frees_exactly_the_owned_frames"],
        expect: &[
            "ownership: every free frame is owned",
            "new frees exactly the owned frames",
        ],
    },
    Control {
        name: "bitmap-size-not-checked",
        file: FRAMES,
        from: "        if provided < needed {\n            return Err(FrameError::BitmapTooSmall { needed, provided });\n        }\n",
        to: "",
        harnesses: &["frames::proofs::new_refuses_a_bitmap_too_small"],
        expect: &[
            "cover: the bitmap holds a bit for every frame of the map",
            "accepts only a bitmap that fits",
        ],
    },
    Control {
        name: "too-many-reserved-accepted",
        file: FRAMES,
        from: "        if reserved.len() > MAX_RESERVED {\n            return Err(FrameError::TooManyReserved);\n        }\n",
        to: "",
        harnesses: &["frames::proofs::new_refuses_more_than_eight_ranges"],
        expect: &["refuses more than 8 ranges, changing nothing"],
    },
    Control {
        name: "allocate-does-not-mark",
        file: FRAMES,
        from: "            self.set_free(frame, false);\n            self.next = frame + 1;\n",
        to: "            self.next = frame + 1;\n",
        harnesses: &[
            "frames::proofs::allocate_hands_out_a_free_frame",
            "frames::proofs::two_hand_outs_never_overlap",
        ],
        expect: &[
            "allocate marks that frame used and nothing else",
            "no frame is handed out twice",
        ],
    },
    Control {
        name: "taken-not-counted",
        file: FRAMES,
        from: "                self.free -= 1;\n",
        to: "",
        harnesses: &[
            "frames::proofs::allocate_hands_out_a_free_frame",
            "frames::proofs::allocate_hands_out_a_free_frame_one_word",
        ],
        expect: &[
            "allocate marks that frame used and nothing else",
            "accounting: free counts the free frames",
        ],
    },
    Control {
        name: "free-without-double-free-check",
        file: FRAMES,
        from: "        if self.is_free(frame) {\n            return Err(FrameError::DoubleFree(addr));\n        }\n",
        to: "",
        harnesses: &[
            "frames::proofs::free_takes_back_only_what_was_handed_out",
            "frames::proofs::free_takes_back_only_what_was_handed_out_one_word",
        ],
        expect: &["free takes back exactly an owned frame in use"],
    },
    Control {
        name: "free-without-ownership-check",
        file: FRAMES,
        from: "        if !self.allocatable(frame) {\n            return Err(FrameError::NotAllocatable(addr));\n        }\n",
        to: "",
        harnesses: &["frames::proofs::free_takes_back_only_what_was_handed_out"],
        expect: &[
            "free takes back exactly an owned frame in use",
            "ownership: every free frame is owned",
        ],
    },
    Control {
        name: "claim-checks-alignment-only",
        file: FRAMES,
        from: "        if !self.frame_is_free(addr) {\n",
        to: "        if !addr.is_multiple_of(FRAME) {\n",
        harnesses: &[
            "frames::proofs::claim_takes_only_a_free_frame",
            "frames::proofs::claim_takes_only_a_free_frame_one_word",
        ],
        expect: &["claim takes exactly an owned free frame"],
    },
    Control {
        name: "run-restarts-unaligned",
        file: FRAMES,
        from: "Some(used) => start = (used + 1).div_ceil(align) * align,",
        to: "Some(used) => start = used + 1,",
        harnesses: &["frames::proofs::allocate_run_hands_out_a_free_aligned_run"],
        expect: &["a run starts on a non-zero frame that is a multiple of its alignment"],
    },
    Control {
        name: "run-skips-its-first-candidate",
        file: FRAMES,
        from: "        let mut start = align;\n",
        to: "        let mut start = align + align;\n",
        harnesses: &["frames::proofs::allocate_run_hands_out_a_free_aligned_run"],
        expect: &["allocate_run fails only when no aligned run is free"],
    },
    Control {
        name: "free-run-without-ownership-check",
        file: FRAMES,
        from: "        if let Some(frame) = (first..end).find(|&f| !self.allocatable(f)) {\n            return Err(FrameError::NotAllocatable(frame * FRAME));\n        }\n",
        to: "",
        harnesses: &["frames::proofs::free_run_is_all_or_nothing"],
        expect: &[
            "every frame of a run taken back was owned and in use",
            "ownership: every free frame is owned",
        ],
    },
    Control {
        name: "free-run-frees-as-it-checks",
        file: FRAMES,
        from: "        if let Some(frame) = (first..end).find(|&f| self.is_free(f)) {\n            return Err(FrameError::DoubleFree(frame * FRAME));\n        }\n        for frame in first..end {\n            self.set_free(frame, true);\n        }\n",
        to: "        for frame in first..end {\n            if self.is_free(frame) {\n                return Err(FrameError::DoubleFree(frame * FRAME));\n            }\n            self.set_free(frame, true);\n        }\n",
        harnesses: &["frames::proofs::free_run_is_all_or_nothing"],
        expect: &["a refused free_run changes nothing"],
    },
    Control {
        name: "free-run-refuses-the-last-frame",
        file: FRAMES,
        from: "        if first == 0 || end > self.frames {\n",
        to: "        if first == 0 || end >= self.frames {\n",
        harnesses: &["frames::proofs::free_run_is_all_or_nothing"],
        expect: &["a refusal names a real fault of the run"],
    },
    Control {
        name: "covered-end-without-loader-memory",
        file: FRAMES,
        from: "            r.kind == RegionKind::Usable as u32 || r.kind == RegionKind::LoaderReclaimable as u32\n",
        to: "            r.kind == RegionKind::Usable as u32\n",
        harnesses: &["frames::proofs::new_frees_exactly_the_owned_frames"],
        expect: &["cover: the bitmap holds a bit for every frame of the map"],
    },
    Control {
        name: "rotation-stub-walks-no-step",
        file: "framekernel/frame/src/heap/proofs.rs",
        from: "    let mut i = table.len() - 1;\n    while i > 0 {\n",
        to: "    let mut i = table.len() - 1;\n    while i > 1 {\n",
        harnesses: &["heap::proofs::the_rotation_stubs_equal_core_s"],
        expect: &["the walks rotate as core's rotation does"],
    },
    Control {
        name: "plan-ignores-alignment",
        file: HEAP,
        from: "    let block = size.max(align).max(MIN_BLOCK).checked_next_power_of_two()?;\n",
        to: "    let block = size.max(MIN_BLOCK).checked_next_power_of_two()?;\n",
        harnesses: &["heap::proofs::plan_fits_every_layout"],
        expect: &[
            "a block holds the size and meets the alignment",
            "a run holds the size and meets the alignment",
            "plan refuses only what it cannot represent",
        ],
    },
    Control {
        name: "take-free-bit-ignores-its-limit",
        file: HEAP,
        from: "            if base + bit < limit {\n",
        to: "            if base + bit < 256 {\n",
        // Only `take_free_bit`'s own harness can see this one: `allocate` calls it only on a
        // slab with fewer blocks in use than it holds, and by the invariant that slab's lowest
        // clear bit lies below the limit, so the check is never what stops it there.
        harnesses: &["heap::proofs::take_free_bit_takes_the_lowest_free_bit"],
        expect: &["the lowest clear bit below the limit"],
    },
    Control {
        name: "free-bit-not-taken",
        file: HEAP,
        from: "                *word |= 1 << bit;\n",
        to: "",
        harnesses: &["heap::proofs::two_allocations_never_overlap"],
        expect: &["no byte is handed out twice"],
    },
    Control {
        name: "alias-not-refused",
        file: HEAP,
        from: "        let Err(at) = self.find(page) else {\n            // The source handed out a frame that is already a slab: refuse to alias it.\n            source.free_pages(page, 1);\n            return None;\n        };\n",
        to: "        let at = self.find(page).unwrap_or_else(|at| at);\n",
        harnesses: &["heap::proofs::allocate_keeps_the_heap_whole"],
        expect: &[
            "the table: sorted by frame, strictly",
            "a block handed out was free and is now live",
        ],
    },
    Control {
        name: "alias-frame-kept",
        file: HEAP,
        from: "            source.free_pages(page, 1);\n            return None;\n",
        to: "            return None;\n",
        harnesses: &["heap::proofs::allocate_keeps_the_heap_whole"],
        expect: &["a failed allocation gives back what it took"],
    },
    Control {
        name: "block-bytes-not-counted",
        file: HEAP,
        from: "        self.stats.bytes_in_use += block;\n",
        to: "",
        harnesses: &["heap::proofs::allocate_keeps_the_heap_whole"],
        expect: &["the totals: slabs and bytes in use are counted"],
    },
    Control {
        name: "slab-inserted-at-the-end",
        file: HEAP,
        from: "        let table = self.slabs.get_mut(at..=len)?;\n",
        to: "        let table = self.slabs.get_mut(len..=len)?;\n",
        harnesses: &["heap::proofs::allocate_keeps_the_heap_whole"],
        expect: &["the table: sorted by frame, strictly"],
    },
    Control {
        name: "heap-free-without-double-free-check",
        file: HEAP,
        from: "                if *w & (1 << bit) == 0 {\n                    return Err(HeapError::DoubleFree(addr));\n                }\n",
        to: "",
        harnesses: &["heap::proofs::free_takes_back_only_a_live_block"],
        expect: &[
            "free takes back exactly a live block of its size",
            "each slab: whole",
            "attempt to subtract with overflow",
        ],
    },
    Control {
        name: "heap-free-without-layout-check",
        file: HEAP,
        from: "                if u64::from(slab.block) != block {\n                    return Err(HeapError::WrongLayout(addr));\n                }\n",
        to: "",
        harnesses: &["heap::proofs::free_takes_back_only_a_live_block"],
        expect: &["free takes back exactly a live block of its size"],
    },
    Control {
        name: "empty-slab-kept",
        file: HEAP,
        from: "                if slab.used == 0 {\n",
        to: "                if slab.used == u32::MAX {\n",
        harnesses: &["heap::proofs::free_takes_back_only_a_live_block"],
        expect: &[
            "each slab: whole",
            "a slab's frame goes back once, exactly when it empties",
        ],
    },
    Control {
        name: "large-free-of-a-slab-frame",
        file: HEAP,
        from: "                if self.find(page).is_ok() || addr != page {\n",
        to: "                if addr != page {\n",
        harnesses: &["heap::proofs::free_takes_back_only_a_live_block"],
        expect: &["a large free names a frame that is no slab"],
    },
];

/// What one harness reported.
#[derive(Debug, Default, PartialEq, Eq)]
struct Outcome {
    /// `VERIFICATION:- SUCCESSFUL`.
    verified: bool,
    /// Each `Failed Checks:` description.
    failed: Vec<String>,
    /// Cover properties satisfied, of how many, when it has any.
    covers: Option<(u64, u64)>,
}

/// The harnesses of a proof file's source, as Kani names them: the function after each
/// `#[kani::proof]`, with the file's module path before it. The function may be public, and
/// it and other attributes may share the attribute's line.
fn harness_names(source: &str, module: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut pending = false;
    for line in source.lines() {
        let mut rest = line.trim();
        if let Some(after) = rest.strip_prefix("#[kani::proof]") {
            pending = true;
            rest = after;
        }
        if !pending {
            continue;
        }
        let rest = skip_attributes(rest);
        let rest = ["pub(crate) ", "pub(super) ", "pub "]
            .iter()
            .find_map(|v| rest.strip_prefix(v))
            .unwrap_or(rest);
        if let Some(signature) = rest.strip_prefix("fn ") {
            if let Some((name, _)) = signature.split_once('(') {
                names.push(format!("{module}::{}", name.trim()));
            }
            pending = false;
        }
    }
    names
}

/// `text` after any attributes it starts with, each `#[...]` with its brackets balanced.
fn skip_attributes(text: &str) -> &str {
    let mut rest = text.trim_start();
    while let Some(inner) = rest.strip_prefix("#[") {
        let mut depth = 1;
        let Some(end) = inner.char_indices().find_map(|(i, c)| {
            match c {
                '[' => depth += 1,
                ']' => depth -= 1,
                _ => {}
            }
            (depth == 0).then_some(i)
        }) else {
            return rest;
        };
        rest = inner[end + 1..].trim_start();
    }
    rest
}

/// Each harness Kani's output reports on, with what it reported, in order.
fn parse_report(output: &str) -> Vec<(String, Outcome)> {
    let mut report: Vec<(String, Outcome)> = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if let Some(name) = line
            .strip_prefix("Checking harness ")
            .and_then(|rest| rest.strip_suffix("..."))
        {
            report.push((name.to_string(), Outcome::default()));
        } else if let Some((_, outcome)) = report.last_mut() {
            if line == "VERIFICATION:- SUCCESSFUL" {
                outcome.verified = true;
            } else if let Some(check) = line.strip_prefix("Failed Checks: ") {
                outcome.failed.push(check.to_string());
            } else if let Some(rest) = line.strip_prefix("** ")
                && let Some((counts, _)) = rest.split_once(" cover properties satisfied")
                && let Some((done, of)) = counts.split_once(" of ")
                && let (Ok(done), Ok(of)) = (done.parse(), of.parse())
            {
                outcome.covers = Some((done, of));
            }
        }
    }
    report
}

/// The compiler warnings in `output` whose location is a proof file.
fn proof_warnings(output: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut last_warning: Option<&str> = None;
    for line in output.lines() {
        if line.starts_with("warning") {
            last_warning = Some(line);
        } else if let Some(warning) = last_warning
            && let Some(at) = line.trim_start().strip_prefix("--> ")
            && PROOF_FILES.iter().any(|p| at.starts_with(p.file))
        {
            found.push(format!("{at}: {warning}"));
            last_warning = None;
        }
    }
    found
}

/// What is wrong with a harness's outcome in a run that must prove it, if anything.
fn proof_problem(name: &str, outcome: &Outcome) -> Option<String> {
    if !outcome.verified {
        return Some(format!(
            "{name}: not verified; failed checks: {}",
            outcome.failed.join("; ")
        ));
    }
    match outcome.covers {
        Some((done, of)) if done == of && of > 0 => None,
        Some((done, of)) => Some(format!(
            "{name}: {done} of {of} cover properties satisfied: an assumption may have made the proof vacuous"
        )),
        None => Some(format!(
            "{name}: no cover property: each harness shows its outcomes are reachable"
        )),
    }
}

/// What is wrong with a harness's outcome under `control`, if anything.
fn control_problem(control: &Control, name: &str, outcome: Option<&Outcome>) -> Option<String> {
    let Some(outcome) = outcome else {
        return Some(format!(
            "control {}: {name} did not run (does the control compile?)",
            control.name
        ));
    };
    if outcome.verified {
        return Some(format!(
            "control {}: {name} still verified: it cannot see this bug",
            control.name
        ));
    }
    if outcome
        .failed
        .iter()
        .any(|check| control.expect.contains(&check.as_str()))
    {
        None
    } else {
        Some(format!(
            "control {}: {name} failed, but not with one of {:?}: {:?}",
            control.name, control.expect, outcome.failed
        ))
    }
}

/// `cargo kani` for `package`, run in `dir`, with `harnesses` (all if empty). Its output is
/// written to `log` and returned; the run's own exit status says nothing here, since a control
/// must fail.
fn cargo_kani(
    dir: &Path,
    target: Option<&Path>,
    package: &str,
    harnesses: &[&str],
    log: &Path,
) -> Result<String> {
    // Each run's peak memory is measured by the system's `time` (`-v` on Linux, where CI runs,
    // `-l` on macOS), which writes it to the run's errors; [`peak_memory`] reads it there. Without
    // one, the run goes ahead and its line says the memory was not measured.
    let mut command = match time_option(std::env::consts::OS) {
        Some(option) if Path::new(TIME).is_file() => {
            let mut timed = Command::new(TIME);
            timed.args([option, "cargo"]);
            timed
        }
        _ => Command::new("cargo"),
    };
    // `-Z stubbing`: the heap's harnesses stub core's slice rotation (`heap/proofs.rs`). Kani
    // 0.68.0 calls the feature unstable; the version is pinned.
    command
        .current_dir(dir)
        .args(["kani", "-Z", "stubbing", "-p", package]);
    for harness in harnesses {
        command.args(["--harness", harness, "--exact"]);
    }
    if let Some(target) = target {
        command.env("CARGO_TARGET_DIR", target);
    }
    // Kani brings its own compiler: nothing of the pinned toolchain's run of `xtask` reaches it.
    for name in [
        "RUSTUP_TOOLCHAIN",
        "RUSTC",
        "RUSTDOC",
        "CARGO",
        "RUSTC_WRAPPER",
    ] {
        command.env_remove(name);
    }
    let shown = format!("cargo kani -p {package} {}", harnesses.join(" "));
    println!("xtask: kani: $ {shown}");
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run `{shown}`: {e}"))?;
    let stderr = child.stderr.take().ok_or("cannot read Kani's errors")?;
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        for line in BufReader::new(stderr)
            .lines()
            .map_while(std::io::Result::ok)
        {
            text.push_str(&line);
            text.push('\n');
        }
        text
    });
    let stdout = child.stdout.take().ok_or("cannot read Kani's output")?;
    let mut output = String::new();
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|e| format!("cannot read Kani's output: {e}"))?;
        let trimmed = line.trim();
        if trimmed.starts_with("Checking harness")
            || trimmed.starts_with("VERIFICATION:-")
            || trimmed.starts_with("Failed Checks:")
            || trimmed.starts_with("** ")
            || trimmed.starts_with("Verification Time")
        {
            println!("xtask: kani:   {trimmed}");
        }
        output.push_str(&line);
        output.push('\n');
    }
    child
        .wait()
        .map_err(|e| format!("cannot run `{shown}`: {e}"))?;
    output.push_str(&errors.join().unwrap_or_default());
    if let Some(dir) = log.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::write(log, &output).map_err(|e| format!("{}: {e}", log.display()))?;
    Ok(output)
}

/// The system's `time`, which measures a run's peak memory.
const TIME: &str = "/usr/bin/time";

/// `time`'s option that reports the peak resident memory on `os` (`std::env::consts::OS`).
fn time_option(os: &str) -> Option<&'static str> {
    match os {
        "linux" => Some("-v"),
        "macos" => Some("-l"),
        _ => None,
    }
}

/// The peak resident memory `time` reported in `output`, in bytes: GNU `time -v`'s
/// "Maximum resident set size (kbytes): N", or BSD `time -l`'s "N  maximum resident set size".
fn peak_memory(output: &str) -> Option<u64> {
    output.lines().find_map(|line| {
        let line = line.trim();
        if let Some(kbytes) = line.strip_prefix("Maximum resident set size (kbytes):") {
            return kbytes.trim().parse::<u64>().ok()?.checked_mul(1024);
        }
        line.strip_suffix("maximum resident set size")?
            .trim()
            .parse::<u64>()
            .ok()
    })
}

/// `output`'s peak memory, as a run's line shows it.
fn memory_shown(output: &str) -> String {
    match peak_memory(output) {
        // GB of 10^9 bytes, as the records give it.
        Some(bytes) => format!("peak memory {:.2} GB", bytes as f64 / 1e9),
        None => "peak memory not measured".into(),
    }
}

/// Refuses any Kani but [`VERSION`] with its bundle's compiler.
fn check_version() -> Result {
    let output = Command::new("cargo")
        .args(["kani", "--version", "--verbose"])
        .env_remove("RUSTUP_TOOLCHAIN")
        .output()
        .map_err(|e| {
            format!("cannot run `cargo kani --version`: {e} (install it: {INSTALL_SCRIPT})")
        })?;
    let text = String::from_utf8_lossy(&output.stdout);
    version_problem(&text).map_or(Ok(()), Err)
}

/// What is wrong with `cargo kani --version --verbose`'s output, if anything.
fn version_problem(text: &str) -> Option<String> {
    let wanted = format!("Kani Rust Verifier {VERSION} ");
    let first = text.lines().next().unwrap_or("");
    if !first.starts_with(&wanted) {
        return Some(format!(
            "Kani is `{first}`, not {VERSION} (owner item 358; install it with {INSTALL_SCRIPT})"
        ));
    }
    if !text.contains(BUNDLE_RUSTC) {
        return Some(format!(
            "Kani {VERSION}'s compiler is not its bundle's ({BUNDLE_RUSTC}): {text}"
        ));
    }
    None
}

/// The harnesses every proof file defines, by package, from the tree at `root`.
fn defined(root: &Path) -> Result<Vec<(&'static str, String)>> {
    let mut all = Vec::new();
    for proof in PROOF_FILES {
        let path = root.join(proof.file);
        let source = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        for name in harness_names(&source, proof.module) {
            all.push((proof.package, name));
        }
    }
    Ok(all)
}

/// Copies every file Git tracks or would track (not ignored) from `root` into `scratch`.
fn populate(root: &Path, scratch: &Path) -> Result {
    if scratch.exists() {
        fs::remove_dir_all(scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    }
    let listed = Command::new("git")
        .current_dir(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
        .map_err(|e| format!("cannot run git ls-files: {e}"))?;
    if !listed.status.success() {
        return Err("git ls-files failed".into());
    }
    for name in listed.stdout.split(|&b| b == 0).filter(|n| !n.is_empty()) {
        let relative =
            Path::new(std::str::from_utf8(name).map_err(|e| format!("a file name: {e}"))?);
        let from = root.join(relative);
        if !from.is_file() {
            continue; // deleted in the working tree
        }
        let to = scratch.join(relative);
        if let Some(dir) = to.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        fs::copy(&from, &to).map_err(|e| format!("{}: {e}", from.display()))?;
    }
    Ok(())
}

/// Applies `control` to the tree at `dir`; its text must occur exactly once.
fn apply(control: &Control, dir: &Path) -> Result {
    let path = dir.join(control.file);
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let found = text.matches(control.from).count();
    if found != 1 {
        return Err(format!(
            "control {}: expected its text exactly once in {}, found it {found} times; update the control to match the source",
            control.name, control.file
        ));
    }
    fs::write(&path, text.replace(control.from, control.to))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// `cargo xtask kani [--harness <name>]`: the proofs, then their controls.
pub(crate) fn run(only: Option<&str>) -> Result {
    gate()?;
    check_version()?;
    let root = repo_root();
    let logs = target_dir().join("xtask/kani");
    let all = defined(&root)?;
    if let Some(name) = only
        && !all.iter().any(|(_, n)| n == name)
    {
        return Err(format!("kani: no harness {name}"));
    }
    let chosen: Vec<&(&str, String)> = all
        .iter()
        .filter(|(_, n)| only.is_none_or(|o| o == n))
        .collect();
    let packages: BTreeSet<&str> = chosen.iter().map(|(p, _)| *p).collect();
    let mut problems = Vec::new();
    // One `cargo kani` for each harness, so that each run's peak memory is that harness's (CI
    // records it; the coordinating session's ruling of 2026-11-27 on Q4's 8 GB).
    let mut runs = 0;
    for package in packages {
        let names: Vec<&str> = chosen
            .iter()
            .filter(|(p, _)| *p == package)
            .map(|(_, n)| n.as_str())
            .collect();
        for name in &names {
            let output = cargo_kani(
                &root,
                None,
                package,
                &[name],
                &logs.join(format!("{name}.log")),
            )?;
            runs += 1;
            problems.extend(proof_warnings(&output));
            let report = parse_report(&output);
            match report.iter().find(|(n, _)| n == name) {
                Some((_, outcome)) => {
                    problems.extend(proof_problem(name, outcome));
                    println!("xtask: kani: {name}: {}", memory_shown(&output));
                }
                None => problems.push(format!("{name}: Kani did not report it")),
            }
            for (reported, _) in &report {
                if reported != name {
                    problems.push(format!(
                        "{reported}: reported in {name}'s run, which asked for {name} alone"
                    ));
                }
            }
        }
    }
    if runs != chosen.len() {
        problems.push(format!(
            "{runs} runs for {} harnesses: each harness runs once",
            chosen.len()
        ));
    }
    if !problems.is_empty() {
        return Err(format!(
            "kani: the proofs failed:\n  {}",
            problems.join("\n  ")
        ));
    }
    println!(
        "xtask: kani: {} harnesses verified, every cover property satisfied",
        chosen.len()
    );
    controls(&root, only)
}

/// Runs each control that names a harness chosen (every control, or those naming `only`).
fn controls(root: &Path, only: Option<&str>) -> Result {
    let scratch = target_dir().join("kani-scratch/tree");
    let target = target_dir().join("kani-scratch/target");
    let logs = target_dir().join("xtask/kani/controls");
    let mut ran = 0;
    for control in CONTROLS {
        let harnesses: Vec<&str> = control
            .harnesses
            .iter()
            .copied()
            .filter(|h| only.is_none_or(|o| o == *h))
            .collect();
        if harnesses.is_empty() {
            continue;
        }
        populate(root, &scratch)?;
        apply(control, &scratch)?;
        let package = package_of(control.file)?;
        let output = cargo_kani(
            &scratch,
            Some(&target),
            package,
            &harnesses,
            &logs.join(format!("{}.log", control.name)),
        )?;
        let report = parse_report(&output);
        let mut problems = Vec::new();
        for name in &harnesses {
            let outcome = report.iter().find(|(n, _)| n == name).map(|(_, o)| o);
            problems.extend(control_problem(control, name, outcome));
        }
        if !problems.is_empty() {
            return Err(format!(
                "kani: a control was not caught:\n  {}",
                problems.join("\n  ")
            ));
        }
        println!(
            "xtask: kani: control {} caught by {}; {}",
            control.name,
            harnesses.join(", "),
            memory_shown(&output)
        );
        ran += 1;
    }
    println!("xtask: kani: {ran} controls, each caught");
    Ok(())
}

/// The package a control's file belongs to.
fn package_of(file: &str) -> Result<&'static str> {
    PROOF_FILES
        .iter()
        .find(|p| p.parent == file || p.file == file)
        .map(|p| p.package)
        .ok_or_else(|| format!("kani: no proof file's package holds {file}"))
}

/// `cargo xtask kani-gate`: what can be checked without Kani.
pub(crate) fn gate() -> Result {
    let root = repo_root();
    let mut problems = Vec::new();
    let defined = defined(&root)?;
    let names: Vec<&str> = defined.iter().map(|(_, n)| n.as_str()).collect();
    let mut sources = Vec::new();
    for proof in PROOF_FILES {
        let path = root.join(proof.file);
        let source = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let parent_path = root.join(proof.parent);
        let parent = fs::read_to_string(&parent_path)
            .map_err(|e| format!("{}: {e}", parent_path.display()))?;
        sources.push((proof, source, parent));
    }
    problems.extend(file_problems(
        &sources
            .iter()
            .map(|(p, s, parent)| (*p, s.as_str(), parent.as_str()))
            .collect::<Vec<_>>(),
    ));
    problems.extend(control_list_problems(&names, CONTROLS, &|file| {
        fs::read_to_string(root.join(file)).ok()
    }));
    // The word `kani` only where the proof modules are declared, outside xtask and the proof
    // files themselves.
    let mut files = Vec::new();
    collect_files(
        &root,
        &|dir| dir.ends_with("developer-tools/xtask"),
        &|name| name.ends_with(".rs"),
        &mut files,
    )
    .map_err(|e| format!("{}: {e}", root.display()))?;
    for file in files {
        let relative = file.strip_prefix(&root).unwrap_or(&file);
        if PROOF_FILES.iter().any(|p| Path::new(p.file) == relative) {
            continue;
        }
        let text = fs::read_to_string(&file).unwrap_or_default();
        let allowed = PROOF_FILES.iter().any(|p| Path::new(p.parent) == relative);
        problems.extend(cfg_problems(
            &relative.display().to_string(),
            &text,
            allowed,
        ));
    }
    // The CI job installs this version and runs the proofs.
    let script = fs::read_to_string(root.join(INSTALL_SCRIPT)).unwrap_or_default();
    if !script.contains(&format!("KANI_VERSION={VERSION}\n")) {
        problems.push(format!(
            "{INSTALL_SCRIPT} does not pin KANI_VERSION={VERSION}"
        ));
    }
    let workflow = fs::read_to_string(root.join(WORKFLOW)).unwrap_or_default();
    if !workflow.contains("run: cargo --locked xtask kani\n") || !workflow.contains(INSTALL_SCRIPT)
    {
        problems.push(format!("{WORKFLOW} has no job that installs Kani with {INSTALL_SCRIPT} and runs `cargo --locked xtask kani`"));
    }
    if problems.is_empty() {
        println!(
            "xtask: kani-gate: {} harnesses in {} proof files, {} controls, each harness controlled; `kani` only on the proof modules' declarations",
            names.len(),
            PROOF_FILES.len(),
            CONTROLS.len()
        );
        Ok(())
    } else {
        Err(format!("kani-gate:\n  {}", problems.join("\n  ")))
    }
}

/// Problems with the proof files and their parents' declarations.
fn file_problems(files: &[(&ProofFile, &str, &str)]) -> Vec<String> {
    let mut problems = Vec::new();
    for (proof, source, parent) in files {
        if !source.starts_with("//!") || !source.lines().any(|l| l == "#![forbid(unsafe_code)]") {
            problems.push(format!(
                "{}: a proof file starts with its documentation and `#![forbid(unsafe_code)]`",
                proof.file
            ));
        }
        let lines: Vec<&str> = parent.lines().collect();
        let declared = lines
            .windows(2)
            .filter(|w| w[0].trim() == "#[cfg(kani)]" && w[1].trim() == MOD_LINE)
            .count();
        if declared != 1 {
            problems.push(format!(
                "{}: expected `#[cfg(kani)]` then `{MOD_LINE}` exactly once, found {declared}",
                proof.parent
            ));
        }
    }
    problems
}

/// Problems with the controls against the harnesses `names`, reading each control's file with
/// `read`.
fn control_list_problems(
    names: &[&str],
    controls: &[Control],
    read: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    for name in names {
        if !seen.insert(*name) {
            problems.push(format!("{name}: two harnesses of one name"));
        }
    }
    let mut control_names = BTreeSet::new();
    for control in controls {
        if !control_names.insert(control.name) {
            problems.push(format!("control {}: named twice", control.name));
        }
        for harness in control.harnesses {
            if !names.contains(harness) {
                problems.push(format!("control {}: no harness {harness}", control.name));
            }
        }
        if control.expect.is_empty() || control.harnesses.is_empty() {
            problems.push(format!(
                "control {}: names no harness or no check",
                control.name
            ));
        }
        match read(control.file) {
            None => problems.push(format!(
                "control {}: cannot read {}",
                control.name, control.file
            )),
            Some(text) => {
                let found = text.matches(control.from).count();
                if found != 1 {
                    problems.push(format!(
                        "control {}: its text is in {} {found} times, not once",
                        control.name, control.file
                    ));
                }
            }
        }
    }
    for name in names {
        if !controls.iter().any(|c| c.harnesses.contains(name)) {
            problems.push(format!("{name}: no control can break it"));
        }
    }
    problems
}

/// Problems with the word `kani` in `text`, the file `name`: outside a comment it may appear
/// only in `#[cfg(kani)]` on the line before `mod proofs;`, and only where `allowed`. Any other
/// use, `cfg(any(kani, ..))`, `cfg_attr(kani, ..)` or `cfg!(kani)` among them, could make Kani
/// prove code that differs from the code that ships.
fn cfg_problems(name: &str, text: &str, allowed: bool) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut problems = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let code = line.split("//").next().unwrap_or("");
        if !names_kani(code) {
            continue;
        }
        let declares = code.trim() == "#[cfg(kani)]"
            && lines.get(i + 1).is_some_and(|next| next.trim() == MOD_LINE);
        if !(allowed && declares) {
            problems.push(format!(
                "{name}:{}: `kani` outside a proof module's declaration",
                i + 1
            ));
        }
    }
    problems
}

/// Whether `code` holds `kani` as a word: with no letter, digit or `_` either side.
fn names_kani(code: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    code.match_indices("kani").any(|(at, found)| {
        !code[..at].chars().next_back().is_some_and(word)
            && !code[at + found.len()..].chars().next().is_some_and(word)
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn peak_memory_reads_gnu_and_bsd_time() {
        let gnu = "\tCommand being timed: \"cargo kani\"\n\tMaximum resident set size (kbytes): 6281920\n";
        assert_eq!(super::peak_memory(gnu), Some(6_281_920 * 1024));
        let bsd = "        97.07 real        95.12 user         1.90 sys\n          6431686656  maximum resident set size\n";
        assert_eq!(super::peak_memory(bsd), Some(6_431_686_656));
        assert_eq!(super::peak_memory("VERIFICATION:- SUCCESSFUL\n"), None);
        assert_eq!(super::memory_shown(bsd), "peak memory 6.43 GB");
        assert_eq!(super::memory_shown(""), "peak memory not measured");
        assert_eq!(super::time_option("linux"), Some("-v"));
        assert_eq!(super::time_option("macos"), Some("-l"));
        assert_eq!(super::time_option("windows"), None);
    }

    use super::*;

    const SAMPLE: &str = "\
Checking harness frames::proofs::good...
RESULTS:
SUMMARY:
 ** 0 of 20 failed (1 unreachable)
 ** 2 of 2 cover properties satisfied

VERIFICATION:- SUCCESSFUL
Verification Time: 1.5s

Checking harness frames::proofs::bad...
SUMMARY:
 ** 1 of 20 failed
 ** 1 of 2 cover properties satisfied (1 unreachable)
Failed Checks: accounting: free counts the free frames
 File: \"framekernel/frame/src/frames/proofs.rs\", line 1, in x
Failed Checks: attempt to subtract with overflow
VERIFICATION:- FAILED
";

    #[test]
    fn harness_names_follow_each_proof_attribute() {
        let source = "\
#[kani::proof]
#[kani::unwind(130)]
fn one() {}

fn helper() {}

#[kani::proof]
fn two_words() {
    let x = 1;
}

#[kani::proof]
pub fn public() {}

#[kani::proof] #[kani::stub(<[T]>::rotate_right, walk)] pub(crate) fn one_line() {}
";
        assert_eq!(
            harness_names(source, "frames::proofs"),
            [
                "frames::proofs::one",
                "frames::proofs::two_words",
                "frames::proofs::public",
                "frames::proofs::one_line"
            ]
        );
    }

    #[test]
    fn the_report_says_what_each_harness_did() {
        let report = parse_report(SAMPLE);
        assert_eq!(report.len(), 2);
        assert_eq!(report[0].0, "frames::proofs::good");
        assert_eq!(
            report[0].1,
            Outcome {
                verified: true,
                failed: vec![],
                covers: Some((2, 2))
            }
        );
        assert_eq!(report[1].0, "frames::proofs::bad");
        assert!(!report[1].1.verified);
        assert_eq!(
            report[1].1.failed,
            [
                "accounting: free counts the free frames",
                "attempt to subtract with overflow"
            ]
        );
        assert_eq!(report[1].1.covers, Some((1, 2)));
    }

    #[test]
    fn a_proof_needs_its_verdict_and_every_cover() {
        let report = parse_report(SAMPLE);
        assert_eq!(proof_problem(&report[0].0, &report[0].1), None);
        assert!(
            proof_problem(&report[1].0, &report[1].1).is_some_and(|p| p.contains("not verified"))
        );
        let vacuous = Outcome {
            verified: true,
            failed: vec![],
            covers: Some((1, 2)),
        };
        assert!(proof_problem("x", &vacuous).is_some_and(|p| p.contains("vacuous")));
        let uncovered = Outcome {
            verified: true,
            failed: vec![],
            covers: None,
        };
        assert!(proof_problem("x", &uncovered).is_some_and(|p| p.contains("no cover")));
        let none = Outcome {
            verified: true,
            failed: vec![],
            covers: Some((0, 0)),
        };
        assert!(proof_problem("x", &none).is_some());
    }

    fn control(expect: &'static [&'static str]) -> Control {
        Control {
            name: "c",
            file: FRAMES,
            from: "a",
            to: "b",
            harnesses: &["h"],
            expect,
        }
    }

    #[test]
    fn a_control_must_fail_its_harness_with_a_named_check() {
        let report = parse_report(SAMPLE);
        let caught = control(&["attempt to subtract with overflow"]);
        assert_eq!(control_problem(&caught, "bad", Some(&report[1].1)), None);
        let other = control(&["ownership: every free frame is owned"]);
        assert!(
            control_problem(&other, "bad", Some(&report[1].1))
                .is_some_and(|p| p.contains("not with one of"))
        );
        assert!(
            control_problem(&caught, "good", Some(&report[0].1))
                .is_some_and(|p| p.contains("still verified"))
        );
        assert!(control_problem(&caught, "gone", None).is_some_and(|p| p.contains("did not run")));
    }

    #[test]
    fn the_version_and_the_bundle_s_compiler_are_pinned() {
        let good = format!(
            "Kani Rust Verifier {VERSION} (kani-{VERSION}) (cargo plugin)\nusing rustc 1.100.0-nightly ({BUNDLE_RUSTC} 2026-08-20)\nCBMC 6.11.0\n"
        );
        assert_eq!(version_problem(&good), None);
        assert!(version_problem(&good.replace(VERSION, "0.67.0")).is_some());
        assert!(version_problem(&good.replace(BUNDLE_RUSTC, "0000000")).is_some());
        assert!(version_problem("").is_some());
    }

    #[test]
    fn warnings_count_only_in_proof_files() {
        let output = "\
warning: unused variable: `x`
  --> framekernel/frame/src/frames/proofs.rs:10:9
warning: trailing semicolon in macro used in expression position
  --> framekernel/frame/sync/src/shootdown.rs:67:15
";
        let found = proof_warnings(output);
        assert_eq!(found.len(), 1);
        assert!(found[0].starts_with("framekernel/frame/src/frames/proofs.rs:10:9"));
    }

    #[test]
    fn cfg_kani_belongs_only_on_a_declaration() {
        let declared = "/// Proofs.\n#[cfg(kani)]\nmod proofs;\n";
        assert!(cfg_problems("p.rs", declared, true).is_empty());
        assert_eq!(
            cfg_problems("p.rs", declared, false).len(),
            1,
            "not a listed parent"
        );
        assert_eq!(
            cfg_problems("p.rs", "#[cfg(kani)]\nfn sneaky() {}\n", true).len(),
            1
        );
        assert_eq!(
            cfg_problems("p.rs", "#[cfg(not(kani))]\nfn other() {}\n", true).len(),
            1
        );
        assert!(cfg_problems("p.rs", "// cfg(kani) in a comment\n", false).is_empty());
        // Every other way of naming it, each of which could change what Kani proves.
        for form in [
            "#[cfg(any(kani, test))]\npub const MAX_RESERVED: usize = 2;\n",
            "#[cfg(not(any(kani)))]\nfn other() {}\n",
            "#[cfg(all(kani, test))]\nfn other() {}\n",
            "#[cfg_attr(kani, inline(never))]\nfn other() {}\n",
            "let fast = cfg!(kani);\n",
            "#[cfg( kani )]\nmod proofs;\n",
            "kani::assume(true);\n",
        ] {
            assert_eq!(cfg_problems("p.rs", form, true).len(), 1, "{form}");
        }
        // A longer identifier is not the word.
        assert!(cfg_problems("p.rs", "let kani_like = 1; let pekani = 2;\n", false).is_empty());
    }

    #[test]
    fn controls_must_match_once_and_cover_every_harness() {
        let read = |_: &str| Some("one a here".to_string());
        let controls = [control(&["x"])];
        assert_eq!(
            control_list_problems(&["h"], &controls, &read),
            Vec::<String>::new()
        );
        let twice = |_: &str| Some("a a".to_string());
        assert!(control_list_problems(&["h"], &controls, &twice)[0].contains("2 times"));
        let problems = control_list_problems(&["h", "lonely"], &controls, &read);
        assert!(problems.iter().any(|p| p.contains("lonely: no control")));
        let problems = control_list_problems(&[], &controls, &read);
        assert!(problems.iter().any(|p| p.contains("no harness h")));
    }

    #[test]
    fn a_parent_declares_its_proofs_once() {
        let proof = &PROOF_FILES[0];
        let good = "//! Proofs.\n#![forbid(unsafe_code)]\n";
        assert!(file_problems(&[(proof, good, "#[cfg(kani)]\nmod proofs;\n")]).is_empty());
        assert_eq!(file_problems(&[(proof, good, "mod proofs;\n")]).len(), 1);
        assert_eq!(
            file_problems(&[(proof, "fn x() {}\n", "#[cfg(kani)]\nmod proofs;\n")]).len(),
            1
        );
    }

    #[test]
    fn the_usage_names_the_pinned_version() {
        assert!(crate::USAGE.contains(&format!("Kani {VERSION} (")));
    }

    /// The controls in the tree: each text found once, each harness controlled.
    #[test]
    fn the_tree_s_controls_apply() {
        let root = repo_root();
        let defined = defined(&root).unwrap();
        let names: Vec<&str> = defined.iter().map(|(_, n)| n.as_str()).collect();
        let read = |file: &str| fs::read_to_string(root.join(file)).ok();
        assert_eq!(
            control_list_problems(&names, CONTROLS, &read),
            Vec::<String>::new()
        );
    }
}
