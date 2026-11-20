//! The boot proofs of the framework's measured stack, `origin_frame::stack::with_measured_stack`
//! (TASKS item 68; ADR-0013, both Report: nothing the kernel does uses a measured depth yet).
//!
//! Both run one recursion of known shape on a measured stack: each level writes an array of
//! [`PAD`] words, keeps it across the next level and adds it up after, and the deepest level
//! samples the stack in a leaf that is never inlined ([`mark`]): the address of its local.
//!
//! - **At boot** ([`prove_at_boot`], before the AML proof, in the boot flow with interrupts off):
//!   at [`SHALLOW`] and [`DEEP`] levels on [`BOOT_STACK`] bytes, the depth measured must equal the
//!   depth sampled, to the byte (the coordinating session's ruling, Q7). `mark` writes two words
//!   below its saved frame pointer, its local and `black_box`'s copy of the local's address, and
//!   in this build the local takes the lower slot (`rbp-0x10`), so it is the deepest word written.
//!   Nothing runs below it: the leaf calls nothing, no interrupt comes, and the kernel's target
//!   uses no red zone, so nothing is written below the stack pointer. A build that gave the local
//!   the upper slot would measure 8 bytes more than it sampled, at both depths, with nothing wrong
//!   in the function: the proof's failure line says so, and a toolchain upgrade (CLAUDE.md §15)
//!   reads `mark`'s disassembly again (the review of TASKS item 68, T68-4). A call with an empty
//!   closure gives what a call costs.
//! - **In a thread** ([`prove_in_thread`], in `sched::prove`): a kernel thread on the boot
//!   processor runs the recursion at [`THREAD_LEVELS`] levels, and at the deepest, after its
//!   sample, yields until the boot flow has had a turn. Meanwhile the boot flow asks the framework
//!   whether the thread's own stack is in use (`stack::slot_in_use`), which it must be: the
//!   thread's saved frame is on the measured stack, and what keeps the thread's own stack from
//!   being freed is its home in its processor's table of contexts (TASKS item 70, which replaced
//!   ruling A1's table of the stacks measured calls left). The depth must be at
//!   least [`TRAP_MARGIN`] bytes below the sample: the yield's trap frame, 22 words, is written
//!   below the yielding call, and the sample's word is at most 32 bytes below that call. Once the
//!   thread has ended and been reaped, its own stack must have been given back, not leaked
//!   (`stack::slot_state`): a record of it left behind would leak it (T68-1). Its bounds
//!   count yields, not time, so a loaded host cannot fail it (TASKS item 66).
//!
//! Neither runs inside another proof's comparison of stack slots or frames (T68-5). The one at boot
//! runs before any proof that compares them. The one in a thread runs just after `sched::prove`'s
//! own comparison of held stacks and the heap ("thread reaping") has closed with a settle, and
//! before the kernel-wide bound, which settles before it reads (TASKS item 67). That settle leaves
//! no stack kept for reuse, so when the thread is reaped its stack is kept (`State::Idle`), not
//! retired, and no shootdown touches it during the proof: a lost state there is a leak, never a
//! shootdown that stalled on an oversubscribed machine (decision items 313 and 314). The stacks it
//! leaves kept for reuse are retired by the next settle. No address in the stack region is printed
//! (ADR-0004 Amendment 2, A6): only depths and sizes.
#![forbid(unsafe_code)]

use core::fmt::Write;
use core::hint::black_box;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use origin_frame::once::SetOnce;
use origin_frame::stack::{self, Measured, StackSlot, State};

use crate::console::Console;
use crate::cpu::{self, CpuIndex, rdtsc};

