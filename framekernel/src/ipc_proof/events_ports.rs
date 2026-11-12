//! Boot proofs of events, ports and the clock as ring 3 reads it (ADR-0005 C9, C10, C13 as
//! Amendment 7 decides them; task S3-6). Ring-3 programs make the calls, and the kernel checks
//! what each saw and how each ended.
//!
//! **Events and ports** (`prove_events_ports`, a failure stops the boot):
//! - **Events:** an event starts with nothing raised; `object_signal` raises and clears
//!   `SIGNALED`, a signal in both masks ends set, and `wait_one` sees each state.
//! - **Bind and deliver:** a binding on an event, and on a channel end, becomes one packet when
//!   its signal is raised, and a signal already raised queues it at once; the same binding made
//!   twice makes two packets; packets leave in the order they were queued.
//! - **Explicit cancel:** `port_cancel` removes an armed binding, and a queued packet, silently;
//!   a second cancel, or one that matches nothing, is `not_found`.
//! - **Implicit cancel follows the handle:** closing the handle a binding was made through queues
//!   one `canceled` packet, and closing another duplicate cancels nothing; a handle moved by a
//!   write cancels its binding the same way; a signal packet already queued stays when its handle
//!   closes, and no `canceled` packet follows it.
//! - **Hostile calls:** a table for each call, refused with the status Amendment 7 gives.
//! - **Quotas:** events, ports and bindings stop at their quotas; closing a port's last handle
//!   gives back the charges of every binding armed for it; a process given no quota gets nothing.
//! - **A wait through a closed handle** ends `canceled` while a duplicate keeps the port open.
//! - **A binder that exits** while another process holds the port leaves its bindings as
//!   `canceled` packets, still charged to it until the holder reads them.
//! - **Costs:** signal, `port_wait` and a fresh bind between two processes, round trips, through
//!   the script interpreter; and bind, signal and take through the kernel's own calls.
//! - **Cleanup:** the heap and the frame allocator end where they started.
//!
//! **The clock** (`prove_user_clock`, reported): ring 3 reads `clock_read` many times; every
//! value lies between what the kernel read before and after, none is less than the one before,
//! a wait until a deadline taken on the same clock ends after it, and a bad out pointer is
//! `fault`.

use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::Ordering;

use origin_abi::limits::DEADLINE_INFINITE;
use origin_abi::{Error, OK, ObjectType, packet_kind, rights, signals, syscall as sys};
use origin_frame::objref::ObjectRef;
use origin_frame::proof_programs::{self, DATA, SCRIPT, STACK};

use super::Arg::Imm;
use super::{
    Arg, CODE, Image, LIMITS, LIST, MSG, OBS, SECOND_SCRIPT, STACK_1, Script, Step, await_end,
    close, create, dup, h, reported_u64, send, slot_addr, statuses_right, step, value_at, wait,
    write, write_slot,
};
use crate::console::Console;
use crate::cpu::CpuIndex;
use crate::event::Event;
use crate::handles::Need;
use crate::ipc::{self, Object};
use crate::paging::{Access, PAGE};
use crate::port;
use crate::process::{Ending, Limits, ObjectLimits, Process};
use crate::{memory, sched, time};

/// Where `port_wait` writes the packet.
const PKT: u64 = DATA + 0x480;
/// Where the clock's readings go, 8 bytes each.
const CLK: u64 = DATA + 0x800;
/// A kernel address: no out pointer may reach it.
const KERNEL: u64 = 0xFFFF_8000_0000_1000;
/// A user address nothing is mapped at.
const UNMAPPED: u64 = 0x10_0000;
/// A handle value no table holds.
const BAD: Arg = Imm(0x7777_0042);
const SIG: u32 = signals::SIGNALED;
const EVENT: u32 = rights::EVENT;

fn ev_create(out: u64, want: i64) -> Step {
    step("", sys::EVENT_CREATE, &[Imm(out)], want)
}

fn port_create(out: u64, want: i64) -> Step {
    step("", sys::PORT_CREATE, &[Imm(out)], want)
}

fn signal(x: Arg, clear: u32, set: u32, want: i64) -> Step {
    step(
        "",
        sys::OBJECT_SIGNAL,
        &[x, Imm(u64::from(clear)), Imm(u64::from(set))],
        want,
    )
}

fn bind(port: Arg, object: Arg, key: u64, mask: u32, want: i64) -> Step {
    step(
        "",
        sys::OBJECT_WAIT_ASYNC,
        &[port, object, Imm(key), Imm(u64::from(mask))],
        want,
    )
}

fn take(port: Arg, deadline: u64, out: u64, want: i64) -> Step {
    step("", sys::PORT_WAIT, &[port, Imm(deadline), Imm(out)], want)
}

fn cancel(port: Arg, object: Arg, key: u64, want: i64) -> Step {
    step("", sys::PORT_CANCEL, &[port, object, Imm(key)], want)
}

fn clock(out: u64, want: i64) -> Step {
    step("", sys::CLOCK_READ, &[Imm(out)], want)
}

