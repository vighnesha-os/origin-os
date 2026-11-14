//! The boot proof "a Rust program in ring 3" (task S4-1; ADR-0011 Amendment 2, L1 and L8;
//! ADR-0013's row of that name).
//!
//! It launches the first program the kernel carries (`tests/kernel/first-program`) six times
//! through the launch path (`launch::load_program`, then `launch::start_program`), each with
//! one argument naming what it is to do, and checks what each did against what the kernel gave
//! it:
//!
//! - twice with `run`, on two processors: each runs in ring 3 at a random base, and the two
//!   bases differ, and their stacks; each finds its start record in RDI and reports its header
//!   and entries as the kernel wrote them; it runs at its base plus its entry; its tables map
//!   each page of its span as its description says and its stack writable and never executable,
//!   with an unmapped page on either side, walked before its thread starts; every relocated
//!   slot in its pages holds its addend plus the base, read back from its memory before its
//!   thread starts; it reads its data only through relocated pointers and gets what the compiler
//!   worked out; the runtime's `mem*` functions copy, move, fill and compare; it writes a message
//!   on BOOTSTRAP that names it as the writer; and it ends with its own exit code;
//! - `write-code`: a write to its own code ends it with a page fault at that address, the error
//!   code saying a write to a present user page;
//! - `jump-data`: a jump into its own writable data ends it with a page fault at that address,
//!   the error code saying an instruction fetch;
//! - `write-relro`: a write to a relocated pointer in its RELRO page ends it with a page fault
//!   at that address, the error code saying a write to a present user page;
//! - `panic`: it ends with the runtime's exit code for a panic.
//!
//! The walk of the tables, and the read-back of the slots, are done for all six launches.
//!
//! Before them, two launches are refused, and each takes nothing: the heap and the free frames,
//! page tables among them, are where they were before it (S4-1's review, finding F2). One is
//! refused before anything is made (a stack of no pages); the other part-way through mapping,
//! at the bound on a space's page tables, after its image and part of its stack are mapped (a
//! stack of [`PART_WAY_STACK`] pages). Every process, and everything it held, is gone
//! afterwards: the heap and the free frames are back where they were. The values the program
//! reports, and their order, are its contract with this file, which keeps its own copy of each
//! of the program's constants.
#![forbid(unsafe_code)]

use alloc::vec::Vec;
use core::fmt::Write;

use origin_abi::start::role;
use origin_abi::{Error, ObjectType, rights};

use crate::console::Console;
use crate::cpu::{self, CpuIndex};
use crate::handles::Need;
use crate::ipc::{self, Object};
use crate::launch::{self, Given, LoadedProgram, ProgramLimits};
use crate::paging::{PAGE, Translation};
use crate::process::{BOOT_PROOF_PRINCIPAL, Ending, Limits, MemoryLimits, ObjectLimits, Process};
use crate::program::{self, Carried, FIRST_PROGRAM, FIRST_PROGRAM_RUN, SegmentAccess};
use crate::{memory, sched};

// The first program's constants (`tests/kernel/first-program/src/main.rs`), copied.
/// The exit code of a `run` that did everything it was asked.
const RAN: u64 = 0x0052_5553_5433;
/// What it writes on BOOTSTRAP.
const GREETING: &[u8] = b"first program: Rust in ring 3";
/// Reported before each deliberate fault, then the address it faults at.
const ABOUT_TO_WRITE_CODE: u64 = 0xC0DE;
const ABOUT_TO_JUMP_INTO_DATA: u64 = 0xDA7A;
const ABOUT_TO_PANIC: u64 = 0x9A1C;
const ABOUT_TO_WRITE_RELRO: u64 = 0x2E10;
/// ((3 + 1) × 2)², through its table of relocated function pointers.
const STEPS: u64 = 64;
/// Its five `mem*` checks, one bit each.
const MEM_CHECKS: u64 = 0b1_1111;
/// The runtime's exit code for a panic (`origin_rt::PANIC_EXIT`).
const PANIC_EXIT: u64 = u64::MAX;