/// The measured stack's size in a thread: a kernel thread's.
const STACK: usize = 16 * 1024;
/// The measured stack's size at boot: about three times what the deep run needs in this build
/// (176 bytes a level), so a frame that grew by half still fits, and a Report proof does not
/// become a double fault that halts the boot (T68-6). Under `MAX_STACK`.
const BOOT_STACK: usize = 32 * 1024;
/// Words each level of the recursion writes and keeps across the next.
const PAD: usize = 16;
/// The two depths of the proof at boot.
const SHALLOW: u32 = 8;
const DEEP: u32 = 64;
/// The depth of the proof in a thread.
const THREAD_LEVELS: u32 = 32;
/// How far below the sample the proof in a thread must have been written: the yield's trap
/// frame (22 words, 176 bytes) less the 32 bytes the sampling leaf can reach below the call it
/// shares with the yield, with room to spare.
const TRAP_MARGIN: usize = 128;
/// The most the thread yields at its deepest level, waiting for the boot flow's turn.
const THREAD_YIELDS: u64 = 1_000;
/// The most turns the boot flow gives the thread before it gives up.
const BOOT_TURNS: u64 = 100_000;

/// The address of this leaf's local. The leaf writes two words below its saved frame pointer, the
/// local and `black_box`'s copy of its address; in this build the local is the lower, so its
/// address is the deepest word written at the deepest level of the recursion (see the module
/// docs, T68-4).
#[inline(never)]
fn mark() -> usize {
    let here = 0u64;
    ptr::from_ref(black_box(&here)).addr()
}

/// What the recursion found: the sum of every level's words, and the sample at its deepest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Found {
    sum: u64,
    sampled: usize,
}

/// One level of the recursion: writes [`PAD`] words, goes `n` levels deeper, then adds its words
/// to what came back. At the bottom it samples the stack and calls `at_deepest`, if given.
#[inline(never)]
fn level(n: u32, at_deepest: Option<&mut dyn FnMut()>) -> Found {
    let mut pad = [0u64; PAD];
    for (i, word) in pad.iter_mut().enumerate() {
        *word = black_box(u64::from(n) << 8 | i as u64);
    }
    black_box(&mut pad);
    let below = if n == 0 {
        let sampled = mark();
        if let Some(at_deepest) = at_deepest {
            at_deepest();
        }
        Found { sum: 0, sampled }
    } else {
        level(n - 1, at_deepest)
    };
    let mine = black_box(&pad)
        .iter()
        .fold(0u64, |sum, word| sum.wrapping_add(*word));
    Found {
        sum: below.sum.wrapping_add(mine),
        sampled: below.sampled,
    }
}

/// The sum [`level`] must return for `levels` levels below the first.
fn expected_sum(levels: u32) -> u64 {
    (0..=u64::from(levels))
        .flat_map(|n| (0..PAD as u64).map(move |i| n << 8 | i))
        .fold(0, u64::wrapping_add)
}

/// The sampled depth: from the top of the stack the sample is on down to it. `None` if the
/// sample is not in the stack region.
fn sampled_depth(sampled: usize) -> Option<usize> {
    let top = stack::slot_top(sampled as u64)?;
    usize::try_from(top.checked_sub(sampled as u64)?).ok()
}