/// A packet a script must have taken: where its words are among the reports, and what they must
/// say. `full` packets report all eight words, the others the key's low word, the kind, the
/// trigger and the observed signals.
#[derive(Clone, Copy)]
struct Packet {
    at: usize,
    full: bool,
    key: u64,
    kind: u32,
    trigger: u32,
    /// The observed signals must hold these...
    observed: u32,
    /// ...and, when `exact`, nothing else.
    exact: bool,
}

/// A script being written, with the packets it must see.
#[derive(Default)]
struct Written {
    script: Script,
    packets: Vec<Packet>,
}

impl Written {
    fn call(&mut self, step: Step) {
        self.script.call(step);
    }

    /// A `port_wait` on `port` that must return a packet with these fields at once.
    fn packet(&mut self, port: Arg, key: u64, kind: u32, trigger: u32, observed: u32) {
        self.packet_with(port, key, kind, trigger, observed, true, false);
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "each is one field the packet must have, or how to compare it"
    )]
    fn packet_with(
        &mut self,
        port: Arg,
        key: u64,
        kind: u32,
        trigger: u32,
        observed: u32,
        exact: bool,
        full: bool,
    ) {
        self.call(take(port, 0, PKT, OK));
        let at = self.script.reported();
        let words: &[u64] = if full {
            &[0, 4, 8, 12, 16, 20, 24, 28]
        } else {
            &[0, 8, 12, 16]
        };
        for &offset in words {
            self.call(value_at("", PKT + offset, 0));
        }
        self.packets.push(Packet {
            at,
            full,
            key,
            kind,
            trigger,
            observed,
            exact,
        });
    }

    /// A `port_wait` on `port` that must find nothing queued.
    fn nothing(&mut self, port: Arg) {
        self.call(take(port, 0, PKT, Error::TimedOut.raw()));
    }
}

/// Whether every packet in `packets` was reported as it must be.
fn packets_right(packets: &[Packet], reports: &[u64]) -> bool {
    let word = |at: usize| reports.get(at).map(|&v| v as u32);
    packets.iter().all(|p| {
        let fields = if p.full {
            (|| {
                let key = reported_u64(reports, p.at)?;
                let kind = word(p.at + 2)?;
                let trigger = word(p.at + 3)?;
                let observed = word(p.at + 4)?;
                let reserved = word(p.at + 5)?;
                let data = reported_u64(reports, p.at + 6)?;
                Some((key, kind, trigger, observed, reserved == 0 && data == 0))
            })()
        } else {
            (|| {
                let key = u64::from(word(p.at)?);
                Some((key, word(p.at + 1)?, word(p.at + 2)?, word(p.at + 3)?, true))
            })()
        };
        fields.is_some_and(|(key, kind, trigger, observed, zero)| {
            let seen = if p.exact {
                observed == p.observed
            } else {
                observed & p.observed == p.observed
            };
            key == p.key && kind == p.kind && trigger == p.trigger && seen && zero
        })
    })
}

/// A scripted process being put together.
struct Program {
    image: Image,
    written: Written,
}

impl Program {
    fn new(limits: Limits) -> Option<Self> {
        let image = Image::new(limits, proof_programs::script())?;
        image.scratch()?;
        Some(Self {
            image,
            written: Written::default(),
        })
    }

    /// Grants `object` with `rights` before the process starts: its handle, as an argument.
    fn grant(&self, object: Object, rights: u32) -> Option<Arg> {
        let handle = self.image.process.grant(object, rights).ok()?;
        Some(Imm(handle))
    }

    /// The process, its script written; its own handle goes in slot 0.
    fn finish(mut self) -> Option<(ObjectRef<Process>, Vec<Step>, Vec<Packet>)> {
        self.image.script(&self.written.script.steps)?;
        let process = self.image.finish()?;
        let own = process
            .grant(Object::Process(process.clone()), rights::PROCESS)
            .ok()?;
        write_slot(&process, 0, own).then_some(())?;
        Some((process, self.written.script.steps, self.written.packets))
    }
}

/// A finished program, started on `cpu`.
fn started(
    program: Program,
    cpu: CpuIndex,
) -> Option<(ObjectRef<Process>, Vec<Step>, Vec<Packet>)> {
    let (process, steps, packets) = program.finish()?;
    Process::start_thread(&process, cpu, CODE, STACK + PAGE, SCRIPT).ok()?;
    Some((process, steps, packets))
}

/// The limits of a process given `events`, `ports` and `bindings`.
fn objects(events: usize, ports: usize, bindings: usize) -> Limits {
    Limits {
        objects: ObjectLimits {
            events,
            ports,
            bindings,
        },
        ..LIMITS
    }
}

fn verdict(ok: bool) -> &'static str {
    if ok { "" } else { ": events and ports failed" }
}

// ------------------------------------------------------------------ one process, every rule

// A process's report log keeps 128 values (`process.rs`), so the rules one process can show by
// itself run in three processes, each with three events, two ports and four bindings. Each
// builder returns where its groups start in its script.

/// What the second and third start with: event A in slot 1, port P in slot 2, a channel in slots
/// 3 and 4.
fn preamble(w: &mut Written) {
    w.call(ev_create(slot_addr(1), OK));
    w.call(port_create(slot_addr(2), OK));
    w.call(create(3));
}