/// Where its reports start for each mode, after the record's: RDI, the entry, the ABI, the
/// entry count, two values for each of its three entries, the argument count.
const MODE_REPORTS: usize = 4 + 2 * 3 + 1;

/// A stack this long cannot be mapped: each page table maps 512 pages (2 MiB), so its pages need
/// at least 32 tables of their own, and a space may have 32 in all, its top-level table among
/// them (`MAX_TABLES` in `origin_frame::userspace`). A launch with it is refused part-way through
/// mapping: the image first, then the stack's pages until a table is refused. Well under
/// [`program::MAX_STACK_PAGES`], so it gets that far.
const PART_WAY_STACK: u64 = 32 * 512;

/// The page fault's vector, and its error code's bits (Intel SDM Vol. 3A, §4.7; AMD64 APM Vol. 2,
/// §8.4.2): present, write, user, and instruction fetch.
const PAGE_FAULT: u64 = 14;
const PF_PRESENT: u64 = 1;
const PF_WRITE: u64 = 2;
const PF_USER: u64 = 4;
const PF_RESERVED: u64 = 8;
const PF_FETCH: u64 = 16;

/// What the creator holds: the other end of each child's BOOTSTRAP.
const CREATOR: Limits = Limits {
    handles: 8,
    channels: 8,
    message_bytes: 4096,
    debug_calls: false,
    memory: MemoryLimits::NONE,
    objects: ObjectLimits::NONE,
    // It never runs (ADR-0005 G5, G6).
    threads: 0,
};

/// What one launch showed.
struct Run {
    /// Where the program and its stack went.
    base: u64,
    stack: u64,
    /// Its record found in RDI, and read back as written; and its entry where it should be.
    record_right: bool,
    entry_right: bool,
    /// Its tables mapped each page as its description says, before the thread started.
    tables_right: bool,
    /// Every relocated slot held its addend plus the base before the thread started.
    slots_right: bool,
    /// What it reported after the record's values.
    after: Vec<u64>,
    /// How it ended.
    ending: Option<Ending>,
    /// Its message on BOOTSTRAP, if it wrote one: the bytes, and whether it named this process.
    message: Option<(Vec<u8>, bool)>,
    /// Loading and starting it.
    micros: u64,
}

/// The entries the launch path writes, in order: `role << 32 | type`, and `rights | 1 << 32`
/// (the handle is not zero).
fn expected_entries() -> [(u64, u64); 3] {
    let entry = |role: u32, ty: ObjectType, rights: u32| {
        (
            u64::from(role) << 32 | ty as u64,
            u64::from(rights) | 1 << 32,
        )
    };
    [
        entry(role::SELF_PROCESS, ObjectType::Process, rights::PROCESS),
        entry(role::SELF_THREAD, ObjectType::Thread, rights::THREAD),
        entry(role::BOOTSTRAP, ObjectType::Channel, rights::CHANNEL),
    ]
}

/// Whether `translation`, a page's, is what `access` asks: present to ring 3, a 4 KiB page,
/// writable for a writable segment alone and executable for the code alone; or absent, where
/// `access` is `None`.
fn mapped_as(translation: Option<Translation>, access: Option<SegmentAccess>) -> bool {
    match (translation, access) {
        (None, None) => true,
        (Some(t), Some(access)) => {
            t.user
                && t.size == PAGE
                && t.writable == (access == SegmentAccess::ReadWrite)
                && t.executable == (access == SegmentAccess::ReadExecute)
        }
        _ => false,
    }
}

