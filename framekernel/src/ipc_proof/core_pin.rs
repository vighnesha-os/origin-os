//! The boot proofs "origin.core and the call pin" and "the flags at a thread's entry" (task S4-2;
//! ADR-0005 B2 and Amendments 8 and 9, G1 and H1; ADR-0011 Amendments 2 and 3, L2, L3, L6 to L8
//! and L13; ADR-0013's rows of those names).
//!
//! Both run the second program the kernel carries, `tests/kernel/pin-program`: a script
//! interpreter in Rust, launched pinned per call like any program that is not one of the
//! kernel's assembly proof programs. It runs the scripts those proofs already write, and makes
//! each call through the import of its number, which the launch path bound to `origin.core`.
//!
//! "origin.core and the call pin" shows, each clause printed and judged:
//!
//! - **the binding**: before a program's thread starts, every one of its import slots holds the
//!   address of its export in that process's `origin.core`, on a page it cannot write;
//! - **every call**: a script that makes each of `origin.kernel`'s calls through `origin.core`
//!   gets the status each must give;
//! - **the hostile calls**: the hostile scripts of "IPC and handles", the IPC one and the memory
//!   calls' one, run again through the stubs, give call for call the statuses they give when
//!   the assembly interpreter makes them directly;
//! - **the pin**: a `syscall` instruction in the program's own code, a jump to one call's
//!   `syscall` with another call's number in RAX, a number no stub has, and a call's own number
//!   with bit 32 set, each stop the thread, and its ending records the number and the address
//!   (G1): for the first, exactly the address the program reported for that instruction;
//! - **the image**: a write to a page of `origin.core` faults there; `memory_unmap` and
//!   `memory_protect` of its range, a fixed `memory_map` over it and one of two pages from a
//!   page below it answer `invalid_args`, and its pages are still mapped read-execute on the
//!   same frames afterwards; three processes hold it, each at a base of its own, on one set of
//!   frames, writable in no space; and the kernel's own `Process::write_page` refuses a page
//!   of it;
//! - **narrow parameters** (L13): through a stub, a call whose 32-bit parameter has bits set
//!   above it is taken as the same call with them clear, where the same registers in a direct
//!   call are `invalid_args`;
//! - **the proof processes** keep their range (L8): a call from outside it is refused with a
//!   status, and a number no method has answers `not_supported`;
//! - everything it made is gone afterwards, and `origin.core`'s frames still hold the image
//!   word for word.
//!
//! It also prints what one call through `origin.core` costs, which nothing judges.
//!
//! "The flags at a thread's entry" (H1): the program's own entry pushes RFLAGS as its first
//! instruction, and the proof finds the direction flag and the six status flags clear and the
//! interrupt flag set in what it reports, on the boot processor and on another where the
//! machine has one.

use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::Ordering;

use origin_abi::limits::DEADLINE_INFINITE;
use origin_abi::{Error, OK, map_access, map_flags, rights, signals, syscall as sys};
use origin_frame::objref::ObjectRef;
use origin_syscalls::origin_kernel::kernel::server;

use super::{
    Arg, CODE, DATA, DATA_PAGES, Hd, Image, Imm, LIMITS, MSG, OBS, RBUF, Scripted, Step, Via, Want,
    await_end, close, create, fault, first_wrong, h, hostile_memory_run, hostile_run, map_request,
    object_request, read, slot_addr, step, wait, write,
};
use crate::console::Console;
use crate::core_image;
use crate::cpu::CpuIndex;
use crate::ipc::{self, Object};
use crate::launch::{self, Given, LoadedProgram, ProgramLimits};
use crate::paging::{Access, PAGE, Translation};
use crate::process::{BOOT_PROOF_PRINCIPAL, Ending, Limits, MemoryLimits, ObjectLimits, Process};
use crate::program::{self, Carried, CoreAt, FIRST_PROGRAM, FIRST_PROGRAM_RUN, PIN_PROGRAM};
use crate::{memory, sched};

/// What marks a failed line of the first proof, and of the second.
const FAILED: &str = ": origin.core failed";
const FLAGS_FAILED: &str = ": entry flags failed";

/// Pages of the pin program's stack.
const STACK_PAGES: u64 = 8;
/// The handles a launched program's start record holds beyond what a script makes: its process
/// and its thread.
const START_HANDLES: usize = 2;

// The pin program's constants (`tests/kernel/pin-program/src/main.rs`), copied.
/// The exit code of `flags`.
const FLAGS_REPORTED: u64 = 0x0046_4C41_4753;
/// The pseudo-steps this proof adds to the interpreter's.
const PSEUDO_OWN_SYSCALL: u64 = -6i64 as u64;
const PSEUDO_JUMP: u64 = -7i64 as u64;
const PSEUDO_TIMED: u64 = -8i64 as u64;

/// The slot a script finds its own process handle in.
const SELF: u64 = 62;
/// Where a script's requests, its mapping's address, a packet and a clock reading go.
const REQ_OBJECT: u64 = DATA + 0x800;
const REQ_MAP: u64 = DATA + 0x840;
const REQ_FIXED: u64 = DATA + 0x880;
const MAPPED: u64 = DATA + 0x8C0;
/// A fixed map of two pages that starts one page below `origin.core`'s base.
const REQ_FIXED_BELOW: u64 = DATA + 0xA00;
const PACKET: u64 = DATA + 0x900;
const CLOCK: u64 = DATA + 0x980;