/// The first: events, then bind and deliver.
fn events_and_delivery(w: &mut Written) -> Vec<usize> {
    let (a, p, c0, c1, a_dup) = (h(1), h(2), h(3), h(4), h(5));
    let events = w.script.steps.len();
    w.call(ev_create(slot_addr(1), OK));
    // An event starts with nothing raised (F17).
    w.call(wait("", a, SIG, 0, Error::TimedOut.raw()));
    w.call(value_at("", OBS, 0));
    w.call(signal(a, 0, SIG, OK));
    w.call(wait("", a, SIG, 0, OK));
    w.call(signal(a, SIG, SIG, OK));
    w.call(wait("", a, SIG, 0, OK));
    w.call(signal(a, SIG, 0, OK));
    w.call(wait("", a, SIG, 0, Error::TimedOut.raw()));
    w.call(signal(a, 0, 0, OK));
    w.call(wait("", a, SIG, 0, Error::TimedOut.raw()));
    // Hostile: only SIGNALED, and only on an event, with `signal` (F5).
    w.call(signal(a, 0, signals::READABLE, Error::InvalidArgs.raw()));
    w.call(signal(a, 1 << 20, 0, Error::InvalidArgs.raw()));
    w.call(signal(h(0), 0, SIG, Error::AccessDenied.raw()));
    w.call(signal(BAD, 0, SIG, Error::BadHandle.raw()));
    w.call(dup("", a, rights::WAIT | rights::DUPLICATE, 5, OK));
    w.call(signal(a_dup, 0, SIG, Error::AccessDenied.raw()));
    w.call(close("", a_dup, OK));
    w.call(ev_create(KERNEL, Error::Fault.raw()));
    w.call(ev_create(SCRIPT, Error::Fault.raw()));

    let deliver = w.script.steps.len();
    w.call(port_create(slot_addr(2), OK));
    w.nothing(p);
    // Armed, then delivered: one binding, one packet (C9, F2).
    w.call(bind(p, a, 1, SIG, OK));
    w.nothing(p);
    w.call(signal(a, 0, SIG, OK));
    w.packet_with(p, 1, packet_kind::SIGNAL, SIG, SIG, true, true);
    w.nothing(p);
    // A signal already raised queues the packet before the call returns (F2).
    w.call(bind(p, a, 2, SIG, OK));
    w.packet(p, 2, packet_kind::SIGNAL, SIG, SIG);
    // The same binding twice: two packets (F9).
    w.call(bind(p, a, 3, SIG, OK));
    w.call(bind(p, a, 3, SIG, OK));
    w.packet(p, 3, packet_kind::SIGNAL, SIG, SIG);
    w.packet(p, 3, packet_kind::SIGNAL, SIG, SIG);
    w.nothing(p);
    // A channel end (F18).
    w.call(create(3));
    w.call(bind(p, c0, 4, signals::READABLE, OK));
    w.call(write("", c1, MSG, 4, OK));
    w.packet_with(
        p,
        4,
        packet_kind::SIGNAL,
        signals::READABLE,
        signals::READABLE,
        false,
        false,
    );
    // Packets leave in the order they were queued (F15).
    w.call(signal(a, SIG, 0, OK));
    w.call(bind(p, a, 5, SIG, OK));
    w.call(bind(p, c0, 6, signals::PEER_CLOSED, OK));
    w.call(signal(a, 0, SIG, OK));
    w.call(close("", c1, OK));
    w.packet(p, 5, packet_kind::SIGNAL, SIG, SIG);
    w.packet_with(
        p,
        6,
        packet_kind::SIGNAL,
        signals::PEER_CLOSED,
        signals::PEER_CLOSED,
        false,
        false,
    );
    w.nothing(p);

    alloc::vec![events, deliver]
}

/// The second: the explicit cancel and the implicit one.
fn cancels(w: &mut Written) -> Vec<usize> {
    let (a, p, c0, a_dup) = (h(1), h(2), h(3), h(5));
    preamble(w);
    // Explicit cancel: an armed binding and a queued packet, silently (F4).
    let explicit = w.script.steps.len();
    w.call(signal(a, SIG, 0, OK));
    w.call(bind(p, a, 7, SIG, OK));
    w.call(cancel(p, a, 7, OK));
    w.call(cancel(p, a, 7, Error::NotFound.raw()));
    w.call(signal(a, 0, SIG, OK));
    w.nothing(p);
    w.call(bind(p, a, 8, SIG, OK));
    w.call(cancel(p, a, 8, OK));
    w.nothing(p);
    w.call(cancel(p, h(0), 8, Error::NotFound.raw()));
    w.call(cancel(p, c0, 8, Error::NotFound.raw()));

    // Implicit cancel follows the handle (F16).
    let implicit = w.script.steps.len();
    w.call(signal(a, SIG, 0, OK));
    w.call(dup("", a, EVENT, 5, OK));
    w.call(bind(p, a_dup, 9, SIG, OK));
    w.call(bind(p, a, 10, SIG, OK));
    w.call(close("", a_dup, OK));
    w.packet(p, 9, packet_kind::CANCELED, SIG, 0);
    w.nothing(p);
    w.call(signal(a, 0, SIG, OK));
    w.packet(p, 10, packet_kind::SIGNAL, SIG, SIG);
    // Moved by a write: the binding does not travel with the handle.
    w.call(signal(a, SIG, 0, OK));
    w.call(dup("", a, EVENT, 11, OK));
    w.call(bind(p, h(11), 11, SIG, OK));
    w.call(create(12));
    w.call(super::copy_handle(slot_addr(11), LIST));
    w.call(send("", h(12), 1, OK));
    w.packet(p, 11, packet_kind::CANCELED, SIG, 0);
    w.call(close("", h(12), OK));
    w.call(close("", h(13), OK));
    // A signal packet already queued stays when its handle closes, and nothing follows it.
    w.call(dup("", a, EVENT, 5, OK));
    w.call(bind(p, a_dup, 12, SIG, OK));
    w.call(signal(a, 0, SIG, OK));
    w.call(close("", a_dup, OK));
    w.packet(p, 12, packet_kind::SIGNAL, SIG, SIG);
    w.nothing(p);

    alloc::vec![explicit, implicit]
}

