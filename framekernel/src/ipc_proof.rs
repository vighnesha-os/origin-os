//! Boot proof of M4's first slice: real system calls between isolated processes (ADR-0005
//! P1–P5, P8, P9; CLAUDE.md §29, §30, §42).
//!
//! Everything here runs in ring 3, through the `syscall` instruction, in processes with
//! their own address spaces. The kernel only builds the processes, grants them their first
//! handles (the start record comes later) and reads back what they reported:
//! - **ping-pong:** two processes on two processors exchange messages over a channel,
//!   checking every value, blocking in `wait_one` and woken from the other processor.
//! - **hostile calls:** a process makes every bad call the task names and each must return
//!   the right status: bad handles, too few rights, another process's handle value, pointers
//!   into the kernel and into unmapped or read-only memory, a closed peer, a full queue, and
//!   the handle transfers that would break the object graph. Then it goes on working.
//! - **exit:** a process exits, or faults, holding a channel; its handles close, and the
//!   peer, blocked on another processor, wakes and sees `PEER_CLOSED`.
//! - **cancellation:** a thread blocked in `wait_one` on a handle another thread closes
//!   completes with `CANCELED`.
//! - **limits:** quotas stop a process, not the kernel.
//! - **hostile processor state:** GS and RFLAGS.AC set from ring 3 reach no kernel code.
//! - **cleanup:** when all of them have ended, the heap and the frame allocator are where
//!   they started, so nothing a process left behind leaked.
//!
//! The ring-3 programs are written in assembly and carried in the kernel's image as data,
//! like any other payload (`origin_frame::proof_programs`, the only place assembly may live). The script interpreter runs tables of system calls the kernel
//! writes into each process's memory, so a hostile sequence is a table with the status each
//! call must return beside it.

#![forbid(unsafe_code)]

use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::Ordering;

use crate::paging::{Access, PAGE};
use origin_abi::limits::{DEADLINE_INFINITE, MAX_MESSAGE_BYTES};
use origin_abi::{
    Error, Handle, OK, ObjectType, read_sizes, received_handle, rights, signals, start,
    status_name, syscall as sys,
};

use origin_frame::objref::ObjectRef;
use origin_frame::proof_programs::{self, DATA, ENTRY_RDI, SCRIPT, STACK};
use origin_frame::userspace::{self, UserFrame};

use crate::console::Console;
use crate::cpu::{self, CpuIndex};
use crate::handles::{GENERATION_BITS, Need};
use crate::ipc::{self, Object};
use crate::launch::{self, Given};
use crate::process::{BOOT_PROOF_PRINCIPAL, Ending, Limits, MemoryLimits, ObjectLimits, Process};
use crate::{memory, sched, syscall, time};

/// Where every proof process's code begins.
const CODE: u64 = 0x40_0000;
/// Pages of scratch memory.
const DATA_PAGES: u64 = 4;
/// Pages of script (64 calls per page).
const SCRIPT_PAGES: u64 = 4;
/// A second read-write region for the 64 KiB message test.
const BIG_OUT: u64 = 0x70_0000;
const BIG_IN: u64 = 0x80_0000;
/// Pages in each big buffer: 64 KiB of message and a page beyond it that stays unmapped,
/// so a miscounted copy faults instead of running on.
const BIG_PAGES: u64 = 16;
/// The stack of thread 1: one page, with no neighbour mapped below it (thread 0's is `STACK`).
const STACK_1: u64 = 0x60_2000;

/// In the scratch memory: each process's handles, one `u64` per slot (ADR-0005 Amendment 6,
/// E1), for [`SLOTS`] slots, in an area of their own at [`SLOT_AREA`]. (Until task S4-0 the
/// slots were `u32`s at `DATA`; the ping-pong programs still take their handle and round count
/// at `DATA` and their buffers at the offsets below, which are therefore unchanged.)
const fn slot_addr(i: u64) -> u64 {
    SLOT_AREA + 8 * i
}
/// Where the handle slots are.
const SLOT_AREA: u64 = DATA + 0x3000;
/// How many handle slots there are.
const SLOTS: u64 = 64;
/// The bytes every test message carries.
const MSG: u64 = DATA + 0x100;
/// Where messages are read to.
const RBUF: u64 = DATA + 0x140;
/// `wait_one`'s output.
const OBS: u64 = DATA + 0x240;
/// `channel_read`'s sizes structure, given its first version's size, 12, before the process
/// starts: every read through it gets the counts and nothing past them (C2).
const SIZES: u64 = DATA + 0x248;
/// A sizes structure too small for the kernel to use.
const BAD_SIZES: u64 = DATA + 0x260;
/// A handle list to send: a `u64` per handle, room for sixteen.
const LIST: u64 = DATA + 0x300;
/// Handles received: a `received_handle` record each (ADR-0005 B3), so the first handle's
/// value is the `u64` here.
const RECV: u64 = DATA + 0x400;

// The slots lie inside the scratch memory, in one page, above everything else the proofs place
// there (the hostile memory proof's area ends at `DATA + 0x2900`); and the list ends before the
// records.
const _: () = assert!(
    SLOT_AREA >= DATA + 0x2900
        && slot_addr(SLOTS) <= DATA + DATA_PAGES * PAGE
        && SLOT_AREA / PAGE == (slot_addr(SLOTS) - 1) / PAGE
        && LIST + 16 * 8 <= RECV
);

/// A test message.
const TEXT: &[u8; 16] = b"origin ipc proof";

// ------------------------------------------------------------------ scripted calls

/// An argument of a scripted call.
#[derive(Clone, Copy)]
enum Arg {
    /// This value.
    Imm(u64),
    /// The handle, a `u64`, at this address, loaded by the interpreter when the call runs.
    Handle(u64),
}
use Arg::{Handle as Hd, Imm};

mod core_pin;
mod directory;
mod events_ports;
mod memory_objects;
pub(crate) use core_pin::{prove_core_pin, prove_entry_flags};
pub(crate) use directory::prove_directory;
pub(crate) use events_ports::{prove_events_ports, prove_user_clock};
pub(crate) use memory_objects::prove_memory;

/// What a step must do.
#[derive(Clone, Copy)]
enum Want {
    /// Return this status.
    Status(i64),
    /// Report this value (a `report32` step).
    Value(u64),
    /// Nothing is reported.
    Nothing,
}

/// One entry of a script.
#[derive(Clone, Copy)]
struct Step {
    /// A label to print, empty for a setup step.
    name: &'static str,
    number: u64,
    args: [Arg; 6],
    want: Want,
}

/// The status of every step is printed by name, setup steps only when they go wrong.
fn step(name: &'static str, number: u64, args: &[Arg], want: i64) -> Step {
    let mut full = [Imm(0); 6];
    for (slot, arg) in full.iter_mut().zip(args) {
        *slot = *arg;
    }
    Step {
        name,
        number,
        args: full,
        want: Want::Status(want),
    }
}

/// Handle slot `i`'s value, as a call argument.
const fn h(i: u64) -> Arg {
    Hd(slot_addr(i))
}

fn create(first: u64) -> Step {
    step("", sys::CHANNEL_CREATE, &[Imm(slot_addr(first))], OK)
}

fn close(name: &'static str, x: Arg, want: i64) -> Step {
    step(name, sys::HANDLE_CLOSE, &[x], want)
}

fn dup(name: &'static str, x: Arg, r: u32, out: u64, want: i64) -> Step {
    step(
        name,
        sys::HANDLE_DUPLICATE,
        &[x, Imm(u64::from(r)), Imm(slot_addr(out))],
        want,
    )
}

fn write(name: &'static str, x: Arg, bytes: u64, len: u64, want: i64) -> Step {
    step(
        name,
        sys::CHANNEL_WRITE,
        &[x, Imm(bytes), Imm(len), Imm(0), Imm(0)],
        want,
    )
}

fn send(name: &'static str, x: Arg, count: u64, want: i64) -> Step {
    step(
        name,
        sys::CHANNEL_WRITE,
        &[x, Imm(MSG), Imm(4), Imm(LIST), Imm(count)],
        want,
    )
}

fn read(name: &'static str, x: Arg, bytes: u64, cap: u64, want: i64) -> Step {
    step(
        name,
        sys::CHANNEL_READ,
        &[x, Imm(bytes), Imm(cap), Imm(0), Imm(0), Imm(SIZES)],
        want,
    )
}

fn read_handles(name: &'static str, x: Arg, want: i64) -> Step {
    step(
        name,
        sys::CHANNEL_READ,
        &[x, Imm(RBUF), Imm(16), Imm(RECV), Imm(4), Imm(SIZES)],
        want,
    )
}

fn wait(name: &'static str, x: Arg, mask: u32, deadline: u64, want: i64) -> Step {
    step(
        name,
        sys::WAIT_ONE,
        &[x, Imm(u64::from(mask)), Imm(deadline), Imm(OBS)],
        want,
    )
}

/// The `u32` at `addr` must be `value` (reported to the kernel, which compares).
const fn value_at(name: &'static str, addr: u64, value: u64) -> Step {
    Step {
        name,
        number: PSEUDO_REPORT32,
        args: [Imm(addr), Imm(0), Imm(0), Imm(0), Imm(0), Imm(0)],
        want: Want::Value(value),
    }
}

/// Copies the `u32` at `from` to `to`, in ring 3, reporting nothing.
const fn copy32(from: u64, to: u64) -> Step {
    Step {
        name: "",
        number: PSEUDO_MOV32,
        args: [Imm(from), Imm(to), Imm(0), Imm(0), Imm(0), Imm(0)],
        want: Want::Nothing,
    }
}

/// Copies the handle at `from` to `to`, all 64 bits of it (ADR-0005 Amendment 6, E1), in
/// ring 3, reporting nothing.
const fn copy_handle(from: u64, to: u64) -> Step {
    Step {
        name: "",
        number: PSEUDO_MOV64,
        args: [Imm(from), Imm(to), Imm(0), Imm(0), Imm(0), Imm(0)],
        want: Want::Nothing,
    }
}

const PSEUDO_MOV32: u64 = -2i64 as u64;
const PSEUDO_REPORT32: u64 = -3i64 as u64;
const PSEUDO_FAULT: u64 = -4i64 as u64;
const PSEUDO_MOV64: u64 = -5i64 as u64;

/// Executes `ud2`: a #UD ends the thread.
const fn fault() -> Step {
    Step {
        name: "",
        number: PSEUDO_FAULT,
        args: [Imm(0); 6],
        want: Want::Nothing,
    }
}

/// A script, and where each value it reports lands among its reports: every call reports its
/// status, and every `report32` its value, in order.
#[derive(Default)]
struct Script {
    steps: Vec<Step>,
}

impl Script {
    fn call(&mut self, step: Step) {
        self.steps.push(step);
    }

    /// How many reports the steps so far make.
    fn reported(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| !matches!(s.want, Want::Nothing))
            .count()
    }

    /// Reports the `len` bytes at `at`, a word at a time; returns where they start.
    fn report_bytes(&mut self, at: u64, len: usize) -> usize {
        let start = self.reported();
        for word in 0..len.div_ceil(4) as u64 {
            self.steps.push(value_at("", at + 4 * word, 0));
        }
        start
    }
}

/// A blocking wait for a message on `x`.
fn readable(x: Arg) -> Step {
    wait("", x, signals::READABLE, DEADLINE_INFINITE, OK)
}

/// Whether every call among `steps` returned the status it must, in `reports`, whatever the
/// values reported between them.
fn statuses_right(steps: &[Step], reports: &[u64]) -> bool {
    let wanted: Vec<&Step> = steps
        .iter()
        .filter(|s| !matches!(s.want, Want::Nothing))
        .collect();
    reports.len() == wanted.len()
        && wanted.iter().zip(reports).all(|(s, &got)| match s.want {
            Want::Status(status) => got == status as u64,
            _ => true,
        })
}

/// An `object_request` (ADR-0005 C7) whose `size` field says `size`, for `length` bytes.
fn object_request(size: u32, length: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&size.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes
}

/// A `map_request` (ADR-0005 C8) whose `size` field says `size`.
fn map_request(
    size: u32,
    access: u32,
    flags: u32,
    offset: u64,
    length: u64,
    address: u64,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&size.to_le_bytes());
    bytes.extend_from_slice(&access.to_le_bytes());
    bytes.extend_from_slice(&flags.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&offset.to_le_bytes());
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(&address.to_le_bytes());
    bytes
}

/// A named group of steps, printed on one line.
struct Group {
    title: &'static str,
    steps: Vec<Step>,
}

// ------------------------------------------------------------------ building processes

/// A process being put together.
struct Image {
    process: Process,
    /// Every page mapped so far: its user address and frame.
    pages: Vec<(u64, UserFrame)>,
}

impl Image {
    /// A process with `program` at [`CODE`], its data pages, and one stack page; system
    /// calls accepted from its code only.
    fn new(limits: Limits, code: &[u8]) -> Option<Self> {
        let code_pages = code.len().div_ceil(PAGE as usize).max(1) as u64;
        Self::with_origin(limits, code, (CODE, CODE + code_pages * PAGE))
    }

    /// As [`Image::new`], but system calls are accepted only from `origin`.
    fn with_origin(limits: Limits, code: &[u8], origin: (u64, u64)) -> Option<Self> {
        Self::build(limits, code, origin, BOOT_PROOF_PRINCIPAL)
    }

    /// As [`Image::new`], acting for `principal` (ADR-0005 C1).
    fn acting_for(limits: Limits, code: &[u8], principal: u64) -> Option<Self> {
        let code_pages = code.len().div_ceil(PAGE as usize).max(1) as u64;
        Self::build(limits, code, (CODE, CODE + code_pages * PAGE), principal)
    }

    fn build(limits: Limits, code: &[u8], origin: (u64, u64), principal: u64) -> Option<Self> {
        Self::from_process(Process::new(limits, origin, principal)?, code)
    }

    /// An image for `process`, made by its caller (a launched child, say): `program` at
    /// [`CODE`], its data pages, and one stack page.
    fn from_process(process: Process, code: &[u8]) -> Option<Self> {
        let code_pages = code.len().div_ceil(PAGE as usize).max(1) as u64;
        let mut image = Self {
            process,
            pages: Vec::new(),
        };
        image.map(CODE, code_pages, Access::ReadExecute)?;
        image.write(CODE, code)?;
        image.map(DATA, DATA_PAGES, Access::ReadWrite)?;
        image.map(STACK, 1, Access::ReadWrite)?;
        Some(image)
    }

    fn map(&mut self, virt: u64, pages: u64, access: Access) -> Option<()> {
        for k in 0..pages {
            let at = virt + k * PAGE;
            let frame = self.process.map_page(at, access).ok()?;
            self.pages.push((at, frame));
        }
        Some(())
    }

