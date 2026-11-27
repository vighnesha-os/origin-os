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
//! - **`cargo xtask kani --package <name>`** runs one package's harnesses and each control on
//!   them: CI's jobs, one for each package of [`PROOF_FILES`] (the coordinating session's ruling
//!   of 2026-11-27, after the one job took 122 minutes against a rule of 120). Together they run
//!   every harness and every control once, each control on all its harnesses, as the whole run
//!   does: a control's harnesses are all in its file's package, which the gate checks.
//! - **`cargo xtask kani --harness <name>`** runs one harness and the controls that name it: what
//!   writing a harness needs on the development machine, in a turn.
//! - **`--list`**, after nothing or `--package <name>`, prints what that run would run and the
//!   counts, and runs nothing.
//! - **Each control** applies one deliberate bug to a scratch copy of the tree under the target
//!   directory (the working tree is never touched), and requires each harness it names to fail
//!   with one of the checks it names. A harness that cannot fail proves nothing. A control whose
//!   text no longer matches the source exactly once stops the run.
//! - **`cargo xtask kani-gate`**, part of `check`, needs no Kani: the harnesses the proof files
//!   define and the controls name agree, each harness has a control, each control's text is in
//!   its file exactly once, the word `kani` appears outside xtask and the proof files only in
//!   the parents' `#[cfg(kani)]` before `mod proofs;`, each proof file forbids `unsafe`, the CI
//!   job installs this version, its matrix lists exactly the packages of [`PROOF_FILES`] and one
//!   step runs one package for each job, and each control's harnesses are in its own package.

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
    // TASKS item 143, step 3: one entry per parent module (the coordinating session's ruling of
    // 2026-11-26), since `check_padding` is private to `image.rs`.
    ProofFile {
        package: "onbf-format",
        file: "runtime/onbf/format/src/proofs.rs",
        module: "proofs",
        parent: "runtime/onbf/format/src/lib.rs",
    },
    ProofFile {
        package: "onbf-format",
        file: "runtime/onbf/format/src/image/proofs.rs",
        module: "image::proofs",
        parent: "runtime/onbf/format/src/image.rs",
    },
    // TASKS item 143, step 4: capability validation, the kernel's two host-built checks.
    ProofFile {
        package: "framekernel",
        file: "framekernel/src/handles/proofs.rs",
        module: "handles::proofs",
        parent: "framekernel/src/handles.rs",
    },
    ProofFile {
        package: "framekernel",
        file: "framekernel/src/mapping/proofs.rs",
        module: "mapping::proofs",
        parent: "framekernel/src/mapping.rs",
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
const HEADER: &str = "runtime/onbf/format/src/header.rs";
const SECTION: &str = "runtime/onbf/format/src/section.rs";
const READ: &str = "runtime/onbf/format/src/read.rs";
const OCBOR: &str = "runtime/onbf/format/src/ocbor.rs";
const SIGNATURE: &str = "runtime/onbf/format/src/signature.rs";
const IMAGE: &str = "runtime/onbf/format/src/image.rs";
const ONBF_PROOFS: &str = "runtime/onbf/format/src/proofs.rs";
const HANDLES: &str = "framekernel/src/handles.rs";
const MAPPING: &str = "framekernel/src/mapping.rs";

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
    // TASKS item 143, step 3: `onbf-format`.
    Control {
        name: "section-count-zero",
        file: HEADER,
        from: "        if !(1..=MAX_SECTIONS).contains(&section_count) {\n",
        to: "        if section_count > MAX_SECTIONS {\n",
        harnesses: &["proofs::header_parse_any_bytes"],
        expect: &["§4.2: 1 to 256 sections"],
    },
    Control {
        name: "header-arch-unchecked",
        file: HEADER,
        from: "        if arch != ARCH_X86_64 {\n",
        to: "        if false {\n",
        harnesses: &["proofs::header_parse_any_bytes"],
        expect: &["§4.2: arch 1, x86-64"],
    },
    Control {
        name: "stored-size-unlimited",
        file: SECTION,
        from: "        if stored_size > MAX_SECTION_CONTENT || content_size > MAX_SECTION_CONTENT {\n",
        to: "        if content_size > MAX_SECTION_CONTENT {\n",
        harnesses: &["proofs::section_entry_parse_any_entry"],
        expect: &["Appendix A: a section's content is at most 1 TiB"],
    },
    Control {
        name: "merkle-loop-runs-on",
        file: SECTION,
        from: "    while hashes > 1 {\n",
        to: "    while hashes > 0 {\n",
        harnesses: &["proofs::merkle_hash_blocks_any_size"],
        // Kani names an unwinding check by the loop's place in its function: the only loop.
        expect: &["unwinding assertion loop 0"],
    },
    Control {
        name: "align-up-wraps",
        file: READ,
        from: "    Some(x.checked_add(mask)? & !mask)\n",
        to: "    Some(x.wrapping_add(mask) & !mask)\n",
        harnesses: &["proofs::align_up_any_value"],
        expect: &["the next multiple"],
    },
    Control {
        name: "slice-end-clamped",
        file: READ,
        from: "    bytes.get(start..end)\n",
        to: "    bytes.get(start..end.min(bytes.len()))\n",
        harnesses: &["proofs::slice_any_range"],
        expect: &["Some only for a range inside the input"],
    },
    Control {
        name: "ocbor-argument-indexed",
        file: OCBOR,
        from: "            let raw = bytes.get(next..end).ok_or(err(K::Truncated))?;\n",
        to: "            let raw = &bytes[next..end];\n",
        harnesses: &["proofs::ocbor_any_document"],
        expect: &[SLICE_PANIC],
    },
    Control {
        name: "ocbor-trailing-data-accepted",
        file: OCBOR,
        from: "    if pos != bytes.len() {\n        return Err(OcborError::at(pos, K::TrailingData));\n    }\n",
        to: "",
        harnesses: &["proofs::ocbor_validate_then_cursor_read"],
        expect: &["P3: the cursor ends at the document's end"],
    },
    // The specification the stand-in rests on, made wrong: a 4-byte character past U+10FFFF.
    Control {
        name: "utf8-walk-past-u10ffff",
        file: ONBF_PROOFS,
        from: "            0xF4 => (4, 0x80, 0x8F),\n",
        to: "            0xF4 => (4, 0x80, 0xBF),\n",
        harnesses: &["proofs::utf8_walk_equals_core"],
        expect: &["the walk refuses where core does, at the same offset"],
    },
    // The walk made to refuse what core accepts: 0xBF as the second byte after 0xF1 to 0xF3.
    Control {
        name: "utf8-walk-refuses-what-core-accepts",
        file: ONBF_PROOFS,
        from: "            0xF1..=0xF3 => (4, 0x80, 0xBF),\n",
        to: "            0xF1..=0xF3 => (4, 0x80, 0xBE),\n",
        harnesses: &["proofs::utf8_walk_equals_core"],
        expect: &["the walk accepts what core accepts"],
    },
    Control {
        name: "le32-indexed",
        file: SIGNATURE,
        from: "    let s = b.get(at..at.checked_add(4)?)?;\n",
        to: "    let s = &b[at..at + 4];\n",
        harnesses: &["proofs::signature_block_any_input"],
        expect: &[SLICE_PANIC],
    },
    Control {
        name: "padding-first-byte-unchecked",
        file: IMAGE,
        from: "    match pad.iter().position(|&b| b != 0) {\n",
        to: "    match pad.iter().skip(1).position(|&b| b != 0) {\n",
        harnesses: &["image::proofs::check_padding_is_exact"],
        expect: &[
            "Ok only when every byte is zero",
            "the byte named is not zero",
        ],
    },
    // TASKS item 143, step 4: the handle table and the map request.
    Control {
        name: "new-quota-uncapped",
        file: HANDLES,
        from: "            limit: limit.min(MAX_HANDLES),\n",
        to: "            limit,\n",
        harnesses: &["handles::proofs::new_is_whole"],
        expect: &["P11: live within the quota"],
    },
    Control {
        name: "join-without-the-one",
        file: HANDLES,
        from: "    ((index as u64 + 1) << GENERATION_BITS) | (generation & LAST_GENERATION)\n",
        to: "    ((index as u64) << GENERATION_BITS) | (generation & LAST_GENERATION)\n",
        harnesses: &["handles::proofs::the_encoding_is_e1s"],
        expect: &["the handle E1 describes"],
    },
    Control {
        name: "missing-slot-as-denied",
        file: HANDLES,
        from: "too (ADR-0017 Z9).\n        let index = nospec::index(index, self.slots.len()).ok_or(Error::BadHandle)?;\n",
        to: "too (ADR-0017 Z9).\n        let index = nospec::index(index, self.slots.len()).ok_or(Error::AccessDenied)?;\n",
        harnesses: &[
            "handles::proofs::get_is_exactly_p1_at_0",
            "handles::proofs::get_is_exactly_p1_at_1",
            "handles::proofs::get_is_exactly_p1_at_2",
            "handles::proofs::remove_keeps_the_table_whole_at_0",
        ],
        expect: &["get is exactly P1's answer", "remove refuses as get does"],
    },
    Control {
        name: "rights-loosened",
        file: HANDLES,
        from: "                } else if r & need.rights == need.rights {\n",
        to: "                } else if r & need.rights != 0 || need.rights == 0 {\n",
        harnesses: &[
            "handles::proofs::get_is_exactly_p1_at_1",
            "handles::proofs::get_is_exactly_p1_at_2",
        ],
        expect: &["get is exactly P1's answer"],
    },
    Control {
        name: "live-not-counted",
        file: HANDLES,
        from: "        self.live += 1;\n",
        to: "",
        harnesses: &[
            "handles::proofs::insert_keeps_the_table_whole_at_0",
            "handles::proofs::insert_keeps_the_table_whole_at_1",
            "handles::proofs::insert_keeps_the_table_whole_at_2",
        ],
        expect: &["live counts the full slots", "one more handle"],
    },
    Control {
        name: "generation-not-bumped",
        file: HANDLES,
        from: "            slot.generation += 1;\n",
        to: "",
        harnesses: &[
            "handles::proofs::remove_keeps_the_table_whole_at_1",
            "handles::proofs::remove_keeps_the_table_whole_at_2",
        ],
        expect: &[
            "remove moves the slot to the next generation, free",
            "E1: a generation never comes down",
        ],
    },
    Control {
        name: "last-generation-reused",
        file: HANDLES,
        from: "        if slot.generation < LAST_GENERATION {\n",
        to: "        if true {\n",
        harnesses: &[
            "handles::proofs::remove_keeps_the_table_whole_at_1",
            "handles::proofs::remove_keeps_the_table_whole_at_2",
        ],
        expect: &[
            "E1: a generation from 1 to the last",
            "H7: a slot used at its last generation is retired",
        ],
    },
    Control {
        name: "duplicate-of-anything",
        file: HANDLES,
        from: "        let (object, old) = self.get(handle, need)?;\n",
        to: "        let Ok((object, old)) = self.get(handle, need) else {\n            return Ok(handle);\n        };\n",
        harnesses: &[
            "handles::proofs::duplicate_only_shrinks_rights_at_0",
            "handles::proofs::duplicate_only_shrinks_rights_at_1",
            "handles::proofs::duplicate_only_shrinks_rights_at_2",
        ],
        expect: &["a duplicate only of a handle that meets the need"],
    },
    Control {
        name: "duplicate-widens",
        file: HANDLES,
        from: "        if new_rights & !old != 0 {\n            return Err(Error::InvalidArgs);\n        }\n",
        to: "",
        harnesses: &[
            "handles::proofs::duplicate_only_shrinks_rights_at_1",
            "handles::proofs::duplicate_only_shrinks_rights_at_2",
        ],
        expect: &["P2: rights only shrink"],
    },
    Control {
        name: "batch-refusal-as-success",
        file: HANDLES,
        from: "        self.check_all(list, need)?;\n",
        to: "        if self.check_all(list, need).is_err() {\n            return Ok(Vec::new());\n        }\n",
        harnesses: &[
            "handles::proofs::a_batch_moves_entirely_or_not_at_all_at_0_of_1",
            "handles::proofs::a_batch_moves_entirely_or_not_at_all_at_0_of_2",
            "handles::proofs::a_batch_moves_entirely_or_not_at_all_at_1_of_1",
            "handles::proofs::a_batch_moves_entirely_or_not_at_all_at_1_of_2",
            "handles::proofs::a_batch_moves_entirely_or_not_at_all_at_2_of_1",
            "handles::proofs::a_batch_moves_entirely_or_not_at_all_at_2_of_2",
        ],
        expect: &["take_all moves only what check_all accepts"],
    },
    Control {
        name: "batch-partial",
        file: HANDLES,
        from: "        self.check_all(list, need)?;\n",
        to: "",
        harnesses: &[
            "handles::proofs::a_batch_moves_entirely_or_not_at_all_at_1_of_2",
            "handles::proofs::a_batch_moves_entirely_or_not_at_all_at_2_of_2",
        ],
        expect: &["a refused batch changes nothing"],
    },
    Control {
        name: "reserve-past-the-quota",
        file: HANDLES,
        from: "        if count > self.free_slots() {\n",
        to: "        if count > self.free_slots() + 1 {\n",
        harnesses: &[
            "handles::proofs::reserve_makes_room_at_0_for_1",
            "handles::proofs::reserve_makes_room_at_0_for_2",
            "handles::proofs::reserve_makes_room_at_1_for_1",
            "handles::proofs::reserve_makes_room_at_1_for_2",
            "handles::proofs::reserve_makes_room_at_2_for_1",
            "handles::proofs::reserve_makes_room_at_2_for_2",
        ],
        expect: &["H6: Ok exactly when the quota has room"],
    },
    Control {
        name: "write-with-execute-denied-not-invalid",
        file: MAPPING,
        from: "        if bits & !map_access::ALL != 0 || bits & map_access::READ == 0 || (write && execute) {\n",
        to: "        if bits & !map_access::ALL != 0 || bits & map_access::READ == 0 {\n",
        harnesses: &["mapping::proofs::allow_from_every_bits"],
        expect: &["access_denied only for execute"],
    },
    Control {
        name: "map-rights-loosened",
        file: MAPPING,
        from: "    if held & allow.needs() != allow.needs() {\n",
        to: "    if held & allow.needs() == 0 {\n",
        harnesses: &["mapping::proofs::check_every_request"],
        expect: &["check is exactly what C6 to C8 and D2 allow, in the module's order"],
    },
    Control {
        name: "within-loosened",
        file: MAPPING,
        from: "            (Self::Read, _) | (Self::ReadWrite, Self::ReadWrite)\n",
        to: "            (Self::Read, _) | (Self::ReadWrite, _)\n",
        harnesses: &["mapping::proofs::within_is_allows_nothing_more"],
        expect: &["within: allows nothing more"],
    },
    // From the review of step 4 (R143-S4-1 to S4-6): what each step keeps, memory, and the map's ranges.
    Control {
        name: "generation-wraps-at-2-32",
        file: HANDLES,
        from: "            slot.generation += 1;\n",
        to: "            slot.generation = if slot.generation == 0xFFFF_FFFF { 1 } else { slot.generation + 1 };\n",
        harnesses: &[
            "handles::proofs::remove_keeps_the_table_whole_at_1",
            "handles::proofs::remove_keeps_the_table_whole_at_2",
        ],
        expect: &[
            "E1: a generation never comes down",
            "remove moves the slot to the next generation, free",
        ],
    },
    Control {
        name: "duplicate-widens-the-original",
        file: HANDLES,
        from: "        let copy = object.clone();\n",
        to: "        let copy = object.clone();\n        if let Some((i, _)) = split(handle) {\n            if let Some(Slot { entry: Some((_, r)), .. }) = self.slots.get_mut(i) {\n                *r = u32::MAX;\n            }\n        }\n",
        harnesses: &[
            "handles::proofs::duplicate_only_shrinks_rights_at_1",
            "handles::proofs::duplicate_only_shrinks_rights_at_2",
        ],
        expect: &["P2: a slot the operation does not name is unchanged"],
    },
    Control {
        name: "batch-reversed",
        file: HANDLES,
        from: "        for &handle in list {\n            taken.push(self.remove(handle, need)?);\n",
        to: "        for &handle in list.iter().rev() {\n            taken.push(self.remove(handle, need)?);\n",
        harnesses: &["handles::proofs::a_batch_moves_entirely_or_not_at_all_at_2_of_2"],
        expect: &["P2: each moved entry is the one named, in the list's order"],
    },
    Control {
        name: "batch-widens-what-it-moves",
        file: HANDLES,
        from: "            taken.push(self.remove(handle, need)?);\n",
        to: "            taken.push(self.remove(handle, need).map(|(o, _)| (o, u32::MAX))?);\n",
        harnesses: &[
            "handles::proofs::a_batch_moves_entirely_or_not_at_all_at_1_of_1",
            "handles::proofs::a_batch_moves_entirely_or_not_at_all_at_2_of_1",
            "handles::proofs::a_batch_moves_entirely_or_not_at_all_at_2_of_2",
        ],
        expect: &["P2: each moved entry is the one named, in the list's order"],
    },
    Control {
        name: "insert-leaves-remove-no-room",
        file: HANDLES,
        from: "                if self.slots.try_reserve(1).is_err() || self.ensure_free_room(1).is_err() {\n",
        to: "                if self.slots.try_reserve(1).is_err() {\n",
        harnesses: &[
            "handles::proofs::insert_keeps_the_table_whole_at_0",
            "handles::proofs::insert_keeps_the_table_whole_at_1",
            "handles::proofs::insert_keeps_the_table_whole_at_2",
        ],
        expect: &["the free list has room for every slot, so remove never allocates"],
    },
    Control {
        name: "reserve-takes-no-memory",
        file: HANDLES,
        from: "        self.slots.try_reserve(fresh).map_err(|_| Error::NoMemory)?;\n        self.ensure_free_room(fresh)\n",
        to: "        let _ = fresh;\n        Ok(())\n",
        harnesses: &[
            "handles::proofs::reserve_makes_room_at_0_for_1",
            "handles::proofs::reserve_makes_room_at_0_for_2",
            "handles::proofs::reserve_makes_room_at_1_for_1",
            "handles::proofs::reserve_makes_room_at_1_for_2",
            "handles::proofs::reserve_makes_room_at_2_for_1",
            "handles::proofs::reserve_makes_room_at_2_for_2",
        ],
        expect: &["the memory for every reserved insert is taken now"],
    },
    Control {
        name: "reserve-at-the-quota-refused",
        file: HANDLES,
        from: "        if count > self.free_slots() {\n",
        to: "        if count >= self.free_slots() {\n",
        harnesses: &[
            "handles::proofs::reserve_makes_room_at_0_for_0",
            "handles::proofs::reserve_makes_room_at_0_for_1",
            "handles::proofs::reserve_makes_room_at_0_for_2",
            "handles::proofs::reserve_makes_room_at_1_for_0",
            "handles::proofs::reserve_makes_room_at_1_for_1",
            "handles::proofs::reserve_makes_room_at_1_for_2",
            "handles::proofs::reserve_makes_room_at_2_for_0",
            "handles::proofs::reserve_makes_room_at_2_for_1",
            "handles::proofs::reserve_makes_room_at_2_for_2",
        ],
        expect: &["H6: Ok exactly when the quota has room"],
    },
    Control {
        name: "get-type-after-rights",
        file: HANDLES,
        from: "                if need.object != 0 && object.object_type() != need.object {\n                    Err(Error::WrongType)\n                } else if r & need.rights == need.rights {\n                    Ok((object, *r))\n                } else {\n                    Err(Error::AccessDenied)\n                }\n",
        to: "                if r & need.rights != need.rights {\n                    Err(Error::AccessDenied)\n                } else if need.object != 0 && object.object_type() != need.object {\n                    Err(Error::WrongType)\n                } else {\n                    Ok((object, *r))\n                }\n",
        harnesses: &[
            "handles::proofs::get_is_exactly_p1_at_1",
            "handles::proofs::get_is_exactly_p1_at_2",
        ],
        expect: &["get is exactly P1's answer"],
    },
    Control {
        name: "user-start-unchecked",
        file: MAPPING,
        from: "    if !base.is_multiple_of(PAGE) || base < USER_START {\n",
        to: "    if !base.is_multiple_of(PAGE) {\n",
        harnesses: &["mapping::proofs::check_every_request"],
        expect: &["check is exactly what C6 to C8 and D2 allow, in the module's order"],
    },
    Control {
        name: "fixed-address-alignment-unchecked",
        file: MAPPING,
        from: "    if !base.is_multiple_of(PAGE) || base < USER_START {\n",
        to: "    if base < USER_START {\n",
        harnesses: &["mapping::proofs::check_every_request"],
        expect: &["check is exactly what C6 to C8 and D2 allow, in the module's order"],
    },
    // Both of the range's checks loosened together: the second (nospec's clamps) refuses on the kept path too.
    Control {
        name: "object-range-unchecked",
        file: MAPPING,
        from: "    if pages == 0\n        || first\n            .checked_add(pages)\n            .is_none_or(|end| end > object_pages)\n    {\n        return Err(Error::InvalidArgs);\n    }\n    // The offset and the length are a program's and pick the object's frames\n    // (`MemoryObject::run`). `nospec` keeps them inside the object on a wrongly guessed path\n    // too; the check above has refused whatever these would.\n    let pages = at_most(pages, object_pages).ok_or(Error::InvalidArgs)?;\n    let first = at_most(first, object_pages - pages).ok_or(Error::InvalidArgs)?;\n",
        to: "    if pages == 0 {\n        return Err(Error::InvalidArgs);\n    }\n",
        harnesses: &["mapping::proofs::check_every_request"],
        expect: &["check is exactly what C6 to C8 and D2 allow, in the module's order"],
    },
];

