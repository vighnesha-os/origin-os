//! The loader's random-number generator (ADR-0016 R10; ADR-0004 H3 as R3 and R10 amend it).
//!
//! The loader needs two random values, the kernel's slide and the direct map's, and it hands
//! the kernel 64 bytes for its own generator's first seed. All of them come from a generator
//! of the loader's own: NIST's HMAC_DRBG over SHA-512 (`origin-crypto`), started as the
//! kernel's root is started (`origin-random`): a known-answer self-test, a start-up test of
//! 1,024 draws of each instruction the processor reports, and a first seed from those draws.
//! With no healthy instruction there is no generator: no slide and no seed.
//!
//! This is the one file of the loader that runs RDSEED and RDRAND (a gate in `cargo xtask
//! check` holds that), each in its 64-bit form.
//!
//! **The firmware's random-number protocol is not asked in this build.** R10 has the loader
//! ask it for 64 bytes that go into the generator's first seed, uncounted; `start` has the
//! place for them (`extra`). That call is another task's (TASKS 18b, "What 18b leaves").
//!
//! **Overwriting (R9), as in the kernel.** The generator's state, its store and the seed area
//! live in pages the loader allocated for them, never in a local. Every step runs with
//! interrupts off on a stack that is part of those pages, so a draw or a seed never rests on
//! the firmware's stack and the firmware's interrupt handlers never run on the generator's.
//! Before interrupts come back on, the registers a step may have left values in are cleared.
//! [`Generator::finish`] uninstantiates the generator and writes zeros over all of the pages,
//! the stack included, with volatile writes. A page between the seed area and the stack is
//! never written: if it is not all zeros at the end, the stack ran over, and the loader stops.

use core::arch::asm;
use core::ptr;
use core::slice;

use origin_random::{Busy, Condition, Ctx, Instruction, Node, Platform, Root, Source, seed};

const PAGE: usize = 4096;
/// Where the root's state lies in the first page; the processor's state is at its start.
const ROOT_OFFSET: usize = PAGE / 2;
/// Pages of the seed area, after the first page.
const AREA_PAGES: usize = seed::AREA_LEN.div_ceil(PAGE);
/// The page after the seed area that nothing writes: it shows a stack that ran over.
const SPARE_PAGE: usize = 1 + AREA_PAGES;
/// Pages of the generator's stack: 32 KiB. The deepest step uses about 3 KiB of the kernel's
/// (its boot proof measures that); the loader's ABI differs, and this leaves ten times that.
const STACK_PAGES: usize = 8;
/// The pages the generator's storage takes: one for the two states, the seed area, the spare
/// page and the stack, which ends where the storage ends.
pub(crate) const PAGES: usize = SPARE_PAGE + 1 + STACK_PAGES;

// Both states fit in their half of the first page.
const _: () = assert!(size_of::<Node>() <= ROOT_OFFSET);
const _: () = assert!(size_of::<Root>() <= PAGE - ROOT_OFFSET);
// And each is aligned where it lies: the storage is page-aligned.
const _: () = assert!(PAGE.is_multiple_of(align_of::<Node>()));
const _: () = assert!(ROOT_OFFSET.is_multiple_of(align_of::<Root>()));

const REPORTS_RDSEED: u8 = 1 << 0;
const REPORTS_RDRAND: u8 = 1 << 1;

