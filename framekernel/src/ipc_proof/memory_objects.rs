//! Boot proof of memory objects (ADR-0005 P7 and P11 as C6, C7 and C8 decide; task S3-5b): ring-3
//! programs create, share, map, protect and unmap memory objects through the system calls, and
//! the kernel checks what each saw and how each ended.
//!
//! - **Sharing:** A makes an object, maps it at a fixed address, writes to it and passes the
//!   handle to B, which maps it too, reads A's word and writes its own, which A then reads. A
//!   reduces its mapping to read-only: an increase is refused, and A's next write faults at the
//!   mapping. B is refused a fixed address over its own mapping, execute, and a mapping that
//!   allows what the handle it is made through does not; places two mappings at random; closes
//!   the handle and still reads through its mapping; unmaps it, and its next read faults there.
//! - **Quotas:** Q's objects are refused past its bound and taken again once one is closed; a
//!   closed object it still maps keeps its charge; a third mapping past its limit of two is
//!   refused.
//! - **Another process:** C maps its object into T only through a handle to T with `write`, and
//!   T reads C's word; unmapping there needs `write` too, and T's next read faults.
//! - **A hostile loop:** H maps and unmaps until its budget is spent; the next unmap is refused
//!   with `should_wait` and changes nothing, and once the budget has refilled it is served.
//! - **An address nobody was told:** a map whose address cannot be written to its caller is
//!   taken back, whatever the caller's budget, and what that cost is charged to it. The kernel
//!   makes the report fail itself: ring 3 cannot make the write of the address fail at will
//!   (its page must go between the call's check and its write).
//! - **Costs:** map, protect and unmap through the kernel's own calls, as QEMU figures; and the
//!   unmap of a shared object's page from a space a thread is reading it in, on another
//!   processor where there is one, after which the thread faults there and the object stays
//!   mapped in its creator's space.

use alloc::vec::Vec;
use core::fmt::Write;

use origin_abi::{Error, OK, map_access, map_flags, rights, syscall as sys};
use origin_frame::objref::ObjectRef;
use origin_frame::proof_programs::{self, DATA, STACK};

use super::Arg::{Handle as Hd, Imm};
use super::{
    Arg, CODE, Image, LIMITS, MSG, PSEUDO_REPORT32, RBUF, RECV, Script, Step, Want, await_end,
    close, copy32, dup, h, map_request, object_request, read, read_handles, readable, reported_u64,
    slot_addr, statuses_right, step, value_at, write, write_slot,
};
use crate::console::Console;
use crate::cpu::{self, CpuIndex};
use crate::handles::Need;
use crate::ipc::{self, Object};
use crate::mapping::{PAGE, PLACE_HIGH, PLACE_LOW, Rate, Request};
use crate::memory_object::{self, MemoryObject};
use crate::process::{Ending, Limits, MemoryLimits, Process};
use crate::{memory, sched, time};

/// Where the proof's mappings go: each its own gigabyte of the user half, clear of the programs'
/// code, data and stacks and of the region random placements use.
const FIXED_A: u64 = 0x0000_0001_0000_0000;
const FIXED_B: u64 = 0x0000_0002_0000_0000;
const FIXED_C: u64 = 0x0000_0003_0000_0000;
const FIXED_T: u64 = 0x0000_0004_0000_0000;
const FIXED_H: u64 = 0x0000_0005_0000_0000;
const FIXED_K: u64 = 0x0000_0006_0000_0000;
/// Where the reading program maps the shared object: below 2 GiB, so that its code can name
/// the address in an immediate.
const READ_AT: u64 = 0x0000_0000_4000_0000;

/// The words A, B and C write, and where each program's copy of them is kept.
const VALUE_A: u32 = 0xA11C_E5A1;
const VALUE_B: u32 = 0x0B0B_5EB2;
const VALUE_C: u32 = 0xC0DE_5EC3;
const VALUES: u64 = DATA + 0x1900;
const VALUE_A_AT: u64 = VALUES;
const VALUE_B_AT: u64 = VALUES + 4;
const VALUE_C_AT: u64 = VALUES + 8;

/// Where each program keeps its requests and the addresses the kernel returns.
const OBJECT_1: u64 = DATA + 0x1000;
const OBJECT_2: u64 = DATA + 0x1010;
const fn request_at(k: u64) -> u64 {
    DATA + 0x1040 + 0x40 * k
}
const fn out(k: u64) -> u64 {
    DATA + 0x1800 + 8 * k
}

/// What every process here may change: far more than it does, except H's.
const PLENTY: Rate = Rate {
    burst: 1024,
    per_second: 1024,
};
/// H's budget: four unmaps of one page, each priced at two units (one shootdown, one processor
/// holding the space), and one unit regained every half second. The fifth unmap is refused
/// unless a second has passed since the first, which a loop of system calls never takes.
const HOSTILE: Rate = Rate {
    burst: 8,
    per_second: 2,
};
/// How long the kernel lets H's budget refill before it lets H try again: the two units the
/// refused unmap is priced at take a second.
const REFILL_NANOS: u64 = 1_200_000_000;