/// RFLAGS bits (Intel SDM Volume 1, the EFLAGS register; AMD APM Volume 1, rFLAGS): carry,
/// parity, auxiliary carry, zero, sign, interrupt enable, direction, overflow.
const CF: u64 = 1 << 0;
const PF: u64 = 1 << 2;
const AF: u64 = 1 << 4;
const ZF: u64 = 1 << 6;
const SF: u64 = 1 << 7;
const IF: u64 = 1 << 9;
const DF: u64 = 1 << 10;
const OF: u64 = 1 << 11;
/// The seven H1 says are clear at a thread's entry.
const CLEAR_AT_ENTRY: u64 = DF | CF | PF | AF | ZF | SF | OF;

/// The page fault's vector and its error code for a write to a present user page (Intel SDM
/// Volume 3A, §4.7): present, write, user.
const PAGE_FAULT: u64 = 14;
const WRITE_TO_PRESENT_USER_PAGE: u64 = 0b111;
const FAULT_BITS: u64 = 0b1_1111;

/// What the creator of these processes may hold: nothing of its own is needed.
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

/// Where the launch path put a pinned process's `origin.core`, program and stack.
#[derive(Clone, Copy)]
pub(super) struct Placed {
    core: u64,
    base: u64,
    entry: u64,
    stack: u64,
    stack_top: u64,
}

/// The pages the hostile memory script maps and unmaps at a fixed address, with a page of room
/// on either side: `[HOSTILE_FIXED - PAGE, HOSTILE_FIXED + 3 * PAGE)`.
const HOSTILE_PAGES: (u64, u64) = (super::HOSTILE_FIXED - PAGE, super::HOSTILE_FIXED + 3 * PAGE);

/// Whether a launch placed `origin.core`, the program or the stack, with its two unmapped
/// neighbours, on [`HOSTILE_PAGES`]. All three are random pages of the programs' region, where
/// that fixed address lies too.
fn on_hostile_pages(loaded: &LoadedProgram, program: &Carried) -> bool {
    let (low, high) = HOSTILE_PAGES;
    let overlaps = |start: u64, end: u64| start < high && low < end;
    overlaps(loaded.core, loaded.core + core_image::image().span)
        || overlaps(loaded.base, loaded.base + program.span)
        || overlaps(loaded.stack - PAGE, loaded.stack_top + PAGE)
}

/// Makes a process for the Rust interpreter under `limits`, with two handles more for its
/// start record's own (see `Scripted::new`): the pin program loaded and bound by the launch
/// path, and the scratch memory every scripted process has at [`DATA`]. A page is mapped at
/// [`CODE`] too, read-only: the scripts name that address as memory a call may not write into,
/// which for the assembly interpreter is its own code.
///
/// A launch whose random places fell on the hostile memory script's fixed pages is loaded once
/// more, and the first is dropped: that script expects those pages free, and a correct kernel
/// would otherwise fail its run through the stubs about once in 10^8 boots (the independent
/// review of 2026-11-13, finding K7). Two such falls in a row, about 3 in 10^17, still fail.
pub(super) fn scripted(creator: &Process, limits: Limits) -> Option<Scripted> {
    let run = ProgramLimits {
        limits: Limits {
            handles: limits.handles + START_HANDLES,
            // A program runs the one thread its launch starts (ADR-0005 G2, G6).
            threads: 1,
            ..limits
        },
        stack_pages: STACK_PAGES,
    };
    let mut loaded = launch::load_program(creator, &PIN_PROGRAM, &run).ok()?;
    if on_hostile_pages(&loaded, &PIN_PROGRAM) {
        loaded = launch::load_program(creator, &PIN_PROGRAM, &run).ok()?;
    }
    let LoadedProgram {
        process,
        core,
        base,
        entry,
        stack,
        stack_top,
    } = loaded;
    let mut image = Image {
        process,
        pages: Vec::new(),
    };
    image.map(DATA, DATA_PAGES, Access::ReadWrite)?;
    image.map(CODE, 1, Access::Read)?;
    Some(Scripted {
        image,
        pinned: Some(Placed {
            core,
            base,
            entry,
            stack,
            stack_top,
        }),
    })
}

/// Starts the pinned process `image` on `cpu` to run the script written into it. With
/// `own_slot`, the program stores its own process handle at that address first.
pub(super) fn start(
    image: Image,
    placed: Placed,
    cpu: CpuIndex,
    own_slot: Option<u64>,
) -> Option<ObjectRef<Process>> {
    let slot = own_slot.map(u64::to_le_bytes);
    let mut args: Vec<&[u8]> = Vec::new();
    args.push(b"script");
    if let Some(slot) = &slot {
        args.push(slot);
    }
    let loaded = LoadedProgram {
        process: image.process,
        core: placed.core,
        base: placed.base,
        entry: placed.entry,
        stack: placed.stack,
        stack_top: placed.stack_top,
    };
    let given = Given {
        bootstrap: None,
        directory: None,
        args: &args,
        env: &[],
    };
    launch::start_program(loaded, given, cpu)
        .ok()
        .map(|launched| launched.process)
}

/// What one scripted run through `origin.core` showed.
struct Ran {
    /// Where `origin.core` and the program were in it.
    core: u64,
    base: u64,
    reports: Vec<u64>,
    ending: Option<Ending>,
}

