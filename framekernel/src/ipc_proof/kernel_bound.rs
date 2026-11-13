//! The boot proof "the kernel-wide bound" (task S4-3; ADR-0005 Amendment 8, G5; ADR-0013's row
//! of that name).
//!
//! Every process is paid for out of the kernel's root before it is made, and given back when its
//! account goes (`process`'s module notes). This proof runs after every other proof that makes a
//! process, and shows, each clause printed and judged:
//!
//! - **the costs**: what each kind costs as this build measures it (`process::costs`), printed;
//! - **back before**: once every earlier proof's processes are gone, the root holds its size
//!   again, and it cannot be sized a second time;
//! - **the refusal**: with a process that never runs holding all of the root but one byte less
//!   than a launch of the first program pays (its limits' figure and the pages the launch maps
//!   for it, the start record at its largest), that launch through `launch::load_program` is
//!   refused with `LIMIT`, and nothing is made: the root, the heap in use, the free frames, the
//!   next process ID and the kernel stacks held are as they were;
//! - **paid to the byte**: once the holder has gone, a process with the program's limits takes
//!   exactly their figure; and, where the kernel's generator places programs, the program
//!   launched and run to its end holds its figure and its pages while it runs, each printed apart,
//!   the record's unused pages given back once it is written, and gives everything back after;
//! - **the refund waits**: a channel charged to a process's account outlives the process
//!   object, and the root has its payment back only when the channel goes (the coordinating
//!   session's ruling (c));
//! - **threads counted**: a process given no threads cannot start one, and the refusal takes
//!   nothing (G5's "a kernel stack and an FPU area for each thread" is paid for by that count);
//! - **back after**: when the proof's processes are gone the root holds its size again; and the
//!   most of it the proofs before this one paid out at once is printed, a figure.
//!
//! Its negative controls (ADR-0013): the payment left out of `Process::with_origin`, and the
//! refusal line fails; the repayment left out of `quota::Payment`'s drop, and a "back" line fails.
#![forbid(unsafe_code)]

use core::fmt::Write;

use origin_abi::Error;

use crate::console::Console;
use crate::cpu::CpuIndex;
use crate::ipc;
use crate::launch::{self, Given};
use crate::process::{
    self, BOOT_PROOF_PRINCIPAL, Limits, MemoryLimits, ObjectLimits, Process, ROOT,
};
use crate::program::{FIRST_PROGRAM, FIRST_PROGRAM_RUN};
use crate::{memory, sched};
use origin_frame::objref::ObjectRef;

/// The creator of the launches, which never runs.
const CREATOR: Limits = Limits {
    handles: 4,
    channels: 0,
    message_bytes: 0,
    debug_calls: false,
    memory: MemoryLimits::NONE,
    objects: ObjectLimits::NONE,
    threads: 0,
};

/// A process that never runs and holds `bytes` of the root besides itself: its quota of queued
/// bytes is in its figure byte for byte, and nothing else of it is.
const fn holder(bytes: usize) -> Limits {
    Limits {
        handles: 0,
        channels: 0,
        message_bytes: bytes,
        debug_calls: false,
        memory: MemoryLimits::NONE,
        objects: ObjectLimits::NONE,
        threads: 0,
    }
}

/// What a refusal must leave as it was: the root's balance, the heap in use, the free frames,
/// the next process ID and the kernel stacks held.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Snapshot {
    left: u64,
    heap: u64,
    frames: u64,
    next_id: u64,
    stacks: usize,
}

fn snapshot() -> Option<Snapshot> {
    let (heap, frames) = memory::stats()?;
    Some(Snapshot {
        left: ROOT.left(),
        heap: heap.bytes_in_use,
        frames,
        next_id: process::next_id(),
        stacks: origin_frame::stack::counts().live,
    })
}

fn verdict(ok: bool) -> &'static str {
    if ok { "" } else { ": kernel bound failed" }
}

/// Waits until the root holds its size again, for up to two seconds: an earlier proof's last
/// threads may still be being reaped on their processors.
fn back_whole(tsc_hz: u64) -> bool {
    let size = ROOT.size();
    sched::yield_until(|| ROOT.left() == size, tsc_hz, 2);
    size > 0 && ROOT.left() == size
}

