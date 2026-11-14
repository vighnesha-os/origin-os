//! The kernel's random-number generator (CLAUDE.md §59; ADR-0016; TASKS 18b).
//!
//! **This is the only way the kernel gets a random value.** The two instructions, RDSEED and
//! RDRAND, are run here and nowhere else in the kernel (a gate in `cargo xtask check` holds
//! that), and what they return never leaves this file: it goes through the health tests into a
//! seed. Every value handed out comes from NIST's HMAC_DRBG over SHA-512 (`origin-crypto`),
//! under the policy in `origin-random`: a root generator, and one for each processor, which the
//! root seeds.
//!
//! What this file adds to that policy is the machine:
//!
//! - **the instructions**, each in its 64-bit form;
//! - **static storage** for each processor's generator and for the root, so a state is never
//!   on a stack (ADR-0016 R9 Z1);
//! - **the step**: every piece of the generator's work runs with interrupts off, on a stack that
//!   belongs to the generator, one for each processor. Before interrupts come back on, the
//!   whole of that stack is overwritten and the scratch registers are cleared (R9 Z3; TASKS
//!   18b S1 and S5). So a draw, a seed or a state never rests on a thread's stack, and the
//!   longest time the generator keeps interrupts off is one step, whatever the retry budget.
//!
//! **What a caller sees** (R11): [`fill`], [`next_u64`] and [`fill_fresh`], each returning a
//! `Result`. With no generator every one of them returns [`Error::NoGenerator`]: there is no
//! value that stands for "none", and no fall-back (R8).
//!
//! **Safety of the safe functions.** A step takes the processor's storage by `try_lock` and
//! marks the processor's generator stack as in use. A request that arrives in the middle of
//! a step on the same processor, which only a non-maskable interrupt or a machine check can
//! bring, finds both taken and is refused: it never runs on a stack that is in use.
//!
//! **What is not covered.** A non-maskable interrupt or a machine check that arrives in the
//! middle of a step saves the interrupted registers on its own stack, which is not overwritten
//! (TASKS 18b S7). No processor goes offline today, so no generator is uninstantiated while
//! the kernel runs.

use alloc::vec::Vec;
use core::arch::asm;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use origin_random::{Busy, Ctx, Node, Platform, Root, Source, seed};

pub use origin_random::{Condition, Error, Instruction};

use crate::cpu::{self, CpuIndex};
use crate::features::Features;
use crate::monotonic;
use crate::once::SetOnce;
use crate::percpu::PerCpu;
use crate::stack::GuardedStack;
use crate::sync::SpinLock;

/// The generator's stack on each processor. The deepest step in today's build uses under half
/// of it; the boot proof measures that at every boot.
const STACK_BYTES: usize = 8 * 1024;

/// What a processor's storage holds under its lock.
struct State {
    node: Node,
    /// The seed area while the processor starts; empty afterwards.
    area: Vec<u8>,
}

/// One processor's generator. A whole number of cache lines, so no two processors share one
/// (R7).
#[repr(align(64))]
pub(crate) struct Slot {
    state: SpinLock<State>,
    /// The generator's stack on this processor.
    stack: SetOnce<GuardedStack>,
    /// A step is running on it.
    in_step: AtomicBool,
    /// For the boot proof alone: the next step leaves the stack as it was.
    keep_stack: AtomicBool,
    /// Which instructions the processor reports: bit 0 RDSEED, bit 1 RDRAND.
    reports: AtomicU8,
    /// The processor's start has finished.
    started: AtomicBool,
    /// What the last step left, for reports: see [`Slot::publish`].
    summary: AtomicU64,
    /// Draws of each instruction that have passed its tests.
    accepted_rdseed: AtomicU64,
    accepted_rdrand: AtomicU64,
    /// The longest step while the processor started, in time-stamp counter ticks.
    longest_start: AtomicU64,
    /// The longest step since.
    longest: AtomicU64,
    /// Steps run.
    steps: AtomicU64,
}