/// A process's limits with a memory quota of `pages` pages, `mappings` mappings and `changes`.
fn limits(pages: usize, mappings: usize, changes: Rate) -> Limits {
    Limits {
        memory: MemoryLimits {
            committed: pages * PAGE as usize,
            mappings,
            changes,
        },
        ..LIMITS
    }
}

/// An `object_request` for `pages` pages.
fn object_pages(pages: u64) -> Vec<u8> {
    object_request(origin_abi::object_request::SIZE, pages * PAGE)
}

/// A `map_request`: `access` and `flags` bits, the object's pages from `first`, `pages` of them,
/// and with `FIXED` the address.
fn map_pages(access: u32, flags: u32, first: u64, pages: u64, address: u64) -> Vec<u8> {
    map_request(
        origin_abi::map_request::SIZE,
        access,
        flags,
        first * PAGE,
        pages * PAGE,
        address,
    )
}

const READ: u32 = map_access::READ;
const READ_WRITE: u32 = map_access::READ | map_access::WRITE;
const FIXED: u32 = map_flags::FIXED;

/// The map requests the programs use, at `request_at(k)` in every program.
fn requests() -> [Vec<u8>; 10] {
    [
        // A's and B's mappings of the shared object.
        map_pages(READ_WRITE, FIXED, 0, 2, FIXED_A),
        map_pages(READ_WRITE, FIXED, 0, 2, FIXED_B),
        // Over B's own mapping; execute; anywhere.
        map_pages(READ, FIXED, 1, 1, FIXED_B + PAGE),
        map_pages(READ | map_access::EXECUTE, 0, 0, 1, 0),
        map_pages(READ, 0, 0, 1, 0),
        // C's own mapping, and C's mapping into T.
        map_pages(READ_WRITE, FIXED, 0, 1, FIXED_C),
        map_pages(READ, FIXED, 0, 1, FIXED_T),
        // H's.
        map_pages(READ, FIXED, 0, 1, FIXED_H),
        // Q's, anywhere.
        map_pages(READ, 0, 0, 1, 0),
        map_pages(READ, 0, 0, 1, 0),
    ]
}
const A_MAP: u64 = 0;
const B_MAP: u64 = 1;
const B_OVER: u64 = 2;
const B_EXECUTE: u64 = 3;
const B_ANYWHERE: u64 = 4;
const C_MAP: u64 = 5;
const T_MAP: u64 = 6;
const H_MAP: u64 = 7;
const Q_MAP: u64 = 8;

fn create_object(request: u64, slot: u64, want: i64) -> Step {
    step(
        "",
        sys::MEMORY_OBJECT_CREATE,
        &[Imm(request), Imm(slot_addr(slot))],
        want,
    )
}

fn map_object(process: Arg, object: Arg, request: u64, address: u64, want: i64) -> Step {
    step(
        "",
        sys::MEMORY_MAP,
        &[process, object, Imm(request_at(request)), Imm(address)],
        want,
    )
}

fn unmap_mapping(process: Arg, address: u64, length: u64, want: i64) -> Step {
    step(
        "",
        sys::MEMORY_UNMAP,
        &[process, Imm(address), Imm(length)],
        want,
    )
}

fn protect_mapping(process: Arg, address: u64, length: u64, access: u32, want: i64) -> Step {
    step(
        "",
        sys::MEMORY_PROTECT,
        &[process, Imm(address), Imm(length), Imm(u64::from(access))],
        want,
    )
}

/// A read of the `u32` at `addr` that must fault: the thread ends, and nothing is reported.
const fn faulting_read(addr: u64) -> Step {
    Step {
        name: "",
        number: PSEUDO_REPORT32,
        args: [Imm(addr), Imm(0), Imm(0), Imm(0), Imm(0), Imm(0)],
        want: Want::Nothing,
    }
}

/// A program being built: its image and its script.
struct Program {
    image: Image,
    script: Script,
}

impl Program {
    /// A scripted process under `limits`, its scratch memory holding the proof's requests and
    /// words.
    fn new(limits: Limits) -> Option<Self> {
        let image = Image::new(limits, proof_programs::script())?;
        image.scratch()?;
        image.write(OBJECT_1, &object_pages(1))?;
        image.write(OBJECT_2, &object_pages(2))?;
        for (k, request) in requests().iter().enumerate() {
            image.write(request_at(k as u64), request)?;
        }
        image.write(VALUE_A_AT, &VALUE_A.to_le_bytes())?;
        image.write(VALUE_B_AT, &VALUE_B.to_le_bytes())?;
        image.write(VALUE_C_AT, &VALUE_C.to_le_bytes())?;
        Some(Self {
            image,
            script: Script::default(),
        })
    }