/// Launches the Rust interpreter from `creator` under `limits`, has `build` write whatever its
/// script needs and give the script, runs it on `cpu` and waits for it to end.
fn run(
    creator: &Process,
    limits: Limits,
    cpu: CpuIndex,
    tsc_hz: u64,
    build: impl FnOnce(&mut Scripted, &Placed) -> Option<Vec<Step>>,
) -> Option<Ran> {
    let mut scripted = Scripted::new(limits, Via::Core(creator))?;
    let placed = scripted.pinned?;
    let (core, base) = (placed.core, placed.base);
    let steps = build(&mut scripted, &placed)?;
    scripted.image.scratch()?;
    scripted.image.script(&steps)?;
    let process = scripted.start(cpu, Some(SELF))?;
    await_end(&process, tsc_hz, 10);
    Some(Ran {
        core,
        base,
        reports: process.reports(),
        ending: process.ending(),
    })
}

/// A step that reports nothing the proof compares by position: the interpreter's own.
const fn pseudo(number: u64, args: [Arg; 6]) -> Step {
    Step {
        name: "",
        number,
        args,
        want: Want::Nothing,
    }
}

/// The limits of a process that makes every call once: a channel, an event, a port and two
/// bindings, one page of memory and one mapping, with room to change it.
fn every_call_limits() -> Limits {
    Limits {
        handles: 12,
        channels: 1,
        memory: MemoryLimits {
            committed: PAGE as usize,
            mappings: 1,
            changes: crate::mapping::Rate {
                burst: 64,
                per_second: 64,
            },
        },
        objects: ObjectLimits {
            events: 1,
            ports: 1,
            bindings: 2,
            // One place on its one port at a time (ADR-0005 I1): its first bind queues at once,
            // and it reads that packet before it binds again and cancels (the independent review
            // of TASKS 57, finding T10).
            bindings_per_port: 1,
        },
        ..LIMITS
    }
}

/// A script that makes every call of `origin.kernel` once or more, each with arguments that
/// must succeed. `thread_exit` is the script's end, and `debug_report` carries every status.
fn every_call() -> Vec<Step> {
    let sig = u64::from(signals::SIGNALED);
    let (event, object, port) = (h(3), h(4), h(5));
    alloc::vec![
        create(0),
        step(
            "handle_duplicate",
            sys::HANDLE_DUPLICATE,
            &[h(0), Imm(rights::CHANNEL.into()), Imm(slot_addr(2))],
            OK
        ),
        write("channel_write", h(0), MSG, 8, OK),
        wait("wait_one", h(1), signals::READABLE, 0, OK),
        read("channel_read", h(1), RBUF, 16, OK),
        close("handle_close", h(2), OK),
        step("debug_write", sys::DEBUG_WRITE, &[Imm(MSG), Imm(16)], OK),
        step("event_create", sys::EVENT_CREATE, &[Imm(slot_addr(3))], OK),
        step(
            "object_signal",
            sys::OBJECT_SIGNAL,
            &[event, Imm(0), Imm(sig)],
            OK
        ),
        step("port_create", sys::PORT_CREATE, &[Imm(slot_addr(5))], OK),
        step(
            "object_wait_async",
            sys::OBJECT_WAIT_ASYNC,
            &[port, event, Imm(7), Imm(sig)],
            OK
        ),
        step(
            "port_wait",
            sys::PORT_WAIT,
            &[port, Imm(0), Imm(PACKET)],
            OK
        ),
        step("", sys::OBJECT_SIGNAL, &[event, Imm(sig), Imm(0)], OK),
        step(
            "",
            sys::OBJECT_WAIT_ASYNC,
            &[port, event, Imm(9), Imm(sig)],
            OK
        ),
        step("port_cancel", sys::PORT_CANCEL, &[port, event, Imm(9)], OK),
        step(
            "memory_object_create",
            sys::MEMORY_OBJECT_CREATE,
            &[Imm(REQ_OBJECT), Imm(slot_addr(4))],
            OK
        ),
        step(
            "memory_map",
            sys::MEMORY_MAP,
            &[h(SELF), object, Imm(REQ_MAP), Imm(MAPPED)],
            OK
        ),
        step(
            "memory_protect",
            sys::MEMORY_PROTECT,
            &[h(SELF), Hd(MAPPED), Imm(PAGE), Imm(map_access::READ.into())],
            OK
        ),
        step(
            "memory_unmap",
            sys::MEMORY_UNMAP,
            &[h(SELF), Hd(MAPPED), Imm(PAGE)],
            OK
        ),
        step("clock_read", sys::CLOCK_READ, &[Imm(CLOCK)], OK),
    ]
}

/// Writes the two requests [`every_call`] and the image clause use: one page of object, and a
/// read-write map of all of it wherever the kernel chooses.
fn write_requests(image: &Image) -> Option<()> {
    image.write(REQ_OBJECT, &object_request(16, PAGE))?;
    image.write(
        REQ_MAP,
        &map_request(40, map_access::READ | map_access::WRITE, 0, 0, PAGE, 0),
    )
}

/// Whether every method of `origin.kernel` is among `steps`' calls, `thread_exit` and
/// `debug_report` apart, which every script makes by ending and by reporting.
fn makes_every_call(steps: &[Step]) -> bool {
    server::CALLS.iter().all(|call| {
        matches!(call.number, sys::THREAD_EXIT | sys::DEBUG_REPORT)
            || steps.iter().any(|s| s.number == call.number)
    })
}

/// Whether `ran`'s reports are exactly the statuses `steps` must give, and it ended by exit 0.
fn script_right(steps: &[Step], ran: &Ran) -> bool {
    super::statuses_right(steps, &ran.reports) && ran.ending == Some(Ending::Exited(0))
}

/// Whether `translation` is a page of `origin.core`'s: present to ring 3, never writable,
/// executable, and on `frame`.
fn core_page(translation: Option<Translation>, page: usize) -> bool {
    let frame = core_image::frame(page).map(|phys| phys.raw());
    translation.is_some_and(|t| {
        t.user && t.size == PAGE && !t.writable && t.executable && Some(t.phys) == frame
    })
}