/// The third: hostile calls and quotas.
fn refusals(w: &mut Written) -> Vec<usize> {
    let (a, p) = (h(1), h(2));
    preamble(w);
    // Hostile calls: each refused with the first failure in D5's order.
    let hostile = w.script.steps.len();
    let (read_only, write_only) = (h(6), h(7));
    w.call(bind(p, a, 13, 0, Error::InvalidArgs.raw()));
    w.call(bind(p, a, 13, 1 << 20, Error::InvalidArgs.raw()));
    w.call(dup("", p, rights::READ, 6, OK));
    w.call(dup("", p, rights::WRITE, 7, OK));
    w.call(bind(read_only, a, 13, SIG, Error::AccessDenied.raw()));
    w.call(bind(a, a, 13, SIG, Error::WrongType.raw()));
    w.call(bind(p, p, 13, SIG, Error::AccessDenied.raw()));
    w.call(bind(BAD, a, 13, SIG, Error::BadHandle.raw()));
    w.call(bind(p, BAD, 13, SIG, Error::BadHandle.raw()));
    w.call(take(write_only, 0, PKT, Error::AccessDenied.raw()));
    w.call(take(a, 0, PKT, Error::WrongType.raw()));
    w.call(take(BAD, 0, PKT, Error::BadHandle.raw()));
    w.call(take(p, 0, KERNEL, Error::Fault.raw()));
    w.call(take(p, 0, SCRIPT, Error::Fault.raw()));
    w.call(cancel(read_only, a, 13, Error::AccessDenied.raw()));
    w.call(cancel(a, a, 13, Error::WrongType.raw()));
    w.call(cancel(p, BAD, 13, Error::BadHandle.raw()));
    // A port raises no signals and has no `wait` (F8).
    w.call(wait("", p, signals::READABLE, 0, Error::AccessDenied.raw()));
    w.call(port_create(KERNEL, Error::Fault.raw()));
    w.nothing(p);

    // Quotas: three events, two ports, four bindings (F7).
    let quotas = w.script.steps.len();
    let p2 = h(10);
    w.call(ev_create(slot_addr(8), OK));
    w.call(ev_create(slot_addr(9), OK));
    w.call(ev_create(slot_addr(14), Error::Limit.raw()));
    w.call(port_create(slot_addr(10), OK));
    w.call(port_create(slot_addr(15), Error::Limit.raw()));
    w.call(signal(a, SIG, 0, OK));
    for _ in 0..4 {
        w.call(bind(p2, a, 14, SIG, OK));
    }
    w.call(bind(p, a, 15, SIG, Error::Limit.raw()));
    // The port's last handle goes, and with it every binding armed for it and its charge (F11).
    w.call(close("", p2, OK));
    w.call(bind(p, a, 15, SIG, OK));
    w.call(signal(a, 0, SIG, OK));
    w.packet(p, 15, packet_kind::SIGNAL, SIG, SIG);
    w.nothing(p);
    alloc::vec![hostile, quotas]
}

/// Whether the calls of `steps[from..to]` returned what they must.
fn group_right(steps: &[Step], reports: &[u64], from: usize, to: usize) -> bool {
    // The reports before `from` are those of the steps before it.
    let skip = steps
        .get(..from)
        .map_or(0, |s| s.iter().filter(|s| reports_one(s)).count());
    let Some(group) = steps.get(from..to) else {
        return false;
    };
    let count = group.iter().filter(|s| reports_one(s)).count();
    reports
        .get(skip..skip + count)
        .is_some_and(|mine| statuses_right(group, mine))
}

/// Whether `step` makes a report.
fn reports_one(step: &Step) -> bool {
    !matches!(step.want, super::Want::Nothing)
}

/// The first call among `steps` that did not return its status: its place in the script, its
/// number, what it returned and what it had to. For the line a failure prints.
fn first_wrong(steps: &[Step], reports: &[u64]) -> Option<(usize, u64, i64, i64)> {
    let mut at = 0;
    for (i, s) in steps.iter().enumerate() {
        if !reports_one(s) {
            continue;
        }
        let got = reports.get(at).map_or(i64::MIN, |&v| v as i64);
        at += 1;
        if let super::Want::Status(want) = s.want
            && got != want
        {
            return Some((i, s.number, got, want));
        }
    }
    None
}