    /// Writes `bytes` into the process's memory at `virt`.
    fn write(&self, virt: u64, bytes: &[u8]) -> Option<()> {
        let mut at = virt;
        let mut rest = bytes;
        while !rest.is_empty() {
            let page = at & !(PAGE - 1);
            let offset = (at - page) as usize;
            let chunk = rest.len().min(PAGE as usize - offset);
            let (head, tail) = rest.split_at_checked(chunk)?;
            let &(_, frame) = self.pages.iter().find(|(v, _)| *v == page)?;
            if !self.process.write_frame(frame, offset, head) {
                return None;
            }
            at += chunk as u64;
            rest = tail;
        }
        Some(())
    }

    fn write_u64(&self, virt: u64, value: u64) -> Option<()> {
        self.write(virt, &value.to_le_bytes())
    }

    /// The scratch memory every scripted process starts with.
    fn scratch(&self) -> Option<()> {
        self.write(MSG, TEXT)?;
        self.write(SIZES, &read_sizes::BASE_SIZE.to_le_bytes())?;
        self.write(BAD_SIZES, &8u32.to_le_bytes())
    }

    /// Maps the script's pages (read-only) and writes `steps` into them.
    fn script(&mut self, steps: &[Step]) -> Option<()> {
        self.map(SCRIPT, SCRIPT_PAGES, Access::Read)?;
        self.write_script(SCRIPT, steps)
    }

    fn write_script(&self, at: u64, steps: &[Step]) -> Option<()> {
        let mut encoded = Vec::with_capacity((steps.len() + 1) * 64);
        for s in steps {
            let mut flags = 0u64;
            let mut args = [0u64; 6];
            for (i, (value, arg)) in args.iter_mut().zip(s.args).enumerate() {
                match arg {
                    Imm(v) => *value = v,
                    Hd(addr) => {
                        *value = addr;
                        flags |= 1 << i;
                    }
                }
            }
            encoded.extend_from_slice(&s.number.to_le_bytes());
            encoded.extend_from_slice(&flags.to_le_bytes());
            for a in args {
                encoded.extend_from_slice(&a.to_le_bytes());
            }
        }
        encoded.extend_from_slice(&u64::MAX.to_le_bytes());
        encoded.extend_from_slice(&[0; 56]);
        self.write(at, &encoded)
    }

    /// The process, now shared; `None` without the memory to share it.
    fn finish(self) -> Option<ObjectRef<Process>> {
        ObjectRef::try_new(self.process).ok()
    }
}

/// What a test process may hold.
const LIMITS: Limits = Limits {
    handles: 64,
    channels: 32,
    message_bytes: 1 << 20,
    debug_calls: true,
    memory: MemoryLimits::NONE,
    objects: ObjectLimits::NONE,
};

/// Waits for `process`'s last thread to end.
fn await_end(process: &Process, tsc_hz: u64, seconds: u64) -> bool {
    sched::yield_until(|| process.ending().is_some(), tsc_hz, seconds);
    process.ending().is_some()
}

/// Whether `reports` are what `steps` must produce, printing each group on its own line.
/// Setup steps are checked too, and named only when one goes wrong.
fn check_groups(com1: &mut Console, groups: &[Group], reports: &[u64]) -> bool {
    let mut at = 0;
    let mut ok = true;
    for group in groups {
        let mut text = alloc::string::String::new();
        let mut group_ok = true;
        for s in &group.steps {
            let got = match s.want {
                Want::Nothing => continue,
                _ => reports.get(at).copied(),
            };
            at += 1;
            let shown = match (s.want, got) {
                (Want::Status(_), Some(v)) => alloc::string::String::from(status_name(v as i64)),
                (Want::Value(_), Some(v)) => alloc::format!("{v:#x}"),
                (_, None) => alloc::string::String::from("nothing"),
                (Want::Nothing, _) => continue,
            };
            let right = match s.want {
                Want::Status(w) => got == Some(w as u64),
                Want::Value(w) => got == Some(w),
                Want::Nothing => true,
            };
            group_ok &= right;
            if !s.name.is_empty() {
                let _ = write!(
                    text,
                    "{}{} -> {}",
                    if text.is_empty() { "" } else { ", " },
                    s.name,
                    shown
                );
            } else if !right {
                let _ = writeln!(
                    com1,
                    "origin-kernel: ipc: {}: a setup step {} returned {shown}: ipc failed",
                    group.title, s.number as i64
                );
            }
        }
        ok &= group_ok;
        let _ = writeln!(
            com1,
            "origin-kernel: ipc: {}: {text}{}",
            group.title,
            if group_ok { "" } else { ": ipc failed" }
        );
    }
    ok
}

/// The first step among `groups` whose report is not what it must be: its group's title, its
/// name (empty for a setup step) and what it reported. `None` when every step is right and
/// `reports` holds exactly one value for each step that reports. [`check_groups`] without the
/// printing, for a proof that runs another proof's script a second way.
fn first_wrong(
    groups: &[Group],
    reports: &[u64],
) -> Option<(&'static str, &'static str, Option<u64>)> {
    let mut at = 0;
    for group in groups {
        for s in &group.steps {
            if matches!(s.want, Want::Nothing) {
                continue;
            }
            let got = reports.get(at).copied();
            at += 1;
            let right = match s.want {
                Want::Status(w) => got == Some(w as u64),
                Want::Value(w) => got == Some(w),
                Want::Nothing => true,
            };
            if !right {
                return Some((group.title, s.name, got));
            }
        }
    }
    (at != reports.len()).then_some(("", "more reports than steps", reports.get(at).copied()))
}

/// How a scripted process makes its calls.
#[derive(Clone, Copy)]
enum Via<'a> {
    /// The assembly interpreter, by its own `syscall` instructions, under the range its creator
    /// names (ADR-0005 A6): one of the kernel's proof processes.
    Direct,
    /// The Rust interpreter the kernel carries, through `origin.core`'s stubs, pinned per call
    /// (ADR-0005 B2; task S4-2), launched by this creator.
    Core(&'a Process),
}

/// A scripted process, made and not yet started: its memory can still be written, and handles
/// granted into its table.
struct Scripted {
    image: Image,
    /// Where the launch path put the carried program, for a process made [`Via::Core`].
    pinned: Option<core_pin::Placed>,
}

impl Scripted {
    /// A process under `limits` that will run a script: the assembly interpreter at [`CODE`],
    /// or the carried Rust interpreter, as `via` says. A process made through `origin.core`
    /// gets two handles more than `limits` gives, for the process and thread handles its start
    /// record holds, so that a script finds the same room in its table either way.
    fn new(limits: Limits, via: Via<'_>) -> Option<Self> {
        match via {
            Via::Direct => Some(Self {
                image: Image::new(limits, proof_programs::script())?,
                pinned: None,
            }),
            Via::Core(creator) => core_pin::scripted(creator, limits),
        }
    }

    /// Starts it on `cpu`, to run the script written at [`SCRIPT`]. With `own_slot`, a handle
    /// to its own process, with C6's rights for one, is in that slot when the script runs.
    fn start(self, cpu: CpuIndex, own_slot: Option<u64>) -> Option<ObjectRef<Process>> {
        match self.pinned {
            Some(placed) => core_pin::start(self.image, placed, cpu, own_slot.map(slot_addr)),
            None => {
                let process = self.image.finish()?;
                if let Some(slot) = own_slot {
                    // Its own handle; the table closes it when the process ends.
                    let own = process.grant(Object::Process(process.clone()), rights::PROCESS);
                    if !own.is_ok_and(|own| write_slot(&process, slot, own)) {
                        process.abandon();
                        return None;
                    }
                }
                Process::start_thread(&process, cpu, CODE, STACK + PAGE, SCRIPT).ok()?;
                Some(process)
            }
        }
    }
}

/// Whether `origin.core` has a stub for `step`'s call: a pseudo-step is the interpreter's own,
/// and a number no method has can be asked of the kernel only by a `syscall` that is no
/// stub's.
fn has_stub(step: &Step) -> bool {
    (step.number as i64) < 0
        || origin_syscalls::origin_kernel::kernel::server::CALLS
            .iter()
            .any(|call| call.number == step.number)
}

// ------------------------------------------------------------------ the proofs

/// Round trips of the ping-pong.
const ROUNDS: u64 = 1000;

/// Two processes exchange [`ROUNDS`] messages over a channel, each on its own processor,
/// each blocking in `wait_one` for the other. Every message's content is checked in ring 3.
fn ping_pong(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let ping_code = proof_programs::ping();
    let pong_code = proof_programs::pong();
    let (Some(ping_image), Some(pong_image)) =
        (Image::new(LIMITS, ping_code), Image::new(LIMITS, pong_code))
    else {
        return false;
    };
    let (Some(ping), Some(pong)) = (
        build_pinger(ping_image, ROUNDS),
        build_pinger(pong_image, ROUNDS),
    ) else {
        return false;
    };
    let Ok((a, b)) = ipc::create_channel(&ping.account) else {
        return false;
    };
    let granted = ping
        .grant(Object::Channel(a), rights::CHANNEL)
        .and_then(|ha| {
            pong.grant(Object::Channel(b), rights::CHANNEL)
                .map(|hb| (ha, hb))
        });
    let Ok((ha, hb)) = granted else {
        return false;
    };
    // The kernel told each process its handle through its data page.
    let told = write_param(&ping, 0, ha) && write_param(&pong, 0, hb);
    let wakes_before = ipc::cross_cpu_wakes();
    let start = cpu::rdtsc();
    if !told
        || Process::start_thread(&pong, far, CODE, STACK + PAGE, 0).is_err()
        || Process::start_thread(&ping, bsp, CODE, STACK + PAGE, 0).is_err()
    {
        return false;
    }
    let ended = await_end(&ping, tsc_hz, 20) && await_end(&pong, tsc_hz, 5);
    let cycles = cpu::rdtsc().wrapping_sub(start);
    let micros = cycles / (tsc_hz / 1_000_000).max(1);
    let (ping_reports, pong_reports) = (ping.reports(), pong.reports());
    let wakes = ipc::cross_cpu_wakes().wrapping_sub(wakes_before);
    let blocked = (ping.blocked(), pong.blocked());
    let right = ended
        && ping.ending() == Some(Ending::Exited(0))
        && pong.ending() == Some(Ending::Exited(0))
        && ping_reports == [ROUNDS]
        && pong_reports == [ROUNDS]
        && blocked.0 + blocked.1 > 0
        && (far == bsp || wakes > 0);
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: ping-pong: two ring-3 processes on {} and {} exchanged {} messages each way, every one checked in ring 3; the processes blocked in wait_one {} and {} times, and {} wake-ups crossed processors{}",
        bsp.named(),
        far.named(),
        ROUNDS,
        blocked.0,
        blocked.1,
        wakes,
        if right { "" } else { ": ipc failed" }
    );
    if right {
        let _ = writeln!(
            com1,
            "origin-kernel: ipc: ping-pong round trip: {} ns between two processors",
            micros * 1000 / ROUNDS.max(1)
        );
    }
    right
}

/// The process of `image`, its round count written (handles are written after the grants).
fn build_pinger(image: Image, rounds: u64) -> Option<ObjectRef<Process>> {
    image.write_u64(DATA + 8, rounds)?;
    image.write(SIZES, &read_sizes::BASE_SIZE.to_le_bytes())?;
    image.finish()
}

/// Writes `value` to the `u64` at `offset` of the process's data page, before it starts.
fn write_param(process: &Process, offset: usize, value: u64) -> bool {
    process
        .frame_of(DATA)
        .is_some_and(|f| process.write_frame(f, offset, &value.to_le_bytes()))
}

