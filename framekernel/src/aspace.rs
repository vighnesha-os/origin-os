//! Boot proofs of the address spaces (CLAUDE.md §28, §29; ADR-0004 H2; ADR-0013; N14 step 3,
//! S3-5a). The mechanism is the framework's (`origin_frame::userspace`, `origin_frame::vm`).
#![forbid(unsafe_code)]

use alloc::vec::Vec;
use core::fmt::Write;

use origin_frame::objref::ObjectRef;
use origin_frame::userspace::{Change, ChangeError, SharedFrame, UserFrame, UserSpace};
use origin_frame::vm::{self, KernelProbe};

use crate::console::Console;
use crate::cpu::{self, CpuIndex, Probe};
use crate::memory;
use crate::paging::{Access, LARGE_PAGE, MapError, PAGE};
use crate::process::{BOOT_PROOF_PRINCIPAL, Ending, Limits, MemoryLimits, ObjectLimits, Process};
use crate::sched;

/// Where each proof program's code runs.
const CODE: u64 = 0x40_0000;
/// Its stack page.
const STACK: u64 = 0x60_0000;
/// The page the proofs take away, restrict or share.
const DATA: u64 = 0x50_0000;
/// How the program and the kernel talk: word 0 is a count the program raises as it runs, and
/// word 1 is 2 when the kernel tells it to go on.
const TALK: u64 = 0x51_0000;
/// A page mapped only after the program has started.
const LATER: u64 = 0x52_0000;
/// Where the many-page proofs map: a region of its own.
const WIDE: u64 = 0x100_0000;
/// The second word of [`TALK`].
const GO: usize = 8;
/// What the kernel writes at [`GO`].
const GO_ON: u64 = 2;
/// What a reader program looks for: no page holds this but the reused frame.
const SECRET: u64 = 0x5EC8_E7F4_0A7B_1EED;
/// What a reader reports if it ever reads [`SECRET`].
const SAW_THE_SECRET: u32 = 0x05EC;
/// What the program that writes after a change of permissions reports.
const WROTE: u32 = 0x77;
/// What is in the page mapped under a running thread.
const LATER_VALUE: u64 = 0x1234;
/// How long the proofs wait for a program to get anywhere: nothing here depends on timing, so
/// this only has to be far above any load seen (ADR-0013 R1).
const WAIT_SECONDS: u64 = 5;
/// Rounds of the map-and-unmap loop whose cost is reported (CLAUDE.md rule 21).
const LOOP_ROUNDS: u32 = 64;

/// What a proof program may hold.
const LIMITS: Limits = Limits {
    handles: 16,
    channels: 4,
    message_bytes: 1 << 20,
    debug_calls: true,
    memory: MemoryLimits::NONE,
    objects: ObjectLimits::NONE,
};

/// `mov edi, value; mov eax, DEBUG_REPORT; syscall`.
fn report(value: u32) -> Vec<u8> {
    let mut p = alloc::vec![0xBF];
    p.extend_from_slice(&value.to_le_bytes());
    p.extend_from_slice(&[0xB8, 1, 0, 0, 0, 0x0F, 0x05]);
    p
}

/// `xor edi, edi; xor eax, eax; syscall; jmp $`: exit 0.
const EXIT: [u8; 8] = [0x31, 0xFF, 0x31, 0xC0, 0x0F, 0x05, 0xEB, 0xFE];

/// `mov rbx, TALK; mov rdx, address`: every program starts with these two.
fn talk_and(address: u64) -> Vec<u8> {
    let mut p = alloc::vec![0x48, 0xC7, 0xC3];
    p.extend_from_slice(&(TALK as u32).to_le_bytes());
    p.extend_from_slice(&[0x48, 0xC7, 0xC2]);
    p.extend_from_slice(&(address as u32).to_le_bytes());
    p
}

/// Reads [`DATA`] in a loop, as [`reader_at`] does.
fn reader() -> Vec<u8> {
    reader_at(DATA)
}

/// Reads `address` in a loop, raising its count each time, and reports [`SAW_THE_SECRET`] if
/// it ever reads [`SECRET`]. A fault at the read ends it.
fn reader_at(address: u64) -> Vec<u8> {
    let mut p = alloc::vec![0x48, 0xB9]; // movabs rcx, SECRET
    p.extend_from_slice(&SECRET.to_le_bytes());
    p.extend_from_slice(&talk_and(address)); // rbx: TALK, rdx: the page
    // loop:
    p.extend_from_slice(&[0x48, 0x8B, 0x02]); // mov rax, [rdx]
    p.extend_from_slice(&[0x48, 0x39, 0xC8]); // cmp rax, rcx
    p.extend_from_slice(&[0x74, 0x05]); // je found
    p.extend_from_slice(&[0x48, 0xFF, 0x03]); // inc qword ptr [rbx]
    p.extend_from_slice(&[0xEB, 0xF3]); // jmp loop
    // found:
    p.extend_from_slice(&report(SAW_THE_SECRET));
    p.extend_from_slice(&EXIT);
    p
}

