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
//! - **threads counted**: a process given no threads cannot start one, and one given one thread
//!   cannot start a second while the first waits; each refusal leaves nothing (G5's "a kernel
//!   stack and an FPU area for each thread" is paid for by that count);
//! - **what a mapping held goes with the space**: a memory object made by Q and mapped in P keeps
//!   Q's payment out after Q is dropped and P's last thread has ended, until P's space has gone
//!   at P's last reap; then it comes back, before P's `TERMINATED` (the independent review's
//!   finding B2);
//! - **a port's bound** (ADR-0005 Amendment 10, I1; TASKS item 57): a port whose creator was given
//!   `bindings_per_port: 2`, bound by another process, takes two bindings armed and refuses a
//!   third with `limit`, leaving nothing; once one fires it takes another; a port whose creator was
//!   given none refuses the first;
//! - **back after**: when the proof's processes are gone the root holds its size again; and the
//!   most of it the proofs before this one paid out at once is printed, a figure.
//!
//! Each clause that compares the kernel's global counts first settles the machine ([`settle`]):
//! every exited thread reaped and the retired kernel stacks reclaimed, so that no other processor
//! frees anything between its two looks (the review's finding B10). Its negative controls are in
//! ADR-0013 (KB-1 to KB-10).
#![forbid(unsafe_code)]

use core::fmt::Write;

use origin_abi::Error;

use origin_abi::limits::DEADLINE_INFINITE;
use origin_abi::{OK, map_access, map_flags, rights, signals};
use origin_frame::proof_programs::{self, SCRIPT, STACK};

use super::Arg::Imm;
use super::{CODE, Image, LIMITS, wait};
use crate::console::Console;
use crate::cpu::{self, CpuIndex};
use crate::ipc::{self, Object};
use crate::launch::{self, Given};
use crate::mapping::Request;
use crate::memory_object::{self, MemoryObject};
use crate::paging::PAGE;
use crate::process::Ending;
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
    retired: usize,
}

fn snapshot() -> Option<Snapshot> {
    let (heap, frames) = memory::stats()?;
    Some(Snapshot {
        left: ROOT.left(),
        heap: heap.bytes_in_use,
        frames,
        next_id: process::next_id(),
        stacks: origin_frame::stack::counts().live,
        retired: origin_frame::stack::counts().retired,
    })
}

/// Settles the machine before a clause compares global counts (the independent review of task
/// S4-3, finding B10): waits until every exited thread has been reaped on its own processor,
/// then reclaims every retired kernel stack from here, which waits for every processor. After
/// that no other processor's reap has anything to free while the clause looks. `false` if a
/// thread was still unreaped after two seconds, or a stack is still retired.
fn settle(tsc_hz: u64) -> bool {
    sched::yield_until(|| sched::zombies() == 0, tsc_hz, 2);
    let reaped = sched::zombies() == 0;
    origin_frame::stack::reclaim();
    reaped && origin_frame::stack::counts().retired == 0
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
        "origin-kernel: kernel bound: costs as this build measures them, in bytes: a process {}; a handle table of 64 entries {}; a channel {}; a memory object {}, and each page of one {} besides its frame; a space's page records once it may map {}; an event {}; a port {}; a binding {}; a thread {}, and the most one system call of it holds that no quota charges {}",
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
        costs.call,
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
            let refused = refusal(com1, &creator, launch, costs.process, tsc_hz);
            let made = paid_to_the_byte(com1, &creator, figure, launch, &costs, bsp, tsc_hz);
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
    let counted = threads_counted(com1, bsp, tsc_hz) && one_thread_at_a_time(com1, bsp, tsc_hz);
    let with_space = objects_go_with_the_space(com1, bsp, tsc_hz);
    let port_bound = ports_bounded_by_their_creators(com1, tsc_hz);

    // Back after: the proof's own processes are gone.
    let after = back_whole(tsc_hz);
    let _ = writeln!(
        com1,
        "origin-kernel: kernel bound: after this proof the root is back at its size: {after}; the most of it the proofs before this one had paid out at once was {} KiB of {} KiB, a figure{}",
        busiest >> 10,
        size >> 10,
        verdict(after)
    );
    before && once && refused && made && waits && counted && with_space && port_bound && after
}