/// The hostile script. `kernel` is an address of kernel data; `foreign` a handle value that
/// is live in another process's table, and not in this one's; `live` a channel end of this
/// process, whose peer the kernel holds, so it is writable.
fn hostile_groups(kernel: u64, foreign: Handle, live: Handle) -> Vec<Group> {
    const UNMAPPED: u64 = 0x0000_2000_0000_0000;
    const TOP_PAGE: u64 = 0x0000_7FFF_FFFF_F000;
    let fault = Error::Fault.raw();
    let bad = Error::BadHandle.raw();
    let denied = Error::AccessDenied.raw();
    let invalid = Error::InvalidArgs.raw();
    let wait_status = Error::ShouldWait.raw();
    let timed_out = Error::TimedOut.raw();
    let peer_closed = Error::PeerClosed.raw();
    let full = rights::CHANNEL;
    let (a, b) = (h(4), h(5));
    let data_end = DATA + DATA_PAGES * PAGE;
    alloc::vec![
        Group {
            title: "bad handles",
            steps: alloc::vec![
                close("handle 0", Imm(0), bad),
                // E1 (task S4-0): a handle fills 64 bits, and the kernel's table places a
                // slot above bit 32, so the low half of a live handle names nothing; nor does
                // its slot at a generation never issued.
                wait("a live handle", Imm(live), signals::WRITABLE, 0, OK),
                wait(
                    "its low 32 bits alone",
                    Imm(live & 0xFFFF_FFFF),
                    signals::WRITABLE,
                    0,
                    bad
                ),
                wait(
                    "its slot at a generation never issued",
                    Imm(live ^ (1 << (GENERATION_BITS - 1))),
                    signals::WRITABLE,
                    0,
                    bad
                ),
                close("", Imm(live), OK),
                write("never issued", Imm(0x7FFF_0042), MSG, 8, bad),
                read("bits above 32", Imm(0x1_0001_0001), RBUF, 16, bad),
                wait(
                    "wait on a bad handle",
                    Imm(0x7FFF_0042),
                    signals::READABLE,
                    0,
                    bad
                ),
                create(0),
                close("", h(0), OK),
                write("closed", h(0), MSG, 8, bad),
                create(2),
                write("stale, slot reused", h(0), MSG, 8, bad),
                write("another process's", Imm(foreign), MSG, 8, bad),
                wait(
                    "wait on another process's",
                    Imm(foreign),
                    signals::READABLE,
                    0,
                    bad
                ),
            ],
        },
        Group {
            title: "too few rights",
            steps: alloc::vec![
                create(4),
                dup("", a, rights::READ | rights::WAIT, 6, OK),
                dup("", a, rights::WRITE, 7, OK),
                dup("", a, rights::READ | rights::WRITE | rights::WAIT, 8, OK),
                write("write without WRITE", h(6), MSG, 8, denied),
                read("read without READ", h(7), RBUF, 16, denied),
                wait("wait without WAIT", h(7), signals::READABLE, 0, denied),
                dup("duplicate without DUPLICATE", h(8), rights::READ, 9, denied),
                copy_handle(slot_addr(6), LIST),
                send("send without TRANSFER", a, 1, denied),
            ],
        },
        Group {
            title: "rights only shrink",
            steps: alloc::vec![
                dup(
                    "",
                    a,
                    rights::READ | rights::WAIT | rights::DUPLICATE,
                    10,
                    OK
                ),
                dup(
                    "adding WRITE",
                    h(10),
                    rights::READ | rights::WRITE,
                    11,
                    invalid
                ),
                dup("an undefined right", a, 1 << 30, 11, invalid),
                dup(
                    "dropping DUPLICATE",
                    h(10),
                    rights::READ | rights::WAIT,
                    11,
                    OK
                ),
                write("the copy cannot write", h(11), MSG, 8, denied),
                read("the copy can read", h(11), RBUF, 16, wait_status),
            ],
        },
        Group {
            title: "bad pointers",
            steps: alloc::vec![
                write("write from the kernel", a, kernel, 8, fault),
                write("from unmapped", a, UNMAPPED, 8, fault),
                write("across the end of mapped", a, data_end - 4, 8, fault),
                write("from address 0", a, 0, 8, fault),
                write("wrapping past the top", a, u64::MAX - 7, 16, fault),
                write("from the top page", a, TOP_PAGE, 8, fault),
                read("read into the kernel", b, kernel, 16, fault),
                read("into unmapped", b, UNMAPPED, 16, fault),
                read("into code", b, CODE, 16, fault),
                read("into read-only memory", b, SCRIPT, 16, fault),
                step(
                    "sizes in the kernel",
                    sys::CHANNEL_READ,
                    &[b, Imm(RBUF), Imm(16), Imm(0), Imm(0), Imm(kernel)],
                    fault,
                ),
                // Room for one handle is a whole record, 12 bytes, not a 4-byte value (B3).
                step(
                    "a handle record across the end of mapped",
                    sys::CHANNEL_READ,
                    &[b, Imm(RBUF), Imm(16), Imm(data_end - 4), Imm(1), Imm(SIZES)],
                    fault,
                ),
                step(
                    "create out to the kernel",
                    sys::CHANNEL_CREATE,
                    &[Imm(kernel)],
                    fault
                ),
                step(
                    "create out to read-only",
                    sys::CHANNEL_CREATE,
                    &[Imm(SCRIPT)],
                    fault
                ),
                step(
                    "duplicate out to the kernel",
                    sys::HANDLE_DUPLICATE,
                    &[a, Imm(rights::READ.into()), Imm(kernel)],
                    fault
                ),
                step(
                    "duplicate out to read-only",
                    sys::HANDLE_DUPLICATE,
                    &[a, Imm(rights::READ.into()), Imm(CODE)],
                    fault
                ),
                step(
                    "wait out to read-only",
                    sys::WAIT_ONE,
                    &[a, Imm(signals::WRITABLE.into()), Imm(0), Imm(SCRIPT)],
                    fault
                ),
                step(
                    "wait out to the kernel",
                    sys::WAIT_ONE,
                    &[a, Imm(signals::WRITABLE.into()), Imm(0), Imm(kernel)],
                    fault
                ),
                step(
                    "handle list in the kernel",
                    sys::CHANNEL_WRITE,
                    &[a, Imm(MSG), Imm(4), Imm(kernel), Imm(1)],
                    fault
                ),
                step(
                    "zero length names no memory",
                    sys::CHANNEL_WRITE,
                    &[a, Imm(UNMAPPED), Imm(0), Imm(UNMAPPED), Imm(0)],
                    OK
                ),
                read("", b, RBUF, 16, OK),
                value_at(
                    "the empty message arrived",
                    SIZES + read_sizes::BYTES_AT as u64,
                    0
                ),
            ],
        },
        Group {
            title: "sizes and numbers",
            steps: alloc::vec![
                write(
                    "one byte too many",
                    a,
                    MSG,
                    MAX_MESSAGE_BYTES as u64 + 1,
                    invalid
                ),
                step(
                    "one handle too many",
                    sys::CHANNEL_WRITE,
                    &[a, Imm(MSG), Imm(4), Imm(LIST), Imm(65)],
                    invalid
                ),
                wait("no signals", a, 0, 0, invalid),
                wait("an undefined signal", a, 1 << 31, 0, invalid),
                step(
                    "a sizes structure too small",
                    sys::CHANNEL_READ,
                    &[b, Imm(RBUF), Imm(16), Imm(0), Imm(0), Imm(BAD_SIZES)],
                    invalid
                ),
                step(
                    "an unknown call number",
                    999,
                    &[],
                    Error::NotSupported.raw()
                ),
                // Events exist since task S3-6, and this process was given no quota for one
                // (ADR-0005 D4, Amendment 7 F7).
                step(
                    "an event with no quota for one",
                    sys::EVENT_CREATE,
                    &[Imm(slot_addr(9))],
                    Error::Limit.raw()
                ),
            ],
        },
        Group {
            title: "queues",
            steps: alloc::vec![
                create(12),
                read("empty", h(13), RBUF, 16, wait_status),
                wait(
                    "nothing by deadline 0",
                    h(13),
                    signals::READABLE,
                    0,
                    timed_out
                ),
                write("written", h(12), MSG, 16, OK),
                wait("readable", h(13), signals::READABLE, 0, OK),
                value_at(
                    "observed",
                    OBS,
                    u64::from(signals::READABLE | signals::WRITABLE)
                ),
                read("4-byte buffer", h(13), RBUF, 4, Error::BufferTooSmall.raw()),
                value_at("it needs", SIZES + read_sizes::BYTES_AT as u64, 16),
                read("still there", h(13), RBUF, 16, OK),
                value_at("received", SIZES + read_sizes::BYTES_AT as u64, 16),
            ],
        },
        Group {
            title: "a full queue",
            steps: full_queue_steps(timed_out, wait_status),
        },
        Group {
            title: "a closed peer",
            steps: alloc::vec![
                create(14),
                write("", h(14), MSG, 16, OK),
                close("", h(14), OK),
                wait("sees PEER_CLOSED", h(15), signals::PEER_CLOSED, 0, OK),
                value_at(
                    "observed",
                    OBS,
                    u64::from(signals::READABLE | signals::PEER_CLOSED)
                ),
                write("write", h(15), MSG, 8, peer_closed),
                read("the message sent before the close", h(15), RBUF, 16, OK),
                read("read past the close", h(15), RBUF, 16, peer_closed),
                wait(
                    "READABLE will never rise",
                    h(15),
                    signals::READABLE,
                    0,
                    timed_out
                ),
            ],
        },
        Group {
            title: "handle transfer",
            steps: alloc::vec![
                create(16),
                create(18),
                copy_handle(slot_addr(18), LIST),
                send("send", h(16), 1, OK),
                write("the sender's handle is gone", h(18), MSG, 4, bad),
                read_handles("received", h(17), OK),
                value_at("with one handle", SIZES + read_sizes::HANDLES_AT as u64, 1),
                // Its record says what it names and what it may do (B3).
                value_at(
                    "its record names a channel",
                    RECV + received_handle::OBJECT_TYPE_AT as u64,
                    ObjectType::Channel as u64
                ),
                value_at(
                    "with the rights it was sent with",
                    RECV + received_handle::RIGHTS_AT as u64,
                    u64::from(full)
                ),
                write("the moved handle writes", Hd(RECV), MSG, 4, OK),
                read("and the other end reads", h(19), RBUF, 16, OK),
                dup("it kept its rights", Hd(RECV), full, 20, OK),
                close("", h(20), OK),
            ],
        },
        Group {
            title: "transfers that would break the graph",
            steps: alloc::vec![
                copy_handle(slot_addr(16), LIST),
                send("an end through its own channel", h(16), 1, invalid),
                copy_handle(slot_addr(17), LIST),
                send("the peer end through its own channel", h(16), 1, invalid),
                create(22),
                create(24),
                copy_handle(slot_addr(19), LIST),
                copy_handle(slot_addr(19), LIST + 8),
                send("the same handle twice", h(16), 2, invalid),
                dup("", h(19), full, 21, OK),
                copy_handle(slot_addr(19), LIST),
                send("an end with another handle", h(16), 1, invalid),
                close("", h(21), OK),
                send("that end, alone again", h(16), 1, OK),
                read_handles("", h(17), OK),
                copy_handle(slot_addr(24), LIST),
                send("into the first channel", h(23), 1, OK),
                copy_handle(slot_addr(22), LIST),
                send("a cycle through two queues", h(25), 1, invalid),
                copy_handle(slot_addr(0), LIST),
                send("a stale handle in the list", h(16), 1, bad),
            ],
        },
    ]
}

/// The hostile process's own process handle, its object, and the weaker handles it makes.
const HOSTILE_SELF: u64 = 56;
const HOSTILE_OBJECT: u64 = 57;
const HOSTILE_READ_ONLY_SELF: u64 = 59;
const HOSTILE_NO_MAP: u64 = 60;
const HOSTILE_NO_WRITE: u64 = 61;
/// Where the hostile process maps its object, and where the kernel tells it.
const HOSTILE_FIXED: u64 = 0x0000_0007_0000_0000;
const HOSTILE_OUT: u64 = DATA + 0x2800;

/// Where the hostile process keeps request `k`.
const fn hostile_request(k: u64) -> u64 {
    DATA + 0x2000 + 0x40 * k
}

/// The hostile process's memory requests, each at `hostile_request(k)`.
fn hostile_requests() -> [Vec<u8>; 20] {
    use origin_abi::{map_flags::FIXED, object_request as object};
    const SIZE: u32 = origin_abi::map_request::SIZE;
    const READ: u32 = origin_abi::map_access::READ;
    const WRITE: u32 = origin_abi::map_access::WRITE;
    const EXECUTE: u32 = origin_abi::map_access::EXECUTE;
    [
        object_request(object::SIZE, PAGE),
        object_request(12, PAGE),
        object_request(object::SIZE, 0),
        object_request(object::SIZE, PAGE + 1),
        object_request(object::SIZE, 3 * PAGE),
        map_request(SIZE, READ | WRITE, FIXED, 0, PAGE, HOSTILE_FIXED),
        map_request(39, READ | WRITE, FIXED, 0, PAGE, HOSTILE_FIXED),
        map_request(SIZE, READ | 1 << 3, 0, 0, PAGE, 0),
        map_request(SIZE, READ, 1 << 1, 0, PAGE, 0),
        map_request(SIZE, WRITE, 0, 0, PAGE, 0),
        map_request(SIZE, READ | WRITE | EXECUTE, 0, 0, PAGE, 0),
        map_request(SIZE, READ | EXECUTE, 0, 0, PAGE, 0),
        map_request(SIZE, READ, 0, 1, PAGE, 0),
        map_request(SIZE, READ, 0, 0, 0, 0),
        map_request(SIZE, READ, 0, 0, 2 * PAGE, 0),
        map_request(SIZE, READ, FIXED, 0, PAGE, 0xFFFF_8000_0000_0000),
        map_request(SIZE, READ, FIXED, 0, PAGE, 0),
        map_request(SIZE, READ, FIXED, 0, PAGE, crate::usercopy::USER_END),
        map_request(SIZE, READ, 0, 0, PAGE, HOSTILE_FIXED),
        map_request(SIZE, READ | WRITE, 0, 0, PAGE, 0),
    ]
}

/// Making memory objects: every malformed request refused for its reason, the quota, a pointer
/// into the kernel; then one made, which cannot be waited on or made executable.
fn memory_object_steps(kernel: u64) -> Vec<Step> {
    let invalid = Error::InvalidArgs.raw();
    let make = |name, request: u64, out: u64, want| {
        step(
            name,
            sys::MEMORY_OBJECT_CREATE,
            &[Imm(request), Imm(out)],
            want,
        )
    };
    let object = slot_addr(HOSTILE_OBJECT);
    alloc::vec![
        make(
            "a request in the kernel",
            kernel,
            object,
            Error::Fault.raw()
        ),
        make(
            "a request of the wrong size",
            hostile_request(1),
            object,
            invalid
        ),
        make("no bytes", hostile_request(2), object, invalid),
        make("not whole pages", hostile_request(3), object, invalid),
        make(
            "past the quota",
            hostile_request(4),
            object,
            Error::Limit.raw()
        ),
        make(
            "its handle into the kernel",
            hostile_request(0),
            kernel,
            Error::Fault.raw()
        ),
        make("one page", hostile_request(0), object, OK),
        wait(
            "wait on it",
            h(HOSTILE_OBJECT),
            signals::READABLE,
            0,
            Error::AccessDenied.raw()
        ),
        dup(
            "give it EXECUTE",
            h(HOSTILE_OBJECT),
            rights::MEMORY_OBJECT | rights::EXECUTE,
            58,
            invalid
        ),
    ]
}

/// Mapping it: the wrong handles and too few rights, each malformed request, the user half's
/// bounds; then mapped, and refused a second mapping over the first.
fn memory_map_steps(kernel: u64) -> Vec<Step> {
    let invalid = Error::InvalidArgs.raw();
    let denied = Error::AccessDenied.raw();
    let wrong = Error::WrongType.raw();
    let (own, object) = (h(HOSTILE_SELF), h(HOSTILE_OBJECT));
    let channel = h(4);
    let map = |name, process: Arg, object: Arg, request: u64, out: u64, want| {
        step(
            name,
            sys::MEMORY_MAP,
            &[process, object, Imm(request), Imm(out)],
            want,
        )
    };
    let asking = |name, k: u64, want| map(name, own, object, hostile_request(k), HOSTILE_OUT, want);
    alloc::vec![
        create(4),
        map(
            "a bad process handle",
            Imm(0),
            object,
            hostile_request(5),
            HOSTILE_OUT,
            Error::BadHandle.raw()
        ),
        map(
            "a channel for the process",
            channel,
            object,
            hostile_request(5),
            HOSTILE_OUT,
            wrong
        ),
        dup("", own, rights::READ, HOSTILE_READ_ONLY_SELF, OK),
        map(
            "a process handle without WRITE",
            h(HOSTILE_READ_ONLY_SELF),
            object,
            hostile_request(5),
            HOSTILE_OUT,
            denied
        ),
        map(
            "a channel for the object",
            own,
            channel,
            hostile_request(5),
            HOSTILE_OUT,
            wrong
        ),
        dup("", object, rights::READ | rights::WRITE, HOSTILE_NO_MAP, OK),
        map(
            "an object handle without MAP",
            own,
            h(HOSTILE_NO_MAP),
            hostile_request(5),
            HOSTILE_OUT,
            denied
        ),
        map(
            "a request in the kernel",
            own,
            object,
            kernel,
            HOSTILE_OUT,
            Error::Fault.raw()
        ),
        map(
            "the address into the kernel",
            own,
            object,
            hostile_request(5),
            kernel,
            Error::Fault.raw()
        ),
        asking("a request of the wrong size", 6, invalid),
        asking("an undefined access", 7, invalid),
        asking("an undefined flag", 8, invalid),
        asking("write without read", 9, invalid),
        asking("write and execute", 10, invalid),
        asking("execute", 11, denied),
        dup("", object, rights::READ | rights::MAP, HOSTILE_NO_WRITE, OK),
        map(
            "write without WRITE",
            own,
            h(HOSTILE_NO_WRITE),
            hostile_request(19),
            HOSTILE_OUT,
            denied
        ),
        asking("a misaligned offset", 12, invalid),
        asking("no length", 13, invalid),
        asking("past the object", 14, invalid),
        asking("in the kernel half", 15, invalid),
        asking("at address 0", 16, invalid),
        asking("at the top page", 17, invalid),
        asking("an address without FIXED", 18, invalid),
        asking("mapped", 5, OK),
        asking("over it", 5, invalid),
    ]
}