/// Whether every page of `origin.core` is mapped at `core` in `process` as [`core_page`] says.
fn core_mapped(process: &Process, core: u64) -> bool {
    let pages = (core_image::image().span / PAGE) as usize;
    pages != 0 && (0..pages).all(|k| core_page(process.translate(core + k as u64 * PAGE), k))
}

/// Whether every import slot of `program`, at `base` in `process` beside an `origin.core` at
/// `core`, holds the address of its export there, on a page the program cannot write: read back
/// through the child's own tables.
fn imports_bound(process: &Process, program: &Carried, base: u64, core: u64) -> bool {
    let at = CoreAt {
        base: core,
        image: core_image::image(),
    };
    let Some(space) = process.memory() else {
        return false;
    };
    !program.imports.is_empty()
        && program.imports.iter().all(|import| {
            let slot = base + import.slot;
            space.read_u64(slot) == program::bound(import, at)
                && program::bound(import, at).is_some()
                && process.translate(slot).is_some_and(|t| !t.writable)
        })
}

/// A run that must be stopped by the pin at its last step: after one call that works, the
/// thread ends with [`Ending::Refused`] for `number` at an address `at` accepts, and nothing
/// of the forbidden call's own got through.
fn stopped(ran: Option<&Ran>, number: u64, at: impl FnOnce(&Ran, u64) -> bool) -> bool {
    ran.is_some_and(|ran| {
        ran.reports == [Error::BadHandle.raw() as u64]
            && matches!(
                ran.ending,
                Some(Ending::Refused { number: n, address }) if n == number && at(ran, address)
            )
    })
}

/// Runs the proof "origin.core and the call pin". Called from the thread proofs with randomness
/// available.
pub(crate) fn prove_core_pin(
    com1: &mut Console,
    cpus: &[CpuIndex],
    bsp: CpuIndex,
    tsc_hz: u64,
) -> bool {
    let before = memory::stats();
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let ok = {
        let Some(creator) = Process::proof(CREATOR, (0, 0), BOOT_PROOF_PRINCIPAL) else {
            return false;
        };
        // The binding first: it is judged on processes that never run, and everything after
        // it rests on it.
        let binding_ok = prove_binding(com1, &creator);
        let calls_ok = prove_calls(com1, &creator, bsp, far, tsc_hz);
        // The figures come before the clauses a planted mistake in the pin fails, so that a
        // boot with the pin's check taken out still prints them (ADR-0018 V7's pair).
        costs(com1, &creator, bsp, tsc_hz);
        let pin_ok = prove_pin(com1, &creator, bsp, tsc_hz);
        let image_ok = prove_image(com1, &creator, bsp, tsc_hz);
        let narrow_ok = prove_narrow(com1, &creator, bsp, tsc_hz);
        binding_ok && calls_ok && pin_ok && image_ok && narrow_ok
    };
    let same = || {
        memory::stats().map(|(h, f)| (h.bytes_in_use, f))
            == before.map(|(h, f)| (h.bytes_in_use, f))
    };
    ipc::reap_closed();
    sched::yield_until(same, tsc_hz, 2);
    let back = same();
    // No process is left to map it: its frames are executable through no mapping, and still
    // writable through none. And after everything above, the hostile runs among it, they hold
    // the image word for word as it was written at boot (the review's finding F1).
    let unmapped = core_image::mappings() == Some((0, 0));
    let intact = core_image::intact();
    let (heap_now, frames_now) = memory::stats().map_or((0, 0), |(h, f)| (h.bytes_in_use, f));
    let (heap_before, frames_before) = before.map_or((0, 0), |(h, f)| (h.bytes_in_use, f));
    let _ = writeln!(
        com1,
        "origin-kernel: core: every process it launched and all they held are gone: heap {heap_now} bytes (was {heap_before}), {frames_now} free frames (was {frames_before}); origin.core's own frames stay, mapped by no process: {unmapped}, and still hold the image word for word as it was written at boot: {intact}{}",
        if back && unmapped && intact {
            ""
        } else {
            FAILED
        }
    );
    ok && back && unmapped && intact
}

/// Every call, and the hostile calls, through `origin.core`.
fn prove_calls(
    com1: &mut Console,
    creator: &Process,
    bsp: CpuIndex,
    far: CpuIndex,
    tsc_hz: u64,
) -> bool {
    let steps = every_call();
    let covered = makes_every_call(&steps);
    let ran = run(creator, every_call_limits(), far, tsc_hz, |scripted, _| {
        write_requests(&scripted.image)?;
        Some(every_call())
    });
    let every = covered && ran.as_ref().is_some_and(|ran| script_right(&steps, ran));
    let made = ran.as_ref().map_or(0, |ran| ran.reports.len());
    let _ = writeln!(
        com1,
        "origin-kernel: core: a Rust program made every one of origin.kernel's {} calls through origin.core: {made} calls of its script, each reported through debug_report, each with the status it must give, then thread_exit: {every}{}",
        server::CALLS.len(),
        if every { "" } else { FAILED }
    );

    // The two hostile scripts of "IPC and handles", through the stubs.
    let direct = |run: Option<&super::HostileRun>| {
        run.map_or((false, 0), |run| {
            (
                run.ended && run.exited && first_wrong(&run.groups, &run.reports).is_none(),
                run.reports.len(),
            )
        })
    };
    let ipc_run = hostile_run(Via::Core(creator), bsp, tsc_hz).map(|(run, _)| run);
    let memory_run = hostile_memory_run(Via::Core(creator), bsp, tsc_hz);
    let (ipc_ok, ipc_calls) = direct(ipc_run.as_ref());
    let (memory_ok, memory_calls) = direct(memory_run.as_ref());
    for (what, run) in [("IPC", &ipc_run), ("memory", &memory_run)] {
        if let Some((title, name, got)) = run
            .as_ref()
            .and_then(|run| first_wrong(&run.groups, &run.reports))
        {
            let _ = writeln!(
                com1,
                "origin-kernel: core: the hostile {what} script through the stubs: `{title}`, `{name}` gave {got:x?}{FAILED}"
            );
        }
    }
    let hostile = ipc_ok && memory_ok;
    let _ = writeln!(
        com1,
        "origin-kernel: core: the hostile scripts of \"IPC and handles\", made through the stubs, gave call for call the statuses they give directly, and each process exited by itself: {ipc_calls} IPC calls (the one call number no method has is left out: no stub can ask it): {ipc_ok}; {memory_calls} memory calls: {memory_ok}{}",
        if hostile { "" } else { FAILED }
    );
    every && hostile
}