/// The check Kani 0.68.0 reports for a panic whose message core formats at run time, as a
/// slice index past the end's is (seen on the first run of `le32-indexed`).
const SLICE_PANIC: &str =
    "This is a placeholder message; Kani doesn't support message formatted at runtime";

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
    // `-Z stubbing`: the heap's harnesses stub core's slice rotation (`heap/proofs.rs`), and
    // `onbf-format`'s stub core's UTF-8 check (`proofs.rs`). Kani 0.68.0 calls the feature
    // unstable; the version is pinned.
    // `--lib`: every proof is in a library, and `framekernel`'s package also holds the kernel
    // image's binary, which builds only for `x86_64-unknown-none`.
    command
        .current_dir(dir)
        .args(["kani", "-Z", "stubbing", "-p", package, "--lib"]);
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

/// The file a harness's run is logged to: its name with `::` made `/`, since CI's artifact
/// upload refuses a path holding `"`, `:`, `<`, `>`, `|`, `*`, `?`, CR or LF.
fn log_name(harness: &str) -> String {
    format!("{}.log", harness.replace("::", "/"))
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

/// Which proofs a run takes: all of them, one harness, or one package's (CI's jobs, one for each
/// package of [`PROOF_FILES`]: the coordinating session's ruling of 2026-11-27, after the one
/// job passed 120 minutes).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Choice<'a> {
    All,
    Harness(&'a str),
    Package(&'a str),
}

impl Choice<'_> {
    /// Whether the choice takes the harness `name` of `package`.
    fn takes(self, package: &str, name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Harness(only) => only == name,
            Self::Package(only) => only == package,
        }
    }
}