/// The first packet in `packets` that was not reported as it must be: its key, and the words
/// reported for it. For the line a failure prints.
fn first_wrong_packet(packets: &[Packet], reports: &[u64]) -> Option<(u64, Vec<u64>)> {
    packets
        .iter()
        .find(|p| !packets_right(core::slice::from_ref(p), reports))
        .map(|p| {
            let words = if p.full { 8 } else { 4 };
            let seen = reports.get(p.at..p.at + words).unwrap_or(&[]).to_vec();
            (p.key, seen)
        })
}

/// What one rules process showed.
struct Ran {
    /// Each group's calls returned what they must.
    groups: Vec<bool>,
    /// Each packet it took was what was bound and seen.
    packets: bool,
    /// Every call returned what it must, and the process exited by itself.
    rest: bool,
}

/// Runs one rules process, its script from `build`, and checks it group by group. A call that
/// returned something else, or a packet that was not what it must be, is named on a line.
fn run_rules(
    com1: &mut Console,
    bsp: CpuIndex,
    tsc_hz: u64,
    name: &str,
    build: fn(&mut Written) -> Vec<usize>,
) -> Ran {
    let failed = Ran {
        groups: Vec::new(),
        packets: false,
        rest: false,
    };
    let Some(mut e) = Program::new(objects(3, 2, 4)) else {
        return failed;
    };
    let starts = build(&mut e.written);
    let Some((process, steps, packets)) = started(e, bsp) else {
        let _ = writeln!(
            com1,
            "origin-kernel: events: a rules process could not be set up{}",
            verdict(false)
        );
        return failed;
    };
    let ended = await_end(&process, tsc_hz, 10) && process.ending() == Some(Ending::Exited(0));
    let reports = process.reports();
    let ends = starts.iter().skip(1).copied().chain([steps.len()]);
    let groups = starts
        .iter()
        .zip(ends)
        .map(|(&from, to)| group_right(&steps, &reports, from, to))
        .collect();
    if let Some((i, number, got, want)) = first_wrong(&steps, &reports) {
        let _ = writeln!(
            com1,
            "origin-kernel: events: the {name} rules process: step {i} (call {number:#x}) returned {got}, not {want}{}",
            verdict(false)
        );
    }
    if let Some((key, seen)) = first_wrong_packet(&packets, &reports) {
        let _ = writeln!(
            com1,
            "origin-kernel: events: the {name} rules process: the packet with key {key} was reported as {seen:x?}{}",
            verdict(false)
        );
    }
    Ran {
        groups,
        packets: packets_right(&packets, &reports),
        rest: statuses_right(&steps, &reports) && ended,
    }
}

/// Every rule one process can show by itself, in three processes.
fn rules(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let first = run_rules(com1, bsp, tsc_hz, "first", events_and_delivery);
    let second = run_rules(com1, bsp, tsc_hz, "second", cancels);
    let third = run_rules(com1, bsp, tsc_hz, "third", refusals);
    let at = |ran: &Ran, i: usize| ran.groups.get(i).copied().unwrap_or(false);
    let (events, deliver) = (at(&first, 0), at(&first, 1));
    let (explicit, implicit) = (at(&second, 0), at(&second, 1));
    let (hostile, quotas) = (at(&third, 0), at(&third, 1));
    let packets = first.packets && second.packets && third.packets;
    let rest = first.rest && second.rest && third.rest;
    let _ = writeln!(
        com1,
        "origin-kernel: events: an event starts with nothing raised; object_signal raises and clears SIGNALED, a signal in both masks ends set, and wait_one sees each state: {events}; a mask with another signal, a handle without signal and a bad pointer are refused: {events}"
    );
    let _ = writeln!(
        com1,
        "origin-kernel: ports: a binding on an event and on a channel end becomes one packet when its signal is raised, at once if it already is, twice if made twice, in the order queued: {deliver}; each packet is what was bound and seen: {packets}"
    );
    let _ = writeln!(
        com1,
        "origin-kernel: ports: port_cancel removes an armed binding and a queued packet silently, then not_found: {explicit}; closing the handle a binding was made through queues one canceled packet, another duplicate's close cancels nothing, a move cancels, and a queued signal packet stays with nothing after it: {implicit}"
    );
    let ok = events && deliver && explicit && implicit && hostile && quotas && packets && rest;
    let _ = writeln!(
        com1,
        "origin-kernel: ports: hostile binds, waits and cancels are refused in D5's order, and a port cannot be waited on: {hostile}; events, ports and bindings stop at their quotas, and closing a port gives back its bindings' charges: {quotas}; every other call returned what it must, and the three processes ended: {rest}{}",
        verdict(ok)
    );
    ok
}

// ------------------------------------------------------------------ across threads and processes