/// The pin: three calls that must stop the thread, and the proof processes' range, kept.
fn prove_pin(com1: &mut Console, creator: &Process, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let bad = Error::BadHandle.raw();
    let pins = &core_image::image().pins;
    // Each run makes one good call first, so that what stops it is the forbidden one.
    let good = close("", Imm(0), bad);

    // A `syscall` instruction in the program's own code.
    let own = run(creator, LIMITS, bsp, tsc_hz, |_, _| {
        Some(alloc::vec![
            good,
            pseudo(
                PSEUDO_OWN_SYSCALL,
                [
                    Imm(sys::DEBUG_REPORT),
                    Imm(0x5EED),
                    Imm(0),
                    Imm(0),
                    Imm(0),
                    Imm(0)
                ],
            ),
        ])
    });
    // The program reports where its own `syscall` instruction is before it runs it, and the
    // ending must record exactly that address (the review of 2026-11-13, finding K3), which is
    // in the program's own code.
    let own_stopped = own.as_ref().is_some_and(|ran| {
        let [status, reported] = ran.reports[..] else {
            return false;
        };
        status == bad as u64
            && program::code_range(&PIN_PROGRAM, ran.base)
                .is_some_and(|(start, end)| (start..end).contains(&reported))
            && ran.ending
                == Some(Ending::Refused {
                    number: sys::DEBUG_REPORT,
                    address: reported,
                })
    });

    // A jump to `debug_report`'s `syscall` with `channel_create`'s number in RAX: the stub's
    // own load of its number is skipped (row E8).
    let jump = |number: u64, argument: u64| {
        run(creator, LIMITS, bsp, tsc_hz, |_, placed| {
            Some(alloc::vec![
                good,
                pseudo(
                    PSEUDO_JUMP,
                    [
                        Imm(placed.core + pins.debug_report),
                        Imm(number),
                        Imm(argument),
                        Imm(0),
                        Imm(0),
                        Imm(0)
                    ],
                ),
            ])
        })
    };
    let at_stub = |ran: &Ran, address: u64| address == ran.core + pins.debug_report;
    let other = jump(sys::CHANNEL_CREATE, slot_addr(0));
    let other_stopped = stopped(other.as_ref(), sys::CHANNEL_CREATE, at_stub);
    // A number no stub has, from a real stub's `syscall`.
    let unknown = jump(9999, 0);
    let unknown_stopped = stopped(unknown.as_ref(), 9999, at_stub);
    // debug_report's own number with bit 32 set, at its own `syscall`: no stub's `mov eax` can
    // make it, and the number is judged whole (finding K2).
    let high = sys::DEBUG_REPORT | 1 << 32;
    let high_stopped = stopped(jump(high, 0x5EED).as_ref(), high, at_stub);
    let pinned = own_stopped && other_stopped && unknown_stopped && high_stopped;
    let _ = writeln!(
        com1,
        "origin-kernel: core: the per-call pin: a syscall in the program's own code stopped the thread, its ending naming call {} and the address the program reported for that instruction: {own_stopped}; a jump to debug_report's syscall with channel_create's number stopped it, naming call {} and the stub's address: {other_stopped}; a number no stub has, 9999, stopped it there too: {unknown_stopped}; and debug_report's own number with bit 32 set, {high:#x}, there too: {high_stopped}{}",
        sys::DEBUG_REPORT,
        sys::CHANNEL_CREATE,
        if pinned { "" } else { FAILED }
    );

    // The kernel's own proof processes keep their range (L8): from outside it a call is
    // refused with a status, never by stopping the thread, and a number no method has answers
    // `not_supported`.
    let outside = Image::with_origin(
        LIMITS,
        origin_frame::proof_programs::script(),
        (CODE - PAGE, CODE),
    )
    .and_then(|mut image| {
        let script = alloc::vec![step("", sys::DEBUG_REPORT, &[Imm(1)], OK), fault()];
        image.scratch()?;
        image.script(&script)?;
        let process = image.finish()?;
        Process::start_thread(
            &process,
            bsp,
            CODE,
            origin_frame::proof_programs::STACK + PAGE,
            origin_frame::proof_programs::SCRIPT,
        )
        .ok()?;
        await_end(&process, tsc_hz, 5);
        Some(
            process.reports().is_empty()
                && matches!(process.ending(), Some(Ending::Faulted { vector: 6, .. })),
        )
    })
    .unwrap_or(false);
    let no_method = alloc::vec![step("", 999, &[], Error::NotSupported.raw())];
    let asked = Scripted::new(LIMITS, Via::Direct)
        .and_then(|mut scripted| {
            scripted.image.scratch()?;
            scripted.image.script(&no_method)?;
            let process = scripted.start(bsp, None)?;
            await_end(&process, tsc_hz, 5);
            Some(
                super::statuses_right(&no_method, &process.reports())
                    && process.ending() == Some(Ending::Exited(0)),
            )
        })
        .unwrap_or(false);
    let kept = outside && asked;
    let _ = writeln!(
        com1,
        "origin-kernel: core: the kernel's proof processes keep their range: a call from outside it was refused with a status and the process went on to its own fault: {outside}; a call number no method has answered NOT_SUPPORTED: {asked}{}",
        if kept { "" } else { FAILED }
    );
    pinned && kept
}