/// Walks `loaded`'s tables before its first thread exists (S4-1's review, finding F1): each
/// page of the span as its description says ([`program::page_access`]); each page of the stack
/// writable and never executable; and the page below the stack and the page above it unmapped.
fn tables_match(loaded: &LoadedProgram) -> bool {
    let child = &loaded.process;
    let image = (0..FIRST_PROGRAM.span / PAGE).all(|k| {
        mapped_as(
            child.translate(loaded.base + k * PAGE),
            program::page_access(&FIRST_PROGRAM, k * PAGE),
        )
    });
    let stack = (loaded.stack..loaded.stack_top)
        .step_by(PAGE as usize)
        .all(|at| mapped_as(child.translate(at), Some(SegmentAccess::ReadWrite)));
    let neighbours = child.translate(loaded.stack - PAGE).is_none()
        && child.translate(loaded.stack_top).is_none();
    image && stack && neighbours && loaded.stack < loaded.stack_top
}

/// Whether a launch of [`FIRST_PROGRAM`] with a stack of `stack_pages` pages is refused with
/// `error`, and the heap and the free frames, page tables among them, are then where they were
/// before it (S4-1's review, finding F2).
fn refused_whole(creator: &Process, stack_pages: u64, error: Error, tsc_hz: u64) -> bool {
    let counts = || memory::stats().map(|(h, f)| (h.bytes_in_use, f));
    let before = counts();
    let run = ProgramLimits {
        stack_pages,
        ..FIRST_PROGRAM_RUN
    };
    let refused = launch::load_program(creator, &FIRST_PROGRAM, &run).err() == Some(error);
    sched::yield_until(|| counts() == before, tsc_hz, 2);
    refused && before.is_some() && counts() == before
}

/// Launches [`FIRST_PROGRAM`] from `creator` on `cpu` with the argument `mode`, waits for it to
/// end, and returns what it showed. `None` if the launch itself was refused.
fn launch(creator: &Process, mode: &[u8], cpu: CpuIndex, tsc_hz: u64) -> Option<Run> {
    let (child_end, creator_end) = ipc::create_channel(&creator.account).ok()?;
    let held = creator
        .grant(Object::Channel(creator_end), rights::CHANNEL)
        .ok()?;
    let given = Given {
        bootstrap: Some(child_end),
        directory: None,
        args: &[mode],
        env: &[],
    };
    let began = cpu::rdtsc();
    let loaded = launch::load_program(creator, &FIRST_PROGRAM, &FIRST_PROGRAM_RUN).ok()?;
    let loading = cpu::rdtsc().wrapping_sub(began);
    let (base, stack, entry) = (loaded.base, loaded.stack, loaded.entry);
    // Walk the child's own tables, and read back through them, before anything of it runs.
    let tables_right = tables_match(&loaded);
    let slots_right = loaded.process.memory().is_some_and(|space| {
        FIRST_PROGRAM.relocations.iter().all(|&slot| {
            space.read_u64(base + slot) == program::relocated(&FIRST_PROGRAM, slot, base)
        })
    });
    let began = cpu::rdtsc();
    let launched = launch::start_program(loaded, given, cpu).ok()?;
    let starting = cpu::rdtsc().wrapping_sub(began);
    let micros = loading.saturating_add(starting) / (tsc_hz / 1_000_000).max(1);
    sched::yield_until(|| launched.process.ending().is_some(), tsc_hz, 10);
    let reports = launched.process.reports();
    let record = reports.get(..MODE_REPORTS).unwrap_or(&[]);
    let abi = u64::from(origin_abi::ABI_VERSION.0) << 32
        | u64::from(origin_abi::ABI_VERSION.1) << 16
        | u64::from(origin_abi::start::ABI_MIN_MINOR);
    let mut wanted = Vec::new();
    wanted.extend([launched.record, entry, abi, 3]);
    for (a, b) in expected_entries() {
        wanted.extend([a, b]);
    }
    wanted.push(1);
    let record_right =
        record.first() == Some(&launched.record) && record.get(2..) == wanted.get(2..);
    let entry_right = record.get(1) == Some(&entry);
    let message = match ipc::channel_read(&creator.handles, held, Need::NOTHING, 64, 0) {
        Ok(ipc::Read::Message { bytes, sender, .. }) => {
            Some((bytes, sender.process == launched.process.id()))
        }
        _ => None,
    };
    let _ = ipc::handle_close(creator, held, Need::NOTHING);
    Some(Run {
        base,
        stack,
        record_right,
        entry_right,
        tables_right,
        slots_right,
        after: reports.get(MODE_REPORTS..).unwrap_or(&[]).to_vec(),
        ending: launched.process.ending(),
        message,
        micros,
    })
}