/// Unmapping and protecting it: without WRITE, anything but the whole mapping, accesses that
/// are malformed or add something; then reduced, refused an increase, unmapped, and gone.
fn memory_change_steps() -> Vec<Step> {
    use origin_abi::map_access::{EXECUTE, READ, WRITE};
    let invalid = Error::InvalidArgs.raw();
    let denied = Error::AccessDenied.raw();
    let own = h(HOSTILE_SELF);
    let unmap = |name, process: Arg, at: u64, length: u64, want| {
        step(
            name,
            sys::MEMORY_UNMAP,
            &[process, Imm(at), Imm(length)],
            want,
        )
    };
    let protect = |name, at: u64, access: u32, want| {
        step(
            name,
            sys::MEMORY_PROTECT,
            &[own, Imm(at), Imm(PAGE), Imm(u64::from(access))],
            want,
        )
    };
    alloc::vec![
        unmap(
            "unmap without WRITE",
            h(HOSTILE_READ_ONLY_SELF),
            HOSTILE_FIXED,
            PAGE,
            denied
        ),
        unmap(
            "unmap where nothing is",
            own,
            HOSTILE_FIXED + PAGE,
            PAGE,
            invalid
        ),
        unmap("unmap none of it", own, HOSTILE_FIXED, 0, invalid),
        unmap("unmap more than it", own, HOSTILE_FIXED, 2 * PAGE, invalid),
        unmap(
            "unmap not whole pages",
            own,
            HOSTILE_FIXED,
            PAGE + 1,
            invalid
        ),
        protect("protect to nothing", HOSTILE_FIXED, 0, invalid),
        protect(
            "protect with an undefined bit",
            HOSTILE_FIXED,
            READ | 1 << 3,
            invalid
        ),
        protect("protect to execute", HOSTILE_FIXED, READ | EXECUTE, denied),
        protect(
            "protect where nothing is",
            HOSTILE_FIXED + PAGE,
            READ,
            invalid
        ),
        protect("protect to read-only", HOSTILE_FIXED, READ, OK),
        protect(
            "protect back to writable",
            HOSTILE_FIXED,
            READ | WRITE,
            denied
        ),
        unmap("unmap", own, HOSTILE_FIXED, PAGE, OK),
        unmap("unmap again", own, HOSTILE_FIXED, PAGE, invalid),
    ]
}

/// Sixteen writes fill a queue; the seventeenth is refused until a read makes room.
fn full_queue_steps(timed_out: i64, wait_status: i64) -> Vec<Step> {
    let mut steps = alloc::vec![create(26)];
    for _ in 0..crate::channel::QUEUE_DEPTH {
        steps.push(write("", h(26), MSG, 16, OK));
    }
    steps.push(write(
        "the write after the last",
        h(26),
        MSG,
        16,
        wait_status,
    ));
    steps.push(wait("not writable", h(26), signals::WRITABLE, 0, timed_out));
    steps.push(read("", h(27), RBUF, 16, OK));
    steps.push(wait(
        "writable again after a read",
        h(26),
        signals::WRITABLE,
        0,
        OK,
    ));
    steps
}

/// A process makes every bad call, each checked against the status it must return; the
/// kernel is undisturbed if the rest of the boot runs.
/// One run of a hostile script: its groups as run, what the process reported, whether it ended
/// in time, and whether it ended by its own exit with 0.
struct HostileRun {
    groups: Vec<Group>,
    reports: Vec<u64>,
    ended: bool,
    exited: bool,
}

/// Runs the hostile script in a process made as `via` says, and returns what it did with the
/// other process's handle value. Made through `origin.core`, the script is less the one step
/// that asks for a call number no method has: there is no stub to ask through.
fn hostile_run(via: Via<'_>, bsp: CpuIndex, tsc_hz: u64) -> Option<(HostileRun, Handle)> {
    // Another process with a live handle whose value is not live in the hostile process:
    // slot 40, which the hostile process's table never reaches.
    let other = Process::new(LIMITS, (CODE, CODE + PAGE), BOOT_PROOF_PRINCIPAL)?;
    let (end, peer) = ipc::create_channel(&other.account).ok()?;
    drop(peer);
    let mut foreign = 0;
    for _ in 0..=40 {
        foreign = other
            .grant(Object::Channel(end.clone()), rights::CHANNEL)
            .ok()?;
    }
    drop(end);

    let kernel = core::ptr::from_ref(&ROUNDS_MARKER).addr() as u64;
    let mut scripted = Scripted::new(LIMITS, via)?;
    // A live end of the hostile process's own, for E1's steps; its peer stays here.
    let (own, own_peer) = ipc::create_channel(&scripted.image.process.account).ok()?;
    let live = scripted
        .image
        .process
        .grant(Object::Channel(own), rights::CHANNEL)
        .ok()?;
    let mut groups = hostile_groups(kernel, foreign, live);
    if matches!(via, Via::Core(_)) {
        for group in &mut groups {
            group.steps.retain(has_stub);
        }
    }
    let steps: Vec<Step> = groups
        .iter()
        .flat_map(|g| g.steps.iter().copied())
        .collect();
    scripted.image.scratch()?;
    scripted.image.script(&steps)?;
    let process = scripted.start(bsp, None)?;
    let ended = await_end(&process, tsc_hz, 10);
    let reports = process.reports();
    let exited = process.ending() == Some(Ending::Exited(0));
    // The process lived through all of it and ended by its own exit; the other process's
    // handle was never reachable from it.
    drop(own_peer);
    // It never ran: only its handle table mattered. It is dropped with its handles.
    drop(other);
    Some((
        HostileRun {
            groups,
            reports,
            ended,
            exited,
        },
        foreign,
    ))
}

fn hostile(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let Some((run, foreign)) = hostile_run(Via::Direct, bsp, tsc_hz) else {
        return false;
    };
    let groups_ok = check_groups(com1, &run.groups, &run.reports);
    let right = run.ended && groups_ok && run.exited && foreign >> GENERATION_BITS == 41;
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: every hostile call returned its status and the process exited by itself ({} calls); user-copy faults recovered: {}{}",
        run.reports.len(),
        cpu::user_copy_faults(),
        if right { "" } else { ": ipc failed" }
    );
    right
}

/// The memory calls' hostile corpus (task S3-5b), in a process of its own, since the first
/// corpus fills a process's report log: it may commit two pages and hold two mappings, and
/// names its own process handle, in slot [`HOSTILE_SELF`], for every call.
fn hostile_memory(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let Some(run) = hostile_memory_run(Via::Direct, bsp, tsc_hz) else {
        return false;
    };
    let groups_ok = check_groups(com1, &run.groups, &run.reports);
    let (ended, reports) = (run.ended, &run.reports);
    let right = ended && groups_ok && run.exited;
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: every hostile memory call returned its status and the process exited by itself ({} calls){}",
        reports.len(),
        if right { "" } else { ": ipc failed" }
    );
    right
}

/// Runs the memory calls' hostile corpus in a process made as `via` says.
fn hostile_memory_run(via: Via<'_>, bsp: CpuIndex, tsc_hz: u64) -> Option<HostileRun> {
    let kernel = core::ptr::from_ref(&ROUNDS_MARKER).addr() as u64;
    let groups = alloc::vec![
        Group {
            title: "memory objects",
            steps: memory_object_steps(kernel),
        },
        Group {
            title: "memory maps",
            steps: memory_map_steps(kernel),
        },
        Group {
            title: "memory unmap and protect",
            steps: memory_change_steps(),
        },
    ];
    let steps: Vec<Step> = groups
        .iter()
        .flat_map(|g| g.steps.iter().copied())
        .collect();
    let limits = Limits {
        memory: MemoryLimits {
            committed: 2 * PAGE as usize,
            mappings: 2,
            changes: crate::mapping::Rate {
                burst: 1024,
                per_second: 1024,
            },
        },
        ..LIMITS
    };
    let mut scripted = Scripted::new(limits, via)?;
    scripted.image.scratch()?;
    scripted.image.script(&steps)?;
    for (k, request) in hostile_requests().iter().enumerate() {
        scripted.image.write(hostile_request(k as u64), request)?;
    }
    // Its own handle, in the slot the script names.
    let process = scripted.start(bsp, Some(HOSTILE_SELF))?;
    let ended = await_end(&process, tsc_hz, 10);
    let reports = process.reports();
    let exited = process.ending() == Some(Ending::Exited(0));
    Some(HostileRun {
        groups,
        reports,
        ended,
        exited,
    })
}

/// What the "kernel pointer" argument points at.
static ROUNDS_MARKER: u64 = 0;

/// A peer blocked in `wait_one` on another processor sees `PEER_CLOSED` when the process at
/// the far end ends, by exiting or by faulting, without ever closing its handle.
fn exit_closes(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let mut ok = true;
    for (label, exit_step) in [("exits", None), ("faults", Some(fault()))] {
        let (Some(mut waiter), Some(mut ender)) = (
            Image::new(LIMITS, proof_programs::script()),
            Image::new(LIMITS, proof_programs::script()),
        ) else {
            return false;
        };
        let Ok((end_a, end_b)) = ipc::create_channel(&ender.process.account) else {
            return false;
        };
        let (Ok(_), Ok(hb)) = (
            ender.process.grant(Object::Channel(end_a), rights::CHANNEL),
            waiter
                .process
                .grant(Object::Channel(end_b), rights::CHANNEL),
        ) else {
            return false;
        };
        let held = Imm(hb);
        let waiter_steps = alloc::vec![
            wait(
                "woken",
                held,
                signals::PEER_CLOSED | signals::READABLE,
                DEADLINE_INFINITE,
                OK,
            ),
            value_at("observed", OBS, u64::from(signals::PEER_CLOSED)),
            write("write", held, MSG, 8, Error::PeerClosed.raw()),
            read("read", held, RBUF, 16, Error::PeerClosed.raw()),
        ];
        let ender_steps = alloc::vec![];
        let ender_steps = match exit_step {
            None => ender_steps,
            Some(step) => alloc::vec![step],
        };
        if waiter.scratch().is_none()
            || waiter.script(&waiter_steps).is_none()
            || ender.scratch().is_none()
            || ender.script(&ender_steps).is_none()
        {
            return false;
        }
        let (Some(waiter), Some(ender)) = (waiter.finish(), ender.finish()) else {
            return false;
        };
        let wakes_before = ipc::cross_cpu_wakes();
        if Process::start_thread(&waiter, far, CODE, STACK + PAGE, SCRIPT).is_err() {
            return false;
        }
        // The peer must be in `wait_one` before the other process ends. It is then blocked,
        // or about to be, and the close wakes it either way (no lost wake-up, `wait`).
        sched::yield_until(|| waiter.waiting.load(Ordering::Acquire) > 0, tsc_hz, 5);
        if Process::start_thread(&ender, bsp, CODE, STACK + PAGE, SCRIPT).is_err() {
            return false;
        }
        let ended = await_end(&ender, tsc_hz, 10) && await_end(&waiter, tsc_hz, 10);
        let groups = [Group {
            title: "the peer of a process that",
            steps: waiter_steps,
        }];
        let right_reports = check_groups(com1, &groups, &waiter.reports());
        let wakes = ipc::cross_cpu_wakes().wrapping_sub(wakes_before);
        let ender_ok = matches!(
            (exit_step.is_some(), ender.ending()),
            (false, Some(Ending::Exited(0))) | (true, Some(Ending::Faulted { vector: 6, .. }))
        );
        let handles_left = ender.handle_count();
        let right =
            ended && right_reports && ender_ok && handles_left == 0 && (far == bsp || wakes > 0);
        ok &= right;
        let _ = writeln!(
            com1,
            "origin-kernel: ipc: when a process {label}: its handles close ({handles_left} left), and its peer on {}, blocked {} times, woke and saw PEER_CLOSED; {} wake-ups crossed processors{}",
            far.named(),
            waiter.blocked(),
            wakes,
            if right { "" } else { ": ipc failed" }
        );
    }
    ok
}

/// A thread blocked in `wait_one` completes with `CANCELED` when another thread of its
/// process closes the handle it waits on.
fn cancel(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let Some(mut image) = Image::new(LIMITS, proof_programs::script()) else {
        return false;
    };
    // The second thread's stack.
    if image.map(STACK_1, 1, Access::ReadWrite).is_none() {
        return false;
    }
    let Ok((a, b)) = ipc::create_channel(&image.process.account) else {
        return false;
    };
    // The peer end stays open in the same process: only the close can end the wait.
    let (Ok(ha), Ok(_)) = (
        image.process.grant(Object::Channel(a), rights::CHANNEL),
        image.process.grant(Object::Channel(b), rights::CHANNEL),
    ) else {
        return false;
    };
    let waited = Imm(ha);
    let first = alloc::vec![wait(
        "the waiter",
        waited,
        signals::READABLE | signals::PEER_CLOSED,
        DEADLINE_INFINITE,
        Error::Canceled.raw(),
    )];
    let second = alloc::vec![close("", waited, OK)];
    if image.scratch().is_none() || image.script(&first).is_none() {
        return false;
    }
    let second_script = SCRIPT + SCRIPT_PAGES * PAGE / 2;
    if image.write_script(second_script, &second).is_none() {
        return false;
    }
    let Some(process) = image.finish() else {
        return false;
    };
    if Process::start_thread(&process, bsp, CODE, STACK + PAGE, SCRIPT).is_err() {
        return false;
    }
    // The closing thread starts only once the waiter is in `wait_one`, past its lookup of
    // the handle: whichever comes first, its block or the close, the wait ends `CANCELED`.
    sched::yield_until(|| process.waiting.load(Ordering::Acquire) > 0, tsc_hz, 5);
    let in_wait = process.waiting.load(Ordering::Acquire) > 0;
    if Process::start_thread(&process, far, CODE, STACK_1 + PAGE, second_script).is_err() {
        return false;
    }
    let ended = await_end(&process, tsc_hz, 10);
    let reports = process.reports();
    // Both threads' statuses are in the one log; the waiter's must be `CANCELED`.
    let canceled = reports.contains(&(Error::Canceled.raw() as u64));
    let right = ended && in_wait && canceled && process.ending() == Some(Ending::Exited(0));
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: a thread in wait_one on a handle another thread closes completes with {}{}",
        if canceled {
            "CANCELED"
        } else {
            "something else"
        },
        if right { "" } else { ": ipc failed" }
    );
    right
}