/// Writes [`DATA`] in a loop, raising its count each time. A fault at the write ends it.
fn writer() -> Vec<u8> {
    let mut p = talk_and(DATA);
    p.extend_from_slice(&[0x31, 0xC0]); // xor eax, eax
    // loop:
    p.extend_from_slice(&[0x48, 0xFF, 0xC0]); // inc rax
    p.extend_from_slice(&[0x48, 0x89, 0x02]); // mov [rdx], rax
    p.extend_from_slice(&[0x48, 0xFF, 0x03]); // inc qword ptr [rbx]
    p.extend_from_slice(&[0xEB, 0xF5]); // jmp loop
    p
}

/// Reads [`DATA`] until the kernel says go on, then writes it and reports [`WROTE`]: whatever
/// translation it had cached was read-only.
fn write_when_told() -> Vec<u8> {
    let mut p = talk_and(DATA);
    // loop:
    p.extend_from_slice(&[0x48, 0x8B, 0x02]); // mov rax, [rdx]
    p.extend_from_slice(&[0x48, 0xFF, 0x03]); // inc qword ptr [rbx]
    p.extend_from_slice(&[0x48, 0x8B, 0x43, GO as u8]); // mov rax, [rbx + GO]
    p.extend_from_slice(&[0x48, 0x83, 0xF8, GO_ON as u8]); // cmp rax, GO_ON
    p.extend_from_slice(&[0x75, 0xF0]); // jne loop
    p.extend_from_slice(&[0x48, 0xC7, 0x02]); // mov qword ptr [rdx], WROTE
    p.extend_from_slice(&WROTE.to_le_bytes());
    p.extend_from_slice(&report(WROTE));
    p.extend_from_slice(&EXIT);
    p
}

/// Waits until the kernel says go on, then reports the word at [`LATER`], which was not mapped
/// when it started.
fn read_later_when_told() -> Vec<u8> {
    let mut p = talk_and(LATER);
    // loop:
    p.extend_from_slice(&[0x48, 0xFF, 0x03]); // inc qword ptr [rbx]
    p.extend_from_slice(&[0x48, 0x8B, 0x43, GO as u8]); // mov rax, [rbx + GO]
    p.extend_from_slice(&[0x48, 0x83, 0xF8, GO_ON as u8]); // cmp rax, GO_ON
    p.extend_from_slice(&[0x75, 0xF3]); // jne loop
    p.extend_from_slice(&[0x48, 0x8B, 0x3A]); // mov rdi, [rdx]
    p.extend_from_slice(&[0xB8, 1, 0, 0, 0, 0x0F, 0x05]); // DEBUG_REPORT
    p.extend_from_slice(&EXIT);
    p
}

/// Reads `address` once, so that its processor holds a translation for it, and then keeps off
/// it: it waits, raising its count, until the kernel says go on, and reads `address` again. A
/// fault at that read ends it. If the read succeeds, the page was still there for it: it
/// reports the word and exits.
fn read_again_when_told(address: u64) -> Vec<u8> {
    let mut p = talk_and(address); // rbx: TALK, rdx: the page
    p.extend_from_slice(&[0x48, 0x8B, 0x02]); // mov rax, [rdx]
    // loop:
    p.extend_from_slice(&[0x48, 0xFF, 0x03]); // inc qword ptr [rbx]
    p.extend_from_slice(&[0x48, 0x8B, 0x43, GO as u8]); // mov rax, [rbx + GO]
    p.extend_from_slice(&[0x48, 0x83, 0xF8, GO_ON as u8]); // cmp rax, GO_ON
    p.extend_from_slice(&[0x75, 0xF3]); // jne loop
    p.extend_from_slice(&[0x48, 0x8B, 0x3A]); // mov rdi, [rdx]
    p.extend_from_slice(&[0xB8, 1, 0, 0, 0, 0x0F, 0x05]); // DEBUG_REPORT
    p.extend_from_slice(&EXIT);
    p
}

/// A process running one of the programs above, and what the kernel holds of it.
struct Subject {
    process: ObjectRef<Process>,
    memory: ObjectRef<UserSpace>,
    talk: UserFrame,
}