    /// The process, its script written; its own handle, with C6's rights, goes in slot 0 once
    /// it exists.
    fn finish(mut self) -> Option<(ObjectRef<Process>, Vec<Step>)> {
        self.image.script(&self.script.steps)?;
        let process = self.image.finish()?;
        let own = process
            .grant(Object::Process(process.clone()), rights::PROCESS)
            .ok()?;
        write_slot(&process, 0, own).then_some(())?;
        Some((process, self.script.steps))
    }
}

/// A started program: its process, its script, and where its reports are.
struct Run<T> {
    process: ObjectRef<Process>,
    steps: Vec<Step>,
    marks: T,
}

impl<T> Run<T> {
    /// Every call returned what its script says.
    fn statuses(&self) -> bool {
        statuses_right(&self.steps, &self.process.reports())
    }

    /// It exited by itself.
    fn exited(&self) -> bool {
        self.process.ending() == Some(Ending::Exited(0))
    }

    /// Its last thread faulted on a page at `address`.
    fn faulted_at(&self, address: u64) -> bool {
        matches!(
            self.process.ending(),
            Some(Ending::Faulted { vector: 14, address: at, .. }) if at == address
        )
    }

    /// The `u32` it reported at `at`.
    fn word(&self, at: usize) -> Option<u32> {
        self.process.reports().get(at).map(|&v| v as u32)
    }

    /// Whether the call it reported at `at` returned `status`.
    fn returned(&self, at: usize, status: i64) -> bool {
        self.process.reports().get(at) == Some(&(status as u64))
    }
}

fn start<T>(program: Program, marks: T, cpu: CpuIndex) -> Option<Run<T>> {
    let (process, steps) = program.finish()?;
    Process::start_thread(&process, cpu, CODE, STACK + PAGE, proof_programs::SCRIPT).ok()?;
    Some(Run {
        process,
        steps,
        marks,
    })
}

// ------------------------------------------------------------------ sharing

struct AMarks {
    /// Its map of the object.
    map: usize,
    /// B's word, read through A's mapping.
    seen_b: usize,
    /// The protect to read-only, and the one that would add write back after it.
    protect: usize,
    /// A's own word, still readable once the mapping is read-only.
    readable: usize,
}

struct BMarks {
    /// Its map of the object A passed, and A's word read through it.
    map: usize,
    seen_a: usize,
    /// The fixed map over B's own mapping, and what that mapping reads after it.
    over: usize,
    kept: usize,
    /// The map that asks for execute.
    execute: usize,
    /// The writable map through a handle without `write`, then the map through one without
    /// `read`.
    by_rights: usize,
    /// The two random maps' statuses, then their addresses, each in two words.
    random: usize,
    /// The handle's close, and what the mapping reads after it.
    close: usize,
    /// The unmap, before the read that must fault.
    unmap: usize,
}