/// The `[start, end)` of `program`'s segments with `access`, at `base`.
fn segments(
    program: &Carried,
    base: u64,
    access: SegmentAccess,
) -> impl Iterator<Item = (u64, u64)> {
    program
        .segments
        .iter()
        .filter(move |s| s.access == access)
        .map(move |s| (base + s.rva, base + s.rva + s.size))
}

/// Whether `run` reported `about` and then an address in one of `program`'s segments with
/// `access`, and ended with a page fault at that address whose error code has `bits` set among
/// present, write, user, reserved and fetch, and no other of them.
fn faulted_in(run: &Run, about: u64, access: SegmentAccess, bits: u64) -> bool {
    let [said, target] = run.after.as_slice() else {
        return false;
    };
    let inside =
        segments(&FIRST_PROGRAM, run.base, access).any(|(lo, hi)| (lo..hi).contains(target));
    let mask = PF_PRESENT | PF_WRITE | PF_USER | PF_RESERVED | PF_FETCH;
    *said == about
        && inside
        && matches!(
            run.ending,
            Some(Ending::Faulted { vector: PAGE_FAULT, error, address })
                if address == *target && error & mask == bits
        )
}

/// Runs the proof. Called from the thread proofs with randomness available.
pub(crate) fn prove(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let before = memory::stats();
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let ok = {
        let Some(creator) = Process::proof(CREATOR, (0, 0), BOOT_PROOF_PRINCIPAL) else {
            return false;
        };
        // Refused before anything is made, and refused part-way through mapping: each takes
        // nothing.
        let early = refused_whole(&creator, 0, Error::InvalidArgs, tsc_hz);
        let late = refused_whole(&creator, PART_WAY_STACK, Error::Limit, tsc_hz);
        let first = launch(&creator, b"run", far, tsc_hz);
        let second = launch(&creator, b"run", bsp, tsc_hz);
        let write_code = launch(&creator, b"write-code", bsp, tsc_hz);
        let jump_data = launch(&creator, b"jump-data", bsp, tsc_hz);
        let write_relro = launch(&creator, b"write-relro", bsp, tsc_hz);
        let panicked = launch(&creator, b"panic", bsp, tsc_hz);
        let all = [
            &first,
            &second,
            &write_code,
            &jump_data,
            &write_relro,
            &panicked,
        ];
        let each = |check: fn(&Run) -> bool| all.iter().all(|r| r.as_ref().is_some_and(check));
        let ran = [&first, &second];
        let both = |check: fn(&Run) -> bool| ran.iter().all(|r| r.as_ref().is_some_and(check));

        let record = each(|r| r.record_right);
        let entry = each(|r| r.entry_right);
        let tables = each(|r| r.tables_right);
        let slots = each(|r| r.slots_right);
        let relocated = both(|r| r.after.first() == Some(&1) && r.after.get(1) == Some(&STEPS));
        let mem = both(|r| r.after.get(2) == Some(&MEM_CHECKS));
        let wrote = both(|r| {
            r.after.get(3) == Some(&0)
                && r.message
                    .as_ref()
                    .is_some_and(|(bytes, named)| bytes.as_slice() == GREETING && *named)
        });
        let exited = both(|r| r.ending == Some(Ending::Exited(RAN)) && r.after.len() == 4);
        let ran_right = record && entry && tables && slots && relocated && mem && wrote && exited;
        let _ = writeln!(
            com1,
            "origin-kernel: rust program: {} ran in ring 3 from the kernel's image at random bases, 6 times: each found its start record in RDI and read its header and 3 entries as written: {record}; ran at its base plus its entry: {entry}; its tables mapped each page of its span as described, and its stack writable and never executable between two unmapped pages: {tables}; every relocated slot held its value plus the base: {slots}; read its data through relocated pointers: {relocated}; the runtime's mem functions copy, move, fill and compare: {mem}; wrote on BOOTSTRAP, named as the writer: {wrote}; ended with its exit code: {exited}{}",
            FIRST_PROGRAM.name,
            if ran_right {
                ""
            } else {
                ": rust program failed"
            }
        );

        let apart = match (&first, &second) {
            (Some(a), Some(b)) => a.base != b.base && a.stack != b.stack,
            _ => false,
        };
        let code_faulted = write_code.as_ref().is_some_and(|r| {
            faulted_in(
                r,
                ABOUT_TO_WRITE_CODE,
                SegmentAccess::ReadExecute,
                PF_PRESENT | PF_WRITE | PF_USER,
            )
        });
        let data_faulted = jump_data.as_ref().is_some_and(|r| {
            faulted_in(
                r,
                ABOUT_TO_JUMP_INTO_DATA,
                SegmentAccess::ReadWrite,
                PF_PRESENT | PF_USER | PF_FETCH,
            )
        });
        let relro_faulted = write_relro.as_ref().is_some_and(|r| {
            faulted_in(
                r,
                ABOUT_TO_WRITE_RELRO,
                SegmentAccess::ReadAfterRelocation,
                PF_PRESENT | PF_WRITE | PF_USER,
            )
        });
        let panic_ended = panicked.as_ref().is_some_and(|r| {
            r.after.as_slice() == [ABOUT_TO_PANIC] && r.ending == Some(Ending::Exited(PANIC_EXIT))
        });
        let refused_right =
            apart && code_faulted && data_faulted && relro_faulted && panic_ended && early && late;
        let _ = writeln!(
            com1,
            "origin-kernel: rust program: two launches differ in base and stack: {apart}; a write to its code faulted there: {code_faulted}; a jump into its data faulted there as a fetch: {data_faulted}; a write to its relocated read-only data faulted there: {relro_faulted}; a panic ended it with the runtime's exit code: {panic_ended}; a launch refused before anything was made took nothing: {early}; a launch refused part-way through mapping, at the bound on its page tables, gave back every frame, table and heap byte it took: {late}{}",
            if refused_right {
                ""
            } else {
                ": rust program failed"
            }
        );
        let took = |run: &Option<Run>| run.as_ref().map_or(0, |r| r.micros);
        let _ = writeln!(
            com1,
            "origin-kernel: rust program: loading and starting it took {} us and {} us ({} pages of image, {} relocations)",
            took(&first),
            took(&second),
            FIRST_PROGRAM.span / PAGE,
            FIRST_PROGRAM.relocations.len()
        );
        ran_right && refused_right
    };
    let same = || {
        memory::stats().map(|(h, f)| (h.bytes_in_use, f))
            == before.map(|(h, f)| (h.bytes_in_use, f))
    };
    ipc::reap_closed();
    sched::yield_until(same, tsc_hz, 2);
    let back = same();
    let (heap_now, frames_now) = memory::stats().map_or((0, 0), |(h, f)| (h.bytes_in_use, f));
    let (heap_before, frames_before) = before.map_or((0, 0), |(h, f)| (h.bytes_in_use, f));
    let _ = writeln!(
        com1,
        "origin-kernel: rust program: every launched process and all they held are gone: heap {heap_now} bytes (was {heap_before}), {frames_now} free frames (was {frames_before}){}",
        if back { "" } else { ": rust program failed" }
    );
    ok && back
}