impl Subject {
    /// A process with `program` at [`CODE`], a stack, and the [`TALK`] page, not started yet.
    fn new(program: &[u8]) -> Option<Self> {
        let mut process = Process::proof(LIMITS, (CODE, CODE + PAGE), BOOT_PROOF_PRINCIPAL)?;
        let code = process.map_page(CODE, Access::ReadExecute).ok()?;
        if !process.write_frame(code, 0, program) {
            return None;
        }
        process.map_page(STACK, Access::ReadWrite).ok()?;
        let talk = process.map_page(TALK, Access::ReadWrite).ok()?;
        let process = ObjectRef::try_new(process).ok()?;
        let memory = process.memory()?;
        Some(Self {
            process,
            memory,
            talk,
        })
    }

    /// Starts its thread on `cpu`.
    fn start(&self, cpu: CpuIndex) -> bool {
        Process::start_thread(&self.process, cpu, CODE, STACK + PAGE, 0).is_ok()
    }

    /// How far its count has got.
    fn count(&self) -> u64 {
        self.memory.read_u64(TALK).unwrap_or(0)
    }

    /// Waits for its count to pass `seen`: the program is running, and any page it touches
    /// in its loop has been touched.
    fn runs_past(&self, seen: u64, tsc_hz: u64) -> bool {
        sched::yield_until(|| self.count() > seen, tsc_hz, WAIT_SECONDS);
        self.count() > seen
    }

    /// Tells the program to go on.
    fn go_on(&self) -> bool {
        self.memory.write_frame(self.talk, GO, &GO_ON.to_le_bytes())
    }

    /// Waits for its thread to end, and says how.
    fn ending(&self, tsc_hz: u64) -> Option<Ending> {
        sched::yield_until(|| self.process.ending().is_some(), tsc_hz, WAIT_SECONDS);
        self.process.ending()
    }

    /// What its last `DEBUG_REPORT` said, if anything.
    fn last_report(&self) -> Option<u64> {
        self.process.reports().last().copied()
    }
}

/// `": address spaces failed"` unless `right`.
const fn verdict(right: bool) -> &'static str {
    if right { "" } else { ": address spaces failed" }
}

/// The processor a proof's program runs on, in words.
fn on_which(far: CpuIndex, bsp: CpuIndex) -> &'static str {
    if far == bsp {
        "this processor"
    } else {
        "another processor"
    }
}

/// What the change should have cost: one page, one shootdown, and one interrupt if the
/// program runs elsewhere.
fn cost_right(
    change: Result<Change, origin_frame::userspace::ChangeError>,
    far: CpuIndex,
    bsp: CpuIndex,
) -> bool {
    change
        == Ok(Change {
            pages: 1,
            shootdowns: 1,
            interrupts: u32::from(far != bsp),
        })
}

/// The frame of a page leaves the tables under a thread that is reading it, and is reused as
/// soon as the unmap returns. The thread must not see what the frame then holds: a TLB entry
/// that outlived the unmap would show it the secret.
fn unmap_before_reuse(com1: &mut Console, far: CpuIndex, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let Some(subject) = Subject::new(&reader()) else {
        return false;
    };
    let Ok(data) = subject.memory.map_page(DATA, Access::Read) else {
        return false;
    };
    if !subject.start(far) || !subject.runs_past(0, tsc_hz) {
        return false;
    }
    let phys = data.phys();
    let cost = subject.memory.unmap(DATA, 1);
    let free = memory::is_frame_free(phys);
    let reused = SharedFrame::claim(phys);
    let planted = reused.as_ref().is_some_and(|f| f.write_u64(0, SECRET));
    let ending = subject.ending(tsc_hz);
    let faulted = ending
        == Some(Ending::Faulted {
            vector: 14,
            error: 0x4,
            address: DATA,
        });
    let saw = subject.last_report() == Some(u64::from(SAW_THE_SECRET));
    drop(reused);
    let right = free && planted && faulted && !saw && cost_right(cost, far, bsp);
    let _ = writeln!(
        com1,
        "origin-kernel: address spaces: a page unmapped under a running thread on {}: frame free when the unmap returned: {free}; reused with a secret: {planted}; the thread faulted at {DATA:#x} (#PF error 0x4): {faulted}; it read the secret: {saw}; {cost:?}{}",
        on_which(far, bsp),
        verdict(right)
    );
    right
}