/// A process given no quota gets nothing (D4).
fn no_quota(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let Some(mut n) = Program::new(LIMITS) else {
        return false;
    };
    n.written.call(ev_create(slot_addr(1), Error::Limit.raw()));
    n.written
        .call(port_create(slot_addr(2), Error::Limit.raw()));
    let Some((process, steps, _)) = started(n, bsp) else {
        return false;
    };
    let ok = await_end(&process, tsc_hz, 5)
        && process.ending() == Some(Ending::Exited(0))
        && statuses_right(&steps, &process.reports());
    let _ = writeln!(
        com1,
        "origin-kernel: ports: a process given no quota can make no event and no port: {ok}{}",
        verdict(ok)
    );
    ok
}

/// A thread in `port_wait` ends `canceled` when another thread closes the handle it waits
/// through, while a duplicate keeps the port open (F3).
fn canceled_wait(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let set_up = |com1: &mut Console| {
        let _ = writeln!(
            com1,
            "origin-kernel: ports: the canceled wait's process could not be set up{}",
            verdict(false)
        );
        false
    };
    let Some(mut image) = Image::new(objects(0, 1, 0), proof_programs::script()) else {
        return set_up(com1);
    };
    let Ok(port) = port::create(&image.process.account) else {
        return set_up(com1);
    };
    let (Ok(waited), Ok(kept)) = (
        image
            .process
            .grant(Object::Port(port.clone()), rights::PORT),
        image.process.grant(Object::Port(port), rights::PORT),
    ) else {
        return set_up(com1);
    };
    let (waited, kept) = (Imm(waited), Imm(kept));
    let first = alloc::vec![take(waited, DEADLINE_INFINITE, PKT, Error::Canceled.raw())];
    let second = alloc::vec![
        close("", waited, OK),
        take(kept, 0, PKT, Error::TimedOut.raw()),
    ];
    // The script's pages are mapped by the first script, then the second goes on them.
    let built = image.map(STACK_1, 1, Access::ReadWrite).is_some()
        && image.scratch().is_some()
        && image.script(&first).is_some()
        && image.write_script(SECOND_SCRIPT, &second).is_some();
    let Some(process) = built.then(|| image.finish()).flatten() else {
        return set_up(com1);
    };
    if Process::start_thread(&process, bsp, CODE, STACK + PAGE, SCRIPT).is_err() {
        return set_up(com1);
    }
    // The closing thread starts only once the waiter is in `port_wait`: whichever comes first,
    // its block or the close, the wait ends `canceled`.
    sched::yield_until(|| process.waiting.load(Ordering::Acquire) > 0, tsc_hz, 5);
    let in_wait = process.waiting.load(Ordering::Acquire) > 0;
    if Process::start_thread(&process, far, CODE, STACK_1 + PAGE, SECOND_SCRIPT).is_err() {
        return set_up(com1);
    }
    let ended = await_end(&process, tsc_hz, 10) && process.ending() == Some(Ending::Exited(0));
    let mut reports = process.reports();
    reports.sort_unstable();
    let mut wanted: Vec<u64> = first
        .iter()
        .chain(&second)
        .filter_map(|s| match s.want {
            super::Want::Status(status) => Some(status as u64),
            _ => None,
        })
        .collect();
    wanted.sort_unstable();
    let ok = in_wait && ended && reports == wanted;
    let _ = writeln!(
        com1,
        "origin-kernel: ports: a thread in port_wait through a handle another thread closes ends canceled, while a duplicate keeps the port open and empty: {ok}{}",
        verdict(ok)
    );
    ok
}

/// A binder that exits while another process holds the port (F11): its bindings become
/// `canceled` packets, still charged to it until the holder reads them.
fn binder_exits(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let (Some(mut holder), Some(mut binder)) = (
        Program::new(objects(2, 1, 0)),
        Program::new(objects(0, 0, 2)),
    ) else {
        return false;
    };
    let account = holder.image.process.account.clone();
    let (Ok(port), Ok(watched), Ok(go)) = (
        port::create(&account),
        Event::create(&account),
        Event::create(&account),
    ) else {
        return false;
    };
    let (Some(q_h), Some(go_h), Some(q_b), Some(z_b)) = (
        holder.grant(Object::Port(port.clone()), rights::PORT),
        holder.grant(Object::Event(go.clone()), EVENT),
        binder.grant(Object::Port(port), rights::PORT),
        binder.grant(Object::Event(watched), EVENT),
    ) else {
        return false;
    };
    binder.written.call(bind(q_b, z_b, 21, SIG, OK));
    binder.written.call(bind(q_b, z_b, 22, SIG, OK));
    // The holder reads only once the binder has ended and the kernel has looked.
    holder
        .written
        .call(wait("", go_h, SIG, DEADLINE_INFINITE, OK));
    holder
        .written
        .packet(q_h, 21, packet_kind::CANCELED, SIG, 0);
    holder
        .written
        .packet(q_h, 22, packet_kind::CANCELED, SIG, 0);
    holder.written.nothing(q_h);
    let Some((b, b_steps, _)) = started(binder, bsp) else {
        return false;
    };
    let Some((hp, h_steps, h_packets)) = started(holder, bsp) else {
        return false;
    };
    let b_ended = await_end(&b, tsc_hz, 5) && b.ending() == Some(Ending::Exited(0));
    let held_after_exit = b.account.objects().bindings.held();
    let _ = go.signal(0, SIG);
    let h_ended = await_end(&hp, tsc_hz, 5) && hp.ending() == Some(Ending::Exited(0));
    let held_after_read = b.account.objects().bindings.held();
    let h_reports = hp.reports();
    // One exit cancels both at once: the two packets may be queued in either order.
    let swapped: Vec<Packet> = h_packets
        .iter()
        .zip(h_packets.iter().rev())
        .map(|(slot, want)| Packet {
            at: slot.at,
            ..*want
        })
        .collect();
    let both = packets_right(&h_packets, &h_reports) || packets_right(&swapped, &h_reports);
    let ok = b_ended
        && h_ended
        && statuses_right(&b_steps, &b.reports())
        && statuses_right(&h_steps, &h_reports)
        && both
        && held_after_exit == 2
        && held_after_read == 0;
    let _ = writeln!(
        com1,
        "origin-kernel: ports: a binder that exits while another process holds the port leaves its 2 bindings as canceled packets, still charged to it ({held_after_exit} after its exit, {held_after_read} once the holder read them): {ok}{}",
        verdict(ok)
    );
    ok
}