/// Runs the proof; `true` if every clause held.
pub(crate) fn prove_kernel_bound(
    com1: &mut Console,
    _cpus: &[CpuIndex],
    bsp: CpuIndex,
    tsc_hz: u64,
) -> bool {
    let size = ROOT.size();
    // The busiest moment of every proof before this one, before this proof's holder takes the
    // root for itself.
    let busiest = size.saturating_sub(ROOT.lowest());
    let Some(costs) = process::costs() else {
        let _ = writeln!(
            com1,
            "origin-kernel: kernel bound: the build's costs could not be measured: kernel bound failed"
        );
        return false;
    };
    let _ = writeln!(
        com1,
        "origin-kernel: kernel bound: costs as this build measures them, in bytes: a process {}; a handle table of 64 entries {}; a channel {}; a memory object {}, and each page of one {} besides its frame; a space's page records once it may map {}; an event {}; a port {}; a binding {}; a thread {}",
        costs.process,
        Limits {
            handles: 64,
            ..holder(0)
        }
        .figure(&costs)
        .map_or(0, |f| f - costs.process),
        costs.channel,
        costs.memory_object,
        costs.memory_page,
        Limits {
            memory: MemoryLimits {
                mappings: 1,
                ..MemoryLimits::NONE
            },
            ..holder(0)
        }
        .figure(&costs)
        .map_or(0, |f| f - costs.process),
        costs.event,
        costs.port,
        costs.binding,
        costs.thread,
    );

    // Back before: every earlier proof's processes are gone, and their payments with them.
    let before = back_whole(tsc_hz);
    let once = process::size_root(u64::MAX).is_none() && ROOT.size() == size;
    let _ = writeln!(
        com1,
        "origin-kernel: kernel bound: after every earlier proof the root is back at its size, {} KiB: {before}; it cannot be sized again: {once}{}",
        size >> 10,
        verdict(before && once)
    );

    let figure = FIRST_PROGRAM_RUN.limits.figure(&costs).unwrap_or(u64::MAX);
    let launch = launch::launch_payment(&FIRST_PROGRAM, &FIRST_PROGRAM_RUN).unwrap_or(u64::MAX);
    let (refused, made) = match Process::proof(CREATOR, (0, 0), BOOT_PROOF_PRINCIPAL) {
        Some(creator) => {
            let refused = refusal(com1, &creator, launch, costs.process);
            let made = paid_to_the_byte(com1, &creator, figure, launch, costs.page, bsp, tsc_hz);
            drop(creator);
            (refused, made)
        }
        None => {
            let _ = writeln!(
                com1,
                "origin-kernel: kernel bound: the launches' creator could not be made: kernel bound failed"
            );
            (false, false)
        }
    };
    let waits = refund_waits(com1, tsc_hz);
    let counted = threads_counted(com1, bsp);

    // Back after: the proof's own processes are gone.
    let after = back_whole(tsc_hz);
    let _ = writeln!(
        com1,
        "origin-kernel: kernel bound: after this proof the root is back at its size: {after}; the most of it the proofs before this one had paid out at once was {} KiB of {} KiB, a figure{}",
        busiest >> 10,
        size >> 10,
        verdict(after)
    );
    before && once && refused && made && waits && counted && after
}

/// The refusal: a holder takes all of the root but `launch - 1` bytes, and a launch that pays
/// `launch` is refused with nothing made.
fn refusal(com1: &mut Console, creator: &Process, launch: u64, process_cost: u64) -> bool {
    let left = ROOT.left();
    // What the holder must hold beside itself to leave one byte less than the launch pays.
    let Some(bytes) = left
        .checked_sub(process_cost)
        .and_then(|n| n.checked_sub(launch))
        .and_then(|n| n.checked_add(1))
        .and_then(|n| usize::try_from(n).ok())
    else {
        let _ = writeln!(
            com1,
            "origin-kernel: kernel bound: {left} bytes left cannot hold a holder and a launch of {launch}: kernel bound failed"
        );
        return false;
    };
    let Some(held) = Process::proof(holder(bytes), (0, 0), BOOT_PROOF_PRINCIPAL) else {
        let _ = writeln!(
            com1,
            "origin-kernel: kernel bound: the holder could not be made: kernel bound failed"
        );
        return false;
    };
    let short = ROOT.left() == launch - 1;
    let before = snapshot();
    let refused = launch::load_program(creator, &FIRST_PROGRAM, &FIRST_PROGRAM_RUN).err()
        == Some(Error::Limit);
    let nothing = before.is_some() && before == snapshot();
    let _ = writeln!(
        com1,
        "origin-kernel: kernel bound: with all of the root but {} bytes held by a process that never runs, one byte short of what a launch of the first program pays, {launch} bytes: {short}; the launch is refused with LIMIT: {refused}; nothing was made, the root, the heap in use, the free frames, the next process ID and the kernel stacks as they were: {nothing}{}",
        ROOT.left(),
        verdict(short && refused && nothing)
    );
    drop(held);
    short && refused && nothing
}