/// The binding (ADR-0011 L3): in two processes loaded with the pin program and one with the
/// first program, none of them started, every import slot holds its export's address in that
/// process's own `origin.core`, on a page the program cannot write.
fn prove_binding(com1: &mut Console, creator: &Process) -> bool {
    let (mut bound, mut first_bound) = (false, false);
    if let (Some(a), Some(b)) = (
        Scripted::new(LIMITS, Via::Core(creator)),
        Scripted::new(LIMITS, Via::Core(creator)),
    ) && let (Some(pa), Some(pb)) = (a.pinned.as_ref(), b.pinned.as_ref())
    {
        bound = imports_bound(&a.image.process, &PIN_PROGRAM, pa.base, pa.core)
            && imports_bound(&b.image.process, &PIN_PROGRAM, pb.base, pb.core);
    }
    if let Ok(first) = launch::load_program(creator, &FIRST_PROGRAM, &FIRST_PROGRAM_RUN) {
        first_bound = imports_bound(&first.process, &FIRST_PROGRAM, first.base, first.core);
    }
    let binding = bound && first_bound;
    let _ = writeln!(
        com1,
        "origin-kernel: core: the binding: before its thread starts, each of {}'s {} import slots holds the address of its export in its own origin.core, on a page it cannot write: {bound}; and each of {}'s {}: {first_bound}{}",
        PIN_PROGRAM.name,
        PIN_PROGRAM.imports.len(),
        FIRST_PROGRAM.name,
        FIRST_PROGRAM.imports.len(),
        if binding { "" } else { FAILED }
    );
    binding
}