/// Quotas stop a process, not the kernel: it can make channels until its channel quota,
/// handles until its table is full, and carry on.
fn quotas(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let limits = Limits {
        handles: 12,
        channels: 4,
        message_bytes: 1 << 20,
        debug_calls: true,
        memory: MemoryLimits::NONE,
        objects: ObjectLimits::NONE,
    };
    let limit = Error::Limit.raw();
    let steps = alloc::vec![
        create(0),
        create(2),
        create(4),
        create(6),
        step(
            "a fifth channel",
            sys::CHANNEL_CREATE,
            &[Imm(slot_addr(8))],
            limit
        ),
        dup("", h(0), rights::READ, 8, OK),
        dup("", h(0), rights::READ, 9, OK),
        dup("", h(0), rights::READ, 10, OK),
        dup("", h(0), rights::READ, 11, OK),
        dup("a thirteenth handle", h(0), rights::READ, 12, limit),
        close("", h(11), OK),
        dup("one more after a close", h(0), rights::READ, 12, OK),
        write("still working", h(2), MSG, 8, OK),
    ];
    let group = [Group {
        title: "quotas",
        steps,
    }];
    let Some(mut image) = Image::new(limits, proof_programs::script()) else {
        return false;
    };
    if image.scratch().is_none() || image.script(&group[0].steps).is_none() {
        return false;
    }
    let Some(process) = image.finish() else {
        return false;
    };
    if Process::start_thread(&process, bsp, CODE, STACK + PAGE, SCRIPT).is_err() {
        return false;
    }
    let ended = await_end(&process, tsc_hz, 10);
    let groups_ok = check_groups(com1, &group, &process.reports());
    ended && groups_ok && process.ending() == Some(Ending::Exited(0))
}

/// Queued messages are charged to the process that sent them, by the heap they hold (P11): a
/// writer reaches its byte quota and gets `LIMIT`, nothing else changes, and its peer, on
/// another processor with its own quota, goes on working. The writer's bytes come back when
/// the peer reads them, and when what is left is read or its end closes.
fn byte_quota(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    // 17 bytes round up to a 32-byte heap block. The quota holds two messages; counted
    // unrounded, a third (51 of 64 bytes) would have fit.
    let limits = Limits {
        handles: 16,
        channels: 4,
        message_bytes: 64,
        debug_calls: true,
        memory: MemoryLimits::NONE,
        objects: ObjectLimits::NONE,
    };
    let (Some(mut writer), Some(mut peer)) = (
        Image::new(limits, proof_programs::script()),
        Image::new(LIMITS, proof_programs::script()),
    ) else {
        return false;
    };
    let Ok((end_a, end_b)) = ipc::create_channel(&writer.process.account) else {
        return false;
    };
    let (Ok(ha), Ok(hb)) = (
        writer
            .process
            .grant(Object::Channel(end_a), rights::CHANNEL),
        peer.process.grant(Object::Channel(end_b), rights::CHANNEL),
    ) else {
        return false;
    };
    let (held_w, held_p) = (Imm(ha), Imm(hb));
    let writer_steps = alloc::vec![
        write("the first", held_w, MSG, 17, OK),
        write("the second", held_w, MSG, 17, OK),
        write(
            "the third is over the quota",
            held_w,
            MSG,
            17,
            Error::Limit.raw()
        ),
        wait(
            "the peer answers",
            held_w,
            signals::READABLE,
            DEADLINE_INFINITE,
            OK
        ),
        read("the answer", held_w, RBUF, 16, OK),
        write("written after the peer read", held_w, MSG, 17, OK),
    ];
    // The peer never reads the writer's last message: its exit closes the end the message
    // is queued for, and that refunds it.
    let peer_steps = alloc::vec![
        read("the first", held_p, RBUF, 32, OK),
        read("the second", held_p, RBUF, 32, OK),
        write("its answer", held_p, MSG, 16, OK),
        wait("", held_p, signals::PEER_CLOSED, DEADLINE_INFINITE, OK),
    ];
    if writer.scratch().is_none()
        || writer.script(&writer_steps).is_none()
        || peer.scratch().is_none()
        || peer.script(&peer_steps).is_none()
    {
        return false;
    }
    let (Some(writer), Some(peer)) = (writer.finish(), peer.finish()) else {
        return false;
    };
    if Process::start_thread(&writer, bsp, CODE, STACK + PAGE, SCRIPT).is_err() {
        return false;
    }
    // The peer starts once the writer has been refused and waits for the answer, so the
    // quota is full when it is proven full.
    sched::yield_until(|| writer.waiting.load(Ordering::Acquire) > 0, tsc_hz, 5);
    if Process::start_thread(&peer, far, CODE, STACK + PAGE, SCRIPT).is_err() {
        return false;
    }
    let ended = await_end(&writer, tsc_hz, 10) && await_end(&peer, tsc_hz, 10);
    let writer_group = [Group {
        title: "a writer over its byte quota",
        steps: writer_steps,
    }];
    let peer_group = [Group {
        title: "its peer, unaffected",
        steps: peer_steps,
    }];
    let groups_ok = check_groups(com1, &writer_group, &writer.reports())
        & check_groups(com1, &peer_group, &peer.reports());
    let charged = (writer.account.queued_bytes(), peer.account.queued_bytes());
    let right = ended
        && groups_ok
        && charged == (0, 0)
        && writer.ending() == Some(Ending::Exited(0))
        && peer.ending() == Some(Ending::Exited(0));
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: queued messages are charged to their sender and refunded on read and close: {} and {} bytes still charged{}",
        charged.0,
        charged.1,
        if right { "" } else { ": ipc failed" }
    );
    right
}

/// The check that a transfer cannot build a reference cycle walks up the chain of queues
/// holding an end, and the walk is bounded (`ipc::MAX_HOLDER_DEPTH`): a transfer into an end
/// nested exactly that deep is accepted, one nested a link deeper gets `LIMIT`, and the
/// process goes on.
fn transfer_depth(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let depth = ipc::MAX_HOLDER_DEPTH as u64;
    // Channel i holds its ends in slots 2i and 2i + 1. A transfer into channel 0 walks up
    // from its peer end: that end is queued in channel 1, whose peer end is queued in
    // channel 2, and so on. `extra` is a channel whose ends are sent through channel 0.
    let extra = depth + 2;
    let mut steps = Vec::new();
    for channel in 0..=extra {
        steps.push(create(2 * channel));
    }
    // Links 1 to `depth`: the peer end of channel i goes into channel i + 1.
    for channel in 0..depth {
        steps.push(copy_handle(slot_addr(2 * channel + 1), LIST));
        steps.push(send("", h(2 * channel + 2), 1, OK));
    }
    steps.push(copy_handle(slot_addr(2 * extra), LIST));
    steps.push(send("nested to the limit", h(0), 1, OK));
    // One more link, and a transfer that would have to walk it is refused.
    steps.push(copy_handle(slot_addr(2 * depth + 1), LIST));
    steps.push(send("", h(2 * depth + 2), 1, OK));
    steps.push(copy_handle(slot_addr(2 * extra + 1), LIST));
    steps.push(send("a link deeper", h(0), 1, Error::Limit.raw()));
    let group = [Group {
        title: "transfers into nested ends",
        steps,
    }];
    let Some(mut image) = Image::new(LIMITS, proof_programs::script()) else {
        return false;
    };
    if image.scratch().is_none() || image.script(&group[0].steps).is_none() {
        return false;
    }
    let Some(process) = image.finish() else {
        return false;
    };
    if Process::start_thread(&process, bsp, CODE, STACK + PAGE, SCRIPT).is_err() {
        return false;
    }
    let ended = await_end(&process, tsc_hz, 10);
    let groups_ok = check_groups(com1, &group, &process.reports());
    ended && groups_ok && process.ending() == Some(Ending::Exited(0))
}

/// A 64 KiB message, the largest the ABI allows, arrives whole.
fn big_message(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let steps = alloc::vec![
        create(0),
        write(
            "sixty-four KiB",
            h(0),
            BIG_OUT,
            MAX_MESSAGE_BYTES as u64,
            OK
        ),
        read("received", h(1), BIG_IN, MAX_MESSAGE_BYTES as u64, OK),
        value_at(
            "bytes",
            SIZES + read_sizes::BYTES_AT as u64,
            MAX_MESSAGE_BYTES as u64
        ),
    ];
    let Some(mut image) = Image::new(LIMITS, proof_programs::script()) else {
        return false;
    };
    if image.map(BIG_OUT, BIG_PAGES, Access::ReadWrite).is_none()
        || image.map(BIG_IN, BIG_PAGES, Access::ReadWrite).is_none()
        || image.scratch().is_none()
        || image.script(&steps).is_none()
    {
        return false;
    }
    let pattern = |k: u64| (k.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 13) ^ k;
    for k in 0..(MAX_MESSAGE_BYTES as u64 / 8) {
        if image.write_u64(BIG_OUT + 8 * k, pattern(k)).is_none() {
            return false;
        }
    }
    let Some(process) = image.finish() else {
        return false;
    };
    // Its memory goes with its last thread: this keeps it to be read afterwards.
    let Some(memory) = process.memory() else {
        return false;
    };
    if Process::start_thread(&process, bsp, CODE, STACK + PAGE, SCRIPT).is_err() {
        return false;
    }
    let ended = await_end(&process, tsc_hz, 10);
    let group = [Group {
        title: "a full-size message",
        steps,
    }];
    let groups_ok = check_groups(com1, &group, &process.reports());
    let mut intact = ended;
    if ended {
        for k in 0..(MAX_MESSAGE_BYTES as u64 / 8) {
            intact &= memory.read_u64(BIG_IN + 8 * k) == Some(pattern(k));
        }
    }
    let right = ended && groups_ok && intact && process.ending() == Some(Ending::Exited(0));
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: 64 KiB through a channel and back, byte for byte: {intact}{}",
        if right { "" } else { ": ipc failed" }
    );
    right
}

/// P4's register convention, the `syscall`-origin rule, and the two hostile processor-state
/// tests.
fn abi_and_entry(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let mut ok = true;

    // Every register but RAX, RCX and R11 survives a call.
    let code = proof_programs::regs();
    let Some(image) = Image::new(LIMITS, code) else {
        return false;
    };
    let Some(regs) = image.finish() else {
        return false;
    };
    if Process::start_thread(&regs, bsp, CODE, STACK + PAGE, 0).is_err() {
        return false;
    }
    let ended = await_end(&regs, tsc_hz, 5);
    let right = ended && regs.reports() == [0] && regs.ending() == Some(Ending::Exited(0));
    ok &= right;
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: registers: every register but RAX, RCX and R11 survives a system call: {right}{}",
        if right { "" } else { ": ipc failed" }
    );

    // System calls only from the code range the creator named (P4, ONBF §6.1): this process
    // is told its origin is the page below its code, so none of its calls is accepted, not
    // even `exit`. It ends by the deliberate fault at the end of its script.
    let Some(mut image) = Image::with_origin(LIMITS, proof_programs::script(), (CODE - PAGE, CODE))
    else {
        return false;
    };
    let script = alloc::vec![step("", sys::DEBUG_REPORT, &[Imm(1)], OK), fault()];
    if image.scratch().is_none() || image.script(&script).is_none() {
        return false;
    }
    let Some(pinned) = image.finish() else {
        return false;
    };
    if Process::start_thread(&pinned, bsp, CODE, STACK + PAGE, SCRIPT).is_err() {
        return false;
    }
    let ended = await_end(&pinned, tsc_hz, 5);
    let refused = pinned.reports().is_empty();
    let right =
        ended && refused && matches!(pinned.ending(), Some(Ending::Faulted { vector: 6, .. }));
    ok &= right;
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: a process's system calls are accepted only from its pinned code range: {}, and it ended by its own fault{}",
        if refused {
            "every call refused"
        } else {
            "a call got through"
        },
        if right { "" } else { ": ipc failed" }
    );

    // A program that changes GS cannot touch the kernel: a timer interrupt or system call
    // from it must find the kernel's GS.
    let gs = proof_programs::gs();
    let Some(image) = Image::new(LIMITS, gs) else {
        return false;
    };
    let Some(process) = image.finish() else {
        return false;
    };
    if Process::start_thread(&process, bsp, CODE, STACK + PAGE, 0).is_err() {
        return false;
    }
    let ended = await_end(&process, tsc_hz, 10);
    let right = ended && process.reports() == [1, 0] && process.ending() == Some(Ending::Exited(0));
    ok &= right;
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: a program that loaded a null GS was interrupted and made system calls, and the kernel used its own GS (reports {:?}){}",
        process.reports(),
        if right { "" } else { ": ipc failed" }
    );

    // A program that sets RFLAGS.AC cannot switch SMAP off in the kernel.
    let ac = proof_programs::ac();
    let Some(image) = Image::new(LIMITS, ac) else {
        return false;
    };
    let Some(process) = image.finish() else {
        return false;
    };
    let leaks_before = cpu::ac_leaks();
    if Process::start_thread(&process, bsp, CODE, STACK + PAGE, 0).is_err() {
        return false;
    }
    let ended = await_end(&process, tsc_hz, 10);
    let leaks = cpu::ac_leaks().wrapping_sub(leaks_before);
    let reported = process.reports();
    let flag_was_set = reported.first().is_some_and(|f| f & (1 << 18) != 0);
    let right = ended && flag_was_set && leaks == 0 && process.ending() == Some(Ending::Exited(0));
    ok &= right;
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: a program with RFLAGS.AC set took interrupts and made a system call: the flag reached kernel code {leaks} times, and stayed set in ring 3: {flag_was_set}{}",
        if right { "" } else { ": ipc failed" }
    );

    // A process without the developer calls cannot use them (the ABI reserves them for
    // developer builds): it cannot hold the console for the whole machine.
    let nodebug = proof_programs::nodebug();
    let limits = Limits {
        debug_calls: false,
        ..LIMITS
    };
    let Some(image) = Image::new(limits, nodebug) else {
        return false;
    };
    let Some(process) = image.finish() else {
        return false;
    };
    if Process::start_thread(&process, bsp, CODE, STACK + PAGE, 0).is_err() {
        return false;
    }
    let ended = await_end(&process, tsc_hz, 5);
    let right =
        ended && process.ending() == Some(Ending::Exited(0)) && process.reports().is_empty();
    ok &= right;
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: a process without the developer calls gets NOT_SUPPORTED: {right}{}",
        if right { "" } else { ": ipc failed" }
    );

    // With UMIP on, `sidt` from ring 3 faults: the kernel's descriptor-table addresses, which
    // reveal the KASLR slide, stay out of reach of programs.
    let sidt = proof_programs::sidt();
    let Some(image) = Image::new(LIMITS, sidt) else {
        return false;
    };
    let Some(process) = image.finish() else {
        return false;
    };
    if Process::start_thread(&process, bsp, CODE, STACK + PAGE, 0).is_err() {
        return false;
    }
    let ended = await_end(&process, tsc_hz, 5);
    let umip = cpu::umip_on();
    let refused = matches!(process.ending(), Some(Ending::Faulted { vector: 13, .. }));
    let right = ended && (!umip || refused);
    ok &= right;
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: sidt from ring 3 with UMIP {}: {}{}",
        if umip { "on" } else { "unavailable" },
        if refused {
            "refused, the process ended by vector 13"
        } else {
            "completed"
        },
        if right { "" } else { ": ipc failed" }
    );

    // A program that single-steps itself ends by #DB, which arrives on its own interrupt
    // stack: a debug exception can come with no usable kernel stack (see `cpu::init`).
    let step = proof_programs::step();
    let Some(image) = Image::new(LIMITS, step) else {
        return false;
    };
    let Some(process) = image.finish() else {
        return false;
    };
    if Process::start_thread(&process, bsp, CODE, STACK + PAGE, 0).is_err() {
        return false;
    }
    let ended = await_end(&process, tsc_hz, 5);
    let (low, high) = cpu::bsp_debug_stack();
    let frame = cpu::last_debug_frame();
    let on_its_stack = (low..high).contains(&frame);
    let right = ended
        && process.reports().is_empty()
        && on_its_stack
        && matches!(
            process.ending(),
            Some(Ending::Faulted {
                vector: 1,
                error: 0,
                address: 0
            })
        );
    ok &= right;
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: a program that single-stepped itself ended by #DB, taken on its own interrupt stack: {on_its_stack}{}",
        if right { "" } else { ": ipc failed" }
    );
    ok
}