/// Round trips of the event and port path between two processes: four reports each, and a
/// process's log keeps 128.
const ROUNDS: u64 = 30;

/// Two processes on two processors take turns, each signalling the other's event and waiting on
/// its own port, binding afresh each time: what one signal costs to reach a `port_wait`, through
/// the script interpreter (QEMU figures, no budget).
fn round_trips(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let (Some(mut w), Some(mut s)) = (
        Program::new(objects(2, 2, 2)),
        Program::new(objects(0, 0, 2)),
    ) else {
        return false;
    };
    let account = w.image.process.account.clone();
    let (Ok(x), Ok(y), Ok(pw), Ok(ps)) = (
        Event::create(&account),
        Event::create(&account),
        port::create(&account),
        port::create(&account),
    ) else {
        return false;
    };
    let (Some(wx), Some(wy), Some(wp), Some(sx), Some(sy), Some(sp)) = (
        w.grant(Object::Event(x.clone()), EVENT),
        w.grant(Object::Event(y.clone()), EVENT),
        w.grant(Object::Port(pw), rights::PORT),
        s.grant(Object::Event(x), EVENT),
        s.grant(Object::Event(y), EVENT),
        s.grant(Object::Port(ps), rights::PORT),
    ) else {
        return false;
    };
    w.written.call(bind(wp, wx, 1, SIG, OK));
    s.written.call(bind(sp, sy, 2, SIG, OK));
    s.written.call(signal(sx, 0, SIG, OK));
    for _ in 0..ROUNDS {
        w.written.call(take(wp, DEADLINE_INFINITE, PKT, OK));
        w.written.call(signal(wx, SIG, 0, OK));
        w.written.call(bind(wp, wx, 1, SIG, OK));
        w.written.call(signal(wy, 0, SIG, OK));
        s.written.call(take(sp, DEADLINE_INFINITE, PKT, OK));
        s.written.call(signal(sy, SIG, 0, OK));
        s.written.call(bind(sp, sy, 2, SIG, OK));
        s.written.call(signal(sx, 0, SIG, OK));
    }
    let Some((wp_, w_steps, _)) = started(w, far) else {
        return false;
    };
    let start = time::monotonic_nanos();
    let Some((sp_, s_steps, _)) = started(s, bsp) else {
        return false;
    };
    let ended = await_end(&wp_, tsc_hz, 10) && await_end(&sp_, tsc_hz, 10);
    let took = time::monotonic_nanos().saturating_sub(start);
    let ok = ended
        && wp_.ending() == Some(Ending::Exited(0))
        && sp_.ending() == Some(Ending::Exited(0))
        && statuses_right(&w_steps, &wp_.reports())
        && statuses_right(&s_steps, &sp_.reports());
    let _ = writeln!(
        com1,
        "origin-kernel: ports: {ROUNDS} round trips between {} and {}, each a signal, a port_wait and a fresh bind on either side, took {} us each (QEMU, through the script interpreter, no budget): {ok}{}",
        bsp.named(),
        far.named(),
        took / ROUNDS / 1000,
        verdict(ok)
    );
    ok
}