/// The control of the proof above, in every boot: a thread that still maps the frame does see
/// the secret when it is written there. So not reading it above means the mapping was gone.
fn a_mapped_reader_sees_the_secret(com1: &mut Console, far: CpuIndex, tsc_hz: u64) -> bool {
    let Some(subject) = Subject::new(&reader()) else {
        return false;
    };
    let Ok(data) = subject.memory.map_page(DATA, Access::Read) else {
        return false;
    };
    if !subject.start(far) || !subject.runs_past(0, tsc_hz) {
        return false;
    }
    let planted = subject.memory.write_frame(data, 0, &SECRET.to_le_bytes());
    let ending = subject.ending(tsc_hz);
    let right = planted
        && ending == Some(Ending::Exited(0))
        && subject.last_report() == Some(u64::from(SAW_THE_SECRET));
    let _ = writeln!(
        com1,
        "origin-kernel: address spaces: control: a thread whose page is still mapped reads the same secret: {right}{}",
        verdict(right)
    );
    right
}

/// Write access goes while a thread is writing: its next write must fault.
fn protect_stops_a_writer(com1: &mut Console, far: CpuIndex, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let Some(subject) = Subject::new(&writer()) else {
        return false;
    };
    if subject.memory.map_page(DATA, Access::ReadWrite).is_err()
        || !subject.start(far)
        || !subject.runs_past(0, tsc_hz)
    {
        return false;
    }
    let cost = subject.memory.protect(DATA, 1, Access::Read);
    let ending = subject.ending(tsc_hz);
    let faulted = ending
        == Some(Ending::Faulted {
            vector: 14,
            error: 0x7,
            address: DATA,
        });
    let readable = subject
        .memory
        .translate(DATA)
        .is_some_and(|t| !t.writable && t.user);
    let right = faulted && readable && cost_right(cost, far, bsp);
    let _ = writeln!(
        com1,
        "origin-kernel: address spaces: a page made read-only under a running writer on {}: the writer faulted at {DATA:#x} (#PF error 0x7): {faulted}; {cost:?}{}",
        on_which(far, bsp),
        verdict(right)
    );
    right
}

/// Write access arrives while a thread is reading the page, and is told it may write: a
/// processor still caching the old permissions must not take a fault for it, because a
/// fault ends the thread.
fn protect_lets_a_reader_write(
    com1: &mut Console,
    far: CpuIndex,
    bsp: CpuIndex,
    tsc_hz: u64,
) -> bool {
    let Some(subject) = Subject::new(&write_when_told()) else {
        return false;
    };
    if subject.memory.map_page(DATA, Access::Read).is_err()
        || !subject.start(far)
        || !subject.runs_past(0, tsc_hz)
    {
        return false;
    }
    let cost = subject.memory.protect(DATA, 1, Access::ReadWrite);
    let told = subject.go_on();
    let ending = subject.ending(tsc_hz);
    let wrote = subject.memory.read_u64(DATA) == Some(u64::from(WROTE));
    let right = told
        && wrote
        && ending == Some(Ending::Exited(0))
        && subject.last_report() == Some(u64::from(WROTE))
        && cost_right(cost, far, bsp);
    let _ = writeln!(
        com1,
        "origin-kernel: address spaces: a page made writable under a running reader on {}: it wrote without a fault: {right}; {cost:?}{}",
        on_which(far, bsp),
        verdict(right)
    );
    right
}

/// A page is mapped while the thread runs, and it reads what is there. It needs no shootdown,
/// and costs none.
fn map_under_a_running_thread(com1: &mut Console, far: CpuIndex, tsc_hz: u64) -> bool {
    let Some(subject) = Subject::new(&read_later_when_told()) else {
        return false;
    };
    if !subject.start(far) || !subject.runs_past(0, tsc_hz) {
        return false;
    }
    let mapped = subject.memory.map_page(LATER, Access::Read);
    let written = mapped.as_ref().is_ok_and(|&frame| {
        subject
            .memory
            .write_frame(frame, 0, &LATER_VALUE.to_le_bytes())
    });
    let told = subject.go_on();
    let ending = subject.ending(tsc_hz);
    let read = subject.last_report().unwrap_or(0);
    let right = written && told && ending == Some(Ending::Exited(0)) && read == LATER_VALUE;
    let _ = writeln!(
        com1,
        "origin-kernel: address spaces: a page mapped under a running thread: it read {read:#x} and exited{}",
        verdict(right)
    );
    right
}