/// A page fault during a user copy comes back as a failed copy, not a dead kernel. The
/// system-call path checks every pointer first, so nothing else ever exercises this; it
/// copies here without a check, on purpose, from a user address no address space maps.
fn copy_fault_recovered(com1: &mut Console) -> bool {
    let mut buffer = [0u8; 16];
    let before = cpu::user_copy_faults();
    let completed = cpu::probe_user_copy_fault(&mut buffer);
    let recovered = cpu::user_copy_faults().wrapping_sub(before);
    let right = !completed && recovered == 1 && buffer == [0; 16];
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: a fault inside a user copy comes back as a failed copy, not a dead kernel: {recovered} fault recovered{}",
        if right { "" } else { ": ipc failed" }
    );
    right
}

// ------------------------------------------------------------------ process and thread objects

/// Where a second thread's script starts, on the script pages.
const SECOND_SCRIPT: u64 = SCRIPT + SCRIPT_PAGES * PAGE / 2;

/// Writes handle slot `k` of a process that has already started, before the thread that reads
/// that slot does.
fn write_slot(process: &Process, k: u64, value: Handle) -> bool {
    let at = slot_addr(k);
    process
        .frame_of(at - at % PAGE)
        .is_some_and(|f| process.write_frame(f, (at % PAGE) as usize, &value.to_le_bytes()))
}

/// Handles name objects of a type, and each call asks for a type: a process or a thread is not a
/// channel, whatever rights it has. A live process or thread raises nothing; a handle to one
/// can be waited on, duplicated, carried in a message and closed like any other.
fn object_types(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let (Some(mut target), Some(mut prober)) = (
        Image::new(LIMITS, proof_programs::script()),
        Image::new(LIMITS, proof_programs::script()),
    ) else {
        return false;
    };
    let Ok((target_end, prober_end)) = ipc::create_channel(&target.process.account) else {
        return false;
    };
    let (Ok(held_t), Ok(_)) = (
        target
            .process
            .grant(Object::Channel(target_end), rights::CHANNEL),
        prober
            .process
            .grant(Object::Channel(prober_end), rights::CHANNEL),
    ) else {
        return false;
    };
    // The target stays alive for as long as the prober holds the other end.
    let target_steps = alloc::vec![wait(
        "",
        Imm(held_t),
        signals::PEER_CLOSED,
        DEADLINE_INFINITE,
        OK
    )];
    if target.scratch().is_none() || target.script(&target_steps).is_none() {
        return false;
    }
    let Some(target) = target.finish() else {
        return false;
    };
    let Ok(thread) = Process::start_thread(&target, bsp, CODE, STACK + PAGE, SCRIPT) else {
        return false;
    };
    let granted = (
        prober
            .process
            .grant(Object::Process(target.clone()), rights::PROCESS),
        prober
            .process
            .grant(Object::Process(target.clone()), rights::READ),
        prober.process.grant(Object::Thread(thread), rights::THREAD),
    );
    let (Ok(process_handle), Ok(read_only), Ok(thread_handle)) = granted else {
        return false;
    };
    let (p, weak, t) = (Imm(process_handle), Imm(read_only), Imm(thread_handle));
    let wrong = Error::WrongType.raw();
    let denied = Error::AccessDenied.raw();
    let timed_out = Error::TimedOut.raw();
    let ended = signals::TERMINATED;
    let group = [Group {
        title: "object types",
        steps: alloc::vec![
            create(20),
            write("a process is no channel", p, MSG, 8, wrong),
            read("a thread is no channel", t, RBUF, 16, wrong),
            write("the type comes before the rights", weak, MSG, 8, wrong),
            wait("a live process", p, ended, 0, timed_out),
            value_at("raises nothing", OBS, 0),
            wait("a live thread", t, ended, 0, timed_out),
            value_at("raises nothing either", OBS, 0),
            wait("waiting needs WAIT", weak, ended, 0, denied),
            dup("", p, rights::WAIT | rights::TRANSFER, 8, OK),
            wait("a duplicate waits", h(8), ended, 0, timed_out),
            dup("and cannot duplicate", h(8), rights::WAIT, 9, denied),
            copy_handle(slot_addr(8), LIST),
            send("a process handle travels", h(20), 1, OK),
            wait(
                "the sender's is gone",
                h(8),
                ended,
                0,
                Error::BadHandle.raw()
            ),
            read_handles("", h(21), OK),
            wait("the received one waits", Hd(RECV), ended, 0, timed_out),
            close("", Hd(RECV), OK),
            close("a thread handle closes", t, OK),
        ],
    }];
    if prober.scratch().is_none() || prober.script(&group[0].steps).is_none() {
        return false;
    }
    let Some(prober) = prober.finish() else {
        return false;
    };
    if Process::start_thread(&prober, bsp, CODE, STACK + PAGE, SCRIPT).is_err() {
        return false;
    }
    let done = await_end(&prober, tsc_hz, 10) && await_end(&target, tsc_hz, 10);
    let groups_ok = check_groups(com1, &group, &prober.reports());
    done && groups_ok
        && prober.ending() == Some(Ending::Exited(0))
        && target.ending() == Some(Ending::Exited(0))
}

/// A thread of one process blocked on a handle to another, on another processor, wakes when that
/// process ends, by exiting or by faulting, and sees `TERMINATED`; it stays raised.
fn process_terminated(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let mut ok = true;
    for (label, exit_step) in [("exits", None), ("faults", Some(fault()))] {
        let (Some(mut waiter), Some(mut ender)) = (
            Image::new(LIMITS, proof_programs::script()),
            Image::new(LIMITS, proof_programs::script()),
        ) else {
            return false;
        };
        let ender_steps: Vec<Step> = exit_step.into_iter().collect();
        if ender.scratch().is_none() || ender.script(&ender_steps).is_none() {
            return false;
        }
        let Some(ender) = ender.finish() else {
            return false;
        };
        let Ok(held) = waiter
            .process
            .grant(Object::Process(ender.clone()), rights::PROCESS)
        else {
            return false;
        };
        let held = Imm(held);
        let ended = signals::TERMINATED;
        let waiter_steps = alloc::vec![
            wait("woken", held, ended, DEADLINE_INFINITE, OK),
            value_at("observed", OBS, u64::from(ended)),
            wait("and it stays raised", held, ended, 0, OK),
        ];
        if waiter.scratch().is_none() || waiter.script(&waiter_steps).is_none() {
            return false;
        }
        let Some(waiter) = waiter.finish() else {
            return false;
        };
        let wakes_before = ipc::cross_cpu_wakes();
        if Process::start_thread(&waiter, far, CODE, STACK + PAGE, SCRIPT).is_err() {
            return false;
        }
        sched::yield_until(|| waiter.waiting.load(Ordering::Acquire) > 0, tsc_hz, 5);
        if Process::start_thread(&ender, bsp, CODE, STACK + PAGE, SCRIPT).is_err() {
            return false;
        }
        let done = await_end(&ender, tsc_hz, 10) && await_end(&waiter, tsc_hz, 10);
        let groups = [Group {
            title: "the waiter on a process that",
            steps: waiter_steps,
        }];
        let right_reports = check_groups(com1, &groups, &waiter.reports());
        let wakes = ipc::cross_cpu_wakes().wrapping_sub(wakes_before);
        let ender_ok = matches!(
            (exit_step.is_some(), ender.ending()),
            (false, Some(Ending::Exited(0))) | (true, Some(Ending::Faulted { vector: 6, .. }))
        );
        let right = done && right_reports && ender_ok && (far == bsp || wakes > 0);
        ok &= right;
        let _ = writeln!(
            com1,
            "origin-kernel: ipc: when a process {label}: TERMINATED reached the thread waiting on it on {}, blocked {} times; {} wake-ups crossed processors{}",
            far.named(),
            waiter.blocked(),
            wakes,
            if right { "" } else { ": ipc failed" }
        );
    }
    ok
}

/// A thread waits for another thread of its own process: `TERMINATED` on a thread is raised when
/// that thread ends, with the process and the waiter still running.
fn thread_terminated(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let Some(mut image) = Image::new(LIMITS, proof_programs::script()) else {
        return false;
    };
    if image.map(STACK_1, 1, Access::ReadWrite).is_none() {
        return false;
    }
    let Ok((a, b)) = ipc::create_channel(&image.process.account) else {
        return false;
    };
    let (Ok(ha), Ok(hb)) = (
        image.process.grant(Object::Channel(a), rights::CHANNEL),
        image.process.grant(Object::Channel(b), rights::CHANNEL),
    ) else {
        return false;
    };
    // Slot 3 holds the handle to the other thread: it exists only once that thread has started.
    let other = h(3);
    let ended = signals::TERMINATED;
    let joined_thread = alloc::vec![wait("", Imm(hb), signals::READABLE, DEADLINE_INFINITE, OK)];
    let joiner = alloc::vec![
        wait(
            "a thread still running",
            other,
            ended,
            0,
            Error::TimedOut.raw()
        ),
        write("", Imm(ha), MSG, 8, OK),
        wait("joined", other, ended, DEADLINE_INFINITE, OK),
        value_at("observed", OBS, u64::from(ended)),
        wait("and it stays raised", other, ended, 0, OK),
    ];
    if image.scratch().is_none()
        || image.script(&joiner).is_none()
        || image.write_script(SECOND_SCRIPT, &joined_thread).is_none()
    {
        return false;
    }
    let Some(process) = image.finish() else {
        return false;
    };
    let Ok(thread) = Process::start_thread(&process, far, CODE, STACK_1 + PAGE, SECOND_SCRIPT)
    else {
        return false;
    };
    let Ok(value) = process.grant(Object::Thread(thread), rights::THREAD) else {
        return false;
    };
    if !write_slot(&process, 3, value)
        || Process::start_thread(&process, bsp, CODE, STACK + PAGE, SCRIPT).is_err()
    {
        return false;
    }
    let done = await_end(&process, tsc_hz, 10);
    // Both threads report to the one log. The other thread's only status, OK, is always somewhere
    // among the first OKs: it cannot come later, because the joiner's wait ends after it.
    let mut steps = joiner;
    steps.insert(2, step("", sys::WAIT_ONE, &[], OK));
    let group = [Group {
        title: "a thread waiting for another thread",
        steps,
    }];
    let groups_ok = check_groups(com1, &group, &process.reports());
    done && groups_ok && process.ending() == Some(Ending::Exited(0))
}

/// The address space of a process goes when the last of its threads has been reaped, and only
/// then, however many references to the process remain; `TERMINATED` is raised after it.
fn memory_goes_with_the_last_thread(
    com1: &mut Console,
    cpus: &[CpuIndex],
    bsp: CpuIndex,
    tsc_hz: u64,
) -> bool {
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let Some(mut image) = Image::new(LIMITS, proof_programs::script()) else {
        return false;
    };
    if image.map(STACK_1, 1, Access::ReadWrite).is_none() {
        return false;
    }
    let Ok((end, kept)) = ipc::create_channel(&image.process.account) else {
        return false;
    };
    let Ok(held) = image.process.grant(Object::Channel(end), rights::CHANNEL) else {
        return false;
    };
    // The first thread stays until the kernel drops the far end; the second exits at once.
    let stays = alloc::vec![wait(
        "",
        Imm(held),
        signals::PEER_CLOSED,
        DEADLINE_INFINITE,
        OK
    )];
    if image.scratch().is_none()
        || image.script(&stays).is_none()
        || image.write_script(SECOND_SCRIPT, &[]).is_none()
    {
        return false;
    }
    let Some(process) = image.finish() else {
        return false;
    };
    let before = userspace::spaces_freed();
    if Process::start_thread(&process, far, CODE, STACK + PAGE, SCRIPT).is_err() {
        return false;
    }
    sched::yield_until(|| process.waiting.load(Ordering::Acquire) > 0, tsc_hz, 5);
    let Ok(second) = Process::start_thread(&process, bsp, CODE, STACK_1 + PAGE, SECOND_SCRIPT)
    else {
        return false;
    };
    // The second thread ends and is reaped here while the first still runs in the space.
    sched::yield_until(|| second.termination().raised(), tsc_hz, 5);
    let second_ended = second.termination().raised();
    sched::reap();
    let while_running = userspace::spaces_freed().wrapping_sub(before);
    let raised_early = process.termination().raised();
    drop(kept);
    sched::yield_until(|| process.termination().raised(), tsc_hz, 10);
    let raised = process.termination().raised();
    let at_terminated = userspace::spaces_freed().wrapping_sub(before);
    let reports = process.reports();
    let right = second_ended
        && while_running == 0
        && !raised_early
        && raised
        && at_terminated == 1
        && reports == [OK as u64]
        && process.memory().is_none()
        && process.ending() == Some(Ending::Exited(0));
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: a process's memory goes when its last thread is reaped, with the process object still held: {while_running} spaces freed while a thread ran, {at_terminated} when TERMINATED was raised{}",
        if right { "" } else { ": ipc failed" }
    );
    right
}