/// The refusal: a holder takes all of the root but `launch - 1` bytes, and a launch that pays
/// `launch` is refused with nothing made.
fn refusal(
    com1: &mut Console,
    creator: &Process,
    launch: u64,
    process_cost: u64,
    tsc_hz: u64,
) -> bool {
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
    let settled = settle(tsc_hz);
    let before = snapshot();
    let refused = launch::load_program(creator, &FIRST_PROGRAM, &FIRST_PROGRAM_RUN).err()
        == Some(Error::Limit);
    let nothing = settled && before.is_some() && before == snapshot();
    let _ = writeln!(
        com1,
        "origin-kernel: kernel bound: with all of the root but {} bytes held by a process that never runs, one byte short of what a launch of the first program pays, {launch} bytes: {short}; the launch is refused with LIMIT: {refused}; nothing is left of it, the root, the heap in use, the free frames, the next process ID and the kernel stacks as they were: {nothing}{}",
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
    costs: &crate::quota::Costs,
    bsp: CpuIndex,
    tsc_hz: u64,
) -> bool {
    // What this launch maps, page by page, found apart from what it paid (the review's finding
    // B3): the program's segments, its stack and its start record on frames of its own, and
    // `origin.core`'s pages on frames every process shares.
    let args: [&[u8]; 1] = [b"run"];
    let record = origin_abi::start::size_of(2, &args, &[])
        .map_or(u64::MAX, |bytes| (bytes as u64).div_ceil(PAGE));
    let image: u64 = FIRST_PROGRAM.segments.iter().map(|s| s.size / PAGE).sum();
    let own = image + FIRST_PROGRAM_RUN.stack_pages + record;
    let core = crate::core_image::image().span / PAGE;
    let want = (own + core, own * costs.page + core * costs.shared_page);
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
                    args: &args,
                    env: &[],
                };
                launch::start_program(loaded, given, bsp)
                    .ok()
                    .is_some_and(|launched| {
                        let process = launched.process;
                        pages = process.account.launch_paid();
                        let (bytes, mapped) = pages;
                        // After the record: the figure, and each page the launch mapped, counted
                        // apart, at its cost: no page left unpaid, and none paid twice.
                        let settled = (mapped, bytes) == want
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
        "origin-kernel: kernel bound: with the holder gone the root has room again: {whole}; a process with the program's limits takes their figure, {figure} bytes, to the byte and gives it back when dropped: {took}; the program launched and run to its end pays {launch} bytes ahead, then holds its figure and its {} pages' {} bytes, each page it maps paid for, until it is gone, then gives everything back: {ran_text}{}",
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
fn threads_counted(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let made = Process::proof(holder(0), (0, 0), BOOT_PROOF_PRINCIPAL)
        .and_then(|process| ObjectRef::try_new(process).ok());
    let Some(process) = made else {
        let _ = writeln!(
            com1,
            "origin-kernel: kernel bound: the process given no threads could not be made: kernel bound failed"
        );
        return false;
    };
    let settled = settle(tsc_hz);
    let before = snapshot();
    let refused = Process::start_thread(&process, bsp, 0x1000, 0x2000, 0).is_err();
    let nothing = settled && before.is_some() && before == snapshot();
    drop(process);
    let _ = writeln!(
        com1,
        "origin-kernel: kernel bound: a process given no threads cannot start one: {refused}; nothing is left of the refusal: {nothing}{}",
        verdict(refused && nothing)
    );
    refused && nothing
}

/// One thread at a time (the review's finding B4): a process given one thread starts it, and
/// while that thread waits, unreaped, a second start is refused and leaves nothing; the process
/// ends when the kernel lets the first go.
fn one_thread_at_a_time(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let limits = Limits {
        threads: 1,
        ..LIMITS
    };
    let built = Image::new(limits, proof_programs::script()).and_then(|mut image| {
        let (kept, end) = ipc::create_channel(&image.process.account).ok()?;
        let held = image
            .process
            .grant(Object::Channel(end), rights::CHANNEL)
            .ok()?;
        // The thread stays until the kernel drops the far end.
        let stays = alloc::vec![wait(
            "",
            Imm(held),
            signals::PEER_CLOSED,
            DEADLINE_INFINITE,
            OK
        )];
        image.scratch()?;
        image.script(&stays)?;
        Some((image.finish()?, kept))
    });
    let Some((process, kept)) = built else {
        let _ = writeln!(
            com1,
            "origin-kernel: kernel bound: the process given one thread could not be made: kernel bound failed"
        );
        return false;
    };
    let first = Process::start_thread(&process, bsp, CODE, STACK + PAGE, SCRIPT).is_ok();
    sched::yield_until(
        || process.waiting.load(core::sync::atomic::Ordering::Acquire) > 0,
        tsc_hz,
        5,
    );
    let waits = process.waiting.load(core::sync::atomic::Ordering::Acquire) == 1;
    let settled = settle(tsc_hz);
    let before = snapshot();
    let second = Process::start_thread(&process, bsp, CODE, STACK + PAGE, SCRIPT).is_err();
    let nothing = settled && before.is_some() && before == snapshot();
    drop(kept);
    sched::yield_until(|| process.termination().raised(), tsc_hz, 5);
    let ended = process.ending() == Some(Ending::Exited(0));
    drop(process);
    let ok = first && waits && second && nothing && ended;
    let _ = writeln!(
        com1,
        "origin-kernel: kernel bound: a process given one thread starts it: {}; while it waits a second start is refused: {second}; nothing is left of the refusal: {nothing}; the first ends when let go: {ended}{}",
        first && waits,
        verdict(ok)
    );
    ok
}

/// What a mapping held goes with the space (the review's finding B2): Q makes a memory object and
/// P maps it; Q is dropped, so the object holds Q's account last. P's one thread ends on this
/// processor, which reaps it only when this proof asks: until then P has ended but its space,
/// which maps the object's frames, has not gone, and Q's payment must still be out. Once P's
/// last reap frees its space, Q's payment comes back, before P's `TERMINATED`.
fn objects_go_with_the_space(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    use origin_frame::userspace;
    const PAGES: u64 = 4;
    // Far from the image's code, data and stack, and from every other proof's places.
    const AT: u64 = 0x0000_0020_0000_0000;
    let left = ROOT.left();
    let q_limits = Limits {
        memory: MemoryLimits {
            committed: (PAGES * PAGE) as usize,
            ..MemoryLimits::NONE
        },
        ..holder(0)
    };
    let p_limits = Limits {
        memory: MemoryLimits {
            mappings: 1,
            ..MemoryLimits::NONE
        },
        threads: 1,
        ..LIMITS
    };
    let built = Process::proof(q_limits, (0, 0), BOOT_PROOF_PRINCIPAL).and_then(|q| {
        let q_paid = left - ROOT.left();
        let object = MemoryObject::create(&q.account, PAGES * PAGE).ok()?;
        let object = ObjectRef::try_new(object).ok()?;
        let mut image = Image::new(p_limits, proof_programs::script())?;
        image.scratch()?;
        image.script(&[])?;
        let request = Request {
            access: map_access::READ | map_access::WRITE,
            flags: map_flags::FIXED,
            offset: 0,
            length: PAGES * PAGE,
            address: AT,
        };
        let told = |_: u64| Ok(());
        memory_object::map(
            &image.process,
            &image.process,
            &object,
            rights::MEMORY_OBJECT,
            &request,
            &told,
        )
        .ok()?;
        // The object is held by P's mapping alone, and Q's account by the object alone.
        drop(object);
        drop(q);
        Some((image.finish()?, q_paid))
    });
    let Some((process, q_paid)) = built else {
        let _ = writeln!(
            com1,
            "origin-kernel: kernel bound: the object's creator and the process that maps it could not be made: kernel bound failed"
        );
        return false;
    };
    let both = ROOT.left();
    let freed = userspace::spaces_freed();
    let started = Process::start_thread(&process, bsp, CODE, STACK + PAGE, SCRIPT).is_ok();
    // Yield to P's thread without reaping it: `sched::yield_until` would reap.
    let start = cpu::rdtsc();
    while process.ending().is_none() && cpu::rdtsc().wrapping_sub(start) < tsc_hz * 5 {
        sched::yield_now();
        cpu::interrupt_window();
    }
    let ended = process.ending() == Some(Ending::Exited(0));
    let space_kept = userspace::spaces_freed() == freed && !process.termination().raised();
    let still_out = ROOT.left() == both;
    sched::reap();
    sched::yield_until(|| process.termination().raised(), tsc_hz, 5);
    let space_gone = userspace::spaces_freed() != freed;
    let back = ROOT.left() == both + q_paid;
    drop(process);
    let ok = started && ended && space_kept && still_out && space_gone && back;
    let _ = writeln!(
        com1,
        "origin-kernel: kernel bound: a memory object another process made, mapped in one whose last thread has ended and is not yet reaped, keeps its maker's {q_paid} bytes out while the space that maps it stands: {}; they come back when the last reap frees that space, before TERMINATED: {}{}",
        started && ended && space_kept && still_out,
        space_gone && back,
        verdict(ok)
    );
    ok
}

/// A port's bound (ADR-0005 Amendment 10, I1): a port is bounded at its creator's
/// `bindings_per_port`, counting bindings armed and reservations in flight, and its creator's
/// figure pays for its index at that bound. Another process binds events to two ports, one whose
/// creator was given two and one whose creator was given none.
fn ports_bounded_by_their_creators(com1: &mut Console, tsc_hz: u64) -> bool {
    use crate::event::Event;
    use crate::handles::Need;
    use crate::port;
    let creator = |bindings_per_port| Limits {
        objects: ObjectLimits {
            events: 0,
            ports: 1,
            bindings: 0,
            bindings_per_port,
        },
        ..holder(0)
    };
    let binder_limits = Limits {
        handles: 8,
        objects: ObjectLimits {
            events: 3,
            ports: 0,
            bindings: 4,
            bindings_per_port: 0,
        },
        ..holder(0)
    };
    let made = (|| {
        let two = Process::proof(creator(2), (0, 0), BOOT_PROOF_PRINCIPAL)?;
        let none = Process::proof(creator(0), (0, 0), BOOT_PROOF_PRINCIPAL)?;
        let binder = Process::proof(binder_limits, (0, 0), BOOT_PROOF_PRINCIPAL)?;
        let bounded = port::create(&two.account).ok()?;
        let empty = port::create(&none.account).ok()?;
        let watch = bounded.clone();
        let at_two = binder.grant(Object::Port(bounded), rights::PORT).ok()?;
        let at_none = binder.grant(Object::Port(empty), rights::PORT).ok()?;
        let mut events = alloc::vec::Vec::new();
        let mut handles = alloc::vec::Vec::new();
        for _ in 0..3 {
            let event = Event::create(&binder.account).ok()?;
            handles.push(
                binder
                    .grant(Object::Event(event.clone()), rights::EVENT)
                    .ok()?,
            );
            events.push(event);
        }
        Some((two, none, binder, watch, at_two, at_none, events, handles))
    })();
    let Some((two, none, binder, watch, at_two, at_none, events, handles)) = made else {
        let _ = writeln!(
            com1,
            "origin-kernel: kernel bound: the bounded ports and their binder could not be made: kernel bound failed"
        );
        return false;
    };
    let bind = |port: u64, event: u64, key: u64| {
        port::bind(
            &binder.handles,
            &binder.account,
            binder.id(),
            (port, Need::NOTHING),
            (event, Need::NOTHING),
            key,
            signals::SIGNALED,
        )
    };
    let event = |k: usize| handles.get(k).copied().unwrap_or(0);
    let two_armed = bind(at_two, event(0), 1).is_ok() && bind(at_two, event(1), 2).is_ok();
    let armed_two = watch.armed() == 2;
    let settled = settle(tsc_hz);
    let held = binder.account.objects().bindings.held();
    let before = snapshot();
    let third = bind(at_two, event(2), 3) == Err(Error::Limit);
    let nothing = settled
        && before.is_some()
        && before == snapshot()
        && binder.account.objects().bindings.held() == held
        && watch.armed() == 2;
    // One binding fires: its slot leaves the index, and the port takes another.
    let fired = events
        .first()
        .is_some_and(|e| e.signal(0, signals::SIGNALED).is_ok());
    let again = fired && watch.armed() == 1 && bind(at_two, event(2), 4).is_ok();
    // A port whose creator was given none refuses the first.
    let zero = bind(at_none, event(2), 5) == Err(Error::Limit);
    // The binder never ran: its table closes as a launch that failed closes it, its bindings
    // cancelled and their charges given back.
    binder.abandon();
    drop((two, none, binder, watch, events));
    let ok = two_armed && armed_two && third && nothing && again && zero;
    let _ = writeln!(
        com1,
        "origin-kernel: kernel bound: a port whose creator was given two bindings at once takes two from another process: {}; a third is refused with LIMIT: {third}; nothing is left of it, the binder's charge and the port's armed count with the rest: {nothing}; once one fires it takes another: {again}; a port whose creator was given none refuses the first: {zero}{}",
        two_armed && armed_two,
        verdict(ok)
    );
    ok
}