impl Slot {
    pub(crate) const fn new() -> Self {
        Self {
            state: SpinLock::new(State {
                node: Node::new(),
                area: Vec::new(),
            }),
            stack: SetOnce::new(),
            in_step: AtomicBool::new(false),
            keep_stack: AtomicBool::new(false),
            reports: AtomicU8::new(0),
            started: AtomicBool::new(false),
            summary: AtomicU64::new(0),
            accepted_rdseed: AtomicU64::new(0),
            accepted_rdrand: AtomicU64::new(0),
            longest_start: AtomicU64::new(0),
            longest: AtomicU64::new(0),
            steps: AtomicU64::new(0),
        }
    }

    /// Publishes what a report may say about `node`: nothing secret.
    fn publish(&self, node: &Node) {
        let counts = node.counts();
        let summary = u64::from(condition_code(node.condition(Instruction::Rdseed)))
            | u64::from(condition_code(node.condition(Instruction::Rdrand))) << 4
            | u64::from(node.has_generator()) << 8
            | u64::from(counts.reseeds_rdseed.min(0xFFFF)) << 16
            | u64::from(counts.reseeds_rdrand.min(0xFFFF)) << 32
            | u64::from(counts.reseeds_root.min(0xFFFF)) << 48;
        self.summary.store(summary, Ordering::Relaxed);
        self.accepted_rdseed
            .store(node.accepted(Instruction::Rdseed), Ordering::Relaxed);
        self.accepted_rdrand
            .store(node.accepted(Instruction::Rdrand), Ordering::Relaxed);
    }
}

const fn condition_code(condition: Condition) -> u8 {
    match condition {
        Condition::Absent => 0,
        Condition::Untested => 1,
        Condition::Testing => 2,
        Condition::Healthy => 3,
        Condition::Unhealthy => 4,
    }
}

const fn condition_of(code: u64) -> Condition {
    match code & 0xF {
        1 => Condition::Untested,
        2 => Condition::Testing,
        3 => Condition::Healthy,
        4 => Condition::Unhealthy,
        _ => Condition::Absent,
    }
}

// The seed the loader hands over is what the policy takes as a root's extra bytes.
const _: () = assert!(seed::EXTRA_LEN as u64 == origin_boot_info::BOOT_SEED_LEN);

/// Every processor's generator (R7: one slot for each processor index; the boot processor's in
/// the kernel's image, the others' allocated once at discovery and never freed, as R7 foresaw:
/// ADR-0008 Amendment 1, D1).
pub(crate) static SLOTS: PerCpu<Slot> =
    PerCpu::new("origin_frame::random::SLOTS", Slot::new(), Slot::new);
/// The root (R7): behind a lock that is held for one step and never across a draw.
static ROOT: SpinLock<Root> = SpinLock::new(Root::new());
/// There is a root: requests can be answered.
static AVAILABLE: AtomicBool = AtomicBool::new(false);
/// The boot's proof of the generator failed: every request is refused from then on.
static WITHHELD: AtomicBool = AtomicBool::new(false);
/// The boot processor's start has run.
static BOOT_STARTED: AtomicBool = AtomicBool::new(false);

/// One `RDSEED` attempt in its 64-bit form: the value if the instruction reported success.
fn rdseed64() -> Option<u64> {
    let (value, ok): (u64, u8);
    // SAFETY: RDSEED writes its destination register and the flags, reads no memory, and is
    // defined on a processor that reports it in CPUID, which `Hardware` checks before it calls
    // this. The carry flag is its answer, so the block does not promise to keep the flags.
    unsafe {
        asm!(
            "rdseed {v}",
            "setc {ok}",
            v = out(reg) value,
            ok = out(reg_byte) ok,
            options(nomem, nostack),
        );
    }
    if ok == 1 { Some(value) } else { None }
}

/// One `RDRAND` attempt in its 64-bit form.
fn rdrand64() -> Option<u64> {
    let (value, ok): (u64, u8);
    // SAFETY: as `rdseed64`: RDRAND has the same effect and the same CPUID gate.
    unsafe {
        asm!(
            "rdrand {v}",
            "setc {ok}",
            v = out(reg) value,
            ok = out(reg_byte) ok,
            options(nomem, nostack),
        );
    }
    if ok == 1 { Some(value) } else { None }
}

const REPORTS_RDSEED: u8 = 1 << 0;
const REPORTS_RDRAND: u8 = 1 << 1;