/// A and B share an object by passing its handle (see the module documentation).
fn sharing(cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> Option<(Run<AMarks>, Run<BMarks>)> {
    let mut a = Program::new(limits(2, 1, PLENTY))?;
    let mut b = Program::new(limits(0, 4, PLENTY))?;
    let (a_end, b_end) = ipc::create_channel(&a.image.process.account).ok()?;
    let to_b = Imm(u64::from(
        a.image
            .process
            .grant(Object::Channel(a_end), rights::CHANNEL)
            .ok()?,
    ));
    let from_a = Imm(u64::from(
        b.image
            .process
            .grant(Object::Channel(b_end), rights::CHANNEL)
            .ok()?,
    ));
    let s = &mut a.script;
    s.call(create_object(OBJECT_2, 1, OK));
    let a_map = s.reported();
    s.call(map_object(h(0), h(1), A_MAP, out(0), OK));
    s.call(copy32(VALUE_A_AT, FIXED_A));
    // The handle moves to B: sharing is by passing it.
    s.call(step(
        "",
        sys::CHANNEL_WRITE,
        &[to_b, Imm(MSG), Imm(4), Imm(slot_addr(1)), Imm(1)],
        OK,
    ));
    s.call(readable(to_b));
    s.call(read("", to_b, RBUF, 16, OK));
    let seen_b = s.reported();
    s.call(value_at("", FIXED_A + 4, 0));
    let protect = s.reported();
    s.call(protect_mapping(h(0), FIXED_A, 2 * PAGE, READ, OK));
    s.call(protect_mapping(
        h(0),
        FIXED_A,
        2 * PAGE,
        READ_WRITE,
        Error::AccessDenied.raw(),
    ));
    let readable_at = s.reported();
    s.call(value_at("", FIXED_A, 0));
    // A write through the read-only mapping: the thread faults there.
    s.call(copy32(VALUE_A_AT, FIXED_A));

    let s = &mut b.script;
    s.call(readable(from_a));
    s.call(read_handles("", from_a, OK));
    let object = Hd(RECV);
    let b_map = s.reported();
    s.call(map_object(h(0), object, B_MAP, out(0), OK));
    let seen_a = s.reported();
    s.call(value_at("", FIXED_B, 0));
    s.call(copy32(VALUE_B_AT, FIXED_B + 4));
    s.call(write("", from_a, MSG, 4, OK));
    // Never over a mapping, and never executable.
    let over = s.reported();
    s.call(map_object(
        h(0),
        object,
        B_OVER,
        out(1),
        Error::InvalidArgs.raw(),
    ));
    let kept = s.reported();
    s.call(value_at("", FIXED_B, 0));
    let execute = s.reported();
    s.call(map_object(
        h(0),
        object,
        B_EXECUTE,
        out(1),
        Error::AccessDenied.raw(),
    ));
    // What a handle allows bounds what a mapping made through it allows (C7): nothing
    // writable without `write`, and nothing at all without `read`. Each handle has `map`, and
    // each place is free.
    s.call(dup("", object, rights::READ | rights::MAP, 1, OK));
    s.call(dup("", object, rights::WRITE | rights::MAP, 2, OK));
    let by_rights = s.reported();
    s.call(map_object(
        h(0),
        h(1),
        A_MAP,
        out(1),
        Error::AccessDenied.raw(),
    ));
    s.call(map_object(
        h(0),
        h(2),
        B_ANYWHERE,
        out(1),
        Error::AccessDenied.raw(),
    ));
    let random = s.reported();
    s.call(map_object(h(0), object, B_ANYWHERE, out(2), OK));
    s.call(map_object(h(0), object, B_ANYWHERE, out(3), OK));
    for k in [2, 3] {
        s.call(value_at("", out(k), 0));
        s.call(value_at("", out(k) + 4, 0));
    }
    // Closing the handle leaves the mapping; unmapping it does not.
    let close_at = s.reported();
    s.call(close("", object, OK));
    s.call(value_at("", FIXED_B, 0));
    let unmap = s.reported();
    s.call(unmap_mapping(h(0), FIXED_B, 2 * PAGE, OK));
    s.call(faulting_read(FIXED_B));

    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let b = start(
        b,
        BMarks {
            map: b_map,
            seen_a,
            over,
            kept,
            execute,
            by_rights,
            random,
            close: close_at,
            unmap,
        },
        bsp,
    )?;
    let a = start(
        a,
        AMarks {
            map: a_map,
            seen_b,
            protect,
            readable: readable_at,
        },
        far,
    )?;
    let _ = await_end(&a.process, tsc_hz, 10) && await_end(&b.process, tsc_hz, 10);
    Some((a, b))
}

// ------------------------------------------------------------------ quotas

/// Q's quota is three pages and two mappings (see the module documentation).
fn quotas(bsp: CpuIndex, tsc_hz: u64) -> Option<Run<()>> {
    let mut q = Program::new(limits(3, 2, PLENTY))?;
    let limit = Error::Limit.raw();
    let s = &mut q.script;
    s.call(create_object(OBJECT_2, 1, OK));
    s.call(create_object(OBJECT_2, 2, limit));
    s.call(create_object(OBJECT_1, 2, OK));
    s.call(close("", h(1), OK));
    s.call(create_object(OBJECT_2, 1, OK));
    s.call(map_object(h(0), h(2), Q_MAP, out(0), OK));
    s.call(map_object(h(0), h(2), Q_MAP + 1, out(1), OK));
    s.call(map_object(h(0), h(2), Q_MAP, out(2), limit));
    // Its handle closed, the one-page object is still mapped twice: still charged.
    s.call(close("", h(2), OK));
    s.call(create_object(OBJECT_1, 3, limit));
    let q = start(q, (), bsp)?;
    let _ = await_end(&q.process, tsc_hz, 10);
    Some(q)
}

// ------------------------------------------------------------------ another process

/// Where C's reports of its maps into T, and of its unmaps there, begin: each the weak
/// handle's, then the strong one's.
struct CMarks {
    map: usize,
    unmap: usize,
}

/// C maps its object into T (see the module documentation).
fn into_another(bsp: CpuIndex, tsc_hz: u64) -> Option<(Run<CMarks>, Run<usize>)> {
    let mut t = Program::new(limits(0, 1, PLENTY))?;
    let mut c = Program::new(limits(1, 1, PLENTY))?;
    let (c_end, t_end) = ipc::create_channel(&c.image.process.account).ok()?;
    let to_t = Imm(u64::from(
        c.image
            .process
            .grant(Object::Channel(c_end), rights::CHANNEL)
            .ok()?,
    ));
    let from_c = Imm(u64::from(
        t.image
            .process
            .grant(Object::Channel(t_end), rights::CHANNEL)
            .ok()?,
    ));
    let s = &mut t.script;
    s.call(readable(from_c));
    s.call(read("", from_c, RBUF, 16, OK));
    let seen_c = s.reported();
    s.call(value_at("", FIXED_T, 0));
    s.call(write("", from_c, MSG, 4, OK));
    s.call(readable(from_c));
    s.call(read("", from_c, RBUF, 16, OK));
    // C has unmapped it: the read faults.
    s.call(faulting_read(FIXED_T));
    let t = start(t, seen_c, bsp)?;

    let strong = c
        .image
        .process
        .grant(Object::Process(t.process.clone()), rights::PROCESS)
        .ok()?;
    let weak = c
        .image
        .process
        .grant(
            Object::Process(t.process.clone()),
            rights::PROCESS & !rights::WRITE,
        )
        .ok()?;
    let (strong, weak) = (Imm(u64::from(strong)), Imm(u64::from(weak)));
    let denied = Error::AccessDenied.raw();
    let s = &mut c.script;
    s.call(create_object(OBJECT_1, 1, OK));
    s.call(map_object(h(0), h(1), C_MAP, out(0), OK));
    s.call(copy32(VALUE_C_AT, FIXED_C));
    let map = s.reported();
    s.call(map_object(weak, h(1), T_MAP, out(1), denied));
    s.call(map_object(strong, h(1), T_MAP, out(1), OK));
    s.call(write("", to_t, MSG, 4, OK));
    s.call(readable(to_t));
    s.call(read("", to_t, RBUF, 16, OK));
    let unmap = s.reported();
    s.call(unmap_mapping(weak, FIXED_T, PAGE, denied));
    s.call(unmap_mapping(strong, FIXED_T, PAGE, OK));
    s.call(write("", to_t, MSG, 4, OK));
    let c = start(c, CMarks { map, unmap }, bsp)?;
    let _ = await_end(&c.process, tsc_hz, 10) && await_end(&t.process, tsc_hz, 10);
    Some((c, t))
}

// ------------------------------------------------------------------ a hostile loop

struct HMarks {
    /// The refused unmap's status.
    refused: usize,
    /// What the still-mapped page reads.
    still: usize,
    /// The unmap served once the budget has refilled.
    served: usize,
}

/// H's loop, refused once its budget is spent and served again once it has refilled (see the
/// module documentation).
fn hostile_loop(bsp: CpuIndex, tsc_hz: u64) -> Option<Run<HMarks>> {
    let mut hp = Program::new(limits(1, 1, HOSTILE))?;
    let holder = Process::new(LIMITS, (CODE, CODE + PAGE), 0)?;
    let (h_end, holder_end) = ipc::create_channel(&holder.account).ok()?;
    let from_kernel = Imm(u64::from(
        hp.image
            .process
            .grant(Object::Channel(h_end), rights::CHANNEL)
            .ok()?,
    ));
    let go = holder
        .grant(Object::Channel(holder_end), rights::CHANNEL)
        .ok()?;
    let s = &mut hp.script;
    s.call(create_object(OBJECT_1, 1, OK));
    for _ in 0..4 {
        s.call(map_object(h(0), h(1), H_MAP, out(0), OK));
        s.call(unmap_mapping(h(0), FIXED_H, PAGE, OK));
    }
    s.call(map_object(h(0), h(1), H_MAP, out(0), OK));
    let refused = s.reported();
    s.call(unmap_mapping(h(0), FIXED_H, PAGE, Error::ShouldWait.raw()));
    // Refused before anything changed: still mapped.
    let still = s.reported();
    s.call(value_at("", FIXED_H, 0));
    s.call(readable(from_kernel));
    let served = s.reported();
    s.call(unmap_mapping(h(0), FIXED_H, PAGE, OK));
    let run = start(
        hp,
        HMarks {
            refused,
            still,
            served,
        },
        bsp,
    )?;
    // Once H has been refused, its budget gets the time to refill; then H goes on.
    sched::yield_until(|| run.process.reports().len() > still, tsc_hz, 10);
    let until = time::monotonic_nanos().saturating_add(REFILL_NANOS);
    sched::yield_until(|| time::monotonic_nanos() >= until, tsc_hz, 2);
    let _ = ipc::channel_write(
        &holder.handles,
        &holder.account,
        holder.identity(),
        go,
        Need::NOTHING,
        alloc::vec![0; 4],
        &[],
        Need::NOTHING,
    );
    let _ = await_end(&run.process, tsc_hz, 10);
    Some(run)
}

// ------------------------------------------------------------------ an address nobody was told

/// The report of a kernel's own map: the kernel has the address from the call itself.
fn told(_at: u64) -> Result<(), Error> {
    Ok(())
}

/// A report that fails, as the write of the address does when its page has gone since the call
/// checked it.
fn untold(_at: u64) -> Result<(), Error> {
    Err(Error::Fault)
}

/// What [`unreported`] found.
struct Unreported {
    /// The failed map left no page and no record, in a process whose budget pays for nothing.
    nothing_left: bool,
    /// Taking the mapping back was charged: the unmap the budget would have paid for must wait.
    charged: bool,
}

/// A map whose report fails, by the kernel's own calls, in two processes of one mapping each.
/// One has a budget that pays for no change at all. The other's pays for one unit, which is
/// what unmapping its one page costs in a space no processor holds: once taking the mapping
/// back has been charged, its unmap is refused until a second has passed, which these calls
/// never take.
fn unreported() -> Option<Unreported> {
    let request = Request {
        access: READ,
        flags: FIXED,
        offset: 0,
        length: PAGE,
        address: FIXED_K,
    };
    // The process, its object, and whether its failed map left the place and its one mapping
    // free: no page is there, and the same map with a report that succeeds is served.
    let lost = |changes: Rate| {
        let process = Process::new(limits(1, 1, changes), (CODE, CODE + PAGE), 0)?;
        let object = ObjectRef::try_new(MemoryObject::create(&process.account, PAGE).ok()?).ok()?;
        let map = |report: fn(u64) -> Result<(), Error>| {
            memory_object::map(
                &process,
                &process,
                &object,
                rights::MEMORY_OBJECT,
                &request,
                report,
            )
        };
        let failed = map(untold) == Err(Error::Fault);
        let unmapped = process.memory()?.translate(FIXED_K).is_none();
        let free = failed && unmapped && map(told) == Ok(FIXED_K);
        Some((process, object, free))
    };
    let (_broke, _object, nothing_left) = lost(Rate {
        burst: 0,
        per_second: 1,
    })?;
    let (payer, _object, free) = lost(Rate {
        burst: 1,
        per_second: 1,
    })?;
    let refused = memory_object::unmap(&payer, &payer, FIXED_K, PAGE) == Err(Error::ShouldWait);
    Some(Unreported {
        nothing_left,
        charged: free && refused,
    })
}

// ------------------------------------------------------------------ costs

/// Map, protect and unmap of 16 pages through the kernel's own calls, in a process no
/// processor holds: microseconds each.
fn costs(tsc_hz: u64) -> Option<[u64; 3]> {
    const PAGES: u64 = 16;
    let k = Process::new(limits(PAGES as usize, 1, PLENTY), (CODE, CODE + PAGE), 0)?;
    let object = ObjectRef::try_new(MemoryObject::create(&k.account, PAGES * PAGE).ok()?).ok()?;
    let request = Request {
        access: READ_WRITE,
        flags: FIXED,
        offset: 0,
        length: PAGES * PAGE,
        address: FIXED_K,
    };
    let per_micro = (tsc_hz / 1_000_000).max(1);
    let t0 = cpu::rdtsc();
    let at = memory_object::map(&k, &k, &object, rights::MEMORY_OBJECT, &request, told).ok()?;
    let t1 = cpu::rdtsc();
    memory_object::protect(&k, &k, at, PAGES * PAGE, READ).ok()?;
    let t2 = cpu::rdtsc();
    memory_object::unmap(&k, &k, at, PAGES * PAGE).ok()?;
    let t3 = cpu::rdtsc();
    Some([t1 - t0, t2 - t1, t3 - t2].map(|ticks| ticks / per_micro))
}

// ------------------------------------------------------------------ a shared object's unmap

/// A ring-3 loop that reads the `u64` at `address` and counts each read in the `u64` at
/// `count`, until the `u64` after the count is 1; then it exits. It keeps its space loaded on
/// its processor, and a fault at the read ends it. Both addresses are below 2 GiB.
fn reading_loop(address: u64, count: u64) -> Vec<u8> {
    let mut p = alloc::vec![0x48, 0xC7, 0xC2]; // mov rdx, address
    p.extend_from_slice(&(address as u32).to_le_bytes());
    p.extend_from_slice(&[0x48, 0xC7, 0xC3]); // mov rbx, count
    p.extend_from_slice(&(count as u32).to_le_bytes());
    // loop:
    p.extend_from_slice(&[0x48, 0x8B, 0x02]); // mov rax, [rdx]
    p.extend_from_slice(&[0x48, 0xFF, 0x03]); // inc qword ptr [rbx]
    p.extend_from_slice(&[0x48, 0x83, 0x7B, 0x08, 0x01]); // cmp qword ptr [rbx + 8], 1
    p.extend_from_slice(&[0x75, 0xF3]); // jne loop
    // xor edi, edi; xor eax, eax; syscall (exit 0); jmp $
    p.extend_from_slice(&[0x31, 0xFF, 0x31, 0xC0, 0x0F, 0x05, 0xEB, 0xFE]);
    p
}

/// What [`shared_unmap`] found.
struct SharedUnmap {
    /// Microseconds the unmap took.
    micros: u64,
    /// The reading thread then faulted at the page.
    faulted: bool,
    /// The object's page is still mapped in its creator's space.
    kept: bool,
}

/// An object mapped in its creator's space and in a reader's, whose thread reads it in a loop
/// on `cpu`: the kernel unmaps it from the reader's space while the thread runs.
fn shared_unmap(cpu: CpuIndex, tsc_hz: u64) -> Option<SharedUnmap> {
    let owner = Process::new(limits(1, 1, PLENTY), (CODE, CODE + PAGE), 0)?;
    let object = ObjectRef::try_new(MemoryObject::create(&owner.account, PAGE).ok()?).ok()?;
    let fixed = |address| Request {
        access: READ,
        flags: FIXED,
        offset: 0,
        length: PAGE,
        address,
    };
    memory_object::map(
        &owner,
        &owner,
        &object,
        rights::MEMORY_OBJECT,
        &fixed(FIXED_K),
        told,
    )
    .ok()?;
    let image = Image::new(limits(0, 1, PLENTY), &reading_loop(READ_AT, DATA))?;
    let &(_, data) = image.pages.iter().find(|&&(at, _)| at == DATA)?;
    let reader = image.finish()?;
    memory_object::map(
        &owner,
        &reader,
        &object,
        rights::MEMORY_OBJECT,
        &fixed(READ_AT),
        told,
    )
    .ok()?;
    Process::start_thread(&reader, cpu, CODE, STACK + PAGE, 0).ok()?;
    let space = reader.memory()?;
    let reads = || space.read_u64(DATA).unwrap_or(0);
    sched::yield_until(|| reads() > 0, tsc_hz, 5);
    let per_micro = (tsc_hz / 1_000_000).max(1);
    let t0 = cpu::rdtsc();
    let unmapped = memory_object::unmap(&owner, &reader, READ_AT, PAGE);
    let micros = (cpu::rdtsc() - t0) / per_micro;
    drop(space);
    let faulted = unmapped.is_ok()
        && await_end(&reader, tsc_hz, 5)
        && matches!(
            reader.ending(),
            Some(Ending::Faulted { vector: 14, address, .. }) if address == READ_AT
        );
    if reader.ending().is_none() {
        // Still reading: the unmap did not reach it. Let it exit.
        let _ = reader.write_frame(data, 8, &1u64.to_le_bytes());
        let _ = await_end(&reader, tsc_hz, 5);
    }
    let kept = owner
        .memory()
        .is_some_and(|space| space.translate(FIXED_K).is_some());
    Some(SharedUnmap {
        micros,
        faulted,
        kept,
    })
}

// ------------------------------------------------------------------ the proof

/// Each line ends `: memory failed` unless all of it holds.
fn verdict(ok: bool) -> &'static str {
    if ok { "" } else { ": memory failed" }
}