/// A range of pages goes at once, under a thread whose processor holds a translation for the
/// last of them. Up to 128 pages is one round, and more than the 32 a shootdown can name one
/// by one is a flush of the space's translations: this proof reaches both.
///
/// The thread reads its page once and then keeps off it until the unmap has returned. So every
/// round finds it running, the count of interrupts is exact, and the fault it takes when it is
/// told to read again shows that the translation it held is gone.
///
/// It used to read its page in a loop throughout. With more than one round that made the count
/// depend on timing: an earlier round's flush drops the thread's own translation, so once the
/// last round had cleared the entries the thread could meet the missing one by itself, fault,
/// and leave the space before that round asked. No processor then held the space, and the
/// round interrupted none. CI's processors under KVM showed it on 2026-11-10 (`shootdowns: 2,
/// interrupts: 1`), with the kernel doing nothing wrong.
fn unmap_many(com1: &mut Console, pages: u64, far: CpuIndex, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let target = WIDE + (pages - 1) * PAGE;
    let Some(subject) = Subject::new(&read_again_when_told(target)) else {
        return false;
    };
    for page in 0..pages {
        if subject
            .memory
            .map_page(WIDE + page * PAGE, Access::Read)
            .is_err()
        {
            return false;
        }
    }
    if !subject.start(far) || !subject.runs_past(0, tsc_hz) {
        return false;
    }
    let cost = subject.memory.unmap(WIDE, pages);
    // Only now does the thread touch its page again.
    let told = subject.go_on();
    let faulted = subject.ending(tsc_hz)
        == Some(Ending::Faulted {
            vector: 14,
            error: 0x4,
            address: target,
        });
    let rounds = pages.div_ceil(128) as u32;
    let expected = Change {
        pages,
        shootdowns: rounds,
        interrupts: if far == bsp { 0 } else { rounds },
    };
    let right = told && faulted && cost == Ok(expected);
    let _ = writeln!(
        com1,
        "origin-kernel: address spaces: {pages} pages unmapped at once under a running thread on {}: it faulted at its page among them: {faulted}; {cost:?}{}",
        on_which(far, bsp),
        verdict(right)
    );
    right
}

/// One frame in two address spaces, each with a thread reading it. Each unmap ends only that
/// space's thread, and the frame is still in use until the last has gone: then it is free as
/// soon as the unmap returns.
fn shared_between_spaces(
    com1: &mut Console,
    first_cpu: CpuIndex,
    second_cpu: CpuIndex,
    tsc_hz: u64,
) -> bool {
    let (Some(first), Some(second), Some(frame)) = (
        Subject::new(&reader()),
        Subject::new(&reader()),
        SharedFrame::new(),
    ) else {
        return false;
    };
    let phys = frame.phys();
    if first.memory.map_shared(DATA, &frame, Access::Read).is_err()
        || second
            .memory
            .map_shared(DATA, &frame, Access::Read)
            .is_err()
    {
        return false;
    }
    let same_frame = [&first, &second]
        .iter()
        .all(|s| s.memory.frame_of(DATA).map(UserFrame::phys) == Some(phys));
    if !first.start(first_cpu)
        || !second.start(second_cpu)
        || !first.runs_past(0, tsc_hz)
        || !second.runs_past(0, tsc_hz)
    {
        return false;
    }
    // The kernel gives up its own reference: only the two mappings hold the frame now.
    drop(frame);
    let first_cost = first.memory.unmap(DATA, 1).is_ok();
    let first_faulted = first.ending(tsc_hz)
        == Some(Ending::Faulted {
            vector: 14,
            error: 0x4,
            address: DATA,
        });
    let count = second.count();
    let second_goes_on = second.runs_past(count, tsc_hz);
    let held = !memory::is_frame_free(phys);
    let second_cost = second.memory.unmap(DATA, 1).is_ok();
    let free = memory::is_frame_free(phys);
    let second_faulted = second.ending(tsc_hz)
        == Some(Ending::Faulted {
            vector: 14,
            error: 0x4,
            address: DATA,
        });
    let right = same_frame
        && first_cost
        && first_faulted
        && second_goes_on
        && held
        && second_cost
        && free
        && second_faulted;
    let _ = writeln!(
        com1,
        "origin-kernel: address spaces: a frame shared by two spaces: unmapped from the first, its thread faulted: {first_faulted}, the other went on: {second_goes_on}, the frame still in use: {held}; unmapped from the second, its thread faulted: {second_faulted}, the frame free when the unmap returned: {free}{}",
        verdict(right)
    );
    right
}