/// The proof at boot (see the module docs): the boot flow, with interrupts off, before any
/// thread exists.
pub(crate) fn prove_at_boot(com1: &mut Console, tsc_hz: u64) -> bool {
    let began = rdtsc();
    let empty = stack::with_measured_stack(BOOT_STACK, || ());
    let call_ticks = rdtsc().wrapping_sub(began);
    let Some(empty) = empty else {
        let _ = writeln!(
            com1,
            "origin-kernel: measured stack: no stack of {BOOT_STACK} bytes could be had: measured stack failed"
        );
        return false;
    };
    let _ = write!(
        com1,
        "origin-kernel: measured stack: at boot, on {BOOT_STACK} bytes: "
    );
    let mut ok = true;
    let mut eight_more = false;
    let (mut shallow, mut deep) = (0usize, 0usize);
    for (levels, measured) in [(SHALLOW, &mut shallow), (DEEP, &mut deep)] {
        let Some(Measured {
            value: found,
            depth,
            size,
        }) = stack::with_measured_stack(BOOT_STACK, || level(levels, None))
        else {
            let _ = writeln!(com1, "no stack for {levels} levels: measured stack failed");
            return false;
        };
        let sampled = sampled_depth(found.sampled);
        let right = found.sum == expected_sum(levels);
        let exact = sampled == Some(depth) && depth < size;
        ok &= right && exact;
        eight_more |= sampled.is_some_and(|sampled| depth == sampled + 8);
        *measured = depth;
        let _ = write!(
            com1,
            "{levels} levels: {depth} bytes measured, {} sampled, value right {right}; ",
            sampled.unwrap_or(0)
        );
    }
    let _ = writeln!(
        com1,
        "{} bytes a level; an empty closure {} bytes deep, the call {} us; {}{}",
        deep.saturating_sub(shallow) / (DEEP - SHALLOW) as usize,
        empty.depth,
        call_ticks.saturating_mul(1_000_000) / tsc_hz.max(1),
        if eight_more {
            "8 bytes more than sampled may be `mark`'s two words in swapped slots, a change in the build rather than the function: read its disassembly first; "
        } else {
            ""
        },
        if ok {
            "passed"
        } else {
            "measured stack failed"
        }
    );
    ok
}

/// The thread's own stack's slot, for the boot flow's question.
static OWN_SLOT: SetOnce<StackSlot> = SetOnce::new();
/// The thread sits at its deepest level, yielding.
static AT_DEPTH: AtomicBool = AtomicBool::new(false);
/// The boot flow's turns while it waits.
static TURNS: AtomicU64 = AtomicU64::new(0);
/// The boot flow asked about the thread's own stack while the thread sat at its deepest level,
/// and what the framework answered.
static ASKED: AtomicBool = AtomicBool::new(false);
static IN_USE: AtomicBool = AtomicBool::new(false);
/// What the thread found: whether the boot flow had a turn across its yields, how many yields
/// that took, the measured and sampled depths, the stack's size, and whether the value was right.
static SWITCHED: AtomicBool = AtomicBool::new(false);
static YIELDS: AtomicU64 = AtomicU64::new(0);
static DEPTH: AtomicU64 = AtomicU64::new(0);
static SAMPLED: AtomicU64 = AtomicU64::new(0);
static SIZE: AtomicU64 = AtomicU64::new(0);
static RIGHT: AtomicBool = AtomicBool::new(false);
/// The thread has stored what it found, or found no stack.
static DONE: AtomicBool = AtomicBool::new(false);
static MEASURED: AtomicBool = AtomicBool::new(false);

/// The thread of [`prove_in_thread`].
extern "sysv64" fn worker(_: u64) -> ! {
    let here = 0u64;
    if let Some(own) = stack::slot_holding(ptr::from_ref(black_box(&here)).addr() as u64) {
        let _ = OWN_SLOT.set(own);
    }
    let (mut switched, mut yields) = (false, 0u64);
    let mut at_deepest = || {
        let before = TURNS.load(Ordering::Acquire);
        AT_DEPTH.store(true, Ordering::Release);
        while yields < THREAD_YIELDS {
            crate::sched::yield_now();
            yields += 1;
            if TURNS.load(Ordering::Acquire) != before {
                switched = true;
                break;
            }
        }
        AT_DEPTH.store(false, Ordering::Release);
    };
    let measured =
        stack::with_measured_stack(STACK, || level(THREAD_LEVELS, Some(&mut at_deepest)));
    if let Some(Measured {
        value: found,
        depth,
        size,
    }) = measured
    {
        DEPTH.store(depth as u64, Ordering::Relaxed);
        SAMPLED.store(
            sampled_depth(found.sampled).map_or(0, |s| s as u64),
            Ordering::Relaxed,
        );
        SIZE.store(size as u64, Ordering::Relaxed);
        RIGHT.store(found.sum == expected_sum(THREAD_LEVELS), Ordering::Relaxed);
        MEASURED.store(true, Ordering::Relaxed);
    }
    SWITCHED.store(switched, Ordering::Relaxed);
    YIELDS.store(yields, Ordering::Relaxed);
    crate::sched::finish(|| DONE.store(true, Ordering::Release))
}