/// The harnesses a run takes, each with its package.
type Harnesses<'a> = Vec<&'a (&'static str, String)>;
/// The controls a run takes, each with the harnesses it runs on.
type Controls = Vec<(&'static Control, Vec<&'static str>)>;

/// The harnesses `choice` takes from `all`, and each control it runs with the harnesses it runs
/// it on: a control runs when it names a harness the choice takes, on those of its harnesses.
fn selection<'a>(
    all: &'a [(&'static str, String)],
    choice: Choice<'_>,
) -> (Harnesses<'a>, Controls) {
    let harnesses: Vec<_> = all.iter().filter(|(p, n)| choice.takes(p, n)).collect();
    let package_of_harness = |name: &str| all.iter().find(|(_, n)| n == name).map(|(p, _)| *p);
    let controls = CONTROLS
        .iter()
        .filter_map(|control| {
            let on: Vec<&'static str> = control
                .harnesses
                .iter()
                .copied()
                .filter(|h| package_of_harness(h).is_some_and(|p| choice.takes(p, h)))
                .collect();
            (!on.is_empty()).then_some((control, on))
        })
        .collect();
    (harnesses, controls)
}

/// Refuses a choice that names no harness or no package of [`PROOF_FILES`].
fn check_choice(all: &[(&'static str, String)], choice: Choice<'_>) -> Result {
    match choice {
        Choice::All => Ok(()),
        Choice::Harness(name) if all.iter().any(|(_, n)| n == name) => Ok(()),
        Choice::Harness(name) => Err(format!("kani: no harness {name}")),
        Choice::Package(package) if PROOF_FILES.iter().any(|p| p.package == package) => Ok(()),
        Choice::Package(package) => Err(format!(
            "kani: no proof file is in package {package} (PROOF_FILES)"
        )),
    }
}

/// `cargo xtask kani [--package <name>] --list`: what such a run would run, and how much, with
/// no Kani and no turn: so that the jobs' union can be counted against the whole.
pub(crate) fn list(choice: Choice<'_>) -> Result {
    let all = defined(&repo_root())?;
    check_choice(&all, choice)?;
    let (harnesses, controls) = selection(&all, choice);
    for (package, name) in &harnesses {
        println!("xtask: kani: harness {name} ({package})");
    }
    for (control, on) in &controls {
        println!("xtask: kani: control {} on {}", control.name, on.join(", "));
    }
    println!(
        "xtask: kani: {} harnesses and {} controls ({} control runs of a harness) for {choice:?}",
        harnesses.len(),
        controls.len(),
        controls.iter().map(|(_, on)| on.len()).sum::<usize>()
    );
    Ok(())
}

/// `cargo xtask kani [--harness <name> | --package <name>]`: the proofs, then their controls.
pub(crate) fn run(choice: Choice<'_>) -> Result {
    gate()?;
    check_version()?;
    let root = repo_root();
    let logs = target_dir().join("xtask/kani");
    let all = defined(&root)?;
    check_choice(&all, choice)?;
    let (chosen, chosen_controls) = selection(&all, choice);
    let packages: BTreeSet<&str> = chosen.iter().map(|(p, _)| *p).collect();
    let mut problems = Vec::new();
    // One `cargo kani` for each harness, so that each run's peak memory is that harness's (CI
    // records it; the coordinating session's ruling of 2026-11-27 on Q4's 8 GB).
    let mut reported = Vec::new();
    for package in packages {
        let names: Vec<&str> = chosen
            .iter()
            .filter(|(p, _)| *p == package)
            .map(|(_, n)| n.as_str())
            .collect();
        for name in &names {
            let output = cargo_kani(&root, None, package, &[name], &logs.join(log_name(name)))?;
            problems.extend(proof_warnings(&output));
            let report = parse_report(&output);
            match report.iter().find(|(n, _)| n == name) {
                Some((_, outcome)) => {
                    problems.extend(proof_problem(name, outcome));
                    println!("xtask: kani: {name}: {}", memory_shown(&output));
                }
                None => problems.push(format!("{name}: Kani did not report it")),
            }
            for (other, _) in &report {
                if other != name {
                    problems.push(format!(
                        "{other}: reported in {name}'s run, which asked for {name} alone"
                    ));
                }
            }
            reported.extend(report.into_iter().map(|(n, _)| n));
        }
    }
    // Each harness once: what Kani reported, across all the runs, is the harnesses chosen.
    let mut once: Vec<&str> = reported.iter().map(String::as_str).collect();
    once.sort_unstable();
    let mut wanted: Vec<&str> = chosen.iter().map(|(_, n)| n.as_str()).collect();
    wanted.sort_unstable();
    if once != wanted {
        problems.push(format!(
            "Kani reported {} harness results for {} harnesses: each harness runs once",
            once.len(),
            wanted.len()
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
    controls(&root, &chosen_controls)
}

/// Runs each control `selection` chose, on the harnesses it chose for it.
fn controls(root: &Path, chosen: &Controls) -> Result {
    let scratch = target_dir().join("kani-scratch/tree");
    let target = target_dir().join("kani-scratch/target");
    let logs = target_dir().join("xtask/kani/controls");
    let mut ran = 0;
    for (control, harnesses) in chosen {
        let control = *control;
        let package = package_of(control.file)?;
        populate(root, &scratch)?;
        apply(control, &scratch)?;
        let output = cargo_kani(
            &scratch,
            Some(&target),
            package,
            harnesses,
            &logs.join(format!("{}.log", control.name)),
        )?;
        let report = parse_report(&output);
        let mut problems = Vec::new();
        for name in harnesses {
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
    // A control may plant its bug in any file of a proof file's crate: under the directory that
    // holds the proof module's parent (`onbf-format`'s parsers sit beside `lib.rs`).
    PROOF_FILES
        .iter()
        .find(|p| {
            p.parent == file
                || p.file == file
                || Path::new(p.parent)
                    .parent()
                    .is_some_and(|dir| Path::new(file).starts_with(dir))
        })
        .map(|p| p.package)
        .ok_or_else(|| format!("kani: no proof file's package holds {file}"))
}

/// Texts that must each stand once, as they are, in a file: the privacy the handle table's
/// proofs rest on. Its entries and their rights are reached only through its methods
/// (`framekernel/src/handles/proofs.rs`), and the doctests in `handles.rs` show it only at the
/// crate's edge; a `pub(crate)` on a field or on `Slot` would pass them and void the proofs'
/// premise inside the kernel (TASKS item 143, step 4, R143-S4-7).
const PRIVATE_PINS: &[(&str, &str)] = &[
    (
        HANDLES,
        "pub struct HandleTable<T> {\n    slots: Vec<Slot<T>>,\n    free: Vec<u32>,\n    limit: usize,\n    live: usize,\n}\n",
    ),
    (HANDLES, "\nstruct Slot<T> {\n"),
];

/// Each [`PRIVATE_PINS`] text that does not stand once in its file, as `read` gives it.
fn private_pin_problems(read: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    PRIVATE_PINS
        .iter()
        .filter_map(|(file, text)| {
            let found = read(file).map_or(0, |source| source.matches(text).count());
            (found != 1).then(|| {
                format!(
                    "{file}: the handle table's fields and `Slot` must stay private to their module, as the proofs assume: expected once, found {found} times: {text:?}"
                )
            })
        })
        .collect()
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
    problems.extend(private_pin_problems(&|file| {
        fs::read_to_string(root.join(file)).ok()
    }));
    // The CI job installs this version and runs the proofs.
    let script = fs::read_to_string(root.join(INSTALL_SCRIPT)).unwrap_or_default();
    if !script.contains(&format!("KANI_VERSION={VERSION}\n")) {
        problems.push(format!(
            "{INSTALL_SCRIPT} does not pin KANI_VERSION={VERSION}"
        ));
    }
    let workflow = fs::read_to_string(root.join(WORKFLOW)).unwrap_or_default();
    problems.extend(workflow_problems(&workflow));
    problems.extend(control_package_problems(&defined));
    if problems.is_empty() {
        println!(
            "xtask: kani-gate: {} harnesses in {} proof files, {} controls, each harness controlled and each control in its own package; CI's matrix lists the {} packages; `kani` only on the proof modules' declarations",
            names.len(),
            PROOF_FILES.len(),
            CONTROLS.len(),
            proof_packages().len()
        );
        Ok(())
    } else {
        Err(format!("kani-gate:\n  {}", problems.join("\n  ")))
    }
}

/// The packages of [`PROOF_FILES`], each once, in the order the list first names them: CI's
/// jobs, one for each.
fn proof_packages() -> Vec<&'static str> {
    let mut packages = Vec::new();
    for proof in PROOF_FILES {
        if !packages.contains(&proof.package) {
            packages.push(proof.package);
        }
    }
    packages
}

/// What the CI job `kani` must hold, each once: Kani installed by [`INSTALL_SCRIPT`], a matrix
/// that lists exactly [`proof_packages`], in that order, and the run of one package for each.
fn workflow_problems(workflow: &str) -> Vec<String> {
    let lines = [
        format!("        run: bash {INSTALL_SCRIPT}\n"),
        format!("        package: [{}]\n", proof_packages().join(", ")),
        "      - run: cargo --locked xtask kani --package ${{ matrix.package }}\n".to_owned(),
    ];
    let mut problems = Vec::new();
    for line in &lines {
        let found = workflow.matches(line.as_str()).count();
        if found != 1 {
            problems.push(format!(
                "{WORKFLOW}: the job `kani` must hold `{}` once, as PROOF_FILES gives it; found it {found} times",
                line.trim()
            ));
        }
    }
    let runs = workflow
        .lines()
        .filter(|l| {
            let l = l.trim_start().trim_start_matches("- ");
            l.starts_with("run:") && l.contains("xtask kani")
        })
        .count();
    if runs != 1 {
        problems.push(format!(
            "{WORKFLOW}: one step runs `xtask kani`, one package for each job; found {runs}"
        ));
    }
    problems
}

/// Each control's harnesses must all be in the package of its file: a package's job runs the
/// control on that package alone, so one that named another's harness would run it nowhere.
fn control_package_problems(defined: &[(&'static str, String)]) -> Vec<String> {
    let mut problems = Vec::new();
    for control in CONTROLS {
        let Ok(package) = package_of(control.file) else {
            problems.push(format!(
                "control {}: no proof file's package holds {}",
                control.name, control.file
            ));
            continue;
        };
        for harness in control.harnesses {
            let of = defined.iter().find(|(_, n)| n == harness).map(|(p, _)| *p);
            if of.is_some_and(|p| p != package) {
                problems.push(format!(
                    "control {}: harness {harness} is not in {package}, the package of {}",
                    control.name, control.file
                ));
            }
        }
    }
    problems
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
        // A control's file is a path inside the tree, so that applying it to the scratch copy
        // can never write outside that copy.
        let path = Path::new(control.file);
        if path.is_absolute()
            || path
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            problems.push(format!(
                "control {}: {} is not a plain path inside the tree",
                control.name, control.file
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
    fn the_handle_tables_privacy_is_pinned() {
        let real = |file: &str| std::fs::read_to_string(crate::repo_root().join(file)).ok();
        assert!(super::private_pin_problems(&real).is_empty());
        let widened = |file: &str| {
            real(file).map(|s| {
                s.replace(
                    "    slots: Vec<Slot<T>>,",
                    "    pub(crate) slots: Vec<Slot<T>>,",
                )
            })
        };
        assert_eq!(super::private_pin_problems(&widened).len(), 1);
        let slot_public = |file: &str| {
            real(file).map(|s| s.replace("\nstruct Slot<T> {", "\npub(crate) struct Slot<T> {"))
        };
        assert_eq!(super::private_pin_problems(&slot_public).len(), 1);
    }

    #[test]
    fn no_log_name_holds_a_character_the_artifact_upload_refuses() {
        let defined = super::defined(&crate::repo_root()).expect("the proof files read");
        assert!(!defined.is_empty());
        for (_, name) in &defined {
            let log = super::log_name(name);
            assert!(
                !log.contains(['"', ':', '<', '>', '|', '*', '?', '\r', '\n']),
                "{log}"
            );
        }
        assert_eq!(
            super::log_name("image::proofs::check_padding_is_exact"),
            "image/proofs/check_padding_is_exact.log"
        );
    }

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

    /// CI's jobs, one for each package, run together every harness and every control exactly
    /// once, each control on all of its harnesses, and the whole run runs the same (the
    /// coordinating session's ruling of 2026-11-27).
    #[test]
    fn the_package_jobs_run_everything_once() {
        let all = defined(&repo_root()).unwrap();
        let mut harnesses: Vec<&str> = Vec::new();
        let mut controls: Vec<(&str, Vec<&str>)> = Vec::new();
        for package in proof_packages() {
            check_choice(&all, Choice::Package(package)).unwrap();
            let (h, c) = selection(&all, Choice::Package(package));
            assert!(!h.is_empty(), "{package}'s job runs a harness");
            assert!(h.iter().all(|(p, _)| *p == package));
            harnesses.extend(h.iter().map(|(_, n)| n.as_str()));
            controls.extend(c.iter().map(|(control, on)| (control.name, on.clone())));
        }
        let mut every: Vec<&str> = all.iter().map(|(_, n)| n.as_str()).collect();
        every.sort_unstable();
        harnesses.sort_unstable();
        assert_eq!(harnesses, every, "each harness in exactly one job");
        controls.sort_unstable();
        let mut wanted: Vec<(&str, Vec<&str>)> = CONTROLS
            .iter()
            .map(|c| (c.name, c.harnesses.to_vec()))
            .collect();
        wanted.sort_unstable();
        assert_eq!(
            controls, wanted,
            "each control in exactly one job, on all its harnesses"
        );
        let (h, c) = selection(&all, Choice::All);
        assert_eq!(h.len(), all.len());
        assert_eq!(c.len(), CONTROLS.len());
        assert!(
            c.iter()
                .all(|(control, on)| on.as_slice() == control.harnesses)
        );
    }

    /// One harness takes the controls that name it, on it alone; a name or a package that is
    /// not there is refused.
    #[test]
    fn a_choice_takes_what_it_names() {
        let all = defined(&repo_root()).unwrap();
        let name = "handles::proofs::new_is_whole";
        let (h, c) = selection(&all, Choice::Harness(name));
        assert_eq!(h.len(), 1);
        assert!(!c.is_empty());
        for (control, on) in &c {
            assert!(control.harnesses.contains(&name));
            assert_eq!(on.as_slice(), [name]);
        }
        let naming = CONTROLS
            .iter()
            .filter(|c| c.harnesses.contains(&name))
            .count();
        assert_eq!(c.len(), naming);
        assert!(check_choice(&all, Choice::Harness("handles::proofs::nothing")).is_err());
        assert!(check_choice(&all, Choice::Package("origin-abi")).is_err());
        assert!(check_choice(&all, Choice::Package("framekernel")).is_ok());
    }

    /// The tree's workflow and controls pass; a matrix that leaves a package out, a second run
    /// of the proofs, or a control on another package's harness does not.
    #[test]
    fn the_matrix_is_proof_files_packages() {
        let workflow = fs::read_to_string(repo_root().join(WORKFLOW)).unwrap();
        assert_eq!(workflow_problems(&workflow), Vec::<String>::new());
        assert_eq!(
            proof_packages(),
            ["origin-frame", "onbf-format", "framekernel"]
        );
        let line = "        package: [origin-frame, onbf-format, framekernel]\n";
        let short = workflow.replace(line, "        package: [origin-frame, onbf-format]\n");
        assert_eq!(workflow_problems(&short).len(), 1);
        let whole = workflow.replace(
            "      - run: cargo --locked xtask kani --package ${{ matrix.package }}\n",
            "      - run: cargo --locked xtask kani\n",
        );
        assert_eq!(workflow_problems(&whole).len(), 1);
        let twice = format!("{workflow}      - run: cargo --locked xtask kani\n");
        assert_eq!(workflow_problems(&twice).len(), 1);
        let all = defined(&repo_root()).unwrap();
        assert_eq!(control_package_problems(&all), Vec::<String>::new());
        let moved: Vec<(&'static str, String)> = all
            .iter()
            .map(|(p, n)| {
                let p = if n == "handles::proofs::new_is_whole" {
                    "onbf-format"
                } else {
                    p
                };
                (p, n.clone())
            })
            .collect();
        assert!(!control_package_problems(&moved).is_empty());
    }
}