/// W^X across the mappings of one frame (review finding 5): a shared frame is never writable
/// through one mapping and executable through another, in one space or two, and never goes
/// from writable to executable in one step. Each refusal changes nothing.
fn shared_frames_keep_w_xor_x(com1: &mut Console) -> bool {
    let (Some(a), Some(b), Some(frame)) = (UserSpace::new(), UserSpace::new(), SharedFrame::new())
    else {
        return false;
    };
    fn refused<T>(outcome: Result<T, ChangeError>) -> bool {
        matches!(outcome, Err(ChangeError::WriteAndExecute))
    }
    let writable = a.map_shared(DATA, &frame, Access::ReadWrite).is_ok();
    let executable_elsewhere = refused(b.map_shared(DATA, &frame, Access::ReadExecute));
    let executable_beside = refused(a.map_shared(LATER, &frame, Access::ReadExecute));
    let read_only = b.map_shared(DATA, &frame, Access::Read).is_ok();
    let no_longer_writable = a.protect(DATA, 1, Access::Read).is_ok();
    let executable = b.protect(DATA, 1, Access::ReadExecute).is_ok();
    let writable_again = refused(a.protect(DATA, 1, Access::ReadWrite));
    let flipped = refused(b.protect(DATA, 1, Access::ReadWrite));
    let counted = frame.mappings() == (0, 1);
    let unmapped = a.unmap(DATA, 1).is_ok() && b.unmap(DATA, 1).is_ok();
    let right = writable
        && executable_elsewhere
        && executable_beside
        && read_only
        && no_longer_writable
        && executable
        && writable_again
        && flipped
        && counted
        && unmapped
        && frame.mappings() == (0, 0);
    let _ = writeln!(
        com1,
        "origin-kernel: address spaces: W^X across a shared frame's mappings: executable in another space while writable in one, refused: {executable_elsewhere}; at another address in the same space, refused: {executable_beside}; read-only beside it: {read_only}; executable once nothing writes it: {executable}; writable again while executable elsewhere, refused: {writable_again}; from executable to writable in one step, refused: {flipped}{}",
        verdict(right)
    );
    right
}

/// Where the page-table proof maps a page every 2 MiB, each needing a page table of its own.
const TABLES: u64 = 0x4000_0000;

/// A run of shared frames is mapped whole or not at all (S3-5b's review, findings B and C).
/// Each refusal comes before any entry of the run is written, so it leaves none of the run
/// mapped and no frame counted, and needs no shootdown: over a page already mapped, with a
/// frame executable in another space, and when the run needs a page table and the space has
/// made as many as one space may, which is a limit and not a want of memory.
fn a_run_is_mapped_whole_or_not_at_all(com1: &mut Console) -> bool {
    let (Some(space), Some(elsewhere)) = (UserSpace::new(), UserSpace::new()) else {
        return false;
    };
    let frames: Vec<SharedFrame> = (0..3).map_while(|_| SharedFrame::new()).collect();
    let [first, second, last] = frames.as_slice() else {
        return false;
    };
    let uncounted = || frames.iter().all(|f| f.mappings() == (0, 0));
    let unmapped =
        |base: u64, pages: u64| (0..pages).all(|k| space.translate(base + k * PAGE).is_none());
    // The run's last page is mapped already.
    let in_the_way = space.map_page(WIDE + 2 * PAGE, Access::Read).is_ok();
    let over = in_the_way
        && space.map_shared_run(WIDE, &frames, Access::ReadWrite)
            == Err(ChangeError::Map(MapError::AlreadyMapped(WIDE + 2 * PAGE)))
        && unmapped(WIDE, 2)
        && uncounted();
    // Its last frame is executable in another space: writable here would make it both.
    let executable = elsewhere
        .map_shared(DATA, last, Access::ReadExecute)
        .is_ok();
    let both = executable
        && space.map_shared_run(LATER, &frames, Access::ReadWrite)
            == Err(ChangeError::WriteAndExecute)
        && unmapped(LATER, 3)
        && first.mappings() == (0, 0)
        && second.mappings() == (0, 0)
        && last.mappings() == (0, 1);
    let both = both && elsewhere.unmap(DATA, 1).is_ok() && uncounted();
    // The space's page tables used up: from `TABLES`, each 2 MiB needs one of its own. Then a
    // run of two pages, its first in the last 2 MiB with a table and its second in the next.
    let made = (0..64u64)
        .take_while(|&k| {
            space
                .map_page(TABLES + k * LARGE_PAGE, Access::Read)
                .is_ok()
        })
        .count() as u64;
    let across = TABLES + made * LARGE_PAGE - PAGE;
    let limit = made > 0
        && space.map_shared_run(across, &[first.clone(), second.clone()], Access::ReadWrite)
            == Err(ChangeError::Limit)
        && unmapped(across, 2)
        && uncounted();
    // And a run that can be mapped is, whole.
    let base = WIDE + 3 * PAGE;
    let whole = space
        .map_shared_run(base, &frames, Access::ReadWrite)
        .is_ok()
        && frames
            .iter()
            .zip(0..)
            .all(|(f, k)| space.frame_of(base + k * PAGE).map(UserFrame::phys) == Some(f.phys()))
        && frames.iter().all(|f| f.mappings() == (1, 0));
    let whole = whole && space.unmap(base, 3).is_ok() && uncounted();
    let right = over && both && limit && whole;
    let _ = writeln!(
        com1,
        "origin-kernel: address spaces: a run of shared frames is mapped whole or not at all: over a page already mapped, refused with none of it mapped: {over}; with a frame executable in another space, refused with none of it mapped and no count taken: {both}; into 2 MiB that needs a page table once the space has made all it may, refused as a limit with none of it mapped: {limit}; mapped whole: {whole}{}",
        verdict(right)
    );
    right
}