/// This processor's two instructions.
struct Hardware(u8);

impl Source for Hardware {
    fn reports(&self, which: Instruction) -> bool {
        match which {
            Instruction::Rdseed => self.0 & REPORTS_RDSEED != 0,
            Instruction::Rdrand => self.0 & REPORTS_RDRAND != 0,
        }
    }

    fn try_once(&mut self, which: Instruction) -> Option<u64> {
        if !self.reports(which) {
            return None;
        }
        match which {
            Instruction::Rdseed => rdseed64(),
            Instruction::Rdrand => rdrand64(),
        }
    }
}

/// What [`on_stack`] calls on the generator's stack.
struct Call<'a> {
    run: &'a mut dyn FnMut(),
}

/// The first frame on the generator's stack.
extern "sysv64" fn enter(call: *mut Call<'_>) {
    // SAFETY: `on_stack` passes a pointer to a `Call` that lives in its own frame for the whole
    // of this call, and nothing else refers to it meanwhile.
    let call = unsafe { &mut *call };
    (call.run)();
}

/// Runs `run` on `stack`, then clears the scratch registers and, with `overwrite`, writes
/// zeros over the whole of `stack`.
///
/// # Safety
///
/// Interrupts are off on this processor, and nothing is running on `stack`: its owner's
/// `in_step` was taken by the caller.
unsafe fn on_stack(stack: &GuardedStack, run: &mut dyn FnMut(), overwrite: bool) {
    let mut call = Call { run };
    let top = stack.top();
    let low = stack.top().wrapping_sub(stack.bytes() as u64);
    let words = if overwrite {
        (stack.bytes() / 8) as u64
    } else {
        0
    };
    // SAFETY: `stack` is mapped, writable memory of `stack.bytes()` bytes ending at its
    // 16-byte-aligned top, with a guard page below, and by the contract above nothing else
    // uses it. The block saves this stack pointer in a callee-saved register, runs `enter` on
    // the other stack and comes back; `enter` is an ordinary System V function, so it leaves
    // the callee-saved registers and the direction flag as it found them. `rep stosq` then
    // writes `words` zero quadwords from `low` upwards, which is exactly that stack and holds
    // nothing live any more. The registers the call may have left values in are cleared. The
    // flags are not kept, and the block says so by not declaring that they are.
    unsafe {
        asm!(
            "mov r12, rsp",
            "mov rsp, {top}",
            "call {enter}",
            "mov rsp, r12",
            "mov rdi, r13",
            "mov rcx, r14",
            "xor eax, eax",
            "rep stosq",
            "xor ecx, ecx",
            "xor edx, edx",
            "xor esi, esi",
            "xor edi, edi",
            "xor r8d, r8d",
            "xor r9d, r9d",
            "xor r10d, r10d",
            "xor r11d, r11d",
            enter = sym enter,
            top = in(reg) top,
            inout("rdi") ptr::from_mut(&mut call) => _,
            in("r13") low,
            in("r14") words,
            out("r12") _,
            clobber_abi("sysv64"),
        );
    }
}