/// Paid to the byte: with the holder gone, a process with the program's limits takes exactly their
/// `figure`; and the program, launched and run to its end where the generator can place it, holds
/// its figure and its launch's pages while it runs, `launch` until its record is written and then
/// what its pages cost, and gives everything back.
fn paid_to_the_byte(
    com1: &mut Console,
    creator: &Process,
    figure: u64,
    launch: u64,
    page: u64,
    bsp: CpuIndex,
    tsc_hz: u64,
) -> bool {
    let left = ROOT.left();
    let whole = left >= launch;
    let took = Process::proof(FIRST_PROGRAM_RUN.limits, (0, 0), BOOT_PROOF_PRINCIPAL).is_some_and(
        |made| {
            let paid = ROOT.left() == left - figure;
            drop(made);
            paid && ROOT.left() == left
        },
    );
    let mut pages = (0, 0);
    let ran = if origin_frame::random::available() {
        launch::load_program(creator, &FIRST_PROGRAM, &FIRST_PROGRAM_RUN)
            .ok()
            .is_some_and(|loaded| {
                let ahead = ROOT.left() == left - launch;
                let given = Given {
                    bootstrap: None,
                    directory: None,
                    args: &[b"run"],
                    env: &[],
                };
                launch::start_program(loaded, given, bsp)
                    .ok()
                    .is_some_and(|launched| {
                        let process = launched.process;
                        pages = process.account.launch_paid();
                        let (bytes, mapped) = pages;
                        // After the record: the figure, and its private pages at `page` each and
                        // `origin.core`'s at their share of the records, no more.
                        let settled = bytes <= mapped * page
                            && ROOT.left() == left - figure - bytes
                            && process.account.figure() == figure;
                        sched::yield_until(|| process.ending().is_some(), tsc_hz, 10);
                        let ended = process.ending().is_some();
                        let still = ROOT.left() == left - figure - bytes;
                        drop(process);
                        sched::yield_until(|| ROOT.left() == left, tsc_hz, 2);
                        ahead && settled && ended && still && ROOT.left() == left
                    })
            })
    } else {
        true
    };
    let ran_text = if !origin_frame::random::available() {
        "not run, no generator"
    } else if ran {
        "true"
    } else {
        "false"
    };
    let _ = writeln!(
        com1,
        "origin-kernel: kernel bound: with the holder gone the root has room again: {whole}; a process with the program's limits takes their figure, {figure} bytes, to the byte and gives it back when dropped: {took}; the program launched and run to its end pays {launch} bytes ahead, then holds its figure and its {} pages' {} bytes until it is gone, then gives everything back: {ran_text}{}",
        pages.1,
        pages.0,
        verdict(whole && took && ran)
    );
    whole && took && ran
}

/// The refund waits: a process whose account a channel is charged to is dropped first, and the
/// root has its payment back only once the channel goes too (ruling (c)).
fn refund_waits(com1: &mut Console, tsc_hz: u64) -> bool {
    let left = ROOT.left();
    let limits = Limits {
        channels: 1,
        ..holder(0)
    };
    let Some(process) = Process::proof(limits, (0, 0), BOOT_PROOF_PRINCIPAL) else {
        let _ = writeln!(
            com1,
            "origin-kernel: kernel bound: the channel's creator could not be made: kernel bound failed"
        );
        return false;
    };
    let paid = left - ROOT.left();
    let Ok((one, two)) = ipc::create_channel(&process.account) else {
        let _ = writeln!(
            com1,
            "origin-kernel: kernel bound: the channel could not be made: kernel bound failed"
        );
        return false;
    };
    drop(process);
    ipc::reap_closed();
    let held = ROOT.left() == left - paid;
    drop(one);
    drop(two);
    ipc::reap_closed();
    sched::yield_until(|| ROOT.left() == left, tsc_hz, 2);
    let back = ROOT.left() == left;
    let _ = writeln!(
        com1,
        "origin-kernel: kernel bound: a process whose channel outlives it keeps its {paid} bytes out of the root after the process object is dropped: {held}; they come back when the channel goes: {back}{}",
        verdict(held && back)
    );
    held && back
}

/// Threads counted: a start in a process whose limits give no threads is refused before it takes
/// a kernel stack, an FPU area or anything else.
fn threads_counted(com1: &mut Console, bsp: CpuIndex) -> bool {
    let made = Process::proof(holder(0), (0, 0), BOOT_PROOF_PRINCIPAL)
        .and_then(|process| ObjectRef::try_new(process).ok());
    let Some(process) = made else {
        let _ = writeln!(
            com1,
            "origin-kernel: kernel bound: the process given no threads could not be made: kernel bound failed"
        );
        return false;
    };
    let before = snapshot();
    let refused = Process::start_thread(&process, bsp, 0x1000, 0x2000, 0).is_err();
    let nothing = before.is_some() && before == snapshot();
    drop(process);
    let _ = writeln!(
        com1,
        "origin-kernel: kernel bound: a process given no threads cannot start one: {refused}; the refusal took nothing: {nothing}{}",
        verdict(refused && nothing)
    );
    refused && nothing
}