/// A program maps and unmaps a page in a loop while a thread of its own runs on another
/// processor (done by the kernel here: no system call exists yet). Each unmap interrupts only
/// the processor that has the space loaded; the others take nothing. Reports what it costs
/// (CLAUDE.md rule 21): what a hostile loop can spend, and what the layer above must charge.
fn the_loop_interrupts_only_the_space(
    com1: &mut Console,
    cpus: &[CpuIndex],
    bsp: CpuIndex,
    far: CpuIndex,
    tsc_hz: u64,
) -> bool {
    let Some(subject) = Subject::new(&reader()) else {
        return false;
    };
    if subject.memory.map_page(DATA, Access::Read).is_err()
        || !subject.start(far)
        || !subject.runs_past(0, tsc_hz)
    {
        return false;
    }
    let others: Vec<CpuIndex> = cpus
        .iter()
        .copied()
        .filter(|&c| c != far && c != bsp)
        .collect();
    let before: Vec<u64> = others
        .iter()
        .map(|&c| origin_frame::tlb::interrupts_taken(c))
        .collect();
    let target_before = origin_frame::tlb::interrupts_taken(far);
    let loaded = subject.memory.processors_loaded();
    let started = cpu::rdtsc();
    let mut total = Change::default();
    let mut refused = false;
    for _ in 0..LOOP_ROUNDS {
        refused |= subject.memory.map_page(LATER, Access::ReadWrite).is_err();
        match subject.memory.unmap(LATER, 1) {
            Ok(change) => {
                total.pages += change.pages;
                total.shootdowns += change.shootdowns;
                total.interrupts += change.interrupts;
            }
            Err(_) => refused = true,
        }
    }
    let cycles = cpu::rdtsc().wrapping_sub(started);
    let interrupted = origin_frame::tlb::interrupts_taken(far).wrapping_sub(target_before);
    let elsewhere = others
        .iter()
        .zip(&before)
        .map(|(&c, &b)| origin_frame::tlb::interrupts_taken(c).wrapping_sub(b))
        .sum::<u64>();
    // It is still running, and its own page is what ends it.
    let still_running = subject.still_running(tsc_hz);
    let unmapped = subject.memory.unmap(DATA, 1).is_ok();
    let ended = subject.ending(tsc_hz).is_some();
    let rounds = u64::from(LOOP_ROUNDS);
    let expected = if far == bsp { 0 } else { rounds };
    let right = !refused
        && still_running
        && unmapped
        && ended
        && total.pages == rounds
        && u64::from(total.shootdowns) == rounds
        && u64::from(total.interrupts) == expected
        && elsewhere == 0
        && (far == bsp || interrupted >= rounds)
        && (far == bsp || loaded == 1);
    let micros = cycles / (tsc_hz / 1_000_000).max(1);
    let _ = writeln!(
        com1,
        "origin-kernel: address spaces: {rounds} maps and unmaps of a page under a thread on {}: {} interrupts, {elsewhere} on processors not in the space; {} us each (QEMU, no budget){}",
        on_which(far, bsp),
        total.interrupts,
        micros / rounds.max(1),
        verdict(right)
    );
    right
}

impl Subject {
    /// Whether its thread is still running a moment from now: its loop does not end by itself.
    fn still_running(&self, tsc_hz: u64) -> bool {
        let count = self.count();
        self.runs_past(count, tsc_hz) && self.process.ending().is_none()
    }
}