/// One `RDSEED` attempt in its 64-bit form: the value if the instruction reported success.
fn rdseed64() -> Option<u64> {
    let (value, ok): (u64, u8);
    // SAFETY: RDSEED writes its destination register and the flags and reads no memory; it is
    // run only on a processor that reports it in CPUID (`Hardware::try_once`). The carry flag
    // is its answer, so the block does not promise to keep the flags.
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

/// The time-stamp counter: one of the things that tell this boot's first seed from another's.
fn rdtsc() -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: RDTSC reads no memory and writes EAX and EDX only; the loader runs at CPL 0,
    // where it is always allowed.
    unsafe {
        asm!(
            "rdtsc",
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
    u64::from(high) << 32 | u64::from(low)
}

/// What [`on_own_stack`] calls on the generator's stack.
struct Call<'a> {
    run: &'a mut dyn FnMut(),
}

/// The first frame on the generator's stack.
extern "sysv64" fn enter(call: *mut Call<'_>) {
    // SAFETY: `on_own_stack` passes a pointer to a `Call` that lives in its own frame for the
    // whole of this call, and nothing else refers to it meanwhile.
    let call = unsafe { &mut *call };
    (call.run)();
}

/// Runs `run` as one step: with interrupts off, on the stack that ends at `top`, and clears
/// the registers the step may have left values in before interrupts come back on.
///
/// # Safety
///
/// `top` is the 16-byte-aligned end of at least [`STACK_PAGES`] writable pages that nothing
/// else uses, and `run` is not already running on them.
unsafe fn on_own_stack(top: u64, run: &mut dyn FnMut()) {
    let mut call = Call { run };
    // SAFETY: the loader runs at CPL 0, so it may turn interrupts off and on. The block saves
    // the flags and this stack pointer in registers a System V function preserves, runs
    // `enter` on the other stack, which by the contract above is writable and unused, and
    // comes back. Then it clears every register a System V function may leave a value in, the
    // vector registers included, and only then restores the flags, the interrupt flag among
    // them. The block does not promise to keep the flags.
    unsafe {
        asm!(
            "pushfq",
            "pop r13",
            "cli",
            "mov r12, rsp",
            "mov rsp, {top}",
            "call {enter}",
            "xor eax, eax",
            "xor ecx, ecx",
            "xor edx, edx",
            "xor esi, esi",
            "xor edi, edi",
            "xor r8d, r8d",
            "xor r9d, r9d",
            "xor r10d, r10d",
            "xor r11d, r11d",
            "xorps xmm0, xmm0",
            "xorps xmm1, xmm1",
            "xorps xmm2, xmm2",
            "xorps xmm3, xmm3",
            "xorps xmm4, xmm4",
            "xorps xmm5, xmm5",
            "xorps xmm6, xmm6",
            "xorps xmm7, xmm7",
            "xorps xmm8, xmm8",
            "xorps xmm9, xmm9",
            "xorps xmm10, xmm10",
            "xorps xmm11, xmm11",
            "xorps xmm12, xmm12",
            "xorps xmm13, xmm13",
            "xorps xmm14, xmm14",
            "xorps xmm15, xmm15",
            "mov rsp, r12",
            "push r13",
            "popfq",
            enter = sym enter,
            top = in(reg) top,
            inout("rdi") ptr::from_mut(&mut call) => _,
            out("r12") _,
            out("r13") _,
            clobber_abi("sysv64"),
        );
    }
}

/// The processor's two instructions.
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

/// How the generator's start went, for the loader's one line about it. Nothing is secret.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Started {
    /// Whether the known-answer self-test passed.
    pub(crate) self_test: bool,
    /// Where RDSEED stands.
    pub(crate) rdseed: Condition,
    /// Where RDRAND stands.
    pub(crate) rdrand: Condition,
    /// Whether there is a generator.
    pub(crate) seeded: bool,
}

/// The loader's generator. There is one, and the boot flow owns it until it is finished.
#[derive(Debug)]
pub(crate) struct Generator {
    /// The storage: [`PAGES`] pages.
    base: *mut u8,
    reports: u8,
    seeded: bool,
}

impl Generator {
    /// Starts the generator in `storage` (R10 steps 1 and 2).
    ///
    /// # Safety
    ///
    /// `storage` is the address of [`PAGES`] zero-filled, writable, page-aligned pages that
    /// nothing else uses, for as long as the generator lives.
    pub(crate) unsafe fn start(storage: u64, rdseed: bool, rdrand: bool) -> (Self, Started) {
        let base = ptr::with_exposed_provenance_mut::<u8>(storage as usize);
        // SAFETY: the first page is writable and page-aligned by the contract above, and
        // `Node` fits in its first half (asserted at the top of this file).
        unsafe { base.cast::<Node>().write(Node::new()) };
        // SAFETY: as above: `Root` fits in the second half of the first page.
        unsafe {
            base.wrapping_add(ROOT_OFFSET)
                .cast::<Root>()
                .write(Root::new())
        };
        let mut generator = Self {
            base,
            reports: (u8::from(rdseed) * REPORTS_RDSEED) | (u8::from(rdrand) * REPORTS_RDRAND),
            seeded: false,
        };
        // The firmware's bytes would be put into `seed::extra_slot` here, and `true` passed:
        // see the module documentation.
        let start = origin_random::start_root(&mut generator, false);
        generator.seeded = start.instantiated;
        let started = Started {
            self_test: start.self_test,
            rdseed: start.rdseed,
            rdrand: start.rdrand,
            seeded: start.instantiated,
        };
        (generator, started)
    }