/// The image: never writable, never unmapped or changed, one set of frames.
fn prove_image(com1: &mut Console, creator: &Process, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let span = core_image::image().span;
    let invalid = Error::InvalidArgs.raw();

    // A write to its first page: the interpreter's copy of a `u32` to that address.
    let written = run(creator, LIMITS, bsp, tsc_hz, |_, placed| {
        Some(alloc::vec![super::copy32(MSG, placed.core)])
    });
    let write_faulted = written.as_ref().is_some_and(|ran| {
        ran.reports.is_empty()
            && matches!(
                ran.ending,
                Some(Ending::Faulted { vector: PAGE_FAULT, error, address })
                    if address == ran.core && error & FAULT_BITS == WRITE_TO_PRESENT_USER_PAGE
            )
    });

    // The memory calls on its range, then a wait the kernel ends once it has looked at the
    // process's tables: the pages must still be there, as they were.
    // An object of two pages, so that the map that starts below the base asks nothing of the
    // object it cannot give, and is refused for the image alone.
    let limits = Limits {
        handles: 8,
        memory: MemoryLimits {
            committed: 2 * PAGE as usize,
            mappings: 1,
            changes: crate::mapping::Rate {
                burst: 64,
                per_second: 64,
            },
        },
        ..LIMITS
    };
    let mut kept = false;
    let mut refused = false;
    'kept: {
        let Some(mut scripted) = Scripted::new(limits, Via::Core(creator)) else {
            break 'kept;
        };
        let Some(core) = scripted.pinned.as_ref().map(|p| p.core) else {
            break 'kept;
        };
        // A channel end of its own whose peer stays here: closing the peer ends its wait.
        let Ok((own, peer)) = ipc::create_channel(&scripted.image.process.account) else {
            break 'kept;
        };
        let Ok(live) = scripted
            .image
            .process
            .grant(Object::Channel(own), rights::CHANNEL)
        else {
            break 'kept;
        };
        let steps = alloc::vec![
            step(
                "",
                sys::MEMORY_OBJECT_CREATE,
                &[Imm(REQ_OBJECT), Imm(slot_addr(4))],
                OK
            ),
            step(
                "unmap",
                sys::MEMORY_UNMAP,
                &[h(SELF), Imm(core), Imm(span)],
                invalid
            ),
            step(
                "protect",
                sys::MEMORY_PROTECT,
                &[h(SELF), Imm(core), Imm(span), Imm(map_access::READ.into())],
                invalid
            ),
            step(
                "map over it",
                sys::MEMORY_MAP,
                &[h(SELF), h(4), Imm(REQ_FIXED), Imm(MAPPED)],
                invalid
            ),
            step(
                "map two pages over it from one page below",
                sys::MEMORY_MAP,
                &[h(SELF), h(4), Imm(REQ_FIXED_BELOW), Imm(MAPPED)],
                invalid
            ),
            wait("", Imm(live), signals::PEER_CLOSED, DEADLINE_INFINITE, OK),
            step("still callable", sys::CLOCK_READ, &[Imm(CLOCK)], OK),
        ];
        let fixed = map_request(40, map_access::READ, map_flags::FIXED, 0, PAGE, core);
        let below = map_request(
            40,
            map_access::READ,
            map_flags::FIXED,
            0,
            2 * PAGE,
            core - PAGE,
        );
        if write_requests(&scripted.image).is_none()
            || scripted
                .image
                .write(REQ_OBJECT, &object_request(16, 2 * PAGE))
                .is_none()
            || scripted.image.write(REQ_FIXED, &fixed).is_none()
            || scripted.image.write(REQ_FIXED_BELOW, &below).is_none()
            || scripted.image.scratch().is_none()
            || scripted.image.script(&steps).is_none()
        {
            break 'kept;
        }
        let Some(process) = scripted.start(bsp, Some(SELF)) else {
            break 'kept;
        };
        // Blocked in its wait, after the four calls: look at its tables as they now are.
        sched::yield_until(
            || process.waiting.load(Ordering::Acquire) != 0 || process.ending().is_some(),
            tsc_hz,
            5,
        );
        kept = process.ending().is_none() && core_mapped(&process, core);
        drop(peer);
        await_end(&process, tsc_hz, 5);
        refused = super::statuses_right(&steps, &process.reports())
            && process.ending() == Some(Ending::Exited(0));
    }

    // Three processes, loaded and never started, two with the pin program and one with the
    // first: one set of frames, at a base of each process's own. A process whose random base
    // equals one drawn before it is loaded once more before anything is judged (finding K7):
    // a correct kernel then fails "each at a base of its own" only on two such draws in a row,
    // about 3 in 10^19 boots, where it was about 7 in 10^10.
    let (mut shared, mut apart, mut write_refused) = (false, false, false);
    let mut executors = 0;
    let core_of = |s: &Scripted| s.pinned.as_ref().map(|p| p.core);
    let a = Scripted::new(LIMITS, Via::Core(creator));
    let mut b = Scripted::new(LIMITS, Via::Core(creator));
    if a.as_ref().and_then(core_of).is_some()
        && a.as_ref().and_then(core_of) == b.as_ref().and_then(core_of)
    {
        b = Scripted::new(LIMITS, Via::Core(creator));
    }
    let load_first = || launch::load_program(creator, &FIRST_PROGRAM, &FIRST_PROGRAM_RUN).ok();
    let mut first = load_first();
    if first.as_ref().is_some_and(|f| {
        Some(f.core) == a.as_ref().and_then(core_of) || Some(f.core) == b.as_ref().and_then(core_of)
    }) {
        first = load_first();
    }
    if let (Some(a), Some(b), Some(first)) = (a.as_ref(), b.as_ref(), first.as_ref())
        && let (Some(pa), Some(pb)) = (a.pinned.as_ref(), b.pinned.as_ref())
    {
        shared = core_mapped(&a.image.process, pa.core)
            && core_mapped(&b.image.process, pb.core)
            && core_mapped(&first.process, first.core);
        apart = pa.core != pb.core && pa.core != first.core && pb.core != first.core;
        let counts = core_image::mappings();
        executors = counts.map_or(0, |(_, executable)| executable);
        shared &= counts.is_some_and(|(writable, executable)| writable == 0 && executable >= 3);
        // The kernel's own way of writing into a process's page, asked to write the byte that is
        // already there, at `origin.core`'s first page in that process: it must refuse
        // (finding K1). The same byte, so that a kernel that wrongly let it through still
        // leaves the image as it was, and the clause alone fails.
        let byte = core_image::image().code.first().copied().unwrap_or(0xCC);
        write_refused = a
            .image
            .process
            .frame_of(pa.core)
            .is_some_and(|frame| !a.image.process.write_page(frame, 0, &[byte]));
    }
    let image_ok = write_faulted && refused && kept && shared && apart && write_refused;
    let _ = writeln!(
        com1,
        "origin-kernel: core: origin.core in a process: a write to its page faulted there: {write_faulted}; memory_unmap and memory_protect of its range, a fixed memory_map over it, and a fixed memory_map of two pages from one page below it each answered INVALID_ARGS, and calls through it went on: {refused}; its {} pages were still mapped read-execute on the same frames afterwards: {kept}; three processes, two of the pin program and one of the first, hold it on one set of frames, executable in {executors} spaces and writable in none: {shared}, each at a base of its own: {apart}; the kernel's own write path into a process's pages refused a page of it: {write_refused}{}",
        span / PAGE,
        if image_ok { "" } else { FAILED }
    );
    image_ok
}

/// The limits of a process that makes the narrow-parameter calls: a channel, an event, a port
/// and a binding.
fn narrow_limits() -> Limits {
    Limits {
        objects: ObjectLimits {
            events: 1,
            ports: 1,
            bindings: 1,
            bindings_per_port: 1,
        },
        ..LIMITS
    }
}

/// Four calls whose 32-bit parameter has bits set above it, in the second, third and fourth
/// registers: each must give `want`.
fn narrow_steps(want: i64) -> Vec<Step> {
    let sig = u64::from(signals::SIGNALED);
    alloc::vec![
        create(0),
        step(
            "rights with bit 40 set",
            sys::HANDLE_DUPLICATE,
            &[
                h(0),
                Imm(u64::from(rights::CHANNEL) | 1 << 40),
                Imm(slot_addr(2))
            ],
            want
        ),
        step(
            "signals with bit 35 set",
            sys::WAIT_ONE,
            &[
                h(0),
                Imm(u64::from(signals::WRITABLE) | 1 << 35),
                Imm(0),
                Imm(OBS)
            ],
            want
        ),
        step("", sys::EVENT_CREATE, &[Imm(slot_addr(3))], OK),
        step(
            "a mask with bit 33 set, in the third register",
            sys::OBJECT_SIGNAL,
            &[h(3), Imm(0), Imm(sig | 1 << 33)],
            want
        ),
        step("", sys::PORT_CREATE, &[Imm(slot_addr(5))], OK),
        step(
            "signals with bit 39 set, in the fourth register",
            sys::OBJECT_WAIT_ASYNC,
            &[h(5), h(3), Imm(7), Imm(sig | 1 << 39)],
            want
        ),
    ]
}