/// The threads alive on processor `cpu`, the boot flow among them.
fn live(cpu: CpuIndex) -> usize {
    crate::sched::QUEUES
        .get(cpu.get())
        .map_or(0, |q| cpu::without_interrupts(|| q.lock().live()))
}

/// The proof in a thread (see the module docs), from the boot flow on processor `bsp` once its
/// scheduler runs.
pub(crate) fn prove_in_thread(com1: &mut Console, bsp: CpuIndex) -> bool {
    if live(bsp) != 1 || crate::sched::spawn(bsp, worker, 0).is_err() {
        let _ = writeln!(
            com1,
            "origin-kernel: measured stack: cannot start its thread: measured stack failed"
        );
        return false;
    }
    let mut turns = 0u64;
    while turns < BOOT_TURNS && !(DONE.load(Ordering::Acquire) && live(bsp) == 1) {
        if AT_DEPTH.load(Ordering::Acquire)
            && !ASKED.load(Ordering::Relaxed)
            && let Some(own) = OWN_SLOT.get()
        {
            IN_USE.store(stack::slot_in_use(*own), Ordering::Relaxed);
            ASKED.store(true, Ordering::Relaxed);
        }
        TURNS.fetch_add(1, Ordering::Release);
        crate::sched::yield_now();
        turns += 1;
    }
    // The thread's stack and FPU area, given back here rather than in a later proof's window.
    crate::sched::reap();
    let ended = DONE.load(Ordering::Acquire) && live(bsp) == 1;
    // Its own stack given back by the reap, not leaked (T68-1; see the module docs for why a lost
    // state here is a leak).
    let own = OWN_SLOT.get().and_then(|own| stack::slot_state(*own));
    let given_back = matches!(
        own,
        Some(State::Idle | State::Retired | State::Reclaiming | State::Waiting | State::Free)
    );
    let measured = MEASURED.load(Ordering::Relaxed);
    let (depth, sampled, size) = (
        DEPTH.load(Ordering::Relaxed) as usize,
        SAMPLED.load(Ordering::Relaxed) as usize,
        SIZE.load(Ordering::Relaxed) as usize,
    );
    let right = RIGHT.load(Ordering::Relaxed);
    let switched = SWITCHED.load(Ordering::Relaxed);
    let in_use = ASKED.load(Ordering::Relaxed) && IN_USE.load(Ordering::Relaxed);
    let below = depth.saturating_sub(sampled);
    let ok = ended
        && measured
        && right
        && switched
        && in_use
        && sampled != 0
        && below >= TRAP_MARGIN
        && depth < size
        && given_back;
    let _ = writeln!(
        com1,
        "origin-kernel: measured stack: in a thread, {THREAD_LEVELS} levels and a yield at the deepest: {depth} bytes measured, {sampled} sampled, {below} below the sample; switched away and back {switched} after {} yields; its own stack in use meanwhile {in_use}; value right {right}; ended {ended}; its own stack given back after the reap {given_back} ({}); {}",
        YIELDS.load(Ordering::Relaxed),
        match own {
            Some(State::Free) => "free",
            Some(State::Live) => "held",
            Some(State::Idle) => "kept for reuse",
            Some(State::Retired) => "retired",
            Some(State::Reclaiming) => "being reclaimed",
            Some(State::Waiting) => "waiting after a stalled shootdown",
            Some(State::Lost) => "lost",
            None => "no slot",
        },
        if ok {
            "passed"
        } else {
            "measured stack failed"
        }
    );
    ok
}