    /// 64 random bits, or `None` with no generator.
    pub(crate) fn next_u64(&mut self) -> Option<u64> {
        let mut bytes = [0u8; 8];
        self.fill(&mut bytes).then(|| u64::from_le_bytes(bytes))
    }

    /// Fills `out`. `false`, with `out` zeroed, when there is no generator.
    pub(crate) fn fill(&mut self, out: &mut [u8]) -> bool {
        if !self.seeded {
            out.fill(0);
            return false;
        }
        origin_random::fill_from_root(self, out).is_ok()
    }

    /// Uninstantiates the generator and overwrites all of its storage, its stack included
    /// (R10 step 4; R9). `false` if the stack ran over into the spare page below it: what the
    /// generator gave is then not to be trusted.
    #[must_use]
    pub(crate) fn finish(mut self) -> bool {
        let _ = self.root_step(&mut |ctx, root| {
            root.uninstantiate();
            ctx.node.uninstantiate();
        });
        let mut intact = true;
        for at in SPARE_PAGE * PAGE..(SPARE_PAGE + 1) * PAGE {
            // SAFETY: `at` is inside the generator's storage (`start`'s contract).
            intact &= unsafe { self.base.wrapping_add(at).read_volatile() } == 0;
        }
        for at in 0..PAGES * PAGE {
            // SAFETY: `at` is inside the generator's storage, which the values above no longer
            // use: `self` is consumed here, and no step is running.
            unsafe { self.base.wrapping_add(at).write_volatile(0) };
        }
        intact
    }

    /// The end of the generator's stack.
    fn stack_top(&self) -> u64 {
        self.base.wrapping_add(PAGES * PAGE).addr() as u64
    }

    /// The processor's state, the root's and the seed area.
    fn parts(&mut self) -> (&mut Node, &mut Root, &mut [u8]) {
        // SAFETY: `start` wrote a `Node` at the start of the storage, which this generator owns
        // exclusively; the borrow of `self` keeps the reference unique.
        let node = unsafe { &mut *self.base.cast::<Node>() };
        // SAFETY: as above, for the `Root` in the second half of the first page.
        let root = unsafe { &mut *self.base.wrapping_add(ROOT_OFFSET).cast::<Root>() };
        // SAFETY: the pages after the first are the seed area: `AREA_LEN` bytes of them,
        // written only through this reference, and distinct from the two states.
        let area =
            unsafe { slice::from_raw_parts_mut(self.base.wrapping_add(PAGE), seed::AREA_LEN) };
        (node, root, area)
    }
}

impl Platform for Generator {
    fn step(&mut self, step: &mut dyn FnMut(&mut Ctx<'_>)) -> Result<(), Busy> {
        self.root_step(&mut |ctx, _| step(ctx))
    }

    fn root_step(&mut self, step: &mut dyn FnMut(&mut Ctx<'_>, &mut Root)) -> Result<(), Busy> {
        let top = self.stack_top();
        let mut source = Hardware(self.reports);
        let (node, root, area) = self.parts();
        let mut run = || {
            step(
                &mut Ctx {
                    node: &mut *node,
                    source: &mut source,
                    area: &mut *area,
                    index: 0,
                    // The loader has no clock and needs none: its generator lives for one
                    // boot's placement and is never due a reseed.
                    now: 0,
                    tsc: rdtsc(),
                },
                &mut *root,
            );
        };
        // SAFETY: `top` is the end of the generator's own stack pages (`start`'s contract),
        // which only steps use; the loader has one thread, and a step never starts another.
        unsafe { on_own_stack(top, &mut run) };
        Ok(())
    }

    fn pause(&mut self) {
        core::hint::spin_loop();
    }

    fn quick(&self) -> bool {
        false
    }
}

/// How an instruction stands, for the loader's line.
pub(crate) const fn condition_name(condition: Condition) -> &'static str {
    match condition {
        Condition::Absent => "absent",
        Condition::Untested => "not tested",
        Condition::Testing => "being tested",
        Condition::Healthy => "healthy",
        Condition::Unhealthy => "unhealthy",
    }
}