/// L13: the same registers through a stub and in a direct call.
fn prove_narrow(com1: &mut Console, creator: &Process, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let through = narrow_steps(OK);
    let through_ok = run(creator, narrow_limits(), bsp, tsc_hz, |_, _| {
        Some(narrow_steps(OK))
    })
    .is_some_and(|ran| script_right(&through, &ran));
    let direct = narrow_steps(Error::InvalidArgs.raw());
    let direct_ok = Scripted::new(narrow_limits(), Via::Direct)
        .and_then(|mut scripted| {
            scripted.image.scratch()?;
            scripted.image.script(&direct)?;
            let process = scripted.start(bsp, None)?;
            await_end(&process, tsc_hz, 5);
            Some(
                super::statuses_right(&direct, &process.reports())
                    && process.ending() == Some(Ending::Exited(0)),
            )
        })
        .unwrap_or(false);
    let narrow = through_ok && direct_ok;
    let _ = writeln!(
        com1,
        "origin-kernel: core: narrow parameters: through its stub, a call whose 32-bit parameter has bits set above it, in the second, third and fourth registers, is the same call with them clear, 4 calls: {through_ok}; the same registers in a direct call by a proof process are each INVALID_ARGS: {direct_ok}{}",
        if narrow { "" } else { FAILED }
    );
    narrow
}

/// What one call through `origin.core` costs: figures under whatever runs this boot, judged by
/// nothing (CLAUDE.md §53; ADR-0018 V3).
fn costs(com1: &mut Console, creator: &Process, bsp: CpuIndex, tsc_hz: u64) {
    const TIMES: u64 = 2000;
    let timed = |number: u64, a: u64, b: u64, c: u64| {
        pseudo(
            PSEUDO_TIMED,
            [Imm(TIMES), Imm(number), Imm(a), Imm(b), Imm(c), Imm(0)],
        )
    };
    let ran = run(creator, LIMITS, bsp, tsc_hz, |_, _| {
        Some(alloc::vec![
            // A handle that names nothing: the call goes in, through the pin and the
            // dispatcher, to the table, and comes back.
            timed(sys::HANDLE_CLOSE, 0, 0, 0),
            // The same with one 32-bit parameter: one instruction more in its stub (L13).
            timed(sys::HANDLE_DUPLICATE, 0, rights::READ.into(), slot_addr(0)),
        ])
    });
    let each = |i: usize| {
        ran.as_ref()
            .and_then(|ran| ran.reports.get(i))
            .map_or(0, |nanos| nanos / TIMES)
    };
    let _ = writeln!(
        com1,
        "origin-kernel: core: one call through origin.core, as it is pinned: {} ns (handle_close of nothing, {TIMES} calls); {} ns with a stub that extends one parameter (handle_duplicate of nothing)",
        each(0),
        each(1)
    );
}

/// Runs the proof "the flags at a thread's entry" (ADR-0005 Amendment 9, H1), on the boot
/// processor and on another.
pub(crate) fn prove_entry_flags(
    com1: &mut Console,
    cpus: &[CpuIndex],
    bsp: CpuIndex,
    tsc_hz: u64,
) -> bool {
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let Some(creator) = Process::proof(CREATOR, (0, 0), BOOT_PROOF_PRINCIPAL) else {
        return false;
    };
    let limits = ProgramLimits {
        limits: Limits {
            handles: START_HANDLES,
            channels: 0,
            message_bytes: 0,
            threads: 1,
            ..LIMITS
        },
        stack_pages: STACK_PAGES,
    };
    let read = |cpu: CpuIndex| -> Option<u64> {
        let loaded = launch::load_program(&creator, &PIN_PROGRAM, &limits).ok()?;
        let given = Given {
            bootstrap: None,
            directory: None,
            args: &[b"flags"],
            env: &[],
        };
        let launched = launch::start_program(loaded, given, cpu).ok()?;
        await_end(&launched.process, tsc_hz, 5);
        let reports = launched.process.reports();
        (launched.process.ending() == Some(Ending::Exited(FLAGS_REPORTED)) && reports.len() == 1)
            .then(|| reports.first().copied())
            .flatten()
    };
    let (here, there) = (read(bsp), read(far));
    let as_h1_says = |flags: Option<u64>| {
        flags.is_some_and(|flags| flags & CLEAR_AT_ENTRY == 0 && flags & IF != 0)
    };
    let right = as_h1_says(here) && as_h1_says(there);
    // A machine with one processor has no other to read on: both readings are its own.
    let where_read = if far == bsp {
        "twice on this machine's one processor"
    } else {
        "on the boot processor and on another"
    };
    let _ = writeln!(
        com1,
        "origin-kernel: entry flags: a program's first instruction pushed RFLAGS, {where_read}, and found {:#x} and {:#x}: the direction flag and the six status flags clear and interrupts on: {right}{}",
        here.unwrap_or(u64::MAX),
        there.unwrap_or(u64::MAX),
        if right { "" } else { FLAGS_FAILED }
    );
    right
}