/// Process IDs are never 0, which is the kernel, and never used twice (ADR-0005 C1).
fn process_ids(com1: &mut Console) -> bool {
    const PROCESSES: usize = 16;
    let mut previous = 0;
    let mut ascending = true;
    for _ in 0..PROCESSES {
        let Some(process) = Process::new(LIMITS, (CODE, CODE + PAGE), BOOT_PROOF_PRINCIPAL) else {
            return false;
        };
        ascending &= process.id() > previous;
        previous = process.id();
        drop(process);
    }
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: process IDs: {PROCESSES} processes created and dropped in turn, each ID above the last and none of them 0: {ascending}{}",
        if ascending { "" } else { ": ipc failed" }
    );
    ascending
}

/// A thread blocked on a handle to a process completes with `CANCELED` when another thread of
/// its process closes that handle, or sends it away (ADR-0005 A2): cancellation does not
/// depend on the object being a channel.
fn cancel_on_any_object(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let mut ok = true;
    for (how, sends) in [("closes", false), ("sends away", true)] {
        // Never started, so nothing else can end the wait.
        let Some(never_runs) = Process::new(LIMITS, (CODE, CODE + PAGE), BOOT_PROOF_PRINCIPAL)
        else {
            return false;
        };
        let Ok(never_runs) = ObjectRef::try_new(never_runs) else {
            return false;
        };
        let Some(mut image) = Image::new(LIMITS, proof_programs::script()) else {
            return false;
        };
        if image.map(STACK_1, 1, Access::ReadWrite).is_none() {
            return false;
        }
        let Ok((a, b)) = ipc::create_channel(&image.process.account) else {
            return false;
        };
        let (Ok(held), Ok(carrier), Ok(_)) = (
            image
                .process
                .grant(Object::Process(never_runs.clone()), rights::PROCESS),
            image.process.grant(Object::Channel(a), rights::CHANNEL),
            image.process.grant(Object::Channel(b), rights::CHANNEL),
        ) else {
            return false;
        };
        let waited = Imm(held);
        let first = alloc::vec![wait(
            "",
            waited,
            signals::TERMINATED,
            DEADLINE_INFINITE,
            Error::Canceled.raw()
        )];
        let second = if sends {
            if image.write(LIST, &held.to_le_bytes()).is_none() {
                return false;
            }
            alloc::vec![send("", Imm(carrier), 1, OK)]
        } else {
            alloc::vec![close("", waited, OK)]
        };
        if image.scratch().is_none()
            || image.script(&first).is_none()
            || image.write_script(SECOND_SCRIPT, &second).is_none()
        {
            return false;
        }
        let Some(process) = image.finish() else {
            return false;
        };
        if Process::start_thread(&process, bsp, CODE, STACK + PAGE, SCRIPT).is_err() {
            return false;
        }
        sched::yield_until(|| process.waiting.load(Ordering::Acquire) > 0, tsc_hz, 5);
        let in_wait = process.waiting.load(Ordering::Acquire) > 0;
        if Process::start_thread(&process, far, CODE, STACK_1 + PAGE, SECOND_SCRIPT).is_err() {
            return false;
        }
        let done = await_end(&process, tsc_hz, 10);
        let canceled = process.reports().contains(&(Error::Canceled.raw() as u64));
        let right = done && in_wait && canceled && process.ending() == Some(Ending::Exited(0));
        ok &= right;
        let _ = writeln!(
            com1,
            "origin-kernel: ipc: a thread in wait_one on a process handle another thread {how} completes with {}{}",
            if canceled {
                "CANCELED"
            } else {
                "something else"
            },
            if right { "" } else { ": ipc failed" }
        );
    }
    ok
}

/// Runs the proofs of process and thread objects, with a leak check of their own: every one of
/// them is dropped by the end, and the memory is back to the frame. Called from the boot flow
/// with every scheduler running, after [`prove`], which leaves nothing behind.
pub(crate) fn prove_objects(
    com1: &mut Console,
    cpus: &[CpuIndex],
    bsp: CpuIndex,
    tsc_hz: u64,
) -> bool {
    let before = memory::stats();
    let mut ok = true;
    // First, while no other address space is being freed: it counts them.
    ok &= memory_goes_with_the_last_thread(com1, cpus, bsp, tsc_hz);
    ok &= object_types(com1, bsp, tsc_hz);
    ok &= process_terminated(com1, cpus, bsp, tsc_hz);
    ok &= thread_terminated(com1, cpus, bsp, tsc_hz);
    ok &= cancel_on_any_object(com1, cpus, bsp, tsc_hz);
    ok &= process_ids(com1);
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
        "origin-kernel: ipc: every process and thread object and all they held are gone: heap {heap_now} bytes (was {heap_before}), {frames_now} free frames (was {frames_before}){}",
        if back { "" } else { ": ipc failed" }
    );
    ok && back
}

// ------------------------------------------------------------------ a message's writer

/// The reader's `read_sizes` structures in the identity proof, each the current 32 bytes: for
/// a read too small for its message, then one per message read.
const IDENT_SMALL: u64 = DATA + 0x500;
const IDENT_FIRST: u64 = DATA + 0x520;
const IDENT_SECOND: u64 = DATA + 0x540;
/// One given the first version's 12 bytes (C2), and what fills it past them.
const IDENT_OLD: u64 = DATA + 0x560;
/// What fills each structure before the reads, so that a field the kernel did not write
/// shows.
const UNWRITTEN: u32 = 0xC5C5_C5C5;
/// The principals of the identity proof's two writers and its reader: both halves of each
/// non-zero, so a field that arrives with only 32 of its 64 bits shows (C1).
const PRINCIPAL_A: u64 = 0xA11C_E000_0000_0001;
const PRINCIPAL_B: u64 = 0x0000_0002_B0B0_0002;
const PRINCIPAL_READER: u64 = 0x5EAD_E500_0000_0003;
/// The lengths of the three messages, each different.
const FIRST_LEN: u64 = 6;
const SECOND_LEN: u64 = 8;
const THIRD_LEN: u64 = 7;

/// The `u64` a script reported as two `report32` halves, low first, starting at `at`.
fn reported_u64(reports: &[u64], at: usize) -> Option<u64> {
    let low = *reports.get(at)?;
    let high = *reports.get(at + 1)?;
    Some((low & 0xFFFF_FFFF) | (high << 32))
}

/// Reports the identity fields of the `read_sizes` at `sizes`: four `report32` steps.
fn report_identity(sizes: u64) -> [Step; 4] {
    [
        value_at("", sizes + read_sizes::SENDER_PROCESS_AT as u64, 0),
        value_at("", sizes + read_sizes::SENDER_PROCESS_AT as u64 + 4, 0),
        value_at("", sizes + read_sizes::SENDER_PRINCIPAL_AT as u64, 0),
        value_at("", sizes + read_sizes::SENDER_PRINCIPAL_AT as u64 + 4, 0),
    ]
}

/// Every message carries its writer's identity, stamped when it is written, and a read
/// reports it through the caller's `read_sizes` as far as its `size` reaches (ADR-0005 B1,
/// C1, C2). Writer A writes the first message on its end of a channel, then sends that end to
/// writer B, which writes the second and third. The reader, told by B that it is done, reads
/// them: first with a buffer too small, then each in turn, the last with the first version's
/// 12-byte `read_sizes`. Each process acts for its own principal.
fn writer_identity(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let script = proof_programs::script();
    let (Some(mut a), Some(mut b), Some(mut reader)) = (
        Image::acting_for(LIMITS, script, PRINCIPAL_A),
        Image::acting_for(LIMITS, script, PRINCIPAL_B),
        Image::acting_for(LIMITS, script, PRINCIPAL_READER),
    ) else {
        return false;
    };
    // X carries the messages, Y carries X's writing end from A to B, Z tells the reader.
    let (Ok((x0, x1)), Ok((y0, y1)), Ok((z0, z1))) = (
        ipc::create_channel(&a.process.account),
        ipc::create_channel(&a.process.account),
        ipc::create_channel(&b.process.account),
    ) else {
        return false;
    };
    let full = rights::CHANNEL;
    let (Ok(ax), Ok(ay), Ok(by), Ok(bz), Ok(rx), Ok(rz)) = (
        a.process.grant(Object::Channel(x0), full),
        a.process.grant(Object::Channel(y0), full),
        b.process.grant(Object::Channel(y1), full),
        b.process.grant(Object::Channel(z0), full),
        reader.process.grant(Object::Channel(x1), full),
        reader.process.grant(Object::Channel(z1), full),
    ) else {
        return false;
    };
    // A's end of X goes in its list to send.
    let moved = ax.to_le_bytes();
    let (ax, ay, by, bz, rx, rz) = (Imm(ax), Imm(ay), Imm(by), Imm(bz), Imm(rx), Imm(rz));
    let a_steps = alloc::vec![
        write("", ax, MSG, FIRST_LEN, OK),
        // The end the first message was written on goes to B.
        step(
            "",
            sys::CHANNEL_WRITE,
            &[ay, Imm(MSG), Imm(4), Imm(LIST), Imm(1)],
            OK
        ),
    ];
    let b_steps = alloc::vec![
        wait("", by, signals::READABLE, DEADLINE_INFINITE, OK),
        read_handles("", by, OK),
        write("", Hd(RECV), MSG, SECOND_LEN, OK),
        write("", Hd(RECV), MSG, THIRD_LEN, OK),
        write("", bz, MSG, 4, OK),
    ];
    let read_into = |sizes: u64, room: u64, want: i64| {
        step(
            "",
            sys::CHANNEL_READ,
            &[rx, Imm(RBUF), Imm(room), Imm(0), Imm(0), Imm(sizes)],
            want,
        )
    };
    let mut reader_steps = alloc::vec![
        wait("", rz, signals::READABLE, DEADLINE_INFINITE, OK),
        read_into(IDENT_SMALL, 2, Error::BufferTooSmall.raw()),
    ];
    reader_steps.extend(report_identity(IDENT_SMALL));
    reader_steps.push(read_into(IDENT_FIRST, 16, OK));
    reader_steps.extend(report_identity(IDENT_FIRST));
    reader_steps.push(read_into(IDENT_SECOND, 16, OK));
    reader_steps.extend(report_identity(IDENT_SECOND));
    reader_steps.push(read_into(IDENT_OLD, 16, OK));
    reader_steps.push(value_at("", IDENT_OLD + read_sizes::BYTES_AT as u64, 0));
    for past in [12, 16, 20, 24, 28] {
        reader_steps.push(value_at("", IDENT_OLD + past, 0));
    }
    // Each structure starts filled with UNWRITTEN, then its `size`.
    let unwritten = UNWRITTEN.to_le_bytes();
    let filled: [u8; read_sizes::SIZE as usize] =
        core::array::from_fn(|i| unwritten.get(i % 4).copied().unwrap_or(0));
    let structure = |at: u64, size: u32| {
        reader.write(at, &filled).is_some() && reader.write(at, &size.to_le_bytes()).is_some()
    };
    let structures = structure(IDENT_SMALL, read_sizes::SIZE)
        && structure(IDENT_FIRST, read_sizes::SIZE)
        && structure(IDENT_SECOND, read_sizes::SIZE)
        && structure(IDENT_OLD, read_sizes::BASE_SIZE);
    let ready = structures
        && a.scratch().is_some()
        && a.write(LIST, &moved).is_some()
        && a.script(&a_steps).is_some()
        && b.scratch().is_some()
        && b.script(&b_steps).is_some()
        && reader.scratch().is_some()
        && reader.script(&reader_steps).is_some();
    if !ready {
        return false;
    }
    let (Some(a), Some(b), Some(reader)) = (a.finish(), b.finish(), reader.finish()) else {
        return false;
    };
    let started = Process::start_thread(&reader, far, CODE, STACK + PAGE, SCRIPT).is_ok()
        && Process::start_thread(&b, far, CODE, STACK + PAGE, SCRIPT).is_ok()
        && Process::start_thread(&a, bsp, CODE, STACK + PAGE, SCRIPT).is_ok();
    let ended = started
        && await_end(&a, tsc_hz, 10)
        && await_end(&b, tsc_hz, 10)
        && await_end(&reader, tsc_hz, 10);
    let all_ok = |p: &Process, steps: &[Step]| {
        let reports = p.reports();
        p.ending() == Some(Ending::Exited(0))
            && reports.len()
                == steps
                    .iter()
                    .filter(|s| !matches!(s.want, Want::Nothing))
                    .count()
            && steps
                .iter()
                .filter(|s| !matches!(s.want, Want::Nothing))
                .zip(&reports)
                .all(|(s, &got)| match s.want {
                    Want::Status(w) => got == w as u64,
                    _ => true,
                })
    };
    let writers_ok = ended && all_ok(&a, &a_steps) && all_ok(&b, &b_steps);
    let reports = reader.reports();
    let identity_at = |at: usize| reported_u64(&reports, at).zip(reported_u64(&reports, at + 2));
    let (a_is, b_is) = (Some((a.id(), PRINCIPAL_A)), Some((b.id(), PRINCIPAL_B)));
    // Report positions: each status, then each `report32`, in script order.
    let statuses_ok = all_ok(&reader, &reader_steps);
    let too_small = identity_at(2);
    let first_seen = identity_at(7);
    let second_seen = identity_at(12);
    let counted = reports.get(17).copied();
    let untouched = reports
        .get(18..23)
        .is_some_and(|past| past.iter().all(|&w| w == u64::from(UNWRITTEN)));
    let not_reader = [too_small, first_seen, second_seen].iter().all(|seen| {
        seen.is_some_and(|(process, principal)| {
            process != reader.id() && principal != PRINCIPAL_READER
        })
    });
    let names_a = first_seen == a_is;
    let names_b = second_seen == b_is;
    let too_small_names_a = too_small == a_is;
    let identity_right =
        ended && writers_ok && statuses_ok && names_a && names_b && too_small_names_a && not_reader;
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: a message's writer: the message written before its end moved to another process names its writer, process and principal: {names_a}; the one written after names the new writer: {names_b}; a read too small for it names the writer too: {too_small_names_a}; none names the reader: {not_reader}{}",
        if identity_right { "" } else { ": ipc failed" }
    );
    if !identity_right {
        let _ = writeln!(
            com1,
            "origin-kernel: ipc: a message's writer: A is ({:#x}, {PRINCIPAL_A:#x}), B is ({:#x}, {PRINCIPAL_B:#x}); seen {too_small:x?}, {first_seen:x?}, {second_seen:x?}; ended {ended}, writers {writers_ok}, statuses {statuses_ok}",
            a.id(),
            b.id()
        );
    }
    let old_right = ended && statuses_ok && counted == Some(THIRD_LEN) && untouched;
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: a reader that gives read_sizes' first size, 12: its read -> {}, its counts -> {} bytes, and nothing written past them: {untouched}{}",
        reports
            .get(16)
            .map_or("nothing", |&s| status_name(s as i64)),
        counted.unwrap_or(0),
        if old_right { "" } else { ": ipc failed" }
    );
    identity_right && old_right
}