/// Binds, signals and takes through the kernel's own calls, in a process never started: what a
/// bind and its delivery cost (QEMU figures, no budget).
fn kernel_costs(com1: &mut Console) -> bool {
    const TIMES: u64 = 1000;
    let Some(image) = Image::new(objects(1, 1, 1), proof_programs::script()) else {
        return false;
    };
    let Some(process) = image.finish() else {
        return false;
    };
    let account = process.account.clone();
    let (Ok(event), Ok(made)) = (Event::create(&account), port::create(&account)) else {
        process.abandon();
        return false;
    };
    let (Ok(he), Ok(hp)) = (
        process.grant(Object::Event(event.clone()), EVENT),
        process.grant(Object::Port(made), rights::PORT),
    ) else {
        process.abandon();
        return false;
    };
    let port_need = |r| Need {
        rights: r,
        object: ObjectType::Port as u32,
    };
    let start = time::monotonic_nanos();
    let mut ok = true;
    for key in 0..TIMES {
        ok &= port::bind(
            &process.handles,
            &account,
            process.id(),
            (hp, port_need(rights::WRITE)),
            (he, Need::rights(rights::WAIT)),
            key,
            SIG,
        )
        .is_ok();
        ok &= event.signal(0, SIG).is_ok();
        ok &= port::wait(
            &process.handles,
            &process.waiting,
            hp,
            port_need(rights::READ),
            0,
        )
        .is_ok_and(|packet| packet.key == key);
        ok &= event.signal(SIG, 0).is_ok();
    }
    let took = time::monotonic_nanos().saturating_sub(start);
    let none_held = account.objects().bindings.held() == 0;
    process.abandon();
    drop(event);
    let ok = ok && none_held;
    let _ = writeln!(
        com1,
        "origin-kernel: ports: a bind, a signal that delivers it, a take and a clear took {} ns together (QEMU, the kernel's own calls, {TIMES} times, no budget): {ok}{}",
        took / TIMES,
        verdict(ok)
    );
    ok
}

/// The events-and-ports proof (ADR-0005 C9, C10, Amendment 7), with a leak check of its own.
/// Called from the boot flow with every scheduler running, after `prove_memory`.
pub(crate) fn prove_events_ports(
    com1: &mut Console,
    cpus: &[CpuIndex],
    bsp: CpuIndex,
    tsc_hz: u64,
) -> bool {
    let before = memory::stats();
    let rules = rules(com1, bsp, tsc_hz);
    let none = no_quota(com1, bsp, tsc_hz);
    let canceled = canceled_wait(com1, cpus, bsp, tsc_hz);
    let exits = binder_exits(com1, bsp, tsc_hz);
    let trips = round_trips(com1, cpus, bsp, tsc_hz);
    let costs = kernel_costs(com1);
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
        "origin-kernel: ports: every process, event, port, binding and packet of the proof is gone: heap {heap_now} bytes (was {heap_before}), {frames_now} free frames (was {frames_before}){}",
        verdict(back)
    );
    rules && none && canceled && exits && trips && costs && back
}

// ------------------------------------------------------------------ the clock

/// Readings of the clock in a row: three reports each, and a process's log keeps 128.
const READS: u64 = 30;
/// How far ahead of the start the timed wait's deadline is.
const WAIT_NANOS: u64 = 20_000_000;

/// The clock as ring 3 reads it (ADR-0005 C13, Amendment 7 F10; ADR-0012): reported, not
/// relied on.
pub(crate) fn prove_user_clock(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let fail = |com1: &mut Console| {
        let _ = writeln!(
            com1,
            "origin-kernel: clock: the reading process could not be set up: user clock failed"
        );
        false
    };
    let Some(mut k) = Program::new(objects(1, 0, 0)) else {
        return fail(com1);
    };
    let Ok(never) = Event::create(&k.image.process.account) else {
        return fail(com1);
    };
    let Some(never) = k.grant(Object::Event(never), EVENT) else {
        return fail(com1);
    };
    for i in 0..READS {
        k.written.call(clock(CLK + 8 * i, OK));
    }
    let readings = k.written.script.report_bytes(CLK, (8 * READS) as usize);
    let deadline = time::monotonic_nanos() + WAIT_NANOS;
    k.written
        .call(wait("", never, SIG, deadline, Error::TimedOut.raw()));
    let after_at = CLK + 8 * READS;
    k.written.call(clock(after_at, OK));
    let after = k.written.script.report_bytes(after_at, 8);
    for bad in [KERNEL, SCRIPT, UNMAPPED] {
        k.written.call(clock(bad, Error::Fault.raw()));
    }
    let first = time::monotonic_nanos();
    let Some((process, steps, _)) = started(k, bsp) else {
        return fail(com1);
    };
    let ended = await_end(&process, tsc_hz, 5) && process.ending() == Some(Ending::Exited(0));
    let last = time::monotonic_nanos();
    let reports = process.reports();
    let values: Vec<u64> = (0..READS as usize)
        .filter_map(|i| reported_u64(&reports, readings + 2 * i))
        .collect();
    let woke = reported_u64(&reports, after).unwrap_or(0);
    let statuses = statuses_right(&steps, &reports);
    let complete = values.len() == READS as usize;
    let ordered = values.windows(2).all(|w| w.first() <= w.get(1));
    let inside = values.iter().all(|&v| (first..=last).contains(&v));
    let after_deadline = woke >= deadline && woke <= last;
    let per_read = match (values.first(), values.last()) {
        (Some(&a), Some(&b)) if READS > 1 => b.saturating_sub(a) / (READS - 1),
        _ => 0,
    };
    let ok = ended && statuses && complete && ordered && inside && after_deadline;
    let _ = writeln!(
        com1,
        "origin-kernel: clock: {READS} reads from ring 3 never went back: {ordered}; each lies between the kernel's readings before and after: {inside}; a wait to a deadline on the same clock ended after it: {after_deadline}; a bad out pointer is fault: {statuses}; one read every {per_read} ns (QEMU, through the script interpreter){}",
        if ok { "" } else { ": user clock failed" }
    );
    ok
}