/// Every part, run and checked.
fn parts(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let (Some((a, b)), Some(q), Some((c, t)), Some(hp)) = (
        sharing(cpus, bsp, tsc_hz),
        quotas(bsp, tsc_hz),
        into_another(bsp, tsc_hz),
        hostile_loop(bsp, tsc_hz),
    ) else {
        let _ = writeln!(
            com1,
            "origin-kernel: memory: the proof's programs could not be built or started{}",
            verdict(false)
        );
        return false;
    };

    // Each detail reads only the reports it is about; the summary line after them checks
    // every other call and how each program ended.
    let shared = a.returned(a.marks.map, OK)
        && b.returned(b.marks.map, OK)
        && b.word(b.marks.seen_a) == Some(VALUE_A)
        && a.word(a.marks.seen_b) == Some(VALUE_B);
    let write_fault = a.returned(a.marks.protect, OK)
        && a.returned(a.marks.protect + 1, Error::AccessDenied.raw())
        && a.word(a.marks.readable) == Some(VALUE_A)
        && a.faulted_at(FIXED_A);
    let read_fault = b.returned(b.marks.unmap, OK) && b.faulted_at(FIXED_B);
    let _ = writeln!(
        com1,
        "origin-kernel: memory: two processes share an object by passing its handle, and each reads what the other wrote: {shared}; protect to read-only refuses an increase and makes a write fault at the mapping: {write_fault}; unmap makes a read fault there: {read_fault}{}",
        verdict(shared && write_fault && read_fault)
    );

    let reports = b.process.reports();
    let placed = [
        reported_u64(&reports, b.marks.random + 2),
        reported_u64(&reports, b.marks.random + 4),
    ];
    let maps_ok = b.returned(b.marks.random, OK) && b.returned(b.marks.random + 1, OK);
    let random = maps_ok
        && match placed {
            [Some(one), Some(two)] => {
                one != two
                    && [one, two]
                        .iter()
                        .all(|&at| at % PAGE == 0 && (PLACE_LOW..PLACE_HIGH).contains(&at))
            }
            _ => false,
        };
    let no_replace =
        b.returned(b.marks.over, Error::InvalidArgs.raw()) && b.word(b.marks.kept) == Some(VALUE_A);
    let no_execute = b.returned(b.marks.execute, Error::AccessDenied.raw());
    let by_rights = b.returned(b.marks.by_rights, Error::AccessDenied.raw())
        && b.returned(b.marks.by_rights + 1, Error::AccessDenied.raw());
    let kept = b.returned(b.marks.close, OK) && b.word(b.marks.close + 1) == Some(VALUE_A);
    let _ = writeln!(
        com1,
        "origin-kernel: memory: a fixed address over a mapping is refused, and the mapping keeps what was written to it: {no_replace}; execute is refused: {no_execute}; a handle without write maps nothing writable, and one without read maps nothing: {by_rights}; two random placements are apart, page-aligned and in their region: {random}; closing the handle leaves the mapping working: {kept}{}",
        verdict(no_replace && no_execute && by_rights && random && kept)
    );

    // Every call of Q's is about its quota.
    let quota = q.statuses() && q.exited();
    // Its objects went with its handles and mappings, and their charge with them.
    let owes_nothing = q.process.account.committed() == 0;
    let _ = writeln!(
        com1,
        "origin-kernel: memory: the quota refuses an object past its bound and takes one again once an object is closed; a closed object still mapped keeps its charge; a third mapping past the limit of two is refused: {quota}; once the process has ended its account owes nothing: {owes_nothing}{}",
        verdict(quota && owes_nothing)
    );

    let denied = Error::AccessDenied.raw();
    let cross = c.returned(c.marks.map, denied)
        && c.returned(c.marks.map + 1, OK)
        && t.word(t.marks) == Some(VALUE_C);
    let cross_unmap = c.returned(c.marks.unmap, denied)
        && c.returned(c.marks.unmap + 1, OK)
        && t.faulted_at(FIXED_T);
    let _ = writeln!(
        com1,
        "origin-kernel: memory: mapping into another process needs write on its handle, and with it the two share the frame: {cross}; unmapping there needs it too, and the other's next read faults: {cross_unmap}{}",
        verdict(cross && cross_unmap)
    );

    let refused = hp.returned(hp.marks.refused, Error::ShouldWait.raw())
        && hp.word(hp.marks.still) == Some(0);
    let served = hp.returned(hp.marks.served, OK) && hp.exited();
    let _ = writeln!(
        com1,
        "origin-kernel: memory: a hostile unmap loop is refused with should_wait once its budget is spent, changing nothing: {refused}; it is served again once the budget refills: {served}{}",
        verdict(refused && served)
    );

    let Some(unreported) = unreported() else {
        let _ = writeln!(
            com1,
            "origin-kernel: memory: the maps whose report fails could not be set up{}",
            verdict(false)
        );
        return false;
    };
    let (nothing_left, charged) = (unreported.nothing_left, unreported.charged);
    let _ = writeln!(
        com1,
        "origin-kernel: memory: a map whose address cannot be written to its caller leaves no page and no record, though the caller's budget pays for nothing: {nothing_left}; what taking it back cost is charged, and the caller's next unmap must wait: {charged}{}",
        verdict(nothing_left && charged)
    );

    let scripts = a.statuses()
        && b.statuses()
        && q.statuses()
        && c.statuses()
        && t.statuses()
        && hp.statuses()
        && a.faulted_at(FIXED_A)
        && b.faulted_at(FIXED_B)
        && t.faulted_at(FIXED_T)
        && q.exited()
        && c.exited()
        && hp.exited();
    let _ = writeln!(
        com1,
        "origin-kernel: memory: every other call of the proof's programs returned what its script says, and each program ended as it should: {scripts}{}",
        verdict(scripts)
    );

    match costs(tsc_hz) {
        Some([map, protect, unmap]) => {
            let _ = writeln!(
                com1,
                "origin-kernel: memory: map, protect and unmap of 16 pages took {map}, {protect} and {unmap} us (QEMU; the kernel's own calls, in a space no processor holds)"
            );
        }
        None => {
            let _ = writeln!(
                com1,
                "origin-kernel: memory: the kernel's own map, protect and unmap failed{}",
                verdict(false)
            );
            return false;
        }
    }

    let far = cpus.iter().copied().find(|&cpu| cpu != bsp).unwrap_or(bsp);
    let Some(unmap) = shared_unmap(far, tsc_hz) else {
        let _ = writeln!(
            com1,
            "origin-kernel: memory: the shared object's reader could not be set up{}",
            verdict(false)
        );
        return false;
    };
    let _ = writeln!(
        com1,
        "origin-kernel: memory: unmapping a page of a shared object from a space a thread is reading it in took {} us (QEMU, no budget); the thread then faulted at the page: {}; the object stays mapped in its creator's space: {}{}",
        unmap.micros,
        unmap.faulted,
        unmap.kept,
        verdict(unmap.faulted && unmap.kept)
    );
    unmap.faulted
        && unmap.kept
        && scripts
        && shared
        && write_fault
        && read_fault
        && no_replace
        && no_execute
        && by_rights
        && random
        && kept
        && quota
        && owes_nothing
        && cross
        && cross_unmap
        && refused
        && served
        && nothing_left
        && charged
}

/// The memory-object proof (ADR-0005 P7, P11, C6–C8), with a leak check of its own. Called from
/// the boot flow with every scheduler running, after [`super::prove_directory`].
pub(crate) fn prove_memory(
    com1: &mut Console,
    cpus: &[CpuIndex],
    bsp: CpuIndex,
    tsc_hz: u64,
) -> bool {
    let before = memory::stats();
    let ok = parts(com1, cpus, bsp, tsc_hz);
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
        "origin-kernel: memory: every process, object and mapping of the proof is gone: heap {heap_now} bytes (was {heap_before}), {frames_now} free frames (was {frames_before}){}",
        verdict(back)
    );
    ok && back
}