/// Runs the proof of a message's writer (ADR-0005 B1, C1, C2), with a leak check of its own.
/// Called from the boot flow with every scheduler running, after [`prove_objects`].
pub(crate) fn prove_identity(
    com1: &mut Console,
    cpus: &[CpuIndex],
    bsp: CpuIndex,
    tsc_hz: u64,
) -> bool {
    let before = memory::stats();
    let ok = writer_identity(com1, cpus, bsp, tsc_hz);
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
        "origin-kernel: ipc: the identity proof's processes and all they held are gone: heap {heap_now} bytes (was {heap_before}), {frames_now} free frames (was {frames_before}){}",
        if back { "" } else { ": ipc failed" }
    );
    ok && back
}

// ------------------------------------------------------------------ process start

/// The launch proof's creator's principal: both halves non-zero, so a child that took only part
/// of it, or any other, shows (C1).
const PRINCIPAL_CREATOR: u64 = 0xC4EA_7012_0000_0007;
/// The arguments and environment every launched child gets.
const LAUNCH_ARGS: [&[u8]; 2] = [b"origin", b"start record"];
const LAUNCH_ENV: [&[u8]; 1] = [b"ROLE=child"];
/// The start-record entries in the order the launch path writes them, with what each must be:
/// role, object type and rights (C5, C6).
const START_ENTRIES: [(u32, u32, u32); 4] = [
    (
        start::role::SELF_PROCESS,
        ObjectType::Process as u32,
        rights::PROCESS,
    ),
    (
        start::role::SELF_THREAD,
        ObjectType::Thread as u32,
        rights::THREAD,
    ),
    (
        start::role::BOOTSTRAP,
        ObjectType::Channel as u32,
        rights::CHANNEL,
    ),
    (
        start::role::DIRECTORY,
        ObjectType::Channel as u32,
        rights::CHANNEL,
    ),
];

/// The address of entry `i`'s handle in a start record at `record`.
const fn entry_handle(record: u64, i: u64) -> u64 {
    record + start::HEADER as u64 + i * start::ENTRY as u64 + start::ENTRY_HANDLE_AT as u64
}

/// What one launched child showed.
struct Launch {
    /// Its start record's address.
    record: u64,
    /// RDI held it, and the record it read in ring 3 is the one the launch wrote: its size,
    /// version, entries (C5, C6), arguments and environment.
    record_right: bool,
    /// Its handles did what their rights allow.
    handles_work: bool,
    /// Its message on BOOTSTRAP named it and its creator's principal.
    names_creator: bool,
    /// Its write to its own record faulted at the record's address.
    faulted_there: bool,
    /// What the launch path took: placing the record, then granting, writing and starting.
    micros: u64,
}

/// Launches one child from `creator` through the launch path, with a BOOTSTRAP end whose peer
/// the creator holds and a DIRECTORY end, and checks what it saw and did. `None` if the
/// launch itself could not run.
fn launch_one(creator: &Process, cpu: CpuIndex, tsc_hz: u64) -> Option<Launch> {
    let code = proof_programs::launched();
    let code_pages = code.len().div_ceil(PAGE as usize).max(1) as u64;
    let child = Process::child_of(creator, LIMITS, (CODE, CODE + code_pages * PAGE))?;
    let mut image = Image::from_process(child, code)?;
    let (Ok((boot_child, boot_creator)), Ok((dir_child, dir_other))) = (
        ipc::create_channel(&creator.account),
        ipc::create_channel(&creator.account),
    ) else {
        return None;
    };
    let held = creator
        .grant(Object::Channel(boot_creator), rights::CHANNEL)
        .ok()?;
    let given = Given {
        bootstrap: Some(boot_child),
        directory: Some(dir_child),
        args: &LAUNCH_ARGS,
        env: &LAUNCH_ENV,
    };
    let placing = cpu::rdtsc();
    let placed = launch::place(&mut image.process, &given).ok()?;
    let placing = cpu::rdtsc().wrapping_sub(placing);
    let record = placed.record;
    let words = placed.size as u64 / 4;
    // The child reports the RDI it started with and its whole record, word by word; uses each
    // handle; then writes its record, which must fault.
    let mut steps = alloc::vec![value_at("", ENTRY_RDI, 0), value_at("", ENTRY_RDI + 4, 0),];
    for w in 0..words {
        steps.push(value_at("", record + 4 * w, 0));
    }
    steps.extend([
        write("", Hd(entry_handle(record, 2)), MSG, 4, OK),
        dup("", Hd(entry_handle(record, 0)), rights::WAIT, 0, OK),
        wait(
            "",
            Hd(entry_handle(record, 1)),
            signals::TERMINATED,
            0,
            Error::TimedOut.raw(),
        ),
        wait("", Hd(entry_handle(record, 3)), signals::WRITABLE, 0, OK),
        copy32(DATA, record),
    ]);
    if image.scratch().is_none() || image.script(&steps).is_none() {
        return None;
    }
    let starting = cpu::rdtsc();
    let launched = launch::start(image.process, placed, given, cpu, CODE, STACK + PAGE).ok()?;
    let starting = cpu::rdtsc().wrapping_sub(starting);
    let micros = placing.saturating_add(starting) / (tsc_hz / 1_000_000).max(1);
    let ended = await_end(&launched.process, tsc_hz, 10);
    drop(dir_other);
    let reports = launched.process.reports();
    // RDI, then the record as the child read it.
    let rdi = reported_u64(&reports, 0);
    let mut bytes = Vec::new();
    for word in reports.get(2..2 + words as usize).unwrap_or(&[]) {
        bytes.extend_from_slice(&(*word as u32).to_le_bytes());
    }
    let decoded = start::decode(&bytes).ok();
    let record_right = rdi == Some(record)
        && bytes.len() == launched.size
        && decoded.is_some_and(|r| {
            let entries: Vec<start::Entry> = r.entries().collect();
            r.abi == origin_abi::ABI_VERSION
                && r.abi_min_minor == origin_abi::ABI_VERSION.1
                && entries.len() == START_ENTRIES.len()
                && entries
                    .iter()
                    .zip(START_ENTRIES)
                    .all(|(e, (role, ty, rights))| {
                        e.role == role && e.object_type == ty && e.rights == rights && e.handle != 0
                    })
                && r.args.iter().eq(LAUNCH_ARGS)
                && r.env.iter().eq(LAUNCH_ENV)
        });
    let statuses = reports.get(2 + words as usize..).unwrap_or(&[]);
    let handles_work = statuses
        == [
            OK as u64,
            OK as u64,
            Error::TimedOut.raw() as u64,
            OK as u64,
        ];
    let names_creator = match ipc::channel_read(&creator.handles, held, Need::NOTHING, 64, 0) {
        Ok(ipc::Read::Message { sender, .. }) => {
            sender.process == launched.process.id() && sender.principal == PRINCIPAL_CREATOR
        }
        _ => false,
    };
    let faulted_there = ended
        && matches!(
            launched.process.ending(),
            Some(Ending::Faulted { vector: 14, address, .. }) if address == record
        );
    Some(Launch {
        record,
        record_right,
        handles_work,
        names_creator,
        faulted_there,
        micros,
    })
}

/// A launch whose start record would pass 64 KiB is refused before anything is mapped, and a
/// launch that fails after its child holds something gives it all back: here, a child whose
/// table holds one handle, so the second start handle is refused. Each passes the BOOTSTRAP
/// end of a channel whose other end the creator holds, which must close.
fn refused_launches(creator: &Process) -> (bool, bool) {
    let code = proof_programs::launched();
    let code_pages = code.len().div_ceil(PAGE as usize).max(1) as u64;
    let origin = (CODE, CODE + code_pages * PAGE);
    let attempt = |limits: Limits, args: &[&[u8]]| -> Option<(Result<(), Error>, bool)> {
        let child = Process::child_of(creator, limits, origin)?;
        let mut image = Image::from_process(child, code)?;
        let (child_end, creator_end) = ipc::create_channel(&creator.account).ok()?;
        let held = creator
            .grant(Object::Channel(creator_end), rights::CHANNEL)
            .ok()?;
        let given = Given {
            bootstrap: Some(child_end),
            directory: None,
            args,
            env: &[],
        };
        let outcome = match launch::place(&mut image.process, &given) {
            // Refused before the child held anything: what it was to be given goes back now.
            Err(e) => {
                drop(given);
                Err(e)
            }
            Ok(placed) => launch::start(
                image.process,
                placed,
                given,
                CpuIndex::BOOT,
                CODE,
                STACK + PAGE,
            )
            .map(|_| ()),
        };
        // The end the child would have had is closed either way.
        let closed = ipc::wait_one(
            &creator.handles,
            &creator.waiting,
            held,
            Need::NOTHING,
            signals::PEER_CLOSED,
            0,
        )
        .is_ok_and(|w| w.status.is_ok());
        let _ = ipc::handle_close(creator, held, Need::NOTHING);
        Some((outcome, closed))
    };
    let large = alloc::vec![7u8; start::MAX_SIZE];
    let too_large = attempt(LIMITS, &[&large])
        .is_some_and(|(outcome, closed)| outcome == Err(Error::Limit) && closed);
    let one_handle = Limits {
        handles: 1,
        ..LIMITS
    };
    let gave_back = attempt(one_handle, &[])
        .is_some_and(|(outcome, closed)| outcome == Err(Error::Limit) && closed);
    (too_large, gave_back)
}

/// The process start proof (ADR-0005 P6, C1, C4–C6): two children launched from one creator
/// through the launch path, and two launches that must be refused, with a leak check of its
/// own. Called from the boot flow with every scheduler running, after [`prove_identity`].
pub(crate) fn prove_launch(
    com1: &mut Console,
    cpus: &[CpuIndex],
    bsp: CpuIndex,
    tsc_hz: u64,
) -> bool {
    let before = memory::stats();
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let ok = {
        let Some(creator) = Process::new(LIMITS, (CODE, CODE + PAGE), PRINCIPAL_CREATOR) else {
            return false;
        };
        let first = launch_one(&creator, far, tsc_hz);
        let second = launch_one(&creator, bsp, tsc_hz);
        let (too_large, gave_back) = refused_launches(&creator);
        let each = |check: fn(&Launch) -> bool| {
            first.as_ref().is_some_and(check) && second.as_ref().is_some_and(check)
        };
        let record_right = each(|l| l.record_right);
        let handles_work = each(|l| l.handles_work);
        let names_creator = each(|l| l.names_creator);
        let faulted_there = each(|l| l.faulted_there);
        let apart = match (&first, &second) {
            (Some(a), Some(b)) => a.record != b.record,
            _ => false,
        };
        let launched_right = record_right && handles_work && names_creator && faulted_there;
        let _ = writeln!(
            com1,
            "origin-kernel: start: two children launched from their creator found their start records in RDI, read-only at random addresses: each record, read back in ring 3, has its size, version, 4 entries with C6's rights, 2 arguments and 1 environment string: {record_right}; each start handle works: {handles_work}; each child's message names it and its creator's principal: {names_creator}; writing its record faulted there: {faulted_there}{}",
            if launched_right { "" } else { ": start failed" }
        );
        let refused_right = apart && too_large && gave_back;
        let _ = writeln!(
            com1,
            "origin-kernel: start: the two records are at different addresses: {apart}; a record over 64 KiB is refused: {too_large}; a launch refused after its child held a handle gives everything back: {gave_back}{}",
            if refused_right { "" } else { ": start failed" }
        );
        let took = |launch: &Option<Launch>| launch.as_ref().map_or(0, |l| l.micros);
        let _ = writeln!(
            com1,
            "origin-kernel: start: the launch path took {} us and {} us (placing the record, then granting, writing and starting)",
            took(&first),
            took(&second)
        );
        launched_right && refused_right
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
        "origin-kernel: start: every launched process and all they held are gone: heap {heap_now} bytes (was {heap_before}), {frames_now} free frames (was {frames_before}){}",
        if back { "" } else { ": start failed" }
    );
    ok && back
}

/// Runs every proof and checks nothing leaked. Called from the boot flow with every
/// scheduler running.
pub(crate) fn prove(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let before = memory::stats();
    let started = time::monotonic_nanos();
    let mut ok = true;
    ok &= ping_pong(com1, cpus, bsp, tsc_hz);
    ok &= hostile(com1, bsp, tsc_hz);
    ok &= hostile_memory(com1, bsp, tsc_hz);
    ok &= exit_closes(com1, cpus, bsp, tsc_hz);
    ok &= cancel(com1, cpus, bsp, tsc_hz);
    ok &= quotas(com1, bsp, tsc_hz);
    ok &= byte_quota(com1, cpus, bsp, tsc_hz);
    ok &= transfer_depth(com1, bsp, tsc_hz);
    ok &= big_message(com1, bsp, tsc_hz);
    ok &= abi_and_entry(com1, bsp, tsc_hz);
    ok &= copy_fault_recovered(com1);
    // Every process has exited and been dropped here. Once each processor has reaped what
    // its own threads left, the heap and the frame count are back where they started.
    let same = || {
        memory::stats().map(|(h, f)| (h.bytes_in_use, f))
            == before.map(|(h, f)| (h.bytes_in_use, f))
    };
    ipc::reap_closed();
    sched::yield_until(same, tsc_hz, 2);
    let back = same();
    let elapsed = time::monotonic_nanos().wrapping_sub(started) / 1_000_000;
    let (heap_now, frames_now) = memory::stats().map_or((0, 0), |(h, f)| (h.bytes_in_use, f));
    let (heap_before, frames_before) = before.map_or((0, 0), |(h, f)| (h.bytes_in_use, f));
    // Every thread's state was complete before it could be picked (`sched::spawn_with`).
    let unready = sched::unready_switches();
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: every process's memory, handles, channels and queued messages are gone: heap {heap_now} bytes (was {heap_before}), {frames_now} free frames (was {frames_before}), {elapsed} ms for the whole proof; threads run before their state existed: {unready}{}",
        if back && unready == 0 {
            ""
        } else {
            ": ipc failed"
        }
    );
    // Every call above returned a status its definition declares: a bug here is invisible from
    // ring 3, where a fallback status stands in for it.
    let (undeclared, first) = syscall::undeclared_statuses();
    let declared = undeclared == 0;
    let _ = writeln!(
        com1,
        "origin-kernel: ipc: a status its call does not declare reached the system-call boundary {undeclared} times{}{}",
        match first {
            Some((call, status)) => alloc::format!(" (the first: {} from {call})", status.name()),
            None => alloc::string::String::new(),
        },
        if declared { "" } else { ": ipc failed" }
    );
    ok && back && unready == 0 && declared
}