/// Runs `work` as one step on the processor the caller is on: interrupts off, on the
/// generator's stack, which is overwritten afterwards. `None` if this processor has no
/// generator stack, or a step is already running on it.
fn step_here<R>(work: impl FnOnce(&'static Slot, u32, u64, u64) -> R) -> Option<R> {
    cpu::without_interrupts(|| {
        let index = cpu::this_cpu();
        let slot = SLOTS.get(index.get())?;
        let stack = slot.stack.get()?;
        if slot.in_step.swap(true, Ordering::Acquire) {
            return None;
        }
        let began = cpu::rdtsc();
        let now = monotonic::monotonic_nanos();
        let mut work = Some(work);
        let mut result = None;
        let mut run = || {
            if let Some(work) = work.take() {
                result = Some(work(slot, index.get() as u32, now, began));
            }
        };
        let overwrite = !slot.keep_stack.load(Ordering::Relaxed);
        // SAFETY: interrupts are off (`without_interrupts`), and `in_step` was clear and is now
        // set, so no step is running on this processor's generator stack; no other processor
        // ever runs on it.
        unsafe { on_stack(stack, &mut run, overwrite) };
        let ticks = cpu::rdtsc().wrapping_sub(began);
        let longest = if slot.started.load(Ordering::Relaxed) {
            &slot.longest
        } else {
            &slot.longest_start
        };
        longest.fetch_max(ticks, Ordering::Relaxed);
        slot.steps.fetch_add(1, Ordering::Relaxed);
        slot.in_step.store(false, Ordering::Release);
        result
    })
}

/// The kernel as the policy's platform.
struct Kernel {
    /// The caller came with interrupts off.
    quick: bool,
}

impl Kernel {
    fn here() -> Self {
        Self {
            quick: !cpu::interrupts_enabled(),
        }
    }
}

impl Platform for Kernel {
    fn step(&mut self, step: &mut dyn FnMut(&mut Ctx<'_>)) -> Result<(), Busy> {
        step_here(|slot, index, now, tsc| {
            let Some(mut state) = slot.state.try_lock() else {
                return Err(Busy::Node);
            };
            let state = &mut *state;
            let mut source = Hardware(slot.reports.load(Ordering::Relaxed));
            let mut ctx = Ctx {
                node: &mut state.node,
                source: &mut source,
                area: &mut state.area,
                index,
                now,
                tsc,
            };
            step(&mut ctx);
            slot.publish(&state.node);
            Ok(())
        })
        .unwrap_or(Err(Busy::Node))
    }

    fn root_step(&mut self, step: &mut dyn FnMut(&mut Ctx<'_>, &mut Root)) -> Result<(), Busy> {
        step_here(|slot, index, now, tsc| {
            let Some(mut state) = slot.state.try_lock() else {
                return Err(Busy::Node);
            };
            let Some(mut root) = ROOT.try_lock() else {
                return Err(Busy::Root);
            };
            let state = &mut *state;
            let mut source = Hardware(slot.reports.load(Ordering::Relaxed));
            let mut ctx = Ctx {
                node: &mut state.node,
                source: &mut source,
                area: &mut state.area,
                index,
                now,
                tsc,
            };
            step(&mut ctx, &mut root);
            slot.publish(&state.node);
            if root.is_instantiated() {
                AVAILABLE.store(true, Ordering::Release);
            }
            Ok(())
        })
        .unwrap_or(Err(Busy::Node))
    }

    fn pause(&mut self) {
        core::hint::spin_loop();
    }

    fn quick(&self) -> bool {
        self.quick
    }
}

/// Writes zeros over `bytes` with volatile writes, which the compiler never removes.
fn overwrite(bytes: &mut [u8]) {
    for byte in bytes {
        // SAFETY: `byte` is a valid, exclusive reference to one byte.
        unsafe { ptr::from_mut(byte).write_volatile(0) };
    }
}

/// Gives processor `index` what its generator needs before it starts: its stack and its seed
/// area. The boot processor does this for every processor, itself included, so that a
/// processor's own start of its generator allocates nothing and maps nothing.
pub(crate) fn prepare(index: CpuIndex) -> bool {
    let Some(slot) = SLOTS.get(index.get()) else {
        return false;
    };
    if slot.stack.get().is_none() {
        let Some(stack) = GuardedStack::new(STACK_BYTES) else {
            return false;
        };
        if slot.stack.set(stack).is_err() {
            return false;
        }
    }
    let mut area = Vec::new();
    if area.try_reserve_exact(seed::AREA_LEN).is_err() {
        return false;
    }
    area.resize(seed::AREA_LEN, 0);
    cpu::without_interrupts(|| slot.state.lock().area = area);
    true
}

/// Records which instructions this processor reports.
fn read_reports(slot: &Slot) {
    let features = Features::decode(&cpu::gather_cpuid());
    let mut reports = 0;
    if features.rdseed {
        reports |= REPORTS_RDSEED;
    }
    if features.rdrand {
        reports |= REPORTS_RDRAND;
    }
    slot.reports.store(reports, Ordering::Relaxed);
}

/// Overwrites and frees this processor's seed area, and marks its start as finished.
///
/// It takes the lock outright. Only a step on this processor holds it, and none is running:
/// this is the processor's own start, outside any step, with interrupts off for the taking.
fn finish_start(slot: &Slot) {
    let mut area = cpu::without_interrupts(|| core::mem::take(&mut slot.state.lock().area));
    overwrite(&mut area);
    slot.started.store(true, Ordering::Release);
}

/// What became of the loader's seed (boot info 0.6; R10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoaderSeed {
    /// The loader gave none.
    None,
    /// The boot info names a secret area that does not lie where one may: it was not read.
    Refused,
    /// Its 64 bytes went into the root's first seed. The whole secret area, the seed and the
    /// stack value after it, was then overwritten where it lay, or, if `overwritten` is false,
    /// could not be.
    Mixed {
        /// Whether all 72 bytes read back as zeros afterwards.
        overwritten: bool,
    },
}

/// What became of the value the loader handed over for the kernel stacks' region (boot info
/// 0.6; ADR-0016 R10 Amendment 1, decision item 166).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackValue {
    /// There was none: the loader named no secret area, or one that does not lie where it may.
    None,
    /// It was taken, once, before the area was overwritten.
    Taken,
    /// Nothing took it before the area was overwritten with the seed.
    Unclaimed,
}

const STACK_VALUE_THERE: u8 = 0;
const STACK_VALUE_TAKEN: u8 = 1;
const STACK_VALUE_GONE: u8 = 2;
/// Whether the stack value may still be read: there, taken, or overwritten untaken.
static STACK_VALUE: AtomicU8 = AtomicU8::new(STACK_VALUE_THERE);

/// The physical address of the loader's secret area, if the boot info names one that lies
/// where it may.
fn secret_area() -> Option<u64> {
    crate::early::boot_data()?.boot_seed()
}

/// The random value the loader handed over for the kernel stacks' region: 64 bits of its
/// generator's output, which lie directly after its seed (decision item 166).
///
/// **Once, and early.** The first call returns the value; every later call returns `None`,
/// and so does every call after the generator's start has overwritten the area. No instruction
/// is run and no generator is asked: this is for the code that places that region while the
/// kernel builds its page tables, before the kernel's generator exists. `None` also when the
/// loader handed over nothing, which is the machine with no healthy instruction: the region
/// then stays at its fixed base (ADR-0004 H3).
///
/// The value is the caller's to keep to itself. Nothing prints it.
pub fn take_stack_value() -> Option<u64> {
    let area = secret_area()?;
    STACK_VALUE
        .compare_exchange(
            STACK_VALUE_THERE,
            STACK_VALUE_TAKEN,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .ok()?;
    let source = ptr::with_exposed_provenance::<u8>(crate::phys::to_virt(
        area.wrapping_add(origin_boot_info::BOOT_SEED_LEN),
    ) as usize);
    let mut bytes = [0u8; origin_boot_info::BOOT_STACK_VALUE_LEN as usize];
    for (at, byte) in bytes.iter_mut().enumerate() {
        // SAFETY: `boot_data` took the area only from inside a region of kind `BootInfo`,
        // which is RAM the loader left to the kernel and the direct map holds, and only clear
        // of the boot-info header and its two arrays; these are the 8 bytes after its first
        // 64. They have not been overwritten: that happens only after `STACK_VALUE` has left
        // `STACK_VALUE_THERE`, and this call is the one that moved it.
        *byte = unsafe { source.wrapping_add(at).read_volatile() };
    }
    Some(u64::from_le_bytes(bytes))
}

/// What became of the loader's value for the kernel stacks' region.
pub fn stack_value() -> StackValue {
    if secret_area().is_none() {
        return StackValue::None;
    }
    match STACK_VALUE.load(Ordering::Acquire) {
        STACK_VALUE_TAKEN => StackValue::Taken,
        _ => StackValue::Unclaimed,
    }
}

/// What [`take_loader_seed`] found, for [`loader_seed`]: 0 none, 1 refused, 2 mixed in and
/// overwritten, 3 mixed in and still there.
static LOADER_SEED: AtomicU8 = AtomicU8::new(0);

/// What became of the loader's seed.
pub fn loader_seed() -> LoaderSeed {
    match LOADER_SEED.load(Ordering::Acquire) {
        1 => LoaderSeed::Refused,
        2 => LoaderSeed::Mixed { overwritten: true },
        3 => LoaderSeed::Mixed { overwritten: false },
        _ => LoaderSeed::None,
    }
}

/// Moves the loader's 64 bytes from the boot-info region into the boot processor's seed area,
/// where the root's first seed takes them (R10), and overwrites them where they lay. Called
/// once, on the boot processor, before any other processor runs.
fn take_loader_seed() -> LoaderSeed {
    let Some(data) = crate::early::boot_data() else {
        return LoaderSeed::None;
    };
    if data.info().boot_seed == 0 {
        return LoaderSeed::None;
    }
    let Some(seed) = data.boot_seed() else {
        LOADER_SEED.store(1, Ordering::Release);
        return LoaderSeed::Refused;
    };
    // From here on the stack value after the seed can no longer be taken: it is overwritten
    // below with the seed.
    let _ = STACK_VALUE.compare_exchange(
        STACK_VALUE_THERE,
        STACK_VALUE_GONE,
        Ordering::AcqRel,
        Ordering::Acquire,
    );
    let source = ptr::with_exposed_provenance_mut::<u8>(crate::phys::to_virt(seed) as usize);
    // As a step, so the bytes pass through the generator's stack and no other.
    let taken = step_here(|slot, _, _, _| {
        let mut state = slot.state.try_lock()?;
        let to = seed::extra_slot(&mut state.area)?;
        for (at, byte) in to.iter_mut().enumerate() {
            // SAFETY: `boot_data` took `seed` only from inside a region of kind `BootInfo`,
            // which is RAM the loader left to the kernel and the direct map holds read-write,
            // and only clear of the boot-info header and its two arrays, so no reference
            // covers the area's 72 bytes, of which these are the first 64. Only the boot
            // processor runs.
            *byte = unsafe { source.wrapping_add(at).read_volatile() };
        }
        Some(())
    })
    .flatten()
    .is_some();
    // The whole area goes: the seed, and the stack value after it.
    let area = origin_boot_info::BOOT_SECRET_LEN as usize;
    for at in 0..area {
        // SAFETY: as the read above; all 72 bytes are the kernel's to overwrite.
        unsafe { source.wrapping_add(at).write_volatile(0) };
    }
    let mut overwritten = true;
    for at in 0..area {
        // SAFETY: as the read above.
        overwritten &= unsafe { source.wrapping_add(at).read_volatile() } == 0;
    }
    let found = if taken {
        LoaderSeed::Mixed { overwritten }
    } else {
        LoaderSeed::Refused
    };
    LOADER_SEED.store(
        match found {
            LoaderSeed::Mixed { overwritten: true } => 2,
            LoaderSeed::Mixed { overwritten: false } => 3,
            _ => 1,
        },
        Ordering::Release,
    );
    found
}

/// How the boot processor's start went: what the boot's first line about the generator says
/// (R13). Nothing here is secret.
#[derive(Clone, Copy, Debug)]
pub struct BootStart {
    /// Whether the known-answer self-test passed.
    pub self_test: bool,
    /// Whether there is a root.
    pub root: bool,
    /// Where RDSEED stands on the boot processor.
    pub rdseed: Condition,
    /// Where RDRAND stands on it.
    pub rdrand: Condition,
    /// The instruction the root's first seed is counted on.
    pub counted: Option<Instruction>,
    /// Whether the other instruction's start-up draws were the root's nonce.
    pub both: bool,
    /// What became of the loader's seed.
    pub loader_seed: LoaderSeed,
    /// What became of the loader's value for the kernel stacks' region.
    pub stack_value: StackValue,
    /// Whether the boot processor has a generator of its own.
    pub generator: bool,
}

/// Starts the root and the boot processor's own generator (R5, R8). Once, on the boot
/// processor, after the heap and the clock and before any other processor starts; a second
/// call, or a call from another processor, does nothing and returns `None`.
pub fn start_boot() -> Option<BootStart> {
    if cpu::this_cpu() != CpuIndex::BOOT || BOOT_STARTED.swap(true, Ordering::AcqRel) {
        return None;
    }
    let slot = SLOTS.get(CpuIndex::BOOT.get())?;
    if !prepare(CpuIndex::BOOT) {
        return None;
    }
    read_reports(slot);
    let loader_seed = take_loader_seed();
    let mut kernel = Kernel { quick: false };
    let root =
        origin_random::start_root(&mut kernel, matches!(loader_seed, LoaderSeed::Mixed { .. }));
    let own = origin_random::start_processor(&mut kernel);
    finish_start(slot);
    let (counted, both) = cpu::without_interrupts(|| {
        let root = ROOT.lock();
        (root.counted(), root.both())
    });
    Some(BootStart {
        self_test: root.self_test,
        root: root.instantiated,
        rdseed: own.rdseed,
        rdrand: own.rdrand,
        counted,
        both,
        loader_seed,
        stack_value: stack_value(),
        generator: own.generator,
    })
}

/// Starts the generator of the application processor this runs on: its instructions' start-up
/// tests and a first seed from the root (R5). The boot processor called [`prepare`] for it.
pub(crate) fn start_this_processor() {
    let Some(slot) = SLOTS.get(cpu::this_cpu().get()) else {
        return;
    };
    if slot.started.load(Ordering::Acquire) {
        return;
    }
    read_reports(slot);
    let _ = origin_random::start_processor(&mut Kernel { quick: false });
    finish_start(slot);
}

/// Whether there is a generator to ask (R8). With none, or once it has been withheld, every
/// request is refused.
pub fn available() -> bool {
    AVAILABLE.load(Ordering::Acquire) && !WITHHELD.load(Ordering::Acquire)
}

/// Withholds randomness for the rest of this boot: every later request is refused as if there
/// were no generator. For the boot flow, when its proof of the generator fails (ADR-0013 R2:
/// what a failed proof withdraws is withheld where it would be used).
pub fn withhold() {
    WITHHELD.store(true, Ordering::Release);
}

/// Fills `out` with random bytes from the generator of the processor the caller is on.
///
/// # Errors
///
/// [`Error::NoGenerator`] when the kernel has no generator (R8); [`Error::NotNow`] when a
/// step is already running on this processor. `out` then holds zeros.
pub fn fill(out: &mut [u8]) -> Result<(), Error> {
    if !available() {
        out.fill(0);
        return Err(Error::NoGenerator);
    }
    origin_random::fill(&mut Kernel::here(), out)
}

/// 64 random bits.
///
/// # Errors
///
/// As [`fill`]. There is no value that stands for "none".
pub fn next_u64() -> Result<u64, Error> {
    if !available() {
        return Err(Error::NoGenerator);
    }
    origin_random::next_u64(&mut Kernel::here())
}

/// Fills `out` with prediction resistance: each piece is generated by the root straight
/// after a reseed from eight fresh RDSEED draws of this processor (R6). For a value that must
/// not depend on any state that existed before the request.
///
/// # Errors
///
/// [`Error::NotNow`] where this processor has no healthy RDSEED or RDSEED did not deliver;
/// [`Error::NoGenerator`]. `out` then holds zeros.
pub fn fill_fresh(out: &mut [u8]) -> Result<(), Error> {
    if !available() {
        out.fill(0);
        return Err(Error::NoGenerator);
    }
    origin_random::fill_fresh(&mut Kernel::here(), out)
}

/// What a report may say about one processor's generator. Nothing here is secret.
#[derive(Clone, Copy, Debug)]
pub struct Processor {
    /// Its start has finished.
    pub started: bool,
    /// It has a generator.
    pub generator: bool,
    /// Where RDSEED stands on it.
    pub rdseed: Condition,
    /// Where RDRAND stands on it.
    pub rdrand: Condition,
    /// Draws of RDSEED that have passed its tests there.
    pub accepted_rdseed: u64,
    /// The same for RDRAND.
    pub accepted_rdrand: u64,
    /// Reseeds from RDSEED, from RDRAND and from the root.
    pub reseeds: [u16; 3],
    /// Steps run on it.
    pub steps: u64,
    /// The longest step while it started, in time-stamp counter ticks.
    pub longest_start: u64,
    /// The longest step since.
    pub longest: u64,
}

/// What processor `index`'s generator last published.
pub fn processor(index: CpuIndex) -> Option<Processor> {
    let slot = SLOTS.get(index.get())?;
    let summary = slot.summary.load(Ordering::Relaxed);
    Some(Processor {
        started: slot.started.load(Ordering::Acquire),
        generator: summary >> 8 & 1 != 0,
        rdseed: condition_of(summary),
        rdrand: condition_of(summary >> 4),
        accepted_rdseed: slot.accepted_rdseed.load(Ordering::Relaxed),
        accepted_rdrand: slot.accepted_rdrand.load(Ordering::Relaxed),
        reseeds: [
            (summary >> 16) as u16,
            (summary >> 32) as u16,
            (summary >> 48) as u16,
        ],
        steps: slot.steps.load(Ordering::Relaxed),
        longest_start: slot.longest_start.load(Ordering::Relaxed),
        longest: slot.longest.load(Ordering::Relaxed),
    })
}

/// What the boot proof found about the generator's stack on this processor (R9 Z3).
#[derive(Clone, Copy, Debug)]
pub struct StackProof {
    /// The stack's size.
    pub size: usize,
    /// How deep one request went on it.
    pub used: usize,
    /// Whether every byte of it was zero after a request.
    pub clean: bool,
}

/// What the stack is painted with before the measured request.
const PAINT: u64 = 0xA5A5_5A5A_C3C3_3C3C;

/// The words of a generator stack, for the boot proof.
struct StackWords {
    low: *mut u64,
    words: usize,
}

impl StackWords {
    /// The words of `stack`.
    ///
    /// # Safety
    ///
    /// For as long as the value is used, nothing runs on `stack` between two of its accesses
    /// except code that has finished by the next one: interrupts are off on the stack's
    /// processor and no step is in progress there.
    unsafe fn of(stack: &GuardedStack) -> Self {
        let size = stack.bytes();
        Self {
            low: ptr::with_exposed_provenance_mut(stack.top().wrapping_sub(size as u64) as usize),
            words: size / 8,
        }
    }

    fn read(&self, word: usize) -> u64 {
        if word >= self.words {
            return 0;
        }
        // SAFETY: `word` is inside the stack, which is mapped and 8-byte aligned, and by
        // `of`'s contract nothing else is using it.
        unsafe { self.low.wrapping_add(word).read_volatile() }
    }

    fn write(&self, word: usize, value: u64) {
        if word >= self.words {
            return;
        }
        // SAFETY: as `read`; the stack is writable.
        unsafe { self.low.wrapping_add(word).write_volatile(value) };
    }
}

/// Measures how much of this processor's generator stack one request uses, and checks that
/// after a request the whole stack holds zeros. `None` if this processor has no generator.
pub fn prove_stack() -> Option<StackProof> {
    cpu::without_interrupts(|| {
        let slot = SLOTS.get(cpu::this_cpu().get())?;
        let stack = slot.stack.get()?;
        if slot.in_step.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: interrupts are off for the whole of this closure, no step is in progress on
        // this processor (checked above), and each `fill` below runs its steps to their end
        // before the stack is looked at again.
        let stack = unsafe { StackWords::of(stack) };
        for word in 0..stack.words {
            stack.write(word, PAINT);
        }
        // One request that leaves the stack as it was, so its depth can be read off.
        slot.keep_stack.store(true, Ordering::Relaxed);
        let mut out = [0u8; 64];
        let asked = fill(&mut out);
        slot.keep_stack.store(false, Ordering::Relaxed);
        let untouched = (0..stack.words)
            .find(|&word| stack.read(word) != PAINT)
            .unwrap_or(stack.words);
        let used = (stack.words - untouched) * 8;
        // An ordinary request overwrites all of it, the paint included.
        let asked_again = fill(&mut out);
        let clean = (0..stack.words).all(|word| stack.read(word) == 0);
        if !clean {
            // Whatever the proof found, the paint and what the measured request left do not
            // stay.
            for word in 0..stack.words {
                stack.write(word, 0);
            }
        }
        (asked.is_ok() && asked_again.is_ok()).then_some(StackProof {
            size: stack.words * 8,
            used,
            clean,
        })
    })
}