/// What reading `addr` does with `space` loaded on this processor: the framework's visit, with
/// interrupts off from the switch to the switch back and the page's translation dropped first
/// (review findings 2 and 4).
fn read_from(space: &UserSpace, addr: u64) -> Probe {
    space.probe_read(addr)
}

/// ADR-0004 H2: the kernel half of every address space is tables allocated once at boot.
/// - Every process space's PML4 entries 256–511 are the fixed ones, and the kernel's own
///   tables still hold them, whatever has been mapped since the kernel went live.
/// - The test of the defect this rule closes: a space is created first, a kernel mapping is
///   then made in a top-level slot of its own, and the kernel, on that space's tables, reads it.
/// - The control, in every boot: from a space that lacks that slot, which is what every space
///   used to be, the same read faults. So the read can tell the two apart. The framework builds
///   that space and drops it within the read (`KernelProbe::read_without_its_slot`).
pub(crate) fn prove_kernel_half(com1: &mut Console) -> bool {
    let no_memory = |com1: &mut Console| {
        let _ = writeln!(
            com1,
            "origin-kernel: address spaces: no memory for the kernel-half proof: address spaces failed"
        );
        false
    };
    let Some(space) = UserSpace::new() else {
        return no_memory(com1);
    };
    let Some(probe) = KernelProbe::new() else {
        let _ = writeln!(
            com1,
            "origin-kernel: address spaces: cannot map the probe page: address spaces failed"
        );
        return false;
    };
    let addr = probe.addr();
    let fixed = space.shares_kernel_half() && vm::kernel_half_unchanged();
    // The control can only work if no translation of the probe outlives a CR3 load (finding 4).
    let local = vm::translate(addr).is_some_and(|t| !t.global);
    let from_space = read_from(&space, addr);
    let Some(from_lacking) = probe.read_without_its_slot() else {
        return no_memory(com1);
    };
    let visible = from_space == Probe::Completed;
    let control = matches!(from_lacking, Probe::Faulted { error: 0, address } if address == addr);
    let right = fixed && local && visible && control;
    let _ = writeln!(
        com1,
        "origin-kernel: address spaces: the kernel half is the same tables in every space: {fixed}; a kernel mapping made at {addr:#x}, in a top-level slot of its own, after a space was created is visible from that space: {visible}; from a space lacking the slot, the same read faults: {control}; the probe page is not global: {local}{}",
        if right { "" } else { ": address spaces failed" }
    );
    right
}

/// Address spaces that change under their own running threads, frames shared between spaces
/// and freed only after the shootdown, and shootdowns that reach only the processors that need
/// them (CLAUDE.md §28; S3-5a). Runs with every scheduler running, and leaves nothing behind.
pub(crate) fn prove(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let before = memory::stats().map(|(h, f)| (h.bytes_in_use, f));
    let mut others = cpus.iter().copied().filter(|&c| c != bsp);
    let far = others.next().unwrap_or(bsp);
    let third = others.next().unwrap_or(far);
    let mut ok = unmap_before_reuse(com1, far, bsp, tsc_hz);
    ok &= a_mapped_reader_sees_the_secret(com1, far, tsc_hz);
    ok &= protect_stops_a_writer(com1, far, bsp, tsc_hz);
    ok &= protect_lets_a_reader_write(com1, far, bsp, tsc_hz);
    ok &= map_under_a_running_thread(com1, far, tsc_hz);
    ok &= unmap_many(com1, 5, far, bsp, tsc_hz);
    ok &= unmap_many(com1, 200, far, bsp, tsc_hz);
    ok &= shared_between_spaces(com1, far, third, tsc_hz);
    ok &= shared_frames_keep_w_xor_x(com1);
    ok &= a_run_is_mapped_whole_or_not_at_all(com1);
    ok &= the_loop_interrupts_only_the_space(com1, cpus, bsp, far, tsc_hz);
    // Each processor reaps what its own threads left, on its own time.
    let same = || memory::stats().map(|(h, f)| (h.bytes_in_use, f)) == before;
    sched::yield_until(same, tsc_hz, WAIT_SECONDS);
    let back = same();
    let (heap_now, frames_now) = memory::stats().map_or((0, 0), |(h, f)| (h.bytes_in_use, f));
    let (heap_before, frames_before) = before.unwrap_or((0, 0));
    let _ = writeln!(
        com1,
        "origin-kernel: address spaces: every process and frame of these proofs is gone: heap {heap_now} bytes (was {heap_before}), {frames_now} free frames (was {frames_before}){}",
        verdict(back)
    );
    ok && back
}
